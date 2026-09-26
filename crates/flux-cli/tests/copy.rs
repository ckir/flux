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

/// §53's complete field list.
const KEYS: [&str; 18] = [
    "files_total",
    "files_copied",
    "files_skipped",
    "files_overwritten",
    "files_hardlinked",
    "files_reflinked",
    "files_degraded",
    "files_verified",
    "files_mismatched",
    "files_failed",
    "bytes_total",
    "bytes_copied",
    "bytes_skipped",
    "errors",
    "duration_ms",
    "average_bytes_per_second",
    "verify_level",
    "hash_algorithm",
];

#[test]
fn it_copies_a_single_file() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&src, b"hello").unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(&dst).unwrap(), b"hello");

    // §44.1 is the reason this cut exists; assert it against a real filesystem, not
    // only through the fake's call ordering.
    let (sm, dm) = (std::fs::metadata(&src).unwrap(), std::fs::metadata(&dst).unwrap());
    assert_eq!(dm.modified().unwrap(), sm.modified().unwrap(), "mtime must be preserved");
}

#[test]
fn a_successful_copy_consumes_its_temporary() {
    // Named for what it proves. On the happy path the publishing rename consumes the
    // temporary, so this does NOT exercise the failure-path cleanup -- that is
    // `a_temporary_that_cannot_be_removed_is_named_in_the_error` and the several
    // `!fs.exists("/dst.flux-partial.op1")` assertions in `flux-core`, which can
    // inject the failures a real filesystem will not produce on demand.
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&src, b"hello").unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();
    // Without this the test passes even when the binary fails outright, since a run
    // that never starts leaves no temporary either.
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let leftovers: Vec<_> = std::fs::read_dir(d.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("flux-partial"))
        .collect();
    assert!(leftovers.is_empty(), "found {leftovers:?}");
}

#[test]
fn a_copy_onto_a_hardlink_of_its_own_source_is_refused() {
    // The live defect 4a fixes: two names, one object. Before the gate, the file was
    // copied onto itself.
    let d = TempDir::new().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&a, b"hello").unwrap();
    std::fs::hard_link(&a, &b).unwrap();

    let out = flux().arg("copy").arg(&a).arg(&b).output().unwrap();

    assert_eq!(out.status.code(), Some(3), "refused before anything changed (§55)");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("SAFETY_REJECTED"), "stderr: {stderr}");
    assert_eq!(std::fs::read(&b).unwrap(), b"hello");
    assert_eq!(std::fs::read(&a).unwrap(), b"hello");
}

#[test]
fn a_copy_onto_its_own_source_by_another_spelling_is_refused() {
    // `..` is not normalized away by Path comparison, so the two spellings differ
    // lexically and only identity can tell they are one file.
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("sub")).unwrap();
    let a = d.path().join("a");
    let same = d.path().join("sub").join("..").join("a");
    std::fs::write(&a, b"hello").unwrap();
    assert_ne!(a, same, "the fixture must be lexically different");

    let out = flux().arg("copy").arg(&a).arg(&same).output().unwrap();

    assert_eq!(out.status.code(), Some(3), "refused before anything changed (§55)");
    assert!(String::from_utf8_lossy(&out.stderr).contains("SAFETY_REJECTED"));
    assert_eq!(std::fs::read(&a).unwrap(), b"hello");
}

#[test]
fn a_copy_between_bare_names_writes_into_the_working_directory() {
    // The one real-filesystem path through `split_destination`'s "empty parent
    // becomes ." rule. `current_dir` sets the CHILD's working directory only, so this
    // is safe under parallel tests.
    let d = TempDir::new().unwrap();
    std::fs::write(d.path().join("a"), b"hello").unwrap();

    let out = flux().current_dir(d.path()).arg("copy").arg("a").arg("b").output().unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(d.path().join("b")).unwrap(), b"hello");
}

