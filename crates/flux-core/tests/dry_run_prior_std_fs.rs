// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Cut 11b: the real-filesystem paired corpus. A prior is built by a real `--durability strict` tree run that is cut
//! short by a panic in the `before_mutation` hook, the way a crash would leave it. The dry `--resume` / `--restart`
//! runs against one byte-identical copy of the destination and the live run against another; the live run is the
//! oracle (spec I10).
//!
//! The unwound run closed its claim store cleanly through `Drop`, so the fixture then makes the store UNCLEAN after
//! the fact: it reopens `state.db`, commits one claim at `Durability::Normal` (which writes no allocator state) and
//! keeps the file image as it stands while the store is open (spec section 10 says `forget` suffices; it
//! keeps redb's file lock for the process, so the image is restored after a clean drop instead). redb repairs the COPY a dry run opens, and the
//! live run repairs the real store, so the paired agreement below is the oracle.
//!
//! The resume test runs the dry run FIRST on the ORIGINAL destination (a byte copy has new object identities, so a
//! live resume on a copy is refused; ruling P9), then the live run on the same destination; copies serve `--restart`.

use flux_core::dry_run::{DryPrior, DryRun, PlannedAction, PlannedEntry, PlannedObject};
use flux_core::lock::LockCode;
use flux_core::run::{self, RunConfig, RunError};
use flux_core::state::{OpState, decode};
use flux_core::{TreeFailure, TreeFailureCause, TreeOutcome};
use flux_fs::{
    CopyOptions, Durability, ExistingPolicy, Links, OperationId, Preserve, Publish, Safety,
};
use flux_platform::{RedbClaimStore, StdFileSystem};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

/// The options of the prior AND of every later run: `--resume` refuses a changed configuration.
fn strict() -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability: Durability::Strict,
        publish: Publish::Replace,
        safety: Safety::Default,
        operation_id: OperationId::new("t"),
        existing: ExistingPolicy::Overwrite,
        links: Links::Copy,
        hardlinks: flux_fs::Hardlinks::Auto,
        cross_filesystems: false,
        verify: flux_fs::VerifyLevel::None,
        hash: flux_fs::HashAlgorithm::Blake3,
    }
}

fn config(dry_run: bool) -> RunConfig {
    RunConfig {
        restart: false,
        break_lock: false,
        operation_id: flux_core::ids::new_id(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: flux_platform::boot_session_id(),
        before_mutation: None,
        heartbeat_interval: run::HEARTBEAT_INTERVAL,
        resume: false,
        descriptor_limit: None,
        resume_verify: run::ResumeVerify::Chunks,
        dry_run,
        dry_temp_root: None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    kind: Kind,
    len: u64,
    modified: Option<SystemTime>,
    link: Option<PathBuf>,
}

/// Every entry under `root`, keyed by its `/`-joined relative path, never following a link. An absent root is empty.
fn snapshot(root: &Path) -> BTreeMap<String, Entry> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Entry>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let key = format!("{prefix}{}", e.file_name().to_string_lossy());
            let md = std::fs::symlink_metadata(e.path()).unwrap();
            let ft = md.file_type();
            let (kind, link) = if ft.is_symlink() {
                (Kind::Symlink, Some(std::fs::read_link(e.path()).unwrap()))
            } else if ft.is_dir() {
                (Kind::Dir, None)
            } else {
                (Kind::File, None)
            };
            // A directory's modification time moves with every entry written beside it; only a file's is a fact.
            let modified = if kind == Kind::File { md.modified().ok() } else { None };
            out.insert(key.clone(), Entry { kind: kind.clone(), len: md.len(), modified, link });
            if kind == Kind::Dir {
                walk(&e.path(), &format!("{key}/"), out);
            }
        }
    }
    let mut out = BTreeMap::new();
    if std::fs::symlink_metadata(root).is_ok() {
        walk(root, "", &mut out);
    }
    out
}

type Counters = (u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64);

fn counters(o: &TreeOutcome) -> Counters {
    (
        o.files_total,
        o.files_copied,
        o.bytes_copied,
        o.directories_created,
        o.files_overwritten,
        o.files_skipped,
        o.bytes_skipped,
        o.symlinks_skipped,
        o.mounts_skipped,
        o.special_files_skipped,
        o.files_degraded,
        o.failures.total(),
    )
}

