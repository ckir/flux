//! The WAL writer thread (cut 9f-1): a bounded channel, a flush policy, and the Strict
//! data-before-checkpoint ordering.
//!
//! Every record the copy loop wants logged is sent as a `Msg`. One thread owns the log segment and
//! every registered duplicate of a partial file. A flush syncs each file's duplicate FIRST (Strict),
//! then appends that file's records, then syncs the log, so a checkpoint is never durable before the
//! data it describes.

use super::MAX_PAYLOAD;
use super::record::{Body, FileBegin, Header, Range, encode};
use super::replay::{Scan, WalError, scan};
use crate::copy::Heartbeat;
use flux_fs::{Code, DirHandle, Durability, FileHandle, FileIdentity, FsError};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
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
    #[cfg(test)]
    pub(crate) fn is_clean(&self) -> bool {
        self.wal.is_none() && self.files.is_empty()
    }
}

pub(crate) enum Msg<W> {
    FileBegin {
        begin: FileBegin,
        dup: Option<W>,
    },
    Range(Range),
    /// Flush now. Spec decision 9 (V16 s132/s187): the writer drains on request, kept for a future cancellation cut.
    #[cfg_attr(not(test), allow(dead_code))]
    Flush,
    /// The `ack` channel must have capacity >= 1: the answer is delivered with `try_send`, so a zero-capacity
    /// or dropped receiver is ignored and never blocks the thread (and a capacity-0 channel loses the answer).
    Close {
        file_id: Vec<u8>,
        ack: Option<mpsc::SyncSender<Option<WalFault>>>,
    },
    /// The file's copy was abandoned (it failed, or the run lost its lock): drop its pending records and its
    /// duplicate. Flushes nothing and answers nothing, so no byte reaches the segment because of it.
    Discard {
        file_id: Vec<u8>,
    },
    /// The channel must have capacity >= 1 (see `Close`): a capacity-0 channel loses the answer, because
    /// `try_send` on it never succeeds.
    /// Flush, then answer with the failure state. Spec decision 9 (V16 s132/s187): drain on request, kept for
    /// cancellation.
    #[cfg_attr(not(test), allow(dead_code))]
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
    /// Shared with the thread. Set when the sender is dropped without a delivered `Stop`: the run is abandoned
    /// and the thread begins nothing more, whatever is still queued (ruling 11, spec decision 9).
    abandoned: Arc<AtomicBool>,
    stopped: AtomicBool,
}

impl<W> Drop for WalSender<W> {
    fn drop(&mut self) {
        if !self.stopped.load(Ordering::SeqCst) {
            self.abandoned.store(true, Ordering::SeqCst);
        }
    }
}

impl<W> WalSender<W> {
    /// Queue `msg`. A full channel is waited out in `SEND_RETRY` steps, calling `beat` before each wait so a
    /// lock-holding run keeps refreshing its lock (ruling 12).
    pub(crate) fn send(&self, msg: Msg<W>, beat: &Heartbeat<'_>) -> Result<(), SendError> {
        let is_stop = matches!(msg, Msg::Stop);
        let mut msg = msg;
        loop {
            match self.tx.try_send(msg) {
                Ok(()) => {
                    if is_stop {
                        self.stopped.store(true, Ordering::SeqCst);
                    }
                    return Ok(());
                }
                Err(TrySendError::Disconnected(_)) => return Err(SendError::WalFailed),
                Err(TrySendError::Full(back)) => {
                    msg = back;
                    beat().map_err(SendError::Heartbeat)?;
                    std::thread::sleep(SEND_RETRY);
                }
            }
        }
    }

    /// The run is abandoned from now on: the thread begins no new flush (it checks the flag before each one and in its
    /// loop). The same flag a dropped sender without `Stop` sets.
    pub(crate) fn mark_abandoned(&self) {
        self.abandoned.store(true, Ordering::SeqCst);
    }

    /// The first WAL fault, once `Failure.wal` is set. A dead log keeps its thread alive and discarding, so
    /// `send` still returns `Ok`; this is how a sender notices the log died.
    pub(crate) fn wal_fault(&self) -> Option<WalFault> {
        lock(&self.failure).wal.clone()
    }
}

/// A cloneable send/fault view of a `WalSender` for a worker thread. It has no `Drop` and no access to `stopped` or
/// `abandoned`, so dropping it never abandons the run; only the owning `WalSender` decides that.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct WalLink<W> {
    tx: SyncSender<Msg<W>>,
    failure: Arc<Mutex<Failure>>,
}

// Manual: a derive would demand `W: Clone`, and neither field needs it.
impl<W> Clone for WalLink<W> {
    fn clone(&self) -> Self {
        Self { tx: self.tx.clone(), failure: Arc::clone(&self.failure) }
    }
}

