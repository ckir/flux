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

/// A new index file in a fresh directory. The returned guard owns the directory: keep it alive while the index is in
/// use and drop the index first.
fn fresh_file() -> (tempfile::TempDir, std::fs::File, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hardlinks.db");
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
    (dir, file, path)
}

#[test]
fn redb_index_passes_the_conformance_suite() {
    // Each index the suite builds gets its own directory; the guards outlive the whole run.
    let dirs = std::cell::RefCell::new(Vec::new());
    conformance::run_all(|| {
        let (dir, file, _) = fresh_file();
        dirs.borrow_mut().push(dir);
        RedbHardlinkIndex::create_file(file).unwrap()
    });
    drop(dirs);
}

#[test]
fn entries_spill_to_the_file_under_a_small_cache() {
    const CACHE: usize = 64 * 1024;
    // 5_000 entries (about 3 s in a debug build, one uncontrolled run; 20_000 took 19 s) still overflow the 64 KiB
    // cache several times.
    const N: u128 = 5_000;
    let (_dir, file, path) = fresh_file();
    let mut index = RedbHardlinkIndex::with_cache_size(file, CACHE).unwrap();
    let target = FluxPathKey(vec![b't'; 40]);
    for i in 0..N {
        let id = ObjectId { volume: 7, index: i };
        index.insert(&id, &HardlinkEntry::Anchor { target: target.clone(), object: None }).unwrap();
    }
    assert_eq!(index.count().unwrap(), N as u64);
    for i in (0..N).step_by(37) {
        let id = ObjectId { volume: 7, index: i };
        assert_eq!(
            index.get(&id).unwrap(),
            Some(HardlinkEntry::Anchor { target: target.clone(), object: None }),
            "entry {i}"
        );
    }
    let len = std::fs::metadata(&path).unwrap().len();
    // `len > CACHE` shows the entries are on disk; it does not show the cache is bounded.
    assert!(len > CACHE as u64, "the entries are on disk, not in RAM: file is {len} bytes");
    // The index closes its file before the directory guard removes the directory.
    drop(index);
}
