// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Tier 2 of the destination index: a `redb` database whose storage is one anonymous scratch file (never synced),
//! holding the rows this run publishes. Every row's key is a 25-byte directory prefix followed by the datum, so a
//! directory's rows are one contiguous key range and retiring a directory is a bounded range delete.

use crate::scratch::{Poison, read_fully_at, write_all_at};
use crate::spill::os_name;
use crate::walk::Tick;
use flux_fs::{Code, FsError, ObjectId};
use redb::{
    CommitError, Database, DatabaseError, Durability, ReadableTable, SetDurabilityError,
    StorageBackend, StorageError, TableDefinition, TableError, TransactionError, Value,
    WriteTransaction,
};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::ops::ControlFlow;
use std::rc::Rc;

const NAMES2: TableDefinition<&[u8], ()> = TableDefinition::new("names2");
const OWNERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("owners");
const LEARNED: TableDefinition<&[u8], u64> = TableDefinition::new("learned");
const IDS: TableDefinition<&[u8], u64> = TableDefinition::new("ids");

/// Bytes of a directory prefix.
const PREFIX: usize = 25;
/// Bytes of an `ObjectId` inside an `ids` key: the volume (8, big-endian) then the index (16, big-endian).
const ID_BYTES: usize = 24;

fn fatal(message: impl Into<String>) -> FsError {
    FsError::new(Code::IoError, io::Error::other(message.into()))
}

/// Re-makes an error from its parts: `FsError` is not `Clone` and its source is never rebuilt with context.
pub(crate) fn remake(e: &FsError) -> FsError {
    FsError::new(e.code, io::Error::new(e.source.kind(), e.source.to_string()))
}

fn from_storage(e: StorageError) -> FsError {
    match e {
        // Classified here, on the original error, so `ENOSPC` stays `DiskFull`.
        StorageError::Io(io) => {
            FsError::new(FsError::classify(&io), io::Error::new(io.kind(), io.to_string()))
        }
        other => fatal(format!("scratch store: {other}")),
    }
}

/// Maps the several error types `redb` returns onto `FsError`.
trait IntoFs {
    fn into_fs(self) -> FsError;
}

impl IntoFs for StorageError {
    fn into_fs(self) -> FsError {
        from_storage(self)
    }
}

impl IntoFs for TableError {
    fn into_fs(self) -> FsError {
        match self {
            TableError::Storage(s) => from_storage(s),
            other => fatal(format!("scratch store: {other}")),
        }
    }
}

impl IntoFs for CommitError {
    fn into_fs(self) -> FsError {
        match self {
            CommitError::Storage(s) => from_storage(s),
            other => fatal(format!("scratch store: {other}")),
        }
    }
}

impl IntoFs for DatabaseError {
    fn into_fs(self) -> FsError {
        match self {
            DatabaseError::Storage(s) => from_storage(s),
            other => fatal(format!("scratch store: {other}")),
        }
    }
}

impl IntoFs for TransactionError {
    fn into_fs(self) -> FsError {
        match self {
            TransactionError::Storage(s) => from_storage(s),
            other => fatal(format!("scratch store: {other}")),
        }
    }
}

impl IntoFs for SetDurabilityError {
    fn into_fs(self) -> FsError {
        fatal(format!("scratch store: {self}"))
    }
}

/// Test-only fault injection and counters, thread-local because the `Database` owns its backend once created and the
/// store is only ever called from the thread that owns it. Nothing counts, and no armed fault fires, until
/// `created()` (called at the end of `ScratchSpace::store()`'s creation).
#[cfg(test)]
pub(crate) mod faults {
    use std::cell::Cell;
    use std::io;

    thread_local! {
        static COUNTING: Cell<bool> = const { Cell::new(false) };
        static WRITES: Cell<u64> = const { Cell::new(0) };
        static COMMITS: Cell<u64> = const { Cell::new(0) };
        static WRITE_AT: Cell<Option<u64>> = const { Cell::new(None) };
        static COMMIT_AT: Cell<Option<u64>> = const { Cell::new(None) };
        static AT_FIRST_RETIRE: Cell<Option<u64>> = const { Cell::new(None) };
        static FINISHED: Cell<bool> = const { Cell::new(false) };
    }

    /// Zeroes the counters and starts counting.
    pub(crate) fn created() {
        WRITES.set(0);
        COMMITS.set(0);
        COUNTING.set(true);
    }

    fn target(now: &'static std::thread::LocalKey<Cell<u64>>, nth: u64) -> u64 {
        // Armed before the run: counting starts from zero. Armed after `store()`: `nth` counts from now.
        if COUNTING.get() { now.get() + nth } else { nth }
    }

