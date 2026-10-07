//! The run (cut 7a design, "The run"; Part 3b-1): a copy that holds its destination's target lock from before its
//! state exists until after that state is gone (F5, Q-K), records a versioned state that a later run classifies
//! (§21.1), and with `--restart` supersedes the prior operations it finds. `tree` is the directory copy and `file` the
//! single-file copy.
//!
//! The CLI (Part 3b-2) renders a `Run`: `copy` exactly as it renders a `copy_tree` / `copy_file` result today, then
//! `stop`, then `warnings`. Its exit status: a `stop` of `RunError::Refused { changed: false, .. }` is 3; any other
//! `stop` is 1; with no `stop`, the copy's own rule decides (`exit_code::for_tree` / `for_file`); `warnings` never
//! change it.

mod place;
mod restart;
mod session;
#[cfg(test)]
mod tests;

use crate::copy::{CopyError, copy_file_guarded, no_before_create, prepare_file};
use crate::lock::record::LockRecord;
use crate::lock::site::NAME_LIMIT;
use crate::lock::{LockCode, LockSite, Refusal, Released, check_capability};
use crate::prior::check_control_plane;
use crate::state::{OpState, file_names_fit, identity_text, native_hex};
use crate::tree::{
    Shared, TreeAbort, TreeFailure, TreeFailureCause, TreeOutcome, copy_tree_at, prepare_source,
};
use flux_fs::{
    Code, CopyOptions, DestinationRoot, DirHandle, FileIdentity, FsError, OperationId, Outcome,
};
use place::{FilePlace, Place, TreePlace, locate_tree};
use session::{Locked, beat_error, from_lock, guarded, lock_io, open_operation, owned};
use std::path::{Path, PathBuf};

/// The `workspace_path` a record carries once the state it named is about to go (Q-K): the spec's literal for "names
/// no workspace", which a dead owner's classification treats as recoverable (`lock/site.rs`, `workspace`).
const NO_WORKSPACE: &str = "none";

/// A test hook (Part 3b-2): called before every guarded destination mutation of the copy, before its §99 check. The CLI
/// installs one only in a debug build, to stop a run at a known point and kill it there.
pub type BeforeMutation = std::sync::Arc<dyn Fn() + Send + Sync>;

/// What the operator asked of the run, beyond the copy itself.
#[derive(Clone)]
pub struct RunConfig {
    /// `--restart`: supersede every resumable prior operation.
    pub restart: bool,
    /// `--break-lock`: take an uncertain lock over (§240.5). The CLI allows it only with `--restart` (exit 2).
    pub break_lock: bool,
    /// This operation's id (`ids::new_id`): it names the state, the record and every temporary.
    pub operation_id: String,
    /// This process's id, in the same form.
    pub owner_instance_id: String,
    /// `flux_platform::boot_session_id()`, or `unknown`.
    pub boot_session_id: String,
    /// `None` outside a test.
    pub before_mutation: Option<BeforeMutation>,
    /// §101: how often the run refreshes its lock record's heartbeat (cut 7b). The CLI passes `HEARTBEAT_INTERVAL`.
    pub heartbeat_interval: std::time::Duration,
}

/// §101's heartbeat interval: 5 s.
pub const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

impl std::fmt::Debug for RunConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunConfig")
            .field("restart", &self.restart)
            .field("break_lock", &self.break_lock)
            .field("operation_id", &self.operation_id)
            .field("owner_instance_id", &self.owner_instance_id)
            .field("boot_session_id", &self.boot_session_id)
            .field("before_mutation", &self.before_mutation.is_some())
            .field("heartbeat_interval", &self.heartbeat_interval)
            .finish()
    }
}

/// What a run did. `copy` is `None` when the run stopped before the copy; otherwise it is what `copy_tree` /
/// `copy_file` would return, and a refusal of the source side (B1) arrives there too. `stop` is why the run itself
/// stopped or failed outside the copy. `warnings` are reported and never fail the run (F6).
#[derive(Debug)]
pub struct Run<T> {
    pub copy: Option<T>,
    pub stop: Option<RunError>,
    pub warnings: Vec<RunWarning>,
}

/// Why the run stopped outside the copy.
#[derive(Debug)]
pub enum RunError {
    /// A refusal ("Errors and exit statuses"). `changed` is false when nothing changed, or what the run created was
    /// removed again (exit 3), and true otherwise (exit 1): ownership lost mid-run, or a refusal after this run's
    /// state exists. `not_removed` is something this run created and could not remove while refusing
    /// (spec:2896-2902).
    Refused { refusal: Box<Refusal>, changed: bool, not_removed: Option<(PathBuf, FsError)> },
    /// An I/O failure in one of the run's own steps, at `path` (exit 1).
    Failed { step: RunStep, path: PathBuf, error: FsError },
}

