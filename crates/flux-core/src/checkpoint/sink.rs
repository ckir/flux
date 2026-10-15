//! The per-file sink: source bytes in, chunk digests out as `Range` records.

use super::worker::{POOL, Worker, WorkerArgs, WorkerFail};
use super::{Checkpoints, FileCkpt};
use crate::copy::Heartbeat;
use crate::wal::digest::ChunkHasher;
use crate::wal::record::{FileBegin, Range};
use crate::wal::writer::{Msg, SEND_RETRY, SendError, WalFault};
use crate::wal::{CHUNK_SIZE, Digest};
use flux_fs::{Code, DirHandle, FsError, Metadata};
use std::io;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::UNIX_EPOCH;

/// The attempt number every checkpointed file uses.
pub(super) const ATTEMPT_ID: u64 = 1;

#[derive(Debug)]
pub(crate) enum SinkError {
    /// The heartbeat failed while waiting; the error is the heartbeat's, unchanged.
    Heartbeat(FsError),
    /// The log itself failed (append, log sync, the writer gone).
    Wal(WalFault),
    /// This file's checkpoint data sync (Strict) or its oversize refusal failed.
    Data(WalFault),
}

pub(crate) struct FileSink<'a, D: DirHandle + Send + 'static> {
    run: &'a Checkpoints<D>,
    file_id: Vec<u8>,
    hasher: ChunkHasher,
    /// Byte offset where `digests` starts.
    range_start: u64,
    /// Digests of completed chunks not yet sent.
    digests: Vec<Digest>,
    /// Set by `complete` and `abort`; `Drop` asserts it.
    ended: bool,
    /// Where the hashing happens. While it is `Worker`, the worker owns the hashing state (`hasher`, `digests`,
    /// `range_start` above are stale until `complete` takes them back).
    hashing: Hashing,
}

/// How a sink hashes (cut 9h, spec 3.4): chosen at `begin`, settled at the first `submit`.
enum Hashing {
    /// On the calling thread, through `wrote`: one CPU, or the worker could not be spawned.
    Inline,
    /// A worker is wanted; the first `submit` spawns it, after `record_proven` and the carry ran inline.
    Pending,
    /// The worker hashes every submitted buffer.
    Worker(Worker),
}

impl<D: DirHandle + Send + 'static> Drop for FileSink<'_, D> {
    fn drop(&mut self) {
        // The worker stops first, so not even a missed `complete`/`abort` leaves it running or sending.
        if let Hashing::Worker(worker) = &mut self.hashing {
            worker.cancel_and_join(None);
        }
        debug_assert!(
            self.ended || std::thread::panicking(),
            "a FileSink must end through complete or abort"
        );
    }
}

/// `(seconds, nanoseconds)` since the Unix epoch, nanoseconds in `0..1_000_000_000`: a time before the epoch has
/// negative seconds (floored), so the pair always denotes `secs + nanos / 1e9`.
pub(crate) fn mtime_pair(t: std::time::SystemTime) -> (i64, u32) {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => (i64::try_from(d.as_secs()).unwrap_or(i64::MAX), d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            let secs = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
            match d.subsec_nanos() {
                0 => (-secs, 0),
                n => (-secs - 1, 1_000_000_000 - n),
            }
        }
    }
}