    /// The `nth` backend `write` from now (from creation, if armed before it) fails with `ENOSPC`.
    pub(crate) fn arm_write_failure(nth: u64) {
        WRITE_AT.set(Some(target(&WRITES, nth)));
    }

    /// The `nth` commit from now (from creation, if armed before it) fails.
    pub(crate) fn arm_commit_failure(nth: u64) {
        COMMIT_AT.set(Some(target(&COMMITS, nth)));
    }

    pub(crate) fn writes() -> u64 {
        WRITES.get()
    }

    pub(crate) fn commits() -> u64 {
        COMMITS.get()
    }

    /// `commits()` as it stood when the first `Store::retire` began.
    pub(crate) fn commits_at_first_retire() -> u64 {
        AT_FIRST_RETIRE.get().unwrap_or(0)
    }

    /// Whether `Store::finish` has been called on this thread.
    pub(crate) fn finish_ran() -> bool {
        FINISHED.get()
    }

    pub(super) fn on_finish() {
        FINISHED.set(true);
    }

    pub(super) fn on_retire() {
        if AT_FIRST_RETIRE.get().is_none() {
            AT_FIRST_RETIRE.set(Some(COMMITS.get()));
        }
    }

    pub(super) fn on_write() -> io::Result<()> {
        if !COUNTING.get() {
            return Ok(());
        }
        WRITES.set(WRITES.get() + 1);
        if WRITE_AT.get() == Some(WRITES.get()) {
            WRITE_AT.set(None);
            let enospc = if cfg!(windows) { 112 } else { 28 };
            return Err(io::Error::from_raw_os_error(enospc));
        }
        Ok(())
    }

    pub(super) fn on_commit() -> Result<(), flux_fs::FsError> {
        if !COUNTING.get() {
            return Ok(());
        }
        COMMITS.set(COMMITS.get() + 1);
        if COMMIT_AT.get() == Some(COMMITS.get()) {
            COMMIT_AT.set(None);
            return Err(super::fatal("injected commit failure"));
        }
        Ok(())
    }
}

/// The database's storage: one anonymous scratch file, never synced (its contents are worthless after a crash).
#[derive(Debug)]
struct ScratchBackend {
    file: File,
    shown: String,
}

impl StorageBackend for ScratchBackend {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.file.metadata()?.len())
    }

    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        let got = read_fully_at(&self.file, out, offset)?;
        if got < out.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{}: short read at {offset}: {got} of {} bytes", self.shown, out.len()),
            ));
        }
        Ok(())
    }

    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        self.file.set_len(len)
    }

    fn sync_data(&self) -> Result<(), io::Error> {
        Ok(())
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        #[cfg(test)]
        faults::on_write()?;
        write_all_at(&self.file, data, offset)
    }
}

/// Adds one to the big-endian number in `bytes`; false when it was all `0xFF`.
fn increment(bytes: &mut [u8]) -> bool {
    for b in bytes.iter_mut().rev() {
        if *b == 0xFF {
            *b = 0;
        } else {
            *b += 1;
            return true;
        }
    }
    false
}

/// The directory a row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirKey {
    /// A directory whose identity is trusted.
    Strong(ObjectId),
    /// A directory without a trusted identity, numbered by `ScratchSpace::next_ordinal`.
    Ordinal(u64),
}

impl DirKey {
    /// Tag 0, the volume (8 BE), the index (16 BE) for a `Strong` key; tag 1, 16 zero bytes, the ordinal (8 BE)
    /// otherwise.
    pub(crate) fn prefix(&self) -> [u8; PREFIX] {
        let mut out = [0u8; PREFIX];
        match self {
            DirKey::Strong(id) => {
                out[1..9].copy_from_slice(&id.volume.to_be_bytes());
                out[9..25].copy_from_slice(&id.index.to_be_bytes());
            }
            DirKey::Ordinal(n) => {
                out[0] = 1;
                out[17..25].copy_from_slice(&n.to_be_bytes());
            }
        }
        out
    }

    /// The prefix and its successor: the directory's range is `[prefix, successor)`.
    pub(crate) fn bounds(&self) -> ([u8; PREFIX], [u8; PREFIX]) {
        let lo = self.prefix();
        let mut hi = lo;
        // Cannot overflow: byte 0 is 0 or 1, so a carry stops there.
        increment(&mut hi);
        (lo, hi)
    }
}

fn id_bytes(id: ObjectId) -> [u8; ID_BYTES] {
    let mut out = [0u8; ID_BYTES];
    out[..8].copy_from_slice(&id.volume.to_be_bytes());
    out[8..].copy_from_slice(&id.index.to_be_bytes());
    out
}

fn row_key(dir: &DirKey, spelling: &OsStr) -> Vec<u8> {
    let mut key = dir.prefix().to_vec();
    key.extend_from_slice(spelling.as_encoded_bytes());
    key
}

