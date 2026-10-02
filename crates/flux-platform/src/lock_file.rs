//! The lock file on a real filesystem (cut 7a Part 1).
//!
//! The OS-native lock is `flock` on Unix and `LockFileEx` on Windows, never `fcntl` record locks, which a process
//! loses when it closes ANY descriptor of the file (`tests/fs_semantics.rs:10-11`).

use flux_fs::{Code, FileIdentity, FsError, LockFile, Result};
use std::fs::File;

/// A lock file open for reading and writing. Dropping it closes the handle, which releases the OS-native lock.
#[derive(Debug)]
pub struct StdLock {
    file: File,
    /// Whether THIS handle holds the lock. Windows range locks do not nest: a second `LockFileEx` by the holder fails
    /// exactly as a stranger's does, and unlocking to find out would open a window in which another process takes
    /// it. So the holder remembers. (Unix `flock` re-locks its own descriptor harmlessly; the flag is kept on both
    /// arms so they behave alike.)
    held: std::sync::atomic::AtomicBool,
}

impl StdLock {
    pub(crate) fn new(file: File) -> Self {
        Self { file, held: std::sync::atomic::AtomicBool::new(false) }
    }
}

/// The one byte the Windows lock covers: far past the 4096-byte record, so the record stays readable while the lock
/// is held (the model's classifier reads it either way). Windows allows a lock beyond end of file.
#[cfg(windows)]
const LOCK_BYTE: u64 = 1 << 62;

fn short_write() -> FsError {
    FsError::new(
        Code::IoError,
        std::io::Error::new(std::io::ErrorKind::WriteZero, "short write of a lock record"),
    )
}

impl LockFile for StdLock {
    fn try_lock(&self) -> Result<bool> {
        use std::sync::atomic::Ordering;
        if self.held.load(Ordering::SeqCst) {
            return Ok(true);
        }
        let granted = os_try_lock(&self.file)?;
        if granted {
            self.held.store(true, Ordering::SeqCst);
        }
        Ok(granted)
    }

    #[cfg(unix)]
    fn read_all(&self, limit: usize) -> Result<Vec<u8>> {
        use std::os::unix::fs::FileExt;
        read_loop(limit, |buf, at| self.file.read_at(buf, at))
    }

    #[cfg(windows)]
    fn read_all(&self, limit: usize) -> Result<Vec<u8>> {
        use std::os::windows::fs::FileExt;
        read_loop(limit, |buf, at| self.file.seek_read(buf, at))
    }

    #[cfg(unix)]
    fn write_at_start(&self, bytes: &[u8]) -> Result<()> {
        use std::os::unix::fs::FileExt;
        let n = self.file.write_at(bytes, 0).map_err(FsError::from_io)?;
        if n != bytes.len() {
            return Err(short_write());
        }
        // Cut any older, longer contents away (plan decision 5).
        self.file.set_len(bytes.len() as u64).map_err(FsError::from_io)
    }

    #[cfg(windows)]
    fn write_at_start(&self, bytes: &[u8]) -> Result<()> {
        use std::os::windows::fs::FileExt;
        let n = self.file.seek_write(bytes, 0).map_err(FsError::from_io)?;
        if n != bytes.len() {
            return Err(short_write());
        }
        // Cut any older, longer contents away (plan decision 5).
        self.file.set_len(bytes.len() as u64).map_err(FsError::from_io)
    }

    fn sync_all(&self) -> Result<()> {
        self.file.sync_all().map_err(FsError::from_io)
    }

    #[cfg(unix)]
    fn identity(&self) -> Result<FileIdentity> {
        let st =
            rustix::fs::fstat(&self.file).map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(crate::std_fs::metadata_from_stat(&st).identity)
    }

    #[cfg(windows)]
    fn identity(&self) -> Result<FileIdentity> {
        Ok(crate::std_fs::identity_of_handle(&self.file))
    }
}

/// One non-blocking attempt at the OS-native lock: `Ok(false)` only when another open file description holds it.
#[cfg(unix)]
fn os_try_lock(file: &File) -> Result<bool> {
    use rustix::fs::FlockOperation;
    match rustix::fs::flock(file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(true),
        Err(e) if e == rustix::io::Errno::WOULDBLOCK => Ok(false),
        Err(e) => Err(FsError::from_io(std::io::Error::from(e))),
    }
}

