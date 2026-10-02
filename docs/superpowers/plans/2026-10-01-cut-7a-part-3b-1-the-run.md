# Cut 7a Part 3b-1: the run, below the CLI - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Everything "The run" needs in `flux-fs` and `flux-core`, unit-tested on the fake:
- the engine's §99 guard before every destination mutation, and the reserved `.flux` control paths;
- `copy_tree` split so a caller can write through its own DEST handle;
- the single-file scan matched by the filesystem's own name equivalence;
- the record rewrite that keeps a crash from stranding a destination;
- the `run` module: a tree copy and a single-file copy under the destination's lock, from the capability gate to the
  release, including `--restart`, `--break-lock`'s state order, the rollback of a refused copy and the finish.

**Architecture:**
- `flux_fs::Code` gains `TargetLockBusy` and `ControlPlaneNamespaceConflict`, the two codes the ENGINE raises.
- `copy_file_guarded` and `copy_tree_at` take a guard, `&dyn Fn() -> flux_fs::Result<()>`, and call it before every
  create, rename and unlink at the destination. `copy_file_at` and `copy_tree` keep their signatures and pass a guard
  that always passes.
- `flux-core::run` holds the run. `run::tree` and `run::file` are its entry points. A private `Place` trait holds what
  differs between a tree (workspaces under `DEST/.flux/operations/`) and a single file (records beside it).
- Nothing here changes the CLI. Part 3b-2 wires `run::tree` / `run::file` into `flux copy`, adds the flags, the exit
  codes, the crash hook and the end-to-end tests, planned against this part's code once it lands.

**Tech Stack:** Rust 2024 (let chains); no new dependency.

**Spec:** `docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md`, as amended by Task 11 of this plan:
"The run", "`--restart` and `--break-lock`", "§21.1 classification", "What each crash window leaves", "Errors and
exit statuses".

---

## What this plan rests on (read and verified at `ce9cc98`)

- `crates/flux-fs/src/error.rs`:
  - `pub enum Code` (`:11-43`), whose last variant is `IoError,` (`:42`);
  - `fn as_str` (`:46-63`), whose last arm is `Code::IoError => "IO_ERROR",` (`:61`);
  - the test `every_code_has_the_spec_string` (`:112-128`), ending with the `SymlinkCreationUnavailable` assertion
    (`:127`).
- `crates/flux-core/src/copy.rs`:
  - `fn discard` (`:131-158`);
  - `pub(crate) fn split_destination` (`:170-187`);
  - `fn identity_gate` (`:212-268`), private today;
  - `pub fn copy_file` (`:283-318`), whose Step 0 refusal is `src == dst` with the message `"source and destination
    are the same path"` (`:296-305`);
  - `pub fn copy_file_at` (`:320-452`): the step-1 sweep `let _ = parent.remove_file(&temp);` (`:338`), the exclusive
    create (`:359`), the `discard(` calls (`:368`, `:371`, `:380`, `:396`, `:411`, `:432`, `:434`, `:438`,
    `:449` - nine call sites), the publish `let published = match opts.publish {` (`:441`);
  - `mod tests` with `fn opts()` (`operation_id: OperationId::new("op1")`, `publish: Publish::Replace`).
- `crates/flux-core/src/tree.rs`:
  - `use crate::copy::{CopyError, CopyStep, copy_file_at, split_destination, weaker};` (`:6`) and
    `use crate::walk::{WalkEvent, walk};` (`:7`);
  - `impl TreeAbort { pub fn changed }` (`:52-58`);
  - `pub fn copy_tree` (`:207-221`) and `fn run_tree` (`:221-345`: steps 1-2 at `:231-241`, steps 3-5 at `:243-279`,
    the walk loop at `:281-343`);
  - `fn enter_dir` (`:347-407`), `fn copy_one` (`:409-466`), `fn preflight` (`:476-497`, private), `fn refuse`
    (`:512-514`), `fn report` (`:517-527`);
  - `mod tests` (`:529`), with `fn opts()`, `fn tree()` (`/src/a`, `/src/sub/b`) and `fn run(...)`.
- `crates/flux-core/src/walk.rs`: `pub struct Walk<'a, F: FileSystem>` (`:72`); `pub fn walk` (`:135`); a directory's
  identity is read when the walk ENTERS it (`Walk::enter`), lazily, not at `walk()`; `WalkError { path, cause }`
  (`:52-56`).
- `crates/flux-core/src/state.rs`:
  - the constants `FLUX_DIR`, `OPERATIONS_DIR`, `MANIFEST`, `TEMP_SUFFIX`, `CREATING_SUFFIX`, `RECORD_INFIX`
    (`:217-228`), `record_name` (`:231`), `temp_name` (`:239`), `file_names_fit` (`:248`);
  - `write_state` (`:255-270`), `read_state` (`:274-279`), `create_workspace` (`:289-304`), `operations_dir`
    (`:309-313`), `control_dir` (`:315-331`), `control_path_conflict` (`:335`, `pub(crate)`);
  - its tests use `fn dest()` (`/p/dest`) and `fn position`.
- `crates/flux-core/src/prior.rs`: `scan_tree` (`:34-85`), `scan_file` (`:89-139`), `resumable_refusal` (`:142`),
  `fn corrupt` (`:170`), `fn existing_dir` (`:183-198`); its tests `a_single_file_scan_reads_only_its_targets_records`,
  `what_changes_after_the_listing_is_passed_over` (a `read_file` hook) and `a_non_utf8_targets_records_are_found`.
- `crates/flux-core/src/lock/`:
  - `held.rs`: `Held` (fields `pub(crate)`: `dir`, `lock_name`, `lock`, `identity`, `record`), `write_record`
    (`:41-47`, sets `record` only after the write and flush succeed), `still_owned` (`:52-70`; its first line returns
    `false` when no record has been written), `release` (`:74-80`), `discard` (`:85-97`, asserts no record);
  - `takeover.rs`: `Claimed::overwrite` (`:93-108`) -> `Overwritten::{Held, Restart}`;
  - `obtain.rs`: `MAX_ATTEMPTS = 8`, `Mode::{Plain, BreakLock}`, `Obtained::{Held { held, leftover_broken },
    Claimed}`, `check_capability`, `obtain(site, capability, mode, operation_id)`;
  - `site.rs`: `LockSite::{directory, file, root}`, `dir()`, `lock_name()`, `target_path_key()`,
    `complete_lock_key()`; `workspace` (`:141-170`) treats a dead owner's `workspace_path` of `none` as `Trusted` and
    `operations/<id>` / `adjacent/<id>` whose state is missing as `Missing`;
  - `classify.rs:45-50`: `Missing` and `Untrusted` both become `Classified::Untrusted` (`ARTIFACT_OWNERSHIP_UNCERTAIN`);
  - `error.rs`: `LockCode` (9 variants), `Refusal { code, holder, detail }` (all `pub`), `LockError::{Refused(Box<
    Refusal>), Io(FsError)}`;
  - `record.rs`: `LockRecord` (8 `pub` fields), `encode`, `decode` -> `Decoded::{Record, Uncertain, Foreign}`;
  - `test_support.rs` (`pub(crate)`): `fake()` (`/p`), `record(site, id, workspace_path)`, `dead_lock`, `live_lock`,
    `refusal`.
- `crates/flux-core/src/fault_fs.rs`: call names recorded as `create_lock(<path>)`, `write_at_start(<lock path>)`,
  `lock_sync_all(<lock path>)`, `create_new(<path>)`, `remove_file(<path>)`, `create_dir(<path>)`,
  `rename_no_replace(<from> -> <to>)`, `rename_replace(<from> -> <to>)`, `remove_dir(<path>)`, `sync_dir(<path>)`.
  `on_nth(name, n, action)` runs `action` just BEFORE the n-th call of `name`, and `fail_nth(name, n, code, kind)`
  fails that call; both are checked inside `record`, hook first (`:633-660`). `fail_write(e)` fails the next write
  on a handle from `create_new`. `set_identity`, `set_lock_capability`, `set_no_replace_support`, `set_type`,
  `add_symlink`. On Windows a joined path displays with `\`, so a test compares call strings after replacing `\` with
  `/`.
- `models/lockproto/impl-map.toml`: `S99_write`, `S21_1_s3`, `S21_1_s5_write_begin`, `S21_1_s5_write_end` and
  `S21_1_s3_refuse_close` are `status = "part-3"`. `tests/model_impl_map.rs` requires an `item`'s function region
  (its doc comment and body) to mention the label as a whole word.
- The repo's clippy gate denies warnings; no `#[allow(clippy::too_many_arguments)]` exists in the repo, so no function
  here takes more than seven arguments.

## Decisions (AGY-FIRST, then the owner, 2026-10-01)

Seams `.clavity/seams/cut7a-p3b-forks.md`, `-forks-2.md`, `-forks-3.md`; replies in `.clavity/scratch/cut7a-p3b/`.
Earlier rulings that still bind (Part 3 forks, 2026-09-30): the guard is a `&dyn Fn` parameter failing with
`Code::TargetLockBusy` (C1); the run is a generic `flux-core` module (D1); the run creates DEST itself (E1); reserved
paths match ASCII case-insensitively (F3); the `.broken` warning covers only what the run sees (G2); one `RunError`
type (H1); the `--restart` sweep walks by path and deletes through handles, and a partial it cannot delete keeps its
prior's ABANDONED state as its record (J1).

1. **A1 - the terminal state.** A copy whose walk finished is COMPLETED, even with per-entry failures (exit 1 stays
   the copy's). FAILED is only for an abort, a single-file copy error, or a failure of the run's own steps. Otherwise
   every repeat copy into a non-empty DEST (collisions) would need `--restart`.
2. **B1 - the source side first.** The source checks (it exists, its type, the lexical floor) and DEST's resolution
   and identity pre-flight run before the lock: a mistyped source creates nothing. DEST is resolved through its
   parent, which holds the lock, and a symlink at DEST is refused `SAFETY_REJECTED` (the CLI gives a canonical DEST,
   so only a dangling link reaches it).
3. **C1 - the copy writes through the run's DEST handle.** `copy_tree_at` takes the root handle and returns it.
4. **D1 - a takeover that must start again keeps its state.** When `Claimed::overwrite` finds the lock path moved
   (`Overwritten::Restart`) after this run's state exists, the run tries again (budget `MAX_ATTEMPTS`) and reuses that
   state. A refusal after it exists is exit 1, and the state stays for a later `--restart`.
5. **E3 - a record is found by name lookup.** For each `*.flux-state.<id>` in the target's directory, the scan looks
   up `<target>.flux-state.<id>` by THIS target's name; the filesystem's own equivalence (case, normalization)
   decides, and `NotFound` means another target's. Exact bytes miss `t`'s record for a target spelled `T` on NTFS or
   APFS; folding in Flux would claim `T`'s record for `t` on Linux. The single-file `--restart` partial is looked up
   the same way.
6. **F1 (3b-2) - the crash hook** is debug-build only; not in this part.
7. **G2 - no finish-time retry.** A temporary the copy could not remove keeps the COMPLETED state as its record, and
   finish steps 3-4 are skipped.
8. **Q-H (a) - `--restart` writes this run's state and record first.** Then each prior is set ABANDONED, its partials
   are deleted, then its state, with a full `still_owned` before every mutation. Under F5 the lock has no record before
   step 5, and `still_owned` is false without one; the model's `Recover`/`TakeOver` also write the record before
   `S21_1_s3`. A crash mid-restart then leaves a dead owner's record and this run's CREATED state (the next run gets
   `RESUMABLE_OPERATION_EXISTS`, cleared by `--restart`), not an empty lock.
9. **Q-I - a refusal of the copy that changed nothing is rolled back.** `SAFETY_REJECTED` or
   `NOREPLACE_PUBLISH_UNAVAILABLE` with nothing changed: the run removes its state, the control directories it
   emptied and a DEST it made, releases the lock, and the refusal keeps exit 3 (§55). A failure while doing so is exit
   1, naming the path. The control directories go only when empty, exactly as at the finish (finish step 4), so a
   `.flux` the operator had left empty goes too; an empty directory named `.flux` holds nothing (panel round 1).
10. **Q-K - the record stops naming the state before the state goes.** Before any removal of this run's own state
    (the finish, the rollback), the held record is rewritten in place with `workspace_path` = `none`. Otherwise a crash
    between the state's removal and the lock's unlink leaves a dead owner's record naming missing state:
    `ARTIFACT_OWNERSHIP_UNCERTAIN`, which no flag clears. Residue: a crash during the 4096-byte rewrite leaves a torn
    record, `TARGET_LOCK_UNCERTAIN`, which `--restart --break-lock` clears.
11. **J2 - Part 3b is cut in two.** This plan is 3b-1.

### Decisions this plan makes (no fork: each follows from a spec rule or the code)

12. **An I/O failure of the run's own steps** before the record exists gives back the lock (`discard`, or a claimed
    lock is closed) and reports `RunError::Failed`; this run's state, if it exists, stays (a later `--restart`
    supersedes it). After the record exists it is "Failure: state FAILED, release the lock the same way" (spec,
    Finish), best effort, with the first failure reported.
13. **Lost ownership** (a failed `still_owned`) before COMPLETED stops the run: the state stays as it is and the lock
    is closed without unlinking (`S99_refuse_close`); the copy's own abort says `TARGET_LOCK_BUSY`, or the run's
    `RunError::Refused { code: TargetLockBusy, changed: true }` does.
14. **The capability gate checks the lock's directory** (DEST's parent, or DEST for a root): that filesystem's locks
    are the ones the protocol relies on.
15. **The reserved-path check is in `copy_tree_at`**, so it applies to every tree copy. A source DIRECTORY at
    `.flux/{operations,standalone,atomic}` or below is reported `CreateDir(CONTROL_PLANE_NAMESPACE_CONFLICT)` and its
    subtree skipped; a source FILE there is reported `Copy(... CONTROL_PLANE_NAMESPACE_CONFLICT)` at `CopyStep::Gate`.
16. **`write_state` removes a stale `<name>.tmp` first** (the Part 3a handoff: a temporary left by an earlier failed
    write would make `create_new` fail forever). The writer holds the lock, and the name carries the operation's id.
17. **A workspace is retired before it is removed** (refinement 9): `operations/<id>` is renamed to
    `operations/<id>.removing` (which the scan passes over), then its `manifest`, a crash's `manifest.tmp` and the
    directory are removed. A record is removed with its `.tmp`.
18. **The copy's guard before an unlink** (`discard`, the step-1 sweep): without the lock the temporary stays and is
    reported as a leftover - an unlink is a mutation (§99, spec:4858-4870).

## File structure

- Modify `crates/flux-fs/src/error.rs` - two `Code` variants (Task 1).
- Modify `crates/flux-core/src/copy.rs` - `Guard`, `unguarded`, `copy_file_guarded`, `prepare_file` (Task 2, Task 9).
- Modify `crates/flux-core/src/tree.rs` - `Source`, `prepare_source`, `Shared`, `copy_tree_at`, `reserved_path`,
  `TreeAbort::refused_unchanged`; `preflight` becomes `pub(crate)` (Task 3).
- Modify `crates/flux-core/src/state.rs` - `RESERVED_DIRS` (Task 3); `PARTIAL_INFIX`, `REMOVING_SUFFIX`, `id_after`,
  `retire_workspace`, `remove_record`, `remove_empty_control_dirs`; `write_state` clears a stale temporary (Task 4).
- Modify `crates/flux-core/src/prior.rs` - `check_control_plane` (Task 4); `scan_file` by name lookup (Task 5).
- Modify `crates/flux-core/tests/state_std_fs.rs` - the real filesystem's name equivalence (Task 5).
- Modify `crates/flux-core/src/lock/held.rs` - `rewrite_record` (Task 6).
- Create `crates/flux-core/src/run/mod.rs` - the public types, `tree`, `file`, the finish (Tasks 7, 8, 9).
- Create `crates/flux-core/src/run/session.rs` - steps 3-5, ownership checks, error mapping (Task 7).
- Create `crates/flux-core/src/run/place.rs` - `Place`, `TreePlace`, `FilePlace`, `locate_tree` (Tasks 7, 8, 9).
- Create `crates/flux-core/src/run/restart.rs` - the supersede (Task 8).
- Create `crates/flux-core/src/run/tests.rs` - the run's tests on the fake (Tasks 7-9).
- Modify `crates/flux-core/src/lib.rs` - `pub mod run;` (Task 7).
- Modify `models/lockproto/impl-map.toml` - five labels gain items (Task 10).
- Modify `docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md` - refinements 12-19 and the crash
  table (Task 11).

## Commands

- One crate's library tests: `cargo test -p flux-core --lib <filter>`.
- The gates: `just check` (Windows: clippy + every test via nextest), `just check-linux` (WSL), `just check-mac`
  (cross-compiled clippy). Expected tail: `Summary [...] N tests run: N passed, 3 skipped` for the first two, and a
  clean `Finished` for the third. Baseline at `ce9cc98`: 479, 465.
- `PYTHONUTF8=1` is set for the user; nothing in this plan runs Python.

---

### Task 1: The engine's two new codes

**Files:**
- Modify: `crates/flux-fs/src/error.rs:42`, `:61`, `:127`

- [ ] **Step 1: Add the failing assertions** to `every_code_has_the_spec_string`, after the
  `SymlinkCreationUnavailable` line (`:127`):

```rust
        assert_eq!(Code::TargetLockBusy.as_str(), "TARGET_LOCK_BUSY");
        assert_eq!(Code::ControlPlaneNamespaceConflict.as_str(), "CONTROL_PLANE_NAMESPACE_CONFLICT");
```

- [ ] **Step 2: Run it to see it fail to compile**

Run: `cargo test -p flux-fs --lib every_code_has_the_spec_string`
Expected: `error[E0599]: no variant or associated item named `TargetLockBusy``.

- [ ] **Step 3: Add the variants** after `IoError,` (`:42`):

```rust
    /// §96, §99 (cut 7a). The destination's target lock belongs to another run, or this run lost it mid-run (a failed
    /// `still_owned`). The engine raises it from its guard and stops the whole operation, never one file.
    TargetLockBusy,
    /// §259.3 (cut 7a). A source entry whose destination is a reserved Flux control path - `DEST/.flux/operations`,
    /// `standalone` or `atomic`, or below one. Path-scoped: that entry fails, and the rest of the copy continues.
    ControlPlaneNamespaceConflict,
```

and the arms after `Code::IoError => "IO_ERROR",` (`:61`):

```rust
            Code::TargetLockBusy => "TARGET_LOCK_BUSY",
            Code::ControlPlaneNamespaceConflict => "CONTROL_PLANE_NAMESPACE_CONFLICT",
```

- [ ] **Step 4: Run it to see it pass**

Run: `cargo test -p flux-fs --lib every_code_has_the_spec_string`
Expected: `test result: ok. 1 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-fs/src/error.rs
git commit -m "feat(fs): TARGET_LOCK_BUSY and CONTROL_PLANE_NAMESPACE_CONFLICT codes for the engine (cut 7a Part 3b-1)"
```

---

### Task 2: The copy's guard (§99 before every mutation)

**Files:**
- Modify: `crates/flux-core/src/copy.rs` (`discard` `:131-158`; `copy_file_at` `:320-452`; `mod tests`)

The tests are written below; implement until they pass.

- [ ] **Step 1: Add the failing tests** at the end of `mod tests` in `copy.rs`:

```rust
    fn lost() -> flux_fs::Result<()> {
        Err(FsError::new(Code::TargetLockBusy, std::io::Error::other("lost")))
    }

    /// A guard that passes its first `owned` calls and fails every one after, counting them.
    fn guard_failing_after(owned: u32, calls: &std::cell::Cell<u32>) -> impl Fn() -> flux_fs::Result<()> + '_ {
        move || {
            calls.set(calls.get() + 1);
            if calls.get() <= owned { Ok(()) } else { lost() }
        }
    }

    #[test]
    fn a_guard_that_fails_first_stops_the_copy_before_anything_is_touched() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let e = copy_file_guarded(&fs, Path::new("/src"), &root, OsStr::new("dst"), &opts(), &lost)
            .unwrap_err();
        assert_eq!((e.code(), e.step), (Code::TargetLockBusy, CopyStep::Create));
        assert!(!fs.called("remove_file") && !fs.called("create_new"), "{:?}", fs.calls());
        assert!(!fs.exists("/dst"));
    }

    #[test]
    fn a_guard_that_fails_at_the_publish_keeps_the_temporary_and_never_renames() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        // Owned for the step-1 sweep and the create (calls 1 and 2), lost at the publish (call 3).
        let guard = guard_failing_after(2, &calls);
        let e = copy_file_guarded(&fs, Path::new("/src"), &root, OsStr::new("dst"), &opts(), &guard)
            .unwrap_err();
        assert_eq!((e.code(), e.step), (Code::TargetLockBusy, CopyStep::Publish));
        assert_eq!(calls.get(), 3);
        assert!(!fs.exists("/dst"), "never published");
        assert!(fs.exists("/dst.flux-partial.op1"), "an unlink is a mutation too: the temporary stays");
        let (left, _) = e.leftover.as_ref().expect("the kept temporary is reported");
        assert_eq!(left, Path::new("dst.flux-partial.op1"));
        assert!(!fs.called("rename_"), "{:?}", fs.calls());
    }

    #[test]
    fn a_guard_that_fails_before_a_discard_keeps_the_temporary_as_a_leftover() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail_write(std::io::Error::other("injected write"));
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        // Owned for the sweep and the create; lost when the failed copy would remove its temporary (call 3).
        let guard = guard_failing_after(2, &calls);
        let e = copy_file_guarded(&fs, Path::new("/src"), &root, OsStr::new("dst"), &opts(), &guard)
            .unwrap_err();
        assert_eq!(e.step, CopyStep::Stream);
        assert!(e.leftover.is_some(), "the kept temporary is reported");
        assert!(fs.exists("/dst.flux-partial.op1"));
        let removals = fs.calls().iter().filter(|c| c.starts_with("remove_file(")).count();
        assert_eq!(removals, 1, "only the step-1 sweep, never the discard: {:?}", fs.calls());
    }
```

- [ ] **Step 2: Run them to see them fail to compile**

Run: `cargo test -p flux-core --lib copy::tests::a_guard`
Expected: `error[E0425]: cannot find function `copy_file_guarded``.

- [ ] **Step 3: Implement.** In `copy.rs`, after the `// DELIBERATELY NO `impl From<FsError> for CopyError`.`
  comment block (before `fn discard`), add:

```rust
/// §99's check before a destination mutation (cut 7a Part 3b): `Ok` while the run still owns the destination's lock.
/// The run's guard fails with `Code::TargetLockBusy`, and the copy then makes no further mutation. A copy that holds no
/// lock passes `&unguarded`.
pub type Guard<'g> = dyn Fn() -> flux_fs::Result<()> + 'g;

/// The guard of a copy that holds no lock.
pub(crate) fn unguarded() -> flux_fs::Result<()> {
    Ok(())
}
```

  Give `discard` the guard as its last parameter and check it first:

```rust
fn discard<D: DirHandle>(
    parent: &D,
    temp: &OsStr,
    step: CopyStep,
    code: Code,
    source: std::io::Error,
    guard: &Guard<'_>,
) -> CopyError {
    // §99: an unlink is a mutation too (spec:4858-4870). Without the lock the temporary stays, reported as a
    // leftover, with the guard's failure as the reason it was kept.
    if let Err(lost) = guard() {
        return CopyError {
            cause: FsError::new(code, source),
            leftover: Some((std::path::PathBuf::from(temp), lost.source)),
            step,
        };
    }
    match parent.remove_file(temp) {
```

  (the rest of `discard` is unchanged). Replace the header of `copy_file_at` (`:320-332`, its doc comment through
  `) -> std::result::Result<Outcome, CopyError> {`) with the wrapper and the new function's header:

```rust
/// Copy `src` to `name` inside `parent`, writing ONLY through `parent`, holding no lock: `copy_file_guarded` with a
/// guard that always passes.
pub fn copy_file_at<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    parent: &F::Dir,
    name: &OsStr,
    opts: &CopyOptions,
) -> std::result::Result<Outcome, CopyError> {
    copy_file_guarded(fs, src, parent, name, opts, &unguarded)
}

/// Copy `src` to `name` inside `parent`, writing ONLY through `parent`.
///
/// Nothing below re-resolves a destination path: the staging temporary, the
/// metadata, the publish and the cleanup all go through the handle, so a parent
/// swapped for a link after it was opened cannot redirect the write (item 114).
///
/// `guard` runs before every destination mutation - the step-1 sweep, the exclusive create, the publishing rename and a
/// failed copy's removal of its temporary - which is §99's `S99_check` before each `S99_write` (cut 7a Part 3b). A
/// failed guard stops the copy at that point: before the create it creates nothing, and at the publish or a removal
/// the temporary stays, reported as `leftover`.
pub fn copy_file_guarded<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    parent: &F::Dir,
    name: &OsStr,
    opts: &CopyOptions,
    guard: &Guard<'_>,
) -> std::result::Result<Outcome, CopyError> {
```

  Before the step-1 sweep (`:338`), insert:

```rust
    guard().map_err(|e| CopyError::at(CopyStep::Create, e))?;
```

  Before the exclusive create (`:359`, `let mut writer = parent.create_new(&temp)...`), insert:

```rust
    // §99 before the temporary exists: a failure here has created nothing.
    guard().map_err(|e| CopyError::at(CopyStep::Create, e))?;
```

  Before `let published = match opts.publish {` (`:441`), insert:

```rust
    // §99 (`S99_check`, then the `S99_write` below): publish only while the lock is still this run's. Otherwise the
    // temporary stays - removing it would be a mutation too - and is reported as the leftover.
    if let Err(lost) = guard() {
        return Err(CopyError {
            leftover: Some((
                std::path::PathBuf::from(&temp),
                std::io::Error::other("kept: this run no longer holds the destination's lock"),
            )),
            ..CopyError::at(CopyStep::Publish, lost)
        });
    }
```

  Append `, guard` as the last argument of all nine `discard(` calls in `copy_file_guarded` (`:368`, `:371`, `:380`,
  `:396`, `:411`, `:432`, `:434`, `:438`, `:449`); for the multi-line calls (`:380`, `:396`, `:411`) add `guard,` as
  the line before the closing `)`.

- [ ] **Step 4: Run the tests to see them pass, and the whole crate stay green**

Run: `cargo test -p flux-core --lib`
Expected: `test result: ok. 247 passed` (244 + 3).

- [ ] **Step 5: Prove each new test can fail.** Temporarily delete the publish guard block; run
  `cargo test -p flux-core --lib copy::tests::a_guard_that_fails_at_the_publish`; expect FAILED. Restore. Temporarily
  delete the `if let Err(lost) = guard()` block in `discard`; expect
  `a_guard_that_fails_before_a_discard_keeps_the_temporary_as_a_leftover` FAILED. Restore. Temporarily delete the guard
  before the step-1 sweep; expect `a_guard_that_fails_first_stops_the_copy_before_anything_is_touched` FAILED. Restore
  and confirm `git diff` shows only the intended change.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-core/src/copy.rs
