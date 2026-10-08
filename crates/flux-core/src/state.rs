//! The operation state (cut 7a spec, "Operation state, `format_version` 1"): one JSON object - a tree operation's
//! `manifest`, or a single-file operation's adjacent record - its codec, its crash-safe write, and the names Flux gives
//! it. `format_version` is judged before any other key, and nothing is parsed heuristically (§193).

use crate::ids::is_id;
use crate::lock::error::refuse;
use crate::lock::site::{NAME_LIMIT, name_len};
use crate::lock::{LockCode, LockError, LockResult};
use flux_fs::{Code, DirHandle, FileHandle, FileIdentity, FsError, ObjectId};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// The version this binary writes (cut 7b). It reads `V1` too and never upgrades a record it rewrites (decision 5).
pub const FORMAT_VERSION: u64 = 2;
/// Cut 7a's version: read, classified as 7a classifies it, and rewritten as itself.
pub const V1: u64 = 1;
/// The longest state record read. A version-1 record is well under 1 KiB; anything longer is not one.
pub const STATE_LIMIT: usize = 64 * 1024;
/// A takeover's value for a prior holder's field it could not read (spec:10705-10709).
pub const UNREADABLE: &str = "unreadable";
/// Every key of a version-1 record: exactly these, no more and no fewer.
const KEYS: [&str; 8] = [
    "format_version",
    "operation_id",
    "kind",
    "destination_root",
    "state",
    "superseded_by",
    "created_at",
    "takeover",
];
/// Version 2's keys for both kinds (§218).
const CLEANUP_KEYS: [&str; 2] = ["cleanup_pending", "cleanup_pending_artifacts"];
/// Version 2's keys a single-file record adds (§249.1).
const FILE_KEYS: [&str; 10] = [
    "artifact_type",
    "attempt_id",
    "artifact_generation",
    "source_identity",
    "target_identity",
    "target_path_key",
    "owner_instance_id",
    "boot_session_id",
    "creation_wall_time",
    "last_heartbeat_wall_time",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Tree,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OpState {
    Created,
    Transferring,
    Failed,
    Completed,
    Abandoned,
}

impl OpState {
    /// §21.1: CREATED, TRANSFERRING and FAILED are resumable. COMPLETED and ABANDONED are terminal (the design's
    /// ABANDONED ruling).
    pub fn is_resumable(self) -> bool {
        matches!(self, Self::Created | Self::Transferring | Self::Failed)
    }

    /// The spec's spelling, as the JSON holds it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "CREATED",
            Self::Transferring => "TRANSFERRING",
            Self::Failed => "FAILED",
            Self::Completed => "COMPLETED",
            Self::Abandoned => "ABANDONED",
        }
    }
}

/// The §240.5 takeover record: the prior holder's fields as read, and the takeover's own time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Takeover {
    pub operation_id: String,
    pub owner_instance_id: String,
    pub boot_session_id: String,
    pub last_heartbeat_wall_time: String,
    pub creation_wall_time: String,
}

impl Takeover {
    /// 7a takes over only an UNREADABLE lock (§240.5 is for `TARGET_LOCK_UNCERTAIN`), so every prior field is
    /// `unreadable`.
    pub fn of_unreadable(creation_wall_time: u64) -> Self {
        Self {
            operation_id: UNREADABLE.to_string(),
            owner_instance_id: UNREADABLE.to_string(),
            boot_session_id: UNREADABLE.to_string(),
            last_heartbeat_wall_time: UNREADABLE.to_string(),
            creation_wall_time: creation_wall_time.to_string(),
        }
    }
}

/// §218's record (cut 7b, version 2, both kinds): `cleanup_pending` is true from the `COMPLETED` write on, and the
/// artifacts are the temporaries the copy could not remove, each in `native_hex`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cleanup {
    pub cleanup_pending: bool,
    pub cleanup_pending_artifacts: Vec<String>,
}

/// §249.1's fields a single-file record adds in version 2 (cut 7b decision 4). The identities are `identity_text`;
/// `target_identity` is `None` while no object stood at the target name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileFields {
    pub artifact_type: String,
    pub attempt_id: String,
    pub artifact_generation: u64,
    pub source_identity: String,
    pub target_identity: Option<String>,
    pub target_path_key: String,
    pub owner_instance_id: String,
    pub boot_session_id: String,
    pub creation_wall_time: String,
    pub last_heartbeat_wall_time: String,
}

/// The adjacent state record's `artifact_type`. §215 and §249 use it to tell apart the artifacts kept beside a target
/// (the lock, the partial, the state record), so it names the record's ROLE; the operation's kind is `kind` (cut 7b
/// Part 1 capstone round 3, owner ruling).
pub const ARTIFACT_STATE: &str = "state";

/// One operation's state, version 1 or 2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationState {
    pub format_version: u64,
    pub operation_id: String,
    pub kind: Kind,
    /// The destination as the operator gave it, in its display form: informational, lossy for a path that is not
    /// valid Unicode, and never read back for a decision (decision 5).
    pub destination_root: String,
    pub state: OpState,
    pub superseded_by: Option<String>,
    /// Nanoseconds since the Unix epoch, UTC, as decimal ASCII.
    pub created_at: String,
    pub takeover: Option<Takeover>,
    /// Version 2 (cut 7b, §218): `Some` exactly when `format_version` is 2.
    #[serde(skip)]
    pub cleanup: Option<Cleanup>,
    /// Version 2, a single file (cut 7b, §249.1): `Some` exactly for a version-2 `Kind::File` record.
    #[serde(skip)]
    pub file: Option<FileFields>,
}

/// Why a state record cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusable {
    /// Unreadable or malformed: `STATE_CORRUPT`.
    Corrupt(String),
    /// A `format_version` this binary does not know: `INCOMPATIBLE_STATE`.
    Incompatible(u64),
}

impl OperationState {
    /// A new operation's state as cut 7a wrote it: version 1, `CREATED`. A real run writes version 2 (`created`); tests
    /// use this to stage the state a 7a binary leaves behind.
    pub fn created_v1(
        operation_id: &str,
        kind: Kind,
        destination_root: &Path,
        created_at: u64,
    ) -> Self {
        Self {
            format_version: V1,
            operation_id: operation_id.to_string(),
            kind,
            destination_root: destination_root.display().to_string(),
            state: OpState::Created,
            superseded_by: None,
            created_at: created_at.to_string(),
            takeover: None,
            cleanup: None,
            file: None,
        }
    }

    /// A new operation's state, version 2, `CREATED`, as a run writes it. `file` is the single-file record's §249.1
    /// fields: `Some` exactly for `Kind::File`.
    pub fn created(
        operation_id: &str,
        kind: Kind,
        destination_root: &Path,
        created_at: u64,
        file: Option<FileFields>,
    ) -> Self {
        assert_eq!(
            kind == Kind::File,
            file.is_some(),
            "a single file's record, and only it, carries §249.1's fields"
        );
        Self {
            format_version: FORMAT_VERSION,
            cleanup: Some(Cleanup {
                cleanup_pending: false,
                cleanup_pending_artifacts: Vec::new(),
            }),
            file,
            ..Self::created_v1(operation_id, kind, destination_root, created_at)
        }
    }

