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

3.2 Worker. One scoped thread per large file (`std::thread::Builder::spawn_scoped`, named `flux-hash`), spawned right after `FileSink::begin` and after `record_proven`, so the main thread has already sent `FileBegin` and any proven `Range`s and the worker only ever appends later ranges (order on the WAL channel: `FileBegin`, proven `Range`s, worker `Range`s in offset order, then `Close`/`Discard` from the main thread). The worker owns the `ChunkHasher` and the pending-digests vector (today's `FileSink.hasher`, `digests`, `range_start`) and applies exactly today's rule: after each completed chunk push the digest; when `range_chunks` digests are pending send one `Range` ending at `(index + 1) * CHUNK_SIZE`; then check `sender.wal_fault()` (as `wrote` does today).

3.3 Sending from the worker. The `Heartbeat` is a `dyn Fn() + 'g` and is not shareable with another thread, so the worker never beats. Its `Range` sends use a worker-side loop: `try_send`; on `Full` sleep `SEND_RETRY` (10 ms) and retry; give up when the file's cancel flag is set, the channel is disconnected, or `wal_fault()` is `Some`. (If `WalSender` cannot be shared by reference with a scoped thread, the plan resolves it by a `clone` of the underlying `SyncSender`; that is a plan-level detail, but the worker must not own a second `WalSender`, whose `Drop` sets `abandoned` unless the sender was stopped, `wal/writer.rs:138-144`, which would make the log read as abandoned.)

3.4 Main thread. `FileSink` keeps ONE public API and two internal modes chosen at `begin` (my call, from the builder check): `Inline` (today's behaviour) and `Worker`. `FileSink::wrote(&[u8], beat)` stays as today's inline hashing step and is used for exactly two things: the `carry` bytes (below) and `Inline` mode. The new entry point is `FileSink::submit(&mut self, buf: Vec<u8>, n: usize, beat) -> Result<Vec<u8>, SinkError>`: `buf` is a filled pool buffer of which the first `n` bytes are valid (the last read of a file is short), and the return value is a buffer to read the next chunk into.
- `Inline` mode: `submit` is `wrote(&buf[..n])` and returns the same `buf`. So `stage_file` has ONE loop shape for a file with a sink (it exchanges an owned `buf` through `submit`); files with no sink keep today's single-buffer loop unchanged. The inline path is production code (the fallback of 3.8) and the oracle for the worker.
- `Worker` mode: the first `submit` spawns the worker (so `wrote` for the carry has already run on the main thread and the `ChunkHasher`, pending digests and `range_start` move into the worker with their state). `wrote` after the first `submit` is a `debug_assert` failure.
- Every `submit` begins by checking the worker's fault: a shared `AtomicBool` that the worker sets when it records a fault (WAL died, send gave up), and the result channel's disconnection (worker panicked or exited). Either returns the stored `SinkError` at once. This makes the stop bound hold even while free buffers are queued (builder-check Q5).
- If a free buffer is available it is returned. If the pool is exhausted the main thread waits for the worker with `recv_timeout(SEND_RETRY)` and calls `beat()` on every timeout (the loop `FileSink::complete` already uses for the `Close` ack). This keeps the heartbeat alive while the worker is blocked on a full WAL channel (I3).
- The file stops at the first `submit` after the fault flag is set; the worker detects a fault only after hashing the buffers queued before the faulting chunk, so the lag is at most `POOL` buffers rather than "at the first completed chunk". This is a deviation from 9f-1's wording and is ledgered as D1 (needs owner).
- `carry` (`decide`'s consumed prefix, `copy.rs` the `!carry.is_empty()` block) is written and then hashed INLINE with `wrote` on the main thread, before the worker exists. Its size is bounded by `decide`'s own read (`source_buf[..got]`, `decide.rs`), so it is a one-off, small cost, it never enters the pool, and I7 holds (builder-check Q4). Pool buffers are all exactly `copy_buffer_len` long; a recycled buffer is resized to full length before the next read.

3.5 `complete`. The main thread closes the submit channel; the worker drains every queued buffer, then returns `(hasher, pending digests, range_start)` as its join result. The main thread waits for it with `recv_timeout` + `beat()`, then runs today's `complete` tail unchanged: push the final short chunk's digest, `send_range`, `Close` with ack, wait for the ack with `beat()` (`FileSink::complete`, `sink.rs`).

3.6 `abort` and `Drop`. `abort` sets the cancel flag, closes the submit channel, joins the worker (the worker observes the flag within one `SEND_RETRY`, so the join is bounded without `beat()`; the wait is still a `recv_timeout` loop, never an unbounded `join`), and only then sends `Discard`. After `abort` returns, the worker is gone, so no `Range` can follow `Discard` (I5). `FileSink::abort`'s doc comment ("without waiting") is updated: it still flushes nothing, but it now waits for the worker. The existing rule "a `FileSink` must end through `complete` or `abort`" (the `Drop` debug-assert) is kept and extended: a dropped sink with a live worker joins it first.

3.7 Where the thread scope lives. A scoped thread must be borrowed inside `std::thread::scope`. The streaming section of `stage_file` is a labelled block (`'stream: { ... }` in `stage_file`, from the `let mut buf` allocation to the `complete` call, `copy.rs` about 777-850) with `break 'stream Err(..)` exits. The scope wraps that block's body so every exit path ends the worker before the scope closes; the plan resolves the exact shape against the code that exists then, and must keep one property: no `break` can leave the scope with the worker alive.

3.8 When the worker is not used. Inline hashing (today's code path, kept verbatim as the fallback) runs when: the file is below `min_len` (no sink at all); `std::thread::available_parallelism()` is 1; or `spawn_scoped` fails (the Strict barrier's `Builder::spawn_scoped` fallback, `copy.rs:1175-1192`, is the pattern). The fallback keeps one code path producing the identical log, so tests can run both and compare (G2). My call: the inline path is the oracle for the worker's output.

3.9 Durability modes. Normal and Strict take the same worker path (the Strict duplicate descriptor `dup` and the writer thread's data sync are untouched). Strict showed no measurable wall gain in the A/B, but a mode-dependent code path is a second path to test; one path is simpler and correct for both.

## 4. Invariants (each pinned by a test in section 7)

- I1. No `Range` covering `[a, b)` is in the log while the destination holds fewer than `b` bytes from this attempt's writes. By construction (3), and pinned by the failure-injection test.
- I2. Digest order and boundaries equal the inline implementation's (G2).
- I3. `beat()` is called at least once per copy-buffer period and at least once per `SEND_RETRY` while the main thread waits on the worker, the pool or the `Close` ack. A full WAL channel never starves the heartbeat.
- I4. After the log has failed, the file stops with `SinkError::Wal` within `POOL` buffers; a lost lock (`guard()` failing in the main loop, section 99) stops the main thread, and `abort` stops the worker before any further WAL send.
- I5. After `abort` or `complete` returns, no worker exists and nothing is sent to the log on the file's behalf.
- I6. A worker panic is a `SinkError::Wal("the WAL writer thread is gone")`-class stop (the file's partial is kept, `StreamFail::Keep`), never a hang and never a silent success: the submit and join channels disconnect, and the main thread maps a disconnected worker to the existing `wal_fault_or_gone()` text.
- I7. At most `POOL * copy_buffer_len` bytes per file are held by the pool; memory does not grow with file size or queue depth.

## 5. Failure cases (state combinations)

| Event | Behaviour |
|---|---|
| `write_all` fails on buffer k | The buffer is never submitted; chunks completed by earlier buffers may already be logged (their bytes were written); `StreamFail::Discard` as today; `abort` joins the worker. |
| Source read error | Same: `abort`, no further sends. |
| WAL append fails (log dies) | The writer thread discards; the worker's next send or `wal_fault()` check returns `Wal`; the worker reports it and exits; the main thread sees it at the next `submit` (within `POOL` buffers) and returns `StreamFail::Keep` (`sink_fail`, `copy.rs:510`). |
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
5. `a_full_channel_does_not_starve_the_heartbeat` (I3): channel cap 1, held writer, pool exhausted; assert `beat` is called repeatedly while the worker is held blocked (a counter that must reach at least 3 within a generous deadline, polled, never an exact time window), and that a `beat` returning an error ends the wait with `SinkError::Heartbeat`. Mutant: drop the `beat()` in the pool wait turns it red.
6. `a_dead_log_stops_the_file_within_pool_buffers` (I4): kill the log; count buffers accepted after the fault; assert `<= POOL`; error is `SinkError::Wal`.
7. `a_panicking_worker_is_a_wal_stop_not_a_hang` (I6): a test-only panic hook in the worker; assert `SinkError::Wal` with the "gone" text, within a deadline.
8. `the_inline_fallback_is_used_for_one_cpu_and_a_failed_spawn`: a test seam for `available_parallelism` and for spawn failure; assert the log equals the worker's.
9. End-to-end: the existing real-process tests `a_killed_large_file_resumes_from_its_validated_prefix` and its siblings (`crates/flux-cli/tests/run.rs:604` onward) must pass UNCHANGED against the worker build; they already assert byte equality and the reuse report. No new baseline constant is introduced.
The plan names the `FaultFs` hooks each test needs against the code that exists then (`on_nth` hooks on `sync_all` exist; a write hook may need adding: a plan task, not a spec decision).

## 8. Acceptance (owner-gated measurement, top level, idle machine, no agents, two runs, quote the range, state uncontrolled load)

- 100 files of 64 MiB, Normal, release build, cache dropped, 4 runs x 2 passes alternating binary order, the worker build against main `14dde8d`: G1 holds if the worker build's wall median is at least 1.5 s below main's (the measured hash gap is about 3.1 s; "half" is my call) with ranges stated. If it is not, the cut is reverted or closed as not worth its complexity, and the TODO entry records the figures.
- Strict on the same case: not slower than main beyond the measured spread (ranges overlap or better).
- `perf stat` user CPU stays about equal (the work moved, it was not removed): record it.
- The `mixed` case and the single-file case: no regression beyond spread.
- Gate: `just check` and `cargo clippy --workspace --all-targets --release -- -D warnings`; CI on every job including Windows (the cancel-flag/join path must be identical there; `spawn_scoped` and `available_parallelism` are portable).

## 9. Deviations and decisions recorded

- D1 (my call, NEEDS OWNER RULING before the plan): "stop at the first completed chunk after the log died" (9f-1 design, sink `wrote` comment) becomes "at the first `submit` after the worker records the fault, at most `POOL` buffers later" (at most 1 MiB of extra copying after the fault). Rationale: the worker detects the fault, the main thread learns it one hand-off later. Cost if wrong: up to 1 MiB of extra writes to a file whose resume state is already untrusted and which is kept, not discarded.
- D2 (my call): `POOL = 4`; tunable by measurement, floor 2.
- D3 (my call): one code path for both durabilities.
- D4 (my call): the worker never beats; the main thread owns the heartbeat.
- D5: no `rayon`; blake3 stays single-threaded (`blake3 = "1.8"` without the `rayon` feature, `Cargo.toml:64`).

## 10. Self-audit (exhaustiveness)

- Contracts and shapes: `submit` signature, `Worker` fields, message order on the WAL channel, buffer ownership (by value, recycled) stated; the digest and log formats are unchanged (G2, tests 1 and 9).
- Placeholders: none left as "TBD". Resolved at plan time, with where: the exact `std::thread::scope` placement in `stage_file` (3.7), sharing `WalSender` by reference versus a cloned `SyncSender` (3.3), the `FaultFs` write hook (section 7), the `available_parallelism` test seam (test 8). Each needs the code at the plan's commit.
- Edge/state combinations: failure table in section 5, including resume-with-proven-prefix, exact-multiple lengths, one CPU, spawn failure, lost lock, kill.
- Requirement mapping: G1 -> section 8; G2 -> tests 1, 9; G3 -> I1-I7 -> tests 2-7; each anti-goal -> an invariant (I1, I3, I5, I7) and a test.
- Not covered by design, deliberately: resume-time hashing cost (`decide`), parallel hashing within a file, Strict-specific tuning.
