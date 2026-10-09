# Cut 9d: group commit of Strict tree publications

Status: DRAFT for the AGY-AFTER panel and owner review (2026-10-09), branch `spec/cut-9d`. Scope picked by the owner on 2026-10-09 after an
AGY-FIRST consult and one AGY-NEGOTIATE round (briefs `.clavity/seams/cut9d-scope.md`, `cut9d-scope-neg1.md`; replies under
`.clavity/scratch/cut9d-scope/`): **9d is group commit; chunk checkpoints, partial-file resume and `--resume-verify` become cut 9e.** The slicing
table of the cut 9a spec (9d = chunk checkpoints) is superseded by this ordering. Parent spec: `FLUX_FULL_UPDATED_SPEC_V16.md` (sections 148,
163, 164, 171, 180-183). Builds on cut 9c (`docs/superpowers/specs/2026-10-09-cut-9c-commit-recovery-design.md`).

## Why this cut

Cut 9c made a Strict tree publication recoverable: a durable `prepared` note before the rename, the claim in the same transaction after it. It
cost two synced `state.db` commits per file. Measured on 2026-10-09 (ext4, release build, page cache dropped, 2 passes x 3 runs; scratch driver
`baseline.py` over `benches/publish/cases.py`):

| Case | Normal | Strict | Syncs per file under Strict (`strace -f -c`) |
|---|---|---|---|
| small, 5000 x 4 KiB | 3.0-4.3 s | 30.7-36.7 s | 10,012 `fdatasync` + 5,011 `fsync` = 3.0 |
| mixed, 500 files | 2.2-3.1 s | 4.5-5.2 s | 1,011 + 511 = 3.0 |
| large, 1 GiB | 2.5-6.0 s | 3.0-3.7 s | 13 + 12 (one file) |

`strace -f -y` on a 3-file copy shows the per-file sequence exactly: `fsync(<name>.flux-partial.<id>)` (the data), `fdatasync(state.db)` (the
note), `renameat2`, `fdatasync(state.db)` (the claim). There is no per-file directory sync (the rename is not synced: cut 9c known limit). Two of
the three syncs are redb commits that do not have to be per file. This cut batches them.

Spec basis. Section 164: "For multiple files/chunks in one batch, all corresponding data must satisfy the configured data-durability boundary
before the WAL batch is acknowledged as durable." Section 171: batching with the 1 s / 64 MiB defaults. Section 182: "The exact optimization may
collapse records, but recovery semantics must remain equivalent." The recovery semantics here are cut 9c's, unchanged: each rename still has its
own durable note first.

## Decisions

1. **Split the copy at the publish point.** `copy_file_guarded` (crates/flux-core/src/copy.rs) keeps its name, signature and behaviour. Internally it
   becomes `stage_file` (steps 1-7: leftover sweep, source, identity gate, existing-file policy, exclusive create, stream, data sync, metadata,
   source recheck) followed by `publish_staged` (the final heartbeat and guard, then the rename). `stage_file` returns a `Staged` value: the temporary's
   name, its `FileIdentity`, `bytes_copied`, `metadata_failures`, `identity_degraded`. The writer handle is dropped (closed) when `stage_file` returns,
   so a staged file holds no descriptor. Every caller other than the Strict tree path calls the pair back to back and is byte-for-byte unchanged,
   including the cut 9c `before_publish` hook, which stays between the two halves for them.
2. **Batching applies to exactly one case:** a tree publication under `--durability=strict` into a store with `supports_prepared()`. Normal
   durability, format-1 stores, single-file copies and lockless copies never batch (they write no notes; nothing to batch).
3. **The batch.** `walk_into` (crates/flux-core/src/tree.rs) owns one pending list of `Pending { depth, path, key, template, staged, plan, target }`
   (`depth` = index of its directory frame on the walk stack). `copy_one` for a batching target runs everything up to and including `stage_file`,
   then appends to the list instead of publishing. Counting (`files_copied`, `bytes_copied`, `files_overwritten`), `finish_copy`'s reports, the claim,
   and `names.record_publication` all move to the flush, in list order, so the report order of one directory is unchanged.
