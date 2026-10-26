// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The spec's error codes, and the `io::Error` wrapper that carries one.

/// `ENOSPC` on Unix, `ERROR_DISK_FULL` on Windows.
#[cfg(unix)]
pub(crate) const ENOSPC_RAW: i32 = 28;
#[cfg(windows)]
pub(crate) const ENOSPC_RAW: i32 = 112;

/// A spec error code: the codes a single-file copy or a tree copy can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    CopyFailed,
    MetadataApplyFailed,
    DiskFull,
    PermissionDenied,
    SourceChanged,
    StrictDurabilityUnavailable,
    SpecialFileUnsupported,
    SafetyRejected,
    /// §2 item 83, §149.4. A DIRECTORY whose object changed under the walk -- not to
    /// be confused with `SourceChanged`, which is a FILE whose length or mtime moved
    /// under `copy_file` (§33). Different objects, different producers, no overlap.
    DirectoryChangedDuringScan,
    /// §241.5. No no-replace publication primitive is available on the destination
    /// for a DIRECTORY operation, which is refused before anything changes. Nothing
    /// in this crate returns it: the engine raises it and the CLI maps it to exit 3.
    /// The vocabulary lands here because a `Code` variant is a `flux-fs` concern.
    NoReplacePublishUnavailable,
    /// §241.5, §2 item 83. Two distinct source paths map to the same destination
    /// object or prefix -- two names a case-insensitive destination folds into one,
    /// or two source roots colliding. Published one, refused the other, overwrote
    /// neither. Raised by the engine, not here.
    DestinationNamespaceCollision,
    /// §105, §207. A destination-side failure with no more specific code -- including
    /// a name or path the destination refuses as too long even through the
    /// extended-length call path. Path-scoped: it fails that ACTION, not the operation.
    DestinationError,
    /// §127, §259.11. A symlink cannot be created at the destination; action-scoped.
    /// This version recreates no symlinks at all, so the CLI reports every symlink it
    /// meets - inside a tree, or given as SOURCE - with this code (cut 5, K5 and K7).
    SymlinkCreationUnavailable,
    IoError,
    /// §96, §99 (cut 7a). The destination's target lock belongs to another run, or this run lost it mid-run (a failed
    /// `still_owned`). The engine raises it from its guard and stops the whole operation, never one file.
    TargetLockBusy,
    /// §259.3 (cut 7a). A source entry whose destination is a reserved Flux control path - `DEST/.flux/operations`,
    /// `standalone` or `atomic`, or below one. Path-scoped: that entry fails, and the rest of the copy continues.
    ControlPlaneNamespaceConflict,
    /// Cut 9a. A state file that is zero-length or cannot be decoded.
    StateCorrupt,
    /// Cut 9a. A state file written in another format than this build reads.
    IncompatibleState,
    /// Cut 9f-1. A checkpoint of the write-ahead log, or the writer that records it, failed. Stops the whole operation.
    WalWriteFailed,
    /// Cut 9f-1. The checkpoint log was written by a newer Flux (an unknown format or checksum version). Stops the
    /// whole operation before any file is copied; `--restart` supersedes it.
    WalFormatUnsupported,
    /// Cut 12, master §15/§16.1. The required link could not be created at the destination (the filesystem, volume
    /// or platform cannot link there, or the hardlink index or anchor cannot be used). Under `--hardlinks=preserve`
    /// that member fails with this code; under `auto` it is copied independently and counted. Path-scoped.
    HardlinkUnavailable,
    /// Cut 12, master §108. `--hardlinks=preserve` was requested but reliable object identity is unavailable, so the
    /// member cannot be checked. Path-scoped.
    HardlinkIdentityUnavailable,
    /// Cut 12, master §92. A dependent was not linked because its group reached terminal `Failed`: its canonical
    /// member failed to copy, so there is nothing to link to. Path-scoped; the tree run reports it as a failed
    /// `Copy` cause, so it counts as a failed file.
    BlockedByCanonicalFailure,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::CopyFailed => "COPY_FAILED",
            Code::MetadataApplyFailed => "METADATA_APPLY_FAILED",
            Code::DiskFull => "DISK_FULL",
            Code::PermissionDenied => "PERMISSION_DENIED",
            Code::SourceChanged => "SOURCE_CHANGED",
            Code::StrictDurabilityUnavailable => "STRICT_DURABILITY_UNAVAILABLE",
            Code::SpecialFileUnsupported => "SPECIAL_FILE_UNSUPPORTED",
            Code::SafetyRejected => "SAFETY_REJECTED",
            Code::DirectoryChangedDuringScan => "DIRECTORY_CHANGED_DURING_SCAN",
            Code::NoReplacePublishUnavailable => "NOREPLACE_PUBLISH_UNAVAILABLE",
            Code::DestinationNamespaceCollision => "DESTINATION_NAMESPACE_COLLISION",
            Code::DestinationError => "DESTINATION_ERROR",
            Code::SymlinkCreationUnavailable => "SYMLINK_CREATION_UNAVAILABLE",
            Code::IoError => "IO_ERROR",
            Code::TargetLockBusy => "TARGET_LOCK_BUSY",
            Code::ControlPlaneNamespaceConflict => "CONTROL_PLANE_NAMESPACE_CONFLICT",
            Code::StateCorrupt => "STATE_CORRUPT",
            Code::IncompatibleState => "INCOMPATIBLE_STATE",
            Code::WalWriteFailed => "WAL_WRITE_FAILED",
            Code::WalFormatUnsupported => "WAL_FORMAT_UNSUPPORTED",
            Code::HardlinkUnavailable => "HARDLINK_UNAVAILABLE",
            Code::HardlinkIdentityUnavailable => "HARDLINK_IDENTITY_UNAVAILABLE",
            Code::BlockedByCanonicalFailure => "BLOCKED_BY_CANONICAL_FAILURE",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{}: {source}", code.as_str())]
