#[cfg(unix)]
mod posix {
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

    #[test]
    fn an_occupied_name_reports_already_exists_through_the_handle() {
        // The engine branches on AlreadyExists to tell an occupied destination from
        // a broken one, so the kind has to survive the handle arm. MEASURED on the
        // Windows arm before this test existed: `create_new` on an occupied name came
        // back ErrorKind::Other there while the PATH-based arm reported AlreadyExists
        // for the identical event, because the NTSTATUS was wrapped with
        // `io::Error::other`, which hardcodes the kind. POSIX gets this for free from
        // errno; the test is here so both arms are pinned to the same answer.
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        drop(root.create_new(OsStr::new("dup")).unwrap());
        let err = match root.create_new(OsStr::new("dup")) {
            Err(e) => e,
            Ok(_) => panic!("create_new must refuse an occupied name"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_symlink_to_a_file_is_also_a_safety_rejection() {
        // The sibling of the /etc test above, which links to a DIRECTORY. O_NOFOLLOW
        // refuses the link itself whatever it points at, so POSIX answers the same
        // either way -- but the Windows arm reaches this case down a DIFFERENT code
        // path (STATUS_NOT_A_DIRECTORY, not the reparse-tag check) and had to be
        // fixed, so both arms carry the test.
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("victim"), b"x").unwrap();
        std::os::unix::fs::symlink(d.path().join("victim"), d.path().join("flink")).unwrap();

        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = root.open_dir(OsStr::new("flink")).unwrap_err();
        assert_eq!(err.code, flux_fs::Code::SafetyRejected);
    }

    #[test]
    fn remove_file_refuses_an_ordinary_directory() {
        // MEASURED before the Windows guard existed: POSIX refused with
        // IsADirectory while the Windows arm OPENED the directory and deleted it,
        // returning Ok. remove_file removing a directory is silent structural damage,
        // so both arms are pinned here.
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("adir")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        let err = root.remove_file(OsStr::new("adir")).unwrap_err();
        assert_eq!(err.source.kind(), std::io::ErrorKind::IsADirectory);
        assert!(d.path().join("adir").is_dir(), "the directory must survive the refusal");
    }

    #[test]
    fn a_missing_target_is_not_a_destination_error() {
        // The distinction open_dir does NOT make, deliberately. A missing COMPONENT
        // of the destination path is a destination problem and open_dir still says
        // DestinationError. A missing file to remove, stat or rename is an ordinary
        // not-found. MEASURED: this arm said DestinationError for all of them because
        // open_dir's mapping had been copied into each helper.
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        for err in [
            root.remove_file(OsStr::new("nosuch")).unwrap_err(),
            root.metadata(OsStr::new("nosuch")).unwrap_err(),
            root.rename_no_replace(OsStr::new("nosuch"), &root, OsStr::new("dest")).unwrap_err(),
        ] {
            assert_eq!(err.code, flux_fs::Code::IoError);
            assert_eq!(err.source.kind(), std::io::ErrorKind::NotFound);
        }

        // ... while the destination component itself still reports DestinationError.
        assert_eq!(
            root.open_dir(OsStr::new("nosuch")).unwrap_err().code,
            flux_fs::Code::DestinationError
        );
    }

    #[test]
    fn a_destination_root_must_be_a_directory() {
        // MEASURED before the Windows check existed: POSIX refused with
        // NotADirectory (OFlags::DIRECTORY states the requirement to the kernel)
        // while Windows returned Ok and handed back a DirHandle wrapping a FILE,
        // failing only later as a masked error on the first child operation.
        let d = TempDir::new().unwrap();
        let f = d.path().join("iamafile");
        std::fs::write(&f, b"x").unwrap();

        let err = match StdFileSystem.destination_root(&f) {
            Err(e) => e,
            Ok(_) => panic!("a file must not be accepted as a destination root"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::NotADirectory);
    }

    #[test]
    fn a_created_directory_is_usable_without_reopening_its_name() {
        // create_dir returns a handle to the directory it just made. On Windows that
        // handle is the one NtCreateFile returned, rather than the result of
        // re-opening the NAME -- re-opening was check-then-act on a name, the pattern
        // this cut removes, and it let another process swap a different plain
        // directory into the window. The handle must be as usable as open_dir's, so
        // this writes through it and reads the child back.
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        let made = root.create_dir(OsStr::new("fresh")).unwrap();
        drop(made.create_new(OsStr::new("inside")).unwrap());
        assert_eq!(made.metadata(OsStr::new("inside")).unwrap().len, 0);
        assert!(d.path().join("fresh/inside").is_file());

        // And it still refuses to create the same name twice.
        assert!(root.create_dir(OsStr::new("fresh")).is_err());
    }
}

#[cfg(windows)]
mod windows_arm {
    use flux_fs::{DestinationRoot, DirHandle};
    use flux_platform::StdFileSystem;
    use std::ffi::OsStr;
    use tempfile::TempDir;

    fn junction(target: &std::path::Path, link: &std::path::Path) -> bool {
        std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &link.display().to_string(),
                &target.display().to_string(),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn a_child_directory_opens() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("child")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        assert!(root.open_dir(OsStr::new("child")).is_ok());
    }

    #[test]
    fn a_junction_is_refused_as_a_safety_rejection() {
        // The Windows half of the cut's reason to exist. NOTE what makes this hard:
        // MEASURED, NtCreateFile with FILE_OPEN_REPARSE_POINT SUCCEEDS on a junction
        // and hands back a handle to it. The refusal therefore comes from the TAG
        // check after the open, not from the open failing as it does on POSIX.
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        if !junction(&d.path().join("real"), &d.path().join("junc")) {
            eprintln!("SKIPPED: could not create a junction");
            return;
        }
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = root.open_dir(OsStr::new("junc")).unwrap_err();
        assert_eq!(err.code, flux_fs::Code::SafetyRejected);
    }

    #[test]
    fn a_plain_file_component_is_a_destination_error() {
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
    fn a_directory_is_created_and_reopened_through_the_handle() {
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let child = root.create_dir(OsStr::new("made")).unwrap();
        assert!(d.path().join("made").is_dir());
        let mut w = child.create_new(OsStr::new("inside")).unwrap();
        w.write_all(b"x").unwrap();
        drop(w);
        assert!(
            d.path().join("made/inside").exists(),
            "the returned handle must address the new directory"
        );
    }

    #[test]
    fn a_file_is_removed_through_the_handle() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("doomed"), b"x").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        root.remove_file(OsStr::new("doomed")).unwrap();
        assert!(!d.path().join("doomed").exists());
    }

    #[test]
    fn a_publish_across_two_handles_refuses_an_occupied_name() {
        // The two-handle rename is what staging-then-publishing needs, and the
        // no-replace form must still refuse an occupied target.
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("stage")).unwrap();
        std::fs::write(d.path().join("stage/tmp"), b"payload").unwrap();
        std::fs::write(d.path().join("taken"), b"old").unwrap();

        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let stage = root.open_dir(OsStr::new("stage")).unwrap();

        stage.rename_no_replace(OsStr::new("tmp"), &root, OsStr::new("taken")).unwrap_err();
        assert_eq!(
            std::fs::read(d.path().join("taken")).unwrap(),
            b"old",
            "the occupied name must survive"
        );

        stage.rename_no_replace(OsStr::new("tmp"), &root, OsStr::new("fresh")).unwrap();
        assert_eq!(std::fs::read(d.path().join("fresh")).unwrap(), b"payload");
    }

    #[test]
    fn metadata_through_the_handle_reads_the_child() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("f"), b"1234").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        assert_eq!(root.metadata(OsStr::new("f")).unwrap().len, 4);
    }

    #[test]
    fn an_occupied_name_reports_already_exists_through_the_handle() {
        // MEASURED before the fix: this arm answered ErrorKind::Other where the
        // path-based arm answered AlreadyExists for the same collision, because every
        // NTSTATUS went through `io::Error::other`, which hardcodes the kind. The
        // engine branches on AlreadyExists to tell an occupied destination from a
        // broken one, so the two arms must agree.
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        drop(root.create_new(OsStr::new("dup")).unwrap());
        let err = match root.create_new(OsStr::new("dup")) {
            Err(e) => e,
            Ok(_) => panic!("create_new must refuse an occupied name"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_publish_onto_an_occupied_name_reports_already_exists() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("src"), b"x").unwrap();
        std::fs::write(d.path().join("taken"), b"old").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        let err =
            root.rename_no_replace(OsStr::new("src"), &root, OsStr::new("taken")).unwrap_err();
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_symlink_to_a_file_is_refused_as_a_safety_rejection() {
        // The case FILE_DIRECTORY_FILE hides. A junction OPENS and is caught by the
        // reparse-tag check, but a symlink whose target is a FILE fails the open
        // outright with STATUS_NOT_A_DIRECTORY -- the same status a plain file gives
        // -- so without a second look this arm called it DESTINATION_ERROR while
        // POSIX called it SAFETY_REJECTED for the identical object.
        //
        // Creating a symlink needs SeCreateSymbolicLinkPrivilege, which an ordinary
        // account does not hold, so this SKIPS rather than fails when it is absent.
        // `a_plain_file_component_is_a_destination_error` is the other half and runs
        // everywhere: if the second look ever over-reaches, THAT one goes red.
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("victim"), b"x").unwrap();
        if std::os::windows::fs::symlink_file(d.path().join("victim"), d.path().join("flink"))
            .is_err()
        {
            eprintln!("SKIPPED: no SeCreateSymbolicLinkPrivilege; cannot create a file symlink");
            return;
        }

        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = root.open_dir(OsStr::new("flink")).unwrap_err();
        assert_eq!(err.code, flux_fs::Code::SafetyRejected);
    }

    #[test]
    fn an_over_long_name_creates_nothing_at_all() {
        // The check_component bound, proven at the filesystem rather than in a unit
        // test. MEASURED before it existed: this created a 100-character file and
        // returned Ok, because UNICODE_STRING.Length is a u16 of BYTES and
        // 32868 * 2 wraps to 200. The assertion that matters is the empty directory:
        // a refusal that still wrote something would be no fix at all.
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let long: std::ffi::OsString = std::ffi::OsString::from("b".repeat(32868));

        let err = match root.create_new(&long) {
            Err(e) => e,
            Ok(_) => panic!("an over-long name must not be created"),
        };
        assert_eq!(err.code, flux_fs::Code::SafetyRejected);
        assert_eq!(
            std::fs::read_dir(d.path()).unwrap().count(),
            0,
            "the refusal must leave the directory untouched"
        );
    }

    #[test]
    fn remove_file_refuses_an_ordinary_directory() {
        // MEASURED before the Windows guard existed: POSIX refused with
        // IsADirectory while the Windows arm OPENED the directory and deleted it,
        // returning Ok. remove_file removing a directory is silent structural damage,
        // so both arms are pinned here.
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("adir")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        let err = root.remove_file(OsStr::new("adir")).unwrap_err();
        assert_eq!(err.source.kind(), std::io::ErrorKind::IsADirectory);
        assert!(d.path().join("adir").is_dir(), "the directory must survive the refusal");
    }

    #[test]
    fn a_missing_target_is_not_a_destination_error() {
        // The distinction open_dir does NOT make, deliberately. A missing COMPONENT
        // of the destination path is a destination problem and open_dir still says
        // DestinationError. A missing file to remove, stat or rename is an ordinary
        // not-found. MEASURED: this arm said DestinationError for all of them because
        // open_dir's mapping had been copied into each helper.
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        for err in [
            root.remove_file(OsStr::new("nosuch")).unwrap_err(),
            root.metadata(OsStr::new("nosuch")).unwrap_err(),
            root.rename_no_replace(OsStr::new("nosuch"), &root, OsStr::new("dest")).unwrap_err(),
        ] {
            assert_eq!(err.code, flux_fs::Code::IoError);
            assert_eq!(err.source.kind(), std::io::ErrorKind::NotFound);
        }

        // ... while the destination component itself still reports DestinationError.
        assert_eq!(
            root.open_dir(OsStr::new("nosuch")).unwrap_err().code,
            flux_fs::Code::DestinationError
        );
    }

    #[test]
    fn a_destination_root_must_be_a_directory() {
        // MEASURED before the Windows check existed: POSIX refused with
        // NotADirectory (OFlags::DIRECTORY states the requirement to the kernel)
        // while Windows returned Ok and handed back a DirHandle wrapping a FILE,
        // failing only later as a masked error on the first child operation.
        let d = TempDir::new().unwrap();
        let f = d.path().join("iamafile");
        std::fs::write(&f, b"x").unwrap();

        let err = match StdFileSystem.destination_root(&f) {
            Err(e) => e,
            Ok(_) => panic!("a file must not be accepted as a destination root"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::NotADirectory);
    }

    #[test]
    fn a_created_directory_is_usable_without_reopening_its_name() {
        // create_dir returns a handle to the directory it just made. On Windows that
        // handle is the one NtCreateFile returned, rather than the result of
        // re-opening the NAME -- re-opening was check-then-act on a name, the pattern
        // this cut removes, and it let another process swap a different plain
        // directory into the window. The handle must be as usable as open_dir's, so
        // this writes through it and reads the child back.
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        let made = root.create_dir(OsStr::new("fresh")).unwrap();
        drop(made.create_new(OsStr::new("inside")).unwrap());
        assert_eq!(made.metadata(OsStr::new("inside")).unwrap().len, 0);
        assert!(d.path().join("fresh/inside").is_file());

        // And it still refuses to create the same name twice.
        assert!(root.create_dir(OsStr::new("fresh")).is_err());
    }

    #[test]
    fn a_junction_is_accepted_as_the_destination_ROOT() {
        // The DEST EXEMPTION, which nothing else pins. Every component BELOW the root
        // is refused if it is a name surrogate -- a_junction_is_refused_as_a_safety_
        // _rejection covers that -- but §149.7 exempts DEST ITSELF, which is resolved
        // by path and DOES follow links. So a junction handed in as the root must be
        // followed, and writes must land in its target.
        //
        // Without this test, "reject junctions" looks like a strictly safer change to
        // make to destination_root, and it would silently break copying into a
        // junctioned destination.
        let d = TempDir::new().unwrap();
        let real = d.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = d.path().join("rootlink");
        if !junction(&real, &link) {
            eprintln!("SKIPPED: could not create a junction");
            return;
        }

        let root = StdFileSystem.destination_root(&link).unwrap();
        drop(root.create_new(OsStr::new("through")).unwrap());
        assert!(real.join("through").is_file(), "the write must land in the junction's TARGET");
    }

    #[test]
    fn the_handle_create_dir_returns_serves_every_child_operation() {
        // create_dir now returns the handle NtCreateFile gave it rather than
        // re-opening the name, so its access mask is no longer open_dir's by
        // construction -- it is open_dir's because it was widened to match. This
        // exercises the whole DirHandle surface through that handle, including a
        // rename ACROSS two handles, which is the one that needs DELETE on the source
        // and a second directory handle as the target.
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let made = root.create_dir(OsStr::new("fresh")).unwrap();

        drop(made.create_new(OsStr::new("a")).unwrap());
        assert_eq!(made.metadata(OsStr::new("a")).unwrap().len, 0);
        made.create_dir(OsStr::new("sub")).unwrap();
        made.open_dir(OsStr::new("sub")).unwrap();
        made.rename_no_replace(OsStr::new("a"), &made, OsStr::new("b")).unwrap();
        made.remove_file(OsStr::new("b")).unwrap();
        made.rename_no_replace(OsStr::new("sub"), &root, OsStr::new("moved")).unwrap();
        assert!(d.path().join("moved").is_dir());
    }
}
