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

/// What the child prints once it holds the lock. Distinctive, so no line libtest prints can match it.
const CHILD_MARKER: &str = "FLUX_CHILD_HOLDS_LOCK";

/// Run as a child by `a_lock_held_by_another_process_is_busy_and_readable_until_that_process_dies`: take the lock on
/// the named file, say so, and hold it until killed.
fn hold_lock_as_child(dir: &str) {
    let d = StdFileSystem
        .destination_root(std::path::Path::new(dir))
        .expect("child opens the directory");
    let lock = d.open_lock(OsStr::new(NAME)).expect("child opens the lock");
    assert!(lock.try_lock().expect("child try_lock"), "the parent released it before spawning");
    // A line of its own: libtest has already printed `test <name> ... ` WITHOUT a newline when the test body runs, so
    // a bare marker would share that line and never match exactly (measured during execution).
    println!("\n{CHILD_MARKER}");
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(120));
}

#[test]
fn a_lock_held_by_another_process_is_busy_and_readable_until_that_process_dies() {
    if let Ok(dir) = std::env::var("FLUX_LOCK_CHILD_DIR") {
        hold_lock_as_child(&dir);
        return;
    }
    let (tmp, d) = dir();
    let record: Vec<u8> = (0..4096u32).map(|i| (i % 253) as u8).collect();
    {
        let h = d.create_lock(OsStr::new(NAME)).unwrap();
        h.write_at_start(&record).unwrap();
        h.sync_all().unwrap();
    } // closed: nobody holds the lock

    /// Kills and reaps the child however this test ends. A failed assertion unwinds past any explicit kill, and on
    /// Windows the child's open handle would also stop the scratch directory from being removed.
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_lock_held_by_another_process_is_busy_and_readable_until_that_process_dies",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FLUX_LOCK_CHILD_DIR", tmp.path())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the child");
    // Declared after `tmp`, so it is dropped first: the child is gone before the directory is removed.
    let mut child = KillOnDrop(child);
    let stdout = child.0.stdout.take().unwrap();
    let mut lines = std::io::BufRead::lines(std::io::BufReader::new(stdout));
    assert!(
        lines.any(|l| l.map(|l| l.trim() == CHILD_MARKER).unwrap_or(false)),
        "the child reports it holds the lock"
    );

    let probe = d.open_lock(OsStr::new(NAME)).unwrap();
    assert!(!probe.try_lock().unwrap(), "another PROCESS holds it: TARGET_LOCK_BUSY");
    assert_eq!(probe.read_all(4096).unwrap(), record, "and its record is readable meanwhile");

    child.0.kill().unwrap();
    child.0.wait().unwrap();
    // The OS releases a dead process's lock when it closes the process's handles; allow it a moment.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !probe.try_lock().unwrap() {
        assert!(std::time::Instant::now() < deadline, "a dead process's lock was never released");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn the_test_machines_scratch_directory_is_a_local_strong_filesystem() {
    // CI and development machines put their temporary directory on a local filesystem (ext4 or tmpfs, APFS, NTFS or
    // ReFS). WSL's /mnt/* (9p) is NOT local and would answer Unsupported - which is why `just check-linux` builds in
    // the WSL-native filesystem.
    let (_tmp, d) = dir();
    assert_eq!(d.lock_capability().unwrap(), flux_fs::LockCapability::LocalStrong);
}

#[test]
fn the_boot_session_id_is_known_here_and_stable_within_one_boot() {
    let a = flux_platform::boot_session_id();
    let b = flux_platform::boot_session_id();
    println!("boot_session_id = {a}");
    assert_ne!(a, "unknown", "every supported platform exposes one");
    assert!(!a.is_empty());
    assert_eq!(a, b, "two reads in one boot agree");
    #[cfg(target_os = "linux")]
    assert_eq!(a, std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap().trim());
}

#[test]
fn a_lock_name_removed_while_its_holder_is_open_can_be_created_and_locked_again_at_once() {
    // The release unlinks and THEN closes (S99_release). In between, another run must be able to take the name:
    // on Unix unlink removes it at once, and on Windows remove_file uses POSIX delete semantics so that it does too.
    let (_tmp, d) = dir();
    let old = d.create_lock(OsStr::new(NAME)).unwrap();
    assert!(old.try_lock().unwrap());
    old.write_at_start(&[1u8; 4096]).unwrap();
    d.remove_file(OsStr::new(NAME)).expect("unlink the held lock by name");
    let new = d
        .create_lock(OsStr::new(NAME))
        .expect("the name is free at once, before the old handle closes");
    assert!(new.try_lock().unwrap(), "a new object: the old handle's lock is not on it");
    assert_ne!(new.identity().unwrap(), old.identity().unwrap());
    assert_eq!(
        old.read_all(4096).unwrap(),
        vec![1u8; 4096],
        "the old handle still reads its own object"
    );
}