git commit -m "feat(core): copy_file_guarded runs a guard before every destination mutation (cut 7a Part 3b-1)"
```

---

### Task 3: `copy_tree` split, the guard in a tree, and the reserved control paths

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`:6-7`, `:52-58`, `:221-466`, `:476`, `mod tests`)

The tests are written below; implement until they pass.

- [ ] **Step 1: Add the failing tests** at the end of `mod tests` in `tree.rs`:

```rust
    #[test]
    fn a_reserved_control_path_is_matched_case_insensitively_and_nothing_else_is() {
        for p in [".flux/operations", ".flux/standalone/x", ".FLUX/Atomic", ".Flux/OPERATIONS/a/b"] {
            assert!(reserved_path(Path::new(p)), "{p}");
        }
        for p in [".flux", ".flux/other", ".flux/operationsx", "x/.flux/operations", "flux/operations", "a"] {
            assert!(!reserved_path(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn a_source_entry_landing_in_a_reserved_control_path_fails_and_the_rest_is_copied() {
        let fs = FaultFs::new();
        for d in ["/src", "/src/.flux", "/src/.flux/operations", "/src/.flux/other", "/src/.FLUX"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.create_dir(Path::new("/src/.FLUX/Standalone")).unwrap();
        fs.write_file("/src/.flux/operations/x", b"x");
        fs.write_file("/src/.flux/atomic", b"y");
        fs.write_file("/src/.flux/other/z", b"z");
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();
        let mut conflicts: Vec<String> = got
            .iter()
            .filter(|f| match &f.cause {
                TreeFailureCause::CreateDir(e) => e.code == Code::ControlPlaneNamespaceConflict,
                TreeFailureCause::Copy(e) => e.code() == Code::ControlPlaneNamespaceConflict,
                _ => false,
            })
            .map(|f| f.path.to_string_lossy().replace('\\', "/"))
            .collect();
        conflicts.sort();
        assert_eq!(conflicts, [".FLUX/Standalone", ".flux/atomic", ".flux/operations"]);
        assert_eq!(fs.read_file("/dst/.flux/other/z").as_deref(), Some(&b"z"[..]), "the rest of .flux is data");
        assert!(!fs.exists("/dst/.flux/operations") && !fs.exists("/dst/.flux/atomic"));
        assert!(!fs.exists("/dst/.FLUX/Standalone"));
        assert_eq!(out.failures.total(), 3);
    }

    /// `copy_tree_at` into a fresh `/dst`, with `guard`.
    fn guarded(fs: &FaultFs, guard: &Guard<'_>) -> (std::result::Result<(), CopyError>, TreeOutcome) {
        let source = prepare_source(fs, Path::new("/src"), Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let o = opts();
        let cx =
            Shared { fs, src_root: Path::new("/src"), src_identity: source.identity, opts: &o, guard };
        let mut out = TreeOutcome::default();
        let (_root, r) = copy_tree_at(&cx, source.events, root, &mut out, &mut |_| {});
        (r, out)
    }

    fn failing_after(owned: u32, calls: &std::cell::Cell<u32>) -> impl Fn() -> flux_fs::Result<()> + '_ {
        move || {
            calls.set(calls.get() + 1);
            if calls.get() <= owned {
                Ok(())
            } else {
                Err(FsError::new(Code::TargetLockBusy, std::io::Error::other("lost")))
            }
        }
    }

    #[test]
    fn a_failed_guard_aborts_the_whole_tree_with_target_lock_busy() {
        let fs = tree();
        let calls = std::cell::Cell::new(0);
        // `a` first: its sweep (1) passes, its create (2) fails.
        let (r, out) = guarded(&fs, &failing_after(1, &calls));
        assert_eq!(r.unwrap_err().code(), Code::TargetLockBusy);
        assert_eq!((out.files_copied, out.failures.total()), (0, 0), "an abort, never a per-file failure");
        assert!(!fs.exists("/dst/a") && !fs.exists("/dst/sub"));
    }

    #[test]
    fn the_guard_runs_before_each_directory_is_created() {
        let fs = tree();
        let calls = std::cell::Cell::new(0);
        // `a`: sweep, create, publish (1-3); then `sub`'s creation (4) fails.
        let (r, out) = guarded(&fs, &failing_after(3, &calls));
        assert_eq!(r.unwrap_err().code(), Code::TargetLockBusy);
        assert_eq!(calls.get(), 4);
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"A"[..]));
        assert!(!fs.exists("/dst/sub"));
        assert_eq!(out.directories_created, 0);
    }

    #[test]
    fn copy_tree_at_hands_the_root_back() {
        let fs = tree();
        let (r, _) = guarded(&fs, &unguarded);
        r.unwrap();
        let source = prepare_source(&fs, Path::new("/src"), Path::new("/dst")).unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let o = opts();
        let cx = Shared { fs: &fs, src_root: Path::new("/src"), src_identity: source.identity, opts: &o, guard: &unguarded };
        let mut out = TreeOutcome::default();
        let (back, r) = copy_tree_at(&cx, source.events, root, &mut out, &mut |_| {});
        assert!(r.is_ok());
        assert_eq!(back.identity().unwrap(), identity_of(&fs, "/dst"), "the same directory");
    }

    #[test]
    fn a_refused_unchanged_abort_is_exactly_the_exit_3_rule() {
        let abort = |code, f: fn(&mut TreeOutcome)| {
            let mut outcome = TreeOutcome::default();
            f(&mut outcome);
            TreeAbort { error: CopyError::at(CopyStep::Resolve, FsError::new(code, std::io::Error::other("x"))), outcome }
        };
        assert!(abort(Code::SafetyRejected, |_| {}).refused_unchanged());
        assert!(abort(Code::NoReplacePublishUnavailable, |_| {}).refused_unchanged());
        assert!(!abort(Code::IoError, |_| {}).refused_unchanged());
        assert!(!abort(Code::TargetLockBusy, |_| {}).refused_unchanged());
        assert!(!abort(Code::SafetyRejected, |o| o.files_copied = 1).refused_unchanged());
        assert!(!abort(Code::SafetyRejected, |o| o.failures.walk = 1).refused_unchanged());
    }
```

- [ ] **Step 2: Run them to see them fail to compile**

Run: `cargo test -p flux-core --lib tree::tests`
Expected: `error[E0425]: cannot find function `reserved_path`` (and `prepare_source`, `copy_tree_at`).

- [ ] **Step 3: Implement.** Replace the two `use crate::...` lines (`:6-7`) with:

```rust
use crate::copy::{CopyError, CopyStep, Guard, copy_file_guarded, split_destination, unguarded, weaker};
use crate::state::{FLUX_DIR, RESERVED_DIRS};
use crate::walk::{Walk, WalkEvent, walk};
```

  and add `FileSystem` to the `flux_fs::{...}` import list (`:8-11`). In `state.rs`, after `pub const CREATING_SUFFIX`
  (`:226`), add the constant this task's matcher reads:

```rust
/// The reserved subdirectories of `DEST/.flux` (the design's `.flux` ruling): Flux's own, never written by a copy.
pub const RESERVED_DIRS: [&str; 3] = [OPERATIONS_DIR, "standalone", "atomic"];
```

  In `impl TreeAbort` (`:52-58`), after `changed`, add:

```rust
    /// §55's exit-3 rule for an abort: a refusal code, after nothing changed and no streamed failure. The CLI's exit
    /// code and the run's rollback of a refused copy (cut 7a Part 3b, Q-I) decide by this one rule.
    pub fn refused_unchanged(&self) -> bool {
        !self.changed()
            && self.outcome.failures.is_empty()
            && matches!(self.error.code(), Code::SafetyRejected | Code::NoReplacePublishUnavailable)
    }
```

  Replace `fn run_tree` and everything after it up to (not including) `/// A `Dir` event under a live frame` (`:221-345`)
  with:

```rust
/// The source side of a tree copy, checked before anything at the destination is touched.
pub(crate) struct Source<'a, F: FileSystem> {
    pub(crate) events: Walk<'a, F>,
    pub(crate) identity: FileIdentity,
}

/// Steps 1-2 of `copy_tree`, which touch no destination: the source root and its identity, and the lexical floor. The
/// run (cut 7a Part 3b, B1) calls this before it takes the destination's lock, so a mistyped source creates nothing.
pub(crate) fn prepare_source<'a, F: FileSystem>(
    fs: &'a F,
    src_root: &'a Path,
    dst_root: &Path,
) -> std::result::Result<Source<'a, F>, CopyError> {
    // 1. The source root. `walk` refuses a missing or non-directory root: the whole
    //    operation failing, before any destination call.
    let events = walk(fs, src_root).map_err(|e| CopyError::at(CopyStep::Source, e))?;
    let identity = fs.metadata(src_root).map_err(|e| CopyError::at(CopyStep::Source, e))?.identity;

    // 2. The lexical floor, before any destination call (§129's own example, `/data`
    //    into `/data/backup`). Runs at every identity strength.
    if lexically_within(dst_root, src_root) {
        return Err(refuse("the destination is the source or lies inside it"));
    }
    Ok(Source { events, identity })
}

/// What every entry of one tree copy shares.
pub(crate) struct Shared<'c, F: DestinationRoot> {
    pub(crate) fs: &'c F,
    pub(crate) src_root: &'c Path,
    pub(crate) src_identity: FileIdentity,
    pub(crate) opts: &'c CopyOptions,
    /// §99's check before each destination mutation; `&unguarded` for a copy that holds no lock.
    pub(crate) guard: &'c Guard<'c>,
}

/// `copy_tree`'s body. Every `?` here is an abort; `copy_tree` pairs it with `out`,
/// which holds whatever was counted before it.
fn run_tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    let source = prepare_source(fs, src_root, dst_root)?;

    // 3-5. Resolve the destination, compare it with the source, create the root.
    let resolve = |e| CopyError::at(CopyStep::Resolve, e);
    let root = match fs.destination_root(dst_root) {
        Ok(root) => {
            preflight(
                source.identity,
                root.identity().map_err(resolve)?,
                opts.safety,
                &mut out.warnings,
            )?;
            root
        }
        Err(e) if e.source.kind() == ErrorKind::NotFound => {
            let (parent_path, name) = split_destination(dst_root)?;
            let parent = fs.destination_root(parent_path).map_err(resolve)?;
            preflight(
                source.identity,
                parent.identity().map_err(resolve)?,
                opts.safety,
                &mut out.warnings,
            )?;
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
    let cx = Shared { fs, src_root, src_identity: source.identity, opts, guard: &unguarded };
    copy_tree_at(&cx, source.events, root, out, on_report).1
}

/// Step 6 of `copy_tree`: the walk, writing through `root` and only below it, and `root` handed back with the result
/// for a caller that goes on writing through it (the run's finish).
///
/// `cx.guard` runs before every destination mutation (§99, cut 7a Part 3b): before each directory is created here, and
/// inside `copy_file_guarded` for each file. A failed guard aborts the whole copy with `TARGET_LOCK_BUSY`. A source
/// entry whose destination is a reserved control path (`reserved_path`) fails with `CONTROL_PLANE_NAMESPACE_CONFLICT`
/// and the rest continues.
pub(crate) fn copy_tree_at<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    events: Walk<'_, F>,
    root: F::Dir,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> (F::Dir, std::result::Result<(), CopyError>) {
    let root_identity = match root.identity() {
        Ok(i) => i,
        Err(e) => return (root, Err(CopyError::at(CopyStep::Resolve, e))),
    };
    let mut stack = vec![Frame::Live { dir: root, created: HashSet::new() }];
    let walked = walk_into(cx, events, root_identity, &mut stack, out, on_report);
    // The walk emits no `Dir` for the root, so no `DirEnd` pops it, and a frame is skipped only when pushed.
    match stack.into_iter().next() {
        Some(Frame::Live { dir, .. }) => (dir, walked),
        _ => unreachable!("the root frame is never popped, and never skipped"),
    }
}

/// `copy_tree_at`'s loop. Every `?` here is an abort.
fn walk_into<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    events: Walk<'_, F>,
    root_identity: FileIdentity,
    stack: &mut Vec<Frame<F::Dir>>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    // 6. The walk. NoReplace is mandatory in a tree (§241.5): every target is planned
    //    as new, and an existing one is refused at Step 2a (F3).
    let opts = CopyOptions { publish: Publish::NoReplace, ..cx.opts.clone() };
    let cx = Shared {
        fs: cx.fs,
        src_root: cx.src_root,
        src_identity: cx.src_identity,
        opts: &opts,
        guard: cx.guard,
    };
    for item in events {
        let live = matches!(stack.last(), Some(Frame::Live { .. }));
        let event = match item {
            Ok(event) => event,
            // Under a skipped subtree the walk still reads; its errors there belong to
            // a failure already reported once.
            Err(e) => {
                if live {
                    report(out, on_report, e.path, TreeFailureCause::Walk(e.cause));
                }
                continue;
            }
        };
        match event {
            WalkEvent::Dir { path, identity } => {
                let frame = if !live {
                    Frame::Skipped
                } else if reserved_path(&path) {
                    let conflict = TreeFailureCause::CreateDir(reserved_conflict());
                    report(out, on_report, path.clone(), conflict);
                    Frame::Skipped
                } else {
                    // §99 before the directory's creation.
                    (cx.guard)().map_err(|e| CopyError::at(CopyStep::Create, e))?;
                    enter_dir(stack, &path, identity, root_identity, cx.opts, out, on_report)?
                };
                // A destination directory about to be entered must not BE the source root
                // (owner, cut 4b capstone round 1): a bind mount inside the destination can
                // present the source under a destination name, and `open_dir` refuses only
                // name-surrogates. Aliases of source SUBdirectories stay the §42 mount cut's.
                if let Frame::Live { dir, .. } = &frame
                    && let (Ok(FileIdentity::Strong(a)), FileIdentity::Strong(b)) =
                        (dir.identity(), cx.src_identity)
                    && a == b
                {
                    return Err(refuse(
                        "a destination directory is the source root itself, by identity",
                    ));
                }
                stack.push(frame);
            }
            WalkEvent::File { path } => {
                out.files_total += 1;
                if let Some(Frame::Live { dir, .. }) = stack.last() {
                    if reserved_path(&path) {
                        let conflict = CopyError::at(CopyStep::Gate, reserved_conflict());
                        report(out, on_report, path, TreeFailureCause::Copy(conflict));
                    } else {
                        copy_one(&cx, dir, path, out, on_report)?;
                    }
                }
            }
            WalkEvent::Symlink { path } => {
                out.files_total += 1;
                if live {
                    report(out, on_report, path, TreeFailureCause::Symlink);
                }
            }
            WalkEvent::Other { path } => {
                out.files_total += 1;
                if live {
                    report(out, on_report, path, TreeFailureCause::SpecialFileSkipped);
                }
            }
            WalkEvent::DirEnd { .. } => {
                stack.pop();
            }
        }
    }
    Ok(())
}

/// P3-F: a path below the source root, so below DEST, that IS a reserved control directory - `.flux/operations`,
/// `.flux/standalone`, `.flux/atomic` - or lies below one. ASCII case-insensitive, because a case-folding destination
/// folds `.FLUX/Operations` onto the reserved name. Everything else under `.flux` is ordinary data (§259.3).
pub(crate) fn reserved_path(path: &Path) -> bool {
    let mut parts = path.components();
    let (Some(Component::Normal(first)), Some(Component::Normal(second))) = (parts.next(), parts.next())
    else {
        return false;
    };
    let same = |a: &std::ffi::OsStr, b: &str| a.as_encoded_bytes().eq_ignore_ascii_case(b.as_bytes());
    same(first, FLUX_DIR) && RESERVED_DIRS.iter().any(|r| same(second, r))
}

fn reserved_conflict() -> FsError {
    FsError::new(
        Code::ControlPlaneNamespaceConflict,
        std::io::Error::other(
            "its destination is a reserved Flux control path (DEST/.flux/operations, standalone or atomic); not copied",
        ),
    )
}
```

  Replace `fn copy_one`'s signature and its first match (`:409-418`, from `/// A `File` event under a live frame.`
  through `match copy_file_at(fs, &src_root.join(&path), parent, name, opts) {`) with:

```rust
/// A `File` event under a live frame.
fn copy_one<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    path: PathBuf,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    let name = path.file_name().expect("a walk path ends in a name");
    match copy_file_guarded(cx.fs, &cx.src_root.join(&path), parent, name, cx.opts, cx.guard) {
```

  and in its `Err(mut e)` arm, right after the leftover rebuild (`if let Some((p, _)) = e.leftover.as_mut() { ... }`),
  insert:

```rust
            // §99 (cut 7a Part 3b): this run no longer owns the destination's lock. The whole operation stops.
            if e.code() == Code::TargetLockBusy {
                return Err(e);
            }
```

  Make `preflight` crate-visible: `fn preflight(` (`:476`) becomes `pub(crate) fn preflight(`. `copy_file_at` is no
  longer imported by `tree.rs` (the import list above already drops it).

- [ ] **Step 4: Run the tests to see them pass, and the crate stay green**

Run: `cargo test -p flux-core --lib`
Expected: `test result: ok. 253 passed` (247 + 6), and `cargo test -p flux-core --test tree_std_fs` still passes.

- [ ] **Step 5: Prove each new test can fail.** One at a time, restoring after each:
  - `reserved_path` returning `same(first, FLUX_DIR) && same(second, OPERATIONS_DIR)` only: expect
    `a_reserved_control_path_is_matched...` and `a_source_entry_landing...` FAILED;
  - the `reserved_path(&path)` branch in the `File` arm removed (always `copy_one`): expect `a_source_entry_landing...`
    FAILED;
  - the guard call before `enter_dir` removed: expect `the_guard_runs_before_each_directory_is_created` FAILED;
  - the `TargetLockBusy` early return in `copy_one` removed: expect `a_failed_guard_aborts_the_whole_tree...` FAILED;
  - `refused_unchanged` without `self.outcome.failures.is_empty()`: expect `a_refused_unchanged_abort...` FAILED.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-core/src/tree.rs crates/flux-core/src/state.rs
git commit -m "feat(core): copy_tree_at writes through a given root under a guard; reserved .flux paths are refused (cut 7a Part 3b-1)"
```

---

### Task 4: State helpers - ids in names, retiring a workspace, removing a record, the control plane

**Files:**
- Modify: `crates/flux-core/src/state.rs` (constants after `:228`; `write_state` `:255-270`; `mod tests`)
- Modify: `crates/flux-core/src/prior.rs` (imports `:9-12`; after `resumable_refusal`; `mod tests`)

The tests are written below; implement until they pass.

- [ ] **Step 1: Add the failing tests** at the end of `mod tests` in `state.rs`:

```rust
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
```

  and at the end of `mod tests` in `prior.rs`:

```rust
    #[test]
    fn the_control_plane_check_refuses_a_foreign_object_at_any_reserved_name() {
        for reserved in ["operations", "standalone", "atomic"] {
            let (fs, d) = dest();
            fs.create_dir(Path::new("/p/dest/.flux")).unwrap();
            fs.write_file(format!("/p/dest/.flux/{reserved}"), b"x");
            let r = refusal(check_control_plane(&d, Path::new("D")));
            assert_eq!(r.code, LockCode::ControlPlaneNamespaceConflict, "{reserved}");
            assert!(r.detail.contains(reserved), "{}", r.detail);
        }
        let (fs, d) = dest();
        check_control_plane(&d, Path::new("D")).expect("no .flux at all");
        fs.create_dir(Path::new("/p/dest/.flux")).unwrap();
        fs.write_file("/p/dest/.flux/other", b"user data");
        check_control_plane(&d, Path::new("D")).expect("any other name under .flux is data");
        let (fs, d) = dest();
        fs.write_file("/p/dest/.flux", b"x");
        assert_eq!(
            refusal(check_control_plane(&d, Path::new("D"))).code,
            LockCode::ControlPlaneNamespaceConflict
        );
    }
```

- [ ] **Step 2: Run them to see them fail to compile**

Run: `cargo test -p flux-core --lib state::tests prior::tests`
Expected: `error[E0425]: cannot find function `id_after`` (and the others).

- [ ] **Step 3: Implement.** In `state.rs`, after `pub const RECORD_INFIX` (`:228`), add:

```rust
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
```

  In `write_state`, after `let temp = temp_name(name);`, insert:

```rust
    // Part 3b: a temporary left by an earlier failed write of this same state would make `create_new` fail forever.
    // Its name carries the operation's id, and the writer holds the destination's lock, so it is the writer's to
    // remove.
    match dir.remove_file(&temp) {
        Ok(()) => {}
        Err(e) if e.source.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
```

  After `operations_dir` (before `fn control_dir`), add:

```rust
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
            Err(e) if matches!(e.code, Code::DestinationError | Code::SafetyRejected) => return Ok(()),
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
```

  In `prior.rs`, add `RESERVED_DIRS` to the `use crate::state::{...}` list (`:9-12`) and, after `resumable_refusal`,
  add:

```rust
/// Step 2 of "The run": `DEST/.flux` and its reserved subdirectories, where they exist, are directories. Read-only: a
/// foreign object at any of them is `CONTROL_PLANE_NAMESPACE_CONFLICT` before anything is created.
pub fn check_control_plane<D: DirHandle>(dest: &D, dest_shown: &Path) -> LockResult<()> {
    let flux_shown = dest_shown.join(FLUX_DIR);
    let Some(flux) = existing_dir(dest, FLUX_DIR, &flux_shown)? else {
        return Ok(());
    };
    for name in RESERVED_DIRS {
        existing_dir(&flux, name, &flux_shown.join(name))?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to see them pass, and the crate stay green**

Run: `cargo test -p flux-core --lib`
Expected: `test result: ok. 260 passed` (253 + 7). The existing `state::tests` that read the call log still pass: the
new `remove_file` of the temporary comes before `create_new`.

- [ ] **Step 5: Prove each new test can fail** (restore after each):
  - `id_after` without the `(!head.is_empty())` check: `an_id_is_read_after_its_infix...` FAILED;
  - `write_state` without the stale-temporary removal: `a_stale_temporary...` FAILED;
  - `retire_workspace` removing the manifest BEFORE the rename (move the `rename_no_replace` line after the block,
    opening `id` instead of `retired`): `a_workspace_is_retired...` FAILED;
  - `remove_record` without the second `remove_if_present`: `a_record_is_removed_with_its_temporary...` FAILED;
  - `remove_empty_control_dirs` treating `DirectoryNotEmpty` as an error: `empty_control_directories...` FAILED;
  - `check_control_plane` with an empty loop: `the_control_plane_check...` FAILED.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-core/src/state.rs crates/flux-core/src/prior.rs
git commit -m "feat(core): retire a workspace before removing it, remove a record with its temporary, check the control plane (cut 7a Part 3b-1)"
```

---

### Task 5: A single file's records are found by the filesystem's own name equivalence (E3)

**Files:**
- Modify: `crates/flux-core/src/prior.rs` (`scan_file` `:87-139`; imports; `mod tests`)
- Modify: `crates/flux-core/tests/state_std_fs.rs`

The tests are written below; implement until they pass.

- [ ] **Step 1: Add the failing tests.** At the end of `mod tests` in `prior.rs`:

```rust
    #[test]
    fn a_record_is_looked_up_by_this_targets_own_name() {
        let (fs, d) = dest();
        // On a case-sensitive directory (the fake), `T`'s record is not `t`'s.
        fs.write_file(rec(OsStr::new("T"), 1), &state(1, Kind::File, OpState::Created).encode());
        assert_eq!(scan_file(&d, OsStr::new("t"), Path::new("P"), &id(0)).unwrap(), Scan::default());
        let calls: Vec<String> = fs.calls().iter().map(|c| c.replace('\\', "/")).collect();
        let wanted = format!("metadata(/p/dest/t.flux-state.{})", id(1));
        assert!(calls.contains(&wanted), "looked up under this target's name: {calls:?}");
    }
```

  At the end of `crates/flux-core/tests/state_std_fs.rs`:

```rust
#[test]
fn a_record_is_matched_the_way_the_filesystem_matches_names() {
    let tmp = tempfile::tempdir().unwrap();
    // The filesystem's own answer, measured here: does `probe` name `PROBE`?
    std::fs::write(tmp.path().join("PROBE"), b"").unwrap();
    let folds = tmp.path().join("probe").exists();
    std::fs::remove_file(tmp.path().join("PROBE")).unwrap();
    let id = flux_core::ids::new_id();
    let prior = OperationState::created(&id, Kind::File, tmp.path(), 1);
    std::fs::write(tmp.path().join(format!("T.flux-state.{id}")), prior.encode()).unwrap();
    let dir = StdFileSystem.destination_root(tmp.path()).unwrap();
    let own = flux_core::ids::new_id();
    let scan = flux_core::prior::scan_file(&dir, OsStr::new("t"), tmp.path(), &own).unwrap();
    assert_eq!(
        scan.resumable.len(),
        usize::from(folds),
        "the record is `t`'s exactly when this filesystem says `t` names `T` (folds = {folds})"
    );
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p flux-core --lib prior::tests::a_record_is_looked_up` and, on Windows (NTFS folds case),
`cargo test -p flux-core --test state_std_fs a_record_is_matched`.
Expected: the first FAILS on the missing `metadata(... t.flux-state ...)` call; the second FAILS on Windows with
`left: 0, right: 1` (on Linux it passes before and after: ext4 does not fold).

- [ ] **Step 3: Implement.** Add `id_after` and `record_name` to the `use crate::state::{...}` list in `prior.rs`
  (`record_name` is only in the test module's imports today; keep that import too or drop it, whichever compiles
  without an unused-import warning), and replace `scan_file` (`:87-139`, its doc comment through its closing brace)
  with:

```rust
/// A single file's prior operations: the records `<target>.flux-state.<id>` in `parent`. `parent_shown` is the
/// target's directory as messages name it.
///
/// E3 (Part 3b): every `*.flux-state.<id>` here is a candidate, and its record is looked up by THIS target's name, so
/// the filesystem's own name equivalence decides whether it is this target's - case on NTFS and APFS, Unicode
/// normalization on APFS - and `NotFound` means another target's. Exact bytes would miss `t.flux-state.<id>` for a
/// target spelled `T` on NTFS; folding in Flux would claim `T`'s record for `t` on a case-sensitive Linux directory,
/// where `T` is another file under another lock.
pub fn scan_file<D: DirHandle>(
    parent: &D,
    target: &OsStr,
    parent_shown: &Path,
    own_id: &str,
) -> LockResult<Scan> {
    let mut ids: Vec<String> = parent
        .read_dir()?
        .iter()
        .filter_map(|e| id_after(&e.name, RECORD_INFIX))
        .filter(|id| *id != own_id)
        .map(str::to_string)
        .collect();
    ids.sort();
    ids.dedup();
    let mut scan = Scan::default();
    for id in ids {
        let name = record_name(target, &id);
        let shown = parent_shown.join(&name);
        match parent.metadata(&name) {
            // Another target's record, or removed by its own finishing run since the listing.
            Err(e) if e.source.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            // A record's name beside a user's target is Flux's (F2), and no crash leaves anything but a regular file
            // there: anything else is unreadable state (§21.1), refused rather than passed over (decision 9; panel
            // round 3).
            Ok(m) if m.file_type != FileType::File => {
                return Err(corrupt(
                    &shown,
                    "a record's name holds something other than a regular file",
                ));
            }
            Ok(_) => {}
        }
        let state = match read_state(parent, &name) {
            Ok(decoded) => usable(decoded, &shown)?,
            // Removed by its own finishing run since the lookup: not a record now.
            Err(e) if e.source.kind() == ErrorKind::NotFound => continue,
            Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
                return Err(corrupt(
                    &shown,
                    &format!("the record is not a regular file: {}", e.source),
                ));
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
```

- [ ] **Step 4: Run the tests to see them pass, and the crate stay green**

Run: `cargo test -p flux-core --lib` and `cargo test -p flux-core --test state_std_fs`
Expected: `test result: ok. 261 passed`; `state_std_fs`: `3 passed`. The existing single-file scan tests still pass:
`u.flux-state.<id>` and `tt.flux-state.<id>` are looked up as `t.flux-state.<id>` and are absent; a directory at a
record's exact name is caught by the lookup's `metadata`.

- [ ] **Step 5: Prove the new tests can fail.** Temporarily restore exact matching (skip any id whose listed entry name
  is not exactly `record_name(target, id)`): `state_std_fs::a_record_is_matched...` FAILS on Windows (`just check`)
  and `a_record_is_looked_up_by_this_targets_own_name` FAILS (no `metadata(... t.flux-state ...)` call). Restore.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-core/src/prior.rs crates/flux-core/tests/state_std_fs.rs
git commit -m "feat(core): a single file's records are looked up by the target's own name (E3, cut 7a Part 3b-1)"
```

---

### Task 6: `Held::rewrite_record` (Q-K)

**Files:**
- Modify: `crates/flux-core/src/lock/held.rs` (after `write_record` `:41-47`; a new `mod tests` at the end)

The tests are written below; implement until they pass.

- [ ] **Step 1: Add the failing tests** as a new module at the end of `held.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::test_support::{fake, record};
    use crate::lock::{LockSite, Mode, Obtained, obtain};
    use flux_fs::LockCapability;
    use std::ffi::OsStr;

    const ID: &str = "11111111111111111111111111111111";

    #[test]
    fn a_record_is_rewritten_in_place_keeping_its_owner() {
        let (fs, d) = fake();
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        let Ok(Obtained::Held { mut held, .. }) =
            obtain(&site, LockCapability::LocalStrong, Mode::Plain, ID)
        else {
            panic!("a fresh lock is acquired");
        };
        let first = record(&site, ID, &format!("operations/{ID}"));
        held.write_record(first.clone()).unwrap();
        let none = LockRecord { workspace_path: "none".to_string(), ..first };
        held.rewrite_record(none.clone()).unwrap();
        let on_disk = decode(&fs.read_file("/p/dest.flux-lock").unwrap());
        assert_eq!(on_disk, Decoded::Record(none.clone()));
        assert_eq!(held.record(), Some(&none));
        assert!(held.still_owned().unwrap(), "the same file, the same owner");
        let calls = fs.calls();
        let count = |p: &str| calls.iter().filter(|c| c.starts_with(p)).count();
        assert_eq!(
            (count("write_at_start("), count("lock_sync_all(")),
            (2, 2),
            "each write is flushed: {calls:?}"
        );
    }

    #[test]
    #[should_panic(expected = "a rewrite keeps the owner")]
    fn a_rewrite_never_changes_the_owner() {
        let (_fs, d) = fake();
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        let Ok(Obtained::Held { mut held, .. }) =
            obtain(&site, LockCapability::LocalStrong, Mode::Plain, ID)
        else {
            panic!("a fresh lock is acquired");
        };
        let first = record(&site, ID, "none");
        held.write_record(first.clone()).unwrap();
        let _ = held.rewrite_record(LockRecord { owner_instance_id: "2".repeat(32), ..first });
    }
}
```

- [ ] **Step 2: Run them to see them fail to compile**

Run: `cargo test -p flux-core --lib lock::held::tests`
Expected: `error[E0599]: no method named `rewrite_record``.

- [ ] **Step 3: Implement.** After `write_record` in `held.rs`, add:

```rust
    /// Q-K (cut 7a Part 3b): rewrite this operation's record IN PLACE - one write through the handle holding the lock,
    /// flushed - keeping its `operation_id` and `owner_instance_id`, so `still_owned` still holds. The run rewrites
    /// `workspace_path` to `none` before it removes the state the record names: a crash after that removal must not
    /// leave a dead owner's record naming missing state (`ARTIFACT_OWNERSHIP_UNCERTAIN`, which no flag clears). A
    /// crash DURING this write leaves a torn record, `TARGET_LOCK_UNCERTAIN`, which `--restart --break-lock` clears.
    pub fn rewrite_record(&mut self, record: LockRecord) -> LockResult<()> {
        let mine = self.record.as_deref().expect("rewrite_record follows write_record");
        assert!(
            record.operation_id == mine.operation_id
                && record.owner_instance_id == mine.owner_instance_id,
            "a rewrite keeps the owner"
        );
        self.lock.write_at_start(&record.encode())?;
        self.lock.sync_all()?;
        self.record = Some(Box::new(record));
        Ok(())
    }
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p flux-core --lib lock::held::tests`
Expected: `test result: ok. 2 passed`. Then `cargo test -p flux-core --lib`: `263 passed`.

- [ ] **Step 5: Prove the tests can fail.** Remove `self.lock.sync_all()?;` from `rewrite_record`: the first test
  FAILS on the `(2, 2)` count. Restore. Remove the `assert!`: the second FAILS (no panic). Restore.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-core/src/lock/held.rs
git commit -m "feat(core): Held::rewrite_record rewrites the record in place, keeping its owner (Q-K, cut 7a Part 3b-1)"
```

---

### Task 7: The `run` module - a tree copy under the destination's lock

**Files:**
- Create: `crates/flux-core/src/run/mod.rs`, `crates/flux-core/src/run/session.rs`, `crates/flux-core/src/run/place.rs`,
  `crates/flux-core/src/run/tests.rs`
- Modify: `crates/flux-core/src/lib.rs` (after `pub mod prior;`)

This task is the run for a tree, without `--restart`'s supersede (Task 8) and without the single file (Task 9). The
tests are written below; implement until they pass.

- [ ] **Step 1: Write the failing tests.** Create `crates/flux-core/src/run/tests.rs`:

```rust
//! The run on the fake filesystem (cut 7a Part 3b-1). `/src` is the source; `/p` is DEST's parent and holds the lock.

use super::*;
use crate::fault_fs::FaultFs;
use crate::lock::LockCode;
use crate::lock::record::{Decoded, decode};
use crate::lock::test_support::{dead_lock, live_lock, record};
use crate::state::{Kind, OperationState, UNREADABLE, decode as decode_state};
use flux_fs::{
    DestinationRoot, Durability, FileSystem, LockCapability, OperationId, Preserve, Publish, Safety,
};
use std::ffi::OsStr;
use std::sync::{Arc, Mutex};

/// This run's operation id.
const ID: &str = "11111111111111111111111111111111";
const LOCK: &str = "/p/dest.flux-lock";

fn id(n: u8) -> String {
    format!("{n:032x}")
}

fn cfg() -> RunConfig {
    RunConfig {
        restart: false,
        break_lock: false,
        operation_id: ID.to_string(),
        owner_instance_id: id(0xee),
        boot_session_id: "test-boot".to_string(),
    }
}

fn restart() -> RunConfig {
    RunConfig { restart: true, ..cfg() }
}

fn opts() -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability: Durability::Normal,
        publish: Publish::Replace,
        safety: Safety::Default,
        operation_id: OperationId::new("replaced by the run"),
    }
}

/// `/src/a` (1 byte) and `/src/sub/b` (2 bytes); `/p` exists and `/p/dest` does not. Three `create_dir` calls.
fn fake() -> FaultFs {
    let fs = FaultFs::new();
    for d in ["/src", "/src/sub", "/p"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/src/a", b"A");
    fs.write_file("/src/sub/b", b"BB");
    fs
}

fn run_tree(fs: &FaultFs, c: &RunConfig) -> (Run<Result<TreeOutcome, TreeAbort>>, Vec<TreeFailure>) {
    let mut got = Vec::new();
    let r = tree(fs, Path::new("/src"), Path::new("/p/dest"), &opts(), c, &mut |f| got.push(f));
    (r, got)
}

/// The call log, with `/` separators on every platform.
fn calls(fs: &FaultFs) -> Vec<String> {
    fs.calls().iter().map(|c| c.replace('\\', "/")).collect()
}

/// The index of the first call starting with `prefix`.
fn at(calls: &[String], prefix: &str) -> usize {
    calls
        .iter()
        .position(|c| c.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix} in {calls:?}"))
}

fn refused(stop: &Option<RunError>) -> (LockCode, bool) {
    match stop {
        Some(RunError::Refused { refusal, changed, .. }) => (refusal.code, *changed),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn manifest(fs: &FaultFs, id: &str) -> OperationState {
    let bytes = fs
        .read_file(format!("/p/dest/.flux/operations/{id}/manifest"))
        .unwrap_or_else(|| panic!("no manifest for {id}"));
    decode_state(&bytes).unwrap()
}

/// A prior tree operation `n` in state `s`, with its workspace under `/p/dest`.
fn prior(fs: &FaultFs, n: u8, s: OpState) {
    for d in ["/p/dest", "/p/dest/.flux", "/p/dest/.flux/operations"] {
        if !fs.exists(d) {
            fs.create_dir(Path::new(d)).unwrap();
        }
    }
    fs.create_dir(Path::new(&format!("/p/dest/.flux/operations/{}", id(n)))).unwrap();
    let state = OperationState {
        state: s,
        ..OperationState::created(&id(n), Kind::Tree, Path::new("/p/dest"), 1)
    };
    fs.write_file(format!("/p/dest/.flux/operations/{}/manifest", id(n)), &state.encode());
}

fn ok(r: &Run<Result<TreeOutcome, TreeAbort>>) -> &TreeOutcome {
    assert!(r.stop.is_none(), "{:?}", r.stop);
    match &r.copy {
        Some(Ok(o)) => o,
        other => panic!("expected a finished copy, got {other:?}"),
    }
}

#[test]
fn a_tree_run_copies_and_leaves_no_state_behind() {
    let fs = fake();
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert!(got.is_empty() && r.warnings.is_empty(), "{got:?} {:?}", r.warnings);
    assert_eq!((out.files_copied, out.directories_created), (2, 2), "DEST and sub");
    assert_eq!(fs.read_file("/p/dest/sub/b").as_deref(), Some(&b"BB"[..]));
    assert!(!fs.exists("/p/dest/.flux") && !fs.exists(LOCK));
}

#[test]
fn the_lock_comes_first_then_the_state_then_the_record_naming_it() {
    let fs = fake();
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    let c = calls(&fs);
    let lock = at(&c, "create_lock(/p/dest.flux-lock)");
    let state = at(&c, &format!("create_dir(/p/dest/.flux/operations/{ID}.creating)"));
    let record = at(&c, "write_at_start(");
    let copy = at(&c, &format!("create_new(/p/dest/a.flux-partial.{ID})"));
    assert!(lock < state && state < record && record < copy, "F5: {c:?}");
}

#[test]
fn the_record_stops_naming_the_state_before_the_state_is_removed() {
    let fs = fake();
    let seen: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
    let keep = Arc::clone(&seen);
    // Renames without replacing: the workspace into place (1), `a` and `sub/b` published (2, 3), the retire (4).
    fs.on_nth("rename_no_replace", 4, move |fs| *keep.lock().unwrap() = fs.read_file(LOCK));
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    let c = calls(&fs);
    let renames: Vec<&String> = c.iter().filter(|x| x.starts_with("rename_no_replace(")).collect();
    assert!(renames[3].contains(&format!("{ID}.removing")), "the 4th is the retire: {renames:?}");
    let bytes = seen.lock().unwrap().clone().expect("the lock exists at the retire");
    let Decoded::Record(rec) = decode(&bytes) else { panic!("a whole record") };
    assert_eq!((rec.operation_id.as_str(), rec.workspace_path.as_str()), (ID, "none"));
}

#[test]
fn an_unsupported_filesystem_is_refused_before_anything_is_created() {
    let fs = fake();
    fs.set_lock_capability(LockCapability::Unsupported);
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::RemoteLockUnsafe, false));
    assert!(r.copy.is_none());
    assert!(!fs.called("create_lock") && !fs.exists("/p/dest"));
}

#[test]
fn a_foreign_object_at_a_reserved_control_path_is_refused_before_the_lock() {
    let fs = fake();
    for d in ["/p/dest", "/p/dest/.flux"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/p/dest/.flux/atomic", b"not Flux's");
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::ControlPlaneNamespaceConflict, false));
    assert!(!fs.called("create_lock"));
}

#[test]
fn a_busy_lock_is_refused_and_left_alone() {
    let fs = fake();
    let p = fs.destination_root(Path::new("/p")).unwrap();
    let site = LockSite::directory(&p, OsStr::new("dest")).unwrap();
    let theirs = record(&site, &id(5), "none").encode();
    let _holder = live_lock(&p, "dest.flux-lock", &theirs);
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::TargetLockBusy, false));
    assert_eq!(fs.read_file(LOCK), Some(theirs));
    assert!(!fs.exists("/p/dest"));
}

#[test]
fn an_empty_lock_without_break_lock_is_uncertain_and_left_alone() {
    let fs = fake();
    let p = fs.destination_root(Path::new("/p")).unwrap();
    dead_lock(&p, "dest.flux-lock", b"");
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::TargetLockUncertain, false));
    assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b""[..]));
}

#[test]
fn a_resumable_prior_operation_is_refused_and_the_lock_removed_again() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::ResumableOperationExists, false));
    assert!(!fs.exists(LOCK), "the lock this run created is gone");
    assert_eq!(manifest(&fs, &id(5)).state, OpState::Failed, "untouched");
    assert!(!fs.exists(format!("/p/dest/.flux/operations/{ID}")));
}

