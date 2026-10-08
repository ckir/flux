//! Real-system tests for cut 8b: replacement on the default filesystem of every OS, name folding on a case-insensitive
//! destination (macOS and Windows natively, a Linux loopback vfat image), and a Windows junction at the destination
//! name. Locally a missing facility skips with a stated reason; when `CI` is set it fails instead.

use flux_core::run::{RunConfig, tree};
use flux_core::{TreeFailure, TreeFailureCause, TreeOutcome};
use flux_fs::{Code, CopyOptions, Durability, OperationId, Preserve, Publish, Safety};
use flux_platform::StdFileSystem;
use std::path::Path;
use tempfile::TempDir;

fn opts() -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability: Durability::Normal,
        publish: Publish::Replace,
        safety: Safety::Default,
        operation_id: OperationId::new("t"),
        existing: flux_fs::ExistingPolicy::Overwrite,
    }
}

fn run_cfg() -> RunConfig {
    RunConfig {
        restart: false,
        break_lock: false,
        operation_id: flux_core::ids::new_id(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: "test".to_string(),
        before_mutation: None,
        heartbeat_interval: std::time::Duration::from_secs(3600),
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// One clean run of `src` over `dst`: the outcome and every reported failure.
fn run(src: &Path, dst: &Path) -> (TreeOutcome, Vec<TreeFailure>) {
    let mut got = Vec::new();
    let r = tree(&StdFileSystem, src, dst, &opts(), &run_cfg(), &mut |f| got.push(f));
    assert!(r.stop.is_none(), "{:?}", r.stop);
    (r.copy.expect("the copy ran").map_err(|a| a.error).expect("the walk completed"), got)
}

/// The code a reported failure carries, for the causes that carry one.
fn code_of(cause: &TreeFailureCause) -> Option<Code> {
    match cause {
        TreeFailureCause::CreateDir(e) => Some(e.code),
        TreeFailureCause::Copy(e) => Some(e.cause.code),
        _ => None,
    }
}

/// `dst` holds `FILE.TXT` = "old"; `src` holds two names differing only in case. A destination that folds them takes
/// the first the walk reaches (a replacement) and reports the second as `DESTINATION_NAMESPACE_COLLISION`; the
/// destination ends with ONE entry, holding the winner's content.
fn assert_two_source_names_fold(src: &Path, dst: &Path) {
    std::fs::write(src.join("File.txt"), b"upper").unwrap();
    std::fs::write(src.join("file.txt"), b"lower").unwrap();
    std::fs::write(dst.join("FILE.TXT"), b"old").unwrap();
    let (out, got) = run(src, dst);
    assert_eq!(got.len(), 1, "exactly the second name is reported: {got:?}");
    assert_eq!(code_of(&got[0].cause), Some(Code::DestinationNamespaceCollision), "{got:?}");
    assert_eq!(out.files_copied, 1, "{got:?}");
    let loser = got[0].path.file_name().unwrap().to_string_lossy().into_owned();
    let winner_body: &[u8] = if loser == "file.txt" { b"upper" } else { b"lower" };
    let entries: Vec<String> = names(dst).into_iter().filter(|n| n != ".flux").collect();
    assert_eq!(entries.len(), 1, "one entry in the destination: {entries:?}");
    assert_eq!(std::fs::read(dst.join(&entries[0])).unwrap(), winner_body);
}

#[test]
fn replacement_works_on_the_default_filesystem() {
    use flux_fs::{ClaimKey, ClaimStatus, ClaimStore, DestinationRoot, DirHandle, FileIdentity};
    use std::ffi::OsStr;
    use std::sync::Arc;

    let d = TempDir::new().unwrap();
    let src = d.path().join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a"), b"A").unwrap();
    std::fs::write(src.join("sub").join("b"), b"BB").unwrap();

    // A clean run: replaced, nothing of the run's workspace or claim store left.
    let dst = d.path().join("dst");
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(dst.join("a"), b"old").unwrap();
    let (out, got) = run(&src, &dst);
    assert_eq!((out.files_copied, got.len()), (2, 0), "{got:?}");
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"A");
    assert_eq!(std::fs::read(dst.join("sub").join("b")).unwrap(), b"BB");
    assert!(!dst.join(".flux").join("operations").exists(), "the workspace is gone");
    assert_eq!(names(&dst), ["a", "sub"]);

    // A run cut short (the guard panics at its 3rd mutation, as a crash would): the workspace and `state.db` stay,
    // and the claim for `a`, recorded before its publish, is in the store. `Strict` makes every claim commit
    // durable, so the answer does not depend on the directory-end sync this test does not reach.
    let dst2 = d.path().join("dst2");
    std::fs::create_dir_all(&dst2).unwrap();
    std::fs::write(dst2.join("a"), b"old").unwrap();
    let seen = std::sync::atomic::AtomicU64::new(0);
    let cfg = RunConfig {
        before_mutation: Some(Arc::new(move || {
            if seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == 3 {
                panic!("cut short on purpose");
            }
        })),
        ..run_cfg()
    };
    let strict = CopyOptions { durability: Durability::Strict, ..opts() };
    let cut = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tree(&StdFileSystem, &src, &dst2, &strict, &cfg, &mut |_| {})
    }));
    assert!(cut.is_err(), "the run was cut short");
    assert_eq!(std::fs::read(dst2.join("a")).unwrap(), b"old", "cut before the publish");

    let ops = dst2.join(".flux").join("operations");
    let ids = names(&ops);
    assert_eq!(ids.len(), 1, "one kept workspace: {ids:?}");
    let db = ops.join(&ids[0]).join("state.db");
    let file =
        std::fs::OpenOptions::new().read(true).write(true).open(&db).expect("state.db is kept");
    let store = flux_platform::RedbClaimStore::create_file(file, Durability::Strict)
        .expect("the kept store opens");
    let root = StdFileSystem.destination_root(&dst2).unwrap();
    let FileIdentity::Strong(parent) = root.identity().unwrap() else {
        panic!("a strong identity")
    };
    let record = store
        .get(&ClaimKey::new(parent, OsStr::new("a")))
        .unwrap()
        .expect("the claim for `a` was recorded");
    assert_eq!(record.target.0, b"a");
    assert_eq!(record.status, ClaimStatus::Existing);
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn a_case_differing_source_name_over_an_existing_entry_on_a_case_insensitive_filesystem() {
    let d = TempDir::new().unwrap();
    let src = d.path().join("src");
    let dst = d.path().join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(dst.join("FILE.TXT"), b"old").unwrap();
    std::fs::write(src.join("file.txt"), b"new").unwrap();
    let (out, got) = run(&src, &dst);
    assert_eq!((out.files_copied, got.len()), (1, 0), "{got:?}");
    let entries: Vec<String> = names(&dst).into_iter().filter(|n| n != ".flux").collect();
    assert_eq!(entries.len(), 1, "one entry: {entries:?}");
    assert_eq!(std::fs::read(dst.join(&entries[0])).unwrap(), b"new");

    // The two-source fold needs a SOURCE filesystem that holds both spellings; the temporary directory of a
    // case-insensitive system cannot, so that part is skipped there.
    let src2 = d.path().join("src2");
    let dst2 = d.path().join("dst2");
    std::fs::create_dir_all(&src2).unwrap();
    std::fs::create_dir_all(&dst2).unwrap();
    std::fs::write(src2.join("File.txt"), b"x").unwrap();
    std::fs::write(src2.join("file.txt"), b"y").unwrap();
    if names(&src2).len() < 2 {
        eprintln!(
            "skipped the two-source fold: the source filesystem folds case, so it holds one name"
        );
        return;
    }
    std::fs::remove_file(src2.join("File.txt")).unwrap();
    std::fs::remove_file(src2.join("file.txt")).unwrap();
    assert_two_source_names_fold(&src2, &dst2);
}

