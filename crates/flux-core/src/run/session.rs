//! Steps 3-5 of "The run", and what the rest of the run shares with them: the §99 checks, and the lock protocol's
//! errors as the run's.

use super::place::Place;
use super::restart::supersede;
use super::resume::validate;
use super::{ResumeNote, RunConfig, RunError, RunStep, RunWarning, stop_after_record};
use crate::lock::record::LockRecord;
use crate::lock::{
    Held, LockCode, LockError, LockResult, LockSite, MAX_ATTEMPTS, Mode, Obtained, Overwritten,
    Refusal, obtain,
};
use crate::prior::{PriorOp, resumable_refusal};
use crate::state::{
    ARTIFACT_STATE, Config, FileFields, OpState, OperationState, Options, Takeover, identity_text,
    wall_time_ns,
};
use flux_fs::{Code, CopyOptions, DirHandle, FsError, LockCapability};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The lock this run holds, its record written, and this operation's state as last written.
pub(crate) struct Locked<'a, D: DirHandle> {
    pub(crate) held: Held<'a, D>,
    pub(crate) state: OperationState,
    /// The lock's path, as messages name it.
    pub(crate) lock_shown: PathBuf,
    /// The heartbeat (cut 7b): due, done, or failed.
    pub(crate) pulse: Pulse,
    /// Cut 9a: what `--resume` did; `None` without the flag.
    pub(crate) resumed: Option<ResumeNote>,
}

/// The run's heartbeat (cut 7b, §101, decisions 6-7): the interval, the instant of the last record write, and whether
/// a heartbeat has failed. In `Cell`s: the heartbeat runs where the run holds only a shared borrow of its lock - the
/// copy's callbacks and the `--restart` sweep's checks.
pub(crate) struct Pulse {
    interval: Duration,
    last: Cell<Instant>,
    failed: Cell<bool>,
}

impl Pulse {
    /// Made as the record is written: the interval counts from then.
    pub(crate) fn new(interval: Duration) -> Self {
        Pulse { interval, last: Cell::new(Instant::now()), failed: Cell::new(false) }
    }

    /// A heartbeat has failed: the run makes no further attempt, and the finish handles a record it can no longer read
    /// as its own (`fail`).
    pub(crate) fn failed(&self) -> bool {
        self.failed.get()
    }

    /// Refresh `held`'s record once the interval has passed since the last write; a failed write is retried once, at
    /// once (decision 7). After a failure it writes nothing and returns `Ok(())`: the failure was already reported once,
    /// and a second one can never replace or nest inside it. The error is the plain I/O error; each caller names the
    /// lock.
    pub(crate) fn beat<D: DirHandle>(&self, held: &Held<'_, D>) -> flux_fs::Result<()> {
        if self.failed.get() || self.last.get().elapsed() < self.interval {
            return Ok(());
        }
        let now = wall_time_ns();
        match held.heartbeat(now).or_else(|_| held.heartbeat(now)) {
            Ok(()) => {
                self.last.set(Instant::now());
                Ok(())
            }
            Err(e) => {
                self.failed.set(true);
                Err(lock_io(e))
            }
        }
    }
}

/// A heartbeat failure as the copy reports it (decision 7): the I/O error, naming the lock path, never
/// TARGET_LOCK_BUSY.
pub(crate) fn beat_error(lock_shown: &Path, e: FsError) -> FsError {
    let code = if e.code == Code::TargetLockBusy { Code::IoError } else { e.code };
    let message = format!(
        "the heartbeat could not refresh the lock record {}: {}",
        lock_shown.display(),
        e.source
    );
    FsError::new(code, std::io::Error::new(e.source.kind(), message))
}

