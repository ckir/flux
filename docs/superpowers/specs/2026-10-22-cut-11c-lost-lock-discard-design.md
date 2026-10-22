# Cut 11c: no `state.db` write after a lost lock, and the fake's two `31a000d` codes

Status: draft r1, 2026-10-22. Branch `spec/cut-11c-lost-lock-discard`, off main `237e948`.

## 1. Why

Flux's rule for a run that has lost the destination lock (master spec section 99; cut 9d decision 8, `docs/superpowers/specs/2026-10-09-cut-9d-group-commit-design.md:83-86`)
is that it writes and removes nothing more: temporaries stay and are reported as leftovers, and the notes in `state.db` stay for recovery at
`--resume`. The temporary half of that rule holds everywhere (`crates/flux-core/src/copy.rs:214` `discard` checks the guard before every unlink).
The `state.db` half does not: three places in `crates/flux-core/src/tree.rs` still delete notes after the lock may be gone.

Measured impact, stated plainly so nobody over-reads it: **no data is lost and no other process is harmed.** The temporary is always kept, a
temporary with no note is swept by the next `--resume` exactly as a note-plus-temporary is redone, and `state.db` is a redb file this process holds
exclusively (cut 9d, line 199), so a new owner cannot open it while this process runs. The defect is a contract violation and an inconsistency
between the unbatched and batched paths, which this cut removes before more work is built on `tree.rs`.

Origin: `TODO.md` "Cut 9c debt" (the unbatched discard, triaged 2026-10-11, re-verified 2026-10-17) and "Main review of 2026-10-17" (the STOP-HELD
loop; the fake drift). Chosen as the next cut by the owner on 2026-10-22 after an AGY-FIRST consult (`.clavity/seams/next-cut-fork-4.md`, ALIGNED).

## 2. Goals

- G1. After this process has lost the destination lock, no code path in `tree.rs` writes `state.db` to discard a note, or to settle a batch in
  which an entry failed. (A batch with no failure is settled without a new guard call, N2 and N5: the same inherent window as an unbatched
  `commit_prepared` after its rename; every entry it commits was renamed under a passing guard and recovery row 1 would commit it anyway.)
- G2. When such a write is skipped, the run behaves exactly as cut 9d's STOP-LOST: the notes stay, every temporary is kept and reported, the run stops
  with a `TargetLockBusy` error, and `--resume` recovers by cut 9c's matrix (no change to `run/recover.rs`).
- G3. The fake (`crates/flux-core/src/fault_fs.rs`) answers the two codes that `31a000d` gave the real platform: `FakeClaimStore::get` on an
  undecodable record is `StateCorrupt`, and `FakeDirHandle::open_claim_store` on a special file is `DestinationError`.
- G4. Cut 9c's spec stops contradicting cut 9d's: decision 6 no longer lists "a lost lock" among the failures that discard a note at once.

## 3. Non-goals

- N1. `RedbClaimStore::insert_if_absent` and `upgrade_own_claim` mapping an undecodable record to `IoError` (`crates/flux-platform/src/claims.rs`
  around lines 215 and 246). Real-store behaviour, pre-existing, not part of `31a000d`. The fake keeps matching it (see 4.4).
- N2. A guard check before `commit_prepared` right after a successful rename (`tree.rs` around lines 1600, 1710, 2016, 2086). That is the inherent
  check-then-rename window; no spec asks for it.
- N3. The non-ASCII fold on two files (main review item 2) and the Windows `AF_UNIX` socket measurement (main review item 4).
- N4. Any change to recovery (`run/recover.rs`), the note format, exit codes, or the single-file copy path.
- N5. Performance: the new guard calls run only on failure paths (4.1), never on a successful publish.

## 4. Design

### 4.1 The rule: check the guard immediately before a post-failure `state.db` write

The three writes are made only after the run's guard (`cx.guard`, the section 99 ownership check, type `Guard` at `copy.rs:170`) has been called
and has passed, immediately before the write. The guard is called ONLY on a path where an entry has already failed; a batch or file with no failure
calls it no more often than today. A guard check is chosen over inspecting the error's code because the loss can surface in two ways: as the
failure itself (a `TargetLockBusy` from the publish guard) or behind another failure (a rename fails with an I/O error, and the copy path's
`discard` then finds the lock gone and keeps the temporary, while the returned code stays the I/O error). The re-check covers both.