fn cause_name(c: &TreeFailureCause) -> &'static str {
    match c {
        TreeFailureCause::Walk(_) => "Walk",
        TreeFailureCause::CreateDir(_) => "CreateDir",
        TreeFailureCause::Copy(_) => "Copy",
        TreeFailureCause::SpecialFileSkipped => "SpecialFileSkipped",
        TreeFailureCause::SymlinkSkipped => "SymlinkSkipped",
        TreeFailureCause::MountSkipped => "MountSkipped",
        TreeFailureCause::PublishedWithComplaints(_) => "PublishedWithComplaints",
        TreeFailureCause::ClaimNotRecorded(_) => "ClaimNotRecorded",
        TreeFailureCause::Planned(_) => "Planned",
    }
}

/// The comparable result of one tree run.
#[derive(Debug)]
struct Shape {
    counters: Counters,
    files_resumed: u64,
    bytes_resumed: u64,
    /// Failure records other than the dry run's `Planned` ones.
    records: Vec<(PathBuf, &'static str)>,
}

struct Ran {
    shape: Option<Shape>,
    planned: Vec<(PathBuf, PlannedEntry)>,
    stop: Option<RunError>,
    warnings: Vec<run::RunWarning>,
    resumed: Option<run::ResumeNote>,
    dry: Option<DryRun>,
}

fn run_tree(src: &Path, dst: &Path, cfg: &RunConfig) -> Ran {
    let mut records = Vec::new();
    let mut planned = Vec::new();
    let r =
        run::tree(&StdFileSystem, src, dst, &strict(), cfg, &mut |f: TreeFailure| match f.cause {
            TreeFailureCause::Planned(p) => planned.push((f.path, p)),
            ref c => records.push((f.path.clone(), cause_name(c))),
        });
    records.sort();
    let shape = match r.copy {
        Some(Ok(out)) => Some(Shape {
            counters: counters(&out),
            files_resumed: out.files_resumed,
            bytes_resumed: out.bytes_resumed,
            records,
        }),
        Some(Err(abort)) => panic!("the walk aborted: {:?}", abort.error),
        None => None,
    };
    Ran { shape, planned, stop: r.stop, warnings: r.warnings, resumed: r.resumed, dry: r.dry }
}

fn slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// A recursive std copy of `from` to `to`, keeping each file's modification time.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), &target).unwrap();
            let m = std::fs::metadata(e.path()).unwrap().modified().unwrap();
            std::fs::File::options().write(true).open(&target).unwrap().set_modified(m).unwrap();
        }
    }
}

struct Fixture {
    d: TempDir,
    src: PathBuf,
    dst: PathBuf,
    id: String,
}

impl Fixture {
    fn ops(&self) -> PathBuf {
        self.dst.join(".flux").join("operations")
    }
    fn state_db(&self) -> PathBuf {
        self.ops().join(&self.id).join("state.db")
    }
    /// An empty temporary root for a dry run, beside the fixture.
    fn temp_root(&self) -> PathBuf {
        let t = self.d.path().join("tmp");
        std::fs::create_dir_all(&t).unwrap();
        t
    }
    /// A byte-identical copy of the destination (the `.flux` workspace and the partials included) beside it.
    fn copy_dst(&self, name: &str) -> PathBuf {
        let to = self.d.path().join(name);
        copy_tree(&self.dst, &to);
        to
    }
}

/// The names of the files the source holds, with their bytes: a root file, then a directory of three.
const FILES: [(&str, usize); 5] = [
    ("a.txt", 40_000),
    ("b.txt", 30_000),
    ("sub/c.txt", 20_000),
    ("sub/d.txt", 10_000),
    ("sub/e.txt", 5_000),
];

/// DEST-relative keys of the files outside `.flux` that are not temporaries.
fn published(snap: &BTreeMap<String, Entry>) -> Vec<String> {
    snap.iter()
        .filter(|(k, e)| {
            e.kind == Kind::File && !k.starts_with(".flux") && !k.contains(".flux-partial.")
        })
        .map(|(k, _)| k.clone())
        .collect()
}

/// DEST-relative keys of the half-copied temporaries.
fn partials(snap: &BTreeMap<String, Entry>) -> Vec<String> {
    snap.keys().filter(|k| k.contains(".flux-partial.")).cloned().collect()
}

