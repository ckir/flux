//! Cut 11a: the types a dry run (`flux copy --dry-run`) is built from.

use crate::state::OpState;
use flux_fs::{
    ClaimKey, ClaimOutcome, ClaimRecord, ClaimStore, FluxPathKey, PreparedRecord, RecoveryOp,
    Result,
};
use std::path::PathBuf;

/// What kind of object a dry run plans to touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedObject {
    File,
    Dir,
    Symlink,
    State,
    Partial,
}

impl PlannedObject {
    pub fn as_str(self) -> &'static str {
        match self {
            PlannedObject::File => "file",
            PlannedObject::Dir => "dir",
            PlannedObject::Symlink => "symlink",
            PlannedObject::State => "state",
            PlannedObject::Partial => "partial",
        }
    }
}

/// Why a planned entry would be skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Existing,
    UpToDate,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::Existing => "existing",
            SkipReason::UpToDate => "up to date",
        }
    }
}

/// What a dry run would do with one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedAction {
    CreateDir,
    Copy,
    /// Cut 11b: a copy whose half-copied temporary (`len` bytes) is present; its continuation is not previewed.
    CopyOverPartial {
        len: u64,
    },
    Overwrite,
    Skip(SkipReason),
    CreateSymlink,
    Resume,
    Discard,
}

impl PlannedAction {
    pub fn as_str(self) -> &'static str {
        match self {
            PlannedAction::CreateDir => "would_create",
            PlannedAction::Copy | PlannedAction::CopyOverPartial { .. } => "would_copy",
            PlannedAction::Overwrite => "would_overwrite",
            PlannedAction::Skip(_) => "would_skip",
            PlannedAction::CreateSymlink => "would_create_symlink",
            PlannedAction::Resume => "would_resume",
            PlannedAction::Discard => "would_discard",
        }
    }
}

/// One planned entry: the object and what would happen to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedEntry {
    pub object: PlannedObject,
    pub action: PlannedAction,
}

/// What a dry run tells the CLI beyond the counters. `planned` is a single file's one entry, `None` for a tree.
/// `prior_seen` is whether the prior-operation scan pushed at least one `DryRunPrior*` warning (the CLI's
/// `prior_operation` in `unprobed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRun {
    pub unprobed_noreplace: bool,
    pub prior_seen: bool,
    pub planned: Option<PlannedEntry>,
    /// Cut 11b: a readable lock record whose owner's liveness was not probed (the dual outcome).
    pub owner_liveness: bool,
    /// Cut 11b: `--restart --break-lock` would take an uncertain lock over.
    pub lock_takeover: bool,
    /// Cut 11b: what `--resume` or `--restart` would do with the prior operations.
    pub prior: Option<DryPrior>,
    /// Cut 11b: half-copied files found beside targets the walk would write (copied or overwritten) under `--resume`;
    /// their continuation is not previewed.
    pub partials_present: u64,
}

/// Cut 11b: what a dry `--resume` or `--restart` would do with the prior operations at DEST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DryPrior {
    /// No prior operation: the run would start a new one.
    StartedNew,
    /// `--resume` would continue operation `operation_id`: `claims` of its claim records (`None` when not counted),
    /// `recovered` prepared notes recovered, `strays` stray temporaries.
    Resume { operation_id: String, claims: Option<u64>, recovered: u64, strays: u64 },
    /// `--restart` would supersede the listed operations (id and state), discarding `partials` half-copied files and
    /// `states` state directories.
    Restart {
        superseded: Vec<(String, OpState)>,
        /// Every `<name>.flux-partial.<prior id>` a real run would delete: DEST-relative for a tree, the bare name
        /// for a single file; the CLI prints the single-file discard lines from `superseded` and this list, a tree's
        /// come through the streamed `Planned` records.
        partials_found: Vec<PathBuf>,
        states: u64,
    },
}

/// A claim store that stores nothing: every insert is fresh, every lookup is absent. Holds no state.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullClaimStore;