/// Steps 3-5 of "The run", then `--restart`'s supersede, then TRANSFERRING: returns the lock with this operation's
/// record written, naming its state.
///
/// - Step 3 obtains the lock: acquired, recovered from a dead owner, or claimed from an uncertain one (`--break-lock`).
/// - Step 4's refusals give back what step 3 created (decision 12).
/// - Step 5 (F5): the state first, then the record naming it. Under `--restart` this record write is the model's
///   `S21_1_s5_write_begin` / `S21_1_s5_write_end`: 7a writes the record once, after the state, and before the
///   revalidation (Q-H (a); the model's `Recover` and `TakeOver` have written it before `S21_1_s3` too). A
///   `--break-lock` claim overwrites the prior record in place (§240.5 step 6) and only then records the takeover in
///   the state (spec:10700-10701).
/// - D1: a claim whose lock path moved (`Overwritten::Restart`) starts again, reusing the state it already made.
/// - When a §99 check fails after the record exists, the lock closes without unlinking as `locked` drops:
///   `S21_1_s3_refuse_close` under `--restart`, `S99_refuse_close` otherwise.
pub(crate) fn open_operation<'a, D: DirHandle, P: Place<D>>(
    site: &LockSite<'a, D>,
    capability: LockCapability,
    place: &mut P,
    cfg: &RunConfig,
    opts: &CopyOptions,
    warnings: &mut Vec<RunWarning>,
) -> Result<Locked<'a, D>, RunError> {
    let own_id = cfg.operation_id.as_str();
    let lock_shown = place.holder_shown().join(site.lock_name());
    let mode = if cfg.break_lock { Mode::BreakLock } else { Mode::Plain };
    // This run's state, once it exists (D1).
    let mut made: Option<OperationState> = None;
    // Cut 9a: what `--resume` did, once step 5 has (it survives a D1 restart with `made`).
    let mut resumed: Option<ResumeNote> = None;
    // Cut 7b: one wall-clock reading for the state and its record, so the record's fields equal the lock record's
    // (Part 1 decision 7), and one attempt id per run.
    let now = wall_time_ns();
    let attempt_id = crate::ids::new_id();
    for _ in 0..MAX_ATTEMPTS {
        // Step 3.
        let obtained = obtain(site, capability, mode, own_id)
            .map_err(|e| from_lock(e, RunStep::Lock, &lock_shown, made.is_some()))?;
        // Step 4.
        // Cut 9b: also what a copy finishes once it holds its own state and record (COMPLETED priors, debris).
        let (priors, adopt, completed, debris): Prior = match place.scan(own_id) {
            Ok(scan) if cfg.restart => (scan.resumable, None, scan.completed, scan.debris),
            Ok(scan) if scan.resumable.is_empty() => {
                (Vec::new(), None, scan.completed, scan.debris)
            }
            Ok(mut scan) if cfg.resume && scan.resumable.len() == 1 => {
                (Vec::new(), Some(scan.resumable.remove(0)), scan.completed, scan.debris)
            }
            Ok(scan) => {
                let refused = resumable_refusal(&scan.resumable);
                let error =
                    from_lock(refused, RunStep::Inspect, place.destination(), made.is_some());
                return Err(give_back(obtained, error, &lock_shown));
            }
            Err(e) => {
                let error = from_lock(e, RunStep::Inspect, place.destination(), made.is_some());
                return Err(give_back(obtained, error, &lock_shown));
            }
        };
        // Step 5 (F5): the state first.
        if made.is_none() {
            let file = place.file_identities().map(|(source, existing)| FileFields {
                artifact_type: ARTIFACT_STATE.to_string(),
                attempt_id: attempt_id.clone(),
                artifact_generation: 1,
                source_identity: identity_text(source),
                target_identity: existing.map(identity_text),
                target_path_key: site.target_path_key(),
                owner_instance_id: cfg.owner_instance_id.clone(),
                boot_session_id: cfg.boot_session_id.clone(),
                creation_wall_time: now.to_string(),
                last_heartbeat_wall_time: now.to_string(),
            });
            if let Some(prior) = adopt {
                // Cut 9a, "Order inside adoption": (1) validate, (2) open or create state.db; the manifest is written
                // TRANSFERRING last, so TRANSFERRING is never on disk without a state.db.
                let config = match validate(&prior, &place.roots(), opts, warnings) {
                    Ok(c) => c,
                    Err(e) => return Err(give_back(obtained, e, &lock_shown)),
                };
                let claims = match place.adopt(&prior.state, opts.durability) {
                    Ok(c) => c,
                    Err(e) => return Err(give_back(obtained, e, &lock_shown)),
                };
                let mut state = prior.state.clone();
                state.config = Some(config);
                if file.is_some()
                    && let Some(f) = &mut state.file
                {
                    f.attempt_id = attempt_id.clone();
                    f.artifact_generation += 1;
                    f.owner_instance_id = cfg.owner_instance_id.clone();
                    f.boot_session_id = cfg.boot_session_id.clone();
                    f.last_heartbeat_wall_time = now.to_string();
                }
                resumed =
                    Some(ResumeNote::Adopted { operation_id: state.operation_id.clone(), claims });
                made = Some(state);
            } else {
                if cfg.resume {
                    resumed = Some(ResumeNote::StartedNew);
                }
                let state = OperationState::created(
                    own_id,
                    place.kind(),
                    place.destination(),
                    now,
                    file,
                    Config::new(place.roots(), Options::of(opts)),
                );
                if let Err(e) = place.create(&state, warnings) {
                    return Err(give_back(obtained, e, &lock_shown));
                }
                made = Some(state);
            }
        }
        let state = made.clone().expect("made above");
        let effective = state.operation_id.clone();
        let id = effective.as_str();
        let record = match record_for(site, id, cfg, place.workspace_path(id), now) {
            Ok(r) => r,
            Err(e) => {
                let error = from_lock(e, RunStep::Record, &lock_shown, true);
                return Err(give_back(obtained, error, &lock_shown));
            }
        };
        // Then the record naming it.
        let mut locked = match obtained {
            Obtained::Held { mut held, leftover_broken } => {
                if let Some(name) = leftover_broken {
                    warnings.push(RunWarning::BrokenLeftover(place.holder_shown().join(name)));
                }
                if let Err(e) = held.write_record(record) {
                    // No record was kept (`write_record` sets it only on success): the lock is still this run's to
                    // remove (decision 12), and its state stays for a later `--restart`.
                    let error = from_lock(e, RunStep::Record, &lock_shown, true);
                    let unwritten = Obtained::Held { held, leftover_broken: None };
                    return Err(give_back(unwritten, error, &lock_shown));
                }
                Locked {
                    held,
                    state,
                    lock_shown: lock_shown.clone(),
                    pulse: Pulse::new(cfg.heartbeat_interval),
                    resumed: resumed.clone(),
                }
            }
            Obtained::Claimed(claimed) => match claimed.overwrite(record) {
                Ok(Overwritten::Held(held)) => {
                    let mut locked = Locked {
                        held,
                        state,
                        lock_shown: lock_shown.clone(),
                        pulse: Pulse::new(cfg.heartbeat_interval),
                        resumed: resumed.clone(),
                    };
                    // §240.5 step 6, after the flush succeeded: the takeover, in this operation's state.
                    locked.state.takeover = Some(Takeover::of_unreadable(wall_time_ns()));
                    if let Err(error) = place.write(&locked.state) {
                        let path = place.shown(id);
                        return Err(stop_after_record(place, locked, RunStep::State, path, error));
                    }
                    locked
                }
                // D1: the claimed file is no longer at the lock path. Start again; the state stays this run's.
                Ok(Overwritten::Restart) => continue,
                Err(e) => return Err(from_lock(e, RunStep::Record, &lock_shown, true)),
            },
        };
        // Q-H (a): `--restart` supersedes, with this operation's state and record already written. When §99 fails
        // there, `locked` drops without unlinking (`S21_1_s3_refuse_close`).
        if let Err(fault) = supersede(place, &locked, &priors, warnings) {
            return Err(match fault {
                Fault::Lost => lost(),
                Fault::Io(path, error) => {
                    stop_after_record(place, locked, RunStep::Restart, path, error)
                }
                // Decision 7: the run's stop, at the lock step, naming the lock.
                Fault::Heartbeat(path, error) => {
                    stop_after_record(place, locked, RunStep::Lock, path, error)
                }
            });
        }
        // Cut 9b: finish what earlier COMPLETED operations left, then remove workspace debris. This run's state and
        // record exist, so `checked` proves ownership before every removal. Only losing the lock stops the run; every
        // other failure is a warning (F6).
        if let Err(fault) = finish_priors(place, &locked, &completed, &debris, warnings) {
            return Err(match fault {
                Fault::Lost => lost(),
                Fault::Io(path, error) => {
                    stop_after_record(place, locked, RunStep::PriorCleanup, path, error)
                }
                Fault::Heartbeat(path, error) => {
                    stop_after_record(place, locked, RunStep::Lock, path, error)
                }
            });
        }
        // The last write of step 5: TRANSFERRING, under §99.
        if let Err(fault) = owned(&locked.held, &locked.lock_shown) {
            return Err(fault.into_error(RunStep::State));
        }
        locked.state.state = OpState::Transferring;
        if let Err(error) = place.write(&locked.state) {
            let path = place.shown(id);
            return Err(stop_after_record(place, locked, RunStep::State, path, error));
        }
        return Ok(locked);
    }
    Err(RunError::Refused {
        refusal: Box::new(Refusal {
            code: LockCode::TargetLockBusy,
            holder: None,
            detail: format!(
                "gave up after {MAX_ATTEMPTS} attempts: the lock this run took over kept moving; this operation's state stays for a later --restart"
            ),
        }),
        changed: made.is_some(),
        not_removed: None,
    })
}

