//! Where an operation's state lives - what differs between a tree and a single file. A tree's state is the workspace
//! `DEST/.flux/operations/<id>/manifest` and its lock is beside DEST; a single file's state is the record
//! `<target>.flux-state.<id>` beside the target, with its lock.

use super::session::{Fault, Locked, checked, failed, from_lock};
use super::{RunError, RunStep, RunWarning};
use crate::copy::{CopyError, CopyStep, split_destination};
use crate::lock::{LockCode, LockResult, Refusal};
use crate::prior::{PriorOp, Scan, scan_file, scan_tree};
use crate::state::{
    FLUX_DIR, Kind, MANIFEST, OPERATIONS_DIR, OpState, OperationState, PARTIAL_INFIX, PROBE,
    PROBE_TEMP, Root, STATE_DB, absolute_lexical, begin_workspace, creating_name, id_after,
    identity_text, native_hex, operations_dir, publish_workspace, record_name,
    remove_empty_control_dirs, remove_record, retire_workspace, write_state,
};
use crate::tree::{TreeOutcome, containment, preflight, primitive_unavailable, reserved_path};
use crate::walk::{WalkEvent, walk};
use flux_fs::{
    ClaimStore, Code, DestinationRoot, DirHandle, Durability, FileIdentity, FileType, FsError,
    OperationId, Safety, temp_path,
};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// One step of "The run" that differs between a tree and a single file.
pub(crate) trait Place<D: DirHandle> {
    /// The operation kind its state records.
    fn kind(&self) -> Kind;
    /// The destination as the operator gave it (the state's `destination_root`, Part 3a decision 5).
    fn destination(&self) -> &Path;
    /// The directory holding the lock, as messages name it.
    fn holder_shown(&self) -> &Path;
    /// The lock record's `workspace_path` for operation `id`: `operations/<id>` or `adjacent/<id>`.
    fn workspace_path(&self, id: &str) -> String;
    /// How messages name operation `id`'s state.
    fn shown(&self, id: &str) -> PathBuf;
    /// Step 4: the prior operations (§21.1).
    fn scan(&self, own_id: &str) -> LockResult<Scan>;
    /// Step 5: create this operation's state, CREATED; for a tree, DEST first where it is absent (E1), then the
    /// section 241.5 probe inside the unpublished workspace (cut 8a, Part P), then the workspace published.
    fn create(
        &mut self,
        state: &OperationState,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<(), RunError>;
    /// Rewrite `state` where its operation's state lives: this run's, or a prior's under `--restart`.
    fn write(&self, state: &OperationState) -> flux_fs::Result<()>;
    /// `--restart`: delete the superseded operations' partials, the heartbeat and `still_owned` before each deletion.
    /// Returns the ids whose partials are all gone; a partial that stays keeps its operation's ABANDONED state as its
    /// record (J1).
    fn sweep(
        &self,
        priors: &[PriorOp],
        locked: &Locked<'_, D>,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<Vec<String>, Fault>;
    /// Remove operation `id`'s state: a workspace, retired first (decision 17), or a record and its temporary.
    fn remove(&self, id: &str) -> Result<(), (PathBuf, FsError)>;
    /// Finish step 4: the empty control directories; on a `rollback` (Q-I), also a DEST this run made.
    fn remove_control_dirs(&mut self, rollback: bool) -> Result<(), (PathBuf, FsError)>;
    /// A single file's identities for its record (cut 7b): the source's, and the object at the target name now
    /// (`None` if there is none, `Unavailable` if it cannot be read). Read under the lock, as the state is made. `None`
    /// for a tree.
    fn file_identities(&self) -> Option<(FileIdentity, Option<FileIdentity>)>;
    /// Cut 9a: the one root this run records (format 3 `roots`).
    fn roots(&self) -> Vec<Root>;
    /// Cut 9a: adopt a validated prior. A tree opens `operations/<id>/state.db` (`open_claim_store`), or, while
    /// `prior.state` is `CREATED` and the file is absent, zero-length or `Code::StateCorrupt`, removes what is there
    /// and creates a fresh store; returns the claim count. A single file opens nothing and returns `None`.
    fn adopt(
        &mut self,
        prior: &OperationState,
        durability: Durability,
    ) -> Result<Option<u64>, RunError>;
}

/// Which file of the probe a failure was at: the temporary's creation, or the publish onto `PROBE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProbeAt {
    Create,
    Publish,
}

/// What the probe found. A `leftover` is a file of the probe's that could not be removed again, by name.
pub(crate) enum Probe {
    /// The primitive is there; `leftover` is `noreplace-probe` (a warning, it goes with the workspace).
    Available { leftover: Option<(OsString, FsError)> },
    /// The primitive is missing (`primitive_unavailable`); `leftover` is the staged temporary.
    Unavailable { leftover: Option<(OsString, FsError)> },
}

/// Part P: stage `PROBE_TEMP` in `workspace` (an empty file), publish it onto `PROBE` with `rename_no_replace`,
/// then remove what was written. `Err` is any failure other than "primitive unavailable" of the publish, or a
/// failed create; the temporary is removed best-effort before returning it. The `Err` says which of the two failed.
pub(crate) fn probe_no_replace<D: DirHandle>(
    workspace: &D,
) -> std::result::Result<Probe, (ProbeAt, FsError)> {
    let temp = OsStr::new(PROBE_TEMP);
    let name = OsStr::new(PROBE);
    // Nothing is written: the writer is dropped at once.
    drop(workspace.create_new(temp).map_err(|e| (ProbeAt::Create, e))?);
    let remove = |n: &OsStr| workspace.remove_file(n).err().map(|e| (n.to_os_string(), e));
    match workspace.rename_no_replace(temp, workspace, name) {
        Ok(()) => Ok(Probe::Available { leftover: remove(name) }),
        Err(e) if primitive_unavailable(&e.source) => {
            Ok(Probe::Unavailable { leftover: remove(temp) })
        }
        Err(e) => {
            let _ = workspace.remove_file(temp);
            Err((ProbeAt::Publish, e))
        }
    }
}

/// What B1 resolved for a tree's DEST: the directory holding the lock, DEST's name in it, and DEST if it exists.
pub(crate) struct LocatedTree<D> {
    pub(crate) holder: D,
    pub(crate) holder_shown: PathBuf,
    pub(crate) name: Option<OsString>,
    pub(crate) dest: Option<D>,
}

/// B1 for DEST: resolve it the way the run uses it - its parent, which holds the lock, then DEST through that parent,
/// never following a link at DEST itself - and run the §129 identity pre-flight against DEST, or its parent while DEST
/// is absent, before anything is created. A filesystem root holds its own lock (`T/.flux-root.lock`).
pub(crate) fn locate_tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    src_identity: FileIdentity,
    safety: Safety,
    out: &mut TreeOutcome,
) -> Result<LocatedTree<F::Dir>, CopyError> {
    let resolve = |e| CopyError::at(CopyStep::Resolve, e);
    if dst_root.file_name().is_none() {
        let holder = fs.destination_root(dst_root).map_err(resolve)?;
        preflight(src_identity, holder.identity().map_err(resolve)?, safety, &mut out.warnings)?;
        containment(fs, src_root, &holder, dst_root, safety, out)?;
        let dest = fs.destination_root(dst_root).map_err(resolve)?;
        return Ok(LocatedTree {
            holder,
            holder_shown: dst_root.to_path_buf(),
            name: None,
            dest: Some(dest),
        });
    }
    let (parent_path, name) = split_destination(dst_root)?;
    let holder = fs.destination_root(parent_path).map_err(resolve)?;
    let dest = match holder.metadata(name) {
        Err(e) if e.source.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(resolve(e)),
        Ok(m) if m.file_type == FileType::Dir => Some(holder.open_dir(name).map_err(resolve)?),
        Ok(m) if m.file_type == FileType::Symlink => {
            return Err(resolve(FsError::new(
                Code::SafetyRejected,
                std::io::Error::other(
                    "the destination is a symlink, which Flux never writes through",
                ),
            )));
        }
        Ok(_) => {
            return Err(resolve(FsError::new(
                Code::DestinationError,
                std::io::Error::from(ErrorKind::NotADirectory),
            )));
        }
    };
    let anchor = match &dest {
        Some(d) => d.identity(),
        None => holder.identity(),
    }
    .map_err(resolve)?;
    preflight(src_identity, anchor, safety, &mut out.warnings)?;
    let (anchor_handle, anchor_shown) = match &dest {
        Some(d) => (d, dst_root),
        None => (&holder, parent_path),
    };
    containment(fs, src_root, anchor_handle, anchor_shown, safety, out)?;
    Ok(LocatedTree {
        holder,
        holder_shown: parent_path.to_path_buf(),
        name: Some(name.to_os_string()),
        dest,
    })
}

