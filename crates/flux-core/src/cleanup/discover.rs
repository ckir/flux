//! Discovery (cut 9b): gathers the facts about every entry of one DEST without taking the lock, and classifies each
//! with `status::classify`. Read-only: the one lock probe never writes and holds nothing afterwards.

use super::status::{
    Age, Facts, LockProbe, LockRecordFacts, ManifestProblem, Subject, Thresholds, classify,
};
use super::{CleanupConfig, Entry, EntryKind, Refused};
use crate::lock::record::{Decoded, RECORD_LEN, decode};
use crate::lock::site::Workspace;
use crate::lock::{LockError, LockSite, check_capability};
use crate::prior::check_control_plane;
use crate::run::place::{LocatedTree, locate_dest};
use crate::state::{
    CREATING_SUFFIX, FLUX_DIR, Kind, MANIFEST, OPERATIONS_DIR, OperationState, REMOVING_SUFFIX,
    Unusable, read_state,
};
use flux_fs::{Code, DestinationRoot, DirHandle, FileType, FsError, LockCapability, LockFile};
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Everything discovery found for one DEST.
pub(crate) struct Discovered<D: DirHandle> {
    pub located: LocatedTree<D>,
    pub capability: LockCapability,
    /// Operations and debris by name, then the root lock, then each `<lock>.broken.<id>` by name.
    pub entries: Vec<(Entry, Facts)>,
    /// A directory that had to be listed and could not be: exit 1 (spec "Exit codes").
    pub listing_failed: Option<(PathBuf, FsError)>,
}

const NS_PER_SEC: u64 = 1_000_000_000;

/// `Age::Future` when `then_ns > now_ns`, else whole seconds.
pub(crate) fn age_ns(now_ns: u64, then_ns: u64) -> Age {
    if then_ns > now_ns { Age::Future } else { Age::Seconds((now_ns - then_ns) / NS_PER_SEC) }
}

/// A file time against `now_ns`; `None` (or a time that is not nanoseconds since the epoch) is `Unreadable`.
pub(crate) fn age_of(now_ns: u64, modified: Option<SystemTime>) -> Age {
    let Some(then) = modified.and_then(|t| t.duration_since(UNIX_EPOCH).ok()) else {
        return Age::Unreadable;
    };
    u64::try_from(then.as_nanos()).map_or(Age::Unreadable, |ns| age_ns(now_ns, ns))
}

fn io_failure(e: LockError) -> FsError {
    match e {
        LockError::Io(e) => e,
        LockError::Refused(r) => FsError::new(Code::IoError, std::io::Error::other(r.detail)),
    }
}

/// One probe: `open_lock`, `read_all(RECORD_LEN)`, `decode`, `try_lock`, drop. Never writes.
pub(crate) fn probe_lock<D: DirHandle>(
    dir: &D,
    name: &OsStr,
    site: &LockSite<'_, D>,
    now: u64,
) -> flux_fs::Result<LockProbe> {
    probe_with_workspace(dir, name, site, now).map(|(probe, _)| probe)
}

/// `probe_lock`, and whether the workspace the record names is missing (`None` without a record): a busy lock's
/// record is not in `LockProbe`, but whether it is a row of its own depends on it.
fn probe_with_workspace<D: DirHandle>(
    dir: &D,
    name: &OsStr,
    site: &LockSite<'_, D>,
    now: u64,
) -> flux_fs::Result<(LockProbe, Option<bool>)> {
    let lock = match dir.open_lock(name) {
        Ok(l) => l,
        Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok((LockProbe::Absent, None)),
        Err(e) => return Err(e),
    };
    // Read before the try-lock (spec decision 5): the bytes are readable whoever holds the lock.
    let decoded = decode(&lock.read_all(RECORD_LEN)?);
    let free = lock.try_lock()?;
    drop(lock);
    let record = match decoded {
        Decoded::Record(r) => Some(r),
        Decoded::Uncertain(_) | Decoded::Foreign => None,
    };
    let Some(record) = record else {
        return Ok((
            if free { LockProbe::Unreadable } else { LockProbe::Busy { names: None } },
            None,
        ));
    };
    let workspace = site.workspace(&record).map_err(io_failure)?;
    // A free lock whose record names a workspace Flux does not trust is a lock a copy refuses
    // (ARTIFACT_OWNERSHIP_UNCERTAIN): it must show as a row, so it reads as unreadable (row 13).
    if free && workspace == Workspace::Untrusted {
        return Ok((LockProbe::Unreadable, None));
    }
    let missing = workspace == Workspace::Missing;
    let probe = if free {
        LockProbe::Free {
            record: Some(LockRecordFacts {
                operation_id: record.operation_id,
                workspace_missing: missing,
                lease_age: age_ns(now, record.last_heartbeat_wall_time),
            }),
        }
    } else {
        LockProbe::Busy { names: Some(record.operation_id) }
    };
    Ok((probe, Some(missing)))
}