/// The step 4 scan as `open_operation` keeps it: the resumable priors to supersede, the one to adopt, and (cut 9b) the
/// COMPLETED priors and the debris directories to clean.
type Prior = (Vec<PriorOp>, Option<PriorOp>, Vec<PriorOp>, Vec<std::ffi::OsString>);

/// Cut 9b: each COMPLETED prior's recorded cleanup, then each debris directory.
fn finish_priors<D: DirHandle, P: Place<D>>(
    place: &P,
    locked: &Locked<'_, D>,
    completed: &[PriorOp],
    debris: &[std::ffi::OsString],
    warnings: &mut Vec<RunWarning>,
) -> Result<(), Fault> {
    for prior in completed {
        place.finish_completed(prior, locked, warnings)?;
    }
    for name in debris {
        place.remove_debris(name, locked, warnings)?;
    }
    Ok(())
}

/// A refusal or failure before this run's record exists: remove the lock this run created (Part 2 decision 7:
/// unlinked while still held), or let go of a claimed one it did not create. If that removal fails, the refusal names
/// the path and becomes exit 1 (spec:2896-2902). This run's state, if it already exists (D1), stays.
fn give_back<D: DirHandle>(
    obtained: Obtained<'_, D>,
    mut error: RunError,
    lock_shown: &Path,
) -> RunError {
    let Obtained::Held { held, .. } = obtained else {
        return error;
    };
    if let Err(e) = held.discard() {
        match &mut error {
            RunError::Refused { refusal, changed, not_removed } => {
                *changed = true;
                // A removal already recorded (the probe's leftover) is kept in the detail, never dropped from the
                // report.
                if let Some((path, displaced)) =
                    not_removed.replace((lock_shown.to_path_buf(), lock_io(e)))
                {
                    refusal.detail.push_str(&format!(
                        "; also not removed: {} ({})",
                        path.display(),
                        displaced.source
                    ));
                }
            }
            RunError::Failed { not_removed, .. } => {
                *not_removed = Some((lock_shown.to_path_buf(), lock_io(e)));
            }
        }
    }
    error
}

