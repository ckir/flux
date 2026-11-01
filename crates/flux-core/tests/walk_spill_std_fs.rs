// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Spec 6.5: a flat directory of 300,000 entries walks in order with the production constants, against a real
//! filesystem. Its own binary so the 300,000 files are made once.

use flux_core::scratch::{ScratchSpace, Thresholds};
use flux_core::walk::{MountPolicy, WalkEvent, WalkOptions, walk_with};
use flux_fs::DestinationRoot;
use flux_platform::StdFileSystem;
use std::path::Path;
use tempfile::TempDir;

#[test]
fn a_flat_directory_of_300k_entries_walks_in_order_with_the_production_constants() {
    let tree = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let mut expected = Vec::with_capacity(300_000);
    for i in 0..300_000u32 {
        let name = format!("f{:06}", (u64::from(i) * 7919) % 300_000);
        std::fs::File::create(tree.path().join(&name)).unwrap();
        expected.push(name);
    }
    expected.sort();
    expected.dedup();
    assert_eq!(expected.len(), 300_000);

    let fs = StdFileSystem;
    let space = ScratchSpace::unplaced(Thresholds::default(), Some(home.path().to_path_buf()));
    space.place(fs.destination_root(home.path()).unwrap(), home.path().to_path_buf());
    let mut walk = walk_with(
        &fs,
        tree.path(),
        WalkOptions { max_depth: 8, mounts: MountPolicy::Cross },
        Some(&space),
    )
    .unwrap();
    let mut names = Vec::with_capacity(300_000);
    for event in walk.by_ref() {
        match event.unwrap() {
            WalkEvent::File { path } => {
                names.push(path.file_name().unwrap().to_str().unwrap().to_owned())
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(names, expected);
    assert_eq!(walk.spilled_directories(), 1);
    space.close();
    // `ScratchSpace::len` and `physical_len` are `pub(crate)`: from here the zero length is shown by the scratch
    // home holding no entry (the file is nameless) after the walk.
    assert_eq!(Path::new(home.path()).read_dir().unwrap().count(), 0);
}
