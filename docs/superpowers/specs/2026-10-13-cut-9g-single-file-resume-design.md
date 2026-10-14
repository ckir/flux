# Cut 9g: single-file resume (design)

Status: IMPLEMENTED; approved by the owner 2026-10-14 (plan `docs/superpowers/plans/2026-10-14-cut-9g-single-file-resume.md`). Spec authority: `FLUX_FULL_UPDATED_SPEC_V16.md` sections 36, 119 (single-file layout), 120, 121. Built on cuts 9f-1 and 9f-2 (`docs/superpowers/specs/2026-10-12-cut-9f1-chunk-checkpoints-design.md`, `docs/superpowers/specs/2026-10-13-cut-9f2-resume-verify-design.md`).

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
3. **Reporting [my call].** A single-file run reports the same facts a tree run reports: bytes reused, whether a partial was resumed, and the discard reason when the partial was not (or not wholly) kept; plus the level line `resume verification: full (source and partial re-read and hashed)` and the JSON field `resume_verify: "full"`. The level line and field appear whenever a single-file `--resume` ADOPTED a prior operation, the source is at least `min_len` (so `plan_resume` ran) and a partial was found: `reused > 0` or a discard is reported (a found partial always yields one or the other, `Unreadable` and `PartialShorter{kept: 0}` included), whatever flag the user gave, so the `chunks`-to-`full` upgrade is never silent. A resume that finds no partial, or a source below `min_len`, prints and records nothing new.
4. A `PartialShorter{kept}` discard is reported under this level exactly as 9f-2 section 3.2 item 4 defines it. `PartialMissing` is NOT reported: with no log there is nothing that says the partial ever began, so a missing partial is a plain fresh copy and reports nothing. **[my call]** (9f-2's rule "no log, no `PartialMissing`" is the same one.)
5. A fresh run (no `--resume`, or `--resume` that adopted nothing) is unchanged: it does not read a partial and does not report a level.

## 4. Design

### 4.1 A log-free resume context
`plan_resume` and `stage_file` take `Option<&FileCkpt<D>>`, and a `FileCkpt` holds a `&Checkpoints<D>` that owns a WAL writer. A single file has no workspace to hold a WAL, so this cut adds a second kind of checkpoint context that needs none. **[my call; the builder confirms the smallest type]** Introduce in `checkpoint/mod.rs`:

```
pub(crate) enum Hook<'r, 'a, D> {
    /// Cut 9f: a WAL-backed context (tree files).
    Logged(&'r FileCkpt<'a, D>),
    /// Cut 9g: no log. `min_len` is the policy's (ruling P1: there is no `durability` field).
    LogFree { min_len: u64 },
}
```

`copy_file_checkpointed`'s `ckpt` parameter becomes `Option<Hook>`; every existing caller passes `Logged(..)` or `None` and behaves exactly as today. Under `LogFree`:
- `plan_resume`: always the `Full` arm; the `(None, _, _)` no-partial case returns `fresh(None)` without calling `load` (there is none).
- `stage_file`: the early sweep (`ckpt.is_none()`) is skipped; the fresh-start sweep at `copy.rs:752` still runs, and the step-2b skip branch (`--skip-existing`/`--update` that skips) sweeps the partial under its own lock check (added after the whole-branch review); no `FileSink` is created (no `begin`, `record_proven`, `wrote`, `complete`), so no log record exists to be torn, stale or forged.
- Strict durability: unchanged. The sink's `dup` handle exists only to flush per batch before a log record claims it; with no log there is no claim, and the existing end-of-file sync before publish still applies.

### 4.2 `run::file`
Pass `Hook::LogFree{..}` to `copy_file_checkpointed` only when `locked.resumed` is `Some(ResumeNote::Adopted{..})` (`run/session.rs:187-201` clones the prior state, so `locked.state.operation_id` is the PRIOR id and `temp_name` names the prior partial); a `StartedNew` or a non-resume run calls `copy_file_guarded` as today. The policy's `min_len` comes from `checkpoint_policy(cfg, opts.durability)`, exactly as `run::tree` gets them; `run::file` keeps its signature and delegates to a new `file_with(.., policy: CheckpointPolicy)` (mirroring `tree_with`) so tests can lower `min_len`. `RunConfig` gains no field, so no literal in `benches/copy.rs`, `tests/integration/support/mod.rs` or the CLI changes (the 9f-2 compiler-oracle lesson: confirm by `cargo check --all-targets`). The reuse (`reused`, `discard`) that `copy_file_checkpointed` already returns is no longer dropped: `run::file` carries it in `Run`, and the CLI prints it.

### 4.3 Where the plumbing lands (the builder confirms each)
`Run<Result<Outcome,CopyError>>` gains a single-file resume record (bytes reused, discard) or `Outcome` gains it; `report::resume_lines`, `Report::file_run` and the JSON; `exit_code::for_file_run` is untouched. `--resume` help text (`flux-cli/src/main.rs:85`) stops saying "a half-copied single file is always recopied".

### 4.4 Which crashes leave a partial
A SIGKILL, power loss or panic leaves the partial (the temp is never swept). A failure inside `stage_file` AFTER the copy started (a read or write error, disk full, a failed duplicate handle, a Strict metadata or sync failure) removes it through `discard` (`copy.rs:843-845`), which leaves the temp as a reported leftover when `guard()` fails (a lost lock). A failure BEFORE the copy mutates anything leaves it untouched and the record stays resumable: a lost heartbeat or lock, or a source read error while `plan_resume` hashes, a failed `open_existing` or `truncate_to`, an identity-gate refusal or a source error at steps 2 and 2a. The one variant that keeps a partial after the copy started, `StreamFail::Keep` (`copy.rs:520`), is produced only by a WAL failure, which a log-free run cannot have; the tree path behaves the same for data errors. So the benefit is the crash case, which is the case that matters. A failed run that kept its state record and removed its partial resumes as a fresh copy and reports nothing, which is today's behaviour.

## 5. Why a log-free resume is safe
No byte is trusted that was not hashed on both sides in this run. The partial is reopened by the identity `decide_full`'s `partial_meta` captured (`open_existing`), truncated to the proven prefix, and appended to. A partial that is not a regular file, has more than one link, or has an unavailable identity is `Unreadable` and recopied (unchanged `plan_resume` rules). A source that changed since the crash differs from the partial at the first changed chunk (or, if only its tail changed, in the carried tail), so the partial's equal prefix is kept only where the source still matches. Accepted costs: the whole partial and the source prefix are re-read on EVERY resume, so a second crash during a long validation re-reads it again (the hash is not incremental); and nothing prints progress while it runs (only the heartbeat beats). Measured in cut 9f-2 at about 10% above `chunks` at 1 to 3.6 GB partials, on a warm cache.

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
12. Property: the `Logged` path behaves exactly as before (the existing 1423 tests pass unchanged), AND a single-file `--resume` that adopted nothing prints and records byte-for-byte what it did before this cut (a differential, so case 7 is not only an absence).

## 7. Exhaustiveness self-audit
Closed in this document: the contract (3), the type change (4.1), the crash cases that leave a partial (4.4), the safety argument (5), the test cases (6).
Open, with where each resolves: (a) the exact shape of the `Hook` type and whether `FileCkpt` can be reused instead of a new enum (builder check; plan); (a2) `file_with`'s name and signature (plan, against the code then); (b) the exact `Run`/`Outcome` field for the single-file reuse and its JSON name (plan, against the code that exists then); (c) whether `finish`'s state record needs to record that a single-file resume happened (this spec says no: no manifest or state change; builder check); (d) a measurement of single-file resume on a 4 GiB file, owner-gated (a step after the build, never delegated); (e) `--resume-verify` help text wording (plan).

## 8. Stand-downs
- AGY-AFTER panel round 1 (agy, 8 seats; the driver ran a solo panel first and folded five findings): GREEN, no live challenge. DISCARDED-BELOW-FLOOR: the hook enum's name (stylistic; section 7 (a) leaves it to the plan), and whether the single-file reuse lives on `Run` or `Outcome` (internal data flow; section 4.3 and 7 (b) leave it to the plan). The owner decisions (log-free `full`, `chunks` upgraded and reported, no sidecar) were not challenged.

## 9. Deviations and amendments recorded at merge
- **P1.** `Hook::LogFree` carries only `min_len`, not `durability` (section 4.1 listed both): with no sink the durability is never read, and an unread field warns.
- **P2.** `run::file` passes `LogFree` only when the source length is at least `min_len` (one extra `fs.metadata(src)`), in addition to the prior having been adopted. Below `min_len` the old path swept the partial BEFORE the claim; `LogFree` would move that sweep after it and shift the e2e stall indices. PROVEN: dropping the `>= min_len` condition turns the existing e2e test `a_killed_single_file_copy_resumes_keeping_its_id_and_bumping_its_generation` red.
- **P3.** `FileResume` carries `recopied` (the bytes the run copied) as well as `reused` and `discard`, so the CLI prints the tree's `resumed X: reused N bytes, recopied M bytes` line.
- **Plan text corrections found in execution (intent honoured):** the heartbeat in `log_free_heartbeat_failure_leaves_the_partial_untouched` fails on the 4th call, because `stage_file` beats twice before `plan_resume`; the pre-publish stall index of the resume in `a_single_file_resume_killed_again_resumes_a_third_time` is 4, not 3 (measured: 1 to 3 leave the damaged partial, 4 leaves a whole partial); the log-free hook replaced by `None` turns 3 of the 4 new e2e tests red (the fourth expects no reuse).
- **Two non-resume fixtures were corrected in review:** `a_fresh_single_file_run_never_reads_a_partial` first put its stray partial under a name the run never opens (vacuous); the partial now sits under the run's own id.
- **Counts:** the suite grew from 1423 to 1452 tests (10 copy, 11 run, 4 report, 4 e2e), 5 skipped, unchanged.
- **Review fix (whole-branch review, Important):** a skipped single-file `--resume` (`--skip-existing`/`--update`) left its partial behind because `stage_file`'s early sweep is skipped for any hook and step 2b returns before the Fresh sweep; `stage_file` now sweeps under guard in the skip branch when a hook is present.
- **Known limitations, accepted:** when `plan_resume` hashed the partial but a later step fails, no level line or discard reason is printed (the tree behaves the same: reuse is reported only for a published file); the level line says 're-read and hashed' for an `Unreadable` discard although nothing was hashed (spec 3.3 asks for the line on any discard, as in 9f-2).
- **Pre-existing, not introduced here:** `plan_resume` reads the partial through `open_read` without comparing that handle's identity to the one `metadata` returned; only `open_existing` checks identity. A concurrent double swap of the partial's name would keep an unproven prefix. Same code as 9f-2's `full`; tracked in the anomalies inbox.
