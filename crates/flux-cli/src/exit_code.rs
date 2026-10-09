//! §55's exit codes, decided from what the engine returned (cut 5).

use flux_core::copy::CopyError;
use flux_core::run::{Run, RunError};
use flux_core::{TreeAbort, TreeOutcome};
use flux_fs::{Code, Outcome};

pub const SUCCESS: u8 = 0;
pub const FAILED: u8 = 1;
pub const USAGE: u8 = 2;
pub const REFUSED: u8 = 3;

/// A tree copy. Exit 3 needs a refusal that changed nothing AND followed no streamed
/// failure: a run that acted on some paths before the refusal is §55's "a partial run
/// hit a refusal on some paths only", exit 1.
pub fn for_tree(r: &Result<TreeOutcome, TreeAbort>) -> u8 {
    match r {
        Ok(out) if out.failures.is_empty() => SUCCESS,
        Ok(_) => FAILED,
        Err(a) if a.refused_unchanged() => REFUSED,
        Err(_) => FAILED,
    }
}

/// A single-file copy. Both of `copy_file`'s `SAFETY_REJECTED` sites (Step 0 and the
/// Step 2a gate) run before the temporary exists, so such a refusal changed nothing.
pub fn for_file(r: &Result<Outcome, CopyError>) -> u8 {
    match r {
        Ok(o) if o.metadata_failures.is_empty() => SUCCESS,
        Ok(_) => FAILED,
        Err(e) if e.code() == Code::SafetyRejected && e.leftover.is_none() => REFUSED,
        Err(_) => FAILED,
    }
}

/// A run under the destination's lock (cut 7a): its own stop decides first - a refusal that changed nothing is 3
/// (§55), any other stop is 1 - and with no stop the copy's own rule decides. Warnings never change it.
pub fn for_tree_run(r: &Run<Result<TreeOutcome, TreeAbort>>) -> u8 {
    for_stop(&r.stop)
        .unwrap_or_else(|| for_tree(r.copy.as_ref().expect("a run with no stop has a copy")))
}

/// `for_tree_run`, for a single file.
pub fn for_file_run(r: &Run<Result<Outcome, CopyError>>) -> u8 {
    for_stop(&r.stop)
        .unwrap_or_else(|| for_file(r.copy.as_ref().expect("a run with no stop has a copy")))
}

/// `flux cleanup` (cut 9b): 1 when a deletion failed, a directory could not be listed or the lock was lost; the
/// whole-run refusal (3) is decided by the caller from `Err(Refused)`.
pub fn for_cleanup(r: &flux_core::cleanup::CleanupReport) -> u8 {
    if r.failed() { FAILED } else { SUCCESS }
}

