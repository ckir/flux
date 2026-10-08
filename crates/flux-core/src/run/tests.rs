//! The run on the fake filesystem (cut 7a Part 3b-1). `/src` is the source; `/p` is DEST's parent and holds the lock.

use super::*;
use crate::copy::CopyStep;
use crate::fault_fs::FaultFs;
use crate::lock::LockCode;
use crate::lock::record::{Decoded, LockRecord, decode};
use crate::lock::test_support::{dead_lock, live_lock, record};
use crate::state::{Kind, OperationState, UNREADABLE, decode as decode_state};
use flux_fs::{
    DestinationRoot, Durability, FileIdentity, FileSystem, LockCapability, ObjectId, OperationId,
    Preserve, Publish, Safety,
};
use std::ffi::OsStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// This run's operation id.
const ID: &str = "11111111111111111111111111111111";
const LOCK: &str = "/p/dest.flux-lock";

fn id(n: u8) -> String {
    format!("{n:032x}")
}

fn cfg() -> RunConfig {
    RunConfig {
        restart: false,
        break_lock: false,
        operation_id: ID.to_string(),
        owner_instance_id: id(0xee),
        boot_session_id: "test-boot".to_string(),
        before_mutation: None,
        // Plan decision 6: the existing tests never heartbeat.
        heartbeat_interval: std::time::Duration::from_secs(3600),
        resume: false,
    }
}

fn restart() -> RunConfig {
    RunConfig { restart: true, ..cfg() }
}

fn opts() -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability: Durability::Normal,
        publish: Publish::Replace,
        safety: Safety::Default,
        operation_id: OperationId::new("replaced by the run"),
        existing: flux_fs::ExistingPolicy::Overwrite,
    }
}

/// `/src/a` (1 byte) and `/src/sub/b` (2 bytes); `/p` exists and `/p/dest` does not. Three `create_dir` calls.
fn fake() -> FaultFs {
    let fs = FaultFs::new();
    for d in ["/src", "/src/sub", "/p"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/src/a", b"A");
    fs.write_file("/src/sub/b", b"BB");
    fs
}

fn run_tree(
    fs: &FaultFs,
    c: &RunConfig,
) -> (Run<Result<TreeOutcome, TreeAbort>>, Vec<TreeFailure>) {
    let mut got = Vec::new();
    let r = tree(fs, Path::new("/src"), Path::new("/p/dest"), &opts(), c, &mut |f| got.push(f));
    (r, got)
}

/// The call log, with `/` separators on every platform.
fn calls(fs: &FaultFs) -> Vec<String> {
    fs.calls().iter().map(|c| c.replace('\\', "/")).collect()
}

/// The index of the first call starting with `prefix`.
fn at(calls: &[String], prefix: &str) -> usize {
    calls
        .iter()
        .position(|c| c.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix} in {calls:?}"))
}

fn refused(stop: &Option<RunError>) -> (LockCode, bool) {
    match stop {
        Some(RunError::Refused { refusal, changed, .. }) => (refusal.code, *changed),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn manifest(fs: &FaultFs, id: &str) -> OperationState {
    let bytes = fs
        .read_file(format!("/p/dest/.flux/operations/{id}/manifest"))
        .unwrap_or_else(|| panic!("no manifest for {id}"));
    decode_state(&bytes).unwrap()
}

/// A prior tree operation `n` in state `s`, with its workspace under `/p/dest`.
fn prior(fs: &FaultFs, n: u8, s: OpState) {
    for d in ["/p/dest", "/p/dest/.flux", "/p/dest/.flux/operations"] {
        if !fs.exists(d) {
            fs.create_dir(Path::new(d)).unwrap();
        }
    }
    fs.create_dir(Path::new(&format!("/p/dest/.flux/operations/{}", id(n)))).unwrap();
    let state = OperationState {
        state: s,
        ..OperationState::created_v1(&id(n), Kind::Tree, Path::new("/p/dest"), 1)
    };
    fs.write_file(format!("/p/dest/.flux/operations/{}/manifest", id(n)), &state.encode());
}

fn ok(r: &Run<Result<TreeOutcome, TreeAbort>>) -> &TreeOutcome {
    assert!(r.stop.is_none(), "{:?}", r.stop);
    match &r.copy {
        Some(Ok(o)) => o,
        other => panic!("expected a finished copy, got {other:?}"),
    }
}

#[test]
fn a_tree_run_copies_and_leaves_no_state_behind() {
    let fs = fake();
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert!(got.is_empty() && r.warnings.is_empty(), "{got:?} {:?}", r.warnings);
    assert_eq!((out.files_copied, out.directories_created), (2, 2), "DEST and sub");
    assert_eq!(fs.read_file("/p/dest/sub/b").as_deref(), Some(&b"BB"[..]));
    assert!(!fs.exists("/p/dest/.flux") && !fs.exists(LOCK));
}

#[test]
fn the_lock_comes_first_then_the_state_then_the_record_naming_it() {
    let fs = fake();
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    let c = calls(&fs);
    let lock = at(&c, "create_lock(/p/dest.flux-lock)");
    let state = at(&c, &format!("create_dir(/p/dest/.flux/operations/{ID}.creating)"));
    let record = at(&c, "write_at_start(");
    let copy = at(&c, &format!("create_new(/p/dest/a.flux-partial.{ID})"));
    assert!(lock < state && state < record && record < copy, "F5: {c:?}");
}

#[test]
fn the_record_stops_naming_the_state_before_the_state_is_removed() {
    let fs = fake();
    let seen: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
    let keep = Arc::clone(&seen);
    // Renames without replacing: the probe (1), the workspace into place (2), `a` and `sub/b` published (3, 4), the retire (5).
    fs.on_nth("rename_no_replace", 5, move |fs| *keep.lock().unwrap() = fs.read_file(LOCK));
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    let c = calls(&fs);
    let renames: Vec<&String> = c.iter().filter(|x| x.starts_with("rename_no_replace(")).collect();
    assert!(renames[4].contains(&format!("{ID}.removing")), "the 5th is the retire: {renames:?}");
    let bytes = seen.lock().unwrap().clone().expect("the lock exists at the retire");
    let Decoded::Record(rec) = decode(&bytes) else { panic!("a whole record") };
    assert_eq!((rec.operation_id.as_str(), rec.workspace_path.as_str()), (ID, "none"));
}

#[test]
fn an_unsupported_filesystem_is_refused_before_anything_is_created() {
    let fs = fake();
    fs.set_lock_capability(LockCapability::Unsupported);
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::RemoteLockUnsafe, false));
    assert!(r.copy.is_none());
    assert!(!fs.called("create_lock") && !fs.exists("/p/dest"));
}

#[test]
fn a_foreign_object_at_a_reserved_control_path_is_refused_before_the_lock() {
    let fs = fake();
    for d in ["/p/dest", "/p/dest/.flux"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/p/dest/.flux/atomic", b"not Flux's");
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::ControlPlaneNamespaceConflict, false));
    assert!(!fs.called("create_lock"));
}

#[test]
fn a_busy_lock_is_refused_and_left_alone() {
    let fs = fake();
    let p = fs.destination_root(Path::new("/p")).unwrap();
    let site = LockSite::directory(&p, OsStr::new("dest")).unwrap();
    let theirs = record(&site, &id(5), "none").encode();
    let _holder = live_lock(&p, "dest.flux-lock", &theirs);
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::TargetLockBusy, false));
    assert_eq!(fs.read_file(LOCK), Some(theirs));
    assert!(!fs.exists("/p/dest"));
}

#[test]
fn an_empty_lock_without_break_lock_is_uncertain_and_left_alone() {
    let fs = fake();
    let p = fs.destination_root(Path::new("/p")).unwrap();
    dead_lock(&p, "dest.flux-lock", b"");
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::TargetLockUncertain, false));
    assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b""[..]));
}

#[test]
fn a_resumable_prior_operation_is_refused_and_the_lock_removed_again() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::ResumableOperationExists, false));
    let d = detail(&r.stop);
    assert!(d.contains("created by an older version") && d.contains("--restart"), "{d}");
    assert!(!d.contains("--resume to continue"), "{d}");
    assert!(!fs.exists(LOCK), "the lock this run created is gone");
    assert_eq!(manifest(&fs, &id(5)).state, OpState::Failed, "untouched");
    assert!(!fs.exists(format!("/p/dest/.flux/operations/{ID}")));
}

#[test]
fn ownership_lost_mid_copy_stops_and_leaves_the_state_and_the_lock() {
    let fs = fake();
    // `create_new`: the probe's temporary (1), this run's CREATED (2) and TRANSFERRING (3) manifests, then `a`'s temporary (4).
    fs.on_nth("create_new", 4, |fs| fs.write_file(LOCK, b"another run's bytes"));
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "the copy's abort is the report: {:?}", r.stop);
    let Some(Err(a)) = &r.copy else { panic!("the copy aborted: {:?}", r.copy) };
    assert_eq!(a.error.code(), Code::TargetLockBusy);
    assert_eq!(manifest(&fs, ID).state, OpState::Transferring, "the state stays as it is");
    assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b"another run's bytes"[..]), "never unlinked");
    assert!(!fs.exists("/p/dest/a"));
}

#[test]
fn a_copy_refusal_that_changed_nothing_is_rolled_back_so_it_stays_exit_3() {
    let fs = FaultFs::new();
    for d in ["/src", "/src/sub", "/p", "/p/dest"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/src/sub/b", b"BB");
    // `sub` IS DEST by identity: the copy refuses it before creating anything (cut 4b).
    fs.set_identity("/src/sub", fs.metadata(Path::new("/p/dest")).unwrap().identity);
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    let Some(Err(a)) = &r.copy else { panic!("the copy refused: {:?}", r.copy) };
    assert!(a.refused_unchanged(), "{a:?}");
    assert!(!fs.exists("/p/dest/.flux") && !fs.exists(LOCK), "what the run made is gone");
    assert!(fs.exists("/p/dest"), "DEST was the operator's");
}

#[test]
fn a_rollback_removes_a_dest_the_run_made() {
    let fs = FaultFs::new();
    for d in ["/src", "/src/sub", "/p"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/src/sub/b", b"BB");
    // `create_dir`: the setup's three, then DEST (4) and `.flux` (5). Once DEST exists, `sub` becomes it by identity.
    fs.on_nth("create_dir", 5, |fs| {
        let dest = fs.metadata(Path::new("/p/dest")).unwrap().identity;
        fs.set_identity("/src/sub", dest);
    });
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    let Some(Err(a)) = &r.copy else { panic!("the copy refused: {:?}", r.copy) };
    assert!(a.refused_unchanged() && a.outcome.directories_created == 0, "{a:?}");
    assert!(!fs.exists("/p/dest") && !fs.exists(LOCK));
}

#[test]
fn a_temporary_the_copy_could_not_remove_keeps_the_completed_state() {
    let fs = fake();
    // `a`'s copy fails while streaming (its temporary is the 4th `create_new`, after the probe's), and removing that temporary fails
    // too: the 5th `remove_file` (the probe's removal, the two state writes clear their temporaries, then `a`'s step-1 sweep).
    fs.on_nth("create_new", 4, |fs| fs.fail_write(std::io::Error::other("injected write")));
    fs.fail_nth("remove_file", 5, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(out.failures.copy, 1, "{got:?}");
    assert!(r.warnings.iter().any(|w| matches!(w, RunWarning::StateKept(_))), "{:?}", r.warnings);
    assert_eq!(manifest(&fs, ID).state, OpState::Completed);
    assert!(fs.exists(format!("/p/dest/a.flux-partial.{ID}")));
    assert!(!fs.exists(LOCK), "the lock is still released");
    let c = manifest(&fs, ID).cleanup.expect("version 2");
    assert!(c.cleanup_pending);
    assert_eq!(
        c.cleanup_pending_artifacts,
        vec![crate::state::native_hex(Path::new(&format!("a.flux-partial.{ID}")))]
    );
}

#[test]
fn break_lock_takes_an_empty_lock_over_and_records_the_takeover_in_the_state() {
    let fs = fake();
    let p = fs.destination_root(Path::new("/p")).unwrap();
    dead_lock(&p, "dest.flux-lock", b"");
    let seen: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
    let keep = Arc::clone(&seen);
    // `create_new`: the probe's temporary (1), the CREATED manifest (2), the takeover (3), TRANSFERRING (4), then `a`'s temporary (5).
    let path = format!("/p/dest/.flux/operations/{ID}/manifest");
    fs.on_nth("create_new", 5, move |fs| *keep.lock().unwrap() = fs.read_file(&path));
    let (r, _) = run_tree(&fs, &RunConfig { break_lock: true, ..restart() });
    ok(&r);
    let state = decode_state(&seen.lock().unwrap().clone().expect("the manifest exists")).unwrap();
    assert_eq!(state.state, OpState::Transferring);
    let t = state.takeover.expect("the takeover is recorded");
    assert_eq!((t.operation_id.as_str(), t.owner_instance_id.as_str()), (UNREADABLE, UNREADABLE));
    assert!(!fs.exists(LOCK) && !fs.exists("/p/dest/.flux"));
}

#[test]
fn a_takeover_whose_lock_moves_starts_again_and_reuses_its_state() {
    let fs = fake();
    let p = fs.destination_root(Path::new("/p")).unwrap();
    dead_lock(&p, "dest.flux-lock", b"");
    // The takeover's overwrite is written, and before its flush the claimed file is moved away (`lock_sync_all` 1).
    fs.on_nth("lock_sync_all", 1, |fs| {
        fs.rename_replace(Path::new(LOCK), Path::new("/p/moved")).unwrap();
    });
    let (r, _) = run_tree(&fs, &RunConfig { break_lock: true, ..restart() });
    ok(&r);
    let c = calls(&fs);
    let creating = format!("create_dir(/p/dest/.flux/operations/{ID}.creating)");
    assert_eq!(
        c.iter().filter(|x| x.starts_with(&creating)).count(),
        1,
        "made once, reused: {c:?}"
    );
    assert!(fs.exists("/p/moved") && !fs.exists(LOCK));
}

#[test]
fn a_refusal_whose_lock_cannot_be_removed_names_it_and_is_exit_1() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    // The refusal's removal of the lock this run created is the run's first `remove_file`.
    fs.fail_nth("remove_file", 1, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let Some(RunError::Refused { refusal, changed, not_removed }) = &r.stop else {
        panic!("expected a refusal, got {:?}", r.stop)
    };
    assert_eq!((refusal.code, *changed), (LockCode::ResumableOperationExists, true));
    let (path, _) = not_removed.as_ref().expect("the lock it could not remove is named");
    assert_eq!(path.to_string_lossy().replace('\\', "/"), LOCK);
}

#[test]
fn ownership_lost_after_completed_is_a_warning_and_the_lock_is_left() {
    let fs = fake();
    // The retire is the 5th rename without replacing (the probe, the workspace, then the two publishes); just before it, the
    // lock is taken over. The transfer is complete and durable by then.
    fs.on_nth("rename_no_replace", 5, |fs| fs.write_file(LOCK, b"another run's bytes"));
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    assert!(
        r.warnings.iter().any(|w| matches!(w, RunWarning::OwnershipLostAfterCompletion(_))),
        "{:?}",
        r.warnings
    );
    assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b"another run's bytes"[..]), "never unlinked");
}

#[test]
fn a_dead_owners_lock_left_beside_the_new_one_is_a_warning() {
    let fs = fake();
    prior(&fs, 5, OpState::Completed);
    let p = fs.destination_root(Path::new("/p")).unwrap();
    let site = LockSite::directory(&p, OsStr::new("dest")).unwrap();
    let dead = record(&site, &id(5), &format!("operations/{}", id(5))).encode();
    dead_lock(&p, "dest.flux-lock", &dead);
    // §240.3 step 5 deletes the moved-aside lock: the run's first `remove_file`. It fails.
    fs.fail_nth("remove_file", 1, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    let broken = r.warnings.iter().any(
        |w| matches!(w, RunWarning::BrokenLeftover(p) if p.to_string_lossy().contains(".broken.")),
    );
    assert!(broken, "{:?}", r.warnings);
}

#[test]
fn restart_supersedes_a_prior_deleting_its_partials_then_its_workspace() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    let other = format!("/p/dest/other.flux-partial.{}", id(6));
    fs.write_file(&other, b"not a superseded operation's");
    let (r, _) = run_tree(&fs, &restart());
    ok(&r);
    assert!(!fs.exists(&partial), "the superseded operation's partial is deleted");
    assert!(fs.exists(&other), "only a superseded operation's partials");
    assert!(!fs.exists(format!("/p/dest/.flux/operations/{}", id(5))), "and then its workspace");
    let c = calls(&fs);
    let record = at(&c, "write_at_start(");
    let abandon =
        at(&c, &format!("rename_replace(/p/dest/.flux/operations/{}/manifest.tmp", id(5)));
    let delete = at(&c, &format!("remove_file({partial})"));
    let retire = at(&c, &format!("rename_no_replace(/p/dest/.flux/operations/{} ", id(5)));
    assert!(record < abandon && abandon < delete && delete < retire, "Q-H (a): {c:?}");
}

#[test]
fn a_partial_restart_cannot_delete_keeps_its_prior_abandoned() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    // `remove_file`: the probe's removal of `noreplace-probe` (1); this run's CREATED and the prior's ABANDONED state
    // writes clear their temporaries (2, 3); then the partial (4).
    fs.fail_nth("remove_file", 4, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &restart());
    ok(&r);
    let c = calls(&fs);
    let removals: Vec<&String> = c.iter().filter(|x| x.starts_with("remove_file(")).collect();
    assert!(
        removals[3].contains("old.flux-partial"),
        "the 4th removal is the partial: {removals:?}"
    );
    assert!(fs.exists(&partial));
    let kept = manifest(&fs, &id(5));
    assert_eq!((kept.state, kept.superseded_by.as_deref()), (OpState::Abandoned, Some(ID)));
    assert!(
        r.warnings.iter().any(|w| matches!(w, RunWarning::PartialKept { .. })),
        "{:?}",
        r.warnings
    );
    let bytes = fs.read_file(format!("/p/dest/.flux/operations/{}/manifest", id(5))).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["format_version"], 1, "a version-1 prior is rewritten as version 1");
    assert_eq!(v.as_object().unwrap().len(), 8, "with only its own eight keys");
}

