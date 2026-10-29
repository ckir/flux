// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! `-v`, `-vv`, `-vvv` and `--quiet` through the real binary with piped stderr (cut 21, design 5.2 and 9). The
//! terminal half (the bar stays below the lines) is in `pty.rs`. The `-vv`/`-vvv` lines are not a stable interface:
//! they are only checked for their shape.
#![cfg(unix)]

use flux_core::lock::record::{Decoded, decode};
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

fn code(out: &Output) -> i32 {
    out.status.code().expect("exited by signal")
}

/// `src/a` (2 bytes), `src/lnk` -> `a`, `src/sub/b` (3), `src/sub/c` (2) and, with `with_locked`, `src/locked`
/// (`secret\n`, mode 000). Returns false when the locked file is readable anyway (running as root): the caller
/// returns early.
fn sample(d: &Path, with_locked: bool) -> bool {
    let src = d.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a"), b"A\n").unwrap();
    std::os::unix::fs::symlink("a", src.join("lnk")).unwrap();
    std::fs::write(src.join("sub").join("b"), b"BB\n").unwrap();
    std::fs::write(src.join("sub").join("c"), b"C\n").unwrap();
    if with_locked {
        use std::os::unix::fs::PermissionsExt;
        let locked = src.join("locked");
        std::fs::write(&locked, b"secret\n").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        return std::fs::File::open(&locked).is_err();
    }
    true
}

/// The summary line carries a varying elapsed time: ` in 0.02s;` becomes ` in T;`.
fn mask_elapsed(line: &str) -> String {
    match (line.find(" in "), line.find(';')) {
        (Some(i), Some(j)) if i < j => format!("{} in T{}", &line[..i], &line[j..]),
        _ => line.to_owned(),
    }
}

fn masked_lines(text: &str) -> Vec<String> {
    text.lines().map(mask_elapsed).collect()
}

/// Every path under `root` (relative) mapped to its bytes, a symlink to its target, a directory to a marker.
fn tree(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let md = std::fs::symlink_metadata(&p).unwrap();
            if md.file_type().is_symlink() {
                out.insert(rel, format!("-> {}", std::fs::read_link(&p).unwrap().display()));
            } else if md.is_dir() {
                if rel == ".flux" {
                    continue;
                }
                out.insert(rel, "<dir>".into());
                stack.push(p);
            } else {
                out.insert(rel, String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned());
            }
        }
    }
    out
}

fn is_verb_line(l: &str) -> bool {
    let t = l.trim_start();
    ["Created", "Copied", "Linked", "Replaced", "Resumed", "Skipped", "Removed", "Hardlinked"]
        .iter()
        .any(|v| t.starts_with(&format!("{v} ")))
}

#[test]
fn v_prints_verb_lines_before_the_summary() {
    let d = TempDir::new().unwrap();
    assert!(sample(d.path(), false));
    let out = flux()
        .args(["copy", "-v"])
        .arg(d.path().join("src"))
        .arg(d.path().join("dst"))
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        masked_lines(&stderr(&out)),
        [
            "    Copied a (2 B)",
            "    Linked lnk -> a",
            "   Created sub",
            "    Copied sub/b (3 B)",
            "    Copied sub/c (2 B)",
            "copied 4 files (7 B), created 2 directories in T; 0 failed, 0 skipped",
        ]
    );
}

#[test]
fn v_with_a_failure_keeps_the_failure_line_in_walk_order() {
    let d = TempDir::new().unwrap();
    if !sample(d.path(), true) {
        return;
    }
    let out = flux()
        .args(["copy", "-v"])
        .arg(d.path().join("src"))
        .arg(d.path().join("dst"))
        .output()
        .unwrap();
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert_eq!(
        masked_lines(&stderr(&out)),
        [
            "    Copied a (2 B)",
            "    Linked lnk -> a",
            "PERMISSION_DENIED: locked: Permission denied (os error 13)",
            "   Created sub",
            "    Copied sub/b (3 B)",
            "    Copied sub/c (2 B)",
            "copied 4 files (7 B), created 2 directories in T; 1 failed, 0 skipped",
        ]
    );
}

