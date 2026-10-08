//! `flux copy` under the destination's lock (cut 7a Part 3b-2): the design's Testing item 4, end to end on the real
//! filesystem. The crash hook exists only in a debug build, which is what `cargo test` builds.

use flux_core::lock::record::{Decoded, LockRecord, decode};
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
        Self::start_env(src, dst, at, marks, &[])
    }

    /// `start`, with extra environment variables for the run.
    fn start_env(src: &Path, dst: &Path, at: u32, marks: &Path, env: &[(&str, &str)]) -> Self {
        Self::start_with(src, dst, at, marks, env, &[])
    }

    /// `start`, with extra arguments before the paths (`--resume`). Each stalled run of one test needs its own
    /// `marks` directory: the announcement file is named by `at` alone.
    fn start_args(src: &Path, dst: &Path, at: u32, marks: &Path, args: &[&str]) -> Self {
        Self::start_with(src, dst, at, marks, &[], args)
    }

    fn start_with(
        src: &Path,
        dst: &Path,
        at: u32,
        marks: &Path,
        env: &[(&str, &str)],
        args: &[&str],
    ) -> Self {
        let announced = marks.join(format!("stalled-{at}"));
        let child = flux()
            .arg("copy")
            .args(args)
            .arg(src)
            .arg(dst)
            .env("FLUX_TEST_STALL_AT", at.to_string())
            .env("FLUX_TEST_STALL_FILE", &announced)
            .envs(env.iter().copied())
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
    assert!(e.contains("--resume to continue it"), "{e}");
    let out = copy(&[os("--restart"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(names(&dst), ["a", "sub"], "the killed run's temporary and state are gone");
    assert_eq!(names(d.path()), ["dst", "src"], "and its lock");
}

/// Cut 8b, "Claim syncing": a run killed mid-tree leaves `<id>/state.db` holding the claims up to the last sync (a
/// directory end), every record decodable and no key twice. 300 files in three directories, each already present at
/// the destination, so every file is a replacement and so a claim. The stall is placed well inside the second
/// directory, so the first has ended (its claims synced) and the second has claimed some files that were never synced.
#[test]
fn a_killed_run_leaves_the_claims_up_to_the_last_sync() {
    use flux_fs::{ClaimRecord, ClaimStatus};
    use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

    const FILES: usize = 300;
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = d.path().join("src");
    let dst = d.path().join("dst");
    for tree in [&src, &dst] {
        for dir in 0..3 {
            let sub = tree.join(format!("d{dir}"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..FILES / 3 {
                let body: &[u8] = if tree == &src { b"new" } else { b"old" };
                std::fs::write(sub.join(format!("f{f:03}")), body).unwrap();
            }
        }
    }
    // Three guarded mutations per replaced file (sweep, temporary, publish): the 500th falls in the second directory.
    Stalled::start(&src, &dst, 500, marks.path()).kill();

    let ops = dst.join(".flux").join("operations");
    let ids = names(&ops);
    assert_eq!(ids.len(), 1, "one kept workspace: {ids:?}");
    let db_path = ops.join(&ids[0]).join("state.db");
    assert!(db_path.is_file(), "the killed run kept its claim store at {}", db_path.display());

    // `redb`'s read-only open refuses a store its owner never closed (`RepairAborted`, measured); the ordinary open
    // repairs it, which is also what a later run would have to do.
    let db = Database::open(&db_path).expect("the killed run's store opens");
    let tx = db.begin_read().unwrap();
    let claims = tx.open_table(TableDefinition::<&[u8], &[u8]>::new("claims")).unwrap();
    let count = claims.len().unwrap() as usize;
    // Lower bound: each directory holds FILES / 3 = 100 replaced files, so the first directory keyed 100 claims, and
    // its DirEnd flush (a synced commit) ran before the stall at the 500th guarded mutation (300 mutations for the
    // first directory, the 500th is inside the second). Upper bound: the second directory's claims are unsynced and
    // may be lost, but never more than all FILES exist.
    assert!(
        (FILES / 3..=FILES).contains(&count),
        "claims survived: {count}, want at least the first directory's {}",
        FILES / 3
    );
    let mut seen = std::collections::BTreeSet::new();
    for row in claims.iter().unwrap() {
        let (k, v) = row.unwrap();
        let record = ClaimRecord::decode(v.value()).expect("every record decodes");
        assert!(
            matches!(record.status, ClaimStatus::Existing | ClaimStatus::Created),
            "{:?}",
            record.status
        );
        assert!(seen.insert(k.value().to_vec()), "a key repeated: {:?}", k.value());
    }
    assert_eq!(seen.len(), count);
    let meta = tx.open_table(TableDefinition::<&str, u64>::new("meta")).unwrap();
    assert_eq!(meta.get("format").unwrap().map(|g| g.value()), Some(1));
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
    // Cut 7b: the record a REAL run left is the current format and carries real identities (test audit A4).
    let record = left.iter().find(|n| n.starts_with("t.flux-state.")).unwrap();
    let state = flux_core::state::decode(&std::fs::read(d.path().join(record)).unwrap())
        .expect("the record decodes");
    assert_eq!(state.format_version, flux_core::state::FORMAT_VERSION);
    let f = state.file.expect("a single-file record carries §249.1's fields");
    assert!(
        matches!(
            flux_core::state::parse_identity(&f.source_identity),
            Some(flux_fs::FileIdentity::Strong(_))
        ),
        "a real source's identity: {}",
        f.source_identity
    );
    assert_eq!(f.target_identity, None, "no object stood at the target");
    let out = copy(&[src.join("a").as_os_str(), t.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("RESUMABLE_OPERATION_EXISTS"), "{}", stderr(&out));
    let out = copy(&[os("--restart"), src.join("a").as_os_str(), t.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(names(d.path()), ["src", "t"], "the record, the partial and the lock are gone");
    assert_eq!(std::fs::read(&t).unwrap(), b"A");
}

/// The lock record a stalled run holds at `lock`.
fn held_record(lock: &Path) -> LockRecord {
    match decode(&std::fs::read(lock).unwrap()) {
        Decoded::Record(r) => r,
        other => panic!("expected a record, got {other:?}"),
    }
}

#[test]
fn a_running_copy_moves_its_lock_records_heartbeat() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    // Interval 0: `a`'s heartbeat runs before its first guarded mutation, where the run stalls.
    let s = Stalled::start_env(
        &src,
        &dst,
        1,
        marks.path(),
        &[("FLUX_TEST_HEARTBEAT_INTERVAL_MS", "0")],
    );
    let r = held_record(&d.path().join("dst.flux-lock"));
    assert!(r.last_heartbeat_wall_time > r.creation_wall_time, "{r:?}");
    s.kill();
}

#[test]
fn without_the_override_a_short_run_keeps_its_creation_heartbeat() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    // 5 s have not passed by the first guarded mutation.
    let s = Stalled::start(&src, &dst, 1, marks.path());
    let r = held_record(&d.path().join("dst.flux-lock"));
    assert_eq!(r.last_heartbeat_wall_time, r.creation_wall_time, "{r:?}");
    s.kill();
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
    let newer: &[u8] = br#"{"format_version":4}"#;
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

// ---- Cut 9a: kill and resume, end to end -------------------------------------------------------------------------

/// Every file under `dir` (relative path with `/` separators -> bytes), skipping the top-level `.flux`.
fn files_under(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let path = e.path();
            let rel = path.strip_prefix(root).unwrap();
            if rel == Path::new(".flux") {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            } else {
                let key: Vec<_> = rel.iter().map(|c| c.to_string_lossy().into_owned()).collect();
                out.insert(key.join("/"), std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// `dst` holds every file of `src` with equal bytes and no other file outside `.flux`.
fn same_bytes(src: &Path, dst: &Path) {
    let (want, got) = (files_under(src), files_under(dst));
    assert_eq!(want.len(), got.len(), "the same number of files");
    for (k, v) in &want {
        assert_eq!(got.get(k), Some(v), "{k} differs or is missing");
    }
}

/// Every file under `dir` including `.flux`, for a "nothing changed" comparison.
fn snapshot(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

const BIG_FILES: usize = 300;

/// 300 files in three directories (`d0`..`d2`, 100 each) under `src`, and the path of a destination that does not
/// exist: every file is new, so a file is still three guarded mutations (sweep, temporary, publish) and a
/// directory one, as in `a_killed_run_leaves_the_claims_up_to_the_last_sync`.
fn big_tree_in(d: &Path) -> (PathBuf, PathBuf) {
    let src = d.join("src");
    for dir in 0..3 {
        let sub = src.join(format!("d{dir}"));
        std::fs::create_dir_all(&sub).unwrap();
        for f in 0..BIG_FILES / 3 {
            std::fs::write(sub.join(format!("f{f:03}")), format!("body {dir} {f}")).unwrap();
        }
    }
    (src, d.join("dst"))
}

/// The one operation id under `dst/.flux/operations`.
fn the_operation(dst: &Path) -> String {
    let ids = names(&dst.join(".flux").join("operations"));
    assert_eq!(ids.len(), 1, "one kept workspace: {ids:?}");
    ids.into_iter().next().unwrap()
}

/// The claims in the killed run's store (the ordinary open repairs a store its owner never closed).
fn claim_count(dst: &Path, id: &str) -> usize {
    use redb::{Database, ReadableDatabase, ReadableTableMetadata, TableDefinition};
    let db_path = dst.join(".flux").join("operations").join(id).join("state.db");
    let db = Database::open(&db_path).expect("the killed run's store opens");
    let tx = db.begin_read().unwrap();
    let claims = tx.open_table(TableDefinition::<&[u8], &[u8]>::new("claims")).unwrap();
    claims.len().unwrap() as usize
}

fn manifest_of(dst: &Path, id: &str) -> flux_core::state::OperationState {
    let path = dst.join(".flux").join("operations").join(id).join("manifest");
    flux_core::state::decode(&std::fs::read(path).unwrap()).expect("the manifest decodes")
}

/// Files published at `dst` (not temporaries), counted to prove where a stall landed.
fn published(dst: &Path) -> usize {
    files_under(dst).keys().filter(|k| !k.contains(".flux-partial.")).count()
}

fn json_of(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", stderr(out)))
}

#[test]
fn a_killed_tree_copy_resumes_as_the_same_operation() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let (src, dst) = big_tree_in(d.path());
    // 1 + 3 * 100 mutations for d0 and its end, then the second directory: the 500th is inside it.
    Stalled::start(&src, &dst, 500, marks.path()).kill();
    let id = the_operation(&dst);
    // The stall landed in the second directory: the first is whole, the third not begun.
    let done = published(&dst);
    assert!((100..200).contains(&done), "the stall landed in d1: {done} files published");
    assert!(claim_count(&dst, &id) >= 100, "the first directory's claims were synced at its end");

    let out = copy(&[os("--resume"), os("--json"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    let note = e.find(&format!("resuming operation {id} (")).unwrap_or_else(|| panic!("{e}"));
    let resumed = e.find("resumed ").unwrap_or_else(|| panic!("{e}"));
    assert!(e.contains(" already-complete files"), "{e}");
    let summary = e.find("copied ").unwrap_or_else(|| panic!("{e}"));
    assert!(note < resumed && resumed < summary, "note, then resumed, then the summary: {e}");
    let v = json_of(&out);
    assert_eq!(v["files_total"].as_u64(), Some(300), "{v}");
    let (copied, skipped) =
        (v["files_copied"].as_u64().unwrap(), v["files_skipped"].as_u64().unwrap());
    assert_eq!(copied + skipped, 300, "{v}");
    assert!(skipped >= 100, "the first directory was not copied again: {v}");
    assert!(copied <= 200, "{v}");
    same_bytes(&src, &dst);
    assert!(!dst.join(".flux").exists(), "the workspace is gone");
    assert!(!d.path().join("dst.flux-lock").exists(), "and the lock");
}

#[test]
fn a_resume_killed_again_resumes_a_third_time() {
    let d = TempDir::new().unwrap();
    let (marks1, marks2) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src, dst) = big_tree_in(d.path());
    Stalled::start(&src, &dst, 500, marks1.path()).kill();
    let id = the_operation(&dst);
    // The resume skips the first directory (and any claimed file) and stalls 200 mutations into what is left.
    Stalled::start_args(&src, &dst, 200, marks2.path(), &["--resume"]).kill();
    assert_eq!(the_operation(&dst), id, "the same operation, not a new one");
    assert!(claim_count(&dst, &id) >= 100, "the claims survived both kills");
    let m = manifest_of(&dst, &id);
    assert_eq!(m.format_version, 3);
    assert_eq!(m.state, flux_core::state::OpState::Transferring);
    assert!(published(&dst) > 100, "the second run made progress past the first directory");

    let out = copy(&[os("--resume"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stderr(&out).contains(&format!("resuming operation {id} (")), "{}", stderr(&out));
    same_bytes(&src, &dst);
    assert!(!dst.join(".flux").exists());
    assert!(!d.path().join("dst.flux-lock").exists());
}

#[test]
fn resume_against_an_incompatible_option_exits_3_and_changes_nothing() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let (src, dst) = big_tree_in(d.path());
    Stalled::start(&src, &dst, 500, marks.path()).kill();
    let id = the_operation(&dst);
    let manifest = dst.join(".flux").join("operations").join(&id).join("manifest");
    let before_manifest = std::fs::read(&manifest).unwrap();
    let before = snapshot(d.path());
    let lock = d.path().join("dst.flux-lock");
    assert!(lock.is_file(), "the killed run's lock");

    let out = copy(&[os("--resume"), os("--skip-existing"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("INCOMPATIBLE_STATE") && e.contains("existing:"), "{e}");
    assert_eq!(std::fs::read(&manifest).unwrap(), before_manifest, "the manifest is untouched");
    let mut after = snapshot(d.path());
    // The destination lock is the one thing outside the workspace that moves: the spec's step 3 takes it over from
    // the dead owner before the options are compared, and the refusal then releases it (measured: it is gone).
    assert!(!lock.exists(), "the refusal released the lock it took over");
    let mut before = before;
    before.remove("dst.flux-lock");
    after.remove("dst.flux-lock");
    let changed: Vec<_> =
        before.keys().chain(after.keys()).filter(|k| before.get(*k) != after.get(*k)).collect();
    assert!(changed.is_empty(), "changed: {changed:?}");

    let out = copy(&[os("--resume"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    same_bytes(&src, &dst);
}

#[test]
fn resume_with_nothing_to_resume_starts_new() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let out = copy(&[os("--resume"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    let note = e.find("no prior operation; starting new").unwrap_or_else(|| panic!("{e}"));
    let summary = e.find("copied ").unwrap_or_else(|| panic!("{e}"));
    assert!(note < summary, "the note comes before the summary: {e}");
    same_bytes(&src, &dst);
}

#[test]
fn resume_and_restart_together_is_a_usage_error() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let out = copy(&[os("--resume"), os("--restart"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(!dst.exists());
}

#[test]
fn a_killed_single_file_copy_resumes_keeping_its_id_and_bumping_its_generation() {
    let d = TempDir::new().unwrap();
    let (m1, m2) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let src = tree_in(d.path());
    let t = d.path().join("t");
    let record_of = |d: &Path| -> (String, flux_core::state::OperationState) {
        let recs: Vec<String> =
            names(d).into_iter().filter(|n| n.starts_with("t.flux-state.")).collect();
        assert_eq!(recs.len(), 1, "exactly one record: {recs:?}");
        let state = flux_core::state::decode(&std::fs::read(d.join(&recs[0])).unwrap())
            .expect("the record decodes");
        (recs[0].clone(), state)
    };

    // Killed with the temporary written (sweep 1, temporary 2, publish 3).
    Stalled::start(&src.join("a"), &t, 3, m1.path()).kill();
    let (_, first) = record_of(d.path());
    let first_file = first.file.clone().expect("a single-file record");
    assert_eq!(first_file.artifact_generation, 1);

    // The resume stalls at 2: after its sweep removed the old partial, before the new one is written.
    Stalled::start_args(&src.join("a"), &t, 2, m2.path(), &["--resume"]).kill();
    let (name, second) = record_of(d.path());
    assert_eq!(second.operation_id, first.operation_id, "the same operation");
    assert!(name.ends_with(&first.operation_id), "{name}");
    let second_file = second.file.clone().expect("a single-file record");
    assert_eq!(second_file.artifact_generation, 2);
    assert_ne!(second_file.attempt_id, first_file.attempt_id);
    assert_eq!(second.state, flux_core::state::OpState::Transferring);
    let left = names(d.path());
    assert!(!left.iter().any(|n| n.starts_with("t.flux-partial.")), "{left:?}");

    let out = copy(&[os("--resume"), src.join("a").as_os_str(), t.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    let note = e
        .find(&format!("resuming operation {}", first.operation_id))
        .unwrap_or_else(|| panic!("{e}"));
    assert!(!e.contains("entries claimed"), "a single file has no count: {e}");
    let summary = e.find("copied ").unwrap_or_else(|| panic!("{e}"));
    assert!(note < summary, "the note comes before the summary: {e}");
    assert_eq!(std::fs::read(&t).unwrap(), b"A");
    assert_eq!(names(d.path()), ["src", "t"]);
}
