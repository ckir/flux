// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

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

/// What a copy does when a file already exists at the destination (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingPolicy {
    /// Replace it (the default).
    Overwrite,
    /// Replace it when the source is newer or the size differs; when either
    /// modification time is unavailable, compare sizes only; equal: keep it.
    Update,
    /// Leave it untouched.
    SkipExisting,
}

/// What a copy does with a symlink in the source tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Links {
    /// Recreate it as a link, payload verbatim.
    Copy,
    /// Leave it out.
    Skip,
}

/// What a tree copy does with a hardlink group in the source tree (master §15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hardlinks {
    /// Recreate the group where the destination can, and copy the member separately where it cannot (the default).
    Auto,
    /// Recreate the group, and fail a member that cannot be linked.
    Preserve,
    /// Copy every member as its own file: today's behaviour.
    Copy,
}

/// How much of a copy is verified by hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerifyLevel {
    #[default]
    None,
    SourceStream,
    Destination,
    Full,
}

/// The hash used for verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HashAlgorithm {
    #[default]
    Blake3,
}

#[derive(Debug, Clone)]
pub struct CopyOptions {
    pub preserve_times: Preserve,
    pub preserve_permissions: Preserve,
    pub durability: Durability,
    pub publish: Publish,
    pub safety: Safety,
    pub operation_id: OperationId,
    pub existing: ExistingPolicy,
    pub links: Links,
    pub hardlinks: Hardlinks,
    pub cross_filesystems: bool,
    pub verify: VerifyLevel,
    pub hash: HashAlgorithm,
    /// Cut 16: which entries a walk selects (`--exclude`, `--recursive`/`--no-recursive`); the default selects everything.
    pub selection: crate::Selection,
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
    /// The published target's identity, read from the temporary's own handle just before the publishing rename (a
    /// rename keeps the object; cut 7b). `Unavailable` where the platform cannot say.
    pub published_identity: crate::FileIdentity,
    /// `true`: the existing destination was left untouched; `bytes_copied == 0`.
    pub skipped: bool,
    /// `true`: the copy was verified by hashing at the requested level.
    pub verified: bool,
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
    fn hardlinks_default_is_not_a_bool() {
        // Three modes, so a boolean cannot express it (master §15).
        assert_ne!(Hardlinks::Auto, Hardlinks::Preserve);
        assert_ne!(Hardlinks::Preserve, Hardlinks::Copy);
        assert_ne!(Hardlinks::Auto, Hardlinks::Copy);
    }

    #[test]
    fn verify_defaults_to_none_and_hash_to_blake3() {
        assert_eq!(VerifyLevel::default(), VerifyLevel::None);
        assert_eq!(HashAlgorithm::default(), HashAlgorithm::Blake3);
    }

    #[test]
    fn preserve_distinguishes_off_from_default() {
        // §44.1 treats "not requested" and "requested leniently" differently, so a
        // boolean cannot express this.
        assert_ne!(Preserve::Off, Preserve::Default);
        assert_ne!(Preserve::Default, Preserve::Strict);
    }

    #[test]
    fn a_clean_outcome_has_no_metadata_failures() {
        let o = Outcome {
            bytes_copied: 10,
            metadata_failures: Vec::new(),
            identity_degraded: None,
            published_identity: crate::FileIdentity::Unavailable,
            skipped: false,
            verified: false,
        };
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
