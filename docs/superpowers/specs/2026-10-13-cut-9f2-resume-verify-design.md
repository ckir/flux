# Cut 9f-2: `--resume-verify` (design)

Status: APPROVED by the owner 2026-10-13; implemented by docs/superpowers/plans/2026-10-13-cut-9f2-resume-verify.md. Spec authority: `FLUX_FULL_UPDATED_SPEC_V16.md` sections 36, 121, 137, 211, 212 (test items 52, 86). Built on cut 9f-1 (`docs/superpowers/specs/2026-10-12-cut-9f1-chunk-checkpoints-design.md`, incl. its final section "Deviations and amendments recorded at merge (plan 2)").

How this was decided: the owner asked for an order proposed from the spec (no interview). The fork "which levels, in what order" was put to agy first (`.clavity/seams/9f2-sequencing-fork{,-r2}.md`; ALIGNED after one negotiation turn) and the owner chose: **`chunks` + `full` now; `metadata` and the section 121 upgrade rule go to cut 9f-3.** Everything marked **[my call]** is not owner-approved.

## 1. Intent

Today `flux copy --resume` validates a half-copied large file with exactly one policy: the both-sides digest proof of cut 9f-1 (recompute the BLAKE3 digest of the source chunk and of the partial chunk, compare with the digest the WAL recorded, continue after the longest matching leading run). There is no flag, and a missing, quarantined or empty log recopies the file from byte 0.

Section 36 gives the operator a choice. This cut ships the two levels that can be built without inventing semantics and without weakening the central property:

- `chunks` (default): exactly today's behaviour.
- `full`: ignore the recorded digests; prove the partial equal to the source by reading and hashing BOTH sides over the whole partial; continue after the longest proven-equal leading run. It needs no WAL entry, so it can salvage a partial that `chunks` must recopy (a quarantined, empty or absent log).

Central property (unchanged, and the test every part of this cut is held to): **`--resume` never publishes a byte that was not proven equal to the source; a damaged, stale, forged or missing WAL never endangers destination data; nothing is silently discarded.**

## 2. Goals, non-goals, anti-goals

Goals: the `--resume-verify=<chunks|full>` flag; `full` as an engine independent of the WAL; every discard, reuse and level used is reported; the default run is byte-for-byte today's behaviour.

Non-goals (each with where it goes):
- `metadata`: the flag value is parsed and refused with a usage error (section 6). Section 36 lists it but no section defines what it verifies; section 211 describes it only as a WAL retention policy; section 121 and test item 86 mention it only as the trigger of an upgrade. As first drafted ("trust the partial up to the log's `valid_through` after checking lengths, mtimes and identities") it would publish bytes nothing proved equal to the source, because a tree run never re-reads the destination after writing (`copy.rs` step 7 only re-stats the SOURCE; the default `--verify` is `source-stream`) and Normal durability makes chunk writes advisory. Goes to 9f-3, where compaction gives it a meaning, and needs an owner decision on its guarantee first.
- The section 121 upgrade rule ("if the requested level needs chunk digests that were compacted away, validate with `full` and report the upgrade"; test item 86). No production path yields compacted digests until 9f-3 (agy and the driver both read `decide.rs:86`, `copy.rs` `plan_resume`, `run/mod.rs` `open_checkpoints`, `wal/record.rs`: there are two record types, `FileBegin` and `Range`, and every `Range` carries digests). Built now it would be dead code held up only by a synthetic fixture. Goes to 9f-3.
- Snapshots, compaction, rotation (9f-3); single-file checkpoints (own decision); `flux verify`; cancellation and pause.
- Persisting the level in the manifest [my call]. See section 5.

Anti-goals (a failure even if it compiles): `full` silently weaker than a full byte proof; a level change on resume that is silent; `--resume-verify` accepted and ignored; the default path changing behaviour or speed; a newer-format WAL (`WAL_FORMAT_UNSUPPORTED`, ruling 23) being bypassed by `full`.

