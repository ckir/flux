//! The Windows `DirHandle`: `NtCreateFile` with `OBJECT_ATTRIBUTES.RootDirectory`.
//!
//! This arm is NOT a transliteration of the POSIX one, and the difference is the
//! thing most likely to be "simplified" back into a defect.
//!
//! MEASURED: `NtCreateFile` with `FILE_OPEN_REPARSE_POINT` SUCCEEDS on a junction
//! and returns a handle TO the junction. There is no Win32 or NT flag that makes
//! the open FAIL on a reparse point, so this arm must open and then INSPECT.
//! Dropping the flag is not an alternative -- without it `NtCreateFile` FOLLOWS
//! the junction, which is the traversal §149.7 forbids.

use flux_fs::{Code, DirHandle, FsError, Metadata, Result, check_component};
use std::ffi::OsStr;
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_INFORMATION, FILE_NON_DIRECTORY_FILE,
    FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION, FileDispositionInformation,
    FileRenameInformation, NtCreateFile, NtSetInformationFile,
};
use windows_sys::Win32::Foundation::{HANDLE, UNICODE_STRING};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_LIST_DIRECTORY: u32 = 0x1;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;

const STATUS_NOT_A_DIRECTORY: i32 = 0xC000_0103u32 as i32;
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034u32 as i32;
const STATUS_OBJECT_PATH_NOT_FOUND: i32 = 0xC000_003Au32 as i32;
const STATUS_ACCESS_DENIED: i32 = 0xC000_0022u32 as i32;
const STATUS_OBJECT_NAME_COLLISION: i32 = 0xC000_0035u32 as i32;
const STATUS_FILE_IS_A_DIRECTORY: i32 = 0xC000_00BAu32 as i32;

/// An `NTSTATUS` as a `std::io::Error` that KEEPS its kind.
///
/// `std::io::Error::other` hardcodes `ErrorKind::Other`, which silently destroyed
/// the one distinction a caller needs most: MEASURED, a `create_new` collision came
/// back `kind=Other` through this arm while the path-based arm reported
/// `kind=AlreadyExists` for the very same event. `create_new` and
/// `rename_no_replace` both exist to report occupancy, so an occupied name must
/// arrive as `AlreadyExists` on every arm or the engine cannot branch on it.
fn nt_io_error(what: &str, status: i32) -> std::io::Error {
    let kind = match status {
        // Both mean THE NAME IS TAKEN. The kernel distinguishes them -- COLLISION
        // when a file occupies the name, FILE_IS_A_DIRECTORY when a directory does --
        // and to a caller of `create_new` that is one situation. MEASURED: POSIX
        // answers AlreadyExists for both, while this arm reported the directory case
        // as ErrorKind::Other, because only the first status was mapped. Round 1
        // fixed the collision and missed its twin.
        //
        // Mapping FILE_IS_A_DIRECTORY globally is safe HERE rather than merely
        // convenient: it can only be returned to a call that asked for a
        // non-directory, and `create_new_at` is the one call in this file that passes
        // FILE_NON_DIRECTORY_FILE. If another call ever does, this mapping needs
        // revisiting with it.
        STATUS_OBJECT_NAME_COLLISION | STATUS_FILE_IS_A_DIRECTORY => {
            std::io::ErrorKind::AlreadyExists
        }
        STATUS_ACCESS_DENIED => std::io::ErrorKind::PermissionDenied,
        STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => std::io::ErrorKind::NotFound,
        _ => std::io::ErrorKind::Other,
    };
    std::io::Error::new(kind, format!("{what}: 0x{:08X}", status as u32))
}

/// `IsReparseTagNameSurrogate`. A surrogate stands in for another NAME -- a symlink
/// or a junction -- and is what §149.7 refuses. Everything else that merely carries
/// `FILE_ATTRIBUTE_REPARSE_POINT` must be TRAVERSED.
///
/// MEASURED on a real machine: 14 reparse points exist under `C:\` and the user
/// profile, and three of them -- OneDrive, Dropbox, MagentaCLOUD -- are cloud-sync
/// placeholders that are neither junctions nor symlinks. OneDrive's tag is
/// 0x9000701A with this bit CLEAR; a junction's is 0xA0000003 with it SET.
/// Rejecting on the attribute bit instead would refuse to write into the user's
/// OneDrive folder.
const fn is_name_surrogate(tag: u32) -> bool {
    tag & 0x2000_0000 != 0
}