/// A real strict tree run, cut short by the panic hook at its `cut`th mutation. With the five files of `FILES` the 15th mutation leaves
/// `sub/c.txt` and `sub/d.txt` published and `a.txt`, `b.txt`, `sub/e.txt` half-done (measured).
fn interrupted(cut: u64) -> Fixture {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("src"), d.path().join("dst"));
    std::fs::create_dir_all(src.join("sub")).unwrap();
    for (name, len) in FILES {
        std::fs::write(src.join(name), vec![b'x'; len]).unwrap();
    }
    std::fs::create_dir(&dst).unwrap();
    let seen = AtomicU64::new(0);
    let cfg = RunConfig {
        before_mutation: Some(Arc::new(move || {
            if seen.fetch_add(1, Ordering::SeqCst) + 1 == cut {
                panic!("cut short on purpose");
            }
        })),
        ..config(false)
    };
    let cut_run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run::tree(&StdFileSystem, &src, &dst, &strict(), &cfg, &mut |_| {})
    }));
    assert!(cut_run.is_err(), "the run was cut short");
    let ids: Vec<String> = std::fs::read_dir(dst.join(".flux").join("operations"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "one kept workspace: {ids:?}");
    let id = ids[0].clone();
    // The fixture's shape: a TRANSFERRING manifest and a half-done file.
    let manifest = std::fs::read(dst.join(".flux/operations").join(&id).join("manifest")).unwrap();
    assert_eq!(decode(&manifest).unwrap().state, OpState::Transferring);
    let snap = snapshot(&dst);
    let published = published(&snap);
    let partials = partials(&snap);
    assert_eq!(published, ["sub/c.txt", "sub/d.txt"], "files the interrupted run published");
    assert_eq!(partials.len(), 3, "files it left half-done: {partials:?}");
    // Leave the store unclean: one more claim at `Normal` durability (no allocator state is written), and the
    // file's bytes as they stand while the store is still open, which is what a crash leaves. `forget` alone would
    // keep redb's file lock for the rest of the process, so the open-file image is restored after a clean drop.
    #[cfg(unix)]
    {
        use flux_fs::{ClaimKey, ClaimRecord, ClaimStatus, ClaimStore, FluxPathKey, ObjectId};
        let db = dst.join(".flux").join("operations").join(&id).join("state.db");
        let mut store = RedbClaimStore::open_file(
            std::fs::File::options().read(true).write(true).open(&db).unwrap(),
            Durability::Normal,
        )
        .expect("the fixture's store reopens");
        let key = ClaimKey::new(ObjectId { volume: 9, index: 9 }, std::ffi::OsStr::new("unclean"));
        let record =
            ClaimRecord { target: FluxPathKey(b"unclean".to_vec()), status: ClaimStatus::Created };
        store.insert_if_absent(&key, &record).expect("the extra claim commits");
        let open_image = std::fs::read(&db).unwrap();
        // redb's header flag byte (offset 9) has its RECOVERY_REQUIRED bit (2) set while a store is open.
        assert_ne!(open_image[9] & 2, 0, "the image is unclean");
        drop(store);
        std::fs::write(&db, open_image).unwrap();
    }
    Fixture { d, src, dst, id }
}

const CUT: u64 = 15;

fn resume_cfg(dry: bool) -> RunConfig {
    RunConfig { resume: true, ..config(dry) }
}

fn restart_cfg(dry: bool) -> RunConfig {
    RunConfig { restart: true, ..config(dry) }
}

/// The paths a dry run planned with `action`, DEST-relative and `/`-joined.
fn planned_with(r: &Ran, action: PlannedAction) -> BTreeSet<String> {
    r.planned.iter().filter(|(_, p)| p.action == action).map(|(p, _)| slash(p)).collect()
}

/// The refusal's code, or `None` when the run did not stop.
fn stop_code(r: &Ran) -> Option<LockCode> {
    match &r.stop {
        None => None,
        Some(RunError::Refused { refusal, .. }) => Some(refusal.code),
        other => panic!("a refusal or nothing: {other:?}"),
    }
}

