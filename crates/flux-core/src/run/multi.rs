//! The place of a several-source run (spec 3.4, 3.5): a `TreePlace` for DEST plus one prefix lock and one root per
//! source. Its `create` makes DEST, takes the prefix locks inside it, checks the names the sources take, and only then
//! makes the workspace, so a refusal at any of those finds no workspace and releases every lock it took.

use super::place::{AdoptedStore, LocatedTree, Place, TreePlace, locate_dest, refused_state};
use super::prefix::{PrefixId, PrefixIdOwned, PrefixLocks, PrefixRefusal};
use super::roots::{SourceKind, SourceSpec, plan_roots};
use super::session::{Fault, Locked, failed, from_lock, lock_io};
use super::{Run, RunConfig, RunError, RunStep, RunWarning, TreeAt, checkpoint_policy, drive};
use crate::copy::{CopyError, CopyStep};
use crate::lock::{LockCode, LockError, LockResult, LockSite, Refusal, check_capability};
use crate::prior::{PriorOp, Scan, check_control_plane};
use crate::state::{
    Kind, OperationState, Root, RootKind, absolute_lexical, identity_text, native_hex, wall_time_ns,
};
use crate::tree::{
    RootDone, RootInput, RootStatus, TreeAbort, TreeFailure, TreeOutcome, containment,
    copy_roots_at, preflight, prepare_source,
};
use crate::walk::MountPolicy;
use flux_fs::{
    Code, CopyOptions, DestinationRoot, DirHandle, Durability, FileIdentity, FileType, FsError,
    LockCapability, OperationId,
};
use std::cell::{OnceCell, RefCell};
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// One root of the run: the name it takes under DEST, what it is, where it comes from, and the identity it had when
/// the run opened it.
pub(crate) struct RootPlan {
    pub name: OsString,
    pub kind: RootKind,
    pub src: PathBuf,
    pub identity: FileIdentity,
}

/// A tree's `TreePlace`, with the prefix locks of its roots.
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
                (RootKind::Tree, FileType::Dir) => {
                    // The same directory as the source (a bind mount, a second mount of one export): a Weak or
                    // Unavailable identity never decides.
                    if matches!(
                        (root.identity, found.identity),
                        (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b
                    ) {
                        return Err(refuse(
                            LockCode::SafetyRejected,
                            format!(
                                "{}: it is the source directory {} itself",
                                shown.display(),
                                root.src.display()
                            ),
                        ));
                    }
                }
                // Anything else at a folder root's name (a file, a link, a special file): the walk would open it as a
                // directory without following links and abort mid-run, after earlier roots were copied.
                (RootKind::Tree, other) => {
                    return Err(refuse(
                        LockCode::DestinationNamespaceCollision,
                        format!(
                            "{}: the source {} is a directory, and a {other:?} is already there",
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

impl<'p, F: DestinationRoot> TreeAt<'p, F> for MultiPlace<'p, F> {
    fn tree_place(&self) -> &TreePlace<'p, F> {
        &self.tree
    }

    fn tree_place_mut(&mut self) -> &mut TreePlace<'p, F> {
        &mut self.tree
    }
}

/// How one root of a several-source run ended, for the report (spec 3.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootReport {
    pub name: OsString,
    pub status: RootStatus,
    pub files_copied: u64,
    pub files_failed: u64,
}

/// A several-source run: the run, as a tree's, and one report per root in prefix order. `roots` is empty when the run
/// stopped before transfer (a refusal), so nothing was begun.
#[derive(Debug)]
pub struct MultiRun {
    pub run: Run<Result<TreeOutcome, TreeAbort>>,
    pub roots: Vec<RootReport>,
}

/// A run that ended before it had a lock: nothing was begun, so no root is reported.
fn ended_early(run: Run<Result<TreeOutcome, TreeAbort>>) -> MultiRun {
    MultiRun { run, roots: Vec::new() }
}

fn aborted(
    mut run: Run<Result<TreeOutcome, TreeAbort>>,
    outcome: TreeOutcome,
    error: CopyError,
) -> MultiRun {
    run.copy = Some(Err(TreeAbort { error, outcome }));
    ended_early(run)
}

/// What a file root is before the lock: a regular file other than `DEST/<name>`, with its identity (the file half of
/// B1: `prepare_file` without a destination to open).
fn prepare_file_root<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    target: &Path,
) -> Result<FileIdentity, CopyError> {
    if src == target {
        return Err(CopyError::at(
            CopyStep::Resolve,
            FsError::new(
                Code::SafetyRejected,
                std::io::Error::other("source and destination are the same path"),
            ),
        ));
    }
    let meta = fs.metadata(src).map_err(|e| CopyError::at(CopyStep::Source, e))?;
    if meta.file_type != FileType::File {
        return Err(CopyError::at(
            CopyStep::Source,
            FsError::new(Code::SpecialFileUnsupported, std::io::Error::other("not a regular file")),
        ));
    }
    Ok(meta.identity)
}

