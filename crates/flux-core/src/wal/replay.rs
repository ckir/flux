//! Two-pass WAL replay (cut 9f-1): `scan` indexes the log, `load_file` rebuilds one file's state.

use super::record::{Body, Decoded, FileBegin, decode_next};
use super::{CHUNK_SIZE, Digest};
use flux_fs::FsError;
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read};

/// Bytes requested per read; a record larger than this is assembled across reads.
const READ_WINDOW: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) struct Scan {
    /// File id -> the byte offsets of that file's records, in stream order.
    pub per_file: HashMap<Vec<u8>, Vec<u64>>,
    /// The highest `(generation, sequence)` seen.
    pub last: Option<(u64, u64)>,
    /// Bytes up to and including the last valid record.
    pub valid_len: u64,
}

#[derive(Debug)]
pub(crate) enum WalError {
    Corrupt { at: u64, why: String },
    SequenceConflict { generation: u64, sequence: u64 },
    Unsupported { format: u16, checksum: u8 },
    Io(FsError),
}

fn io_err(e: io::Error) -> WalError {
    WalError::Io(FsError::from_io(e))
}

/// Reads up to one window more into `window` through `chunk`; `false` when the reader is exhausted.
fn fill<R: Read>(reader: &mut R, window: &mut Vec<u8>, chunk: &mut [u8]) -> Result<bool, WalError> {
    loop {
        match reader.read(chunk) {
            Ok(n) => {
                window.extend_from_slice(&chunk[..n]);
                return Ok(n > 0);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_err(e)),
        }
    }
}

/// Pass one: stream the segment, index every file's records, and find where the valid log ends.
pub(crate) fn scan<R: Read>(mut reader: R, operation_id: &[u8]) -> Result<Scan, WalError> {
    let mut out = Scan { per_file: HashMap::new(), last: None, valid_len: 0 };
    let mut seen: HashMap<(u64, u64), [u8; 16]> = HashMap::new();
    let mut window: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; READ_WINDOW];
    let mut start = 0usize;
    let mut offset = 0u64;
    loop {
        let (header, body, len, checksum) = match decode_next(&window[start..]) {
            Decoded::Record { header, body, len, checksum } => (header, body, len, checksum),
            // An empty window or a partial record: more bytes may complete it. Only a read
            // returning nothing makes it the end of the log (ruling 15).
            Decoded::End | Decoded::TornTail => {
                window.drain(..start);
                start = 0;
                if fill(&mut reader, &mut window, &mut chunk)? {
                    continue;
                }
                break;
            }
            Decoded::Corrupt(why) => return Err(WalError::Corrupt { at: offset, why }),
            Decoded::Unsupported { format, checksum } => {
                return Err(WalError::Unsupported { format, checksum });
            }
        };
        if header.operation_id != operation_id {
            return Err(WalError::Corrupt { at: offset, why: "foreign operation id".to_owned() });
        }
        let pair = (header.generation, header.sequence);
        if let Some(prior) = seen.get(&pair) {
            if *prior != checksum {
                return Err(WalError::SequenceConflict { generation: pair.0, sequence: pair.1 });
            }
        } else {
            let in_order = match out.last {
                None => pair.1 == 1,
                Some((g, s)) => {
                    (pair.0 == g && pair.1 == s.saturating_add(1)) || (pair.0 > g && pair.1 == 1)
                }
            };
            if !in_order {
                return Err(WalError::Corrupt {
                    at: offset,
                    why: "sequence regression".to_owned(),
                });
            }
            seen.insert(pair, checksum);
            out.last = Some(pair);
            let file_id = match &body {
                Body::FileBegin(f) => &f.file_id,
                Body::Range(r) => &r.file_id,
            };
            out.per_file.entry(file_id.clone()).or_default().push(offset);
        }
        start += len;
        offset += len as u64;
        out.valid_len = offset;
    }
    Ok(out)
}

/// One file's effective state after replay.
#[derive(Debug)]
pub(crate) struct FileState {
    pub begin: FileBegin,
    /// Chunk index -> digest, the effective set.
    pub digests: BTreeMap<u64, Digest>,
}

