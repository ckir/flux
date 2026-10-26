// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The run's side of the hardlink index (cut 12, Task 4): the lazily created per-run index file, and the helpers that
//! open an anchor's directory and read an anchor's object. Nothing here links yet.

// The consumers (the engine's `copy_one`, Tasks 5-6) land after this plumbing.
#![allow(dead_code)]

use crate::run::recover::name_of;
use crate::state::HARDLINK_INDEX;
use flux_fs::{Code, DirHandle, FileIdentity, FluxPathKey, FsError, ObjectId};
use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::ops::Deref;

enum SlotState<I> {
    Unopened,
    Open(I),
    Failed(Code),
}

/// The run's hardlink index, created on first use inside the operation workspace (or a dry run's temporary
/// directory). A creation failure is remembered and answered on every call (plan ruling 9).
pub(crate) struct HardlinkSlot<D: DirHandle> {
    dir: Option<D>,
    state: RefCell<SlotState<D::Index>>,
}

impl<D: DirHandle> HardlinkSlot<D> {
    pub(crate) fn new(dir: D) -> Self {
        HardlinkSlot { dir: Some(dir), state: RefCell::new(SlotState::Unopened) }
    }

    /// A slot with no directory, already failed: the workspace or temp directory could not be opened (ruling 9).
    /// `with` answers `code`; `close` returns `None`.
    pub(crate) fn unavailable(code: Code) -> Self {
        HardlinkSlot { dir: None, state: RefCell::new(SlotState::Failed(code)) }
    }

    /// Lazily removes a stale `HARDLINK_INDEX` and creates a fresh one on the first call; a creation failure is kept
    /// and returned on every call (ruling 9).
    pub(crate) fn with<R>(
        &self,
        f: impl FnOnce(&mut D::Index) -> flux_fs::Result<R>,
    ) -> flux_fs::Result<R> {
        let mut state = self.state.borrow_mut();
        if matches!(*state, SlotState::Unopened) {
            *state = match self.create() {
                Ok(index) => SlotState::Open(index),
                Err(code) => SlotState::Failed(code),
            };
        }
        match &mut *state {
            SlotState::Open(index) => f(index),
            SlotState::Failed(code) => Err(unavailable_error(*code)),
            SlotState::Unopened => unreachable!("the state was just set"),
        }
    }

    /// A stale file from an earlier run of this operation is removed first (a `NotFound` is the usual answer), then a
    /// fresh index is created exclusively.
    fn create(&self) -> Result<D::Index, Code> {
        let dir = self.dir.as_ref().expect("an unopened slot has a directory");
        match dir.remove_file(OsStr::new(HARDLINK_INDEX)) {
            Ok(()) => {}
            Err(e) if e.source.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.code),
        }
        dir.create_hardlink_index(OsStr::new(HARDLINK_INDEX)).map_err(|e| e.code)
    }

    /// Drops the index; with `remove`, removes the file too (a `NotFound` is fine; ruling 15 passes `false` on a lost
    /// lock). Returns the directory (`None` for an `unavailable` slot).
    pub(crate) fn close(self, remove: bool) -> Option<D> {
        let HardlinkSlot { dir, state } = self;
        // The index is closed before its file is removed.
        drop(state);
        let dir = dir?;
        if remove {
            // Best effort: whatever stays is removed with the workspace (`finish_retire`). An index never opened has
            // no file of this run's; a stale one from an earlier run goes too.
            let _ = dir.remove_file(OsStr::new(HARDLINK_INDEX));
        }
        Some(dir)
    }

    /// The creation failure, if one was recorded.
    pub(crate) fn failure(&self) -> Option<Code> {
        match &*self.state.borrow() {
            SlotState::Failed(code) => Some(*code),
            _ => None,
        }
    }
}

fn unavailable_error(code: Code) -> FsError {
    FsError::new(code, std::io::Error::other("the hardlink index could not be created"))
}

/// The directory an anchor lives in: DEST itself for a one-component key, else a handle opened below it.
pub(crate) enum ParentDir<'a, D> {
    Dest(&'a D),
    Opened(D),
}

impl<D> Deref for ParentDir<'_, D> {
    type Target = D;
    fn deref(&self) -> &D {
        match self {
            ParentDir::Dest(d) => d,
            ParentDir::Opened(d) => d,
        }
    }
}

/// The anchor's directory and name, opened component by component from `dest` with `open_dir` (never a path), from
/// the claim-key bytes of `target`; `None` when a component does not decode (`recover::name_of`) or an open fails.
pub(crate) fn anchor_parent<'a, D: DirHandle>(
    dest: &'a D,
    target: &FluxPathKey,
) -> Option<(ParentDir<'a, D>, OsString)> {
    let mut components = target.0.split(|b| *b == 0).collect::<Vec<_>>();
    let name = name_of(components.pop()?)?;
    if name.is_empty() {
        return None;
    }
    let mut dir: Option<D> = None;
    for bytes in components {
        let component = name_of(bytes)?;
        let next = match &dir {
            Some(d) => d.open_dir(&component),
            None => dest.open_dir(&component),
        }
        .ok()?;
        dir = Some(next);
    }
    Some((dir.map_or(ParentDir::Dest(dest), ParentDir::Opened), name))
}

