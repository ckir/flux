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
    assert!(
        stdout(&out).ends_with(
            "verified: 3 matched, 0 missing, 0 mismatched, 1 extra, 0 unreadable; 6 bytes hashed\n"
        ),
        "{}",
        stdout(&out)
    );
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
    // The resolver shows the canonical ancestor plus the absent remainder (K6).
    let shown = std::fs::canonicalize(d.path()).unwrap().join("nowhere");
    let line = format!(
        "destination does not exist: {} (every source path is reported missing)",
        shown.display()
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

#[test]
fn a_real_copy_verifies_clean_and_a_flipped_byte_is_caught() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let copy = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();
    assert_eq!(copy.status.code(), Some(0), "{}", stderr(&copy));

    let out = flux().arg("verify").arg(&src).arg(&dst).arg("--json").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let lines = ndjson(&out);
    let summary = &lines.last().unwrap()["summary"];
    assert_eq!(summary["matched"], 3, "{lines:?}");
    assert_eq!(summary["extra"], 0, "{lines:?}");

    let b = dst.join("sub").join("b");
    let mut bytes = std::fs::read(&b).unwrap();
    *bytes.last_mut().unwrap() ^= 0xff;
    std::fs::write(&b, bytes).unwrap();
    let out = flux().arg("verify").arg(&src).arg(&dst).arg("--json").output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let lines = ndjson(&out);
    let records: Vec<&serde_json::Value> =
        lines.iter().filter(|l| l.get("summary").is_none() && l["outcome"] != "match").collect();
    assert_eq!(records.len(), 1, "{lines:?}");
    assert_eq!(records[0]["outcome"], "mismatched");
    assert_eq!(records[0]["path"], "sub/b");
    assert_eq!(records[0]["code"], "VERIFY_MISMATCH");
    assert_eq!(lines.last().unwrap()["summary"]["mismatched"], 1);
}

#[test]
fn control_state_the_copy_leaves_is_not_reported() {
    let d = TempDir::new().unwrap();
    let (src, dst) = pair_in(d.path());
    let zeros = "0".repeat(32);
    let op = dst.join(".flux").join("operations").join(&zeros);
    std::fs::create_dir_all(&op).unwrap();
    std::fs::write(op.join("state"), b"s").unwrap();
    std::fs::write(dst.join(".flux-root.lock"), b"l").unwrap();
    std::fs::write(dst.join(format!("a.flux-partial.{zeros}")), b"p").unwrap();
    std::fs::write(dst.join("note.flux-partial.txt"), b"user").unwrap();

    let out = flux().arg("verify").arg(&src).arg(&dst).arg("--json").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let lines = ndjson(&out);
    let extras: Vec<&serde_json::Value> =
        lines.iter().filter(|l| l["outcome"] == "extra").collect();
    assert_eq!(extras.len(), 1, "{lines:?}");
    assert_eq!(extras[0]["path"], "note.flux-partial.txt");
    assert_eq!(lines.last().unwrap()["summary"]["extra"], 1);
}

