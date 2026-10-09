//! The cleanup deletion pass (cut 9b, spec "Deletion procedure" and "Locking"): under DEST's root lock, re-read and
//! re-classify every row the listing marked eligible, then delete what the spec authorises and nothing else. When in
//! doubt a row is kept or skipped and reported; the listing's verdict alone never deletes anything.

use super::artifacts::{remove_validated, validate};
use super::discover::{Discovered, child_dir, probe_lock, read_manifest, thresholds};
use super::status::{Facts, LockProbe, Subject, classify};
use super::{Action, CleanupConfig, CleanupReport, Entry, EntryKind};
use crate::lock::LockCode;
use crate::lock::record::LockRecord;
use crate::lock::{Held, LockError, LockSite, Released, obtain_cleanup_lock};
use crate::run::RunWarning;
use crate::run::session::{Fault, Pulse, checked_held, lock_io};
use crate::run::sweep::sweep_partials;
use crate::state::{
    CREATING_SUFFIX, FLUX_DIR, MANIFEST, OpState, OperationState, REMOVING_SUFFIX, finish_retire,
    from_native_hex, remove_empty_control_dirs, retire_workspace, write_state,
};
use flux_fs::{Code, DestinationRoot, DirHandle, FileType, FsError};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const LOST_TEXT: &str =
    "this cleanup no longer holds the destination's lock: another run took it over or removed it";

/// The lock this pass holds: its handle, the path messages name, and the heartbeat.
pub(crate) struct CleanupLock<'a, D: DirHandle> {
    pub held: Held<'a, D>,
    pub lock_shown: PathBuf,
    pub pulse: Pulse,
}

impl<D: DirHandle> CleanupLock<'_, D> {
    /// The check before every destructive step: the operator's hook (tests), the heartbeat, then ownership.
    fn check(&self, cfg: &CleanupConfig) -> Result<(), Fault> {
        if let Some(hook) = &cfg.before_mutation {
            hook();
        }
        checked_held(&self.held, &self.lock_shown, &self.pulse)
    }
}

fn skipped(entry: &Entry, reason: &str) -> Action {
    Action::Skipped { id: entry.id.clone(), reason: reason.to_string() }
}

fn io_other(message: impl Into<String>) -> std::io::Error {
    std::io::Error::other(message.into())
}

/// Where a `Fault` happened, as the report names it.
fn lost_of(fault: Fault, lock_shown: &Path) -> (PathBuf, FsError) {
    match fault {
        Fault::Lost => {
            (lock_shown.to_path_buf(), FsError::new(Code::TargetLockBusy, io_other(LOST_TEXT)))
        }
        Fault::Io(path, error) | Fault::Heartbeat(path, error) => (path, error),
    }
}

/// The unusual directory `.flux/operations` below `dest`, if it is there.
fn open_operations<D: DirHandle>(dest: &D) -> flux_fs::Result<Option<D>> {
    match child_dir(dest, FLUX_DIR)? {
        Some(flux) => child_dir(&flux, crate::state::OPERATIONS_DIR),
        None => Ok(None),
    }
}

/// The rows in the spec's order: debris and COMPLETED operations, then the other operations, then moved-aside locks.
fn rank(entry: &Entry, facts: &Facts) -> u8 {
    match (&entry.kind, &facts.subject) {
        (EntryKind::Debris, _) => 0,
        (EntryKind::Operation, Subject::Operation { manifest: Ok(OpState::Completed), .. }) => 0,
        (EntryKind::Operation, _) => 1,
        (EntryKind::RootLock, _) => 2,
    }
}

/// What a row needs, beside the lock: the handles it works through and the operator's settings.
struct Pass<'a, F: DestinationRoot> {
    fs: &'a F,
    cfg: &'a CleanupConfig,
    holder: &'a F::Dir,
    holder_shown: &'a Path,
    site: &'a LockSite<'a, F::Dir>,
    dest: Option<&'a F::Dir>,
    dest_shown: PathBuf,
    operations: Option<&'a F::Dir>,
    operations_shown: PathBuf,
}

impl<F: DestinationRoot> Pass<'_, F> {
    /// One eligible row: re-read, re-classify with the lock ours, then the row's procedure. A row that is no longer
    /// eligible is skipped, never deleted on the listing's word.
    fn row(
        &self,
        entry: &Entry,
        check: &mut dyn FnMut() -> Result<(), Fault>,
        actions: &mut Vec<Action>,
    ) -> Result<(), Fault> {
        match entry.kind {
            EntryKind::Operation => self.operation(entry, check, actions),
            EntryKind::Debris => self.debris(entry, check, actions),
            EntryKind::RootLock => self.moved_aside(entry, check, actions),
        }
    }

    fn operation(
        &self,
        entry: &Entry,
        check: &mut dyn FnMut() -> Result<(), Fault>,
        actions: &mut Vec<Action>,
    ) -> Result<(), Fault> {
        let changed = || skipped(entry, CHANGED);
        let id = entry.id.as_str();
        let (Some(dest), Some(operations)) = (self.dest, self.operations) else {
            actions.push(changed());
            return Ok(());
        };
        if !crate::ids::is_id(id) {
            actions.push(changed());
            return Ok(());
        }
        let workspace = match operations.open_dir(OsStr::new(id)) {
            Ok(w) => w,
            Err(e) if e.source.kind() == ErrorKind::NotFound => {
                actions.push(changed());
                return Ok(());
            }
            Err(error) => {
                actions.push(Action::Kept { path: self.operations_shown.join(id), error });
                return Ok(());
            }
        };
        let (manifest, manifest_age) = read_manifest(&workspace, id, self.cfg.now);
        // The workspace handle is closed before anything is retired.
        drop(workspace);
        let facts = Facts {
            subject: Subject::Operation {
                id: id.to_string(),
                manifest: manifest.as_ref().map(|s| s.state).map_err(Clone::clone),
                manifest_age,
            },
            // The lock is ours now: nobody else's record names this operation.
            lock: LockProbe::Free { record: None },
            force: self.cfg.force,
        };
        let verdict = classify(&facts, &thresholds(self.cfg));
        let Ok(state) = manifest else {
            actions.push(changed());
            return Ok(());
        };
        if !verdict.eligible {
            actions.push(changed());
            return Ok(());
        }
        if state.state == OpState::Completed {
            finish_completed(
                dest,
                &self.dest_shown,
                operations,
                &self.operations_shown,
                &state,
                check,
                actions,
            )
            .map(|_| ())
        } else {
            abandon_and_sweep(
                self.fs,
                dest,
                &self.dest_shown,
                operations,
                &self.operations_shown,
                &state,
                check,
                actions,
            )
        }
    }

    fn debris(
        &self,
        entry: &Entry,
        check: &mut dyn FnMut() -> Result<(), Fault>,
        actions: &mut Vec<Action>,
    ) -> Result<(), Fault> {
        let name = entry.id.as_str();
        let is_debris = name
            .strip_suffix(CREATING_SUFFIX)
            .or_else(|| name.strip_suffix(REMOVING_SUFFIX))
            .is_some_and(crate::ids::is_id);
        let Some(operations) = self.operations.filter(|_| is_debris) else {
            actions.push(skipped(entry, CHANGED));
            return Ok(());
        };
        // Still there, and still a directory (never a link or a file that took its name).
        let present =
            matches!(operations.metadata(OsStr::new(name)), Ok(m) if m.file_type == FileType::Dir);
        let facts = Facts {
            subject: Subject::Debris,
            lock: LockProbe::Free { record: None },
            force: self.cfg.force,
        };
        if !present || !classify(&facts, &thresholds(self.cfg)).eligible {
            actions.push(skipped(entry, CHANGED));
            return Ok(());
        }
        remove_debris(operations, &self.operations_shown, OsStr::new(name), check, actions)
    }

    /// A `<lock>.broken.<id>` file: its own record, re-read, decides.
    fn moved_aside(
        &self,
        entry: &Entry,
        check: &mut dyn FnMut() -> Result<(), Fault>,
        actions: &mut Vec<Action>,
    ) -> Result<(), Fault> {
        let name = OsStr::new(entry.id.as_str());
        let mut prefix = self.site.lock_name().to_os_string();
        prefix.push(".broken.");
        let bytes = name.as_encoded_bytes();
        let one_component =
            Path::new(name).components().count() == 1 && Path::new(name).file_name() == Some(name);
        let named_so = bytes.len() > prefix.len() && bytes.starts_with(prefix.as_encoded_bytes());
        if !(one_component && named_so) {
            actions.push(skipped(entry, CHANGED));
            return Ok(());
        }
        let probe =
            probe_lock(self.holder, name, self.site, self.cfg.now).unwrap_or(LockProbe::Unreadable);
        let facts = Facts {
            subject: Subject::RootLock { moved_aside: true },
            lock: probe,
            force: self.cfg.force,
        };
        if !classify(&facts, &thresholds(self.cfg)).eligible {
            actions.push(skipped(entry, CHANGED));
            return Ok(());
        }
        check()?;
        let shown = self.holder_shown.join(name);
        match self.holder.remove_file(name) {
            Ok(()) => actions.push(Action::Removed(shown)),
            Err(e) if e.source.kind() == ErrorKind::NotFound => {
                actions.push(skipped(entry, CHANGED))
            }
            Err(error) => actions.push(Action::Kept { path: shown, error }),
        }
        Ok(())
    }
}

