//! The place of a several-source run (spec 3.4, 3.5): a `TreePlace` for DEST plus one prefix lock and one root per
//! source. Its `create` makes DEST, takes the prefix locks inside it, checks the names the sources take, and only then
//! makes the workspace, so a refusal at any of those finds no workspace and releases every lock it took.

use super::place::{AdoptedStore, Place, TreePlace, refused_state};
use super::prefix::{PrefixId, PrefixIdOwned, PrefixLocks, PrefixRefusal};
use super::session::{Fault, Locked, failed, from_lock, lock_io};
use super::{RunError, RunStep, RunWarning};
use crate::lock::{LockCode, LockError, LockResult, Refusal};
use crate::prior::{PriorOp, Scan};
use crate::state::{
    Kind, OperationState, Root, RootKind, absolute_lexical, identity_text, native_hex,
};
use flux_fs::{
    DestinationRoot, DirHandle, Durability, FileIdentity, FileType, FsError, LockCapability,
};
use std::cell::{OnceCell, RefCell};
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// One root of the run: the name it takes under DEST, what it is, where it comes from, and the identity it had when
/// the run opened it.
#[allow(dead_code)] // used by run::session (plan task 7)
pub(crate) struct RootPlan {
    pub name: OsString,
    pub kind: RootKind,
    pub src: PathBuf,
    pub identity: FileIdentity,
}

/// A tree's `TreePlace`, with the prefix locks of its roots.
#[allow(dead_code)] // used by run::session (plan task 7)
pub(crate) struct MultiPlace<'p, F: DestinationRoot> {
    pub(crate) tree: TreePlace<'p, F>,
    /// In prefix order.
    pub(crate) roots: Vec<RootPlan>,
    /// Owned by the caller, outlives the place: the handle the prefix locks live in, where DEST has a name.
    pub(crate) lock_dir: &'p OnceCell<F::Dir>,
    pub(crate) capability: LockCapability,
    pub(crate) prefix_id: PrefixIdOwned,
    /// A `RefCell` so that `Place::remove`, which takes `&self`, can release the locks (the trait does not change).
    pub(crate) prefix: RefCell<Option<PrefixLocks<'p, F::Dir>>>,
}

/// Release the locks held in `cell`, if any; the failures only. A second call finds nothing.
fn release_cell<D: DirHandle>(
    cell: &RefCell<Option<PrefixLocks<'_, D>>>,
) -> Vec<(OsString, LockError)> {
    cell.borrow_mut().take().map(PrefixLocks::release_all).unwrap_or_default()
}

fn refuse(code: LockCode, detail: String) -> RunError {
    RunError::Refused {
        refusal: Box::new(Refusal { code, holder: None, detail }),
        changed: false,
        not_removed: None,
    }
}

/// `err`, with `not_removed` set where a rollback removed less than it should have.
fn with_not_removed(mut err: RunError, left: (PathBuf, FsError)) -> RunError {
    match &mut err {
        RunError::Refused { changed, not_removed, .. } => {
            *changed = true;
            not_removed.get_or_insert(left);
        }
        RunError::Failed { not_removed, .. } => {
            not_removed.get_or_insert(left);
        }
    }
    err
}

#[allow(dead_code)] // used by run::session (plan task 7)
impl<'p, F: DestinationRoot> MultiPlace<'p, F> {
    /// Release the prefix locks, each with `Held::release`; the failures only. A second call is a no-op.
    pub(crate) fn release_prefix_locks(&mut self) -> Vec<(OsString, LockError)> {
        release_cell(&self.prefix)
    }

    fn lock_path(&self, name: &OsStr) -> PathBuf {
        let mut file = name.to_os_string();
        file.push(".flux-lock");
        self.tree.dest_shown.join(file)
    }

    /// A release failure cannot replace the error that caused the release; it is a warning.
    fn warn_release(&self, failed: Vec<(OsString, LockError)>, warnings: &mut Vec<RunWarning>) {
        for (name, e) in failed {
            warnings
                .push(RunWarning::NotRemoved { path: self.lock_path(&name), error: lock_io(e) });
        }
    }

    /// Roll back what this run made in DEST once the locks are released: the control directories and a DEST it made.
    fn roll_back(&mut self, err: RunError) -> RunError {
        match self.tree.remove_control_dirs(true) {
            Ok(()) => err,
            Err(left) => with_not_removed(err, left),
        }
    }

