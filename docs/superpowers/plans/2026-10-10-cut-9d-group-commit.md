# Cut 9d: group commit of Strict tree publications - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Under `--durability=strict`, a tree copy stages the files of a directory and publishes them in groups: one `prepare_many` transaction for the notes, the renames, one `apply_recovery` transaction for the claims. Syncs per file fall from 3.0 to about 1.0; cut 9c's recovery semantics are unchanged.

**Architecture:**
- `copy_file_guarded` is split into `stage_file` (steps 1-7, ends with the closed temporary) and `publish_staged` (final heartbeat/guard, rename); every caller but the Strict tree still calls the pair back to back.
- `Frame::Live` gains a `Batch` of `Pending` entries. `copy_one` stages into it; `flush_batch` publishes it. Policy (`BatchPolicy`) rides in `Shared`, so tests inject thresholds and a clock.
- One new store method, `ClaimStore::prepare_many`; the commit side reuses `apply_recovery`.
- Rollout: Tasks 3-4 build the batch with `BatchPolicy::DEFAULT.files = 1` (batching OFF), so every existing test stays green while the new machinery is tested at the tree level with explicit policies. Task 5 flips the default to 64 and re-derives the existing tests in the same commit.

**Tech Stack:** Rust 2024 (MSRV 1.98.1), `redb` 4.3, the `FaultFs` fake, nextest through `just`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-09-cut-9d-group-commit-design.md` (approved by the owner 2026-10-10; panel GREEN at `d7015fa`). Executors read it with this plan. Where the two differ the spec wins and the executor reports the conflict, except in the four numbered "Plan rulings" below, which are decisions the spec leaves open.

## Global Constraints

- The gate is `just check` = `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `typos` + `cargo nextest run --workspace --no-tests=pass` + `cargo test --doc --workspace`. Run it before every commit; use `cargo nextest run --workspace --no-fail-fast` when classifying failures (fail-fast truncates the list). Push after every task and read CI on that commit (Windows-only failures surface only there).
- Batching applies ONLY to a tree publication under `Durability::Strict` into a store with `supports_prepared()` (spec decision 2), with `BatchPolicy.files > 1`. Normal, format-1 stores, single files and lockless copies are byte-for-byte the cut 9c/9a paths.
- Constants (spec decision 5): `BATCH_FILES = 64`, `BATCH_BYTES = 64 MiB` (`64 << 20`), `BATCH_AGE = 1 s`. No CLI flag.
- Each file's note is durable BEFORE its rename (section 164). A lost lock writes and removes NOTHING (spec decision 8).
- `before_mutation` runs from the run's `guard` closure (`crates/flux-core/src/run/mod.rs:268-273`, `:430-435`): the guard count of a Strict tree changes (spec decision 11); Task 5 recomputes each stall index and never loosens an assertion.
- Do not add `unwrap` on store or filesystem results in non-test code; errors keep their `Code`.

## Plan rulings (open in the spec, decided here)

1. **"Batching off" seam.** `BatchPolicy.files == 1` makes `copy_one` take the unbatched cut 9c path (per-file `prepare`/`commit_prepared`). The spec's equivalence test ("`BATCH_FILES = 1`: N per-file `prepare` calls and no `prepare_many`") needs exactly this. A real one-file batch (a directory with one file, policy 64) uses `prepare_many` with one note.
2. **Leftovers after a lost lock.** `CopyError.leftover` holds one path. The failing entry k's leftover travels in the returned error (as in an unbatched copy); the temporaries of entries k+1.. are each reported through `on_report` as `TreeFailureCause::Copy` with a `TargetLockBusy` error carrying that temporary as `leftover` (path relative to DEST). The run's report closure already collects such leftovers (`run/mod.rs:289-296`).
3. **Fault key names.** The fake's new call/fault key is `claim_prepare_many` (the spec says `prepare_many`; every existing key carries the `claim_` prefix). The call string is `claim_prepare_many(<n>)`, `n` = number of notes.
4. **Test seams instead of a 64 MiB source.** The `BATCH_BYTES` trigger and the age trigger are tested with an injected `BatchPolicy` (small `bytes`, fake `now`), not with a sparse 64 MiB file and a real second. One test pins `BatchPolicy::DEFAULT` to 64 / `64 << 20` / 1 s. The spec's "strace-free proxy: transaction count for 1000 small files on the real filesystem" is realised at the store (Task 1: a counting redb `StorageBackend` shows `prepare_many` of 64 notes costs the same backend syncs as one `prepare`) plus the counting fake (Task 5); the real-filesystem sync count is the owner-gated `strace -f -c` measurement (Task 8).

## Review Focus

Failure modes the spec implies that no single mechanism task owns; each has a test in the named task.

1. A directory of exactly 64 and of 65 files (batches 64 / 64+1; the 64th file flushes before the 65th is staged). Task 3.
2. A subdirectory sorted between two files of one directory (`a0`, `m/`, `z`): the child's `DirEnd` flush runs while the parent's batch is pending; counts and the final tree are unchanged. Task 3.
3. A stage failure mid-batch (disk full at entry 30 of 40): the 29 pending entries are still published and noted; only entry 30 is reported. Task 4.
4. Two sources whose names fold (`File` / `file`) in one directory of a case-folding destination: the same outcome as an unbatched copy (one published, one `DestinationNamespaceCollision`). Task 3.
5. A kill in the middle of a batch's renames, then `--resume` on the real binary: renamed files are recovered, unrenamed ones redone, the tree equals the source. Task 6.

---

### Task 1: `ClaimStore::prepare_many`

**Files:**
- Modify: `crates/flux-fs/src/claims.rs` (trait at `:190-225`; conformance `run_prepared_all` at `:272-285`)
- Modify: `crates/flux-platform/src/claims.rs` (`impl ClaimStore for RedbClaimStore` at `:197`; tests module at `:~415`)
- Modify: `crates/flux-core/src/fault_fs.rs` (`impl ClaimStore for FakeClaimStore` at `:246`; tests at `:~2822`)

**Interfaces:**
- Consumes: `ClaimStore::{prepare, apply_recovery}` semantics (note exists at key => `Code::IoError`; Immediate commit; all-or-nothing).
- Produces: `fn prepare_many(&mut self, notes: &[(ClaimKey, PreparedRecord)]) -> Result<()>` on the `ClaimStore` trait: ONE Immediate transaction inserting every note; a key that already has a note (in the store OR earlier in `notes`) is `Code::IoError` and NOTHING changes; an empty slice is `Ok(())` and commits nothing; format-1 store: `Code::IncompatibleState` (the `require_prepared` path). Conformance case `prepare_many_inserts_all_or_none`. Fake call string `claim_prepare_many(<n>)`, fault key `claim_prepare_many`.

