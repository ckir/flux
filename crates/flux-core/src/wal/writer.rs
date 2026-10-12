//! The WAL writer thread (cut 9f-1): a bounded channel, a flush policy, and the Strict
//! data-before-checkpoint ordering.
//!
//! Every record the copy loop wants logged is sent as a `Msg`. One thread owns the log segment and
//! every registered duplicate of a partial file. A flush syncs each file's duplicate FIRST (Strict),
//! then appends that file's records, then syncs the log, so a checkpoint is never durable before the
//! data it describes.

use super::record::{Body, FileBegin, Header, Range, encode};
use super::replay::{Scan, WalError, scan};
use crate::copy::Heartbeat;
use flux_fs::{Code, DirHandle, Durability, FileHandle, FileIdentity, FsError};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Directory of the log inside the workspace.
pub(crate) const DIR_NAME: &str = "wal";
/// The one segment of the log.
pub(crate) const SEGMENT_NAME: &str = "000001";
/// How long a sender waits between retries on a full channel.
pub(crate) const SEND_RETRY: Duration = Duration::from_millis(10);
/// Channel capacity of the production configuration.
pub(crate) const CHANNEL_CAP: usize = 64;
/// Most digests one record may carry: 32,000 * 32 bytes plus framing stays under `MAX_PAYLOAD`.
const RANGE_DIGESTS_MAX: usize = 32_000;

#[derive(Debug, Clone, Copy)]
pub(crate) struct WalConfig {
    pub flush_interval: Duration,
    pub flush_bytes: u64,
    pub channel_cap: usize,
    pub durability: Durability,
}

impl WalConfig {
    pub(crate) const DEFAULT: WalConfig = WalConfig {
        flush_interval: Duration::from_secs(1),
        flush_bytes: 64 << 20,
        channel_cap: CHANNEL_CAP,
        durability: Durability::Normal,
    };
}

/// What a failure looked like, kept as a record because `FsError` is not `Clone` (ruling 21).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WalFault {
    pub code: Code,
    pub kind: io::ErrorKind,
    pub raw_os_error: Option<i32>,
    pub message: String,
}

impl WalFault {
    pub(crate) fn from_error(e: &FsError) -> WalFault {
        WalFault {
            code: e.code,
            kind: e.source.kind(),
            raw_os_error: e.source.raw_os_error(),
            message: e.source.to_string(),
        }
    }

    pub(crate) fn new(code: Code, kind: io::ErrorKind, message: &str) -> WalFault {
        WalFault { code, kind, raw_os_error: None, message: message.to_owned() }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Failure {
    /// The first WAL error; later ones are not recorded.
    pub wal: Option<WalFault>,
    /// Per file, the first data-sync (or no-duplicate, or oversized-range) error.
    pub files: HashMap<Vec<u8>, WalFault>,
}

impl Failure {
    pub(crate) fn is_clean(&self) -> bool {
        self.wal.is_none() && self.files.is_empty()
    }
}

pub(crate) enum Msg<W> {
    FileBegin { begin: FileBegin, dup: Option<W> },
    Range(Range),
    Flush,
    Close { file_id: Vec<u8>, ack: Option<mpsc::SyncSender<Option<WalFault>>> },
    Drain(mpsc::SyncSender<Failure>),
    Stop,
}

#[derive(Debug)]
pub(crate) enum SendError {
    /// The writer thread is gone.
    WalFailed,
    /// The heartbeat failed while the channel was full; the error is the heartbeat's, unchanged.
    Heartbeat(FsError),
}

fn lock(m: &Mutex<Failure>) -> MutexGuard<'_, Failure> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) struct WalSender<W> {
    tx: SyncSender<Msg<W>>,
    failure: Arc<Mutex<Failure>>,
}

impl<W> WalSender<W> {
    /// Queue `msg`. A full channel is waited out in `SEND_RETRY` steps, calling `beat` before each wait so a
    /// lock-holding run keeps refreshing its lock (ruling 12).
    pub(crate) fn send(&self, msg: Msg<W>, beat: &Heartbeat<'_>) -> Result<(), SendError> {
        let mut msg = msg;
        loop {
            match self.tx.try_send(msg) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => return Err(SendError::WalFailed),
                Err(TrySendError::Full(back)) => {
                    msg = back;
                    beat().map_err(SendError::Heartbeat)?;
                    std::thread::sleep(SEND_RETRY);
                }
            }
        }
    }