/// Pass two: apply one file's records, in offset order, over a fresh read of the same segment.
pub(crate) fn load_file<R: Read>(
    mut reader: R,
    scan: &Scan,
    file_id: &[u8],
) -> Result<Option<FileState>, WalError> {
    let Some(offsets) = scan.per_file.get(file_id) else {
        return Ok(None);
    };
    let mut window: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; READ_WINDOW];
    // Absolute offset of `window[0]`; the reader sits at `base + window.len()`.
    let mut base = 0u64;
    let mut begin: Option<FileBegin> = None;
    let mut digests: BTreeMap<u64, Digest> = BTreeMap::new();
    for &at in offsets {
        let corrupt = |why: &str| WalError::Corrupt { at, why: why.to_owned() };
        let end = base + window.len() as u64;
        if at < base {
            return Err(corrupt("offsets out of order"));
        }
        if at >= end {
            let gap = at - end;
            let copied =
                io::copy(&mut reader.by_ref().take(gap), &mut io::sink()).map_err(io_err)?;
            if copied < gap {
                return Err(corrupt("segment ends before an indexed record"));
            }
            window.clear();
        } else {
            window.drain(..usize::try_from(at - base).expect("within the window"));
        }
        base = at;
        let (body, len) = loop {
            match decode_next(&window) {
                Decoded::Record { body, len, .. } => break (body, len),
                Decoded::TornTail | Decoded::End => {
                    if !fill(&mut reader, &mut window, &mut chunk)? {
                        return Err(corrupt("segment ends inside an indexed record"));
                    }
                }
                _ => return Err(corrupt("an indexed record no longer decodes")),
            }
        };
        window.drain(..len);
        base += len as u64;
        match body {
            Body::FileBegin(f) => {
                digests.retain(|index, _| index.saturating_mul(CHUNK_SIZE) < f.valid_through);
                begin = Some(f);
            }
            Body::Range(r) => {
                if begin.is_some() {
                    let first = r.range_start / CHUNK_SIZE;
                    for (i, d) in r.digests.into_iter().enumerate() {
                        digests.insert(first + i as u64, d);
                    }
                }
            }
        }
    }
    Ok(begin.map(|begin| FileState { begin, digests }))
}

#[cfg(test)]
mod tests {
    use super::super::CHUNK_SIZE;
    use super::super::record::{Body, FileBegin, Header, Range, encode};
    use super::*;
    use std::io::Cursor;

    const OP: &[u8] = b"op-replay";
    const MIB: u64 = CHUNK_SIZE;

    fn header(generation: u64, sequence: u64) -> Header {
        Header { operation_id: OP.to_vec(), generation, sequence }
    }

    fn begin(file: u8, valid_through: u64) -> Body {
        Body::FileBegin(FileBegin {
            file_id: vec![file],
            attempt_id: 1,
            destination: b"out".to_vec(),
            source_len: 10 * MIB,
            source_mtime: None,
            source_identity: vec![7; 4],
            chunk_size: CHUNK_SIZE,
            valid_through,
        })
    }

    /// A range of whole chunks starting at chunk `first`, digest `i` being `[tag + i; 32]`.
    fn range(file: u8, first: u64, count: u64, tag: u8) -> Body {
        Body::Range(Range {
            file_id: vec![file],
            attempt_id: 1,
            range_start: first * MIB,
            range_end: (first + count) * MIB,
            digests: (0..count).map(|i| [tag.wrapping_add(i as u8); 32]).collect(),
        })
    }

    fn one(g: u64, s: u64, body: &Body) -> Vec<u8> {
        encode(&header(g, s), body)
    }

    fn stream(records: &[(u64, u64, Body)]) -> Vec<u8> {
        records.iter().flat_map(|(g, s, b)| one(*g, *s, b)).collect()
    }

    fn run(bytes: &[u8]) -> Result<Scan, WalError> {
        scan(Cursor::new(bytes.to_vec()), OP)
    }

