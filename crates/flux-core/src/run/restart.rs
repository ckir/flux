//! `--restart` (the design's "`--restart` and `--break-lock`", amended by Q-H (a)).

use super::RunWarning;
use super::place::Place;
use super::session::{Fault, Locked, owned};
use crate::prior::PriorOp;
use crate::state::{OpState, OperationState};
use flux_fs::DirHandle;

/// Supersede every resumable prior operation, with this operation's state and record already written (Q-H (a)): each
/// is durably set ABANDONED with `superseded_by` = this operation, then the priors' partials are deleted, then each
/// prior's state.
///
/// `S21_1_s3`: §99's `still_owned` before EVERY one of those mutations (spec:9365-9367); when it fails, the caller
/// closes the lock without unlinking (`S21_1_s3_refuse_close`). A partial that cannot be deleted keeps its operation's
/// ABANDONED state as the record naming it (J1). A state that cannot be removed is a warning: an ABANDONED operation
/// is passed over by every later run.
pub(crate) fn supersede<D: DirHandle, P: Place<D>>(
    place: &P,
    locked: &Locked<'_, D>,
    priors: &[PriorOp],
    warnings: &mut Vec<RunWarning>,
) -> Result<(), Fault> {
    let own = &locked.state.operation_id;
    for prior in priors {
        owned(&locked.held, &locked.lock_shown)?;
        let abandoned = OperationState {
            state: OpState::Abandoned,
            superseded_by: Some(own.clone()),
            ..prior.state.clone()
        };
        place.write(&abandoned).map_err(|e| Fault::Io(prior.shown.clone(), e))?;
    }
    let gone = place.sweep(priors, &locked.held, &locked.lock_shown, warnings)?;
    for prior in priors.iter().filter(|p| gone.contains(&p.state.operation_id)) {
        owned(&locked.held, &locked.lock_shown)?;
        if let Err((path, error)) = place.remove(&prior.state.operation_id) {
            warnings.push(RunWarning::NotRemoved { path, error });
        }
    }
    Ok(())
}