/// The run's own steps, for a failure's message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStep {
    /// Reading the destination's control plane or its prior state (steps 2 and 4).
    Inspect,
    /// Acquiring, classifying, recovering or taking over the lock (step 3).
    Lock,
    /// Creating or rewriting this operation's state (step 5 and its later transitions).
    State,
    /// Writing this operation's lock record (step 5; `--break-lock` step 6).
    Record,
    /// Superseding a prior operation (`--restart`).
    Restart,
    /// Removing what a refused copy left (Q-I).
    Rollback,
    /// The section 241.5 no-replace probe inside the unpublished workspace (cut 8a, Part P).
    Probe,
}

impl RunStep {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inspect => "inspecting the destination's state",
            Self::Lock => "obtaining the destination's lock",
            Self::State => "writing the operation's state",
            Self::Record => "writing the lock record",
            Self::Restart => "superseding a prior operation",
            Self::Rollback => "removing what the refused copy left",
            Self::Probe => "probing the destination for no-replace publication",
        }
    }
}

/// Reported, never a failure (F6): the exit status stays what the copy decided.
#[derive(Debug)]
pub enum RunWarning {
    /// §240.3 recovery left the dead owner's lock here, moved aside (the crash table's `.broken` row).
    BrokenLeftover(PathBuf),
    /// Part of this run's own state, an empty control directory, or the lock could not be removed (F6).
    NotRemoved { path: PathBuf, error: FsError },
    /// A temporary the copy could not remove keeps this COMPLETED state as its record (G2).
    StateKept(PathBuf),
    /// A superseded operation's partial at `path` could not be deleted; the ABANDONED state at `kept` stays as its
    /// record (J1).
    PartialKept { path: PathBuf, error: FsError, kept: PathBuf },
    /// Ownership was lost after COMPLETED (finish step 5): the lock was closed without unlinking.
    OwnershipLostAfterCompletion(PathBuf),
    /// A file of the no-replace probe could not be removed (Part P); it goes with the operation's state.
    ProbeNotRemoved { path: PathBuf, error: FsError },
}