/// A tree's DEST: its parent holds the lock, and its state lives under `DEST/.flux/operations/`.
pub(crate) struct TreePlace<'p, F: DestinationRoot> {
    pub(crate) fs: &'p F,
    /// DEST's parent, which holds the lock - or DEST itself, for a filesystem root.
    pub(crate) holder: &'p F::Dir,
    pub(crate) holder_shown: PathBuf,
    /// DEST's name in `holder`; `None` for a root.
    pub(crate) name: Option<OsString>,
    /// DEST, once it exists (E1: the run makes it at step 5).
    pub(crate) dest: Option<F::Dir>,
    pub(crate) dest_shown: PathBuf,
    /// This run made DEST: a rollback removes it, and the outcome counts it.
    pub(crate) created_dest: bool,
    /// `DEST/.flux/operations/`, once step 5 made or opened it.
    pub(crate) operations: Option<F::Dir>,
    /// The run's claim store (`<id>/state.db`), from its creation until `run::tree` takes it for the walk.
    pub(crate) claims: Option<<F::Dir as DirHandle>::Claims>,
    /// `opts.durability`, for the store's creation.
    pub(crate) durability: Durability,
    /// The source root as the operator named it, and its identity, for the record's `roots` (cut 9a).
    pub(crate) src_root: PathBuf,
    pub(crate) src_identity: FileIdentity,
}

