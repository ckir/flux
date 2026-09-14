//! Filesystem probes FS-1 to FS-11 for the lock-protocol model.
//!
//! Design: docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md, Section 10.
//! Each probe confirms one assumption the model's filesystem (models/lockproto/FsModel.tla,
//! Section 5.1) makes about the platform it runs on. A failing probe is never weakened to
//! pass: first check the filesystem type it prints (a runner's overlay or network filesystem is
//! not the platform's native one), reproduce on a native local filesystem, and if it still
//! fails, the model's assumption for that platform is wrong and the model is fixed.
//!
//! The "OS-native lock" is the handle-scoped lock the model assumes: `flock` on Unix and
//! `LockFileEx` on Windows (never `fcntl` record locks, which a process loses when it closes any
//! handle to the file).

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::Path;

use tempfile::TempDir;

/// A fresh directory for one probe. Bind it first in each test so it drops after every handle
/// opened inside it: `TempDir` ignores a failed removal, and Windows cannot remove a file that is
/// still open without delete-sharing.
fn scratch() -> TempDir {
    let dir = tempfile::tempdir().expect("create a scratch directory");
    println!("scratch filesystem: {}", platform::fs_type(dir.path()));
    dir
}

fn create(path: &Path) -> File {
    OpenOptions::new().read(true).write(true).create_new(true).open(path).expect("create file")
}

#[cfg(unix)]
mod platform {
    use std::fs::{File, OpenOptions};
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use rustix::fs::{CWD, FlockOperation, RenameFlags};

    pub fn open(path: &Path) -> File {
        OpenOptions::new().read(true).write(true).open(path).expect("open file")
    }

    /// Take the OS-native lock without waiting; true if it was granted.
    pub fn try_lock(file: &File) -> bool {
        match rustix::fs::flock(file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => true,
            Err(e) if e == rustix::io::Errno::WOULDBLOCK => false,
            Err(e) => panic!("flock failed unexpectedly: {e}"),
        }
    }

