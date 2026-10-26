// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The real `RedbHardlinkIndex` against the backend-independent conformance suite, and its spill behaviour.

use std::fs::OpenOptions;

use flux_fs::hardlinks::conformance;
use flux_fs::{FluxPathKey, HardlinkEntry, HardlinkIndex, ObjectId};
use flux_platform::RedbHardlinkIndex;

fn fresh_file() -> (std::fs::File, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hardlinks.db");
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
    // The tempdir must outlive the index built from it; leaking it is fine in a test.
    std::mem::forget(dir);
    (file, path)
}

#[test]
fn redb_index_passes_the_conformance_suite() {
    conformance::run_all(|| RedbHardlinkIndex::create_file(fresh_file().0).unwrap());
}

#[test]
fn entries_spill_to_the_file_under_a_small_cache() {
    const CACHE: usize = 64 * 1024;
    const N: u128 = 20_000;
    let (file, path) = fresh_file();
    let mut index = RedbHardlinkIndex::with_cache_size(file, CACHE).unwrap();
    let target = FluxPathKey(vec![b't'; 40]);
    for i in 0..N {
        let id = ObjectId { volume: 7, index: i };
        index.insert(&id, &HardlinkEntry::Anchor { target: target.clone(), object: None }).unwrap();
    }
    assert_eq!(index.count().unwrap(), N as u64);
    for i in (0..N).step_by(97) {
        let id = ObjectId { volume: 7, index: i };
        assert_eq!(
            index.get(&id).unwrap(),
            Some(HardlinkEntry::Anchor { target: target.clone(), object: None }),
            "entry {i}"
        );
    }
    let len = std::fs::metadata(&path).unwrap().len();
    assert!(len > CACHE as u64, "the entries are on disk, not in RAM: file is {len} bytes");
}
