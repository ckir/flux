//! Recursive copy (cut 4b): `copy_tree` drives the ordered walk and `copy_file_at`,
//! writing only through destination directory handles (§149.7).
//!
//! Design authority: `docs/superpowers/specs/2026-09-26-cut-4b-copy-tree-design.md`.

use crate::copy::CopyError;
use flux_fs::{FileIdentity, FileType, FsError, MetadataFailure};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a tree copy did, counted. Individual failures are STREAMED to the sink as
/// they happen, never accumulated here, so a tree with a million failures costs what a
/// clean one does.
#[derive(Debug, Default)]
pub struct TreeOutcome {
    pub files_copied: u64,
    pub bytes_copied: u64,
    /// Directories THIS operation created, the root included when it made it. A
    /// pre-existing directory merged into is not counted.
    pub directories_created: u64,
    pub failures: FailureTally,
    /// Identity comparisons that were SKIPPED because a side was not `Strong`. Not
    /// failures: the copies happened. The engine captures; cut 5's CLI renders.
    pub warnings: WeakIdentityWarnings,
}

#[derive(Debug)]
pub struct TreeFailure {
    /// Relative to the source root, as the walk reports it.
    pub path: PathBuf,
    pub cause: TreeFailureCause,
}

/// Each variant is defined by WHERE the failure happened, so no failure can be
/// expressed two ways.
#[derive(Debug)]
pub enum TreeFailureCause {
    /// The walk could not read or enter something.
    Walk(FsError),
    /// A destination directory could not be created or entered - including a
    /// `DESTINATION_NAMESPACE_COLLISION` between two source names the destination folds
    /// into one. Its subtree is skipped and reported once, here.
    CreateDir(FsError),
    /// One file's copy failed. `CopyError` intact, so a leftover temporary is still
    /// reported per file.
    Copy(CopyError),
    /// A symlink or other non-regular entry, which this cut does not recreate.
    Unsupported(FileType),
    /// The file IS at the destination; some of its metadata could not be applied.
    /// Published with complaints, which exits 1 like the single-file case.
    PublishedWithComplaints(Vec<MetadataFailure>),
}

/// One counter per `TreeFailureCause` variant. Fixed size: it costs the same on a
/// clean tree and on one where every entry failed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FailureTally {
    pub walk: u64,
    pub create_dir: u64,
    pub copy: u64,
    pub unsupported: u64,
    pub published_with_complaints: u64,
}

impl FailureTally {
    /// The only total. Derived, never stored beside the parts.
    pub fn total(&self) -> u64 {
        self.walk + self.create_dir + self.copy + self.unsupported + self.published_with_complaints
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// EXHAUSTIVE, with no wildcard arm: a new cause must name its counter here or the
    /// crate does not compile. A `_ =>` arm would let a new cause count nothing, and the
    /// miscount would surface only as a wrong exit code.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "copy_tree calls it; Task 5 removes this line")
    )]
    pub(crate) fn count(&mut self, cause: &TreeFailureCause) {
        match cause {
            TreeFailureCause::Walk(_) => self.walk += 1,
            TreeFailureCause::CreateDir(_) => self.create_dir += 1,
            TreeFailureCause::Copy(_) => self.copy += 1,
            TreeFailureCause::Unsupported(_) => self.unsupported += 1,
            TreeFailureCause::PublishedWithComplaints(_) => self.published_with_complaints += 1,
        }
    }
}

/// Skipped identity comparisons, aggregated for one warning apiece (walker design,
/// "When identity is weak or unavailable").
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WeakIdentityWarnings {
    /// One group per affected volume (`ObjectId::volume`), in stable order.
    pub weak: BTreeMap<u64, DegradedGroup>,
    /// Everything whose identity was `Unavailable`, which names no volume to group by.
    pub unavailable: Option<DegradedGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradedGroup {
    pub count: u64,
    /// The first path that degraded, relative to the source root. The EMPTY path is the
    /// source root itself (the §129 pre-flight).
    pub example: PathBuf,
}

impl WeakIdentityWarnings {
    pub fn is_empty(&self) -> bool {
        self.weak.is_empty() && self.unavailable.is_none()
    }

    /// Record one comparison that was skipped. `weaker` is the weaker side, as
    /// `copy::weaker` computes it; `Strong` is not a degradation and is ignored.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "copy_tree calls it; Task 5 removes this line")
    )]
    pub(crate) fn record(&mut self, weaker: FileIdentity, example: &Path) {
        let group = match weaker {
            FileIdentity::Strong(_) => return,
            FileIdentity::Weak(id) => self
                .weak
                .entry(id.volume)
                .or_insert_with(|| DegradedGroup { count: 0, example: example.to_path_buf() }),
            FileIdentity::Unavailable => self
                .unavailable
                .get_or_insert_with(|| DegradedGroup { count: 0, example: example.to_path_buf() }),
        };
        group.count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_fs::{Code, ObjectId};

    #[test]
    fn the_tally_counts_each_cause_in_its_own_field() {
        let e = || FsError::new(Code::IoError, std::io::Error::other("x"));
        let mut t = FailureTally::default();
        t.count(&TreeFailureCause::Walk(e()));
        t.count(&TreeFailureCause::CreateDir(e()));
        t.count(&TreeFailureCause::Copy(CopyError::at(crate::copy::CopyStep::Create, e())));
        t.count(&TreeFailureCause::Unsupported(FileType::Symlink));
        t.count(&TreeFailureCause::PublishedWithComplaints(Vec::new()));
        assert_eq!(
            t,
            FailureTally {
                walk: 1,
                create_dir: 1,
                copy: 1,
                unsupported: 1,
                published_with_complaints: 1
            }
        );
        assert_eq!(t.total(), 5);
        assert!(!t.is_empty());
        assert!(FailureTally::default().is_empty());
    }

    #[test]
    fn warnings_group_weak_by_volume_and_keep_the_first_example() {
        let weak = |volume| FileIdentity::Weak(ObjectId { volume, index: 1 });
        let mut w = WeakIdentityWarnings::default();
        w.record(weak(7), Path::new("a"));
        w.record(weak(7), Path::new("b"));
        w.record(weak(9), Path::new("c"));
        w.record(FileIdentity::Unavailable, Path::new("d"));
        w.record(FileIdentity::Strong(ObjectId { volume: 1, index: 1 }), Path::new("e"));

        assert_eq!(w.weak[&7], DegradedGroup { count: 2, example: PathBuf::from("a") });
        assert_eq!(w.weak[&9], DegradedGroup { count: 1, example: PathBuf::from("c") });
        assert_eq!(w.unavailable, Some(DegradedGroup { count: 1, example: PathBuf::from("d") }));
        assert!(!w.is_empty());
        assert!(WeakIdentityWarnings::default().is_empty());
    }
}
