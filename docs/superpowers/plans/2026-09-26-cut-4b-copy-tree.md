# Cut 4b: `copy_tree` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A recursive copy engine, `copy_tree`, that writes only through destination directory handles, refuses every overlap of source and destination, never replaces an existing file, and reports every failure without stopping the walk.

**Architecture:** `DirHandle` gains `identity()`; `CopyError` gains `step: CopyStep`; `copy_file_at`'s Step 2a gate refuses an existing destination under `Publish::NoReplace`. A new `crates/flux-core/src/tree.rs` holds the outcome types and `copy_tree`, which drives the ordered walk with a stack of destination handles kept in lockstep with it.

**Tech Stack:** Rust (edition 2024, stable 1.98), `rustix` (POSIX), `windows-sys` (Windows), `cargo nextest`, `just`.

**Design authority:** `docs/superpowers/specs/2026-09-26-cut-4b-copy-tree-design.md` (owner-approved; adversarial panel GREEN at round 3). Every citation below was read at `2a83f16` (`main` `4449857` plus the spec).

---

## Ground rules for every task

- **Worktree:** `E:\Rust\flux-engine`, branch `spec/cut-4b` (off `main` at `4449857`, no upstream). NEVER run a bare `git push`; publishing is `just pr` after the capstone and test audit, with owner approval.
- **Step 0 — state verification.** Confirm each quoted "current" text. If it differs, STOP and report `STATE_MISMATCH: <file>: <what differs>`. Do not adapt.
- **Shape-divergence stop.** If making it compile changes a type, name, variant, field, code, message or value shown in this plan, STOP and report `[plan] -> [yours] because <reason>`. `cargo fmt --all` rewrapping is fine.
- **Tests are the oracle.** The tests here are already written: add them verbatim, implement until they pass, never edit a test to match the code. A test that fails after the plan's code is implemented exactly is a plan defect: STOP and report it.
- **Gates (exact; run from Git Bash in the worktree):** `just check` (Windows; expect `Summary [...] N tests run: N passed, K skipped`, exit 0); `just check-linux` (WSL; clippy + nextest, exit 0; may take minutes the first time); `just check-mac` (macOS clippy, exit 0).
- **Mutation checks** use `cargo nextest run -p flux-core --no-fail-fast` (or `-p flux-platform`) so a count is never truncated by fail-fast; restore each mutant and confirm `git diff` shows only the intended change.
- **Write files with the Write/Edit tools, never a Bash heredoc.**
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Where this plan refines the spec (declared, not silent)

1. **Skipped subtrees are a `Skipped` frame on the handle stack**, not a separate prefix set. MEASURED from `crates/flux-core/src/walk.rs`: `enter` pushes a frame before it returns `Ok(WalkEvent::Dir)` and pushes nothing on `Err`; `next` emits `DirEnd` for every pushed frame except the root's. So one stack entry per `Dir`, popped at `DirEnd`, stays in lockstep with the walk by construction; a prefix set would be a second structure that could drift. Behaviour is identical.
2. **`copy_tree`'s own outer `Err`s use `CopyStep::Resolve`** (lexical floor, pre-flight, destination resolution, root creation, per-directory aborts) and **`CopyStep::Source`** (the source root) - the spec's rule that a site belonging to no copy step uses `Resolve`, applied to the tree. Decision 3's abort carries the failing file's own `CopyError`, step `Publish`.
3. **`FakeDirHandle::identity` mints an identity on demand only for a filesystem ROOT** (`/`), which is implicitly present in the fake (`destination_root` and `create_dir` already treat it so); every other directory must already have one, exactly as `FaultFs::metadata` requires.

## File map

| File | Change |
|---|---|
| `crates/flux-fs/src/fs.rs` | `DirHandle::identity` (Task 1) |
| `crates/flux-platform/src/dir_unix.rs` | POSIX `identity` (Task 1) |
| `crates/flux-platform/src/dir_windows.rs` | Windows `identity` (Task 1) |
| `crates/flux-platform/src/std_fs.rs` | `identity_of_handle` borrows any raw handle (Task 1) |
| `crates/flux-platform/tests/dir_handle.rs` | identity tests, both arms (Task 1) |
| `crates/flux-core/src/fault_fs.rs` | `FakeDirHandle::identity` + test (Task 1) |
| `crates/flux-core/src/copy.rs` | `CopyStep`, `CopyError.step`, `CopyError::at`, the NoReplace gate, `pub(crate)` helpers, tests (Tasks 2, 3) |
| `crates/flux-core/src/tree.rs` | NEW: outcome types, `copy_tree`, tests (Tasks 4, 5) |
| `crates/flux-core/src/lib.rs` | `pub mod tree;` and re-exports (Task 4) |
| `crates/flux-core/Cargo.toml`, `crates/flux-core/tests/tree_std_fs.rs` | real-filesystem tests (Task 6) |
| `TODO.md` | bookkeeping (Task 7) |

---

### Task 1: `DirHandle::identity`

**Files:** `crates/flux-fs/src/fs.rs`, `crates/flux-platform/src/dir_unix.rs`, `crates/flux-platform/src/dir_windows.rs`, `crates/flux-platform/src/std_fs.rs`, `crates/flux-platform/tests/dir_handle.rs`, `crates/flux-core/src/fault_fs.rs`.

- [ ] **Step 0: Verify state.**
  - `fs.rs`: `pub trait DirHandle: Sized {` whose first items are `type Writer: FileHandle;` and `fn open_dir(&self, name: &std::ffi::OsStr) -> Result<Self>;`; the only implementors in the workspace are `StdDir` in `dir_unix.rs` and in `dir_windows.rs`, and `FakeDirHandle` in `fault_fs.rs` (`grep -rn "impl DirHandle for" crates` must print exactly those three).
  - `dir_unix.rs`: `pub struct StdDir(OwnedFd);` and inside `impl DirHandle for StdDir` a `fn metadata(&self, name: &OsStr) -> Result<Metadata>` ending `Ok(crate::std_fs::metadata_from_stat(&st))`.
  - `dir_windows.rs`: `pub struct StdDir(OwnedHandle);`; `use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};`.
  - `std_fs.rs`: `#[cfg(windows)] pub(crate) fn identity_of_handle(file: &File) -> FileIdentity {` whose body begins `use std::os::windows::io::AsRawHandle;` and calls `file.as_raw_handle()`; and `#[cfg(unix)] pub(crate) fn metadata_from_stat(st: &rustix::fs::Stat) -> Metadata`.
  - `fault_fs.rs`: `fn mint_identity(g: &mut Inner, path: &Path)`; `impl FakeDirHandle` with `fn my_path(&self) -> PathBuf`; `impl DirHandle for FakeDirHandle {` beginning `type Writer = FakeHandle;`.

- [ ] **Step 1: Add the tests (the oracle).**

  In `crates/flux-platform/tests/dir_handle.rs`, append to the END of `mod posix` AND to the END of `mod windows_arm` (the same test in both):

```rust
    #[test]
    fn a_handle_reports_the_identity_of_the_directory_it_holds() {
        use flux_fs::FileSystem;
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let sub = root.open_dir(OsStr::new("sub")).unwrap();
        let made = root.create_dir(OsStr::new("made")).unwrap();

        for (handle, path) in [
            (&root, d.path().to_path_buf()),
            (&sub, d.path().join("sub")),
            (&made, d.path().join("made")),
        ] {
            let by_handle = handle.identity().unwrap();
            assert!(matches!(by_handle, flux_fs::FileIdentity::Strong(_)), "{path:?}: {by_handle:?}");
            assert_eq!(by_handle, StdFileSystem.metadata(&path).unwrap().identity, "{path:?}");
        }
        assert_ne!(root.identity().unwrap(), sub.identity().unwrap());
    }
```

  In `crates/flux-core/src/fault_fs.rs`, append to the end of `mod tests`:

```rust
    #[test]
    fn a_fake_handle_reports_the_identity_of_its_directory() {
        use flux_fs::{DestinationRoot, DirHandle, FileSystem};
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let root = fs.destination_root(Path::new("/")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();

        assert!(matches!(root.identity().unwrap(), flux_fs::FileIdentity::Strong(_)));
        assert_eq!(d.identity().unwrap(), fs.metadata(Path::new("/d")).unwrap().identity);
        assert_ne!(root.identity().unwrap(), d.identity().unwrap());
        // Asking is not a filesystem call: it must not shift `fail_nth` counts.
        let calls = fs.calls().len();
        let _ = d.identity().unwrap();
        assert_eq!(fs.calls().len(), calls);
    }
```

- [ ] **Step 2: Run, expect FAIL to compile** (`no method named identity`).
- [ ] **Step 3: Implement.**

  `fs.rs`, inside `pub trait DirHandle`, directly after `fn open_dir(...)`:

```rust
    /// The identity of the directory this handle HOLDS - never of a path, so a name
    /// re-pointed after the handle was opened does not change the answer (§149.7).
    /// Containment is compared against the resolved destination through this (cut-4
    /// amendment, decision 2).
    fn identity(&self) -> Result<FileIdentity>;
```

  `dir_unix.rs`, inside `impl DirHandle for StdDir`, directly after `fn open_dir`:

```rust
    fn identity(&self) -> Result<flux_fs::FileIdentity> {
        let st = rustix::fs::fstat(&self.0)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(crate::std_fs::metadata_from_stat(&st).identity)
    }
```

  `std_fs.rs`: change the signature line `pub(crate) fn identity_of_handle(file: &File) -> FileIdentity {` to `pub(crate) fn identity_of_handle(file: &impl std::os::windows::io::AsRawHandle) -> FileIdentity {`, and in its doc comment replace `which is why it is private and takes a \`&File\` rather than` with `which is why it is private and takes an open handle rather than` (the next line, `a path.`, stays). The body is unchanged.

  `dir_windows.rs`, inside `impl DirHandle for StdDir`, directly after `fn open_dir`:

```rust
    fn identity(&self) -> Result<flux_fs::FileIdentity> {
        Ok(crate::std_fs::identity_of_handle(&self.0))
    }
```

  `fault_fs.rs`, inside `impl DirHandle for FakeDirHandle`, directly after `type Writer = FakeHandle;`:

```rust
    fn identity(&self) -> Result<flux_fs::FileIdentity> {
        let path = self.my_path();
        let mut g = self.inner.lock().unwrap();
        // A root is implicitly present in the fake (`destination_root` and `create_dir`
        // treat it so), so it gets an identity on first ask. Every other directory was
        // minted when it was created, and a missing one is a creation path that skipped
        // `mint_identity` - the same loud failure `metadata` gives.
        if path.has_root() && path.parent().is_none() {
            mint_identity(&mut g, &path);
        }
        Ok(*g.identities.get(&path).unwrap_or_else(|| {
            panic!("no identity minted for {}: a creation path skipped mint_identity", path.display())
        }))
    }
```

  If any identifier above is unresolved in its file (e.g. `FileIdentity` not in scope in `fs.rs`), STOP and report `STATE_MISMATCH` rather than importing a substitute.

- [ ] **Step 4: Run, expect PASS.** `cargo nextest run -p flux-core --no-fail-fast` and `cargo nextest run -p flux-platform --no-fail-fast -E 'test(a_handle_reports)'` on Windows, then `just check-linux` (runs the POSIX arm) and `just check-mac`.
- [ ] **Step 5: Mutation.** In the POSIX `identity`, return `Ok(flux_fs::FileIdentity::Unavailable)` → `a_handle_reports_the_identity_of_the_directory_it_holds` RED under `just check-linux`. In the Windows `identity`, the same → RED under `cargo nextest run -p flux-platform --no-fail-fast`. Restore both.
- [ ] **Step 6: Commit** — `feat(flux-fs): DirHandle::identity, the resolved directory's own identity`.

---

### Task 2: `CopyError` names the step it failed at

**Files:** `crates/flux-core/src/copy.rs`.

- [ ] **Step 0: Verify state.** `pub struct CopyError { pub cause: FsError, pub leftover: Option<(std::path::PathBuf, std::io::Error)>, }` (with its doc comments); `impl CopyError { fn new(cause: FsError) -> Self { CopyError { cause, leftover: None } } ... }`; `fn discard<D: DirHandle>(parent: &D, temp: &OsStr, code: Code, source: std::io::Error) -> CopyError`; `grep -n "CopyError::new\|discard(" crates/flux-core/src/copy.rs` lists: `fn new` itself, the two in `discard`, one each in `split_destination`, `identity_gate`'s `refuse`, `copy_file` (lexical Step 0 and `destination_root`), and in `copy_file_at` the source `metadata`, the type check, `open_read`, `create_new`, two `discard` calls in the stream loop, one for durability, two for metadata, three for the Step 7 re-check and one for publish. If the list differs, STOP.

- [ ] **Step 1: Add the test (the oracle)**, at the end of `mod tests`:

```rust
    #[test]
    fn every_failure_names_the_step_it_failed_at() {
        let step = |fs: &FaultFs, src: &str, dst: &str, o: &CopyOptions| {
            copy_file(fs, Path::new(src), Path::new(dst), o).unwrap_err().step
        };
        let with_src = || {
            let fs = FaultFs::new();
            fs.write_file("/src", b"hello");
            fs
        };

        // Resolve: the lexical refusal, a destination naming no file, a missing parent.
        assert_eq!(step(&with_src(), "/src", "/src", &opts()), CopyStep::Resolve);
        assert_eq!(step(&with_src(), "/src", "/", &opts()), CopyStep::Resolve);
        assert_eq!(step(&with_src(), "/src", "/missing/dst", &opts()), CopyStep::Resolve);

        // Source: missing, not a regular file, cannot be opened.
        assert_eq!(step(&FaultFs::new(), "/nope", "/dst", &opts()), CopyStep::Source);
        let special = FaultFs::new();
        special.add_special("/src");
        assert_eq!(step(&special, "/src", "/dst", &opts()), CopyStep::Source);
        let unreadable = with_src();
        unreadable.fail("open_read", flux_fs::Code::PermissionDenied);
        assert_eq!(step(&unreadable, "/src", "/dst", &opts()), CopyStep::Source);

        // Gate: the destination is the source by identity.
        let alias = with_src();
        alias.write_file("/dst", b"hello");
        let id = flux_fs::FileIdentity::Strong(flux_fs::ObjectId { volume: 1, index: 9_001 });
        alias.set_identity("/src", id);
        alias.set_identity("/dst", id);
        assert_eq!(step(&alias, "/src", "/dst", &opts()), CopyStep::Gate);

        // Create, Stream, Durability, Metadata, Recheck, Publish.
        let create = with_src();
        create.fail("create_new", flux_fs::Code::PermissionDenied);
        assert_eq!(step(&create, "/src", "/dst", &opts()), CopyStep::Create);

        let stream = with_src();
        stream.fail_write(std::io::Error::other("the device hiccupped"));
        assert_eq!(step(&stream, "/src", "/dst", &opts()), CopyStep::Stream);

        let durable = with_src();
        durable.fail("sync_all", flux_fs::Code::IoError);
        let mut strict_sync = opts();
        strict_sync.durability = Durability::Strict;
        assert_eq!(step(&durable, "/src", "/dst", &strict_sync), CopyStep::Durability);

        let meta = with_src();
        meta.fail("set_times", flux_fs::Code::PermissionDenied);
        let mut strict_times = opts();
        strict_times.preserve_times = Preserve::Strict;
        assert_eq!(step(&meta, "/src", "/dst", &strict_times), CopyStep::Metadata);

        let recheck = with_src();
        recheck.vanish_on_second_metadata("/src");
        assert_eq!(step(&recheck, "/src", "/dst", &opts()), CopyStep::Recheck);

        let publish = with_src();
        publish.fail("rename_replace", flux_fs::Code::PermissionDenied);
        assert_eq!(step(&publish, "/src", "/dst", &opts()), CopyStep::Publish);
    }
```

- [ ] **Step 2: Run, expect FAIL to compile** (`CopyStep` and `.step` do not exist).
- [ ] **Step 3: Implement.**
  1. Directly above `pub struct CopyError`, add:

```rust
/// Where in `copy_file_at` (or its `copy_file` wrapper) a copy failed. A caller that
/// must tell a failure at the exclusive create from one at the publish - both can say
/// `AlreadyExists` - reads this instead of guessing from the error kind (cut 4b, F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyStep {
    /// Resolving the destination: the lexical Step 0 refusal, splitting the path,
    /// opening the parent. Also any failure that belongs to no copy step.
    Resolve,
    /// Step 2: the source's metadata, its type, opening it.
    Source,
    /// Step 2a: the destination identity gate.
    Gate,
    /// Step 3: the exclusive create of the staging temporary.
    Create,
    /// Step 4: streaming the bytes.
    Stream,
    /// Step 5: strict durability.
    Durability,
    /// Step 6: applying metadata under `Preserve::Strict`.
    Metadata,
    /// Step 7: the source re-check.
    Recheck,
    /// Step 7: the publishing rename.
    Publish,
}
```

  2. In `pub struct CopyError`, after the `leftover` field, add:

```rust
    /// Which step failed. Set where the failure happens; see `CopyStep`.
    pub step: CopyStep,
```

  3. Replace `fn new(cause: FsError) -> Self { CopyError { cause, leftover: None } }` with:

