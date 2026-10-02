//! The run on the fake filesystem (cut 7a Part 3b-1). `/src` is the source; `/p` is DEST's parent and holds the lock.

use super::*;
use crate::copy::CopyStep;
use crate::fault_fs::FaultFs;
use crate::lock::LockCode;
use crate::lock::record::{Decoded, decode};
use crate::lock::test_support::{dead_lock, live_lock, record};
use crate::state::{Kind, OperationState, UNREADABLE, decode as decode_state};
use flux_fs::{
    DestinationRoot, Durability, FileSystem, LockCapability, OperationId, Preserve, Publish, Safety,
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
    // Renames without replacing: the workspace into place (1), `a` and `sub/b` published (2, 3), the retire (4).
    fs.on_nth("rename_no_replace", 4, move |fs| *keep.lock().unwrap() = fs.read_file(LOCK));
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    let c = calls(&fs);
    let renames: Vec<&String> = c.iter().filter(|x| x.starts_with("rename_no_replace(")).collect();
    assert!(renames[3].contains(&format!("{ID}.removing")), "the 4th is the retire: {renames:?}");
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
    assert!(!fs.exists(LOCK), "the lock this run created is gone");
    assert_eq!(manifest(&fs, &id(5)).state, OpState::Failed, "untouched");
    assert!(!fs.exists(format!("/p/dest/.flux/operations/{ID}")));
}

#[test]
fn ownership_lost_mid_copy_stops_and_leaves_the_state_and_the_lock() {
    let fs = fake();
    // `create_new`: this run's CREATED (1) and TRANSFERRING (2) manifests, then `a`'s temporary (3).
    fs.on_nth("create_new", 3, |fs| fs.write_file(LOCK, b"another run's bytes"));
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
    // `a`'s copy fails while streaming (its temporary is the 3rd `create_new`), and removing that temporary fails
    // too: the 4th `remove_file` (the two state writes clear their temporaries, then `a`'s step-1 sweep).
    fs.on_nth("create_new", 3, |fs| fs.fail_write(std::io::Error::other("injected write")));
    fs.fail_nth("remove_file", 4, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
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
    // `create_new`: the CREATED manifest (1), the takeover (2), TRANSFERRING (3), then `a`'s temporary (4).
    let path = format!("/p/dest/.flux/operations/{ID}/manifest");
    fs.on_nth("create_new", 4, move |fs| *keep.lock().unwrap() = fs.read_file(&path));
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
    // The retire is the 4th rename without replacing (the workspace, then the two publishes); just before it, the
    // lock is taken over. The transfer is complete and durable by then.
    fs.on_nth("rename_no_replace", 4, |fs| fs.write_file(LOCK, b"another run's bytes"));
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
    // `remove_file`: this run's CREATED and the prior's ABANDONED state writes clear their temporaries (1, 2); then
    // the partial (3).
    fs.fail_nth("remove_file", 3, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &restart());
    ok(&r);
    let c = calls(&fs);
    let removals: Vec<&String> = c.iter().filter(|x| x.starts_with("remove_file(")).collect();
    assert!(
        removals[2].contains("old.flux-partial"),
        "the 3rd removal is the partial: {removals:?}"
    );
    assert!(fs.exists(&partial));
    let kept = manifest(&fs, &id(5));
    assert_eq!((kept.state, kept.superseded_by.as_deref()), (OpState::Abandoned, Some(ID)));
    assert!(
        r.warnings.iter().any(|w| matches!(w, RunWarning::PartialKept { .. })),
        "{:?}",
        r.warnings
    );
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
    // `rename_no_replace`: this run's workspace into place (1), then the prior's retire (2), which fails.
    fs.fail_nth(
        "rename_no_replace",
        2,
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
    // A tree's guarded mutations: `a`'s sweep, temporary and publish; `sub`'s creation; `sub/b`'s three. A single
    // file's: its sweep, temporary and publish.
    for (tree_run, expected) in [(true, 7), (false, 3)] {
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
    assert_eq!(f.artifact_type, "file");
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
/// the workspace (1), `a`'s and `sub/b`'s publishes (2, 3), then the retire (4), which fails.
fn kept_tree_manifest(fs: &FaultFs) -> OperationState {
    fs.fail_nth(
        "rename_no_replace",
        4,
        Code::PermissionDenied,
        std::io::ErrorKind::PermissionDenied,
    );
    let (r, _) = run_tree(fs, &cfg());
    ok(&r);
    manifest(fs, ID)
}

#[test]
fn a_tree_manifest_is_version_2_with_cleanup_keys_and_no_file_fields() {
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
fn a_clean_trees_completed_manifest_says_cleanup_pending_with_an_empty_list() {
    let fs = fake();
    let s = kept_tree_manifest(&fs);
    assert_eq!(s.state, OpState::Completed);
    let c = s.cleanup.expect("version 2");
    assert!(c.cleanup_pending && c.cleanup_pending_artifacts.is_empty(), "{c:?}");
}
