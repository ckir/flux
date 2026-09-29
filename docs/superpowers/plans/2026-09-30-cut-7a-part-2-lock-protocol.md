# Cut 7a Part 2: the lock protocol - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The destination lock's protocol in `flux-core::lock`:
- acquisition, §96.1;
- classifying an existing lock, §240.1-240.4;
- dead-owner recovery, §240.3;
- the `--break-lock` takeover, §240.5;
- revalidation and release, §99;
- a top-level `obtain` that chains them;
- the model-conformance table and its test.

It is built on Part 1's primitives.

**Architecture:**
- Every step is one function whose comment names the model label it implements (`models/lockproto/algorithm.txt`).
- Following F5, `acquire` and `recover` return a `Held` lock with NO record, and `take_over` returns a `Claimed` one.
  Part 3 creates the operation state, then writes the record (`Held::write_record` / `Claimed::overwrite`).
- Protocol branches are unit-tested on the fake filesystem, which gains a hook for "another process acts between two
  of this one's calls". One integration test holds a real OS lock across processes.

**Tech Stack:** Rust 2024; `flux-fs` traits; `flux-core`'s `FaultFs` (tests); `toml` (the conformance test, already a
root dev-dependency).

**Spec:** `docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md`: "The run" steps 1 and 3,
"Classifying an existing lock", "`--restart` and `--break-lock`" (the §240.5 part), "Errors and exit statuses",
"Model conformance". The model: `algorithm.txt` procedures `Classify`, `Acquire`, `Recover`, `TakeOver`, `Publish`,
and the `plain`, `rec` and `brk` processes.

**Part 3** (written after this part lands): the operation state and its crash-safe writes, §21.1 prior state,
`--restart`, the run's finish path, the CLI flags and exit codes, the engine's `still_owned` calls, and the
`DEST/.flux` checks.

---

## What this plan rests on (read and verified at `7a1a358`)

- `crates/flux-fs/src/lock.rs`:
  - `LockCapability::{LocalStrong, RemoteStrong, RemoteUnverified, Unsupported}` with `allows_exclusive()`;
  - `LockFile::{try_lock, read_all(limit), write_at_start, sync_all, identity}`.
- `crates/flux-fs/src/fs.rs`:
  - `DirHandle` has `identity() -> Result<FileIdentity>` (`:237`), `open_dir`, `metadata` (-> `Metadata { len,
    file_type, permissions, modified, identity }`), `remove_file`, `rename_no_replace(from, other: &Self, to)`
    (`:280-285`), `create_lock`, `open_lock` and `lock_capability`;
  - `FileIdentity::{Strong(ObjectId), Weak(ObjectId), Unavailable}` (`:72-76`), `FileType::{File, Dir, Symlink,
    Other}` (`:30-35`).
- `crates/flux-fs/src/error.rs`: `FsError { code, source: io::Error }`, `Code::{SafetyRejected, DestinationError,
  IoError, ...}`.
- `crates/flux-core/src/lock/record.rs`:
  - `LockRecord` (8 fields; `Clone`, `PartialEq`), `encode()`;
  - `decode(&[u8]) -> Decoded::{Record, Uncertain(Uncertain), Foreign}`;
  - `Uncertain` (`Debug`, `Copy`, `PartialEq`), `RECORD_LEN`.
- `crates/flux-core/src/lock/mod.rs` today is only `pub mod record;` (plus a doc comment).
- `crates/flux-core/src/ids.rs`: `new_id()`, `is_id()`.
- `crates/flux-core/src/fault_fs.rs`:
  - The fake is `#[cfg(test)]`, so protocol tests live in the `lock` modules' `mod tests`. `pub struct FaultFs`,
    `pub struct FakeDirHandle` (`:872`), `pub struct FakeLock`.
  - The call keys used below are `create_lock`, `open_lock`, `try_lock`, `read_all`, `write_at_start`,
    `lock_set_len`, `lock_sync_all`, `metadata` (FaultFs::metadata, `:629`), `remove_file` (`:736`),
    `rename_no_replace` (`:703-708`) and `lock_capability`.
  - The helpers are `fail`, `fail_kind`, `fail_always`, `calls`, `called`, `exists`, `read_file`, `write_file` and
    `set_type`.
  - `record(&self, call, key)` begins `let mut g = self.inner.lock().unwrap();` (`:534-540`). `struct Inner` derives
    only `Default` (`:18-19`).
- The protocol labels in `algorithm.txt` (60, measured with a grep of `^\s+(S96_1|S240_1|S240_3|S240_5|S99|S21_1|S251_1)_...:`)
  are listed in Task 9.
- Root dev-dependencies include `toml` and `serde` (`Cargo.toml:24-29`). `flux-core`'s dev-dependencies are
  `flux-platform` and `tempfile`.

## Decisions (forks through AGY-FIRST and the owner, 2026-09-30; seam `.clavity/seams/cut7a-p2-forks.md`)

1. **Keys are lowercase hex (P2-A, owner: A1).**
   - `target_path_key` is the hex of the target's final name in its exact platform bytes (`OsStr::as_encoded_bytes`:
     raw bytes on Unix, WTF-8 on Windows; §103).
   - `complete_lock_key` is `<volume as 16 hex>:<index as 32 hex>/<target_path_key>` when the lock directory's identity
     is Strong, and `none/<target_path_key>` otherwise (§259.6: "where reliably available").
   - A filesystem root's lock has an empty `target_path_key`.
2. **Bounded retries (P2-B, owner ruling after negotiation):**
   - `obtain` makes at most `MAX_ATTEMPTS = 8` passes.
   - `own_lock` retries the OS-native lock at most `OWNLOCK_TRIES = 5` times, 20 ms apart, while the path still names
     its empty file.
   - When the passes run out, the refusal reports what was last seen:
     - an empty or torn lock that NOBODY held (the last pass classified it uncertain and a `--break-lock` takeover had
       to restart): `TARGET_LOCK_UNCERTAIN`;
     - a lock another process held, or anything else: `TARGET_LOCK_BUSY`.
   - Exit 3 either way.
3. **`Held` has no record until Part 3 writes one (P2-C, owner: accepted).** `acquire` and `recover` return an empty
   held lock. The model's `Recover` writes its record at `S240_3_s4_record_*` before deleting the moved file
   (`S240_3_s5`); here the moved file is deleted first and the record comes after the state. That is the spec's
   declared refinement 2 (F5). A crash in between leaves an empty lock: uncertain, cleared by `--break-lock`.
4. **The conformance table lands here (P2-D, owner: D2)**, with `S21_1_s3`, `S21_1_s5_*`, `S21_1_s3_refuse_close` and
   `S99_write` marked `part-3`.
5. **`ARTIFACT_OWNERSHIP_UNCERTAIN` only for a DEAD owner (P2-E, owner: accepted).** A live owner is `TARGET_LOCK_BUSY`
   first. `workspace_path = none` is trusted (a cleanup lock, §120); a dead one is recovered through §240.3 like any
   dead lock.
6. **Classification order.**
   1. Foreign content, or an object that is not a regular file, gives `CONTROL_PLANE_NAMESPACE_CONFLICT`, as the model
      judges content first.
   2. Otherwise a lock another process holds gives `TARGET_LOCK_BUSY`, reporting the record if readable: the spec
      table's first row, §240.2, and ruling 2.
   3. Otherwise an empty, torn, checksum-failed or newer-version record gives uncertain.
   4. Otherwise a valid record: a trusted, present workspace means the owner is dead, and the lock is kept for
      recovery. A missing or untrusted workspace gives `ARTIFACT_OWNERSHIP_UNCERTAIN`.

   Where this differs from the model's `plain` run (a HELD torn lock is `TARGET_LOCK_UNCERTAIN` there, `TARGET_LOCK_BUSY`
   here), it differs only in which refusal is reported. Both refuse and change nothing.
7. **`Held::discard` unlinks WHILE holding the OS-native lock, then closes.** The spec's "The run" step 4 says "the
   lock file, closed first". Closing first would let a `--break-lock` take the empty lock over in the gap, and the
   unlink would then delete the taker's lock: the plan-3 finding recorded at `S96_1_ownlock_wait`. `discard` also
   removes only a file whose identity is still its own.
8. **The takeover's re-read (§240.5 step 5, `S240_5_s5`) restarts on foreign content**, where the model continues to
   step 6. A non-Flux object written into the lock file between classification and takeover must go back through
   classification (`CONTROL_PLANE_NAMESPACE_CONFLICT`), never be overwritten.
9. **`still_owned` fails closed** when the held file's identity is `Unavailable`: a comparison of two `Unavailable`
   values proves nothing. The LocalStrong allowlist's filesystems all give strong identities.
10. **A `workspace_path` is trusted only when the record's `operation_id` passes `is_id`**, so that `operations/<id>`
    can never smuggle a path such as `../x` into the lookup.
11. **Leftover `.broken.*` files.** `obtain` reports the one its own recovery could not delete. Noticing others beside
    the lock needs a directory listing, which `DirHandle` does not offer, so that is Part 3's.

## Ground rules

- Worktree `E:\Rust\flux-engine`, branch `spec/cut-7a`.
- **Step 0 of every task:** confirm each quoted current text before editing. If it differs, STOP and report
  `STATE_MISMATCH: <file>: <what differs>`.
- **Shape-divergence stop:** any changed name, signature, type, error code, `ErrorKind`, constant, or the order of
  protocol steps means STOP and report `[plan] -> [yours] because <reason>`. Exceptions, which you report but do not
  stop for:
  - `cargo fmt` reformatting;
  - dropping an import the compiler reports unused in a test;
  - adding a derive the tests need to compile.
- **Oracle:** the tests in each task are already written. Implement until they pass; never edit an assertion.
- **Formatting:** run `cargo fmt` before every gate.
- **Gates after each task:**
  - `just check` (the Windows host) must exit 0 with nextest reporting every test passed.
  - Tasks 8 and 10 also run `just check-linux` (exit 0, nextest summary all passed) and `just check-mac` (exit 0).
- **Commits:** name the paths; each message ends with a `Co-Authored-By:` line for the model you are.

## File map

| File | Change |
|---|---|
| `crates/flux-core/src/fault_fs.rs` | `on_nth` hooks (Task 1) |
| `crates/flux-core/src/lock/mod.rs` | module list and re-exports (Task 2, extended per task) |
| `crates/flux-core/src/lock/error.rs` (new) | `LockCode`, `Refusal`, `LockError`, `LockResult` |
| `crates/flux-core/src/lock/site.rs` (new) | `LockSite`: lock names, keys, the `.flux-dir.lock` check, workspace trust |
| `crates/flux-core/src/lock/test_support.rs` (new, `cfg(test)`) | the fake's scaffolding shared by the protocol tests |
| `crates/flux-core/src/lock/held.rs` (new) | `Held`, `Released` |
| `crates/flux-core/src/lock/acquire.rs` (new) | `acquire`, `own_lock` (§96.1) |
| `crates/flux-core/src/lock/classify.rs` (new) | `classify` (§240.1) |
| `crates/flux-core/src/lock/recover.rs` (new) | `recover` (§240.3) |
| `crates/flux-core/src/lock/takeover.rs` (new) | `take_over`, `Claimed`, `Overwritten` (§240.5) |
| `crates/flux-core/src/lock/obtain.rs` (new) | `check_capability`, `obtain`, `Mode`, `Obtained` |
| `crates/flux-core/tests/lock_protocol.rs` (new) | the real OS lock across processes |
| `models/lockproto/impl-map.toml` (new), `tests/model_impl_map.rs` (new) | model conformance |

---

### Task 1: the fake's `on_nth` hook

**Files:** Modify `crates/flux-core/src/fault_fs.rs`.

- [ ] **Step 0:**
  - `struct Inner` is preceded by `#[derive(Default)]` and ends with the field `lock_capability:
    Option<LockCapability>,` (Part 1).
  - `fn record(&self, call: String, key: &str) -> Result<()>` begins:

    ```
        let mut g = self.inner.lock().unwrap();
        g.calls.push(call);
        let n = {
            let c = g.call_counts.entry(key.to_string()).or_insert(0);
            *c += 1;
            *c
        };
    ```
- [ ] **Step 1: the test.** Append to the fake's `mod tests`:

```rust
    #[test]
    fn a_hook_runs_just_before_the_nth_call_and_can_drive_the_fake() {
        use flux_fs::FileSystem;
        let fs = FaultFs::new();
        fs.on_nth("metadata", 2, |fs| fs.write_file("/b", b"y"));
        assert!(fs.metadata(Path::new("/b")).is_err(), "first call: the hook has not run");
        assert!(fs.metadata(Path::new("/b")).is_ok(), "second call: the hook ran just before it");
        assert!(fs.metadata(Path::new("/b")).is_ok(), "a hook runs once");
    }
```

- [ ] **Step 2: the hook.** Add to `struct Inner`, after `lock_capability`:

```rust
    /// call name -> (which call, an action). The action runs just BEFORE that call, outside the fake's own lock,
    /// so it can drive the fake itself: "another process acts between two of this one's calls" (cut 7a Part 2's
    /// protocol tests). Consumed on use.
    hooks: HashMap<String, (u32, Box<dyn FnOnce(&FaultFs) + Send>)>,
```

  Add to `impl FaultFs`, after `set_lock_capability`:

```rust
    /// Run `action` just before the `nth` call to `name` (1-based, counted like `fail_nth`).
    pub fn on_nth(&self, name: &str, nth: u32, action: impl FnOnce(&FaultFs) + Send + 'static) {
        self.inner.lock().unwrap().hooks.insert(name.to_string(), (nth, Box::new(action)));
    }
```

  In `record`, directly after the `let n = { ... };` statement, insert:

