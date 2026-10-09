//! The `redb`-backed claim store (cut 8b, format 2 in cut 9c). One file, three tables: `claims`
//! (key = `ClaimKey::encode`, value = `ClaimRecord::encode`), `prepared` (key = `ClaimKey::encode`,
//! value = `PreparedRecord::encode`; absent in a format-1 store) and `meta`.

use std::fs::File;
use std::io;

use flux_fs::{
    ClaimKey, ClaimOutcome, ClaimRecord, ClaimStatus, ClaimStore, Code, Durability, FluxPathKey,
    FsError, ObjectId, PreparedRecord, RecoveryOp, Result,
};
use redb::{
    Builder, Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition,
};

/// `redb`'s page cache: small and fixed (section 10.1).
pub const CACHE_BYTES: usize = 16 * 1024 * 1024;
/// Under `Durability::Normal`, the commit that makes this many unsynced commits is synced.
pub const SYNC_CAP: u32 = 1000;

const CLAIMS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("claims");
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
const PREPARED: TableDefinition<&[u8], &[u8]> = TableDefinition::new("prepared");
/// Written by `create_file`. `open_file` also reads 1 (no `prepared` table), never upgrading it.
pub const FORMAT: u64 = 2;

fn io_err(e: impl std::error::Error + Send + Sync + 'static) -> FsError {
    FsError::new(Code::IoError, io::Error::other(e))
}

fn storage_err(e: redb::StorageError) -> FsError {
    match e {
        redb::StorageError::Corrupted(_) => FsError::new(Code::StateCorrupt, io::Error::other(e)),
        // redb reports a non-empty file with the wrong magic number as `Io(InvalidData)`, not `Corrupted`
        // (redb-4.3.0 src/tree_store/page_store/page_manager.rs:1136). That is a corrupt store, not an I/O failure.
        redb::StorageError::Io(ref io_e) if io_e.kind() == io::ErrorKind::InvalidData => {
            FsError::new(Code::StateCorrupt, io::Error::other(e))
        }
        // `Io`, `PreviousIo` and every other storage error.
        _ => FsError::new(Code::IoError, io::Error::other(e)),
    }
}

fn database_err(e: redb::DatabaseError) -> FsError {
    match e {
        redb::DatabaseError::RepairAborted => FsError::new(Code::StateCorrupt, io::Error::other(e)),
        redb::DatabaseError::UpgradeRequired(_) => {
            FsError::new(Code::IncompatibleState, io::Error::other(e))
        }
        redb::DatabaseError::Storage(s) => storage_err(s),
        // `DatabaseAlreadyOpen`, `TransactionInProgress` and anything else.
        _ => FsError::new(Code::IoError, io::Error::other(e)),
    }
}

fn table_err(e: redb::TableError) -> FsError {
    match e {
        redb::TableError::Storage(s) => storage_err(s),
        // `TableDoesNotExist` and every other table error.
        _ => FsError::new(Code::StateCorrupt, io::Error::other(e)),
    }
}

fn transaction_err(e: redb::TransactionError) -> FsError {
    match e {
        redb::TransactionError::Storage(s) => storage_err(s),
        _ => FsError::new(Code::IoError, io::Error::other(e)),
    }
}

pub struct RedbClaimStore {
    db: Database,
    durability: Durability,
    unsynced: u32,
    format: u64,
}

impl RedbClaimStore {
    /// `file` must be empty (a fresh `state.db`). Writes the `meta` entry `format` = 2 durably.
    pub fn create_file(file: File, durability: Durability) -> Result<Self> {
        Self::with_cache_size(file, durability, CACHE_BYTES)
    }

    /// Open an EXISTING store. A zero-length file is `Code::StateCorrupt` (redb would silently
    /// initialise it); a missing `meta` or `claims` table is `StateCorrupt`, and so is a
    /// missing `prepared` table in a format-2 store; a `format` other than 1 or 2 is
    /// `Code::IncompatibleState`. A format-1 store is opened as it is, never upgraded.
    pub fn open_file(file: File, durability: Durability) -> Result<Self> {
        Self::open_with_cache_size(file, durability, CACHE_BYTES)
    }

