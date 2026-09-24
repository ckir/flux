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
fn rename_no_replace_refuses_a_directory_occupying_the_name() {
    // A directory at the target is a case the old check-then-act body got right only
    // by accident -- `symlink_metadata` says "something is there" without saying what.
    // The atomic primitives refuse it as the OS's own answer, and on every platform.
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    fs.create_new(&from).unwrap();
    std::fs::create_dir(&to).unwrap();

    let err = fs.rename_no_replace(&from, &to).expect_err("a directory occupies the name");
    assert_eq!(err.code, flux_fs::Code::IoError);

    assert!(std::fs::metadata(&to).unwrap().is_dir(), "the directory must survive");
    assert!(from.exists(), "the source must be untouched");
}

#[test]
fn rename_no_replace_succeeds_when_the_name_is_free() {
    // The happy path had no test at all: both existing tests assert refusal, so an
    // implementation that refused EVERYTHING would have passed them both.
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    let mut f = fs.create_new(&from).unwrap();
    f.write_all(b"payload").unwrap();
    drop(f);

    fs.rename_no_replace(&from, &to).expect("a free name must be claimable");

    assert_eq!(std::fs::read(&to).unwrap(), b"payload");
    assert!(!from.exists(), "the source name must be gone after a rename");
}

#[test]
fn rename_no_replace_reports_a_missing_source() {
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("absent"), d.path().join("to"));
    let fs = StdFileSystem;

    let err = fs.rename_no_replace(&from, &to).expect_err("there is nothing to rename");
    assert_eq!(err.code, flux_fs::Code::IoError);
    assert!(!to.exists(), "nothing may appear at the target");
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
    assert_eq!(
        m.file_type,
        flux_fs::FileType::Symlink,
        "a symlink must report as a symlink, or the refusal is bypassed"
    );
}

#[test]
fn metadata_reports_a_directory_as_not_a_file() {
    let d = TempDir::new().unwrap();
    let fs = StdFileSystem;
    assert_eq!(fs.metadata(d.path()).unwrap().file_type, flux_fs::FileType::Dir);
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

    // `Result<(), String>` rather than a bool: a bare bool forced the assert to GUESS
    // why, and it guessed a Windows cause even on Unix. The reason travels with the
    // failure instead.
    #[cfg(unix)]
    let made: Result<(), String> =
        std::os::unix::fs::symlink(&target, &link).map_err(|e| format!("symlink failed: {e}"));

    #[cfg(windows)]
    let made: Result<(), String> = make_dir_reparse_point(&target, &link);

    // NOT a skip. On Windows a junction needs no privilege and on Unix a symlink needs
    // none either, so failing to create one is a broken environment rather than an
    // absent capability - and a silent skip reports `ok` while testing nothing, which is
    // how this property came to have no coverage on Windows in the first place.
    if let Err(why) = made {
        panic!(
            "could not create a reparse point at {}: {why}. This is the only test covering \
             the no-follow behaviour on this platform, so it fails rather than skipping.",
            link.display()
        );
    }

    let fs = StdFileSystem;
    let target_id = fs.metadata(&target).unwrap().identity;
    let link_id = fs.metadata(&link).unwrap().identity;

    assert!(matches!(target_id, FileIdentity::Strong(_)), "target: {target_id:?}");
    assert!(matches!(link_id, FileIdentity::Strong(_)), "link: {link_id:?}");
    assert_ne!(target_id, link_id, "a symlink is its own object, not its target");
}

