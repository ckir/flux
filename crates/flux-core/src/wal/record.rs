//! WAL record encoding and its corruption classes (cut 9f-1).
//!
//! Layout, little-endian: `MAGIC(4) FORMAT_VERSION(u16) CHECKSUM_VERSION(u8) record_type(u8)
//! operation_id_len(u16) operation_id generation(u64) sequence(u64) payload_len(u32) payload
//! checksum(16) COMMIT(1)`. The checksum is the first 16 bytes of BLAKE3 over every byte before it.

use super::{CHECKSUM_VERSION, CHUNK_SIZE, COMMIT, Digest, FORMAT_VERSION, MAGIC, MAX_PAYLOAD};

const TYPE_FILE_BEGIN: u8 = 1;
const TYPE_RANGE: u8 = 2;
/// `MAGIC` through `operation_id_len`.
const PREFIX_LEN: usize = 10;
/// `generation`, `sequence`, `payload_len`.
const FIXED_LEN: usize = 20;
/// Checksum and `COMMIT`.
const SUFFIX_LEN: usize = 17;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Header {
    pub operation_id: Vec<u8>,
    pub generation: u64,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileBegin {
    pub file_id: Vec<u8>,
    pub attempt_id: u64,
    pub destination: Vec<u8>,
    pub source_len: u64,
    pub source_mtime: Option<(i64, u32)>,
    pub source_identity: Vec<u8>,
    pub chunk_size: u64,
    pub valid_through: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Range {
    pub file_id: Vec<u8>,
    pub attempt_id: u64,
    pub range_start: u64,
    pub range_end: u64,
    pub digests: Vec<Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Body {
    FileBegin(FileBegin),
    Range(Range),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decoded {
    Record { header: Header, body: Body, len: usize, checksum: [u8; 16] },
    End,
    TornTail,
    Corrupt(String),
    Unsupported { format: u16, checksum: u8 },
}

fn checksum_of(covered: &[u8]) -> [u8; 16] {
    blake3::hash(covered).as_bytes()[..16].try_into().expect("sixteen bytes")
}

fn put_bytes(out: &mut Vec<u8>, v: &[u8]) {
    let len = u32::try_from(v.len()).expect("a WAL field is far below 4 GiB");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(v);
}

pub(crate) fn encode(h: &Header, b: &Body) -> Vec<u8> {
    let mut payload = Vec::new();
    let record_type = match b {
        Body::FileBegin(f) => {
            put_bytes(&mut payload, &f.file_id);
            payload.extend_from_slice(&f.attempt_id.to_le_bytes());
            put_bytes(&mut payload, &f.destination);
            payload.extend_from_slice(&f.source_len.to_le_bytes());
            match f.source_mtime {
                None => payload.push(0),
                Some((secs, nanos)) => {
                    payload.push(1);
                    payload.extend_from_slice(&secs.to_le_bytes());
                    payload.extend_from_slice(&nanos.to_le_bytes());
                }
            }
            put_bytes(&mut payload, &f.source_identity);
            payload.extend_from_slice(&f.chunk_size.to_le_bytes());
            payload.extend_from_slice(&f.valid_through.to_le_bytes());
            TYPE_FILE_BEGIN
        }
        Body::Range(r) => {
            put_bytes(&mut payload, &r.file_id);
            payload.extend_from_slice(&r.attempt_id.to_le_bytes());
            payload.extend_from_slice(&r.range_start.to_le_bytes());
            payload.extend_from_slice(&r.range_end.to_le_bytes());
            let n = u32::try_from(r.digests.len()).expect("a range holds few digests");
            payload.extend_from_slice(&n.to_le_bytes());
            for d in &r.digests {
                payload.extend_from_slice(d);
            }
            TYPE_RANGE
        }
    };
    let op_len = u16::try_from(h.operation_id.len()).expect("an operation id is far below 64 KiB");
    let payload_len = u32::try_from(payload.len()).expect("a payload is far below 4 GiB");
    let mut out = Vec::with_capacity(
        PREFIX_LEN + h.operation_id.len() + FIXED_LEN + payload.len() + SUFFIX_LEN,
    );
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.push(CHECKSUM_VERSION);
    out.push(record_type);
    out.extend_from_slice(&op_len.to_le_bytes());
    out.extend_from_slice(&h.operation_id);
    out.extend_from_slice(&h.generation.to_le_bytes());
    out.extend_from_slice(&h.sequence.to_le_bytes());
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.extend_from_slice(&payload);
    let sum = checksum_of(&out);
    out.extend_from_slice(&sum);
    out.push(COMMIT);
    out
}

/// A forward-only reader over a payload; every read is bounds-checked.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn vec(&mut self) -> Option<Vec<u8>> {
        let n = self.u32()? as usize;
        Some(self.take(n)?.to_vec())
    }
}

fn parse_file_begin(r: &mut Reader<'_>) -> Option<FileBegin> {
    let file_id = r.vec()?;
    let attempt_id = r.u64()?;
    let destination = r.vec()?;
    let source_len = r.u64()?;
    let source_mtime = match r.u8()? {
        0 => None,
        1 => Some((r.i64()?, r.u32()?)),
        _ => return None,
    };
    let source_identity = r.vec()?;
    let chunk_size = r.u64()?;
    let valid_through = r.u64()?;
    Some(FileBegin {
        file_id,
        attempt_id,
        destination,
        source_len,
        source_mtime,
        source_identity,
        chunk_size,
        valid_through,
    })
}

fn parse_range(r: &mut Reader<'_>) -> Option<Range> {
    let file_id = r.vec()?;
    let attempt_id = r.u64()?;
    let range_start = r.u64()?;
    let range_end = r.u64()?;
    let count = r.u32()? as usize;
    // Bounded by the payload actually present, so a flipped count cannot allocate.
    if count.checked_mul(32)? > r.bytes.len() - r.at {
        return None;
    }
    let mut digests = Vec::with_capacity(count);
    for _ in 0..count {
        digests.push(r.take(32)?.try_into().ok()?);
    }
    Some(Range { file_id, attempt_id, range_start, range_end, digests })
}

/// The span the record at the start of `bytes` DECLARES, when its lengths can be trusted: valid
/// `MAGIC`, a supported format and checksum version, a readable `operation_id_len` and a
/// `payload_len` within the bound. `None` otherwise. It does not check the checksum, the type or
/// that `bytes` is long enough to hold the span.
pub(crate) fn declared_span(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < PREFIX_LEN || bytes[..4] != MAGIC {
        return None;
    }
    if u16::from_le_bytes([bytes[4], bytes[5]]) != FORMAT_VERSION || bytes[6] != CHECKSUM_VERSION {
        return None;
    }
    let fixed_at = PREFIX_LEN + usize::from(u16::from_le_bytes([bytes[8], bytes[9]]));
    let payload_len_at = fixed_at + 16;
    let payload_len =
        u32::from_le_bytes(bytes.get(payload_len_at..payload_len_at + 4)?.try_into().ok()?);
    if payload_len > MAX_PAYLOAD {
        return None;
    }
    Some(fixed_at + FIXED_LEN + payload_len as usize + SUFFIX_LEN)
}

pub(crate) fn decode_next(bytes: &[u8]) -> Decoded {
    if bytes.is_empty() {
        return Decoded::End;
    }
    if bytes.len() < PREFIX_LEN {
        return Decoded::TornTail;
    }
    if bytes[..4] != MAGIC {
        return Decoded::Corrupt("bad magic".to_owned());
    }
    let format = u16::from_le_bytes([bytes[4], bytes[5]]);
    let checksum_version = bytes[6];
    if format != FORMAT_VERSION || checksum_version != CHECKSUM_VERSION {
        return Decoded::Unsupported { format, checksum: checksum_version };
    }
    let record_type = bytes[7];
    let op_len = usize::from(u16::from_le_bytes([bytes[8], bytes[9]]));
    let fixed_at = PREFIX_LEN + op_len;
    if bytes.len() < fixed_at + FIXED_LEN {
        return Decoded::TornTail;
    }
    let payload_len_at = fixed_at + 16;
    let payload_len = u32::from_le_bytes(
        bytes[payload_len_at..payload_len_at + 4].try_into().expect("four bytes"),
    );
    if payload_len > MAX_PAYLOAD {
        return Decoded::Corrupt(format!(
            "payload_len {payload_len} above the bound {MAX_PAYLOAD}"
        ));
    }
    let payload_at = fixed_at + FIXED_LEN;
    let checksum_at = payload_at + payload_len as usize;
    let len = checksum_at + SUFFIX_LEN;
    if bytes.len() < len {
        return Decoded::TornTail;
    }
    let checksum: [u8; 16] =
        bytes[checksum_at..checksum_at + 16].try_into().expect("sixteen bytes");
    if checksum_of(&bytes[..checksum_at]) != checksum {
        return Decoded::Corrupt("checksum mismatch".to_owned());
    }
    if bytes[len - 1] != COMMIT {
        return Decoded::Corrupt("wrong commit byte".to_owned());
    }
    let header = Header {
        operation_id: bytes[PREFIX_LEN..fixed_at].to_vec(),
        generation: u64::from_le_bytes(
            bytes[fixed_at..fixed_at + 8].try_into().expect("eight bytes"),
        ),
        sequence: u64::from_le_bytes(
            bytes[fixed_at + 8..fixed_at + 16].try_into().expect("eight bytes"),
        ),
    };
    let mut reader = Reader { bytes: &bytes[payload_at..checksum_at], at: 0 };
    let body = match record_type {
        TYPE_FILE_BEGIN => parse_file_begin(&mut reader).map(Body::FileBegin),
        TYPE_RANGE => match parse_range(&mut reader) {
            Some(r) => {
                if r.range_start % CHUNK_SIZE != 0 {
                    return Decoded::Corrupt("range_start is not chunk aligned".to_owned());
                }
                if r.range_end <= r.range_start {
                    return Decoded::Corrupt("range_end is not above range_start".to_owned());
                }
                if r.digests.len() as u64 != (r.range_end - r.range_start).div_ceil(CHUNK_SIZE) {
                    return Decoded::Corrupt("digest count does not match the range".to_owned());
                }
                Some(Body::Range(r))
            }
            None => None,
        },
        other => return Decoded::Corrupt(format!("unknown record type {other}")),
    };
    match body {
        Some(body) if reader.at == reader.bytes.len() => {
            Decoded::Record { header, body, len, checksum }
        }
        _ => Decoded::Corrupt("malformed payload".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(sequence: u64) -> Header {
        Header { operation_id: b"op-7f3a".to_vec(), generation: 3, sequence }
    }

    fn file_begin() -> Body {
        Body::FileBegin(FileBegin {
            file_id: vec![1, 2, 3, 4],
            attempt_id: 9,
            destination: b"out/a.bin".to_vec(),
            source_len: 5 << 20,
            source_mtime: Some((-5, 999_999_999)),
            source_identity: vec![0xAB; 12],
            chunk_size: CHUNK_SIZE,
            valid_through: 2 * CHUNK_SIZE,
        })
    }

    fn range_of(start: u64, end: u64, digests: usize) -> Body {
        Body::Range(Range {
            file_id: vec![1, 2, 3, 4],
            attempt_id: 9,
            range_start: start,
            range_end: end,
            digests: (0..digests).map(|i| [i as u8 + 1; 32]).collect(),
        })
    }

    fn range() -> Body {
        // Two and a bit chunks: three digests.
        range_of(CHUNK_SIZE, 3 * CHUNK_SIZE + 5, 3)
    }

    /// Recompute the checksum after a test patched a covered byte.
    fn reseal(mut bytes: Vec<u8>) -> Vec<u8> {
        let at = bytes.len() - SUFFIX_LEN;
        let sum = blake3::hash(&bytes[..at]);
        bytes[at..at + 16].copy_from_slice(&sum.as_bytes()[..16]);
        bytes
    }

    #[test]
    fn a_file_begin_round_trips() {
        let bytes = encode(&header(1), &file_begin());
        let Decoded::Record { header: h, body, len, checksum } = decode_next(&bytes) else {
            panic!("expected a record");
        };
        assert_eq!(h, header(1));
        assert_eq!(body, file_begin());
        assert_eq!(len, bytes.len());
        assert_eq!(checksum[..], bytes[bytes.len() - 17..bytes.len() - 1]);
        assert_eq!(bytes[bytes.len() - 1], 0xC3);
        assert_eq!(bytes[..4], *b"FWAL");
    }

    #[test]
    fn an_empty_operation_id_round_trips_and_its_span_is_exact() {
        let h = Header { operation_id: Vec::new(), generation: 3, sequence: 1 };
        for body in [file_begin(), range()] {
            let bytes = encode(&h, &body);
            assert_eq!(declared_span(&bytes), Some(bytes.len()));
            let Decoded::Record { header, body: decoded, len, .. } = decode_next(&bytes) else {
                panic!("an empty operation id is still a record");
            };
            assert_eq!(header, h);
            assert_eq!(decoded, body);
            assert_eq!(len, bytes.len());
        }
    }

    #[test]
    fn a_file_begin_without_an_mtime_round_trips() {
        let Body::FileBegin(mut f) = file_begin() else { unreachable!() };
        f.source_mtime = None;
        let bytes = encode(&header(1), &Body::FileBegin(f.clone()));
        let Decoded::Record { body, len, .. } = decode_next(&bytes) else {
            panic!("expected a record")
        };
        assert_eq!(body, Body::FileBegin(f));
        assert_eq!(len, bytes.len());
    }

    #[test]
    fn a_range_round_trips() {
        let bytes = encode(&header(2), &range());
        let Decoded::Record { header: h, body, len, .. } = decode_next(&bytes) else {
            panic!("expected a record");
        };
        assert_eq!(h, header(2));
        assert_eq!(body, range());
        assert_eq!(len, bytes.len());
    }

    #[test]
    fn two_records_decode_in_sequence() {
        let a = encode(&header(1), &file_begin());
        let b = encode(&header(2), &range());
        let both = [a.as_slice(), b.as_slice()].concat();
        let Decoded::Record { header: h1, len: l1, .. } = decode_next(&both) else {
            panic!("first")
        };
        assert_eq!((h1.sequence, l1), (1, a.len()));
        let Decoded::Record { header: h2, len: l2, .. } = decode_next(&both[l1..]) else {
            panic!("second")
        };
        assert_eq!((h2.sequence, l2), (2, b.len()));
        assert_eq!(decode_next(&both[l1 + l2..]), Decoded::End);
    }

    #[test]
    fn every_shorter_prefix_is_a_torn_tail() {
        for bytes in [encode(&header(1), &file_begin()), encode(&header(2), &range())] {
            for n in 0..bytes.len() {
                let want = if n == 0 { Decoded::End } else { Decoded::TornTail };
                assert_eq!(decode_next(&bytes[..n]), want, "prefix of {n} bytes");
            }
        }
    }

    #[test]
    fn every_single_byte_flip_is_never_a_record() {
        for bytes in [encode(&header(1), &file_begin()), encode(&header(2), &range())] {
            let op_len = header(0).operation_id.len();
            let payload_len_at = PREFIX_LEN + op_len + 16;
            for pos in 0..bytes.len() {
                let mut flipped = bytes.clone();
                flipped[pos] ^= 1;
                let got = decode_next(&flipped);
                let is_length =
                    (8..10).contains(&pos) || (payload_len_at..payload_len_at + 4).contains(&pos);
                match got {
                    Decoded::Record { .. } => panic!("flip at {pos} decoded as a record"),
                    Decoded::Unsupported { .. } => {
                        assert!((4..7).contains(&pos), "Unsupported at {pos}")
                    }
                    Decoded::TornTail => assert!(is_length, "TornTail at {pos}"),
                    Decoded::Corrupt(_) => {}
                    Decoded::End => panic!("End at {pos}"),
                }
                if !is_length && !(4..7).contains(&pos) {
                    assert!(matches!(got, Decoded::Corrupt(_)), "position {pos} must be Corrupt");
                }
            }
        }
    }

    #[test]
    fn a_range_with_the_wrong_digest_count_is_corrupt() {
        let bytes = encode(&header(1), &range_of(0, 2 * CHUNK_SIZE, 1));
        assert!(matches!(decode_next(&bytes), Decoded::Corrupt(_)));
        let bytes = encode(&header(1), &range_of(0, CHUNK_SIZE, 2));
        assert!(matches!(decode_next(&bytes), Decoded::Corrupt(_)));
    }

    #[test]
    fn an_unaligned_range_start_is_corrupt() {
        let bytes = encode(&header(1), &range_of(CHUNK_SIZE + 1, 2 * CHUNK_SIZE + 1, 1));
        assert!(matches!(decode_next(&bytes), Decoded::Corrupt(_)));
    }

    #[test]
    fn an_empty_range_is_corrupt() {
        let bytes = encode(&header(1), &range_of(CHUNK_SIZE, CHUNK_SIZE, 0));
        assert!(matches!(decode_next(&bytes), Decoded::Corrupt(_)));
        let bytes = encode(&header(1), &range_of(2 * CHUNK_SIZE, CHUNK_SIZE, 0));
        assert!(matches!(decode_next(&bytes), Decoded::Corrupt(_)));
    }

    #[test]
    fn an_unknown_record_type_is_corrupt() {
        let mut bytes = encode(&header(1), &file_begin());
        bytes[7] = 3;
        assert!(matches!(decode_next(&reseal(bytes)), Decoded::Corrupt(_)));
    }

    #[test]
    fn a_wrong_commit_byte_with_the_full_length_is_corrupt() {
        let mut bytes = encode(&header(1), &file_begin());
        *bytes.last_mut().unwrap() = 0;
        assert!(matches!(decode_next(&bytes), Decoded::Corrupt(_)));
    }

    #[test]
    fn a_newer_version_is_unsupported_before_the_checksum_is_judged() {
        let mut bytes = encode(&header(1), &file_begin());
        bytes[4] = 2;
        assert_eq!(decode_next(&bytes), Decoded::Unsupported { format: 2, checksum: 1 });
        let mut bytes = encode(&header(1), &file_begin());
        bytes[6] = 2;
        assert_eq!(decode_next(&bytes), Decoded::Unsupported { format: 1, checksum: 2 });
    }

    #[test]
    fn a_payload_length_above_the_bound_is_corrupt_not_torn() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FWAL");
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&[1, 1]);
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 16]);
        bytes.extend_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
        assert_eq!(bytes.len(), 30);
        assert!(matches!(decode_next(&bytes), Decoded::Corrupt(_)));
    }

    /// The persisted layout, spelled out by hand: every byte of two small records, little-endian, so a
    /// reordered field, a changed width or a changed constant cannot pass by round-tripping through itself.
    #[test]
    fn the_wire_layout_is_pinned() {
        fn blake16(preceding: &[u8]) -> Vec<u8> {
            blake3::hash(preceding).as_bytes()[..16].to_vec()
        }
        let file_begin = |mtime: Option<(i64, u32)>| {
            Body::FileBegin(FileBegin {
                file_id: b"f".to_vec(),
                attempt_id: 1,
                destination: b"d".to_vec(),
                source_len: 5,
                source_mtime: mtime,
                source_identity: b"i".to_vec(),
                chunk_size: CHUNK_SIZE,
                valid_through: 0,
            })
        };
        let head = |record_type: u8, sequence: u8, payload_len: u8| -> Vec<u8> {
            [
                &b"FWAL"[..],                     // MAGIC
                &[1, 0],                          // FORMAT_VERSION = 1, u16
                &[1],                             // CHECKSUM_VERSION = 1
                &[record_type],                   // record_type
                &[2, 0],                          // operation_id_len = 2, u16
                b"op",                            // operation_id
                &[1, 0, 0, 0, 0, 0, 0, 0],        // generation = 1, u64
                &[sequence, 0, 0, 0, 0, 0, 0, 0], // sequence, u64
                &[payload_len, 0, 0, 0],          // payload_len, u32
            ]
            .concat()
        };
        let seal = |preceding: Vec<u8>| -> Vec<u8> {
            [preceding.clone(), blake16(&preceding), vec![0xC3]].concat()
        };

        // FileBegin with an mtime: a 60-byte payload.
        let payload_with_mtime = [
            &[1, 0, 0, 0][..],
            b"f",                      // file_id: u32 length 1, bytes
            &[1, 0, 0, 0, 0, 0, 0, 0], // attempt_id = 1, u64
            &[1, 0, 0, 0],
            b"d",                      // destination: u32 length 1, bytes
            &[5, 0, 0, 0, 0, 0, 0, 0], // source_len = 5, u64
            &[1],                      // mtime tag 1 = present
            &[7, 0, 0, 0, 0, 0, 0, 0], // mtime seconds = 7, i64
            &[9, 0, 0, 0],             // mtime nanos = 9, u32
            &[1, 0, 0, 0],
            b"i",                         // source_identity: u32 length 1, bytes
            &[0, 0, 0x10, 0, 0, 0, 0, 0], // chunk_size = 1 MiB, u64
            &[0, 0, 0, 0, 0, 0, 0, 0],    // valid_through = 0, u64
        ]
        .concat();
        assert_eq!(payload_with_mtime.len(), 60);
        let want = seal([head(1, 1, 60), payload_with_mtime].concat());
        assert_eq!(encode(&header_pinned(1), &file_begin(Some((7, 9)))), want);

        // The same record without an mtime: tag 0 and no seconds or nanos, 12 bytes shorter.
        let payload_without_mtime = [
            &[1, 0, 0, 0][..],
            b"f",
            &[1, 0, 0, 0, 0, 0, 0, 0],
            &[1, 0, 0, 0],
            b"d",
            &[5, 0, 0, 0, 0, 0, 0, 0],
            &[0], // mtime tag 0 = absent
            &[1, 0, 0, 0],
            b"i",
            &[0, 0, 0x10, 0, 0, 0, 0, 0],
            &[0, 0, 0, 0, 0, 0, 0, 0],
        ]
        .concat();
        assert_eq!(payload_without_mtime.len(), 48);
        let want = seal([head(1, 1, 48), payload_without_mtime].concat());
        assert_eq!(encode(&header_pinned(1), &file_begin(None)), want);

        // Range of one chunk with one digest of 0x11 bytes: a 65-byte payload.
        let payload_range = [
            &[1, 0, 0, 0][..],
            b"f",                         // file_id: u32 length 1, bytes
            &[1, 0, 0, 0, 0, 0, 0, 0],    // attempt_id = 1, u64
            &[0, 0, 0, 0, 0, 0, 0, 0],    // range_start = 0, u64
            &[0, 0, 0x10, 0, 0, 0, 0, 0], // range_end = 1 MiB, u64
            &[1, 0, 0, 0],                // digest count = 1, u32
            &[0x11; 32],                  // the digest
        ]
        .concat();
        assert_eq!(payload_range.len(), 65);
        let preceding = [head(2, 2, 65), payload_range].concat();
        let want = seal(preceding.clone());
        let got = encode(
            &header_pinned(2),
            &Body::Range(Range {
                file_id: b"f".to_vec(),
                attempt_id: 1,
                range_start: 0,
                range_end: CHUNK_SIZE,
                digests: vec![[0x11; 32]],
            }),
        );
        assert_eq!(got, want);
        // The checksum is the first 16 BLAKE3 bytes of everything before it, and the last byte is COMMIT.
        let at = got.len() - SUFFIX_LEN;
        assert_eq!(got[at..at + 16], blake3::hash(&preceding).as_bytes()[..16]);
        assert_eq!(got[got.len() - 1], 0xC3);
    }

    fn header_pinned(sequence: u64) -> Header {
        Header { operation_id: b"op".to_vec(), generation: 1, sequence }
    }

    /// The record `rec` with its payload passed through `f`, the length field rewritten and a valid checksum.
    fn resealed_payload(rec: &[u8], f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let op_len = usize::from(u16::from_le_bytes([rec[8], rec[9]]));
        let payload_at = PREFIX_LEN + op_len + FIXED_LEN;
        let mut payload = rec[payload_at..rec.len() - SUFFIX_LEN].to_vec();
        f(&mut payload);
        let mut out = rec[..payload_at - 4].to_vec();
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&[0u8; 16]);
        out.push(COMMIT);
        reseal(out)
    }

    #[test]
    fn a_sealed_payload_with_a_bad_nested_length_is_corrupt() {
        let begin = encode(&header(1), &file_begin());
        let ranged = encode(&header(2), &range());
        // The control: patching nothing still decodes, so each failure below is the patch's.
        assert!(matches!(decode_next(&resealed_payload(&begin, |_| {})), Decoded::Record { .. }));
        assert!(matches!(decode_next(&resealed_payload(&ranged, |_| {})), Decoded::Record { .. }));
        // (i) A file_id length larger than the remaining payload.
        let rec = resealed_payload(&begin, |p| {
            let too_long = p.len() as u32 + 1;
            p[0..4].copy_from_slice(&too_long.to_le_bytes());
        });
        assert_eq!(decode_next(&rec), Decoded::Corrupt("malformed payload".to_owned()));
        // (ii) A digest count of 0xFFFF_FFFF: file_id (4 + 4) + attempt_id, range_start, range_end (24).
        let rec =
            resealed_payload(&ranged, |p| p[32..36].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()));
        assert_eq!(decode_next(&rec), Decoded::Corrupt("malformed payload".to_owned()));
        // (iii) An mtime tag of 2, on a record with no mtime bytes after the tag (so a lenient reader that
        // treated an unknown tag as "absent" would decode it): file_id (8) + attempt_id (8) +
        // destination (4 + 9) + source_len (8) put the tag at payload offset 37.
        let Body::FileBegin(mut no_mtime) = file_begin() else { unreachable!() };
        no_mtime.source_mtime = None;
        let plain = encode(&header(1), &Body::FileBegin(no_mtime));
        assert!(matches!(decode_next(&resealed_payload(&plain, |_| {})), Decoded::Record { .. }));
        let rec = resealed_payload(&plain, |p| {
            assert_eq!(p[37], 0);
            p[37] = 2;
        });
        assert_eq!(decode_next(&rec), Decoded::Corrupt("malformed payload".to_owned()));
        // (iv) Trailing payload bytes after a well-formed body.
        for (rec, name) in [(&begin, "file_begin"), (&ranged, "range")] {
            let rec = resealed_payload(rec, |p| p.push(0));
            assert_eq!(
                decode_next(&rec),
                Decoded::Corrupt("malformed payload".to_owned()),
                "{name}"
            );
        }
    }
}