/// A refusal of the adopted state that changed nothing.
fn refused_state(code: LockCode, detail: String) -> RunError {
    RunError::Refused {
        refusal: Box::new(Refusal { code, holder: None, detail }),
        changed: false,
        not_removed: None,
    }
}

const BY_HAND: &str = "Flux never deletes state it cannot read (§249.4): inspect it, and remove it by hand if it is not needed";

impl<F: DestinationRoot> TreePlace<'_, F> {
    fn operations_shown(&self) -> PathBuf {
        self.dest_shown.join(FLUX_DIR).join(OPERATIONS_DIR)
    }

    /// Remove `<id>.creating`, then the control directories (and a DEST this run made); the first failed removal.
    fn unwind_creating(&mut self, operations: F::Dir, id: &str) -> Option<(PathBuf, FsError)> {
        let mut not_removed = operations
            .remove_dir(&creating_name(id))
            .err()
            .map(|e| (self.operations_shown().join(creating_name(id)), e));
        self.operations = Some(operations);
        if let Err(first) = self.remove_control_dirs(true) {
            not_removed.get_or_insert(first);
        }
        not_removed
    }

    /// The refusal for a destination without the no-replace primitive: nothing was published, and what this run
    /// made is removed again; a leftover makes it `changed`.
    fn refuse_no_replace(
        &mut self,
        operations: F::Dir,
        id: &str,
        leftover: Option<(OsString, FsError)>,
    ) -> RunError {
        let shown = self.operations_shown().join(creating_name(id));
        let leftover = leftover.map(|(n, e)| (shown.join(n), e));
        let unwound = self.unwind_creating(operations, id);
        let not_removed = leftover.or(unwound);
        RunError::Refused {
            refusal: Box::new(Refusal {
                code: LockCode::NoReplacePublishUnavailable,
                holder: None,
                detail: format!(
                    "{}: the destination has no atomic no-replace publication primitive, which a directory copy needs so that it never replaces a file (probed at {})",
                    self.dest_shown.display(),
                    shown.join(PROBE).display()
                ),
            }),
            changed: not_removed.is_some(),
            not_removed,
        }
    }

    fn operations(&self) -> &F::Dir {
        self.operations
            .as_ref()
            .expect("step 5 made operations/ before any state is rewritten or removed")
    }
}

