// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! `--quiet` end to end: only the cause of a non-zero exit, warnings, a dry run's output and `--json` are printed
//! (design 5.3). The warning half is a unit test in `main.rs` (`quiet_keeps_warnings`): a warning cannot be forced
//! from a process test on a temporary filesystem.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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

fn code(out: &Output) -> i32 {
    out.status.code().expect("exited by signal")
}

/// `src/a` (1 byte) and `src/sub/b` (2 bytes) under `d`.
fn tree_in(d: &Path) -> PathBuf {
    let src = d.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a"), b"A").unwrap();
    std::fs::write(src.join("sub").join("b"), b"BB").unwrap();
    src
}

/// `tree_in(d)` plus an identical `d/dst`; returns `(src, dst)`.
fn pair_in(d: &Path) -> (PathBuf, PathBuf) {
    let src = tree_in(d);
    let dst = d.join("dst");
    std::fs::create_dir_all(dst.join("sub")).unwrap();
    std::fs::write(dst.join("a"), b"A").unwrap();
    std::fs::write(dst.join("sub").join("b"), b"BB").unwrap();
    (src, dst)
}

/// Makes `path` unreadable; false (the caller returns early) when this process can read it anyway (root).
#[cfg(unix)]
fn make_unreadable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::File::open(path).is_err()
}

#[test]
fn quiet_clean_copy_prints_nothing() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let out = flux().args(["copy", "--quiet"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{}", stderr(&out));
    assert_eq!(std::fs::read(dst.join("sub").join("b")).unwrap(), b"BB");
    // The single-file path too.
    let one = d.path().join("one");
    let out = flux().args(["copy", "-q"]).arg(src.join("a")).arg(&one).output().unwrap();
    assert_eq!(code(&out), 0);
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{}", stderr(&out));
}

#[cfg(unix)]
#[test]
fn quiet_failed_copy_prints_the_cause_not_the_summary() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&src, b"secret").unwrap();
    if !make_unreadable(&src) {
        return;
    }
    let out = flux().args(["copy", "--quiet"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let err = stderr(&out);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.starts_with("PERMISSION_DENIED:"), "{err}");
    assert!(out.stdout.is_empty());
}

/// `tree_in(d)` plus a symlink and, on unix, a FIFO: both are skip records under `--links=skip`.
fn skippable_tree_in(d: &Path) -> PathBuf {
    let src = tree_in(d);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("a", src.join("lnk")).unwrap();
        let fifo = std::ffi::CString::new(src.join("fifo").to_str().unwrap()).unwrap();
        // SAFETY: `fifo` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    }
    src
}

#[test]
fn quiet_keeps_the_dry_run_output() {
    let d = TempDir::new().unwrap();
    let src = skippable_tree_in(d.path());
    let dst = d.path().join("dst");
    let run = |extra: &[&str]| {
        flux()
            .args(["copy", "--dry-run", "--links=skip"])
            .args(extra)
            .arg(&src)
            .arg(&dst)
            .output()
            .unwrap()
    };
    let (plain, quiet) = (run(&[]), run(&["--quiet"]));
    assert_eq!(code(&plain), 0);
    assert!(!stderr(&plain).is_empty());
    #[cfg(unix)]
    assert!(
        stderr(&plain).contains("symlink") && stderr(&plain).contains("special"),
        "{}",
        stderr(&plain)
    );
    assert_eq!(stderr(&quiet), stderr(&plain));
    assert_eq!(stdout(&quiet), stdout(&plain));
    assert!(!dst.exists(), "a dry run writes nothing");
}

#[test]
fn quiet_dry_run_of_several_sources_equals_the_plain_dry_run() {
    let d = TempDir::new().unwrap();
    let one = skippable_tree_in(d.path());
    let two = d.path().join("two");
    std::fs::create_dir_all(&two).unwrap();
    std::fs::write(two.join("x"), b"X").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("x", two.join("lnk2")).unwrap();
    let dst = d.path().join("dst");
    let run = |extra: &[&str]| {
        flux()
            .args(["copy", "--dry-run", "--links=skip"])
            .args(extra)
            .arg(&one)
            .arg(&two)
            .arg(&dst)
            .output()
            .unwrap()
    };
    let (plain, quiet) = (run(&[]), run(&["--quiet"]));
    assert_eq!(code(&plain), 0, "{}", stderr(&plain));
    assert!(!stderr(&plain).is_empty());
    assert_eq!(stderr(&quiet), stderr(&plain));
    assert_eq!(stdout(&quiet), stdout(&plain));
    assert!(!dst.exists(), "a dry run writes nothing");
}

#[test]
fn quiet_help_states_the_rule() {
    let out = flux().args(["copy", "--help"]).output().unwrap();
    let help = stdout(&out);
    assert!(help.contains("cause of a non-zero exit"), "{help}");
    assert!(!help.contains("summary line and the dry-run notes"), "{help}");
}