    /// The record's bytes, in the record's OWN version (decision 5: a rewrite never upgrades it). Version 1 is the
    /// derived struct, keys in `KEYS`' order, byte for byte as 7a wrote it. Version 2 adds `Cleanup` and, for a single
    /// file, `FileFields`, as one object.
    pub fn encode(&self) -> Vec<u8> {
        if self.format_version == V1 {
            return serde_json::to_vec(self).expect("an operation state always serializes");
        }
        let mut map = object(self);
        if let Some(c) = &self.cleanup {
            map.extend(object(c));
        }
        if let Some(f) = &self.file {
            map.extend(object(f));
        }
        serde_json::to_vec(&map).expect("an operation state always serializes")
    }
}

/// Decode a state record. In order: the size; JSON; an object; `format_version` (before any other key, F4); exactly
/// that version's keys for its kind; their types; their values.
pub fn decode(bytes: &[u8]) -> Result<OperationState, Unusable> {
    use serde_json::Value;
    if bytes.len() > STATE_LIMIT {
        return Err(Unusable::Corrupt(format!("longer than {STATE_LIMIT} bytes")));
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| Unusable::Corrupt(format!("not JSON: {e}")))?;
    let Value::Object(mut map) = value else {
        return Err(Unusable::Corrupt("not a JSON object".to_string()));
    };
    let version = match map.get("format_version") {
        Some(v) => v.as_u64().ok_or_else(|| {
            Unusable::Corrupt(format!("format_version is not a whole number: {v}"))
        })?,
        None => return Err(Unusable::Corrupt("no format_version".to_string())),
    };
    if version != V1 && version != FORMAT_VERSION {
        return Err(Unusable::Incompatible(version));
    }
    let v2 = version == FORMAT_VERSION;
    let file = v2 && map.get("kind").and_then(Value::as_str) == Some("file");
    let mut expected: Vec<&str> = KEYS.to_vec();
    if v2 {
        expected.extend(CLEANUP_KEYS);
    }
    if file {
        expected.extend(FILE_KEYS);
    }
    if let Some(key) = map.keys().find(|k| !expected.contains(&k.as_str())) {
        return Err(Unusable::Corrupt(format!("unknown key {key}")));
    }
    if let Some(key) = expected.iter().find(|k| !map.contains_key(**k)) {
        return Err(Unusable::Corrupt(format!("missing key {key}")));
    }
    let malformed = |e: serde_json::Error| Unusable::Corrupt(format!("malformed: {e}"));
    let mut take = |keys: &[&str]| -> Value {
        Value::Object(
            keys.iter().filter_map(|k| map.remove(*k).map(|v| (k.to_string(), v))).collect(),
        )
    };
    let cleanup = if v2 {
        Some(serde_json::from_value::<Cleanup>(take(&CLEANUP_KEYS)).map_err(malformed)?)
    } else {
        None
    };
    let fields = if file {
        Some(serde_json::from_value::<FileFields>(take(&FILE_KEYS)).map_err(malformed)?)
    } else {
        None
    };
    let mut state: OperationState =
        serde_json::from_value(Value::Object(map)).map_err(malformed)?;
    state.cleanup = cleanup;
    state.file = fields;
    validate(&state).map_err(Unusable::Corrupt)?;
    Ok(state)
}

fn object<T: Serialize>(v: &T) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::to_value(v).expect("a state part always serializes") {
        serde_json::Value::Object(m) => m,
        other => unreachable!("a state part is a JSON object, not {other}"),
    }
}

fn validate(s: &OperationState) -> Result<(), String> {
    let digits = |v: &str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit());
    if !is_id(&s.operation_id) {
        return Err(format!("operation_id is not an id: {}", s.operation_id));
    }
    if let Some(by) = &s.superseded_by
        && !is_id(by)
    {
        return Err(format!("superseded_by is not an id: {by}"));
    }
    if !digits(&s.created_at) {
        return Err(format!("created_at is not decimal nanoseconds: {}", s.created_at));
    }
    if let Some(t) = &s.takeover {
        let id_or_unreadable = |v: &str| is_id(v) || v == UNREADABLE;
        if !id_or_unreadable(&t.operation_id) || !id_or_unreadable(&t.owner_instance_id) {
            return Err("the takeover's prior holder is neither an id nor unreadable".to_string());
        }
        if t.boot_session_id.is_empty() {
            return Err("the takeover's boot_session_id is empty".to_string());
        }
        if !(digits(&t.last_heartbeat_wall_time) || t.last_heartbeat_wall_time == UNREADABLE) {
            return Err("the takeover's last_heartbeat_wall_time is not a time".to_string());
        }
        if !digits(&t.creation_wall_time) {
            return Err("the takeover's creation_wall_time is not a time".to_string());
        }
    }
    let v2 = s.format_version == FORMAT_VERSION;
    if v2 != s.cleanup.is_some() || (v2 && s.kind == Kind::File) != s.file.is_some() {
        return Err("the record's fields do not match its version and kind".to_string());
    }
    if let Some(c) = &s.cleanup {
        if c.cleanup_pending != (s.state == OpState::Completed) {
            return Err(format!(
                "cleanup_pending is {} in a {} record",
                c.cleanup_pending,
                s.state.as_str()
            ));
        }
        if let Some(bad) = c.cleanup_pending_artifacts.iter().find(|a| from_native_hex(a).is_none())
        {
            return Err(format!("a cleanup_pending_artifacts entry is not native-unit hex: {bad}"));
        }
    }
    if let Some(f) = &s.file {
        let hex = |v: &str| {
            !v.is_empty()
                && v.len().is_multiple_of(2)
                && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        };
        if f.artifact_type != ARTIFACT_STATE {
            return Err(format!("artifact_type is not {ARTIFACT_STATE}: {}", f.artifact_type));
        }
        if !is_id(&f.attempt_id) || !is_id(&f.owner_instance_id) {
            return Err("attempt_id or owner_instance_id is not an id".to_string());
        }
        if f.boot_session_id.is_empty() {
            return Err("boot_session_id is empty".to_string());
        }
        if f.artifact_generation == 0 {
            return Err("artifact_generation is 0".to_string());
        }
        if !digits(&f.creation_wall_time) || !digits(&f.last_heartbeat_wall_time) {
            return Err(
                "creation_wall_time or last_heartbeat_wall_time is not decimal nanoseconds"
                    .to_string(),
            );
        }
        if !hex(&f.target_path_key) {
            return Err(format!("target_path_key is not hex: {}", f.target_path_key));
        }
        if parse_identity(&f.source_identity).is_none() {
            return Err(format!("source_identity is not an identity: {}", f.source_identity));
        }
        match &f.target_identity {
            Some(t) if parse_identity(t).is_none() => {
                return Err(format!("target_identity is not an identity: {t}"));
            }
            None if s.state == OpState::Completed => {
                return Err("a COMPLETED single-file record has no target_identity".to_string());
            }
            _ => {}
        }
    }
    Ok(())
}

