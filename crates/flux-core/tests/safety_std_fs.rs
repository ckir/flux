//! Real-system tests for cut 8a: a Linux bind mount inside DEST (Part M and the `mount_root` query against the
//! same mount) and a link in a parent component of DEST (Part A). Locally a missing facility skips with a stated
//! reason; when `CI` is set it fails instead.

use flux_core::run::RunConfig;
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

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[cfg(target_os = "linux")]
mod linux_mount {
    use super::*;
    use flux_core::run::tree;
    use flux_core::{TreeFailure, TreeFailureCause, TreeOutcome};
    use flux_fs::{DestinationRoot, DirHandle, MountRoot};
    use std::path::PathBuf;

    /// `src/a` (1 byte) and `src/sub/b` (2 bytes) under `d`.
    fn tree_in(d: &Path) -> PathBuf {
        let src = d.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a"), b"A").unwrap();
        std::fs::write(src.join("sub").join("b"), b"BB").unwrap();
        src
    }

    fn run(
        src: &Path,
        dst: &Path,
    ) -> (Result<TreeOutcome, flux_core::copy::CopyError>, Vec<TreeFailure>) {
        let mut got = Vec::new();
        let r = tree(&StdFileSystem, src, dst, &opts(), &run_cfg(), &mut |f| got.push(f));
        assert!(r.stop.is_none(), "{:?}", r.stop);
        (r.copy.expect("the copy ran").map_err(|a| a.error), got)
    }

    /// `sudo -n mount --bind src dst`, unmounted on drop. `None` when the facility is missing locally; on CI
    /// (`CI` set) a missing facility is a failure, never a skip.
    struct Bind(std::path::PathBuf);

    fn bind(src: &Path, dst: &Path) -> Option<Bind> {
        let ok = std::process::Command::new("sudo")
            .args(["-n", "mount", "--bind"])
            .arg(src)
            .arg(dst)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(Bind(dst.to_path_buf()));
        }
        assert!(std::env::var_os("CI").is_none(), "CI must be able to `sudo -n mount --bind`");
        eprintln!("skipped: no passwordless sudo for `mount --bind`");
        None
    }

    impl Drop for Bind {
        fn drop(&mut self) {
            let ok = std::process::Command::new("sudo")
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
    fn a_bind_mount_of_a_source_subdirectory_inside_the_destination_is_skipped_and_the_source_is_untouched()
     {
        let d = TempDir::new().unwrap();
        let src = tree_in(d.path());
        let dst = d.path().join("dst");
        std::fs::create_dir_all(dst.join("sub")).unwrap();
        let Some(_mount) = bind(&src.join("sub"), &dst.join("sub")) else { return };

        // The query, against this mount.
        let root = StdFileSystem.destination_root(&dst).unwrap();
        let sub = root.open_dir(std::ffi::OsStr::new("sub")).unwrap();
        assert_eq!(sub.mount_root(&root).unwrap(), MountRoot::Yes);
        drop(sub);
        drop(root);

        let (r, got) = run(&src, &dst);
        let out = r.unwrap();
        assert_eq!((out.files_copied, out.failures.create_dir), (1, 1), "{got:?}");
        assert_eq!(got[0].path, Path::new("sub"));
        let TreeFailureCause::CreateDir(e) = &got[0].cause else { panic!("{:?}", got[0].cause) };
        assert_eq!(e.code, Code::SafetyRejected);
        assert_eq!(
            names(&src.join("sub")),
            vec!["b"],
            "nothing landed in the source through the mount"
        );
        assert_eq!(names(&dst), vec!["a", "sub"]);
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
        resume: false,
        descriptor_limit: None,
    }
}

/// `src/0-first` (reached by the walk BEFORE `sub`), `src/sub/b`; `holder/link` -> `src/sub`; DEST = `holder/link/out`.
/// Without Part A the run would create `out` inside `src/sub` and write `0-first` there before the walk's own
/// check could stop it.
fn link_fixture(
    d: &Path,
    link: impl FnOnce(&Path, &Path) -> bool,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let src = d.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("0-first"), b"F").unwrap();
    std::fs::write(src.join("sub").join("b"), b"BB").unwrap();
    let holder = d.join("holder");
    std::fs::create_dir(&holder).unwrap();
    if !link(&src.join("sub"), &holder.join("link")) {
        return None;
    }
    Some((src, holder.join("link").join("out")))
}

fn assert_refused_before_the_lock(src: &Path, dst: &Path) {
    let mut got = Vec::new();
    let r =
        flux_core::run::tree(&StdFileSystem, src, dst, &opts(), &run_cfg(), &mut |f| got.push(f));
    assert!(r.stop.is_none(), "{:?}", r.stop);
    let Some(Err(a)) = &r.copy else { panic!("{:?}", r.copy) };
    assert_eq!(a.error.code(), Code::SafetyRejected);
    assert!(a.error.to_string().contains("resolved location"), "{}", a.error);
    assert!(a.refused_unchanged());
    assert_eq!(names(&src.join("sub")), vec!["b"], "no `out`, no lock, no workspace in the source");
    assert_eq!(std::fs::read(src.join("0-first")).unwrap(), b"F");
    assert_eq!(std::fs::read(src.join("sub").join("b")).unwrap(), b"BB");
    assert!(got.is_empty());
}

#[cfg(unix)]
#[test]
fn a_symlink_in_a_parent_component_of_dest_into_a_source_subdirectory_is_refused_before_the_lock() {
    let d = TempDir::new().unwrap();
    let (src, dst) =
        link_fixture(d.path(), |t, l| std::os::unix::fs::symlink(t, l).is_ok()).unwrap();
    assert_refused_before_the_lock(&src, &dst);
}

#[cfg(windows)]
#[test]
fn a_junction_in_a_parent_component_of_dest_into_a_source_subdirectory_is_refused_before_the_lock()
{
    let d = TempDir::new().unwrap();
    let junction = |t: &Path, l: &Path| {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", &l.display().to_string(), &t.display().to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    // Not a skip: `mklink /J` needs no privilege (`flux-platform/tests/dir_handle.rs`, the junction tests).
    let (src, dst) = link_fixture(d.path(), junction)
        .expect("could not create a junction; mklink /J requires no privilege");
    assert_refused_before_the_lock(&src, &dst);
}