#[test]
fn a_version_2_prior_superseded_by_restart_stays_version_2() {
    let fs = fake();
    for d in ["/p/dest", "/p/dest/.flux", "/p/dest/.flux/operations"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.create_dir(Path::new(&format!("/p/dest/.flux/operations/{}", id(5)))).unwrap();
    let prior = OperationState {
        state: OpState::Failed,
        ..OperationState::created_v2(&id(5), Kind::Tree, Path::new("/p/dest"), 1, None)
    };
    fs.write_file(format!("/p/dest/.flux/operations/{}/manifest", id(5)), &prior.encode());
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    // As `a_partial_restart_cannot_delete_keeps_its_prior_abandoned`: the 3rd `remove_file` is the partial.
    fs.fail_nth("remove_file", 4, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &restart());
    ok(&r);
    let kept = manifest(&fs, &id(5));
    assert_eq!((kept.format_version, kept.state), (crate::state::V2, OpState::Abandoned));
    assert_eq!(kept.cleanup.map(|c| c.cleanup_pending), Some(false));
}

#[test]
fn restart_with_nothing_to_supersede_is_a_plain_run() {
    let fs = fake();
    let (r, _) = run_tree(&fs, &restart());
    ok(&r);
    assert!(!fs.exists("/p/dest/.flux") && !fs.exists(LOCK));
}

#[test]
fn restart_stops_when_ownership_is_lost_and_deletes_nothing_after() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    // `rename_replace`: this run's CREATED manifest (1), then the prior's ABANDONED one (2): just before it, the
    // lock is taken over.
    fs.on_nth("rename_replace", 2, |fs| fs.write_file(LOCK, b"another run's bytes"));
    let (r, _) = run_tree(&fs, &restart());
    assert_eq!(refused(&r.stop), (LockCode::TargetLockBusy, true));
    assert!(r.copy.is_none());
    assert!(fs.exists(&partial), "§99 failed: nothing more is deleted");
    assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b"another run's bytes"[..]), "never unlinked");
}

/// `/p/dest/.flux/operations/<ID>.creating/<name>`, as the call log shows it.
fn creating(name: &str) -> String {
    format!("/p/dest/.flux/operations/{ID}.creating/{name}")
}

#[test]
fn the_probe_is_the_runs_first_no_replace_rename_and_leaves_nothing_in_the_workspace() {
    let fs = fake();
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    let c = calls(&fs);
    let probe = at(
        &c,
        &format!(
            "rename_no_replace({} -> {})",
            creating("noreplace-probe.tmp"),
            creating("noreplace-probe")
        ),
    );
    let removed = at(&c, &format!("remove_file({})", creating("noreplace-probe")));
    let manifest = at(&c, &format!("create_new({})", creating("manifest.tmp")));
    let published = at(
        &c,
        &format!(
            "rename_no_replace(/p/dest/.flux/operations/{ID}.creating -> /p/dest/.flux/operations/{ID})"
        ),
    );
    assert!(probe < removed && removed < manifest && manifest < published, "{c:?}");
    assert_eq!(
        c.iter()
            .filter(|x| x.starts_with("rename_no_replace(") && x.contains("noreplace-probe"))
            .count(),
        1,
        "once per run"
    );
}

#[test]
fn a_missing_no_replace_primitive_is_refused_with_nothing_changed() {
    let fs = fake();
    fs.set_no_replace_support(false);
    let (r, got) = run_tree(&fs, &cfg());
    assert!(r.copy.is_none() && got.is_empty(), "{:?}", r.copy);
    let Some(RunError::Refused { refusal, changed, not_removed }) = &r.stop else {
        panic!("{:?}", r.stop)
    };
    assert_eq!(
        (refusal.code, *changed, not_removed.is_none()),
        (LockCode::NoReplacePublishUnavailable, false, true)
    );
    assert!(refusal.detail.contains("noreplace-probe"), "{}", refusal.detail);
    assert!(!fs.exists("/p/dest"), "DEST this run made is removed again");
    assert!(!fs.exists(LOCK));
    assert!(
        !calls(&fs).iter().any(|c| c.contains("manifest.tmp")),
        "no manifest was ever staged: {:?}",
        calls(&fs)
    );
}

#[test]
fn a_probe_file_that_cannot_be_removed_is_a_warning_and_the_run_continues() {
    let fs = fake();
    // The probe's removal of `noreplace-probe` is the run's first `remove_file` (no lock exists to clean up and the
    // probe precedes the manifest's `.tmp` sweep). Retirement then removes the file with the workspace (spec 241.5).
    fs.fail("remove_file", Code::PermissionDenied);
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!((out.files_copied, got.len()), (2, 0));
    let kept = format!("/p/dest/.flux/operations/{ID}/noreplace-probe");
    assert!(
        r.warnings.iter().any(|w| matches!(w, RunWarning::ProbeNotRemoved { path, .. } if path.to_string_lossy().replace('\\', "/") == kept)),
        "{:?}", r.warnings
    );
    assert!(
        !r.warnings.iter().any(|w| matches!(w, RunWarning::NotRemoved { .. })),
        "the retire removes the probe file with the workspace: {:?}",
        r.warnings
    );
    assert!(!fs.exists(&kept), "the probe file is gone");
    assert!(
        !fs.exists(format!("/p/dest/.flux/operations/{ID}.removing")),
        "no stranded `.removing`"
    );
    assert!(!fs.exists(LOCK), "the lock is still released");
}

