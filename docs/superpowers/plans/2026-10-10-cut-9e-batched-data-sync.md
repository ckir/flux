# Cut 9e: a batched, parallel data-sync barrier - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Under `--durability=strict`, a batched tree publication defers each file's data `fsync` from staging to one barrier per batch, where up to 16 threads sync the pending temporaries at once; the notes, renames and claims then follow exactly as cut 9d left them. Strict small-file trees get the one cost cut 9d could not reach.

**Architecture:**
- `stage_file` gains a `SyncAt` argument: `Staging` (today's step 5, now after the metadata step) or `Barrier` (no sync; the open writer rides in `Staged<W>.writer`). Every caller but the Strict batched tree passes `Staging`.
- A new `copy::sync_staged_many` is the barrier: positional per-entry results from a `std::thread::scope` pool, the main thread beating the heartbeat on an absolute deadline while it waits.
- `flush_batch` calls the barrier FIRST, closes the writers, then runs its existing heartbeat-and-guard pair, discards the failed-sync temporaries, and continues with the unchanged 9d sequence. No guard call is added, so the end-to-end stall indices do not move.
- The walk caps open writers across the whole frame stack; the cap comes from the soft descriptor limit through `RunConfig`.
- Rollout: Tasks 1-2 are additive (no caller takes the new path); Task 3 switches the Strict batched tree to the barrier and is the one commit whose observable call order changes.

**Tech Stack:** Rust 2024 (MSRV 1.98.1), `std::thread::scope` and `std::sync::mpsc` (no new crate), `rustix` gains its `process` feature in `flux-platform` (unix) for `getrlimit`, the `FaultFs` fake, nextest through `just`.

**Spec:** `docs/superpowers/specs/2026-10-10-cut-9e-batched-data-sync-design.md` (approved by the owner 2026-10-10 after a 6-round panel). Executors read it with this plan. Where the two differ the spec wins and the executor reports the conflict, except in the numbered "Plan rulings" below, which settle what the spec leaves to the plan.

**Code base:** written against `spec/cut-9d` at `276da4b` (PR #82 with `main` merged in; its auto-merge was pending CI when this plan was written). The merge adds no code commit, so every citation below holds on `main` once #82 lands; Task 1's Step 0 re-verifies the cited lines before the first edit.

## Global Constraints

- The gate is `just check` = `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `typos` + `cargo nextest run --workspace --no-tests=pass` + `cargo test --doc --workspace`. Run it before every commit; use `cargo nextest run --workspace --no-fail-fast` when classifying failures. Push after every task and read CI on that commit (Windows and macOS failures surface only there).
- The barrier applies ONLY where cut 9d batches: a tree publication under `Durability::Strict` into a store with `supports_prepared()`, with `BatchPolicy.files > 1` (`tree.rs:934`, `:1081`). Normal mode, `files == 1`, single-file copy and lockless copies keep the inline sync.
- Constants (spec): `SYNC_THREADS = 16`, `OPEN_WRITERS_MAX = 256`. The effective cap is `min(256, soft RLIMIT_NOFILE / 4)`, never below 1. No CLI flag.
- Order inside a flush (spec decisions 4-5): barrier -> writers closed -> heartbeat -> guard -> discard the failed-sync temporaries -> `prepare_many` -> renames -> one `apply_recovery`. A lost lock writes and removes nothing (9d decision 8); a failed heartbeat removes every temporary of the batch.
- The Strict stall indices in `crates/flux-cli/tests/recovery.rs:252-266` and `run.rs:730` are oracles: they must pass UNCHANGED after Task 3 (spec decision 5). A changed index is a plan defect to report, not a constant to edit.
- Worker threads call `FileHandle::sync_all` and nothing else: never the guard, the heartbeat, the claim store or the fake's hooks (spec decision 2).
- `--json` output is section 53's fixed field list (`crates/flux-cli/src/report.rs:16-39`): the new counters do NOT go there.
- Do not add `unwrap` on store or filesystem results in non-test code; errors keep their `Code`.

## Plan rulings (left to the plan by the spec, decided here)

1. **`SyncAt` instead of a bool.** `stage_file`'s new parameter is `SyncAt::Staging | SyncAt::Barrier` (spec: "`defer_sync: bool` (or an options field)"). `Staged` becomes `Staged<W>` with `writer: Option<W>`, and the generic cascades to `Stage<W>`, `Pending<W>`, `Batch<W>` and `Frame<D: DirHandle>` (`batch: Batch<D::Writer>`); the alternative, a `Box<dyn FileHandle + Sync>`, needs a `'static` bound the trait does not have.
2. **Inline sync moves after the metadata step on every path** (spec decision 6 asks it of the barrier path; one placement keeps a single code path): `SyncAt::Staging` syncs after step 6 and before step 7. No existing test pins the old order (`rg '"sync_all' crates` lists presence checks and faults only: `copy.rs:1135`, `:1142`, `:1246`, `:1619`, `state.rs:1143` is the state file's own write).
3. **Where the open-writer count lives.** The spec says "`Shared` counts open writers"; `Shared` is immutable and `stage_into_batch` sees one frame, so the cap is enforced in `walk_into`'s `File` arm, before the frame borrow: it sums `batch.pending.len()` over the live frames and flushes shallowest-first while the sum is at or over the cap. The count is derived, not stored.
4. **Heartbeat failure during the wait is handled from `SyncOutcome.heartbeat`, not re-detected.** `Pulse::beat` returns `Ok(())` forever after one failure (`run/session.rs:61`), so the flush's own heartbeat call would not see it: `flush_batch` takes the heartbeat-failure branch directly when `SyncOutcome.heartbeat` is `Some`.
5. **The barrier's beat cadence is `BatchPolicy.beat_every`**, set by the run to `RunConfig.heartbeat_interval` (5 s; the debug env `FLUX_TEST_HEARTBEAT_INTERVAL_MS` already overrides it in the CLI). The barrier counts from its own start, so the record's gap can reach two intervals (10 s) against a 30 s lease threshold (`cleanup::LEASE_THRESHOLD`); `Pulse` rate-limits the real write, so a frequent call costs nothing.
6. **Counters channel.** `TreeOutcome` gains `barrier_max: Duration` and `cap_flushes: u64`; `Pulse` gains the longest gap between two record writes, surfaced as `Run.beat_gap_max`. The CLI writes all three as one JSON object to `FLUX_TEST_COUNTERS_FILE` in debug builds only (the `FLUX_TEST_STALL_FILE` pattern, `main.rs:170-188`). Nothing is added to stderr or `--json`. The acceptance run reads the counters from a debug binary in a separate, untimed pass.
7. **Fake additions.** `FaultFs::delay("sync_all", d)` makes every `FakeHandle::sync_all` sleep `d` (outside the fake's lock); `FakeHandle` records `drop_writer(<path>)` in the call log when it is dropped with a sink (readers have none). The fault key stays `sync_all` (consumed per handle at `create_new`, `fault_fs.rs:1198`); a middle entry is faulted with `on_nth("create_new", k, |fs| fs.fail("sync_all", ..))`.
8. **Descriptor limit plumbing.** `flux-core` does not depend on `flux-platform` (dev-dependency only, `crates/flux-core/Cargo.toml:19`), so the CLI reads `flux_platform::soft_descriptor_limit() -> Option<u64>` and passes it as `RunConfig.descriptor_limit: Option<u64>`; `tree::open_writers_cap(limit) -> usize` applies the formula.
9. **Every temporary is reported exactly once.** A failed-sync entry's report (`StrictDurabilityUnavailable` at `CopyStep::Durability`) replaces, never joins, the report the pair's failure branch would otherwise make for it: under a lost lock it goes through `on_report` with the temporary as `leftover`, and if that entry is entry 0 the returned `TargetLockBusy` error carries NO leftover (the run collects leftovers from the reports, `run/mod.rs:290-296`, and the abort's error separately, so the same temporary would otherwise be listed twice); under a failed heartbeat its temporary is removed with the others and the report carries a leftover only if the removal failed.

## Review Focus

Failure modes the spec implies that no single mechanism task owns; each has a test in the named task.

1. Every entry of a batch fails its sync (the volume went away): every temporary removed and reported, `prepare_many` never called, the flush returns `Ok(())` and the walk continues. Task 3.
2. The heartbeat fails DURING the barrier (not at the pair): the workers are joined, the writers dropped, every temporary removed, the flush returns `Err` at `CopyStep::Heartbeat`, no note is written, and no second heartbeat call is made. Task 3.
3. Strict single-file copy still syncs, once, after the metadata step; Normal never syncs. Task 1.
4. A `Replace` entry (a case-variant target) in a barrier batch publishes as under 9d: the existing replace tests of `tree.rs` and `run/tests.rs` are the oracle and stay green unchanged. Task 3.
5. A tree of many directories with one or two files each never trips the cap and reports `cap_flushes == 0`; a nested tree with a pending parent batch trips it and flushes the PARENT first. Task 4.

---

### Task 1: `SyncAt`, `Staged<W>.writer`, and the `Sync` bound

**Files:**
- Modify: `crates/flux-fs/src/fs.rs:103-108` (`FileHandle`), `:125` (`FileSystem::Writer`), `:227` (`DirHandle::Writer`)
- Modify: `crates/flux-core/src/copy.rs:462-474` (`Staged`, `Stage`), `:481-490` (`stage_file` signature), `:572-583` (step 5), `:649-650` (the return), `:694` and `:709` (`copy_file_guarded`), `:715-730` (`publish_staged`)
- Modify: `crates/flux-core/src/tree.rs:1315` (the `stage()` closure), `:369-394` (`Pending`, `Batch`), `:232-258` (`Frame`), plus every signature naming them (`copy_one :884`, `stage_into_batch :1277-1289`, `flush_batch :1352-1359`, `drain_batches :1577-1583`, `enter_dir :680-690`, `walk_into :533-540`, and the frame construction in `copy_tree_at :512-519`)
- Test: `crates/flux-core/src/copy.rs` (test module, `stage_hello` at `:784`)

**Interfaces:**
- Produces: `pub(crate) enum SyncAt { Staging, Barrier }`; `pub(crate) struct Staged<W> { pub temp: OsString, pub identity: FileIdentity, pub bytes_copied: u64, pub metadata_failures: Vec<MetadataFailure>, pub identity_degraded: Option<FileIdentity>, pub writer: Option<W> }`; `pub(crate) enum Stage<W> { Skipped(Outcome), Staged(Staged<W>) }`; `stage_file<F: DestinationRoot>(fs, src, parent, name, opts, guard, beat, before_create, sync: SyncAt) -> Result<Stage<F::Writer>, CopyError>`; `publish_staged<D: DirHandle>(parent, name, staged: Staged<D::Writer>, publish, guard, beat)`.
- `trait FileSystem { type Writer: FileHandle + Sync; }` and `trait DirHandle { type Writer: FileHandle + Sync; }`.

- [ ] **Step 0: State verification.** Open the cited lines and confirm: `Staged` has five fields and no writer (`copy.rs:462-468`); step 5 syncs before step 6 (`:572`); `Frame<D>` is unbounded (`tree.rs:232`); `Pending.staged: Staged` (`:380`). If any differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: Write the failing tests** in `copy.rs`'s test module (`stage_hello` grows a `SyncAt` parameter; the existing callers pass `SyncAt::Staging`):

```rust
#[test]
fn staging_at_the_barrier_keeps_the_writer_open_and_does_not_sync() {
    let fs = FaultFs::new();
    fs.write_file("/src", b"hello");
    let root = fs.destination_root(Path::new("/")).unwrap();
    let staged = stage_hello(&fs, &root, SyncAt::Barrier);
    assert!(staged.writer.is_some());
    assert!(!fs.called("sync_all("), "{:?}", fs.calls());
    assert_eq!(fs.read_file("/dst.flux-partial.op1").as_deref(), Some(&b"hello"[..]));
}

#[test]
fn staging_inline_syncs_once_after_the_metadata_step_and_returns_no_writer() {
    let fs = FaultFs::new();
    fs.write_file("/src", b"hello");
    let root = fs.destination_root(Path::new("/")).unwrap();
    let mut o = opts(); // `opts()` already sets `preserve_times: Preserve::Default`, so `set_times` is called
    o.durability = Durability::Strict;
    let staged = stage_with(&fs, &root, &o, SyncAt::Staging); // new helper: `stage_hello` with explicit opts
    assert!(staged.writer.is_none());
    let c = fs.calls();
    let sync = c.iter().position(|x| x.starts_with("sync_all(")).expect("synced");
    let times = c.iter().position(|x| x.starts_with("set_times(")).expect("times set");
    assert!(times < sync, "metadata before the sync: {c:?}");
    assert_eq!(c.iter().filter(|x| x.starts_with("sync_all(")).count(), 1);
}
```
Add `stage_with(fs, root, opts, sync) -> Staged<FakeHandle>` beside `stage_hello` (`copy.rs:784`), which becomes a call to it with `opts()`; the assertion is the order `set_times(` < `sync_all(` and a count of one.

- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core staging_` - expected: compile error (`SyncAt` undefined).

- [ ] **Step 3: Implement.**
  - `fs.rs`: add `+ Sync` to both `Writer` associated types. All three implementors (`StdFile`, `FakeHandle`, the test `NullWriter`) are already `Sync`; nothing else changes. If the compiler disagrees for any implementor, STOP and report (do not add `unsafe impl`).
  - `copy.rs`: `SyncAt`; `Staged<W>` with `writer: Option<W>`; `Stage<W>`; `stage_file` takes `sync: SyncAt`, moves the step-5 block to after step 6 (keeping its `StrictDurabilityUnavailable` at `CopyStep::Durability` and its `discard` on failure), guarded by `sync == SyncAt::Staging && opts.durability == Durability::Strict`; returns `writer: match sync { Staging => None, Barrier => Some(writer) }`. `copy_file_guarded` passes `SyncAt::Staging`. `publish_staged` destructures `writer` and drops it first (it is `None` on every path that reaches it after Task 3; dropping is the safe default).
  - `tree.rs`: the generic cascade of ruling 1 (`Frame<D: DirHandle>`; `Batch<W>` gets a manual `impl<W> Default` so no `W: Default` bound appears); the `stage()` closure at `:1315` passes `SyncAt::Staging` for now (Task 3 switches it).

- [ ] **Step 4: Gate.** `just check` - expected: green; the two new tests pass; no existing test changes.

- [ ] **Step 5: Commit.** `git commit -m "feat(copy): SyncAt and Staged<W>.writer, the Writer: Sync bound; inline sync after the metadata step"`

---

### Task 2: the barrier `sync_staged_many`, and the fake's delay and drop records

**Files:**
- Create: nothing; Modify: `crates/flux-core/src/copy.rs` (after `publish_staged`)
- Modify: `crates/flux-core/src/fault_fs.rs:19-...` (`Inner` gains `sync_delay: Option<Duration>`), `:620-629` (`FakeHandle`), `:660-672` (`sync_all`), the `FaultFs` helpers near `:850`
- Test: `crates/flux-core/src/copy.rs` test module; `crates/flux-core/src/fault_fs.rs` test module

**Interfaces:**
- Produces: `pub(crate) struct SyncOutcome { pub results: Vec<Result<(), FsError>>, pub heartbeat: Option<FsError> }`; `pub(crate) fn sync_staged_many<W: FileHandle + Sync>(writers: &[&W], threads: usize, beat: &Heartbeat<'_>, beat_every: Duration) -> SyncOutcome`.
- Fake: `FaultFs::delay(&self, name: &str, d: Duration)` (only `"sync_all"` is honoured; others are stored and ignored); the call-log string `drop_writer(<path>)`; `FaultFs::peak_concurrent_syncs(&self) -> usize` (the most `sync_all` calls in flight at once, counted under the fake's lock at entry and exit of `FakeHandle::sync_all`); `FaultFs::rendezvous(&self, name: &str, parties: usize, timeout: Duration)` (only `"sync_all"` honoured): each `sync_all` call, after incrementing the in-flight count, waits on a `Condvar` until the count reaches `parties` or `timeout` passes, so concurrency is proven by a meeting, not by a race against the scheduler.

- [ ] **Step 0: State verification.** `FakeHandle::sync_all` records `sync_all(<path>)` and consults `sync_fault` only (`fault_fs.rs:661-672`); there is no `impl Drop for FakeHandle` (`rg "impl Drop" crates/flux-core/src/fault_fs.rs` lists `FakeClaimStore` and `FakeLock` only). Otherwise STOP with `STATE_MISMATCH`.

- [ ] **Step 1: Write the failing tests.**

In `copy.rs` (helpers: `handles(fs, n)` creates `n` writers through `fs.create_new(Path::new(&format!("/t{i}")))`; `counting_beat()` returns a closure counting calls in a `Cell<u32>` and recording `std::thread::current().id()`):

```rust
#[test]
fn barrier_results_are_positional_and_every_writer_is_synced_once() {
    let fs = FaultFs::new();
    let ws = handles(&fs, 5);
    let refs: Vec<&FakeHandle> = ws.iter().collect();
    let out = sync_staged_many(&refs, 16, &no_heartbeat, Duration::from_secs(5));
    assert_eq!(out.results.len(), 5);
    assert!(out.results.iter().all(Result::is_ok) && out.heartbeat.is_none());
    assert_eq!(fs.calls().iter().filter(|c| c.starts_with("sync_all(")).count(), 5);
}

#[test]
fn a_faulted_writer_fails_at_its_own_index_only() {
    let fs = FaultFs::new();
    fs.on_nth("create_new", 3, |fs| fs.fail("sync_all", Code::StrictDurabilityUnavailable));
    let ws = handles(&fs, 4);
    let refs: Vec<&FakeHandle> = ws.iter().collect();
    let out = sync_staged_many(&refs, 2, &no_heartbeat, Duration::from_secs(5));
    let failed: Vec<usize> = out.results.iter().enumerate().filter(|(_, r)| r.is_err()).map(|(i, _)| i).collect();
    assert_eq!(failed, [2]);
    assert_eq!(out.results[2].as_ref().unwrap_err().code, Code::StrictDurabilityUnavailable);
}

#[test]
fn one_thread_syncs_in_order() {
    let fs = FaultFs::new();
    let ws = handles(&fs, 3);
    let refs: Vec<&FakeHandle> = ws.iter().collect();
    sync_staged_many(&refs, 1, &no_heartbeat, Duration::from_secs(5));
    let synced: Vec<String> = fs.calls().into_iter().filter(|c| c.starts_with("sync_all(")).collect();
    assert_eq!(synced, ["sync_all(/t0)", "sync_all(/t1)", "sync_all(/t2)"]);
}

#[test]
fn the_heartbeat_is_called_from_the_main_thread_once_the_interval_has_passed_in_total() {
    // 5 syncs x 20 ms = 100 ms, one worker; a 25 ms deadline must fire at least once. A fresh full timeout per
    // receive (the mutant) would never reach 25 ms, because a result lands every 20 ms.
    let fs = FaultFs::new();
    fs.delay("sync_all", Duration::from_millis(20));
    let ws = handles(&fs, 5);
    let refs: Vec<&FakeHandle> = ws.iter().collect();
    let (beat, count, thread) = counting_beat();
    let out = sync_staged_many(&refs, 1, &beat, Duration::from_millis(25));
    assert!(out.heartbeat.is_none());
    assert!(count.get() >= 1, "no heartbeat during a 100 ms barrier");
    assert_eq!(thread.get(), Some(std::thread::current().id()));
}

#[test]
fn sixteen_threads_sync_concurrently() {
    // Deterministic: every sync waits for a second one to be in flight (or 2 s). With real workers the two meet at
    // once and the peak is >= 2; under the mutant "ignore `threads`, one worker" each sync waits out the 2 s alone and
    // the peak stays 1 (red, and slow: a mutant run, not CI). No sleep, no scheduler race.
    let fs = FaultFs::new();
    fs.rendezvous("sync_all", 2, Duration::from_secs(2));
    let ws = handles(&fs, 8);
    let refs: Vec<&FakeHandle> = ws.iter().collect();
    let started = std::time::Instant::now();
    sync_staged_many(&refs, 16, &no_heartbeat, Duration::from_secs(5));
    assert!(fs.peak_concurrent_syncs() >= 2, "{}", fs.peak_concurrent_syncs());
    assert!(started.elapsed() < Duration::from_secs(2), "the syncs met; nobody waited out the timeout");
}

#[test]
fn a_failed_heartbeat_ends_the_wait_after_the_join_and_is_reported_once() {
    let fs = FaultFs::new();
    fs.delay("sync_all", Duration::from_millis(20));
    let ws = handles(&fs, 4);
    let refs: Vec<&FakeHandle> = ws.iter().collect();
    let calls = Cell::new(0u32);
    let beat = || { calls.set(calls.get() + 1); Err(FsError::new(Code::IoError, std::io::Error::other("torn"))) };
    let out = sync_staged_many(&refs, 1, &beat, Duration::from_millis(10));
    assert_eq!(out.heartbeat.as_ref().map(|e| e.code), Some(Code::IoError));
    assert_eq!(calls.get(), 1, "no second heartbeat call after a failure");
    assert_eq!(out.results.len(), 4, "every writer still has a result: the workers were joined");
    assert_eq!(fs.calls().iter().filter(|c| c.starts_with("sync_all(")).count(), 4);
}
```

In `fault_fs.rs`'s test module:

```rust
#[test]
fn a_dropped_writer_is_recorded_and_a_dropped_reader_is_not() {
    let fs = FaultFs::new();
    fs.write_file("/r", b"r");
    drop(fs.create_new(Path::new("/w")).unwrap());
    drop(fs.open_read(Path::new("/r")).unwrap());
    let drops: Vec<String> = fs.calls().into_iter().filter(|c| c.starts_with("drop_writer(")).collect();
    assert_eq!(drops, ["drop_writer(/w)"]);
}

#[test]
fn sync_all_from_several_threads_records_every_call() {
    let fs = FaultFs::new();
    let ws: Vec<FakeHandle> = (0..8).map(|i| fs.create_new(Path::new(&format!("/t{i}"))).unwrap()).collect();
    std::thread::scope(|s| { for w in &ws { s.spawn(move || w.sync_all().unwrap()); } });
    assert_eq!(fs.calls().iter().filter(|c| c.starts_with("sync_all(")).count(), 8);
}
```

- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core barrier_ one_thread a_faulted_writer the_heartbeat_is a_failed_heartbeat_ends sixteen_threads a_dropped_writer sync_all_from` - expected: compile errors (`sync_staged_many`, `delay`, `peak_concurrent_syncs` undefined).

- [ ] **Step 3: Implement `sync_staged_many`.** The algorithm is fixed by the spec, so it is given:
  - `workers = threads.clamp(1, writers.len().max(1))`; if `writers` is empty return at once.
  - Inside `std::thread::scope`: an `AtomicUsize` index and an `mpsc::channel::<(usize, Result<(), FsError>)>()`; each worker loops `i = next.fetch_add(1)`, stops at `i >= n`, sends `(i, writers[i].sync_all())`. The `Sender` clones are dropped by the workers; the main thread drops its own before waiting.
  - The main thread: `let start = Instant::now(); let mut deadline = start + beat_every; let mut slots: Vec<Option<Result<..>>> = vec![None; n]` (use a `Vec<Option<_>>` built with `resize_with`; `FsError` is not `Clone`); loop while `received < n`: `match rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))` - `Ok((i, r))` stores it; `Err(Timeout)` => if `heartbeat.is_none() { if let Err(e) = beat() { heartbeat = Some(e) } }` then `deadline += beat_every` (an absolute step, never "now + interval"); `Err(Disconnected)` => break.
  - After the scope: `results = slots.into_iter().map(|s| s.expect("every index is sent exactly once")).collect()`. A worker panic propagates out of the scope (spec known limit 1); no catch.
  - `Heartbeat` is `dyn Fn` without `Send`, so `beat` must be used only in the main thread's loop, never captured by a spawned closure.
- [ ] **Step 4: Implement the fake additions.** `Inner.delays: HashMap<String, Duration>` and `pub fn delay(&self, name: &str, d: Duration)`; in `FakeHandle::sync_all`, read the delay under the lock, release the lock, then `std::thread::sleep`, then record and fault as today (the record must stay under its own short lock, so concurrent syncs interleave in the log without a race). `Inner.syncs_in_flight` and `Inner.syncs_peak` (both `usize`), incremented and decremented in `sync_all` around the sleep, read by `peak_concurrent_syncs`; the rendezvous lives in its OWN pair, not in the fake's main lock: `Inner.rendezvous: Option<Arc<(Mutex<usize>, Condvar, usize, Duration)>>` (the shared in-flight count behind its own `Mutex`, the `Condvar`, `parties`, `timeout`). `sync_all` locks `inner` only long enough to clone that `Arc` (and to read the delay), releases it, then on the small pair: lock, increment, `notify_all`, `wait_timeout_while(.., |n| *n < parties)`, unlock; then the optional sleep; then relock `inner` to record the call, update `syncs_peak` from the small pair's count read before the wait, and apply the fault. `FaultFs.inner`'s type, `FakeLock`, `FakeClaimStore` and `open_read` are untouched; `peak_concurrent_syncs` reads `Inner.syncs_peak`. `impl Drop for FakeHandle`: if `self.sink` is `Some`, push `format!("drop_writer({})", self.path.display())` to `calls`. Check that no existing test asserts an exact full call log that a trailing `drop_writer(` would now break (`rg "assert_eq!\(.*calls\(\)" crates/flux-core/src` and read each hit); if one does, extend its expected list rather than filtering the log.

- [ ] **Step 5: Gate.** `just check` - green.

- [ ] **Step 6: Commit.** `git commit -m "feat(copy): sync_staged_many, the barrier with a main-thread heartbeat deadline; fake delay and drop records"`

---

### Task 3: the barrier inside `flush_batch`

**Files:**
- Modify: `crates/flux-core/src/tree.rs:29-66` (`TreeOutcome`), `:337-366` (constants, `BatchPolicy`), `:1315` (the `stage()` closure), `:1352-1389` (`flush_batch` head and the 4.1 pair), `:1390` (before `prepare_many`)
- Modify: `crates/flux-core/src/run/mod.rs:490-508` (`batch_policy` takes `&RunConfig`), `:287` (its call)
- Test: `crates/flux-core/src/tree.rs` test module (helpers `batched` `:2992`, `batched_with` `:3001`, `log` `:3034`, `count` `:3038`, `positions` `:3043`, `created` `:3054`, `renamed` `:3059`, `prepares` `:3064`, `NO_AGE` `:2960`, `policy` `:2962`)

**Interfaces:**
- Consumes: `SyncAt`, `Staged<W>.writer`, `sync_staged_many`, `SyncOutcome` (Tasks 1-2).
- Produces: `pub(crate) const SYNC_THREADS: usize = 16;` `BatchPolicy { .., sync_threads: usize, beat_every: Duration }` with `DEFAULT.sync_threads = SYNC_THREADS`, `DEFAULT.beat_every = crate::run::HEARTBEAT_INTERVAL`; `TreeOutcome { .., barrier_max: Duration, cap_flushes: u64 }` (`cap_flushes` stays 0 until Task 4); `fn batch_policy(cfg: &RunConfig) -> BatchPolicy` setting `beat_every: cfg.heartbeat_interval`.

- [ ] **Step 0: State verification.** `flush_batch` begins with `std::mem::take(batch).pending`, the empty check, the `claims` `let-else`, then the 4.1 heartbeat and guard, then the `notes` vector and `prepare_many` (`tree.rs:1360-1403`). `report.rs:902-912` builds a `TreeOutcome` with `..TreeOutcome::default()`. Otherwise STOP with `STATE_MISMATCH`.

- [ ] **Step 1: Write the failing tests** in `tree.rs`'s 9d test module. Shared pieces: `strict()` = `policy(64)` with `sync_threads: 1`; `n_files(n)` sources; a `beat_log(fs)` closure that records `fs.calls().len()` at each heartbeat call into a `RefCell<Vec<usize>>` and returns `Ok(())`; `drops(c)` = `positions(c, "drop_writer(")`.

```rust
#[test]
fn every_sync_of_a_batch_precedes_its_prepare_many_which_precedes_its_first_rename() {
    let fs = n_files(3);
    let (r, out, _) = batched(&fs, strict(), Durability::Strict);
    r.unwrap();
    let c = log(&fs);
    let syncs = positions(&c, "sync_all(");
    let prepare = positions(&c, "claim_prepare_many(")[0];
    assert_eq!(syncs.len(), 3, "{c:?}");
    assert!(syncs.iter().all(|&s| s < prepare), "{c:?}");
    assert!(prepare < renamed(&c, "f0"), "{c:?}");
    assert!(syncs.iter().all(|&s| s > created(&c, "f2")), "no sync before the last staging: {c:?}");
    assert_eq!(out.files_copied, 3);
}

#[test]
fn a_failed_sync_discards_and_reports_that_entry_and_the_rest_are_noted_and_published() {
    let fs = n_files(3);
    fs.on_nth("create_new", 2, |fs| fs.fail("sync_all", Code::StrictDurabilityUnavailable));
    let (r, out, got) = batched(&fs, strict(), Durability::Strict);
    r.unwrap();
    let c = log(&fs);
    assert_eq!(prepares(&c), ["claim_prepare_many(2)"], "{c:?}");
    assert_eq!(out.files_copied, 2);
    assert_eq!(got.len(), 1);
    let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
    assert_eq!((e.code(), e.step, e.leftover.is_none()), (Code::StrictDurabilityUnavailable, CopyStep::Durability, true));
    assert!(!fs.exists("/dst/f1") && !fs.exists("/dst/f1.flux-partial.op1"));
    let remove = first(&c, "remove_file(/dst/f1.flux-partial.op1)");
    let prepare = positions(&c, "claim_prepare_many(")[0];
    assert!(remove < prepare, "the discard precedes the notes: {c:?}");
}

#[test]
fn a_batch_whose_every_sync_fails_writes_no_note_and_the_walk_goes_on() {
    let fs = sources(&[("a/x", b"x"), ("a/y", b"y"), ("b/z", b"z")]);
    fs.on_nth("create_new", 1, |fs| fs.fail("sync_all", Code::IoError));
    fs.on_nth("create_new", 2, |fs| fs.fail("sync_all", Code::IoError));
    let (r, out, got) = batched(&fs, strict(), Durability::Strict);
    r.unwrap();
    let c = log(&fs);
    assert_eq!(prepares(&c), ["claim_prepare_many(1)"], "only b's batch is noted: {c:?}");
    assert_eq!((out.files_copied, got.len()), (1, 2));
    assert!(fs.exists("/dst/b/z") && !fs.exists("/dst/a/x") && !fs.exists("/dst/a/y"));
}

#[test]
fn the_writers_are_closed_before_the_post_barrier_heartbeat_on_every_path() {
    for scenario in ["ok", "heartbeat", "lost"] {
        let fs = n_files(2);
        let beats = std::cell::RefCell::new(Vec::new());
        let beat = |..| /* records fs.calls().len() at every call; in "heartbeat" it fails at its FIRST call after the barrier (the pair's; the batch has no delay, so no beat lands inside the barrier), which is call K+1 */;
        let guard = /* fails on its first call after the barrier in "lost": count guard calls, `failing_after(k, ..)` with k = the guard calls before the flush */;
        let (_, _, _) = batched_with(&fs, strict(), Durability::Strict, &guard, &beat);
        let c = log(&fs);
        let last_sync = *positions(&c, "sync_all(").last().unwrap();
        let first_beat_after = *beats.borrow().iter().find(|&&len| len > last_sync).expect("a heartbeat after the barrier");
        assert!(drops(&c).iter().all(|&d| d < first_beat_after), "{scenario}: {c:?}");
        assert_eq!(drops(&c).len(), 2, "{scenario}: {c:?}");
    }
}

#[test]
fn a_heartbeat_failure_after_the_barrier_removes_every_temporary_and_writes_no_note() {
    let fs = n_files(3);
    let beat = failing_after(/* the heartbeat calls before this flush */ K, &Cell::new(0));
    let (r, _, got) = batched_with(&fs, strict(), Durability::Strict, &unguarded, &beat);
    assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
    assert_eq!(fs.prepared_count(), 0);
    for i in 0..3 { assert!(!fs.exists(&format!("/dst/f{i}.flux-partial.op1")) && !fs.exists(&format!("/dst/f{i}"))); }
    assert!(got.is_empty(), "every removal succeeded, nothing to report: {got:?}");
}

#[test]
fn a_heartbeat_failure_during_the_barrier_is_handled_as_the_post_barrier_one_and_not_re_detected() {
    let fs = n_files(3);
    fs.delay("sync_all", Duration::from_millis(20));
    let p = BatchPolicy { beat_every: Duration::from_millis(10), ..strict() };
    let calls = Cell::new(0u32);
    let beat = || { calls.set(calls.get() + 1); if calls.get() == K + 1 { Err(..IoError..) } else { Ok(()) } }; // fails on its first call inside the barrier
    let (r, _, _) = batched_with(&fs, p, Durability::Strict, &unguarded, &beat);
    assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
    assert_eq!(fs.prepared_count(), 0);
    assert_eq!(calls.get(), K + 1, "no heartbeat call after the one that failed");
    for i in 0..3 { assert!(!fs.exists(&format!("/dst/f{i}.flux-partial.op1"))); }
}

#[test]
fn a_lost_lock_after_the_barrier_keeps_every_temporary_and_writes_no_note() {
    // Mutant: drop the pair (or move the barrier after it): this test then renames and notes.
    let fs = n_files(3);
    let guard = failing_after(/* guard calls before the flush */ G, &Cell::new(0));
    let (r, _, got) = batched_with(&fs, strict(), Durability::Strict, &guard, &no_heartbeat);
    let e = r.unwrap_err();
    assert_eq!(e.code(), Code::TargetLockBusy);
    assert!(e.leftover.is_some(), "entry 0's temporary rides in the error");
    assert_eq!(got.len(), 2, "entries 1 and 2 are reported kept: {got:?}");
    assert_eq!(fs.prepared_count(), 0);
    assert!(!fs.called("remove_file("), "{:?}", fs.calls());
    for i in 0..3 { assert!(fs.exists(&format!("/dst/f{i}.flux-partial.op1"))); }
}

#[test]
fn a_failed_sync_entry_is_still_reported_when_the_lock_is_lost_at_the_pair() {
    let fs = n_files(2);
    fs.on_nth("create_new", 1, |fs| fs.fail("sync_all", Code::StrictDurabilityUnavailable));
    let guard = failing_after(G, &Cell::new(0));
    let (r, _, got) = batched_with(&fs, strict(), Durability::Strict, &guard, &no_heartbeat);
    let e = r.unwrap_err();
    assert_eq!(e.code(), Code::TargetLockBusy);
    assert!(e.leftover.is_none(), "entry 0 is the failed-sync entry: its report carries the leftover, not the error (ruling 9)");
    let reported: Vec<(Code, bool)> = got.iter().map(|f| match &f.cause { TreeFailureCause::Copy(e) => (e.code(), e.leftover.is_some()), c => panic!("{c:?}") }).collect();
    assert_eq!(reported, [(Code::StrictDurabilityUnavailable, true), (Code::TargetLockBusy, true)], "{got:?}");
}

#[test]
fn a_failed_sync_entry_under_a_heartbeat_failure_is_reported_once_with_the_sync_code() {
    let fs = n_files(2);
    fs.on_nth("create_new", 1, |fs| fs.fail("sync_all", Code::StrictDurabilityUnavailable));
    let beat = failing_after(K, &Cell::new(0));
    let (r, _, got) = batched_with(&fs, strict(), Durability::Strict, &unguarded, &beat);
    assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
    assert_eq!(got.len(), 1, "{got:?}");
    let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
    assert_eq!((e.code(), e.leftover.is_none()), (Code::StrictDurabilityUnavailable, true), "removed, so no leftover");
    assert!(!fs.exists("/dst/f0.flux-partial.op1") && !fs.exists("/dst/f1.flux-partial.op1"));
}

#[test]
fn one_and_sixteen_threads_give_the_same_outcome() {
    let a = { let fs = n_files(70); let (r, out, got) = batched(&fs, BatchPolicy { sync_threads: 1, ..policy(64) }, Durability::Strict); r.unwrap(); (out.files_copied, out.bytes_copied, got.len(), fs.claim_count(), fs.prepared_count()) };
    let b = { let fs = n_files(70); let (r, out, got) = batched(&fs, BatchPolicy { sync_threads: 16, ..policy(64) }, Durability::Strict); r.unwrap(); (out.files_copied, out.bytes_copied, got.len(), fs.claim_count(), fs.prepared_count()) };
    assert_eq!(a, b);
    assert_eq!(a.0, 70);
    // This test cannot tell 16 threads from 1 (a one-worker mutant passes it); Task 2's `sixteen_threads_sync_concurrently` does.
}

#[test]
fn barrier_max_records_the_longest_barrier() {
    let fs = n_files(2);
    fs.delay("sync_all", Duration::from_millis(50));
    let (r, out, _) = batched(&fs, strict(), Durability::Strict);
    r.unwrap();
    assert!(out.barrier_max >= Duration::from_millis(50), "{:?}", out.barrier_max); // mutant: a hardcoded zero
}

#[test]
fn the_default_policy_syncs_from_sixteen_threads_at_the_heartbeat_interval() {
    assert_eq!((BatchPolicy::DEFAULT.sync_threads, BatchPolicy::DEFAULT.beat_every), (16, crate::run::HEARTBEAT_INTERVAL));
}
```
`K` and `G` (the heartbeat and guard calls that precede the first flush of a 3-file batch) are derived from the log by the implementer, as 9d's tests derive theirs: count `beat`/`guard` calls in a passing run with counting closures, then assert the count in the test so a shift is visible. Replace the `/* .. */` sketches with the real closures; the shape of each is the existing `failing_after` (`tree.rs:2792`).

- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core tree::` - expected: compile errors (`sync_threads`, `beat_every`, `barrier_max` undefined).

- [ ] **Step 3: Implement.**
  - Constants and `BatchPolicy` fields; `TreeOutcome` fields (`#[derive(Default)]` covers `Duration`).
  - `run/mod.rs`: `batch_policy(cfg: &RunConfig)` sets `beat_every: cfg.heartbeat_interval` (debug age override unchanged); the call at `:287` passes `cfg`.
  - The `stage()` closure (`tree.rs:1315`) passes `SyncAt::Barrier` when `cx.opts.durability == Durability::Strict`, else `SyncAt::Staging` (a Normal batch never reaches here; the branch is for completeness and costs nothing).
  - `flush_batch`, after the `claims` `let-else` and BEFORE the 4.1 pair:
    1. Two parallel vectors built in one pass over `entries.iter().enumerate()`: `writers: Vec<&W>` (each `p.staged.writer.as_ref()` that is `Some`) and `indices: Vec<usize>` (that entry's position), so `synced.results[j]` belongs to `entries[indices[j]]`; an entry without a writer (staged `Staging`; none today, but the code must not assume it) counts as synced; `let started = Instant::now(); let synced = sync_staged_many(&writers, cx.batch.sync_threads, cx.beat, cx.batch.beat_every); out.barrier_max = out.barrier_max.max(started.elapsed());`
    2. Close: `for p in &mut entries { p.staged.writer = None; }` (dropping records `drop_writer(` in the fake).
    3. Map results onto entries: `failed: Vec<Option<FsError>>` per entry (`Some(e)` for a failed sync; `None` for a synced entry or one without a writer), so each later report and `discard` has the sync's own error as its `source`.
    4. If `synced.heartbeat` is `Some(e)`: run the heartbeat-failure block (`:1368-1379`) with that `e` and return; do not call `cx.beat` again (ruling 4). Refactor that block into a helper `fn stop_held_before_notes(..)` shared by this call site and the pair's, extended per ruling 9: a failed-sync entry's temporary is removed like the others, but it is reported through `on_report` as `StrictDurabilityUnavailable` at `CopyStep::Durability` (a leftover only if its removal failed) instead of the heartbeat leftover report.
    5. The 4.1 pair as today. In its lost-lock branch, every entry (failed-sync ones included) is kept; a failed-sync entry is reported ONCE through `on_report`, as `TreeFailureCause::Copy` of a `CopyError` at `CopyStep::Durability` with `Code::StrictDurabilityUnavailable` and its temporary as `leftover` (relative to DEST, as `report_kept` builds it), in place of the `TargetLockBusy` report; if entry 0 is such an entry, the returned error is `CopyError::at(CopyStep::Publish, lost)` with no leftover (ruling 9). Concretely the branch becomes: `for (i, p) in entries.iter().enumerate().skip(1) { if failed[i] { report the sync failure with its temporary as leftover } else { report_kept(..) } }`, then the return built from entry 0 by the same test.
    6. After the pair passes: partition `entries` into `(failed, ok)`; for each failed one, `discard(parent, &temp, CopyStep::Durability, Code::StrictDurabilityUnavailable, e.source, cx.guard)` with `e` its stored `FsError`, then `finish_copy(path, Err(that), out, on_report)` (it reports and returns `Ok` for this code). If `finish_copy` returns a stop (a `TargetLockBusy` the discard's own guard noticed): report every entry not yet settled, failed and ok alike, through `report_kept` with the stop's cause, exactly as the per-entry `prepare` fallback does at `tree.rs:1425-1431`, then return the stop (ruling 9: nothing is left unreported). Then the unchanged 9d sequence from `// 4.2` on `ok`. An empty `ok` returns `Ok(())` after the discards (no `prepare_many(0)`).
  - The heartbeat-failure block removes every temporary including the failed-sync ones (they are all in `entries`).

- [ ] **Step 4: Gate and oracles.** `just check` - green. Then confirm the stall oracles did not move: `cargo nextest run -p flux-cli --test recovery --test run` passes with `AT_D1_F000_PUBLISH = 435`, `AT_D1_CREATE = 305`, `AT_D1_F000_PUBLISH_NORMAL = 306` and `run.rs`'s constants untouched. If any fails, STOP and report the new index: it means a guard call was added, which the spec forbids.

- [ ] **Step 5: Mutants (named per test, run one at a time, revert each).** Skip the barrier call -> `every_sync_of_a_batch_precedes...` red; swallow a failed result (treat all as ok) -> `a_failed_sync_discards...` red; remove the pair -> `a_lost_lock_after_the_barrier...` red; treat a failed heartbeat as a lost lock -> `a_heartbeat_failure_after_the_barrier...` red; re-call `cx.beat` instead of using `SyncOutcome.heartbeat` -> `..._not_re_detected` red (the count); keep the writers until after `prepare_many` -> `the_writers_are_closed...` red; hardcode `barrier_max = Duration::ZERO` -> `barrier_max_records...` red. Record the seven results in the commit message body.

- [ ] **Step 6: Commit.** `git commit -m "feat(tree): the data-sync barrier runs before the flush's heartbeat and guard; failed syncs discarded before the notes"`

---

### Task 4: the open-writer cap

**Files:**
- Modify: `crates/flux-core/src/tree.rs:337-366` (`OPEN_WRITERS_MAX`, `BatchPolicy.open_writers`, `open_writers_cap`), `:591-613` (`walk_into`'s `File` arm)
- Modify: `crates/flux-core/src/run/mod.rs:50-67` (`RunConfig.descriptor_limit: Option<u64>`), `batch_policy`
- Modify: `crates/flux-platform/src/lib.rs` (`pub fn soft_descriptor_limit() -> Option<u64>`), `crates/flux-platform/Cargo.toml:18-19` (`rustix = { workspace = true, features = ["process"] }` on the unix dependency)
- Modify: `crates/flux-cli/src/main.rs:154-165` (`run_config` sets `descriptor_limit: flux_platform::soft_descriptor_limit()`; the literal at `:305-315` is a `CleanupConfig` and is untouched)
- Modify: `crates/flux-cli/tests/recovery.rs:60` and `:259-260`, `crates/flux-cli/tests/run.rs:83` (the `Stalled` helpers pin `FLUX_TEST_OPEN_WRITERS=256`; see Step 3)
- Modify: every other FULL `RunConfig` literal (one without a `..base` struct update) sets `descriptor_limit: None`: `crates/flux-core/tests/safety_std_fs.rs:125`, `crates/flux-core/tests/replace_std_fs.rs:25` and `:114`, and those of the 20 literals in `crates/flux-core/src/run/tests.rs` (`:27`, `:40` and the rest) that do not end in `..cfg()` or `..restart()` (12 of the 20 do and need nothing). `rg -n "RunConfig \{" crates` prints 29 lines: the struct (`run/mod.rs:50`), its `Debug` impl (`:72`) and 27 literals.
- Test: `tree.rs` test module; `flux-platform` unit test

**Interfaces:**
- Produces: `pub(crate) const OPEN_WRITERS_MAX: usize = 256;` `pub(crate) fn open_writers_cap(soft_limit: Option<u64>) -> usize` = `min(256, limit / 4).max(1)`, `256` for `None`; `BatchPolicy.open_writers: usize` (`DEFAULT = OPEN_WRITERS_MAX`); `RunConfig.descriptor_limit: Option<u64>`; `flux_platform::soft_descriptor_limit()` = `rustix::process::getrlimit(Resource::Nofile).current` on unix, `None` on Windows.

- [ ] **Step 0: State verification.** `walk_into`'s `File` arm borrows `stack.last_mut()` before `copy_one` (`tree.rs:593-610`); `RunConfig` has eight fields ending in `resume` (`run/mod.rs:50-67`); `flux-platform`'s unix `rustix` dependency has no `features` key (`Cargo.toml:18-19`); `rg -n "RunConfig \{" crates` prints 29 lines as described under Files; this box's `ulimit -Sn` is 524288 (so the cap is 256 here). Otherwise STOP with `STATE_MISMATCH`.

- [ ] **Step 1: Write the failing tests.**

```rust
#[test]
fn open_writers_cap_is_a_quarter_of_the_soft_limit_capped_at_256_and_at_least_1() {
    assert_eq!(open_writers_cap(None), 256);
    assert_eq!(open_writers_cap(Some(1024)), 256);
    assert_eq!(open_writers_cap(Some(4096)), 256);
    assert_eq!(open_writers_cap(Some(64)), 16);
    assert_eq!(open_writers_cap(Some(3)), 1);
}

#[test]
fn the_cap_flushes_the_shallowest_pending_batch_first_and_never_holds_more_writers() {
    let fs = sources(&[("a0", b"a"), ("m/x", b"x"), ("m/y", b"y"), ("m/z", b"z"), ("w", b"w")]);
    let p = BatchPolicy { open_writers: 2, ..strict() };
    let (r, out, _) = batched(&fs, p, Durability::Strict);
    r.unwrap();
    let c = log(&fs);
    // Running count of live writers over the log: +1 at create_new, -1 at drop_writer; never above 2.
    let (mut live, mut peak) = (0i64, 0i64);
    for x in &c { if x.starts_with("create_new(") { live += 1; peak = peak.max(live); } else if x.starts_with("drop_writer(") { live -= 1; } }
    assert_eq!(peak, 2, "{c:?}");
    assert!(renamed(&c, "a0") < created(&c, "m/y"), "the parent's batch flushes before m/y is staged: {c:?}");
    assert_eq!(out.files_copied, 5);
    assert!(out.cap_flushes >= 1, "{}", out.cap_flushes);
}

#[test]
fn many_small_directories_never_trip_the_cap() {
    let entries: Vec<(String, Vec<u8>)> = (0..40).map(|d| (format!("d{d:02}/f"), b"f".to_vec())).collect();
    let refs: Vec<(&str, &[u8])> = entries.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    let fs = sources(&refs);
    let (r, out, _) = batched(&fs, strict(), Durability::Strict);
    r.unwrap();
    assert_eq!((out.files_copied, out.cap_flushes), (40, 0));
}
```
In `flux-platform` (unix-only test): `soft_descriptor_limit()` is `Some(n)` with `n >= 64` (every supported platform's default is at least 256; 64 leaves margin for a constrained CI sandbox).

- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core open_writers_cap the_cap_flushes many_small` - compile errors.

- [ ] **Step 3: Implement.**
  - `tree.rs`: the constant, the field, `open_writers_cap`. In `walk_into`'s `File` arm, before the `stack.last_mut()` borrow: `while pending_total(stack) >= cx.batch.open_writers { flush the first (index-lowest) live frame whose batch is non-empty via flush_batch(cx, dir, names, batch, out, on_report)?; out.cap_flushes += 1; }` where `pending_total` sums `batch.pending.len()` over `Frame::Live` frames. The loop ends because each iteration empties one batch; if no frame has a pending entry the condition is false (`0 >= cap` only when cap is 0, which `open_writers_cap` never returns; `debug_assert!(cx.batch.open_writers >= 1)`).
  - `run/mod.rs`: `RunConfig.descriptor_limit`; `batch_policy(cfg)` sets `open_writers: crate::tree::open_writers_cap(cfg.descriptor_limit)`.
  - `flux-platform`: the `process` feature on the unix `rustix` dependency; `soft_descriptor_limit()` with `#[cfg(unix)]` and `#[cfg(windows)]` bodies.
  - `flux-cli`: `run_config` sets `descriptor_limit: flux_platform::soft_descriptor_limit()`; every full test literal sets `descriptor_limit: None` (the compiler lists them).
  - **Debug-only override, so the stall oracles never depend on the host's descriptor limit:** in a debug build `batch_policy` reads `FLUX_TEST_OPEN_WRITERS=<n>` and uses it as `open_writers` (absent or unparsable: the computed cap), exactly as `FLUX_TEST_BATCH_AGE_MS` is read (`run/mod.rs:495-503`); a release build has no override. The `Stalled` helpers pin `FLUX_TEST_OPEN_WRITERS=256` beside `FLUX_TEST_BATCH_AGE_MS` (`crates/flux-cli/tests/recovery.rs:60`, `run.rs:83`), and the comment at `recovery.rs:259-260` names both pins. Why: macOS's default soft limit is 256 (a cap of 64, which the stall tree's single live directory never reaches, but a smaller limit on a constrained runner would force early flushes and move every index); Task 5's counters test sets the same pin.

- [ ] **Step 4: Gate.** `just check` - green; `cargo deny check` is part of CI (`Cargo deny` job) and must stay green with the feature change (no new crate).

- [ ] **Step 5: Mutant.** Ignore `open_writers` (delete the while loop) -> `the_cap_flushes_...` red (peak 4, `cap_flushes` 0). Record it in the commit body.

- [ ] **Step 6: Commit.** `git commit -m "feat(tree): cap open writers across the frame stack at min(256, RLIMIT_NOFILE/4), flushing the shallowest batch first"`

---

### Task 5: counters for the acceptance run (debug-only file) and the heartbeat gap

**Files:**
- Modify: `crates/flux-core/src/run/session.rs:38-75` (`Pulse.gap_max: Cell<Duration>`, updated in `beat` on a successful write with `last.elapsed()` before `last` is reset; `pub(crate) fn gap_max(&self)`)
- Modify: `crates/flux-core/src/run/mod.rs:91-98` (`Run<T>.beat_gap_max: Duration`, set from `locked.pulse.gap_max()` where `run.copy` is set)
- Modify: `crates/flux-cli/src/main.rs` (after the run's reporting at `:240-262`: `counters_file(&run, outcome)` in debug builds only, the `debug_hook` pattern at `:170-188`)
- Test: `crates/flux-cli/tests/run.rs` (an end-to-end test under `Stalled`'s env pattern at `:83`)

**Interfaces:**
- Produces: debug-only env `FLUX_TEST_COUNTERS_FILE=<path>`: after a tree run the CLI writes `{"barrier_max_ms":<u64>,"cap_flushes":<u64>,"beat_gap_max_ms":<u64>}` (one line, `serde_json`) to `<path>`. Release builds ignore the variable.

- [ ] **Step 1: Write the failing test** in `run.rs`: run the debug binary on a 10-file Strict tree with `FLUX_TEST_COUNTERS_FILE` set (and `FLUX_TEST_BATCH_AGE_MS=3600000` as the other tests do); assert the file exists, parses as JSON with exactly those three keys, `cap_flushes == 0`, and `barrier_max_ms <= 60000`. A second assertion runs Normal and expects `barrier_max_ms == 0` (Normal never reaches the barrier).

- [ ] **Step 2: Run it to see it fail** (`cargo nextest run -p flux-cli --test run counters`): the file is not written.

- [ ] **Step 3: Implement** the three pieces. `Run<T>` is built in `run/mod.rs`; set `beat_gap_max` at the same point `resumed` is set (`:263`), from the pulse. The CLI function reads the env, builds the object from `outcome.barrier_max`, `outcome.cap_flushes`, `run.beat_gap_max`, writes with `std::fs::write`, and ignores every error (a test aid must not fail a copy).

- [ ] **Step 4: Gate.** `just check` - green.

- [ ] **Step 5: Commit.** `git commit -m "feat(cli): debug-only FLUX_TEST_COUNTERS_FILE with the barrier, cap and heartbeat-gap counters"`

---

### Task 6: docs - known limits, debt, the 9e -> 9f renumbering

**Files:**
- Modify: `TODO.md` (a "Cut 9e known limits" section after "Cut 9d debt" at `:534-548`, the seven limits of the spec's "Known limits" verbatim; a "Cut 9e debt" section: 9e-2 `syncfs` and its two conditions; the acceptance measurement's heartbeat-gap figure to be filled by Task 7)
- Modify: `docs/superpowers/specs/2026-10-09-cut-9d-group-commit-design.md:5` and `:192` ("cut 9e" -> "cut 9f" where it names chunk checkpoints, partial-file resume and `--resume-verify`)
- Check: `rg -n "cut 9e|cut-9e" docs TODO.md FLUX_FULL_UPDATED_SPEC_V16.md` lists only this spec, this plan and the renamed lines.

- [ ] **Step 1: Edit, run `typos`, commit.** `git commit -m "docs: cut 9e known limits and debt; chunk checkpoints renumbered to cut 9f"`

---

### Task 7: the acceptance measurement (OWNER-GATED, top level, idle machine)

Runs ONLY after the owner says so, from the top-level session with no other tool call until it completes (timing discipline). Not delegated.

**Files:**
- Create: the scratchpad driver `measure9e.py` (from `measure9d.py`: three binaries alternating per pass - `pre` = `main@49fabc4`, `d9` = the merged 9d head, `e9` = this branch; cases `small`, `flat5000`, `nested500x10`, `onepdir5000x1`, `mixed`, `large`; 2 passes x 3 runs, page cache dropped, `strace -f -c -e trace=fsync,fdatasync` once per case and binary, criterion `small_1000x4kib` on the three, idleness census with a busy-core control, `top` snapshots). A third, untimed pass runs the DEBUG `e9` binary once per case with `FLUX_TEST_COUNTERS_FILE` and records the counters.
- Modify: the PR description and `TODO.md` (the figures).

- [ ] **Step 1: Build the three binaries** (`git worktree add` for `d9` at the merged 9d head; `pre9d` exists in the scratchpad), verify the field is clear (`pgrep -af "measure9|flux copy|cargo bench"` empty), reset `target/bench-work`.
- [ ] **Step 2: Ask the owner**, then run backgrounded and go idle.
- [ ] **Step 3: Report** against the spec's gates, all as same-session ratios: Strict small and flat5000 at least 50% faster than `d9`'s median; syncs per file in `[0.99, 1.15]`; Normal within the spread; Strict large and mixed not worse than their spread; RSS within 2 MiB of `d9`; nested, onepdir, the background-writer run and the counters (longest barrier, longest heartbeat gap, cap flushes) reported, not gated. Quote ranges, state the uncontrolled load. A failed gate is stated in the PR; thresholds are not moved.

---

## Self-review (run after writing; results recorded here)

1. **Spec coverage.** Decision 1 -> Tasks 1, 3. Decision 2 (threads, deadline, main-thread heartbeat, join-before-return) -> Task 2 and ruling 4. Decision 3 (`Sync` bound) -> Task 1. Decisions 4-5 (positional results, the order barrier/close/pair/discard/notes, reports never shadowed, no new guard) -> Task 3 Step 3 and its tests 2, 7, 8, and Step 4's oracle check. Decision 6 (metadata before sync, writers closed before the pair) -> Tasks 1 and 3 (test 4). Decision 7 (cap, RLIMIT) -> Task 4. Decision 8 (platforms) -> no code; CI is the oracle. Decision 9 (9e-2) -> Task 6 debt. Interfaces: `SyncOutcome` and `sync_staged_many(entries, threads, beat, interval)` -> Task 2 (`writers: &[&W]` is the "entries"); `barrier_max`, `cap_flushes` -> Tasks 3, 4, 5. Tests section: order, partial failure, lease, heartbeat-after, lost-lock-after, deadline, writers closed, equivalence, `barrier_max`, cap, thread safety, re-derived oracles, non-vacuity -> Tasks 2-4 (the "lease" test is Task 2's deadline test, which asserts the main thread id). Measurement -> Task 7. Known limits -> Task 6. Gap: none found.
2. **Step scan.** Every code step names the file, the signature and the values; the only given bodies are the barrier algorithm (the spec fixes it) and the cap loop (its termination argument matters). The test sketches with `/* .. */` in Task 3 name the closure shape (`failing_after`) and what the constant must be derived from; the implementer writes them.
3. **Type consistency.** `Staged<W>`/`Stage<W>` (Task 1) are what Task 3 reads `writer` from; `sync_staged_many(&[&W], usize, &Heartbeat, Duration) -> SyncOutcome` (Task 2) is what Task 3 calls; `BatchPolicy.{sync_threads, beat_every}` (Task 3) and `.open_writers` (Task 4) are set by the one `batch_policy(cfg)`; `TreeOutcome.{barrier_max, cap_flushes}` (Tasks 3, 4) are what Task 5 writes; `RunConfig.descriptor_limit` (Task 4) is read by `batch_policy`.
4. **Review Focus.** Items 1-5 each have a named test (Task 3 tests 3, 6; Task 1 test 2; the 9d replace tests as oracle in Task 3 Step 4; Task 4 tests 2-3).
5. **Proportion.** The plan is about the spec's length plus the test code; no function body is transcribed beyond the two algorithms above.

**Exhaustiveness audit (owner-facing):** the spec's open item (whether a test pins the old step 5/6 order) is closed by ruling 2 with the grep that proves it. Remaining to the executor by design: the two call-count constants `K` and `G` in Task 3's tests (derived from the log at execution, as 9d did), and the exact `Preserve` variant in Task 1's second test (read from `copy.rs:772`).