impl<'a, D: DirHandle + Send + 'static> FileSink<'a, D> {
    /// Registers the file with the log. `valid_through` is a byte offset on a chunk boundary, or the source length
    /// (a fully validated partial whose last chunk is short): the bytes fed to `wrote` start there, and chunk
    /// numbering resumes from it. At the source length no bytes follow, so no chunk is ever numbered from it.
    pub(crate) fn begin(
        fc: &FileCkpt<'a, D>,
        src: &Metadata,
        valid_through: u64,
        dup: Option<D::Writer>,
        beat: &Heartbeat<'_>,
    ) -> Result<Self, SinkError> {
        // A hard assert: a misaligned value in a release build would write digests labelled with the wrong chunks. A
        // panic in a library path is acceptable here because the value can only come from `decide`, which never
        // produces one.
        assert!(
            valid_through.is_multiple_of(CHUNK_SIZE) || valid_through == src.len,
            "valid_through must be a chunk boundary or the source length"
        );
        let first_chunk = valid_through.div_ceil(CHUNK_SIZE);
        let begin = FileBegin {
            file_id: fc.file_id.clone(),
            attempt_id: ATTEMPT_ID,
            destination: fc.destination.clone(),
            source_len: src.len,
            source_mtime: src.modified.map(mtime_pair),
            source_identity: crate::state::identity_text(src.identity).into_bytes(),
            chunk_size: CHUNK_SIZE,
            valid_through,
        };
        let sink = FileSink {
            run: fc.run,
            file_id: fc.file_id.clone(),
            hasher: ChunkHasher::from_chunk(first_chunk),
            range_start: valid_through,
            digests: Vec::new(),
            // Nothing is registered until the message is queued, so a failed `begin` leaves nothing to close.
            ended: true,
            hashing: if Worker::available() { Hashing::Pending } else { Hashing::Inline },
        };
        sink.send(Msg::FileBegin { begin, dup }, beat)?;
        let mut sink = sink;
        sink.ended = false;
        Ok(sink)
    }

    fn send(&self, msg: Msg<D::Writer>, beat: &Heartbeat<'_>) -> Result<(), SinkError> {
        self.run.sender.send(msg, beat).map_err(|e| match e {
            SendError::WalFailed => SinkError::Wal(self.wal_fault_or_gone()),
            SendError::Heartbeat(e) => SinkError::Heartbeat(e),
        })
    }

    fn wal_fault_or_gone(&self) -> WalFault {
        self.run.sender.wal_fault().unwrap_or_else(|| {
            WalFault::new(
                Code::WalWriteFailed,
                io::ErrorKind::BrokenPipe,
                "the WAL writer thread is gone",
            )
        })
    }

    /// The fault for a worker that is gone (its channels disconnected): the log's fault when it has one, then the
    /// fault the worker recorded, else the worker itself stopped (it panicked).
    fn worker_gone(&self, worker: Option<&Worker>) -> WalFault {
        self.run.sender.wal_fault().or_else(|| worker.and_then(Worker::fault)).unwrap_or_else(
            || {
                WalFault::new(
                    Code::WalWriteFailed,
                    io::ErrorKind::BrokenPipe,
                    "the checkpoint hashing worker stopped",
                )
            },
        )
    }

    /// Checks (1) and (2) of `submit`: the log's fault, then the worker's.
    fn check_faults(&self, worker: &Worker) -> Result<(), SinkError> {
        if let Some(f) = self.run.sender.wal_fault() {
            return Err(SinkError::Wal(f));
        }
        if let Some(f) = worker.fault() {
            return Err(SinkError::Wal(f));
        }
        Ok(())
    }

    /// Spawns the worker from the sink's current hashing state. The worker gets COPIES: the sink's own state is reset
    /// only once the spawn succeeded, so a failed spawn leaves it untouched and the sink continues `Inline` exactly
    /// as a sink that never tried (ruling R7).
    fn spawn_worker(&mut self, buf_len: usize) -> Hashing {
        let args = WorkerArgs {
            link: self.run.sender.link(),
            file_id: self.file_id.clone(),
            hasher: self.hasher.clone(),
            digests: self.digests.clone(),
            range_start: self.range_start,
            range_chunks: self.run.policy.range_chunks.max(1),
            extra_buffers: (0..POOL - 1).map(|_| vec![0u8; buf_len]).collect(),
            #[cfg(test)]
            inject: super::worker::INJECT.get(),
        };
        match Worker::spawn(args) {
            Ok(worker) => {
                self.hasher = ChunkHasher::new();
                self.digests.clear();
                Hashing::Worker(worker)
            }
            Err(_) => Hashing::Inline,
        }
    }

    /// Hands over a pool buffer whose first `n` bytes were just written to the destination, and returns a buffer
    /// (at its full length) to read the next bytes into. Inline, it hashes them here and returns `buf`. With a worker:
    /// checks the log's fault, then the worker's, queues `buf`, and waits for a free buffer, beating every
    /// `SEND_RETRY` and re-checking both faults. A log fault stops the file within one buffer; a fault only the worker
    /// saw, within `POOL` buffers.
    pub(crate) fn submit(
        &mut self,
        buf: Vec<u8>,
        n: usize,
        beat: &Heartbeat<'_>,
    ) -> Result<Vec<u8>, SinkError> {
        if matches!(self.hashing, Hashing::Pending) {
            self.hashing = self.spawn_worker(buf.len());
        }
        let Hashing::Worker(worker) = &self.hashing else {
            self.wrote(&buf[..n], beat)?;
            return Ok(buf);
        };
        self.check_faults(worker)?;
        if worker.submit(buf, n).is_err() {
            return Err(SinkError::Wal(self.worker_gone(Some(worker))));
        }
        loop {
            match worker.recv_free(SEND_RETRY) {
                Ok(free) => return Ok(free),
                Err(RecvTimeoutError::Timeout) => {
                    beat().map_err(SinkError::Heartbeat)?;
                    self.check_faults(worker)?;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(SinkError::Wal(self.worker_gone(Some(worker))));
                }
            }
        }
    }

    /// Sends the digests gathered so far as one `Range` ending at `end`.
    fn send_range(&mut self, end: u64, beat: &Heartbeat<'_>) -> Result<(), SinkError> {
        if self.digests.is_empty() {
            return Ok(());
        }
        let range = Range {
            file_id: self.file_id.clone(),
            attempt_id: ATTEMPT_ID,
            range_start: self.range_start,
            range_end: end,
            digests: std::mem::take(&mut self.digests),
        };
        self.range_start = end;
        self.send(Msg::Range(range), beat)
    }

    /// Records digests `full` proved for `[0, valid_through)`: one `Range` per `policy.range_chunks` chunks, from
    /// offset 0, the last ending at `valid_through`. Call once, right after `begin`, before the first `wrote`.
    /// `range_start`, the hasher and the pending digests are left as `begin` set them.
    pub(crate) fn record_proven(
        &mut self,
        digests: &[Digest],
        beat: &Heartbeat<'_>,
    ) -> Result<(), SinkError> {
        let valid_through = self.range_start;
        debug_assert!(
            digests.len() as u64 == valid_through.div_ceil(CHUNK_SIZE),
            "one proven digest per chunk of the prefix"
        );
        debug_assert!(
            self.hasher.bytes_seen() == valid_through.div_ceil(CHUNK_SIZE) * CHUNK_SIZE
                && self.digests.is_empty(),
            "record_proven comes right after begin, before the first wrote"
        );
        let per_range = self.run.policy.range_chunks.max(1);
        for (group, piece) in digests.chunks(per_range).enumerate() {
            let start = (group * per_range) as u64 * CHUNK_SIZE;
            let end = (start + piece.len() as u64 * CHUNK_SIZE).min(valid_through);
            let range = Range {
                file_id: self.file_id.clone(),
                attempt_id: ATTEMPT_ID,
                range_start: start,
                range_end: end,
                digests: piece.to_vec(),
            };
            self.send(Msg::Range(range), beat)?;
        }
        Ok(())
    }

    /// Feeds source bytes, hashing them on this thread. Returns `SinkError::Wal` at the first completed chunk after
    /// the log died. Legal only before the first `submit` that spawned a worker (the carry), or in `Inline` mode.
    pub(crate) fn wrote(&mut self, bytes: &[u8], beat: &Heartbeat<'_>) -> Result<(), SinkError> {
        debug_assert!(
            !matches!(self.hashing, Hashing::Worker(_)),
            "wrote after the worker took over the hashing"
        );
        for (index, digest) in self.hasher.update(bytes) {
            self.digests.push(digest);
            if self.digests.len() >= self.run.policy.range_chunks.max(1) {
                self.send_range((index + 1) * CHUNK_SIZE, beat)?;
            }
            // A dead log keeps its thread alive and `send` keeps succeeding, so ask it: stop the file at once.
            if let Some(fault) = self.run.sender.wal_fault() {
                return Err(SinkError::Wal(fault));
            }
        }
        Ok(())
    }

    /// Sends the last digests (the final short chunk included), closes the file, and waits for the writer's
    /// answer. A worker first drains every queued buffer and hands its hashing state back (waiting beats every
    /// `SEND_RETRY`). The sink is ended whatever the result: its `Close` was sent (or the writer is gone).
    pub(crate) fn complete(&mut self, beat: &Heartbeat<'_>) -> Result<(), SinkError> {
        self.ended = true;
        if let Hashing::Worker(worker) = std::mem::replace(&mut self.hashing, Hashing::Inline) {
            match worker.finish(beat) {
                Ok(end) => {
                    self.hasher = end.hasher;
                    self.digests = end.digests;
                    self.range_start = end.range_start;
                }
                Err(WorkerFail::Fault(f)) => return Err(SinkError::Wal(f)),
                Err(WorkerFail::Heartbeat(e)) => return Err(SinkError::Heartbeat(e)),
                Err(WorkerFail::Gone) => return Err(SinkError::Wal(self.worker_gone(None))),
            }
        }
        let end = self.hasher.bytes_seen();
        let hasher = std::mem::replace(&mut self.hasher, ChunkHasher::new());
        if let Some((_, digest)) = hasher.finish() {
            self.digests.push(digest);
        }
        self.send_range(end, beat)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(Msg::Close { file_id: self.file_id.clone(), ack: Some(tx) }, beat)?;
        loop {
            match rx.recv_timeout(SEND_RETRY) {
                Ok(None) => return Ok(()),
                Ok(Some(fault)) => {
                    // A `Close` ack alone cannot say whose fault it is: the log's failure is the only tell.
                    return Err(match self.run.sender.wal_fault() {
                        Some(wal) => SinkError::Wal(wal),
                        None => SinkError::Data(fault),
                    });
                }
                Err(RecvTimeoutError::Timeout) => beat().map_err(SinkError::Heartbeat)?,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(SinkError::Wal(self.wal_fault_or_gone()));
                }
            }
        }
    }

    /// Stops and joins the worker (if any), then discards the file's pending records and duplicate. Nothing is
    /// flushed, but it waits for the worker, beating every `SEND_RETRY`, so no `Range` can follow the `Discard`.
    /// Errors are ignored: a heartbeat failure or a dead writer means the run abandons the log, which closes every
    /// duplicate. A no-op once the sink has ended.
    pub(crate) fn abort(&mut self, beat: &Heartbeat<'_>) {
        if self.ended {
            return;
        }
        self.ended = true;
        // The joined worker stays in place (`Drop`'s join is then a no-op), so its state stays observable.
        if let Hashing::Worker(worker) = &mut self.hashing {
            worker.cancel_and_join(Some(beat));
        }
        let _ = self.send(Msg::Discard { file_id: self.file_id.clone() }, beat);
    }
}

