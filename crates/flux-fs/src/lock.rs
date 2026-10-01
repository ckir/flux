//! The destination lock's file (spec §96.1, §235, §259.6).
//!
//! Only the primitives: the protocol that uses them lives in `flux-core`.

use crate::{FileIdentity, Result};

/// What the filesystem that holds a lock file can promise (§235.1).
///
/// Only the two strong classes allow an operation that needs target exclusivity; the other two are refused with
/// `REMOTE_LOCK_UNSAFE` (§235.4: prefer refusing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockCapability {
    LocalStrong,
    RemoteStrong,
    RemoteUnverified,
    Unsupported,
}

impl LockCapability {
    /// §235.1's table: whether an operation needing target exclusivity may proceed.
    pub fn allows_exclusive(self) -> bool {
        matches!(self, Self::LocalStrong | Self::RemoteStrong)
    }
}

/// A lock file held open for reading and writing (§96.1, §259.6).
///
/// Dropping the handle closes it, and closing it releases the OS-native lock, as does the death of the process that
/// holds it. That is what makes a dead owner's lock recoverable (§240.2, §240.3).
///
/// The OS-native lock must never cover a byte that `read_all` or `write_at_start` touches: Windows range locks are
/// mandatory, so a lock over the record would make it unreadable to every other handle.
pub trait LockFile {
    /// Take the OS-native lock without waiting. `Ok(true)` means it was granted to THIS handle (again, if it already
    /// held it); `Ok(false)` means another handle holds it. Any other failure is an error, never `false`. Windows
    /// range locks do not nest, so an implementation remembers that its handle holds the lock rather than asking the
    /// OS again.
    fn try_lock(&self) -> Result<bool>;

    /// The file's bytes from offset 0, reading at most `limit + 1` bytes, so that a caller can tell a file longer than
    /// `limit` from one of exactly `limit`. Readable while another handle holds the OS-native lock: the model's
    /// classifier reads the record whether or not its try-lock succeeded (§240.1). Positional: it neither uses nor
    /// promises a file cursor. `limit` is a record-sized bound, never close to `usize::MAX`.
    fn read_all(&self, limit: usize) -> Result<Vec<u8>>;

    /// Write `bytes` at offset 0 in ONE write call (§259.6: "written in one write call"), then cut the file to exactly
    /// `bytes.len()`. A short write is an error. The cut matters for a takeover of an oversized torn lock: without it
    /// the new record would be followed by the old tail and read as uncertain (plan decision 5). Positional, like
    /// `read_all`.
    fn write_at_start(&self, bytes: &[u8]) -> Result<()>;

    /// Flush the file's data and metadata, including the length `write_at_start` set.
    fn sync_all(&self) -> Result<()>;

    /// The identity of the object this handle holds - not of a path (§107, §240.3 step 3).
    fn identity(&self) -> Result<FileIdentity>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_strong_classes_allow_an_exclusive_operation() {
        assert!(LockCapability::LocalStrong.allows_exclusive());
        assert!(LockCapability::RemoteStrong.allows_exclusive());
        assert!(!LockCapability::RemoteUnverified.allows_exclusive());
        assert!(!LockCapability::Unsupported.allows_exclusive());
    }
}