4. **The flush**, in this order, always:
   1. `beat()` and `guard()` (the section 101 and 99 checks), as before any synced write under DEST.
   2. `prepare_many`: ONE Immediate transaction inserting every note of the batch (new `ClaimStore` method, all-or-nothing). The data of every file
      is already durable (decision 1: `stage_file` synced each writer under Strict), so section 164's ordering holds for the whole batch.
   3. For each pending entry in order: `publish_staged` (final `beat()`/`guard()`, rename).
   4. `commit_many`: ONE Immediate transaction applying a `commit_prepared` for every entry that renamed (new method, built on `apply_in` like
      `apply_recovery`; it takes `RecoveryOp::Commit` values).
   5. For each entry that failed its rename: `discard_prepared` for its note (best effort, as cut 9c) and the temporary is discarded as today.
5. **Flush triggers.** The batch flushes (a) when it holds `BATCH_FILES` = 64 entries or `BATCH_BYTES` = 64 MiB of staged source bytes
   (section 148.3's threshold), (b) when the next file to stage is itself at least `BATCH_BYTES` long (the pending small files publish first, then the
   large file is staged alone), (c) at a `WalkEvent::DirEnd` whose frame has pending entries (the frame's directory handle is about to be dropped; the
   flush needs it for the renames), (d) when the batch is older than `BATCH_AGE` = 1 s at the next staging, (e) when the walk ends or stops with an
   error (the flush runs before the error is returned, decision 8), (f) before staging a file whose name case-folds equal to a pending name in the
   same frame (decision 6). The thresholds are constants in `tree.rs`; there is no flag.
6. **Name hazards while files are pending.** Staged names are not in the destination yet and not in the frame's `NameIndex`. Two source files whose
   names differ only by case, onto a case-folding destination, are today resolved in order: the second sees the first. The batch keeps that by
   tracking the folded names (ASCII fold, the same fold `reserved_path` uses) of the pending entries per frame, and flushing before it would stage a
   second entry with the same fold. A pending fresh-directory entry cannot collide with anything else on disk.
7. **Failure isolation = cut 9c's, by falling back to the per-file path.** If `prepare_many` errs, nothing was written (all-or-nothing). The flush
   then retries each entry on its own through the existing `prepare` (one commit each), so a store error is attributed to the files it actually
   hits, exactly as in cut 9c, and an entry whose own `prepare` fails is discarded and reported at `CopyStep::Claim`. If `commit_many` errs, nothing
   changed; the flush retries each renamed entry through `commit_prepared`, and an entry whose own commit fails is reported `ClaimNotRecorded`
   with its note left for recovery. The fallback is the ONLY place the per-file calls remain, so the common path writes two commits per batch.
8. **Stopping the run.** A lost lock (`guard()` fails at entry k) or a heartbeat failure stops the whole operation as today. The flush then: commits
   the claims of the entries 0..k-1 that renamed (they are published; their notes must become claims or be left for recovery), discards the notes of
   the entries not yet renamed, discards their temporaries, and returns the stop. If the commit itself cannot be written, the notes stay and cut 9c
   recovery decides at `--resume` (rows 1 to 4 of its matrix).