/// Create a directory reparse point at `link` pointing at `target`, returning whether
/// it worked. Prefers a symlink; falls back to a JUNCTION, which needs no privilege
/// where a symlink needs Developer Mode or SeCreateSymbolicLinkPrivilege.
///
/// MEASURED: `New-Item -ItemType Junction` succeeds unelevated, and a junction is a
/// name-surrogate reparse point, so it exercises FILE_FLAG_OPEN_REPARSE_POINT exactly
/// as a symlink does.
#[cfg(windows)]
fn make_dir_reparse_point(target: &std::path::Path, link: &std::path::Path) -> Result<(), String> {
    if std::os::windows::fs::symlink_dir(target, link).is_ok() {
        return Ok(());
    }
    // Separate args, NOT one quoted string: MEASURED, `cmd /C` strips outer quotes and
    // then misreads nested ones, creating nothing even for an ordinary path. Separate
    // args work for ordinary paths and paths with spaces; they do not survive a `&`.
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output();
    // The POSTCONDITION, never the exit status: MEASURED, `mklink /J` returns 0 even
    // when its target does not exist. The stat error is REPORTED, not swallowed.
    match std::fs::symlink_metadata(link) {
        Ok(m) if m.file_type().is_symlink() => Ok(()),
        Ok(m) => Err(format!("path exists but is not a reparse point: {:?}", m.file_type())),
        Err(e) => Err(format!(
            "nothing at the link path after `mklink /J` ({e}); a `&` in the temp path is the likeliest cause, since cmd.exe treats it as a command separator. cmd said: {}",
            out.map(|o| {
                // BOTH streams: cmd writes its failure to stderr, so reporting stdout
                // alone printed an empty string on the one path that matters.
                let mut said = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                let err = String::from_utf8_lossy(&o.stderr).trim().to_owned();
                if !err.is_empty() {
                    said.push_str(&err);
                }
                said
            })
            .unwrap_or_else(|e| format!("<could not run cmd: {e}>"))
        )),
    }
}

#[cfg(windows)]
#[test]
fn metadata_reports_a_reparse_point_as_not_a_file() {
    // The Windows half of `metadata_reports_a_symlink_as_not_a_file`, which is
    // `#[cfg(unix)]` because symlinks used to need Developer Mode. A junction does not,
    // so that justification is obsolete and the property is testable here after all.
    //
    // It matters most on THIS platform: Windows is where `metadata` and
    // `symlink_metadata` diverge on reparse points, and a walk that gates descent on
    // `is_file` would follow a link it should have reported.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let link = dir.path().join("link");

    if let Err(why) = make_dir_reparse_point(&target, &link) {
        panic!("could not create a reparse point at {}: {why}", link.display());
    }

    let m = StdFileSystem.metadata(&link).unwrap();
    assert_eq!(
        m.file_type,
        flux_fs::FileType::Symlink,
        "a name-surrogate reparse point reports as a symlink"
    );
}

#[test]
fn read_dir_lists_children_with_their_types() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("sub")).unwrap();
    std::fs::write(d.path().join("f.txt"), b"x").unwrap();

    let fs = StdFileSystem;
    let mut got: Vec<(String, flux_fs::FileType)> = fs
        .read_dir(d.path())
        .unwrap()
        .into_iter()
        .map(|e| (e.name.to_string_lossy().into_owned(), e.file_type))
        .collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(
        got,
        vec![
            ("f.txt".to_string(), flux_fs::FileType::File),
            ("sub".to_string(), flux_fs::FileType::Dir),
        ]
    );
}

#[test]
fn read_dir_on_an_empty_directory_is_empty_not_an_error() {
    // An empty directory is exactly what a tree copy must handle, and the old fake
    // could not even express one.
    let d = tempfile::tempdir().unwrap();
    assert!(StdFileSystem.read_dir(d.path()).unwrap().is_empty());
}

#[test]
fn read_dir_on_a_missing_path_fails() {
    let d = tempfile::tempdir().unwrap();
    assert!(StdFileSystem.read_dir(&d.path().join("nope")).is_err());
}

#[test]
fn create_dir_makes_one_level_and_refuses_a_missing_parent() {
    let d = tempfile::tempdir().unwrap();
    let fs = StdFileSystem;

    fs.create_dir(&d.path().join("a")).unwrap();
    assert_eq!(fs.metadata(&d.path().join("a")).unwrap().file_type, flux_fs::FileType::Dir);

    // Fails if the parent is missing: the walk creates ancestors in order, so it
    // never needs the recursive form, and silently creating them would hide a bug.
    assert!(fs.create_dir(&d.path().join("missing/b")).is_err());
    // And refuses to replace something that is already there.
    assert!(fs.create_dir(&d.path().join("a")).is_err());
}

/// Create a directory link at `link` pointing at `target`, by whatever mechanism the
/// platform allows WITHOUT privilege.
///
/// Two arms because the test below asserts a CROSS-PLATFORM property and was measured
/// on both, so gating it to one platform would leave the other half of the claim
/// untested. `cfg(unix)` covers macOS, so between them the two arms cover every target
/// the CI matrix builds.
#[cfg(windows)]
fn make_dir_link(target: &std::path::Path, link: &std::path::Path) -> Result<(), String> {
    make_dir_reparse_point(target, link)
}

