//! Steps 3-5 of "The run", and what the rest of the run shares with them: the §99 checks, and the lock protocol's
//! errors as the run's.

use super::place::Place;
use super::restart::supersede;
use super::{RunConfig, RunError, RunStep, RunWarning, stop_after_record};
use crate::lock::record::LockRecord;
use crate::lock::{
    Held, LockCode, LockError, LockResult, LockSite, MAX_ATTEMPTS, Mode, Obtained, Overwritten,
    Refusal, obtain,
};
use crate::prior::resumable_refusal;
use crate::state::{OpState, OperationState, Takeover, wall_time_ns};
use flux_fs::{Code, DirHandle, FsError, LockCapability};
use std::path::{Path, PathBuf};

/// The lock this run holds, its record written, and this operation's state as last written.
pub(crate) struct Locked<'a, D: DirHandle> {
    pub(crate) held: Held<'a, D>,
    pub(crate) state: OperationState,
    /// The lock's path, as messages name it.
    pub(crate) lock_shown: PathBuf,
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
    warnings: &mut Vec<RunWarning>,
) -> Result<Locked<'a, D>, RunError> {
    let id = cfg.operation_id.as_str();
    let lock_shown = place.holder_shown().join(site.lock_name());
    let mode = if cfg.break_lock { Mode::BreakLock } else { Mode::Plain };
    // This run's state, once it exists (D1).
    let mut made: Option<OperationState> = None;
    for _ in 0..MAX_ATTEMPTS {
        // Step 3.
        let obtained = obtain(site, capability, mode, id)
            .map_err(|e| from_lock(e, RunStep::Lock, &lock_shown, made.is_some()))?;
        // Step 4.
        let priors = match place.scan(id) {
            Ok(scan) if scan.resumable.is_empty() || cfg.restart => scan.resumable,
            Ok(scan) => {
                let refused = resumable_refusal(&scan.resumable[0]);
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
            let state =
                OperationState::created_v1(id, place.kind(), place.destination(), wall_time_ns());
            if let Err(e) = place.create(&state) {
                return Err(give_back(obtained, e, &lock_shown));
            }
            made = Some(state);
        }
        let state = made.clone().expect("made above");
        let record = match record_for(site, cfg, place.workspace_path(id)) {
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
                Locked { held, state, lock_shown: lock_shown.clone() }
            }
            Obtained::Claimed(claimed) => match claimed.overwrite(record) {
                Ok(Overwritten::Held(held)) => {
                    let mut locked = Locked { held, state, lock_shown: lock_shown.clone() };
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
    if let Err(e) = held.discard()
        && let RunError::Refused { changed, not_removed, .. } = &mut error
    {
        *changed = true;
        *not_removed = Some((lock_shown.to_path_buf(), lock_io(e)));
    }
    error
}

/// This operation's lock record for `site` (§259.6's fields; in 7a `last_heartbeat_wall_time` equals
/// `creation_wall_time`).
fn record_for<D: DirHandle>(
    site: &LockSite<'_, D>,
    cfg: &RunConfig,
    workspace_path: String,
) -> LockResult<LockRecord> {
    let now = wall_time_ns();
    Ok(LockRecord {
        complete_lock_key: site.complete_lock_key()?,
        operation_id: cfg.operation_id.clone(),
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
}

impl Fault {
    pub(crate) fn into_error(self, step: RunStep) -> RunError {
        match self {
            Fault::Lost => lost(),
            Fault::Io(path, error) => RunError::Failed { step, path, error },
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

const LOST: &str =
    "this run no longer holds the destination's lock: another run took it over or removed it";

/// The copy's guard (`copy_file_guarded`, `copy_tree_at`): §99 as the `FsError` the engine aborts on. A check that
/// cannot tell counts as lost: the copy must not go on writing.
pub(crate) fn guarded<D: DirHandle>(held: &Held<'_, D>) -> flux_fs::Result<()> {
    match held.still_owned() {
        Ok(true) => Ok(()),
        Ok(false) => Err(FsError::new(Code::TargetLockBusy, std::io::Error::other(LOST))),
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
        LockError::Io(error) => RunError::Failed { step, path: path.to_path_buf(), error },
    }
}

/// An I/O failure as the run's, at `step` and `path`.
pub(crate) fn failed(step: RunStep, path: &Path, error: FsError) -> RunError {
    RunError::Failed { step, path: path.to_path_buf(), error }
}

/// The I/O error inside a lock-protocol error. A refusal cannot come from the calls this is used on; its detail is
/// kept if one ever does.
pub(crate) fn lock_io(e: LockError) -> FsError {
    match e {
        LockError::Io(e) => e,
        LockError::Refused(r) => FsError::new(Code::IoError, std::io::Error::other(r.detail)),
    }
}