/// One non-blocking attempt at the OS-native lock: `Ok(false)` only when another handle holds `LOCK_BYTE`.
#[cfg(windows)]
fn os_try_lock(file: &File) -> Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;
    // SAFETY: an all-zero OVERLAPPED is valid, and only its offset fields are then set; the handle is open for the
    // whole call; the call is synchronous (FAIL_IMMEDIATELY on a synchronous handle), so `ov` outlives it.
    let ok = unsafe {
        let mut ov: OVERLAPPED = std::mem::zeroed();
        ov.Anonymous.Anonymous.Offset = LOCK_BYTE as u32;
        ov.Anonymous.Anonymous.OffsetHigh = (LOCK_BYTE >> 32) as u32;
        LockFileEx(
            file.as_raw_handle() as HANDLE,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut ov,
        )
    };
    if ok != 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    if e.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        return Ok(false);
    }
    Err(FsError::from_io(e))
}

/// Read from offset 0 until end of file or `limit + 1` bytes, whichever comes first.
pub(crate) fn read_loop(
    limit: usize,
    mut read_at: impl FnMut(&mut [u8], u64) -> std::io::Result<usize>,
) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; limit + 1];
    let mut n = 0;
    while n < buf.len() {
        let got = read_at(&mut buf[n..], n as u64).map_err(FsError::from_io)?;
        if got == 0 {
            break;
        }
        n += got;
    }
    buf.truncate(n);
    Ok(buf)
}