    pub(crate) fn failure(&self) -> Failure {
        lock(&self.failure).clone()
    }
}

pub(crate) struct WalThread {
    handle: JoinHandle<()>,
    failure: Arc<Mutex<Failure>>,
}

impl WalThread {
    /// Wait for the thread and return the final failure state (after `Stop`'s flush).
    pub(crate) fn join(self) -> Failure {
        if let Err(panic) = self.handle.join() {
            std::panic::resume_unwind(panic);
        }
        lock(&self.failure).clone()
    }
}

fn io_err(code: Code, kind: io::ErrorKind, why: &str) -> WalError {
    WalError::Io(FsError::new(code, io::Error::new(kind, why)))
}

fn is_not_found(e: &FsError) -> bool {
    e.source.kind() == io::ErrorKind::NotFound
}

/// What `open` returns: the sender, the thread, and the scan of a segment that already existed.
pub(crate) type Opened<W> = (WalSender<W>, WalThread, Option<Scan>);

/// Open the writer for `workspace`'s log, resuming an existing segment (scan, truncate a torn tail, continue
/// the sequence) or leaving it uncreated until the first flush with a record.
pub(crate) fn open<D: DirHandle + Send + 'static>(
    workspace: D,
    operation_id: Vec<u8>,
    generation: u64,
    cfg: WalConfig,
) -> Result<Opened<D::Writer>, WalError> {
    let mut found: Option<Scan> = None;
    let mut wal: Option<D::Writer> = None;
    let mut next_seq = 1u64;
    let dir = match workspace.open_dir(OsStr::new(DIR_NAME)) {
        Ok(d) => Some(d),
        Err(e) if is_not_found(&e) => None,
        Err(e) => return Err(WalError::Io(e)),
    };
    if let Some(dir) = dir {
        let meta = match dir.metadata(OsStr::new(SEGMENT_NAME)) {
            Ok(m) => Some(m),
            Err(e) if is_not_found(&e) => None,
            Err(e) => return Err(WalError::Io(e)),
        };
        if let Some(meta) = meta {
            if meta.identity == FileIdentity::Unavailable {
                return Err(io_err(
                    Code::DestinationError,
                    io::ErrorKind::Unsupported,
                    "the WAL segment has no usable identity, so it cannot be reopened safely",
                ));
            }
            let reader = dir.open_read(OsStr::new(SEGMENT_NAME)).map_err(WalError::Io)?;
            let scanned = scan(reader, &operation_id)?;
            next_seq = match scanned.last {
                None => 1,
                Some((g, s)) if g == generation => s + 1,
                Some((g, _)) if generation > g => 1,
                Some(_) => {
                    return Err(WalError::SequenceConflict { generation, sequence: 1 });
                }
            };
            let mut writer = dir
                .open_existing(OsStr::new(SEGMENT_NAME), &meta.identity)
                .map_err(WalError::Io)?;
            writer.truncate_to(scanned.valid_len).map_err(WalError::Io)?;
            wal = Some(writer);
            found = Some(scanned);
        }
    }
    let (tx, rx) = mpsc::sync_channel(cfg.channel_cap);
    let failure = Arc::new(Mutex::new(Failure::default()));
    let state = State {
        workspace,
        wal,
        operation_id,
        generation,
        next_seq,
        cfg,
        dups: HashMap::new(),
        pending: Vec::new(),
        pending_bytes: 0,
        first_pending_at: None,
        failure: Arc::clone(&failure),
        dead: false,
    };
    let handle = std::thread::Builder::new()
        .name("flux-wal".to_owned())
        .spawn(move || state.run(rx))
        .map_err(|e| WalError::Io(FsError::from_io(e)))?;
    Ok((WalSender { tx, failure: Arc::clone(&failure) }, WalThread { handle, failure }, found))
}

