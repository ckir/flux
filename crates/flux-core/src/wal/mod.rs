// No caller until cut 9f-1 plan 2 wires the WAL into the copy loop; plan 2 removes this line.
#![allow(dead_code)]

//! Write-ahead log for chunk checkpoints (cut 9f-1).

/// Size of one checkpointed chunk of the source: 1 MiB.
pub(crate) const CHUNK_SIZE: u64 = 1 << 20;

/// A BLAKE3 digest of one chunk.
pub(crate) type Digest = [u8; 32];

/// First four bytes of every WAL record.
pub(crate) const MAGIC: [u8; 4] = *b"FWAL";
/// Record layout version.
pub(crate) const FORMAT_VERSION: u16 = 1;
/// Checksum rule version (1 = first 16 bytes of BLAKE3).
pub(crate) const CHECKSUM_VERSION: u8 = 1;
/// Final byte of a complete record.
pub(crate) const COMMIT: u8 = 0xC3;
/// Largest accepted `payload_len`; above it a record is corrupt, never torn.
pub(crate) const MAX_PAYLOAD: u32 = 1 << 20;

pub(crate) mod digest;
pub(crate) mod record;
pub(crate) mod replay;
