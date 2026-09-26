//! The POSIX `DirHandle`: `openat` with `O_NOFOLLOW | O_DIRECTORY` (§149.7).

use flux_fs::{Code, DirHandle, FsError, Metadata, Result, check_component};
use rustix::fs::{Access, AtFlags, Mode, OFlags, accessat, openat, statat};
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
                    // `st_mode` is `u32` on Linux but `u16` on macOS: the cast is a
                    // no-op on one platform and required on the other.
                    #[allow(clippy::unnecessary_cast)]
                    let mode = st.st_mode as u32;
                    let is_link = (mode & libc_s_ifmt()) == libc_s_iflnk();
                    if is_link {
                        FsError::new(Code::SafetyRejected, io)
                    } else {
                        FsError::new(Code::DestinationError, io)
                    }
                }
                // The stat did not answer, and WHY matters. This arm used to read
                // every failure as "the name vanished between the open and the
                // stat", which is only one of the reasons it can fail: a directory
                // we may traverse but not stat answers EACCES, and calling that a
                // broken destination hides a permission problem the caller could
                // act on. ENOENT really is the vanished case. Anything else is
                // reported as what it is rather than as what it most often is.
                Err(Errno::ACCESS) | Err(Errno::PERM) => FsError::new(Code::PermissionDenied, io),
                Err(Errno::NOENT) => FsError::new(Code::DestinationError, io),
                Err(_) => FsError::new(Code::IoError, io),
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

    fn identity(&self) -> Result<flux_fs::FileIdentity> {
        let st =
            rustix::fs::fstat(&self.0).map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(crate::std_fs::metadata_from_stat(&st).identity)
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

    /// A directory is refused with `IoError`/`IsADirectory` on every platform, the
    /// shape the Windows arm builds explicitly. Linux gets it for free from EISDIR.
    /// MEASURED on macOS CI: `unlinkat` answers EPERM for a directory, which maps to
    /// PERMISSION_DENIED -- a code the user sees, for a failure that has nothing to
    /// do with permissions. But EPERM is also what a genuinely forbidden unlink
    /// returns there (an immutable file), so the errno alone cannot tell them apart.
    /// As in `classify`, one `statat` separates them, and only on the failure path.
    ///
    /// The stat runs after the unlink failed, so the name can change in between;
    /// the worst case is a wrong error CODE for an unlink that already refused,
    /// never a removal. If the stat itself fails, the original EPERM stands.
    fn remove_file(&self, name: &OsStr) -> Result<()> {
        use rustix::fs::FileType;
        use rustix::io::Errno;
        check_component(name)?;
        rustix::fs::unlinkat(&self.0, name, AtFlags::empty()).map_err(|e| {
            if e == Errno::PERM
                && let Ok(st) = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)
                && FileType::from_raw_mode(st.st_mode) == FileType::Directory
            {
                return FsError::new(
                    Code::IoError,
                    std::io::Error::new(
                        std::io::ErrorKind::IsADirectory,
                        "remove_file refuses a directory",
                    ),
                );
            }
            FsError::from_io(std::io::Error::from(e))
        })
    }

    /// NO FALLBACK, and that is the decision rather than an omission.
    ///
    /// `RENAME_NOREPLACE` is not universal. MEASURED on a 9p-mounted volume: this
    /// fails with EINVAL even for a name that is FREE, so no-replace publication is
    /// unavailable on such a destination -- the exact POSIX counterpart of the tag
    /// query that answers nothing on FAT32.
    ///
    /// A review proposed falling back to stat-then-rename. That fix is REFUSED, and
    /// the reason is measured rather than stylistic: a non-atomic fallback
    /// reintroduces precisely the check-then-act window this cut exists to close,
    /// and under contention the difference is not marginal -- an atomic no-replace
    /// rename was measured at 400 correct refusals out of 400, and the check-then-act
    /// form at 3188 wrong outcomes. Silently degrading to that on filesystems the
    /// caller cannot see would be worse than failing, because the guarantee would be
    /// gone while the API still claimed it.
    ///
    /// Failing loudly is therefore correct here. What is MISSING is upstream: §241.5
    /// wants a destination with no no-replace primitive refused UP FRONT with
    /// NoReplacePublishUnavailable, before anything changes, rather than discovered
    /// at the first publish. That probe belongs to the engine, which does not exist
    /// yet, and is tracked; this measurement is the evidence that such destinations
    /// are real and reachable rather than hypothetical.
    fn rename_no_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        rustix::fs::renameat_with(&self.0, from, &other.0, to, rustix::fs::RenameFlags::NOREPLACE)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }

    fn rename_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        // The TARGET lives in `other`. See `is_write_protected_at`.
        if is_write_protected_at(&other.0, to) {
            return Err(FsError::new(
                Code::PermissionDenied,
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "destination is read-only",
                ),
            ));
        }
        rustix::fs::renameat(&self.0, from, &other.0, to)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }
}

/// The handle-relative twin of `destination_is_write_protected` in `std_fs.rs`,
/// asking the same question the same way, of a NAME inside `dir`.
///
/// `renameat` checks the DIRECTORY's write permission and never the target's, so
/// without this it replaces a file the user marked read-only -- `cp -f` semantics
/// where plain `cp` refuses. Same three rules as the path version, for the same
/// measured reasons recorded there: a missing name protects nothing; a symlink is
/// replaced and never followed, so its target's mode is irrelevant; and only
/// EACCES / EPERM / EROFS from an EFFECTIVE-uid `accessat` mean "you may not write
/// this" -- anything else (ETXTBSY for a running binary) is not a protection.
fn is_write_protected_at(dir: &OwnedFd, name: &OsStr) -> bool {
    use rustix::fs::FileType;
    use rustix::io::Errno;
    match statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(_) => false,
        Ok(st) if FileType::from_raw_mode(st.st_mode) == FileType::Symlink => false,
        Ok(_) => matches!(
            accessat(dir, name, Access::WRITE_OK, AtFlags::EACCESS),
            Err(Errno::ACCESS | Errno::PERM | Errno::ROFS)
        ),
    }
}