/// `Debug` is required, not decorative: the tests call `.unwrap_err()` on a
/// `Result<StdDir, _>`, which needs `StdDir: Debug` to compile.
#[derive(Debug)]
pub struct StdDir(OwnedHandle);

impl StdDir {
    pub(crate) fn from_handle(h: OwnedHandle) -> Self {
        Self(h)
    }
}

impl DirHandle for StdDir {
    type Writer = crate::StdFile;

    fn open_dir(&self, name: &OsStr) -> Result<Self> {
        check_component(name)?;

        let mut wide: Vec<u16> = name.encode_wide().collect();
        let bytes = (wide.len() * 2) as u16;
        let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
        let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
        oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
        oa.RootDirectory = self.0.as_raw_handle() as HANDLE;
        oa.ObjectName = &raw const us;

        let mut h: HANDLE = std::ptr::null_mut();
        let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };

        // SAFETY: every pointer is to a live local that outlives the call, and
        // `wide` outlives `us` which borrows it.
        //
        // ShareAccess is NOT a free choice: 0 would take every directory the
        // writer walks EXCLUSIVELY, so any other process merely reading the tree
        // would fail the copy.
        let status = unsafe {
            NtCreateFile(
                &raw mut h,
                FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE,
                &raw const oa,
                &raw mut iosb,
                std::ptr::null(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_OPEN,
                FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                std::ptr::null(),
                0,
            )
        };

        if status != 0 {
            // STATUS_NOT_A_DIRECTORY CONFLATES two things, exactly as POSIX's
            // ENOTDIR does: a plain file, and a symlink whose target is a file.
            // FILE_DIRECTORY_FILE rejects the second before the reparse-tag check
            // further down can ever see it, so without this the arms disagree on
            // the same object -- POSIX answers SafetyRejected (its ENOTDIR path
            // runs statat to separate them) while this one answered
            // DestinationError. Inspecting here is the same second look POSIX
            // already takes, and it is equally racy for the same reason: the name
            // may be gone by now, in which case there is nothing to refuse and the
            // original status stands.
            if status == STATUS_NOT_A_DIRECTORY
                && let Some(tag) = surrogate_tag_at(&self.0, name)
            {
                return Err(FsError::new(
                    Code::SafetyRejected,
                    std::io::Error::other(format!("name-surrogate reparse point, tag 0x{tag:08X}")),
                ));
            }
            let code = match status {
                STATUS_NOT_A_DIRECTORY
                | STATUS_OBJECT_NAME_NOT_FOUND
                | STATUS_OBJECT_PATH_NOT_FOUND => Code::DestinationError,
                STATUS_ACCESS_DENIED => Code::PermissionDenied,
                _ => Code::IoError,
            };
            return Err(FsError::new(code, nt_io_error("NtCreateFile", status)));
        }

        // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `h` is a valid handle we own.
        let opened = unsafe { OwnedHandle::from_raw_handle(h as _) };

        // THE STEP THAT HAS NO POSIX COUNTERPART. The open succeeded even if this
        // is a junction, so ask what it is. Dropping `opened` on the reject path
        // closes the handle, which matters: a refused directory must not leave a
        // sharing constraint behind for a later operation on the same tree.
        let tag = crate::dir_windows::reparse_tag_of(&opened)?;
        if let Some(tag) = tag
            && is_name_surrogate(tag)
        {
            return Err(FsError::new(
                Code::SafetyRejected,
                std::io::Error::other(format!("name-surrogate reparse point, tag 0x{tag:08X}")),
            ));
        }

        Ok(Self(opened))
    }

    fn create_dir(&self, name: &OsStr) -> Result<Self> {
        check_component(name)?;
        Ok(Self(crate::dir_windows::create_dir_at(&self.0, name)?))
    }

    fn create_new(&self, name: &OsStr) -> Result<Self::Writer> {
        check_component(name)?;
        crate::dir_windows::create_new_at(&self.0, name)
    }

    fn metadata(&self, name: &OsStr) -> Result<Metadata> {
        check_component(name)?;
        crate::dir_windows::metadata_at(&self.0, name)
    }

    fn remove_file(&self, name: &OsStr) -> Result<()> {
        check_component(name)?;
        crate::dir_windows::remove_file_at(&self.0, name)
    }

