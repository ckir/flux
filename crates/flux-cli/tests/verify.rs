// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

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

/// Every stdout line parsed; a line that does not parse panics.
fn ndjson(out: &Output) -> Vec<serde_json::Value> {
    stdout(out)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l:?}")))
        .collect()
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

fn verify(args: &[&Path]) -> Output {
    flux().arg("verify").args(args).output().unwrap()
}

const RECORD_KEYS: [&str; 5] = ["path", "outcome", "kind", "code", "detail"];
const SUMMARY_KEYS: [&str; 11] = [
    "source",
    "dest",
    "hash",
    "matched",
    "missing",
    "mismatched",
    "extra",
    "unreadable",
    "not_entered",
    "bytes_hashed",
    "exit",
];

/// The object's keys, as a set (serde_json's `Value` sorts them), and in the order the text spells them.
fn keys(v: &serde_json::Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

/// Whether `want` are exactly `v`'s keys and `line` spells them in that order.
fn keys_in_order(line: &str, v: &serde_json::Value, want: &[&str]) -> bool {
    let mut got = keys(v);
    got.sort_unstable();
    let mut sorted = want.to_vec();
    sorted.sort_unstable();
    let at: Vec<Option<usize>> = want.iter().map(|k| line.find(&format!("\"{k}\":"))).collect();
    got == sorted && at.iter().all(Option::is_some) && at.windows(2).all(|w| w[0] < w[1])
}

#[test]
fn exit_0_for_identical_trees_and_with_extras_only() {
    let d = TempDir::new().unwrap();
    let (src, dst) = pair_in(d.path());
    let out = verify(&[&src, &dst]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    std::fs::write(dst.join("extra"), b"x").unwrap();
    let out = verify(&[&src, &dst]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).lines().any(|l| l == "extra: extra"), "{}", stdout(&out));
}

#[test]
fn exit_1_for_missing_mismatched_and_unreadable() {
    let d = TempDir::new().unwrap();
    let (src, dst) = pair_in(d.path());
    std::fs::remove_file(dst.join("a")).unwrap();
    let out = verify(&[&src, &dst]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).lines().any(|l| l == "missing: a"), "{}", stdout(&out));

    std::fs::write(dst.join("a"), b"A").unwrap();
    std::fs::write(dst.join("sub").join("b"), b"BX").unwrap();
    let out = verify(&[&src, &dst]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("mismatched: sub/b: VERIFY_MISMATCH"), "{}", stdout(&out));

    #[cfg(unix)]
    {
        std::fs::write(dst.join("sub").join("b"), b"BB").unwrap();
        for side in [&src, &dst] {
            let st = Command::new("mkfifo").arg(side.join("p")).status().unwrap();
            assert!(st.success());
        }
        let mut child = flux()
            .arg("verify")
            .arg(&src)
            .arg(&dst)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("verify did not return on a FIFO within 10 s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
        assert!(
            stdout(&out)
                .contains("unreadable: p: SPECIAL_FILE_UNSUPPORTED: special file not compared"),
            "{}",
            stdout(&out)
        );
    }
}

#[test]
fn exit_2_for_a_folder_onto_a_file_an_unknown_flag_and_resume() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let file = d.path().join("f");
    std::fs::write(&file, b"x").unwrap();
    let out = verify(&[&src, &file]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("error:"), "{}", stderr(&out));
    assert!(out.stdout.is_empty(), "{}", stdout(&out));

    let dst = d.path().join("dst");
    for flag in [["--exclude", "x"], ["--resume", ""]] {
        let mut c = flux();
        c.arg("verify").arg(&src).arg(&dst);
        c.args(flag.iter().filter(|s| !s.is_empty()));
        let out = c.output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{flag:?}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn exit_2_for_nested_or_identical_roots_with_nothing_on_stdout() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let sub = src.join("sub");
    let out = verify(&[&src, &src]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("nothing was compared"), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
    for (a, b) in [(&src, &sub), (&sub, &src)] {
        let out = verify(&[a, b]);
        assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
        assert!(out.stdout.is_empty(), "{}", stdout(&out));
    }
}

#[test]
fn an_absent_destination_prints_the_leading_line_and_every_path_missing() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("nowhere");
    let out = verify(&[&src, &dst]);
    assert_eq!(out.status.code(), Some(1));
    let line = format!(
        "destination does not exist: {} (every source path is reported missing)",
        dst.display()
    );
    assert!(stderr(&out).contains(&line), "{}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(&lines[..3], ["missing: a", "missing: sub", "missing: sub/b"], "{text}");
    assert_eq!(
        lines[3],
        "verified: 0 matched, 3 missing, 0 mismatched, 0 extra, 0 unreadable; 0 bytes hashed"
    );
    assert_eq!(lines.len(), 4);
    assert!(!dst.exists());
}

#[test]
fn human_output_hides_matches_unless_verbose() {
    let d = TempDir::new().unwrap();
    let (src, dst) = pair_in(d.path());
    let summary =
        "verified: 3 matched, 0 missing, 0 mismatched, 0 extra, 0 unreadable; 6 bytes hashed";
    let out = verify(&[&src, &dst]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), format!("{summary}\n"));
    let out = flux().arg("verify").arg(&src).arg(&dst).arg("--verbose").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), format!("match: a\nmatch: sub\nmatch: sub/b\n{summary}\n"));
}

