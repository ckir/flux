# Cut 7b Part 1: state version 2, its fields, and `cleanup_pending` - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every run writes its operation state as version 2. The single-file record carries all of §249.1's fields, with
real filesystem identities, and every `COMPLETED` write carries §218's `cleanup_pending` and the list of temporaries
the copy left. Version-1 states from cut 7a are still read, and are never upgraded.

**Architecture:**
- **Codec (`crates/flux-core/src/state.rs`):** one in-memory `OperationState` gains two optional parts, `cleanup`
  (both kinds) and `file` (a single file). Version 1 keeps its derived, byte-exact encoding. Version 2 is encoded by
  merging those parts' JSON objects into the base object.
- **Identity:** a new `FileHandle::identity` reads a file's identity from its open handle. The copy uses it to
  return the published target's identity in `Outcome`.
- **The run:** fills the new fields at state creation (Part 3b-1's `open_operation`) and at the finish (`complete`).

**Tech Stack:** Rust 2024, serde/serde_json, rustix (POSIX), windows-sys (Windows), the `FaultFs` fake, nextest via
`just`.

**Spec:** `docs/superpowers/specs/2026-10-02-cut-7b-state-v2-and-heartbeat-design.md` (accepted at `4e09fcb`). Part 2
(the heartbeat) is planned after this part lands (owner ruling, `.clavity/seams/cut7b-split.md`).

---

## Decisions this plan makes (each traced to the spec or to measured code)

1. **`created` writes version 2; `created_v1` is 7a's.** `OperationState::created(id, kind, dest, now, file)` is what a
   run writes. 7a's constructor is renamed `created_v1` and kept for tests that stage a state a 7a binary left.
   Every existing call site is a test or the run's one call (`crates/flux-core/src/run/session.rs:71`); all of them
   use it through `..created(..)`, so adding fields breaks no struct literal.
2. **Version 1 stays byte-exact.** `a_state_encodes_to_the_documented_json_and_back` (`state.rs:442-454`) pins
   version 1's exact bytes, so version 1 keeps `serde_json::to_vec(self)` on the derived struct. The new fields are
   `#[serde(skip)]` on it. Version 2 is built as a `serde_json::Map`, whose keys serde_json orders alphabetically
   (its default `BTreeMap`, no `preserve_order` feature in `Cargo.toml:68`). Decoding never depends on key order.
3. **`boot_session_id` is checked non-empty, not 32-hex.** It is a dashed UUID on Linux, a boot time on macOS, or
   `unknown` (`crates/flux-platform/src/lock_file.rs:291-310`). This corrects the spec's consistency bullet; Task 8
   amends it.
4. **`FileIdentity` text uses `ObjectId`'s real field names:** `strong:<volume>:<index>`, `weak:<volume>:<index>`,
   `unavailable` (`crates/flux-fs/src/fs.rs:52-76`; the field is `index`, which the spec calls `id`). Decimal, with no
   leading zeros, so text round-trips exactly. Task 8 fixes the spec's wording.
5. **The published identity is read just BEFORE the publishing rename,** from the temporary's own writer, after the
   §99 guard. A rename keeps the object, so this is the target's identity once published. The fake's writer knows
   only its temporary's PATH (`fault_fs.rs:276-285`), and `move_object` carries the identity across the rename
   (`fault_fs.rs:183-218`), so reading before the rename is correct on the fake as well as on both platforms.
6. **The existing target's identity is read under the lock**, when the run builds its state (inside
   `open_operation`, after `obtain`), through the target's directory handle. `NotFound` is `null`; any other error is
   `"unavailable"`.
7. **One `now` per run.** `open_operation` reads the wall clock once. The state's `created_at`, `creation_wall_time`
   and `last_heartbeat_wall_time`, and the lock record's two times, all carry it. That makes "the same values as this
   run's lock record" exact even across D1's restarts.
8. **The non-UTF-8 test is a unit test of the encoding.** The spec's Testing bullet asks for "a non-UTF-8 name on Linux
   (real filesystem)". The property at stake, losslessness, belongs to `native_hex`, which is a pure function. Making a
   real filesystem refuse to delete one temporary inside a run would need a permission trick that root and Windows
   both defeat. The run-level listing is tested on the fake. Task 8 records this.

## File structure

| File | Change |
|---|---|
| `crates/flux-core/src/state.rs` | version 2: `Cleanup`, `FileFields`, `created`/`created_v1`, encode/decode per version, consistency rules, `identity_text`/`parse_identity`, `native_hex`/`from_native_hex` |
| `crates/flux-fs/src/fs.rs` | `FileHandle::identity`; the test `NullWriter` |
| `crates/flux-fs/src/options.rs` | `Outcome::published_identity` |
| `crates/flux-platform/src/std_fs.rs` | `StdFile::identity` (POSIX `fstat`, Windows `FileIdInfo`) |
| `crates/flux-core/src/fault_fs.rs` | `FakeHandle::identity` |
| `crates/flux-core/src/walk.rs` | the test `NeverHandle` |
| `crates/flux-core/src/copy.rs` | `prepare_file` returns the source identity; `copy_file_guarded` returns the published identity |
| `crates/flux-core/src/run/{mod,session,place,tests}.rs` | the record's fields at creation; `cleanup_pending` and the published identity at `COMPLETED` |
| `crates/flux-core/src/{prior,state}.rs` tests, `crates/flux-core/tests/state_std_fs.rs` | `created` -> `created_v1` where a 7a state is staged |
| `crates/flux-cli/src/{exit_code,report}.rs` tests | the new `Outcome` field |
| `crates/flux-platform/tests/dir_handle.rs` | the cross-platform handle-identity test |
| `docs/superpowers/specs/2026-10-02-cut-7b-state-v2-and-heartbeat-design.md`, `TODO.md` | Task 8 |

## Commands

- One crate's tests: `cargo test -p flux-core --lib <filter>` (expect `test result: ok`).
- The repository gate, exactly as the repo runs it: `just check` (Windows host); `just check-linux` (WSL);
  `just check-mac` (cross-check). Do not add flags.
- Before each commit: `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` (the repo's own
  lint gate inside `just check`).

Every new test must be shown red under a mutant of the behaviour it pins before its task is committed. Each task names
its mutant(s).

---

### Task 1: The version-2 codec

**Files:** Modify `crates/flux-core/src/state.rs`; mechanical renames in `crates/flux-core/src/prior.rs` (tests),
`crates/flux-core/src/run/tests.rs`, `crates/flux-core/tests/state_std_fs.rs`.