#[test]
fn a_folders_contents_are_copied_onto_a_fresh_destination() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"A");
    assert_eq!(std::fs::read(dst.join("sub").join("b")).unwrap(), b"BB");
    assert!(!dst.join("src").exists(), "§4.1: the CONTENTS land on DEST");
    assert!(
        stderr(&out).contains("copied 2 files (3 bytes), created 2 directories; 0 failed"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_folder_merges_into_an_existing_folder_and_reports_each_collision() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::create_dir(&dst).unwrap();
    std::fs::write(dst.join("a"), b"old").unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.lines().any(|l| l.starts_with("DESTINATION_NAMESPACE_COLLISION: a: ")), "{err}");
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"old", "never replaced");
    assert_eq!(
        std::fs::read(dst.join("sub").join("b")).unwrap(),
        b"BB",
        "the walk continued past the collision"
    );
}

#[test]
fn a_folder_onto_an_existing_file_is_a_usage_error() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("f");
    std::fs::write(&dst, b"x").unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stderr(&out).starts_with("error: "), "{}", stderr(&out));
    assert_eq!(std::fs::read(&dst).unwrap(), b"x");
    assert!(out.stdout.is_empty(), "no JSON on a usage error");
}

#[test]
fn a_destination_inside_the_source_is_refused_with_exit_3() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());

    let out = flux().arg("copy").arg(&src).arg(src.join("backup")).output().unwrap();

    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("SAFETY_REJECTED"), "{}", stderr(&out));
    assert!(!src.join("backup").exists());
}

#[test]
fn a_file_into_an_existing_folder_lands_under_its_own_name() {
    let d = TempDir::new().unwrap();
    std::fs::write(d.path().join("a"), b"hello").unwrap();
    std::fs::create_dir(d.path().join("out")).unwrap();

    let out =
        flux().arg("copy").arg(d.path().join("a")).arg(d.path().join("out")).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(std::fs::read(d.path().join("out").join("a")).unwrap(), b"hello");
}

#[test]
fn a_file_to_a_path_ending_in_a_separator_lands_under_its_own_name() {
    let d = TempDir::new().unwrap();
    std::fs::write(d.path().join("a"), b"hello").unwrap();
    std::fs::create_dir(d.path().join("out")).unwrap();
    let mut dest = d.path().join("out").into_os_string();
    dest.push("/");

    let out = flux().arg("copy").arg(d.path().join("a")).arg(&dest).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(std::fs::read(d.path().join("out").join("a")).unwrap(), b"hello");
}

#[test]
fn a_file_to_a_separator_path_whose_folder_is_missing_fails() {
    let d = TempDir::new().unwrap();
    std::fs::write(d.path().join("a"), b"hello").unwrap();
    let mut dest = d.path().join("missing").into_os_string();
    dest.push("/");

    let out = flux().arg("copy").arg(d.path().join("a")).arg(&dest).output().unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(!d.path().join("missing").exists(), "§4.1 does not create it");
}

#[test]
fn json_reports_every_section_53_field() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());

    let out =
        flux().arg("copy").arg(&src).arg(d.path().join("dst")).arg("--json").output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = json(&out);
    assert_eq!(v.as_object().unwrap().len(), KEYS.len());
    for k in KEYS {
        assert!(v.get(k).is_some(), "missing {k}");
    }
    assert_eq!((v["files_total"].as_u64(), v["files_copied"].as_u64()), (Some(2), Some(2)));
    assert_eq!((v["bytes_copied"].as_u64(), v["bytes_total"].as_u64()), (Some(3), Some(3)));
    assert_eq!(v["errors"].as_u64(), Some(0));
    assert_eq!(v["verify_level"], "none");
    assert_eq!(v["hash_algorithm"], "blake3");
}

#[test]
fn json_is_printed_when_the_copy_fails() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::create_dir(&dst).unwrap();
    std::fs::write(dst.join("a"), b"old").unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).arg("--json").output().unwrap();

    assert_eq!(out.status.code(), Some(1));
    let v = json(&out);
    assert_eq!((v["errors"].as_u64(), v["files_failed"].as_u64()), (Some(1), Some(1)));
    assert_eq!(v["files_copied"].as_u64(), Some(1));
}

