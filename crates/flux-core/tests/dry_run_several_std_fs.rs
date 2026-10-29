// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Cut 18: a dry run over several sources on the real filesystem. A before/after snapshot of DEST's parent (plus the
//! bytes of every file) is the oracle for zero mutation; the live run on a second identical tree is the oracle for the
//! counters; a lock held on a separate handle is the oracle for the dry run never touching the lock. The fold-probe
//! refusal needs a case-insensitive volume and is skipped elsewhere (the fake's T16 guards it on Linux).

use flux_core::run::{self, RunConfig, SourceKind, SourceSpec};
use flux_core::{TreeFailure, TreeOutcome};
use flux_fs::{
    CopyOptions, DestinationRoot, DirHandle, Durability, ExistingPolicy, Links, LockFile,
    OperationId, Preserve, Publish, Safety,
};
use flux_platform::StdFileSystem;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tempfile::TempDir;

fn opts() -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability: Durability::Normal,
        publish: Publish::Replace,
        safety: Safety::Default,
        operation_id: OperationId::new("t"),
        existing: ExistingPolicy::Overwrite,
        links: Links::Copy,
        hardlinks: flux_fs::Hardlinks::Auto,
        cross_filesystems: false,
        verify: flux_fs::VerifyLevel::None,
        hash: flux_fs::HashAlgorithm::Blake3,
        selection: flux_fs::Selection::default(),
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
        progress: None,
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
    /// The file's bytes; empty for anything else.
    bytes: Vec<u8>,
}

/// Every entry under `root`, keyed by its `/`-joined relative path, never following a link. An absent root is empty.
fn snapshot(root: &Path) -> BTreeMap<String, Entry> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Entry>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let key = format!("{prefix}{}", e.file_name().to_string_lossy());
            let md = std::fs::symlink_metadata(e.path()).unwrap();
            let ft = md.file_type();
            let (kind, link, bytes) = if ft.is_symlink() {
                (Kind::Symlink, Some(std::fs::read_link(e.path()).unwrap()), Vec::new())
            } else if ft.is_dir() {
                (Kind::Dir, None, Vec::new())
            } else {
                (Kind::File, None, std::fs::read(e.path()).unwrap())
            };
            out.insert(
                key.clone(),
                Entry {
                    kind: kind.clone(),
                    len: md.len(),
                    modified: md.modified().ok(),
                    link,
                    bytes,
                },
            );
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

fn spec(src: PathBuf, kind: SourceKind) -> SourceSpec {
    let name = src.file_name().unwrap().to_os_string();
    SourceSpec { src, name, kind }
}

/// Two folders and a file under `base`: `alpha` (with a subfolder), `beta`, and `gamma.txt`.
fn build_sources(base: &Path) -> Vec<SourceSpec> {
    let (alpha, beta) = (base.join("alpha"), base.join("beta"));
    std::fs::create_dir_all(alpha.join("sub")).unwrap();
    std::fs::create_dir_all(&beta).unwrap();
    std::fs::write(alpha.join("a.txt"), b"A").unwrap();
    std::fs::write(alpha.join("sub").join("b.txt"), b"BB").unwrap();
    std::fs::write(beta.join("c.txt"), b"CCC").unwrap();
    let gamma = base.join("gamma.txt");
    std::fs::write(&gamma, b"GGGG").unwrap();
    vec![
        spec(alpha, SourceKind::Folder),
        spec(beta, SourceKind::Folder),
        spec(gamma, SourceKind::File),
    ]
}

fn counters(o: &TreeOutcome) -> [u64; 12] {
    [
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
    ]
}

fn run_trees(specs: Vec<SourceSpec>, dst: &Path, dry: bool) -> run::MultiRun {
    run::trees(&StdFileSystem, specs, dst, &opts(), &config(dry), &mut |_: TreeFailure| {})
}

#[test]
fn a_several_source_dry_run_changes_nothing_and_predicts_the_real_copy() {
    let d = TempDir::new().unwrap();
    let dst = d.path().join("dst");
    let specs = build_sources(&d.path().join("in"));
    let whole_before = snapshot(d.path());

    let dry = run_trees(specs, &dst, true);

    assert!(dry.run.stop.is_none(), "{:?}", dry.run.stop);
    assert!(dry.run.dry.is_some(), "a dry run carries its note");
    assert_eq!(snapshot(d.path()), whole_before, "a dry run changed the tree around DEST");
    assert!(!dst.exists(), "the dry run created DEST");
    let dry_counters = counters(&dry.run.copy.expect("a copy result").expect("Ok"));

    // The oracle: the live run on a second, identical tree.
    let e = TempDir::new().unwrap();
    let live_dst = e.path().join("dst");
    let live = run_trees(build_sources(&e.path().join("in")), &live_dst, false);
    assert!(live.run.stop.is_none(), "{:?}", live.run.stop);
    let live_counters = counters(&live.run.copy.expect("a copy result").expect("Ok"));
    assert_eq!(dry_counters, live_counters, "the dry and live counters disagree");
    assert_eq!(dry_counters[1], 4, "files_copied");
    assert!(live_dst.join("alpha").join("sub").join("b.txt").exists());
}