/// A directory copy under the destination's lock ("The run").
pub fn tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    cfg: &RunConfig,
    on_report: &mut dyn FnMut(TreeFailure),
) -> Run<Result<TreeOutcome, TreeAbort>> {
    let mut run = Run { copy: None, stop: None, warnings: Vec::new() };
    let opts =
        CopyOptions { operation_id: OperationId::new(cfg.operation_id.as_str()), ..opts.clone() };
    let mut out = TreeOutcome::default();
    // B1: the source side, then DEST and the identity pre-flight, before anything is created.
    let source = match prepare_source(fs, src_root, dst_root) {
        Ok(s) => s,
        Err(error) => {
            run.copy = Some(Err(TreeAbort { error, outcome: out }));
            return run;
        }
    };
    let located = match locate_tree(fs, src_root, dst_root, source.identity, opts.safety, &mut out)
    {
        Ok(l) => l,
        Err(error) => {
            run.copy = Some(Err(TreeAbort { error, outcome: out }));
            return run;
        }
    };
    let holder = located.holder;
    // Step 1: the lock's filesystem (decision 14).
    let capability = match check_capability(&holder) {
        Ok(c) => c,
        Err(e) => {
            run.stop = Some(from_lock(e, RunStep::Lock, &located.holder_shown, false));
            return run;
        }
    };
    // Step 2: DEST's control plane, where DEST exists.
    if let Some(dest) = &located.dest
        && let Err(e) = check_control_plane(dest, dst_root)
    {
        run.stop = Some(from_lock(e, RunStep::Inspect, dst_root, false));
        return run;
    }
    let site = match &located.name {
        Some(name) => LockSite::directory(&holder, name),
        None => Ok(LockSite::root(&holder)),
    };
    let site = match site {
        Ok(s) => s,
        Err(e) => {
            run.stop = Some(from_lock(e, RunStep::Lock, &located.holder_shown, false));
            return run;
        }
    };
    let mut place = TreePlace {
        fs,
        holder: &holder,
        holder_shown: located.holder_shown,
        name: located.name,
        dest: located.dest,
        dest_shown: dst_root.to_path_buf(),
        created_dest: false,
        operations: None,
        claims: None,
        durability: opts.durability,
    };
    // Steps 3-5 (and `--restart`): the lock, then this operation's state and the record naming it.
    let locked = match open_operation(&site, capability, &mut place, cfg, &mut run.warnings) {
        Ok(l) => l,
        Err(e) => {
            run.stop = Some(e);
            return run;
        }
    };
    // Step 6: the copy, with §99 before every destination mutation.
    let mut leftovers: Vec<PathBuf> = Vec::new();
    let walked = {
        let guard = || {
            if let Some(hook) = &cfg.before_mutation {
                hook();
            }
            guarded(&locked.held, &locked.pulse)
        };
        let beat =
            || locked.pulse.beat(&locked.held).map_err(|e| beat_error(&locked.lock_shown, e));
        // The store lives only for the walk: the cell is dropped at the end of this block, before `finish`
        // removes the file.
        let claims = place.claims.take().map(std::cell::RefCell::new);
        let cx = Shared {
            fs,
            src_root,
            src_identity: source.identity,
            opts: &opts,
            guard: &guard,
            beat: &beat,
            claims: claims.as_ref(),
        };
        let root = place.dest.take().expect("step 5 made DEST");
        let mut report = |f: TreeFailure| {
            if let TreeFailureCause::Copy(e) = &f.cause
                && let Some((path, _)) = &e.leftover
            {
                leftovers.push(path.clone());
            }
            on_report(f);
        };
        let (root, walked) = copy_tree_at(&cx, source.events, root, &mut out, &mut report);
        place.dest = Some(root);
        walked
    };
    let mut result = match walked {
        Ok(()) => Ok(out),
        Err(error) => Err(TreeAbort { error, outcome: out }),
    };
    let ended = match &result {
        Ok(_) => Ended::Completed { leftovers, published: None },
        // Cut 7b: a failed heartbeat is the copy's failure, whatever its code.
        Err(_) if locked.pulse.failed() => Ended::Failed,
        Err(a) if a.error.code() == Code::TargetLockBusy => Ended::Lost,
        Err(a) if a.refused_unchanged() => Ended::RefusedUnchanged,
        Err(_) => Ended::Failed,
    };
    run.stop = finish(&mut place, locked, ended, &mut run.warnings);
    // E1: a DEST this run made counts among the directories it created, unless a rollback removed it again.
    if place.created_dest {
        match &mut result {
            Ok(o) => o.directories_created += 1,
            Err(a) => a.outcome.directories_created += 1,
        }
    }
    run.copy = Some(result);
    run
}

/// A single-file copy under the destination's lock ("The run"; its state is the record beside the target, F2).
pub fn file<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    dst: &Path,
    opts: &CopyOptions,
    cfg: &RunConfig,
) -> Run<Result<Outcome, CopyError>> {
    let mut run = Run { copy: None, stop: None, warnings: Vec::new() };
    let opts =
        CopyOptions { operation_id: OperationId::new(cfg.operation_id.as_str()), ..opts.clone() };
    // B1.
    let (parent, parent_path, name, source_identity) = match prepare_file(fs, src, dst, &opts) {
        Ok(p) => p,
        Err(e) => {
            run.copy = Some(Err(e));
            return run;
        }
    };
    // Every name the run makes beside the target must fit (Part 3a decision 4), before anything is made.
    if !file_names_fit(name) {
        run.stop = Some(RunError::Refused {
            refusal: Box::new(Refusal {
                code: LockCode::PathComponentInvalid,
                holder: None,
                detail: format!(
                    "the target name {} leaves no room for the names Flux keeps beside it (its state record's temporary is the name plus 48 units, over {NAME_LIMIT})",
                    name.to_string_lossy()
                ),
            }),
            changed: false,
            not_removed: None,
        });
        return run;
    }
    // Step 1.
    let capability = match check_capability(&parent) {
        Ok(c) => c,
        Err(e) => {
            run.stop = Some(from_lock(e, RunStep::Lock, parent_path, false));
            return run;
        }
    };
    let site = match LockSite::file(&parent, name) {
        Ok(s) => s,
        Err(e) => {
            run.stop = Some(from_lock(e, RunStep::Lock, parent_path, false));
            return run;
        }
    };
    let mut place = FilePlace {
        dir: &parent,
        dir_shown: parent_path.to_path_buf(),
        target: name.to_os_string(),
        destination: dst.to_path_buf(),
        source_identity,
    };
    // Steps 3-5.
    let locked = match open_operation(&site, capability, &mut place, cfg, &mut run.warnings) {
        Ok(l) => l,
        Err(e) => {
            run.stop = Some(e);
            return run;
        }
    };
    // Step 6, under §99.
    let copied = {
        let guard = || {
            if let Some(hook) = &cfg.before_mutation {
                hook();
            }
            guarded(&locked.held, &locked.pulse)
        };
        let beat =
            || locked.pulse.beat(&locked.held).map_err(|e| beat_error(&locked.lock_shown, e));
        copy_file_guarded(fs, src, &parent, name, &opts, &guard, &beat, &no_before_create)
    }
    .map_err(|mut e| {
        // As `copy_file` reports it: the leftover in the frame of the path the operator gave.
        if let Some((path, _)) = e.leftover.as_mut() {
            *path = dst.with_file_name(&*path);
        }
        e
    });
    let ended = match &copied {
        // A skipped file published nothing, so it records no identity.
        Ok(o) => Ended::Completed {
            leftovers: Vec::new(),
            published: (!o.skipped).then_some(o.published_identity),
        },
        // Cut 7b: a failed heartbeat is the copy's failure, whatever its code.
        Err(_) if locked.pulse.failed() => Ended::Failed,
        Err(e) if e.code() == Code::TargetLockBusy => Ended::Lost,
        Err(e) if e.code() == Code::SafetyRejected && e.leftover.is_none() => {
            Ended::RefusedUnchanged
        }
        Err(_) => Ended::Failed,
    };
    run.stop = finish(&mut place, locked, ended, &mut run.warnings);
    run.copy = Some(copied);
    run
}