impl<F: DestinationRoot> Place<F::Dir> for TreePlace<'_, F> {
    fn kind(&self) -> Kind {
        Kind::Tree
    }

    fn destination(&self) -> &Path {
        &self.dest_shown
    }

    fn holder_shown(&self) -> &Path {
        &self.holder_shown
    }

    fn workspace_path(&self, id: &str) -> String {
        format!("operations/{id}")
    }

    fn shown(&self, id: &str) -> PathBuf {
        self.operations_shown().join(id).join(MANIFEST)
    }

    fn scan(&self, own_id: &str) -> LockResult<Scan> {
        match &self.dest {
            Some(dest) => scan_tree(dest, &self.dest_shown, own_id),
            None => Ok(Scan::default()),
        }
    }

    fn create(
        &mut self,
        state: &OperationState,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<(), RunError> {
        if self.dest.is_none() {
            let name = self.name.as_deref().expect("only a named DEST can be absent");
            let dest = match self.holder.create_dir(name) {
                Ok(d) => {
                    self.created_dest = true;
                    d
                }
                Err(e) if e.source.kind() == ErrorKind::AlreadyExists => self
                    .holder
                    .open_dir(name)
                    .map_err(|e| failed(RunStep::State, &self.dest_shown, e))?,
                Err(e) => return Err(failed(RunStep::State, &self.dest_shown, e)),
            };
            self.dest = Some(dest);
        }
        let dest = self.dest.as_ref().expect("made above");
        let operations = operations_dir(dest, &self.dest_shown).map_err(|e| {
            from_lock(e, RunStep::State, &self.operations_shown(), self.created_dest)
        })?;
        let id = state.operation_id.as_str();
        let shown_creating = self.operations_shown().join(creating_name(id));
        let building = begin_workspace(&operations, id)
            .map_err(|e| failed(RunStep::State, &shown_creating, e))?;
        match probe_no_replace(&building) {
            Ok(Probe::Available { leftover: None }) => {}
            Ok(Probe::Available { leftover: Some((name, error)) }) => {
                // The file's path once the workspace is published.
                warnings.push(RunWarning::ProbeNotRemoved {
                    path: self.operations_shown().join(id).join(name),
                    error,
                });
            }
            Ok(Probe::Unavailable { leftover }) => {
                drop(building);
                return Err(self.refuse_no_replace(operations, id, leftover));
            }
            Err((at, error)) => {
                drop(building);
                // Best effort, but a leftover is reported: the CLI prints the warnings of a stopped run too.
                if let Some((path, error)) = self.unwind_creating(operations, id) {
                    warnings.push(RunWarning::NotRemoved { path, error });
                }
                let file = if at == ProbeAt::Create { PROBE_TEMP } else { PROBE };
                return Err(failed(RunStep::Probe, &shown_creating.join(file), error));
            }
        }
        let workspace = publish_workspace(&operations, building, state)
            .map_err(|e| failed(RunStep::State, &self.shown(id), e))?;
        // After the publishing rename: Windows refuses to rename a directory with an open file inside it.
        match workspace.create_claim_store(OsStr::new(STATE_DB), self.durability) {
            Ok(store) => self.claims = Some(store),
            Err(error) => {
                drop(workspace);
                // Retire the published workspace (it holds only its manifest), then the control directories.
                let shown_id = self.operations_shown().join(id);
                let mut not_removed =
                    retire_workspace(&operations, id).err().map(|e| (shown_id.clone(), e));
                self.operations = Some(operations);
                if let Err(first) = self.remove_control_dirs(true) {
                    not_removed.get_or_insert(first);
                }
                if let Some((path, error)) = not_removed {
                    warnings.push(RunWarning::NotRemoved { path, error });
                }
                return Err(failed(RunStep::State, &shown_id.join(STATE_DB), error));
            }
        }
        drop(workspace);
        self.operations = Some(operations);
        Ok(())
    }

    fn write(&self, state: &OperationState) -> flux_fs::Result<()> {
        let workspace = self.operations().open_dir(OsStr::new(&state.operation_id))?;
        write_state(&workspace, OsStr::new(MANIFEST), state)
    }

    fn sweep(
        &self,
        priors: &[PriorOp],
        locked: &Locked<'_, F::Dir>,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<Vec<String>, Fault> {
        let ids: BTreeSet<&str> = priors.iter().map(|p| p.state.operation_id.as_str()).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let dest = self.dest.as_ref().expect("DEST exists once this run's state does");
        let operations = self.operations_shown();
        let mut kept: BTreeSet<&str> = BTreeSet::new();
        // J1: every `<name>.flux-partial.<prior-id>` under DEST, found by a walk of DEST by path that skips the
        // reserved control directories, and deleted through handles opened from DEST's own, one component at a time.
        let events = match walk(self.fs, &self.dest_shown) {
            Ok(events) => events,
            Err(error) => {
                warnings.push(RunWarning::PartialKept {
                    path: self.dest_shown.clone(),
                    error,
                    kept: operations,
                });
                return Ok(Vec::new());
            }
        };
        for item in events {
            let path = match item {
                Ok(WalkEvent::File { path }) => path,
                Ok(_) => continue,
                // A directory the sweep cannot read may hold a partial: every prior's state stays as its record.
                Err(e) => {
                    let path = self.dest_shown.join(&e.path);
                    warnings.push(RunWarning::PartialKept {
                        path,
                        error: e.cause,
                        kept: operations.clone(),
                    });
                    kept.extend(ids.iter().copied());
                    continue;
                }
            };
            if reserved_path(&path) {
                continue;
            }
            let Some(prior) = path
                .file_name()
                .and_then(|n| id_after(n, PARTIAL_INFIX))
                .and_then(|i| ids.get(i).copied())
            else {
                continue;
            };
            checked(locked)?;
            if let Err(error) = remove_below(dest, &path) {
                let shown = self.dest_shown.join(&path);
                warnings.push(RunWarning::PartialKept {
                    path: shown,
                    error,
                    kept: self.shown(prior),
                });
                kept.insert(prior);
            }
        }
        Ok(ids.difference(&kept).map(|i| (*i).to_string()).collect())
    }

    fn remove(&self, id: &str) -> Result<(), (PathBuf, FsError)> {
        retire_workspace(self.operations(), id).map_err(|e| (self.operations_shown().join(id), e))
    }

    fn remove_control_dirs(&mut self, rollback: bool) -> Result<(), (PathBuf, FsError)> {
        // Each handle closes before its directory is removed.
        self.operations = None;
        let dest = self.dest.as_ref().expect("DEST exists once this run's state does");
        remove_empty_control_dirs(dest).map_err(|e| (self.dest_shown.join(FLUX_DIR), e))?;
        if rollback && self.created_dest {
            let name = self.name.as_deref().expect("a DEST this run made has a name");
            self.dest = None;
            self.holder.remove_dir(name).map_err(|e| (self.dest_shown.clone(), e))?;
            self.created_dest = false;
        }
        Ok(())
    }

    fn file_identities(&self) -> Option<(FileIdentity, Option<FileIdentity>)> {
        None
    }

    fn roots(&self) -> Vec<Root> {
        vec![Root {
            source_root: native_hex(&absolute_lexical(&self.src_root)),
            source_identity: identity_text(self.src_identity),
            destination_prefix: String::new(),
        }]
    }

    fn adopt(
        &mut self,
        prior: &OperationState,
        durability: Durability,
    ) -> Result<Option<u64>, RunError> {
        let id = prior.operation_id.as_str();
        let db = self.operations_shown().join(id).join(STATE_DB);
        let dest = self.dest.as_ref().expect("the scan found the prior under DEST");
        let operations = operations_dir(dest, &self.dest_shown)
            .map_err(|e| from_lock(e, RunStep::State, &self.operations_shown(), false))?;
        let workspace = operations
            .open_dir(OsStr::new(id))
            .map_err(|e| failed(RunStep::State, &self.operations_shown().join(id), e))?;
        let name = OsStr::new(STATE_DB);
        let store = match workspace.open_claim_store(name, durability) {
            Ok(store) => store,
            Err(e)
                if (e.source.kind() == ErrorKind::NotFound || e.code == Code::StateCorrupt)
                    && prior.state == OpState::Created =>
            {
                // A crash between the manifest and the store's creation: nothing was claimed. Start the store again.
                match workspace.remove_file(name) {
                    Ok(()) => {}
                    Err(e) if e.source.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(failed(RunStep::State, &db, e)),
                }
                let store = workspace
                    .create_claim_store(name, durability)
                    .map_err(|e| failed(RunStep::State, &db, e))?;
                self.claims = Some(store);
                self.operations = Some(operations);
                return Ok(Some(0));
            }
            Err(e) if e.source.kind() == ErrorKind::NotFound => {
                return Err(refused_state(
                    LockCode::StateCorrupt,
                    format!("{}: the workspace has no state.db; {BY_HAND}", db.display()),
                ));
            }
            Err(e) if e.code == Code::StateCorrupt => {
                return Err(refused_state(
                    LockCode::StateCorrupt,
                    format!("{}: {}; {BY_HAND}", db.display(), e.source),
                ));
            }
            Err(e) if e.code == Code::IncompatibleState => {
                return Err(refused_state(
                    LockCode::IncompatibleState,
                    format!(
                        "{}: {}; use the Flux version that wrote it, or run again with --restart to supersede it",
                        db.display(),
                        e.source
                    ),
                ));
            }
            Err(e) => return Err(failed(RunStep::State, &db, e)),
        };
        let claims = store.count().map_err(|e| failed(RunStep::State, &db, e))?;
        self.claims = Some(store);
        self.operations = Some(operations);
        Ok(Some(claims))
    }
}

