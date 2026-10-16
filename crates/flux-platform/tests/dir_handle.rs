#[cfg(unix)]
mod posix {
    use flux_fs::{DestinationRoot, DirHandle};
    use flux_platform::StdFileSystem;
    use std::ffi::OsStr;
    use tempfile::TempDir;

    #[test]
    fn a_subdirectory_is_not_a_mount_root_and_the_dev_mount_is() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let sub = root.open_dir(OsStr::new("sub")).unwrap();
        assert_eq!(sub.mount_root(&root).unwrap(), flux_fs::MountRoot::No);
        // `/dev` is its own mount on Linux (devtmpfs) and on macOS (devfs): a different device from `/`.
        let slash = StdFileSystem.destination_root(std::path::Path::new("/")).unwrap();
        let dev = slash.open_dir(OsStr::new("dev")).unwrap();
        assert_eq!(dev.mount_root(&slash).unwrap(), flux_fs::MountRoot::Yes);
    }

    #[test]
    fn canonical_path_names_the_directory_the_handle_holds_even_through_a_link() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        std::os::unix::fs::symlink(d.path().join("real"), d.path().join("alias")).unwrap();
        // `destination_root` follows a link AT the root (section 149.7 exempts it); the handle holds `real`.
        let through = StdFileSystem.destination_root(&d.path().join("alias")).unwrap();
        let direct = StdFileSystem.destination_root(&d.path().join("real")).unwrap();
        let expected = std::fs::canonicalize(d.path().join("real")).unwrap();
        assert_eq!(through.canonical_path().unwrap(), expected);
        assert_eq!(direct.canonical_path().unwrap(), expected);
    }

    #[test]
    fn a_child_directory_opens() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("child")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        assert!(root.open_dir(OsStr::new("child")).is_ok());
    }

    #[test]
    fn the_handle_and_the_path_describe_one_object_identically() {
        // The engine will compare identities from both sides: one taken through a
        // directory handle (`metadata_from_stat`), one by path (`identity_of`, via
        // std). They convert the raw `stat` fields separately, and the widths differ
        // by platform (macOS: `st_mode: u16`, `st_dev: i32`), so a conversion that
        // drifts from std's would make one object look like two. MEASURED before this
        // test: forcing the handle side's `dev` to 0 left the whole suite green.
        use flux_fs::FileSystem;
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("f"), b"1234").unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        for name in ["f", "sub"] {
            let by_handle = root.metadata(OsStr::new(name)).unwrap();
            let by_path = StdFileSystem.metadata(&d.path().join(name)).unwrap();
            assert!(matches!(by_path.identity, flux_fs::FileIdentity::Strong(_)));
            assert_eq!(by_handle, by_path, "{name}: handle and path disagree");
        }
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

    #[cfg(target_os = "macos")]
    #[test]
    fn a_genuine_eperm_on_a_file_stays_permission_denied() {
        // macOS answers EPERM both for a directory and for a forbidden unlink, and
        // remove_file stats to tell them apart. This pins the other half: a FILE the
        // owner has made immutable (`chflags uchg`, no root needed) must still say
        // PERMISSION_DENIED, not be reclassified as a directory.
        let d = TempDir::new().unwrap();
        let f = d.path().join("locked");
        std::fs::write(&f, b"x").unwrap();
        let chflags = |flag: &str| {
            let ok = std::process::Command::new("chflags").arg(flag).arg(&f).status().unwrap();
            assert!(ok.success(), "chflags {flag} failed");
        };
        chflags("uchg");
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = root.remove_file(OsStr::new("locked")).unwrap_err();
        chflags("nouchg"); // before any assert, or TempDir cannot clean up
        // EPERM (1), not EACCES: otherwise this never reaches the branch it guards.
        assert_eq!(err.source.raw_os_error(), Some(1), "expected EPERM, got {:?}", err.source);
        assert_eq!(err.code, flux_fs::Code::PermissionDenied);
        assert!(f.is_file(), "the file must survive the refusal");
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

        // And it still refuses to create the same name twice -- checking WHICH
        // error, because `is_err()` alone would also pass if the handle had stopped
        // working entirely, which is the failure this test is meant to notice.
        let err = match root.create_dir(OsStr::new("fresh")) {
            Err(e) => e,
            Ok(_) => panic!("create_dir must refuse an occupied name"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn create_new_reports_already_exists_when_a_directory_holds_the_name() {
        // The twin of an_occupied_name_reports_already_exists_through_the_handle, and
        // the case that round's fix missed. The kernel distinguishes a name held by a
        // file from a name held by a directory -- STATUS_OBJECT_NAME_COLLISION versus
        // STATUS_FILE_IS_A_DIRECTORY -- and to create_new they are one situation: the
        // name is taken. MEASURED before the fix: POSIX answered AlreadyExists for
        // both while Windows answered ErrorKind::Other for the directory.
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("taken")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        let err = match root.create_new(OsStr::new("taken")) {
            Err(e) => e,
            Ok(_) => panic!("create_new must refuse a name a directory holds"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn create_new_refuses_a_name_a_symlink_already_holds() {
        // The POSIX twin of the Windows FILE_OPEN_REPARSE_POINT case. This arm passes
        // O_CREAT | O_EXCL and NOT O_NOFOLLOW, relying on POSIX's rule that O_EXCL
        // makes open() fail with EEXIST when the final component is a symlink,
        // whatever it points at. That is a spec guarantee this cut leans on, so it is
        // measured rather than assumed -- with a DANGLING link, which is the case
        // where following would actually create something outside the tree.
        let d = TempDir::new().unwrap();
        let outside = d.path().join("outside");
        std::os::unix::fs::symlink(&outside, d.path().join("bait")).unwrap();
        assert!(!outside.exists(), "the link must dangle for this to mean anything");

        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = match root.create_new(OsStr::new("bait")) {
            Err(e) => e,
            Ok(_) => panic!("a name a symlink holds is taken"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(!outside.exists(), "nothing may have been created at the link's TARGET");
    }

    #[test]
    fn create_claim_store_makes_a_store_through_the_handle() {
        use flux_fs::{ClaimKey, ClaimRecord, ClaimStatus, ClaimStore, Durability, FluxPathKey};
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut store =
            root.create_claim_store(OsStr::new("state.db"), Durability::Normal).unwrap();
        let key = ClaimKey::new(flux_fs::ObjectId { volume: 1, index: 1 }, OsStr::new("a"));
        let rec = ClaimRecord { target: FluxPathKey(b"a".to_vec()), status: ClaimStatus::Existing };
        store.insert_if_absent(&key, &rec).unwrap();
        store.flush().unwrap();
        assert!(
            d.path().join("state.db").is_file(),
            "the store is a file in the handle's directory"
        );
        let err = match root.create_claim_store(OsStr::new("state.db"), Durability::Normal) {
            Err(e) => e,
            Ok(_) => panic!("a taken name must be refused"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn the_real_store_passes_the_conformance_suite_through_the_handle() {
        use flux_fs::Durability;
        let d = TempDir::new().unwrap();
        let n = std::cell::Cell::new(0u32);
        flux_fs::claims::conformance::run_all(|| {
            n.set(n.get() + 1);
            let sub = format!("c{}", n.get());
            std::fs::create_dir(d.path().join(&sub)).unwrap();
            let root = StdFileSystem.destination_root(&d.path().join(&sub)).unwrap();
            root.create_claim_store(OsStr::new("state.db"), Durability::Normal).unwrap()
        });
    }

    #[test]
    fn open_claim_store_refuses_a_link_a_directory_and_a_missing_name() {
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let open = |name: &str| match root
            .open_claim_store(OsStr::new(name), flux_fs::Durability::Normal)
        {
            Err(e) => e,
            Ok(_) => panic!("{name} must be refused"),
        };

        std::os::unix::fs::symlink(d.path().join("elsewhere"), d.path().join("lnk")).unwrap();
        assert_eq!(open("lnk").code, flux_fs::Code::SafetyRejected);

        std::fs::create_dir(d.path().join("adir")).unwrap();
        let e = open("adir");
        assert_eq!(e.code, flux_fs::Code::DestinationError);
        assert_eq!(e.source.kind(), std::io::ErrorKind::IsADirectory);

        let e = open("missing");
        assert_eq!(e.source.kind(), std::io::ErrorKind::NotFound);

        let store =
            root.create_claim_store(OsStr::new("state.db"), flux_fs::Durability::Normal).unwrap();
        drop(store);
        let store =
            root.open_claim_store(OsStr::new("state.db"), flux_fs::Durability::Normal).unwrap();
        assert_eq!(flux_fs::ClaimStore::count(&store).unwrap(), 0);
    }

    #[test]
    fn create_claim_store_never_follows_a_link() {
        let d = TempDir::new().unwrap();
        let outside = d.path().join("outside");
        std::os::unix::fs::symlink(&outside, d.path().join("state.db")).unwrap();
        assert!(!outside.exists(), "the link must dangle for this to mean anything");
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = match root.create_claim_store(OsStr::new("state.db"), flux_fs::Durability::Normal)
        {
            Err(e) => e,
            Ok(_) => panic!("a name a symlink holds is taken"),
        };
        assert!(
            err.source.kind() == std::io::ErrorKind::AlreadyExists
                || err.code == flux_fs::Code::SafetyRejected,
            "got {err:?}"
        );
        assert!(!outside.exists(), "nothing may have been created at the link's TARGET");
    }

    #[test]
    fn a_symlink_is_accepted_as_the_destination_root() {
        // The POSIX twin of a_junction_is_accepted_as_the_destination_root. Every
        // component BELOW the root is refused if it is a link -- that is what
        // a_symlinked_component_is_refused_as_a_safety_rejection pins -- but 149.7
        // exempts DEST ITSELF, which is resolved by path and DOES follow links. This
        // arm expresses that by NOT passing O_NOFOLLOW on the root open, which is a
        // deliberate omission and therefore worth a test: adding NOFOLLOW there would
        // look like tightening a safety flag while silently breaking every copy into
        // a symlinked destination.
        let d = TempDir::new().unwrap();
        let real = d.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, d.path().join("rootlink")).unwrap();

        let root = StdFileSystem.destination_root(&d.path().join("rootlink")).unwrap();
        drop(root.create_new(OsStr::new("through")).unwrap());
        assert!(real.join("through").is_file(), "the write must land in the link's TARGET");
    }

    #[test]
    fn rename_replace_refuses_a_read_only_target() {
        // The path version's rule, now for the handle (std_fs.rs,
        // rename_replace_refuses_a_read_only_target). `renameat` checks the
        // DIRECTORY's permission, not the file's, so without the guard a read-only
        // destination is silently replaced.
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("from")).unwrap());
        let to = d.path().join("to");
        std::fs::write(&to, b"protected").unwrap();
        let mut perms = std::fs::metadata(&to).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&to, perms).unwrap();

        let err = root.rename_replace(OsStr::new("from"), &root, OsStr::new("to")).unwrap_err();

        assert_eq!(err.code, flux_fs::Code::PermissionDenied);
        assert!(err.source.raw_os_error().is_none(), "the guard must refuse, not the OS");
        assert_eq!(std::fs::read(&to).unwrap(), b"protected", "the protected file must survive");
        std::fs::remove_file(&to).expect("a read-only file must still be removable");
    }

    #[test]
    fn rename_replace_allows_a_symlink_whose_target_is_read_only() {
        // The guard judges the NAME being replaced; renameat replaces the link and
        // never touches its target (std_fs.rs, the same-named test).
        let d = TempDir::new().unwrap();
        let target = d.path().join("target");
        std::fs::write(&target, b"protected").unwrap();
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&target, perms).unwrap();
        std::os::unix::fs::symlink(&target, d.path().join("link")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut w = root.create_new(OsStr::new("from")).unwrap();
        std::io::Write::write_all(&mut w, b"new").unwrap();
        drop(w);

        root.rename_replace(OsStr::new("from"), &root, OsStr::new("link"))
            .expect("replacing a symlink must not consult its target");

        assert_eq!(std::fs::read(&target).unwrap(), b"protected", "the target must be untouched");
        assert_eq!(std::fs::read(d.path().join("link")).unwrap(), b"new");
        std::fs::remove_file(&target).expect("a read-only file must still be removable");
    }

    #[test]
    fn rename_replace_refuses_a_target_this_user_cannot_write() {
        // A root-owned 0644 file: the owner write bit is set, so a mode-bit check
        // passes, yet this user cannot write it (std_fs.rs, the same-named test).
        // Skips without passwordless sudo, exactly as that test does.
        if !std::process::Command::new("sudo")
            .args(["-n", "true"])
            .status()
            .is_ok_and(|s| s.success())
        {
            eprintln!("skipped: no passwordless sudo, cannot build a root-owned fixture");
            return;
        }
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("from")).unwrap());
        let to = d.path().join("to");
        let sh =
            format!("echo protected > {t} && chmod 0644 {t} && chown 0:0 {t}", t = to.display());
        assert!(
            std::process::Command::new("sudo")
                .args(["-n", "sh", "-c", &sh])
                .status()
                .unwrap()
                .success(),
            "fixture"
        );

        assert!(
            root.rename_replace(OsStr::new("from"), &root, OsStr::new("to")).is_err(),
            "a file we cannot write must be refused"
        );
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
    fn a_handle_reports_the_identity_of_the_directory_it_holds() {
        use flux_fs::FileSystem;
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let sub = root.open_dir(OsStr::new("sub")).unwrap();
        let made = root.create_dir(OsStr::new("made")).unwrap();

        for (handle, path) in [
            (&root, d.path().to_path_buf()),
            (&sub, d.path().join("sub")),
            (&made, d.path().join("made")),
        ] {
            let by_handle = handle.identity().unwrap();
            assert!(
                matches!(by_handle, flux_fs::FileIdentity::Strong(_)),
                "{path:?}: {by_handle:?}"
            );
            assert_eq!(by_handle, StdFileSystem.metadata(&path).unwrap().identity, "{path:?}");
        }
        assert_ne!(root.identity().unwrap(), sub.identity().unwrap());
    }

    #[test]
    fn open_read_streams_a_child_file() {
        use std::io::{Read, Write};
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut w = root.create_new(OsStr::new("f")).unwrap();
        w.write_all(&bytes).unwrap();
        drop(w);
        let mut r = root.open_read(OsStr::new("f")).unwrap();
        let mut got = Vec::new();
        r.read_to_end(&mut got).unwrap();
        assert_eq!(got, bytes);
    }

    #[test]
    fn open_read_refuses_a_symlink_as_safety_rejected() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("f"), b"target").unwrap();
        std::os::unix::fs::symlink("f", d.path().join("l")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let Err(e) = root.open_read(OsStr::new("l")) else { panic!("a link must not be followed") };
        assert_eq!(e.code, flux_fs::Code::SafetyRejected, "{e:?}");
    }

    #[test]
    fn open_read_refuses_a_directory() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("d")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let Err(e) = root.open_read(OsStr::new("d")) else { panic!("a directory is not readable") };
        assert_eq!(e.code, flux_fs::Code::DestinationError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::IsADirectory, "{e:?}");
    }

    #[test]
    fn open_read_of_a_missing_name_is_not_found() {
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let Err(e) = root.open_read(OsStr::new("nope")) else { panic!("a missing name") };
        assert_eq!(e.code, flux_fs::Code::IoError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::NotFound, "{e:?}");
    }

    /// The Task 7 production path: the WAL thread syncs a duplicate of a writer made through a `DirHandle`,
    /// while the copy loop keeps writing through the original.
    #[test]
    fn a_duplicate_of_a_created_child_syncs_on_another_thread_while_the_original_writes() {
        use flux_fs::FileHandle;
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut w = root.create_new(OsStr::new("f")).unwrap();
        w.write_all(b"abc").unwrap();
        let dup = w.duplicate().unwrap();
        std::thread::scope(|s| {
            s.spawn(|| dup.sync_all().unwrap()).join().unwrap();
        });
        w.write_all(b"def").unwrap();
        // The duplicate still works after the original wrote.
        std::thread::scope(|s| {
            s.spawn(|| dup.sync_all().unwrap()).join().unwrap();
        });
        drop(dup);
        drop(w);
        assert_eq!(std::fs::read(d.path().join("f")).unwrap(), b"abcdef");
    }

    #[test]
    fn a_duplicate_of_a_reopened_child_syncs_on_another_thread() {
        use flux_fs::FileHandle;
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut w = root.create_new(OsStr::new("f")).unwrap();
        w.write_all(b"abcde").unwrap();
        let id = w.identity().unwrap();
        drop(w);
        let mut w2 = root.open_existing(OsStr::new("f"), &id).unwrap();
        w2.truncate_to(3).unwrap();
        let dup = w2.duplicate().unwrap();
        std::thread::scope(|s| {
            s.spawn(|| dup.sync_all().unwrap()).join().unwrap();
        });
        w2.write_all(b"XY").unwrap();
        std::thread::scope(|s| {
            s.spawn(|| dup.sync_all().unwrap()).join().unwrap();
        });
        drop(dup);
        drop(w2);
        assert_eq!(std::fs::read(d.path().join("f")).unwrap(), b"abcXY");
    }

    #[test]
    fn open_existing_appends_after_the_prefix() {
        use flux_fs::FileHandle;
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut w = root.create_new(OsStr::new("f")).unwrap();
        w.write_all(b"abc").unwrap();
        let id = w.identity().unwrap();
        drop(w);
        let mut w2 = root.open_existing(OsStr::new("f"), &id).unwrap();
        assert_eq!(w2.identity().unwrap(), id);
        w2.truncate_to(2).unwrap();
        w2.write_all(b"Z").unwrap();
        drop(w2);
        assert_eq!(root.read_file(OsStr::new("f"), 100).unwrap(), b"abZ");
    }

    #[test]
    fn open_existing_refuses_a_planted_symlink() {
        use flux_fs::FileHandle;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let w = root.create_new(OsStr::new("f")).unwrap();
        let id = w.identity().unwrap();
        drop(w);
        std::fs::remove_file(d.path().join("f")).unwrap();
        // A dangling link: following it would CREATE nothing with this flag set, but a follow of a link to an existing
        // file would open it. Plant the dangling one and assert its target is never created.
        std::os::unix::fs::symlink("elsewhere", d.path().join("f")).unwrap();
        let Err(e) = root.open_existing(OsStr::new("f"), &id) else {
            panic!("a link must not be followed")
        };
        assert_eq!(e.code, flux_fs::Code::SafetyRejected, "{e:?}");
        assert!(
            !d.path().join("elsewhere").exists(),
            "the link's target must not have been created"
        );
    }

    #[test]
    fn open_existing_refuses_a_symlink_to_an_existing_file() {
        use flux_fs::FileHandle;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let w = root.create_new(OsStr::new("real")).unwrap();
        let id = w.identity().unwrap();
        drop(w);
        std::os::unix::fs::symlink("real", d.path().join("l")).unwrap();
        // The link resolves to a file whose identity IS `id`: only NOFOLLOW stops this open.
        let Err(e) = root.open_existing(OsStr::new("l"), &id) else {
            panic!("a link must not be followed")
        };
        assert_eq!(e.code, flux_fs::Code::SafetyRejected, "{e:?}");
    }

    #[test]
    fn open_existing_refuses_an_identity_mismatch() {
        use flux_fs::FileHandle;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let w = root.create_new(OsStr::new("f")).unwrap();
        let id = w.identity().unwrap();
        drop(w);
        // Keep the first inode alive so the filesystem cannot hand its number to the second file.
        std::fs::hard_link(d.path().join("f"), d.path().join("keep")).unwrap();
        std::fs::remove_file(d.path().join("f")).unwrap();
        drop(root.create_new(OsStr::new("f")).unwrap());
        let Err(e) = root.open_existing(OsStr::new("f"), &id) else {
            panic!("a different object at the name must be refused")
        };
        assert_eq!(e.code, flux_fs::Code::DestinationError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::InvalidData, "{e:?}");
    }

    // A name that is neither 8.3-compatible nor short, so an auto-generated short name would exist on a volume that
    // makes them: the link count must still be 1 (a count of 2 would make resume refuse every partial).
    #[test]
    fn a_long_named_file_reports_one_link() {
        use flux_fs::{DirHandle, FileHandle};
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let name = OsStr::new("a.long-name.flux-partial.0123456789abcdef0123456789abcdef");
        let w = root.create_new(name).unwrap();
        let id = w.identity().unwrap();
        drop(w);
        assert_eq!(root.metadata(name).unwrap().nlink, Some(1));
        // The opened-handle path: `open_existing` refuses a count above 1, so it opening is the handle's own report.
        drop(root.open_existing(name, &id).unwrap());
    }

    #[test]
    fn open_existing_refuses_a_hard_linked_file_and_leaves_the_other_name_alone() {
        use flux_fs::{DirHandle, FileHandle};
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut w = root.create_new(OsStr::new("victim")).unwrap();
        w.write_all(b"precious").unwrap();
        let id = w.identity().unwrap();
        drop(w);
        // The planted name: the same object under a second name, so its identity IS `id` and only the link count
        // stops this open from writing through it.
        std::fs::hard_link(d.path().join("victim"), d.path().join("partial")).unwrap();
        assert_eq!(root.metadata(OsStr::new("partial")).unwrap().nlink, Some(2));
        let Err(e) = root.open_existing(OsStr::new("partial"), &id) else {
            panic!("a hard-linked file must not be opened for writing")
        };
        assert_eq!(e.code, flux_fs::Code::SafetyRejected, "{e:?}");
        assert!(e.source.to_string().contains("2 hard links"), "{e:?}");
        assert_eq!(std::fs::read(d.path().join("victim")).unwrap(), b"precious");
        // Unlinking the planted name leaves the object with one link, and it opens again.
        std::fs::remove_file(d.path().join("partial")).unwrap();
        assert_eq!(root.metadata(OsStr::new("victim")).unwrap().nlink, Some(1));
        drop(root.open_existing(OsStr::new("victim"), &id).unwrap());
    }

    #[test]
    fn open_existing_refuses_an_unavailable_expectation() {
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("f")).unwrap());
        let Err(e) = root.open_existing(OsStr::new("f"), &flux_fs::FileIdentity::Unavailable)
        else {
            panic!("an unavailable expectation proves nothing")
        };
        assert_eq!(e.code, flux_fs::Code::DestinationError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::InvalidData, "{e:?}");
    }

    #[test]
    fn open_existing_of_a_missing_name_is_not_found() {
        use flux_fs::FileHandle;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let id = root.create_new(OsStr::new("g")).unwrap().identity().unwrap();
        let Err(e) = root.open_existing(OsStr::new("nope"), &id) else { panic!("a missing name") };
        assert_eq!(e.code, flux_fs::Code::IoError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::NotFound, "{e:?}");
    }

    #[test]
    fn open_existing_refuses_a_directory() {
        use flux_fs::FileHandle;
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("d")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let id = root.create_new(OsStr::new("g")).unwrap().identity().unwrap();
        let Err(e) = root.open_existing(OsStr::new("d"), &id) else {
            panic!("a directory is not a state file")
        };
        assert_eq!(e.code, flux_fs::Code::DestinationError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::IsADirectory, "{e:?}");
    }

    fn link(payload: &OsStr) -> flux_fs::Link {
        flux_fs::Link { payload: payload.to_os_string(), kind: flux_fs::LinkKind::Unspecified }
    }

    #[test]
    fn create_symlink_and_read_link_round_trip_every_payload_shape() {
        use std::os::unix::ffi::OsStrExt;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let payloads: [&OsStr; 5] = [
            OsStr::new("rel/x"),
            OsStr::new("/abs/x"),
            OsStr::new("dangling"),
            OsStr::new("../../etc/passwd"),
            OsStr::from_bytes(b"na\xefve"),
        ];
        for (i, payload) in payloads.iter().enumerate() {
            let name = format!("l{i}");
            root.create_symlink(OsStr::new(&name), &link(payload)).unwrap();
            let got = root.read_link(OsStr::new(&name)).unwrap();
            assert_eq!(got.payload.as_bytes(), payload.as_bytes(), "{name}: handle read_link");
            assert_eq!(
                got.kind,
                flux_fs::LinkKind::Unspecified,
                "{name}: a POSIX link has no kind"
            );
            let std_got = std::fs::read_link(d.path().join(&name)).unwrap();
            assert_eq!(std_got.as_os_str().as_bytes(), payload.as_bytes(), "{name}: std agrees");
        }
    }

    #[test]
    fn create_symlink_refuses_an_occupied_name() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("file"), b"keep").unwrap();
        std::fs::create_dir(d.path().join("dir")).unwrap();
        std::os::unix::fs::symlink("nowhere", d.path().join("dangling")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        for name in ["file", "dir", "dangling"] {
            let e = root.create_symlink(OsStr::new(name), &link(OsStr::new("other"))).unwrap_err();
            assert_eq!(e.source.kind(), std::io::ErrorKind::AlreadyExists, "{name}: {e:?}");
        }
        assert_eq!(std::fs::read(d.path().join("file")).unwrap(), b"keep");
        assert!(d.path().join("dir").is_dir());
        assert_eq!(
            std::fs::read_link(d.path().join("dangling")).unwrap(),
            std::path::Path::new("nowhere"),
            "an occupied dangling link keeps its payload"
        );
    }

    #[test]
    fn create_symlink_refuses_a_bad_component() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("a")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let e = root.create_symlink(OsStr::new("a/b"), &link(OsStr::new("x"))).unwrap_err();
        assert_eq!(e.code, flux_fs::Code::SafetyRejected, "{e:?}");
        assert!(std::fs::symlink_metadata(d.path().join("a/b")).is_err(), "nothing created");
        assert_eq!(std::fs::read_dir(d.path().join("a")).unwrap().count(), 0);
    }

    #[test]
    fn a_created_links_metadata_is_the_links_own() {
        use flux_fs::DirHandle as _;
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("target"), b"payload bytes").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        root.create_symlink(OsStr::new("l"), &link(OsStr::new("target"))).unwrap();
        let l = root.metadata(OsStr::new("l")).unwrap();
        let t = root.metadata(OsStr::new("target")).unwrap();
        assert_eq!(l.file_type, flux_fs::FileType::Symlink);
        assert_eq!(t.file_type, flux_fs::FileType::File);
        assert!(matches!(l.identity, flux_fs::FileIdentity::Strong(_)), "{l:?}");
        assert_ne!(l.identity, t.identity, "the link is its own object");
        // Cross-check against the OS: the link's inode is not the target's.
        use std::os::unix::fs::MetadataExt;
        let std_target = std::fs::metadata(d.path().join("l")).unwrap();
        let std_link = std::fs::symlink_metadata(d.path().join("l")).unwrap();
        assert_ne!(std_link.ino(), std_target.ino());
    }

    #[test]
    fn rename_and_remove_act_on_the_link_never_its_target() {
        let d = TempDir::new().unwrap();
        let target = d.path().join("target");
        std::fs::write(&target, b"precious").unwrap();
        std::fs::write(d.path().join("victim"), b"replaced").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let kept = |why: &str| {
            assert_eq!(std::fs::read(&target).unwrap(), b"precious", "{why}");
            assert!(!std::fs::symlink_metadata(&target).unwrap().is_symlink(), "{why}");
        };

        // rename_no_replace moves the link itself.
        root.create_symlink(OsStr::new("l1"), &link(OsStr::new("target"))).unwrap();
        root.rename_no_replace(OsStr::new("l1"), &root, OsStr::new("l2")).unwrap();
        assert!(std::fs::symlink_metadata(d.path().join("l1")).is_err());
        assert_eq!(
            std::fs::read_link(d.path().join("l2")).unwrap(),
            std::path::Path::new("target")
        );
        kept("after rename_no_replace");

        // rename_replace over a regular file replaces that file with the link.
        root.rename_replace(OsStr::new("l2"), &root, OsStr::new("victim")).unwrap();
        assert!(std::fs::symlink_metadata(d.path().join("l2")).is_err());
        let v = std::fs::symlink_metadata(d.path().join("victim")).unwrap();
        assert!(v.is_symlink(), "the link now sits at the replaced name");
        kept("after rename_replace");

        // remove_file unlinks the link and leaves the target.
        root.remove_file(OsStr::new("victim")).unwrap();
        assert!(std::fs::symlink_metadata(d.path().join("victim")).is_err());
        kept("after remove_file");
    }

    #[test]
    fn read_link_refuses_a_file_and_a_directory_as_invalid_input() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("f"), b"x").unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        for name in ["f", "sub"] {
            let e = root.read_link(OsStr::new(name)).unwrap_err();
            assert_eq!(e.code, flux_fs::Code::IoError, "{name}: {e:?}");
            assert_eq!(e.source.kind(), std::io::ErrorKind::InvalidInput, "{name}: {e:?}");
        }
    }

    #[test]
    fn an_empty_payload_is_the_os_error_not_a_panic() {
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let r = root.create_symlink(OsStr::new("l"), &link(OsStr::new("")));
        let name = d.path().join("l");
        // Linux answers ENOENT for an empty symlink target and creates nothing. macOS creates an empty-target link, so
        // elsewhere either outcome is accepted: an error leaves nothing behind, a success reads back an empty payload.
        #[cfg(target_os = "linux")]
        {
            let e = r.unwrap_err();
            assert_eq!(e.source.kind(), std::io::ErrorKind::NotFound, "{e:?}");
            assert!(std::fs::symlink_metadata(&name).is_err(), "nothing created: {e:?}");
        }
        #[cfg(not(target_os = "linux"))]
        match r {
            Err(e) => assert!(std::fs::symlink_metadata(&name).is_err(), "nothing created: {e:?}"),
            Ok(_) => {
                assert!(std::fs::read_link(&name).unwrap().as_os_str().is_empty(), "empty payload");
                std::fs::remove_file(&name).unwrap();
            }
        }
    }
}