fn refused_by(e: LockError) -> Refused {
    match e {
        LockError::Refused(r) => Refused { code: r.code.as_str(), detail: r.detail },
        LockError::Io(e) => Refused { code: e.code.as_str(), detail: e.to_string() },
    }
}

pub(crate) fn thresholds(cfg: &CleanupConfig) -> Thresholds {
    Thresholds {
        retention_secs: cfg.retention.as_secs(),
        lease_secs: cfg.lease_threshold.as_secs(),
    }
}

/// Classifies `facts` into a report row.
fn entry(kind: EntryKind, id: String, facts: Facts, t: &Thresholds) -> (Entry, Facts) {
    let verdict = classify(&facts, t);
    let state = match &facts.subject {
        Subject::Operation { manifest: Ok(s), .. } => Some(*s),
        _ => None,
    };
    let row = Entry {
        kind,
        id,
        status: verdict.status,
        eligible: verdict.eligible,
        state,
        age_seconds: verdict.age_seconds,
        note: verdict.note,
    };
    (row, facts)
}

/// What an operation's manifest says, and the manifest's age; the whole record when it is usable. The deletion pass
/// re-reads through this under the lock, so listing and deletion decide on the same reading rule.
pub(crate) fn read_manifest<D: DirHandle>(
    workspace: &D,
    id: &str,
    now: u64,
) -> (Result<OperationState, ManifestProblem>, Age) {
    let name = OsStr::new(MANIFEST);
    let age = match workspace.metadata(name) {
        Ok(m) => age_of(now, m.modified),
        Err(_) => Age::Unreadable,
    };
    let manifest = match read_state(workspace, name) {
        Err(e) if e.source.kind() == ErrorKind::NotFound => Err(ManifestProblem::Missing),
        Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
            Err(ManifestProblem::NotRegular(e.source.to_string()))
        }
        Err(e) => Err(ManifestProblem::Corrupt(e.to_string())),
        Ok(Err(Unusable::Corrupt(why))) => Err(ManifestProblem::Corrupt(why)),
        Ok(Err(Unusable::Incompatible(v))) => Err(ManifestProblem::UnknownFormat(v)),
        Ok(Ok(s)) if s.operation_id != id || s.kind != Kind::Tree => {
            Err(ManifestProblem::WrongOperation)
        }
        Ok(Ok(s)) => Ok(s),
    };
    (manifest, age)
}

/// `read_manifest`, keeping only the state.
fn manifest_of<D: DirHandle>(
    workspace: &D,
    id: &str,
    now: u64,
) -> (Result<crate::state::OpState, ManifestProblem>, Age) {
    let (manifest, age) = read_manifest(workspace, id, now);
    (manifest.map(|s| s.state), age)
}

/// The directory `name` in `parent`: `None` when it is absent (or not a directory, which `check_control_plane`
/// refuses before this is reached); any other failure is an error, never an empty listing.
pub(crate) fn child_dir<D: DirHandle>(parent: &D, name: &str) -> flux_fs::Result<Option<D>> {
    let name = OsStr::new(name);
    match parent.metadata(name) {
        Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
        Ok(m) if m.file_type == FileType::Dir => parent.open_dir(name).map(Some),
        Ok(_) => Ok(None),
    }
}

