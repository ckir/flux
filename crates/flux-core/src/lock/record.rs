//! The lock record, `format_version` 1 (§259.6; cut 7a spec, "Lock record" and "Decoding").
//!
//! A fixed 4096 bytes: the magic, the version, eight length-prefixed UTF-8 fields in the order of spec:12975-12982,
//! zero padding, and the first 16 bytes of the BLAKE3 hash of everything before them.

pub const RECORD_LEN: usize = 4096;
pub const MAGIC: [u8; 8] = *b"FLUXLOCK";
pub const FORMAT_VERSION: u32 = 1;
const CHECKSUM_AT: usize = RECORD_LEN - 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockRecord {
    pub complete_lock_key: String,
    pub operation_id: String,
    pub owner_instance_id: String,
    pub boot_session_id: String,
    pub target_path_key: String,
    /// One of `operations/<id>`, `adjacent/<id>`, `none` (cut 7a spec); the codec does not judge it.
    pub workspace_path: String,
    /// Nanoseconds since the Unix epoch, UTC; written as decimal ASCII.
    pub creation_wall_time: u64,
    pub last_heartbeat_wall_time: u64,
}

/// What a lock file's bytes are (cut 7a spec, "Decoding", in its order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    Record(LockRecord),
    /// Ownership cannot be established from these bytes: `TARGET_LOCK_UNCERTAIN`, never foreign.
    Uncertain(Uncertain),
    /// Not a Flux record: `CONTROL_PLANE_NAMESPACE_CONFLICT`, never overwritten.
    Foreign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uncertain {
    Empty,
    /// A torn write: a zero-or-magic prefix of at most 4096 bytes, or the magic with the wrong length.
    Torn,
    Checksum,
    /// A newer `format_version` with a valid checksum; 7a never guesses its layout.
    UnknownVersion(u32),
    /// A valid checksum over a layout this version cannot parse.
    Malformed,
}

impl LockRecord {
    /// Exactly `RECORD_LEN` bytes. Panics if the fields do not fit: every field is bounded (spec: "an encoder that
    /// would exceed it is a bug"), and a truncated record would be worse than a crash.
    pub fn encode(&self) -> Vec<u8> {
        let times =
            [self.creation_wall_time.to_string(), self.last_heartbeat_wall_time.to_string()];
        let fields: [&str; 8] = [
            &self.complete_lock_key,
            &self.operation_id,
            &self.owner_instance_id,
            &self.boot_session_id,
            &self.target_path_key,
            &self.workspace_path,
            &times[0],
            &times[1],
        ];
        let mut out = Vec::with_capacity(RECORD_LEN);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        for f in fields {
            let len = u16::try_from(f.len()).expect("a lock-record field is far below 64 KiB");
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(f.as_bytes());
        }
        assert!(
            out.len() <= CHECKSUM_AT,
            "lock record fields take {} bytes, more than {CHECKSUM_AT}",
            out.len()
        );
        out.resize(CHECKSUM_AT, 0);
        let sum = blake3::hash(&out);
        out.extend_from_slice(&sum.as_bytes()[..16]);
        out
    }
}

/// Classify a lock file's bytes (cut 7a spec, "Decoding", in this order).
pub fn decode(bytes: &[u8]) -> Decoded {
    if bytes.is_empty() {
        return Decoded::Uncertain(Uncertain::Empty);
    }
    let zero_or_magic = bytes.iter().zip(MAGIC.iter()).all(|(b, m)| *b == 0 || b == m);
    let whole_magic = bytes.len() >= MAGIC.len() && bytes[..MAGIC.len()] == MAGIC;
    if !zero_or_magic || (bytes.len() > RECORD_LEN && !whole_magic) {
        return Decoded::Foreign;
    }
    if bytes.len() != RECORD_LEN || !whole_magic {
        return Decoded::Uncertain(Uncertain::Torn);
    }
    if blake3::hash(&bytes[..CHECKSUM_AT]).as_bytes()[..16] != bytes[CHECKSUM_AT..] {
        return Decoded::Uncertain(Uncertain::Checksum);
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("four bytes"));
    if version != FORMAT_VERSION {
        return Decoded::Uncertain(Uncertain::UnknownVersion(version));
    }
    parse_v1(&bytes[12..CHECKSUM_AT])
        .map_or(Decoded::Uncertain(Uncertain::Malformed), Decoded::Record)
}

