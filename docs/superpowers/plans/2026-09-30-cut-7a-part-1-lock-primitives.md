# Cut 7a Part 1: lock-file primitives, capability, boot session, IDs and the record codec - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Flux everything the destination lock stands on, without the protocol itself:
- a lock file that is created or opened through a directory handle and never follows a link;
- a non-blocking OS-native lock on that file;
- the lock capability of the filesystem it lives on;
- the host's boot session ID;
- random operation and owner-instance IDs;
- the §259.6 lock record's encoder and decoder.

**Architecture:**
- `flux-fs` gains a `LockFile` trait, the §235.1 `LockCapability` enum, and three `DirHandle` methods (`create_lock`,
  `open_lock`, `lock_capability`) with an associated `Lock` type.
- `flux-platform` implements them: `flock` on Unix; on Windows, `LockFileEx` on ONE byte far past the record, so that a
  record stays readable while another process holds the lock.
- The test fake in `flux-core` models the lock per OBJECT, so a held lock follows its file across a rename.
- `flux-core` also gets `ids` and `lock::record`.

Nothing is wired into `flux copy` yet: the protocol is Part 2, and the state and CLI are Part 3.

**Tech Stack:** Rust 2024 edition, `rustix` (Unix), `windows-sys` 0.61 (Windows), `blake3`, and the new dependencies
`uuid` (random IDs) and `libc` (macOS only, for `sysctlbyname`).

**Spec:** `docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md`, owner-approved at `fe80cf3`. It
supplies:
- "Components / flux-platform";
- "Components / flux-fs";
- "Formats: Operation ID and owner instance ID";
- "Formats: Lock record";
- "Decoding".

**Parts 2 and 3 are written after this part lands** (plan discipline): they cite code this part creates.

---

## What this plan rests on (read and verified at `fe80cf3`)

- `DirHandle` (`crates/flux-fs/src/fs.rs:223-298`) has one associated type, `Writer: FileHandle`, where `FileHandle` is
  `Write` plus `sync_all` (`:102-104`). It carries a refusal contract (`:200-222`): never traverse a name-surrogate,
  refuse a non-component name, never substitute a name.
- There are exactly three implementors:
  - `StdDir` on Unix (`crates/flux-platform/src/dir_unix.rs:68`);
  - `StdDir` on Windows (`crates/flux-platform/src/dir_windows.rs:97`);
  - `FakeDirHandle` (`crates/flux-core/src/fault_fs.rs:793`).
- Unix `create_new` is `openat(WRONLY | CREATE | EXCL | CLOEXEC, 0o666)` (`dir_unix.rs:97-107`); `identity` uses
  `fstat` + `metadata_from_stat` (`:84-88`).
- Windows `create_new_at` is `NtCreateFile` relative to the parent handle, with `FILE_CREATE` and
  `FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT` (`dir_windows.rs:304-371`).
  - `nt_io_error` maps `STATUS_FILE_IS_A_DIRECTORY` to `AlreadyExists`, on the stated premise that only `create_new_at`
    passes `FILE_NON_DIRECTORY_FILE` (`:57-61`); `open_lock_at` below breaks that premise, so it maps that status itself.
  - `reparse_tag_of(&OwnedHandle) -> Result<Option<u32>>` (`:862`) and `is_name_surrogate` (`:82-84`) are what
    `open_dir` uses to refuse a surrogate after opening it (`:168-180`).
  - `identity_of_handle` is at `crates/flux-platform/src/std_fs.rs:749`.
- The fake is `#[cfg(test)]` only (`crates/flux-core/src/lib.rs:9-11`):
  - it keeps `files`, `identities` (per path, minted by `mint_identity`, `fault_fs.rs:144-152`) and `types`;
  - `move_object` carries identity across a rename (`:180-190`);
  - `record(call, key)` logs a call and applies injected faults (`:424-444`);
  - `FakeDirHandle` delegates to `FaultFs` by full path (`:772-921`).
- The OS-lock probes (`crates/flux-platform/tests/fs_semantics.rs:10-11, 46-53, 120-135`) use `flock` on Unix and
  `LockFileEx` over the full 64-bit range on Windows.
- **The model's classifier reads the record whether or not its try-lock succeeded** (`models/lockproto/algorithm.txt`,
  `S240_1_trylock` then `S240_1_read`, 129-172): a held lock must not make the record unreadable. On Windows a byte
  range locked with `LockFileEx` cannot be read by another handle, so the Windows arm locks one byte at offset `1 << 62`
  (far past the 4096-byte record; Windows allows a lock beyond end of file), never the record's bytes. Task 3 measures
  this across two processes.
- The workspace (`Cargo.toml:56-73`):
  - `blake3 = "1.8"` is already listed;
  - `rustix` has feature `fs`;
  - `windows-sys` 0.61 has `Win32_Foundation`, `Win32_Storage_FileSystem`, `Win32_System_IO`, `Win32_Security`,
    `Wdk_Foundation` and `Wdk_Storage_FileSystem`;
  - `uuid` and `libc` are not listed.
- `FsError::new(code, io::Error)` and `FsError::from_io` are at `crates/flux-fs/src/error.rs:79-100`; `Code` has
  `SafetyRejected`, `DestinationError` and `IoError` (`:10-45`).

## Decisions this plan makes (declared)

1. **The Windows OS-native lock is one byte at offset `1 << 62`**, not the full range the probe used. The reason is
   above; Task 3 proves that another process reads the record while the lock is held.
2. **The Windows boot session is `BootTime` from `NtQuerySystemInformation(SystemTimeOfDayInformation)`**, a value fixed
   at boot, rather than the spec's "current time minus `GetTickCount64`, rounded to the second". The latter can differ
   by a second between two processes, and a boot ID must compare equal within one boot. On macOS it is `kern.boottime`
   through `libc::sysctlbyname`; on Linux, `/proc/sys/kernel/random/boot_id`.
3. **`open_lock` refuses** a name-surrogate (`SafetyRejected`), a directory (`DestinationError`, kind `IsADirectory`)
   and, on Unix, any non-regular file (`DestinationError`), and a missing name is `IoError` with kind `NotFound`, on
   every implementation.
4. **The fake keys a held lock by the file's object ID**, so a rename carries the lock, as §240.3's move-aside needs. It
   does not model reading an unlinked open file: such a read is `NotFound`. Part 2 extends the fake if a protocol test
   needs more.

## Ground rules

- Worktree `E:\Rust\flux-engine`, branch `spec/cut-7a`. Do not push.
- **Step 0 of every task:** confirm each quoted current text before editing; if it differs, STOP and report
  `STATE_MISMATCH: <file>: <what differs>`.
- **Shape-divergence stop:** a changed type, method name, signature, error code or `ErrorKind`, file name, or byte
  layout: STOP and report `[plan] -> [yours] because <reason>`. Exception: adapting a `windows-sys` or `rustix` call to
  the exact signature the crate version exposes (a pointer cast, a type alias), when the behaviour is identical. Report
  it; do not stop.
- **Oracle:** the tests in each task are already written - implement until they pass. Never edit a test to pass.
- **Gates:**
  - after each task: `just check` (the Windows host; the final line reports all tests passed);
  - after any task that touches Unix code: `just check-linux` (it ends `GATE: linux OK`);
  - after any task that touches macOS code: `just check-mac` (cross-clippy, exit 0).
- **Commits:** named paths only; each commit message ends with a `Co-Authored-By:` line for the model you are.

## File map

| File | Change |
|---|---|
| `Cargo.toml` | workspace deps `uuid` (feature `v4`) and `libc`; the `windows-sys` feature `Wdk_System_SystemInformation` |
| `crates/flux-fs/src/lock.rs` (new), `crates/flux-fs/src/lib.rs` | `LockCapability`, `LockFile` |
| `crates/flux-fs/src/fs.rs` | `DirHandle::Lock`, `create_lock`, `open_lock`, `lock_capability` |
| `crates/flux-platform/src/lock_file.rs` (new), `lib.rs`, `dir_unix.rs`, `dir_windows.rs`, `Cargo.toml` | `StdLock`, the trait methods, capability, `boot_session_id` |
| `crates/flux-platform/tests/lock_file.rs` (new) | real-filesystem and cross-process tests |
| `crates/flux-core/src/fault_fs.rs` | `FakeLock` and the fake's trait methods |
| `crates/flux-core/Cargo.toml`, `src/lib.rs`, `src/ids.rs` (new), `src/lock/mod.rs` (new), `src/lock/record.rs` (new) | IDs and the record codec |

---

### Task 1: `LockCapability` and the `LockFile` trait (`flux-fs`)

**Files:** Create `crates/flux-fs/src/lock.rs`; modify `crates/flux-fs/src/lib.rs`.

- [ ] **Step 0:** `crates/flux-fs/src/lib.rs` has `pub mod fs;` followed by `pub mod name;`, and the
  `pub use fs::{ DestinationRoot, ... };` block.