#[test]
fn a_probe_error_whose_cleanup_also_fails_reports_the_leftover_as_a_warning() {
    let fs = fake();
    fs.fail_always("remove_file", Code::PermissionDenied);
    fs.fail("rename_no_replace", Code::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let (step, path) = failed_at(&r.stop);
    assert_eq!((step, path), (RunStep::Probe, creating("noreplace-probe")));
    let left = creating("").trim_end_matches('/').to_string();
    assert!(
        r.warnings.iter().any(|w| matches!(w, RunWarning::NotRemoved { path, .. }
            if path.to_string_lossy().replace('\\', "/") == left)),
        "the `.creating` leftover is named: {:?}",
        r.warnings
    );
    assert!(fs.exists(&left), "left where it is");
}

#[test]
fn a_failed_probe_temporary_create_names_the_temporary() {
    let fs = fake();
    // The probe's temporary is the run's first `create_new`.
    fs.fail("create_new", Code::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.copy.is_none());
    let (step, path) = failed_at(&r.stop);
    assert_eq!((step, path), (RunStep::Probe, creating("noreplace-probe.tmp")));
    assert!(!fs.exists("/p/dest"), "a DEST this run made does not strand on a probe failure");
}

#[test]
fn a_refused_probe_whose_temporary_cannot_be_removed_names_it_and_is_exit_1() {
    let fs = fake();
    fs.set_no_replace_support(false);
    fs.fail("remove_file", Code::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let Some(RunError::Refused { refusal, changed, not_removed }) = &r.stop else {
        panic!("{:?}", r.stop)
    };
    assert_eq!((refusal.code, *changed), (LockCode::NoReplacePublishUnavailable, true));
    let (path, _) = not_removed.as_ref().expect("the temporary is named");
    assert_eq!(path.to_string_lossy().replace('\\', "/"), creating("noreplace-probe.tmp"));
    assert!(
        fs.exists(creating("noreplace-probe.tmp")) && fs.exists("/p/dest"),
        "left where they are"
    );
    assert!(!fs.exists(LOCK), "the lock is still released");
}

#[test]
fn another_probe_publish_error_fails_the_run_at_the_probe_step() {
    let fs = fake();
    // The probe's publish is the run's first `rename_no_replace`.
    fs.fail("rename_no_replace", Code::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.copy.is_none());
    let (step, path) = failed_at(&r.stop);
    assert_eq!((step, path), (RunStep::Probe, creating("noreplace-probe")));
    assert!(!fs.exists(creating("noreplace-probe.tmp")), "the temporary is removed best-effort");
    assert!(!fs.exists("/p/dest"), "a DEST this run made does not strand on a probe failure");
    assert!(!fs.exists(LOCK));
}

#[test]
fn restart_probes_again_and_a_refusal_there_leaves_the_priors_resumable() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    fs.set_no_replace_support(false);
    let (r, _) = run_tree(&fs, &restart());
    assert_eq!(refused(&r.stop), (LockCode::NoReplacePublishUnavailable, false));
    assert_eq!(manifest(&fs, &id(5)).state, OpState::Failed, "not superseded");
    assert!(!fs.exists(format!("/p/dest/.flux/operations/{ID}.creating")) && !fs.exists(LOCK));
    assert!(fs.exists("/p/dest"), "DEST was the operator's");
}

#[test]
fn a_refusal_keeps_the_probe_leftover_when_the_lock_cannot_be_removed() {
    let fs = fake();
    fs.set_no_replace_support(false);
    fs.fail_always("remove_file", Code::PermissionDenied); // the probe temporary AND the lock both stay
    let (r, _) = run_tree(&fs, &cfg());
    let Some(RunError::Refused { refusal, changed, not_removed }) = &r.stop else {
        panic!("{:?}", r.stop)
    };
    assert_eq!((refusal.code, *changed), (LockCode::NoReplacePublishUnavailable, true));
    assert!(
        refusal.detail.contains("noreplace-probe.tmp"),
        "the displaced leftover is kept: {}",
        refusal.detail
    );
    let (path, _) = not_removed.as_ref().expect("the lock is named");
    assert_eq!(path.to_string_lossy().replace('\\', "/"), LOCK);
}

#[test]
fn a_single_file_run_never_probes() {
    let fs = fake();
    fs.set_no_replace_support(false);
    let r = run_file(&fs, &cfg());
    assert!(r.stop.is_none() && r.copy.as_ref().is_some_and(|c| c.is_ok()), "{r:?}");
    assert!(!calls(&fs).iter().any(|c| c.contains("noreplace-probe")));
}

const T_LOCK: &str = "/p/t.flux-lock";

fn record_path(id: &str) -> String {
    format!("/p/t.flux-state.{id}")
}

fn run_file(fs: &FaultFs, c: &RunConfig) -> Run<Result<flux_fs::Outcome, CopyError>> {
    file(fs, Path::new("/src/a"), Path::new("/p/t"), &opts(), c)
}

fn file_prior(fs: &FaultFs, n: u8) {
    let prior = OperationState {
        state: OpState::Failed,
        ..OperationState::created_v1(&id(n), Kind::File, Path::new("/p/t"), 1)
    };
    fs.write_file(record_path(&id(n)), &prior.encode());
}

#[test]
fn a_single_file_run_copies_and_leaves_no_record_behind() {
    let fs = fake();
    let r = run_file(&fs, &cfg());
    assert!(r.stop.is_none() && r.warnings.is_empty(), "{:?} {:?}", r.stop, r.warnings);
    assert!(matches!(r.copy, Some(Ok(_))), "{:?}", r.copy);
    assert_eq!(fs.read_file("/p/t").as_deref(), Some(&b"A"[..]));
    assert!(!fs.exists(record_path(ID)) && !fs.exists(T_LOCK));
    let c = calls(&fs);
    let lock = at(&c, "create_lock(/p/t.flux-lock)");
    let state = at(&c, &format!("create_new(/p/t.flux-state.{ID}.tmp)"));
    assert!(lock < state, "{c:?}");
}

#[test]
fn a_skipped_single_file_run_completes_and_records_no_published_identity() {
    let fs = fake();
    fs.write_file("/p/t", b"old");
    let o = CopyOptions { existing: flux_fs::ExistingPolicy::SkipExisting, ..opts() };
    let r = file(&fs, Path::new("/src/a"), Path::new("/p/t"), &o, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert!(matches!(&r.copy, Some(Ok(out)) if out.skipped), "{:?}", r.copy);
    assert!(!fs.exists(record_path(ID)) && !fs.exists(T_LOCK));
    assert_eq!(fs.read_file("/p/t").as_deref(), Some(&b"old"[..]));
}

#[test]
fn a_target_name_too_long_for_its_record_is_refused_before_anything_is_made() {
    let fs = fake();
    let long = format!("/p/{}", "n".repeat(255 - 47));
    let r = file(&fs, Path::new("/src/a"), Path::new(&long), &opts(), &cfg());
    assert_eq!(refused(&r.stop), (LockCode::PathComponentInvalid, false));
    assert!(!fs.called("create_lock") && !fs.called("create_new"));
}

#[test]
fn a_mistyped_source_creates_nothing() {
    let fs = fake();
    let r = file(&fs, Path::new("/src/missing"), Path::new("/p/t"), &opts(), &cfg());
    assert!(r.stop.is_none());
    assert!(matches!(&r.copy, Some(Err(e)) if e.step == CopyStep::Source), "{:?}", r.copy);
    assert!(!fs.called("create_lock"), "B1: {:?}", fs.calls());
}

#[test]
fn a_failed_single_file_copy_leaves_a_failed_record_and_releases_the_lock() {
    let fs = fake();
    // `create_new`: the CREATED record (1), TRANSFERRING (2), then the copy's temporary (3), whose write fails.
    fs.on_nth("create_new", 3, |fs| fs.fail_write(std::io::Error::other("injected write")));
    let r = run_file(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert!(matches!(&r.copy, Some(Err(e)) if e.step == CopyStep::Stream), "{:?}", r.copy);
    let state = decode_state(&fs.read_file(record_path(ID)).unwrap()).unwrap();
    assert_eq!(state.state, OpState::Failed);
    assert!(!fs.exists(T_LOCK));
}

#[test]
fn a_resumable_single_file_prior_is_refused_without_restart() {
    let fs = fake();
    file_prior(&fs, 5);
    let r = run_file(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::ResumableOperationExists, false));
    assert!(!fs.exists(T_LOCK) && !fs.exists("/p/t"));
}

#[test]
fn restart_supersedes_a_single_files_prior_and_its_partial() {
    let fs = fake();
    file_prior(&fs, 5);
    fs.write_file(format!("/p/t.flux-partial.{}", id(5)), b"half");
    let r = run_file(&fs, &restart());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?} {:?}", r.stop, r.copy);
    assert!(!fs.exists(record_path(&id(5))) && !fs.exists(format!("/p/t.flux-partial.{}", id(5))));
}

#[test]
fn a_single_file_refusal_under_the_lock_is_rolled_back() {
    let fs = fake();
    // B1's checks pass; then, as the lock is created, the target becomes a directory: the copy's own gate refuses it.
    fs.on_nth("create_lock", 1, |fs| fs.create_dir(Path::new("/p/t")).unwrap());
    let r = run_file(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert!(
        matches!(&r.copy, Some(Err(e)) if e.code() == Code::SafetyRejected && e.leftover.is_none()),
        "{:?}",
        r.copy
    );
    assert!(!fs.exists(record_path(ID)) && !fs.exists(T_LOCK), "what the run made is gone");
    assert!(fs.exists("/p/t"));
}

// Part 3b-1 test audit (round 1): each test below was red under the mutant named in its comment.

fn aborted(r: &Run<Result<TreeOutcome, TreeAbort>>) -> &TreeAbort {
    match &r.copy {
        Some(Err(a)) => a,
        other => panic!("expected an abort, got {other:?}"),
    }
}

fn failed_at(stop: &Option<RunError>) -> (RunStep, String) {
    match stop {
        Some(RunError::Failed { step, path, .. }) => {
            (*step, path.to_string_lossy().replace('\\', "/"))
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_symlink_at_dest_is_refused_before_anything_is_made() {
    let fs = fake();
    fs.add_symlink("/p/dest");
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert_eq!(aborted(&r).error.code(), Code::SafetyRejected);
    assert!(!fs.called("create_lock"), "{:?}", fs.calls());
}

#[test]
fn a_file_at_dest_is_a_destination_error_before_anything_is_made() {
    let fs = fake();
    fs.write_file("/p/dest", b"a file");
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert_eq!(aborted(&r).error.code(), Code::DestinationError);
    assert!(!fs.called("create_lock"), "{:?}", fs.calls());
}

/// `run_tree` with the destination chosen by the test.
fn run_tree_to(fs: &FaultFs, dst: &str) -> (Run<Result<TreeOutcome, TreeAbort>>, Vec<TreeFailure>) {
    let mut got = Vec::new();
    let r = tree(fs, Path::new("/src"), Path::new(dst), &opts(), &cfg(), &mut |f| got.push(f));
    (r, got)
}

#[test]
fn a_destination_whose_resolved_anchor_lies_inside_the_source_is_refused_before_the_lock() {
    // DEST is absent, so the anchor is its parent `/p`, which (through a link in a parent component) IS `/src/sub`:
    // its canonical path says so, and the object at that path has the handle's identity.
    let fs = fake();
    fs.set_canonical_path("/p", "/src/sub");
    fs.set_identity("/src/sub", fs.metadata(Path::new("/p")).unwrap().identity);
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    let a = aborted(&r);
    assert_eq!((a.error.code(), a.error.step), (Code::SafetyRejected, CopyStep::Resolve));
    assert!(a.refused_unchanged());
    assert!(a.error.to_string().contains("resolved location"), "{}", a.error);
    assert!(!fs.called("create_lock"), "{:?}", calls(&fs));
    assert!(!fs.exists("/p/dest"));
}

#[test]
fn an_existing_destination_whose_resolved_path_lies_inside_the_source_is_refused() {
    let fs = fake();
    fs.create_dir(Path::new("/p/dest")).unwrap();
    fs.set_canonical_path("/p/dest", "/src/sub");
    fs.set_identity("/src/sub", fs.metadata(Path::new("/p/dest")).unwrap().identity);
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(aborted(&r).error.code(), Code::SafetyRejected);
    assert!(!fs.called("create_lock") && !fs.exists("/p/dest/.flux"));
}

#[test]
fn a_root_destination_whose_resolved_location_lies_inside_the_source_is_refused() {
    // `locate_tree`'s filesystem-root branch (Review Focus 4).
    let fs = fake();
    fs.set_canonical_path("/", "/src/sub");
    fs.set_identity("/src/sub", fs.destination_root(Path::new("/")).unwrap().identity().unwrap());
    let (r, _) = run_tree_to(&fs, "/");
    assert_eq!(aborted(&r).error.code(), Code::SafetyRejected);
    assert!(!fs.called("create_lock"));
}

#[test]
fn a_merely_similar_resolved_name_is_not_inside_the_source() {
    let fs = fake();
    fs.create_dir(Path::new("/srcs")).unwrap();
    fs.set_canonical_path("/p", "/srcs");
    fs.set_identity("/srcs", fs.metadata(Path::new("/p")).unwrap().identity);
    let (r, _) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(out.files_copied, 2);
    assert!(out.containment_degraded.is_none());
}

#[test]
fn an_unsupported_canonical_path_query_warns_under_default_and_refuses_under_strict() {
    let lax = fake();
    lax.fail_kind("canonical_path", Code::IoError, std::io::ErrorKind::Unsupported);
    let (r, _) = run_tree(&lax, &cfg());
    let out = ok(&r);
    assert_eq!(
        out.containment_degraded.as_deref(),
        Some(Path::new("/p")),
        "the anchor's query failed"
    );

    let tight = fake();
    tight.fail_kind("canonical_path", Code::IoError, std::io::ErrorKind::Unsupported);
    let mut got = Vec::new();
    let strict = CopyOptions { safety: Safety::Strict, ..opts() };
    let r = tree(&tight, Path::new("/src"), Path::new("/p/dest"), &strict, &cfg(), &mut |f| {
        got.push(f)
    });
    assert_eq!(aborted(&r).error.code(), Code::SafetyRejected);
    assert!(!tight.called("create_lock") && !tight.exists("/p/dest"));
}

#[test]
fn another_canonical_path_error_aborts_before_the_lock() {
    let fs = fake();
    fs.fail_kind("canonical_path", Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let a = aborted(&r);
    assert_eq!((a.error.code(), a.error.step), (Code::PermissionDenied, CopyStep::Resolve));
    assert!(!fs.called("create_lock"));
}

#[test]
fn a_canonical_path_naming_another_object_aborts_before_the_lock() {
    // The handle's directory was renamed away and another directory holds the reported name (Review Focus 3).
    let fs = fake();
    fs.set_canonical_path("/p", "/src/sub"); // `/src/sub` keeps its own identity: not `/p`'s
    let (r, _) = run_tree(&fs, &cfg());
    let a = aborted(&r);
    assert_eq!((a.error.code(), a.error.step), (Code::DestinationError, CopyStep::Resolve));
    assert!(!fs.called("create_lock"));
}

#[test]
fn an_unconfirmed_path_outside_the_source_is_degraded_but_one_inside_it_is_still_refused() {
    // Weak identity at the reported path: the all-clear cannot be confirmed (warn / strict-refuse)...
    let outside = fake();
    outside.create_dir(Path::new("/srcs")).unwrap();
    outside.set_canonical_path("/p", "/srcs");
    outside.set_identity("/srcs", FileIdentity::Weak(ObjectId { volume: 1, index: 77 }));
    let (r, _) = run_tree(&outside, &cfg());
    assert_eq!(ok(&r).containment_degraded.as_deref(), Some(Path::new("/p")));

    // ...but a reported path INSIDE the source refuses in every mode, confirmed or not (agy panel r1).
    for safety in [Safety::Default, Safety::Strict] {
        let inside = fake();
        inside.set_canonical_path("/p", "/src/sub");
        inside.set_identity("/src/sub", FileIdentity::Weak(ObjectId { volume: 1, index: 77 }));
        let mut got = Vec::new();
        let o = CopyOptions { safety, ..opts() };
        let r = tree(&inside, Path::new("/src"), Path::new("/p/dest"), &o, &cfg(), &mut |f| {
            got.push(f)
        });
        assert_eq!(aborted(&r).error.code(), Code::SafetyRejected, "{safety:?}");
        assert!(!inside.called("create_lock"));
    }
}

#[test]
fn an_ownership_check_that_cannot_read_the_lock_is_a_failure_not_a_loss() {
    let fs = fake();
    // `rename_replace`: the CREATED record (1), TRANSFERRING (2), the publish (3). Just before the publish, every
    // later read of the lock fails: the finish's `still_owned` cannot tell.
    fs.on_nth("rename_replace", 3, |fs| fs.fail_always("read_all", Code::IoError));
    let r = run_file(&fs, &cfg());
    assert!(matches!(r.copy, Some(Ok(_))), "{:?}", r.copy);
    assert_eq!(failed_at(&r.stop), (RunStep::State, T_LOCK.to_string()));
    let state = decode_state(&fs.read_file(record_path(ID)).unwrap()).unwrap();
    assert_eq!(state.state, OpState::Transferring, "nothing written without proof of ownership");
    assert!(fs.exists(T_LOCK), "never unlinked");
}

#[test]
fn a_guard_that_cannot_read_the_lock_aborts_the_copy_as_target_lock_busy() {
    let fs = fake();
    // `create_new`: the CREATED record (1), TRANSFERRING (2), the copy's temporary (3). From there every read of the
    // lock fails, so the publish's guard cannot tell.
    fs.on_nth("create_new", 3, |fs| fs.fail_always("read_all", Code::IoError));
    let r = run_file(&fs, &cfg());
    assert!(matches!(&r.copy, Some(Err(e)) if e.code() == Code::TargetLockBusy), "{:?}", r.copy);
    assert!(!fs.exists("/p/t"), "never published");
}

#[test]
fn a_failed_transferring_write_still_records_failed_and_releases_the_lock() {
    let fs = fake();
    // `rename_replace`: the CREATED record (1), then TRANSFERRING (2), which fails.
    fs.fail_nth("rename_replace", 2, Code::IoError, std::io::ErrorKind::Other);
    let r = run_file(&fs, &cfg());
    assert!(r.copy.is_none(), "{:?}", r.copy);
    assert_eq!(failed_at(&r.stop).0, RunStep::State);
    let state = decode_state(&fs.read_file(record_path(ID)).unwrap()).unwrap();
    assert_eq!(state.state, OpState::Failed);
    assert!(!fs.exists(T_LOCK), "released");
}

#[test]
fn a_failed_write_of_failed_is_reported_beside_the_copy_error() {
    let fs = fake();
    // The copy's temporary is the 3rd `create_new`, and its write fails; the FAILED write is the 3rd `rename_replace`
    // (after the CREATED and TRANSFERRING ones), and it fails too.
    fs.on_nth("create_new", 3, |fs| fs.fail_write(std::io::Error::other("injected write")));
    fs.fail_nth("rename_replace", 3, Code::IoError, std::io::ErrorKind::Other);
    let r = run_file(&fs, &cfg());
    assert!(matches!(&r.copy, Some(Err(e)) if e.step == CopyStep::Stream), "{:?}", r.copy);
    let (step, path) = failed_at(&r.stop);
    assert_eq!((step, path), (RunStep::State, record_path(ID)));
    assert!(!fs.exists(T_LOCK), "still released");
}

#[test]
fn a_directory_the_restart_sweep_cannot_read_keeps_every_prior() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    // `d` lists as a directory but cannot be listed itself: the walk reports it and goes on.
    fs.write_file("/p/dest/d", b"");
    fs.set_type("/p/dest/d", flux_fs::FileType::Dir);
    let (r, _) = run_tree(&fs, &restart());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert!(
        r.warnings.iter().any(|w| matches!(w, RunWarning::PartialKept { path, .. }
            if path.to_string_lossy().replace('\\', "/").ends_with("/p/dest/d"))),
        "{:?}",
        r.warnings
    );
    let kept = manifest(&fs, &id(5));
    assert_eq!((kept.state, kept.superseded_by.as_deref()), (OpState::Abandoned, Some(ID)));
}

#[test]
fn a_partial_inside_a_reserved_control_directory_is_never_swept() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    fs.create_dir(Path::new("/p/dest/.flux/standalone")).unwrap();
    let reserved = format!("/p/dest/.flux/standalone/y.flux-partial.{}", id(5));
    fs.write_file(&reserved, b"not the sweep's");
    let (r, _) = run_tree(&fs, &restart());
    ok(&r);
    assert!(fs.exists(&reserved));
}

#[test]
fn a_single_files_partial_that_cannot_be_deleted_keeps_its_prior() {
    let fs = fake();
    file_prior(&fs, 5);
    let partial = format!("/p/t.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    // `remove_file`: the CREATED and ABANDONED writes clear their temporaries (1, 2); then the partial (3).
    fs.fail_nth("remove_file", 3, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let r = run_file(&fs, &restart());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?} {:?}", r.stop, r.copy);
    assert!(fs.exists(&partial));
    let kept = decode_state(&fs.read_file(record_path(&id(5))).unwrap()).unwrap();
    assert_eq!((kept.state, kept.superseded_by.as_deref()), (OpState::Abandoned, Some(ID)));
    assert!(
        r.warnings.iter().any(|w| matches!(w, RunWarning::PartialKept { .. })),
        "{:?}",
        r.warnings
    );
}

#[test]
fn a_prior_whose_state_cannot_be_removed_is_a_warning() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    // `rename_no_replace`: the probe (1), this run's workspace into place (2), then the prior's retire (3), which fails.
    fs.fail_nth(
        "rename_no_replace",
        3,
        Code::PermissionDenied,
        std::io::ErrorKind::PermissionDenied,
    );
    let (r, _) = run_tree(&fs, &restart());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert!(
        r.warnings.iter().any(|w| matches!(w, RunWarning::NotRemoved { path, .. }
            if path.to_string_lossy().contains(&id(5)))),
        "{:?}",
        r.warnings
    );
    assert_eq!(manifest(&fs, &id(5)).state, OpState::Abandoned, "passed over by every later run");
}

/// Before the `n`-th flush of a claimed lock's overwrite, move the claimed file away and leave a fresh empty lock in
/// its place, and arm the same for the next flush: every `--break-lock` attempt has to start again.
fn keep_moving_the_lock(fs: &FaultFs, n: u32) {
    fs.on_nth("lock_sync_all", n, move |fs| {
        fs.rename_replace(Path::new(LOCK), Path::new(&format!("/p/moved{n}"))).unwrap();
        fs.write_file(LOCK, b"");
        keep_moving_the_lock(fs, n + 1);
    });
}

#[test]
fn a_takeover_that_never_holds_gives_up_and_says_its_state_exists() {
    let fs = fake();
    let p = fs.destination_root(Path::new("/p")).unwrap();
    dead_lock(&p, "dest.flux-lock", b"");
    keep_moving_the_lock(&fs, 1);
    let (r, _) = run_tree(&fs, &RunConfig { break_lock: true, ..restart() });
    assert_eq!(refused(&r.stop), (LockCode::TargetLockBusy, true));
    assert!(r.copy.is_none());
    assert_eq!(manifest(&fs, ID).state, OpState::Created, "the state stays for a later --restart");
}

// Part 3b-2.

#[test]
fn a_refusal_of_the_lock_names_the_lock_path() {
    let fs = fake();
    let p = fs.destination_root(Path::new("/p")).unwrap();
    dead_lock(&p, "dest.flux-lock", b"");
    let (r, _) = run_tree(&fs, &cfg());
    let Some(RunError::Refused { refusal, .. }) = &r.stop else { panic!("{:?}", r.stop) };
    assert_eq!(refusal.code, LockCode::TargetLockUncertain);
    assert!(refusal.detail.replace('\\', "/").starts_with(LOCK), "{}", refusal.detail);
}

#[test]
fn the_hook_runs_once_before_each_guarded_mutation() {
    // A tree's guarded mutations: `a`'s sweep, temporary and publish; `sub`'s creation; `sub/b`'s three; `sub`'s
    // directory-end flush (cut 8b). A single file's: its sweep, temporary and publish.
    for (tree_run, expected) in [(true, 8), (false, 3)] {
        let fs = fake();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let hook: BeforeMutation = Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        let c = RunConfig { before_mutation: Some(hook), ..cfg() };
        if tree_run {
            ok(&run_tree(&fs, &c).0);
        } else {
            let r = run_file(&fs, &c);
            assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?}", r.stop);
        }
        assert_eq!(count.load(Ordering::SeqCst), expected, "tree {tree_run}");
    }
}

#[test]
fn a_hook_that_never_returns_stops_the_copy_before_that_mutation() {
    let fs = fake();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    // The third guarded mutation is `a`'s publish; the CLI's hook blocks there forever, this one panics instead.
    let hook: BeforeMutation = Arc::new(move || {
        if seen.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
            panic!("the test's stall point");
        }
    });
    let c = RunConfig { before_mutation: Some(hook), ..cfg() };
    let stalled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_tree(&fs, &c)));
    assert!(stalled.is_err(), "the hook stopped the run");
    assert_eq!(count.load(Ordering::SeqCst), 3);
    assert!(fs.exists(format!("/p/dest/a.flux-partial.{ID}")), "the temporary was made");
    assert!(!fs.exists("/p/dest/a"), "the publish did not happen");
}

fn file_record(fs: &FaultFs, id: &str) -> OperationState {
    decode_state(&fs.read_file(record_path(id)).unwrap()).unwrap()
}

/// `create_new`: the CREATED record (1), TRANSFERRING (2), then the copy's temporary (3), whose write fails: the
/// FAILED record stays, with every field the run wrote at creation.
fn failed_file_run(fs: &FaultFs) -> OperationState {
    fs.on_nth("create_new", 3, |fs| fs.fail_write(std::io::Error::other("injected write")));
    let r = run_file(fs, &cfg());
    assert!(matches!(&r.copy, Some(Err(e)) if e.step == CopyStep::Stream), "{:?}", r.copy);
    file_record(fs, ID)
}

#[test]
fn a_single_file_record_carries_section_249_1_from_its_creation() {
    let fs = fake();
    let s = failed_file_run(&fs);
    let f = s.file.clone().expect("a version-2 single-file record");
    assert_eq!(s.format_version, crate::state::FORMAT_VERSION);
    assert_eq!(f.artifact_type, "state");
    assert!(crate::ids::is_id(&f.attempt_id) && f.attempt_id != ID, "{}", f.attempt_id);
    assert_eq!(f.artifact_generation, 1);
    let src = fs.metadata(Path::new("/src/a")).unwrap().identity;
    assert_eq!(f.source_identity, crate::state::identity_text(src));
    assert_eq!(f.target_identity, None, "no object stood at /p/t");
    assert_eq!(f.target_path_key, crate::lock::site::hex(b"t"));
    assert_eq!(
        (f.owner_instance_id.as_str(), f.boot_session_id.as_str()),
        (id(0xee).as_str(), "test-boot")
    );
    assert_eq!(f.creation_wall_time, s.created_at);
    assert_eq!(f.last_heartbeat_wall_time, s.created_at);
    let other = fake();
    assert_ne!(
        failed_file_run(&other).file.unwrap().attempt_id,
        f.attempt_id,
        "one attempt id per run"
    );
}

#[test]
fn a_replaced_targets_identity_is_recorded_when_the_state_is_made() {
    let fs = fake();
    fs.write_file("/p/t", b"old");
    let old = fs.metadata(Path::new("/p/t")).unwrap().identity;
    let s = failed_file_run(&fs);
    assert_eq!(s.file.unwrap().target_identity, Some(crate::state::identity_text(old)));
}

/// A clean tree run whose COMPLETED workspace cannot be retired, so its manifest stays to be read. `rename_no_replace`:
/// the probe (1), the workspace (2), `a`'s and `sub/b`'s publishes (3, 4), then the retire (5), which fails.
fn kept_tree_manifest(fs: &FaultFs) -> OperationState {
    fs.fail_nth(
        "rename_no_replace",
        5,
        Code::PermissionDenied,
        std::io::ErrorKind::PermissionDenied,
    );
    let (r, _) = run_tree(fs, &cfg());
    ok(&r);
    manifest(fs, ID)
}

#[test]
fn a_tree_manifest_is_the_current_version_with_cleanup_keys_and_no_file_fields() {
    let fs = fake();
    let s = kept_tree_manifest(&fs);
    assert_eq!(s.format_version, crate::state::FORMAT_VERSION);
    assert!(s.file.is_none());
    assert!(s.cleanup.is_some());
}

