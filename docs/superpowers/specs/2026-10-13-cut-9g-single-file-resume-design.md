# Cut 9g: single-file resume (design)

Status: DRAFT, not yet approved by the owner. Spec authority: `FLUX_FULL_UPDATED_SPEC_V16.md` sections 36, 119 (single-file layout), 120, 121. Built on cuts 9f-1 and 9f-2 (`docs/superpowers/specs/2026-10-12-cut-9f1-chunk-checkpoints-design.md`, `docs/superpowers/specs/2026-10-13-cut-9f2-resume-verify-design.md`).

How this was decided: the owner chose to skip the interview and asked for a scope proposal. The cut 9f-3 scope was put to agy first (`.clavity/seams/9f3-scope-fork{,-r2}.md`; ALIGNED after one negotiation round) and the owner approved: 9f-3 as parked is NOT built (closed by ruling in `TODO.md`), and the next cut is single-file checkpoints. The storage fork for this cut was put to agy first (`.clavity/seams/sf-ckpt-fork.md`; ALIGNED) and the owner approved all three points: (1) a `full`-only single-file resume with no new on-disk state; (2) `--resume-verify=chunks` on a single file runs as `full` and is reported as such; (3) write this spec. Everything marked **[my call]** is not owner-approved.

## 1. Goal

`flux copy SRC DST --resume`, where `SRC` is a single file, continues a half-copied file instead of restarting from byte 0. Today `run::file` (`crates/flux-core/src/run/mod.rs:473`) calls `copy_file_guarded` with no checkpoint context, so `stage_file` removes the partial (`copy.rs:672`) and recopies.

Central safety property (unchanged from 9f-1/9f-2): **`--resume` never publishes a byte not proven equal to the source; destination data is never endangered by a damaged, stale, forged or missing log; nothing is silently discarded.** Here there is no log at all, so the proof is `decide_full`'s alone: source and partial are hashed chunk by chunk and the partial is kept up to the first chunk that is not equal.

## 2. Non-goals

