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
   becomes `stage_file` (steps 1-7: leftover sweep, source, identity gate, existing-file policy, the cut 8b `before_create` claim, exclusive create, stream,
   data sync, metadata, source recheck; `before_create` runs inside `stage_file` at its present place, so a Replace target's `Existing` claim is made
   BEFORE its temporary exists, exactly as today, and two targets can never stage onto the same stored name) followed by `publish_staged` (the final heartbeat and guard, then the rename). `stage_file` returns a `Staged` value: the temporary's
   name, its `FileIdentity`, `bytes_copied`, `metadata_failures`, `identity_degraded`. The writer handle is dropped (closed) when `stage_file` returns,
   so a staged file holds no descriptor. Every caller other than the Strict tree path calls the pair back to back and is byte-for-byte unchanged,
   including the cut 9c `before_publish` hook, which stays between the two halves for them.
2. **Batching applies to exactly one case:** a tree publication under `--durability=strict` into a store with `supports_prepared()`. Normal
   durability, format-1 stores, single-file copies and lockless copies never batch (they write no notes; nothing to batch).
3. **The batch lives in the directory frame.** `Frame::Live` (crates/flux-core/src/tree.rs) gains `pending: Vec<Pending>` with
   `Pending { path, key, template, staged, plan, target }` and the batch's start time and staged-byte total. Each frame has its own batch, so a
   staged entry is always next to the directory handle its rename needs, and the `DirEnd` flush runs on the frame that is about to drop that
   handle. `copy_one` for a batching target runs everything up to and including `stage_file`, then appends to its frame's list instead of
   publishing. Counting (`files_copied`, `bytes_copied`, `files_overwritten`), `finish_copy`'s reports, the claim,
   and `names.record_publication` all move to the flush, in list order, so the report order of one directory is unchanged.
4. **The flush**, in this order, always:
   1. `beat()` and `guard()` (the section 101 and 99 checks), as before any synced write under DEST.
   2. `prepare_many`: ONE Immediate transaction inserting every note of the batch (new `ClaimStore` method, all-or-nothing). The data of every file
      is already durable (decision 1: `stage_file` synced each writer under Strict), so section 164's ordering holds for the whole batch.
   3. For each pending entry in order: `publish_staged` (final `beat()`/`guard()`, rename).
   4. `apply_recovery` (cut 9c's existing method: ONE Immediate transaction, each op with `commit_prepared`'s rules, nothing changes when any
      errs) called with a `RecoveryOp::Commit` for every entry that renamed. The call is skipped only when it would carry no
      op at all (an empty call costs a sync; cut 9c deferred minor); step 5 adds the Discard ops to it.
   5. Entries that failed their rename: their temporaries are discarded as today and their notes go in the SAME `apply_recovery` call as
      `RecoveryOp::Discard` ops, so step 4 is one transaction for the whole batch (Commit ops for the renamed, Discard ops for the failed; the
      call is skipped when both lists are empty). A batch of failing renames costs one sync, not one per file.
   6. Counting happens at the rename, not at the commit: each entry that renames is credited to `files_copied`, `bytes_copied` (and
      `files_overwritten`) at once, before any later entry is tried, so an abort at entry k returns an outcome that counts the k entries it
      published. `TreeAbort::changed()` (crates/flux-core/src/tree.rs:79) reads `files_copied`; an uncounted published file would make a lost-lock
      abort look `changed == false` and let the cut 7a "refused unchanged" rollback delete a destination holding those files.
