//! §55's exit codes, decided from what the engine returned (cut 5).

use flux_core::copy::CopyError;
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
        Err(a) if !a.changed() && a.outcome.failures.is_empty() && is_refusal(a.error.code()) => {
            REFUSED
        }
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

/// §55's exit-3 refusals this engine can raise.
fn is_refusal(code: Code) -> bool {
    matches!(code, Code::SafetyRejected | Code::NoReplacePublishUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::copy::CopyStep;
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
            })
        };
        assert_eq!(for_file(&ok(0)), SUCCESS);
        assert_eq!(for_file(&ok(1)), FAILED);
        assert_eq!(for_file(&Err(err(Code::SafetyRejected))), REFUSED);
        assert_eq!(for_file(&Err(err(Code::IoError))), FAILED);
    }
}
