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
        Self::with_cache_size(file, durability, CACHE_BYTES)
    }

    fn with_cache_size(file: File, durability: Durability, cache_bytes: usize) -> Result<Self> {
        let db = Builder::new().set_cache_size(cache_bytes).create_file(file).map_err(io_err)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use flux_fs::{ClaimStatus, ObjectId};
    use std::ffi::OsStr;

    #[test]
    fn the_cache_bound_is_16_mib() {
        assert_eq!(CACHE_BYTES, 16 * 1024 * 1024);
    }

    /// Needs redb's `cache_metrics` (a dev-dependency feature); without it `cache_stats()` is all zeros.
    #[test]
    fn the_cache_size_setting_reaches_redb() {
        const BOUND: usize = 1 << 20;
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.path().join("state.db"))
            .unwrap();
        let mut store = RedbClaimStore::with_cache_size(file, Durability::Normal, BOUND).unwrap();
        let parent = ObjectId { volume: 1, index: 1 };
        // About 4x the bound in pages: 1000 claims of ~4000 bytes.
        for i in 0u32..1000 {
            let key = ClaimKey::new(parent, OsStr::new(&format!("claim-{i}")));
            let mut target = format!("t{i}-").into_bytes();
            target.resize(4000, b'x');
            let rec = ClaimRecord { target: FluxPathKey(target), status: ClaimStatus::Existing };
            store.insert_if_absent(&key, &rec).unwrap();
        }
        let stats = store.db.cache_stats();
        eprintln!("MEASURED evictions={} used_bytes={}", stats.evictions(), stats.used_bytes());
        assert!(stats.evictions() > 0, "{stats:?}");
        assert!(stats.used_bytes() <= BOUND + BOUND / 8, "{stats:?}");
    }
}
