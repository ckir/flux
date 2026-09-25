# Cut 4a — single-file safety through directory handles: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route every single-file copy through a destination directory handle, and refuse a copy whose destination is the source itself by identity.

**Architecture:** One algorithm, `copy_file_at(fs, src, parent: &Dir, name, opts)`, writes only through the `DirHandle` it is given. `copy_file(fs, src, dst, opts)` keeps its signature shape (bound tightened to `DestinationRoot`) and becomes a wrapper that resolves the destination's parent once. A new Step 2a gate stats the destination through the handle and refuses an identity-equal or directory destination, degrading (reported via `Outcome.identity_degraded`) or refusing (under `Safety::Strict`) when identity cannot be compared. The POSIX handle `rename_replace` gains the read-only guard the path version already has; the Windows arm is measured first and guarded if needed.

**Tech Stack:** Rust (edition 2024, stable 1.98), `rustix` (POSIX), `windows-sys` NT APIs (Windows), `cargo nextest`, `just`.

**Design authority:** `docs/superpowers/specs/2026-09-25-cut-4-after-handle-relative-writes.md` (decisions 1 and 6), then `docs/superpowers/specs/2026-09-23-directory-walker-design.md` sections "The hole the directory checks do not cover" and "Where `--safety=strict` lives" for the gate's semantics. Where those documents show code, THE CODE ON `main` WINS; every citation below was grep-verified against `main` at `2dad9ae`.

---

## Ground rules for every task