#[test]
fn the_single_file_record_and_its_lock_record_carry_one_time() {
    let fs = fake();
    // As `failed_file_run`, and the lock's own removal at the release fails, so the lock record stays to be read.
    // `remove_file`: CREATED's and TRANSFERRING's temporaries (1, 2), the copy's step-1 sweep (3), the failed copy's
    // temporary (4), FAILED's temporary (5), then the lock (6).
    fs.fail_nth("remove_file", 6, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let s = failed_file_run(&fs);
    let Decoded::Record(rec) = decode(&fs.read_file(T_LOCK).expect("the lock stays")) else {
        panic!("a whole record")
    };
    let f = s.file.unwrap();
    assert_eq!(f.creation_wall_time, rec.creation_wall_time.to_string());
    assert_eq!(f.last_heartbeat_wall_time, rec.last_heartbeat_wall_time.to_string());
}

#[test]
fn a_completed_record_says_cleanup_pending_and_names_its_target_before_anything_is_removed() {
    let fs = fake();
    // `remove_file`: CREATED and TRANSFERRING clear their temporaries (1, 2), the copy's step-1 sweep (3), COMPLETED
    // (4), then the record itself (5), which fails: the COMPLETED record stays, as a crash there would leave it.
    fs.fail_nth("remove_file", 5, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let r = run_file(&fs, &cfg());
    assert!(matches!(r.copy, Some(Ok(_))), "{:?}", r.copy);
    let s = file_record(&fs, ID);
    assert_eq!(s.state, OpState::Completed);
    let c = s.cleanup.expect("version 2");
    assert!(c.cleanup_pending && c.cleanup_pending_artifacts.is_empty(), "{c:?}");
    let published = fs.metadata(Path::new("/p/t")).unwrap().identity;
    assert_eq!(s.file.unwrap().target_identity, Some(crate::state::identity_text(published)));
}

#[test]
fn a_failed_completed_write_leaves_a_failed_record_the_next_run_can_read() {
    let fs = fake();
    // `rename_replace`: CREATED (1), TRANSFERRING (2), the publish (3), then COMPLETED (4), which fails; the FAILED
    // write after it (5) succeeds. The record must not carry the COMPLETED write's `cleanup_pending` (capstone r4).
    fs.fail_nth("rename_replace", 4, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let r = run_file(&fs, &cfg());
    assert!(matches!(&r.stop, Some(RunError::Failed { step: RunStep::State, .. })), "{:?}", r.stop);
    let s = decode_state(&fs.read_file(record_path(ID)).unwrap())
        .expect("a record the next run can read");
    assert_eq!(s.state, OpState::Failed);
    assert_eq!(s.cleanup.map(|c| c.cleanup_pending), Some(false));
}

#[test]
fn an_existing_target_that_cannot_be_read_is_recorded_unavailable_not_absent() {
    let fs = fake();
    fs.write_file("/p/t", b"old");
    // `metadata`: the source (1), the target's identity gate (2), the lock's two checks (3, 4), then the record's
    // read of the existing target under the lock (5), which fails with something other than NotFound.
    fs.fail_nth("metadata", 5, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let s = failed_file_run(&fs);
    assert_eq!(s.file.unwrap().target_identity.as_deref(), Some("unavailable"));
}

#[test]
fn a_version_2_single_file_prior_superseded_by_restart_keeps_its_fields() {
    let fs = fake();
    let fields = crate::state::FileFields {
        artifact_type: crate::state::ARTIFACT_STATE.to_string(),
        attempt_id: id(7),
        artifact_generation: 1,
        source_identity: "strong:1:2".to_string(),
        target_identity: None,
        target_path_key: crate::lock::site::hex(b"t"),
        owner_instance_id: id(8),
        boot_session_id: "boot".to_string(),
        creation_wall_time: "1".to_string(),
        last_heartbeat_wall_time: "1".to_string(),
    };
    let prior = OperationState {
        state: OpState::Failed,
        ..OperationState::created_v2(&id(5), Kind::File, Path::new("/p/t"), 1, Some(fields.clone()))
    };
    fs.write_file(record_path(&id(5)), &prior.encode());
    fs.write_file(format!("/p/t.flux-partial.{}", id(5)), b"half");
    // `remove_file`: this run's CREATED write (1), the prior's ABANDONED write (2), then the prior's partial (3), which
    // fails, so the ABANDONED record stays as the partial's record (J1).
    fs.fail_nth("remove_file", 3, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let r = run_file(&fs, &restart());
    assert!(matches!(r.copy, Some(Ok(_))), "{:?} {:?}", r.stop, r.copy);
    let kept = file_record(&fs, &id(5));
    assert_eq!((kept.state, kept.superseded_by.as_deref()), (OpState::Abandoned, Some(ID)));
    assert_eq!(kept.file, Some(fields), "superseding keeps the prior's §249.1 fields");
}

#[test]
fn a_leftover_inside_a_folder_is_listed_by_its_path_below_dest() {
    let fs = fake();
    // `sub/b`'s copy fails while streaming: `create_new` is the probe's temporary (1), the manifest (2), TRANSFERRING (3),
    // `a`'s temporary (4), `sub/b`'s (5). Removing it fails too: `remove_file` is the probe's (1), the two state writes
    // (2, 3), `a`'s sweep (4), `sub/b`'s sweep (5), then `sub/b`'s temporary (6).
    fs.on_nth("create_new", 5, |fs| fs.fail_write(std::io::Error::other("injected write")));
    fs.fail_nth("remove_file", 6, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, got) = run_tree(&fs, &cfg());
    ok(&r);
    assert_eq!(got.len(), 1, "{got:?}");
    let c = manifest(&fs, ID).cleanup.expect("version 2");
    let leftover = Path::new("sub").join(format!("b.flux-partial.{ID}"));
    assert_eq!(c.cleanup_pending_artifacts, vec![crate::state::native_hex(&leftover)]);
}

#[test]
fn a_published_target_whose_identity_cannot_be_read_is_recorded_unavailable() {
    let fs = fake();
    fs.fail("handle_identity", Code::IoError);
    // As `a_completed_record_says_cleanup_pending_and_names_its_target_before_anything_is_removed`: the record's own
    // removal (the 5th `remove_file`) fails, so the COMPLETED record stays to be read.
    fs.fail_nth("remove_file", 5, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let r = run_file(&fs, &cfg());
    assert!(matches!(r.copy, Some(Ok(_))), "{:?}", r.copy);
    let s = file_record(&fs, ID);
    assert_eq!(s.state, OpState::Completed);
    assert_eq!(s.file.unwrap().target_identity.as_deref(), Some("unavailable"));
}

#[test]
fn a_clean_trees_completed_manifest_says_cleanup_pending_with_an_empty_list() {
    let fs = fake();
    let s = kept_tree_manifest(&fs);
    assert_eq!(s.state, OpState::Completed);
    let c = s.cleanup.expect("version 2");
    assert!(c.cleanup_pending && c.cleanup_pending_artifacts.is_empty(), "{c:?}");
}

// Cut 7b Part 2: the heartbeat.

/// Every heartbeat due at once.
fn beating() -> RunConfig {
    RunConfig { heartbeat_interval: std::time::Duration::ZERO, ..cfg() }
}

fn lock_record(bytes: &[u8]) -> LockRecord {
    match decode(bytes) {
        Decoded::Record(r) => r,
        other => panic!("expected a record, got {other:?}"),
    }
}

fn count(calls: &[String], prefix: &str) -> usize {
    calls.iter().filter(|c| c.starts_with(prefix)).count()
}

/// Calls 1 (this run's record) and on succeed; the `nth` and the `nth + 1` `write_at_start` fail - a heartbeat and its
/// retry. With `tear`, the lock's bytes are torn just before the first of them.
fn fail_heartbeat(fs: &FaultFs, nth: u32, lock: &'static str, tear: bool) {
    fs.on_nth("write_at_start", nth, move |fs| {
        if tear {
            fs.write_file(lock, b"torn");
        }
        fs.fail("write_at_start", Code::IoError);
    });
    fs.fail_nth("write_at_start", nth, Code::IoError, std::io::ErrorKind::Other);
}

fn file_error(r: &Run<Result<flux_fs::Outcome, CopyError>>) -> &CopyError {
    match &r.copy {
        Some(Err(e)) => e,
        other => panic!("expected a failed copy, got {other:?}"),
    }
}

#[test]
fn a_heartbeat_refreshes_the_lock_record_keeping_its_owner_and_never_flushes() {
    let fs = fake();
    let seen = Arc::new(Mutex::new(None));
    let keep = Arc::clone(&seen);
    // `rename_replace`: CREATED (1), TRANSFERRING (2), the publish (3), after the copy's heartbeats.
    fs.on_nth("rename_replace", 3, move |fs| *keep.lock().unwrap() = fs.read_file(T_LOCK));
    let r = run_file(&fs, &beating());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?} {:?}", r.stop, r.copy);
    let during = lock_record(&seen.lock().unwrap().clone().expect("the lock, read at the publish"));
    assert!(
        during.last_heartbeat_wall_time > during.creation_wall_time,
        "the heartbeat moved: {during:?}"
    );
    assert_eq!((during.operation_id.as_str(), during.owner_instance_id), (ID, id(0xee)));
    assert_eq!(during.workspace_path, format!("adjacent/{ID}"));
    let c = calls(&fs);
    assert!(count(&c, "write_at_start(") > 2, "heartbeats were written: {c:?}");
    assert_eq!(count(&c, "lock_sync_all("), 2, "only the record and Q-K flush: {c:?}");
}

#[test]
fn a_heartbeat_waits_for_its_interval() {
    let fs = fake();
    let r = run_file(&fs, &cfg());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?}", r.stop);
    assert_eq!(count(&calls(&fs), "write_at_start("), 2, "the record and Q-K, no heartbeat");
}

#[test]
fn a_failed_heartbeat_write_is_retried_once() {
    let fs = fake();
    // The first heartbeat (write 2) fails; its retry (write 3) succeeds.
    fs.fail_nth("write_at_start", 2, Code::IoError, std::io::ErrorKind::Other);
    let r = run_file(&fs, &beating());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?} {:?}", r.stop, r.copy);
    assert!(!fs.exists(T_LOCK) && !fs.exists(record_path(ID)), "a clean run");
}

#[test]
fn two_failed_heartbeat_writes_stop_the_copy_naming_the_lock_and_the_run_records_failed() {
    let fs = fake();
    fail_heartbeat(&fs, 2, T_LOCK, false);
    let r = run_file(&fs, &beating());
    let e = file_error(&r);
    assert_eq!((e.step, e.code()), (CopyStep::Heartbeat, Code::IoError));
    let message = e.cause.source.to_string().replace('\\', "/");
    assert!(message.contains(T_LOCK), "names the lock: {message}");
    assert!(r.stop.is_none(), "one error for it: {:?}", r.stop);
    assert_eq!(file_record(&fs, ID).state, OpState::Failed, "the record was intact");
    assert!(!fs.exists(T_LOCK), "released");
    assert!(!fs.exists("/p/t"), "never published");
}

#[test]
fn a_torn_heartbeat_is_that_failure_never_a_lost_lock() {
    let fs = fake();
    fail_heartbeat(&fs, 2, T_LOCK, true);
    let r = run_file(&fs, &beating());
    let e = file_error(&r);
    assert_eq!((e.step, e.code()), (CopyStep::Heartbeat, Code::IoError));
    assert!(r.stop.is_none(), "no stop of the finish's own, no TARGET_LOCK_BUSY: {:?}", r.stop);
    assert_eq!(
        file_record(&fs, ID).state,
        OpState::Transferring,
        "nothing written without proof of ownership"
    );
    assert_eq!(fs.read_file(T_LOCK).as_deref(), Some(&b"torn"[..]), "closed without unlinking");
}

#[test]
fn a_temporary_kept_after_a_torn_heartbeat_names_the_heartbeat_not_another_run() {
    let fs = fake();
    // Writes: the record (1), then the sweep's (2), the create's (3) and the chunk's (4) heartbeats; the publish's (5)
    // and its retry (6) fail, the record torn.
    fail_heartbeat(&fs, 5, T_LOCK, true);
    let r = run_file(&fs, &beating());
    let e = file_error(&r);
    assert_eq!(e.step, CopyStep::Heartbeat);
    let (left, why) =
        e.leftover.as_ref().expect("the temporary stays: the guard fails on the torn record");
    assert!(left.to_string_lossy().contains("t.flux-partial."), "{left:?}");
    let why = why.to_string();
    assert!(why.contains("heartbeat") && !why.contains("another run"), "{why}");
    assert!(fs.exists(format!("/p/t.flux-partial.{ID}")));
}

#[test]
fn a_tree_stops_at_a_failed_heartbeat_and_records_failed() {
    let fs = fake();
    fail_heartbeat(&fs, 2, LOCK, false);
    let (r, got) = run_tree(&fs, &beating());
    let a = aborted(&r);
    assert_eq!((a.error.step, a.error.code()), (CopyStep::Heartbeat, Code::IoError));
    assert!(got.is_empty(), "an abort, never a file's failure: {got:?}");
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert_eq!(manifest(&fs, ID).state, OpState::Failed);
    assert!(!fs.exists(LOCK), "released");
    assert!(!fs.exists("/p/dest/a"));
}

/// The first heartbeat (write 2) and its retry (write 3) fail with `code`.
fn fail_heartbeat_with(fs: &FaultFs, code: Code) {
    fs.on_nth("write_at_start", 2, move |fs| fs.fail("write_at_start", code));
    fs.fail_nth("write_at_start", 2, code, std::io::ErrorKind::Other);
}

#[test]
fn a_failed_heartbeat_fails_the_copy_whatever_its_code() {
    let fs = fake();
    // From the copy itself, SAFETY_REJECTED with no leftover is a refusal that changed nothing (Q-I's rollback).
    fail_heartbeat_with(&fs, Code::SafetyRejected);
    let r = run_file(&fs, &beating());
    let e = file_error(&r);
    assert_eq!((e.step, e.code()), (CopyStep::Heartbeat, Code::SafetyRejected), "the code is kept");
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert_eq!(file_record(&fs, ID).state, OpState::Failed, "recorded FAILED, never rolled back");
}

#[test]
fn a_tree_whose_heartbeat_fails_is_failed_whatever_its_code() {
    let fs = fake();
    // Nothing is copied before the first heartbeat, so from the copy itself this abort would be a refusal that
    // changed nothing (`TreeAbort::refused_unchanged`), rolled back.
    fail_heartbeat_with(&fs, Code::SafetyRejected);
    let (r, _) = run_tree(&fs, &beating());
    let a = aborted(&r);
    assert_eq!((a.error.step, a.error.code()), (CopyStep::Heartbeat, Code::SafetyRejected));
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert_eq!(manifest(&fs, ID).state, OpState::Failed, "recorded FAILED, never rolled back");
}

#[test]
fn a_heartbeat_failure_is_never_reported_as_target_lock_busy() {
    let fs = fake();
    fail_heartbeat_with(&fs, Code::TargetLockBusy);
    let r = run_file(&fs, &beating());
    let e = file_error(&r);
    assert_eq!((e.step, e.code()), (CopyStep::Heartbeat, Code::IoError));
    assert_eq!(file_record(&fs, ID).state, OpState::Failed, "a failure, not a lost lock");
}

#[test]
fn the_finish_never_heartbeats() {
    let fs = fake();
    let r = run_file(&fs, &beating());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?}", r.stop);
    let c = calls(&fs);
    let publish = at(&c, &format!("rename_replace(/p/t.flux-partial.{ID}"));
    let after = &c[publish..];
    assert_eq!(
        (count(after, "write_at_start("), count(after, "lock_sync_all(")),
        (1, 1),
        "after the publish only Q-K writes the record: {after:?}"
    );
}

#[test]
fn restart_heartbeats_during_its_sweep_before_the_copy() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    let (r, _) =
        run_tree(&fs, &RunConfig { heartbeat_interval: std::time::Duration::ZERO, ..restart() });
    ok(&r);
    let c = calls(&fs);
    let before = &c[..at(&c, &format!("remove_file({partial})"))];
    // The record (1), then the heartbeats before the ABANDONED write and before the partial's deletion.
    assert_eq!(count(before, "write_at_start("), 3, "{before:?}");
    assert_eq!(count(before, "lock_sync_all("), 1, "only the record flushes: {before:?}");
}

#[test]
fn a_failed_heartbeat_during_restart_stops_the_run_at_the_lock_step_and_copies_nothing() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    // The first heartbeat, before the ABANDONED write, and its retry.
    fail_heartbeat(&fs, 2, LOCK, false);
    let (r, _) =
        run_tree(&fs, &RunConfig { heartbeat_interval: std::time::Duration::ZERO, ..restart() });
    assert_eq!(failed_at(&r.stop), (RunStep::Lock, LOCK.to_string()));
    assert!(r.copy.is_none(), "no copy runs");
    assert!(r.warnings.is_empty(), "one error for it: {:?}", r.warnings);
    assert_eq!(manifest(&fs, &id(5)).state, OpState::Failed, "the prior is untouched");
    assert!(fs.exists(&partial));
    assert_eq!(manifest(&fs, ID).state, OpState::Failed, "the record was intact");
    assert!(!fs.exists(LOCK), "released");
}

// Cut 7b Part 2 test audit: each test below was red under the mutant named in its comment.

#[test]
fn restart_heartbeats_and_checks_ownership_before_removing_a_superseded_state() {
    // Mutant: delete `checked(locked)?` in `supersede`'s removal loop (restart.rs).
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    // Writes: the record (1), the heartbeats before the ABANDONED write (2) and the partial's deletion (3); the one
    // before the prior's state is removed (4) fails, and its retry.
    fail_heartbeat(&fs, 4, LOCK, false);
    let (r, _) =
        run_tree(&fs, &RunConfig { heartbeat_interval: std::time::Duration::ZERO, ..restart() });
    assert_eq!(failed_at(&r.stop), (RunStep::Lock, LOCK.to_string()));
    assert!(!fs.exists(&partial), "the sweep ran");
    assert_eq!(
        manifest(&fs, &id(5)).state,
        OpState::Abandoned,
        "the prior's state was not removed"
    );
}

#[test]
fn restart_stops_before_removing_a_superseded_state_once_ownership_is_lost() {
    // Mutant: delete `checked(locked)?` in `supersede`'s removal loop (restart.rs).
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    // `remove_file`: the probe's removal of `noreplace-probe` (1); this run's CREATED and the prior's ABANDONED state
    // writes clear their temporaries (2, 3); then the partial (4). Just before it, the lock is taken over, so the next check is the removal loop's.
    fs.on_nth("remove_file", 4, |fs| fs.write_file(LOCK, b"another run's bytes"));
    let (r, _) = run_tree(&fs, &restart());
    assert_eq!(refused(&r.stop), (LockCode::TargetLockBusy, true));
    assert_eq!(
        manifest(&fs, &id(5)).state,
        OpState::Abandoned,
        "the prior's state was not removed"
    );
}