- [ ] **Step 1: write `crates/flux-fs/src/lock.rs`, including its test:**

```rust
//! The destination lock's file (spec §96.1, §235, §259.6).
//!
//! Only the primitives: the protocol that uses them lives in `flux-core`.

use crate::{FileIdentity, Result};

/// What the filesystem that holds a lock file can promise (§235.1).
///
/// Only the two strong classes allow an operation that needs target exclusivity; the other two are refused with
/// `REMOTE_LOCK_UNSAFE` (§235.4: prefer refusing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockCapability {
    LocalStrong,
    RemoteStrong,
    RemoteUnverified,
    Unsupported,
}

impl LockCapability {
    /// §235.1's table: whether an operation needing target exclusivity may proceed.
    pub fn allows_exclusive(self) -> bool {
        matches!(self, Self::LocalStrong | Self::RemoteStrong)
    }
}

/// A lock file held open for reading and writing (§96.1, §259.6).
///
/// Dropping the handle closes it, and closing it releases the OS-native lock, as does the death of the process that
/// holds it. That is what makes a dead owner's lock recoverable (§240.2, §240.3).
pub trait LockFile {
    /// Take the OS-native lock without waiting. `Ok(true)` means it was granted to THIS handle (again, if it already
    /// held it); `Ok(false)` means another handle holds it. Any other failure is an error, never `false`.
    fn try_lock(&self) -> Result<bool>;

    /// The file's bytes from offset 0, reading at most `limit + 1` bytes, so that a caller can tell a file longer than
    /// `limit` from one of exactly `limit`. Readable while another handle holds the OS-native lock: the model's
    /// classifier reads the record whether or not its try-lock succeeded (§240.1).
    fn read_all(&self, limit: usize) -> Result<Vec<u8>>;

    /// Write `bytes` at offset 0 in ONE write call (§259.6: "written in one write call"). A short write is an error.
    fn write_at_start(&self, bytes: &[u8]) -> Result<()>;

    fn sync_all(&self) -> Result<()>;

    /// The identity of the object this handle holds - not of a path (§107, §240.3 step 3).
    fn identity(&self) -> Result<FileIdentity>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_strong_classes_allow_an_exclusive_operation() {
        assert!(LockCapability::LocalStrong.allows_exclusive());
        assert!(LockCapability::RemoteStrong.allows_exclusive());
        assert!(!LockCapability::RemoteUnverified.allows_exclusive());
        assert!(!LockCapability::Unsupported.allows_exclusive());
    }
}
```

- [ ] **Step 2:** in `crates/flux-fs/src/lib.rs`, after `pub mod name;` add `pub mod lock;`, and after
  `pub use name::check_component;` add `pub use lock::{LockCapability, LockFile};`.
- [ ] **Step 3:** `cargo test -p flux-fs lock` → `test lock::tests::only_the_strong_classes_allow_an_exclusive_operation ... ok`; then `just check`.
- [ ] **Step 4:** commit `crates/flux-fs/src/lock.rs`, `crates/flux-fs/src/lib.rs`:
  `feat(fs): LockCapability and the LockFile trait (cut 7a Part 1)`.

---

### Task 2: `DirHandle::create_lock` / `open_lock` and the three lock-file types

**Files:**
- modify `crates/flux-fs/src/fs.rs`;
- create `crates/flux-platform/src/lock_file.rs` and `crates/flux-platform/tests/lock_file.rs`;
- modify `crates/flux-platform/src/lib.rs`, `crates/flux-platform/src/dir_unix.rs`,
  `crates/flux-platform/src/dir_windows.rs` and `crates/flux-core/src/fault_fs.rs`.

- [ ] **Step 0:** verify:
  - `fs.rs:223-224` reads `pub trait DirHandle: Sized {` / `    type Writer: FileHandle;`;
  - `dir_unix.rs:68-69` and `dir_windows.rs:97-98` read `impl DirHandle for StdDir {` / `    type Writer = crate::StdFile;`;
  - `fault_fs.rs:793-794` reads `impl DirHandle for FakeDirHandle {` / `    type Writer = FakeHandle;`;
  - `dir_windows.rs` has `fn create_new_at` (`:304`), `fn reparse_tag_of` (`:862`) and `const fn is_name_surrogate`
    (`:82`).
- [ ] **Step 1: the platform tests (they fail to compile until Step 4).** Create
  `crates/flux-platform/tests/lock_file.rs`:

```rust
//! The lock file on a real filesystem (cut 7a Part 1): created and opened through a directory handle, locked without
//! waiting, and readable while another handle holds the lock.

use flux_fs::{Code, DestinationRoot, DirHandle, LockFile};
use flux_platform::StdFileSystem;
use std::ffi::OsStr;
use std::io::ErrorKind;

fn dir() -> (tempfile::TempDir, flux_platform::StdDir) {
    let tmp = tempfile::tempdir().expect("scratch directory");
    let d = StdFileSystem.destination_root(tmp.path()).expect("open the scratch directory");
    (tmp, d)
}

const NAME: &str = "dest.flux-lock";

#[test]
fn create_lock_is_exclusive() {
    let (_tmp, d) = dir();
    let _held = d.create_lock(OsStr::new(NAME)).expect("first create");
    let e = d.create_lock(OsStr::new(NAME)).expect_err("the name is taken");
    assert_eq!(e.source.kind(), ErrorKind::AlreadyExists);
}

#[test]
fn a_second_handle_cannot_take_a_held_lock_until_the_first_is_dropped() {
    let (_tmp, d) = dir();
    let first = d.create_lock(OsStr::new(NAME)).unwrap();
    assert!(first.try_lock().unwrap(), "an unheld lock is granted");
    assert!(first.try_lock().unwrap(), "re-taking a lock this handle holds is granted");
    let second = d.open_lock(OsStr::new(NAME)).unwrap();
    assert!(!second.try_lock().unwrap(), "held by the first handle");
    drop(first);
    assert!(second.try_lock().unwrap(), "closing the holder releases the lock");
}

#[test]
fn the_record_is_readable_while_another_handle_holds_the_lock() {
    // The model's classifier reads the record whether or not its try-lock succeeded (S240_1_read). On Windows a
    // LockFileEx range cannot be read by another handle, which is why the lock sits on one byte far past the record.
    let (_tmp, d) = dir();
    let holder = d.create_lock(OsStr::new(NAME)).unwrap();
    let record: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    holder.write_at_start(&record).unwrap();
    holder.sync_all().unwrap();
    assert!(holder.try_lock().unwrap());
    let reader = d.open_lock(OsStr::new(NAME)).unwrap();
    assert!(!reader.try_lock().unwrap());
    assert_eq!(reader.read_all(4096).unwrap(), record, "read through a second handle while the lock is held");
    assert_eq!(holder.read_all(4096).unwrap(), record, "and through the holder's own handle");
}

#[test]
fn read_all_reads_one_byte_past_its_limit_so_an_oversized_file_shows() {
    let (_tmp, d) = dir();
    let h = d.create_lock(OsStr::new(NAME)).unwrap();
    h.write_at_start(&[7u8; 5000]).unwrap();
    assert_eq!(h.read_all(4096).unwrap().len(), 4097);
    assert_eq!(h.read_all(10_000).unwrap().len(), 5000);
    let empty = d.create_lock(OsStr::new("empty.flux-lock")).unwrap();
    assert!(empty.read_all(4096).unwrap().is_empty());
}

#[test]
fn write_at_start_overwrites_in_place_and_keeps_the_object() {
    let (_tmp, d) = dir();
    let h = d.create_lock(OsStr::new(NAME)).unwrap();
    h.write_at_start(&[1u8; 4096]).unwrap();
    let before = h.identity().unwrap();
    h.write_at_start(&[2u8; 4096]).unwrap();
    assert_eq!(h.read_all(4096).unwrap(), vec![2u8; 4096]);
    assert_eq!(h.identity().unwrap(), before, "§240.5 step 6 overwrites the SAME object");
}

#[test]
fn a_lock_handle_reports_the_identity_the_directory_reports_for_its_name() {
    let (_tmp, d) = dir();
    let h = d.create_lock(OsStr::new(NAME)).unwrap();
    let by_name = d.metadata(OsStr::new(NAME)).unwrap().identity;
    assert!(matches!(by_name, flux_fs::FileIdentity::Strong(_)), "a local filesystem gives a strong identity");
    assert_eq!(h.identity().unwrap(), by_name);
}

#[test]
fn open_lock_refuses_a_missing_name_and_a_directory() {
    let (_tmp, d) = dir();
    let e = d.open_lock(OsStr::new("absent.flux-lock")).expect_err("nothing there");
    assert_eq!((e.code, e.source.kind()), (Code::IoError, ErrorKind::NotFound));
    let _sub = d.create_dir(OsStr::new("adir")).unwrap();
    let e = d.open_lock(OsStr::new("adir")).expect_err("a directory is not a lock file");
    assert_eq!((e.code, e.source.kind()), (Code::DestinationError, ErrorKind::IsADirectory));
}

#[cfg(unix)]
#[test]
fn open_lock_refuses_a_symlink_even_to_a_real_lock_file() {
    let (tmp, d) = dir();
    let _real = d.create_lock(OsStr::new(NAME)).unwrap();
    std::os::unix::fs::symlink(tmp.path().join(NAME), tmp.path().join("link.flux-lock")).unwrap();
    let e = d.open_lock(OsStr::new("link.flux-lock")).expect_err("never follow a link");
    assert_eq!(e.code, Code::SafetyRejected);
}

#[cfg(windows)]
#[test]
fn open_lock_refuses_a_junction() {
    let (tmp, d) = dir();
    let target = tmp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let link = tmp.path().join("junction.flux-lock");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J needs no privilege");
    let e = d.open_lock(OsStr::new("junction.flux-lock")).expect_err("never follow a link");
    // FILE_NON_DIRECTORY_FILE meets a junction (a directory reparse point) first: either refusal is a refusal,
    // and neither may open the target.
    assert!(matches!(e.code, Code::SafetyRejected | Code::DestinationError), "{e:?}");
}
```

