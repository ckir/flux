# Cut 9e: a batched, parallel data-sync barrier for Strict tree publications

Status: APPROVED by the owner 2026-10-10 (design in chat, then the written spec after the panel). Adversarial panel: 6 rounds with agy, all findings folded, NOT GREEN (round 6 still found definition gaps; the owner chose to ship to review at the round cap, see Stand-downs). Stacked on cut 9d (PR #82; branch
`spec/cut-9d`): 9e edits the 9d flush and must be planned against the merged 9d code. Scope was set by an AGY-FIRST consult and a two-round
AGY-NEGOTIATE (briefs `.clavity/seams/cut9e-scope.md`, `cut9e-negotiate-r1.md`, `cut9e-negotiate-r2.md`; replies under `.clavity/scratch/`) and
decided by the owner: **9e-1 = a portable thread-pool barrier for the per-file data syncs; 9e-2 = a conditional Linux `syncfs` fast path.**
**Renumbering:** the cut 9d spec called chunk checkpoints, partial-file resume and `--resume-verify` "cut 9e". They become **cut 9f**.
Parent spec: `FLUX_FULL_UPDATED_SPEC_V16.md` (sections 148.4, 164, 165-169, 181).

## Why this cut

Cut 9d cut Strict syncs per file from 3.0 to 1.04 and made the small shape 34-35% faster, short of the 40% its spec gated on (PR #82). The
cause was measured, not guessed:

- `perf trace -s`, small shape, Strict: the per-file data `fsync` is about 70% of syscall time (5,011 calls, about 6 ms each). Every other
  syscall is under 1 s in total. The process is blocked for about 12.7 s of a 17.8 s run (5.1 s CPU).
- Spike (throwaway `spike9e.py`, ext4, 5000 x 4 KiB, batches of 64, page cache dropped, 3 reps, order alternated; the idleness probe read busy
  with its busy-core control). Medians:

| Mode | Idle | With a background writer dirtying about 2 GiB |
|---|---|---|
| A write + `fsync` each file (today) | 26.8 s | 81.1 s |
| B write the batch, then `fsync` each, sequentially | 23.1 s | - |
| C write the batch, then one `syncfs` | 1.9 s | 4.8 s |
| D4 / D8 / D16 write the batch, then `fsync` from 4 / 8 / 16 threads | 11.0 / 7.9 / 5.2 s | D16 11.2 s |
| E8 8 threads, each write + `fsync` | 7.9 s | 17.8 s |

Deferring the syncs alone (B) buys 14%; overlapping them (D) buys 59-86%; `syncfs` (C) buys 93-94%. The spike is Python, flat and 4 KiB only; it
is not the engine and the real gain is smaller (reads, renames and about 25 other syscalls per file remain).

## Decisions

1. **A barrier inside the 9d flush.** Under a batched Strict run, `stage_file` skips its step 5 (`writer.sync_all()`) and returns the still-open
   writer in `Staged`. `flush_batch` makes every pending temp's data durable at one barrier, then runs the unchanged 9d sequence (`prepare_many`,
   renames, one `apply_recovery`). The recovery invariant of cut 9c is unchanged: data durable before the `prepared` note, note durable before
   the rename. Normal mode, `files == 1` (batching off) and single-file copy keep the inline step 5.
2. **Threads confined to the barrier.** The barrier runs `sync_all` on the pending writers from `std::thread::scope` workers: `BatchPolicy.sync_threads`
   (default `SYNC_THREADS = 16`, the best measured count) workers, fewer when fewer entries are pending. `sync_threads == 1` is a sequential loop
   on the same code path, and is the seam tests use for a deterministic call order. No new dependency. A worker calls `sync_all` and nothing else:
   never the guard, the heartbeat or the claim store. **The main thread keeps the lease alive:** while workers run it waits on their results with a
   **absolute deadline** (`next_beat = last_beat + heartbeat_interval`; each wait is `recv_timeout(next_beat - now)`, never a fresh full interval, or a
   steady stream of results would reset the timer and starve the heartbeat) and calls the heartbeat when the deadline passes (the heartbeat closure never leaves the main thread), because a barrier
   can take seconds (11 s measured under a background writer) and a record older than the lease threshold can be taken over by another run
   (`lock/obtain.rs:208`). A heartbeat that fails during the wait ends the wait only after the workers are joined (the scope needs them), and is
   then handled as decision 5's heartbeat case; once a heartbeat has failed, the wait goes on joining without calling it again.
   Implementation note (final review, 2026-10-10): the barrier's wake cadence is HALF the heartbeat interval, clamped to at least 1 ms, because `Pulse::beat` writes the record only once a full interval has passed since the last write; a wake every full interval would write only every other time.