/// Now, in nanoseconds since the Unix epoch (UTC), as the lock record and the state hold it.
pub fn wall_time_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// A `FileIdentity` as version 2 writes it: `strong:<volume>:<index>`, `weak:<volume>:<index>` or `unavailable`, the
/// numbers decimal (`flux_fs::ObjectId`).
pub fn identity_text(id: FileIdentity) -> String {
    match id {
        FileIdentity::Strong(o) => format!("strong:{}:{}", o.volume, o.index),
        FileIdentity::Weak(o) => format!("weak:{}:{}", o.volume, o.index),
        FileIdentity::Unavailable => UNAVAILABLE.to_string(),
    }
}

/// `identity_text`'s inverse: `None` for anything it could not have written (a leading zero included, so the text
/// round-trips exactly).
pub fn parse_identity(text: &str) -> Option<FileIdentity> {
    if text == UNAVAILABLE {
        return Some(FileIdentity::Unavailable);
    }
    let mut parts = text.split(':');
    let (strength, volume, index) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let canonical = |v: &str| {
        !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) && (v == "0" || !v.starts_with('0'))
    };
    if !canonical(volume) || !canonical(index) {
        return None;
    }
    let o = ObjectId { volume: volume.parse().ok()?, index: index.parse().ok()? };
    match strength {
        "strong" => Some(FileIdentity::Strong(o)),
        "weak" => Some(FileIdentity::Weak(o)),
        _ => None,
    }
}

const UNAVAILABLE: &str = "unavailable";

/// A path as lowercase hex of the platform's NATIVE name units (cut 7b, `cleanup_pending_artifacts`): the raw bytes on
/// POSIX, the UTF-16 code units little-endian on Windows, with `/` (as a native unit) between components. Lossless
/// and specified - not `as_encoded_bytes`, whose encoding Rust leaves unspecified (spec panel r2, PD-1).
pub fn native_hex(path: &Path) -> String {
    let mut units = Vec::new();
    for (i, part) in path.iter().enumerate() {
        if i > 0 {
            units.extend(native_units(OsStr::new("/")));
        }
        units.extend(native_units(part));
    }
    crate::lock::site::hex(&units)
}

/// `native_hex`'s inverse: `None` for text it could not have produced (empty, odd, upper-case, an empty component).
pub fn from_native_hex(text: &str) -> Option<PathBuf> {
    if text.is_empty()
        || !text.len().is_multiple_of(2)
        || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let bytes: Vec<u8> = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16))
        .collect::<Result<_, _>>()
        .ok()?;
    let mut path = PathBuf::new();
    for part in native_parts(&bytes)? {
        path.push(part);
    }
    Some(path)
}

#[cfg(unix)]
fn native_units(s: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

#[cfg(windows)]
fn native_units(s: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().flat_map(u16::to_le_bytes).collect()
}

#[cfg(unix)]
fn native_parts(bytes: &[u8]) -> Option<Vec<OsString>> {
    use std::os::unix::ffi::OsStringExt;
    bytes
        .split(|b| *b == b'/')
        .map(|p| (!p.is_empty()).then(|| OsString::from_vec(p.to_vec())))
        .collect()
}

#[cfg(windows)]
fn native_parts(bytes: &[u8]) -> Option<Vec<OsString>> {
    use std::os::windows::ffi::OsStringExt;
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    units
        .split(|u| *u == u16::from(b'/'))
        .map(|p| (!p.is_empty()).then(|| OsString::from_wide(p)))
        .collect()
}

/// `DEST/.flux`, the control plane.
pub const FLUX_DIR: &str = ".flux";
/// `DEST/.flux/operations`, where tree workspaces live.
pub const OPERATIONS_DIR: &str = "operations";
/// A tree operation's state, inside its workspace.
pub const MANIFEST: &str = "manifest";
/// Section 241.5's probe file, inside a workspace (cut 8a, Part P).
pub const PROBE: &str = "noreplace-probe";
/// The claim store (cut 8b), inside a workspace: created beside the manifest and removed with it.
pub const STATE_DB: &str = "state.db";
/// The probe's staging temporary.
pub const PROBE_TEMP: &str = "noreplace-probe.tmp";
/// A state file's temporary: `<name>.tmp` (decision 4; the name it stages already carries the operation id).
pub const TEMP_SUFFIX: &str = ".tmp";
/// A workspace being built: `<id>.creating`, renamed to `<id>` once its manifest is written (decision 3).
pub const CREATING_SUFFIX: &str = ".creating";
/// The reserved subdirectories of `DEST/.flux` (the design's `.flux` ruling): Flux's own, never written by a copy.
pub const RESERVED_DIRS: [&str; 3] = [OPERATIONS_DIR, "standalone", "atomic"];
/// The single-file state record is `<target>.flux-state.<id>` (F2).
pub const RECORD_INFIX: &str = ".flux-state.";

/// A copy's temporary is `<name>.flux-partial.<id>` (§18.1, normative; `flux_fs::temp_path`).
pub const PARTIAL_INFIX: &str = ".flux-partial.";
/// A workspace being removed: `<id>.removing`, a name the §21.1 scan passes over (refinement 9, reversed).
pub const REMOVING_SUFFIX: &str = ".removing";

/// The operation id that ends `name` after `infix`: `name` is `<something><infix><32 lowercase hex>` with `<something>`
/// not empty. A single-file record (`.flux-state.`) and a copy's temporary (`.flux-partial.`) are named this way.
pub fn id_after<'n>(name: &'n OsStr, infix: &str) -> Option<&'n str> {
    let bytes = name.as_encoded_bytes();
    let start = bytes.len().checked_sub(32)?;
    let id = std::str::from_utf8(&bytes[start..]).ok().filter(|s| is_id(s))?;
    let head = bytes[..start].strip_suffix(infix.as_bytes())?;
    (!head.is_empty()).then_some(id)
}

/// `<target>.flux-state.<id>`.
pub fn record_name(target: &OsStr, operation_id: &str) -> OsString {
    let mut name = target.to_os_string();
    name.push(RECORD_INFIX);
    name.push(operation_id);
    name
}

/// `<name>.tmp`.
pub fn temp_name(name: &OsStr) -> OsString {
    let mut temp = name.to_os_string();
    temp.push(TEMP_SUFFIX);
    temp
}

/// Whether every name a single-file run creates beside `target` fits `NAME_LIMIT`. The longest is its state record's
/// temporary, target + 48 units. The lock (+10) and the copy's partial (+46) are shorter. Part 3b refuses
/// `PATH_COMPONENT_INVALID` up front when this is false (decision 4).
pub fn file_names_fit(target: &OsStr) -> bool {
    name_len(&temp_name(&record_name(target, &"0".repeat(32)))) <= NAME_LIMIT
}