## 3. Behaviour

### 3.1 The flag
`--resume-verify=<metadata|chunks|full>`, default `chunks`, on `flux copy`. The clap field is `Option<..>` with NO `default_value` (so an unflagged run is never read as "flag given" by a `requires` rule); resolution maps `None` to `chunks`. [my call] Giving the flag at all `requires = "resume"` (clap), because on a fresh run there is nothing to verify and silently ignoring a flag is the thing this repo forbids; a fresh run is unaffected. It may differ between the original run and the resume (section 121: "`--resume-verify` may also change"); no `INCOMPATIBLE_STATE` can arise from it.

### 3.2 `full`: the algorithm
Where it hooks in: `plan_resume` branches on the level BEFORE it calls `fc.run.load(..)` (today `copy.rs` returns `Fresh` at `Ok(None)` from `load`, which is exactly the absent, empty and quarantined cases `full` exists to salvage); `chunks` keeps that early return unchanged. A `full` implementation that is only a mode of `decide` would never be reached in those cases.
Input: the partial `P` (opened as in 9f-1 `plan_resume`, same identity checks, same hard-link refusal, same Unavailable-identity rule), the source `S`, the source's current length `L`, chunk size `CHUNK_SIZE` (the one 9f-1 uses).
1. If no partial exists: `Fresh` with `Discard::PartialMissing`, as today.
2. For chunk `k = 0, 1, ...` with `n = min(CHUNK_SIZE, L - k*CHUNK_SIZE)`: stop (with `PartialShorter`) BEFORE reading `S` when `len(P) < k*CHUNK_SIZE + n`, exactly as `decide` does, so `carry` stays empty on a short partial; otherwise read the chunk from `S` and from `P`; the chunk is proven only if both supplied all `n` bytes and the two hash equal (compare digests of the bytes read; BLAKE3, as the log uses). Heartbeat once per chunk, exactly as `decide` does.
3. The validated prefix ends at the first chunk that is not proven. `Start::Resume { offset }` with `offset` = end of the last proven chunk (so chunk-aligned, except `offset == L` when the whole source is proven, which 9f-1 ruling 6 already allows). `offset == 0` is `Start::Fresh`.
4. A first mismatch reports `Discard::MismatchAt(start of that chunk)` (the same variant `chunks` uses); a partial that ended before it was proven to the end reports `Discard::PartialShorter { kept }`. The mismatching source chunk is carried (`Verdict.carry`) exactly as `decide` does so no source byte is read twice.
5. An I/O error reading the SOURCE returns `Err` (ruling 5); an error reading the PARTIAL ends validation like a mismatch.
6. The recorded digests are never consulted for validity. The source gate of `decide` (length, modified time, identity compared with `FileBegin`) is NOT applied by `full`, because it needs a log; byte equality of `P[..offset]` with `S[..offset]` under the CURRENT source is the proof, and the existing step-7 recheck (source unchanged since the read) still guards the publish.
[my call] `full` also trusts nothing beyond `offset`: bytes of `P` past `offset` are truncated before appending, as `chunks` does.

### 3.3 The log after a `full` validation
`chunks` leaves the validated prefix's digests in the existing log and appends after it. `full` may validate a prefix for which the log holds no records (empty, quarantined, absent), or records that disagree with the bytes just proven. The resumed run's log must let a LATER `chunks` resume validate the whole prefix, or "a second crash recopies everything" silently returns.
Decided (measured at `wal/replay.rs` `load_file`, and `wal/writer.rs`): replay inserts every `Range`'s digests into a map keyed by chunk index, a later record overwrites an earlier one, `FileBegin` prunes only digests at or past its `valid_through`, and neither replay nor the writer checks a `Range`'s position against `valid_through` (no gap or overlap rule), so NO record-format or replay change is needed. After `full` proves chunks `0..m` (offset `o`), the run opens its sink with `valid_through = o` as today, then, before the first copied byte, sends `Range` records covering `[0, o)` whose digests are the ones `full` just computed from the proven bytes (ALWAYS, even when the old log already held digests: a later record wins, so a forged or stale old digest is overwritten). This needs a new sink entry point (name left to the builder) that sends already-computed digests as `Range`s from chunk 0; `FileSink::begin`'s chunk-boundary assertion is unchanged. When `o == L` (the whole source proven) the same applies and there is no copied tail.

