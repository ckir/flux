//! A lock this run holds: its file's handle and OS-native lock, and the record once one is written.

use super::error::LockResult;
use super::record::{Decoded, LockRecord, RECORD_LEN, decode};
use flux_fs::{DirHandle, FileIdentity, LockFile};
use std::ffi::OsString;
use std::io::ErrorKind;

/// A lock this run holds. It has no record until `write_record` (F5: the record names state that already exists).
pub struct Held<'a, D: DirHandle> {
    pub(crate) dir: &'a D,
    pub(crate) lock_name: OsString,
    pub(crate) lock: D::Lock,
    pub(crate) identity: FileIdentity,
    /// Boxed: an inline record makes `Held` about 250 bytes, and every enum carrying one trips
    /// `clippy::large_enum_variant`.
    pub(crate) record: Option<Box<LockRecord>>,
}

/// What `release` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Released {
    /// Still owned: the lock was unlinked by name, then closed (`S99_release`, `S99_release_lands`, `S99_close`).
    Unlinked,
    /// No longer this operation's: closed without unlinking (`S99_refuse_close`).
    NotOwned,
}

impl<'a, D: DirHandle> Held<'a, D> {
    pub fn identity(&self) -> &FileIdentity {
        &self.identity
    }

    pub fn record(&self) -> Option<&LockRecord> {
        self.record.as_deref()
    }

    /// `S96_1_record_begin` / `S96_1_record_end`, and after a recovery `S240_3_s4_record_begin` /
    /// `S240_3_s4_record_end`: this operation's record, in one write through the handle holding the lock, flushed.
    /// Called once, after the operation's state exists (F5).
    pub fn write_record(&mut self, record: LockRecord) -> LockResult<()> {
        assert!(self.record.is_none(), "a lock record is written once per tenure");
        self.lock.write_at_start(&record.encode())?;
        self.lock.sync_all()?;
        self.record = Some(Box::new(record));
        Ok(())
    }

    /// §99, `S99_check` (and `S99_release_check`): the lock path still names the file this run holds, AND the record
    /// in it is still this operation's. Both: a §240.5 takeover overwrites the record in place, so the identity alone
    /// stays the same (spec:4882-4885). Never opens the lock path (the design's "cheap by construction").
    pub fn still_owned(&self) -> LockResult<bool> {
        let Some(mine) = &self.record else { return Ok(false) };
        let at_path = match self.dir.metadata(&self.lock_name) {
            Ok(m) => m.identity,
            Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        // Decision 9: two Unavailable identities prove nothing.
        if matches!(self.identity, FileIdentity::Unavailable) || at_path != self.identity {
            return Ok(false);
        }
        Ok(match decode(&self.lock.read_all(RECORD_LEN)?) {
            Decoded::Record(r) => {
                r.operation_id == mine.operation_id && r.owner_instance_id == mine.owner_instance_id
            }
            Decoded::Uncertain(_) | Decoded::Foreign => false,
        })
    }

    /// `S99_release_check`, then `S99_release` / `S99_release_lands` / `S99_close`: unlink the lock by name only
    /// while it is still this operation's, then close. Otherwise `S99_refuse_close`: close without unlinking, since the
    /// path may now name another operation's lock.
    pub fn release(self) -> LockResult<Released> {
        if !self.still_owned()? {
            return Ok(Released::NotOwned);
        }
        self.dir.remove_file(&self.lock_name)?;
        Ok(Released::Unlinked)
    }

    /// Remove a lock this run created and wrote no record into (a refusal after acquisition, "The run" step 4).
    /// Decision 7: unlinked WHILE the OS-native lock is still held, and only if the path still names this file; then
    /// closed.
    pub fn discard(self) -> LockResult<()> {
        assert!(self.record.is_none(), "discard is for a lock with no record");
        let at_path = match self.dir.metadata(&self.lock_name) {
            Ok(m) => m.identity,
            Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if at_path == self.identity {
            self.dir.remove_file(&self.lock_name)?;
        }
        Ok(())
    }
}