#[test]
fn ndjson_has_a_record_per_path_and_a_final_summary() {
    let d = TempDir::new().unwrap();
    let (src, dst) = pair_in(d.path());
    let out = flux().arg("verify").arg(&src).arg(&dst).arg("--json").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let lines = ndjson(&out);
    assert_eq!(lines.len(), 4);
    let text = stdout(&out);
    let raw: Vec<&str> = text.lines().collect();
    for (r, line) in lines[..3].iter().zip(&raw) {
        assert!(keys_in_order(line, r, &RECORD_KEYS), "{line}");
        assert_eq!(r["outcome"], "match");
    }
    let last = &lines[3];
    assert_eq!(keys(last), ["summary"]);
    assert!(keys_in_order(raw[3], &last["summary"], &SUMMARY_KEYS), "{}", raw[3]);
    assert_eq!(last["summary"]["exit"], 0);
    assert_eq!(last["summary"]["matched"], 3);
    assert_eq!(last["summary"]["hash"], "blake3");
    let verbose =
        flux().arg("verify").arg(&src).arg(&dst).args(["--verbose", "--json"]).output().unwrap();
    assert_eq!(verbose.stdout, out.stdout);
}

#[test]
fn a_file_source_maps_like_copy() {
    let d = TempDir::new().unwrap();
    let a = d.path().join("a.txt");
    let b = d.path().join("b.txt");
    std::fs::write(&a, b"same").unwrap();
    std::fs::write(&b, b"same").unwrap();
    let out = flux().arg("verify").arg(&a).arg(&b).arg("--verbose").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).lines().any(|l| l == "match: b.txt"), "{}", stdout(&out));

    let folder = d.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("a.txt"), b"same").unwrap();
    let out = flux().arg("verify").arg(&a).arg(&folder).arg("--verbose").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).lines().any(|l| l == "match: a.txt"), "{}", stdout(&out));

    let absent = d.path().join("absent.txt");
    let out = verify(&[&a, &absent]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).lines().any(|l| l == "missing: absent.txt"), "{}", stdout(&out));
}

#[cfg(unix)]
#[test]
fn a_symlink_source_is_refused_like_copy() {
    let d = TempDir::new().unwrap();
    let target = d.path().join("t");
    std::fs::write(&target, b"x").unwrap();
    let link = d.path().join("l");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let dst = d.path().join("dst");
    let out = flux().arg("verify").arg(&link).arg(&dst).arg("--json").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).starts_with("SYMLINK_CREATION_UNAVAILABLE:"), "{}", stderr(&out));
    let lines = ndjson(&out);
    assert_eq!(lines.len(), 1);
    assert_eq!(keys(&lines[0]), ["summary"]);
    assert_eq!(lines[0]["summary"]["exit"], 1);
    assert_eq!(lines[0]["summary"]["matched"], 0);
}