/// How the copy ended, as the finish needs it.
enum Ended {
    /// The walk finished, or the single file was copied (A1). `leftovers`: the temporaries the copy could not remove,
    /// relative to DEST (a tree). `published`: a single file's published target (cut 7b).
    Completed { leftovers: Vec<PathBuf>, published: Option<FileIdentity> },
    /// The copy stopped because this run no longer owns the lock.
    Lost,
    /// The copy refused with nothing changed (Q-I).
    RefusedUnchanged,
    /// Any other failure of the copy.
    Failed,
}

/// Step 7, "Finish".
fn finish<D: DirHandle, P: Place<D>>(
    place: &mut P,
    locked: Locked<'_, D>,
    ended: Ended,
    warnings: &mut Vec<RunWarning>,
) -> Option<RunError> {
    match ended {
        Ended::Completed { leftovers, published } => {
            complete(place, locked, leftovers, published, warnings)
        }
        // Ownership lost before COMPLETED (decision 13): the state stays as it is, and `locked` drops here, closing
        // the lock without unlinking it (`S99_refuse_close`). The copy's own TARGET_LOCK_BUSY is the report.
        Ended::Lost => None,
        Ended::RefusedUnchanged => rollback(place, locked),
        Ended::Failed => fail(place, locked),
    }
}

/// The success path: COMPLETED, durably and while still owned; then, unless a leftover temporary keeps it (G2; the
/// COMPLETED write carries `cleanup_pending` and the leftovers (cut 7b)), the
/// record stops naming the state (Q-K), the state goes, and the empty control directories; then the release. After
/// COMPLETED nothing may fail the run (F6, spec:9363): each failure from here is a warning.
fn complete<D: DirHandle, P: Place<D>>(
    place: &mut P,
    mut locked: Locked<'_, D>,
    leftovers: Vec<PathBuf>,
    published: Option<FileIdentity>,
    warnings: &mut Vec<RunWarning>,
) -> Option<RunError> {
    if let Err(fault) = owned(&locked.held, &locked.lock_shown) {
        return Some(fault.into_error(RunStep::State));
    }
    // §218 (cut 7b decision 10): cleanup_pending in this same COMPLETED write, before anything is removed, with what
    // the copy left; a single file also records its published target. Built as a copy and adopted only once written:
    // if the write fails, `fail` writes FAILED from the state as it was, never a FAILED record carrying the COMPLETED
    // write's `cleanup_pending`, which the next run would refuse as STATE_CORRUPT (capstone r4).
    let mut done = locked.state.clone();
    done.state = OpState::Completed;
    if let Some(c) = &mut done.cleanup {
        c.cleanup_pending = true;
        c.cleanup_pending_artifacts = leftovers.iter().map(|p| native_hex(p)).collect();
    }
    if let (Some(f), Some(id)) = (&mut done.file, published) {
        f.target_identity = Some(identity_text(id));
    }
    if let Err(error) = place.write(&done) {
        let path = place.shown(&locked.state.operation_id);
        return Some(stop_after_record(place, locked, RunStep::State, path, error));
    }
    locked.state = done;
    if !leftovers.is_empty() {
        warnings.push(RunWarning::StateKept(place.shown(&locked.state.operation_id)));
    } else if let Err((path, error)) = remove_own(place, &mut locked, false) {
        warnings.push(RunWarning::NotRemoved { path, error });
    }
    // Steps 5-6: `S99_release_check`, then `S99_release` - or `S99_refuse_close`.
    match locked.held.release() {
        Ok(Released::Unlinked) => {}
        Ok(Released::NotOwned) => {
            warnings.push(RunWarning::OwnershipLostAfterCompletion(locked.lock_shown));
        }
        Err(e) => {
            warnings.push(RunWarning::NotRemoved { path: locked.lock_shown, error: lock_io(e) })
        }
    }
    None
}

