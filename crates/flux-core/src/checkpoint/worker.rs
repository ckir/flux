//! The hashing worker (cut 9h): one plain thread per large file that turns copied buffers into chunk digests and
//! sends them to the log as `Range` records, so the copy thread does not hash.
//!
//! Buffers move by value: the main thread submits a filled buffer AFTER writing it, the worker hashes it and hands it
//! back through a free list, so at most `POOL` buffers exist per file. The worker never beats (the heartbeat is not
//! shareable); the main thread beats while it waits on the worker.

use super::sink::ATTEMPT_ID;
use crate::copy::Heartbeat;
use crate::wal::digest::ChunkHasher;
use crate::wal::record::Range;
use crate::wal::writer::{Msg, SEND_RETRY, WalFault, WalLink};
use crate::wal::{CHUNK_SIZE, Digest};
use flux_fs::{Code, FsError};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[cfg(test)]
use std::cell::Cell;

/// Buffers per file: the caller's own plus `POOL - 1` extras.
pub(super) const POOL: usize = 4;

/// What the worker starts from: the sink's hashing state, moved in at the first `submit`.
pub(super) struct WorkerArgs<W> {
    pub link: WalLink<W>,
    pub file_id: Vec<u8>,
    pub hasher: ChunkHasher,
    pub digests: Vec<Digest>,
    pub range_start: u64,
    pub range_chunks: usize,
    /// The free list's initial content; with the caller's own buffer exactly `POOL` buffers circulate.
    pub extra_buffers: Vec<Vec<u8>>,
    #[cfg(test)]
    pub inject: Inject,
}

/// Test-only faults the worker injects into itself.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Inject {
    /// After this many `Range` messages were sent, the next send fails as if the link had.
    pub fail_send_after_ranges: Option<usize>,
    /// Panic on this job (1-based).
    pub panic_on_job: Option<usize>,
}

/// Test-only override of the mode selection.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Force {
    Auto,
    OneCpu,
    SpawnFails,
    /// A worker whatever the durability (the Strict held-writer tests need one).
    Worker,
}

#[cfg(test)]
thread_local! {
    pub(super) static FORCE: Cell<Force> = const { Cell::new(Force::Auto) };
    pub(super) static INJECT: Cell<Inject> =
        const { Cell::new(Inject { fail_send_after_ranges: None, panic_on_job: None }) };
    static SPAWNED: Cell<usize> = const { Cell::new(0) };
    /// Jobs this thread handed to a worker through `Worker::submit` (ruling R9).
    static SUBMITTED: Cell<usize> = const { Cell::new(0) };
}

/// Worker threads alive, counted per SPAWNING thread: libtest runs tests as threads of one process, so a
/// process-wide count would see other tests' workers (ruling R12).
#[cfg(test)]
static LIVE: Mutex<Option<std::collections::HashMap<std::thread::ThreadId, usize>>> =
    Mutex::new(None);

/// Decrements the spawning thread's entry in `LIVE` when dropped: created at spawn (on the spawning thread), moved
/// into the worker thread, dropped last in its body (or with the closure, when the spawn fails). It carries the
/// spawner's id because it drops on the worker thread.
#[cfg(test)]
struct LiveGuard(std::thread::ThreadId);

#[cfg(test)]
impl LiveGuard {
    fn new() -> LiveGuard {
        let me = std::thread::current().id();
        *LIVE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert_with(Default::default)
            .entry(me)
            .or_insert(0) += 1;
        LiveGuard(me)
    }
}

#[cfg(test)]
impl Drop for LiveGuard {
    fn drop(&mut self) {
        if let Some(n) = LIVE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert_with(Default::default)
            .get_mut(&self.0)
        {
            *n -= 1;
        }
    }
}

/// The worker's hashing state when it ends cleanly: what `FileSink::complete` continues from.
pub(super) struct WorkerEnd {
    pub hasher: ChunkHasher,
    pub digests: Vec<Digest>,
    pub range_start: u64,
}

#[derive(Debug)]
pub(super) enum WorkerFail {
    /// The heartbeat failed while waiting; the error is the heartbeat's, unchanged.
    Heartbeat(FsError),
    /// The worker recorded a fault (the log died, or its send failed).
    Fault(WalFault),
    /// The worker ended without a result (it panicked).
    Gone,
}

