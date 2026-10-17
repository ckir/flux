//! Cut 11b: the dry run's read-only preview of the prior operations' lock and state.

use super::RunWarning;
use crate::lock::record::{Decoded, LockRecord, RECORD_LEN, Uncertain, decode};
use crate::lock::site::{LockSite, Workspace};
use crate::lock::{
    LockError, foreign_refusal, orphan_refusal, uncertain_refusal, untrusted_refusal,
};
use flux_fs::{Code, DirHandle, FsError, LockCapability};
use std::io::ErrorKind;

/// What a dry run found at the lock path, read without opening or trying the lock.
// Task 3 wires the dry run's lock preview.
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
// Task 3 wires the dry run's lock preview.
#[allow(dead_code)]
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