#[test]
fn quiet_verify_of_an_absent_destination_keeps_the_notice() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let out = flux().args(["verify", "-q"]).arg(&src).arg(d.path().join("nope")).output().unwrap();
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(stderr(&out).contains("destination does not exist"), "{}", stderr(&out));
    let so = stdout(&out);
    assert!(so.lines().any(|l| l.starts_with("missing: a")), "{so}");
    assert!(!so.contains("matched,") && !stderr(&out).contains("matched,"), "no summary: {so}");
}

#[test]
fn quiet_verify_of_several_sources_into_an_absent_destination_keeps_the_notices() {
    let d = TempDir::new().unwrap();
    let one = tree_in(d.path());
    let two = d.path().join("two");
    std::fs::create_dir_all(&two).unwrap();
    std::fs::write(two.join("x"), b"X").unwrap();
    let out = flux()
        .args(["verify", "-q"])
        .arg(&one)
        .arg(&two)
        .arg(d.path().join("nope"))
        .output()
        .unwrap();
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let e = stderr(&out);
    assert_eq!(e.matches("destination does not exist").count(), 2, "{e}");
    assert!(!e.contains("matched,"), "no per-root line: {e}");
    assert!(stdout(&out).lines().any(|l| l.starts_with("missing:")), "{}", stdout(&out));
}

#[cfg(unix)]
#[test]
fn quiet_hides_skip_records_and_resume_notes() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    std::os::unix::fs::symlink("a", src.join("link")).unwrap();
    let run = |extra: &[&str], dst: &str| {
        flux()
            .args(["copy", "--links=skip"])
            .args(extra)
            .arg(&src)
            .arg(d.path().join(dst))
            .output()
            .unwrap()
    };
    let loud = run(&[], "loud");
    assert_eq!(code(&loud), 0, "{}", stderr(&loud));
    assert!(stderr(&loud).contains("link"), "a skip record is printed normally: {}", stderr(&loud));
    let quiet = run(&["--quiet"], "quiet");
    assert_eq!(code(&quiet), 0, "{}", stderr(&quiet));
    assert!(quiet.stdout.is_empty());
    assert!(stderr(&quiet).is_empty(), "skip record and summary are hidden: {}", stderr(&quiet));
}

fn without_timing(out: &Output) -> serde_json::Value {
    let mut v: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {:?}", stdout(out)));
    let o = v.as_object_mut().expect("a json object");
    o.remove("duration_ms");
    o.remove("average_bytes_per_second");
    v
}

#[test]
fn quiet_with_json_leaves_stdout_unchanged() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let a = flux().args(["copy", "--json"]).arg(&src).arg(d.path().join("a")).output().unwrap();
    let b = flux()
        .args(["copy", "--json", "--quiet"])
        .arg(&src)
        .arg(d.path().join("b"))
        .output()
        .unwrap();
    assert_eq!((code(&a), code(&b)), (0, 0));
    assert_eq!(without_timing(&a), without_timing(&b));
    assert!(stderr(&b).is_empty(), "{}", stderr(&b));
}

