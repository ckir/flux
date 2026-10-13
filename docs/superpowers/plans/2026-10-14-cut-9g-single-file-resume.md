# Cut 9g: single-file resume Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy SRC DST --resume` for a single file continues a half-copied file from its proven prefix, with no log and no new file beside the target.

**Architecture:** A second checkpoint-context kind, `Hook::LogFree`, lets `stage_file`/`plan_resume` run the existing `decide_full` proof with no WAL and no `FileSink`. `run::file` passes it only for an adopted prior operation whose source is at least `min_len`, and surfaces the reuse on a new `Run.file_resume`; the CLI prints it with the level line.

**Tech Stack:** Rust workspace (`flux-core`, `flux-cli`, `flux-fs`); `FaultFs` fakes for unit tests; real-process e2e tests in `crates/flux-cli/tests/run.rs`.

**Spec:** `docs/superpowers/specs/2026-10-13-cut-9g-single-file-resume-design.md` (owner-approved 2026-10-14; panel GREEN). Built on 9f-1/9f-2 designs under `docs/superpowers/specs/`. Every code citation below was grep-verified at main `6d946ff` plus the docs commits on branch `docs/9f3-scope-closure`.

## Global Constraints
- Central safety property: `--resume` never publishes a byte not proven equal to the source; nothing is silently discarded (spec section 1).
- Single-file layout stays exactly `target`, `target.flux-partial.<id>`, `target.flux-state.<id>`, `target.flux-lock` (spec section 2; FLUX spec section 119). No new file, no manifest/state change.
- `--resume-verify=metadata` keeps its exit 2 refusal, checked first in `copy()` (unchanged).
- `chunks` and unflagged run as `full` on a single file and the level line says so (spec 3.2, 3.3).
- A missing partial reports nothing (no `PartialMissing`): there is no log (spec 3.4).
- Gate for every task: `just check` exits 0 and `cargo clippy --workspace --all-targets --release -- -D warnings` is clean. The pre-existing suite (1423 passed, 5 skipped at `8e2c2be`) passes unchanged except the tests this plan names.
- Commit trailer: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. No `git checkout/switch/stash/reset`. Never commit `.clavity/` or `.superpowers/`.

## Rulings made while planning (spec deviations, ledgered at merge in spec section 9)
- **P1.** `Hook::LogFree` carries only `min_len`, not `durability` (spec 4.1 listed both): with no sink the durability is never read, and an unread field warns.
- **P2.** `run::file` passes `LogFree` only when the source length is at least `min_len` (one extra `fs.metadata(src)`), not merely when a prior was adopted. Reason: below `min_len` the old path swept the partial BEFORE the claim; `LogFree` would move that sweep after it and shift the e2e stall indices that `a_killed_single_file_copy_resumes_keeping_its_id_and_bumping_its_generation` (`crates/flux-cli/tests/run.rs:1002`) pins. A source that changes size between the metadata read and the copy is safe both ways: if it shrinks below `min_len`, `stage_file`'s own filter takes the fresh path and sweeps; if it grows, the old path ran. That test is the oracle for this ruling and must pass UNCHANGED (except the one added assertion in Task 4).
- **P3.** `FileResume` carries `recopied` (the bytes the run copied) as well as `reused` and `discard`, so the CLI prints the tree's `resumed X: reused N bytes, recopied M bytes` line.

## Review Focus
1. A partial with a differing tail (torn or garbage bytes): kept to the last equal chunk, reported `MismatchAt`. Tests: Task 1 `log_free_reports_a_differing_unrecorded_tail`; Task 2 `a_single_file_resume_reports_a_mismatch_inside_the_partial`.
2. A partial longer than the source: truncated to the source length, published equal. Task 1 `log_free_with_a_partial_longer_than_the_source_proves_the_whole_source`.
3. A zero-length partial and a source that changed since the crash: recopied, reported. Task 1 `log_free_with_an_empty_partial_recopies_and_reports_it_shorter`.
4. A heartbeat or lock loss during the hash: the partial is untouched. Task 1 `log_free_heartbeat_failure_leaves_the_partial_untouched`.
5. A resume killed again, then resumed a third time: Task 4 `a_single_file_resume_killed_again_resumes_a_third_time`.

---