- **Worktree:** `E:\Rust\flux-engine`, branch `feat/copy-file-handles`, based on `main` at `2dad9ae`. Its upstream is `origin/main`, so NEVER run a bare `git push`; publishing is `just pr` at the end, after the capstone, with owner approval.
- **Step 0 of every task — state verification.** Open each file the task names and confirm the quoted "current code" matches. If it does not, STOP and report `STATE_MISMATCH: <file>: <what differs>`. Do not adapt.
- **Shape-divergence stop.** If making something compile would change the type, shape or encoding of any value shown in this plan, STOP and report `[plan] -> [yours] because <reason>`.
- **Tests are the oracle.** Where a task supplies tests, they are already written — implement until they pass. If a test looks wrong, report the conflict; never edit a test to match the code.
- **Gates (exact commands, do not invent stricter ones):**
  - Windows, the repo gate: `just check` → expect a line `Summary [...] N tests run: N passed, 2 skipped` and exit 0.
  - Linux (WSL): `wsl.exe -d Ubuntu-26.04 bash -l <script>` with a Linux-side `CARGO_TARGET_DIR`, running `cargo clippy --workspace --all-targets -- -D warnings` and `cargo nextest run --workspace --no-tests=pass --no-fail-fast`. From Git Bash prefix `MSYS_NO_PATHCONV=1`.
  - macOS (cross-check, no Mac needed): `cargo clippy --workspace --exclude flux --all-targets --target aarch64-apple-darwin -- -D warnings` (the root crate is excluded because criterion's `alloca` build script needs a macOS C toolchain).
- **Write any file containing a backslash with the Write/Edit tools, never a Bash heredoc** — the Bash tool collapses `\\` to `\`.
- Commit after each task with a Conventional Commit message ending in `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## File map

| File | Change |
|---|---|
| `crates/flux-core/src/fault_fs.rs` | `destination_root` accepts a root path (Task 1) |
| `crates/flux-fs/src/options.rs` | `Safety` enum; `CopyOptions.safety`; `Outcome.identity_degraded` (Task 2) |
| `crates/flux-fs/src/lib.rs` | export `Safety` (Task 2) |
| `crates/flux-cli/src/main.rs` | `safety: Safety::Default` (Task 2) |
| `crates/flux-platform/src/dir_unix.rs` | read-only guard on `rename_replace` (Task 3) |
| `crates/flux-platform/src/dir_windows.rs` | read-only guard on `rename_replace`, if measurement requires it (Task 4) |
| `crates/flux-platform/tests/dir_handle.rs` | guard tests, both arms (Tasks 3, 4) |
| `crates/flux-core/src/copy.rs` | `copy_file_at`, the wrapper, `split_destination`, the Step 2a gate, tests (Tasks 5, 6) |
| `crates/flux-core/src/lib.rs` | export `copy_file_at` (Task 5) |
| `crates/flux-cli/tests/copy.rs` | end-to-end alias refusals (Task 7) |
| `TODO.md`, walker design | bookkeeping (Task 8) |

---

### Task 1: `FaultFs::destination_root` accepts the filesystem root

Every existing `copy_file` test writes to `/dst`. After Task 5, `copy_file` opens `/dst`'s parent `/` with `destination_root`, and the fake refuses it today because `/` was never `create_dir`'d. The fake already treats a root as implicitly present for `create_dir` (`crates/flux-core/src/fault_fs.rs:702-713`); this makes `destination_root` agree.

**Files:**
- Modify: `crates/flux-core/src/fault_fs.rs` — `impl DestinationRoot for FaultFs`, currently at lines 728-752
- Test: same file, `mod tests`

- [ ] **Step 0: Verify state.** `destination_root` must currently begin:

```rust
    fn destination_root(&self, path: &Path) -> Result<Self::Dir> {
        let mut g = self.inner.lock().unwrap();
        if !g.directories.contains(path) {
```

- [ ] **Step 1: Write the failing test** (append inside `mod tests` in `fault_fs.rs`):

```rust
    #[test]
    fn destination_root_accepts_the_filesystem_root() {
        // A real filesystem always has its root, and create_dir already treats a
        // root as present (the root-parent rule in `create_dir`). copy_file opens
        // `/dst`'s parent through destination_root, so the fake must agree.
        use flux_fs::{DestinationRoot, DirHandle};
        let fs = FaultFs::new();
        let root = fs.destination_root(Path::new("/")).expect("the root is a directory");
        let mut w = root.create_new(std::ffi::OsStr::new("f")).unwrap();
        std::io::Write::write_all(&mut w, b"x").unwrap();
        drop(w);
        assert_eq!(fs.read_file("/f").as_deref(), Some(&b"x"[..]));
    }
```

- [ ] **Step 2: Run it, expect FAIL.** `cargo nextest run -p flux-core -E 'test(destination_root_accepts_the_filesystem_root)'` → FAIL: `the root is a directory: FsError { code: IoError, .. NotFound .. }`.

- [ ] **Step 3: Implement.** Replace the guard line `if !g.directories.contains(path) {` with:

```rust
        // A path that has a root but no parent IS a root (`/`, `C:\`), which exists on
        // any real filesystem. Same rule `create_dir` applies to a root-level create.
        let is_root = path.has_root() && path.parent().is_none();
        if !is_root && !g.directories.contains(path) {
```

- [ ] **Step 4: Run it, expect PASS**, then the whole crate: `cargo nextest run -p flux-core --no-fail-fast` → all pass.
- [ ] **Step 5: Commit** — `test(flux-core): FaultFs::destination_root accepts the filesystem root`.

---

### Task 2: `Safety`, `CopyOptions.safety`, `Outcome.identity_degraded`

Plumbing only; no behaviour yet. Both structs have exactly two construction sites each (grep-verified): `CopyOptions` at `crates/flux-cli/src/main.rs:31` and `crates/flux-core/src/copy.rs:255`; `Outcome` at `crates/flux-core/src/copy.rs:245` and `crates/flux-fs/src/options.rs:95`.

**Files:**
- Modify: `crates/flux-fs/src/options.rs:51-71`, `:95`
- Modify: `crates/flux-fs/src/lib.rs:9-12`
- Modify: `crates/flux-cli/src/main.rs:4`, `:31-38`
- Modify: `crates/flux-core/src/copy.rs:245`, `:254-262`

- [ ] **Step 0: Verify state.** `options.rs` must contain:

```rust
#[derive(Debug, Clone)]
pub struct CopyOptions {
    pub preserve_times: Preserve,
    pub preserve_permissions: Preserve,
    pub durability: Durability,
    pub publish: Publish,
    pub operation_id: OperationId,
}
```

and

```rust
#[derive(Debug)]
pub struct Outcome {
    pub bytes_copied: u64,
    /// Empty on a clean copy. Non-empty means published-with-complaints: the caller
    /// reports each as METADATA_APPLY_FAILED and the operation exits 1.
    pub metadata_failures: Vec<MetadataFailure>,
}
```

- [ ] **Step 1: Add `Safety`** in `options.rs`, directly above `pub struct CopyOptions`:

```rust
/// Whether a safety check that cannot be made with full confidence refuses or degrades.
///
/// An enum rather than a bool for the reason `Preserve`, `Durability` and `Publish` are:
/// named states read better at every construction site, and a third mode is easy to add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    /// Degrade to the lexical floor when identity is not `Strong` on both sides, and
    /// report the degradation (`Outcome::identity_degraded`).
    Default,
    /// Refuse whenever identity is not `Strong` on both sides of an existing destination.
    Strict,
}
```

- [ ] **Step 2: Add the fields.** In `CopyOptions`, after `pub publish: Publish,` add `pub safety: Safety,`. In `Outcome`, after `metadata_failures` add:

```rust
    /// `Some` when the Step 2a identity gate could not compare the source with an
    /// EXISTING destination and fell back to the lexical floor. Carries the WEAKER of
    /// the two sides, which is what a warning aggregates on: `Weak(id)` names
    /// `id.volume`, `Unavailable` names nothing. `None` on the normal path AND when the
    /// destination did not exist, which is not a degradation.
    pub identity_degraded: Option<crate::FileIdentity>,
```

- [ ] **Step 3: Fix the four construction sites.**
  - `options.rs:95`: `let o = Outcome { bytes_copied: 10, metadata_failures: Vec::new(), identity_degraded: None };`
  - `copy.rs:245`: `Ok(Outcome { bytes_copied, metadata_failures, identity_degraded: None })` (Task 6 replaces `None`).
  - `copy.rs` `opts()` helper: add `safety: flux_fs::Safety::Default,` after `publish: Publish::Replace,`.
  - `main.rs`: extend the import to `use flux_fs::{CopyOptions, Durability, OperationId, Preserve, Publish, Safety};` and add `safety: Safety::Default,` after `publish: Publish::Replace,`. (The `--safety=strict` flag is cut 5.)
- [ ] **Step 4: Export.** In `crates/flux-fs/src/lib.rs` add `Safety` to the `pub use options::{...}` list, keeping it alphabetical: `CopyOptions, Durability, MetadataFailure, MetadataItem, OperationId, Outcome, Preserve, Publish, Safety, temp_path,`.
- [ ] **Step 5: Verify.** `just check` → exit 0, test count unchanged.
- [ ] **Step 6: Commit** — `feat(flux-fs): Safety on CopyOptions and identity_degraded on Outcome`.

---

### Task 3: POSIX — the handle `rename_replace` refuses a read-only target

`StdDir::rename_replace` is a bare `renameat` (`crates/flux-platform/src/dir_unix.rs:171-176`). The path version refuses a destination this user may not write (`destination_is_write_protected`, `crates/flux-platform/src/std_fs.rs:78-102`) because `rename(2)` checks the DIRECTORY, not the file. Task 5 routes `copy_file` through the handle, so without this the handle path silently overwrites a read-only file that `flux copy` refuses today.

**Files:**
- Modify: `crates/flux-platform/src/dir_unix.rs:4` (imports), `:171-176`
- Test: `crates/flux-platform/tests/dir_handle.rs`, inside `mod posix` (starts line 2)

- [ ] **Step 0: Verify state.** `dir_unix.rs` line 4 is `use rustix::fs::{AtFlags, Mode, OFlags, openat, statat};` and `rename_replace` is:

```rust
    fn rename_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        rustix::fs::renameat(&self.0, from, &other.0, to)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }
```

- [ ] **Step 1: Write the failing tests** — append inside `mod posix` in `dir_handle.rs`. They mirror `crates/flux-platform/tests/std_fs.rs:30`, `:68` and `:219`, which pin the same rule for the path version.

```rust
    #[test]
    fn rename_replace_refuses_a_read_only_target() {
        // The path version's rule, now for the handle (std_fs.rs,
        // rename_replace_refuses_a_read_only_target). `renameat` checks the
        // DIRECTORY's permission, not the file's, so without the guard a read-only
        // destination is silently replaced.
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("from")).unwrap());
        let to = d.path().join("to");
        std::fs::write(&to, b"protected").unwrap();
        let mut perms = std::fs::metadata(&to).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&to, perms).unwrap();

        let err = root.rename_replace(OsStr::new("from"), &root, OsStr::new("to")).unwrap_err();

        assert_eq!(err.code, flux_fs::Code::PermissionDenied);
        assert!(err.source.raw_os_error().is_none(), "the guard must refuse, not the OS");
        assert_eq!(std::fs::read(&to).unwrap(), b"protected", "the protected file must survive");
        std::fs::remove_file(&to).expect("a read-only file must still be removable");
    }

    #[test]
    fn rename_replace_allows_a_symlink_whose_target_is_read_only() {
        // The guard judges the NAME being replaced; renameat replaces the link and
        // never touches its target (std_fs.rs, the same-named test).
        let d = TempDir::new().unwrap();
        let target = d.path().join("target");
        std::fs::write(&target, b"protected").unwrap();
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&target, perms).unwrap();
        std::os::unix::fs::symlink(&target, d.path().join("link")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut w = root.create_new(OsStr::new("from")).unwrap();
        std::io::Write::write_all(&mut w, b"new").unwrap();
        drop(w);

        root.rename_replace(OsStr::new("from"), &root, OsStr::new("link"))
            .expect("replacing a symlink must not consult its target");

        assert_eq!(std::fs::read(&target).unwrap(), b"protected", "the target must be untouched");
        assert_eq!(std::fs::read(d.path().join("link")).unwrap(), b"new");
        std::fs::remove_file(&target).expect("a read-only file must still be removable");
    }

    #[test]
    fn rename_replace_refuses_a_target_this_user_cannot_write() {
        // A root-owned 0644 file: the owner write bit is set, so a mode-bit check
        // passes, yet this user cannot write it (std_fs.rs, the same-named test).
        // Skips without passwordless sudo, exactly as that test does.
        if !std::process::Command::new("sudo").args(["-n", "true"]).status().is_ok_and(|s| s.success())
        {
            eprintln!("skipped: no passwordless sudo, cannot build a root-owned fixture");
            return;
        }
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("from")).unwrap());
        let to = d.path().join("to");
        let sh = format!("echo protected > {t} && chmod 0644 {t} && chown 0:0 {t}", t = to.display());
        assert!(
            std::process::Command::new("sudo").args(["-n", "sh", "-c", &sh]).status().unwrap().success(),
            "fixture"
        );

        assert!(
            root.rename_replace(OsStr::new("from"), &root, OsStr::new("to")).is_err(),
            "a file we cannot write must be refused"
        );
        assert_eq!(
            std::process::Command::new("sudo")
                .args(["-n", "cat", &to.display().to_string()])
                .output()
                .unwrap()
                .stdout,
            b"protected\n",
            "the protected file must survive"
        );
        let _ = std::process::Command::new("sudo")
            .args(["-n", "rm", "-f", &to.display().to_string()])
            .status();
    }
```

- [ ] **Step 2: Run in WSL, expect FAIL** for `rename_replace_refuses_a_read_only_target` (the rename succeeds) and, where sudo is available, `..._this_user_cannot_write`. The symlink test passes before and after (it pins that the guard does not over-refuse).
- [ ] **Step 3: Implement.** In `dir_unix.rs` change line 4 to `use rustix::fs::{Access, AtFlags, Mode, OFlags, accessat, openat, statat};`, add this free function below `impl DirHandle for StdDir` (after its closing brace):

```rust
/// The handle-relative twin of `destination_is_write_protected` in `std_fs.rs`,
/// asking the same question the same way, of a NAME inside `dir`.
///
/// `renameat` checks the DIRECTORY's write permission and never the target's, so
/// without this it replaces a file the user marked read-only -- `cp -f` semantics
/// where plain `cp` refuses. Same three rules as the path version, for the same
/// measured reasons recorded there: a missing name protects nothing; a symlink is
/// replaced and never followed, so its target's mode is irrelevant; and only
/// EACCES / EPERM / EROFS from an EFFECTIVE-uid `accessat` mean "you may not write
/// this" -- anything else (ETXTBSY for a running binary) is not a protection.
fn is_write_protected_at(dir: &OwnedFd, name: &OsStr) -> bool {
    use rustix::fs::FileType;
    use rustix::io::Errno;
    match statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(_) => false,
        Ok(st) if FileType::from_raw_mode(st.st_mode) == FileType::Symlink => false,
        Ok(_) => matches!(
            accessat(dir, name, Access::WRITE_OK, AtFlags::EACCESS),
            Err(Errno::ACCESS | Errno::PERM | Errno::ROFS)
        ),
    }
}
```

and replace `rename_replace`'s body with:

```rust
    fn rename_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        // The TARGET lives in `other`. See `is_write_protected_at`.
        if is_write_protected_at(&other.0, to) {
            return Err(FsError::new(
                Code::PermissionDenied,
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "destination is read-only"),
            ));
        }
        rustix::fs::renameat(&self.0, from, &other.0, to)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }
```

- [ ] **Step 4: Run in WSL, expect PASS** for all three, then the full Linux gate. Also run the macOS cross-check (it compiles `dir_unix.rs` for Apple, where `st_mode` is `u16`; `from_raw_mode` takes the platform's raw mode, so no cast is needed).
- [ ] **Step 5: Mutation check.** Temporarily make `is_write_protected_at` return `false` unconditionally; `rename_replace_refuses_a_read_only_target` must go RED in WSL. Revert.
- [ ] **Step 6: Commit** — `fix(flux-platform): the handle rename_replace refuses a read-only target on POSIX`.

---

### Task 4: Windows — measure, then guard the handle `rename_replace`

Whether the NT rename (`rename_at`, `crates/flux-platform/src/dir_windows.rs:513-619`, `ReplaceIfExists = true`) refuses a read-only or ACL-protected target by itself is **unknown and must be measured**. The path guard's comments record that `MoveFileExW` REPLACED an ACL-protected file (`std_fs.rs:106-111`), and `rename_at` maps every `NtSetInformationFile` failure to `Code::IoError` (`:614-616`), not `PermissionDenied`.

**Files:**
- Test: `crates/flux-platform/tests/dir_handle.rs`, inside `mod windows_arm` (starts line 294)
- Modify (only if Step 2 requires it): `crates/flux-platform/src/dir_windows.rs:211-215` and a new helper

- [ ] **Step 0: Verify state.** `dir_windows.rs:211-215`:

```rust
    fn rename_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        crate::dir_windows::rename_at(&self.0, from, &other.0, to, true)
    }
```

- [ ] **Step 1: Write the tests** — append inside `mod windows_arm`. They mirror `std_fs.rs:30` and `:277` and REQUIRE the same outcome as the path version (`PermissionDenied`, contents survive):

```rust
    #[test]
    fn rename_replace_refuses_a_read_only_target() {
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("from")).unwrap());
        let to = d.path().join("to");
        std::fs::write(&to, b"protected").unwrap();
        let mut perms = std::fs::metadata(&to).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&to, perms).unwrap();

        let err = root.rename_replace(OsStr::new("from"), &root, OsStr::new("to")).unwrap_err();

        assert_eq!(err.code, flux_fs::Code::PermissionDenied);
        assert!(err.source.raw_os_error().is_none(), "the guard must refuse, not the OS");
        assert_eq!(std::fs::read(&to).unwrap(), b"protected", "the protected file must survive");
        std::fs::remove_file(&to).expect("a read-only file must still be removable");
    }

    #[test]
    fn rename_replace_refuses_a_target_denied_by_acl() {
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("from")).unwrap());
        let to = d.path().join("to");
        std::fs::write(&to, b"protected").unwrap();
        let user = std::env::var("USERNAME").expect("USERNAME");
        let denied = std::process::Command::new("icacls")
            .args([&to.display().to_string(), "/deny", &format!("{user}:(W,D,DC)")])
            .output()
            .is_ok_and(|o| o.status.success());
        if !denied {
            eprintln!("skipped: could not apply a deny ACE");
            return;
        }
        assert!(!std::fs::metadata(&to).unwrap().permissions().readonly(), "attribute is unset");

        let refused = root.rename_replace(OsStr::new("from"), &root, OsStr::new("to")).is_err();

        let _ = std::process::Command::new("icacls")
            .args([&to.display().to_string(), "/remove:d", &user])
            .output();
        let content = std::fs::read(&to).expect("the destination must still be readable");
        assert!(refused, "a destination we may not write must be refused");
        assert_eq!(content, b"protected", "the protected contents must survive");
    }
```

- [ ] **Step 2: MEASURE on Windows before implementing.** `cargo nextest run -p flux-platform --no-fail-fast -E 'test(rename_replace_refuses)'` and record, per test, PASS or FAIL and the failing assertion. Expected possibilities: the NT rename refuses natively (then the read-only test fails only on the `raw_os_error().is_none()` or `code` assertion) or replaces the file (then the content assertion fails). Paste the output into the commit message of Step 4.
- [ ] **Step 3: Implement the guard** (required whenever Step 2 shows any FAIL — which the `raw_os_error().is_none()` assertion makes certain unless an unexpected guard already exists). Add, in `dir_windows.rs` directly above `fn rename_at`:

```rust
/// The handle-relative twin of `destination_is_write_protected` in `std_fs.rs`
/// (Windows half), asking the same two questions of a NAME inside `p`.
///
/// Both halves, because the path version MEASURED that neither alone is enough: the
/// read-only ATTRIBUTE is set while a DELETE-access open succeeds, and an ACL denying
/// (W,D,DC) leaves the attribute unset while the DELETE open fails with ACCESS_DENIED.
/// DELETE, not write, because rename needs DELETE on the target, and a write-intent
/// open would ask a cloud-sync filter to hydrate an offline file. Only
/// STATUS_ACCESS_DENIED counts: a sharing violation is someone else holding the file,
/// not a protection, and the rename reports it itself. A symlink or junction is
/// replaced as a name, never followed, so its target is never consulted.
fn is_write_protected_at(p: &OwnedHandle, n: &OsStr) -> bool {
    match metadata_at(p, n) {
        Err(_) => false,
        Ok(m) if m.file_type == flux_fs::FileType::Symlink => false,
        Ok(m) => {
            m.permissions == Some(flux_fs::Perms::ReadOnly(true)) || delete_access_denied_at(p, n)
        }
    }
}

/// Whether a handle-relative open of `n` for DELETE is refused with ACCESS_DENIED.
/// Same object attributes and share mode as `remove_file_at`'s open, and
/// FILE_OPEN_REPARSE_POINT so a link is probed as the link.
fn delete_access_denied_at(p: &OwnedHandle, n: &OsStr) -> bool {
    let mut wide: Vec<u16> = n.encode_wide().collect();
    let bytes = (wide.len() * 2) as u16;
    let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
    let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
    oa.RootDirectory = p.as_raw_handle() as HANDLE;
    oa.ObjectName = &raw const us;
    let mut raw: HANDLE = std::ptr::null_mut();
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is to a live local that outlives the call, and `wide`
    // outlives `us` which borrows it.
    let status = unsafe {
        NtCreateFile(
            &raw mut raw,
            DELETE | SYNCHRONIZE,
            &raw const oa,
            &raw mut iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if status == 0 {
        // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `raw` is a handle we own.
        drop(unsafe { OwnedHandle::from_raw_handle(raw as _) });
        return false;
    }
    status == STATUS_ACCESS_DENIED
}
```

and change `rename_replace` (lines 211-215) to:

```rust
    fn rename_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        // The TARGET lives in `other`. See `is_write_protected_at`.
        if crate::dir_windows::is_write_protected_at(&other.0, to) {
            return Err(FsError::new(
                Code::PermissionDenied,
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "destination is read-only"),
            ));
        }
        crate::dir_windows::rename_at(&self.0, from, &other.0, to, true)
    }
```

Every identifier used above already exists in `dir_windows.rs` (`metadata_at` :355, `NtCreateFile`, `OBJECT_ATTRIBUTES`, `UNICODE_STRING`, `IO_STATUS_BLOCK`, `DELETE`, `SYNCHRONIZE`, the `FILE_SHARE_*` flags, `FILE_OPEN`, `FILE_OPEN_REPARSE_POINT`, `STATUS_ACCESS_DENIED`, `OwnedHandle`, `HANDLE`, `encode_wide`); if the compiler reports any as unresolved, STOP and report `STATE_MISMATCH` rather than importing a substitute.
- [ ] **Step 4: Run, expect PASS** on Windows (`just check`). Mutation: make `is_write_protected_at` return `false`; the read-only test must go RED. Revert.
- [ ] **Step 5: Commit** — `fix(flux-platform): the handle rename_replace refuses a read-only target on Windows`, with Step 2's measured output in the body.

---

### Task 5: `copy_file_at` and the wrapper (behaviour-preserving)

Move the algorithm onto a directory handle. No new behaviour: every existing `copy_file` test must pass UNMODIFIED (they are the oracle — in particular `a_self_copy_is_refused_before_anything_is_touched` (empty call list), `a_leftover_from_this_invocation_is_swept_first` (first call is `remove_file(/dst.flux-partial.op1)`), and `a_temporary_that_cannot_be_removed_is_named_in_the_error` (leftover path `/dst.flux-partial.op1`)). The handle methods of `FakeDirHandle` delegate to the path methods with the joined path (`fault_fs.rs:848-898`), so the call log strings are unchanged.

**Files:**
- Modify: `crates/flux-core/src/copy.rs:1-246`
- Modify: `crates/flux-core/src/lib.rs:13`

- [ ] **Step 0: Verify state.** `copy.rs:122-127` is the `copy_file` signature over `F: FileSystem`; `discard` is at `:97-120` and takes `fs: &F, temp: &Path`; `lib.rs:13` is `pub use copy::copy_file;`.
- [ ] **Step 1: Write the new tests** (append inside `mod tests` in `copy.rs`):

```rust
    #[test]
    fn split_destination_separates_parent_and_name() {
        let ok = |d: &str| {
            let (p, n) = split_destination(Path::new(d)).unwrap();
            (p.to_path_buf(), n.to_os_string())
        };
        assert_eq!(ok("/d/b"), (Path::new("/d").into(), "b".into()));
        // A bare name's parent is the EMPTY path; `destination_root("")` would fail.
        assert_eq!(ok("b"), (Path::new(".").into(), "b".into()));
        assert_eq!(ok("/b"), (Path::new("/").into(), "b".into()));
    }

    #[test]
    fn a_destination_naming_no_file_is_refused_before_any_call() {
        for dst in ["/", "/d/.."] {
            let fs = FaultFs::new();
            fs.write_file("/src", b"hello");
            let err = copy_file(&fs, Path::new("/src"), Path::new(dst), &opts()).unwrap_err();
            assert_eq!(err.code(), flux_fs::Code::DestinationError, "{dst}");
            assert!(fs.calls().is_empty(), "{dst}: refuse before any call; got {:?}", fs.calls());
        }
    }

    #[test]
    fn copy_file_at_writes_into_the_directory_it_is_given() {
        use flux_fs::DestinationRoot;
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();

        let out = copy_file_at(&fs, Path::new("/src"), &d, std::ffi::OsStr::new("f"), &opts())
            .unwrap();

        assert_eq!(out.bytes_copied, 5);
        assert_eq!(fs.read_file("/d/f").as_deref(), Some(&b"hello"[..]));
        assert!(!fs.exists("/d/f.flux-partial.op1"));
    }

    #[test]
    fn copy_file_at_reports_a_leftover_relative_to_its_directory() {
        use flux_fs::DestinationRoot;
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        fs.fail("rename_replace", flux_fs::Code::PermissionDenied);
        fs.fail_always("remove_file", flux_fs::Code::PermissionDenied);

        let err = copy_file_at(&fs, Path::new("/src"), &d, std::ffi::OsStr::new("f"), &opts())
            .unwrap_err();

        let (path, _) = err.leftover.as_ref().expect("the leftover must be reported");
        assert_eq!(path, Path::new("f.flux-partial.op1"), "relative to the directory handle");
    }
```

- [ ] **Step 2: Run, expect FAIL to compile** (`split_destination` and `copy_file_at` do not exist).
- [ ] **Step 3: Implement.** Make these edits to `copy.rs`, in order:

(a) Imports — replace lines 3-8 with:

```rust
use flux_fs::{
    Code, CopyOptions, DestinationRoot, DirHandle, Durability, FileHandle, FileSystem, FileType,
    FsError, MetadataFailure, MetadataItem, OperationId, Outcome, Preserve, Publish, temp_path,
};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::Path;
```

(b) `discard` — replace the whole function (lines 92-120, doc comment included) with the handle form. The body is identical except that it removes through `parent` and records the leftover RELATIVE to it:

```rust
/// Remove the temporary after a failure and build the error to return.
///
/// The removal is never discarded: if the temporary survives, its path goes into the
/// error, because a leftover the caller is never told about is a leak the user cannot
/// even find. Only called where the temporary is known to exist.
///
/// The leftover is recorded RELATIVE to `parent` -- the only frame `copy_file_at`
/// has. `copy_file` joins it onto the parent path it resolved, so its callers see
/// the same full path as before.
fn discard<D: DirHandle>(parent: &D, temp: &OsStr, code: Code, source: std::io::Error) -> CopyError {
    match parent.remove_file(temp) {
        Ok(()) => CopyError::new(FsError::new(code, source)),
        // NotFound means it is already gone -- something else removed it, or it was
        // never created. Reporting a leftover here would send someone hunting a file
        // that does not exist.
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => {
            CopyError::new(FsError::new(code, source))
        }
        // Carry the removal's own error too, in `leftover` and NOT over `source`:
        // wrapping the primary failure in `Error::other(format!(..))` destroyed
        // `raw_os_error()` and, MEASURED on Linux, turned `classify` from `DiskFull`
        // into `IoError`.
        Err(why) => CopyError {
            cause: FsError::new(code, source),
            leftover: Some((std::path::PathBuf::from(temp), why.source)),
        },
    }
}

/// `<name>.flux-partial.<operation-id>` (§18.1), as a single component for a handle.
/// Built by `temp_path` so the normative name has exactly one definition.
fn temp_name(name: &OsStr, id: &OperationId) -> OsString {
    temp_path(Path::new(name), id).into_os_string()
}

/// Split `dst` into the directory to open and the one component to write there.
///
/// A path with no file name -- a root, or one ending in `..` -- names nothing to
/// write, so it is a destination error. A bare name's parent is the EMPTY path,
/// which names the working directory but which `destination_root` cannot open, so
/// it becomes `.`.
fn split_destination(dst: &Path) -> std::result::Result<(&Path, &OsStr), CopyError> {
    let Some(name) = dst.file_name() else {
        return Err(CopyError::new(FsError::new(
            Code::DestinationError,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "the destination names no file"),
        )));
    };
    let parent = match dst.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    Ok((parent, name))
}
```

(c) Replace `pub fn copy_file` (lines 122-246) with the wrapper followed by `copy_file_at`:

```rust
/// Copy one file by path. A wrapper: it resolves the destination's PARENT once, as a
/// directory handle, and hands the algorithm to `copy_file_at`, so a single-file copy
/// writes through a handle exactly as a tree copy does (§149.7).
pub fn copy_file<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    dst: &Path,
    opts: &CopyOptions,
) -> std::result::Result<Outcome, CopyError> {
    // 0. refuse a LEXICAL self-copy BEFORE touching the filesystem. This must precede
    //    every call below, because `a_self_copy_is_refused_before_anything_is_touched`
    //    asserts the recorded call list is empty. The IDENTITY half of the same
    //    refusal -- `flux copy a b` where `b` is `a` by another name -- is Step 2a in
    //    `copy_file_at`, which has to stat the destination and so cannot keep this
    //    step's promise; the two coexist rather than one replacing the other.
    if src == dst {
        return Err(CopyError::new(FsError::new(
            Code::SafetyRejected,
            std::io::Error::other("source and destination are the same path"),
        )));
    }
    let (parent_path, name) = split_destination(dst)?;
    // The one path-based call on the write side. §149.7 exempts resolving the
    // destination itself: it may follow links, because the user named it.
    let parent = fs.destination_root(parent_path).map_err(CopyError::new)?;
    copy_file_at(fs, src, &parent, name, opts).map_err(|mut e| {
        // Rebuilt from `dst`, NOT joined onto `parent_path`: for a bare name the
        // opened parent is a synthesized `.`, and joining would report
        // `./b.flux-partial.x` where the user's own spelling gives `b.flux-partial.x`.
        if let Some((path, _)) = e.leftover.as_mut() {
            *path = dst.with_file_name(&*path);
        }
        e
    })
}

/// Copy `src` to `name` inside `parent`, writing ONLY through `parent`.
///
/// Nothing below re-resolves a destination path: the staging temporary, the
/// metadata, the publish and the cleanup all go through the handle, so a parent
/// swapped for a link after it was opened cannot redirect the write (item 114).
pub fn copy_file_at<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    parent: &F::Dir,
    name: &OsStr,
    opts: &CopyOptions,
) -> std::result::Result<Outcome, CopyError> {
    let temp = temp_name(name, &opts.operation_id);

    // 1. this invocation's own leftover, if any (§18.1). A no-op in practice: the
    //    operation id is generated per invocation and never persisted, so nothing from
    //    an earlier run carries this name. It stays because §18.1 asks the id to be
    //    "deterministic enough for discovery", and a derivable id makes this live.
    let _ = parent.remove_file(&temp);

    // 2. source, captured for the step-7 re-check
    // Safe to `?`: nothing has been created yet, so there is nothing to leak.
    let src_meta = fs.metadata(src).map_err(CopyError::new)?;
    if src_meta.file_type != FileType::File {
        return Err(CopyError::new(FsError::new(
            Code::SpecialFileUnsupported,
            std::io::Error::other("not a regular file"),
        )));
    }
    let mut reader = fs.open_read(src).map_err(CopyError::new)?;

    // 3. exclusive create (FS-1)
    // Still safe: if this FAILS, this call is precisely what did not create the
    // temporary, so there is nothing of ours on disk.
    let mut writer = parent.create_new(&temp).map_err(CopyError::new)?;

    // 4. stream
    let mut bytes_copied = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match std::io::Read::read(&mut reader, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(discard(parent, &temp, copy_code(&e), e)),
        };
        if let Err(e) = writer.write_all(&buf[..n]) {
            return Err(discard(parent, &temp, copy_code(&e), e));
        }
        bytes_copied += n as u64;
    }

    // 5. durability
    if opts.durability == Durability::Strict
        && let Err(e) = writer.sync_all()
    {
        return Err(discard(parent, &temp, Code::StrictDurabilityUnavailable, e.source));
    }

    // 6. metadata, on the temporary, BEFORE publication (§44.1)
    let mut metadata_failures = Vec::new();

    if opts.preserve_times != Preserve::Off
        && let Err(e) = fs.set_times(&writer, src_meta.modified)
    {
        if opts.preserve_times == Preserve::Strict {
            return Err(discard(parent, &temp, Code::MetadataApplyFailed, e.source));
        }
        metadata_failures.push(MetadataFailure { item: MetadataItem::Times, error: e });
    }

    if opts.preserve_permissions != Preserve::Off
        && let Err(e) = fs.set_permissions(&writer, src_meta.permissions)
    {
        if opts.preserve_permissions == Preserve::Strict {
            return Err(discard(parent, &temp, Code::MetadataApplyFailed, e.source));
        }
        metadata_failures.push(MetadataFailure { item: MetadataItem::Permissions, error: e });
    }

    // 7. the source must not have changed under us (Section 33), then publish.
    //    Note the `match` rather than `?`: a `?` here would return with the temporary
    //    still on disk, which is the one leak the `discard` helper exists to prevent.
    let now = match fs.metadata(src) {
        Ok(m) => m,
        // Gone is the most complete form of "changed under us", and the taxonomy has
        // a code for that. Reporting IO_ERROR here would hide it among unrelated
        // failures.
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => {
            let gone = std::io::Error::other("source disappeared during the copy");
            return Err(discard(parent, &temp, Code::SourceChanged, gone));
        }
        Err(e) => return Err(discard(parent, &temp, e.code, e.source)),
    };
    if now.len != src_meta.len || now.modified != src_meta.modified {
        let changed = std::io::Error::other("source changed");
        return Err(discard(parent, &temp, Code::SourceChanged, changed));
    }

    let published = match opts.publish {
        Publish::Replace => parent.rename_replace(&temp, parent, name),
        Publish::NoReplace => parent.rename_no_replace(&temp, parent, name),
    };
    if let Err(e) = published {
        // Keep whatever the platform layer mapped. A publish failure is NOT the content
        // copy -- that already succeeded -- so it must not be relabelled COPY_FAILED,
        // and an unclassified one stays IO_ERROR, the declared catch-all.
        return Err(discard(parent, &temp, e.code, e.source));
    }

    // A successful rename consumed the temporary; there is nothing left to remove.

    Ok(Outcome { bytes_copied, metadata_failures, identity_degraded: None })
}
```

(d) `lib.rs:13` becomes `pub use copy::{copy_file, copy_file_at};`.

- [ ] **Step 4: Run, expect PASS.** `cargo nextest run -p flux-core --no-fail-fast` → every pre-existing test plus the four new ones pass, NO existing test edited. Then `just check`, the WSL gate, and the macOS cross-check.
- [ ] **Step 5: Commit** — `refactor(flux-core): copy_file writes through a destination handle (copy_file_at)`.

---

### Task 6: the Step 2a destination identity gate

Semantics: walker design "What the gate does, case by case" and "The degraded case for files, which fails OPEN". The rows are evaluated IN ORDER and `NotFound` short-circuits first, so `Safety::Strict` never refuses a fresh destination. Placement: after the source type check, before `open_read` and before the staging temporary exists, so a refusal changes nothing on disk.

**Files:**
- Modify: `crates/flux-core/src/copy.rs` (imports, a new `identity_gate` fn, the call in `copy_file_at`, the final `Ok(Outcome ..)`)
- Test: `crates/flux-core/src/copy.rs`, `mod tests`

- [ ] **Step 0: Verify state.** Task 5 is committed; `copy_file_at` ends `Ok(Outcome { bytes_copied, metadata_failures, identity_degraded: None })`.
- [ ] **Step 1: Write the failing tests** (append inside `mod tests`). `FaultFs` mints a distinct `Strong` identity per path unless `set_identity` overrides it (`fault_fs.rs:144-152`, `:348-352`); the gate's destination stat is the SECOND global `metadata` call (source, gate, source re-check), which is what `fail_nth("metadata", 2, ..)` targets (`fault_fs.rs:293-297`).

```rust
    fn strong(index: u128) -> flux_fs::FileIdentity {
        flux_fs::FileIdentity::Strong(flux_fs::ObjectId { volume: 1, index })
    }
    fn weak(volume: u64) -> flux_fs::FileIdentity {
        flux_fs::FileIdentity::Weak(flux_fs::ObjectId { volume, index: 1 })
    }
    fn strict() -> CopyOptions {
        let mut o = opts();
        o.safety = flux_fs::Safety::Strict;
        o
    }

    #[test]
    fn a_destination_that_is_the_source_by_identity_is_refused_before_anything_is_created() {
        // The hardlink case (§129, invariant 22): two names, one object. Nothing may
        // be created, and the destination -- which IS the source -- stays intact.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.write_file("/dst", b"hello");
        fs.set_identity("/src", strong(4242));
        fs.set_identity("/dst", strong(4242));

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert!(!fs.called("open_read"), "refused before the source is opened");
        assert!(!fs.called("create_new"), "refused before the temporary exists");
        assert!(!fs.called("rename_replace"));
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn a_fresh_destination_passes_silently_even_under_strict_with_a_weak_source() {
        // NotFound short-circuits FIRST: no object, so no alias. Without the ordering,
        // strict would refuse every copy onto removable media (a weak source).
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.set_identity("/src", weak(9));

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &strict()).unwrap();

        assert_eq!(out.identity_degraded, None, "absent is not a degradation");
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn a_directory_at_the_destination_is_refused_with_the_real_reason() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.create_dir(Path::new("/dst")).unwrap();

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert!(err.to_string().contains("directory"), "names the reason: {err}");
        assert!(!fs.called("create_new"));
    }

    #[test]
    fn a_distinct_strong_destination_is_replaced_without_complaint() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"old");

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &strict()).unwrap();

        assert_eq!(out.identity_degraded, None);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"new"[..]));
    }

    #[test]
    fn a_weak_destination_degrades_and_reports_the_weaker_side() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"old");
        fs.set_identity("/dst", weak(7));

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.identity_degraded, Some(weak(7)));
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"new"[..]), "default still copies");
    }

    #[test]
    fn a_weak_destination_is_refused_under_strict_and_left_untouched() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"old");
        fs.set_identity("/dst", weak(7));

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &strict()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert!(!fs.called("create_new"));
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"old"[..]));
    }

    #[test]
    fn an_unavailable_side_degrades_to_the_unavailable_bucket() {
        // Unavailable outranks Weak as the weaker side: it names no volume at all.
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"old");
        fs.set_identity("/src", flux_fs::FileIdentity::Unavailable);
        fs.set_identity("/dst", weak(7));

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.identity_degraded, Some(flux_fs::FileIdentity::Unavailable));
    }

    #[test]
    fn a_destination_that_cannot_be_inspected_is_a_degraded_comparison() {
        // A stat that fails for a reason other than NotFound is a comparison that did
        // not happen -- degrade (Unavailable) by default, refuse under strict.
        let lax = FaultFs::new();
        lax.write_file("/src", b"new");
        lax.fail_nth("metadata", 2, flux_fs::Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        let out = copy_file(&lax, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();
        assert_eq!(out.identity_degraded, Some(flux_fs::FileIdentity::Unavailable));

        let tight = FaultFs::new();
        tight.write_file("/src", b"new");
        tight.fail_nth("metadata", 2, flux_fs::Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        let err = copy_file(&tight, Path::new("/src"), Path::new("/dst"), &strict()).unwrap_err();
        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert!(!tight.called("create_new"));
    }

    #[test]
    fn a_symlink_at_the_destination_is_judged_as_the_name_and_replaced() {
        // The gate stats the NAME (DirHandle::metadata never follows): a link is its
        // own object, distinct from the source, so it is replaced as intended.
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.add_symlink("/dst");

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.identity_degraded, None);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"new"[..]));
    }
```

Also add one assertion to the existing `a_clean_copy_publishes_and_reports_nothing` test, after `assert!(out.metadata_failures.is_empty());`: `assert_eq!(out.identity_degraded, None);` — this is the one permitted edit to an existing test: it ADDS an assertion and removes none.

- [ ] **Step 2: Run, expect FAIL** for the identity, directory, weak-under-strict, unavailable and cannot-be-inspected tests (the gate does not exist yet).
- [ ] **Step 3: Implement.** Add `FileIdentity, Metadata, Safety` to the `flux_fs` import list in `copy.rs`. Add these two functions above `pub fn copy_file`:

```rust
/// Step 2a (§129, foundational invariant 22): refuse a destination that IS the source.
///
/// Evaluated IN ORDER, and the order is load-bearing: `NotFound` short-circuits first,
/// because a destination that does not exist has no identity to alias, so neither
/// `Safety` setting may refuse it. Every other row concerns a destination that exists.
///
/// | destination            | result                                              |
/// |------------------------|-----------------------------------------------------|
/// | `NotFound`             | proceed, not degraded                               |
/// | other stat failure     | degraded `Unavailable` (strict: refuse)             |
/// | a directory            | refuse, naming it                                   |
/// | both `Strong`, equal   | refuse                                              |
/// | both `Strong`, differ  | proceed                                             |
/// | either side not Strong | degraded, the weaker side (strict: refuse)          |
///
/// The stat goes through the HANDLE and never follows a link, so a symlink at the
/// destination is judged as the NAME being replaced, never as its target.
///
/// Residual, stated rather than hidden: this stats at Step 2a and publishes at Step 7,
/// so a destination that becomes an alias in between is not caught. The harm is
/// bounded to a broken hardlink, not lost data -- the published bytes were read from
/// the source before the destination was touched.
fn identity_gate<D: DirHandle>(
    parent: &D,
    name: &OsStr,
    src: &Metadata,
    safety: Safety,
) -> std::result::Result<Option<FileIdentity>, CopyError> {
    let refuse = |why: &'static str| {
        CopyError::new(FsError::new(Code::SafetyRejected, std::io::Error::other(why)))
    };
    let degraded = |weaker: FileIdentity| match safety {
        Safety::Default => Ok(Some(weaker)),
        Safety::Strict => Err(refuse(
            "the destination exists and its identity cannot be compared with full confidence",
        )),
    };
    match parent.metadata(name) {
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => degraded(FileIdentity::Unavailable),
        Ok(m) if m.file_type == FileType::Dir => Err(refuse("the destination is a directory")),
        Ok(m) => match (src.identity, m.identity) {
            (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b => {
                Err(refuse("the destination is the source itself, by identity"))
            }
            (FileIdentity::Strong(_), FileIdentity::Strong(_)) => Ok(None),
            (s, d) => degraded(weaker(s, d)),
        },
    }
}

/// The less trustworthy of two identities, for the warning's aggregation key.
/// `Unavailable` is weaker than `Weak`; between two `Weak`, the DESTINATION's volume
/// keys it, because that is the side the user did not name (walker design,
/// "Stand-downs").
fn weaker(src: FileIdentity, dst: FileIdentity) -> FileIdentity {
    match (src, dst) {
        (FileIdentity::Unavailable, _) | (_, FileIdentity::Unavailable) => FileIdentity::Unavailable,
        (_, d @ FileIdentity::Weak(_)) => d,
        (s, _) => s,
    }
}
```

In `copy_file_at`, insert between the type-check `if` block and `let mut reader = ...`:

```rust
    // 2a. the destination must not BE the source. Before the temporary exists, so a
    //     refusal changes nothing on disk.
    let identity_degraded = identity_gate(parent, name, &src_meta, opts.safety)?;
```

and change the final line to `Ok(Outcome { bytes_copied, metadata_failures, identity_degraded })`.

- [ ] **Step 4: Run, expect PASS.** `cargo nextest run -p flux-core --no-fail-fast`, then `just check`, the WSL gate and the macOS cross-check.
- [ ] **Step 5: Mutation checks** (each must turn at least one Task 6 test RED; revert after each):
  1. delete the `if a == b` refusal arm;
  2. move the `NotFound` arm below the `Err(_)` arm;
  3. make `degraded` always return `Ok(Some(weaker))`;
  4. make `weaker` return `dst` unconditionally (`an_unavailable_side_degrades_to_the_unavailable_bucket` must go RED: its source is `Unavailable`, its destination `Weak`).
- [ ] **Step 6: Commit** — `feat(flux-core): refuse a copy onto the source itself by identity (Step 2a)`.

---

### Task 7: end-to-end refusals through the real CLI

`FaultFs` proves the gate's logic; this proves a REAL filesystem reports the identity the gate needs, on every CI leg.

**Files:**
- Test: `crates/flux-cli/tests/copy.rs` (append)

- [ ] **Step 0: Verify state.** The file defines `fn flux() -> Command` and two tests, `it_copies_a_single_file` and `a_successful_copy_consumes_its_temporary`.
- [ ] **Step 1: Write the tests:**

```rust
#[test]
fn a_copy_onto_a_hardlink_of_its_own_source_is_refused() {
    // The live defect 4a fixes: two names, one object. Before the gate, the file was
    // copied onto itself.
    let d = TempDir::new().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&a, b"hello").unwrap();
    std::fs::hard_link(&a, &b).unwrap();

    let out = flux().arg("copy").arg(&a).arg(&b).output().unwrap();

    assert!(!out.status.success(), "must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("SAFETY_REJECTED"), "stderr: {stderr}");
    assert_eq!(std::fs::read(&b).unwrap(), b"hello");
    assert_eq!(std::fs::read(&a).unwrap(), b"hello");
}

#[test]
fn a_copy_onto_its_own_source_by_another_spelling_is_refused() {
    // `..` is not normalized away by Path comparison, so the two spellings differ
    // lexically and only identity can tell they are one file.
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("sub")).unwrap();
    let a = d.path().join("a");
    let same = d.path().join("sub").join("..").join("a");
    std::fs::write(&a, b"hello").unwrap();
    assert_ne!(a, same, "the fixture must be lexically different");

    let out = flux().arg("copy").arg(&a).arg(&same).output().unwrap();

    assert!(!out.status.success(), "must be refused");
    assert!(String::from_utf8_lossy(&out.stderr).contains("SAFETY_REJECTED"));
    assert_eq!(std::fs::read(&a).unwrap(), b"hello");
}

#[test]
fn a_copy_between_bare_names_writes_into_the_working_directory() {
    // The one real-filesystem path through `split_destination`'s "empty parent
    // becomes ." rule. `current_dir` sets the CHILD's working directory only, so this
    // is safe under parallel tests.
    let d = TempDir::new().unwrap();
    std::fs::write(d.path().join("a"), b"hello").unwrap();

    let out = flux().current_dir(d.path()).arg("copy").arg("a").arg("b").output().unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(d.path().join("b")).unwrap(), b"hello");
}
```

- [ ] **Step 2: Run** `cargo nextest run -p flux-cli --no-fail-fast` on Windows, then in WSL → PASS on both (Tasks 5 and 6 are in). If either FAILS, STOP and report the stderr — it means a real filesystem does not report the identity the gate relies on, which is a design question, not a test to adjust.
- [ ] **Step 3: Commit** — `test(flux-cli): a copy onto its own source is refused end to end`.

---

### Task 8: bookkeeping

**Files:**
- Modify: `TODO.md`
- Modify: `docs/superpowers/specs/2026-09-23-directory-walker-design.md` (the "Reading this document against merged code" section, starting line 1586)

- [ ] **Step 0: Verify state.** `TODO.md` contains the headings `- [ ] **The self-copy refusal compares paths, not filesystem identity**` and `## Engine (walker cut 4) prerequisites` with the two entries added by PR #46.
- [ ] **Step 1: TODO.md.**
  - Change `- [ ] **The self-copy refusal compares paths, not filesystem identity**` to `- [x]` and append to its body: `DONE in cut 4a: \`copy_file_at\`'s Step 2a gate refuses a destination that is the source by identity; the lexical Step 0 refusal stays in front of it.`
  - Under `## Engine (walker cut 4) prerequisites`, replace the section's intro line with: `Cut 4 is split (see \`docs/superpowers/specs/2026-09-25-cut-4-after-handle-relative-writes.md\`); both entries below are cut 4b's.`
  - Append to the item-113 entry: `DECIDED 2026-09-25 (owner, agy aligned): the probe stays deferred with the workspace; cut 4b aborts the whole operation on the FIRST publish that fails with "primitive unavailable" (Unsupported, or EINVAL/ENOSYS on unix after the temporary was created), exit 1.`
  - Append to the `move_object` entry: `DECIDED 2026-09-25: not needed in cut 4 -- the engine renames only files. Stays debt for the first cut that renames a directory.`
  - Add a new entry at the end of that section: `- [ ] **§42 mount boundaries are not enforced.** The walk descends into a directory on another volume (a mount), and would read \`/proc\` inside a copied tree. Needs a walk-level rule (the walk must not even read the mounted subtree), a \`--cross-filesystems\` option, and a report channel. Its own cut, decided 2026-09-25.`
- [ ] **Step 2: Walker design pointer.** Insert immediately after the heading `## Reading this document against merged code` a paragraph: `**Cut 4 was re-planned after §149.7 merged.** \`2026-09-25-cut-4-after-handle-relative-writes.md\` supersedes this document's item-114 mitigation, its symlinked-anchor refusal, and its single engine cut; read it first.`
- [ ] **Step 3: Verify** `typos` on both files → clean. **Commit** — `docs: record the cut-4 split and close the self-copy identity gap`.

---

### Task 9: final verification (no publishing)

- [ ] **Step 1:** `just check` on Windows → exit 0; record the Summary line.
- [ ] **Step 2:** the WSL gate (clippy + nextest) → exit 0; record the Summary line.
- [ ] **Step 3:** the macOS cross-check → exit 0.
- [ ] **Step 4:** `git status --short` clean; `git log --oneline origin/main..HEAD` lists the Task 1-8 commits.
- [ ] **Step 5: STOP.** Report completion. The AGY-CAPSTONE runs next, then (owner-approved) `just pr`, which arms auto-merge — so nothing is pushed before the capstone is GREEN.

---

## Self-review (done by the plan author)

- **Spec coverage (amendment decisions 1 and 6, walker-design gate semantics):** handle-form algorithm + wrapper → Task 5; root / `..` / bare-name edges → Task 5 tests; read-only guard consequence → Tasks 3-4; `Safety` + `identity_degraded` → Task 2; gate rows (NotFound first, directory, equal, differ, weak, unavailable, uninspectable, symlink-as-name) → Task 6, one test per row; strict never refusing a fresh destination → Task 6; real-filesystem identity → Task 7; TODO / design bookkeeping → Task 8. Decisions 2, 3, 7, 8 are 4b's by decision 6.
- **Placeholders:** none; every code step shows the code. Task 4 branches on a measurement by design, and says what to do on each outcome.
- **Type consistency:** `copy_file_at<F: DestinationRoot>(fs, src, parent: &F::Dir, name: &OsStr, opts)` is used identically in Tasks 5-6; `identity_degraded: Option<FileIdentity>` in Tasks 2 and 6; `Safety::{Default, Strict}` in Tasks 2 and 6.
