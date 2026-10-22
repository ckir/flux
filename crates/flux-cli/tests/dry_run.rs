//! `flux copy --dry-run` end to end (cut 11a, Task 6).

use flux_core::state::{Cleanup, Config, Kind, OpState, OperationState, Options, Root, native_hex};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn flux() -> Command {
    Command::new(env!("CARGO_BIN_EXE_flux"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {:?}", out.stdout))
}

/// `src/a` (1 byte) and `src/sub/b` (2 bytes) under `d`.
fn tree_in(d: &Path) -> PathBuf {
    let src = d.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a"), b"A").unwrap();
    std::fs::write(src.join("sub").join("b"), b"BB").unwrap();
    src
}

const UNPROBED: &str = "note: the no-replace publication primitive was not probed under --dry-run";
const UNPROBED_LOCK: &str = "note: the destination's lock was not taken and no operation state was written under --dry-run; a real run may be refused or fail there";

#[test]
fn a_dry_tree_run_prints_one_line_per_entry_and_the_dry_summary_and_creates_nothing() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let out = flux().args(["copy", "--dry-run"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    for want in [
        "a: object_type=file action=would_copy",
        "sub: object_type=dir action=would_create",
        "sub/b: object_type=file action=would_copy",
        UNPROBED,
        UNPROBED_LOCK,
        "dry run: would copy 2 files (3 bytes), overwrite 0, skip 0, create 2 directories; 0 errors",
    ] {
        assert!(e.contains(want), "missing {want:?} in {e}");
    }
    assert!(!e.contains("copied 2 files"), "{e}");
    // The notes print in the `unprobed` order, before the summary.
    let (noreplace, lock, summary) = (
        e.find(UNPROBED).unwrap(),
        e.find(UNPROBED_LOCK).unwrap(),
        e.find("dry run: would").unwrap(),
    );
    assert!(noreplace < lock && lock < summary, "{e}");
    assert!(!dst.exists());
}

#[test]
fn a_dry_tree_run_json_carries_dry_run_and_unprobed_and_a_live_run_does_not() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let out = flux().args(["copy", "--dry-run", "--json"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["dry_run"], true, "{v}");
    assert_eq!(v["unprobed"], serde_json::json!(["noreplace_publish", "target_lock"]), "{v}");
    assert_eq!(v.as_object().unwrap().len(), 20, "{v}");
    assert_eq!(v["files_copied"], 2, "{v}");
    // Nothing moved, so there is no throughput (M-3).
    assert_eq!(v["average_bytes_per_second"], 0, "{v}");

    let live = d.path().join("live");
    let out = flux().args(["copy", "--json"]).arg(&src).arg(&live).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = json(&out);
    assert!(v.get("dry_run").is_none(), "{v}");
    assert!(v.get("unprobed").is_none(), "{v}");
    assert_eq!(v.as_object().unwrap().len(), 18, "{v}");
}

#[test]
fn a_dry_single_file_run_prints_its_line_and_only_the_lock_note() {
    let d = TempDir::new().unwrap();
    let a = d.path().join("a");
    let b = d.path().join("b");
    std::fs::write(&a, b"A").unwrap();
    let out = flux().args(["copy", "--dry-run"]).arg(&a).arg(&b).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains(&format!("{}: object_type=file action=would_copy", b.display())), "{e}");
    assert!(e.contains("dry run: would copy 1 files"), "{e}");
    assert!(!e.contains(UNPROBED), "{e}");
    assert!(e.contains(UNPROBED_LOCK), "{e}");
    assert!(
        e.find("action=would_copy").unwrap() < e.find(UNPROBED_LOCK).unwrap()
            && e.find(UNPROBED_LOCK).unwrap() < e.find("dry run: would").unwrap(),
        "{e}"
    );
    assert!(!e.contains("copied 1 files"), "{e}");
    assert!(!b.exists());

    let out = flux().args(["copy", "--dry-run", "--json"]).arg(&a).arg(&b).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["unprobed"], serde_json::json!(["target_lock"]), "{v}");
    assert_eq!(v["dry_run"], true, "{v}");
    assert_eq!(v["average_bytes_per_second"], 0, "{v}");

    std::fs::write(&b, b"old").unwrap();
    let out =
        flux().args(["copy", "--dry-run", "--skip-existing"]).arg(&a).arg(&b).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("action=would_skip (existing)"), "{e}");
    assert_eq!(std::fs::read(&b).unwrap(), b"old");
}

