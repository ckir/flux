// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The directory primitives the operation state rests on (cut 7a Part 3a): listing, reading a small file, removing an
//! empty directory, flushing a directory, and renaming a directory - each through a directory handle, never following
//! a link (§149.7).

use flux_fs::{Code, DestinationRoot, DirHandle, FileType};
#[cfg(unix)]
use flux_fs::{FileIdentity, ObjectId};
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

#[test]
fn a_directory_is_renamed_without_replacing_and_keeps_its_contents() {
    let (tmp, d) = dir();
    let building = d.create_dir(OsStr::new("w.creating")).unwrap();
    drop(building.create_new(OsStr::new("manifest")).unwrap());
    drop(building);
    d.rename_no_replace(OsStr::new("w.creating"), &d, OsStr::new("w")).expect("rename a directory");
    assert!(tmp.path().join("w").join("manifest").is_file());
    assert!(!tmp.path().join("w.creating").exists());
    drop(d.create_dir(OsStr::new("x")).unwrap());
    let e = d
        .rename_no_replace(OsStr::new("w"), &d, OsStr::new("x"))
        .expect_err("never replaces a directory");
    assert_eq!(e.source.kind(), ErrorKind::AlreadyExists, "{e:?}");
    assert!(tmp.path().join("w").join("manifest").is_file(), "the source stays");
}

/// Holds a PowerShell process that holds an AF_UNIX socket open. .NET deletes the socket file when the socket closes,
/// so the file exists only while this lives.
#[cfg(windows)]
struct HeldSocket(std::process::Child);