#[cfg(target_os = "linux")]
mod linux_vfat {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    /// A vfat image mounted with `sudo -n mount -o loop`, unmounted on drop.
    struct Mounted(PathBuf);

    fn id(flag: &str) -> String {
        let out = Command::new("id").arg(flag).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// `None` when the facility (`mkfs.vfat` or passwordless `sudo mount`) is missing locally; on CI (`CI` set) a
    /// missing facility is a failure, never a skip.
    fn mount(image: &Path, at: &Path) -> Option<Mounted> {
        let fail = |why: &str| {
            assert!(std::env::var_os("CI").is_none(), "CI must provide: {why}");
            eprintln!("skipped: {why}");
            None
        };
        let f = std::fs::File::create(image).unwrap();
        f.set_len(16 * 1024 * 1024).unwrap();
        drop(f);
        // Each attempt's stderr (or its spawn error) is kept, so a CI failure says why.
        let mut mkfs_log = String::new();
        let made = ["mkfs.vfat", "/usr/sbin/mkfs.vfat", "/sbin/mkfs.vfat"].iter().any(|bin| {
            match Command::new(bin).arg(image).output() {
                Ok(o) if o.status.success() => true,
                Ok(o) => {
                    mkfs_log.push_str(&format!("{bin}: {}\n", String::from_utf8_lossy(&o.stderr)));
                    false
                }
                Err(e) => {
                    mkfs_log.push_str(&format!("{bin}: {e}\n"));
                    false
                }
            }
        });
        if !made {
            return fail(&format!("`mkfs.vfat` (dosfstools) is missing or failed:\n{mkfs_log}"));
        }
        let mount_log = match Command::new("sudo")
            .args(["-n", "mount", "-o"])
            .arg(format!("loop,uid={},gid={}", id("-u"), id("-g")))
            .arg(image)
            .arg(at)
            .output()
        {
            Ok(o) if o.status.success() => None,
            Ok(o) => Some(String::from_utf8_lossy(&o.stderr).into_owned()),
            Err(e) => Some(e.to_string()),
        };
        if let Some(log) = mount_log {
            return fail(&format!("passwordless `sudo -n mount -o loop`:\n{log}"));
        }
        Some(Mounted(at.to_path_buf()))
    }

    impl Drop for Mounted {
        fn drop(&mut self) {
            let ok = Command::new("sudo")
                .args(["-n", "umount"])
                .arg(&self.0)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                eprintln!("warning: could not unmount {}", self.0.display());
            }
        }
    }

