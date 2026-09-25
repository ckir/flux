#![cfg(unix)]

use flux_fs::{DestinationRoot, DirHandle};
use flux_platform::StdFileSystem;
use std::ffi::OsStr;
use tempfile::TempDir;

#[test]
fn a_child_directory_opens() {
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("child")).unwrap();
    let root = StdFileSystem.destination_root(d.path()).unwrap();
    assert!(root.open_dir(OsStr::new("child")).is_ok());
}

#[test]
fn a_symlinked_component_is_refused_as_a_safety_rejection() {
    // THE test this whole cut exists for. An attacker replaces a directory Flux
    // created with a symlink pointing outside DEST; the open must refuse rather
    // than traverse, and it must say SAFETY_REJECTED rather than a generic error,
    // because §149.7 distinguishes the two.
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("real")).unwrap();
    std::os::unix::fs::symlink("/etc", d.path().join("escape")).unwrap();

    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let err = root.open_dir(OsStr::new("escape")).unwrap_err();
    assert_eq!(err.code, flux_fs::Code::SafetyRejected);
}

#[test]
fn a_plain_file_component_is_a_destination_error_not_a_safety_rejection() {
    // The distinction that costs an extra stat. MEASURED: `openat` answers ENOTDIR
    // for BOTH a symlink and a plain file, so the errno alone cannot tell them
    // apart -- and §149.7 wants DESTINATION_ERROR here and SAFETY_REJECTED above.
    // If this test and the one above ever report the same code, the stat was lost.
    let d = TempDir::new().unwrap();
    std::fs::write(d.path().join("plain"), b"x").unwrap();

    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let err = root.open_dir(OsStr::new("plain")).unwrap_err();
    assert_eq!(err.code, flux_fs::Code::DestinationError);
}

#[test]
fn a_missing_component_is_a_destination_error() {
    let d = TempDir::new().unwrap();
    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let err = root.open_dir(OsStr::new("absent")).unwrap_err();
    assert_eq!(err.code, flux_fs::Code::DestinationError);
}

#[test]
fn a_separator_is_refused_before_the_filesystem_is_touched() {
    let d = TempDir::new().unwrap();
    std::fs::create_dir_all(d.path().join("a/b")).unwrap();
    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let err = root.open_dir(OsStr::new("a/b")).unwrap_err();
    assert_eq!(err.code, flux_fs::Code::SafetyRejected);
}

#[test]
fn a_created_file_lands_in_the_handles_directory() {
    use std::io::Write;
    let d = TempDir::new().unwrap();
    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let mut w = root.create_new(OsStr::new("made")).unwrap();
    w.write_all(b"payload").unwrap();
    drop(w);
    assert_eq!(std::fs::read(d.path().join("made")).unwrap(), b"payload");
}

#[test]
fn the_handle_still_addresses_the_original_directory_after_a_swap() {
    // Item 114's attack, and the property the whole design buys. The handle is
    // opened, the NAME is then repointed at somewhere else, and a write through
    // the handle must still land in the ORIGINAL directory rather than following
    // the new name. A path-based writer fails this; a handle-based one cannot.
    use std::io::Write;
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("target")).unwrap();
    std::fs::create_dir(d.path().join("elsewhere")).unwrap();

    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let held = root.open_dir(OsStr::new("target")).unwrap();

    // RENAME aside rather than remove, and the difference is not stylistic.
    // MEASURED: rmdir marks the inode IS_DEADDIR, after which the kernel refuses
    // EVERY new entry under it -- ENOENT -- even through a file descriptor that is
    // still open. A remove-based swap therefore tests nothing this code does; it
    // tests that Linux forbids the whole operation. Renaming keeps the directory
    // alive, and the assertion is sharper for it: the write must follow the INODE
    // the handle holds, not the NAME that was swapped.
    std::fs::rename(d.path().join("target"), d.path().join("target.bak")).unwrap();
    std::os::unix::fs::symlink(d.path().join("elsewhere"), d.path().join("target")).unwrap();

    let mut w = held.create_new(OsStr::new("payload")).unwrap();
    w.write_all(b"x").unwrap();
    drop(w);

    assert!(
        !d.path().join("elsewhere/payload").exists(),
        "the write followed the swapped name; the handle was not load-bearing"
    );
    assert!(
        d.path().join("target.bak/payload").exists(),
        "the write must land in the directory the handle holds, under its new name"
    );
}
