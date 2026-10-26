// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The run's side of the hardlink index (cut 12, Task 4): the lazily created per-run index file, and the helpers that
//! open an anchor's directory and read an anchor's object. Nothing here links yet.

use crate::run::recover::name_of;
use crate::state::HARDLINK_INDEX;
use flux_fs::{Code, DirHandle, FileIdentity, FileType, FluxPathKey, FsError, Metadata, ObjectId};
use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::ops::Deref;

// Task 6 consumes this.
#[allow(dead_code)]
enum SlotState<I> {
    Unopened,
    Open(I),
    Failed(Code),
}

/// Where a slot's index file goes.
// Task 6 consumes this.
#[allow(dead_code)]
enum Home<D> {
    /// A directory in hand (the live run's workspace).
    Ready(D),
    /// A dry run's private directory `parent/name`, made at the first `with` and removed by `close(true)`.
    Pending { parent: D, name: OsString },
    /// `Pending`, but the parent could not be opened: the first `with` answers this code.
    PendingFailed(Code),
    /// `Pending` after the directory was made.
    Made { parent: D, name: OsString, dir: D },
    /// No directory: an `unavailable` slot.
    Absent,
}

/// The run's hardlink index, created on first use inside the operation workspace (or a dry run's temporary
/// directory). A creation failure is remembered and answered on every call (plan ruling 9).
pub(crate) struct HardlinkSlot<D: DirHandle> {
    home: RefCell<Home<D>>,
    state: RefCell<SlotState<D::Index>>,
}

impl<D: DirHandle> HardlinkSlot<D> {
    pub(crate) fn new(dir: D) -> Self {
        HardlinkSlot {
            home: RefCell::new(Home::Ready(dir)),
            state: RefCell::new(SlotState::Unopened),
        }
    }

    /// A dry run's slot: nothing is made until the first `with` (a tree without a hardlink candidate leaves nothing
    /// under the temporary root and warns of nothing). The first `with` creates `parent/name`; `parent` is `Err` when the
    /// temporary root could not be opened, and that code is then answered by `with`.
    pub(crate) fn deferred(parent: Result<D, Code>, name: OsString) -> Self {
        let home = match parent {
            Ok(parent) => Home::Pending { parent, name },
            Err(code) => Home::PendingFailed(code),
        };
        HardlinkSlot { home: RefCell::new(home), state: RefCell::new(SlotState::Unopened) }
    }

    /// A slot with no directory, already failed: the workspace or temp directory could not be opened (ruling 9).
    /// `with` answers `code`; `close` returns `None`.
    pub(crate) fn unavailable(code: Code) -> Self {
        HardlinkSlot {
            home: RefCell::new(Home::Absent),
            state: RefCell::new(SlotState::Failed(code)),
        }
    }

    // Task 6 consumes `with` and `create`.
    #[allow(dead_code)]
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