#[test]
fn a_heartbeat_restarts_its_interval() {
    // Mutant: delete `self.last.set(Instant::now());` in `Pulse::beat` (session.rs).
    let fs = fake();
    let first = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&first);
    // The single file's heartbeats come before its sweep, its create, its one chunk and its publish. The hook runs at
    // the sweep's guard and waits out the interval, so the create's heartbeat is due and the two after it are not.
    let hook: BeforeMutation = Arc::new(move || {
        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
    });
    let c = RunConfig {
        heartbeat_interval: std::time::Duration::from_secs(1),
        before_mutation: Some(hook),
        ..cfg()
    };
    let r = run_file(&fs, &c);
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?} {:?}", r.stop, r.copy);
    assert_eq!(
        count(&calls(&fs), "write_at_start("),
        3,
        "the record, one heartbeat, and Q-K: {:?}",
        calls(&fs)
    );
}

#[test]
fn a_heartbeat_failure_message_names_the_lock() {
    // Mutant: change the wording `beat_error` builds (session.rs). The operator reads this line; it must say what
    // failed.
    let fs = fake();
    fail_heartbeat(&fs, 2, T_LOCK, false);
    let r = run_file(&fs, &beating());
    let message = file_error(&r).to_string().replace('\\', "/");
    assert!(
        message.contains(&format!("the heartbeat could not refresh the lock record {T_LOCK}: ")),
        "{message}"
    );
    assert!(message.starts_with("IO_ERROR: "), "{message}");
}

// Cut 8a test audit: each test below is red under the one-line mutant named in its comment.

