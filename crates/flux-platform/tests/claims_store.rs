//! The real `RedbClaimStore` against the backend-independent conformance suite.

use std::ffi::OsStr;
use std::fs::OpenOptions;

use flux_fs::claims::conformance;
use flux_fs::{ClaimKey, ClaimRecord, ClaimStatus, ClaimStore, Durability, FluxPathKey, ObjectId};
use flux_platform::RedbClaimStore;

// The tempdirs must outlive the stores built from them; leaking them is fine in a test.
fn fresh(durability: Durability) -> RedbClaimStore {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
    let store = RedbClaimStore::from_file(file, durability).unwrap();
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