```rust
    pub(crate) fn at(step: CopyStep, cause: FsError) -> Self {
        CopyError { cause, leftover: None, step }
    }
```

  4. `discard` gains a `step: CopyStep` parameter after `temp: &OsStr`; its three constructions become `CopyError::at(step, FsError::new(code, source))` (twice) and `CopyError { cause: FsError::new(code, source), leftover: Some((std::path::PathBuf::from(temp), why.source)), step }`.
  5. Every site, exactly (no other change): `split_destination` → `CopyError::at(CopyStep::Resolve, ...)`; `identity_gate`'s `refuse` → `CopyError::at(CopyStep::Gate, ...)`; in `copy_file` the lexical Step 0 → `CopyError::at(CopyStep::Resolve, ...)` and `fs.destination_root(parent_path).map_err(CopyError::new)?` → `.map_err(|e| CopyError::at(CopyStep::Resolve, e))?`; in `copy_file_at` the source `metadata`, the `SpecialFileUnsupported` refusal and `open_read` → `CopyStep::Source`; `create_new` → `CopyStep::Create`; the two stream-loop `discard` calls → `discard(parent, &temp, CopyStep::Stream, ...)`; durability → `CopyStep::Durability`; the two `Preserve::Strict` metadata `discard`s → `CopyStep::Metadata`; the three Step 7 re-check `discard`s (gone, other error, changed) → `CopyStep::Recheck`; the publish `discard` → `CopyStep::Publish`.
  6. Make `split_destination` and `weaker` `pub(crate)` (Task 5 uses them).
- [ ] **Step 4: Run, expect PASS.** `cargo nextest run -p flux-core --no-fail-fast` → every existing test plus the new one pass, no existing test edited. Then `just check`.
- [ ] **Step 5: Mutation.** Set the publish `discard` to `CopyStep::Recheck` → the new test RED; restore. Set `create_new`'s step to `CopyStep::Source` → RED; restore.
- [ ] **Step 6: Commit** — `feat(flux-core): CopyError names the step a copy failed at`.

---

### Task 3: under `NoReplace`, Step 2a refuses an existing destination before copying

**Files:** `crates/flux-core/src/copy.rs`.

- [ ] **Step 0: Verify state.** `fn identity_gate<D: DirHandle>(parent: &D, name: &OsStr, src: &Metadata, safety: Safety)` with the match quoted in the spec's "API" section, and `copy_file_at` calling `identity_gate(parent, name, &src_meta, opts.safety)?`; the existing test `no_replace_refuses_an_existing_target_and_cleans_up` asserts `err.code() == flux_fs::Code::IoError` and `!fs.exists("/dst.flux-partial.op1")`.
- [ ] **Step 1: Add the tests (the oracle)**, at the end of `mod tests`:

```rust
    #[test]
    fn under_no_replace_an_existing_destination_is_refused_before_any_bytes_are_copied() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"existing");
        let mut o = opts();
        o.publish = Publish::NoReplace;

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap_err();

        assert_eq!(err.step, CopyStep::Gate);
        assert_eq!(err.code(), flux_fs::Code::IoError);
        assert_eq!(err.cause.source.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(!fs.called("create_new"), "refused before the temporary exists");
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"existing"[..]));
    }

    #[test]
    fn under_no_replace_the_identity_rows_still_come_first() {
        // An identity-equal destination is still SAFETY_REJECTED, not AlreadyExists.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.write_file("/dst", b"hello");
        let id = flux_fs::FileIdentity::Strong(flux_fs::ObjectId { volume: 1, index: 9_002 });
        fs.set_identity("/src", id);
        fs.set_identity("/dst", id);
        let mut o = opts();
        o.publish = Publish::NoReplace;

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert_eq!(err.step, CopyStep::Gate);
    }

    #[test]
    fn under_replace_an_existing_destination_is_still_replaced() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"old");
        copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"new"[..]));
    }
```

- [ ] **Step 2: Run, expect the first test RED** (the copy proceeds to `create_new`).
- [ ] **Step 3: Implement.** Give `identity_gate` a fifth parameter `publish: Publish` and replace its final arm `Ok(m) => match (src.identity, m.identity) { ... },` with:

```rust
        Ok(m) => {
            let verdict = match (src.identity, m.identity) {
                (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b => {
                    return Err(refuse("the destination is the source itself, by identity"));
                }
                (FileIdentity::Strong(_), FileIdentity::Strong(_)) => None,
                (s, d) => degraded(weaker(s, d))?,
            };
            // F3 (cut 4b): a publish that never replaces refuses an EXISTING destination
            // here, after every identity row and before the temporary exists, instead of
            // copying every byte and failing at the rename. Same code and kind the
            // rename gives (IO_ERROR, AlreadyExists); `copy_tree` maps it to
            // DESTINATION_NAMESPACE_COLLISION. A symlink occupying the name reaches this
            // row too - the handle stat never follows it - and is left untouched.
            if publish == Publish::NoReplace {
                return Err(CopyError::at(
                    CopyStep::Gate,
                    FsError::new(
                        Code::IoError,
                        std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            "the destination exists and this publish never replaces",
                        ),
                    ),
                ));
            }
            Ok(verdict)
        }
```

  and change the call in `copy_file_at` to `identity_gate(parent, name, &src_meta, opts.safety, opts.publish)?`.
- [ ] **Step 4: Run, expect PASS**, all of `flux-core` (`no_replace_refuses_an_existing_target_and_cleans_up` passes unmodified), then `just check`.
- [ ] **Step 5: Mutation.** Delete the `if publish == Publish::NoReplace { ... }` block → `under_no_replace_an_existing_destination_is_refused_before_any_bytes_are_copied` RED; restore. Move that block ABOVE the `let verdict = ...` → `under_no_replace_the_identity_rows_still_come_first` RED; restore.
- [ ] **Step 6: Commit** — `feat(flux-core): under NoReplace, Step 2a refuses an existing destination before copying`.

---

### Task 4: the tree outcome types

**Files:** Create `crates/flux-core/src/tree.rs`; modify `crates/flux-core/src/lib.rs`.

- [ ] **Step 0: Verify state.** `lib.rs` has `pub mod copy;`, `pub mod walk;`, `#[cfg(test)] pub mod fault_fs;`, `pub use copy::{copy_file, copy_file_at};` and the `walk` re-export; no `tree` module exists.
- [ ] **Step 1: Create `tree.rs`** with the module doc, the types, and their tests:

```rust
//! Recursive copy (cut 4b): `copy_tree` drives the ordered walk and `copy_file_at`,
//! writing only through destination directory handles (§149.7).
//!
//! Design authority: `docs/superpowers/specs/2026-09-26-cut-4b-copy-tree-design.md`.

use crate::copy::CopyError;
use flux_fs::{FileIdentity, FileType, FsError, MetadataFailure};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a tree copy did, counted. Individual failures are STREAMED to the sink as
/// they happen, never accumulated here, so a tree with a million failures costs what a
/// clean one does.
#[derive(Debug, Default)]
pub struct TreeOutcome {
    pub files_copied: u64,
    pub bytes_copied: u64,
    /// Directories THIS operation created, the root included when it made it. A
    /// pre-existing directory merged into is not counted.
    pub directories_created: u64,
    pub failures: FailureTally,
    /// Identity comparisons that were SKIPPED because a side was not `Strong`. Not
    /// failures: the copies happened. The engine captures; cut 5's CLI renders.
    pub warnings: WeakIdentityWarnings,
}

#[derive(Debug)]
pub struct TreeFailure {
    /// Relative to the source root, as the walk reports it.
    pub path: PathBuf,
    pub cause: TreeFailureCause,
}

/// Each variant is defined by WHERE the failure happened, so no failure can be
/// expressed two ways.
#[derive(Debug)]
pub enum TreeFailureCause {
    /// The walk could not read or enter something.
    Walk(FsError),
    /// A destination directory could not be created or entered - including a
    /// `DESTINATION_NAMESPACE_COLLISION` between two source names the destination folds
    /// into one. Its subtree is skipped and reported once, here.
    CreateDir(FsError),
    /// One file's copy failed. `CopyError` intact, so a leftover temporary is still
    /// reported per file.
    Copy(CopyError),
    /// A symlink or other non-regular entry, which this cut does not recreate.
    Unsupported(FileType),
    /// The file IS at the destination; some of its metadata could not be applied.
    /// Published with complaints, which exits 1 like the single-file case.
    PublishedWithComplaints(Vec<MetadataFailure>),
}

/// One counter per `TreeFailureCause` variant. Fixed size: it costs the same on a
/// clean tree and on one where every entry failed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FailureTally {
    pub walk: u64,
    pub create_dir: u64,
    pub copy: u64,
    pub unsupported: u64,
    pub published_with_complaints: u64,
}

impl FailureTally {
    /// The only total. Derived, never stored beside the parts.
    pub fn total(&self) -> u64 {
        self.walk + self.create_dir + self.copy + self.unsupported + self.published_with_complaints
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// EXHAUSTIVE, with no wildcard arm: a new cause must name its counter here or the
    /// crate does not compile. A `_ =>` arm would let a new cause count nothing, and the
    /// miscount would surface only as a wrong exit code.
    #[cfg_attr(not(test), expect(dead_code, reason = "copy_tree calls it; Task 5 removes this line"))]
    pub(crate) fn count(&mut self, cause: &TreeFailureCause) {
        match cause {
            TreeFailureCause::Walk(_) => self.walk += 1,
            TreeFailureCause::CreateDir(_) => self.create_dir += 1,
            TreeFailureCause::Copy(_) => self.copy += 1,
            TreeFailureCause::Unsupported(_) => self.unsupported += 1,
            TreeFailureCause::PublishedWithComplaints(_) => self.published_with_complaints += 1,
        }
    }
}

/// Skipped identity comparisons, aggregated for one warning apiece (walker design,
/// "When identity is weak or unavailable").
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WeakIdentityWarnings {
    /// One group per affected volume (`ObjectId::volume`), in stable order.
    pub weak: BTreeMap<u64, DegradedGroup>,
    /// Everything whose identity was `Unavailable`, which names no volume to group by.
    pub unavailable: Option<DegradedGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradedGroup {
    pub count: u64,
    /// The first path that degraded, relative to the source root. The EMPTY path is the
    /// source root itself (the §129 pre-flight).
    pub example: PathBuf,
}

impl WeakIdentityWarnings {
    pub fn is_empty(&self) -> bool {
        self.weak.is_empty() && self.unavailable.is_none()
    }

    /// Record one comparison that was skipped. `weaker` is the weaker side, as
    /// `copy::weaker` computes it; `Strong` is not a degradation and is ignored.
    #[cfg_attr(not(test), expect(dead_code, reason = "copy_tree calls it; Task 5 removes this line"))]
    pub(crate) fn record(&mut self, weaker: FileIdentity, example: &Path) {
        let group = match weaker {
            FileIdentity::Strong(_) => return,
            FileIdentity::Weak(id) => self
                .weak
                .entry(id.volume)
                .or_insert_with(|| DegradedGroup { count: 0, example: example.to_path_buf() }),
            FileIdentity::Unavailable => self
                .unavailable
                .get_or_insert_with(|| DegradedGroup { count: 0, example: example.to_path_buf() }),
        };
        group.count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_fs::{Code, ObjectId};

    #[test]
    fn the_tally_counts_each_cause_in_its_own_field() {
        let e = || FsError::new(Code::IoError, std::io::Error::other("x"));
        let mut t = FailureTally::default();
        t.count(&TreeFailureCause::Walk(e()));
        t.count(&TreeFailureCause::CreateDir(e()));
        t.count(&TreeFailureCause::Copy(CopyError::at(crate::copy::CopyStep::Create, e())));
        t.count(&TreeFailureCause::Unsupported(FileType::Symlink));
        t.count(&TreeFailureCause::PublishedWithComplaints(Vec::new()));
        assert_eq!(
            t,
            FailureTally { walk: 1, create_dir: 1, copy: 1, unsupported: 1, published_with_complaints: 1 }
        );
        assert_eq!(t.total(), 5);
        assert!(!t.is_empty());
        assert!(FailureTally::default().is_empty());
    }

    #[test]
    fn warnings_group_weak_by_volume_and_keep_the_first_example() {
        let weak = |volume| FileIdentity::Weak(ObjectId { volume, index: 1 });
        let mut w = WeakIdentityWarnings::default();
        w.record(weak(7), Path::new("a"));
        w.record(weak(7), Path::new("b"));
        w.record(weak(9), Path::new("c"));
        w.record(FileIdentity::Unavailable, Path::new("d"));
        w.record(FileIdentity::Strong(ObjectId { volume: 1, index: 1 }), Path::new("e"));

        assert_eq!(w.weak[&7], DegradedGroup { count: 2, example: PathBuf::from("a") });
        assert_eq!(w.weak[&9], DegradedGroup { count: 1, example: PathBuf::from("c") });
        assert_eq!(w.unavailable, Some(DegradedGroup { count: 1, example: PathBuf::from("d") }));
        assert!(!w.is_empty());
        assert!(WeakIdentityWarnings::default().is_empty());
    }
}
```

- [ ] **Step 2: Wire it.** In `lib.rs`, add `pub mod tree;` after `pub mod walk;`, and after the `walk` re-export add `pub use tree::{DegradedGroup, FailureTally, TreeFailure, TreeFailureCause, TreeOutcome, WeakIdentityWarnings};`.
- [ ] **Step 3: Run, expect PASS.** `cargo nextest run -p flux-core --no-fail-fast`, then `just check`. (`count` and `record` have no non-test caller until Task 5; the `cfg_attr(not(test), expect(dead_code, ...))` on each says so, and because an UNFULFILLED `expect` is itself a warning, Task 5 cannot forget to remove them.)
- [ ] **Step 4: Mutation.** In `count`, make the `Copy` arm increment `create_dir` → `the_tally_counts_each_cause_in_its_own_field` RED; restore. In `record`, key `Weak` by `id.index` → `warnings_group_weak_by_volume_and_keep_the_first_example` RED; restore.
- [ ] **Step 5: Commit** — `feat(flux-core): the tree outcome types`.

---

### Task 5: `copy_tree`

**Files:** `crates/flux-core/src/tree.rs`, `crates/flux-core/src/lib.rs`.

- [ ] **Step 0: Verify state.** Tasks 1-4 are committed: `DirHandle::identity` exists; `CopyError { cause, leftover, step }` with `CopyError::at` `pub(crate)`; `copy::split_destination` and `copy::weaker` are `pub(crate)`; `tree.rs` holds Task 4's types. `crate::walk` exports `walk` and `WalkEvent` with variants `Dir { path, identity }`, `File { path }`, `Symlink { path }`, `Other { path }`, `DirEnd { path }`, and the walk's `Item` is `Result<WalkEvent, WalkError>` where `WalkError { path, cause: FsError }`.

- [ ] **Step 1: Add the tests (the oracle)** to `tree.rs`'s `mod tests`. Replace that module's `use` lines with:

```rust
    use super::*;
    use crate::fault_fs::FaultFs;
    use flux_fs::{Code, Durability, FileSystem, ObjectId, OperationId, Preserve};
```

  and append:

