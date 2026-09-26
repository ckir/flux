//! What a copy was asked to do, and what it did.

use std::path::{Path, PathBuf};

/// Three states, not two. §44.1 makes a strict failure prevent publication while a
/// default failure does not, and "not requested at all" is a third thing again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preserve {
    /// Explicitly requested. Failure fails the file's action; nothing is published.
    Strict,
    /// Applied by default. Failure is reported; the file is still published.
    Default,
    /// Not attempted.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    Normal,
    Strict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publish {
    Replace,
    NoReplace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataItem {
    Times,
    Permissions,
}

/// §18.1: the textual form is implementation-defined but must be collision-resistant
/// and deterministic enough for discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationId(String);

impl OperationId {
    pub fn new(s: impl Into<String>) -> Self {
        OperationId(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether a safety check that cannot be made with full confidence refuses or degrades.
///
/// An enum rather than a bool for the reason `Preserve`, `Durability` and `Publish` are:
/// named states read better at every construction site, and a third mode is easy to add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    /// Degrade to the lexical floor when identity is not `Strong` on both sides, and
    /// report the degradation (`Outcome::identity_degraded`).
    Default,
    /// Refuse whenever identity is not `Strong` on both sides of an existing destination.
    Strict,
}

#[derive(Debug, Clone)]
pub struct CopyOptions {
    pub preserve_times: Preserve,
    pub preserve_permissions: Preserve,
    pub durability: Durability,
    pub publish: Publish,
    pub safety: Safety,
    pub operation_id: OperationId,
}

#[derive(Debug)]
pub struct MetadataFailure {
    pub item: MetadataItem,
    pub error: crate::FsError,
}

#[derive(Debug)]
pub struct Outcome {
    pub bytes_copied: u64,
    /// Empty on a clean copy. Non-empty means published-with-complaints: the caller
    /// reports each as METADATA_APPLY_FAILED and the operation exits 1.
    pub metadata_failures: Vec<MetadataFailure>,
    /// `Some` when the Step 2a identity gate could not compare the source with an
    /// EXISTING destination and fell back to the lexical floor. Carries the WEAKER of
    /// the two sides, which is what a warning aggregates on: `Weak(id)` names
    /// `id.volume`, `Unavailable` names nothing. `None` on the normal path AND when the
    /// destination did not exist, which is not a degradation.
    pub identity_degraded: Option<crate::FileIdentity>,
}

/// `<target>.flux-partial.<operation-id>`, in the target's directory (§18.1, normative).
pub fn temp_path(target: &Path, id: &OperationId) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".flux-partial.");
    name.push(id.as_str());
    target.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserve_distinguishes_off_from_default() {
        // §44.1 treats "not requested" and "requested leniently" differently, so a
        // boolean cannot express this.
        assert_ne!(Preserve::Off, Preserve::Default);
        assert_ne!(Preserve::Default, Preserve::Strict);
    }

    #[test]
    fn a_clean_outcome_has_no_metadata_failures() {
        let o =
            Outcome { bytes_copied: 10, metadata_failures: Vec::new(), identity_degraded: None };
        assert!(o.metadata_failures.is_empty());
        assert_eq!(o.bytes_copied, 10);
    }

    #[test]
    fn temp_name_follows_the_normative_structure() {
        // §18.1 makes `<target>.flux-partial.<operation-id>` normative.
        let id = OperationId::new("abc123");
        let p = temp_path(std::path::Path::new("/dest/file.iso"), &id);
        assert_eq!(p.file_name().unwrap(), "file.iso.flux-partial.abc123");
    }
}
