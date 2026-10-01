# Cut 7a Part 3a: the operation state and the prior-state scan - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Everything Part 3b's run needs below the CLI:
- four directory-handle primitives, on both platforms and the fake;
- directory renames in the fake;
- the fake's Windows legacy-delete window;
- the versioned operation state: its codec, its crash-safe write, and a workspace that never exists without its
  manifest;
- §21.1's scan of a destination's prior operations.

**Architecture:**
- `DirHandle` gains `read_dir`, `read_file`, `remove_dir` and `sync`, so the control plane under `DEST` is read and
  written through handles, as §149.7 requires of every destination entry.
- `flux-core::state` holds the JSON codec (`format_version` judged first) and the crash-safe write (temporary, flush,
  rename, flush the directory). A tree workspace is built as `operations/<id>.creating/` and renamed into place.
- `flux-core::prior` classifies every prior operation, and refuses with the new `LockCode`s.
- Nothing here changes a copy yet. Part 3b wires the run, the engine hooks and the CLI.

**Tech Stack:** Rust 2024; `rustix` 1.1 (`Dir`, `unlinkat`, `fsync`); `windows-sys` 0.61 (`NtCreateFile`,
`GetFileInformationByHandleEx`, `FlushFileBuffers`); `serde` / `serde_json` (new in `flux-core`).

**Spec:** `docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md`:
- "Operation state, `format_version` 1";
- "The run", steps 2, 4 and 5 (the parts below the CLI);
- "§21.1 classification of a prior operation's state";
- "What each crash window leaves".

**Part 3b** (planned after this part lands) is the rest:
- the run's orchestration, steps 1-7, including `--restart`, the `--break-lock` state order and the finish path;
- the engine's §99 hooks;
- the reserved `.flux` paths;
- the CLI flags, the exit codes and the end-to-end tests.

---

## What this plan rests on (read and verified at `4066fb2`)

- `crates/flux-fs/src/fs.rs`:
  - `DirEntry { name: OsString, file_type: FileType }` (`:41-44`);
  - `FileType::{File, Dir, Symlink, Other}` (`:30-35`);
  - `trait DirHandle` (`:223-320`), whose last method is `fn lock_capability(&self) -> Result<crate::LockCapability>;`
    (`:319`) followed by the trait's closing `}` (`:320`).
- `crates/flux-platform/src/dir_unix.rs`:
  - `pub struct StdDir(OwnedFd)` (`:11`);
  - `impl DirHandle for StdDir` (`:68-245`), ending with `open_lock`, whose last line is
    `Ok(crate::StdLock::new(std::fs::File::from(fd)))` (`:243`).
- `crates/flux-platform/src/dir_windows.rs`:
  - the constants `FILE_READ_ATTRIBUTES`, `FILE_LIST_DIRECTORY`, `FILE_SYNCHRONOUS_IO_NONALERT` (`:33-35`) and the
    `STATUS_*` constants (`:37-42`);
  - `nt_io_error` (`:52-79`);
  - `is_name_surrogate` (`:91-93`);
  - `impl DirHandle for StdDir` (`:106-254`), ending with `lock_capability` (`:251-253`);
  - `remove_file_at` (`:550-676`), whose disposition code starts at the comment `// POSIX delete first: the NAME goes
    at once` (`:624`);
  - `reparse_tag_of` (`:1009`);
  - `mod judge_tests` (`:1176`).
- `crates/flux-platform/src/lock_file.rs`: `fn read_loop(limit, read_at)` (`:148-163`) reads at most `limit + 1`
  bytes. It is private today.
- `crates/flux-core/src/fault_fs.rs`:
  - `struct Inner` (`:18-100`), which ends with the `hooks` field;
  - `struct DirNode` and its doc comment (`:102-114`);
  - `missing_source` (`:140-145`), `mint_identity` (`:155-163`) and `move_object` (`:168-202`);
  - `FaultFs::metadata` (`:647`), `rename_replace` (`:711`), `rename_no_replace` (`:723`), `remove_file`
    (`:754-777`) and `read_dir` (`:779-806`);
  - `FakeDirHandle::lock_for` (`:926-938`);
  - `impl DirHandle for FakeDirHandle` (`:941-1140`), ending with `lock_capability` (`:1137-1139`);
  - `impl Drop for FakeLock` (`:352-360`);
  - `mod tests` (`:1143`).
- `crates/flux-core/src/lock/`:
  - `error.rs`: `LockCode` (6 variants), `refuse` (`pub(crate)`) and `LockResult`;
  - `site.rs`: `NAME_LIMIT = 255` (`:17`) and two private `fn name_len` (`:209-217`);
  - `obtain.rs`: its `mod tests` imports (`:145-151`);
  - `test_support.rs`: `fake()`, `record()` and `refusal()`.
- `crates/flux-core/src/ids.rs`: `new_id()` and `is_id()`.
- `crates/flux-core/Cargo.toml` `[dependencies]` is `flux-fs`, `uuid` and `blake3`. The workspace defines `serde`
  (with `derive`) and `serde_json` (`Cargo.toml` `[workspace.dependencies]`).
- `rustix` 1.1.5 (`E:\.cargo\registry\...\rustix-1.1.5`):
  - `fs::Dir::read_from(fd)` opens its own descriptor with `openat(fd, ".")` (`backend/*/fs/dir.rs`);
  - `DirEntry::{file_name() -> &CStr, file_type() -> FileType}` (`FileType::Unknown` exists);
  - `AtFlags::REMOVEDIR`, `fs::fsync`;
  - the `std` feature (a default) enables `alloc`, which `Dir` needs.
- **Measured on this machine, 2026-09-30** (scratch probe, NTFS `C:` and ReFS `E:`):
  - `NtCreateFile` relative to a directory handle with an EMPTY name reopens that directory;
  - `GetFileInformationByHandleEx(FileFullDirectoryRestartInfo, then FileFullDirectoryInfo)` on the reopened handle
    lists it, including `.` and `..`;
  - a junction's entry has attributes `0x410` and `EaSize` = `0xA0000003` (its reparse tag), while a plain entry's
    `EaSize` is a real EA size (`..` gave `0xDC` on NTFS);
  - `FlushFileBuffers` on a directory handle fails `ACCESS_DENIED` with list access only, and succeeds with
    `FILE_ADD_FILE` (`0x2`).

## Decisions (AGY-FIRST, then the owner, 2026-09-30)

The seams are `.clavity/seams/cut7a-p3-forks.md` and `cut7a-p3-forks-2.md`. The replies are in
`.clavity/scratch/cut7a-p3/`. B and J were settled after one negotiation turn.

1. **Part 3 is cut in two (P3-A: A2).** This plan is 3a.
2. **The control plane goes through directory handles (P3-B: B1, §149.7).**
   - `DirHandle` gains `read_dir`, `read_file(name, limit)`, `remove_dir` and `sync`.
   - On Windows, `read_dir` and `sync` reopen the directory relative to its own handle (an empty name), with the
     access each needs. `sync` uses `FILE_ADD_FILE` (measured above).
3. **A tree workspace never exists without its manifest (P3-K: K1).**
   - It is built as `operations/<id>.creating/`.
   - Its manifest is written inside, crash-safely.
   - It is then renamed to `operations/<id>` without replacing, and `operations/` is flushed.
   - The §21.1 scan ignores every entry of `operations/` that is not a directory named by an id: a crash's
     `<id>.creating`, or a stray file. Those are cut 9's.
   - Without this, a crash between the `mkdir` and the manifest's rename would leave a manifest-less workspace:
     `STATE_CORRUPT`, which no flag clears, behind an empty lock.
4. **Temporary names (P3-L: L2).**
   - A state file's temporary is `<name>.tmp`. The staged name already carries the operation id (`manifest` lives in
     `<id>/`, and the record is `<target>.flux-state.<id>`).
   - `state::file_names_fit` answers whether a single-file run's longest name fits `NAME_LIMIT`. That name is its
     record's temporary: target + 48 units.
   - Part 3b refuses `PATH_COMPONENT_INVALID` up front when it does not.
5. **`destination_root` is the display form (P3-M: M1).** It is lossy for a path that is not valid Unicode, and it is
   never read back for a decision.
6. **The fake models Windows' legacy delete (P3-I: I1).**
   - A file removed while a lock handle on it is open stays at its name, delete-pending, until the last handle closes.
   - `obtain` then ends in `TARGET_LOCK_BUSY` after its attempt budget, and succeeds once the handle closes.
7. **The prior-state refusals are `LockCode`s (P3-H: H1, "the state codes added to `LockCode`"):**
   `StateCorrupt`, `IncompatibleState` and `ResumableOperationExists`. The scan returns `LockResult`, so every exit-3
   refusal of the run is one `Refusal` type.
8. **Scan order and precedence.**
   - Entries are judged in name order.
   - The first unusable state refuses (`STATE_CORRUPT` or `INCOMPATIBLE_STATE`), even beside resumable ones, because
     `--restart` cannot clear it (spec:1664-1666).
   - Otherwise the scan returns every resumable operation. Deciding `RESUMABLE_OPERATION_EXISTS` against `--restart`
     is 3b's.
9. **An entry that changes between the listing and its read** is judged as what the read finds:
   - a workspace that is gone, or no longer a directory, is not an operation;
   - a single-file record that is gone was removed by its own finishing run;
   - a workspace whose manifest is gone is `STATE_CORRUPT` (§21.1's last row);
   - a record's exact name holding anything but a regular file is `STATE_CORRUPT` (panel round 3). The spec
     classifies every record. Unlike an `operations/` entry (decision 3), the name beside a user's target is Flux's by
     F2 alone, and no crash leaves a non-file there.
10. **The fake moves a directory** (with its subtree, its identities, and the snapshot paths of handles open on or
    below it) only through `rename_no_replace`. `rename_replace` of a directory is not modelled, and the fake panics
    on it.

## Handed to Part 3b (found while planning 3a; each goes through 3b's AGY-FIRST)

- **The finish path has K1's dead end in reverse.** "Finish" step 3 removes a tree's `manifest`, then `rmdir`s the
  workspace. A crash in between leaves a manifest-less workspace: `STATE_CORRUPT`. 3b should first retire the
  workspace to a name that is not an id (for example `<id>.removing`). The same applies to `--restart` step 4, which
  deletes the prior operation's workspace.
- **A state write's temporary goes with its state (panel round 2).** When 3b removes a single-file record (the finish
  path, `--restart` step 4), it also removes `<record>.tmp`. A crash inside a later rewrite leaves that temporary
  beside the record, and the valid record naming the id is the authority to delete it. The same applies to
  `manifest.tmp` inside a workspace being removed.
- `RunError` (P3-H), the engine hook (P3-C), DEST's creation (P3-E), the reserved paths (P3-F), the `.broken` warning
  (P3-G) and the partial sweep (P3-J) are all settled calls. 3b implements them.

## Ground rules

- Worktree `E:\Rust\flux-engine`, branch `spec/cut-7a`.
- **Step 0 of every task:** confirm each quoted current text before editing. If it differs, STOP and report
  `STATE_MISMATCH: <file>: <what differs>`.
- **Shape-divergence stop:** any changed name, signature, type, error code, `ErrorKind`, constant, JSON key or
  spelling, file name, or order of steps means STOP and report `[plan] -> [yours] because <reason>`. Exceptions, which
  you report but do not stop for:
  - `cargo fmt` reformatting;
  - dropping an import the compiler reports unused;
  - adding a derive the tests need to compile;
  - a rewrite clippy demands that keeps the same behaviour (for example `sort_by` to `sort_by_key`), reported with
    the lint's name.
- **Oracle:** the tests in each task are already written. Implement until they pass; never edit an assertion.
- **Formatting:** run `cargo fmt` before every gate.
- **Exit status:** read a gate's exit status directly (`just check; echo rc=$?`), never through a pipe.
- **Gates after each task:**
  - `just check` (Windows) exits 0 with every test passing.
  - Tasks 1-5 and 11 also run `just check-linux` (exit 0, nextest summary all passed) and `just check-mac` (exit 0),
    because they touch a platform arm.
- **Non-vacuity:** every task names a logic mutant. Apply it, confirm that the NAMED new test fails, revert it, and
  do not commit it.
- **Commits:** name the paths. Each message ends with a `Co-Authored-By:` line for the model you are.

## File map

| File | Change |
|---|---|
| `crates/flux-fs/src/fs.rs` | `DirHandle::{read_dir, read_file, remove_dir, sync}` (Tasks 1-4) |
| `crates/flux-platform/src/dir_unix.rs` | the four, POSIX (Tasks 1-4) |
| `crates/flux-platform/src/dir_windows.rs` | the four, Windows; `reopen`; `delete_open` factored out of `remove_file_at` (Tasks 1-4) |
| `crates/flux-platform/src/lock_file.rs` | `read_loop` becomes `pub(crate)` (Task 2) |
| `crates/flux-platform/tests/dir_control.rs` (new) | the four and a directory rename, on the real filesystem (Tasks 1-5) |
| `crates/flux-core/src/fault_fs.rs` | the four (Tasks 1-4); directory renames (Task 5); legacy delete (Task 6) |
| `crates/flux-core/src/lock/obtain.rs` | a test: `obtain` in the delete-pending window (Task 6) |
| `crates/flux-core/src/lock/error.rs` | three `LockCode`s (Task 7) |
| `crates/flux-core/src/lock/site.rs` | `name_len` becomes `pub(crate)` (Task 8) |
| `crates/flux-core/Cargo.toml`, `src/lib.rs` | `serde`, `serde_json`; `pub mod state; pub mod prior;` (Tasks 7, 9) |
| `crates/flux-core/src/state.rs` (new) | codec (Task 7); names, crash-safe write, workspace, `operations_dir` (Task 8) |
| `crates/flux-core/src/prior.rs` (new) | §21.1 scan (Task 9) |
| `crates/flux-core/tests/state_std_fs.rs` (new) | the state and the scan on the real filesystem (Tasks 8, 9) |
| `docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md` | refinements 9-11 (Task 10) |