impl<W> WalSender<W> {
    /// A link for a worker thread to send through and read the log's failure state.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn link(&self) -> WalLink<W> {
        WalLink { tx: self.tx.clone(), failure: Arc::clone(&self.failure) }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl<W> WalLink<W> {
    /// Queue `msg` without waiting; a full channel is the caller's to handle.
    // The `Err` hands the unsent message back, so it is as large as `Msg`; the brief fixes this signature.
    #[allow(clippy::result_large_err)]
    pub(crate) fn try_send(&self, msg: Msg<W>) -> Result<(), TrySendError<Msg<W>>> {
        self.tx.try_send(msg)
    }

    /// The first WAL fault, the same read as `WalSender::wal_fault`.
    pub(crate) fn fault(&self) -> Option<WalFault> {
        lock(&self.failure).wal.clone()
    }
}

pub(crate) struct WalThread {
    handle: JoinHandle<()>,
    failure: Arc<Mutex<Failure>>,
}

impl WalThread {
    /// Wait for the thread and return the final failure state (after `Stop`'s flush).
    ///
    /// Drop the sender before joining, unless `Stop` was delivered: the thread exits only on `Stop` or when
    /// the sender is gone, so joining while the sender is alive and no `Stop` was delivered blocks forever.
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
    // The record layout stores the id's length in a u16; a longer id would panic the thread inside `encode`.
    if operation_id.len() > usize::from(u16::MAX) {
        return Err(io_err(
            Code::DestinationError,
            io::ErrorKind::InvalidInput,
            "the operation id is longer than 65,535 bytes",
        ));
    }
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
    let abandoned = Arc::new(AtomicBool::new(false));
    let state = State {
        abandoned: Arc::clone(&abandoned),
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
    Ok((
        WalSender { tx, failure: Arc::clone(&failure), abandoned, stopped: AtomicBool::new(false) },
        WalThread { handle, failure },
        found,
    ))
}

struct State<D: DirHandle> {
    abandoned: Arc<AtomicBool>,
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
            let deadline =
                self.first_pending_at.and_then(|f| f.checked_add(self.cfg.flush_interval));
            let received = match deadline {
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some(d) => rx.recv_timeout(d.saturating_duration_since(Instant::now())),
            };
            // std delivers queued messages before it reports a disconnect, so the flag, not the channel, says
            // whether the run was abandoned.
            if self.abandoned.load(Ordering::SeqCst) {
                return self.abandon();
            }
            let msg = match received {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => {
                    self.flush();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return self.abandon(),
            };
            match msg {
                Msg::FileBegin { begin, dup } => {
                    if self.dead {
                        continue;
                    }
                    // A `FileBegin` starts a new attempt at this id: it REPLACES the registered duplicate
                    // (`None` removes the old one, so a stale duplicate never satisfies the Strict guard or
                    // syncs the wrong file) and forgets the previous attempt's fault, which that attempt's
                    // `Close` already answered. `Failure.wal` is untouched.
                    match dup {
                        Some(d) => {
                            self.dups.insert(begin.file_id.clone(), d);
                        }
                        None => {
                            self.dups.remove(&begin.file_id);
                        }
                    }
                    lock(&self.failure).files.remove(&begin.file_id);
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
                        let _ = ack.try_send(answer);
                    }
                }
                Msg::Discard { file_id } => {
                    // Honoured on a dead log too (there it only drops what is already dropped).
                    let gone: u64 = self
                        .pending
                        .iter()
                        .filter(|(id, _)| *id == file_id)
                        .map(|(_, body)| match body {
                            Body::Range(r) => r.range_end.saturating_sub(r.range_start),
                            Body::FileBegin(_) => 0,
                        })
                        .sum();
                    self.pending.retain(|(id, _)| *id != file_id);
                    self.pending_bytes = self.pending_bytes.saturating_sub(gone);
                    if self.pending.is_empty() {
                        self.first_pending_at = None;
                    }
                    self.dups.remove(&file_id);
                }
                Msg::Drain(ack) => {
                    self.flush();
                    let snapshot = lock(&self.failure).clone();
                    let _ = ack.try_send(snapshot);
                }
                Msg::Stop => {
                    self.flush();
                    self.dups.clear();
                    return;
                }
            }
            let timer_due = self
                .first_pending_at
                .and_then(|first| first.checked_add(self.cfg.flush_interval))
                .is_some_and(|due| Instant::now() >= due);
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