    #[test]
    fn scan_indexes_offsets_per_file_and_tracks_last() {
        let (r0, r1, r2) = (begin(b'A', 0), range(b'A', 0, 1, 1), begin(b'B', 0));
        let bytes = stream(&[(1, 1, r0.clone()), (1, 2, r1.clone()), (1, 3, r2)]);
        let (len0, len1) = (one(1, 1, &r0).len() as u64, one(1, 2, &r1).len() as u64);
        let s = run(&bytes).unwrap();
        assert_eq!(s.per_file[&vec![b'A']], vec![0, len0]);
        assert_eq!(s.per_file[&vec![b'B']], vec![len0 + len1]);
        assert_eq!(s.per_file.len(), 2);
        assert_eq!(s.last, Some((1, 3)));
        assert_eq!(s.valid_len, bytes.len() as u64);
    }

    #[test]
    fn a_torn_tail_ends_the_scan_at_the_last_valid_record() {
        let mut bytes =
            stream(&[(1, 1, begin(b'A', 0)), (1, 2, range(b'A', 0, 1, 1)), (1, 3, begin(b'B', 0))]);
        let whole = bytes.len() as u64;
        let fourth = one(1, 4, &range(b'B', 0, 1, 2));
        bytes.extend_from_slice(&fourth[..fourth.len() - 5]);
        let s = run(&bytes).unwrap();
        assert_eq!(s.valid_len, whole);
        assert_eq!(s.last, Some((1, 3)));
    }

    #[test]
    fn a_record_larger_than_the_read_window_scans() {
        // 2,200 digests: 70,400 bytes of digests, above one 64 KiB read.
        let big = range(b'A', 0, 2_200, 1);
        let bytes = stream(&[(1, 1, begin(b'A', 0)), (1, 2, big.clone())]);
        assert!(one(1, 2, &big).len() > 65_536);
        let s = run(&bytes).unwrap();
        let len0 = one(1, 1, &begin(b'A', 0)).len() as u64;
        assert_eq!(s.per_file[&vec![b'A']], vec![0, len0]);
        assert_eq!(s.last, Some((1, 2)));
        assert_eq!(s.valid_len, bytes.len() as u64);
    }

