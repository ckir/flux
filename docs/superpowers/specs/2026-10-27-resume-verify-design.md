# `--resume` together with `--verify` (cut 15) - design

Status: DRAFT, 2026-10-27, branch `feat/resume-verify` (off main f6bb251). Owner opted out of the brainstorm questions
("skip it"); every open call below is the author's (section 7) and is for the owner to override.
Prior art: cut 13 design `docs/superpowers/specs/2026-10-26-copy-verification-design.md` (sections 1, 2, C4, C9, C7),
`TODO.md:1205`, the AGY-FIRST ranking `.clavity/seams/next-cut-fork-5.md` (peer: cut 15 is small).

## 1. Intent

Cut 13 shipped `flux copy --verify=...` but refuses it with `--resume` (`crates/flux-cli/src/main.rs:403-408`, engine
backstop `crates/flux-core/src/copy.rs:861-869`), because a resumed file's prefix was never hashed by the resuming run,
so a whole-file digest would prove nothing about it. This cut lifts the refusal: a file that resumes mid-way gets a
whole-file source digest that covers the prefix too, and every verify level then means for it exactly what it means for
a fresh copy.

## 2. Goals, non-goals, anti-goals

Goals
- `flux copy --resume --verify[=source-stream|destination|full]` is accepted; for every file this run copies or resumes
  the guarantee per level is the same as for a fresh copy. Files a prior run completed and this run skips are NOT
  covered (R1): the README and `--help` say so in one sentence, and a CLI test pins it (tests 9b, 9c).
- A resumed file's digest covers bytes `[0, offset)` of the source (the validated prefix) plus the carry plus the tail.
- No extra source read for the prefix: the resume decision already reads those source bytes.
- Zero cost when `--verify` is `none` (the prefix hasher is `None`).

Non-goals
- Re-hashing files a prior run COMPLETED and this run skips (`tree.rs` `resumed_complete`, `tree.rs:1560,1822`): they stay
  skipped and are not counted in `files_verified` (call R1).
- The `source-stream` default flip, the page-cache boundary, `--exclude` for verify: unchanged TODO items.
- A manifest field recording the earlier run's verify level (cut 13 C7 stands).