- A sidecar WAL or any new file beside the target (section 119's single-file layout is exactly `target`, `target.flux-partial.<id>`, `target.flux-state.<id>`, `target.flux-lock`; it stays so). A sidecar is a follow-up only if a measured need appears.
- Digest checkpoints (`chunks` as a distinct level) for a single file. Compaction, `metadata` and the section 121 upgrade rule (closed in `TODO.md`).
- Any change to the tree path, the manifest or state formats, or `flux cleanup`.
- Resuming a partial after a graceful failure that already removed it (see 4.4).

## 3. Behaviour (the contract)

1. A single-file `--resume` that adopts a prior operation (`locked.resumed` is `Some`) and whose source is at least `min_len` (64 MiB in production, the policy's value) validates the partial `<target>.flux-partial.<prior-op-id>` with `decide_full(source_len, partial_len, source, partial)`, truncates the partial to the proven prefix, and copies the rest. A source below `min_len` recopies as today.
2. **Level.** `--resume-verify` unflagged or `chunks` runs as `full` on a single file. `full` is identical. `metadata` keeps its exit 2 refusal (checked first in `copy()`, unchanged).
3. **Reporting [my call].** A single-file run reports the same facts a tree run reports: bytes reused, whether a partial was resumed, and the discard reason when the partial was not (or not wholly) kept; plus the level line `resume verification: full (source and partial re-read and hashed)` and the JSON field `resume_verify: "full"`. The level line and field appear whenever a single-file `--resume` ADOPTED a prior operation and a partial existed (reused or discarded), whatever flag the user gave, so the `chunks`-to-`full` upgrade is never silent. An unflagged single-file resume that finds no partial prints and records nothing new.
4. A `PartialShorter{kept}` discard is reported under this level exactly as 9f-2 section 3.2 item 4 defines it. `PartialMissing` is NOT reported: with no log there is nothing that says the partial ever began, so a missing partial is a plain fresh copy and reports nothing. **[my call]** (9f-2's rule "no log, no `PartialMissing`" is the same one.)
5. A fresh run (no `--resume`, or `--resume` that adopted nothing) is unchanged: it does not read a partial and does not report a level.

## 4. Design

### 4.1 A log-free resume context
`plan_resume` and `stage_file` take `Option<&FileCkpt<D>>`, and a `FileCkpt` holds a `&Checkpoints<D>` that owns a WAL writer. A single file has no workspace to hold a WAL, so this cut adds a second kind of checkpoint context that needs none. **[my call; the builder confirms the smallest type]** Introduce in `checkpoint/mod.rs`:

```
pub(crate) enum Hook<'a, D> {
    /// Cut 9f: a WAL-backed context (tree files).
    Logged(&'a FileCkpt<'a, D>),
    /// Cut 9g: no log. `min_len` and `durability` are the policy's.
    LogFree { min_len: u64, durability: Durability },
}
```

`copy_file_checkpointed`'s `ckpt` parameter becomes `Option<Hook>`; every existing caller passes `Logged(..)` or `None` and behaves exactly as today. Under `LogFree`:
- `plan_resume`: always the `Full` arm; the `(None, _, _)` no-partial case returns `fresh(None)` without calling `load` (there is none).
- `stage_file`: the early sweep (`ckpt.is_none()`) is skipped; the fresh-start sweep at `copy.rs:752` still runs; no `FileSink` is created (no `begin`, `record_proven`, `wrote`, `complete`), so no log record exists to be torn, stale or forged.
- Strict durability: unchanged. The sink's `dup` handle exists only to flush per batch before a log record claims it; with no log there is no claim, and the existing end-of-file sync before publish still applies.

### 4.2 `run::file`
Pass `Hook::LogFree{..}` to `copy_file_checkpointed` only when `cfg.resume && locked.resumed.is_some()`; otherwise `copy_file_guarded` as today. The reuse (`reused`, `discard`) that `copy_file_checkpointed` already returns is no longer dropped: `run::file` carries it in `Run`, and the CLI prints it.

### 4.3 Where the plumbing lands (the builder confirms each)
`Run<Result<Outcome,CopyError>>` gains a single-file resume record (bytes reused, discard) or `Outcome` gains it; `report::resume_lines`, `Report::file_run` and the JSON; `exit_code::for_file_run` is untouched. `--resume` help text (`flux-cli/src/main.rs:85`) stops saying "a half-copied single file is always recopied".

### 4.4 Which crashes leave a partial
A SIGKILL, power loss or panic leaves the partial (the temp is never swept); a graceful failure inside `stage_file` (`StreamFail::Discard`) removes it, as the tree path does. So the benefit is the crash case, which is the case that matters. A failed run that kept its state record and removed its partial resumes as a fresh copy and reports nothing, which is today's behaviour.

## 5. Why a log-free resume is safe
No byte is trusted that was not hashed on both sides in this run. The partial is reopened by the identity `decide_full`'s `partial_meta` captured (`open_existing`), truncated to the proven prefix, and appended to. A partial that is not a regular file, has more than one link, or has an unavailable identity is `Unreadable` and recopied (unchanged `plan_resume` rules). A source that changed since the crash differs from the partial at the first changed chunk (or, if only its tail changed, in the carried tail), so the partial's equal prefix is kept only where the source still matches. Cost: the whole partial and the source prefix are re-read each resume; measured in cut 9f-2 at equal to about 10% above `chunks` at 1 to 3.6 GB partials, on a warm cache.

## 6. Cases and tests
Oracles are the design lines cited; a conflict surfaces rather than editing a test.
1. Partial of 3 MiB of a 5 MiB source, no log: resumed, reused 3 MiB, copied 2 MiB, `PartialShorter{kept: 3 MiB}`, published equal to source. Mutant: `run::file` passes `None` (recopies).
2. Unflagged and explicit `chunks` print and record the same as `full` (the level line, `resume_verify: "full"`). Mutant: report gated on the flag instead of the level used.
3. A differing byte inside the partial: `MismatchAt(<chunk start>)`, kept up to that chunk. Mutant: partial trusted past a mismatch.
4. Partial longer than the source: truncated to the source length, published equal, reused the whole source (the 9f-2 case under strict, run through `run::file`).
5. No partial: reports nothing, fresh copy. Mutant: `PartialMissing` reported.
6. Source below `min_len`: recopied, no level line. Boundary: source exactly `min_len` resumes (9f-2 7.8's `full` case).
7. `--resume` with no prior operation: fresh, no level line. Fresh run without `--resume`: unchanged output (byte-for-byte).
8. Partial with two links, or not a regular file: `Unreadable`, recopied, never written through.
9. Heartbeat failure during the hash: nothing appended or truncated (no log exists, so the partial is untouched; assert its length unchanged).
10. Real-process e2e (`flux-cli/tests/run.rs`): kill a single-file copy mid-way (the 9f-2 kill-test pattern, sized once and shared, NOT duplicated five times), then `--resume`: bytes reused > 0, destination equal to the source. Differential: an unflagged tree resume's output is byte-identical to before this cut.
11. Strict durability single-file resume publishes equal bytes and syncs at the end (the existing end-of-file sync).
12. Property: the `Logged` path behaves exactly as before (the existing 1423 tests pass unchanged).

## 7. Exhaustiveness self-audit
Closed in this document: the contract (3), the type change (4.1), the crash cases that leave a partial (4.4), the safety argument (5), the test cases (6).
Open, with where each resolves: (a) the exact shape of the `Hook` type and whether `FileCkpt` can be reused instead of a new enum (builder check; plan); (b) the exact `Run`/`Outcome` field for the single-file reuse and its JSON name (plan, against the code that exists then); (c) whether `finish`'s state record needs to record that a single-file resume happened (this spec says no: no manifest or state change; builder check); (d) a measurement of single-file resume on a 4 GiB file, owner-gated (plan, not a build step); (e) `--resume-verify` help text wording (plan).

## 8. Deviations and amendments recorded at merge
(empty until merge)