---

### Task 1: `DirHandle::read_dir`

**Files:**
- Modify `crates/flux-fs/src/fs.rs`, `crates/flux-platform/src/dir_unix.rs`,
  `crates/flux-platform/src/dir_windows.rs` and `crates/flux-core/src/fault_fs.rs`.
- Create `crates/flux-platform/tests/dir_control.rs`.

- [ ] **Step 0:**
  - `crates/flux-fs/src/fs.rs` has, as the last lines of `trait DirHandle`:

    ```
        fn lock_capability(&self) -> Result<crate::LockCapability>;
    }
    ```
  - `dir_unix.rs`'s `impl DirHandle for StdDir` ends:

    ```
            Ok(crate::StdLock::new(std::fs::File::from(fd)))
        }
    }
    ```
  - `dir_windows.rs`'s `impl DirHandle for StdDir` ends:

    ```
        fn lock_capability(&self) -> Result<flux_fs::LockCapability> {
            crate::lock_file::capability_of(&self.0)
        }
    }
    ```
  - `dir_windows.rs` `:34` is `const FILE_LIST_DIRECTORY: u32 = 0x1;`.
  - `fault_fs.rs`'s `impl DirHandle for FakeDirHandle` ends:

    ```
        fn lock_capability(&self) -> Result<LockCapability> {
            self.fs().record("lock_capability()".to_string(), "lock_capability")?;
            Ok(self.inner.lock().unwrap().lock_capability.unwrap_or(LockCapability::LocalStrong))
        }
    }
    ```
  - `crates/flux-platform/tests/dir_control.rs` does not exist.

- [ ] **Step 1: the tests.** Create `crates/flux-platform/tests/dir_control.rs`:

```rust
//! The directory primitives the operation state rests on (cut 7a Part 3a): listing, reading a small file, removing an
//! empty directory, flushing a directory, and renaming a directory - each through a directory handle, never following
//! a link (§149.7).

use flux_fs::{Code, DestinationRoot, DirHandle, FileType};
use flux_platform::StdFileSystem;
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::Path;

fn dir() -> (tempfile::TempDir, flux_platform::StdDir) {
    let tmp = tempfile::tempdir().expect("scratch directory");
    let d = StdFileSystem.destination_root(tmp.path()).expect("open the scratch directory");
    (tmp, d)
}

/// The listing, sorted by name.
fn listing(d: &flux_platform::StdDir) -> Vec<(OsString, FileType)> {
    let mut v: Vec<_> =
        d.read_dir().expect("list").into_iter().map(|e| (e.name, e.file_type)).collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// A link to the directory `target` at `link`: a symlink on Unix, a junction on Windows (`mklink /J` needs no
/// privilege, so this never skips).
fn dir_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("symlink");
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .expect("run mklink");
        assert!(status.success(), "mklink /J needs no privilege");
    }
}

#[test]
fn read_dir_lists_files_directories_and_links_as_links() {
    let (tmp, d) = dir();
    assert!(listing(&d).is_empty(), "an empty directory lists nothing: no . and no ..");
    std::fs::write(tmp.path().join("a"), b"x").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    dir_link(&tmp.path().join("sub"), &tmp.path().join("link"));
    assert_eq!(
        listing(&d),
        vec![
            (OsString::from("a"), FileType::File),
            (OsString::from("link"), FileType::Symlink),
            (OsString::from("sub"), FileType::Dir),
        ]
    );
}

#[test]
fn read_dir_works_on_created_and_opened_handles_and_starts_again_each_call() {
    let (_tmp, d) = dir();
    let sub = d.create_dir(OsStr::new("sub")).unwrap();
    drop(sub.create_new(OsStr::new("f")).unwrap());
    let want = vec![(OsString::from("f"), FileType::File)];
    assert_eq!(listing(&sub), want);
    let again = d.open_dir(OsStr::new("sub")).unwrap();
    assert_eq!(listing(&again), want);
    assert_eq!(listing(&again), want, "a second listing through the same handle lists everything again");
}
```

  Append to `fault_fs.rs`'s `mod tests`:

```rust
    #[test]
    fn a_fake_handle_lists_its_directory() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        fs.write_file("/p/a", b"x");
        let sub = d.create_dir(OsStr::new("sub")).unwrap();
        fs.write_file("/p/sub/deeper", b"y");
        let mut got: Vec<_> =
            d.read_dir().unwrap().into_iter().map(|e| (e.name, e.file_type)).collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            vec![(OsString::from("a"), FileType::File), (OsString::from("sub"), FileType::Dir)]
        );
        assert!(fs.called("read_dir("), "{:?}", fs.calls());
        drop(sub);
    }
```

  Append to `dir_windows.rs`'s `mod judge_tests`:

```rust
    #[test]
    fn a_listed_entry_type_reads_the_tag_only_on_a_reparse_point() {
        // Measured (cut 7a Part 3a): a junction lists as 0x410 with EaSize 0xA0000003.
        assert_eq!(entry_type(0x410, JUNCTION), flux_fs::FileType::Symlink);
        // A cloud placeholder directory is a directory, as `open_dir` treats it.
        assert_eq!(entry_type(FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT, ONEDRIVE), flux_fs::FileType::Dir);
        // EaSize is a real EA size when the reparse bit is clear, even one with the surrogate bit set.
        assert_eq!(entry_type(FILE_ATTRIBUTE_DIRECTORY, JUNCTION), flux_fs::FileType::Dir);
        assert_eq!(entry_type(FILE_ATTRIBUTE_NORMAL, JUNCTION), flux_fs::FileType::File);
    }
```

- [ ] **Step 2: the trait.** In `crates/flux-fs/src/fs.rs`, directly after
  `fn lock_capability(&self) -> Result<crate::LockCapability>;` and before the trait's closing `}`, insert:

```rust

    /// The entries of the directory this handle holds, without `.` and `..`, in no particular order (§21.1's scan).
    ///
    /// Each entry's type judges the NAME, as `metadata` does: a symlink, junction or other name-surrogate is
    /// `FileType::Symlink`, never its target's type. A directory carrying a non-surrogate reparse point (a cloud
    /// placeholder) is a `Dir`, as `open_dir` treats it.
    fn read_dir(&self) -> Result<Vec<crate::DirEntry>>;
```

- [ ] **Step 3: POSIX.** In `dir_unix.rs`, after `open_lock`'s closing `}` and before the impl's closing `}`, insert:

```rust

    fn read_dir(&self) -> Result<Vec<flux_fs::DirEntry>> {
        use rustix::fs::{Dir, FileType};
        use std::os::unix::ffi::OsStrExt;
        let io = |e: rustix::io::Errno| FsError::from_io(std::io::Error::from(e));
        let mut out = Vec::new();
        // `read_from` reads through a descriptor of its own (an `openat` of "."), so this handle is untouched and every
        // call lists from the start.
        for entry in Dir::read_from(&self.0).map_err(io)? {
            let entry = entry.map_err(io)?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let name = OsStr::from_bytes(bytes).to_os_string();
            let kind = match entry.file_type() {
                // No `d_type` on this filesystem: ask the name itself, never its target.
                FileType::Unknown => match statat(&self.0, &name, AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(st) => FileType::from_raw_mode(st.st_mode),
                    // Gone since the listing: no longer an entry.
                    Err(rustix::io::Errno::NOENT) => continue,
                    Err(e) => return Err(io(e)),
                },
                known => known,
            };
            let file_type = match kind {
                FileType::RegularFile => flux_fs::FileType::File,
                FileType::Directory => flux_fs::FileType::Dir,
                FileType::Symlink => flux_fs::FileType::Symlink,
                _ => flux_fs::FileType::Other,
            };
            out.push(flux_fs::DirEntry { name, file_type });
        }
        Ok(out)
    }
```

- [ ] **Step 4: Windows.**
  - After `const FILE_LIST_DIRECTORY: u32 = 0x1;` insert `const FILE_ADD_FILE: u32 = 0x2;`. Task 4 uses it; add it
    now so the constants stay together. If clippy reports it unused before Task 4, add
    `#[allow(dead_code)] // used by Task 4's sync` above it and remove that line in Task 4.
  - In `impl DirHandle for StdDir`, after `lock_capability`, insert:

```rust

    fn read_dir(&self) -> Result<Vec<flux_fs::DirEntry>> {
        read_dir_at(&self.0)
    }
```

  After the impl's closing `}` (before `fn create_dir_at`), insert:

```rust

/// The directory `p` holds, opened AGAIN relative to itself (an empty name), with `access`. The fresh handle carries
/// the access this caller needs and an enumeration cursor of its own. MEASURED on NTFS and ReFS (cut 7a Part 3a): the
/// empty-name open succeeds, the new handle lists the directory, and with `FILE_ADD_FILE` it can be flushed.
fn reopen(p: &OwnedHandle, access: u32) -> Result<OwnedHandle> {
    let us = UNICODE_STRING { Length: 0, MaximumLength: 0, Buffer: std::ptr::null_mut() };
    let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
    oa.RootDirectory = p.as_raw_handle() as HANDLE;
    oa.ObjectName = &raw const us;
    let mut h: HANDLE = std::ptr::null_mut();
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is to a live local that outlives the call. ShareAccess matches `open_dir`'s reasoning.
    let status = unsafe {
        NtCreateFile(
            &raw mut h,
            access | SYNCHRONIZE,
            &raw const oa,
            &raw mut iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status != 0 {
        let code = if status == STATUS_ACCESS_DENIED { Code::PermissionDenied } else { Code::IoError };
        return Err(FsError::new(code, nt_io_error("NtCreateFile", status)));
    }
    // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `h` is a valid handle we own.
    Ok(unsafe { OwnedHandle::from_raw_handle(h as _) })
}

fn read_dir_at(p: &OwnedHandle) -> Result<Vec<flux_fs::DirEntry>> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FULL_DIR_INFO, FileFullDirectoryInfo, FileFullDirectoryRestartInfo,
        GetFileInformationByHandleEx,
    };
    let h = reopen(p, FILE_LIST_DIRECTORY)?;
    // `u64` words, not bytes: each record starts 8-byte aligned inside the buffer (`NextEntryOffset` keeps it so), so
    // the buffer itself must be, for the reason `rename_at` records.
    let mut buf: Vec<u64> = vec![0u64; 8192];
    let mut out = Vec::new();
    let mut class = FileFullDirectoryRestartInfo;
    loop {
        // SAFETY: `buf` is live and writable for exactly the size given, and the handle outlives the call.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                h.as_raw_handle() as _,
                class,
                buf.as_mut_ptr().cast(),
                (buf.len() * size_of::<u64>()) as u32,
            )
        };
        if ok == 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            }
            return Err(FsError::from_io(e));
        }
        class = FileFullDirectoryInfo;
        let base: *const u8 = buf.as_ptr().cast();
        let mut off = 0usize;
        loop {
            // SAFETY: the call filled `buf` with a chain of FILE_FULL_DIR_INFO records. `off` is 0 or a sum of
            // `NextEntryOffset`s, each landing on the next record inside the buffer, and each name is
            // `FileNameLength` bytes long.
            let (next, attributes, tag, name) = unsafe {
                let info = base.add(off).cast::<FILE_FULL_DIR_INFO>();
                let units = (*info).FileNameLength as usize / 2;
                let name = std::slice::from_raw_parts((&raw const (*info).FileName).cast::<u16>(), units);
                (
                    (*info).NextEntryOffset,
                    (*info).FileAttributes,
                    (*info).EaSize,
                    std::ffi::OsString::from_wide(name),
                )
            };
            if name != "." && name != ".." {
                out.push(flux_fs::DirEntry { name, file_type: entry_type(attributes, tag) });
            }
            if next == 0 {
                break;
            }
            off += next as usize;
        }
    }
    Ok(out)
}

/// An entry's type from what enumeration reports. On a reparse point `EaSize` carries the reparse TAG, not an EA size.
/// MEASURED (cut 7a Part 3a, NTFS and ReFS): a junction lists as `0x410` with `EaSize` `0xA0000003`, while a plain
/// entry's `EaSize` is a real size (`0xDC` for `..` on NTFS), so it is read as a tag only when the bit is set. A
/// name-surrogate is a link; any other reparse point on a directory is a directory, as `open_dir` treats it.
fn entry_type(attributes: u32, ea_size: u32) -> flux_fs::FileType {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && is_name_surrogate(ea_size) {
        flux_fs::FileType::Symlink
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        flux_fs::FileType::Dir
    } else {
        flux_fs::FileType::File
    }
}
```

- [ ] **Step 5: the fake.** In `impl DirHandle for FakeDirHandle`, after `lock_capability`, insert:

```rust

    fn read_dir(&self) -> Result<Vec<DirEntry>> {
        self.fs().read_dir(&self.my_path())
    }
```

- [ ] **Step 6:**
  - `cargo test -p flux-platform --test dir_control` passes 2 tests.
  - `cargo test -p flux-platform a_listed_entry_type` passes 1 test (Windows).
  - `cargo test -p flux-core a_fake_handle_lists` passes 1 test.
  - Non-vacuity: delete the `if bytes == b"." || bytes == b".."` check (Linux, in `just check-linux`) and the
    `if name != "." && name != ".."` guard (Windows). `read_dir_lists_files_directories_and_links_as_links` must fail
    at "an empty directory lists nothing". Revert both.
  - Then run `just check`, `just check-linux` and `just check-mac`.
- [ ] **Step 7:** commit the five files: `feat(fs): DirHandle::read_dir on POSIX, Windows and the fake (cut 7a Part
  3a)`.

---

### Task 2: `DirHandle::read_file`

**Files:** Modify `crates/flux-fs/src/fs.rs`, `crates/flux-platform/src/dir_unix.rs`,
`crates/flux-platform/src/dir_windows.rs`, `crates/flux-platform/src/lock_file.rs`, `crates/flux-core/src/fault_fs.rs`
and `crates/flux-platform/tests/dir_control.rs`.

- [ ] **Step 0:**
  - Task 1 is committed.
  - `lock_file.rs` `:148` reads `fn read_loop(`.
  - `dir_windows.rs` imports `use std::fs::File;` (`:14`).

- [ ] **Step 1: the tests.** Append to `dir_control.rs`:

```rust
#[test]
fn read_file_reads_at_most_one_byte_past_its_limit() {
    let (tmp, d) = dir();
    std::fs::write(tmp.path().join("s"), b"0123456789").unwrap();
    assert_eq!(d.read_file(OsStr::new("s"), 64).unwrap(), b"0123456789");
    assert_eq!(
        d.read_file(OsStr::new("s"), 4).unwrap(),
        b"01234",
        "limit + 1 bytes, so an oversized file shows"
    );
}

#[test]
fn read_file_refuses_a_missing_name_and_a_directory() {
    let (_tmp, d) = dir();
    let e = d.read_file(OsStr::new("absent"), 8).expect_err("nothing there");
    assert_eq!((e.code, e.source.kind()), (Code::IoError, ErrorKind::NotFound));
    drop(d.create_dir(OsStr::new("adir")).unwrap());
    let e = d.read_file(OsStr::new("adir"), 8).expect_err("a directory is not a state file");
    assert_eq!((e.code, e.source.kind()), (Code::DestinationError, ErrorKind::IsADirectory));
}

#[test]
fn read_file_never_follows_a_link() {
    let (tmp, d) = dir();
    #[cfg(unix)]
    {
        std::fs::write(tmp.path().join("real"), b"secret").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("real"), tmp.path().join("link")).unwrap();
        let e = d.read_file(OsStr::new("link"), 64).expect_err("never follow a link");
        assert_eq!(e.code, Code::SafetyRejected, "{e:?}");
    }
    #[cfg(windows)]
    {
        std::fs::create_dir(tmp.path().join("real")).unwrap();
        dir_link(&tmp.path().join("real"), &tmp.path().join("link"));
        let e = d.read_file(OsStr::new("link"), 64).expect_err("never follow a link");
        // FILE_NON_DIRECTORY_FILE meets the junction first; either refusal is a refusal (as `open_lock`'s junction
        // test says), and neither reads the target.
        assert!(matches!(e.code, Code::SafetyRejected | Code::DestinationError), "{e:?}");
    }
}

#[cfg(unix)]
#[test]
fn read_file_refuses_a_fifo_without_hanging() {
    let (tmp, d) = dir();
    let status = std::process::Command::new("mkfifo")
        .arg(tmp.path().join("fifo"))
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo");
    let e = d.read_file(OsStr::new("fifo"), 8).expect_err("a FIFO is not a state file");
    assert_eq!(e.code, Code::DestinationError);
}
```

  Append to `fault_fs.rs`'s `mod tests`:

```rust
    #[test]
    fn a_fake_handle_reads_a_file_and_refuses_what_the_real_ones_refuse() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        fs.write_file("/p/s", b"0123456789");
        assert_eq!(d.read_file(OsStr::new("s"), 4).unwrap(), b"01234");
        let e = d.read_file(OsStr::new("absent"), 4).unwrap_err();
        assert_eq!((e.code, e.source.kind()), (Code::IoError, std::io::ErrorKind::NotFound));
        drop(d.create_dir(OsStr::new("sub")).unwrap());
        let e = d.read_file(OsStr::new("sub"), 4).unwrap_err();
        assert_eq!((e.code, e.source.kind()), (Code::DestinationError, std::io::ErrorKind::IsADirectory));
        fs.write_file("/p/l", b"");
        fs.set_type("/p/l", FileType::Symlink);
        assert_eq!(d.read_file(OsStr::new("l"), 4).unwrap_err().code, Code::SafetyRejected);
        assert!(fs.called("read_file("), "{:?}", fs.calls());
    }
```

- [ ] **Step 2: the trait.** After Task 1's `fn read_dir(...)`, insert:

```rust

    /// Read a child REGULAR FILE: at most `limit + 1` bytes, so a caller can tell a file longer than `limit` (as
    /// `LockFile::read_all` does). Never follows a link.
    ///
    /// Refuses as `open_lock` does: a name-surrogate (`Code::SafetyRejected`); a directory (`Code::DestinationError`,
    /// kind `IsADirectory`); any other non-regular file where the platform can tell (`Code::DestinationError`). A
    /// missing name is `Code::IoError` with kind `NotFound`.
    fn read_file(&self, name: &std::ffi::OsStr, limit: usize) -> Result<Vec<u8>>;
```

- [ ] **Step 3: `read_loop`.** In `lock_file.rs`, change `fn read_loop(` to `pub(crate) fn read_loop(`.

- [ ] **Step 4: POSIX.** After Task 1's `read_dir` in `dir_unix.rs`, insert:

```rust

    fn read_file(&self, name: &OsStr, limit: usize) -> Result<Vec<u8>> {
        use rustix::fs::FileType;
        use std::os::unix::fs::FileExt;
        check_component(name)?;
        // `open_lock`'s flags, read-only: NOFOLLOW refuses a link, NONBLOCK keeps a FIFO from hanging the open, NOCTTY
        // keeps a terminal from becoming this process's.
        let fd = openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| match e {
            rustix::io::Errno::LOOP => FsError::new(Code::SafetyRejected, std::io::Error::from(e)),
            _ => FsError::from_io(std::io::Error::from(e)),
        })?;
        let st = rustix::fs::fstat(&fd).map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        match FileType::from_raw_mode(st.st_mode) {
            FileType::RegularFile => {}
            // A read-only open of a directory succeeds on POSIX, so it is refused here rather than by the kernel.
            FileType::Directory => {
                return Err(FsError::new(
                    Code::DestinationError,
                    std::io::Error::new(
                        std::io::ErrorKind::IsADirectory,
                        "a directory is not a state file",
                    ),
                ));
            }
            _ => {
                return Err(FsError::new(
                    Code::DestinationError,
                    std::io::Error::other("not a regular file"),
                ));
            }
        }
        let file = std::fs::File::from(fd);
        crate::lock_file::read_loop(limit, |buf, at| file.read_at(buf, at))
    }
```

- [ ] **Step 5: Windows.** In the impl, after Task 1's `read_dir`:

```rust

    fn read_file(&self, name: &OsStr, limit: usize) -> Result<Vec<u8>> {
        check_component(name)?;
        read_file_at(&self.0, name, limit)
    }
```

  After `fn entry_type`, insert:

```rust

/// A child regular file, read. Opened as `open_lock_at` opens one (`FILE_OPEN_REPARSE_POINT`, so a link is the object
/// opened and then refused), for reading only.
fn read_file_at(p: &OwnedHandle, n: &OsStr, limit: usize) -> Result<Vec<u8>> {
    use std::os::windows::fs::FileExt;
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
            FILE_GENERIC_READ | SYNCHRONIZE,
            &raw const oa,
            &raw mut iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status != 0 {
        // Answered first: `nt_io_error` maps FILE_IS_A_DIRECTORY to AlreadyExists, for `create_new_at`'s sake.
        if status == STATUS_FILE_IS_A_DIRECTORY {
            return Err(FsError::new(
                Code::DestinationError,
                std::io::Error::new(std::io::ErrorKind::IsADirectory, "a directory is not a state file"),
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
    if let Some(tag) = reparse_tag_of(&opened)?
        && is_name_surrogate(tag)
    {
        return Err(FsError::new(
            Code::SafetyRejected,
            std::io::Error::other(format!("name-surrogate reparse point, tag 0x{tag:08X}")),
        ));
    }
    let file = File::from(opened);
    crate::lock_file::read_loop(limit, |buf, at| file.seek_read(buf, at))
}
```

- [ ] **Step 6: the fake.** After Task 1's `read_dir` in `impl DirHandle for FakeDirHandle`:

```rust

    fn read_file(&self, name: &OsStr, limit: usize) -> Result<Vec<u8>> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().record(format!("read_file({})", child_path.display()), "read_file")?;
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
                    std::io::Error::other("not a regular file"),
                ));
            }
            _ => {}
        }
        if g.directories.contains(&child_path) {
            return Err(FsError::new(
                Code::DestinationError,
                std::io::Error::new(std::io::ErrorKind::IsADirectory, "a directory is not a state file"),
            ));
        }
        let Some(bytes) = g.files.get(&child_path) else {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        };
        let mut bytes = bytes.clone();
        bytes.truncate(limit + 1);
        Ok(bytes)
    }
```

- [ ] **Step 7:**
  - `cargo test -p flux-platform --test dir_control` passes 5 tests on Windows (6 in `just check-linux`).
  - `cargo test -p flux-core a_fake_handle_reads` passes.
  - Non-vacuity:
    - In `dir_unix.rs` (checked by `just check-linux`), replace the `FileType::Directory => { return Err(...) }` arm
      with `FileType::Directory => {}`. `read_file_refuses_a_missing_name_and_a_directory` must fail.
    - In the fake, change `bytes.truncate(limit + 1)` to `bytes.truncate(limit)`.
      `a_fake_handle_reads_a_file_and_refuses_what_the_real_ones_refuse` must fail.
    - Revert both.
  - Then run the three gates.
- [ ] **Step 8:** commit the six files: `feat(fs): DirHandle::read_file, refusing links, directories and special files
  (cut 7a Part 3a)`.

---

### Task 3: `DirHandle::remove_dir`

**Files:** Modify `crates/flux-fs/src/fs.rs`, `crates/flux-platform/src/dir_unix.rs`,
`crates/flux-platform/src/dir_windows.rs`, `crates/flux-core/src/fault_fs.rs` and
`crates/flux-platform/tests/dir_control.rs`.

- [ ] **Step 0:**
  - Task 2 is committed.
  - `dir_windows.rs` `:42` is `const STATUS_FILE_IS_A_DIRECTORY: i32 = 0xC000_00BAu32 as i32;`.
  - `remove_file_at` contains, from `    // POSIX delete first: the NAME goes at once` to its closing `}`, a POSIX-then-legacy
    disposition block that ends:

    ```
        if status != 0 {
            return Err(FsError::new(Code::IoError, nt_io_error("NtSetInformationFile", status)));
        }
        drop(h);
        Ok(())
    }
    ```

- [ ] **Step 1: the tests.** Append to `dir_control.rs`:

```rust
#[test]
fn remove_dir_removes_an_empty_directory_and_refuses_a_full_or_missing_one() {
    let (tmp, d) = dir();
    drop(d.create_dir(OsStr::new("empty")).unwrap());
    d.remove_dir(OsStr::new("empty")).expect("an empty directory");
    assert!(!tmp.path().join("empty").exists());
    let full = d.create_dir(OsStr::new("full")).unwrap();
    drop(full.create_new(OsStr::new("f")).unwrap());
    drop(full);
    let e = d.remove_dir(OsStr::new("full")).expect_err("not empty");
    assert_eq!(e.source.kind(), ErrorKind::DirectoryNotEmpty, "{e:?}");
    assert!(tmp.path().join("full").join("f").exists(), "nothing inside is touched");
    let e = d.remove_dir(OsStr::new("absent")).expect_err("nothing there");
    assert_eq!(e.source.kind(), ErrorKind::NotFound, "{e:?}");
}

#[test]
fn remove_dir_refuses_a_file_and_a_link_and_never_follows_one() {
    let (tmp, d) = dir();
    std::fs::write(tmp.path().join("file"), b"x").unwrap();
    let e = d.remove_dir(OsStr::new("file")).expect_err("a file is not a directory");
    assert_eq!(e.source.kind(), ErrorKind::NotADirectory, "{e:?}");
    assert!(tmp.path().join("file").exists());
    std::fs::create_dir(tmp.path().join("target")).unwrap();
    dir_link(&tmp.path().join("target"), &tmp.path().join("link"));
    let e = d.remove_dir(OsStr::new("link")).expect_err("a link is not removed as a directory");
    assert_eq!(e.source.kind(), ErrorKind::NotADirectory, "{e:?}");
    assert!(std::fs::symlink_metadata(tmp.path().join("link")).is_ok(), "the link stays");
    assert!(tmp.path().join("target").is_dir(), "and so does its target");
}
```

  Append to `fault_fs.rs`'s `mod tests`:

```rust
    #[test]
    fn a_fake_remove_dir_removes_only_an_empty_real_directory() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        let empty = d.create_dir(OsStr::new("empty")).unwrap();
        let before = fs.metadata(Path::new("/p/empty")).unwrap().identity;
        drop(empty);
        d.remove_dir(OsStr::new("empty")).unwrap();
        assert!(!fs.exists("/p/empty"));
        drop(d.create_dir(OsStr::new("empty")).unwrap());
        assert_ne!(
            fs.metadata(Path::new("/p/empty")).unwrap().identity,
            before,
            "a directory made again at the name is a new object"
        );
        drop(d.create_dir(OsStr::new("full")).unwrap());
        fs.write_file("/p/full/f", b"x");
        let e = d.remove_dir(OsStr::new("full")).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::DirectoryNotEmpty);
        fs.write_file("/p/file", b"x");
        let e = d.remove_dir(OsStr::new("file")).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::NotADirectory);
        drop(d.create_dir(OsStr::new("link")).unwrap());
        fs.set_type("/p/link", FileType::Symlink);
        let e = d.remove_dir(OsStr::new("link")).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::NotADirectory);
        assert!(fs.exists("/p/link"));
        let e = d.remove_dir(OsStr::new("absent")).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::NotFound);
    }
```

- [ ] **Step 2: the trait.** After Task 2's `fn read_file(...)`, insert:

```rust

    /// Remove an EMPTY child directory (`rmdir`).
    ///
    /// Refuses, with these kinds (the engine branches on them): a name that is not a directory - a file, or a link of
    /// any kind, which is never followed and never removed here - `NotADirectory`; a directory that is not empty,
    /// `DirectoryNotEmpty`; a missing name, `NotFound`.
    fn remove_dir(&self, name: &std::ffi::OsStr) -> Result<()>;
```

- [ ] **Step 3: POSIX.** After `read_file` in `dir_unix.rs`:

```rust

    fn remove_dir(&self, name: &OsStr) -> Result<()> {
        check_component(name)?;
        // `AT_REMOVEDIR` refuses a symlink with ENOTDIR even when it points at a directory: a link is never followed.
        rustix::fs::unlinkat(&self.0, name, AtFlags::REMOVEDIR)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }
```

- [ ] **Step 4: Windows.**
  - After `const STATUS_FILE_IS_A_DIRECTORY: ...;` insert
    `const STATUS_DIRECTORY_NOT_EMPTY: i32 = 0xC000_0101u32 as i32;`.
  - In the impl, after `read_file`:

```rust

    fn remove_dir(&self, name: &OsStr) -> Result<()> {
        check_component(name)?;
        remove_dir_at(&self.0, name)
    }
```

  - In `remove_file_at`, replace everything from the line `    // POSIX delete first: the NAME goes at once, as
    `unlinkat` does and as the test fake models, even while other` through the function's closing `}` with:

```rust
    let status = delete_open(&h);
    if status != 0 {
        return Err(FsError::new(Code::IoError, nt_io_error("NtSetInformationFile", status)));
    }
    drop(h);
    Ok(())
}

/// Mark the object `h` holds deleted; the final `NTSTATUS`, 0 on success. Shared by `remove_file_at` and
/// `remove_dir_at`.
///
/// POSIX delete first: the NAME goes at once, as `unlinkat` does and as the test fake models, even while other handles
/// hold the file open - the lock's release unlinks and THEN closes (spec S99_release). Legacy semantics leave the name
/// "delete pending" until the last handle closes, and every create or open of it meanwhile fails with
/// STATUS_DELETE_PENDING (measured on NTFS, cut 7a Part 1 capstone). A volume or system without POSIX delete (FAT,
/// exFAT, some redirectors, Windows before 10 1709) refuses the class or the flag; only then fall back to the legacy
/// call, and `nt_io_error` / the create paths map the pending state for whoever meets it.
fn delete_open(h: &OwnedHandle) -> i32 {
    let mut posix = FILE_DISPOSITION_INFORMATION_EX {
        Flags: FILE_DISPOSITION_DELETE | FILE_DISPOSITION_POSIX_SEMANTICS,
    };
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `posix` is a live FILE_DISPOSITION_INFORMATION_EX of the size given, and the handle outlives the call.
    let status = unsafe {
        NtSetInformationFile(
            h.as_raw_handle() as _,
            &raw mut iosb,
            (&raw mut posix).cast(),
            size_of::<FILE_DISPOSITION_INFORMATION_EX>() as u32,
            FileDispositionInformationEx,
        )
    };
    // Success, or a failure the legacy call would not cure.
    if !matches!(
        status,
        STATUS_INVALID_PARAMETER
            | STATUS_NOT_SUPPORTED
            | STATUS_INVALID_INFO_CLASS
            | STATUS_INVALID_DEVICE_REQUEST
    ) {
        return status;
    }
    let mut info = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a live FILE_DISPOSITION_INFORMATION of the size given, and the handle outlives the call.
    unsafe {
        NtSetInformationFile(
            h.as_raw_handle() as _,
            &raw mut iosb,
            (&raw mut info).cast(),
            size_of::<FILE_DISPOSITION_INFORMATION>() as u32,
            FileDispositionInformation,
        )
    }
}

/// `rmdir`, relative to `p`. Opened for DELETE as a DIRECTORY with `FILE_OPEN_REPARSE_POINT`, so a junction or directory
/// symlink is the object opened - and then refused, as `unlinkat(AT_REMOVEDIR)` refuses a symlink. A file is refused
/// by the open itself. A non-surrogate reparse point (a cloud placeholder) is a real directory and is removed.
fn remove_dir_at(p: &OwnedHandle, n: &OsStr) -> Result<()> {
    let not_a_directory = || {
        FsError::new(
            Code::IoError,
            std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "remove_dir refuses what is not a directory",
            ),
        )
    };
    let mut wide: Vec<u16> = n.encode_wide().collect();
    let bytes = (wide.len() * 2) as u16;
    let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
    let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
    oa.RootDirectory = p.as_raw_handle() as HANDLE;
    oa.ObjectName = &raw const us;
    let mut raw: HANDLE = std::ptr::null_mut();
    let mut open_iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is to a live local that outlives the call, and `wide` outlives `us`. FILE_READ_ATTRIBUTES
    // because the reparse check below queries this handle.
    let status = unsafe {
        NtCreateFile(
            &raw mut raw,
            DELETE | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            &raw const oa,
            &raw mut open_iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if status != 0 {
        if status == STATUS_NOT_A_DIRECTORY {
            return Err(not_a_directory());
        }
        let code = match status {
            STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => Code::IoError,
            STATUS_ACCESS_DENIED => Code::PermissionDenied,
            _ => Code::IoError,
        };
        return Err(FsError::new(code, nt_io_error("NtCreateFile", status)));
    }
    // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `raw` is a valid handle we own.
    let h = unsafe { OwnedHandle::from_raw_handle(raw as _) };
    if let Some(tag) = reparse_tag_of(&h)?
        && is_name_surrogate(tag)
    {
        return Err(not_a_directory());
    }
    let status = delete_open(&h);
    if status == STATUS_DIRECTORY_NOT_EMPTY {
        return Err(FsError::new(
            Code::IoError,
            std::io::Error::new(
                std::io::ErrorKind::DirectoryNotEmpty,
                format!("NtSetInformationFile: 0x{:08X}", status as u32),
            ),
        ));
    }
    if status != 0 {
        return Err(FsError::new(Code::IoError, nt_io_error("NtSetInformationFile", status)));
    }
    drop(h);
    Ok(())
}
```

- [ ] **Step 5: the fake.** After Task 2's `read_file` in `impl DirHandle for FakeDirHandle`:

```rust

    fn remove_dir(&self, name: &OsStr) -> Result<()> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().record(format!("remove_dir({})", child_path.display()), "remove_dir")?;
        let mut g = self.inner.lock().unwrap();
        // A link (of any kind) or a file: as `unlinkat(AT_REMOVEDIR)` answers, never followed.
        if g.types.get(&child_path) == Some(&FileType::Symlink) || g.files.contains_key(&child_path) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::new(
                    std::io::ErrorKind::NotADirectory,
                    "remove_dir refuses what is not a directory",
                ),
            ));
        }
        if !g.directories.contains(&child_path) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        let occupied = g
            .files
            .keys()
            .chain(g.directories.iter())
            .any(|k| k.parent() == Some(child_path.as_path()));
        if occupied {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::DirectoryNotEmpty),
            ));
        }
        g.directories.remove(&child_path);
        g.types.remove(&child_path);
        g.identities.remove(&child_path);
        g.times.remove(&child_path);
        g.perms.remove(&child_path);
        // The name no longer binds a node: a directory made there later is a new one.
        g.dir_node_by_path.remove(&child_path);
        if let Some(node) = g.dir_nodes.get_mut(&self.id) {
            node.children.remove(name);
        }
        Ok(())
    }
```

- [ ] **Step 6:**
  - `cargo test -p flux-platform --test dir_control` passes (7 on Windows).
  - `cargo test -p flux-core a_fake_remove_dir` passes.
  - The existing `remove_file` tests (`cargo test -p flux-platform --test dir_handle`) still pass: the factoring kept
    the behaviour.
  - Non-vacuity:
    - Windows: delete the `if let Some(tag) = reparse_tag_of(&h)? && is_name_surrogate(tag) { ... }` block from
      `remove_dir_at`. `remove_dir_refuses_a_file_and_a_link_and_never_follows_one` must fail at "a link is not
      removed as a directory".
    - Linux (`just check-linux`): replace `AtFlags::REMOVEDIR` with `AtFlags::empty()`.
      `remove_dir_removes_an_empty_directory...` must fail.
    - Revert both.
  - Then run the three gates.
- [ ] **Step 7:** commit the five files: `feat(fs): DirHandle::remove_dir, refusing files, links and full directories
  (cut 7a Part 3a)`.

---

### Task 4: `DirHandle::sync`

**Files:** Modify `crates/flux-fs/src/fs.rs`, `crates/flux-platform/src/dir_unix.rs`,
`crates/flux-platform/src/dir_windows.rs`, `crates/flux-core/src/fault_fs.rs` and
`crates/flux-platform/tests/dir_control.rs`.

- [ ] **Step 0:** Task 3 is committed. `const FILE_ADD_FILE: u32 = 0x2;` exists in `dir_windows.rs`, possibly with
  Task 1's `#[allow(dead_code)]` line, which this task removes.

- [ ] **Step 1: the tests.** Append to `dir_control.rs`:

```rust
#[test]
fn sync_flushes_every_kind_of_directory_handle() {
    let (_tmp, d) = dir();
    d.sync().expect("the destination root");
    let sub = d.create_dir(OsStr::new("sub")).unwrap();
    sub.sync().expect("a created directory");
    d.open_dir(OsStr::new("sub")).unwrap().sync().expect("an opened directory");
}
```

  Append to `fault_fs.rs`'s `mod tests`:

```rust
    #[test]
    fn a_fake_sync_is_recorded_and_can_fail() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        d.sync().unwrap();
        assert!(fs.called("sync_dir("), "{:?}", fs.calls());
        fs.fail("sync_dir", Code::IoError);
        assert!(d.sync().is_err());
    }
```

- [ ] **Step 2: the trait.** After Task 3's `fn remove_dir(...)`, insert:

```rust

    /// Flush this directory's entries to stable storage, so that a create, rename or removal inside it survives a
    /// crash (the crash-safe state write: temporary, flush, rename, flush the directory).
    fn sync(&self) -> Result<()>;
```

- [ ] **Step 3: POSIX.** After `remove_dir` in `dir_unix.rs`:

```rust

    fn sync(&self) -> Result<()> {
        rustix::fs::fsync(&self.0).map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }
```

- [ ] **Step 4: Windows.** Remove Task 1's `#[allow(dead_code)]` line above `FILE_ADD_FILE`, if one was added. In the
  impl, after `remove_dir`:

```rust

    fn sync(&self) -> Result<()> {
        sync_at(&self.0)
    }
```

  After `fn remove_dir_at`, insert:

```rust

/// Flush the directory `p` holds. MEASURED (cut 7a Part 3a, NTFS and ReFS): `FlushFileBuffers` on a directory needs
/// write access - `FILE_ADD_FILE` suffices, and the list-only handle a `StdDir` holds answers ACCESS_DENIED - so the
/// flush goes through a reopened handle.
fn sync_at(p: &OwnedHandle) -> Result<()> {
    use windows_sys::Win32::Storage::FileSystem::FlushFileBuffers;
    let h = reopen(p, FILE_ADD_FILE)?;
    // SAFETY: the handle is live for the call.
    if unsafe { FlushFileBuffers(h.as_raw_handle() as _) } == 0 {
        return Err(FsError::from_io(std::io::Error::last_os_error()));
    }
    Ok(())
}
```

- [ ] **Step 5: the fake.** After Task 3's `remove_dir`:

```rust

    fn sync(&self) -> Result<()> {
        self.fs().record(format!("sync_dir({})", self.my_path().display()), "sync_dir")
    }
```