```rust
        let hook = match g.hooks.get(key) {
            Some((at, _)) if *at == n => g.hooks.remove(key),
            _ => None,
        };
        if let Some((_, action)) = hook {
            // Released first: the action calls back into the fake, and `std::sync::Mutex` is not reentrant.
            drop(g);
            action(&FaultFs { inner: std::sync::Arc::clone(&self.inner) });
            g = self.inner.lock().unwrap();
        }
```

- [ ] **Step 3:** `cargo test -p flux-core a_hook_runs` gives 1 passed. Non-vacuity (do not commit): delete the
  `action(...)` call, and the test must FAIL at "second call". Then run `just check`.
- [ ] **Step 4:** commit `crates/flux-core/src/fault_fs.rs`: `test(core): the fake runs a hook just before the nth call
  (cut 7a Part 2)`.

---

### Task 2: errors, the lock site, and the test scaffolding

**Files:** Create `crates/flux-core/src/lock/error.rs`, `site.rs` and `test_support.rs`; replace
`crates/flux-core/src/lock/mod.rs`.

- [ ] **Step 0:** `crates/flux-core/src/lock/mod.rs` is exactly:

```rust
//! The destination lock (§96.1, §240, §259.6). Part 1 holds only the record codec; the protocol is Part 2.

pub mod record;
```

- [ ] **Step 1:** replace `crates/flux-core/src/lock/mod.rs` with:

```rust
//! The destination lock (§96.1, §240, §259.6): the record codec, and the protocol that the TLA+ model in
//! `models/lockproto/` checks. Each protocol function names the model labels it implements; `impl-map.toml` there
//! maps every label to its function.

pub mod error;
pub mod record;
pub mod site;
#[cfg(test)]
pub(crate) mod test_support;

pub use error::{LockCode, LockError, LockResult, Refusal};
pub use site::{LockSite, SiteKind};
```

- [ ] **Step 2:** create `crates/flux-core/src/lock/error.rs`:

```rust
//! What the lock protocol refuses with, and why.

use super::record::LockRecord;
use flux_fs::FsError;

/// The spec's error codes the lock protocol produces ("Errors and exit statuses"). The CLI maps them to exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockCode {
    TargetLockBusy,
    TargetLockUncertain,
    ControlPlaneNamespaceConflict,
    ArtifactOwnershipUncertain,
    PathComponentInvalid,
    RemoteLockUnsafe,
}

impl LockCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TargetLockBusy => "TARGET_LOCK_BUSY",
            Self::TargetLockUncertain => "TARGET_LOCK_UNCERTAIN",
            Self::ControlPlaneNamespaceConflict => "CONTROL_PLANE_NAMESPACE_CONFLICT",
            Self::ArtifactOwnershipUncertain => "ARTIFACT_OWNERSHIP_UNCERTAIN",
            Self::PathComponentInvalid => "PATH_COMPONENT_INVALID",
            Self::RemoteLockUnsafe => "REMOTE_LOCK_UNSAFE",
        }
    }
}

/// A refusal the protocol decided. Nothing of the destination was changed by the refusing step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: LockCode,
    /// The holder's record, where it was readable (§96.2: a refusal reports the holder).
    pub holder: Option<LockRecord>,
    pub detail: String,
}

#[derive(Debug)]
pub enum LockError {
    Refused(Refusal),
    Io(FsError),
}

impl From<FsError> for LockError {
    fn from(e: FsError) -> Self {
        Self::Io(e)
    }
}

pub type LockResult<T> = std::result::Result<T, LockError>;

pub(crate) fn refuse(code: LockCode, holder: Option<LockRecord>, detail: impl Into<String>) -> LockError {
    LockError::Refused(Refusal { code, holder, detail: detail.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_the_spec_string() {
        assert_eq!(LockCode::TargetLockBusy.as_str(), "TARGET_LOCK_BUSY");
        assert_eq!(LockCode::TargetLockUncertain.as_str(), "TARGET_LOCK_UNCERTAIN");
        assert_eq!(LockCode::ControlPlaneNamespaceConflict.as_str(), "CONTROL_PLANE_NAMESPACE_CONFLICT");
        assert_eq!(LockCode::ArtifactOwnershipUncertain.as_str(), "ARTIFACT_OWNERSHIP_UNCERTAIN");
        assert_eq!(LockCode::PathComponentInvalid.as_str(), "PATH_COMPONENT_INVALID");
        assert_eq!(LockCode::RemoteLockUnsafe.as_str(), "REMOTE_LOCK_UNSAFE");
    }
}
```

- [ ] **Step 3:** create `crates/flux-core/src/lock/site.rs`:

```rust
//! Where a target's lock lives, what it is called, the keys recorded in it, and which workspaces its record may name.

use super::error::{LockCode, LockResult, refuse};
use super::record::LockRecord;
use flux_fs::{DirHandle, FileIdentity, FileType};
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;

/// `P/<T-name>.flux-lock` (§96.1).
pub const LOCK_SUFFIX: &str = ".flux-lock";
/// `T/.flux-root.lock` for a filesystem root (§96.1, spec:4738-4748).
pub const ROOT_LOCK_NAME: &str = ".flux-root.lock";
/// The directory-lock acquirer's name, whose presence a per-name acquirer checks (`S96_1_dircheck`).
pub const DIR_LOCK_NAME: &str = ".flux-dir.lock";
/// The longest lock name 7a creates: 255 bytes on Unix, 255 UTF-16 units on Windows. Longer needs §96.1's
/// directory-lock fallback, which 7a does not implement (`PATH_COMPONENT_INVALID`, refinement 6).
pub const NAME_LIMIT: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteKind {
    /// A directory target `T`: the lock is `P/<T>.flux-lock`, the workspace `T/.flux/operations/<id>/`.
    Directory,
    /// A filesystem-root target `T`: the lock is `T/.flux-root.lock`.
    Root,
    /// A single-file target: the lock is `P/<target>.flux-lock`, the state `P/<target>.flux-state.<id>`.
    File,
}

/// What a trusted, dead owner's recorded workspace looks like on disk (§120).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Workspace {
    Trusted,
    Missing,
    Untrusted,
}

/// A target's lock: the directory that holds the lock file, its name, and the target's own name.
pub struct LockSite<'a, D: DirHandle> {
    dir: &'a D,
    lock_name: OsString,
    target_name: OsString,
    kind: SiteKind,
}

impl<'a, D: DirHandle> LockSite<'a, D> {
    /// A directory target `dest_name` inside `parent`.
    pub fn directory(parent: &'a D, dest_name: &OsStr) -> LockResult<Self> {
        Self::named(parent, dest_name, SiteKind::Directory)
    }

    /// A single-file target `target_name` inside `parent`.
    pub fn file(parent: &'a D, target_name: &OsStr) -> LockResult<Self> {
        Self::named(parent, target_name, SiteKind::File)
    }

    /// A filesystem-root target, whose lock lives inside it.
    pub fn root(dest: &'a D) -> Self {
        Self { dir: dest, lock_name: ROOT_LOCK_NAME.into(), target_name: OsString::new(), kind: SiteKind::Root }
    }

    fn named(dir: &'a D, name: &OsStr, kind: SiteKind) -> LockResult<Self> {
        let mut lock_name = name.to_os_string();
        lock_name.push(LOCK_SUFFIX);
        if name_len(&lock_name) > NAME_LIMIT {
            return Err(refuse(
                LockCode::PathComponentInvalid,
                None,
                format!(
                    "the lock name {} is longer than {NAME_LIMIT} units; the directory-lock fallback is not in this version",
                    lock_name.to_string_lossy()
                ),
            ));
        }
        Ok(Self { dir, lock_name, target_name: name.to_os_string(), kind })
    }

    pub fn dir(&self) -> &'a D {
        self.dir
    }

    pub fn lock_name(&self) -> &OsStr {
        &self.lock_name
    }

    pub fn kind(&self) -> SiteKind {
        self.kind
    }

    /// `<lock-name>.broken.<operation-id>`: where §240.3 step 2 moves a dead owner's lock.
    pub fn broken_name(&self, operation_id: &str) -> OsString {
        let mut n = self.lock_name.clone();
        n.push(".broken.");
        n.push(operation_id);
        n
    }

    /// The target's final name in `FluxPathKey` encoding, as lowercase hex (decision 1). Empty for a root.
    pub fn target_path_key(&self) -> String {
        hex(self.target_name.as_encoded_bytes())
    }

    /// §259.6's `K`: the lock directory's physical identity where it is strong, then the target key (decision 1).
    pub fn complete_lock_key(&self) -> LockResult<String> {
        let prefix = match self.dir.identity()? {
            FileIdentity::Strong(o) => format!("{:016x}:{:032x}", o.volume, o.index),
            FileIdentity::Weak(_) | FileIdentity::Unavailable => "none".to_string(),
        };
        Ok(format!("{prefix}/{}", self.target_path_key()))
    }

    /// `S96_1_dircheck`: a per-name acquirer checks that `P/.flux-dir.lock` is absent. A root's lock is not per-name.
    pub(crate) fn dir_lock_present(&self) -> LockResult<bool> {
        if self.kind == SiteKind::Root {
            return Ok(false);
        }
        match self.dir.metadata(OsStr::new(DIR_LOCK_NAME)) {
            Ok(_) => Ok(true),
            Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// §120: is the dead owner's recorded `workspace_path` one Flux trusts, and is it there (decision 10)?
    pub(crate) fn workspace(&self, record: &LockRecord) -> LockResult<Workspace> {
        let id = record.operation_id.as_str();
        if !crate::ids::is_id(id) {
            return Ok(Workspace::Untrusted);
        }
        let path = record.workspace_path.as_str();
        if path == "none" {
            // A cleanup lock names no workspace (§259.6); a dead one is recovered like any other (decision 5).
            return Ok(Workspace::Trusted);
        }
        let present = match self.kind {
            SiteKind::Directory | SiteKind::Root if path == format!("operations/{id}") => self.operation_dir_exists(id)?,
            SiteKind::File if path == format!("adjacent/{id}") => {
                let mut state = self.target_name.clone();
                state.push(".flux-state.");
                state.push(id);
                match self.dir.metadata(&state) {
                    Ok(m) => m.file_type == FileType::File,
                    Err(e) if e.source.kind() == ErrorKind::NotFound => false,
                    Err(e) => return Err(e.into()),
                }
            }
            _ => return Ok(Workspace::Untrusted),
        };
        Ok(if present { Workspace::Trusted } else { Workspace::Missing })
    }

    /// `T/.flux/operations/<id>/`, each level looked up by metadata first so a missing level is `false`, not an error.
    fn operation_dir_exists(&self, id: &str) -> LockResult<bool> {
        let dest_owned;
        let dest: &D = match self.kind {
            SiteKind::Root => self.dir,
            _ => {
                if !has_dir(self.dir, &self.target_name)? {
                    return Ok(false);
                }
                dest_owned = self.dir.open_dir(&self.target_name)?;
                &dest_owned
            }
        };
        if !has_dir(dest, OsStr::new(".flux"))? {
            return Ok(false);
        }
        let flux = dest.open_dir(OsStr::new(".flux"))?;
        if !has_dir(&flux, OsStr::new("operations"))? {
            return Ok(false);
        }
        let operations = flux.open_dir(OsStr::new("operations"))?;
        has_dir(&operations, OsStr::new(id))
    }
}

fn has_dir<D: DirHandle>(d: &D, name: &OsStr) -> LockResult<bool> {
    match d.metadata(name) {
        Ok(m) => Ok(m.file_type == FileType::Dir),
        Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(windows)]
fn name_len(name: &OsStr) -> usize {
    use std::os::windows::ffi::OsStrExt;
    name.encode_wide().count()
}

#[cfg(not(windows))]
fn name_len(name: &OsStr) -> usize {
    name.as_encoded_bytes().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::test_support::{fake, record, refusal};
    use flux_fs::{DirHandle, FileSystem};
    use std::path::Path;

    #[test]
    fn lock_names_follow_the_target() {
        let (_fs, d) = fake();
        let dir = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        assert_eq!(dir.lock_name(), OsStr::new("dest.flux-lock"));
        assert_eq!(dir.broken_name("ab"), OsString::from("dest.flux-lock.broken.ab"));
        assert_eq!(LockSite::file(&d, OsStr::new("t.bin")).unwrap().lock_name(), OsStr::new("t.bin.flux-lock"));
        assert_eq!(LockSite::root(&d).lock_name(), OsStr::new(".flux-root.lock"));
    }

    #[test]
    fn a_lock_name_over_the_limit_is_path_component_invalid() {
        let (_fs, d) = fake();
        let fits = "a".repeat(NAME_LIMIT - LOCK_SUFFIX.len());
        assert!(LockSite::directory(&d, OsStr::new(&fits)).is_ok(), "exactly {NAME_LIMIT} units fits");
        let over = "a".repeat(NAME_LIMIT - LOCK_SUFFIX.len() + 1);
        assert_eq!(refusal(LockSite::directory(&d, OsStr::new(&over))).code, LockCode::PathComponentInvalid);
    }

    #[test]
    fn the_keys_are_hex_and_carry_the_directory_identity() {
        let (_fs, d) = fake();
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        assert_eq!(site.target_path_key(), "64657374", "hex of b\"dest\"");
        let FileIdentity::Strong(o) = d.identity().unwrap() else { panic!("the fake's directories are strong") };
        assert_eq!(
            site.complete_lock_key().unwrap(),
            format!("{:016x}:{:032x}/64657374", o.volume, o.index)
        );
        assert!(LockSite::root(&d).complete_lock_key().unwrap().ends_with('/'), "a root's target key is empty");
    }

    #[test]
    fn the_directory_lock_check_sees_only_a_per_name_site() {
        let (fs, d) = fake();
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        assert!(!site.dir_lock_present().unwrap());
        fs.write_file("/p/.flux-dir.lock", b"");
        assert!(site.dir_lock_present().unwrap());
        assert!(!LockSite::root(&d).dir_lock_present().unwrap(), "a root lock has no per-name acquirer");
    }

    #[test]
    fn a_workspace_is_trusted_only_in_its_exact_derived_form() {
        let (fs, d) = fake();
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        let id = crate::ids::new_id();
        let ops = format!("operations/{id}");
        assert_eq!(site.workspace(&record(&site, &id, &ops)).unwrap(), Workspace::Missing);
        for dir in ["/p/dest", "/p/dest/.flux", "/p/dest/.flux/operations", &format!("/p/dest/.flux/operations/{id}")] {
            fs.create_dir(Path::new(dir)).unwrap();
        }
        assert_eq!(site.workspace(&record(&site, &id, &ops)).unwrap(), Workspace::Trusted);
        assert_eq!(site.workspace(&record(&site, &id, "none")).unwrap(), Workspace::Trusted, "a cleanup lock");
        let other = crate::ids::new_id();
        assert_eq!(
            site.workspace(&record(&site, &id, &format!("operations/{other}"))).unwrap(),
            Workspace::Untrusted,
            "another operation's workspace"
        );
        assert_eq!(site.workspace(&record(&site, &id, &format!("adjacent/{id}"))).unwrap(), Workspace::Untrusted);
        assert_eq!(
            site.workspace(&record(&site, "../x", "operations/../x")).unwrap(),
            Workspace::Untrusted,
            "an operation id that is not an id is never a path"
        );
    }

    #[test]
    fn a_single_files_workspace_is_its_adjacent_state_record() {
        let (fs, d) = fake();
        let site = LockSite::file(&d, OsStr::new("t.bin")).unwrap();
        let id = crate::ids::new_id();
        let adjacent = format!("adjacent/{id}");
        assert_eq!(site.workspace(&record(&site, &id, &adjacent)).unwrap(), Workspace::Missing);
        fs.write_file(format!("/p/t.bin.flux-state.{id}"), b"{}");
        assert_eq!(site.workspace(&record(&site, &id, &adjacent)).unwrap(), Workspace::Trusted);
        assert_eq!(site.workspace(&record(&site, &id, &format!("operations/{id}"))).unwrap(), Workspace::Untrusted);
    }
}
```