const CHANGED: &str = "changed since listing";

/// The deletion pass over `discovered` (Task 3), under a lock this function acquires and releases (spec "Locking").
pub(crate) fn run_pass<F: DestinationRoot>(
    fs: &F,
    discovered: &Discovered<F::Dir>,
    cfg: &CleanupConfig,
    report: &mut CleanupReport,
) {
    let mut rows: Vec<&(Entry, Facts)> =
        discovered.entries.iter().filter(|row| row.0.eligible).collect();
    // The dry run, and a DEST with nothing to do, never touch the lock.
    if cfg.dry_run || rows.is_empty() {
        return;
    }
    let located = &discovered.located;
    let site = match &located.name {
        Some(name) => LockSite::directory(&located.holder, name),
        None => Ok(LockSite::root(&located.holder)),
    };
    let lock_shown_of = |site: &LockSite<'_, F::Dir>| located.holder_shown.join(site.lock_name());
    let site = match site {
        Ok(site) => site,
        Err(e) => {
            let error = lock_io(e);
            report.actions.push(Action::Kept { path: located.holder_shown.clone(), error });
            skip_all(&rows, "the lock could not be named", &mut report.actions);
            return;
        }
    };
    let lock_shown = lock_shown_of(&site);
    // Never `as u64`: a `Duration::MAX` threshold wraps to a small number and would reclaim a young lock.
    let lease_ns = u64::try_from(cfg.lease_threshold.as_nanos()).unwrap_or(u64::MAX);
    let obtained =
        obtain_cleanup_lock(&site, discovered.capability, &cfg.operation_id, cfg.now, lease_ns);
    let obtained = match obtained {
        Ok(o) => o,
        Err(LockError::Refused(refusal)) if refusal.code == LockCode::TargetLockBusy => {
            skip_all(&rows, "destination lock busy", &mut report.actions);
            return;
        }
        Err(refused) => {
            let (reason, error) = match refused {
                LockError::Refused(r) => (
                    r.detail.clone(),
                    FsError::new(
                        Code::IoError,
                        io_other(format!("{}: {}", r.code.as_str(), r.detail)),
                    ),
                ),
                LockError::Io(e) => (e.to_string(), e),
            };
            report.actions.push(Action::Kept { path: lock_shown, error });
            skip_all(&rows, &reason, &mut report.actions);
            return;
        }
    };
    let crate::lock::CleanupObtained { mut held, leftover_broken, reclaimed } = obtained;
    // An orphan the acquisition reclaimed is the `root-lock` row's action; it came first, since nothing else can
    // be deleted without the lock.
    if reclaimed.is_some() {
        report.actions.push(Action::Removed(lock_shown.clone()));
    }
    if let Some(name) = leftover_broken {
        let error = FsError::new(
            Code::IoError,
            io_other("the moved-aside lock could not be removed after the lock was reclaimed"),
        );
        report.actions.push(Action::Kept { path: located.holder_shown.join(name), error });
    }
    // The orphan lock's own row is done by the acquisition or not at all.
    let lock_name = site.lock_name().to_string_lossy().into_owned();
    rows.retain(|row| {
        let entry = &row.0;
        let is_lock = entry.kind == EntryKind::RootLock && entry.id == lock_name;
        if is_lock && reclaimed.is_none() {
            report.actions.push(skipped(entry, CHANGED));
        }
        !is_lock
    });
    // The record: workspace "none" (section 259.6), the pass's own ids.
    let written = site.complete_lock_key().and_then(|key| {
        held.write_record(LockRecord {
            complete_lock_key: key,
            operation_id: cfg.operation_id.clone(),
            owner_instance_id: cfg.owner_instance_id.clone(),
            boot_session_id: cfg.boot_session_id.clone(),
            target_path_key: site.target_path_key(),
            workspace_path: "none".to_string(),
            creation_wall_time: cfg.now,
            last_heartbeat_wall_time: cfg.now,
        })
    });
    if let Err(e) = written {
        report.actions.push(Action::Kept { path: lock_shown.clone(), error: lock_io(e) });
        skip_all(&rows, "the lock record could not be written", &mut report.actions);
        if let Err(e) = held.discard() {
            report.actions.push(Action::Kept { path: lock_shown, error: lock_io(e) });
        }
        return;
    }
    let lock = CleanupLock {
        held,
        lock_shown: lock_shown.clone(),
        pulse: Pulse::new(cfg.heartbeat_interval),
    };

    let dest = located.dest.as_ref();
    let dest_shown = match &located.name {
        Some(name) => located.holder_shown.join(name),
        None => located.holder_shown.clone(),
    };
    let mut operations = None;
    if let Some(dest) = dest {
        match open_operations(dest) {
            Ok(found) => operations = found,
            Err(error) => {
                let path = dest_shown.join(FLUX_DIR);
                report.actions.push(Action::Kept { path, error });
                // Every operation and debris row is dropped here, and says so.
                rows.retain(|row| {
                    let keep = row.0.kind == EntryKind::RootLock;
                    if !keep {
                        report
                            .actions
                            .push(skipped(&row.0, "cannot open the operations directory"));
                    }
                    keep
                });
            }
        }
    }
    rows.sort_by_key(|row| rank(&row.0, &row.1));

    let mut lost = None;
    {
        let mut check = || lock.check(cfg);
        let pass = Pass {
            fs,
            cfg,
            holder: &located.holder,
            holder_shown: &located.holder_shown,
            site: &site,
            dest,
            dest_shown: dest_shown.clone(),
            operations: operations.as_ref(),
            operations_shown: dest_shown.join(FLUX_DIR).join(crate::state::OPERATIONS_DIR),
        };
        for (at, row) in rows.iter().enumerate() {
            let entry = &row.0;
            let before = report.actions.len();
            match pass.row(entry, &mut check, &mut report.actions) {
                Ok(()) => {}
                Err(fault) => {
                    lost = Some(lost_of(fault, &lock_shown));
                    // The row that was cut short keeps the lines it already has; one that did nothing says why.
                    let from = if report.actions.len() == before { at } else { at + 1 };
                    skip_all(&rows[from..], "lock lost", &mut report.actions);
                    break;
                }
            }
        }
        if lost.is_none()
            && let Some(dest) = dest
        {
            // Closed before the directories it holds are removed.
            drop(operations.take());
            match check() {
                Ok(()) => {
                    if let Err(error) = remove_empty_control_dirs(dest) {
                        report
                            .actions
                            .push(Action::Kept { path: dest_shown.join(FLUX_DIR), error });
                    }
                }
                Err(fault) => lost = Some(lost_of(fault, &lock_shown)),
            }
        }
    }
    let CleanupLock { held, .. } = lock;
    // S251_1_delete: the pass's own lock goes last, while its OS-native lock is still held (`release` checks
    // ownership first and unlinks by name only while the lock is still ours).
    let released = held.release();
    // The handle closed with the release (it consumes the lock), unlinked or not.
    match released {
        Ok(Released::Unlinked) => {}
        Ok(Released::NotOwned) => {
            lost = lost.or_else(|| {
                Some((lock_shown.clone(), FsError::new(Code::TargetLockBusy, io_other(LOST_TEXT))))
            });
        }
        Err(e) => report.actions.push(Action::Kept { path: lock_shown, error: lock_io(e) }),
    }
    if lost.is_some() {
        report.lost = lost;
    }
}

