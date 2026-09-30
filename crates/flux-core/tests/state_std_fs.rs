//! The operation state on a real filesystem (cut 7a Part 3a): the workspace built and renamed into place, and its
//! manifest rewritten crash-safely.

use flux_core::state::{self, Kind, OpState, OperationState};
use flux_fs::{DestinationRoot, DirHandle};
use flux_platform::StdFileSystem;
use std::ffi::{OsStr, OsString};

fn names<D: DirHandle>(d: &D) -> Vec<OsString> {
    let mut v: Vec<_> = d.read_dir().unwrap().into_iter().map(|e| e.name).collect();
    v.sort();
    v
}

#[test]
fn a_workspace_is_created_and_its_manifest_rewritten_leaving_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = StdFileSystem.destination_root(tmp.path()).unwrap();
    let id = flux_core::ids::new_id();
    let s = OperationState::created(&id, Kind::Tree, tmp.path(), state::wall_time_ns());
    let ops = state::operations_dir(&dest, tmp.path()).unwrap();
    let ws = state::create_workspace(&ops, &s).unwrap();
    assert_eq!(
        names(&ops),
        vec![OsString::from(id.as_str())],
        "the workspace, never its creating name"
    );
    let moving = OperationState { state: OpState::Transferring, ..s };
    state::write_state(&ws, OsStr::new(state::MANIFEST), &moving).unwrap();
    assert_eq!(state::read_state(&ws, OsStr::new(state::MANIFEST)).unwrap(), Ok(moving));
    assert_eq!(names(&ws), vec![OsString::from(state::MANIFEST)], "no temporary is left");
}

#[test]
fn the_scan_finds_a_resumable_workspace_and_passes_over_a_creating_one() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = StdFileSystem.destination_root(tmp.path()).unwrap();
    let ops = state::operations_dir(&dest, tmp.path()).unwrap();
    let prior = OperationState::created(&flux_core::ids::new_id(), Kind::Tree, tmp.path(), 1);
    drop(state::create_workspace(&ops, &prior).unwrap());
    let mut creating = flux_core::ids::new_id();
    creating.push_str(state::CREATING_SUFFIX);
    drop(ops.create_dir(OsStr::new(&creating)).unwrap());
    let scan = flux_core::prior::scan_tree(&dest, tmp.path(), &flux_core::ids::new_id()).unwrap();
    assert_eq!(scan.resumable.len(), 1);
    assert_eq!(scan.resumable[0].state, prior);
}