/// Up to `limit` keys of `table` in `[lo, hi)`, in key order.
fn keys_in<V: Value + 'static>(
    txn: &WriteTransaction,
    def: TableDefinition<&[u8], V>,
    lo: &[u8],
    hi: &[u8],
    limit: usize,
) -> Result<Vec<Vec<u8>>, FsError> {
    let table = txn.open_table(def).map_err(IntoFs::into_fs)?;
    let mut out = Vec::new();
    for row in table.range(lo..hi).map_err(IntoFs::into_fs)?.take(limit) {
        let (key, _) = row.map_err(IntoFs::into_fs)?;
        out.push(key.value().to_vec());
    }
    Ok(out)
}

/// The tier-2 rows of a run (see the module comment).
pub(crate) struct Store {
    db: Database,
    txn: Option<WriteTransaction>,
    writes_since_commit: usize,
    commit_every: usize,
    poison: Rc<Poison>,
}

impl Drop for Store {
    fn drop(&mut self) {
        // The live transaction goes before the database, so the backend (and the file) is released here.
        self.txn.take();
    }
}

impl Store {
    /// A store over `file`. The caller poisons the space on `Err`.
    pub(crate) fn create(
        file: File,
        shown: String,
        cache: usize,
        commit_every: usize,
        poison: Rc<Poison>,
    ) -> Result<Store, FsError> {
        let db = Database::builder()
            .set_cache_size(cache)
            .create_with_backend(ScratchBackend { file, shown })
            .map_err(IntoFs::into_fs)?;
        let mut store = Store {
            db,
            txn: None,
            writes_since_commit: 0,
            commit_every: commit_every.max(1),
            poison,
        };
        store.begin()?;
        Ok(store)
    }

    fn begin(&mut self) -> Result<(), FsError> {
        let mut txn = self.db.begin_write().map_err(IntoFs::into_fs)?;
        txn.set_durability(Durability::Immediate).map_err(IntoFs::into_fs)?;
        self.txn = Some(txn);
        Ok(())
    }

    fn txn(&self) -> Result<&WriteTransaction, FsError> {
        self.txn.as_ref().ok_or_else(|| fatal("the scratch store has no live transaction"))
    }

    /// Checks the poison first; an error from `f` poisons the space with a copy of itself (never with something
    /// that came out of `Poison::error()`, which would nest its prefix) and is returned.
    fn run<T>(&mut self, f: impl FnOnce(&mut Store) -> Result<T, FsError>) -> Result<T, FsError> {
        self.poison.check()?;
        match f(self) {
            Ok(v) => Ok(v),
            Err(e) => {
                self.poison.set(remake(&e));
                Err(e)
            }
        }
    }

    /// Commits the live transaction and opens its successor.
    fn rotate(&mut self) -> Result<(), FsError> {
        self.commit()?;
        self.writes_since_commit = 0;
        self.begin()
    }

    fn commit(&mut self) -> Result<(), FsError> {
        let txn =
            self.txn.take().ok_or_else(|| fatal("the scratch store has no live transaction"))?;
        #[cfg(test)]
        faults::on_commit()?;
        txn.commit().map_err(IntoFs::into_fs)
    }

    /// Counts one write; at `commit_every` writes commits and rotates the transaction.
    fn after_write(&mut self) -> Result<(), FsError> {
        self.writes_since_commit += 1;
        if self.writes_since_commit >= self.commit_every {
            self.rotate()?;
        }
        Ok(())
    }

    pub(crate) fn publish_name(&mut self, dir: &DirKey, spelling: &OsStr) -> Result<(), FsError> {
        self.run(|s| {
            let key = row_key(dir, spelling);
            {
                let mut t = s.txn()?.open_table(NAMES2).map_err(IntoFs::into_fs)?;
                t.insert(&key[..], ()).map_err(IntoFs::into_fs)?;
            }
            s.after_write()
        })
    }

    pub(crate) fn has_name(&mut self, dir: &DirKey, spelling: &OsStr) -> Result<bool, FsError> {
        self.run(|s| {
            let key = row_key(dir, spelling);
            let t = s.txn()?.open_table(NAMES2).map_err(IntoFs::into_fs)?;
            let found = t.get(&key[..]).map_err(IntoFs::into_fs)?.is_some();
            Ok(found)
        })
    }

    pub(crate) fn set_owner(
        &mut self,
        dir: &DirKey,
        spelling: &OsStr,
        target: &flux_fs::FluxPathKey,
    ) -> Result<(), FsError> {
        self.run(|s| {
            let key = row_key(dir, spelling);
            {
                let mut t = s.txn()?.open_table(OWNERS).map_err(IntoFs::into_fs)?;
                t.insert(&key[..], &target.0[..]).map_err(IntoFs::into_fs)?;
            }
            s.after_write()
        })
    }