#[test]
fn ownership_lost_mid_copy_stops_and_leaves_the_state_and_the_lock() {
    let fs = fake();
    // `create_new`: this run's CREATED (1) and TRANSFERRING (2) manifests, then `a`'s temporary (3).
    fs.on_nth("create_new", 3, |fs| fs.write_file(LOCK, b"another run's bytes"));
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "the copy's abort is the report: {:?}", r.stop);
    let Some(Err(a)) = &r.copy else { panic!("the copy aborted: {:?}", r.copy) };
    assert_eq!(a.error.code(), Code::TargetLockBusy);
    assert_eq!(manifest(&fs, ID).state, OpState::Transferring, "the state stays as it is");
    assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b"another run's bytes"[..]), "never unlinked");
    assert!(!fs.exists("/p/dest/a"));
}

#[test]
fn a_copy_refusal_that_changed_nothing_is_rolled_back_so_it_stays_exit_3() {
    let fs = FaultFs::new();
    for d in ["/src", "/src/sub", "/p", "/p/dest"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/src/sub/b", b"BB");
    // `sub` IS DEST by identity: the copy refuses it before creating anything (cut 4b).
    fs.set_identity("/src/sub", fs.metadata(Path::new("/p/dest")).unwrap().identity);
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    let Some(Err(a)) = &r.copy else { panic!("the copy refused: {:?}", r.copy) };
    assert!(a.refused_unchanged(), "{a:?}");
    assert!(!fs.exists("/p/dest/.flux") && !fs.exists(LOCK), "what the run made is gone");
    assert!(fs.exists("/p/dest"), "DEST was the operator's");
}

#[test]
fn a_rollback_removes_a_dest_the_run_made() {
    let fs = FaultFs::new();
    for d in ["/src", "/src/sub", "/p"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/src/sub/b", b"BB");
    // `create_dir`: the setup's three, then DEST (4) and `.flux` (5). Once DEST exists, `sub` becomes it by identity.
    fs.on_nth("create_dir", 5, |fs| {
        let dest = fs.metadata(Path::new("/p/dest")).unwrap().identity;
        fs.set_identity("/src/sub", dest);
    });
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    let Some(Err(a)) = &r.copy else { panic!("the copy refused: {:?}", r.copy) };
    assert!(a.refused_unchanged() && a.outcome.directories_created == 0, "{a:?}");
    assert!(!fs.exists("/p/dest") && !fs.exists(LOCK));
}

#[test]
fn a_temporary_the_copy_could_not_remove_keeps_the_completed_state() {
    let fs = fake();
    // `a`'s copy fails while streaming (its temporary is the 3rd `create_new`), and removing that temporary fails
    // too: the 4th `remove_file` (the two state writes clear their temporaries, then `a`'s step-1 sweep).
    fs.on_nth("create_new", 3, |fs| fs.fail_write(std::io::Error::other("injected write")));
    fs.fail_nth("remove_file", 4, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(out.failures.copy, 1, "{got:?}");
    assert!(r.warnings.iter().any(|w| matches!(w, RunWarning::StateKept(_))), "{:?}", r.warnings);
    assert_eq!(manifest(&fs, ID).state, OpState::Completed);
    assert!(fs.exists(format!("/p/dest/a.flux-partial.{ID}")));
    assert!(!fs.exists(LOCK), "the lock is still released");
}

#[test]
fn break_lock_takes_an_empty_lock_over_and_records_the_takeover_in_the_state() {
    let fs = fake();
    let p = fs.destination_root(Path::new("/p")).unwrap();
    dead_lock(&p, "dest.flux-lock", b"");
    let seen: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
    let keep = Arc::clone(&seen);
    // `create_new`: the CREATED manifest (1), the takeover (2), TRANSFERRING (3), then `a`'s temporary (4).
    let path = format!("/p/dest/.flux/operations/{ID}/manifest");
    fs.on_nth("create_new", 4, move |fs| *keep.lock().unwrap() = fs.read_file(&path));
    let (r, _) = run_tree(&fs, &RunConfig { break_lock: true, ..restart() });
    ok(&r);
    let state = decode_state(&seen.lock().unwrap().clone().expect("the manifest exists")).unwrap();
    assert_eq!(state.state, OpState::Transferring);
    let t = state.takeover.expect("the takeover is recorded");
    assert_eq!((t.operation_id.as_str(), t.owner_instance_id.as_str()), (UNREADABLE, UNREADABLE));
    assert!(!fs.exists(LOCK) && !fs.exists("/p/dest/.flux"));
}

#[test]
fn a_takeover_whose_lock_moves_starts_again_and_reuses_its_state() {
    let fs = fake();
    let p = fs.destination_root(Path::new("/p")).unwrap();
    dead_lock(&p, "dest.flux-lock", b"");
    // The takeover's overwrite is written, and before its flush the claimed file is moved away (`lock_sync_all` 1).
    fs.on_nth("lock_sync_all", 1, |fs| {
        fs.rename_replace(Path::new(LOCK), Path::new("/p/moved")).unwrap();
    });
    let (r, _) = run_tree(&fs, &RunConfig { break_lock: true, ..restart() });
    ok(&r);
    let c = calls(&fs);
    let creating = format!("create_dir(/p/dest/.flux/operations/{ID}.creating)");
    assert_eq!(c.iter().filter(|x| x.starts_with(&creating)).count(), 1, "made once, reused: {c:?}");
    assert!(fs.exists("/p/moved") && !fs.exists(LOCK));
}

#[test]
fn a_refusal_whose_lock_cannot_be_removed_names_it_and_is_exit_1() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    // The refusal's removal of the lock this run created is the run's first `remove_file`.
    fs.fail_nth("remove_file", 1, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let Some(RunError::Refused { refusal, changed, not_removed }) = &r.stop else {
        panic!("expected a refusal, got {:?}", r.stop)
    };
    assert_eq!((refusal.code, *changed), (LockCode::ResumableOperationExists, true));
    let (path, _) = not_removed.as_ref().expect("the lock it could not remove is named");
    assert_eq!(path.to_string_lossy().replace('\\', "/"), LOCK);
}

#[test]
fn ownership_lost_after_completed_is_a_warning_and_the_lock_is_left() {
    let fs = fake();
    // The retire is the 4th rename without replacing (the workspace, then the two publishes); just before it, the
    // lock is taken over. The transfer is complete and durable by then.
    fs.on_nth("rename_no_replace", 4, |fs| fs.write_file(LOCK, b"another run's bytes"));
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    assert!(
        r.warnings.iter().any(|w| matches!(w, RunWarning::OwnershipLostAfterCompletion(_))),
        "{:?}",
        r.warnings
    );
    assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b"another run's bytes"[..]), "never unlinked");
}

#[test]
fn a_dead_owners_lock_left_beside_the_new_one_is_a_warning() {
    let fs = fake();
    prior(&fs, 5, OpState::Completed);
    let p = fs.destination_root(Path::new("/p")).unwrap();
    let site = LockSite::directory(&p, OsStr::new("dest")).unwrap();
    let dead = record(&site, &id(5), &format!("operations/{}", id(5))).encode();
    dead_lock(&p, "dest.flux-lock", &dead);
    // §240.3 step 5 deletes the moved-aside lock: the run's first `remove_file`. It fails.
    fs.fail_nth("remove_file", 1, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    let broken = r.warnings.iter().any(|w| {
        matches!(w, RunWarning::BrokenLeftover(p) if p.to_string_lossy().contains(".broken."))
    });
    assert!(broken, "{:?}", r.warnings);
}
```

  Add to `crates/flux-core/src/lib.rs`, after `pub mod prior;`:

```rust
pub mod run;
```

- [ ] **Step 2: Run them to see them fail to compile**

Run: `cargo test -p flux-core --lib run::`
Expected: `error[E0583]: file not found for module `run``.

- [ ] **Step 3: Implement.** Create `crates/flux-core/src/run/mod.rs`:

```rust
//! The run (cut 7a design, "The run"; Part 3b-1): a copy that holds its destination's target lock from before its
//! state exists until after that state is gone (F5, Q-K), records a versioned state that a later run classifies
//! (§21.1), and with `--restart` supersedes the prior operations it finds. `tree` is the directory copy and `file` the
//! single-file copy.
//!
//! The CLI (Part 3b-2) renders a `Run`: `copy` exactly as it renders a `copy_tree` / `copy_file` result today, then
//! `stop`, then `warnings`. Its exit status: a `stop` of `RunError::Refused { changed: false, .. }` is 3; any other
//! `stop` is 1; with no `stop`, the copy's own rule decides (`exit_code::for_tree` / `for_file`); `warnings` never
//! change it.

mod place;
mod session;
#[cfg(test)]
mod tests;

use crate::lock::record::LockRecord;
use crate::lock::{LockSite, Refusal, Released, check_capability};
use crate::prior::check_control_plane;
use crate::state::OpState;
use crate::tree::{
    Shared, TreeAbort, TreeFailure, TreeFailureCause, TreeOutcome, copy_tree_at, prepare_source,
};
use flux_fs::{Code, CopyOptions, DestinationRoot, DirHandle, FsError, OperationId};
use place::{Place, TreePlace, locate_tree};
use session::{Locked, from_lock, guarded, lock_io, open_operation, owned};
use std::path::{Path, PathBuf};

/// The `workspace_path` a record carries once the state it named is about to go (Q-K): the spec's literal for "names
/// no workspace", which a dead owner's classification treats as recoverable (`lock/site.rs`, `workspace`).
const NO_WORKSPACE: &str = "none";

/// What the operator asked of the run, beyond the copy itself.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// `--restart`: supersede every resumable prior operation.
    pub restart: bool,
    /// `--break-lock`: take an uncertain lock over (§240.5). The CLI allows it only with `--restart` (exit 2).
    pub break_lock: bool,
    /// This operation's id (`ids::new_id`): it names the state, the record and every temporary.
    pub operation_id: String,
    /// This process's id, in the same form.
    pub owner_instance_id: String,
    /// `flux_platform::boot_session_id()`, or `unknown`.
    pub boot_session_id: String,
}

/// What a run did. `copy` is `None` when the run stopped before the copy; otherwise it is what `copy_tree` /
/// `copy_file` would return, and a refusal of the source side (B1) arrives there too. `stop` is why the run itself
/// stopped or failed outside the copy. `warnings` are reported and never fail the run (F6).
#[derive(Debug)]
pub struct Run<T> {
    pub copy: Option<T>,
    pub stop: Option<RunError>,
    pub warnings: Vec<RunWarning>,
}

/// Why the run stopped outside the copy.
#[derive(Debug)]
pub enum RunError {
    /// A refusal ("Errors and exit statuses"). `changed` is false when nothing changed, or what the run created was
    /// removed again (exit 3), and true otherwise (exit 1): ownership lost mid-run, or a refusal after this run's
    /// state exists. `not_removed` is something this run created and could not remove while refusing
    /// (spec:2896-2902).
    Refused { refusal: Box<Refusal>, changed: bool, not_removed: Option<(PathBuf, FsError)> },
    /// An I/O failure in one of the run's own steps, at `path` (exit 1).
    Failed { step: RunStep, path: PathBuf, error: FsError },
}

/// The run's own steps, for a failure's message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStep {
    /// Reading the destination's control plane or its prior state (steps 2 and 4).
    Inspect,
    /// Acquiring, classifying, recovering or taking over the lock (step 3).
    Lock,
    /// Creating or rewriting this operation's state (step 5 and its later transitions).
    State,
    /// Writing this operation's lock record (step 5; `--break-lock` step 6).
    Record,
    /// Superseding a prior operation (`--restart`).
    Restart,
    /// Removing what a refused copy left (Q-I).
    Rollback,
}

