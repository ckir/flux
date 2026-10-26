// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The `redb`-backed hardlink index (cut 12). One file, one table `anchors` (key = `encode_object`, value =
//! `HardlinkEntry::encode`). The index is per-run scratch rebuilt by the next run, so no commit is synced.

use std::fs::File;
use std::io;

use crate::claims::{CACHE_BYTES, io_err, storage_err, table_err, transaction_err};
use flux_fs::{Code, FsError, HardlinkEntry, HardlinkIndex, ObjectId, Result, encode_object};
use redb::{Builder, Database, ReadableDatabase, ReadableTableMetadata, TableDefinition};

const ANCHORS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("anchors");

pub struct RedbHardlinkIndex {
    db: Database,
}

impl RedbHardlinkIndex {
    /// `file` must be empty (a fresh index file).
    pub fn create_file(file: File) -> Result<Self> {
        Self::with_cache_size(file, CACHE_BYTES)
    }

    /// As `create_file` with `redb`'s page cache bounded to `cache_bytes`.
    pub fn with_cache_size(file: File, cache_bytes: usize) -> Result<Self> {
        let db = Builder::new().set_cache_size(cache_bytes).create_file(file).map_err(io_err)?;
        let mut tx = db.begin_write().map_err(io_err)?;
        tx.set_durability(redb::Durability::None).map_err(io_err)?;
        tx.open_table(ANCHORS).map_err(io_err)?;
        tx.commit().map_err(io_err)?;
        Ok(RedbHardlinkIndex { db })
    }
}

impl HardlinkIndex for RedbHardlinkIndex {
    fn get(&self, id: &ObjectId) -> Result<Option<HardlinkEntry>> {
        let tx = self.db.begin_read().map_err(transaction_err)?;
        let table = tx.open_table(ANCHORS).map_err(table_err)?;
        let got = table.get(encode_object(id).as_slice()).map_err(storage_err)?;
        match got {
            None => Ok(None),
            Some(g) => match HardlinkEntry::decode(g.value()) {
                Some(e) => Ok(Some(e)),
                None => Err(FsError::new(
                    Code::StateCorrupt,
                    io::Error::other("undecodable hardlink index entry"),
                )),
            },
        }
    }

    fn insert(&mut self, id: &ObjectId, entry: &HardlinkEntry) -> Result<()> {
        let mut tx = self.db.begin_write().map_err(io_err)?;
        tx.set_durability(redb::Durability::None).map_err(io_err)?;
        {
            let mut table = tx.open_table(ANCHORS).map_err(io_err)?;
            table
                .insert(encode_object(id).as_slice(), entry.encode().as_slice())
                .map_err(io_err)?;
        }
        tx.commit().map_err(io_err)?;
        Ok(())
    }

    fn count(&self) -> Result<u64> {
        let tx = self.db.begin_read().map_err(transaction_err)?;
        let table = tx.open_table(ANCHORS).map_err(table_err)?;
        table.len().map_err(storage_err)
    }
}
