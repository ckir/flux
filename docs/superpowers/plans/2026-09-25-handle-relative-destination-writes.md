# Handle-Relative Destination Writes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the writer a directory-handle API so every destination entry is created and opened relative to a handle its parent holds, never by a path the kernel re-resolves.

**Architecture:** `flux-fs` gains two traits and leaves `FileSystem` untouched — `DirHandle` (the open directory) and `DestinationRoot` (the one path-based call, resolving `DEST` at start). `flux-platform` implements them twice: POSIX via `rustix::fs::openat` with `O_NOFOLLOW | O_DIRECTORY`, Windows via `NtCreateFile` with `OBJECT_ATTRIBUTES.RootDirectory`. `flux-core`'s `FaultFs` implements them with synthetic handles. No engine, no CLI.

**Tech Stack:** Rust 2024, MSRV 1.98.1. `rustix` 1.1 (`features = ["fs"]`, already a `cfg(unix)` dependency). `windows-sys` 0.61 — **three features must be added**. `tempfile` for tests.

---

## Before you start: verified ground truth

Every citation was grep-verified, and **every code block in this plan was compile-verified**, against the tree at `acc6edb` on branch `feat/handle-relative-writes` in worktree `E:\Rust\flux-handles`. If any does not match what you see, **STOP and report `STATE_MISMATCH: <what differs>`**.

**The design is `docs/superpowers/specs/2026-09-25-handle-relative-destination-writes-design.md`.** Read it before Task 1. It is the oracle for every behavioural question this plan does not answer, and it records WHY each platform arm differs — which is the part most likely to be "simplified" into a defect.

### The two platforms behave in OPPOSITE ways. This is the whole difficulty.

Assuming symmetry will produce code that compiles, passes the tests you thought to write, and fails to keep the guarantee. Both of these were MEASURED this session, not reasoned:

**POSIX — the open REFUSES.** `openat(dirfd, name, O_RDONLY|O_NOFOLLOW|O_DIRECTORY)` on Linux:

| child | result |
|---|---|
| real directory | opens |
| symlink (in-tree or escaping) | `ENOTDIR` |
| plain file | `ENOTDIR` |
| missing | `ENOENT` |

**It is `ENOTDIR`, not the `ELOOP` that `O_NOFOLLOW` is associated with** — with `O_DIRECTORY` set, Linux reports the type mismatch first. So `ENOTDIR` **conflates a symlink with an ordinary non-directory**, and §149.7 needs those reported differently. One `statat(AT_SYMLINK_NOFOLLOW)` on the **error path only** separates them.

**Windows — the open SUCCEEDS.** `NtCreateFile` handle-relative with `FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT`, measured unelevated:

| child | NTSTATUS | |
|---|---|---|
| real directory | `0x00000000` | SUCCESS |
| plain file | `0xC0000103` | `STATUS_NOT_A_DIRECTORY` |
| **junction** | **`0x00000000`** | **SUCCESS — it opens** |
| missing | `0xC0000034` | `STATUS_OBJECT_NAME_NOT_FOUND` |

`FILE_OPEN_REPARSE_POINT` means *"open the reparse point itself rather than following it"*. So Windows **cannot rely on the open failing** and must open-then-inspect.

### Reject only NAME SURROGATES on Windows, never the reparse-point bit

An adversarial panel caught this as blocking, and it verified on the development machine: **14 reparse points exist under `C:\` and the user profile, and three — `OneDrive`, `Dropbox`, `MagentaCLOUD` — are cloud-sync placeholders that are neither junctions nor symlinks.** Rejecting on `FILE_ATTRIBUTE_REPARSE_POINT` refuses to write into the user's OneDrive folder.

| object | tag | `tag & 0x20000000` |
|---|---|---|
| `OneDrive` | `0x9000701A` | **0 — not a surrogate, TRAVERSE** |
| `Recent` (junction) | `0xA0000003` | **set — surrogate, REJECT** |

The test is `IsReparseTagNameSurrogate(tag)`, i.e. `tag & 0x20000000`, read through `FileAttributeTagInformation`. Prefer it over naming `IO_REPARSE_TAG_SYMLINK`/`MOUNT_POINT`, because it also covers surrogate tags that do not exist yet.

### Line citations, verified

| citation | what is there |
|---|---|
| `crates/flux-fs/src/fs.rs` | 334 lines; `pub trait FileSystem` at `:106`, closing `}` at `:188`; `#[cfg(test)] mod tests` at `:190` with `NullFs` inside it |
| `crates/flux-fs/src/lib.rs` | re-exports a NAMED list from `fs` — new public types must be added to it or they are invisible |
| `crates/flux-platform/src/std_fs.rs` | imports `flux_fs` at `:8` |
| `crates/flux-core/src/fault_fs.rs` | `struct Inner` at `:15`, `pub struct FaultFs` at `:147`, `impl FileSystem for FaultFs` at `:385` |
| `crates/flux-platform/tests/std_fs.rs` | `make_dir_reparse_point` at `:427`; `make_dir_link` is cfg-split at `:549` and `:554` |

**LINE NUMBERS DRIFT AS TASKS LAND.** Locate every target by NAME and use the range only to confirm you found the right one.

### The gate, and the two runners

`just check` = `cargo fmt --check` → `cargo clippy --workspace --all-targets -- -D warnings` → `typos` → `cargo nextest run --workspace --no-tests=pass` → `cargo test --doc --workspace`. Verified at `justfile:46`.

**`just check` runs on WINDOWS ONLY here.** Every task in this plan touches `#[cfg]` code, so every one is cross-checked:

```
wsl -e bash -lc 'cd /mnt/e/Rust/flux-handles && cargo nextest run --workspace --no-tests=pass'
```

**Use `cargo nextest run --no-fail-fast` for every filtered run, never `cargo test`.** Two reasons, both measured in this repository: a `cargo test` filter matching nothing exits 0, and **nextest fail-fasts by default**, so a run that prints `5/9 tests run` was CANCELLED and cannot support "and nothing else went red". Read the count, not the colour.

**Never run `just model`.** TLC is CI-only.

### Baseline

`171 tests run: 171 passed, 2 skipped` on Windows; `171 passed, 1 skipped` under WSL. Record what you actually observe and use the delta.

---

## File structure

| File | Responsibility | Change |
|---|---|---|
| `crates/flux-fs/src/fs.rs` | the portable trait surface | Modify: add `DirHandle` + `DestinationRoot` before the test module |
| `crates/flux-fs/src/lib.rs` | the crate's public names | Modify: re-export both traits |
| `crates/flux-fs/src/name.rs` | **Create**: one-component name validation, shared by all three implementors |
| `crates/flux-platform/src/dir_unix.rs` | **Create**: the POSIX `DirHandle` |
| `crates/flux-platform/src/dir_windows.rs` | **Create**: the Windows `DirHandle` |
| `crates/flux-platform/src/lib.rs` | crate root | Modify: declare the two cfg-gated modules |
| `crates/flux-platform/src/std_fs.rs` | the real adapter | Modify: `impl DestinationRoot for StdFileSystem` |
| `crates/flux-platform/tests/dir_handle.rs` | **Create**: adapter behaviour against a real filesystem |
| `crates/flux-core/src/fault_fs.rs` | the in-memory fake | Modify: synthetic handle + both impls |
| `Cargo.toml` | workspace deps | Modify: three `windows-sys` features |

**The two platform arms get their own files rather than `#[cfg]` blocks inside `std_fs.rs`.** That file is already 480+ lines and mixes four cfg styles; adding two more implementations inline makes the Windows open-then-inspect sequence — the subtlest code in this cut — unreadable. Files that differ per platform are the one case where splitting by platform IS splitting by responsibility.

---

### Task 1: The traits, the exports, and name validation

**Files:**
- Create: `crates/flux-fs/src/name.rs`
- Modify: `crates/flux-fs/src/fs.rs` (insert before `#[cfg(test)] mod tests`), `crates/flux-fs/src/lib.rs`

Nothing implements these yet. The task ends with a crate that compiles and a validated helper both platform arms will use.

- [ ] **Step 0: Record the base commit**

```bash
git tag -f itemb-base HEAD
git rev-parse --short itemb-base
```

A git tag, not a shell variable: under subagent-driven execution each task runs in its own shell, so a variable is empty by Task 5 and `git diff --stat ..HEAD` silently compares nothing against nothing. Task 5 diffs against this tag and deletes it. Expected: `acc6edb`.

- [ ] **Step 1: Verify the state matches this plan**

Confirm `crates/flux-fs/src/fs.rs` is 334 lines, that `pub trait FileSystem: Send + Sync {` is at `:106`, and that `#[cfg(test)]` is at `:190`:

```bash
wc -l crates/flux-fs/src/fs.rs
rg -n '^pub trait FileSystem|^#\[cfg\(test\)\]' crates/flux-fs/src/fs.rs
```

If it differs, STOP and report `STATE_MISMATCH: <what differs>`.

- [ ] **Step 2: Write the failing test for name validation**

Create `crates/flux-fs/src/name.rs` containing ONLY this test module for now:

