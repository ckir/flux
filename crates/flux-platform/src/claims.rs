//! The `redb`-backed claim store (cut 8b). One file, two tables: `claims` (key =
//! `ClaimKey::encode`, value = `ClaimRecord::encode`) and `meta`.

use std::fs::File;
use std::io;

use flux_fs::{
    ClaimKey, ClaimOutcome, ClaimRecord, ClaimStatus, ClaimStore, Code, Durability, FluxPathKey,
    FsError, Result,
};
use redb::{Builder, Database, ReadableDatabase, ReadableTable, TableDefinition};

/// `redb`'s page cache: small and fixed (section 10.1).
pub const CACHE_BYTES: usize = 16 * 1024 * 1024;
/// Under `Durability::Normal`, the commit that makes this many unsynced commits is synced.
pub const SYNC_CAP: u32 = 1000;

const CLAIMS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("claims");
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
const FORMAT: u64 = 1;

fn io_err(e: impl std::error::Error + Send + Sync + 'static) -> FsError {
    FsError::new(Code::IoError, io::Error::other(e))
}

pub struct RedbClaimStore {
    db: Database,
    durability: Durability,
    unsynced: u32,
}

impl RedbClaimStore {
    /// `file` must be empty (a fresh `state.db`) or a valid database. Writes the `meta`
    /// entry `format` = 1 durably.
    pub fn from_file(file: File, durability: Durability) -> Result<Self> {
        let db = Builder::new().set_cache_size(CACHE_BYTES).create_file(file).map_err(io_err)?;
        let mut tx = db.begin_write().map_err(io_err)?;
        tx.set_durability(redb::Durability::Immediate).map_err(io_err)?;
        {
            tx.open_table(CLAIMS).map_err(io_err)?;
            let mut meta = tx.open_table(META).map_err(io_err)?;
            meta.insert("format", FORMAT).map_err(io_err)?;
        }
        tx.commit().map_err(io_err)?;
        Ok(RedbClaimStore { db, durability, unsynced: 0 })
    }

    /// Commits made since the last synced commit (always 0 under `Strict`).
    pub fn unsynced(&self) -> u32 {
        self.unsynced
    }

    /// The redb durability for the next claim commit, and the unsynced count after it.
    fn next_commit(&self) -> (redb::Durability, u32) {
        match self.durability {
            Durability::Strict => (redb::Durability::Immediate, 0),
            Durability::Normal if self.unsynced + 1 >= SYNC_CAP => (redb::Durability::Immediate, 0),
            Durability::Normal => (redb::Durability::None, self.unsynced + 1),
        }
    }

    fn read(&self, key: &ClaimKey) -> Result<Option<ClaimRecord>> {
        let tx = self.db.begin_read().map_err(io_err)?;
        let table = tx.open_table(CLAIMS).map_err(io_err)?;
        let got = table.get(key.encode().as_slice()).map_err(io_err)?;
        Ok(got.and_then(|g| ClaimRecord::decode(g.value())))
    }
}

impl ClaimStore for RedbClaimStore {
    fn insert_if_absent(&mut self, key: &ClaimKey, record: &ClaimRecord) -> Result<ClaimOutcome> {
        let (dur, after) = self.next_commit();
        let mut tx = self.db.begin_write().map_err(io_err)?;
        tx.set_durability(dur).map_err(io_err)?;
        {
            let mut table = tx.open_table(CLAIMS).map_err(io_err)?;
            let k = key.encode();
            if let Some(g) = table.get(k.as_slice()).map_err(io_err)? {
                let stored = ClaimRecord::decode(g.value()).ok_or_else(|| {
                    FsError::new(Code::IoError, io::Error::other("corrupt claim record"))
                })?;
                return Ok(ClaimOutcome::Present(stored));
            }
            table.insert(k.as_slice(), record.encode().as_slice()).map_err(io_err)?;
        }
        tx.commit().map_err(io_err)?;
        self.unsynced = after;
        Ok(ClaimOutcome::Inserted)
    }

    fn get(&self, key: &ClaimKey) -> Result<Option<ClaimRecord>> {
        self.read(key)
    }

    fn upgrade_own_claim(&mut self, key: &ClaimKey, target: &FluxPathKey) -> Result<()> {
        let (dur, after) = self.next_commit();
        let mut tx = self.db.begin_write().map_err(io_err)?;
        tx.set_durability(dur).map_err(io_err)?;
        {
            let mut table = tx.open_table(CLAIMS).map_err(io_err)?;
            let k = key.encode();
            let stored = match table.get(k.as_slice()).map_err(io_err)? {
                Some(g) => ClaimRecord::decode(g.value()),
                None => None,
            };
            match stored {
                Some(r) if r.target == *target => {}
                _ => {
                    return Err(FsError::new(
                        Code::IoError,
                        io::Error::other("upgrade of a missing or foreign claim"),
                    ));
                }
            }
            let upgraded = ClaimRecord { target: target.clone(), status: ClaimStatus::Created };
            table.insert(k.as_slice(), upgraded.encode().as_slice()).map_err(io_err)?;
        }
        tx.commit().map_err(io_err)?;
        self.unsynced = after;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.durability == Durability::Normal && self.unsynced > 0 {
            let mut tx = self.db.begin_write().map_err(io_err)?;
            tx.set_durability(redb::Durability::Immediate).map_err(io_err)?;
            tx.commit().map_err(io_err)?;
            self.unsynced = 0;
        }
        Ok(())
    }
}