- [ ] **Step 1: Write the failing tests.**
  - flux-fs conformance: add `pub fn prepare_many_inserts_all_or_none<S: ClaimStore>(mut s: S)` and call it from `run_prepared_all`. Asserts: three notes via `prepare_many` -> `s.prepared()` lists exactly those three; a second `prepare_many` containing one fresh key and one already-noted key returns `Err` with `code == Code::IoError` and `prepared()` still lists only the original three (the fresh key was NOT added); a slice with the same key twice is `IoError` and adds nothing; an empty slice is `Ok` and changes nothing.
  - flux-platform (`tests` module): `prepare_many_costs_one_commit`. Wrap a `std::fs::File`-less in-memory `redb::StorageBackend` that counts `sync_data` calls (`redb::Builder::new().create_with_backend(backend)`, `redb-4.3.0/src/db.rs:2255`; the trait is at `db.rs:74`). Build the `RedbClaimStore { db, durability: Durability::Strict, unsynced: 0, format: FORMAT }` directly (same module, private fields) after creating the three tables in one write transaction. Assert: the sync delta of `prepare_many` with 64 notes equals the sync delta of one `prepare`, and is `> 0`.
  - fault_fs tests: `prepare_many_records_its_call_honours_a_fault_and_is_all_or_nothing` mirroring `prepare_records_its_call_and_honours_a_fault` (`fault_fs.rs:2822`): the call string is `claim_prepare_many(2)` for two notes; `fs.fail("claim_prepare_many", Code::IoError)` makes the call fail with nothing added; a duplicate key adds nothing.

- [ ] **Step 2: Run them to see them fail.**
  Run: `cargo nextest run -p flux-fs -p flux-platform -p flux-core prepare_many`
  Expected: compile error "no method named `prepare_many`".

- [ ] **Step 3: Implement.**
  - Trait: add the method with the doc above, next to `prepare`.
  - `RedbClaimStore`: `self.require_prepared()?;` then `self.immediate(|tx| { open `PREPARED`; for each (k, r): error `IoError` if the key is present (use the same message as `prepare`), else insert })`. An empty slice returns `Ok(())` before opening a transaction. Duplicates inside the slice are caught because the second `get` sees the first insert inside the same transaction.
  - `FakeClaimStore`: `self.record(format!("claim_prepare_many({})", notes.len()), "claim_prepare_many")?; self.require_prepared()?;` then work on a clone of the prepared map and swap it in only on success (the pattern of `apply`, `fault_fs.rs:175-186`). The empty slice still records the call and returns `Ok`.

- [ ] **Step 4: Run to verify.**
  Run: `cargo nextest run -p flux-fs -p flux-platform -p flux-core prepare_many` then `just check`.
  Expected: PASS. Mutant check (do and revert): make the redb impl commit per note with `self.immediate` in a loop; `prepare_many_costs_one_commit` must go red and nothing else.

- [ ] **Step 5: Commit** `feat: ClaimStore::prepare_many (one Immediate transaction for a batch of notes)`.

---

### Task 2: Split `copy_file_guarded` into `stage_file` and `publish_staged`

**Files:**
- Modify: `crates/flux-core/src/copy.rs` (`discard` at `:206`; `copy_file_guarded` at `:492-708`; tests module at `:710`)