/// This operation's lock record for `site` (§259.6's fields; in 7a `last_heartbeat_wall_time` equals
/// `creation_wall_time`; both are the run's one `now` (cut 7b)).
fn record_for<D: DirHandle>(
    site: &LockSite<'_, D>,
    operation_id: &str,
    cfg: &RunConfig,
    workspace_path: String,
    now: u64,
) -> LockResult<LockRecord> {
    Ok(LockRecord {
        complete_lock_key: site.complete_lock_key()?,
        operation_id: operation_id.to_string(),
        owner_instance_id: cfg.owner_instance_id.clone(),
        boot_session_id: cfg.boot_session_id.clone(),
        target_path_key: site.target_path_key(),
        workspace_path,
        creation_wall_time: now,
        last_heartbeat_wall_time: now,
    })
}

/// Why a §99 check did not pass.
pub(crate) enum Fault {
    /// `still_owned` said no: another run took the lock over or removed it.
    Lost,
    /// It could not tell, or a step after it failed, at this path.
    Io(PathBuf, FsError),
    /// The heartbeat before it failed, twice (cut 7b decision 7), at the lock's path.
    Heartbeat(PathBuf, FsError),
}

impl Fault {
    pub(crate) fn into_error(self, step: RunStep) -> RunError {
        match self {
            Fault::Lost => lost(),
            Fault::Io(path, error) => RunError::Failed { step, path, error, not_removed: None },
            // Decision 7: a heartbeat outside the copy fails the lock step, whatever step it interrupted.
            Fault::Heartbeat(path, error) => {
                RunError::Failed { step: RunStep::Lock, path, error, not_removed: None }
            }
        }
    }
}

