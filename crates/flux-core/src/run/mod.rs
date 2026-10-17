//! The run (cut 7a design, "The run"; Part 3b-1): a copy that holds its destination's target lock from before its
//! state exists until after that state is gone (F5, Q-K), records a versioned state that a later run classifies
//! (§21.1), and with `--restart` supersedes the prior operations it finds. `tree` is the directory copy and `file` the
//! single-file copy.
//!
//! The CLI (Part 3b-2) renders a `Run`: `copy` exactly as it renders a `copy_tree` / `copy_file` result today, then
//! `stop`, then `warnings`. Its exit status: a `stop` of `RunError::Refused { changed: false, .. }` is 3; any other
//! `stop` is 1; with no `stop`, the copy's own rule decides (`exit_code::for_tree` / `for_file`); `warnings` never
//! change it.

mod dry_prior;
pub(crate) mod place;
mod recover;
mod restart;
mod resume;
pub(crate) mod session;
pub(crate) mod sweep;
#[cfg(test)]
mod tests;

pub use crate::checkpoint::ResumeVerify;
use crate::checkpoint::{CheckpointOpenError, CheckpointPolicy, Checkpoints, Hook};
use crate::copy::{
    CopyError, CopyStep, FilePlan, copy_file_checkpointed, copy_file_guarded, no_before_create,
    no_before_publish, no_heartbeat, plan_file, prepare_file, unguarded,
};
use crate::dry_run::{
    DryPrior, DryRun, NullClaimStore, PlannedAction, PlannedEntry, PlannedObject,
};
use crate::lock::record::LockRecord;
use crate::lock::site::NAME_LIMIT;
use crate::lock::{LockCode, LockSite, Refusal, Released, check_capability};
use crate::prior::{check_control_plane, resumable_refusal};
use crate::state::{OpState, file_names_fit, identity_text, native_hex};
use crate::tree::{
    RootIdentity, Shared, Source, TreeAbort, TreeFailure, TreeFailureCause, TreeOutcome,
    copy_tree_at, copy_tree_frames, prepare_source, report,
};
use crate::walk::MountPolicy;
use dry_prior::{LockPreview, dry_lock};
use flux_fs::{
    ClaimStore, Code, CopyOptions, DestinationRoot, DirHandle, FileIdentity, FsError,
    LockCapability, OperationId, Outcome,
};
use place::{FilePlace, Place, TreePlace, locate_tree};
use session::{Locked, beat_error, from_lock, guarded, lock_io, open_operation, owned};
use std::ffi::OsStr;
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
    /// `--resume`: continue the one resumable prior operation as itself.
    pub resume: bool,
    /// Cut 9e: the process's soft descriptor limit (`flux_platform::soft_descriptor_limit()`), which caps the writers a
    /// batched tree holds open; `None` when unknown (Windows) or in a test that does not care.
    pub descriptor_limit: Option<u64>,
    /// Cut 9f-2: how much of a checkpointed file a resume re-reads (`--resume-verify`).
    pub resume_verify: ResumeVerify,
    /// `--dry-run` (spec cut 11a; D6): plan the copy, change nothing.
    pub dry_run: bool,
    /// Cut 11b: where a dry resume copies the prior's claim store; `None` means `std::env::temp_dir()`; tests pass a
    /// fake directory.
    pub dry_temp_root: Option<PathBuf>,
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
            .field("resume", &self.resume)
            .field("descriptor_limit", &self.descriptor_limit)
            .field("resume_verify", &self.resume_verify)
            .field("dry_run", &self.dry_run)
            .field("dry_temp_root", &self.dry_temp_root)
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
    /// Cut 9a: set once the run adopted or started under `--resume`; `None` without the flag or when the run stopped
    /// before step 5.
    pub resumed: Option<ResumeNote>,
    /// Cut 9e: the longest gap between two heartbeat writes of the lock record (known limit 6); `Duration::ZERO` when
    /// the run never held the lock.
    pub beat_gap_max: std::time::Duration,
    /// Cut 9g: what a single-file resume reused and discarded; `None` unless an ADOPTED single-file run reused a
    /// prefix of its partial or discarded one, and always `None` on a tree or on a failed copy.
    pub file_resume: Option<FileResume>,
    /// Cut 11a: `Some` only for a dry run.
    pub dry: Option<crate::dry_run::DryRun>,
}

/// Cut 9g: what a single-file resume reused and discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResume {
    pub reused: u64,
    pub recopied: u64,
    pub discard: Option<crate::Discard>,
}

