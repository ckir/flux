//! The POSIX `DirHandle`: `openat` with `O_NOFOLLOW | O_DIRECTORY` (§149.7).

use flux_fs::{Code, DirHandle, FsError, Metadata, Result, check_component};
use rustix::fs::{AtFlags, Mode, OFlags, openat, statat};
use std::ffi::OsStr;
use std::os::fd::OwnedFd;

/// `Debug` is required, not decorative: the tests call `.unwrap_err()` on a
/// `Result<StdDir, _>`, which needs `StdDir: Debug` to compile.
#[derive(Debug)]
pub struct StdDir(OwnedFd);

impl StdDir {
    pub(crate) fn from_fd(fd: OwnedFd) -> Self {
        Self(fd)
    }

    /// Classify a failed `openat`. MEASURED: `ENOTDIR` covers BOTH a symlink and an
    /// ordinary non-directory, so the errno alone cannot answer §149.7's question.
    /// One `statat(SYMLINK_NOFOLLOW)` separates them, and it runs ONLY here -- on a
    /// path that has already failed -- so the happy path pays nothing.
    ///
    /// Note it is NOT `ELOOP`, which is the error `O_NOFOLLOW` is usually
    /// associated with: with `O_DIRECTORY` also set the kernel reports the type
    /// mismatch first. A reader who "corrects" this to `ELOOP` removes the check.
    fn classify(&self, name: &OsStr, e: rustix::io::Errno) -> FsError {
        use rustix::io::Errno;
        let io = std::io::Error::from(e);
        match e {
            Errno::NOTDIR => match statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(st) => {
                    let is_link = (st.st_mode as u32 & libc_s_ifmt()) == libc_s_iflnk();
                    if is_link {
                        FsError::new(Code::SafetyRejected, io)
                    } else {
                        FsError::new(Code::DestinationError, io)
                    }
                }
                // The name vanished between the open and the stat. It is no longer
                // a component we can judge, so report what we can prove.
                Err(_) => FsError::new(Code::DestinationError, io),
            },
            Errno::NOENT => FsError::new(Code::DestinationError, io),
            Errno::ACCESS | Errno::PERM => FsError::new(Code::PermissionDenied, io),
            _ => FsError::new(Code::IoError, io),
        }
    }
}

const fn libc_s_ifmt() -> u32 {
    0o170000
}
const fn libc_s_iflnk() -> u32 {
    0o120000
}

impl DirHandle for StdDir {
    type Writer = crate::StdFile;

    fn open_dir(&self, name: &OsStr) -> Result<Self> {
        check_component(name)?;
        match openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => Ok(Self(fd)),
            Err(e) => Err(self.classify(name, e)),
        }
    }

    fn create_dir(&self, name: &OsStr) -> Result<Self> {
        check_component(name)?;
        rustix::fs::mkdirat(&self.0, name, Mode::from_raw_mode(0o777))
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        self.open_dir(name)
    }

    fn create_new(&self, name: &OsStr) -> Result<Self::Writer> {
        check_component(name)?;
        let fd = openat(
            &self.0,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o666),
        )
        .map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(crate::std_fs::std_file_from(std::fs::File::from(fd)))
    }

    fn metadata(&self, name: &OsStr) -> Result<Metadata> {
        check_component(name)?;
        let st = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(crate::std_fs::metadata_from_stat(&st))
    }

    fn remove_file(&self, name: &OsStr) -> Result<()> {
        check_component(name)?;
        rustix::fs::unlinkat(&self.0, name, AtFlags::empty())
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }

    fn rename_no_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        rustix::fs::renameat_with(&self.0, from, &other.0, to, rustix::fs::RenameFlags::NOREPLACE)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }

    fn rename_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        rustix::fs::renameat(&self.0, from, &other.0, to)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }
}