/// Several sources under the destination's lock (spec 3.3 to 3.8): `specs` is two or more sources, and the run is not
/// a dry one. `tree_with`'s run, with the roots' prefix locks taken inside DEST before the workspace exists and
/// released on every path before `finish`.
pub fn trees<F: DestinationRoot>(
    fs: &F,
    specs: Vec<SourceSpec>,
    dst_root: &Path,
    opts: &CopyOptions,
    cfg: &RunConfig,
    on_report: &mut dyn FnMut(TreeFailure),
) -> MultiRun {
    // A one-root manifest would be format 4 with a `kind`, which the validator rejects: the caller routes one source
    // to `tree` or `file`.
    debug_assert!(!cfg.dry_run, "a dry run of several sources is not supported");
    // `MultiPlace::remove` releases this run's prefix locks whatever operation id it is given, and a supersede or an
    // adoption removes a prior's: neither is supported with several sources.
    debug_assert!(
        !cfg.restart && !cfg.resume,
        "several sources support neither --restart nor --resume"
    );
    debug_assert!(specs.len() >= 2, "one source is `tree` or `file`");
    let mut run = Run {
        copy: None,
        stop: None,
        warnings: Vec::new(),
        resumed: None,
        beat_gap_max: std::time::Duration::ZERO,
        file_resume: None,
        dry: None,
    };
    let opts =
        CopyOptions { operation_id: OperationId::new(cfg.operation_id.as_str()), ..opts.clone() };
    let mut out = TreeOutcome::default();
    // Group 1: the names, then the sources (B1), before DEST is touched.
    let specs = match plan_roots(specs) {
        Ok(s) => s,
        Err(e) => {
            run.stop = Some(e);
            return ended_early(run);
        }
    };
    let mut plans = Vec::with_capacity(specs.len());
    let mut inputs: Vec<RootInput<'_, F>> = Vec::with_capacity(specs.len());
    for spec in &specs {
        let target = dst_root.join(&spec.name);
        let (kind, identity) = match spec.kind {
            SourceKind::Folder => {
                match prepare_source(fs, &spec.src, &target, MountPolicy::of(&opts)) {
                    Ok(source) => {
                        let identity = source.identity;
                        inputs.push(RootInput::Folder {
                            name: spec.name.clone(),
                            src_root: spec.src.clone(),
                            source,
                        });
                        (RootKind::Tree, identity)
                    }
                    Err(error) => return aborted(run, out, error),
                }
            }
            SourceKind::File if spec.src.file_name() != Some(spec.name.as_os_str()) => {
                // The copy reads `<parent of the source>/<name>`, so the name must be the source's own.
                run.stop = Some(refuse(
                    LockCode::PathComponentInvalid,
                    format!(
                        "the name {:?} for the source file {} is not its file name",
                        spec.name,
                        spec.src.display()
                    ),
                ));
                return ended_early(run);
            }
            SourceKind::File => match prepare_file_root(fs, &spec.src, &target) {
                Ok(identity) => {
                    // A bare file name has the empty path as its parent: the working directory.
                    let src_parent = match spec.src.parent() {
                        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                        _ => PathBuf::from("."),
                    };
                    inputs.push(RootInput::File { name: spec.name.clone(), src_parent, identity });
                    (RootKind::File, identity)
                }
                Err(error) => return aborted(run, out, error),
            },
        };
        plans.push(RootPlan { name: spec.name.clone(), kind, src: spec.src.clone(), identity });
    }
    // DEST once, then the identity pre-flight and the containment check of each folder root, as `locate_tree` runs
    // them for one tree.
    let LocatedTree { holder, holder_shown, name, dest } = match locate_dest(fs, dst_root) {
        Ok(l) => l,
        Err(error) => return aborted(run, out, error),
    };
    let anchor = match &dest {
        Some(d) => d.identity(),
        None => holder.identity(),
    };
    let anchor = match anchor {
        Ok(a) => a,
        Err(e) => return aborted(run, out, CopyError::at(CopyStep::Resolve, e)),
    };
    let (anchor_handle, anchor_shown) = match &dest {
        Some(d) => (d, dst_root),
        None => (&holder, holder_shown.as_path()),
    };
    for plan in plans.iter().filter(|p| p.kind == RootKind::Tree) {
        let checked =
            preflight(plan.identity, anchor, opts.safety, &mut out.warnings).and_then(|()| {
                containment(fs, &plan.src, anchor_handle, anchor_shown, opts.safety, &mut out)
            });
        if let Err(error) = checked {
            return aborted(run, out, error);
        }
    }
    // Step 1: the lock's filesystem (decision 14).
    let capability = match check_capability(&holder) {
        Ok(c) => c,
        Err(e) => {
            run.stop = Some(from_lock(e, RunStep::Lock, &holder_shown, false));
            return ended_early(run);
        }
    };
    // Step 2: DEST's control plane, where DEST exists.
    if let Some(dest) = &dest
        && let Err(e) = check_control_plane(dest, dst_root)
    {
        run.stop = Some(from_lock(e, RunStep::Inspect, dst_root, false));
        return ended_early(run);
    }
    let site = match &name {
        Some(name) => LockSite::directory(&holder, name),
        None => Ok(LockSite::root(&holder)),
    };
    let site = match site {
        Ok(s) => s,
        Err(e) => {
            run.stop = Some(from_lock(e, RunStep::Lock, &holder_shown, false));
            return ended_early(run);
        }
    };
    // Owned by this function, so it outlives the place that opens the prefix locks' directory into it.
    let lock_dir = OnceCell::new();
    let first = (specs[0].src.clone(), plans[0].identity);
    let mut place = MultiPlace {
        tree: TreePlace {
            fs,
            holder: &holder,
            holder_shown,
            name,
            dest,
            dest_shown: dst_root.to_path_buf(),
            created_dest: false,
            operations: None,
            claims: None,
            durability: opts.durability,
            // Read only by `TreePlace::roots`, which `MultiPlace` overrides.
            src_root: first.0.clone(),
            src_identity: first.1,
        },
        roots: plans,
        lock_dir: &lock_dir,
        capability,
        prefix_id: PrefixIdOwned {
            operation_id: cfg.operation_id.clone(),
            owner_instance_id: cfg.owner_instance_id.clone(),
            boot_session_id: cfg.boot_session_id.clone(),
            now: wall_time_ns(),
        },
        prefix: RefCell::new(None),
    };
    let mut done: Vec<RootDone> = Vec::new();
    let run = drive(
        fs,
        run,
        &mut place,
        &site,
        capability,
        cfg,
        &opts,
        checkpoint_policy(cfg, opts.durability),
        &first.0,
        first.1,
        out,
        on_report,
        |cx, place, out, report| {
            let dest = place.tree.dest.as_ref().expect("step 5 made DEST");
            let (roots, walked) =
                copy_roots_at(cx, dest, place.tree.created_dest, inputs, out, report);
            done = roots;
            walked
        },
        // Every exit releases the prefix locks: a `Held` has no `Drop`.
        |place, warnings| {
            let failed = place.release_prefix_locks();
            place.warn_release(failed, warnings);
        },
    );
    let roots = if done.is_empty() && run.copy.is_some() {
        // The copy ended before any root was begun (the checkpoint log could not be opened): every root is unstarted.
        place
            .roots
            .iter()
            .map(|r| RootReport {
                name: r.name.clone(),
                status: RootStatus::NotStarted,
                files_copied: 0,
                files_failed: 0,
            })
            .collect()
    } else {
        done.into_iter()
            .map(|d| RootReport {
                name: d.name,
                status: d.status,
                files_copied: d.files_copied,
                files_failed: d.files_failed,
            })
            .collect()
    };
    MultiRun { run, roots }
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
            std::path::absolute("/src/f").expect("absolute")
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

/// `run::trees` on the fake filesystem. `/src/{a,b,c}` are folders, `/src/f` is a file; `/p` exists and `/p/dest` does
/// not unless a test makes it.
#[cfg(test)]
mod trees_tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use crate::run::ResumeVerify;
    use crate::state::{OpState, decode as decode_state, from_native_hex};
    use flux_fs::{ExistingPolicy, FileSystem, Links, ObjectId, Preserve, Publish, Safety};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};

    const ID: &str = "11111111111111111111111111111111";
    const LOCK: &str = "/p/dest.flux-lock";
    const THEIRS: &[u8] = b"another run's bytes";

    fn cfg() -> RunConfig {
        RunConfig {
            restart: false,
            break_lock: false,
            operation_id: ID.to_string(),
            owner_instance_id: format!("{:032x}", 0xee),
            boot_session_id: "test-boot".to_string(),
            before_mutation: None,
            heartbeat_interval: Duration::from_secs(3600),
            resume: false,
            descriptor_limit: None,
            resume_verify: ResumeVerify::Chunks,
            dry_run: false,
            dry_temp_root: None,
        }
    }

    fn opts() -> CopyOptions {
        CopyOptions {
            preserve_times: Preserve::Default,
            preserve_permissions: Preserve::Default,
            durability: Durability::Normal,
            publish: Publish::Replace,
            safety: Safety::Default,
            operation_id: OperationId::new("replaced by the run"),
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            cross_filesystems: false,
        }
    }

    fn fake() -> FaultFs {
        let fs = FaultFs::new();
        for d in ["/src", "/src/a", "/src/a/sub", "/src/b", "/src/c", "/p"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        for (path, bytes) in [
            ("/src/a/x", &b"ax"[..]),
            ("/src/a/sub/y", b"ay"),
            ("/src/b/k", b"bk"),
            ("/src/c/z", b"cz"),
            ("/src/f", b"F"),
        ] {
            fs.write_file(path, bytes);
        }
        fs
    }

    fn spec(name: &str) -> SourceSpec {
        let kind = if name == "f" { SourceKind::File } else { SourceKind::Folder };
        SourceSpec { src: Path::new("/src").join(name), name: OsString::from(name), kind }
    }

    fn go_with(
        fs: &FaultFs,
        specs: Vec<SourceSpec>,
        o: &CopyOptions,
        c: &RunConfig,
    ) -> (MultiRun, Vec<TreeFailure>) {
        let mut got = Vec::new();
        let r = trees(fs, specs, Path::new("/p/dest"), o, c, &mut |f| got.push(f));
        (r, got)
    }

    fn go(fs: &FaultFs, names: &[&str]) -> (MultiRun, Vec<TreeFailure>) {
        go_with(fs, names.iter().map(|n| spec(n)).collect(), &opts(), &cfg())
    }

    fn calls(fs: &FaultFs) -> Vec<String> {
        fs.calls().iter().map(|c| c.replace('\\', "/")).collect()
    }

    /// The 1-based count, among the calls named `op`, of the first one that mentions `needle`: the `n` for `fail_nth`
    /// and `on_nth`, read from a reference run on an identical fixture (the fake is deterministic).
    fn nth_of(fs: &FaultFs, op: &str, needle: &str) -> u32 {
        let head = format!("{op}(");
        let mut n = 0;
        for c in calls(fs) {
            if c.starts_with(&head) {
                n += 1;
                if c.contains(needle) {
                    return n;
                }
            }
        }
        panic!("no {op} mentioning {needle} in {:?}", calls(fs));
    }

    /// The reference run's `nth_of`, for a fixture `build` makes.
    fn reference(build: impl Fn() -> FaultFs, names: &[&str], op: &str, needle: &str) -> u32 {
        let fs = build();
        let (r, _) = go(&fs, names);
        assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
        nth_of(&fs, op, needle)
    }

    fn lose_lock(fs: &FaultFs, op: &str, nth: u32) {
        fs.on_nth(op, nth, |fs| fs.write_file(LOCK, THEIRS));
    }

    type Seen = Arc<Mutex<Option<OperationState>>>;

    /// The manifest as it is on disk when the `nth` call named `op` is made.
    fn watch_manifest(fs: &FaultFs, op: &str, nth: u32) -> Seen {
        let seen: Seen = Arc::default();
        let keep = Arc::clone(&seen);
        fs.on_nth(op, nth, move |fs| {
            let bytes = fs
                .read_file(format!("/p/dest/.flux/operations/{ID}/manifest"))
                .expect("the manifest exists");
            *keep.lock().unwrap() = Some(decode_state(&bytes).unwrap());
        });
        seen
    }

    fn manifest(fs: &FaultFs) -> OperationState {
        let bytes = fs
            .read_file(format!("/p/dest/.flux/operations/{ID}/manifest"))
            .expect("the workspace is kept");
        decode_state(&bytes).unwrap()
    }

    fn copy_of(r: &MultiRun) -> &Result<TreeOutcome, TreeAbort> {
        r.run.copy.as_ref().expect("the copy ran")
    }

    fn ok(r: &MultiRun) -> &TreeOutcome {
        assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
        match copy_of(r) {
            Ok(o) => o,
            Err(a) => panic!("the copy aborted: {a:?}"),
        }
    }

    fn statuses(r: &MultiRun) -> Vec<RootStatus> {
        r.roots.iter().map(|x| x.status).collect()
    }

    fn names(r: &MultiRun) -> Vec<OsString> {
        r.roots.iter().map(|x| x.name.clone()).collect()
    }

    fn os(names: &[&str]) -> Vec<OsString> {
        names.iter().map(OsString::from).collect()
    }

    fn lock_files(fs: &FaultFs) -> Vec<String> {
        let mut found: Vec<String> = ["a", "b", "c", "f", "sub"]
            .iter()
            .map(|n| format!("/p/dest/{n}.flux-lock"))
            .chain(std::iter::once(LOCK.to_string()))
            .filter(|p| fs.exists(p))
            .collect();
        found.sort();
        found
    }

    fn prefix_lock_files(fs: &FaultFs) -> Vec<String> {
        lock_files(fs).into_iter().filter(|p| p != LOCK).collect()
    }

    fn only_code(got: &[TreeFailure]) -> Code {
        assert_eq!(got.len(), 1, "{got:?}");
        match &got[0].cause {
            crate::tree::TreeFailureCause::Copy(e) => e.code(),
            crate::tree::TreeFailureCause::CreateDir(e) => e.code,
            other => panic!("expected a copy or directory failure, got {other:?}"),
        }
    }

    fn secs(s: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(s)
    }

    fn strong(fs: &FaultFs, path: &str) -> bool {
        matches!(fs.metadata(Path::new(path)).unwrap().identity, FileIdentity::Strong(_))
    }

    #[test]
    fn two_folders_and_a_file_land_at_dest_name() {
        let fs = fake();

        let (r, got) = go(&fs, &["a", "b", "f"]);

        let out = ok(&r);
        assert!(got.is_empty() && r.run.warnings.is_empty(), "{got:?} {:?}", r.run.warnings);
        for (path, bytes) in [("a/x", "ax"), ("a/sub/y", "ay"), ("b/k", "bk"), ("f", "F")] {
            assert_eq!(
                fs.read_file(Path::new("/p/dest").join(path)).as_deref(),
                Some(bytes.as_bytes()),
                "{path}"
            );
        }
        // DEST, `DEST/a`, `DEST/b`, and `DEST/a/sub` (the walk's).
        assert_eq!(out.directories_created, 4);
        assert_eq!((out.files_total, out.files_copied), (4, 4));
        assert_eq!(statuses(&r), [RootStatus::Completed; 3]);
        assert_eq!(names(&r), os(&["a", "b", "f"]));
        assert!(!fs.exists("/p/dest/.flux"), "the workspace is gone");
        assert!(lock_files(&fs).is_empty(), "{:?}", lock_files(&fs));
    }

    /// The indices of the calls that touch `DEST/<name>` or below, not the prefix lock `DEST/<name>.flux-lock`.
    fn touches(c: &[String], name: &str) -> Vec<usize> {
        let dir = format!("create_dir(/p/dest/{name})");
        let below = format!("/p/dest/{name}/");
        c.iter()
            .enumerate()
            .filter(|(_, x)| **x == dir || x.contains(&below))
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn roots_are_copied_in_prefix_order_whatever_the_argument_order() {
        for given in [["a", "b", "c"], ["c", "b", "a"], ["b", "c", "a"]] {
            let fs = fake();

            let (r, _) = go(&fs, &given);

            ok(&r);
            let c = calls(&fs);
            let (a, b, z) = (touches(&c, "a"), touches(&c, "b"), touches(&c, "c"));
            assert!(
                a.last() < b.first() && b.last() < z.first(),
                "a, then b, then c, given {given:?}: {c:?}"
            );
            assert_eq!(names(&r), os(&["a", "b", "c"]), "given {given:?}");
        }
    }

    /// `/src/f` holding `NEW` modified at second 20, `/p/dest` made, and `DEST/f` holding `OLD` modified at `dest`
    /// when it is `Some`; and the same for the single-file command, which targets `/p/out/f`. Returns the multi run
    /// and the single one.
    fn file_case(
        policy: ExistingPolicy,
        dest: Option<u64>,
    ) -> (MultiRun, FaultFs, Run<Result<flux_fs::Outcome, CopyError>>, FaultFs) {
        let build = |dir: &str| {
            let fs = fake();
            fs.write_file("/src/f", b"NEW");
            fs.set_modified("/src/f", secs(20));
            fs.create_dir(Path::new(dir)).unwrap();
            if let Some(t) = dest {
                let target = format!("{dir}/f");
                fs.write_file(&target, b"OLD");
                fs.set_modified(&target, secs(t));
                assert!(strong(&fs, &target) && strong(&fs, "/src/f"));
            }
            fs
        };
        let o = CopyOptions { existing: policy, ..opts() };
        let multi = build("/p/dest");
        let (r, got) = go_with(&multi, vec![spec("b"), spec("f")], &o, &cfg());
        assert!(got.is_empty(), "{got:?}");
        let single = build("/p/out");
        let one = crate::run::file(&single, Path::new("/src/f"), Path::new("/p/out/f"), &o, &cfg());
        (r, multi, one, single)
    }

    #[test]
    fn an_existing_name_follows_the_existing_policy_for_a_file_root() {
        for policy in
            [ExistingPolicy::Overwrite, ExistingPolicy::Update, ExistingPolicy::SkipExisting]
        {
            for dest in [None, Some(10), Some(30)] {
                let (r, multi, one, single) = file_case(policy, dest);
                let label = format!("{policy:?} with a destination modified at {dest:?}");
                let out = ok(&r);
                assert!(one.stop.is_none(), "{label}: {:?}", one.stop);
                let Some(Ok(single_out)) = &one.copy else { panic!("{label}: {:?}", one.copy) };

                // The decision the policy makes: a skip keeps what is there.
                let skipped = match (policy, dest) {
                    (_, None) | (ExistingPolicy::Overwrite, _) => false,
                    (ExistingPolicy::SkipExisting, Some(_)) => true,
                    (ExistingPolicy::Update, Some(t)) => t >= 20,
                };
                let bytes: &[u8] = if skipped { b"OLD" } else { b"NEW" };
                assert_eq!(multi.read_file("/p/dest/f").as_deref(), Some(bytes), "{label}");
                assert_eq!(single.read_file("/p/out/f").as_deref(), Some(bytes), "{label}");
                assert_eq!(single_out.skipped, skipped, "{label}");
                assert_eq!(out.files_skipped, u64::from(skipped), "{label}");
                // The folder root `b` adds one file of two bytes.
                assert_eq!(out.files_copied, u64::from(!skipped) + 1, "{label}");
                assert_eq!(out.bytes_copied, single_out.bytes_copied + 2, "{label}");
                assert_eq!(out.files_overwritten, u64::from(!skipped && dest.is_some()), "{label}");
                assert_eq!(statuses(&r), [RootStatus::Completed; 2], "{label}");
            }
        }
    }

    #[test]
    fn a_weak_destination_identity_makes_a_file_root_a_collision_like_a_tree_file() {
        // Spec 3.6, the first accepted difference: the identity of the destination DIRECTORY (the DEST that holds the
        // file root's target) is not Strong. A tree file under it cannot claim its target, so an existing target is a
        // collision under the default safety (the old bytes are kept) and the directory is refused under strict; a file
        // root follows the same rule. Compared with the same file as a one-file folder in a one-source `run::tree`.
        for weak in [
            FileIdentity::Weak(ObjectId { volume: 5, index: 9_000_100 }),
            FileIdentity::Unavailable,
        ] {
            for safety in [Safety::Default, Safety::Strict] {
                let o = CopyOptions { safety, ..opts() };
                let label = format!("{weak:?} under {safety:?}");
                let build = |dir: &str| {
                    let fs = fake();
                    fs.write_file("/src/f", b"NEW");
                    fs.write_file("/src/g", b"G");
                    fs.create_dir(Path::new("/src/t")).unwrap();
                    fs.write_file("/src/t/f", b"NEW");
                    fs.create_dir(Path::new(dir)).unwrap();
                    fs.write_file(format!("{dir}/f"), b"OLD");
                    fs.set_identity(dir, weak);
                    fs
                };
                let multi = build("/p/dest");
                let specs = vec![
                    spec("f"),
                    SourceSpec {
                        src: PathBuf::from("/src/g"),
                        name: OsString::from("g"),
                        kind: SourceKind::File,
                    },
                ];
                let (r, got) = go_with(&multi, specs, &o, &cfg());
                let tree = build("/p/out");
                let mut tree_got = Vec::new();
                let t = crate::run::tree(
                    &tree,
                    Path::new("/src/t"),
                    Path::new("/p/out"),
                    &o,
                    &cfg(),
                    &mut |f| tree_got.push(f),
                );

                assert_eq!(
                    multi.read_file("/p/dest/f").as_deref(),
                    Some(&b"OLD"[..]),
                    "{label}: the old bytes are kept"
                );
                assert_eq!(tree.read_file("/p/out/f").as_deref(), Some(&b"OLD"[..]), "{label}");
                if safety == Safety::Default {
                    assert!(r.run.stop.is_none() && t.stop.is_none(), "{label}");
                    assert_eq!(only_code(&got), Code::DestinationNamespaceCollision, "{label}");
                    assert_eq!(
                        only_code(&tree_got),
                        Code::DestinationNamespaceCollision,
                        "{label}"
                    );
                    assert_eq!(got[0].path, Path::new("f"), "{label}");
                    let Some(Ok(out)) = &r.run.copy else { panic!("{label}: {:?}", r.run.copy) };
                    let Some(Ok(tree_out)) = &t.copy else { panic!("{label}: {:?}", t.copy) };
                    assert_eq!(out.replace_degraded, tree_out.replace_degraded, "{label}");
                    assert!(out.replace_degraded.is_some(), "{label}");
                    assert_eq!(
                        statuses(&r),
                        [RootStatus::Failed, RootStatus::Completed],
                        "{label}"
                    );
                } else {
                    // The directory is refused whole, before anything is copied.
                    let code = |c: &Option<Result<TreeOutcome, TreeAbort>>| match c {
                        Some(Err(a)) => a.error.code(),
                        other => panic!("{label}: expected a refusal, got {other:?}"),
                    };
                    assert_eq!(code(&r.run.copy), Code::SafetyRejected, "{label}");
                    assert_eq!(code(&t.copy), Code::SafetyRejected, "{label}");
                    assert!(!multi.exists("/p/dest/g"), "{label}: nothing was copied");
                }
            }
        }
    }

    #[test]
    fn a_skip_never_reaches_the_identity_gate_for_a_file_root() {
        // Spec 3.6, the second accepted difference: `--skip-existing` skips before the identity gate in a tree, and
        // after it in `plan_file`. A source whose identity is Weak is refused by the gate under `--safety strict`
        // (the destination exists and the pair cannot be compared with confidence); the tree skips.
        let weak = FileIdentity::Weak(ObjectId { volume: 5, index: 9_000_101 });
        let o = CopyOptions {
            existing: ExistingPolicy::SkipExisting,
            safety: Safety::Strict,
            ..opts()
        };
        let build = |dir: &str| {
            let fs = fake();
            fs.create_dir(Path::new(dir)).unwrap();
            fs.write_file(format!("{dir}/f"), b"OLD");
            fs.set_identity("/src/f", weak);
            assert!(strong(&fs, &format!("{dir}/f")));
            fs
        };
        let multi = build("/p/dest");
        let (r, got) = go_with(&multi, vec![spec("b"), spec("f")], &o, &cfg());
        let single = build("/p/out");
        let one = crate::run::file(&single, Path::new("/src/f"), Path::new("/p/out/f"), &o, &cfg());

        let out = ok(&r);
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_skipped, out.files_copied), (1, 1), "f skipped, b/k copied");
        assert_eq!(multi.read_file("/p/dest/f").as_deref(), Some(&b"OLD"[..]));
        assert_eq!(statuses(&r), [RootStatus::Completed; 2]);
        // `file` reaches the gate first and refuses.
        match &one.copy {
            Some(Err(e)) => assert_eq!(e.code(), Code::SafetyRejected, "{e:?}"),
            other => panic!("expected the gate's refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_failing_file_in_root_one_does_not_stop_root_two() {
        let n = reference(fake, &["a", "b"], "create_new", "/p/dest/a/x.flux-partial");
        let fs = fake();
        fs.fail_nth("create_new", n, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let (r, got) = go(&fs, &["a", "b"]);

        let out = ok(&r);
        assert_eq!(statuses(&r), [RootStatus::Failed, RootStatus::Completed]);
        let per_root: Vec<_> = r.roots.iter().map(|x| (x.files_copied, x.files_failed)).collect();
        assert_eq!(per_root, [(1, 1), (1, 0)]);
        assert_eq!(out.failures.copy, 1);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new("a/x"), "reported under its root's name");
        assert_eq!(fs.read_file("/p/dest/b/k").as_deref(), Some(&b"bk"[..]));
    }

    #[test]
    fn a_lost_lock_between_roots_aborts_and_names_the_rest() {
        // The lock is lost at the last claim of root `a` (the second): the next thing the run does is root `b`'s
        // guard, before `DEST/b` is made.
        let n = reference(fake, &["a", "b", "c"], "claim_insert", "(x)");
        let fs = fake();
        lose_lock(&fs, "claim_insert", n);

        let (r, _) = go(&fs, &["a", "b", "c"]);

        assert!(r.run.stop.is_none(), "the copy's abort is the report: {:?}", r.run.stop);
        let Some(Err(a)) = &r.run.copy else { panic!("the copy aborted: {:?}", r.run.copy) };
        assert_eq!(a.error.code(), Code::TargetLockBusy);
        // The root the abort struck is `Failed` (the driver's rule, tree.rs `RootStatus::Failed`), though nothing of it
        // was made; the roots after it were never begun.
        assert_eq!(
            statuses(&r),
            [RootStatus::Completed, RootStatus::NotStarted, RootStatus::NotStarted]
        );
        assert_eq!(names(&r), os(&["a", "b", "c"]));
        assert_eq!((r.roots[1].files_copied, r.roots[1].files_failed), (0, 0));
        assert_eq!((r.roots[2].files_copied, r.roots[2].files_failed), (0, 0));
        assert!(!fs.exists("/p/dest/b") && !fs.exists("/p/dest/c"), "{:?}", calls(&fs));
        assert!(fs.exists("/p/dest/a/x") && fs.exists("/p/dest/a/sub/y"));
        assert_eq!(manifest(&fs).state, OpState::Transferring, "the workspace is kept as it is");
        assert_eq!(fs.read_file(LOCK).as_deref(), Some(THEIRS), "the lock is never unlinked");
        assert!(prefix_lock_files(&fs).is_empty(), "{:?}", prefix_lock_files(&fs));
    }

    #[test]
    fn prefix_lock_files_are_gone_after_success_failure_and_refusal() {
        // Success.
        let fs = fake();
        let (r, _) = go(&fs, &["a", "b", "c"]);
        ok(&r);
        assert!(lock_files(&fs).is_empty(), "success: {:?}", lock_files(&fs));

        // Failure: root `b` cannot be created, so the run ends FAILED and keeps its workspace (no `remove`).
        let n = reference(fake, &["a", "b", "c"], "create_dir", "create_dir(/p/dest/b)");
        let fs = fake();
        fs.fail_nth("create_dir", n, Code::IoError, std::io::ErrorKind::Other);
        let (r, _) = go(&fs, &["a", "b", "c"]);
        assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
        assert!(matches!(&r.run.copy, Some(Err(a)) if a.error.code() == Code::IoError));
        assert_eq!(
            statuses(&r),
            [RootStatus::Completed, RootStatus::Failed, RootStatus::NotStarted]
        );
        assert_eq!(manifest(&fs).state, OpState::Failed);
        assert_eq!(prefix_lock_files(&fs), Vec::<String>::new(), "failure");
        assert!(!fs.exists(LOCK), "the operation lock is released as well");

        // Refusal: a file where the folder root `b` lands, found with every prefix lock held.
        let fs = fake();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        fs.write_file("/p/dest/b", b"in the way");
        let (r, _) = go(&fs, &["a", "b", "c"]);
        match &r.run.stop {
            Some(RunError::Refused { refusal, changed: false, .. }) => {
                assert_eq!(refusal.code, LockCode::DestinationNamespaceCollision);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(r.run.copy.is_none() && r.roots.is_empty());
        assert!(lock_files(&fs).is_empty(), "refusal: {:?}", lock_files(&fs));
        assert!(!fs.exists("/p/dest/.flux"), "no workspace was made");
    }

    #[test]
    fn a_held_prefix_lock_on_the_last_root_refuses_and_nothing_is_copied() {
        let fs = fake();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        // Another operation holds the LAST root's lock in key order.
        let dest = fs.destination_root(Path::new("/p/dest")).unwrap();
        let other = PrefixIdOwned {
            operation_id: "55555555555555555555555555555555".to_string(),
            owner_instance_id: "other".to_string(),
            boot_session_id: "test-boot".to_string(),
            now: 5,
        };
        let (_held, _) =
            PrefixLocks::acquire(&dest, LockCapability::LocalStrong, &os(&["c"]), &other.as_ref())
                .unwrap();
        let mark = fs.calls().len();

        let (r, _) = go(&fs, &["c", "a", "b"]);

        match &r.run.stop {
            Some(RunError::Refused { refusal, changed: false, .. }) => {
                assert_eq!(refusal.code, LockCode::TargetLockBusy);
            }
            other => panic!("expected TARGET_LOCK_BUSY with nothing changed, got {other:?}"),
        }
        assert!(r.run.copy.is_none() && r.roots.is_empty());
        let c = calls(&fs)[mark..].to_vec();
        for name in ["a", "b", "c"] {
            assert!(touches(&c, name).is_empty(), "nothing is written into DEST/{name}: {c:?}");
        }
        assert!(
            !c.iter().any(|x| x.starts_with("create_dir(/p/dest/.flux")
                || x.starts_with("create_claim_store(")),
            "no workspace: {c:?}"
        );
        assert!(fs.exists("/p/dest/c.flux-lock"), "their lock is untouched");
        assert!(!fs.exists("/p/dest/a.flux-lock") && !fs.exists("/p/dest/b.flux-lock"));
        assert!(!fs.exists(LOCK), "the operation lock is released");
        assert!(fs.exists("/p/dest"), "a DEST that was there stays");
    }

    #[test]
    fn a_failure_opening_the_operation_after_the_prefix_locks_releases_them() {
        // The prefix locks are taken by `create`; the operation lock's own record is written after it. When that write
        // fails the opening fails with the prefix locks still held, and nothing else would release them.
        let fs = fake();
        // The records written at the start of a lock: three prefix locks, then the operation lock's.
        fs.fail_nth("write_at_start", 4, Code::IoError, std::io::ErrorKind::Other);

        let (r, _) = go(&fs, &["a", "b", "c"]);

        assert!(
            matches!(r.run.stop, Some(RunError::Failed { step: RunStep::Record, .. })),
            "{:?}",
            r.run.stop
        );
        assert!(r.run.copy.is_none() && r.roots.is_empty());
        assert!(prefix_lock_files(&fs).is_empty(), "{:?}", prefix_lock_files(&fs));
        assert!(!fs.exists("/p/dest/a"), "nothing was copied");
    }

    #[test]
    fn a_file_root_whose_name_is_not_its_source_name_is_refused() {
        let fs = fake();
        let renamed = SourceSpec {
            src: PathBuf::from("/src/f"),
            name: OsString::from("other"),
            kind: SourceKind::File,
        };

        let (r, _) = go_with(&fs, vec![spec("a"), renamed], &opts(), &cfg());

        match &r.run.stop {
            Some(RunError::Refused { refusal, changed: false, .. }) => {
                assert_eq!(refusal.code, LockCode::PathComponentInvalid);
                assert!(refusal.detail.contains("/src/f"), "{}", refusal.detail);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(r.run.copy.is_none() && r.roots.is_empty());
        assert!(!fs.exists("/p/dest") && !fs.called("create_lock"), "{:?}", calls(&fs));
    }

    #[test]
    fn a_folder_root_over_a_symlink_or_other_object_is_refused_before_transfer() {
        let fs = fake();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        fs.add_symlink_to("/p/dest/b", "/elsewhere", flux_fs::LinkKind::Unspecified);
        let mark = fs.calls().len();

        let (r, _) = go(&fs, &["a", "b"]);

        match &r.run.stop {
            Some(RunError::Refused { refusal, changed: false, .. }) => {
                assert_eq!(refusal.code, LockCode::DestinationNamespaceCollision);
                let shown = Path::new("/p/dest").join("b");
                assert!(
                    refusal.detail.contains(&shown.display().to_string()),
                    "{}",
                    refusal.detail
                );
            }
            other => panic!("expected a refusal before transfer, got {other:?}"),
        }
        assert!(r.run.copy.is_none() && r.roots.is_empty());
        let c = calls(&fs)[mark..].to_vec();
        let last_lock = c.iter().rposition(|x| x.starts_with("create_lock(")).expect("locks");
        assert!(
            !c[last_lock..].iter().any(|x| touches_dir(x, "a")
                || x.starts_with("create_dir(/p/dest/.flux")
                || x.starts_with("create_claim_store(")),
            "no write into DEST/a and no workspace after the last prefix lock: {c:?}"
        );
        assert!(!fs.exists("/p/dest/a") && !fs.exists("/p/dest/.flux"));
        assert!(lock_files(&fs).is_empty(), "{:?}", lock_files(&fs));
        assert!(fs.exists("/p/dest"), "a DEST that was there stays");
    }

    fn touches_dir(c: &str, name: &str) -> bool {
        c == format!("create_dir(/p/dest/{name})") || c.contains(&format!("/p/dest/{name}/"))
    }

    #[test]
    fn a_folder_root_whose_target_is_the_source_is_refused_safety_rejected() {
        let fs = fake();
        let same = FileIdentity::Strong(ObjectId { volume: 1, index: 9_000_300 });
        fs.set_identity("/src/b", same);
        fs.create_dir(Path::new("/p/dest")).unwrap();
        fs.create_dir(Path::new("/p/dest/b")).unwrap();
        fs.set_identity("/p/dest/b", same);

        let (r, _) = go(&fs, &["a", "b"]);

        match &r.run.stop {
            Some(RunError::Refused { refusal, changed: false, .. }) => {
                assert_eq!(refusal.code, LockCode::SafetyRejected);
            }
            other => panic!("expected a refusal before transfer, got {other:?}"),
        }
        assert!(r.run.copy.is_none() && r.roots.is_empty());
        assert!(!fs.exists("/p/dest/a") && !fs.exists("/p/dest/.flux"), "{:?}", calls(&fs));
        assert!(lock_files(&fs).is_empty(), "{:?}", lock_files(&fs));
    }

    #[test]
    fn counters_are_sums_over_roots_and_a_file_root_counts_as_one_file() {
        let n = reference(fake, &["a", "f"], "create_new", "/p/dest/a/x.flux-partial");
        let fs = fake();
        fs.fail_nth("create_new", n, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let (r, got) = go(&fs, &["a", "f"]);

        let out = ok(&r);
        assert_eq!(statuses(&r), [RootStatus::Failed, RootStatus::Completed]);
        // `a`: x fails, sub/y copies (2 bytes); `f`: one file of one byte.
        assert_eq!(out.files_total, 3);
        assert_eq!(out.files_copied, 2);
        assert_eq!(out.bytes_copied, 3);
        // DEST, `DEST/a` and `DEST/a/sub`.
        assert_eq!(out.directories_created, 3);
        assert_eq!((out.failures.copy, out.failures.total()), (1, 1));
        assert_eq!(got.len(), 1, "{got:?}");
        let copied: u64 = r.roots.iter().map(|x| x.files_copied).sum();
        let failed: u64 = r.roots.iter().map(|x| x.files_failed).sum();
        assert_eq!((copied, failed), (out.files_copied, out.failures.copy));
        assert_eq!(copied + failed, out.files_total, "every file is one or the other");
        assert_eq!(r.roots[1].files_copied, 1, "a file root counts as one file");
    }

    #[test]
    fn nested_roots_copy_twice() {
        let fs = fake();
        let nested = SourceSpec {
            src: PathBuf::from("/src/a/sub"),
            name: OsString::from("sub"),
            kind: SourceKind::Folder,
        };
        // After the first file is published the workspace and its manifest are there.
        let seen = watch_manifest(&fs, "claim_insert", 1);

        let (r, got) = go_with(&fs, vec![spec("a"), nested], &opts(), &cfg());

        let out = ok(&r);
        assert!(got.is_empty(), "nothing is refused: {got:?}");
        assert_eq!(fs.read_file("/p/dest/a/sub/y").as_deref(), Some(&b"ay"[..]));
        assert_eq!(fs.read_file("/p/dest/sub/y").as_deref(), Some(&b"ay"[..]));
        assert_eq!(fs.read_file("/p/dest/a/x").as_deref(), Some(&b"ax"[..]));
        assert_eq!(out.files_copied, 3, "y is copied twice");
        assert_eq!(names(&r), os(&["a", "sub"]));
        assert_eq!(statuses(&r), [RootStatus::Completed; 2]);
        let state = seen.lock().unwrap().take().expect("the manifest was read in flight");
        let config = state.config.expect("a version-5 record");
        let prefixes: Vec<_> =
            config.roots.iter().map(|r| from_native_hex(&r.destination_prefix).unwrap()).collect();
        assert_eq!(
            prefixes,
            [PathBuf::from("a"), PathBuf::from("sub")],
            "two roots in prefix order"
        );
    }

    #[test]
    fn source_containing_dot_flux_operations_is_refused_as_for_one_source() {
        // Spec 3.6: `reserved_path` runs on the walk-relative path, so a source holding `.flux/operations/...` has that
        // entry refused exactly as a one-source run refuses it.
        let build = || {
            let fs = fake();
            for d in ["/src/a/.flux", "/src/a/.flux/operations"] {
                fs.create_dir(Path::new(d)).unwrap();
            }
            fs.write_file("/src/a/.flux/operations/foo", b"mine");
            fs
        };
        let single = build();
        let mut one = Vec::new();
        let s = crate::run::tree(
            &single,
            Path::new("/src/a"),
            Path::new("/p/dest"),
            &opts(),
            &cfg(),
            &mut |f| one.push(f),
        );
        assert!(s.stop.is_none(), "{:?}", s.stop);
        let single_code = only_code(&one);
        assert_eq!(single_code, Code::ControlPlaneNamespaceConflict);

        let fs = build();
        let (r, got) = go(&fs, &["a", "b"]);

        ok(&r);
        assert_eq!(only_code(&got), single_code);
        assert_eq!(
            got[0].path,
            Path::new("a").join(&one[0].path),
            "the same entry, under its root"
        );
        assert!(!fs.exists("/p/dest/a/.flux/operations/foo"), "the entry was not copied");
        assert_eq!(statuses(&r), [RootStatus::Failed, RootStatus::Completed]);
        assert_eq!(fs.read_file("/p/dest/a/x").as_deref(), Some(&b"ax"[..]), "the rest continued");
    }

    /// Whether a path component is one Flux keeps for itself: `.flux`, a prefix or operation lock, a state file.
    fn is_control(component: &str) -> bool {
        component == ".flux"
            || component.ends_with(".flux-lock")
            || component.contains(".flux-state.")
    }

    #[test]
    fn the_workspace_is_under_dest_and_nothing_control_is_inside_a_root() {
        let fs = fake();
        let seen = watch_manifest(&fs, "claim_insert", 1);
        let in_flight = Arc::new(Mutex::new(Vec::new()));
        let keep = Arc::clone(&in_flight);
        // Taken at the same moment as the manifest: the workspace and the prefix locks exist.
        fs.on_nth("claim_flush", 1, move |fs| {
            let mut found = keep.lock().unwrap();
            found.push(fs.exists(format!("/p/dest/.flux/operations/{ID}")));
            found.push(["a", "b", "f"].iter().all(|n| fs.exists(format!("/p/dest/{n}.flux-lock"))));
        });

        let (r, _) = go(&fs, &["f", "b", "a"]);

        ok(&r);
        // Every path the run named below a root has no control component.
        for c in calls(&fs) {
            for token in c.split(['(', ')', ' ', '>']).filter(|t| t.starts_with("/p/dest/")) {
                for name in ["a", "b", "f"] {
                    if let Some(below) = token.strip_prefix(&format!("/p/dest/{name}/")) {
                        assert!(
                            !below.split('/').any(is_control),
                            "control state inside DEST/{name}: {c}"
                        );
                    }
                }
            }
        }
        assert_eq!(*in_flight.lock().unwrap(), [true, true], "workspace and locks, in flight");
        let state = seen.lock().unwrap().take().expect("the manifest was read in flight");
        assert_eq!(state.format_version, 5);
        assert_eq!(state.kind, Kind::Tree);
        let config = state.config.expect("a version-5 record");
        let roots: Vec<_> = config
            .roots
            .iter()
            .map(|r| (from_native_hex(&r.destination_prefix).unwrap(), r.kind))
            .collect();
        assert_eq!(
            roots,
            [
                (PathBuf::from("a"), Some(RootKind::Tree)),
                (PathBuf::from("b"), Some(RootKind::Tree)),
                (PathBuf::from("f"), Some(RootKind::File)),
            ],
            "the roots in key order whatever the order given"
        );
        // Nothing of the run's own is left anywhere afterwards.
        assert!(!fs.exists("/p/dest/.flux") && lock_files(&fs).is_empty());
    }

    #[test]
    fn a_one_source_restart_supersedes_an_aborted_several_source_run() {
        // Root `b` cannot be created, so the several-source run ends FAILED and keeps its version-5 workspace.
        let n = reference(fake, &["a", "b", "c"], "create_dir", "create_dir(/p/dest/b)");
        let fs = fake();
        fs.fail_nth("create_dir", n, Code::IoError, std::io::ErrorKind::Other);
        let (r, _) = go(&fs, &["a", "b", "c"]);
        assert!(matches!(&r.run.copy, Some(Err(a)) if a.error.code() == Code::IoError));
        let prior = manifest(&fs);
        assert_eq!(prior.state, OpState::Failed);
        assert_eq!(prior.format_version, crate::state::MULTI_ROOT_FORMAT);

        // Without --restart the one-source run is refused, and the prior stays as it is.
        let second = RunConfig { operation_id: format!("{:032x}", 0x22), ..cfg() };
        let refused = crate::run::tree(
            &fs,
            Path::new("/src/a"),
            Path::new("/p/dest"),
            &opts(),
            &second,
            &mut |_| {},
        );
        match &refused.stop {
            Some(RunError::Refused { refusal, .. }) => {
                assert_eq!(refusal.code, LockCode::ResumableOperationExists);
            }
            other => panic!("expected the prior to refuse the run, got {other:?}"),
        }
        assert_eq!(manifest(&fs).state, OpState::Failed);

        // With --restart the prior is ABANDONED and the copy succeeds.
        let restart = RunConfig { restart: true, ..second };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let r = crate::run::tree(
            &fs,
            Path::new("/src/a"),
            Path::new("/p/dest"),
            &opts(),
            &restart,
            &mut |f| sink.lock().unwrap().push(f),
        );
        assert!(r.stop.is_none(), "{:?}", r.stop);
        match &r.copy {
            Some(Ok(_)) => {}
            other => panic!("the copy should succeed, got {other:?}"),
        }
        assert!(seen.lock().unwrap().is_empty(), "{:?}", seen.lock().unwrap());
        assert_eq!(fs.read_file("/p/dest/x").as_deref(), Some(&b"ax"[..]));
        assert_eq!(fs.read_file("/p/dest/sub/y").as_deref(), Some(&b"ay"[..]));
        assert!(!fs.exists(format!("/p/dest/.flux/operations/{ID}")), "the old workspace is gone");
        assert!(lock_files(&fs).is_empty(), "{:?}", lock_files(&fs));
    }

    #[test]
    fn stale_prefix_locks_of_a_crashed_run_are_recovered_by_the_next_trees_run() {
        let fs = fake();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        // A crashed run: its prefix locks are on disk with a dead owner (`Held` has no `Drop`, so dropping the set
        // without releasing it is the crash), and so is the operation lock's file.
        let dest = fs.destination_root(Path::new("/p/dest")).unwrap();
        let crashed = PrefixIdOwned {
            operation_id: "55555555555555555555555555555555".to_string(),
            owner_instance_id: "dead".to_string(),
            boot_session_id: "old-boot".to_string(),
            now: 5,
        };
        let (held, _) = PrefixLocks::acquire(
            &dest,
            LockCapability::LocalStrong,
            &os(&["a", "b"]),
            &crashed.as_ref(),
        )
        .unwrap();
        drop(held);
        assert_eq!(prefix_lock_files(&fs).len(), 2, "the stale locks are on disk");

        let (r, got) = go(&fs, &["a", "b"]);

        ok(&r);
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(statuses(&r), [RootStatus::Completed; 2]);
        assert_eq!(fs.read_file("/p/dest/a/x").as_deref(), Some(&b"ax"[..]));
        assert_eq!(fs.read_file("/p/dest/b/k").as_deref(), Some(&b"bk"[..]));
        assert!(lock_files(&fs).is_empty(), "{:?}", lock_files(&fs));
    }
}
