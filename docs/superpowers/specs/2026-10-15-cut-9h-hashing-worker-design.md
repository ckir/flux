# Cut 9h: checkpoint hashing on a worker thread (design)

Status: design for review. Against main `14dde8d` (the code is unchanged since `f52f5bd`; later commits are docs). Every code citation below was read at that commit.

## 1. Why

Cut 9f-1 digests each 1 MiB chunk of a large file's source bytes with BLAKE3 on the copy thread, between the write of one read buffer and the next read (`crates/flux-core/src/copy.rs` stage_file loop, `FileSink::wrote` in `crates/flux-core/src/checkpoint/sink.rs`). Files of at least `CHECKPOINT_MIN_LEN` (64 MiB) are affected. Measured 2026-10-15 on this box (ext4, 8 vCPUs), 100 files of 64 MiB, release builds, cache dropped per run:

- `perf`: user CPU 4.4-4.5 s (Strict) and 4.50-4.53 s (Normal) versus 0.21-0.30 s before cut 9f-1 plan 2; `blake3_hash_many_avx2` 27.5% / 28.7% of samples, on the `flux` main thread.
- A/B with the hashing skipped (a throwaway mutant of `FileSink::wrote`), 4 runs x 2 passes: Normal wall median 22.24 s (19.68-33.52) with hashing versus 19.15 s (16.38-21.33) without (-14%; ratio 0.77 and 0.89 per pass); Strict 24.57 s (21.40-25.92) versus 23.85 s (22.68-25.93) (-3%, ranges overlap). Background load (three `agy` sessions, `claude`) was uncontrolled; idle census 0.07-0.09, busy-core control 0.186.

So hashing is on the wall-time critical path under Normal and about free under Strict. The cut-9f-1 design lists "a measurable slowdown of an uninterrupted large copy that the acceptance run does not report" as an anti-goal (`2026-10-12-cut-9f1-chunk-checkpoints-design.md`, anti-goals). The tree copy is sequential (no whole-file worker pools; a stated non-goal of 9f-1), so most cores are idle during a large-file copy: the work can move to another core without competing with the copy.

agy was consulted first (seam `hash-off-copy-thread-fork.md`; ALIGNED, recommended leaving it; its "masked by I/O" argument is refuted for Normal by the A/B above, its spec citation and its "8 copy threads" scenario were wrong). The owner chose to measure first, then to design the worker.

## 2. Goals, non-goals, anti-goals

Goals
- G1. Under Normal, a large-file tree copy recovers at least half of the measured gap to the no-hash build (see section 8 for the measurable form).
- G2. The log a run writes is byte-for-byte the log the inline implementation writes for the same source, durability and policy: same `FileBegin`, same `Range` boundaries, same digests, same order. Resume, `--resume-verify`, quarantine and WAL replay are untouched.
- G3. Every invariant of cut 9f-1 keeps holding (section 4).

Non-goals
- No change to the digest (BLAKE3 over exactly one 1 MiB chunk of source bytes), `CHUNK_SIZE`, the record format, `wal/` or `decide`/`decide_full`.
- No hashing parallelism inside one file (no `rayon`, no chunk-level fan-out). One worker per file.
- The single-file log-free path (cut 9g, `Hook::LogFree`) has no sink and is untouched. The hashing that `decide`/`decide_full` do on `--resume` is a separate cost and out of scope.
- No worker for files below `min_len`.

Anti-goals (a failure even if every test passes)
- A digest in the log for bytes the destination does not hold (section 4, I1).
- A heartbeat starved by a blocked worker or a full channel (I3).
- A worker that outlives its file, or a `Range`/`Close` reaching the log after `Discard` (I5).
- A slower copy than today on a machine with one core, or a memory cost that scales with file size.

## 3. Design

The hashing happens AFTER the write, on a worker, with buffers handed over by value and recycled. The ordering property then holds by construction: a buffer reaches the worker only after `writer.write_all` returned for it, so no digest can exist for bytes the destination has not been handed.

