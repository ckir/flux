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
fn read_loop(
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