#[test]
fn dry_run_combines_with_resume_and_restart() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    for extra in [vec!["--resume"], vec!["--restart"], vec!["--restart", "--break-lock"]] {
        let out =
            flux().args(["copy", "--dry-run"]).args(&extra).arg(&src).arg(&dst).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{extra:?}: {}", stderr(&out));
        assert!(!stderr(&out).contains("does not combine"), "{extra:?}: {}", stderr(&out));
    }
    let out = flux().args(["copy", "--dry-run", "--resume"]).arg(&src).arg(&dst).output().unwrap();
    let e = stderr(&out);
    assert!(e.contains("no prior operation; starting new"), "{e}");
    assert!(e.contains("dry run: would copy 2 files"), "{e}");
    assert!(!dst.exists());
}

#[cfg(unix)]
/// A tree prior operation `id` under `dst` in `state`, with a partial `sub/b.flux-partial.<id>`.
fn prior_in(src: &Path, dst: &Path, id: &str, state: OpState) -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(src).unwrap();
    let config = Config::new(
        vec![Root {
            source_root: native_hex(&std::fs::canonicalize(src).unwrap()),
            source_identity: format!("strong:{}:{}", m.dev(), m.ino()),
            destination_prefix: String::new(),
            kind: None,
        }],
        Options {
            preserve_times: "default".to_owned(),
            preserve_permissions: "default".to_owned(),
            durability: "normal".to_owned(),
            safety: "default".to_owned(),
            existing: "overwrite".to_owned(),
            links: "copy".to_owned(),
            cross_filesystems: false,
        },
    );
    let mut st = OperationState::created(id, Kind::Tree, dst, 5, None, config);
    st.state = state;
    if state == OpState::Completed {
        st.cleanup = Some(Cleanup {
            cleanup_pending: true,
            cleanup_pending_artifacts: vec![native_hex(Path::new(&format!(
                "sub/b.flux-partial.{id}"
            )))],
        });
    }
    let ws = dst.join(".flux").join("operations").join(id);
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("manifest"), st.encode()).unwrap();
    {
        use flux_fs::{DestinationRoot, DirHandle};
        let root = flux_platform::StdFileSystem.destination_root(&ws).unwrap();
        drop(
            root.create_claim_store(std::ffi::OsStr::new("state.db"), flux_fs::Durability::Normal)
                .unwrap(),
        );
    }
    std::fs::create_dir_all(dst.join("sub")).unwrap();
    let partial = dst.join("sub").join(format!("b.flux-partial.{id}"));
    std::fs::write(&partial, b"half").unwrap();
    ws
}