/// The worker is gone: its submit channel is closed.
#[derive(Debug)]
pub(super) struct Gone;

struct Job {
    buf: Vec<u8>,
    n: usize,
}

/// Why the worker body ended.
// Built once per worker and moved straight into the result channel: boxing `WorkerEnd` would buy nothing.
#[allow(clippy::large_enum_variant)]
enum Exit {
    End(WorkerEnd),
    Fault(WalFault),
    Cancelled,
}

pub(super) struct Worker {
    /// `None` once closed (`finish`, `cancel_and_join`).
    submit: Option<SyncSender<Job>>,
    free: Receiver<Vec<u8>>,
    result: Receiver<Result<WorkerEnd, WalFault>>,
    cancel: Arc<AtomicBool>,
    /// Set (after `fault`) when the worker records a fault; read first so `Worker::fault` is cheap.
    faulted: Arc<AtomicBool>,
    fault: Arc<Mutex<Option<WalFault>>>,
    /// `None` once the thread is reaped.
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    /// Whether a worker should be used at all: not on a single CPU.
    pub(super) fn available() -> bool {
        #[cfg(test)]
        match FORCE.get() {
            Force::OneCpu => return false,
            // The worker tests do not depend on the machine's CPU count.
            Force::Auto | Force::Worker => return true,
            Force::SpawnFails => {}
        }
        // An unknown count is treated as one CPU: the inline path is always correct.
        std::thread::available_parallelism().is_ok_and(|n| n.get() > 1)
    }

    /// Whether a file's hashing runs on a worker: only under Normal durability (ruling R15; under Strict the fsyncs
    /// mask the hashing and the worker would only cost CPU), and not on a single CPU.
    pub(super) fn wanted(durability: flux_fs::Durability) -> bool {
        #[cfg(test)]
        match FORCE.get() {
            Force::Worker => return true,
            Force::OneCpu => return false,
            // `Pending` is selected and the spawn then fails: the fallback is exercised for either durability.
            Force::SpawnFails => return Worker::available(),
            Force::Auto => {}
        }
        durability == flux_fs::Durability::Normal && Worker::available()
    }

    pub(super) fn spawn<W: Send + 'static>(args: WorkerArgs<W>) -> io::Result<Worker> {
        #[cfg(test)]
        if FORCE.get() == Force::SpawnFails {
            return Err(io::Error::other("injected worker spawn failure"));
        }
        let (submit_tx, submit_rx) = mpsc::sync_channel::<Job>(POOL);
        let (free_tx, free_rx) = mpsc::channel::<Vec<u8>>();
        for b in args.extra_buffers {
            // The receiver is alive: it is ours.
            let _ = free_tx.send(b);
        }
        let (result_tx, result_rx) = mpsc::sync_channel::<Result<WorkerEnd, WalFault>>(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let faulted = Arc::new(AtomicBool::new(false));
        let fault = Arc::new(Mutex::new(None));
        let state = State {
            link: args.link,
            file_id: args.file_id,
            hasher: args.hasher,
            digests: args.digests,
            range_start: args.range_start,
            range_chunks: args.range_chunks.max(1),
            cancel: Arc::clone(&cancel),
            #[cfg(test)]
            inject: args.inject,
            #[cfg(test)]
            sent: 0,
        };
        let faulted_w = Arc::clone(&faulted);
        let fault_w = Arc::clone(&fault);
        #[cfg(test)]
        let live = LiveGuard::new();
        let handle = std::thread::Builder::new().name("flux-hash".to_owned()).spawn(move || {
            #[cfg(test)]
            let _live = live;
            let result = match state.run(&submit_rx, &free_tx) {
                Exit::End(end) => Ok(end),
                Exit::Fault(f) => {
                    *fault_w.lock().unwrap_or_else(|p| p.into_inner()) = Some(f.clone());
                    faulted_w.store(true, Ordering::SeqCst);
                    Err(f)
                }
                // Nobody waits for a result; the disconnect tells `cancel_and_join` the worker is done.
                Exit::Cancelled => return,
            };
            // The last act. The capacity is 1 and this is the only send, so it never blocks.
            let _ = result_tx.send(result);
        })?;
        #[cfg(test)]
        SPAWNED.set(SPAWNED.get() + 1);
        Ok(Worker {
            submit: Some(submit_tx),
            free: free_rx,
            result: result_rx,
            cancel,
            faulted,
            fault,
            handle: Some(handle),
        })
    }