- [ ] **Step 0: verify state.** Open `crates/flux-core/src/state.rs` and confirm:
  - `FORMAT_VERSION: u64 = 1` at line 16;
  - `KEYS` has 8 entries (lines 22-31);
  - `OperationState` derives `Serialize, Deserialize` with `#[serde(deny_unknown_fields)]` (lines 95-109);
  - `decode` at 148-177 and `validate` at 179-208.

  If any differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: rename 7a's constructor to `created_v1`.** In `state.rs` rename `pub fn created(` (line 122) to
  `pub fn created_v1(` and replace its doc comment with:

```rust
    /// A new operation's state as cut 7a wrote it: version 1, `CREATED`. A real run writes version 2 (`created`); tests
    /// use this to stage the state a 7a binary leaves behind.
```

  Then rename every call `OperationState::created(` to `OperationState::created_v1(` in:
  - `state.rs` (its tests, line 439);
  - `prior.rs` (line 240);
  - `run/tests.rs` (lines 108 and 459);
  - `tests/state_std_fs.rs` (lines 20, 39, 57).

  Rename the call in `run/session.rs:71` to `created_v1` as well, so the run keeps writing version 1 until Task 4
  switches it to the new `created`. Do steps 1-6 before building.

- [ ] **Step 2: the new types and constants.** Replace lines 15-16 (`/// The version this binary writes ...` and
  `pub const FORMAT_VERSION: u64 = 1;`) with:

```rust
/// The version this binary writes (cut 7b). It reads `V1` too and never upgrades a record it rewrites (decision 5).
pub const FORMAT_VERSION: u64 = 2;
/// Cut 7a's version: read, classified as 7a classifies it, and rewritten as itself.
pub const V1: u64 = 1;
```

  After `const KEYS: [&str; 8] = [...];` add:

```rust
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
```

  After `impl Takeover { ... }` add:

```rust
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

/// A single file's `artifact_type`.
pub const ARTIFACT_FILE: &str = "file";
```

  In `OperationState`, change the doc line `/// One operation's state, version 1.` to
  `/// One operation's state, version 1 or 2.` and add after `pub takeover: Option<Takeover>,`:

```rust
    /// Version 2 (cut 7b, §218): `Some` exactly when `format_version` is 2.
    #[serde(skip)]
    pub cleanup: Option<Cleanup>,
    /// Version 2, a single file (cut 7b, §249.1): `Some` exactly for a version-2 `Kind::File` record.
    #[serde(skip)]
    pub file: Option<FileFields>,
```

  In `created_v1`'s body, set `format_version: V1,` and add `cleanup: None, file: None,` at the end of the literal.

- [ ] **Step 3: identity text and native-unit hex.** Add after `wall_time_ns`:

```rust
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
    let canonical =
        |v: &str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) && (v == "0" || !v.starts_with('0'));
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
    if text.is_empty() || text.len() % 2 != 0 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
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
    bytes.split(|b| *b == b'/').map(|p| (!p.is_empty()).then(|| OsString::from_vec(p.to_vec()))).collect()
}

#[cfg(windows)]
fn native_parts(bytes: &[u8]) -> Option<Vec<OsString>> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    units.split(|u| *u == u16::from(b'/')).map(|p| (!p.is_empty()).then(|| OsString::from_wide(p))).collect()
}
```

  Change the imports at the top of `state.rs`:
  - `use flux_fs::{Code, DirHandle, FileHandle, FsError};` becomes
    `use flux_fs::{Code, DirHandle, FileHandle, FileIdentity, FsError, ObjectId};`;
  - `use std::path::Path;` becomes `use std::path::{Path, PathBuf};`.

  `crate::lock::site::hex` is `pub(crate)` (`lock/site.rs:204`), so it is visible here.

- [ ] **Step 4: the version-2 constructor and encoder.** Add after `created_v1`:

```rust
    /// A new operation's state, version 2, `CREATED`, as a run writes it. `file` is the single-file record's §249.1
    /// fields: `Some` exactly for `Kind::File`.
    pub fn created(
        operation_id: &str,
        kind: Kind,
        destination_root: &Path,
        created_at: u64,
        file: Option<FileFields>,
    ) -> Self {
        assert_eq!(kind == Kind::File, file.is_some(), "a single file's record, and only it, carries §249.1's fields");
        Self {
            format_version: FORMAT_VERSION,
            cleanup: Some(Cleanup { cleanup_pending: false, cleanup_pending_artifacts: Vec::new() }),
            file,
            ..Self::created_v1(operation_id, kind, destination_root, created_at)
        }
    }
```

  Replace `encode` (lines 140-143) with:

```rust
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
```

  And add this free function after `decode`:

```rust
fn object<T: Serialize>(v: &T) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::to_value(v).expect("a state part always serializes") {
        serde_json::Value::Object(m) => m,
        other => unreachable!("a state part is a JSON object, not {other}"),
    }
}
```

- [ ] **Step 5: the decoder.** Replace `decode` (lines 146-177) with:

```rust
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
        Value::Object(keys.iter().filter_map(|k| map.remove(*k).map(|v| (k.to_string(), v))).collect())
    };
    let cleanup = if v2 { Some(serde_json::from_value::<Cleanup>(take(&CLEANUP_KEYS)).map_err(malformed)?) } else { None };
    let fields = if file { Some(serde_json::from_value::<FileFields>(take(&FILE_KEYS)).map_err(malformed)?) } else { None };
    let mut state: OperationState = serde_json::from_value(Value::Object(map)).map_err(malformed)?;
    state.cleanup = cleanup;
    state.file = fields;
    validate(&state).map_err(Unusable::Corrupt)?;
    Ok(state)
}
```

  If the borrow checker rejects the `take` closure capturing `map` mutably while `malformed` is in scope, make `take` a
  free function `fn take(map: &mut serde_json::Map<String, Value>, keys: &[&str]) -> Value` with the same body. That
  changes no value or shape.

- [ ] **Step 6: version 2's consistency rules.** Append to `validate`, before its final `Ok(())`:

```rust
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
        if let Some(bad) = c.cleanup_pending_artifacts.iter().find(|a| from_native_hex(a).is_none()) {
            return Err(format!("a cleanup_pending_artifacts entry is not native-unit hex: {bad}"));
        }
    }
    if let Some(f) = &s.file {
        let hex = |v: &str| !v.is_empty() && v.len() % 2 == 0 && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if f.artifact_type != ARTIFACT_FILE {
            return Err(format!("artifact_type is not {ARTIFACT_FILE}: {}", f.artifact_type));
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
            return Err("creation_wall_time or last_heartbeat_wall_time is not decimal nanoseconds".to_string());
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
```

  (`digits` is the closure already defined at the top of `validate`, line 180.)