The three sites are the complete list: the panel traced every `state.db` write in `tree.rs` and its callees (round 1, question 2) and found
no other discard or settle made after a failure; the success-path commits after a rename are N2. (The final review found one further class, outside G1:
in `flush_batch`, `prepare_many` and the per-entry `prepare` fallback can write a NOTE for the next entry after a `discard` inside the same flush saw
the lock lost and kept a temporary. The next publish guard then stops the run, and a note for a never-renamed temporary is recovery row 2, so no data
is at risk; it is tracked in TODO.md, not fixed here.) `cx.guard` is in scope at all four unbatched call sites
(they already pass it to the copy, `tree.rs:1580-1586`) and in `flush_batch`.

A transient I/O error in `still_owned` at a re-check is `TargetLockBusy` (`session.rs:474`) and therefore stops the run where, without the
re-check, one file would have failed. That is intended: the run cannot tell a transient error from a loss, and section 99 treats both as a loss.

The run's guard (`crates/flux-core/src/run/mod.rs:388-393`) is more than a check: it first calls the test hook `cfg.before_mutation` (the CLI's
end-to-end stall tests count its calls to pick a stall index, `run/mod.rs:725-727`), then `session.rs:465` `guarded`, whose every error is
`Code::TargetLockBusy`, and on a loss it marks the checkpoint log abandoned. All three are wanted here: a loss found by the re-check is a loss
like any other. Because the new calls are on failure paths only, no stall index of a run without a failure shifts.

### 4.2 Site 1: the unbatched Strict path (`discard_note_on_failure`, `tree.rs:3011`)

Today: `if wrote.get() && result.is_err() { let _ = claims.borrow_mut().discard_prepared(key); }`, called at four sites (`tree.rs` 1589, 1696,
2011, 2079) before `finish_copy`.

New:
- When a note was written and the copy failed with `TargetLockBusy`, do not discard and do not call the guard: the run has already decided
  the lock is lost (`finish_copy` stops on that code, `tree.rs:3085`), and a re-check could pass after a transient error in `still_owned`
  (`session.rs:474` maps any I/O error to `TargetLockBusy`), which would discard a note while the drain keeps every temporary.
- When a note was written and the copy failed with any other code, call the guard. If it passes, discard the note as today (best effort). If it
  fails, do not discard.
