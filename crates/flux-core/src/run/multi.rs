// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The place of a several-source run (spec 3.4, 3.5): a `TreePlace` for DEST plus one prefix lock and one root per
//! source. Its `create` makes DEST, takes the prefix locks inside it, checks the names the sources take, and only then
//! makes the workspace, so a refusal at any of those finds no workspace and releases every lock it took.

use super::dry_prior::{LockPreview, broken_leftovers, dry_lock_record, dry_restart};
use super::place::{AdoptedStore, LocatedTree, Place, TreePlace, locate_dest, refused_state};
use super::prefix::{PrefixId, PrefixIdOwned, PrefixLocks, PrefixRefusal};
use super::roots::{SourceKind, SourceSpec, plan_roots};
use super::session::{Fault, Locked, failed, from_lock, lock_io};
use super::{
    DryStart, Run, RunConfig, RunError, RunStep, RunWarning, TreeAt, batch_policy,
    checkpoint_policy, drive, dry_hardlink_slot, dry_start,
};
use crate::copy::{CopyError, CopyStep, no_heartbeat, unguarded};
use crate::dry_run::{DryPrior, DryRun, NullClaimStore};
use crate::lock::{LockCode, LockError, LockResult, LockSite, Refusal, check_capability};
use crate::prior::{PriorOp, Scan, check_control_plane};
use crate::state::{
    Kind, OperationState, Root, RootKind, absolute_lexical, identity_text, native_hex, wall_time_ns,
};
use crate::tree::{
    RootDone, RootInput, RootStatus, RootsDest, Shared, TreeAbort, TreeFailure, TreeOutcome,
    containment, copy_roots_at, preflight, prepare_source,
};
use crate::walk::MountPolicy;
use flux_fs::{
    Code, CopyOptions, DestinationRoot, DirEntry, DirHandle, Durability, FileIdentity, FileType,
    FsError, LockCapability, OperationId,
};
use std::cell::{OnceCell, RefCell};
use std::collections::{BinaryHeap, HashMap, HashSet};
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

/// The most entries the fold-probe asks about: on a filesystem with `Weak` identities every candidate would otherwise
/// cost two `metadata` calls.
const FOLD_CANDIDATES: usize = 8;

/// An entry the fold-probe can use: a valid UTF-8 name holding an ASCII letter.
fn fold_candidate(entry: &DirEntry) -> Option<&str> {
    let name = entry.name.to_str()?;
    name.bytes().any(|b| b.is_ascii_alphabetic()).then_some(name)
}

/// `name` with the case of its first ASCII letter swapped.
fn swap_first_letter(name: &str) -> String {
    let mut done = false;
    name.chars()
        .map(|c| {
            if !done && c.is_ascii_alphabetic() {
                done = true;
                if c.is_ascii_uppercase() { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() }
            } else {
                c
            }
        })
        .collect()
}

/// Spec 4.2 step 2 (vi): whether `dir`, whose listing is `entries`, folds ASCII case. Read-only. It asks about at most
/// `FOLD_CANDIDATES` entries, the smallest usable names bytewise (a listing's order is arbitrary, so it must not decide
/// which): the variant swaps the case of the first ASCII letter. A variant that is itself an entry means two names
/// differing by case coexist (`Some(false)`); both names resolving to one `Strong` identity is `Some(true)`; a variant
/// that is not found is `Some(false)`; anything else (an error, a `Weak` or `Unavailable` identity) tries the next
/// candidate, and with none left the answer is `None`: undecided, never a refusal.
fn folds_ascii_case<D: DirHandle>(dir: &D, entries: &[DirEntry]) -> Option<bool> {
    // A bounded max-heap keeps the smallest names without sorting the listing.
    let mut heap: BinaryHeap<&str> = BinaryHeap::with_capacity(FOLD_CANDIDATES + 1);
    for name in entries.iter().filter_map(fold_candidate) {
        heap.push(name);
        if heap.len() > FOLD_CANDIDATES {
            heap.pop();
        }
    }
    let present: HashSet<&OsStr> = entries.iter().map(|e| e.name.as_os_str()).collect();
    for name in heap.into_sorted_vec() {
        let variant = swap_first_letter(name);
        if present.contains(OsStr::new(&variant)) {
            return Some(false);
        }
        let Ok(entry) = dir.metadata(OsStr::new(name)) else { continue };
        let FileIdentity::Strong(id) = entry.identity else { continue };
        match dir.metadata(OsStr::new(&variant)) {
            Ok(found) => {
                if matches!(found.identity, FileIdentity::Strong(other) if other == id) {
                    return Some(true);
                }
            }
            Err(e) if e.source.kind() == ErrorKind::NotFound => return Some(false),
            Err(_) => {}
        }
    }
    None
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
    /// A `RefCell` so that `Place::remove`, which takes `&self`, can release the locks (the trait does not change). Released for this run's own id only.
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
            PrefixRefusal::Lock { name, mut error } => {
                if let LockError::Refused(r) = &mut error {
                    match r.code {
                        LockCode::TargetLockUncertain => {
                            r.detail = "this prefix lock has an unreadable record; if no Flux run is active on this destination, remove that file and run again with the same flags".to_string();
                        }
                        LockCode::ArtifactOwnershipUncertain => {
                            r.detail = crate::lock::UNTRUSTED_DETAIL.to_string();
                        }
                        _ => {}
                    }
                }
                from_lock(error, RunStep::Lock, &self.lock_path(&name), false)
            }
        }
    }

    /// The dry run's preview of what `create` does between making DEST and making the workspace (spec 4.2 step 2): every
    /// prefix lock's file read in prefix order, then the targets checked, so a refusal has the order of the real run's.
    /// With DEST absent only the fold-probe runs, on DEST's parent. The notes (the aggregate of the readable prefix locks, the `.broken.` leftovers) and
    /// `owner_liveness` are set only once every lock has passed, and are kept when the target check then refuses, as the
    /// real run's leftovers are pushed before its `check_targets`.
    pub(crate) fn dry_create_preview(
        &self,
        dry: &mut DryRun,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<(), RunError> {
        let twins = self.case_twins();
        let Some(dest) = self.tree.dest.as_ref() else {
            // DEST is absent: no lock file or target to read, only the fold-probe, on DEST's parent.
            if let Some((i, Some(t))) = twins.iter().copied().enumerate().find(|(_, t)| t.is_some())
                && self.dry_fold(None) == Some(true)
            {
                return Err(self.twin_collision(i, t));
            }
            return Ok(());
        };
        // One listing for every leftover scan; the scan is best-effort, as `dry_lock`'s.
        let listing = dest.read_dir().ok();
        // The fold-probe's answer, asked for at the first name with an ASCII-case twin before it.
        let mut folds: Option<Option<bool>> = None;
        let now = wall_time_ns();
        let mut seen: Vec<(FileIdentity, &OsString)> = Vec::new();
        let mut recorded: Vec<String> = Vec::new();
        for (i, root) in self.roots.iter().enumerate() {
            let name = &root.name;
            let site = LockSite::directory(dest, name).map_err(|error| {
                self.prefix_error(PrefixRefusal::Lock { name: name.clone(), error })
            })?;
            let lock_found = match dest.metadata(site.lock_name()) {
                Ok(found) => {
                    // The lock files of two names are one file where the filesystem folds the names together; the
                    // targets are not compared (two hardlinked targets are a legitimate copy).
                    if let FileIdentity::Strong(id) = found.identity {
                        if let Some((_, other)) = seen
                            .iter()
                            .find(|(s, _)| matches!(s, FileIdentity::Strong(o) if *o == id))
                        {
                            return Err(self.prefix_error(PrefixRefusal::Collision {
                                name: name.clone(),
                                other: (*other).clone(),
                            }));
                        }
                        seen.push((found.identity, name));
                    }
                    true
                }
                Err(_) => false,
            };
            // A name whose twin is before it is the same lock file on a directory that folds ASCII case, as the real
            // `take` meets the first lock's file before it classifies anything.
            if let Some(t) = twins[i]
                && *folds.get_or_insert_with(|| self.dry_fold(listing.as_deref())) == Some(true)
            {
                return Err(self.twin_collision(i, t));
            }
            // `acquire` looks for `.flux-dir.lock` only after it created this name's lock file; an existing file goes
            // straight to its classification.
            if !lock_found {
                let present = site.dir_lock_present().map_err(|error| {
                    self.prefix_error(PrefixRefusal::Lock { name: name.clone(), error })
                })?;
                if present {
                    let busy = LockError::Refused(Box::new(Refusal {
                        code: LockCode::TargetLockBusy,
                        holder: None,
                        // The text of `lock/acquire.rs`'s `S96_1_dircheck`.
                        detail:
                            "a directory lock (.flux-dir.lock) is present beside the target's lock"
                                .to_string(),
                    }));
                    return Err(
                        self.prefix_error(PrefixRefusal::Lock { name: name.clone(), error: busy })
                    );
                }
            }
            // A scratch for the record note: one aggregate replaces the per-lock notes.
            let mut scratch = Vec::new();
            match dry_lock_record(&site, false, self.capability, now, &mut scratch).0 {
                LockPreview::Refused(error) => {
                    return Err(
                        self.prefix_error(PrefixRefusal::Lock { name: name.clone(), error })
                    );
                }
                LockPreview::Error(e) => {
                    return Err(self.prefix_error(PrefixRefusal::Lock {
                        name: name.clone(),
                        error: LockError::Io(e),
                    }));
                }
                LockPreview::Recorded(_) => recorded.push(name.to_string_lossy().into_owned()),
                // `break_lock` is false, so a takeover is not offered.
                LockPreview::Absent | LockPreview::Takeover(_) => {}
            }
        }
        if !recorded.is_empty() {
            warnings.push(RunWarning::DryRunPrefixLocks { names: recorded });
            dry.owner_liveness = true;
        }
        if let Some(entries) = &listing {
            for root in &self.roots {
                let mut lock_name = root.name.clone();
                lock_name.push(".flux-lock");
                broken_leftovers(entries, &lock_name, warnings);
            }
        }
        self.check_targets_in(dest)
    }

    /// For each root, the index of the first root before it whose name is equal to it after ASCII lower-casing (the
    /// names are unique, so they differ in bytes): the trigger of the fold-probe.
    fn case_twins(&self) -> Vec<Option<usize>> {
        let mut first: HashMap<Vec<u8>, usize> = HashMap::new();
        self.roots
            .iter()
            .enumerate()
            .map(|(i, root)| {
                let key = root.name.as_encoded_bytes().to_ascii_lowercase();
                let twin = first.get(&key).copied();
                first.entry(key).or_insert(i);
                twin
            })
            .collect()
    }

    /// The refusal for root `i` and its earlier twin `twin` (the real run's, `prefix_error`).
    fn twin_collision(&self, i: usize, twin: usize) -> RunError {
        let other = &self.roots[twin].name;
        self.prefix_error(PrefixRefusal::Collision {
            name: self.roots[i].name.clone(),
            other: other.clone(),
        })
    }

    /// Spec 4.2 step 2 (vi): whether the directory the lock files live in folds ASCII case. From DEST's own listing; with
    /// DEST absent, from its parent's, read once; with DEST present but nothing in its listing to probe with (empty,
    /// unusable or failed), from its parent only if both are `Strong` on one volume: an existing DEST can be a mount
    /// point or carry its own case flag, and a decided answer from the wrong directory is a favourable guess.
    fn dry_fold(&self, listing: Option<&[DirEntry]>) -> Option<bool> {
        let holder = self.tree.holder;
        let Some(dest) = self.tree.dest.as_ref() else {
            return folds_ascii_case(holder, &holder.read_dir().ok()?);
        };
        if let Some(entries) = listing
            && entries.iter().any(|e| fold_candidate(e).is_some())
        {
            return folds_ascii_case(dest, entries);
        }
        // A filesystem root is its own holder: no parent.
        self.tree.name.as_ref()?;
        match (dest.identity().ok()?, holder.identity().ok()?) {
            (FileIdentity::Strong(d), FileIdentity::Strong(p)) if d.volume == p.volume => {
                folds_ascii_case(holder, &holder.read_dir().ok()?)
            }
            _ => None,
        }
    }

    /// Group 2: what is at `DEST/<name>` now. An object of the other kind than the root is a collision; a file root
    /// whose target is the source itself is unsafe. An object of the right kind is the engine's per-file decision.
    fn check_targets(&self) -> Result<(), RunError> {
        self.check_targets_in(self.tree.dest.as_ref().expect("create_dest made it"))
    }

    /// `check_targets` against `dest`: the directory `create_dest` made, or (a dry run) the one that exists.
    fn check_targets_in(&self, dest: &F::Dir) -> Result<(), RunError> {
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
        // A prior's workspace (a supersede) is retired without touching this run's locks.
        if id != self.prefix_id.operation_id {
            return self.tree.remove(id);
        }
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

/// Several sources under the destination's lock (spec 3.3 to 3.8): `specs` is two or more sources; a dry run (`cfg.dry_run`)
/// branches to `dry_trees` after Group 1. `tree_with`'s run, with the roots' prefix locks taken inside DEST before the workspace exists and
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
    // Adoption (`--resume`) is the unsupported case: `MultiPlace::remove` releases the prefix locks only for this
    // run's own id, so a supersede (`--restart`) removing a prior's workspace leaves them held.
    debug_assert!(!cfg.resume, "several sources do not support --resume");
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
                    Ok(mut source) => {
                        source.events.set_selection(&opts.selection);
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
    if cfg.dry_run {
        return dry_trees(fs, run, out, place, &site, anchor, cfg, &opts, inputs, on_report);
    }
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
            let (roots, walked) = copy_roots_at(
                cx,
                RootsDest::Existing(dest),
                place.tree.created_dest,
                inputs,
                out,
                report,
            );
            done = roots;
            walked
        },
        // Every exit releases the prefix locks: a `Held` has no `Drop`.
        |place, warnings| {
            let failed = place.release_prefix_locks();
            place.warn_release(failed, warnings);
        },
    );
    let roots = root_reports(&place.roots, done, run.copy.is_some());
    MultiRun { run, roots }
}