#[cfg(windows)]
#[test]
fn a_delete_pending_name_is_occupied_to_a_create_and_gone_to_an_open() {
    // The legacy-semantics fallback, staged directly: mark the file deleted with the legacy disposition class
    // while a handle keeps it open, which leaves the name "delete pending".
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };
    const DELETE: u32 = 0x0001_0000;
    const FILE_SHARE_ALL: u32 = 0x7;
    let (tmp, d) = dir();
    drop(d.create_lock(OsStr::new(NAME)).unwrap());
    let pending = std::fs::OpenOptions::new()
        .access_mode(DELETE | 0x0012_0089) // DELETE | FILE_GENERIC_READ
        .share_mode(FILE_SHARE_ALL)
        .open(tmp.path().join(NAME))
        .expect("open for delete");
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `info` is a live FILE_DISPOSITION_INFO of the size given; the handle is open.
    let ok = unsafe {
        SetFileInformationByHandle(
            pending.as_raw_handle() as _,
            FileDispositionInfo,
            (&raw const info).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    assert_ne!(ok, 0, "legacy delete disposition: {}", std::io::Error::last_os_error());
    let e = d.create_lock(OsStr::new(NAME)).expect_err("the name is still occupied");
    assert_eq!(e.source.kind(), std::io::ErrorKind::AlreadyExists, "{e:?}");
    let e = d.open_lock(OsStr::new(NAME)).expect_err("the object is logically gone");
    assert_eq!((e.code, e.source.kind()), (Code::IoError, std::io::ErrorKind::NotFound), "{e:?}");
    // `create_new` (a data file's create) meets the same pending name the same way.
    let e = match d.create_new(OsStr::new(NAME)) {
        Ok(_) => panic!("still occupied to a data-file create"),
        Err(e) => e,
    };
    assert_eq!(e.source.kind(), std::io::ErrorKind::AlreadyExists, "{e:?}");
    drop(pending);
    d.create_lock(OsStr::new(NAME)).expect("free once the last handle closes");
}

#[test]
fn the_holder_writes_and_cuts_its_record_after_taking_the_lock() {
    // The protocol's order (F5): take the lock, THEN write the record. Every other test writes before locking.
    let (_tmp, d) = dir();
    let holder = d.create_lock(OsStr::new(NAME)).unwrap();
    assert!(holder.try_lock().unwrap());
    holder.write_at_start(&[1u8; 5000]).expect("write while holding the lock");
    holder.write_at_start(&[2u8; 4096]).expect("overwrite and cut while holding the lock");
    holder.sync_all().unwrap();
    let other = d.open_lock(OsStr::new(NAME)).unwrap();
    assert!(!other.try_lock().unwrap(), "writing and cutting did not release the lock");
    assert_eq!(
        other.read_all(4096).unwrap(),
        vec![2u8; 4096],
        "another handle reads the new record"
    );
}

#[cfg(windows)]
#[test]
fn open_lock_refuses_a_file_symlink() {
    // The junction test is refused earlier, as a directory; this one reaches the name-surrogate check itself.
    let (tmp, d) = dir();
    drop(d.create_lock(OsStr::new(NAME)).unwrap());
    let link = tmp.path().join("link.flux-lock");
    if let Err(e) = std::os::windows::fs::symlink_file(tmp.path().join(NAME), &link) {
        // A file symlink needs SeCreateSymbolicLinkPrivilege (an administrator, or Developer Mode). CI's Windows
        // runner has it, so there this test must run; a machine without it skips with a note.
        assert!(std::env::var_os("CI").is_none(), "CI must be able to create a file symlink: {e}");
        eprintln!("skipped: cannot create a file symlink here ({e})");
        return;
    }
    let e = d.open_lock(OsStr::new("link.flux-lock")).expect_err("never follow a link");
    assert_eq!(e.code, Code::SafetyRejected, "{e:?}");
}