fn skip_all(rows: &[&(Entry, Facts)], reason: &str, actions: &mut Vec<Action>) {
    actions.extend(rows.iter().map(|row| skipped(&row.0, reason)));
}

/// One COMPLETED operation's recorded leftovers, then its workspace (spec "Deletion procedure"). `check` runs before every removal.
/// Returns the number of leftovers removed when the workspace was retired, `None` when something was kept.
pub(crate) fn finish_completed<D: DirHandle>(
    dest: &D,
    dest_shown: &Path,
    operations: &D,
    operations_shown: &Path,
    state: &OperationState,
    check: &mut dyn FnMut() -> Result<(), Fault>,
    actions: &mut Vec<Action>,
) -> Result<Option<u64>, Fault> {
    // A format-1 record has no `cleanup` field and so no artifacts (spec row 4).
    let hexes: &[String] = state.cleanup.as_ref().map_or(&[], |c| &c.cleanup_pending_artifacts);
    // Every artifact is validated before anything is deleted: one refused name keeps the whole operation.
    let mut leftovers = Vec::with_capacity(hexes.len());
    for hex in hexes {
        match validate(hex, &state.operation_id) {
            Ok(rel) => leftovers.push(rel),
            Err(_) => {
                let shown = from_native_hex(hex).unwrap_or_else(|| PathBuf::from(hex));
                actions.push(Action::Kept {
                    path: dest_shown.join(shown),
                    error: FsError::new(
                        Code::SafetyRejected,
                        io_other("not a recognized leftover"),
                    ),
                });
                return Ok(None);
            }
        }
    }
    let mut removed = 0;
    for rel in leftovers {
        check()?;
        match remove_validated(dest, &rel) {
            // `AlreadyGone` converges a re-run after a kill (artifact rule 4).
            Ok(_) => {
                removed += 1;
                actions.push(Action::Removed(dest_shown.join(&rel)));
            }
            Err(error) => {
                actions.push(Action::Kept { path: dest_shown.join(&rel), error });
                return Ok(None);
            }
        }
    }
    check()?;
    let id = state.operation_id.as_str();
    match retire_workspace(operations, id) {
        Ok(()) => {
            actions.push(Action::Removed(operations_shown.join(id)));
            Ok(Some(removed))
        }
        Err(error) => {
            actions.push(Action::Kept { path: operations_shown.join(id), error });
            Ok(None)
        }
    }
}

/// `<id>.creating` / `<id>.removing`: `finish_retire`, reported as one action.
pub(crate) fn remove_debris<D: DirHandle>(
    operations: &D,
    operations_shown: &Path,
    name: &OsStr,
    check: &mut dyn FnMut() -> Result<(), Fault>,
    actions: &mut Vec<Action>,
) -> Result<(), Fault> {
    check()?;
    let shown = operations_shown.join(name);
    match finish_retire(operations, name) {
        Ok(()) => actions.push(Action::Removed(shown)),
        // A stranger inside makes the directory not empty: kept, never emptied.
        Err(error) => actions.push(Action::Kept { path: shown, error }),
    }
    Ok(())
}

