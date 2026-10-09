//! `flux cleanup` end to end on the real filesystem (cut 9b, Task 8). The crash hook exists only in a debug build,
//! which is what `cargo test` builds; `flux cleanup` counts its guarded destructive steps with the same hook as
//! `flux copy`.

use flux_core::lock::record::{Decoded, LockRecord, decode};
use flux_core::state::{Cleanup, Config, Kind, OpState, OperationState, Options, Root, native_hex};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime};
use tempfile::TempDir;

fn flux() -> Command {
    Command::new(env!("CARGO_BIN_EXE_flux"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
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

fn cleanup(dst: &Path, flags: &[&str]) -> Output {
    flux().arg("cleanup").arg(dst).args(flags).output().unwrap()
}

/// A flux run (`copy` or `cleanup`) stopped just before its `at`-th guarded mutation, holding what it holds there.
/// It is killed when dropped. Each stalled run of one test needs its own `marks` directory: the announcement file is
/// named by `at` alone.
struct Stalled(Child);

impl Stalled {
    fn start(src: &Path, dst: &Path, at: u32, marks: &Path) -> Self {
        let mut cmd = flux();
        cmd.arg("copy").arg(src).arg(dst);
        Self::spawn(cmd, at, marks)
    }

    fn start_cleanup(dst: &Path, flags: &[&str], at: u32, marks: &Path) -> Self {
        let mut cmd = flux();
        cmd.arg("cleanup").arg(dst).args(flags);
        Self::spawn(cmd, at, marks)
    }

    fn spawn(mut cmd: Command, at: u32, marks: &Path) -> Self {
        let announced = marks.join(format!("stalled-{at}"));
        let child = cmd
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

/// A file's bytes, or a placeholder when it cannot be read. A live copy can hold a file with a mandatory lock (on
/// Windows redb's `state.db` gives error 33), and the snapshot is the proof that nothing was deleted: an unreadable
/// file must stay in it (length and error kind), and one deleted since the listing must differ from what it was.
fn contents_or_placeholder(path: &Path) -> Vec<u8> {
    match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => b"<gone>".to_vec(),
        Err(e) => {
            let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            format!("<unreadable: {:?}, {len} bytes>", e.kind()).into_bytes()
        }
    }
}

/// Every file and directory under `root` (and `root` itself), keyed by its path relative to `root`; a file maps to
/// its bytes, a directory to `None`.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let path = e.path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if e.file_type().unwrap().is_dir() {
                out.insert(rel, None);
                walk(root, &path, out);
            } else {
                out.insert(rel, Some(contents_or_placeholder(&path)));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Every path under `root` whose name contains `.flux-partial.`.
fn partials_under(root: &Path) -> Vec<PathBuf> {
    snapshot(root)
        .into_keys()
        .filter(|p| p.file_name().unwrap().to_string_lossy().contains(".flux-partial."))
        .collect()
}

/// The single operation id in `dst/.flux/operations`.
fn only_operation(dst: &Path) -> String {
    let ids = names(&dst.join(".flux").join("operations"));
    assert_eq!(ids.len(), 1, "one workspace: {ids:?}");
    ids[0].clone()
}

/// Whether the report has a `removed <path>` action line (the summary's "0 removed" is not one).
fn has_removed(text: &str) -> bool {
    text.lines().any(|l| l.starts_with("removed "))
}

fn ago(secs: u64) -> SystemTime {
    SystemTime::now() - Duration::from_secs(secs)
}

fn set_mtime(path: &Path, when: SystemTime) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(when).unwrap();
}

/// Ages a killed copy past the 30 s lease: the manifest's mtime and the (dead) owner's lock record heartbeat. A
/// freshly killed copy is RESUMABLE-young, which even `--force` refuses to touch.
fn age_killed_copy(d: &Path, dst: &Path) {
    let id = only_operation(dst);
    set_mtime(&dst.join(".flux").join("operations").join(&id).join("manifest"), ago(3600));
    let record = age_lock(d);
    assert_eq!(record.operation_id, id, "the killed copy's lock record");
}

/// Rewrites the dead owner's lock record `dst.flux-lock` under `d` with a heartbeat long ago, so the 30 s lease gate
/// (which also guards taking over a dead cleanup's lock) passes. Returns the record as it was.
fn age_lock(d: &Path) -> LockRecord {
    let lock = d.join("dst.flux-lock");
    let bytes = std::fs::read(&lock).unwrap();
    let Decoded::Record(mut record) = decode(&bytes) else { panic!("a lock record") };
    let before = record.clone();
    record.last_heartbeat_wall_time = 1;
    std::fs::write(&lock, record.encode()).unwrap();
    before
}

/// A COMPLETED operation, `cleanup_pending`, naming `sub/b.flux-partial.<id>`, with that file present; returns the id.
fn completed_fixture(dst: &Path) -> String {
    let id = "a".repeat(32);
    let config = Config::new(
        vec![Root {
            source_root: native_hex(Path::new("/s")),
            source_identity: "strong:3:9".to_owned(),
            destination_prefix: String::new(),
        }],
        Options {
            preserve_times: "default".to_owned(),
            preserve_permissions: "default".to_owned(),
            durability: "normal".to_owned(),
            safety: "default".to_owned(),
            existing: "overwrite".to_owned(),
        },
    );
    let mut state = OperationState::created(&id, Kind::Tree, dst, 5, None, config);
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
    std::fs::write(dst.join("sub").join(format!("b.flux-partial.{id}")), b"half").unwrap();
    id
}

#[test]
fn a_killed_copy_is_listed_resumable_and_dry_run_changes_nothing() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    // `a`'s guarded mutations: its sweep (1), its temporary (2), its publish (3): killed with the temporary written.
    Stalled::start(&src, &dst, 3, marks.path()).kill();
    assert!(!partials_under(&dst).is_empty());

    // Just killed, its lease (30 s) has not run out: not even `--force` touches it.
    let young = snapshot(d.path());
    let out = cleanup(&dst, &["--force"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.lines().any(|l| {
            let t: Vec<&str> = l.split_whitespace().collect();
            t.starts_with(&["RESUMABLE", "no", "operation"])
        }),
        "{text}"
    );
    assert!(!has_removed(&text), "{text}");
    assert_eq!(snapshot(d.path()), young, "a young RESUMABLE operation survives --force");

    age_killed_copy(d.path(), &dst);
    let before = snapshot(d.path());

    let out = cleanup(&dst, &["--dry-run"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    let rows: Vec<&str> = text.lines().filter(|l| l.contains("RESUMABLE")).collect();
    assert_eq!(rows.len(), 1, "{text}");
    assert!(rows[0].contains("no"), "{text}");
    assert!(text.contains("cleanup (dry run): "), "{text}");
    assert_eq!(snapshot(d.path()), before, "a dry run changes nothing and takes no lock");

    let out = cleanup(&dst, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!has_removed(&stdout(&out)), "{}", stdout(&out));
    assert_eq!(snapshot(d.path()), before, "without --force a RESUMABLE operation is kept");

    let out = cleanup(&dst, &["--force"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(has_removed(&stdout(&out)), "{}", stdout(&out));
    assert!(!dst.join(".flux").exists(), "{:?}", snapshot(&dst));
    assert!(partials_under(&dst).is_empty());
    assert_eq!(names(d.path()), ["dst", "src"], "the dead copy's lock is gone too");
}

#[test]
fn a_hand_written_completed_record_with_a_leftover_is_finished_by_cleanup_and_by_copy() {
    // By cleanup.
    let d = TempDir::new().unwrap();
    let dst = d.path().join("dst");
    let id = completed_fixture(&dst);
    let out = cleanup(&dst, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("COMPLETED_BUT_UNCLEAN  yes"), "{text}");
    assert!(!dst.join("sub").join(format!("b.flux-partial.{id}")).exists(), "{text}");
    assert!(!dst.join(".flux").exists(), "{text}");

    // By the next copy, from the same fixture.
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let id = completed_fixture(&dst);
    let out = copy(&[os("--json"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(
        e.contains(&format!(
            "note: finished cleaning up the earlier operation {id} (1 leftovers removed)"
        )),
        "{e}"
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 18, "{json}");
    assert!(!dst.join("sub").join(format!("b.flux-partial.{id}")).exists());
    assert!(!dst.join(".flux").join("operations").join(&id).exists());
}

#[test]
fn a_cleanup_during_a_live_copy_lists_it_live_and_deletes_nothing() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let live = Stalled::start(&src, &dst, 3, marks.path());
    let before = snapshot(d.path());
    let out = cleanup(&dst, &["--force"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    let rows: Vec<&str> = text.lines().filter(|l| l.contains("LIVE")).collect();
    assert_eq!(rows.len(), 1, "{text}");
    assert!(!has_removed(&text), "{text}");
    assert_eq!(snapshot(d.path()), before, "a live copy's destination is untouched");

    live.kill();
    age_killed_copy(d.path(), &dst);
    let out = cleanup(&dst, &["--force"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(has_removed(&stdout(&out)), "{}", stdout(&out));
    assert!(!dst.join(".flux").exists());
    assert!(partials_under(&dst).is_empty());
    assert_eq!(names(d.path()), ["dst", "src"], "and the pass's own lock is released");
}

#[test]
fn a_cleanup_killed_after_abandoning_finishes_on_the_next_run() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let marks2 = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    Stalled::start(&src, &dst, 3, marks.path()).kill();
    age_killed_copy(d.path(), &dst);
    let id = only_operation(&dst);
    // The pass's guarded steps, in order: 1 the ABANDONED manifest write, 2 the partial's removal. Stalling at 2
    // leaves the manifest ABANDONED and the partial still on disk.
    Stalled::start_cleanup(&dst, &["--force"], 2, marks2.path()).kill();
    let manifest = dst.join(".flux").join("operations").join(&id).join("manifest");
    let text = String::from_utf8(std::fs::read(&manifest).unwrap()).unwrap();
    assert!(text.contains("ABANDONED"), "{text}");
    assert!(!partials_under(&dst).is_empty(), "the partial is still there");
    // The dead cleanup's own lock record is as young as the kill: the lease gate refuses to take the lock over
    // until it is 30 s old (measured: "skipped ...: destination lock busy"), so age it as the clock would.
    let dead = age_lock(d.path());
    assert_eq!(dead.workspace_path, "none", "a cleanup pass's record");

    let out = cleanup(&dst, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.lines().any(|l| {
            let t: Vec<&str> = l.split_whitespace().collect();
            t.starts_with(&["STALE", "yes", "operation"])
        }),
        "{text}"
    );
    assert!(has_removed(&text), "{text}");
    assert!(!dst.join(".flux").exists(), "{text}");
    assert!(partials_under(&dst).is_empty());
    assert_eq!(names(d.path()), ["dst", "src"], "no lock left behind");
}

#[test]
fn an_orphan_root_lock_blocks_a_copy_and_cleanup_reclaims_it() {
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
    assert!(stderr(&out).contains("flux cleanup"), "{}", stderr(&out));

    let out = cleanup(&dst, &["--dry-run"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.lines().any(|l| {
            let t: Vec<&str> = l.split_whitespace().collect();
            t.starts_with(&["STALE", "yes", "root-lock"])
        }),
        "{text}"
    );
    assert_eq!(std::fs::read(&lock).unwrap(), record, "a dry run leaves the lock alone");

    let out = cleanup(&dst, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(has_removed(&stdout(&out)), "{}", stdout(&out));
    let left = names(d.path());
    assert!(
        !left.iter().any(|n| n == "dst.flux-lock" || n.starts_with("dst.flux-lock.broken.")),
        "{left:?}"
    );
}

#[test]
fn exit_codes_and_json_shape() {
    let d = TempDir::new().unwrap();
    let dst = d.path().join("dst");
    let out = cleanup(&dst, &["--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert_eq!(text.lines().count(), 1, "{text}");
    // The JSON carries the destination as the CLI resolved it (`main.rs` passes the canonical path to
    // `cleanup_report::json`): macOS `/private/var/...`, Windows `\\?\C:\...`. Never the raw argument.
    let expected = flux_cli::resolve::canonical_with_remainder(&dst).unwrap();
    assert!(
        text.starts_with(&format!(
            "{{\"destination\":{},\"dry_run\":false,\"entries\":[],\"actions\":[],\"summary\":{{",
            serde_json::to_string(&expected.display().to_string()).unwrap()
        )),
        "{text}"
    );
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        json["summary"],
        serde_json::json!({"entries": 0, "eligible": 0, "removed": 0, "kept": 0, "skipped": 0})
    );

    // A file at the destination.
    let file = d.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let out = cleanup(&file, &[]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).starts_with("DESTINATION_ERROR"), "{}", stderr(&out));

    // `.flux` a file.
    let ns = d.path().join("ns");
    std::fs::create_dir(&ns).unwrap();
    std::fs::write(ns.join(".flux"), b"x").unwrap();
    let out = cleanup(&ns, &[]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("CONTROL_PLANE_NAMESPACE_CONFLICT"), "{}", stderr(&out));

    // No argument.
    let out = flux().arg("cleanup").output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
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
fn the_snapshot_keeps_an_unreadable_file_and_sees_its_deletion() {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new().unwrap();
    let f = d.path().join("locked");
    std::fs::write(&f, b"secret").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&f).is_ok() {
        println!("skipped: running as root, file permissions are not enforced");
        return;
    }
    let before = snapshot(d.path());
    assert!(before.contains_key(Path::new("locked")), "an unreadable file is still recorded");
    assert_eq!(before, snapshot(d.path()), "stable while nothing changes");
    std::fs::remove_file(&f).unwrap();
    assert_ne!(before, snapshot(d.path()), "its deletion is a difference");
}

#[cfg(unix)]
#[test]
fn an_unreadable_flux_dir_exits_1_and_still_lists_the_root_lock() {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new().unwrap();
    let dst = d.path().join("dst");
    completed_fixture(&dst);
    let flux_dir = dst.join(".flux");
    let _restore = Restore(flux_dir.clone());
    std::fs::set_permissions(&flux_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root ignores permissions: probe, and say so rather than fail.
    if std::fs::read_dir(&flux_dir).is_ok() {
        println!("skipped: running as root, directory permissions are not enforced");
        return;
    }
    let out = cleanup(&dst, &[]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("could not list"), "{}", stderr(&out));
    assert!(
        stdout(&out).starts_with("STATUS"),
        "the table header is still printed: {}",
        stdout(&out)
    );
}

#[cfg(unix)]
#[test]
fn a_leftover_that_cannot_be_removed_exits_1() {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new().unwrap();
    let dst = d.path().join("dst");
    let id = completed_fixture(&dst);
    let sub = dst.join("sub");
    let _restore = Restore(sub.clone());
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores directory permissions: probe, and say so rather than fail.
    if std::fs::write(sub.join("probe"), b"x").is_ok() {
        println!("skipped: running as root, directory permissions are not enforced");
        return;
    }
    let out = cleanup(&dst, &[]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.lines().any(|l| l.starts_with("kept ")), "{text}");
    assert!(dst.join(".flux").join("operations").join(&id).exists(), "the workspace stays");
    assert!(sub.join(format!("b.flux-partial.{id}")).exists());
}