impl ClaimStore for NullClaimStore {
    fn insert_if_absent(&mut self, _key: &ClaimKey, _record: &ClaimRecord) -> Result<ClaimOutcome> {
        Ok(ClaimOutcome::Inserted)
    }
    fn get(&self, _key: &ClaimKey) -> Result<Option<ClaimRecord>> {
        Ok(None)
    }
    fn upgrade_own_claim(&mut self, _key: &ClaimKey, _target: &FluxPathKey) -> Result<()> {
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
    fn count(&self) -> Result<u64> {
        Ok(0)
    }
    fn supports_prepared(&self) -> bool {
        false
    }
    fn prepare(&mut self, _key: &ClaimKey, _record: &PreparedRecord) -> Result<()> {
        Ok(())
    }
    fn prepare_many(&mut self, _notes: &[(ClaimKey, PreparedRecord)]) -> Result<()> {
        Ok(())
    }
    fn commit_prepared(
        &mut self,
        _key: &ClaimKey,
        _target: &FluxPathKey,
        _planned: Option<&ClaimKey>,
    ) -> Result<()> {
        Ok(())
    }
    fn discard_prepared(&mut self, _key: &ClaimKey) -> Result<()> {
        Ok(())
    }
    fn prepared(&self) -> Result<Vec<(ClaimKey, PreparedRecord)>> {
        Ok(Vec::new())
    }
    fn apply_recovery(&mut self, _ops: &[RecoveryOp]) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_fs::{ClaimKey, ClaimOutcome, ClaimRecord, ClaimStatus, ClaimStore, FluxPathKey};
    use flux_fs::{ObjectId, PreparedRecord};
    use std::ffi::OsStr;

    #[test]
    fn the_null_store_answers_absent_inserted_zero_and_no_prepared_support() {
        let key = ClaimKey::new(ObjectId { volume: 1, index: 1 }, OsStr::new("a"));
        let target = FluxPathKey(b"a".to_vec());
        let record = ClaimRecord { target: target.clone(), status: ClaimStatus::Created };
        let note = PreparedRecord {
            target: target.clone(),
            temp_name: b"t".to_vec(),
            identity: String::new(),
            dir_path: String::new(),
            name: b"a".to_vec(),
            planned_name: Vec::new(),
            replacement: false,
        };
        let mut s = NullClaimStore;
        assert!(matches!(s.insert_if_absent(&key, &record).unwrap(), ClaimOutcome::Inserted));
        assert_eq!(s.get(&key).unwrap(), None);
        assert_eq!(s.count().unwrap(), 0);
        assert!(!s.supports_prepared());
        assert!(s.prepared().unwrap().is_empty());
        s.upgrade_own_claim(&key, &target).unwrap();
        s.flush().unwrap();
        s.prepare(&key, &note).unwrap();
        s.prepare_many(&[]).unwrap();
        s.commit_prepared(&key, &target, None).unwrap();
        s.discard_prepared(&key).unwrap();
        s.apply_recovery(&[]).unwrap();
    }

    #[test]
    fn every_planned_action_and_object_has_its_stream_word() {
        assert_eq!(PlannedAction::CreateDir.as_str(), "would_create");
        assert_eq!(PlannedAction::Copy.as_str(), "would_copy");
        assert_eq!(PlannedAction::CopyOverPartial { len: 3 }.as_str(), "would_copy");
        assert_eq!(PlannedAction::Overwrite.as_str(), "would_overwrite");
        assert_eq!(PlannedAction::Skip(SkipReason::UpToDate).as_str(), "would_skip");
        assert_eq!(PlannedAction::Skip(SkipReason::Existing).as_str(), "would_skip");
        assert_eq!(PlannedAction::CreateSymlink.as_str(), "would_create_symlink");
        assert_eq!(SkipReason::Existing.as_str(), "existing");
        assert_eq!(SkipReason::UpToDate.as_str(), "up to date");
        assert_eq!(PlannedObject::File.as_str(), "file");
        assert_eq!(PlannedObject::Dir.as_str(), "dir");
        assert_eq!(PlannedObject::Symlink.as_str(), "symlink");
        assert_eq!(PlannedAction::Resume.as_str(), "would_resume");
        assert_eq!(PlannedAction::Discard.as_str(), "would_discard");
        assert_eq!(PlannedObject::State.as_str(), "state");
        assert_eq!(PlannedObject::Partial.as_str(), "partial");
    }
}