**Interfaces:**
- Consumes: nothing new.
- Produces (all `pub(crate)` in `copy.rs`):
  - `pub(crate) struct Staged { pub temp: OsString, pub identity: FileIdentity, pub bytes_copied: u64, pub metadata_failures: Vec<MetadataFailure>, pub identity_degraded: Option<FileIdentity> }`
  - `pub(crate) enum Stage { Skipped(Outcome), Staged(Staged) }` (`Skipped` is step 2b's skip; the tree never receives it because it passes `ExistingPolicy::Overwrite`)
  - `pub(crate) fn stage_file<F: DestinationRoot>(fs: &F, src: &Path, parent: &F::Dir, name: &OsStr, opts: &CopyOptions, guard: &Guard<'_>, beat: &Heartbeat<'_>, before_create: &BeforeCreate<'_>) -> Result<Stage, CopyError>`: current steps 1-7 up to and including the source recheck and reading `writer.identity()` (`copy.rs:661`); the writer is dropped on return (no descriptor held); on every error path the temporary is already discarded exactly as today.
  - `pub(crate) fn publish_staged<D: DirHandle>(parent: &D, name: &OsStr, staged: Staged, publish: Publish, guard: &Guard<'_>, beat: &Heartbeat<'_>) -> Result<Outcome, CopyError>`: the final `beat()` then `guard()` then the rename (`rename_replace` for `Publish::Replace`, `rename_no_replace` for `NoReplace`), with the exact error handling of `copy.rs:676-697` (heartbeat failure -> `discard(.., CopyStep::Heartbeat ..)`; guard failure -> `CopyError { leftover: Some((temp, "kept: this run no longer holds the destination's lock")), step: Publish, .. }`; rename failure -> `discard(.., CopyStep::Publish ..)`); builds the same `Outcome`.
  - `discard` becomes `pub(crate)` (Task 4 calls it for staged temporaries).
  - `copy_file_guarded` keeps its name, nine parameters and behaviour: it calls `stage_file`, returns the `Skipped` outcome unchanged, runs `before_publish(&PublishIntent { temp, identity })` between the halves (same position, same discard-on-error code as `copy.rs:662-672`), then `publish_staged`.

- [ ] **Step 1: Write the failing tests** in `copy.rs`'s `tests` module (FaultFs, `opts()` as the other tests):
  - `stage_file_leaves_a_closed_temporary_and_no_target`: after `stage_file` the call log has `create_new(/dst.flux-partial.op1)` and no `rename_`; `/dst.flux-partial.op1` holds the bytes; `/dst` does not exist; the returned `Staged.bytes_copied == 5`.
  - `publish_staged_renames_and_reports_the_outcome`: stage then `publish_staged(.., Publish::NoReplace, ..)`: `/dst` has the bytes, the temporary is gone, `Outcome.published_identity == staged.identity`, call log shows `rename_no_replace`.
  - `publish_staged_with_a_lost_guard_keeps_the_temporary_as_leftover`: a guard that fails -> `Err` with `step == CopyStep::Publish`, `code() == Code::TargetLockBusy`, `leftover` is `/dst.flux-partial.op1`'s name, and the temporary still exists (nothing removed).
  - Existing tests are the oracle for the unchanged behaviour: all `copy.rs` tests, and `the_note_precedes_the_publish_heartbeat_and_guard` (`run/tests.rs:4302`) must stay green untouched.

- [ ] **Step 2: Run to see them fail** (`stage_file` not found): `cargo nextest run -p flux-core stage_file publish_staged`.

- [ ] **Step 3: Implement.** Cut the existing function at the line `let published_identity = writer.identity()...` (`copy.rs:661`): everything before it becomes `stage_file` (return `Stage::Staged` there instead of continuing; the early `Ok(Outcome { skipped: true, .. })` of step 2b becomes `Stage::Skipped`); everything after the hook becomes `publish_staged`. No behaviour change: this is a move, so do not reword error messages.

- [ ] **Step 4: Verify.** `just check`; then `cargo nextest run -p flux-core --no-fail-fast` must show the same pass count as before the task plus the three new tests. Mutant (do and revert): in `copy_file_guarded` call `before_publish` AFTER `publish_staged`'s beat/guard is impossible by construction, so instead drop the `before_publish` call; `the_note_precedes_the_publish_heartbeat_and_guard` and the cut 9c Task 5 tests must go red.

- [ ] **Step 5: Commit** `refactor: split copy_file_guarded into stage_file and publish_staged`.

---

### Task 3: The batch (staging, triggers, happy-path flush), default OFF

**Files:**
- Modify: `crates/flux-core/src/tree.rs`: `Shared` (`:316-329`), `run_tree` (`:381`), `copy_tree_at` (`:441-453`), `walk_into` (`:506-545`), `enter_dir` (`:731`), `copy_one` (`:781`), tests module (`:1380`; the four existing `Shared { .. }` literals at `:2272`, `:2346`, `:2383`, `:2410`).
- Modify: `crates/flux-core/src/run/mod.rs:278` (`Shared` literal) and `crates/flux-core/src/run/tests.rs:2286` (`Shared` literal): add `batch: BatchPolicy::DEFAULT` (mechanical).

**Interfaces:**
- Consumes: Task 1 `prepare_many`; Task 2 `stage_file`, `publish_staged`, `Stage`, `Staged`.
- Produces (private to `tree.rs` unless marked):
  - `pub(crate) struct BatchPolicy { pub files: usize, pub bytes: u64, pub age: std::time::Duration, pub now: fn() -> std::time::Instant }` (`Clone, Copy`); `BatchPolicy::DEFAULT` with `bytes: BATCH_BYTES`, `age: BATCH_AGE`, `now: std::time::Instant::now` and `files: DEFAULT_FILES` where `const DEFAULT_FILES: usize = 1;` with the comment "Task 5 sets this to BATCH_FILES". Constants `BATCH_FILES: usize = 64`, `BATCH_BYTES: u64 = 64 << 20`, `BATCH_AGE: Duration = Duration::from_secs(1)`.
  - `Shared` gains `pub(crate) batch: BatchPolicy`.
  - `struct Pending { path: PathBuf, name: OsString, key: ClaimKey, planned: Option<ClaimKey>, template: PreparedRecord, staged: Staged, target: FluxPathKey, replaced: Option<(OsString, Metadata)> }` (`replaced` = `Plan::Replace`'s `stored` and `meta`; `planned` = the second claim key of a case-variant replacement, as `tree.rs:1079`).
  - `#[derive(Default)] struct Batch { pending: Vec<Pending>, bytes: u64, since: Option<Instant>, folded: HashSet<Vec<u8>> }`; `Frame::Live` gains `batch: Batch` (constructed `Batch::default()` in `copy_tree_at` and `enter_dir`).
  - `fn flush_batch<F: DestinationRoot>(cx: &Shared<'_, F>, parent: &F::Dir, names: &mut NameIndex, batch: &mut Batch, out: &mut TreeOutcome, on_report: &mut dyn FnMut(TreeFailure)) -> Result<(), CopyError>`; `Err` only for a stop (Task 4 defines the stops; in this task any `Err` from `publish_staged` that `finish_copy` returns is propagated after the rest of the algorithm below).
  - `fn drain_batches<F: DestinationRoot>(cx, stack: &mut [Frame<F::Dir>], out, on_report, walk_failed: bool) -> Result<(), CopyError>`: flushes every `Live` frame's batch from the top of the stack down WITHOUT popping (the root frame must survive for `copy_tree_at`'s return); when `walk_failed` or an earlier stop exists, a later stop is not returned but reported (Plan ruling 2's reporting, path = the leftover's path or empty). `copy_tree_at` calls it after `walk_into` returns, whatever it returned, and the walk's own error wins: `walked.and(drained)`.

**Behaviour of the batched path in `copy_one`** (the algorithm the signatures do not determine). Batched iff `strict` (`tree.rs:961`) && `cx.batch.files > 1` && claims and `claim_parent` are `Some`; otherwise the existing code is untouched.
1. Before name resolution: if the batch is non-empty and (`batch.folded` contains the ASCII-lowercased `name` bytes, or `batch.since` is `Some(t)` with `(cx.batch.now)() - t >= cx.batch.age`), call `flush_batch` first (`?`).
2. Resolve the plan exactly as today (`New` / `Replace` / skip / collision paths all unchanged).
2b. (Task 3 amendment, spec decision 6) If the batch is non-empty and `parent.metadata(&flux_fs::temp_path(Path::new(name), &cx.opts.operation_id))` succeeds (anything at the temporary's name: on a folding destination that is a pending sibling's temporary, which `stage_file`'s step-1 sweep would delete), flush first. Test seam: `BatchPolicy` gains `fold_precheck: bool` (true by default); a test sets it false so the fake's ASCII fold pair reaches the probe instead of the ASCII pre-check. Test `a_fold_equal_pending_temporary_is_never_swept`: with `fold_precheck: false`, case-insensitive fake, sources `File` and `file` in one directory: `File`'s published bytes are `File`'s source bytes, `file` is reported `DestinationNamespaceCollision` (as the unbatched run reports it), and the call log shows `File`'s rename BEFORE `file`'s `create_new`. Mutant: drop the probe -> this test red (the wrong bytes appear under `File`).
3. If the batch is non-empty and `cx.fs.metadata(&src)?.len >= cx.batch.bytes` (a failed stat is reported like `copy_one`'s other source errors), flush first. (The stat is only paid when something is pending.)
4. Stage with `stage_file(cx.fs, &src, parent, name, &opts, cx.guard, cx.beat, &before_create)` where `opts` is `existing: Overwrite`, `publish: NoReplace` (New) or `Replace` (Replace), and `before_create` is `&no_before_create` (New) or the existing `Existing`-claim closure (Replace, `tree.rs:1017-1035`).
   - `Err(e)` with `e.step == CopyStep::Create` and `e.cause.source.kind() == ErrorKind::AlreadyExists` while the batch is non-empty: `flush_batch`, then stage ONCE more (spec decision 6 backstop). Any other `Err`, or the second failure: `finish_copy(path, Err(e), ..)?` (no note exists yet, nothing else to undo).
   - `Ok(Stage::Staged(s))`: push `Pending`; `batch.bytes += s.bytes_copied`; `batch.since.get_or_insert((cx.batch.now)())`; insert the lowercased name bytes into `batch.folded`. `Ok(Stage::Skipped(_))` is unreachable (assert).
5. After pushing: flush if `pending.len() >= cx.batch.files` or `batch.bytes >= cx.batch.bytes`.

**`flush_batch` happy path** (spec decision 4). Take the entries out of `batch` and reset it. Empty: return `Ok`. Then: `(cx.beat)()` and `(cx.guard)()` (map errors as `walk_into` does at `tree.rs:537-538`); `prepare_many` with one `(key, template + temp_name + identity_text(staged.identity))` per entry (build exactly as `write_note`, `tree.rs:1125-1129`); for each entry in order `publish_staged(parent, &name, staged, publish, cx.guard, cx.beat)`; on `Ok(o)` immediately `finish_copy(path, Ok(o), ..)` (counts `files_copied`, `bytes_copied`, degraded, complaints), `out.files_overwritten += 1` for a Replace, `names.record_publication(&target, stored, &name, replaced_id, identity)` (types as at `tree.rs:1010` / `:1100`: `stored: Option<&OsStr>` is `None` for New and `Some(stored.as_os_str())` for Replace; `replaced_id: Option<ObjectId>`) as `tree.rs:1010` / `:1100`, and push `RecoveryOp::Commit { key, target, planned }`; on a non-stop `Err` push `RecoveryOp::Discard { key }` and `finish_copy(path, Err(e), ..)?`. After the loop call `claims.borrow_mut().apply_recovery(&ops)` once unless `ops` is empty; its error is reported `ClaimNotRecorded` per renamed entry (Task 4 adds the per-entry fallback).
`walk_into`'s `DirEnd` arm becomes: pop the frame; if `Live`, `flush_batch` with the popped frame's `dir`, `names` and `batch`; then the existing `claim_parent: Some(_)` beat/guard/`claims.flush()` block; then the handle drops. The `File` arm passes `&mut batch` to `copy_one`.

- [ ] **Step 1: Write the failing tests** in `tree.rs`'s `tests` module, using a helper `batched(fs: &FaultFs, policy: BatchPolicy, durability: Durability) -> (Result<(), CopyError>, TreeOutcome, Vec<TreeFailure>)` that builds `Shared` with a claim store from `root.create_claim_store(OsStr::new("state.db"), durability)` (pattern: `run/tests.rs:2262-2299`) and `copy_tree_at`. Policies: `BatchPolicy { files: 4, ..BatchPolicy::DEFAULT }` unless stated. Counting uses `fs.calls()` prefixes `claim_prepare_many(`, `claim_apply_recovery`, `claim_prepare(`, `claim_commit_prepared(`.
  - `n_files_in_one_directory_make_ceil_n_over_batch_calls`: 10 files, `files: 4`: 3 `claim_prepare_many(` and 3 `claim_apply_recovery`, zero `claim_prepare(` and zero `claim_commit_prepared(`; every file published with its claim `Created`; `fs.prepared_count() == 0`; `out.files_copied == 10`.
  - `the_boundary_of_a_batch`: with `files: 4`, 4 files -> exactly 1 `claim_prepare_many(4)`; 5 files -> `claim_prepare_many(4)` then `claim_prepare_many(1)`; and the 4th file's rename precedes the 5th file's `create_new` (call order). (Review Focus 1.)
  - `normal_durability_never_batches`: `Durability::Normal`, `files: 4`: no `claim_prepare_many`, no `claim_apply_recovery`.
  - `a_format_1_store_never_batches`: `fs.set_claim_store_format` to 1 (as `run/tests.rs:4102`) under Strict: no `claim_prepare_many(`.
  - `files_one_is_the_unbatched_path`: Strict, `files: 1`, 3 files: 3 `claim_prepare(`, 3 `claim_commit_prepared(`, no `claim_prepare_many(`.
  - `bytes_trigger`: `BatchPolicy { files: 64, bytes: 10, .. }`, files of 6 bytes: after the 2nd file (12 >= 10) a flush happens before the 3rd is staged.
  - `a_large_file_flushes_the_pending_small_ones_first_and_is_staged_alone`: `bytes: 10`, order `a` (2 bytes), `b` (20 bytes), `c` (2 bytes): `a`'s rename precedes `b`'s `create_new`; `b` is flushed immediately after it is staged (its own `claim_prepare_many(1)`), `c` is separate.
  - `a_directory_end_flushes_the_child_batch_before_the_parents` (Review Focus 2): sources `a0`, `m/x`, `z` with `files: 64`: call order is `rename(m/x)` ... `claim_apply_recovery` (m's) before `rename(a0)`; the parent's `claim_prepare_many(2)` holds `a0` and `z`; the final tree has all three.
  - `the_age_trigger_uses_the_injected_clock`: a `thread_local!` clock behind `fn fake_now() -> Instant` (base `Instant::now()` plus a settable offset); with `age = 1 s`, stage 2 files at t=0, set t=2 s, the 3rd file triggers a flush BEFORE it is staged (2 renames precede the 3rd `create_new`).
  - `the_walk_end_flushes_the_root_batch`: 3 files in the root, `files: 64`: all published after `copy_tree_at` returns `Ok`, one `claim_prepare_many(3)`.
  - `names_that_fold_in_one_directory_flush_before_the_second_is_staged` (Review Focus 4): `fs.set_case_insensitive(true)`, sources `File` and `file` in one directory (the source fake must hold both; if the fake folds the source too, put them in two source directories merged by... use the fake's existing case-fold fixture at `run/tests.rs:2312` for the shape), `files: 64`: the first is renamed before the second's `create_new`; the second is reported `DestinationNamespaceCollision` exactly as the unbatched run reports it (assert the same code and step as a `files: 1` run of the same fixture).
  - `an_exclusive_create_collision_with_pending_entries_flushes_and_retries_once`: the backstop for a fold the ASCII pre-check misses. Two files `a`, `b`, `files: 64`; `fs.fail_nth("create_new", 2, Code::IoError, ErrorKind::AlreadyExists)` makes `b`'s first exclusive create fail. Assert: `a` is renamed (flush) BEFORE `b`'s second `create_new`; `b`'s second attempt succeeds and `b` is published; `create_new` was called exactly 3 times in all. A second test with both of `b`'s creates failing `AlreadyExists` asserts exactly 2 attempts for `b` (one retry only) and that `b` is reported once, `IoError` at `CopyStep::Create` (the unbatched classification; `finish_copy` maps only Gate and Publish).
  - `equivalence_of_batched_and_unbatched`: the same 130-file, two-directory tree copied with `files: 64` and with `files: 1` (two fresh fakes): `fs.read_file` of every destination file equal, `fs.claim(..)` of every claim equal, `out` counters equal (`files_copied`, `bytes_copied`, `directories_created`), `got` reports equal; the call logs DIFFER as designed (batched: `claim_prepare_many(` present and `claim_prepare(` absent; unbatched: the reverse).
  - `batch_policy_default_matches_the_spec`: `BATCH_FILES == 64`, `BATCH_BYTES == 64 << 20`, `BATCH_AGE == Duration::from_secs(1)`, `BatchPolicy::DEFAULT.bytes`/`age` equal them. (The `files` field is pinned in Task 5.)

- [ ] **Step 2: Run to see them fail.** `cargo nextest run -p flux-core --lib tree::tests::` -> compile errors (`BatchPolicy`, `Shared.batch`).

- [ ] **Step 3: Implement** as specified above. Keep `copy_one`'s unbatched branches byte-for-byte; put the batched branch behind the gate so the diff to the old path is the gate plus the new branch.

- [ ] **Step 4: Verify.** `just check` green with the default OFF (every existing test unchanged). Mutants (do and revert), each must turn exactly the named test red: drop the `files >= cx.batch.files` flush -> `the_boundary_of_a_batch`; skip the fold check -> `names_that_fold_in_one_directory...`; flush the parent at a child's `DirEnd` too early or never at walk end -> `the_walk_end_flushes_the_root_batch`; count at the end of the flush instead of at each rename is Task 4's test.

- [ ] **Step 5: Commit** `feat: stage Strict tree files into a per-directory batch and flush it with prepare_many + apply_recovery (off by default)`.

---

### Task 4: Failure isolation, stops, abort drain

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`flush_batch`, `drain_batches`, `walk_into`, `copy_tree_at`; tests module)

**Interfaces:**
- Consumes: Task 3's `Pending`, `Batch`, `flush_batch`, `drain_batches`, `Shared.batch`; `copy::discard` (`pub(crate)` from Task 2); Task 1's `prepare_many`.
- Produces: no new names; completes the semantics of `flush_batch` / `drain_batches`.

**Semantics to implement** (spec decisions 7 and 8; the order below is the order in `flush_batch`):
1. **Step-1 failures.** `(cx.beat)()` fails (lock held): remove each entry's temporary with `copy::discard(parent, &temp, CopyStep::Heartbeat, ..)` (guarded; a removal that fails reports its leftover via `on_report`), no notes exist yet, return `Err(CopyError::at(CopyStep::Heartbeat, e))`. `(cx.guard)()` fails (lock lost): nothing is written or removed; entry 0's leftover rides in the returned error (`step: Publish`, `TargetLockBusy`, `leftover: Some((temp relative to DEST, "kept: this run no longer holds the destination's lock"))`), entries 1.. are reported via `on_report` (Plan ruling 2).
2. **`prepare_many` fails** (nothing was written): retry each entry through `prepare(&key, &record)`; an entry whose own `prepare` fails has its temporary removed (`copy::discard(.., CopyStep::Claim, ..)`) and is reported through `finish_copy(path, Err(..))` (a `CopyStep::Claim` failure, as `write_note` fails today); the others continue.
3. **Per-entry publish loop.** On `Ok`: count and `record_publication` at once (Task 3). On `Err(e)`:
   - lost lock (`e.code() == Code::TargetLockBusy`, i.e. `publish_staged`'s guard failure): STOP-LOST. Write and remove nothing more. Entries k+1.. are reported as leftovers (Plan ruling 2). Renamed entries keep their notes (no `apply_recovery`). Return `finish_copy(path, Err(e), ..)`'s `Err`.
   - heartbeat failure (`e.step == CopyStep::Heartbeat`) or `primitive_unavailable` at publish (both make `finish_copy` return `Err`): STOP-HELD. This entry's temporary is already discarded by `publish_staged`; push `Discard` for its key; for entries k+1.. remove the temporary (`copy::discard`, guarded) and push `Discard`; ORDER MATTERS: remove every temporary BEFORE the `apply_recovery` that discards the notes (a crash in between then leaves notes with no temporary, which recovery classifies GONE and discards; the reverse order could orphan temporaries with no note); apply all ops in one `apply_recovery` (best effort, errors ignored: recovery decides at `--resume`); return the `Err`.
   - any other failure (`AlreadyExists` -> collision, etc.): push `Discard { key }`, `finish_copy` reports it, continue.
4. **Settle.** One `apply_recovery(&ops)` (skipped when `ops` is empty). On `Err`: nothing changed; retry per op: `commit_prepared(&key, &target, planned.as_ref())` for a `Commit` (its failure is reported `ClaimNotRecorded(e)` at the entry's path, the note stays), `discard_prepared(&key)` for a `Discard` (best effort).
5. **Drain.** `copy_tree_at` already calls `drain_batches` (Task 3); this task verifies by test that every abort path reaches it: an error returned by `walk_into`'s `?` (a lost lock at a `Dir` event, at a `DirEnd`, inside `copy_one`) still flushes both frames' batches.
6a. (Task 3 review ruling) In `walk_into`'s `Dir` arm, before `enter_dir`: when the top frame is `Live`, its batch is non-empty and `batch.folded` contains the ASCII-lowercased directory name, `flush_batch` the top frame first (a file `M` pending while directory `m/` is entered would otherwise let the directory win). Test `a_directory_whose_name_folds_to_a_pending_file_flushes_first`: case-insensitive fake, source file `M` then directory `m/` with a file: the batched run's reports (code and step) equal the unbatched run's.
6. `flush_batch` returns `Err` only for STOP-LOST / STOP-HELD / step-1 failures. Per-entry failures go through `on_report`.

- [ ] **Step 1: Write the failing tests** (tree level, helper `batched` of Task 3; guard/beat closures as `failing_after` at `tree.rs:2292`):
  - `a_failing_prepare_many_falls_back_to_per_entry_prepare`: `fs.fail("claim_prepare_many", Code::IoError)` (one shot), 3 files, `files: 4`: calls show `claim_prepare_many(3)` then three `claim_prepare(`; all three files published; claims `Created`; nothing reported; `prepared_count() == 0`.
  - `one_entry_own_prepare_failing_fails_only_that_file`: `fail_always("claim_prepare_many")` plus `fs.fail_nth("claim_prepare", 2, ..)`: file 2 is reported (`TreeFailureCause::Copy`, `step == CopyStep::Claim`), its temporary is gone, no note for it; files 1 and 3 published and claimed; `files_copied == 2`.
  - `a_failing_apply_recovery_falls_back_to_per_entry_commits`: `fs.fail("claim_apply_recovery", Code::IoError)` one shot: three `claim_commit_prepared(` follow; all claims `Created`; nothing reported.
  - `one_entry_own_commit_failing_is_claim_not_recorded`: `fail_always("claim_apply_recovery")` + `fail_nth("claim_commit_prepared", 2, ..)`: file 2 is published (read its bytes) and reported `ClaimNotRecorded` at its path, its note remains (`prepared_count() == 1`), the others are claimed; no file is both published and reported failed apart from that `ClaimNotRecorded`.
  - `a_failed_rename_discards_its_note_inside_the_one_apply_recovery`: `fail_nth("rename_no_replace", 2, Code::IoError, ErrorKind::Other)` (entry 2 of 3): exactly one `claim_apply_recovery` call carrying both the two commits and the discard (assert `count == 1` and no `claim_discard_prepared(` calls), entry 2 reported at `CopyStep::Publish`, its temporary removed, `prepared_count() == 0`.
  - `the_rename_collision_is_a_namespace_collision`: a destination file appears between staging and flush (`fs.write_file` in an `on_nth("claim_prepare_many", 1, ..)` hook): that entry is reported `DestinationNamespaceCollision`, the others publish.
  - `a_lost_lock_at_entry_k_writes_and_removes_nothing` (spec "Tests"): 4 files in one batch, a guard that fails from its (stage-guards + 1 + 2)-th call, i.e. at entry 2's publish guard: `TreeAbort`-equivalent assertions on the returned `CopyError` (`TargetLockBusy`, `leftover.is_some()`) and on `out`: `files_copied == 2`; entries 0-1 renamed with their notes still present (`prepared_count() == 4`), no `claim_apply_recovery` and no `claim_discard_prepared(` call after the loss; entries 2-3 keep their temporaries (`fs.exists`) and are each named as a leftover (entry 2 in the error, entry 3 in `got`); nothing removed after the loss (assert the call log has no `remove_file` after the failing guard). Then build `TreeAbort { error, outcome: out }` and assert `changed() == true` and `refused_unchanged() == false` (the reason the count is taken at the rename).
  - `a_lost_lock_at_the_step_one_guard_leaves_every_temporary`: the guard fails at the flush's first guard: no `claim_prepare_many` call, every temporary present, all entries named as leftovers, `files_copied == 0`, `changed() == true` because of the leftover.
  - `a_heartbeat_failure_with_the_lock_held_commits_the_renamed_and_discards_the_rest`: beat fails at entry 2's publish heartbeat: entries 0-1 committed (claims `Created`), entry 2 and 3 temporaries removed and notes discarded (`prepared_count() == 0`), returns `Err` with `step == CopyStep::Heartbeat`.
  - `a_stage_failure_mid_batch_does_not_lose_the_pending_entries` (Review Focus 3): 40 files, `files: 64`, `fs.fail_nth("create_new", 30, Code::DiskFull, ErrorKind::Other)`: file 30 is reported once; the other 39 are published and claimed after the directory ends.
  - `an_abort_drains_every_frame`: a nested tree where the guard fails at a `DirEnd` flush of the child while the parent holds pending entries: both frames' staged temporaries are accounted for (child's per the lost-lock rule, parent's reported as leftovers via the drain), the walk's error is the one returned, and no pending entry is silently dropped (assert every staged temporary either was renamed or is named in `got`/the error).
  - `the_count_is_taken_at_the_rename`: a guard that fails at entry 2 of 4: `out.files_copied == 2` (mutant: moving the count to the end of the flush turns it red).

- [ ] **Step 2: Run to see them fail** (`cargo nextest run -p flux-core --lib tree::tests:: --no-fail-fast`): the fallback and stop semantics are absent.

- [ ] **Step 3: Implement** the six items above. Reuse `finish_copy` for every report and mapping so the classification (`TargetLockBusy`, heartbeat, `primitive_unavailable`, collision) stays in one place.

- [ ] **Step 4: Verify.** `just check` (still default OFF, existing tests unchanged). Mutants (do and revert), each red in exactly the named test: remove the Discard op of a failed rename -> `a_failed_rename_discards...`; call `apply_recovery` after STOP-LOST -> `a_lost_lock_at_entry_k...`; skip `drain_batches` in `copy_tree_at` -> `an_abort_drains_every_frame`.

- [ ] **Step 5: Commit** `feat: batch failure isolation (per-entry fallbacks), lost-lock and heartbeat stops, abort drain`.

---

### Task 5: Turn batching on; re-derive every existing test that depended on the old order

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`DEFAULT_FILES = BATCH_FILES`; add the `files` assertion to `batch_policy_default_matches_the_spec`)
- Modify: `crates/flux-core/src/run/tests.rs` (the cut 9c Task 5/6 tests listed below)
- Modify: `crates/flux-cli/tests/recovery.rs`, `crates/flux-cli/tests/run.rs` (stall indices)
- Add: run-level counting tests in `crates/flux-core/src/run/tests.rs`

**Interfaces:** none new. The oracle for every re-derived test is spec decision 11 and decisions 4, 7, 8 plus the guard-count derivation below; **no assertion is loosened**: each test keeps its intent (what it proves) and changes only the call names, indices or counts that the design changes. If a test cannot keep its intent, STOP and report `STATE_MISMATCH: <test> <why>`.

**Behaviour changes the tests now encode (flag these in the commit body):**
- Under Strict a parent directory's files are renamed AFTER its subdirectories' files (the child's `DirEnd` flush runs first). Counts and final trees are unchanged.
- A lost lock no longer discards the notes (spec decision 8); the old `a_lost_lock_after_the_note_discards_it_and_keeps_the_temporary` asserted the opposite and is rewritten.

**Guard-count derivation** (the stall hook counts `guard()` calls, `run/mod.rs:268-273`). Per directory of 100 new files under Strict with 64-file batches: 1 (the directory's create) + 64x2 (sweep, create) + 1 (flush step 1) + 64 (renames) + 36x2 + 1 (DirEnd flush step 1) + 36 + 1 (`claims.flush()` guard) = **304**. Normal keeps 1 + 100x3 + 1 = 302 per directory.

| Where | Old | New | Why |
|---|---|---|---|
| `recovery.rs` `AT_D1_CREATE` (used by `killed_strict`, i.e. Strict) | 303 | **305** | d0 = 304 guards, d1's create is the next |
| `recovery.rs` `AT_D1_F000_PUBLISH` for Strict | 306 | **435** | 305 + 128 + 1 (flush step 1) + 1 (f000's publish guard); held with 64 temporaries and 64 notes, none renamed |
| `recovery.rs` `a_normal_copy_never_writes_a_note` (Normal) | 306 | 306 | keep a separate constant `AT_D1_F000_PUBLISH_NORMAL = 306`; Normal does not batch |
| `run.rs` claim-count test, run 1 (Normal) | 303 | 303 | unchanged |
| `run.rs` claim-count test, resume run `--durability strict` | 606 | **610** | d0 = 2 (create, DirEnd flush guard; its files are skipped), d1 = 304, d2 = 304 |

The doc comments above `AT_D1_*` and above the claim-count test are rewritten with these derivations. The e2e assertions that counted notes change with them: `prepared_count` after the Strict publish-guard stall is **64** (not 1); `a_renamed_publication_is_recovered_and_reported` renames `f000`'s temporary by hand and still expects `recovered 1 interrupted publications` (the other 63 notes are NOT RENAMED, discarded and redone; `recover.rs:275` increments `recovered` only for RENAMED verdicts, verified). Its `files_skipped >= 101` assertion stays.

**Existing tests predicted to change (the run is the authority: after flipping the default run `cargo nextest run --workspace --no-fail-fast`, classify EVERY red test; a red test not in this list or the table above is reported as `STATE_MISMATCH` before touching it).** All in `run/tests.rs` unless stated:
- `a_strict_publication_prepares_before_the_rename_and_commits_after_it` (`:4058`): `claim_prepare(a)`/`claim_commit_prepared(a)` -> `claim_prepare_many(` before the renames of its directory and `claim_apply_recovery` after; `sub` flushes first (its `DirEnd`), then the root at walk end.
- `a_prepare_failure_fails_the_file_before_the_rename` (`:4118`): to fail file `a`'s note use `fail_always("claim_prepare_many")` plus `fs.fail("claim_prepare", ..)` (the fallback's first per-entry prepare); same expectations.
- `a_commit_failure_keeps_the_note_and_reports_claim_not_recorded` (`:4146`): `fail_always("claim_apply_recovery")` plus `fs.fail("claim_commit_prepared", ..)`.
- `a_publish_failure_after_the_note_discards_it` (`:4164`): `a`'s rename is now the 4th `rename_no_replace` (probe, workspace publish, `sub/b`, then `a` at the root's walk-end flush); the discard now appears as part of `claim_apply_recovery`, not `claim_discard_prepared(a)`.
- `a_lost_lock_after_the_note_discards_it_and_keeps_the_temporary` (`:4187`): rewritten as `a_lost_lock_after_the_notes_keeps_them_and_the_temporaries`: notes stay (`prepared_count() == 1` or the batch size), no `claim_discard_prepared(`, temporaries kept, never unlinked lock.
- `a_strict_replacement_commits_the_planned_claim_too` (`:4207`): `claim_prepare(A)` -> `claim_prepare_many(`, `claim_commit_prepared(A)` -> `claim_apply_recovery`; the planned claim assertions are unchanged.
- `a_strict_note_carries_the_publications_fields` (`:4246`): the hooks read the notes before `claim_apply_recovery` calls; the order of `notes` becomes `sub/b` first, then `A`.
- `the_note_precedes_the_publish_heartbeat_and_guard` (`:4302`): the window between `claim_prepare_many(` and the first rename holds the heartbeat then the guard.
- `recovery_row_4_gone_discards` (`:4614`), `a_failed_recovery_transaction_is_retried_by_the_next_resume` (`:5176`): the `claim_apply_recovery` counts include the walk's batch flushes; assert the recovery's own call(s) by position (those before the first `rename_no_replace`), not by total.
- `a_crashed_case_variant_replacement_is_recovered_with_both_claims` (`:5016`): leave the note by failing the batch commit AND its fallback (`fail_always("claim_apply_recovery")` + `fail_always("claim_commit_prepared")`).
- Not changed (verify green untouched): `a_format_1_store_under_strict_writes_no_note`, `a_normal_publication_writes_no_note`, `a_single_file_copy_under_strict_writes_no_note_and_touches_no_claim_store`, `crates/flux-core/tests/replace_std_fs.rs` (`:119`: the cut-short run now panics at the `sub` directory's guard instead of `a`'s publish guard, with `a` staged and its `Existing` claim recorded; its assertions hold, update its comment).

- [ ] **Step 1: Write the new failing run-level tests** in `run/tests.rs` (default policy, so >64 files needed): `a_strict_tree_of_130_files_makes_three_batches`: `/src/d/f000..f129`, Strict through `run_tree_with`: `count(&c, "claim_prepare_many(") == 3` with sizes 64, 64, 2 (assert the three call strings), `count(&c, "claim_apply_recovery") == 3`, zero `claim_prepare(` and `claim_commit_prepared(`, every claim `Created`, `prepared_count() == 0`, workspace removed. `a_normal_tree_of_130_files_makes_no_batch_calls` (Normal: none of the new calls). Also pin `BatchPolicy::DEFAULT.files == BATCH_FILES`.

- [ ] **Step 2: Flip the default** (`DEFAULT_FILES = BATCH_FILES`) and run `cargo nextest run --workspace --no-fail-fast`; record the red list; it must match the lists above.

- [ ] **Step 3: Re-derive each red test** per the rules above; update the `AT_*` constants and their comments from the table; update `Stalled` call sites that used a shared constant for Normal and Strict.

- [ ] **Step 4: Verify.** `just check` green. For each changed stall index, the existing assertion that the stall "landed" must hold (`prepared_count == 64`, `d1/f000` temporary present and not published, `claim_count == 300` at 610): these are the measurement that the derivation above is right; if one fails, recompute from the observed announcement and correct the comment, do not adjust the assertion.

- [ ] **Step 5: Commit** `feat: group commit of Strict tree publications is on (64 files / 64 MiB / 1 s)`; push; read CI.

---

### Task 6: Real-filesystem tests

**Files:**
- Modify: `tests/integration/mod.rs`, `tests/integration/support/mod.rs` (`copy_tree` takes durability through a new `copy_tree_with(src, dst, durability)`; `copy_tree` calls it with `Normal`)
- Modify: `crates/flux-cli/tests/recovery.rs`

**Interfaces:** Consumes the whole feature. Produces tests only.

- [ ] **Step 1: Write the tests.**
  - `tests/integration`: `strict_tree_of_many_files_arrives_byte_for_byte`: 3 directories x 70 files (so each splits 64 + 6) plus a zero-byte file and a 300 KiB file per directory, copied with `Durability::Strict`; `tree_bytes(dst) == tree_bytes(src)`; no `.flux` workspace and no `*.flux-partial.*` anywhere under `dst`; `out.files_copied` equals the file count.
  - `crates/flux-cli/tests/recovery.rs`: `a_strict_copy_killed_between_renames_resumes_to_the_same_tree` (Review Focus 5): kill at stall index `AT_D1_F000_PUBLISH + 10` (= 445; the stall precedes `f010`'s publish guard, so `f000..f009` are renamed and the other 54 are still staged, each with its note). Assert it landed: `prepared_count == 64` and exactly ten of `f000..f009` exist under their final names. Then `--resume --durability strict`: exit 0, stderr contains `recovered 10 interrupted publications` (`run/recover.rs:275` counts only RENAMED verdicts, so the 54 redone files are not in the count), `same_bytes(src, dst)`, the workspace is removed.
  - Same file: `a_strict_copy_killed_after_a_batch_commit_resumes` (the commit-done row): stall at `AT_D1_F000_PUBLISH + 64` (= 499, `f064`'s sweep guard, after `f063`'s rename and after the batch's `apply_recovery`); assert `prepared_count == 0` and `claim_count == 100 + 64`; the resume completes with `same_bytes`. A stall between the last rename and the commit is not reachable (`apply_recovery` has no guard before it, the reason cut 9c has no kill-the-resume test either); that row is covered by the fault-injected Task 4 tests.

- [ ] **Step 2: Run** `cargo nextest run -p flux --test integration` and `cargo nextest run -p flux-cli --test recovery`; expected FAIL before any needed helper exists, PASS after.

- [ ] **Step 3: Implement** the helper change only; no production code.

- [ ] **Step 4: Verify** `just check`. Mutant (do and revert): in `flush_batch` rename before `prepare_many` (swap the order) -> the kill-between-renames test must go red (a renamed file without a note makes `prepared_count`/`recovered` disagree). If it does not, the test is vacuous: strengthen it before committing.

- [ ] **Step 5: Commit** `test: real-filesystem coverage of group commit (byte-for-byte, kill between renames)`.

---

### Task 7: Documentation and records

**Files:**
- Modify: `TODO.md` (add "Cut 9d known limits" after "Cut 9c debt", `:511`; and a "Cut 9d debt" list)
- Modify: `docs/superpowers/specs/2026-10-09-cut-9d-group-commit-design.md` (Status line: approved, plan path; record the four Plan rulings as a "Plan rulings" paragraph)
- Modify: the V16 spec amendment appendix only if cut 9c's amendments mention per-file note commits (`rg -n "prepared" FLUX_FULL_UPDATED_SPEC_V16.md` and amend 182.1/182.2 wording to "per batch"); otherwise leave it.

- [ ] **Step 1:** Write the six known limits of the spec ("Known limits" 1-6) into `TODO.md` verbatim in substance, plus debt: directory handles kept across `DirEnd` (limit 1 fix), folding the `Existing` claim into `prepare_many` (limit 6 fix), the source-recheck window (a staged file is rechecked at staging, not at its rename: a source changed after the recheck is published as the snapshot read, as an unbatched copy already allows between recheck and rename, now up to 64 files / 1 s wide).
- [ ] **Step 2:** `typos` and `just check`; commit `docs: cut 9d known limits and plan rulings`.

---

### Task 8: Measurement (owner-gated; NOT delegated, NOT run by a subagent)

The owner approves each timed run; the main thread runs it with no concurrent tool calls (timing discipline in the global instructions): kill stale measurements first, background the run, make no tool call until it completes, two passes, quote the range, state what was not controlled.

- [ ] **Step 1: Build both binaries.** `target/release/flux` at this branch's head, and the pre-9d binary from `main` at `49fabc4` (a detached `git worktree` with its own `CARGO_TARGET_DIR`), so (ii) and (iii) have a baseline: only the flat 5000x4 KiB, mixed and large cases were measured before 9d.
- [ ] **Step 2: Protocol** (`baseline.py` in the session scratchpad, `benches/publish/cases.py` shapes, cache dropped, 2 passes x 3 runs): flat 5000x4 KiB, mixed 500, large 1 GiB, each Normal and Strict; nested 500 dirs x 10 x 4 KiB and 5000 dirs x 1 x 4 KiB (new shapes, both binaries); `strace -f -c -e trace=fsync,fdatasync` sync counts per file; peak RSS (`/usr/bin/time -v`); `cargo bench -p flux --bench copy` `small_1000x4kib/strict` (20 directories x 50 files, so batches of 50).
- [ ] **Step 3: Acceptance** (spec "Measurement"): flat Strict small <= 1.15 syncs per file and >= 40% faster wall time; nested 500x10 <= 1.3 syncs per file; Normal unchanged within the baseline spread; Strict large and mixed not worse than the baseline spread; peak RSS within 2 MiB of 19 MiB; 5000x1 reported, not gated. Write the before/after table into the PR description; if a gate fails, report it, do not tune the thresholds.

---

## Self-review

**Spec coverage.** Decision 1 -> Task 2. Decision 2 -> Task 3 gate + tests (`normal_durability_never_batches`, `a_format_1_store...`). Decision 3 -> Task 3 types. Decision 4 (1-6) -> Task 3 flush + Task 4 (count at the rename test). Decision 5 (a-f) -> Task 3 triggers + tests; 5(e) abort drain -> Task 4. Decision 6 -> Task 3 (fold + backstop tests). Decision 7 -> Task 4 fallbacks. Decision 8 -> Task 4 stops. Decision 9 -> no code; Task 6 e2e. Decision 10 -> Task 8 RSS. Decision 11 -> Task 5 table and list. Interfaces added -> Task 1, 2, 3. Tests section -> Tasks 1, 3, 4, 5, 6 (deviations are Plan rulings 3-4). Measurement -> Task 8. Known limits -> Task 7. Out of scope: nothing planned.

**Spec silences closed here.** The off seam (ruling 1), multiple leftovers after a lost lock (ruling 2), the fake's key name (ruling 3), test seams (ruling 4), the extra `metadata` stat for the large-file trigger (Task 3 step 3, paid only when something is pending), the root frame surviving the drain (Task 3), and the baseline for the nested/one-per-directory shapes (Task 8 step 1).

**Behaviour changes called out for the owner.** (1) A lost lock now leaves notes (it used to discard them, `run/tests.rs:4187`), per spec decision 8. (2) Under Strict, a parent directory's files publish after its subdirectories' files. (3) The source recheck sits up to 64 files / 1 s before the rename.

**Unverified by construction.** The stall indices 305 / 435 / 610 are derived from the guard-count arithmetic above and are checked by the landing assertions in Task 5 step 4; the list of existing red tests is predicted from reading, and the nextest run is the authority.