#[test]
fn a_dry_resume_on_a_crashed_prior_matches_the_live_walk() {
    let f = interrupted(CUT);
    let tmp = f.temp_root();

    // A byte copy of the destination has NEW object identities (inodes), and a resume decides "was this file
    // published" by identity, so on a copy the live run refuses with COMMIT_STATE_UNCERTAIN. The dry run must predict
    // that refusal too; this is the paired agreement on copies.
    let dry_copy = f.copy_dst("dry_copy");
    let live_copy = f.copy_dst("live_copy");
    assert_eq!(snapshot(&dry_copy), snapshot(&live_copy), "identical copies");
    let dry_c = run_tree(
        &f.src,
        &dry_copy,
        &RunConfig { dry_temp_root: Some(tmp.clone()), ..resume_cfg(true) },
    );
    let live_c = run_tree(&f.src, &live_copy, &resume_cfg(false));
    assert_eq!(stop_code(&live_c), Some(LockCode::CommitStateUncertain), "{:?}", live_c.stop);
    assert_eq!(stop_code(&dry_c), stop_code(&live_c), "{:?} {:?}", dry_c.stop, live_c.stop);
    assert!(snapshot(&tmp).is_empty());

    // The preview that matters runs on the ORIGINAL destination, where the identities hold: a dry run changes
    // nothing (proved by the snapshots), so the live run that follows on the same destination is the same state as
    // a live run on an identical copy would have been.
    let before = snapshot(f.d.path());
    let dry = run_tree(
        &f.src,
        &f.dst,
        &RunConfig { dry_temp_root: Some(tmp.clone()), ..resume_cfg(true) },
    );
    assert!(dry.stop.is_none(), "{:?}", dry.stop);
    assert_eq!(snapshot(f.d.path()), before, "a dry run changes nothing");
    assert!(
        snapshot(&tmp).is_empty(),
        "the private temporary directory is gone: {:?}",
        snapshot(&tmp)
    );

    let live_before = snapshot(&f.dst);
    let live = run_tree(&f.src, &f.dst, &resume_cfg(false));
    assert!(live.stop.is_none(), "{:?}", live.stop);
    let live_after = snapshot(&f.dst);

    // Oracle 1: the counters of the live walk.
    let (d, l) = (dry.shape.as_ref().unwrap(), live.shape.as_ref().unwrap());
    assert_eq!(
        (d.files_resumed, d.bytes_resumed, d.counters.1),
        (l.files_resumed, l.bytes_resumed, l.counters.1),
        "dry {d:?} live {l:?}"
    );
    assert_eq!((l.files_resumed, l.bytes_resumed), (2, 30_000), "{l:?}");
    assert!(d.records.is_empty() && l.records.is_empty(), "{:?} {:?}", d.records, l.records);

    // Oracle 2: the live run streams no "resumed" record, so its resumed set is the published files whose bytes it
    // left untouched (same length and modification time before and after).
    let live_resumed: BTreeSet<String> = published(&live_before)
        .into_iter()
        .filter(|k| live_after.get(k) == live_before.get(k))
        .collect();
    assert_eq!(
        live_resumed,
        BTreeSet::from(["sub/c.txt".to_string(), "sub/d.txt".to_string()]),
        "the live run left exactly the prior's published files untouched"
    );
    assert_eq!(planned_with(&dry, PlannedAction::Resume), live_resumed);

    // The preview names the prior and the recovery count the live run reports.
    let Some(DryPrior::Resume { operation_id, recovered, .. }) =
        dry.dry.as_ref().unwrap().prior.clone()
    else {
        panic!("a Resume prior: {:?}", dry.dry);
    };
    let Some(run::ResumeNote::Adopted { operation_id: live_id, recovered: live_recovered, .. }) =
        live.resumed.clone()
    else {
        panic!("the live run adopted: {:?}", live.resumed);
    };
    assert_eq!(operation_id, f.id);
    assert_eq!((operation_id, recovered), (live_id, live_recovered), "same prior, same recovery");
    assert_eq!(published(&live_after).len(), FILES.len(), "every file ends up published");
}

#[test]
fn a_dry_restart_lists_what_the_live_restart_removes() {
    let f = interrupted(CUT);
    let dry_dst = f.copy_dst("dry_dst");
    let live_dst = f.copy_dst("live_dst");
    let tmp = f.temp_root();
    let before = snapshot(&dry_dst);

    let dry = run_tree(
        &f.src,
        &dry_dst,
        &RunConfig { dry_temp_root: Some(tmp.clone()), ..restart_cfg(true) },
    );
    assert!(dry.stop.is_none(), "{:?}", dry.stop);
    assert_eq!(snapshot(&dry_dst), before, "a dry restart changes nothing");
    assert!(snapshot(&tmp).is_empty());
    let dry_discard: BTreeSet<String> = dry
        .planned
        .iter()
        .filter(|(_, p)| {
            p.action == PlannedAction::Discard
                && matches!(p.object, PlannedObject::State | PlannedObject::Partial)
        })
        .map(|(p, _)| slash(p))
        .collect();
    assert_eq!(dry_discard.len(), 4, "three partials and the state directory: {dry_discard:?}");
    assert!(dry_discard.contains(&format!(".flux/operations/{}", f.id)), "{dry_discard:?}");

    let live_before = snapshot(&live_dst);
    let live = run_tree(&f.src, &live_dst, &restart_cfg(false));
    assert!(live.stop.is_none(), "{:?}", live.stop);
    let live_after = snapshot(&live_dst);
    // Removed by the live restart: the prior's partials and its operation directory (the keys under it collapse to
    // the directory itself).
    let removed: BTreeSet<String> = live_before
        .keys()
        .filter(|k| !live_after.contains_key(*k))
        // The workspace containers empty out with the operation directory; what is under it is part of it.
        .filter(|k| *k != ".flux" && *k != ".flux/operations")
        .filter(|k| !k.starts_with(&format!(".flux/operations/{}/", f.id)))
        .cloned()
        .collect();
    assert_eq!(dry_discard, removed);
}