impl RunStep {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inspect => "inspecting the destination's state",
            Self::Lock => "obtaining the destination's lock",
            Self::State => "writing the operation's state",
            Self::Record => "writing the lock record",
            Self::Restart => "superseding a prior operation",
            Self::Rollback => "removing what the refused copy left",
        }
    }
}

/// Reported, never a failure (F6): the exit status stays what the copy decided.
#[derive(Debug)]
pub enum RunWarning {
    /// §240.3 recovery left the dead owner's lock here, moved aside (the crash table's `.broken` row).
    BrokenLeftover(PathBuf),
    /// Part of this run's own state, an empty control directory, or the lock could not be removed (F6).
    NotRemoved { path: PathBuf, error: FsError },
    /// A temporary the copy could not remove keeps this COMPLETED state as its record (G2).
    StateKept(PathBuf),
    /// A superseded operation's partial at `path` could not be deleted; the ABANDONED state at `kept` stays as its
    /// record (J1).
    PartialKept { path: PathBuf, error: FsError, kept: PathBuf },
    /// Ownership was lost after COMPLETED (finish step 5): the lock was closed without unlinking.
    OwnershipLostAfterCompletion(PathBuf),
}

/// A directory copy under the destination's lock ("The run").
pub fn tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    cfg: &RunConfig,
    on_report: &mut dyn FnMut(TreeFailure),
) -> Run<Result<TreeOutcome, TreeAbort>> {
    let mut run = Run { copy: None, stop: None, warnings: Vec::new() };
    let opts = CopyOptions { operation_id: OperationId::new(cfg.operation_id.as_str()), ..opts.clone() };
    let mut out = TreeOutcome::default();
    // B1: the source side, then DEST and the identity pre-flight, before anything is created.
    let source = match prepare_source(fs, src_root, dst_root) {
        Ok(s) => s,
        Err(error) => {
            run.copy = Some(Err(TreeAbort { error, outcome: out }));
            return run;
        }
    };
    let located = match locate_tree(fs, dst_root, source.identity, opts.safety, &mut out.warnings) {
        Ok(l) => l,
        Err(error) => {
            run.copy = Some(Err(TreeAbort { error, outcome: out }));
            return run;
        }
    };
    let holder = located.holder;
    // Step 1: the lock's filesystem (decision 14).
    let capability = match check_capability(&holder) {
        Ok(c) => c,
        Err(e) => {
            run.stop = Some(from_lock(e, RunStep::Lock, &located.holder_shown, false));
            return run;
        }
    };
    // Step 2: DEST's control plane, where DEST exists.
    if let Some(dest) = &located.dest
        && let Err(e) = check_control_plane(dest, dst_root)
    {
        run.stop = Some(from_lock(e, RunStep::Inspect, dst_root, false));
        return run;
    }
    let site = match &located.name {
        Some(name) => LockSite::directory(&holder, name),
        None => Ok(LockSite::root(&holder)),
    };
    let site = match site {
        Ok(s) => s,
        Err(e) => {
            run.stop = Some(from_lock(e, RunStep::Lock, &located.holder_shown, false));
            return run;
        }
    };
    let mut place = TreePlace {
        fs,
        holder: &holder,
        holder_shown: located.holder_shown,
        name: located.name,
        dest: located.dest,
        dest_shown: dst_root.to_path_buf(),
        created_dest: false,
        operations: None,
    };
    // Steps 3-5 (and `--restart`): the lock, then this operation's state and the record naming it.
    let locked = match open_operation(&site, capability, &mut place, cfg, &mut run.warnings) {
        Ok(l) => l,
        Err(e) => {
            run.stop = Some(e);
            return run;
        }
    };
    // Step 6: the copy, with §99 before every destination mutation.
    let mut leftovers = false;
    let walked = {
        let guard = || guarded(&locked.held);
        let cx = Shared { fs, src_root, src_identity: source.identity, opts: &opts, guard: &guard };
        let root = place.dest.take().expect("step 5 made DEST");
        let mut report = |f: TreeFailure| {
            if matches!(&f.cause, TreeFailureCause::Copy(e) if e.leftover.is_some()) {
                leftovers = true;
            }
            on_report(f);
        };
        let (root, walked) = copy_tree_at(&cx, source.events, root, &mut out, &mut report);
        place.dest = Some(root);
        walked
    };
    let mut result = match walked {
        Ok(()) => Ok(out),
        Err(error) => Err(TreeAbort { error, outcome: out }),
    };
    let ended = match &result {
        Ok(_) => Ended::Completed { leftovers },
        Err(a) if a.error.code() == Code::TargetLockBusy => Ended::Lost,
        Err(a) if a.refused_unchanged() => Ended::RefusedUnchanged,
        Err(_) => Ended::Failed,
    };
    run.stop = finish(&mut place, locked, ended, &mut run.warnings);
    // E1: a DEST this run made counts among the directories it created, unless a rollback removed it again.
    if place.created_dest {
        match &mut result {
            Ok(o) => o.directories_created += 1,
            Err(a) => a.outcome.directories_created += 1,
        }
    }
    run.copy = Some(result);
    run
}

/// How the copy ended, as the finish needs it.
enum Ended {
    /// The walk finished, or the single file was copied (A1). `leftovers`: a temporary the copy could not remove.
    Completed { leftovers: bool },
    /// The copy stopped because this run no longer owns the lock.
    Lost,
    /// The copy refused with nothing changed (Q-I).
    RefusedUnchanged,
    /// Any other failure of the copy.
    Failed,
}

/// Step 7, "Finish".
fn finish<D: DirHandle, P: Place<D>>(
    place: &mut P,
    locked: Locked<'_, D>,
    ended: Ended,
    warnings: &mut Vec<RunWarning>,
) -> Option<RunError> {
    match ended {
        Ended::Completed { leftovers } => complete(place, locked, leftovers, warnings),
        // Ownership lost before COMPLETED (decision 13): the state stays as it is, and `locked` drops here, closing
        // the lock without unlinking it (`S99_refuse_close`). The copy's own TARGET_LOCK_BUSY is the report.
        Ended::Lost => None,
        Ended::RefusedUnchanged => rollback(place, locked),
        Ended::Failed => fail(place, locked),
    }
}

/// The success path: COMPLETED, durably and while still owned; then, unless a leftover temporary keeps it (G2), the
/// record stops naming the state (Q-K), the state goes, and the empty control directories; then the release. After
/// COMPLETED nothing may fail the run (F6, spec:9363): each failure from here is a warning.
fn complete<D: DirHandle, P: Place<D>>(
    place: &mut P,
    mut locked: Locked<'_, D>,
    leftovers: bool,
    warnings: &mut Vec<RunWarning>,
) -> Option<RunError> {
    if let Err(fault) = owned(&locked.held, &locked.lock_shown) {
        return Some(fault.into_error(RunStep::State));
    }
    locked.state.state = OpState::Completed;
    if let Err(error) = place.write(&locked.state) {
        let path = place.shown(&locked.state.operation_id);
        return Some(stop_after_record(place, locked, RunStep::State, path, error));
    }
    if leftovers {
        warnings.push(RunWarning::StateKept(place.shown(&locked.state.operation_id)));
    } else if let Err((path, error)) = remove_own(place, &mut locked, false) {
        warnings.push(RunWarning::NotRemoved { path, error });
    }
    // Steps 5-6: `S99_release_check`, then `S99_release` - or `S99_refuse_close`.
    match locked.held.release() {
        Ok(Released::Unlinked) => {}
        Ok(Released::NotOwned) => {
            warnings.push(RunWarning::OwnershipLostAfterCompletion(locked.lock_shown));
        }
        Err(e) => warnings.push(RunWarning::NotRemoved { path: locked.lock_shown, error: lock_io(e) }),
    }
    None
}

/// Q-K, then finish steps 3-4, or a rollback's removals: the record stops naming this operation's state before the
/// state goes, so a crash part-way never leaves a dead owner's record naming missing state.
fn remove_own<D: DirHandle, P: Place<D>>(
    place: &mut P,
    locked: &mut Locked<'_, D>,
    rollback: bool,
) -> Result<(), (PathBuf, FsError)> {
    let record = locked.held.record().expect("written at step 5").clone();
    let none = LockRecord { workspace_path: NO_WORKSPACE.to_string(), ..record };
    if let Err(e) = locked.held.rewrite_record(none) {
        return Err((locked.lock_shown.clone(), lock_io(e)));
    }
    place.remove(&locked.state.operation_id)?;
    place.remove_control_dirs(rollback)
}

/// The failure path: FAILED (resumable: the next run gets RESUMABLE_OPERATION_EXISTS until `--restart`), then the
/// release "the same way". The copy's own error is the report; a failure here is reported beside it.
fn fail<D: DirHandle, P: Place<D>>(place: &P, mut locked: Locked<'_, D>) -> Option<RunError> {
    if let Err(fault) = owned(&locked.held, &locked.lock_shown) {
        return Some(fault.into_error(RunStep::State));
    }
    locked.state.state = OpState::Failed;
    let stop = place.write(&locked.state).err().map(|error| RunError::Failed {
        step: RunStep::State,
        path: place.shown(&locked.state.operation_id),
        error,
    });
    match locked.held.release() {
        Ok(_) => stop,
        Err(e) => stop.or(Some(RunError::Failed {
            step: RunStep::State,
            path: locked.lock_shown,
            error: lock_io(e),
        })),
    }
}

/// A failure of the run's own step after the record exists (decision 12): FAILED and the release, best effort. The
/// first failure is the report; a second one here leaves what a crash at this point leaves, which the next run
/// classifies.
fn stop_after_record<D: DirHandle, P: Place<D>>(
    place: &P,
    locked: Locked<'_, D>,
    step: RunStep,
    path: PathBuf,
    error: FsError,
) -> RunError {
    let _second = fail(place, locked);
    RunError::Failed { step, path, error }
}

/// Q-I: the copy refused with nothing changed, after this run's state exists. What the run created is removed again -
/// Q-K first, then the state, the control directories it emptied and a DEST it made - and the lock released, so the
/// copy's refusal keeps exit 3 (§55: created and removed again does not count). A failure here is exit 1, naming the
/// path; the lock is still released, and whatever state remains is what a crash at that point leaves.
fn rollback<D: DirHandle, P: Place<D>>(place: &mut P, mut locked: Locked<'_, D>) -> Option<RunError> {
    if let Err(fault) = owned(&locked.held, &locked.lock_shown) {
        return Some(fault.into_error(RunStep::Rollback));
    }
    if let Err((path, error)) = remove_own(place, &mut locked, true) {
        // `release` unlinks only a lock that is still this run's; its own failure leaves a dead owner's lock.
        let _released = locked.held.release();
        return Some(RunError::Failed { step: RunStep::Rollback, path, error });
    }
    match locked.held.release() {
        Ok(Released::Unlinked) => None,
        Ok(Released::NotOwned) => Some(session::lost()),
        Err(e) => Some(RunError::Failed {
            step: RunStep::Rollback,
            path: locked.lock_shown,
            error: lock_io(e),
        }),
    }
}
```

  Create `crates/flux-core/src/run/session.rs`:

```rust
//! Steps 3-5 of "The run", and what the rest of the run shares with them: the §99 checks, and the lock protocol's
//! errors as the run's.

use super::place::Place;
use super::{RunConfig, RunError, RunStep, RunWarning, stop_after_record};
use crate::lock::record::LockRecord;
use crate::lock::{
    Held, LockCode, LockError, LockResult, LockSite, MAX_ATTEMPTS, Mode, Obtained, Overwritten, Refusal,
    obtain,
};
use crate::prior::resumable_refusal;
use crate::state::{OpState, OperationState, Takeover, wall_time_ns};
use flux_fs::{Code, DirHandle, FsError, LockCapability};
use std::path::{Path, PathBuf};

/// The lock this run holds, its record written, and this operation's state as last written.
pub(crate) struct Locked<'a, D: DirHandle> {
    pub(crate) held: Held<'a, D>,
    pub(crate) state: OperationState,
    /// The lock's path, as messages name it.
    pub(crate) lock_shown: PathBuf,
}

/// Steps 3-5 of "The run", then `--restart`'s supersede, then TRANSFERRING: returns the lock with this operation's
/// record written, naming its state.
///
/// - Step 3 obtains the lock: acquired, recovered from a dead owner, or claimed from an uncertain one (`--break-lock`).
/// - Step 4's refusals give back what step 3 created (decision 12).
/// - Step 5 (F5): the state first, then the record naming it. Under `--restart` this record write is the model's
///   `S21_1_s5_write_begin` / `S21_1_s5_write_end`: 7a writes the record once, after the state, and before the
///   revalidation (Q-H (a); the model's `Recover` and `TakeOver` have written it before `S21_1_s3` too). A
///   `--break-lock` claim overwrites the prior record in place (§240.5 step 6) and only then records the takeover in
///   the state (spec:10700-10701).
/// - D1: a claim whose lock path moved (`Overwritten::Restart`) starts again, reusing the state it already made.
/// - When a §99 check fails after the record exists, the lock closes without unlinking as `locked` drops:
///   `S21_1_s3_refuse_close` under `--restart`, `S99_refuse_close` otherwise.
pub(crate) fn open_operation<'a, D: DirHandle, P: Place<D>>(
    site: &LockSite<'a, D>,
    capability: LockCapability,
    place: &mut P,
    cfg: &RunConfig,
    warnings: &mut Vec<RunWarning>,
) -> Result<Locked<'a, D>, RunError> {
    let id = cfg.operation_id.as_str();
    let lock_shown = place.holder_shown().join(site.lock_name());
    let mode = if cfg.break_lock { Mode::BreakLock } else { Mode::Plain };
    // This run's state, once it exists (D1).
    let mut made: Option<OperationState> = None;
    for _ in 0..MAX_ATTEMPTS {
        // Step 3.
        let obtained = obtain(site, capability, mode, id)
            .map_err(|e| from_lock(e, RunStep::Lock, &lock_shown, made.is_some()))?;
        // Step 4. (Task 8 binds the resumable operations for `--restart`.)
        match place.scan(id) {
            Ok(scan) if scan.resumable.is_empty() || cfg.restart => {}
            Ok(scan) => {
                let refused = resumable_refusal(&scan.resumable[0]);
                let error = from_lock(refused, RunStep::Inspect, place.destination(), made.is_some());
                return Err(give_back(obtained, error, &lock_shown));
            }
            Err(e) => {
                let error = from_lock(e, RunStep::Inspect, place.destination(), made.is_some());
                return Err(give_back(obtained, error, &lock_shown));
            }
        }
        // Step 5 (F5): the state first.
        if made.is_none() {
            let state = OperationState::created(id, place.kind(), place.destination(), wall_time_ns());
            if let Err(e) = place.create(&state) {
                return Err(give_back(obtained, e, &lock_shown));
            }
            made = Some(state);
        }
        let state = made.clone().expect("made above");
        let record = match record_for(site, cfg, place.workspace_path(id)) {
            Ok(r) => r,
            Err(e) => {
                let error = from_lock(e, RunStep::Record, &lock_shown, true);
                return Err(give_back(obtained, error, &lock_shown));
            }
        };
        // Then the record naming it.
        let mut locked = match obtained {
            Obtained::Held { mut held, leftover_broken } => {
                if let Some(name) = leftover_broken {
                    warnings.push(RunWarning::BrokenLeftover(place.holder_shown().join(name)));
                }
                if let Err(e) = held.write_record(record) {
                    // No record was kept (`write_record` sets it only on success): the lock is still this run's to
                    // remove (decision 12), and its state stays for a later `--restart`.
                    let error = from_lock(e, RunStep::Record, &lock_shown, true);
                    let unwritten = Obtained::Held { held, leftover_broken: None };
                    return Err(give_back(unwritten, error, &lock_shown));
                }
                Locked { held, state, lock_shown: lock_shown.clone() }
            }
            Obtained::Claimed(claimed) => match claimed.overwrite(record) {
                Ok(Overwritten::Held(held)) => {
                    let mut locked = Locked { held, state, lock_shown: lock_shown.clone() };
                    // §240.5 step 6, after the flush succeeded: the takeover, in this operation's state.
                    locked.state.takeover = Some(Takeover::of_unreadable(wall_time_ns()));
                    if let Err(error) = place.write(&locked.state) {
                        let path = place.shown(id);
                        return Err(stop_after_record(place, locked, RunStep::State, path, error));
                    }
                    locked
                }
                // D1: the claimed file is no longer at the lock path. Start again; the state stays this run's.
                Ok(Overwritten::Restart) => continue,
                Err(e) => return Err(from_lock(e, RunStep::Record, &lock_shown, true)),
            },
        };
        // The last write of step 5: TRANSFERRING, under §99.
        if let Err(fault) = owned(&locked.held, &locked.lock_shown) {
            return Err(fault.into_error(RunStep::State));
        }
        locked.state.state = OpState::Transferring;
        if let Err(error) = place.write(&locked.state) {
            let path = place.shown(id);
            return Err(stop_after_record(place, locked, RunStep::State, path, error));
        }
        return Ok(locked);
    }
    Err(RunError::Refused {
        refusal: Box::new(Refusal {
            code: LockCode::TargetLockBusy,
            holder: None,
            detail: format!(
                "gave up after {MAX_ATTEMPTS} attempts: the lock this run took over kept moving; this operation's state stays for a later --restart"
            ),
        }),
        changed: made.is_some(),
        not_removed: None,
    })
}