### 3.4 Cases (what each level does)
| State found | `chunks` (today) | `full` |
|---|---|---|
| log valid, digests cover the partial | prove each recorded chunk both sides, resume after the matching run | prove every chunk of the partial both sides directly, resume after the matching run (more reads: the unrecorded tail too) |
| log valid, partial longer than the recorded chunks | unrecorded tail truncated | the tail is proven and kept when it equals the source |
| log empty (`FileBegin` only) or absent | `Fresh` (recopy) | validate directly; salvage the equal prefix |
| log quarantined (ruling 23) | `Fresh` | validate directly; the quarantine warning is still reported |
| log of a NEWER format | run STOPS `WAL_FORMAT_UNSUPPORTED` | same stop, unchanged: `full` must never bypass it |
| source changed since the crash | `Discard::SourceChanged(..)` from the gate | no gate: byte comparison finds the first differing chunk (`MismatchAt`), or equality if the change was outside the partial |
| partial missing | `Discard::PartialMissing` | same |

### 3.5 Reporting
A run at level `full` that attempted a partial resume states the level in its report (one line) and in `--json` (field `resume_verify`); a `chunks` run adds nothing (section 8b) [my call; builder chooses the text, tests pin it]. A `full` that proved a prefix a `chunks` run would have recopied (a quarantined or empty log) is reported as a partial resume like any other (`resumed <file>: reused N bytes, recopied M bytes`). All 9f-1 discard lines are unchanged.

## 4. Spec text reconciled (deviations recorded here)
- Sections 137 and 121 read differently on `full`: 137 "may discard all partial progress and perform complete verification", 121 "re-reading and hashing source and destination". [my call] `full` salvages a proven prefix and recopies only from the first unproven chunk; "may discard" is permissive, and discarding everything when the proof fails at chunk 0 is the degenerate case of the same algorithm. Recorded as a deviation (reinterpreting a spec formula is a deviation to ledger).
- Section 36 default `chunks`: unchanged.
- Test items 52 and 86: item 52's "`--resume-verify` may change on resume" holds (no compat effect). The "missing digests upgrade to `full`" half and item 86 are 9f-3 (non-goal above). The spec amendment note to add at merge: "9f-2 implements `chunks` and `full`; the upgrade rule and `metadata` wait for compaction".

## 5. Manifest and compatibility [my call]
No manifest change, no format bump, no new `Options` field: the level is a per-invocation setting. Reason: it does not take part in the configuration fingerprint (section 121 lets it change), nothing in 9f-2 needs the ORIGINAL run's level, and `state::Options` rejects unknown fields (`#[serde(deny_unknown_fields)]`), so adding a field now would make this binary's manifests unreadable by the current format-4 readers for no gain. 9f-3 adds a recorded retention policy (with a default and a format decision) when compaction needs one. Cost if wrong: 9f-3 pays the bump it would otherwise pay now.

## 6. Errors and exit codes
- `--resume-verify=metadata`: exit `2` (`USAGE`), message naming the level and saying it is not implemented in this version, that `chunks` and `full` are, and nothing else. [my call: refused at argument handling, before any lock is taken, so nothing changes on disk.]
- `--resume-verify` without `--resume`: clap's usage error, exit 2.
- Every other failure keeps its 9f-1 code. `full` adds no new `Code`.