- [ ] **Step 4:** create `crates/flux-core/src/lock/test_support.rs`:

```rust
//! Scaffolding the protocol tests share: a fake filesystem with a directory `/p`, records, and simulated owners.

use super::error::{LockError, LockResult, Refusal};
use super::record::LockRecord;
use super::site::LockSite;
use crate::fault_fs::{FakeDirHandle, FakeLock, FaultFs};
use flux_fs::{DestinationRoot, DirHandle, FileSystem, LockFile};
use std::ffi::OsStr;
use std::path::Path;

/// A fake filesystem and a handle on its directory `/p`, which holds every test's lock.
pub(crate) fn fake() -> (FaultFs, FakeDirHandle) {
    let fs = FaultFs::new();
    fs.create_dir(Path::new("/p")).unwrap();
    let d = fs.destination_root(Path::new("/p")).unwrap();
    (fs, d)
}

/// A record for `site` naming `operation_id` and `workspace_path`.
pub(crate) fn record(site: &LockSite<'_, FakeDirHandle>, operation_id: &str, workspace_path: &str) -> LockRecord {
    LockRecord {
        complete_lock_key: site.complete_lock_key().unwrap(),
        operation_id: operation_id.to_string(),
        owner_instance_id: crate::ids::new_id(),
        boot_session_id: "test-boot".to_string(),
        target_path_key: site.target_path_key(),
        workspace_path: workspace_path.to_string(),
        creation_wall_time: 1,
        last_heartbeat_wall_time: 1,
    }
}

/// A lock whose owner wrote `bytes` and then died: the file stays, and nobody holds its OS-native lock.
pub(crate) fn dead_lock(d: &FakeDirHandle, name: &str, bytes: &[u8]) {
    let lock = d.create_lock(OsStr::new(name)).unwrap();
    assert!(lock.try_lock().unwrap());
    lock.write_at_start(bytes).unwrap();
}

/// A live owner: the returned handle holds the OS-native lock on a file holding `bytes` until it is dropped.
pub(crate) fn live_lock(d: &FakeDirHandle, name: &str, bytes: &[u8]) -> FakeLock {
    let lock = d.create_lock(OsStr::new(name)).unwrap();
    assert!(lock.try_lock().unwrap());
    lock.write_at_start(bytes).unwrap();
    lock
}

/// The refusal a protocol call returned; panics on success or an I/O error.
pub(crate) fn refusal<T>(r: LockResult<T>) -> Refusal {
    match r {
        Err(LockError::Refused(x)) => x,
        Err(LockError::Io(e)) => panic!("expected a refusal, got an I/O error: {e:?}"),
        Ok(_) => panic!("expected a refusal, got success"),
    }
}
```

- [ ] **Step 5:** `cargo test -p flux-core lock::` gives every `lock::error`, `lock::site` and existing `lock::record`
  test passing. Then run `just check`.
- [ ] **Step 6: non-vacuity (do not commit; report each):**
  - `is_id` guard: delete the `if !crate::ids::is_id(id) { ... }` guard. `a_workspace_is_trusted_only_in_its_exact_derived_form`
    must FAIL at "an operation id that is not an id".
  - Name limit: change `> NAME_LIMIT` to `> NAME_LIMIT + 1`. `a_lock_name_over_the_limit_is_path_component_invalid`
    must FAIL.
- [ ] **Step 7:** commit the four files: `feat(core): lock errors, lock sites, keys and workspace trust (cut 7a Part 2)`.

---

### Task 3: `Held`, and acquisition (§96.1)

**Files:** Create `crates/flux-core/src/lock/held.rs` and `crates/flux-core/src/lock/acquire.rs`; modify `mod.rs`.

- [ ] **Step 0:** `mod.rs` is as Task 2 left it.
- [ ] **Step 1:** in `mod.rs`, add `mod acquire;` before `pub mod error;` and `mod held;` after it, and add
  `pub use held::{Held, Released};` after the `pub use error::...` line.
- [ ] **Step 2:** create `crates/flux-core/src/lock/held.rs`:

```rust
//! A lock this run holds: its file's handle and OS-native lock, and the record once one is written.

use super::error::LockResult;
use super::record::{Decoded, LockRecord, RECORD_LEN, decode};
use flux_fs::{DirHandle, FileIdentity, LockFile};
use std::ffi::OsString;
use std::io::ErrorKind;

/// A lock this run holds. It has no record until `write_record` (F5: the record names state that already exists).
pub struct Held<'a, D: DirHandle> {
    pub(crate) dir: &'a D,
    pub(crate) lock_name: OsString,
    pub(crate) lock: D::Lock,
    pub(crate) identity: FileIdentity,
    pub(crate) record: Option<LockRecord>,
}

/// What `release` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Released {
    /// Still owned: the lock was unlinked by name, then closed (`S99_release`, `S99_release_lands`, `S99_close`).
    Unlinked,
    /// No longer this operation's: closed without unlinking (`S99_refuse_close`).
    NotOwned,
}

impl<'a, D: DirHandle> Held<'a, D> {
    pub fn identity(&self) -> &FileIdentity {
        &self.identity
    }

    pub fn record(&self) -> Option<&LockRecord> {
        self.record.as_ref()
    }

    /// `S96_1_record_begin` / `S96_1_record_end`, and after a recovery `S240_3_s4_record_begin` /
    /// `S240_3_s4_record_end`: this operation's record, in one write through the handle holding the lock, flushed.
    /// Called once, after the operation's state exists (F5).
    pub fn write_record(&mut self, record: LockRecord) -> LockResult<()> {
        assert!(self.record.is_none(), "a lock record is written once per tenure");
        self.lock.write_at_start(&record.encode())?;
        self.lock.sync_all()?;
        self.record = Some(record);
        Ok(())
    }

    /// §99, `S99_check` (and `S99_release_check`): the lock path still names the file this run holds, AND the record
    /// in it is still this operation's. Both: a §240.5 takeover overwrites the record in place, so the identity alone
    /// stays the same (spec:4882-4885). Never opens the lock path (the design's "cheap by construction").
    pub fn still_owned(&self) -> LockResult<bool> {
        let Some(mine) = &self.record else { return Ok(false) };
        let at_path = match self.dir.metadata(&self.lock_name) {
            Ok(m) => m.identity,
            Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        // Decision 9: two Unavailable identities prove nothing.
        if matches!(self.identity, FileIdentity::Unavailable) || at_path != self.identity {
            return Ok(false);
        }
        Ok(match decode(&self.lock.read_all(RECORD_LEN)?) {
            Decoded::Record(r) => r.operation_id == mine.operation_id && r.owner_instance_id == mine.owner_instance_id,
            Decoded::Uncertain(_) | Decoded::Foreign => false,
        })
    }

    /// `S99_release_check`, then `S99_release` / `S99_release_lands` / `S99_close`: unlink the lock by name only
    /// while it is still this operation's, then close. Otherwise `S99_refuse_close`: close without unlinking, since the
    /// path may now name another operation's lock.
    pub fn release(self) -> LockResult<Released> {
        if !self.still_owned()? {
            return Ok(Released::NotOwned);
        }
        self.dir.remove_file(&self.lock_name)?;
        Ok(Released::Unlinked)
    }

    /// Remove a lock this run created and wrote no record into (a refusal after acquisition, "The run" step 4).
    /// Decision 7: unlinked WHILE the OS-native lock is still held, and only if the path still names this file; then
    /// closed.
    pub fn discard(self) -> LockResult<()> {
        assert!(self.record.is_none(), "discard is for a lock with no record");
        let at_path = match self.dir.metadata(&self.lock_name) {
            Ok(m) => m.identity,
            Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if at_path == self.identity {
            self.dir.remove_file(&self.lock_name)?;
        }
        Ok(())
    }
}
```

- [ ] **Step 3:** create `crates/flux-core/src/lock/acquire.rs`:

```rust
//! §96.1 acquisition: create the lock exclusively, check the directory lock, take the OS-native lock, verify.

use super::error::{LockCode, LockResult, refuse};
use super::held::Held;
use super::site::LockSite;
use flux_fs::{DirHandle, FileIdentity, LockFile};
use std::io::ErrorKind;
use std::time::Duration;

/// Tries of the OS-native lock on a freshly created file, while its path still names it and it is still empty
/// (decision 2).
pub(crate) const OWNLOCK_TRIES: u32 = 5;
const OWNLOCK_PAUSE: Duration = Duration::from_millis(20);

/// What a pass that must start again last saw (decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Last {
    /// Another process held the OS-native lock.
    HeldByOther,
    /// The last classification found an empty or torn lock that nobody held.
    EmptyOrTornUnheld,
    /// Anything else: the path changed under this pass.
    Other,
}

pub(crate) enum Acquire<'a, D: DirHandle> {
    Acquired(Held<'a, D>),
    /// The lock name is taken: classify what is there.
    Exists,
    Restart(Last),
}

/// `S96_1_create`, `S96_1_dircheck` (and `S96_1_backoff`), then `own_lock`.
pub(crate) fn acquire<'a, D: DirHandle>(site: &LockSite<'a, D>) -> LockResult<Acquire<'a, D>> {
    // S96_1_create
    let lock = match site.dir().create_lock(site.lock_name()) {
        Ok(l) => l,
        Err(e) if e.source.kind() == ErrorKind::AlreadyExists => return Ok(Acquire::Exists),
        Err(e) => return Err(e.into()),
    };
    // S96_1_dircheck: a present directory lock means S96_1_backoff - remove what this pass created, refuse BUSY.
    if site.dir_lock_present()? {
        site.dir().remove_file(site.lock_name())?;
        drop(lock);
        return Err(refuse(
            LockCode::TargetLockBusy,
            None,
            "a directory lock (.flux-dir.lock) is present beside the target's lock",
        ));
    }
    Ok(match own_lock(site, lock)? {
        Ok(held) => Acquire::Acquired(held),
        Err(last) => Acquire::Restart(last),
    })
}

/// `S96_1_ownlock`, `S96_1_ownlock_wait`, `S96_1_ownlock_verify`, `S96_1_ownlock_close`; and the same four steps of a
/// recovery's own lock, `S240_3_s4_lock`, `S240_3_s4_lock_wait`, `S240_3_s4_lock_verify`, `S240_3_s4_lock_close`.
/// Giving up closes the handle and never removes the file by name, which could delete a takeover's lock.
pub(crate) fn own_lock<'a, D: DirHandle>(
    site: &LockSite<'a, D>,
    lock: D::Lock,
) -> LockResult<Result<Held<'a, D>, Last>> {
    let identity = lock.identity()?;
    let mut tries = 1;
    while !lock.try_lock()? {
        // S96_1_ownlock_wait: retry only while the path still names this file and it is still empty.
        if tries >= OWNLOCK_TRIES || !still_empty_at_path(site, &lock, &identity)? {
            return Ok(Err(Last::HeldByOther)); // S96_1_ownlock_close: the handle drops, nothing is unlinked
        }
        tries += 1;
        std::thread::sleep(OWNLOCK_PAUSE);
    }
    // S96_1_ownlock_verify: holding the lock, the path must still name the file, and nobody may have written it.
    if !still_empty_at_path(site, &lock, &identity)? {
        return Ok(Err(Last::Other));
    }
    Ok(Ok(Held { dir: site.dir(), lock_name: site.lock_name().to_os_string(), lock, identity, record: None }))
}

fn still_empty_at_path<D: DirHandle>(site: &LockSite<'_, D>, lock: &D::Lock, identity: &FileIdentity) -> LockResult<bool> {
    let at_path = match site.dir().metadata(site.lock_name()) {
        Ok(m) => m.identity,
        Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    Ok(at_path == *identity && lock.read_all(0)?.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::held::Released;
    use crate::lock::record::{Decoded, decode};
    use crate::lock::test_support::{fake, record, refusal};
    use flux_fs::{DestinationRoot, FileSystem};
    use std::ffi::OsStr;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    const LOCK: &str = "/p/dest.flux-lock";

    fn site(d: &crate::fault_fs::FakeDirHandle) -> LockSite<'_, crate::fault_fs::FakeDirHandle> {
        LockSite::directory(d, OsStr::new("dest")).unwrap()
    }

    fn acquired<'a, D: DirHandle>(a: Acquire<'a, D>) -> Held<'a, D> {
        match a {
            Acquire::Acquired(h) => h,
            Acquire::Exists => panic!("expected Acquired, got Exists"),
            Acquire::Restart(l) => panic!("expected Acquired, got Restart({l:?})"),
        }
    }

    #[test]
    fn a_fresh_lock_is_created_held_and_empty() {
        let (fs, d) = fake();
        let site = site(&d);
        let held = acquired(acquire(&site).unwrap());
        assert!(held.record().is_none(), "F5: no record until the state exists");
        assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b""[..]));
        let other = d.open_lock(OsStr::new("dest.flux-lock")).unwrap();
        assert!(!other.try_lock().unwrap(), "the acquirer holds the OS-native lock");
    }

    #[test]
    fn a_taken_name_is_reported_as_existing() {
        let (fs, d) = fake();
        fs.write_file(LOCK, b"");
        assert!(matches!(acquire(&site(&d)).unwrap(), Acquire::Exists));
    }

    #[test]
    fn a_directory_lock_beside_it_backs_off_busy_and_removes_what_it_created() {
        let (fs, d) = fake();
        fs.write_file("/p/.flux-dir.lock", b"");
        assert_eq!(refusal(acquire(&site(&d))).code, LockCode::TargetLockBusy);
        assert!(!fs.exists(LOCK), "S96_1_backoff removes the lock this pass created");
    }

    #[test]
    fn a_lock_another_handle_takes_on_the_fresh_file_restarts_without_removing_it() {
        let (fs, d) = fake();
        let slot = Arc::new(Mutex::new(None));
        let keep = Arc::clone(&slot);
        fs.on_nth("try_lock", 1, move |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            let other = d.open_lock(OsStr::new("dest.flux-lock")).unwrap();
            assert!(other.try_lock().unwrap());
            *keep.lock().unwrap() = Some(other);
        });
        assert!(matches!(acquire(&site(&d)).unwrap(), Acquire::Restart(Last::HeldByOther)));
        assert!(fs.exists(LOCK), "giving up closes; it never unlinks by name");
        drop(slot);
    }

    #[test]
    fn a_lock_path_replaced_before_the_verify_restarts() {
        let (fs, d) = fake();
        fs.on_nth("try_lock", 1, |fs| {
            fs.rename_no_replace(Path::new(LOCK), Path::new("/p/elsewhere")).unwrap();
            fs.write_file(LOCK, b"");
        });
        assert!(matches!(acquire(&site(&d)).unwrap(), Acquire::Restart(Last::Other)));
    }

    #[test]
    fn a_written_record_is_owned_until_another_operation_overwrites_it() {
        let (fs, d) = fake();
        let site = site(&d);
        let mut held = acquired(acquire(&site).unwrap());
        assert!(!held.still_owned().unwrap(), "no record yet: not provably owned");
        let mine = record(&site, &crate::ids::new_id(), "none");
        held.write_record(mine.clone()).unwrap();
        assert_eq!(decode(&fs.read_file(LOCK).unwrap()), Decoded::Record(mine));
        assert!(held.still_owned().unwrap());
        // A takeover overwrites the record in place: same file, another operation's record.
        let theirs = record(&site, &crate::ids::new_id(), "none");
        d.open_lock(OsStr::new("dest.flux-lock")).unwrap().write_at_start(&theirs.encode()).unwrap();
        assert!(!held.still_owned().unwrap(), "the identity alone is not ownership");
        assert_eq!(held.release().unwrap(), Released::NotOwned);
        assert!(fs.exists(LOCK), "a lock that is not ours is never unlinked");
    }

    #[test]
    fn still_owned_is_false_once_the_path_names_another_file() {
        let (fs, d) = fake();
        let site = site(&d);
        let mut held = acquired(acquire(&site).unwrap());
        held.write_record(record(&site, &crate::ids::new_id(), "none")).unwrap();
        fs.rename_no_replace(Path::new(LOCK), Path::new("/p/aside")).unwrap();
        assert!(!held.still_owned().unwrap(), "the path is empty");
        fs.write_file(LOCK, b"");
        assert!(!held.still_owned().unwrap(), "the path names another file");
    }

    #[test]
    fn release_unlinks_an_owned_lock() {
        let (fs, d) = fake();
        let site = site(&d);
        let mut held = acquired(acquire(&site).unwrap());
        held.write_record(record(&site, &crate::ids::new_id(), "none")).unwrap();
        assert_eq!(held.release().unwrap(), Released::Unlinked);
        assert!(!fs.exists(LOCK));
        let again = d.create_lock(OsStr::new("dest.flux-lock")).unwrap();
        assert!(again.try_lock().unwrap(), "closing released the OS-native lock");
    }

    #[test]
    fn discard_removes_only_its_own_recordless_lock() {
        let (fs, d) = fake();
        let site = site(&d);
        acquired(acquire(&site).unwrap()).discard().unwrap();
        assert!(!fs.exists(LOCK), "its own empty lock is removed");
        let held = acquired(acquire(&site).unwrap());
        fs.rename_no_replace(Path::new(LOCK), Path::new("/p/aside")).unwrap();
        fs.write_file(LOCK, b"someone else's");
        held.discard().unwrap();
        assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b"someone else's"[..]), "another file is never removed");
    }
}
```

- [ ] **Step 4:** `cargo test -p flux-core lock::acquire` gives 9 passed. Then run `just check`.
- [ ] **Step 5: non-vacuity (do not commit; report each):**
  - Drop the record half of `still_owned`: make the match arm `Decoded::Record(_) => true`.
    `a_written_record_is_owned_until_another_operation_overwrites_it` must FAIL.
  - Drop the verify step: make `own_lock` skip the `still_empty_at_path` check after the loop.
    `a_lock_path_replaced_before_the_verify_restarts` must FAIL.
  - Drop the identity check in `discard`: remove the file unconditionally. `discard_removes_only_its_own_recordless_lock`
    must FAIL.
- [ ] **Step 6:** commit `held.rs`, `acquire.rs` and `mod.rs`: `feat(core): acquire a destination lock (§96.1) and
  hold it (cut 7a Part 2)`.

---

### Task 4: classification (§240.1)

**Files:** Create `crates/flux-core/src/lock/classify.rs`; modify `mod.rs`.

- [ ] **Step 0:** `mod.rs` has `mod acquire;` and `mod held;` (Task 3).
- [ ] **Step 1:** in `mod.rs`, add `mod classify;` after `mod acquire;`.
- [ ] **Step 2:** create `crates/flux-core/src/lock/classify.rs`:

```rust
//! §240.1 classification of an existing lock (decision 6's order).

use super::error::LockResult;
use super::record::{Decoded, LockRecord, RECORD_LEN, Uncertain, decode};
use super::site::{LockSite, Workspace};
use flux_fs::{Code, DirHandle, FileIdentity, LockFile};
use std::io::ErrorKind;

pub(crate) enum Classified<D: DirHandle> {
    /// Nothing at the lock path any more: acquire again.
    Vanished,
    /// Another process holds the OS-native lock (§240.2): `TARGET_LOCK_BUSY`, with its record if readable.
    Busy(Option<LockRecord>),
    /// Empty, torn, checksum-failed or newer-version, and nobody holds it: `TARGET_LOCK_UNCERTAIN`.
    Uncertain(Uncertain),
    /// Not a Flux lock: `CONTROL_PLANE_NAMESPACE_CONFLICT`, never overwritten.
    Foreign,
    /// A dead owner whose recorded workspace is missing or untrusted: `ARTIFACT_OWNERSHIP_UNCERTAIN`.
    Untrusted(LockRecord),
    /// A dead owner with a trusted workspace. The handle and its OS-native lock are KEPT for §240.3 recovery.
    Dead { lock: D::Lock, identity: FileIdentity, record: LockRecord },
}

/// `S240_1_open`, `S240_1_trylock`, `S240_1_read`, then `S240_1_close` unless the lock is kept for recovery.
pub(crate) fn classify<D: DirHandle>(site: &LockSite<'_, D>) -> LockResult<Classified<D>> {
    // S240_1_open: without creating it; a link, a directory or another non-regular object is not a lock.
    let lock = match site.dir().open_lock(site.lock_name()) {
        Ok(l) => l,
        Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok(Classified::Vanished),
        Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => return Ok(Classified::Foreign),
        Err(e) => return Err(e.into()),
    };
    // S240_1_trylock
    let got = lock.try_lock()?;
    // S240_1_read
    let decoded = decode(&lock.read_all(RECORD_LEN)?);
    // S240_1_close happens as `lock` drops on every return except `Dead`.
    match decoded {
        Decoded::Foreign => Ok(Classified::Foreign),
        Decoded::Record(record) if !got => Ok(Classified::Busy(Some(record))),
        Decoded::Uncertain(_) if !got => Ok(Classified::Busy(None)),
        Decoded::Uncertain(why) => Ok(Classified::Uncertain(why)),
        Decoded::Record(record) => match site.workspace(&record)? {
            Workspace::Trusted => {
                let identity = lock.identity()?;
                Ok(Classified::Dead { lock, identity, record })
            }
            Workspace::Missing | Workspace::Untrusted => Ok(Classified::Untrusted(record)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FakeDirHandle;
    use crate::lock::test_support::{dead_lock, fake, live_lock, record};
    use flux_fs::{FileSystem, FileType};
    use std::ffi::OsStr;
    use std::path::Path;

    const NAME: &str = "dest.flux-lock";

    fn site(d: &FakeDirHandle) -> LockSite<'_, FakeDirHandle> {
        LockSite::directory(d, OsStr::new("dest")).unwrap()
    }

    fn workspace(fs: &crate::fault_fs::FaultFs, id: &str) {
        for dir in ["/p/dest", "/p/dest/.flux", "/p/dest/.flux/operations", &format!("/p/dest/.flux/operations/{id}")] {
            fs.create_dir(Path::new(dir)).unwrap();
        }
    }

    #[test]
    fn nothing_at_the_path_has_vanished() {
        let (_fs, d) = fake();
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Vanished));
    }

    #[test]
    fn a_held_lock_is_busy_and_reports_its_record_when_readable() {
        let (_fs, d) = fake();
        let site = site(&d);
        let rec = record(&site, &crate::ids::new_id(), "none");
        let _live = live_lock(&d, NAME, &rec.encode());
        assert!(matches!(classify(&site).unwrap(), Classified::Busy(Some(r)) if r == rec));
    }

    #[test]
    fn a_held_empty_lock_is_busy_not_uncertain() {
        // An acquirer inside F5's window: created and held, no record yet (decision 6).
        let (_fs, d) = fake();
        let _live = live_lock(&d, NAME, b"");
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Busy(None)));
    }

    #[test]
    fn foreign_content_is_foreign_even_when_held() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"hello, not a lock");
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Foreign));
        let (_fs, d) = fake();
        let _live = live_lock(&d, NAME, b"hello, not a lock");
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Foreign), "content is judged first");
    }

    #[test]
    fn an_object_that_is_not_a_regular_file_is_foreign() {
        let (fs, d) = fake();
        fs.create_dir(Path::new("/p/dest.flux-lock")).unwrap();
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Foreign), "a directory");
        let (fs, d) = fake();
        fs.write_file("/p/dest.flux-lock", b"");
        fs.set_type("/p/dest.flux-lock", FileType::Symlink);
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Foreign), "a link");
    }

    #[test]
    fn an_unheld_empty_or_torn_lock_is_uncertain() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"");
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Uncertain(Uncertain::Empty)));
        let (_fs, d) = fake();
        dead_lock(&d, NAME, &[0u8; 4096]);
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Uncertain(Uncertain::Torn)));
    }

    #[test]
    fn a_dead_owner_with_its_workspace_is_dead_and_the_lock_is_kept() {
        let (fs, d) = fake();
        let site = site(&d);
        let id = crate::ids::new_id();
        workspace(&fs, &id);
        let rec = record(&site, &id, &format!("operations/{id}"));
        dead_lock(&d, NAME, &rec.encode());
        let Classified::Dead { record: seen, .. } = classify(&site).unwrap() else { panic!("expected Dead") };
        assert_eq!(seen, rec);
        let kept = classify(&site);
        assert!(matches!(kept.unwrap(), Classified::Dead { .. }), "the first result was dropped, releasing the lock");
    }

    #[test]
    fn the_dead_classification_keeps_the_os_lock_while_it_lives() {
        let (fs, d) = fake();
        let site = site(&d);
        let id = crate::ids::new_id();
        workspace(&fs, &id);
        dead_lock(&d, NAME, &record(&site, &id, &format!("operations/{id}")).encode());
        let dead = classify(&site).unwrap();
        assert!(matches!(dead, Classified::Dead { .. }));
        let other = d.open_lock(OsStr::new(NAME)).unwrap();
        assert!(!other.try_lock().unwrap(), "kept for recovery");
        drop(dead);
        assert!(other.try_lock().unwrap());
    }

    #[test]
    fn a_dead_owner_whose_workspace_is_missing_is_untrusted_and_released() {
        let (_fs, d) = fake();
        let site = site(&d);
        let id = crate::ids::new_id();
        let rec = record(&site, &id, &format!("operations/{id}"));
        dead_lock(&d, NAME, &rec.encode());
        assert!(matches!(classify(&site).unwrap(), Classified::Untrusted(r) if r == rec));
        assert!(d.open_lock(OsStr::new(NAME)).unwrap().try_lock().unwrap(), "closed on refusal");
    }

    #[test]
    fn a_dead_cleanup_lock_is_dead() {
        let (_fs, d) = fake();
        let site = site(&d);
        dead_lock(&d, NAME, &record(&site, &crate::ids::new_id(), "none").encode());
        assert!(matches!(classify(&site).unwrap(), Classified::Dead { .. }));
    }
}
```