    /// Rename that refuses to replace an existing target.
    pub fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
        rustix::fs::renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE).map_err(Into::into)
    }

    /// Device and inode, widened to the Windows identity type.
    pub fn identity_of_handle(file: &File) -> (u64, u128) {
        let meta = file.metadata().expect("fstat");
        (meta.dev(), u128::from(meta.ino()))
    }

    pub fn identity_of_name(path: &Path) -> (u64, u128) {
        let meta = std::fs::metadata(path).expect("stat");
        (meta.dev(), u128::from(meta.ino()))
    }

    #[cfg(target_os = "linux")]
    pub fn fs_type(path: &Path) -> String {
        match rustix::fs::statfs(path) {
            Ok(st) => format!("statfs f_type 0x{:x}", st.f_type),
            Err(_) => "unknown".to_string(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn fs_type(path: &Path) -> String {
        match rustix::fs::statfs(path) {
            Ok(st) => {
                let name: Vec<u8> =
                    st.f_fstypename.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
                String::from_utf8_lossy(&name).into_owned()
            }
            Err(_) => "unknown".to_string(),
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::fs::{File, OpenOptions};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        DeleteFileW, FILE_ID_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FileIdInfo, GetFileInformationByHandleEx, GetVolumeInformationW, GetVolumePathNameW,
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, MOVEFILE_REPLACE_EXISTING,
        MoveFileExW,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    fn wide(path: &Path) -> Vec<u16> {
        OsStr::new(path).encode_wide().chain(Some(0)).collect()
    }

    fn check(ok: i32) -> std::io::Result<()> {
        if ok != 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    }

    /// Open an existing file; `share_delete` chooses whether others may rename or delete it.
    pub fn open_shared(path: &Path, share_delete: bool) -> File {
        let mut share = FILE_SHARE_READ | FILE_SHARE_WRITE;
        if share_delete {
            share |= FILE_SHARE_DELETE;
        }
        OpenOptions::new().read(true).write(true).share_mode(share).open(path).expect("open file")
    }

    pub fn open(path: &Path) -> File {
        open_shared(path, true)
    }

    /// Take the OS-native lock without waiting; true if it was granted. Like `flock`, it covers
    /// the whole file: the full 64-bit byte range from offset 0.
    pub fn try_lock(file: &File) -> bool {
        // SAFETY: an all-zero OVERLAPPED is valid (offset 0, no event); the handle is open.
        let ok = unsafe {
            let mut overlapped: OVERLAPPED = std::mem::zeroed();
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
        ok != 0
    }

    /// Rename; `replace` chooses MOVEFILE_REPLACE_EXISTING.
    pub fn rename(from: &Path, to: &Path, replace: bool) -> std::io::Result<()> {
        let flags = if replace { MOVEFILE_REPLACE_EXISTING } else { 0 };
        // SAFETY: both paths are NUL-terminated UTF-16 buffers that outlive the call.
        check(unsafe { MoveFileExW(wide(from).as_ptr(), wide(to).as_ptr(), flags) })
    }

    pub fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
        rename(from, to, false)
    }

    pub fn delete(path: &Path) -> std::io::Result<()> {
        // SAFETY: the path is a NUL-terminated UTF-16 buffer that outlives the call.
        check(unsafe { DeleteFileW(wide(path).as_ptr()) })
    }

    /// Volume serial and the 128-bit file id. The 64-bit `nFileIndex` from
    /// `GetFileInformationByHandle` is not guaranteed unique on ReFS (Dev Drive), so read
    /// `FILE_ID_INFO` instead.
    pub fn identity_of_handle(file: &File) -> (u64, u128) {
        // SAFETY: an all-zero FILE_ID_INFO is valid; the handle is open and the buffer size
        // passed is the size of the struct written to.
        let info = unsafe {
            let mut info: FILE_ID_INFO = std::mem::zeroed();
            check(GetFileInformationByHandleEx(
                file.as_raw_handle() as HANDLE,
                FileIdInfo,
                (&raw mut info).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            ))
            .expect("GetFileInformationByHandleEx(FileIdInfo)");
            info
        };
        (info.VolumeSerialNumber, u128::from_le_bytes(info.FileId.Identifier))
    }

    pub fn identity_of_name(path: &Path) -> (u64, u128) {
        identity_of_handle(&open(path))
    }

    pub fn fs_type(path: &Path) -> String {
        let mut root = [0u16; 261];
        let mut name = [0u16; 64];
        // SAFETY: the buffers are writable and their lengths are passed; null pointers are
        // allowed for the outputs this call does not need.
        let ok = unsafe {
            GetVolumePathNameW(wide(path).as_ptr(), root.as_mut_ptr(), root.len() as u32) != 0
                && GetVolumeInformationW(
                    root.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    name.as_mut_ptr(),
                    name.len() as u32,
                ) != 0
        };
        if !ok {
            return "unknown".to_string();
        }
        let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
        String::from_utf16_lossy(&name[..end])
    }
}

use platform::{identity_of_handle, identity_of_name, open, rename_no_replace, try_lock};

#[test]
fn fs1_second_exclusive_create_fails() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let _first = create(&path);
    let second = OpenOptions::new().write(true).create_new(true).open(&path);
    assert_eq!(second.err().map(|e| e.kind()), Some(ErrorKind::AlreadyExists));
}

#[test]
fn fs2_no_replace_rename_fails_when_target_exists() {
    let dir = scratch();
    let (from, to) = (dir.path().join("from"), dir.path().join("to"));
    drop(create(&from));
    drop(create(&to));
    let refused = rename_no_replace(&from, &to).err().map(|e| e.kind());
    assert_eq!(refused, Some(ErrorKind::AlreadyExists), "no-replace rename onto an existing name");
    assert!(from.exists() && to.exists());
    // Control: the same call onto a free name moves `from`, so the refusal above came from the
    // existing target, not from an unsupported flag or swapped arguments.
    let free = dir.path().join("free");
    rename_no_replace(&from, &free).expect("no-replace rename onto a free name");
    assert!(!from.exists() && free.exists());
}

#[test]
fn fs3_lock_fails_while_another_handle_holds_it() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let holder = create(&path);
    assert!(try_lock(&holder), "first lock must be granted");
    let other = open(&path);
    assert!(!try_lock(&other), "second handle got a lock another handle holds");
}

#[cfg(unix)]
#[test]
fn fs4_rename_of_open_locked_file_succeeds() {
    let dir = scratch();
    let (path, moved) = (dir.path().join("a.flux-lock"), dir.path().join("a.flux-lock.moved"));
    let holder = create(&path);
    assert!(try_lock(&holder));
    assert!(!try_lock(&open(&path)), "precondition: the lock is held");
    std::fs::rename(&path, &moved).expect("rename an open, locked file");
    assert!(!path.exists() && moved.exists());
}

#[cfg(unix)]
#[test]
fn fs5_unlink_of_open_file_succeeds_and_handle_still_writes() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;

    let mut handle = create(&path);
    std::fs::remove_file(&path).expect("unlink an open file");
    assert!(!path.exists());
    handle.write_all(b"record").expect("write to the unnamed file");
    handle.seek(SeekFrom::Start(0)).unwrap();
    let mut back = String::new();
    handle.read_to_string(&mut back).unwrap();
    assert_eq!(back, "record");
    assert_eq!(handle.metadata().unwrap().nlink(), 0, "the object has no name left");
}

#[cfg(windows)]
#[test]
fn fs6_file_open_without_delete_sharing_cannot_be_renamed_deleted_or_replaced() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let other = dir.path().join("other");
    drop(create(&path));
    drop(create(&other));
    let _held = platform::open_shared(&path, false);
    let moved = dir.path().join("moved");
    assert!(platform::rename(&path, &moved, false).is_err(), "rename succeeded");
    assert!(platform::delete(&path).is_err(), "delete succeeded");
    assert!(std::fs::rename(&other, &path).is_err(), "replacing rename succeeded");
    assert!(path.exists() && other.exists());
}