    /// The fault the worker recorded, if any.
    pub(super) fn fault(&self) -> Option<WalFault> {
        if !self.faulted.load(Ordering::SeqCst) {
            return None;
        }
        self.fault.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Hands `buf` to the worker; its first `n` bytes are the data.
    pub(super) fn submit(&self, buf: Vec<u8>, n: usize) -> Result<(), Gone> {
        debug_assert!(n <= buf.len(), "n is within the buffer");
        self.submit.as_ref().ok_or(Gone)?.send(Job { buf, n }).map_err(|_| Gone)?;
        #[cfg(test)]
        SUBMITTED.set(SUBMITTED.get() + 1);
        Ok(())
    }

    /// A buffer the worker is done with, at its full length.
    pub(super) fn recv_free(&self, timeout: Duration) -> Result<Vec<u8>, RecvTimeoutError> {
        self.free.recv_timeout(timeout)
    }

    /// Closes the submit channel, lets the worker drain every queued job, and returns its final state. Waits in
    /// `SEND_RETRY` steps, beating on each.
    pub(super) fn finish(mut self, beat: &Heartbeat<'_>) -> Result<WorkerEnd, WorkerFail> {
        self.submit = None;
        loop {
            match self.result.recv_timeout(SEND_RETRY) {
                Ok(Ok(end)) => {
                    self.reap();
                    return Ok(end);
                }
                Ok(Err(f)) => {
                    self.reap();
                    return Err(WorkerFail::Fault(f));
                }
                // Returning drops `self`, whose `Drop` cancels and joins the worker.
                Err(RecvTimeoutError::Timeout) => beat().map_err(WorkerFail::Heartbeat)?,
                Err(RecvTimeoutError::Disconnected) => {
                    self.reap();
                    return Err(WorkerFail::Gone);
                }
            }
        }
    }

    /// Stops the worker at its next check (between jobs, or between send retries), closes the submit channel and
    /// joins it. Beats while waiting when given a heartbeat; its errors are ignored. A no-op once reaped.
    pub(super) fn cancel_and_join(&mut self, beat: Option<&Heartbeat<'_>>) {
        if self.handle.is_none() {
            return;
        }
        self.cancel.store(true, Ordering::SeqCst);
        self.submit = None;
        loop {
            match self.result.recv_timeout(SEND_RETRY) {
                Ok(_) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(beat) = beat {
                        let _ = beat();
                    }
                }
            }
        }
        self.reap();
    }

    /// Joins the thread. A panic in it was already reported as `Gone` (its result sender dropped on unwind), so the
    /// join's `Err` is dropped, never resumed.
    fn reap(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }

    #[cfg(test)]
    pub(super) fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    #[cfg(test)]
    pub(super) fn spawned_for_tests() -> usize {
        SPAWNED.get()
    }

    #[cfg(test)]
    pub(super) fn live_for_tests() -> usize {
        let me = std::thread::current().id();
        LIVE.lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|m| m.get(&me).copied())
            .unwrap_or(0)
    }

    /// Jobs handed over by this thread (a thread-local, like `spawned_for_tests`).
    #[cfg(test)]
    pub(super) fn submitted_for_tests() -> usize {
        SUBMITTED.get()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel_and_join(None);
    }
}

/// Why a send stopped.
enum Stop {
    Cancelled,
    Fault(WalFault),
}

/// The thread's state: today's `FileSink` hashing state plus the link.
struct State<W> {
    link: WalLink<W>,
    file_id: Vec<u8>,
    hasher: ChunkHasher,
    digests: Vec<Digest>,
    range_start: u64,
    range_chunks: usize,
    cancel: Arc<AtomicBool>,
    #[cfg(test)]
    inject: Inject,
    /// `Range` messages sent so far.
    #[cfg(test)]
    sent: usize,
}