/// A refusal or failure before this run's record exists: remove the lock this run created (Part 2 decision 7:
/// unlinked while still held), or let go of a claimed one it did not create. If that removal fails, the refusal names
/// the path and becomes exit 1 (spec:2896-2902). This run's state, if it already exists (D1), stays.
fn give_back<D: DirHandle>(obtained: Obtained<'_, D>, mut error: RunError, lock_shown: &Path) -> RunError {
    let Obtained::Held { held, .. } = obtained else {
        return error;
    };
    if let Err(e) = held.discard()
        && let RunError::Refused { changed, not_removed, .. } = &mut error
    {
        *changed = true;
        *not_removed = Some((lock_shown.to_path_buf(), lock_io(e)));
    }
    error
}

/// This operation's lock record for `site` (§259.6's fields; in 7a `last_heartbeat_wall_time` equals
/// `creation_wall_time`).
fn record_for<D: DirHandle>(
    site: &LockSite<'_, D>,
    cfg: &RunConfig,
    workspace_path: String,
) -> LockResult<LockRecord> {
    let now = wall_time_ns();
    Ok(LockRecord {
        complete_lock_key: site.complete_lock_key()?,
        operation_id: cfg.operation_id.clone(),
        owner_instance_id: cfg.owner_instance_id.clone(),
        boot_session_id: cfg.boot_session_id.clone(),
        target_path_key: site.target_path_key(),
        workspace_path,
        creation_wall_time: now,
        last_heartbeat_wall_time: now,
    })
}

/// Why a §99 check did not pass.
pub(crate) enum Fault {
    /// `still_owned` said no: another run took the lock over or removed it.
    Lost,
    /// It could not tell, or a step after it failed, at this path.
    Io(PathBuf, FsError),
}

impl Fault {
    pub(crate) fn into_error(self, step: RunStep) -> RunError {
        match self {
            Fault::Lost => lost(),
            Fault::Io(path, error) => RunError::Failed { step, path, error },
        }
    }
}

/// §99 (`S99_check`, and `S21_1_s3` under `--restart`): this run still owns its lock.
pub(crate) fn owned<D: DirHandle>(held: &Held<'_, D>, lock_shown: &Path) -> Result<(), Fault> {
    match held.still_owned() {
        Ok(true) => Ok(()),
        Ok(false) => Err(Fault::Lost),
        Err(e) => Err(Fault::Io(lock_shown.to_path_buf(), lock_io(e))),
    }
}

const LOST: &str =
    "this run no longer holds the destination's lock: another run took it over or removed it";

/// The copy's guard (`copy_file_guarded`, `copy_tree_at`): §99 as the `FsError` the engine aborts on. A check that
/// cannot tell counts as lost: the copy must not go on writing.
pub(crate) fn guarded<D: DirHandle>(held: &Held<'_, D>) -> flux_fs::Result<()> {
    match held.still_owned() {
        Ok(true) => Ok(()),
        Ok(false) => Err(FsError::new(Code::TargetLockBusy, std::io::Error::other(LOST))),
        Err(e) => Err(FsError::new(Code::TargetLockBusy, lock_io(e).source)),
    }
}

/// Ownership lost mid-run (decision 13): TARGET_LOCK_BUSY, exit 1, the state left as it is.
pub(crate) fn lost() -> RunError {
    RunError::Refused {
        refusal: Box::new(Refusal {
            code: LockCode::TargetLockBusy,
            holder: None,
            detail: format!("{LOST}; this operation's state stays as it is"),
        }),
        changed: true,
        not_removed: None,
    }
}

/// A lock-protocol error as the run's: a refusal keeps its code; an I/O error is the run's failure at `step`, `path`.
pub(crate) fn from_lock(e: LockError, step: RunStep, path: &Path, changed: bool) -> RunError {
    match e {
        LockError::Refused(refusal) => RunError::Refused { refusal, changed, not_removed: None },
        LockError::Io(error) => RunError::Failed { step, path: path.to_path_buf(), error },
    }
}

/// An I/O failure as the run's, at `step` and `path`.
pub(crate) fn failed(step: RunStep, path: &Path, error: FsError) -> RunError {
    RunError::Failed { step, path: path.to_path_buf(), error }
}

/// The I/O error inside a lock-protocol error. A refusal cannot come from the calls this is used on; its detail is
/// kept if one ever does.
pub(crate) fn lock_io(e: LockError) -> FsError {
    match e {
        LockError::Io(e) => e,
        LockError::Refused(r) => FsError::new(Code::IoError, std::io::Error::other(r.detail)),
    }
}
```

  Create `crates/flux-core/src/run/place.rs`:

```rust
//! Where an operation's state lives - what differs between a tree and a single file. A tree's state is the workspace
//! `DEST/.flux/operations/<id>/manifest` and its lock is beside DEST; a single file's state is the record
//! `<target>.flux-state.<id>` beside the target, with its lock.

use super::session::{failed, from_lock};
use super::{RunError, RunStep};
use crate::copy::{CopyError, CopyStep, split_destination};
use crate::lock::LockResult;
use crate::prior::{Scan, scan_tree};
use crate::state::{
    FLUX_DIR, Kind, MANIFEST, OPERATIONS_DIR, OperationState, create_workspace, operations_dir,
    remove_empty_control_dirs, retire_workspace, write_state,
};
use crate::tree::{WeakIdentityWarnings, preflight};
use flux_fs::{Code, DestinationRoot, DirHandle, FileIdentity, FileType, FsError, Safety};
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// One step of "The run" that differs between a tree and a single file.
pub(crate) trait Place<D: DirHandle> {
    /// The operation kind its state records.
    fn kind(&self) -> Kind;
    /// The destination as the operator gave it (the state's `destination_root`, Part 3a decision 5).
    fn destination(&self) -> &Path;
    /// The directory holding the lock, as messages name it.
    fn holder_shown(&self) -> &Path;
    /// The lock record's `workspace_path` for operation `id`: `operations/<id>` or `adjacent/<id>`.
    fn workspace_path(&self, id: &str) -> String;
    /// How messages name operation `id`'s state.
    fn shown(&self, id: &str) -> PathBuf;
    /// Step 4: the prior operations (§21.1).
    fn scan(&self, own_id: &str) -> LockResult<Scan>;
    /// Step 5: create this operation's state, CREATED; for a tree, DEST first where it is absent (E1).
    fn create(&mut self, state: &OperationState) -> Result<(), RunError>;
    /// Rewrite `state` where its operation's state lives: this run's, or a prior's under `--restart`.
    fn write(&self, state: &OperationState) -> flux_fs::Result<()>;
    /// Remove operation `id`'s state: a workspace, retired first (decision 17), or a record and its temporary.
    fn remove(&self, id: &str) -> Result<(), (PathBuf, FsError)>;
    /// Finish step 4: the empty control directories; on a `rollback` (Q-I), also a DEST this run made.
    fn remove_control_dirs(&mut self, rollback: bool) -> Result<(), (PathBuf, FsError)>;
}

/// What B1 resolved for a tree's DEST: the directory holding the lock, DEST's name in it, and DEST if it exists.
pub(crate) struct LocatedTree<D> {
    pub(crate) holder: D,
    pub(crate) holder_shown: PathBuf,
    pub(crate) name: Option<OsString>,
    pub(crate) dest: Option<D>,
}

/// B1 for DEST: resolve it the way the run uses it - its parent, which holds the lock, then DEST through that parent,
/// never following a link at DEST itself - and run the §129 identity pre-flight against DEST, or its parent while DEST
/// is absent, before anything is created. A filesystem root holds its own lock (`T/.flux-root.lock`).
pub(crate) fn locate_tree<F: DestinationRoot>(
    fs: &F,
    dst_root: &Path,
    src_identity: FileIdentity,
    safety: Safety,
    warnings: &mut WeakIdentityWarnings,
) -> Result<LocatedTree<F::Dir>, CopyError> {
    let resolve = |e| CopyError::at(CopyStep::Resolve, e);
    if dst_root.file_name().is_none() {
        let holder = fs.destination_root(dst_root).map_err(resolve)?;
        preflight(src_identity, holder.identity().map_err(resolve)?, safety, warnings)?;
        let dest = fs.destination_root(dst_root).map_err(resolve)?;
        return Ok(LocatedTree {
            holder,
            holder_shown: dst_root.to_path_buf(),
            name: None,
            dest: Some(dest),
        });
    }
    let (parent_path, name) = split_destination(dst_root)?;
    let holder = fs.destination_root(parent_path).map_err(resolve)?;
    let dest = match holder.metadata(name) {
        Err(e) if e.source.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(resolve(e)),
        Ok(m) if m.file_type == FileType::Dir => Some(holder.open_dir(name).map_err(resolve)?),
        Ok(m) if m.file_type == FileType::Symlink => {
            return Err(resolve(FsError::new(
                Code::SafetyRejected,
                std::io::Error::other("the destination is a symlink, which Flux never writes through"),
            )));
        }
        Ok(_) => {
            return Err(resolve(FsError::new(
                Code::DestinationError,
                std::io::Error::from(ErrorKind::NotADirectory),
            )));
        }
    };
    let anchor = match &dest {
        Some(d) => d.identity(),
        None => holder.identity(),
    }
    .map_err(resolve)?;
    preflight(src_identity, anchor, safety, warnings)?;
    Ok(LocatedTree {
        holder,
        holder_shown: parent_path.to_path_buf(),
        name: Some(name.to_os_string()),
        dest,
    })
}

/// A tree's DEST: its parent holds the lock, and its state lives under `DEST/.flux/operations/`.
pub(crate) struct TreePlace<'p, F: DestinationRoot> {
    pub(crate) fs: &'p F,
    /// DEST's parent, which holds the lock - or DEST itself, for a filesystem root.
    pub(crate) holder: &'p F::Dir,
    pub(crate) holder_shown: PathBuf,
    /// DEST's name in `holder`; `None` for a root.
    pub(crate) name: Option<OsString>,
    /// DEST, once it exists (E1: the run makes it at step 5).
    pub(crate) dest: Option<F::Dir>,
    pub(crate) dest_shown: PathBuf,
    /// This run made DEST: a rollback removes it, and the outcome counts it.
    pub(crate) created_dest: bool,
    /// `DEST/.flux/operations/`, once step 5 made or opened it.
    pub(crate) operations: Option<F::Dir>,
}

impl<F: DestinationRoot> TreePlace<'_, F> {
    fn operations_shown(&self) -> PathBuf {
        self.dest_shown.join(FLUX_DIR).join(OPERATIONS_DIR)
    }

    fn operations(&self) -> &F::Dir {
        self.operations.as_ref().expect("step 5 made operations/ before any state is rewritten or removed")
    }
}

impl<F: DestinationRoot> Place<F::Dir> for TreePlace<'_, F> {
    fn kind(&self) -> Kind {
        Kind::Tree
    }

    fn destination(&self) -> &Path {
        &self.dest_shown
    }

    fn holder_shown(&self) -> &Path {
        &self.holder_shown
    }

    fn workspace_path(&self, id: &str) -> String {
        format!("operations/{id}")
    }

    fn shown(&self, id: &str) -> PathBuf {
        self.operations_shown().join(id).join(MANIFEST)
    }

    fn scan(&self, own_id: &str) -> LockResult<Scan> {
        match &self.dest {
            Some(dest) => scan_tree(dest, &self.dest_shown, own_id),
            None => Ok(Scan::default()),
        }
    }

    fn create(&mut self, state: &OperationState) -> Result<(), RunError> {
        if self.dest.is_none() {
            let name = self.name.as_deref().expect("only a named DEST can be absent");
            let dest = match self.holder.create_dir(name) {
                Ok(d) => {
                    self.created_dest = true;
                    d
                }
                Err(e) if e.source.kind() == ErrorKind::AlreadyExists => self
                    .holder
                    .open_dir(name)
                    .map_err(|e| failed(RunStep::State, &self.dest_shown, e))?,
                Err(e) => return Err(failed(RunStep::State, &self.dest_shown, e)),
            };
            self.dest = Some(dest);
        }
        let dest = self.dest.as_ref().expect("made above");
        let operations = operations_dir(dest, &self.dest_shown)
            .map_err(|e| from_lock(e, RunStep::State, &self.operations_shown(), self.created_dest))?;
        let workspace = create_workspace(&operations, state)
            .map_err(|e| failed(RunStep::State, &self.shown(&state.operation_id), e))?;
        drop(workspace);
        self.operations = Some(operations);
        Ok(())
    }

    fn write(&self, state: &OperationState) -> flux_fs::Result<()> {
        let workspace = self.operations().open_dir(OsStr::new(&state.operation_id))?;
        write_state(&workspace, OsStr::new(MANIFEST), state)
    }

    fn remove(&self, id: &str) -> Result<(), (PathBuf, FsError)> {
        retire_workspace(self.operations(), id).map_err(|e| (self.operations_shown().join(id), e))
    }

    fn remove_control_dirs(&mut self, rollback: bool) -> Result<(), (PathBuf, FsError)> {
        // Each handle closes before its directory is removed.
        self.operations = None;
        let dest = self.dest.as_ref().expect("DEST exists once this run's state does");
        remove_empty_control_dirs(dest).map_err(|e| (self.dest_shown.join(FLUX_DIR), e))?;
        if rollback && self.created_dest {
            let name = self.name.as_deref().expect("a DEST this run made has a name");
            self.dest = None;
            self.holder.remove_dir(name).map_err(|e| (self.dest_shown.clone(), e))?;
            self.created_dest = false;
        }
        Ok(())
    }
}
```

  (`TreePlace::fs` is read by Task 8's sweep. Until then, if clippy reports it as never read, keep the field and add
  `#[allow(dead_code)]` on it with the comment `// read by the --restart sweep (Task 8)`, and remove the attribute in
  Task 8.)

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p flux-core --lib run::`
Expected: `test result: ok. 17 passed`. Then `cargo test -p flux-core --lib`: `280 passed` (263 + 17), and
`cargo clippy -p flux-core --all-targets -- -D warnings` is clean.

- [ ] **Step 5: Prove the tests can fail** (one at a time; restore after each):
  - in `remove_own`, move the `rewrite_record` after `place.remove(...)`:
    `the_record_stops_naming_the_state_before_the_state_is_removed` FAILED (the lock still names `operations/<id>`
    at the retire);
  - in `open_operation`, create the state AFTER `held.write_record(record)` (swap the two blocks for the `Held` arm):
    `the_lock_comes_first_then_the_state_then_the_record_naming_it` FAILED;
  - in `finish`, map `Ended::Lost` to `fail(place, locked)`: `ownership_lost_mid_copy...` FAILED (the state becomes
    FAILED, or the lock is unlinked);
  - in `tree`, compute `ended` with `Ended::Failed` instead of `Ended::RefusedUnchanged`: both rollback tests FAILED;
  - in `TreePlace::remove_control_dirs`, drop the `rollback && self.created_dest` block:
    `a_rollback_removes_a_dest_the_run_made` FAILED;
  - in `complete`, ignore `leftovers`: `a_temporary_the_copy_could_not_remove_keeps_the_completed_state` FAILED;
  - in `open_operation`'s `Claimed` arm, drop the takeover write: `break_lock_takes_an_empty_lock_over...` FAILED;
  - in `open_operation`, replace `Ok(Overwritten::Restart) => continue` with
    `Ok(Overwritten::Restart) => return Err(lost())`: `a_takeover_whose_lock_moves...` FAILED;
  - in `give_back`, skip the `discard`: `a_resumable_prior_operation_is_refused_and_the_lock_removed_again` FAILED;
  - in `give_back`, never set `not_removed`: `a_refusal_whose_lock_cannot_be_removed_names_it_and_is_exit_1` FAILED;
  - in `complete`, drop the `Released::NotOwned` warning: `ownership_lost_after_completed_is_a_warning...` FAILED;
  - in `open_operation`, drop the `BrokenLeftover` push: `a_dead_owners_lock_left_beside_the_new_one_is_a_warning`
    FAILED.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-core/src/lib.rs crates/flux-core/src/run
git commit -m "feat(core): the run - a tree copy under the destination's lock, from the capability gate to the release (cut 7a Part 3b-1)"
```

---

### Task 8: `--restart` supersedes the prior operations (Q-H (a), J1)

**Files:**
- Create: `crates/flux-core/src/run/restart.rs`
- Modify: `crates/flux-core/src/run/mod.rs` (`mod restart;`), `session.rs` (step 4 and after the record),
  `place.rs` (`Place::sweep`, `TreePlace::sweep`, `remove_below`), `tests.rs`

The tests are written below; implement until they pass.

- [ ] **Step 1: Add the failing tests** at the end of `run/tests.rs`:

```rust
#[test]
fn restart_supersedes_a_prior_deleting_its_partials_then_its_workspace() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    let other = format!("/p/dest/other.flux-partial.{}", id(6));
    fs.write_file(&other, b"not a superseded operation's");
    let (r, _) = run_tree(&fs, &restart());
    ok(&r);
    assert!(!fs.exists(&partial), "the superseded operation's partial is deleted");
    assert!(fs.exists(&other), "only a superseded operation's partials");
    assert!(!fs.exists(format!("/p/dest/.flux/operations/{}", id(5))), "and then its workspace");
    let c = calls(&fs);
    let record = at(&c, "write_at_start(");
    let abandon = at(&c, &format!("rename_replace(/p/dest/.flux/operations/{}/manifest.tmp", id(5)));
    let delete = at(&c, &format!("remove_file({partial})"));
    let retire = at(&c, &format!("rename_no_replace(/p/dest/.flux/operations/{} ", id(5)));
    assert!(record < abandon && abandon < delete && delete < retire, "Q-H (a): {c:?}");
}

#[test]
fn a_partial_restart_cannot_delete_keeps_its_prior_abandoned() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    // `remove_file`: this run's CREATED and the prior's ABANDONED state writes clear their temporaries (1, 2); then
    // the partial (3).
    fs.fail_nth("remove_file", 3, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &restart());
    ok(&r);
    let c = calls(&fs);
    let removals: Vec<&String> = c.iter().filter(|x| x.starts_with("remove_file(")).collect();
    assert!(removals[2].contains("old.flux-partial"), "the 3rd removal is the partial: {removals:?}");
    assert!(fs.exists(&partial));
    let kept = manifest(&fs, &id(5));
    assert_eq!((kept.state, kept.superseded_by.as_deref()), (OpState::Abandoned, Some(ID)));
    assert!(r.warnings.iter().any(|w| matches!(w, RunWarning::PartialKept { .. })), "{:?}", r.warnings);
}

#[test]
fn restart_with_nothing_to_supersede_is_a_plain_run() {
    let fs = fake();
    let (r, _) = run_tree(&fs, &restart());
    ok(&r);
    assert!(!fs.exists("/p/dest/.flux") && !fs.exists(LOCK));
}

#[test]
fn restart_stops_when_ownership_is_lost_and_deletes_nothing_after() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    // `rename_replace`: this run's CREATED manifest (1), then the prior's ABANDONED one (2): just before it, the
    // lock is taken over.
    fs.on_nth("rename_replace", 2, |fs| fs.write_file(LOCK, b"another run's bytes"));
    let (r, _) = run_tree(&fs, &restart());
    assert_eq!(refused(&r.stop), (LockCode::TargetLockBusy, true));
    assert!(r.copy.is_none());
    assert!(fs.exists(&partial), "§99 failed: nothing more is deleted");
    assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b"another run's bytes"[..]), "never unlinked");
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p flux-core --lib run::tests::restart`
Expected: FAILED - with Task 7's code `--restart` proceeds without superseding, so the partial and the prior's
workspace survive.