3. **`FileSystem::Writer` must be `Sync`.** Today the associated type is only `FileHandle` (`crates/flux-fs/src/fs.rs:125`, and the twin at
   :227); `sync_all` is already `&self`, so the cut adds `Sync` to the bound and the plan checks every implementor (the real std and Windows
   writers, the fake, the `NullWriter` test double at :453) against it.
4. **Per-entry failure attribution.** The barrier returns one result per entry, **positional** (`results[i]` is `entries[i]`'s; the flush splits the entries by it, preserving their order). An entry whose `sync_all` failed is reported
   `Code::StrictDurabilityUnavailable` at `CopyStep::Durability`, exactly the report step 5 gives today, and its temp is discarded under decision 5's rule (removed on the way to `prepare_many`, or on a
   heartbeat failure; kept as a leftover on a lost lock). The other
   entries continue to `prepare_many` in the same flush. No note is written for a failed entry. **Order:** the failed entries are discarded
   AFTER decision 5's heartbeat and guard have passed and BEFORE `prepare_many`, so every removal runs under a lock just checked. `discard` itself checks the
   guard first and keeps the temp as a leftover if the lock is gone (`copy.rs:216`); a loss between that check and the discard is the 9d
   hardening debt (a lost lock noticed only inside a cleanup `discard`), not new here.