## File Structure
- `crates/flux-core/src/checkpoint/mod.rs`: the `Hook` enum (beside `FileCkpt`, line 166).
- `crates/flux-core/src/copy.rs`: `plan_resume` (564), `stage_file` (651), `copy_file_checkpointed` (1005) take a `Hook`; unit tests.
- `crates/flux-core/src/tree.rs`: the three production callers wrap `fc` in `Hook::Logged`.
- `crates/flux-core/src/run/mod.rs`: `Run.file_resume`, `FileResume`, `file_with`, wiring.
- `crates/flux-core/src/run/tests.rs`: run-level single-file resume tests.
- `crates/flux-cli/src/report.rs`, `main.rs`: lines, level line, JSON, help text.
- `crates/flux-cli/tests/run.rs`: real-process e2e.
- `TODO.md`, spec section 9: docs.

---

### Task 1: `Hook` and the log-free path in `copy.rs`

**Files:**
- Modify: `crates/flux-core/src/checkpoint/mod.rs` (add `Hook` after `FileCkpt`, line 166-170).
- Modify: `crates/flux-core/src/copy.rs`: `plan_resume` (564-634), `stage_file` (651; uses of `ckpt` at 672, 726-728, 752, 767), `copy_file_checkpointed` (1005-1030), and the test call sites listed below.
- Modify: `crates/flux-core/src/tree.rs`: `copy_file_checkpointed` callers (1283 passing `fc.as_ref()` at 1293; 1390 passing `fc.as_ref()` at 1400) and the `stage_file` caller (1490 passing `fc` at 1506).
- Test: `crates/flux-core/src/copy.rs` `mod tests`.

**FILES clause (the compiler will name every one; check both axes after):** `checkpoint/mod.rs`, `copy.rs` (production plus test call sites at 1365, 1579, 3372, 3625, 3887 passing `Some(&fc)`, and 1266 passing `None`, which needs no change), `tree.rs`. Touch nothing else.

**Interfaces:**
- Produces, in `checkpoint/mod.rs`:
```rust
/// Cut 9g: what a file checkpoints through.
pub(crate) enum Hook<'r, 'a, D: DirHandle + Send + 'static> {
    /// A WAL-backed context (tree files, cuts 9f-1/9f-2).
    Logged(&'r FileCkpt<'a, D>),
    /// No log (a single file). `plan_resume` runs as `full`; no `FileSink` exists.
    LogFree { min_len: u64 },
}
```
  `Hook` is `Copy`/`Clone` by MANUAL impls (`*self`), with no `D: Clone` bound. `impl Hook { pub(crate) fn min_len(&self) -> u64 }` returns `fc.run.min_len()` or the field.
- `stage_file` and `copy_file_checkpointed` take `ckpt: Option<Hook<'_, '_, F::Dir>>` in place of `Option<&FileCkpt<'_, F::Dir>>`. `copy_file_guarded` is unchanged (it passes `None`).
- `plan_resume`'s first parameter becomes `hook: Hook<'_, '_, D>`.