5. **Flush triggers.** The batch flushes (a) when it holds `BATCH_FILES` = 64 entries or `BATCH_BYTES` = 64 MiB of staged source bytes
   in that frame (section 148.3's threshold), (b) when the next file to stage is itself at least `BATCH_BYTES` long (the pending small files publish first, then the
   large file is staged alone), (c) at a `WalkEvent::DirEnd` whose frame has pending entries, in this order: pop the frame, flush its batch using the popped frame's
   directory handle, then the existing `claims.flush()` (with its `beat()`/`guard()`), then drop the handle (the flush needs the handle for the renames), (d) when the batch is older than `BATCH_AGE` = 1 s at the next staging, (e) when the walk ends or stops with an
   error (every live frame's flush runs before the error is returned, decision 8). Implementation constraint: `walk_into` returns its aborts with `?`, which would drop the frame stack and its pending batches unflushed; every abort therefore goes through one exit that drains the stack from the top, flushes each frame's batch (dropping a flush's own stop per decision 8), and only then returns the first error. A test aborts the walk (a lost lock at a `DirEnd`) with pending entries in two nested frames and asserts both batches were flushed or recovered, (f) before staging a file whose name case-folds equal to a pending name in the
   same frame (decision 6). The thresholds are constants in `tree.rs`; there is no flag.
6. **Name hazards while files are pending.** Staged names are not in the destination yet and not in the frame's `NameIndex`. Two source files whose
   names differ only by case, onto a case-folding destination, are today resolved in order: the second sees the first. The batch keeps that by
   tracking the folded names (ASCII fold, the same fold `reserved_path` uses) of the pending entries per frame, and flushing before it would stage a
   second entry with the same fold. A pending fresh-directory entry cannot collide with anything else on disk.
   The ASCII fold is a cheap pre-check, not the guarantee: a destination may fold more than ASCII (`É`/`é`, and on some systems more). The
   backstop that does not depend on any fold table: if `stage_file`'s exclusive create fails with `AlreadyExists` while the frame has pending
   entries, the frame's batch is flushed and the stage is retried ONCE (the earlier name is then published, so the retry meets the collision exactly
   where an unbatched copy does, at `rename_no_replace`, and gets its `DestinationNamespaceCollision` classification). The temporaries are named
   `<name>.flux-partial.<operation id>` (copy.rs `temp_name`), so two such names also collide as temporaries, which is why the create is where it shows.
7. **Failure isolation = cut 9c's, by falling back to the per-file path.** If `prepare_many` errs, nothing was written (all-or-nothing). The flush
   then retries each entry on its own through the existing `prepare` (one commit each), so a store error is attributed to the files it actually
   hits, exactly as in cut 9c, and an entry whose own `prepare` fails is discarded and reported at `CopyStep::Claim`. If the `apply_recovery` commit errs, nothing
   changed; the flush retries each renamed entry through `commit_prepared` and each failed-rename entry through `discard_prepared` (best effort), and an entry whose own commit fails is reported `ClaimNotRecorded`
   with its note left for recovery. The fallback is the ONLY place the per-file calls remain, so the common path writes two commits per batch.
8. **Stopping the run.**
   - *Lost lock* (`guard()` fails at entry k): the flush writes NOTHING more and removes NOTHING. The renamed entries 0..k-1 keep their notes (no
     claim is committed after the loss), the entries k.. keep their temporaries, reported as leftovers (`CopyError.leftover`) exactly as an
     unbatched copy reports them (cut 9c's rule in `copy_file_guarded`: removing a temporary after the loss would be a mutation), and the
     notes of all of them stay. `--resume` classifies them by cut 9c's matrix: rows 1 (RENAMED, finalized), 2/3/3b (NOT RENAMED, redone), 4 (GONE).
   - *Heartbeat failure with the lock still held*: the entries that renamed are committed through `apply_recovery`; the unrenamed entries'
     temporaries and notes are discarded as an unbatched failure discards them.
   - *Which error returns.* A flush reports per-entry failures through `on_report` (`ClaimNotRecorded`, the claim-step failures) and returns `Err`
     only for a stop (the two cases above). When the walk already holds an error, the flush still runs (clause 5(e)) but its stop is dropped: the
     walk's error is the one returned, because both come from the same lost lock or heartbeat and the report would say the same thing twice.
9. **Recovery is unchanged.** A kill leaves, per file of the batch: no note and a temporary (before `prepare_many`: the leftover sweep removes
   the temporary at `--resume`); a note and a temporary (after `prepare_many`, before that file's rename: matrix rows 2, 3 or 3b, NOT RENAMED); a note and a
   published target (after the rename, before the commit: matrix row 1, RENAMED); or a claim (after the commit). Up to `BATCH_FILES` notes
   can coexist; cut 9c's phase 1 classifies any number and phase 2 applies them in one transaction. No change to `run/recover.rs`.
10. **Memory.** The pending list is bounded by `BATCH_FILES` entries (a path, a key, a note template, a `Staged`): well under 100 KiB. No file
    descriptor is held by a staged entry (decision 1). Peak RSS must stay at the baseline's 19 MiB class (acceptance, below).
11. **Mutation hook order.** `before_mutation` is called from the run's `guard` closure (run/mod.rs:269 and :431). The guard still runs once before
    each exclusive create and once before each rename, but the creates of a batch now precede its renames. Tests that stall "at the n-th guarded
    mutation" (crates/flux-cli/tests/run.rs) keep working only if n is recomputed; the plan lists each such test and its new n. A stall run under
    Normal durability (for example the first run of `the_claim_count_equals_the_file_count_under_the_default_policy`, index 303) does not batch and
    keeps its n; the Strict runs do (that test's `--resume --durability strict` run at index 606, and the Strict stalls of the recovery e2e tests).

## Interfaces added (names are the plan's contract; signatures are final in the plan)

- `flux_fs::ClaimStore::prepare_many(&mut self, notes: &[(ClaimKey, PreparedRecord)]) -> Result<()>`: one Immediate transaction; a key that
  already has a note is `Code::IoError` and NOTHING changes.
- No `commit_many`: the commit side reuses `apply_recovery` with `RecoveryOp::Commit` values (decision 4.4).
- a conformance case for `prepare_many` (flux-fs `conformance`), run against the redb store and the fake.
- `copy::stage_file`, `copy::publish_staged`, `copy::Staged` (crate-private).
- `FaultFs` fake: the fault key `prepare_many` (the existing `apply_recovery` key covers the commit), and the existing keys keep their meaning for the fallback path.

## Behaviour that does not change

Exit codes, the report text and the 18 JSON keys, `--resume` and `--restart`, the section 99 and 101 checks before every mutation, cut 9c's note
format and `meta.format` 2, and every Normal-durability behaviour. A Strict single file still writes no note.

## Tests

- Counting, the point of the cut. With a counting `ClaimStore` (a wrapper on the fake): N files in one directory under Strict produce
  `ceil(N / 64)` `prepare_many` and `apply_recovery` calls and ZERO per-file `prepare`/`commit_prepared` calls; Normal produces none of them.
- Real filesystem (crates/flux-cli/tests or tests/integration): `strace`-free proxy: the claim store's transaction count for 1000 small files.
- Each trigger of decision 5 has a test that fails if the trigger is removed (64 files, 64 MiB via a sparse source, a large file between small ones,
  `DirEnd` with pending entries in a nested tree, age via an injected clock, walk end, case-fold collision).
- Failure isolation (decision 7): `prepare_many` failing, then one entry's own `prepare` failing; the `apply_recovery` commit failing, then one entry's own
  commit failing; each asserts WHICH files are reported and that no file is both published and reported failed.
- Lost lock mid-publish (decision 8): the guard fails at entry k; `outcome.files_copied` counts the k renamed entries, `TreeAbort::changed()` is
  true and `refused_unchanged()` false (so no rollback runs), entries before k keep their notes, entries from k keep their temporaries (reported
  as leftovers) and nothing under DEST is removed after the loss; a resume then recovers all of them.
- Crash matrix (cut 9c's six rows, extended): kill after `prepare_many`, after j renames, after all renames before the `apply_recovery` commit; each resumes to
  the same final state as an unbatched run; fault-injected through `FaultFs` (a failing rename at entry j; a failing `apply_recovery`) and one real
  kill-and-resume e2e.
- Equivalence: the same tree copied with batching on (default) and off (`BATCH_FILES = 1`, a test seam) yields the same destination bytes, the
  same claims and the same report, AND the counting store's call log differs as the design says (on: `ceil(N / 64)` `prepare_many` calls and no
  per-file `prepare`; off: N per-file `prepare` calls and no `prepare_many`), so an implementation that ignores the seam fails the test.
- Non-ASCII fold collision (reasoned, not measured: no case-folding filesystem on the dev box): a FaultFs destination that folds `É`/`é` and
  fails the second exclusive create with `AlreadyExists`; the copy flushes, retries, and reports `DestinationNamespaceCollision` for the second file.
- Existing tests that depend on the old order (decision 11) are listed in the plan and re-derived, not loosened.

## Measurement (acceptance)

Re-run the 2026-10-09 protocol on the same box: `baseline.py` (large, small, mixed; Normal and Strict; cache dropped; 2 passes x 3 runs), the
`strace -f -c` sync counts, peak RSS, and `cargo bench -p flux --bench copy` (`small_1000x4kib/strict`). Acceptance:
- Strict small: syncs per file falls from 3.0 to at most 1.15; wall time falls by at least 40% (a floor, from the sync arithmetic: 3 to about 1
  sync per file; the figure is a prediction, not a promise).
- Normal: no change outside the baseline's spread. Strict large and Strict mixed: not worse than the baseline's spread.
- Peak RSS within 2 MiB of the baseline's 19 MiB.
- Shapes, reported whether or not they gate (a flat 5000-file directory is the best case for the batch, so it alone proves too little): (i) the
  flat 5000 x 4 KiB case above; (ii) nested, 500 directories x 10 files x 4 KiB (batches of 10): syncs per file at most 1.3; (iii) 5000 directories x
  1 file (no batching by known limit 1): the measured syncs per file and wall time are reported next to the baseline's, so the owner sees the shape
  the cut does not help. Only (i) and (ii) gate.
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

6. A Replace target (an existing destination file, `Plan::Replace`) still pays one synced commit for its `Existing` claim in `before_create`,
   because `insert_if_absent` is Immediate under Strict (crates/flux-platform/src/claims.rs:183, `next_commit`): a replaced file costs 4 syncs
   today (data, `Existing` claim, note, claim) and about 2 after this cut (data, `Existing` claim). Folding the `Existing` claim into
   `prepare_many` is the later fix; it needs the exclusivity the claim gives (decision 1) to come from the in-frame pending names instead.

## Out of scope

Chunk checkpoints, partial-file resume, `--resume-verify`, snapshots, compaction, rotation and any WAL file (cut 9e, whose own design decides WAL
file versus redb rows); single-file notes; batching the data syncs; a CLI flag for the batch size.

## Stand-downs

Panel round 1 (2026-10-09, solo seats plus agy; reply `.clavity/scratch/cut9d-panel/r1-reply.md`, flagged `[13b] TRUNCATED REPLY` because its token
order was wrong; the file copy holds the same seven-seat report, so no content was missing).
- FOLDED: decision 8 - lost lock writes and removes nothing (agy's "state.db split-brain" mechanism is false: `state.db` is a redb file held under
  an exclusive file lock by this process, so a new owner cannot open it; the conclusion is kept because cut 9c recovery already covers the
  uncommitted renames).
- FOLDED: the pending list lives in `Frame::Live` (agy's Literal Implementer and question 2).
- FOLDED: which error returns from a flush (agy's Protocol Pedant); the shape table in Measurement (agy's Mechanism Gamer and question 3).
- REJECTED: "falling back to per-entry `prepare` after a failed `prepare_many` spams a broken store" (agy's Axiom Breaker): an unbatched Strict
  copy already fails and reports each file separately and the run continues (crates/flux-core/src/tree.rs:1003-1010, `ClaimNotRecorded` per file),
  so the fallback reproduces cut 9c's behaviour; its extra cost is bounded by `BATCH_FILES` = 64 failed commits.

Panel round 2 (2026-10-09; reply `.clavity/scratch/cut9d-panel/r2-reply.md`: report only, no census, echo or verdict token, so the round counts as
incomplete in form; every claim was verified against the code before folding).
- FOLDED: counting at the rename, not at the flush end (traced from agy's Blindspot Auditor lead to `TreeAbort::changed()`, tree.rs:79, a worse
  consequence than the one agy stated: a lost-lock abort would report `changed == false` over published files).
- FOLDED: failed renames' notes are discarded inside the one `apply_recovery` call (agy's Cascade Analyst: a per-entry `discard_prepared` is one
  sync each).
- FOLDED: the DirEnd ordering (agy's Literal Implementer) and the statement that `before_create` runs inside `stage_file` (agy's Axiom Breaker
  misread the spec as deferring it; the spec was silent, now explicit).
- REJECTED: "`recover_publications` compares the note's temp name with the resuming run's own operation id, so every resume is `INVALID_NOTE`"
  (agy's open question 2): the function receives the ADOPTED prior's id (crates/flux-core/src/run/place.rs:403 `let id = state.operation_id`,
  :618 `recover_publications(dest, &self.dest_shown, id, ...)`), and cut 9c's e2e test `a_renamed_publication_is_recovered_and_reported` resumes
  through it.

Panel round 3 (2026-10-09; reply `.clavity/scratch/cut9d-panel/r3-reply.md`, complete with census and verdict `FINDINGS`; tree unchanged).
- FOLDED: decision 4.4 contradicted 4.5 and the `apply_recovery` fallback ignored failed-rename notes (agy's Protocol Pedant); both reworded.
- FOLDED as a recorded limit: Replace targets keep the synced `Existing` claim (agy's Cascade Analyst; verified at claims.rs:183). agy's wording,
  "completely defeats the batching benefit", is overstated: a replaced file goes from 4 syncs to 2.
- FOLDED: decision 11 names the Strict stall (index 606) of the claim-count test as the affected one. REJECTED in part: agy's index 303 is the
  test's first run under the default (Normal) policy, which does not batch (run.rs:727), so it does not move.

Panel round 4 (2026-10-09; reply `.clavity/scratch/cut9d-panel/r4-reply.md`; every seat "no new findings", the Crash-Window Cartographer found no
wrong or undecidable pair; two answers to open questions were findings).
- FOLDED: the non-ASCII fold gap and its backstop (agy's answer to question 2; reasoned, flagged so in Tests).
- FOLDED: the equivalence test also asserts the store-call log (agy's answer to question 3).

Panel round 5 (2026-10-09; reply `.clavity/scratch/cut9d-panel/r5-reply.md`; every seat "no new findings", Boundary Smuggler checked six name hazards
against code lines and found each safe; PANEL VERDICT: minor).
- FOLDED: the abort-drains-the-stack implementation constraint (agy's answer to question 3, an implementation-plan risk, not a spec defect).