## 7. Safety properties each test must pin (named; mutants listed in the plan)
1. `full` resumes ONLY a prefix whose bytes equal the source: a partial whose byte at offset `x` differs resumes at the chunk start before `x`, for `x` in chunk 0, a middle chunk, the last (short) chunk.
2. `full` ignores a lying log: a log whose digests match the partial but not the source (and the reverse) does not make `full` trust either side; the output equals the source.
3. Both sides: a source-only difference and a partial-only difference each stop validation (one test each; either-side mutant must go red).
4. `full` salvages where `chunks` recopies: empty log, quarantined log, absent log, each with a valid partial prefix; `chunks` on the same fixtures still recopies (the default path is unchanged).
5. A newer-format log stops the run under `full` exactly as under `chunks`.
6. The log after `full` lets a later `chunks` resume reuse the same prefix (the section 3.3 requirement), including after a second crash.
7. Heartbeat failure during `full` validation aborts like `decide`'s; a lost lock writes nothing (9f-1's no-WAL-write-after-lost-lock holds).
8. A partial longer than the source, a zero-length partial, a partial exactly `CHUNK_SIZE`, a source of exactly `min_len` (64 MiB), and `offset == L`.
9. The flag: default is `chunks`; `metadata` exits 2 with a taking-no-action guarantee (no workspace created); `--resume-verify` without `--resume` exits 2; `chunks` output is byte-identical to a run without the flag (differential test over the 9f-1 matrix).
10. A real-process kill-and-resume test with `--resume-verify=full` on a real large file, destination byte-identical to the source (the 9f-1 test at `crates/flux-cli/tests/run.rs` is the model).
Each test must be shown red by a logic mutant of the guarded line (the repo's non-vacuous bar).

## 8. Measurement (owner-gated; top level only, machine idle, two runs, range quoted, uncontrolled load stated)
`full` reads the whole partial and the same prefix of the source: expected cost about the validation rate measured in 9f-1 (about 1 GB/s warm) on both sides. Measure `chunks` against `full` on a 4 GiB file killed at 4 s and 7 s, so the operator-facing statement "full costs about X more on resume" is a measured range, not a guess. Not delegated to a subagent.

## 8b. Questions the builder check raised, and their answers
- `full` runs only where `chunks` can: when a checkpoint context exists. With none (checkpoints unavailable or unreadable at open, a run warning already reports it) `full` has no sink to write the new log with, so the file recopies exactly as today and the existing warning stands; `full` adds no second warning.
- `full` bypasses the `load` early return of `plan_resume`: "no entry for the file", "entry with no digests" and "entry with digests" are the same path for `full` (it never reads `FileState`: not the digests, not `valid_through`, not `source_len`). The partial's identity, hard-link and open checks run first, as in `chunks`. The quarantine warning is raised at open (`run/mod.rs`), independent of the level.
- A partial LONGER than the source: no discard line, exactly as `decide` (oracle: the existing `decide` test for a partial longer than its checkpoints); the excess is truncated silently as today, a behaviour inherited from 9f-1 (its known minor), not widened here.
- Reporting: the level line and the `--json` field `resume_verify` appear ONLY when the level is `full` and the run attempted a partial resume of at least one large file; a `chunks` run's output is byte-identical to today's (that is what the section 7.9 differential test compares). A salvaged prefix produces the existing `resumed <file>: reused N bytes, recopied M bytes` line; no new warning type. A single-file copy prints no level line (it changes nothing).
- Heartbeat and I/O mapping: `full` shares `decide`'s helpers, so a heartbeat failure maps to `CopyStep::Heartbeat` and a source read error to the same step as in `chunks`; if `full` is a sibling function it must reuse, not copy, that wiring.

## 9. What is left to the builder
- The exact structure: a `Mode` argument to `decide` versus a sibling function; the type that carries the level from the CLI through `RunConfig` to `plan_resume`.
- The name and shape of the sink entry point that sends the proven prefix's digests (section 3.3).
- The report wording and the `--json` field name (section 3.5), the usage-error text (section 6).
- Reuse of the existing buffers, heartbeat, discard and carry types; no new public re-exports unless the CLI needs them.
Everything else above is decided.

## 10. Exhaustiveness self-audit (gaps found, and where each closes)
- Under-specified "what": `Discard` is reused unchanged (no new variant); `Verdict` MAY gain one field carrying the digests `full` computed for the proven prefix (the vehicle section 3.3 needs from `plan_resume` to the sink; `chunks` leaves it empty), the flag type is a clap `ValueEnum` of three values, no manifest field. CLOSED here.
- `full` without a log needs a binding between partial and source: the partial's name carries the operation id and sits under the destination lock, and `full` proves bytes against the current source, so an alien file at that name cannot be published unproven (agy's round-2 reading, which retracted its round-1 worry; the driver confirmed the name format at `copy.rs:249`). CLOSED.
- Placeholders: none left unowned; section 9 lists the builder's calls.
- Cases: section 3.4 covers log valid / empty / absent / quarantined / newer / source changed / partial missing; section 7.8 the size boundaries. A Strict-durability run is unchanged by the level (durability governs the writes, the level the validation); the plan's test list must include one `full` + Strict case. Single-file copy has no checkpoints in 9f-1, so the flag has no effect there: [my call] it is accepted and the single-file path is untouched; the plan must add a test that it is accepted and changes nothing.
- Each stated requirement maps to a section: flag (3.1), `full` (3.2), WAL independence (3.2, 3.4), reporting (3.5), spec reconciliation (4), compat (5), exit codes (6), tests (7), measurement (8).
- OPEN, resolved in the PLAN (it needs line-level facts of the then-current code): the exact call sites that thread the level; the names of the existing tests that become the differential baseline.