/// The eight fields of `format_version` 1, then zero padding to the checksum. `None` for anything else.
fn parse_v1(body: &[u8]) -> Option<LockRecord> {
    let mut at = 0usize;
    let complete_lock_key = next_field(body, &mut at)?;
    let operation_id = next_field(body, &mut at)?;
    let owner_instance_id = next_field(body, &mut at)?;
    let boot_session_id = next_field(body, &mut at)?;
    let target_path_key = next_field(body, &mut at)?;
    let workspace_path = next_field(body, &mut at)?;
    let creation_wall_time = time(&next_field(body, &mut at)?)?;
    let last_heartbeat_wall_time = time(&next_field(body, &mut at)?)?;
    // Everything after the fields is padding, and padding is zero: anything else is a layout this version does not
    // know, so it is Malformed rather than silently ignored.
    if body[at..].iter().any(|&b| b != 0) {
        return None;
    }
    Some(LockRecord {
        complete_lock_key,
        operation_id,
        owner_instance_id,
        boot_session_id,
        target_path_key,
        workspace_path,
        creation_wall_time,
        last_heartbeat_wall_time,
    })
}

/// One u16 LE length and that many bytes of UTF-8, advancing `at` past them.
fn next_field(body: &[u8], at: &mut usize) -> Option<String> {
    let len = u16::from_le_bytes(body.get(*at..*at + 2)?.try_into().ok()?) as usize;
    let text = std::str::from_utf8(body.get(*at + 2..*at + 2 + len)?).ok()?.to_string();
    *at += 2 + len;
    Some(text)
}