```rust
//! One-component name validation, shared by every `DirHandle` implementation.

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn a_plain_component_is_accepted() {
        assert!(check_component(OsStr::new("payload.txt")).is_ok());
        assert!(check_component(OsStr::new("a file with spaces")).is_ok());
    }

    #[test]
    fn a_separator_is_refused_before_any_syscall() {
        // THE reason this helper exists. A handle-relative call takes ONE component;
        // a caller that slips "a/b" through would hand the middle component back to
        // kernel path resolution, which is the hole §149.7 closes.
        for bad in ["a/b", "a\\b", "/abs", "nested/deep/path"] {
            let err = check_component(OsStr::new(bad)).unwrap_err();
            assert_eq!(err.code, crate::Code::SafetyRejected, "{bad} must be refused");
        }
    }

    #[test]
    fn dot_and_dotdot_are_refused() {
        // `..` escapes the parent, which is the whole point of holding a handle to it.
        for bad in [".", ".."] {
            let err = check_component(OsStr::new(bad)).unwrap_err();
            assert_eq!(err.code, crate::Code::SafetyRejected, "{bad} must be refused");
        }
    }

    #[test]
    fn an_empty_name_is_refused() {
        let err = check_component(OsStr::new("")).unwrap_err();
        assert_eq!(err.code, crate::Code::SafetyRejected);
    }
}
```

- [ ] **Step 3: Run it to verify it fails**

Add `pub mod name;` to `crates/flux-fs/src/lib.rs` immediately after the `pub mod fs;` line, then:

Run: `cargo nextest run -p flux-fs --no-fail-fast -E 'test(check_component) + test(a_separator_is_refused) + test(dot_and_dotdot) + test(an_empty_name)'`

Expected: FAIL to COMPILE with **`error[E0425]`** naming `check_component` as not found. Match on the error CODE, not rustc's sentence, which moves across releases.

- [ ] **Step 4: Implement the helper**

Insert at the TOP of `crates/flux-fs/src/name.rs`, above the test module:

```rust
use crate::{Code, FsError, Result};
use std::ffi::OsStr;

/// Refuse anything that is not exactly one path component, BEFORE any syscall.
///
/// A handle-relative call takes one component by construction. `OsStr` cannot
/// enforce that, so this does: a caller that passes `"a/b"` would otherwise hand
/// the middle component back to kernel path resolution, which is precisely the
/// resolution §149.7 exists to remove.
///
/// Both separators are rejected on every platform, not just the native one. A
/// name carrying a backslash is not a valid single component on Windows, and on
/// POSIX it is a legal filename that this project has no reason to create and
/// every reason to refuse from a caller that thought it was a separator.
pub fn check_component(name: &OsStr) -> Result<()> {
    let refuse = |why: &str| {
        Err(FsError::new(
            Code::SafetyRejected,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, why.to_string()),
        ))
    };

    let Some(s) = name.to_str() else {
        // A non-UTF-8 name is fine as a NAME; it just cannot be scanned as `str`.
        // Fall back to the byte view, which is enough to find separators.
        return check_component_bytes(name);
    };

    if s.is_empty() {
        return refuse("an empty name is not a path component");
    }
    if s == "." || s == ".." {
        return refuse("`.` and `..` are not publishable components");
    }
    if s.contains('/') || s.contains('\\') {
        return refuse("a path component may not contain a separator");
    }
    Ok(())
}

#[cfg(unix)]
fn check_component_bytes(name: &OsStr) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let b = name.as_bytes();
    if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') || b.contains(&b'\\') {
        return Err(FsError::new(
            Code::SafetyRejected,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a single path component".to_string(),
            ),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_component_bytes(name: &OsStr) -> Result<()> {
    // Windows `OsStr` is WTF-16; an unpaired surrogate is the only way to reach
    // here, and such a name carries no separator by construction.
    let _ = name;
    Ok(())
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p flux-fs --no-fail-fast -E 'test(a_plain_component) + test(a_separator_is_refused) + test(dot_and_dotdot) + test(an_empty_name)'`

Expected: PASS, and **`Starting 4 tests`**. If it says fewer, a test name does not match the filter and you are running less than you think.

- [ ] **Step 6: Prove the tests are not vacuous**

Change `if s.contains('/') || s.contains('\\')` to `if false`.

Run: `cargo nextest run -p flux-fs --no-fail-fast -E 'test(a_separator_is_refused)'`

Expected: `a_separator_is_refused_before_any_syscall` FAILS. Confirm THAT test went red, not merely that the suite returned non-zero. **Revert, and verify the revert by READING the file** — not by trusting an edit tool's return value.

- [ ] **Step 7: Add the traits**

In `crates/flux-fs/src/fs.rs`, insert immediately BEFORE the `#[cfg(test)]` line:

```rust
/// An open directory under `DEST` (§149.7).
///
/// Every destination entry is created or opened relative to one of these, never
/// by a path the kernel re-resolves. That is the entire point: a path is checked
/// and used as two operations, and the object beneath it can change in between.
///
/// Names are single components and are refused otherwise — see
/// `crate::name::check_component`.
pub trait DirHandle: Sized {
    type Writer: FileHandle;

    /// Open a child DIRECTORY, refusing to traverse a symlink, junction or other
    /// name-surrogate reparse point. This is the operation §149.7 is about.
    fn open_dir(&self, name: &std::ffi::OsStr) -> Result<Self>;

    /// Create a child directory and return a handle to it.
    fn create_dir(&self, name: &std::ffi::OsStr) -> Result<Self>;

    /// Create a child file exclusively, as `FileSystem::create_new` does by path.
    fn create_new(&self, name: &std::ffi::OsStr) -> Result<Self::Writer>;

    /// Metadata for a child, judging the NAME and never its target.
    fn metadata(&self, name: &std::ffi::OsStr) -> Result<Metadata>;

    fn remove_file(&self, name: &std::ffi::OsStr) -> Result<()>;

    /// Publish `from` in this directory onto `to` in `other`, atomically and
    /// without replacing. The two-handle form is what makes staging in one
    /// directory and publishing into another expressible at all.
    fn rename_no_replace(
        &self,
        from: &std::ffi::OsStr,
        other: &Self,
        to: &std::ffi::OsStr,
    ) -> Result<()>;

    fn rename_replace(
        &self,
        from: &std::ffi::OsStr,
        other: &Self,
        to: &std::ffi::OsStr,
    ) -> Result<()>;
}

/// Resolving `DEST` once, at start, is the only path-based call in the writer.
///
/// This one DOES follow links: resolving `DEST` is exactly the operation §149.7
/// exempts, because a user who points `DEST` at a symlink has chosen that
/// destination. The clause protects what is BELOW it.
pub trait DestinationRoot: FileSystem {
    type Dir: DirHandle<Writer = Self::Writer>;

    fn destination_root(&self, path: &Path) -> Result<Self::Dir>;
}
```

**This exact text is compile-verified**, including the `DirHandle<Writer = Self::Writer>` bound — it resolves unambiguously because `DestinationRoot: FileSystem`.

- [ ] **Step 8: Export them**

In `crates/flux-fs/src/lib.rs`, the `pub use fs::{...}` list is a NAMED list; a type absent from it is invisible outside the crate. Replace that line with:

```rust
pub use fs::{
    DestinationRoot, DirEntry, DirHandle, FileHandle, FileIdentity, FileSystem, FileType,
    Metadata, ObjectId, Perms,
};
```

and add, after the `pub mod name;` line you added in Step 3:

```rust
pub use name::check_component;
```

- [ ] **Step 9: Run the gate**

Run: `just check`
Then: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-handles && cargo nextest run --workspace --no-tests=pass'`

Expected: both pass. The workspace total rises by 4 (the name tests). **No implementor exists yet and that is correct** — the traits have no `impl` anywhere, which compiles fine.

- [ ] **Step 10: Commit**

```bash
git add crates/flux-fs/src/name.rs crates/flux-fs/src/fs.rs crates/flux-fs/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(flux-fs): add the DirHandle and DestinationRoot traits

§149.7 binds the WRITER: every destination entry must be created and opened
relative to a directory handle held for its parent, never by a path the kernel
re-resolves between the check and the use.

FileSystem is deliberately untouched. §149.7 binds the writer, not the
filesystem abstraction -- a reader walking the source has no use for
handle-relative opens, and NullFs and EscapingFs are test stubs with no business
holding handles. Widening FileSystem would have forced all four implementors to
answer a question only one of them is asked.

Names are single components, refused before any syscall. OsStr cannot enforce
that and a Path parameter would invite join()ed values, which would hand the
middle component straight back to kernel resolution -- the exact hole this
closes. Both separators are rejected on both platforms: a backslash is not a
valid component on Windows, and on POSIX it is a legal filename this project has
no reason to create and every reason to refuse from a caller that thought it was
a separator.

Nothing implements these yet. The trait text is compile-verified, including the
DirHandle<Writer = Self::Writer> bound, which resolves because DestinationRoot
extends FileSystem.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: The POSIX arm

**Files:**
- Create: `crates/flux-platform/src/dir_unix.rs`
- Modify: `crates/flux-platform/src/lib.rs`
- Test: `crates/flux-platform/tests/dir_handle.rs` (create)

**This task's code cannot be exercised by `just check`.** Every step runs under WSL.

- [ ] **Step 1: Verify the state**

Confirm Task 1 landed and that `rustix` is a `cfg(unix)` dependency of this crate:

```bash
rg -n 'DirHandle|DestinationRoot' crates/flux-fs/src/lib.rs
rg -n 'rustix' crates/flux-platform/Cargo.toml
```

Expected: both traits in the export list; `rustix` present under a `[target.'cfg(unix)'.dependencies]` section. If either differs, STOP and report `STATE_MISMATCH`.

- [ ] **Step 2: Write the failing tests**

Create `crates/flux-platform/tests/dir_handle.rs`:

```rust
#![cfg(unix)]

