# Cut 11b: `flux copy --dry-run` with `--resume`, `--restart` and `--break-lock` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A dry run combined with `--resume`, `--restart` or `--restart --break-lock` previews that invocation at a destination holding prior operation state, changing nothing and taking no lock; plain `--dry-run` with a resumable prior predicts the real run's refusal.

**Architecture:** Cut 11a's dry mode (one engine path, `dry_tree` / `dry_file` in `crates/flux-core/src/run/mod.rs`) gains a read-only lock-record preview, the prior-classification arms of `open_operation` re-expressed without a lock, a resume preview that runs the live adoption code on a COPY of the prior's claim store in a private temporary directory, and a restart preview over a read-only extraction of the sweep's walk. New engine code lives in a new module `crates/flux-core/src/run/dry_prior.rs`; the CLI renders new stream words, notes and `unprobed` keys.

**Tech Stack:** Rust (workspace toolchain), redb 4.3.0 through `flux-platform`'s `RedbClaimStore`, the `FaultFs` fake, nextest, `just check`.

**Spec:** `docs/superpowers/specs/2026-10-17-cut-11b-dry-run-prior-design.md` (r6, owner-approved, panel GREEN at `5af2c5a`). Sections cited below as "spec 3.2" etc.

## Global Constraints

- Gate: `just check` (fmt, clippy with `-D warnings`, typos, nextest) passes at every commit; the expected pass line at the start is `1643 tests run: 1643 passed, 6 skipped` (it grows with each task).
- Invariants I1-I7 of cut 11a and I8-I11 of the spec: no lock call of any kind (`open_lock`, `create_lock`, `try_lock`), no write under DEST or beside it; the ONE write a dry run makes is the copy in `<temp>` (spec I11). The fake's `forbidden` helpers (`crates/flux-core/src/run/tests.rs:7744-7786`, `crates/flux-core/src/tree.rs:7188`) are the proof and are rescoped in Task 5, not weakened.
- No new platform surface (spec D7): the copy uses `DirHandle::open_read` (`crates/flux-fs/src/fs.rs:452`), `create_new` (`:315`, `fn create_new(&self, name: &std::ffi::OsStr) -> Result<Self::Writer>`), `create_dir(name) -> Self` (`:307`), `open_claim_store` (`:413`), `remove_file`, `remove_dir`.
- No probe of the on-disk store (spec D7/P2); no `set_quick_repair`; no `tempfile` promotion: `<temp>` is made and removed through `DirHandle` calls so the fake models it (Task 6).
- Exact strings: every note, stream word and summary line is given in the spec (3.2, 3.4, 3.5, 3.6) and copied verbatim here; a test pins each.
- Commit trailer `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`; never commit `.clavity/` or `.superpowers/`; seat-id tokens like `AB-1` do not pass `typos`: write `finding 1`.
- Every `RunConfig` literal in the repository changes in Task 1 (a new field); the compiler is the oracle for the list, which Task 1's FILES clause pre-lists.

## Review Focus

1. A readable lock record whose owner is alive: the dual-outcome note prints, `owner_liveness` is in `unprobed`, the exit code reflects the unheld case (0 when clean) and no `open_lock`/`try_lock` appears in the fake's log (Task 2 test `dry_a_recorded_lock_prints_the_dual_outcome_and_previews_the_dead_case`).
2. Plain `--dry-run` with a resumable prior now exits 3 with `RESUMABLE_OPERATION_EXISTS` where cut 11a printed a note and the plan (Task 3 test `dry_a_plain_run_with_a_resumable_prior_predicts_the_refusal`; the two 11a tests at `run/tests.rs:7882-7954` change expectation).
3. A crashed prior with prepared notes: the dry resume's `files_resumed`, `would recover` count and per-entry actions equal the live resume's on an identical copy, and DEST is byte-identical after the dry run (Task 9 test `a_dry_resume_on_a_crashed_prior_matches_the_live_walk`).
4. The copy's temporary directory is gone after every return path, including a torn copy and a predicted refusal inside adoption (Task 6 test `dry_adopt_removes_its_temporary_directory_on_every_path`).
5. `--dry-run --restart --break-lock` against a torn record previews the takeover and changes nothing; the same without `--break-lock` predicts `TARGET_LOCK_UNCERTAIN` (Task 2 test `dry_an_uncertain_record_is_refused_or_taken_over_by_mode`; Task 8 CLI test 136).

---

### Task 1: The planned-entry words, the dry-run report fields and the temp root

**Files:**
- Modify: `crates/flux-core/src/dry_run.rs:8-79` (the enums, `as_str`, `DryRun`), `:166-178` (the words test)
- Modify: `crates/flux-core/src/run/mod.rs:56-80` (`RunConfig`), `:230-237` (`RunWarning` dry variants)
- Modify (every `RunConfig { .. }` literal, the compiler names them): `crates/flux-core/src/run/tests.rs:26-40`, `crates/flux-core/tests/replace_std_fs.rs`, `crates/flux-core/tests/safety_std_fs.rs`, `crates/flux-core/tests/dry_run_std_fs.rs`, `crates/flux-core/tests/dry_run_memory.rs`, `crates/flux-cli/src/main.rs:192-209`, `benches/copy.rs`, `tests/integration/support/mod.rs`
- Test: `crates/flux-core/src/dry_run.rs` (`mod tests`)

**Interfaces:**
- Produces: `PlannedObject::{File, Dir, Symlink, State, Partial}` with `as_str` `"state"`, `"partial"`; `PlannedAction::{CreateDir, Copy, Overwrite, Skip(SkipReason), CreateSymlink, Resume, Discard}` with `"would_resume"`, `"would_discard"`; in `dry_run.rs`: `pub enum DryPrior { StartedNew, Resume { operation_id: String, claims: Option<u64>, recovered: u64, strays: u64 }, Restart { superseded: Vec<(String, OpState)>, partials: u64, states: u64 } }` (`OpState` from `crate::state`); `DryRun { unprobed_noreplace: bool, prior_seen: bool, planned: Option<PlannedEntry>, owner_liveness: bool, lock_takeover: bool, prior: Option<DryPrior>, partials_present: u64 }`; `RunConfig.dry_temp_root: Option<PathBuf>` (doc: "cut 11b: where a dry resume copies the prior's claim store; `None` means `std::env::temp_dir()`; tests pass a fake directory"); `RunWarning::DryRunLockRecord { operation_id: String, owner_instance_id: String, boot_session_id: String, heartbeat_age_secs: Option<u64>, workspace_path: String }` (`None` = in the future), `RunWarning::DryRunLockTakeover { why: String }`, `RunWarning::DryRunBrokenLeftover { name: String }`, `RunWarning::DryRunDebris { name: String }`, `RunWarning::DryRunStoreFresh { operation_id: String }`, `RunWarning::DryRunStoreTorn`.
- The `DryRunPrior` doc comment at `run/mod.rs:230-231` is reworded: "a dry run found a resumable prior operation at DEST (cut 11b previews what the invocation does with it; this note is printed by a plain dry run before its predicted refusal)".