    fn open_with_cache_size(
        file: File,
        durability: Durability,
        cache_bytes: usize,
    ) -> Result<Self> {
        let len = file.metadata().map_err(FsError::from_io)?.len();
        if len == 0 {
            return Err(FsError::new(
                Code::StateCorrupt,
                io::Error::other("zero-length claim store"),
            ));
        }
        let db =
            Builder::new().set_cache_size(cache_bytes).create_file(file).map_err(database_err)?;
        let format;
        {
            let tx = db.begin_read().map_err(transaction_err)?;
            let meta = tx.open_table(META).map_err(table_err)?;
            match meta.get("format").map_err(storage_err)?.map(|g| g.value()) {
                Some(n @ (1 | 2)) => format = n,
                Some(n) => {
                    return Err(FsError::new(
                        Code::IncompatibleState,
                        io::Error::other(format!(
                            "claim store format {n}, this binary reads 1 and {FORMAT}"
                        )),
                    ));
                }
                None => {
                    return Err(FsError::new(
                        Code::StateCorrupt,
                        io::Error::other("claim store has no format"),
                    ));
                }
            }
            tx.open_table(CLAIMS).map_err(table_err)?;
            if format == 2 {
                tx.open_table(PREPARED).map_err(table_err)?;
            }
        }
        Ok(RedbClaimStore { db, durability, unsynced: 0, format })
    }

    fn with_cache_size(file: File, durability: Durability, cache_bytes: usize) -> Result<Self> {
        let db = Builder::new().set_cache_size(cache_bytes).create_file(file).map_err(io_err)?;
        let mut tx = db.begin_write().map_err(io_err)?;
        tx.set_durability(redb::Durability::Immediate).map_err(io_err)?;
        {
            tx.open_table(CLAIMS).map_err(io_err)?;
            tx.open_table(PREPARED).map_err(io_err)?;
            let mut meta = tx.open_table(META).map_err(io_err)?;
            meta.insert("format", FORMAT).map_err(io_err)?;
        }
        tx.commit().map_err(io_err)?;
        Ok(RedbClaimStore { db, durability, unsynced: 0, format: FORMAT })
    }

    /// The format read at open (or written by `create_file`): 1 or 2.
    pub fn format(&self) -> u64 {
        self.format
    }

    fn require_prepared(&self) -> Result<()> {
        if self.format == 2 {
            return Ok(());
        }
        Err(FsError::new(
            Code::IncompatibleState,
            io::Error::other("the claim store has no prepared table"),
        ))
    }