/// Decimal ASCII digits only: no sign, no space, no empty string (`str::parse` alone would accept a leading `+`).
fn time(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) { None } else { s.parse().ok() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> LockRecord {
        LockRecord {
            complete_lock_key: "vol:1:obj:77/dest".to_string(),
            operation_id: "0123456789abcdef0123456789abcdef".to_string(),
            owner_instance_id: "fedcba9876543210fedcba9876543210".to_string(),
            boot_session_id: "9f1c2d3e-0000-4000-8000-000000000001".to_string(),
            target_path_key: "dest".to_string(),
            workspace_path: "operations/0123456789abcdef0123456789abcdef".to_string(),
            creation_wall_time: 1_790_000_000_123_456_789,
            last_heartbeat_wall_time: 1_790_000_000_123_456_789,
        }
    }

    /// Re-checksum `bytes` after a test changed something before the checksum.
    fn reseal(mut bytes: Vec<u8>) -> Vec<u8> {
        let sum = blake3::hash(&bytes[..CHECKSUM_AT]);
        bytes[CHECKSUM_AT..].copy_from_slice(&sum.as_bytes()[..16]);
        bytes
    }

    #[test]
    fn a_record_round_trips_at_exactly_4096_bytes() {
        let bytes = sample().encode();
        assert_eq!(bytes.len(), RECORD_LEN);
        assert_eq!(bytes[..8], MAGIC);
        assert_eq!(decode(&bytes), Decoded::Record(sample()));
    }

    #[test]
    fn empty_and_torn_files_are_uncertain_never_foreign() {
        let good = sample().encode();
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("all zero, record length", vec![0u8; RECORD_LEN]),
            ("all zero, short", vec![0u8; 100]),
            ("a partly written magic", [b"FLUX".as_slice(), &[0u8; 4], &[9u8; 50]].concat()),
            ("a zero first sector, later bytes written", [&[0u8; 512][..], &good[512..]].concat()),
            ("the magic, truncated", good[..4000].to_vec()),
            ("the magic, too long", [good.as_slice(), &[1u8; 10]].concat()),
        ];
        assert_eq!(decode(&[]), Decoded::Uncertain(Uncertain::Empty));
        for (why, bytes) in cases {
            assert_eq!(decode(&bytes), Decoded::Uncertain(Uncertain::Torn), "{why}");
        }
    }

    #[test]
    fn a_failed_checksum_is_uncertain() {
        let mut bytes = sample().encode();
        bytes[20] ^= 1;
        assert_eq!(decode(&bytes), Decoded::Uncertain(Uncertain::Checksum));
    }

    #[test]
    fn a_newer_version_with_a_valid_checksum_is_uncertain_not_parsed() {
        let mut bytes = sample().encode();
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(decode(&reseal(bytes)), Decoded::Uncertain(Uncertain::UnknownVersion(2)));
    }

    #[test]
    fn nonzero_padding_or_a_non_numeric_time_is_malformed() {
        let mut bytes = sample().encode();
        bytes[CHECKSUM_AT - 1] = 1;
        assert_eq!(decode(&reseal(bytes)), Decoded::Uncertain(Uncertain::Malformed));
        let mut r = sample().encode();
        // The first digit of the creation time (the first match; the heartbeat carries the same value).
        let at = r.windows(19).position(|w| w == b"1790000000123456789").unwrap();
        r[at] = b'x';
        assert_eq!(decode(&reseal(r)), Decoded::Uncertain(Uncertain::Malformed));
    }

    #[test]
    fn other_content_is_foreign() {
        assert_eq!(decode(b"hello, this is somebody's file"), Decoded::Foreign);
        assert_eq!(
            decode(&[0u8; RECORD_LEN + 1]),
            Decoded::Foreign,
            "over 4096 bytes without the magic"
        );
        assert_eq!(
            decode(b"FLUXLOCX"),
            Decoded::Foreign,
            "one byte off the magic, neither zero nor magic"
        );
    }

    #[test]
    #[should_panic(expected = "more than")]
    fn an_encoder_that_would_exceed_the_size_panics_rather_than_truncate() {
        let mut r = sample();
        r.target_path_key = "k".repeat(5000);
        r.encode();
    }

    #[test]
    fn the_layout_is_the_documented_one_byte_for_byte() {
        let mut r = sample();
        // Distinct times, so that swapping the two time fields shows.
        r.last_heartbeat_wall_time = 1_790_000_000_987_654_321;
        let bytes = r.encode();
        assert_eq!(&bytes[..8], b"FLUXLOCK");
        assert_eq!(&bytes[8..12], &1u32.to_le_bytes());
        let creation = r.creation_wall_time.to_string();
        let heartbeat = r.last_heartbeat_wall_time.to_string();
        let mut at = 12;
        for field in [
            r.complete_lock_key.as_str(),
            r.operation_id.as_str(),
            r.owner_instance_id.as_str(),
            r.boot_session_id.as_str(),
            r.target_path_key.as_str(),
            r.workspace_path.as_str(),
            creation.as_str(),
            heartbeat.as_str(),
        ] {
            assert_eq!(
                &bytes[at..at + 2],
                &(field.len() as u16).to_le_bytes(),
                "length of {field}"
            );
            assert_eq!(&bytes[at + 2..at + 2 + field.len()], field.as_bytes());
            at += 2 + field.len();
        }
        assert!(bytes[at..CHECKSUM_AT].iter().all(|&b| b == 0), "zero padding up to the checksum");
        assert_eq!(&bytes[CHECKSUM_AT..], &blake3::hash(&bytes[..CHECKSUM_AT]).as_bytes()[..16]);
    }

    #[test]
    fn a_flipped_padding_or_checksum_byte_is_a_checksum_failure() {
        for at in [CHECKSUM_AT - 1, CHECKSUM_AT, RECORD_LEN - 1] {
            let mut bytes = sample().encode();
            bytes[at] ^= 1;
            assert_eq!(decode(&bytes), Decoded::Uncertain(Uncertain::Checksum), "byte {at}");
        }
    }

    #[test]
    fn a_signed_time_is_malformed() {
        let mut bytes = sample().encode();
        let at = bytes.windows(19).position(|w| w == b"1790000000123456789").unwrap();
        bytes[at] = b'+';
        assert_eq!(decode(&reseal(bytes)), Decoded::Uncertain(Uncertain::Malformed));
    }

    #[test]
    fn fields_that_exactly_fill_the_record_round_trip() {
        let mut r = sample();
        let others: usize = 12
            + [
                &r.complete_lock_key,
                &r.operation_id,
                &r.owner_instance_id,
                &r.boot_session_id,
                &r.workspace_path,
            ]
            .iter()
            .map(|f| 2 + f.len())
            .sum::<usize>()
            + 2
            + r.creation_wall_time.to_string().len()
            + 2
            + r.last_heartbeat_wall_time.to_string().len()
            + 2;
        r.target_path_key = "k".repeat(CHECKSUM_AT - others);
        let bytes = r.encode();
        assert_eq!(bytes.len(), RECORD_LEN);
        assert_ne!(
            bytes[CHECKSUM_AT - 1],
            0,
            "the last field reaches the checksum: no padding at all"
        );
        assert_eq!(decode(&bytes), Decoded::Record(r));
    }

    #[test]
    fn a_field_that_is_not_utf8_is_malformed() {
        let mut bytes = sample().encode();
        // The first byte of the first field's text: magic 8, version 4, then its u16 length.
        bytes[14] = 0xFF;
        assert_eq!(decode(&reseal(bytes)), Decoded::Uncertain(Uncertain::Malformed));
    }
}