pub struct FsError {
    pub code: Code,
    /// The failure itself, kept INTACT. Never wrap this to bolt on context: rebuilding
    /// it as `Error::other(format!(..))` destroys `raw_os_error()`, and MEASURED on
    /// Linux that turns `classify` from `DiskFull` into `IoError`. Context that belongs
    /// to a caller's workflow -- a staging temporary left behind, say -- belongs in that
    /// caller's own error type, not here: this is the portable filesystem contract and
    /// every operation shares it.
    #[source]
    pub source: std::io::Error,
}

impl FsError {
    /// Classify an OS error WITHOUT consuming it. The mapping lives here, once,
    /// because the raw numbers differ per OS: callers use this rather than branching
    /// on an `errno` themselves. `IoError` means "this layer could not classify it",
    /// which lets a caller substitute a code that fits where the failure happened.
    pub fn classify(source: &std::io::Error) -> Code {
        if source.raw_os_error() == Some(ENOSPC_RAW) {
            Code::DiskFull
        } else if source.kind() == std::io::ErrorKind::PermissionDenied {
            Code::PermissionDenied
        } else {
            Code::IoError
        }
    }

    /// Map an OS error to a spec code and keep it as the source.
    pub fn from_io(source: std::io::Error) -> Self {
        let code = Self::classify(&source);
        FsError { code, source }
    }

    pub fn new(code: Code, source: std::io::Error) -> Self {
        FsError { code, source }
    }
}

pub type Result<T> = std::result::Result<T, FsError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_the_spec_string() {
        assert_eq!(Code::CopyFailed.as_str(), "COPY_FAILED");
        assert_eq!(Code::MetadataApplyFailed.as_str(), "METADATA_APPLY_FAILED");
        assert_eq!(Code::DiskFull.as_str(), "DISK_FULL");
        assert_eq!(Code::PermissionDenied.as_str(), "PERMISSION_DENIED");
        assert_eq!(Code::SourceChanged.as_str(), "SOURCE_CHANGED");
        assert_eq!(Code::StrictDurabilityUnavailable.as_str(), "STRICT_DURABILITY_UNAVAILABLE");
        assert_eq!(Code::SpecialFileUnsupported.as_str(), "SPECIAL_FILE_UNSUPPORTED");
        assert_eq!(Code::SafetyRejected.as_str(), "SAFETY_REJECTED");
        assert_eq!(Code::IoError.as_str(), "IO_ERROR");
        assert_eq!(Code::DirectoryChangedDuringScan.as_str(), "DIRECTORY_CHANGED_DURING_SCAN");
        assert_eq!(Code::NoReplacePublishUnavailable.as_str(), "NOREPLACE_PUBLISH_UNAVAILABLE");
        assert_eq!(Code::DestinationNamespaceCollision.as_str(), "DESTINATION_NAMESPACE_COLLISION");
        assert_eq!(Code::DestinationError.as_str(), "DESTINATION_ERROR");
        assert_eq!(Code::SymlinkCreationUnavailable.as_str(), "SYMLINK_CREATION_UNAVAILABLE");
        assert_eq!(Code::TargetLockBusy.as_str(), "TARGET_LOCK_BUSY");
        assert_eq!(Code::StateCorrupt.as_str(), "STATE_CORRUPT");
        assert_eq!(Code::IncompatibleState.as_str(), "INCOMPATIBLE_STATE");
        assert_eq!(
            Code::ControlPlaneNamespaceConflict.as_str(),
            "CONTROL_PLANE_NAMESPACE_CONFLICT"
        );
        assert_eq!(Code::HardlinkUnavailable.as_str(), "HARDLINK_UNAVAILABLE");
        assert_eq!(Code::HardlinkIdentityUnavailable.as_str(), "HARDLINK_IDENTITY_UNAVAILABLE");
        assert_eq!(Code::BlockedByCanonicalFailure.as_str(), "BLOCKED_BY_CANONICAL_FAILURE");
    }

    #[test]
    fn enospc_maps_to_disk_full() {
        let io = std::io::Error::from_raw_os_error(ENOSPC_RAW);
        assert_eq!(FsError::from_io(io).code, Code::DiskFull);
    }

    #[test]
    fn permission_denied_maps_to_permission_denied() {
        let io = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(FsError::from_io(io).code, Code::PermissionDenied);
    }

    #[test]
    fn classify_does_not_consume_the_error() {
        let io = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(FsError::classify(&io), Code::PermissionDenied);
        // still usable: this is what lets a caller pick its own fallback code
        assert_eq!(io.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn wal_write_failed_has_its_taxonomy_string() {
        assert_eq!(Code::WalWriteFailed.as_str(), "WAL_WRITE_FAILED");
        assert_eq!(Code::WalFormatUnsupported.as_str(), "WAL_FORMAT_UNSUPPORTED");
    }

    #[test]
    fn anything_else_maps_to_io_error() {
        let io = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(FsError::from_io(io).code, Code::IoError);
    }
}