- [ ] **Step 3: Implement.** Create `crates/flux-core/src/run/restart.rs`:

```rust
//! `--restart` (the design's "`--restart` and `--break-lock`", amended by Q-H (a)).

use super::RunWarning;
use super::place::Place;
use super::session::{Fault, Locked, owned};
use crate::prior::PriorOp;
use crate::state::{OpState, OperationState};
use flux_fs::DirHandle;

/// Supersede every resumable prior operation, with this operation's state and record already written (Q-H (a)): each
/// is durably set ABANDONED with `superseded_by` = this operation, then the priors' partials are deleted, then each
/// prior's state.
///
/// `S21_1_s3`: §99's `still_owned` before EVERY one of those mutations (spec:9365-9367); when it fails, the caller
/// closes the lock without unlinking (`S21_1_s3_refuse_close`). A partial that cannot be deleted keeps its operation's
/// ABANDONED state as the record naming it (J1). A state that cannot be removed is a warning: an ABANDONED operation
/// is passed over by every later run.
pub(crate) fn supersede<D: DirHandle, P: Place<D>>(
    place: &P,
    locked: &Locked<'_, D>,
    priors: &[PriorOp],
    warnings: &mut Vec<RunWarning>,
) -> Result<(), Fault> {
    let own = &locked.state.operation_id;
    for prior in priors {
        owned(&locked.held, &locked.lock_shown)?;
        let abandoned = OperationState {
            state: OpState::Abandoned,
            superseded_by: Some(own.clone()),
            ..prior.state.clone()
        };
        place.write(&abandoned).map_err(|e| Fault::Io(prior.shown.clone(), e))?;
    }
    let gone = place.sweep(priors, &locked.held, &locked.lock_shown, warnings)?;
    for prior in priors.iter().filter(|p| gone.contains(&p.state.operation_id)) {
        owned(&locked.held, &locked.lock_shown)?;
        if let Err((path, error)) = place.remove(&prior.state.operation_id) {
            warnings.push(RunWarning::NotRemoved { path, error });
        }
    }
    Ok(())
}
```

  In `run/mod.rs`, after `mod place;`, add `mod restart;`.

  In `run/session.rs`:
  - add `use super::restart::supersede;`;
  - replace the step-4 `match place.scan(id) {` statement's first arm and binding - i.e. change
    `match place.scan(id) {` to `let priors = match place.scan(id) {`, the arm
    `Ok(scan) if scan.resumable.is_empty() || cfg.restart => {}` to
    `Ok(scan) if scan.resumable.is_empty() || cfg.restart => scan.resumable,`, and the statement's closing `}` to
    `};` - and drop the `(Task 8 binds ...)` note from the step-4 comment;
  - after the `let mut locked = match obtained { ... };` statement and before `// The last write of step 5`, insert:

```rust
        // Q-H (a): `--restart` supersedes, with this operation's state and record already written. When §99 fails
        // there, `locked` drops without unlinking (`S21_1_s3_refuse_close`).
        if let Err(fault) = supersede(place, &locked, &priors, warnings) {
            return Err(match fault {
                Fault::Lost => lost(),
                Fault::Io(path, error) => {
                    stop_after_record(place, locked, RunStep::Restart, path, error)
                }
            });
        }
```

  In `run/place.rs`:
  - extend the imports: `use super::RunWarning;`, `use super::session::{Fault, owned};`,
    `use crate::lock::Held;`, `use crate::prior::PriorOp;`, `use crate::state::{PARTIAL_INFIX, id_after};`,
    `use crate::tree::reserved_path;`, `use crate::walk::{WalkEvent, walk};`, `use std::collections::BTreeSet;`;
  - add to `trait Place`, after `write`:

```rust
    /// `--restart`: delete the superseded operations' partials, `still_owned` before each deletion. Returns the ids
    /// whose partials are all gone; a partial that stays keeps its operation's ABANDONED state as its record (J1).
    fn sweep(
        &self,
        priors: &[PriorOp],
        held: &Held<'_, D>,
        lock_shown: &Path,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<Vec<String>, Fault>;
```

  - add to `impl Place<F::Dir> for TreePlace<'_, F>`, after `write`:

```rust
    fn sweep(
        &self,
        priors: &[PriorOp],
        held: &Held<'_, F::Dir>,
        lock_shown: &Path,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<Vec<String>, Fault> {
        let ids: BTreeSet<&str> = priors.iter().map(|p| p.state.operation_id.as_str()).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let dest = self.dest.as_ref().expect("DEST exists once this run's state does");
        let operations = self.operations_shown();
        let mut kept: BTreeSet<&str> = BTreeSet::new();
        // J1: every `<name>.flux-partial.<prior-id>` under DEST, found by a walk of DEST by path that skips the
        // reserved control directories, and deleted through handles opened from DEST's own, one component at a time.
        let events = match walk(self.fs, &self.dest_shown) {
            Ok(events) => events,
            Err(error) => {
                warnings.push(RunWarning::PartialKept { path: self.dest_shown.clone(), error, kept: operations });
                return Ok(Vec::new());
            }
        };
        for item in events {
            let path = match item {
                Ok(WalkEvent::File { path }) => path,
                Ok(_) => continue,
                // A directory the sweep cannot read may hold a partial: every prior's state stays as its record.
                Err(e) => {
                    let path = self.dest_shown.join(&e.path);
                    warnings.push(RunWarning::PartialKept { path, error: e.cause, kept: operations.clone() });
                    kept.extend(ids.iter().copied());
                    continue;
                }
            };
            if reserved_path(&path) {
                continue;
            }
            let Some(prior) = path
                .file_name()
                .and_then(|n| id_after(n, PARTIAL_INFIX))
                .and_then(|i| ids.get(i).copied())
            else {
                continue;
            };
            owned(held, lock_shown)?;
            if let Err(error) = remove_below(dest, &path) {
                let shown = self.dest_shown.join(&path);
                warnings.push(RunWarning::PartialKept { path: shown, error, kept: self.shown(prior) });
                kept.insert(prior);
            }
        }
        Ok(ids.difference(&kept).map(|i| (*i).to_string()).collect())
    }
```

  - add at the end of `place.rs`:

```rust
/// Remove the file at `rel` below `dest` through handles opened one component at a time, so no link is followed
/// (§149.7; J1).
fn remove_below<D: DirHandle>(dest: &D, rel: &Path) -> flux_fs::Result<()> {
    let mut parts: Vec<&OsStr> = rel.iter().collect();
    let name = parts.pop().expect("a walk path ends in a name");
    let mut opened: Option<D> = None;
    for part in parts {
        let next = opened.as_ref().unwrap_or(dest).open_dir(part)?;
        opened = Some(next);
    }
    opened.as_ref().unwrap_or(dest).remove_file(name)
}
```

  (If Task 7 added `#[allow(dead_code)]` on `TreePlace::fs`, remove it now.)

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p flux-core --lib run::`
Expected: `test result: ok. 21 passed`; then `cargo test -p flux-core --lib`: `284 passed`; clippy clean.

- [ ] **Step 5: Prove the tests can fail** (restore after each):
  - in `supersede`, delete the first loop's `owned(...)?;` AND the sweep's `owned(held, lock_shown)?;`:
    `restart_stops_when_ownership_is_lost...` FAILED (the partial is deleted);
  - in `supersede`, call `place.sweep` BEFORE the ABANDONED loop: `restart_supersedes_a_prior...` FAILED on the order;
  - in `TreePlace::sweep`, ignore a failed `remove_below` (no `kept.insert`): `a_partial_restart_cannot_delete...`
    FAILED (the workspace is removed);
  - in `TreePlace::sweep`, accept any `.flux-partial.<id>` (drop the `ids.get(i)` filter):
    `restart_supersedes_a_prior...` FAILED (`other.flux-partial` deleted).

- [ ] **Step 6: Commit**

```bash
git add crates/flux-core/src/run
git commit -m "feat(core): --restart supersedes each resumable prior under a full still_owned before every mutation (cut 7a Part 3b-1)"
```

---

### Task 9: The single-file run

**Files:**
- Modify: `crates/flux-core/src/copy.rs` (`identity_gate` becomes `pub(crate)`; a new `prepare_file`)
- Modify: `crates/flux-core/src/run/mod.rs` (`file`), `run/place.rs` (`FilePlace`), `run/tests.rs`

The tests are written below; implement until they pass.

- [ ] **Step 1: Add the failing tests.** Add `use crate::copy::CopyStep;` to the imports at the top of `run/tests.rs`,
  and at its end:

```rust
const T_LOCK: &str = "/p/t.flux-lock";

fn record_path(id: &str) -> String {
    format!("/p/t.flux-state.{id}")
}

fn run_file(fs: &FaultFs, c: &RunConfig) -> Run<Result<flux_fs::Outcome, CopyError>> {
    file(fs, Path::new("/src/a"), Path::new("/p/t"), &opts(), c)
}

fn file_prior(fs: &FaultFs, n: u8) {
    let prior = OperationState {
        state: OpState::Failed,
        ..OperationState::created(&id(n), Kind::File, Path::new("/p/t"), 1)
    };
    fs.write_file(record_path(&id(n)), &prior.encode());
}

#[test]
fn a_single_file_run_copies_and_leaves_no_record_behind() {
    let fs = fake();
    let r = run_file(&fs, &cfg());
    assert!(r.stop.is_none() && r.warnings.is_empty(), "{:?} {:?}", r.stop, r.warnings);
    assert!(matches!(r.copy, Some(Ok(_))), "{:?}", r.copy);
    assert_eq!(fs.read_file("/p/t").as_deref(), Some(&b"A"[..]));
    assert!(!fs.exists(record_path(ID)) && !fs.exists(T_LOCK));
    let c = calls(&fs);
    let lock = at(&c, "create_lock(/p/t.flux-lock)");
    let state = at(&c, &format!("create_new(/p/t.flux-state.{ID}.tmp)"));
    assert!(lock < state, "{c:?}");
}

#[test]
fn a_target_name_too_long_for_its_record_is_refused_before_anything_is_made() {
    let fs = fake();
    let long = format!("/p/{}", "n".repeat(255 - 47));
    let r = file(&fs, Path::new("/src/a"), Path::new(&long), &opts(), &cfg());
    assert_eq!(refused(&r.stop), (LockCode::PathComponentInvalid, false));
    assert!(!fs.called("create_lock") && !fs.called("create_new"));
}

#[test]
fn a_mistyped_source_creates_nothing() {
    let fs = fake();
    let r = file(&fs, Path::new("/src/missing"), Path::new("/p/t"), &opts(), &cfg());
    assert!(r.stop.is_none());
    assert!(matches!(&r.copy, Some(Err(e)) if e.step == CopyStep::Source), "{:?}", r.copy);
    assert!(!fs.called("create_lock"), "B1: {:?}", fs.calls());
}

#[test]
fn a_failed_single_file_copy_leaves_a_failed_record_and_releases_the_lock() {
    let fs = fake();
    // `create_new`: the CREATED record (1), TRANSFERRING (2), then the copy's temporary (3), whose write fails.
    fs.on_nth("create_new", 3, |fs| fs.fail_write(std::io::Error::other("injected write")));
    let r = run_file(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert!(matches!(&r.copy, Some(Err(e)) if e.step == CopyStep::Stream), "{:?}", r.copy);
    let state = decode_state(&fs.read_file(record_path(ID)).unwrap()).unwrap();
    assert_eq!(state.state, OpState::Failed);
    assert!(!fs.exists(T_LOCK));
}

#[test]
fn a_resumable_single_file_prior_is_refused_without_restart() {
    let fs = fake();
    file_prior(&fs, 5);
    let r = run_file(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::ResumableOperationExists, false));
    assert!(!fs.exists(T_LOCK) && !fs.exists("/p/t"));
}

#[test]
fn restart_supersedes_a_single_files_prior_and_its_partial() {
    let fs = fake();
    file_prior(&fs, 5);
    fs.write_file(format!("/p/t.flux-partial.{}", id(5)), b"half");
    let r = run_file(&fs, &restart());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?} {:?}", r.stop, r.copy);
    assert!(!fs.exists(record_path(&id(5))) && !fs.exists(format!("/p/t.flux-partial.{}", id(5))));
}

#[test]
fn a_single_file_refusal_under_the_lock_is_rolled_back() {
    let fs = fake();
    // B1's checks pass; then, as the lock is created, the target becomes a directory: the copy's own gate refuses it.
    fs.on_nth("create_lock", 1, |fs| fs.create_dir(Path::new("/p/t")).unwrap());
    let r = run_file(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert!(
        matches!(&r.copy, Some(Err(e)) if e.code() == Code::SafetyRejected && e.leftover.is_none()),
        "{:?}",
        r.copy
    );
    assert!(!fs.exists(record_path(ID)) && !fs.exists(T_LOCK), "what the run made is gone");
    assert!(fs.exists("/p/t"));
}
```

- [ ] **Step 2: Run them to see them fail to compile**

Run: `cargo test -p flux-core --lib run::tests`
Expected: `error[E0425]: cannot find function `file``.

- [ ] **Step 3: Implement.** In `copy.rs`, change `fn identity_gate<D: DirHandle>(` to
  `pub(crate) fn identity_gate<D: DirHandle>(`, and after `copy_file` add:

```rust
/// B1 (cut 7a Part 3b): `copy_file`'s checks that need no lock - Step 0, the source's type (Step 2) and the identity
/// gate (Step 2a) - made by the run before it takes the destination's lock, so a refusal there creates nothing. The
/// copy makes Steps 2 and 2a again under the lock. Returns DEST's parent, its path, and the target's name.
pub(crate) fn prepare_file<'d, F: DestinationRoot>(
    fs: &F,
    src: &Path,
    dst: &'d Path,
    opts: &CopyOptions,
) -> std::result::Result<(F::Dir, &'d Path, &'d OsStr), CopyError> {
    if src == dst {
        return Err(CopyError::at(
            CopyStep::Resolve,
            FsError::new(
                Code::SafetyRejected,
                std::io::Error::other("source and destination are the same path"),
            ),
        ));
    }
    let (parent_path, name) = split_destination(dst)?;
    let parent = fs.destination_root(parent_path).map_err(|e| CopyError::at(CopyStep::Resolve, e))?;
    let src_meta = fs.metadata(src).map_err(|e| CopyError::at(CopyStep::Source, e))?;
    if src_meta.file_type != FileType::File {
        return Err(CopyError::at(
            CopyStep::Source,
            FsError::new(Code::SpecialFileUnsupported, std::io::Error::other("not a regular file")),
        ));
    }
    identity_gate(&parent, name, &src_meta, opts.safety, opts.publish)?;
    Ok((parent, parent_path, name))
}
```

  In `run/place.rs`, add `record_name`, `remove_record`, `scan_file` and `temp_path`/`OperationId` to the imports
  (`use crate::state::{record_name, remove_record};`, `use crate::prior::scan_file;`,
  `use flux_fs::{OperationId, temp_path};`) and, at the end of the file:

```rust
/// A single file's directory: it holds the lock, the record `<target>.flux-state.<id>` and the copy.
pub(crate) struct FilePlace<'p, D: DirHandle> {
    pub(crate) dir: &'p D,
    pub(crate) dir_shown: PathBuf,
    pub(crate) target: OsString,
    /// The destination as the operator gave it.
    pub(crate) destination: PathBuf,
}

impl<D: DirHandle> Place<D> for FilePlace<'_, D> {
    fn kind(&self) -> Kind {
        Kind::File
    }

    fn destination(&self) -> &Path {
        &self.destination
    }

    fn holder_shown(&self) -> &Path {
        &self.dir_shown
    }

    fn workspace_path(&self, id: &str) -> String {
        format!("adjacent/{id}")
    }

    fn shown(&self, id: &str) -> PathBuf {
        self.dir_shown.join(record_name(&self.target, id))
    }

    fn scan(&self, own_id: &str) -> LockResult<Scan> {
        scan_file(self.dir, &self.target, &self.dir_shown, own_id)
    }

    fn create(&mut self, state: &OperationState) -> Result<(), RunError> {
        self.write(state).map_err(|e| failed(RunStep::State, &self.shown(&state.operation_id), e))
    }

    fn write(&self, state: &OperationState) -> flux_fs::Result<()> {
        write_state(self.dir, &record_name(&self.target, &state.operation_id), state)
    }

    fn sweep(
        &self,
        priors: &[PriorOp],
        held: &Held<'_, D>,
        lock_shown: &Path,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<Vec<String>, Fault> {
        let mut gone = Vec::new();
        for prior in priors {
            let id = &prior.state.operation_id;
            // `<target>.flux-partial.<id>`, looked up by this target's own name (E3).
            let partial = temp_path(Path::new(&self.target), &OperationId::new(id.as_str())).into_os_string();
            owned(held, lock_shown)?;
            match self.dir.remove_file(&partial) {
                Ok(()) => gone.push(id.clone()),
                Err(e) if e.source.kind() == ErrorKind::NotFound => gone.push(id.clone()),
                Err(error) => warnings.push(RunWarning::PartialKept {
                    path: self.dir_shown.join(&partial),
                    error,
                    kept: prior.shown.clone(),
                }),
            }
        }
        Ok(gone)
    }

    fn remove(&self, id: &str) -> Result<(), (PathBuf, FsError)> {
        remove_record(self.dir, &record_name(&self.target, id)).map_err(|e| (self.shown(id), e))
    }

    fn remove_control_dirs(&mut self, _rollback: bool) -> Result<(), (PathBuf, FsError)> {
        // A single file's state needs no control directory (F2).
        Ok(())
    }
}
```

  In `run/mod.rs`, extend the imports (`use crate::copy::{CopyError, copy_file_guarded, prepare_file};`,
  `use crate::lock::LockCode;`, `use crate::lock::site::NAME_LIMIT;`, `use crate::state::file_names_fit;`,
  `use flux_fs::Outcome;`, and `FilePlace` in the `use place::{...}` list) and, after `tree`, add:

```rust
/// A single-file copy under the destination's lock ("The run"; its state is the record beside the target, F2).
pub fn file<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    dst: &Path,
    opts: &CopyOptions,
    cfg: &RunConfig,
) -> Run<Result<Outcome, CopyError>> {
    let mut run = Run { copy: None, stop: None, warnings: Vec::new() };
    let opts = CopyOptions { operation_id: OperationId::new(cfg.operation_id.as_str()), ..opts.clone() };
    // B1.
    let (parent, parent_path, name) = match prepare_file(fs, src, dst, &opts) {
        Ok(p) => p,
        Err(e) => {
            run.copy = Some(Err(e));
            return run;
        }
    };
    // Every name the run makes beside the target must fit (Part 3a decision 4), before anything is made.
    if !file_names_fit(name) {
        run.stop = Some(RunError::Refused {
            refusal: Box::new(Refusal {
                code: LockCode::PathComponentInvalid,
                holder: None,
                detail: format!(
                    "the target name {} leaves no room for the names Flux keeps beside it (its state record's temporary is the name plus 48 units, over {NAME_LIMIT})",
                    name.to_string_lossy()
                ),
            }),
            changed: false,
            not_removed: None,
        });
        return run;
    }
    // Step 1.
    let capability = match check_capability(&parent) {
        Ok(c) => c,
        Err(e) => {
            run.stop = Some(from_lock(e, RunStep::Lock, parent_path, false));
            return run;
        }
    };
    let site = match LockSite::file(&parent, name) {
        Ok(s) => s,
        Err(e) => {
            run.stop = Some(from_lock(e, RunStep::Lock, parent_path, false));
            return run;
        }
    };
    let mut place = FilePlace {
        dir: &parent,
        dir_shown: parent_path.to_path_buf(),
        target: name.to_os_string(),
        destination: dst.to_path_buf(),
    };
    // Steps 3-5.
    let locked = match open_operation(&site, capability, &mut place, cfg, &mut run.warnings) {
        Ok(l) => l,
        Err(e) => {
            run.stop = Some(e);
            return run;
        }
    };
    // Step 6, under §99.
    let copied = {
        let guard = || guarded(&locked.held);
        copy_file_guarded(fs, src, &parent, name, &opts, &guard)
    }
    .map_err(|mut e| {
        // As `copy_file` reports it: the leftover in the frame of the path the operator gave.
        if let Some((path, _)) = e.leftover.as_mut() {
            *path = dst.with_file_name(&*path);
        }
        e
    });
    let ended = match &copied {
        Ok(_) => Ended::Completed { leftovers: false },
        Err(e) if e.code() == Code::TargetLockBusy => Ended::Lost,
        Err(e) if e.code() == Code::SafetyRejected && e.leftover.is_none() => Ended::RefusedUnchanged,
        Err(_) => Ended::Failed,
    };
    run.stop = finish(&mut place, locked, ended, &mut run.warnings);
    run.copy = Some(copied);
    run
}
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p flux-core --lib run::`
Expected: `test result: ok. 28 passed`; then `cargo test -p flux-core --lib`: `291 passed`; clippy clean.

- [ ] **Step 5: Prove the tests can fail** (restore after each):
  - in `file`, skip the `file_names_fit` check: `a_target_name_too_long...` FAILED (a lock is created);
  - in `file`, take the lock before `prepare_file` (move the B1 block after `open_operation`, adapting nothing else):
    `a_mistyped_source_creates_nothing` FAILED;
  - in `file`, compute `ended` without the `RefusedUnchanged` arm: `a_single_file_refusal_under_the_lock...` FAILED;
  - in `FilePlace::sweep`, skip the removal (push the id as gone): `restart_supersedes_a_single_files_prior...`
    FAILED.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-core/src/copy.rs crates/flux-core/src/run
git commit -m "feat(core): the single-file run - B1 checks first, the record beside the target, the same finish (cut 7a Part 3b-1)"
```