- [ ] **Step 1: Extend the words test**

In `every_planned_action_and_object_has_its_stream_word` add: `assert_eq!(PlannedAction::Resume.as_str(), "would_resume"); assert_eq!(PlannedAction::Discard.as_str(), "would_discard"); assert_eq!(PlannedObject::State.as_str(), "state"); assert_eq!(PlannedObject::Partial.as_str(), "partial");`

- [ ] **Step 2: Run it to see it fail to compile** — Run: `cargo nextest run -p flux-core -E 'test(every_planned_action_and_object_has_its_stream_word)'`; Expected: a compile error naming `Resume`.

- [ ] **Step 3: Add the variants, `DryPrior`, the `DryRun` fields, the `RunConfig` field and the warnings** as in Interfaces; set `owner_liveness: false, lock_takeover: false, prior: None, partials_present: 0` at the two `DryRun { .. }` literals (`run/mod.rs:1153`, `:1195`) and `dry_temp_root: None` in every `RunConfig` literal the compiler names (the CLI literal at `main.rs:192-209` gets `Some(std::env::temp_dir())`).

- [ ] **Step 4: Gate** — Run: `just check`; Expected: rc 0, `1643 passed`.

- [ ] **Step 5: Commit** — `git add` the files above; message `feat(core): cut 11b, the resume and discard words, DryPrior, the dry-run report fields and the dry temp root`.

### Task 2: The lock-record preview (spec 3.2)

**Files:**
- Modify: `crates/flux-core/src/lock/obtain.rs:55-135` (extract the refusal constructors; `obtain` keeps its behaviour)
- Create: `crates/flux-core/src/run/dry_prior.rs`; register `mod dry_prior;` in `crates/flux-core/src/run/mod.rs` beside `mod place;`
- Test: `crates/flux-core/src/run/tests.rs` (new tests after `a_dry_run_with_the_lock_held_by_another_still_plans`, `:7956`)

**Interfaces:**
- Consumes: `LockSite` (`lock/site.rs:38`; `dir()`, `lock_name()`, `kind()`, `workspace(&record)` at `:95-142`), `decode` and `RECORD_LEN` (`lock/record.rs:6, 84`), `Decoded`, `Uncertain` (`:27-45`), `LockCapability::allows_exclusive` (`crates/flux-fs/src/lock.rs:21`), `refuse` (`lock/error.rs:71`), `UNTRUSTED_DETAIL` (`obtain.rs:55`), `DirHandle::read_file(name, limit)` and `read_dir`.
- Produces in `obtain.rs`: `pub(crate) fn busy_refusal(holder: Option<LockRecord>) -> LockError`, `pub(crate) fn foreign_refusal() -> LockError`, `pub(crate) fn untrusted_refusal(record: LockRecord) -> LockError`, `pub(crate) fn orphan_refusal(record: LockRecord, kind: SiteKind) -> LockError`, `pub(crate) fn uncertain_refusal(why: Uncertain) -> LockError`, each returning exactly what the arms at `obtain.rs:77-118` return today, and `obtain` calling them.
- Produces in `dry_prior.rs`: `pub(crate) enum LockPreview { Absent, Refused(LockError), Takeover(Uncertain), Recorded(LockRecord), Error(FsError) }` and `pub(crate) fn dry_lock<D: DirHandle>(site: &LockSite<'_, D>, break_lock: bool, capability: LockCapability, now_ns: u64, warnings: &mut Vec<RunWarning>) -> LockPreview` implementing the table of spec 3.2: `read_file(lock_name, RECORD_LEN)`; `NotFound` -> `Absent`; `SafetyRejected`/`DestinationError` -> `Refused(foreign_refusal())`; other errors -> `Error(e)`; `Decoded::Foreign` -> `Refused(foreign_refusal())`; `Decoded::Uncertain(why)` -> `Refused(uncertain_refusal(why))` unless `break_lock`, then `capability.allows_exclusive()` decides: false -> `Refused(uncertain_refusal(why))`, true -> `Takeover(why)` with `warnings.push(DryRunLockTakeover { why: format!("{why:?}") })`; `Decoded::Record(r)` -> `site.workspace(&r)`: `Untrusted` -> `Refused(untrusted_refusal(r))`, `Missing` -> `Refused(orphan_refusal(r, site.kind()))`, `Trusted` -> `warnings.push(DryRunLockRecord { .., heartbeat_age_secs: now_ns.checked_sub(r.last_heartbeat_wall_time).map(|d| d / 1_000_000_000) })` and `Recorded(r)`; a `workspace` error -> `Error`. Then one `read_dir` of `site.dir()`: every name that starts with `<lock_name>.broken.` pushes `DryRunBrokenLeftover { name }`.

- [ ] **Step 1: Write the fake tests** (helpers `fake()`, `live_lock`, `dead_lock`, `record` from `lock/test_support.rs:12-45`, `LockSite::directory(&p, OsStr::new("dest"))` as at `run/tests.rs:7958-7961`):
  - `dry_lock_rows_of_the_record_table`: absent -> `Absent`; a foreign file (`dead_lock(&d, "dest.flux-lock", b"hello, not a lock")`) -> `Refused` with code `ControlPlaneNamespaceConflict`; a torn record (`dead_lock(.., &[0u8; 100])`) with `break_lock: false` -> `Refused` `TargetLockUncertain`; the same with `break_lock: true` and a capability that allows exclusive -> `Takeover(Uncertain::Torn)` and one `DryRunLockTakeover` warning; the same with `LockCapability::Unsupported` -> `Refused` `TargetLockUncertain`; a dead record whose workspace is missing (`record(&site, &id(5), "operations/<id(5)>")`, no directory) -> `Refused` `ArtifactOwnershipUncertain`; a dead record with `workspace_path` `"none"` -> `Recorded` and one `DryRunLockRecord` whose `heartbeat_age_secs` is `Some(_)`; a record whose heartbeat is `now + 10 s` -> `heartbeat_age_secs == None`; a `.broken.` sibling (`dead_lock(&d, "dest.flux-lock.broken.<id(7)>", ..)`) -> one `DryRunBrokenLeftover { name }`.
  - `dry_lock_never_opens_or_locks`: after every row, `calls(&fs)` has no entry starting with `open_lock(`, `create_lock(` or `try_lock`, and exactly one starting with `read_file(/p/dest.flux-lock)` per `dry_lock` call that found a file.
  - In `lock/obtain.rs`'s own tests nothing changes: they are the oracle that the extraction preserved `obtain`.

