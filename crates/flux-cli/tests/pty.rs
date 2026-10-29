// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The live display of `flux copy` through the real binary on a pseudo-terminal (cut 20): the piped-stderr
//! tests elsewhere never see it, because the display is hidden off a terminal. Linux only; the engine and the
//! renderer are tested headlessly on every platform.
#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::TempDir;

/// A pseudo-terminal pair: the master we read, the slave the child writes to.
fn open_pty() -> (File, OwnedFd) {
    // SAFETY: plain libc calls on descriptors this function opens and owns; each result is checked.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0, "posix_openpt");
        assert_eq!(libc::grantpt(master), 0, "grantpt");
        assert_eq!(libc::unlockpt(master), 0, "unlockpt");
        let mut name = [0 as libc::c_char; 128];
        assert_eq!(libc::ptsname_r(master, name.as_mut_ptr(), name.len()), 0, "ptsname_r");
        let slave = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        assert!(slave >= 0, "open the slave");
        let size = libc::winsize { ws_row: 24, ws_col: 100, ws_xpixel: 0, ws_ypixel: 0 };
        assert_eq!(libc::ioctl(slave, libc::TIOCSWINSZ, &size), 0, "TIOCSWINSZ");
        (File::from_raw_fd(master), OwnedFd::from_raw_fd(slave))
    }
}

/// `src/a` (1 byte) and `src/sub/b` (2 bytes) under `d`, and `other/c` (1 byte).
fn trees_in(d: &Path) {
    let src = d.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a"), b"A").unwrap();
    std::fs::write(src.join("sub").join("b"), b"BB").unwrap();
    std::fs::create_dir_all(d.join("other")).unwrap();
    std::fs::write(d.join("other").join("c"), b"C").unwrap();
}

/// Run `flux copy <extra> <sources> dst` (the sources named under a temporary directory) with stderr on a
/// pseudo-terminal; its exit success and everything it wrote to the terminal.
fn copy_on_a_terminal(extra: &[&str], sources: &[&str]) -> (bool, String) {
    let dir = TempDir::new().unwrap();
    trees_in(dir.path());
    let (mut master, slave) = open_pty();
    let mut child = Command::new(env!("CARGO_BIN_EXE_flux"))
        .arg("copy")
        .args(extra)
        .args(sources.iter().map(|n| dir.path().join(n)))
        .arg(dir.path().join("dst"))
        .env("TERM", "xterm")
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    // The `Stdio` held the only parent copy of the slave and `spawn` has consumed it: the master now reads
    // end-of-file (EIO) once the child exits.
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match master.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
        }
    }
    let status = child.wait().unwrap();
    (status.success(), String::from_utf8_lossy(&bytes).into_owned())
}

/// The terminal control that clears a line: what clearing the bar writes.
const CLEAR_LINE: &str = "\u{1b}[2K";

/// The summary line of a one-source run and of a several-source run.
const ONE_SOURCE: (&[&str], &str) = (&["src"], "copied 2 files");
const SEVERAL_SOURCES: (&[&str], &str) = (&["src", "other"], "root src: 2 copied");

#[test]
fn a_copy_on_a_terminal_draws_the_frame_and_clears_it_before_the_summary() {
    for (sources, marker) in [ONE_SOURCE, SEVERAL_SOURCES] {
        let (ok, t) = copy_on_a_terminal(&[], sources);
        assert!(ok, "{sources:?}: {t:?}");
        let summary = t.find(marker).unwrap_or_else(|| panic!("{sources:?}: no summary: {t:?}"));
        let frame = t[..summary]
            .rfind("Discovered")
            .unwrap_or_else(|| panic!("{sources:?}: no frame: {t:?}"));
        // The bar is cleared after its last frame and before the summary (`live.finish()` runs before any output).
        assert!(
            t[frame..summary].contains(CLEAR_LINE),
            "{sources:?}: the bar is still up at the summary: {t:?}"
        );
    }
}

#[test]
fn json_and_dry_run_on_a_terminal_draw_no_frame() {
    for (sources, _) in [ONE_SOURCE, SEVERAL_SOURCES] {
        for flag in ["--json", "--dry-run"] {
            let (ok, t) = copy_on_a_terminal(&[flag], sources);
            assert!(ok, "{sources:?} {flag}: {t:?}");
            assert!(
                !t.contains("Discovered") && !t.contains(CLEAR_LINE),
                "{sources:?} {flag} drew a frame: {t:?}"
            );
        }
    }
}