---

### Task 10: Model conformance - five labels gain their items

**Files:**
- Modify: `models/lockproto/impl-map.toml`

- [ ] **Step 1: Replace the five `part-3` entries.** In `impl-map.toml`, replace

```toml
[S99_write]
status = "part-3"
reason = "the engine's mutation after its still_owned check (copy_file and copy_tree hooks)"
```

  with

```toml
[S99_write]
item = "crates/flux-core/src/copy.rs::copy_file_guarded"
```

  and replace the four `S21_1_*` entries with `status = "part-3"` (`S21_1_s3`, `S21_1_s5_write_begin`,
  `S21_1_s5_write_end`, `S21_1_s3_refuse_close`) with:

```toml
[S21_1_s3]
item = "crates/flux-core/src/run/restart.rs::supersede"
[S21_1_s5_write_begin]
item = "crates/flux-core/src/run/session.rs::open_operation"
[S21_1_s5_write_end]
item = "crates/flux-core/src/run/session.rs::open_operation"
[S21_1_s3_refuse_close]
item = "crates/flux-core/src/run/session.rs::open_operation"
```

  Each named function already mentions its label as a whole word: `copy_file_guarded`'s doc comment and its publish
  comment (`S99_write`); `supersede`'s doc comment (`S21_1_s3`); `open_operation`'s doc comment (`S21_1_s5_write_begin`,
  `S21_1_s5_write_end`, `S21_1_s3_refuse_close`).

- [ ] **Step 2: Run the conformance test**

Run: `cargo test --test model_impl_map`
Expected: `test result: ok.` (every test in the file passes). A label whose function does not mention it fails with
`fn <name> in <file> never mentions the label`; fix the doc comment, never the map.

- [ ] **Step 3: Prove the check bites.** Temporarily change `S21_1_s3`'s item to
  `"crates/flux-core/src/run/session.rs::give_back"` (a function that never mentions it): the test FAILS with
  `never mentions the label`. Restore.

- [ ] **Step 4: Commit**

```bash
git add models/lockproto/impl-map.toml
git commit -m "test(model): the run's labels map to their Rust functions (cut 7a Part 3b-1)"
```

---

### Task 11: The spec says what the code now does

**Files:**
- Modify: `docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md` ("The run" step 7, "`--restart` and
  `--break-lock`", "What each crash window leaves", "Declared refinements of the spec")

- [ ] **Step 1: "The run", step 1.** Replace `1. **Capability.** Classify the destination filesystem.` with
  `1. **Capability.** Classify the filesystem of the directory that holds the lock - DEST's parent, or DEST for a root
  (refinement 14).`

- [ ] **Step 2: "The run", step 7, the success path.** Replace the `- Success:` bullet and its six numbered steps with:

```markdown
   - Success - the copy's walk finished, even with per-entry failures (refinement 12):
     1. state `COMPLETED` (durable), after `still_owned`;
     2. remove the run's own temporaries: the copy removes a failed file's temporary at once, and that removal is this
        step (refinement 19);
     3. ONLY IF no temporary was left: rewrite the held record in place with `workspace_path` = `none` (refinement
        17), then remove the state - for a tree, the workspace retired to `operations/<id>.removing`, then its
        `manifest` and a crash's `manifest.tmp`, then the directory (refinement 9); for a single file, the adjacent
        record and its `.tmp`. If a temporary was left, the state stays `COMPLETED` as the only record naming it (the
        id that finds it, refinement 7), and steps 3-4 are skipped (panel round 5);
     4. `rmdir` `operations/` and `.flux/` if they are empty (a failure because they are not empty is not an error);
     5. `still_owned` (`S99_release_check`);
     6. unlink the lock by name, then close the handle, which releases the OS-native lock (`S99_release`).
```

  Replace the `- Failure: state \`FAILED\`, release the lock the same way, exit 1.` sentence's opening
  `- Failure:` with `- Failure - an abort of the copy, a single-file copy error, or a failure of the run's own steps
  (refinement 12):`, and add after that bullet:

```markdown
   - A refusal of the copy that changed nothing (`SAFETY_REJECTED`, `NOREPLACE_PUBLISH_UNAVAILABLE`): the run removes
     what it created - the record rewritten to `none` first, then the state, the control directories it emptied, and
     a DEST it made - and releases the lock, so the refusal stays exit 3 (refinement 16, §55). A removal that fails
     is exit 1, naming the path.
```

- [ ] **Step 3: "`--restart` and `--break-lock`".** Replace the numbered steps 1-5 under **`--restart`** and the
  paragraph after them (`A crash in 3-5 leaves ...`) with:

```markdown
**`--restart`** (spec:1646-1668), holding the lock throughout. This operation's state and record come FIRST
(refinement 15): `still_owned` needs a record to compare, and none exists before step 5 (F5).
1. The prior operation's lock was already acquired or recovered at step 3, and step 5 created this operation's state
   (CREATED) and wrote the record naming it.
2. For each resumable prior operation: `still_owned` (§99; `S21_1_s3`), then durably set it `ABANDONED` with
   `superseded_by` = this operation.
3. Delete their partials. 7a's state does not list partials (their records are `state.db`'s, cut 8), so they are
   found by the prior operation's EXACT id: every `<name>.flux-partial.<prior-id>` under DEST (a walk that skips only
   the three reserved subdirectories `DEST/.flux/operations/`, `standalone/` and `atomic/`, because user files may
   legally live elsewhere under `DEST/.flux/`), deleted through handles opened from DEST's one component at a time.
   The walk costs one traversal of the WHOLE existing DEST - which can far exceed the copy itself when DEST is large
   and the source small - and runs only on an explicit `--restart`. 7b's `roots` and cut 8's recorded partials are
   what bound it later. Beside a single-file target the partial is `target.flux-partial.<prior-id>`, looked up by the
   target's own name (refinement 13). Authority comes from the valid prior state naming that id, never from the name
   pattern alone (spec:9369-9371 forbids that only when the record is missing or corrupt), and `still_owned` runs
   before EACH deletion (spec:9365-9367). A partial that cannot be deleted keeps its prior's ABANDONED state as the
   record naming it, reported as a warning.
4. Remove each prior's state whose partials are all gone (a workspace retired first, refinement 9), `still_owned`
   before each.
5. Continue with step 5's last write: TRANSFERRING.

A crash during 2-4 leaves this operation's CREATED state and its record, now a dead owner's, and each prior ABANDONED
or still resumable: the next run recovers the lock (§240.3), gets `RESUMABLE_OPERATION_EXISTS` for this operation's
state, and a new `--restart` supersedes everything. `--restart` never overrides a live owner, or missing or corrupt
state (spec:1664-1666).
```

- [ ] **Step 4: "What each crash window leaves".** Replace the row
  `| during \`--restart\` after ABANDONED | an ABANDONED prior | proceeds |` with:

```markdown
| during `--restart`'s supersede | this operation's CREATED state and its record (a dead owner's), and each prior ABANDONED or still resumable | recovers the lock (§240.3), then `RESUMABLE_OPERATION_EXISTS` for this operation's state; a new `--restart` supersedes it and the rest |
```

  and replace the row `| after COMPLETED | a completed state or its leftover | proceeds |` with:

```markdown
| after COMPLETED, before the record is rewritten | a dead owner's record naming the COMPLETED state | recovers the lock and proceeds past the COMPLETED state |
| during the record's in-place rewrite (refinement 17) | a torn record | `TARGET_LOCK_UNCERTAIN`; `--restart --break-lock` clears it |
| after the record names `none` | a dead owner's record naming no workspace, and the COMPLETED state whole, retired (`<id>.removing`) or gone | recovers the lock and proceeds; a `.removing` workspace is cut 9's |
| during a refused copy's rollback (refinement 16) | as the three rows above, with this operation's state CREATED or TRANSFERRING before the rewrite | before the rewrite: recovers the lock, then `RESUMABLE_OPERATION_EXISTS` (cleared by `--restart`); after it: proceeds |
```

- [ ] **Step 5: "Declared refinements of the spec".** Append after refinement 11:

```markdown
12. **A copy whose walk finished is COMPLETED** (Part 3b, A1), even with per-entry failures (exit 1 stays the copy's).
    FAILED is for an abort, a single-file copy error, or a failure of the run's own steps. Otherwise every repeat
    copy into a non-empty DEST - collisions are per-entry failures - would need `--restart`.
13. **A single file's records are found by name lookup** (Part 3b, E3): for each `*.flux-state.<id>` beside the
    target, `<target>.flux-state.<id>` is looked up by the target's own name, and the filesystem's equivalence (case,
    normalization) decides. Exact bytes would miss a record on NTFS or APFS for a target spelled in another case;
    folding in Flux would claim another file's record on Linux.
14. **The source side comes first, and the capability is the lock's** (Part 3b, B1). The source checks, DEST's
    resolution through its parent (a symlink at DEST is refused `SAFETY_REJECTED`) and the identity pre-flight run
    before step 1, so a mistyped source creates nothing. Step 1 classifies the filesystem of the directory that holds
    the lock.
15. **`--restart` writes this operation's state and record before it supersedes** (Part 3b, Q-H (a)). `still_owned`
    compares the record, and F5 writes none before step 5; the model's `Recover` and `TakeOver` also write it before
    `S21_1_s3`. A crash mid-restart then leaves a dead owner's record and a resumable state, not an empty lock.
16. **A refused copy that changed nothing is rolled back** (Part 3b, Q-I): the run removes what it created and keeps
    the copy's exit 3 (§55: created and removed again does not count).
17. **The record stops naming the state before the state goes** (Part 3b, Q-K): the held record is rewritten in place
    with `workspace_path` = `none` before the finish or a rollback removes this operation's state. Otherwise a crash
    between the state's removal and the lock's unlink leaves a dead owner's record naming missing state,
    `ARTIFACT_OWNERSHIP_UNCERTAIN`, which no flag clears. A crash during the rewrite leaves a torn record,
    `TARGET_LOCK_UNCERTAIN`, cleared by `--restart --break-lock`.
18. **A takeover that must start again keeps its state** (Part 3b, D1): when the in-place overwrite finds the lock path
    moved, the run obtains the lock again (at most `MAX_ATTEMPTS` times) and reuses the state it already created.
19. **The copy's immediate removal of a failed file's temporary is finish step 2** (Part 3b, G2): the finish does not
    retry it, and a temporary that stayed keeps the COMPLETED state as its record.
```

- [ ] **Step 6: Check the spec's own gates**

Run: `just typos` (the `typos` step of `just check`; justfile `:33-34`, `:46`).
Expected: no finding. (The full gate runs in Task 12.)

- [ ] **Step 7: Commit**

```bash
git add docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md
git commit -m "docs(spec): refinements 12-19 and the amended crash table for the run (cut 7a Part 3b-1)"
```

---

### Task 12: Verify, push, CI

- [ ] **Step 1: The gates, in order**

Run: `just check`
Expected: `Summary [...] 526 tests run: 526 passed, 3 skipped` (479 at `ce9cc98`, plus 46 library tests and 1
integration test).

Run: `just check-linux`
Expected: `Summary [...] 512 tests run: 512 passed, 3 skipped` (465 + 47). If it says the WSL distribution is not
available while `wsl -l -v` lists it, start it once (`wsl -d Ubuntu-26.04 -e true`) and re-run (the logged anomaly).

Run: `just check-mac`
Expected: a clean `Finished`.

- [ ] **Step 2: The CLI is unchanged.** `cargo test -p flux-cli` passes exactly as at `ce9cc98` (the CLI still calls
  `copy_tree` / `copy_file`; Part 3b-2 switches it to `run::tree` / `run::file`).

- [ ] **Step 3: Push and watch CI**

```bash
git push origin spec/cut-7a
```

Then `gh run list --branch spec/cut-7a --limit 1` and `gh run watch <id>` until all jobs finish; expected: every job
green, including the macOS test job (the real filesystem name-equivalence test, Task 5, runs on APFS there).

- [ ] **Step 4: Record** the commits and the gate figures in the execution ledger (memory), then the AGY-CAPSTONE over
  the range `<plan commit>..HEAD` (code and tests, excluding `docs/` and `Cargo.lock`).

---

## What Part 3b-2 builds on (the contract this part leaves)

- `flux_core::run::{tree, file, RunConfig, Run, RunError, RunStep, RunWarning}`:
  - `tree(fs, src_root, dst_root, opts, cfg, on_report) -> Run<Result<TreeOutcome, TreeAbort>>`;
  - `file(fs, src, dst, opts, cfg) -> Run<Result<Outcome, CopyError>>`;
  - `opts.operation_id` is replaced by `cfg.operation_id`.
- The CLI builds `RunConfig` with two `flux_core::ids::new_id()` (the operation and this process) and
  `flux_platform::boot_session_id()`, and refuses `--break-lock` without `--restart` (usage, exit 2).
- Exit status: a `stop` of `RunError::Refused { changed: false, .. }` is 3; any other `stop` is 1; with no `stop`, the
  copy's own rule (`exit_code::for_tree` / `for_file`) decides, which may now use `TreeAbort::refused_unchanged`.
  `warnings` never change it.
- Rendering: a refusal prints `CODE: detail` with the holder's `owner_instance_id`, `boot_session_id`,
  `last_heartbeat_wall_time` and `workspace_path` where `refusal.holder` is `Some` (§96.2), and the `not_removed` path;
  a `Failed` prints its code, `step.as_str()`, the path and the error; each warning is one stderr line.
- Still to do in 3b-2: the flags, the exit codes, the rendering, the debug-build crash hook (F1), the end-to-end tests
  of the spec's Testing item 4, and `TODO.md` ("What this closes").

## Self-review (done before the panel)

- **Spec coverage.** "The run" steps 1-7: Tasks 7 and 9 (step 2: Task 4's `check_control_plane`; step 6's guard:
  Tasks 2-3; the reserved paths: Task 3). §21.1 classification: Part 3a, plus E3 (Task 5). `--restart`: Task 8.
  `--break-lock`'s state order and takeover key: Task 7. The crash table and refinements: Task 11. Model conformance:
  Task 10. The CLI, its flags, exit codes and end-to-end tests: Part 3b-2 by decision 11.
- **Known untested here, on purpose:** a filesystem root as DEST (the fake's `/` has no identity; the path is two
  calls through code the other tests run); a refusal at step 5's `create` after DEST was made (only a race reaches
  it); the D1 budget's exhaustion (`MAX_ATTEMPTS` moves in a row). The test audit may scope them.
- **Types.** `Guard<'g>`, `Shared<'c, F>`, `Source<'a, F>`, `Locked<'a, D>`, `Fault`, `Place<D>`, `TreePlace<'p, F>`,
  `FilePlace<'p, D>` are each defined once, in the task that first uses them; every later reference matches.

## Stand-downs

- **Panel (agy, 2 rounds): GREEN at round 2.** Briefs `.clavity/seams/cut7a-p3b1-plan-panel-r1.md` and `-r2.md`, replies
  `.clavity/scratch/cut7a-p3b1-plan-panel/r1-reply.md` and `r2-reply.md`. Folded: decision 9 states that a rollback
  removes an empty `.flux` as the finish does (round 1, open question). Earlier, while planning: the finish-order crash
  window (Q-K) and `--restart`'s revalidation before any record (Q-H), each an owner ruling after an AGY-FIRST consult.
- REJECTED (round 1): "`scan_file` compares the entry's name byte for byte" - the quoted line is in neither the plan
  nor the code; Task 5's `scan_file` reads only the trailing id (`id_after`) and looks the record up by the target's
  own name. The reviewer withdrew it after reading Task 5.
- REJECTED (round 2): "a rolled-back refusal exits 0" - the exit rule (this plan, "What Part 3b-2 builds on") gives a
  run with no `stop` the copy's own rule, and `exit_code::for_tree` (`crates/flux-cli/src/exit_code.rs:14-16`) gives
  an unchanged refusal exit 3. The reviewer withdrew it after reading those lines.