- [ ] **Step 2: Run, expect compile failure** — `cargo nextest run -p flux-core -E 'test(dry_lock_)'`.

- [ ] **Step 3: Implement** the constructors and `dry_lock` as in Interfaces. `now_ns` comes from the caller (`crate::state::wall_time_ns`, `state.rs:671`).

- [ ] **Step 4: Run the new tests and the lock tests** — `cargo nextest run -p flux-core -E 'test(dry_lock_) | test(lock::)'`; Expected: all pass.

- [ ] **Step 5: Gate and commit** — `just check` rc 0; message `feat(core): cut 11b, the read-only lock-record preview and the shared refusal constructors`.

### Task 3: The prior-classification arms and the plain dry run's refusal (spec 3.3, D1)

**Files:**
- Modify: `crates/flux-core/src/run/mod.rs:1060-1197` (`dry_prior_notes`, `dry_tree`, `dry_file`)
- Modify: `crates/flux-core/src/run/tests.rs:7882-7954` (the two 11a prior tests), new tests
- Test: `crates/flux-core/src/run/tests.rs`

**Interfaces:**
- Consumes: Task 2's `dry_lock`/`LockPreview`; `Place::scan` (`place.rs:44`), `Scan { resumable, completed, debris }` (`prior.rs:30-36`), `resumable_refusal(&[PriorOp]) -> LockError` (`prior.rs:174`), `from_lock(e, RunStep::Inspect, place.destination(), false)` (`session.rs`, imported at `run/mod.rs:42`), `check_capability` (`obtain.rs:36`, already imported at `run/mod.rs:29`), `LockSite::directory` / `file` / `root`.
- Produces: `dry_tree` and `dry_file` restructured as: (1) `dry_lock` with the site cut 11a already builds; `Refused(e)` or `Error(e)` -> `run.stop = Some(from_lock(e, RunStep::Lock, place.holder_shown(), false))`, `run.dry = Some(DryRun { .. })` with the flags set, and RETURN before any scan (spec 3.2 "ends the preview there"); `Recorded` sets `owner_liveness = true`, `Takeover` sets `lock_takeover = true`; (2) the scan: `Err(e)` -> `run.stop = Some(from_lock(e, RunStep::Inspect, place.destination(), false))`, return; `Ok(scan)`: completed priors and debris push their notes (`DryRunPriorCompleted`, `DryRunDebris`) and set `prior_seen`; then by invocation: plain with `resumable` non-empty -> push `DryRunPrior` per resumable (the 11a note, reworded in Task 1), then `run.stop = Some(from_lock(resumable_refusal(&scan.resumable), RunStep::Inspect, ..))`, return; `cfg.resume` with none -> `prior = Some(DryPrior::StartedNew)` and the fresh plan; `cfg.resume` with one -> Task 6/7 (for now: `todo!()` is NOT allowed; this task leaves a `DryPrior::StartedNew`-free placeholder by returning the fresh plan with `prior: None` and a `// Task 6` comment, and its test asserts only the arms built here); `cfg.resume` with two or more -> the refusal; `cfg.restart` -> Task 4 (this task: the fresh plan with `prior: None`).
- `dry_prior_notes` keeps only the completed/debris/unreadable notes; its resumable loop moves into the plain arm.

- [ ] **Step 1: Update the two 11a tests**: in `a_dry_run_notes_each_prior_and_still_plans` and `a_dry_run_says_whether_it_saw_a_prior_through_both_entry_points` (`run/tests.rs:7882-7954`) the `TRANSFERRING` cases now assert `r.stop` is a `RunError::Refused` with code `ResumableOperationExists` and `changed: false`, `r.copy.is_none()`, the `DryRunPrior` note still present and `r.dry.as_ref().unwrap().prior_seen`; the completed cases keep their expectations.

- [ ] **Step 2: Write new tests**: `dry_a_plain_run_with_a_resumable_prior_predicts_the_refusal` (tree and file: `prior(&fs, 5, OpState::Transferring)` / `file_prior(&fs, 5)`, exit shape as above, `forbidden(&fs, base)`); `dry_resume_with_no_prior_starts_new` (`RunConfig { resume: true, ..dry() }`: `prior == Some(DryPrior::StartedNew)`, `files_copied == 2`); `dry_resume_with_two_priors_predicts_the_refusal` (`prior(&fs, 5, Transferring)`, `prior(&fs, 6, Transferring)`); `dry_a_predicted_lock_refusal_prints_no_prior_note` (a foreign lock plus a resumable prior: `stop` is `ControlPlaneNamespaceConflict`, no `DryRunPrior` warning, no `read_dir(/p/dest/.flux` call); `dry_a_recorded_lock_prints_the_dual_outcome_and_previews_the_dead_case` (`live_lock` with a `"none"` workspace record, no prior: `owner_liveness`, one `DryRunLockRecord`, `files_copied == 2`, `stop.is_none()`, no lock call).

- [ ] **Step 3: Run, expect failures** — `cargo nextest run -p flux-core -E 'test(dry_)'`.

- [ ] **Step 4: Implement** the restructure as in Interfaces.

- [ ] **Step 5: Run and gate** — the `dry_` tests pass; `just check` rc 0.

- [ ] **Step 6: Commit** — `feat(core): cut 11b, the dry run classifies priors as the real run does and predicts RESUMABLE_OPERATION_EXISTS`.

### Task 4: The restart preview (spec 3.5)

