//! The lock file on a real filesystem (cut 7a Part 1): created and opened through a directory handle, locked without
//! waiting, and readable while another handle holds the lock.

use flux_fs::{Code, DestinationRoot, DirHandle, LockFile};
use flux_platform::StdFileSystem;
use std::ffi::OsStr;
use std::io::ErrorKind;

fn dir() -> (tempfile::TempDir, flux_platform::StdDir) {
    let tmp = tempfile::tempdir().expect("scratch directory");
    let d = StdFileSystem.destination_root(tmp.path()).expect("open the scratch directory");
    (tmp, d)
}

const NAME: &str = "dest.flux-lock";

#[test]
fn create_lock_is_exclusive() {
    let (_tmp, d) = dir();
    let _held = d.create_lock(OsStr::new(NAME)).expect("first create");
    let e = d.create_lock(OsStr::new(NAME)).expect_err("the name is taken");
    assert_eq!(e.source.kind(), ErrorKind::AlreadyExists);
}

#[test]
fn a_second_handle_cannot_take_a_held_lock_until_the_first_is_dropped() {
    let (_tmp, d) = dir();
    let first = d.create_lock(OsStr::new(NAME)).unwrap();
    assert!(first.try_lock().unwrap(), "an unheld lock is granted");
    assert!(first.try_lock().unwrap(), "re-taking a lock this handle holds is granted");
    let second = d.open_lock(OsStr::new(NAME)).unwrap();
    assert!(!second.try_lock().unwrap(), "held by the first handle");
    drop(first);
    assert!(second.try_lock().unwrap(), "closing the holder releases the lock");
}

#[test]
fn the_record_is_readable_while_another_handle_holds_the_lock() {
    // The model's classifier reads the record whether or not its try-lock succeeded (S240_1_read). On Windows a
    // LockFileEx range cannot be read by another handle, which is why the lock sits on one byte far past the record.
    let (_tmp, d) = dir();
    let holder = d.create_lock(OsStr::new(NAME)).unwrap();
    let record: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    holder.write_at_start(&record).unwrap();
    holder.sync_all().unwrap();
    assert!(holder.try_lock().unwrap());
    let reader = d.open_lock(OsStr::new(NAME)).unwrap();
    assert!(!reader.try_lock().unwrap());
    assert_eq!(
        reader.read_all(4096).unwrap(),
        record,
        "read through a second handle while the lock is held"
    );
    assert_eq!(holder.read_all(4096).unwrap(), record, "and through the holder's own handle");
}

#[test]
fn read_all_reads_one_byte_past_its_limit_so_an_oversized_file_shows() {
    let (_tmp, d) = dir();
    let h = d.create_lock(OsStr::new(NAME)).unwrap();
    h.write_at_start(&[7u8; 5000]).unwrap();
    assert_eq!(h.read_all(4096).unwrap().len(), 4097);
    assert_eq!(h.read_all(10_000).unwrap().len(), 5000);
    let empty = d.create_lock(OsStr::new("empty.flux-lock")).unwrap();
    assert!(empty.read_all(4096).unwrap().is_empty());
}

#[test]
fn write_at_start_overwrites_in_place_and_keeps_the_object() {
    let (_tmp, d) = dir();
    let h = d.create_lock(OsStr::new(NAME)).unwrap();
    // An oversized torn lock, as a takeover finds it: longer than the record it will be overwritten with.
    h.write_at_start(&[1u8; 5000]).unwrap();
    let before = h.identity().unwrap();
    h.write_at_start(&[2u8; 4096]).unwrap();
    assert_eq!(
        h.read_all(4096).unwrap(),
        vec![2u8; 4096],
        "exactly the new record: the old tail is cut away"
    );
    assert_eq!(h.identity().unwrap(), before, "§240.5 step 6 overwrites the SAME object");
}

#[test]
fn a_lock_handle_reports_the_identity_the_directory_reports_for_its_name() {
    let (_tmp, d) = dir();
    let h = d.create_lock(OsStr::new(NAME)).unwrap();
    let by_name = d.metadata(OsStr::new(NAME)).unwrap().identity;
    assert!(
        matches!(by_name, flux_fs::FileIdentity::Strong(_)),
        "a local filesystem gives a strong identity"
    );
    assert_eq!(h.identity().unwrap(), by_name);
}

#[test]
fn open_lock_refuses_a_missing_name_and_a_directory() {
    let (_tmp, d) = dir();
    let e = d.open_lock(OsStr::new("absent.flux-lock")).expect_err("nothing there");
    assert_eq!((e.code, e.source.kind()), (Code::IoError, ErrorKind::NotFound));
    let _sub = d.create_dir(OsStr::new("adir")).unwrap();
    let e = d.open_lock(OsStr::new("adir")).expect_err("a directory is not a lock file");
    assert_eq!((e.code, e.source.kind()), (Code::DestinationError, ErrorKind::IsADirectory));
}

#[cfg(unix)]
#[test]
fn open_lock_refuses_a_symlink_even_to_a_real_lock_file() {
    let (tmp, d) = dir();
    let _real = d.create_lock(OsStr::new(NAME)).unwrap();
    std::os::unix::fs::symlink(tmp.path().join(NAME), tmp.path().join("link.flux-lock")).unwrap();
    let e = d.open_lock(OsStr::new("link.flux-lock")).expect_err("never follow a link");
    assert_eq!(e.code, Code::SafetyRejected);
}

#[cfg(windows)]
#[test]
fn open_lock_refuses_a_junction() {
    let (tmp, d) = dir();
    let target = tmp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let link = tmp.path().join("junction.flux-lock");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J needs no privilege");
    let e = d.open_lock(OsStr::new("junction.flux-lock")).expect_err("never follow a link");
    // FILE_NON_DIRECTORY_FILE meets a junction (a directory reparse point) first: either refusal is a refusal,
    // and neither may open the target.
    assert!(matches!(e.code, Code::SafetyRejected | Code::DestinationError), "{e:?}");
}

#[cfg(unix)]
#[test]
fn open_lock_refuses_a_fifo_without_hanging() {
    let (tmp, d) = dir();
    let status = std::process::Command::new("mkfifo")
        .arg(tmp.path().join("fifo.flux-lock"))
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo");
    let e = d.open_lock(OsStr::new("fifo.flux-lock")).expect_err("a FIFO is not a lock file");
    assert_eq!(e.code, Code::DestinationError);
}