#[test]
fn a_missing_source_exits_1_and_counts_one_error() {
    let d = TempDir::new().unwrap();
    let dst = d.path().join("dst");

    let out =
        flux().arg("copy").arg(d.path().join("nope")).arg(&dst).arg("--json").output().unwrap();

    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("nope"), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!((v["errors"].as_u64(), v["files_total"].as_u64()), (Some(1), Some(0)));
    assert!(!dst.exists());
}

#[test]
fn a_third_positional_is_a_usage_error() {
    let out = flux().args(["copy", "a", "b", "c"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[cfg(unix)]
#[test]
fn a_fifo_in_a_folder_is_skipped_reported_and_exits_0() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let made = Command::new("mkfifo").arg(src.join("pipe")).status().unwrap();
    assert!(made.success(), "this test needs mkfifo");
    let dst = d.path().join("dst");

    let out = flux().arg("copy").arg(&src).arg(&dst).arg("--json").output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out)
            .lines()
            .any(|l| l == "SPECIAL_FILE_UNSUPPORTED: pipe: object_type=special action=skipped"),
        "{}",
        stderr(&out)
    );
    assert!(std::fs::symlink_metadata(dst.join("pipe")).is_err());
    let v = json(&out);
    assert_eq!((v["files_skipped"].as_u64(), v["files_total"].as_u64()), (Some(1), Some(3)));
}

#[cfg(unix)]
#[test]
fn a_symlink_in_a_folder_fails_the_run_with_exit_1() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    std::os::unix::fs::symlink("a", src.join("link")).unwrap();
    let dst = d.path().join("dst");

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).lines().any(|l| l.starts_with("SYMLINK_CREATION_UNAVAILABLE: link: ")),
        "{}",
        stderr(&out)
    );
    assert!(std::fs::symlink_metadata(dst.join("link")).is_err());
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"A");
}

#[cfg(unix)]
#[test]
fn a_symlink_given_as_source_is_never_followed() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    std::os::unix::fs::symlink(&src, d.path().join("to-dir")).unwrap();
    std::os::unix::fs::symlink(src.join("a"), d.path().join("to-file")).unwrap();
    std::os::unix::fs::symlink(d.path().join("nowhere"), d.path().join("dangling")).unwrap();

    for link in ["to-dir", "to-file", "dangling"] {
        let dst = d.path().join(format!("dst-{link}"));
        let out =
            flux().arg("copy").arg(d.path().join(link)).arg(&dst).arg("--json").output().unwrap();

        assert_eq!(out.status.code(), Some(1), "{link}: {}", stderr(&out));
        // K7: exactly one line, the whole explanation, and no summary.
        assert_eq!(
            stderr(&out),
            format!(
                "SYMLINK_CREATION_UNAVAILABLE: {}: a symlink is copied as a link (§25), which this version cannot create; name its target to copy what it points at
",
                d.path().join(link).display()
            ),
            "{link}"
        );
        assert!(std::fs::symlink_metadata(&dst).is_err(), "{link}: nothing was created");
        let v = json(&out);
        assert_eq!(
            (v["files_total"].as_u64(), v["files_failed"].as_u64(), v["errors"].as_u64()),
            (Some(1), Some(1), Some(1)),
            "{link}"
        );
    }
}

/// Linux only: the exit code rests on `open_dir` answering ENOTDIR for a link (see the
/// plan's refinement 3); macOS was not measured.
#[cfg(target_os = "linux")]
#[test]
fn a_dangling_destination_link_is_refused_with_exit_3() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::os::unix::fs::symlink(d.path().join("nowhere"), &dst).unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("SAFETY_REJECTED"), "{}", stderr(&out));
    assert!(std::fs::symlink_metadata(&dst).unwrap().file_type().is_symlink());
    assert!(!d.path().join("nowhere").exists(), "nothing written through the link");
}