## 11. Deviations and amendments recorded at merge
Rulings made while building (plan `docs/superpowers/plans/2026-10-13-cut-9f2-resume-verify.md`; ledger `.superpowers/sdd/`), each a line-level call on top of this design:
- **Ruling 1.** Under `full`, with no partial, `plan_resume` reads the log only to choose the report: `Ok(Some(_))` reports `PartialMissing`, `Ok(None)` and `Err(_)` report nothing (validity is never taken from the log).
- **Ruling 2.** The proven prefix is re-recorded as one `Range` per `policy.range_chunks` chunks, because `MAX_PAYLOAD` is 1 MiB (a single record would be refused above 32 GiB).
- **Ruling 3.** Under `full`, a differing unrecorded tail reports `MismatchAt(<chunk start>)`; `chunks` truncates it silently.
- **Section 3.2 item 4 over the plan.** Two plan tests said `discard None` for a salvaged partial shorter than the source; the design wins: `PartialShorter{kept}` is reported.
- **Plan gaps found by the compiler or the mutants:** `benches/copy.rs` and `tests/integration/support/mod.rs` hold `RunConfig` literals the plan's file list omitted; the plan's CLI differential ran on a tree with no partial (so `None`/`chunks` mapped to `full` survived), and the `metadata` refusal test used a valid source (a refusal after `resolve::job` survived). Fixed by a partial-resume differential (unflagged and `--resume-verify chunks`, with and without the log) and a missing-source case.
- **Known narrow crash window (not closed):** `FileSink::begin` sends `FileBegin{valid_through}` and `record_proven` then sends its `Range`s as separate records; the writer may flush between them. A power loss in that window leaves a log claiming `valid_through` with fewer digests, so a later `chunks` resume recopies the unrecorded part silently (safe: nothing unproven is published; the same as the 9f-1 behaviour for unrecorded data). `full` still salvages it.
- **Level line scope:** the line is printed whenever `full` was requested and a partial resume or any discard occurred, including discards where nothing was re-read (`PartialMissing`, `Unreadable`); the wording is as designed (section 3.5).