/// Q-K, then finish steps 3-4, or a rollback's removals: the record stops naming this operation's state before the
/// state goes, so a crash part-way never leaves a dead owner's record naming missing state.
fn remove_own<D: DirHandle, P: Place<D>>(
    place: &mut P,
    locked: &mut Locked<'_, D>,
    rollback: bool,
) -> Result<(), (PathBuf, FsError)> {
    let record = locked.held.record().expect("written at step 5").clone();
    let none = LockRecord { workspace_path: NO_WORKSPACE.to_string(), ..record };
    if let Err(e) = locked.held.rewrite_record(none) {
        return Err((locked.lock_shown.clone(), lock_io(e)));
    }
    place.remove(&locked.state.operation_id)?;
    place.remove_control_dirs(rollback)
}

/// The failure path: FAILED (resumable: the next run gets RESUMABLE_OPERATION_EXISTS until `--restart`), then the
/// release "the same way". The copy's own error is the report; a failure here is reported beside it. After a failed
/// heartbeat, a failed check ends it with nothing written (cut 7b).
fn fail<D: DirHandle, P: Place<D>>(place: &P, mut locked: Locked<'_, D>) -> Option<RunError> {
    if let Err(fault) = owned(&locked.held, &locked.lock_shown) {
        // Cut 7b decision 7, "Torn": after a failed heartbeat, a record that no longer proves ownership is most likely
        // the one the heartbeat tore. That failure is already the report: write nothing, add no stop, and close without
        // unlinking as `locked` drops. The next run meets TARGET_LOCK_UNCERTAIN, which `--restart --break-lock` clears.
        if locked.pulse.failed() {
            return None;
        }
        return Some(fault.into_error(RunStep::State));
    }
    locked.state.state = OpState::Failed;
    let stop = place.write(&locked.state).err().map(|error| RunError::Failed {
        step: RunStep::State,
        path: place.shown(&locked.state.operation_id),
        error,
    });
    match locked.held.release() {
        Ok(_) => stop,
        Err(e) => stop.or(Some(RunError::Failed {
            step: RunStep::State,
            path: locked.lock_shown,
            error: lock_io(e),
        })),
    }
}

/// A failure of the run's own step after the record exists (decision 12): FAILED and the release, best effort. The
/// first failure is the report; a second one here leaves what a crash at this point leaves, which the next run
/// classifies.
fn stop_after_record<D: DirHandle, P: Place<D>>(
    place: &P,
    locked: Locked<'_, D>,
    step: RunStep,
    path: PathBuf,
    error: FsError,
) -> RunError {
    let _second = fail(place, locked);
    RunError::Failed { step, path, error }
}

/// Q-I: the copy refused with nothing changed, after this run's state exists. What the run created is removed again -
/// Q-K first, then the state, the control directories it emptied and a DEST it made - and the lock released, so the
/// copy's refusal keeps exit 3 (§55: created and removed again does not count). A failure here is exit 1, naming the
/// path; the lock is still released, and whatever state remains is what a crash at that point leaves.
fn rollback<D: DirHandle, P: Place<D>>(
    place: &mut P,
    mut locked: Locked<'_, D>,
) -> Option<RunError> {
    if let Err(fault) = owned(&locked.held, &locked.lock_shown) {
        return Some(fault.into_error(RunStep::Rollback));
    }
    if let Err((path, error)) = remove_own(place, &mut locked, true) {
        // `release` unlinks only a lock that is still this run's; its own failure leaves a dead owner's lock.
        let _released = locked.held.release();
        return Some(RunError::Failed { step: RunStep::Rollback, path, error });
    }
    match locked.held.release() {
        Ok(Released::Unlinked) => None,
        Ok(Released::NotOwned) => Some(session::lost()),
        Err(e) => Some(RunError::Failed {
            step: RunStep::Rollback,
            path: locked.lock_shown,
            error: lock_io(e),
        }),
    }
}
