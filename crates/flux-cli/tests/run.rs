//! `flux copy` under the destination's lock (cut 7a Part 3b-2): the design's Testing item 4, end to end on the real
//! filesystem. The crash hook exists only in a debug build, which is what `cargo test` builds.

use flux_core::lock::record::LockRecord;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn flux() -> Command {
    Command::new(env!("CARGO_BIN_EXE_flux"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `src/a` (1 byte) and `src/sub/b` (2 bytes) under `d`.
fn tree_in(d: &Path) -> PathBuf {
    let src = d.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a"), b"A").unwrap();
    std::fs::write(src.join("sub").join("b"), b"BB").unwrap();
    src
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn os(s: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(s)
}

fn copy(args: &[&std::ffi::OsStr]) -> Output {
    flux().arg("copy").args(args).output().unwrap()
}

/// A `flux copy src dst` stopped just before its `at`-th guarded mutation, holding what it holds there (the debug
/// build's hook, Part 3b-2). It is killed when dropped.
struct Stalled(Child);

impl Stalled {
    fn start(src: &Path, dst: &Path, at: u32, marks: &Path) -> Self {
        let announced = marks.join(format!("stalled-{at}"));
        let child = flux()
            .arg("copy")
            .arg(src)
            .arg(dst)
            .env("FLUX_TEST_STALL_AT", at.to_string())
            .env("FLUX_TEST_STALL_FILE", &announced)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut s = Stalled(child);
        let deadline = Instant::now() + Duration::from_secs(60);
        while !announced.exists() {
            if let Some(status) = s.0.try_wait().unwrap() {
                panic!("the run exited before its stall point: {status}");
            }
            assert!(Instant::now() < deadline, "the run never reached its stall point");
            std::thread::sleep(Duration::from_millis(20));
        }
        s
    }

    /// A crash: the process dies holding what it held.
    fn kill(mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}

impl Drop for Stalled {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_tree_copy_and_a_file_copy_leave_no_state_or_lock_behind() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(names(&dst), ["a", "sub"]);
    let t = d.path().join("t");
    let out = copy(&[src.join("a").as_os_str(), t.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(names(d.path()), ["dst", "src", "t"]);
}

#[test]
fn a_killed_run_leaves_a_resumable_operation_that_restart_supersedes() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    // `a`'s guarded mutations: its sweep (1), its temporary (2), its publish (3): killed with the temporary written.
    Stalled::start(&src, &dst, 3, marks.path()).kill();
    let left = names(&dst);
    assert!(left.iter().any(|n| n.starts_with("a.flux-partial.")), "{left:?}");
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("RESUMABLE_OPERATION_EXISTS") && e.contains("--restart"), "{e}");
    let out = copy(&[os("--restart"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(names(&dst), ["a", "sub"], "the killed run's temporary and state are gone");
    assert_eq!(names(d.path()), ["dst", "src"], "and its lock");
}

#[test]
fn a_killed_single_file_run_leaves_a_resumable_record_that_restart_supersedes() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let t = d.path().join("t");
    // The single file's guarded mutations: its sweep (1), its temporary (2), its publish (3): killed with the
    // temporary written and the record beside the target.
    Stalled::start(&src.join("a"), &t, 3, marks.path()).kill();
    let left = names(d.path());
    assert!(left.iter().any(|n| n.starts_with("t.flux-partial.")), "{left:?}");
    assert!(left.iter().any(|n| n.starts_with("t.flux-state.")), "{left:?}");
    let out = copy(&[src.join("a").as_os_str(), t.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("RESUMABLE_OPERATION_EXISTS"), "{}", stderr(&out));
    let out = copy(&[os("--restart"), src.join("a").as_os_str(), t.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(names(d.path()), ["src", "t"], "the record, the partial and the lock are gone");
    assert_eq!(std::fs::read(&t).unwrap(), b"A");
}

#[test]
fn a_live_holder_makes_another_run_busy_and_names_the_lock_and_the_holder() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let holder = Stalled::start(&src, &dst, 1, marks.path());
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("TARGET_LOCK_BUSY") && e.contains("dst.flux-lock"), "{e}");
    assert!(e.contains("  holder owner_instance_id: "), "§96.2: {e}");
    holder.kill();
}

#[test]
fn an_empty_lock_is_uncertain_until_restart_break_lock_takes_it_over() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::write(d.path().join("dst.flux-lock"), b"").unwrap();
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("TARGET_LOCK_UNCERTAIN") && e.contains("--restart --break-lock"), "{e}");
    let out = copy(&[os("--restart"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "--restart alone never takes a lock over: {}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("TARGET_LOCK_UNCERTAIN"), "{}", stderr(&out));
    let out = copy(&[os("--break-lock"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(2), "--break-lock alone is a usage error: {}", stderr(&out));
    let out = copy(&[os("--restart"), os("--break-lock"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(names(&dst), ["a", "sub"]);
    assert_eq!(names(d.path()), ["dst", "src"]);
}

#[test]
fn unreadable_or_newer_state_is_refused_and_preserved() {
    let corrupt: &[u8] = b"garbage";
    let newer: &[u8] = br#"{"format_version":2}"#;
    for (bytes, code) in [(corrupt, "STATE_CORRUPT"), (newer, "INCOMPATIBLE_STATE")] {
        let d = TempDir::new().unwrap();
        let src = tree_in(d.path());
        let dst = d.path().join("dst");
        let workspace = dst.join(".flux").join("operations").join("1".repeat(32));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("manifest"), bytes).unwrap();
        let out = copy(&[src.as_os_str(), dst.as_os_str()]);
        assert_eq!(out.status.code(), Some(3), "{code}: {}", stderr(&out));
        assert!(stderr(&out).contains(code), "{}", stderr(&out));
        assert_eq!(std::fs::read(workspace.join("manifest")).unwrap(), bytes, "{code}: preserved");
        assert!(!dst.join("a").exists(), "{code}: nothing copied");
    }
}

#[test]
fn a_foreign_object_at_dest_flux_is_a_control_plane_conflict() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::create_dir(&dst).unwrap();
    std::fs::write(dst.join(".flux"), b"not Flux's").unwrap();
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("CONTROL_PLANE_NAMESPACE_CONFLICT"), "{}", stderr(&out));
    assert_eq!(names(&dst), [".flux"]);
}

#[test]
fn a_source_entry_landing_in_a_reserved_control_path_fails_alone() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    std::fs::create_dir_all(src.join(".flux").join("operations")).unwrap();
    std::fs::write(src.join(".flux").join("operations").join("x"), b"x").unwrap();
    std::fs::write(src.join(".flux").join("other"), b"data").unwrap();
    let dst = d.path().join("dst");
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("CONTROL_PLANE_NAMESPACE_CONFLICT"), "{}", stderr(&out));
    assert_eq!(
        std::fs::read(dst.join(".flux").join("other")).unwrap(),
        b"data",
        "the rest of .flux is data"
    );
    assert!(!dst.join(".flux").join("operations").exists());
    assert_eq!(std::fs::read(dst.join("sub").join("b")).unwrap(), b"BB");
}

#[test]
fn a_dead_owners_record_naming_a_missing_workspace_is_artifact_ownership_uncertain() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let id = "a".repeat(32);
    let record = LockRecord {
        complete_lock_key: "none/00".to_string(),
        operation_id: id.clone(),
        owner_instance_id: "b".repeat(32),
        boot_session_id: "unknown".to_string(),
        target_path_key: "00".to_string(),
        workspace_path: format!("operations/{id}"),
        creation_wall_time: 1,
        last_heartbeat_wall_time: 1,
    }
    .encode();
    let lock = d.path().join("dst.flux-lock");
    std::fs::write(&lock, &record).unwrap();
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("ARTIFACT_OWNERSHIP_UNCERTAIN"), "{e}");
    assert!(e.contains(&format!("  holder workspace_path: operations/{id}")), "{e}");
    assert_eq!(std::fs::read(&lock).unwrap(), record, "preserved");
}

#[test]
fn a_target_name_too_long_for_its_state_record_is_refused_before_anything_is_made() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let long = d.path().join("n".repeat(230));
    let out = copy(&[src.join("a").as_os_str(), long.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("PATH_COMPONENT_INVALID"), "{}", stderr(&out));
    assert_eq!(names(d.path()), ["src"]);
}

#[test]
fn json_on_a_refusal_has_exactly_the_section_53_fields_and_one_error() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::write(d.path().join("dst.flux-lock"), b"").unwrap();
    let out = copy(&[os("--json"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v.as_object().unwrap().len(), 18, "§53's complete field list: {v}");
    assert_eq!(v["errors"].as_u64(), Some(1));
}