    #[test]
    fn a_vfat_loopback_destination_folds_case() {
        let d = TempDir::new().unwrap();
        let src = d.path().join("src");
        let dst = d.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        // Declared after the directory so it is unmounted first.
        let Some(_mounted) = mount(&d.path().join("vfat.img"), &dst) else { return };
        assert_two_source_names_fold(&src, &dst);
    }
}

#[cfg(windows)]
#[test]
fn a_file_over_a_junction_at_the_destination_name() {
    let d = TempDir::new().unwrap();
    let src = d.path().join("src");
    let dst = d.path().join("dst");
    let target = d.path().join("target");
    for p in [&src, &dst, &target] {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(target.join("keep"), b"keep").unwrap();
    std::fs::write(src.join("j"), b"FILE").unwrap();
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(dst.join("j"))
        .arg(&target)
        .output()
        .unwrap();
    assert!(made.status.success(), "mklink /J: {}", String::from_utf8_lossy(&made.stderr));

    let mut got = Vec::new();
    let r = tree(&StdFileSystem, &src, &dst, &opts(), &run_cfg(), &mut |f| got.push(f));

    // MEASURED on windows-latest in 2026-10: the run completes, and the file is refused as ONE per-target failure at
    // the publish step (IoError; the OS answered access denied), with no leftover temporary, and the destination name
    // is still the junction. On a different outcome, re-measure; do not weaken these assertions.
    let entry = std::fs::symlink_metadata(dst.join("j"));
    eprintln!(
        "junction outcome: stop={:?} copy_ok={} failures={got:?} dst/j={:?}",
        r.stop,
        matches!(r.copy, Some(Ok(_))),
        entry.as_ref().map(|m| (m.is_file(), m.is_dir(), m.file_type().is_symlink())),
    );
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert!(matches!(r.copy, Some(Ok(_))), "the copy ran and the walk completed");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].path, Path::new("j"));
    match &got[0].cause {
        TreeFailureCause::Copy(e) => {
            assert_eq!(e.step, flux_core::copy::CopyStep::Publish, "{e:?}");
            assert_eq!(e.cause.code, Code::IoError, "{e:?}");
            assert!(e.leftover.is_none(), "no temporary stays: {e:?}");
        }
        other => panic!("expected a copy failure, got {other:?}"),
    }
    let entry = entry.expect("the destination name still exists");
    assert!(entry.file_type().is_symlink(), "dst/j is still the junction");
    for n in names(&dst) {
        assert!(!n.contains("flux-partial"), "no partial stays: {n}");
    }
    assert_eq!(names(&target), ["keep"], "nothing was created inside the junction's target");
    assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"keep");
    assert_eq!(std::fs::read(src.join("j")).unwrap(), b"FILE");
}