#[cfg(windows)]
#[test]
fn fs7_file_open_with_delete_sharing_can_be_renamed_deleted_and_replaced() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let moved = dir.path().join("moved");
    drop(create(&path));
    let held = platform::open_shared(&path, true);
    platform::rename(&path, &moved, false).expect("rename a file open with delete-sharing");
    assert!(!path.exists() && moved.exists());

    platform::delete(&moved).expect("delete a file open with delete-sharing");
    let name_gone = OpenOptions::new().write(true).create_new(true).open(&moved).is_ok();
    println!(
        "delete of an open file: {}",
        if name_gone {
            "name removed at once (POSIX semantics)"
        } else {
            "name pending until close"
        }
    );
    drop(held);

    let target = dir.path().join("target");
    let source = dir.path().join("source");
    drop(create(&target));
    drop(create(&source));
    let source_id = identity_of_name(&source);
    let _held_target = platform::open_shared(&target, true);
    let legacy = platform::rename(&source, &target, true);
    println!("MoveFileExW(MOVEFILE_REPLACE_EXISTING) onto an open, delete-shared file: {legacy:?}");
    // The model's replacing rename is the POSIX-semantics rename that std::fs::rename uses; the
    // legacy MoveFileExW replace above refuses any open target (measured on Windows 11 NTFS and
    // ReFS).
    std::fs::rename(&source, &target).expect("replacing rename onto a shared-delete file");
    assert!(!source.exists());
    assert_eq!(identity_of_name(&target), source_id, "the name does not hold the moved file");
}

#[test]
fn fs8_name_replaced_by_new_file_reports_new_identity() {
    let dir = scratch();
    let (path, fresh) = (dir.path().join("a.flux-lock"), dir.path().join("fresh"));
    let old = create(&path);
    let old_id = identity_of_handle(&old);
    drop(create(&fresh));
    // `fresh` is created while the old file is still named and open, so the two are distinct
    // objects; the probe checks that the name reports the new one after the replace.
    std::fs::rename(&fresh, &path).expect("replace the name with a new file");
    assert_ne!(identity_of_name(&path), old_id, "a replaced name reported the old identity");
}

#[test]
fn fs9_closing_a_handle_releases_its_lock() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let holder = create(&path);
    assert!(try_lock(&holder));
    assert!(!try_lock(&open(&path)), "precondition: the lock is held");
    drop(holder);
    assert!(try_lock(&open(&path)), "the lock survived its handle's close");
}

#[test]
fn fs10_listing_returns_entries_present_throughout() {
    let dir = scratch();
    for name in ["a.flux-lock", "b.flux-lock"] {
        drop(create(&dir.path().join(name)));
    }
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["a.flux-lock", "b.flux-lock"]);
}

#[test]
fn fs11_lock_is_scoped_to_its_handle() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let holder = create(&path);
    assert!(try_lock(&holder));
    drop(open(&path)); // open and close a second handle to the same file
    assert!(!try_lock(&open(&path)), "closing another handle released the lock");
    drop(holder);
}
