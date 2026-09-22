//! The one real `FileSystem`, over `std::fs`.
//!
//! `rename_replace` uses `std::fs::rename`, which has POSIX replace semantics on
//! every platform Rust supports. On Windows this succeeds where
//! `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` fails — FS-6 and FS-7 measured exactly
//! that, and Section 241.5 is amended in Task 9 to name it.

use flux_fs::{FileHandle, FileSystem, FsError, Metadata, Perms, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::time::SystemTime;

/// The source handle. Read only -- it does not implement `Write` at all, so even a
/// direct (non-generic) caller of `open_read` cannot write to the file it opened.
pub struct StdReader(File);

impl Read for StdReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

/// The temporary's handle.
pub struct StdFile(File);

impl Write for StdFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl FileHandle for StdFile {
    fn sync_all(&self) -> Result<()> {
        self.0.sync_all().map_err(FsError::from_io)
    }
}

pub struct StdFileSystem;

impl FileSystem for StdFileSystem {
    type Reader = StdReader;
    type Writer = StdFile;

    fn open_read(&self, path: &Path) -> Result<Self::Reader> {
        File::open(path).map(StdReader).map_err(FsError::from_io)
    }

    fn create_new(&self, path: &Path) -> Result<Self::Writer> {
        OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(path)
            .map(StdFile)
            .map_err(FsError::from_io)
    }

    fn metadata(&self, path: &Path) -> Result<Metadata> {
        // `symlink_metadata`, NOT `metadata`: `std::fs::metadata` dereferences a
        // symlink and would report the TARGET as a regular file, so a symlink would
        // sail past the SPECIAL_FILE_UNSUPPORTED refusal and be copied as its target.
        // Measured: for a symlink to a regular file, `metadata().is_file()` is true
        // and only `symlink_metadata()` sees the link. §2 Foundational Invariants
        // item 21 also says symlink targets never contribute unless link-following is
        // explicitly enabled, which it is not in this cut.
        let m = std::fs::symlink_metadata(path).map_err(FsError::from_io)?;
        Ok(Metadata {
            len: m.len(),
            is_file: m.is_file(),
            permissions: Some(perms_of(&m)),
            modified: m.modified().ok(),
        })
    }

    fn set_times(&self, file: &Self::Writer, modified: Option<SystemTime>) -> Result<()> {
        let Some(t) = modified else { return Ok(()) };
        let times = std::fs::FileTimes::new().set_modified(t);
        file.0.set_times(times).map_err(FsError::from_io)
    }

    fn set_permissions(&self, file: &Self::Writer, perms: Option<Perms>) -> Result<()> {
        let Some(p) = perms else { return Ok(()) };
        let native = match p {
            #[cfg(unix)]
            Perms::UnixMode(mode) => std::os::unix::fs::PermissionsExt::from_mode(mode),
            #[cfg(not(unix))]
            Perms::UnixMode(_) => return Ok(()),
            Perms::ReadOnly(ro) => {
                let mut q = file.0.metadata().map_err(FsError::from_io)?.permissions();
                q.set_readonly(ro);
                q
            }
        };
        file.0.set_permissions(native).map_err(FsError::from_io)
    }

    fn rename_replace(&self, from: &Path, to: &Path) -> Result<()> {
        std::fs::rename(from, to).map_err(FsError::from_io)
    }

    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()> {
        if to.exists() {
            return Err(FsError::new(
                flux_fs::Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        std::fs::rename(from, to).map_err(FsError::from_io)
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        std::fs::remove_file(path).map_err(FsError::from_io)
    }
}

#[cfg(unix)]
fn perms_of(m: &std::fs::Metadata) -> Perms {
    use std::os::unix::fs::PermissionsExt;
    Perms::UnixMode(m.permissions().mode())
}

#[cfg(not(unix))]
fn perms_of(m: &std::fs::Metadata) -> Perms {
    Perms::ReadOnly(m.permissions().readonly())
}