    #[allow(dead_code)]
    /// Makes a deferred slot's directory, then removes a stale file from an earlier run of this operation (a `NotFound`
    /// is the usual answer) and creates a fresh index exclusively.
    fn create(&self) -> Result<D::Index, Code> {
        let mut home = self.home.borrow_mut();
        if matches!(*home, Home::PendingFailed(_)) {
            let Home::PendingFailed(code) = *home else { unreachable!() };
            return Err(code);
        }
        if matches!(*home, Home::Pending { .. }) {
            let Home::Pending { parent, name } = std::mem::replace(&mut *home, Home::Absent) else {
                unreachable!()
            };
            match parent.create_dir(&name) {
                Ok(dir) => *home = Home::Made { parent, name, dir },
                Err(e) if e.source.kind() == ErrorKind::AlreadyExists => {
                    // A leftover of an earlier preview with this id: use it.
                    match parent.open_dir(&name) {
                        Ok(dir) => *home = Home::Made { parent, name, dir },
                        Err(e) => {
                            *home = Home::Pending { parent, name };
                            return Err(e.code);
                        }
                    }
                }
                Err(e) => {
                    *home = Home::Pending { parent, name };
                    return Err(e.code);
                }
            }
        }
        let dir = match &*home {
            Home::Ready(d) | Home::Made { dir: d, .. } => d,
            _ => unreachable!("an unopened slot has a directory or failed above"),
        };
        match dir.remove_file(OsStr::new(HARDLINK_INDEX)) {
            Ok(()) => {}
            Err(e) if e.source.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.code),
        }
        dir.create_hardlink_index(OsStr::new(HARDLINK_INDEX)).map_err(|e| e.code)
    }

    /// Drops the index; with `remove`, removes the file too, but only when the slot attempted a creation (an unopened
    /// slot touches nothing; a `NotFound` is fine; ruling 15 passes `false` on a lost lock). A deferred slot's private
    /// directory is removed with it. Returns the directory: `None` for an `unavailable` slot, a deferred one that
    /// was never made, and a deferred one that `remove` took away.
    pub(crate) fn close(self, remove: bool) -> Option<D> {
        let HardlinkSlot { home, state } = self;
        let attempted = !matches!(*state.borrow(), SlotState::Unopened);
        // The index is closed before its file is removed.
        drop(state);
        match home.into_inner() {
            Home::Ready(dir) => {
                if remove && attempted {
                    // Best effort: whatever stays goes with the workspace (`finish_retire`).
                    let _ = dir.remove_file(OsStr::new(HARDLINK_INDEX));
                }
                Some(dir)
            }
            Home::Made { parent, name, dir } => {
                if remove {
                    let _ = dir.remove_file(OsStr::new(HARDLINK_INDEX));
                    // The handle goes first, so a platform that cannot remove an open directory can.
                    drop(dir);
                    let _ = parent.remove_dir(&name);
                    None
                } else {
                    Some(dir)
                }
            }
            Home::Pending { .. } | Home::PendingFailed(_) | Home::Absent => None,
        }
    }

    /// The creation failure, if one was recorded.
    pub(crate) fn failure(&self) -> Option<Code> {
        match &*self.state.borrow() {
            SlotState::Failed(code) => Some(*code),
            _ => None,
        }
    }
}

// Task 6 consumes this.
#[allow(dead_code)]
fn unavailable_error(code: Code) -> FsError {
    FsError::new(code, std::io::Error::other("the hardlink index could not be created"))
}

/// The directory an anchor lives in: DEST itself for a one-component key, else a handle opened below it.
// Task 6 consumes this.
#[allow(dead_code)]
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
// Task 6 consumes this.
#[allow(dead_code)]
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

/// What a regular file's metadata says about hardlink grouping (cut 12, Task 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Candidacy {
    /// A single name, or not a regular file: an ordinary copy.
    NotCandidate,
    /// The link count is unknown (`nlink == None`) on a `Strong` identity: it cannot be checked.
    Unchecked,
    /// `nlink > 1` but the identity is `Weak` or `Unavailable`: equivalence must never be inferred.
    Weak(FileIdentity),
    /// `nlink > 1` and a `Strong` identity: the object a group is keyed by.
    Candidate(ObjectId),
}