9. **Recovery is unchanged.** A kill leaves, per file of the batch: no note and a temporary (before `prepare_many`: the leftover sweep removes
   the temporary at `--resume`); a note and a temporary (after `prepare_many`, before that file's rename: matrix rows 2, 3 or 3b, NOT RENAMED); a note and a
   published target (after the rename, before `commit_many`: matrix row 1, RENAMED); or a claim (after `commit_many`). Up to `BATCH_FILES` notes
   can coexist; cut 9c's phase 1 classifies any number and phase 2 applies them in one transaction. No change to `run/recover.rs`.
10. **Memory.** The pending list is bounded by `BATCH_FILES` entries (a path, a key, a note template, a `Staged`): well under 100 KiB. No file
    descriptor is held by a staged entry (decision 1). Peak RSS must stay at the baseline's 19 MiB class (acceptance, below).
11. **Mutation hook order.** `before_mutation` is called from the run's `guard` closure (run/mod.rs:269 and :431). The guard still runs once before
    each exclusive create and once before each rename, but the creates of a batch now precede its renames. Tests that stall "at the n-th guarded
    mutation" (crates/flux-cli/tests/run.rs) keep working only if n is recomputed; the plan lists each such test and its new n.

## Interfaces added (names are the plan's contract; signatures are final in the plan)

- `flux_fs::ClaimStore::prepare_many(&mut self, notes: &[(ClaimKey, PreparedRecord)]) -> Result<()>`: one Immediate transaction; a key that
  already has a note is `Code::IoError` and NOTHING changes.
- `flux_fs::ClaimStore::commit_many(&mut self, ops: &[CommitOp]) -> Result<()>` with `CommitOp { key, target, planned }`: one Immediate
  transaction, each with `commit_prepared`'s rules; any error leaves the store unchanged.
- conformance cases for both (flux-fs `conformance`), run against the redb store and the fake.
- `copy::stage_file`, `copy::publish_staged`, `copy::Staged` (crate-private).
- `FaultFs` fake: the fault keys `prepare_many` and `commit_many`, and the existing keys keep their meaning for the fallback path.

## Behaviour that does not change

Exit codes, the report text and the 18 JSON keys, `--resume` and `--restart`, the section 99 and 101 checks before every mutation, cut 9c's note
format and `meta.format` 2, and every Normal-durability behaviour. A Strict single file still writes no note.

## Tests

- Counting, the point of the cut. With a counting `ClaimStore` (a wrapper on the fake): N files in one directory under Strict produce
  `ceil(N / 64)` `prepare_many` and `commit_many` calls and ZERO per-file `prepare`/`commit_prepared` calls; Normal produces none of them.
- Real filesystem (crates/flux-cli/tests or tests/integration): `strace`-free proxy: the claim store's transaction count for 1000 small files.
- Each trigger of decision 5 has a test that fails if the trigger is removed (64 files, 64 MiB via a sparse source, a large file between small ones,
  `DirEnd` with pending entries in a nested tree, age via an injected clock, walk end, case-fold collision).
- Failure isolation (decision 7): `prepare_many` failing, then one entry's own `prepare` failing; `commit_many` failing, then one entry's own
  commit failing; each asserts WHICH files are reported and that no file is both published and reported failed.
- Lost lock mid-publish (decision 8): the guard fails at entry k; entries before k are claims, entries from k have neither note nor temporary.
- Crash matrix (cut 9c's six rows, extended): kill after `prepare_many`, after j renames, after all renames before `commit_many`; each resumes to
  the same final state as an unbatched run; fault-injected through `FaultFs` (a failing rename at entry j; a failing `commit_many`) and one real
  kill-and-resume e2e.
- Equivalence: the same tree copied with batching on (default) and off (`BATCH_FILES = 1`, a test seam) yields the same destination bytes, the
  same claims and the same report.
- Existing tests that depend on the old order (decision 11) are listed in the plan and re-derived, not loosened.

## Measurement (acceptance)

Re-run the 2026-10-09 protocol on the same box: `baseline.py` (large, small, mixed; Normal and Strict; cache dropped; 2 passes x 3 runs), the
`strace -f -c` sync counts, peak RSS, and `cargo bench -p flux --bench copy` (`small_1000x4kib/strict`). Acceptance:
- Strict small: syncs per file falls from 3.0 to at most 1.15; wall time falls by at least 40% (a floor, from the sync arithmetic: 3 to about 1
  sync per file; the figure is a prediction, not a promise).
- Normal: no change outside the baseline's spread. Strict large and Strict mixed: not worse than the baseline's spread.
- Peak RSS within 2 MiB of the baseline's 19 MiB.
Timing follows the standing discipline: the owner approves each run, the main thread runs it idle in the background, two passes, the range is
quoted, and what was not controlled is stated.

## Known limits (to be recorded in `TODO.md` "Cut 9d known limits")

1. A tree with one small file per directory gets no batching: decision 5(c) flushes at every `DirEnd`. Keeping directory handles alive across
   `DirEnd` is the later fix.
2. A crash loses up to `BATCH_FILES` files' staging work (they are recopied at `--resume`) and up to that many claims reach recovery as notes.
3. The rename is still not directory-synced (cut 9c limit 1 stands).
4. The data `fsync` per file is not batched; it is the floor of about 1 sync per file.
5. The age bound is checked at the next staging, not by a timer: a long file being copied delays the publication of the files staged before it,
   unless it is at least `BATCH_BYTES` long (decision 5(b)).

## Out of scope

Chunk checkpoints, partial-file resume, `--resume-verify`, snapshots, compaction, rotation and any WAL file (cut 9e, whose own design decides WAL
file versus redb rows); single-file notes; batching the data syncs; a CLI flag for the batch size.
