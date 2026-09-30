//! The directory primitives the operation state rests on (cut 7a Part 3a): listing, reading a small file, removing an
//! empty directory, flushing a directory, and renaming a directory - each through a directory handle, never following
//! a link (§149.7).

use flux_fs::{Code, DestinationRoot, DirHandle, FileType};
use flux_platform::StdFileSystem;
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::Path;

fn dir() -> (tempfile::TempDir, flux_platform::StdDir) {
    let tmp = tempfile::tempdir().expect("scratch directory");
    let d = StdFileSystem.destination_root(tmp.path()).expect("open the scratch directory");
    (tmp, d)
}

/// The listing, sorted by name.
fn listing(d: &flux_platform::StdDir) -> Vec<(OsString, FileType)> {
    let mut v: Vec<_> =
        d.read_dir().expect("list").into_iter().map(|e| (e.name, e.file_type)).collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// A link to the directory `target` at `link`: a symlink on Unix, a junction on Windows (`mklink /J` needs no
/// privilege, so this never skips).
fn dir_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("symlink");
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .expect("run mklink");
        assert!(status.success(), "mklink /J needs no privilege");
    }
}

#[test]
fn read_dir_lists_files_directories_and_links_as_links() {
    let (tmp, d) = dir();
    assert!(listing(&d).is_empty(), "an empty directory lists nothing: no . and no ..");
    std::fs::write(tmp.path().join("a"), b"x").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    dir_link(&tmp.path().join("sub"), &tmp.path().join("link"));
    assert_eq!(
        listing(&d),
        vec![
            (OsString::from("a"), FileType::File),
            (OsString::from("link"), FileType::Symlink),
            (OsString::from("sub"), FileType::Dir),
        ]
    );
}

#[test]
fn read_dir_works_on_created_and_opened_handles_and_starts_again_each_call() {
    let (_tmp, d) = dir();
    let sub = d.create_dir(OsStr::new("sub")).unwrap();
    drop(sub.create_new(OsStr::new("f")).unwrap());
    let want = vec![(OsString::from("f"), FileType::File)];
    assert_eq!(listing(&sub), want);
    let again = d.open_dir(OsStr::new("sub")).unwrap();
    assert_eq!(listing(&again), want);
    assert_eq!(
        listing(&again),
        want,
        "a second listing through the same handle lists everything again"
    );
}

#[test]
fn read_file_reads_at_most_one_byte_past_its_limit() {
    let (tmp, d) = dir();
    std::fs::write(tmp.path().join("s"), b"0123456789").unwrap();
    assert_eq!(d.read_file(OsStr::new("s"), 64).unwrap(), b"0123456789");
    assert_eq!(
        d.read_file(OsStr::new("s"), 4).unwrap(),
        b"01234",
        "limit + 1 bytes, so an oversized file shows"
    );
}

#[test]
fn read_file_refuses_a_missing_name_and_a_directory() {
    let (_tmp, d) = dir();
    let e = d.read_file(OsStr::new("absent"), 8).expect_err("nothing there");
    assert_eq!((e.code, e.source.kind()), (Code::IoError, ErrorKind::NotFound));
    drop(d.create_dir(OsStr::new("adir")).unwrap());
    let e = d.read_file(OsStr::new("adir"), 8).expect_err("a directory is not a state file");
    assert_eq!((e.code, e.source.kind()), (Code::DestinationError, ErrorKind::IsADirectory));
}

#[test]
fn read_file_never_follows_a_link() {
    let (tmp, d) = dir();
    #[cfg(unix)]
    {
        std::fs::write(tmp.path().join("real"), b"secret").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("real"), tmp.path().join("link")).unwrap();
        let e = d.read_file(OsStr::new("link"), 64).expect_err("never follow a link");
        assert_eq!(e.code, Code::SafetyRejected, "{e:?}");
    }
    #[cfg(windows)]
    {
        std::fs::create_dir(tmp.path().join("real")).unwrap();
        dir_link(&tmp.path().join("real"), &tmp.path().join("link"));
        let e = d.read_file(OsStr::new("link"), 64).expect_err("never follow a link");
        // FILE_NON_DIRECTORY_FILE meets the junction first; either refusal is a refusal (as `open_lock`'s junction
        // test says), and neither reads the target.
        assert!(matches!(e.code, Code::SafetyRejected | Code::DestinationError), "{e:?}");
    }
}

#[cfg(unix)]
#[test]
fn read_file_refuses_a_fifo_without_hanging() {
    let (tmp, d) = dir();
    let status = std::process::Command::new("mkfifo")
        .arg(tmp.path().join("fifo"))
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo");
    let e = d.read_file(OsStr::new("fifo"), 8).expect_err("a FIFO is not a state file");
    assert_eq!(e.code, Code::DestinationError);
}

#[test]
fn remove_dir_removes_an_empty_directory_and_refuses_a_full_or_missing_one() {
    let (tmp, d) = dir();
    drop(d.create_dir(OsStr::new("empty")).unwrap());
    d.remove_dir(OsStr::new("empty")).expect("an empty directory");
    assert!(!tmp.path().join("empty").exists());
    let full = d.create_dir(OsStr::new("full")).unwrap();
    drop(full.create_new(OsStr::new("f")).unwrap());
    drop(full);
    let e = d.remove_dir(OsStr::new("full")).expect_err("not empty");
    assert_eq!(e.source.kind(), ErrorKind::DirectoryNotEmpty, "{e:?}");
    assert!(tmp.path().join("full").join("f").exists(), "nothing inside is touched");
    let e = d.remove_dir(OsStr::new("absent")).expect_err("nothing there");
    assert_eq!(e.source.kind(), ErrorKind::NotFound, "{e:?}");
}

#[test]
fn remove_dir_refuses_a_file_and_a_link_and_never_follows_one() {
    let (tmp, d) = dir();
    std::fs::write(tmp.path().join("file"), b"x").unwrap();
    let e = d.remove_dir(OsStr::new("file")).expect_err("a file is not a directory");
    assert_eq!(e.source.kind(), ErrorKind::NotADirectory, "{e:?}");
    assert!(tmp.path().join("file").exists());
    std::fs::create_dir(tmp.path().join("target")).unwrap();
    dir_link(&tmp.path().join("target"), &tmp.path().join("link"));
    let e = d.remove_dir(OsStr::new("link")).expect_err("a link is not removed as a directory");
    assert_eq!(e.source.kind(), ErrorKind::NotADirectory, "{e:?}");
    assert!(std::fs::symlink_metadata(tmp.path().join("link")).is_ok(), "the link stays");
    assert!(tmp.path().join("target").is_dir(), "and so does its target");
}

#[test]
fn sync_flushes_every_kind_of_directory_handle() {
    let (_tmp, d) = dir();
    d.sync().expect("the destination root");
    let sub = d.create_dir(OsStr::new("sub")).unwrap();
    sub.sync().expect("a created directory");
    d.open_dir(OsStr::new("sub")).unwrap().sync().expect("an opened directory");
}