3.1 Pool. Per large file, `POOL = 4` buffers of `copy_buffer_len(src_len)` bytes (256 KiB at the cap), i.e. at most 1 MiB per file, allocated once per file next to today's single `buf` (my call: 4 lets the copy thread read and write one buffer while up to three are queued or hashing; the plan may tune it under measurement, never below 2). The copy loop takes a free buffer, reads into it, writes it, then submits it to the worker; the worker returns it through a free-list channel.

3.2 Worker. One `'static` thread per large file (`std::thread::Builder::new().name("flux-hash").spawn`, the way the WAL writer thread is spawned, `wal/writer.rs:286`; it is a plain thread, NOT a `std::thread::scope` thread, see 3.7), spawned LAZILY at the first `submit` (3.4), which is necessarily after `FileSink::begin`, after `record_proven` and after the `carry` has been hashed inline, so the main thread has already sent `FileBegin` and any proven `Range`s and the worker only ever appends later ranges (order on the WAL channel: `FileBegin`, proven `Range`s, worker `Range`s in offset order, then `Close`/`Discard` from the main thread). The worker owns the `ChunkHasher` and the pending-digests vector (today's `FileSink.hasher`, `digests`, `range_start`) and applies exactly today's rule: after each completed chunk push the digest; when `range_chunks` digests are pending send one `Range` ending at `(index + 1) * CHUNK_SIZE`; then check `sender.wal_fault()` (as `wrote` does today).

3.3 Sending from the worker. The `Heartbeat` is a `dyn Fn() + 'g` and is not shareable with another thread, so the worker never beats. A `'static` thread cannot borrow the `WalSender` held by `Checkpoints`, so `WalSender` gains one accessor returning a small cloneable link, `WalLink<W>`, holding a clone of its `SyncSender<Msg<W>>` and a clone of its `Arc<Mutex<Failure>>`, with two methods: `try_send(Msg<W>) -> Result<(), TrySendError<..>>` and `fault() -> Option<WalFault>` (the same read `WalSender::wal_fault` does). A bare `SyncSender` clone is not enough: `wal_fault()` reads `failure` (`wal/writer.rs:178`). The worker's `Range` sends loop: `try_send`; on `Full` sleep `SEND_RETRY` (10 ms) and retry; give up when the file's cancel flag is set, the channel is disconnected, or `fault()` is `Some`. The worker must never own a second `WalSender`, whose `Drop` sets `abandoned` unless the sender was stopped (`wal/writer.rs:138-144`), which would make the log read as abandoned; a `WalLink` has no such `Drop`. Because a link keeps the channel connected, the worker must be joined (or have exited) before the run stops the log; 3.6 guarantees that on every exit path. The `'static` bound holds: the WAL thread already moves `Msg<D::Writer>` and the receiver into a plain spawned thread (`wal/writer.rs:286`), and `Checkpoints` requires `D: DirHandle + Send + 'static` (`checkpoint/mod.rs:69`).

3.4 Main thread. `FileSink` keeps ONE public API and two internal modes chosen at `begin` (my call, from the builder check): `Inline` (today's behaviour) and `Worker`. `FileSink::wrote(&[u8], beat)` stays as today's inline hashing step and is used for exactly two things: the `carry` bytes (below) and `Inline` mode. The new entry point is `FileSink::submit(&mut self, buf: Vec<u8>, n: usize, beat) -> Result<Vec<u8>, SinkError>`: `buf` is a filled pool buffer of which the first `n` bytes are valid (the last read of a file is short), and the return value is a buffer to read the next chunk into.
- `Inline` mode: `submit` is `wrote(&buf[..n])` and returns the same `buf`. So `stage_file` has ONE loop shape for a file with a sink (it exchanges an owned `buf` through `submit`); files with no sink keep today's single-buffer loop unchanged. The inline path is production code (the fallback of 3.8) and the oracle for the worker.
- `Worker` mode: the first `submit` spawns the worker (so `wrote` for the carry has already run on the main thread and the `ChunkHasher`, pending digests and `range_start` move into the worker with their state). `wrote` after the first `submit` is a `debug_assert` failure.
- Every `submit` begins by checking the worker's fault: a shared `AtomicBool` that the worker sets when it records a fault (WAL died, send gave up), and the result channel's disconnection (worker panicked or exited). Either returns the stored `SinkError` at once. This makes the stop bound hold even while free buffers are queued (builder-check Q5).
- If a free buffer is available it is returned. If the pool is exhausted the main thread waits for the worker with `recv_timeout(SEND_RETRY)` and calls `beat()` on every timeout (the loop `FileSink::complete` already uses for the `Close` ack). This keeps the heartbeat alive while the worker is blocked on a full WAL channel (I3).
- The main thread polls the log itself at the top of EVERY `submit`: `self.run.sender.wal_fault()` (the writer thread records a log fault into the shared `Failure`; the same read `wrote` does today, but per 256 KiB buffer instead of per completed 1 MiB chunk), then the worker's own fault flag (set when the worker's own send fails or its channel disconnects). A log fault therefore stops the file within one buffer of becoming visible, which is TIGHTER than today (`FileSink::wrote` checks only when a chunk completes, `sink.rs:177-186`, so up to about 1 MiB of further copying). Only a fault that exists solely in the worker (its own send failure) can lag, by at most `POOL` buffers. Decision D1 (settled by the D1/D6 negotiation, section 9).
- `carry` (`decide`'s consumed prefix, `copy.rs` the `!carry.is_empty()` block) is written and then hashed INLINE with `wrote` on the main thread, before the worker exists. Its size is bounded by `decide`'s own read (`source_buf[..got]`, `decide.rs`), so it is a one-off, small cost, it never enters the pool, and I7 holds (builder-check Q4). Pool buffers are all exactly `copy_buffer_len` long; a recycled buffer is resized to full length before the next read.

3.5 `complete`. If no `submit` ever happened (the whole remaining file was `carry`, or the first read hit EOF) there is no worker: `complete` runs today's tail directly on the main thread's state, with no channel to close and nothing to join. Otherwise the main thread closes the submit channel; the worker drains every queued buffer, then returns `Result<(hasher, pending digests, range_start), SinkError>` as its join result (an `Err` when it faulted while draining; `complete` returns it). The main thread waits for it with `recv_timeout` + `beat()`, then runs today's `complete` tail unchanged: push the final short chunk's digest, `send_range`, `Close` with ack, wait for the ack with `beat()` (`FileSink::complete`, `sink.rs`).

3.6 `abort` and `Drop`. `abort` sets the cancel flag, closes the submit channel and waits for the worker with a `recv_timeout(SEND_RETRY)` loop that calls `beat()` on every timeout, exactly as `submit` and `complete` do (errors from `beat()` are ignored here, as `abort` already ignores them for its `Discard` send); only after the worker is gone does it send `Discard`. After `abort` returns, no `Range` can follow `Discard` (I5). `FileSink::abort`'s doc comment ("without waiting") is updated: it still flushes nothing, but it now waits for the worker. The existing rule "a `FileSink` must end through `complete` or `abort`" (the `Drop` debug-assert) is kept and extended: a dropped sink with a live worker cancels and joins it first.

3.7 Why no `std::thread::scope`. Two panel rounds found that a scoped worker is the wrong tool here. In `stage_file` the `sink` is declared BEFORE the `'stream: { ... }` block (`copy.rs:778`, block from 779) and `abort` runs AFTER it (`copy.rs:848-852`); the block exits with `break 'stream Err(..)` on read, write and heartbeat failures. A `scope` wrapped around only that block would wait for a worker still blocked on `submit_rx.recv()`, because `abort` has not run: an unconditional hang on any failed read, write or beat (round 1, verified). Enclosing `abort` in the scope does not help a panic, which skips `abort` and then blocks in the scope's destructor (round 2). And `spawn_scoped` needs a `&'scope Scope` that neither `FileSink::begin` nor `submit` has, which would put a lifetime parameter on `FileSink` and across the whole streaming block (round 2). A plain `'static` thread avoids all three: `FileSink` owns the `JoinHandle`; `abort`, `complete` and `Drop` stop and join it (3.5, 3.6); and if a path ever skips them (a panic unwinding past the sink), dropping the sink's submit sender disconnects the worker, which then exits on its own; the detached thread holds only a `WalLink` and its buffers, so it cannot hang the run. The cost is the `WalLink` accessor (3.3) and that the join is explicit code, not scope-enforced; tests 4b and 4c pin both.

3.8 When the worker is not used. Inline hashing (today's code path, kept verbatim as the fallback) runs when: the file is below `min_len` (no sink at all); `std::thread::available_parallelism()` is 1; or the thread spawn fails (the Strict barrier's `Builder::spawn_scoped` fallback to inline work, `copy.rs:1175-1192`, is the pattern). The fallback keeps one code path producing the identical log, so tests can run both and compare (G2). My call: the inline path is the oracle for the worker's output.

3.9 Durability modes. Normal and Strict take the same worker path (the Strict duplicate descriptor `dup` and the writer thread's data sync are untouched). Strict showed no measurable wall gain in the A/B, but a mode-dependent code path is a second path to test; one path is simpler and correct for both.

## 4. Invariants (each pinned by a test in section 7)

- I1. No `Range` covering `[a, b)` is in the log while the destination holds fewer than `b` bytes from this attempt's writes. By construction (3), and pinned by the failure-injection test.
- I2. Digest order and boundaries equal the inline implementation's (G2).
- I3. `beat()` is called at least once per copy-buffer period and at least once per `SEND_RETRY` while the main thread waits on the worker (including in `abort` and `complete`), the pool or the `Close` ack. A full WAL channel never starves the heartbeat.
- I4. After the log has failed, the file stops with `SinkError::Wal` at the first `submit` after the fault is recorded in `Failure.wal` (at most one buffer of further copying); a worker-only fault (its own send failed, or the worker is gone) stops the file within `POOL` buffers. A lost lock (`guard()` failing in the main loop, section 99) stops the main thread, and `abort` stops the worker before any further WAL send.
- I5. After `abort` or `complete` returns, no worker exists and nothing is sent to the log on the file's behalf.
- I6. A worker panic is a `SinkError::Wal("the WAL writer thread is gone")`-class stop (the file's partial is kept, `StreamFail::Keep`), never a hang and never a silent success: the submit and join channels disconnect, and the main thread maps a disconnected worker to a `WalFault` of the same class (`Code::WalWriteFailed`, `StreamFail::Keep`) whose message says the checkpoint hashing worker stopped, so a worker panic is not reported as the log writer being gone (panel round 2, low).
- I7. At most `POOL * copy_buffer_len` bytes per file are held by the pool; memory does not grow with file size or queue depth.

## 5. Failure cases (state combinations)

| Event | Behaviour |
|---|---|
| `write_all` fails on buffer k | The buffer is never submitted; chunks completed by earlier buffers may already be logged (their bytes were written); `StreamFail::Discard` as today; `abort` joins the worker. |
| Source read error | Same: `abort`, no further sends. |
| WAL append fails (log dies) | The writer thread records `Failure.wal` and discards; the main thread sees it at the top of the next `submit` (`wal_fault()`), returns `SinkError::Wal`, and `stage_file` returns `StreamFail::Keep` (`sink_fail`, `copy.rs:510`) after `abort` joins the worker. The worker's own next send or `fault()` check also fails and it exits. |
| Lock lost mid-file | The main loop's `guard()`/`beat()` fails first: `StreamFail::Discard`, `abort`, join. The worker never beats and never sends after the cancel flag. |
| Kill (SIGKILL) mid-file | Same as today's crash: the log holds ranges for written chunks only (I1); resume re-validates against real bytes (decision 7.4 of 9f-1), so a range lost with the unflushed batch only costs a re-hash. |
| Pool exhausted, WAL channel full | Main waits with `beat()` every 10 ms; the worker retries sends every 10 ms; bounded by `StreamFail` on a lost lock. |
| Resume with a proven prefix (`full`) | `record_proven` runs on the main thread before the worker spawns; numbering starts at `valid_through / CHUNK_SIZE` exactly as today (`ChunkHasher::from_chunk`). |
| File length an exact multiple of `CHUNK_SIZE`, shorter than one buffer, or zero tail | Same digests as inline (the hasher is unchanged; only its thread changes). |
| `available_parallelism() == 1` or spawn failure | Inline path. |

## 6. Interfaces

- `FileSink::submit(&mut self, buf: Vec<u8>, n: usize, beat: &Heartbeat<'_>) -> Result<Vec<u8>, SinkError>` is added; `wrote` stays (carry and `Inline` mode); `begin`, `record_proven`, `complete`, `abort` keep their signatures. The `submit` call site in the `stage_file` loop replaces the `wrote(&buf[..n])` call there; the existing sink tests that call `wrote` keep covering `Inline` mode, and the "stops at the first chunk after the WAL died" test stays an `Inline` test while the worker gets test 6. The `wrote(` call sites in `sink.rs` tests and the two in `copy.rs` are the compiler's list when the signature changes (search repo-wide, exclude the signature, do not pin a count).
- A private `Worker` type in `checkpoint/` owns the thread handle, the submit sender, the free-list receiver, the cancel flag and the result channel.
- No change to `wal/`, `Msg`, record encoding, `run/`, `tree.rs` or the CLI. `StreamFail` and `sink_fail` keep their mapping.

## 7. Tests (written first; the oracle is named)

Oracle: the inline path's log for the same input, and invariants I1-I7.

1. `the_worker_log_equals_the_inline_log`: run `FileSink` inline and with the worker over the same data (sizes: below one chunk, exactly one chunk, 3.5 chunks, a multiple of `POOL * buffer`, with and without a proven prefix and a `carry`), compare the decoded `Range` sequences for equality, AND compare each logged digest with `blake3::hash` of the corresponding source slice computed directly in the test (worker and inline share `ChunkHasher`, so equality between them alone would pass a shared hashing bug). Mutant: hash the buffer before an offset shift (wrong chunk boundary) turns it red.
2. `no_range_for_a_buffer_whose_write_failed` (I1): `FaultFs` fails the write of the buffer that would complete chunk k; assert the log holds no range reaching past the last fully written chunk. Mutant: submit the buffer before `write_all` turns this red (the new test must be the one that goes red).
3. `complete_drains_every_queued_buffer_before_close` (I2/I5): submit more buffers than the worker has consumed (a `FaultFs` hook holds the worker's send), release, assert the last `Range` precedes `Close` and covers `source_len`.
4. `abort_joins_the_worker_and_nothing_follows_discard` (I5): abort with buffers queued and a held send; assert the log order and that no thread named `flux-hash` outlives the call.
4b. `a_failed_read_write_or_beat_does_not_hang` (I5, 3.7): inject each of a source read error, a `write_all` error and a failing `beat()` after the worker has started; assert `stage_file` returns the matching error within a deadline (run on a helper thread with a timeout so a hang fails the test instead of hanging the suite), and that the `flux-hash` thread is gone. Mutant: drop the `abort` call (or the worker join inside it) so the run waits on or leaks a live worker turns it red.
4c. `a_panic_in_the_copy_thread_does_not_leave_a_worker_blocked` (3.7): panic inside the copy loop (a test hook) under `catch_unwind` with a worker running; assert the `flux-hash` thread exits within a deadline after the sink is dropped. Mutant: keep a submit-sender clone alive in the worker's own state so the channel never disconnects turns it red.
5. `a_full_channel_does_not_starve_the_heartbeat` (I3): channel cap 1, held writer, pool exhausted; assert `beat` is called repeatedly while the worker is held blocked (a counter that must reach at least 3 within a generous deadline, polled, never an exact time window), and that a `beat` returning an error ends the wait with `SinkError::Heartbeat`. Mutant: drop the `beat()` in the pool wait turns it red.
6. `a_dead_log_stops_the_file_within_one_buffer` (I4): kill the log (`FaultFs` append failure); count buffers accepted after the fault is recorded; assert `<= 1`; error is `SinkError::Wal`. A second case (`a_worker_only_fault_stops_the_file_within_pool_buffers`): make only the worker's send fail (a test seam on the link) and assert `<= POOL`. Mutant: drop the main-thread `wal_fault()` poll in `submit` turns the first red.
7. `a_panicking_worker_is_a_wal_stop_not_a_hang` (I6): a test-only panic hook in the worker; assert `SinkError::Wal` with the "gone" text, within a deadline.
8. `the_inline_fallback_is_used_for_one_cpu_and_a_failed_spawn`: a test seam for `available_parallelism` and for spawn failure; assert the log equals the worker's.
9. End-to-end: the existing real-process tests `a_killed_large_file_resumes_from_its_validated_prefix` and its siblings (`crates/flux-cli/tests/run.rs:604` onward) must pass UNCHANGED against the worker build; they already assert byte equality and the reuse report. No new baseline constant is introduced.
The plan names the `FaultFs` hooks each test needs against the code that exists then (`on_nth` hooks on `sync_all` exist; a write hook may need adding: a plan task, not a spec decision).

## 8. Acceptance (owner-gated measurement, top level, idle machine, no agents, two runs, quote the range, state uncontrolled load)

- 100 files of 64 MiB, Normal, release build, cache dropped, 4 runs x 2 passes alternating binary order, the worker build against main `14dde8d`: G1 holds if the worker build's wall median is at least 1.5 s below main's (the measured hash gap is about 3.1 s; "half" is my call) with ranges stated. If it is not, the cut is reverted or closed as not worth its complexity, and the TODO entry records the figures.
- Strict on the same case: not slower than main beyond the measured spread (ranges overlap or better).
- `perf stat` user CPU stays about equal (the work moved, it was not removed): record it.
- The `mixed` case and the single-file case: no regression beyond spread.
- Gate: `just check` and `cargo clippy --workspace --all-targets --release -- -D warnings`; CI on every job including Windows (the cancel-flag/join path must be identical there; `thread::Builder::spawn` and `available_parallelism` are portable).

## 9. Deviations and decisions recorded

- D1 (settled by negotiation with agy, 2026-10-15; owner confirms): the stop bound after the log dies is NOT relaxed. The main thread polls `wal_fault()` on every `submit` (3.4), so a log fault stops the file within one 256 KiB buffer, tighter than the 9f-1 behaviour (about 1 MiB). Considered and rejected: D1-a (worker alone detects; up to about 2 MiB lag by agy's count, and 1 MiB by mine), D1-c (a rendezvous per chunk), D1-d (detect only at `complete`). Cost of the poll: an uncontended `Mutex<Failure>` lock plus an `Option` clone, measured about 10 ns per call (a micro-benchmark outside the repository: 26,214 calls = 0.27 ms for a 6.4 GiB copy); contention with the writer thread was not measured, and the writer holds the lock only for in-memory map operations (`wal/writer.rs`). agy's first answer objected to this poll on mutex cost; after reading the code itself it withdrew the objection and picked D1-b.
- D2 (my call): `POOL = 4`; tunable by measurement, floor 2.
- D3 (my call): one code path for both durabilities.
- D4 (my call): the worker never beats; the main thread owns the heartbeat.
- D6 (from panel rounds 1-2; confirmed by the D1/D6 negotiation with agy, which picked it independently and named main-thread proxying (D6-c) as the strongest alternative): a plain `'static` worker thread with a `WalLink`, not a scoped thread (3.7). Its weakest point, named by agy: a leaked `FileSink` would leave a sender clone alive and keep the log channel connected; tests 4b and 4c and the join in `abort`/`complete`/`Drop` bound that. Fallback if the extra sender clone is judged unacceptable: D6-c. Cost if wrong: one extra accessor on `WalSender`; the scoped alternative needs a lifetime on `FileSink` and a hang-proof scope shape.
- D5: no `rayon`; blake3 stays single-threaded (`blake3 = "1.8"` without the `rayon` feature, `Cargo.toml:64`).

## 10. Self-audit (exhaustiveness)

- Contracts and shapes: `submit` signature, `Worker` fields, message order on the WAL channel, buffer ownership (by value, recycled) stated; the digest and log formats are unchanged (G2, tests 1 and 9).
- Placeholders: none left as "TBD". Resolved at plan time, with where: the exact shape of the `WalLink` accessor on `WalSender` (3.3), the `FaultFs` write hook (section 7), the `available_parallelism` test seam (test 8). Each needs the code at the plan's commit.
- Edge/state combinations: failure table in section 5, including resume-with-proven-prefix, exact-multiple lengths, one CPU, spawn failure, lost lock, kill.
- Requirement mapping: G1 -> section 8; G2 -> tests 1, 9; G3 -> I1-I7 -> tests 2-7; each anti-goal -> an invariant (I1, I3, I5, I7) and a test.
- Not covered by design, deliberately: resume-time hashing cost (`decide`), parallel hashing within a file, Strict-specific tuning.

## Stand-downs (adversarial panel, 3 rounds, agy plus a solo floor; final round GREEN)

- ALTERNATIVE CONSIDERED, NOT CHOSEN (round 3, low): main-thread proxying, where the worker returns completed `Range` records to the main thread over the recycle channel and the main thread forwards `Msg::Range`. It would avoid a second `SyncSender` clone, but it puts a blocking send (with `beat()`) and the fault checks back on the copy thread and couples digest hand-back to buffer recycling. WalLink is bounded by the sink's join (3.3, 3.6), so the clone cannot outlive a file. Cost if wrong: the `WalLink` accessor on `WalSender`; the plan can switch without touching the log format.
- DISCARDED-BELOW-FLOOR (round 2, low): `abort` ignoring `beat()` errors in its join loop, because `FileSink::abort` already ignores them for its `Discard` send (`checkpoint/sink.rs`, `let _ = self.send(Msg::Discard ..)`); the run is already failing and the join is bounded by the cancel flag within one `SEND_RETRY`.
- FOLDED (round 3, low): the `abort` doc comment update, already in 3.6.
- Panel record: round 1 (solo floor + agy): 4 findings, all verified and folded (scope hang on `break 'stream`, abort wait without `beat()`, 3.2/3.4 spawn contradiction, `SyncSender`-only clone). Round 2: 2 findings, both verified and folded by D6 (a `Scope` is needed to spawn; shape (a) hangs on panic). Round 3: GREEN. agy's verdicts were checked against the code at the cited lines (`copy.rs:778` and `848-852`, `wal/writer.rs:129-144`, `checkpoint/mod.rs:144-162`); one citation (`V16:6415-6416`, from the earlier fork consult) was wrong and not used.

## 11. Implementation notes (recorded after execution)

- The release profile is `panic = "abort"` (Cargo.toml:111, comment lines 103-106), so invariant I6 (a worker panic surfaces as a log-stop error) holds only in unwinding builds (tests and dev); in the shipped binary a worker panic aborts the process and crash recovery applies.
- A kill, or a faked crash, can lose the digests of up to POOL buffers (1 MiB) that were queued when `abort` cancelled the worker (spec 3.6); resume re-hashes them. Existing fixtures that fake a crash with a failing heartbeat and assert the exact log hash inline (`OneCpu` guard, ruling R11); the worker's log equality is pinned by `the_worker_log_equals_the_inline_log` (sink.rs), the copy.rs test comparing Inline and worker logs, and the flux-cli real-process tests.
- D1 outcome: the main thread polls `wal_fault()` at the top of every `submit`; a log fault stops the file within one buffer (tighter than the 9f-1 behaviour of up to about 1 MiB).
- Test mechanisms added: `FaultFs::fail_write_nth`, `FaultFs::fail_read_nth`, `WalLink`, the `checkpoint::test_seams` facade; the worker tests need more than one CPU.