/// The path a `Refused` names as not removed, with `/` separators.
fn not_removed_path(stop: &Option<RunError>) -> Option<String> {
    match stop {
        Some(RunError::Refused { not_removed, .. }) => {
            not_removed.as_ref().map(|(p, _)| p.to_string_lossy().replace('\\', "/"))
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// Mutant: place.rs `refuse_no_replace` keeps only `leftover` and drops what `unwind_creating` could not remove.
#[test]
fn a_refusal_whose_only_leftover_is_the_creating_directory_names_it_and_is_exit_1() {
    let fs = fake();
    fs.set_no_replace_support(false);
    // The probe temporary is removed (no fault on `remove_file`); the run's first `remove_dir` is `<id>.creating`'s.
    fs.fail_kind("remove_dir", Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let first_remove_dir = calls(&fs).into_iter().find(|c| c.starts_with("remove_dir(")).unwrap();
    assert_eq!(first_remove_dir, format!("remove_dir(/p/dest/.flux/operations/{ID}.creating)"));
    let Some(RunError::Refused { refusal, changed, .. }) = &r.stop else { panic!("{:?}", r.stop) };
    assert_eq!((refusal.code, *changed), (LockCode::NoReplacePublishUnavailable, true));
    let left = creating("").trim_end_matches('/').to_string();
    assert_eq!(not_removed_path(&r.stop), Some(left.clone()));
    assert!(fs.exists(&left), "left where it is");
    assert!(!fs.exists(creating("noreplace-probe.tmp")), "the probe temporary was removed");
    assert!(!fs.exists(LOCK), "the lock is still released");
}

// Mutant: place.rs `unwind_creating` ignores a failure to remove the control directories.
#[test]
fn a_refusal_whose_control_directories_cannot_be_removed_is_exit_1() {
    let fs = fake();
    fs.set_no_replace_support(false);
    // The run's remove_dir calls are `<id>.creating`, `.flux/operations`, `.flux`, DEST: fail the second.
    fs.fail_nth("remove_dir", 2, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let removed: Vec<String> =
        calls(&fs).into_iter().filter(|c| c.starts_with("remove_dir(")).collect();
    assert_eq!(removed[1], "remove_dir(/p/dest/.flux/operations)", "{removed:?}");
    let Some(RunError::Refused { refusal, changed, .. }) = &r.stop else { panic!("{:?}", r.stop) };
    assert_eq!((refusal.code, *changed), (LockCode::NoReplacePublishUnavailable, true));
    assert_eq!(not_removed_path(&r.stop), Some("/p/dest/.flux".to_string()));
    assert!(fs.exists("/p/dest"), "DEST is kept: the unwind stopped at the control directory");
    assert!(!fs.exists(LOCK), "the lock is still released");
}

// Mutant for the four tests below: tree.rs `containment` takes the source side as the lexical `src_root`, confirmed,
// without querying it (`let s = Canonical { path: src_root.to_path_buf(), confirmed: true };`).
#[test]
fn a_source_root_whose_canonical_path_is_unsupported_degrades_containment() {
    let lax = fake();
    // Per path: the global fault would be consumed by the anchor's query, which comes first.
    lax.fail_canonical_path_of("/src", Code::IoError, std::io::ErrorKind::Unsupported);
    let (r, _) = run_tree(&lax, &cfg());
    let out = ok(&r);
    assert_eq!(out.files_copied, 2);
    assert_eq!(
        out.containment_degraded.as_deref(),
        Some(Path::new("/src")),
        "the source root's query failed, so the source root is named"
    );

    let tight = fake();
    tight.fail_canonical_path_of("/src", Code::IoError, std::io::ErrorKind::Unsupported);
    let mut got = Vec::new();
    let strict = CopyOptions { safety: Safety::Strict, ..opts() };
    let r = tree(&tight, Path::new("/src"), Path::new("/p/dest"), &strict, &cfg(), &mut |f| {
        got.push(f)
    });
    assert_eq!(aborted(&r).error.code(), Code::SafetyRejected);
    assert!(!tight.called("create_lock") && !tight.exists("/p/dest"));
}

#[test]
fn another_source_root_canonical_path_error_aborts_before_the_lock() {
    let fs = fake();
    fs.fail_canonical_path_of("/src", Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let a = aborted(&r);
    assert_eq!((a.error.code(), a.error.step), (Code::PermissionDenied, CopyStep::Resolve));
    assert!(!fs.called("create_lock"));
}

#[test]
fn a_source_root_canonical_path_naming_another_object_aborts_before_the_lock() {
    let fs = fake();
    fs.create_dir(Path::new("/elsewhere")).unwrap(); // keeps its own Strong identity: not `/src`'s
    fs.set_canonical_path("/src", "/elsewhere");
    let (r, _) = run_tree(&fs, &cfg());
    let a = aborted(&r);
    assert_eq!((a.error.code(), a.error.step), (Code::DestinationError, CopyStep::Resolve));
    assert!(!fs.called("create_lock"));
}

#[test]
fn an_unconfirmed_source_canonical_path_alone_degrades_the_check() {
    let fs = fake();
    fs.create_dir(Path::new("/srcs")).unwrap();
    fs.set_canonical_path("/src", "/srcs");
    fs.set_identity("/srcs", FileIdentity::Weak(ObjectId { volume: 1, index: 77 }));
    let (r, _) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(out.files_copied, 2);
    assert_eq!(
        out.containment_degraded.as_deref(),
        Some(Path::new("/p")),
        "the anchor is confirmed and outside the source; only the source side is unconfirmed"
    );
}

#[test]
fn a_tree_run_creates_state_db_in_the_workspace_and_removes_it_with_it() {
    let fs = fake();
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    let c = calls(&fs);
    let probe = at(
        &c,
        &format!(
            "rename_no_replace({} -> {})",
            creating("noreplace-probe.tmp"),
            creating("noreplace-probe")
        ),
    );
    let published = at(
        &c,
        &format!(
            "rename_no_replace(/p/dest/.flux/operations/{ID}.creating -> /p/dest/.flux/operations/{ID})"
        ),
    );
    // Created in the published `<id>`, never in `<id>.creating` (Windows refuses to rename a directory holding an
    // open file).
    let store = at(&c, &format!("create_claim_store(/p/dest/.flux/operations/{ID}/state.db)"));
    assert!(probe < published && published < store, "{c:?}");
    assert!(!c.iter().any(|x| x.contains(".creating/state.db")), "{c:?}");
    assert!(!fs.exists("/p/dest/.flux"), "the workspace and its state.db are gone");
    assert!(!fs.exists(format!("/p/dest/.flux/operations/{ID}.removing")));
}

#[test]
fn a_refused_probe_removes_nothing_it_did_not_make() {
    let fs = fake();
    fs.set_no_replace_support(false);
    let (r, _) = run_tree(&fs, &cfg());
    assert!(matches!(r.stop, Some(RunError::Refused { .. })), "{:?}", r.stop);
    let c = calls(&fs);
    assert!(!c.iter().any(|x| x.starts_with("create_claim_store(")), "{c:?}");
    assert!(!c.iter().any(|x| x.contains("state.db")), "{c:?}");
}

#[test]
fn a_failed_state_db_creation_unwinds_and_names_the_file() {
    let fs = fake();
    fs.fail("create_claim_store", Code::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let (step, path) = failed_at(&r.stop);
    assert_eq!(step, RunStep::State);
    assert!(path.ends_with("state.db"), "{path}");
    for suffix in ["", ".removing", ".creating"] {
        assert!(!fs.exists(format!("/p/dest/.flux/operations/{ID}{suffix}")), "{suffix}");
    }
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert!(!fs.exists("/p/dest"), "control dirs and the DEST this run made are gone");
    assert!(!fs.exists(LOCK), "the lock is released");
    assert_eq!(fs.claim_stores_open(), 0);
}

#[test]
fn a_failed_state_creation_that_cannot_remove_its_lock_reports_the_leftover() {
    let fs = fake();
    fs.fail("create_claim_store", Code::PermissionDenied);
    // The lock this run created is given back after the unwinding, so its removal is the run's last `remove_file`:
    // the probe and manifest temporaries (1, 2), the five files of the renamed operation directory (3-7), then the
    // lock (8).
    fs.fail_nth("remove_file", 8, Code::DiskFull, std::io::ErrorKind::StorageFull);
    let (r, _) = run_tree(&fs, &cfg());
    let Some(RunError::Failed { step, path, error, not_removed }) = &r.stop else {
        panic!("expected a failure, got {:?}", r.stop)
    };
    assert_eq!(*step, RunStep::State);
    assert!(path.ends_with("state.db"), "{path:?}");
    assert_eq!(error.code, Code::PermissionDenied, "the original error is unchanged");
    let (left, why) = not_removed.as_ref().expect("the lock it could not remove is named");
    assert_eq!(left.to_string_lossy().replace('\\', "/"), LOCK);
    assert_eq!(why.code, Code::DiskFull);
}

#[test]
fn a_failed_run_with_a_removable_lock_reports_no_leftover() {
    let fs = fake();
    fs.fail("create_claim_store", Code::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let Some(RunError::Failed { step, not_removed, .. }) = &r.stop else {
        panic!("expected a failure, got {:?}", r.stop)
    };
    assert_eq!(*step, RunStep::State);
    assert!(not_removed.is_none(), "{not_removed:?}");
    assert!(!fs.exists(LOCK));
}

#[test]
fn a_kept_workspace_keeps_state_db() {
    let fs = fake();
    fs.on_nth("create_new", 4, |fs| fs.fail_write(std::io::Error::other("injected write")));
    fs.fail_nth("remove_file", 5, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.warnings.iter().any(|w| matches!(w, RunWarning::StateKept(_))), "{:?}", r.warnings);
    assert!(fs.exists(format!("/p/dest/.flux/operations/{ID}/state.db")));
}

#[test]
fn the_store_is_closed_when_state_db_is_removed_and_after_the_run() {
    let fs = fake();
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    assert_eq!(
        fs.claim_stores_open_at_last_state_db_removal(),
        Some(0),
        "state.db was removed while its store was still open (or never removed)"
    );
    assert_eq!(fs.claim_stores_open(), 0);
}

// ---- Cut 8b, Task 10: the replacement protocol in the walk ------------------------------------------------------

use crate::tree::{TreeFailure, TreeFailureCause};
use flux_fs::{
    ClaimOutcome, ClaimRecord, ClaimStatus, ClaimStore, ExistingPolicy, FileType, FluxPathKey,
};

fn with_policy(policy: ExistingPolicy) -> CopyOptions {
    CopyOptions { existing: policy, ..opts() }
}

fn run_tree_with(
    fs: &FaultFs,
    c: &RunConfig,
    o: &CopyOptions,
) -> (Run<Result<TreeOutcome, TreeAbort>>, Vec<TreeFailure>) {
    let mut got = Vec::new();
    let r = tree(fs, Path::new("/src"), Path::new("/p/dest"), o, c, &mut |f| got.push(f));
    (r, got)
}

/// `fake()` with `/p/dest` already there.
fn dest_fake() -> FaultFs {
    let fs = fake();
    fs.create_dir(Path::new("/p/dest")).unwrap();
    fs
}

fn strong(fs: &FaultFs, path: &str) -> ObjectId {
    match fs.metadata(Path::new(path)).unwrap().identity {
        FileIdentity::Strong(id) => id,
        other => panic!("{path} is not strong: {other:?}"),
    }
}

fn key(s: &str) -> FluxPathKey {
    FluxPathKey(s.as_bytes().to_vec())
}

/// Write `bytes` at `path` with the given modified time (`write_file` leaves it unavailable).
fn put(fs: &FaultFs, path: &str, bytes: &[u8], secs: u64) {
    let mut w = fs.create_new(Path::new(path)).unwrap();
    std::io::Write::write_all(&mut w, bytes).unwrap();
    let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    fs.set_times(&w, Some(t)).unwrap();
}

fn code_of(f: &TreeFailure) -> Code {
    match &f.cause {
        TreeFailureCause::Copy(e) => e.code(),
        TreeFailureCause::CreateDir(e) | TreeFailureCause::ClaimNotRecorded(e) => e.code,
        other => panic!("unexpected cause {other:?}"),
    }
}

/// The index of the first call starting with `prefix` at or after `from`.
fn after(calls: &[String], from: usize, prefix: &str) -> usize {
    from + calls[from..]
        .iter()
        .position(|c| c.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix} after {from} in {calls:?}"))
}

#[test]
fn an_existing_file_is_replaced_by_default() {
    let fs = dest_fake();
    fs.write_file("/p/dest/a", b"old");
    let dest = strong(&fs, "/p/dest");
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert!(got.is_empty(), "{got:?}");
    assert_eq!((out.files_overwritten, out.files_copied, out.files_skipped), (1, 2, 0));
    assert!(out.failures.is_empty());
    assert_eq!(fs.read_file("/p/dest/a").as_deref(), Some(&b"A"[..]));
    assert_eq!(
        fs.claim(dest, "a"),
        Some(ClaimRecord { target: key("a"), status: ClaimStatus::Created })
    );
    let sub = strong(&fs, "/p/dest/sub");
    assert_eq!(
        fs.claim(sub, "b"),
        Some(ClaimRecord { target: key("sub\0b"), status: ClaimStatus::Created })
    );
    // The protocol claims one key per target whose planned and stored names agree: `(dest, "a")` for the replaced
    // entry (inserted `Existing`, upgraded to `Created`) and `(sub, "b")` for the new file. Nothing else claims.
    assert_eq!(fs.claim_count(), 2);
}

#[test]
fn the_claim_comes_after_the_gate_and_before_the_temporary() {
    let fs = dest_fake();
    fs.write_file("/p/dest/a", b"old");
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    let c = calls(&fs);
    let first = at(&c, "metadata(/p/dest/a)");
    let claim = at(&c, "claim_insert(a)");
    // The resolve stat and the identity gate's stat both precede the claim.
    let stats = c[..claim].iter().filter(|x| *x == "metadata(/p/dest/a)").count();
    assert!(stats >= 2, "resolve and gate stat before the claim: {c:?}");
    let temp = after(&c, claim, &format!("create_new(/p/dest/a.flux-partial.{ID}"));
    let rename = after(&c, temp, "rename_replace(");
    let upgrade = at(&c, "claim_upgrade(a)");
    assert!(first < claim && claim < temp && temp < rename && rename < upgrade, "{c:?}");
}

#[test]
fn skip_existing_skips_and_claims_nothing() {
    let fs = dest_fake();
    fs.write_file("/p/dest/a", b"old");
    let dest = strong(&fs, "/p/dest");
    let (r, got) = run_tree_with(&fs, &cfg(), &with_policy(ExistingPolicy::SkipExisting));
    let out = ok(&r);
    assert!(got.is_empty(), "a skipped file is not a failure: {got:?}");
    assert_eq!((out.files_skipped, out.bytes_skipped), (1, 1), "the SOURCE length of a");
    assert_eq!((out.files_copied, out.files_overwritten), (1, 0), "only sub/b is new");
    assert_eq!(fs.read_file("/p/dest/a").as_deref(), Some(&b"old"[..]));
    assert_eq!(fs.claim(dest, "a"), None);
    assert_eq!(fs.claim_count(), 1, "sub/b only");
    let c = calls(&fs);
    assert!(!c.iter().any(|x| x.starts_with("claim_insert(a)") || x.contains("a.flux-partial")));
}

#[test]
fn update_replaces_only_a_newer_or_different_size_file() {
    let fs = FaultFs::new();
    for d in ["/src", "/src/sub", "/p", "/p/dest", "/p/dest/sub"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    // (name, source bytes, source time, destination bytes, destination time, replaced)
    put(&fs, "/src/a", b"new", 20);
    put(&fs, "/p/dest/a", b"old", 10); // newer source: replaced
    put(&fs, "/src/c", b"CCC", 5);
    put(&fs, "/p/dest/c", b"dd", 99); // different size: replaced, though older
    put(&fs, "/src/d", b"new", 5);
    put(&fs, "/p/dest/d", b"old", 99); // older, same size: skipped
    put(&fs, "/src/sub/b", b"new", 30);
    put(&fs, "/p/dest/sub/b", b"old", 30); // same time, same size: skipped
    let (r, got) = run_tree_with(&fs, &cfg(), &with_policy(ExistingPolicy::Update));
    let out = ok(&r);
    assert!(got.is_empty(), "{got:?}");
    assert_eq!(fs.read_file("/p/dest/a").as_deref(), Some(&b"new"[..]));
    assert_eq!(fs.read_file("/p/dest/c").as_deref(), Some(&b"CCC"[..]));
    assert_eq!(fs.read_file("/p/dest/d").as_deref(), Some(&b"old"[..]));
    assert_eq!(fs.read_file("/p/dest/sub/b").as_deref(), Some(&b"old"[..]));
    assert_eq!((out.files_overwritten, out.files_copied), (2, 2));
    assert_eq!((out.files_skipped, out.bytes_skipped), (2, 6), "d and sub/b, 3 source bytes each");
    assert_eq!(fs.claim_count(), 2, "only the replaced entries are claimed");
}

#[test]
fn a_new_file_is_claimed_created() {
    let fs = fake();
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert!(got.is_empty(), "{got:?}");
    assert_eq!((out.files_copied, out.files_overwritten, out.files_skipped), (2, 0, 0));
    let dest = strong(&fs, "/p/dest");
    let sub = strong(&fs, "/p/dest/sub");
    assert_eq!(
        fs.claim(dest, "a"),
        Some(ClaimRecord { target: key("a"), status: ClaimStatus::Created })
    );
    assert_eq!(
        fs.claim(sub, "b"),
        Some(ClaimRecord { target: key("sub\0b"), status: ClaimStatus::Created })
    );
    assert_eq!(fs.claim_count(), 2);
    assert!(!calls(&fs).iter().any(|x| x.starts_with("rename_replace(/p/dest/a.flux-partial")));
}

#[test]
fn a_policy_over_new_files_copies_them_and_the_copy_path_never_skips() {
    for policy in [ExistingPolicy::SkipExisting, ExistingPolicy::Update] {
        // DEST made by the run, and DEST pre-existing with the files absent.
        for fs in [fake(), dest_fake()] {
            let (r, got) = run_tree_with(&fs, &cfg(), &with_policy(policy));
            let out = ok(&r);
            assert!(got.is_empty(), "{policy:?}: {got:?}");
            assert_eq!(out.files_copied, 2, "{policy:?}");
            assert_eq!((out.files_skipped, out.bytes_skipped, out.files_overwritten), (0, 0, 0));
            assert_eq!(fs.read_file("/p/dest/a").as_deref(), Some(&b"A"[..]));
            assert_eq!(fs.read_file("/p/dest/sub/b").as_deref(), Some(&b"BB"[..]));
        }
    }
}

/// A case-folding destination `/p/dest` holding `existing` (content `old`), and two sources that fold together.
fn folding(existing: &str) -> FaultFs {
    let fs = FaultFs::new();
    for d in ["/src", "/p", "/p/dest"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/src/File.txt", b"upper");
    fs.write_file("/src/file.txt", b"lower");
    fs.write_file(format!("/p/dest/{existing}"), b"old");
    fs.set_case_insensitive(true);
    fs
}

/// One of the two folding sources replaced the entry, the other is a collision, and the entry holds the FIRST
/// source's content (the walk sorts `File.txt` before `file.txt`).
fn assert_one_replaced_one_collides(
    fs: &FaultFs,
    got: &[TreeFailure],
    out: &TreeOutcome,
    at: &str,
    stored: &str,
) {
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(code_of(&got[0]), Code::DestinationNamespaceCollision);
    assert_eq!(got[0].path, PathBuf::from("file.txt"));
    assert_eq!((out.files_overwritten, out.files_copied), (1, 1));
    assert_eq!(fs.read_file(at).as_deref(), Some(&b"upper"[..]), "the first source's content");
    // Both spellings are claimed for the first source, whatever the platform left on disk: the stored name `stored`
    // (upgraded to Created) and the planned name `File.txt` (inserted Created).
    let dest = strong(fs, "/p/dest");
    let created = |t: &str| Some(ClaimRecord { target: key(t), status: ClaimStatus::Created });
    assert_eq!(fs.claim(dest, stored), created("File.txt"), "the stored spelling");
    assert_eq!(fs.claim(dest, "File.txt"), created("File.txt"), "the planned spelling");
}

#[test]
fn two_source_names_folding_onto_one_existing_entry_collide() {
    let fs = folding("FILE.TXT");
    let (r, got) = run_tree(&fs, &cfg());
    assert_one_replaced_one_collides(&fs, &got, ok(&r), "/p/dest/FILE.TXT", "FILE.TXT");
    // The entry keeps its old spelling on this platform (Linux and macOS).
    assert!(!fs.exists("/p/dest/File.txt"));
}

#[test]
fn two_source_names_folding_onto_one_existing_entry_collide_when_a_replace_renames() {
    let fs = folding("FILE.TXT");
    fs.set_replace_renames(true);
    let (r, got) = run_tree(&fs, &cfg());
    // Windows: the entry takes the planned spelling of the first source.
    assert_one_replaced_one_collides(&fs, &got, ok(&r), "/p/dest/File.txt", "FILE.TXT");
}

#[test]
fn two_source_names_folding_onto_one_entry_stored_lowercase_collide() {
    let fs = folding("file.txt");
    let (r, got) = run_tree(&fs, &cfg());
    assert_one_replaced_one_collides(&fs, &got, ok(&r), "/p/dest/file.txt", "file.txt");
}

#[test]
fn a_directory_in_the_way_fails_only_that_target() {
    let fs = dest_fake();
    fs.create_dir(Path::new("/p/dest/a")).unwrap();
    let dest = strong(&fs, "/p/dest");
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(code_of(&got[0]), Code::DestinationError);
    assert_eq!(got[0].path, PathBuf::from("a"));
    assert_eq!((out.files_copied, out.files_overwritten), (1, 0), "sub/b is copied");
    assert_eq!(fs.metadata(Path::new("/p/dest/a")).unwrap().file_type, FileType::Dir);
    assert_eq!(fs.claim(dest, "a"), None);
    assert_eq!(fs.read_file("/p/dest/sub/b").as_deref(), Some(&b"BB"[..]));
}

#[test]
fn a_symlink_at_the_destination_is_replaced_as_a_link() {
    let fs = dest_fake();
    // The fake's links point nowhere, so the link's "target" is a file beside it that must stay as it is.
    fs.write_file("/p/target", b"T");
    fs.add_symlink("/p/dest/a");
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert!(got.is_empty(), "{got:?}");
    assert_eq!((out.files_overwritten, out.files_copied), (1, 2));
    assert_eq!(fs.metadata(Path::new("/p/dest/a")).unwrap().file_type, FileType::File);
    assert_eq!(fs.read_file("/p/dest/a").as_deref(), Some(&b"A"[..]));
    assert!(fs.exists("/p/target"), "the link's target still exists");
    assert_eq!(fs.read_file("/p/target").as_deref(), Some(&b"T"[..]), "and is untouched");
}

#[test]
fn a_read_only_destination_fails_that_target_with_permission_denied() {
    let fs = dest_fake();
    fs.write_file("/p/dest/a", b"old");
    let dest = strong(&fs, "/p/dest");
    // The fake has no read-only bit: the refusal of the replacing rename is how cut 4 tests model it. Armed at the
    // claim, so the fault meets `a`'s publish and not the state manifest's earlier `rename_replace`.
    fs.on_nth("claim_insert", 1, |fs| {
        fs.fail_kind(
            "rename_replace",
            Code::PermissionDenied,
            std::io::ErrorKind::PermissionDenied,
        );
    });
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(code_of(&got[0]), Code::PermissionDenied);
    assert_eq!(got[0].path, PathBuf::from("a"));
    assert_eq!(fs.read_file("/p/dest/a").as_deref(), Some(&b"old"[..]));
    assert_eq!((out.files_copied, out.files_overwritten), (1, 0));
    assert_eq!(fs.read_file("/p/dest/sub/b").as_deref(), Some(&b"BB"[..]));
    // The claim stays: it is never released.
    assert_eq!(fs.claim(dest, "a").map(|r| r.status), Some(ClaimStatus::Existing));
}

#[test]
fn a_weak_parent_identity_degrades_to_no_replace() {
    let fs = dest_fake();
    fs.write_file("/p/dest/a", b"old");
    fs.set_identity("/p/dest", FileIdentity::Weak(ObjectId { volume: 9, index: 9 }));
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(code_of(&got[0]), Code::DestinationNamespaceCollision);
    assert_eq!(got[0].path, PathBuf::from("a"));
    assert_eq!(fs.read_file("/p/dest/a").as_deref(), Some(&b"old"[..]));
    assert_eq!((out.files_overwritten, out.files_copied), (0, 1), "sub/b is new");
    assert_eq!(out.replace_degraded.as_ref().map(|g| g.count), Some(1));
    // `sub` was created by this run and has a Strong identity: its file is claimed; the weak root claims nothing.
    assert_eq!(fs.claim_count(), 1);
    assert!(!calls(&fs).iter().any(|x| x.starts_with("claim_insert(a)")));
}

#[test]
fn a_weak_directory_under_strict_safety_is_rejected_with_its_subtree() {
    let fs = dest_fake();
    fs.create_dir(Path::new("/p/dest/sub")).unwrap();
    fs.write_file("/p/dest/sub/b", b"old");
    fs.set_identity("/p/dest/sub", FileIdentity::Weak(ObjectId { volume: 9, index: 9 }));
    let strict = CopyOptions { safety: Safety::Strict, ..opts() };
    let (r, got) = run_tree_with(&fs, &cfg(), &strict);
    let out = ok(&r);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(code_of(&got[0]), Code::SafetyRejected);
    assert_eq!(got[0].path, PathBuf::from("sub"));
    assert_eq!(fs.read_file("/p/dest/sub/b").as_deref(), Some(&b"old"[..]));
    assert_eq!((out.files_copied, out.files_overwritten), (1, 0), "a is new; sub is skipped");
    assert!(out.replace_degraded.is_none());
}

#[test]
fn a_claim_store_error_before_the_rename_fails_that_target_only() {
    let fs = dest_fake();
    fs.write_file("/p/dest/a", b"old");
    fs.fail("claim_insert", Code::IoError);
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(code_of(&got[0]), Code::IoError);
    assert_eq!(got[0].path, PathBuf::from("a"));
    assert!(matches!(&got[0].cause, TreeFailureCause::Copy(e) if e.step == CopyStep::Claim));
    assert_eq!(fs.read_file("/p/dest/a").as_deref(), Some(&b"old"[..]));
    assert!(
        !calls(&fs).iter().any(|x| x.starts_with("create_new(/p/dest/a.flux-partial")),
        "nothing was created for it"
    );
    assert_eq!((out.files_copied, out.files_overwritten), (1, 0), "the sibling is copied");
    assert_eq!(fs.read_file("/p/dest/sub/b").as_deref(), Some(&b"BB"[..]));
}

#[test]
fn a_claim_store_error_after_the_rename_is_claim_not_recorded_and_the_file_is_counted() {
    let fs = dest_fake();
    fs.write_file("/p/dest/a", b"old");
    fs.fail_nth("claim_upgrade", 1, Code::IoError, std::io::ErrorKind::Other);
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(matches!(&got[0].cause, TreeFailureCause::ClaimNotRecorded(_)), "{got:?}");
    assert_eq!(got[0].path, PathBuf::from("a"));
    assert_eq!(fs.read_file("/p/dest/a").as_deref(), Some(&b"A"[..]), "published");
    assert_eq!((out.files_overwritten, out.files_copied), (1, 2));
    assert_eq!(out.failures.claim_not_recorded, 1);
    assert_eq!(out.failures.total(), 1, "exit 1 follows from a counted failure");
    let dest = strong(&fs, "/p/dest");
    assert_eq!(fs.claim(dest, "a").map(|r| r.status), Some(ClaimStatus::Existing));
}

#[test]
fn a_target_that_finds_its_own_claim_proceeds() {
    use crate::copy::{no_heartbeat, unguarded};
    use crate::tree::{Shared, copy_tree_at, prepare_source};
    let fs = dest_fake();
    fs.write_file("/p/dest/a", b"old");
    let root = fs.destination_root(Path::new("/p/dest")).unwrap();
    let dest = strong(&fs, "/p/dest");
    // A claim already on `(dest, "a")` for this very target, as a resumed target would find it.
    let mut store = root.create_claim_store(OsStr::new("state.db"), Durability::Normal).unwrap();
    let own = ClaimRecord { target: key("a"), status: ClaimStatus::Existing };
    let key_a = flux_fs::ClaimKey::new(dest, OsStr::new("a"));
    assert!(matches!(store.insert_if_absent(&key_a, &own).unwrap(), ClaimOutcome::Inserted));
    let claims = std::cell::RefCell::new(store);
    let source = prepare_source(&fs, Path::new("/src"), Path::new("/p/dest")).unwrap();
    let o = opts();
    let cx = Shared {
        fs: &fs,
        src_root: Path::new("/src"),
        src_identity: source.identity,
        opts: &o,
        guard: &unguarded,
        beat: &no_heartbeat,
        claims: Some(&claims),
    };
    let mut out = TreeOutcome::default();
    let mut got = Vec::new();
    let (_root, walked) =
        copy_tree_at(&cx, source.events, root, false, &mut out, &mut |f| got.push(f));
    walked.unwrap();
    assert!(got.is_empty(), "{got:?}");
    assert_eq!((out.files_overwritten, out.files_copied), (1, 2));
    assert_eq!(fs.read_file("/p/dest/a").as_deref(), Some(&b"A"[..]));
    assert_eq!(
        fs.claim(dest, "a").map(|r| (r.target, r.status)),
        Some((key("a"), ClaimStatus::Created))
    );
}

/// Case-folding sources `File.txt` / `file.txt` under `dir` of `/src`, and a DEST the run creates.
fn fold_into_created(dir: &str) -> FaultFs {
    let fs = FaultFs::new();
    for d in ["/src", "/p"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    if !dir.is_empty() {
        fs.create_dir(Path::new(&format!("/src/{dir}"))).unwrap();
    }
    let at = |n: &str| if dir.is_empty() { format!("/src/{n}") } else { format!("/src/{dir}/{n}") };
    fs.write_file(at("File.txt"), b"upper");
    fs.write_file(at("file.txt"), b"lower");
    fs.set_case_insensitive(true);
    // The first target's post-publish claim fails, so only the no-replace rename can refuse the second.
    fs.fail_nth("claim_insert", 1, Code::IoError, std::io::ErrorKind::Other);
    fs
}

fn assert_fold_refused_by_no_replace(
    got: &[TreeFailure],
    out: &TreeOutcome,
    first: &str,
    second: &str,
) {
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(matches!(&got[0].cause, TreeFailureCause::ClaimNotRecorded(_)), "{got:?}");
    assert_eq!(got[0].path, PathBuf::from(first));
    assert_eq!(code_of(&got[1]), Code::DestinationNamespaceCollision);
    assert_eq!(got[1].path, PathBuf::from(second));
    assert_eq!(
        (out.files_copied, out.files_overwritten),
        (1, 0),
        "the first is published and counted"
    );
    assert_eq!(out.failures.claim_not_recorded, 1);
}

#[test]
fn a_fold_in_a_created_directory_is_refused_by_the_no_replace_rename_even_when_the_first_claim_failed()
 {
    let fs = fold_into_created("sub");
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_fold_refused_by_no_replace(&got, out, "sub/File.txt", "sub/file.txt");
    assert_eq!(
        fs.read_file("/p/dest/sub/File.txt").as_deref(),
        Some(&b"upper"[..]),
        "the FIRST source's content"
    );
    assert!(
        !calls(&fs).iter().any(|x| x.starts_with("rename_replace(/p/dest/sub/")),
        "no replacing rename"
    );
}

#[test]
fn a_fold_in_a_destination_the_run_created_is_refused_by_the_no_replace_rename_even_when_the_first_claim_failed()
 {
    let fs = fold_into_created("");
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_fold_refused_by_no_replace(&got, out, "File.txt", "file.txt");
    assert_eq!(
        fs.read_file("/p/dest/File.txt").as_deref(),
        Some(&b"upper"[..]),
        "the FIRST source's content"
    );
    assert!(
        !calls(&fs).iter().any(|x| x.starts_with("rename_replace(/p/dest/File")),
        "no replacing rename"
    );
}

/// Case-folding sources `File.txt` / `file.txt` and a DEST that ALREADY EXISTED (empty), with the first claim failing.
fn fold_into_existing() -> FaultFs {
    let fs = FaultFs::new();
    for d in ["/src", "/p", "/p/dest"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/src/File.txt", b"upper");
    fs.write_file("/src/file.txt", b"lower");
    fs.set_case_insensitive(true);
    fs.fail_nth("claim_insert", 1, Code::IoError, std::io::ErrorKind::Other);
    fs
}

fn assert_existing_fold_refused(fs: &FaultFs) {
    let (r, got) = run_tree(fs, &cfg());
    let out = ok(&r);
    assert_fold_refused_by_no_replace(&got, out, "File.txt", "file.txt");
    assert_eq!(
        fs.read_file("/p/dest/File.txt").as_deref(),
        Some(&b"upper"[..]),
        "the FIRST source's content"
    );
    assert!(
        !calls(fs).iter().any(|x| x.starts_with("rename_replace(/p/dest/File")
            || x.starts_with("rename_replace(/p/dest/file")),
        "no replacing rename: {:?}",
        calls(fs)
    );
}

#[test]
fn a_fold_in_a_pre_existing_directory_is_refused_even_when_the_first_claim_failed() {
    assert_existing_fold_refused(&fold_into_existing());
}

#[test]
fn a_fold_in_a_pre_existing_directory_is_refused_even_when_the_first_claim_failed_windows_style() {
    let fs = fold_into_existing();
    fs.set_replace_renames(true);
    assert_existing_fold_refused(&fs);
}

// Cut 8b Task 11: the directory-end flush and the final sync. Each test is red under the one-line mutant named in its
// comment.

/// `fake()` plus a second directory, `/src/sub2/c`, so a run has two directories below the root.
fn fake_two_dirs() -> FaultFs {
    let fs = fake();
    fs.create_dir(Path::new("/src/sub2")).unwrap();
    fs.write_file("/src/sub2/c", b"C");
    fs
}

/// The number of `claim_flush` calls in the log.
fn flushes(fs: &FaultFs) -> usize {
    count(&calls(fs), "claim_flush")
}

// Mutant: tree.rs `walk_into`'s `DirEnd` arm drops the `(cx.guard)()` call before the flush.
#[test]
fn a_directory_end_flushes_after_the_heartbeat_and_the_guard() {
    let fs = fake();
    let hooks = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hooks);
    let hook: BeforeMutation = Arc::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    // The guard hooks counted just before `sub/b`'s claim insert (the 2nd) and just before the flush.
    let (at_upgrade, at_flush) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (h, u) = (Arc::clone(&hooks), Arc::clone(&at_upgrade));
    fs.on_nth("claim_insert", 2, move |_| u.store(h.load(Ordering::SeqCst), Ordering::SeqCst));
    let (h, f) = (Arc::clone(&hooks), Arc::clone(&at_flush));
    fs.on_nth("claim_flush", 1, move |_| f.store(h.load(Ordering::SeqCst), Ordering::SeqCst));
    let c = RunConfig { before_mutation: Some(hook), ..beating() };
    let (r, got) = run_tree(&fs, &c);
    ok(&r);
    assert!(got.is_empty(), "{got:?}");
    assert_eq!(
        at_flush.load(Ordering::SeqCst),
        at_upgrade.load(Ordering::SeqCst) + 1,
        "exactly one guard between the last claim of `sub` and its flush"
    );
    let c = calls(&fs);
    let upgrade = at(&c, "claim_insert(b)");
    let beat = after(&c, upgrade, "write_at_start(");
    let flush = after(&c, upgrade, "claim_flush");
    assert!(upgrade < beat && beat < flush, "the heartbeat sits between: {c:?}");
}

// Mutant: the `DirEnd` arm flushes whatever frame it pops, `Frame::Skipped` included.
#[test]
fn a_skipped_directory_flushes_nothing() {
    let fs = dest_fake();
    fs.add_symlink("/p/dest/sub");
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].path, PathBuf::from("sub"));
    assert_eq!(out.failures.claim_not_recorded, 0);
    assert_eq!(flushes(&fs), 0, "{:?}", calls(&fs));
}

// Mutant: the `DirEnd` arm discards the flush error (`let _ = ...flush();`).
#[test]
fn a_flush_error_is_claim_not_recorded_for_that_directory_and_the_walk_continues() {
    let fs = fake_two_dirs();
    fs.fail_nth("claim_flush", 1, Code::IoError, std::io::ErrorKind::Other);
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(matches!(&got[0].cause, TreeFailureCause::ClaimNotRecorded(_)), "{got:?}");
    assert_eq!(got[0].path, PathBuf::from("sub"));
    assert_eq!(out.failures.claim_not_recorded, 1);
    assert_eq!(fs.read_file("/p/dest/sub/b").as_deref(), Some(&b"BB"[..]));
    assert_eq!(fs.read_file("/p/dest/sub2/c").as_deref(), Some(&b"C"[..]), "the walk continued");
    assert_eq!(flushes(&fs), 2, "the sibling was flushed too");
}

// Mutant: the `DirEnd` arm drops the `(cx.guard)()?` line (or ignores its error).
#[test]
fn a_lost_lock_aborts_at_the_directory_end_flush() {
    let fs = fake();
    // Just before `sub/b`'s claim insert (the 2nd) the lock is taken over; nothing after the insert guards but the
    // directory end.
    fs.on_nth("claim_insert", 2, |fs| fs.write_file(LOCK, b"another run's bytes"));
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "the copy's abort is the report: {:?}", r.stop);
    let a = aborted(&r);
    assert_eq!(a.error.code(), Code::TargetLockBusy);
    assert_eq!(flushes(&fs), 0, "no flush, no final sync: {:?}", calls(&fs));
}

// Mutant: `run::tree` makes the final sync for every `Ended`.
#[test]
fn a_clean_run_makes_no_final_sync() {
    let fs = fake_two_dirs();
    let (r, got) = run_tree(&fs, &cfg());
    ok(&r);
    assert!(got.is_empty(), "{got:?}");
    assert_eq!(flushes(&fs), 2, "one per directory, the root excluded: {:?}", calls(&fs));
}

/// A run that fails after `a` and `sub/b`'s directory were made: the no-replace publish of `sub/b` is unavailable.
/// `rename_no_replace` calls before it are the workspace's and `a`'s.
fn failing_run(fs: &FaultFs, nth: u32) {
    fs.fail_nth("rename_no_replace", nth, Code::IoError, std::io::ErrorKind::Unsupported);
}

// Mutants: `run::tree` skips the final sync for `Ended::Failed`; it syncs when the heartbeat failed.
#[test]
fn a_kept_workspace_after_a_failure_gets_a_final_sync() {
    let fs = fake();
    failing_run(&fs, 4);
    let (r, _) = run_tree(&fs, &cfg());
    let a = aborted(&r);
    assert_eq!(a.error.code(), Code::NoReplacePublishUnavailable, "{a:?}");
    assert_eq!(manifest(&fs, ID).state, OpState::Failed);
    assert_eq!(flushes(&fs), 1, "the final sync: `sub` never ended: {:?}", calls(&fs));
    let c = calls(&fs);
    assert!(at(&c, "claim_insert(a)") < at(&c, "claim_flush"), "{c:?}");
    assert!(!fs.exists(LOCK), "released");

    // After a failed heartbeat nothing is written: no final sync. The abort is the heartbeat's.
    let fs = fake();
    fail_heartbeat(&fs, 2, LOCK, false);
    let (r, _) = run_tree(&fs, &beating());
    let a = aborted(&r);
    assert_eq!(a.error.step, CopyStep::Heartbeat);
    assert_eq!(flushes(&fs), 0, "{:?}", calls(&fs));
}

// Mutants: `run::tree` skips the final sync for a completed run with leftovers; it lets a failed one go unreported.
#[test]
fn a_completed_run_with_leftovers_gets_a_final_sync_and_reports_its_failure() {
    let fs = fake();
    // As `a_temporary_the_copy_could_not_remove_keeps_the_completed_state`; the flushes are `sub`'s (1), then the final
    // sync's (2), which fails.
    fs.on_nth("create_new", 4, |fs| fs.fail_write(std::io::Error::other("injected write")));
    fs.fail_nth("remove_file", 5, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    fs.fail_nth("claim_flush", 2, Code::IoError, std::io::ErrorKind::Other);
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(flushes(&fs), 2, "{:?}", calls(&fs));
    let last = got.last().expect("the final sync's failure is streamed");
    assert!(matches!(&last.cause, TreeFailureCause::ClaimNotRecorded(_)), "{got:?}");
    assert_eq!(last.path, PathBuf::new(), "the root");
    assert_eq!((out.failures.claim_not_recorded, out.failures.copy), (1, 1), "{got:?}");
    assert!(r.warnings.iter().any(|w| matches!(w, RunWarning::StateKept(_))), "{:?}", r.warnings);
    assert_eq!(manifest(&fs, ID).state, OpState::Completed);

    // Without the injected flush failure the final sync is silent.
    let fs = fake();
    fs.on_nth("create_new", 4, |fs| fs.fail_write(std::io::Error::other("injected write")));
    fs.fail_nth("remove_file", 5, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(flushes(&fs), 2, "{:?}", calls(&fs));
    assert_eq!(out.failures.claim_not_recorded, 0, "{got:?}");
}

// Mutants: `run::tree` syncs for `Ended::RefusedUnchanged`; it syncs for `Ended::Lost`.
#[test]
fn no_final_sync_after_a_refusal_or_a_lost_lock() {
    // A refusal: `sub` is DEST by identity, nothing changed, the workspace is removed again.
    let fs = FaultFs::new();
    for d in ["/src", "/src/sub", "/p", "/p/dest"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/src/sub/b", b"BB");
    fs.set_identity("/src/sub", fs.metadata(Path::new("/p/dest")).unwrap().identity);
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert!(aborted(&r).refused_unchanged());
    assert_eq!(flushes(&fs), 0, "{:?}", calls(&fs));

    // A lost lock, before the first directory ends and where the walk's own guard notices.
    let fs = fake();
    fs.on_nth("create_new", 4, |fs| fs.write_file(LOCK, b"another run's bytes"));
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(aborted(&r).error.code(), Code::TargetLockBusy);
    assert_eq!(flushes(&fs), 0, "{:?}", calls(&fs));
}

// Cut 9a Task 6: `--resume` adopts the one resumable prior.

use crate::fault_fs::FakeClaimStore;
use crate::state::{Config, Options, Root, absolute_lexical, identity_text, native_hex};
use flux_fs::{ClaimKey, DirHandle};

fn resume() -> RunConfig {
    RunConfig { resume: true, ..cfg() }
}

fn detail(stop: &Option<RunError>) -> String {
    match stop {
        Some(RunError::Refused { refusal, .. }) => refusal.detail.clone(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn state_db(n: u8) -> String {
    format!("/p/dest/.flux/operations/{}/state.db", id(n))
}

fn tree_config(fs: &FaultFs, o: &CopyOptions) -> Config {
    Config::new(
        vec![Root {
            source_root: native_hex(&absolute_lexical(Path::new("/src"))),
            source_identity: identity_text(fs.metadata(Path::new("/src")).unwrap().identity),
            destination_prefix: String::new(),
        }],
        Options::of(o),
    )
}

fn write_manifest(fs: &FaultFs, state: &OperationState) {
    fs.write_file(
        format!("/p/dest/.flux/operations/{}/manifest", state.operation_id),
        &state.encode(),
    );
}

/// A version-3 prior tree operation `n` in state `s`, written under `/p/dest` with `o`'s options.
fn prior3_with(fs: &FaultFs, n: u8, s: OpState, o: &CopyOptions) -> OperationState {
    for d in ["/p/dest", "/p/dest/.flux", "/p/dest/.flux/operations"] {
        if !fs.exists(d) {
            fs.create_dir(Path::new(d)).unwrap();
        }
    }
    fs.create_dir(Path::new(&format!("/p/dest/.flux/operations/{}", id(n)))).unwrap();
    let state = OperationState {
        state: s,
        ..OperationState::created(
            &id(n),
            Kind::Tree,
            Path::new("/p/dest"),
            1,
            None,
            tree_config(fs, o),
        )
    };
    write_manifest(fs, &state);
    state
}

fn prior3(fs: &FaultFs, n: u8, s: OpState) -> OperationState {
    prior3_with(fs, n, s, &opts())
}

fn prior_store(fs: &FaultFs, n: u8) -> FakeClaimStore {
    fs.destination_root(Path::new(&format!("/p/dest/.flux/operations/{}", id(n))))
        .unwrap()
        .create_claim_store(OsStr::new("state.db"), Durability::Normal)
        .unwrap()
}

fn adopted(r: &Run<Result<TreeOutcome, TreeAbort>>, n: u8, claims: Option<u64>) {
    assert_eq!(
        r.resumed,
        Some(ResumeNote::Adopted { operation_id: id(n), claims }),
        "{:?} {:?}",
        r.stop,
        r.resumed
    );
}

fn lock_record_during(fs: &FaultFs, nth_create_new: u32) -> Arc<Mutex<Option<LockRecord>>> {
    let seen: Arc<Mutex<Option<LockRecord>>> = Arc::default();
    let keep = Arc::clone(&seen);
    fs.on_nth("create_new", nth_create_new, move |fs| {
        if let Some(Decoded::Record(rec)) = fs.read_file(LOCK).map(|b| decode(&b)) {
            *keep.lock().unwrap() = Some(rec);
        }
    });
    seen
}

#[test]
fn resume_with_no_prior_starts_a_new_operation_and_says_so() {
    let fs = fake();
    let seen: Arc<Mutex<Option<OperationState>>> = Arc::default();
    let keep = Arc::clone(&seen);
    // `create_new`: the probe's temporary (1), CREATED (2), TRANSFERRING (3), then `a`'s temporary (4).
    fs.on_nth("create_new", 4, move |fs| *keep.lock().unwrap() = Some(manifest(fs, ID)));
    let (r, _) = run_tree(&fs, &resume());
    ok(&r);
    assert_eq!(r.resumed, Some(ResumeNote::StartedNew));
    let m = seen.lock().unwrap().clone().expect("read during the copy");
    assert_eq!(m.format_version, crate::state::FORMAT_VERSION);
    let src = identity_text(fs.metadata(Path::new("/src")).unwrap().identity);
    assert_eq!(m.config.unwrap().roots[0].source_identity, src);
    assert!(!fs.exists("/p/dest/.flux") && !fs.exists(LOCK));
    assert_eq!(run_tree(&fake(), &cfg()).0.resumed, None, "no flag, no note");
}

#[test]
fn a_new_tree_manifest_is_version_3_with_roots_options_and_fingerprint() {
    let fs = fake();
    let s = kept_tree_manifest(&fs);
    assert_eq!(s.format_version, 3);
    let c = s.config.expect("version 3 carries its configuration");
    assert_eq!(c.roots.len(), 1);
    assert_eq!(c.roots[0].destination_prefix, "");
    assert_eq!(c.roots[0].source_root, native_hex(&absolute_lexical(Path::new("/src"))));
    assert_eq!(c.options, Options::of(&opts()));
    assert_eq!(c.configuration_fingerprint, c.options.fingerprint());
}

#[test]
fn resume_adopts_a_prior_in_each_resumable_state_under_its_own_id() {
    for s in [OpState::Created, OpState::Transferring, OpState::Failed] {
        let fs = fake();
        prior3(&fs, 5, s);
        drop(prior_store(&fs, 5));
        // `create_new`: the TRANSFERRING manifest (1), then `a`'s temporary (2).
        let rec = lock_record_during(&fs, 2);
        let (r, _) = run_tree(&fs, &resume());
        let out = ok(&r);
        adopted(&r, 5, Some(0));
        assert_eq!(out.files_copied, 2, "{s:?}");
        let c = calls(&fs);
        assert!(!c.iter().any(|x| {
            x.starts_with(&format!("create_dir(/p/dest/.flux/operations/{ID}.creating)"))
        }));
        let created = c.iter().filter(|x| x.starts_with("create_claim_store(")).count();
        assert_eq!(created, 1, "{s:?}: only the staging made a store: {c:?}");
        let rec = rec.lock().unwrap().clone().expect("the record during the copy");
        assert_eq!(rec.operation_id, id(5));
        assert_eq!(rec.workspace_path, format!("operations/{}", id(5)));
        assert!(!fs.exists("/p/dest/.flux") && !fs.exists(LOCK), "{s:?}");
    }
}

#[test]
fn the_adopted_id_names_the_partials_and_the_old_partial_is_swept() {
    let fs = fake();
    prior3(&fs, 5, OpState::Failed);
    drop(prior_store(&fs, 5));
    let partial = format!("/p/dest/a.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    let (r, _) = run_tree(&fs, &resume());
    ok(&r);
    let c = calls(&fs);
    let removed = at(&c, &format!("remove_file({partial})"));
    let created = at(&c, &format!("create_new({partial})"));
    assert!(removed < created, "{c:?}");
    assert!(!c.iter().any(|x| x.contains(&format!("flux-partial.{ID}"))), "{c:?}");
}

#[test]
fn the_adopted_manifest_goes_transferring_before_the_copy() {
    let fs = fake();
    let before = prior3(&fs, 5, OpState::Failed);
    drop(prior_store(&fs, 5));
    let seen: Arc<Mutex<Option<OperationState>>> = Arc::default();
    let keep = Arc::clone(&seen);
    fs.on_nth("create_new", 2, move |fs| *keep.lock().unwrap() = Some(manifest(fs, &id(5))));
    let (r, _) = run_tree(&fs, &resume());
    ok(&r);
    let m = seen.lock().unwrap().clone().expect("read during the copy");
    assert_eq!((m.state, m.format_version), (OpState::Transferring, 3));
    assert_eq!(m.config, before.config);
}

#[test]
fn two_resumable_priors_are_refused_naming_both_with_and_without_resume() {
    for c in [cfg(), resume()] {
        let fs = fake();
        prior3(&fs, 5, OpState::Failed);
        prior3(&fs, 6, OpState::Created);
        let (r, _) = run_tree(&fs, &c);
        assert_eq!(refused(&r.stop), (LockCode::ResumableOperationExists, false));
        let d = detail(&r.stop);
        for part in [id(5).as_str(), id(6).as_str(), "--resume needs exactly one"] {
            assert!(d.contains(part), "{part}: {d}");
        }
        assert!(!fs.exists(LOCK));
        assert_eq!(manifest(&fs, &id(5)).state, OpState::Failed);
        assert_eq!(manifest(&fs, &id(6)).state, OpState::Created);
    }
}

#[test]
fn a_single_format_3_prior_without_resume_advertises_resume() {
    let fs = fake();
    prior3(&fs, 5, OpState::Failed);
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(refused(&r.stop), (LockCode::ResumableOperationExists, false));
    let d = detail(&r.stop);
    assert!(d.contains("--resume to continue it") && d.contains("--restart"), "{d}");
}

#[test]
fn a_format_1_or_2_prior_is_refused_with_the_older_version_messages() {
    for version in [1_u64, 2] {
        let stage = |fs: &FaultFs| {
            if version == 1 {
                prior(fs, 5, OpState::Failed);
            } else {
                prior(fs, 5, OpState::Failed);
                let v2 = OperationState {
                    state: OpState::Failed,
                    ..OperationState::created_v2(&id(5), Kind::Tree, Path::new("/p/dest"), 1, None)
                };
                write_manifest(fs, &v2);
            }
        };
        let fs = fake();
        stage(&fs);
        let (r, _) = run_tree(&fs, &cfg());
        assert_eq!(refused(&r.stop), (LockCode::ResumableOperationExists, false));
        let d = detail(&r.stop);
        assert!(d.contains("created by an older version") && d.contains("--restart"), "{d}");
        assert!(!d.contains("--resume to continue"), "{d}");
        let fs = fake();
        stage(&fs);
        let before = fs.read_file(format!("/p/dest/.flux/operations/{}/manifest", id(5)));
        let (r, _) = run_tree(&fs, &resume());
        assert_eq!(refused(&r.stop), (LockCode::IncompatibleState, false));
        let d = detail(&r.stop);
        assert!(d.contains(&format!("format {version}")) && d.contains("--restart"), "{d}");
        assert!(!fs.exists(LOCK));
        assert_eq!(
            fs.read_file(format!("/p/dest/.flux/operations/{}/manifest", id(5))),
            before,
            "untouched"
        );
    }
}

#[test]
fn an_incompatible_option_is_refused_naming_it_and_nothing_changes() {
    let fs = fake();
    let mut p = prior3(&fs, 5, OpState::Failed);
    drop(prior_store(&fs, 5));
    let mut options = Options::of(&opts());
    options.existing = "update".to_string();
    p.config = Some(Config::new(p.config.unwrap().roots, options));
    write_manifest(&fs, &p);
    let (r, _) = run_tree(&fs, &resume());
    assert_eq!(refused(&r.stop), (LockCode::IncompatibleState, false));
    assert!(detail(&r.stop).starts_with("existing:"), "{}", detail(&r.stop));
    assert_eq!(manifest(&fs, &id(5)), p);
    assert!(!fs.exists(LOCK));
    assert!(!fs.called("open_claim_store("), "validate comes before the store is opened");
}

#[test]
fn durability_normal_to_strict_is_recorded_on_adoption() {
    let strict = CopyOptions { durability: Durability::Strict, ..opts() };
    let fs = fake();
    prior3(&fs, 5, OpState::Failed);
    drop(prior_store(&fs, 5));
    // The first heartbeat and its retry fail: the copy aborts and the run records FAILED.
    fail_heartbeat_with(&fs, Code::IoError);
    let mut got = Vec::new();
    let c = RunConfig { resume: true, ..beating() };
    let r = tree(&fs, Path::new("/src"), Path::new("/p/dest"), &strict, &c, &mut |f| got.push(f));
    adopted(&r, 5, Some(0));
    let kept = manifest(&fs, &id(5));
    assert_eq!(kept.state, OpState::Failed, "{:?} {:?}", r.stop, r.copy);
    assert_eq!(kept.config.unwrap().options.durability, "strict");
    // The reverse is refused with the spec's text.
    let fs = fake();
    prior3_with(&fs, 5, OpState::Failed, &strict);
    drop(prior_store(&fs, 5));
    let (r, _) = run_tree(&fs, &resume());
    assert_eq!(refused(&r.stop), (LockCode::IncompatibleState, false));
    assert_eq!(
        detail(&r.stop),
        "durability: the operation runs with strict durability; pass --durability strict"
    );
}

#[test]
fn a_mapping_mismatch_is_refused_and_strong_identities_decide() {
    let edit = |fs: &FaultFs, f: &dyn Fn(&mut Root)| {
        let mut p = prior3(fs, 5, OpState::Failed);
        drop(prior_store(fs, 5));
        let mut c = p.config.take().unwrap();
        f(&mut c.roots[0]);
        p.config = Some(c);
        write_manifest(fs, &p);
    };
    // (a) another identity.
    let fs = fake();
    edit(&fs, &|r| r.source_identity = "strong:7:7".to_string());
    let (r, _) = run_tree(&fs, &resume());
    assert_eq!(refused(&r.stop), (LockCode::IncompatibleState, false));
    assert!(detail(&r.stop).contains("source root"), "{}", detail(&r.stop));
    // (b) another spelling of the same Strong identity.
    let fs = fake();
    edit(&fs, &|r| r.source_root = native_hex(Path::new("/elsewhere")));
    let (r, _) = run_tree(&fs, &resume());
    ok(&r);
    adopted(&r, 5, Some(0));
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    // (c) a Weak identity: the path decides, and says so.
    let fs = fake();
    let weak = FileIdentity::Weak(ObjectId { volume: 9, index: 9 });
    fs.set_identity("/src", weak);
    edit(&fs, &|r| r.source_identity = identity_text(weak));
    let (r, _) = run_tree(&fs, &resume());
    adopted(&r, 5, Some(0));
    assert!(
        r.warnings
            .iter()
            .any(|w| matches!(w, RunWarning::ResumeMappingByPath(p) if p == Path::new("/src"))),
        "{:?} {:?}",
        r.warnings,
        r.stop
    );
}

#[test]
fn a_tampered_fingerprint_is_state_corrupt_on_resume() {
    let fs = fake();
    let mut p = prior3(&fs, 5, OpState::Failed);
    drop(prior_store(&fs, 5));
    p.config.as_mut().unwrap().configuration_fingerprint = "0".repeat(64);
    write_manifest(&fs, &p);
    let (r, _) = run_tree(&fs, &resume());
    assert_eq!(refused(&r.stop), (LockCode::StateCorrupt, false));
    assert!(!fs.exists(LOCK));
}

/// The three ways a workspace has no usable `state.db`: absent, zero-length, garbage.
fn break_store(fs: &FaultFs, how: u8) {
    match how {
        0 => {}
        1 => fs.write_file(state_db(5), b""),
        _ => fs.write_file(state_db(5), b"garbage"),
    }
}

#[test]
fn a_created_manifest_without_a_usable_state_db_resumes_with_a_fresh_store() {
    for how in 0..3 {
        let fs = fake();
        prior3(&fs, 5, OpState::Created);
        break_store(&fs, how);
        let (r, _) = run_tree(&fs, &resume());
        ok(&r);
        adopted(&r, 5, Some(0));
        let c = calls(&fs);
        let open = at(&c, &format!("open_claim_store({})", state_db(5)));
        let create = at(&c, &format!("create_claim_store({})", state_db(5)));
        assert!(open < create, "{how}: {c:?}");
        if how > 0 {
            let removed = at(&c, &format!("remove_file({})", state_db(5)));
            assert!(open < removed && removed < create, "{how}: {c:?}");
        }
    }
}

#[test]
fn a_transferring_or_failed_manifest_without_a_usable_state_db_is_state_corrupt() {
    for s in [OpState::Transferring, OpState::Failed] {
        for how in 0..3 {
            let fs = fake();
            let p = prior3(&fs, 5, s);
            break_store(&fs, how);
            let (r, _) = run_tree(&fs, &resume());
            assert_eq!(refused(&r.stop), (LockCode::StateCorrupt, false), "{s:?} {how}");
            assert!(detail(&r.stop).contains("state.db"), "{}", detail(&r.stop));
            assert!(!fs.exists(LOCK));
            assert_eq!(manifest(&fs, &id(5)), p);
            assert!(!fs.called("create_claim_store("));
        }
    }
}

#[test]
fn an_incompatible_state_db_is_refused() {
    let fs = fake();
    prior3(&fs, 5, OpState::Failed);
    drop(prior_store(&fs, 5));
    fs.set_claim_store_format(state_db(5), 2);
    let (r, _) = run_tree(&fs, &resume());
    assert_eq!(refused(&r.stop), (LockCode::IncompatibleState, false));
    assert!(detail(&r.stop).contains("state.db"), "{}", detail(&r.stop));
    assert!(!fs.exists(LOCK));
}

#[test]
fn a_state_db_that_cannot_be_opened_fails_the_run_at_the_state_step() {
    let fs = fake();
    prior3(&fs, 5, OpState::Failed);
    drop(prior_store(&fs, 5));
    fs.fail("open_claim_store", Code::PermissionDenied);
    let (r, _) = run_tree(&fs, &resume());
    let (step, path) = failed_at(&r.stop);
    assert_eq!(step, RunStep::State);
    assert!(path.ends_with("state.db"), "{path}");
    assert!(!fs.exists(LOCK));
}

#[test]
fn the_claim_count_is_reported_on_adoption() {
    let fs = fake();
    prior3(&fs, 5, OpState::Failed);
    let mut store = prior_store(&fs, 5);
    for name in ["x", "y"] {
        let key = ClaimKey::new(ObjectId { volume: 1, index: 1 }, OsStr::new(name));
        let rec = ClaimRecord {
            target: FluxPathKey(name.as_bytes().to_vec()),
            status: ClaimStatus::Created,
        };
        store.insert_if_absent(&key, &rec).unwrap();
    }
    store.flush().unwrap();
    drop(store);
    let (r, _) = run_tree(&fs, &resume());
    adopted(&r, 5, Some(2));
}

#[test]
fn a_resume_that_fails_before_transferring_can_be_resumed_again() {
    let fs = fake();
    prior3(&fs, 5, OpState::Created);
    drop(prior_store(&fs, 5));
    // The TRANSFERRING write is the first `rename_replace`; the failure path's FAILED write is the second, and just
    // before it the manifest on disk must still be the prior's CREATED.
    fs.fail_nth("rename_replace", 1, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let seen: Arc<Mutex<Option<OpState>>> = Arc::default();
    let keep = Arc::clone(&seen);
    fs.on_nth("rename_replace", 2, move |fs| {
        *keep.lock().unwrap() = Some(manifest(fs, &id(5)).state)
    });
    let (r, _) = run_tree(&fs, &resume());
    assert_eq!(failed_at(&r.stop).0, RunStep::State);
    assert_eq!(
        *seen.lock().unwrap(),
        Some(OpState::Created),
        "TRANSFERRING never reached the disk"
    );
    let c = calls(&fs);
    assert!(
        at(&c, "open_claim_store(") < at(&c, "rename_replace(")
            && !c.iter().any(|x| x.contains("flux-partial")),
        "{c:?}"
    );
    assert_eq!(manifest(&fs, &id(5)).state, OpState::Failed, "resumable");
    let (r, _) = run_tree(&fs, &resume());
    ok(&r);
    adopted(&r, 5, Some(0));
}

fn file_prior3(fs: &FaultFs, n: u8, source_identity: Option<&str>) -> OperationState {
    let src = identity_text(fs.metadata(Path::new("/src/a")).unwrap().identity);
    let shown = source_identity.map_or(src, str::to_string);
    let fields = crate::state::FileFields {
        artifact_type: crate::state::ARTIFACT_STATE.to_string(),
        attempt_id: id(7),
        artifact_generation: 1,
        source_identity: shown.clone(),
        target_identity: Some("strong:3:3".to_string()),
        target_path_key: crate::lock::site::hex(b"t"),
        owner_instance_id: id(8),
        boot_session_id: "boot".to_string(),
        creation_wall_time: "1".to_string(),
        last_heartbeat_wall_time: "1".to_string(),
    };
    let config = Config::new(
        vec![Root {
            source_root: native_hex(&absolute_lexical(Path::new("/src/a"))),
            source_identity: shown,
            destination_prefix: native_hex(Path::new("t")),
        }],
        Options::of(&opts()),
    );
    let state = OperationState {
        state: OpState::Failed,
        ..OperationState::created(&id(n), Kind::File, Path::new("/p/t"), 1, Some(fields), config)
    };
    fs.write_file(record_path(&id(n)), &state.encode());
    state
}

#[test]
fn a_single_file_resume_adopts_the_record_bumping_its_generation() {
    let fs = fake();
    file_prior3(&fs, 5, None);
    let partial = format!("/p/t.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    let seen: Arc<Mutex<Option<OperationState>>> = Arc::default();
    let keep = Arc::clone(&seen);
    let rec: Arc<Mutex<Option<Decoded>>> = Arc::new(Mutex::new(None));
    let keep_rec = Arc::clone(&rec);
    // `create_new`: the TRANSFERRING record (1), then the copy's temporary (2).
    fs.on_nth("create_new", 2, move |fs| {
        *keep.lock().unwrap() = Some(file_record(fs, &id(5)));
        *keep_rec.lock().unwrap() = fs.read_file(T_LOCK).map(|b| decode(&b));
    });
    let r = run_file(&fs, &resume());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?} {:?}", r.stop, r.copy);
    assert_eq!(
        r.resumed,
        Some(ResumeNote::Adopted { operation_id: id(5), claims: None }),
        "no claim store for a single file"
    );
    let s = seen.lock().unwrap().clone().expect("read during the copy");
    let f = s.file.expect("a single-file record");
    assert_eq!((s.state, f.artifact_generation), (OpState::Transferring, 2));
    assert!(crate::ids::is_id(&f.attempt_id) && f.attempt_id != id(7), "{}", f.attempt_id);
    assert_eq!(f.owner_instance_id, id(0xee));
    assert_eq!(f.boot_session_id, "test-boot", "the run's session, not the prior's");
    assert_ne!(f.last_heartbeat_wall_time, "1", "refreshed");
    assert_eq!(f.last_heartbeat_wall_time, lock_time(&rec), "the run's one reading");
    assert_eq!(f.creation_wall_time, "1");
    let src = identity_text(fs.metadata(Path::new("/src/a")).unwrap().identity);
    assert_eq!(f.source_identity, src, "kept");
    assert_eq!(f.target_identity.as_deref(), Some("strong:3:3"), "kept");
    assert_eq!(f.target_path_key, crate::lock::site::hex(b"t"), "kept");
    let Some(Decoded::Record(lock)) = rec.lock().unwrap().clone() else {
        panic!("the lock record during the copy")
    };
    assert_eq!(lock.operation_id, id(5));
    let c = calls(&fs);
    let removed = at(&c, &format!("remove_file({partial})"));
    let created = at(&c, &format!("create_new({partial})"));
    assert!(removed < created, "{c:?}");
    assert_eq!(fs.read_file("/p/t").as_deref(), Some(&b"A"[..]));
    assert!(!fs.exists(record_path(&id(5))) && !fs.exists(T_LOCK));
}

fn lock_time(rec: &Arc<Mutex<Option<Decoded>>>) -> String {
    match rec.lock().unwrap().clone() {
        Some(Decoded::Record(r)) => r.last_heartbeat_wall_time.to_string(),
        other => panic!("the lock record during the copy: {}", other.is_some()),
    }
}

#[test]
fn a_single_file_record_for_another_source_is_refused() {
    let fs = fake();
    let p = file_prior3(&fs, 5, Some("strong:7:7"));
    let r = run_file(&fs, &resume());
    assert_eq!(refused(&r.stop), (LockCode::IncompatibleState, false));
    assert!(!fs.exists(T_LOCK));
    assert_eq!(file_record(&fs, &id(5)), p, "untouched");
}

#[test]
fn restart_still_supersedes_everything_and_ignores_resume_semantics() {
    let fs = fake();
    prior3(&fs, 5, OpState::Failed);
    prior3(&fs, 6, OpState::Created);
    let (r, _) = run_tree(&fs, &restart());
    ok(&r);
    assert_eq!(r.resumed, None);
    let c = calls(&fs);
    for n in [5, 6] {
        assert!(
            c.iter().any(|x| x.starts_with("rename_no_replace(")
                && x.contains(&format!("operations/{} ", id(n)))),
            "{n} retired: {c:?}"
        );
    }
    assert!(!fs.exists("/p/dest/.flux") && !fs.exists(LOCK));
}

#[test]
fn a_refused_unchanged_copy_after_adoption_keeps_the_adopted_workspace() {
    let fs = FaultFs::new();
    for d in ["/src", "/src/sub", "/p", "/p/dest"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/src/sub/b", b"BB");
    let p = prior3(&fs, 5, OpState::Failed);
    let mut store = prior_store(&fs, 5);
    let key = ClaimKey::new(ObjectId { volume: 1, index: 1 }, OsStr::new("x"));
    let rec = flux_fs::ClaimRecord {
        target: flux_fs::FluxPathKey(b"x".to_vec()),
        status: flux_fs::ClaimStatus::Created,
    };
    store.insert_if_absent(&key, &rec).unwrap();
    store.flush().unwrap();
    drop(store);
    // `sub` IS DEST by identity: the copy refuses it before creating anything.
    fs.set_identity("/src/sub", fs.metadata(Path::new("/p/dest")).unwrap().identity);
    let (r, _) = run_tree(&fs, &resume());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    let Some(Err(a)) = &r.copy else { panic!("the copy refused: {:?}", r.copy) };
    assert!(a.refused_unchanged(), "{a:?}");
    adopted(&r, 5, Some(1));
    assert!(fs.exists(state_db(5)), "the recorded progress survives");
    let m = manifest(&fs, &id(5));
    assert_eq!((m.state, m.format_version, m.operation_id), (OpState::Failed, 3, id(5)));
    assert_eq!(m.config, p.config);
    assert!(!fs.exists(LOCK), "released");
}

#[test]
fn a_refused_unchanged_single_file_after_adoption_keeps_the_record() {
    let fs = fake();
    file_prior3(&fs, 5, None);
    fs.on_nth("create_lock", 1, |fs| fs.create_dir(Path::new("/p/t")).unwrap());
    let r = run_file(&fs, &resume());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert!(
        matches!(&r.copy, Some(Err(e)) if e.code() == Code::SafetyRejected && e.leftover.is_none()),
        "{:?}",
        r.copy
    );
    let s = file_record(&fs, &id(5));
    assert_eq!(
        (s.state, s.format_version, s.operation_id.as_str()),
        (OpState::Failed, 3, id(5).as_str())
    );
    assert!(!fs.exists(T_LOCK), "released");
}