    /// Opens the segment if it is not open yet. Returns the `wal/` directory handle when THIS call created the
    /// segment, so the caller can make its directory entries durable.
    fn ensure_wal(&mut self) -> Result<Option<D>, FsError> {
        if self.wal.is_some() {
            return Ok(None);
        }
        let dir = match self.workspace.create_dir(OsStr::new(DIR_NAME)) {
            Ok(d) => d,
            Err(e) if e.source.kind() == io::ErrorKind::AlreadyExists => {
                self.workspace.open_dir(OsStr::new(DIR_NAME))?
            }
            Err(e) => return Err(e),
        };
        self.wal = Some(dir.create_new(OsStr::new(SEGMENT_NAME))?);
        Ok(Some(dir))
    }

    fn flush(&mut self) {
        if self.abandoned.load(Ordering::SeqCst) {
            // Abandoned while this flush was queued: begin nothing. An append already in flight is not stopped.
            return self.abandon();
        }
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
        let mut oversized: HashMap<&[u8], &'static str> = HashMap::new();
        for (id, body) in &pending {
            if seen.insert(id.as_slice()) {
                order.push(id.as_slice());
            }
            if self.payload_len(body) > u64::from(MAX_PAYLOAD) {
                let why = match body {
                    Body::Range(_) => "range too large for one record",
                    Body::FileBegin(_) => "record too large for one record",
                };
                oversized.entry(id.as_slice()).or_insert(why);
            }
        }
        // Files whose records this flush must not append, and why.
        let mut bad: HashSet<Vec<u8>> = HashSet::new();
        for id in order {
            if lock(&self.failure).files.contains_key(id) {
                // A file whose data sync already failed never gets a checkpoint later: a retried sync that
                // succeeds proves nothing about the pages the failed one lost.
                bad.insert(id.to_vec());
            } else if let Some(why) = oversized.get(id) {
                self.fail_file(
                    id,
                    WalFault::new(Code::DestinationError, io::ErrorKind::InvalidInput, why),
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
        let created = match self.ensure_wal() {
            Ok(created) => created,
            Err(e) => {
                self.fail_wal(WalFault::from_error(&e));
                return;
            }
        };
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
        if self.cfg.durability == Durability::Strict {
            let mut synced = wal.sync_all();
            // The segment's own directory entries, after its data, so a Strict acknowledgement is true of the
            // name as well as the bytes (plan 1 ruling 22). Only the flush that created the segment needs it.
            if synced.is_ok()
                && let Some(dir) = created
            {
                synced = dir.sync().and_then(|()| self.workspace.sync());
            }
            if let Err(e) = synced {
                self.fail_wal(WalFault::from_error(&e));
            }
        }
    }

    /// The payload length `encode` would give `body`, read from the encoded record (the length field sits after
    /// `MAGIC(4) FORMAT_VERSION(2) CHECKSUM_VERSION(1) record_type(1) operation_id_len(2) operation_id
    /// generation(8) sequence(8)`). A record over `MAX_PAYLOAD` would read back as `Corrupt`.
    fn payload_len(&self, body: &Body) -> u64 {
        let header = Header {
            operation_id: self.operation_id.clone(),
            generation: self.generation,
            sequence: self.next_seq,
        };
        let bytes = encode(&header, body);
        let at = 10 + self.operation_id.len() + 16;
        let field: [u8; 4] = bytes[at..at + 4].try_into().expect("four length bytes");
        u64::from(u32::from_le_bytes(field))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::{FakeDirHandle, FakeHandle, FaultFs};
    use crate::wal::CHUNK_SIZE;
    use crate::wal::record::{Decoded, decode_next};
    use crate::wal::replay::{DiscardedTail, TailReason};
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

    /// The call log with every path separator normalised to `/`. The fake builds child paths with `Path::join`,
    /// which yields `\` on Windows, and its log is an ORDER oracle, not a path oracle.
    fn unix(calls: Vec<String>) -> Vec<String> {
        calls.into_iter().map(|c| c.replace('\\', "/")).collect()
    }

    fn calls_unix(fs: &FaultFs) -> Vec<String> {
        unix(fs.calls())
    }

    fn syncs(fs: &FaultFs) -> Vec<String> {
        calls_unix(fs).into_iter().filter(|c| c.starts_with("sync_all(")).collect()
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
        // A `Close` for another file answers without flushing, and the thread handles messages in order, so
        // once it is answered the `FileBegin` before it has certainly been processed.
        let (tx, rx) = mpsc::sync_channel(1);
        s.send(Msg::Close { file_id: b"other".to_vec(), ack: Some(tx) }, beat).unwrap();
        assert_eq!(rx.recv().unwrap(), None);
        assert!(wal_bytes(&fs).is_none());
        assert!(!calls_unix(&fs).iter().any(|c| c.starts_with("create_dir(/ws/wal")));
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
        // Both records are on disk BEFORE `Stop`, which would flush them on exit anyway: only the timer put them there.
        let bytes =
            wait_for("the timer flush", || wal_bytes(&fs).filter(|b| records(b).len() == 2));
        let r = records(&bytes);
        assert!(matches!(r[0].1, Body::FileBegin(_)));
        assert!(matches!(r[1].1, Body::Range(_)));
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
        assert!(!calls_unix(&fs).iter().any(|c| c.starts_with("create_dir(/ws/wal")));
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
        // `FakeHandle::sync_all` logs its call directly rather than through `FaultFs::record`, but it runs the hook
        // (`run_hook`), so `on_nth("sync_all", ..)` fires.
        // The call log carries the same fact: the WAL's directory and segment are created only after the
        // partial's sync, so the WAL was absent when it ran.
        let calls = calls_unix(&fs);
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
        // The failure released the duplicate (and only it: the original `p` is still alive) before any later send.
        let drops = calls_unix(&fs).iter().filter(|c| *c == "drop_writer(/ws/p)").count();
        assert_eq!(drops, 1);
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
    fn wal_fault_is_some_after_a_failed_append_while_send_still_succeeds() {
        let (fs, dir) = ws();
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        fs.fail("create_new", Code::DiskFull);
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        assert_eq!(s.wal_fault(), None);
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        drain(&s, beat);
        assert_eq!(s.wal_fault().map(|w| w.code), Some(Code::DiskFull));
        s.send(Msg::Range(range(b"p", 1, 2)), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn a_link_delivers_a_range_that_reaches_the_log() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        let l = s.link();
        s.send(begin_msg(b"p", None), beat).unwrap();
        l.try_send(Msg::Range(range(b"p", 0, 1))).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let r = records(&wal_bytes(&fs).unwrap());
        assert_eq!(r.len(), 2);
        assert!(matches!(r[0].1, Body::FileBegin(_)));
        assert!(matches!(r[1].1, Body::Range(_)));
    }

    #[test]
    fn a_link_reads_the_same_fault_as_the_sender() {
        let (fs, dir) = ws();
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        fs.fail("create_new", Code::DiskFull);
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        let l = s.link();
        assert_eq!(l.fault(), None);
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        drain(&s, beat);
        assert!(l.fault().is_some());
        assert!(s.wal_fault().is_some());
        assert_eq!(l.fault(), s.wal_fault());
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn dropping_a_link_does_not_abandon_the_run() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        let l = s.link();
        s.send(begin_msg(b"p", None), beat).unwrap();
        drop(l);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let r = records(&wal_bytes(&fs).unwrap());
        assert_eq!(r.len(), 1);
        assert!(matches!(r[0].1, Body::FileBegin(_)));
    }

    #[test]
    fn a_link_is_send_and_sync_for_a_send_writer() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<WalLink<FakeHandle>>();
        assert_sync::<WalLink<FakeHandle>>();
    }

    /// Wait until every message sent so far was handled, flushing nothing (a `Close` of a file with no pending
    /// records answers without a flush; `drain` would flush).
    fn settle(s: &Sender, beat: &Heartbeat<'_>) {
        let (tx, rx) = mpsc::sync_channel(1);
        s.send(Msg::Close { file_id: b"nobody".to_vec(), ack: Some(tx) }, beat).unwrap();
        rx.recv().unwrap();
    }

    fn body_file(b: &Body) -> &[u8] {
        match b {
            Body::FileBegin(f) => &f.file_id,
            Body::Range(r) => &r.file_id,
        }
    }

    #[test]
    fn mark_abandoned_stops_a_flush_that_would_otherwise_start() {
        let beat: &Heartbeat = &|| Ok(());
        // The control: an eager policy flushes as soon as a range is queued.
        let (fs, dir) = ws();
        let (s, t) = start(dir, 1, cfg(1, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        settle(&s, beat);
        assert!(wal_bytes(&fs).is_some(), "without the mark the range flushes at once");
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        // The same messages after the mark: the thread begins no flush.
        let (fs, dir) = ws();
        let (s, t) = start(dir, 1, cfg(1, HOUR, Durability::Normal));
        s.mark_abandoned();
        s.send(begin_msg(b"p", None), beat).unwrap();
        let _ = s.send(Msg::Range(range(b"p", 0, 1)), beat);
        drop(s);
        t.join();
        assert!(wal_bytes(&fs).is_none(), "no segment was ever created");
    }

    #[test]
    fn discard_drops_the_files_pending_records_and_its_duplicate_and_flushes_nothing() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        s.send(Msg::Discard { file_id: b"p".to_vec() }, beat).unwrap();
        settle(&s, beat);
        let drops =
            |fs: &FaultFs| calls_unix(fs).iter().filter(|c| *c == "drop_writer(/ws/p)").count();
        assert_eq!(drops(&fs), 1, "the duplicate was dropped");
        assert!(wal_bytes(&fs).is_none(), "nothing was appended");
        s.send(Msg::Stop, beat).unwrap();
        assert!(t.join().is_clean());
        assert!(wal_bytes(&fs).is_none(), "not even the Stop flushed the discarded records");
        assert!(syncs(&fs).is_empty());
        drop(p);
    }

    #[test]
    fn discard_leaves_other_files_pending_records_to_flush_later() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        s.send(begin_msg(b"q", None), beat).unwrap();
        s.send(Msg::Range(range(b"q", 0, 1)), beat).unwrap();
        s.send(Msg::Discard { file_id: b"p".to_vec() }, beat).unwrap();
        settle(&s, beat);
        assert!(wal_bytes(&fs).is_none());
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let recs = records(&wal_bytes(&fs).unwrap());
        assert_eq!(recs.len(), 2);
        assert!(recs.iter().all(|(_, b)| body_file(b) == b"q"), "{recs:?}");
    }

    #[test]
    fn discard_keeps_the_flush_threshold_accounting_exact() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        // The trigger is 5 chunks of pending range bytes.
        let (s, t) = start(dir, 1, cfg(5 * CHUNK_SIZE, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 3)), beat).unwrap();
        s.send(begin_msg(b"q", None), beat).unwrap();
        s.send(Msg::Range(range(b"q", 0, 1)), beat).unwrap();
        settle(&s, beat);
        assert!(wal_bytes(&fs).is_none(), "4 pending chunks: below the trigger");
        s.send(Msg::Discard { file_id: b"p".to_vec() }, beat).unwrap();
        // Stale accounting would count p's 3 chunks still: 3 + 1 + 3 crosses 5 here.
        s.send(Msg::Range(range(b"q", 1, 4)), beat).unwrap();
        settle(&s, beat);
        assert!(wal_bytes(&fs).is_none(), "q's 4 chunks alone: still below the trigger");
        s.send(Msg::Range(range(b"q", 4, 5)), beat).unwrap();
        for _ in 0..5000 {
            if wal_bytes(&fs).is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let recs = records(&wal_bytes(&fs).expect("q's 5 chunks cross the trigger"));
        assert!(recs.iter().all(|(_, b)| body_file(b) == b"q"), "{recs:?}");
        s.send(Msg::Stop, beat).unwrap();
        t.join();
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
        let drops = calls_unix(&fs).iter().filter(|c| *c == "drop_writer(/ws/p)").count();
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
        let calls = calls_unix(&fs);
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
        assert_eq!(drained.files.keys().collect::<Vec<_>>(), [&b"p".to_vec()]);
        assert_eq!(drained.files[&b"p".to_vec()].code, Code::StrictDurabilityUnavailable);
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
    fn sequence_conflict_text_is_true_for_both_producers() {
        use crate::wal::replay::tests::{begin as rbegin, range as rrange, stream};
        // The writer: the log holds generation 2, the run is generation 1.
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 2, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let again = fs.destination_root(Path::new("/ws")).unwrap();
        let err = open(again, OP.to_vec(), 1, cfg(BIG, HOUR, Durability::Normal)).err().unwrap();
        assert_eq!(
            err.to_string(),
            "generation 1, sequence 1 conflicts with the records already in the log"
        );
        // The replay: a duplicate (generation 1, sequence 2) pair with different payloads.
        let bytes = stream(&[
            (1, 1, rbegin(b'A', 0)),
            (1, 2, rrange(b'A', 0, 1, 1)),
            (1, 2, rrange(b'A', 0, 1, 9)),
        ]);
        let err = crate::wal::replay::scan(std::io::Cursor::new(bytes), b"op-replay").unwrap_err();
        assert_eq!(
            err.to_string(),
            "generation 1, sequence 2 conflicts with the records already in the log"
        );
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
        let calls = calls_unix(&fs);
        assert!(!calls.iter().any(|c| c.starts_with("open_existing(")));
        assert!(!calls.iter().any(|c| c.starts_with("truncate_to(")));
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
        assert!(!calls_unix(&fs).iter().any(|c| c.starts_with("open_existing(")));
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
        assert!(calls_unix(&fs).iter().any(|c| c == "drop_writer(/ws/p)"));
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
        assert!(calls_unix(&fs).contains(&format!("truncate_to({WAL_PATH}, {len})")));
        s.send(Msg::Range(range(b"p", 1, 2)), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let bytes = wal_bytes(&fs).unwrap();
        let whole = scan(&bytes[..], OP).expect("the whole file scans");
        assert_eq!(whole.last, Some((1, 3)));
        assert_eq!(whole.valid_len, bytes.len() as u64);
    }

    #[test]
    fn reopen_truncates_a_zero_filled_tail() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let c = cfg(BIG, HOUR, Durability::Normal);
        let (s, t) = start(dir, 1, c);
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let len = wal_bytes(&fs).unwrap().len() as u64;
        let mut padded = wal_bytes(&fs).unwrap();
        padded.extend(std::iter::repeat_n(0u8, 300));
        fs.write_file(WAL_PATH, &padded);

        let again = fs.destination_root(Path::new("/ws")).unwrap();
        let (s, t, found) = open(again, OP.to_vec(), 1, c).expect("a zero-filled tail is torn");
        let found = found.expect("a scan");
        assert_eq!((found.valid_len, found.last), (len, Some((1, 2))));
        assert_eq!(
            found.discarded_tail,
            Some(DiscardedTail { offset: len, len: 300, reason: TailReason::ZeroFilled })
        );
        assert_eq!(wal_bytes(&fs).unwrap().len() as u64, len);
        assert!(calls_unix(&fs).contains(&format!("truncate_to({WAL_PATH}, {len})")));
        s.send(Msg::Range(range(b"p", 1, 2)), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let bytes = wal_bytes(&fs).unwrap();
        let whole = scan(&bytes[..], OP).expect("the whole file scans clean");
        assert_eq!((whole.last, whole.valid_len), (Some((1, 3)), bytes.len() as u64));
        assert_eq!(whole.discarded_tail, None);
    }

    #[test]
    fn an_oversized_range_is_refused_not_written() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"big", None), beat).unwrap();
        s.send(Msg::Range(range(b"big", 0, 32_769)), beat).unwrap();
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

    #[test]
    fn a_range_just_under_the_payload_bound_is_accepted() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 32_000)), beat).unwrap();
        assert!(drain(&s, beat).is_clean());
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 2);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn a_long_file_id_makes_a_small_range_oversized() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        let long = vec![7u8; 1 << 20];
        s.send(begin_msg(b"ok", None), beat).unwrap();
        s.send(Msg::Range(range(&long, 0, 1)), beat).unwrap();
        let f = drain(&s, beat);
        let fault = &f.files[&long];
        assert_eq!(fault.kind, io::ErrorKind::InvalidInput);
        assert_eq!(fault.message, "range too large for one record");
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 1);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn an_oversized_file_begin_is_refused() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        let mut big = begin(b"big");
        big.destination = vec![b'd'; 1 << 20];
        s.send(Msg::FileBegin { begin: big, dup: None }, beat).unwrap();
        s.send(begin_msg(b"ok", None), beat).unwrap();
        let f = drain(&s, beat);
        assert_eq!(f.files[&b"big".to_vec()].message, "record too large for one record");
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 1);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn a_dead_or_zero_capacity_ack_never_blocks_the_thread() {
        let (_fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        s.send(Msg::Close { file_id: b"p".to_vec(), ack: Some(tx) }, beat).unwrap();
        let (tx, _rx) = mpsc::sync_channel(0);
        s.send(Msg::Close { file_id: b"p".to_vec(), ack: Some(tx) }, beat).unwrap();
        let (tx, _rx) = mpsc::sync_channel(0);
        s.send(Msg::Drain(tx), beat).unwrap();
        // The thread is still alive and answering.
        assert!(drain(&s, beat).is_clean());
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn an_operation_id_above_the_u16_bound_is_refused_at_open() {
        let (_fs, dir) = ws();
        let too_long = vec![b'o'; usize::from(u16::MAX) + 1];
        match open(dir, too_long, 1, cfg(BIG, HOUR, Durability::Normal)).err() {
            Some(WalError::Io(e)) => assert_eq!(e.source.kind(), io::ErrorKind::InvalidInput),
            other => panic!("expected Io(InvalidInput), got {other:?}"),
        }
        // Exactly 65,535 bytes is the largest id, and it encodes.
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let longest = vec![b'o'; usize::from(u16::MAX)];
        let (s, t, _) =
            open(dir, longest.clone(), 1, cfg(BIG, HOUR, Durability::Normal)).expect("open");
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let r = records(&wal_bytes(&fs).unwrap());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0.operation_id, longest);
    }

    #[test]
    fn a_refiled_file_does_not_inherit_the_previous_duplicate() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        let f = drain(&s, beat);
        // The second attempt has no duplicate registered: refused, and nothing synced or appended.
        let fault = &f.files[&b"p".to_vec()];
        assert_eq!(fault.kind, io::ErrorKind::InvalidInput);
        assert_eq!(fault.message, "no duplicate registered");
        assert!(wal_bytes(&fs).is_none());
        assert!(syncs(&fs).is_empty());
        // The first attempt's duplicate was dropped when the second `FileBegin` replaced it.
        let drops = calls_unix(&fs).iter().filter(|c| *c == "drop_writer(/ws/p)").count();
        assert_eq!(drops, 1);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        drop(p);
    }