#[test]
fn a_several_source_dry_run_plans_while_another_handle_holds_the_destinations_lock() {
    let d = TempDir::new().unwrap();
    let dst = d.path().join("dst");
    let specs = build_sources(&d.path().join("in"));

    let parent = StdFileSystem.destination_root(d.path()).unwrap();
    let lock = parent.create_lock(OsStr::new("dst.flux-lock")).unwrap();
    assert!(lock.try_lock().unwrap());
    // A live owner's lock carries a valid record (an empty file is a holder inside its first write).
    let now = flux_core::state::wall_time_ns();
    let record = flux_core::lock::record::LockRecord {
        complete_lock_key: "test-lock-key".into(),
        operation_id: flux_core::ids::new_id(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: "test-boot".into(),
        target_path_key: "test-target-key".into(),
        workspace_path: "none".into(),
        creation_wall_time: now,
        last_heartbeat_wall_time: now,
    };
    lock.write_at_start(&record.encode()).unwrap();
    lock.sync_all().unwrap();
    let before = snapshot(d.path());

    let r = run_trees(specs, &dst, true);

    assert!(r.run.stop.is_none(), "{:?}", r.run.stop);
    let out = r.run.copy.expect("a copy result").expect("Ok");
    assert_eq!(out.files_copied, 4, "files_copied");
    let note = r.run.dry.expect("a dry run carries its note");
    assert!(note.owner_liveness && !note.lock_takeover, "{note:?}");
    assert!(!dst.exists(), "the dry run created DEST");
    assert_eq!(snapshot(d.path()), before, "a dry run changed the tree around DEST");
    assert_eq!(std::fs::read(d.path().join("dst.flux-lock")).unwrap(), record.encode());
    let second = parent.open_lock(OsStr::new("dst.flux-lock")).unwrap();
    assert!(!second.try_lock().unwrap(), "the holder still holds the lock");
    drop(lock);
}

/// True when the volume under `dir` finds a file by the upper-case twin of its name.
fn folds_case(dir: &Path) -> bool {
    let probe = dir.join("fluxcaseprobe");
    std::fs::write(&probe, b"p").unwrap();
    let folded = dir.join("FLUXCASEPROBE").exists();
    std::fs::remove_file(&probe).unwrap();
    folded
}

#[test]
fn two_names_that_differ_only_by_case_are_refused_by_a_dry_run_on_a_folding_volume() {
    let d = TempDir::new().unwrap();
    if !folds_case(d.path()) {
        if cfg!(any(target_os = "macos", windows)) {
            panic!("expected a case-folding temporary volume on this OS");
        }
        eprintln!(
            "skipped: the temporary volume is case-sensitive (the fake's T16 guards the probe)"
        );
        return;
    }
    // Different parents: two siblings named `Data` and `data` cannot coexist on exactly this volume.
    let (x, y) = (d.path().join("x").join("Data"), d.path().join("y").join("data"));
    std::fs::create_dir_all(&x).unwrap();
    std::fs::create_dir_all(&y).unwrap();
    std::fs::write(x.join("a.txt"), b"A").unwrap();
    std::fs::write(y.join("b.txt"), b"B").unwrap();
    let dst = d.path().join("dst");
    let whole_before = snapshot(d.path());

    let r = run_trees(
        vec![
            SourceSpec { src: x, name: OsString::from("Data"), kind: SourceKind::Folder },
            SourceSpec { src: y, name: OsString::from("data"), kind: SourceKind::Folder },
        ],
        &dst,
        true,
    );

    match r.run.stop {
        Some(run::RunError::Refused { refusal, .. }) => {
            assert_eq!(refusal.code.as_str(), "DESTINATION_NAMESPACE_COLLISION", "{refusal:?}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(snapshot(d.path()), whole_before, "a dry run changed the tree");
    assert!(!dst.exists(), "the dry run created DEST");
}