- [ ] **Step 7: the tests are already written below - implement until they pass.** In `state.rs`'s `mod tests`:
  - change `fn created()` (line 438-440) to call `created_v1`;
  - in `format_version_is_judged_before_any_other_key` (line 477) change `"format_version":2` to
    `"format_version":3` and `Incompatible(2)` to `Incompatible(3)`;
  - append these tests:

```rust
    fn file_fields() -> FileFields {
        FileFields {
            artifact_type: ARTIFACT_FILE.to_string(),
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
        let mut want: Vec<String> = KEYS.iter().chain(CLEANUP_KEYS.iter()).map(|k| k.to_string()).collect();
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
            "format_version", "artifact_type", "operation_id", "attempt_id", "target_identity", "target_path_key",
            "source_identity", "artifact_generation", "owner_instance_id", "boot_session_id", "creation_wall_time",
            "last_heartbeat_wall_time", "state", "superseded_by", "cleanup_pending", "cleanup_pending_artifacts",
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
        let v1 = OperationState { state: OpState::Abandoned, superseded_by: Some(id(2)), ..created() };
        assert_eq!(keys(&v1).len(), 8, "a version-1 record stays version 1");
        assert_eq!(decode(&v1.encode()).unwrap().format_version, V1);
        let v2 = OperationState { state: OpState::Abandoned, superseded_by: Some(id(2)), ..file_v2() };
        assert_eq!(decode(&v2.encode()).unwrap(), v2);
    }

    #[test]
    fn each_version_2_key_is_required_and_no_other_is_accepted() {
        let edited = |s: &OperationState, edit: &dyn Fn(&mut serde_json::Map<String, serde_json::Value>)| {
            let mut v: serde_json::Value = serde_json::from_slice(&s.encode()).unwrap();
            edit(v.as_object_mut().unwrap());
            decode(v.to_string().as_bytes())
        };
        for key in CLEANUP_KEYS.iter().chain(FILE_KEYS.iter()) {
            let r = edited(&file_v2(), &|m| {
                m.remove(*key);
            });
            assert!(matches!(&r, Err(Unusable::Corrupt(w)) if *w == format!("missing key {key}")), "{key}: {r:?}");
        }
        let r = edited(&tree_v2(), &|m| {
            m.insert("attempt_id".to_string(), id(7).into());
        });
        assert!(matches!(&r, Err(Unusable::Corrupt(w)) if w == "unknown key attempt_id"), "a tree has no file keys: {r:?}");
        let r = edited(&created(), &|m| {
            m.insert("cleanup_pending".to_string(), false.into());
        });
        assert!(matches!(&r, Err(Unusable::Corrupt(w)) if w == "unknown key cleanup_pending"), "version 1 has none: {r:?}");
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
            ("pending while not COMPLETED", decode(&{
                let mut s = file_v2();
                s.cleanup.as_mut().unwrap().cleanup_pending = true;
                s
            }.encode())),
            ("COMPLETED without pending", completed(&|s| s.cleanup.as_mut().unwrap().cleanup_pending = false)),
            ("an artifact that is not hex", completed(&|s| s.cleanup.as_mut().unwrap().cleanup_pending_artifacts = vec!["xyz".into()])),
            ("COMPLETED with no target_identity", completed(&|s| s.file.as_mut().unwrap().target_identity = None)),
            ("a target_identity that is not one", completed(&|s| s.file.as_mut().unwrap().target_identity = Some("strong:3".into()))),
            ("a source_identity that is not one", completed(&|s| s.file.as_mut().unwrap().source_identity = "x".into())),
            ("an attempt_id that is not an id", completed(&|s| s.file.as_mut().unwrap().attempt_id = "1".into())),
            ("an empty boot_session_id", completed(&|s| s.file.as_mut().unwrap().boot_session_id = String::new())),
            ("artifact_generation 0", completed(&|s| s.file.as_mut().unwrap().artifact_generation = 0)),
            ("another artifact_type", completed(&|s| s.file.as_mut().unwrap().artifact_type = "partial".into())),
            ("a target_path_key that is not hex", completed(&|s| s.file.as_mut().unwrap().target_path_key = "t".into())),
            ("a time that is not decimal", completed(&|s| s.file.as_mut().unwrap().creation_wall_time = "5s".into())),
        ] {
            assert!(matches!(r, Err(Unusable::Corrupt(_))), "{what}: {r:?}");
        }
    }

    #[test]
    fn a_file_identity_round_trips_through_its_text_and_nothing_else_parses() {
        let big = ObjectId { volume: u64::MAX, index: u128::from(u64::MAX) + 1 };
        for id in [FileIdentity::Strong(big), FileIdentity::Weak(ObjectId { volume: 0, index: 7 }), FileIdentity::Unavailable] {
            assert_eq!(parse_identity(&identity_text(id)), Some(id), "{}", identity_text(id));
        }
        for bad in ["", "strong:1", "strong:1:2:3", "strong:01:2", "Strong:1:2", "strong:-1:2", "unknown", "weak:1:x"] {
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
            assert_eq!(native_hex(Path::new(&lone)), "610000d8", "UTF-16LE units, an unpaired surrogate kept");
            assert_eq!(from_native_hex(&native_hex(Path::new(&lone))).as_deref(), Some(Path::new(&lone)));
        }
    }
```

  `"2f00"` is a bad input on both platforms, by the code above:
  - on POSIX its bytes `2f 00` split at `/` into an empty first component, so `native_parts` returns `None`;
  - on Windows they are the one unit `/`, which splits into two empty components, so it returns `None` too.

- [ ] **Step 8: run and prove.** `cargo fmt --all && cargo test -p flux-core --lib state::` must pass. Then prove the
  new tests are not vacuous, each mutant ALONE, the named test red, then restore:
  - (a) in `validate`, delete the `cleanup_pending != (s.state == OpState::Completed)` check:
    `version_2s_consistency_rules_are_state_corrupt` goes red;
  - (b) in `decode`, skip `expected.extend(FILE_KEYS)`:
    `a_version_2_file_record_carries_all_sixteen_of_section_249_1_and_round_trips` goes red;
  - (c) in `encode`, delete the `if self.format_version == V1` early return:
    `a_state_encodes_to_the_documented_json_and_back` goes red. It is the only test that pins version 1's key ORDER:
    the map path writes the same 8 keys, alphabetically, so `a_rewrite_keeps_its_records_version` stays green under
    this mutant by design (panel r1, MG-1). That test pins the version, which Task 6's mutant attacks;
  - (d) in `parse_identity`, drop the leading-zero rule:
    `a_file_identity_round_trips_through_its_text_and_nothing_else_parses` goes red.

  Then `cargo test -p flux-core` (the whole crate) passes: the run still writes version 1 through `created_v1` until
  Task 4.

- [ ] **Step 9: commit.** `cargo clippy --workspace --all-targets -- -D warnings` clean, then:

```bash
git add crates/flux-core/src/state.rs crates/flux-core/src/prior.rs crates/flux-core/src/run/tests.rs crates/flux-core/src/run/session.rs crates/flux-core/tests/state_std_fs.rs
git commit -m "state: version 2 - cleanup_pending, the single-file record's §249.1 fields, identity text, native-unit hex; version 1 still read and never upgraded"
```

### Task 2: `FileHandle::identity`

**Files:** Modify `crates/flux-fs/src/fs.rs`, `crates/flux-platform/src/std_fs.rs`,
`crates/flux-core/src/fault_fs.rs`, `crates/flux-core/src/walk.rs`; test in
`crates/flux-platform/tests/dir_handle.rs` and `crates/flux-core/src/fault_fs.rs`.

- [ ] **Step 0: verify state.** Confirm:
  - `pub trait FileHandle: Write { fn sync_all(&self) -> Result<()>; }` at `crates/flux-fs/src/fs.rs:102-104`;
  - `impl FileHandle for StdFile` at `std_fs.rs:41-45`;
  - `impl FileHandle for FakeHandle` at `fault_fs.rs:316`;
  - `impl FileHandle for NullWriter` at `fs.rs:393`;
  - `impl flux_fs::FileHandle for NeverHandle` at `walk.rs:758`;
  - `lock_file.rs:89-99` reads a handle's identity through `metadata_from_stat` (POSIX) and `identity_of_handle`
    (Windows).

  STOP on any mismatch.

- [ ] **Step 1: the trait.** In `fs.rs` replace the trait with:

```rust
/// A handle open for writing. Deliberately NOT `Read`: see `FileSystem::Reader`.
pub trait FileHandle: Write {
    fn sync_all(&self) -> Result<()>;
    /// The identity of the file this handle HOLDS - never of a path (cut 7b): a rename keeps it, so the copy reads the
    /// published target's identity from its temporary's handle.
    fn identity(&self) -> Result<FileIdentity>;
}
```

  In `NullWriter`'s impl (fs.rs:393) add:

```rust
        fn identity(&self) -> crate::Result<crate::FileIdentity> {
            Ok(crate::FileIdentity::Unavailable)
        }
```

  In `NeverHandle`'s impl (walk.rs:758) add:

```rust
        fn identity(&self) -> Result<flux_fs::FileIdentity> {
            Ok(flux_fs::FileIdentity::Unavailable)
        }
```

- [ ] **Step 2: the platforms.** In `std_fs.rs`'s `impl FileHandle for StdFile` add, mirroring `lock_file.rs:89-99`:

```rust
    #[cfg(unix)]
    fn identity(&self) -> Result<FileIdentity> {
        let st = rustix::fs::fstat(&self.0).map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(metadata_from_stat(&st).identity)
    }

    #[cfg(windows)]
    fn identity(&self) -> Result<FileIdentity> {
        Ok(identity_of_handle(&self.0))
    }
```

  (`FileIdentity`, `FsError`, `Result` and both helpers are already in scope in `std_fs.rs`. If the compiler says one
  is not, add it to that file's existing `use flux_fs::{...}` - never a new import of a different type.)

- [ ] **Step 3: the fake.** In `fault_fs.rs`'s `impl FileHandle for FakeHandle` add:

```rust
    fn identity(&self) -> Result<flux_fs::FileIdentity> {
        // The object at the handle's path: a writer's path is its temporary's until the rename, and `move_object`
        // carries the identity across it, so the copy reads this before publishing (cut 7b Part 1 decision 5).
        let Some(sink) = &self.sink else { return Ok(flux_fs::FileIdentity::Unavailable) };
        Ok(sink
            .lock()
            .unwrap()
            .identities
            .get(&self.path)
            .copied()
            .unwrap_or(flux_fs::FileIdentity::Unavailable))
    }
```

- [ ] **Step 4: the tests are already written - implement until they pass.** Append to
  `crates/flux-platform/tests/dir_handle.rs`, at top level outside both `cfg` modules (it runs on every platform):

```rust
/// Cut 7b: a written file's handle names its object, the one a look-up of its name finds, before and after a rename.
#[test]
fn a_written_files_handle_identity_is_its_objects_across_a_rename() {
    use flux_fs::{DestinationRoot, DirHandle, FileHandle, FileIdentity};
    use std::ffi::OsStr;
    use std::io::Write;
    let d = tempfile::TempDir::new().unwrap();
    let root = flux_platform::StdFileSystem.destination_root(d.path()).unwrap();
    let mut w = root.create_new(OsStr::new("t.tmp")).unwrap();
    w.write_all(b"x").unwrap();
    let held = w.identity().unwrap();
    assert!(!matches!(held, FileIdentity::Unavailable), "{held:?}");
    assert_eq!(root.metadata(OsStr::new("t.tmp")).unwrap().identity, held);
    root.rename_replace(OsStr::new("t.tmp"), &root, OsStr::new("t")).unwrap();
    assert_eq!(root.metadata(OsStr::new("t")).unwrap().identity, held, "a rename keeps the object");
    assert_eq!(w.identity().unwrap(), held);
}
```

  Append to `fault_fs.rs`'s `mod tests`:

```rust
    #[test]
    fn a_fake_writers_identity_is_its_objects_and_a_rename_carries_it() {
        use flux_fs::{DestinationRoot, DirHandle, FileHandle};
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let p = fs.destination_root(Path::new("/p")).unwrap();
        let w = p.create_new(OsStr::new("t.tmp")).unwrap();
        let held = w.identity().unwrap();
        assert!(matches!(held, FileIdentity::Strong(_)));
        p.rename_replace(OsStr::new("t.tmp"), &p, OsStr::new("t")).unwrap();
        assert_eq!(fs.metadata(Path::new("/p/t")).unwrap().identity, held);
    }
```

  (If `OsStr`, `FileIdentity` or `FileSystem` is not yet imported in that `mod tests`, add it to the module's existing
  `use` lines.)

- [ ] **Step 5: prove and run.** Mutants, each alone, the named test red, then restore:
  - (a) `FakeHandle::identity` returns `Ok(FileIdentity::Unavailable)`: the fake test goes red;
  - (b) `StdFile::identity` returns `Ok(FileIdentity::Unavailable)` on the host's platform: the `dir_handle` test goes
    red.

  Then `cargo test -p flux-fs -p flux-platform -p flux-core` passes.

- [ ] **Step 6: commit.**

```bash
git add crates/flux-fs/src/fs.rs crates/flux-platform/src/std_fs.rs crates/flux-core/src/fault_fs.rs crates/flux-core/src/walk.rs crates/flux-platform/tests/dir_handle.rs
git commit -m "flux-fs: FileHandle::identity - a file's identity from its own handle, POSIX fstat, Windows FileIdInfo, and the fake"
```

### Task 3: The copy returns the published target's identity; `prepare_file` returns the source's

**Files:** Modify `crates/flux-fs/src/options.rs`, `crates/flux-core/src/copy.rs`, `crates/flux-core/src/run/mod.rs`
(the one `prepare_file` call), `crates/flux-cli/src/exit_code.rs` and `crates/flux-cli/src/report.rs` (tests).

- [ ] **Step 0: verify state.** Confirm:
  - `Outcome` at `crates/flux-fs/src/options.rs:80` has three fields;
  - `Ok(Outcome { bytes_copied, metadata_failures, identity_degraded })` at `copy.rs:550`;
  - the §99 guard before the publish at `copy.rs:525-534`;
  - `prepare_file` at `copy.rs:343-370` returns `(parent, parent_path, name)`, and its only caller is
    `run/mod.rs:265`;
  - `Outcome` is built in tests at `exit_code.rs:128,181`, `report.rs:388,411,656` and `options.rs:116`.

  STOP on any mismatch.

- [ ] **Step 1:** in `options.rs`'s `Outcome`, after `identity_degraded`, add:

```rust
    /// The published target's identity, read from the temporary's own handle just before the publishing rename (a
    /// rename keeps the object; cut 7b). `Unavailable` where the platform cannot say.
    pub published_identity: crate::FileIdentity,
```

  At each test construction listed in Step 0 add `published_identity: crate::FileIdentity::Unavailable` in
  `options.rs`, or `flux_fs::FileIdentity::Unavailable` in the CLI files.

- [ ] **Step 2:** in `copy.rs`, between the guard block (ending line 534) and `let published = match opts.publish`, add:

```rust
    // Cut 7b: the object about to be published, read from its own handle - a rename keeps it - so the run records the
    // target's identity without a look-up by name after the rename.
    let published_identity = writer.identity().unwrap_or(FileIdentity::Unavailable);
```

  Change line 550 to `Ok(Outcome { bytes_copied, metadata_failures, identity_degraded, published_identity })`.
  `FileIdentity` and `FileHandle` must be in scope in `copy.rs`; add them to its existing `use flux_fs::{...}` if the
  compiler asks.

- [ ] **Step 3:** change `prepare_file`'s return type to
  `std::result::Result<(F::Dir, &'d Path, &'d OsStr, FileIdentity), CopyError>`, its doc to end "... and the source's
  identity, for the single-file record (cut 7b)", and its last line to
  `Ok((parent, parent_path, name, src_meta.identity))`. In `run/mod.rs:265` destructure
  `let (parent, parent_path, name, _source_identity) = ...`; Task 4 uses it and drops the underscore.

- [ ] **Step 4: the test is already written - implement until it passes.** Append to `copy.rs`'s `mod tests`:

```rust
    #[test]
    fn a_copy_returns_its_published_targets_identity() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        fs.write_file("/s", b"abc");
        let parent = fs.destination_root(Path::new("/d")).unwrap();
        let out = copy_file_at(&fs, Path::new("/s"), &parent, OsStr::new("t"), &opts()).unwrap();
        assert_eq!(out.published_identity, fs.metadata(Path::new("/d/t")).unwrap().identity);
        assert!(matches!(out.published_identity, FileIdentity::Strong(_)));
    }
```

  (Use the module's existing imports; add `DestinationRoot`, `OsStr` or `FileIdentity` to its `use` lines only if the
  compiler asks.)

- [ ] **Step 5: prove and run.** Mutant: read the identity AFTER the publishing rename instead of before, so the fake
  reads the temporary's now-empty path: the test goes red. Restore. Then `cargo test -p flux-fs -p flux-core -p
  flux-cli` passes.

- [ ] **Step 6: commit.**

```bash
git add crates/flux-fs/src/options.rs crates/flux-core/src/copy.rs crates/flux-core/src/run/mod.rs crates/flux-cli/src/exit_code.rs crates/flux-cli/src/report.rs
git commit -m "copy: return the published target's identity, read from the temporary's handle before the rename; prepare_file returns the source's"
```

### Task 4: The run writes version 2 with the record's fields

**Files:** Modify `crates/flux-core/src/run/session.rs`, `crates/flux-core/src/run/place.rs`,
`crates/flux-core/src/run/mod.rs`; tests in `crates/flux-core/src/run/tests.rs`.

- [ ] **Step 0: verify state.** Confirm:
  - `open_operation` at `session.rs:38-148` creates the state with `OperationState::created_v1(...)` at line 71 (after
    Task 1);
  - `record_for(site, cfg, workspace_path)` at lines 172-188 reads `wall_time_ns()` itself;
  - the `Place` trait at `place.rs:27-57`;
  - `FilePlace { dir, dir_shown, target, destination }` at `place.rs:313-319`, built at `run/mod.rs:303-308`.

  STOP on mismatch.

- [ ] **Step 1: the place gives its identities.** In `place.rs` add to the `Place` trait:

```rust
    /// A single file's identities for its record (cut 7b): the source's, and the object at the target name now
    /// (`None` if there is none, `Unavailable` if it cannot be read). Read under the lock, as the state is made. `None`
    /// for a tree.
    fn file_identities(&self) -> Option<(FileIdentity, Option<FileIdentity>)>;
```

  `TreePlace` implements it as `None`. `FilePlace` gains a field `pub(crate) source_identity: FileIdentity` and
  implements:

```rust
    fn file_identities(&self) -> Option<(FileIdentity, Option<FileIdentity>)> {
        let existing = match self.dir.metadata(&self.target) {
            Ok(m) => Some(m.identity),
            Err(e) if e.source.kind() == ErrorKind::NotFound => None,
            Err(_) => Some(FileIdentity::Unavailable),
        };
        Some((self.source_identity, existing))
    }
```

  In `run/mod.rs` rename `_source_identity` to `source_identity` and add `source_identity,` to the `FilePlace { ... }`
  literal.

- [ ] **Step 2: one `now`, and version 2.** In `session.rs`:
  - change `record_for`'s signature to `fn record_for<D: DirHandle>(site: &LockSite<'_, D>, cfg: &RunConfig,
    workspace_path: String, now: u64) -> LockResult<LockRecord>`;
  - delete its `let now = wall_time_ns();`;
  - change its doc's last clause to "...in 7a `last_heartbeat_wall_time` equals `creation_wall_time`; both are the
    run's one `now` (cut 7b)";
  - in `open_operation`, before `for _ in 0..MAX_ATTEMPTS {`, add:

```rust
    // Cut 7b: one wall-clock reading for the state and its record, so the record's fields equal the lock record's
    // (Part 1 decision 7), and one attempt id per run.
    let now = wall_time_ns();
    let attempt_id = crate::ids::new_id();
```

  - replace the `if made.is_none() { ... }` block's state construction with:

```rust
        if made.is_none() {
            let file = place.file_identities().map(|(source, existing)| FileFields {
                artifact_type: ARTIFACT_FILE.to_string(),
                attempt_id: attempt_id.clone(),
                artifact_generation: 1,
                source_identity: identity_text(source),
                target_identity: existing.map(identity_text),
                target_path_key: site.target_path_key(),
                owner_instance_id: cfg.owner_instance_id.clone(),
                boot_session_id: cfg.boot_session_id.clone(),
                creation_wall_time: now.to_string(),
                last_heartbeat_wall_time: now.to_string(),
            });
            let state = OperationState::created(id, place.kind(), place.destination(), now, file);
            if let Err(e) = place.create(&state) {
                return Err(give_back(obtained, e, &lock_shown));
            }
            made = Some(state);
        }
```

  - change `record_for(site, cfg, place.workspace_path(id))` to `record_for(site, cfg, place.workspace_path(id), now)`;
  - extend the imports: `use crate::state::{ARTIFACT_FILE, FileFields, OpState, OperationState, Takeover,
    identity_text, wall_time_ns};`.

- [ ] **Step 3: the tests are already written - implement until they pass.** Append to `run/tests.rs`:

```rust
fn record(fs: &FaultFs, id: &str) -> OperationState {
    decode_state(&fs.read_file(record_path(id)).unwrap()).unwrap()
}

/// `create_new`: the CREATED record (1), TRANSFERRING (2), then the copy's temporary (3), whose write fails: the
/// FAILED record stays, with every field the run wrote at creation.
fn failed_file_run(fs: &FaultFs) -> OperationState {
    fs.on_nth("create_new", 3, |fs| fs.fail_write(std::io::Error::other("injected write")));
    let r = run_file(fs, &cfg());
    assert!(matches!(&r.copy, Some(Err(e)) if e.step == CopyStep::Stream), "{:?}", r.copy);
    record(fs, ID)
}

#[test]
fn a_single_file_record_carries_section_249_1_from_its_creation() {
    let fs = fake();
    let s = failed_file_run(&fs);
    let f = s.file.clone().expect("a version-2 single-file record");
    assert_eq!(s.format_version, crate::state::FORMAT_VERSION);
    assert_eq!(f.artifact_type, "file");
    assert!(crate::ids::is_id(&f.attempt_id) && f.attempt_id != ID, "{}", f.attempt_id);
    assert_eq!(f.artifact_generation, 1);
    let src = fs.metadata(Path::new("/src/a")).unwrap().identity;
    assert_eq!(f.source_identity, crate::state::identity_text(src));
    assert_eq!(f.target_identity, None, "no object stood at /p/t");
    assert_eq!(f.target_path_key, crate::lock::site::hex(b"t"));
    assert_eq!((f.owner_instance_id.as_str(), f.boot_session_id.as_str()), (id(0xee).as_str(), "test-boot"));
    assert_eq!(f.creation_wall_time, s.created_at);
    assert_eq!(f.last_heartbeat_wall_time, s.created_at);
    let other = fake();
    assert_ne!(failed_file_run(&other).file.unwrap().attempt_id, f.attempt_id, "one attempt id per run");
}

#[test]
fn a_replaced_targets_identity_is_recorded_when_the_state_is_made() {
    let fs = fake();
    fs.write_file("/p/t", b"old");
    let old = fs.metadata(Path::new("/p/t")).unwrap().identity;
    let s = failed_file_run(&fs);
    assert_eq!(s.file.unwrap().target_identity, Some(crate::state::identity_text(old)));
}

/// A clean tree run whose COMPLETED workspace cannot be retired, so its manifest stays to be read. `rename_no_replace`:
/// the workspace (1), `a`'s and `sub/b`'s publishes (2, 3), then the retire (4), which fails.
fn kept_tree_manifest(fs: &FaultFs) -> OperationState {
    fs.fail_nth("rename_no_replace", 4, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(fs, &cfg());
    ok(&r);
    manifest(fs, ID)
}

#[test]
fn a_tree_manifest_is_version_2_with_cleanup_keys_and_no_file_fields() {
    let fs = fake();
    let s = kept_tree_manifest(&fs);
    assert_eq!(s.format_version, crate::state::FORMAT_VERSION);
    assert!(s.file.is_none());
    assert!(s.cleanup.is_some());
}

#[test]
fn the_single_file_record_and_its_lock_record_carry_one_time() {
    let fs = fake();
    // As `failed_file_run`, and the lock's own removal at the release fails, so the lock record stays to be read.
    // `remove_file`: CREATED's and TRANSFERRING's temporaries (1, 2), the copy's step-1 sweep (3), the failed copy's
    // temporary (4), FAILED's temporary (5), then the lock (6).
    fs.fail_nth("remove_file", 6, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let s = failed_file_run(&fs);
    let Decoded::Record(rec) = decode(&fs.read_file(T_LOCK).expect("the lock stays")) else {
        panic!("a whole record")
    };
    let f = s.file.unwrap();
    assert_eq!(f.creation_wall_time, rec.creation_wall_time.to_string());
    assert_eq!(f.last_heartbeat_wall_time, rec.last_heartbeat_wall_time.to_string());
}
```

  Both fault counts are reasoned from the call order, not measured. Verify each with `calls(&fs)` before relying on
  it:
  - the 4th `rename_no_replace` must be the retire of `/p/dest/.flux/operations/<ID>`;
  - the 6th `remove_file` must be `/p/t.flux-lock`.

  If either differs, adjust only that number. That changes no value the run writes. Report what you measured.

- [ ] **Step 4: prove and run.** Mutants, each alone, the named test red, then restore:
  - (a) `target_identity: None` always: `a_replaced_targets_identity_is_recorded_when_the_state_is_made` goes red;
  - (b) `attempt_id: cfg.operation_id.clone()`: `a_single_file_record_carries_section_249_1_from_its_creation` goes
    red;
  - (c) `source_identity: identity_text(FileIdentity::Unavailable)`: the same test goes red;
  - (d) `created_v1(...)` in place of `created(...)` (version 1): the same test and
    `a_tree_manifest_is_version_2_with_cleanup_keys_and_no_file_fields` go red;
  - (e) `record_for` reads its own `wall_time_ns()` again instead of taking `now`:
    `the_single_file_record_and_its_lock_record_carry_one_time` goes red. Windows' clock ticks every 100 ns, so two
    reads can coincide. If the test stays green, add `std::thread::sleep(std::time::Duration::from_millis(2));`
    before that read IN THE MUTANT ONLY, and say so.

  Then `cargo test -p flux-core` passes. The 7a tests that count calls (`create_new`, `remove_file`, `write_at_start`)
  must still pass unchanged. Part 1 adds a `metadata` call, but no call of those kinds.

- [ ] **Step 5: commit.**

```bash
git add crates/flux-core/src/run/session.rs crates/flux-core/src/run/place.rs crates/flux-core/src/run/mod.rs crates/flux-core/src/run/tests.rs
git commit -m "run: write version 2 - the single-file record's §249.1 fields at creation, one time for the state and its lock record"
```

### Task 5: The `COMPLETED` write carries `cleanup_pending`, the leftovers and the published identity

**Files:** Modify `crates/flux-core/src/run/mod.rs`; tests in `crates/flux-core/src/run/tests.rs`.

- [ ] **Step 0: verify state.** Confirm:
  - `Ended::Completed { leftovers: bool }` at `run/mod.rs:350`;
  - `complete(place, locked, leftovers: bool, warnings)` at 379-410;
  - the tree's report closure setting `leftovers = true` at 221-226;
  - the file run's `Ok(_) => Ended::Completed { leftovers: false }` at 335.

  STOP on mismatch.

- [ ] **Step 1:** change the enum variant and its doc:

```rust
    /// The walk finished, or the single file was copied (A1). `leftovers`: the temporaries the copy could not remove,
    /// relative to DEST (a tree). `published`: a single file's published target (cut 7b).
    Completed { leftovers: Vec<PathBuf>, published: Option<FileIdentity> },
```

  In `tree`:
  - replace `let mut leftovers = false;` with `let mut leftovers: Vec<PathBuf> = Vec::new();`;
  - replace the closure's `if matches!(...) { leftovers = true; }` with:

```rust
            if let TreeFailureCause::Copy(e) = &f.cause
                && let Some((path, _)) = &e.leftover
            {
                leftovers.push(path.clone());
            }
```

  - change `Ok(_) => Ended::Completed { leftovers },` to `Ok(_) => Ended::Completed { leftovers, published: None },`.

  In `file`, change `Ok(_) => Ended::Completed { leftovers: false },` to
  `Ok(o) => Ended::Completed { leftovers: Vec::new(), published: Some(o.published_identity) },`.

  In `finish`, pass them through:
  `Ended::Completed { leftovers, published } => complete(place, locked, leftovers, published, warnings),`.

- [ ] **Step 2:** in `complete`:
  - change the parameter `leftovers: bool` to `leftovers: Vec<PathBuf>, published: Option<FileIdentity>`;
  - after `locked.state.state = OpState::Completed;` add:

```rust
    // §218 (cut 7b decision 10): cleanup_pending in this same COMPLETED write, before anything is removed, with what
    // the copy left; a single file also records its published target.
    if let Some(c) = &mut locked.state.cleanup {
        c.cleanup_pending = true;
        c.cleanup_pending_artifacts = leftovers.iter().map(|p| native_hex(p)).collect();
    }
    if let (Some(f), Some(id)) = (&mut locked.state.file, published) {
        f.target_identity = Some(identity_text(id));
    }
```

  - change `if leftovers {` to `if !leftovers.is_empty() {`;
  - update the doc comment's "unless a leftover temporary keeps it (G2)" to "unless a leftover temporary keeps it
    (G2); the COMPLETED write carries `cleanup_pending` and the leftovers (cut 7b)";
  - extend the imports: `use crate::state::{OpState, file_names_fit, identity_text, native_hex};` and add
    `FileIdentity` to the `use flux_fs::{...}` line.

- [ ] **Step 3: the tests are already written - implement until they pass.** In `run/tests.rs`, strengthen
  `a_temporary_the_copy_could_not_remove_keeps_the_completed_state` (line 269) by adding at its end:

```rust
    let c = manifest(&fs, ID).cleanup.expect("version 2");
    assert!(c.cleanup_pending);
    assert_eq!(
        c.cleanup_pending_artifacts,
        vec![crate::state::native_hex(Path::new(&format!("a.flux-partial.{ID}")))]
    );
```

  Append:

```rust
#[test]
fn a_completed_record_says_cleanup_pending_and_names_its_target_before_anything_is_removed() {
    let fs = fake();
    // `remove_file`: CREATED and TRANSFERRING clear their temporaries (1, 2), the copy's step-1 sweep (3), COMPLETED
    // (4), then the record itself (5), which fails: the COMPLETED record stays, as a crash there would leave it.
    fs.fail_nth("remove_file", 5, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let r = run_file(&fs, &cfg());
    assert!(matches!(r.copy, Some(Ok(_))), "{:?}", r.copy);
    let s = record(&fs, ID);
    assert_eq!(s.state, OpState::Completed);
    let c = s.cleanup.expect("version 2");
    assert!(c.cleanup_pending && c.cleanup_pending_artifacts.is_empty(), "{c:?}");
    let published = fs.metadata(Path::new("/p/t")).unwrap().identity;
    assert_eq!(s.file.unwrap().target_identity, Some(crate::state::identity_text(published)));
}

#[test]
fn a_clean_trees_completed_manifest_says_cleanup_pending_with_an_empty_list() {
    let fs = fake();
    let s = kept_tree_manifest(&fs);
    assert_eq!(s.state, OpState::Completed);
    let c = s.cleanup.expect("version 2");
    assert!(c.cleanup_pending && c.cleanup_pending_artifacts.is_empty(), "{c:?}");
}
```

  The single-file fault count is reasoned from the call order. Verify with `calls(&fs)` that the 5th `remove_file`
  is the record's removal (`remove_file(/p/t.flux-state.<ID>)`) before relying on it. If it is not, adjust only the
  number. Report what you measured.

- [ ] **Step 4: prove and run.** Mutants, each alone, the named test red, then restore:
  - (a) delete the `c.cleanup_pending = true;` line: both new tests and the strengthened one go red;
  - (b) `c.cleanup_pending_artifacts = Vec::new();` always: the strengthened leftover test goes red;
  - (c) delete the `target_identity` assignment: the single-file test goes red (decode itself refuses a COMPLETED file
    record with no `target_identity`, so the read fails).

  Then `cargo test -p flux-core` passes.

- [ ] **Step 5: commit.**

```bash
git add crates/flux-core/src/run/mod.rs crates/flux-core/src/run/tests.rs
git commit -m "run: the COMPLETED write carries cleanup_pending, the leftover temporaries and the published target's identity (§218)"
```

### Task 6: `--restart` keeps a superseded prior's version

**Files:** tests in `crates/flux-core/src/run/tests.rs` only. `restart.rs:27-31` already rewrites a prior with
`..prior.state.clone()`, which keeps its `format_version`, `cleanup` and `file`, and Task 1's `encode` writes each
record in its own version.

- [ ] **Step 0: verify state.** Confirm `restart.rs:27-31` builds the ABANDONED state as
  `OperationState { state: OpState::Abandoned, superseded_by: Some(own.clone()), ..prior.state.clone() }`. STOP on
  mismatch.

- [ ] **Step 1: the tests are already written.** Strengthen `a_partial_restart_cannot_delete_keeps_its_prior_abandoned`
  (its `prior()` stages a version-1 state, via Task 1's rename) by adding at its end:

```rust
    let bytes = fs.read_file(format!("/p/dest/.flux/operations/{}/manifest", id(5))).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["format_version"], 1, "a version-1 prior is rewritten as version 1");
    assert_eq!(v.as_object().unwrap().len(), 8, "with only its own eight keys");
```

  Append:

```rust
#[test]
fn a_version_2_prior_superseded_by_restart_stays_version_2() {
    let fs = fake();
    for d in ["/p/dest", "/p/dest/.flux", "/p/dest/.flux/operations"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.create_dir(Path::new(&format!("/p/dest/.flux/operations/{}", id(5)))).unwrap();
    let prior =
        OperationState { state: OpState::Failed, ..OperationState::created(&id(5), Kind::Tree, Path::new("/p/dest"), 1, None) };
    fs.write_file(format!("/p/dest/.flux/operations/{}/manifest", id(5)), &prior.encode());
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    // As `a_partial_restart_cannot_delete_keeps_its_prior_abandoned`: the 3rd `remove_file` is the partial.
    fs.fail_nth("remove_file", 3, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &restart());
    ok(&r);
    let kept = manifest(&fs, &id(5));
    assert_eq!((kept.format_version, kept.state), (crate::state::FORMAT_VERSION, OpState::Abandoned));
    assert_eq!(kept.cleanup.map(|c| c.cleanup_pending), Some(false));
}
```

- [ ] **Step 2: prove and run.** Mutant: in `restart.rs`, rebuild the ABANDONED state from
  `OperationState::created(&prior.state.operation_id, prior.state.kind, Path::new(&prior.state.destination_root),
  0, None)` with `state` and `superseded_by` set, discarding the prior's version. The strengthened version-1 test goes
  red. Restore. Then `cargo test -p flux-core` passes.

- [ ] **Step 3: commit.**

```bash
git add crates/flux-core/src/run/tests.rs
git commit -m "test: --restart rewrites a superseded prior in its own version"
```

### Task 7: The repository gates

- [ ] Run `just check` (expect every test passing; the count was 561 at `f380220`, and this part adds 18: 8 in Task 1, 2 in Task 2, 1 in Task 3, 4 in Task 4, 2 in
  Task 5, 1 in Task 6).
- [ ] Run `just check-linux` (547 at `f380220`, plus the same new tests).
- [ ] Run `just check-mac` (clean).

Report each gate's final summary line. A count that differs from the estimate is fine; a failure is not.

### Task 8: Spec amendments and the deferrals

**Files:** `docs/superpowers/specs/2026-10-02-cut-7b-state-v2-and-heartbeat-design.md`, `TODO.md`.

- [ ] In the spec's "Version 2" table, change the `FileIdentity` sentence to:
  "`"strong:<volume>:<index>"`, `"weak:<volume>:<index>"`, or `"unavailable"`; `volume` and `index` are the decimal
  `ObjectId` fields, with no leading zero (`crates/flux-fs/src/fs.rs:52-76`; `index` is a u128)."
- [ ] In "Consistency", replace "`attempt_id`, `owner_instance_id` and `boot_session_id` are 32-hex ids" with
  "`attempt_id` and `owner_instance_id` are 32-hex ids; `boot_session_id` is non-empty (it is a dashed UUID, a boot
  time or `unknown`, `crates/flux-platform/src/lock_file.rs:291-310`)". Also add a bullet: "`artifact_type` is
  `file`; `artifact_generation` is at least 1; `target_path_key` is non-empty lowercase hex."
- [ ] In "Testing", change "including a non-UTF-8 name on Linux (real filesystem)" to "the encoding's losslessness is
  a unit test of `native_hex` (a non-UTF-8 byte on POSIX, an unpaired surrogate on Windows); the run's listing is
  tested on the fake (Part 1 plan, decision 8)".
- [ ] Add to `TODO.md`, before `## Closed`:

```markdown
## Deferred from cut 7b

Each is deferred to the cut that first reads it (cut 7b spec, `docs/superpowers/specs/2026-10-02-cut-7b-state-v2-and-heartbeat-design.md`, Scope).

- [ ] **`configuration_fingerprint` (§19, §121)** - cut 9: read only by resume; most §121 options do not exist yet.
- [ ] **Checkpoints and `last_checkpoint` (§19, §139, §148, §164)** - cuts 8/9: the spec defines them only through
      the WAL and `state.db` (§119's layout).
- [ ] **`roots` (§19, §18.3)** - cut 10: one root until several sources exist.
- [ ] **`last_heartbeat_monotonic` (§229.2)** - cut 9: "diagnostic/current-session data only".
- [ ] **Finishing a prior run's `cleanup_pending` (§21.1 "may", §218)** - cut 9: a COMPLETED prior with
      `cleanup_pending = true` and its artifact list is proceeded past and left.
- [ ] **Stale classification from heartbeat age (§102)** - cut 9.
```

- [ ] Commit:

```bash
git add docs/superpowers/specs/2026-10-02-cut-7b-state-v2-and-heartbeat-design.md TODO.md
git commit -m "docs: cut 7b Part 1 - spec amendments from planning; the deferrals in TODO.md"
```

---

## What Part 2 builds on

Part 2 (the heartbeat) is planned after this part lands, against its code:
- `Held` (`crates/flux-core/src/lock/held.rs`), unchanged here;
- `copy_file_guarded`'s signature, which Task 3 does not change (it changes the `Outcome` it returns);
- `open_operation`'s single `now`;
- the finish's `complete`, as Task 5 leaves it.

## Stand-downs

Plan panel (agy, rounds 1-2; briefs `.clavity/seams/cut7b-p1-plan-panel-r{1,2}.md`):
- FOLDED r1 MG-1: Task 1's version-1 encoding mutant is caught only by the byte-exact test (`68a31e7`).
- FOLDED r2 (citation census, row 10): `FileHandle` is at `crates/flux-fs/src/fs.rs:102-104`, not `99-101`.
- Noted, no change: r2's census row 6 quoted a line that is not in `crates/flux-core/src/run/mod.rs:265`. The plan's
  own citation of that line is correct, so the census row was the error. The three fault counts were confirmed by the
  call-count seat, and each still carries a verify-before-relying instruction for the executing subagent.