- [ ] **Step 2: the trait.** In `crates/flux-fs/src/fs.rs`, replace `pub trait DirHandle: Sized {\n    type Writer: FileHandle;` with:

```rust
pub trait DirHandle: Sized {
    type Writer: FileHandle;

    /// A lock file held open for reading and writing (§96.1). See `crate::LockFile`.
    type Lock: crate::LockFile;
```

  and immediately before the closing `}` of the trait (after `rename_replace`'s declaration, `fs.rs:292-297`) add:

```rust

    /// Create a lock file exclusively, open for reading AND writing (§96.1: "created with exclusive creation").
    ///
    /// The same refusals as `create_new`: MUST FAIL with `ErrorKind::AlreadyExists` if the name is taken by anything,
    /// including a link, and never create through a link.
    fn create_lock(&self, name: &std::ffi::OsStr) -> Result<Self::Lock>;

    /// Open an EXISTING lock file for reading and writing, without creating it (§240.5 step 2; the classifier's open,
    /// §240.1). Never follows a link.
    ///
    /// Refuses: a name-surrogate (`Code::SafetyRejected`); a directory (`Code::DestinationError`, kind
    /// `IsADirectory`); any other non-regular file where the platform can tell (`Code::DestinationError`). A missing
    /// name is `Code::IoError` with kind `NotFound`.
    fn open_lock(&self, name: &std::ffi::OsStr) -> Result<Self::Lock>;
```

- [ ] **Step 3: `StdLock`.** Create `crates/flux-platform/src/lock_file.rs`:

```rust
//! The lock file on a real filesystem (cut 7a Part 1).
//!
//! The OS-native lock is `flock` on Unix and `LockFileEx` on Windows, never `fcntl` record locks, which a process
//! loses when it closes ANY descriptor of the file (`tests/fs_semantics.rs:10-11`).

use flux_fs::{Code, FileIdentity, FsError, LockFile, Result};
use std::fs::File;

/// A lock file open for reading and writing. Dropping it closes the handle, which releases the OS-native lock.
#[derive(Debug)]
pub struct StdLock {
    file: File,
    /// Whether THIS handle holds the lock. Windows range locks do not nest: a second `LockFileEx` by the holder fails
    /// exactly as a stranger's does, and unlocking to find out would open a window in which another process takes
    /// it. So the holder remembers. (Unix `flock` re-locks its own descriptor harmlessly; the flag is kept on both
    /// arms so they behave alike.)
    held: std::sync::atomic::AtomicBool,
}

impl StdLock {
    pub(crate) fn new(file: File) -> Self {
        Self { file, held: std::sync::atomic::AtomicBool::new(false) }
    }
}

/// The one byte the Windows lock covers: far past the 4096-byte record, so the record stays readable while the lock
/// is held (the model's classifier reads it either way). Windows allows a lock beyond end of file.
#[cfg(windows)]
const LOCK_BYTE: u64 = 1 << 62;

fn short_write() -> FsError {
    FsError::new(
        Code::IoError,
        std::io::Error::new(std::io::ErrorKind::WriteZero, "short write of a lock record"),
    )
}

impl LockFile for StdLock {
    fn try_lock(&self) -> Result<bool> {
        use std::sync::atomic::Ordering;
        if self.held.load(Ordering::SeqCst) {
            return Ok(true);
        }
        let granted = os_try_lock(&self.file)?;
        if granted {
            self.held.store(true, Ordering::SeqCst);
        }
        Ok(granted)
    }

    #[cfg(unix)]
    fn read_all(&self, limit: usize) -> Result<Vec<u8>> {
        use std::os::unix::fs::FileExt;
        read_loop(limit, |buf, at| self.file.read_at(buf, at))
    }

    #[cfg(windows)]
    fn read_all(&self, limit: usize) -> Result<Vec<u8>> {
        use std::os::windows::fs::FileExt;
        read_loop(limit, |buf, at| self.file.seek_read(buf, at))
    }

    #[cfg(unix)]
    fn write_at_start(&self, bytes: &[u8]) -> Result<()> {
        use std::os::unix::fs::FileExt;
        let n = self.file.write_at(bytes, 0).map_err(FsError::from_io)?;
        if n == bytes.len() { Ok(()) } else { Err(short_write()) }
    }

    #[cfg(windows)]
    fn write_at_start(&self, bytes: &[u8]) -> Result<()> {
        use std::os::windows::fs::FileExt;
        let n = self.file.seek_write(bytes, 0).map_err(FsError::from_io)?;
        if n == bytes.len() { Ok(()) } else { Err(short_write()) }
    }

    fn sync_all(&self) -> Result<()> {
        self.file.sync_all().map_err(FsError::from_io)
    }

    #[cfg(unix)]
    fn identity(&self) -> Result<FileIdentity> {
        let st = rustix::fs::fstat(&self.file).map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(crate::std_fs::metadata_from_stat(&st).identity)
    }

    #[cfg(windows)]
    fn identity(&self) -> Result<FileIdentity> {
        Ok(crate::std_fs::identity_of_handle(&self.file))
    }
}

/// One non-blocking attempt at the OS-native lock: `Ok(false)` only when another open file description holds it.
#[cfg(unix)]
fn os_try_lock(file: &File) -> Result<bool> {
    use rustix::fs::FlockOperation;
    match rustix::fs::flock(file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(true),
        Err(e) if e == rustix::io::Errno::WOULDBLOCK => Ok(false),
        Err(e) => Err(FsError::from_io(std::io::Error::from(e))),
    }
}

/// One non-blocking attempt at the OS-native lock: `Ok(false)` only when another handle holds `LOCK_BYTE`.
#[cfg(windows)]
fn os_try_lock(file: &File) -> Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx};
    use windows_sys::Win32::System::IO::OVERLAPPED;
    // SAFETY: an all-zero OVERLAPPED is valid, and only its offset fields are then set; the handle is open for the
    // whole call; the call is synchronous (FAIL_IMMEDIATELY on a synchronous handle), so `ov` outlives it.
    let ok = unsafe {
        let mut ov: OVERLAPPED = std::mem::zeroed();
        ov.Anonymous.Anonymous.Offset = LOCK_BYTE as u32;
        ov.Anonymous.Anonymous.OffsetHigh = (LOCK_BYTE >> 32) as u32;
        LockFileEx(file.as_raw_handle() as HANDLE, LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY, 0, 1, 0, &mut ov)
    };
    if ok != 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    if e.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        return Ok(false);
    }
    Err(FsError::from_io(e))
}

/// Read from offset 0 until end of file or `limit + 1` bytes, whichever comes first.
fn read_loop(limit: usize, mut read_at: impl FnMut(&mut [u8], u64) -> std::io::Result<usize>) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; limit + 1];
    let mut n = 0;
    while n < buf.len() {
        let got = read_at(&mut buf[n..], n as u64).map_err(FsError::from_io)?;
        if got == 0 {
            break;
        }
        n += got;
    }
    buf.truncate(n);
    Ok(buf)
}
```

  In `crates/flux-platform/src/lib.rs`, after `pub mod std_fs;` add `mod lock_file;`, and after the last `pub use` add
  `pub use lock_file::StdLock;`.

- [ ] **Step 4: the Unix methods.** In `crates/flux-platform/src/dir_unix.rs`:
  - after `    type Writer = crate::StdFile;` (`:69`) add `    type Lock = crate::StdLock;`;
  - before the closing `}` of `impl DirHandle for StdDir` (after `rename_replace`, `:177-192`) add:

```rust

    fn create_lock(&self, name: &OsStr) -> Result<Self::Lock> {
        check_component(name)?;
        let fd = openat(
            &self.0,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o666),
        )
        .map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(crate::StdLock::new(std::fs::File::from(fd)))
    }

    fn open_lock(&self, name: &OsStr) -> Result<Self::Lock> {
        use rustix::fs::FileType;
        use rustix::io::Errno;
        check_component(name)?;
        // NONBLOCK: an open of a FIFO planted at the lock's name must not hang (it is refused just below). It changes
        // nothing for a regular file, whose reads and writes never block on it.
        let fd = openat(&self.0, name, OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC, Mode::empty())
            .map_err(|e| match e {
                // O_NOFOLLOW on a symlink in the final component: the link itself is refused, never followed.
                Errno::LOOP => FsError::new(Code::SafetyRejected, std::io::Error::from(e)),
                Errno::ISDIR => FsError::new(
                    Code::DestinationError,
                    std::io::Error::new(std::io::ErrorKind::IsADirectory, "a directory is not a lock file"),
                ),
                _ => FsError::from_io(std::io::Error::from(e)),
            })?;
        let st = rustix::fs::fstat(&fd).map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
            return Err(FsError::new(
                Code::DestinationError,
                std::io::Error::other("a lock path holds something other than a regular file"),
            ));
        }
        Ok(crate::StdLock::new(std::fs::File::from(fd)))
    }
```

  (`FsError::from_io` of `ENOENT` gives `Code::IoError`, kind `NotFound`, as the test expects.)

- [ ] **Step 5: the Windows methods.** In `crates/flux-platform/src/dir_windows.rs`:
  - after `    type Writer = crate::StdFile;` (`:98`) add `    type Lock = crate::StdLock;`;
  - before the closing `}` of `impl DirHandle for StdDir` add:

```rust

    fn create_lock(&self, name: &OsStr) -> Result<Self::Lock> {
        check_component(name)?;
        open_lock_at(&self.0, name, FILE_CREATE)
    }

    fn open_lock(&self, name: &OsStr) -> Result<Self::Lock> {
        check_component(name)?;
        open_lock_at(&self.0, name, FILE_OPEN)
    }
```

  and after `fn create_new_at` (it ends at `:371`) add:

```rust

/// The lock file, created (`FILE_CREATE`) or opened (`FILE_OPEN`) relative to `p`, for reading AND writing.
///
/// `FILE_OPEN_REPARSE_POINT` for both, as `create_new_at` explains: a create through a dangling link must collide,
/// and an open must get the LINK, which is then refused below rather than followed. Share modes: read, write and
/// DELETE, because another process must be able to open the lock to classify it, and §240.3 renames a dead owner's
/// lock aside while it is open.
fn open_lock_at(p: &OwnedHandle, n: &OsStr, disposition: u32) -> Result<crate::StdLock> {
    use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ;
    let mut wide: Vec<u16> = n.encode_wide().collect();
    let bytes = (wide.len() * 2) as u16;
    let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
    let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
    oa.RootDirectory = p.as_raw_handle() as HANDLE;
    oa.ObjectName = &raw const us;
    let mut h: HANDLE = std::ptr::null_mut();
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is to a live local that outlives the call, and `wide` outlives `us`.
    let status = unsafe {
        NtCreateFile(
            &raw mut h,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | SYNCHRONIZE,
            &raw const oa,
            &raw mut iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            disposition,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status != 0 {
        // An OPEN that meets a directory is not a name collision: `nt_io_error` maps FILE_IS_A_DIRECTORY to
        // AlreadyExists for `create_new_at`'s sake, so answer it here first.
        if disposition == FILE_OPEN && status == STATUS_FILE_IS_A_DIRECTORY {
            return Err(FsError::new(
                Code::DestinationError,
                std::io::Error::new(std::io::ErrorKind::IsADirectory, "a directory is not a lock file"),
            ));
        }
        let code = match status {
            STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => Code::IoError,
            STATUS_ACCESS_DENIED => Code::PermissionDenied,
            _ => Code::IoError,
        };
        return Err(FsError::new(code, nt_io_error("NtCreateFile", status)));
    }
    // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `h` is a valid handle we own.
    let opened = unsafe { OwnedHandle::from_raw_handle(h as _) };
    // The open succeeded even on a surrogate; refuse it, as `open_dir` does. A created file cannot be one.
    if disposition == FILE_OPEN
        && let Some(tag) = reparse_tag_of(&opened)?
        && is_name_surrogate(tag)
    {
        return Err(FsError::new(
            Code::SafetyRejected,
            std::io::Error::other(format!("name-surrogate reparse point, tag 0x{tag:08X}")),
        ));
    }
    Ok(crate::StdLock::new(File::from(opened)))
}
```

  Also extend `nt_io_error`'s comment at `:57-61`: after "`create_new_at` is the one call in this file that passes
  FILE_NON_DIRECTORY_FILE." add "`open_lock_at` passes it too, and answers FILE_IS_A_DIRECTORY for an OPEN before
  calling this."

- [ ] **Step 6: the fake.** In `crates/flux-core/src/fault_fs.rs`:
  - to `use flux_fs::{ ... }` add `LockCapability, LockFile`;
  - to `struct Inner` (before its closing `}` at `:89`) add:

```rust
    /// Object index -> the `FakeLock` handle id holding its OS-native lock. Keyed by OBJECT, not path: a real
    /// OS-native lock follows the file across a rename, and §240.3 renames a lock aside while holding it.
    lock_holders: HashMap<u128, u64>,
    next_lock_handle: u64,
    /// What `lock_capability` answers; `None` means `LocalStrong`.
    lock_capability: Option<LockCapability>,
```

  and after `impl FileHandle for FakeHandle { ... }` (ends `:251`) add:

```rust

/// A lock file in the fake. It addresses its OBJECT (by identity), so a rename carries it.
pub struct FakeLock {
    object: u128,
    handle: u64,
    inner: std::sync::Arc<Mutex<Inner>>,
}

impl FakeLock {
    /// The path now holding this handle's object, or `NotFound` if none does (the fake does not model reading an
    /// unlinked open file).
    fn path(g: &Inner, object: u128) -> Result<PathBuf> {
        g.identities
            .iter()
            .find(|(_, id)| matches!(id, flux_fs::FileIdentity::Strong(o) if o.index == object))
            .map(|(p, _)| p.clone())
            .ok_or_else(|| FsError::new(Code::IoError, std::io::Error::from(std::io::ErrorKind::NotFound)))
    }

    fn record(&self, call: &str) -> Result<()> {
        let fs = FaultFs { inner: std::sync::Arc::clone(&self.inner) };
        let path = { Self::path(&self.inner.lock().unwrap(), self.object).unwrap_or_default() };
        fs.record(format!("{call}({})", path.display()), call)
    }
}

impl LockFile for FakeLock {
    fn try_lock(&self) -> Result<bool> {
        self.record("try_lock")?;
        let mut g = self.inner.lock().unwrap();
        match g.lock_holders.get(&self.object) {
            Some(&h) if h != self.handle => Ok(false),
            _ => {
                g.lock_holders.insert(self.object, self.handle);
                Ok(true)
            }
        }
    }

    fn read_all(&self, limit: usize) -> Result<Vec<u8>> {
        self.record("read_all")?;
        let g = self.inner.lock().unwrap();
        let path = Self::path(&g, self.object)?;
        let mut bytes = g.files.get(&path).cloned().unwrap_or_default();
        bytes.truncate(limit + 1);
        Ok(bytes)
    }

    fn write_at_start(&self, bytes: &[u8]) -> Result<()> {
        self.record("write_at_start")?;
        let mut g = self.inner.lock().unwrap();
        let path = Self::path(&g, self.object)?;
        let file = g.files.entry(path).or_default();
        if file.len() < bytes.len() {
            file.resize(bytes.len(), 0);
        }
        file[..bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    fn sync_all(&self) -> Result<()> {
        self.record("lock_sync_all")
    }

    fn identity(&self) -> Result<flux_fs::FileIdentity> {
        Ok(flux_fs::FileIdentity::Strong(flux_fs::ObjectId { volume: 1, index: self.object }))
    }
}

impl Drop for FakeLock {
    /// Closing the handle releases the OS-native lock, as on every real platform.
    fn drop(&mut self) {
        if let Ok(mut g) = self.inner.lock()
            && g.lock_holders.get(&self.object) == Some(&self.handle)
        {
            g.lock_holders.remove(&self.object);
        }
    }
}
```

  In `impl FaultFs` (after `pub fn called`, `:316-318`) add:

```rust

    /// What every `lock_capability` call answers from now on (default `LocalStrong`).
    pub fn set_lock_capability(&self, capability: LockCapability) {
        self.inner.lock().unwrap().lock_capability = Some(capability);
    }
```

  In `impl DirHandle for FakeDirHandle`, after `    type Writer = FakeHandle;` add `    type Lock = FakeLock;`,
  and before its closing `}` (after `rename_replace`, `:915-921`) add:

```rust

    fn create_lock(&self, name: &OsStr) -> Result<Self::Lock> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().record(format!("create_lock({})", child_path.display()), "create_lock")?;
        {
            // Not through `create_new`: that would count as a `create_new` call for nth-fault injection and consume
            // a pending `write_fault` meant for a data file. The refusal is the same: a file OR a directory holds the
            // name (the reason is recorded in `create_new`).
            let mut g = self.inner.lock().unwrap();
            if g.files.contains_key(&child_path) || g.directories.contains(&child_path) {
                return Err(FsError::new(Code::IoError, std::io::Error::from(std::io::ErrorKind::AlreadyExists)));
            }
            g.files.insert(child_path.clone(), Vec::new());
            mint_identity(&mut g, &child_path);
        }
        self.lock_for(&child_path)
    }

    fn open_lock(&self, name: &OsStr) -> Result<Self::Lock> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().record(format!("open_lock({})", child_path.display()), "open_lock")?;
        {
            let g = self.inner.lock().unwrap();
            match g.types.get(&child_path) {
                Some(FileType::Symlink) => {
                    return Err(FsError::new(
                        Code::SafetyRejected,
                        std::io::Error::other("refuses to follow a symlink or other name-surrogate"),
                    ));
                }
                Some(FileType::Other) => {
                    return Err(FsError::new(
                        Code::DestinationError,
                        std::io::Error::other("a lock path holds something other than a regular file"),
                    ));
                }
                _ => {}
            }
            if g.directories.contains(&child_path) {
                return Err(FsError::new(
                    Code::DestinationError,
                    std::io::Error::new(std::io::ErrorKind::IsADirectory, "a directory is not a lock file"),
                ));
            }
            if !g.files.contains_key(&child_path) {
                return Err(FsError::new(Code::IoError, std::io::Error::from(std::io::ErrorKind::NotFound)));
            }
        }
        self.lock_for(&child_path)
    }
```

  and in `impl FakeDirHandle` (after `fn my_path`, `:783-790`) add:

```rust

    /// A new `FakeLock` handle onto the object now at `path`.
    fn lock_for(&self, path: &Path) -> Result<FakeLock> {
        let mut g = self.inner.lock().unwrap();
        let object = match g.identities.get(path) {
            Some(flux_fs::FileIdentity::Strong(o)) => o.index,
            _ => panic!("no strong identity minted for {}: a creation path skipped mint_identity", path.display()),
        };
        g.next_lock_handle += 1;
        let handle = g.next_lock_handle;
        Ok(FakeLock { object, handle, inner: std::sync::Arc::clone(&self.inner) })
    }
```

  Add these tests at the end of the fake's `mod tests`. They reach the fake through a `FakeDirHandle` from
  `destination_root`, as the module's other `DirHandle` tests do (`fault_fs.rs:1342-1359`), and they need the same
  in-test imports those use. Put this line at the top of EACH of the four tests below (and of Task 4's fake test):
  `use flux_fs::{DestinationRoot, DirHandle, FileSystem, LockFile}; use std::ffi::OsStr;`

```rust
    #[test]
    fn a_fake_lock_is_exclusive_per_object_and_released_on_drop() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let first = d.create_lock(OsStr::new("x.flux-lock")).unwrap();
        assert_eq!(
            d.create_lock(OsStr::new("x.flux-lock")).unwrap_err().source.kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(first.try_lock().unwrap());
        let second = d.open_lock(OsStr::new("x.flux-lock")).unwrap();
        assert!(!second.try_lock().unwrap());
        drop(first);
        assert!(second.try_lock().unwrap());
    }

    #[test]
    fn a_fake_lock_follows_its_object_across_a_rename() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let holder = d.create_lock(OsStr::new("x.flux-lock")).unwrap();
        holder.write_at_start(b"record").unwrap();
        assert!(holder.try_lock().unwrap());
        d.rename_no_replace(OsStr::new("x.flux-lock"), &d, OsStr::new("x.flux-lock.broken.1")).unwrap();
        let moved = d.open_lock(OsStr::new("x.flux-lock.broken.1")).unwrap();
        assert!(!moved.try_lock().unwrap(), "the lock moved with the object");
        assert_eq!(moved.read_all(4096).unwrap(), b"record");
        assert_eq!(moved.identity().unwrap(), holder.identity().unwrap());
    }

    #[test]
    fn a_fake_open_lock_refuses_what_the_real_ones_refuse() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let e = d.open_lock(OsStr::new("absent")).unwrap_err();
        assert_eq!((e.code, e.source.kind()), (Code::IoError, std::io::ErrorKind::NotFound));
        fs.write_file("/d/link", b"");
        fs.set_type("/d/link", FileType::Symlink);
        assert_eq!(d.open_lock(OsStr::new("link")).unwrap_err().code, Code::SafetyRejected);
        drop(d.create_dir(OsStr::new("sub")).unwrap());
        let e = d.open_lock(OsStr::new("sub")).unwrap_err();
        assert_eq!((e.code, e.source.kind()), (Code::DestinationError, std::io::ErrorKind::IsADirectory));
    }

    #[test]
    fn a_fake_lock_write_overwrites_in_place() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let h = d.create_lock(OsStr::new("x")).unwrap();
        h.write_at_start(&[1u8; 8]).unwrap();
        h.write_at_start(&[2u8; 4]).unwrap();
        assert_eq!(h.read_all(100).unwrap(), vec![2, 2, 2, 2, 1, 1, 1, 1]);
        assert_eq!(h.read_all(3).unwrap().len(), 4, "limit + 1");
    }
```

- [ ] **Step 7: gates.**
  - `cargo test -p flux-platform --test lock_file` → all pass on Windows.
  - `cargo test -p flux-core fault_fs` → all pass.
  - `just check`.
  - `just check-linux` → `GATE: linux OK`; this runs the Unix arm and its tests.
  - `just check-mac` → exit 0.
- [ ] **Step 8:** commit the files of this task:
  `feat: lock files through a directory handle, with a non-blocking OS-native lock (cut 7a Part 1)`.

---

### Task 3: the lock across two processes

**Files:** Modify `crates/flux-platform/tests/lock_file.rs` (append).

- [ ] **Step 1: append the test.**

```rust
/// Run as a child by `a_lock_held_by_another_process_is_busy_and_readable_until_that_process_dies`: take the lock on
/// the named file, say so, and hold it until killed.
fn hold_lock_as_child(dir: &str) {
    let d = StdFileSystem.destination_root(std::path::Path::new(dir)).expect("child opens the directory");
    let lock = d.open_lock(OsStr::new(NAME)).expect("child opens the lock");
    assert!(lock.try_lock().expect("child try_lock"), "the parent released it before spawning");
    println!("LOCKED");
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(120));
}

#[test]
fn a_lock_held_by_another_process_is_busy_and_readable_until_that_process_dies() {
    if let Ok(dir) = std::env::var("FLUX_LOCK_CHILD_DIR") {
        hold_lock_as_child(&dir);
        return;
    }
    let (tmp, d) = dir();
    let record: Vec<u8> = (0..4096u32).map(|i| (i % 253) as u8).collect();
    {
        let h = d.create_lock(OsStr::new(NAME)).unwrap();
        h.write_at_start(&record).unwrap();
        h.sync_all().unwrap();
    } // closed: nobody holds the lock

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_lock_held_by_another_process_is_busy_and_readable_until_that_process_dies",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FLUX_LOCK_CHILD_DIR", tmp.path())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the child");
    let stdout = child.stdout.take().unwrap();
    let mut lines = std::io::BufRead::lines(std::io::BufReader::new(stdout));
    assert!(
        lines.any(|l| l.map(|l| l.trim() == "LOCKED").unwrap_or(false)),
        "the child reports it holds the lock"
    );

    let probe = d.open_lock(OsStr::new(NAME)).unwrap();
    assert!(!probe.try_lock().unwrap(), "another PROCESS holds it: TARGET_LOCK_BUSY");
    assert_eq!(probe.read_all(4096).unwrap(), record, "and its record is readable meanwhile");

    child.kill().unwrap();
    child.wait().unwrap();
    // The OS releases a dead process's lock when it closes the process's handles; allow it a moment.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !probe.try_lock().unwrap() {
        assert!(std::time::Instant::now() < deadline, "a dead process's lock was never released");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
```

- [ ] **Step 2:** `cargo test -p flux-platform --test lock_file a_lock_held_by_another_process` → ok on Windows; then
  `just check`, `just check-linux`.
- [ ] **Step 3: non-vacuity (do not commit).**
  - In the Windows `os_try_lock`, temporarily lock `u32::MAX, u32::MAX` bytes from offset 0 (the probe's full range)
    instead of `LOCK_BYTE`. Re-run the test: it must FAIL at "its record is readable meanwhile". Revert.
  - Temporarily make the Unix `os_try_lock` return `Ok(true)` unconditionally. `just check-linux` must fail this test
    at "another PROCESS holds it". Revert.
  - Temporarily make `StdLock::try_lock` skip the `held` check. On Windows,
    `a_second_handle_cannot_take_a_held_lock_until_the_first_is_dropped` must fail at "re-taking a lock this handle
    holds is granted". Revert.
  - Record all three outcomes in the report.
- [ ] **Step 4:** commit `crates/flux-platform/tests/lock_file.rs`:
  `test(platform): the OS-native lock across two processes (cut 7a Part 1)`.

---

### Task 4: lock capability

**Files:** modify `crates/flux-fs/src/fs.rs`, `crates/flux-platform/src/lock_file.rs`,
`crates/flux-platform/src/dir_unix.rs`, `crates/flux-platform/src/dir_windows.rs` and
`crates/flux-core/src/fault_fs.rs`; append tests.

- [ ] **Step 0:** Task 2's trait methods exist (`fn open_lock` in `fs.rs`).
- [ ] **Step 1: tests.** Append to `crates/flux-platform/tests/lock_file.rs`:

```rust
#[test]
fn the_test_machines_scratch_directory_is_a_local_strong_filesystem() {
    // CI and development machines put their temporary directory on a local filesystem (ext4 or tmpfs, APFS, NTFS or
    // ReFS). WSL's /mnt/* (9p) is NOT local and would answer Unsupported - which is why `just check-linux` builds in
    // the WSL-native filesystem.
    let (_tmp, d) = dir();
    assert_eq!(d.lock_capability().unwrap(), flux_fs::LockCapability::LocalStrong);
}
```

  Append to the fake's tests:

```rust
    #[test]
    fn the_fake_answers_the_capability_it_is_told() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        assert_eq!(d.lock_capability().unwrap(), LockCapability::LocalStrong);
        fs.set_lock_capability(LockCapability::Unsupported);
        assert_eq!(d.lock_capability().unwrap(), LockCapability::Unsupported);
    }
```

- [ ] **Step 2: the trait.** In `fs.rs`, after `fn open_lock`'s declaration add:

```rust

    /// The lock capability of the filesystem this directory is on (§235.1). A failure to tell is `Unsupported`, never
    /// an error: §235.4 prefers refusing.
    fn lock_capability(&self) -> Result<crate::LockCapability>;
```

- [ ] **Step 3: the platform.** Append to `crates/flux-platform/src/lock_file.rs`:

```rust

/// The built-in LOCAL allowlist (cut 7a spec, F1). Everything else - SMB, NFS, 9p, FUSE, FAT, overlay - is
/// `Unsupported`, and so is any failure to tell.
#[cfg(target_os = "linux")]
pub(crate) fn capability_of(fd: &std::os::fd::OwnedFd) -> flux_fs::LockCapability {
    // ext2/3/4, XFS, Btrfs, tmpfs, F2FS (statfs(2) f_type magic numbers).
    const LOCAL: [u32; 5] = [0xEF53, 0x5846_5342, 0x9123_683E, 0x0102_1994, 0xF2F5_2010];
    match rustix::fs::fstatfs(fd) {
        // f_type's width differs by architecture; every magic fits in 32 bits.
        #[allow(clippy::unnecessary_cast)]
        Ok(st) if LOCAL.contains(&(st.f_type as u64 as u32)) => flux_fs::LockCapability::LocalStrong,
        _ => flux_fs::LockCapability::Unsupported,
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn capability_of(fd: &std::os::fd::OwnedFd) -> flux_fs::LockCapability {
    match rustix::fs::fstatfs(fd) {
        Ok(st) => {
            let name: Vec<u8> = st.f_fstypename.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
            match name.as_slice() {
                b"apfs" | b"hfs" => flux_fs::LockCapability::LocalStrong,
                _ => flux_fs::LockCapability::Unsupported,
            }
        }
        Err(_) => flux_fs::LockCapability::Unsupported,
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(crate) fn capability_of(_fd: &std::os::fd::OwnedFd) -> flux_fs::LockCapability {
    flux_fs::LockCapability::Unsupported
}

/// NTFS or ReFS, on a fixed or removable drive. The filesystem NAME alone is not enough: an SMB share of an NTFS
/// volume reports "NTFS", so the drive type decides local versus remote.
#[cfg(windows)]
pub(crate) fn capability_of(h: &std::os::windows::io::OwnedHandle) -> flux_fs::LockCapability {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW, GetVolumePathNameW,
    };
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    let raw = h.as_raw_handle() as HANDLE;

    let mut fs_name = [0u16; 64];
    // SAFETY: every out-pointer is either null (not wanted) or a live buffer with its length passed.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            raw,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return flux_fs::LockCapability::Unsupported;
    }
    let end = fs_name.iter().position(|&c| c == 0).unwrap_or(fs_name.len());
    let fs_name = String::from_utf16_lossy(&fs_name[..end]);
    if fs_name != "NTFS" && fs_name != "ReFS" {
        return flux_fs::LockCapability::Unsupported;
    }

    let mut path = vec![0u16; 32_768];
    // SAFETY: `path` is live and its length is passed; flags 0 = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS.
    let n = unsafe { GetFinalPathNameByHandleW(raw, path.as_mut_ptr(), path.len() as u32, 0) } as usize;
    if n == 0 || n >= path.len() {
        return flux_fs::LockCapability::Unsupported;
    }
    path.truncate(n);
    let text = String::from_utf16_lossy(&path);
    if text.starts_with(r"\\?\UNC\") {
        return flux_fs::LockCapability::Unsupported;
    }
    // "\\?\C:\x" -> "C:\x": GetVolumePathNameW and GetDriveTypeW are given the ordinary form.
    let plain: Vec<u16> = text.strip_prefix(r"\\?\").unwrap_or(&text).encode_utf16().chain(Some(0)).collect();
    let mut root = vec![0u16; plain.len() + 1];
    // SAFETY: `plain` is NUL-terminated; `root` is live with its length passed.
    let ok = unsafe { GetVolumePathNameW(plain.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
    if ok == 0 {
        return flux_fs::LockCapability::Unsupported;
    }
    // SAFETY: GetVolumePathNameW wrote a NUL-terminated root.
    match unsafe { GetDriveTypeW(root.as_ptr()) } {
        DRIVE_FIXED | DRIVE_REMOVABLE => flux_fs::LockCapability::LocalStrong,
        _ => flux_fs::LockCapability::Unsupported,
    }
}
```

  In `dir_unix.rs`'s `impl DirHandle` add `fn lock_capability(&self) -> Result<flux_fs::LockCapability> { Ok(crate::lock_file::capability_of(&self.0)) }`.
  In `dir_windows.rs`'s `impl DirHandle` add the same body. Make the module visible to them: in `lib.rs` change
  `mod lock_file;` to `pub(crate) mod lock_file;`.
  In the fake's `impl DirHandle for FakeDirHandle` add:

```rust

    fn lock_capability(&self) -> Result<LockCapability> {
        self.fs().record("lock_capability()".to_string(), "lock_capability")?;
        Ok(self.inner.lock().unwrap().lock_capability.unwrap_or(LockCapability::LocalStrong))
    }
```

- [ ] **Step 4: gates.**
  - `cargo test -p flux-platform --test lock_file the_test_machines` → ok on Windows. The dev machine's scratch
    directory is on C: (NTFS) or E: (ReFS Dev Drive); report which.
  - `just check`.
  - `just check-linux` → `GATE: linux OK`.
  - `just check-mac` → exit 0.
- [ ] **Step 5: non-vacuity (do not commit).** Remove `0xEF53` from the Linux list: `just check-linux` must fail
  `the_test_machines_scratch_directory_is_a_local_strong_filesystem` if its scratch directory is ext4. Report the
  `f_type` it printed; if the scratch directory is tmpfs, remove `0x0102_1994` instead. Revert.
- [ ] **Step 6:** commit: `feat: lock capability from a built-in local allowlist (cut 7a Part 1, F1)`.

---

### Task 5: the boot session ID

**Files:** modify `crates/flux-platform/src/lock_file.rs`, `crates/flux-platform/src/lib.rs`,
`crates/flux-platform/Cargo.toml` and `Cargo.toml`; append a test.

- [ ] **Step 1: dependencies.**
  - In `Cargo.toml` `[workspace.dependencies]` add `libc = "0.2"`.
  - Append `"Wdk_System_SystemInformation"` to the `windows-sys` features list.
  - In `crates/flux-platform/Cargo.toml`, after the `[target.'cfg(windows)'.dependencies]` block, add:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
libc = { workspace = true }
```

- [ ] **Step 2: the test.** Append to `crates/flux-platform/tests/lock_file.rs`:

```rust
#[test]
fn the_boot_session_id_is_known_here_and_stable_within_one_boot() {
    let a = flux_platform::boot_session_id();
    let b = flux_platform::boot_session_id();
    assert_ne!(a, "unknown", "every supported platform exposes one");
    assert!(!a.is_empty());
    assert_eq!(a, b, "two reads in one boot agree");
    #[cfg(target_os = "linux")]
    assert_eq!(a, std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap().trim());
}
```

- [ ] **Step 3: the function.** Append to `crates/flux-platform/src/lock_file.rs`:

```rust

/// The host's boot session, for the lock record's `boot_session_id` (§259.6, §229). `"unknown"` when the platform
/// cannot say, and such a value never helps prove a reboot.
#[cfg(target_os = "linux")]
pub fn boot_session_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// `kern.boottime`, fixed for the whole boot.
#[cfg(target_os = "macos")]
pub fn boot_session_id() -> String {
    let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
    let mut len = std::mem::size_of::<libc::timeval>();
    let name = c"kern.boottime";
    // SAFETY: `name` is NUL-terminated; `tv` and `len` are live and correctly sized.
    let rc = unsafe {
        libc::sysctlbyname(name.as_ptr(), (&raw mut tv).cast(), &raw mut len, std::ptr::null_mut(), 0)
    };
    if rc == 0 { format!("{}.{:06}", tv.tv_sec, tv.tv_usec) } else { "unknown".to_string() }
}

/// `BootTime` from `SystemTimeOfDayInformation`, fixed at boot (100 ns units since 1601). Not "now minus
/// GetTickCount64": that differs by a second between two processes of one boot (plan decision 2).
#[cfg(windows)]
pub fn boot_session_id() -> String {
    use windows_sys::Wdk::System::SystemInformation::NtQuerySystemInformation;
    #[repr(C)]
    struct SystemTimeOfDayInformation {
        boot_time: i64,
        current_time: i64,
        time_zone_bias: i64,
        time_zone_id: u32,
        reserved: u32,
        boot_time_bias: u64,
        sleep_time_bias: u64,
    }
    const SYSTEM_TIME_OF_DAY_INFORMATION: i32 = 3;
    let mut info: SystemTimeOfDayInformation = unsafe { std::mem::zeroed() };
    let mut len = 0u32;
    // SAFETY: `info` is live and its size is passed; the class is the documented SystemTimeOfDayInformation.
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_TIME_OF_DAY_INFORMATION as _,
            (&raw mut info).cast(),
            std::mem::size_of::<SystemTimeOfDayInformation>() as u32,
            &raw mut len,
        )
    };
    if status == 0 && info.boot_time != 0 { info.boot_time.to_string() } else { "unknown".to_string() }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn boot_session_id() -> String {
    "unknown".to_string()
}
```

  In `crates/flux-platform/src/lib.rs` change `pub use lock_file::StdLock;` to
  `pub use lock_file::{StdLock, boot_session_id};`.

- [ ] **Step 4: gates.**
  - `cargo test -p flux-platform --test lock_file boot_session` → ok (report the value printed on this machine).
  - `just check`.
  - `just check-linux` → `GATE: linux OK`.
  - `just check-mac` → exit 0.
  - `cargo deny check` (or the repo's `just` recipe for it, if one exists) → passes with `libc` added.
- [ ] **Step 5:** commit: `feat(platform): the host's boot session id (cut 7a Part 1)`.

---

### Task 6: operation and owner-instance IDs (`flux-core`)

**Files:** modify `Cargo.toml`, `crates/flux-core/Cargo.toml` and `crates/flux-core/src/lib.rs`; create
`crates/flux-core/src/ids.rs`.

- [ ] **Step 1:**
  - In `Cargo.toml` `[workspace.dependencies]` add `uuid = { version = "1", features = ["v4"] }`.
  - In `crates/flux-core/Cargo.toml` `[dependencies]` add `uuid = { workspace = true }` and `blake3 = { workspace = true }`
    (the latter for Task 7).
- [ ] **Step 2:** create `crates/flux-core/src/ids.rs`:

```rust
//! Operation and owner-instance IDs (cut 7a spec, "Operation ID and owner instance ID"; §18.1: collision-resistant).

/// A fresh random version-4 UUID as 32 lowercase hex digits, no dashes.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Whether `s` has exactly the shape `new_id` produces.
pub fn is_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_32_lowercase_hex_digits_and_fresh_each_time() {
        let a = new_id();
        assert!(is_id(&a), "{a}");
        assert_ne!(a, new_id());
    }

    #[test]
    fn is_id_rejects_every_other_shape() {
        for bad in ["", "0123456789abcdef0123456789abcde", "0123456789ABCDEF0123456789ABCDEF",
                    "0123456789abcdef0123456789abcdeg", "01234567-89ab-cdef-0123-456789abcdef"] {
            assert!(!is_id(bad), "{bad}");
        }
    }
}
```

  In `crates/flux-core/src/lib.rs` add `pub mod ids;` after `pub mod copy;`.
- [ ] **Step 3:** `cargo test -p flux-core ids` → 2 passed; `just check`; `cargo deny check` passes with `uuid`.
- [ ] **Step 4:** commit: `feat(core): random operation and owner-instance ids (cut 7a Part 1)`.

---

### Task 7: the lock record codec (`flux-core`)

**Files:** create `crates/flux-core/src/lock/mod.rs` and `crates/flux-core/src/lock/record.rs`; modify
`crates/flux-core/src/lib.rs`.

- [ ] **Step 1:** create `crates/flux-core/src/lock/mod.rs`:

```rust
//! The destination lock (§96.1, §240, §259.6). Part 1 holds only the record codec; the protocol is Part 2.

pub mod record;
```

  In `lib.rs` add `pub mod lock;` after `pub mod ids;`.
- [ ] **Step 2:** create `crates/flux-core/src/lock/record.rs` with its tests (the tests are the oracle):

```rust
//! The lock record, `format_version` 1 (§259.6; cut 7a spec, "Lock record" and "Decoding").
//!
//! A fixed 4096 bytes: the magic, the version, eight length-prefixed UTF-8 fields in the order of spec:12975-12982,
//! zero padding, and the first 16 bytes of the BLAKE3 hash of everything before them.

pub const RECORD_LEN: usize = 4096;
pub const MAGIC: [u8; 8] = *b"FLUXLOCK";
pub const FORMAT_VERSION: u32 = 1;
const CHECKSUM_AT: usize = RECORD_LEN - 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockRecord {
    pub complete_lock_key: String,
    pub operation_id: String,
    pub owner_instance_id: String,
    pub boot_session_id: String,
    pub target_path_key: String,
    /// One of `operations/<id>`, `adjacent/<id>`, `none` (cut 7a spec); the codec does not judge it.
    pub workspace_path: String,
    /// Nanoseconds since the Unix epoch, UTC; written as decimal ASCII.
    pub creation_wall_time: u64,
    pub last_heartbeat_wall_time: u64,
}

/// What a lock file's bytes are (cut 7a spec, "Decoding", in its order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    Record(LockRecord),
    /// Ownership cannot be established from these bytes: `TARGET_LOCK_UNCERTAIN`, never foreign.
    Uncertain(Uncertain),
    /// Not a Flux record: `CONTROL_PLANE_NAMESPACE_CONFLICT`, never overwritten.
    Foreign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uncertain {
    Empty,
    /// A torn write: a zero-or-magic prefix of at most 4096 bytes, or the magic with the wrong length.
    Torn,
    Checksum,
    /// A newer `format_version` with a valid checksum; 7a never guesses its layout.
    UnknownVersion(u32),
    /// A valid checksum over a layout this version cannot parse.
    Malformed,
}

impl LockRecord {
    /// Exactly `RECORD_LEN` bytes. Panics if the fields do not fit: every field is bounded (spec: "an encoder that
    /// would exceed it is a bug"), and a truncated record would be worse than a crash.
    pub fn encode(&self) -> Vec<u8> {
        let times = [self.creation_wall_time.to_string(), self.last_heartbeat_wall_time.to_string()];
        let fields: [&str; 8] = [
            &self.complete_lock_key,
            &self.operation_id,
            &self.owner_instance_id,
            &self.boot_session_id,
            &self.target_path_key,
            &self.workspace_path,
            &times[0],
            &times[1],
        ];
        let mut out = Vec::with_capacity(RECORD_LEN);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        for f in fields {
            let len = u16::try_from(f.len()).expect("a lock-record field is far below 64 KiB");
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(f.as_bytes());
        }
        assert!(out.len() <= CHECKSUM_AT, "lock record fields take {} bytes, more than {CHECKSUM_AT}", out.len());
        out.resize(CHECKSUM_AT, 0);
        let sum = blake3::hash(&out);
        out.extend_from_slice(&sum.as_bytes()[..16]);
        out
    }
}

/// Classify a lock file's bytes (cut 7a spec, "Decoding", in this order).
pub fn decode(bytes: &[u8]) -> Decoded {
    if bytes.is_empty() {
        return Decoded::Uncertain(Uncertain::Empty);
    }
    let zero_or_magic = bytes.iter().zip(MAGIC.iter()).all(|(b, m)| *b == 0 || b == m);
    let whole_magic = bytes.len() >= MAGIC.len() && bytes[..MAGIC.len()] == MAGIC;
    if !zero_or_magic || (bytes.len() > RECORD_LEN && !whole_magic) {
        return Decoded::Foreign;
    }
    if bytes.len() != RECORD_LEN || !whole_magic {
        return Decoded::Uncertain(Uncertain::Torn);
    }
    if blake3::hash(&bytes[..CHECKSUM_AT]).as_bytes()[..16] != bytes[CHECKSUM_AT..] {
        return Decoded::Uncertain(Uncertain::Checksum);
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("four bytes"));
    if version != FORMAT_VERSION {
        return Decoded::Uncertain(Uncertain::UnknownVersion(version));
    }
    parse_v1(&bytes[12..CHECKSUM_AT]).map_or(Decoded::Uncertain(Uncertain::Malformed), Decoded::Record)
}

/// The eight fields of `format_version` 1, then zero padding to the checksum. `None` for anything else.
fn parse_v1(body: &[u8]) -> Option<LockRecord> {
    let mut at = 0usize;
    let complete_lock_key = next_field(body, &mut at)?;
    let operation_id = next_field(body, &mut at)?;
    let owner_instance_id = next_field(body, &mut at)?;
    let boot_session_id = next_field(body, &mut at)?;
    let target_path_key = next_field(body, &mut at)?;
    let workspace_path = next_field(body, &mut at)?;
    let creation_wall_time = time(&next_field(body, &mut at)?)?;
    let last_heartbeat_wall_time = time(&next_field(body, &mut at)?)?;
    // Everything after the fields is padding, and padding is zero: anything else is a layout this version does not
    // know, so it is Malformed rather than silently ignored.
    if body[at..].iter().any(|&b| b != 0) {
        return None;
    }
    Some(LockRecord {
        complete_lock_key,
        operation_id,
        owner_instance_id,
        boot_session_id,
        target_path_key,
        workspace_path,
        creation_wall_time,
        last_heartbeat_wall_time,
    })
}

/// One u16 LE length and that many bytes of UTF-8, advancing `at` past them.
fn next_field(body: &[u8], at: &mut usize) -> Option<String> {
    let len = u16::from_le_bytes(body.get(*at..*at + 2)?.try_into().ok()?) as usize;
    let text = std::str::from_utf8(body.get(*at + 2..*at + 2 + len)?).ok()?.to_string();
    *at += 2 + len;
    Some(text)
}

/// Decimal ASCII digits only: no sign, no space, no empty string (`str::parse` alone would accept a leading `+`).
fn time(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) { None } else { s.parse().ok() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> LockRecord {
        LockRecord {
            complete_lock_key: "vol:1:obj:77/dest".to_string(),
            operation_id: "0123456789abcdef0123456789abcdef".to_string(),
            owner_instance_id: "fedcba9876543210fedcba9876543210".to_string(),
            boot_session_id: "9f1c2d3e-0000-4000-8000-000000000001".to_string(),
            target_path_key: "dest".to_string(),
            workspace_path: "operations/0123456789abcdef0123456789abcdef".to_string(),
            creation_wall_time: 1_790_000_000_123_456_789,
            last_heartbeat_wall_time: 1_790_000_000_123_456_789,
        }
    }

    /// Re-checksum `bytes` after a test changed something before the checksum.
    fn reseal(mut bytes: Vec<u8>) -> Vec<u8> {
        let sum = blake3::hash(&bytes[..CHECKSUM_AT]);
        bytes[CHECKSUM_AT..].copy_from_slice(&sum.as_bytes()[..16]);
        bytes
    }

    #[test]
    fn a_record_round_trips_at_exactly_4096_bytes() {
        let bytes = sample().encode();
        assert_eq!(bytes.len(), RECORD_LEN);
        assert_eq!(bytes[..8], MAGIC);
        assert_eq!(decode(&bytes), Decoded::Record(sample()));
    }

    #[test]
    fn empty_and_torn_files_are_uncertain_never_foreign() {
        let good = sample().encode();
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("all zero, record length", vec![0u8; RECORD_LEN]),
            ("all zero, short", vec![0u8; 100]),
            ("a partly written magic", [b"FLUX".as_slice(), &[0u8; 4], &[9u8; 50]].concat()),
            ("a zero first sector, later bytes written", [&[0u8; 512][..], &good[512..]].concat()),
            ("the magic, truncated", good[..4000].to_vec()),
            ("the magic, too long", [good.as_slice(), &[1u8; 10]].concat()),
        ];
        assert_eq!(decode(&[]), Decoded::Uncertain(Uncertain::Empty));
        for (why, bytes) in cases {
            assert_eq!(decode(&bytes), Decoded::Uncertain(Uncertain::Torn), "{why}");
        }
    }

    #[test]
    fn a_failed_checksum_is_uncertain() {
        let mut bytes = sample().encode();
        bytes[20] ^= 1;
        assert_eq!(decode(&bytes), Decoded::Uncertain(Uncertain::Checksum));
    }

    #[test]
    fn a_newer_version_with_a_valid_checksum_is_uncertain_not_parsed() {
        let mut bytes = sample().encode();
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(decode(&reseal(bytes)), Decoded::Uncertain(Uncertain::UnknownVersion(2)));
    }

    #[test]
    fn nonzero_padding_or_a_non_numeric_time_is_malformed() {
        let mut bytes = sample().encode();
        bytes[CHECKSUM_AT - 1] = 1;
        assert_eq!(decode(&reseal(bytes)), Decoded::Uncertain(Uncertain::Malformed));
        let mut r = sample().encode();
        // The first digit of the creation time (the first match; the heartbeat carries the same value).
        let at = r.windows(19).position(|w| w == b"1790000000123456789").unwrap();
        r[at] = b'x';
        assert_eq!(decode(&reseal(r)), Decoded::Uncertain(Uncertain::Malformed));
    }

    #[test]
    fn other_content_is_foreign() {
        assert_eq!(decode(b"hello, this is somebody's file"), Decoded::Foreign);
        assert_eq!(decode(&[0u8; RECORD_LEN + 1]), Decoded::Foreign, "over 4096 bytes without the magic");
        assert_eq!(decode(b"FLUXLOCX"), Decoded::Foreign, "one byte off the magic, neither zero nor magic");
    }

    #[test]
    #[should_panic(expected = "more than")]
    fn an_encoder_that_would_exceed_the_size_panics_rather_than_truncate() {
        let mut r = sample();
        r.target_path_key = "k".repeat(5000);
        r.encode();
    }
}
```

- [ ] **Step 3:** `cargo test -p flux-core lock::record` → 7 passed; `just check`.
- [ ] **Step 4: non-vacuity (do not commit):**
  - swap `*b == 0 || b == m` for `b == m` in `decode`: `empty_and_torn_files_are_uncertain_never_foreign` must fail;
  - remove the checksum comparison: `a_failed_checksum_is_uncertain` must fail;
  - revert both, and report.
- [ ] **Step 5:** commit: `feat(core): the lock record codec, format_version 1 (cut 7a Part 1)`.

---

### Task 8: Part 1 verification

- [ ] **Step 1:** `just check`, `just check-linux` (`GATE: linux OK`) and `just check-mac` all pass; `git status --short`
  is clean.
- [ ] **Step 2 (driver):** push `spec/cut-7a` (a branch push, no PR, once the owner approves it) so that CI's three
  platforms run the new tests. Then write the Part 2 plan against the code this part created.

---

## Self-review

- **Spec coverage (the parts of the 7a spec that Part 1 owns):**
  - "flux-platform": the OS-native lock (Tasks 2-3), open-existing, read, handle identity (Task 2), capability
    (Task 4), `boot_session_id` (Task 5);
  - "flux-fs": the traits (Tasks 1-2, 4) and the fake (Tasks 2, 4);
  - "Formats: Operation ID and owner instance ID" (Task 6);
  - "Formats: Lock record" and "Decoding" (Task 7).
  - Everything else is Parts 2 and 3.
- **Declared against the spec:** the Windows lock byte (decision 1) and the Windows boot source (decision 2).
- **Type consistency:**
  - `LockFile { try_lock, read_all, write_at_start, sync_all, identity }`;
  - `DirHandle::{Lock, create_lock, open_lock, lock_capability}`;
  - `StdLock`, `FakeLock`, `capability_of`, `boot_session_id`;
  - `ids::{new_id, is_id}`;
  - `lock::record::{LockRecord, Decoded, Uncertain, decode, RECORD_LEN, MAGIC, FORMAT_VERSION}`.