#[cfg(unix)]
#[test]
fn test_79_dry_resume_and_restart_report_and_change_nothing() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let id = "b".repeat(32);
    let ws = prior_in(&src, &dst, &id, OpState::Transferring);
    let partial = dst.join("sub").join(format!("b.flux-partial.{id}"));

    let out = flux().args(["copy", "--dry-run", "--restart"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    for want in [
        format!(".flux/operations/{id}: object_type=state action=would_discard"),
        format!("sub/b.flux-partial.{id}: object_type=partial action=would_discard"),
        format!("note: would supersede operation {id} (state TRANSFERRING)"),
        "dry run: would discard 1 partials and 1 states".to_owned(),
        "sub/b: object_type=file action=would_copy".to_owned(),
        "dry run: would copy 2 files".to_owned(),
    ] {
        assert!(e.contains(&want), "missing {want:?} in {e}");
    }
    assert!(ws.join("manifest").exists() && partial.exists(), "a dry run changes nothing");

    let out = flux().args(["copy", "--dry-run", "--resume"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains(&format!("would resume operation {id}")), "{e}");
    assert!(e.contains("dry run: would copy"), "{e}");
    assert!(e.contains("dry run: 1 partial files present, continuation not previewed"), "{e}");
    assert!(ws.join("manifest").exists() && partial.exists(), "{e}");
    let out = flux()
        .args(["copy", "--dry-run", "--resume", "--json"])
        .arg(&src)
        .arg(&dst)
        .output()
        .unwrap();
    let keys: Vec<String> = json(&out)["unprobed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(keys.last().map(String::as_str), Some("resume_verify"), "{keys:?}");
    assert!(
        stderr(&out).contains(
            "note: --resume-verify=chunks would be applied by a real run; nothing was verified"
        ),
        "{}",
        stderr(&out)
    );

    // A completed prior: the fresh plan with the completed note.
    let d2 = TempDir::new().unwrap();
    let src2 = tree_in(d2.path());
    let dst2 = d2.path().join("dst");
    let id2 = "c".repeat(32);
    prior_in(&src2, &dst2, &id2, OpState::Completed);
    let out =
        flux().args(["copy", "--dry-run", "--resume"]).arg(&src2).arg(&dst2).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains(&format!("an earlier completed operation {id2} exists at DEST")), "{e}");
    assert!(e.contains("a: object_type=file action=would_copy"), "{e}");
}

#[cfg(unix)]
#[test]
fn a_single_file_restart_lists_its_discards() {
    let d = TempDir::new().unwrap();
    let a = d.path().join("a");
    let b = d.path().join("b");
    std::fs::write(&a, b"A").unwrap();
    let id = "d".repeat(32);
    let m = std::fs::metadata(&a).unwrap();
    let config = Config::new(
        vec![Root {
            source_root: native_hex(&std::fs::canonicalize(&a).unwrap()),
            source_identity: format!(
                "strong:{}:{}",
                std::os::unix::fs::MetadataExt::dev(&m),
                std::os::unix::fs::MetadataExt::ino(&m)
            ),
            destination_prefix: String::new(),
            kind: None,
        }],
        Options {
            preserve_times: "default".to_owned(),
            preserve_permissions: "default".to_owned(),
            durability: "normal".to_owned(),
            safety: "default".to_owned(),
            existing: "overwrite".to_owned(),
            links: "copy".to_owned(),
            cross_filesystems: false,
        },
    );
    let fields = flux_core::state::FileFields {
        artifact_type: "state".to_owned(),
        attempt_id: "1".repeat(32),
        artifact_generation: 1,
        source_identity: "strong:1:1".to_owned(),
        target_identity: None,
        target_path_key: "6b".to_owned(),
        owner_instance_id: "2".repeat(32),
        boot_session_id: "boot".to_owned(),
        creation_wall_time: "1".to_owned(),
        last_heartbeat_wall_time: "1".to_owned(),
    };
    let mut st = OperationState::created(&id, Kind::File, &b, 1, Some(fields), config);
    st.state = OpState::Transferring;
    let record = d.path().join(format!("b.flux-state.{id}"));
    std::fs::write(&record, st.encode()).unwrap();
    let partial = d.path().join(format!("b.flux-partial.{id}"));
    std::fs::write(&partial, b"h").unwrap();

    let out = flux().args(["copy", "--dry-run", "--restart"]).arg(&a).arg(&b).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    for want in [
        format!("{}: object_type=state action=would_discard", record.display()),
        format!("{}: object_type=partial action=would_discard", partial.display()),
        format!("note: would supersede operation {id} (state TRANSFERRING)"),
        "dry run: would discard 1 partials and 1 states".to_owned(),
    ] {
        assert!(e.contains(&want), "missing {want:?} in {e}");
    }
    assert!(record.exists() && partial.exists() && !b.exists());
}

#[test]
fn test_136_takeover_preview() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let lock = d.path().join("dst.flux-lock");
    std::fs::write(&lock, [0u8; 100]).unwrap();

    let out = flux()
        .args(["copy", "--dry-run", "--restart", "--break-lock", "--json"])
        .arg(&src)
        .arg(&dst)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(
        e.contains("note: --break-lock would take over the lock, whose record is unreadable ("),
        "{e}"
    );
    assert!(e.contains("a real run refuses with TARGET_LOCK_BUSY if the OS lock is held"), "{e}");
    let keys = json(&out)["unprobed"].clone();
    assert!(keys.as_array().unwrap().iter().any(|k| k == "lock_takeover"), "{keys}");
    assert_eq!(std::fs::read(&lock).unwrap(), vec![0u8; 100], "the lock bytes are untouched");

    // A leftover of an interrupted lock recovery: its note is made before the refusal and prints before it.
    std::fs::write(d.path().join("dst.flux-lock.broken.x"), b"").unwrap();
    let out = flux().args(["copy", "--dry-run", "--restart"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("TARGET_LOCK_UNCERTAIN"), "{e}");
    let note = e.find("note: an interrupted lock recovery left dst.flux-lock.broken.x").expect(&e);
    let refusal = e.find("TARGET_LOCK_UNCERTAIN").unwrap();
    assert!(note < refusal, "the notes print before the refusal (ruling P10): {e}");
    assert!(!e.contains("dry run: would copy"), "no plan follows a predicted refusal: {e}");
    assert_eq!(std::fs::read(&lock).unwrap(), vec![0u8; 100]);
}

#[test]
fn a_recorded_lock_prints_the_dual_outcome_and_exits_zero() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let now = flux_core::state::wall_time_ns();
    let record = flux_core::lock::record::LockRecord {
        complete_lock_key: "test-lock-key".into(),
        operation_id: "e".repeat(32),
        owner_instance_id: "f".repeat(32),
        boot_session_id: "test-boot".into(),
        target_path_key: "test-target-key".into(),
        workspace_path: "none".into(),
        creation_wall_time: now,
        last_heartbeat_wall_time: now,
    };
    std::fs::write(d.path().join("dst.flux-lock"), record.encode()).unwrap();
    let out = flux().args(["copy", "--dry-run", "--json"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(
        e.contains(&format!(
            "note: a lock record for operation {} exists (owner instance {}",
            "e".repeat(32),
            "f".repeat(32)
        )),
        "{e}"
    );
    assert!(e.contains("if it is held, TARGET_LOCK_BUSY; if not, the preview below applies, and this exit code reflects that case"), "{e}");
    assert_eq!(
        json(&out)["unprobed"],
        serde_json::json!(["noreplace_publish", "target_lock", "owner_liveness"])
    );
}

#[cfg(unix)]
#[test]
fn the_json_unprobed_order_is_fixed() {
    let order = [
        "noreplace_publish",
        "target_lock",
        "owner_liveness",
        "lock_takeover",
        "prior_operation",
        "resume_verify",
    ];
    let relative = |keys: &serde_json::Value| -> Vec<usize> {
        keys.as_array()
            .unwrap()
            .iter()
            .map(|k| order.iter().position(|o| k == o).unwrap_or_else(|| panic!("unknown {k}")))
            .collect()
    };
    // Resume with a completed prior: noreplace, lock, prior, resume_verify.
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    prior_in(&src, &dst, &"c".repeat(32), OpState::Completed);
    let out = flux()
        .args(["copy", "--dry-run", "--resume", "--json"])
        .arg(&src)
        .arg(&dst)
        .output()
        .unwrap();
    assert_eq!(
        json(&out)["unprobed"],
        serde_json::json!(["noreplace_publish", "target_lock", "prior_operation", "resume_verify"]),
        "{}",
        stderr(&out)
    );
    // A torn lock taken over under --restart: noreplace, lock, takeover.
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::write(d.path().join("dst.flux-lock"), [0u8; 100]).unwrap();
    let out = flux()
        .args(["copy", "--dry-run", "--restart", "--break-lock", "--json"])
        .arg(&src)
        .arg(&dst)
        .output()
        .unwrap();
    let r = relative(&json(&out)["unprobed"]);
    assert!(r.windows(2).all(|w| w[0] < w[1]), "{r:?}");
    assert!(r.contains(&3), "{r:?}");
}

#[test]
fn a_dry_run_exits_as_the_real_run_would() {
    // DEST is a regular file: the same status in both (a usage stop in `resolve`, 2; the brief said 1).
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::write(&dst, b"x").unwrap();
    let dry = flux().args(["copy", "--dry-run"]).arg(&src).arg(&dst).output().unwrap();
    let live = flux().args(["copy"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(dry.status.code(), Some(2), "{}", stderr(&dry));
    assert_eq!(live.status.code(), Some(2), "{}", stderr(&live));

    // DEST below a regular file: the engine's pre-flight refuses (status 1), and the dry report is still marked dry.
    let below = dst.join("x");
    let live = flux().args(["copy", "--json"]).arg(&src).arg(&below).output().unwrap();
    assert_eq!(live.status.code(), Some(1), "{}", stderr(&live));
    assert!(json(&live).get("dry_run").is_none());
    let dry = flux().args(["copy", "--dry-run", "--json"]).arg(&src).arg(&below).output().unwrap();
    assert_eq!(dry.status.code(), Some(1), "{}", stderr(&dry));
    assert_eq!(json(&dry)["dry_run"], true);
    assert!(!stderr(&dry).contains("copied "), "{}", stderr(&dry));

    // DEST inside SOURCE: status 3 in both, nothing created.
    let inside = src.join("dst");
    let dry = flux().args(["copy", "--dry-run"]).arg(&src).arg(&inside).output().unwrap();
    assert_eq!(dry.status.code(), Some(3), "{}", stderr(&dry));
    assert!(!inside.exists());
    let live = flux().args(["copy"]).arg(&src).arg(&inside).output().unwrap();
    assert_eq!(live.status.code(), Some(3), "{}", stderr(&live));
    assert!(!inside.exists());

    // A missing source: a pre-engine stop, still marked dry.
    let missing = d.path().join("missing");
    let out = flux()
        .args(["copy", "--dry-run", "--json"])
        .arg(&missing)
        .arg(d.path().join("out"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(json(&out)["dry_run"], true);
}

#[test]
fn a_dry_run_notes_a_completed_prior_and_still_plans() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let id = "a".repeat(32);
    let config = Config::new(
        vec![Root {
            source_root: native_hex(Path::new("/s")),
            source_identity: "strong:3:9".to_owned(),
            destination_prefix: String::new(),
            kind: None,
        }],
        Options {
            preserve_times: "default".to_owned(),
            preserve_permissions: "default".to_owned(),
            durability: "normal".to_owned(),
            safety: "default".to_owned(),
            existing: "overwrite".to_owned(),
            links: "copy".to_owned(),
            cross_filesystems: false,
        },
    );
    let mut state = OperationState::created(&id, Kind::Tree, &dst, 5, None, config);
    state.state = OpState::Completed;
    let leftover = format!("sub/b.flux-partial.{id}");
    state.cleanup = Some(Cleanup {
        cleanup_pending: true,
        cleanup_pending_artifacts: vec![native_hex(Path::new(&leftover))],
    });
    let ws = dst.join(".flux").join("operations").join(&id);
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("manifest"), state.encode()).unwrap();
    std::fs::create_dir_all(dst.join("sub")).unwrap();
    let partial = dst.join("sub").join(format!("b.flux-partial.{id}"));
    std::fs::write(&partial, b"half").unwrap();

    let out = flux().args(["copy", "--dry-run"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(
        e.contains(&format!("note: an earlier completed operation {id} exists at DEST")),
        "{e}"
    );
    assert!(e.contains("a: object_type=file action=would_copy"), "{e}");
    assert!(ws.join("manifest").exists());
    assert!(partial.exists(), "a dry run finishes no cleanup");

    // The JSON says so too (ruling P10): the prior is among the unprobed checks, after the other two.
    let out = flux().args(["copy", "--dry-run", "--json"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        json(&out)["unprobed"],
        serde_json::json!(["noreplace_publish", "target_lock", "prior_operation"])
    );
}

/// Restores a directory's permissions when dropped, so `TempDir` can remove it.
#[cfg(unix)]
struct Restore(PathBuf);

#[cfg(unix)]
impl Drop for Restore {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
#[test]
fn a_dry_tree_run_counts_a_walk_failure_in_its_summary_and_exits_as_live_does() {
    // Test audit 2026-10-17: the summary's error count is the report's `errors` (a walk failure counts, though no
    // file failed); every other CLI dry test has 0 errors.
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let sub = src.join("sub");
    let _restore = Restore(sub.clone());
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root ignores permissions: probe, and say so rather than fail.
    if std::fs::read_dir(&sub).is_ok() {
        println!("skipped: running as root, directory permissions are not enforced");
        return;
    }
    let dst = d.path().join("dst");
    let dry = flux().args(["copy", "--dry-run", "--json"]).arg(&src).arg(&dst).output().unwrap();
    let e = stderr(&dry);
    assert_eq!(dry.status.code(), Some(1), "{e}");
    // `sub` fails to list before it is planned, so only DEST itself would be created.
    assert!(
        e.contains("dry run: would copy 1 files (1 bytes), overwrite 0, skip 0, create 1 directories; 1 errors"),
        "{e}"
    );
    let counts = |v: &serde_json::Value| {
        (v["errors"].as_u64(), v["files_failed"].as_u64(), v["files_copied"].as_u64())
    };
    let v = json(&dry);
    assert_eq!(counts(&v), (Some(1), Some(0), Some(1)), "{v}");
    assert!(!dst.exists());

    let live = flux().args(["copy", "--json"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(live.status.code(), Some(1), "{}", stderr(&live));
    assert_eq!(counts(&json(&live)), counts(&v), "{}", stderr(&live));
}

#[cfg(unix)]
#[test]
fn a_dry_run_stopped_on_a_symlink_source_is_still_marked_dry() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let link = d.path().join("to-dir");
    std::os::unix::fs::symlink(&src, &link).unwrap();
    let dst = d.path().join("dst");
    let out = flux().args(["copy", "--dry-run", "--json"]).arg(&link).arg(&dst).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).starts_with("SYMLINK_CREATION_UNAVAILABLE: "), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["dry_run"], true, "{v}");
    assert!(v.get("unprobed").is_none(), "no dry branch ran: {v}");
    assert!(!dst.exists());
}