/// `nlink > 1` and `Strong` is a candidate; `nlink == None` is `Unchecked`; `nlink > 1` with a `Weak`/`Unavailable`
/// identity is `Weak(identity)`; a regular file with `nlink <= 1` is not a candidate. Non-regular files never are.
pub(crate) fn candidacy(meta: &Metadata) -> Candidacy {
    if meta.file_type != FileType::File {
        return Candidacy::NotCandidate;
    }
    match (meta.nlink, meta.identity) {
        (Some(n), _) if n <= 1 => Candidacy::NotCandidate,
        (Some(_), FileIdentity::Strong(id)) => Candidacy::Candidate(id),
        (Some(_), weaker) => Candidacy::Weak(weaker),
        // The table defines the unknown count for a `Strong` identity only; a weak one could never link anyway.
        (None, FileIdentity::Strong(_)) => Candidacy::Unchecked,
        (None, _) => Candidacy::NotCandidate,
    }
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

    // Mutant: `close` removes the file for an unopened slot as well.
    #[test]
    fn closing_an_unopened_slot_touches_nothing() {
        let (fs, d) = setup();
        fs.write_file("/w/hardlinks.db", b"stale");
        drop(HardlinkSlot::new(d).close(true));
        assert!(fs.exists("/w/hardlinks.db"));
        assert!(!fs.calls().iter().any(|c| c.starts_with("remove_file")), "{:?}", fs.calls());
    }

    // Ruling 15: a lost lock closes with `remove: false`. The run-level version (a real group's index surviving a lost
    // lock) lands with Task 6, when the engine opens the index.
    #[test]
    fn close_without_remove_keeps_an_open_slots_file() {
        let (fs, d) = setup();
        let slot = HardlinkSlot::new(d);
        slot.with(|_| Ok(())).unwrap();
        assert!(slot.close(false).is_some());
        assert!(fs.exists("/w/hardlinks.db"));
    }

    #[test]
    fn a_deferred_slot_makes_nothing_until_used_and_close_removes_its_directory() {
        let (fs, d) = setup();
        let parent = fs.destination_root(Path::new("/w")).unwrap();
        let slot = HardlinkSlot::deferred(Ok(parent), OsString::from("flux-dry-x-hardlinks"));
        assert!(!fs.exists("/w/flux-dry-x-hardlinks"));
        slot.with(|_| Ok(())).unwrap();
        assert!(fs.exists("/w/flux-dry-x-hardlinks/hardlinks.db"));
        assert!(slot.close(true).is_none());
        assert!(!fs.exists("/w/flux-dry-x-hardlinks"));
        drop(d);
        // Never used: nothing made, nothing removed, no failure.
        let slot = HardlinkSlot::deferred(
            Err::<crate::fault_fs::FakeDirHandle, _>(Code::IoError),
            OsString::from("n"),
        );
        assert_eq!(slot.failure(), None);
        assert_eq!(slot.with(|_| Ok(())).unwrap_err().code, Code::IoError);
        assert_eq!(slot.failure(), Some(Code::IoError));
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

    fn meta_of(fs: &FaultFs, path: &str) -> Metadata {
        fs.metadata(Path::new(path)).unwrap()
    }

    // Mutant: `nlink == Some(1)` returns `Candidate`.
    #[test]
    fn candidacy_of_a_single_name_is_not_a_candidate() {
        let (fs, _d) = setup();
        fs.write_file("/w/f", b"x");
        assert_eq!(candidacy(&meta_of(&fs, "/w/f")), Candidacy::NotCandidate);
        fs.set_link_count("/w/f", 0);
        assert_eq!(candidacy(&meta_of(&fs, "/w/f")), Candidacy::NotCandidate);
        // A single name is never touched by identity strength.
        fs.set_link_count("/w/f", 1);
        fs.set_identity("/w/f", FileIdentity::Unavailable);
        assert_eq!(candidacy(&meta_of(&fs, "/w/f")), Candidacy::NotCandidate);
    }

    #[test]
    fn candidacy_classifies_the_multiply_linked_and_the_unknown() {
        let (fs, _d) = setup();
        fs.write_file("/w/f", b"x");
        let id = ObjectId { volume: 3, index: 9 };
        fs.set_link_count("/w/f", 2);
        fs.set_identity("/w/f", FileIdentity::Strong(id));
        assert_eq!(candidacy(&meta_of(&fs, "/w/f")), Candidacy::Candidate(id));
        fs.set_identity("/w/f", FileIdentity::Weak(id));
        assert_eq!(candidacy(&meta_of(&fs, "/w/f")), Candidacy::Weak(FileIdentity::Weak(id)));
        fs.set_identity("/w/f", FileIdentity::Unavailable);
        assert_eq!(candidacy(&meta_of(&fs, "/w/f")), Candidacy::Weak(FileIdentity::Unavailable));
        fs.set_identity("/w/f", FileIdentity::Strong(id));
        fs.set_link_count_unknown("/w/f");
        assert_eq!(candidacy(&meta_of(&fs, "/w/f")), Candidacy::Unchecked);
        // Unknown count on a non-Strong identity: the table defines `Unchecked` for Strong identities only, and a
        // weak identity could never link anyway, so it is not a candidate and not counted.
        fs.set_identity("/w/f", FileIdentity::Weak(id));
        assert_eq!(candidacy(&meta_of(&fs, "/w/f")), Candidacy::NotCandidate);
    }

    #[test]
    fn candidacy_ignores_a_directory() {
        let (fs, _d) = setup();
        fs.create_dir(Path::new("/w/d")).unwrap();
        fs.set_link_count("/w/d", 5);
        assert_eq!(candidacy(&meta_of(&fs, "/w/d")), Candidacy::NotCandidate);
    }
}