    fn rename_no_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        crate::dir_windows::rename_at(&self.0, from, &other.0, to, false)
    }

    fn rename_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        crate::dir_windows::rename_at(&self.0, from, &other.0, to, true)
    }
}

fn create_dir_at(p: &OwnedHandle, n: &OsStr) -> Result<OwnedHandle> {
    let mut wide: Vec<u16> = n.encode_wide().collect();
    let bytes = (wide.len() * 2) as u16;
    let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
    let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
    oa.RootDirectory = p.as_raw_handle() as HANDLE;
    oa.ObjectName = &raw const us;

    let mut h: HANDLE = std::ptr::null_mut();
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };

    // SAFETY: every pointer is to a live local that outlives the call, and `wide`
    // outlives `us` which borrows it. ShareAccess is not a free choice, for the
    // reason `open_dir` records: 0 would lock the object against every other
    // process.
    let status = unsafe {
        NtCreateFile(
            &raw mut h,
            // Matches `open_dir`'s mask exactly, because this handle is now
            // RETURNED to the caller rather than closed, and must be as usable as
            // one `open_dir` hands back.
            FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE,
            &raw const oa,
            &raw mut iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
            std::ptr::null(),
            0,
        )
    };

    if status != 0 {
        let code = match status {
            STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => Code::DestinationError,
            STATUS_ACCESS_DENIED => Code::PermissionDenied,
            _ => Code::IoError,
        };
        return Err(FsError::new(code, nt_io_error("NtCreateFile", status)));
    }

    // RETURN the handle rather than closing it and re-opening the NAME.
    //
    // It used to drop this and let `DirHandle::create_dir` call `open_dir(name)`, so
    // that the reparse-tag check ran on the thing just created. That was
    // check-then-act on a name -- the exact pattern this cut exists to remove --
    // and it bought nothing: `FILE_CREATE` with no EA buffer cannot produce a
    // reparse point, so the tag check had no question to answer. What the re-open
    // DID add was a window in which another process could rename the fresh directory
    // away and leave a different one in its place, and `open_dir` would have accepted
    // that one and handed it back as the directory we had just made.
    //
    // A surrogate substituted into that window was never the danger, since `open_dir`
    // refuses those; a PLAIN directory was, and nothing refuses that. Returning the
    // handle closes the window rather than narrowing it, and costs one syscall less.
    // The POSIX arm cannot do the same -- `mkdirat` returns no descriptor, so it must
    // re-open by name and keeps the race by necessity.
    //
    // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `h` is a valid handle we own.
    Ok(unsafe { OwnedHandle::from_raw_handle(h as _) })
}

fn create_new_at(p: &OwnedHandle, n: &OsStr) -> Result<crate::StdFile> {
    let mut wide: Vec<u16> = n.encode_wide().collect();
    let bytes = (wide.len() * 2) as u16;
    let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
    let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
    oa.RootDirectory = p.as_raw_handle() as HANDLE;
    oa.ObjectName = &raw const us;

    let mut h: HANDLE = std::ptr::null_mut();
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };

    // SAFETY: every pointer is to a live local that outlives the call, and `wide`
    // outlives `us` which borrows it. ShareAccess matches `open_dir`'s reasoning.
    let status = unsafe {
        NtCreateFile(
            &raw mut h,
            FILE_GENERIC_WRITE | SYNCHRONIZE,
            &raw const oa,
            &raw mut iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_CREATE,
            // FILE_OPEN_REPARSE_POINT even though this CREATES. Without it the
            // existence check follows a surrogate sitting on the name, and a dangling
            // one would have the kernel create the file at the LINK'S TARGET instead
            // of refusing -- a write outside the destination tree, which is the whole
            // thing this cut prevents. With it, the link itself is what occupies the
            // name, so FILE_CREATE reports a collision, matching POSIX, where
            // openat(O_CREAT | O_EXCL) refuses a symlinked final component with
            // EEXIST.
            //
            // Measured with a junction, dangling and not, since a file symlink needs
            // a privilege this account lacks: both already answered AlreadyExists, so
            // this closes the case that could NOT be measured rather than one that was
            // observed failing. Every other NtCreateFile in this file already passes
            // the flag; this was the one that did not.
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };

    if status != 0 {
        let code = match status {
            STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => Code::DestinationError,
            STATUS_ACCESS_DENIED => Code::PermissionDenied,
            _ => Code::IoError,
        };
        return Err(FsError::new(code, nt_io_error("NtCreateFile", status)));
    }

    // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `h` is a valid handle we own.
    let opened = unsafe { OwnedHandle::from_raw_handle(h as _) };
    Ok(crate::std_fs::std_file_from(std::fs::File::from(opened)))
}