#[test]
fn quiet_verify_clean_prints_nothing() {
    let d = TempDir::new().unwrap();
    let (src, dst) = pair_in(d.path());
    let out = flux().args(["verify", "--quiet"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{}", stdout(&out));
    // Several sources: no per-root lines either.
    let other = d.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("x"), b"X").unwrap();
    let both = d.path().join("both");
    std::fs::create_dir_all(both.join("src")).unwrap();
    std::fs::create_dir_all(both.join("other")).unwrap();
    std::fs::create_dir_all(both.join("src").join("sub")).unwrap();
    std::fs::write(both.join("src").join("a"), b"A").unwrap();
    std::fs::write(both.join("src").join("sub").join("b"), b"BB").unwrap();
    std::fs::write(both.join("other").join("x"), b"X").unwrap();
    let out = flux().args(["verify", "-q"]).arg(&src).arg(&other).arg(&both).output().unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{}{}", stdout(&out), stderr(&out));
}

#[test]
fn quiet_verify_mismatch_prints_the_records_only() {
    let d = TempDir::new().unwrap();
    let (src, dst) = pair_in(d.path());
    std::fs::write(dst.join("a"), b"Z").unwrap();
    std::fs::remove_file(dst.join("sub").join("b")).unwrap();
    let out = flux().args(["verify", "--quiet"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(code(&out), 1);
    let so = stdout(&out);
    let mut got: Vec<&str> = so.lines().collect();
    got.sort_unstable();
    assert_eq!(got.len(), 2, "{so}");
    assert!(got[0].starts_with("mismatched: a"), "{so}");
    assert!(got[1].starts_with("missing: sub/b") || got[1].starts_with("missing: sub"), "{so}");
    assert!(!so.contains("matched,"), "no summary line: {so}");
    assert!(out.stderr.is_empty(), "{}", stderr(&out));
}

#[test]
fn quiet_verify_ignores_extra_records_at_exit_0() {
    let d = TempDir::new().unwrap();
    let (src, dst) = pair_in(d.path());
    std::fs::write(dst.join("surplus"), b"?").unwrap();
    let loud = flux().arg("verify").arg(&src).arg(&dst).output().unwrap();
    assert_eq!(code(&loud), 0, "an extra alone does not fail verify: {}", stdout(&loud));
    assert!(stdout(&loud).contains("extra: surplus"), "{}", stdout(&loud));
    let quiet = flux().args(["verify", "-q"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(code(&quiet), 0);
    assert!(quiet.stdout.is_empty() && quiet.stderr.is_empty(), "{}", stdout(&quiet));
}

#[test]
fn quiet_cleanup_clean_prints_nothing() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let c = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();
    assert_eq!(code(&c), 0);
    let loud = flux().arg("cleanup").arg(&dst).output().unwrap();
    assert_eq!(code(&loud), 0, "{}", stderr(&loud));
    assert!(!stdout(&loud).is_empty());
    let quiet = flux().args(["cleanup", "--quiet"]).arg(&dst).output().unwrap();
    assert_eq!(code(&quiet), 0);
    assert!(quiet.stdout.is_empty() && quiet.stderr.is_empty(), "{}", stdout(&quiet));
    // A dry run and --json keep their output.
    let dry = flux().args(["cleanup", "-q", "--dry-run"]).arg(&dst).output().unwrap();
    assert!(!stdout(&dry).is_empty());
    let json = flux().args(["cleanup", "-q", "--json"]).arg(&dst).output().unwrap();
    assert!(stdout(&json).trim_start().starts_with('{'), "{}", stdout(&json));
}

#[test]
fn every_nonzero_exit_prints_a_cause_under_quiet() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let mut runs: Vec<(&str, Output)> = Vec::new();
    #[cfg(unix)]
    {
        let (f, to) = (d.path().join("locked"), d.path().join("locked-copy"));
        std::fs::write(&f, b"x").unwrap();
        if make_unreadable(&f) {
            runs.push((
                "unreadable file",
                flux().args(["copy", "-q"]).arg(&f).arg(&to).output().unwrap(),
            ));
        }
    }
    // A containment refusal: a tree copied into itself.
    runs.push((
        "containment",
        flux().args(["copy", "-q"]).arg(&src).arg(src.join("sub").join("inside")).output().unwrap(),
    ));
    // A usage error.
    runs.push((
        "usage",
        flux()
            .args(["copy", "-q", "--resume-verify=metadata"])
            .arg(&src)
            .arg(d.path().join("u"))
            .output()
            .unwrap(),
    ));
    // A verify mismatch.
    let (vs, vd) = pair_in(&d.path().join("v"));
    std::fs::write(vd.join("a"), b"Z").unwrap();
    runs.push((
        "verify mismatch",
        flux().args(["verify", "-q"]).arg(&vs).arg(&vd).output().unwrap(),
    ));
    // A cleanup refusal: the destination's parent does not exist.
    runs.push((
        "cleanup refusal",
        flux()
            .args(["cleanup", "-q"])
            .arg(d.path().join("no").join("such").join("dst"))
            .output()
            .unwrap(),
    ));
    for (name, out) in &runs {
        let c = code(out);
        assert_ne!(c, 0, "{name}: stderr {} stdout {}", stderr(out), stdout(out));
        let said = stderr(out).lines().chain(stdout(out).lines()).any(|l| !l.trim().is_empty());
        assert!(said, "{name} (exit {c}) printed no cause");
    }
}

#[test]
fn v_and_q_together_exit_2_with_the_same_message_in_every_position() {
    let d = TempDir::new().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&a, b"x").unwrap();
    let cases: [Vec<&std::ffi::OsStr>; 4] = [
        vec!["copy".as_ref(), "-v".as_ref(), "-q".as_ref(), a.as_ref(), b.as_ref()],
        vec!["-q".as_ref(), "copy".as_ref(), "-v".as_ref(), a.as_ref(), b.as_ref()],
        vec!["verify".as_ref(), "-vq".as_ref(), a.as_ref(), b.as_ref()],
        vec!["cleanup".as_ref(), "-v".as_ref(), "-q".as_ref(), b.as_ref()],
    ];
    let mut messages = Vec::new();
    for args in &cases {
        let out = flux().args(args).output().unwrap();
        assert_eq!(code(&out), 2, "{args:?}: {}", stderr(&out));
        let err = stderr(&out);
        assert!(err.contains("-v") && err.contains("-q"), "{args:?}: {err}");
        assert!(out.stdout.is_empty());
        messages.push(err);
    }
    assert!(messages.windows(2).all(|w| w[0] == w[1]), "{messages:?}");
}