    #[test]
    fn a_corrupt_middle_record_is_wal_corrupt() {
        let r0 = begin(b'A', 0);
        let mut bytes =
            stream(&[(1, 1, r0.clone()), (1, 2, range(b'A', 0, 1, 1)), (1, 3, begin(b'B', 0))]);
        let at = one(1, 1, &r0).len() + 12;
        bytes[at] ^= 1;
        match run(&bytes) {
            Err(WalError::Corrupt { at, .. }) => assert_eq!(at, one(1, 1, &r0).len() as u64),
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn an_unsupported_version_is_reported() {
        let mut bytes = one(1, 1, &begin(b'A', 0));
        bytes[4] = 2;
        assert!(matches!(run(&bytes), Err(WalError::Unsupported { format: 2, checksum: 1 })));
    }

    #[test]
    fn an_identical_duplicate_is_idempotent() {
        let r1 = range(b'A', 0, 1, 1);
        let bytes = stream(&[
            (1, 1, begin(b'A', 0)),
            (1, 2, r1.clone()),
            (1, 2, r1.clone()),
            (1, 3, range(b'A', 1, 1, 2)),
        ]);
        let len0 = one(1, 1, &begin(b'A', 0)).len() as u64;
        let len1 = one(1, 2, &r1).len() as u64;
        let s = run(&bytes).unwrap();
        assert_eq!(s.per_file[&vec![b'A']], vec![0, len0, len0 + 2 * len1]);
        assert_eq!(s.last, Some((1, 3)));
        assert_eq!(s.valid_len, bytes.len() as u64);
    }

    #[test]
    fn a_conflicting_duplicate_is_sequence_conflict() {
        let bytes = stream(&[
            (1, 1, begin(b'A', 0)),
            (1, 2, range(b'A', 0, 1, 1)),
            (1, 2, range(b'A', 0, 1, 9)),
        ]);
        assert!(matches!(
            run(&bytes),
            Err(WalError::SequenceConflict { generation: 1, sequence: 2 })
        ));
    }

    #[test]
    fn a_generation_bump_restarts_the_sequence() {
        let ok =
            stream(&[(1, 1, begin(b'A', 0)), (1, 2, range(b'A', 0, 1, 1)), (2, 1, begin(b'A', 0))]);
        assert_eq!(run(&ok).unwrap().last, Some((2, 1)));
        let bad =
            stream(&[(1, 1, begin(b'A', 0)), (1, 2, range(b'A', 0, 1, 1)), (2, 2, begin(b'A', 0))]);
        assert!(matches!(run(&bad), Err(WalError::Corrupt { .. })));
    }

    #[test]
    fn a_sequence_regression_is_corrupt() {
        // A gap.
        let gap = stream(&[(1, 1, begin(b'A', 0)), (1, 3, range(b'A', 0, 1, 1))]);
        assert!(matches!(run(&gap), Err(WalError::Corrupt { .. })));
        // A re-sent (1,1) with the same bytes is a duplicate, skipped; `last` stays (1,3).
        let dup = stream(&[
            (1, 1, begin(b'A', 0)),
            (1, 2, range(b'A', 0, 1, 1)),
            (1, 3, range(b'A', 1, 1, 2)),
            (1, 1, begin(b'A', 0)),
        ]);
        let s = run(&dup).unwrap();
        assert_eq!(s.last, Some((1, 3)));
        assert_eq!(s.valid_len, dup.len() as u64);
        // No earlier (1,1): the stream cannot even start.
        let headless = stream(&[
            (1, 2, range(b'A', 0, 1, 1)),
            (1, 3, range(b'A', 1, 1, 2)),
            (1, 1, begin(b'A', 0)),
        ]);
        assert!(matches!(run(&headless), Err(WalError::Corrupt { .. })));
        // A never-seen pair at or below `last`.
        let back =
            stream(&[(1, 1, begin(b'A', 0)), (2, 1, begin(b'A', 0)), (1, 2, range(b'A', 0, 1, 1))]);
        assert!(matches!(run(&back), Err(WalError::Corrupt { .. })));
    }

    #[test]
    fn a_foreign_operation_id_is_corrupt() {
        let other = Header { operation_id: b"someone-else".to_vec(), generation: 1, sequence: 1 };
        let bytes = encode(&other, &begin(b'A', 0));
        assert!(matches!(run(&bytes), Err(WalError::Corrupt { at: 0, .. })));
    }

    fn state(records: &[(u64, u64, Body)], file: u8) -> Option<FileState> {
        let bytes = stream(records);
        let s = run(&bytes).unwrap();
        load_file(Cursor::new(bytes), &s, &[file]).unwrap()
    }

    fn keys(st: &FileState) -> Vec<u64> {
        st.digests.keys().copied().collect()
    }

    #[test]
    fn load_file_merges_ranges_into_chunk_digests() {
        let st = state(
            &[
                (1, 1, begin(b'A', 0)),
                (1, 2, begin(b'B', 0)),
                (1, 3, range(b'A', 0, 2, 10)),
                (1, 4, range(b'B', 0, 1, 99)),
                (1, 5, range(b'A', 2, 1, 50)),
            ],
            b'A',
        )
        .unwrap();
        assert_eq!(keys(&st), vec![0, 1, 2]);
        assert_eq!(st.digests[&0], [10; 32]);
        assert_eq!(st.digests[&1], [11; 32]);
        assert_eq!(st.digests[&2], [50; 32]);
    }

    #[test]
    fn a_later_file_begin_drops_digests_at_or_past_valid_through() {
        let records =
            [(1, 1, begin(b'A', 0)), (1, 2, range(b'A', 0, 4, 1)), (1, 3, begin(b'A', 2 * MIB))];
        let st = state(&records, b'A').unwrap();
        assert_eq!(keys(&st), vec![0, 1]);
        assert_eq!(
            st.begin,
            match &records[2].2 {
                Body::FileBegin(f) => f.clone(),
                Body::Range(_) => unreachable!(),
            }
        );
        let mut more = records.to_vec();
        more.push((1, 4, range(b'A', 2, 1, 77)));
        let st = state(&more, b'A').unwrap();
        assert_eq!(keys(&st), vec![0, 1, 2]);
        assert_eq!(st.digests[&2], [77; 32]);
    }

    /// Yields 1 to 7 bytes per call, so no read ever fills the window.
    struct Trickle {
        inner: Cursor<Vec<u8>>,
        calls: usize,
    }

    impl Trickle {
        fn new(bytes: &[u8]) -> Self {
            Self { inner: Cursor::new(bytes.to_vec()), calls: 0 }
        }
    }

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            let cap = buf.len().min(1 + self.calls % 7);
            self.inner.read(&mut buf[..cap])
        }
    }