/// `Some(id)` for `FileIdentity::Strong(id)`, else `None`: the object an anchor entry records.
pub(crate) fn strong(identity: FileIdentity) -> Option<ObjectId> {
    match identity {
        FileIdentity::Strong(id) => Some(id),
        FileIdentity::Weak(_) | FileIdentity::Unavailable => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use flux_fs::{DestinationRoot, FileSystem as _};
    use std::path::Path;

    fn setup() -> (FaultFs, crate::fault_fs::FakeDirHandle) {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/w")).unwrap();
        let d = fs.destination_root(Path::new("/w")).unwrap();
        (fs, d)
    }

    fn key(s: &str) -> FluxPathKey {
        FluxPathKey(s.as_bytes().to_vec())
    }

    #[test]
    fn the_slot_creates_the_index_on_first_use_and_close_removes_it() {
        let (fs, d) = setup();
        let slot = HardlinkSlot::new(d);
        assert!(!fs.exists("/w/hardlinks.db"));
        slot.with(|_| Ok(())).unwrap();
        assert!(fs.exists("/w/hardlinks.db"));
        assert!(slot.close(true).is_some());
        assert!(!fs.exists("/w/hardlinks.db"));
    }

    #[test]
    fn close_without_remove_keeps_the_file() {
        let (fs, d) = setup();
        let slot = HardlinkSlot::new(d);
        slot.with(|_| Ok(())).unwrap();
        drop(slot.close(false));
        assert!(fs.exists("/w/hardlinks.db"));
        // Only the first use's stale removal; `close(false)` removes nothing.
        let removals = fs.calls().iter().filter(|c| *c == "remove_file(/w/hardlinks.db)").count();
        assert_eq!(removals, 1);
    }

    #[test]
    fn a_stale_index_is_removed_before_a_fresh_one_is_created() {
        let (fs, d) = setup();
        fs.write_file("/w/hardlinks.db", b"stale bytes");
        let slot = HardlinkSlot::new(d);
        slot.with(|_| Ok(())).unwrap();
        let calls = fs.calls();
        let rm = calls.iter().position(|c| c == "remove_file(/w/hardlinks.db)").unwrap();
        let mk = calls.iter().position(|c| c == "create_hardlink_index(/w/hardlinks.db)").unwrap();
        assert!(rm < mk, "{calls:?}");
        assert!(fs.hardlink_index_entries("/w/hardlinks.db").unwrap().is_empty());
    }

    #[test]
    fn a_creation_failure_is_answered_on_every_call() {
        let (fs, d) = setup();
        fs.fail_always("create_hardlink_index", Code::IoError);
        let slot = HardlinkSlot::new(d);
        for _ in 0..2 {
            let e = slot.with(|_| Ok(())).unwrap_err();
            assert_eq!(e.code, Code::IoError);
        }
        let n = fs.calls().iter().filter(|c| c.starts_with("create_hardlink_index")).count();
        assert_eq!(n, 1);
        assert_eq!(slot.failure(), Some(Code::IoError));
    }

    #[test]
    fn an_unavailable_slot_answers_its_code_and_closes_to_nothing() {
        let slot = HardlinkSlot::<crate::fault_fs::FakeDirHandle>::unavailable(Code::DiskFull);
        assert_eq!(slot.failure(), Some(Code::DiskFull));
        assert_eq!(slot.with(|_| Ok(())).unwrap_err().code, Code::DiskFull);
        assert!(slot.close(true).is_none());
    }

    #[test]
    fn anchor_parent_opens_components_from_dest_and_refuses_a_link_component() {
        let (fs, d) = setup();
        fs.create_dir(Path::new("/w/d")).unwrap();
        fs.create_dir(Path::new("/w/d/e")).unwrap();
        fs.write_file("/w/d/e/f", b"x");
        fs.write_file("/w/top", b"y");
        let (dir, name) = anchor_parent(&d, &key("d\0e\0f")).unwrap();
        assert_eq!(name, OsString::from("f"));
        assert!(matches!(dir, ParentDir::Opened(_)));
        assert!(dir.metadata(&name).is_ok());
        // One component: DEST itself.
        let (dir, name) = anchor_parent(&d, &key("top")).unwrap();
        assert!(matches!(dir, ParentDir::Dest(_)));
        assert!(dir.metadata(&name).is_ok());
        // A link component.
        let (fs2, d2) = setup();
        fs2.create_dir(Path::new("/w/d")).unwrap();
        fs2.add_symlink("/w/d/e");
        assert!(anchor_parent(&d2, &key("d\0e\0f")).is_none());
        // Empty key.
        assert!(anchor_parent(&d, &key("")).is_none());
    }

    #[test]
    fn strong_keeps_only_a_strong_identity() {
        let id = ObjectId { volume: 1, index: 2 };
        assert_eq!(strong(FileIdentity::Strong(id)), Some(id));
        assert_eq!(strong(FileIdentity::Weak(id)), None);
        assert_eq!(strong(FileIdentity::Unavailable), None);
    }
}