- When that guard fails, the copy's result becomes a STOP: a `TargetLockBusy` `CopyError` built from the guard's error, with the original
  error's `step`, carrying the original error's leftover (the temporary the copy path kept, still relative to the entry's directory), so
  `finish_copy` returns it as a stop, rebuilds the leftover relative to DEST on the way (`tree.rs:3080-3082`), and the run ends as any lost-lock
  run ends.
- The original failure is not swallowed: it is reported for this entry through `on_report` as `TreeFailureCause::Copy(original)` before the
  stop is returned, WITHOUT a leftover, and with the per-entry code remap `finish_copy` applies (`AlreadyExists` at the gate or publish becomes
  `DestinationNamespaceCollision`). This holds whatever the original's code, including one `finish_copy` would itself have returned as a stop
  (a heartbeat failure, `WalWriteFailed`, a missing no-replace primitive): that original is reported as this entry's failure, and the run's
  stop is the lost lock (my call: a lost lock is the governing fact, as `tree.rs:703` already ranks it above a heartbeat failure). The leftover travels exactly once, in the stop: the run collects every reported leftover
  (`run/mod.rs:430-435`), so a leftover in both would be listed twice.

### 4.3 Sites 2 and 3: the batched flush (`flush_batch`, `tree.rs:2533`)

Site 2, STOP-HELD (`tree.rs:2725-2748`): the first stop is not `TargetLockBusy` (a heartbeat failure with the lock held, or a missing no-replace
primitive). Today the loop discards each later temporary through `copy.rs` `discard` (which keeps it if the guard fails) and then always runs
`apply_recovery(&ops)`.
New: before `apply_recovery`, call the guard. If it passes, behave as today. If it fails, do not call `apply_recovery`; return a `TargetLockBusy`
stop built from the guard's error, at `CopyStep::Publish` (the step `kept`, `tree.rs:2785`, gives a lost-lock entry) and with no leftover, so `drain_batches` switches the remaining frames to `KeepAll` (`tree.rs:2954`) exactly as for STOP-LOST. The
original stop is reported for its entry through `on_report` as `TreeFailureCause::Copy(stop)` (not swallowed), with the leftover it carries,
which is already relative to DEST (`finish_copy` rebuilds it, `tree.rs:3080-3082`, before it returns a stop). The entries renamed before the stop
(`ops` holds their `Commit`, 9d decision 8 second bullet) are not committed: they keep their notes, finalized at `--resume` (recovery row 1).
Temporaries the loop already removed while the lock was held stay removed (that was legal when it happened); those it kept are reported as
leftovers, as today. The returned stop carries no leftover of its own, so `drain_batches` never reports one twice.

Site 3, the batch's settling transaction (`tree.rs:2752-2770`): reached when no entry stopped. When at least one entry failed (a `Discard` op is in
`ops`), call the guard before `apply_recovery`. If it passes, behave as today, including decision 7's per-entry fallback. If it fails, run neither
`apply_recovery` nor the per-entry fallback, and return a `TargetLockBusy` stop built from the guard's error, at `CopyStep::Publish` and with no
leftover: the renamed entries keep their notes
(recovery row 1, RENAMED, finalized at `--resume`) and the failed entries' temporaries were already kept or removed by the copy path. When no
entry failed, nothing changes (no extra guard call; N5).

### 4.4 The fake (`fault_fs.rs`)

- `FakeClaimStore::get` (`fault_fs.rs` around line 307): an undecodable record is `Code::StateCorrupt` with the message
  `undecodable claim record`, as `RedbClaimStore::read` (`claims.rs:197-200`). `insert_if_absent` (around 295) and `upgrade_own_claim` (around
  320) keep `IoError`, matching the real store (N1). So `corrupt_claim()` splits into two, or `get` builds its own error; the plan decides.
- `FakeDirHandle::open_claim_store` (around line 2154): a name whose type is `FileType::Other` (what `add_special` makes) is refused with
  `Code::DestinationError`, message `a claim store path holds something other than a regular file`, as `FakeDirHandle::open_lock` refuses one
  (around 2253) and as the real `dir_unix.rs` claim-store open does (around 420-431). Checked after the symlink arm and before the bytes are read.

### 4.5 Spec amendment (G4)

`docs/superpowers/specs/2026-10-09-cut-9c-commit-recovery-design.md` decision 6 (around line 82): "(the heartbeat, the guard, the rename itself
failing, a lost lock)" becomes "(the heartbeat, the rename itself failing, or another failure, while this run still holds the lock)", with a
sentence: "After a lost lock nothing is discarded: the note stays and recovery decides it by the matrix (cut 9d decision 8; cut 11c)." Lines
76-77 of that spec already say a lost lock leaves the note (recovery row 2) and stay as they are.

## 5. Failure behaviour (what the operator sees)

| Situation | Before | After |
|---|---|---|
| Strict, unbatched: the publish guard fails after the note | note discarded, run stops `TargetLockBusy`, temporary kept | note kept, run stops `TargetLockBusy`, temporary kept |
| Strict, unbatched: the rename fails (I/O), the lock is then found lost | note discarded, file fails, run continues until the next guard stops it | note kept, file's failure reported, run stops `TargetLockBusy` at once |
| Batched STOP-HELD, the lock lost during the later discards | notes discarded, original stop returned, other frames drained `RemoveAll` | notes kept, original stop reported, `TargetLockBusy` returned, other frames `KeepAll` |
| Batched, an entry failed, the lock lost before the settling transaction | renamed committed, failed discarded | nothing written, `TargetLockBusy` returned, renamed finalized at `--resume` |
| Any of the above with the lock still held | unchanged | unchanged (one extra guard call on the failure path) |