use flux_fs::{DestinationRoot, DirHandle};
use flux_platform::StdFileSystem;
use std::ffi::OsStr;
use tempfile::TempDir;

#[test]
fn a_child_directory_opens() {
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("child")).unwrap();
    let root = StdFileSystem.destination_root(d.path()).unwrap();
    assert!(root.open_dir(OsStr::new("child")).is_ok());
}

#[test]
fn a_symlinked_component_is_refused_as_a_safety_rejection() {
    // THE test this whole cut exists for. An attacker replaces a directory Flux
    // created with a symlink pointing outside DEST; the open must refuse rather
    // than traverse, and it must say SAFETY_REJECTED rather than a generic error,
    // because §149.7 distinguishes the two.
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("real")).unwrap();
    std::os::unix::fs::symlink("/etc", d.path().join("escape")).unwrap();

    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let err = root.open_dir(OsStr::new("escape")).unwrap_err();
    assert_eq!(err.code, flux_fs::Code::SafetyRejected);
}

#[test]
fn a_plain_file_component_is_a_destination_error_not_a_safety_rejection() {
    // The distinction that costs an extra stat. MEASURED: `openat` answers ENOTDIR
    // for BOTH a symlink and a plain file, so the errno alone cannot tell them
    // apart -- and §149.7 wants DESTINATION_ERROR here and SAFETY_REJECTED above.
    // If this test and the one above ever report the same code, the stat was lost.
    let d = TempDir::new().unwrap();
    std::fs::write(d.path().join("plain"), b"x").unwrap();

    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let err = root.open_dir(OsStr::new("plain")).unwrap_err();
    assert_eq!(err.code, flux_fs::Code::DestinationError);
}

#[test]
fn a_missing_component_is_a_destination_error() {
    let d = TempDir::new().unwrap();
    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let err = root.open_dir(OsStr::new("absent")).unwrap_err();
    assert_eq!(err.code, flux_fs::Code::DestinationError);
}

#[test]
fn a_separator_is_refused_before_the_filesystem_is_touched() {
    let d = TempDir::new().unwrap();
    std::fs::create_dir_all(d.path().join("a/b")).unwrap();
    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let err = root.open_dir(OsStr::new("a/b")).unwrap_err();
    assert_eq!(err.code, flux_fs::Code::SafetyRejected);
}

#[test]
fn a_created_file_lands_in_the_handles_directory() {
    use std::io::Write;
    let d = TempDir::new().unwrap();
    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let mut w = root.create_new(OsStr::new("made")).unwrap();
    w.write_all(b"payload").unwrap();
    drop(w);
    assert_eq!(std::fs::read(d.path().join("made")).unwrap(), b"payload");
}

#[test]
fn the_handle_still_addresses_the_original_directory_after_a_swap() {
    // Item 114's attack, and the property the whole design buys. The handle is
    // opened, the NAME is then repointed at somewhere else, and a write through
    // the handle must still land in the ORIGINAL directory rather than following
    // the new name. A path-based writer fails this; a handle-based one cannot.
    use std::io::Write;
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("target")).unwrap();
    std::fs::create_dir(d.path().join("elsewhere")).unwrap();

    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let held = root.open_dir(OsStr::new("target")).unwrap();

    // RENAME aside rather than remove, and the difference is not stylistic.
    // MEASURED: rmdir marks the inode IS_DEADDIR, after which the kernel refuses
    // EVERY new entry under it -- ENOENT -- even through a file descriptor that is
    // still open. A remove-based swap therefore tests nothing this code does; it
    // tests that Linux forbids the whole operation. Renaming keeps the directory
    // alive, and the assertion is sharper for it: the write must follow the INODE
    // the handle holds, not the NAME that was swapped.
    std::fs::rename(d.path().join("target"), d.path().join("target.bak")).unwrap();
    std::os::unix::fs::symlink(d.path().join("elsewhere"), d.path().join("target")).unwrap();

    let mut w = held.create_new(OsStr::new("payload")).unwrap();
    w.write_all(b"x").unwrap();
    drop(w);

    assert!(
        !d.path().join("elsewhere/payload").exists(),
        "the write followed the swapped name; the handle was not load-bearing"
    );
    assert!(
        d.path().join("target.bak/payload").exists(),
        "the write must land in the directory the handle holds, under its new name"
    );
}
```

- [ ] **Step 3: Run them to verify they fail**

Run: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-handles && cargo nextest run -p flux-platform --no-fail-fast -E "binary(dir_handle)"'`

Expected: FAIL to COMPILE with **`error[E0277]`** — `StdFileSystem` does not implement `DestinationRoot`. A compile failure is the correct failure here.

- [ ] **Step 4: Implement the POSIX handle**

Create `crates/flux-platform/src/dir_unix.rs`:

```rust
//! The POSIX `DirHandle`: `openat` with `O_NOFOLLOW | O_DIRECTORY` (§149.7).

use flux_fs::{Code, DirHandle, FsError, Metadata, Result, check_component};
use rustix::fs::{AtFlags, Mode, OFlags, openat, statat};
use std::ffi::OsStr;
use std::os::fd::OwnedFd;

/// `Debug` is required, not decorative: the tests call `.unwrap_err()` on a
/// `Result<StdDir, _>`, which needs `StdDir: Debug` to compile.
#[derive(Debug)]
pub struct StdDir(OwnedFd);

impl StdDir {
    pub(crate) fn from_fd(fd: OwnedFd) -> Self {
        Self(fd)
    }

    /// Classify a failed `openat`. MEASURED: `ENOTDIR` covers BOTH a symlink and an
    /// ordinary non-directory, so the errno alone cannot answer §149.7's question.
    /// One `statat(SYMLINK_NOFOLLOW)` separates them, and it runs ONLY here -- on a
    /// path that has already failed -- so the happy path pays nothing.
    ///
    /// Note it is NOT `ELOOP`, which is the error `O_NOFOLLOW` is usually
    /// associated with: with `O_DIRECTORY` also set the kernel reports the type
    /// mismatch first. A reader who "corrects" this to `ELOOP` removes the check.
    fn classify(&self, name: &OsStr, e: rustix::io::Errno) -> FsError {
        use rustix::io::Errno;
        let io = std::io::Error::from(e);
        match e {
            Errno::NOTDIR => match statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(st) => {
                    let is_link = (st.st_mode as u32 & libc_s_ifmt()) == libc_s_iflnk();
                    if is_link {
                        FsError::new(Code::SafetyRejected, io)
                    } else {
                        FsError::new(Code::DestinationError, io)
                    }
                }
                // The name vanished between the open and the stat. It is no longer
                // a component we can judge, so report what we can prove.
                Err(_) => FsError::new(Code::DestinationError, io),
            },
            Errno::NOENT => FsError::new(Code::DestinationError, io),
            Errno::ACCESS | Errno::PERM => FsError::new(Code::PermissionDenied, io),
            _ => FsError::new(Code::IoError, io),
        }
    }
}

const fn libc_s_ifmt() -> u32 {
    0o170000
}
const fn libc_s_iflnk() -> u32 {
    0o120000
}

impl DirHandle for StdDir {
    type Writer = crate::StdFile;

    fn open_dir(&self, name: &OsStr) -> Result<Self> {
        check_component(name)?;
        match openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => Ok(Self(fd)),
            Err(e) => Err(self.classify(name, e)),
        }
    }

    fn create_dir(&self, name: &OsStr) -> Result<Self> {
        check_component(name)?;
        rustix::fs::mkdirat(&self.0, name, Mode::from_raw_mode(0o777))
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        self.open_dir(name)
    }

    fn create_new(&self, name: &OsStr) -> Result<Self::Writer> {
        check_component(name)?;
        let fd = openat(
            &self.0,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o666),
        )
        .map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(crate::std_fs::std_file_from(std::fs::File::from(fd)))
    }

    fn metadata(&self, name: &OsStr) -> Result<Metadata> {
        check_component(name)?;
        let st = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(crate::std_fs::metadata_from_stat(&st))
    }

    fn remove_file(&self, name: &OsStr) -> Result<()> {
        check_component(name)?;
        rustix::fs::unlinkat(&self.0, name, AtFlags::empty())
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }

    fn rename_no_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        rustix::fs::renameat_with(
            &self.0,
            from,
            &other.0,
            to,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }

    fn rename_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        rustix::fs::renameat(&self.0, from, &other.0, to)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }
}
```

**Two helpers referenced above do NOT exist yet and must be added to `std_fs.rs` first. Both were
grep-checked; do not assume either is there.**

```bash
rg -n '^pub struct StdFile|type Writer = |fn metadata_from_stat' crates/flux-platform/src/std_fs.rs
```

Expected: `pub struct StdFile(File);` at `:28` and `type Writer = StdFile;` at `:174`, and **no match
for `metadata_from_stat`**. Note the writer type is **`StdFile`**, not `StdWriter` — an earlier draft of
this plan named a type that does not exist, which is why this step greps rather than asserts.

1. **`StdFile`'s field is private** (`pub struct StdFile(File);` — a tuple struct whose field is not
   `pub`), so another module cannot construct one. Add beside it:

```rust
/// Construct a writer from an already-open file. `DirHandle::create_new` opens
/// relative to a directory handle, so it cannot go through the path-based
/// `create_new` and needs this.
///
/// `#[cfg(unix)]` for now because its only caller is `dir_unix.rs`. Without the
/// gate, Windows `just check` fails on `-D dead_code` until Task 3 lands a second
/// caller. Task 3 widens the gate; do not delete it here.
#[cfg(unix)]
pub(crate) fn std_file_from(f: File) -> StdFile {
    StdFile(f)
}
```

2. **`metadata_from_stat` does not exist.** Extract it from the `#[cfg(unix)] fn metadata` body in
   `std_fs.rs`, which already builds a `Metadata` from a stat result, and make it `pub(crate)` taking
   `&rustix::fs::Stat`. Do NOT duplicate the conversion: `identity_of` and the `§107` zero-inode rule
   live inside it, and a second copy will drift.

**If either cannot be done without changing a PUBLIC signature, STOP and report `SHAPE_DIVERGENCE`.**

- [ ] **Step 5: Declare the module**

In `crates/flux-platform/src/lib.rs`, add:

```rust
#[cfg(unix)]
mod dir_unix;
#[cfg(unix)]
pub use dir_unix::StdDir;
```

- [ ] **Step 6: Implement `DestinationRoot`**

In `crates/flux-platform/src/std_fs.rs`, after the `impl FileSystem for StdFileSystem` block:

```rust
/// Resolving `DEST` is the one path-based call in the writer, and it DOES follow
/// links -- §149.7 exempts it, because a user who points DEST at a symlink has
/// chosen that destination. Everything BELOW it goes through a handle.
#[cfg(unix)]
impl flux_fs::DestinationRoot for StdFileSystem {
    type Dir = crate::StdDir;

    fn destination_root(&self, path: &Path) -> Result<Self::Dir> {
        use rustix::fs::{CWD, Mode, OFlags, openat};
        let fd = openat(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(crate::StdDir::from_fd(fd))
    }
}
```

Note the absence of `NOFOLLOW` here, and that it is deliberate.

- [ ] **Step 7: Run the tests**

Run: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-handles && cargo nextest run -p flux-platform --no-fail-fast -E "binary(dir_handle)"'`

Expected: PASS, and **`Starting 7 tests`**.

- [ ] **Step 8: Prove the safety test is not vacuous**

Remove `OFlags::NOFOLLOW` from `open_dir`'s flag set — the single most plausible "simplification" of this file.

Run: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-handles && cargo nextest run -p flux-platform --no-fail-fast -E "binary(dir_handle)"'`

Expected: `a_symlinked_component_is_refused_as_a_safety_rejection` FAILS — without `NOFOLLOW` the symlink is traversed and the open succeeds. Confirm THAT test went red. **Revert, verify by READING the file, and re-run to confirm all 7 pass.**

- [ ] **Step 9: Prove the classification is not vacuous**

Change `classify`'s `Errno::NOTDIR` arm to return `FsError::new(Code::DestinationError, io)` unconditionally, dropping the stat.

Run the same command.

Expected: `a_symlinked_component_is_refused_as_a_safety_rejection` FAILS while `a_plain_file_component_is_a_destination_error_not_a_safety_rejection` still PASSES — which is exactly the conflation the stat exists to resolve. **Revert and verify by reading.**

- [ ] **Step 10: Run the gate and cross-check**

Run: `just check`
Then: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-handles && cargo nextest run --workspace --no-tests=pass'`

Expected: both pass. Windows is unchanged (the file is `#[cfg(unix)]`); WSL rises by 7.

- [ ] **Step 11: Commit**

```bash
git add crates/flux-platform/src/dir_unix.rs crates/flux-platform/src/lib.rs crates/flux-platform/src/std_fs.rs crates/flux-platform/tests/dir_handle.rs
git commit -m "$(cat <<'EOF'
feat(flux-platform): the POSIX DirHandle

openat with O_NOFOLLOW | O_DIRECTORY, so a symlink where a directory should be
is refused by the kernel rather than traversed. That is §149.7's requirement and
it costs nothing on the happy path.

The error classification is where the work is, and it rests on a measurement.
openat answers ENOTDIR for BOTH a symlink and an ordinary non-directory -- NOT
the ELOOP that O_NOFOLLOW is usually associated with, because O_DIRECTORY makes
the kernel report the type mismatch first. §149.7 needs those two reported
differently: SAFETY_REJECTED for the reparse point, DESTINATION_ERROR for the
plain file. So one statat(SYMLINK_NOFOLLOW) runs on the ERROR path only and
separates them, and two tests assert they never collapse into the same code.

destination_root deliberately omits NOFOLLOW: §149.7 exempts DEST itself,
because a user who points DEST at a symlink has chosen that destination. The
clause protects what is below it.

Non-vacuousness proven twice: dropping NOFOLLOW reds the symlink test, and
dropping the stat reds it while leaving the plain-file test green -- which is
the conflation the stat exists to resolve.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: The Windows arm

**Files:**
- Create: `crates/flux-platform/src/dir_windows.rs`
- Modify: `Cargo.toml`, `crates/flux-platform/src/lib.rs`, `crates/flux-platform/src/std_fs.rs`, `crates/flux-platform/tests/dir_handle.rs`

**This is the subtlest task in the plan.** The Windows arm does something structurally different from the POSIX one, and the difference is not a style choice.

- [ ] **Step 1: Add the `windows-sys` features**

In the root `Cargo.toml`, replace the `windows-sys` line with:

```toml
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_Storage_FileSystem", "Win32_System_IO", "Win32_Security", "Wdk_Foundation", "Wdk_Storage_FileSystem"] }
```

`NtCreateFile` lives in `Wdk_Storage_FileSystem`, `OBJECT_ATTRIBUTES` in `Wdk_Foundation`, and the latter references `SECURITY_DESCRIPTOR` so `Win32_Security` comes with it. Verified to compile and link at 0.61.2.

- [ ] **Step 2: Write the failing tests**

Append to `crates/flux-platform/tests/dir_handle.rs`. **The `#![cfg(unix)]` at the top of that file must become a per-test split** — change the first line to a blank and mark the existing tests `#[cfg(unix)]` individually, or move them into a `#[cfg(unix)] mod posix { ... }`. Then add:

```rust
#[cfg(windows)]
mod windows_arm {
    use flux_fs::{DestinationRoot, DirHandle};
    use flux_platform::StdFileSystem;
    use std::ffi::OsStr;
    use tempfile::TempDir;

    fn junction(target: &std::path::Path, link: &std::path::Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", &link.display().to_string(), &target.display().to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn a_child_directory_opens() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("child")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        assert!(root.open_dir(OsStr::new("child")).is_ok());
    }

    #[test]
    fn a_junction_is_refused_as_a_safety_rejection() {
        // The Windows half of the cut's reason to exist. NOTE what makes this hard:
        // MEASURED, NtCreateFile with FILE_OPEN_REPARSE_POINT SUCCEEDS on a junction
        // and hands back a handle to it. The refusal therefore comes from the TAG
        // check after the open, not from the open failing as it does on POSIX.
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        if !junction(&d.path().join("real"), &d.path().join("junc")) {
            eprintln!("SKIPPED: could not create a junction");
            return;
        }
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = root.open_dir(OsStr::new("junc")).unwrap_err();
        assert_eq!(err.code, flux_fs::Code::SafetyRejected);
    }

    #[test]
    fn a_plain_file_component_is_a_destination_error() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("plain"), b"x").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = root.open_dir(OsStr::new("plain")).unwrap_err();
        assert_eq!(err.code, flux_fs::Code::DestinationError);
    }

    #[test]
    fn a_missing_component_is_a_destination_error() {
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = root.open_dir(OsStr::new("absent")).unwrap_err();
        assert_eq!(err.code, flux_fs::Code::DestinationError);
    }

    #[test]
    fn a_created_file_lands_in_the_handles_directory() {
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut w = root.create_new(OsStr::new("made")).unwrap();
        w.write_all(b"payload").unwrap();
        drop(w);
        assert_eq!(std::fs::read(d.path().join("made")).unwrap(), b"payload");
    }
}
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo nextest run -p flux-platform --no-fail-fast -E 'test(/windows_arm/)'`

Expected: FAIL to COMPILE — `StdFileSystem` does not implement `DestinationRoot` on Windows.

- [ ] **Step 4: Implement the Windows handle**

Create `crates/flux-platform/src/dir_windows.rs`:

```rust
//! The Windows `DirHandle`: `NtCreateFile` with `OBJECT_ATTRIBUTES.RootDirectory`.
//!
//! This arm is NOT a transliteration of the POSIX one, and the difference is the
//! thing most likely to be "simplified" back into a defect.
//!
//! MEASURED: `NtCreateFile` with `FILE_OPEN_REPARSE_POINT` SUCCEEDS on a junction
//! and returns a handle TO the junction. There is no Win32 or NT flag that makes
//! the open FAIL on a reparse point, so this arm must open and then INSPECT.
//! Dropping the flag is not an alternative -- without it `NtCreateFile` FOLLOWS
//! the junction, which is the traversal §149.7 forbids.

use flux_fs::{Code, DirHandle, FsError, Metadata, Result, check_component};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT, NtCreateFile,
};
use windows_sys::Win32::Foundation::{HANDLE, UNICODE_STRING};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_LIST_DIRECTORY: u32 = 0x1;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;

const STATUS_NOT_A_DIRECTORY: i32 = 0xC000_0103u32 as i32;
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034u32 as i32;
const STATUS_OBJECT_PATH_NOT_FOUND: i32 = 0xC000_003Au32 as i32;
const STATUS_ACCESS_DENIED: i32 = 0xC000_0022u32 as i32;

/// `IsReparseTagNameSurrogate`. A surrogate stands in for another NAME -- a symlink
/// or a junction -- and is what §149.7 refuses. Everything else that merely carries
/// `FILE_ATTRIBUTE_REPARSE_POINT` must be TRAVERSED.
///
/// MEASURED on a real machine: 14 reparse points exist under `C:\` and the user
/// profile, and three of them -- OneDrive, Dropbox, MagentaCLOUD -- are cloud-sync
/// placeholders that are neither junctions nor symlinks. OneDrive's tag is
/// 0x9000701A with this bit CLEAR; a junction's is 0xA0000003 with it SET.
/// Rejecting on the attribute bit instead would refuse to write into the user's
/// OneDrive folder.
const fn is_name_surrogate(tag: u32) -> bool {
    tag & 0x2000_0000 != 0
}

pub struct StdDir(OwnedHandle);

impl StdDir {
    pub(crate) fn from_handle(h: OwnedHandle) -> Self {
        Self(h)
    }
}

impl DirHandle for StdDir {
    type Writer = crate::StdFile;

    fn open_dir(&self, name: &OsStr) -> Result<Self> {
        check_component(name)?;

        let mut wide: Vec<u16> = name.encode_wide().collect();
        let bytes = (wide.len() * 2) as u16;
        let mut us = UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: wide.as_mut_ptr(),
        };
        let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
        oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
        oa.RootDirectory = self.0.as_raw_handle() as HANDLE;
        oa.ObjectName = &raw const us;

        let mut h: HANDLE = std::ptr::null_mut();
        let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };

        // SAFETY: every pointer is to a live local that outlives the call, and
        // `wide` outlives `us` which borrows it.
        //
        // ShareAccess is NOT a free choice: 0 would take every directory the
        // writer walks EXCLUSIVELY, so any other process merely reading the tree
        // would fail the copy.
        let status = unsafe {
            NtCreateFile(
                &raw mut h,
                FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE,
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
            let code = match status {
                STATUS_NOT_A_DIRECTORY
                | STATUS_OBJECT_NAME_NOT_FOUND
                | STATUS_OBJECT_PATH_NOT_FOUND => Code::DestinationError,
                STATUS_ACCESS_DENIED => Code::PermissionDenied,
                _ => Code::IoError,
            };
            return Err(FsError::new(
                code,
                std::io::Error::other(format!("NtCreateFile: 0x{:08X}", status as u32)),
            ));
        }

        // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `h` is a valid handle we own.
        let opened = unsafe { OwnedHandle::from_raw_handle(h as _) };

        // THE STEP THAT HAS NO POSIX COUNTERPART. The open succeeded even if this
        // is a junction, so ask what it is. Dropping `opened` on the reject path
        // closes the handle, which matters: a refused directory must not leave a
        // sharing constraint behind for a later operation on the same tree.
        let tag = crate::dir_windows::reparse_tag_of(&opened)?;
        if let Some(tag) = tag
            && is_name_surrogate(tag)
        {
            return Err(FsError::new(
                Code::SafetyRejected,
                std::io::Error::other(format!("name-surrogate reparse point, tag 0x{tag:08X}")),
            ));
        }

        Ok(Self(opened))
    }

    fn create_dir(&self, name: &OsStr) -> Result<Self> {
        check_component(name)?;
        crate::dir_windows::create_dir_at(&self.0, name)?;
        self.open_dir(name)
    }

    fn create_new(&self, name: &OsStr) -> Result<Self::Writer> {
        check_component(name)?;
        crate::dir_windows::create_new_at(&self.0, name)
    }

    fn metadata(&self, name: &OsStr) -> Result<Metadata> {
        check_component(name)?;
        crate::dir_windows::metadata_at(&self.0, name)
    }

    fn remove_file(&self, name: &OsStr) -> Result<()> {
        check_component(name)?;
        crate::dir_windows::remove_file_at(&self.0, name)
    }

    fn rename_no_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        crate::dir_windows::rename_at(&self.0, from, &other.0, to, false)
    }

    fn rename_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        crate::dir_windows::rename_at(&self.0, from, &other.0, to, true)
    }
}
```

**`reparse_tag_of` is given in full below and is the ONLY helper this task implements.** The other
five -- `create_dir_at`, `create_new_at`, `metadata_at`, `remove_file_at`, `rename_at` -- are **Task
4**, and leaving them as `unimplemented!()` stubs is the CORRECT state at the end of this task.

**They were split out after a panel round, and the reason is worth carrying.** An earlier draft told
the implementer to "extend the pattern" from `open_dir`. That does not work: `remove_file_at` and
`rename_at` go through `NtSetInformationFile`, not `NtCreateFile`, and `FILE_RENAME_INFORMATION` is a
VARIABLE-LENGTH struct with a trailing `FileName: [u16; 1]` that must be built in an oversized buffer.
A different technique, not a variation on one.

All six live in `dir_windows.rs` itself, as private module functions — NOT at the crate root, which `crate::` alone would mean and which
`crates/flux-platform/src/lib.rs` re-exports only three names into. Their signatures are pinned here so
the implementer is extending a pattern rather than inventing an interface:

```rust
fn reparse_tag_of(h: &OwnedHandle) -> Result<Option<u32>>;
fn create_dir_at(parent: &OwnedHandle, name: &OsStr) -> Result<()>;
fn create_new_at(parent: &OwnedHandle, name: &OsStr) -> Result<crate::StdFile>;
fn metadata_at(parent: &OwnedHandle, name: &OsStr) -> Result<Metadata>;
fn remove_file_at(parent: &OwnedHandle, name: &OsStr) -> Result<()>;
fn rename_at(from_dir: &OwnedHandle, from: &OsStr, to_dir: &OwnedHandle, to: &OsStr, replace: bool) -> Result<()>;
```

Each is an `NtCreateFile` or `NtSetInformationFile` call against the same `OBJECT_ATTRIBUTES` +
`RootDirectory` pattern as `open_dir`, with `ShareAccess` and `DesiredAccess` stated rather than
defaulted for the same reason. `rename_at` uses `FILE_RENAME_INFORMATION` with its `RootDirectory` field
set to `to_dir` and `ReplaceIfExists` from the `replace` argument.

**`reparse_tag_of` is given in full below and must be implemented FIRST**, because it is the only one
the safety tests depend on — the other five can be stubbed with `unimplemented!()` long enough to get
Step 6's five tests green, then filled in before Step 8's gate, which will not pass while a stub
remains reachable from a compiled path.

Stub the five for now, with these exact signatures:

```rust
fn create_dir_at(_p: &OwnedHandle, _n: &OsStr) -> Result<()> { unimplemented!("Task 4") }
fn create_new_at(_p: &OwnedHandle, _n: &OsStr) -> Result<crate::StdFile> { unimplemented!("Task 4") }
fn metadata_at(_p: &OwnedHandle, _n: &OsStr) -> Result<Metadata> { unimplemented!("Task 4") }
fn remove_file_at(_p: &OwnedHandle, _n: &OsStr) -> Result<()> { unimplemented!("Task 4") }
fn rename_at(_fd: &OwnedHandle, _f: &OsStr, _td: &OwnedHandle, _t: &OsStr, _r: bool) -> Result<()> { unimplemented!("Task 4") }
```

