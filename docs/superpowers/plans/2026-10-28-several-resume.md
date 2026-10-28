# `flux copy --resume` with several sources (cut 19) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy --resume a b c DEST` continues a killed several-source run as the same operation, and `flux copy --dry-run --resume a b c DEST` previews it.

**Architecture:** `resume::validate` compares every root; `MultiPlace::adopt` (today a refusal) takes the prefix locks, checks the targets and adopts the one keyed claim store through the `TreePlace` it holds; the walk, `drive`, `settle` and `finish` already carry `resume` and the adopted store. The dry run gets the same adoption on a private copy of the store, previewed in the real order: compatibility, prefix locks, target check, store.

**Tech Stack:** Rust workspace (`flux-fs`, `flux-core`, `flux-cli`), the `FaultFs` fake for engine tests, `cargo nextest`, `just check`.

**Spec:** `docs/superpowers/specs/2026-10-28-several-resume-design.md` (panel record in its Stand-downs; read it first, this plan argues from it).

## Global Constraints

- Repo gate: `just check` (fmt-check, clippy `--all-targets -- -D warnings`, typos, licence headers, unused-deps, doc-check, tests). Expected: exit 0. Do not invent stricter flags. Every task ends with `just check` exit 0 BEFORE its commit; clippy flags `too_many_arguments` above seven parameters and dead code.
- Nextest filters: `cargo nextest run -p flux-core --no-fail-fast -E 'test(NAME)'` (a library unit test's name carries its module path, for example `run::multi::...`); `-p flux-cli` for the CLI tests. Add `--no-fail-fast` whenever the claim is "nothing else went red". Expected pass line: `N tests run: N passed`.
- Commit trailer on every commit: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>` (an implementer uses its own model name). Never write a short git sha in prose or commit bodies (typos gate). Stay on branch `feat/several-resume`.
- Never run `git checkout`, `switch`, `stash` or `reset`.
- The one-source resume and dry resume are unchanged byte for byte: their messages, `unprobed` keys and report lines (spec N4). A dry run writes nothing under DEST or its parent and takes no OS lock (cut 18 G3).
- A wrong value or encoding is a contract: the refusal code for every incompatibility is `INCOMPATIBLE_STATE`; the aside name of a recovered prefix lock stays `<name>.flux-lock.broken.<this run's id>`; the prefix lock records carry THIS run's id (spec section 3, P-A).
- Comments follow the surrounding density (short, why-not-what), ASCII where the repo's gates require it. Windows and macOS legs are verified only by CI after the push; engine tests normalise paths through `calls()`; the CLI kill test is Linux-only like cut 17's.

## Review Focus

1. A resumed run killed again and resumed a third time (successive `.broken.<id>` asides, a WAL generation that grows): Task 2, `a_second_kill_and_a_second_resume_finish_the_run`.
2. A source list reordered, renamed, or with a root changed from folder to file: Task 1 (unit) and Task 3 (CLI exit and text).
3. A kill between the prefix locks, before any manifest exists, then `--resume`: Task 2, `a_resume_with_no_prior_starts_new_over_prefix_lock_residue`.
4. A wrong-kind target or a foreign prefix lock at DEST before the resume: no lock file left, the prior untouched: Task 2, `a_resume_whose_adoption_is_refused_leaves_no_lock_file`.
5. A dry resume of the same states changes nothing and agrees with the real resume's counts: Task 4.

---

### Task 1: `resume::validate` compares every root

**Files:**
- Modify: `crates/flux-core/src/run/resume.rs` (the roots row of `validate`, about lines 80-118, and `source_mismatch` at about 163)
- Test: the `tests` module of the same file (`root()` at about 195, `roots_now()`)

**Interfaces:**
- Consumes: `Root { source_root, source_identity, destination_prefix, kind: Option<RootKind> }` (`state.rs:81`), `RootKind::{Tree, File}`.
- Produces: `validate(prior: &PriorOp, roots: &[Root], opts: &CopyOptions, warnings: &mut Vec<RunWarning>) -> Result<Config, RunError>` (unchanged signature) now refusing a difference in ANY root.

Behaviour (spec 4.2). After the existing count check, loop `stored.roots.iter().zip(roots)` in order; per root, in this order: `kind` (compared only when both are `Some`), source (identity where both `Strong`, else `source_root` equal, else mismatch), `destination_prefix`. The first difference refuses with `INCOMPATIBLE_STATE`. When `stored.roots.len() > 1` every message of this row is prefixed `root <name>: ` where `<name>` is the stored root's `destination_prefix` decoded (`shown`); with one root the messages are byte-identical to today's. New kind text: `the operation copies a <folder|file>, this run's source is a <folder|file>; run again with --restart to supersede it` (`Tree` is `folder`). `by_path` becomes `Vec<PathBuf>`: every root matched by path pushes one `RunWarning::ResumeMappingByPath`, in order.

- [ ] **Step 1: Write the failing tests** in the `tests` module. Build multi-root fixtures by cloning the file's `root()` helper with a `kind: Some(..)` and a non-empty `destination_prefix` (`native_hex(Path::new("a"))`). Names and assertions:
  - `a_swapped_pair_of_roots_is_refused_naming_the_first_root`: stored `[a, b]`, now `[b, a]` (strong identities differ): `Err` is `INCOMPATIBLE_STATE`, text starts `root a: the source root differs`, contains `--restart`.
  - `a_changed_kind_is_refused_naming_the_root_and_restart`: second root `Tree` stored, `File` now: text equals `root b: the operation copies a folder, this run's source is a file; run again with --restart to supersede it`.
  - `a_renamed_root_is_refused_with_the_destination_text`: same sources and kinds, second root's prefix `b` stored, `c` now: text starts `root b: the destination differs`.
  - `every_weak_root_matched_by_path_warns_once`: both roots `Weak` identity, same paths: `warnings` holds two `ResumeMappingByPath`, in root order.
  - `a_one_root_refusal_has_no_root_tag`: the existing single-root mismatch text does not contain `root `.
- [ ] **Step 2: Run them to verify they fail.** `cargo nextest run -p flux-core --no-fail-fast -E 'test(run::resume)'` Expected: the five fail, every existing test passes.
- [ ] **Step 3: Implement** as described above in `validate` and give `source_mismatch` an `Option<&str>` tag parameter (both of its call sites are in this function).
- [ ] **Step 4: Run** the same command. Expected: all pass. Mutant check: make the loop compare only the first pair; `a_swapped_pair...` must go red; restore.
- [ ] **Step 5: `just check`, then commit** `git add crates/flux-core/src/run/resume.rs && git commit -m "feat: resume compatibility compares every root"` with the trailer.

---

### Task 2: `MultiPlace::adopt` and the real resume

**Files:**
- Modify: `crates/flux-core/src/run/multi.rs`: `MultiPlace::adopt` (about 598), the `debug_assert!(!cfg.resume, ..)` and its comment in `trees()` (about 691-693), the test `adopting_a_prior_is_refused_as_an_incompatible_state` (about 1355).
- Test: the test module that holds `crashed_ab`, `crash`, `go_with`, `restart_cfg`, `lock_files`, `prefix_lock_files`, `manifest`, `calls` (about 2396-2460).

**Interfaces:**
- Consumes: Task 1's `validate` (called by `session.rs:181` before `adopt`), `TreePlace::adopt` (`run/place.rs:604`), `MultiPlace::{lock_directory, prefix_error, check_targets, release_prefix_locks, warn_release}`, `PrefixLocks::acquire(dir, capability, &names, &id)`.
- Produces: `MultiPlace::adopt(&mut self, prior: &OperationState, durability: Durability, warnings: &mut Vec<RunWarning>) -> Result<Option<AdoptedStore>, RunError>` working for a several-source prior.

Behaviour (spec 4.3, 4.4). `adopt`, in order: (1) take the prefix locks exactly as `create` does (lock directory, `PrefixLocks::acquire` with `self.prefix_id`, store in `self.prefix`, push `.broken` leftovers as `BrokenLeftover`; a refusal through `prefix_error`). (2) `check_targets()`; on error release the locks (`release_prefix_locks` then `warn_release`) and return it. (3) `self.tree.adopt(prior, durability, warnings)`; on error release the locks and return it. Return its result. `MultiPlace::remove` is NOT changed (spec 4.4: `settle` releases the locks before `finish`). Delete the `debug_assert!` and rewrite the comment above it to say a resume adopts in `MultiPlace::adopt`, and `remove` of a prior's id retires only a workspace.

**STOP conditions (report, do not adapt):** if a resumed run copies a file the prior completed, or `files_resumed` stays 0, the walk does not carry the adopted store for a keyed root: report `STATE_MISMATCH: <what>` and which of `drive`'s `Shared` fields (`run/mod.rs` about 583) is involved. Do not change `tree.rs`.

- [ ] **Step 1: Write the failing tests** (engine, fake fs). `resume_cfg(n)` is a new helper next to `restart_cfg`: `RunConfig { operation_id: format!("{:032x}", n), resume: true, ..cfg() }`. Crash points come from `reference(fake, &["a","b"], "rename_no_replace", "/p/dest/b/k")` and `crash`, which leaves root `a` complete and `b` partial.
  - `a_several_source_resume_continues_a_crashed_run`: `ok(&r)`; `r.run.resumed` is `Some(ResumeNote::Adopted { operation_id, .. })` with `operation_id == ID`; `ok(&r).files_resumed >= 1`; after the resume the log (from a `mark = calls(&fs).len()`) holds no `create_new(` of `/p/dest/a/x` or of its partial; `/p/dest/a/x` still reads `ax` and `/p/dest/b/k` reads `bk`; `lock_files(&fs)` is empty; `/p/dest/.flux/operations/{ID}` is gone.
  - `a_second_kill_and_a_second_resume_finish_the_run`: crash, resume with `resume_cfg(0x22)` but crash again mid-resume (`crash`-style with `resume_cfg`), then resume with `resume_cfg(0x33)`: `ok`, same final tree, `lock_files` empty, and two distinct `.flux-lock.broken.` names exist for each prefix (one per resume's id: `{0x22:032x}` and `{0x33:032x}`).
  - `a_resume_with_no_prior_starts_new_over_prefix_lock_residue`: kill the run at the first call touching `/p/dest/b.flux-lock` (find it with `reference(fake, &["a","b"], "create_lock", "/p/dest/b.flux-lock")`: `FaultFs` records a lock file as `create_lock(<path>)`, `fault_fs.rs:2313`), so prefix locks exist and no manifest; `go_with` using `resume_cfg(0x22)` ends `ok`, `r.run.resumed` is `Some(ResumeNote::StartedNew)`, the tree is complete.
  - `a_resume_whose_adoption_is_refused_leaves_no_lock_file`: after `crashed_ab()`, make `/p/dest/b` a regular file; the resume refuses `DestinationNamespaceCollision`; `prefix_lock_files(&fs)` is empty; the prior's manifest is still `Transferring` and its workspace exists. A second variant: a torn `state.db` (use the one-source analogue in `run/tests.rs`, `a_dry_resume_of_a_created_prior_without_a_store_plans_fresh` and its real twin, as the pattern) refuses `StateCorrupt` with no `*.flux-lock` file left.
  - `a_created_prior_without_a_store_resumes_with_a_fresh_store`: adapt the one-source test `a_created_manifest_without_a_usable_state_db_resumes_with_a_fresh_store` (`run/tests.rs:3324`) to two roots.
  - `a_one_source_prior_and_a_several_source_resume_refuse_each_other`: a several-source resume over a one-source TREE prior refuses `IncompatibleState` with the count text; a one-source resume (through the single-tree entry the tests in `run/tests.rs` use) over a several-source prior refuses the same way.
  - Delete `adopting_a_prior_is_refused_as_an_incompatible_state`.
- [ ] **Step 2: Run them to verify they fail.** `cargo nextest run -p flux-core --no-fail-fast -E 'test(run::multi)'` Expected: the new tests fail (debug assert or refusal), the rest pass.
- [ ] **Step 3: Implement** `adopt` and the `trees()` change as above.
- [ ] **Step 4: Run** the same command. Expected: all pass. Mutants (each must turn its named test red, then restore): skip the release on the `check_targets` error (`..refused_leaves_no_lock_file`); make `adopt` skip step 1 (`..continues_a_crashed_run`, because no lock is held and `lock_files`/ordering differ, or the run proceeds without locks: record what went red); pass `prior_id` instead of `self.prefix_id` (`a_second_kill..`, the aside names).
- [ ] **Step 5: `just check`, then commit** `git add -A crates/flux-core/src/run/multi.rs && git commit -m "feat: resume a crashed several-source run"` with the trailer.

---

### Task 3: the CLI accepts `--resume` with several sources

**Files:**
- Modify: `crates/flux-cli/src/main.rs` (`several_flag_gate` at about 650 and its call at about 662; the `Copy` doc comment at about 36-42; the `--resume` help at about 148-153 only if it names the one-source limit)
- Test: `crates/flux-cli/tests/several_sources.rs` (replace `resume_exits_2_naming_the_flag` at about 151; the kill harness `Stalled` and `STALL_AT` at about 542-600)

**Interfaces:**
- Consumes: Task 2's working engine; `Stalled::start_several(&[&Path], &out, STALL_AT, marks, &[])` and `.kill()` as `a_dry_restart_over_a_killed_run_prints_the_discards_and_changes_nothing` (about 668) uses them.
- Produces: `flux copy --resume a b c DEST` exits 0 after a kill; a changed list exits 3 (`REFUSED`).

- [ ] **Step 1: Write the failing tests** (Linux-only, gated like the cut 17 kill tests):
  - Remove `resume_exits_2_naming_the_flag`. Add `a_resume_over_a_killed_run_finishes_it`: start several sources with `Stalled`, `kill()`, then run `flux copy --resume --json` with the same sources: exit 0; `--json` has `roots` with every root `completed` and `files_skipped` > 0 or the resumed total the one-source resume test asserts (read `crates/flux-cli/tests/` for its key); the destination equals the sources byte for byte; no `*.flux-lock` file is left.
  - `a_resume_with_a_changed_source_list_is_refused_naming_restart`: after the kill, resume with the two sources swapped: exit 3, stderr contains `INCOMPATIBLE_STATE` and `--restart` and the text `root `.
  - `a_resume_with_no_prior_run_copies_normally`: `--resume` on a fresh DEST exits 0.
  - `--help` mentions that all four flags work with one or several sources (assert on the `Copy` doc sentence).
- [ ] **Step 2: Run to verify they fail.** `cargo nextest run -p flux-cli --no-fail-fast -E 'test(several_sources)'` Expected: the new ones fail with the usage-error text.
- [ ] **Step 3: Implement**: delete `several_flag_gate` and its call in `copy_several`; rewrite the `Copy` doc sentence to say `--restart`, `--break-lock`, `--dry-run` and `--resume` work with one or several sources; keep the help of `--resume-verify` unchanged. Leave any now-unused import for clippy to name and remove it.
- [ ] **Step 4: Run** the same command. Expected: pass.
- [ ] **Step 5: `just check`, then commit** `git add -A crates/flux-cli && git commit -m "feat: flux copy --resume accepts several sources"` with the trailer.

---

### Task 4: the dry resume preview

**Files:**
- Modify: `crates/flux-core/src/run/mod.rs` (`ResumePreview` type about 1281; `dry_start` and its doc about 1296-1380; the `dry_tree` closure about 1427; the `dry_file` closure about 1579), `crates/flux-core/src/run/dry_prior.rs` (`dry_adopt` about 267, `adopt_on_copy` about 287), `crates/flux-core/src/run/multi.rs` (`dry_trees` about 918-1010)
- Test: the module with `dry_cfg`, `crashed_ab_dry`, `dry_restart_cfg` (about 2783, 3357)

**Interfaces:**
- Consumes: Task 2's fixtures; `MultiPlace::dry_create_preview(&self, dry: &mut DryRun, warnings) -> Result<(), RunError>` (cut 18); `validate` (Task 1).
- Produces: `type ResumePreview<'a> = dyn FnMut(&PriorOp, &mut DryRun, &mut Vec<RunWarning>) -> Result<DryPrior, RunError> + 'a;`; `dry_adopt<F>(fs, place: &TreePlace<'_, F>, roots: &[Root], prior, opts, temp_root, warnings) -> DryAdopt<F::Dir>` and the same extra parameter on `adopt_on_copy` (seven parameters: stay at or under clippy's limit).

Behaviour (spec 4.6). `dry_start`'s resume arm calls `resume_preview(&scan.resumable[0], &mut dry, warnings)`; rewrite its doc and the `CreatePreview` doc so neither says "never a resume". The `dry_tree` and `dry_file` closures take and ignore the new parameter; `dry_tree` passes `&place_ref.roots()` to `dry_adopt`. In `dry_trees` move `temp_root` and the parked `adopted` store above `dry_start`; its resume closure runs, in order: `validate(prior, &Place::roots(&place), opts, warnings)`; `place.dry_create_preview(dry, warnings)`; then `dry_adopt(fs, &place.tree, &Place::roots(&place), prior, opts, &temp_root, warnings)`, parking the store as `dry_tree` does. After `dry_start`, the walk's `Shared` uses the parked store, `resume: store.is_some()`, `dry_prior: Some(id)`, `dry_partials: Some(&partials)` and sets `dry.partials_present`, as `dry_tree` does (`run/mod.rs` about 1455-1535). Delete the refusing closure and the comment above it (about 938).

- [ ] **Step 1: Write the failing tests** (a `dry_resume_cfg` helper: `RunConfig { operation_id: format!("{:032x}", 0x22), resume: true, ..dry_cfg() }`, over `crashed_ab_dry()` and a variant crashed at `b/k`):
  - `a_dry_resume_agrees_with_the_real_resume_and_changes_nothing`: run the dry resume on a clone-equivalent fake (build twice with the same crash); `r.run.stop` is none; `dry.prior` is `Some(DryPrior::Resume { operation_id: ID, claims, .. })` with `claims >= 1`; then the REAL resume on the second fake: the dry run's would-skip count equals the real `files_resumed`, and `crate::run::tests::forbidden_in(&calls(&fs)[base..])` holds on the dry fake (no write, no lock).
  - `a_dry_resume_refuses_in_the_real_order`: (a) a changed list: `IncompatibleState` and the `dry` flags show NO prefix-lock note (compat came first); (b) a wrong-kind target plus a readable prefix lock: `DestinationNamespaceCollision` after the `DryRunPrefixLocks` note; (c) a torn store: `StateCorrupt` and `DryRunStoreTorn` warning, with the aggregated prefix note already present.
  - `a_dry_resume_with_no_prior_plans_fresh`: `DryPrior::StartedNew`, lock preview as cut 18.
  - `a_dry_resume_removes_its_temporary_store_copy`: the temp root is empty afterwards (use `dry_temp_root` as the one-source test does).
- [ ] **Step 2: Run to verify they fail.** `cargo nextest run -p flux-core --no-fail-fast -E 'test(run::multi)'` Expected: the new tests fail with the "not supported" refusal.
- [ ] **Step 3: Implement** as above.
- [ ] **Step 4: Run** the filter, then the one-source dry tests: `cargo nextest run -p flux-core --no-fail-fast -E 'test(run::tests)'`. Expected: all pass (the one-source behaviour is unchanged). Mutant: call `dry_create_preview` after `dry_adopt`; `..in_the_real_order` must go red.
- [ ] **Step 5: `just check`, then commit** `git add -A crates/flux-core && git commit -m "feat: dry-run preview of a several-source resume"` with the trailer.

---

### Task 5: the dry resume in the CLI, and the documents

**Files:**
- Modify: `crates/flux-cli/src/main.rs` (probably NO change: `copy_several` delegates to `tree_output` at about 711, which already feeds `report::unprobed_keys(d, args.resume)` and `report::dry_extra_lines(d, outcome.files_resumed)` at about 592 and 631; edit only if the new test shows a missing line or key); `ROADMAP.md` (Phase 2 and Phase 8 rows), `TODO.md` (the `--resume` several-sources item at about 1134 and the L1 mentions at about 1127), `docs/accepted-boundaries.md` (L1 at about 106; add B4 of the spec), `docs/superpowers/specs/2026-10-27-several-restart-design.md` and `docs/superpowers/specs/2026-10-28-several-dry-run-design.md` (a dated note under their N1), `README.md` only if it states a several-source `--resume` limit (it did not at the time of writing: grep before editing).
- Test: `crates/flux-cli/tests/several_sources.rs`

**Interfaces:** Consumes Tasks 3 and 4. Produces the documented state: `--resume` works with several sources.

- [ ] **Step 1: Write the failing tests** (Linux-only, kill harness): `a_dry_resume_over_a_killed_run_prints_the_plan_and_changes_nothing`: after `Stalled::...kill()`, `flux copy --dry-run --resume` exits 0, stdout/stderr show the one-source dry resume's resume lines (`dry_extra_lines`) and the per-root `would copy` lines, `--json` has `dry_run: true`, `roots`, and `unprobed` including `owner_liveness`; the destination tree and every lock file are byte-identical before and after (snapshot the directory listing and bytes). `a_dry_resume_with_a_changed_list_exits_3_naming_restart`.
- [ ] **Step 2: Run to verify they fail.** `cargo nextest run -p flux-cli --no-fail-fast -E 'test(several_sources)'` Expected: the new tests fail on the missing resume lines or keys.
- [ ] **Step 3: Implement** the CLI wiring, then edit the documents: ROADMAP says `--resume` and `--dry-run --resume` over several sources are done (cut 19); TODO closes L1's resume part and adds the `.broken` residue note (B4); accepted-boundaries L1 is rewritten to say an aborted several-source run is resumed with the same source list, and gains B4; the two older specs get `Update 2026-10-28 (cut 19): ...` lines.
- [ ] **Step 4: Run** the filter. Expected: pass. Then `just check` exit 0.
- [ ] **Step 5: Commit** `git add -A && git commit -m "feat: dry resume over several sources in the CLI; documents"` with the trailer (check `git status --short` first: nothing unrelated is staged).

---

## Self-review (run at drafting)

- Spec coverage: G1 and G2 (Task 2 tests 1, 3), G3 (Task 1, Task 3 test 2), G4 (Task 4, Task 5), G5 (Task 2 test 4, 5, 6). 4.1 (Task 3, 5), 4.2 (Task 1), 4.3 and 4.4 (Task 2), 4.5 (Task 2: nothing to change in `drive`, verified by the first test; counters in Task 3), 4.6 (Task 4), docs (Task 5), B1 to B4 (Task 5 documents). Spec T1..T15 map to: T1 Task 2 test 1; T2 Task 2 test 1 (lock files empty); T3 Task 1; T4 Task 2 test 4; T5 Task 2 test 3; T6 Task 2 test 5; T7 Task 2 test 6; T8 to T11 Task 4; T12 to T15 Tasks 3 and 5.
- Type consistency: `ResumePreview`, `dry_adopt(..., roots, ...)`, `validate` keep the names used across Tasks 1, 2, 4. `Place::roots(&place)` is spelled out because `MultiPlace` also has a field `roots`.
- Known unverified at drafting (each has a STOP clause or a test that decides): that the keyed walk resumes file roots and folder roots without a `tree.rs` change (Task 2 test 1); the `--json` key for resumed totals (Task 3, found in the one-source resume CLI test).

## Stand-downs
- Plan panel round 1: LI-1 (the lock op name is `create_lock`), the test-name citation and the `main.rs` wiring note were measured true and FOLDED above. The other five seats: no new findings.