/// One report per root, in prefix order. `done` is empty while the copy ran when it ended before any root was begun
/// (the checkpoint log could not be opened): every root is then unstarted.
fn root_reports(plans: &[RootPlan], done: Vec<RootDone>, copy_ran: bool) -> Vec<RootReport> {
    if done.is_empty() && copy_ran {
        plans
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
    }
}

/// The dry branch of `trees` (spec 4.2): the fresh plan. The lock preview and the scan are `dry_start`'s, then every
/// root is planned in prefix order against what DEST holds, over a claim store that stores nothing and one hardlink
/// slot all the roots share. DEST is never made: an absent one is planned, and counted after the walk unless it
/// refused with nothing changed (I7), as `dry_tree` counts it. `out` is the outcome `trees` has already filled with the
/// pre-flight and containment warnings.
// One input per piece of the run, as `dry_tree`'s.
#[allow(clippy::too_many_arguments)]
fn dry_trees<F: DestinationRoot>(
    fs: &F,
    mut run: Run<Result<TreeOutcome, TreeAbort>>,
    mut out: TreeOutcome,
    place: MultiPlace<'_, F>,
    site: &LockSite<'_, F::Dir>,
    anchor: FileIdentity,
    cfg: &RunConfig,
    opts: &CopyOptions,
    inputs: Vec<RootInput<'_, F>>,
    on_report: &mut dyn FnMut(TreeFailure),
) -> MultiRun {
    let (dest_shown, operations_shown, absent) =
        (place.tree.dest_shown.clone(), place.tree.operations_shown(), place.tree.dest.is_none());
    let mut restart_preview = |priors: &[PriorOp], warnings: &mut Vec<RunWarning>| {
        if absent {
            DryPrior::Restart { superseded: Vec::new(), partials_found: Vec::new(), states: 0 }
        } else {
            dry_restart(fs, &dest_shown, &operations_shown, priors, warnings, &mut *on_report)
        }
    };
    // Several sources do not resume: the refusal `MultiPlace::adopt` gives, were a prior resumable.
    let mut resume_preview = |_: &PriorOp, _: &mut Vec<RunWarning>| {
        Err(refused_state(
            LockCode::IncompatibleState,
            "resuming several sources is not supported in this version".to_string(),
        ))
    };
    let mut dry = match dry_start(
        site,
        place.capability,
        &place,
        cfg,
        &mut restart_preview,
        &mut resume_preview,
        &mut |dry, warnings| place.dry_create_preview(dry, warnings),
        &mut run.warnings,
    ) {
        DryStart::Plan(dry) => dry,
        DryStart::Stop(stop, dry) => {
            run.stop = Some(stop);
            run.dry = Some(dry);
            return ended_early(run);
        }
    };
    let temp_root = cfg.dry_temp_root.clone().unwrap_or_else(std::env::temp_dir);
    let (hardlink_slot, hardlinks_path) = dry_hardlink_slot(fs, &temp_root, cfg, opts);
    let null = RefCell::new(NullClaimStore);
    let partials = std::cell::Cell::new(0u64);
    let src_root = place.tree.src_root.clone();
    let cx = Shared {
        fs,
        // Read only through `for_root`, which gives each root its own.
        src_root: &src_root,
        src_identity: place.tree.src_identity,
        opts,
        guard: &unguarded,
        beat: &no_heartbeat,
        claims: Some(&null),
        resume: false,
        batch: batch_policy(cfg),
        ckpt: None,
        dry_run: true,
        dry_prior: None,
        dry_partials: Some(&partials),
        key_prefix: None,
        hardlinks: hardlink_slot.as_ref(),
        anchor_root: None,
    };
    let dest = match &place.tree.dest {
        Some(d) => RootsDest::Existing(d),
        None => RootsDest::Planned(anchor),
    };
    let (done, walked) = copy_roots_at(&cx, dest, false, inputs, &mut out, on_report);
    // Cut 12: the index file and its private directory go with the slot.
    if let Some(slot) = hardlink_slot {
        let failure = slot.failure();
        drop(slot.close(true));
        if let Some(code) = failure {
            run.warnings.push(RunWarning::HardlinkIndexUnavailable { path: hardlinks_path, code });
        }
    }
    let mut result = match walked {
        Ok(()) => Ok(out),
        Err(error) => Err(TreeAbort { error, outcome: out }),
    };
    // I7: DEST's creation is counted after the walk, unless the walk refused with nothing changed.
    if absent && !matches!(&result, Err(a) if a.refused_unchanged()) {
        match &mut result {
            Ok(o) => o.directories_created += 1,
            Err(a) => a.outcome.directories_created += 1,
        }
    }
    dry.unprobed_noreplace = true;
    run.dry = Some(dry);
    run.copy = Some(result);
    let roots = root_reports(&place.roots, done, true);
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
    fn remove_of_a_prior_keeps_this_runs_prefix_locks() {
        let fx = fx();
        let mut p = place(&fx, vec![root("data", RootKind::Tree)]);
        let mut warnings = Vec::new();
        p.create(&state(), &mut warnings).unwrap();
        let prior = "22222222222222222222222222222222";
        fx.fs.create_dir(Path::new(&format!("/p/dest/.flux/operations/{prior}"))).unwrap();
        p.remove(prior).unwrap();
        assert!(
            !fx.fs.exists(format!("/p/dest/.flux/operations/{prior}")),
            "the prior's workspace is retired"
        );
        assert_eq!(
            lock_files(&fx.fs),
            vec!["/p/dest/data.flux-lock".to_string()],
            "this run's prefix lock is still held"
        );
        assert!(p.prefix.borrow().is_some());
    }

    #[test]
    fn remove_of_this_run_releases_its_prefix_locks() {
        let fx = fx();
        let mut p = place(&fx, vec![root("data", RootKind::Tree)]);
        let mut warnings = Vec::new();
        p.create(&state(), &mut warnings).unwrap();
        p.remove(ID).unwrap();
        assert!(lock_files(&fx.fs).is_empty(), "{:?}", lock_files(&fx.fs));
        assert!(p.prefix.borrow().is_none());
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
    use crate::dry_run::{PlannedAction, PlannedObject};
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
            hardlinks: flux_fs::Hardlinks::Auto,
            cross_filesystems: false,
            verify: flux_fs::VerifyLevel::None,
            hash: flux_fs::HashAlgorithm::Blake3,
            selection: flux_fs::Selection::default(),
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
    fn a_group_across_two_roots_links_in_one_operation() {
        let fs = FaultFs::new();
        for d in ["/s1", "/s2", "/p"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/s1/a", b"shared");
        let (d1, d2) = (
            fs.destination_root(Path::new("/s1")).unwrap(),
            fs.destination_root(Path::new("/s2")).unwrap(),
        );
        d2.create_hardlink(OsStr::new("a"), &d1, OsStr::new("a")).unwrap();
        let specs = ["s1", "s2"]
            .map(|n| SourceSpec {
                src: Path::new("/").join(n),
                name: OsString::from(n),
                kind: SourceKind::Folder,
            })
            .to_vec();
        let (r, got) = go_with(&fs, specs, &opts(), &cfg());
        let out = ok(&r);
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_copied, out.files_hardlinked), (1, 1));
        let id = |p: &str| match fs.metadata(Path::new(p)).unwrap().identity {
            FileIdentity::Strong(id) => id,
            other => panic!("{p}: {other:?}"),
        };
        assert_eq!(id("/p/dest/s1/a"), id("/p/dest/s2/a"));
        assert_eq!(fs.read_file("/p/dest/s2/a").as_deref(), Some(&b"shared"[..]));
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

    fn lock_prefix(name: &str) -> String {
        format!("{}: ", Path::new("/p/dest").join(format!("{name}.flux-lock")).display())
    }

    #[test]
    fn an_unreadable_prefix_lock_names_the_file_once_and_promises_no_break_lock() {
        let fs = fake();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        fs.write_file("/p/dest/a.flux-lock", b""); // an empty record, nobody holds it: Uncertain under Mode::Plain
        let (r, _) = go(&fs, &["a", "b"]);
        let detail = match &r.run.stop {
            Some(RunError::Refused { refusal, changed: false, .. }) => {
                assert_eq!(refusal.code, LockCode::TargetLockUncertain);
                refusal.detail.clone()
            }
            other => panic!("expected TARGET_LOCK_UNCERTAIN, got {other:?}"),
        };
        assert!(detail.starts_with(&lock_prefix("a")), "{detail}");
        assert_eq!(detail.matches("a.flux-lock").count(), 1, "{detail}");
        assert!(detail.contains("the same flags") && !detail.contains("--break-lock"), "{detail}");
        assert!(!fs.exists("/p/dest/.flux"), "nothing was created");
    }

    #[test]
    fn an_orphan_prefix_lock_says_remove_by_hand_not_cleanup() {
        let fs = fake();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        // A one-source copy into DEST/a crashed (its record names operations/<id> under DEST/a), then DEST/a was removed.
        let dest = fs.destination_root(Path::new("/p/dest")).unwrap();
        let site = crate::lock::LockSite::directory(&dest, &OsString::from("a")).unwrap();
        let rec = crate::lock::test_support::record(
            &site,
            "33333333333333333333333333333333",
            "operations/33333333333333333333333333333333",
        );
        crate::lock::test_support::dead_lock(&dest, "a.flux-lock", &rec.encode());
        let (r, _) = go(&fs, &["a", "b"]);
        let detail = match &r.run.stop {
            Some(RunError::Refused { refusal, .. }) => {
                assert_eq!(refusal.code, LockCode::ArtifactOwnershipUncertain);
                refusal.detail.clone()
            }
            other => panic!("expected ARTIFACT_OWNERSHIP_UNCERTAIN, got {other:?}"),
        };
        assert!(detail.starts_with(&lock_prefix("a")), "{detail}");
        assert!(
            detail.contains("remove it by hand") && !detail.contains("flux cleanup"),
            "{detail}"
        );
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
                assert!(refusal.detail.starts_with(&lock_prefix("c")), "{}", refusal.detail);
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

    // ---- Cut 16 Task 3b: the selection applies to each folder root and the count sums over the roots ----

    fn selecting() -> CopyOptions {
        let typed = vec!["*.tmp".to_string()];
        CopyOptions { selection: flux_fs::Selection::compile(&typed, true).unwrap(), ..opts() }
    }

    fn with_tmp_files() -> FaultFs {
        let fs = fake();
        for p in [
            "/src/a/t1.tmp",
            "/src/a/sub/t2.tmp",
            "/src/b/u1.tmp",
            "/src/b/u2.tmp",
            "/src/b/u3.tmp",
        ] {
            fs.write_file(p, b"T");
        }
        fs
    }

    #[test]
    fn the_selection_applies_to_each_root_and_the_counts_sum() {
        let fs = with_tmp_files();
        let specs = ["a", "b"].map(spec).to_vec();
        let (r, got) = go_with(&fs, specs, &selecting(), &cfg());
        let out = ok(&r);
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(out.excluded, 5, "2 under a and 3 under b");
        assert_eq!(out.files_total, 3, "x, y, k");
        assert!(!fs.exists("/p/dest/a/t1.tmp") && !fs.exists("/p/dest/b/u3.tmp"));
        assert!(fs.exists("/p/dest/a/x") && fs.exists("/p/dest/b/k"));
    }

    #[test]
    fn only_the_roots_that_exclude_contribute() {
        // `a` excludes two, `b` none, `c` none.
        let fs = fake();
        fs.write_file("/src/a/t1.tmp", b"T");
        fs.write_file("/src/a/sub/t2.tmp", b"T");
        let specs = ["a", "b", "c"].map(spec).to_vec();
        let (r, _) = go_with(&fs, specs, &selecting(), &cfg());
        assert_eq!(ok(&r).excluded, 2);
        // And the mirror: only the second root excludes.
        let fs = fake();
        fs.write_file("/src/b/u1.tmp", b"T");
        let specs = ["a", "b"].map(spec).to_vec();
        let (r, _) = go_with(&fs, specs, &selecting(), &cfg());
        assert_eq!(ok(&r).excluded, 1);
    }

    #[test]
    fn a_root_that_fails_before_its_walk_counts_zero_and_the_rest_are_not_reached() {
        // The first root cannot be created: nothing was walked, so nothing was counted, and `b` never starts.
        let n = reference(with_tmp_files, &["a", "b"], "create_dir", "create_dir(/p/dest/a)");
        let fs = with_tmp_files();
        fs.fail_nth("create_dir", n, Code::IoError, std::io::ErrorKind::Other);
        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &selecting(), &cfg());
        let Some(Err(a)) = &r.run.copy else { panic!("the copy aborted: {:?}", r.run.copy) };
        assert_eq!(a.outcome.excluded, 0);
        assert_eq!(statuses(&r), [RootStatus::Failed, RootStatus::NotStarted]);
        // The second root cannot be created: only `a`'s two count.
        let n = reference(with_tmp_files, &["a", "b"], "create_dir", "create_dir(/p/dest/b)");
        let fs = with_tmp_files();
        fs.fail_nth("create_dir", n, Code::IoError, std::io::ErrorKind::Other);
        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &selecting(), &cfg());
        let Some(Err(a)) = &r.run.copy else { panic!("the copy aborted: {:?}", r.run.copy) };
        assert_eq!(a.outcome.excluded, 2);
    }

    #[test]
    fn a_file_root_is_copied_whatever_the_patterns_say_and_adds_nothing() {
        let fs = with_tmp_files();
        let typed = vec!["f".to_string(), "*.tmp".to_string()];
        let o =
            CopyOptions { selection: flux_fs::Selection::compile(&typed, true).unwrap(), ..opts() };
        let (r, got) = go_with(&fs, ["a", "f"].map(spec).to_vec(), &o, &cfg());
        let out = ok(&r);
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(fs.read_file("/p/dest/f").as_deref(), Some(&b"F"[..]));
        assert_eq!(out.excluded, 2, "a's two tmp files; the file root f adds none");
    }

    /// Run `names` on `fs` and die at the `nth` call to `op`, as a crashed process would: unwinding drops the run's
    /// `FakeLock`s (the OS locks go, every file stays) and no `settle` runs, so the prefix lock files, the destination
    /// lock and the TRANSFERRING manifest are left behind.
    fn crash(fs: &FaultFs, names: &[&str], op: &str, nth: u32) {
        fs.on_nth(op, nth, |_| panic!("the test's crash point"));
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| go(fs, names)));
        assert!(died.is_err(), "the run was meant to die at {op} #{nth}");
    }

    fn restart_cfg(n: u8) -> RunConfig {
        RunConfig { operation_id: format!("{:032x}", n), restart: true, ..cfg() }
    }

    /// A fake whose `["a", "b"]` run died at the publish of `a/x`.
    fn crashed_ab() -> FaultFs {
        let n = reference(fake, &["a", "b"], "rename_no_replace", "/p/dest/a/x");
        let fs = fake();
        crash(&fs, &["a", "b"], "rename_no_replace", n);
        fs
    }

    #[test]
    fn a_several_source_restart_supersedes_a_crashed_run_abandoning_it_first() {
        let fs = crashed_ab();
        assert!(fs.exists(format!("/p/dest/a/x.flux-partial.{ID}")));
        assert_eq!(manifest(&fs).state, OpState::Transferring);
        assert_eq!(prefix_lock_files(&fs).len(), 2, "{:?}", prefix_lock_files(&fs));
        assert!(fs.exists(LOCK));

        // The reference restart, on a fake crashed the same way, says which `rename_no_replace` retires the prior.
        let fs2 = crashed_ab();
        let (r2, _) = go_with(&fs2, ["a", "b"].map(spec).to_vec(), &opts(), &restart_cfg(0x22));
        ok(&r2);
        let k = nth_of(&fs2, "rename_no_replace", &format!("operations/{ID}.removing"));

        let seen = watch_manifest(&fs, "rename_no_replace", k);
        let mark = fs.calls().len();
        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &opts(), &restart_cfg(0x22));
        ok(&r);

        let prior = seen
            .lock()
            .unwrap()
            .clone()
            .expect("the prior's manifest was read at the retiring rename");
        assert_eq!(prior.state, OpState::Abandoned);
        assert_eq!(prior.superseded_by.as_deref(), Some(format!("{:032x}", 0x22).as_str()));
        assert!(!fs.exists(format!("/p/dest/a/x.flux-partial.{ID}")));
        assert!(!fs.exists(format!("/p/dest/.flux/operations/{ID}")));
        assert_eq!(fs.read_file("/p/dest/a/x").as_deref(), Some(&b"ax"[..]));
        assert_eq!(fs.read_file("/p/dest/b/k").as_deref(), Some(&b"bk"[..]));

        // ORDER: the first `create_new` outside `DEST/.flux` is the first partial of the copy; no prefix lock of a
        // name this run uses may be released before it (anti-goal A1). This probe is the only guard of A1 at this
        // level.
        let log = calls(&fs);
        let after = &log[mark..];
        let first_copy_write = after
            .iter()
            .position(|c| {
                c.starts_with("create_new(") && !c.starts_with("create_new(/p/dest/.flux")
            })
            .expect("the restart creates a partial file");
        for name in ["a", "b"] {
            let release = format!("remove_file(/p/dest/{name}.flux-lock)");
            let at =
                after.iter().position(|c| *c == release).unwrap_or_else(|| panic!("no {release}"));
            assert!(
                first_copy_write < at,
                "{name}: the first partial was created at {first_copy_write}, released at {at}: {after:?}"
            );
        }

        assert!(lock_files(&fs).is_empty(), "{:?}", lock_files(&fs));
        for name in ["a", "b"] {
            assert!(!fs.exists(format!("/p/dest/{name}.flux-lock.broken.{:032x}", 0x22)));
        }
    }

    #[test]
    fn a_restart_right_after_a_crash_does_not_wait_for_a_lease() {
        let fs = crashed_ab();
        let bytes = fs.read_file(LOCK).expect("the crashed run left its destination lock");
        let crate::lock::record::Decoded::Record(rec) = crate::lock::record::decode(&bytes) else {
            panic!("the destination lock is not a record");
        };
        // A "now" heartbeat: nothing has aged. `classify`'s `Dead` arm reads no heartbeat (classify.rs:54-57), so the
        // restart must take the lock at once. The mutant that proves it is a STAND-IN (refuse when the record's last
        // heartbeat equals its creation time), because `obtain` takes no clock; the real lease gate would be
        // `obtain_cleanup_lock`'s (`crates/flux-core/src/lock/obtain.rs`). The premise that the heartbeat is "now"
        // holds because the crashed run wrote it with the real clock.
        assert_eq!(rec.last_heartbeat_wall_time, rec.creation_wall_time);
        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &opts(), &restart_cfg(0x23));
        ok(&r);
    }

    #[test]
    fn a_restart_with_other_sources_leaves_the_unused_prefix_lock_and_succeeds() {
        let fs = crashed_ab();
        let (r, _) = go_with(&fs, ["a", "c"].map(spec).to_vec(), &opts(), &restart_cfg(0x24));
        ok(&r);
        // L6 residue, named on purpose: the prior's `b` lock belongs to no root of this run, so nothing releases it.
        assert!(fs.exists("/p/dest/b.flux-lock"));
        assert!(!fs.exists("/p/dest/a.flux-lock"));
        assert!(!fs.exists("/p/dest/c.flux-lock"));
        assert!(!fs.exists(LOCK));
        assert_eq!(fs.read_file("/p/dest/c/z").as_deref(), Some(&b"cz"[..]));
    }

    /// The manifest of the operation `id`, decoded.
    fn manifest_of(fs: &FaultFs, id: &str) -> OperationState {
        let bytes = fs
            .read_file(format!("/p/dest/.flux/operations/{id}/manifest"))
            .expect("the workspace is kept");
        decode_state(&bytes).unwrap()
    }

    /// Like `nth_of`, but the first match is looked for at call-log position `from` or later. The count still starts at
    /// the first call named `op`, because `on_nth` counts from the fake's creation.
    fn nth_of_from(fs: &FaultFs, from: usize, op: &str, needle: &str) -> u32 {
        let head = format!("{op}(");
        let mut n = 0;
        for (at, c) in calls(fs).into_iter().enumerate() {
            if c.starts_with(&head) {
                n += 1;
                if at >= from && c.contains(needle) {
                    return n;
                }
            }
        }
        panic!("no {op} mentioning {needle} from {from} in {:?}", calls(fs));
    }

    /// `watch_manifest` for the operation `id`.
    fn watch_manifest_of(fs: &FaultFs, id: &str, op: &str, nth: u32) -> Seen {
        let seen: Seen = Arc::default();
        let keep = Arc::clone(&seen);
        let id = id.to_string();
        fs.on_nth(op, nth, move |fs| {
            let bytes = fs
                .read_file(format!("/p/dest/.flux/operations/{id}/manifest"))
                .expect("the manifest exists");
            *keep.lock().unwrap() = Some(decode_state(&bytes).unwrap());
        });
        seen
    }

    /// The fixture of the takeover tests: a DEST with an empty (unreadable, nobody holds it) destination lock.
    fn empty_dest_lock() -> FaultFs {
        let fs = fake();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        fs.write_file(LOCK, b"");
        fs
    }

    #[test]
    fn break_lock_takes_over_an_empty_destination_lock_for_several_sources() {
        let id = format!("{:032x}", 0x25);
        let cfg_break = || RunConfig { break_lock: true, ..restart_cfg(0x25) };
        // The manifest is read during the run: a completed run retires its own workspace.
        let fs2 = empty_dest_lock();
        let (r2, _) = go_with(&fs2, ["a", "b"].map(spec).to_vec(), &opts(), &cfg_break());
        ok(&r2);
        let k = nth_of(&fs2, "rename_no_replace", &format!("operations/{id}.removing"));

        let fs = empty_dest_lock();
        let seen = watch_manifest_of(&fs, &id, "rename_no_replace", k);
        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &opts(), &cfg_break());
        ok(&r);
        let own = seen.lock().unwrap().clone().expect("the run's manifest was read");
        assert!(own.takeover.is_some(), "the takeover is recorded: {own:?}");
        assert!(!fs.exists(LOCK));
    }

    #[test]
    fn both_locks_unreadable_refuses_at_the_prefix_lock_and_writes_nothing() {
        let id = format!("{:032x}", 0x25);
        let fs = empty_dest_lock();
        fs.write_file("/p/dest/a.flux-lock", b"");
        let cfg_break = RunConfig { break_lock: true, ..restart_cfg(0x25) };
        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &opts(), &cfg_break);
        match &r.run.stop {
            Some(RunError::Refused { refusal, changed: false, .. }) => {
                assert_eq!(refusal.code, LockCode::TargetLockUncertain);
                let detail = &refusal.detail;
                assert!(detail.starts_with(&lock_prefix("a")), "{detail}");
                assert!(detail.contains("the same flags"), "{detail}");
                assert!(!detail.contains("--break-lock"), "{detail}");
            }
            other => panic!("expected TARGET_LOCK_UNCERTAIN with nothing changed, got {other:?}"),
        }
        assert_eq!(
            fs.read_file(LOCK).as_deref(),
            Some(&b""[..]),
            "the destination lock is as it was"
        );
        assert!(!fs.exists(format!("/p/dest/.flux/operations/{id}")), "nothing was written");
    }

    #[test]
    fn a_live_prefix_lock_refuses_a_restart_and_leaves_the_prior_untouched() {
        let fs = crashed_ab();
        // Another operation recovers the dead lock of `a` and holds it.
        let dest = fs.destination_root(Path::new("/p/dest")).unwrap();
        let other = PrefixIdOwned {
            operation_id: "55555555555555555555555555555555".to_string(),
            owner_instance_id: "other".to_string(),
            boot_session_id: "test-boot".to_string(),
            now: 5,
        };
        let (_held, _) =
            PrefixLocks::acquire(&dest, LockCapability::LocalStrong, &os(&["a"]), &other.as_ref())
                .unwrap();
        let manifest_path = format!("/p/dest/.flux/operations/{ID}/manifest");
        let partial = format!("/p/dest/a/x.flux-partial.{ID}");
        let workspace = format!("/p/dest/.flux/operations/{ID}");
        let snapshot = |fs: &FaultFs| {
            (fs.read_file(&manifest_path), fs.exists(&partial), fs.exists(&workspace))
        };
        let before = snapshot(&fs);
        assert!(before.0.is_some() && before.1 && before.2);

        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &opts(), &restart_cfg(0x26));

        match &r.run.stop {
            Some(RunError::Refused { refusal, .. }) => {
                assert_eq!(refusal.code, LockCode::TargetLockBusy);
                assert!(refusal.detail.starts_with(&lock_prefix("a")), "{}", refusal.detail);
            }
            other => panic!("expected TARGET_LOCK_BUSY, got {other:?}"),
        }
        assert!(snapshot(&fs) == before, "the prior is untouched");
    }

    #[test]
    fn a_several_source_restart_supersedes_a_crashed_one_source_tree_run() {
        let tree = |fs: &FaultFs, c: &RunConfig| {
            crate::run::tree(fs, Path::new("/src/a"), Path::new("/p/dest"), &opts(), c, &mut |_| {})
        };
        // The prior is a one-source tree run that died at the publish of `x`: a resumable TRANSFERRING workspace.
        let fs2 = fake();
        let r2 = tree(&fs2, &cfg());
        assert!(r2.stop.is_none(), "{:?}", r2.stop);
        let n = nth_of(&fs2, "rename_no_replace", "/p/dest/x");

        let fs = fake();
        fs.on_nth("rename_no_replace", n, |_| panic!("the test's crash point"));
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tree(&fs, &cfg())));
        assert!(died.is_err(), "the run was meant to die at the publish of x");
        let prior = manifest(&fs);
        assert_eq!(prior.state, OpState::Transferring);
        assert!(matches!(prior.kind, crate::state::Kind::Tree), "{:?}", prior.kind);
        assert_ne!(prior.format_version, crate::state::MULTI_ROOT_FORMAT);
        assert!(fs.exists(format!("/p/dest/x.flux-partial.{ID}")));

        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &opts(), &restart_cfg(0x27));
        ok(&r);
        assert!(!fs.exists(format!("/p/dest/.flux/operations/{ID}")), "the prior is retired");
        assert!(!fs.exists(format!("/p/dest/x.flux-partial.{ID}")), "its partial is gone");
        assert!(lock_files(&fs).is_empty(), "{:?}", lock_files(&fs));
        assert_eq!(fs.read_file("/p/dest/a/x").as_deref(), Some(&b"ax"[..]));
    }

    #[test]
    fn one_restart_supersedes_a_crashed_run_and_a_restart_that_crashed_before_abandoning_it() {
        let id2 = format!("{:032x}", 0x28);
        let one_source = |fs: &FaultFs| {
            let c = RunConfig { restart: true, operation_id: id2.clone(), ..cfg() };
            crate::run::tree(
                fs,
                Path::new("/src/a"),
                Path::new("/p/dest"),
                &opts(),
                &c,
                &mut |_| {},
            )
        };
        // The reference: the same first crash, then the same restart un-crashed.
        let fs2 = crashed_ab();
        let mark2 = fs2.calls().len();
        let r2 = one_source(&fs2);
        assert!(r2.stop.is_none(), "{:?}", r2.stop);
        let k = nth_of_from(&fs2, mark2, "create_new", &format!("operations/{ID}/manifest"));

        // The twice-crashed fake: the several-source run died, then the one-source restart died before abandoning it.
        let crashed_twice = || {
            let fs = crashed_ab();
            fs.on_nth("create_new", k, |_| panic!("the restart's crash point"));
            let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| one_source(&fs)));
            assert!(died.is_err(), "the restart was meant to die before abandoning its prior");
            fs
        };
        let fs = crashed_twice();
        assert_eq!(manifest_of(&fs, ID).state, OpState::Transferring);
        // Measured: the restart died before it wrote anything past its own first manifest, which is still CREATED.
        assert_eq!(
            manifest_of(&fs, &id2).state,
            OpState::Created,
            "the restart's own workspace exists"
        );

        // The reference restart on a fake crashed the same way says which `rename_no_replace` retires each prior.
        let fs3 = crashed_twice();
        let (r3, _) = go_with(&fs3, ["a", "b"].map(spec).to_vec(), &opts(), &restart_cfg(0x29));
        ok(&r3);
        let k_first = nth_of(&fs3, "rename_no_replace", &format!("operations/{ID}.removing"));
        let k_second = nth_of(&fs3, "rename_no_replace", &format!("operations/{id2}.removing"));
        // Both priors must have been marked ABANDONED, by this run, before their workspaces were retired: the content
        // of every prior's manifest is read just before its own retiring rename.
        let seen_first = watch_manifest_of(&fs, ID, "rename_no_replace", k_first);
        let seen_second = watch_manifest_of(&fs, &id2, "rename_no_replace", k_second);

        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &opts(), &restart_cfg(0x29));
        ok(&r);
        for (which, seen) in
            [("the crashed run", &seen_first), ("the crashed restart", &seen_second)]
        {
            let prior =
                seen.lock().unwrap().clone().unwrap_or_else(|| {
                    panic!("{which}: its manifest was read at its retiring rename")
                });
            assert_eq!(prior.state, OpState::Abandoned, "{which}");
            assert_eq!(
                prior.superseded_by.as_deref(),
                Some(format!("{:032x}", 0x29).as_str()),
                "{which}"
            );
        }
        assert!(!fs.exists(format!("/p/dest/.flux/operations/{ID}")));
        assert!(!fs.exists(format!("/p/dest/.flux/operations/{id2}")));
        assert_eq!(fs.read_file("/p/dest/a/x").as_deref(), Some(&b"ax"[..]));
        assert_eq!(fs.read_file("/p/dest/b/k").as_deref(), Some(&b"bk"[..]));
    }

    // ----- Several sources, --dry-run: the fresh plan (cut 18 task 2) -----

    fn dry_fake() -> FaultFs {
        let fs = fake();
        fs.create_dir(Path::new("/tmp")).unwrap();
        fs
    }

    fn dry_cfg() -> RunConfig {
        RunConfig { dry_run: true, dry_temp_root: Some(PathBuf::from("/tmp")), ..cfg() }
    }

    /// `nth_of` for the LAST call mentioning `needle`: the fake's `open_dir` ends in the last `metadata` of a path.
    fn last_nth_of(fs: &FaultFs, op: &str, needle: &str) -> u32 {
        let head = format!("{op}(");
        let (mut n, mut found) = (0, None);
        for c in calls(fs) {
            if c.starts_with(&head) {
                n += 1;
                if c.contains(needle) {
                    found = Some(n);
                }
            }
        }
        found.unwrap_or_else(|| panic!("no {op} mentioning {needle} in {:?}", calls(fs)))
    }

    type Counters = (u64, u64, u64, u64, u64, u64, u64, u64, u64, u64);

    fn counters(o: &TreeOutcome) -> Counters {
        (
            o.files_copied,
            o.bytes_copied,
            o.directories_created,
            o.files_overwritten,
            o.files_total,
            o.failures.total(),
            o.excluded,
            o.files_skipped,
            o.bytes_skipped,
            o.files_hardlinked,
        )
    }

    fn rows(r: &MultiRun) -> Vec<(OsString, RootStatus, u64, u64)> {
        r.roots.iter().map(|x| (x.name.clone(), x.status, x.files_copied, x.files_failed)).collect()
    }

    /// The real run on `build()` and the dry run on another `build()`: the same counters, pre-flight warnings and
    /// per-root rows, the dry one having touched nothing it must not. Returns the dry run and what it streamed.
    fn dry_agrees(
        build: impl Fn() -> FaultFs,
        names: &[&str],
        o: &CopyOptions,
    ) -> (MultiRun, Vec<TreeFailure>) {
        let real_fs = build();
        let (real, _) = go_with(&real_fs, names.iter().map(|n| spec(n)).collect(), o, &cfg());
        let fs = build();
        let base = calls(&fs).len();
        let (dry, got) = go_with(&fs, names.iter().map(|n| spec(n)).collect(), o, &dry_cfg());
        assert!(dry.run.stop.is_none(), "{:?}", dry.run.stop);
        assert_eq!(counters(ok(&dry)), counters(ok(&real)));
        assert_eq!(ok(&dry).warnings, ok(&real).warnings);
        assert_eq!(rows(&dry), rows(&real));
        crate::run::tests::forbidden_in(&calls(&fs)[base..]);
        (dry, got)
    }

    fn planned(got: &[TreeFailure], path: &str) -> Vec<(PlannedObject, PlannedAction)> {
        got.iter()
            .filter(|f| f.path == Path::new(path))
            .filter_map(|f| match &f.cause {
                crate::tree::TreeFailureCause::Planned(e) => Some((e.object, e.action)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_dry_run_plans_two_folders_and_a_file_into_an_absent_dest() {
        let real_fs = dry_fake();
        let (real, _) = go(&real_fs, &["a", "b", "f"]);
        let real_dirs = ok(&real).directories_created;
        assert_eq!(real_dirs, 4);

        let fs = dry_fake();
        let (r, got) =
            go_with(&fs, ["a", "b", "f"].iter().map(|n| spec(n)).collect(), &opts(), &dry_cfg());

        let out = ok(&r);
        assert!(r.run.stop.is_none());
        assert_eq!(names(&r), os(&["a", "b", "f"]));
        assert_eq!(statuses(&r), vec![RootStatus::Completed; 3]);
        let per_root: Vec<u64> = r.roots.iter().map(|x| x.files_copied).collect();
        assert_eq!(per_root, [2, 1, 1]);
        assert_eq!(out.files_copied, per_root.iter().sum::<u64>());
        assert_eq!(out.files_copied, 4);
        assert_eq!(out.directories_created, real_dirs);
        assert!(
            got.iter().all(|f| f.path != Path::new("dest") && f.path != Path::new("")),
            "{got:?}"
        );
        let would_create = (PlannedObject::Dir, PlannedAction::CreateDir);
        assert_eq!(planned(&got, "a"), [would_create]);
        assert_eq!(planned(&got, "b"), [would_create]);
        assert!(r.run.dry.is_some());
    }

    #[test]
    fn a_dry_run_agrees_with_the_real_run_and_changes_nothing() {
        let (r, _) = dry_agrees(dry_fake, &["a", "b", "f"], &opts());

        assert!(r.run.dry.as_ref().unwrap().unprobed_noreplace);

        // A source directory with a weak identity: the pre-flight warning `trees` records is the dry outcome's too.
        let weak = || {
            let fs = dry_fake();
            fs.set_identity("/src/a", FileIdentity::Weak(ObjectId { volume: 5, index: 1 }));
            fs
        };
        let (r, _) = dry_agrees(weak, &["a", "b", "f"], &opts());
        assert_ne!(ok(&r).warnings, crate::tree::WeakIdentityWarnings::default());
        assert!(
            !r.run
                .warnings
                .iter()
                .any(|w| matches!(w, RunWarning::HardlinkIndexUnavailable { .. }))
        );
    }

    #[test]
    fn a_dry_run_over_existing_targets_overwrites_or_skips_per_root() {
        let existing = || {
            let fs = dry_fake();
            for d in ["/p/dest", "/p/dest/a", "/p/dest/b"] {
                fs.create_dir(Path::new(d)).unwrap();
            }
            fs.write_file("/p/dest/a/x", b"OTHER BYTES");
            fs.write_file("/p/dest/b/k", b"bk");
            fs
        };
        for existing_policy in [ExistingPolicy::Overwrite, ExistingPolicy::SkipExisting] {
            let o = CopyOptions { existing: existing_policy, ..opts() };
            let (_, got) = dry_agrees(existing, &["a", "b", "f"], &o);
            assert!(!got.is_empty());
        }
    }

    #[test]
    fn a_root_holding_a_dot_flux_file_is_planned_as_the_real_run_copies_it() {
        let as_file = || {
            let fs = dry_fake();
            fs.write_file("/src/c/.flux", b"a file named like the control directory");
            fs
        };
        let as_dir = || {
            let fs = dry_fake();
            fs.create_dir(Path::new("/src/c/.flux")).unwrap();
            fs.write_file("/src/c/.flux/g", b"g");
            fs
        };
        for build in [&as_file as &dyn Fn() -> FaultFs, &as_dir] {
            let (r, got) = dry_agrees(build, &["b", "c"], &opts());
            assert!(
                got.iter().all(|f| !matches!(
                    &f.cause,
                    crate::tree::TreeFailureCause::Copy(e) if e.code() == Code::SafetyRejected
                )),
                "{got:?}"
            );
            assert_eq!(ok(&r).failures.total(), 0);
        }
    }

    #[test]
    fn a_dry_run_shares_one_hardlink_slot_across_roots() {
        let grouped = || {
            let fs = dry_fake();
            for d in ["/s1", "/s2"] {
                fs.create_dir(Path::new(d)).unwrap();
            }
            fs.write_file("/s1/a", b"shared");
            let (d1, d2) = (
                fs.destination_root(Path::new("/s1")).unwrap(),
                fs.destination_root(Path::new("/s2")).unwrap(),
            );
            d2.create_hardlink(OsStr::new("a"), &d1, OsStr::new("a")).unwrap();
            fs
        };
        let specs = || {
            ["s1", "s2"]
                .map(|n| SourceSpec {
                    src: Path::new("/").join(n),
                    name: OsString::from(n),
                    kind: SourceKind::Folder,
                })
                .to_vec()
        };
        let real_fs = grouped();
        let (real, _) = go_with(&real_fs, specs(), &opts(), &cfg());
        assert_eq!(ok(&real).files_hardlinked, 1);

        let fs = grouped();
        let (r, _) = go_with(&fs, specs(), &opts(), &dry_cfg());

        assert_eq!(counters(ok(&r)), counters(ok(&real)));
        assert_eq!((ok(&r).files_copied, ok(&r).files_hardlinked), (1, 1));
    }

    #[test]
    fn a_dry_run_counts_excluded_per_root_and_in_total() {
        let typed = ["x".to_string(), "k".to_string()];
        let o =
            CopyOptions { selection: flux_fs::Selection::compile(&typed, true).unwrap(), ..opts() };

        let (r, _) = dry_agrees(dry_fake, &["a", "b", "f"], &o);

        assert_eq!(ok(&r).excluded, 2);
        let per_root: Vec<u64> = r.roots.iter().map(|x| x.files_copied).collect();
        assert_eq!(per_root, [1, 0, 1]);
    }

    #[test]
    fn a_dry_run_into_an_absent_dest_reads_no_locks_and_plans_every_root_new() {
        for safety in [Safety::Default, Safety::Strict] {
            let fs = dry_fake();
            let o = CopyOptions { safety, ..opts() };

            let (r, _) =
                go_with(&fs, ["a", "b", "f"].iter().map(|n| spec(n)).collect(), &o, &dry_cfg());

            assert_eq!(statuses(&r), vec![RootStatus::Completed; 3], "{safety:?}");
            ok(&r);
            let log = calls(&fs);
            assert!(!log.iter().any(|c| c.starts_with("read_file(/p/dest/")), "{log:?}");
            assert!(!log.iter().any(|c| c == "metadata(/p/dest/a)"), "{log:?}");
        }
    }

    /// A DEST that exists with none of the roots in it, and the dry run reference on it.
    fn dest_only() -> FaultFs {
        let fs = dry_fake();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        fs
    }

    fn dry_names(fs: &FaultFs, names: &[&str]) -> (MultiRun, Vec<TreeFailure>) {
        go_with(fs, names.iter().map(|n| spec(n)).collect(), &opts(), &dry_cfg())
    }

    #[test]
    fn a_folder_root_that_cannot_be_opened_aborts_with_the_create_step_error() {
        let reference = dest_only();
        dry_names(&reference, &["a", "b", "f"]);
        let n = last_nth_of(&reference, "metadata", "/p/dest/a)");
        let fs = dest_only();
        fs.fail_nth("metadata", n, Code::IoError, ErrorKind::PermissionDenied);

        let (r, _) = dry_names(&fs, &["a", "b", "f"]);

        assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
        let Some(Err(abort)) = &r.run.copy else { panic!("expected an abort: {:?}", r.run.copy) };
        assert_eq!(abort.error.step, CopyStep::Create);
        assert_eq!(
            statuses(&r),
            [RootStatus::Failed, RootStatus::NotStarted, RootStatus::NotStarted]
        );
    }

    #[test]
    fn a_failed_root_aborts_and_a_counted_failure_does_not() {
        // T12: the second root cannot be opened.
        let reference = dest_only();
        dry_names(&reference, &["a", "b", "f"]);
        let n = last_nth_of(&reference, "metadata", "/p/dest/b)");
        let fs = dest_only();
        fs.fail_nth("metadata", n, Code::IoError, ErrorKind::PermissionDenied);
        let (r, _) = dry_names(&fs, &["a", "b", "f"]);
        assert_eq!(
            statuses(&r),
            [RootStatus::Completed, RootStatus::Failed, RootStatus::NotStarted]
        );

        // T12b: a source file of the second root that cannot be read (`read_dir` is made before the lock, in
        // `prepare_source`, so it cannot fail mid-plan) is a counted failure, and the third root goes on.
        let reference = dest_only();
        dry_names(&reference, &["a", "b", "f"]);
        let n = nth_of(&reference, "metadata", "/src/b/k");
        let fs = dest_only();
        fs.fail_nth("metadata", n, Code::IoError, ErrorKind::PermissionDenied);
        let (r, _) = dry_names(&fs, &["a", "b", "f"]);
        assert_eq!(
            statuses(&r),
            [RootStatus::Completed, RootStatus::Failed, RootStatus::Completed]
        );
        assert!(ok_or_abort_outcome(&r).failures.total() >= 1);
    }

    fn ok_or_abort_outcome(r: &MultiRun) -> &TreeOutcome {
        match copy_of(r) {
            Ok(o) => o,
            Err(a) => &a.outcome,
        }
    }

    #[test]
    fn dest_is_counted_once_when_the_plan_completes() {
        let real_fs = dry_fake();
        let (real, _) = go(&real_fs, &["a", "f"]);
        let fs = dry_fake();

        let (r, _) = dry_names(&fs, &["a", "f"]);

        assert_eq!(ok(&r).directories_created, ok(&real).directories_created);
    }

    #[test]
    fn dest_is_not_counted_when_the_plan_refuses_with_nothing_changed() {
        let fs = dry_fake();
        fs.write_file("/src/g", b"G");
        fs.set_identity("/p", FileIdentity::Weak(ObjectId { volume: 5, index: 1 }));
        let o = CopyOptions { safety: Safety::Strict, ..opts() };
        let files = ["f", "g"]
            .map(|n| SourceSpec {
                src: Path::new("/src").join(n),
                name: OsString::from(n),
                kind: SourceKind::File,
            })
            .to_vec();

        let (r, _) = go_with(&fs, files, &o, &dry_cfg());

        let Some(Err(abort)) = &r.run.copy else { panic!("expected an abort: {:?}", r.run.copy) };
        assert!(abort.refused_unchanged(), "{abort:?}");
        assert_eq!(abort.outcome.directories_created, 0);
    }

    // ----- Several sources, --dry-run: the prefix locks, the directory lock, the targets and --restart (cut 18 task 3) -----

    /// The code and detail of a run that stopped with a refusal.
    fn refusal_of(r: &MultiRun) -> (LockCode, String) {
        match &r.run.stop {
            Some(RunError::Refused { refusal, .. }) => (refusal.code, refusal.detail.clone()),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The real run on `build()` and the dry run on another `build()` refuse with the same code and text. Returns the
    /// dry run.
    fn dry_refuses_as_real(build: impl Fn() -> FaultFs, names: &[&str]) -> (MultiRun, LockCode) {
        let real_fs = build();
        let (real, _) = go(&real_fs, names);
        let want = refusal_of(&real);
        let fs = build();
        let base = calls(&fs).len();
        let (dry, got) = dry_names(&fs, names);
        assert_eq!(refusal_of(&dry), want, "{names:?}");
        assert!(got.is_empty(), "nothing is streamed on a refusal: {got:?}");
        assert!(dry.run.copy.is_none() && dry.roots.is_empty());
        crate::run::tests::forbidden_in(&calls(&fs)[base..]);
        (dry, want.0)
    }

    fn strong_id(index: u128) -> FileIdentity {
        FileIdentity::Strong(ObjectId { volume: 1, index: 9_500_000 + index })
    }

    fn with_dest(build: impl Fn(&FaultFs) + Copy) -> impl Fn() -> FaultFs {
        move || {
            let fs = dry_fake();
            fs.create_dir(Path::new("/p/dest")).unwrap();
            build(&fs);
            fs
        }
    }

    #[test]
    fn dry_refusals_match_the_real_run() {
        // A file root over a directory.
        let (_, code) = dry_refuses_as_real(
            with_dest(|fs| fs.create_dir(Path::new("/p/dest/f")).unwrap()),
            &["a", "f"],
        );
        assert_eq!(code, LockCode::DestinationNamespaceCollision);
        // A folder root over a file.
        let (_, code) = dry_refuses_as_real(
            with_dest(|fs| fs.write_file("/p/dest/a", b"in the way")),
            &["a", "b"],
        );
        assert_eq!(code, LockCode::DestinationNamespaceCollision);
        // An empty lock record.
        let (_, code) = dry_refuses_as_real(
            with_dest(|fs| fs.write_file("/p/dest/a.flux-lock", b"")),
            &["a", "b"],
        );
        assert_eq!(code, LockCode::TargetLockUncertain);
        // An orphan: the record names a workspace that is not there.
        let orphan = with_dest(|fs| {
            let dest = fs.destination_root(Path::new("/p/dest")).unwrap();
            let site = crate::lock::LockSite::directory(&dest, &OsString::from("a")).unwrap();
            let id = "33333333333333333333333333333333";
            let rec = crate::lock::test_support::record(&site, id, &format!("operations/{id}"));
            crate::lock::test_support::dead_lock(&dest, "a.flux-lock", &rec.encode());
        });
        let (_, code) = dry_refuses_as_real(orphan, &["a", "b"]);
        assert_eq!(code, LockCode::ArtifactOwnershipUncertain);
        // A foreign file: more bytes than a record holds.
        let (_, code) = dry_refuses_as_real(
            with_dest(|fs| fs.write_file("/p/dest/a.flux-lock", &vec![b'x'; 5000])),
            &["a", "b"],
        );
        assert_eq!(code, LockCode::ControlPlaneNamespaceConflict);
        // The source directory itself at DEST/a; and the source file itself at DEST/f.
        let same_dir = with_dest(|fs| {
            fs.create_dir(Path::new("/p/dest/a")).unwrap();
            fs.set_identity("/src/a", strong_id(1));
            fs.set_identity("/p/dest/a", strong_id(1));
        });
        let (_, code) = dry_refuses_as_real(same_dir, &["a", "b"]);
        assert_eq!(code, LockCode::SafetyRejected);
        let same_file = with_dest(|fs| {
            fs.write_file("/p/dest/f", b"F");
            fs.set_identity("/src/f", strong_id(2));
            fs.set_identity("/p/dest/f", strong_id(2));
        });
        let (_, code) = dry_refuses_as_real(same_file, &["a", "f"]);
        assert_eq!(code, LockCode::SafetyRejected);

        // Order: the lock of a root before its own target; every lock before every target.
        let both = with_dest(|fs| {
            fs.write_file("/p/dest/a.flux-lock", b"");
            fs.write_file("/p/dest/a", b"in the way");
        });
        let (_, code) = dry_refuses_as_real(both, &["a", "b"]);
        assert_eq!(code, LockCode::TargetLockUncertain);
        let across = with_dest(|fs| {
            fs.write_file("/p/dest/a", b"in the way");
            fs.write_file("/p/dest/b.flux-lock", b"");
        });
        let (r, code) = dry_refuses_as_real(across, &["a", "b"]);
        assert_eq!(code, LockCode::TargetLockUncertain);
        assert!(refusal_of(&r).1.contains("b.flux-lock"), "{:?}", refusal_of(&r));

        // The directory lock is looked at for the first name whose lock file is absent, and only there.
        let dir_lock = with_dest(|fs| fs.write_file("/p/dest/.flux-dir.lock", b""));
        let (r, code) = dry_refuses_as_real(dir_lock, &["a", "b"]);
        assert_eq!(code, LockCode::TargetLockBusy);
        assert!(refusal_of(&r).1.contains("a.flux-lock"), "{:?}", refusal_of(&r));
        let after_unreadable = with_dest(|fs| {
            fs.write_file("/p/dest/.flux-dir.lock", b"");
            fs.write_file("/p/dest/a.flux-lock", b"");
        });
        let (_, code) = dry_refuses_as_real(after_unreadable, &["a", "b"]);
        assert_eq!(code, LockCode::TargetLockUncertain);
    }

    #[test]
    fn dry_run_over_hardlinked_file_roots_is_not_an_alias_refusal() {
        let files = || {
            ["h1", "h2"]
                .map(|n| SourceSpec {
                    src: Path::new("/src").join(n),
                    name: OsString::from(n),
                    kind: SourceKind::File,
                })
                .to_vec()
        };
        let fs = dry_fake();
        fs.write_file("/src/h1", b"shared");
        let (d1, d2) = (
            fs.destination_root(Path::new("/src")).unwrap(),
            fs.destination_root(Path::new("/src")).unwrap(),
        );
        d2.create_hardlink(OsStr::new("h2"), &d1, OsStr::new("h1")).unwrap();
        let (real, _) = go_with(&fs, files(), &opts(), &cfg());
        ok(&real);
        let (d1, d2) = (
            fs.destination_root(Path::new("/p/dest")).unwrap(),
            fs.destination_root(Path::new("/p/dest")).unwrap(),
        );
        let id = |d: &<FaultFs as DestinationRoot>::Dir, n: &str| {
            d.metadata(OsStr::new(n)).unwrap().identity
        };
        assert_eq!(id(&d1, "h1"), id(&d2, "h2"), "the real run hardlinked the targets");

        let skip = CopyOptions { existing: ExistingPolicy::SkipExisting, ..opts() };
        let (r, _) = go_with(&fs, files(), &skip, &dry_cfg());

        assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
        assert_eq!(ok(&r).files_skipped, 2, "{:?}", counters(ok(&r)));
        assert_eq!(ok(&r).files_copied, 0);
    }

    /// DEST holding the prefix locks of a crashed run for `names`, with no resumable operation.
    fn dead_prefix_locks(fs: &FaultFs, names: &[&str]) {
        let dest = fs.destination_root(Path::new("/p/dest")).unwrap();
        let crashed = PrefixIdOwned {
            operation_id: "55555555555555555555555555555555".to_string(),
            owner_instance_id: "dead".to_string(),
            boot_session_id: "old-boot".to_string(),
            now: 5,
        };
        let (held, _) =
            PrefixLocks::acquire(&dest, LockCapability::LocalStrong, &os(names), &crashed.as_ref())
                .unwrap();
        drop(held);
    }

    fn prefix_note_names(r: &MultiRun) -> Vec<Vec<String>> {
        r.run
            .warnings
            .iter()
            .filter_map(|w| match w {
                RunWarning::DryRunPrefixLocks { names } => Some(names.clone()),
                _ => None,
            })
            .collect()
    }

    fn liveness(r: &MultiRun) -> bool {
        r.run.dry.as_ref().expect("the dry flags").owner_liveness
    }

    fn lock_record_notes(r: &MultiRun) -> usize {
        r.run.warnings.iter().filter(|w| matches!(w, RunWarning::DryRunLockRecord { .. })).count()
    }

    #[test]
    fn prefix_lock_notes_are_one_aggregate_and_survive_a_target_refusal() {
        // Two readable trusted prefix locks and no resumable prior: one aggregate note, no refusal.
        let fs = dest_only();
        dead_prefix_locks(&fs, &["a", "b"]);
        assert_eq!(prefix_lock_files(&fs).len(), 2);
        let (r, _) = dry_names(&fs, &["a", "b"]);
        assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
        assert_eq!(prefix_note_names(&r), [["a", "b"]]);
        assert_eq!(lock_record_notes(&r), 0);
        assert!(liveness(&r));

        // Negative control: no lock files.
        let fs = dest_only();
        let (r, _) = dry_names(&fs, &["a", "b"]);
        assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
        assert!(prefix_note_names(&r).is_empty());
        assert!(!liveness(&r));

        // Mixed: root 1 readable, root 2 unreadable: root 2's refusal, none of root 1's notes.
        let fs = dest_only();
        dead_prefix_locks(&fs, &["a"]);
        fs.write_file("/p/dest/b.flux-lock", b"");
        let (r, _) = dry_names(&fs, &["a", "b"]);
        let (code, detail) = refusal_of(&r);
        assert_eq!(code, LockCode::TargetLockUncertain);
        assert!(detail.contains("b.flux-lock"), "{detail}");
        assert!(
            prefix_note_names(&r).is_empty() && lock_record_notes(&r) == 0,
            "{:?}",
            r.run.warnings
        );
        assert!(!liveness(&r));

        // Combined: root 1 readable plus a leftover, root 2's target the wrong kind: the target's refusal and the notes.
        let fs = dest_only();
        dead_prefix_locks(&fs, &["a"]);
        let broken = format!("a.flux-lock.broken.{ID}");
        fs.write_file(Path::new("/p/dest").join(&broken), b"");
        fs.create_dir(Path::new("/p/dest/f")).unwrap();
        let (r, _) = dry_names(&fs, &["a", "f"]);
        assert_eq!(refusal_of(&r).0, LockCode::DestinationNamespaceCollision);
        assert_eq!(prefix_note_names(&r), [["a"]]);
        assert!(liveness(&r));
        let at = |want: &dyn Fn(&RunWarning) -> bool| r.run.warnings.iter().position(want);
        let record = at(&|w| matches!(w, RunWarning::DryRunPrefixLocks { .. })).unwrap();
        let leftover =
            at(&|w| matches!(w, RunWarning::DryRunBrokenLeftover { name } if *name == broken));
        assert!(record < leftover.expect("the leftover is noted"), "{:?}", r.run.warnings);
    }

    fn dry_restart_cfg() -> RunConfig {
        RunConfig { operation_id: format!("{:032x}", 0x22), restart: true, ..dry_cfg() }
    }

    fn crashed_ab_dry() -> FaultFs {
        let fs = crashed_ab();
        fs.create_dir(Path::new("/tmp")).unwrap();
        fs
    }

    fn discards(got: &[TreeFailure]) -> usize {
        got.iter()
            .filter(|f| {
                matches!(&f.cause, crate::tree::TreeFailureCause::Planned(e) if e.action == PlannedAction::Discard)
            })
            .count()
    }

    #[test]
    fn a_dry_restart_previews_the_supersede_of_a_crashed_run() {
        let fs = crashed_ab_dry();
        let base = calls(&fs).len();
        let specs = ["a", "b"].map(spec).to_vec();

        let (r, got) = go_with(&fs, specs, &opts(), &dry_restart_cfg());

        assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
        assert_eq!(prefix_note_names(&r), [["a", "b"]]);
        assert!(liveness(&r));
        match &r.run.dry.as_ref().unwrap().prior {
            Some(DryPrior::Restart { superseded, partials_found, states }) => {
                assert_eq!(superseded.len(), 1, "{superseded:?}");
                assert_eq!(superseded[0].0, ID);
                assert_eq!(partials_found, &[PathBuf::from(format!("a/x.flux-partial.{ID}"))]);
                assert_eq!(*states, 1);
            }
            other => panic!("expected a restart preview, got {other:?}"),
        }
        assert!(discards(&got) >= 1, "{got:?}");
        crate::run::tests::forbidden_in(&calls(&fs)[base..]);

        // The ordering row: a wrong-kind target refuses before the supersede is previewed (A2).
        let fs = crashed_ab_dry();
        fs.create_dir(Path::new("/p/dest/f")).unwrap();
        let specs = ["a", "b", "f"].map(spec).to_vec();
        let (r, got) = go_with(&fs, specs, &opts(), &dry_restart_cfg());
        assert_eq!(refusal_of(&r).0, LockCode::DestinationNamespaceCollision);
        assert_eq!(discards(&got), 0, "{got:?}");
    }

    #[test]
    fn a_dry_restart_break_lock_takes_over_an_empty_destination_lock() {
        let cfg_break = || RunConfig { break_lock: true, ..dry_restart_cfg() };
        let build = || {
            let fs = empty_dest_lock();
            fs.create_dir(Path::new("/tmp")).unwrap();
            fs
        };
        let fs = build();
        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &opts(), &cfg_break());
        assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
        assert!(r.run.dry.as_ref().unwrap().lock_takeover);

        // An unreadable prefix lock too: the prefix refusal.
        let fs = build();
        fs.write_file("/p/dest/a.flux-lock", b"");
        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &opts(), &cfg_break());
        let (code, detail) = refusal_of(&r);
        assert_eq!(code, LockCode::TargetLockUncertain);
        assert!(detail.contains("a.flux-lock"), "{detail}");
    }

    #[test]
    fn a_plain_dry_run_over_a_crashed_run_refuses_naming_restart() {
        let fs = crashed_ab_dry();
        fs.create_dir(Path::new("/p/dest/f")).unwrap();
        let specs = ["a", "b", "f"].map(spec).to_vec();

        let plain = RunConfig { restart: false, ..dry_restart_cfg() };
        let (r, got) = go_with(&fs, specs, &opts(), &plain);

        let (code, detail) = refusal_of(&r);
        assert_eq!(code, LockCode::ResumableOperationExists);
        assert!(detail.contains("--restart"), "{detail}");
        assert!(got.is_empty(), "{got:?}");

        // --restart over no DEST at all: nothing to supersede, and the fresh plan.
        let fs = dry_fake();
        let (r, _) = go_with(&fs, ["a", "b"].map(spec).to_vec(), &opts(), &dry_restart_cfg());
        assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
        assert_eq!(
            r.run.dry.as_ref().unwrap().prior,
            Some(DryPrior::Restart {
                superseded: Vec::new(),
                partials_found: Vec::new(),
                states: 0
            })
        );
        assert_eq!(statuses(&r), vec![RootStatus::Completed; 2]);
    }

    // ----- Several sources, --dry-run: the fold-probe (cut 18 task 4) -----

    const CASE_NAMES: [&str; 2] = ["Data", "data"];

    /// `dry_fake` with the sources `Data`, `Mid` and `data` (made while the fake is case-sensitive: they differ only by
    /// case), then folding set to `fold`.
    fn case_fake(fold: bool) -> FaultFs {
        let fs = dry_fake();
        for n in ["Data", "Mid", "data"] {
            fs.create_dir(Path::new("/src").join(n).as_path()).unwrap();
            fs.write_file(format!("/src/{n}/k"), n.as_bytes());
        }
        fs.set_case_insensitive(fold);
        fs
    }

    /// A DEST holding `entries` (files), over `case_fake(fold)`.
    fn case_dest(fold: bool, entries: &[&str]) -> FaultFs {
        let fs = case_fake(false);
        fs.create_dir(Path::new("/p/dest")).unwrap();
        for e in entries {
            fs.write_file(format!("/p/dest/{e}"), b"");
        }
        fs.set_case_insensitive(fold);
        fs
    }

    /// DEST holds the lock file of a crashed run for each of `locked`, in the spelling given, over a folding fake.
    fn residue_of(locked: &[&str]) -> FaultFs {
        let fs = case_fake(true);
        fs.create_dir(Path::new("/p/dest")).unwrap();
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
            &os(locked),
            &crashed.as_ref(),
        )
        .unwrap();
        drop(held);
        fs
    }

    /// The dry run on `build()`, with the calls it made.
    fn dry_with_calls(build: impl Fn() -> FaultFs, names: &[&str]) -> (MultiRun, Vec<String>) {
        let fs = build();
        let base = calls(&fs).len();
        let (dry, _) = dry_names(&fs, names);
        (dry, calls(&fs)[base..].to_vec())
    }

    fn proceeds(r: &MultiRun) {
        assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
        assert!(r.run.dry.is_some());
    }

    fn collides(r: &MultiRun) {
        let (code, detail) = refusal_of(r);
        assert_eq!(code, LockCode::DestinationNamespaceCollision, "{detail}");
        assert!(detail.contains("are one name on this filesystem"), "{detail}");
    }

    #[test]
    fn a_residue_lock_file_is_an_alias_for_two_case_equal_names() {
        // T14: the lock file of one spelling is the file of the other.
        let (_, code) = dry_refuses_as_real(|| residue_of(&["Data"]), &CASE_NAMES);
        assert_eq!(code, LockCode::DestinationNamespaceCollision);
        // The mirror.
        let (_, code) = dry_refuses_as_real(|| residue_of(&["data"]), &CASE_NAMES);
        assert_eq!(code, LockCode::DestinationNamespaceCollision);
        // Three roots, the twins apart in prefix order (bytewise: Data, Mid, data).
        let (dry, code) = dry_refuses_as_real(|| residue_of(&["Data"]), &["Data", "Mid", "data"]);
        assert_eq!(code, LockCode::DestinationNamespaceCollision);
        collides(&dry);

        // The listing of DEST fails: the alias is still reported (it needs no listing).
        let reference = residue_of(&["Data"]);
        let base = calls(&reference).len();
        dry_names(&reference, &CASE_NAMES);
        let n = nth_of_after(&reference, base, "read_dir", "(/p/dest)");
        let fs = residue_of(&["Data"]);
        fs.fail_nth("read_dir", n, Code::IoError, ErrorKind::PermissionDenied);
        let (dry, _) = dry_names(&fs, &CASE_NAMES);
        collides(&dry);

        // Equal Unavailable, and separately equal Weak, identities decide nothing.
        for identity in
            [FileIdentity::Unavailable, FileIdentity::Weak(ObjectId { volume: 3, index: 4 })]
        {
            let fs = residue_of(&["Data"]);
            fs.set_identity("/p/dest/Data.flux-lock", identity);
            let (dry, _) = dry_names(&fs, &CASE_NAMES);
            proceeds(&dry);
        }

        // Two hardlinked existing targets, no lock file: not an alias (a previous copy under `Hardlinks::Auto`).
        let fs = case_dest(false, &[]);
        for n in ["Data", "data"] {
            fs.create_dir(Path::new("/p/dest").join(n).as_path()).unwrap();
            fs.set_identity(format!("/p/dest/{n}"), strong_id(77));
        }
        let (dry, _) = dry_names(&fs, &CASE_NAMES);
        proceeds(&dry);
    }

    /// `nth_of` over the calls after the first `base`.
    fn nth_of_after(fs: &FaultFs, base: usize, op: &str, needle: &str) -> u32 {
        let head = format!("{op}(");
        let mut n = 0;
        for c in &calls(fs)[base..] {
            if c.starts_with(&head) {
                n += 1;
                if c.contains(needle) {
                    return n;
                }
            }
        }
        panic!("no {op} mentioning {needle}");
    }

    /// How many times the parent `/p` was listed. The destination lock's own leftover scan lists it once in every dry
    /// run, so a probe that read the parent makes two.
    fn parent_listings(log: &[String]) -> usize {
        log.iter().filter(|c| *c == "read_dir(/p)").count()
    }

    fn mentions(log: &[String], op: &str, path: &str) -> bool {
        log.iter().any(|c| c == &format!("{op}({path})"))
    }

    fn with_parent_entry(fold: bool, dest: Option<&[&str]>) -> impl Fn() -> FaultFs {
        let dest: Option<Vec<String>> = dest.map(|d| d.iter().map(|s| s.to_string()).collect());
        move || {
            let fs = case_fake(false);
            fs.write_file("/p/q", b"");
            if let Some(entries) = &dest {
                fs.create_dir(Path::new("/p/dest")).unwrap();
                for e in entries {
                    fs.write_file(format!("/p/dest/{e}"), b"");
                }
            }
            fs.set_case_insensitive(fold);
            fs
        }
    }

    fn real_proceeds(build: impl Fn() -> FaultFs, names: &[&str]) {
        let fs = build();
        let (real, _) = go(&fs, names);
        ok(&real);
    }

    #[test]
    fn the_fold_probe_decides_from_the_listing_of_a_directory() {
        // (1) DEST absent: the parent's entry decides.
        let build = with_parent_entry(true, None);
        let (_, code) = dry_refuses_as_real(&build, &CASE_NAMES);
        assert_eq!(code, LockCode::DestinationNamespaceCollision);
        let (_, log) = dry_with_calls(&build, &CASE_NAMES);
        assert_eq!(parent_listings(&log), 2, "{log:?}");
        // (2) folding off: the plan goes on, as the real run does.
        let build = with_parent_entry(false, None);
        real_proceeds(&build, &CASE_NAMES);
        let (dry, _) = dry_with_calls(&build, &CASE_NAMES);
        proceeds(&dry);

        // (2b) eight entries, folding off: decided at the first candidate in bytewise order.
        let build = with_parent_entry(false, Some(&["a", "b", "c", "d", "e", "f", "g", "h"]));
        real_proceeds(&build, &CASE_NAMES);
        let (dry, log) = dry_with_calls(&build, &CASE_NAMES);
        proceeds(&dry);
        assert_eq!(parent_listings(&log), 1, "{log:?}");
        for n in ["b", "c", "d", "e", "f", "g", "h", "B", "C", "D", "E", "F", "G", "H"] {
            assert!(!mentions(&log, "metadata", &format!("/p/dest/{n}")), "{n}: {log:?}");
        }

        // (3) DEST holds an entry, folding on: collision, and the parent is not read.
        let build = with_parent_entry(true, Some(&["q"]));
        let (_, code) = dry_refuses_as_real(&build, &CASE_NAMES);
        assert_eq!(code, LockCode::DestinationNamespaceCollision);
        let (_, log) = dry_with_calls(&build, &CASE_NAMES);
        assert_eq!(parent_listings(&log), 1, "{log:?}");

        // (4) DEST holds nothing with a letter, the parent is on its volume: the parent decides.
        let build = with_parent_entry(true, Some(&["1", "2"]));
        let (_, code) = dry_refuses_as_real(&build, &CASE_NAMES);
        assert_eq!(code, LockCode::DestinationNamespaceCollision);
        let (_, log) = dry_with_calls(&build, &CASE_NAMES);
        assert_eq!(parent_listings(&log), 2, "{log:?}");
        // (4b) the parent is on another volume: undecided (the real run refuses; the blind spot).
        let other_volume = || {
            let fs = with_parent_entry(true, Some(&["1", "2"]))();
            fs.set_identity("/p", FileIdentity::Strong(ObjectId { volume: 99, index: 5 }));
            fs
        };
        let (dry, _) = dry_with_calls(other_volume, &CASE_NAMES);
        proceeds(&dry);
        // (4c) DEST's listing fails: the parent decides on one volume, and nothing decides on two.
        let build = with_parent_entry(true, Some(&["q"]));
        let reference = build();
        let base = calls(&reference).len();
        dry_names(&reference, &CASE_NAMES);
        let n = nth_of_after(&reference, base, "read_dir", "(/p/dest)");
        let fs = build();
        fs.fail_nth("read_dir", n, Code::IoError, ErrorKind::PermissionDenied);
        let base = calls(&fs).len();
        let (dry, _) = dry_names(&fs, &CASE_NAMES);
        collides(&dry);
        assert_eq!(parent_listings(&calls(&fs)[base..]), 2);
        let fs = build();
        fs.set_identity("/p", FileIdentity::Strong(ObjectId { volume: 99, index: 5 }));
        fs.fail_nth("read_dir", n, Code::IoError, ErrorKind::PermissionDenied);
        let base = calls(&fs).len();
        let (dry, _) = dry_names(&fs, &CASE_NAMES);
        proceeds(&dry);
        assert_eq!(parent_listings(&calls(&fs)[base..]), 1);

        // (5) no ASCII letter anywhere: undecided. DEST is `/1/2`, so the parent holds nothing but `2`.
        let fs = case_fake(false);
        for d in ["/1", "/1/2"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.set_case_insensitive(true);
        let specs = CASE_NAMES.map(spec).to_vec();
        let r = trees(&fs, specs, Path::new("/1/2"), &opts(), &dry_cfg(), &mut |_| {});
        proceeds(&r);

        // (6) `Ab` and `ab` coexist: case-sensitive, no refusal.
        let build = || case_dest(false, &["Ab", "ab"]);
        real_proceeds(build, &CASE_NAMES);
        let (dry, _) = dry_with_calls(build, &CASE_NAMES);
        proceeds(&dry);

        // (7) no two names equal after ASCII lower-casing: no probe.
        for names in [&["a", "b"][..], &["\u{c9}", "\u{e9}"][..]] {
            let build = || {
                let fs = with_parent_entry(true, Some(&["q"]))();
                for n in names {
                    fs.create_dir(Path::new("/src").join(n).as_path()).ok();
                    fs.write_file(format!("/src/{n}/k"), b"k");
                }
                fs
            };
            let (dry, log) = dry_with_calls(build, names);
            proceeds(&dry);
            assert_eq!(parent_listings(&log), 1, "{log:?}");
            assert!(!mentions(&log, "metadata", "/p/dest/Q"), "{log:?}");
            assert!(!mentions(&log, "metadata", "/p/dest/q"), "{log:?}");
        }

        // (8) a name that is not UTF-8 is not a candidate.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let odd = OsString::from_vec(vec![0xff, b'a']);
            let build = || {
                let fs = case_dest(true, &[]);
                fs.write_file(Path::new("/p/dest").join(&odd), b"");
                fs
            };
            let (dry, log) = dry_with_calls(build, &CASE_NAMES);
            collides(&dry);
            assert!(
                !log.iter().any(|c| c.starts_with("metadata(") && c.contains('\u{fffd}')),
                "{log:?}"
            );
        }

        // (9) an undecidable candidate is passed over, whichever way it is undecidable.
        for undecided in
            [FileIdentity::Unavailable, FileIdentity::Weak(ObjectId { volume: 1, index: 424_242 })]
        {
            let build = || {
                let fs = case_dest(true, &["a", "b"]);
                fs.set_identity("/p/dest/a", undecided);
                fs
            };
            let (dry, _) = dry_with_calls(build, &CASE_NAMES);
            collides(&dry);
            // Nothing else to ask: undecided, no refusal.
            let alone = || {
                let fs = case_dest(true, &["a"]);
                fs.set_identity("/p/dest/a", undecided);
                fs
            };
            let (dry, _) = dry_with_calls(alone, &CASE_NAMES);
            proceeds(&dry);
        }
        // The cap: eight undecidable entries stop the walk before the thirty-two decidable ones.
        let build = || {
            let fs = case_dest(true, &[]);
            for i in 0..8 {
                fs.write_file(format!("/p/dest/u{i}"), b"");
                fs.set_identity(format!("/p/dest/u{i}"), FileIdentity::Unavailable);
            }
            for i in 0..32 {
                fs.write_file(format!("/p/dest/z{i:02}"), b"");
            }
            fs
        };
        let (dry, log) = dry_with_calls(build, &CASE_NAMES);
        proceeds(&dry);
        let asked = |c: &String, p: &str| c.starts_with("metadata(") && c.contains(p);
        let probed = log.iter().filter(|c| asked(c, "/p/dest/u") || asked(c, "/p/dest/U")).count();
        assert!((1..=16).contains(&probed), "{probed}: {log:?}");
        assert!(!log.iter().any(|c| asked(c, "/p/dest/z") || asked(c, "/p/dest/Z")), "{log:?}");

        // (10) a residue lock file and the probe both apply: one collision.
        let (dry, code) = dry_refuses_as_real(
            || {
                let fs = residue_of(&["Data"]);
                fs.write_file("/p/dest/q", b"");
                fs
            },
            &CASE_NAMES,
        );
        assert_eq!(code, LockCode::DestinationNamespaceCollision);
        collides(&dry);

        // (11) an earlier root whose lock cannot be read is reported before the collision.
        let (_, code) = dry_refuses_as_real(
            || {
                let fs = case_dest(true, &["q"]);
                fs.write_file("/p/dest/Data.flux-lock", b"");
                fs
            },
            &CASE_NAMES,
        );
        assert_eq!(code, LockCode::TargetLockUncertain);
    }
}