    pub(crate) fn owner(
        &mut self,
        dir: &DirKey,
        spelling: &OsStr,
    ) -> Result<Option<flux_fs::FluxPathKey>, FsError> {
        self.run(|s| {
            let key = row_key(dir, spelling);
            let t = s.txn()?.open_table(OWNERS).map_err(IntoFs::into_fs)?;
            let found = t
                .get(&key[..])
                .map_err(IntoFs::into_fs)?
                .map(|v| flux_fs::FluxPathKey(v.value().to_vec()));
            Ok(found)
        })
    }

    /// Records `seq` for the spelling only if it has none yet.
    pub(crate) fn learn(
        &mut self,
        dir: &DirKey,
        spelling: &OsStr,
        seq: u64,
    ) -> Result<(), FsError> {
        self.run(|s| {
            let key = row_key(dir, spelling);
            let wrote = {
                let mut t = s.txn()?.open_table(LEARNED).map_err(IntoFs::into_fs)?;
                let present = t.get(&key[..]).map_err(IntoFs::into_fs)?.is_some();
                if !present {
                    t.insert(&key[..], seq).map_err(IntoFs::into_fs)?;
                }
                !present
            };
            if wrote { s.after_write() } else { Ok(()) }
        })
    }

    pub(crate) fn learned(
        &mut self,
        dir: &DirKey,
        spelling: &OsStr,
    ) -> Result<Option<u64>, FsError> {
        self.run(|s| {
            let key = row_key(dir, spelling);
            let t = s.txn()?.open_table(LEARNED).map_err(IntoFs::into_fs)?;
            let found = t.get(&key[..]).map_err(IntoFs::into_fs)?.map(|v| v.value());
            Ok(found)
        })
    }

    /// One `ids` row: the datum is the 24 id bytes then the spelling; a `None` spelling is a wipe row (the id bytes
    /// and nothing after). A `Some` spelling of length 0 is refused: `tree.rs` never publishes an empty name.
    pub(crate) fn ids_row(
        &mut self,
        dir: &DirKey,
        id: ObjectId,
        spelling: Option<&OsStr>,
        seq: u64,
    ) -> Result<(), FsError> {
        self.run(|s| {
            let name = spelling.map_or(&[][..], OsStr::as_encoded_bytes);
            if spelling.is_some() && name.is_empty() {
                return Err(fatal("an empty name was published to the scratch store"));
            }
            let mut key = dir.prefix().to_vec();
            key.extend_from_slice(&id_bytes(id));
            key.extend_from_slice(name);
            {
                let mut t = s.txn()?.open_table(IDS).map_err(IntoFs::into_fs)?;
                t.insert(&key[..], seq).map_err(IntoFs::into_fs)?;
            }
            s.after_write()
        })
    }

    /// Every `ids` row of `id` in `dir`, in key order: a wipe row is `None`.
    pub(crate) fn ids_rows(
        &mut self,
        dir: &DirKey,
        id: ObjectId,
    ) -> Result<Vec<(Option<OsString>, u64)>, FsError> {
        self.run(|s| {
            let lo = {
                let mut k = dir.prefix().to_vec();
                k.extend_from_slice(&id_bytes(id));
                k
            };
            let mut hi = lo.clone();
            // Cannot overflow: the first byte is 0 or 1.
            increment(&mut hi);
            let t = s.txn()?.open_table(IDS).map_err(IntoFs::into_fs)?;
            let mut out = Vec::new();
            for row in t.range(&lo[..]..&hi[..]).map_err(IntoFs::into_fs)? {
                let (key, seq) = row.map_err(IntoFs::into_fs)?;
                let rest = &key.value()[lo.len()..];
                out.push(((!rest.is_empty()).then(|| os_name(rest)), seq.value()));
            }
            Ok(out)
        })
    }

    /// Visits the directory's `names2` spellings in byte order, `batch` keys per range read, each page resuming
    /// after the last key of the one before. Nothing grows with the row count.
    pub(crate) fn for_each_name2(
        &mut self,
        dir: &DirKey,
        batch: usize,
        visit: &mut dyn FnMut(OsString) -> Result<ControlFlow<()>, FsError>,
    ) -> Result<(), FsError> {
        let batch = batch.max(1);
        let (lo, hi) = dir.bounds();
        let mut lo = lo.to_vec();
        loop {
            // Only the page read is inside `run`: a visitor's error is the caller's and must not poison the store.
            let keys = self.run(|s| keys_in(s.txn()?, NAMES2, &lo, &hi, batch))?;
            for key in &keys {
                if visit(os_name(&key[PREFIX..]))?.is_break() {
                    return Ok(());
                }
            }
            let Some(last) = keys.last().filter(|_| keys.len() == batch) else {
                return Ok(());
            };
            // The smallest key after `last`.
            lo = last.clone();
            lo.push(0);
        }
    }

