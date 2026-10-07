//! What the lock protocol refuses with, and why.

use super::record::LockRecord;
use flux_fs::FsError;

/// The spec's error codes the lock protocol and the prior-state scan produce ("Errors and exit statuses"). The CLI maps them to exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockCode {
    TargetLockBusy,
    TargetLockUncertain,
    ControlPlaneNamespaceConflict,
    ArtifactOwnershipUncertain,
    PathComponentInvalid,
    RemoteLockUnsafe,
    StateCorrupt,
    IncompatibleState,
    ResumableOperationExists,
    /// Raised by the run (section 241.5), not by the lock protocol, as `PathComponentInvalid` already is: the
    /// destination has no atomic no-replace publication primitive.
    NoReplacePublishUnavailable,
}

impl LockCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TargetLockBusy => "TARGET_LOCK_BUSY",
            Self::TargetLockUncertain => "TARGET_LOCK_UNCERTAIN",
            Self::ControlPlaneNamespaceConflict => "CONTROL_PLANE_NAMESPACE_CONFLICT",
            Self::ArtifactOwnershipUncertain => "ARTIFACT_OWNERSHIP_UNCERTAIN",
            Self::PathComponentInvalid => "PATH_COMPONENT_INVALID",
            Self::RemoteLockUnsafe => "REMOTE_LOCK_UNSAFE",
            Self::StateCorrupt => "STATE_CORRUPT",
            Self::IncompatibleState => "INCOMPATIBLE_STATE",
            Self::ResumableOperationExists => "RESUMABLE_OPERATION_EXISTS",
            Self::NoReplacePublishUnavailable => "NOREPLACE_PUBLISH_UNAVAILABLE",
        }
    }
}

/// A refusal the protocol decided. Nothing of the destination was changed by the refusing step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: LockCode,
    /// The holder's record, where it was readable (§96.2: a refusal reports the holder). For
    /// `ARTIFACT_OWNERSHIP_UNCERTAIN` its `workspace_path` is what the caller turns into "the preserved path" the
    /// refusal-guidance table asks the message to name.
    pub holder: Option<LockRecord>,
    pub detail: String,
}

#[derive(Debug)]
pub enum LockError {
    /// Boxed: a `Refusal` carries a whole `LockRecord`, and an unboxed one makes every `LockResult` large
    /// (`clippy::result_large_err`).
    Refused(Box<Refusal>),
    Io(FsError),
}

impl From<FsError> for LockError {
    fn from(e: FsError) -> Self {
        Self::Io(e)
    }
}

pub type LockResult<T> = std::result::Result<T, LockError>;

pub(crate) fn refuse(
    code: LockCode,
    holder: Option<LockRecord>,
    detail: impl Into<String>,
) -> LockError {
    LockError::Refused(Box::new(Refusal { code, holder, detail: detail.into() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_the_spec_string() {
        assert_eq!(LockCode::TargetLockBusy.as_str(), "TARGET_LOCK_BUSY");
        assert_eq!(LockCode::TargetLockUncertain.as_str(), "TARGET_LOCK_UNCERTAIN");
        assert_eq!(
            LockCode::ControlPlaneNamespaceConflict.as_str(),
            "CONTROL_PLANE_NAMESPACE_CONFLICT"
        );
        assert_eq!(LockCode::ArtifactOwnershipUncertain.as_str(), "ARTIFACT_OWNERSHIP_UNCERTAIN");
        assert_eq!(LockCode::PathComponentInvalid.as_str(), "PATH_COMPONENT_INVALID");
        assert_eq!(LockCode::RemoteLockUnsafe.as_str(), "REMOTE_LOCK_UNSAFE");
        assert_eq!(LockCode::StateCorrupt.as_str(), "STATE_CORRUPT");
        assert_eq!(LockCode::IncompatibleState.as_str(), "INCOMPATIBLE_STATE");
        assert_eq!(LockCode::ResumableOperationExists.as_str(), "RESUMABLE_OPERATION_EXISTS");
        assert_eq!(LockCode::NoReplacePublishUnavailable.as_str(), "NOREPLACE_PUBLISH_UNAVAILABLE");
    }
}
