//! The per-file sink: source bytes in, chunk digests out as `Range` records.

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
const ATTEMPT_ID: u64 = 1;

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
}

impl<D: DirHandle + Send + 'static> Drop for FileSink<'_, D> {
    fn drop(&mut self) {
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

    /// Feeds source bytes. Returns `SinkError::Wal` at the first completed chunk after the log died.
    pub(crate) fn wrote(&mut self, bytes: &[u8], beat: &Heartbeat<'_>) -> Result<(), SinkError> {
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
    /// answer. The sink is ended whatever the result: its `Close` was sent (or the writer is gone).
    pub(crate) fn complete(&mut self, beat: &Heartbeat<'_>) -> Result<(), SinkError> {
        self.ended = true;
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

    /// Discards the file's pending records and duplicate without waiting (nothing is flushed). Errors are ignored: a heartbeat failure or a dead writer means the run
    /// abandons the log, which closes every duplicate. A no-op once the sink has ended.
    pub(crate) fn abort(&mut self, beat: &Heartbeat<'_>) {
        if self.ended {
            return;
        }
        self.ended = true;
        let _ = self.send(Msg::Discard { file_id: self.file_id.clone() }, beat);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{CHECKPOINT_MIN_LEN, CheckpointOpenError, CheckpointPolicy, RANGE_CHUNKS};
    use super::*;
    use crate::fault_fs::{FakeDirHandle, FaultFs};
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
}