- [ ] **Step 3:** `cargo test -p flux-core lock::classify` gives 10 passed. Then run `just check`.
- [ ] **Step 4: non-vacuity (do not commit; report each):**
  - Swap the order so a held uncertain lock is uncertain: move the `Decoded::Uncertain(why) =>` arm above the
    `Decoded::Uncertain(_) if !got` arm. `a_held_empty_lock_is_busy_not_uncertain` must FAIL.
  - Treat a missing workspace as trusted: `Workspace::Trusted | Workspace::Missing =>` in the `Dead` arm.
    `a_dead_owner_whose_workspace_is_missing_is_untrusted_and_released` must FAIL.
- [ ] **Step 5:** commit `classify.rs` and `mod.rs`: `feat(core): classify an existing lock (§240.1) (cut 7a Part 2)`.

---

### Task 5: recovery from a dead owner (§240.3)

**Files:** Create `crates/flux-core/src/lock/recover.rs`; modify `mod.rs`.

- [ ] **Step 0:** `mod.rs` has `mod classify;` (Task 4).
- [ ] **Step 1:** in `mod.rs`, add `mod recover;` after `mod held;`.
- [ ] **Step 2:** create `crates/flux-core/src/lock/recover.rs`:

```rust
//! §240.3: replace a dead owner's lock by moving it aside. The caller classified it `Dead` and still holds its handle
//! and OS-native lock. The new lock is returned EMPTY: the record comes after the state (decision 3, F5).

use super::acquire::{Last, own_lock};
use super::error::LockResult;
use super::held::Held;
use super::record::{Decoded, LockRecord, RECORD_LEN, decode};
use super::site::LockSite;
use flux_fs::{DirHandle, FileIdentity, LockFile};
use std::ffi::OsString;
use std::io::ErrorKind;

pub(crate) enum Recovered<'a, D: DirHandle> {
    /// A new, empty, held lock; plus the moved-aside file if its deletion failed (a warning, decision 11).
    Held { held: Held<'a, D>, leftover: Option<OsString> },
    Restart(Last),
}

/// `S240_3_s1` to `S240_3_s5`, with `S240_3_putback`, `S240_3_s4_drop`, `S240_3_restart` and `S240_3_release`.
pub(crate) fn recover<'a, D: DirHandle>(
    site: &LockSite<'a, D>,
    old: D::Lock,
    old_identity: FileIdentity,
    seen: &LockRecord,
    operation_id: &str,
) -> LockResult<Recovered<'a, D>> {
    let dir = site.dir();
    // S240_3_s1: the path still names the same dead owner's file, holding the same record.
    let at_path = match dir.metadata(site.lock_name()) {
        Ok(m) => Some(m.identity),
        Err(e) if e.source.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if at_path.as_ref() != Some(&old_identity) || !holds(&old, seen)? {
        return Ok(Recovered::Restart(Last::Other)); // S240_3_restart; S240_3_release drops `old`
    }
    // S240_3_s2: only one of several concurrent recoverers can move it.
    let broken = site.broken_name(operation_id);
    match dir.rename_no_replace(site.lock_name(), dir, &broken) {
        Ok(()) => {}
        Err(e) if matches!(e.source.kind(), ErrorKind::NotFound | ErrorKind::AlreadyExists) => {
            return Ok(Recovered::Restart(Last::Other));
        }
        Err(e) => return Err(e.into()),
    }
    // S240_3_s3: the moved file is the one step 1 re-read, by identity AND record (a takeover rewrites in place).
    let moved = match dir.metadata(&broken) {
        Ok(m) => Some(m.identity),
        Err(e) if e.source.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if moved.as_ref() != Some(&old_identity) || !holds(&old, seen)? {
        // S240_3_putback: back without replacing, then start again.
        let _ = dir.rename_no_replace(&broken, dir, site.lock_name());
        return Ok(Recovered::Restart(Last::Other));
    }
    // S240_3_s4: its own lock, exclusively.
    let new = match dir.create_lock(site.lock_name()) {
        Ok(l) => l,
        Err(e) if e.source.kind() == ErrorKind::AlreadyExists => {
            // S240_3_s4_drop: another operation owns the target now; the moved file's owner is dead, so drop it.
            drop(old);
            let _ = dir.remove_file(&broken);
            return Ok(Recovered::Restart(Last::Other));
        }
        Err(e) => return Err(e.into()),
    };
    let held = match own_lock(site, new)? {
        Ok(h) => h,
        Err(last) => {
            // S240_3_s4_lock_close, then S240_3_s4_drop.
            drop(old);
            let _ = dir.remove_file(&broken);
            return Ok(Recovered::Restart(last));
        }
    };
    // S240_3_s5: delete the moved file (S240_3_release closes `old` first). A failure leaves it for a later
    // cleanup; the crash table accepts the same leftover.
    drop(old);
    let leftover = match dir.remove_file(&broken) {
        Ok(()) => None,
        Err(_) => Some(broken),
    };
    Ok(Recovered::Held { held, leftover })
}

/// The record read through the kept handle is still exactly `seen`.
fn holds<L: LockFile>(lock: &L, seen: &LockRecord) -> LockResult<bool> {
    Ok(matches!(decode(&lock.read_all(RECORD_LEN)?), Decoded::Record(r) if &r == seen))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::{FakeDirHandle, FaultFs};
    use crate::lock::classify::{Classified, classify};
    use crate::lock::test_support::{dead_lock, fake, record};
    use flux_fs::{Code, DestinationRoot, FileSystem};
    use std::ffi::OsStr;
    use std::path::Path;

    const NAME: &str = "dest.flux-lock";
    const LOCK: &str = "/p/dest.flux-lock";

    fn site(d: &FakeDirHandle) -> LockSite<'_, FakeDirHandle> {
        LockSite::directory(d, OsStr::new("dest")).unwrap()
    }

    /// A dead owner's lock with a present workspace, classified; returns the kept handle, its identity, the record.
    fn dead(fs: &FaultFs, site: &LockSite<'_, FakeDirHandle>) -> (crate::fault_fs::FakeLock, FileIdentity, LockRecord) {
        let id = crate::ids::new_id();
        for dir in ["/p/dest", "/p/dest/.flux", "/p/dest/.flux/operations", &format!("/p/dest/.flux/operations/{id}")] {
            fs.create_dir(Path::new(dir)).unwrap();
        }
        let rec = record(site, &id, &format!("operations/{id}"));
        dead_lock(site.dir(), NAME, &rec.encode());
        match classify(site).unwrap() {
            Classified::Dead { lock, identity, record } => (lock, identity, record),
            _ => panic!("expected Dead"),
        }
    }

    #[test]
    fn a_dead_owners_lock_is_replaced_by_an_empty_held_lock() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        let me = crate::ids::new_id();
        let Recovered::Held { held, leftover } = recover(&site, old, old_id.clone(), &rec, &me).unwrap() else {
            panic!("expected Held")
        };
        assert_eq!(leftover, None);
        assert!(held.record().is_none(), "the record comes after the state (F5)");
        assert_ne!(held.identity(), &old_id, "a new file");
        assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b""[..]));
        assert!(!fs.exists(format!("/p/dest.flux-lock.broken.{me}")), "S240_3_s5 deleted the moved file");
    }

    #[test]
    fn a_record_changed_since_classification_restarts_without_moving_anything() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        let other = record(&site, &crate::ids::new_id(), "none");
        d.open_lock(OsStr::new(NAME)).unwrap().write_at_start(&other.encode()).unwrap();
        assert!(matches!(recover(&site, old, old_id, &rec, &crate::ids::new_id()).unwrap(), Recovered::Restart(_)));
        assert_eq!(fs.read_file(LOCK), Some(other.encode()), "S240_3_s1 moved nothing");
    }

    #[test]
    fn a_taken_move_aside_name_restarts() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        let me = crate::ids::new_id();
        fs.write_file(format!("/p/dest.flux-lock.broken.{me}"), b"x");
        assert!(matches!(recover(&site, old, old_id, &rec, &me).unwrap(), Recovered::Restart(_)));
        assert_eq!(fs.read_file(LOCK), Some(rec.encode()), "the lock stays where it was");
    }

    #[test]
    fn a_file_rewritten_as_it_moved_is_put_back() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        let other = record(&site, &crate::ids::new_id(), "none");
        let bytes = other.encode();
        fs.on_nth("rename_no_replace", 1, move |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            d.open_lock(OsStr::new(NAME)).unwrap().write_at_start(&bytes).unwrap();
        });
        let me = crate::ids::new_id();
        assert!(matches!(recover(&site, old, old_id, &rec, &me).unwrap(), Recovered::Restart(_)));
        assert_eq!(fs.read_file(LOCK), Some(other.encode()), "S240_3_putback restored it at the lock path");
        assert!(!fs.exists(format!("/p/dest.flux-lock.broken.{me}")));
    }

    #[test]
    fn another_lock_created_after_the_move_wins_and_the_moved_file_is_dropped() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        // `dead` created the lock once; recovery's create is the second `create_lock`.
        fs.on_nth("create_lock", 2, |fs| fs.write_file(LOCK, b""));
        let me = crate::ids::new_id();
        assert!(matches!(recover(&site, old, old_id, &rec, &me).unwrap(), Recovered::Restart(_)));
        assert!(!fs.exists(format!("/p/dest.flux-lock.broken.{me}")), "S240_3_s4_drop");
        assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b""[..]), "the other operation's lock is untouched");
    }

    #[test]
    fn a_moved_file_that_cannot_be_deleted_is_reported_as_a_leftover() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        fs.fail("remove_file", Code::IoError);
        let me = crate::ids::new_id();
        let Recovered::Held { leftover, .. } = recover(&site, old, old_id, &rec, &me).unwrap() else {
            panic!("expected Held")
        };
        assert_eq!(leftover, Some(OsString::from(format!("dest.flux-lock.broken.{me}"))));
    }
}
```

- [ ] **Step 3:** `cargo test -p flux-core lock::recover` gives 6 passed. Then run `just check`.
- [ ] **Step 4: non-vacuity (do not commit; report each):**
  - Drop the s3 record half: make the s3 condition `moved.as_ref() != Some(&old_identity)` only.
    `a_file_rewritten_as_it_moved_is_put_back` must FAIL.
  - Drop the s1 re-read: delete the s1 `if ... { return ...Restart }`.
    `a_record_changed_since_classification_restarts_without_moving_anything` must FAIL.
- [ ] **Step 5:** commit `recover.rs` and `mod.rs`: `feat(core): recover a dead owner's lock (§240.3) (cut 7a Part 2)`.

---

### Task 6: the `--break-lock` takeover (§240.5)

**Files:** Create `crates/flux-core/src/lock/takeover.rs`; modify `mod.rs`.

- [ ] **Step 0:** `mod.rs` has `mod recover;` (Task 5).
- [ ] **Step 1:** in `mod.rs`, add `mod takeover;` after `pub mod site;`, and `pub use takeover::{Claimed,
  Overwritten};` after `pub use site::...`.
- [ ] **Step 2:** create `crates/flux-core/src/lock/takeover.rs`:

```rust
//! §240.5: take an uncertain lock over IN PLACE, so the lock path is never empty. Steps 1-5 here; step 6's overwrite
//! waits until Part 3 has created this operation's state (F5, the design's refinement 8).

use super::acquire::Last;
use super::error::{LockCode, LockResult, refuse};
use super::held::Held;
use super::record::{Decoded, LockRecord, RECORD_LEN, Uncertain, decode};
use super::site::LockSite;
use flux_fs::{Code, DirHandle, FileIdentity, LockCapability, LockFile};
use std::ffi::OsString;
use std::io::ErrorKind;

/// An uncertain lock this run has opened and locked, whose record it has not yet overwritten.
pub struct Claimed<'a, D: DirHandle> {
    dir: &'a D,
    lock_name: OsString,
    lock: D::Lock,
    identity: FileIdentity,
    prior: Uncertain,
}

/// What `Claimed::overwrite` came to.
pub enum Overwritten<'a, D: DirHandle> {
    Held(Held<'a, D>),
    /// The lock path no longer names the claimed file: start again (§240.5 step 6, amended 2026-09-17).
    Restart,
}

pub(crate) enum TakeOver<'a, D: DirHandle> {
    Claimed(Claimed<'a, D>),
    Restart(Last),
}

/// `S240_5_s1` to `S240_5_s5`; `S240_5_close` is `lock` dropping on every non-claimed return.
pub(crate) fn take_over<'a, D: DirHandle>(
    site: &LockSite<'a, D>,
    capability: LockCapability,
    reported: Uncertain,
) -> LockResult<TakeOver<'a, D>> {
    // S240_5_s1: strong identity and OS-native locks, decided from the capability, never by a trial lock.
    if !matches!(site.dir().identity()?, FileIdentity::Strong(_)) || !capability.allows_exclusive() {
        return Err(refuse(
            LockCode::TargetLockUncertain,
            None,
            "--break-lock needs strong file identity and a local filesystem with OS-native locks",
        ));
    }
    // S240_5_s2: open the existing file without creating it; gone or not a regular file, start again.
    let lock = match site.dir().open_lock(site.lock_name()) {
        Ok(l) => l,
        Err(e)
            if e.source.kind() == ErrorKind::NotFound
                || matches!(e.code, Code::SafetyRejected | Code::DestinationError) =>
        {
            return Ok(TakeOver::Restart(Last::Other));
        }
        Err(e) => return Err(e.into()),
    };
    // S240_5_s3: the OS-native lock without waiting; held means the owner or another takeover is alive.
    if !lock.try_lock()? {
        return Err(refuse(LockCode::TargetLockBusy, None, "another process holds the lock"));
    }
    // S240_5_s4: the open file is still the one at the lock path.
    let identity = lock.identity()?;
    if at_path(site.dir(), &site.lock_name().to_os_string())? != Some(identity.clone()) {
        return Ok(TakeOver::Restart(Last::Other));
    }
    // S240_5_s5: a record written meanwhile is another holder (and, locked by us, a dead one: recovery's case), and
    // foreign content goes back through classification (decision 8). Still unreadable: continue.
    match decode(&lock.read_all(RECORD_LEN)?) {
        Decoded::Uncertain(_) => {}
        Decoded::Record(_) | Decoded::Foreign => return Ok(TakeOver::Restart(Last::Other)),
    }
    Ok(TakeOver::Claimed(Claimed {
        dir: site.dir(),
        lock_name: site.lock_name().to_os_string(),
        lock,
        identity,
        prior: reported,
    }))
}

impl<'a, D: DirHandle> Claimed<'a, D> {
    /// What the classification read in the prior holder's place (its record was unreadable).
    pub fn prior(&self) -> Uncertain {
        self.prior
    }

    /// `S240_5_s6_write_begin`, `S240_5_s6_write_end`, `S240_5_s6_flush`, `S240_5_s6`: overwrite the record in place
    /// in one write, flush, and check the identity against the lock path again. Called after this operation's state
    /// exists (F5). A write or flush failure is an error after the write (exit 1).
    pub fn overwrite(self, record: LockRecord) -> LockResult<Overwritten<'a, D>> {
        self.lock.write_at_start(&record.encode())?;
        self.lock.sync_all()?;
        if at_path(self.dir, &self.lock_name)? != Some(self.identity.clone()) {
            return Ok(Overwritten::Restart);
        }
        Ok(Overwritten::Held(Held {
            dir: self.dir,
            lock_name: self.lock_name,
            lock: self.lock,
            identity: self.identity,
            record: Some(record),
        }))
    }
}

fn at_path<D: DirHandle>(dir: &D, name: &OsString) -> LockResult<Option<FileIdentity>> {
    match dir.metadata(name) {
        Ok(m) => Ok(Some(m.identity)),
        Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FakeDirHandle;
    use crate::lock::test_support::{dead_lock, fake, live_lock, record, refusal};
    use flux_fs::FileSystem;
    use std::ffi::OsStr;
    use std::path::Path;

    const NAME: &str = "dest.flux-lock";
    const LOCK: &str = "/p/dest.flux-lock";
    const STRONG: LockCapability = LockCapability::LocalStrong;

    fn site(d: &FakeDirHandle) -> LockSite<'_, FakeDirHandle> {
        LockSite::directory(d, OsStr::new("dest")).unwrap()
    }

    fn claimed<'a>(t: TakeOver<'a, FakeDirHandle>) -> Claimed<'a, FakeDirHandle> {
        match t {
            TakeOver::Claimed(c) => c,
            TakeOver::Restart(l) => panic!("expected Claimed, got Restart({l:?})"),
        }
    }

    #[test]
    fn an_unsupported_capability_stays_uncertain() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"");
        let r = take_over(&site(&d), LockCapability::Unsupported, Uncertain::Empty);
        assert_eq!(refusal(r).code, LockCode::TargetLockUncertain);
    }

    #[test]
    fn an_empty_lock_is_claimed_then_overwritten_in_place() {
        let (fs, d) = fake();
        let site = site(&d);
        dead_lock(&d, NAME, b"");
        let before = d.metadata(OsStr::new(NAME)).unwrap().identity;
        let c = claimed(take_over(&site, STRONG, Uncertain::Empty).unwrap());
        assert_eq!(c.prior(), Uncertain::Empty);
        let mine = record(&site, &crate::ids::new_id(), "none");
        let Overwritten::Held(held) = c.overwrite(mine.clone()).unwrap() else { panic!("expected Held") };
        assert_eq!(fs.read_file(LOCK), Some(mine.encode()));
        assert_eq!(held.identity(), &before, "the same file: never an empty lock path");
        assert!(held.still_owned().unwrap());
    }

    #[test]
    fn a_held_lock_is_busy() {
        let (_fs, d) = fake();
        let _live = live_lock(&d, NAME, b"");
        assert_eq!(refusal(take_over(&site(&d), STRONG, Uncertain::Empty)).code, LockCode::TargetLockBusy);
    }

    #[test]
    fn a_vanished_lock_restarts() {
        let (_fs, d) = fake();
        assert!(matches!(take_over(&site(&d), STRONG, Uncertain::Empty).unwrap(), TakeOver::Restart(_)));
    }

    #[test]
    fn a_record_or_foreign_content_written_meanwhile_restarts() {
        let (_fs, d) = fake();
        let site = site(&d);
        dead_lock(&d, NAME, &record(&site, &crate::ids::new_id(), "none").encode());
        assert!(matches!(take_over(&site, STRONG, Uncertain::Empty).unwrap(), TakeOver::Restart(_)));
        let (fs, d) = fake();
        dead_lock(&d, NAME, b"");
        fs.on_nth("read_all", 1, |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            d.open_lock(OsStr::new(NAME)).unwrap().write_at_start(b"hello, not a lock").unwrap();
        });
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        assert!(matches!(take_over(&site, STRONG, Uncertain::Empty).unwrap(), TakeOver::Restart(_)), "decision 8");
        assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b"hello, not a lock"[..]), "never overwritten");
    }

    #[test]
    fn a_path_replaced_before_the_identity_check_restarts() {
        let (fs, d) = fake();
        dead_lock(&d, NAME, b"");
        fs.on_nth("metadata", 1, |fs| {
            fs.rename_no_replace(Path::new(LOCK), Path::new("/p/aside")).unwrap();
            fs.write_file(LOCK, b"");
        });
        assert!(matches!(take_over(&site(&d), STRONG, Uncertain::Empty).unwrap(), TakeOver::Restart(_)));
    }

    #[test]
    fn an_overwrite_whose_file_left_the_path_restarts() {
        let (fs, d) = fake();
        let site = site(&d);
        dead_lock(&d, NAME, b"");
        let c = claimed(take_over(&site, STRONG, Uncertain::Empty).unwrap());
        fs.on_nth("lock_sync_all", 1, |fs| {
            fs.rename_no_replace(Path::new(LOCK), Path::new("/p/aside")).unwrap();
            fs.write_file(LOCK, b"");
        });
        let r = c.overwrite(record(&site, &crate::ids::new_id(), "none")).unwrap();
        assert!(matches!(r, Overwritten::Restart));
    }
}
```

- [ ] **Step 3:** `cargo test -p flux-core lock::takeover` gives 7 passed. Then run `just check`.
- [ ] **Step 4: non-vacuity (do not commit; report each):**
  - Drop step 6's re-check: `overwrite` returns `Held` without the `at_path` comparison.
    `an_overwrite_whose_file_left_the_path_restarts` must FAIL.
  - Continue on foreign content: make the s5 arm `Decoded::Uncertain(_) | Decoded::Foreign => {}`.
    `a_record_or_foreign_content_written_meanwhile_restarts` must FAIL at "decision 8".
- [ ] **Step 5:** commit `takeover.rs` and `mod.rs`: `feat(core): take an uncertain lock over in place (§240.5) (cut 7a
  Part 2)`.

---

### Task 7: `obtain` and the capability gate

**Files:** Create `crates/flux-core/src/lock/obtain.rs`; modify `mod.rs`.

- [ ] **Step 0:** `mod.rs` has `mod takeover;` (Task 6).
- [ ] **Step 1:** in `mod.rs`, add `mod obtain;` after `mod held;`, and `pub use obtain::{MAX_ATTEMPTS, Mode,
  Obtained, check_capability, obtain};` after `pub use held::...`.
- [ ] **Step 2:** create `crates/flux-core/src/lock/obtain.rs`:

```rust
//! The run's steps 1 and 3 ("The run"): the capability gate, then acquisition, classifying and recovering or taking
//! over as the model's `plain`, `rec` and `brk` processes do (`S21_1_decide`, `S21_1_refused`,
//! `S21_1_restart_decide`).

use super::acquire::{Acquire, Last, acquire};
use super::classify::{Classified, classify};
use super::error::{LockCode, LockError, LockResult, refuse};
use super::held::Held;
use super::recover::{Recovered, recover};
use super::site::LockSite;
use super::takeover::{Claimed, TakeOver, take_over};
use flux_fs::{DirHandle, LockCapability};
use std::ffi::OsString;

/// Passes through acquire-or-classify before giving up (decision 2).
pub const MAX_ATTEMPTS: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A run without `--break-lock`.
    Plain,
    /// `--restart --break-lock`: an uncertain lock is taken over in place (§240.5).
    BreakLock,
}

pub enum Obtained<'a, D: DirHandle> {
    /// A lock this run holds with no record yet: create the state, then `Held::write_record` (F5).
    Held { held: Held<'a, D>, leftover_broken: Option<OsString> },
    /// §240.5 steps 1-5 done: report the prior holder, create the state, then `Claimed::overwrite`.
    Claimed(Claimed<'a, D>),
}

/// The run's step 1: only the LOCAL allowlist proceeds; anything else, or a failure to tell, is `REMOTE_LOCK_UNSAFE`
/// with the cause (F1; plan decision 6 of Part 1).
pub fn check_capability<D: DirHandle>(dir: &D) -> LockResult<LockCapability> {
    match dir.lock_capability() {
        Ok(c) if c.allows_exclusive() => Ok(c),
        Ok(c) => Err(refuse(
            LockCode::RemoteLockUnsafe,
            None,
            format!("the destination filesystem is not supported for locking ({c:?})"),
        )),
        Err(e) => Err(refuse(
            LockCode::RemoteLockUnsafe,
            None,
            format!("the destination filesystem's lock capability could not be determined: {}", e.source),
        )),
    }
}

/// The run's step 3. `operation_id` names a recovery's move-aside file.
pub fn obtain<'a, D: DirHandle>(
    site: &LockSite<'a, D>,
    capability: LockCapability,
    mode: Mode,
    operation_id: &str,
) -> LockResult<Obtained<'a, D>> {
    let mut last = Last::Other;
    for _ in 0..MAX_ATTEMPTS {
        match acquire(site)? {
            Acquire::Acquired(held) => return Ok(Obtained::Held { held, leftover_broken: None }),
            Acquire::Restart(l) => {
                last = l;
                continue;
            }
            Acquire::Exists => {}
        }
        // S21_1_decide (a plain run) and S21_1_restart_decide (--restart --break-lock); S21_1_refused is each refusal.
        match classify(site)? {
            Classified::Vanished => last = Last::Other,
            Classified::Busy(holder) => {
                return Err(refuse(LockCode::TargetLockBusy, holder, "another run holds the lock; wait for it to finish"));
            }
            Classified::Foreign => {
                return Err(refuse(
                    LockCode::ControlPlaneNamespaceConflict,
                    None,
                    "a non-Flux object occupies the lock path; move it away",
                ));
            }
            Classified::Untrusted(record) => {
                return Err(refuse(
                    LockCode::ArtifactOwnershipUncertain,
                    Some(record),
                    "the dead owner's record names a workspace that is missing or not one Flux trusts",
                ));
            }
            Classified::Uncertain(why) => match mode {
                Mode::Plain => {
                    return Err(refuse(
                        LockCode::TargetLockUncertain,
                        None,
                        format!(
                            "the lock's record is unreadable ({why:?}); if no Flux run is active on this destination, run again with --restart --break-lock"
                        ),
                    ));
                }
                Mode::BreakLock => match take_over(site, capability, why)? {
                    TakeOver::Claimed(c) => return Ok(Obtained::Claimed(c)),
                    TakeOver::Restart(_) => last = Last::EmptyOrTornUnheld,
                },
            },
            Classified::Dead { lock, identity, record } => {
                match recover(site, lock, identity, &record, operation_id)? {
                    Recovered::Held { held, leftover } => {
                        return Ok(Obtained::Held { held, leftover_broken: leftover });
                    }
                    Recovered::Restart(l) => last = l,
                }
            }
        }
    }
    Err(give_up(last))
}

/// Decision 2 (fork B): the refusal reports what the last pass saw. Exit 3 either way.
fn give_up(last: Last) -> LockError {
    match last {
        Last::EmptyOrTornUnheld => refuse(
            LockCode::TargetLockUncertain,
            None,
            format!(
                "gave up after {MAX_ATTEMPTS} attempts on an empty or torn lock that nobody holds; run again with --restart --break-lock"
            ),
        ),
        Last::HeldByOther | Last::Other => refuse(
            LockCode::TargetLockBusy,
            None,
            format!("gave up after {MAX_ATTEMPTS} attempts; another run keeps holding or changing the lock"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FakeDirHandle;
    use crate::lock::test_support::{dead_lock, fake, live_lock, record, refusal};
    use flux_fs::{Code, FileSystem, LockFile};
    use std::ffi::OsStr;
    use std::io::ErrorKind;
    use std::path::Path;

    const NAME: &str = "dest.flux-lock";
    const STRONG: LockCapability = LockCapability::LocalStrong;

    fn site(d: &FakeDirHandle) -> LockSite<'_, FakeDirHandle> {
        LockSite::directory(d, OsStr::new("dest")).unwrap()
    }

    fn me() -> String {
        crate::ids::new_id()
    }

    #[test]
    fn the_capability_gate_passes_only_the_allowlist() {
        let (fs, d) = fake();
        assert_eq!(check_capability(&d).unwrap(), LockCapability::LocalStrong);
        fs.set_lock_capability(LockCapability::Unsupported);
        assert_eq!(refusal(check_capability(&d)).code, LockCode::RemoteLockUnsafe);
        let (fs, d) = fake();
        fs.fail("lock_capability", Code::IoError);
        let r = refusal(check_capability(&d));
        assert_eq!(r.code, LockCode::RemoteLockUnsafe);
        assert!(r.detail.contains("could not be determined"), "the cause is reported: {}", r.detail);
    }

    #[test]
    fn a_free_target_is_acquired_with_no_record() {
        let (_fs, d) = fake();
        let Obtained::Held { held, leftover_broken } = obtain(&site(&d), STRONG, Mode::Plain, &me()).unwrap() else {
            panic!("expected Held")
        };
        assert!(held.record().is_none());
        assert_eq!(leftover_broken, None);
    }

    #[test]
    fn a_live_owner_is_busy_and_reported() {
        let (_fs, d) = fake();
        let site = site(&d);
        let rec = record(&site, &me(), "none");
        let _live = live_lock(&d, NAME, &rec.encode());
        let r = refusal(obtain(&site, STRONG, Mode::Plain, &me()));
        assert_eq!(r.code, LockCode::TargetLockBusy);
        assert_eq!(r.holder, Some(rec));
    }

    #[test]
    fn an_uncertain_lock_refuses_a_plain_run_and_is_claimed_by_break_lock() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"");
        let site = site(&d);
        assert_eq!(refusal(obtain(&site, STRONG, Mode::Plain, &me())).code, LockCode::TargetLockUncertain);
        assert!(matches!(obtain(&site, STRONG, Mode::BreakLock, &me()).unwrap(), Obtained::Claimed(_)));
    }

    #[test]
    fn a_foreign_lock_is_a_namespace_conflict_in_every_mode() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"hello, not a lock");
        let site = site(&d);
        for mode in [Mode::Plain, Mode::BreakLock] {
            assert_eq!(refusal(obtain(&site, STRONG, mode, &me())).code, LockCode::ControlPlaneNamespaceConflict);
        }
    }

    #[test]
    fn a_dead_owner_is_recovered_in_every_mode() {
        for mode in [Mode::Plain, Mode::BreakLock] {
            let (fs, d) = fake();
            let site = site(&d);
            let id = me();
            for dir in ["/p/dest", "/p/dest/.flux", "/p/dest/.flux/operations", &format!("/p/dest/.flux/operations/{id}")] {
                fs.create_dir(Path::new(dir)).unwrap();
            }
            dead_lock(&d, NAME, &record(&site, &id, &format!("operations/{id}")).encode());
            let old = d.metadata(OsStr::new(NAME)).unwrap().identity;
            let Obtained::Held { held, .. } = obtain(&site, STRONG, mode, &me()).unwrap() else {
                panic!("expected Held for {mode:?}")
            };
            assert_ne!(held.identity(), &old, "{mode:?}: a new lock replaced the dead owner's");
        }
    }

    #[test]
    fn a_dead_owner_with_a_missing_workspace_is_artifact_ownership_uncertain() {
        let (_fs, d) = fake();
        let site = site(&d);
        let id = me();
        let rec = record(&site, &id, &format!("operations/{id}"));
        dead_lock(&d, NAME, &rec.encode());
        let r = refusal(obtain(&site, STRONG, Mode::BreakLock, &me()));
        assert_eq!(r.code, LockCode::ArtifactOwnershipUncertain);
        assert_eq!(r.holder, Some(rec));
    }

    #[test]
    fn a_race_that_never_settles_gives_up_busy_after_the_budget() {
        let (fs, d) = fake();
        // Every create finds the name taken, and every classification finds it gone again.
        fs.fail_kind("create_lock", Code::IoError, ErrorKind::AlreadyExists);
        fs.fail_always("create_lock", Code::IoError);
        let r = refusal(obtain(&site(&d), STRONG, Mode::Plain, &me()));
        assert_eq!(r.code, LockCode::TargetLockBusy);
        let creates = fs.calls().iter().filter(|c| c.starts_with("create_lock(")).count();
        assert_eq!(creates, MAX_ATTEMPTS as usize);
    }

    #[test]
    fn giving_up_reports_what_the_last_pass_saw() {
        let code = |l| match give_up(l) {
            LockError::Refused(r) => r.code,
            LockError::Io(e) => panic!("{e:?}"),
        };
        assert_eq!(code(Last::EmptyOrTornUnheld), LockCode::TargetLockUncertain);
        assert_eq!(code(Last::HeldByOther), LockCode::TargetLockBusy);
        assert_eq!(code(Last::Other), LockCode::TargetLockBusy);
    }

    #[test]
    fn a_claimed_lock_is_still_held_against_a_plain_run() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"");
        let site = site(&d);
        let claim = obtain(&site, STRONG, Mode::BreakLock, &me()).unwrap();
        assert!(matches!(claim, Obtained::Claimed(_)));
        assert_eq!(refusal(obtain(&site, STRONG, Mode::Plain, &me())).code, LockCode::TargetLockBusy);
        drop(claim);
        assert!(d.open_lock(OsStr::new(NAME)).unwrap().try_lock().unwrap());
    }
}
```

- [ ] **Step 3:** `cargo test -p flux-core lock::obtain` gives 10 passed. Then run `just check`.
- [ ] **Step 4: non-vacuity (do not commit; report each):**
  - Make `give_up` always return `TARGET_LOCK_BUSY`: `giving_up_reports_what_the_last_pass_saw` must FAIL.
  - Take a dead owner over instead of recovering it in break-lock mode (route `Classified::Dead` to a refusal in
    `Mode::BreakLock`): `a_dead_owner_is_recovered_in_every_mode` must FAIL at "BreakLock".
- [ ] **Step 5:** commit `obtain.rs` and `mod.rs`: `feat(core): obtain a destination lock through the checked protocol
  (cut 7a Part 2)`.

---

### Task 8: the protocol across real processes

**Files:** Create `crates/flux-core/tests/lock_protocol.rs`.

- [ ] **Step 0:** Tasks 2-7 are committed. `flux_core::lock` exports `LockSite`, `obtain`, `check_capability`,
  `Mode`, `Obtained`, `LockCode` and `LockError`. `flux_core::lock::record::LockRecord` and `flux_core::ids::new_id`
  are public.
- [ ] **Step 1:** create `crates/flux-core/tests/lock_protocol.rs`:

```rust
//! The lock protocol on a real filesystem, with a real OS-native lock held by ANOTHER PROCESS: while it lives, the
//! target is BUSY and its holder is reported; once it dies, the next run recovers its lock (§240.2, §240.3).

use flux_core::lock::record::LockRecord;
use flux_core::lock::{LockCode, LockError, LockSite, Mode, Obtained, check_capability, obtain};
use flux_fs::DestinationRoot;
use flux_platform::StdFileSystem;
use std::ffi::OsStr;
use std::io::BufRead;
use std::path::Path;

const CHILD_MARKER: &str = "FLUX_CHILD_HOLDS_TARGET_LOCK";

/// Run as a child: obtain the lock on `<dir>/dest`, create its workspace, write the record, report the operation id,
/// and hold the lock until killed.
fn hold_as_child(dir: &str) {
    let parent = StdFileSystem.destination_root(Path::new(dir)).expect("child opens the directory");
    let site = LockSite::directory(&parent, OsStr::new("dest")).unwrap();
    let capability = check_capability(&parent).unwrap();
    let id = flux_core::ids::new_id();
    let Obtained::Held { mut held, .. } = obtain(&site, capability, Mode::Plain, &id).unwrap() else {
        panic!("child: expected a held lock")
    };
    std::fs::create_dir_all(Path::new(dir).join("dest").join(".flux").join("operations").join(&id)).unwrap();
    held.write_record(LockRecord {
        complete_lock_key: site.complete_lock_key().unwrap(),
        operation_id: id.clone(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: flux_platform::boot_session_id(),
        target_path_key: site.target_path_key(),
        workspace_path: format!("operations/{id}"),
        creation_wall_time: 1,
        last_heartbeat_wall_time: 1,
    })
    .unwrap();
    // A line of its own: libtest has already printed `test <name> ... ` without a newline.
    println!("\n{CHILD_MARKER} {id}");
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(120));
}

#[test]
fn a_live_owner_in_another_process_is_busy_and_its_death_lets_the_next_run_recover() {
    if let Ok(dir) = std::env::var("FLUX_LOCK_PROTOCOL_CHILD_DIR") {
        hold_as_child(&dir);
        return;
    }
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_live_owner_in_another_process_is_busy_and_its_death_lets_the_next_run_recover",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FLUX_LOCK_PROTOCOL_CHILD_DIR", tmp.path())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Declared after `tmp`, so it drops first: the child is gone before the directory is removed.
    let mut child = KillOnDrop(child);
    let stdout = child.0.stdout.take().unwrap();
    let child_id = std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .find_map(|l| l.trim().strip_prefix(&format!("{CHILD_MARKER} ")).map(str::to_string))
        .expect("the child reports that it holds the lock");

    let parent = StdFileSystem.destination_root(tmp.path()).unwrap();
    let site = LockSite::directory(&parent, OsStr::new("dest")).unwrap();
    let capability = check_capability(&parent).unwrap();
    match obtain(&site, capability, Mode::Plain, &flux_core::ids::new_id()) {
        Err(LockError::Refused(r)) => {
            assert_eq!(r.code, LockCode::TargetLockBusy);
            assert_eq!(r.holder.map(|h| h.operation_id), Some(child_id), "the refusal reports the holder");
        }
        Err(LockError::Io(e)) => panic!("{e:?}"),
        Ok(_) => panic!("a live owner in another process must make the target busy"),
    }

    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let held = loop {
        match obtain(&site, capability, Mode::Plain, &flux_core::ids::new_id()) {
            Ok(Obtained::Held { held, leftover_broken }) => {
                assert_eq!(leftover_broken, None);
                break held;
            }
            Ok(Obtained::Claimed(_)) => panic!("a plain run never takes over"),
            Err(LockError::Refused(r)) if r.code == LockCode::TargetLockBusy => {
                assert!(std::time::Instant::now() < deadline, "a dead process's lock was never released");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => panic!("{e:?}"),
        }
    };
    assert!(held.record().is_none(), "recovered: an empty held lock, awaiting this run's state (F5)");
    let broken = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".broken."))
        .count();
    assert_eq!(broken, 0, "the moved-aside lock was deleted");
}
```

- [ ] **Step 2:** `cargo test -p flux-core --test lock_protocol` gives 1 passed on Windows. Then run `just check`,
  `just check-linux` (confirm this test is PASS in its output) and `just check-mac`.
- [ ] **Step 3: non-vacuity (do not commit; report):** make `classify` treat a held record as dead: change `Decoded::Record(record) if !got => Ok(Classified::Busy(Some(record))),` so it never matches (`if false`). This test must FAIL at "must make the target busy", or at the recovery, because recovery cannot move a lock another process holds. Report which, then revert.
- [ ] **Step 4:** afterwards, `tasklist | findstr lock_protocol` (Windows) prints nothing. Commit the test file:
  `test(core): the lock protocol against a real OS lock held by another process (cut 7a Part 2)`.

---

### Task 9: model conformance

**Files:** Create `models/lockproto/impl-map.toml` and `tests/model_impl_map.rs`.