**Behaviour (the contract; the code is the implementer's):**
- `plan_resume`: `Logged` is exactly today's behaviour. `LogFree` behaves as the `Full` arm with no log: it never calls `load`; with no partial it returns `fresh(None)` (no `PartialMissing`).
- `stage_file`: `LogFree` skips the early sweep (the existing `ckpt.is_none()` test already does, because `ckpt` is `Some`), still sweeps on `Start::Fresh` (line 752), and creates NO `FileSink` (the block at 767 matches `Some(Hook::Logged(fc))` only; `proven` is then ignored). The min_len filter at 726 and 767 uses `Hook::min_len()`.

- [ ] **Step 0: STATE-VERIFICATION.** Open the files above and confirm the line numbers and the signatures quoted here still match. If any differs materially, STOP and report `STATE_MISMATCH: <what>`.
- [ ] **Step 1: Write the failing tests** in `copy.rs` `mod tests`, after `full_keeps_a_partial_of_exactly_one_chunk`. First a helper beside `stage_full` (line 3365), modelled on it:
```rust
fn stage_log_free(
    fs: &FaultFs,
    root: &Dir,
    durability: Durability,
    beat: &Heartbeat<'_>,
) -> StageResult {
    let mut o = opts();
    o.durability = durability;
    stage_file(
        fs,
        Path::new("/src"),
        root,
        OsStr::new("dst"),
        &o,
        &unguarded,
        beat,
        &no_before_create,
        SyncAt::Staging,
        Some(Hook::LogFree { min_len: MIB as u64 }),
    )
}
```
  Tests (`ckpt_fs(n)` gives `/src` of `(i % 251) as u8` bytes; `PARTIAL` is `/dst.flux-partial.op1`; the helper's `opts()` operation id makes that name; each test builds its own `FaultFs`):
  1. `log_free_resumes_the_equal_prefix_of_a_partial_with_no_log`: `ckpt_fs(5 * MIB)`, partial = `src[..3 * MIB]`; `staged_of(stage_log_free(..Normal..))`; assert `reused == 3 * MIB`, `bytes_copied == 2 * MIB`, `discard == Some(Discard::PartialShorter { kept: 3 * MIB as u64 })`, `read_file(PARTIAL) == src`, and that `fs.calls()` contains no call string containing `"wal"` and no `create_new(` after the mark taken before staging. Mutant: `plan_resume` calls `load` for `LogFree` (returns early with no log, so `reused == 0`).
  2. `log_free_with_no_partial_is_a_fresh_copy_that_reports_nothing`: no partial; `reused == 0`, `discard == None`, `create_new(PARTIAL)` called. Mutant: `PartialMissing` reported.
  3. `log_free_reports_a_differing_unrecorded_tail`: partial = `src[..3 * MIB]` plus `MIB + MIB / 2` bytes of `0xEE`; assert `discard == Some(Discard::MismatchAt(3 * MIB as u64))`, `reused == 3 * MIB`, `read_file(PARTIAL) == src`. Mutant: the partial is trusted past a mismatch.
  4. `log_free_with_a_partial_longer_than_the_source_proves_the_whole_source`: source `3 * MIB + 12345`, partial = source plus `2 * MIB` of `0xEE`; `reused == len`, `bytes_copied == 0`, `discard == None`, `truncate_to(PARTIAL, len)` called, and `read_file(PARTIAL)` equals the source.
  5. `log_free_with_an_empty_partial_recopies_and_reports_it_shorter`: partial of length 0 at `PARTIAL`; `reused == 0`, `discard == Some(Discard::PartialShorter { kept: 0 })`.
  6. `log_free_below_min_len_recopies_without_reading_the_partial`: `ckpt_fs(MIB / 2)`, a 4-byte partial; `reused == 0`, `discard == None`; `fs.bytes_read(PARTIAL) == 0`. Mutant: the min_len filter at 726 removed (the partial is read).
  7. `log_free_heartbeat_failure_leaves_the_partial_untouched`: `ckpt_fs(5 * MIB)`, partial `src[..3 * MIB]`; a heartbeat closure that fails on its third call (the first is `stage_file`'s own, the second is `decide_full`'s first chunk, so the failure lands after one chunk was hashed and before any mutation); the result is `Err` with `step == CopyStep::Heartbeat`; the partial still has length `3 * MIB` and equals `src[..3 * MIB]`; no `truncate_to(` call after the mark.
  8. `log_free_under_strict_syncs_the_partial_before_publish`: as 1 under `Durability::Strict`; `sync_all(PARTIAL)` is among `fs.calls()` after the mark.
  Also update the five existing `Some(&fc)` test call sites mechanically to `Some(Hook::Logged(&fc))`.
- [ ] **Step 2: Run to verify they fail** (compile error: `Hook` undefined): `cargo nextest run -p flux-core --no-fail-fast log_free_` — expected: build failure naming `Hook`.
- [ ] **Step 3: Implement** `Hook`, its `Copy`/`Clone`, `min_len()`; change the three signatures; in `plan_resume` derive `level` from the hook (`LogFree` is `Full`) and make the two `fc.run.load(..)` uses reachable only for `Logged`; in `stage_file` match the sink block on `Hook::Logged`; wrap the production callers in `tree.rs` with `fc.as_ref().map(Hook::Logged)` (and `fc` at 1506 likewise). `Hook` is `pub(crate)`; re-export it where `FileCkpt` is imported by `copy.rs` and `tree.rs`.
- [ ] **Step 4: Run** `cargo nextest run -p flux-core --no-fail-fast log_free_` then `just check`. Expected: the eight new tests pass; `just check` exit 0 with the whole pre-existing suite unchanged.
- [ ] **Step 5: Prove non-vacuousness.** Temporary logic mutants, each reverted after: (a) `LogFree` treated as `Chunks` in `plan_resume`; (b) `stage_file` builds a sink for `LogFree` (any `Err`/WAL call shows in test 1's `"wal"` assertion); (c) the early sweep removed only for `Logged` (so `LogFree` sweeps first). Report which NEW test went red for each. A mutant no new test catches is a finding, not a pass.
- [ ] **Step 6: Commit** `feat(copy): a log-free resume hook for single files (cut 9g)` with the trailer.

**ORACLES:** spec 3.1-3.4, 4.1, 4.4 and 5; the existing 9f-2 tests `full_*` in `copy.rs` define `Logged`'s unchanged behaviour. If a test and the code disagree, surface it; do not edit the test to match.
**SHAPE-DIVERGENCE:** if the `Hook` lifetimes or bounds cannot be written as given, STOP and report `[spec shape] -> [yours] because <reason>`.

---

### Task 2: `run::file` wiring and `Run.file_resume`

**Files:**
- Modify: `crates/flux-core/src/run/mod.rs`: `Run<T>` (101), `tree_with` literal (235), `file` (474-575), new `FileResume`, new `file_with`.
- Modify: `crates/flux-cli/src/exit_code.rs` (literals at 214, 232) and `crates/flux-cli/src/report.rs` tests (literals at 1162, 1176, 1188, 1220): add `file_resume: None`.
- Test: `crates/flux-core/src/run/tests.rs`, in `mod checkpointing` (helpers `big_fs`, `policy`, `lazy`, `pattern`; the outer `file_prior3`, `resume()`, `id`, `opts`, `fake`).

**FILES clause:** `run/mod.rs`, `run/tests.rs`, `exit_code.rs`, `report.rs` (struct-literal fixes only, in this task). `benches/copy.rs:125` and `tests/integration/support/mod.rs:51` call `run::file(..)` and need no change because its signature is kept; confirm with `cargo check --workspace --all-targets`.

**Interfaces:**
- Consumes: Task 1's `Hook`, `copy_file_checkpointed(.., Some(Hook::LogFree { min_len }))` returning `(Outcome, Reuse)`.
- Produces:
```rust
/// Cut 9g: what a single-file resume reused and discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResume {
    pub reused: u64,
    pub recopied: u64,
    pub discard: Option<crate::Discard>,
}
// Run<T> gains:  pub file_resume: Option<FileResume>,
pub fn file<F: DestinationRoot>(/* unchanged */) -> Run<Result<Outcome, CopyError>>;      // delegates
pub(crate) fn file_with<F: DestinationRoot>(
    fs: &F, src: &Path, dst: &Path, opts: &CopyOptions, cfg: &RunConfig, policy: CheckpointPolicy,
) -> Run<Result<Outcome, CopyError>>;
```
  `file` calls `file_with(.., checkpoint_policy(cfg, opts.durability))`, as `tree` calls `tree_with`.

**Behaviour:** In `file_with`, after `open_operation` and the `opts` rebinding: `log_free` is true iff `locked.resumed` is `Some(ResumeNote::Adopted { .. })` AND `fs.metadata(src)` is `Ok(m)` with `m.len >= policy.min_len` (ruling P2). If `log_free`, call `copy_file_checkpointed(.., Some(Hook::LogFree { min_len: policy.min_len }))` and map `(outcome, reuse)` to `outcome`, setting `run.file_resume = Some(FileResume { reused: reuse.reused, recopied: outcome.bytes_copied, discard: reuse.discard })` ONLY when `reuse.reused > 0 || reuse.discard.is_some()`; otherwise call `copy_file_guarded` exactly as today. The `.map_err` leftover rewriting and the `Ended` logic are unchanged. `file_resume` stays `None` for every non-adopted run and on `Err`.

- [ ] **Step 0: STATE-VERIFICATION** as Task 1 (`run/mod.rs:101, 235, 474-575`, `session.rs:187-209`).
- [ ] **Step 1: Write the failing tests** in `run/tests.rs` `mod checkpointing`. Fixture: `fake()` plus `/src/a` = `pattern(5 * MIB)`, `file_prior3(&fs, 5, None)`, a partial at `format!("/p/t.flux-partial.{}", id(5))`; call `file_with(&fs, Path::new("/src/a"), Path::new("/p/t"), &opts(), &resume(), lazy(Durability::Normal))` (`resume()` is the existing single-file resume config used at `run/tests.rs:3268`; if the nested module cannot see it, import via `super::`):
  1. `a_single_file_resume_keeps_the_proven_prefix_of_its_partial`: partial = `pattern(5 * MIB)[..3 * MIB]`; assert `Ok`, `r.file_resume == Some(FileResume { reused: 3 * MIB as u64, recopied: 2 * MIB as u64, discard: Some(crate::Discard::PartialShorter { kept: 3 * MIB as u64 }) })`, and `fs.read_file("/p/t")` equals the source. Mutant: `file_with` always calls `copy_file_guarded`.
  2. `a_single_file_resume_reports_a_mismatch_inside_the_partial`: partial = 3 MiB prefix with one byte flipped at `2 * MIB + 5`; `discard == Some(Discard::MismatchAt(2 * MIB as u64))`, `reused == 2 * MIB`, published equal to source.
  3. `a_single_file_resume_with_no_partial_reports_nothing`: no partial file; `r.file_resume == None`; published equal.
  4. `a_single_file_resume_below_min_len_reports_nothing`: source `pattern(MIB / 2)`, a 4-byte partial; `file_resume == None`; published equal; the partial is gone.
  5. `a_run_that_adopted_nothing_reports_no_file_resume`: `resume()` with no prior record (`StartedNew`) and a partial under another id present: `file_resume == None` and that other partial untouched. Mutant: `log_free` ignores adoption.
  6. `a_fresh_single_file_run_never_reads_a_partial`: the non-resume `cfg()` with a partial under the prior id: `file_resume == None`, `fs.bytes_read` of that partial is 0.
  7. `a_single_file_resume_under_strict_publishes_equal_bytes`: as 1 with `Durability::Strict` policy and options; the published file equals the source.
- [ ] **Step 2: Run to verify they fail**: `cargo nextest run -p flux-core --no-fail-fast a_single_file_resume a_run_that_adopted a_fresh_single_file` — expected: compile failure (`file_with`, `FileResume`, `file_resume` undefined).
- [ ] **Step 3: Implement** as specified; add `file_resume: None` at every `Run { .. }` literal the compiler names.
- [ ] **Step 4: Run** the new tests, then `just check` and `cargo check --workspace --all-targets`. Expected: new tests pass; whole suite unchanged.
- [ ] **Step 5: Non-vacuousness.** Mutants: (a) `log_free` drops the `Adopted` test; (b) drops the `>= min_len` test (ruling P2: expect `a_killed_single_file_copy_resumes_keeping_its_id_and_bumping_its_generation` in `crates/flux-cli/tests/run.rs` to go red in Task 4's gate run; if it does NOT, report that the size gate is unproven by any test); (c) `recopied` set to `reuse.reused`. Name the test that goes red for each.
- [ ] **Step 6: Commit** `feat(run): a single-file resume continues its proven prefix (cut 9g)`.

**ORACLES:** spec 3.1, 3.3, 3.4, 3.5, 4.2; `session.rs:187-209` for adoption.

---

### Task 3: CLI lines, level line, JSON field, help text

**Files:**
- Modify: `crates/flux-cli/src/report.rs`: `file_run` (141) sets `resume_verify`; new `file_checkpoint_lines`, `file_resume_verify_lines`; extract the `Discard` to text `match` of `checkpoint_lines` (427-452) into a shared private `fn discard_why(&Discard) -> String` so both builders print one wording.
- Modify: `crates/flux-cli/src/main.rs`: the `Job::File` arm (329-362) and the `--resume` doc comment (line 85).
- Test: `report.rs` `mod tests` (the existing `checkpoint_lines_name_each_reuse_and_each_discard`, line 977, is the model).

**Interfaces:**
```rust
pub fn file_checkpoint_lines(run: &Run<Result<Outcome, CopyError>>, target: &str, normal_durability: bool) -> Vec<String>;
pub fn file_resume_verify_lines(run: &Run<Result<Outcome, CopyError>>) -> Vec<String>;
```
`file_checkpoint_lines` mirrors `checkpoint_lines`: for `Some(FileResume { reused, recopied, discard })`: a `resumed {target}: reused {reused} bytes, recopied {recopied} bytes` line when `reused > 0`; a `checkpoints discarded for {target}: {why}` line when `discard` is `Some`; the closing `note: --durability=normal checkpoints are advisory; ...` when `normal_durability` and either line printed. `None` prints nothing. `file_resume_verify_lines` prints the existing level-line text iff `file_resume.is_some()`. `Report.resume_verify` is `Some("full")` iff `file_resume.is_some()`.

- [ ] **Step 0: STATE-VERIFICATION** (`report.rs:141, 394-470, 977`; `main.rs:85, 329-362`).
- [ ] **Step 1: Write the failing tests** (report.rs `mod tests`): `file_checkpoint_lines_name_the_reuse_and_the_discard` (a `FileResume { reused: 3145728, recopied: 2097152, discard: Some(Discard::PartialShorter { kept: 3145728 }) }` yields exactly the three strings, the first being `resumed t: reused 3145728 bytes, recopied 2097152 bytes`, the second `checkpoints discarded for t: partial shorter than its checkpoints (kept 3145728 bytes)`, the third the note, when `normal_durability`); `file_checkpoint_lines_print_nothing_without_a_file_resume`; `the_level_line_and_json_field_appear_exactly_when_a_single_file_resume_ran` (level line present and `Report::file_run(..).resume_verify == Some("full")` for `Some(..)`; absent and `None` otherwise); and one test that a `Discard::MismatchAt(7)` prints `mismatch at offset 7`. Build the `Run` through the existing literal helpers in `report.rs` tests (add `file_resume`).
- [ ] **Step 2: Run to verify they fail**: `cargo nextest run -p flux-cli --no-fail-fast file_checkpoint_lines the_level_line_and_json_field` — expected: compile failure.
- [ ] **Step 3: Implement.** In `main.rs` print, after `lines(report::resume_lines(&run, 0))`: `lines(report::file_checkpoint_lines(&run, &dst.display().to_string(), opts.durability == Durability::Normal)); lines(report::file_resume_verify_lines(&run));` before the summary line. Replace the help sentence "and a half-copied single file is always recopied" with: "a half-copied single file is validated by re-reading and hashing it against the source (`--resume-verify=chunks` runs as `full` for a single file)". Keep the doc comment's other text byte-identical.
- [ ] **Step 4: Run** the new tests and `just check`. The existing `a_single_file_resume_accepts_full_and_prints_no_level_line` (`run.rs:889`) must still pass UNCHANGED (nothing adopted, so no level line).
- [ ] **Step 5: Non-vacuousness:** mutants — the level line printed for `None`; `resume_verify` set from the flag instead of `file_resume`; the `reused > 0` guard dropped. Name the red test for each.
- [ ] **Step 6: Commit** `feat(cli): report a single-file resume and its level (cut 9g)`.

**ORACLES:** spec 3.2, 3.3, 4.3; the existing tree tests for `checkpoint_lines` define the wording.

---

### Task 4: Real-process e2e

**Files:**
- Modify: `crates/flux-cli/tests/run.rs`: extract the xorshift generator inside `a_killed_large_file_resumes_with` (lines ~611-624) into `fn write_xorshift(path: &Path, len: usize)` with identical output, used by the old test and the new ones; add the tests below; add ONE assertion to `a_killed_single_file_copy_resumes_keeping_its_id_and_bumping_its_generation` (1002): after `let e = stderr(&out);` assert `!e.contains("resume verification")` (a 1-byte file never reaches `min_len`).

**Interfaces:** consumes the `Stalled` helper (`run.rs:47-115`), `names`, `copy`, `TempDir`.

- [ ] **Step 0: STATE-VERIFICATION** (`run.rs:47-115, 590-700, 1002-1050`).
- [ ] **Step 1: Write the failing tests** (each on a real 70 MiB file, the real 64 MiB threshold, no env var):
  1. `a_killed_single_file_copy_resumes_its_validated_prefix`: `write_xorshift(src, 70 MiB)`; `Stalled::start(&src, &t, 3, marks.path()).kill()` (sweep 1, temporary 2, publish 3: the partial is whole); locate `t.flux-partial.*`, truncate it to 40 MiB and append 1 MiB of garbage; run `copy(["--resume", "--json", src, t])`; assert exit 0, stderr contains a `resumed <target>: reused 41943040 bytes, recopied` line (the target is printed as the path the user gave), `checkpoints discarded for`, and `resume verification: full (source and partial re-read and hashed)`, JSON `resume_verify == "full"`, `t` equals the source, no `t.flux-partial.*` left, `names(d)` is `["src", "t"]` plus the marks-free tree.
  2. `an_explicit_chunks_single_file_resume_prints_what_full_prints`: the same fixture twice (two TempDirs, or sequential after restoring the partial) with `--resume-verify chunks` and `--resume-verify full`: stderr (minus the duration/summary elapsed text, as `a_chunks_resume_prints_what_an_unflagged_resume_prints` at `run.rs:852` strips it) and JSON identical.
  3. `a_single_file_resume_killed_again_resumes_a_third_time`: after the setup of 1, `Stalled::start_args(&src, &t, 3, m2.path(), &["--resume"]).kill()`, which stalls in the resume before its publish; then resume again: exit 0, `t` equals the source, one `t.flux-state.*` record only, same operation id throughout.
  4. `a_single_file_resume_with_no_partial_prints_no_level_line`: `--resume` with a prior record killed before its temporary exists (stall at 2): exit 0, no `resume verification` and no `resumed t:` line.
- [ ] **Step 2: Run to verify they fail**: `cargo nextest run -p flux-cli --no-fail-fast a_killed_single_file_copy_resumes_its_validated_prefix` — expected: FAIL (no reuse line).
- [ ] **Step 3: Implement** only test code plus the helper extraction.
- [ ] **Step 4: Run** `cargo nextest run -p flux-cli --no-fail-fast` then `just check`. Expected: new tests pass; every existing e2e test unchanged and green, including `a_killed_single_file_copy_resumes_keeping_its_id_and_bumping_its_generation`.
- [ ] **Step 5: Non-vacuousness:** mutant — `file_with` passes `None` (all four new tests that expect reuse go red); mutant — drop the P2 size gate (report whether `a_killed_single_file_copy_resumes_keeping_its_id_and_bumping_its_generation` goes red; if not, write that in the report).
- [ ] **Step 6: Commit** `test(cli): a killed single file resumes from its proven prefix (cut 9g)`.

**ANOMALIES / TIMING:** the e2e tests are slow (70 MiB); report their wall time per test but do NOT treat it as a measurement.

---

### Task 5: Docs and gate

**Files:** `TODO.md` (the checkpoints entry near line 974: "Still open: checkpoints for the single-file layout" becomes delivered by cut 9g, keeping the closures paragraph); `docs/superpowers/specs/2026-10-13-cut-9g-single-file-resume-design.md` (status line to IMPLEMENTED; section 9 gets rulings P1-P3 and any plan gaps the implementers reported).

- [ ] **Step 1:** edit both; run `just check` and `cargo clippy --workspace --all-targets --release -- -D warnings`. Expected: exit 0 and clean; the test count is 1423 plus the new tests.
- [ ] **Step 2: Commit** `docs: cut 9g shipped; TODO and spec amendments`.

---

## After the plan (not tasks; owner-gated)
Whole-branch Opus review; AGY-CAPSTONE rounds until GREEN, then AGY-TEST-AUDIT (the owner scopes the gaps); owner confirmation; then the timing measurement of a single-file resume on a 4 GiB file (top level only, idle machine, two runs, range quoted, background load stated), and only then a PR on the owner's explicit say-so.

## Self-review
- **Spec coverage:** 3.1 (T1, T2), 3.2 and 3.3 (T3, T4), 3.4 (T1 test 2, T2 test 3, T4 test 4), 3.5 (T2 tests 5-6, existing `run.rs:889`), 4.1 (T1), 4.2 (T2), 4.3 (T2, T3), 4.4 (T4), section 5 (T1 tests 3-5, 7), section 6 cases 1-12 map as: 1 T1.1/T2.1, 2 T3 and T4.2, 3 T1.3/T2.2, 4 T1.4, 5 T1.2/T2.3, 6 T1.6/T2.4 (the `source == min_len` case is covered by a `full_resumes_a_source_of_exactly_min_len` sibling that already exists for `Logged`; add `log_free_resumes_a_source_of_exactly_min_len` if the implementer finds the `>=` filter is exercised only there), 7 T2.5-6 and `run.rs:889`, 8 existing `plan_resume` rules (unchanged code, not re-tested for `LogFree`), 9 T1.7, 10 T4.1, 11 T1.8/T2.7, 12 the whole-suite gate.
- **Known gaps with owner:** case 8 (hard-linked or non-regular partial under `LogFree`) rides on the shared `plan_resume` code before the `Hook` branch; the capstone and test audit decide whether a `LogFree`-specific test is owed. Case 6's exact-`min_len` single-file test is conditional as noted above.
- **Placeholders:** none. Names introduced (`Hook`, `LogFree`, `FileResume`, `file_with`, `file_checkpoint_lines`, `file_resume_verify_lines`, `stage_log_free`, `write_xorshift`) are each defined in one task and used consistently.
