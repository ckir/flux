# `--resume` together with `--verify` (cut 15) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy --resume --verify[=...]` is accepted, and a resumed file's whole-file digest covers its validated prefix, so every verify level means for it what it means for a fresh copy.

**Architecture:** `decide` and `decide_full` (`crates/flux-core/src/checkpoint/decide.rs`) take an `Option<&mut flux_hash::Hasher>` and feed it the source bytes of each chunk they accept; `plan_resume` threads it through; `stage_file` (`crates/flux-core/src/copy.rs`) creates the hasher before `plan_resume`, drops the cut 13 refusal of a resumed file, and keeps feeding the carry and the tail as today. The CLI refusal goes. No new flag, report field or manifest field.

**Tech Stack:** Rust (workspace toolchain), `blake3` and `flux-hash`, the `FaultFs` fake for engine tests, the `flux` binary for CLI tests.

**Spec:** `docs/superpowers/specs/2026-10-27-resume-verify-design.md` (section numbers and test numbers below are the design's). Every `file:line` here was read at `68af82be874a94df1cfacfb020fbaefb9d993771`; line numbers drift as tasks land, so an implementer locates by the quoted text when a number is off.

## Global Constraints

- The gate is `just check` (fmt-check, clippy with `-D warnings`, typos, doc-check, license, the workspace nextest run, doc tests); run it before every commit that touches code. Expected tail: `Summary [ ... ] N tests run: N passed, M skipped`, no failure, exit 0. Do not add stricter flags than the repo's.
- Never write a short git sha in prose, a comment or a commit body (typos reads some seven-character shas as misspellings); full shas only, or none.
- Windows CI: a path printed or compared in a CLI test goes through the helpers the neighbouring tests use, never a raw joined string.
- Commit per task, the message ending with the attribution trailer the executing session's system reminder prescribes; stage the task's files EXPLICITLY (`git add <paths>`, never `-a`); never push, never `just pr`.
- `--verify=none` pays nothing: the prefix hasher is `None` (design goals).
- The partial bytes `decide`/`decide_full` trust do not change: only a `feed` is added at the accept point (design anti-goals). No test is edited to match the code; the oracle is the design's tests 1-9c.
- Reports, exit codes and every other CLI message stay as they are, except the intended change in design R5 (every `--resume` with a `--verify` level, `--dry-run` included, exits 0/1, not 2).

## Review Focus

1. A partial longer than the source under `decide_full`: the digest is of the source, not of the extra bytes. Pinned (a pin, not a mutant killer: the loop is bounded by `source_len`) in Task 1 (`prefix_hasher_ignores_a_partial_longer_than_the_source`).
2. A partial equal to the whole source (offset equals the source length, so the tail loop reads nothing): the digest is complete and `destination` still passes. Pinned in Task 1 (`..._short_final_chunk`) and Task 2 (`a_complete_partial_is_resumed_and_verified`).
3. Under `chunks` a log that covers only part of the partial: the digest covers the logged prefix, the rest is re-read and fed once. Pinned in Task 1 (`prefix_hasher_covers_only_the_logged_prefix`) and Task 2 (`resume_verify_passes_with_a_partial_longer_than_its_log`).
4. A verify mismatch on a resumed file: nothing is published, no partial is left, the next run recopies. Pinned in Task 2 (`a_resumed_file_with_a_corrupted_prefix_fails_verification`, with its unflipped control).
5. A file the prior run completed and this run skips: not counted in `files_verified`. Pinned in Task 3 (`resume_verify_does_not_count_a_skipped_complete_file`).

---

## Task 0: Commit this plan and the design amendments (DONE by the planning session, in the same commit as this plan)

**Files:** Create `docs/superpowers/plans/2026-10-27-resume-verify.md` (this file); Modify `docs/superpowers/specs/2026-10-27-resume-verify-design.md` (test 9c wording only).

- [ ] **Step 1:** Four exact edits to the design (plan ruling P1: `fake_src` already holds a second file, `/src/sub/b`, so no combined fixture is needed; the resumed-and-verified half is pinned by tests 4, 5 and 9b).
  - Test 9c: replace the whole paragraph that starts "9c. `resume_verify_skipped_complete_file_is_not_counted`" and ends "(`~5990`)." (4 lines) by: "9c. `resume_verify_skipped_complete_file_is_not_counted` (engine tree tests, `run/tests.rs`): `resumable_with(&verifying(opts()), 100, b"o", 101, ClaimStatus::Created)` (`:3699`) then `run_tree_with(&fs, &resume(), &verifying(opts()))`: `/src/a` is skipped as resumed and `/src/sub/b` is copied fresh, so `files_resumed == 1`, `files_verified == 1` (not 2), `files_mismatched == 0` (R1, `tree.rs:1822-1830,4233`)."
  - Section 2: "and a CLI test pins it (tests 9b, 9c)" → "and tests 9b (CLI) and 9c (engine tree) pin it".
  - Section 5.2: delete the parenthesis that begins "(12 at" and ends "so grep by path)" and write "(`run/recover.rs` has an unrelated private `fn decide`, so grep by path)" (no count is pinned).
  - Section 8, the round-3 note: "test 9c needs a combined fixture" → "test 9c uses the existing two-file `fake_src`".
- [ ] **Step 2:** `typos` on both files; `git add` both; commit `docs: implementation plan for --resume with --verify (cut 15)`.

## Task 1: The deciders feed a prefix hasher

**Files:** Modify `crates/flux-core/src/checkpoint/decide.rs` (the two definitions at `:72` and `:142`, their `mod tests`); Modify `crates/flux-core/src/copy.rs:691,693` (pass `None`, nothing else in this task).

**Interfaces:**
- Produces: `pub(crate) fn decide<S: Read, P: Read>(state: &FileState, gate: &Gate<'_>, partial_len: Option<u64>, source: &mut S, partial: Option<&mut P>, beat: &Heartbeat<'_>, prefix: Option<&mut flux_hash::Hasher>) -> Result<Verdict, FsError>` and `pub(crate) fn decide_full<S: Read, P: Read>(source_len: u64, partial_len: u64, source: &mut S, partial: &mut P, beat: &Heartbeat<'_>, prefix: Option<&mut flux_hash::Hasher>) -> Result<Verdict, FsError>`. Invariant, to be written in both doc comments: on `Start::Resume{offset}` the hasher has been fed exactly source bytes `[0, offset)`; on `Start::Fresh` it has been fed nothing; the failing chunk is never fed (it comes back as `carry`).
- Consumes: `flux_hash::Hasher::update(&mut self, &[u8])`; flux-core already depends on flux-hash (`crates/flux-core/Cargo.toml:13`); write the type as the full path `flux_hash::Hasher`, no `use`.

- [ ] **Step 1: Write the failing tests** in `decide.rs` `mod tests`. Add helpers `run_prefix(st: &FileState, len: usize, source: &[u8], partial: &[u8]) -> (Verdict, flux_hash::Digest)` and `run_full_prefix(len: usize, source: &[u8], partial: &[u8]) -> (Verdict, flux_hash::Digest)` (same as `run`/`run_full` at `:249` and `:575` but passing `Some(&mut h)` and returning `h.finalize()`), and `fn whole(b: &[u8]) -> [u8; 32] { *blake3::hash(b).as_bytes() }`. Tests, each asserting `d.as_bytes() == &whole(..)`, with `source_bytes`, `state`, `digests_of`, `MIB` as the neighbouring tests use them:
  - `prefix_hasher_holds_the_accepted_source_chunks_only` (both deciders): 5 MiB source, partial equal but for `partial[2 * MIB + 7] ^= 1` → `Start::Resume{offset: 2 MiB}`, `carry == src[2 MiB..3 MiB]`, digest `== whole(&src[..2 * MIB])`.
  - `prefix_hasher_covers_a_short_final_chunk` (both): source `source_bytes(2 * MIB + 1000)`, partial equal → `Resume{offset: 2 MiB + 1000}`, digest `== whole(&src)`.
  - `prefix_hasher_covers_only_the_logged_prefix` (`decide` only): 5 MiB, `digests_of` with `d.retain(|k, _| *k < 3)` (as `a_partial_longer_than_its_checkpoints_is_cut_to_the_proven_offset`), partial `== src` → `Resume{offset: 3 MiB}`, digest `== whole(&src[..3 * MIB])`.
  - `prefix_hasher_ignores_a_partial_longer_than_the_source` (`decide_full`): partial `src` plus `[1, 2, 3]` → digest `== whole(&src)`.
  - `prefix_hasher_is_untouched_when_the_verdict_is_fresh`: digest `== whole(b"")` for: a chunk-0 mismatch (`partial[3] ^= 1`) under both deciders; the three `SourceChanged` gates (`decide`, built as the cases loop at `:377-392`); no partial (`partial_len: None`, `partial: None`); an empty digest map.
  Existing direct calls pass `None` as the new last argument (13 sites: `decide.rs:257, 389, 402, 452, 468, 497, 541` for `decide`, where `:402` is the turbofish form `decide::<_, Cursor<Vec<u8>>>(..)`; `:578, 637, 650, 686, 700, 710` for `decide_full`; the compiler is the oracle, no count is a gate).
- [ ] **Step 2: Run** `cargo nextest run -p flux-core prefix_hasher` → does not compile (arity) / FAIL.
- [ ] **Step 3: Implement** both signatures per Interfaces. At each accept point, immediately before `pos = start + len as u64` (`decide.rs:132` and `:174`), `if let Some(h) = prefix.as_deref_mut() { h.update(&source_buf[..len]); }` (`prefix` bound `mut`). Update the two doc comments and the `copy.rs:691,693` calls with `None`.
- [ ] **Step 4: Run** `cargo nextest run -p flux-core` → PASS. Then the mutants, by hand, after `git add`-ing the finished work (so `git diff` shows only the mutation), restoring the file after each and confirming `git diff` is empty again: no `update` in `decide` (fails the `chunks`, short-chunk and logged-prefix tests); no `update` in `decide_full` (fails the `decide_full` arms); `update(&source_buf[..CHUNK_SIZE as usize])` (fails `..._short_final_chunk`); `update` also on the failing chunk (fails `..._accepted_source_chunks_only`). Not claimed killable: feeding `partial_buf` (design section 3, equivalent).
- [ ] **Step 5: `just check`, then commit** (`git add crates/flux-core/src/checkpoint/decide.rs crates/flux-core/src/copy.rs`).

## Task 2: `stage_file` hashes the prefix and stops refusing a resumed file

**Files:** Modify `crates/flux-core/src/copy.rs` (`plan_resume` `:627`, `stage_file` `:849-870`, `mod tests`).

**Interfaces:**
- Consumes: Task 1's `decide`/`decide_full` signatures.
- Produces: `fn plan_resume<D: DirHandle + Send + 'static, R: std::io::Read>(hook: Hook<'_, '_, D>, parent: &D, temp: &OsStr, src_meta: &Metadata, reader: &mut R, beat: &Heartbeat<'_>, prefix: Option<&mut flux_hash::Hasher>) -> std::result::Result<(Verdict, Option<FileIdentity>), CopyError>` (crate-private, one caller).

- [ ] **Step 1: Write the failing tests** in `copy.rs` `mod tests`, replacing `verifying_a_resumed_partial_is_refused_before_it_is_touched` (`:5426`). Add helpers `stage_logged_with(fs, root, run, o: &CopyOptions) -> StageResult` (as `stage_full` at `:3890` but with the caller's options) and use `stage_log_free_with(fs, root, o, &no_heartbeat)` (`:4523`) for the log-free hook. Spell the two enums with full paths in these tests (`crate::checkpoint::ResumeVerify::{Chunks, Full}` and `flux_fs::VerifyLevel::{Destination, Full}` clash if imported together; the module already imports `VerifyLevel::{Destination, Full, SourceStream}` at `:5168`). Fixtures: `resume_fixture(5 * MIB, 3)` (`:3967`), `run_gen_as(&fs, 2, Durability::Normal, ResumeVerify::Chunks | ResumeVerify::Full)` (`:4202`), `level(v)` (`:5126`), `PARTIAL`, `TEMP`:
  - `resume_verify_passes_for_every_hook_and_level` (design tests 4 and 5): for `(Logged/Chunks, Logged/Full, LogFree)` × `(Destination, Full)`, fresh fixture each time, `fs.write_file(PARTIAL, &src[..3 * MIB])`; the staged value has `reused == 3 MiB`, `verified == true`, `fs.read_file(PARTIAL) == src`. (`SourceStream` once: staged, `verified == false`, `reused == 3 MiB`.)
  - `resume_verify_passes_with_a_partial_longer_than_its_log`: Logged/`ResumeVerify::Chunks`, `VerifyLevel::Destination`, `fs.write_file(PARTIAL, &src[..4 * MIB])` with the 3-chunk log: `reused == 3 MiB`, `bytes_copied == 2 MiB`, `verified == true`, `fs.read_file(PARTIAL) == src` (as `full_salvages_the_unrecorded_tail_that_chunks_truncates`, `:4227`, whose control arm this mirrors).
  - `a_complete_partial_is_resumed_and_verified`: `fs.write_file(PARTIAL, &src)` (partial equals the 5 MiB source, log of 3 chunks, hook Logged/`ResumeVerify::Full` or `LogFree` so all 5 MiB are proven), `VerifyLevel::Destination`: `reused == 5 MiB`, `bytes_copied == 0`, `verified == true`.
  - `a_resumed_file_with_a_corrupted_prefix_fails_verification` (design test 7): Logged/`ResumeVerify::Chunks` via `stage_logged_with(.., &level(VerifyLevel::Destination))`, partial `src[..3 * MIB]`, with a hook `fs.on_nth("open_read", n, |fs| { let mut b = fs.read_file(TEMP).unwrap(); b[10] ^= 1; fs.write_file(TEMP, &b); })` (the closure is `Send + 'static`; `TEMP` and `PARTIAL` are the same path) placed before the temporary's re-read (compute `n` from a reference run on an identical fixture: `let calls = fs_ref.calls(); let mark = calls.iter().position(|c| c == &format!("open_read({PARTIAL})")).unwrap();` is `plan_resume`'s read of the partial, and `n = ordinal(&calls, "open_read(", &format!("open_read({PARTIAL})"), mark + 1)` (`copy.rs:5141`, `after` is an index into `calls`) is the temporary's re-read; the hook fires just before that open): the result is an error with `code() == Code::VerifyMismatch`, `!fs.exists(PARTIAL)`, `!fs.exists("/dst")`. End each arm with `run.abandon()` as the engine test it replaces did (`copy.rs:5426`). Control arm, same fixture without the hook: `verified == true`. The control is what fails if `None` is passed to `plan_resume` (the digest then misses the prefix and the unflipped compare fails); the flipped arm shows the re-read covers the prefix.
  - `a_mismatch_inside_the_prefix_feeds_the_carry_once` (design test 6): for `k == 0` (flip `partial[10]`) and `k > 0` (flip `partial[MIB + 10]`) and both of Logged/Chunks and Logged/Full: staged with `Destination`, `verified == true`, `fs.read_file(PARTIAL) == src`, and `matches!(staged.discard, Some(Discard::MismatchAt(_)))`.
  - The engine refusal test is deleted (design test 8); no replacement named after it.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core resume_verify_passes a_complete_partial a_resumed_file a_mismatch_inside` → FAIL (`CopyStep::Verify` refusal).
- [ ] **Step 3: Implement.** Add the `prefix` parameter to `plan_resume` and pass it to the one decider the match arm calls (`copy.rs:691`, `:693`; only one arm runs). In `stage_file`, move the hasher creation (`copy.rs:871`, `(opts.verify != VerifyLevel::None).then(flux_hash::Hasher::new)`) above the `plan_resume` call (`:851`) and pass `hasher.as_mut()`; delete the cut 13 C9 guard (`:859-869`) and its comment; carry (`:932`) and tail (`:954`) feeding are unchanged. Update `plan_resume`'s doc comment with the Task 1 invariant.
- [ ] **Step 4: Run** `cargo nextest run -p flux-core` → PASS. Mutants, by hand (after `git add`): pass `None` to `plan_resume` from `stage_file` (kills `resume_verify_passes_for_every_hook_and_level`, `a_complete_partial...`, the control arm of the corrupted-prefix test); create the hasher after the verdict (does not compile: the compiler is the oracle); skip the carry feed (kills `a_mismatch_inside...`).
- [ ] **Step 5: `just check`, then commit** (`git add crates/flux-core/src/copy.rs`).

## Task 3: The CLI accepts the combination

**Files:** Modify `crates/flux-cli/src/main.rs:403-408` (delete the refusal block); Modify `crates/flux-cli/tests/copy.rs:850-905` (replace `resume_with_verify_is_refused_and_changes_nothing` and delete the helper `snapshot` with its doc comment at `:852-870`, whose only two uses, `:880` and `:902`, are inside that test; remove any `use` the compiler then reports unused, because `clippy -D warnings` fails on dead code); Modify `crates/flux-cli/tests/run.rs:636` (helper returns its JSON; new test); Modify `crates/flux-core/src/run/tests.rs` (test 9c, near `:3727`).

**Interfaces:** Produces: `fn a_killed_large_file_resumes_with(extra: &[&str], log_deleted: bool) -> serde_json::Value` (returns the `v` parsed at `run.rs:692`; the existing callers ignore it).

- [ ] **Step 1: Write the failing tests.**
  - `resume_with_verify_flag_is_accepted` (design test 9, replaces the refusal test in `tests/copy.rs`): for `flag` in `--verify`, `--verify=source-stream`, `--verify=destination`, `--verify=full`, with `tree_in(d.path())` and a fresh `dst`: `flux copy --resume <flag> --json src dst` exits 0 and `json(&out)["verify_level"]` is `"destination"` for the bare flag, else the named level; `std::fs::read(dst.join("a")) == b"A"`. Plus `--dry-run --resume --verify=full --json`: exit 0, `verify_level == "full"` (design R5, C6).
  - `a_killed_large_file_resumes_and_verifies` (design test 9b, `tests/run.rs`): the existing helper with `extra = &["--verify=destination"]` and `log_deleted = false`; assert on the returned value `v["files_verified"] == 1`, `v["files_mismatched"] == 0`, `v["verify_level"] == "destination"`. The helper keeps its own `reused 41943040 bytes, recopied` and byte-for-byte assertions. One test, not a matrix.
  - `resume_verify_does_not_count_a_skipped_complete_file` (design test 9c, `run/tests.rs`): `let o = verifying(opts()); let fs = resumable_with(&o, 100, b"o", 101, ClaimStatus::Created); let (r, got) = run_tree_with(&fs, &resume(), &o);` then `out = ok(&r)`: `got.is_empty()`, `(out.files_resumed, out.files_verified, out.files_mismatched) == (1, 1, 0)`. This passes before the code change (it pins the design's R1); RED means a skipped file got counted.
- [ ] **Step 2: Run** `cargo nextest run -p flux-cli resume_with_verify_flag_is_accepted a_killed_large_file_resumes_and_verifies` → FAIL (exit 2). `cargo nextest run -p flux-core resume_verify_does_not_count` → PASS (expected).
- [ ] **Step 3: Implement.** Delete the `if args.resume && args.verify.is_some_and(..)` block (`main.rs:403-408`). Change the helper's return type and `v` to be returned at its end.
- [ ] **Step 4: Run** `cargo nextest run -p flux-cli -p flux-core` → PASS (the 70 MiB test is slow; it is one test). Mutant, by hand: make Task 2's `stage_file` pass `None` → `a_killed_large_file_resumes_and_verifies` fails with a `VERIFY_MISMATCH` exit.
- [ ] **Step 5: `just check`, then commit** (`git add` the four files above).

## Task 4: Docs

**Files:** Modify `crates/flux-cli/src/main.rs:177` (the `--verify` help sentence), `README.md:86`, `ROADMAP.md:45`, `TODO.md:1205`.

- [ ] **Step 1:** In the `--verify` doc comment replace "Cannot be combined with --resume." with: "With --resume the check covers the files this run copies or resumes; files a prior run completed are skipped, not verified by --verify." (design R6; the `--resume` help at `main.rs:129`, "files it completed are verified and skipped", is left as is). Search `crates` and the docs for the old sentence (`rg "Cannot be combined with --resume|cannot be combined with .--resume"`); no file outside the historical specs, plans and the capstone ledger may still say it (restrict the search to `crates README.md ROADMAP.md TODO.md`).
- [ ] **Step 2:** `README.md:86`: replace "`--verify` cannot be combined with `--resume`." by the same sentence in prose. `ROADMAP.md:45`: the future-tense "several sources are cut 14b" becomes "several sources are done (cut 14b)"; the closing "Remaining: the `source-stream` default and verification of a resumed file (`--verify` is refused with `--resume`)" becomes "Remaining: the `source-stream` default", and add "resumed-file verification is done (cut 15)" next to the cut 14b sentence. `TODO.md:1205`: tick the `--resume` with `--verify` item (`- [x]`) with "shipped on branch `feat/resume-verify`".
- [ ] **Step 3: Run** `typos` and `just check`; `cargo run -q -p flux-cli -- copy --help | rg -n "resum"` shows the new sentence.
- [ ] **Step 4: Commit** (`git add` the four files).

## Self-review

- Spec coverage: goals 1-3 → Tasks 1-3; zero cost → the `Option` plus the unchanged `None` path (Task 2 `SourceStream`/`none` runs unchanged: the existing cut 13 tests stay green without edits); R1 → Task 3 (9c) and Task 4 text; R2 → Task 3 test 9; R5 → Task 3 dry-run arm; R6 → Task 4; tests 1-3 → Task 1; 4-8 → Task 2; 9, 9b, 9c → Task 3.
- Types: the new parameter is spelled `prefix: Option<&mut flux_hash::Hasher>` in every task.
- Not in the plan, on purpose: an `accepted-boundaries.md` entry for R1 (optional per the panel; the owner may ask), a rollback of orphan log records after a verify mismatch (design section 8: harmless, same as a fresh checkpointed copy).
