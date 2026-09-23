use flux_fs::{FileIdentity, FileSystem};
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
    // The refusal must come from OUR guard, not from whatever the OS happened to do.
    // Windows' own `rename` ALSO refuses a read-only target, so without this the test
    // passes even with the guard deleted -- measured by mutation: removing the
    // attribute check left all seven tests green. Our error carries no raw OS code;
    // an OS refusal would carry ERROR_ACCESS_DENIED.
    assert!(err.source.raw_os_error().is_none(), "the guard must refuse, not the OS");
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

#[test]
#[cfg(windows)]
fn rename_replace_refuses_a_target_denied_by_acl() {
    // The Windows case the read-only ATTRIBUTE cannot see, and the mirror of the
    // Unix root-owned-0644 case. Measured before the fix: the attribute was unset,
    // the guard passed, `rename` returned Ok, and the protected contents were gone.
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    let mut f = fs.create_new(&from).unwrap();
    f.write_all(b"new").unwrap();
    drop(f);
    std::fs::write(&to, b"protected").unwrap();

    let user = std::env::var("USERNAME").expect("USERNAME");
    let denied = std::process::Command::new("icacls")
        .args([&to.display().to_string(), "/deny", &format!("{user}:(W,D,DC)")])
        .output()
        .is_ok_and(|o| o.status.success());
    if !denied {
        eprintln!("skipped: could not apply a deny ACE");
        return;
    }

    // The attribute alone is blind here: that is the whole point of the case.
    assert!(!std::fs::metadata(&to).unwrap().permissions().readonly(), "attribute is unset");

    let refused = fs.rename_replace(&from, &to).is_err();

    // Drop the deny ACE BEFORE reading back: it denies (W,D,DC), and reading through
    // it is not something this test should depend on.
    let _ = std::process::Command::new("icacls")
        .args([&to.display().to_string(), "/remove:d", &user])
        .output();
    let content = std::fs::read(&to).expect("the destination must still be readable");

    assert!(refused, "a destination we may not write must be refused");
    assert_eq!(content, b"protected", "the protected contents must survive");
}

#[test]
fn two_hardlinks_to_one_file_share_an_identity() {
    // The property that makes identity worth having: the SAME object reached by two
    // different paths reports the SAME id, which no path comparison can tell you.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::write(&a, b"x").unwrap();
    std::fs::hard_link(&a, &b).unwrap();

    let fs = StdFileSystem;
    let ia = fs.metadata(&a).unwrap().identity;
    let ib = fs.metadata(&b).unwrap().identity;

    assert!(matches!(ia, FileIdentity::Strong(_)), "local fs must be strong, got {ia:?}");
    assert_eq!(ia, ib, "two links to one object are one object");
}

#[test]
fn two_distinct_files_have_distinct_identities() {
    // The control. Without it, an implementation returning one constant id for
    // everything would pass the test above.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::write(&a, b"x").unwrap();
    std::fs::write(&b, b"x").unwrap();

    let fs = StdFileSystem;
    assert_ne!(fs.metadata(&a).unwrap().identity, fs.metadata(&b).unwrap().identity);
}

#[test]
fn a_directory_has_an_identity() {
    // On Windows this is what FILE_FLAG_BACKUP_SEMANTICS buys: without it the open
    // fails on a directory and identity degrades to Unavailable for exactly the
    // entries the walk needs it for.
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();

    let id = StdFileSystem.metadata(&sub).unwrap().identity;
    assert!(matches!(id, FileIdentity::Strong(_)), "directories need identity too, got {id:?}");
}

#[test]
fn one_directory_reached_two_ways_is_one_object() {
    // `.` inside a directory is that same directory. If identity did not survive the
    // spelling of the path, a later walk's ancestor check would never fire.
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let same = sub.join(".");

    let fs = StdFileSystem;
    let a = fs.metadata(&sub).unwrap().identity;
    let b = fs.metadata(&same).unwrap().identity;

    assert!(matches!(a, FileIdentity::Strong(_)), "got {a:?}");
    assert_eq!(a, b, "one directory, two spellings, one identity");
}

#[test]
fn a_symlink_reports_its_own_identity_not_its_targets() {
    // The mutation this catches: dropping FILE_FLAG_OPEN_REPARSE_POINT on Windows, or
    // using `metadata` instead of `symlink_metadata` on Unix. Either makes a link
    // report its TARGET's identity, and a later walk would compare a link against the
    // target's id - taking an ordinary directory for a cycle, or missing a real one.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let link = dir.path().join("link");

    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &link).is_ok();
    #[cfg(windows)]
    let made = {
        // A symlink needs Developer Mode or SeCreateSymbolicLinkPrivilege, and a stock
        // machine has neither - so this used to SKIP, leaving the only Windows coverage
        // of FILE_FLAG_OPEN_REPARSE_POINT at zero. MEASURED: removing that flag left the
        // whole Windows suite green.
        //
        // A JUNCTION needs no privilege (MEASURED: `New-Item -ItemType Junction`
        // succeeds unelevated) and is also a name-surrogate reparse point, so it
        // exercises the same flag. Try the symlink first, fall back to the junction.
        std::os::windows::fs::symlink_dir(&target, &link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&target)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
    };

    if !made {
        // Windows needs Developer Mode or SeCreateSymbolicLinkPrivilege. Say so loudly:
        // a silent skip is how a test stops protecting the platform it was written for.
        eprintln!("SKIPPED a_symlink_reports_its_own_identity: could not create a symlink here");
        return;
    }

    let fs = StdFileSystem;
    let target_id = fs.metadata(&target).unwrap().identity;
    let link_id = fs.metadata(&link).unwrap().identity;

    assert!(matches!(target_id, FileIdentity::Strong(_)), "target: {target_id:?}");
    assert!(matches!(link_id, FileIdentity::Strong(_)), "link: {link_id:?}");
    assert_ne!(target_id, link_id, "a symlink is its own object, not its target");
}