```rust
    fn opts() -> CopyOptions {
        CopyOptions {
            preserve_times: Preserve::Default,
            preserve_permissions: Preserve::Default,
            durability: Durability::Normal,
            publish: Publish::Replace,
            safety: Safety::Default,
            operation_id: OperationId::new("op1"),
        }
    }

    fn strict() -> CopyOptions {
        let mut o = opts();
        o.safety = Safety::Strict;
        o
    }

    fn run(
        fs: &FaultFs,
        src: &str,
        dst: &str,
        o: &CopyOptions,
    ) -> (std::result::Result<TreeOutcome, CopyError>, Vec<TreeFailure>) {
        let mut got = Vec::new();
        let r = copy_tree(fs, Path::new(src), Path::new(dst), o, &mut |f| got.push(f));
        (r, got)
    }

    /// `/src/a` (1 byte) and `/src/sub/b` (2 bytes). `/dst` absent.
    fn tree() -> FaultFs {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.create_dir(Path::new("/src/sub")).unwrap();
        fs.write_file("/src/a", b"A");
        fs.write_file("/src/sub/b", b"BB");
        fs
    }

    fn identity_of(fs: &FaultFs, p: &str) -> FileIdentity {
        fs.metadata(Path::new(p)).unwrap().identity
    }

    fn calls_since(fs: &FaultFs, n: usize) -> Vec<String> {
        fs.calls()[n..].to_vec()
    }

    #[test]
    fn lexically_within_compares_components() {
        assert!(lexically_within(Path::new("/data"), Path::new("/data")));
        assert!(lexically_within(Path::new("/data/backup"), Path::new("/data")));
        assert!(lexically_within(Path::new("/data/./backup"), Path::new("/data")));
        assert!(!lexically_within(Path::new("/database"), Path::new("/data")));
        assert!(!lexically_within(Path::new("/other"), Path::new("/data")));
    }

    #[test]
    fn a_missing_source_root_fails_the_whole_operation_before_the_destination() {
        let fs = FaultFs::new();
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let e = r.unwrap_err();
        assert_eq!(e.step, CopyStep::Source);
        assert!(got.is_empty());
        assert!(!fs.called("create_dir"));
    }

    #[test]
    fn a_destination_inside_the_source_is_refused_lexically_before_any_call() {
        for dst in ["/src", "/src/backup"] {
            let fs = tree();
            // A WEAK source identity, so the identity pre-flight cannot catch the overlap
            // (it degrades under Default and proceeds): the lexical floor is then the only
            // guard, and a mutant that disables it creates `/src/backup` and goes red.
            // Without this, the pre-flight refuses both spellings on its own (the anchor IS
            // `/src`) and the test could not tell the floor from the pre-flight (plan
            // panel round 1).
            fs.set_identity("/src", FileIdentity::Weak(ObjectId { volume: 5, index: 1 }));
            let n = fs.calls().len();
            let (r, _) = run(&fs, "/src", dst, &opts());
            assert_eq!(r.unwrap_err().code(), Code::SafetyRejected, "{dst}");
            assert!(
                !calls_since(&fs, n).iter().any(|c| c.starts_with("create_dir")),
                "{dst}: nothing created"
            );
        }
    }

    #[test]
    fn a_destination_that_is_the_source_by_identity_is_refused_before_anything_is_created() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_identity("/dst", identity_of(&fs, "/src"));
        let n = fs.calls().len();

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(got.is_empty());
        assert!(!calls_since(&fs, n).iter().any(|c| c.starts_with("create")));
    }

    #[test]
    fn an_absent_destination_is_anchored_on_its_parent() {
        let fs = tree();
        fs.create_dir(Path::new("/out")).unwrap();
        fs.set_identity("/out", identity_of(&fs, "/src"));

        let (r, _) = run(&fs, "/src", "/out/copy", &opts());

        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(!fs.exists("/out/copy"));
    }

    #[test]
    fn a_file_at_the_destination_root_is_refused_as_not_a_directory() {
        let fs = tree();
        fs.write_file("/dst", b"a file");
        let (r, _) = run(&fs, "/src", "/dst", &opts());
        let e = r.unwrap_err();
        assert_eq!(e.cause.source.kind(), std::io::ErrorKind::NotADirectory);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"a file"[..]));
    }

    #[test]
    fn a_weak_preflight_warns_under_default_and_refuses_under_strict() {
        let weak = FileIdentity::Weak(ObjectId { volume: 9, index: 1 });

        let lax = tree();
        lax.set_identity("/src", weak);
        let (r, _) = run(&lax, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert_eq!(out.warnings.weak[&9].example, PathBuf::new(), "the root is the empty path");

        let tight = tree();
        tight.set_identity("/src", weak);
        let (r, _) = run(&tight, "/src", "/dst", &strict());
        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(!tight.exists("/dst"));
    }

    #[test]
    fn a_fresh_tree_is_copied_through_handles_with_no_replace() {
        let fs = tree();
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_copied, out.bytes_copied, out.directories_created), (2, 3, 2));
        assert!(out.failures.is_empty());
        assert!(out.warnings.is_empty());
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"A"[..]));
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]));
        assert!(fs.called("rename_no_replace"), "NoReplace is mandatory in a tree");
        assert!(!fs.called("rename_replace"), "the caller's Replace is overridden");
    }

    #[test]
    fn an_existing_destination_directory_is_merged_and_not_counted() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/sub")).unwrap();
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_copied, out.directories_created), (2, 0));
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]));
    }

    #[test]
    fn an_existing_destination_file_is_left_intact_and_reported_as_a_collision() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/a", b"old");

        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new("a"));
        match &got[0].cause {
            TreeFailureCause::Copy(e) => {
                assert_eq!(e.code(), Code::DestinationNamespaceCollision);
                assert_eq!(e.step, CopyStep::Gate);
            }
            other => panic!("expected Copy, got {other:?}"),
        }
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"old"[..]));
        assert!(
            !fs.calls().iter().any(|c| c.starts_with("create_new") && c.contains("a.flux-partial")),
            "the bytes were never copied"
        );
        assert_eq!(out.failures.copy, 1);
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]), "the walk continued");
    }

    #[test]
    fn a_file_that_is_the_source_by_identity_fails_alone_and_the_walk_continues() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/a", b"A");
        fs.set_identity("/dst/a", identity_of(&fs, "/src/a"));

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        match &got[0].cause {
            TreeFailureCause::Copy(e) => assert_eq!(e.code(), Code::SafetyRejected),
            other => panic!("expected Copy, got {other:?}"),
        }
        assert_eq!(out.files_copied, 1);
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]));
    }

    #[test]
    fn two_source_directories_folded_into_one_destination_are_a_collision() {
        // `A` and `a` in one source directory; the destination "folds" them: `/dst/a`
        // pre-exists carrying the identity `/dst/A` will get when this operation makes it.
        let fs = FaultFs::new();
        for d in ["/src", "/src/A", "/src/a", "/dst", "/dst/a"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/src/A/x", b"x");
        fs.write_file("/src/a/y", b"y");
        let folded = FileIdentity::Strong(ObjectId { volume: 1, index: 9_003 });
        fs.set_identity("/dst/A", folded);
        fs.set_identity("/dst/a", folded);

        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new("a"));
        match &got[0].cause {
            TreeFailureCause::CreateDir(e) => assert_eq!(e.code, Code::DestinationNamespaceCollision),
            other => panic!("expected CreateDir, got {other:?}"),
        }
        assert!(fs.exists("/dst/A/x"));
        assert!(!fs.exists("/dst/a/y"), "the folded subtree was not merged");
        assert_eq!(out.failures.create_dir, 1);
    }

    #[test]
    fn a_source_directory_that_is_the_destination_aborts_the_operation() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_identity("/src/sub", identity_of(&fs, "/dst"));

        let (r, _) = run(&fs, "/src", "/dst", &opts());

        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(!fs.exists("/dst/sub/b"));
    }

    #[test]
    fn a_weak_directory_warns_under_default_and_aborts_under_strict() {
        let weak = FileIdentity::Weak(ObjectId { volume: 7, index: 1 });

        let lax = tree();
        lax.set_identity("/src/sub", weak);
        let (r, got) = run(&lax, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert!(got.is_empty());
        assert_eq!(out.warnings.weak[&7], DegradedGroup { count: 1, example: PathBuf::from("sub") });

        let tight = tree();
        tight.set_identity("/src/sub", weak);
        let (r, _) = run(&tight, "/src", "/dst", &strict());
        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
    }

    #[test]
    fn a_file_whose_destination_cannot_be_inspected_lands_in_the_unavailable_bucket() {
        // metadata calls in order: walk's root stat (1), copy_tree's source identity (2),
        // copy_file_at's source stat (3), the Step 2a gate's destination stat (4).
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/src/a", b"A");
        fs.fail_nth("metadata", 4, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        let stats: Vec<_> = fs.calls().into_iter().filter(|c| c.starts_with("metadata(")).collect();
        assert!(stats[3].contains("dst"), "the 4th stat is the gate's: {stats:?}");
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(out.files_copied, 1);
        assert_eq!(out.warnings.unavailable, Some(DegradedGroup { count: 1, example: PathBuf::from("a") }));
    }

    #[test]
    fn a_destination_without_the_no_replace_primitive_aborts_at_the_first_publish() {
        let fs = tree();
        fs.set_no_replace_support(false);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let e = r.unwrap_err();
        assert_eq!(e.code(), Code::NoReplacePublishUnavailable);
        assert_eq!(e.step, CopyStep::Publish);
        assert!(got.is_empty());
        assert!(!fs.exists("/dst/a.flux-partial.op1"), "the temporary was discarded");
        assert!(!fs.exists("/dst/sub"), "stopped at the first file, before `sub`");
    }

    #[test]
    fn a_create_failure_is_one_failure_not_an_abort_even_if_it_says_unsupported() {
        // Decision 3 is sound only at the PUBLISH step: `Unsupported` at `create_new`
        // is one file's failure. Without the step guard this would abort the tree.
        let fs = tree();
        fs.fail_kind("create_new", Code::IoError, std::io::ErrorKind::Unsupported);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        match &got[0].cause {
            TreeFailureCause::Copy(e) => assert_eq!(e.step, CopyStep::Create),
            other => panic!("expected Copy, got {other:?}"),
        }
        assert_eq!(out.files_copied, 1);
    }

    #[test]
    fn primitive_unavailable_is_unsupported_or_a_raw_einval_only() {
        assert!(primitive_unavailable(&std::io::Error::from(std::io::ErrorKind::Unsupported)));
        assert!(!primitive_unavailable(&std::io::Error::from(std::io::ErrorKind::InvalidInput)));
        assert!(!primitive_unavailable(&std::io::Error::from(std::io::ErrorKind::AlreadyExists)));
        #[cfg(unix)]
        assert!(primitive_unavailable(&std::io::Error::from_raw_os_error(22)), "EINVAL");
    }

    #[test]
    fn a_metadata_complaint_is_reported_with_the_file_published() {
        let fs = tree();
        fs.fail("set_times", Code::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        match &got[0].cause {
            TreeFailureCause::PublishedWithComplaints(v) => assert_eq!(v.len(), 1),
            other => panic!("expected PublishedWithComplaints, got {other:?}"),
        }
        assert!(fs.exists(Path::new("/dst").join(&got[0].path)), "the file is there");
        assert_eq!(out.files_copied, 2);
        assert_eq!(out.failures.published_with_complaints, 1);
    }

    #[test]
    fn a_symlink_is_reported_unsupported_and_never_copied() {
        let fs = tree();
        fs.add_symlink("/src/link");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, Path::new("link"));
        assert!(matches!(got[0].cause, TreeFailureCause::Unsupported(FileType::Symlink)));
        assert!(!fs.exists("/dst/link"));
        assert_eq!(out.failures.unsupported, 1);
    }

    #[test]
    fn a_walk_error_is_reported_and_the_walk_continues() {
        let fs = tree();
        // read_dir calls: the walk's root listing (1), then `sub` (2).
        fs.fail_nth("read_dir", 2, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, Path::new("sub"));
        assert!(matches!(got[0].cause, TreeFailureCause::Walk(_)));
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"A"[..]));
        assert!(!fs.exists("/dst/sub"), "the walk emitted no Dir for it");
        assert_eq!(out.failures.walk, 1);
    }

    #[test]
    fn a_failed_directory_is_reported_once_and_its_descendants_never_copied() {
        let fs = tree();
        fs.create_dir(Path::new("/src/sub/deeper")).unwrap();
        fs.write_file("/src/sub/deeper/c", b"c");
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/sub", b"a file where a directory belongs");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1, "reported once: {got:?}");
        assert_eq!(got[0].path, Path::new("sub"));
        assert!(matches!(got[0].cause, TreeFailureCause::CreateDir(_)));
        assert!(
            !fs.calls().iter().any(|c| c.contains("flux-partial") && !c.contains("a.flux-partial")),
            "nothing under `sub` reached copy_file_at: {:?}",
            fs.calls()
        );
        assert_eq!(out.files_copied, 1);
    }

    #[test]
    fn a_leftover_temporary_is_reported_by_its_destination_relative_path() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.create_dir(Path::new("/src/sub")).unwrap();
        fs.write_file("/src/sub/b", b"BB");
        fs.fail("rename_no_replace", Code::PermissionDenied);
        fs.fail_always("remove_file", Code::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        r.unwrap();
        match &got[0].cause {
            TreeFailureCause::Copy(e) => {
                let (path, _) = e.leftover.as_ref().expect("the leftover is reported");
                assert_eq!(path, Path::new("sub/b.flux-partial.op1"));
            }
            other => panic!("expected Copy, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run, expect FAIL to compile** (`copy_tree`, `lexically_within`, `primitive_unavailable` do not exist).
- [ ] **Step 3: Implement.** Replace `tree.rs`'s `use` block with:

```rust
use crate::copy::{CopyError, CopyStep, copy_file_at, split_destination, weaker};
use crate::walk::{WalkEvent, walk};
use flux_fs::{
    Code, CopyOptions, DestinationRoot, DirHandle, FileIdentity, FileType, FsError,
    MetadataFailure, ObjectId, Publish, Safety,
};
use std::collections::{BTreeMap, HashSet};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
```

  and append, after `impl WeakIdentityWarnings` and before `#[cfg(test)]`:

```rust
/// The directory one frame of the walk writes into, or a marker that its subtree was
/// refused. One entry per `Dir` the walk emits, popped at its `DirEnd`: `walk.rs`'s
/// `enter` pushes a frame of its own before it returns `Dir` and none on `Err`, and
/// `next` emits `DirEnd` for every such frame, so this stack cannot drift from the walk.
enum Frame<D> {
    Live {
        dir: D,
        /// `Strong` identities of the directories this operation created INSIDE `dir`.
        /// A fold is two source names in ONE source directory landing on one
        /// destination name, so both creations happen here; the set is dropped with the
        /// frame, which bounds it like the walk's one-directory listing (spec line 997,
        /// invariant 11).
        created: HashSet<ObjectId>,
    },
    /// Its directory failed; everything below it is skipped, and was reported once.
    Skipped,
}

/// Copy the tree at `src_root` into `dst_root`, writing only through directory
/// handles. See the design for the full contract; in brief:
///
/// - The outer `Err` means the operation did not happen or had to stop: the source
///   root missing or not a directory; the lexical floor; the §129 pre-flight (an
///   identity match, or a degraded comparison under `Safety::Strict`); a directory
///   reached mid-walk that is the destination by identity, or whose comparison is
///   degraded under `Strict`; a destination root that cannot be resolved or created;
///   and the first publish reporting that the no-replace primitive is unavailable.
/// - Every other failure goes to `on_failure`, and the walk continues.
/// - Every file is published with `Publish::NoReplace` whatever `opts.publish` says
///   (§241.5): an existing destination file is never replaced; it is reported
///   `DESTINATION_NAMESPACE_COLLISION`.
pub fn copy_tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<TreeOutcome, CopyError> {
    // 1. The source root. `walk` refuses a missing or non-directory root: the whole
    //    operation failing, before any destination call.
    let events = walk(fs, src_root).map_err(|e| CopyError::at(CopyStep::Source, e))?;
    let src_identity =
        fs.metadata(src_root).map_err(|e| CopyError::at(CopyStep::Source, e))?.identity;

    // 2. The lexical floor, before any destination call (§129's own example, `/data`
    //    into `/data/backup`). Runs at every identity strength.
    if lexically_within(dst_root, src_root) {
        return Err(refuse("the destination is the source or lies inside it"));
    }

    let mut out = TreeOutcome::default();

    // 3-5. Resolve the destination, compare it with the source, create the root.
    let resolve = |e| CopyError::at(CopyStep::Resolve, e);
    let root = match fs.destination_root(dst_root) {
        Ok(root) => {
            preflight(src_identity, root.identity().map_err(resolve)?, opts.safety, &mut out.warnings)?;
            root
        }
        Err(e) if e.source.kind() == ErrorKind::NotFound => {
            let (parent_path, name) = split_destination(dst_root)?;
            let parent = fs.destination_root(parent_path).map_err(resolve)?;
            preflight(src_identity, parent.identity().map_err(resolve)?, opts.safety, &mut out.warnings)?;
            // Decision 8 on the root, every failure fatal. Nothing else is created on
            // `parent`, so there is no sibling to fold against.
            match parent.create_dir(name) {
                Ok(root) => {
                    out.directories_created += 1;
                    root
                }
                Err(e) if e.source.kind() == ErrorKind::AlreadyExists => {
                    parent.open_dir(name).map_err(resolve)?
                }
                Err(e) => return Err(resolve(e)),
            }
        }
        Err(e) => return Err(resolve(e)),
    };
    let root_identity = root.identity().map_err(resolve)?;

    // 6. The walk. NoReplace is mandatory in a tree (§241.5): every target is planned
    //    as new, and an existing one is refused at Step 2a (F3).
    let opts = CopyOptions { publish: Publish::NoReplace, ..opts.clone() };
    let mut stack = vec![Frame::Live { dir: root, created: HashSet::new() }];
    for item in events {
        let live = matches!(stack.last(), Some(Frame::Live { .. }));
        let event = match item {
            Ok(event) => event,
            // Under a skipped subtree the walk still reads; its errors there belong to
            // a failure already reported once.
            Err(e) => {
                if live {
                    report(&mut out, on_failure, e.path, TreeFailureCause::Walk(e.cause));
                }
                continue;
            }
        };
        match event {
            WalkEvent::Dir { path, identity } => {
                let frame = if live {
                    enter_dir(&mut stack, &path, identity, root_identity, &opts, &mut out, on_failure)?
                } else {
                    Frame::Skipped
                };
                stack.push(frame);
            }
            WalkEvent::File { path } => {
                if let Some(Frame::Live { dir, .. }) = stack.last() {
                    copy_one(fs, src_root, dir, path, &opts, &mut out, on_failure)?;
                }
            }
            WalkEvent::Symlink { path } => {
                if live {
                    report(&mut out, on_failure, path, TreeFailureCause::Unsupported(FileType::Symlink));
                }
            }
            WalkEvent::Other { path } => {
                if live {
                    report(&mut out, on_failure, path, TreeFailureCause::Unsupported(FileType::Other));
                }
            }
            WalkEvent::DirEnd { .. } => {
                stack.pop();
            }
        }
    }
    Ok(out)
}

/// A `Dir` event under a live frame: the dynamic §129 check, then decision 8.
fn enter_dir<D: DirHandle>(
    stack: &mut [Frame<D>],
    path: &Path,
    identity: FileIdentity,
    root_identity: FileIdentity,
    opts: &CopyOptions,
    out: &mut TreeOutcome,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<Frame<D>, CopyError> {
    // The dynamic half of §129 / §149.6: a directory reached mid-walk that IS the
    // destination root means the source reached it by an alias. Checked before
    // anything is created for it.
    match (identity, root_identity) {
        (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b => {
            return Err(refuse("a source directory is the destination itself, by identity"));
        }
        (FileIdentity::Strong(_), FileIdentity::Strong(_)) => {}
        (s, d) => match opts.safety {
            Safety::Default => out.warnings.record(weaker(s, d), path),
            Safety::Strict => {
                return Err(refuse(
                    "a source directory cannot be compared with the destination with full confidence",
                ));
            }
        },
    }

    let Some(Frame::Live { dir: parent, created }) = stack.last_mut() else {
        unreachable!("enter_dir is called only under a live frame");
    };
    let name = path.file_name().expect("a walk path ends in a name");
    // Decision 8: create first; open on AlreadyExists. Neither call traverses a link.
    let failure = match parent.create_dir(name) {
        Ok(child) => {
            out.directories_created += 1;
            if let Ok(FileIdentity::Strong(id)) = child.identity() {
                created.insert(id);
            }
            return Ok(Frame::Live { dir: child, created: HashSet::new() });
        }
        Err(e) if e.source.kind() == ErrorKind::AlreadyExists => match parent.open_dir(name) {
            Ok(child) => match child.identity() {
                // Created by THIS operation moments ago, under another source name.
                Ok(FileIdentity::Strong(id)) if created.contains(&id) => FsError::new(
                    Code::DestinationNamespaceCollision,
                    std::io::Error::other(
                        "this operation already created that directory under another source name",
                    ),
                ),
                // Pre-existing: merge into it.
                _ => return Ok(Frame::Live { dir: child, created: HashSet::new() }),
            },
            // SAFETY_REJECTED (a link) or DESTINATION_ERROR (a file, or it vanished).
            Err(e) => e,
        },
        Err(e) => e,
    };
    report(out, on_failure, path.to_path_buf(), TreeFailureCause::CreateDir(failure));
    Ok(Frame::Skipped)
}

/// A `File` event under a live frame.
fn copy_one<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    parent: &F::Dir,
    path: PathBuf,
    opts: &CopyOptions,
    out: &mut TreeOutcome,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    let name = path.file_name().expect("a walk path ends in a name");
    match copy_file_at(fs, &src_root.join(&path), parent, name, opts) {
        Ok(o) => {
            out.files_copied += 1;
            out.bytes_copied += o.bytes_copied;
            if let Some(weaker) = o.identity_degraded {
                out.warnings.record(weaker, &path);
            }
            if !o.metadata_failures.is_empty() {
                report(out, on_failure, path, TreeFailureCause::PublishedWithComplaints(o.metadata_failures));
            }
            Ok(())
        }
        Err(mut e) => {
            // `copy_file_at` records a leftover relative to its handle; rebuild it in
            // the tree's frame, relative to the destination root.
            if let Some((p, _)) = e.leftover.as_mut() {
                *p = path.with_file_name(&*p);
            }
            // Decision 3: this destination lacks a no-replace primitive. Found at the
            // FIRST publish, so the whole operation stops instead of failing every file.
            if e.step == CopyStep::Publish && primitive_unavailable(&e.cause.source) {
                e.cause.code = Code::NoReplacePublishUnavailable;
                return Err(e);
            }
            // F3 and §241.5: the name was taken - by a file that already existed
            // (refused at Step 2a) or one that appeared before publish (the no-replace
            // rename refused it). `source` is kept, so `raw_os_error` survives.
            if matches!(e.step, CopyStep::Gate | CopyStep::Publish)
                && e.cause.source.kind() == ErrorKind::AlreadyExists
            {
                e.cause.code = Code::DestinationNamespaceCollision;
            }
            report(out, on_failure, path, TreeFailureCause::Copy(e));
            Ok(())
        }
    }
}

/// "Primitive unavailable" (decision 3). `Unsupported` covers ENOSYS and EOPNOTSUPP
/// (std maps both there) and the fake's `set_no_replace_support(false)`; a RAW `EINVAL`
/// on unix is the measured WSL 9p answer. An `InvalidInput` with no raw OS error is
/// one Flux raised itself (an interior NUL), not the filesystem. Sound only for a
/// failure at `CopyStep::Publish`: the temporary's name contains the target's, so a
/// name the filesystem rejects fails at `Create` first.
fn primitive_unavailable(e: &std::io::Error) -> bool {
    e.kind() == ErrorKind::Unsupported
        || (cfg!(unix) && e.kind() == ErrorKind::InvalidInput && e.raw_os_error().is_some())
}

/// The §129 pre-flight: the source root against the RESOLVED destination anchor
/// (decision 2). A destination symlinked to the source is caught here, by identity.
fn preflight(
    src: FileIdentity,
    anchor: FileIdentity,
    safety: Safety,
    warnings: &mut WeakIdentityWarnings,
) -> std::result::Result<(), CopyError> {
    match (src, anchor) {
        (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b => {
            Err(refuse("the destination resolves to the source, by identity"))
        }
        (FileIdentity::Strong(_), FileIdentity::Strong(_)) => Ok(()),
        (s, d) => match safety {
            Safety::Default => {
                warnings.record(weaker(s, d), Path::new(""));
                Ok(())
            }
            Safety::Strict => Err(refuse(
                "the source cannot be compared with the destination with full confidence",
            )),
        },
    }
}

/// `inner` equals `outer` or lies inside it, compared component by component with `.`
/// dropped and no filesystem access. `..` is not resolved (that needs the
/// filesystem): a spelling through `..` is compared as written, and identity is the
/// check that sees through it. Paths are compared as given; cut 5's CLI canonicalizes
/// both roots before calling the engine.
fn lexically_within(inner: &Path, outer: &Path) -> bool {
    let parts = |p: &Path| -> Vec<Component<'_>> {
        p.components().filter(|c| !matches!(c, Component::CurDir)).collect()
    };
    let (i, o) = (parts(inner), parts(outer));
    i.len() >= o.len() && i[..o.len()] == o[..]
}

fn refuse(why: &'static str) -> CopyError {
    CopyError::at(CopyStep::Resolve, FsError::new(Code::SafetyRejected, std::io::Error::other(why)))
}

/// Count the failure, then stream it. The only place either happens.
fn report(
    out: &mut TreeOutcome,
    on_failure: &mut dyn FnMut(TreeFailure),
    path: PathBuf,
    cause: TreeFailureCause,
) {
    out.failures.count(&cause);
    on_failure(TreeFailure { path, cause });
}
```

  Delete the two `#[cfg_attr(not(test), expect(dead_code, ...))]` lines Task 4 put on `FailureTally::count` and `WeakIdentityWarnings::record` (`copy_tree` now calls both; leaving them makes the unfulfilled expectation a warning, which `-D warnings` rejects).

  In `lib.rs`, add `copy_tree` to the tree re-export: `pub use tree::{DegradedGroup, FailureTally, TreeFailure, TreeFailureCause, TreeOutcome, WeakIdentityWarnings, copy_tree};`.

  If the borrow checker rejects a construction above (for example `resolve` used as both a closure and a function argument), the fix must keep every type, name and message; if it cannot, STOP and report.