**`create_new` is exercised by Step 2's `a_created_file_lands_in_the_handles_directory`**, so either
implement `create_new_at` here (it is the simplest of the five -- see Task 4 Step 4's table) or mark
that one test `#[ignore]` and remove the attribute in Task 4. Either is fine; say which you did.

```rust
/// Read a handle's reparse tag, or `None` when it is not a reparse point.
#[cfg(windows)]
pub(crate) fn reparse_tag_of(h: &std::os::windows::io::OwnedHandle) -> flux_fs::Result<Option<u32>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Wdk::Storage::FileSystem::{FileAttributeTagInformation, NtQueryInformationFile};
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    #[repr(C)]
    struct FileAttributeTagInfo {
        file_attributes: u32,
        reparse_tag: u32,
    }

    let mut info = FileAttributeTagInfo { file_attributes: 0, reparse_tag: 0 };
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a live, correctly sized FILE_ATTRIBUTE_TAG_INFORMATION and
    // the handle outlives the call.
    let status = unsafe {
        NtQueryInformationFile(
            h.as_raw_handle() as _,
            &raw mut iosb,
            (&raw mut info).cast(),
            size_of::<FileAttributeTagInfo>() as u32,
            FileAttributeTagInformation,
        )
    };
    if status != 0 {
        return Err(flux_fs::FsError::new(
            flux_fs::Code::IoError,
            std::io::Error::other(format!("NtQueryInformationFile: 0x{:08X}", status as u32)),
        ));
    }
    if info.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(None);
    }
    Ok(Some(info.reparse_tag))
}
```

Put it in `dir_windows.rs` and reference it as `self`-module rather than `crate::` if that is simpler; adjust the call site to match. **Both symbols were verified to exist at `windows-sys` 0.61.2** — `NtQueryInformationFile` at
`src/Windows/Wdk/Storage/FileSystem/mod.rs:671` (linking `ntdll.dll`), `FileAttributeTagInformation` as
`FILE_INFORMATION_CLASS = 35i32` at `:3148`, and `FILE_ATTRIBUTE_REPARSE_POINT` as `1024u32` at
`src/Windows/Win32/Storage/FileSystem/mod.rs:1414`. **If your patch version has moved any of them, STOP
and report `STATE_MISMATCH`** rather than reaching for a different crate.

- [ ] **Step 5: Declare the module and implement `DestinationRoot`**

In `crates/flux-platform/src/lib.rs`:

```rust
#[cfg(windows)]
mod dir_windows;
#[cfg(windows)]
pub use dir_windows::StdDir;
```

In `crates/flux-platform/src/std_fs.rs`:

```rust
/// See the Unix twin: DEST itself is resolved by path and DOES follow links,
/// because §149.7 exempts it. Everything below goes through a handle.
#[cfg(windows)]
impl flux_fs::DestinationRoot for StdFileSystem {
    type Dir = crate::StdDir;

    fn destination_root(&self, path: &Path) -> Result<Self::Dir> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        let f = OpenOptions::new()
            .access_mode(0x80 | 0x1 | 0x0010_0000)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .map_err(FsError::from_io)?;
        Ok(crate::StdDir::from_handle(f.into()))
    }
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run -p flux-platform --no-fail-fast -E 'test(/windows_arm/)'`

Expected: PASS, and **`Starting 4 tests ... (1 test ... skipped)`** if you took the `#[ignore]`
route for `a_created_file_lands_in_the_handles_directory`. **nextest EXCLUDES an ignored test from the
`Starting N` count** and reports it separately as skipped, so "five tests, one ignored" reads as 4 + 1,
never as `Starting 5`. Do not halt on that.

- [ ] **Step 7: Prove the tag check is not vacuous, and that it is not over-broad**

**Mutant A — drop the surrogate test.** Change `is_name_surrogate` to `tag & 0xFFFF_FFFF != 0`, so any reparse point is refused.

Run: `cargo nextest run -p flux-platform --no-fail-fast -E 'test(/windows_arm/)'`

Expected: the junction test still PASSES. **That is the point of this mutant: it shows the junction test alone cannot catch over-rejection**, which is why the measured OneDrive tag is recorded in the source comment rather than left to a test this machine cannot host. Revert.

**Mutant B — drop the inspection entirely.** Delete the `is_name_surrogate` block.

Run the same command.

Expected: `a_junction_is_refused_as_a_safety_rejection` FAILS — because the open SUCCEEDED, which is the measured Windows behaviour and the whole reason this step exists. Confirm THAT test went red. **Revert and verify by reading.**

- [ ] **Step 8: Run the gate and cross-check**

Run: `just check`
Then: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-handles && cargo nextest run --workspace --no-tests=pass'`

Expected: both pass. Windows rises by 5; WSL is unchanged by this task.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/flux-platform/src/dir_windows.rs crates/flux-platform/src/lib.rs crates/flux-platform/src/std_fs.rs crates/flux-platform/tests/dir_handle.rs
git commit -m "$(cat <<'EOF'
feat(flux-platform): the Windows DirHandle

NtCreateFile with OBJECT_ATTRIBUTES.RootDirectory, which is the only
handle-relative open Windows offers -- Win32 has none.

This arm is NOT a transliteration of the POSIX one. MEASURED: NtCreateFile with
FILE_OPEN_REPARSE_POINT SUCCEEDS on a junction and returns a handle TO the
junction, where POSIX openat REFUSES a symlink outright. So Windows opens and
then INSPECTS, and dropping the flag is not an alternative -- without it
NtCreateFile FOLLOWS the junction, the exact traversal §149.7 forbids.

It rejects only NAME SURROGATES, tag & 0x20000000 read through
FileAttributeTagInformation, never the FILE_ATTRIBUTE_REPARSE_POINT bit.
MEASURED on a real machine: 14 reparse points exist under C:\ and the user
profile and THREE of them -- OneDrive, Dropbox, MagentaCLOUD -- are cloud-sync
placeholders that are neither junctions nor symlinks. OneDrive's tag is
0x9000701A with the surrogate bit CLEAR; a junction's is 0xA0000003 with it SET.
Rejecting on the attribute bit would have refused to write into the user's
OneDrive folder.

DesiredAccess and ShareAccess are stated rather than defaulted. A ShareAccess of
0 takes every directory the writer walks EXCLUSIVELY, so any other process
merely reading the tree would fail the copy.

Two mutants, and the first is the interesting one: widening the surrogate test to
match every reparse point leaves the junction test GREEN, which is why the
measured OneDrive tag lives in a comment rather than in a test this machine
cannot host. Deleting the inspection reds the junction test, because the open
itself succeeded.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: The Windows handle-relative helpers

**Files:**
- Modify: `crates/flux-platform/src/dir_windows.rs`, `crates/flux-platform/tests/dir_handle.rs`
- Already done for you: `crates/flux-platform/src/std_fs.rs` -- `std_file_from` is now ungated and
  `type_of`, `perms_of` and `identity_of_handle` are now `pub(crate)`. **Do not edit that file.** An
  earlier draft left `std_file_from` behind `#[cfg(unix)]` with a comment saying the previous task
  would widen the gate; it did not, because it had no Windows caller to widen it for, and this task hit
  it as a compile error.

Split out of Task 3 after a panel round found "extend the pattern from `open_dir`" unexecutable. Two of
these go through a **different NT call**, and one builds a **variable-length struct**.

**Every symbol below was grep-verified in `windows-sys` 0.61.2**, in
`src/Windows/Wdk/Storage/FileSystem/mod.rs`:

| symbol | where |
|---|---|
| `NtSetInformationFile` | `:688`, links `ntdll.dll` |
| `FileRenameInformation` | `:3216`, `FILE_INFORMATION_CLASS = 10i32` |
| `FileDispositionInformation` | `:3157`, `= 13i32` |
| `FileBasicInformation` | `:3149`, `= 4i32` |
| `FILE_RENAME_INFORMATION` | `:2496` — `{ Anonymous, RootDirectory: HANDLE, FileNameLength: u32, FileName: [u16; 1] }` |
| `FILE_DISPOSITION_INFORMATION` | `:1699` — `{ DeleteFile: bool }` |

- [ ] **Step 1: Verify the state, and un-ignore the deferred test**

```bash
rg -c 'unimplemented!' crates/flux-platform/src/dir_windows.rs
rg -n '#\[ignore' crates/flux-platform/tests/dir_handle.rs
```

Expected: **5** stubs, and **one `#[ignore]`** on `a_created_file_lands_in_the_handles_directory` in the
`windows_arm` module. If the stub count differs, STOP and report `STATE_MISMATCH`.

**Remove that `#[ignore]` attribute now**, before writing anything else. Task 3 added it because
`create_new` routes through `create_new_at`, which was a stub; this task implements it. Leaving the
attribute would let the suite pass while the helper it guards is still `unimplemented!()`, which is the
one failure mode a green run must not hide.

- [ ] **Step 2: Write the failing tests**

Add inside the existing `#[cfg(windows)] mod windows_arm` block in `crates/flux-platform/tests/dir_handle.rs`:

```rust
    #[test]
    fn a_directory_is_created_and_reopened_through_the_handle() {
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let child = root.create_dir(OsStr::new("made")).unwrap();
        assert!(d.path().join("made").is_dir());
        let mut w = child.create_new(OsStr::new("inside")).unwrap();
        w.write_all(b"x").unwrap();
        drop(w);
        assert!(d.path().join("made/inside").exists(), "the returned handle must address the new directory");
    }

    #[test]
    fn a_file_is_removed_through_the_handle() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("doomed"), b"x").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        root.remove_file(OsStr::new("doomed")).unwrap();
        assert!(!d.path().join("doomed").exists());
    }

    #[test]
    fn a_publish_across_two_handles_refuses_an_occupied_name() {
        // The two-handle rename is what staging-then-publishing needs, and the
        // no-replace form must still refuse an occupied target.
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("stage")).unwrap();
        std::fs::write(d.path().join("stage/tmp"), b"payload").unwrap();
        std::fs::write(d.path().join("taken"), b"old").unwrap();

        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let stage = root.open_dir(OsStr::new("stage")).unwrap();

        stage.rename_no_replace(OsStr::new("tmp"), &root, OsStr::new("taken")).unwrap_err();
        assert_eq!(std::fs::read(d.path().join("taken")).unwrap(), b"old", "the occupied name must survive");

        stage.rename_no_replace(OsStr::new("tmp"), &root, OsStr::new("fresh")).unwrap();
        assert_eq!(std::fs::read(d.path().join("fresh")).unwrap(), b"payload");
    }

    #[test]
    fn metadata_through_the_handle_reads_the_child() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("f"), b"1234").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        assert_eq!(root.metadata(OsStr::new("f")).unwrap().len, 4);
    }
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo nextest run -p flux-platform --no-fail-fast -E 'test(/windows_arm/)'`

Expected: the four new tests PANIC with `not implemented: Task 4`. A panic, not a compile error — the
stubs exist with the right signatures, which is what Task 3 left behind.

- [ ] **Step 4: Implement the three `NtCreateFile`-shaped helpers**

`create_dir_at`, `create_new_at` and `metadata_at` follow `open_dir`'s structure exactly — build the
`UNICODE_STRING`, zero an `OBJECT_ATTRIBUTES`, set `RootDirectory` to the parent handle, call
`NtCreateFile`. They differ only in disposition and options:

| helper | CreateDisposition | CreateOptions | DesiredAccess |
|---|---|---|---|
| `create_dir_at` | `FILE_CREATE` | `FILE_DIRECTORY_FILE` | `FILE_LIST_DIRECTORY \| SYNCHRONIZE` |
| `create_new_at` | `FILE_CREATE` | `FILE_NON_DIRECTORY_FILE \| FILE_SYNCHRONOUS_IO_NONALERT` | `FILE_GENERIC_WRITE \| SYNCHRONIZE` |
| `metadata_at` | `FILE_OPEN` | `FILE_OPEN_REPARSE_POINT \| FILE_SYNCHRONOUS_IO_NONALERT` | `FILE_READ_ATTRIBUTES \| SYNCHRONIZE` |

`ShareAccess` is `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE` for all three, for the reason
`open_dir` records: a `ShareAccess` of 0 locks the object against every other process.

`create_new_at` converts its raw handle with `OwnedHandle::from_raw_handle` and returns
`crate::std_fs::std_file_from(File::from(h))`.

**`metadata_at` does NOT hand-roll an `NtQueryInformationFile` call**, and an earlier draft of this
plan said it should -- passing `FileBasicInformation` (`= 4i32`). That was wrong twice over, and both
were caught by executing it. `FILE_BASIC_INFORMATION` (`:1598`) carries four timestamps and
`FileAttributes` and **no size field**, so it cannot answer the `len == 4` that this task's own oracle
test asserts; and nothing in that class carries IDENTITY, so the `Metadata` it built would report
`FileIdentity::Unavailable` on every filesystem. The POSIX arm fills identity from
`identity_of_raw(st_dev, st_ino)`, so that would be a silent cross-arm divergence in a field §107
makes load-bearing: every identity comparison reached through a Windows handle would degrade to the
lexical floor, which fails OPEN.

Once the handle is open, the object is already pinned, so build the `Metadata` **exactly as the
path-based Windows `metadata` does at `crates/flux-platform/src/std_fs.rs:221`** -- the only thing that
had to be handle-relative was the open:

```rust
    // SAFETY: NtCreateFile returned this handle and nothing else owns it.
    let f = File::from(unsafe { OwnedHandle::from_raw_handle(h as _) });
    let m = f.metadata().map_err(FsError::from_io)?;
    Ok(Metadata {
        len: m.len(),
        file_type: crate::std_fs::type_of(&m),
        permissions: Some(crate::std_fs::perms_of(&m)),
        modified: m.modified().ok(),
        identity: crate::std_fs::identity_of_handle(&f),
    })
```

`FILE_READ_ATTRIBUTES` is enough access for all of it: the path-based arm asks for `access_mode(0)`,
which is less, and gets the same answers. Reusing those three helpers rather than re-deriving the
mapping is the point -- it is what makes the two Windows arms agree by construction instead of by
inspection.

**`create_dir_at` returns `Result<()>`; the caller re-opens.** `DirHandle::create_dir` already calls
`self.open_dir(name)` afterwards, which is what runs the reparse-tag check on the thing just created.

- [ ] **Step 5: Implement `remove_file_at` — a DIFFERENT NT call**

Open the name with `DELETE | SYNCHRONIZE` and `FILE_OPEN_REPARSE_POINT`, so a link is removed rather
than followed, then:

```rust
    let mut info = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a live FILE_DISPOSITION_INFORMATION of the size given, and
    // the handle outlives the call.
    let status = unsafe {
        NtSetInformationFile(
            h.as_raw_handle() as _,
            &raw mut iosb,
            (&raw mut info).cast(),
            size_of::<FILE_DISPOSITION_INFORMATION>() as u32,
            FileDispositionInformation,
        )
    };
    if status != 0 {
        return Err(FsError::new(
            Code::IoError,
            std::io::Error::other(format!("NtSetInformationFile: 0x{:08X}", status as u32)),
        ));
    }
    drop(h);
    Ok(())
```

The delete takes effect when the last handle closes, which is why `h` is dropped immediately after.

- [ ] **Step 6: Implement `rename_at` — a VARIABLE-LENGTH struct**

This is the one that cannot be extrapolated from anything above. `FILE_RENAME_INFORMATION` ends in
`FileName: [u16; 1]` — a placeholder for a name of any length — so the struct is built inside a byte
buffer larger than itself:

```rust
    let wide: Vec<u16> = to.encode_wide().collect();
    let name_bytes = wide.len() * 2;
    let total = size_of::<FILE_RENAME_INFORMATION>() + name_bytes;
    let mut buf = vec![0u8; total];

    // SAFETY: `buf` is at least size_of::<FILE_RENAME_INFORMATION>() bytes, and a
    // Vec<u8> allocation is at least pointer-aligned, which is this struct's
    // alignment (that of HANDLE). The name is written into the trailing space the
    // [u16; 1] placeholder stands for.
    unsafe {
        let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        (&raw mut (*info).Anonymous.ReplaceIfExists).write(replace);
        (&raw mut (*info).RootDirectory).write(to_dir.as_raw_handle() as HANDLE);
        (&raw mut (*info).FileNameLength).write(name_bytes as u32);
        std::ptr::copy_nonoverlapping(
            wide.as_ptr(),
            (&raw mut (*info).FileName).cast::<u16>(),
            wide.len(),
        );
    }
```

Then open `from` in `from_dir` with `DELETE | SYNCHRONIZE` and `FILE_OPEN_REPARSE_POINT`, and call
`NtSetInformationFile` with `FileRenameInformation` (`= 10i32`) and `buf.len() as u32` as the length.

**`RootDirectory` on the information struct is what makes this handle-relative on the DESTINATION
side.** Without it the rename resolves `FileName` as a path, which is the resolution this whole cut
removes — and it is the reason `rename_at` takes two handles rather than one.

**The `Anonymous` union** carries `ReplaceIfExists` as a `bool` in one arm and `Flags` as a `u32` in the
other. Use the `ReplaceIfExists` arm: the `Flags` arm belongs to `FileRenameInformationEx`, a different
information class, and writing it here sets a bit pattern the kernel reads as a flag set.

- [ ] **Step 7: Run the tests**

Run: `cargo nextest run -p flux-platform --no-fail-fast -E 'test(/windows_arm/)'`

Expected: PASS, and **`Starting 9 tests`** — five from Task 3, four here.

- [ ] **Step 8: Prove the rename is really handle-relative**

Change `rename_at` to write `std::ptr::null_mut()` into `RootDirectory` instead of `to_dir`'s handle.

Run the same command.

Expected: `a_publish_across_two_handles_refuses_an_occupied_name` FAILS. With a null `RootDirectory` the
name is resolved as a path rather than relative to the destination handle, so the publish does not land
where the test looks for it. Confirm THAT test went red, not merely that the suite returned non-zero.
**Revert, and verify the revert by READING the file.**

- [ ] **Step 9: Run the gate and cross-check**

Run: `just check`
Then: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-handles && cargo nextest run --workspace --no-tests=pass'`

Expected: both pass; Windows up 4. **No stub may remain:**

```bash
rg -n 'unimplemented!' crates/flux-platform/src/dir_windows.rs
```

Expected: **no matches**.

- [ ] **Step 10: Commit**

```bash
git add crates/flux-platform/src/dir_windows.rs crates/flux-platform/tests/dir_handle.rs
git commit -m "$(cat <<'XEOF'
feat(flux-platform): the Windows handle-relative helpers

Split out of the handle task after a panel round found "extend the pattern from
open_dir" unexecutable, and it was right. remove_file_at and rename_at go
through NtSetInformationFile rather than NtCreateFile, and
FILE_RENAME_INFORMATION is a VARIABLE-LENGTH struct whose trailing
FileName: [u16; 1] is a placeholder, so it has to be built inside an oversized
byte buffer. That is a different technique, not a variation on one, and an
implementer told to extrapolate would have had to invent it.

RootDirectory on the rename information struct is what makes the publish
handle-relative on the DESTINATION side. Without it the name resolves as a path,
which is the resolution this cut exists to remove -- and that is what the mutant
proves: nulling it reds the two-handle publish test while everything else stays
green.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
XEOF
)"
```

---

### Task 5: The fake

**Files:**
- Modify: `crates/flux-core/src/fault_fs.rs`

- [ ] **Step 1: Verify the state**

Confirm `struct Inner` is at `:15`, `pub struct FaultFs` at `:147` and `impl FileSystem for FaultFs` at `:385`:

```bash
rg -n '^struct Inner|^pub struct FaultFs|^impl FileSystem for FaultFs' crates/flux-core/src/fault_fs.rs
```

If it differs, STOP and report `STATE_MISMATCH`.

- [ ] **Step 2: Write the failing test**

Add to the `#[cfg(test)] mod tests` block at the end of `crates/flux-core/src/fault_fs.rs`:

```rust
    #[test]
    fn a_handle_still_addresses_its_directory_after_the_name_is_repointed() {
        // Item 114's attack, staged without a kernel. The fake's handle is an id
        // into its tree rather than a name, so repointing the NAME leaves the
        // handle addressing the ORIGINAL node -- which is exactly what a real
        // directory handle does, and exactly what a path does not.
        use flux_fs::{DestinationRoot, DirHandle};
        use std::ffi::OsStr;

        let fs = FaultFs::new();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/target")).unwrap();
        fs.create_dir(Path::new("/elsewhere")).unwrap();

        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let held = root.open_dir(OsStr::new("target")).unwrap();

        // The attacker swaps what the NAME means.
        fs.repoint_for_test(Path::new("/dst/target"), Path::new("/elsewhere"));

        held.create_new(OsStr::new("payload")).unwrap();

        assert!(
            !fs.exists("/elsewhere/payload"),
            "the write followed the swapped name; the handle was not load-bearing"
        );
    }

    #[test]
    fn the_fake_refuses_a_name_with_a_separator() {
        use flux_fs::{DestinationRoot, DirHandle};
        use std::ffi::OsStr;

        let fs = FaultFs::new();
        fs.create_dir(Path::new("/dst")).unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let err = root.open_dir(OsStr::new("a/b")).unwrap_err();
        assert_eq!(err.code, Code::SafetyRejected);
    }
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo nextest run -p flux-core --no-fail-fast -E 'test(a_handle_still_addresses) + test(the_fake_refuses_a_name)'`

Expected: FAIL to COMPILE — `FaultFs` implements neither trait and has no `repoint_for_test`.

- [ ] **Step 4: Implement the fake's handle**

`FaultFs` stores its tree in `Inner`. Add a node-id map and a handle that holds an id, then implement both traits so that:

- `destination_root(path)` resolves `path` by name ONCE and returns a handle holding the resulting node id;
- every `DirHandle` method resolves its child **relative to the stored id**, never by re-walking the path;
- `open_dir` calls `flux_fs::check_component` first, then refuses a child whose node is marked as a link with `Code::SafetyRejected`;
- `repoint_for_test(name, new_target)` rebinds a NAME to a different node without touching the node the handle holds — this is the attacker, and it exists only for the test above.

**The exact field layout is yours to choose**, because `Inner`'s shape is not fixed by this plan and a subagent reading it will see the current one. What is NOT yours to choose: the handle must hold an ID, not a path. **If you find yourself storing a `PathBuf` in the handle, STOP and report `SHAPE_DIVERGENCE`** — a path-holding handle reproduces the defect this cut removes, and the test above would then pass for the wrong reason.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p flux-core --no-fail-fast -E 'test(a_handle_still_addresses) + test(the_fake_refuses_a_name)'`

Expected: PASS, and **`Starting 2 tests`**.

- [ ] **Step 6: Prove the attack test is not vacuous**

Make `open_dir` resolve by PATH each time instead of by the stored id — the defect the design removes.

Run the same command.

Expected: `a_handle_still_addresses_its_directory_after_the_name_is_repointed` FAILS. **Revert and verify by reading.**

- [ ] **Step 7: Run the gate and cross-check**

Run: `just check`
Then: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-handles && cargo nextest run --workspace --no-tests=pass'`

Expected: both pass, both up 2.

- [ ] **Step 8: Commit**

```bash
git add crates/flux-core/src/fault_fs.rs
git commit -m "$(cat <<'EOF'
test(flux-core): FaultFs implements DirHandle with synthetic handles

The fake's handle is an ID into its tree, not a name. That is what makes it a
real test rather than an interface it merely satisfies: the property
handle-relative writes provide is "the parent cannot be swapped between
resolution and use", and repointing a NAME in the fake leaves the handle
addressing the original node -- which is what a kernel directory handle does and
what a path does not.

So item 114's attack is staged directly, with no kernel and no privileges: open
a handle, repoint the name at somewhere else, write through the handle, and
assert the write did not follow. Resolving by path instead of by the stored id
reds that test, which is the mutant proving it.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Final verification

**Files:** none modified.

- [ ] **Step 1: Run the full gate on Windows**

Run: `just check`

Expected: all stages pass.

- [ ] **Step 2: Run the full suite under WSL**

Run: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-handles && cargo nextest run --workspace --no-tests=pass'`

Expected: pass.

- [ ] **Step 3: Confirm nothing outside the named files changed**

```bash
git diff --stat itemb-base..HEAD
```

Expected: exactly these, and nothing else.

```
Cargo.toml
crates/flux-core/src/fault_fs.rs
crates/flux-fs/src/fs.rs
crates/flux-fs/src/lib.rs
crates/flux-fs/src/name.rs
crates/flux-platform/src/dir_unix.rs
crates/flux-platform/src/dir_windows.rs
crates/flux-platform/src/lib.rs
crates/flux-platform/src/std_fs.rs
crates/flux-platform/tests/dir_handle.rs
```

Anything else is out of scope and should be reported, not committed.

- [ ] **Step 4: Confirm the forbidden shapes are absent**

```bash
rg -n 'Errno::LOOP' crates/flux-platform/src/
rg -n 'FILE_ATTRIBUTE_REPARSE_POINT' crates/flux-platform/src/dir_windows.rs
```

The first must have **no matches**. `ELOOP` — `Errno::LOOP` in rustix — is the error POSIX associates
with `O_NOFOLLOW`, and is NOT what this code sees (measured: `ENOTDIR`). A match means someone matched
on the expected behaviour rather than the measured one, and the symlink refusal would fall through to
`IoError`.

**Grep for `Errno::LOOP`, not for the bare word `ELOOP`.** An earlier draft did the latter and would
have failed on `dir_unix.rs`'s own comments, which mention `ELOOP` precisely to say it is not what
happens — the documentation that stops the mistake would have tripped the check that guards against it.

The second must match **only inside `reparse_tag_of`**, where it distinguishes "not a reparse point" from a tag. A match inside `open_dir`'s decision means the surrogate test was replaced with the attribute bit, which refuses OneDrive.

- [ ] **Step 5: Confirm no engine or CLI code was touched**

```bash
git diff --stat itemb-base..HEAD -- crates/flux-cli crates/flux-core/src/copy.rs crates/flux-core/src/walk.rs
```

Expected: **empty**. This cut is a platform primitive; `copy_tree` consuming it is cut 4.

- [ ] **Step 6: Delete the base tag**

```bash
git tag -d itemb-base
```

- [ ] **Step 7: STOP**

Pushing and opening a pull request are outward actions. `just pr "title" body.md` exists and runs the gate itself, but **do not run it without explicit approval.** Report completion and wait.

---

## Self-review

**Spec coverage.** §149.7's clauses each map to a task: handle-relative creation and opening → Tasks 2 and 3; `DEST` resolved once and following links → `destination_root` in both arms, with the omission of `NOFOLLOW` called out in code and commit; reparse point rejected as `SAFETY_REJECTED` → the symlink test (Task 2) and the junction test (Task 3); other errors to `DESTINATION_ERROR`/`PERMISSION_DENIED`/`IO_ERROR` → `classify` and the NTSTATUS match; single components → Task 1's `check_component`, asserted in all three implementors. The spec's fourth clause — *"symlinks this operation creates are leaves"* — has **no task, correctly**: nothing in this cut creates a symlink, so it is vacuous here and is recorded in the spec as an obligation on whichever cut adds them.

**Placeholder scan.** One task step is deliberately not literal code: **Task 5 Step 4** describes the `FaultFs` field layout as the implementer's choice. That is not a placeholder dodge — `Inner`'s shape is not fixed by this plan and pasting a struct that does not match the tree would be worse than useless. The step pins the two things that ARE contractual (the handle holds an id, not a path; `check_component` runs first) and gives a `SHAPE_DIVERGENCE` trigger for the one way to get it wrong.

**Type consistency.** `StdDir` is the handle type in both platform files, so `impl DestinationRoot for StdFileSystem` names `crate::StdDir` under either cfg. `type Writer = crate::StdFile` matches the `DestinationRoot: FileSystem` bound because `StdFileSystem::Writer` is `StdFile`, verified at `crates/flux-platform/src/std_fs.rs:174`. `check_component` is exported from `flux_fs` at the crate root in Task 1 Step 8 and imported unqualified everywhere after.

**An earlier draft of this plan failed exactly this check, and the check as first written did not catch it.** It named the writer type `StdWriter` in three code blocks. No such type exists — it is `StdFile` at `std_fs.rs:28`. The type-consistency paragraph had asserted the two matched, *using the fabricated name on both sides of the comparison*, which is self-consistent and false. A consistency check that compares a plan against itself proves nothing; this one now cites the line in the tree. The same pass found that `StdFile`'s field is private, so the constructor Task 2 needs does not exist either.

**Known gaps, stated rather than hidden.**

1. ~~**Task 3 leaves five helpers unimplemented**, each the same `RootDirectory` pattern as `open_dir`; if that proves too thin it should become its own task.~~ **It was too thin, and it is now Task 4.** A panel round established that the premise was false rather than optimistic: `remove_file_at` and `rename_at` do not use `NtCreateFile` at all, and `FILE_RENAME_INFORMATION`'s trailing `FileName: [u16; 1]` requires building the struct inside an oversized buffer — a technique nothing in Task 3 demonstrates. The gap is closed rather than carried: Task 4 gives every NT symbol with its verified line, a disposition/options/access table for the three `NtCreateFile`-shaped helpers, the full body for both `NtSetInformationFile` ones, and a mutant that nulls `RootDirectory` to prove the rename is genuinely handle-relative. **The lesson worth keeping is that "extend the pattern" is a placeholder wearing a technique's clothes** — it passes a placeholder scan because it names no TBD, and it is exactly as unexecutable.
2. **The over-rejection case has no test on this machine.** Mutant A in Task 3 demonstrates why: widening the surrogate check leaves every test green, because no OneDrive-style placeholder exists in a `TempDir`. The measured tags live in a source comment instead. A machine with a cloud-sync root could add the test; this one cannot.
3. **Nested junction traversal is unmeasured.** Only a single junction directly under the parent was probed. Each component is opened one at a time so the design does not depend on it, but it is untested ground.
4. **No engine consumes any of this yet**, by construction. Cut 4 is where the first caller appears, and it is where `copy_file`'s signature question gets answered.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-09-25-handle-relative-destination-writes.md`. Two execution options:

**1. Subagent-Driven (recommended)** — a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.
