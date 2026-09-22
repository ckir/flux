use flux_fs::FileSystem;
use flux_platform::StdFileSystem;
use std::io::Write;
use tempfile::TempDir;

#[test]
fn create_new_is_exclusive() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("a");
    let fs = StdFileSystem;
    fs.create_new(&p).unwrap();
    assert!(fs.create_new(&p).is_err(), "FS-1: a second exclusive create must fail");
}

#[test]
fn rename_replace_replaces_an_existing_target() {
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    let mut f = fs.create_new(&from).unwrap();
    f.write_all(b"new").unwrap();
    drop(f);
    std::fs::write(&to, b"old").unwrap();

    fs.rename_replace(&from, &to).unwrap();
    assert_eq!(std::fs::read(&to).unwrap(), b"new");
}

#[test]
fn rename_replace_refuses_a_read_only_target() {
    // Plain `cp` refuses to overwrite a read-only file; only `cp -f` replaces it.
    // `std::fs::rename` checks the DIRECTORY's write permission, not the file's, so
    // without this guard a copy silently destroys a protection the user set. Measured:
    // on Linux `cp a b` with `b` read-only fails "Permission denied" while
    // `std::fs::rename` onto it succeeds; on Windows the rename itself fails. This
    // pins the refusal on BOTH platforms, so the behaviour no longer depends on which
    // one you are standing on.
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    fs.create_new(&from).unwrap();
    std::fs::write(&to, b"protected").unwrap();

    let mut perms = std::fs::metadata(&to).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&to, perms).unwrap();

    let err = fs.rename_replace(&from, &to).unwrap_err();

    assert_eq!(err.code, flux_fs::Code::PermissionDenied);
    assert_eq!(std::fs::read(&to).unwrap(), b"protected", "the protected file must survive");

    // Remove it here rather than clearing the read-only bit: `set_readonly(false)`
    // grants world-write on Unix, which clippy rightly refuses. `remove_file` handles
    // a read-only file on both platforms, so this both cleans up after TempDir and
    // pins that property.
    std::fs::remove_file(&to).expect("a read-only file must still be removable");
}

#[cfg(unix)]
#[test]
fn rename_replace_allows_a_symlink_whose_target_is_read_only() {
    // The read-only guard must judge the NAME being replaced, not what it points at.
    // `rename` replaces the symlink itself and never touches the target, so refusing
    // here is a false positive: a copy that should succeed and does not. Unix-only
    // because creating a symlink on Windows needs Developer Mode.
    let d = TempDir::new().unwrap();
    let target = d.path().join("target");
    let link = d.path().join("link");
    let from = d.path().join("from");

    std::fs::write(&target, b"protected").unwrap();
    let mut perms = std::fs::metadata(&target).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&target, perms).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let fs = StdFileSystem;
    let mut f = fs.create_new(&from).unwrap();
    f.write_all(b"new").unwrap();
    drop(f);

    fs.rename_replace(&from, &link).expect("replacing a symlink must not consult its target");

    assert_eq!(std::fs::read(&target).unwrap(), b"protected", "the target must be untouched");
    assert_eq!(std::fs::read(&link).unwrap(), b"new", "the name must now hold the copy");

    std::fs::remove_file(&target).expect("a read-only file must still be removable");
}

#[test]
fn rename_no_replace_refuses_an_existing_target() {
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    fs.create_new(&from).unwrap();
    std::fs::write(&to, b"old").unwrap();

    assert!(fs.rename_no_replace(&from, &to).is_err(), "FS-2");
    assert_eq!(std::fs::read(&to).unwrap(), b"old");
}

#[test]
fn set_times_on_a_handle_moves_the_mtime() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("a");
    let fs = StdFileSystem;
    let h = fs.create_new(&p).unwrap();

    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    fs.set_times(&h, Some(t)).unwrap();
    drop(h);

    let got = std::fs::metadata(&p).unwrap().modified().unwrap();
    assert_eq!(got, t);
}

#[cfg(unix)]
#[test]
fn metadata_reports_a_symlink_as_not_a_file() {
    // Unix-only: creating a symlink on Windows needs Developer Mode or admin, which a
    // CI runner may not have. The code path under test is not platform-specific.
    let d = TempDir::new().unwrap();
    let target = d.path().join("target");
    std::fs::write(&target, b"hello").unwrap();
    let link = d.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let m = StdFileSystem.metadata(&link).unwrap();
    assert!(!m.is_file, "a symlink must not read as a regular file, or the refusal is bypassed");
}

#[test]
fn metadata_reports_a_directory_as_not_a_file() {
    let d = TempDir::new().unwrap();
    let fs = StdFileSystem;
    assert!(!fs.metadata(d.path()).unwrap().is_file);
}

#[test]
#[cfg(unix)]
fn rename_no_replace_refuses_a_dangling_symlink() {
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    fs.create_new(&from).unwrap();
    // `Path::exists` FOLLOWS the link, so a dangling symlink reports false while the
    // NAME `to` is occupied. Replacing it destroys a link the user created, which is
    // exactly what this method promises not to do. Same root cause as the
    // `symlink_metadata` fix in `rename_replace`; this is the sibling site.
    std::os::unix::fs::symlink(d.path().join("nowhere"), &to).unwrap();

    assert!(
        fs.rename_no_replace(&from, &to).is_err(),
        "FS-2: a dangling symlink occupies the name"
    );
    let m = std::fs::symlink_metadata(&to).expect("the link itself must survive");
    assert!(m.file_type().is_symlink(), "the link must not have been replaced");
}

#[test]
#[cfg(unix)]
fn rename_replace_refuses_a_target_this_user_cannot_write() {
    // The case `Permissions::readonly()` cannot see. A root-owned 0644 file has the
    // owner write bit SET, so `readonly()` reports false and the old guard passed --
    // yet this user cannot write it, and `cp` refuses with "Permission denied".
    // `rename(2)` would replace it anyway, because rename consults the DIRECTORY's
    // permission and never the file's. Measured before the fix: the file was
    // destroyed by a normal user.
    //
    // Needs a file owned by somebody else, so it needs root to build the fixture.
    // Skips rather than fails where that is not available, so it never goes red
    // spuriously; GitHub's ubuntu and macos runners both grant passwordless sudo.
    if !std::process::Command::new("sudo").args(["-n", "true"]).status().is_ok_and(|s| s.success())
    {
        eprintln!("skipped: no passwordless sudo, cannot build a root-owned fixture");
        return;
    }

    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    let mut f = fs.create_new(&from).unwrap();
    f.write_all(b"new").unwrap();
    drop(f);

    let sh = format!("echo protected > {t} && chmod 0644 {t} && chown 0:0 {t}", t = to.display());
    assert!(
        std::process::Command::new("sudo")
            .args(["-n", "sh", "-c", &sh])
            .status()
            .unwrap()
            .success(),
        "fixture"
    );

    // The owner write bit is set, so the attribute check alone would let this through.
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&to).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o644, "the fixture must look writable to a bit check");
    assert!(!std::fs::metadata(&to).unwrap().permissions().readonly(), "readonly() is blind here");

    assert!(fs.rename_replace(&from, &to).is_err(), "a file we cannot write must be refused");
    assert_eq!(
        std::process::Command::new("sudo")
            .args(["-n", "cat", &to.display().to_string()])
            .output()
            .unwrap()
            .stdout,
        b"protected\n",
        "the protected file must survive"
    );

    let _ = std::process::Command::new("sudo")
        .args(["-n", "rm", "-f", &to.display().to_string()])
        .status();
}