- [ ] **Step 0:** `algorithm.txt` has exactly these 60 protocol labels (the grep in "What this plan rests on"):
  `S240_1_open S240_1_trylock S240_1_read S240_1_close S96_1_create S96_1_dircheck S96_1_ownlock S96_1_ownlock_verify
  S96_1_record_begin S96_1_record_end S96_1_backoff S96_1_ownlock_wait S96_1_ownlock_close S240_3_s1 S240_3_s2
  S240_3_s3 S240_3_s4 S240_3_s4_lock S240_3_s4_lock_verify S240_3_s4_record_begin S240_3_s4_record_end S240_3_s5
  S240_3_s4_drop S240_3_s4_lock_wait S240_3_s4_lock_close S240_3_putback S240_3_restart S240_3_release S240_5_s1
  S240_5_s2 S240_5_s3 S240_5_s4 S240_5_s5 S240_5_s6_seed S240_5_s6_write_begin S240_5_s6_write_end S240_5_s6_flush
  S240_5_s6 S240_5_seed_write_begin S240_5_seed_write_end S240_5_seed_rename S240_5_close S99_check S99_write
  S240_5_inflight_lands S99_release_check S99_release S99_release_lands S99_close S99_refuse_close S21_1_decide
  S21_1_refused S251_1_classify S251_1_delete S251_1_close S21_1_restart_decide S21_1_s3 S21_1_s5_write_begin
  S21_1_s5_write_end S21_1_s3_refuse_close`.
- [ ] **Step 1:** create `models/lockproto/impl-map.toml`:

```toml
# Every protocol label of algorithm.txt, and the Rust function that implements it, or why none does (cut 7a design,
# "Model conformance"). `item` is "<file relative to the repository>::<function>"; the file must define that function
# AND mention the label. `status` is one of: part-3 (cut 7a Part 3 implements it), seed-only (a seeded defect, never
# implemented), model-only (an event of the model, not a step of the code), not-in-7a (a later cut). Checked by
# tests/model_impl_map.rs.

[S240_1_open]
item = "crates/flux-core/src/lock/classify.rs::classify"
[S240_1_trylock]
item = "crates/flux-core/src/lock/classify.rs::classify"
[S240_1_read]
item = "crates/flux-core/src/lock/classify.rs::classify"
[S240_1_close]
item = "crates/flux-core/src/lock/classify.rs::classify"

[S96_1_create]
item = "crates/flux-core/src/lock/acquire.rs::acquire"
[S96_1_dircheck]
item = "crates/flux-core/src/lock/acquire.rs::acquire"
[S96_1_backoff]
item = "crates/flux-core/src/lock/acquire.rs::acquire"
[S96_1_ownlock]
item = "crates/flux-core/src/lock/acquire.rs::own_lock"
[S96_1_ownlock_wait]
item = "crates/flux-core/src/lock/acquire.rs::own_lock"
[S96_1_ownlock_verify]
item = "crates/flux-core/src/lock/acquire.rs::own_lock"
[S96_1_ownlock_close]
item = "crates/flux-core/src/lock/acquire.rs::own_lock"
[S96_1_record_begin]
item = "crates/flux-core/src/lock/held.rs::write_record"
[S96_1_record_end]
item = "crates/flux-core/src/lock/held.rs::write_record"

[S240_3_s1]
item = "crates/flux-core/src/lock/recover.rs::recover"
[S240_3_s2]
item = "crates/flux-core/src/lock/recover.rs::recover"
[S240_3_s3]
item = "crates/flux-core/src/lock/recover.rs::recover"
[S240_3_s4]
item = "crates/flux-core/src/lock/recover.rs::recover"
[S240_3_s4_lock]
item = "crates/flux-core/src/lock/acquire.rs::own_lock"
[S240_3_s4_lock_wait]
item = "crates/flux-core/src/lock/acquire.rs::own_lock"
[S240_3_s4_lock_verify]
item = "crates/flux-core/src/lock/acquire.rs::own_lock"
[S240_3_s4_lock_close]
item = "crates/flux-core/src/lock/acquire.rs::own_lock"
[S240_3_s4_record_begin]
item = "crates/flux-core/src/lock/held.rs::write_record"
[S240_3_s4_record_end]
item = "crates/flux-core/src/lock/held.rs::write_record"
[S240_3_s5]
item = "crates/flux-core/src/lock/recover.rs::recover"
[S240_3_s4_drop]
item = "crates/flux-core/src/lock/recover.rs::recover"
[S240_3_putback]
item = "crates/flux-core/src/lock/recover.rs::recover"
[S240_3_restart]
item = "crates/flux-core/src/lock/recover.rs::recover"
[S240_3_release]
item = "crates/flux-core/src/lock/recover.rs::recover"

[S240_5_s1]
item = "crates/flux-core/src/lock/takeover.rs::take_over"
[S240_5_s2]
item = "crates/flux-core/src/lock/takeover.rs::take_over"
[S240_5_s3]
item = "crates/flux-core/src/lock/takeover.rs::take_over"
[S240_5_s4]
item = "crates/flux-core/src/lock/takeover.rs::take_over"
[S240_5_s5]
item = "crates/flux-core/src/lock/takeover.rs::take_over"
[S240_5_close]
item = "crates/flux-core/src/lock/takeover.rs::take_over"
[S240_5_s6_write_begin]
item = "crates/flux-core/src/lock/takeover.rs::overwrite"
[S240_5_s6_write_end]
item = "crates/flux-core/src/lock/takeover.rs::overwrite"
[S240_5_s6_flush]
item = "crates/flux-core/src/lock/takeover.rs::overwrite"
[S240_5_s6]
item = "crates/flux-core/src/lock/takeover.rs::overwrite"
[S240_5_s6_seed]
status = "seed-only"
reason = "SEED_RENAME_OVER_TAKEOVER: a takeover by rename-over, a defect the model seeds; 7a overwrites in place"
[S240_5_seed_write_begin]
status = "seed-only"
reason = "SEED_RENAME_OVER_TAKEOVER's private record write"
[S240_5_seed_write_end]
status = "seed-only"
reason = "SEED_RENAME_OVER_TAKEOVER's private record write"
[S240_5_seed_rename]
status = "seed-only"
reason = "SEED_RENAME_OVER_TAKEOVER's rename over the lock"

[S99_check]
item = "crates/flux-core/src/lock/held.rs::still_owned"
[S99_write]
status = "part-3"
reason = "the engine's mutation after its still_owned check (copy_file and copy_tree hooks)"
[S240_5_inflight_lands]
status = "model-only"
reason = "an issued write completing after a takeover: an event the model checks, not a step the code takes"
[S99_release_check]
item = "crates/flux-core/src/lock/held.rs::release"
[S99_release]
item = "crates/flux-core/src/lock/held.rs::release"
[S99_release_lands]
item = "crates/flux-core/src/lock/held.rs::release"
[S99_close]
item = "crates/flux-core/src/lock/held.rs::release"
[S99_refuse_close]
item = "crates/flux-core/src/lock/held.rs::release"

[S21_1_decide]
item = "crates/flux-core/src/lock/obtain.rs::obtain"
[S21_1_refused]
item = "crates/flux-core/src/lock/obtain.rs::obtain"
[S21_1_restart_decide]
item = "crates/flux-core/src/lock/obtain.rs::obtain"
[S21_1_s3]
status = "part-3"
reason = "--restart's revalidation before rewriting the record"
[S21_1_s5_write_begin]
status = "part-3"
reason = "--restart's record write for the new operation"
[S21_1_s5_write_end]
status = "part-3"
reason = "--restart's record write for the new operation"
[S21_1_s3_refuse_close]
status = "part-3"
reason = "--restart's refusal close"

[S251_1_classify]
status = "not-in-7a"
reason = "flux cleanup (a later cut)"
[S251_1_delete]
status = "not-in-7a"
reason = "flux cleanup (a later cut)"
[S251_1_close]
status = "not-in-7a"
reason = "flux cleanup (a later cut)"
```

- [ ] **Step 2:** create `tests/model_impl_map.rs`:

```rust
//! Model conformance (cut 7a design, "Model conformance"): every protocol label of the lock model maps to the Rust
//! function that implements it - a function that exists and mentions the label - or says why none does; and no code
//! mentions a label marked for a later cut.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const FAMILIES: [&str; 7] = ["S96_1_", "S240_1_", "S240_3_", "S240_5_", "S99_", "S21_1_", "S251_1_"];
const STATUSES: [&str; 4] = ["part-3", "seed-only", "model-only", "not-in-7a"];

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Every `<label>:` line of the PlusCal algorithm in a protocol family.
fn model_labels() -> BTreeSet<String> {
    read("models/lockproto/algorithm.txt")
        .lines()
        .filter_map(|line| {
            let label = line.trim().strip_suffix(':')?;
            let ok = FAMILIES.iter().any(|f| label.starts_with(f))
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            ok.then(|| label.to_string())
        })
        .collect()
}

fn entries() -> toml::Table {
    toml::from_str(&read("models/lockproto/impl-map.toml")).expect("impl-map.toml parses")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn every_protocol_label_has_exactly_one_entry() {
    let labels = model_labels();
    assert!(labels.len() >= 50, "found only {} labels: the label parse no longer matches algorithm.txt", labels.len());
    let keys: BTreeSet<String> = entries().keys().cloned().collect();
    let missing: Vec<_> = labels.difference(&keys).collect();
    let extra: Vec<_> = keys.difference(&labels).collect();
    assert!(missing.is_empty() && extra.is_empty(), "no entry for {missing:?}; no such label for {extra:?}");
}

#[test]
fn every_entry_names_a_function_that_mentions_its_label_or_a_reason() {
    for (label, value) in entries() {
        let t = value.as_table().unwrap_or_else(|| panic!("{label}: not a table"));
        let item = t.get("item").and_then(|v| v.as_str());
        let status = t.get("status").and_then(|v| v.as_str());
        match (item, status) {
            (Some(item), None) => {
                let (file, func) = item.split_once("::").unwrap_or_else(|| panic!("{label}: item {item} has no ::"));
                let src = read(file);
                assert!(src.contains(&format!("fn {func}")), "{label}: {file} defines no fn {func}");
                assert!(src.contains(label.as_str()), "{label}: {file} never mentions the label");
            }
            (None, Some(status)) => {
                assert!(STATUSES.contains(&status), "{label}: unknown status {status}");
                let reason = t.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                assert!(!reason.is_empty(), "{label}: a status needs a reason");
            }
            _ => panic!("{label}: exactly one of `item` or `status`"),
        }
    }
}

#[test]
fn no_code_mentions_a_label_marked_not_in_7a() {
    let later: Vec<String> = entries()
        .into_iter()
        .filter(|(_, v)| v.get("status").and_then(|s| s.as_str()) == Some("not-in-7a"))
        .map(|(k, _)| k)
        .collect();
    assert!(!later.is_empty(), "the check needs at least one not-in-7a label to mean anything");
    let mut files = Vec::new();
    rust_files(&repo().join("crates"), &mut files);
    for file in files {
        let src = std::fs::read_to_string(&file).unwrap();
        for label in &later {
            assert!(!src.contains(label.as_str()), "{} mentions {label}, which is not in 7a", file.display());
        }
    }
}
```

- [ ] **Step 3:** `cargo test --test model_impl_map` gives 3 passed. Then run `just check`.
- [ ] **Step 4: non-vacuity (do not commit; report each):**
  - Delete the `[S99_close]` entry: `every_protocol_label_has_exactly_one_entry` must FAIL.
  - Change `S96_1_backoff`'s item to `crates/flux-core/src/lock/classify.rs::classify`:
    `every_entry_names_a_function_that_mentions_its_label_or_a_reason` must FAIL, because that file never mentions
    the label.
  - Add the comment `// S251_1_delete` to `obtain.rs`: `no_code_mentions_a_label_marked_not_in_7a` must FAIL.
- [ ] **Step 5:** commit both files: `test: the lock model's labels map to the Rust protocol (cut 7a Part 2)`.

---

### Task 10: Part 2 verification

- [ ] **Step 1:** `just check`, `just check-linux` (exit 0, all passed) and `just check-mac` (exit 0) all pass.
  `git status --short` is clean.
- [ ] **Step 2 (driver):** push `spec/cut-7a`. CI's three test jobs must be green; the real-process test runs natively
  on macOS there. Then the capstone and the test audit, then the Part 3 plan.

---

## Self-review

**Spec coverage.** Part 2 owns these parts of the design spec:

| Spec item | Where |
|---|---|
| "The run" step 1: the capability gate | Task 7 |
| Step 3: acquire, the `.flux-dir.lock` check, `PATH_COMPONENT_INVALID` | Tasks 2 and 3 |
| "Classifying an existing lock": every row, and the holder report | Tasks 4 and 7 |
| §240.3 recovery | Task 5 |
| §240.5 steps 1-6 | Task 6 |
| `still_owned` and release (Finish steps 5-6) | Task 3 |
| "Model conformance" | Task 9 |
| "Testing" items 1-3 | Tasks 3-8 |

Deferred to Part 3:
- the state, §21.1, `--restart`;
- the finish path's state steps;
- the CLI;
- the engine hooks;
- noticing other `.broken.*` files;
- "Testing" item 4.

**Declared against the spec or the model:** decisions 3 (recovery's record after the state), 6 (a held torn lock is
BUSY), 7 (`discard` unlinks while holding), 8 (the takeover restarts on foreign content) and 9 (`still_owned` fails
closed on `Unavailable`).

**Type consistency:**

| Item | Visibility |
|---|---|
| `LockSite<'a, D>`, `SiteKind`, `Held<'a, D>`, `Released` | public |
| `Claimed<'a, D>`, `Overwritten<'a, D>`, `Obtained<'a, D>`, `Mode` | public |
| `LockCode`, `Refusal`, `LockError`, `LockResult` | public |
| `check_capability`, `obtain`, `MAX_ATTEMPTS` | public |
| `Workspace`, `Last`, `Acquire`, `Classified`, `Recovered`, `TakeOver` | `pub(crate)` |
| `acquire`, `own_lock`, `classify`, `recover`, `take_over`, `give_up`, `refuse`, `hex` | `pub(crate)` or private |