Exit codes are unchanged: `TargetLockBusy` keeps the exit code it has today.

## 6. Tests (obligations; the plan names them and their fixtures)

- T1. Unbatched Strict, the guard fails at the publish (after the note): no `claim_discard_prepared` call after the failure; the note is still in
  the fake's prepared table; the run's error is `TargetLockBusy`; the temporary is reported as a leftover, once. This pins the
  `TargetLockBusy` early return of 4.2, not the guard re-check (no re-check runs here).
- T2. Unbatched Strict, the rename fails with an I/O error and the guard fails at the copy path's discard: no `claim_discard_prepared`; the I/O
  failure is reported for that entry without a leftover; the run stops `TargetLockBusy` with the temporary as its one leftover, relative to
  DEST; no later file is attempted. The fixture's guard stays failed once it has failed (a lost lock does not come back), or the copy path's
  discard consumes a one-shot failure and the re-check passes. T2 is the test that pins site 1's guard re-check.
- T3. Batched STOP-HELD: a heartbeat stop at entry k with the lock held, then the guard fails at a later entry's discard: no
  `claim_apply_recovery` after that; the notes stay; the returned error is `TargetLockBusy`; a second frame's pending batch is kept (reported), not
  removed.
- T4. Batched settling: an entry fails per-entry and the guard fails before the settling transaction (the fixture's guard passes through every
  staging, publish and the failed entry's own discard, and fails first at the post-loop check; armed earlier, the flush takes the STOP-LOST
  branch and site 3 is never reached): no `claim_apply_recovery` and no per-entry
  fallback calls; the returned error is `TargetLockBusy`; a `--resume` over the resulting state (fake) finalizes the renamed entries.
- T5. The lock-held counterparts of T1-T4 still discard or settle as today (pins that the guard re-check does not suppress a legal write).
- T6. The fake: `get` on an undecodable record is `StateCorrupt`; `insert_if_absent` and `upgrade_own_claim` on one stay `IoError`;
  `open_claim_store` on an `add_special` name is `DestinationError`. Where a conformance test runs the same case against the real store, it covers
  both.
- T7. Mutation check, run by the driver: removing site 1's `TargetLockBusy` early return turns T1 red; removing site 1's guard re-check turns T2
  red; removing site 2's turns T3 red; removing site 3's turns T4 red.
- No existing test pins the old behaviour (panel round 2, question 3: `a_failed_rename_discards_its_note_inside_the_one_apply_recovery`,
  `tree.rs:6115`, and `a_publish_failure_after_the_note_discards_it`, `run/tests.rs:4558`, hold the lock throughout and become T5's
  counterparts; `a_lost_lock_at_entry_k_writes_and_removes_nothing`, `tree.rs:6157`, and
  `a_lost_lock_after_the_notes_keeps_them_and_the_temporaries`, `run/tests.rs:4584`, already assert no write). Lock-held failure tests gain one
  guard call, which in `run/tests.rs` shows in the call log (the run's guard reads the lock); assertions that count calls are re-derived.
- Existing tests that pin the old behaviour, if the implementation finds any after all, (a note discarded, or `apply_recovery` called, after a lost guard) change their expectation, and each
  such change is listed in the plan with the reason; any guard-call index a fixture counts that shifts because of a new failure-path guard call is
  re-derived, not guessed.

## 7. Builder's latitude

The plan decides: how the guard reaches `discard_note_on_failure` (a parameter, or the function moves into a closure); how the stop and the
reported original failure are built (helper names); whether `corrupt_claim` splits; test names and fixtures. The contracts in sections 4 and 5 are
fixed.

## 8. Docs to update when the cut lands

`TODO.md`: close "Cut 9c debt" item 3 (the unbatched discard) and "Main review of 2026-10-17" items 1 (STOP-HELD) and 3 (fake drift), each
pointing at this cut; keep items 2 and 4 open. The cut 9c spec amendment of 4.5.

## Stand-downs

None. Panel record: round 1 solo (2 folded), round 1 agy (4 folded), round 2 agy GREEN (no findings), builder check (2 gaps settled in 4.2
and 4.3, the rest answered in 4.1 and 6).