/// The built-in LOCAL allowlist (cut 7a spec, F1). Everything else - SMB, NFS, 9p, FUSE, FAT, overlay - is
/// `Unsupported`. An OS failure of the query is an `Err` carrying it (plan decision 6).
#[cfg(target_os = "linux")]
pub(crate) fn capability_of(fd: &std::os::fd::OwnedFd) -> Result<flux_fs::LockCapability> {
    // ext2/3/4, XFS, Btrfs, tmpfs, F2FS (statfs(2) f_type magic numbers).
    const LOCAL: [u32; 5] = [0xEF53, 0x5846_5342, 0x9123_683E, 0x0102_1994, 0xF2F5_2010];
    let st = rustix::fs::fstatfs(fd).map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
    // f_type's width and signedness differ by architecture; every magic fits in 32 bits.
    #[allow(clippy::unnecessary_cast)]
    let magic = st.f_type as u64 as u32;
    Ok(if LOCAL.contains(&magic) {
        flux_fs::LockCapability::LocalStrong
    } else {
        flux_fs::LockCapability::Unsupported
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn capability_of(fd: &std::os::fd::OwnedFd) -> Result<flux_fs::LockCapability> {
    let st = rustix::fs::fstatfs(fd).map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
    let name: Vec<u8> = st.f_fstypename.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    Ok(match name.as_slice() {
        b"apfs" | b"hfs" => flux_fs::LockCapability::LocalStrong,
        _ => flux_fs::LockCapability::Unsupported,
    })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(crate) fn capability_of(_fd: &std::os::fd::OwnedFd) -> Result<flux_fs::LockCapability> {
    Ok(flux_fs::LockCapability::Unsupported)
}

/// NTFS or ReFS, on a fixed or removable drive. The filesystem NAME alone is not enough: an SMB share of an NTFS
/// volume reports "NTFS", so the drive type decides local versus remote.
#[cfg(windows)]
pub(crate) fn capability_of(
    h: &std::os::windows::io::OwnedHandle,
) -> Result<flux_fs::LockCapability> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
    };
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    let raw = h.as_raw_handle() as HANDLE;
    let os_error = || FsError::from_io(std::io::Error::last_os_error());

    let mut fs_name = [0u16; 64];
    // SAFETY: every out-pointer is either null (not wanted) or a live buffer with its length passed.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            raw,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(os_error());
    }
    let end = fs_name.iter().position(|&c| c == 0).unwrap_or(fs_name.len());
    let fs_name = String::from_utf16_lossy(&fs_name[..end]);
    if fs_name != "NTFS" && fs_name != "ReFS" {
        return Ok(flux_fs::LockCapability::Unsupported);
    }

    // The directory's final path, first as VOLUME_NAME_GUID: `\\?\Volume{GUID}\...` names the volume itself, for
    // every local volume, with or without a drive letter (a volume mounted in a folder has none). Its root is what
    // GetDriveTypeW is asked about: never a path resolved against the current directory, never one a long-path limit
    // truncates. A network share has no GUID path, so that query fails for it; the DOS form then tells a share
    // (`\\?\UNC\...`: Unsupported) from a real failure (an error, plan decision 6).
    const VOLUME_NAME_GUID: u32 = 0x1;
    let final_path = |flags: u32| -> std::io::Result<String> {
        let mut path = vec![0u16; 32_768];
        // SAFETY: `path` is live and its length is passed.
        let n =
            unsafe { GetFinalPathNameByHandleW(raw, path.as_mut_ptr(), path.len() as u32, flags) }
                as usize;
        if n == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n >= path.len() {
            // The buffer holds the longest path Windows has; a longer answer is not one this code can read.
            return Err(std::io::Error::other(
                "the directory's final path is longer than 32767 units",
            ));
        }
        path.truncate(n);
        Ok(String::from_utf16_lossy(&path))
    };
    let guid_path = match final_path(VOLUME_NAME_GUID) {
        Ok(p) => p,
        Err(guid_error) => {
            return match final_path(0) {
                Ok(dos) if dos.starts_with(r"\\?\UNC\") => Ok(flux_fs::LockCapability::Unsupported),
                _ => Err(FsError::from_io(guid_error)),
            };
        }
    };
    // "\\?\Volume{GUID}\rest" -> "\\?\Volume{GUID}\": the root runs up to and including the fourth backslash.
    let root_len = guid_path.match_indices('\\').nth(3).map(|(i, _)| i + 1);
    let (Some(root_len), true) = (root_len, guid_path.starts_with(r"\\?\Volume{")) else {
        return Err(FsError::from_io(std::io::Error::other(format!(
            "an unexpected volume path: {guid_path}"
        ))));
    };
    let root: Vec<u16> = guid_path[..root_len].encode_utf16().chain(Some(0)).collect();
    // SAFETY: `root` is NUL-terminated and live for the call.
    Ok(match unsafe { GetDriveTypeW(root.as_ptr()) } {
        DRIVE_FIXED | DRIVE_REMOVABLE => flux_fs::LockCapability::LocalStrong,
        _ => flux_fs::LockCapability::Unsupported,
    })
}

/// The host's boot session, for the lock record's `boot_session_id` (§259.6, §229). `"unknown"` when the platform
/// cannot say, and such a value never helps prove a reboot.
///
/// In 7a the ID is recorded and reported only: a dead owner is decided by the OS-native lock. The macOS and Windows
/// sources are boot TIMES, which the OS may adjust when the wall clock is stepped; that is unmeasured, and cut 7b must
/// settle it before any rule relies on two readings being equal within one boot.
#[cfg(target_os = "linux")]
pub fn boot_session_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// `kern.boottime`, set at boot (see the Linux arm's note on clock steps).
#[cfg(target_os = "macos")]
pub fn boot_session_id() -> String {
    let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
    let mut len = std::mem::size_of::<libc::timeval>();
    let name = c"kern.boottime";
    // SAFETY: `name` is NUL-terminated; `tv` and `len` are live and correctly sized.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut tv).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    // A write shorter than a whole `timeval` is not a boot time.
    if rc == 0 && len == std::mem::size_of::<libc::timeval>() {
        format!("{}.{:06}", tv.tv_sec, tv.tv_usec)
    } else {
        "unknown".to_string()
    }
}

/// `BootTime` from `SystemTimeOfDayInformation`, set at boot (100 ns units since 1601; see the Linux arm's note). Not "now minus
/// GetTickCount64": that differs by a second between two processes of one boot (plan decision 2).
#[cfg(windows)]
pub fn boot_session_id() -> String {
    use windows_sys::Wdk::System::SystemInformation::NtQuerySystemInformation;
    // Layout only: the kernel fills every field, and only `boot_time` is read.
    #[allow(dead_code)]
    #[repr(C)]
    struct SystemTimeOfDayInformation {
        boot_time: i64,
        current_time: i64,
        time_zone_bias: i64,
        time_zone_id: u32,
        reserved: u32,
        boot_time_bias: u64,
        sleep_time_bias: u64,
    }
    const SYSTEM_TIME_OF_DAY_INFORMATION: i32 = 3;
    // SAFETY: every field is a plain integer, so all-zero is a valid value.
    let mut info: SystemTimeOfDayInformation = unsafe { std::mem::zeroed() };
    let mut len = 0u32;
    // SAFETY: `info` is live and its size is passed; the class is the documented SystemTimeOfDayInformation.
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_TIME_OF_DAY_INFORMATION as _,
            (&raw mut info).cast(),
            std::mem::size_of::<SystemTimeOfDayInformation>() as u32,
            &raw mut len,
        )
    };
    // NT_SUCCESS: a non-negative NTSTATUS.
    if status >= 0 && info.boot_time != 0 {
        info.boot_time.to_string()
    } else {
        "unknown".to_string()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn boot_session_id() -> String {
    "unknown".to_string()
}