#[cfg(unix)]
fn make_dir_link(target: &std::path::Path, link: &std::path::Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link).map_err(|e| e.to_string())
}

#[test]
fn read_dir_follows_a_symlink_to_a_directory_which_is_why_file_type_is_checked() {
    // This is the reachable tree escape. MEASURED on Windows via a junction and on
    // Linux via a symlink, identically: read_dir on the link enumerates the TARGET.
    // So a walk that descended on `!is_file` alone would copy files from outside the
    // tree. `Metadata.file_type` is what lets the walk refuse.
    let d = tempfile::tempdir().unwrap();
    let real = d.path().join("real");
    let link = d.path().join("link");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("marker.txt"), b"x").unwrap();
    if make_dir_link(&real, &link).is_err() {
        eprintln!("skipped: cannot create a directory link here");
        return;
    }

    let fs = StdFileSystem;
    let names: Vec<String> = fs
        .read_dir(&link)
        .unwrap()
        .into_iter()
        .map(|e| e.name.to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"marker.txt".to_string()), "read_dir FOLLOWS the link");

    // And the refusal that makes it safe: the link reports as a Symlink, not a Dir.
    assert_eq!(fs.metadata(&link).unwrap().file_type, flux_fs::FileType::Symlink);
}

#[cfg(unix)]
#[test]
fn read_dir_reports_non_utf8_names_intact() {
    // A fake that agrees with the implementation about encoding proves nothing
    // about the OS. §241: the on-disk name is bytes, and it must survive read_dir
    // without a lossy conversion.
    //
    // `cfg(unix)` is NOT the same as "permits arbitrary bytes in a name", and this
    // test asserted that it was. POSIX allows any byte but `/` and NUL, and Linux
    // follows it -- but macOS's APFS and HFS+ ENFORCE UTF-8 and refuse this name
    // outright. MEASURED on a macos-latest runner, which is where it was caught:
    // `Os { code: 92, kind: Uncategorized, message: "Illegal byte sequence" }`.
    // There are three naming regimes here, not two: arbitrary bytes on Linux,
    // WTF-16 on Windows, and enforced UTF-8 on macOS.
    //
    // So the CONTRACT is stated as what it actually is -- on a filesystem that
    // permits such a name, `read_dir` must return it intact -- and the test skips,
    // loudly, where the OS will not create one. Gating to `target_os = "linux"`
    // would have worked too and was rejected: it would silently drop the check on
    // every other unix that does allow these names.
    use std::os::unix::ffi::OsStrExt;
    let d = tempfile::tempdir().unwrap();
    let raw = std::ffi::OsStr::from_bytes(&[b'x', 0xFF, b'y']);
    if std::fs::write(d.path().join(raw), b"").is_err() {
        eprintln!("skipped: this filesystem refuses a non-UTF-8 name");
        return;
    }

    let got = StdFileSystem.read_dir(d.path()).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name.as_bytes(), &[b'x', 0xFF, b'y'], "name survived intact");
}

#[test]
fn rename_no_replace_refuses_a_path_with_an_interior_nul() {
    // A REGRESSION pin, not a hypothetical. Going direct to MoveFileExW lost the
    // interior-NUL rejection `std::fs` performs, and MoveFileExW stops reading at the
    // first NUL -- so this published at "to" while the caller asked for "to\0bar", and
    // returned Ok. A method whose whole contract is "publish exactly here or refuse"
    // must never write to a path it was not given.
    //
    // Cross-platform on purpose: rustix rejects the same path with EINVAL, so both arms
    // owe the same answer and this test is what holds them to it.
    let d = TempDir::new().unwrap();
    let from = d.path().join("from");
    let fs = StdFileSystem;
    fs.create_new(&from).unwrap();

    let mut s = d.path().join("to").into_os_string();
    s.push("\u{0}bar");
    let to = std::path::PathBuf::from(s);

    let err = fs.rename_no_replace(&from, &to).expect_err("an interior NUL is not a path");
    assert_eq!(err.source.kind(), std::io::ErrorKind::InvalidInput);
    assert!(from.exists(), "the source must be untouched");
    assert!(!d.path().join("to").exists(), "nothing may appear at the truncated path");
}