fn for_stop(stop: &Option<RunError>) -> Option<u8> {
    match stop {
        None => None,
        Some(RunError::Refused { changed: false, .. }) => Some(REFUSED),
        Some(_) => Some(FAILED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::copy::CopyStep;
    use flux_core::lock::{LockCode, Refusal};
    use flux_core::run::{Run, RunError, RunStep};
    use flux_fs::{FsError, MetadataFailure, MetadataItem};
    use std::path::PathBuf;

    fn err(code: Code) -> CopyError {
        CopyError {
            cause: FsError::new(code, std::io::Error::other("x")),
            leftover: None,
            step: CopyStep::Resolve,
        }
    }

    // Returns `TreeAbort` by value like `copy_tree` does (plan refinement 6).
    #[allow(clippy::result_large_err)]
    fn abort(code: Code, f: impl FnOnce(&mut TreeOutcome)) -> Result<TreeOutcome, TreeAbort> {
        let mut outcome = TreeOutcome::default();
        f(&mut outcome);
        Err(TreeAbort { error: err(code), outcome })
    }

    #[test]
    fn a_clean_tree_exits_0_and_any_failure_exits_1() {
        assert_eq!(for_tree(&Ok(TreeOutcome::default())), SUCCESS);
        let mut out = TreeOutcome::default();
        out.failures.copy = 1;
        assert_eq!(for_tree(&Ok(out)), FAILED);
    }

    #[test]
    fn a_claim_not_recorded_failure_exits_1() {
        let mut out = TreeOutcome::default();
        out.failures.claim_not_recorded = 1;
        assert_eq!(for_tree(&Ok(out)), FAILED);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn warnings_and_skipped_special_files_still_exit_0() {
        let mut out = TreeOutcome::default();
        out.special_files_skipped = 3;
        out.files_degraded = 2;
        assert_eq!(for_tree(&Ok(out)), SUCCESS);
    }

    #[test]
    fn an_unchanged_refusal_exits_3() {
        assert_eq!(for_tree(&abort(Code::SafetyRejected, |_| {})), REFUSED);
        assert_eq!(for_tree(&abort(Code::NoReplacePublishUnavailable, |_| {})), REFUSED);
    }

    #[test]
    fn a_refusal_after_a_change_exits_1() {
        assert_eq!(for_tree(&abort(Code::SafetyRejected, |o| o.directories_created = 1)), FAILED);
        assert_eq!(for_tree(&abort(Code::SafetyRejected, |o| o.files_copied = 1)), FAILED);
        let mut e = err(Code::NoReplacePublishUnavailable);
        e.leftover = Some((PathBuf::from("a.flux-partial.1"), std::io::Error::other("busy")));
        let left = Err(TreeAbort { error: e, outcome: TreeOutcome::default() });
        assert_eq!(for_tree(&left), FAILED);
    }

    #[test]
    fn a_refusal_after_a_streamed_failure_exits_1() {
        assert_eq!(for_tree(&abort(Code::SafetyRejected, |o| o.failures.walk = 1)), FAILED);
    }

    #[test]
    fn an_unchanged_abort_that_is_not_a_refusal_exits_1() {
        assert_eq!(for_tree(&abort(Code::IoError, |_| {})), FAILED);
    }

    #[test]
    fn single_file_exit_codes() {
        let ok = |complaints: usize| {
            Ok(Outcome {
                bytes_copied: 1,
                metadata_failures: (0..complaints)
                    .map(|_| MetadataFailure {
                        item: MetadataItem::Times,
                        error: FsError::new(Code::MetadataApplyFailed, std::io::Error::other("x")),
                    })
                    .collect(),
                identity_degraded: None,
                published_identity: flux_fs::FileIdentity::Unavailable,
                skipped: false,
            })
        };
        assert_eq!(for_file(&ok(0)), SUCCESS);
        assert_eq!(for_file(&ok(1)), FAILED);
        assert_eq!(for_file(&Err(err(Code::SafetyRejected))), REFUSED);
        assert_eq!(for_file(&Err(err(Code::IoError))), FAILED);
    }

    #[test]
    fn cleanup_exits_1_only_on_a_kept_action_a_failed_listing_or_a_lost_lock() {
        use flux_core::cleanup::{Action, CleanupReport};
        let io = || FsError::new(Code::IoError, std::io::Error::other("x"));
        let p = || PathBuf::from("p");
        assert_eq!(for_cleanup(&CleanupReport::default()), SUCCESS);
        let ok = CleanupReport {
            actions: vec![
                Action::Removed(p()),
                Action::Skipped { id: "i".into(), reason: "r".into() },
            ],
            ..CleanupReport::default()
        };
        assert_eq!(for_cleanup(&ok), SUCCESS);
        let kept = CleanupReport {
            actions: vec![Action::Kept { path: p(), error: io() }],
            ..CleanupReport::default()
        };
        assert_eq!(for_cleanup(&kept), FAILED);
        let listing =
            CleanupReport { listing_failed: Some((p(), io())), ..CleanupReport::default() };
        assert_eq!(for_cleanup(&listing), FAILED);
        let lost = CleanupReport { lost: Some((p(), io())), ..CleanupReport::default() };
        assert_eq!(for_cleanup(&lost), FAILED);
    }

    fn refused(changed: bool) -> RunError {
        RunError::Refused {
            refusal: Box::new(Refusal {
                code: LockCode::TargetLockBusy,
                holder: None,
                detail: "x".to_string(),
            }),
            changed,
            not_removed: None,
        }
    }

    fn failed() -> RunError {
        RunError::Failed {
            step: RunStep::State,
            path: PathBuf::from("p"),
            error: FsError::new(Code::IoError, std::io::Error::other("x")),
            not_removed: None,
        }
    }

    #[test]
    fn a_tree_runs_own_stop_decides_before_its_copy() {
        let run = |copy, stop| Run { copy, stop, warnings: Vec::new(), resumed: None };
        assert_eq!(for_tree_run(&run(None, Some(refused(false)))), REFUSED);
        assert_eq!(for_tree_run(&run(None, Some(refused(true)))), FAILED);
        assert_eq!(for_tree_run(&run(None, Some(failed()))), FAILED);
        assert_eq!(for_tree_run(&run(Some(Ok(TreeOutcome::default())), Some(failed()))), FAILED);
        assert_eq!(for_tree_run(&run(Some(Ok(TreeOutcome::default())), None)), SUCCESS);
        let rolled_back = abort(Code::SafetyRejected, |_| {});
        assert_eq!(for_tree_run(&run(Some(rolled_back), None)), REFUSED, "a refusal rolled back");
    }

    #[test]
    fn a_file_runs_own_stop_decides_before_its_copy() {
        let run = |copy, stop| Run { copy, stop, warnings: Vec::new(), resumed: None };
        let copied = || {
            Ok(Outcome {
                bytes_copied: 1,
                metadata_failures: Vec::new(),
                identity_degraded: None,
                published_identity: flux_fs::FileIdentity::Unavailable,
                skipped: false,
            })
        };
        assert_eq!(for_file_run(&run(None, Some(refused(false)))), REFUSED);
        assert_eq!(for_file_run(&run(Some(copied()), Some(refused(true)))), FAILED);
        assert_eq!(for_file_run(&run(Some(copied()), None)), SUCCESS);
        assert_eq!(for_file_run(&run(Some(Err(err(Code::SafetyRejected))), None)), REFUSED);
    }
}
