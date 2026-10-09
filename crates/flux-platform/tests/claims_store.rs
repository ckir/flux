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
fn real_store_passes_the_prepared_conformance_suite_normal() {
    conformance::run_prepared_all(|| fresh(Durability::Normal));
}

#[test]
fn real_store_passes_the_prepared_conformance_suite_strict() {
    conformance::run_prepared_all(|| fresh(Durability::Strict));
}

#[test]
fn a_fresh_store_is_format_2() {
    let s = fresh(Durability::Strict);
    assert_eq!(s.format(), 2);
    assert!(s.supports_prepared());
}

#[test]
fn note_commits_are_immediate_under_normal() {
    let mut s = fresh(Durability::Normal);
    let parent = ObjectId { volume: 1, index: 1 };
    let note = flux_fs::PreparedRecord {
        target: FluxPathKey(b"t".to_vec()),
        temp_name: b"n.flux-partial.x".to_vec(),
        identity: "strong:1:9".to_string(),
        dir_path: String::new(),
        name: b"n".to_vec(),
        planned_name: Vec::new(),
        replacement: false,
    };
    for i in 0u32..5 {
        s.prepare(&ClaimKey::new(parent, OsStr::new(&format!("n{i}"))), &note).unwrap();
        assert_eq!(s.unsynced(), 0);
    }
    assert_eq!(s.unsynced(), 0);
    let rec = ClaimRecord { target: FluxPathKey(b"t".to_vec()), status: ClaimStatus::Existing };
    s.insert_if_absent(&ClaimKey::new(parent, OsStr::new("c")), &rec).unwrap();
    assert_eq!(s.unsynced(), 1);
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

    const PREPARED: TableDefinition<&[u8], &[u8]> = TableDefinition::new("prepared");

    fn build(path: &std::path::Path, format: u64, with_prepared: bool) {
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(path).unwrap();
        let db = Builder::new().create_file(file).unwrap();
        let tx = db.begin_write().unwrap();
        {
            tx.open_table(CLAIMS).unwrap();
            if with_prepared {
                tx.open_table(PREPARED).unwrap();
            }
            let mut meta = tx.open_table(META).unwrap();
            meta.insert("format", format).unwrap();
        }
        tx.commit().unwrap();
    }

    #[test]
    fn a_format_3_store_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        build(&path, 3, true);
        assert_eq!(open_err(&path).code, Code::IncompatibleState);
    }

    #[test]
    fn a_format_2_store_without_a_prepared_table_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        build(&path, 2, false);
        assert_eq!(open_err(&path).code, Code::StateCorrupt);
    }

    #[test]
    fn a_format_1_store_opens_with_supports_prepared_false_and_refuses_notes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        build(&path, 1, false);
        let mut s = RedbClaimStore::open_file(open_rw(&path), Durability::Strict).unwrap();
        assert_eq!(s.format(), 1);
        assert!(!s.supports_prepared());
        let note = flux_fs::PreparedRecord {
            target: FluxPathKey(b"t".to_vec()),
            temp_name: b"x".to_vec(),
            identity: "strong:1:9".to_string(),
            dir_path: String::new(),
            name: b"n".to_vec(),
            planned_name: Vec::new(),
            replacement: false,
        };
        assert_eq!(s.prepare(&key(0), &note).unwrap_err().code, Code::IncompatibleState);
        assert_eq!(s.discard_prepared(&key(0)).unwrap_err().code, Code::IncompatibleState);
        assert_eq!(
            s.commit_prepared(&key(0), &rec().target, None).unwrap_err().code,
            Code::IncompatibleState
        );
        assert_eq!(s.apply_recovery(&[]).unwrap_err().code, Code::IncompatibleState);
        assert!(s.prepared().unwrap().is_empty());
        assert_eq!(s.insert_if_absent(&key(1), &rec()).unwrap(), flux_fs::ClaimOutcome::Inserted);
        drop(s);
        // Never upgraded in place: meta.format is still 1 and no prepared table exists.
        let db = redb::Database::open(&path).unwrap();
        let tx = redb::ReadableDatabase::begin_read(&db).unwrap();
        let meta = redb::ReadableTable::get(&tx.open_table(META).unwrap(), "format")
            .unwrap()
            .unwrap()
            .value();
        assert_eq!(meta, 1);
        assert!(tx.open_table(PREPARED).is_err(), "no prepared table may appear");
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

    #[test]
    fn a_store_missing_its_claims_table_is_state_corrupt_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
        let db = Builder::new().create_file(file).unwrap();
        let tx = db.begin_write().unwrap();
        {
            let mut meta = tx.open_table(META).unwrap();
            meta.insert("format", 1u64).unwrap();
        }
        tx.commit().unwrap();
        drop(db);
        assert_eq!(open_err(&path).code, Code::StateCorrupt);
    }
}