/// What `--resume` did (cut 9a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeNote {
    /// `--resume` found no resumable prior: a new operation started.
    StartedNew,
    /// The prior adopted; `claims` is its claim count (a tree), `None` for a single file. `recovered` is the number of
    /// interrupted publications commit recovery found published (cut 9c, RENAMED notes only; 0 for a single file).
    Adopted { operation_id: String, claims: Option<u64>, recovered: u64 },
}

/// Why the run stopped outside the copy.
#[derive(Debug)]
pub enum RunError {
    /// A refusal ("Errors and exit statuses"). `changed` is false when nothing changed, or what the run created was
    /// removed again (exit 3), and true otherwise (exit 1): ownership lost mid-run, or a refusal after this run's
    /// state exists. `not_removed` is something this run created and could not remove while refusing
    /// (spec:2896-2902).
    Refused { refusal: Box<Refusal>, changed: bool, not_removed: Option<(PathBuf, FsError)> },
    /// An I/O failure in one of the run's own steps, at `path` (exit 1). `not_removed` is something this run created
    /// and could not remove while failing.
    Failed { step: RunStep, path: PathBuf, error: FsError, not_removed: Option<(PathBuf, FsError)> },
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
    /// Cut 9b: finishing an earlier COMPLETED operation's cleanup, or removing workspace debris.
    PriorCleanup,
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
            Self::PriorCleanup => "finishing an earlier operation's cleanup",
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
    /// Cut 9a: the source root's identity could not decide the mapping check (a side is not Strong), so the
    /// stored path decided, byte for byte. `path` is the source root this run names.
    ResumeMappingByPath(PathBuf),
    /// Cut 9b: an earlier COMPLETED operation's recorded cleanup was finished (`removed` leftovers, then its state).
    PriorCleaned { operation_id: String, removed: u64 },
    /// Cut 9b: an earlier operation's cleanup could not be finished; `path` is the first thing kept, with its error.
    PriorCleanupFailed { operation_id: String, path: PathBuf, error: FsError },
    /// Cut 9f-1: the checkpoint log's segment cannot be reopened safely on this filesystem (its identity is
    /// unavailable); the tree ran without checkpoints, so every large file recopied.
    CheckpointsUnavailable { why: String },
    /// Cut 9f-1: the checkpoint log's handles could not be opened, or reading it failed with an I/O error; the tree ran
    /// without checkpoints, so every large file recopied. (Damage is `WalQuarantined`; an unknown format stops the run.)
    CheckpointsUnreadable { why: String },
    /// Cut 9f-1: the checkpoint log ended in `len` bytes at `offset` that the replay cut (spec decision 7.1);
    /// the records before `offset` are intact and were used.
    WalTailDiscarded { offset: u64, len: u64, reason: &'static str },
    /// Cut 9f-1, owner ruling 23: the checkpoint log was damaged mid-log (`code` is `WAL_CORRUPT` or
    /// `WAL_SEQUENCE_CONFLICT`; `why` is the replay's text). It was renamed to `wal/{set_aside_as}` and a fresh log
    /// was started; none of the old log's checkpoints were used, so every large file recopied.
    WalQuarantined { code: &'static str, why: String, set_aside_as: String },
    /// Cut 9f-1, owner ruling 23: as `WalQuarantined`, but the damaged log could not be set aside (`error`), so the
    /// tree ran without checkpoints.
    WalNotQuarantined { code: &'static str, why: String, error: String },
    /// Cut 11a: a dry run found a resumable prior operation at DEST (cut 11b previews what the invocation does with it;
    /// this note is printed by a plain dry run before its predicted refusal).
    DryRunPrior { operation_id: String, state: OpState },
    /// Cut 11a: a dry run found an earlier COMPLETED operation; a real run would finish its recorded cleanup.
    DryRunPriorCompleted { operation_id: String },
    /// Cut 11a: a dry run could not read the prior operations' records; `detail` is `CODE: text`.
    DryRunPriorUnreadable { detail: String },
    /// Cut 11b: a dry run read the lock record but did not probe its owner's liveness; `heartbeat_age_secs` is `None`
    /// when the heartbeat lies in the future.
    DryRunLockRecord {
        operation_id: String,
        owner_instance_id: String,
        boot_session_id: String,
        heartbeat_age_secs: Option<u64>,
        workspace_path: String,
    },
    /// Cut 11b: a dry `--restart --break-lock` would take an uncertain lock over; `why` says what made it uncertain.
    DryRunLockTakeover { why: String },
    /// Cut 11b: a dry run found a broken leftover (`name`) beside the state that a real run would remove.
    DryRunBrokenLeftover { name: String },
    /// Cut 11b: a dry run found debris (`name`) in the state directory that a real run would remove.
    DryRunDebris { name: String },
    /// Cut 11b: a dry resume found the prior's claim store freshly created (empty), so it has no claims to count.
    DryRunStoreFresh { operation_id: String },
    /// Cut 11b: a dry resume found the prior's claim store torn, so its claims were not counted.
    DryRunStoreTorn,
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
    tree_with(fs, src_root, dst_root, opts, cfg, on_report, checkpoint_policy(cfg, opts.durability))
}

/// `tree` with the checkpoint policy given (cut 9f-1): the test seam, so a test can checkpoint small files.
pub(crate) fn tree_with<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    cfg: &RunConfig,
    on_report: &mut dyn FnMut(TreeFailure),
    policy: CheckpointPolicy,
) -> Run<Result<TreeOutcome, TreeAbort>> {
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
    // B1: the source side, then DEST and the identity pre-flight, before anything is created.
    let source = match prepare_source(fs, src_root, dst_root, MountPolicy::of(&opts)) {
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
        src_root: src_root.to_path_buf(),
        src_identity: source.identity,
    };
    // Cut 11a: a dry run plans the walk and touches nothing: no lock, no state, no claim store, no WAL, no finish.
    if cfg.dry_run {
        return dry_tree(run, &mut place, &site, capability, source, cfg, &opts, on_report, out);
    }
    // Steps 3-5 (and `--restart`): the lock, then this operation's state and the record naming it.
    let locked = match open_operation(&site, capability, &mut place, cfg, &opts, &mut run.warnings)
    {
        Ok(l) => l,
        Err(e) => {
            run.stop = Some(e);
            return run;
        }
    };
    run.resumed = locked.resumed.clone();
    run.beat_gap_max = locked.pulse.gap_max();
    // Cut 9a: under `--resume` the effective id is the prior's; the partials are named by it.
    let opts = CopyOptions { operation_id: OperationId::new(&locked.state.operation_id), ..opts };
    // Cut 9f-1: the checkpoint log, opened from two handles on this operation's workspace, below, once the guard and beat
    // closures exist (owner ruling 23: setting a damaged log aside is a destination mutation, so it runs under them).
    // A damaged log never aborts a copy, except an unsupported format (a newer Flux wrote it) and a lost lock. In a
    // `RefCell` because the guard and beat closures read it for the whole walk while the end of the run must take it by
    // value to `finish` or `abandon` it.
    let ckpt: std::cell::RefCell<Option<Checkpoints<F::Dir>>> = std::cell::RefCell::new(None);
    // Step 6: the copy, with §99 before every destination mutation.
    let mut leftovers: Vec<PathBuf> = Vec::new();
    // The closures flag the log the moment either detects a loss: from then on no NEW flush begins (a flush already in
    // flight may complete or tear; a loss not yet detected is outside this).
    let flagged = |e| {
        if let Some(c) = &*ckpt.borrow() {
            c.mark_abandoned();
        }
        e
    };
    let guard = || {
        if let Some(hook) = &cfg.before_mutation {
            hook();
        }
        guarded(&locked.held, &locked.pulse).map_err(flagged)
    };
    let beat = || {
        locked
            .pulse
            .beat(&locked.held)
            .map_err(|e| beat_error(&locked.lock_shown, e))
            .map_err(flagged)
    };
    let opened = open_checkpoints(&place, &locked.state, policy, &mut run.warnings, &guard, &beat);
    let mut stopped_early = None;
    match opened {
        Ok(c) => *ckpt.borrow_mut() = c,
        Err(error) => stopped_early = Some(error),
    }
    // The store stays open past the walk, for the final sync; it is dropped before `finish` removes the file.
    let claims = place.claims.take().map(std::cell::RefCell::new);
    let walked = if let Some(error) = stopped_early {
        // The log could not be opened (or set aside) under the run's lock: nothing is copied.
        Err(error)
    } else {
        let held = ckpt.borrow();
        let cx = Shared {
            fs,
            src_root,
            src_identity: source.identity,
            opts: &opts,
            guard: &guard,
            beat: &beat,
            claims: claims.as_ref().map(|c| c as &std::cell::RefCell<dyn flux_fs::ClaimStore>),
            resume: cfg.resume,
            batch: batch_policy(cfg),
            ckpt: held.as_ref(),
            dry_run: false,
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
        let (root, walked) =
            copy_tree_at(&cx, source.events, root, place.created_dest, &mut out, &mut report);
        place.dest = Some(root);
        walked
    };
    let mut result = match walked {
        Ok(()) => Ok(out),
        Err(error) => Err(TreeAbort { error, outcome: out }),
    };
    // Cut 9f-1 (the guard and beat closures already flagged the log at the moment they detected a loss): the WAL thread
    // ends BEFORE anything else: its directory handles must be closed before `finish` may
    // remove the workspace (a held handle blocks the rename on Windows). A lost lock or a failed heartbeat abandons the
    // log (no NEW flush begins once the loss was detected: the closures flagged the log; a flush already in flight may complete or tear, and a loss not yet detected is outside this); every other ending sends `Stop`, which flushes, and joins.
    if let Some(run_ckpt) = ckpt.take() {
        let lost = matches!(&result, Err(a) if a.error.code() == Code::TargetLockBusy)
            || locked.pulse.failed();
        if lost {
            run_ckpt.abandon();
        } else {
            let failure = run_ckpt.finish(&beat);
            // `finish` swallows a heartbeat error from its final `Stop` (the pulse records it), so a failed pulse
            // after it still makes this run Failed, never Completed.
            if let Some(fault) = failure.wal
                && let Ok(outcome) = &mut result
            {
                let why = &fault.message;
                let cause = FsError::new(
                    Code::WalWriteFailed,
                    std::io::Error::other(format!(
                        "checkpoints could not be written ({why}); resume state is NOT trustworthy"
                    )),
                );
                let error =
                    CopyError { cause, leftover: None, step: crate::copy::CopyStep::Checkpoint };
                result = Err(TreeAbort { error, outcome: std::mem::take(outcome) });
            }
        }
    }
    let ended = match &result {
        // Cut 9f-1: a heartbeat that failed during the final `Stop` ends the run Failed even though the copy was Ok.
        Ok(_) if locked.pulse.failed() => Ended::Failed,
        Ok(_) => Ended::Completed { leftovers, published: None },
        // Cut 7b: a failed heartbeat is the copy's failure, whatever its code.
        Err(_) if locked.pulse.failed() => Ended::Failed,
        Err(a) if a.error.code() == Code::TargetLockBusy => Ended::Lost,
        Err(a) if a.refused_unchanged() => Ended::RefusedUnchanged,
        Err(_) => Ended::Failed,
    };
    // Cut 8b, "Claim syncing", the explicit final sync: the claims written since the last directory end, only when the
    // workspace is KEPT. NOTE: closing the redb store below is itself a commit and an fsync of `state.db` on EVERY
    // path (clean completion, refusal and lost lock included), and under `Normal` it makes earlier `None` commits
    // durable. Harmless (it is this run's own workspace file) but not avoidable: redb has no non-committing close.
    if let Some(claims) = &claims {
        match &ended {
            // Best effort: the heartbeat intact and ownership held; its error never replaces the primary one.
            Ended::Failed => {
                if !locked.pulse.failed() && owned(&locked.held, &locked.lock_shown).is_ok() {
                    let _ignored = claims.borrow_mut().flush();
                }
            }
            // A failure is reported, and counted before `finish`.
            Ended::Completed { leftovers, .. } if !leftovers.is_empty() => {
                let synced =
                    beat().and_then(|()| guard()).and_then(|()| claims.borrow_mut().flush());
                if let Err(e) = synced {
                    let outcome = match &mut result {
                        Ok(o) => o,
                        Err(a) => &mut a.outcome,
                    };
                    report(
                        outcome,
                        on_report,
                        PathBuf::new(),
                        TreeFailureCause::ClaimNotRecorded(e),
                    );
                }
            }
            _ => {}
        }
    }
    drop(claims);
    // Cut 9e: the barrier's beats happen during the walk, so the value taken at the lock is stale by now.
    run.beat_gap_max = locked.pulse.gap_max();
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
    file_with(fs, src, dst, opts, cfg, checkpoint_policy(cfg, opts.durability))
}

/// `file` with the checkpoint policy given (cut 9g): the test seam, as `tree_with` is for `tree`.
pub(crate) fn file_with<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    dst: &Path,
    opts: &CopyOptions,
    cfg: &RunConfig,
    policy: CheckpointPolicy,
) -> Run<Result<Outcome, CopyError>> {
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
        src: src.to_path_buf(),
    };
    // Cut 11a: a dry run plans the one file and touches nothing.
    if cfg.dry_run {
        return dry_file(run, fs, &place, &site, capability, src, cfg, &opts);
    }
    // Steps 3-5.
    let locked = match open_operation(&site, capability, &mut place, cfg, &opts, &mut run.warnings)
    {
        Ok(l) => l,
        Err(e) => {
            run.stop = Some(e);
            return run;
        }
    };
    run.resumed = locked.resumed.clone();
    let opts = CopyOptions { operation_id: OperationId::new(&locked.state.operation_id), ..opts };
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
        // Cut 9g: only an adopted prior can have a partial worth continuing, and only a source the policy would
        // checkpoint (ruling P2: the same `min_len` gate as the tree's files).
        let log_free = matches!(locked.resumed, Some(ResumeNote::Adopted { .. }))
            && fs.metadata(src).is_ok_and(|m| m.len >= policy.min_len);
        if log_free {
            copy_file_checkpointed(
                fs,
                src,
                &parent,
                name,
                &opts,
                &guard,
                &beat,
                &no_before_create,
                &no_before_publish,
                Some(Hook::LogFree { min_len: policy.min_len }),
            )
            .map(|(outcome, reuse)| {
                if reuse.reused > 0 || reuse.discard.is_some() {
                    run.file_resume = Some(FileResume {
                        reused: reuse.reused,
                        recopied: outcome.bytes_copied,
                        discard: reuse.discard,
                    });
                }
                outcome
            })
        } else {
            copy_file_guarded(
                fs,
                src,
                &parent,
                name,
                &opts,
                &guard,
                &beat,
                &no_before_create,
                &no_before_publish,
            )
        }
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

/// Cut 9e: how often the sync barrier's waiting thread wakes to beat: half the heartbeat interval, because
/// `Pulse::beat` writes only once a full interval has passed since its last write, so a wake every full interval would
/// write only every other time. Clamped to 1 ms so a zero interval cannot make the wait a busy loop.
fn barrier_beat_every(cfg: &RunConfig) -> std::time::Duration {
    (cfg.heartbeat_interval / 2).max(std::time::Duration::from_millis(1))
}

/// Cut 9d: the batch policy of a tree run, `BatchPolicy::DEFAULT` with cut 9e's `beat_every` from the run's heartbeat
/// interval (`barrier_beat_every`). In a debug build, `FLUX_TEST_BATCH_AGE_MS=<n>`
/// replaces its age (absent or unparsable: the default's), because the end-to-end stall tests count guard calls to
/// pick a stall index, and an age flush on the real clock (a slow runner staging a batch for over a second) adds guard
/// calls and shifts every index. Likewise `FLUX_TEST_OPEN_WRITERS=<n>` replaces `open_writers` (cut 9e: the cap derives
/// from the host's descriptor limit, and a small one would flush early and shift the same indices). 0 is floored to 1, the
/// same floor as `open_writers_cap`.
#[cfg(debug_assertions)]
fn batch_policy(cfg: &RunConfig) -> crate::tree::BatchPolicy {
    let default = crate::tree::BatchPolicy::DEFAULT;
    let age = std::env::var("FLUX_TEST_BATCH_AGE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(default.age);
    let open_writers = std::env::var("FLUX_TEST_OPEN_WRITERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|n| n.max(1))
        .unwrap_or_else(|| crate::tree::open_writers_cap(cfg.descriptor_limit));
    crate::tree::BatchPolicy { age, beat_every: barrier_beat_every(cfg), open_writers, ..default }
}

#[cfg(not(debug_assertions))]
fn batch_policy(cfg: &RunConfig) -> crate::tree::BatchPolicy {
    crate::tree::BatchPolicy {
        beat_every: barrier_beat_every(cfg),
        open_writers: crate::tree::open_writers_cap(cfg.descriptor_limit),
        ..crate::tree::BatchPolicy::DEFAULT
    }
}

/// Cut 9f-1: the checkpoint policy of a tree run: `CheckpointPolicy::DEFAULT` with the run's durability and (cut 9f-2)
/// its `--resume-verify` level. In a debug
/// build, `FLUX_TEST_CHECKPOINT_MIN=<n>` (bytes) replaces `min_len` (absent or unparsable: the default's).
#[cfg(debug_assertions)]
fn checkpoint_policy(cfg: &RunConfig, durability: flux_fs::Durability) -> CheckpointPolicy {
    let default = CheckpointPolicy::DEFAULT;
    let min_len = std::env::var("FLUX_TEST_CHECKPOINT_MIN")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default.min_len);
    CheckpointPolicy {
        min_len,
        wal: crate::wal::writer::WalConfig { durability, ..default.wal },
        verify: cfg.resume_verify,
        ..default
    }
}

#[cfg(not(debug_assertions))]
fn checkpoint_policy(cfg: &RunConfig, durability: flux_fs::Durability) -> CheckpointPolicy {
    let default = CheckpointPolicy::DEFAULT;
    CheckpointPolicy {
        wal: crate::wal::writer::WalConfig { durability, ..default.wal },
        verify: cfg.resume_verify,
        ..default
    }
}

/// The most quarantine names `set_aside` tries for one generation (`.corrupt-<g>`, `-2`, `-3`, ...).
const QUARANTINE_NAMES_MAX: u32 = 64;

/// Cut 9f-1: open the run's checkpoint log, or say why there is none. `Ok(None)` also when the state carries no WAL
/// generation. The caller builds the policy's WAL durability from the run's (`checkpoint_policy`).
///
/// Owner ruling 23, per open-time failure: an unsupported format stops the run (`Err`, before any file is copied);
/// mid-log damage or a sequence conflict sets the segment aside and starts a fresh log (`quarantine`); an unavailable
/// identity or any other failure warns and the run has no checkpoints. `guard` and `beat` are the run's own closures: the
/// quarantine is a destination mutation, so it runs under them, and their error is the run's lost lock (`Err`).
fn open_checkpoints<F: DestinationRoot>(
    place: &TreePlace<'_, F>,
    state: &crate::state::OperationState,
    policy: CheckpointPolicy,
    warnings: &mut Vec<RunWarning>,
    guard: &dyn Fn() -> flux_fs::Result<()>,
    beat: &dyn Fn() -> flux_fs::Result<()>,
) -> Result<Option<Checkpoints<F::Dir>>, CopyError> {
    let Some(generation) = state.wal.as_ref().map(|w| w.wal_generation) else {
        return Ok(None);
    };
    let id = state.operation_id.as_str();
    let handles =
        || place.open_workspace(id).and_then(|w| place.open_workspace(id).map(|r| (w, r)));
    let (workspace, read_dir) = match handles() {
        Ok(h) => h,
        Err(e) => {
            warnings.push(RunWarning::CheckpointsUnreadable { why: e.to_string() });
            return Ok(None);
        }
    };
    match Checkpoints::open(workspace, read_dir, id, generation, policy) {
        Ok(c) => {
            if let Some(t) = c.discarded_tail() {
                use crate::wal::replay::TailReason;
                let reason = match t.reason {
                    TailReason::ZeroFilled => "zero-filled",
                    TailReason::SpanReachesEnd => "its declared span reaches the end of the log",
                    TailReason::TornRecord => "a record cut short",
                };
                warnings.push(RunWarning::WalTailDiscarded {
                    offset: t.offset,
                    len: t.len,
                    reason,
                });
            }
            Ok(Some(c))
        }
        Err(CheckpointOpenError::Unavailable(why)) => {
            warnings.push(RunWarning::CheckpointsUnavailable { why });
            Ok(None)
        }
        Err(CheckpointOpenError::FormatUnsupported(why)) => {
            let message = format!(
                "the checkpoint log was written by a newer Flux, or its version bytes are damaged ({why}); this run copies nothing; use the Flux version that wrote it, or run again with --restart to supersede it"
            );
            Err(CopyError {
                cause: FsError::new(Code::WalFormatUnsupported, std::io::Error::other(message)),
                leftover: None,
                step: crate::copy::CopyStep::Checkpoint,
            })
        }
        Err(CheckpointOpenError::Corrupt { code, why }) => {
            // Under §99: a lost lock aborts the quarantine, and the run ends as any lost lock does.
            let lost = |cause| CopyError {
                cause,
                leftover: None,
                step: crate::copy::CopyStep::Checkpoint,
            };
            guard().and_then(|()| beat()).map_err(lost)?;
            let set_aside_as = match set_aside(place, id, generation) {
                Ok(name) => name,
                Err(error) => {
                    warnings.push(RunWarning::WalNotQuarantined {
                        code,
                        why,
                        error: error.to_string(),
                    });
                    return Ok(None);
                }
            };
            // No segment exists now, so the open starts a clean log: the old log's checkpoints are not used.
            let reopened = handles().map_err(|e| e.to_string()).and_then(|(w, r)| {
                Checkpoints::open(w, r, id, generation, policy).map_err(|e| match e {
                    CheckpointOpenError::Unavailable(m)
                    | CheckpointOpenError::FormatUnsupported(m)
                    | CheckpointOpenError::Corrupt { why: m, .. } => m,
                    CheckpointOpenError::Fatal(e) => e.to_string(),
                })
            })
            .map_err(|m| {
                format!("it was set aside as wal/{set_aside_as}, but a fresh log could not be opened: {m}")
            });
            match reopened {
                Ok(c) => {
                    warnings.push(RunWarning::WalQuarantined { code, why, set_aside_as });
                    Ok(Some(c))
                }
                Err(error) => {
                    warnings.push(RunWarning::WalNotQuarantined { code, why, error });
                    Ok(None)
                }
            }
        }
        Err(CheckpointOpenError::Fatal(e)) => {
            warnings.push(RunWarning::CheckpointsUnreadable { why: e.to_string() });
            Ok(None)
        }
    }
}

/// Owner ruling 23: rename the workspace's `wal/000001` to `000001.corrupt-<generation>` (then `-2`, `-3`, ... when that
/// name is taken, up to `QUARANTINE_NAMES_MAX`), through handles opened and closed here. Returns the new name.
fn set_aside<F: DestinationRoot>(
    place: &TreePlace<'_, F>,
    id: &str,
    generation: u64,
) -> flux_fs::Result<String> {
    use crate::wal::writer::{DIR_NAME, SEGMENT_NAME};
    let workspace = place.open_workspace(id)?;
    let wal = workspace.open_dir(OsStr::new(DIR_NAME))?;
    for n in 1..=QUARANTINE_NAMES_MAX {
        let name = if n == 1 {
            format!("{SEGMENT_NAME}.corrupt-{generation}")
        } else {
            format!("{SEGMENT_NAME}.corrupt-{generation}-{n}")
        };
        match wal.rename_no_replace(OsStr::new(SEGMENT_NAME), &wal, OsStr::new(&name)) {
            Ok(()) => return Ok(name),
            Err(e) if e.source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(FsError::new(
        Code::IoError,
        std::io::Error::other(format!("all {QUARANTINE_NAMES_MAX} quarantine names are taken")),
    ))
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
        // Q-I removes what THIS run created. An adopted prior is not that: its workspace and recorded progress go only
        // under `--restart`, so it is kept FAILED (still resumable) and the copy's refusal stays the report.
        Ended::RefusedUnchanged if matches!(locked.resumed, Some(ResumeNote::Adopted { .. })) => {
            fail(place, locked)
        }
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
        not_removed: None,
    });
    match locked.held.release() {
        Ok(_) => stop,
        Err(e) => stop.or(Some(RunError::Failed {
            step: RunStep::State,
            path: locked.lock_shown,
            error: lock_io(e),
            not_removed: None,
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
    RunError::Failed { step, path, error, not_removed: None }
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
        return Some(RunError::Failed { step: RunStep::Rollback, path, error, not_removed: None });
    }
    match locked.held.release() {
        Ok(Released::Unlinked) => None,
        Ok(Released::NotOwned) => Some(session::lost()),
        Err(e) => Some(RunError::Failed {
            step: RunStep::Rollback,
            path: locked.lock_shown,
            error: lock_io(e),
            not_removed: None,
        }),
    }
}

/// Cut 11a: the notes a dry run makes of the completed priors and the debris at DEST. It never inspects an owner.
/// Answers whether it pushed at least one note.
fn dry_prior_notes(scan: &crate::prior::Scan, warnings: &mut Vec<RunWarning>) -> bool {
    let before = warnings.len();
    for p in &scan.completed {
        warnings
            .push(RunWarning::DryRunPriorCompleted { operation_id: p.state.operation_id.clone() });
    }
    for name in &scan.debris {
        warnings.push(RunWarning::DryRunDebris { name: name.to_string_lossy().into_owned() });
    }
    warnings.len() > before
}

/// Cut 11b: how a dry run's lock preview and prior classification ended: a predicted refusal (with the report's
/// flags), or the flags the plan carries.
enum DryStart {
    Stop(RunError, DryRun),
    Plan(DryRun),
}

/// Cut 11b, spec 3.2 and 3.3: the lock preview first (a predicted refusal ends the preview before any scan or
/// note), then the scan classified as the real run's step 4 classifies it. The caller fills `unprobed_noreplace`
/// and `planned`.
fn dry_start<D: DirHandle, P: Place<D>>(
    site: &LockSite<'_, D>,
    capability: LockCapability,
    place: &P,
    cfg: &RunConfig,
    warnings: &mut Vec<RunWarning>,
) -> DryStart {
    let mut dry = DryRun {
        unprobed_noreplace: false,
        prior_seen: false,
        planned: None,
        owner_liveness: false,
        lock_takeover: false,
        prior: None,
        partials_present: 0,
    };
    match dry_lock(site, cfg.break_lock, capability, crate::state::wall_time_ns(), warnings) {
        LockPreview::Absent => {}
        LockPreview::Recorded(_) => dry.owner_liveness = true,
        LockPreview::Takeover(_) => dry.lock_takeover = true,
        LockPreview::Refused(e) => {
            let stop = from_lock(e, RunStep::Lock, place.holder_shown(), false);
            return DryStart::Stop(stop, dry);
        }
        LockPreview::Error(e) => {
            let stop = from_lock(
                crate::lock::LockError::Io(e),
                RunStep::Lock,
                place.holder_shown(),
                false,
            );
            return DryStart::Stop(stop, dry);
        }
    }
    let scan = match place.scan(&cfg.operation_id) {
        Ok(scan) => scan,
        Err(e) => {
            let stop = from_lock(e, RunStep::Inspect, place.destination(), false);
            return DryStart::Stop(stop, dry);
        }
    };
    dry.prior_seen = dry_prior_notes(&scan, warnings);
    if cfg.restart {
        // Task 4: the restart preview runs here.
    } else if scan.resumable.is_empty() {
        dry.prior = cfg.resume.then_some(DryPrior::StartedNew);
    } else if cfg.resume && scan.resumable.len() == 1 {
        // Task 6: the resume preview runs here.
    } else {
        for p in &scan.resumable {
            warnings.push(RunWarning::DryRunPrior {
                operation_id: p.state.operation_id.clone(),
                state: p.state.state,
            });
        }
        dry.prior_seen = true;
        let stop = from_lock(
            resumable_refusal(&scan.resumable),
            RunStep::Inspect,
            place.destination(),
            false,
        );
        return DryStart::Stop(stop, dry);
    }
    DryStart::Plan(dry)
}

/// Cut 11a: the dry branch of `tree_with`. Reads DEST's prior operations for the notes, then plans the walk over a
/// claim store that stores nothing; the lock, the workspace, the WAL and `finish` are never reached.
#[allow(clippy::too_many_arguments)]
fn dry_tree<F: DestinationRoot>(
    mut run: Run<Result<TreeOutcome, TreeAbort>>,
    place: &mut TreePlace<'_, F>,
    site: &LockSite<'_, F::Dir>,
    capability: LockCapability,
    source: Source<'_, F>,
    cfg: &RunConfig,
    opts: &CopyOptions,
    on_report: &mut dyn FnMut(TreeFailure),
    mut out: TreeOutcome,
) -> Run<Result<TreeOutcome, TreeAbort>> {
    let mut dry = match dry_start(site, capability, &*place, cfg, &mut run.warnings) {
        DryStart::Plan(dry) => dry,
        DryStart::Stop(stop, dry) => {
            run.stop = Some(stop);
            run.dry = Some(dry);
            return run;
        }
    };
    let fs = place.fs;
    let src_root = place.src_root.clone();
    let null = std::cell::RefCell::new(NullClaimStore);
    let cx = Shared {
        fs,
        src_root: &src_root,
        src_identity: source.identity,
        opts,
        guard: &unguarded,
        beat: &no_heartbeat,
        claims: Some(&null),
        resume: false,
        batch: batch_policy(cfg),
        ckpt: None,
        dry_run: true,
    };
    let root = place.dest.take();
    let absent = root.is_none();
    let identity = match &root {
        Some(r) => r.identity().map(RootIdentity::Existing),
        None => place.holder.identity().map(RootIdentity::Planned),
    };
    let walked = match identity {
        Ok(root_identity) => {
            let (root, walked) = copy_tree_frames(
                &cx,
                source.events,
                root,
                root_identity,
                false,
                &mut out,
                on_report,
            );
            place.dest = root;
            walked
        }
        Err(e) => {
            place.dest = root;
            Err(CopyError::at(CopyStep::Resolve, e))
        }
    };
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
    run
}

/// Cut 11a: the dry branch of `file_with`: the prior notes, then what `plan_file` says the copy would do.
#[allow(clippy::too_many_arguments)]
fn dry_file<F: DestinationRoot>(
    mut run: Run<Result<Outcome, CopyError>>,
    fs: &F,
    place: &FilePlace<'_, F::Dir>,
    site: &LockSite<'_, F::Dir>,
    capability: LockCapability,
    src: &Path,
    cfg: &RunConfig,
    opts: &CopyOptions,
) -> Run<Result<Outcome, CopyError>> {
    let mut dry = match dry_start(site, capability, place, cfg, &mut run.warnings) {
        DryStart::Plan(dry) => dry,
        DryStart::Stop(stop, dry) => {
            run.stop = Some(stop);
            run.dry = Some(dry);
            return run;
        }
    };
    let planned = match plan_file(fs, src, Some(place.dir), &place.target, opts) {
        Ok(FilePlan::Proceed { src_meta, degraded, replaces }) => {
            run.copy = Some(Ok(Outcome {
                bytes_copied: src_meta.len,
                metadata_failures: Vec::new(),
                identity_degraded: degraded,
                published_identity: FileIdentity::Unavailable,
                skipped: false,
            }));
            let action = if replaces { PlannedAction::Overwrite } else { PlannedAction::Copy };
            Some(PlannedEntry { object: PlannedObject::File, action })
        }
        Ok(FilePlan::Skip { reason, degraded }) => {
            run.copy = Some(Ok(Outcome {
                bytes_copied: 0,
                metadata_failures: Vec::new(),
                identity_degraded: degraded,
                published_identity: FileIdentity::Unavailable,
                skipped: true,
            }));
            Some(PlannedEntry { object: PlannedObject::File, action: PlannedAction::Skip(reason) })
        }
        Err(e) => {
            run.copy = Some(Err(e));
            None
        }
    };
    dry.planned = planned;
    run.dry = Some(dry);
    run
}