    /// Removes the directory's rows from all four tables, in passes of at most `commit_every` rows, committing and
    /// rotating after each and ticking between them, until every range is empty.
    pub(crate) fn retire(&mut self, dir: &DirKey, tick: &mut Tick<'_>) -> Result<(), FsError> {
        #[cfg(test)]
        faults::on_retire();
        self.run(|s| {
            let (lo, hi) = dir.bounds();
            loop {
                let mut room = s.commit_every;
                let mut removed = 0;
                removed += s.remove_range(NAMES2, &lo, &hi, &mut room)?;
                removed += s.remove_range(OWNERS, &lo, &hi, &mut room)?;
                removed += s.remove_range(LEARNED, &lo, &hi, &mut room)?;
                removed += s.remove_range(IDS, &lo, &hi, &mut room)?;
                if removed == 0 {
                    return Ok(());
                }
                s.rotate()?;
                tick()?;
            }
        })
    }

    /// Removes up to `*room` keys of `[lo, hi)` from the table, lowering `*room` by what it removed.
    fn remove_range<V: Value + 'static>(
        &mut self,
        def: TableDefinition<&[u8], V>,
        lo: &[u8],
        hi: &[u8],
        room: &mut usize,
    ) -> Result<usize, FsError> {
        if *room == 0 {
            return Ok(0);
        }
        let txn = self.txn()?;
        let keys = keys_in(txn, def, lo, hi, *room)?;
        let mut table = txn.open_table(def).map_err(IntoFs::into_fs)?;
        for key in &keys {
            table.remove(&key[..]).map_err(IntoFs::into_fs)?;
        }
        *room -= keys.len();
        Ok(keys.len())
    }

    /// Commits the live transaction: the run's final commit, whose error is the fatal scratch error (spec 6.6).
    pub(crate) fn finish(&mut self) -> Result<(), FsError> {
        #[cfg(test)]
        faults::on_finish();
        self.run(|s| {
            if s.txn.is_none() {
                return Ok(());
            }
            s.commit()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use crate::scratch::{ScratchSpace, Thresholds};
    use flux_fs::{DestinationRoot, FileSystem, FluxPathKey};
    use std::path::{Path, PathBuf};

    fn thresholds() -> Thresholds {
        Thresholds::new(4, 8, 2, 16, 3).unwrap().with_index(4, 2, 4, 1 << 20, 2, 128).unwrap()
    }

    fn placed(fs: &FaultFs) -> ScratchSpace<'static> {
        fs.create_dir(Path::new("/s")).unwrap();
        let home = fs.destination_root(Path::new("/s")).unwrap();
        let space = ScratchSpace::unplaced(thresholds(), None);
        space.place(home, PathBuf::from("/s"));
        space
    }

    fn id(volume: u64, index: u128) -> ObjectId {
        ObjectId { volume, index }
    }

    fn name(s: &str) -> OsString {
        OsString::from(s)
    }

    fn creations(fs: &FaultFs) -> usize {
        fs.calls().iter().filter(|c| c.starts_with("create_scratch(")).count()
    }

    fn no_tick() -> impl FnMut() -> Result<(), FsError> {
        || Ok(())
    }

    #[test]
    fn a_store_is_created_on_first_use_in_an_unlinked_file() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let before = creations(&fs);
        let mut store = space.store().unwrap();
        assert_eq!(creations(&fs), before + 1);
        let dir = DirKey::Ordinal(1);
        store.publish_name(&dir, &name("a")).unwrap();
        assert!(store.has_name(&dir, &name("a")).unwrap());
        assert!(!store.has_name(&dir, &name("b")).unwrap());
    }

    #[test]
    fn two_directories_never_see_each_others_rows() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let mut store = space.store().unwrap();
        let strong = DirKey::Strong(id(1, 7));
        let ord = DirKey::Ordinal(1);
        store.publish_name(&strong, &name("a")).unwrap();
        store.publish_name(&ord, &name("a")).unwrap();
        store.set_owner(&strong, &name("a"), &FluxPathKey(b"S".to_vec())).unwrap();
        store.set_owner(&ord, &name("a"), &FluxPathKey(b"O".to_vec())).unwrap();
        assert_eq!(store.owner(&strong, &name("a")).unwrap(), Some(FluxPathKey(b"S".to_vec())));
        assert_eq!(store.owner(&ord, &name("a")).unwrap(), Some(FluxPathKey(b"O".to_vec())));
        let mut tick = no_tick();
        store.retire(&ord, &mut tick).unwrap();
        assert!(!store.has_name(&ord, &name("a")).unwrap());
        assert_eq!(store.owner(&ord, &name("a")).unwrap(), None);
        assert!(store.has_name(&strong, &name("a")).unwrap());
        assert_eq!(store.owner(&strong, &name("a")).unwrap(), Some(FluxPathKey(b"S".to_vec())));
    }

    #[test]
    fn a_retired_child_leaves_the_parents_rows() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let mut store = space.store().unwrap();
        let parent = DirKey::Strong(id(1, 7));
        let child = DirKey::Strong(id(1, 8));
        for d in [&parent, &child] {
            store.publish_name(d, &name("a")).unwrap();
            store.learn(d, &name("a"), 5).unwrap();
            store.ids_row(d, id(9, 9), Some(&name("a")), 1).unwrap();
        }
        let mut tick = no_tick();
        store.retire(&child, &mut tick).unwrap();
        assert!(!store.has_name(&child, &name("a")).unwrap());
        assert_eq!(store.learned(&child, &name("a")).unwrap(), None);
        assert!(store.ids_rows(&child, id(9, 9)).unwrap().is_empty());
        assert!(store.has_name(&parent, &name("a")).unwrap());
        assert_eq!(store.learned(&parent, &name("a")).unwrap(), Some(5));
        assert_eq!(store.ids_rows(&parent, id(9, 9)).unwrap().len(), 1);
    }

    #[test]
    fn retiring_a_lower_directory_leaves_every_higher_one() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let mut store = space.store().unwrap();
        let low = DirKey::Strong(id(1, 7));
        let higher = [DirKey::Strong(id(1, 8)), DirKey::Ordinal(1)];
        let obj = id(9, 9);
        let owner = FluxPathKey(b"T".to_vec());
        for d in [&low, &higher[0], &higher[1]] {
            store.publish_name(d, &name("a")).unwrap();
            store.set_owner(d, &name("a"), &owner).unwrap();
            store.learn(d, &name("a"), 5).unwrap();
            store.ids_row(d, obj, None, 1).unwrap();
            store.ids_row(d, obj, Some(&name("a")), 2).unwrap();
        }
        let mut tick = no_tick();
        store.retire(&low, &mut tick).unwrap();
        assert!(!store.has_name(&low, &name("a")).unwrap());
        assert_eq!(store.owner(&low, &name("a")).unwrap(), None);
        assert_eq!(store.learned(&low, &name("a")).unwrap(), None);
        assert!(store.ids_rows(&low, obj).unwrap().is_empty());
        for d in &higher {
            assert!(store.has_name(d, &name("a")).unwrap());
            assert_eq!(store.owner(d, &name("a")).unwrap(), Some(owner.clone()));
            assert_eq!(store.learned(d, &name("a")).unwrap(), Some(5));
            assert_eq!(store.ids_rows(d, obj).unwrap(), vec![(None, 1), (Some(name("a")), 2)]);
        }
    }

    #[test]
    fn the_ordinal_prefix_sorts_after_every_strong_prefix_and_ranges_do_not_overlap() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut dirs = vec![
            DirKey::Strong(id(0, 0)),
            DirKey::Strong(id(u64::MAX, u128::MAX)),
            DirKey::Ordinal(0),
            DirKey::Ordinal(u64::MAX),
        ];
        for i in 0..100u64 {
            let (a, b, c) = (next(), next(), next());
            dirs.push(DirKey::Strong(id(a, (u128::from(b) << 64) | u128::from(c))));
            dirs.push(DirKey::Ordinal(if i % 2 == 0 { a } else { i }));
        }
        dirs.sort_by_key(DirKey::prefix);
        dirs.dedup();
        let strong_max = dirs
            .iter()
            .filter(|d| matches!(d, DirKey::Strong(_)))
            .map(DirKey::prefix)
            .max()
            .unwrap();
        for d in dirs.iter().filter(|d| matches!(d, DirKey::Ordinal(_))) {
            assert!(d.prefix() > strong_max);
        }
        for pair in dirs.windows(2) {
            let (lo0, hi0) = pair[0].bounds();
            let (lo1, _) = pair[1].bounds();
            assert!(lo0 < hi0);
            assert!(hi0 <= lo1, "{:?} overlaps {:?}", pair[0], pair[1]);
        }
        for a in &dirs {
            let (lo, hi) = a.bounds();
            for b in dirs.iter().filter(|b| *b != a) {
                let (l, h) = b.bounds();
                assert!(hi <= l || h <= lo);
            }
        }
    }

    #[test]
    fn commits_rotate_the_transaction_and_reads_see_every_earlier_write() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let mut store = space.store().unwrap();
        let dir = DirKey::Ordinal(3);
        for i in 0..7 {
            store.publish_name(&dir, &name(&format!("n{i}"))).unwrap();
            for j in 0..=i {
                assert!(store.has_name(&dir, &name(&format!("n{j}"))).unwrap());
            }
        }
        assert_eq!(faults::commits(), 3);
        store.finish().unwrap();
        assert_eq!(faults::commits(), 4);
    }

    #[test]
    fn a_visitors_error_leaves_the_store_usable() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let mut store = space.store().unwrap();
        let dir = DirKey::Ordinal(3);
        for i in 0..5 {
            store.publish_name(&dir, &name(&format!("n{i}"))).unwrap();
        }
        let visitor_error =
            || FsError::new(Code::PermissionDenied, io::Error::other("visitor says no"));
        let e = store
            .for_each_name2(&dir, 2, &mut |n| {
                if n == name("n2") { Err(visitor_error()) } else { Ok(ControlFlow::Continue(())) }
            })
            .unwrap_err();
        assert_eq!(e.code, Code::PermissionDenied);
        assert_eq!(e.source.to_string(), "visitor says no", "the visitor's own error, unprefixed");
        assert!(space.poison_error().is_none(), "a visitor's error is not a store failure");
        assert!(store.has_name(&dir, &name("n4")).unwrap());
    }

    #[test]
    fn ids_rows_return_wipe_and_spelling_rows_with_their_seq() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let mut store = space.store().unwrap();
        let dir = DirKey::Strong(id(2, 2));
        let (one, two) = (id(5, 50), id(5, 51));
        store.ids_row(&dir, one, None, 1).unwrap();
        store.ids_row(&dir, one, Some(&name("x")), 2).unwrap();
        store.ids_row(&dir, one, Some(&name("y")), 3).unwrap();
        store.ids_row(&dir, two, Some(&name("z")), 4).unwrap();
        assert_eq!(
            store.ids_rows(&dir, one).unwrap(),
            vec![(None, 1), (Some(name("x")), 2), (Some(name("y")), 3)]
        );
        assert_eq!(store.ids_rows(&dir, two).unwrap(), vec![(Some(name("z")), 4)]);
        assert!(store.ids_row(&dir, one, Some(OsStr::new("")), 9).is_err());
    }

    #[test]
    fn for_each_name2_pages_through_every_row_in_order() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let mut store = space.store().unwrap();
        let dir = DirKey::Ordinal(1);
        let other = DirKey::Ordinal(2);
        let names: Vec<String> = (0..11).map(|i| format!("f{i:02}")).collect();
        for n in names.iter().rev() {
            store.publish_name(&dir, &name(n)).unwrap();
        }
        store.publish_name(&other, &name("zzz")).unwrap();
        let mut seen = Vec::new();
        store
            .for_each_name2(&dir, 3, &mut |n| {
                seen.push(n);
                Ok(ControlFlow::Continue(()))
            })
            .unwrap();
        assert_eq!(seen, names.iter().map(|n| name(n)).collect::<Vec<_>>());
        let mut count = 0;
        store
            .for_each_name2(&dir, 3, &mut |_| {
                count += 1;
                Ok(if count == 5 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) })
            })
            .unwrap();
        assert_eq!(count, 5);
    }

    #[test]
    fn retire_removes_in_batches_and_ticks_between_them() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let mut store = space.store().unwrap();
        let dir = DirKey::Ordinal(1);
        for i in 0..9 {
            store.publish_name(&dir, &name(&format!("n{i}"))).unwrap();
        }
        let mut ticks = 0;
        let mut tick = || {
            ticks += 1;
            Ok(())
        };
        store.retire(&dir, &mut tick).unwrap();
        assert!(ticks >= 4, "ticks {ticks}");
        let mut left = 0;
        store
            .for_each_name2(&dir, 4, &mut |_| {
                left += 1;
                Ok(ControlFlow::Continue(()))
            })
            .unwrap();
        assert_eq!(left, 0);
        for i in 0..9 {
            assert!(!store.has_name(&dir, &name(&format!("n{i}"))).unwrap());
        }
        assert!(faults::commits_at_first_retire() > 0);
    }

    #[test]
    fn a_write_error_at_a_commit_boundary_poisons_the_space() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let dir = DirKey::Ordinal(1);
        {
            let _ = space.store().unwrap();
        }
        faults::arm_write_failure(1);
        let mut store = space.store().unwrap();
        store.publish_name(&dir, &name("a")).unwrap();
        let err = store.publish_name(&dir, &name("b")).unwrap_err();
        assert_eq!(err.code, Code::DiskFull);
        let poison = space.poison_error().expect("poisoned");
        assert_eq!(poison.code, Code::DiskFull);
        assert_eq!(poison.source.to_string().matches("scratch space is unusable").count(), 1);
        let again = store.has_name(&dir, &name("a")).unwrap_err();
        assert_eq!(again.code, Code::DiskFull);
        assert!(again.source.to_string().contains("scratch space is unusable"));
        drop(store);
        assert!(space.store().is_err());
    }

    #[test]
    fn the_fault_counters_start_after_creation() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let mut store = space.store().unwrap();
        assert_eq!((faults::writes(), faults::commits()), (0, 0));
        let dir = DirKey::Ordinal(1);
        for i in 0..3 {
            store.publish_name(&dir, &name(&format!("n{i}"))).unwrap();
        }
        assert_eq!(faults::commits(), 1);
        assert!(faults::writes() > 0);
    }

    #[test]
    fn a_failed_final_commit_is_returned() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let mut store = space.store().unwrap();
        let dir = DirKey::Ordinal(1);
        for i in 0..3 {
            store.publish_name(&dir, &name(&format!("n{i}"))).unwrap();
        }
        faults::arm_commit_failure(1);
        assert!(store.finish().is_err());
        assert!(space.poison_error().is_some());
    }
}