Anti-goals (a failure even if it "works")
- A resumed file reported verified when only its tail, or only the prefix chunks the log recorded, was compared.
- A digest that includes prefix bytes the partial does not actually hold (the digest must be of the SOURCE bytes that
  were proven equal to the partial's bytes, never of unvalidated partial bytes).
- Any change to which partial bytes are trusted (the `decide`/`decide_full` verdicts are unchanged).

## 3. Facts this design rests on (read at f6bb251)

- `stage_file` creates the hasher after the verdict: `copy.rs:871`; the carry is already fed to it, `copy.rs:932-935`;
  tail chunks at `copy.rs:954`. Only the validated prefix `[0, offset)` is missing.
- Both deciders read the source prefix chunk by chunk into `source_buf` (always `CHUNK_SIZE` long, so only
  `[..len]` is valid) and accept a chunk only when the source bytes AND the partial bytes match:
  `checkpoint/decide.rs:123-133` (`decide`, `source_ok && partial_ok`, `pos` set at 132),
  `checkpoint/decide.rs:164-175` (`decide_full`, `digest == hash(partial)`, `pos` set at 174). So at the instant a
  chunk is accepted, `source_buf[..len]` equals the partial's chunk (up to a BLAKE3 collision) and is exactly the
  bytes the whole-file digest needs. That equality is guaranteed by the accept predicate, so a "feed `partial_buf`
  instead" change is an EQUIVALENT mutant and is not claimed to be killable (solo panel r1, M1).
- `Start::Resume{offset}`: `offset` is a multiple of `CHUNK_SIZE` after a full chunk, or `source_len` when the last
  (short) chunk was accepted (`len = (gate.len - start).min(CHUNK_SIZE)`, `decide.rs:115`); at `offset == source_len`
  the tail loop reads nothing and the digest is complete after the prefix. `Start::Fresh` is returned only with
  `pos == 0` or a mismatch at `k == 0` (no accepted chunk was hashed in either case).
- `plan_resume` is the single caller of both deciders (`copy.rs:627-710`, calls at 691 and 693), for
  `Hook::Logged` (`chunks` or `full`) and `Hook::LogFree` (`full`). A decider error (heartbeat or read) returns
  `Err` from `plan_resume` (`copy.rs:705-708`) before the stream, so a half-fed hasher is dropped unused.
- `flux_hash::Hasher` is an incremental BLAKE3 with `update`/`finalize` (`crates/flux-hash/src/lib.rs`).
- Tests that pin today's refusal: `crates/flux-cli/tests/copy.rs:873` (`resume_with_verify_is_refused_and_changes_nothing`)
  and the engine test `crates/flux-core/src/copy.rs:5426` (`verifying_a_resumed_partial_is_refused_before_it_is_touched`,
  pins `(CopyStep::Verify, Code::CopyFailed)`); both are rewritten (tests 8 and CLI below).
- Under `source-stream` the hasher is created but never finalized or compared (`copy.rs:997` gates the comparison to
  `Destination | Full`), so after this cut `--resume --verify=source-stream` hashes the prefix for no observable
  effect; it was refused before only because of the missing prefix (cut 13 C4). Accepted (R2); CPU for the prefix is
  doubled whenever a verify level is on, because the decider's own hash and the prefix hasher both read it.

## 4. Approaches

- A. Feed the prefix hasher inside the deciders (chosen). `plan_resume` receives `Option<&mut flux_hash::Hasher>`; the
  deciders `update` it with `source_buf[..len]` for each accepted chunk. `stage_file` creates the hasher BEFORE
  `plan_resume`, hands it in, and keeps using it for carry and tail. No extra read; the prefix bytes hashed are by
  construction source==partial bytes. Cost: the two decider signatures and `plan_resume`'s gain a parameter.
- B. Re-read and hash the source prefix `[0, offset)` in `stage_file` after the verdict. Isolated from the deciders but
  reads the prefix a second time (the very cost A avoids; for a resume of a large file that is the dominant cost) and
  must rewind the source reader. Rejected: slower, and it hashes source bytes after, not at, the point they were proven
  equal to the partial, a window the source could change in (the step 1b/7 rechecks would catch most, not all).
- C. Keep refusing; document "verify a resumed copy afterwards with `flux verify`" (today's text). Zero risk, no gain;
  it is the status quo that this cut exists to lift. Rejected.

## 5. Design (approach A)

5.1 Deciders. `decide(.., prefix: Option<&mut Hasher>)` and `decide_full(.., prefix: Option<&mut Hasher>)`: where a
chunk is accepted (just before `pos = start + len`), `if let Some(h) = prefix.as_deref_mut() { h.update(&source_buf[..len]); }`.
Invariant (tested): on `Start::Resume{offset}` the hasher has been fed exactly source bytes `[0, offset)`; on
`Start::Fresh` it has been fed nothing. The mismatch/short exits return without feeding the failing chunk (the carry is
fed later by `stage_file`, `copy.rs:932`).

5.2 `plan_resume` threads `Option<&mut Hasher>` to whichever decider it calls; paths that return `fresh(..)` before a
decider run feed nothing. Reborrow with `as_deref_mut()` inside the loop. Call sites the signature change touches
(the plan's FILES clause; the compiler is the oracle, no count is pinned): the two definitions `decide.rs:72,142`, the
two calls at `copy.rs:691,693`, the re-export at `checkpoint/mod.rs:26`, and every direct call in `decide.rs` tests and
helpers (12 at f6bb251; `run/recover.rs` has an unrelated private `fn decide`, so grep by path); test helpers pass `None`.

5.3 `stage_file`: move `let mut hasher = (opts.verify != VerifyLevel::None).then(flux_hash::Hasher::new)` above the `plan_resume` call
(`copy.rs:851`, the call is at 851-852); pass `hasher.as_mut()`. Delete the C9 guard `copy.rs:861-869`. Everything after is unchanged: carry
`copy.rs:932`, tail `copy.rs:954`, source digest `copy.rs:998`, destination re-read and `full` second source read
(`copy.rs:1016,1042`) compare against the now-whole digest. The destination re-read already covers the whole temporary
file, prefix included, so `destination` now proves the prefix too.

5.4 A `Fresh` verdict never follows a partially fed hasher (the 5.1 invariant). `Hasher` exposes no state to assert on,
so the guard is the unit tests of 5.1 (tests 1-3), not a `debug_assert!` (call R4).

5.5 CLI: delete the refusal `main.rs:403-408`. Reports: `files_verified` counts at publish as today (cut 13 C5), so a
resumed-and-verified file counts once; a file skipped as complete does not count (R1).

5.6 Docs: `README.md:86` ("`--verify` cannot be combined with `--resume`" -> replaced by one sentence: with `--resume`
the verification covers the files this run copies or resumes, not those a prior run completed), the `--help` text at
`crates/flux-cli/src/main.rs:177` (same replacement), `TODO.md:1205` -> done, `ROADMAP.md:45` (the whole
"Remaining:" clause becomes "the `source-stream` default" (the parenthetical and "verification of a resumed file" both go), cut 13 spec left untouched (historical).

## 6. Tests (the oracle; written before the code, named)

Engine (`crates/flux-core`, `checkpoint/decide.rs` tests and `copy.rs` tests):
1. `decide_feeds_prefix_hasher_with_accepted_source_chunks`: source of `2 * MIB + 1000` bytes, partial equal to the
   source for the first 2 MiB then a different third chunk (mismatch, `Resume{offset: 2 MiB}` plus carry): the hasher's
   digest == blake3(source[0..2 MiB]); the carry is NOT in it. Kills: no `update`; `update` of the failing chunk.
1b. same fixture but the partial reaches `source_len` (the short final chunk accepted, `offset == source_len`): digest ==
   blake3(whole source). Kills: `update(&source_buf[..CHUNK_SIZE])` for the short chunk (stale bytes past `len`).
2. 1 and 1b for `decide_full`.
3. `decide_fresh_feeds_nothing`: mismatch at chunk 0, `SourceChanged` (each of the three), `PartialMissing`, empty
   digests: the hasher's digest == blake3("") (the initial state).
4. `resumed_file_verify_destination_passes`: crash after K chunks (the existing resume fixtures near `copy.rs:5426`),
   resume with `VerifyLevel::Destination` under `chunks`, under `full`, and through `Hook::LogFree`: published bytes ==
   source and `Outcome.verified` (`copy.rs:548`) true (the field the existing cut 13 verified-tests assert; the plan names it).
   Kills: `stage_file` passes `None` to `plan_resume` (the digest then misses the prefix and the compare fails).
5. `resumed_file_verify_full_passes` (the second source read compares with the whole digest), same three hooks.
6. `resumed_file_mismatch_at_k0_and_kgt0_feeds_carry_once`: a prefix chunk corrupted after the log recorded it, for
   k == 0 (Fresh plus carry) and k > 0 (Resume plus carry), each under `chunks` and `full`: the final bytes verify and
   the digest equals blake3(source) (the carry is hashed exactly once). Kills: the decider feeding the failing chunk.
7. `resumed_file_destination_corruption_in_prefix_is_a_mismatch`: after the resume decision, a fault hook flips a byte
   of the temporary's prefix `[0, offset)` before the destination re-read (a `fs.on_nth("open_read", n, ..)` hook as in
   `copy.rs:5229,5312`; the plan recomputes the ordinal because `plan_resume` opens the partial too): the file fails with
   `VERIFY_MISMATCH` and nothing is published. This is the test that shows `destination` now proves the prefix.
8. The engine test `verifying_a_resumed_partial_is_refused_before_it_is_touched` (`copy.rs:5426`) is replaced by 4.
CLI (`crates/flux-cli/tests/copy.rs`):
9. `resume_with_verify_flag_is_accepted` (`crates/flux-cli/tests/copy.rs`, replaces `:873`): `--resume --verify` bare, and
   each of `=source-stream|destination|full`, on a fresh destination: exit 0, `verify_level` in the JSON report. Plus
   `--dry-run --resume --verify=full`: exit 0 (before this cut the refusal at `main.rs:403` made it exit 2; the change of
   exit code is intended, section 5.5). The small CLI fixtures in this file cannot reach a resume (a tree file resumes
   only at 64 MiB or more, `main.rs:131`), so this test pins acceptance only.
9b. `a_killed_large_file_resumes_and_verifies` (`crates/flux-cli/tests/run.rs`, via the existing
   `a_killed_large_file_resumes_with(extra, log_deleted)` helper at `:636`, which today returns `()` and drops its parsed
   JSON `v` at `:692-693`: the plan changes it to return `v` (existing callers ignore the value) and calls it with
   `--verify=destination`; the helper's own `reused N bytes, recopied` line and exit-0 assertions stay):
   the resumed file is published byte-equal and `files_verified == 1`, `files_mismatched == 0`. This is the
   end-to-end test that fails if `None` is passed to `plan_resume`. It reuses the 64 MiB `Stalled` harness and is
   slow; it is one test, not a matrix (the matrix is tests 4 and 5 in the engine, at `min_len: MIB`).
9c. `resume_verify_skipped_complete_file_is_not_counted` (engine tree tests, `run/tests.rs`, with `min_len` set small as
   at `:5914`): a tree of one file the prior run completed (skipped by `resumed_complete`) and one half-copied file:
   `files_verified == 1` (not 2) and `files_resumed` counts the skipped one (R1, `tree.rs:1822-1830,4233`). No existing fixture builds both kinds of file in one tree: the plan combines
   `resumable(.., ClaimStatus::Created)` (`run/tests.rs:~3720`) with the half-copied setup of `crashed_run` (`~5990`).
Mutants, run by hand and each named to the test that must fail: no `update` in `decide` (1, 4); no `update` in
`decide_full` (2, 4); stale-length `update` (1b); `None` passed to `plan_resume` (4, 5); the failing chunk fed (1, 6).
Not claimed killable: feeding `partial_buf` for `source_buf` (equivalent, section 3).

## 7. Author's calls (owner may override)

- R1. Files a prior run completed and this run skips are NOT re-hashed or counted. Why: they are skipped on size+mtime
  today by design (`tree.rs:1822`); re-hashing them is a separate feature ("re-verify a finished tree" is `flux verify`).
  Cost to the owner: a resumed `--verify` run does not prove files finished by the crashed run.
- R2. `source-stream` is allowed with `--resume` (reverses cut 13 C4, which refused it only because no prefix hash
  existed).
- R3. Approach A over B (section 4).
- R4. No `debug_assert` on hasher state; the invariant is pinned by tests 1-3.
- R5. No new CLI flag, no report field, no manifest field. Intended observable change, listed so it is not a surprise:
  every `--resume` with a `--verify` level, including `--dry-run`, exits 0/1 instead of 2.
- R6. `--help` wording: the `--resume` help (`main.rs:129`, "files it completed are verified and skipped") speaks of
  the resume's own check, not `--verify`; the new `--verify` sentence at `main.rs:177` says "files a prior run completed
  are skipped, not verified by --verify", so the two cannot be read as one claim. The same disambiguation goes in the README.

## 8. Self-audit (gaps and where they close)

- Contracts: the only new signature is `Option<&mut flux_hash::Hasher>` on `decide`, `decide_full`, `plan_resume` (written with the full path: flux-core already depends on flux-hash, `Cargo.toml:13`, and `decide.rs` imports no hash type)
  (crate-private); no serialized shape changes. Closed.
- Edge states: partial longer than source prefix chunks (`shorter`): hashed only accepted chunks; partial ending
  mid-chunk: the partial tail past `pos` is truncated (`copy.rs` `truncate_to(offset)`) and re-read from the source
  into the stream, so it is hashed once. LogFree/`full`/`chunks` all covered by tests 1, 2, 4, 5. Zero-length source:
  `min_len` filter means no resume. The `Chunks` last-short-chunk case was traced in panel r1 (A2): `len` is
  `(gate.len - start).min(CHUNK_SIZE)` and acceptance needs `partial_len >= start + len`, so the offset is
  `source_len` and test 1b pins it. Closed.
- Requirements -> sections: goals 1-3 -> 5.1-5.3 and tests 4, 5, 9; zero cost -> `Option`; anti-goals -> tests 1, 3, 6, 7.
- Round-1 panel (solo Sonnet; agy round 1 folded) findings all FOLDED above: citation fixes (main.rs:403-408, copy.rs:851,
  627-710, decide.rs:123-133 and 164-175), the existing engine test at copy.rs:5426, README:86 and main.rs:177,
  short-chunk and offset==source_len tests, the equivalent mutant removed, the uncompilable "hasher after
  plan_resume" mutant replaced, test 7 given an injection point, R1 versus the guarantee goal reconciled (goals, test
  9), `source-stream` hashing for no effect and doubled prefix CPU stated, call sites listed.
- Round-2 solo (Sonnet) FINDINGS-7, all FOLDED: the call-site count (14 real, none pinned now), test 9 split because the
  `copy.rs` CLI fixtures cannot reach a resume (9 acceptance, 9b end-to-end in `tests/run.rs`, 9c skipped-file count),
  test 7 and section 5.5 line cites, the ROADMAP "Remaining:" clause, the `--dry-run`/exit-code change (R5), the help
  wording collision (R6). Resource Vampire: no new findings.
- Round-3 solo (Sonnet) FINDINGS-2 and agy round 3, FOLDED: test 9b needs the helper to return its JSON, test 9c needs a
  combined fixture, `tree.rs:4233`, `Outcome.verified`, the `flux_hash` full path (all above). agy C1 (NIT, measured TRUE):
  when a resumed file fails the destination or second-source compare, `discard` unlinks the temporary
  (`copy.rs:239-258`), the log keeps its records, and the next `--resume` finds no partial (`plan_resume`'s no-partial
  arm, `copy.rs:696-702`) and recopies from scratch (`Discard::PartialMissing`); the orphan records are harmless and the
  path is the same as for any checkpointed fresh copy that fails verification (cut 13), now also reachable for a resume.
  No rollback of the log is added.

## Stand-downs

- `DISCARDED-BELOW-FLOOR`: A3 (prefix hashed twice) is stated in section 3 as a cost, not changed: the decider's check
  hash and the whole-file hasher are different digests of the same bytes, and sharing them is impossible because
  BLAKE3 chunk digests do not compose (`TODO.md:1205`).
- `REJECTED`: the suggested `debug_assert_eq!(source_buf[..len], partial_buf[..len])` at accept: it compares what the
  accept predicate (`decide.rs:123-126,164-167`) already proves equal, so it cannot fail and would add a memcmp per
  chunk in debug builds for nothing.
- `REJECTED` (agy r3 M1, a mutant that hashes the prefix only for `Destination | Full`, so `source-stream` creates no
  hasher): equivalent. Under `source-stream` the hasher is never read (`copy.rs:997` gates the comparison to
  `Destination | Full`), so no test can observe the difference, and a white-box `hasher.is_some()` assertion would pin an
  implementation detail with no behaviour behind it. Section 3 already records that `source-stream` hashes for no
  observable effect (R2).