mod undecodable_claims {
    use super::*;
    use flux_fs::{PreparedRecord, RecoveryOp};
    use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

    const CLAIMS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("claims");

    fn target() -> FluxPathKey {
        FluxPathKey(b"t".to_vec())
    }

    fn key(name: &str) -> ClaimKey {
        ClaimKey::new(ObjectId { volume: 1, index: 1 }, OsStr::new(name))
    }

    /// A store at a fresh path holding a note for `key("a")`, closed again.
    fn with_note() -> std::path::PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        std::mem::forget(dir);
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
        let mut s = RedbClaimStore::create_file(file, Durability::Strict).unwrap();
        let note = PreparedRecord {
            target: target(),
            temp_name: b"a.flux-partial.x".to_vec(),
            identity: "strong:1:9".to_string(),
            dir_path: String::new(),
            name: b"a".to_vec(),
            planned_name: Vec::new(),
            replacement: false,
        };
        s.prepare(&key("a"), &note).unwrap();
        path
    }

    fn garbage_at(path: &std::path::Path, k: &ClaimKey) {
        let db = Database::open(path).unwrap();
        let tx = db.begin_write().unwrap();
        {
            tx.open_table(CLAIMS)
                .unwrap()
                .insert(k.encode().as_slice(), [0xffu8; 3].as_slice())
                .unwrap();
        }
        tx.commit().unwrap();
    }

    fn claims_rows(path: &std::path::Path) -> Vec<(Vec<u8>, Vec<u8>)> {
        let db = Database::open(path).unwrap();
        let tx = db.begin_read().unwrap();
        let t = tx.open_table(CLAIMS).unwrap();
        t.iter()
            .unwrap()
            .map(|r| {
                let (k, v) = r.unwrap();
                (k.value().to_vec(), v.value().to_vec())
            })
            .collect()
    }

    fn open(path: &std::path::Path) -> RedbClaimStore {
        let f = OpenOptions::new().read(true).write(true).open(path).unwrap();
        RedbClaimStore::open_file(f, Durability::Strict).unwrap()
    }

    /// Both entry points refuse with StateCorrupt and change nothing.
    fn refuses(path: &std::path::Path, planned: Option<&ClaimKey>) {
        let before = claims_rows(path);
        let mut s = open(path);
        let e = s.commit_prepared(&key("a"), &target(), planned).unwrap_err();
        assert_eq!(e.code, Code::StateCorrupt, "{e:?}");
        let op = RecoveryOp::Commit { key: key("a"), target: target(), planned: planned.cloned() };
        let e = s.apply_recovery(&[op]).unwrap_err();
        assert_eq!(e.code, Code::StateCorrupt, "{e:?}");
        assert_eq!(s.prepared().unwrap().len(), 1, "the note is still listed");
        drop(s);
        assert_eq!(claims_rows(path), before, "claims unchanged");
    }

    #[test]
    fn an_undecodable_claim_at_the_key_is_state_corrupt() {
        // Mutant (claims.rs `apply_in`): `None => true` (foreign, IoError) for an undecodable claim.
        let path = with_note();
        garbage_at(&path, &key("a"));
        refuses(&path, None);
    }

    #[test]
    fn an_undecodable_claim_at_the_planned_key_is_state_corrupt() {
        let path = with_note();
        garbage_at(&path, &key("A"));
        refuses(&path, Some(&key("A")));
    }

    #[test]
    fn a_decodable_foreign_claim_stays_an_io_error() {
        let path = with_note();
        let mut s = open(&path);
        let other =
            ClaimRecord { target: FluxPathKey(b"other".to_vec()), status: ClaimStatus::Existing };
        s.insert_if_absent(&key("a"), &other).unwrap();
        let e = s.commit_prepared(&key("a"), &target(), None).unwrap_err();
        assert_eq!(e.code, Code::IoError, "{e:?}");
    }
}