    /// The directory the prefix locks live in: DEST. A filesystem root is its own holder; a named DEST is opened once
    /// through its parent into the caller's cell.
    fn lock_directory(&self) -> Result<&'p F::Dir, RunError> {
        let holder: &'p F::Dir = self.tree.holder;
        let Some(name) = self.tree.name.as_deref() else { return Ok(holder) };
        if self.lock_dir.get().is_none() {
            let dir = holder
                .open_dir(name)
                .map_err(|e| failed(RunStep::State, &self.tree.dest_shown, e))?;
            // Only this place sets it, and only here, after the `get` above found it empty.
            let _ = self.lock_dir.set(dir);
        }
        Ok(self.lock_dir.get().expect("set above"))
    }

    fn prefix_error(&self, refusal: PrefixRefusal) -> RunError {
        match refusal {
            PrefixRefusal::Collision { name, other } => refuse(
                LockCode::DestinationNamespaceCollision,
                format!(
                    "{}: the names {:?} and {:?} are one name on this filesystem, so they cannot be two sources' prefixes",
                    self.tree.dest_shown.display(),
                    other,
                    name
                ),
            ),
            PrefixRefusal::Lock(e) => from_lock(e, RunStep::Lock, &self.tree.dest_shown, false),
        }
    }

    /// Group 2: what is at `DEST/<name>` now. An object of the other kind than the root is a collision; a file root
    /// whose target is the source itself is unsafe. An object of the right kind is the engine's per-file decision.
    fn check_targets(&self) -> Result<(), RunError> {
        let dest = self.tree.dest.as_ref().expect("create_dest made it");
        for root in &self.roots {
            let shown = self.tree.dest_shown.join(&root.name);
            let found = match dest.metadata(&root.name) {
                Ok(m) => m,
                Err(e) if e.source.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(failed(RunStep::Inspect, &shown, e)),
            };
            match (root.kind, found.file_type) {
                (RootKind::File, FileType::Dir) => {
                    return Err(refuse(
                        LockCode::DestinationNamespaceCollision,
                        format!(
                            "{}: the source {} is a file, and a directory is already there",
                            shown.display(),
                            root.src.display()
                        ),
                    ));
                }
                (RootKind::Tree, FileType::File) => {
                    return Err(refuse(
                        LockCode::DestinationNamespaceCollision,
                        format!(
                            "{}: the source {} is a directory, and a file is already there",
                            shown.display(),
                            root.src.display()
                        ),
                    ));
                }
                _ => {}
            }
            if root.kind == RootKind::File
                && matches!(
                    (root.identity, found.identity),
                    (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b
                )
            {
                return Err(refuse(
                    LockCode::SafetyRejected,
                    format!(
                        "{}: it is the source file {} itself",
                        shown.display(),
                        root.src.display()
                    ),
                ));
            }
        }
        Ok(())
    }
}

