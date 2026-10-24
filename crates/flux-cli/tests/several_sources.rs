// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! `flux copy SRC... DEST` end to end (several-sources cut, task 8): the binary against a real filesystem.

use std::collections::BTreeMap;
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

/// A folder `name` under `base` holding `a` (1 byte) and `sub/b` (2 bytes), both marked with the folder's name.
fn folder(base: &Path, name: &str) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a"), format!("A-{name}")).unwrap();
    std::fs::write(dir.join("sub").join("b"), format!("BB-{name}")).unwrap();
    dir
}

/// Every file under `dir`, by `/`-separated path relative to it, with its bytes; a directory is `None`.
fn snapshot(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if path.is_dir() {
                out.insert(rel, None);
                walk(root, &path, out);
            } else {
                out.insert(rel, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn control_plane_names(dir: &Path) -> Vec<String> {
    snapshot(dir)
        .keys()
        .filter(|k| {
            k.split('/').any(|c| {
                let c = c.to_ascii_lowercase();
                c == ".flux" || c.ends_with(".flux-lock")
            })
        })
        .cloned()
        .collect()
}

#[test]
fn three_positionals_copy_each_source_to_dest_name() {
    let d = TempDir::new().unwrap();
    let (alpha, beta) = (folder(d.path(), "alpha"), folder(d.path(), "beta"));
    let file = d.path().join("note.txt");
    std::fs::write(&file, b"hello").unwrap();
    let out_dir = d.path().join("out");

    let out = flux().arg("copy").arg(&alpha).arg(&file).arg(&beta).arg(&out_dir).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(snapshot(&out_dir.join("alpha")), snapshot(&alpha));
    assert_eq!(snapshot(&out_dir.join("beta")), snapshot(&beta));
    assert_eq!(std::fs::read(out_dir.join("note.txt")).unwrap(), b"hello");
    // A1: no control-plane residue under any root, and the operation workspace is gone.
    assert_eq!(control_plane_names(&out_dir), Vec::<String>::new());
    assert!(!out_dir.join(".flux").exists(), "DEST/.flux is left behind");
}

#[test]
fn the_result_does_not_depend_on_argument_order() {
    let d = TempDir::new().unwrap();
    let (alpha, beta) = (folder(d.path(), "alpha"), folder(d.path(), "beta"));
    let file = d.path().join("note.txt");
    std::fs::write(&file, b"hello").unwrap();
    let (one, two) = (d.path().join("one"), d.path().join("two"));

    let a = flux().arg("copy").arg(&alpha).arg(&beta).arg(&file).arg(&one).output().unwrap();
    let b = flux().arg("copy").arg(&file).arg(&beta).arg(&alpha).arg(&two).output().unwrap();

    assert_eq!(a.status.code(), Some(0), "stderr: {}", stderr(&a));
    assert_eq!(b.status.code(), Some(0), "stderr: {}", stderr(&b));
    assert_eq!(snapshot(&one), snapshot(&two));
}

#[test]
fn nested_sources_copy_twice() {
    let d = TempDir::new().unwrap();
    let p = folder(d.path(), "p");
    let q = folder(&p, "q");
    let out_dir = d.path().join("out");

    let out = flux().arg("copy").arg(&p).arg(&q).arg(&out_dir).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(snapshot(&out_dir.join("q")), snapshot(&q));
    assert_eq!(snapshot(&out_dir.join("p").join("q")), snapshot(&q));
    assert_eq!(std::fs::read(out_dir.join("p").join("a")).unwrap(), b"A-p");
}

#[test]
fn dest_is_created_when_missing() {
    let d = TempDir::new().unwrap();
    let (alpha, beta) = (folder(d.path(), "alpha"), folder(d.path(), "beta"));
    let out_dir = d.path().join("not-yet");
    assert!(!out_dir.exists());

    let out = flux().arg("copy").arg(&alpha).arg(&beta).arg(&out_dir).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(out_dir.join("alpha").join("a").is_file() && out_dir.join("beta").join("a").is_file());
}

#[test]
fn dest_that_is_a_file_exits_2() {
    let d = TempDir::new().unwrap();
    let (alpha, beta) = (folder(d.path(), "alpha"), folder(d.path(), "beta"));
    let dest = d.path().join("dest");
    std::fs::write(&dest, b"keep").unwrap();

    let out = flux().arg("copy").arg("--json").arg(&alpha).arg(&beta).arg(&dest).output().unwrap();

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("error: ") && stderr(&out).contains("existing file"),
        "{}",
        stderr(&out)
    );
    assert!(out.stdout.is_empty(), "a usage error prints no JSON");
    assert_eq!(std::fs::read(&dest).unwrap(), b"keep");
}

#[test]
fn resume_restart_and_dry_run_exit_2_naming_the_flag() {
    let d = TempDir::new().unwrap();
    let (alpha, beta) = (folder(d.path(), "alpha"), folder(d.path(), "beta"));
    let out_dir = d.path().join("out");
    for flag in ["--resume", "--restart", "--dry-run"] {
        let out = flux()
            .arg("copy")
            .arg(flag)
            .arg("--json")
            .arg(&alpha)
            .arg(&beta)
            .arg(&out_dir)
            .output()
            .unwrap();
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{flag}: {err}");
        assert!(
            err.contains(&format!("error: {flag} with several sources is not supported yet")),
            "{flag}: {err}"
        );
        assert_eq!(
            err.contains("flux cleanup --force"),
            flag == "--restart",
            "only --restart carries the cleanup hint: {flag}: {err}"
        );
        assert!(out.stdout.is_empty(), "{flag}: a usage error prints no JSON");
        assert!(!out_dir.exists(), "{flag}: nothing may be created");
    }
}

#[test]
fn byte_equal_names_exit_3_with_the_code_and_dest_untouched() {
    let d = TempDir::new().unwrap();
    let one = folder(&d.path().join("x"), "same");
    let two = folder(&d.path().join("y"), "same");
    let out_dir = d.path().join("out");

    let out = flux().arg("copy").arg(&one).arg(&two).arg(&out_dir).output().unwrap();

    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("DESTINATION_NAMESPACE_COLLISION"), "{}", stderr(&out));
    assert!(!out_dir.exists(), "DEST must be untouched");
}

#[test]
fn a_dot_flux_source_exits_3() {
    let d = TempDir::new().unwrap();
    let hidden = folder(d.path(), ".flux");
    let beta = folder(d.path(), "beta");
    let out_dir = d.path().join("out");

    let out = flux().arg("copy").arg(&hidden).arg(&beta).arg(&out_dir).output().unwrap();

    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("CONTROL_PLANE_NAMESPACE_CONFLICT"), "{}", stderr(&out));
    assert!(!out_dir.exists());
}

