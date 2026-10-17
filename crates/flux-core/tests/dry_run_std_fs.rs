//! Cut 11a: a dry run on the real filesystem. The live run on identical, untouched trees is the oracle for the paired
//! test; a before/after snapshot is the oracle for zero mutation; a lock held on a separate handle is the oracle for
//! the dry run never touching the lock.

use flux_core::dry_run::{PlannedAction, PlannedEntry};
use flux_core::run::{self, RunConfig};
use flux_core::{TreeFailure, TreeFailureCause, TreeOutcome};
use flux_fs::{
    CopyOptions, DestinationRoot, DirHandle, Durability, ExistingPolicy, Links, LockFile,
    OperationId, Preserve, Publish, Safety,
};
use flux_platform::StdFileSystem;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn opts(existing: ExistingPolicy, links: Links, safety: Safety) -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability: Durability::Normal,
        publish: Publish::Replace,
        safety,
        operation_id: OperationId::new("t"),
        existing,
        links,
        cross_filesystems: false,
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
            out.insert(
                key.clone(),
                Entry { kind: kind.clone(), len: md.len(), modified: md.modified().ok(), link },
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

fn has_flux_name(key: &str) -> bool {
    key.split('/').any(|c| c.starts_with(".flux"))
}

/// Flux's own workspace under DEST: `.flux` itself and its reserved control directories. A source `.flux/other/x`
/// is data (P11) and is NOT control.
fn is_control(key: &str) -> bool {
    let mut parts = key.split('/');
    parts.next() == Some(".flux")
        && parts.next().is_none_or(|c| matches!(c, "operations" | "standalone" | "atomic"))
}

fn write_aged(path: &Path, bytes: &[u8], modified: SystemTime) {
    std::fs::write(path, bytes).unwrap();
    std::fs::File::options().write(true).open(path).unwrap().set_modified(modified).unwrap();
}

/// The three existing-file shapes `Update` distinguishes, in `dir`; the source copy is `now`-stamped.
fn trio(src: &Path, dst: &Path) {
    let now = SystemTime::now();
    let hour = Duration::from_secs(3600);
    write_aged(&src.join("older_same_len.txt"), b"NEW1", now);
    write_aged(&dst.join("older_same_len.txt"), b"OLD1", now - hour);
    write_aged(&src.join("newer.txt"), b"NEW2", now);
    write_aged(&dst.join("newer.txt"), b"OLD2", now + hour);
    write_aged(&src.join("diff_len.txt"), b"NEW3-longer", now);
    write_aged(&dst.join("diff_len.txt"), b"OLD3", now + hour);
}

struct Case {
    name: &'static str,
    existing: ExistingPolicy,
    links: Links,
    safety: Safety,
    /// Builds the trees under `src` (exists) and `dst` (does not exist yet).
    build: fn(&Path, &Path),
}

fn mk_dst(_src: &Path, dst: &Path) {
    std::fs::create_dir(dst).unwrap();
}

fn plain_source(src: &Path) {
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), b"A").unwrap();
    std::fs::write(src.join("sub").join("b.txt"), b"BB").unwrap();
}

fn flux_dir_source(src: &Path) {
    std::fs::create_dir_all(src.join(".flux").join("operations")).unwrap();
    std::fs::write(src.join(".flux").join("operations").join("op.json"), b"{}").unwrap();
    std::fs::create_dir_all(src.join(".flux").join("other")).unwrap();
    std::fs::write(src.join(".flux").join("other").join("x.txt"), b"X").unwrap();
    std::fs::write(src.join("a.txt"), b"A").unwrap();
}

fn flux_file_source(src: &Path) {
    std::fs::create_dir_all(src).unwrap();
    std::fs::write(src.join(".flux"), b"a file named like the control directory").unwrap();
    std::fs::write(src.join("a.txt"), b"A").unwrap();
}