- [ ] **Step 6:**
  - `cargo test -p flux-platform --test dir_control` passes (8 on Windows).
  - `cargo test -p flux-core a_fake_sync` passes.
  - Non-vacuity (Windows): in `sync_at`, change `reopen(p, FILE_ADD_FILE)` to `reopen(p, FILE_LIST_DIRECTORY)`.
    `sync_flushes_every_kind_of_directory_handle` must fail with ACCESS_DENIED. Revert it.
  - Then run the three gates.
- [ ] **Step 7:** commit the five files: `feat(fs): DirHandle::sync flushes a directory (cut 7a Part 3a)`.

---

### Task 5: directory renames

The real arms already rename a directory. This task pins that with tests, and teaches the fake to do it.

**Files:** Modify `crates/flux-core/src/fault_fs.rs` and `crates/flux-platform/tests/dir_control.rs`.

- [ ] **Step 0:**
  - Task 4 is committed.
  - In `fault_fs.rs`:
    - `missing_source` reads `if g.files.contains_key(from) {`;
    - `move_object` begins `let bytes = g.files.remove(from).unwrap_or_default();`;
    - `FaultFs::rename_no_replace` tests occupancy with `if g.files.contains_key(&t) {`;
    - `FaultFs::rename_replace` calls `missing_source(&g, &f)?;` then `move_object(&mut g, &f, &t);`;
    - `DirNode`'s doc comment contains `` `path` is a snapshot taken ONCE, when the node is minted, and used only to``.

- [ ] **Step 1: the tests.** Append to `dir_control.rs`:

```rust
#[test]
fn a_directory_is_renamed_without_replacing_and_keeps_its_contents() {
    let (tmp, d) = dir();
    let building = d.create_dir(OsStr::new("w.creating")).unwrap();
    drop(building.create_new(OsStr::new("manifest")).unwrap());
    drop(building);
    d.rename_no_replace(OsStr::new("w.creating"), &d, OsStr::new("w")).expect("rename a directory");
    assert!(tmp.path().join("w").join("manifest").is_file());
    assert!(!tmp.path().join("w.creating").exists());
    drop(d.create_dir(OsStr::new("x")).unwrap());
    let e = d
        .rename_no_replace(OsStr::new("w"), &d, OsStr::new("x"))
        .expect_err("never replaces a directory");
    assert_eq!(e.source.kind(), ErrorKind::AlreadyExists, "{e:?}");
    assert!(tmp.path().join("w").join("manifest").is_file(), "the source stays");
}
```

  Append to `fault_fs.rs`'s `mod tests`:

```rust
    #[test]
    fn a_fake_directory_rename_moves_the_subtree_and_open_handles_follow() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        let building = d.create_dir(OsStr::new("w.creating")).unwrap();
        fs.write_file("/p/w.creating/manifest", b"m");
        let dir_id = fs.metadata(Path::new("/p/w.creating")).unwrap().identity;
        let file_id = fs.metadata(Path::new("/p/w.creating/manifest")).unwrap().identity;
        d.rename_no_replace(OsStr::new("w.creating"), &d, OsStr::new("w")).unwrap();
        assert!(!fs.exists("/p/w.creating") && !fs.exists("/p/w.creating/manifest"));
        assert_eq!(fs.read_file("/p/w/manifest").as_deref(), Some(&b"m"[..]));
        assert_eq!(fs.metadata(Path::new("/p/w")).unwrap().identity, dir_id, "the same object");
        assert_eq!(fs.metadata(Path::new("/p/w/manifest")).unwrap().identity, file_id);
        assert_eq!(
            building.read_file(OsStr::new("manifest"), 8).unwrap(),
            b"m",
            "a handle opened before the rename follows the directory"
        );
        let opened = d.open_dir(OsStr::new("w")).unwrap();
        assert_eq!(opened.read_file(OsStr::new("manifest"), 8).unwrap(), b"m");
        assert_eq!(d.open_dir(OsStr::new("w.creating")).unwrap_err().code, Code::DestinationError);
        drop(d.create_dir(OsStr::new("x")).unwrap());
        let e = d.rename_no_replace(OsStr::new("w"), &d, OsStr::new("x")).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::AlreadyExists);
    }
```

- [ ] **Step 2: the fake.**
  - In `missing_source`, change `if g.files.contains_key(from) {` to
    `if g.files.contains_key(from) || g.directories.contains(from) {`.
  - In `FaultFs::rename_no_replace`, change `if g.files.contains_key(&t) {` to
    `if g.files.contains_key(&t) || g.directories.contains(&t) {`.
  - In `FaultFs::rename_replace`, between `missing_source(&g, &f)?;` and `move_object(&mut g, &f, &t);`, insert:

```rust
        assert!(
            !g.directories.contains(&f),
            "the fake moves a directory only through rename_no_replace (cut 7a Part 3a, decision 10)"
        );
```

  - In `move_object`, insert as its first statements:

```rust
    if g.directories.contains(from) {
        move_directory(g, from, to);
        return;
    }
```

  - After `move_object`'s closing `}`, insert:

```rust

/// A directory rename moves its whole subtree, and every handle open on it or below it follows the OBJECT, as on a
/// real filesystem (cut 7a Part 3a: a workspace is built under one name and renamed to another). The name binding moves
/// from the old parent's node to the new parent's.
fn move_directory(g: &mut Inner, from: &Path, to: &Path) {
    let moved = |p: &Path| -> Option<PathBuf> {
        let rest = p.strip_prefix(from).ok()?;
        Some(if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) })
    };
    fn rekey<V>(map: &mut HashMap<PathBuf, V>, moved: &dyn Fn(&Path) -> Option<PathBuf>) {
        let keys: Vec<PathBuf> = map.keys().filter(|k| moved(k).is_some()).cloned().collect();
        for key in keys {
            let value = map.remove(&key).expect("listed just above");
            map.insert(moved(&key).expect("filtered just above"), value);
        }
    }
    rekey(&mut g.files, &moved);
    rekey(&mut g.times, &moved);
    rekey(&mut g.perms, &moved);
    rekey(&mut g.identities, &moved);
    rekey(&mut g.types, &moved);
    rekey(&mut g.dir_node_by_path, &moved);
    let dirs: Vec<PathBuf> = g.directories.iter().filter(|d| moved(d).is_some()).cloned().collect();
    for dir in dirs {
        g.directories.remove(&dir);
        g.directories.insert(moved(&dir).expect("filtered just above"));
    }
    for node in g.dir_nodes.values_mut() {
        if let Some(p) = moved(&node.path) {
            node.path = p;
        }
    }
    let node = g.dir_node_by_path.get(to).copied();
    if let (Some(parent), Some(name)) = (from.parent(), from.file_name())
        && let Some(pid) = g.dir_node_by_path.get(parent).copied()
    {
        g.dir_nodes.get_mut(&pid).expect("a bound id names a node").children.remove(name);
    }
    if let (Some(parent), Some(name), Some(id)) = (to.parent(), to.file_name(), node)
        && let Some(pid) = g.dir_node_by_path.get(parent).copied()
    {
        g.dir_nodes
            .get_mut(&pid)
            .expect("a bound id names a node")
            .children
            .insert(name.to_os_string(), id);
    }
}
```

  - In `DirNode`'s doc comment, replace
    `` `path` is a snapshot taken ONCE, when the node is minted, and used only to`` with
    `` `path` is a snapshot taken when the node is minted - moved only when a directory rename moves that directory or an ancestor, since the object moved (cut 7a Part 3a) - and used only to``.

- [ ] **Step 3:**
  - `cargo test -p flux-platform --test dir_control` passes (9 on Windows).
  - `cargo test -p flux-core fault_fs` passes, including every earlier fake test.
  - Non-vacuity: remove the `if g.directories.contains(from) { move_directory(...); return; }` guard from
    `move_object`. `a_fake_directory_rename_moves_the_subtree_and_open_handles_follow` must fail. Revert it.
  - Then run the three gates.
- [ ] **Step 4:** commit the two files: `test(fs): a directory renames without replacing; the fake moves directories
  (cut 7a Part 3a)`.

---

### Task 6: the fake's legacy-delete window

**Files:** Modify `crates/flux-core/src/fault_fs.rs` and `crates/flux-core/src/lock/obtain.rs`.

- [ ] **Step 0:**
  - Task 5 is committed.
  - `struct Inner`'s last field is `hooks`, with `#[allow(clippy::type_complexity)]` above it.
  - `FaultFs::remove_file` reads, after its `record(...)?;`:

    ```
            let mut g = self.inner.lock().unwrap();
            if g.files.remove(&p).is_none() {
    ```
  - `FaultFs::metadata`, after `self.record(format!("metadata({})", p.display()), "metadata")?;`, has
    `let mut g = self.inner.lock().unwrap();`.
  - `FakeDirHandle::lock_for` contains `g.next_lock_handle += 1;`.
  - `FakeDirHandle::open_lock`'s locked block begins `match g.types.get(&child_path) {`.
  - `impl Drop for FakeLock` is:

    ```
        fn drop(&mut self) {
            if let Ok(mut g) = self.inner.lock()
                && g.lock_holders.get(&self.object) == Some(&self.handle)
            {
                g.lock_holders.remove(&self.object);
            }
        }
    ```

- [ ] **Step 1: the tests.** Append to `fault_fs.rs`'s `mod tests`:

```rust
    #[test]
    fn legacy_delete_keeps_a_name_occupied_until_the_last_lock_handle_closes() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        fs.set_legacy_delete(true);
        let name = OsStr::new("x.flux-lock");
        let held = d.create_lock(name).unwrap();
        let other = d.open_lock(name).unwrap();
        d.remove_file(name).unwrap();
        drop(held);
        let gone = |e: FsError| e.source.kind() == std::io::ErrorKind::NotFound;
        assert!(gone(d.metadata(name).unwrap_err()), "a stat finds nothing");
        assert!(gone(d.open_lock(name).unwrap_err()), "an open finds nothing");
        assert!(gone(d.read_file(name, 8).unwrap_err()), "a read finds nothing");
        assert_eq!(
            d.create_lock(name).unwrap_err().source.kind(),
            std::io::ErrorKind::AlreadyExists,
            "a create finds the name occupied"
        );
        drop(other);
        assert!(!fs.exists("/p/x.flux-lock"), "the last close deletes it");
        drop(d.create_lock(name).unwrap());
    }

    #[test]
    fn without_legacy_delete_a_removal_is_immediate_even_while_a_handle_is_open() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        let name = OsStr::new("x.flux-lock");
        let held = d.create_lock(name).unwrap();
        d.remove_file(name).unwrap();
        assert!(!fs.exists("/p/x.flux-lock"));
        drop(d.create_lock(name).unwrap());
        drop(held);
    }
```

  Append to `obtain.rs`'s `mod tests`:

```rust
    #[test]
    fn a_released_lock_still_open_elsewhere_under_legacy_delete_is_busy_until_it_closes() {
        let (fs, d) = fake();
        fs.set_legacy_delete(true);
        let site = site(&d);
        let Obtained::Held { held: mut theirs, .. } =
            obtain(&site, STRONG, Mode::Plain, &me()).unwrap()
        else {
            panic!("expected Held")
        };
        theirs.write_record(record(&site, &me(), "none")).unwrap();
        // Another run's classification handle, still open when the holder releases.
        let classifier = d.open_lock(OsStr::new(NAME)).unwrap();
        assert_eq!(theirs.release().unwrap(), crate::lock::Released::Unlinked);
        let r = refusal(obtain(&site, STRONG, Mode::Plain, &me()));
        assert_eq!(r.code, LockCode::TargetLockBusy, "{}", r.detail);
        drop(classifier);
        assert!(matches!(
            obtain(&site, STRONG, Mode::Plain, &me()).unwrap(),
            Obtained::Held { .. }
        ));
    }
```

- [ ] **Step 2: the knob.**
  - In `struct Inner`, after the `hooks` field, add:

```rust
    /// Windows' legacy delete (cut 7a Part 1 capstone; `set_legacy_delete`).
    legacy_delete: bool,
    /// Object index -> live `FakeLock` handles on it.
    open_locks: HashMap<u128, u32>,
    /// Paths removed while a lock handle was open under legacy delete: still occupying their name, found by nothing.
    pending: HashSet<PathBuf>,
```

  - In `impl FaultFs`, after `on_nth`, add:

```rust
    /// Windows' legacy delete semantics (a volume without POSIX delete; cut 7a Part 1 capstone). A file removed while
    /// a lock handle on it is open stays at its name, "delete pending", until the last such handle closes. Meanwhile a
    /// create at the name finds it occupied, and a stat, an open or a read finds nothing.
    pub fn set_legacy_delete(&self, on: bool) {
        self.inner.lock().unwrap().legacy_delete = on;
    }
```

  - After `fn mint_identity`, add:

```rust

/// Forget a removed file: its bytes and everything keyed by its path, so a later object at the path starts fresh.
fn purge(g: &mut Inner, path: &Path) {
    g.files.remove(path);
    g.identities.remove(path);
    g.times.remove(path);
    g.perms.remove(path);
    g.types.remove(path);
}
```

  - In `FaultFs::remove_file`, replace everything after `let mut g = self.inner.lock().unwrap();` up to the
    function's final `Ok(())` (the `if g.files.remove(&p).is_none() { ... }` block and the removals of `identities`,
    `times`, `perms` and `types`, with their comments) with:

```rust
        if g.pending.contains(&p) || !g.files.contains_key(&p) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        let object = match g.identities.get(&p) {
            Some(flux_fs::FileIdentity::Strong(o)) => Some(o.index),
            _ => None,
        };
        if g.legacy_delete && object.is_some_and(|o| g.open_locks.get(&o).is_some_and(|&n| n > 0)) {
            g.pending.insert(p);
            return Ok(());
        }
        // The object is gone, so its state goes with it: a later file at the SAME path must not inherit a dead
        // object's identity, permissions or type (each MEASURED as a leak before it was removed here).
        purge(&mut g, &p);
```

  - In `FaultFs::metadata`, directly after `let mut g = self.inner.lock().unwrap();`, insert:

```rust
        if g.pending.contains(&p) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
```

  - In `FakeDirHandle::open_lock`'s locked block, and in `FakeDirHandle::read_file` directly after
    `let g = self.inner.lock().unwrap();`, insert first:

```rust
            if g.pending.contains(&child_path) {
                return Err(FsError::new(
                    Code::IoError,
                    std::io::Error::from(std::io::ErrorKind::NotFound),
                ));
            }
```

  - In `FakeDirHandle::lock_for`, after `g.next_lock_handle += 1;`, add
    `*g.open_locks.entry(object).or_insert(0) += 1;`.
  - Replace `impl Drop for FakeLock`'s `fn drop` with:

```rust
    fn drop(&mut self) {
        let Ok(mut g) = self.inner.lock() else { return };
        if g.lock_holders.get(&self.object) == Some(&self.handle) {
            g.lock_holders.remove(&self.object);
        }
        let left = match g.open_locks.get_mut(&self.object) {
            Some(n) => {
                *n -= 1;
                *n
            }
            None => 0,
        };
        if left == 0 {
            g.open_locks.remove(&self.object);
            // The last handle closed: a delete-pending name goes now.
            if let Ok(path) = FakeLock::path(&g, self.object)
                && g.pending.remove(&path)
            {
                purge(&mut g, &path);
            }
        }
    }
```

- [ ] **Step 3:**
  - `cargo test -p flux-core legacy_delete` passes 1 test; `cargo test -p flux-core without_legacy_delete` passes 1;
    `cargo test -p flux-core a_released_lock_still_open` passes 1.
  - The whole `flux-core` suite passes.
  - Non-vacuity: in `FaultFs::remove_file`, change `if g.legacy_delete && ...` to `if false && ...`.
    `legacy_delete_keeps_a_name_occupied...` and
    `a_released_lock_still_open_elsewhere_under_legacy_delete_is_busy_until_it_closes` must both fail. Revert it.
  - Then run `just check`.
- [ ] **Step 4:** commit the two files: `test(core): the fake models Windows' legacy delete; obtain is BUSY in its
  window (cut 7a Part 3a)`.

---

### Task 7: the state codes and the state codec

**Files:**
- Modify `crates/flux-core/Cargo.toml`, `crates/flux-core/src/lib.rs` and `crates/flux-core/src/lock/error.rs`.
- Create `crates/flux-core/src/state.rs`.

- [ ] **Step 0:**
  - Task 6 is committed.
  - `crates/flux-core/Cargo.toml`'s `[dependencies]` block is exactly:

    ```
    flux-fs = { path = "../flux-fs" }
    uuid = { workspace = true }
    blake3 = { workspace = true }
    ```
  - `crates/flux-core/src/lib.rs` has `pub mod lock;` followed by `pub mod tree;`.
  - `lock/error.rs`'s `LockCode` has exactly the six variants `TargetLockBusy`, `TargetLockUncertain`,
    `ControlPlaneNamespaceConflict`, `ArtifactOwnershipUncertain`, `PathComponentInvalid`, `RemoteLockUnsafe`. Its doc
    comment is `/// The spec's error codes the lock protocol produces ("Errors and exit statuses"). The CLI maps them to
    exits.`.

- [ ] **Step 1: dependencies and module.**
  - Append to `[dependencies]`: `serde = { workspace = true }` and `serde_json = { workspace = true }`.
  - In `lib.rs`, after `pub mod lock;`, insert `pub mod state;`.

- [ ] **Step 2: the codes.** In `lock/error.rs`:
  - Replace the `LockCode` doc comment with `/// The spec's error codes the lock protocol and the prior-state scan
    produce ("Errors and exit statuses"). The CLI maps them to exits.`.
  - After `RemoteLockUnsafe,` add:

```rust
    StateCorrupt,
    IncompatibleState,
    ResumableOperationExists,
```

  - In `as_str`, after the `RemoteLockUnsafe` arm, add:

```rust
            Self::StateCorrupt => "STATE_CORRUPT",
            Self::IncompatibleState => "INCOMPATIBLE_STATE",
            Self::ResumableOperationExists => "RESUMABLE_OPERATION_EXISTS",
```

  - In the test `every_code_has_the_spec_string`, after its last assertion, add:

```rust
        assert_eq!(LockCode::StateCorrupt.as_str(), "STATE_CORRUPT");
        assert_eq!(LockCode::IncompatibleState.as_str(), "INCOMPATIBLE_STATE");
        assert_eq!(LockCode::ResumableOperationExists.as_str(), "RESUMABLE_OPERATION_EXISTS");
```

- [ ] **Step 3: the codec.** Create `crates/flux-core/src/state.rs`:

```rust
//! The operation state (cut 7a spec, "Operation state, `format_version` 1"): one JSON object - a tree operation's
//! `manifest`, or a single-file operation's adjacent record - its codec, its crash-safe write, and the names Flux gives
//! it. `format_version` is judged before any other key, and nothing is parsed heuristically (§193).

use crate::ids::is_id;
use serde::{Deserialize, Serialize};
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
    pub fn created(operation_id: &str, kind: Kind, destination_root: &Path, created_at: u64) -> Self {
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
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| Unusable::Corrupt(format!("not JSON: {e}")))?;
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
    let state: OperationState = serde_json::from_value(value)
        .map_err(|e| Unusable::Corrupt(format!("malformed: {e}")))?;
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
            matches!(decode(v.to_string().as_bytes()), Err(Unusable::Corrupt(w)) if w.contains("extra"))
        );
        let mut v: serde_json::Value = serde_json::from_slice(&created().encode()).unwrap();
        v.as_object_mut().unwrap().remove("superseded_by");
        assert!(
            matches!(decode(v.to_string().as_bytes()), Err(Unusable::Corrupt(w)) if w.contains("superseded_by"))
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
        for (what, bytes) in [("not JSON", &b"{"[..]), ("not an object", &b"[1]"[..]), ("empty", &b""[..])] {
            assert!(matches!(decode(bytes), Err(Unusable::Corrupt(_))), "{what}");
        }
        assert!(matches!(decode(&vec![b' '; STATE_LIMIT + 1]), Err(Unusable::Corrupt(_))), "oversized");
    }

    #[test]
    fn an_unreadable_takeover_round_trips() {
        let t = Takeover::of_unreadable(9);
        assert_eq!(
            (t.operation_id.as_str(), t.last_heartbeat_wall_time.as_str(), t.creation_wall_time.as_str()),
            (UNREADABLE, UNREADABLE, "9")
        );
        let s = OperationState { takeover: Some(t), superseded_by: Some(id(2)), ..created() };
        assert_eq!(decode(&s.encode()), Ok(s));
    }
}
```

- [ ] **Step 4:**
  - `cargo test -p flux-core state::` passes 6 tests.
  - `cargo test -p flux-core every_code_has` passes.
  - Non-vacuity:
    - Move the `format_version` block in `decode` to after the two key checks.
      `format_version_is_judged_before_any_other_key` must fail. Revert it.
    - Delete the `if !is_id(&s.operation_id) { ... }` check. `malformed_values_are_corrupt` must fail. Revert it.
  - Then run `just check`.
- [ ] **Step 5:** commit the four files: `feat(core): the version-1 operation state codec and the three state refusal
  codes (cut 7a Part 3a)`.

---

### Task 8: names, the crash-safe write, and the workspace

**Files:**
- Modify `crates/flux-core/src/state.rs` and `crates/flux-core/src/lock/site.rs`.
- Create `crates/flux-core/tests/state_std_fs.rs`.

- [ ] **Step 0:** Task 7 is committed. `site.rs` has the two `fn name_len(name: &OsStr) -> usize {`, one
  `#[cfg(windows)]` and one `#[cfg(not(windows))]`.

- [ ] **Step 1: `name_len`.** In `site.rs`, make both `fn name_len` `pub(crate) fn name_len`.

- [ ] **Step 2: the I/O.** In `state.rs`:
  - Replace the imports with:

```rust
use crate::ids::is_id;
use crate::lock::error::refuse;
use crate::lock::site::{NAME_LIMIT, name_len};
use crate::lock::{LockCode, LockError, LockResult};
use flux_fs::{Code, DirHandle, FileHandle, FsError};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Write};
use std::path::Path;
```

  - After `fn wall_time_ns`, before `#[cfg(test)]`, insert:

```rust

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
/// The single-file state record is `<target>.flux-state.<id>` (F2).
pub const RECORD_INFIX: &str = ".flux-state.";

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
pub fn write_state<D: DirHandle>(dir: &D, name: &OsStr, state: &OperationState) -> flux_fs::Result<()> {
    let temp = temp_name(name);
    let mut file = dir.create_new(&temp)?;
    let written = file.write_all(&state.encode()).map_err(FsError::from_io).and_then(|()| file.sync_all());
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
pub fn create_workspace<D: DirHandle>(operations: &D, state: &OperationState) -> flux_fs::Result<D> {
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
```

- [ ] **Step 3: the tests.** Append inside `state.rs`'s `mod tests`, after its last test:

```rust
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
        calls.iter().position(|c| c.starts_with(prefix)).unwrap_or_else(|| panic!("no {prefix} in {calls:?}"))
    }

    #[test]
    fn a_state_is_written_through_a_flushed_temporary_then_renamed_and_the_directory_flushed() {
        let (fs, d) = dest();
        write_state(&d, OsStr::new("rec"), &created()).unwrap();
        assert_eq!(read_state(&d, OsStr::new("rec")).unwrap(), Ok(created()));
        assert!(!fs.exists("/p/dest/rec.tmp"));
        let calls = fs.calls();
        let order = ["create_new(", "sync_all(", "rename_replace(", "sync_dir("].map(|p| position(&calls, p));
        assert!(order.is_sorted(), "{calls:?}");
    }

    #[test]
    fn a_failed_write_leaves_the_old_state_and_no_temporary() {
        let (fs, d) = dest();
        write_state(&d, OsStr::new("rec"), &created()).unwrap();
        let newer = OperationState { state: OpState::Transferring, ..created() };
        fs.fail("rename_replace", Code::IoError);
        assert!(write_state(&d, OsStr::new("rec"), &newer).is_err());
        assert_eq!(read_state(&d, OsStr::new("rec")).unwrap(), Ok(created()), "the rename is the commit");
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
```

- [ ] **Step 4: the real filesystem.** Create `crates/flux-core/tests/state_std_fs.rs`:

```rust
//! The operation state on a real filesystem (cut 7a Part 3a): the workspace built and renamed into place, and its
//! manifest rewritten crash-safely.

use flux_core::state::{self, Kind, OpState, OperationState};
use flux_fs::{DestinationRoot, DirHandle};
use flux_platform::StdFileSystem;
use std::ffi::{OsStr, OsString};

fn names<D: DirHandle>(d: &D) -> Vec<OsString> {
    let mut v: Vec<_> = d.read_dir().unwrap().into_iter().map(|e| e.name).collect();
    v.sort();
    v
}

#[test]
fn a_workspace_is_created_and_its_manifest_rewritten_leaving_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = StdFileSystem.destination_root(tmp.path()).unwrap();
    let id = flux_core::ids::new_id();
    let s = OperationState::created(&id, Kind::Tree, tmp.path(), state::wall_time_ns());
    let ops = state::operations_dir(&dest, tmp.path()).unwrap();
    let ws = state::create_workspace(&ops, &s).unwrap();
    assert_eq!(names(&ops), vec![OsString::from(id.as_str())], "the workspace, never its creating name");
    let moving = OperationState { state: OpState::Transferring, ..s };
    state::write_state(&ws, OsStr::new(state::MANIFEST), &moving).unwrap();
    assert_eq!(state::read_state(&ws, OsStr::new(state::MANIFEST)).unwrap(), Ok(moving));
    assert_eq!(names(&ws), vec![OsString::from(state::MANIFEST)], "no temporary is left");
}
```

- [ ] **Step 5:**
  - `cargo test -p flux-core state::` passes 12 tests.
  - `cargo test -p flux-core --test state_std_fs` passes 1 test.
  - Non-vacuity:
    - Delete the final `dir.sync()` in `write_state` (return `Ok(())`).
      `a_state_is_written_through_a_flushed_temporary_then_renamed_and_the_directory_flushed` must fail. Revert it.
    - In `create_workspace`, build the workspace directly as `id`: replace `create_dir(&creating)` with
      `create_dir(id)`, and delete the rename. `a_workspace_appears_only_with_its_manifest` must fail. Revert it.
  - Then run `just check`.
- [ ] **Step 6:** commit the three files: `feat(core): the crash-safe state write, and a workspace that never exists
  without its manifest (cut 7a Part 3a)`.

---

### Task 9: the §21.1 scan