5. **The barrier runs BEFORE the existing step-4.1 heartbeat and guard.** `flush_batch` today begins with a heartbeat and a guard
   (`tree.rs` ~1367, "4.1") and then `prepare_many`. 9e inserts the barrier ahead of that pair, so the existing pair becomes the post-barrier
   check: **no heartbeat or guard call is added, and the Strict stall indices of the end-to-end tests do not move** (the stall hook counts
   guard calls; the heartbeats during the wait are not counted; the plan verifies the claim by running the 9d stall tests unchanged). Syncing
   the temps writes nothing new to the destination (their data was written at staging), so it needs no lock; every removal and write still runs
   after the guard. The two outcomes of the pair are the 9d ones and must not be conflated: a **failed heartbeat** (the lock is still held)
   removes every temp of the batch, failed-sync entries included, guarded, and reports any that stays as a leftover; a **lost lock** (the guard)
   writes and removes nothing: entry 0's temp rides in the error and the rest, failed-sync entries included, are reported through `on_report`
   (9d plan ruling 2). **Order after the pair passes:** (a) discard the failed-sync entries' temps, (b) `prepare_many` for the entries that
   synced, (c) the renames, (d) the one `apply_recovery`. Discards come before the notes so that notes exist only for entries that continue; a
   crash between (a) and (b) leaves temps with no note (the continuing entries' are synced and durable, the failed ones' are removed or
   unsynced), and the resume sweep removes every one like any leftover; the files are recopied, as after any 9d crash before the note.
   **Reports are never shadowed:** a failed-sync entry is reported `StrictDurabilityUnavailable` through `on_report` whichever way the pair
   goes (its temp then follows the pair's rule: removed on a heartbeat failure, kept on a lost lock), in addition to the pair's own error.
6. **Metadata before the barrier.** Times and permissions (step 6) are applied to the handle before it is synced, so the barrier also makes
   them durable. This is strictly stronger than today (metadata applied after the sync, never synced), costs nothing, and removes an order
   dependency between the old step 5 and 6. If the plan finds a test that pins the old order, the test is re-derived, not the order kept.
   **The writers are closed right after the barrier returns, unconditionally and BEFORE the heartbeat and guard of decision 5** (not on the exit
   branches; the join has completed by then): `Staged` has no writer today (9d drops it inside `stage_file`, `copy.rs:462`); 9e adds
   `Staged.writer`, and `flush_batch` takes every one out and drops it before
   `prepare_many` and before any rename, so the descriptor lifetime ends where the 9d staging ended it, one step later. When the heartbeat
   fails during the wait the workers are joined first (the scope borrows the writers, so they cannot be dropped earlier), then the writers are
   dropped. The reason is
   descriptor accounting (decision 7) and keeping the 9d descriptor lifetime, not a Windows rename failure: the Windows platform code opens
   temps with `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE` (`dir_windows.rs:631`, `std_fs.rs:624`), so an open handle would not block
   the rename or the unlink there; the Windows CI stays the oracle for any sharing surprise. Closing a descriptor here is not a
   mutation in the section 99 sense: the barrier has just synced every successful entry, so nothing of theirs is dirty and the close writes
   nothing (a failed-sync entry is discarded, and its dirty data is of no interest); the close therefore precedes decision 5's
   heartbeat and guard without contradicting it.
7. **Open-writer budget.** A pending entry now holds a descriptor. A directory frame holds at most 64 entries, but the walker's stack holds
   several frames. `OPEN_WRITERS_MAX = 256` run-wide, **lowered at startup to a quarter of the soft `RLIMIT_NOFILE`** where the platform has one (the walker's
   directory handles, `state.db` and the lock hold descriptors of their own, so a cap equal to the limit would be unsafe; 256 is the ceiling,
   not the assumption): before staging a new entry while the stack already holds that many, the walker flushes pending
   batches shallowest frame first until it is below the cap. The cap is a `BatchPolicy` field (`open_writers`) so a test can set it to 2.
8. **Platforms.** Threads and per-file `sync_all` are portable (Linux `fsync`, Windows `FlushFileBuffers` on the write handle, macOS whatever
   the platform layer's `sync_all` maps to). The gain is not measured off Linux; the spec claims it only for the box measured. Sections 166-169
   (platform mapping, network filesystems, `STRICT_DURABILITY_UNAVAILABLE`) are unchanged: `sync_all` already is the strict primitive.
9. **9e-2, conditional, not designed here.** A Linux `syncfs(2)` on the destination filesystem at the barrier measured 93-94% faster. Two
   facts keep it out of 9e-1: its error is filesystem-wide (a foreign writer's writeback error would abort the batch; safe, since a discarded
   batch loses nothing, but a false failure), and before kernel 5.8 it cannot report writeback errors at all, so a wrong capability guess would
   write `prepared` notes for data not on disk. There is no clean in-process probe (a `uname` check is fragile under backports). 9e-2 ships
   only if a task first (a) finds a probe that does not depend on the kernel version string, and (b) measures `syncfs` under a heavy
   (tens of GiB) writer. Otherwise 9e-2 is dropped and recorded as such. 9e-1 must not preclude it: the barrier is one function behind
   `BatchPolicy`.
10. **Drain after a stop (owner ruling, 2026-10-10, after the capstone).** The drain that follows the walk (9d decision 5(e)) has three modes,
    chosen from how the walk, or a flush during the drain, ended. `Publish`: the walk finished or failed in an ordinary way, so every live frame's batch is flushed in full
    (barrier, notes, renames), unchanged. `KeepAll`: the walk, or a flush during the drain, stopped with a lost lock (`TargetLockBusy`, tested first, so a lost lock noticed
    at a heartbeat step is still a lost lock): nothing is written or removed (decision 8); every pending temporary is kept and reported as a
    `TargetLockBusy` copy failure whose leftover is the temporary, with no barrier, no note and no guard call. `RemoveAll`: the walk, or a flush during the drain,
    stopped at a failed heartbeat (`CopyStep::Heartbeat`) with the lock held: a failed heartbeat is the copy's failure (cut 7b), so every pending
    temporary is removed (guarded), with no barrier and no note; a temporary that cannot be removed is reported as a leftover. This changes
    9d's drain-after-heartbeat-failure behaviour: the drain used to go on and publish the remaining batches (the heartbeat's failure was
    swallowed and the ownership re-checked); it now removes them, and the files are recopied at `--resume`. A stop that first appears during the drain switches the mode for the
    frames not yet drained (capstone round 5).

## Interfaces added or changed (names are the plan's contract; signatures are final in the plan)

- `copy.rs`: `stage_file` takes a `defer_sync: bool` (or an options field); `Staged` gains `writer: Option<F::Writer>`; a new
  `sync_staged_many(entries, threads, beat, interval) -> SyncOutcome`, where `SyncOutcome { results: Vec<Result<()>>, heartbeat: Option<..> }`
  carries the positional per-entry results AND a heartbeat failure that ended the wait (decision 2), so `flush_batch` can route it to decision 5.
- `tree.rs`: `BatchPolicy` gains `sync_threads: usize` and `open_writers: usize`; `Shared` counts open writers; `flush_batch` gains the
  barrier step ahead of the existing beat and guard (decision 5), the discard-before-`prepare_many` order, and two counters in the outcome:
  `barrier_max` (the longest barrier) and `cap_flushes` (flushes forced by the open-writer cap), which the acceptance run reads and the plan
  surfaces through the lightest existing channel.
- Constants `SYNC_THREADS = 16`, `OPEN_WRITERS_MAX = 256`.

## Behaviour that does not change

Normal mode, single-file copy, `files == 1`, the recovery matrix and its verdicts, the claim and note schema (`meta.format` stays 2), every
flush trigger of 9d, and the order data -> note -> rename.

## Tests

- **Order:** with `sync_threads == 1`, the call log shows every `sync_all` of a batch before `claim_prepare_many`, which precedes the renames.
- **Partial failure:** one entry's `sync_all` is faulted; that entry's temp is discarded and reported `StrictDurabilityUnavailable`, the others
  are published and claimed, and `prepare_many` carries only the successful entries.
- **Lease:** with a barrier that outlasts `heartbeat_interval` (a fake sync that sleeps), the heartbeat is called during the wait, from the main
  thread only.
- **Heartbeat failure after the barrier:** every temp removed, none left, no note. **Lost lock after the barrier:** every temp kept as a
  leftover, no note, nothing removed. Each is red under ITS OWN mutant, because the "skip the barrier" mutant leaves both green (the post-barrier
  check still fails and still leaves the temps): drop the heartbeat/guard pair (or move the barrier after it) (the lost-lock test then renames and notes); treat a failed
  heartbeat like a lost lock (the heartbeat test then leaves temps behind).
- **Deadline:** a fake whose workers each finish just inside `heartbeat_interval` still sees the heartbeat called once the interval has passed
  in total (mutant: a fresh full timeout per receive).
- **Mutants for the rest:** writers closed: keep the writers until after `prepare_many`; lease: drop the heartbeat from the wait loop;
  equivalence: lose or reorder one entry's result; cap: ignore `open_writers`; thread safety is carried by the `Writer: Sync` bound at compile
  time (no runtime mutant); the re-derived 9d tests are oracles, not new tests.
- **Writers closed:** at the moment of the first heartbeat call after the barrier, no writer of the batch is alive (the fake records each writer's
  drop in its call log, so the test compares the drop's position with the heartbeat's), on the success path and on both failure paths.
- **Equivalence:** the same tree under `sync_threads` 1 and 16 produces the same outcome set, claims and counters, **excluding `barrier_max`** (a duration). A separate test pins `barrier_max`: a fake whose
  `sync_all` sleeps 50 ms yields `barrier_max >= 50 ms` (mutant: a hardcoded zero).
- **Lost lock after the barrier:** every temp kept as a leftover, no note, no removal.
- **Open-writer cap:** with `open_writers == 2` a three-directory tree never holds more than 2 writers (observed through the fake) and every file
  still arrives.
- **Thread safety:** the fake's call log and fault table are exercised from several threads without a race (the fake's own tests).
- **Re-derived:** the 9d end-to-end stall constants in `recovery.rs`/`run.rs`, and the cut 9c/9d `run/tests.rs` cases whose call sequence moved.
  Oracle: the 9d tests; a value that looks wrong is surfaced, not edited to match.