fn corpus() -> Vec<Case> {
    let mut v = vec![
        Case {
            name: "fresh dest",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, _dst| plain_source(src),
        },
        Case {
            name: "existing dest, overwrite",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, dst| {
                mk_dst(src, dst);
                plain_source(src);
                trio(src, dst);
            },
        },
        Case {
            name: "existing dest, skip-existing",
            existing: ExistingPolicy::SkipExisting,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, dst| {
                mk_dst(src, dst);
                plain_source(src);
                trio(src, dst);
            },
        },
        Case {
            name: "existing dest, update",
            existing: ExistingPolicy::Update,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, dst| {
                mk_dst(src, dst);
                plain_source(src);
                trio(src, dst);
            },
        },
        Case {
            name: "file at a directory name",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, dst| {
                mk_dst(src, dst);
                std::fs::create_dir_all(src.join("d")).unwrap();
                std::fs::write(src.join("d").join("x.txt"), b"X").unwrap();
                std::fs::write(src.join("ok.txt"), b"OK").unwrap();
                std::fs::write(dst.join("d"), b"i am a file").unwrap();
            },
        },
        Case {
            name: "nested absent subtree",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, dst| {
                mk_dst(src, dst);
                std::fs::create_dir_all(src.join("a").join("b").join("c")).unwrap();
                std::fs::write(src.join("a").join("b").join("c").join("f.txt"), b"F").unwrap();
                std::fs::write(src.join("a").join("top.txt"), b"T").unwrap();
            },
        },
        Case {
            // P11: a top-level `.flux` source directory merges as live does, so DEST needs no pre-created `.flux`.
            name: "reserved .flux/operations source path",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, dst| {
                mk_dst(src, dst);
                std::fs::create_dir_all(src.join(".flux").join("operations")).unwrap();
                std::fs::write(src.join(".flux").join("operations").join("op.json"), b"{}")
                    .unwrap();
                std::fs::write(src.join("a.txt"), b"A").unwrap();
            },
        },
        Case {
            name: "top-level .flux directory, existing dest",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, dst| {
                mk_dst(src, dst);
                flux_dir_source(src);
            },
        },
        Case {
            name: "top-level .flux directory, fresh dest",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, _dst| flux_dir_source(src),
        },
        Case {
            name: "top-level .flux file, existing dest",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, dst| {
                mk_dst(src, dst);
                flux_file_source(src);
            },
        },
        Case {
            name: "top-level .flux file, fresh dest",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, _dst| flux_file_source(src),
        },
        Case {
            // P12: the temporary's name (the name plus a suffix) is over the limit; live fails `create_new`.
            name: "a name whose temporary is over the name limit",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, dst| {
                mk_dst(src, dst);
                std::fs::create_dir_all(src).unwrap();
                std::fs::write(src.join("n".repeat(240)), b"N").unwrap();
                std::fs::write(src.join("ok.txt"), b"OK").unwrap();
            },
        },
        Case {
            // The destination name is a hard link to the source file: the identity gate refuses it in both modes.
            name: "hardlink alias of the source at the destination name",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, dst| {
                mk_dst(src, dst);
                plain_source(src);
                std::fs::hard_link(src.join("a.txt"), dst.join("a.txt")).unwrap();
            },
        },
    ];
    // ext4's identities are Strong, so `--safety strict` passes in both modes. Other filesystems may refuse the root.
    #[cfg(target_os = "linux")]
    v.push(Case {
        name: "strict safety on a strong-identity filesystem, existing dest",
        existing: ExistingPolicy::Overwrite,
        links: Links::Copy,
        safety: Safety::Strict,
        build: |src, dst| {
            mk_dst(src, dst);
            plain_source(src);
            trio(src, dst);
        },
    });
    #[cfg(unix)]
    {
        fn link_source(src: &Path) {
            std::fs::create_dir_all(src).unwrap();
            std::fs::write(src.join("real.txt"), b"R").unwrap();
            std::os::unix::fs::symlink("real.txt", src.join("link")).unwrap();
        }
        v.push(Case {
            name: "symlink, links=copy, fresh dest",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, _dst| link_source(src),
        });
        v.push(Case {
            name: "symlink over an existing file, links=copy, overwrite",
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            safety: Safety::Default,
            build: |src, dst| {
                mk_dst(src, dst);
                link_source(src);
                std::fs::write(dst.join("link"), b"a regular file").unwrap();
            },
        });
        v.push(Case {
            name: "symlink, links=skip",
            existing: ExistingPolicy::Overwrite,
            links: Links::Skip,
            safety: Safety::Default,
            build: |src, _dst| link_source(src),
        });
    }
    v
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

fn cause_code(c: &TreeFailureCause) -> String {
    match c {
        TreeFailureCause::Walk(e)
        | TreeFailureCause::CreateDir(e)
        | TreeFailureCause::ClaimNotRecorded(e) => format!("{:?}", e.code),
        TreeFailureCause::Copy(e) => format!("{:?}", e.code()),
        _ => String::new(),
    }
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

/// The comparable result of one tree run.
#[derive(Debug, PartialEq, Eq)]
struct Shape {
    /// `Ok`, or `Err` with the error code and `refused_unchanged()`.
    result: String,
    counters: Counters,
    records: Vec<(PathBuf, &'static str, String)>,
    stopped: bool,
}

fn run_tree(
    src: &Path,
    dst: &Path,
    o: &CopyOptions,
    dry: bool,
) -> (Shape, Vec<(PathBuf, PlannedEntry)>, Option<flux_core::dry_run::DryRun>) {
    let mut records = Vec::new();
    let mut planned = Vec::new();
    let r =
        run::tree(&StdFileSystem, src, dst, o, &config(dry), &mut |f: TreeFailure| match f.cause {
            TreeFailureCause::Planned(p) => planned.push((f.path, p)),
            ref c => records.push((f.path.clone(), cause_name(c), cause_code(c))),
        });
    let stopped = r.stop.is_some();
    let (result, counters_) = match r.copy {
        Some(Ok(out)) => ("Ok".to_string(), counters(&out)),
        Some(Err(abort)) => (
            format!(
                "Err({:?}, refused_unchanged={})",
                abort.error.code(),
                abort.refused_unchanged()
            ),
            counters(&abort.outcome),
        ),
        None => ("None".to_string(), Counters::default()),
    };
    records.sort();
    (Shape { result, counters: counters_, records, stopped }, planned, r.dry)
}

fn slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[test]
fn a_dry_run_leaves_the_destination_byte_for_byte_and_the_real_run_then_agrees() {
    for case in corpus() {
        let d = TempDir::new().unwrap();
        let (src, dst) = (d.path().join("src"), d.path().join("dst"));
        std::fs::create_dir(&src).unwrap();
        (case.build)(&src, &dst);
        let o = opts(case.existing, case.links, case.safety);
        let name = case.name;

        let before = snapshot(&dst);
        let whole_before = snapshot(d.path());
        let flux_before: Vec<_> =
            whole_before.keys().filter(|k| has_flux_name(k)).cloned().collect();

        let (dry_shape, dry_planned, dry_note) = run_tree(&src, &dst, &o, true);

        assert!(dry_note.is_some(), "{name}: a dry run carries its note");
        assert!(!dry_planned.is_empty(), "{name}: a dry run of a non-empty source plans something");
        assert_eq!(snapshot(&dst), before, "{name}: a dry run changed DEST");
        assert_eq!(
            snapshot(d.path()),
            whole_before,
            "{name}: a dry run changed the tree around DEST"
        );
        let flux_after: Vec<_> =
            snapshot(d.path()).keys().filter(|k| has_flux_name(k)).cloned().collect();
        assert_eq!(flux_after, flux_before, "{name}: a dry run created a .flux* entry");

        let (live_shape, live_planned, live_note) = run_tree(&src, &dst, &o, false);
        assert!(live_note.is_none() && live_planned.is_empty(), "{name}: a live run plans nothing");
        assert_eq!(dry_shape, live_shape, "{name}: the dry and live runs disagree");

        let after = snapshot(&dst);
        let changed: std::collections::BTreeSet<String> = after
            .iter()
            .filter(|(k, _)| !is_control(k))
            .filter(|(k, e)| match before.get(*k) {
                None => true,
                // A directory's own mtime moves when a child is added; that is not the directory being planned.
                Some(b) if b.kind == Kind::Dir && e.kind == Kind::Dir => false,
                Some(b) => b != *e,
            })
            .map(|(k, _)| k.clone())
            .collect();
        let planned: std::collections::BTreeSet<String> = dry_planned
            .iter()
            .filter(|(_, p)| {
                matches!(
                    p.action,
                    PlannedAction::CreateDir
                        | PlannedAction::Copy
                        | PlannedAction::Overwrite
                        | PlannedAction::CreateSymlink
                )
            })
            .map(|(p, _)| slash(p))
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(planned, changed, "{name}: planned paths differ from what the live run changed");
    }
}

#[test]
fn a_dry_run_plans_while_another_handle_holds_the_destinations_lock() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("src"), d.path().join("dst"));
    plain_source(&src);

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

    let (shape, planned, note) =
        run_tree(&src, &dst, &opts(ExistingPolicy::Overwrite, Links::Copy, Safety::Default), true);

    assert_eq!(shape.result, "Ok", "{shape:?}");
    assert_eq!(shape.counters.1, 2, "files_copied");
    assert!(note.is_some() && !planned.is_empty());
    let note = note.unwrap();
    assert!(note.owner_liveness && !note.lock_takeover, "{note:?}");
    assert!(!dst.exists(), "the dry run created DEST");
    assert!(d.path().join("dst.flux-lock").exists(), "the lock file is still there");
    assert_eq!(std::fs::read(d.path().join("dst.flux-lock")).unwrap(), record.encode());
    let second = parent.open_lock(OsStr::new("dst.flux-lock")).unwrap();
    assert!(!second.try_lock().unwrap(), "the holder still holds the lock");
    drop(lock);
}

#[test]
fn a_dry_single_file_run_into_a_folder_target_plans_and_creates_nothing() {
    let d = TempDir::new().unwrap();
    let src = d.path().join("a.txt");
    std::fs::write(&src, b"A").unwrap();
    let out = d.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let whole_before = snapshot(d.path());
    let before = snapshot(&out);

    let r = run::file(
        &StdFileSystem,
        &src,
        &out.join("a.txt"),
        &opts(ExistingPolicy::Overwrite, Links::Copy, Safety::Default),
        &config(true),
    );

    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert!(matches!(r.copy, Some(Ok(_))), "{:?}", r.copy);
    let note = r.dry.expect("a dry run carries its note");
    let planned = note.planned.expect("a single file plans one entry");
    assert_eq!(planned.action, PlannedAction::Copy);
    assert_eq!(snapshot(&out), before);
    assert_eq!(snapshot(d.path()), whole_before);
    assert!(
        snapshot(d.path()).keys().all(|k| !has_flux_name(k) && !k.contains("a.txt.flux-")),
        "a temporary or state entry was created"
    );
}