struct State<D: DirHandle> {
    workspace: D,
    wal: Option<D::Writer>,
    operation_id: Vec<u8>,
    generation: u64,
    next_seq: u64,
    cfg: WalConfig,
    dups: HashMap<Vec<u8>, D::Writer>,
    pending: Vec<(Vec<u8>, Body)>,
    pending_bytes: u64,
    first_pending_at: Option<Instant>,
    failure: Arc<Mutex<Failure>>,
    dead: bool,
}

impl<D: DirHandle> State<D> {
    fn run(mut self, rx: Receiver<Msg<D::Writer>>) {
        loop {
            let msg = match self.first_pending_at {
                None => match rx.recv() {
                    Ok(m) => m,
                    Err(_) => return self.abandon(),
                },
                Some(first) => {
                    let wait =
                        (first + self.cfg.flush_interval).saturating_duration_since(Instant::now());
                    match rx.recv_timeout(wait) {
                        Ok(m) => m,
                        Err(RecvTimeoutError::Timeout) => {
                            self.flush();
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => return self.abandon(),
                    }
                }
            };
            match msg {
                Msg::FileBegin { begin, dup } => {
                    if self.dead {
                        continue;
                    }
                    if let Some(d) = dup {
                        self.dups.insert(begin.file_id.clone(), d);
                    }
                    self.push(begin.file_id.clone(), Body::FileBegin(begin), 0);
                }
                Msg::Range(r) => {
                    if self.dead {
                        continue;
                    }
                    let bytes = r.range_end.saturating_sub(r.range_start);
                    self.push(r.file_id.clone(), Body::Range(r), bytes);
                }
                Msg::Flush => self.flush(),
                Msg::Close { file_id, ack } => {
                    // File completion is a permitted early flush; the duplicate outlives every pending record.
                    if self.pending.iter().any(|(id, _)| *id == file_id) {
                        self.flush();
                    }
                    self.dups.remove(&file_id);
                    if let Some(ack) = ack {
                        let answer = {
                            let g = lock(&self.failure);
                            g.files.get(&file_id).cloned().or_else(|| g.wal.clone())
                        };
                        let _ = ack.send(answer);
                    }
                }
                Msg::Drain(ack) => {
                    self.flush();
                    let snapshot = lock(&self.failure).clone();
                    let _ = ack.send(snapshot);
                }
                Msg::Stop => {
                    self.flush();
                    self.dups.clear();
                    return;
                }
            }
            let timer_due = self
                .first_pending_at
                .is_some_and(|first| first.elapsed() >= self.cfg.flush_interval);
            if self.pending_bytes >= self.cfg.flush_bytes || timer_due {
                self.flush();
            }
        }
    }

    /// The sender vanished without `Stop`: the run is being abandoned, so nothing pending is made durable (ruling 11).
    fn abandon(&mut self) {
        self.pending.clear();
        self.dups.clear();
    }

    fn push(&mut self, file_id: Vec<u8>, body: Body, bytes: u64) {
        if self.pending.is_empty() {
            self.first_pending_at = Some(Instant::now());
        }
        self.pending.push((file_id, body));
        self.pending_bytes = self.pending_bytes.saturating_add(bytes);
    }

    fn fail_file(&self, file_id: &[u8], fault: WalFault) {
        lock(&self.failure).files.entry(file_id.to_vec()).or_insert(fault);
    }

    /// The log is dead: record the first error and release every duplicate at once (an open duplicate would block
    /// a rename on Windows, and no sync through it is ever needed again).
    fn fail_wal(&mut self, fault: WalFault) {
        lock(&self.failure).wal.get_or_insert(fault);
        self.dead = true;
        self.dups.clear();
        self.wal = None;
        self.pending.clear();
        self.pending_bytes = 0;
        self.first_pending_at = None;
    }

    fn ensure_wal(&mut self) -> Result<(), FsError> {
        if self.wal.is_some() {
            return Ok(());
        }
        let dir = match self.workspace.create_dir(OsStr::new(DIR_NAME)) {
            Ok(d) => d,
            Err(e) if e.source.kind() == io::ErrorKind::AlreadyExists => {
                self.workspace.open_dir(OsStr::new(DIR_NAME))?
            }
            Err(e) => return Err(e),
        };
        self.wal = Some(dir.create_new(OsStr::new(SEGMENT_NAME))?);
        Ok(())
    }

    fn flush(&mut self) {
        if self.dead {
            self.pending.clear();
            self.pending_bytes = 0;
            self.first_pending_at = None;
            return;
        }
        if self.pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending);
        self.pending_bytes = 0;
        self.first_pending_at = None;

        let mut order: Vec<&[u8]> = Vec::new();
        let mut seen: HashSet<&[u8]> = HashSet::new();
        let mut oversized: HashSet<&[u8]> = HashSet::new();
        for (id, body) in &pending {
            if seen.insert(id.as_slice()) {
                order.push(id.as_slice());
            }
            if let Body::Range(r) = body
                && r.digests.len() > RANGE_DIGESTS_MAX
            {
                oversized.insert(id.as_slice());
            }
        }
        // Files whose records this flush must not append, and why.
        let mut bad: HashSet<Vec<u8>> = HashSet::new();
        for id in order {
            if lock(&self.failure).files.contains_key(id) {
                // A file whose data sync already failed never gets a checkpoint later: a retried sync that
                // succeeds proves nothing about the pages the failed one lost.
                bad.insert(id.to_vec());
            } else if oversized.contains(id) {
                self.fail_file(
                    id,
                    WalFault::new(
                        Code::DestinationError,
                        io::ErrorKind::InvalidInput,
                        "range too large for one record",
                    ),
                );
                bad.insert(id.to_vec());
            } else if self.cfg.durability == Durability::Strict {
                // Data before checkpoint: the duplicate is synced BEFORE any record of this file is appended.
                let outcome = match self.dups.get(id) {
                    None => Some(WalFault::new(
                        Code::DestinationError,
                        io::ErrorKind::InvalidInput,
                        "no duplicate registered",
                    )),
                    Some(dup) => dup.sync_all().err().map(|e| WalFault::from_error(&e)),
                };
                if let Some(fault) = outcome {
                    self.fail_file(id, fault);
                    bad.insert(id.to_vec());
                }
            }
        }
        let keep: Vec<&(Vec<u8>, Body)> =
            pending.iter().filter(|(id, _)| !bad.contains(id)).collect();
        if keep.is_empty() {
            return;
        }
        if let Err(e) = self.ensure_wal() {
            self.fail_wal(WalFault::from_error(&e));
            return;
        }
        let mut buf = Vec::new();
        for (_, body) in keep {
            let header = Header {
                operation_id: self.operation_id.clone(),
                generation: self.generation,
                sequence: self.next_seq,
            };
            self.next_seq += 1;
            buf.extend_from_slice(&encode(&header, body));
        }
        let wal = self.wal.as_mut().expect("ensure_wal opened the segment");
        if let Err(e) = wal.write_all(&buf) {
            self.fail_wal(WalFault::from_error(&FsError::from_io(e)));
            return;
        }
        if self.cfg.durability == Durability::Strict
            && let Err(e) = wal.sync_all()
        {
            self.fail_wal(WalFault::from_error(&e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::{FakeDirHandle, FakeHandle, FaultFs};
    use crate::wal::CHUNK_SIZE;
    use crate::wal::record::{Decoded, decode_next};
    use flux_fs::{DestinationRoot, FileSystem};
    use std::cell::Cell;
    use std::path::Path;

    const WAL_PATH: &str = "/ws/wal/000001";
    const OP: &[u8] = b"op";
    const BIG: u64 = 1 << 50;
    const HOUR: Duration = Duration::from_secs(3600);

    type Sender = WalSender<FakeHandle>;

    fn ws() -> (FaultFs, FakeDirHandle) {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/ws")).unwrap();
        let dir = fs.destination_root(Path::new("/ws")).unwrap();
        (fs, dir)
    }

    fn cfg(bytes: u64, interval: Duration, durability: Durability) -> WalConfig {
        WalConfig { flush_interval: interval, flush_bytes: bytes, channel_cap: 64, durability }
    }

    fn begin(file: &[u8]) -> FileBegin {
        FileBegin {
            file_id: file.to_vec(),
            attempt_id: 1,
            destination: b"d".to_vec(),
            source_len: 4 * CHUNK_SIZE,
            source_mtime: None,
            source_identity: Vec::new(),
            chunk_size: CHUNK_SIZE,
            valid_through: 0,
        }
    }

    fn range(file: &[u8], start_mib: u64, end_mib: u64) -> Range {
        Range {
            file_id: file.to_vec(),
            attempt_id: 1,
            range_start: start_mib * CHUNK_SIZE,
            range_end: end_mib * CHUNK_SIZE,
            digests: vec![[0u8; 32]; (end_mib - start_mib) as usize],
        }
    }

    fn start(dir: FakeDirHandle, generation: u64, c: WalConfig) -> (Sender, WalThread) {
        let (s, t, _) = open(dir, OP.to_vec(), generation, c).expect("open");
        (s, t)
    }

    fn wal_bytes(fs: &FaultFs) -> Option<Vec<u8>> {
        fs.read_file(WAL_PATH)
    }

    fn records(bytes: &[u8]) -> Vec<(Header, Body)> {
        let mut out = Vec::new();
        let mut at = 0;
        loop {
            match decode_next(&bytes[at..]) {
                Decoded::Record { header, body, len, .. } => {
                    out.push((header, body));
                    at += len;
                }
                Decoded::End => return out,
                other => panic!("unexpected {other:?} at {at}"),
            }
        }
    }

    fn wait_for<T>(what: &str, f: impl Fn() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn drain(s: &Sender, beat: &Heartbeat<'_>) -> Failure {
        let (tx, rx) = mpsc::sync_channel(1);
        s.send(Msg::Drain(tx), beat).unwrap();
        rx.recv().unwrap()
    }

    fn syncs(fs: &FaultFs) -> Vec<String> {
        fs.calls().into_iter().filter(|c| c.starts_with("sync_all(")).collect()
    }

    fn pos(calls: &[String], entry: &str) -> usize {
        calls.iter().position(|c| c == entry).unwrap_or_else(|| panic!("{entry} not in {calls:?}"))
    }

    fn begin_msg(file: &[u8], dup: Option<FakeHandle>) -> Msg<FakeHandle> {
        Msg::FileBegin { begin: begin(file), dup }
    }

    #[test]
    fn nothing_is_created_until_the_first_flush_with_a_record() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        // The thread has certainly seen the begin once a Drain is answered... which flushes. Use the log instead:
        // nothing but the setup's own call may mention the WAL until a flush.
        assert!(wal_bytes(&fs).is_none());
        assert!(!fs.calls().iter().any(|c| c == "create_dir(/ws/wal)"));
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let r = records(&wal_bytes(&fs).unwrap());
        assert_eq!(r.len(), 1);
        assert!(matches!(r[0].1, Body::FileBegin(_)));
    }

    #[test]
    fn the_byte_threshold_flushes() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(2 * CHUNK_SIZE, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        s.send(Msg::Range(range(b"p", 1, 2)), beat).unwrap();
        let bytes =
            wait_for("the threshold flush", || wal_bytes(&fs).filter(|b| records(b).len() == 3));
        assert_eq!(records(&bytes).len(), 3);
        assert!(syncs(&fs).is_empty());
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn the_timer_flushes_from_the_first_pending_record() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, Duration::from_millis(50), Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        wait_for("the timer flush", || wal_bytes(&fs));
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn an_idle_timer_writes_nothing() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, Duration::from_millis(20), Durability::Normal));
        std::thread::sleep(Duration::from_millis(200));
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        assert!(wal_bytes(&fs).is_none());
        assert!(!fs.calls().iter().any(|c| c.starts_with("create_dir(/ws/wal")));
        assert!(syncs(&fs).is_empty());
    }

    #[test]
    fn strict_syncs_the_partial_then_appends_then_syncs_the_wal() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(CHUNK_SIZE, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        drain(&s, beat);
        assert_eq!(syncs(&fs), ["sync_all(/ws/p)", "sync_all(/ws/wal/000001)"]);
        // `FakeHandle::sync_all` does not pass through `FaultFs::record`, so `on_nth("sync_all", ..)` never fires.
        // The call log carries the same fact: the WAL's directory and segment are created only after the
        // partial's sync, so the WAL was absent when it ran.
        let calls = fs.calls();
        let partial = pos(&calls, "sync_all(/ws/p)");
        assert!(pos(&calls, "create_dir(/ws/wal)") > partial);
        assert!(pos(&calls, "create_new(/ws/wal/000001)") > partial);
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 2);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn normal_never_syncs() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        drain(&s, beat);
        assert!(syncs(&fs).is_empty());
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 2);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn a_failed_partial_sync_flags_the_file_and_appends_nothing_for_it() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let q = fs.create_new(Path::new("/ws/q")).unwrap();
        fs.fail("sync_all", Code::StrictDurabilityUnavailable);
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"q", Some(q.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"q", 0, 1)), beat).unwrap();
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        let f = drain(&s, beat);
        assert_eq!(f.files.keys().collect::<Vec<_>>(), [&b"p".to_vec()]);
        assert_eq!(f.files[&b"p".to_vec()].code, Code::StrictDurabilityUnavailable);
        assert!(f.wal.is_none());
        let r = records(&wal_bytes(&fs).unwrap());
        assert_eq!(r.len(), 2);
        for (_, body) in &r {
            let id = match body {
                Body::FileBegin(b) => &b.file_id,
                Body::Range(r) => &r.file_id,
            };
            assert_eq!(id, b"q");
        }
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn a_failed_wal_create_sets_the_wal_failure_and_later_sends_do_not_block() {
        let (fs, dir) = ws();
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        fs.fail("create_new", Code::DiskFull);
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        let f = drain(&s, beat);
        assert_eq!(f.wal.as_ref().map(|w| w.code), Some(Code::DiskFull));
        let dropped_at = pos(&fs.calls(), "drop_writer(/ws/p)");
        let calls_before = fs.calls().len();
        assert!(dropped_at < calls_before);
        // 100 more sends against a dead log: each returns. A transiently full channel is fine (the thread is
        // still receiving); a beat budget of 500 (about 5 s of waiting) means the thread stopped receiving.
        let waits = Cell::new(0u32);
        let blocked: &Heartbeat = &|| {
            waits.set(waits.get() + 1);
            if waits.get() > 500 {
                return Err(FsError::new(
                    Code::TargetLockBusy,
                    io::Error::other("the sender blocked"),
                ));
            }
            Ok(())
        };
        for i in 0..100u64 {
            s.send(Msg::Range(range(b"p", i, i + 1)), blocked).unwrap();
        }
        s.send(Msg::Stop, beat).unwrap();
        let f = t.join();
        assert!(f.wal.is_some());
    }

    #[test]
    fn close_drops_the_duplicate() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Close { file_id: b"p".to_vec(), ack: None }, beat).unwrap();
        drain(&s, beat);
        let drops = fs.calls().iter().filter(|c| *c == "drop_writer(/ws/p)").count();
        assert_eq!(drops, 1);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        drop(p);
    }

    #[test]
    fn close_flushes_that_files_pending_records_before_dropping_the_duplicate() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        s.send(Msg::Close { file_id: b"p".to_vec(), ack: None }, beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        let f = t.join();
        assert!(f.is_clean());
        let calls = fs.calls();
        assert!(pos(&calls, "sync_all(/ws/p)") < pos(&calls, "drop_writer(/ws/p)"));
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 2);
        drop(p);
    }

    #[test]
    fn strict_without_a_duplicate_appends_nothing_and_flags_the_file() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        let f = drain(&s, beat);
        assert_eq!(f.files[&b"p".to_vec()].kind, io::ErrorKind::InvalidInput);
        assert!(wal_bytes(&fs).is_none());
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn two_files_failing_in_one_flush_are_both_reported() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        fs.fail("sync_all", Code::StrictDurabilityUnavailable);
        let q = fs.create_new(Path::new("/ws/q")).unwrap();
        fs.fail("sync_all", Code::StrictDurabilityUnavailable);
        let r = fs.create_new(Path::new("/ws/r")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        for (id, h) in [(&b"p"[..], &p), (&b"q"[..], &q), (&b"r"[..], &r)] {
            s.send(begin_msg(id, Some(h.duplicate().unwrap())), beat).unwrap();
            s.send(Msg::Range(range(id, 0, 1)), beat).unwrap();
        }
        let f = drain(&s, beat);
        let mut keys: Vec<_> = f.files.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, [b"q".to_vec(), b"r".to_vec()]);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn drain_returns_the_failure_state_and_join_returns_the_final_one() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        fs.fail("sync_all", Code::StrictDurabilityUnavailable);
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        let drained = drain(&s, beat);
        assert!(!drained.files.is_empty());
        s.send(Msg::Stop, beat).unwrap();
        assert_eq!(t.join().files, drained.files);
    }

    #[test]
    fn close_acks_that_files_failure() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        fs.fail("sync_all", Code::StrictDurabilityUnavailable);
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let h = fs.create_new(Path::new("/ws/h")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        s.send(Msg::Close { file_id: b"p".to_vec(), ack: Some(tx) }, beat).unwrap();
        let f = rx.recv().unwrap().expect("p failed its sync");
        assert_eq!(f.code, Code::StrictDurabilityUnavailable);
        s.send(begin_msg(b"h", Some(h.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"h", 0, 1)), beat).unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        s.send(Msg::Close { file_id: b"h".to_vec(), ack: Some(tx) }, beat).unwrap();
        assert_eq!(rx.recv().unwrap(), None);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn close_after_a_wal_failure_acks_the_wal_error() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        fs.fail("create_new", Code::DiskFull);
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        assert!(drain(&s, beat).wal.is_some());
        let (tx, rx) = mpsc::sync_channel(1);
        s.send(Msg::Close { file_id: b"p".to_vec(), ack: Some(tx) }, beat).unwrap();
        let f = rx.recv().unwrap().expect("the WAL failed");
        assert_eq!(f.code, Code::DiskFull);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn open_with_a_lower_generation_is_refused() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 2, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let before = wal_bytes(&fs).unwrap();
        let again = fs.destination_root(Path::new("/ws")).unwrap();
        let err = open(again, OP.to_vec(), 1, cfg(BIG, HOUR, Durability::Normal)).err();
        assert!(matches!(err, Some(WalError::SequenceConflict { generation: 1, sequence: 1 })));
        assert_eq!(wal_bytes(&fs).unwrap(), before);
    }

    #[test]
    fn open_on_an_unavailable_identity_is_unsupported() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        fs.set_identity(WAL_PATH, FileIdentity::Unavailable);
        let again = fs.destination_root(Path::new("/ws")).unwrap();
        match open(again, OP.to_vec(), 1, cfg(BIG, HOUR, Durability::Normal)).err() {
            Some(WalError::Io(e)) => assert_eq!(e.source.kind(), io::ErrorKind::Unsupported),
            other => panic!("expected Io(Unsupported), got {other:?}"),
        }
        assert!(!fs.calls().iter().any(|c| c.starts_with("open_existing(")));
    }

    #[test]
    fn a_dropped_sender_without_stop_flushes_nothing() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        drop(s);
        t.join();
        assert!(wal_bytes(&fs).is_none());
        assert!(fs.calls().iter().any(|c| c == "drop_writer(/ws/p)"));
        drop(p);
    }

    #[test]
    fn stop_flushes_pending() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 2);
    }

    #[test]
    fn a_full_channel_blocks_the_sender_and_beats() {
        let (fs, dir) = ws();
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        fs.delay("sync_all", Duration::from_millis(300));
        let mut c = cfg(BIG, HOUR, Durability::Strict);
        c.channel_cap = 1;
        let (s, t) = start(dir, 1, c);
        let beats = Cell::new(0u32);
        let beat: &Heartbeat = &|| {
            beats.set(beats.get() + 1);
            Ok(())
        };
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Flush, beat).unwrap();
        for i in 0..3 {
            s.send(Msg::Range(range(b"p", i, i + 1)), beat).unwrap();
        }
        assert!(beats.get() >= 1, "the sender never beat while blocked");
        s.send(Msg::Stop, &|| Ok(())).unwrap();
        t.join();
    }

    #[test]
    fn send_after_join_is_wal_failed() {
        let (_fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        assert!(matches!(s.send(Msg::Range(range(b"p", 0, 1)), beat), Err(SendError::WalFailed)));
    }

    #[test]
    fn a_heartbeat_error_while_the_channel_is_full_is_returned_as_is() {
        let (fs, dir) = ws();
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        fs.delay("sync_all", Duration::from_millis(300));
        let mut c = cfg(BIG, HOUR, Durability::Strict);
        c.channel_cap = 1;
        let (s, t) = start(dir, 1, c);
        let ok: &Heartbeat = &|| Ok(());
        let bad: &Heartbeat =
            &|| Err(FsError::new(Code::TargetLockBusy, io::Error::other("lock lost")));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), ok).unwrap();
        s.send(Msg::Flush, ok).unwrap();
        let mut outcome = None;
        for i in 0..5 {
            if let Err(e) = s.send(Msg::Range(range(b"p", i, i + 1)), bad) {
                outcome = Some(e);
                break;
            }
        }
        match outcome {
            Some(SendError::Heartbeat(e)) => assert_eq!(e.code, Code::TargetLockBusy),
            other => panic!("expected the heartbeat error, got {other:?}"),
        }
        s.send(Msg::Stop, ok).unwrap();
        t.join();
    }

    #[test]
    fn reopen_continues_the_sequence_in_the_same_generation_and_restarts_in_a_new_one() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let c = cfg(BIG, HOUR, Durability::Normal);
        let (s, t) = start(dir, 1, c);
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();

        let again = fs.destination_root(Path::new("/ws")).unwrap();
        let (s, t, found) = open(again, OP.to_vec(), 1, c).expect("reopen");
        assert_eq!(found.expect("a scan").last, Some((1, 2)));
        s.send(Msg::Range(range(b"p", 1, 2)), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let r = records(&wal_bytes(&fs).unwrap());
        assert_eq!(r.len(), 3);
        assert_eq!((r[2].0.generation, r[2].0.sequence), (1, 3));

        let third = fs.destination_root(Path::new("/ws")).unwrap();
        let (s, t, found) = open(third, OP.to_vec(), 2, c).expect("new generation");
        assert_eq!(found.expect("a scan").last, Some((1, 3)));
        s.send(Msg::Range(range(b"p", 2, 3)), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let r = records(&wal_bytes(&fs).unwrap());
        assert_eq!(r.len(), 4);
        assert_eq!((r[3].0.generation, r[3].0.sequence), (2, 1));
    }

    #[test]
    fn reopen_truncates_a_torn_tail() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let c = cfg(BIG, HOUR, Durability::Normal);
        let (s, t) = start(dir, 1, c);
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let good = wal_bytes(&fs).unwrap();
        let len = good.len() as u64;
        let mut torn = good.clone();
        torn.extend_from_slice(&[0xAB; 7]);
        fs.write_file(WAL_PATH, &torn);

        let again = fs.destination_root(Path::new("/ws")).unwrap();
        let (s, t, found) = open(again, OP.to_vec(), 1, c).expect("reopen");
        assert_eq!(found.expect("a scan").valid_len, len);
        assert_eq!(wal_bytes(&fs).unwrap().len() as u64, len);
        assert!(fs.calls().contains(&format!("truncate_to({WAL_PATH}, {len})")));
        s.send(Msg::Range(range(b"p", 1, 2)), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let bytes = wal_bytes(&fs).unwrap();
        let whole = scan(&bytes[..], OP).expect("the whole file scans");
        assert_eq!(whole.last, Some((1, 3)));
        assert_eq!(whole.valid_len, bytes.len() as u64);
    }

    #[test]
    fn an_oversized_range_is_refused_not_written() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"big", None), beat).unwrap();
        s.send(Msg::Range(range(b"big", 0, 32_001)), beat).unwrap();
        s.send(begin_msg(b"ok", None), beat).unwrap();
        s.send(Msg::Range(range(b"ok", 0, 1)), beat).unwrap();
        let f = drain(&s, beat);
        let fault = &f.files[&b"big".to_vec()];
        assert_eq!(fault.code, Code::DestinationError);
        assert_eq!(fault.kind, io::ErrorKind::InvalidInput);
        assert_eq!(fault.message, "range too large for one record");
        assert!(!f.files.contains_key(b"ok".as_slice()));
        let r = records(&wal_bytes(&fs).unwrap());
        assert_eq!(r.len(), 2);
        for (_, body) in &r {
            let id = match body {
                Body::FileBegin(b) => &b.file_id,
                Body::Range(r) => &r.file_id,
            };
            assert_eq!(id, b"ok");
        }
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }
}