#[test]
fn vv_and_vvv_add_lines_that_carry_paths() {
    let d = TempDir::new().unwrap();
    assert!(sample(d.path(), false));
    let src = d.path().join("src");
    for (flag, n) in [("-vv", 2), ("-vvv", 3)] {
        let dst = d.path().join(format!("dst{n}"));
        let out = flux().args(["copy", flag]).arg(&src).arg(&dst).output().unwrap();
        assert_eq!(code(&out), 0, "{flag}: {}", stderr(&out));
        let e = stderr(&out);
        let lines: Vec<&str> = e.lines().collect();
        assert!(
            lines.iter().any(|l| l.starts_with("debug ") && l.contains("path=")),
            "{flag}: no debug line with a path: {e}"
        );
        for verb in ["    Copied a (2 B)", "    Linked lnk -> a", "   Created sub"] {
            assert!(lines.contains(&verb), "{flag}: lost {verb:?}: {e}");
        }
        if n == 3 {
            assert!(lines.iter().any(|l| l.starts_with("trace")), "{flag}: no trace line: {e}");
        }
    }
}

#[test]
fn json_with_v_keeps_stdout_unchanged() {
    let d = TempDir::new().unwrap();
    assert!(sample(d.path(), false));
    let src = d.path().join("src");
    let run = |flags: &[&str], dst: &str| {
        flux().arg("copy").args(flags).arg(&src).arg(d.path().join(dst)).output().unwrap()
    };
    let strip = |out: &Output| {
        let mut v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let o = v.as_object_mut().unwrap();
        o.remove("duration_ms");
        o.remove("average_bytes_per_second");
        v
    };
    let plain = run(&["--json"], "d1");
    let verbose = run(&["--json", "-v"], "d2");
    assert_eq!(code(&plain), 0, "{}", stderr(&plain));
    assert_eq!(code(&verbose), 0, "{}", stderr(&verbose));
    // Both runs name their own destination, so those fields differ by construction: compare the rest.
    let (mut a, mut b) = (strip(&plain), strip(&verbose));
    for v in [&mut a, &mut b] {
        let o = v.as_object_mut().unwrap();
        for k in ["destination", "dst", "operation_id"] {
            o.remove(k);
        }
    }
    assert_eq!(a, b);
    assert_eq!(
        strip(&plain).as_object().unwrap().len(),
        strip(&verbose).as_object().unwrap().len()
    );
    assert!(stderr(&plain).lines().all(|l| !is_verb_line(l)), "{}", stderr(&plain));
    let e = stderr(&verbose);
    assert!(e.contains("    Copied a (2 B)") && e.contains("   Created sub"), "{e}");
}

#[test]
fn dry_run_with_v_adds_no_verb_lines() {
    let d = TempDir::new().unwrap();
    assert!(sample(d.path(), false));
    let out = flux()
        .args(["copy", "--dry-run", "-v"])
        .arg(d.path().join("src"))
        .arg(d.path().join("dst"))
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    for stream in [stdout(&out), stderr(&out)] {
        assert!(stream.lines().all(|l| !is_verb_line(l)), "{stream}");
    }
    assert!(stdout(&out).contains("would_copy") || stderr(&out).contains("would_copy"));
    assert!(!d.path().join("dst").exists(), "a dry run writes nothing");
}

