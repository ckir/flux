//! The spec's error codes, and the `io::Error` wrapper that carries one.

/// `ENOSPC` on Unix, `ERROR_DISK_FULL` on Windows.
#[cfg(unix)]
pub(crate) const ENOSPC_RAW: i32 = 28;
#[cfg(windows)]
pub(crate) const ENOSPC_RAW: i32 = 112;

/// A spec error code. Only codes single-file copy can actually produce.
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
    fn anything_else_maps_to_io_error() {
        let io = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(FsError::from_io(io).code, Code::IoError);
    }
}