fn metadata_at(p: &OwnedHandle, n: &OsStr) -> Result<Metadata> {
    let mut wide: Vec<u16> = n.encode_wide().collect();
    let bytes = (wide.len() * 2) as u16;
    let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
    let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
    oa.RootDirectory = p.as_raw_handle() as HANDLE;
    oa.ObjectName = &raw const us;

    let mut h: HANDLE = std::ptr::null_mut();
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };

    // SAFETY: every pointer is to a live local that outlives the call, and `wide`
    // outlives `us` which borrows it. ShareAccess matches `open_dir`'s reasoning.
    let status = unsafe {
        NtCreateFile(
            &raw mut h,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &raw const oa,
            &raw mut iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };

    if status != 0 {
        let code = match status {
            // A missing TARGET is not a broken DESTINATION. MEASURED on both arms:
            // POSIX answers IoError/NotFound here while this arm answered
            // DestinationError, because open_dir's mapping was copied in. open_dir
            // KEEPS DestinationError -- a missing component of the destination path
            // IS a destination problem, and both arms' tests pin that -- but a
            // missing file to remove, stat or rename is an ordinary not-found, and
            // the engine must not read it as a broken tree.
            STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => Code::IoError,
            STATUS_ACCESS_DENIED => Code::PermissionDenied,
            _ => Code::IoError,
        };
        return Err(FsError::new(code, nt_io_error("NtCreateFile", status)));
    }

    // Once the handle is open the object is already pinned -- the open is the only
    // part of this that had to be handle-relative. Build the `Metadata` exactly as
    // the path-based Windows `metadata` does at `std_fs.rs:221`, reusing its three
    // helpers rather than re-deriving the mapping, which is what makes the two
    // Windows arms agree by construction instead of by inspection.
    //
    // SAFETY: NtCreateFile returned this handle and nothing else owns it.
    let f = File::from(unsafe { OwnedHandle::from_raw_handle(h as _) });
    let m = f.metadata().map_err(FsError::from_io)?;
    Ok(Metadata {
        len: m.len(),
        file_type: crate::std_fs::type_of(&m),
        permissions: Some(crate::std_fs::perms_of(&m)),
        modified: m.modified().ok(),
        identity: crate::std_fs::identity_of_handle(&f),
    })
}