#[test]
fn a_missing_source_exits_1() {
    let d = TempDir::new().unwrap();
    let beta = folder(d.path(), "beta");
    let missing = d.path().join("no-such-source");
    let out_dir = d.path().join("out");

    let out = flux().arg("copy").arg(&beta).arg(&missing).arg(&out_dir).output().unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("no-such-source"), "{}", stderr(&out));
    assert!(!out_dir.exists());
}

#[cfg(unix)]
#[test]
fn a_symlink_source_exits_1_naming_it() {
    let d = TempDir::new().unwrap();
    let (alpha, beta) = (folder(d.path(), "alpha"), folder(d.path(), "beta"));
    let link = d.path().join("the-link");
    std::os::unix::fs::symlink(&beta, &link).unwrap();
    let out_dir = d.path().join("out");

    let out = flux().arg("copy").arg(&alpha).arg(&link).arg(&out_dir).output().unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("the-link"), "{}", stderr(&out));
    assert!(!out_dir.exists());
}

#[test]
fn json_has_roots_for_several_sources_and_not_for_one() {
    let d = TempDir::new().unwrap();
    let (alpha, zeta) = (folder(d.path(), "alpha"), folder(d.path(), "zeta"));
    let file = d.path().join("mid.txt");
    std::fs::write(&file, b"hello").unwrap();
    let out_dir = d.path().join("out");

    // Argument order is zeta, mid.txt, alpha; the report is in prefix order.
    let out = flux()
        .arg("copy")
        .arg("--json")
        .arg(&zeta)
        .arg(&file)
        .arg(&alpha)
        .arg(&out_dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = json(&out);
    assert_eq!(v.as_object().unwrap().len(), 19);
    // `serde_json::Value` sorts its keys: the order is read from the text.
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.find("\"hash_algorithm\"").unwrap() < text.find("\"roots\"").unwrap(),
        "roots comes after the 18 keys: {text}"
    );
    assert!(text.trim_end().ends_with("}]}"), "roots is the last key: {text}");
    let roots = v["roots"].as_array().unwrap();
    let names: Vec<&str> = roots.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["alpha", "mid.txt", "zeta"]);
    for r in roots {
        assert_eq!(r["status"], "completed");
        assert_eq!(r["files_failed"], 0);
        assert_eq!(r.as_object().unwrap().len(), 4);
    }
    assert_eq!(roots[0]["files_copied"], 2);
    assert_eq!(roots[1]["files_copied"], 1);
    assert!(stderr(&out).contains("root alpha: 2 copied, 0 failed"), "{}", stderr(&out));

    let one =
        flux().arg("copy").arg("--json").arg(&alpha).arg(d.path().join("single")).output().unwrap();
    assert_eq!(one.status.code(), Some(0), "stderr: {}", stderr(&one));
    let v = json(&one);
    assert_eq!(v.as_object().unwrap().len(), 18);
    assert!(v.get("roots").is_none());
}