    /// One Immediate write transaction running `f`; any error drops the transaction, so nothing
    /// changes. Does not touch `unsynced` (an Immediate commit syncs earlier unsynced commits too,
    /// but the count is left for the next claim commit to reset).
    fn immediate<T>(&self, f: impl FnOnce(&redb::WriteTransaction) -> Result<T>) -> Result<T> {
        let mut tx = self.db.begin_write().map_err(io_err)?;
        tx.set_durability(redb::Durability::Immediate).map_err(io_err)?;
        let out = f(&tx)?;
        tx.commit().map_err(io_err)?;
        Ok(out)
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

    fn count(&self) -> Result<u64> {
        let tx = self.db.begin_read().map_err(transaction_err)?;
        let table = tx.open_table(CLAIMS).map_err(table_err)?;
        table.len().map_err(storage_err)
    }

    fn supports_prepared(&self) -> bool {
        self.format == 2
    }

    fn prepare(&mut self, key: &ClaimKey, record: &PreparedRecord) -> Result<()> {
        self.require_prepared()?;
        self.immediate(|tx| {
            let mut table = tx.open_table(PREPARED).map_err(io_err)?;
            let k = key.encode();
            if table.get(k.as_slice()).map_err(io_err)?.is_some() {
                return Err(FsError::new(
                    Code::IoError,
                    io::Error::other("a prepared note already exists at this key"),
                ));
            }
            table.insert(k.as_slice(), record.encode().as_slice()).map_err(io_err)?;
            Ok(())
        })
    }

    fn commit_prepared(
        &mut self,
        key: &ClaimKey,
        target: &FluxPathKey,
        planned: Option<&ClaimKey>,
    ) -> Result<()> {
        self.require_prepared()?;
        let op = RecoveryOp::Commit {
            key: key.clone(),
            target: target.clone(),
            planned: planned.cloned(),
        };
        self.immediate(|tx| apply_in(tx, &op))
    }

    fn discard_prepared(&mut self, key: &ClaimKey) -> Result<()> {
        self.require_prepared()?;
        let op = RecoveryOp::Discard { key: key.clone() };
        self.immediate(|tx| apply_in(tx, &op))
    }

    fn prepared(&self) -> Result<Vec<(ClaimKey, PreparedRecord)>> {
        if self.format != 2 {
            return Ok(Vec::new());
        }
        let tx = self.db.begin_read().map_err(transaction_err)?;
        let table = tx.open_table(PREPARED).map_err(table_err)?;
        let mut out = Vec::new();
        for entry in table.iter().map_err(storage_err)? {
            let (k, v) = entry.map_err(storage_err)?;
            let corrupt =
                |what: &str| FsError::new(Code::StateCorrupt, io::Error::other(what.to_string()));
            let key = decode_key(k.value()).ok_or_else(|| corrupt("corrupt prepared key"))?;
            let rec = PreparedRecord::decode(v.value())
                .ok_or_else(|| corrupt("corrupt prepared note"))?;
            out.push((key, rec));
        }
        Ok(out)
    }

    fn apply_recovery(&mut self, ops: &[RecoveryOp]) -> Result<()> {
        self.require_prepared()?;
        self.immediate(|tx| ops.iter().try_for_each(|op| apply_in(tx, op)))
    }
}

/// Inverse of `ClaimKey::encode` (volume u64 BE, index u128 BE, name bytes).
fn decode_key(bytes: &[u8]) -> Option<ClaimKey> {
    let (volume, rest) = bytes.split_first_chunk::<8>()?;
    let (index, name) = rest.split_first_chunk::<16>()?;
    Some(ClaimKey {
        parent: ObjectId {
            volume: u64::from_be_bytes(*volume),
            index: u128::from_be_bytes(*index),
        },
        name: name.to_vec(),
    })
}

/// One recovery operation inside `tx`: `commit_prepared`'s and `discard_prepared`'s rules.
fn apply_in(tx: &redb::WriteTransaction, op: &RecoveryOp) -> Result<()> {
    let mut prepared = tx.open_table(PREPARED).map_err(io_err)?;
    match op {
        RecoveryOp::Discard { key } => {
            prepared.remove(key.encode().as_slice()).map_err(io_err)?;
        }
        RecoveryOp::Commit { key, target, planned } => {
            let mut claims = tx.open_table(CLAIMS).map_err(io_err)?;
            let foreign = || {
                FsError::new(
                    Code::IoError,
                    io::Error::other("a claim owned by another target is in the way"),
                )
            };
            let owned_by_other =
                |claims: &redb::Table<&[u8], &[u8]>, k: &[u8]| -> Result<Option<bool>> {
                    // None: absent. Some(false): ours. Some(true): foreign. An undecodable record is a corrupt store
                    // (as on every other path), not somebody else's claim.
                    match claims.get(k).map_err(io_err)? {
                        None => Ok(None),
                        Some(g) => match ClaimRecord::decode(g.value()) {
                            Some(r) => Ok(Some(r.target != *target)),
                            None => Err(FsError::new(
                                Code::StateCorrupt,
                                io::Error::other("undecodable claim record"),
                            )),
                        },
                    }
                };
            let k = key.encode();
            if owned_by_other(&claims, k.as_slice())? == Some(true) {
                return Err(foreign());
            }
            let pk = planned.as_ref().map(|p| p.encode());
            let planned_absent = match &pk {
                Some(pk) => match owned_by_other(&claims, pk.as_slice())? {
                    Some(true) => return Err(foreign()),
                    Some(false) => false,
                    None => true,
                },
                None => false,
            };
            let created = ClaimRecord { target: target.clone(), status: ClaimStatus::Created };
            claims.insert(k.as_slice(), created.encode().as_slice()).map_err(io_err)?;
            if let (Some(pk), true) = (&pk, planned_absent) {
                claims.insert(pk.as_slice(), created.encode().as_slice()).map_err(io_err)?;
            }
            prepared.remove(k.as_slice()).map_err(io_err)?;
        }
    }
    Ok(())
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