#[cfg(test)]
mod tests {
    use super::super::worker::{FORCE, Force, INJECT, Inject};
    use super::super::{CHECKPOINT_MIN_LEN, CheckpointOpenError, CheckpointPolicy, RANGE_CHUNKS};
    use super::*;
    use crate::fault_fs::{FakeDirHandle, FakeHandle, FaultFs};
    use crate::wal::record::{Body, Decoded, Header, decode_next};
    use crate::wal::writer::WalConfig;
    use flux_fs::{DestinationRoot, Durability, FileHandle, FileIdentity, FileSystem, FileType};
    use std::path::Path;
    use std::time::Duration;

    const MIB: usize = 1 << 20;
    const WAL_PATH: &str = "/ws/wal/000001";
    const HOUR: Duration = Duration::from_secs(3600);

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

    fn fc(run: &Checkpoints<FakeDirHandle>) -> FileCkpt<'_, FakeDirHandle> {
        FileCkpt { run, file_id: b"f".to_vec(), destination: b"/ws/f".to_vec() }
    }

    fn meta(len: u64) -> Metadata {
        Metadata {
            len,
            file_type: FileType::File,
            permissions: None,
            modified: None,
            identity: FileIdentity::Unavailable,
            nlink: Some(1),
        }
    }

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn records(fs: &FaultFs) -> Vec<(Header, Body)> {
        let bytes = fs.read_file(WAL_PATH).expect("the segment exists");
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

    fn ranges(fs: &FaultFs) -> Vec<Range> {
        records(fs)
            .into_iter()
            .filter_map(|(_, b)| match b {
                Body::Range(r) => Some(r),
                Body::FileBegin(_) => None,
            })
            .collect()
    }

    fn drain(run: &Checkpoints<FakeDirHandle>, beat: &Heartbeat<'_>) {
        let (tx, rx) = mpsc::sync_channel(1);
        run.sender.send(Msg::Drain(tx), beat).unwrap();
        rx.recv().unwrap();
    }

    #[test]
    fn the_constants_are_pinned() {
        assert_eq!(CHECKPOINT_MIN_LEN, 64 << 20);
        assert_eq!(RANGE_CHUNKS, 16);
        let d = CheckpointPolicy::DEFAULT;
        assert_eq!(d.min_len, CHECKPOINT_MIN_LEN);
        assert_eq!(d.range_chunks, RANGE_CHUNKS);
        assert_eq!(d.wal.flush_interval, WalConfig::DEFAULT.flush_interval);
        assert_eq!(d.wal.flush_bytes, WalConfig::DEFAULT.flush_bytes);
        assert_eq!(d.wal.channel_cap, WalConfig::DEFAULT.channel_cap);
        assert_eq!(d.wal.durability, WalConfig::DEFAULT.durability);
    }

    #[test]
    fn ranges_coalesce_every_range_chunks_completed_chunks() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(4, Durability::Normal));
        let total = 8 * MIB + 5;
        let src = data(total);
        let mut sink = FileSink::begin(&fc(&run), &meta(total as u64), 0, None, beat).unwrap();
        for piece in src[..8 * MIB].chunks(MIB / 4) {
            sink.wrote(piece, beat).unwrap();
        }
        sink.wrote(&src[8 * MIB..], beat).unwrap();
        sink.complete(beat).unwrap();
        drop(sink);
        assert!(run.finish(beat).is_clean());
        let r = ranges(&fs);
        let shape: Vec<(u64, u64, usize)> =
            r.iter().map(|r| (r.range_start, r.range_end, r.digests.len())).collect();
        assert_eq!(
            shape,
            [
                (0, 4 * MIB as u64, 4),
                (4 * MIB as u64, 8 * MIB as u64, 4),
                (8 * MIB as u64, 8 * MIB as u64 + 5, 1),
            ]
        );
        assert!(r.iter().all(|r| r.file_id == b"f" && r.attempt_id == 1));
    }

    #[test]
    fn the_digests_are_the_blake3_of_each_source_chunk() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(4, Durability::Normal));
        let total = 3 * MIB + 100;
        let src = data(total);
        let mut sink = FileSink::begin(&fc(&run), &meta(total as u64), 0, None, beat).unwrap();
        for piece in src.chunks(300_000) {
            sink.wrote(piece, beat).unwrap();
        }
        sink.complete(beat).unwrap();
        drop(sink);
        assert!(run.finish(beat).is_clean());
        let got: Vec<Digest> = ranges(&fs).into_iter().flat_map(|r| r.digests).collect();
        let want: Vec<Digest> = src.chunks(MIB).map(|c| *blake3::hash(c).as_bytes()).collect();
        assert_eq!(want.len(), 4);
        assert_eq!(got, want);
    }

    #[test]
    fn begin_records_the_source_and_resume_numbers_chunks_from_valid_through() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(4, Durability::Normal));
        let mut m = meta(3 * MIB as u64);
        m.modified = Some(UNIX_EPOCH + Duration::new(1_700_000_000, 700));
        m.identity = FileIdentity::Strong(flux_fs::ObjectId { volume: 3, index: 9 });
        let src = data(2 * MIB);
        let mut sink = FileSink::begin(&fc(&run), &m, MIB as u64, None, beat).unwrap();
        sink.wrote(&src[MIB..], beat).unwrap();
        sink.complete(beat).unwrap();
        drop(sink);
        assert!(run.finish(beat).is_clean());
        let recs = records(&fs);
        let Body::FileBegin(b) = &recs[0].1 else { panic!("first record is not a FileBegin") };
        assert_eq!(b.file_id, b"f");
        assert_eq!(b.attempt_id, 1);
        assert_eq!(b.destination, b"/ws/f");
        assert_eq!(b.source_len, 3 * MIB as u64);
        assert_eq!(b.source_mtime, Some((1_700_000_000, 700)));
        assert_eq!(b.source_identity, b"strong:3:9");
        assert_eq!(b.chunk_size, CHUNK_SIZE);
        assert_eq!(b.valid_through, MIB as u64);
        let r = ranges(&fs);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].range_start, r[0].range_end), (MIB as u64, 2 * MIB as u64));
        assert_eq!(r[0].digests, vec![*blake3::hash(&src[MIB..]).as_bytes()]);
    }

    #[test]
    fn begin_accepts_a_valid_through_equal_to_the_source_length() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(4, Durability::Normal));
        let len = 2 * MIB as u64 + 5;
        // A fully validated partial whose last chunk is short: nothing is left to hash, so nothing is numbered.
        let mut sink = FileSink::begin(&fc(&run), &meta(len), len, None, beat).unwrap();
        sink.complete(beat).unwrap();
        drop(sink);
        assert!(run.finish(beat).is_clean());
        let recs = records(&fs);
        let Body::FileBegin(b) = &recs[0].1 else { panic!("first record is not a FileBegin") };
        assert_eq!(b.valid_through, len);
        assert_eq!(b.source_len, len);
        assert!(ranges(&fs).is_empty(), "no Range: no bytes arrived");
    }

    #[test]
    #[should_panic(expected = "valid_through must be a chunk boundary or the source length")]
    fn begin_refuses_a_misaligned_valid_through_other_than_the_source_length() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(4, Durability::Normal));
        // Neither a chunk boundary nor the source length (3 MiB).
        let _ = FileSink::begin(&fc(&run), &meta(3 * MIB as u64), MIB as u64 + 1, None, beat);
    }

    #[test]
    fn wrote_stops_at_the_first_chunk_after_the_wal_died() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let run = open_run(&fs, policy(4, Durability::Normal));
        let mut sink = FileSink::begin(
            &fc(&run),
            &meta(8 * MIB as u64),
            0,
            Some(p.duplicate().unwrap()),
            beat,
        )
        .unwrap();
        fs.fail("create_new", Code::DiskFull);
        // The FileBegin is flushed by the drain; the segment create fails, killing the log.
        drain(&run, beat);
        assert!(run.sender.wal_fault().is_some());
        let src = data(2 * MIB);
        // Less than one chunk: no chunk completes, so nothing is checked yet.
        sink.wrote(&src[..MIB - 1], beat).unwrap();
        // One more byte completes chunk 0, which is fewer chunks than a range (4): the check is per chunk.
        match sink.wrote(&src[MIB - 1..MIB], beat) {
            Err(SinkError::Wal(f)) => assert_eq!(f.code, Code::DiskFull),
            other => panic!("expected Wal, got {other:?}"),
        }
        sink.abort(beat);
        drop(sink);
        run.abandon();
    }

    #[test]
    fn complete_waits_for_the_close_ack_and_reports_a_data_sync_failure_as_data() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        fs.fail("sync_all", Code::StrictDurabilityUnavailable);
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let run = open_run(&fs, policy(4, Durability::Strict));
        let mut sink =
            FileSink::begin(&fc(&run), &meta(MIB as u64), 0, Some(p.duplicate().unwrap()), beat)
                .unwrap();
        sink.wrote(&data(MIB), beat).unwrap();
        match sink.complete(beat) {
            Err(SinkError::Data(f)) => assert_eq!(f.code, Code::StrictDurabilityUnavailable),
            other => panic!("expected Data, got {other:?}"),
        }
        // complete ended the sink even though it failed, so abort is a no-op and Drop is quiet.
        sink.abort(beat);
        drop(sink);
        assert!(run.finish(beat).wal.is_none());
        drop(p);
    }

    #[test]
    fn a_wal_create_failure_is_reported_as_wal() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let run = open_run(&fs, policy(4, Durability::Strict));
        let mut sink =
            FileSink::begin(&fc(&run), &meta(MIB as u64), 0, Some(p.duplicate().unwrap()), beat)
                .unwrap();
        // Armed after the partial's writer exists, so the one-shot hits the segment's create.
        fs.fail("create_new", Code::DiskFull);
        sink.wrote(&data(MIB), beat).unwrap();
        match sink.complete(beat) {
            Err(SinkError::Wal(f)) => assert_eq!(f.code, Code::DiskFull),
            other => panic!("expected Wal, got {other:?}"),
        }
        drop(sink);
        assert!(run.finish(beat).wal.is_some());
        drop(p);
    }

    #[test]
    fn open_maps_a_corrupt_segment_to_corrupt_and_an_unavailable_identity_to_unavailable() {
        let (fs, _dir) = ws();
        fs.create_dir(Path::new("/ws/wal")).unwrap();
        fs.write_file(WAL_PATH, &[0xAB; 64]);
        let a = fs.destination_root(Path::new("/ws")).unwrap();
        let b = fs.destination_root(Path::new("/ws")).unwrap();
        match Checkpoints::open(a, b, "op", 1, policy(4, Durability::Normal)) {
            Err(CheckpointOpenError::Corrupt { .. }) => {}
            other => panic!("expected Corrupt, got {:?}", other.err()),
        }

        let (fs, _dir) = ws();
        fs.create_dir(Path::new("/ws/wal")).unwrap();
        fs.write_file(WAL_PATH, b"");
        fs.set_identity(WAL_PATH, FileIdentity::Unavailable);
        let a = fs.destination_root(Path::new("/ws")).unwrap();
        let b = fs.destination_root(Path::new("/ws")).unwrap();
        match Checkpoints::open(a, b, "op", 1, policy(4, Durability::Normal)) {
            Err(CheckpointOpenError::Unavailable(_)) => {}
            other => panic!("expected Unavailable, got {:?}", other.err()),
        }
    }

    #[test]
    fn finish_flushes_and_abandon_does_not() {
        let beat: &Heartbeat = &|| Ok(());
        let (fs, _dir) = ws();
        let run = open_run(&fs, policy(1, Durability::Normal));
        let mut sink = FileSink::begin(&fc(&run), &meta(MIB as u64), 0, None, beat).unwrap();
        sink.wrote(&data(MIB), beat).unwrap();
        // Pending records only: no Close, no drain, no timer. The test ends the sink by hand.
        sink.ended = true;
        drop(sink);
        assert!(run.finish(beat).is_clean());
        assert_eq!(records(&fs).len(), 2);

        let (fs, _dir) = ws();
        let run = open_run(&fs, policy(1, Durability::Normal));
        let mut sink = FileSink::begin(&fc(&run), &meta(MIB as u64), 0, None, beat).unwrap();
        sink.wrote(&data(MIB), beat).unwrap();
        sink.ended = true;
        drop(sink);
        run.abandon();
        assert!(fs.read_file(WAL_PATH).is_none());
    }

    #[test]
    fn checkpoints_mark_abandoned_reaches_the_writer() {
        let beat: &Heartbeat = &|| Ok(());
        let eager = || CheckpointPolicy {
            wal: WalConfig { flush_bytes: 1, ..policy(1, Durability::Normal).wal },
            ..policy(1, Durability::Normal)
        };
        // The control: eager, unmarked, the range reaches the segment.
        let (fs, _dir) = ws();
        let run = open_run(&fs, eager());
        let mut sink = FileSink::begin(&fc(&run), &meta(MIB as u64), 0, None, beat).unwrap();
        sink.wrote(&data(MIB), beat).unwrap();
        for _ in 0..5000 {
            if fs.read_file(WAL_PATH).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(fs.read_file(WAL_PATH).is_some(), "the unmarked eager run flushed");
        sink.abort(beat);
        drop(sink);
        run.abandon();
        // Marked first: nothing is ever written.
        let (fs, _dir) = ws();
        let run = open_run(&fs, eager());
        run.mark_abandoned();
        if let Ok(mut sink) = FileSink::begin(&fc(&run), &meta(MIB as u64), 0, None, beat) {
            let _ = sink.wrote(&data(MIB), beat);
            // Time for an unflagged writer to flush what was queued.
            std::thread::sleep(std::time::Duration::from_millis(100));
            sink.abort(beat);
            drop(sink);
        }
        run.abandon();
        assert!(fs.read_file(WAL_PATH).is_none());
    }

    #[test]
    fn abort_discards_pending_records_and_drops_the_duplicate() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let run = open_run(&fs, policy(4, Durability::Strict));
        let mut sink =
            FileSink::begin(&fc(&run), &meta(MIB as u64), 0, Some(p.duplicate().unwrap()), beat)
                .unwrap();
        sink.wrote(&data(MIB), beat).unwrap();
        let drops = |fs: &FaultFs| {
            fs.calls().iter().filter(|c| c.replace('\\', "/") == "drop_writer(/ws/p)").count()
        };
        assert_eq!(drops(&fs), 0);
        sink.abort(beat);
        // A drain is answered after the Discard before it was handled, so the duplicate is gone by now, with
        // no Stop (which would also clear it). The drain's own flush has nothing left to write.
        drain(&run, beat);
        assert_eq!(drops(&fs), 1);
        sink.abort(beat);
        drop(sink);
        assert!(run.finish(beat).is_clean());
        assert_eq!(drops(&fs), 1);
        assert!(fs.read_file(WAL_PATH).is_none(), "the aborted file's records were never written");
        drop(p);
    }

    fn chunk_digests(src: &[u8]) -> Vec<Digest> {
        src.chunks(MIB).map(|c| *blake3::hash(c).as_bytes()).collect()
    }

    fn range_shape(fs: &FaultFs) -> Vec<(u64, u64, usize)> {
        ranges(fs).iter().map(|r| (r.range_start, r.range_end, r.digests.len())).collect()
    }

    #[test]
    fn record_proven_sends_ranges_from_chunk_zero_up_to_valid_through() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(2, Durability::Normal));
        let src = data(5 * MIB);
        let d = chunk_digests(&src[..3 * MIB]);
        let mut sink =
            FileSink::begin(&fc(&run), &meta(5 * MIB as u64), 3 * MIB as u64, None, beat).unwrap();
        sink.record_proven(&d, beat).unwrap();
        sink.complete(beat).unwrap();
        drop(sink);
        assert!(run.finish(beat).is_clean());
        let recs = records(&fs);
        assert!(matches!(recs[0].1, Body::FileBegin(_)));
        assert_eq!(recs.len(), 3, "a FileBegin and two Ranges, nothing else");
        let r = ranges(&fs);
        assert_eq!(
            (r[0].range_start, r[0].range_end, r[0].digests.clone()),
            (0, 2 * MIB as u64, vec![d[0], d[1]])
        );
        assert_eq!(
            (r[1].range_start, r[1].range_end, r[1].digests.clone()),
            (2 * MIB as u64, 3 * MIB as u64, vec![d[2]])
        );
        assert!(r.iter().all(|r| r.file_id == b"f" && r.attempt_id == ATTEMPT_ID));
    }

    #[test]
    fn record_proven_then_wrote_numbers_the_copied_chunks_after_the_prefix() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(2, Durability::Normal));
        let src = data(5 * MIB);
        let d = chunk_digests(&src[..3 * MIB]);
        let mut sink =
            FileSink::begin(&fc(&run), &meta(5 * MIB as u64), 3 * MIB as u64, None, beat).unwrap();
        sink.record_proven(&d, beat).unwrap();
        sink.wrote(&src[3 * MIB..], beat).unwrap();
        sink.complete(beat).unwrap();
        drop(sink);
        assert!(run.finish(beat).is_clean());
        let m = MIB as u64;
        assert_eq!(range_shape(&fs), [(0, 2 * m, 2), (2 * m, 3 * m, 1), (3 * m, 5 * m, 2)]);
        let a = fs.destination_root(Path::new("/ws")).unwrap();
        let b = fs.destination_root(Path::new("/ws")).unwrap();
        let run2 = Checkpoints::open(a, b, "op", 2, policy(2, Durability::Normal))
            .unwrap_or_else(|e| panic!("open: {e:?}"));
        let state = run2.load(b"f").expect("loads").expect("the file is indexed");
        run2.abandon();
        let want: Vec<(u64, Digest)> =
            chunk_digests(&src).into_iter().enumerate().map(|(i, d)| (i as u64, d)).collect();
        let got: Vec<(u64, Digest)> = state.digests.into_iter().collect();
        assert_eq!(got, want);
    }

    #[test]
    fn record_proven_at_the_source_length_ends_the_last_range_there() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(2, Durability::Normal));
        let len = 2 * MIB + 777;
        let src = data(len);
        let d = chunk_digests(&src);
        assert_eq!(d.len(), 3);
        let mut sink =
            FileSink::begin(&fc(&run), &meta(len as u64), len as u64, None, beat).unwrap();
        sink.record_proven(&d, beat).unwrap();
        sink.complete(beat).unwrap();
        drop(sink);
        assert!(run.finish(beat).is_clean());
        let m = MIB as u64;
        assert_eq!(range_shape(&fs), [(0, 2 * m, 2), (2 * m, 2 * m + 777, 1)]);
        let r = ranges(&fs);
        assert_eq!(r[1].digests, vec![d[2]]);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a FileSink must end through complete or abort")]
    fn a_sink_dropped_without_ending_trips_the_debug_assertion() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(4, Durability::Normal));
        let sink = FileSink::begin(&fc(&run), &meta(MIB as u64), 0, None, beat).unwrap();
        drop(sink);
    }

    // ---- cut 9h: the hashing worker behind `submit` ----

    const PIECE: usize = MIB / 4;
    /// A generous bound for anything that should happen at once; past it the tests' heartbeats fail, so a hang ends
    /// the test with `SinkError::Heartbeat` instead of hanging the suite.
    const DEADLINE: Duration = Duration::from_secs(10);
    /// How long each `sync_all` of a held run is delayed (the plan's "Test mechanisms" recipe).
    const HOLD: Duration = Duration::from_millis(300);

    /// The mode seam for this thread; reset to `Auto` when dropped, so a failed assert cannot leak it.
    struct ForceGuard;

    impl ForceGuard {
        fn set(f: Force) -> ForceGuard {
            FORCE.set(f);
            ForceGuard
        }
    }

    impl Drop for ForceGuard {
        fn drop(&mut self) {
            FORCE.set(Force::Auto);
            INJECT.set(Inject::default());
        }
    }

    /// A heartbeat that succeeds until `DEADLINE` has passed since it was made, then fails.
    fn deadline_beat() -> impl Fn() -> flux_fs::Result<()> {
        let end = std::time::Instant::now() + DEADLINE;
        move || {
            if std::time::Instant::now() < end {
                Ok(())
            } else {
                Err(FsError::new(Code::IoError, io::Error::other("test deadline passed")))
            }
        }
    }

    /// One file through a sink: a proven prefix of `prefix` bytes (`record_proven`), then `carry` bytes through
    /// `wrote`, then the rest through `submit` in `PIECE` buffers. Returns the logged ranges and the workers spawned.
    fn run_file(
        force: Force,
        src: &[u8],
        prefix: usize,
        carry: usize,
        range_chunks: usize,
    ) -> (Vec<Range>, usize) {
        let _force = ForceGuard::set(force);
        let spawned = Worker::spawned_for_tests();
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let run = open_run(&fs, policy(range_chunks, Durability::Normal));
        let mut sink =
            FileSink::begin(&fc(&run), &meta(src.len() as u64), prefix as u64, None, beat).unwrap();
        if prefix > 0 {
            sink.record_proven(&chunk_digests(&src[..prefix]), beat).unwrap();
        }
        let carry_end = (prefix + carry).min(src.len());
        sink.wrote(&src[prefix..carry_end], beat).unwrap();
        let mut buf = vec![0u8; PIECE];
        for piece in src[carry_end..].chunks(PIECE) {
            buf[..piece.len()].copy_from_slice(piece);
            buf = sink.submit(buf, piece.len(), beat).unwrap();
            assert_eq!(buf.len(), PIECE, "a pool buffer keeps its full length");
        }
        sink.complete(beat).unwrap();
        drop(sink);
        assert!(run.finish(beat).is_clean());
        (ranges(&fs), Worker::spawned_for_tests() - spawned)
    }

    /// Each logged digest is BLAKE3 of its own source chunk, and together they cover the whole source in order.
    fn assert_oracle(r: &[Range], src: &[u8], what: &str) {
        let got: Vec<Digest> = r.iter().flat_map(|r| r.digests.clone()).collect();
        assert_eq!(got, chunk_digests(src), "{what}: digests differ from the blake3 oracle");
        let mut at = 0;
        for range in r {
            assert_eq!(range.range_start, at, "{what}: ranges are contiguous");
            at = range.range_end;
        }
        assert_eq!(at, src.len() as u64, "{what}: the last range ends at the source length");
    }

    /// A run whose writer is held (the plan's recipe): Strict, every `sync_all` delayed by `HOLD`, a one-slot
    /// channel, `range_chunks = 1`; the sink is begun with the partial's duplicate, then a `Flush` the writer is stuck
    /// in. The returned handle is the partial, kept open for the duplicate's sake.
    fn held_run(fs: &FaultFs) -> (Checkpoints<FakeDirHandle>, FakeHandle) {
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        fs.delay("sync_all", HOLD);
        let mut pol = policy(1, Durability::Strict);
        pol.wal.channel_cap = 1;
        (open_run(fs, pol), p)
    }

    fn held_sink<'a>(
        run: &'a Checkpoints<FakeDirHandle>,
        p: &FakeHandle,
        len: usize,
        beat: &Heartbeat<'_>,
    ) -> FileSink<'a, FakeDirHandle> {
        let sink =
            FileSink::begin(&fc(run), &meta(len as u64), 0, Some(p.duplicate().unwrap()), beat)
                .unwrap();
        run.sender.send(Msg::Flush, beat).unwrap();
        sink
    }

    fn worker_of<'s>(sink: &'s FileSink<'_, FakeDirHandle>) -> &'s Worker {
        match &sink.hashing {
            Hashing::Worker(w) => w,
            Hashing::Inline => panic!("the sink is Inline"),
            Hashing::Pending => panic!("the sink is Pending"),
        }
    }

    #[test]
    fn the_worker_log_equals_the_inline_log() {
        let sizes = [MIB - 1, MIB, 3 * MIB + MIB / 2, 8 * MIB, 8 * MIB + 5];
        // (proven prefix, carry): none, a 2 MiB proven prefix, a 300 KiB carry.
        let cases = [(0, 0), (2 * MIB, 0), (0, 300 * 1024)];
        for size in sizes {
            let src = data(size);
            for (prefix, carry) in cases {
                if prefix > size {
                    continue;
                }
                let what = format!("size {size}, prefix {prefix}, carry {carry}");
                let (inline, inline_spawned) = run_file(Force::OneCpu, &src, prefix, carry, 4);
                let (worker, worker_spawned) = run_file(Force::Auto, &src, prefix, carry, 4);
                assert_eq!(inline_spawned, 0, "{what}: Inline spawned a worker");
                assert_eq!(
                    worker_spawned,
                    usize::from(prefix + carry < size),
                    "{what}: the worker run spawns one worker when a submit happens (needs > 1 CPU)"
                );
                assert_eq!(worker, inline, "{what}: the worker log differs from the inline log");
                assert_oracle(&worker, &src, &what);
            }
        }
    }

    #[test]
    fn complete_without_a_submit_runs_inline() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let _force = ForceGuard::set(Force::Auto);
        let spawned = Worker::spawned_for_tests();
        let run = open_run(&fs, policy(4, Durability::Normal));
        let len = 300 * 1024;
        let src = data(len);
        let mut sink = FileSink::begin(&fc(&run), &meta(len as u64), 0, None, beat).unwrap();
        // The whole remaining file is the carry: no submit, so no worker.
        sink.wrote(&src, beat).unwrap();
        // `complete` returns Ok only after the Close was acknowledged; the Close flushed the file's records.
        sink.complete(beat).unwrap();
        assert_eq!(
            range_shape(&fs),
            [(0, len as u64, 1)],
            "the final digest was flushed by the Close"
        );
        drop(sink);
        assert!(run.finish(beat).is_clean());
        assert_oracle(&ranges(&fs), &src, "carry only");
        assert_eq!(Worker::spawned_for_tests(), spawned, "no flux-hash thread was spawned");
    }

    #[test]
    fn complete_drains_every_queued_buffer_before_close() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let _force = ForceGuard::set(Force::Auto);
        let (run, p) = held_run(&fs);
        let len = 4 * MIB + 5;
        let src = data(len);
        let mut sink = held_sink(&run, &p, len, beat);
        let mut buf = vec![0u8; PIECE];
        // The writer is held, so the worker's second Range finds the one slot full and the queue backs up: more
        // buffers are submitted than the worker has consumed.
        for piece in src.chunks(PIECE) {
            buf[..piece.len()].copy_from_slice(piece);
            buf = sink.submit(buf, piece.len(), beat).unwrap();
        }
        assert!(matches!(sink.hashing, Hashing::Worker(_)));
        sink.complete(beat).unwrap();
        // Read BEFORE `finish`: only the Close's flush can have put the last Range on disk by now.
        let r = ranges(&fs);
        assert_eq!(
            r.last().map(|r| r.range_end),
            Some(len as u64),
            "the last Range preceded the Close"
        );
        assert_oracle(&r, &src, "held writer");
        drop(sink);
        assert!(run.finish(beat).is_clean());
        assert_eq!(ranges(&fs), r, "nothing for the file followed the Close");
        drop(p);
    }

    #[test]
    fn abort_joins_the_worker_and_nothing_follows_discard() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let _force = ForceGuard::set(Force::Auto);
        let (run, p) = held_run(&fs);
        let len = 8 * MIB;
        let src = data(len);
        let mut sink = held_sink(&run, &p, len, beat);
        let mut buf = vec![0u8; PIECE];
        // Three chunks' worth: the worker sends Range 0 into the slot and is retrying Range 1 with buffers queued.
        for piece in src[..3 * MIB].chunks(PIECE) {
            buf[..piece.len()].copy_from_slice(piece);
            buf = sink.submit(buf, piece.len(), beat).unwrap();
        }
        sink.abort(beat);
        assert!(worker_of(&sink).is_finished(), "abort returned with the worker alive");
        drop(sink);
        drain(&run, beat);
        assert!(run.finish(beat).is_clean());
        // The FileBegin was flushed by the held Flush; every Range was discarded and none followed the Discard.
        let recs = records(&fs);
        assert!(matches!(recs[0].1, Body::FileBegin(_)));
        assert!(
            ranges(&fs).is_empty(),
            "a Range reached the log after the Discard: {:?}",
            range_shape(&fs)
        );
        drop(p);
    }

    #[test]
    fn a_full_channel_does_not_starve_the_heartbeat() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (fs, _dir) = ws();
        let ok: &Heartbeat = &|| Ok(());
        let (run, p) = held_run(&fs);
        let len = 8 * MIB;
        let src = data(len);
        let beats = AtomicUsize::new(0);
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|s| {
            let helper = s.spawn(|| {
                let ok: &Heartbeat = &|| Ok(());
                let tick = deadline_beat();
                let beat: &Heartbeat = &|| {
                    beats.fetch_add(1, Ordering::SeqCst);
                    tick()
                };
                let mut sink = held_sink(&run, &p, len, ok);
                let mut buf = vec![0u8; PIECE];
                for piece in src.chunks(PIECE) {
                    buf[..piece.len()].copy_from_slice(piece);
                    buf = match sink.submit(buf, piece.len(), beat) {
                        Ok(b) => b,
                        Err(e) => {
                            sink.abort(ok);
                            panic!("submit: {e:?}");
                        }
                    };
                }
                done.store(true, Ordering::SeqCst);
                sink.complete(ok).unwrap();
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                if beats.load(Ordering::SeqCst) >= 3 {
                    assert!(!done.load(Ordering::SeqCst), "the beats came after the submits ended");
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "the pool wait never beat 3 times");
                std::thread::sleep(Duration::from_millis(2));
            }
            helper.join().unwrap();
        });
        assert!(done.load(Ordering::SeqCst));
        assert!(run.finish(ok).is_clean());
        assert_oracle(&ranges(&fs), &src, "held writer");

        // A failing heartbeat ends the pool wait with that error.
        let (fs, _dir) = ws();
        let (run, p) = held_run(&fs);
        let mut sink = held_sink(&run, &p, len, ok);
        let failing: &Heartbeat =
            &|| Err(FsError::new(Code::IoError, io::Error::other("the beat failed")));
        let mut buf = vec![0u8; PIECE];
        let mut got = None;
        for piece in src.chunks(PIECE) {
            buf[..piece.len()].copy_from_slice(piece);
            match sink.submit(buf, piece.len(), failing) {
                Ok(b) => buf = b,
                Err(e) => {
                    got = Some(e);
                    break;
                }
            }
        }
        match got {
            Some(SinkError::Heartbeat(e)) => assert_eq!(e.code, Code::IoError),
            other => panic!("expected Heartbeat, got {other:?}"),
        }
        sink.abort(ok);
        drop(sink);
        run.abandon();
    }

    #[test]
    fn a_dead_log_stops_the_file_within_one_buffer() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let _force = ForceGuard::set(Force::Auto);
        let p = fs.create_new(Path::new("/ws/p")).unwrap();
        let run = open_run(&fs, policy(4, Durability::Normal));
        let mut sink = FileSink::begin(
            &fc(&run),
            &meta(8 * MIB as u64),
            0,
            Some(p.duplicate().unwrap()),
            beat,
        )
        .unwrap();
        fs.fail("create_new", Code::DiskFull);
        // The FileBegin is flushed by the drain; the segment create fails, killing the log.
        drain(&run, beat);
        assert!(run.sender.wal_fault().is_some());
        let src = data(2 * MIB);
        let mut accepted = 0;
        let mut buf = vec![0u8; PIECE];
        let mut got = None;
        for piece in src.chunks(PIECE) {
            buf[..piece.len()].copy_from_slice(piece);
            match sink.submit(buf, piece.len(), beat) {
                Ok(b) => {
                    accepted += 1;
                    buf = b;
                }
                Err(e) => {
                    got = Some(e);
                    break;
                }
            }
        }
        assert!(accepted <= 1, "{accepted} buffers were accepted after the log died");
        match got {
            Some(SinkError::Wal(f)) => assert_eq!(f.code, Code::DiskFull),
            other => panic!("expected Wal, got {other:?}"),
        }
        sink.abort(beat);
        drop(sink);
        run.abandon();
    }

    #[test]
    fn a_worker_only_fault_stops_the_file_within_pool_buffers() {
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let _force = ForceGuard::set(Force::Auto);
        INJECT.set(Inject { fail_send_after_ranges: Some(0), ..Inject::default() });
        let run = open_run(&fs, policy(1, Durability::Normal));
        let len = 16 * MIB;
        let src = data(len);
        let mut sink = FileSink::begin(&fc(&run), &meta(len as u64), 0, None, beat).unwrap();
        // One-chunk buffers (not PIECE): the very first job completes a chunk and its Range send fails, so every
        // accepted submit counts against the bound.
        let mut accepted = 0;
        let mut buf = vec![0u8; MIB];
        let mut got = None;
        for piece in src.chunks(MIB) {
            buf.copy_from_slice(piece);
            match sink.submit(buf, MIB, beat) {
                Ok(b) => {
                    accepted += 1;
                    buf = b;
                }
                Err(e) => {
                    got = Some(e);
                    break;
                }
            }
        }
        assert!(accepted <= POOL, "{accepted} buffers were accepted after the worker failed");
        match got {
            Some(SinkError::Wal(f)) => {
                assert!(f.message.contains("injected worker send failure"), "{f:?}");
            }
            other => panic!("expected Wal, got {other:?}"),
        }
        assert!(run.sender.wal_fault().is_none(), "only the worker failed");
        sink.abort(beat);
        drop(sink);
        run.abandon();
    }

    #[test]
    fn a_fault_during_the_pool_wait_ends_the_file() {
        use std::cell::Cell;
        let (fs, _dir) = ws();
        let ok: &Heartbeat = &|| Ok(());
        let _force = ForceGuard::set(Force::Auto);
        let (run, p) = held_run(&fs);
        let len = 8 * MIB;
        let src = data(len);
        let mut sink = held_sink(&run, &p, len, ok);
        // The writer is inside the Flush's delayed duplicate sync; the segment's create comes after it and fails, so
        // the log dies only once the hold ends, while the submits below are waiting on the exhausted pool.
        fs.fail("create_new", Code::DiskFull);
        let beats = Cell::new(0usize);
        let tick = deadline_beat();
        let beat: &Heartbeat = &|| {
            beats.set(beats.get() + 1);
            tick()
        };
        let mut buf = vec![0u8; PIECE];
        let mut got = None;
        for piece in src.chunks(PIECE) {
            buf[..piece.len()].copy_from_slice(piece);
            match sink.submit(buf, piece.len(), beat) {
                Ok(b) => buf = b,
                Err(e) => {
                    got = Some(e);
                    break;
                }
            }
        }
        match got {
            Some(SinkError::Wal(f)) => assert_eq!(f.code, Code::DiskFull),
            other => panic!("expected Wal, got {other:?}"),
        }
        assert!(beats.get() >= 1, "the fault did not land during a pool wait");
        sink.abort(ok);
        drop(sink);
        run.abandon();
        drop(p);
    }

    #[test]
    fn the_inline_fallback_is_used_for_one_cpu_and_a_failed_spawn() {
        let src = data(8 * MIB + 5);
        // (mode, proven prefix, carry): the failed spawn is tried with hashing state already in the sink (a carry
        // with a partial chunk, a proven prefix, both), so a fallback that lost that state would log differently.
        let cases = [
            (Force::OneCpu, 0, 0),
            (Force::SpawnFails, 0, 0),
            (Force::SpawnFails, 0, 300 * 1024),
            (Force::SpawnFails, 2 * MIB, 0),
            (Force::SpawnFails, 2 * MIB, 300 * 1024),
        ];
        for (force, prefix, carry) in cases {
            let what = format!("{force:?}, prefix {prefix}, carry {carry}");
            let (fallback, spawned) = run_file(force, &src, prefix, carry, 4);
            assert_eq!(spawned, 0, "{what}: a flux-hash thread was spawned");
            let (never, _) = run_file(Force::OneCpu, &src, prefix, carry, 4);
            assert_eq!(
                fallback, never,
                "{what}: the fallback log differs from a never-spawned sink's"
            );
            let (worker, worker_spawned) = run_file(Force::Auto, &src, prefix, carry, 4);
            assert_eq!(worker_spawned, 1, "{what}: the worker run used a worker (needs > 1 CPU)");
            assert_eq!(fallback, worker, "{what}: the fallback log differs from the worker's");
            assert_oracle(&fallback, &src, &what);
        }
    }

    #[test]
    fn a_panicking_worker_is_a_wal_stop_not_a_hang() {
        let (fs, _dir) = ws();
        let ok: &Heartbeat = &|| Ok(());
        let _force = ForceGuard::set(Force::Auto);
        INJECT.set(Inject { panic_on_job: Some(1), ..Inject::default() });
        let run = open_run(&fs, policy(4, Durability::Normal));
        let len = 4 * MIB;
        let src = data(len);
        let tick = deadline_beat();
        let beat: &Heartbeat = &tick;
        let mut sink = FileSink::begin(&fc(&run), &meta(len as u64), 0, None, ok).unwrap();
        let mut buf = vec![0u8; PIECE];
        let mut got = None;
        for piece in src.chunks(PIECE) {
            buf[..piece.len()].copy_from_slice(piece);
            match sink.submit(buf, piece.len(), beat) {
                Ok(b) => buf = b,
                Err(e) => {
                    got = Some(e);
                    break;
                }
            }
        }
        let got = match got {
            Some(e) => e,
            None => sink.complete(beat).expect_err("a panicked worker cannot complete"),
        };
        match got {
            SinkError::Wal(f) => {
                assert_eq!(f.code, Code::WalWriteFailed);
                assert!(f.message.contains("hashing worker stopped"), "{f:?}");
            }
            other => panic!("expected Wal, got {other:?}"),
        }
        sink.abort(ok);
        drop(sink);
        run.abandon();
    }

    #[test]
    fn an_ended_sink_leaves_no_worker() {
        let beat: &Heartbeat = &|| Ok(());
        let _force = ForceGuard::set(Force::Auto);
        for complete in [false, true] {
            let (fs, _dir) = ws();
            let run = open_run(&fs, policy(4, Durability::Normal));
            let start = Worker::live_for_tests();
            let len = 2 * MIB;
            let src = data(len);
            let mut sink = FileSink::begin(&fc(&run), &meta(len as u64), 0, None, beat).unwrap();
            let mut buf = vec![0u8; PIECE];
            for piece in src.chunks(PIECE) {
                buf[..piece.len()].copy_from_slice(piece);
                buf = sink.submit(buf, piece.len(), beat).unwrap();
            }
            assert_eq!(Worker::live_for_tests(), start + 1, "a worker is running");
            if complete {
                sink.complete(beat).unwrap();
            } else {
                sink.abort(beat);
            }
            drop(sink);
            assert_eq!(
                Worker::live_for_tests(),
                start,
                "complete={complete}: a worker outlived its sink"
            );
            assert!(run.finish(beat).is_clean());
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    fn dropping_an_unended_sink_joins_its_worker_then_trips_the_assert() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static LIVE_AT_ASSERT: AtomicUsize = AtomicUsize::new(usize::MAX);
        let (fs, _dir) = ws();
        let beat: &Heartbeat = &|| Ok(());
        let _force = ForceGuard::set(Force::Auto);
        let (run, p) = held_run(&fs);
        let start = Worker::live_for_tests();
        let mut sink = held_sink(&run, &p, 8 * MIB, beat);
        let mut buf = vec![0u8; PIECE];
        // Busy: blocked on the held writer's full channel, so only a cancel ends it before the hold does.
        for piece in data(3 * MIB).chunks(PIECE) {
            buf[..piece.len()].copy_from_slice(piece);
            buf = sink.submit(buf, piece.len(), beat).unwrap();
        }
        assert_eq!(Worker::live_for_tests(), start + 1);
        // The hook runs at the panic, before any unwinding drops the sink's fields: it sees whether the worker was
        // already joined by `Drop` itself.
        let me = std::thread::current().id();
        let prev = std::sync::Arc::new(std::panic::take_hook());
        let prev_in_hook = std::sync::Arc::clone(&prev);
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == me {
                LIVE_AT_ASSERT.store(Worker::live_for_tests(), Ordering::SeqCst);
            }
            prev_in_hook(info);
        }));
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(sink)));
        drop(std::panic::take_hook());
        if let Ok(prev) = std::sync::Arc::try_unwrap(prev) {
            std::panic::set_hook(prev);
        }
        let payload = unwound.expect_err("dropping an unended sink must trip the assert");
        let msg = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        assert!(msg.contains("a FileSink must end through complete or abort"), "{msg}");
        assert_eq!(
            LIVE_AT_ASSERT.load(Ordering::SeqCst),
            start,
            "the worker was still alive when the assert fired"
        );
        run.abandon();
        drop(p);
    }
}
