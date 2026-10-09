//! Commit recovery at `--resume` (cut 9c Task 7), end to end on the real filesystem with the real `flux` binary: a
//! Strict tree copy is killed at a publish guard with the debug build's stall hook (which exists only in a debug
//! build, what `cargo test` builds), and the next `--resume` decides each prepared note against what is on disk.

use flux_core::state::{identity_text, native_hex};
use flux_fs::{ClaimKey, FileSystem, FluxPathKey, PreparedRecord};
use flux_platform::StdFileSystem;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const PREPARED: TableDefinition<&[u8], &[u8]> = TableDefinition::new("prepared");

fn flux() -> Command {
    Command::new(env!("CARGO_BIN_EXE_flux"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
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

/// A `flux copy src dst` stopped just before its `at`-th guarded mutation, holding what it holds there. It is killed
/// when dropped.
struct Stalled(Child);

impl Stalled {
    /// `args` go before the paths. Each stalled run of one test needs its own `marks` directory: the announcement file
    /// is named by `at` alone.
    fn start_args(src: &Path, dst: &Path, at: u32, marks: &Path, args: &[&str]) -> Self {
        let announced = marks.join(format!("stalled-{at}"));
        let child = flux()
            .arg("copy")
            .args(args)
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

const BIG_FILES: usize = 300;

/// 300 files in three directories (`d0`..`d2`, 100 each) under `src`, and the path of a destination that does not
/// exist: every file is new, so a file is three guarded mutations (sweep, temporary, publish) and a directory one.
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

fn operation_dir(dst: &Path, id: &str) -> PathBuf {
    dst.join(".flux").join("operations").join(id)
}

/// The killed run's store (the ordinary open repairs a store its owner never closed).
fn store(dst: &Path, id: &str) -> Database {
    Database::open(operation_dir(dst, id).join("state.db")).expect("the killed run's store opens")
}

fn prepared_count(dst: &Path, id: &str) -> usize {
    let db = store(dst, id);
    let tx = db.begin_read().unwrap();
    tx.open_table(PREPARED).unwrap().len().unwrap() as usize
}

fn claim_count(dst: &Path, id: &str) -> usize {
    let db = store(dst, id);
    let tx = db.begin_read().unwrap();
    let claims = tx.open_table(TableDefinition::<&[u8], &[u8]>::new("claims")).unwrap();
    claims.len().unwrap() as usize
}

fn format_of(dst: &Path, id: &str) -> Option<u64> {
    let db = store(dst, id);
    let tx = db.begin_read().unwrap();
    let meta = tx.open_table(TableDefinition::<&str, u64>::new("meta")).unwrap();
    meta.get("format").unwrap().map(|g| g.value())
}

type Pairs = Vec<(Vec<u8>, Vec<u8>)>;

/// Every row of the store's three tables. The FILE's bytes cannot be compared: redb rewrites its header on every
/// open (measured: an open that only reads flips the primary-slot byte), so "changed nothing" is the tables' rows.
fn rows(dst: &Path, id: &str) -> (Pairs, Pairs, Vec<(String, u64)>) {
    let db = store(dst, id);
    let tx = db.begin_read().unwrap();
    let dump = |def: TableDefinition<&[u8], &[u8]>| -> Vec<(Vec<u8>, Vec<u8>)> {
        tx.open_table(def)
            .unwrap()
            .iter()
            .unwrap()
            .map(|r| {
                let (k, v) = r.unwrap();
                (k.value().to_vec(), v.value().to_vec())
            })
            .collect()
    };
    let claims = dump(TableDefinition::new("claims"));
    let prepared = dump(PREPARED);
    let meta = tx
        .open_table(TableDefinition::<&str, u64>::new("meta"))
        .unwrap()
        .iter()
        .unwrap()
        .map(|r| {
            let (k, v) = r.unwrap();
            (k.value().to_string(), v.value())
        })
        .collect();
    (claims, prepared, meta)
}

/// Every note in the store, decoded.
fn notes(dst: &Path, id: &str) -> Vec<(Vec<u8>, PreparedRecord)> {
    let db = store(dst, id);
    let tx = db.begin_read().unwrap();
    let table = tx.open_table(PREPARED).unwrap();
    table
        .iter()
        .unwrap()
        .map(|row| {
            let (k, v) = row.unwrap();
            (k.value().to_vec(), PreparedRecord::decode(v.value()).expect("a note decodes"))
        })
        .collect()
}

/// The identity of `path` as the engine compares it: the platform's own metadata.
fn identity_of(path: &Path) -> flux_fs::FileIdentity {
    StdFileSystem.metadata(path).unwrap().identity
}

fn strong_id(path: &Path) -> flux_fs::ObjectId {
    match identity_of(path) {
        flux_fs::FileIdentity::Strong(o) => o,
        other => panic!("{} has no strong identity here: {other:?}", path.display()),
    }
}

/// Insert, as a Strict publication's prepare step would have, a note for the published `dst/<dir>/<name>` with the
/// given identity text, keyed by the directory's real identity. The temporary is named for operation `id`.
fn note(dst: &Path, id: &str, dir: &str, name: &str, identity: &str) {
    let record = PreparedRecord {
        target: FluxPathKey::from_relative(&Path::new(dir).join(name)).unwrap(),
        temp_name: format!("{name}.flux-partial.{id}").into_bytes(),
        identity: identity.to_string(),
        dir_path: native_hex(Path::new(dir)),
        name: name.as_bytes().to_vec(),
        planned_name: Vec::new(),
        replacement: false,
    };
    let key = ClaimKey::new(strong_id(&dst.join(dir)), os(name));
    let db = store(dst, id);
    let tx = db.begin_write().unwrap();
    {
        let mut table = tx.open_table(PREPARED).unwrap();
        table.insert(key.encode().as_slice(), record.encode().as_slice()).unwrap();
    }
    tx.commit().unwrap();
}

fn same_bytes(src: &Path, dst: &Path) {
    fn all(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if rel == Path::new(".flux") {
                continue;
            }
            if path.is_dir() {
                all(root, &path, out);
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let (mut want, mut got) = (Default::default(), Default::default());
    all(src, src, &mut want);
    all(dst, dst, &mut got);
    assert_eq!(want.len(), 300);
    assert_eq!(want, got, "every file equals its source, and nothing else is there");
}

/// A Strict copy killed at stall `at`; returns the operation id.
fn killed_strict(src: &Path, dst: &Path, at: u32, marks: &Path) -> String {
    Stalled::start_args(src, dst, at, marks, &["--durability", "strict"]).kill();
    the_operation(dst)
}

/// Guard calls in `big_tree_in`: d0's create is #1, its 100 files #2..#301 at three guards each (sweep, create,
/// publish), its DirEnd flush #302, d1's create #303, d1/f000's sweep #304, create #305, publish #306. The stall
/// precedes the guarded mutation, so 306 is held with `d1/f000`'s temporary written and its note prepared, before the
/// rename.
const AT_D1_F000_PUBLISH: u32 = 306;
/// d1's create: d0 whole and its claims synced, d1 not begun.
const AT_D1_CREATE: u32 = 303;

#[test]
fn a_strict_copy_killed_at_a_publish_guard_leaves_a_note_that_resume_discards_and_redoes() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let (src, dst) = big_tree_in(d.path());
    let id = killed_strict(&src, &dst, AT_D1_F000_PUBLISH, marks.path());
    assert!(marks.path().join(format!("stalled-{AT_D1_F000_PUBLISH}")).exists());

    // Measured: the stall landed at d1/f000's publish guard (the index was right).
    assert_eq!(prepared_count(&dst, &id), 1, "the note is written before the publish");
    let temp = dst.join("d1").join(format!("f000.flux-partial.{id}"));
    assert!(temp.exists(), "the temporary is written: {:?}", names(&dst.join("d1")));
    assert!(!dst.join("d1").join("f000").exists(), "not yet published");

    let out =
        copy(&[os("--resume"), os("--durability"), os("strict"), src.as_os_str(), dst.as_os_str()]);
    let e = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{e}");
    assert!(!e.contains("recovered"), "nothing was renamed, so nothing is recovered: {e}");
    assert!(!dst.join(".flux").exists(), "the finished copy removes its workspace");
    same_bytes(&src, &dst);
}

#[test]
fn a_hand_written_uncertain_note_refuses_resume_with_exit_3_and_changes_nothing() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let (src, dst) = big_tree_in(d.path());
    let id = killed_strict(&src, &dst, AT_D1_CREATE, marks.path());
    assert!(dst.join("d0").join("f000").exists());
    // The temporary's identity is `unavailable`: nothing can show that d0/f000 is the object the note describes.
    note(&dst, &id, "d0", "f000", "unavailable");
    assert_eq!(prepared_count(&dst, &id), 1);
    let manifest = operation_dir(&dst, &id).join("manifest");
    let (rows_before, manifest_before) = (rows(&dst, &id), std::fs::read(&manifest).unwrap());
    assert_eq!(rows_before.1.len(), 1);

    let out =
        copy(&[os("--resume"), os("--durability"), os("strict"), src.as_os_str(), dst.as_os_str()]);
    let e = stderr(&out);
    assert_eq!(out.status.code(), Some(3), "{e}");
    assert!(e.contains("COMMIT_STATE_UNCERTAIN"), "{e}");
    assert!(e.contains("Move or delete the destination ENTRY you do not want"), "{e}");
    assert!(e.contains("or run again with --restart to supersede this operation"), "{e}");
    // The CLI canonicalizes DEST (macOS /var -> /private/var, Windows \\?\ prefix), so assert the tail that survives.
    let tail = Path::new("d0").join("f000").display().to_string();
    assert!(e.contains(&tail), "the file's path ends {tail}: {e}");
    assert_eq!(rows(&dst, &id), rows_before, "state.db holds the same rows (claims, notes, meta)");
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        manifest_before,
        "the manifest is byte-identical"
    );

    let out = copy(&[
        os("--restart"),
        os("--durability"),
        os("strict"),
        src.as_os_str(),
        dst.as_os_str(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!dst.join(".flux").exists());
    same_bytes(&src, &dst);
}

/// A record that does not decode is a corrupt store, not an undecided file: `--resume` refuses STATE_CORRUPT (exit 3),
/// changes nothing, and `--restart` is the way out (spec decision 5).
#[test]
fn a_note_that_does_not_decode_makes_resume_refuse_state_corrupt_and_change_nothing() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let (src, dst) = big_tree_in(d.path());
    let id = killed_strict(&src, &dst, AT_D1_CREATE, marks.path());
    // Version byte 9: no build of this format reads it.
    let key = ClaimKey::new(strong_id(&dst.join("d0")), os("zzz"));
    {
        let db = store(&dst, &id);
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx.open_table(PREPARED).unwrap();
            table.insert(key.encode().as_slice(), [9u8, 0, 0, 0, 0].as_slice()).unwrap();
        }
        tx.commit().unwrap();
    }
    assert_eq!(prepared_count(&dst, &id), 1);
    let manifest = operation_dir(&dst, &id).join("manifest");
    let (rows_before, manifest_before) = (rows(&dst, &id), std::fs::read(&manifest).unwrap());

    let out =
        copy(&[os("--resume"), os("--durability"), os("strict"), src.as_os_str(), dst.as_os_str()]);
    let e = stderr(&out);
    assert_eq!(out.status.code(), Some(3), "{e}");
    assert!(e.contains("STATE_CORRUPT"), "{e}");
    assert!(!e.contains("COMMIT_STATE_UNCERTAIN"), "{e}");
    assert_eq!(rows(&dst, &id), rows_before, "state.db holds the same rows");
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        manifest_before,
        "the manifest is byte-identical"
    );
}

/// The crash after the rename and before the claim is durable: the engine's own note stays, and the temporary has
/// been renamed to the entry. (Reproduced by renaming the held temporary by hand. The note is the one the engine
/// wrote, so this is a true writer-to-validator round trip: its identity is checked against the published file's.)
#[test]
fn a_renamed_publication_is_recovered_and_reported() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let (src, dst) = big_tree_in(d.path());
    let id = killed_strict(&src, &dst, AT_D1_F000_PUBLISH, marks.path());
    let entry = dst.join("d1").join("f000");
    std::fs::rename(dst.join("d1").join(format!("f000.flux-partial.{id}")), &entry).unwrap();

    // The note holds the file's real identity, encoded as the engine does.
    let engine_note = notes(&dst, &id).remove(0).1;
    assert_eq!(engine_note.identity, identity_text(identity_of(&entry)));
    assert!(engine_note.identity.starts_with("strong:"), "{}", engine_note.identity);
    assert_eq!(prepared_count(&dst, &id), 1);

    let out = copy(&[
        os("--resume"),
        os("--json"),
        os("--durability"),
        os("strict"),
        src.as_os_str(),
        dst.as_os_str(),
    ]);
    let e = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{e}");
    assert!(e.contains("recovered 1 interrupted publications"), "{e}");
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|x| panic!("{x}: {e}"));
    // d0 (100 files) and d1/f000 are skipped as resumed: the recovered file is counted among them.
    assert!(json["files_skipped"].as_u64().unwrap() >= 101, "{json}");
    assert!(!dst.join(".flux").exists(), "the finished copy removes its workspace");
    same_bytes(&src, &dst);
}

#[test]
fn a_normal_copy_never_writes_a_note() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let (src, dst) = big_tree_in(d.path());
    Stalled::start_args(&src, &dst, AT_D1_F000_PUBLISH, marks.path(), &[]).kill();
    let id = the_operation(&dst);
    assert!(
        dst.join("d1").join(format!("f000.flux-partial.{id}")).exists(),
        "stalled at the same point"
    );
    assert_eq!(prepared_count(&dst, &id), 0);
    assert_eq!(format_of(&dst, &id), Some(2));
    assert!(claim_count(&dst, &id) <= 300);
}

#[test]
fn a_strict_copy_writes_format_2() {
    // The refusal of an older binary is pinned at the unit level by the format-3 test; here: this binary WRITES 2.
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let (src, dst) = big_tree_in(d.path());
    let id = killed_strict(&src, &dst, AT_D1_CREATE, marks.path());
    assert_eq!(format_of(&dst, &id), Some(2));
}