#[test]
fn rename_no_replace_reports_a_missing_source_before_an_occupied_target() {
    // THE ONE TEST THAT DISTINGUISHES THIS BODY FROM THE FORBIDDEN ONE, and the reason
    // it is worth its own test rather than folding into the missing-source case above.
    //
    // check-then-act evaluates `symlink_metadata(to)` first, finds the target occupied
    // and answers AlreadyExists -- without ever looking at the source. The atomic
    // primitives hand both paths to the kernel, which resolves the source first and
    // answers NotFound. MEASURED as ENOENT(2) under Linux and ERROR_FILE_NOT_FOUND(2)
    // on Windows.
    //
    // So this asserts an ORDERING that only a real atomic primitive produces, and it is
    // what stops a future commit quietly restoring the symlink_metadata pre-check that
    // FLUX_FULL_UPDATED_SPEC_V16.md:10876 forbids by name. Every other test in this file
    // passes against that body.
    let d = TempDir::new().unwrap();
    let from = d.path().join("absent");
    let to = d.path().join("occupied");
    let fs = StdFileSystem;
    fs.create_new(&to).unwrap();

    let err = fs.rename_no_replace(&from, &to).expect_err("there is nothing to rename");
    assert_eq!(
        err.source.kind(),
        std::io::ErrorKind::NotFound,
        "the kernel resolves the source first; a destination pre-check would say AlreadyExists"
    );
    assert!(to.exists(), "the occupying target must survive");
}

#[test]
fn rename_no_replace_refuses_the_source_itself() {
    // Publishing a name onto itself is a collision, not a no-op: the name is occupied,
    // and by definition the occupant is not being replaced by something new. Linux and
    // macOS answer EEXIST for free. Windows answers Ok, so the Windows arm has to veto
    // it explicitly -- this is the test that holds it to the same contract.
    let d = TempDir::new().unwrap();
    let p = d.path().join("x");
    let fs = StdFileSystem;
    fs.create_new(&p).unwrap();

    let err = fs.rename_no_replace(&p, &p).expect_err("a name cannot be published onto itself");
    assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(p.exists(), "the object must survive");
}

#[test]
fn rename_no_replace_refuses_a_second_link_to_the_source() {
    // The sharper half, and the one that is unambiguously a contract breach rather than
    // a question of taste: `to` is a DISTINCT directory entry that EXISTS. MEASURED
    // before the veto existed -- Windows returned Ok and CONSUMED the source name,
    // leaving one link where there had been two, while Linux refused with EEXIST.
    let d = TempDir::new().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    let fs = StdFileSystem;
    fs.create_new(&a).unwrap();

    // Hard links need filesystem support and, on some Windows configurations, a
    // privilege this process may lack. Skip rather than fail, following the precedent
    // the symlink and non-UTF-8 tests in this file set.
    if std::fs::hard_link(&a, &b).is_err() {
        eprintln!("SKIPPED: this filesystem will not create a hard link");
        return;
    }

    let err = fs.rename_no_replace(&a, &b).expect_err("the target name is occupied");
    assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(a.exists(), "the source link must survive");
    assert!(b.exists(), "the target link must survive");
}

#[cfg(windows)]
#[test]
fn rename_no_replace_refuses_a_case_only_change() {
    // Pinned because it READS like a defect and is not one, so the next person to
    // notice it finds this test instead of "fixing" it. Windows resolves both names to
    // one object, making this a same-object publish, which the identity half of the
    // veto refuses.
    //
    // It is not a regression: the check-then-act body this method replaced refused it
    // too -- MEASURED, `symlink_metadata("FILE.TXT")` succeeds on a case-insensitive
    // volume -- and macOS refuses it as well, since `RENAME_EXCL` on case-insensitive
    // APFS sees an occupied name. Changing a name's case is `rename_replace`'s job;
    // this method publishes a NEW name without replacing, and the name is not new.
    let d = TempDir::new().unwrap();
    let lower = d.path().join("file.txt");
    let upper = d.path().join("FILE.TXT");
    let fs = StdFileSystem;
    fs.create_new(&lower).unwrap();

    let err = fs.rename_no_replace(&lower, &upper).expect_err("same object, occupied name");
    assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(lower.exists(), "the object must survive");
}
