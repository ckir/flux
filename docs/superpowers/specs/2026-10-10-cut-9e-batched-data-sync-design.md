# Cut 9e: a batched, parallel data-sync barrier for Strict tree publications

Status: DRAFT, design approved by the owner in chat on 2026-10-10; the written spec awaits review. Stacked on cut 9d (PR #82; branch
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
   never the guard, the heartbeat, the call log's ordering-sensitive methods or the claim store, so lock-loss behaviour is untouched.
3. **`FileSystem::Writer` must be `Sync`** (`sync_all(&self)` is already `&self`). Real and fake writers satisfy it; the plan verifies each
   implementor and widens the trait bound only if the compiler requires it.
4. **Per-entry failure attribution.** The barrier returns one result per entry. An entry whose `sync_all` failed has its temp discarded and is
   reported `Code::StrictDurabilityUnavailable` at `CopyStep::Durability`, exactly the report step 5 gives today. The other entries continue
   to `prepare_many` in the same flush. No note is written for a failed entry.
5. **An extra guard after the barrier.** The barrier can take seconds, so `flush_batch` runs one more heartbeat and guard after it, before
   `prepare_many`. A lost lock there writes and removes nothing (the 9d decision 8 rule): every entry's temp is kept as a leftover, none discarded.
   This adds one guard call per flush and shifts every Strict stall index of the end-to-end tests; the plan re-derives them as 9d did.
6. **Metadata before the barrier.** Times and permissions (step 6) are applied to the handle before it is synced, so the barrier also makes
   them durable. This is strictly stronger than today (metadata applied after the sync, never synced), costs nothing, and removes an order
   dependency between the old step 5 and 6. If the plan finds a test that pins the old order, the test is re-derived, not the order kept.
7. **Open-writer budget.** A pending entry now holds a descriptor. A directory frame holds at most 64 entries, but the walker's stack holds
   several frames. `OPEN_WRITERS_MAX = 256` run-wide: before staging a new entry while the stack already holds that many, the walker flushes pending
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

## Interfaces added or changed (names are the plan's contract; signatures are final in the plan)

- `copy.rs`: `stage_file` takes a `defer_sync: bool` (or an options field); `Staged` gains `writer: Option<F::Writer>`; a new
  `sync_staged_many(entries, threads) -> Vec<Result<()>>`.
- `tree.rs`: `BatchPolicy` gains `sync_threads: usize` and `open_writers: usize`; `Shared` counts open writers; `flush_batch` gains the
  barrier step and the post-barrier beat and guard.
- Constants `SYNC_THREADS = 16`, `OPEN_WRITERS_MAX = 256`.

## Behaviour that does not change

Normal mode, single-file copy, `files == 1`, the recovery matrix and its verdicts, the claim and note schema (`meta.format` stays 2), every
flush trigger of 9d, and the order data -> note -> rename.

## Tests

- **Order:** with `sync_threads == 1`, the call log shows every `sync_all` of a batch before `claim_prepare_many`, which precedes the renames.
- **Partial failure:** one entry's `sync_all` is faulted; that entry's temp is discarded and reported `StrictDurabilityUnavailable`, the others
  are published and claimed, and `prepare_many` carries only the successful entries.
- **Equivalence:** the same tree under `sync_threads` 1 and 16 produces the same outcome set, claims and counters.
- **Lost lock after the barrier:** every temp kept as a leftover, no note, no removal.
- **Open-writer cap:** with `open_writers == 2` a three-directory tree never holds more than 2 writers (observed through the fake) and every file
  still arrives.
- **Thread safety:** the fake's call log and fault table are exercised from several threads without a race (the fake's own tests).
- **Re-derived:** the 9d end-to-end stall constants in `recovery.rs`/`run.rs`, and the cut 9c/9d `run/tests.rs` cases whose call sequence moved.
  Oracle: the 9d tests; a value that looks wrong is surfaced, not edited to match.
- **Non-vacuity:** each new test is shown red under a logic mutant (skip the barrier; sync after `prepare_many`; swallow a failed result).

## Measurement (acceptance)

Same protocol as 9d: `measure9d.py`-style driver, page cache dropped, 2 passes x 3 runs alternating binary order, strace sync counts,
criterion `small_1000x4kib`, idleness census with a busy-core control, background load stated. Baselines: the 9d "after" figures (small 17.8 s,
flat5000 16.3 s) and the pre-9d ones. Gates:

- Strict small and flat5000: at least 50% faster than the 9d "after" median (the spike projects about 74% below the pre-9d median).
- Syncs per file stay at most 1.15 (the barrier changes when they happen, not how many).
- Normal unchanged within the baseline spread; Strict large and mixed not worse than their spread; peak RSS within 2 MiB of the 9d figure.
- Reported, not gated: nested 500x10, one file per directory (no benefit by design), a run with a background writer.
If a gate fails the PR states it, as PR #82 did; thresholds are not moved.

## Known limits (to be recorded in `TODO.md` "Cut 9e known limits")

1. First threads in the engine: a worker panic is caught at the scope's join and reported as `StrictDurabilityUnavailable` for the entries it held.
2. The barrier lengthens the window between the temps' creation and the notes by up to the slowest sync; the post-barrier guard bounds the lock
   risk, not the source-change window (9d known limit 5 still applies).
3. A shallow pending batch still waits while a subtree is walked (9d limit 5), now holding descriptors; the cap in decision 7 bounds that.
4. The gain is measured on Linux ext4 only.
5. One file per directory gets no benefit (9d limit 1).

## Out of scope

Chunk checkpoints, partial-file resume, `--resume-verify` (cut 9f); whole-file worker pools and `--workers` (a later cut); `syncfs` unless
decision 9's two conditions are met; a per-file directory sync (cut 9c known limit).

## Stand-downs

- Whole-file parallelism (agy's round 1 and 2 first pick): rejected for 9e; the spike's E8 was slower than the barrier (7.9 s against 5.2 s)
  and it would put threads through the guard, heartbeat and call-log model. agy moved to the barrier pool in round 2.
- `syncfs` as the 9e core (the owner's driver's first synthesis): deferred to 9e-2 on the two conditions above; agy rejects it outright. The
  two positions were left unresolved by AGY-NEGOTIATE and the owner chose the split.

## Self-audit (exhaustiveness)

- Contracts: names and fields are listed under Interfaces; signatures are fixed in the plan (the plan writes against merged 9d code, per the
  plan-vs-spec discipline), which is where `defer_sync`'s exact form and the `Writer: Sync` bound resolve.
- Numbers: 16 threads and 256 writers are measured/derived; the 256 cap is an assumption against the 1024 descriptor default and is checked by
  the cap test, not measured under load.
- Cases: lost lock after the barrier, partial sync failure, worker panic, cap, `sync_threads == 1`, Normal and `files == 1` are covered
  under Decisions or Tests. Unresolved here and owned by the plan: whether any existing test pins the old step 5 / 6 order (decision 6).
- Every owner-approved design point (mechanism, failure, handle budget, tests, measurement, out of scope) maps to a section above.
