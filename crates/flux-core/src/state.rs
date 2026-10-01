//! The operation state (cut 7a spec, "Operation state, `format_version` 1"): one JSON object - a tree operation's
//! `manifest`, or a single-file operation's adjacent record - its codec, its crash-safe write, and the names Flux gives
//! it. `format_version` is judged before any other key, and nothing is parsed heuristically (§193).

use crate::ids::is_id;
use crate::lock::error::refuse;
use crate::lock::site::{NAME_LIMIT, name_len};
use crate::lock::{LockCode, LockError, LockResult};
use flux_fs::{Code, DirHandle, FileHandle, FsError};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Write};
use std::path::Path;

/// The version this binary writes and reads. 7b bumps it and reads this one too.
pub const FORMAT_VERSION: u64 = 1;
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

/// One operation's state, version 1.
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
    /// A new operation's state, `CREATED`.
    pub fn created(
        operation_id: &str,
        kind: Kind,
        destination_root: &Path,
        created_at: u64,
    ) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            operation_id: operation_id.to_string(),
            kind,
            destination_root: destination_root.display().to_string(),
            state: OpState::Created,
            superseded_by: None,
            created_at: created_at.to_string(),
            takeover: None,
        }
    }

    /// The record's bytes: the keys in `KEYS`' order, compact JSON.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an operation state always serializes")
    }
}

/// Decode a state record. In order: the size; JSON; an object; `format_version` (before any other key, F4); exactly
/// the eight keys; their types; their values.
pub fn decode(bytes: &[u8]) -> Result<OperationState, Unusable> {
    use serde_json::Value;
    if bytes.len() > STATE_LIMIT {
        return Err(Unusable::Corrupt(format!("longer than {STATE_LIMIT} bytes")));
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| Unusable::Corrupt(format!("not JSON: {e}")))?;
    let Value::Object(map) = &value else {
        return Err(Unusable::Corrupt("not a JSON object".to_string()));
    };
    let version = match map.get("format_version") {
        Some(v) => v.as_u64().ok_or_else(|| {
            Unusable::Corrupt(format!("format_version is not a whole number: {v}"))
        })?,
        None => return Err(Unusable::Corrupt("no format_version".to_string())),
    };
    if version != FORMAT_VERSION {
        return Err(Unusable::Incompatible(version));
    }
    if let Some(key) = map.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(Unusable::Corrupt(format!("unknown key {key}")));
    }
    if let Some(key) = KEYS.iter().find(|k| !map.contains_key(**k)) {
        return Err(Unusable::Corrupt(format!("missing key {key}")));
    }
    let state: OperationState =
        serde_json::from_value(value).map_err(|e| Unusable::Corrupt(format!("malformed: {e}")))?;
    validate(&state).map_err(Unusable::Corrupt)?;
    Ok(state)
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
    Ok(())
}

/// Now, in nanoseconds since the Unix epoch (UTC), as the lock record and the state hold it.
pub fn wall_time_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// `DEST/.flux`, the control plane.
pub const FLUX_DIR: &str = ".flux";
/// `DEST/.flux/operations`, where tree workspaces live.
pub const OPERATIONS_DIR: &str = "operations";
/// A tree operation's state, inside its workspace.
pub const MANIFEST: &str = "manifest";
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

/// Create a tree operation's workspace `operations/<id>/`, holding its manifest, so that it never exists without one
/// (decision 3):
/// 1. build it as `<id>.creating`, with the manifest written inside crash-safely;
/// 2. rename it to `<id>` without replacing;
/// 3. flush `operations/`.
///
/// A crash before the rename leaves only `<id>.creating`, which the §21.1 scan ignores (cut 9's cleanup). Returns the
/// workspace's handle.
pub fn create_workspace<D: DirHandle>(
    operations: &D,
    state: &OperationState,
) -> flux_fs::Result<D> {
    let id = OsStr::new(&state.operation_id);
    let mut creating = id.to_os_string();
    creating.push(CREATING_SUFFIX);
    {
        let building = operations.create_dir(&creating)?;
        write_state(&building, OsStr::new(MANIFEST), state)?;
        // Closed before the rename.
    }
    operations.rename_no_replace(&creating, operations, id)?;
    operations.sync()?;
    operations.open_dir(id)
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
        for name in [OsString::from(MANIFEST), temp_name(OsStr::new(MANIFEST))] {
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
        OperationState::created(&id(1), Kind::Tree, Path::new("/d"), 5)
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
            decode(br#"{"format_version":2,"anything":[1,2]}"#),
            Err(Unusable::Incompatible(2))
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
}