**Files:**
- Modify `crates/flux-core/src/lib.rs` and `crates/flux-core/tests/state_std_fs.rs`.
- Create `crates/flux-core/src/prior.rs`.

- [ ] **Step 0:** Task 8 is committed. `lib.rs` has `pub mod lock;` followed by `pub mod state;`.

- [ ] **Step 1: the module.** In `lib.rs`, after `pub mod lock;`, insert `pub mod prior;`. Create
  `crates/flux-core/src/prior.rs`:

```rust
//! §21.1: classify a destination's prior operations before this run creates its own (the run's step 4). A tree's are
//! the workspaces in `DEST/.flux/operations/`; a single file's are the `<target>.flux-state.<id>` records beside it.
//! The entry named with this run's own id is skipped: after a `--break-lock` takeover this run's CREATED state already
//! exists, and it is never a prior operation.

use crate::ids::is_id;
use crate::lock::error::refuse;
use crate::lock::{LockCode, LockError, LockResult};
use crate::state::{
    FLUX_DIR, Kind, MANIFEST, OPERATIONS_DIR, OperationState, RECORD_INFIX, Unusable,
    control_path_conflict, read_state,
};
use flux_fs::{Code, DirHandle, FileType};
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// A prior operation that can still be resumed: `RESUMABLE_OPERATION_EXISTS` without `--restart`, superseded with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorOp {
    pub state: OperationState,
    /// Its state's path, for messages: `DEST/.flux/operations/<id>/manifest`, or `<target>.flux-state.<id>`.
    pub shown: PathBuf,
}

/// What the scan found: every resumable prior operation, in name order. COMPLETED and ABANDONED ones are passed over
/// (§21.1: proceed; what they left is cut 9's).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub resumable: Vec<PriorOp>,
}

/// A tree's prior operations. `dest_shown` is DEST as messages name it.
pub fn scan_tree<D: DirHandle>(dest: &D, dest_shown: &Path, own_id: &str) -> LockResult<Scan> {
    let flux_shown = dest_shown.join(FLUX_DIR);
    let Some(flux) = existing_dir(dest, FLUX_DIR, &flux_shown)? else {
        return Ok(Scan::default());
    };
    let ops_shown = flux_shown.join(OPERATIONS_DIR);
    let Some(operations) = existing_dir(&flux, OPERATIONS_DIR, &ops_shown)? else {
        return Ok(Scan::default());
    };
    let mut entries = operations.read_dir()?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut scan = Scan::default();
    for entry in entries {
        // Decision 3: an operation is a DIRECTORY named by an id. Anything else here - a crash's `<id>.creating`, a
        // stray file - is not one, and is left for cut 9's cleanup.
        let Some(id) = entry.name.to_str().filter(|n| is_id(n)) else { continue };
        if entry.file_type != FileType::Dir || id == own_id {
            continue;
        }
        let shown = ops_shown.join(id).join(MANIFEST);
        let workspace = match operations.open_dir(&entry.name) {
            Ok(d) => d,
            // Gone, or no longer a directory, since the listing: not an operation now (decision 9).
            Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => continue,
            Err(e) => return Err(e.into()),
        };
        let state = match read_state(&workspace, OsStr::new(MANIFEST)) {
            Ok(decoded) => usable(decoded, &shown)?,
            // §21.1's last row: a workspace with no manifest is missing state (§249.4).
            Err(e) if e.source.kind() == ErrorKind::NotFound => {
                return Err(corrupt(&shown, "the workspace has no manifest"));
            }
            Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
                return Err(corrupt(&shown, &format!("the manifest is not a regular file: {}", e.source)));
            }
            Err(e) => return Err(e.into()),
        };
        if state.operation_id != id || state.kind != Kind::Tree {
            return Err(corrupt(&shown, "the manifest names another operation, or a single-file one"));
        }
        if state.state.is_resumable() {
            scan.resumable.push(PriorOp { state, shown });
        }
    }
    Ok(scan)
}

/// A single file's prior operations: the records `<target>.flux-state.<id>` in `parent`. `parent_shown` is the
/// target's directory as messages name it.
pub fn scan_file<D: DirHandle>(
    parent: &D,
    target: &OsStr,
    parent_shown: &Path,
    own_id: &str,
) -> LockResult<Scan> {
    let mut prefix = target.to_os_string();
    prefix.push(RECORD_INFIX);
    let mut entries = parent.read_dir()?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut scan = Scan::default();
    for entry in entries {
        // Exactly `<target>.flux-state.<id>`: another target's record, or a temporary (`<id>.tmp`), is not one.
        let Some(rest) = entry.name.as_encoded_bytes().strip_prefix(prefix.as_encoded_bytes()) else {
            continue;
        };
        let Some(id) = std::str::from_utf8(rest).ok().filter(|r| is_id(r)) else { continue };
        if id == own_id {
            continue;
        }
        let shown = parent_shown.join(&entry.name);
        // A record's exact name beside a user's target is Flux's (F2), and no crash leaves anything but a regular file
        // there: anything else is unreadable state (§21.1), refused rather than passed over (decision 9; panel round 3).
        if entry.file_type != FileType::File {
            return Err(corrupt(&shown, "a record's name holds something other than a regular file"));
        }
        let state = match read_state(parent, &entry.name) {
            Ok(decoded) => usable(decoded, &shown)?,
            // Removed by its own finishing run since the listing: not a record now.
            Err(e) if e.source.kind() == ErrorKind::NotFound => continue,
            Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
                return Err(corrupt(&shown, &format!("the record is not a regular file: {}", e.source)));
            }
            Err(e) => return Err(e.into()),
        };
        if state.operation_id != id || state.kind != Kind::File {
            return Err(corrupt(&shown, "the record names another operation, or a tree"));
        }
        if state.state.is_resumable() {
            scan.resumable.push(PriorOp { state, shown });
        }
    }
    Ok(scan)
}

/// `RESUMABLE_OPERATION_EXISTS` for `op`, naming it and the way out (the refusal-guidance table).
pub fn resumable_refusal(op: &PriorOp) -> LockError {
    refuse(
        LockCode::ResumableOperationExists,
        None,
        format!(
            "operation {} is {} ({}); run again with --restart to supersede it (its partials are deleted)",
            op.state.operation_id,
            op.state.state.as_str(),
            op.shown.display()
        ),
    )
}

fn usable(decoded: Result<OperationState, Unusable>, shown: &Path) -> LockResult<OperationState> {
    match decoded {
        Ok(state) => Ok(state),
        Err(Unusable::Corrupt(why)) => Err(corrupt(shown, &why)),
        Err(Unusable::Incompatible(version)) => Err(refuse(
            LockCode::IncompatibleState,
            None,
            format!(
                "{} has format_version {version}: it was written by a newer Flux; use that version, or remove it by hand",
                shown.display()
            ),
        )),
    }
}

fn corrupt(shown: &Path, why: &str) -> LockError {
    refuse(
        LockCode::StateCorrupt,
        None,
        format!(
            "{}: {why}; Flux never deletes state it cannot read (§249.4): inspect it, and remove it by hand if it is not needed",
            shown.display()
        ),
    )
}

/// A control directory that may be absent: `None` if it is, a handle if it is a directory, and
/// `CONTROL_PLANE_NAMESPACE_CONFLICT` if something else holds the name.
fn existing_dir<D: DirHandle>(parent: &D, name: &str, shown: &Path) -> LockResult<Option<D>> {
    let name = OsStr::new(name);
    match parent.metadata(name) {
        Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(m) if m.file_type != FileType::Dir => Err(control_path_conflict(shown)),
        Ok(_) => match parent.open_dir(name) {
            Ok(d) => Ok(Some(d)),
            Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
                Err(control_path_conflict(shown))
            }
            Err(e) => Err(e.into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::{FakeDirHandle, FaultFs};
    use crate::lock::test_support::refusal;
    use crate::state::{OpState, record_name};
    use flux_fs::{DestinationRoot, FileSystem};

    const OPS: &str = "/p/dest/.flux/operations";

    fn id(n: u8) -> String {
        format!("{n:032x}")
    }

    fn state(n: u8, kind: Kind, s: OpState) -> OperationState {
        OperationState { state: s, ..OperationState::created(&id(n), kind, Path::new("/p/dest"), 1) }
    }

    /// A fake with `/p/dest`; the handle is on it.
    fn dest() -> (FaultFs, FakeDirHandle) {
        let fs = FaultFs::new();
        for p in ["/p", "/p/dest"] {
            fs.create_dir(Path::new(p)).unwrap();
        }
        let d = fs.destination_root(Path::new("/p/dest")).unwrap();
        (fs, d)
    }

    fn operations(fs: &FaultFs) {
        for p in ["/p/dest/.flux", OPS] {
            fs.create_dir(Path::new(p)).unwrap();
        }
    }

    fn workspace(fs: &FaultFs, n: u8, manifest: Option<&[u8]>) {
        fs.create_dir(Path::new(&format!("{OPS}/{}", id(n)))).unwrap();
        if let Some(bytes) = manifest {
            fs.write_file(format!("{OPS}/{}/manifest", id(n)), bytes);
        }
    }

    fn scan(d: &FakeDirHandle, own: u8) -> LockResult<Scan> {
        scan_tree(d, Path::new("D"), &id(own))
    }

    fn ids(s: &Scan) -> Vec<String> {
        s.resumable.iter().map(|p| p.state.operation_id.clone()).collect()
    }

    #[test]
    fn no_control_directory_means_no_prior_operation() {
        let (fs, d) = dest();
        assert_eq!(scan(&d, 0).unwrap(), Scan::default());
        fs.create_dir(Path::new("/p/dest/.flux")).unwrap();
        assert_eq!(scan(&d, 0).unwrap(), Scan::default(), ".flux without operations/");
    }

    #[test]
    fn a_foreign_object_at_a_control_path_is_a_conflict() {
        let (fs, d) = dest();
        fs.write_file("/p/dest/.flux", b"x");
        let r = refusal(scan(&d, 0));
        assert_eq!(r.code, LockCode::ControlPlaneNamespaceConflict);
        assert!(r.detail.contains(".flux"), "{}", r.detail);
        let (fs, d) = dest();
        fs.create_dir(Path::new("/p/dest/.flux")).unwrap();
        fs.write_file(OPS, b"x");
        assert_eq!(refusal(scan(&d, 0)).code, LockCode::ControlPlaneNamespaceConflict);
    }

    #[test]
    fn only_created_transferring_and_failed_operations_are_resumable() {
        let (fs, d) = dest();
        operations(&fs);
        for (n, s) in [
            (1, OpState::Created),
            (2, OpState::Transferring),
            (3, OpState::Failed),
            (4, OpState::Completed),
            (5, OpState::Abandoned),
            (6, OpState::Created),
        ] {
            workspace(&fs, n, Some(&state(n, Kind::Tree, s).encode()));
        }
        let found = scan(&d, 6).unwrap();
        assert_eq!(ids(&found), vec![id(1), id(2), id(3)], "COMPLETED, ABANDONED and this run's own are passed over");
        let shown = found.resumable[0].shown.display().to_string();
        assert!(shown.contains(&id(1)) && shown.ends_with("manifest"), "{shown}");
    }

    #[test]
    fn what_is_not_a_directory_named_by_an_id_is_not_an_operation() {
        let (fs, d) = dest();
        operations(&fs);
        fs.create_dir(Path::new(&format!("{OPS}/junk"))).unwrap();
        fs.create_dir(Path::new(&format!("{OPS}/{}.creating", id(7)))).unwrap();
        fs.write_file(format!("{OPS}/{}", id(8)), &state(8, Kind::Tree, OpState::Created).encode());
        assert_eq!(scan(&d, 0).unwrap(), Scan::default());
    }

    #[test]
    fn a_workspace_with_no_manifest_is_state_corrupt() {
        let (fs, d) = dest();
        operations(&fs);
        workspace(&fs, 9, None);
        let r = refusal(scan(&d, 0));
        assert_eq!(r.code, LockCode::StateCorrupt);
        assert!(r.detail.contains(&id(9)) && r.detail.contains("no manifest"), "{}", r.detail);
        assert!(r.detail.contains("remove it by hand"), "{}", r.detail);
    }

    #[test]
    fn an_unreadable_newer_or_mismatched_manifest_is_refused() {
        for (bytes, code) in [
            (b"garbage".to_vec(), LockCode::StateCorrupt),
            (br#"{"format_version":2}"#.to_vec(), LockCode::IncompatibleState),
            (state(3, Kind::Tree, OpState::Created).encode(), LockCode::StateCorrupt),
            (state(4, Kind::File, OpState::Created).encode(), LockCode::StateCorrupt),
        ] {
            let (fs, d) = dest();
            operations(&fs);
            workspace(&fs, 4, Some(&bytes));
            let r = refusal(scan(&d, 0));
            assert_eq!(r.code, code, "{}", r.detail);
            if code == LockCode::IncompatibleState {
                assert!(r.detail.contains("format_version 2"), "{}", r.detail);
            }
        }
    }

    #[test]
    fn a_corrupt_state_is_refused_even_beside_a_resumable_one() {
        let (fs, d) = dest();
        operations(&fs);
        workspace(&fs, 1, Some(&state(1, Kind::Tree, OpState::Created).encode()));
        workspace(&fs, 2, Some(b"garbage"));
        assert_eq!(refusal(scan(&d, 0)).code, LockCode::StateCorrupt);
    }

    #[test]
    fn a_single_file_scan_reads_only_its_targets_records() {
        let (fs, d) = dest();
        let put = |name: String, s: &OperationState| fs.write_file(format!("/p/dest/{name}"), &s.encode());
        let rec = |n: u8| record_name(OsStr::new("t"), &id(n)).into_string().unwrap();
        put(rec(1), &state(1, Kind::File, OpState::Created));
        put(rec(2), &state(2, Kind::File, OpState::Completed));
        put(format!("u.flux-state.{}", id(3)), &state(3, Kind::File, OpState::Created));
        put(format!("{}.tmp", rec(4)), &state(4, Kind::File, OpState::Created));
        put(format!("tt.flux-state.{}", id(5)), &state(5, Kind::File, OpState::Created));
        put(rec(6), &state(6, Kind::File, OpState::Created));
        let found = scan_file(&d, OsStr::new("t"), Path::new("P"), &id(6)).unwrap();
        assert_eq!(ids(&found), vec![id(1)]);
        assert!(found.resumable[0].shown.ends_with(rec(1)));

        fs.write_file(format!("/p/dest/{}", rec(7)), b"garbage");
        assert_eq!(
            refusal(scan_file(&d, OsStr::new("t"), Path::new("P"), &id(6))).code,
            LockCode::StateCorrupt
        );
        let (fs, d) = dest();
        fs.write_file(format!("/p/dest/{}", rec(8)), &state(8, Kind::Tree, OpState::Created).encode());
        assert_eq!(
            refusal(scan_file(&d, OsStr::new("t"), Path::new("P"), &id(0))).code,
            LockCode::StateCorrupt,
            "a tree's state in a single file's record"
        );
        let (fs, d) = dest();
        fs.create_dir(Path::new(&format!("/p/dest/{}", rec(9)))).unwrap();
        assert_eq!(
            refusal(scan_file(&d, OsStr::new("t"), Path::new("P"), &id(0))).code,
            LockCode::StateCorrupt,
            "a directory at a record's exact name is never passed over"
        );
    }

    #[test]
    fn a_resumable_refusal_names_the_operation_and_the_way_out() {
        let op = PriorOp {
            state: state(1, Kind::Tree, OpState::Failed),
            shown: PathBuf::from("D/.flux/operations/x/manifest"),
        };
        let r = refusal::<()>(Err(resumable_refusal(&op)));
        assert_eq!(r.code, LockCode::ResumableOperationExists);
        for part in [id(1).as_str(), "FAILED", "--restart", "manifest"] {
            assert!(r.detail.contains(part), "{part}: {}", r.detail);
        }
    }
}
```