    #[test]
    fn a_restaged_file_checkpoints_again_after_a_sync_failure() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        fs.fail("sync_all", Code::StrictDurabilityUnavailable);
        let first = fs.create_new(Path::new("/ws/first")).unwrap();
        let second = fs.create_new(Path::new("/ws/second")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        // Attempt 1: its duplicate's sync fails, and its `Close` answers the fault.
        s.send(begin_msg(b"p", Some(first.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        s.send(Msg::Close { file_id: b"p".to_vec(), ack: Some(tx) }, beat).unwrap();
        let fault = rx.recv().unwrap().expect("attempt 1 failed its sync");
        assert_eq!(fault.code, Code::StrictDurabilityUnavailable);
        assert!(wal_bytes(&fs).is_none());
        // Attempt 2 of the same file id, with a healthy duplicate: it checkpoints again.
        s.send(begin_msg(b"p", Some(second.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        let f = drain(&s, beat);
        assert!(f.is_clean(), "{f:?}");
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 2);
        assert_eq!(
            syncs(&fs),
            ["sync_all(/ws/first)", "sync_all(/ws/second)", "sync_all(/ws/wal/000001)"]
        );
        // And its `Close` no longer answers the previous attempt's fault.
        let (tx, rx) = mpsc::sync_channel(1);
        s.send(Msg::Close { file_id: b"p".to_vec(), ack: Some(tx) }, beat).unwrap();
        assert_eq!(rx.recv().unwrap(), None);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        drop((first, second));
    }

    #[test]
    fn a_file_whose_sync_failed_gets_no_checkpoint_in_a_later_flush() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        fs.fail("sync_all", Code::StrictDurabilityUnavailable);
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        let first = drain(&s, beat);
        assert_eq!(first.files[&b"p".to_vec()].code, Code::StrictDurabilityUnavailable);
        // A later flush for the same attempt: the injected fault is spent, so a retried sync would succeed,
        // and that proves nothing about the pages the failed one lost. Nothing may be appended and no sync retried.
        s.send(Msg::Range(range(b"p", 1, 2)), beat).unwrap();
        let second = drain(&s, beat);
        assert_eq!(second.files[&b"p".to_vec()].code, Code::StrictDurabilityUnavailable);
        assert!(wal_bytes(&fs).is_none());
        assert_eq!(syncs(&fs), ["sync_all(/ws/p)"]);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn a_later_strict_flush_syncs_the_duplicate_again() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        drain(&s, beat);
        s.send(Msg::Range(range(b"p", 1, 2)), beat).unwrap();
        drain(&s, beat);
        assert_eq!(
            syncs(&fs),
            [
                "sync_all(/ws/p)",
                "sync_all(/ws/wal/000001)",
                "sync_all(/ws/p)",
                "sync_all(/ws/wal/000001)"
            ]
        );
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 3);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn a_sender_dropped_mid_flush_begins_no_further_flush() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        // The first `sync_all` of the run is the first flush's sync of the partial. The hook announces that the thread
        // is inside it and holds it there until the test releases it, so the flush is provably in flight (no sleeps).
        let (entered_tx, entered) = mpsc::channel::<()>();
        let (release, released) = mpsc::channel::<()>();
        fs.on_nth("sync_all", 1, move |_| {
            entered_tx.send(()).unwrap();
            released
                .recv_timeout(Duration::from_secs(10))
                .expect("the test never released the held first sync_all");
        });
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        s.send(Msg::Flush, beat).unwrap();
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("the flush never reached its first sync_all");
        // A second flush is queued behind the one in flight, then the sender goes while that flush is blocked.
        s.send(Msg::Range(range(b"p", 1, 2)), beat).unwrap();
        s.send(Msg::Flush, beat).unwrap();
        drop(s);
        release.send(()).unwrap();
        t.join();
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 2);
        assert_eq!(syncs(&fs).iter().filter(|c| *c == "sync_all(/ws/p)").count(), 1);
        drop(p);
    }

    #[test]
    fn a_stopped_sender_that_is_then_dropped_is_not_abandoned() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        drop(s);
        t.join();
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 1);
    }