- [ ] **Step 4: Run, expect PASS.** `cargo nextest run -p flux-core --no-fail-fast` (every test), then `just check`, `just check-linux`, `just check-mac`.
- [ ] **Step 5: Mutation checks** (each must turn the NAMED test red; restore after each):
  1. `lexically_within` returns `false` → `a_destination_inside_the_source_is_refused_lexically_before_any_call`.
  2. `preflight`'s equal arm returns `Ok(())` → `a_destination_that_is_the_source_by_identity_is_refused_before_anything_is_created`.
  3. In `enter_dir`, delete the `created.contains(&id)` arm → `two_source_directories_folded_into_one_destination_are_a_collision`.
  4. In `enter_dir`, the equal-identity arm does nothing → `a_source_directory_that_is_the_destination_aborts_the_operation`.
  5. Drop the `publish: Publish::NoReplace` override (use `opts` as given) → `a_fresh_tree_is_copied_through_handles_with_no_replace`.
  6. In `copy_one`, drop the `e.step == CopyStep::Publish &&` condition from the decision-3 check → `a_create_failure_is_one_failure_not_an_abort_even_if_it_says_unsupported`.
  10. In `primitive_unavailable`, drop `&& e.raw_os_error().is_some()` → `primitive_unavailable_is_unsupported_or_a_raw_einval_only`.
  7. Delete the collision remap in `copy_one` → `an_existing_destination_file_is_left_intact_and_reported_as_a_collision`.
  8. Push `Frame::Live` for a `Dir` under a skipped frame (reuse the parent) → `a_failed_directory_is_reported_once_and_its_descendants_never_copied`.
  9. Remove the leftover rebuild → `a_leftover_temporary_is_reported_by_its_destination_relative_path`.
  Record, per mutant, which tests went red.