**Files:**
- Modify: `crates/flux-core/src/run/sweep.rs:19-88` (extract the walk), `crates/flux-core/src/run/dry_prior.rs`, `crates/flux-core/src/run/mod.rs` (`dry_tree`, `dry_file` restart arms)
- Test: `crates/flux-core/src/run/sweep.rs` (`mod tests`, existing sweep test is the oracle), `crates/flux-core/src/run/tests.rs`

**Interfaces:**
- Produces in `sweep.rs`: `pub(crate) fn partials_under<F: DestinationRoot>(fs: &F, dest_shown: &Path, operations_shown: &Path, ids: &BTreeSet<&str>, warnings: &mut Vec<RunWarning>) -> Vec<(PathBuf, String)>` (each partial's DEST-relative path and its prior id, found exactly as `sweep.rs:36-73` finds them, with the same `PartialKept` warnings for an unlistable directory and no `kept` bookkeeping); `sweep_partials` calls it and then runs `check()` + `remove_below` per entry (behaviour unchanged: `each_deleted_partial_is_reported_with_its_path_and_the_others_stay` at `sweep.rs:110` is the oracle).
- Produces in `dry_prior.rs`: `pub(crate) fn dry_restart<F: DestinationRoot>(fs: &F, place: &TreePlace<'_, F>, priors: &[PriorOp], warnings: &mut Vec<RunWarning>, on_report: &mut dyn FnMut(TreeFailure)) -> DryPrior` emitting, per prior, `TreeFailure { path: PathBuf::from(".flux/operations/<id>"), cause: TreeFailureCause::Planned(PlannedEntry { object: State, action: Discard }) }` and `DryRunPriorSuperseded`-free (the note is derived by the CLI from `DryPrior::Restart.superseded`), then one `Planned(Partial, Discard)` per `partials_under` entry, returning `DryPrior::Restart { superseded: priors.iter().map(|p| (id, state)).collect(), partials, states: priors.len() as u64 }`. The single-file form in `dry_file`: the state record path is `record_name(&place.target, id)` (`state.rs:836`), the partial `temp_path`'s name for the target with the prior's id.
- `dry_tree`'s `cfg.restart` arm: `Takeover`/`Recorded` already handled by Task 3; `dry_restart` runs before the fresh plan; DEST absent (`place.dest.is_none()`) -> `DryPrior::Restart { superseded: [], partials: 0, states: 0 }` without a walk.

- [ ] **Step 1: Tests**: `dry_restart_lists_every_partial_and_state_it_would_discard` (fake: `prior(&fs, 5, Transferring)`, `prior(&fs, 6, Failed)`, partials `fs.write_file("/p/dest/sub/a.flux-partial.<id(5)>", ..)` and `"/p/dest/b.flux-partial.<id(6)>"` and a foreign `"/p/dest/c.flux-partial.<id(9)>"`: the streamed `Planned` records are exactly the two states and the two partials, in walk order, `prior == Restart { superseded: [(id(5), Transferring), (id(6), Failed)], partials: 2, states: 2 }`, every partial still exists, `forbidden(&fs, base)`); `dry_restart_then_plans_the_fresh_tree` (`files_copied == 2` after); `dry_restart_of_an_absent_dest_discards_nothing`; `dry_restart_on_a_single_file_names_its_state_and_partial` (`file_prior(&fs, 5)`, a `/p/t.flux-partial.<id(5)>`).

- [ ] **Step 2: Run, expect failures.** — `cargo nextest run -p flux-core -E 'test(dry_restart)'`.

- [ ] **Step 3: Implement** `partials_under`, the `sweep_partials` refactor, `dry_restart`, the arms.

- [ ] **Step 4: Run** `cargo nextest run -p flux-core -E 'test(dry_restart) | test(sweep::)'`; Expected: pass. Gate: `just check` rc 0.

- [ ] **Step 5: Commit** — `feat(core): cut 11b, the dry --restart lists the states and partials a real run would discard`.

### Task 5: The fake's store copy seam and the rescoped `forbidden`

**Files:**
- Modify: `crates/flux-core/src/fault_fs.rs:2116-2200` (`create_claim_store`, `open_claim_store`), its `mod tests`
- Modify: `crates/flux-core/src/run/tests.rs:7744-7786` (`forbidden`), `crates/flux-core/src/tree.rs:7188-7230` (`forbidden`)
- Test: `crates/flux-core/src/fault_fs.rs`

**Interfaces:**
- Produces: `create_claim_store` writes the file bytes `b"flux-fake-claim-store\0" ++ <own path>` instead of `Vec::new()` (`fault_fs.rs:2131`); `open_claim_store` on a path with no `claim_files` entry whose bytes start with that marker resolves the origin path after the NUL, deep-clones `claim_files[origin]` and `prepared_files[origin]` into NEW `ClaimMap`s registered under the copy's path (and pushed to `claim_stores`/`prepared_stores`), and opens the copy: a byte copy of a store is an independent store. Zero-length stays `StateCorrupt` "zero-length claim store"; other bytes stay "not a claim store" (`fault_fs.rs:2176-2178`; the test at `run/tests.rs:3297` is the oracle).
- `forbidden` (both copies): the banned list applies only to a call whose argument path starts with `/p` or `/src`; `ALLOWED` is checked only for calls under `/p` or `/src` too; a call under any other root (the fake's `<temp>`) is neither banned nor allow-listed, and the test that uses a `<temp>` asserts its calls separately (Task 6).

- [ ] **Step 1: Fake tests**: `a_byte_copy_of_a_claim_store_opens_as_an_independent_store` (create a store under `/a`, insert one claim and one note, drop it; copy the file bytes to `/b/state.db` via `write_file`; `open_claim_store` on `/b` sees the claim and the note; inserting into the copy does not change the original; the original's bytes start with the marker); `a_zero_length_or_garbage_state_db_is_still_corrupt`.

- [ ] **Step 2: Run, expect failure.** — `cargo nextest run -p flux-core -E 'test(a_byte_copy_of_a_claim_store)'`.

- [ ] **Step 3: Implement** the marker and the resolution; rescope both `forbidden` helpers (their doc comments say so).

- [ ] **Step 4: Gate** — `just check` rc 0 (every existing test still passes: the marker is never read by a test except through `open_claim_store`).

- [ ] **Step 5: Commit** — `test(core): the fake's claim store survives a byte copy as an independent store; forbidden scoped to /p and /src (cut 11b)`.

### Task 6: The copy, `dry_adopt` and the tree resume preview (spec 3.4 steps 1-5)

**Files:**
- Modify: `crates/flux-core/src/run/recover.rs:42-45, 257-303` (`Recovered.strays`, `dry: bool`)
- Modify: `crates/flux-core/src/run/place.rs:618-640` (the live call passes `dry: false`)
- Modify: `crates/flux-core/src/run/dry_prior.rs`, `crates/flux-core/src/run/mod.rs` (`dry_tree`'s `--resume` arm), `crates/flux-core/src/tree.rs:409-418` (`Shared.dry_prior`, the `claims` doc), `:1380-1400` and `:1875-1895` (the resume arms stream `Planned(File|Symlink, Resume)` under `cx.dry_run`), `:2171` area (`dry_stop_file`: the partial check)
- Test: `crates/flux-core/src/run/tests.rs`, `crates/flux-core/src/run/recover.rs` (`mod tests`)

**Interfaces:**
- Consumes: `resume::validate` (`run/resume.rs:32`), `recover_publications` (`recover.rs:257`), `Place::roots` (`place.rs:88`), `TreePlace.dest` / `open_workspace(id)` (`place.rs:352`), `temp_path(target: &Path, id: &OperationId)` (`crates/flux-fs/src/options.rs:123`; build the `OperationId` from the prior's id string), `OperationState.state` (`state.rs:314`), `DirHandle::{open_read, create_new, create_dir, remove_file, remove_dir, open_claim_store}`, `FileHandle::sync_all` (`fs.rs:123`).
- Produces in `recover.rs`: `Recovered { recovered: u64, strays: u64 }`; `recover_publications(dest, dest_shown, operation_id, store, warnings, manifest_shown, dry: bool)`: under `dry` the loop at `:293-300` is skipped and `strays` is `strays.len() as u64` (live: the removals run and `strays` counts the rows too).
- Produces in `dry_prior.rs`: `pub(crate) struct TempDir<D: DirHandle> { parent: D, name: OsString, dir: Option<D> }` with `fn make<F: DestinationRoot>(fs: &F, root: &Path, operation_id: &str) -> Result<TempDir<F::Dir>, FsError>` (`fs.destination_root(root)`, then `create_dir(OsStr::new(&format!("flux-dry-{operation_id}")))`; the id is unique per run, so no random suffix) and `impl Drop` that drops `dir`, then `parent.remove_file` of `state.db` through a fresh `open_dir`, then `parent.remove_dir(name)`, ignoring errors; `pub(crate) fn copy_store<D: DirHandle>(workspace: &D, temp: &D) -> Result<(), FsError>` (`open_read("state.db")`, `create_new("state.db")`, `std::io::copy`, `sync_all`); `pub(crate) enum DryAdopt<C> { Store { claims: C, prior: DryPrior }, Refused(RunError) }`; `pub(crate) fn dry_adopt<F: DestinationRoot>(fs: &F, place: &TreePlace<'_, F>, prior: &PriorOp, opts: &CopyOptions, temp_root: &Path, warnings: &mut Vec<RunWarning>) -> DryAdopt<<F::Dir as DirHandle>::Claims>` implementing spec 3.4 steps 1-3: `validate` -> `Refused(e)`; a `CREATED` prior whose `open_read("state.db")` is `NotFound`, or whose file is zero-length / corrupt when opened later -> `DryRunStoreFresh` and `Store { claims: <a fresh store created in <temp> through create_claim_store>, .. }`; `open_read` `NotFound` past `CREATED` -> `Refused(refused_state(LockCode::StateCorrupt, format!("{}: the workspace has no state.db; {BY_HAND}", db.display())))` (the live text at `place.rs:609-614`; make `refused_state`, `state_error` and `BY_HAND` `pub(crate)` in `place.rs`); other read errors -> `Refused(failed(RunStep::State, &db, e))` (`session.rs:509`); `TempDir::make` or the copy's write failing -> `Refused(RunError::Failed { .. })` naming `<temp>` (the `Failed` shape at `exit_code.rs:203-210` shows the fields); `temp.open_claim_store("state.db", opts.durability)` failing `StateCorrupt`/`IncompatibleState` -> `DryRunStoreTorn` then `Refused(state_error(&db_on_disk, e))`; then `recover_publications(dest, .., prior.state.operation_id, &mut store, warnings, &manifest_shown, true)`: `Err(Uncertain(list))` -> `Refused(refused_state(LockCode::CommitStateUncertain, uncertain_detail(&list)))`, `Err(Store(e))` -> `Refused(state_error(..))`; `Ok(r)` -> `Store { claims: store, prior: DryPrior::Resume { operation_id, claims: store.count().ok(), recovered: r.recovered, strays: r.strays } }`. The `TempDir` is returned inside `DryAdopt::Store` as a third field `temp: TempDir<F::Dir>` so `dry_tree` holds it in a local declared BEFORE the store's `RefCell` (spec 3.4 step 2: dropped after the store).
- `dry_tree`'s `--resume` arm (one resumable): `dry_adopt`; `Refused(e)` -> `run.stop = Some(e)`, return; `Store { temp, claims, prior }` -> `let temp = temp; let store = RefCell::new(claims);` then the walk with `Shared { resume: true, claims: Some(&store), dry_prior: Some(&prior_id), .. }`; after the walk `run.dry.prior = Some(prior)`, `partials_present` from the walk's count (a `Cell<u64>` in `Shared`? no: `TreeOutcome` gains no field; `Shared.dry_partials: &Cell<u64>` set by `dry_stop_file`).
- `tree.rs`: `Shared` gains `pub(crate) dry_prior: Option<&'c str>` and `pub(crate) dry_partials: Option<&'c Cell<u64>>`; the `claims` doc comment at `:413` becomes "`None` for a copy that holds no lock; a dry run passes its null store or, under `--resume`, the copy of the prior's store (cut 11b)"; in the file resume arm (`:1386-1400`) and the link arm (`:1880-1895`), when the claim resumes the entry AND `cx.dry_run`, stream `Planned(File|Symlink, Resume)` through `report` before returning (the counters already count it); in `dry_stop_file`, when `cx.dry_prior` is `Some(id)`, one `parent.metadata(temp_path(stored_name, id))`: `Ok(_)` -> the planned entry's reason text is `"partial of <len> bytes present; its continuation is not previewed"` (a new `PlannedAction::Copy` reason: add `pub reason: Option<String>`? No: keep `PlannedEntry` `Copy`; the CLI prints the suffix from `TreeFailure`'s path list? Decision: add `PlannedAction::CopyOverPartial { len: u64 }` with `as_str` `"would_copy"` and the CLI appending the suffix; Task 1's enum gains it here, with its words test line) and `dry_partials` += 1; `NotFound` -> nothing; other errors -> nothing (metadata only).

- [ ] **Step 1: `recover.rs` test**: `recovery_under_dry_counts_strays_and_removes_nothing` (a `Renamed` note whose stray temp still exists: `dry: true` leaves the temp in place, `strays == 1`, `recovered == 1`, the store's claim is `Created`; `dry: false` on a fresh fixture removes it, `strays == 1` too).

- [ ] **Step 2: `run/tests.rs` tests** (fixtures: `prior(&fs, 5, Transferring)`, `store_of(&fs, 5)` at `:4849`, `note_at` at `:4865`, `temp_of` at `:4860`; `RunConfig { resume: true, dry_temp_root: Some(PathBuf::from("/flux-dry")), ..dry() }` with `fs.create_dir("/flux-dry")`):
  - `a_dry_resume_previews_resumed_entries_from_a_copy_of_the_store`: claims `Created` for `a` matching the destination (copy `a` into `/p/dest/a` and insert the claim as the live walk would: use the live run once to build the fixture, then `forbidden`-free snapshot), then the dry resume: the streamed records contain `Planned(File, Resume)` for `a` and `Planned(File, Copy)` for `sub/b`, `files_resumed == 1`, `prior == Some(Resume { operation_id: id(5), claims: Some(1), recovered: 0, strays: 0 })`, the on-disk store unchanged (its `calls` show no write), `/flux-dry/flux-dry-<ID>` gone after the run, calls under `/flux-dry` are `create_dir`, `create_new`, `open_claim_store`, `claim_*` reads and `claim_apply_recovery`, `remove_file`, `remove_dir` only; `forbidden(&fs, base)`.
  - `a_dry_resume_recovers_a_renamed_note_on_the_copy_and_keeps_the_stray`: a `Renamed` note (the fixture pattern of the cut 9c tests near `run/tests.rs:4866`): `recovered == 1`, `strays == 1`, the stray temp still exists, the entry streams `Resume`.
  - `a_dry_resume_with_an_uncertain_note_predicts_commit_state_uncertain`: `stop` code `CommitStateUncertain`, `<temp>` gone.
  - `a_dry_resume_of_a_created_prior_without_a_store_plans_fresh`: `prior(&fs, 5, OpState::Created)`, no store: `DryRunStoreFresh`, `files_copied == 2`.
  - `a_dry_resume_of_a_transferring_prior_without_a_store_predicts_state_corrupt`.
  - `a_dry_resume_reports_a_partial_as_would_copy_with_its_size`: `fs.write_file("/p/dest/sub/" + temp_of("b", 5), b"BB")`: the record for `sub/b` is `CopyOverPartial { len: 2 }`, `partials_present == 1`.
  - `dry_adopt_removes_its_temporary_directory_on_every_path`: the refused paths above plus a torn store (`fs.write_file(state_db(5), b"garbage")` over an existing map: `DryRunStoreTorn`, `stop` `StateCorrupt` whose detail names `/p/dest/.flux/operations/<id>/state.db`), each leaving nothing under `/flux-dry`.
  - `a_dry_resume_matches_the_live_resume_on_the_fake`: the fixture built by a live run interrupted with `fs.fail(..)` as the cut 9a/9c tests do (`run/tests.rs:2018-2104` show the pattern), then dry and live on two fakes built the same way: `files_resumed`, `files_copied`, `recovered` equal, and the dry run's `Planned` records' paths with `Resume` equal the live run's resumed set.

- [ ] **Step 3: Run, expect failures.** — `cargo nextest run -p flux-core -E 'test(dry_resume) | test(dry_adopt) | test(recovery_under_dry)'`.

- [ ] **Step 4: Implement** as in Interfaces (the `CopyOverPartial` variant and its word test line included).

- [ ] **Step 5: Run and gate** — the tests pass; `just check` rc 0.

- [ ] **Step 6: Commit** — `feat(core): cut 11b, a dry --resume previews the walk from a repaired, recovered copy of the prior's claim store`.

### Task 7: The single-file resume preview (spec 3.4 step 6)

**Files:**
- Modify: `crates/flux-core/src/run/mod.rs:1159-1197` (`dry_file`)
- Test: `crates/flux-core/src/run/tests.rs`

**Interfaces:**
- Consumes: `resume::validate`, `FilePlace` (`place.rs:643`), `file_prior` / `file_prior3` helpers (`run/tests.rs:777, 3465`), `temp_path`.
- Produces: in `dry_file`'s `--resume` arm with one resumable: `validate(&prior, &place.roots(), opts, warnings)`: `Err(e)` -> `run.stop = Some(e)`; `Ok(_)` -> `prior = Some(DryPrior::Resume { operation_id, claims: None, recovered: 0, strays: 0 })`, then `plan_file` as today; `metadata` of `temp_path(target, prior id)` on `place.dir`: present -> `planned.action = CopyOverPartial { len }`, `partials_present = 1`.

- [ ] **Step 1: Tests**: `a_dry_single_file_resume_validates_and_reports_its_partial` (`file_prior(&fs, 5)`, a `/p/t.flux-partial.<id(5)>` of 3 bytes: `prior == Resume { claims: None, .. }`, `planned.action == CopyOverPartial { len: 3 }`, `partials_present == 1`, no lock call); `a_dry_single_file_resume_predicts_incompatible_state` (a prior with other options: `stop` code `IncompatibleState`).

- [ ] **Step 2-4: Run (fail), implement, run (pass), `just check` rc 0.**

- [ ] **Step 5: Commit** — `feat(core): cut 11b, the single-file dry --resume validates the prior and reports its partial`.

### Task 8: The CLI (spec 3.1, 3.6, 3.7; tests 79 and 136)

**Files:**
- Modify: `crates/flux-cli/src/main.rs:281-285` (remove the usage error), `:327-360` and `:382-425` (the notes and summary lines in both branches)
- Modify: `crates/flux-cli/src/report.rs:200-240` and `:380-395` (the planned-entry line: the `CopyOverPartial` suffix), `:341-380` (`dry_run_summary_line` unchanged; `unprobed_keys`, `unprobed_lines`), `:486-502` (the new `RunWarning` arms), `:513-531` (`resume_lines` gains the dry forms)
- Test: `crates/flux-cli/src/report.rs` (`mod tests`), `crates/flux-cli/tests/dry_run.rs:122-140` (the usage-error test is replaced)

**Interfaces:**
- `unprobed_keys(d: &DryRun) -> Vec<&'static str>` in the order `noreplace_publish`, `target_lock`, `owner_liveness`, `lock_takeover`, `prior_operation`, `resume_verify`; `prior_operation` only when `d.prior_seen` (completed priors and debris, Task 3); `resume_verify` when `d.prior.is_some()` or the run had `--resume` (pass `resume: bool`; the CLI passes `args.resume`).
- Note strings (verbatim from the spec): `DryRunLockRecord` -> `note: a lock record for operation {operation_id} exists (owner instance {owner_instance_id}, boot session {boot_session_id}, last heartbeat {age} ago, workspace {workspace_path}); a real run tries the OS lock: if it is held, TARGET_LOCK_BUSY; if not, the preview below applies, and this exit code reflects that case` where `{age}` is `{n} s` or `in the future` (the phrase "last heartbeat in the future ago" is avoided: print `last heartbeat in the future` for `None`); `DryRunLockTakeover` -> `note: --break-lock would take over the lock, whose record is unreadable ({why}); a real run refuses with TARGET_LOCK_BUSY if the OS lock is held`; `DryRunBrokenLeftover` -> `note: an interrupted lock recovery left {name}; a real run classifies it`; `DryRunDebris` -> `note: a real run would remove the workspace debris {name}`; `DryRunStoreFresh` -> `note: would create a fresh claim store for operation {operation_id}`; `DryRunStoreTorn` -> `note: the prior's claim store could not be read consistently; a live run may have been writing it: run the dry run again before deciding on --restart`; the `resume_verify` line `note: --resume-verify={mode} would be applied by a real run; nothing was verified` (`mode` is `chunks` or `full`).
- `resume_lines` under dry (a new `pub fn dry_prior_lines(p: &DryPrior, files_resumed: u64) -> Vec<String>`): `StartedNew` -> `no prior operation; starting new`; `Resume` -> `would resume operation {id} ({n} entries claimed)` or `would resume operation {id}` when `claims` is `None`, then `would recover {recovered} interrupted publications` when > 0, `would remove {strays} stray temporaries` when > 0, and after the walk `would resume {files_resumed} already-complete files` when > 0; `Restart` -> `note: would supersede operation {id} (state {STATE})` per prior, then the summary lines `dry run: would discard {partials} partials and {states} states` when either > 0.
- Summary extras after `dry_run_summary_line`: `dry run: would resume R already-complete files` (R = `files_resumed`, when > 0), the discard line above, `dry run: {n} partial files present, continuation not previewed` when `partials_present > 0`.
- Stream line: `CopyOverPartial { len }` prints `{p}: object_type=file action=would_copy (partial of {len} bytes present; its continuation is not previewed)`; `State`/`Partial` objects print `object_type=state` / `object_type=partial` with `action=would_discard`.
- Order of stderr (spec 3.6): lock note, prior notes, resume/restart lines, the 11a `unprobed` notes, the `resume_verify` note, the stream lines are printed as they arrive (before all of these, as in 11a), then the summary lines. A predicted refusal prints the refusal line only (as 11a's pre-flight refusals).

- [ ] **Step 1: Replace the usage-error test** in `crates/flux-cli/tests/dry_run.rs:122-140` with `dry_run_combines_with_resume_and_restart` (both exit 0 on a fresh tree; `--resume` prints `no prior operation; starting new`). Add: `test_79_dry_resume_and_restart_report_and_change_nothing` (a prior built by running a real copy, then corrupting nothing: `--dry-run --restart` lists `.flux/operations/<id>: object_type=state action=would_discard` and prints the fresh plan, exit 0, the workspace still present; `--dry-run --resume` on a completed prior prints the fresh plan with the completed note); `test_136_takeover_preview` (a torn lock file beside DEST: `--dry-run --restart --break-lock` prints the takeover note, `unprobed` has `lock_takeover`, exit per the preview, the lock bytes unchanged; without `--break-lock` exit 3 with `TARGET_LOCK_UNCERTAIN`); `a_recorded_lock_prints_the_dual_outcome_and_exits_zero` (a lock file with a valid record whose workspace is `none`: the note, `owner_liveness`, exit 0); `the_json_unprobed_order_is_fixed` (asserts the full order with a fixture that sets all six).
- [ ] **Step 2: `report.rs` unit tests** for each note string, `dry_prior_lines`, the `CopyOverPartial` line, the `state`/`partial` lines, `unprobed_keys` order.
- [ ] **Step 3: Run, expect failures** — `cargo nextest run -p flux-cli`.
- [ ] **Step 4: Implement.**
- [ ] **Step 5: Run and gate** — `cargo nextest run -p flux-cli` passes; `just check` rc 0.
- [ ] **Step 6: Commit** — `feat(cli): cut 11b, --dry-run combines with --resume, --restart and --break-lock; the notes, lines and unprobed keys`.

### Task 9: The real-filesystem paired corpus (spec section 6)

**Files:**
- Create: `crates/flux-core/tests/dry_run_prior_std_fs.rs` (helpers copied from `dry_run_std_fs.rs:65-133, 373-405`: `snapshot`, `mk_dst`, `counters`, `run_tree`)
- Test: the file itself

**Interfaces:**
- Consumes: `flux_platform::StdFileSystem`, `RedbClaimStore` (`crates/flux-platform/src/claims.rs:71-88`), `DestinationRoot::destination_root` (`std_fs.rs:716`), `flux_core::run::{tree, file}` entry points as `dry_run_std_fs.rs` calls them.

- [ ] **Step 1: Tests**:
  - `a_dry_resume_on_a_crashed_prior_matches_the_live_walk`: build DEST by a real `tree` run with a `BeforeMutation` hook (`RunConfig.before_mutation`, `run/mod.rs:67-68`) that aborts after the first file so the store is left open and the manifest `TRANSFERRING`; make the store "not closed cleanly" by `std::mem::forget` of the store handle the hook captured (the allocator-state table is written only by a clean close, spec section 10; `flux-core` has no redb dependency, so the test does not assert `RepairAborted` directly: the paired agreement below is the oracle, and the copy path serves a clean store too); copy the whole DEST to a sibling; dry `--resume` on one, live `--resume` on the other: `files_resumed`, `bytes_resumed`, `files_copied` equal; the dry run's `Resume` paths equal the live run's resumed set; `snapshot(dest)` before == after the dry run; no `flux-dry-*` directory under `std::env::temp_dir()` after.
  - `a_dry_restart_lists_what_the_live_restart_removes`: the same fixture plus two partial files: the dry `would_discard` set equals the set of paths the live `--restart` removes (snapshot difference).
  - `a_store_held_open_by_another_handle_is_copied_without_refusal`: open the prior's `state.db` through `RedbClaimStore::open_file` and keep it open; the dry `--resume` returns either the preview or the `DryRunStoreTorn` note, never a stop with `TargetLockBusy`, and never blocks (a 30 s timeout thread guards the assertion).
  - `a_truncated_store_predicts_state_corrupt_and_leaves_no_temp`: truncate `state.db` to half: `stop` code `StateCorrupt`, the detail names the on-disk path, `temp_dir()` holds no `flux-dry-*`.

- [ ] **Step 2-4: Run (fail), implement fixtures, run (pass): `cargo nextest run -p flux-core --test dry_run_prior_std_fs`; `just check` rc 0.**

- [ ] **Step 5: Commit** — `test(core): cut 11b, the real-filesystem paired corpus for dry --resume and --restart`.

### Task 10: Documentation

**Files:**
- Modify: `docs/superpowers/specs/2026-10-17-cut-11a-dry-run-design.md` (section 3.5 and ruling P10: a one-line pointer "superseded by cut 11b"), `docs/accepted-boundaries.md:90-98` (replace the prior bullet with cut 11b's N1 and N4 as limits), `TODO.md:612` (closed: strike through with the cut 11b pointer; add a "Cut 11b known limits" section: N1 partial continuation, N4 `.broken.*`, N5 liveness, D7's undetected writer, D8's temp disk), `ROADMAP.md:47` (row 8: cut 11b built), `crates/flux-cli/src/main.rs:115` (the `--dry-run` help text no longer names cut 11b as future)
- Test: `typos`, `just check`

- [ ] **Step 1: Edit the five documents and the help text.**
- [ ] **Step 2: `just check` rc 0; `git diff --stat` shows only those files.**
- [ ] **Step 3: Commit** — `docs: cut 11b, the dry-run prior preview in the limits, boundaries, roadmap and help`.

---

## Self-review (writing-plans)

1. Spec coverage: 3.1 -> Task 8; 3.2 -> Task 2 (+3 for the wiring); 3.3 -> Task 3; 3.4 steps 1-5 -> Task 6, step 6 -> Task 7; 3.5 -> Task 4; 3.6, 3.7 -> Task 8; I8-I11 -> Tasks 2, 5, 6 (`forbidden`); section 5 rows -> Tasks 2, 3, 6, 7, 9; section 6 -> Tasks 2-9; D1 -> Task 3; D4 -> Tasks 6-8; D5 -> Tasks 1, 4; D6 -> Tasks 3, 8; D7/P2 -> Task 6 (no probe); D8 -> Tasks 5, 6; the docs list in section 6 -> Task 10. Gap closed in the plan: the spec's `would_copy (partial ...)` reason needed a carrier: `PlannedAction::CopyOverPartial { len }` (Task 6), words test in Task 6.
2. Step scan: every test step names the test and its assertions; code steps give signatures; the one open choice the spec left (`tempfile` vs std) is decided here: neither, the `<temp>` goes through `DirHandle` calls so the fake models it (Task 6 `TempDir`).
3. Type consistency: `DryPrior` (Task 1) is used by Tasks 3, 4, 6, 7, 8 with the same fields; `LockPreview` (Task 2) by Task 3; `partials_under` (Task 4) by `dry_restart`; `Recovered.strays` (Task 6) by Task 8's lines; `CopyOverPartial { len: u64 }` by Tasks 6, 7, 8.
4. Review Focus: the five lines each name their pinning test (Tasks 2, 3, 6, 8, 9).
5. Proportion: the plan is shorter than the spec plus the code it drives; bodies appear only for the `dry_lock` table and the `dry_adopt` error mapping, both of which the signatures do not determine.

## Exhaustiveness self-audit

- Under-specified: the `DryRunLockRecord` age rendering (fixed in Task 8: `{n} s` or `in the future`); where `partials_present` is counted (`Shared.dry_partials: &Cell<u64>`, Task 6); who owns `<temp>` (a local of `dry_tree` declared before the store, Task 6); the fake's copy seam (Task 5).
- Placeholders: Task 3 leaves the `--resume`-with-one and `--restart` arms as the fresh plan with `prior: None` until Tasks 4 and 6 fill them (stated, no `todo!()`).
- Unverified by the plan author, flagged for the implementer: how a real interrupted run leaves `state.db` "not closed cleanly" (Task 9 step 1 says which assertion to make and what to do if it cannot be made); whether `create_new` on `FakeDirHandle` records a call the rescoped `forbidden` ignores under `/flux-dry` (Task 5's scoping makes it so by construction).
- Cases: the matrix rows of spec 3.2/3.3/5 each have a test in Tasks 2, 3, 4, 6, 7, 9; the CLI order and keys in Task 8.

## Stand-downs (adversarial-panel-review on the plan)
Round 1 (agy escalation, `.clavity/seams/11b-plan-panel-r1.md`, `.clavity/scratch/11b-plan-panel/r1-reply.md`): `PANEL VERDICT: GREEN`, six seats, no findings. `DISCARDED-BELOW-FLOOR`, both folded anyway as one-token edits: `claims: store.count().ok()` (a `Result` where the field is an `Option`), `OsStr::new(&format!(..))` for `create_dir`'s argument (`crates/flux-fs/src/fs.rs:307` takes `&OsStr`). Peer's read carried into execution: Task 5 has the widest blast radius (every fake-backed claim-store test), so its task review runs on the full `flux-core` suite, not the task's tests alone. `UNVERIFIED-ACCEPTED`: none; Windows runtime behaviour of the `TempDir` drop order is measured by CI on the push.
