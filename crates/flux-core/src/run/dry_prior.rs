// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Cut 11b: the dry run's read-only preview of the prior operations' lock and state.

use super::place::{BY_HAND, Place, TreePlace, refused_state, state_error};
use super::recover::{RecoverError, recover_publications, uncertain_detail};
use super::resume::validate;
use super::session::failed;
use super::sweep::partials_under;
use super::{RunError, RunStep, RunWarning};
use crate::dry_run::{DryPrior, PlannedAction, PlannedEntry, PlannedObject};
use crate::lock::record::{Decoded, LockRecord, RECORD_LEN, Uncertain, decode};
use crate::lock::site::{LockSite, Workspace};
use crate::lock::{
    LockCode, LockError, foreign_refusal, orphan_refusal, takeover_unavailable_refusal,
    uncertain_refusal, untrusted_refusal,
};
use crate::prior::PriorOp;
use crate::state::{FLUX_DIR, OPERATIONS_DIR, OpState, STATE_DB};
use crate::tree::{TreeFailure, TreeFailureCause};
use flux_fs::{
    ClaimStore, Code, CopyOptions, DestinationRoot, DirEntry, DirHandle, FileHandle, FileIdentity,
    FsError, LockCapability,
};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

/// What a dry run found at the lock path, read without opening or trying the lock.
// The `Takeover` and `Recorded` payloads are read by the resume and restart previews (Task 6).
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
/// run reads the same as a dead one's. The `bool` is false where the read itself failed, so no leftover scan follows.
pub(crate) fn dry_lock_record<D: DirHandle>(
    site: &LockSite<'_, D>,
    break_lock: bool,
    capability: LockCapability,
    now_ns: u64,
    warnings: &mut Vec<RunWarning>,
) -> (LockPreview, bool) {
    let preview = match site.dir().read_file(site.lock_name(), RECORD_LEN) {
        Err(e) if e.source.kind() == ErrorKind::NotFound => LockPreview::Absent,
        Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
            LockPreview::Refused(foreign_refusal())
        }
        Err(e) => return (LockPreview::Error(e), false),
        Ok(bytes) => match decode(&bytes) {
            Decoded::Foreign => LockPreview::Refused(foreign_refusal()),
            Decoded::Uncertain(why) => {
                if !break_lock {
                    LockPreview::Refused(uncertain_refusal(why))
                } else {
                    // The live takeover's first check (S240_5_s1): strong identity and OS-native locks.
                    match site.dir().identity() {
                        Err(e) => return (LockPreview::Error(e), false),
                        Ok(id)
                            if matches!(id, FileIdentity::Strong(_))
                                && capability.allows_exclusive() =>
                        {
                            warnings
                                .push(RunWarning::DryRunLockTakeover { why: format!("{why:?}") });
                            LockPreview::Takeover(why)
                        }
                        Ok(_) => LockPreview::Refused(takeover_unavailable_refusal()),
                    }
                }
            }
            Decoded::Record(r) => match site.workspace(&r) {
                Err(LockError::Io(e)) => return (LockPreview::Error(e), false),
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
    (preview, true)
}

/// The `.broken.` leftovers of one lock that a real run would reclaim, noted from an already-read listing so
/// several locks can share one directory read.
pub(crate) fn broken_leftovers(
    entries: &[DirEntry],
    lock_name: &OsStr,
    warnings: &mut Vec<RunWarning>,
) {
    let mut prefix = lock_name.as_encoded_bytes().to_vec();
    prefix.extend_from_slice(b".broken.");
    for entry in entries {
        if entry.name.as_encoded_bytes().starts_with(&prefix) {
            warnings.push(RunWarning::DryRunBrokenLeftover {
                name: entry.name.to_string_lossy().into_owned(),
            });
        }
    }
}

/// `dry_lock_record`, then the leftover scan. The scan is best-effort: a listing error is ignored, since an
/// unreadable directory would already have failed the record read.
pub(crate) fn dry_lock<D: DirHandle>(
    site: &LockSite<'_, D>,
    break_lock: bool,
    capability: LockCapability,
    now_ns: u64,
    warnings: &mut Vec<RunWarning>,
) -> LockPreview {
    let (preview, scan) = dry_lock_record(site, break_lock, capability, now_ns, warnings);
    if scan && let Ok(entries) = site.dir().read_dir() {
        broken_leftovers(&entries, site.lock_name(), warnings);
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
    // The unlistable flag is ignored: `warnings` already carries the `PartialKept` message, and nothing is retired here.
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

/// Cut 11b, spec 3.4 step 2: the private directory `<temp root>/flux-dry-<operation id>` a dry `--resume` copies the
/// prior's claim store into. The operation id is unique per run, so the name needs no random suffix. Dropping it
/// removes `state.db` and the directory.
pub(crate) struct TempDir<D: DirHandle> {
    parent: D,
    name: OsString,
    dir: Option<D>,
}

impl<D: DirHandle> TempDir<D> {
    /// Create `<root>/flux-dry-<operation_id>` through a handle on `root`.
    pub(crate) fn make<F: DestinationRoot<Dir = D>>(
        fs: &F,
        root: &Path,
        operation_id: &str,
    ) -> Result<Self, FsError> {
        let parent = fs.destination_root(root)?;
        let name = OsString::from(format!("flux-dry-{operation_id}"));
        let dir = parent.create_dir(&name)?;
        Ok(TempDir { parent, name, dir: Some(dir) })
    }

    pub(crate) fn dir(&self) -> &D {
        self.dir.as_ref().expect("the handle is held until the drop")
    }
}

impl<D: DirHandle> Drop for TempDir<D> {
    fn drop(&mut self) {
        // The handle goes first, so a platform that cannot remove an open directory can.
        self.dir.take();
        // Best-effort: a removal that fails leaves the directory behind, and nothing reports it (a drop has no
        // channel, and the preview's result is already decided).
        let _ = self.parent.open_dir(&self.name).and_then(|d| d.remove_file(OsStr::new(STATE_DB)));
        let _ = self.parent.remove_dir(&self.name);
    }
}

/// Which side of `copy_store` failed: the prior's workspace (the reading) or the private copy (the writing).
pub(crate) enum CopyStoreError {
    Read(FsError),
    Write(FsError),
}

/// Spec 3.4 step 2: copy the workspace's `state.db` byte for byte into `temp`, synced. The on-disk store is only
/// read.
pub(crate) fn copy_store<D: DirHandle>(workspace: &D, temp: &D) -> Result<(), CopyStoreError> {
    let name = OsStr::new(STATE_DB);
    let mut reader = workspace.open_read(name).map_err(CopyStoreError::Read)?;
    let mut writer = temp.create_new(name).map_err(CopyStoreError::Write)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| CopyStoreError::Read(io_error(e)))?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).map_err(|e| CopyStoreError::Write(io_error(e)))?;
    }
    writer.sync_all().map_err(CopyStoreError::Write)
}

fn io_error(e: std::io::Error) -> FsError {
    FsError::new(FsError::classify(&e), e)
}

/// How the dry adoption of a prior ended: a repaired, recovered private copy of its store (with the directory that
/// holds it, which must outlive the store), or the refusal the real adoption would meet.
pub(crate) enum DryAdopt<D: DirHandle> {
    Store { temp: TempDir<D>, claims: D::Claims, prior: DryPrior },
    Refused(RunError),
}

/// The private copy's store and the directory holding it, parked by a dry `--resume` for the walk.
pub(crate) type Parked<F> = Option<(
    TempDir<<F as DestinationRoot>::Dir>,
    <<F as DestinationRoot>::Dir as DirHandle>::Claims,
)>;

/// Spec 3.4 steps 1-5: adopt `prior` the way a real `--resume` does, on a private copy of its store. Nothing under
/// DEST is written: the workspace is only read, and the repair and the commit recovery run on the copy.
pub(crate) fn dry_adopt<F: DestinationRoot>(
    fs: &F,
    place: &TreePlace<'_, F>,
    prior: &PriorOp,
    opts: &CopyOptions,
    temp_root: &Path,
    warnings: &mut Vec<RunWarning>,
) -> DryAdopt<F::Dir> {
    match adopt_on_copy(fs, place, prior, opts, temp_root, warnings) {
        Ok((temp, claims, prior)) => DryAdopt::Store { temp, claims, prior },
        Err(e) => DryAdopt::Refused(e),
    }
}

type Adopted<F> = (
    TempDir<<F as DestinationRoot>::Dir>,
    <<F as DestinationRoot>::Dir as DirHandle>::Claims,
    DryPrior,
);

fn adopt_on_copy<F: DestinationRoot>(
    fs: &F,
    place: &TreePlace<'_, F>,
    prior: &PriorOp,
    opts: &CopyOptions,
    temp_root: &Path,
    warnings: &mut Vec<RunWarning>,
) -> Result<Adopted<F>, RunError> {
    // 1. The compatibility table, as the real adoption applies it.
    validate(prior, &place.roots(), opts, warnings)?;
    let id = prior.state.operation_id.as_str();
    let created = prior.state.state == OpState::Created;
    let db = place.operations_shown().join(id).join(STATE_DB);
    // A prior found at DEST means `locate_tree` opened DEST.
    let dest = place.dest.as_ref().expect("a prior implies DEST exists");
    let workspace = dest
        .open_dir(OsStr::new(FLUX_DIR))
        .and_then(|d| d.open_dir(OsStr::new(OPERATIONS_DIR)))
        .and_then(|d| d.open_dir(OsStr::new(id)))
        .map_err(|e| failed(RunStep::State, &place.operations_shown().join(id), e))?;
    // 2. The private copy.
    let temp_shown = temp_root.join(format!("flux-dry-{id}"));
    let temp_db = temp_shown.join(STATE_DB);
    let temp =
        TempDir::make(fs, temp_root, id).map_err(|e| failed(RunStep::State, &temp_shown, e))?;
    let copied = match copy_store(&workspace, temp.dir()) {
        Ok(()) => true,
        // A crash between the manifest and the store's creation: nothing was claimed (the real adoption restarts it).
        Err(CopyStoreError::Read(e)) if e.source.kind() == ErrorKind::NotFound && created => false,
        Err(CopyStoreError::Read(e)) if e.source.kind() == ErrorKind::NotFound => {
            return Err(refused_state(
                LockCode::StateCorrupt,
                format!("{}: the workspace has no state.db; {BY_HAND}", db.display()),
            ));
        }
        Err(CopyStoreError::Read(e)) => return Err(failed(RunStep::State, &db, e)),
        Err(CopyStoreError::Write(e)) => return Err(failed(RunStep::State, &temp_db, e)),
    };
    // 3. Open the copy. A zero-length or undecodable store of a CREATED prior is a fresh one, as live.
    let opened = if copied {
        match temp.dir().open_claim_store(OsStr::new(STATE_DB), opts.durability) {
            Err(e) if e.code == Code::StateCorrupt && created => None,
            Err(e) if matches!(e.code, Code::StateCorrupt | Code::IncompatibleState) => {
                warnings.push(RunWarning::DryRunStoreTorn);
                return Err(state_error(&db, e));
            }
            Err(e) => return Err(failed(RunStep::State, &db, e)),
            Ok(store) => Some(store),
        }
    } else {
        None
    };
    let resume = |claims, recovered, strays| DryPrior::Resume {
        operation_id: id.to_string(),
        claims,
        recovered,
        strays,
    };
    let Some(mut store) = opened else {
        warnings.push(RunWarning::DryRunStoreFresh { operation_id: id.to_string() });
        let fresh = fresh_store(&temp, opts, &temp_db)?;
        return Ok((temp, fresh, resume(Some(0), 0, 0)));
    };
    // 4. Commit recovery on the copy; the stray temporaries stay.
    // Counted before the recovery, as the real adoption counts it (`Adopted.claims`).
    let claims = Some(store.count().map_err(|e| state_error(&db, e))?);
    let (mut recovered, mut strays) = (0, 0);
    if store.supports_prepared() {
        let manifest = place.shown(id);
        match recover_publications(
            dest,
            &place.dest_shown,
            id,
            &mut store,
            warnings,
            &manifest,
            true,
        ) {
            Ok(r) => (recovered, strays) = (r.recovered, r.strays),
            Err(RecoverError::Uncertain(list)) => {
                return Err(refused_state(LockCode::CommitStateUncertain, uncertain_detail(&list)));
            }
            Err(RecoverError::Store(e)) => return Err(state_error(&db, e)),
        }
    }
    Ok((temp, store, resume(claims, recovered, strays)))
}

/// An empty store in the private directory, replacing whatever copy was there.
fn fresh_store<D: DirHandle>(
    temp: &TempDir<D>,
    opts: &CopyOptions,
    temp_db: &Path,
) -> Result<D::Claims, RunError> {
    let name = OsStr::new(STATE_DB);
    match temp.dir().remove_file(name) {
        Ok(()) => {}
        Err(e) if e.source.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(failed(RunStep::State, temp_db, e)),
    }
    temp.dir()
        .create_claim_store(name, opts.durability)
        .map_err(|e| failed(RunStep::State, temp_db, e))
}
