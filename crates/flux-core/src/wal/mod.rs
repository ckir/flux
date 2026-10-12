// No caller until cut 9f-1 plan 2 wires the WAL into the copy loop; plan 2 removes this line.
#![allow(dead_code)]

//! Write-ahead log for chunk checkpoints (cut 9f-1).

/// Size of one checkpointed chunk of the source: 1 MiB.
pub(crate) const CHUNK_SIZE: u64 = 1 << 20;

/// A BLAKE3 digest of one chunk.
pub(crate) type Digest = [u8; 32];

pub(crate) mod digest;