#[test]
fn verify_v_prints_match_records_on_stdout_and_verbose_is_the_same() {
    let d = TempDir::new().unwrap();
    assert!(sample(d.path(), false));
    let (src, dst) = (d.path().join("src"), d.path().join("dst"));
    let out = flux().args(["copy"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let run = |flag: &str| flux().args(["verify", flag]).arg(&src).arg(&dst).output().unwrap();
    let short = run("-v");
    let long = run("--verbose");
    assert_eq!(code(&short), 0, "{}", stderr(&short));
    assert_eq!(code(&long), 0, "{}", stderr(&long));
    assert_eq!(stdout(&short), stdout(&long));
    assert_eq!(
        stdout(&short),
        "match: a\nmatch: lnk\nmatch: sub\nmatch: sub/b\nmatch: sub/c\n\
         verified: 5 matched, 0 missing, 0 mismatched, 0 extra, 0 unreadable; 14 bytes hashed\n"
    );
}

/// A copy run with the crash hook stalling before its `at`-th guarded mutation; killed on drop.
struct Stalled(Child);

impl Drop for Stalled {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn stall(src: &Path, dst: &Path, at: u32, marks: &Path) -> Stalled {
    let announced = marks.join(format!("stalled-{at}"));
    let child = flux()
        .arg("copy")
        .arg(src)
        .arg(dst)
        .env("FLUX_TEST_STALL_AT", at.to_string())
        .env("FLUX_TEST_STALL_FILE", &announced)
        .env("FLUX_TEST_BATCH_AGE_MS", "3600000")
        .env("FLUX_TEST_OPEN_WRITERS", "256")
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

impl Stalled {
    fn kill(mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}

#[test]
fn cleanup_v_prints_removed_lines() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    assert!(sample(d.path(), false));
    let (src, dst) = (d.path().join("src"), d.path().join("dst"));
    // `a`'s sweep (1), temporary (2), publish (3): killed with the temporary written.
    stall(&src, &dst, 3, marks.path()).kill();
    // Age the killed copy past the 30 s lease: its manifest's mtime and its lock record's heartbeat.
    let ops = dst.join(".flux").join("operations");
    let id = std::fs::read_dir(&ops).unwrap().next().unwrap().unwrap().file_name();
    let manifest =
        std::fs::OpenOptions::new().write(true).open(ops.join(&id).join("manifest")).unwrap();
    manifest.set_modified(SystemTime::now() - Duration::from_secs(3600)).unwrap();
    let lock: PathBuf = d.path().join("dst.flux-lock");
    let bytes = std::fs::read(&lock).unwrap();
    let Decoded::Record(mut record) = decode(&bytes) else { panic!("a lock record") };
    record.last_heartbeat_wall_time = 1;
    std::fs::write(&lock, record.encode()).unwrap();

    let out = flux().args(["cleanup", "-v", "--force"]).arg(&dst).output().unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.lines().any(|l| l.trim_start().starts_with("Removed ")), "no Removed line: {e}");
    assert!(!dst.join(".flux").exists());
}

#[test]
fn v_with_a_dead_stderr_pipe_exits_like_without() {
    use std::os::fd::{FromRawFd, OwnedFd};
    let d = TempDir::new().unwrap();
    assert!(sample(d.path(), false));
    let src = d.path().join("src");
    let normal = flux().arg("copy").arg(&src).arg(d.path().join("plain")).output().unwrap();
    assert_eq!(code(&normal), 0);

    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `pipe` fills two fresh descriptors this test owns; the read end is closed, the write end is moved
    // into an `OwnedFd`.
    let write_end = unsafe {
        assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
        assert_eq!(libc::close(fds[0]), 0);
        OwnedFd::from_raw_fd(fds[1])
    };
    let status = flux()
        .args(["copy", "-v"])
        .arg(&src)
        .arg(d.path().join("dead"))
        .stdout(Stdio::null())
        .stderr(Stdio::from(write_end))
        .status()
        .unwrap();
    assert!(status.code().is_some(), "killed by a signal: {status}");
    assert_eq!(status.code(), normal.status.code());
    assert_eq!(tree(&d.path().join("dead")), tree(&d.path().join("plain")));
}

#[test]
fn a_hostile_file_name_is_escaped_in_the_verb_line() {
    let d = TempDir::new().unwrap();
    let src = d.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("x\u{1b}[2Jy"), b"A\n").unwrap();
    let out = flux().args(["copy", "-v"]).arg(&src).arg(d.path().join("dst")).output().unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!out.stderr.contains(&0x1b), "a raw ESC reached stderr: {:?}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("Copied x\\u{1b}[2Jy (2 B)"), "ESC not shown escaped: {e}");
}

#[test]
fn resume_with_different_verbosity_is_accepted() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    assert!(sample(d.path(), false));
    let src = d.path().join("src");
    let dst = d.path().join("dst");
    // Killed with the temporary of `a` written: a resumable operation started at default verbosity.
    stall(&src, &dst, 3, marks.path()).kill();
    let out = flux().args(["copy", "--resume", "-v"]).arg(&src).arg(&dst).output().unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let e = stderr(&out);
    assert!(!e.contains("RESUMABLE") && !e.contains("INVALID"), "{e}");
    let reference = flux().arg("copy").arg(&src).arg(d.path().join("ref")).output().unwrap();
    assert_eq!(code(&reference), 0);
    assert_eq!(tree(&dst), tree(&d.path().join("ref")));
}
