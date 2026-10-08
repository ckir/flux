//! The real `RedbClaimStore` against the backend-independent conformance suite.

use std::ffi::OsStr;
use std::fs::OpenOptions;

use flux_fs::claims::conformance;
use flux_fs::{
    ClaimKey, ClaimRecord, ClaimStatus, ClaimStore, Code, Durability, FluxPathKey, ObjectId,
};
use flux_platform::RedbClaimStore;

// The tempdirs must outlive the stores built from them; leaking them is fine in a test.
fn fresh(durability: Durability) -> RedbClaimStore {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
    let store = RedbClaimStore::create_file(file, durability).unwrap();
    std::mem::forget(dir);
    store
}

#[test]
fn real_store_passes_the_conformance_suite_normal() {
    conformance::run_all(|| fresh(Durability::Normal));
}

#[test]
fn real_store_passes_the_conformance_suite_strict() {
    conformance::run_all(|| fresh(Durability::Strict));
}

#[test]
fn strict_durability_never_accumulates_unsynced_commits() {
    let mut s = fresh(Durability::Strict);
    let parent = ObjectId { volume: 1, index: 1 };
    for i in 0u32..5 {
        let key = ClaimKey::new(parent, OsStr::new(&format!("k{i}")));
        let rec = ClaimRecord { target: FluxPathKey(b"t".to_vec()), status: ClaimStatus::Existing };
        s.insert_if_absent(&key, &rec).unwrap();
        assert_eq!(s.unsynced(), 0);
    }
}

#[test]
fn the_cap_syncs_every_1000_unsynced_commits() {
    let mut s = fresh(Durability::Normal);
    let parent = ObjectId { volume: 1, index: 1 };
    for i in 0u32..2500 {
        let key = ClaimKey::new(parent, OsStr::new(&format!("k{i}")));
        let rec = ClaimRecord { target: FluxPathKey(b"t".to_vec()), status: ClaimStatus::Existing };
        s.insert_if_absent(&key, &rec).unwrap();
    }
    assert_eq!(s.unsynced(), 500);
    s.flush().unwrap();
    assert_eq!(s.unsynced(), 0);
}

mod open {
    use super::*;
    use redb::{Builder, TableDefinition};

    fn rec() -> ClaimRecord {
        ClaimRecord { target: FluxPathKey(b"t".to_vec()), status: ClaimStatus::Existing }
    }

    fn key(i: u32) -> ClaimKey {
        ClaimKey::new(ObjectId { volume: 1, index: 1 }, OsStr::new(&format!("k{i}")))
    }

    fn open_rw(path: &std::path::Path) -> std::fs::File {
        OpenOptions::new().read(true).write(true).open(path).unwrap()
    }

    fn open_err(path: &std::path::Path) -> flux_fs::FsError {
        match RedbClaimStore::open_file(open_rw(path), Durability::Strict) {
            Err(e) => e,
            Ok(_) => panic!("open_file must refuse this file"),
        }
    }

    #[test]
    fn a_store_reopens_with_its_claims_and_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
        let mut store = RedbClaimStore::create_file(file, Durability::Normal).unwrap();
        for i in 0..3 {
            store.insert_if_absent(&key(i), &rec()).unwrap();
        }
        store.flush().unwrap();
        drop(store);

        let mut store = RedbClaimStore::open_file(open_rw(&path), Durability::Normal).unwrap();
        for i in 0..3 {
            assert_eq!(store.get(&key(i)).unwrap(), Some(rec()));
        }
        assert_eq!(store.count().unwrap(), 3);
        store.insert_if_absent(&key(3), &rec()).unwrap();
        assert_eq!(store.count().unwrap(), 4);
    }

    #[test]
    fn a_zero_length_file_is_state_corrupt_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(open_err(&path).code, Code::StateCorrupt);
    }

    #[test]
    fn a_file_that_is_not_a_store_is_state_corrupt_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        std::fs::write(&path, b"not a redb file at all, long enough, and then some more text to pass sixty-four bytes").unwrap();
        assert_eq!(open_err(&path).code, Code::StateCorrupt);
    }

    const CLAIMS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("claims");
    const META: TableDefinition<&str, u64> = TableDefinition::new("meta");

    #[test]
    fn a_store_whose_format_is_not_1_is_incompatible_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
        let db = Builder::new().create_file(file).unwrap();
        let tx = db.begin_write().unwrap();
        {
            tx.open_table(CLAIMS).unwrap();
            let mut meta = tx.open_table(META).unwrap();
            meta.insert("format", 2u64).unwrap();
        }
        tx.commit().unwrap();
        drop(db);
        assert_eq!(open_err(&path).code, Code::IncompatibleState);
    }

    #[test]
    fn a_store_missing_its_meta_table_is_state_corrupt_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
        let db = Builder::new().create_file(file).unwrap();
        let tx = db.begin_write().unwrap();
        {
            tx.open_table(CLAIMS).unwrap();
        }
        tx.commit().unwrap();
        drop(db);
        assert_eq!(open_err(&path).code, Code::StateCorrupt);
    }
}