/// Write `state` to `name` in `dir` crash-safely (F4): `<name>.tmp` written and flushed, renamed over `name`, `dir`
/// flushed. On a failure before the rename the temporary is removed where it can be, and `name` still holds what it
/// held before: the rename is the commit point.
pub fn write_state<D: DirHandle>(
    dir: &D,
    name: &OsStr,
    state: &OperationState,
) -> flux_fs::Result<()> {
    let temp = temp_name(name);
    // Part 3b: a temporary left by an earlier failed write of this same state would make `create_new` fail forever.
    // Its name carries the operation's id, and the writer holds the destination's lock, so it is the writer's to
    // remove.
    match dir.remove_file(&temp) {
        Ok(()) => {}
        Err(e) if e.source.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut file = dir.create_new(&temp)?;
    let written =
        file.write_all(&state.encode()).map_err(FsError::from_io).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written.and_then(|()| dir.rename_replace(&temp, dir, name)) {
        let _ = dir.remove_file(&temp);
        return Err(e);
    }
    dir.sync()
}

/// Read and decode the state record `name` in `dir`. The outer error is I/O (a refused link or directory among it);
/// the inner one says what the bytes are not.
pub fn read_state<D: DirHandle>(
    dir: &D,
    name: &OsStr,
) -> flux_fs::Result<Result<OperationState, Unusable>> {
    Ok(decode(&dir.read_file(name, STATE_LIMIT)?))
}

/// `<id>.creating`.
pub fn creating_name(id: &str) -> OsString {
    let mut creating = OsString::from(id);
    creating.push(CREATING_SUFFIX);
    creating
}

/// Step 1 of `create_workspace`: `<id>.creating`, empty, as a handle.
pub fn begin_workspace<D: DirHandle>(operations: &D, id: &str) -> flux_fs::Result<D> {
    operations.create_dir(&creating_name(id))
}

/// Steps 2-3: the manifest written into `building` crash-safely, `building` closed, `<id>.creating` renamed onto
/// `<id>` without replacing, `operations/` flushed; returns the published workspace's handle.
pub fn publish_workspace<D: DirHandle>(
    operations: &D,
    building: D,
    state: &OperationState,
) -> flux_fs::Result<D> {
    let id = OsStr::new(&state.operation_id);
    write_state(&building, OsStr::new(MANIFEST), state)?;
    // Closed before the rename.
    drop(building);
    operations.rename_no_replace(&creating_name(&state.operation_id), operations, id)?;
    operations.sync()?;
    operations.open_dir(id)
}

/// Create a tree operation's workspace `operations/<id>/`, holding its manifest, so that it never exists without one
/// (decision 3):
/// 1. build it as `<id>.creating` (`begin_workspace`);
/// 2. write the manifest inside crash-safely, rename it to `<id>` without replacing, and flush `operations/`
///    (`publish_workspace`).
///
/// A crash before the rename leaves only `<id>.creating`, which the §21.1 scan ignores (cut 9's cleanup). Returns the
/// workspace's handle.
pub fn create_workspace<D: DirHandle>(
    operations: &D,
    state: &OperationState,
) -> flux_fs::Result<D> {
    let building = begin_workspace(operations, &state.operation_id)?;
    publish_workspace(operations, building, state)
}

/// `DEST/.flux/operations/`, creating `.flux` and `operations` as needed (the run's step 5) and flushing each parent a
/// creation changed. A non-directory at either name is `CONTROL_PLANE_NAMESPACE_CONFLICT`: step 2 checks it first, and
/// this repeats the check because the name can change in between.
pub fn operations_dir<D: DirHandle>(dest: &D, dest_shown: &Path) -> LockResult<D> {
    let flux_shown = dest_shown.join(FLUX_DIR);
    let flux = control_dir(dest, FLUX_DIR, &flux_shown)?;
    control_dir(&flux, OPERATIONS_DIR, &flux_shown.join(OPERATIONS_DIR))
}

/// Remove a tree operation's workspace without ever leaving `operations/<id>` without its manifest (refinement 9,
/// reversed; Part 3b decision 17): rename it to `<id>.removing`, which the §21.1 scan passes over, then remove its
/// `manifest`, a crash's `manifest.tmp`, and the directory. A failure part-way leaves only `<id>.removing` (cut 9's).
pub fn retire_workspace<D: DirHandle>(operations: &D, id: &str) -> flux_fs::Result<()> {
    let mut retired = OsString::from(id);
    retired.push(REMOVING_SUFFIX);
    operations.rename_no_replace(OsStr::new(id), operations, &retired)?;
    {
        let dir = operations.open_dir(&retired)?;
        for name in [
            OsString::from(MANIFEST),
            temp_name(OsStr::new(MANIFEST)),
            OsString::from(PROBE),
            OsString::from(PROBE_TEMP),
            OsString::from(STATE_DB),
        ] {
            remove_if_present(&dir, &name)?;
        }
        // Closed before the directory is removed.
    }
    operations.remove_dir(&retired)
}

/// Remove a single-file operation's state record, then a crash's `<record>.tmp` beside it.
pub fn remove_record<D: DirHandle>(dir: &D, name: &OsStr) -> flux_fs::Result<()> {
    remove_if_present(dir, name)?;
    remove_if_present(dir, &temp_name(name))
}

/// Finish step 4: `DEST/.flux/operations/`, then `DEST/.flux/`, each only if it is empty. Absent, not empty, or not a
/// directory Flux can open is not an error: whatever else is there is not this run's.
pub fn remove_empty_control_dirs<D: DirHandle>(dest: &D) -> flux_fs::Result<()> {
    let tolerated =
        |e: &FsError| matches!(e.source.kind(), ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty);
    {
        let flux = match dest.open_dir(OsStr::new(FLUX_DIR)) {
            Ok(f) => f,
            Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) if matches!(e.code, Code::DestinationError | Code::SafetyRejected) => {
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        match flux.remove_dir(OsStr::new(OPERATIONS_DIR)) {
            Ok(()) => {}
            Err(e) if tolerated(&e) => {}
            Err(e) => return Err(e),
        }
        // `.flux` is closed before it is removed.
    }
    match dest.remove_dir(OsStr::new(FLUX_DIR)) {
        Ok(()) => Ok(()),
        Err(e) if tolerated(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

fn remove_if_present<D: DirHandle>(dir: &D, name: &OsStr) -> flux_fs::Result<()> {
    match dir.remove_file(name) {
        Ok(()) => Ok(()),
        Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn control_dir<D: DirHandle>(parent: &D, name: &str, shown: &Path) -> LockResult<D> {
    let name = OsStr::new(name);
    match parent.create_dir(name) {
        Ok(d) => {
            parent.sync()?;
            Ok(d)
        }
        Err(e) if e.source.kind() == ErrorKind::AlreadyExists => match parent.open_dir(name) {
            Ok(d) => Ok(d),
            Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
                Err(control_path_conflict(shown))
            }
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    }
}

/// `CONTROL_PLANE_NAMESPACE_CONFLICT` for a control path a non-Flux object occupies, with the refusal-guidance
/// table's advice.
pub(crate) fn control_path_conflict(shown: &Path) -> LockError {
    refuse(
        LockCode::ControlPlaneNamespaceConflict,
        None,
        format!(
            "a non-Flux object occupies {}, a Flux control path; move it away",
            shown.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> String {
        format!("{n:032x}")
    }

    fn created() -> OperationState {
        OperationState::created_v1(&id(1), Kind::Tree, Path::new("/d"), 5)
    }

    #[test]
    fn a_state_encodes_to_the_documented_json_and_back() {
        let s = created();
        let text = String::from_utf8(s.encode()).unwrap();
        assert_eq!(
            text,
            format!(
                r#"{{"format_version":1,"operation_id":"{}","kind":"tree","destination_root":"/d","state":"CREATED","superseded_by":null,"created_at":"5","takeover":null}}"#,
                id(1)
            )
        );
        assert_eq!(decode(text.as_bytes()), Ok(s));
    }

    #[test]
    fn every_state_and_kind_has_its_spec_spelling_and_resumability() {
        for (state, text, resumable) in [
            (OpState::Created, "CREATED", true),
            (OpState::Transferring, "TRANSFERRING", true),
            (OpState::Failed, "FAILED", true),
            (OpState::Completed, "COMPLETED", false),
            (OpState::Abandoned, "ABANDONED", false),
        ] {
            assert_eq!(state.as_str(), text);
            assert_eq!(state.is_resumable(), resumable, "{text}");
            let s = OperationState { state, ..created() };
            let json = String::from_utf8(s.encode()).unwrap();
            assert!(json.contains(&format!(r#""state":"{text}""#)), "{json}");
            assert_eq!(decode(&s.encode()), Ok(s));
        }
        let f = OperationState { kind: Kind::File, ..created() };
        assert!(String::from_utf8(f.encode()).unwrap().contains(r#""kind":"file""#));
    }

    #[test]
    fn format_version_is_judged_before_any_other_key() {
        assert_eq!(
            decode(br#"{"format_version":3,"anything":[1,2]}"#),
            Err(Unusable::Incompatible(3))
        );
        assert!(matches!(decode(br#"{"kind":"tree"}"#), Err(Unusable::Corrupt(_))), "none");
        assert!(matches!(decode(br#"{"format_version":"1"}"#), Err(Unusable::Corrupt(_))), "text");
        assert!(matches!(decode(br#"{"format_version":1.0}"#), Err(Unusable::Corrupt(_))), "float");
    }

    #[test]
    fn a_version_1_record_has_exactly_its_eight_keys() {
        let mut v: serde_json::Value = serde_json::from_slice(&created().encode()).unwrap();
        v["extra"] = serde_json::json!(true);
        assert!(
            matches!(decode(v.to_string().as_bytes()), Err(Unusable::Corrupt(w)) if w == "unknown key extra")
        );
        let mut v: serde_json::Value = serde_json::from_slice(&created().encode()).unwrap();
        v.as_object_mut().unwrap().remove("superseded_by");
        assert!(
            matches!(decode(v.to_string().as_bytes()), Err(Unusable::Corrupt(w)) if w == "missing key superseded_by")
        );
    }

    #[test]
    fn malformed_values_are_corrupt() {
        let bad = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut v: serde_json::Value = serde_json::from_slice(&created().encode()).unwrap();
            edit(&mut v);
            decode(v.to_string().as_bytes())
        };
        for (what, r) in [
            ("an operation_id that is not an id", bad(&|v| v["operation_id"] = "../x".into())),
            ("a superseded_by that is not an id", bad(&|v| v["superseded_by"] = "x".into())),
            ("a created_at that is not decimal", bad(&|v| v["created_at"] = "12a".into())),
            ("an unknown state", bad(&|v| v["state"] = "DONE".into())),
            ("an unknown kind", bad(&|v| v["kind"] = "dir".into())),
            (
                "a takeover missing a key",
                bad(&|v| v["takeover"] = serde_json::json!({"operation_id": "unreadable"})),
            ),
        ] {
            assert!(matches!(r, Err(Unusable::Corrupt(_))), "{what}: {r:?}");
        }
        for (what, bytes) in
            [("not JSON", &b"{"[..]), ("not an object", &b"[1]"[..]), ("empty", &b""[..])]
        {
            assert!(matches!(decode(bytes), Err(Unusable::Corrupt(_))), "{what}");
        }
        assert!(
            matches!(decode(&vec![b' '; STATE_LIMIT + 1]), Err(Unusable::Corrupt(_))),
            "oversized"
        );
    }

    #[test]
    fn an_unreadable_takeover_round_trips() {
        let t = Takeover::of_unreadable(9);
        assert_eq!(
            (
                t.operation_id.as_str(),
                t.last_heartbeat_wall_time.as_str(),
                t.creation_wall_time.as_str()
            ),
            (UNREADABLE, UNREADABLE, "9")
        );
        let s = OperationState { takeover: Some(t), superseded_by: Some(id(2)), ..created() };
        assert_eq!(decode(&s.encode()), Ok(s));
    }

    /// Each takeover check refuses on its own (capstone round 2: no test exercised a rejection, so deleting any of
    /// them left the suite green). The base takeover is valid with READ values, so a case that decodes must be that
    /// one field's fault.
    #[test]
    fn each_malformed_takeover_field_is_corrupt() {
        let read = Takeover {
            operation_id: id(3),
            owner_instance_id: id(4),
            boot_session_id: "boot".to_string(),
            last_heartbeat_wall_time: "7".to_string(),
            creation_wall_time: "9".to_string(),
        };
        let valid = OperationState { takeover: Some(read.clone()), ..created() };
        assert_eq!(decode(&valid.encode()), Ok(valid), "the base case is valid");
        type Edit = fn(&mut Takeover);
        let cases: [(&str, Edit); 6] = [
            ("an operation_id that is neither an id nor unreadable", |t| {
                t.operation_id = "x".to_string()
            }),
            ("an owner_instance_id that is neither", |t| t.owner_instance_id = "x".to_string()),
            ("an empty boot_session_id", |t| t.boot_session_id = String::new()),
            ("a last_heartbeat_wall_time that is neither a time nor unreadable", |t| {
                t.last_heartbeat_wall_time = "later".to_string()
            }),
            ("a creation_wall_time that is not a time", |t| {
                t.creation_wall_time = "later".to_string()
            }),
            ("a creation_wall_time of unreadable: it is the takeover's own time", |t| {
                t.creation_wall_time = UNREADABLE.to_string()
            }),
        ];
        for (what, edit) in cases {
            let mut t = read.clone();
            edit(&mut t);
            let s = OperationState { takeover: Some(t), ..created() };
            assert!(matches!(decode(&s.encode()), Err(Unusable::Corrupt(_))), "{what}");
        }
    }

    use crate::fault_fs::{FakeDirHandle, FaultFs};
    use crate::lock::test_support::refusal;
    use flux_fs::{DestinationRoot, FileSystem, FileType};

    /// A fake with `/p/dest`; the handle is on it.
    fn dest() -> (FaultFs, FakeDirHandle) {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        let d = fs.destination_root(Path::new("/p/dest")).unwrap();
        (fs, d)
    }

    fn position(calls: &[String], prefix: &str) -> usize {
        calls
            .iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} in {calls:?}"))
    }

    #[test]
    fn a_state_is_written_through_a_flushed_temporary_then_renamed_and_the_directory_flushed() {
        let (fs, d) = dest();
        write_state(&d, OsStr::new("rec"), &created()).unwrap();
        assert_eq!(read_state(&d, OsStr::new("rec")).unwrap(), Ok(created()));
        assert!(!fs.exists("/p/dest/rec.tmp"));
        let calls = fs.calls();
        let order = ["create_new(", "sync_all(", "rename_replace(", "sync_dir("]
            .map(|p| position(&calls, p));
        assert!(order.is_sorted(), "{calls:?}");
    }

    #[test]
    fn a_failed_write_leaves_the_old_state_and_no_temporary() {
        let (fs, d) = dest();
        write_state(&d, OsStr::new("rec"), &created()).unwrap();
        let newer = OperationState { state: OpState::Transferring, ..created() };
        fs.fail("rename_replace", Code::IoError);
        assert!(write_state(&d, OsStr::new("rec"), &newer).is_err());
        assert_eq!(
            read_state(&d, OsStr::new("rec")).unwrap(),
            Ok(created()),
            "the rename is the commit"
        );
        assert!(!fs.exists("/p/dest/rec.tmp"));
        fs.fail("sync_all", Code::IoError);
        assert!(write_state(&d, OsStr::new("rec"), &newer).is_err());
        assert_eq!(read_state(&d, OsStr::new("rec")).unwrap(), Ok(created()));
        assert!(!fs.exists("/p/dest/rec.tmp"));
    }

    #[test]
    fn begin_then_publish_is_create_workspace() {
        let (fs, d) = dest();
        let ops = operations_dir(&d, Path::new("D")).unwrap();
        let path = format!("/p/dest/.flux/operations/{}", id(1));
        let building = begin_workspace(&ops, &created().operation_id).unwrap();
        assert!(
            fs.exists(format!("{path}.creating"))
                && !fs.exists(format!("{path}.creating/manifest"))
        );
        let ws = publish_workspace(&ops, building, &created()).unwrap();
        assert_eq!(read_state(&ws, OsStr::new(MANIFEST)).unwrap(), Ok(created()));
        assert!(fs.exists(format!("{path}/manifest")) && !fs.exists(format!("{path}.creating")));
    }

    #[test]
    fn a_workspace_appears_only_with_its_manifest() {
        let (fs, d) = dest();
        let ops = operations_dir(&d, Path::new("D")).unwrap();
        let ws = create_workspace(&ops, &created()).unwrap();
        let path = format!("/p/dest/.flux/operations/{}", id(1));
        assert_eq!(read_state(&ws, OsStr::new(MANIFEST)).unwrap(), Ok(created()));
        assert!(fs.exists(format!("{path}/manifest")));
        assert!(!fs.exists(format!("{path}.creating")));
        let calls = fs.calls();
        let rename = position(&calls, "rename_no_replace(");
        assert!(calls[rename].contains(".creating"), "{}", calls[rename]);
        assert!(
            calls[..rename].iter().any(|c| c.starts_with("rename_replace(")),
            "the manifest is committed before the workspace appears: {calls:?}"
        );
        assert!(
            calls[rename + 1..].iter().any(|c| c.starts_with("sync_dir(")),
            "operations/ is flushed after: {calls:?}"
        );
    }

    #[test]
    fn a_workspace_whose_rename_fails_is_left_only_under_its_creating_name() {
        let (fs, d) = dest();
        let ops = operations_dir(&d, Path::new("D")).unwrap();
        fs.fail("rename_no_replace", Code::IoError);
        assert!(create_workspace(&ops, &created()).is_err());
        let path = format!("/p/dest/.flux/operations/{}", id(1));
        assert!(fs.exists(format!("{path}.creating/manifest")));
        assert!(!fs.exists(&path));
    }

    #[test]
    fn the_operations_directory_is_made_once_and_a_foreign_object_is_a_conflict() {
        let (fs, d) = dest();
        drop(operations_dir(&d, Path::new("D")).unwrap());
        assert!(fs.exists("/p/dest/.flux/operations"));
        drop(operations_dir(&d, Path::new("D")).expect("a second call opens what the first made"));

        let (fs, d) = dest();
        fs.write_file("/p/dest/.flux", b"not a directory");
        let r = refusal(operations_dir(&d, Path::new("D")));
        assert_eq!(r.code, LockCode::ControlPlaneNamespaceConflict);
        assert!(r.detail.contains(".flux") && r.detail.contains("move it away"), "{}", r.detail);

        let (fs, d) = dest();
        fs.create_dir(Path::new("/p/dest/.flux")).unwrap();
        fs.write_file("/p/dest/.flux/operations", b"");
        fs.set_type("/p/dest/.flux/operations", FileType::Symlink);
        assert_eq!(
            refusal(operations_dir(&d, Path::new("D"))).code,
            LockCode::ControlPlaneNamespaceConflict
        );
    }

    #[test]
    fn state_names_and_the_longest_name_a_single_file_run_creates() {
        assert_eq!(
            record_name(OsStr::new("t"), &id(1)),
            OsString::from(format!("t.flux-state.{}", id(1)))
        );
        assert_eq!(temp_name(OsStr::new("manifest")), OsString::from("manifest.tmp"));
        assert!(file_names_fit(OsStr::new(&"n".repeat(NAME_LIMIT - 48))));
        assert!(!file_names_fit(OsStr::new(&"n".repeat(NAME_LIMIT - 47))));
    }

    #[test]
    fn an_id_is_read_after_its_infix_and_nothing_else_is() {
        let id = "a".repeat(32);
        let after = |n: String| id_after(OsStr::new(&n), RECORD_INFIX).map(str::to_string);
        assert_eq!(after(format!("t.flux-state.{id}")), Some(id.clone()));
        assert_eq!(after(format!(".flux-state.{id}")), None, "no target before the infix");
        assert_eq!(after(format!("t.flux-state.{id}.tmp")), None, "a temporary");
        assert_eq!(after(format!("t.flux-state.{}", "A".repeat(32))), None, "not lowercase");
        assert_eq!(after(format!("t.flux-partial.{id}")), None, "another infix");
        assert_eq!(after(format!("t.flux-state.{}", &id[1..])), None, "31 digits");
        assert_eq!(
            id_after(OsStr::new(&format!("x.flux-partial.{id}")), PARTIAL_INFIX),
            Some(id.as_str())
        );
    }

    #[test]
    fn a_stale_temporary_of_the_same_state_does_not_wedge_its_next_write() {
        let (fs, d) = dest();
        fs.write_file("/p/dest/rec.tmp", b"left by an earlier failed write");
        write_state(&d, OsStr::new("rec"), &created()).unwrap();
        assert_eq!(read_state(&d, OsStr::new("rec")).unwrap(), Ok(created()));
        assert!(!fs.exists("/p/dest/rec.tmp"));
    }

    #[test]
    fn a_workspace_is_retired_to_a_non_id_name_before_it_is_removed() {
        let (fs, d) = dest();
        let ops = operations_dir(&d, Path::new("D")).unwrap();
        drop(create_workspace(&ops, &created()).unwrap());
        let path = format!("/p/dest/.flux/operations/{}", id(1));
        fs.write_file(format!("{path}/manifest.tmp"), b"a crash's temporary");
        retire_workspace(&ops, &id(1)).unwrap();
        assert!(!fs.exists(&path) && !fs.exists(format!("{path}.removing")));
        let calls: Vec<String> = fs.calls().iter().map(|c| c.replace('\\', "/")).collect();
        let retire = position(&calls, &format!("rename_no_replace({path} -> {path}.removing)"));
        let manifest = position(&calls, &format!("remove_file({path}.removing/manifest)"));
        assert!(retire < manifest, "the id name goes first: {calls:?}");
    }

    #[test]
    fn a_retire_removes_the_probe_file_and_its_temporary() {
        let (fs, d) = dest();
        let ops = operations_dir(&d, Path::new("D")).unwrap();
        drop(create_workspace(&ops, &created()).unwrap());
        let path = format!("/p/dest/.flux/operations/{}", id(1));
        fs.write_file(format!("{path}/{PROBE}"), b"");
        fs.write_file(format!("{path}/{PROBE_TEMP}"), b"");
        retire_workspace(&ops, &id(1)).unwrap();
        assert!(!fs.exists(&path) && !fs.exists(format!("{path}.removing")));
    }

    #[test]
    fn retire_removes_state_db() {
        let (fs, d) = dest();
        let ops = operations_dir(&d, Path::new("D")).unwrap();
        drop(create_workspace(&ops, &created()).unwrap());
        let path = format!("/p/dest/.flux/operations/{}", id(1));
        fs.write_file(format!("{path}/{STATE_DB}"), b"");
        retire_workspace(&ops, &id(1)).unwrap();
        assert!(!fs.exists(&path) && !fs.exists(format!("{path}.removing")));
    }

    #[test]
    fn a_retire_that_stops_part_way_leaves_nothing_the_scan_reads() {
        let (fs, d) = dest();
        let ops = operations_dir(&d, Path::new("D")).unwrap();
        drop(create_workspace(&ops, &created()).unwrap());
        // The manifest's removal fails, as if the run crashed right after the rename.
        fs.fail("remove_file", Code::IoError);
        assert!(retire_workspace(&ops, &id(1)).is_err());
        assert!(fs.exists(format!("/p/dest/.flux/operations/{}.removing/manifest", id(1))));
        let scan = crate::prior::scan_tree(&d, Path::new("D"), &id(9)).unwrap();
        assert!(scan.resumable.is_empty(), "a `.removing` workspace is not an operation");
    }

    #[test]
    fn a_record_is_removed_with_its_temporary_and_absence_is_not_an_error() {
        let (fs, d) = dest();
        let rec = record_name(OsStr::new("t"), &id(1));
        let shown = format!("/p/dest/{}", rec.to_string_lossy());
        write_state(&d, &rec, &created()).unwrap();
        fs.write_file(format!("{shown}.tmp"), b"a crash's temporary");
        remove_record(&d, &rec).unwrap();
        assert!(!fs.exists(&shown) && !fs.exists(format!("{shown}.tmp")));
        remove_record(&d, &rec).expect("already gone is not an error");
    }

    #[test]
    fn empty_control_directories_are_removed_and_a_non_empty_one_is_kept() {
        let (fs, d) = dest();
        drop(operations_dir(&d, Path::new("D")).unwrap());
        remove_empty_control_dirs(&d).unwrap();
        assert!(!fs.exists("/p/dest/.flux"));
        remove_empty_control_dirs(&d).expect("absent is not an error");
        drop(operations_dir(&d, Path::new("D")).unwrap());
        fs.write_file("/p/dest/.flux/user-file", b"the user's");
        remove_empty_control_dirs(&d).unwrap();
        assert!(!fs.exists("/p/dest/.flux/operations"), "the empty one goes");
        assert!(fs.exists("/p/dest/.flux/user-file"), "a .flux holding anything else stays");
    }

    fn file_fields() -> FileFields {
        FileFields {
            artifact_type: ARTIFACT_STATE.to_string(),
            attempt_id: id(7),
            artifact_generation: 1,
            source_identity: "strong:3:9".to_string(),
            target_identity: None,
            target_path_key: "74".to_string(),
            owner_instance_id: id(8),
            boot_session_id: "boot".to_string(),
            creation_wall_time: "5".to_string(),
            last_heartbeat_wall_time: "5".to_string(),
        }
    }

    fn tree_v2() -> OperationState {
        OperationState::created(&id(1), Kind::Tree, Path::new("/d"), 5, None)
    }

    fn file_v2() -> OperationState {
        OperationState::created(&id(1), Kind::File, Path::new("/d/t"), 5, Some(file_fields()))
    }

    fn keys(s: &OperationState) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_slice(&s.encode()).unwrap();
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }

    #[test]
    fn a_version_2_tree_state_carries_the_cleanup_keys_and_round_trips() {
        let s = tree_v2();
        let mut want: Vec<String> =
            KEYS.iter().chain(CLEANUP_KEYS.iter()).map(|k| k.to_string()).collect();
        want.sort();
        assert_eq!(keys(&s), want);
        assert_eq!(decode(&s.encode()), Ok(s));
    }

    #[test]
    fn a_version_2_file_record_carries_all_sixteen_of_section_249_1_and_round_trips() {
        let s = file_v2();
        let k = keys(&s);
        assert_eq!(k.len(), 8 + 2 + 10, "{k:?}");
        for need in [
            "format_version",
            "artifact_type",
            "operation_id",
            "attempt_id",
            "target_identity",
            "target_path_key",
            "source_identity",
            "artifact_generation",
            "owner_instance_id",
            "boot_session_id",
            "creation_wall_time",
            "last_heartbeat_wall_time",
            "state",
            "superseded_by",
            "cleanup_pending",
            "cleanup_pending_artifacts",
        ] {
            assert!(k.iter().any(|x| x == need), "§249.1's {need} is missing: {k:?}");
        }
        let text = String::from_utf8(s.encode()).unwrap();
        assert!(text.contains(r#""target_identity":null"#), "{text}");
        assert_eq!(decode(&s.encode()), Ok(s));
    }

    #[test]
    fn a_version_1_record_decodes_as_version_1_with_no_new_fields() {
        let s = decode(&created().encode()).unwrap();
        assert_eq!((s.format_version, s.cleanup.is_none(), s.file.is_none()), (V1, true, true));
    }

    #[test]
    fn a_rewrite_keeps_its_records_version() {
        let v1 =
            OperationState { state: OpState::Abandoned, superseded_by: Some(id(2)), ..created() };
        assert_eq!(keys(&v1).len(), 8, "a version-1 record stays version 1");
        assert_eq!(decode(&v1.encode()).unwrap().format_version, V1);
        let v2 =
            OperationState { state: OpState::Abandoned, superseded_by: Some(id(2)), ..file_v2() };
        assert_eq!(decode(&v2.encode()).unwrap(), v2);
    }

    #[test]
    fn each_version_2_key_is_required_and_no_other_is_accepted() {
        let edited =
            |s: &OperationState, edit: &dyn Fn(&mut serde_json::Map<String, serde_json::Value>)| {
                let mut v: serde_json::Value = serde_json::from_slice(&s.encode()).unwrap();
                edit(v.as_object_mut().unwrap());
                decode(v.to_string().as_bytes())
            };
        for key in CLEANUP_KEYS.iter().chain(FILE_KEYS.iter()) {
            let r = edited(&file_v2(), &|m| {
                m.remove(*key);
            });
            assert!(
                matches!(&r, Err(Unusable::Corrupt(w)) if *w == format!("missing key {key}")),
                "{key}: {r:?}"
            );
        }
        let r = edited(&tree_v2(), &|m| {
            m.insert("attempt_id".to_string(), id(7).into());
        });
        assert!(
            matches!(&r, Err(Unusable::Corrupt(w)) if w == "unknown key attempt_id"),
            "a tree has no file keys: {r:?}"
        );
        let r = edited(&created(), &|m| {
            m.insert("cleanup_pending".to_string(), false.into());
        });
        assert!(
            matches!(&r, Err(Unusable::Corrupt(w)) if w == "unknown key cleanup_pending"),
            "version 1 has none: {r:?}"
        );
    }

    #[test]
    fn version_2s_consistency_rules_are_state_corrupt() {
        let completed = |f: &dyn Fn(&mut OperationState)| {
            let mut s = OperationState { state: OpState::Completed, ..file_v2() };
            s.cleanup.as_mut().unwrap().cleanup_pending = true;
            s.file.as_mut().unwrap().target_identity = Some("strong:3:10".to_string());
            f(&mut s);
            decode(&s.encode())
        };
        assert!(completed(&|_| {}).is_ok(), "the baseline is valid");
        for (what, r) in [
            (
                "pending while not COMPLETED",
                decode(
                    &{
                        let mut s = file_v2();
                        s.cleanup.as_mut().unwrap().cleanup_pending = true;
                        s
                    }
                    .encode(),
                ),
            ),
            (
                "COMPLETED without pending",
                completed(&|s| s.cleanup.as_mut().unwrap().cleanup_pending = false),
            ),
            (
                "an artifact that is not hex",
                completed(&|s| {
                    s.cleanup.as_mut().unwrap().cleanup_pending_artifacts = vec!["xyz".into()]
                }),
            ),
            (
                "COMPLETED with no target_identity",
                completed(&|s| s.file.as_mut().unwrap().target_identity = None),
            ),
            (
                "a target_identity that is not one",
                completed(&|s| s.file.as_mut().unwrap().target_identity = Some("strong:3".into())),
            ),
            (
                "a source_identity that is not one",
                completed(&|s| s.file.as_mut().unwrap().source_identity = "x".into()),
            ),
            (
                "an attempt_id that is not an id",
                completed(&|s| s.file.as_mut().unwrap().attempt_id = "1".into()),
            ),
            (
                "an empty boot_session_id",
                completed(&|s| s.file.as_mut().unwrap().boot_session_id = String::new()),
            ),
            (
                "artifact_generation 0",
                completed(&|s| s.file.as_mut().unwrap().artifact_generation = 0),
            ),
            (
                "an owner_instance_id that is not an id",
                completed(&|s| s.file.as_mut().unwrap().owner_instance_id = "x".into()),
            ),
            (
                "another artifact_type",
                completed(&|s| s.file.as_mut().unwrap().artifact_type = "file".into()),
            ),
            (
                "a target_path_key that is not hex",
                completed(&|s| s.file.as_mut().unwrap().target_path_key = "t".into()),
            ),
            (
                "a time that is not decimal",
                completed(&|s| s.file.as_mut().unwrap().creation_wall_time = "5s".into()),
            ),
        ] {
            assert!(matches!(r, Err(Unusable::Corrupt(_))), "{what}: {r:?}");
        }
    }

    #[test]
    fn a_file_identity_round_trips_through_its_text_and_nothing_else_parses() {
        let big = ObjectId { volume: u64::MAX, index: u128::from(u64::MAX) + 1 };
        for id in [
            FileIdentity::Strong(big),
            FileIdentity::Weak(ObjectId { volume: 0, index: 7 }),
            FileIdentity::Unavailable,
        ] {
            assert_eq!(parse_identity(&identity_text(id)), Some(id), "{}", identity_text(id));
        }
        for bad in [
            "",
            "strong:1",
            "strong:1:2:3",
            "strong:01:2",
            "Strong:1:2",
            "strong:-1:2",
            "unknown",
            "weak:1:x",
        ] {
            assert_eq!(parse_identity(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_path_round_trips_through_native_unit_hex() {
        let p = Path::new("sub").join("a.flux-partial.op");
        assert_eq!(from_native_hex(&native_hex(&p)).as_deref(), Some(p.as_path()));
        for bad in ["", "7", "7G", "AA", "2f00"] {
            assert_eq!(from_native_hex(bad), None, "{bad}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let odd = Path::new(OsStr::from_bytes(b"ab\xff"));
            assert_eq!(native_hex(odd), "6162ff", "the raw bytes, not a UTF-8 replacement");
            assert_eq!(from_native_hex(&native_hex(odd)).as_deref(), Some(odd));
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let lone = std::ffi::OsString::from_wide(&[u16::from(b'a'), 0xD800]);
            assert_eq!(
                native_hex(Path::new(&lone)),
                "610000d8",
                "UTF-16LE units, an unpaired surrogate kept"
            );
            assert_eq!(
                from_native_hex(&native_hex(Path::new(&lone))).as_deref(),
                Some(Path::new(&lone))
            );
        }
    }
}