#[cfg(windows)]
mod windows_arm {
    use flux_fs::{DestinationRoot, DirHandle};
    use flux_platform::StdFileSystem;
    use std::ffi::OsStr;
    use tempfile::TempDir;

    #[test]
    fn mount_root_is_never_reported_on_windows() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let sub = root.open_dir(OsStr::new("sub")).unwrap();
        assert_eq!(sub.mount_root(&root).unwrap(), flux_fs::MountRoot::No);
    }

    #[test]
    fn canonical_path_names_the_junctions_target_in_the_same_form_as_canonicalize() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        if !junction(&d.path().join("real"), &d.path().join("junc")) {
            panic!("could not create a junction; mklink /J requires no privilege");
        }
        let through = StdFileSystem.destination_root(&d.path().join("junc")).unwrap();
        let direct = StdFileSystem.destination_root(&d.path().join("real")).unwrap();
        let expected = std::fs::canonicalize(d.path().join("real")).unwrap();
        assert_eq!(through.canonical_path().unwrap(), expected);
        assert_eq!(direct.canonical_path().unwrap(), expected);
        assert!(expected.to_string_lossy().starts_with(r"\\?\"), "{expected:?}");
    }

    #[test]
    fn create_claim_store_makes_a_store_through_the_handle() {
        use flux_fs::{ClaimKey, ClaimRecord, ClaimStatus, ClaimStore, Durability, FluxPathKey};
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut store =
            root.create_claim_store(OsStr::new("state.db"), Durability::Normal).unwrap();
        let key = ClaimKey::new(flux_fs::ObjectId { volume: 1, index: 1 }, OsStr::new("a"));
        let rec = ClaimRecord { target: FluxPathKey(b"a".to_vec()), status: ClaimStatus::Existing };
        store.insert_if_absent(&key, &rec).unwrap();
        store.flush().unwrap();
        assert!(
            d.path().join("state.db").is_file(),
            "the store is a file in the handle's directory"
        );
        let err = match root.create_claim_store(OsStr::new("state.db"), Durability::Normal) {
            Err(e) => e,
            Ok(_) => panic!("a taken name must be refused"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn the_real_store_passes_the_conformance_suite_through_the_handle() {
        use flux_fs::Durability;
        let d = TempDir::new().unwrap();
        let n = std::cell::Cell::new(0u32);
        flux_fs::claims::conformance::run_all(|| {
            n.set(n.get() + 1);
            let sub = format!("c{}", n.get());
            std::fs::create_dir(d.path().join(&sub)).unwrap();
            let root = StdFileSystem.destination_root(&d.path().join(&sub)).unwrap();
            root.create_claim_store(OsStr::new("state.db"), Durability::Normal).unwrap()
        });
    }

    #[test]
    fn create_claim_store_never_follows_a_link() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        if !junction(&d.path().join("real"), &d.path().join("state.db")) {
            panic!("could not create a junction; mklink /J requires no privilege");
        }
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = match root.create_claim_store(OsStr::new("state.db"), flux_fs::Durability::Normal)
        {
            Err(e) => e,
            Ok(_) => panic!("a name a junction holds is taken"),
        };
        assert!(
            err.source.kind() == std::io::ErrorKind::AlreadyExists
                || err.code == flux_fs::Code::SafetyRejected,
            "got {err:?}"
        );
        assert!(
            std::fs::read_dir(d.path().join("real")).unwrap().next().is_none(),
            "nothing may have been created at the junction's TARGET"
        );
    }

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

    fn link(payload: &OsStr) -> flux_fs::Link {
        flux_fs::Link { payload: payload.to_os_string(), kind: flux_fs::LinkKind::Unspecified }
    }

    /// Whether this process may create a symlink at all (Developer Mode or the privilege).
    fn can_create_symlinks(dir: &std::path::Path) -> bool {
        let probe = dir.join("can_create_probe");
        let ok = std::os::windows::fs::symlink_file("nowhere", &probe).is_ok();
        let _ = std::fs::remove_file(&probe);
        ok
    }

    #[test]
    fn create_symlink_without_the_right_is_unavailable_and_creates_nothing() {
        let d = TempDir::new().unwrap();
        if can_create_symlinks(d.path()) {
            println!("skipped: this runner can create symlinks");
            return;
        }
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let e = root.create_symlink(OsStr::new("l"), &link(OsStr::new("target"))).unwrap_err();
        assert_eq!(e.code, flux_fs::Code::SymlinkCreationUnavailable, "{e:?}");
        assert!(std::fs::symlink_metadata(d.path().join("l")).is_err(), "nothing created");
    }

    #[test]
    fn create_symlink_round_trips_file_and_dir_links_when_the_runner_can() {
        use flux_fs::{Link, LinkKind};
        let d = TempDir::new().unwrap();
        if !can_create_symlinks(d.path()) {
            println!("skipped: this runner cannot create symlinks");
            return;
        }
        std::fs::create_dir(d.path().join("target_dir")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let cases: [(&str, &str, LinkKind); 5] = [
            ("a", r"x\y", LinkKind::File),
            ("dl", "target_dir", LinkKind::Dir),
            ("d", "nowhere", LinkKind::File),
            ("abs", r"C:\Windows", LinkKind::File),
            ("L", "target_dir", LinkKind::Dir),
        ];
        for (name, payload, kind) in cases {
            let made = Link { payload: payload.into(), kind };
            root.create_symlink(OsStr::new(name), &made).unwrap();
            let back = root.read_link(OsStr::new(name)).unwrap();
            assert_eq!(back.payload, OsStr::new(payload), "{name}");
            assert_eq!(back.kind, kind, "{name}");
        }
        for (name, payload, kind) in cases {
            let made = Link { payload: payload.into(), kind };
            let e = root.create_symlink(OsStr::new(name), &made).unwrap_err();
            assert_eq!(e.source.kind(), std::io::ErrorKind::AlreadyExists, "{name}: {e:?}");
        }
        // `Unspecified` creates a file link (spec D9).
        let u = Link { payload: "x".into(), kind: LinkKind::Unspecified };
        root.create_symlink(OsStr::new("u"), &u).unwrap();
        assert_eq!(root.read_link(OsStr::new("u")).unwrap().kind, LinkKind::File);
        // An exclusive create does not follow a dangling link.
        let dangling = Link { payload: "nowhere".into(), kind: LinkKind::File };
        let e = root.create_symlink(OsStr::new("d"), &dangling).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::AlreadyExists, "{e:?}");
        assert!(!d.path().join("nowhere").exists(), "nothing created through the dangling link");
        // A regular file and a directory holding the name are AlreadyExists too.
        std::fs::write(d.path().join("rf"), b"x").unwrap();
        std::fs::create_dir(d.path().join("rd")).unwrap();
        for name in ["rf", "rd"] {
            let e = root.create_symlink(OsStr::new(name), &link(OsStr::new("z"))).unwrap_err();
            assert_eq!(e.source.kind(), std::io::ErrorKind::AlreadyExists, "{name}: {e:?}");
        }
        // Nothing is created through an existing link.
        let e = root.create_symlink(OsStr::new("L\\child"), &link(OsStr::new("z")));
        assert!(e.is_err(), "a separator is a bad component");
        assert!(
            root.open_dir(OsStr::new("L")).is_err(),
            "a handle cannot be opened through a link"
        );
        assert_eq!(std::fs::read_dir(d.path().join("target_dir")).unwrap().count(), 0);
    }

    #[test]
    fn rename_replace_replaces_a_directory_link_with_a_regular_file() {
        let d = TempDir::new().unwrap();
        if !can_create_symlinks(d.path()) {
            println!("skipped: this runner cannot create symlinks");
            return;
        }
        let t = d.path().join("T");
        std::fs::create_dir(&t).unwrap();
        std::fs::write(t.join("inside"), b"keep").unwrap();
        std::os::windows::fs::symlink_dir("T", d.path().join("L")).unwrap();
        std::fs::write(d.path().join("f"), b"staged bytes").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        root.rename_replace(OsStr::new("f"), &root, OsStr::new("L")).unwrap();
        let meta = std::fs::symlink_metadata(d.path().join("L")).unwrap();
        assert!(meta.is_file() && !meta.is_symlink(), "L is now a regular file: {meta:?}");
        assert_eq!(std::fs::read(d.path().join("L")).unwrap(), b"staged bytes");
        assert_eq!(std::fs::read(t.join("inside")).unwrap(), b"keep", "T is untouched");
        assert!(std::fs::symlink_metadata(d.path().join("f")).is_err(), "f is gone");
    }

    #[test]
    fn rename_replace_over_a_junction_is_still_refused() {
        let d = TempDir::new().unwrap();
        let t = d.path().join("T");
        std::fs::create_dir(&t).unwrap();
        std::fs::write(t.join("inside"), b"keep").unwrap();
        if !junction(&t, &d.path().join("J")) {
            println!("skipped: mklink /J failed on this runner");
            return;
        }
        std::fs::write(d.path().join("f"), b"staged bytes").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let r = root.rename_replace(OsStr::new("f"), &root, OsStr::new("J"));
        assert!(r.is_err(), "replacing a junction must be refused");
        let meta = std::fs::symlink_metadata(d.path().join("J")).unwrap();
        assert!(!meta.is_file(), "J is still not a regular file: {meta:?}");
        assert!(
            std::os::windows::fs::MetadataExt::file_attributes(&meta) & 0x400 != 0,
            "J is still a reparse point: {meta:?}"
        );
        assert_eq!(std::fs::read(t.join("inside")).unwrap(), b"keep", "T is intact");
        assert!(std::fs::symlink_metadata(d.path().join("f")).is_ok(), "f still exists");
    }

    #[test]
    fn rename_replace_still_replaces_a_file_with_a_directory_link() {
        let d = TempDir::new().unwrap();
        if !can_create_symlinks(d.path()) {
            println!("skipped: this runner cannot create symlinks");
            return;
        }
        let t = d.path().join("T");
        std::fs::create_dir(&t).unwrap();
        std::os::windows::fs::symlink_dir("T", d.path().join("staged")).unwrap();
        std::fs::write(d.path().join("F"), b"old").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        root.rename_replace(OsStr::new("staged"), &root, OsStr::new("F")).unwrap();
        let meta = std::fs::symlink_metadata(d.path().join("F")).unwrap();
        assert!(meta.is_symlink(), "F is now a link: {meta:?}");
        assert!(std::fs::symlink_metadata(d.path().join("staged")).is_err(), "staged is gone");
        assert!(t.is_dir(), "T is untouched");
    }

    #[test]
    fn read_link_on_a_junction_is_special_file_unsupported() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        if !junction(&d.path().join("real"), &d.path().join("junc")) {
            panic!("could not create a junction; mklink /J requires no privilege");
        }
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let e = root.read_link(OsStr::new("junc")).unwrap_err();
        assert_eq!(e.code, flux_fs::Code::SpecialFileUnsupported, "{e:?}");
    }

    #[test]
    fn read_link_on_a_file_is_invalid_input() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("f"), b"x").unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let e = root.read_link(OsStr::new("f")).unwrap_err();
        assert_eq!(e.code, flux_fs::Code::IoError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::InvalidInput, "{e:?}");
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
            // NOT a skip. `mklink /J` needs no privilege on Windows -- every
            // junction test in this file relies on that and they all run here --
            // so a failure to make one is a broken environment, not a capability
            // this machine legitimately lacks. A silent skip would let all of
            // these vanish from a CI run while it still reported green, which is
            // the one thing a skipping test must never do.
            panic!("could not create a junction; mklink /J requires no privilege");
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

        // The error is INSPECTED, not merely required to exist: a discarded
        // unwrap_err() would pass if the rename failed for any unrelated reason,
        // and this test is the one that proves the publish is handle-relative.
        let err =
            stage.rename_no_replace(OsStr::new("tmp"), &root, OsStr::new("taken")).unwrap_err();
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
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

        // And it still refuses to create the same name twice -- checking WHICH
        // error, because `is_err()` alone would also pass if the handle had stopped
        // working entirely, which is the failure this test is meant to notice.
        let err = match root.create_dir(OsStr::new("fresh")) {
            Err(e) => e,
            Ok(_) => panic!("create_dir must refuse an occupied name"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_junction_is_accepted_as_the_destination_root() {
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
            // NOT a skip. `mklink /J` needs no privilege on Windows -- every
            // junction test in this file relies on that and they all run here --
            // so a failure to make one is a broken environment, not a capability
            // this machine legitimately lacks. A silent skip would let all of
            // these vanish from a CI run while it still reported green, which is
            // the one thing a skipping test must never do.
            panic!("could not create a junction; mklink /J requires no privilege");
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

    #[test]
    fn remove_file_still_removes_a_junction_but_not_its_target() {
        // The OTHER half of the directory guard, and the half its fix puts at risk.
        // POSIX is the specification: unlinkat refuses a DIRECTORY but unlinks a
        // SYMLINK whatever it points at. So a name surrogate must stay removable --
        // and removing it must not touch what it points at.
        //
        // The guard decides on the reparse TAG, not the reparse BIT. A plain
        // directory has tag 0, which is not a surrogate, so it is refused by
        // remove_file_refuses_an_ordinary_directory above; a junction's tag
        // 0xA0000003 IS a surrogate, so it is removed here. The bit alone could not
        // tell these apart from a OneDrive directory placeholder (tag 0x9000701A,
        // measured on this machine), which is a real directory carrying cloud
        // metadata and must be refused like any other.
        let d = TempDir::new().unwrap();
        let target = d.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("precious"), b"x").unwrap();
        if !junction(&target, &d.path().join("link")) {
            // NOT a skip. `mklink /J` needs no privilege on Windows -- every
            // junction test in this file relies on that and they all run here --
            // so a failure to make one is a broken environment, not a capability
            // this machine legitimately lacks. A silent skip would let all of
            // these vanish from a CI run while it still reported green, which is
            // the one thing a skipping test must never do.
            panic!("could not create a junction; mklink /J requires no privilege");
        }

        let root = StdFileSystem.destination_root(d.path()).unwrap();
        root.remove_file(OsStr::new("link")).unwrap();

        assert!(!d.path().join("link").exists(), "the junction itself must be removed");
        assert!(target.join("precious").is_file(), "its TARGET must be untouched");
    }

    #[test]
    fn remove_file_works_on_a_volume_that_cannot_answer_a_tag_query() {
        // MEASURED on a real FAT32 volume: FileAttributeTagInformation does not
        // answer there. An earlier version of the guard refused whenever the answer
        // was unknown, and the result was that remove_file returned IsADirectory for
        // an ORDINARY FILE -- no file on that volume could be deleted at all. The
        // guard now falls back to FileBasicInformation, which reports the directory
        // attribute without a tag, so a file deletes and a directory is still
        // refused.
        //
        // Set FLUX_FAT32_ROOT to an existing directory on such a volume to run it.
        // There is no way to synthesise one, so it skips by default rather than
        // pinning a drive letter no other machine has.
        let Ok(base) = std::env::var("FLUX_FAT32_ROOT") else {
            eprintln!("SKIPPED: set FLUX_FAT32_ROOT to a directory on a FAT32 volume");
            return;
        };
        let dir = std::path::Path::new(&base).join("fluxguard");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let root = StdFileSystem.destination_root(&dir).unwrap();
        drop(root.create_new(OsStr::new("afile")).unwrap());
        root.remove_file(OsStr::new("afile")).unwrap();
        assert!(!dir.join("afile").exists());

        std::fs::create_dir(dir.join("adir")).unwrap();
        let err = root.remove_file(OsStr::new("adir")).unwrap_err();
        assert_eq!(err.source.kind(), std::io::ErrorKind::IsADirectory);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_new_reports_already_exists_when_a_directory_holds_the_name() {
        // The twin of an_occupied_name_reports_already_exists_through_the_handle, and
        // the case that round's fix missed. The kernel distinguishes a name held by a
        // file from a name held by a directory -- STATUS_OBJECT_NAME_COLLISION versus
        // STATUS_FILE_IS_A_DIRECTORY -- and to create_new they are one situation: the
        // name is taken. MEASURED before the fix: POSIX answered AlreadyExists for
        // both while Windows answered ErrorKind::Other for the directory.
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("taken")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();

        let err = match root.create_new(OsStr::new("taken")) {
            Err(e) => e,
            Ok(_) => panic!("create_new must refuse a name a directory holds"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_stream_separator_is_refused_before_the_filesystem_is_touched() {
        // MEASURED before this check: create_new("host:stream") returned Ok and wrote
        // an alternate data STREAM inside the existing file `host` -- the directory
        // still held one entry and no `host:stream` was in it. The caller asked for
        // one name and a different object was written, reported as success.
        //
        // A colon is legal in a POSIX filename, so only the Windows arm refuses it;
        // the length bound in the same function applies to both, because that one
        // refuses nothing any filesystem accepts.
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("host")).unwrap());

        let err = match root.create_new(OsStr::new("host:stream")) {
            Err(e) => e,
            Ok(_) => panic!("a name carrying a stream separator is not one component"),
        };
        assert_eq!(err.code, flux_fs::Code::SafetyRejected);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn create_new_refuses_a_name_a_junction_already_holds() {
        // WHAT THIS PROVES: a name a junction holds is taken, so create_new refuses
        // it rather than writing through.
        //
        // WHAT IT DOES NOT PROVE, stated because its previous name claimed otherwise.
        // It does NOT guard FILE_OPEN_REPARSE_POINT. MEASURED BY MUTATION: removing
        // that flag from create_new_at leaves this test, and the whole suite, green.
        // A junction makes FILE_CREATE fail whether or not the open would have
        // followed it, because the name is occupied either way -- so this test
        // cannot fail for the reason its old name advertised.
        //
        // The test that DOES guard the flag is
        // create_new_refuses_a_name_a_dangling_symlink_holds below, which needs
        // SeCreateSymbolicLinkPrivilege and skips without it. Two tests, honestly
        // labelled, beat one whose name implies coverage the mutation denies.
        let d = TempDir::new().unwrap();
        let target = d.path().join("target");
        std::fs::create_dir(&target).unwrap();
        if !junction(&target, &d.path().join("link")) {
            // NOT a skip. `mklink /J` needs no privilege on Windows -- every
            // junction test in this file relies on that and they all run here --
            // so a failure to make one is a broken environment, not a capability
            // this machine legitimately lacks. A silent skip would let all of
            // these vanish from a CI run while it still reported green, which is
            // the one thing a skipping test must never do.
            panic!("could not create a junction; mklink /J requires no privilege");
        }

        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = match root.create_new(OsStr::new("link")) {
            Err(e) => e,
            Ok(_) => panic!("a name a surrogate holds is taken"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn an_unpaired_surrogate_does_not_smuggle_a_separator_past_validation() {
        // Every rule check_component enforces was one unpaired surrogate away from
        // being skipped. A Windows OsStr is WTF-16, so a name carrying a lone
        // surrogate has to_str() == None and took a branch that returned Ok
        // unconditionally -- under a comment asserting such a name "carries no
        // separator by construction", which does not follow, because a surrogate can
        // sit beside any other character.
        //
        // MEASURED before the fix: "host" + U+D800 + ":stream" was ACCEPTED and
        // create_new wrote an object the caller had not named. The backslash form was
        // refused only by the kernel, and only by accident.
        use std::os::windows::ffi::OsStringExt;

        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("host")).unwrap());

        let build = |tail: &str| {
            let mut w: Vec<u16> = "host".encode_utf16().collect();
            w.push(0xD800);
            w.extend(tail.encode_utf16());
            std::ffi::OsString::from_wide(&w)
        };

        // The corpus is EVERY rule the code-unit path enforces, not a sample of
        // them. It held the colon and the two separators and omitted the NUL, and
        // MEASURED: deleting the NUL arm from that path left the whole suite green.
        // A validator with four rules needs four cases here, or the untested rule is
        // the one that silently goes away.
        for tail in [":stream", r"\..\escape", "/escape", "\0nul"] {
            let name = build(tail);
            assert!(name.to_str().is_none(), "the name must take the non-UTF-8 branch");
            let err = match root.create_new(&name) {
                Err(e) => e,
                Ok(_) => panic!("a surrogate must not smuggle {tail:?} past validation"),
            };
            assert_eq!(err.code, flux_fs::Code::SafetyRejected);
        }

        assert_eq!(
            std::fs::read_dir(d.path()).unwrap().count(),
            1,
            "nothing beyond `host` may have been created"
        );
    }

    #[test]
    fn open_dir_works_on_a_volume_that_cannot_answer_a_tag_query() {
        // MEASURED on a real FAT32 volume, where the tag query answers 0xC000000D:
        // open_dir asks for a reparse tag on EVERY directory it opens, and failing
        // that call outright meant directory traversal on the whole volume failed.
        // The sibling guard may_remove had already learned to fall back to
        // FileBasicInformation; this one had not.
        //
        // The fallback is deliberately asymmetric. A CLEAR reparse attribute proves
        // there is no reparse point, so None is the true answer -- that is this test.
        // A SET attribute with no readable tag cannot be judged and is REFUSED, since
        // answering None there would traverse a surrogate. FAT32 has no reparse
        // points at all, so only the first case arises on it.
        //
        // Set FLUX_FAT32_ROOT to a directory on such a volume to run this.
        let Ok(base) = std::env::var("FLUX_FAT32_ROOT") else {
            eprintln!("SKIPPED: set FLUX_FAT32_ROOT to a directory on a FAT32 volume");
            return;
        };
        let dir = std::path::Path::new(&base).join("fluxtraverse");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("child")).unwrap();

        let root = StdFileSystem.destination_root(&dir).unwrap();
        let child = root.open_dir(OsStr::new("child")).unwrap();
        drop(child.create_new(OsStr::new("inside")).unwrap());
        assert!(dir.join("child/inside").is_file(), "the write must land through the handle");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_symlink_on_a_fat32_volume_is_unavailable_and_creates_nothing() {
        // Cut 10a ruling R12: a FAT volume has no file ids, so the handle's identity is
        // Unavailable, create_symlink SKIPS its identity re-check and calls
        // CreateSymbolicLinkW, which such a volume cannot satisfy. Expected outcome is
        // Code::SymlinkCreationUnavailable ("the destination filesystem cannot hold a
        // symlink"), Win32 error 1 or 50. NOT yet measured: this test IS the
        // measurement, so the PROBE line below is always printed (run with --nocapture).
        //
        // Set FLUX_FAT32_ROOT to a directory on a FAT32 volume to run this.
        let Ok(base) = std::env::var("FLUX_FAT32_ROOT") else {
            eprintln!("SKIPPED: set FLUX_FAT32_ROOT to a directory on a FAT32 volume");
            return;
        };

        // Removes the scratch directory on every path, a panic included.
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let dir = std::path::Path::new(&base).join("fluxsymlink");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _cleanup = Cleanup(dir.clone());

        let root = StdFileSystem.destination_root(&dir).unwrap();
        let identity = root.identity().unwrap();
        eprintln!("PROBE fat32 identity: {identity:?}");
        assert!(
            matches!(identity, flux_fs::FileIdentity::Unavailable),
            "FLUX_FAT32_ROOT must name a volume with no file ids (FAT32); identity was {identity:?}"
        );

        let link = flux_fs::Link { payload: "x".into(), kind: flux_fs::LinkKind::File };
        let outcome = root.create_symlink(OsStr::new("l"), &link);
        let e = match outcome {
            Ok(()) => {
                let _ = std::fs::remove_file(dir.join("l"));
                let _ = std::fs::remove_dir(dir.join("l"));
                eprintln!("PROBE fat32 create_symlink: unexpectedly created");
                panic!("PROBE fat32 create_symlink: unexpectedly created");
            }
            Err(e) => e,
        };
        eprintln!(
            "PROBE fat32 create_symlink: code={:?} kind={:?} raw_os_error={:?} message={}",
            e.code,
            e.source.kind(),
            e.source.raw_os_error(),
            e
        );
        assert_eq!(e.code, flux_fs::Code::SymlinkCreationUnavailable);

        let after = root.metadata(OsStr::new("l"));
        assert!(
            matches!(&after, Err(m) if m.source.kind() == std::io::ErrorKind::NotFound),
            "nothing named `l` may exist afterwards; metadata said {:?}",
            after.as_ref().map(|_| "exists").map_err(|m| m.source.kind())
        );
    }

    #[test]
    fn create_new_refuses_a_name_a_dangling_symlink_holds() {
        // THIS is the test that guards FILE_OPEN_REPARSE_POINT, and the reason the
        // flag is there. Without it the kernel follows the link during the existence
        // check, and a DANGLING link has no existing object to collide with -- so
        // FILE_CREATE succeeds and writes the file at the link's TARGET, outside the
        // destination tree.
        //
        // Its junction sibling above cannot catch that, because a junction occupies
        // the name either way. Measured by mutation: removing the flag leaves that
        // test green.
        //
        // Creating a file symlink needs SeCreateSymbolicLinkPrivilege, which an
        // ordinary account does not hold, so this SKIPS rather than fails and does
        // not run on the machine this was written on. The POSIX arm covers the same
        // property unconditionally in create_new_refuses_a_name_a_symlink_already_holds.
        let d = TempDir::new().unwrap();
        let outside = d.path().join("outside");
        if std::os::windows::fs::symlink_file(&outside, d.path().join("bait")).is_err() {
            eprintln!("SKIPPED: no SeCreateSymbolicLinkPrivilege; cannot create a file symlink");
            return;
        }
        assert!(!outside.exists(), "the link must dangle for this to mean anything");

        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let err = match root.create_new(OsStr::new("bait")) {
            Err(e) => e,
            Ok(_) => panic!("a name a symlink holds is taken"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(!outside.exists(), "nothing may have been created at the link's TARGET");
    }

    #[test]
    fn rename_moves_a_junction_itself_and_not_what_it_points_at() {
        // The trait requires that NO mutating method traverses a name-surrogate, and
        // rename_at passes FILE_OPEN_REPARSE_POINT for exactly that reason -- but
        // nothing tested it. MEASURED: removing that flag left all 207 tests green,
        // so the rule held only by the flag being there, not by anything noticing if
        // it left. remove_file and create_new each had a guard; rename had none.
        //
        // Renaming a junction must move the LINK. If the open followed it instead,
        // the rename would act on the target directory -- moving something outside
        // the destination subtree that the caller never named.
        let d = TempDir::new().unwrap();
        let target = d.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("precious"), b"x").unwrap();
        if !junction(&target, &d.path().join("link")) {
            panic!("could not create a junction; mklink /J requires no privilege");
        }

        let root = StdFileSystem.destination_root(d.path()).unwrap();
        root.rename_no_replace(OsStr::new("link"), &root, OsStr::new("moved")).unwrap();

        // The link moved.
        assert!(!d.path().join("link").exists(), "the junction must have moved");
        // Its target did not, and is still reachable through the moved link.
        assert!(target.is_dir(), "the TARGET must not have been renamed");
        assert!(target.join("precious").is_file(), "the target's contents must be untouched");
        assert!(
            d.path().join("moved/precious").is_file(),
            "the moved name must still resolve through the junction to the same target"
        );
    }

    #[test]
    fn rename_replace_refuses_a_read_only_target() {
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("from")).unwrap());
        let to = d.path().join("to");
        std::fs::write(&to, b"protected").unwrap();
        let mut perms = std::fs::metadata(&to).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&to, perms).unwrap();

        let err = root.rename_replace(OsStr::new("from"), &root, OsStr::new("to")).unwrap_err();

        assert_eq!(err.code, flux_fs::Code::PermissionDenied);
        assert!(err.source.raw_os_error().is_none(), "the guard must refuse, not the OS");
        assert_eq!(std::fs::read(&to).unwrap(), b"protected", "the protected file must survive");
        std::fs::remove_file(&to).expect("a read-only file must still be removable");
    }

    #[test]
    fn rename_replace_refuses_a_target_denied_by_acl() {
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("from")).unwrap());
        let to = d.path().join("to");
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
        assert!(!std::fs::metadata(&to).unwrap().permissions().readonly(), "attribute is unset");

        let refused = root.rename_replace(OsStr::new("from"), &root, OsStr::new("to")).is_err();

        let _ = std::process::Command::new("icacls")
            .args([&to.display().to_string(), "/remove:d", &user])
            .output();
        let content = std::fs::read(&to).expect("the destination must still be readable");
        assert!(refused, "a destination we may not write must be refused");
        assert_eq!(content, b"protected", "the protected contents must survive");
    }

    #[test]
    fn a_created_file_accepts_the_metadata_a_copy_applies() {
        // copy_file_at applies times and the read-only bit through the writer that
        // create_new returned. MEASURED: a FILE_GENERIC_WRITE-only handle could not
        // read its own attributes, so set_permissions failed with ACCESS_DENIED.
        use flux_fs::{FileSystem, Perms};
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let w = root.create_new(OsStr::new("f")).unwrap();
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);

        StdFileSystem.set_times(&w, Some(when)).expect("times through the handle");
        StdFileSystem
            .set_permissions(&w, Some(Perms::ReadOnly(true)))
            .expect("the read-only bit through the handle");
        drop(w);

        let m = std::fs::metadata(d.path().join("f")).unwrap();
        assert!(m.permissions().readonly());
        assert_eq!(m.modified().unwrap(), when);
        std::fs::remove_file(d.path().join("f")).expect("a read-only file must still be removable");
    }

    #[test]
    fn a_handle_reports_the_identity_of_the_directory_it_holds() {
        use flux_fs::FileSystem;
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let sub = root.open_dir(OsStr::new("sub")).unwrap();
        let made = root.create_dir(OsStr::new("made")).unwrap();

        for (handle, path) in [
            (&root, d.path().to_path_buf()),
            (&sub, d.path().join("sub")),
            (&made, d.path().join("made")),
        ] {
            let by_handle = handle.identity().unwrap();
            assert!(
                matches!(by_handle, flux_fs::FileIdentity::Strong(_)),
                "{path:?}: {by_handle:?}"
            );
            assert_eq!(by_handle, StdFileSystem.metadata(&path).unwrap().identity, "{path:?}");
        }
        assert_ne!(root.identity().unwrap(), sub.identity().unwrap());
    }

    #[test]
    fn open_read_streams_a_child_file() {
        use std::io::{Read, Write};
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut w = root.create_new(OsStr::new("f")).unwrap();
        w.write_all(&bytes).unwrap();
        drop(w);
        let mut r = root.open_read(OsStr::new("f")).unwrap();
        let mut got = Vec::new();
        r.read_to_end(&mut got).unwrap();
        assert_eq!(got, bytes);
    }

    #[test]
    fn open_read_answers_a_junction_as_a_directory_and_leaves_its_target_alone() {
        // A junction is the only name-surrogate CI can plant (a file symlink needs a privilege the runner lacks).
        // FILE_NON_DIRECTORY_FILE answers STATUS_FILE_IS_A_DIRECTORY BEFORE the reparse-tag check, so the junction is
        // a DestinationError/IsADirectory here (a symlink, if the runner could make one, would be SafetyRejected).
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        if !junction(&d.path().join("real"), &d.path().join("junc")) {
            panic!("could not create a junction; mklink /J requires no privilege");
        }
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let Err(e) = root.open_read(OsStr::new("junc")) else {
            panic!("a junction must not be opened for reading")
        };
        assert_eq!(e.code, flux_fs::Code::DestinationError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::IsADirectory, "{e:?}");
        assert!(
            std::fs::read_dir(d.path().join("real")).unwrap().next().is_none(),
            "the junction's TARGET is untouched"
        );
    }

    #[test]
    fn open_read_refuses_a_directory() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("d")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let Err(e) = root.open_read(OsStr::new("d")) else { panic!("a directory is not readable") };
        assert_eq!(e.code, flux_fs::Code::DestinationError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::IsADirectory, "{e:?}");
    }

    #[test]
    fn open_read_of_a_missing_name_is_not_found() {
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let Err(e) = root.open_read(OsStr::new("nope")) else { panic!("a missing name") };
        assert_eq!(e.code, flux_fs::Code::IoError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::NotFound, "{e:?}");
    }

    /// The Task 7 production path: the WAL thread syncs a duplicate of a writer made through a `DirHandle`,
    /// while the copy loop keeps writing through the original.
    #[test]
    fn a_duplicate_of_a_created_child_syncs_on_another_thread_while_the_original_writes() {
        use flux_fs::FileHandle;
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut w = root.create_new(OsStr::new("f")).unwrap();
        w.write_all(b"abc").unwrap();
        let dup = w.duplicate().unwrap();
        std::thread::scope(|s| {
            s.spawn(|| dup.sync_all().unwrap()).join().unwrap();
        });
        w.write_all(b"def").unwrap();
        // The duplicate still works after the original wrote.
        std::thread::scope(|s| {
            s.spawn(|| dup.sync_all().unwrap()).join().unwrap();
        });
        drop(dup);
        drop(w);
        assert_eq!(std::fs::read(d.path().join("f")).unwrap(), b"abcdef");
    }

    #[test]
    fn a_duplicate_of_a_reopened_child_syncs_on_another_thread() {
        use flux_fs::FileHandle;
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut w = root.create_new(OsStr::new("f")).unwrap();
        w.write_all(b"abcde").unwrap();
        let id = w.identity().unwrap();
        drop(w);
        let mut w2 = root.open_existing(OsStr::new("f"), &id).unwrap();
        w2.truncate_to(3).unwrap();
        let dup = w2.duplicate().unwrap();
        std::thread::scope(|s| {
            s.spawn(|| dup.sync_all().unwrap()).join().unwrap();
        });
        w2.write_all(b"XY").unwrap();
        std::thread::scope(|s| {
            s.spawn(|| dup.sync_all().unwrap()).join().unwrap();
        });
        drop(dup);
        drop(w2);
        assert_eq!(std::fs::read(d.path().join("f")).unwrap(), b"abcXY");
    }

    #[test]
    fn open_existing_appends_after_the_prefix() {
        use flux_fs::FileHandle;
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut w = root.create_new(OsStr::new("f")).unwrap();
        w.write_all(b"abc").unwrap();
        let id = w.identity().unwrap();
        drop(w);
        let mut w2 = root.open_existing(OsStr::new("f"), &id).unwrap();
        assert_eq!(w2.identity().unwrap(), id);
        w2.truncate_to(2).unwrap();
        w2.write_all(b"Z").unwrap();
        drop(w2);
        assert_eq!(root.read_file(OsStr::new("f"), 100).unwrap(), b"abZ");
    }

    #[test]
    fn open_existing_answers_a_junction_as_a_directory_and_leaves_its_target_alone() {
        use flux_fs::FileHandle;
        // As `open_read`'s: FILE_NON_DIRECTORY_FILE answers STATUS_FILE_IS_A_DIRECTORY before the reparse-tag check.
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        if !junction(&d.path().join("real"), &d.path().join("junc")) {
            panic!("could not create a junction; mklink /J requires no privilege");
        }
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let id = root.create_new(OsStr::new("g")).unwrap().identity().unwrap();
        let Err(e) = root.open_existing(OsStr::new("junc"), &id) else {
            panic!("a junction must not be opened for writing")
        };
        assert_eq!(e.code, flux_fs::Code::DestinationError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::IsADirectory, "{e:?}");
        assert!(
            std::fs::read_dir(d.path().join("real")).unwrap().next().is_none(),
            "nothing was created inside the junction's TARGET"
        );
    }

    #[test]
    fn open_existing_refuses_an_identity_mismatch() {
        use flux_fs::FileHandle;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let w = root.create_new(OsStr::new("f")).unwrap();
        let id = w.identity().unwrap();
        drop(w);
        std::fs::remove_file(d.path().join("f")).unwrap();
        drop(root.create_new(OsStr::new("f")).unwrap());
        let Err(e) = root.open_existing(OsStr::new("f"), &id) else {
            panic!("a different object at the name must be refused")
        };
        assert_eq!(e.code, flux_fs::Code::DestinationError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::InvalidData, "{e:?}");
    }

    // A name that is neither 8.3-compatible nor short, so an auto-generated short name would exist on a volume that
    // makes them: the link count must still be 1 (a count of 2 would make resume refuse every partial).
    #[test]
    fn a_long_named_file_reports_one_link() {
        use flux_fs::{DirHandle, FileHandle};
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let name = OsStr::new("a.long-name.flux-partial.0123456789abcdef0123456789abcdef");
        let w = root.create_new(name).unwrap();
        let id = w.identity().unwrap();
        drop(w);
        assert_eq!(root.metadata(name).unwrap().nlink, Some(1));
        // The opened-handle path: `open_existing` refuses a count above 1, so it opening is the handle's own report.
        drop(root.open_existing(name, &id).unwrap());
    }

    #[test]
    fn open_existing_refuses_a_hard_linked_file_and_leaves_the_other_name_alone() {
        use flux_fs::{DirHandle, FileHandle};
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let mut w = root.create_new(OsStr::new("victim")).unwrap();
        w.write_all(b"precious").unwrap();
        let id = w.identity().unwrap();
        drop(w);
        // The planted name: the same object under a second name, so its identity IS `id` and only the link count
        // stops this open from writing through it.
        std::fs::hard_link(d.path().join("victim"), d.path().join("partial")).unwrap();
        assert_eq!(root.metadata(OsStr::new("partial")).unwrap().nlink, Some(2));
        let Err(e) = root.open_existing(OsStr::new("partial"), &id) else {
            panic!("a hard-linked file must not be opened for writing")
        };
        assert_eq!(e.code, flux_fs::Code::SafetyRejected, "{e:?}");
        assert!(e.source.to_string().contains("2 hard links"), "{e:?}");
        assert_eq!(std::fs::read(d.path().join("victim")).unwrap(), b"precious");
        // Unlinking the planted name leaves the object with one link, and it opens again.
        std::fs::remove_file(d.path().join("partial")).unwrap();
        assert_eq!(root.metadata(OsStr::new("victim")).unwrap().nlink, Some(1));
        drop(root.open_existing(OsStr::new("victim"), &id).unwrap());
    }

    #[test]
    fn open_existing_refuses_an_unavailable_expectation() {
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        drop(root.create_new(OsStr::new("f")).unwrap());
        let Err(e) = root.open_existing(OsStr::new("f"), &flux_fs::FileIdentity::Unavailable)
        else {
            panic!("an unavailable expectation proves nothing")
        };
        assert_eq!(e.code, flux_fs::Code::DestinationError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::InvalidData, "{e:?}");
    }

    #[test]
    fn open_existing_of_a_missing_name_is_not_found() {
        use flux_fs::FileHandle;
        let d = TempDir::new().unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let id = root.create_new(OsStr::new("g")).unwrap().identity().unwrap();
        let Err(e) = root.open_existing(OsStr::new("nope"), &id) else { panic!("a missing name") };
        assert_eq!(e.code, flux_fs::Code::IoError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::NotFound, "{e:?}");
    }

    #[test]
    fn open_existing_refuses_a_directory() {
        use flux_fs::FileHandle;
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("d")).unwrap();
        let root = StdFileSystem.destination_root(d.path()).unwrap();
        let id = root.create_new(OsStr::new("g")).unwrap().identity().unwrap();
        let Err(e) = root.open_existing(OsStr::new("d"), &id) else {
            panic!("a directory is not a state file")
        };
        assert_eq!(e.code, flux_fs::Code::DestinationError, "{e:?}");
        assert_eq!(e.source.kind(), std::io::ErrorKind::IsADirectory, "{e:?}");
    }
}

/// Cut 7b: a written file's handle names its object, the one a look-up of its name finds, before and after a rename.
#[test]
fn a_written_files_handle_identity_is_its_objects_across_a_rename() {
    use flux_fs::{DestinationRoot, DirHandle, FileHandle, FileIdentity};
    use std::ffi::OsStr;
    use std::io::Write;
    let d = tempfile::TempDir::new().unwrap();
    let root = flux_platform::StdFileSystem.destination_root(d.path()).unwrap();
    let mut w = root.create_new(OsStr::new("t.tmp")).unwrap();
    w.write_all(b"x").unwrap();
    let held = w.identity().unwrap();
    assert!(!matches!(held, FileIdentity::Unavailable), "{held:?}");
    assert_eq!(root.metadata(OsStr::new("t.tmp")).unwrap().identity, held);
    root.rename_replace(OsStr::new("t.tmp"), &root, OsStr::new("t")).unwrap();
    assert_eq!(root.metadata(OsStr::new("t")).unwrap().identity, held, "a rename keeps the object");
    assert_eq!(w.identity().unwrap(), held);
}