- [ ] **Step 6: Commit** — `feat(flux-core): copy_tree, the safe engine`, with the mutation results in the body.

---

### Task 6: `copy_tree` on real filesystems

**Files:** `crates/flux-core/Cargo.toml`; create `crates/flux-core/tests/tree_std_fs.rs`.

- [ ] **Step 0: Verify state.** `crates/flux-core/Cargo.toml` has `[dependencies] flux-fs = { path = "../flux-fs" }` and no `[dev-dependencies]`; `crates/flux-platform/Cargo.toml`'s `[dependencies]` names only `flux-fs` (so a `flux-core -> flux-platform` dev-dependency makes no cycle); the root `Cargo.toml`'s `[workspace.dependencies]` has `tempfile = "3"`.
- [ ] **Step 1: Add the dev-dependencies** to `crates/flux-core/Cargo.toml`:

```toml
[dev-dependencies]
flux-platform = { path = "../flux-platform" }
tempfile = { workspace = true }
```

- [ ] **Step 2: Create `crates/flux-core/tests/tree_std_fs.rs`** (the oracle):

```rust
//! `copy_tree` against real filesystems, on every CI leg. The engine's rules are pinned
//! against `FaultFs` in `src/tree.rs`; this proves a real filesystem reports what they
//! rely on - identity, `AlreadyExists`, and a followed destination link.

use flux_core::copy::CopyError;
use flux_core::{TreeFailure, TreeFailureCause, TreeOutcome, copy_tree};
use flux_fs::{Code, CopyOptions, Durability, OperationId, Preserve, Publish, Safety};
use flux_platform::StdFileSystem;
use std::path::Path;
use tempfile::TempDir;

fn opts() -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability: Durability::Normal,
        publish: Publish::Replace,
        safety: Safety::Default,
        operation_id: OperationId::new("t"),
    }
}

fn run(src: &Path, dst: &Path) -> (Result<TreeOutcome, CopyError>, Vec<TreeFailure>) {
    let mut got = Vec::new();
    let r = copy_tree(&StdFileSystem, src, dst, &opts(), &mut |f| got.push(f));
    (r, got)
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn a_real_tree_is_copied_into_a_fresh_destination() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("src"), d.path().join("dst"));
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), b"A").unwrap();
    std::fs::write(src.join("sub").join("b.txt"), b"BB").unwrap();

    let (r, got) = run(&src, &dst);

    let out = r.unwrap();
    assert!(got.is_empty(), "{got:?}");
    assert_eq!((out.files_copied, out.bytes_copied, out.directories_created), (2, 3, 2));
    assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"A");
    assert_eq!(std::fs::read(dst.join("sub").join("b.txt")).unwrap(), b"BB");
    assert_eq!(names(&dst), ["a.txt", "sub"], "no temporary left behind");
}

#[test]
fn an_existing_destination_file_is_left_intact_and_reported() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("src"), d.path().join("dst"));
    std::fs::create_dir(&src).unwrap();
    std::fs::create_dir(&dst).unwrap();
    std::fs::write(src.join("a.txt"), b"new").unwrap();
    std::fs::write(dst.join("a.txt"), b"old").unwrap();

    let (r, got) = run(&src, &dst);

    r.unwrap();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].path, Path::new("a.txt"));
    match &got[0].cause {
        TreeFailureCause::Copy(e) => assert_eq!(e.code(), Code::DestinationNamespaceCollision),
        other => panic!("expected Copy, got {other:?}"),
    }
    assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"old");
    assert_eq!(names(&dst), ["a.txt"], "no temporary left behind");
}

#[cfg(unix)]
#[test]
fn a_destination_symlinked_to_the_source_is_refused_before_anything_is_created() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("src"), d.path().join("dst"));
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("a.txt"), b"A").unwrap();
    std::os::unix::fs::symlink(&src, &dst).unwrap();
    let before = names(&src);

    let (r, got) = run(&src, &dst);

    assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
    assert!(got.is_empty());
    assert_eq!(names(&src), before, "nothing was written into the source");
}
```

- [ ] **Step 3: Run, expect PASS.** `cargo nextest run -p flux-core --no-fail-fast --test tree_std_fs` on Windows (two tests), then `just check-linux` (three, including the symlink one), `just check-mac`, and `just check`. If a real filesystem fails one of these, STOP and report the output: it means a filesystem does not behave as the engine relies on, which is a design question.
- [ ] **Step 4: Commit** — `test(flux-core): copy_tree on real filesystems`.

---

### Task 7: bookkeeping

**Files:** `TODO.md`.

- [ ] **Step 0: Verify state.** `TODO.md` line `- [ ] \`flux-core\`: recursive directory copy` (under "Phase 2 — portable copy (next)"), and the entry beginning `- [ ] **Item 113's up-front no-replace probe is not built.**`.
- [ ] **Step 1:** Tick the Phase 2 line: `- [x] \`flux-core\`: recursive directory copy — done: \`copy_tree\` (cut 4b, \`crates/flux-core/src/tree.rs\`); the CLI surface is cut 5.`
- [ ] **Step 2:** Append to the END of the item-113 entry's body, as a new sentence on its own line indented like the entry: `Cut 4b delivers the interim: \`copy_tree\` aborts the whole operation with NOREPLACE_PUBLISH_UNAVAILABLE at the FIRST publish that reports the primitive unavailable (\`primitive_unavailable\`, \`crates/flux-core/src/tree.rs\`). The up-front probe still waits for the workspace.`
- [ ] **Step 3:** Add a new entry at the end of "Known gaps in the single-file copy (from the PR #32 capstone)", wrapped to the neighbours' width and indentation: `- [ ] **A tree copy never replaces an existing destination file.** \`copy_tree\` publishes every file no-replace and reports an existing one \`DESTINATION_NAMESPACE_COLLISION\`, untouched (cut 4b, F3), because §241.5's replacement claim needs the durable \`state.db\` of the operation workspace. So §5.1's default \`--overwrite\` is unmet for directories until that store exists; \`flux copy dir existing-dir\` reports one collision per pre-existing file.`
- [ ] **Step 4:** `typos TODO.md` → exit 0. **Commit** — `docs: record copy_tree in TODO (the interim item-113 abort, the unmet overwrite default)`.

---

### Task 8: final verification (no publishing)

- [ ] **Step 1:** `just check` → exit 0; record the Summary line.
- [ ] **Step 2:** `just check-linux` → exit 0; record the Summary line.
- [ ] **Step 3:** `just check-mac` → exit 0.
- [ ] **Step 4:** `git status --short` clean; `git log --oneline origin/main..HEAD` lists the spec commits and Tasks 1-7.
- [ ] **Step 5: STOP.** AGY-CAPSTONE runs next on the range, then AGY-TEST-AUDIT, then (owner-approved) `just pr`, which arms auto-merge.

## Self-review (done by the plan author)

- **Spec coverage:** API - `copy_tree` signature, outer-`Err` list, `TreeOutcome` / `TreeFailure` / causes incl. `PublishedWithComplaints` / `FailureTally` / warnings → Tasks 4-5; `CopyStep` with the spec's nine variants → Task 2; `DirHandle::identity` first, three arms → Task 1; algorithm steps 1-5 (source, lexical floor, destination resolution incl. `NotADirectory` and the absent-parent anchor, pre-flight incl. Strict, root creation) → Task 5 (`copy_tree` head); step 6 (handle stack, per-frame fold set, per-directory abort and Strict, decision 8, files with NoReplace forced, the Gate/Publish collision remap, decision 3's abort, `PublishedWithComplaints`, degraded warnings, leftover rebuild, per-file SAFETY_REJECTED continuing, `Symlink`/`Other`, `DirEnd`, walk errors) → Task 5 (`enter_dir`, `copy_one`, the loop); step 7 (the NoReplace gate after the identity rows) → Task 3; step 8 (no directory metadata) → nothing is written for it; testing → Task 5's tests plus Task 1's and Task 6's real-filesystem tests; out-of-scope and residues → nothing built for them, TODO entries in Task 7.
- **Placeholders:** none; every code step shows its code.
- **Consistency:** `CopyStep` variants, `CopyError::at`, `TreeFailureCause` variants, `FailureTally` fields, `DegradedGroup { count, example }`, and `copy_tree`'s signature are identical wherever they appear; test helper names (`tree`, `run`, `opts`, `strict`, `identity_of`, `calls_since`) are defined once, in Task 5 Step 1.
