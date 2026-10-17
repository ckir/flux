//! Cut 11b: the dry run's read-only preview of the prior operations' lock and state.

use super::RunWarning;
use super::sweep::partials_under;
use crate::dry_run::{DryPrior, PlannedAction, PlannedEntry, PlannedObject};
use crate::lock::record::{Decoded, LockRecord, RECORD_LEN, Uncertain, decode};
use crate::lock::site::{LockSite, Workspace};
use crate::lock::{
    LockError, foreign_refusal, orphan_refusal, uncertain_refusal, untrusted_refusal,
};
use crate::prior::PriorOp;
use crate::tree::{TreeFailure, TreeFailureCause};
use flux_fs::{Code, DestinationRoot, DirHandle, FsError, LockCapability};
use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// What a dry run found at the lock path, read without opening or trying the lock.
// The `Takeover` and `Recorded` payloads are read by the resume and restart previews (Tasks 4 and 6).
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum LockPreview {
    /// No lock file: a real run would create one.
    Absent,
    /// A real run would refuse with this.
    Refused(LockError),
    /// `--break-lock` would take this uncertain record over.
    Takeover(Uncertain),
    /// A readable record whose workspace is trusted and present.
    Recorded(LockRecord),
    /// The read itself failed.
    Error(FsError),
}

/// Spec 3.2: classify the lock file from its bytes alone. It never opens the lock, so a lock held by another
/// run reads the same as a dead one's.
pub(crate) fn dry_lock<D: DirHandle>(
    site: &LockSite<'_, D>,
    break_lock: bool,
    capability: LockCapability,
    now_ns: u64,
    warnings: &mut Vec<RunWarning>,
) -> LockPreview {
    let preview = match site.dir().read_file(site.lock_name(), RECORD_LEN) {
        Err(e) if e.source.kind() == ErrorKind::NotFound => LockPreview::Absent,
        Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
            LockPreview::Refused(foreign_refusal())
        }
        Err(e) => return LockPreview::Error(e),
        Ok(bytes) => match decode(&bytes) {
            Decoded::Foreign => LockPreview::Refused(foreign_refusal()),
            Decoded::Uncertain(why) => {
                if break_lock && capability.allows_exclusive() {
                    warnings.push(RunWarning::DryRunLockTakeover { why: format!("{why:?}") });
                    LockPreview::Takeover(why)
                } else {
                    LockPreview::Refused(uncertain_refusal(why))
                }
            }
            Decoded::Record(r) => match site.workspace(&r) {
                Err(LockError::Io(e)) => return LockPreview::Error(e),
                Err(refused) => LockPreview::Refused(refused),
                Ok(Workspace::Untrusted) => LockPreview::Refused(untrusted_refusal(r)),
                Ok(Workspace::Missing) => LockPreview::Refused(orphan_refusal(r, site.kind())),
                Ok(Workspace::Trusted) => {
                    warnings.push(RunWarning::DryRunLockRecord {
                        operation_id: r.operation_id.clone(),
                        owner_instance_id: r.owner_instance_id.clone(),
                        boot_session_id: r.boot_session_id.clone(),
                        // None when the heartbeat is in the future.
                        heartbeat_age_secs: now_ns
                            .checked_sub(r.last_heartbeat_wall_time)
                            .map(|d| d / 1_000_000_000),
                        workspace_path: r.workspace_path.clone(),
                    });
                    LockPreview::Recorded(r)
                }
            },
        },
    };
    // The `.broken.` leftovers a real run would reclaim. Best-effort: a listing error is ignored, since an
    // unreadable directory would already have failed the record read above.
    let mut prefix = site.lock_name().as_encoded_bytes().to_vec();
    prefix.extend_from_slice(b".broken.");
    if let Ok(entries) = site.dir().read_dir() {
        for entry in entries {
            if entry.name.as_encoded_bytes().starts_with(&prefix) {
                warnings.push(RunWarning::DryRunBrokenLeftover {
                    name: entry.name.to_string_lossy().into_owned(),
                });
            }
        }
    }
    preview
}

/// One streamed `Planned(object, Discard)` record.
fn discard(path: PathBuf, object: PlannedObject) -> TreeFailure {
    TreeFailure {
        path,
        cause: TreeFailureCause::Planned(PlannedEntry { object, action: PlannedAction::Discard }),
    }
}

/// Spec 3.5, a tree: what `--restart` would supersede, listed read-only. Per prior, in order, its state directory,
/// then every partial the sweep would delete (`partials_under`, the walk the real sweep uses), each streamed as a
/// planned discard. A directory the walk cannot list is reported through `warnings`, as the real sweep does.
pub(crate) fn dry_restart<F: DestinationRoot>(
    fs: &F,
    dest_shown: &Path,
    operations_shown: &Path,
    priors: &[PriorOp],
    warnings: &mut Vec<RunWarning>,
    on_report: &mut dyn FnMut(TreeFailure),
) -> DryPrior {
    for prior in priors {
        let path = PathBuf::from(".flux/operations").join(&prior.state.operation_id);
        on_report(discard(path, PlannedObject::State));
    }
    let ids: BTreeSet<&str> = priors.iter().map(|p| p.state.operation_id.as_str()).collect();
    let (found, _) = if ids.is_empty() {
        (Vec::new(), false)
    } else {
        partials_under(fs, dest_shown, operations_shown, &ids, warnings)
    };
    let mut partials_found = Vec::with_capacity(found.len());
    for (path, _) in found {
        on_report(discard(path.clone(), PlannedObject::Partial));
        partials_found.push(path);
    }
    DryPrior::Restart {
        superseded: priors.iter().map(|p| (p.state.operation_id.clone(), p.state.state)).collect(),
        partials_found,
        states: priors.len() as u64,
    }
}