    fn dir_syncs(fs: &FaultFs) -> Vec<String> {
        calls_unix(fs).into_iter().filter(|c| c.starts_with("sync_dir(")).collect()
    }

    #[test]
    fn strict_makes_the_new_segment_entries_durable_after_the_segment_itself() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        drain(&s, beat);
        let calls = calls_unix(&fs);
        let seg = pos(&calls, "sync_all(/ws/wal/000001)");
        assert!(pos(&calls, "sync_dir(/ws/wal)") > seg);
        assert!(pos(&calls, "sync_dir(/ws)") > seg);
        // A second flush into the same segment needs no directory sync.
        s.send(Msg::Range(range(b"p", 0, 1)), beat).unwrap();
        drain(&s, beat);
        assert_eq!(dir_syncs(&fs).len(), 2);
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn normal_makes_no_directory_sync() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        s.send(begin_msg(b"p", None), beat).unwrap();
        drain(&s, beat);
        assert!(dir_syncs(&fs).is_empty());
        assert!(syncs(&fs).is_empty());
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn a_reopened_segment_issues_no_directory_sync() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let c = cfg(BIG, HOUR, Durability::Strict);
        let (s, t) = start(dir, 1, c);
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        let before = dir_syncs(&fs).len();
        assert_eq!(before, 2);
        let again = fs.destination_root(Path::new("/ws")).unwrap();
        let (s, t, _) = open(again, OP.to_vec(), 1, c).expect("reopen");
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        s.send(Msg::Stop, beat).unwrap();
        t.join();
        assert_eq!(records(&wal_bytes(&fs).unwrap()).len(), 2);
        assert_eq!(dir_syncs(&fs).len(), before);
    }

    #[test]
    fn a_failed_directory_sync_is_a_wal_failure() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        fs.fail("sync_dir", Code::IoError);
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Strict));
        s.send(begin_msg(b"p", Some(p.duplicate().unwrap())), beat).unwrap();
        let f = drain(&s, beat);
        assert_eq!(f.wal.as_ref().map(|w| w.code), Some(Code::IoError));
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn the_call_log_helper_normalises_windows_separators() {
        let raw = vec![
            "sync_all(/ws\\wal\\000001)".to_owned(),
            "truncate_to(/ws\\wal\\000001, 7)".to_owned(),
            "sync_all(/ws/p)".to_owned(),
        ];
        assert_eq!(
            unix(raw),
            ["sync_all(/ws/wal/000001)", "truncate_to(/ws/wal/000001, 7)", "sync_all(/ws/p)"]
        );
    }

    #[test]
    fn the_payload_bound_is_exact() {
        let (fs, dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let (s, t) = start(dir, 1, cfg(BIG, HOUR, Durability::Normal));
        // A Range payload is 4 + L + 8 + 8 + 8 + 4 + 32n; one digest (an empty range does not decode) gives
        // 64 + L, so L = MAX_PAYLOAD - 64 is exactly at the bound.
        let at_bound = vec![1u8; MAX_PAYLOAD as usize - 64];
        let over = vec![2u8; MAX_PAYLOAD as usize - 64 + 1];
        s.send(Msg::Range(range(&at_bound, 0, 1)), beat).unwrap();
        s.send(Msg::Range(range(&over, 0, 1)), beat).unwrap();
        let f = drain(&s, beat);
        assert!(!f.files.contains_key(&at_bound));
        let fault = &f.files[&over];
        assert_eq!(fault.kind, io::ErrorKind::InvalidInput);
        assert_eq!(fault.message, "range too large for one record");
        let bytes = wal_bytes(&fs).unwrap();
        // `records` panics unless the record decodes as `Record`.
        let r = records(&bytes);
        assert_eq!(r.len(), 1);
        assert!(matches!(&r[0].1, Body::Range(x) if x.file_id == at_bound));
        s.send(Msg::Stop, beat).unwrap();
        t.join();
    }

    #[test]
    fn the_wal_defaults_are_pinned() {
        let d = WalConfig::DEFAULT;
        assert_eq!(d.flush_interval, Duration::from_secs(1));
        assert_eq!(d.flush_bytes, 64 << 20);
        assert_eq!(d.channel_cap, 64);
        assert_eq!(d.durability, Durability::Normal);
        assert_eq!(CHANNEL_CAP, 64);
        assert_eq!(SEND_RETRY, Duration::from_millis(10));
        assert_eq!(DIR_NAME, "wal");
        assert_eq!(SEGMENT_NAME, "000001");
    }
}