/// Gathers and classifies every entry of the DEST at `dst_root`. Lock-free: one probe of each lock file, nothing
/// written. A refusal means nothing was examined.
pub(crate) fn discover<F: DestinationRoot>(
    fs: &F,
    dst_root: &std::path::Path,
    cfg: &CleanupConfig,
) -> Result<Discovered<F::Dir>, Refused> {
    let located = locate_dest(fs, dst_root)
        .map_err(|e| Refused { code: e.cause.code.as_str(), detail: e.to_string() })?;
    let capability = check_capability(&located.holder).map_err(refused_by)?;
    // A namespace conflict refuses the whole run (exit 3); an I/O error is only a directory cleanup could not
    // list (exit 1): the operations listing is skipped and the root lock is still examined.
    let mut control_plane_failed = None;
    if let Some(dest) = &located.dest {
        match check_control_plane(dest, dst_root) {
            Ok(()) => {}
            Err(LockError::Io(e)) => control_plane_failed = Some((dst_root.join(FLUX_DIR), e)),
            Err(e) => return Err(refused_by(e)),
        }
    }
    let site = match &located.name {
        Some(name) => LockSite::directory(&located.holder, name).map_err(refused_by)?,
        None => LockSite::root(&located.holder),
    };
    let t = thresholds(cfg);
    let mut entries = Vec::new();
    let mut listing_failed = control_plane_failed;

    // The root lock first: its record feeds the operation it names. A probe that fails is not readable, and
    // nothing is eligible beside it.
    let lock_name = site.lock_name().to_os_string();
    let (probe, missing) = probe_with_workspace(&located.holder, &lock_name, &site, cfg.now)
        .unwrap_or((LockProbe::Unreadable, None));
    let lock_is_a_row = match &probe {
        LockProbe::Absent => false,
        LockProbe::Unreadable | LockProbe::Free { record: None } => true,
        LockProbe::Busy { names: None } => true,
        LockProbe::Busy { names: Some(_) } => missing == Some(true),
        LockProbe::Free { record: Some(r) } => r.workspace_missing,
    };

    // The operations listing: an absent `.flux` or `operations/` is an empty one.
    if let Some(dest) = located.dest.as_ref().filter(|_| listing_failed.is_none()) {
        let operations_shown = dst_root.join(FLUX_DIR).join(OPERATIONS_DIR);
        let operations = match child_dir(dest, FLUX_DIR) {
            Ok(Some(flux)) => match child_dir(&flux, OPERATIONS_DIR) {
                Ok(found) => found,
                Err(e) => {
                    listing_failed = Some((operations_shown.clone(), e));
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                listing_failed = Some((dst_root.join(FLUX_DIR), e));
                None
            }
        };
        if let Some(operations) = operations {
            match operations.read_dir() {
                Err(e) => listing_failed = Some((operations_shown.clone(), e)),
                Ok(mut listing) => {
                    listing.sort_by(|a, b| a.name.cmp(&b.name));
                    for item in listing.iter().filter(|i| i.file_type == FileType::Dir) {
                        let Some(name) = item.name.to_str() else { continue };
                        let debris = name
                            .strip_suffix(CREATING_SUFFIX)
                            .or_else(|| name.strip_suffix(REMOVING_SUFFIX))
                            .is_some_and(crate::ids::is_id);
                        if debris {
                            let facts = Facts {
                                subject: Subject::Debris,
                                lock: probe.clone(),
                                force: cfg.force,
                            };
                            entries.push(entry(EntryKind::Debris, name.to_string(), facts, &t));
                        } else if crate::ids::is_id(name) {
                            match operations.open_dir(&item.name) {
                                Err(e) => listing_failed = Some((operations_shown.join(name), e)),
                                Ok(workspace) => {
                                    let (manifest, manifest_age) =
                                        manifest_of(&workspace, name, cfg.now);
                                    let facts = Facts {
                                        subject: Subject::Operation {
                                            id: name.to_string(),
                                            manifest,
                                            manifest_age,
                                        },
                                        lock: probe.clone(),
                                        force: cfg.force,
                                    };
                                    entries.push(entry(
                                        EntryKind::Operation,
                                        name.to_string(),
                                        facts,
                                        &t,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if lock_is_a_row {
        let facts = Facts {
            subject: Subject::RootLock { moved_aside: false },
            lock: probe,
            force: cfg.force,
        };
        entries.push(entry(
            EntryKind::RootLock,
            lock_name.to_string_lossy().into_owned(),
            facts,
            &t,
        ));
    }

    // Every `<lock>.broken.<id>` beside the lock, by name, classified by its own record.
    let mut prefix = lock_name;
    prefix.push(".broken.");
    match located.holder.read_dir() {
        Err(e) => listing_failed = listing_failed.or(Some((located.holder_shown.clone(), e))),
        Ok(mut listing) => {
            listing.sort_by(|a, b| a.name.cmp(&b.name));
            for item in listing.iter().filter(|i| i.file_type == FileType::File) {
                let bytes = item.name.as_encoded_bytes();
                if bytes.len() <= prefix.len() || !bytes.starts_with(prefix.as_encoded_bytes()) {
                    continue;
                }
                let broken: OsString = item.name.clone();
                let probe = match probe_lock(&located.holder, &broken, &site, cfg.now) {
                    Ok(LockProbe::Absent) => continue,
                    Ok(p) => p,
                    Err(_) => LockProbe::Unreadable,
                };
                let facts = Facts {
                    subject: Subject::RootLock { moved_aside: true },
                    lock: probe,
                    force: cfg.force,
                };
                entries.push(entry(
                    EntryKind::RootLock,
                    broken.to_string_lossy().into_owned(),
                    facts,
                    &t,
                ));
            }
        }
    }
    Ok(Discovered { located, capability, entries, listing_failed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::status::{Age, LockProbe, LockRecordFacts};
    use crate::cleanup::{CleanupConfig, DEFAULT_RETENTION, EntryKind, LEASE_THRESHOLD, Status};
    use crate::fault_fs::{FakeDirHandle, FaultFs};
    use crate::lock::LockSite;
    use crate::lock::test_support::{dead_lock, fake, live_lock, record};
    use crate::state::{Kind, OpState, OperationState};
    use flux_fs::{Code, DirHandle, FileSystem, LockCapability, LockFile};
    use std::ffi::OsStr;
    use std::path::Path;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// 2033-05-18.
    const NOW: u64 = 2_000_000_000_000_000_000;
    const SEC: u64 = 1_000_000_000;

    fn cfg() -> CleanupConfig {
        CleanupConfig {
            retention: DEFAULT_RETENTION,
            lease_threshold: LEASE_THRESHOLD,
            now: NOW,
            force: false,
            dry_run: true,
            operation_id: id(200),
            owner_instance_id: id(201),
            boot_session_id: "test-boot".to_string(),
            before_mutation: None,
            heartbeat_interval: Duration::from_secs(5),
        }
    }

    fn id(n: u8) -> String {
        format!("{n:032x}")
    }

    fn at(ns: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_nanos(ns)
    }

    fn setup() -> (FaultFs, FakeDirHandle) {
        let (fs, d) = fake();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        (fs, d)
    }

    fn ops_dir(fs: &FaultFs) {
        for d in ["/p/dest/.flux", "/p/dest/.flux/operations"] {
            if !fs.exists(d) {
                fs.create_dir(Path::new(d)).unwrap();
            }
        }
    }

    /// Operation `n` in state `s`, its manifest modified at `modified` (`None`: never set).
    fn operation(fs: &FaultFs, n: u8, s: OpState, modified: Option<u64>) {
        ops_dir(fs);
        let dir = format!("/p/dest/.flux/operations/{}", id(n));
        fs.create_dir(Path::new(&dir)).unwrap();
        let state = OperationState {
            state: s,
            ..OperationState::created_v1(&id(n), Kind::Tree, Path::new("/p/dest"), 1)
        };
        fs.write_file(format!("{dir}/manifest"), &state.encode());
        if let Some(m) = modified {
            fs.set_modified(format!("{dir}/manifest"), at(m));
        }
    }

    fn dir(fs: &FaultFs, name: &str) {
        ops_dir(fs);
        fs.create_dir(Path::new(&format!("/p/dest/.flux/operations/{name}"))).unwrap();
    }

    fn put_lock(d: &FakeDirHandle, name: &str, names: u8, workspace: &str, heartbeat: u64) {
        let site = LockSite::directory(d, OsStr::new("dest")).unwrap();
        let mut r = record(&site, &id(names), workspace);
        r.last_heartbeat_wall_time = heartbeat;
        dead_lock(d, name, &r.encode());
    }

    fn run(fs: &FaultFs, cfg: &CleanupConfig) -> Result<Discovered<FakeDirHandle>, Refused> {
        discover(fs, Path::new("/p/dest"), cfg)
    }

    fn refused(r: Result<Discovered<FakeDirHandle>, Refused>) -> Refused {
        match r {
            Err(e) => e,
            Ok(_) => panic!("expected a refusal"),
        }
    }

    fn summary(found: &Discovered<FakeDirHandle>) -> Vec<(EntryKind, String, Status)> {
        found.entries.iter().map(|(e, _)| (e.kind, e.id.clone(), e.status)).collect()
    }

    /// The fake's call log with `\` folded to `/`, so assertions hold on Windows too.
    fn calls(fs: &FaultFs) -> Vec<String> {
        fs.calls().iter().map(|c| c.replace('\\', "/")).collect()
    }

    fn try_locks(fs: &FaultFs) -> usize {
        calls(fs).iter().filter(|c| c.starts_with("try_lock(")).count()
    }

    fn site(d: &FakeDirHandle) -> LockSite<'_, FakeDirHandle> {
        LockSite::directory(d, OsStr::new("dest")).unwrap()
    }

    #[test]
    fn a_lock_naming_an_existing_operation_is_not_a_row() {
        let (fs, d) = setup();
        operation(&fs, 1, OpState::Transferring, Some(NOW - 3600 * SEC));
        put_lock(&d, "dest.flux-lock", 1, &format!("operations/{}", id(1)), NOW - 10 * SEC);
        let found = run(&fs, &cfg()).unwrap();
        assert_eq!(summary(&found), vec![(EntryKind::Operation, id(1), Status::Resumable)]);
        // The lease came from the record (10 s), not the manifest (an hour): Row 7 rather than Row 9.
        assert_eq!(
            found.entries[0].1.lock,
            LockProbe::Free {
                record: Some(LockRecordFacts {
                    operation_id: id(1),
                    workspace_missing: false,
                    lease_age: Age::Seconds(10),
                })
            }
        );
    }

    #[test]
    fn a_free_lock_is_probed_once_and_released() {
        let (fs, d) = setup();
        put_lock(&d, "dest.flux-lock", 1, &format!("operations/{}", id(1)), NOW - 10 * SEC);
        let before = try_locks(&fs);
        let calls_before = fs.calls().len();
        let probe = probe_lock(&d, OsStr::new("dest.flux-lock"), &site(&d), NOW).unwrap();
        assert_eq!(
            probe,
            LockProbe::Free {
                record: Some(LockRecordFacts {
                    operation_id: id(1),
                    workspace_missing: true,
                    lease_age: Age::Seconds(10),
                })
            }
        );
        assert_eq!(try_locks(&fs) - before, 1);
        let all_calls = calls(&fs);
        let probe_calls = &all_calls[calls_before..];
        assert!(
            probe_calls.iter().all(|c| ["open_lock(", "read_all(", "try_lock(", "metadata("]
                .iter()
                .any(|p| c.starts_with(p))),
            "{probe_calls:?}"
        );
        let again = d.open_lock(OsStr::new("dest.flux-lock")).unwrap();
        assert!(again.try_lock().unwrap(), "the probe left nothing held");
    }

    #[test]
    fn a_held_lock_is_busy_and_names_its_owner() {
        let (fs, d) = setup();
        let site = site(&d);
        let rec = record(&site, &id(1), &format!("operations/{}", id(1)));
        let _held = live_lock(&d, "dest.flux-lock", &rec.encode());
        let probe = probe_lock(&d, OsStr::new("dest.flux-lock"), &site, NOW).unwrap();
        assert_eq!(probe, LockProbe::Busy { names: Some(id(1)) });
        // Distractor: bytes that are no record are busy with no owner named.
        let _other = live_lock(&d, "other.flux-lock", b"not a record at all");
        let probe = probe_lock(&d, OsStr::new("other.flux-lock"), &site, NOW).unwrap();
        assert_eq!(probe, LockProbe::Busy { names: None });
        drop(fs);
    }

    #[test]
    fn a_torn_or_empty_free_lock_is_unreadable() {
        let (_fs, d) = setup();
        dead_lock(&d, "empty.flux-lock", b"");
        dead_lock(&d, "torn.flux-lock", b"FLUXLOCK");
        for name in ["empty.flux-lock", "torn.flux-lock"] {
            let probe = probe_lock(&d, OsStr::new(name), &site(&d), NOW).unwrap();
            assert_eq!(probe, LockProbe::Unreadable, "{name}");
        }
    }

    #[test]
    fn an_absent_lock_is_absent() {
        let (_fs, d) = setup();
        let probe = probe_lock(&d, OsStr::new("dest.flux-lock"), &site(&d), NOW).unwrap();
        assert_eq!(probe, LockProbe::Absent);
    }

    #[test]
    fn the_listing_yields_operations_debris_and_the_lock_in_order() {
        let (fs, d) = setup();
        operation(&fs, 1, OpState::Completed, Some(NOW - SEC));
        dir(&fs, &format!("{}.creating", id(2)));
        dir(&fs, &format!("{}.removing", id(3)));
        fs.write_file(format!("/p/dest/.flux/operations/{}", id(4)), b"a file named by an id");
        fs.write_file("/p/dest/.flux/operations/notes.txt", b"x");
        put_lock(&d, "dest.flux-lock", 9, &format!("operations/{}", id(9)), NOW - 60 * SEC);
        let found = run(&fs, &cfg()).unwrap();
        assert_eq!(
            summary(&found),
            vec![
                (EntryKind::Operation, id(1), Status::CompletedButUnclean),
                (EntryKind::Debris, format!("{}.creating", id(2)), Status::Stale),
                (EntryKind::Debris, format!("{}.removing", id(3)), Status::Stale),
                (EntryKind::RootLock, "dest.flux-lock".to_string(), Status::Stale),
            ]
        );
        assert!(found.listing_failed.is_none());
        assert!(found.entries.iter().all(|(e, _)| e.eligible), "{:?}", found.entries);
    }

    #[test]
    fn a_manifest_problem_is_corrupt_and_an_unknown_format_is_uncertain() {
        let (fs, _d) = setup();
        dir(&fs, &id(1)); // no manifest
        operation(&fs, 2, OpState::Created, Some(NOW - SEC));
        let mut v9 = OperationState::created_v1(&id(2), Kind::Tree, Path::new("/p/dest"), 1);
        v9.format_version = 9;
        fs.write_file(format!("/p/dest/.flux/operations/{}/manifest", id(2)), &v9.encode());
        // A manifest that names another id.
        operation(&fs, 3, OpState::Created, Some(NOW - SEC));
        let other = OperationState::created_v1(&id(7), Kind::Tree, Path::new("/p/dest"), 1);
        fs.write_file(format!("/p/dest/.flux/operations/{}/manifest", id(3)), &other.encode());
        let found = run(&fs, &cfg()).unwrap();
        let by_id = |n: u8| found.entries.iter().find(|(e, _)| e.id == id(n)).unwrap().0.clone();
        assert_eq!(by_id(1).status, Status::Corrupt);
        assert_eq!(by_id(1).note, "manifest missing");
        assert_eq!((by_id(2).status, by_id(2).note.as_str()), (Status::Uncertain, "format 9"));
        assert_eq!(by_id(3).status, Status::Corrupt);
        assert_eq!(by_id(3).note, "manifest names another operation");
        assert!(found.entries.iter().all(|(e, _)| !e.eligible));
    }

    #[test]
    fn ages_come_from_the_manifest_mtime_and_the_record_heartbeat() {
        // An 8-day-old manifest and no lock: past retention, STALE.
        let (fs, _d) = setup();
        operation(&fs, 1, OpState::Transferring, Some(NOW - 8 * 86_400 * SEC));
        let found = run(&fs, &cfg()).unwrap();
        assert_eq!(summary(&found), vec![(EntryKind::Operation, id(1), Status::Stale)]);
        assert!(found.entries[0].0.eligible);
        assert_eq!(found.entries[0].0.age_seconds, Some(8 * 86_400));
        // The same, with a dead lock naming it that heartbeat 10 s ago: the lease wins.
        let (fs, d) = setup();
        operation(&fs, 1, OpState::Transferring, Some(NOW - 8 * 86_400 * SEC));
        put_lock(&d, "dest.flux-lock", 1, &format!("operations/{}", id(1)), NOW - 10 * SEC);
        let found = run(&fs, &cfg()).unwrap();
        assert_eq!(summary(&found), vec![(EntryKind::Operation, id(1), Status::Resumable)]);
        assert!(!found.entries[0].0.eligible);
        // No modified time at all: unreadable.
        let (fs, _d) = setup();
        operation(&fs, 1, OpState::Transferring, None);
        let found = run(&fs, &cfg()).unwrap();
        assert_eq!(found.entries[0].0.status, Status::Uncertain);
        assert_eq!(found.entries[0].0.note, "LEASE_AGE_UNCERTAIN");
    }

    #[test]
    fn a_dest_without_operations_or_without_dest_still_examines_the_lock() {
        let (fs, d) = fake();
        put_lock(&d, "dest.flux-lock", 1, &format!("operations/{}", id(1)), NOW - 60 * SEC);
        let found = run(&fs, &cfg()).unwrap();
        assert_eq!(
            summary(&found),
            vec![(EntryKind::RootLock, "dest.flux-lock".to_string(), Status::Stale)]
        );
        let (fs, _d) = setup();
        let found = run(&fs, &cfg()).unwrap();
        assert!(found.entries.is_empty());
        assert!(found.listing_failed.is_none());
    }

    /// A fake with a dead, decodable lock beside DEST, so a probe made before a refusal would call `try_lock`.
    fn with_dead_lock() -> FaultFs {
        let (fs, d) = fake();
        put_lock(&d, "dest.flux-lock", 1, &format!("operations/{}", id(1)), NOW - 60 * SEC);
        fs
    }

    fn probed_after(fs: &FaultFs, from: usize) -> bool {
        calls(fs)[from..].iter().any(|c| c.starts_with("try_lock("))
    }

    #[test]
    fn refusals_before_any_probe() {
        // DEST is a file.
        let fs = with_dead_lock();
        fs.write_file("/p/dest", b"x");
        let from = fs.calls().len();
        assert_eq!(refused(run(&fs, &cfg())).code, "DESTINATION_ERROR");
        assert!(!probed_after(&fs, from), "{:?}", fs.calls());
        // A symlink at DEST.
        let fs = with_dead_lock();
        fs.add_symlink("/p/dest");
        let from = fs.calls().len();
        assert_eq!(refused(run(&fs, &cfg())).code, "SAFETY_REJECTED");
        assert!(!probed_after(&fs, from), "{:?}", fs.calls());
        // DEST/.flux is a file.
        let fs = with_dead_lock();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        fs.write_file("/p/dest/.flux", b"x");
        let from = fs.calls().len();
        assert_eq!(refused(run(&fs, &cfg())).code, "CONTROL_PLANE_NAMESPACE_CONFLICT");
        assert!(!probed_after(&fs, from), "{:?}", fs.calls());
        // A lock capability that does not allow exclusivity.
        for capability in [LockCapability::RemoteUnverified, LockCapability::Unsupported] {
            let fs = with_dead_lock();
            fs.create_dir(Path::new("/p/dest")).unwrap();
            fs.set_lock_capability(capability);
            let from = fs.calls().len();
            assert_eq!(refused(run(&fs, &cfg())).code, "REMOTE_LOCK_UNSAFE", "{capability:?}");
            assert!(!probed_after(&fs, from), "{capability:?} {:?}", fs.calls());
        }
    }

    #[test]
    fn the_record_is_read_before_the_lock_is_tried() {
        let (fs, d) = setup();
        put_lock(&d, "dest.flux-lock", 1, &format!("operations/{}", id(1)), NOW - 60 * SEC);
        let from = fs.calls().len();
        probe_lock(&d, OsStr::new("dest.flux-lock"), &site(&d), NOW).unwrap();
        let calls = &calls(&fs)[from..];
        let at = |p: &str| calls.iter().position(|c| c.starts_with(p)).unwrap();
        assert!(at("read_all(") < at("try_lock("), "{calls:?}");
    }

    #[test]
    fn an_untrusted_workspace_path_is_an_uncertain_root_lock_row() {
        for force in [false, true] {
            let (fs, d) = setup();
            put_lock(&d, "dest.flux-lock", 1, "garbage", NOW - 600 * SEC);
            let found = run(&fs, &CleanupConfig { force, ..cfg() }).unwrap();
            assert_eq!(
                summary(&found),
                vec![(EntryKind::RootLock, "dest.flux-lock".to_string(), Status::Uncertain)]
            );
            assert!(!found.entries[0].0.eligible);
        }
    }

    #[test]
    fn an_unreadable_flux_dir_is_a_failed_listing_not_an_empty_one() {
        let build = || {
            let (fs, d) = setup();
            ops_dir(&fs);
            put_lock(&d, "dest.flux-lock", 9, &format!("operations/{}", id(9)), NOW - 60 * SEC);
            fs
        };
        // The first two `metadata` calls look at the destination itself (`locate_dest`); the third is the control
        // plane check's first look at `.flux`. An I/O error there is a failed listing, not a refusal.
        let fs = build();
        fs.fail_nth("metadata", 3, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        let found = run(&fs, &cfg()).unwrap();
        let (path, error) = found.listing_failed.as_ref().expect("reported, not swallowed");
        assert!(path.ends_with(".flux"), "{path:?}");
        assert_eq!(error.code, Code::PermissionDenied);
        assert_eq!(
            summary(&found),
            vec![(EntryKind::RootLock, "dest.flux-lock".to_string(), Status::Stale)]
        );
    }

    #[test]
    fn a_listing_that_fails_is_reported_not_fatal() {
        let (fs, d) = setup();
        operation(&fs, 1, OpState::Completed, Some(NOW - SEC));
        put_lock(&d, "dest.flux-lock", 9, &format!("operations/{}", id(9)), NOW - 60 * SEC);
        fs.fail("read_dir", Code::PermissionDenied);
        let found = run(&fs, &cfg()).unwrap();
        let (path, error) = found.listing_failed.as_ref().expect("the failed listing is reported");
        assert!(path.ends_with("operations"), "{path:?}");
        assert_eq!(error.code, Code::PermissionDenied);
        assert_eq!(
            summary(&found),
            vec![(EntryKind::RootLock, "dest.flux-lock".to_string(), Status::Stale)]
        );
    }

    #[test]
    fn a_broken_file_is_an_entry_by_its_own_record() {
        let (fs, d) = setup();
        let old = format!("dest.flux-lock.broken.{}", id(5));
        let young = format!("dest.flux-lock.broken.{}", id(6));
        put_lock(&d, &old, 5, &format!("operations/{}", id(5)), NOW - 60 * SEC);
        put_lock(&d, &young, 6, &format!("operations/{}", id(6)), NOW - 5 * SEC);
        let found = run(&fs, &cfg()).unwrap();
        assert_eq!(
            summary(&found),
            vec![
                (EntryKind::RootLock, old, Status::Stale),
                (EntryKind::RootLock, young, Status::Uncertain),
            ]
        );
        assert!(found.entries[0].0.eligible);
        assert!(!found.entries[1].0.eligible);
    }

    #[test]
    fn ages_are_whole_seconds_and_a_time_after_now_is_the_future() {
        assert_eq!(age_ns(NOW, NOW), Age::Seconds(0));
        assert_eq!(age_ns(NOW, NOW - 1_999_999_999), Age::Seconds(1));
        assert_eq!(age_ns(NOW, NOW + 1), Age::Future);
        assert_eq!(age_of(NOW, Some(at(NOW - 5 * SEC))), Age::Seconds(5));
        assert_eq!(age_of(NOW, Some(at(NOW + SEC))), Age::Future);
        assert_eq!(age_of(NOW, None), Age::Unreadable);
    }
}