#[cfg(unix)]
#[test]
fn links_skip_leaves_a_source_symlink_out_end_to_end() {
    let d = TempDir::new().unwrap();
    let src = d.path().join("src");
    let dst = d.path().join("dst");
    for side in [&src, &dst] {
        std::fs::create_dir(side).unwrap();
        std::fs::write(side.join("f"), b"F").unwrap();
        std::os::unix::fs::symlink("target-one", side.join("l")).unwrap();
    }
    let paths = |out: &Output| -> Vec<(String, String)> {
        ndjson(out)
            .iter()
            .filter(|l| l.get("summary").is_none())
            .map(|l| {
                (l["path"].as_str().unwrap().to_owned(), l["outcome"].as_str().unwrap().to_owned())
            })
            .collect()
    };
    let run = |extra: &[&str]| {
        flux()
            .arg("verify")
            .arg(&src)
            .arg(&dst)
            .args(extra)
            .args(["--verbose", "--json"])
            .output()
            .unwrap()
    };

    // Without the flag the payloads are compared: both sides have `l`.
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(paths(&out).contains(&("l".into(), "match".into())), "{:?}", paths(&out));

    // With `--links skip` the source symlink produces no record from the source side.
    let out = run(&["--links", "skip"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let got = paths(&out);
    assert_eq!(
        got.iter().filter(|(p, _)| p == "l").collect::<Vec<_>>(),
        [&("l".to_owned(), "extra".to_owned())],
        "{got:?}"
    );

    // A destination-only symlink is `extra` under `--links skip`.
    std::os::unix::fs::symlink("elsewhere", dst.join("only-dest")).unwrap();
    let out = run(&["--links", "skip"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let got = paths(&out);
    assert!(got.contains(&("only-dest".into(), "extra".into())), "{got:?}");
    assert!(got.contains(&("f".into(), "match".into())), "{got:?}");
    // The source symlink `l` is left out, so the destination's `l` is a leftover.
    assert!(got.contains(&("l".into(), "extra".into())), "{got:?}");
}

#[test]
fn help_shows_the_exit_codes_and_the_guarantees() {
    let out = flux().args(["verify", "--help"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Exit 0: nothing missing, mismatched or unreadable; 1:"), "{text}");
    assert!(text.contains("What a pass guarantees:"), "{text}");
}

/// Restores a directory's mode on drop, so the tempdir can be removed even when an assertion fails.
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
fn a_destination_only_directory_that_cannot_be_listed_is_extra_with_a_warning_and_exit_0() {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new().unwrap();
    let (src, dst) = pair_in(d.path());
    let lost = dst.join("lost+found");
    std::fs::create_dir(&lost).unwrap();
    std::fs::write(lost.join("inside"), b"i").unwrap();
    std::fs::set_permissions(&lost, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = Restore(lost.clone());
    assert!(
        std::fs::read_dir(&lost).is_err(),
        "this user can list a mode-000 directory (running as root?): the unlistable case cannot be exercised here"
    );
    let out = verify(&[&src, &dst]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).lines().any(|l| l.starts_with("extra: lost+found: PERMISSION_DENIED: ")),
        "{}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("warning: could not read destination-only directory: lost+found: "),
        "{}",
        stderr(&out)
    );
    let out = flux().arg("verify").arg(&src).arg(&dst).arg("--json").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let lines = ndjson(&out);
    let extra = lines.iter().find(|l| l["path"] == "lost+found").expect("the record");
    assert_eq!((extra["outcome"].as_str(), extra["kind"].as_str()), (Some("extra"), Some("dir")));
    assert_eq!(extra["code"], "PERMISSION_DENIED");
    let s = &lines.last().unwrap()["summary"];
    assert_eq!(
        (s["extra"].as_u64(), s["unreadable"].as_u64(), s["exit"].as_u64()),
        (Some(1), Some(0), Some(0))
    );
    assert!(stderr(&out).contains("warning: could not read destination-only directory"));
}

#[test]
fn a_refused_run_does_not_also_announce_a_missing_destination() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let inside = src.join("nope");
    let out = verify(&[&src, &inside]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("nothing was compared"), "{}", stderr(&out));
    assert!(!stderr(&out).contains("does not exist"), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn a_closed_stdout_stops_the_run_with_exit_1_and_no_panic() {
    use std::io::BufRead;
    let d = TempDir::new().unwrap();
    let (src, dst) = pair_in(d.path());
    // About 600 KB of output: far more than a pipe holds, so the child is still writing when the reader leaves.
    for i in 0..6000 {
        std::fs::write(dst.join(format!("{}-{i:05}", "x".repeat(80))), b"").unwrap();
    }
    let mut child = flux()
        .arg("verify")
        .arg(&src)
        .arg(&dst)
        .arg("--verbose")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert!(first.starts_with("extra: ") || first.starts_with("match: "), "{first}");
    drop(reader);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("verify kept running after its stdout was closed");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let out = child.wait_with_output().unwrap();
    assert_eq!(status.code(), Some(1), "{}", stderr(&out));
    assert!(!stderr(&out).contains("panicked"), "{}", stderr(&out));
    assert!(stderr(&out).is_empty(), "a broken pipe is silent: {}", stderr(&out));
}

#[cfg(unix)]
#[test]
fn awkward_names_are_unambiguous_and_never_split_a_line() {
    use std::os::unix::ffi::OsStrExt;
    let d = TempDir::new().unwrap();
    let (src, dst) = pair_in(d.path());
    // macOS enforces UTF-8 file names, so the invalid-byte cases run on Linux only; the control-byte and
    // backslash escaping is exercised on both.
    let names: &[&[u8]] = if cfg!(target_os = "macos") {
        &[b"bad\nname", b"back\\slash", b"plain"]
    } else {
        &[b"bad\xff\nname", b"bad\xfe\nname", b"back\\slash", b"plain"]
    };
    let expected: &[&str] = if cfg!(target_os = "macos") {
        &["back\\\\slash", "bad\\nname", "plain"]
    } else {
        &["back\\\\slash", "bad\\xfe\\nname", "bad\\xff\\nname", "plain"]
    };
    for name in names {
        std::fs::write(dst.join(std::ffi::OsStr::from_bytes(name)), b"").unwrap();
    }
    let out = flux().arg("verify").arg(&src).arg(&dst).arg("--json").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let lines = ndjson(&out);
    let paths: Vec<&str> = lines
        .iter()
        .filter(|l| l["outcome"] == "extra")
        .map(|l| l["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, expected);
    let out = verify(&[&src, &dst]);
    // One line per extra plus the summary, whatever the names hold.
    assert_eq!(stdout(&out).lines().count(), names.len() + 1, "{}", stdout(&out));
}

const SEVERAL_SUMMARY_KEYS: [&str; 11] = [
    "sources",
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

/// `a/x` (`A`), `b/y` (`BB`) and an identical `dest/{a/x, b/y}` under `d`; returns `(a, b, dest)`.
fn two_roots(d: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let (a, b, dest) = (d.join("a"), d.join("b"), d.join("dest"));
    for (root, file, bytes) in [(&a, "x", &b"A"[..]), (&b, "y", &b"BB"[..])] {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(file), bytes).unwrap();
        let under = dest.join(root.file_name().unwrap());
        std::fs::create_dir_all(&under).unwrap();
        std::fs::write(under.join(file), bytes).unwrap();
    }
    (a, b, dest)
}

#[test]
fn three_positionals_verify_each_root_under_dest() {
    let d = TempDir::new().unwrap();
    let (a, b, dest) = two_roots(d.path());
    // A real several-source copy first (design test 11); `two_roots`' hand-made dest is replaced by it.
    std::fs::remove_dir_all(&dest).unwrap();
    let copy = flux().arg("copy").arg(&a).arg(&b).arg(&dest).output().unwrap();
    assert_eq!(copy.status.code(), Some(0), "{}", stderr(&copy));
    let out = verify(&[&a, &b, &dest]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).lines().last(),
        Some("verified: 2 matched, 0 missing, 0 mismatched, 0 extra, 0 unreadable; 6 bytes hashed"),
        "{}",
        stdout(&out)
    );
    std::fs::write(dest.join("b").join("y"), b"BX").unwrap();
    let out = verify(&[&a, &b, &dest]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stdout(&out).lines().any(|l| l.starts_with("mismatched: b/y: VERIFY_MISMATCH: ")),
        "{}",
        stdout(&out)
    );
}

#[test]
fn dest_strangers_and_copy_residue_are_invisible() {
    let d = TempDir::new().unwrap();
    let (a, b, dest) = two_roots(d.path());
    std::fs::create_dir_all(dest.join("c")).unwrap();
    std::fs::write(dest.join("c").join("z"), b"Z").unwrap();
    std::fs::write(dest.join("a.flux-lock"), b"").unwrap();
    let state = dest.join(".flux").join("operations").join("0".repeat(32));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("state"), b"s").unwrap();
    let out = verify(&[&a, &b, &dest]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!stdout(&out).lines().any(|l| l.starts_with("extra:")), "{}", stdout(&out));
}

#[test]
fn json_several_summary_has_sources_and_no_source() {
    let d = TempDir::new().unwrap();
    let (a, b, dest) = two_roots(d.path());
    let ta = d.path().join(".").join("a");
    let tb = d.path().join("b").join("..").join("b");
    let td = d.path().join(".").join("dest");
    let out = flux().arg("verify").arg(&ta).arg(&tb).arg(&td).arg("--json").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    let last = text.lines().last().unwrap();
    let v: serde_json::Value = serde_json::from_str(last).unwrap();
    let s = &v["summary"];
    assert!(keys_in_order(last, s, &SEVERAL_SUMMARY_KEYS), "{last}");
    let want = [std::fs::canonicalize(&a).unwrap(), std::fs::canonicalize(&b).unwrap()];
    let got: Vec<&str> =
        s["sources"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
    assert_eq!(got, [want[0].to_str().unwrap(), want[1].to_str().unwrap()]);
    assert_ne!(got[0], ta.to_str().unwrap());
    assert_ne!(got[1], tb.to_str().unwrap());
    assert_eq!(
        s["dest"].as_str().unwrap(),
        std::fs::canonicalize(&dest).unwrap().to_str().unwrap()
    );

    // A stopped run prints the strings as typed.
    let absent = d.path().join("absent");
    let out = flux().arg("verify").arg(&a).arg(&absent).arg(&dest).arg("--json").output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let lines = ndjson(&out);
    assert_eq!(lines.len(), 1, "{}", stdout(&out));
    let s = &lines[0]["summary"];
    let got: Vec<&str> =
        s["sources"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
    assert_eq!(got, [a.to_str().unwrap(), absent.to_str().unwrap()]);
    assert_eq!(s["exit"], 1);

    // The one-source summary is unchanged.
    let out = flux().arg("verify").arg(&a).arg(dest.join("a")).arg("--json").output().unwrap();
    let text = stdout(&out);
    let v: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert!(
        v["summary"].get("source").is_some() && v["summary"].get("sources").is_none(),
        "{text}"
    );
}

#[test]
fn per_root_stderr_lines_and_the_absent_dest_lines() {
    let d = TempDir::new().unwrap();
    let (a, b, dest) = two_roots(d.path());
    for json in [false, true] {
        let mut c = flux();
        c.arg("verify").arg(&a).arg(&b).arg(&dest);
        if json {
            c.arg("--json");
        }
        let out = c.output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        let e = stderr(&out);
        let ra = e
            .find("root a: 1 matched, 0 missing, 0 mismatched, 0 extra, 0 unreadable")
            .unwrap_or_else(|| panic!("json={json}: {e}"));
        let rb = e
            .find("root b: 1 matched, 0 missing, 0 mismatched, 0 extra, 0 unreadable")
            .unwrap_or_else(|| panic!("json={json}: {e}"));
        assert!(ra < rb, "{e}");
    }
    let gone = d.path().join("gone");
    let out = verify(&[&a, &b, &gone]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let e = stderr(&out);
    let canon = std::fs::canonicalize(d.path()).unwrap().join("gone");
    for n in ["a", "b"] {
        let want = format!(
            "destination does not exist: {}/{n} (every source path is reported missing)",
            canon.display()
        );
        assert!(e.contains(&want), "{e}");
    }
    let o = stdout(&out);
    assert!(o.lines().any(|l| l == "missing: a/x"), "{o}");
    assert!(o.lines().any(|l| l == "missing: b/y"), "{o}");
}

#[test]
fn refusals_match_copy() {
    let d = TempDir::new().unwrap();
    let (a, b, dest) = two_roots(d.path());

    let out = verify(&[&a]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(stderr(&out).contains("error"), "{}", stderr(&out));

    let file_dest = d.path().join("file");
    std::fs::write(&file_dest, b"f").unwrap();
    let absent = d.path().join("absent");
    let out = verify(&[&absent, &b, &file_dest]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));

    let out = verify(&[&a, &b, &file_dest]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("several sources cannot be verified against the existing file"),
        "{}",
        stderr(&out)
    );

    let (x, y) = (d.path().join("x"), d.path().join("y"));
    for p in [&x, &y] {
        std::fs::create_dir_all(p.join("a")).unwrap();
    }
    let out = verify(&[&x.join("a"), &y.join("a"), &dest]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("error:"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));

    let q = d.path().join("q.flux-lock");
    std::fs::create_dir_all(&q).unwrap();
    let out = verify(&[&a, &q, &dest]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(stderr(&out).contains("error"), "{}", stderr(&out));

    #[cfg(unix)]
    {
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let out = verify(&[&b, &link, &dest]);
        assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
        let want = format!(
            "SYMLINK_CREATION_UNAVAILABLE: {}: a symlink SOURCE is not followed; copy its parent folder, or name its target",
            link.display()
        );
        assert!(stderr(&out).contains(&want), "{}", stderr(&out));
    }
}

#[test]
fn help_shows_the_several_source_synopsis_and_guarantee() {
    let out = flux().arg("verify").arg("--help").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let o = stdout(&out);
    assert!(o.contains("SOURCE... DEST"), "{o}");
    assert!(o.contains("With several sources, only"), "{o}");
}

#[cfg(unix)]
#[test]
fn a_root_name_with_a_newline_or_invalid_bytes_prints_escaped_on_stderr() {
    let d = TempDir::new().unwrap();
    let (a, _b, dest) = two_roots(d.path());
    let odd = d.path().join("a\nb");
    std::fs::create_dir_all(&odd).unwrap();
    std::fs::write(odd.join("x"), b"A").unwrap();
    std::fs::create_dir_all(dest.join("a\nb")).unwrap();
    std::fs::write(dest.join("a\nb").join("x"), b"A").unwrap();
    let out = verify(&[&a, &odd, &dest]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.lines().any(|l| l.starts_with("root a\\nb: 1 matched")), "{e:?}");
}

#[test]
fn a_refused_root_check_and_a_refused_alias_exit_2_with_nothing_on_stdout() {
    let d = TempDir::new().unwrap();
    let (a, _b, dest) = two_roots(d.path());
    let out = verify(&[&a, &dest, &dest]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    let e = stderr(&out);
    let canon = std::fs::canonicalize(&dest).unwrap();
    let want = format!(
        "error: root dest: {}/dest: the destination is the source or lies inside it",
        canon.display()
    );
    assert!(e.contains(&want), "{e}");
    assert!(e.trim_end().ends_with("; nothing was verified"), "{e}");
}
