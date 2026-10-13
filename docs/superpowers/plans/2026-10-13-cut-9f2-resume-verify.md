# Cut 9f-2: `--resume-verify` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy --resume --resume-verify=<chunks|full>`: `chunks` is exactly today's both-sides digest proof; `full` proves the partial equal to the source by hashing both sides over the whole partial, needs no WAL, salvages what `chunks` recopies, and re-records the proven prefix so a later `chunks` resume can use it.

**Architecture:** The level rides in `RunConfig` and `CheckpointPolicy` (the run's checkpoint handle), so `plan_resume` reads it from the `FileCkpt` it already has. `full` is a sibling of `decide` in `checkpoint/decide.rs` (pure over `Read`ers, same `Verdict`), reached by `plan_resume` BEFORE the log is loaded; `Verdict` gains the digests `full` proved, which `stage_file` hands to a new `FileSink::record_proven` right after `begin`. The CLI maps the flag, refuses `metadata` before anything runs, and prints a level line plus a `--json` field only when `full` attempted a partial resume. No manifest change, no WAL format change.

**Tech Stack:** Rust (edition 2024), clap `ValueEnum`, BLAKE3, the existing `FaultFs` fake and `nextest`.

**Spec:** `docs/superpowers/specs/2026-10-13-cut-9f2-resume-verify-design.md` (the design, owner-approved 2026-10-13), which argues from `FLUX_FULL_UPDATED_SPEC_V16.md` sections 36 (line 2301), 121 (line 5827), 137 (line 6395). Written against local main `0f64ec2`; every file:line below was read at that commit.

## Global Constraints

- Default level `chunks`; an unflagged `--resume` run's bytes on disk, stderr and `--json` are unchanged (design 3.5, 8b).
- No manifest field, no manifest format bump, no WAL record-format change (design 5, 3.3). `state::Options` has `deny_unknown_fields` (`state.rs:63`), so a new field there is forbidden.
- `full` never consults recorded digests for validity and applies no `FileBegin` gate (design 3.2 step 6). It reads the log at most to decide whether a missing partial is reported (Ruling 1 below).
- A newer-format WAL stops the run `WAL_FORMAT_UNSUPPORTED` under both levels (owner ruling 23, `run/mod.rs:91-93`); `full` must never bypass it.
- `full` runs only where a checkpoint context exists (`ckpt_file` at `copy.rs:696`); without one the file recopies as today and no extra warning is printed (design 8b).
- Every new test is shown red by the logic mutant named beside it (the repo's non-vacuous bar; hand-apply, run, restore). Do not leave a mutant in the tree.
- Gate as the repo runs it: `just check` (= `fmt-check clippy typos test`; `test` = `cargo nextest run --workspace --no-tests=pass` then `cargo test --doc --workspace`). Baseline at `0f64ec2`: 1382 passed, 5 skipped. Also `cargo clippy --workspace --all-targets --release -- -D warnings` (the CI release lane).
- Commits: conventional (`feat(core): …`, `test(cli): …`), each ending with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. Never commit `.clavity/` or `.superpowers/`.
- Plan rulings (made here, not in the design; check them when reading):
  - **Ruling 1.** Under `full`, when no partial exists, `plan_resume` calls `fc.run.load(..)` ONLY to choose the report: `Ok(Some(_))` (the log began the file) reports `Discard::PartialMissing` as `chunks` does; `Ok(None)` or `Err(_)` reports nothing. Why: without this, every large file a resumed tree has not yet started would print `checkpoints discarded for <f>: partial missing` under `full` (the log is the only thing that says a partial should exist). Cost if wrong: one report line's presence; validity is untouched.
  - **Ruling 2.** The proven prefix is re-recorded as one `Range` per `policy.range_chunks` chunks (not one record), because `MAX_PAYLOAD` is 1 MiB (`wal/mod.rs:18`) and a single record of 32768 digests (a 32 GiB file) would be refused as oversized (`writer.rs:488-491`).
  - **Ruling 3.** Under `full`, a partial whose unrecorded tail differs from the source reports `MismatchAt(<that chunk's start>)` (it was compared and found unequal), where `chunks` truncates it silently. Design 3.4 row 2 implies it ("kept when it equals the source"); nothing is silently discarded.

## Review Focus

1. A partial LONGER than the source under `full`: the whole source is proven, nothing is read past the source length, `offset == L`, no discard line (design 8b; test in Task 2 and Task 4).
2. A file above 32 GiB under `full` (more than 32768 proven chunks, 1 MiB of digests): the proven prefix's digests must be split into several `Range` records or the writer refuses the record as oversized (Ruling 2; test in Task 3 with a small `range_chunks`).
3. A heartbeat failure while `full` is still hashing: the copy fails at `CopyStep::Heartbeat` and NOTHING is appended to the log (the sink has not begun yet; test in Task 4).
4. `--resume-verify=full` on a run whose checkpoints could not be opened (`CheckpointsUnavailable`): every large file recopies, the existing warning stands, no level line and no `resume_verify` field (design 8b; test in Task 5, plus Task 6's `resume_verify_lines` unit test on an outcome with no partial resume and no discard, which is exactly what this run produces; the real binary cannot be given an unavailable segment identity).
5. `--resume-verify=metadata` together with `--json`: exit 2, the usage line on stderr, NO JSON object on stdout, nothing created at DEST (design 6; test in Task 6).

---

### Task 1: The level type and its plumbing (`ResumeVerify`)

Tier hint: Sonnet (a struct-shape change across crates; the compiler names every site, the FILES list pre-names them).

**Files:**
- Modify: `crates/flux-core/src/checkpoint/mod.rs:27-41` (`CheckpointPolicy`, `DEFAULT`), `:102-108` (accessors)
- Modify: `crates/flux-core/src/run/mod.rs:50-72` (`RunConfig`), `:77-91` (`Debug`), `:647-664` (both `checkpoint_policy`)
- Modify (every `CheckpointPolicy` literal): `crates/flux-core/src/checkpoint/sink.rs:220` (`:533` spreads `..policy(..)` and needs no edit); `crates/flux-core/src/copy.rs:1287`; `crates/flux-core/src/run/tests.rs:5528`
- Modify (every `RunConfig` literal without `..`): `crates/flux-core/src/run/tests.rs:27`; `crates/flux-core/tests/safety_std_fs.rs:125`; `crates/flux-core/tests/replace_std_fs.rs:25`; `crates/flux-cli/src/main.rs:158` (placeholder `ResumeVerify::Chunks`; Task 6 replaces it)
- Test: `crates/flux-core/src/run/tests.rs` (beside `the_checkpoint_policy_carries_the_runs_durability`, line 5803)

**Interfaces:**
- Produces: `pub enum ResumeVerify { Chunks, Full }` in `checkpoint/mod.rs` (`Debug, Clone, Copy, PartialEq, Eq, Default`; `#[default] Chunks`), re-exported as `pub use crate::checkpoint::ResumeVerify;` from `run/mod.rs` (the CLI's path: `flux_core::run::ResumeVerify`).
- Produces: `CheckpointPolicy.verify: ResumeVerify` (`DEFAULT` = `Chunks`); `Checkpoints::resume_verify(&self) -> ResumeVerify`.
- Produces: `RunConfig.resume_verify: ResumeVerify`; `checkpoint_policy(cfg, durability)` copies it into the policy (both the debug and the release variant; rename `_cfg` to `cfg`).

- [ ] **Step 1: Write the failing test** in `run/tests.rs` next to line 5803:

```rust
// Mutant: `checkpoint_policy` builds the policy with `ResumeVerify::Chunks` whatever `cfg.resume_verify` says.
#[test]
fn the_checkpoint_policy_carries_the_runs_resume_verify() {
    for level in [ResumeVerify::Chunks, ResumeVerify::Full] {
        let c = RunConfig { resume_verify: level, ..cfg() };
        assert_eq!(checkpoint_policy(&c, Durability::Normal).verify, level);
    }
    assert_eq!(CheckpointPolicy::DEFAULT.verify, ResumeVerify::Chunks);
}
```

- [ ] **Step 2: Run it**: `cargo nextest run -p flux-core the_checkpoint_policy_carries_the_runs_resume_verify` — Expected: compile error (no `resume_verify` field).
- [ ] **Step 3: Add the type, the two fields, the accessor and the re-export**; update every literal in the FILES list (`verify: ResumeVerify::Chunks` in tests and `DEFAULT`; `resume_verify: ResumeVerify::Chunks` in every `RunConfig` literal). The `Debug` impl of `RunConfig` gains `.field("resume_verify", &self.resume_verify)`.
- [ ] **Step 4: Run** `cargo nextest run --workspace --no-tests=pass` — Expected: all pass (1383 passed). Apply the mutant (hard-code `Chunks` in `checkpoint_policy`), confirm the test is red, restore.
- [ ] **Step 5: Commit** `feat(core): ResumeVerify level carried by RunConfig and CheckpointPolicy (cut 9f-2)`.

---

### Task 2: `decide_full`, the WAL-free proof

Tier hint: Sonnet (a well-specified pure function with a complete test list; the oracle is design 3.2 and `decide` at `decide.rs:64-128`).

**Files:**
- Modify: `crates/flux-core/src/checkpoint/decide.rs` (`Verdict` at `:39-44`; its literals at `:72`, `:121`, `:127`; new fn after `decide`)
- Modify: `crates/flux-core/src/checkpoint/mod.rs:19` (`pub(crate) use decide::{..., decide_full}`)
- Modify (every `Verdict` literal): `crates/flux-core/src/copy.rs:564`, `:699`, and the destructuring at `:701`
- Test: `crates/flux-core/src/checkpoint/decide.rs` `mod tests` (helpers `source_bytes`, `digests_of`, `counting`, `Counting`, `assert_positioned`, `ok_beat` exist at `:143-224`)

**Interfaces:**
- Produces: `Verdict.proven: Vec<Digest>` — the BLAKE3 digests of chunks `0..offset.div_ceil(CHUNK_SIZE)` for a `full` verdict; ALWAYS empty from `decide` (`chunks`) and from every `fresh(..)`.
- Produces:
  ```rust
  pub(crate) fn decide_full<S: Read, P: Read>(
      source_len: u64,
      partial_len: u64,
      source: &mut S,
      partial: &mut P,
      beat: &Heartbeat<'_>,
  ) -> Result<Verdict, FsError>
  ```
  Algorithm (design 3.2; reuse `read_full` and `hash`): for `k = 0..` while `start = k*CHUNK_SIZE < source_len`, `len = min(CHUNK_SIZE, source_len - start)`: if `partial_len < start + len` → stop with `PartialShorter { kept: pos }` (nothing more read); else `beat()?`, read `len` from the source (`Err` → return `Err`, ruling 5), read `len` from the partial (an error or a short read = not proven); proven iff both supplied `len` bytes and the two hashes are equal; on not proven return `MismatchAt(start)` with `carry = source bytes read` and `start = Fresh` if `k == 0` else `Resume { offset: start }`; on proven push the digest to `proven`, `pos = start + len`. After the loop: `Resume { offset: pos }` (or `Fresh` at 0), `carry` empty, discard `PartialShorter` only if the loop stopped on the partial's length. The source reader is left at `offset + carry.len()` (same contract as `decide`).

- [ ] **Step 1: Write the failing tests** (one `#[test]` each, names exact; `MIB`, `source_bytes` as in the file; a `run_full(len, source, partial)` helper mirroring `run`):
  - `full_proves_an_equal_partial_to_the_end` — src 5 MiB, partial == src: `Resume { offset: 5 MiB }`, `carry` empty, `discard None`, `proven == digests_of(&src)` values in order. Also partial = src + 3 bytes: identical verdict and the source counter read exactly 5 MiB (Review Focus 1). Mutant: `proven` pushed only for `k > 0`.
  - `full_resumes_at_the_chunk_start_before_a_differing_byte` — src 4 MiB + 12345, three partials: byte 10 flipped → `Fresh`, `MismatchAt(0)`, `carry == src[..MIB]`, `proven` empty; byte `2*MIB + 10` → `Resume { offset: 2 MiB }`, `MismatchAt(2 MiB)`, `carry == src[2 MiB..3 MiB]`, `proven.len() == 2`; byte `4*MIB + 100` → `Resume { offset: 4 MiB }`, `MismatchAt(4 MiB)`, `carry == src[4 MiB..]`, `proven.len() == 4`. Each `assert_positioned`. Mutant: `==` → `!=` on the hash comparison (every case goes red).
  - `full_a_source_that_ends_inside_a_chunk_is_a_mismatch_with_the_short_carry` — `source_len` 3 MiB, the source reader has only 2 MiB + 100 bytes, partial 3 MiB: `Resume { offset: 2 MiB }`, `MismatchAt(2 MiB)`, `carry.len() == 100`. Mutant: `got == len` dropped from the proof.
  - `full_a_partial_read_error_stops_validation_at_that_chunk` — a `Read` that fails at chunk 1: `Resume { offset: MIB }`, `MismatchAt(MIB)`, `carry == src[MIB..2 MIB]`. Mutant: a partial read error treated as proven.
  - `full_a_shorter_partial_keeps_the_chunks_that_fit_and_reads_no_source_past_them` — src 5 MiB, partial `src[..2 MiB + MIB/2]`: `Resume { offset: 2 MiB }`, `PartialShorter { kept: 2 MiB }`, `carry` empty, `proven.len() == 2`, source counter == 2 MiB. A zero-length partial: `Fresh`, `PartialShorter { kept: 0 }`, source counter 0. A partial of exactly one chunk: `Resume { offset: MIB }`, `PartialShorter { kept: MIB }`. Mutant: `<` → `<=` in the partial-length stop.
  - `full_the_heartbeat_runs_once_per_chunk_and_its_failure_aborts` — model `the_heartbeat_runs_once_per_chunk_and_its_failure_aborts` (`decide.rs:390`): 5 MiB equal sides → 5 beats; a beat failing on the 3rd call → `Err` with that error. Mutant: the beat moved outside the loop.
  - `full_a_source_read_error_is_an_error` — a source `Read` failing at chunk 1 → `Err(_)`. Mutant: source error treated as a mismatch.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core checkpoint::decide::tests::full_` — Expected: compile errors (`decide_full`, `proven`).
- [ ] **Step 3: Add `proven` to `Verdict`** (empty in every existing literal, including `copy.rs:564` and `:699`; extend the destructuring at `copy.rs:701` with `proven` and `let _ = proven;` for now — Task 4 uses it) and implement `decide_full`.
- [ ] **Step 4: Run** `cargo nextest run -p flux-core` — Expected: all pass. Apply each named mutant in turn, confirm its test is red, restore.
- [ ] **Step 5: Commit** `feat(core): decide_full proves a partial against the source without the WAL (cut 9f-2)`.

---

### Task 3: `FileSink::record_proven`

Tier hint: Sonnet.

**Files:**
- Modify: `crates/flux-core/src/checkpoint/sink.rs:125-154` (beside `send_range` / `wrote`)
- Test: `crates/flux-core/src/checkpoint/sink.rs` `mod tests` (helpers `ws`, `policy`, `open_run`, `fc`, `meta`, `records` at `:212-260`)

**Interfaces:**
- Produces:
  ```rust
  /// Records digests `full` proved for `[0, valid_through)`: one `Range` per `policy.range_chunks` chunks, from offset 0, the last ending at `valid_through`. Call once, right after `begin`, before the first `wrote`.
  pub(crate) fn record_proven(&mut self, digests: &[Digest], beat: &Heartbeat<'_>) -> Result<(), SinkError>
  ```
  Precondition (`debug_assert!`): `digests.len() as u64 == self.range_start.div_ceil(CHUNK_SIZE)` and no bytes fed yet. Each `Range { file_id, attempt_id: ATTEMPT_ID, range_start: g * range_chunks * CHUNK_SIZE, range_end: min((g+1) * range_chunks * CHUNK_SIZE, valid_through), digests: that group }` sent through `send` as `Msg::Range` (the record decoder requires `digests.len() == (end-start).div_ceil(CHUNK_SIZE)`, `record.rs:278-286`). `self.range_start`, `self.hasher`, `self.digests` are unchanged afterwards. An empty slice sends nothing.

- [ ] **Step 1: Write the failing tests** in `sink.rs`:
  - `record_proven_sends_ranges_from_chunk_zero_up_to_valid_through` — `policy(2, Normal)`, source 5 MiB, `begin(.., valid_through = 3 MiB, ..)`, `record_proven(&[d0, d1, d2])`, `complete`; the decoded segment holds, after the `FileBegin`, `Range { range_start: 0, range_end: 2 MiB, digests: [d0, d1] }` then `Range { 2 MiB, 3 MiB, [d2] }`, and nothing else from this file. Mutant: `range_end` not clamped to `valid_through` (the decoder rejects the record; the test sees no second range or a `Corrupt`).
  - `record_proven_then_wrote_numbers_the_copied_chunks_after_the_prefix` — as above then `wrote(&src[3 MiB..])`, `complete`: the final ranges are `[0,2M)`, `[2M,3M)`, `[3M,5M)` and a fresh `load` through `Checkpoints::open` (generation 2) yields digests for chunks 0..5 equal to the chunk hashes of `src`. Mutant: `record_proven` advances `self.range_start` (the copied range then starts at 0 again and replay overwrites chunks 0 and 1 with the wrong digests).
  - `record_proven_at_the_source_length_ends_the_last_range_there` — source 2 MiB + 777, `valid_through = src.len`, 3 digests → ranges `[0,2M)` (with `range_chunks` 2) and `[2M, 2M+777)`. Mutant: `div_ceil` → integer division in the precondition / group count.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core checkpoint::sink::tests::record_proven` — Expected: compile error.
- [ ] **Step 3: Implement `record_proven`.**
- [ ] **Step 4: Run** `cargo nextest run -p flux-core` — Expected: all pass; mutants red then restored.
- [ ] **Step 5: Commit** `feat(core): FileSink::record_proven re-records a proven prefix as Range records (cut 9f-2)`.

---

### Task 4: `plan_resume` branches on the level; `stage_file` records the prefix

Tier hint: Sonnet, with the driver's review on Opus (this is the safety-bearing wiring).

**Files:**
- Modify: `crates/flux-core/src/copy.rs:556-610` (`plan_resume`), `:697-701` (the verdict destructuring), `:749-753` (after `FileSink::begin`)
- Test: `crates/flux-core/src/copy.rs` `mod tests` (fixtures `resume_policy`, `run_gen`, `stage_full`, `staged_of`, `crash_after`, `wal_begin_valid_through`, `replayed`, `resume_fixture`, `chunk_digests` at `:3294-3394`; `ckpt_fs` at `:1299`; `wal_bodies` at `:1338`)

**Interfaces:**
- Consumes: `fc.run.resume_verify()` (Task 1), `decide_full` and `Verdict.proven` (Task 2), `FileSink::record_proven` (Task 3).
- Produces: `plan_resume`'s signature unchanged. Its body, complete (no other arms):
  1. `let level = fc.run.resume_verify();`
  2. `Chunks`: `fc.run.load(&fc.file_id)` exactly as today (`Ok(Some)` → state; `Ok(None)` → `fresh(None)`; `Err` → `Unreadable`). `Full`: no load here.
  3. The partial's metadata, identity, hard-link and open checks, unchanged (`copy.rs:571-596`), for both levels.
  4. `Chunks` → `decide(&state, &gate, partial_len, reader, partial_reader.as_mut(), &watched)`. `Full` with `(Some(len), Some(p))` → `decide_full(src_meta.len, len, reader, p, &watched)`. `Full` with no partial → Ruling 1: `fresh(Some(PartialMissing))` iff `fc.run.load(&fc.file_id)` is `Ok(Some(_))`, else `fresh(None)`.
  5. The `beat_failed` → `CopyStep::Heartbeat` / `CopyStep::Source` mapping is shared by both levels (`copy.rs:597-608`).
- Produces: in `stage_file`, `let Verdict { start, carry, discard: why, proven } = verdict;` and, inside the `'stream` block right after `Ok(s) => sink = Some(s)` at `copy.rs:750`: if `proven` is non-empty, `s.record_proven(&proven, beat)`, an `Err` breaking `'stream` with `sink_fail(e, name)` like the other sink errors.

- [ ] **Step 1: Write the failing tests** in `copy.rs` after `a_fully_validated_partial_copies_nothing_and_still_publishes`. Add `run_gen_as(fs, generation, durability, verify)` (a `CheckpointPolicy { verify, ..resume_policy(durability) }`), keep `run_gen` = `Chunks`, and `stage_full` as is (it takes the run). `MIB`, `PARTIAL`, `pattern` exist.
  - `full_salvages_the_unrecorded_tail_that_chunks_truncates` — two SEPARATE fixtures (a `chunks` run that finishes appends chunk 3's digest to the log, so a later run on the same log would reuse 4 MiB even under the mutant; panel r1, measured by reading `Checkpoints::finish`). Fixture A, `resume_fixture(5 MiB, 3)`, partial `src[..4 MiB]`, `chunks` run (generation 2): `reused == 3 MiB`, `bytes_copied == 2 MiB`, `discard None` (the control). Fixture B, a fresh `resume_fixture(5 MiB, 3)`, the same partial, a `full` run (generation 2): `reused == 4 MiB`, `bytes_copied == 1 MiB`, `discard None`, output `== src`, `truncate_to(PARTIAL, 4 MiB)` called; `finish`; `replayed(3).digests` values `== chunk_digests(&src)`. Mutant: `plan_resume` ignores the level (always `chunks`; fixture B then reuses only 3 MiB).
  - `full_salvages_a_partial_with_no_log_at_all` — `ckpt_fs(5 MiB)`, write partial `src[..3 MiB]`, NO `crash_after` (an empty log, `load` would say `Ok(None)`). `full` (generation 1): `reused == 3 MiB`, `discard None`, no `create_new(PARTIAL)` and no `remove_file(PARTIAL)` after the mark, output `== src`; `wal_begin_valid_through == [3 MiB]`; `replayed(2).digests` values `== chunk_digests(&src)` (all five: the three re-recorded plus the two copied). A `chunks` control on a SEPARATE fresh `ckpt_fs(5 MiB)` with the same partial recopies (`reused == 0`, `create_new` called); never run it after the `full` run on the same filesystem, whose log then covers the file. Mutant: `Full` arm falls through to the `load` early return.
  - `full_records_the_proven_prefix_even_when_the_old_log_already_covers_it` — `resume_fixture(5 MiB, 3)`, partial `src[..3 MiB]`, `full`: `wal_bodies` after the run contain, after the generation-2 `FileBegin` (`valid_through == 3 MiB`), a `Range` with `range_start == 0` (the re-record) before the `Range` starting at 3 MiB. Mutant: `record_proven` skipped when `proven` is non-empty (the `if` inverted).
  - `full_ignores_a_log_that_matches_the_partial_but_not_the_source` — `resume_fixture(5 MiB, 3)`, partial `src[..3 MiB]`, then rewrite `/src` with chunk 1 changed (same length; `fs.set_modified` to the old time so a `chunks` gate would pass): `full` → `MismatchAt(MIB)`, `reused == MIB`, output `== new source`. And the reverse, partial chunk 1 flipped with the source intact (the log matches the source, not the partial): `MismatchAt(MIB)`. Mutant: `decide_full` hashes only the partial (property 7.2/7.3 at the copy level).
  - `a_chunks_resume_after_a_full_resume_that_crashed_reuses_the_full_prefix` (property 7.6) — `ckpt_fs(5 MiB)`, partial `src[..3 MiB]`, no log; a `full` run whose beat fails once the partial reaches 4 MiB (model `crash_after`'s beat), `step == CopyStep::Heartbeat`; put `src[..4 MiB]` at `PARTIAL`; a `chunks` run (next generation): `reused == 4 MiB`, `discard None`, output `== src`. Mutant: `record_proven` sends `range_start = valid_through` (the proven digests land on the wrong chunks, and `chunks` finds a mismatch at 0).
  - `full_with_no_partial_reports_partial_missing_only_when_the_log_began_the_file` (Ruling 1) — `resume_fixture(5 MiB, 3)` (no partial) under `full`: `discard == Some(PartialMissing)`; a fresh `ckpt_fs` with no log under `full`: `discard == None`; both `reused == 0`, output `== src`. Mutant: the `load` result ignored (always `PartialMissing`, or never).
  - `full_with_a_partial_longer_than_the_source_proves_the_whole_source_under_strict` — src 3 MiB + 12345, partial `src` + 2 MiB of garbage, `Durability::Strict` via `copy_file_checkpointed` (model `a_fully_validated_partial_copies_nothing_and_still_publishes`, `copy.rs:3560-3614`): `bytes_copied == 0`, `reuse == (len, None)`, `truncate_to(PARTIAL, len)` called, published `== src`, `sync_all(PARTIAL)` called, `wal_begin_valid_through == [len]`. Mutant: the loop bound uses `partial_len` instead of `source_len`.
  - `full_a_differing_unrecorded_tail_is_reported_as_a_mismatch` (Ruling 3) — `resume_fixture(5 MiB, 3)`, partial `src[..3 MiB]` + 1.5 MiB of `0xEE`: `full` → `MismatchAt(3 MiB)`, `reused == 3 MiB`, output `== src`. Mutant: the mismatching chunk treated as `PartialShorter`.
  - `a_heartbeat_failure_during_full_validation_appends_nothing` (property 7.7, Review Focus 3) — `ckpt_fs(5 MiB)`, partial `src[..3 MiB]`, a beat that fails on its 4th call (`stage_file` beats at `copy.rs:639` and `:690` before `plan_resume`, then `decide_full` beats once per chunk, so the 4th is after chunk 0 was proven): `stage_full` under `full` returns `Err` with `step == CopyStep::Heartbeat`; `wal_bodies(&fs)` is empty (no `FileBegin`, no `Range`); the partial is untouched (`!fs.called("truncate_to(..")`). Mutant: `record_proven`/`begin` moved before `plan_resume`.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core copy::tests::full_ copy::tests::a_chunks_resume_after copy::tests::a_heartbeat_failure_during_full` — Expected: FAIL (the `Full` arm does not exist: every `full` run behaves as `chunks`).
- [ ] **Step 3: Implement** the `plan_resume` branch and the `record_proven` call as specified in Interfaces.
- [ ] **Step 4: Run** `cargo nextest run --workspace --no-tests=pass` — Expected: all pass (the 9f-1 matrix is untouched under `Chunks`). Apply each named mutant, confirm red, restore.
- [ ] **Step 5: Commit** `feat(core): --resume-verify=full in plan_resume, the proven prefix re-recorded (cut 9f-2)`.

---

### Task 5: The run under `full` (quarantine, empty, absent and newer logs)

Tier hint: Sonnet (tests only against fixtures that exist; the oracle is design 3.4 and the 9f-1 tests at `run/tests.rs:5937-5980`, `:6600-6608`, `:6779-6864`).

**Files:**
- Test: `crates/flux-core/src/run/tests.rs` `mod checkpointing` (fixtures `big_fs`, `crashed_run`, `resuming`, `resume_run`, `assert_progress`, `assert_safe_redo`, `quarantined`, `corrupt_mid_log`, `crashed_segment`, `wal()`, `partial()`, `published`)

**Interfaces:**
- Consumes: `RunConfig.resume_verify` (Task 1). Add `fn resuming_full() -> RunConfig { RunConfig { resume_verify: ResumeVerify::Full, ..resuming() } }` and `resume_run_as(fs, d, cfg)` (a `resume_run` that takes the config; `resume_run` keeps calling it with `resuming()`).

- [ ] **Step 1: Write the failing tests** (each run with `lazy(Durability::Normal)` unless stated):
  - `a_full_resume_salvages_the_partial_after_the_log_was_quarantined` — `crashed_run`, `corrupt_mid_log`, `resume_run_as(.., resuming_full())`: `quarantined(&r).len() == 1` with `WAL_CORRUPT` (the warning is still reported), `assert_progress(&fs, &r, mark, 3 MiB)` (where `chunks` does `assert_safe_redo`, test at `:6779`). Mutant: `plan_resume` ignores the level.
  - `a_full_resume_salvages_the_partial_with_a_zero_length_log` — `crashed_run`, `fs.write_file(wal(), b"")`, `full`: `assert_progress(.., 3 MiB)`, `r.warnings.is_empty()`, no discard. Mutant: same as above (keep both: they pin two different `Ok(None)` origins, `scan == None` and an empty `per_file`).
  - `a_full_resume_salvages_the_partial_with_no_log` — `crashed_run`, `fs.remove_file(wal())`, `full`: `assert_progress(.., 3 MiB)`, no warnings.
  - `an_unknown_wal_format_stops_a_full_resume_before_any_copy` (property 7.5) — the body of `an_unknown_wal_format_stops_the_run_before_any_copy` (`:6817`) with `resuming_full()`: the same code, text parts, untouched partial and segment, `Failed` state. Mutant: `open_checkpoints` maps `FormatUnsupported` to a warning when the level is `full`.
  - `a_full_resume_of_a_large_file_never_started_reports_no_discard` (Ruling 1) — `big_fs(5 MiB)` plus `fs.write_file("/src/zzz", &pattern(2 MiB))` (sorted after `big`, so the crash at 3 MiB of `big` never starts it), `crashed_run`, `full`: `ok(&r).checkpoint_discards.is_empty()`, both files published equal to their patterns, `files_partially_resumed == 1`. Mutant: `Full` with no partial always reports `PartialMissing`.
  - `a_full_resume_with_a_missing_partial_names_the_file` — `crashed_run`, remove the partial, `full`: `checkpoint_discards == [("big", PartialMissing)]`, `bytes_reused == 0` (the `chunks` model is `:5899`). Mutant: the `load` result ignored (never `PartialMissing`).
  - `a_full_resume_whose_checkpoints_are_unavailable_recopies_and_warns_once` (Review Focus 4) — `crashed_run`, `fs.set_identity(wal(), FileIdentity::Unavailable)`, `full`: exactly one `CheckpointsUnavailable` warning, no `truncate_to`, published, and `partial_resumes` and `checkpoint_discards` both empty (so Task 6 prints no level line and no JSON field) (model `:5961`). Mutant: `full` runs without a checkpoint context.
  - `a_full_resume_under_strict_salvages_a_batched_partial` — `crashed_run_as(Strict)`, `resume_run_as(.., Strict, resuming_full())` with `strict()` options: `bytes_reused >= 3 MiB`, `files_partially_resumed == 1`, output equal (model `:5871`). Mutant: `stage_into_batch` passes `None` for `fc` (already pinned for `chunks`; this pins it for `full` too).
- [ ] **Step 2: Run** `cargo nextest run -p flux-core run::tests::checkpointing::a_full_resume run::tests::checkpointing::an_unknown_wal_format_stops_a_full` — Expected: with Tasks 1-4 in place these PASS except where the fixture is wrong; a test that passes on first run must still be shown red by its mutant in Step 3 (the test's job is the mutant, not the first run).
- [ ] **Step 3: Apply each mutant, confirm red, restore.**
- [ ] **Step 4: Run** `cargo nextest run --workspace --no-tests=pass` — Expected: all pass.
- [ ] **Step 5: Commit** `test(core): --resume-verify=full salvages quarantined, empty and absent logs; newer logs still stop (cut 9f-2)`.

---

### Task 6: The CLI: flag, `metadata` refusal, level line, `--json` field, e2e

Tier hint: Sonnet.

**Files:**
- Modify: `crates/flux-cli/src/main.rs:53-115` (`CopyArgs`, the arg enums), `:157-169` (`run_config`), `:232-335` (`copy`: the refusal first, then the lines and the field), the `--resume` doc comment at `:82-88`
- Modify: `crates/flux-cli/src/report.rs:21-65` (`Report`), after `checkpoint_lines` (`:452`)
- Test: `crates/flux-cli/src/report.rs` `mod tests` (`:498-520`, `:911`, `:1024`); `crates/flux-cli/tests/run.rs` (helpers `flux`, `copy`, `os`, `stderr`, `names`, `tree_in`, `Stalled::start_args`, `the_operation`; the model kill test at `:580-671`)

**Interfaces:**
- Produces in `main.rs`: `#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)] enum ResumeVerifyArg { Metadata, Chunks, Full }`; on `CopyArgs`:
  ```rust
  /// How a half-copied large file is validated before --resume continues it: `chunks` (the default) proves each
  /// recorded chunk's digest on both sides; `full` re-reads and hashes the whole partial against the source, needs no
  /// checkpoint log, and keeps the equal prefix; `metadata` is not implemented in this version.
  #[arg(long, value_enum, requires = "resume")]
  resume_verify: Option<ResumeVerifyArg>,
  ```
  (NO `default_value`: design 3.1.) `run_config` maps `None | Some(Chunks)` → `ResumeVerify::Chunks`, `Some(Full)` → `Full` (`Metadata` is unreachable there: refused first). First statement of `copy()`, before `resolve::job`:
  `error: --resume-verify=metadata is not implemented in this version; use chunks (the default) or full` to stderr, return `exit_code::USAGE`, nothing else printed (no JSON).
- Produces in `report.rs`: `Report.resume_verify: Option<&'static str>` as the LAST field, `#[serde(skip_serializing_if = "Option::is_none")]`, `None` in `zero()`;
  `pub fn resume_verify_used(outcome: &TreeOutcome, full: bool) -> bool` = `full && (!outcome.partial_resumes.is_empty() || !outcome.checkpoint_discards.is_empty())`;
  `pub fn resume_verify_lines(outcome: &TreeOutcome, full: bool) -> Vec<String>` = `["resume verification: full (source and partial re-read and hashed)"]` when used, else empty.
  `main.rs` tree branch: after `checkpoint_lines`, `lines(report::resume_verify_lines(outcome, full))`; `rep.resume_verify = report::resume_verify_used(outcome, full).then_some("full")` before `json(args, &rep)` (`rep` becomes `let mut`). The single-file branch is untouched (design 8b).

- [ ] **Step 1: Write the failing unit tests** in `report.rs`:
  - `resume_verify_lines_print_only_for_full_with_a_partial_resume_or_discard` — an outcome with one `partial_resumes` entry: `full` → the one line, `chunks` → empty; an outcome with only a `checkpoint_discards` entry: `full` → the line; an empty outcome: `full` → empty. Mutant: `||` → `&&`.
  - `the_json_carries_resume_verify_only_when_set` — `Report::pre_engine(false).to_json()` has no `"resume_verify"` substring; the same report with `resume_verify = Some("full")` serializes `"resume_verify":"full"` as its last key, and `the_json_has_every_section_53_field_in_order` still passes. Mutant: `skip_serializing_if` removed (the field appears as `null`).
- [ ] **Step 2: Write the failing e2e tests** in `tests/run.rs` (`tree_in` makes a small tree; `copy` runs the binary):
  - `resume_verify_metadata_is_a_usage_error_that_creates_nothing` (Review Focus 5) — `copy(["--resume", "--resume-verify", "metadata", "--json", src, dst])`: exit code `2`, stderr contains `--resume-verify=metadata is not implemented`, stdout empty, `dst` does not exist and `dst.flux-lock` does not exist. Mutant: the refusal placed after `resolve::job` or after the run.
  - `resume_verify_without_resume_is_a_usage_error` — `copy(["--resume-verify", "full", src, dst])`: exit `2`, `dst` absent. Mutant: `requires = "resume"` removed.
  - `a_chunks_resume_prints_what_an_unflagged_resume_prints` (property 7.9, the differential) — two fresh destinations: `copy(["--resume", "--json", src, dst_a])` and `copy(["--resume", "--resume-verify", "chunks", "--json", src, dst_b])`: both exit `0`; stderr equal after replacing `dst_a`/`dst_b` path text and dropping the last line (the summary, which carries the duration); stdout JSON objects equal after removing `duration_ms` and `average_bytes_per_second`, and neither contains `resume_verify`. Mutant: `None` mapped to `Full`, or the level line printed under `chunks`.
  - `a_single_file_resume_accepts_full_and_prints_no_level_line` — a single file `src/a` to `dst/a` (`Job::File`): `copy(["--resume", "--resume-verify", "full", "--json", src/a, dst/a])` exits `0`, the file is copied, stderr has no `resume verification` line, stdout JSON has no `resume_verify`. Mutant: the level line added to the single-file branch.
  - `a_killed_large_file_resumes_under_full_from_its_validated_prefix_without_its_log` (property 7.10) — the body of `a_killed_large_file_resumes_from_its_validated_prefix` (`:590-671`) with these changes: after the kill and the 40 MiB + garbage edit, `std::fs::remove_file(&segment)` (a `chunks` resume would recopy everything); resume with `["--resume", "--resume-verify", "full", "--durability", "strict", "--json", src, dst]`; exit `0`; stderr has `resumed big: reused 41943040 bytes, recopied`, `checkpoints discarded for big: mismatch at offset 41943040` and `resume verification: full`; stdout parses as JSON with `"resume_verify" == "full"`; bytes equal chunk by chunk; no workspace, no partial. Mutant: `record_proven` not called (the test still passes: it pins the CLI wiring and the salvage; Task 4 pins the re-record) — name the mutant that DOES kill it: `run_config` maps `Full` → `Chunks` (the resume recopies: `reused 0`, no `resumed` line).
- [ ] **Step 3: Run** `cargo nextest run -p flux-cli` — Expected: compile errors in `report.rs` tests; the e2e tests fail (unknown argument `--resume-verify`).
- [ ] **Step 4: Implement** the arg, the refusal, `run_config`, the two report functions, the field, and the two `main.rs` call sites. Replace Task 1's placeholder in `run_config`.
- [ ] **Step 5: Run** `cargo nextest run --workspace --no-tests=pass` and `cargo test --doc --workspace` — Expected: all pass. Apply each mutant, confirm red, restore. Run `cargo run -q -- copy --help` and check the `--resume-verify` help renders.
- [ ] **Step 6: Commit** `feat(cli): --resume-verify=<chunks|full>, metadata refused, level line and resume_verify JSON field (cut 9f-2)`.

---

### Task 7: Docs that name the cut

Tier hint: Haiku (specified edits).

**Files:**
- Modify: `TODO.md:960-961` (the sentence that lists `--resume-verify (cut 9f-2)` as still open)
- Modify: `docs/superpowers/specs/2026-10-13-cut-9f2-resume-verify-design.md:3` (Status line)

- [ ] **Step 1:** In `TODO.md:960`, change the open list to read: still open are snapshots, compaction and rotation of the WAL (cut 9f-3, with `--resume-verify=metadata` and the section 121 upgrade rule), and checkpoints for the single-file layout; `--resume-verify=chunks|full` shipped in cut 9f-2.
- [ ] **Step 2:** Design status line: `Status: APPROVED by the owner 2026-10-13; implemented by docs/superpowers/plans/2026-10-13-cut-9f2-resume-verify.md.` Leave the "Deviations and amendments recorded at merge" section for the driver to add at merge (the 9f-1 design's pattern), listing: Rulings 1-3 above and the two design deviations already in design section 4.
- [ ] **Step 3:** `just typos` — Expected: clean. Commit `docs: cut 9f-2 shipped; TODO and design status (cut 9f-2)`.

---

## Not in this plan (owner-gated or later)

- The measurement of `full` against `chunks` (design 8): top level only, after the owner approves, machine idle, two runs.
- The capstone, the test audit, the design's "Deviations and amendments recorded at merge" section and the memory update: the driver's finishing steps, before any PR.
- `metadata`, the section 121 upgrade rule, compaction: cut 9f-3.

## Self-review (run against the design)

- Spec coverage: 3.1 flag → T1, T6; 3.2 algorithm → T2; 3.2 placement before `load` → T4; 3.3 log after `full` → T3, T4 (`full_records_the_proven_prefix…`, `a_chunks_resume_after_a_full_resume…`); 3.4 every row → T4 (log valid, partial longer, empty, absent, source changed, partial missing) and T5 (quarantined, newer, zero-length, absent, never started); 3.5 reporting → T6; 4 deviations → recorded at merge (T7 points there); 5 no manifest change → Global Constraints (no task touches `state.rs`); 6 exit codes → T6; 7.1-7.10 → T2 (1, 3, 7, 8), T4 (2, 3, 4, 6, 7, 8), T5 (4, 5), T6 (9, 10); 8b builder answers → T4 (no `FileState` for validity), T5 (unavailable checkpoints), T6 (single file, `chunks` byte-identical); 10 Strict + full → T4 (`…under_strict`) and T5 (`…under_strict_salvages…`).
- Step scan: every test step names the test and its assertions with values; every code step names the signature and the file; the one algorithm block (T2) is the thing the signature does not determine.
- Type consistency: `ResumeVerify` (T1) is what `CheckpointPolicy.verify`, `RunConfig.resume_verify`, `Checkpoints::resume_verify()` (T1, T4, T5) and `run_config` (T6) use; `Verdict.proven: Vec<Digest>` (T2) is what `record_proven(&[Digest])` (T3) and `stage_file` (T4) consume; `resume_verify_used` / `resume_verify_lines` (T6) take `(&TreeOutcome, bool)`.
- Review Focus: each of the five has a named test (T2/T4, T3, T4, T5/T6, T6).
- Proportion: the design is 114 lines; this plan is about three times that, most of it test names and assertions, which is where the decisions are.