/// The tier 2 scale harness (spec 6.3); see `index::scale_tests` for the knobs. `#[ignore]`, run by name.
#[cfg(test)]
mod scale_tests {
    use super::*;
    use crate::index::scale_tests::{ScaleDir, mix, name, percentiles, scale_n, status_kb};
    use crate::scratch::{ScratchSpace, Thresholds};
    use flux_fs::DestinationRoot;
    use flux_platform::StdFileSystem;
    use std::time::Instant;

    fn id_of(i: u64) -> ObjectId {
        ObjectId { volume: 1, index: u128::from(mix(i ^ 0x5CA1_E5CA_1E00_0000)) }
    }

    #[test]
    #[ignore = "scale harness: run by name with FLUX_SCALE_N, under --no-capture"]
    fn tier2_criteria() {
        let n = scale_n();
        let scale = ScaleDir::new("tier2");
        let thresholds = Thresholds { publication_commit: 1_024, ..Thresholds::default() };
        let space = ScratchSpace::unplaced(thresholds, None);
        space.place(StdFileSystem.destination_root(&scale.0).unwrap(), scale.0.clone());
        let dir = DirKey::Ordinal(1);
        println!(
            "tier2: n={n} dir={} publication_commit={}",
            scale.0.display(),
            thresholds.publication_commit
        );

        let mut store = space.store().unwrap();
        let started = Instant::now();
        for i in 0..n {
            // Random key order: the id is a hash of the row number.
            store.ids_row(&dir, id_of(i), Some(&os_name(&name(i))), i).unwrap();
            if i + 1 == n / 2 {
                println!("rss_anon_kb_at_half={} rows={}", status_kb("RssAnon:"), i + 1);
            }
        }
        let ingest = started.elapsed().as_secs_f64();
        println!("ingest_s={ingest:.2} inserts_per_s={:.0}", n as f64 / ingest);
        println!(
            "rss_anon_kb_at_end={} vm_rss_kb_at_end={} rows={n}",
            status_kb("RssAnon:"),
            status_kb("VmRSS:")
        );

        // Rows read back: 1,000 random inserted rows, each found with its spelling and sequence number.
        let mut s = 777u64;
        for _ in 0..1_000.min(n) {
            s = mix(s);
            let i = s % n;
            let rows = store.ids_rows(&dir, id_of(i)).unwrap();
            assert!(
                rows.contains(&(Some(os_name(&name(i))), i)),
                "inserted row {i} did not read back: {rows:?}"
            );
        }
        println!("readback_rows=1000 all_found=true");

        // Point lookups over the ingested table. `has_name` reads `names2`, which `ids_row` does not fill, so the
        // lookup that exercises the ingested `ids` table is `ids_rows` (one id's key range); `has_name` is timed too.
        let probes = 100_000usize.min(n as usize);
        let mut lat = Vec::with_capacity(probes);
        s = 4242;
        for _ in 0..probes {
            s = mix(s);
            let i = s % n;
            let t = Instant::now();
            let rows = store.ids_rows(&dir, id_of(i)).unwrap();
            lat.push(t.elapsed().as_nanos() as u64);
            assert!(!rows.is_empty());
        }
        let (median, p99) = percentiles(&mut lat);
        println!("ids_rows_point_lookup: probes={probes} median_us={median:.1} p99_us={p99:.1}");
        lat.clear();
        s = 4242;
        for _ in 0..probes {
            s = mix(s);
            let nm = os_name(&name(s % n));
            let t = Instant::now();
            let _ = store.has_name(&dir, &nm).unwrap();
            lat.push(t.elapsed().as_nanos() as u64);
        }
        let (median, p99) = percentiles(&mut lat);
        println!(
            "has_name_lookup (names2, not populated by ids_row): probes={probes} median_us={median:.1} p99_us={p99:.1}"
        );
        println!("rss_anon_kb_after_lookups={}", status_kb("RssAnon:"));
    }
}