impl<W> State<W> {
    fn run(mut self, jobs: &Receiver<Job>, free: &mpsc::Sender<Vec<u8>>) -> Exit {
        #[cfg(test)]
        let mut job_count = 0usize;
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Exit::Cancelled;
            }
            let Ok(job) = jobs.recv() else { break };
            #[cfg(test)]
            {
                job_count += 1;
                if self.inject.panic_on_job == Some(job_count) {
                    panic!("injected worker panic on job {job_count}");
                }
            }
            let done = self.hasher.update(&job.buf[..job.n]);
            // The bytes are consumed: the buffer goes back before any send can block. The main thread may be gone.
            let _ = free.send(job.buf);
            for (index, digest) in done {
                self.digests.push(digest);
                if self.digests.len() >= self.range_chunks {
                    let end = (index + 1) * CHUNK_SIZE;
                    let range = Range {
                        file_id: self.file_id.clone(),
                        attempt_id: ATTEMPT_ID,
                        range_start: self.range_start,
                        range_end: end,
                        digests: std::mem::take(&mut self.digests),
                    };
                    self.range_start = end;
                    match self.send(range) {
                        Ok(()) => {}
                        Err(Stop::Cancelled) => return Exit::Cancelled,
                        Err(Stop::Fault(f)) => return Exit::Fault(f),
                    }
                }
                // A dead log keeps its thread alive and sends keep succeeding, so ask it after every chunk.
                if let Some(f) = self.link.fault() {
                    return Exit::Fault(f);
                }
            }
        }
        Exit::End(WorkerEnd {
            hasher: self.hasher,
            digests: self.digests,
            range_start: self.range_start,
        })
    }

    /// `try_send` until it lands, retrying every `SEND_RETRY` on a full channel; gives up on cancel, a log fault, or
    /// a disconnected channel.
    fn send(&mut self, range: Range) -> Result<(), Stop> {
        #[cfg(test)]
        if self.inject.fail_send_after_ranges.is_some_and(|k| self.sent >= k) {
            return Err(Stop::Fault(WalFault::new(
                Code::WalWriteFailed,
                io::ErrorKind::BrokenPipe,
                "injected worker send failure",
            )));
        }
        let mut msg = Msg::Range(range);
        loop {
            // Before EVERY attempt, the first and each one after a retry sleep: nothing is sent after a cancel.
            if self.cancel.load(Ordering::SeqCst) {
                return Err(Stop::Cancelled);
            }
            match self.link.try_send(msg) {
                Ok(()) => {
                    #[cfg(test)]
                    {
                        self.sent += 1;
                    }
                    return Ok(());
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(Stop::Fault(self.link.fault().unwrap_or_else(|| {
                        WalFault::new(
                            Code::WalWriteFailed,
                            io::ErrorKind::BrokenPipe,
                            "the WAL writer thread is gone",
                        )
                    })));
                }
                Err(TrySendError::Full(back)) => {
                    msg = back;
                    if let Some(f) = self.link.fault() {
                        return Err(Stop::Fault(f));
                    }
                    std::thread::sleep(SEND_RETRY);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::{CheckpointPolicy, Checkpoints};
    use crate::fault_fs::{FakeDirHandle, FakeHandle, FaultFs};
    use crate::wal::record::{Body, Decoded, FileBegin, decode_next};
    use crate::wal::writer::WalConfig;
    use flux_fs::{Code, DestinationRoot, Durability, FileHandle, FileSystem};
    use std::collections::HashSet;
    use std::path::Path;
    use std::time::Instant;

    const MIB: usize = 1 << 20;
    const PIECE: usize = MIB / 4;
    const WAL_PATH: &str = "/ws/wal/000001";
    const HOUR: Duration = Duration::from_secs(3600);
    /// A generous bound for anything that should happen at once; a hang fails the test instead of the suite.
    const DEADLINE: Duration = Duration::from_secs(10);
    /// How long each `sync_all` is held in test 4: far longer than a cancel takes, so the oracle is the writer's state.
    const HOLD: Duration = Duration::from_secs(1);

    // `ws`, `policy` and `open_run` duplicate the sink tests' helpers (ruling R2).
    fn ws() -> (FaultFs, FakeDirHandle) {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/ws")).unwrap();
        let dir = fs.destination_root(Path::new("/ws")).unwrap();
        (fs, dir)
    }

    fn policy(range_chunks: usize, durability: Durability) -> CheckpointPolicy {
        CheckpointPolicy {
            min_len: 1 << 20,
            range_chunks,
            wal: WalConfig {
                flush_interval: HOUR,
                flush_bytes: 1 << 50,
                channel_cap: 64,
                durability,
            },
            verify: crate::checkpoint::ResumeVerify::Chunks,
        }
    }

    fn open_run(fs: &FaultFs, p: CheckpointPolicy) -> Checkpoints<FakeDirHandle> {
        let a = fs.destination_root(Path::new("/ws")).unwrap();
        let b = fs.destination_root(Path::new("/ws")).unwrap();
        Checkpoints::open(a, b, "op", 1, p).unwrap_or_else(|e| panic!("open: {e:?}"))
    }

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// Registers file `f` with the log, as `FileSink::begin` does before any worker exists.
    fn begin(run: &Checkpoints<FakeDirHandle>, dup: Option<FakeHandle>, beat: &Heartbeat<'_>) {
        let begin = FileBegin {
            file_id: b"f".to_vec(),
            attempt_id: ATTEMPT_ID,
            destination: b"/ws/f".to_vec(),
            source_len: 8 * CHUNK_SIZE,
            source_mtime: None,
            source_identity: Vec::new(),
            chunk_size: CHUNK_SIZE,
            valid_through: 0,
        };
        run.sender.send(Msg::FileBegin { begin, dup }, beat).unwrap();
    }

    fn drain(run: &Checkpoints<FakeDirHandle>, beat: &Heartbeat<'_>) {
        let (tx, rx) = mpsc::sync_channel(1);
        run.sender.send(Msg::Drain(tx), beat).unwrap();
        rx.recv().unwrap();
    }

    fn args(
        run: &Checkpoints<FakeDirHandle>,
        range_chunks: usize,
        extra_buffers: Vec<Vec<u8>>,
    ) -> WorkerArgs<FakeHandle> {
        WorkerArgs {
            link: run.sender.link(),
            file_id: b"f".to_vec(),
            hasher: ChunkHasher::new(),
            digests: Vec::new(),
            range_start: 0,
            range_chunks,
            extra_buffers,
            inject: Inject::default(),
        }
    }

    fn extras(len: usize) -> Vec<Vec<u8>> {
        (0..POOL - 1).map(|_| vec![0u8; len]).collect()
    }

    fn ranges(fs: &FaultFs) -> Vec<Range> {
        let bytes = fs.read_file(WAL_PATH).expect("the segment exists");
        let mut out = Vec::new();
        let mut at = 0;
        loop {
            match decode_next(&bytes[at..]) {
                Decoded::Record { body, len, .. } => {
                    if let Body::Range(r) = body {
                        out.push(r);
                    }
                    at += len;
                }
                Decoded::End => return out,
                other => panic!("unexpected {other:?} at {at}"),
            }
        }
    }

    fn wait_for<T>(what: &str, f: impl Fn() -> Option<T>) -> T {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn the_worker_digests_equal_the_inline_hasher() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(4, Durability::Normal));
        begin(&run, None, beat);
        let total = 5 * MIB + 7;
        let src = data(total);
        let w = Worker::spawn(args(&run, 4, extras(PIECE))).unwrap();
        let mut buf = vec![0u8; PIECE];
        for piece in src.chunks(PIECE) {
            buf[..piece.len()].copy_from_slice(piece);
            w.submit(buf, piece.len()).unwrap();
            buf = w.recv_free(DEADLINE).unwrap();
        }
        let end = match w.finish(beat) {
            Ok(end) => end,
            Err(e) => panic!("finish: {e:?}"),
        };
        assert!(run.finish(beat).is_clean());
        let logged = ranges(&fs);
        let m = MIB as u64;
        let shape: Vec<(u64, u64, usize)> =
            logged.iter().map(|r| (r.range_start, r.range_end, r.digests.len())).collect();
        assert_eq!(shape, [(0, 4 * m, 4)]);
        assert!(logged.iter().all(|r| r.file_id == b"f" && r.attempt_id == 1));
        assert_eq!(end.range_start, 4 * m);
        assert_eq!(end.hasher.bytes_seen(), total as u64);
        let mut got: Vec<Digest> = logged.into_iter().flat_map(|r| r.digests).collect();
        got.extend(end.digests);
        got.extend(end.hasher.finish().map(|(_, d)| d));
        let mut inline = ChunkHasher::new();
        let mut want: Vec<Digest> = inline.update(&src).into_iter().map(|(_, d)| d).collect();
        want.extend(inline.finish().map(|(_, d)| d));
        assert_eq!(want.len(), 6);
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g, w, "digest {i} differs from the inline hasher");
            let slice = &src[i * MIB..((i + 1) * MIB).min(total)];
            assert_eq!(g, blake3::hash(slice).as_bytes(), "digest {i} is not BLAKE3 of its slice");
        }
    }

    #[test]
    fn buffers_are_recycled_and_the_pool_is_bounded() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(4, Durability::Normal));
        begin(&run, None, beat);
        let extra = extras(PIECE);
        let mut buf = vec![0u8; PIECE];
        let pool: HashSet<usize> =
            extra.iter().chain([&buf]).map(|b| b.as_ptr() as usize).collect();
        assert_eq!(pool.len(), POOL);
        let w = Worker::spawn(args(&run, 4, extra)).unwrap();
        let mut seen = HashSet::new();
        let mut fed = 0u64;
        for i in 0..20 {
            buf.fill(i as u8);
            // Short fills too: a buffer whose first `n` bytes are valid must come back at its full length.
            let n = PIECE - i;
            w.submit(buf, n).unwrap();
            fed += n as u64;
            buf = w.recv_free(DEADLINE).unwrap();
            assert_eq!(buf.len(), PIECE, "a returned buffer keeps its full length");
            seen.insert(buf.as_ptr() as usize);
        }
        assert!(seen.len() <= POOL, "{} distinct buffers seen", seen.len());
        assert!(seen.is_subset(&pool), "every buffer seen is one of the pool's");
        let end = match w.finish(beat) {
            Ok(end) => end,
            Err(e) => panic!("finish: {e:?}"),
        };
        assert_eq!(end.hasher.bytes_seen(), fed);
        assert!(run.finish(beat).is_clean());
    }

    #[test]
    fn a_log_fault_sets_the_fault_and_the_worker_exits() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let run = open_run(&fs, policy(4, Durability::Normal));
        begin(&run, Some(p.duplicate().unwrap()), beat);
        fs.fail("create_new", Code::DiskFull);
        // The FileBegin is flushed by the drain; the segment create fails, killing the log.
        drain(&run, beat);
        assert!(run.sender.wal_fault().is_some());
        let w = Worker::spawn(args(&run, 4, extras(MIB))).unwrap();
        assert!(w.fault().is_none(), "no job yet, so no check yet");
        // One chunk, fewer than a range (4): the worker sends nothing, so only its per-chunk check sees the fault.
        w.submit(vec![7u8; MIB], MIB).unwrap();
        let f = wait_for("the worker's fault", || w.fault());
        assert_eq!(f.code, Code::DiskFull);
        // The worker exits: once the extras and the job's buffer are back, the free channel disconnects.
        let mut back = 0;
        loop {
            match w.recv_free(DEADLINE) {
                Ok(_) => back += 1,
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("the worker did not exit"),
            }
        }
        assert_eq!(back, POOL, "the three extras and the job's buffer");
        match w.finish(beat) {
            Err(WorkerFail::Fault(f)) => assert_eq!(f.code, Code::DiskFull),
            Ok(_) => panic!("expected Fault, got Ok"),
            Err(e) => panic!("expected Fault, got {e:?}"),
        }
        run.abandon();
    }

    /// A run whose writer is held: Strict, every `sync_all` delayed by `HOLD`, a one-slot channel, and a `Flush` the
    /// writer is stuck in (the duplicate's `sync_all` comes first, so the segment does not exist until the hold ends).
    /// The returned handle is the partial, kept open for the duplicate's sake.
    fn held_run(fs: &FaultFs, beat: &Heartbeat<'_>) -> (Checkpoints<FakeDirHandle>, FakeHandle) {
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        fs.delay("sync_all", HOLD);
        let mut pol = policy(1, Durability::Strict);
        pol.wal.channel_cap = 1;
        let run = open_run(fs, pol);
        begin(&run, Some(p.duplicate().unwrap()), beat);
        run.sender.send(Msg::Flush, beat).unwrap();
        (run, p)
    }

    /// Feeds a `range_chunks = 1` worker on a held run until it is in its full-channel retry loop: the first Range
    /// fills the one slot, the second finds it full.
    fn block_on_a_full_channel(w: &Worker) {
        let mut buf = vec![0u8; MIB];
        for _ in 0..3 {
            w.submit(buf, MIB).unwrap();
            buf = w.recv_free(DEADLINE).unwrap();
        }
        // Two more buffers back (the first two jobs' own): the second job is hashed, its Range is being sent.
        for _ in 0..2 {
            w.recv_free(DEADLINE).unwrap();
        }
    }

    #[test]
    fn cancel_stops_a_worker_blocked_on_a_full_channel() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (run, _p) = held_run(&fs, beat);
        let live = Worker::live_for_tests();
        let mut w = Worker::spawn(args(&run, 1, extras(MIB))).unwrap();
        block_on_a_full_channel(&w);
        let t = Instant::now();
        w.cancel_and_join(Some(beat));
        let took = t.elapsed();
        assert!(
            fs.read_file(WAL_PATH).is_none(),
            "cancel_and_join returned only after the writer was released (took {took:?})"
        );
        assert!(took < HOLD, "cancel_and_join took {took:?}");
        assert!(w.is_finished());
        assert_eq!(Worker::live_for_tests(), live);
        drop(w);
        run.abandon();
    }

    #[test]
    fn a_panicking_worker_reports_gone() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(4, Durability::Normal));
        begin(&run, None, beat);
        let mut a = args(&run, 4, extras(PIECE));
        a.inject = Inject { panic_on_job: Some(1), ..Inject::default() };
        let w = Worker::spawn(a).unwrap();
        w.submit(vec![0u8; PIECE], PIECE).unwrap();
        match w.finish(beat) {
            Err(WorkerFail::Gone) => {}
            Ok(_) => panic!("expected Gone, got Ok"),
            Err(e) => panic!("expected Gone, got {e:?}"),
        }
        run.abandon();
    }

    #[test]
    fn dropping_a_worker_joins_its_thread() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (run, _p) = held_run(&fs, beat);
        let start = Worker::live_for_tests();
        let spawned = Worker::spawned_for_tests();
        let w = Worker::spawn(args(&run, 1, extras(MIB))).unwrap();
        assert_eq!(Worker::spawned_for_tests(), spawned + 1);
        assert_eq!(Worker::live_for_tests(), start + 1);
        // Busy: blocked on a full channel with a Range pending, so closing its submit channel alone cannot end it
        // before the writer is released; only the cancel flag can.
        block_on_a_full_channel(&w);
        drop(w);
        // No polling: `drop` returns only after the thread is joined, and the live guard drops inside the thread.
        assert_eq!(Worker::live_for_tests(), start, "drop returned with the worker thread alive");
        assert!(
            fs.read_file(WAL_PATH).is_none(),
            "drop waited for the writer's release instead of cancelling the worker"
        );
        run.abandon();
    }

    #[test]
    fn a_cancelled_worker_sends_no_range() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(4, Durability::Normal));
        begin(&run, None, beat);
        // The thread body is driven on this thread, so the cancel lands at a known point: a spawned worker may already
        // be past its loop-top check, blocked in `recv`, when a test sets the flag.
        let cancel = Arc::new(AtomicBool::new(true));
        let state = || State {
            link: run.sender.link(),
            file_id: b"f".to_vec(),
            hasher: ChunkHasher::new(),
            digests: Vec::new(),
            range_start: 0,
            range_chunks: 4,
            cancel: Arc::clone(&cancel),
            inject: Inject::default(),
            sent: 0,
        };
        // Cancelled between jobs, with a queued job that completes a whole Range (4 chunks): the body takes no job,
        // so the job's buffer never comes back.
        let (jobs_tx, jobs_rx) = mpsc::sync_channel(POOL);
        let (free_tx, free_rx) = mpsc::channel();
        jobs_tx.send(Job { buf: data(4 * MIB), n: 4 * MIB }).unwrap();
        drop(jobs_tx);
        assert!(matches!(state().run(&jobs_rx, &free_tx), Exit::Cancelled));
        assert!(free_rx.try_recv().is_err(), "the cancelled worker took a job");
        // Cancelled with a Range built (a later chunk of the same job, or an attempt after a retry sleep), the
        // channel NOT full: nothing is sent.
        let mut s = state();
        let range = Range {
            file_id: b"f".to_vec(),
            attempt_id: ATTEMPT_ID,
            range_start: 0,
            range_end: CHUNK_SIZE,
            digests: vec![[0u8; 32]],
        };
        assert!(matches!(s.send(range), Err(Stop::Cancelled)), "a Range was sent after the cancel");
        assert_eq!(s.sent, 0);
        drop(s);
        assert!(run.finish(beat).is_clean());
        assert!(ranges(&fs).is_empty(), "no Range reached the log");
    }
}