- [ ] **Step 2: the real filesystem.** Append to `crates/flux-core/tests/state_std_fs.rs`:

```rust

#[test]
fn the_scan_finds_a_resumable_workspace_and_passes_over_a_creating_one() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = StdFileSystem.destination_root(tmp.path()).unwrap();
    let ops = state::operations_dir(&dest, tmp.path()).unwrap();
    let prior = OperationState::created(&flux_core::ids::new_id(), Kind::Tree, tmp.path(), 1);
    drop(state::create_workspace(&ops, &prior).unwrap());
    let mut creating = flux_core::ids::new_id();
    creating.push_str(state::CREATING_SUFFIX);
    drop(ops.create_dir(OsStr::new(&creating)).unwrap());
    let scan = flux_core::prior::scan_tree(&dest, tmp.path(), &flux_core::ids::new_id()).unwrap();
    assert_eq!(scan.resumable.len(), 1);
    assert_eq!(scan.resumable[0].state, prior);
}
```

- [ ] **Step 3:**
  - `cargo test -p flux-core prior::` passes 9 tests.
  - `cargo test -p flux-core --test state_std_fs` passes 2 tests.
  - Non-vacuity:
    - Delete `|| id == own_id` from `scan_tree`'s skip.
      `only_created_transferring_and_failed_operations_are_resumable` must fail. Revert it.
    - Replace `.filter(|n| is_id(n))` with `.filter(|_| true)` in `scan_tree`.
      `what_is_not_a_directory_named_by_an_id_is_not_an_operation` must fail. Revert it.
    - Delete `if state.state.is_resumable()` (push every state). The same `only_created...` test must fail. Revert it.
    - In `scan_file`, replace the `if entry.file_type != FileType::File { return Err(corrupt(...)); }` block with
      `if entry.file_type != FileType::File { continue; }`. `a_single_file_scan_reads_only_its_targets_records` must
      fail at "a directory at a record's exact name". Revert it.
  - Then run `just check`.
- [ ] **Step 4:** commit the three files: `feat(core): the §21.1 prior-state scan for trees and single files (cut 7a
  Part 3a)`.

---

### Task 10: the design spec's refinements

**Files:** Modify `docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md`.

- [ ] **Step 0:**
  - The "Operation state" bullets contain `- Written crash-safely: `<name>.tmp.<id>` in the same directory, `sync`,
    rename over the final name, `sync` the`.
  - The first bullet begins `- A tree copy keeps it at `DEST/.flux/operations/<id>/manifest`.`.
  - "Declared refinements of the spec" ends with item 8, whose last sentence is `It exists so that no record ever names
    missing state (F5).`.
  - The crash table's last row begins `| during §240.3 recovery, between the move-aside and the new lock |`.
  - "What this closes, and what it leaves" has a list headed `**New residues:**`.

- [ ] **Step 1: the edits.**
  - Replace `` `<name>.tmp.<id>` in the same directory`` with `` `<name>.tmp` in the same directory (refinement 10)``.
  - After the first bullet's sentence `A single-file copy keeps it at `target.flux-state.<id>`, beside the target.`,
    insert: ` A tree workspace is built as `operations/<id>.creating/` and renamed to `<id>` once its manifest is
    written (refinement 9).`
  - Append to "Declared refinements of the spec", after item 8:

```markdown
9. **A tree workspace never exists without its manifest** (Part 3a, owner-approved K1). It is built as
   `operations/<id>.creating/`, its manifest is written inside crash-safely, and it is renamed to `<id>` without
   replacing. The §21.1 scan ignores every entry of `operations/` that is not a directory named by an id. Otherwise a
   crash between the `mkdir` and the manifest's rename would leave a manifest-less workspace (`STATE_CORRUPT`, which
   no flag clears) behind an empty lock. Part 3b retires a workspace the same way before removing it.
10. **A state file's temporary is `<name>.tmp`** (Part 3a, L2), not `<name>.tmp.<id>`: the name it stages already
    carries the operation id. A single-file run whose longest name (its record's temporary, target + 48 units) would
    exceed the name-length limit is refused `PATH_COMPONENT_INVALID` before anything is created.
11. **`destination_root` is the path's display form** (Part 3a, M1): lossy for a path that is not valid Unicode, and
    never read back for a decision.
```

  - Append three rows to "What each crash window leaves":

```markdown
| while creating a tree operation's workspace, before its rename | an empty lock, and `operations/<id>.creating/` | `TARGET_LOCK_UNCERTAIN`; `--restart --break-lock` proceeds, and the scan passes over the `.creating` directory (cut 9's) |
| while writing a single-file operation's first state record, before its rename | an empty lock, and `<target>.flux-state.<id>.tmp` | `TARGET_LOCK_UNCERTAIN`; `--restart --break-lock` proceeds, and the scan passes over the `.tmp` file (cut 9's) |
| while rewriting a state, before its rename | the previous state, whole, and `<name>.tmp` beside it | as the previous state says; removing that state later removes its `.tmp` too |
```

  - In "What this closes, and what it leaves", append to the "**New residues:**" list:

```markdown
- Crash leftovers of a first state write that never completed - `operations/<id>.creating/` and
  `<target>.flux-state.<id>.tmp` - which no state names, so only cut 9's cleanup reclaims them (refinements 9-10).
```

- [ ] **Step 2:** `just typos` passes (`just check` runs it too).
- [ ] **Step 3:** commit the file: `docs: refinements 9-11 of the cut 7a design (Part 3a)`.

---

### Task 11: Part 3a verification

- [ ] **Step 1:** `just check`, `just check-linux` (exit 0, all passed) and `just check-mac` (exit 0) all pass.
  `git status --short` is clean.
- [ ] **Step 2 (driver):** push `spec/cut-7a`. CI's three test jobs must be green; `dir_control.rs` runs natively on
  macOS there. Then the capstone and the test audit, then Part 3b's forks and plan.

---

## Self-review

**Spec coverage.** Part 3a owns these parts of the design spec:

| Spec item | Where |
|---|---|
| "Operation state": JSON, `format_version` first, exactly its keys, `STATE_CORRUPT` / `INCOMPATIBLE_STATE` | Task 7 |
| "Operation state": crash-safe write; the manifest's and the record's places and names | Tasks 4, 8 |
| "The run" step 2: a foreign object at `DEST/.flux` or a reserved directory | Tasks 8, 9 (`control_path_conflict`; 3b calls it up front) |
| Step 4: the §21.1 scan, own id skipped, every row of its table | Task 9 |
| Step 5: `DEST/.flux/`, `operations/`, `operations/<id>/` and the manifest | Task 8 |
| `flux-fs`: the fake models what the protocol needs (legacy delete) | Task 6 |
| Refinements 9-11 recorded | Task 10 |

Left to Part 3b:
- steps 1, 3, 5's record and state transitions, 6 and 7;
- `--restart` and `--break-lock`'s state order;
- the engine's §99 hooks;
- the reserved-path check for source entries;
- the CLI;
- "Testing" item 4 (the end-to-end tests).

**Placeholder scan.** Every step names its file and gives its code. The only conditional is Task 1's
`#[allow(dead_code)]` on `FILE_ADD_FILE`, which Task 4 resolves either way.

**Type consistency:**

| Item | Visibility | Defined | Used |
|---|---|---|---|
| `DirHandle::{read_dir, read_file, remove_dir, sync}` | public | Tasks 1-4 | 8, 9 |
| `state::{OperationState, Kind, OpState, Takeover, Unusable, decode, wall_time_ns}` | public | 7 | 8, 9, 3b |
| `state::{write_state, read_state, create_workspace, operations_dir, record_name, temp_name, file_names_fit}` | public | 8 | 9, 3b |
| `state::{FLUX_DIR, OPERATIONS_DIR, MANIFEST, TEMP_SUFFIX, CREATING_SUFFIX, RECORD_INFIX, FORMAT_VERSION, STATE_LIMIT, UNREADABLE}` | public | 7, 8 | 8, 9 |
| `state::control_path_conflict` | `pub(crate)` | 8 | 9 |
| `prior::{PriorOp, Scan, scan_tree, scan_file, resumable_refusal}` | public | 9 | 3b |
| `LockCode::{StateCorrupt, IncompatibleState, ResumableOperationExists}` | public | 7 | 9 |
| `site::name_len`, `lock_file::read_loop` | `pub(crate)` | 8, 2 | 8, 2 |
| `FaultFs::set_legacy_delete` | public (test-only module) | 6 | 6 |

**Declared against the spec:** decisions 3 (workspace built and renamed), 4 (`.tmp` names), 5 (lossy
`destination_root`), 8 (corrupt state takes precedence) and 9 (entries that change mid-scan). All are recorded as
refinements 9-11, or in the scan's comments.

## Stand-downs

Panel round 1 (agy), at `7d1b803`:
- REJECTED: "`oa.ObjectName = &raw const us` does not compile, the field is `*mut`". windows-sys 0.61
  `Wdk/Foundation/mod.rs:1611` declares `pub ObjectName: *const ...UNICODE_STRING`, and `dir_windows.rs:119` assigns
  it the same way in code that builds today. agy withdrew it on the negotiation turn.
- Agreed below the floor: let-chains are stable on the 1.98.1 toolchain and already used in the repo.

Panel round 2 (agy), at `7d1b803`. Every task's Step 0 quotes were checked, and every named mutant was traced to its
test going red.
- FOLDED: a crash during a single-file state write leaves `<target>.flux-state.<id>.tmp` in the user's directory.
  This adds the crash-table rows and a residue (Task 10), and the handoff that 3b removes a state's `.tmp` together
  with the state.
- DISCARDED-BELOW-FLOOR: "the fake's `remove_dir` does not model a delete-pending directory".
  - `delete_open` tries POSIX delete first, which frees the name at once on NTFS and ReFS (`dir_windows.rs:624-647`
    today).
  - The legacy fallback runs only on a volume without POSIX delete, and `capability_of` refuses every volume that is
    not NTFS or ReFS (`lock_file.rs:232`) before any state exists.
  - What is left is NTFS on Windows 10 before 1709, which is out of support.

Panel round 3 (agy), at `7047448`. The Fold Auditor found round 2's fold correct and its spec anchor present.
- FOLDED: `scan_file` passed over a directory or link at a record's exact name, where the spec classifies every record.
  It is now `STATE_CORRUPT` (decision 9, `scan_file`, a test and its mutant).

Panel round 4 (agy), at `12dc8d0`: GREEN, with no findings.
- The Fold Auditor traced the new `scan_file` branch through the fake's listing and saw its mutant go red.
- The Consistency Checker found the fold consistent with decision 3, the crash rows and the `.tmp` pass-over.
- No ordinary crash reaches the new `STATE_CORRUPT`.