#[test]
fn a_store_held_open_by_another_handle_is_copied_without_refusal() {
    let f = interrupted(CUT);
    let dry_dst = f.dst.clone();
    let tmp = f.temp_root();
    // The prior's store, held open on a separate handle for the whole run.
    let db = f.state_db();
    let held = std::fs::File::options().read(true).write(true).open(&db).unwrap();
    let _store =
        RedbClaimStore::open_file(held, Durability::Normal).expect("the second handle opens");
    let (src, cfg) =
        (f.src.clone(), RunConfig { dry_temp_root: Some(tmp.clone()), ..resume_cfg(true) });
    let (tx, rx) = std::sync::mpsc::channel();
    let dst = dry_dst.clone();
    std::thread::spawn(move || {
        let r = run_tree(&src, &dst, &cfg);
        let _ = tx.send((r.stop, r.warnings, r.dry));
    });
    let (stop, warnings, dry) =
        rx.recv_timeout(Duration::from_secs(30)).expect("the dry run returns, it does not block");
    if let Some(RunError::Refused { refusal, .. }) = &stop {
        assert_ne!(refusal.code, LockCode::TargetLockBusy, "{refusal:?}");
    }
    // Windows: redb's writer holds a mandatory `LockFileEx` over the whole file (redb 4.4.0
    // `page_manager.rs` `lock_whole_storage`), so the copy's read fails with ERROR_LOCK_VIOLATION
    // (33): spec 3.4 step 2's read error, the live `failed(RunStep::State)`, exit 1. Elsewhere the
    // lock is advisory and the copy streams.
    #[cfg(windows)]
    assert!(
        matches!(&stop, Some(RunError::Failed { step: run::RunStep::State, error, .. })
            if error.source.raw_os_error() == Some(33)),
        "the copy's read meets redb's lock: {stop:?} {warnings:?} {dry:?}"
    );
    #[cfg(not(windows))]
    {
        let previewed = stop.is_none()
            && matches!(dry.as_ref().and_then(|d| d.prior.as_ref()), Some(DryPrior::Resume { .. }));
        let torn = warnings.iter().any(|w| matches!(w, run::RunWarning::DryRunStoreTorn));
        assert!(
            previewed || torn,
            "the preview or the torn-store note: {stop:?} {warnings:?} {dry:?}"
        );
    }
    assert!(snapshot(&tmp).is_empty(), "no temporary left");
}

#[test]
fn a_truncated_store_predicts_state_corrupt_and_leaves_no_temp() {
    let f = interrupted(CUT);
    let dry_dst = f.copy_dst("dry_dst");
    let tmp = f.temp_root();
    let db = dry_dst.join(".flux").join("operations").join(&f.id).join("state.db");
    let len = std::fs::metadata(&db).unwrap().len();
    std::fs::File::options().write(true).open(&db).unwrap().set_len(len / 2).unwrap();
    let before = snapshot(&dry_dst);

    let dry = run_tree(
        &f.src,
        &dry_dst,
        &RunConfig { dry_temp_root: Some(tmp.clone()), ..resume_cfg(true) },
    );
    let Some(RunError::Refused { refusal, .. }) = &dry.stop else {
        panic!("a refusal: {:?}", dry.stop);
    };
    assert_eq!(refusal.code, LockCode::StateCorrupt, "{refusal:?}");
    let detail = refusal.detail.replace('\\', "/");
    assert!(
        detail.contains(&format!(".flux/operations/{}/state.db", f.id)),
        "the detail names the on-disk store: {detail}"
    );
    assert!(snapshot(&tmp).is_empty(), "no temporary left");
    assert_eq!(snapshot(&dry_dst), before, "nothing changed");
}