/// A STALE (or forced RESUMABLE) operation: ABANDONED, the J1 sweep for its id, then the workspace only when every partial is gone.
#[allow(clippy::too_many_arguments)]
pub(crate) fn abandon_and_sweep<F: DestinationRoot>(
    fs: &F,
    dest: &F::Dir,
    dest_shown: &Path,
    operations: &F::Dir,
    operations_shown: &Path,
    state: &OperationState,
    check: &mut dyn FnMut() -> Result<(), Fault>,
    actions: &mut Vec<Action>,
) -> Result<(), Fault> {
    let id = state.operation_id.as_str();
    let manifest_shown = operations_shown.join(id).join(MANIFEST);
    if state.state != OpState::Abandoned {
        check()?;
        let written = operations.open_dir(OsStr::new(id)).and_then(|workspace| {
            let abandoned = OperationState { state: OpState::Abandoned, ..state.clone() };
            write_state(&workspace, OsStr::new(MANIFEST), &abandoned)
        });
        if let Err(error) = written {
            actions.push(Action::Kept { path: manifest_shown, error });
            return Ok(());
        }
    }
    let ids: BTreeSet<&str> = [id].into();
    let mut warnings = Vec::new();
    let swept = sweep_partials(
        fs,
        dest,
        dest_shown,
        operations_shown,
        &ids,
        &|i| operations_shown.join(i),
        check,
        &mut |path| actions.push(Action::Removed(path)),
        &mut warnings,
    );
    for warning in warnings {
        if let RunWarning::PartialKept { path, error, .. } = warning {
            actions.push(Action::Kept { path, error });
        }
    }
    let gone = swept?;
    if gone.iter().any(|g| g == id) {
        check()?;
        match retire_workspace(operations, id) {
            Ok(()) => actions.push(Action::Removed(operations_shown.join(id))),
            Err(error) => actions.push(Action::Kept { path: operations_shown.join(id), error }),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::{DEFAULT_RETENTION, EntryKind, LEASE_THRESHOLD, Status, Summary, cleanup};
    use crate::fault_fs::{FakeDirHandle, FaultFs};
    use crate::lock::record::{Decoded, decode};
    use crate::lock::test_support::{dead_lock, fake, live_lock, record};
    use crate::state::{Cleanup, Kind, native_hex};
    use flux_fs::FileSystem;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// 2033-05-18.
    const NOW: u64 = 2_000_000_000_000_000_000;
    const SEC: u64 = 1_000_000_000;
    const DAY: u64 = 86_400 * SEC;
    const LOCK: &str = "/p/dest.flux-lock";
    const OPS: &str = "/p/dest/.flux/operations";

    fn id(n: u8) -> String {
        format!("{n:032x}")
    }

    fn at(ns: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_nanos(ns)
    }

    fn cfg() -> CleanupConfig {
        CleanupConfig {
            retention: DEFAULT_RETENTION,
            lease_threshold: LEASE_THRESHOLD,
            now: NOW,
            force: false,
            dry_run: false,
            operation_id: id(200),
            owner_instance_id: id(201),
            boot_session_id: "test-boot".to_string(),
            before_mutation: None,
            heartbeat_interval: Duration::from_secs(5),
        }
    }

    /// The fake's call log with `\` folded to `/`, so assertions hold on Windows too.
    fn calls(fs: &FaultFs) -> Vec<String> {
        fs.calls().iter().map(|c| c.replace('\\', "/")).collect()
    }

    fn setup() -> (FaultFs, FakeDirHandle) {
        let (fs, d) = fake();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        (fs, d)
    }

    fn ops_dir(fs: &FaultFs) {
        for d in ["/p/dest/.flux", OPS] {
            if !fs.exists(d) {
                fs.create_dir(Path::new(d)).unwrap();
            }
        }
    }

    /// The workspace for `state`, its manifest modified at `modified`.
    fn workspace(fs: &FaultFs, state: &OperationState, modified: Option<u64>) {
        ops_dir(fs);
        let dir = format!("{OPS}/{}", state.operation_id);
        fs.create_dir(Path::new(&dir)).unwrap();
        fs.write_file(format!("{dir}/manifest"), &state.encode());
        if let Some(m) = modified {
            fs.set_modified(format!("{dir}/manifest"), at(m));
        }
    }

    fn v1(n: u8, s: OpState) -> OperationState {
        OperationState {
            state: s,
            ..OperationState::created_v1(&id(n), Kind::Tree, Path::new("/p/dest"), 1)
        }
    }

    /// A COMPLETED operation naming `leftovers` (relative paths) as its pending artifacts.
    fn completed(n: u8, leftovers: &[String]) -> OperationState {
        OperationState {
            state: OpState::Completed,
            cleanup: Some(Cleanup {
                cleanup_pending: true,
                cleanup_pending_artifacts: leftovers
                    .iter()
                    .map(|l| native_hex(Path::new(l)))
                    .collect(),
            }),
            ..OperationState::created_v2(&id(n), Kind::Tree, Path::new("/p/dest"), 1, None)
        }
    }

    fn partial(name: &str, n: u8) -> String {
        format!("{name}.flux-partial.{}", id(n))
    }

    fn run(fs: &FaultFs, cfg: &CleanupConfig) -> CleanupReport {
        cleanup(fs, Path::new("/p/dest"), cfg).unwrap_or_else(|r| panic!("refused: {r:?}"))
    }

    fn norm(p: &Path) -> String {
        p.display().to_string().replace('\\', "/")
    }

    /// The actions as the report prints them.
    fn lines(r: &CleanupReport) -> Vec<String> {
        r.actions
            .iter()
            .map(|a| match a {
                Action::Removed(p) => format!("removed {}", norm(p)),
                Action::Kept { path, error } => format!("kept {}: {}", norm(path), error.source),
                Action::Skipped { id, reason } => format!("skipped {id}: {reason}"),
            })
            .collect()
    }

    fn manifest(fs: &FaultFs, n: u8) -> OperationState {
        let bytes = fs.read_file(format!("{OPS}/{}/manifest", id(n))).expect("the manifest exists");
        crate::state::decode(&bytes).expect("decodes")
    }

    fn pos(c: &[String], prefix: &str) -> usize {
        c.iter().position(|x| x.starts_with(prefix)).unwrap_or_else(|| panic!("no {prefix}: {c:?}"))
    }

    /// Two leftovers (one below `sub`) of a COMPLETED `A`.
    fn completed_with_two_leftovers() -> FaultFs {
        let (fs, _d) = setup();
        fs.create_dir(Path::new("/p/dest/sub")).unwrap();
        let (a, b) = (partial("a", 1), format!("sub/{}", partial("b", 1)));
        fs.write_file(format!("/p/dest/{a}"), b"x");
        fs.write_file(format!("/p/dest/{b}"), b"x");
        workspace(&fs, &completed(1, &[a, b]), Some(NOW));
        fs
    }

    // ---------- COMPLETED ----------

    #[test]
    fn a_completed_operation_loses_its_leftovers_then_its_workspace() {
        let fs = completed_with_two_leftovers();
        let r = run(&fs, &cfg());
        let a = format!("/p/dest/{}", partial("a", 1));
        let b = format!("/p/dest/sub/{}", partial("b", 1));
        assert_eq!(
            lines(&r),
            vec![
                format!("removed {a}"),
                format!("removed {b}"),
                format!("removed {OPS}/{}", id(1))
            ]
        );
        assert!(!fs.exists("/p/dest/.flux"), "the empty control directories went");
        assert!(!fs.exists(LOCK), "the pass's own lock was released");
        assert!(!r.failed());
        let c = calls(&fs);
        let rename = format!("rename_no_replace({OPS}/{} -> {OPS}/{}.removing)", id(1), id(1));
        let (first, second) =
            (pos(&c, &format!("remove_file({a})")), pos(&c, &format!("remove_file({b})")));
        assert!(first < second && second < pos(&c, &rename), "{c:?}");
    }

    #[test]
    fn the_summary_counts_actions_not_rows() {
        let fs = completed_with_two_leftovers();
        let r = run(&fs, &cfg());
        assert_eq!(
            r.summary(),
            Summary { entries: 1, eligible: 1, removed: 3, kept: 0, skipped: 0 }
        );
    }

    #[test]
    fn a_leftover_that_cannot_be_removed_keeps_the_workspace() {
        let fs = completed_with_two_leftovers();
        fs.fail_always("remove_file", Code::PermissionDenied);
        let r = run(&fs, &cfg());
        let l = lines(&r);
        assert!(l[0].starts_with(&format!("kept /p/dest/{}", partial("a", 1))), "{l:?}");
        assert!(l.iter().all(|x| x.starts_with("kept")), "nothing was removed: {l:?}");
        assert_eq!(l.len(), 2, "the leftover, then the lock that could not be unlinked: {l:?}");
        assert!(l[1].starts_with(&format!("kept {LOCK}")), "{l:?}");
        assert_eq!(manifest(&fs, 1).state, OpState::Completed);
        assert!(
            fs.exists(format!("/p/dest/sub/{}", partial("b", 1))),
            "the stopped operation deletes no more"
        );
        assert!(r.failed());
    }

    #[test]
    fn a_refused_artifact_keeps_everything() {
        let (fs, _d) = setup();
        let valid = partial("a", 1);
        fs.write_file(format!("/p/dest/{valid}"), b"x");
        fs.write_file("/p/dest/report.doc", b"mine");
        workspace(&fs, &completed(1, &[valid.clone(), "report.doc".to_string()]), Some(NOW));
        let r = run(&fs, &cfg());
        assert_eq!(r.actions.len(), 1, "{:?}", lines(&r));
        let Action::Kept { path, error } = &r.actions[0] else { panic!("{:?}", lines(&r)) };
        assert_eq!(norm(path), "/p/dest/report.doc");
        assert!(error.source.to_string().contains("not a recognized leftover"), "{error:?}");
        assert!(fs.exists("/p/dest/report.doc"));
        assert!(fs.exists(format!("/p/dest/{valid}")), "not even the valid artifact is deleted");
        assert_eq!(manifest(&fs, 1).state, OpState::Completed);
        assert!(r.failed());
    }

    #[test]
    fn an_already_deleted_leftover_counts_as_removed() {
        let (fs, _d) = setup();
        let gone = partial("a", 1);
        workspace(&fs, &completed(1, std::slice::from_ref(&gone)), Some(NOW));
        let r = run(&fs, &cfg());
        assert_eq!(
            lines(&r),
            vec![format!("removed /p/dest/{gone}"), format!("removed {OPS}/{}", id(1))]
        );
        assert!(!fs.exists(format!("{OPS}/{}", id(1))));
        assert!(!r.failed());
    }

    #[test]
    fn a_format_1_completed_workspace_is_retired_with_no_artifacts() {
        let (fs, _d) = setup();
        workspace(&fs, &v1(1, OpState::Completed), Some(NOW));
        let r = run(&fs, &cfg());
        assert_eq!(lines(&r), vec![format!("removed {OPS}/{}", id(1))]);
    }

    #[test]
    fn a_failure_mid_retire_leaves_removing_that_the_next_pass_finishes() {
        let (fs, _d) = setup();
        workspace(&fs, &v1(1, OpState::Completed), Some(NOW));
        fs.fail_nth("remove_dir", 1, Code::PermissionDenied, ErrorKind::PermissionDenied);
        let r = run(&fs, &cfg());
        let l = lines(&r);
        assert_eq!(l.len(), 1, "{l:?}");
        assert!(l[0].starts_with(&format!("kept {OPS}/{}", id(1))), "{l:?}");
        assert!(fs.exists(format!("{OPS}/{}.removing", id(1))));
        assert!(r.failed());
        let again = run(&fs, &cfg());
        assert_eq!(again.entries.len(), 1);
        assert_eq!(
            (again.entries[0].kind, again.entries[0].status, again.entries[0].eligible),
            (EntryKind::Debris, Status::Stale, true)
        );
        assert_eq!(lines(&again), vec![format!("removed {OPS}/{}.removing", id(1))]);
        assert!(!fs.exists("/p/dest/.flux") && !again.failed());
    }

    // ---------- debris ----------

    #[test]
    fn debris_is_finished_and_a_stranger_inside_keeps_it() {
        let (fs, _d) = setup();
        ops_dir(&fs);
        let (b, c, d) = (
            format!("{}.creating", id(2)),
            format!("{}.removing", id(3)),
            format!("{}.removing", id(4)),
        );
        for (name, inside) in [(&b, "manifest.tmp"), (&c, "state.db"), (&d, "extra")] {
            fs.create_dir(Path::new(&format!("{OPS}/{name}"))).unwrap();
            fs.write_file(format!("{OPS}/{name}/{inside}"), b"x");
        }
        let r = run(&fs, &cfg());
        let l = lines(&r);
        assert_eq!(l[0], format!("removed {OPS}/{b}"));
        assert_eq!(l[1], format!("removed {OPS}/{c}"));
        assert!(l[2].starts_with(&format!("kept {OPS}/{d}")), "{l:?}");
        let Action::Kept { error, .. } = &r.actions[2] else { unreachable!() };
        assert_eq!(error.source.kind(), ErrorKind::DirectoryNotEmpty);
        assert_eq!(l.len(), 3, "{l:?}");
        assert!(fs.exists(format!("{OPS}/{d}/extra")), "a stranger is never removed");
        assert!(r.failed());
    }

    // ---------- STALE and forced RESUMABLE ----------

    #[test]
    fn a_stale_operation_is_abandoned_swept_and_retired_last() {
        let (fs, _d) = setup();
        fs.create_dir(Path::new("/p/dest/sub")).unwrap();
        let (x, y, z) = (partial("x", 1), format!("sub/{}", partial("y", 1)), partial("z", 9));
        for f in [&x, &y, &z] {
            fs.write_file(format!("/p/dest/{f}"), b"half");
        }
        workspace(&fs, &v1(1, OpState::Failed), Some(NOW - 8 * DAY));
        // What the manifest holds when the first partial is about to go.
        let seen = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&seen);
        let path = format!("{OPS}/{}/manifest", id(1));
        // Call 1 is the manifest write's stale-temporary removal; call 2 is the first partial.
        fs.on_nth("remove_file", 2, move |fs| {
            *sink.lock().unwrap() = fs.read_file(&path);
        });
        let r = run(&fs, &cfg());
        let before_any_removal =
            crate::state::decode(seen.lock().unwrap().as_ref().expect("a removal happened"))
                .unwrap();
        assert_eq!(before_any_removal.state, OpState::Abandoned, "ABANDONED is written first");
        assert_eq!(before_any_removal.superseded_by, None);
        let l = lines(&r);
        let mut partials = vec![l[0].clone(), l[1].clone()];
        partials.sort();
        let mut want = vec![format!("removed /p/dest/{x}"), format!("removed /p/dest/{y}")];
        want.sort();
        assert_eq!(partials, want, "{l:?}");
        assert_eq!(l[2], format!("removed {OPS}/{}", id(1)), "the workspace goes last");
        assert_eq!(l.len(), 3, "{l:?}");
        assert!(fs.exists(format!("/p/dest/{z}")), "another operation's partial stays");
        assert!(!r.failed());
    }

    #[test]
    fn a_partial_that_stays_keeps_the_abandoned_workspace() {
        let (fs, _d) = setup();
        let x = partial("x", 1);
        fs.write_file(format!("/p/dest/{x}"), b"half");
        workspace(&fs, &v1(1, OpState::Failed), Some(NOW - 8 * DAY));
        // Call 1 is the manifest write's stale-temporary removal; call 2 is the partial.
        fs.fail_nth("remove_file", 2, Code::PermissionDenied, ErrorKind::PermissionDenied);
        let r = run(&fs, &cfg());
        let l = lines(&r);
        assert_eq!(l.len(), 1, "{l:?}");
        assert!(l[0].starts_with(&format!("kept /p/dest/{x}")), "{l:?}");
        let kept = manifest(&fs, 1);
        assert_eq!((kept.state, kept.superseded_by), (OpState::Abandoned, None));
        assert!(fs.exists(format!("{OPS}/{}", id(1))));
        assert!(fs.exists(format!("/p/dest/{x}")));
        assert!(r.failed());
    }

    #[test]
    fn a_resumable_operation_is_deleted_only_with_force() {
        let (fs, _d) = setup();
        workspace(&fs, &v1(1, OpState::Transferring), Some(NOW - 3600 * SEC));
        let r = run(&fs, &cfg());
        assert_eq!(r.entries[0].status, Status::Resumable);
        assert!(!r.entries[0].eligible);
        assert!(r.actions.is_empty());
        assert!(!fs.called("create_lock("), "no eligible row: no lock: {:?}", fs.calls());
        let forced = run(&fs, &CleanupConfig { force: true, ..cfg() });
        assert!(forced.entries[0].eligible);
        assert_eq!(lines(&forced), vec![format!("removed {OPS}/{}", id(1))]);
        assert!(!fs.exists(format!("{OPS}/{}", id(1))));
    }

    #[test]
    fn revalidation_skips_a_row_that_changed() {
        let (fs, _d) = setup();
        let x = partial("x", 1);
        fs.write_file(format!("/p/dest/{x}"), b"half");
        workspace(&fs, &v1(1, OpState::Failed), Some(NOW - 8 * DAY));
        let path = format!("{OPS}/{}/manifest", id(1));
        let revived = v1(1, OpState::Transferring).encode();
        let p = path.clone();
        fs.on_nth("create_lock", 1, move |fs| {
            fs.write_file(&p, &revived);
            fs.set_modified(&p, at(NOW));
        });
        let r = run(&fs, &cfg());
        assert!(r.entries[0].eligible, "the listing saw it STALE");
        assert_eq!(lines(&r), vec![format!("skipped {}: changed since listing", id(1))]);
        assert!(fs.exists(&path) && fs.exists(format!("/p/dest/{x}")), "nothing was removed");
        assert_eq!(manifest(&fs, 1).state, OpState::Transferring);
        assert!(!r.failed());
    }

    // ---------- the lock ----------

    fn other_owner_bytes(d: &FakeDirHandle) -> Vec<u8> {
        let site = LockSite::directory(d, OsStr::new("dest")).unwrap();
        record(&site, &id(77), "none").encode()
    }

    #[test]
    fn a_busy_lock_skips_every_eligible_row_with_exit_0() {
        // Busy before the listing: the row is not even eligible.
        let (fs, d) = setup();
        workspace(&fs, &completed(1, &[]), Some(NOW));
        let _live = live_lock(&d, "dest.flux-lock", &other_owner_bytes(&d));
        let r = run(&fs, &cfg());
        let row = r.entries.iter().find(|e| e.kind == EntryKind::Operation).unwrap();
        assert_eq!((row.status, row.eligible), (Status::CompletedButUnclean, false));
        assert_eq!(row.note, format!("destination lock held by {}", id(77)));
        assert!(r.actions.is_empty() && !r.failed());
        assert!(fs.exists(format!("{OPS}/{}", id(1))));

        // Busy only after the listing: the acquisition finds it taken.
        let (fs, d) = setup();
        workspace(&fs, &completed(1, &[]), Some(NOW));
        let bytes = other_owner_bytes(&d);
        let held = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&held);
        fs.on_nth("create_lock", 1, move |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            *sink.lock().unwrap() = Some(live_lock(&d, "dest.flux-lock", &bytes));
        });
        let r = run(&fs, &cfg());
        assert!(r.entries[0].eligible);
        assert_eq!(lines(&r), vec![format!("skipped {}: destination lock busy", id(1))]);
        assert!(!r.failed());
        assert!(fs.exists(format!("{OPS}/{}", id(1))), "nothing was deleted");
        drop(held);
    }

    #[test]
    fn a_lost_lock_stops_the_pass() {
        let (fs, _d) = setup();
        let (a, b) = (partial("a", 1), partial("b", 2));
        for f in [&a, &b] {
            fs.write_file(format!("/p/dest/{f}"), b"x");
        }
        workspace(&fs, &completed(1, std::slice::from_ref(&a)), Some(NOW));
        workspace(&fs, &completed(2, std::slice::from_ref(&b)), Some(NOW));
        // Just before the first leftover goes, another run takes the lock over.
        fs.on_nth("remove_file", 1, |fs| fs.write_file(LOCK, b"another run's bytes"));
        let r = run(&fs, &cfg());
        assert_eq!(
            lines(&r),
            vec![format!("removed /p/dest/{a}"), format!("skipped {}: lock lost", id(2))]
        );
        assert!(r.lost.is_some() && r.failed());
        assert!(fs.exists(format!("/p/dest/{b}")) && fs.exists(format!("{OPS}/{}", id(1))));
        assert_eq!(
            fs.read_file(LOCK).as_deref(),
            Some(&b"another run's bytes"[..]),
            "never unlinked"
        );
    }

    #[test]
    fn no_destructive_step_runs_after_the_lock_is_lost() {
        // Each row: the call just before which another run takes the lock, then what must still exist afterwards
        // (leftover a, leftover b, the workspace, `.flux`). The loss is seen by the check before the NEXT step.
        let cases: [(&str, u32, [bool; 4]); 4] = [
            ("lock_sync_all", 1, [true, true, true, true]),
            ("remove_file", 1, [false, true, true, true]),
            ("remove_file", 2, [false, false, true, true]),
            ("rename_no_replace", 1, [false, false, false, true]),
        ];
        for (call, nth, expect) in cases {
            let fs = completed_with_two_leftovers();
            fs.on_nth(call, nth, |fs| fs.write_file(LOCK, b"another run's bytes"));
            let r = run(&fs, &cfg());
            let present = [
                fs.exists(format!("/p/dest/{}", partial("a", 1))),
                fs.exists(format!("/p/dest/sub/{}", partial("b", 1))),
                fs.exists(format!("{OPS}/{}", id(1))),
                fs.exists("/p/dest/.flux"),
            ];
            assert_eq!(present, expect, "lost before {call} #{nth}: {:?}", lines(&r));
            assert!(r.lost.is_some() && r.failed(), "{call} #{nth}");
        }
    }

    #[test]
    fn no_destructive_step_of_an_abandon_runs_after_the_lock_is_lost() {
        // Each row: the call just before which another run takes the lock, then (the manifest reads ABANDONED, the
        // partial exists, the workspace exists).
        let cases: [(&str, u32, [bool; 3]); 3] = [
            ("lock_sync_all", 1, [false, true, true]),
            ("rename_replace", 1, [true, true, true]),
            // Call 1 of `remove_file` is the manifest write's stale-temporary removal; call 2 is the partial.
            ("remove_file", 2, [true, false, true]),
        ];
        for (call, nth, expect) in cases {
            let (fs, _d) = setup();
            let x = partial("x", 1);
            fs.write_file(format!("/p/dest/{x}"), b"half");
            workspace(&fs, &v1(1, OpState::Failed), Some(NOW - 8 * DAY));
            fs.on_nth(call, nth, |fs| fs.write_file(LOCK, b"another run's bytes"));
            let r = run(&fs, &cfg());
            let present = [
                manifest(&fs, 1).state == OpState::Abandoned,
                fs.exists(format!("/p/dest/{x}")),
                fs.exists(format!("{OPS}/{}", id(1))),
            ];
            assert_eq!(present, expect, "lost before {call} #{nth}: {:?}", lines(&r));
            assert!(r.lost.is_some() && r.failed(), "{call} #{nth}");
        }
    }

    #[test]
    fn the_pass_writes_its_own_record_with_no_workspace() {
        let fs = completed_with_two_leftovers();
        let seen = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&seen);
        fs.on_nth("remove_file", 1, move |fs| *sink.lock().unwrap() = fs.read_file(LOCK));
        run(&fs, &cfg());
        let bytes = seen.lock().unwrap().take().expect("the lock existed during the pass");
        let Decoded::Record(rec) = decode(&bytes) else { panic!("not a record") };
        assert_eq!(
            (
                rec.operation_id.as_str(),
                rec.owner_instance_id.as_str(),
                rec.workspace_path.as_str()
            ),
            (id(200).as_str(), id(201).as_str(), "none")
        );
        assert_eq!(
            (rec.boot_session_id.as_str(), rec.creation_wall_time, rec.last_heartbeat_wall_time),
            ("test-boot", NOW, NOW)
        );
    }

    #[test]
    fn before_mutation_runs_before_each_destructive_step() {
        let fs = completed_with_two_leftovers();
        let count = Arc::new(AtomicUsize::new(0));
        let hook = Arc::clone(&count);
        let cfg = CleanupConfig {
            before_mutation: Some(Arc::new(move || {
                hook.fetch_add(1, Ordering::SeqCst);
            })),
            ..cfg()
        };
        run(&fs, &cfg);
        // Two leftovers, the workspace, and the control directories.
        assert_eq!(count.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_young_dead_lock_naming_the_operation_is_never_abandoned() {
        let (fs, _d) = setup();
        let x = partial("x", 1);
        fs.write_file(format!("/p/dest/{x}"), b"half");
        workspace(&fs, &v1(1, OpState::Failed), Some(NOW - 8 * DAY));
        // After the listing, a resume of A takes the lock and crashes seconds ago, its workspace still there.
        fs.on_nth("create_lock", 1, |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
            let mut rec = record(&site, &id(1), &format!("operations/{}", id(1)));
            rec.last_heartbeat_wall_time = NOW - 5 * SEC;
            dead_lock(&d, "dest.flux-lock", &rec.encode());
        });
        let r = run(&fs, &cfg());
        assert!(r.entries[0].eligible, "the listing saw it STALE");
        assert_eq!(lines(&r), vec![format!("skipped {}: destination lock busy", id(1))]);
        assert!(!r.failed());
        assert_eq!(manifest(&fs, 1).state, OpState::Failed, "not ABANDONED");
        assert!(fs.exists(format!("/p/dest/{x}")) && fs.exists(format!("{OPS}/{}", id(1))));
        let c = calls(&fs);
        assert!(
            !c.iter().any(|x| x.starts_with("remove_file(") || x.starts_with("rename_no_replace(")),
            "{c:?}"
        );
    }

    /// A COMPLETED v1 operation 1 and a `.creating` debris directory 2, both eligible.
    fn two_rows() -> FaultFs {
        let (fs, _d) = setup();
        workspace(&fs, &v1(1, OpState::Completed), Some(NOW));
        fs.create_dir(Path::new(&format!("{OPS}/{}.creating", id(2)))).unwrap();
        fs
    }

    #[test]
    fn unopenable_operations_dir_skips_every_eligible_row() {
        // Discovery and `check_control_plane` run before the pass and `fail_nth` counts from the start, so the fault
        // is armed from a hook that fires when the pass writes its lock record: the very next `metadata` call is
        // the pass's own look at `.flux`.
        let fs = two_rows();
        fs.on_nth("lock_sync_all", 1, |fs| {
            let seen = fs.calls().iter().filter(|c| c.starts_with("metadata(")).count();
            let nth = u32::try_from(seen).unwrap() + 1;
            fs.fail_nth("metadata", nth, Code::PermissionDenied, ErrorKind::PermissionDenied);
        });
        let r = run(&fs, &cfg());
        let l = lines(&r);
        assert_eq!(l.len(), 3, "{l:?}");
        assert!(l[0].starts_with("kept /p/dest/.flux: "), "{l:?}");
        assert!(
            l.contains(&format!("skipped {}: cannot open the operations directory", id(1))),
            "{l:?}"
        );
        assert!(
            l.contains(&format!(
                "skipped {}.creating: cannot open the operations directory",
                id(2)
            )),
            "{l:?}"
        );
        assert!(r.failed());
        assert!(fs.exists(format!("{OPS}/{}", id(1))), "nothing was removed");
    }

    #[test]
    fn the_pass_orders_debris_and_completed_before_abandon_before_broken() {
        let (fs, d) = setup();
        // The listing order (by name) is the reverse of the pass order: the stale operation is 1, the debris 2, the
        // COMPLETED operation 3, and the moved-aside lock comes last in the listing as well.
        let (x, a) = (partial("x", 1), partial("a", 3));
        fs.write_file(format!("/p/dest/{x}"), b"half");
        fs.write_file(format!("/p/dest/{a}"), b"x");
        workspace(&fs, &v1(1, OpState::Failed), Some(NOW - 8 * DAY));
        fs.create_dir(Path::new(&format!("{OPS}/{}.removing", id(2)))).unwrap();
        workspace(&fs, &completed(3, std::slice::from_ref(&a)), Some(NOW));
        put_lock(&d, &format!("dest.flux-lock.broken.{}", id(60)), 60, NOW - 60 * SEC);
        let r = run(&fs, &cfg());
        assert!(r.entries.iter().all(|e| e.eligible), "{:?}", r.entries);
        let l = lines(&r);
        let at = |what: String| {
            l.iter()
                .position(|x| *x == format!("removed {what}"))
                .unwrap_or_else(|| panic!("{what}: {l:?}"))
        };
        let debris = at(format!("{OPS}/{}.removing", id(2)));
        let leftover = at(format!("/p/dest/{a}"));
        let partial_x = at(format!("/p/dest/{x}"));
        let broken = at(format!("{LOCK}.broken.{}", id(60)));
        assert!(debris < partial_x && leftover < partial_x, "debris and COMPLETED first: {l:?}");
        assert!(partial_x < broken, "the broken file last: {l:?}");
    }

    // ---------- the root lock ----------

    /// A dead owner `n`'s lock file `name`, its workspace missing, its last heartbeat at `heartbeat`.
    fn put_lock(d: &FakeDirHandle, name: &str, n: u8, heartbeat: u64) {
        let site = LockSite::directory(d, OsStr::new("dest")).unwrap();
        let mut r = record(&site, &id(n), &format!("operations/{}", id(n)));
        r.last_heartbeat_wall_time = heartbeat;
        dead_lock(d, name, &r.encode());
    }

    fn broken_files(d: &FakeDirHandle) -> Vec<String> {
        d.read_dir()
            .unwrap()
            .into_iter()
            .map(|i| i.name.to_string_lossy().into_owned())
            .filter(|n| n.contains(".broken."))
            .collect()
    }

    #[test]
    fn an_orphan_root_lock_is_reclaimed_by_the_acquisition() {
        let (fs, d) = setup();
        put_lock(&d, "dest.flux-lock", 50, NOW - 60 * SEC);
        let r = run(&fs, &cfg());
        assert_eq!(
            (r.entries[0].kind, r.entries[0].status, r.entries[0].eligible),
            (EntryKind::RootLock, Status::Stale, true)
        );
        assert_eq!(lines(&r), vec![format!("removed {LOCK}")]);
        assert!(broken_files(&d).is_empty(), "no .broken file remains");
        assert!(!fs.exists(LOCK), "the pass's own lock was released");
        let c = calls(&fs);
        assert!(
            c.iter().any(|x| x.starts_with(&format!("rename_no_replace({LOCK} -> {LOCK}.broken."))),
            "{c:?}"
        );
        assert!(!r.failed());
    }

    #[test]
    fn a_moved_aside_lock_that_cannot_be_removed_is_kept() {
        let (fs, d) = setup();
        put_lock(&d, "dest.flux-lock", 50, NOW - 60 * SEC);
        // The first `remove_file` is the acquisition's removal of the moved-aside file.
        fs.fail_nth("remove_file", 1, Code::PermissionDenied, ErrorKind::PermissionDenied);
        let r = run(&fs, &cfg());
        let l = lines(&r);
        assert_eq!(l[0], format!("removed {LOCK}"));
        assert!(l[1].starts_with(&format!("kept {LOCK}.broken.{}", id(200))), "{l:?}");
        assert_eq!(l.len(), 2, "{l:?}");
        assert!(r.failed());
    }

    #[test]
    fn a_broken_file_is_removed_under_the_lock() {
        let (fs, d) = setup();
        put_lock(&d, &format!("dest.flux-lock.broken.{}", id(60)), 60, NOW - 60 * SEC);
        let r = run(&fs, &cfg());
        assert_eq!(lines(&r), vec![format!("removed {LOCK}.broken.{}", id(60))]);
        assert!(broken_files(&d).is_empty() && !fs.exists(LOCK));

        let (fs, d) = setup();
        put_lock(&d, &format!("dest.flux-lock.broken.{}", id(60)), 60, NOW - 5 * SEC);
        let r = run(&fs, &cfg());
        assert_eq!((r.entries[0].status, r.entries[0].eligible), (Status::Uncertain, false));
        assert!(r.actions.is_empty());
        assert_eq!(broken_files(&d).len(), 1, "a recent lease is left alone");
    }

    /// A debris row is eligible, and the dead lock beside it is an orphan of age `age_s` that the pass's
    /// acquisition may or may not reclaim.
    fn orphan_beside_debris(age_s: u64) -> (FaultFs, FakeDirHandle, Vec<u8>) {
        let (fs, d) = setup();
        ops_dir(&fs);
        fs.create_dir(Path::new(&format!("{OPS}/{}.creating", id(2)))).unwrap();
        put_lock(&d, "dest.flux-lock", 50, NOW - age_s * SEC);
        let before = fs.read_file(LOCK).unwrap();
        (fs, d, before)
    }

    #[test]
    fn an_enormous_lease_threshold_reclaims_nothing() {
        // A 60 s old orphan: the row itself is UNCERTAIN under these thresholds, but the debris beside it is
        // eligible, so the pass runs and its acquisition must refuse the orphan. `Duration::MAX` is not enough to
        // catch `as u64`: its nanoseconds are 2^64 - 1 modulo 2^64 by coincidence. `2^64 + 1 s` wraps to exactly
        // 1 s, which a truncating cast would let reclaim the orphan.
        let wraps_to_one_second = Duration::new(18_446_744_074, 709_551_616);
        assert_eq!(
            wraps_to_one_second.as_nanos() as u64,
            1_000_000_000,
            "the arithmetic of this test"
        );
        for lease_threshold in [Duration::MAX, wraps_to_one_second] {
            let (fs, _d, before) = orphan_beside_debris(60);
            let r = run(&fs, &CleanupConfig { lease_threshold, ..cfg() });
            let l = lines(&r);
            assert_eq!(l.len(), 2, "{lease_threshold:?}: {l:?}");
            assert!(l[0].starts_with(&format!("kept {LOCK}")), "{l:?}");
            assert!(l[1].starts_with(&format!("skipped {}.creating: ", id(2))), "{l:?}");
            assert!(l[1].contains("younger than the lease threshold"), "{l:?}");
            assert_eq!(fs.read_file(LOCK), Some(before), "the orphan was not touched");
            assert!(fs.exists(format!("{OPS}/{}.creating", id(2))));
            assert!(r.failed());
        }
    }

    #[test]
    fn a_young_orphan_lock_refuses_the_pass_and_keeps_everything() {
        let (fs, _d, before) = orphan_beside_debris(10);
        let r = run(&fs, &cfg());
        let l = lines(&r);
        assert_eq!(l.len(), 2, "{l:?}");
        assert!(l[0].starts_with(&format!("kept {LOCK}")), "{l:?}");
        assert!(l[1].starts_with(&format!("skipped {}.creating: ", id(2))), "{l:?}");
        assert_eq!(fs.read_file(LOCK), Some(before));
        assert!(fs.exists(format!("{OPS}/{}.creating", id(2))));
        assert!(r.failed());
    }

    // ---------- the dry run ----------

    #[test]
    fn dry_run_takes_no_lock_and_removes_nothing() {
        let fs = completed_with_two_leftovers();
        ops_dir(&fs);
        fs.create_dir(Path::new(&format!("{OPS}/{}.creating", id(2)))).unwrap();
        let r = run(&fs, &CleanupConfig { dry_run: true, ..cfg() });
        let c = calls(&fs);
        for forbidden in [
            "create_lock(",
            "remove_file(",
            "rename_no_replace(",
            "rename_replace(",
            "write_at_start(",
            "remove_dir(",
        ] {
            assert!(!c.iter().any(|x| x.starts_with(forbidden)), "{forbidden}: {c:?}");
        }
        assert_eq!(r.entries.len(), 2);
        assert!(r.entries.iter().all(|e| e.eligible));
        assert_eq!(r.summary().eligible, 2);
        assert!(r.actions.is_empty() && !r.failed());
    }
}