#[cfg(windows)]
impl Drop for HeldSocket {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A socket file at `path`, held until the returned value drops. POSIX binds one with std. Windows 10 and later
/// create one on an AF_UNIX `bind`, which std cannot do there, so PowerShell (.NET) binds it.
#[cfg(unix)]
fn socket_file(path: &Path) -> std::os::unix::net::UnixListener {
    std::os::unix::net::UnixListener::bind(path).expect("bind a unix socket")
}

#[cfg(windows)]
fn socket_file(path: &Path) -> HeldSocket {
    let script = format!(
        "$s = [System.Net.Sockets.Socket]::new([System.Net.Sockets.AddressFamily]::Unix, \
         [System.Net.Sockets.SocketType]::Stream, [System.Net.Sockets.ProtocolType]::Unspecified); \
         $s.Bind([System.Net.Sockets.UnixDomainSocketEndPoint]::new('{}')); Start-Sleep -Seconds 300",
        path.display()
    );
    let child = std::process::Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .spawn()
        .expect("pwsh is on PATH (it is on the GitHub Windows image)");
    let held = HeldSocket(child);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::fs::symlink_metadata(path).is_err() {
        assert!(std::time::Instant::now() < deadline, "the socket file never appeared");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    held
}

#[test]
fn a_socket_file_lists_as_other_and_is_never_read_as_a_state_file() {
    let (tmp, d) = dir();
    let _held = socket_file(&tmp.path().join("sock"));
    assert_eq!(listing(&d), vec![(OsString::from("sock"), FileType::Other)]);
    let e = d.read_file(OsStr::new("sock"), 8).expect_err("a socket is not a state file");
    assert_eq!(e.code, Code::DestinationError, "{e:?}");
}

#[test]
fn read_dir_each_yields_exactly_the_entries_that_exist() {
    let (tmp, d) = dir();
    std::fs::write(tmp.path().join("a"), b"x").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    dir_link(&tmp.path().join("sub"), &tmp.path().join("link"));
    let mut got = Vec::new();
    d.read_dir_each(&mut |e| {
        got.push((e.name, e.file_type));
        Ok(std::ops::ControlFlow::Continue(()))
    })
    .expect("stream");
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(got.len(), 3);
    assert_eq!(
        got,
        vec![
            (OsString::from("a"), FileType::File),
            (OsString::from("link"), FileType::Symlink),
            (OsString::from("sub"), FileType::Dir),
        ]
    );
}

#[test]
fn read_dir_each_stops_on_break_and_returns_a_visitor_error() {
    use std::ops::ControlFlow::{Break, Continue};
    let (tmp, d) = dir();
    for n in ["a", "b", "c"] {
        std::fs::write(tmp.path().join(n), b"x").unwrap();
    }
    let mut calls = 0;
    let r = d.read_dir_each(&mut |_| {
        calls += 1;
        Ok(Break(()))
    });
    assert!(r.is_ok());
    assert_eq!(calls, 1);
    let mut calls = 0;
    let err = d
        .read_dir_each(&mut |_| {
            calls += 1;
            if calls == 2 {
                return Err(flux_fs::FsError::new(Code::IoError, std::io::Error::other("x")));
            }
            Ok(Continue(()))
        })
        .unwrap_err();
    assert_eq!(err.code, Code::IoError);
    assert_eq!(calls, 2);
}

#[cfg(unix)]
#[test]
fn read_dir_ino_is_the_object_index_of_the_entry() {
    let (tmp, d) = dir();
    std::fs::write(tmp.path().join("a"), b"x").unwrap();
    std::fs::write(tmp.path().join("b"), b"x").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    let entries = d.read_dir().expect("list");
    assert!(entries.len() >= 3, "the loop must not be vacuous");
    for entry in entries {
        let Ok(m) = d.metadata(&entry.name) else { panic!("metadata of {:?}", entry.name) };
        let FileIdentity::Strong(ObjectId { index, .. }) = m.identity else {
            panic!("a strong identity for {:?}", entry.name)
        };
        assert_eq!(entry.ino, Some(index as u64), "{:?}", entry.name);
    }
}

/// Read `len` bytes at `offset` without moving a cursor, on either platform.
fn read_at(f: &std::fs::File, offset: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    #[cfg(unix)]
    std::os::unix::fs::FileExt::read_exact_at(f, &mut buf, offset).expect("read_at");
    #[cfg(windows)]
    {
        let n = std::os::windows::fs::FileExt::seek_read(f, &mut buf, offset).expect("seek_read");
        assert_eq!(n, len);
    }
    buf
}

/// Write `bytes` at `offset` without moving a cursor, on either platform.
fn write_at(f: &std::fs::File, offset: u64, bytes: &[u8]) {
    #[cfg(unix)]
    std::os::unix::fs::FileExt::write_all_at(f, bytes, offset).expect("write_at");
    #[cfg(windows)]
    {
        let n = std::os::windows::fs::FileExt::seek_write(f, bytes, offset).expect("seek_write");
        assert_eq!(n, bytes.len());
    }
}

#[test]
fn create_scratch_leaves_no_name_in_the_directory() {
    let (_tmp, d) = dir();
    std::fs::write(_tmp.path().join("keep"), b"x").unwrap();
    let before = listing(&d);
    let f = d.create_scratch().expect("create a scratch file");
    write_at(&f, 0, b"abc");
    assert_eq!(read_at(&f, 0, 3), b"abc");
    assert_eq!(listing(&d), before, "the scratch file has no name in the directory");
}

#[test]
fn two_scratch_files_are_independent() {
    let (_tmp, d) = dir();
    let a = d.create_scratch().expect("first");
    let b = d.create_scratch().expect("second");
    write_at(&a, 0, b"first!");
    write_at(&b, 0, b"second");
    assert_eq!(read_at(&a, 0, 6), b"first!");
    assert_eq!(read_at(&b, 0, 6), b"second");
}

#[cfg(target_os = "linux")]
#[test]
fn memory_backed_says_false_for_the_test_tempdir_and_true_for_dev_shm() {
    let (tmp, d) = dir();
    // The runner's temp dir may itself be tmpfs: ask `stat -f` (an oracle independent of the code under test).
    let fs_type = std::process::Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(tmp.path())
        .output()
        .expect("run stat -f");
    let fs_type = String::from_utf8_lossy(&fs_type.stdout).trim().to_string();
    let shm = Path::new("/dev/shm");
    let expected = Some(fs_type == "tmpfs" || fs_type == "ramfs");
    assert_eq!(d.memory_backed().expect("fstatfs"), expected, "the test tempdir");
    if shm.exists() {
        let s = StdFileSystem.destination_root(shm).expect("open /dev/shm");
        assert_eq!(s.memory_backed().expect("fstatfs"), Some(true), "/dev/shm is tmpfs");
    } else {
        eprintln!("skipping the /dev/shm half: it does not exist here");
    }
}