- **Non-vacuity:** each new test is shown red under a logic mutant, named per test: skip the barrier or sync after `prepare_many` (the order
  test); swallow a failed result (the partial-failure test); the mutants named above for the lost-lock, heartbeat and deadline tests.

## Measurement (acceptance)

Same protocol as 9d: `measure9d.py`-style driver, page cache dropped, 2 passes x 3 runs alternating binary order, strace sync counts,
criterion `small_1000x4kib`, idleness census with a busy-core control, background load stated. **Three binaries in one session**, alternating:
pre-9d (`49fabc4`), the merged 9d head, and 9e. Absolute times drift between sessions (the same small shape read 17.8 s and 33.0 s on the
2026-10-10 runs, ratio unchanged), so a gate is a ratio taken inside one session, never against a figure from an earlier one. Gates:

- Strict small and flat5000: at least 50% faster than the 9d median of the same session (the spike projects about 74% below the pre-9d median).
- Syncs per file stay at most 1.15 AND at least 0.99 (the barrier changes when they happen, not how many; a lower bound so that a mutant that
  skips the barrier cannot pass on zero syncs). The order test and the crash tests carry the durability claim; the speed gate does not.
- Normal unchanged within the baseline spread; Strict large and mixed not worse than their spread; peak RSS within 2 MiB of the 9d figure.
- Reported, not gated: nested 500x10, one file per directory (no benefit by design), a run with a background writer.
If a gate fails the PR states it, as PR #82 did; thresholds are not moved.

## Known limits (to be recorded in `TODO.md` "Cut 9e known limits")

1. First threads in the engine. The shipped binary builds with `panic = "abort"` (`Cargo.toml:111`), so a worker panic is a hard crash, not a
   reported failure; it is safe for the recovery matrix (no note exists yet, the sweep removes the temps) but is not a handled error. In
   unwinding test builds the panic propagates out of the scope and fails the test. No catch-and-report path is built. A thread the OS refuses to spawn is not a panic: a worker that cannot be spawned is skipped; with no worker spawned the syncs run on the main thread.