/// §99 (`S99_check`, and `S21_1_s3` under `--restart`): this run still owns its lock.
pub(crate) fn owned<D: DirHandle>(held: &Held<'_, D>, lock_shown: &Path) -> Result<(), Fault> {
    match held.still_owned() {
        Ok(true) => Ok(()),
        Ok(false) => Err(Fault::Lost),
        Err(e) => Err(Fault::Io(lock_shown.to_path_buf(), lock_io(e))),
    }
}

/// The `--restart` sweep's check (cut 7b, "The heartbeat"): the heartbeat, then §99. Two calls: `owned` stays a pure
/// check.
pub(crate) fn checked<D: DirHandle>(locked: &Locked<'_, D>) -> Result<(), Fault> {
    checked_held(&locked.held, &locked.lock_shown, &locked.pulse)
}

/// `checked` for a caller that holds the pieces rather than a `Locked` (cut 9b's cleanup).
pub(crate) fn checked_held<D: DirHandle>(
    held: &Held<'_, D>,
    lock_shown: &Path,
    pulse: &Pulse,
) -> Result<(), Fault> {
    if let Err(e) = pulse.beat(held) {
        return Err(Fault::Heartbeat(lock_shown.to_path_buf(), e));
    }
    owned(held, lock_shown)
}

const LOST: &str =
    "this run no longer holds the destination's lock: another run took it over or removed it";

const AFTER_HEARTBEAT: &str = "kept: a heartbeat write failed, so this run cannot tell whether the destination's lock is still its own";

/// The copy's guard (`copy_file_guarded`, `copy_tree_at`): §99 as the `FsError` the engine aborts on. A check that
/// cannot tell counts as lost: the copy must not go on writing. After a failed heartbeat (cut 7b) its reason names the
/// heartbeat: the record may be the one the heartbeat tore, not another run's.
pub(crate) fn guarded<D: DirHandle>(held: &Held<'_, D>, pulse: &Pulse) -> flux_fs::Result<()> {
    let why = if pulse.failed() { AFTER_HEARTBEAT } else { LOST };
    match held.still_owned() {
        Ok(true) => Ok(()),
        Ok(false) => Err(FsError::new(Code::TargetLockBusy, std::io::Error::other(why))),
        Err(e) if pulse.failed() => Err(FsError::new(
            Code::TargetLockBusy,
            std::io::Error::other(format!("{why}: {}", lock_io(e).source)),
        )),
        Err(e) => Err(FsError::new(Code::TargetLockBusy, lock_io(e).source)),
    }
}

/// Ownership lost mid-run (decision 13): TARGET_LOCK_BUSY, exit 1, the state left as it is.
pub(crate) fn lost() -> RunError {
    RunError::Refused {
        refusal: Box::new(Refusal {
            code: LockCode::TargetLockBusy,
            holder: None,
            detail: format!("{LOST}; this operation's state stays as it is"),
        }),
        changed: true,
        not_removed: None,
    }
}

/// A lock-protocol error as the run's: a refusal keeps its code; an I/O error is the run's failure at `step`, `path`.
/// A refusal of the lock names the path to act on - the lock, or the directory that would hold it - as the
/// refusal-guidance table requires (Part 3b-2 decision 7).
pub(crate) fn from_lock(e: LockError, step: RunStep, path: &Path, changed: bool) -> RunError {
    match e {
        LockError::Refused(mut refusal) => {
            if step == RunStep::Lock {
                refusal.detail = format!("{}: {}", path.display(), refusal.detail);
            }
            RunError::Refused { refusal, changed, not_removed: None }
        }
        LockError::Io(error) => {
            RunError::Failed { step, path: path.to_path_buf(), error, not_removed: None }
        }
    }
}

/// An I/O failure as the run's, at `step` and `path`.
pub(crate) fn failed(step: RunStep, path: &Path, error: FsError) -> RunError {
    RunError::Failed { step, path: path.to_path_buf(), error, not_removed: None }
}

/// The I/O error inside a lock-protocol error. A refusal cannot come from the calls this is used on; its detail is
/// kept if one ever does.
pub(crate) fn lock_io(e: LockError) -> FsError {
    match e {
        LockError::Io(e) => e,
        LockError::Refused(r) => FsError::new(Code::IoError, std::io::Error::other(r.detail)),
    }
}
