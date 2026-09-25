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
            let code = match status {
                STATUS_NOT_A_DIRECTORY
                | STATUS_OBJECT_NAME_NOT_FOUND
                | STATUS_OBJECT_PATH_NOT_FOUND => Code::DestinationError,
                STATUS_ACCESS_DENIED => Code::PermissionDenied,
                _ => Code::IoError,
            };
            return Err(FsError::new(
                code,
                std::io::Error::other(format!("NtCreateFile: 0x{:08X}", status as u32)),
            ));
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
        crate::dir_windows::create_dir_at(&self.0, name)?;
        self.open_dir(name)
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

fn create_dir_at(p: &OwnedHandle, n: &OsStr) -> Result<()> {
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
            FILE_LIST_DIRECTORY | SYNCHRONIZE,
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
        return Err(FsError::new(
            code,
            std::io::Error::other(format!("NtCreateFile: 0x{:08X}", status as u32)),
        ));
    }

    // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `h` is a valid handle we own.
    // `DirHandle::create_dir` calls `self.open_dir(name)` right after this returns,
    // which is what runs the reparse-tag check on the thing just created -- so this
    // handle is closed immediately rather than returned.
    drop(unsafe { OwnedHandle::from_raw_handle(h as _) });
    Ok(())
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
            FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
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
        return Err(FsError::new(
            code,
            std::io::Error::other(format!("NtCreateFile: 0x{:08X}", status as u32)),
        ));
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
            STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => Code::DestinationError,
            STATUS_ACCESS_DENIED => Code::PermissionDenied,
            _ => Code::IoError,
        };
        return Err(FsError::new(
            code,
            std::io::Error::other(format!("NtCreateFile: 0x{:08X}", status as u32)),
        ));
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
            STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => Code::DestinationError,
            STATUS_ACCESS_DENIED => Code::PermissionDenied,
            _ => Code::IoError,
        };
        return Err(FsError::new(
            code,
            std::io::Error::other(format!("NtCreateFile: 0x{:08X}", status as u32)),
        ));
    }
    // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `raw` is a valid handle we own.
    let h = unsafe { OwnedHandle::from_raw_handle(raw as _) };

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
        return Err(FsError::new(
            Code::IoError,
            std::io::Error::other(format!("NtSetInformationFile: 0x{:08X}", status as u32)),
        ));
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
            STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => Code::DestinationError,
            STATUS_ACCESS_DENIED => Code::PermissionDenied,
            _ => Code::IoError,
        };
        return Err(FsError::new(
            code,
            std::io::Error::other(format!("NtCreateFile: 0x{:08X}", status as u32)),
        ));
    }
    // SAFETY: NtCreateFile returned STATUS_SUCCESS, so `raw` is a valid handle we own.
    let h = unsafe { OwnedHandle::from_raw_handle(raw as _) };

    let wide: Vec<u16> = t.encode_wide().collect();
    let name_bytes = wide.len() * 2;
    let total = size_of::<FILE_RENAME_INFORMATION>() + name_bytes;
    let mut buf = vec![0u8; total];

    // SAFETY: `buf` is at least size_of::<FILE_RENAME_INFORMATION>() bytes, and a
    // Vec<u8> allocation is at least pointer-aligned, which is this struct's
    // alignment (that of HANDLE). The name is written into the trailing space the
    // [u16; 1] placeholder stands for.
    unsafe {
        let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
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
    // SAFETY: `buf` holds a fully-initialized FILE_RENAME_INFORMATION plus its
    // trailing name, and `buf.len()` -- passed below, not
    // size_of::<FILE_RENAME_INFORMATION>() alone -- is exactly its total size. `h`
    // outlives the call.
    let status = unsafe {
        NtSetInformationFile(
            h.as_raw_handle() as _,
            &raw mut iosb,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            FileRenameInformation,
        )
    };
    if status != 0 {
        return Err(FsError::new(
            Code::IoError,
            std::io::Error::other(format!("NtSetInformationFile: 0x{:08X}", status as u32)),
        ));
    }
    drop(h);
    Ok(())
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