/// Whether this process (and so the child it spawns) runs as root, which reads any file.
#[cfg(unix)]
fn running_as_root() -> bool {
    let out = Command::new("id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim() == "0"
}

#[cfg(unix)]
#[test]
fn a_failing_file_in_root_one_still_copies_root_two_and_exits_1() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        eprintln!("skipped: root reads an unreadable file");
        return;
    }
    let d = TempDir::new().unwrap();
    let (alpha, beta) = (folder(d.path(), "alpha"), folder(d.path(), "beta"));
    let locked = alpha.join("locked");
    std::fs::write(&locked, b"secret").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let out_dir = d.path().join("out");

    let out =
        flux().arg("copy").arg("--json").arg(&alpha).arg(&beta).arg(&out_dir).output().unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert_eq!(snapshot(&out_dir.join("beta")), snapshot(&beta), "root two is complete");
    assert!(!out_dir.join("alpha").join("locked").exists());
    let v = json(&out);
    let roots = v["roots"].as_array().unwrap();
    assert_eq!(
        (roots[0]["name"].as_str(), roots[0]["status"].as_str()),
        (Some("alpha"), Some("failed"))
    );
    assert_eq!(roots[0]["files_failed"], 1);
    assert_eq!(
        (roots[1]["name"].as_str(), roots[1]["status"].as_str()),
        (Some("beta"), Some("completed"))
    );
    assert!(v["files_failed"].as_u64().unwrap() >= 1);
}

#[test]
fn an_alias_is_refused_on_a_case_insensitive_destination() {
    let d = TempDir::new().unwrap();
    // Probe the filesystem DEST will live on.
    let probe = d.path().join("probe");
    std::fs::write(&probe, b"x").unwrap();
    let insensitive = d.path().join("PROBE").exists();
    let lower = folder(&d.path().join("x"), "p");
    let upper = folder(&d.path().join("y"), "P");
    let out_dir = d.path().join("out");

    let out = flux().arg("copy").arg(&lower).arg(&upper).arg(&out_dir).output().unwrap();

    if insensitive {
        assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
        assert!(stderr(&out).contains("DESTINATION_NAMESPACE_COLLISION"), "{}", stderr(&out));
        assert!(!out_dir.join("p").exists() && !out_dir.join("P").exists());
    } else {
        assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
        assert_eq!(snapshot(&out_dir.join("p")), snapshot(&lower));
        assert_eq!(snapshot(&out_dir.join("P")), snapshot(&upper));
    }
}

#[test]
fn a_file_source_over_an_existing_directory_exits_3() {
    let d = TempDir::new().unwrap();
    let beta = folder(d.path(), "beta");
    let file = d.path().join("thing");
    std::fs::write(&file, b"file").unwrap();
    let out_dir = d.path().join("out");
    std::fs::create_dir_all(out_dir.join("thing")).unwrap();
    std::fs::write(out_dir.join("thing").join("keep"), b"keep").unwrap();

    let out = flux().arg("copy").arg(&beta).arg(&file).arg(&out_dir).output().unwrap();

    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert_eq!(std::fs::read(out_dir.join("thing").join("keep")).unwrap(), b"keep");
    assert!(!out_dir.join("beta").exists(), "refused before any root is copied");
}

#[cfg(unix)]
#[test]
fn a_symlink_at_a_folder_roots_destination_name_exits_3_and_copies_nothing() {
    let d = TempDir::new().unwrap();
    let (a, b) = (folder(&d.path().join("src"), "a"), folder(&d.path().join("src"), "b"));
    let out_dir = d.path().join("dest");
    std::fs::create_dir(&out_dir).unwrap();
    let elsewhere = d.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, out_dir.join("b")).unwrap();

    let out = flux().arg("copy").arg(&a).arg(&b).arg(&out_dir).output().unwrap();

    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("DESTINATION_NAMESPACE_COLLISION"), "{}", stderr(&out));
    assert!(!out_dir.join("a").exists(), "root a must not be copied");
    assert!(!out_dir.join(".flux").exists(), "no workspace is left");
    assert!(control_plane_names(&out_dir).is_empty(), "{:?}", control_plane_names(&out_dir));
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing went through the link");
}