#[allow(dead_code)] // used by run::session (plan task 7)
impl<F: DestinationRoot> Place<F::Dir> for MultiPlace<'_, F> {
    fn kind(&self) -> Kind {
        self.tree.kind()
    }

    fn destination(&self) -> &Path {
        self.tree.destination()
    }

    fn holder_shown(&self) -> &Path {
        self.tree.holder_shown()
    }

    fn workspace_path(&self, id: &str) -> String {
        self.tree.workspace_path(id)
    }

    fn shown(&self, id: &str) -> PathBuf {
        self.tree.shown(id)
    }

    fn scan(&self, own_id: &str) -> LockResult<Scan> {
        self.tree.scan(own_id)
    }

    fn create(
        &mut self,
        state: &OperationState,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<(), RunError> {
        self.tree.create_dest()?;
        let dir = match self.lock_directory() {
            Ok(dir) => dir,
            Err(e) => return Err(self.roll_back(e)),
        };
        let names: Vec<OsString> = self.roots.iter().map(|r| r.name.clone()).collect();
        let id: PrefixId<'_> = self.prefix_id.as_ref();
        let (locks, leftovers) = match PrefixLocks::acquire(dir, self.capability, &names, &id) {
            Ok(taken) => taken,
            // `acquire` has released what it took.
            Err(refusal) => {
                let e = self.prefix_error(refusal);
                return Err(self.roll_back(e));
            }
        };
        *self.prefix.get_mut() = Some(locks);
        for name in leftovers {
            warnings.push(RunWarning::BrokenLeftover(self.tree.dest_shown.join(name)));
        }
        if let Err(e) = self.check_targets() {
            let failed = self.release_prefix_locks();
            self.warn_release(failed, warnings);
            return Err(self.roll_back(e));
        }
        // The workspace's own rollbacks remove DEST and its control directories, which cannot go while a lock file is
        // in DEST: the locks are released before each of them, and whatever still holds after any other failure here
        // (the early returns of the workspace roll nothing back) is released below.
        let mut released = Vec::new();
        let result = {
            let Self { tree, prefix, .. } = &mut *self;
            tree.create_workspace(state, warnings, &mut || released.extend(release_cell(prefix)))
        };
        if result.is_err() {
            released.extend(self.release_prefix_locks());
        }
        self.warn_release(released, warnings);
        result
    }

    fn write(&self, state: &OperationState) -> flux_fs::Result<()> {
        self.tree.write(state)
    }

    fn sweep(
        &self,
        priors: &[PriorOp],
        locked: &Locked<'_, F::Dir>,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<Vec<String>, Fault> {
        self.tree.sweep(priors, locked, warnings)
    }

    fn remove(&self, id: &str) -> Result<(), (PathBuf, FsError)> {
        // The prefix locks go before the workspace is retired (spec 3.4). A lock that would not release is reported
        // after the workspace has been dealt with, and does not stop it.
        let released = release_cell(&self.prefix);
        self.tree.remove(id)?;
        match released.into_iter().next() {
            None => Ok(()),
            Some((name, e)) => Err((self.lock_path(&name), lock_io(e))),
        }
    }

    fn remove_control_dirs(&mut self, rollback: bool) -> Result<(), (PathBuf, FsError)> {
        self.tree.remove_control_dirs(rollback)
    }

    fn file_identities(&self) -> Option<(FileIdentity, Option<FileIdentity>)> {
        self.tree.file_identities()
    }

    fn finish_completed(
        &self,
        prior: &PriorOp,
        locked: &Locked<'_, F::Dir>,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<Option<u64>, Fault> {
        self.tree.finish_completed(prior, locked, warnings)
    }

    fn remove_debris(
        &self,
        name: &OsStr,
        locked: &Locked<'_, F::Dir>,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<(), Fault> {
        self.tree.remove_debris(name, locked, warnings)
    }

    fn roots(&self) -> Vec<Root> {
        self.roots
            .iter()
            .map(|r| Root {
                source_root: native_hex(&absolute_lexical(&r.src)),
                source_identity: identity_text(r.identity),
                destination_prefix: native_hex(Path::new(&r.name)),
                kind: Some(r.kind),
            })
            .collect()
    }

    fn adopt(
        &mut self,
        _prior: &OperationState,
        _durability: Durability,
        _warnings: &mut Vec<RunWarning>,
    ) -> Result<Option<AdoptedStore>, RunError> {
        Err(refused_state(
            LockCode::IncompatibleState,
            "resuming several sources is not supported in this version".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::{FakeDirHandle, FaultFs};
    use crate::run::place::{LocatedTree, locate_dest};
    use crate::state::from_native_hex;
    use flux_fs::{Code, FileSystem, ObjectId};

    const ID: &str = "11111111111111111111111111111111";

    /// The fake finds a handle's path by the identity's index alone (`fault_fs.rs`, `ObjectId` lookups), so a test's
    /// identities sit far above the ones the fake mints.
    fn object(index: u128) -> FileIdentity {
        FileIdentity::Strong(ObjectId { volume: 1, index: 9_000_000 + index })
    }

    /// `/src/d` (a directory) and `/src/f` (a file) are the sources; `/p` exists and `/p/dest` does not.
    struct Fx {
        fs: FaultFs,
        located: LocatedTree<FakeDirHandle>,
        lock_dir: OnceCell<FakeDirHandle>,
    }

    fn fx() -> Fx {
        let fs = FaultFs::new();
        for d in ["/src", "/src/d", "/p"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/src/f", b"F");
        fs.set_identity("/src/f", object(7));
        let located = locate_dest(&fs, Path::new("/p/dest")).expect("/p is there");
        Fx { fs, located, lock_dir: OnceCell::new() }
    }

    fn root(name: &str, kind: RootKind) -> RootPlan {
        let (src, identity) = match kind {
            RootKind::Tree => ("/src/d", object(6)),
            RootKind::File => ("/src/f", object(7)),
        };
        RootPlan { name: OsString::from(name), kind, src: PathBuf::from(src), identity }
    }

    fn place<'p>(fx: &'p Fx, roots: Vec<RootPlan>) -> MultiPlace<'p, FaultFs> {
        let located = &fx.located;
        // DEST, if the test made it before the place is built.
        let dest = located.name.as_ref().and_then(|n| located.holder.open_dir(n).ok());
        MultiPlace {
            tree: TreePlace {
                fs: &fx.fs,
                holder: &located.holder,
                holder_shown: located.holder_shown.clone(),
                name: located.name.clone(),
                dest,
                dest_shown: PathBuf::from("/p/dest"),
                created_dest: false,
                operations: None,
                claims: None,
                durability: Durability::Normal,
                src_root: PathBuf::from("/src"),
                src_identity: object(1),
            },
            roots,
            lock_dir: &fx.lock_dir,
            capability: LockCapability::LocalStrong,
            prefix_id: PrefixIdOwned {
                operation_id: ID.to_string(),
                owner_instance_id: "owner".to_string(),
                boot_session_id: "boot".to_string(),
                now: 5,
            },
            prefix: RefCell::new(None),
        }
    }

    fn state() -> OperationState {
        OperationState::created_v1(ID, Kind::Tree, Path::new("/p/dest"), 1)
    }

    fn calls(fs: &FaultFs) -> Vec<String> {
        fs.calls().iter().map(|c| c.replace('\\', "/")).collect()
    }

    fn at(calls: &[String], prefix: &str) -> usize {
        calls
            .iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} in {calls:?}"))
    }

    fn refused(r: &Result<(), RunError>) -> (LockCode, bool) {
        match r {
            Err(RunError::Refused { refusal, changed, .. }) => (refusal.code, *changed),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    fn lock_files(fs: &FaultFs) -> Vec<String> {
        ["data", "Data", "f"]
            .iter()
            .map(|n| format!("/p/dest/{n}.flux-lock"))
            .filter(|p| fs.exists(p))
            .collect()
    }

    #[test]
    fn create_orders_dest_then_locks_then_checks_then_workspace() {
        let fx = fx();
        let mut p = place(&fx, vec![root("data", RootKind::Tree), root("f", RootKind::File)]);
        let mut warnings = Vec::new();
        p.create(&state(), &mut warnings).expect("created");
        let c = calls(&fx.fs);
        let dest = at(&c, "create_dir(/p/dest)");
        let first_lock = at(&c, "create_lock(");
        let last_lock = c.iter().rposition(|x| x.starts_with("create_lock(")).unwrap();
        let check = at(&c, "metadata(/p/dest/data)");
        let workspace = at(&c, &format!("create_dir(/p/dest/.flux/operations/{ID}.creating)"));
        assert!(dest < first_lock && first_lock < last_lock, "{c:?}");
        assert!(last_lock < check && check < workspace, "{c:?}");
        assert_eq!(lock_files(&fx.fs).len(), 2, "both locks are held while the run lives");
        assert!(p.release_prefix_locks().is_empty());
        assert!(lock_files(&fx.fs).is_empty());
    }

    #[test]
    fn a_group_2_refusal_makes_no_workspace_and_removes_a_dest_it_made() {
        let fx = fx();
        // Another party puts a directory at the file root's name once DEST exists, and it is gone again by the time
        // DEST is removed: what is pinned is that the rollback removes the DEST this run made.
        fx.fs.on_nth("create_lock", 1, |fs| {
            fs.create_dir(Path::new("/p/dest/data")).unwrap();
        });
        fx.fs.on_nth("remove_dir", 1, |fs| {
            let dest = fs.destination_root(Path::new("/p/dest")).unwrap();
            dest.remove_dir(OsStr::new("data")).unwrap();
        });
        let mut p = place(&fx, vec![root("data", RootKind::File)]);
        let r = p.create(&state(), &mut Vec::new());
        assert_eq!(refused(&r), (LockCode::DestinationNamespaceCollision, false));
        let c = calls(&fx.fs);
        let check = at(&c, "metadata(/p/dest/data)");
        assert!(check > at(&c, "create_lock("), "{c:?}");
        assert!(!c[check..].iter().any(|x| x.starts_with("create_dir(/p/dest/.flux")), "{c:?}");
        assert!(!fx.fs.exists("/p/dest"), "DEST is removed");
        assert!(!fx.fs.exists("/p/dest/data.flux-lock"));
    }

    #[test]
    fn a_dest_that_cannot_be_emptied_is_reported_as_not_removed() {
        let fx = fx();
        fx.fs.on_nth("create_lock", 1, |fs| {
            fs.create_dir(Path::new("/p/dest/data")).unwrap();
        });
        let mut p = place(&fx, vec![root("data", RootKind::File)]);
        match p.create(&state(), &mut Vec::new()) {
            Err(RunError::Refused { refusal, changed: true, not_removed: Some((path, _)) }) => {
                assert_eq!(refusal.code, LockCode::DestinationNamespaceCollision);
                assert_eq!(path, PathBuf::from("/p/dest"));
            }
            other => panic!("expected a refusal with a leftover, got {other:?}"),
        }
        assert!(!fx.fs.exists("/p/dest/data.flux-lock"), "the lock is released first");
    }

    #[test]
    fn a_target_that_is_the_source_is_refused_safety_rejected() {
        let fx = fx();
        fx.fs.create_dir(Path::new("/p/dest")).unwrap();
        fx.fs.write_file("/p/dest/f", b"F");
        fx.fs.set_identity("/p/dest/f", object(7));
        let mut p = place(&fx, vec![root("f", RootKind::File)]);
        let r = p.create(&state(), &mut Vec::new());
        assert_eq!(refused(&r), (LockCode::SafetyRejected, false));
        let c = calls(&fx.fs);
        let check = at(&c, "metadata(/p/dest/f)");
        assert!(!c[check..].iter().any(|x| x.starts_with("create_dir(/p/dest/.flux")), "{c:?}");
        assert!(fx.fs.exists("/p/dest"), "a DEST that was there stays");
        assert!(lock_files(&fx.fs).is_empty());
    }

    #[test]
    fn an_object_of_the_right_kind_is_left_to_the_engine() {
        let fx = fx();
        fx.fs.create_dir(Path::new("/p/dest")).unwrap();
        fx.fs.create_dir(Path::new("/p/dest/data")).unwrap();
        fx.fs.write_file("/p/dest/f", b"other");
        fx.fs.set_identity("/p/dest/f", object(99));
        let mut p = place(&fx, vec![root("data", RootKind::Tree), root("f", RootKind::File)]);
        p.create(&state(), &mut Vec::new()).expect("created");
        assert!(p.release_prefix_locks().is_empty());
    }

    #[test]
    fn a_folder_root_over_a_file_is_a_collision() {
        let fx = fx();
        fx.fs.create_dir(Path::new("/p/dest")).unwrap();
        fx.fs.write_file("/p/dest/data", b"x");
        let mut p = place(&fx, vec![root("data", RootKind::Tree)]);
        let r = p.create(&state(), &mut Vec::new());
        assert_eq!(refused(&r), (LockCode::DestinationNamespaceCollision, false));
        assert!(lock_files(&fx.fs).is_empty());
    }

    #[test]
    fn an_alias_refuses_and_leaves_nothing() {
        let fx = fx();
        fx.fs.set_case_insensitive(true);
        let mut p = place(&fx, vec![root("Data", RootKind::Tree), root("data", RootKind::Tree)]);
        let r = p.create(&state(), &mut Vec::new());
        assert_eq!(refused(&r), (LockCode::DestinationNamespaceCollision, false));
        let c = calls(&fx.fs);
        let second = c.iter().rposition(|x| x.starts_with("create_lock(")).unwrap();
        assert!(!c[second..].iter().any(|x| x.starts_with("create_dir(/p/dest/.flux")), "{c:?}");
        assert!(lock_files(&fx.fs).is_empty());
        assert!(!fx.fs.exists("/p/dest"), "the DEST this run made is gone");
    }

    #[test]
    fn a_claim_store_failure_rolls_back_dest_and_the_prefix_locks() {
        let fx = fx();
        fx.fs.fail("create_claim_store", Code::IoError);
        let mut p = place(&fx, vec![root("data", RootKind::Tree)]);
        let mut warnings = Vec::new();
        match p.create(&state(), &mut warnings) {
            Err(RunError::Failed { step: RunStep::State, .. }) => {}
            other => panic!("expected a state failure, got {other:?}"),
        }
        let c = calls(&fx.fs);
        let at_store = at(&c, "create_claim_store(");
        assert!(at_store > at(&c, "create_lock("), "{c:?}");
        assert!(!fx.fs.exists("/p/dest"), "DEST and the workspace are gone");
        assert!(lock_files(&fx.fs).is_empty());
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn an_early_workspace_failure_still_releases_the_prefix_locks() {
        let fx = fx();
        let made = fx.fs.calls().iter().filter(|c| c.starts_with("create_dir(")).count();
        // DEST, `.flux`, `operations`, then `<id>.creating`.
        fx.fs.fail_nth(
            "create_dir",
            u32::try_from(made + 4).unwrap(),
            Code::IoError,
            ErrorKind::Other,
        );
        let mut p = place(&fx, vec![root("data", RootKind::Tree)]);
        match p.create(&state(), &mut Vec::new()) {
            Err(RunError::Failed { step: RunStep::State, path, .. }) => {
                assert!(path.to_string_lossy().ends_with(".creating"), "{path:?}");
            }
            other => panic!("expected a state failure, got {other:?}"),
        }
        let c = calls(&fx.fs);
        let failed_at = at(&c, &format!("create_dir(/p/dest/.flux/operations/{ID}.creating)"));
        assert!(failed_at > at(&c, "create_lock("), "{c:?}");
        assert!(lock_files(&fx.fs).is_empty(), "no lock file remains");
        assert!(fx.fs.exists("/p/dest"), "no rollback on this path, as for a tree");
    }

    #[test]
    fn remove_releases_the_prefix_locks_before_the_workspace_is_retired() {
        let fx = fx();
        let mut p = place(&fx, vec![root("data", RootKind::Tree)]);
        p.create(&state(), &mut Vec::new()).expect("created");
        assert_eq!(lock_files(&fx.fs).len(), 1);
        p.tree.claims = None;
        let mark = fx.fs.calls().len();
        p.remove(ID).expect("removed");
        let c = calls(&fx.fs)[mark..].to_vec();
        let unlock = at(&c, "remove_file(/p/dest/data.flux-lock)");
        let retire = at(&c, "rename_no_replace(");
        assert!(unlock < retire, "{c:?}");
        assert!(lock_files(&fx.fs).is_empty());
    }

    #[test]
    fn roots_records_kind_prefix_and_absolute_source_per_root() {
        let fx = fx();
        let mut relative = root("data", RootKind::Tree);
        relative.src = PathBuf::from("rel/d");
        let p = place(&fx, vec![relative, root("f", RootKind::File)]);
        let roots = p.roots();
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].kind, Some(RootKind::Tree));
        assert_eq!(roots[1].kind, Some(RootKind::File));
        assert_eq!(roots[0].destination_prefix, native_hex(Path::new("data")));
        assert_eq!(roots[1].destination_prefix, native_hex(Path::new("f")));
        assert_eq!(roots[0].source_identity, identity_text(object(6)));
        assert_eq!(roots[1].source_identity, identity_text(object(7)));
        let first = from_native_hex(&roots[0].source_root).expect("decodes");
        assert!(first.is_absolute() && first.ends_with("rel/d"), "{first:?}");
        assert_eq!(
            from_native_hex(&roots[1].source_root).expect("decodes"),
            PathBuf::from("/src/f")
        );
    }

    #[test]
    fn kind_is_tree_and_workspace_path_is_operations_id() {
        let fx = fx();
        let p = place(&fx, vec![root("data", RootKind::Tree)]);
        assert_eq!(p.kind(), Kind::Tree);
        assert_eq!(p.workspace_path(ID), format!("operations/{ID}"));
    }

    #[test]
    fn adopting_a_prior_is_refused_as_an_incompatible_state() {
        let fx = fx();
        let mut p = place(&fx, vec![root("data", RootKind::Tree)]);
        let r = p.adopt(&state(), Durability::Normal, &mut Vec::new());
        match r {
            Err(RunError::Refused { refusal, changed: false, .. }) => {
                assert_eq!(refusal.code, LockCode::IncompatibleState);
                assert!(refusal.detail.contains("resuming several sources is not supported"));
            }
            other => panic!("expected a refusal, got {:?}", other.map(|_| ())),
        }
    }
}
