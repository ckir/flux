# Cut 9h: checkpoint hashing worker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the per-chunk BLAKE3 hashing of checkpointed large files (>= `CHECKPOINT_MIN_LEN`) off the copy thread onto one plain worker thread per file, with a byte-identical log, a tighter stop bound after a log failure, and no new hang or leak.

**Architecture:** `FileSink` gains an internal mode (`Inline` today's behaviour, or `Worker`) and a new `submit(buf, n, beat)` that hands a written buffer to a `flux-hash` thread and returns a recycled buffer; the worker owns the `ChunkHasher` and sends `Range` records to the WAL thread through a cloneable `WalLink`. The main thread polls `wal_fault()` on every `submit`. `stage_file`'s copy loop exchanges an owned `buf` through `submit` for files with a sink.

**Tech Stack:** Rust (workspace `flux-core`), `std::thread`, `std::sync::mpsc`, `blake3` (already a dependency), `FaultFs` fakes for tests, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-10-15-cut-9h-hashing-worker-design.md` (approved by the owner 2026-10-15; D1 = main-thread `wal_fault()` poll per `submit`, D6 = plain `'static` thread with a `WalLink`). Every code citation below was read on branch `spec/cut-9h-hashing-worker` at `feab421`; the code is identical to main `14dde8d`.

## Global Constraints

- Gate: `just check` (fmt-check, clippy, typos, `cargo nextest run --workspace --no-tests=pass`) and `cargo clippy --workspace --all-targets --release -- -D warnings`; run `cargo fmt` first. Last known baseline: 1458 passed, 5 skipped.
- No change to the digest (BLAKE3 over exactly one `CHUNK_SIZE` = 1 MiB chunk of source bytes, `wal/mod.rs:4`), the record format, `wal/` message set, `decide`/`decide_full`, `run/`, `tree.rs` or the CLI.
- No `rayon`; no `std::thread::scope` (spec 3.7). The worker is `std::thread::Builder::new().name("flux-hash")`.
- `POOL = 4` buffers per file, each `copy_buffer_len(src_len)` bytes (<= `COPY_BUF_MAX` = 256 KiB, `copy.rs:177`).
- A `Range` is only ever created after the `write_all` of its bytes returned `Ok` (spec I1).
- Commit trailer: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. No `git checkout/switch/stash/reset` in any dispatch. Never commit `.clavity/` or `.superpowers/`. Do not push or open a PR without the owner's say-so (`just pr` arms auto-merge). Read CI after every push.
- Timing measurements: top level only, owner-approved, machine idle, no agents running, two runs, quote the range, state uncontrolled load. Never delegated.

## File Structure

- Modify `crates/flux-core/src/fault_fs.rs`: one new fault (`fail_write_nth`) so a test can fail the Nth buffer write.
- Modify `crates/flux-core/src/wal/writer.rs`: `WalLink<W>` and `WalSender::link`.
- Create `crates/flux-core/src/checkpoint/worker.rs`: the hashing thread (`Worker`), its channels, its test seams.
- Modify `crates/flux-core/src/checkpoint/mod.rs`: `mod worker;` (beside `mod sink;`, line 7).
- Modify `crates/flux-core/src/checkpoint/sink.rs`: modes, `submit`, `complete`/`abort`/`Drop` changes, tests. Task 4 also removes the temporary `#[allow(dead_code)]` on `mod worker;` and the `#[cfg_attr(not(test), allow(dead_code))]` on `WalLink`/`link` (ruling R5).
- Modify `crates/flux-core/src/copy.rs`: the copy loop (lines 777-852) and tests. Task 5 also: deletes the temporary `#[cfg_attr(not(test), allow(dead_code))]` on `FileSink::submit` (`sink.rs`, Task 4), adds the `#[cfg(test)]` `submitted_for_tests` counter to `checkpoint/worker.rs` (R9) and, only if needed for Task 5 test 2, a mid-stream read fault `FaultFs::fail_read_nth(path_contains, nth, kind)` mirroring `fail_write_nth` in `fault_fs.rs`.
- Modify `docs/superpowers/specs/2026-10-12-cut-9f1-chunk-checkpoints-design.md` (deviations note), `TODO.md` (the hashing-cost entry), `README.md` only if it names the stop bound (check by grep).

## Review Focus

1. A file whose whole remaining length is `carry` (no read happens), or whose first read hits EOF: zero `submit`s, so no worker; `complete` must still write the final digest and `Close`. Pinned in Task 4 (`complete_without_a_submit_runs_inline`).
2. A fault that lands while the pool wait loop is running, or between the last `submit` and `complete`: the file must end with `SinkError::Wal`, not hang. Pinned in Task 4 (`a_fault_during_the_pool_wait_ends_the_file`) and Task 3.
3. The WAL channel stays full for a long time: the main thread must keep calling `beat()` while it waits for a free buffer or for the worker at `complete`/`abort`. Pinned in Task 4 (`a_full_channel_does_not_starve_the_heartbeat`).
4. A read, write or heartbeat error, or a panic, with the worker running and queued buffers: no leaked or blocked `flux-hash` thread, no `Range` after `Discard`. Pinned in Tasks 4 and 5 (`abort_joins_the_worker_and_nothing_follows_discard`, `a_failed_read_write_or_beat_does_not_hang`, `a_panic_in_the_copy_thread_does_not_leave_a_worker_blocked`).
5. A resumed file (proven prefix via `record_proven`, a `carry`): chunk numbering and `Range` boundaries must equal the inline run. Pinned in Task 4 (`the_worker_log_equals_the_inline_log`, with a proven prefix and a carry).

## Test mechanisms (verified against existing tests)

- **Holding the WAL writer so its channel stays full** (Task 3 test 4; Task 4 tests 3, 5, 8): the pattern of `a_full_channel_blocks_the_sender_and_beats` (`wal/writer.rs:1210-1231`): `Durability::Strict` with a duplicate handle (`fs.create_new(Path::new("/ws/p"))` then `.duplicate()` passed to `FileSink::begin`), `fs.delay("sync_all", Duration::from_millis(300))`, `channel_cap = 1` in the `WalConfig` of the policy, then `run.sender.send(Msg::Flush, beat)` (the `Flush` variant is available under `cfg(test)`). The writer is then stuck in the delayed `sync_all`, so further sends return `Full`. The "release" is the delay expiring; tests assert counts (for example `beat` calls >= 3) inside that window, never exact timing.
- **Worker-only fault and panic** (Task 4 tests 7 and 10): a `#[cfg(test)] thread_local!` `INJECT: Cell<Inject>` in `worker.rs` (`Inject` is `Copy`), read by `FileSink` when it builds `WorkerArgs`; production code builds `WorkerArgs` without the field (`#[cfg(test)]` on both the field and its initializer).
- **Mode selection** (`Force` seam, Task 3): same `thread_local!` pattern.

## Execution notes (model gating)

Tasks 1, 2, 6: Sonnet implementer (mechanical, well specified). Tasks 3, 4, 5: concurrency next to the durability log, so error-cost raises the tier: Opus implementer and Opus reviewer. Every dispatch carries: STATE-VERIFICATION Step 0 (`STATE_MISMATCH:`), SHAPE-DIVERGENCE STOP, NAME THE ORACLE, no elided enumerations, exact commands with the expected pass line, the ANOMALIES clause, and a FILES clause (pre-list every file the compiler will name for a signature change). The tests in each task are already written in the plan: "implement until they pass". Timing (Task 6 Step 4) is run by the controller at the top level.

---

### Task 1: `FaultFs::fail_write_nth`

**Files:** Modify `crates/flux-core/src/fault_fs.rs`: `struct Inner` (the `write_fault` field is at line 83), `impl Write for FakeHandle::write` (line 688), the pub fault methods near `fail_write` (line 1239). Test: the `mod tests` of `fault_fs.rs`.

**Why:** Test 2 of the spec needs the write of buffer k to fail (not "the next write", which `fail_write` provides and which is taken by the handle at creation, lines 1383-1393). `on_nth` hooks cannot reach an open handle's `write_fault`.

**Interfaces:**
- Produces: `pub fn FaultFs::fail_write_nth(&self, path_contains: &str, nth: u32, kind: std::io::ErrorKind)`: the `nth` (1-based) call to `write` on a WRITER handle (a handle with a sink, as `sync_all` at line 723 uses) whose path contains `path_contains` returns `Err(io::Error::new(kind, "injected write failure"))`, once. Only writes to matching handles are counted: the WAL writer thread also writes through `FakeHandle::write` (to `/ws/wal/...`), so an unfiltered count would be nondeterministic (ruling R1, ledger).

- [ ] **Step 1: Write the failing test** `fail_write_nth_fails_exactly_the_nth_write_once` in `fault_fs.rs` tests: create a writer with `FaultFs::create_new`, call `fail_write_nth("p", 3, ErrorKind::StorageFull)` on a handle created at `/ws/p`, and also write once to a second handle at `/ws/other` between the writes (it must not be counted), write three 4-byte slices through `write_all`; assert writes 1 and 2 succeed, the third returns `Err` whose `kind()` is `StorageFull`, a fourth write succeeds, and `fs.read_file(path)` holds 12 bytes minus the failed one (8 bytes).
- [ ] **Step 2: Run:** `cargo nextest run -p flux-core fail_write_nth` Expected: FAIL (`fail_write_nth` not found).
- [ ] **Step 3: Implement** `Inner.write_nth: Option<(String, u32, std::io::ErrorKind)>` and `Inner.writes: u32` (the count of matching writes) (default `None`/`0`), the method above, and in `FakeHandle::write` (before the existing `write_fault` take, line 689): `if let Some(sink) = &self.sink { let mut g = sink.lock().unwrap(); if let Some((pat, n, kind)) = g.write_nth.clone() && self.path.to_string_lossy().contains(&pat) { g.writes += 1; if g.writes == n { g.write_nth = None; return Err(std::io::Error::new(kind, "injected write failure")); } } }`. The compiler names every `Inner { .. }` literal; add the two fields there (search repo-wide, exclude the definition).
- [ ] **Step 4: Run:** `cargo nextest run -p flux-core fail_write_nth` Expected: PASS. Then `cargo nextest run -p flux-core` Expected: all pass.
- [ ] **Step 5: Commit** `test(fault_fs): fail_write_nth fails the Nth writer write (cut 9h, task 1)`.

---

### Task 2: `WalLink`

**Files:** Modify `crates/flux-core/src/wal/writer.rs`: add `WalLink<W>` after `WalSender` (struct at lines 129-136, `impl` at 146-181) ; tests in its `mod tests` (the existing sender tests use `s.send(Msg::Stop, beat)`, line ~711).

**Interfaces:**
- Produces: `pub(crate) struct WalLink<W> { tx: SyncSender<Msg<W>>, failure: Arc<Mutex<Failure>> }`; `impl<W> WalSender<W> { pub(crate) fn link(&self) -> WalLink<W> }` (clones `tx` and the `Arc`); `impl<W> WalLink<W> { pub(crate) fn try_send(&self, msg: Msg<W>) -> Result<(), TrySendError<Msg<W>>>; pub(crate) fn fault(&self) -> Option<WalFault> }` where `fault` is the same read as `WalSender::wal_fault` (line 178-180). `WalLink` has NO `Drop`, never touches `stopped` or `abandoned`.

- [ ] **Step 1: Write the failing tests** in `writer.rs` tests: (a) `a_link_delivers_a_range_that_reaches_the_log`: open a writer as the existing tests do, `let l = s.link();` send `FileBegin` through `s.send` then a `Range` through `l.try_send`, `Stop` through `s.send`, join, assert the segment holds both records (decode with `decode_next`, as `sink.rs` tests do); (b) `a_link_reads_the_same_fault_as_the_sender`: kill the log (`fs.fail("create_new", Code::DiskFull)`, drain) and assert `l.fault() == s.wal_fault()` and both `Some`; (c) `dropping_a_link_does_not_abandon_the_run`: create a link, drop it, `s.send(Msg::Stop)`, join, assert the segment flushed what was pending (not abandoned); (d) `a_link_is_send_and_sync_for_a_send_writer` : a compile-time assertion `fn assert_send<T: Send>() {}` over `WalLink<FakeHandle-writer type used by the tests>`.
- [ ] **Step 2: Run:** `cargo nextest run -p flux-core a_link dropping_a_link` Expected: FAIL (no `link`).
- [ ] **Step 3: Implement** the type and methods above; `use std::sync::mpsc::TrySendError` is already imported at line 18. `WalSender::link` and the `WalLink` methods are unused in non-test builds until Task 4: annotate them `#[cfg_attr(not(test), allow(dead_code))]` (the same idiom `Msg::Flush` uses, `wal/writer.rs`), and Task 4 removes the annotations (ruling R5; otherwise `clippy -D warnings` fails at this task's gate).
- [ ] **Step 4: Run** the same command. Expected: PASS; then `cargo nextest run -p flux-core wal::` all pass.
- [ ] **Step 5: Commit** `feat(wal): WalLink, a cloneable send/fault view for a worker thread (cut 9h, task 2)`.

---

### Task 3: the hashing `Worker`

**Files:** Create `crates/flux-core/src/checkpoint/worker.rs`; modify `crates/flux-core/src/checkpoint/mod.rs` (add `mod worker;` after `mod sink;`, line 7). Tests in `worker.rs`.

**Interfaces:**
- Consumes: `WalLink<W>` (Task 2), `ChunkHasher` (`wal/digest.rs`: `new`, `from_chunk(index)`, `update(&mut self, &[u8]) -> Vec<(u64, Digest)>`, `finish(self) -> Option<(u64, Digest)>`, `bytes_seen`), `Range` (`wal/record.rs:38-44`), `SEND_RETRY`, `WalFault`, `Msg`.
- Produces (all `pub(super)`):
  - `const POOL: usize = 4;`
  - `struct WorkerArgs<W> { link: WalLink<W>, file_id: Vec<u8>, hasher: ChunkHasher, digests: Vec<Digest>, range_start: u64, range_chunks: usize, extra_buffers: Vec<Vec<u8>>, #[cfg(test)] inject: Inject }` and `#[cfg(test)] #[derive(Default)] struct Inject { fail_send_after_ranges: Option<usize>, panic_on_job: Option<usize> }`.
  - `struct WorkerEnd { hasher: ChunkHasher, digests: Vec<Digest>, range_start: u64 }`.
  - `struct Worker` with: `fn spawn<W: Send + 'static>(args: WorkerArgs<W>) -> std::io::Result<Worker>`; `fn fault(&self) -> Option<WalFault>` (reads the shared fault slot; cheap atomic first); `fn submit(&self, buf: Vec<u8>, n: usize) -> Result<(), Gone>`; `fn recv_free(&self, timeout: Duration) -> Result<Vec<u8>, RecvTimeoutError>`; `fn finish(self, beat: &Heartbeat<'_>) -> Result<WorkerEnd, WorkerFail>` (close the submit channel, wait for the result with `recv_timeout(SEND_RETRY)` calling `beat()` on every timeout, then reap the thread); `fn cancel_and_join(&mut self, beat: Option<&Heartbeat<'_>>)` (set cancel, close submit, wait the same way, ignore `beat` errors, reap); `enum WorkerFail { Heartbeat(FsError), Fault(WalFault), Gone }`; `struct Gone;`.
  - Behaviour (thread body, spec 3.2/3.3): loop `recv` jobs; `hasher.update(&buf[..n])`; for each completed chunk push the digest; when `digests.len() >= range_chunks` send one `Range { file_id, attempt_id: 1, range_start, range_end: (index + 1) * CHUNK_SIZE, digests: take }` with `link.try_send`, retrying every `SEND_RETRY` on `Full` until success, `Disconnected`, the cancel flag, or `link.fault()` is `Some`; after each chunk check `link.fault()`; on any fault set the shared fault slot and flag and exit; return the buffer through the free channel after each job; on channel close send `WorkerEnd` (or the fault) as the final result. The worker never calls `beat`. `ATTEMPT_ID` is `1`: import it as a `pub(super) const` from `sink.rs` (it is private at line 15 today; make it `pub(super)`).
  - Test observability (all `#[cfg(test)]`): `Worker::is_finished(&self) -> bool`; `Worker::spawned_for_tests() -> usize` (a `thread_local!` counter incremented on each successful `spawn` on the calling thread, so a test can assert that NO worker was spawned or that one was); `Worker::live_for_tests() -> usize` (a process-wide `AtomicUsize` incremented at spawn and decremented by a drop guard at the end of the thread body; the tests that assert `== 0` poll it against a deadline; the gate is `cargo nextest`, one process per test, so parallel tests do not disturb it).
  - Test seams: `thread_local!` in `worker.rs` under `#[cfg(test)]`: `pub(super) static FORCE: Cell<Force>` with `enum Force { Auto, OneCpu, SpawnFails }`; `Worker::available() -> bool` returns false under `Force::OneCpu` and when `std::thread::available_parallelism()` is 1; `Worker::spawn` returns `Err` under `Force::SpawnFails`.

- [ ] **Step 1: Write the failing tests** (in `worker.rs`, with the `FaultFs` + `Checkpoints` harness copied from `sink.rs` tests, lines 246-322: `ws`, `policy`, `open_run`; reuse by making those test helpers `pub(super)` under `#[cfg(test)]` in `sink.rs`, or duplicate the four small ones):
  1. `the_worker_digests_equal_the_inline_hasher`: feed 5 MiB + 7 bytes through `submit` in 256 KiB pieces (plus the short tail), `finish`, assert `WorkerEnd` plus the `Range`s logged equal `ChunkHasher::new()` run inline over the same bytes (digests compared one by one, and each against `blake3::hash` of its 1 MiB source slice).
  2. `buffers_are_recycled_and_the_pool_is_bounded`: submit 20 pieces; assert at most `POOL` distinct buffer allocations are ever seen (compare `as_ptr()` of returned buffers) and every returned buffer still has its full length.
  3. `a_log_fault_sets_the_fault_and_the_worker_exits`: kill the log, submit a chunk; `recv_free`/`fault()` eventually report `Some(Wal)` within a polled deadline; `finish` returns `WorkerFail::Fault`.
  4. `cancel_stops_a_worker_blocked_on_a_full_channel`: channel cap 1 (`policy` `channel_cap`), the writer held so sends return `Full` (the hold mechanism in "Test mechanisms" above); call `cancel_and_join` and assert it returns within a deadline and the thread is gone (`Worker` exposes `is_finished()` for tests).
  5. `a_panicking_worker_reports_gone`: `Inject { panic_on_job: Some(1) }`; `finish` returns `WorkerFail::Gone`; no panic propagates to the caller.
  6. `dropping_a_worker_joins_its_thread`: spawn a `Worker`, submit one job, then simply drop it (no `finish`, no `cancel_and_join`); assert `live_for_tests()` returns to its starting value within a polled deadline (ruling R3).
- [ ] **Step 2: Run:** `cargo nextest run -p flux-core checkpoint::worker` Expected: FAIL (module does not exist / does not compile).
- [ ] **Step 3: Implement** `worker.rs` per the Interfaces block. `Drop for Worker`: if not already joined, `cancel_and_join(None)`. Declare it `#[allow(dead_code)] mod worker;` in `checkpoint/mod.rs` and say so in the commit message; Task 4 removes the `allow` (ruling R5). The test harness helpers it needs (`ws`, `policy`, `open_run`) are DUPLICATED in `worker.rs`'s own `mod tests`, not shared with `sink.rs` (ruling R2).
- [ ] **Step 4: Run:** the same command. Expected: PASS (6 tests); then `cargo nextest run -p flux-core` all pass; `cargo clippy -p flux-core --all-targets -- -D warnings` clean.
- [ ] **Step 5: Mutants** (non-vacuity; each must turn the NAMED test red, restore after): (a) in the worker, hash `&buf[..n + 1]` (wrong slice) -> test 1; (b) skip the `link.fault()` check -> test 3; (c) never set the cancel flag in `cancel_and_join` -> test 4; (d) in `finish`, skip reaping so a panic propagates -> test 5. Use `cargo nextest run --no-fail-fast` and read which test failed.
- [ ] **Step 6: Commit** `feat(checkpoint): the hashing worker thread (cut 9h, task 3)`.

---

### Task 4: `FileSink` modes, `submit`, and the lifecycle

**Files:** Modify `crates/flux-core/src/checkpoint/sink.rs`: the struct (lines 27-37), `Drop` (39-47), `begin` (68-105; the struct literal at 93-101), `wrote` (176-188), `complete` (192-218), `abort` (222-228), tests (from line 232). Make `ATTEMPT_ID` `pub(super)` (line 15).

**Interfaces:**
- Consumes: Task 3's `Worker`, `WorkerArgs`, `WorkerEnd`, `WorkerFail`, `POOL`, `Worker::available()`; `WalSender::link` (Task 2).
- Produces: `FileSink::submit(&mut self, buf: Vec<u8>, n: usize, beat: &Heartbeat<'_>) -> Result<Vec<u8>, SinkError>`; internal `enum Hashing<W> { Inline, Pending, Worker(Worker) }` field `hashing` on `FileSink`; `begin`'s signature is unchanged (`Pending` when `Worker::available()`, else `Inline`). `wrote` keeps its signature and is legal only before the first `submit` (a `debug_assert!(!matches!(self.hashing, Hashing::Worker(_)))`).
- Behaviour:
  - `submit` in `Inline`: `self.wrote(&buf[..n], beat)?; Ok(buf)`.
  - `submit` in `Pending`: build `WorkerArgs` by moving `self.hasher` (replace with `ChunkHasher::new()`), `self.digests` (take), `self.range_start`, `file_id.clone()`, `range_chunks = self.run.policy.range_chunks.max(1)`, `extra_buffers` = `POOL - 1` fresh `vec![0u8; buf.len()]`, `link = self.run.sender.link()`; on `Worker::spawn` error set `Inline` and behave as `Inline`; on success set `Worker(w)` and continue as `Worker`.
  - `submit` in `Worker`, in this order: (1) `if let Some(f) = self.run.sender.wal_fault() { return Err(SinkError::Wal(f)) }`; (2) `if let Some(f) = worker.fault() { return Err(SinkError::Wal(f)) }`; (3) `worker.submit(buf, n)` (`Gone` -> `SinkError::Wal(self.worker_gone())`, where `worker_gone` reports the log's fault first, then the worker's recorded fault, else "the checkpoint hashing worker stopped"); (4) take a free buffer: a loop of `worker.recv_free(SEND_RETRY)`; on `Timeout` call `beat().map_err(SinkError::Heartbeat)?` and repeat checks (1) and (2); on `Disconnected` return `SinkError::Wal(self.wal_fault_or_gone())`.
  - `complete`: if `Worker`, replace `hashing` with `Inline`, call `worker.finish(beat)`; `Ok(end)` -> put `end.hasher/digests/range_start` back into `self` and continue with today's tail unchanged; `Err(Fault(f))` -> `Err(SinkError::Wal(f))`, `Err(Heartbeat(e))` -> `Err(SinkError::Heartbeat(e))`, `Err(Gone)` -> `Err(SinkError::Wal(self.wal_fault_or_gone()))`; set `self.ended = true` first as today. `Pending`/`Inline`: today's tail.
  - `abort`: if `Worker`, `cancel_and_join(Some(beat))` BEFORE the `Discard` send; update the doc comment (it no longer says "without waiting": it flushes nothing but waits for the worker).
  - `Drop`: if `Worker`, `cancel_and_join(None)` first, then the existing debug-assert.
  - The worker-mode stop bounds: a log fault (via check 1) stops the file within one buffer; a worker-only fault (check 2, `Disconnected`) within `POOL` buffers.

- [ ] **Step 1: Write the failing tests** (sink tests module; helpers `ws`, `policy`, `open_run`, `fc`, `meta`, `data`, `records`, `ranges`, `drain`, `chunk_digests`, `range_shape` already exist, lines 246-322 and 629-636). Each test runs through `submit` with 256 KiB buffers unless stated; `Force` seams from Task 3 select the mode:
  1. `the_worker_log_equals_the_inline_log`: for sizes `[MIB-1, MIB, 3*MIB+MIB/2, 8*MIB, 8*MIB+5]` and for (no prefix, a proven prefix of 2 MiB through `record_proven`, a 300 KiB `carry` fed through `wrote` first), run once with `Force::OneCpu` (Inline) and once Worker; assert `ranges(&fs)` equal (same boundaries, same digests, same order) and each digest equals `chunk_digests(&src)[i]` (the independent `blake3` oracle; `chunk_digests` is at line 629).
  2. `complete_without_a_submit_runs_inline`: only `wrote(&carry)` then `complete`; assert the final digest and `Close` are logged and no thread named `flux-hash` was spawned (`worker` count seam: `Worker::spawned_for_tests()` thread-local counter).
  3. `complete_drains_every_queued_buffer_before_close`: hold the worker's sends (channel cap 1 + held writer), submit more buffers than consumed, release; assert the last `Range` is in the log before `run.finish` and ends at `source_len` (read the segment after the `Close` ack; `Close` is not a log record, so the order against it is observed by timing of the ack, not by `records(&fs)`).
  4. `abort_joins_the_worker_and_nothing_follows_discard`: abort with buffers queued; assert `records(&fs)` shows no `Range` after the point of `Discard` (`fs.read_file(WAL_PATH)` is `None` for an aborted-only file, as `abort_discards_pending_records_and_drops_the_duplicate` asserts at line ~624) and the worker thread is finished (`Worker::is_finished`).
  5. `a_full_channel_does_not_starve_the_heartbeat`: channel cap 1, writer held, pool exhausted; run `submit` on a helper thread; count `beat` calls through an `AtomicUsize`; poll (deadline 5 s) until the count is >= 3 while the helper is still blocked; then release the writer and assert completion; a `beat` that returns `Err` ends the wait with `SinkError::Heartbeat`.
  6. `a_dead_log_stops_the_file_within_one_buffer`: as `wrote_stops_at_the_first_chunk_after_the_wal_died` (line 445-473) but via `submit`: kill the log (`fs.fail("create_new", Code::DiskFull)`, `drain`), then count `submit` calls that succeed; assert `<= 1`; error is `SinkError::Wal` with `Code::DiskFull`. Keep the existing `wrote` test (it covers `Inline`).
  7. `a_worker_only_fault_stops_the_file_within_pool_buffers`: `Inject { fail_send_after_ranges: Some(0) }`: assert the error is `SinkError::Wal` and the number of accepted submits `<= POOL`.
  8. `a_fault_during_the_pool_wait_ends_the_file`: pool exhausted, kill the log while `submit` waits; assert it returns `SinkError::Wal` within a deadline (no hang).
  9. `the_inline_fallback_is_used_for_one_cpu_and_a_failed_spawn`: under `Force::OneCpu` and `Force::SpawnFails` assert the logged ranges equal the worker's and no `flux-hash` thread was spawned.
  10. `a_panicking_worker_is_a_wal_stop_not_a_hang`: `Inject { panic_on_job: Some(1) }`: the `submit`/`complete` returns `SinkError::Wal` whose message says the hashing worker stopped, within a deadline.
  11. (a) `an_ended_sink_leaves_no_worker`: after `abort` (and, in a second case, `complete`) drop the sink and assert `live_for_tests()` is back to its starting value; (b) `#[cfg(debug_assertions)] dropping_an_unended_sink_joins_its_worker_then_trips_the_assert`: `catch_unwind` around dropping a sink that has a live worker and was not ended; assert the unwind carries the existing "a FileSink must end through complete or abort" message AND that the worker has finished by then (the `Drop` stops the worker BEFORE the assert) (ruling R4).
- [ ] **Step 2: Run:** `cargo nextest run -p flux-core checkpoint::sink` Expected: FAIL (no `submit`).
- [ ] **Step 3: Implement** per the Interfaces block. Worker-fault messages: `wal_fault_or_gone()` text is kept for a dead WAL thread; for a vanished worker use `WalFault::new(Code::WalWriteFailed, io::ErrorKind::BrokenPipe, "the checkpoint hashing worker stopped")`.
- [ ] **Step 4: Run:** `cargo nextest run -p flux-core checkpoint::` Expected: PASS (existing 12 sink tests plus the new ones); `cargo nextest run -p flux-core` all pass; clippy clean.
- [ ] **Step 5: Mutants** (restore after each; the NAMED new test must be the one that reds): (a) `submit` skips check (1) (`wal_fault()` poll) -> test 6; (b) `abort` skips `cancel_and_join` -> test 4; (c) `complete` skips `finish` and sends `Close` first -> test 3; (d) pool wait omits `beat()` -> test 5; (e) `Pending` spawns before `wrote`'s carry is absorbed (swap order) -> test 1 with the carry case.
- [ ] **Step 6: Commit** `feat(checkpoint): FileSink hashes on a worker thread, with Inline fallback (cut 9h, task 4)`.

---

### Task 5: `stage_file` hands buffers through `submit`

**Files:** Modify `crates/flux-core/src/copy.rs`: the copy loop (lines 777-852: `buf` at 777, `sink` at 778, the `'stream` block at 779, the carry block at 808-817, the loop 819-840, `complete` 841-846, `abort` 848-852); tests in its `mod tests` (the `ckpt_run` / `stage_ckpt` helpers are at lines 1363-1385). Also: `crates/flux-core/src/checkpoint/sink.rs` (delete the temporary dead-code allowance on `FileSink::submit`), `crates/flux-core/src/checkpoint/worker.rs` (add the `#[cfg(test)]` `submitted_for_tests` counter, ruling R9), and, only if test 2 needs it, `crates/flux-core/src/fault_fs.rs` (`fail_read_nth(path_contains, nth, kind)` mirroring `fail_write_nth`, counting reads on READER handles whose path contains the substring).

**Interfaces:**
- Consumes: `FileSink::submit` (Task 4), `FaultFs::fail_write_nth` (Task 1).
- Produces: no public change. In the loop, for a file with a sink: `buf = s.submit(std::mem::take(&mut buf), n, beat)` after `write_all` succeeded and `bytes_copied += n`; an `Err(e)` breaks `'stream` with `sink_fail(e, name)` exactly as today (line 832-835). The carry block (808-817) keeps `s.wrote(&carry, beat)`. A file with no sink keeps today's loop (no take/replace).
- A worker-mode returned buffer has the same length as the original (the pool never truncates; `n` travels separately), so `Read::read(&mut reader, &mut buf)` (line 820) keeps reading a full buffer.

- [ ] **Step 1: Write the failing tests** in `copy.rs` tests using `ckpt_run`/`stage_ckpt` with `CheckpointPolicy { min_len: 1 << 20, range_chunks: 4, .. }` and a 6 MiB source:
  1. `no_job_for_a_buffer_whose_write_failed` (I1): `fs.fail_write_nth("flux-partial", k, ErrorKind::StorageFull)` (the staged partial is named `<name>.flux-partial.<operation-id>`, `copy.rs:251`; WAL writes do not match) with `k = 16` on a 6 MiB source and 256 KiB buffers (write 16 is the buffer that would complete chunk 3); run `stage_ckpt`; assert it returns the discard error and that exactly `k - 1 = 15` jobs were handed to the worker (`Worker::submitted_for_tests()`, a `#[cfg(test)]` thread-local counter incremented in `Worker::submit`, added in this task: ruling R9). Deterministic: the log-based observation of a wrongly ordered `Range` is racy because `abort` sends `Discard`, which drops unflushed records, so the log alone could not turn red. Also assert `wal_bodies` holds no `Range` reaching past 3 MiB (secondary). Mutant: call `s.submit` BEFORE `writer.write_all` -> `submitted_for_tests()` is `k`, the test reds.
  2. `a_failed_read_write_or_beat_does_not_hang` (I5): three cases (a source read error via a failing read fake or a truncated source `fs.fail("open_read"...)` mid-copy if available, otherwise a `Read` wrapper hook; `fail_write_nth`; a `beat` closure that fails on its 5th call): each run on a helper thread with a 10 s join deadline; assert the matching error code and that no `flux-hash` thread is alive afterwards (`Worker::live_for_tests()` thread-local counter from Task 3).
  3. `a_panic_in_the_copy_thread_does_not_leave_a_worker_blocked`: a `beat` closure that panics on its 5th call, wrapped in `std::panic::catch_unwind(AssertUnwindSafe(..))`; after the unwind assert `Worker::live_for_tests() == 0` within a polled deadline.
  4. `a_checkpointed_copy_logs_the_same_ranges_with_and_without_the_worker`: `Force::OneCpu` versus `Force::Auto` over the same fixture through `stage_ckpt`; equal `wal_bodies`.
  Existing copy tests (`log_free_*`, the checkpoint stage tests at lines 3396, 3649, 3911) must still pass unchanged.
- [ ] **Step 2: Run:** `cargo nextest run -p flux-core copy::tests::no_range copy::tests::a_failed_read copy::tests::a_panic_in copy::tests::a_checkpointed_copy` Expected: FAIL (the loop still calls `wrote`, so tests 2-3 do not exercise a worker and 1/4 may pass vacuously: confirm each test fails or is vacuous BEFORE the change by checking `Worker::spawned_for_tests() > 0` is asserted in each, which makes them red now).
- [ ] **Step 3: Implement** the loop change per the Interfaces block (`buf` stays `let mut buf = vec![0u8; copy_buffer_len(src_meta.len)]`, line 777).
- [ ] **Step 4: Run:** the command above Expected: PASS; `cargo nextest run --workspace --no-tests=pass --no-fail-fast` Expected: all pass (baseline 1458 passed, 5 skipped plus the new tests); the flux-cli real-process tests `a_killed_large_file_resumes_from_its_validated_prefix` and siblings (`crates/flux-cli/tests/run.rs:604` onward) pass unchanged.
- [ ] **Step 5: Mutants:** (a) call `s.submit` BEFORE `writer.write_all` (swap the two statements, lines 827-835) -> test 1 reds on the job count; (b) drop the `abort` call at 848-852 -> test 2 reds (hang caught by the deadline); (c) keep a submit-sender clone alive inside the worker state so the channel never disconnects -> test 3 reds; (d) route `submit` to `wrote` always -> test 4's `spawned_for_tests` assertion reds.
- [ ] **Step 6: Commit** `feat(copy): the copy loop hands written buffers to the hashing worker (cut 9h, task 5)`.

---

### Task 6: docs, the gate, and the owner-gated measurement

**Files:** Modify `docs/superpowers/specs/2026-10-12-cut-9f1-chunk-checkpoints-design.md` (a short "Deviations and amendments recorded at merge (cut 9h)" note: the stop bound is now one buffer; hashing moved to a worker), `TODO.md` (the "Checkpoint hashing cost on many large files (both durabilities)" entry: record the measured outcome, close or reword), `README.md` only if `grep -n "first completed chunk\|hashing" README.md` finds text this change falsifies.

- [ ] **Step 1:** `cargo fmt`; `just check` > `check.log` 2>&1; read the exit code (`echo rc=$?`, never through a pipe). Expected: rc=0, all tests pass, no warnings.
- [ ] **Step 2:** `cargo clippy --workspace --all-targets --release -- -D warnings`. Expected: clean.
- [ ] **Step 3:** Update the three docs above; commit `docs: cut 9h merge notes (task 6)`.
- [ ] **Step 4 (controller, top level, owner approval first):** acceptance run per spec section 8: 100 files of 64 MiB, Normal then Strict, release builds of main `14dde8d` versus this branch, cache dropped per run, 4 runs x 2 passes alternating binary order, idle census with a busy-core control, no agents; script `measure9f1.py` pattern (copy from the earlier session scratchpad if still present, otherwise rewrite from the spec). Pass criteria: Normal wall median at least 1.5 s below main's with both passes' medians below main's; Strict not slower beyond the measured spread; user CPU about equal; `mixed` and the single-file case within spread. Record ranges and the uncontrolled load. If Normal misses the criterion the cut is reverted or closed as not worth its complexity (owner decides).
- [ ] **Step 5:** AGY-CAPSTONE over the committed implementation, rounds until GREEN; then agy-test-audit (ledger row in `docs/agy-test-audit-ledger.md` BEFORE the marker); then the finishing menu. No PR before those and the owner's say-so.

---

## Self-review (run by the author)

- Spec coverage: G1 -> Task 6 Step 4; G2 -> Task 4 test 1, Task 5 test 4; G3/I1 -> Task 5 test 1 and the loop order; I2 -> Task 4 test 1; I3 -> Task 4 test 5; I4 -> Task 4 tests 6-8; I5 -> Task 4 test 4, Task 5 tests 2-3; I6 -> Task 4 test 10, Task 3 test 5; I7 -> Task 3 test 2; 3.8 fallback -> Task 4 test 9; D1 -> Task 4 test 6; D6 -> Tasks 2-3, test seams; spec tests 1-9 map to Tasks 3-5 and the existing real-process tests (spec test 9 = unchanged `run.rs` tests, Task 5 Step 4).
- Placeholders: none. Items resolved at execution against live code, with where: the exact `Read` failure injection for Task 5 test 2 (if `FaultFs` has no mid-stream read fault, add one in Task 1's file and say so in the commit).
- Type consistency: `submit(buf, n, beat)` identical in Tasks 4 and 5; `Worker::finish`/`cancel_and_join` names identical in Tasks 3 and 4; `WalLink::try_send`/`fault` identical in Tasks 2 and 3; `POOL` defined once in Task 3.
- Proportion: the plan lists signatures, tests with assertions and mutants; bodies are not written out.