fn remove_file_at(p: &OwnedHandle, n: &OsStr) -> Result<()> {
    // Open the name with DELETE | SYNCHRONIZE and FILE_OPEN_REPARSE_POINT, so a
    // link is removed rather than followed -- the same reasoning `open_dir`
    // records for opening the object itself rather than what it points to.
    let mut wide: Vec<u16> = n.encode_wide().collect();
    let bytes = (wide.len() * 2) as u16;
    let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
    let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
    oa.RootDirectory = p.as_raw_handle() as HANDLE;
    oa.ObjectName = &raw const us;

    let mut raw: HANDLE = std::ptr::null_mut();
    let mut open_iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };

    // SAFETY: every pointer is to a live local that outlives the call, and `wide`
    // outlives `us` which borrows it. ShareAccess matches `open_dir`'s reasoning.
    let status = unsafe {
        NtCreateFile(
            &raw mut raw,
            // FILE_READ_ATTRIBUTES is not decoration: the directory guard below
            // queries this handle, and DELETE alone does not grant that query.
            DELETE | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            &raw const oa,
            &raw mut open_iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if status != 0 {
        let code = match status {
            // A missing TARGET is not a broken DESTINATION. MEASURED on both arms:
            // POSIX answers IoError/NotFound here while this arm answered
            // DestinationError, because open_dir's mapping was copied in. open_dir
            // KEEPS DestinationError -- a missing component of the destination path
            // IS a destination problem, and both arms' tests pin that -- but a
            // missing file to remove, stat or rename is an ordinary not-found, and
            // the engine must not read it as a broken tree.
            STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => Code::IoError,
            STATUS_ACCESS_DENIED => Code::PermissionDenied,
            _ => Code::IoError,
        };
        return Err(FsError::new(code, nt_io_error("NtCreateFile", status)));
    }
    // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `raw` is a valid handle we own.
    let h = unsafe { OwnedHandle::from_raw_handle(raw as _) };

    // REMOVE_FILE MUST NOT REMOVE A DIRECTORY, and without this it did.
    //
    // MEASURED, both arms, before this guard: POSIX `unlinkat(AtFlags::empty())`
    // refuses an ordinary directory with EISDIR (IoError / IsADirectory), while this
    // arm opened it and FileDispositionInformation DELETED it, returning Ok. The
    // open omits FILE_NON_DIRECTORY_FILE on purpose -- a junction is a directory and
    // must stay removable -- so the constraint the open cannot express is applied
    // here instead.
    //
    // A reparse-point directory (a junction, a directory symlink) is still removed,
    // which MATCHES POSIX: `unlinkat` unlinks a symlink whatever it points at. Only
    // a PLAIN directory is refused.
    if !may_remove(&h) {
        return Err(FsError::new(
            Code::IoError,
            std::io::Error::new(
                std::io::ErrorKind::IsADirectory,
                "remove_file refuses a directory, or could not determine that it is not one",
            ),
        ));
    }

    let mut info = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a live FILE_DISPOSITION_INFORMATION of the size given, and
    // the handle outlives the call.
    let status = unsafe {
        NtSetInformationFile(
            h.as_raw_handle() as _,
            &raw mut iosb,
            (&raw mut info).cast(),
            size_of::<FILE_DISPOSITION_INFORMATION>() as u32,
            FileDispositionInformation,
        )
    };
    if status != 0 {
        return Err(FsError::new(Code::IoError, nt_io_error("NtSetInformationFile", status)));
    }
    drop(h);
    Ok(())
}

fn rename_at(fd: &OwnedHandle, f: &OsStr, td: &OwnedHandle, t: &OsStr, r: bool) -> Result<()> {
    // Open `from` in `from_dir` with DELETE | SYNCHRONIZE and
    // FILE_OPEN_REPARSE_POINT, so a link is renamed rather than followed.
    let mut wide_from: Vec<u16> = f.encode_wide().collect();
    let bytes = (wide_from.len() * 2) as u16;
    let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide_from.as_mut_ptr() };
    let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
    oa.RootDirectory = fd.as_raw_handle() as HANDLE;
    oa.ObjectName = &raw const us;

    let mut raw: HANDLE = std::ptr::null_mut();
    let mut open_iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };

    // SAFETY: every pointer is to a live local that outlives the call, and
    // `wide_from` outlives `us` which borrows it. ShareAccess matches `open_dir`'s
    // reasoning.
    let status = unsafe {
        NtCreateFile(
            &raw mut raw,
            DELETE | SYNCHRONIZE,
            &raw const oa,
            &raw mut open_iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if status != 0 {
        let code = match status {
            // A missing TARGET is not a broken DESTINATION. MEASURED on both arms:
            // POSIX answers IoError/NotFound here while this arm answered
            // DestinationError, because open_dir's mapping was copied in. open_dir
            // KEEPS DestinationError -- a missing component of the destination path
            // IS a destination problem, and both arms' tests pin that -- but a
            // missing file to remove, stat or rename is an ordinary not-found, and
            // the engine must not read it as a broken tree.
            STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => Code::IoError,
            STATUS_ACCESS_DENIED => Code::PermissionDenied,
            _ => Code::IoError,
        };
        return Err(FsError::new(code, nt_io_error("NtCreateFile", status)));
    }
    // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `raw` is a valid handle we own.
    let h = unsafe { OwnedHandle::from_raw_handle(raw as _) };

    let wide: Vec<u16> = t.encode_wide().collect();
    let name_bytes = wide.len() * 2;
    let total = size_of::<FILE_RENAME_INFORMATION>() + name_bytes;

    // A `u64` buffer, NOT a `Vec<u8>`, and the difference is soundness rather than
    // taste. An earlier version allocated `vec![0u8; total]` under a SAFETY comment
    // asserting "a Vec<u8> allocation is at least pointer-aligned". Rust guarantees
    // no such thing: `Vec<u8>` carries `align_of::<u8>()`, which is 1. MEASURED, the
    // system allocator happens to return 16-byte-aligned blocks, so the write worked
    // -- but a SAFETY comment that rests on what an allocator HAPPENS to do is not a
    // proof, and both the field writes and the `copy_nonoverlapping` of a `[u16]`
    // below carry their own alignment requirements. Allocating `u64` satisfies all of
    // them by construction. The const assertion below is what keeps that true if the
    // struct ever changes.
    const {
        assert!(align_of::<FILE_RENAME_INFORMATION>() <= align_of::<u64>());
    }
    let mut buf: Vec<u64> = vec![0u64; total.div_ceil(size_of::<u64>())];
    let base: *mut u8 = buf.as_mut_ptr().cast();

    // SAFETY: `base` addresses at least `total` zeroed bytes, aligned to 8, which the
    // assertion above pins as no weaker than this struct needs. The name is written
    // into the trailing space the `[u16; 1]` placeholder stands for.
    unsafe {
        let info = base.cast::<FILE_RENAME_INFORMATION>();
        (&raw mut (*info).Anonymous.ReplaceIfExists).write(r);
        (&raw mut (*info).RootDirectory).write(td.as_raw_handle() as HANDLE);
        (&raw mut (*info).FileNameLength).write(name_bytes as u32);
        std::ptr::copy_nonoverlapping(
            wide.as_ptr(),
            (&raw mut (*info).FileName).cast::<u16>(),
            wide.len(),
        );
    }

    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `base` holds a fully-initialized FILE_RENAME_INFORMATION plus its
    // trailing name, and `total` -- passed below, not
    // size_of::<FILE_RENAME_INFORMATION>() alone -- is exactly its size. It is NOT
    // `buf.len()`: `buf` counts u64 WORDS and is rounded up, so its length is both
    // the wrong unit and, for a name whose bytes do not fill the last word, too long.
    // `buf` outlives `base`, and `h` outlives the call.
    let status = unsafe {
        NtSetInformationFile(
            h.as_raw_handle() as _,
            &raw mut iosb,
            base.cast(),
            total as u32,
            FileRenameInformation,
        )
    };
    if status != 0 {
        return Err(FsError::new(Code::IoError, nt_io_error("NtSetInformationFile", status)));
    }
    drop(h);
    Ok(())
}

/// The name-surrogate tag of a child that is NOT a directory, or `None`.
///
/// Opens with no `FILE_DIRECTORY_FILE` and no `FILE_NON_DIRECTORY_FILE`, so it
/// accepts whatever the name is, plus `FILE_OPEN_REPARSE_POINT` so it sees the LINK
/// and never its target. Every failure answers `None`: this runs only on a path that
/// is already returning an error, and its single job is to decide whether that error
/// should be reclassified as a refusal. It must never invent a new failure of its own.
fn surrogate_tag_at(parent: &OwnedHandle, name: &OsStr) -> Option<u32> {
    let mut wide: Vec<u16> = name.encode_wide().collect();
    let bytes = (wide.len() * 2) as u16;
    let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
    let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
    oa.RootDirectory = parent.as_raw_handle() as HANDLE;
    oa.ObjectName = &raw const us;

    let mut h: HANDLE = std::ptr::null_mut();
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };

    // SAFETY: every pointer is to a live local that outlives the call, and `wide`
    // outlives `us` which borrows it. ShareAccess matches `open_dir`'s reasoning.
    let status = unsafe {
        NtCreateFile(
            &raw mut h,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &raw const oa,
            &raw mut iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `h` is a valid handle we own.
    let opened = unsafe { OwnedHandle::from_raw_handle(h as _) };
    match reparse_tag_of(&opened) {
        Ok(Some(tag)) if is_name_surrogate(tag) => Some(tag),
        _ => None,
    }
}

/// Read a handle's reparse tag, or `None` when it is not a reparse point.
#[cfg(windows)]
pub(crate) fn reparse_tag_of(
    h: &std::os::windows::io::OwnedHandle,
) -> flux_fs::Result<Option<u32>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Wdk::Storage::FileSystem::{
        FileAttributeTagInformation, NtQueryInformationFile,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    #[repr(C)]
    struct FileAttributeTagInfo {
        file_attributes: u32,
        reparse_tag: u32,
    }

    let mut info = FileAttributeTagInfo { file_attributes: 0, reparse_tag: 0 };
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a live, correctly sized FILE_ATTRIBUTE_TAG_INFORMATION and
    // the handle outlives the call.
    let status = unsafe {
        NtQueryInformationFile(
            h.as_raw_handle() as _,
            &raw mut iosb,
            (&raw mut info).cast(),
            size_of::<FileAttributeTagInfo>() as u32,
            FileAttributeTagInformation,
        )
    };
    if status != 0 {
        return Err(flux_fs::FsError::new(
            flux_fs::Code::IoError,
            std::io::Error::other(format!("NtQueryInformationFile: 0x{:08X}", status as u32)),
        ));
    }
    if info.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(None);
    }
    Ok(Some(info.reparse_tag))
}

/// May `remove_file` remove what this handle addresses?
///
/// POSIX is the specification: `unlinkat(AtFlags::empty())` refuses a DIRECTORY with
/// EISDIR, and unlinks a SYMLINK whatever it points at. So a name surrogate -- a
/// junction or a directory symlink -- is removable, and a real directory is not.
///
/// Two earlier versions of this were both wrong, in opposite directions, and both
/// were caught by review of the fix rather than of the original code.
///
/// It first returned a bare `bool` that answered "not a directory" when the query
/// FAILED, so the guard FAILED OPEN: an unanswered query let the delete proceed,
/// which is the defect the guard exists to stop.
///
/// Fixing that by refusing whenever the answer was unknown broke the ordinary case
/// instead. MEASURED on a real FAT32 volume: `FileAttributeTagInformation` does not
/// answer there, so `remove_file` on an ordinary FILE came back `IsADirectory` and
/// no file on that volume could be deleted at all.
///
/// So the question is asked twice, narrowing what each answer has to carry.
/// `FileBasicInformation` is the most basic class there is and reports
/// `FILE_ATTRIBUTE_DIRECTORY` without any tag; without a tag a junction cannot be
/// told from a plain directory, so that fallback refuses every directory. Nothing is
/// lost by that: a volume that cannot answer the tag query has no reparse points to
/// distinguish. Only if BOTH queries fail does this refuse outright, which keeps the
/// fail-closed property for the case that actually motivated it.
#[cfg(windows)]
fn may_remove(h: &std::os::windows::io::OwnedHandle) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_BASIC_INFORMATION, FileAttributeTagInformation, FileBasicInformation,
        NtQueryInformationFile,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    #[repr(C)]
    struct FileAttributeTagInfo {
        file_attributes: u32,
        reparse_tag: u32,
    }

    let mut tag_info = FileAttributeTagInfo { file_attributes: 0, reparse_tag: 0 };
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `tag_info` is a live, correctly sized FILE_ATTRIBUTE_TAG_INFORMATION
    // and the handle outlives the call.
    let status = unsafe {
        NtQueryInformationFile(
            h.as_raw_handle() as _,
            &raw mut iosb,
            (&raw mut tag_info).cast(),
            size_of::<FileAttributeTagInfo>() as u32,
            FileAttributeTagInformation,
        )
    };
    if status == 0 {
        // A directory is removable ONLY as a name surrogate. Reading the TAG rather
        // than the reparse BIT is the whole point, and the bit alone was a defect:
        // a directory carrying a NON-surrogate reparse point -- a OneDrive
        // placeholder, tag 0x9000701A, measured on this machine -- is a real
        // directory with cloud metadata, not a link, and deleting it is exactly the
        // structural damage this guard exists to stop. It is the same rule `open_dir`
        // applies when it decides what to traverse.
        if tag_info.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return is_name_surrogate(tag_info.reparse_tag);
        }
        return true;
    }

    let mut basic: FILE_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let mut iosb2: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `basic` is a live, correctly sized FILE_BASIC_INFORMATION and the
    // handle outlives the call.
    let status = unsafe {
        NtQueryInformationFile(
            h.as_raw_handle() as _,
            &raw mut iosb2,
            (&raw mut basic).cast(),
            size_of::<FILE_BASIC_INFORMATION>() as u32,
            FileBasicInformation,
        )
    };
    if status != 0 {
        // Neither query answered. Refuse: see the fail-closed reasoning above.
        return false;
    }
    basic.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
}