    #[test]
    fn short_reads_do_not_end_the_scan_early() {
        let big = range(b'A', 0, 2_200, 1);
        let mut bytes = stream(&[(1, 1, begin(b'A', 0)), (1, 2, big)]);
        let whole = bytes.len() as u64;
        let s = scan(Trickle::new(&bytes), OP).unwrap();
        assert_eq!((s.last, s.valid_len), (Some((1, 2)), whole));
        let third = one(1, 3, &range(b'A', 0, 1, 2));
        bytes.extend_from_slice(&third[..third.len() - 5]);
        let s = scan(Trickle::new(&bytes), OP).unwrap();
        assert_eq!((s.last, s.valid_len), (Some((1, 2)), whole));
    }

    #[test]
    fn offsets_stay_absolute_across_window_drains() {
        let mut records = vec![(1, 1, begin(b'A', 0))];
        for i in 0..700u64 {
            records.push((1, i + 2, range(b'A', i, 1, 1)));
        }
        let mut want = Vec::new();
        let mut at = 0u64;
        for (g, sq, b) in &records {
            want.push(at);
            at += one(*g, *sq, b).len() as u64;
        }
        let bytes = stream(&records);
        assert!(at > 65_536);
        let s = run(&bytes).unwrap();
        assert_eq!(s.per_file[&vec![b'A']], want);
        assert_eq!(s.valid_len, at);
        let st = load_file(Cursor::new(bytes), &s, b"A").unwrap().unwrap();
        assert_eq!(st.digests.len(), 700);
    }

    /// begin A, a 2,200-digest range for B (over one window), then a range for A past the window.
    fn skipping_stream() -> (Vec<u8>, Scan) {
        let bytes = stream(&[
            (1, 1, begin(b'A', 0)),
            (1, 2, range(b'B', 0, 2_200, 1)),
            (1, 3, range(b'A', 5, 1, 42)),
        ]);
        let s = run(&bytes).unwrap();
        (bytes, s)
    }

    #[test]
    fn load_file_skips_forward_past_the_window() {
        let (bytes, s) = skipping_stream();
        assert!(s.per_file[&vec![b'A']][1] > 65_536);
        let st = load_file(Cursor::new(bytes.clone()), &s, b"A").unwrap().unwrap();
        assert_eq!(keys(&st), vec![5]);
        assert_eq!(st.digests[&5], [42; 32]);
        let st = load_file(Trickle::new(&bytes), &s, b"A").unwrap().unwrap();
        assert_eq!(keys(&st), vec![5]);
    }

    #[test]
    fn a_reader_truncated_before_an_indexed_record_is_corrupt() {
        let (bytes, s) = skipping_stream();
        let last = s.per_file[&vec![b'A']][1] as usize;
        // Cut inside the skipped gap: the skip itself comes up short.
        let cut = Cursor::new(bytes[..last - 100].to_vec());
        match load_file(cut, &s, b"A") {
            Err(WalError::Corrupt { why, .. }) => {
                assert!(why.contains("before an indexed"), "{why}")
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
        // Cut inside the record itself.
        let cut = Cursor::new(bytes[..bytes.len() - 3].to_vec());
        assert!(matches!(load_file(cut, &s, b"A"), Err(WalError::Corrupt { .. })));
    }

    #[test]
    fn a_range_before_any_file_begin_is_skipped() {
        let st = state(&[(1, 1, range(b'A', 0, 1, 1)), (1, 2, begin(b'A', 0))], b'A').unwrap();
        assert!(st.digests.is_empty());
    }

    #[test]
    fn load_file_of_an_unknown_file_is_none() {
        assert!(state(&[(1, 1, begin(b'A', 0))], b'Z').is_none());
    }
}