/// Remove the file at `rel` below `dest` through handles opened one component at a time, so no link is followed
/// (§149.7; J1).
fn remove_below<D: DirHandle>(dest: &D, rel: &Path) -> flux_fs::Result<()> {
    let mut parts: Vec<&OsStr> = rel.iter().collect();
    let name = parts.pop().expect("a walk path ends in a name");
    let mut opened: Option<D> = None;
    for part in parts {
        let next = opened.as_ref().unwrap_or(dest).open_dir(part)?;
        opened = Some(next);
    }
    opened.as_ref().unwrap_or(dest).remove_file(name)
}

/// A single file's directory: it holds the lock, the record `<target>.flux-state.<id>` and the copy.
pub(crate) struct FilePlace<'p, D: DirHandle> {
    pub(crate) dir: &'p D,
    pub(crate) dir_shown: PathBuf,
    pub(crate) target: OsString,
    /// The destination as the operator gave it.
    pub(crate) destination: PathBuf,
    /// The source's identity, read when the source was opened (cut 7b).
    pub(crate) source_identity: FileIdentity,
    /// The source file as the operator named it (cut 9a: the record's root).
    pub(crate) src: PathBuf,
}

impl<D: DirHandle> Place<D> for FilePlace<'_, D> {
    fn kind(&self) -> Kind {
        Kind::File
    }

    fn destination(&self) -> &Path {
        &self.destination
    }

    fn holder_shown(&self) -> &Path {
        &self.dir_shown
    }

    fn workspace_path(&self, id: &str) -> String {
        format!("adjacent/{id}")
    }

    fn shown(&self, id: &str) -> PathBuf {
        self.dir_shown.join(record_name(&self.target, id))
    }

    fn scan(&self, own_id: &str) -> LockResult<Scan> {
        scan_file(self.dir, &self.target, &self.dir_shown, own_id)
    }

    fn create(
        &mut self,
        state: &OperationState,
        _warnings: &mut Vec<RunWarning>,
    ) -> Result<(), RunError> {
        self.write(state).map_err(|e| failed(RunStep::State, &self.shown(&state.operation_id), e))
    }

    fn write(&self, state: &OperationState) -> flux_fs::Result<()> {
        write_state(self.dir, &record_name(&self.target, &state.operation_id), state)
    }

    fn sweep(
        &self,
        priors: &[PriorOp],
        locked: &Locked<'_, D>,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<Vec<String>, Fault> {
        let mut gone = Vec::new();
        for prior in priors {
            let id = &prior.state.operation_id;
            // `<target>.flux-partial.<id>`, looked up by this target's own name (E3).
            let partial =
                temp_path(Path::new(&self.target), &OperationId::new(id.as_str())).into_os_string();
            checked(locked)?;
            match self.dir.remove_file(&partial) {
                Ok(()) => gone.push(id.clone()),
                Err(e) if e.source.kind() == ErrorKind::NotFound => gone.push(id.clone()),
                Err(error) => warnings.push(RunWarning::PartialKept {
                    path: self.dir_shown.join(&partial),
                    error,
                    kept: prior.shown.clone(),
                }),
            }
        }
        Ok(gone)
    }

    fn remove(&self, id: &str) -> Result<(), (PathBuf, FsError)> {
        remove_record(self.dir, &record_name(&self.target, id)).map_err(|e| (self.shown(id), e))
    }

    fn remove_control_dirs(&mut self, _rollback: bool) -> Result<(), (PathBuf, FsError)> {
        // A single file's state needs no control directory (F2).
        Ok(())
    }

    fn file_identities(&self) -> Option<(FileIdentity, Option<FileIdentity>)> {
        let existing = match self.dir.metadata(&self.target) {
            Ok(m) => Some(m.identity),
            Err(e) if e.source.kind() == ErrorKind::NotFound => None,
            Err(_) => Some(FileIdentity::Unavailable),
        };
        Some((self.source_identity, existing))
    }

    fn roots(&self) -> Vec<Root> {
        vec![Root {
            source_root: native_hex(&absolute_lexical(&self.src)),
            source_identity: identity_text(self.source_identity),
            destination_prefix: native_hex(Path::new(&self.target)),
        }]
    }

    fn adopt(
        &mut self,
        _prior: &OperationState,
        _durability: Durability,
    ) -> Result<Option<u64>, RunError> {
        Ok(None)
    }
}