2. The barrier lengthens the window between the temps' creation and the notes by up to the slowest sync; the post-barrier guard bounds the lock
   risk, not the source-change window (9d known limit 5 still applies).
3. A shallow pending batch still waits while a subtree is walked (9d limit 5), now holding descriptors; the cap in decision 7 bounds that.
4. The gain is measured on Linux ext4 only.
5. One file per directory gets no benefit (9d limit 1).
6. The heartbeat is itself a synced write (`lock/held.rs:48`), so on a filesystem where a burst of concurrent `fsync` calls holds the journal, the
   heartbeat can queue behind the barrier and arrive late. Today's per-file `fsync` has the same exposure at a smaller scale. Not measured here:
   the acceptance run records the longest barrier and the longest heartbeat gap, and reports them.
7. A `sync_all` that hangs (a dead network mount) hangs the run exactly as today's inline sync does: the system call is not cancellable. The
   main thread keeps the heartbeat going during the wait, so the lease stays alive; no timeout or cancel is built, and none is claimed.

## Out of scope

Chunk checkpoints, partial-file resume, `--resume-verify` (cut 9f); whole-file worker pools and `--workers` (a later cut); `syncfs` unless
decision 9's two conditions are met; a per-file directory sync (cut 9c known limit).

## Stand-downs

- Whole-file parallelism (agy's round 1 and 2 first pick): rejected for 9e; the spike's E8 was slower than the barrier (7.9 s against 5.2 s)
  and it would put threads through the guard, heartbeat and call-log model. agy moved to the barrier pool in round 2.
- `syncfs` as the 9e core (the owner's driver's first synthesis): deferred to 9e-2 on the two conditions above; agy rejects it outright. The
  two positions were left unresolved by AGY-NEGOTIATE and the owner chose the split.

- Panel rounds 1-5 (agy, 2026-10-10). REJECTED: "`next_beat - now` panics when `now` is later" (measured on rustc 1.99.0: `Instant - Instant`
  returns 0ns); "a heartbeat failure inside the rename loop is misclassified GONE" (`tree.rs` ~1487-1503, the 9d stop-held path removes the later
  temps before the `apply_recovery` that discards the notes, and it is 9d code, not 9e); "closing the writers before the guard breaks the
  lost-lock rule" (after a successful barrier nothing is dirty, so the close writes nothing; decision 6); "discard-failed-sync is catastrophic
  because a foreign `syncfs` error aborts the batch" (that is `syncfs`, deferred to 9e-2; a discarded batch loses no data). UNVERIFIED-ACCEPTED:
  the heartbeat queueing behind the journal (known limit 6), measured at acceptance. Round 5 ended with all findings folded and no round-6 run
  (the round cap asks the owner); agy's round 3 and round 5 replies were delivered through its reply file after the driver reported a stalled
  peer, so those two rounds carry no echo or verdict token.
- Panel round 6 (2026-10-10). FOLDED: decision 4 and 5 now say the same about a failed-sync temp; the Windows rename claim is replaced by the
  descriptor-accounting reason (the platform code opens temps with `FILE_SHARE_DELETE`); the duplicate drop wording is removed;
  `sync_staged_many` returns a `SyncOutcome` carrying a heartbeat failure; `barrier_max` has its own test and is excluded from the equivalence
  test. REJECTED: "closing a failed-sync entry's handle on Windows writes the MFT without the lock, violating section 99" (unsupported: nothing in
  `dir_windows.rs` or `std_fs.rs` makes `CloseHandle` a destination mutation, and the temp is this run's own file; a close creates no write that
  the earlier staging did not).

## Self-audit (exhaustiveness)

- Contracts: names and fields are listed under Interfaces; signatures are fixed in the plan (the plan writes against merged 9d code, per the
  plan-vs-spec discipline), which is where `defer_sync`'s exact form and the `Writer: Sync` bound resolve.
- Numbers: 16 threads and 256 writers are measured/derived; the 256 cap is an assumption against the 1024 descriptor default and is checked by
  the cap test, not measured under load.
- Cases: lost lock after the barrier, partial sync failure, worker panic, cap, `sync_threads == 1`, Normal and `files == 1` are covered
  under Decisions or Tests. Unresolved here and owned by the plan: whether any existing test pins the old step 5 / 6 order (decision 6).
- Every owner-approved design point (mechanism, failure, handle budget, tests, measurement, out of scope) maps to a section above.
