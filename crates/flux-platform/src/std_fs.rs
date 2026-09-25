//! The one real `FileSystem`, over `std::fs`.
//!
//! `rename_replace` uses `std::fs::rename`, which has POSIX replace semantics on
//! every platform Rust supports. On Windows this succeeds where
//! `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` fails — FS-6 and FS-7 measured exactly
//! that, and Section 241.5 is amended in Task 9 to name it.

use flux_fs::{
    DirEntry, FileHandle, FileIdentity, FileSystem, FileType, FsError, Metadata, ObjectId, Perms,
    Result,
};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
#[cfg(unix)]
use std::time::Duration;
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

/// Construct a writer from an already-open file. `DirHandle::create_new` opens
/// relative to a directory handle, so it cannot go through the path-based
/// `create_new` and needs this.
///
/// Ungated: `dir_unix.rs` and `dir_windows.rs` both call it. It was briefly
/// `#[cfg(unix)]`, because for one task the POSIX arm was its only caller and
/// Windows `just check` fails on `-D dead_code`; the comment then said the NEXT
/// task would widen it, and named the wrong task. Nobody widened it, and the
/// Windows arm hit it as a compile error instead. A gate whose removal is somebody
/// else's homework is a gate that stays.
pub(crate) fn std_file_from(f: File) -> StdFile {
    StdFile(f)
}

pub struct StdFileSystem;

/// Can this process actually replace the destination NAME?
///
/// `Permissions::readonly()` asks only whether ANY write bit is set, which is the wrong
/// question. Measured on Linux: a root-owned `0644` file reports writable to a normal
/// user, `cp` refuses it with "Permission denied", and `rename(2)` replaces it anyway,
/// because rename consults the DIRECTORY's permission and never the file's. So ask the
/// question `cp` asks -- can WE write this name?
///
/// KNOWN AND UNCLOSEABLE: this is check-then-act, so a permission change landing between
/// the probe and the `rename` is not seen, and the OS is no backstop -- `rename` is
/// exactly what does NOT consult the file. The race cannot be closed through `std`,
/// whose `rename` takes paths rather than the handle we probed with, and neither
/// platform offers a "rename only if I may replace the target" primitive. `cp` carries
/// the same window. Recorded rather than papered over.
#[cfg(unix)]
fn destination_is_write_protected(to: &Path) -> bool {
    use rustix::fs::{Access, AtFlags, CWD, accessat};

    match std::fs::symlink_metadata(to) {
        // Nothing occupies the name, so there is nothing to protect.
        Err(_) => false,
        // `rename` replaces the LINK and never follows it, so the target's permissions
        // are irrelevant. `accessat` WOULD follow it, and without this arm it refuses a
        // rename that is perfectly safe -- the case
        // `rename_replace_allows_a_symlink_whose_target_is_read_only` pins.
        Ok(m) if m.file_type().is_symlink() => false,
        // `EACCESS` asks about the EFFECTIVE uid, which is the credential `rename`
        // itself enforces; plain `access(2)` would ask about the real one.
        //
        // Only these three errnos mean "you may not write this". Anything else -- EIO,
        // ENOMEM, ETXTBSY -- is NOT a protection, and converting it into "destination is
        // read-only" would both lie and hide a real failure. Measured: for a RUNNING
        // binary, `access(W_OK)` returns OK while `open(O_WRONLY)` returns ETXTBSY, and
        // `rename` over it succeeds, so atomic binary replacement keeps working.
        Ok(_) => matches!(
            accessat(CWD, to, Access::WRITE_OK, AtFlags::EACCESS),
            Err(rustix::io::Errno::ACCESS | rustix::io::Errno::PERM | rustix::io::Errno::ROFS)
        ),
    }
}

/// The Windows half, asking the SAME question as the Unix half.
///
/// The read-only ATTRIBUTE is not the protection here. Measured on Windows 11 with an
/// ACL denying `(W,D,DC)` to the current user: the attribute is unset, so an attribute
/// check passes, and `rename` then replaced the file and destroyed its contents. An
/// attribute check would also have left this platform refusing where Unix allows and
/// allowing where Unix refuses -- the very divergence the guard exists to remove.
///
/// So it asks the OS instead, with a `DELETE`-access probe, AND keeps the attribute
/// check -- measured across four cases, neither alone is sufficient:
///
/// ```text
///                  .write(true)   .access_mode(DELETE)   attribute
/// ACL denies W,D    Some(5)        Some(5)               false
/// read-only attr    Some(5)        None                  true
/// plain writable    None           None                  false
/// held FileShare    Some(32)       Some(32)              false
/// ```
///
/// The TOCTOU note on the Unix twin applies here too.
#[cfg(windows)]
fn destination_is_write_protected(to: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    /// `ERROR_ACCESS_DENIED`.
    const ACCESS_DENIED: i32 = 5;
    /// The Win32 `DELETE` right, which is what `rename` actually needs on the target.
    const DELETE: u32 = 0x0001_0000;

    match std::fs::symlink_metadata(to) {
        Err(_) => false,
        // As on Unix: `rename` replaces the LINK, so the target's ACL is irrelevant.
        Ok(m) if m.file_type().is_symlink() => false,
        // BOTH questions, because MEASURED, neither alone is enough:
        //
        //   read-only ATTRIBUTE set  -> attribute true,  DELETE probe None
        //   ACL denies (W,D,DC)      -> attribute false, DELETE probe 5
        //
        // Probing for DELETE rather than WRITE is deliberate twice over. `rename` needs
        // DELETE on the target, not write, so asking about write can refuse a rename the
        // OS would allow; and a write-intent open asks a cloud-sync filter driver to
        // HYDRATE an offline file -- a blocking download -- merely to answer a
        // permission question.
        //
        // Only `ERROR_ACCESS_DENIED` counts. MEASURED, with a control that signalled only
        // once the handle was actually open: a file held with `FileShare::None` gives
        // code 32, `ERROR_SHARING_VIOLATION`, from BOTH probes. That is somebody else
        // holding the file, not a protection, so it must not refuse -- let `rename`
        // report the sharing violation itself. Same allow-list discipline as the Unix
        // arm's errnos. See the TOCTOU note on the Unix twin, which applies here too.
        Ok(m) => {
            m.permissions().readonly()
                || std::fs::OpenOptions::new()
                    .access_mode(DELETE)
                    .open(to)
                    .err()
                    .and_then(|e| e.raw_os_error())
                    == Some(ACCESS_DENIED)
        }
    }
}

// `rustix::fs::renameat_with` is gated `any(apple, linux_kernel, target_os = "redox")`,
// so a Unix outside that set has no atomic no-replace primitive reachable from here.
// Fail at BUILD time naming the reason, rather than at link time naming a missing
// function -- and do NOT add a check-then-act fallback, which
// FLUX_FULL_UPDATED_SPEC_V16.md:10876 forbids as a substitute by name.
#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        // rustix's own gate includes redox, and redox sets target_family = "unix",
        // so omitting it here would refuse to compile on a platform the dependency
        // fully supports. An earlier draft did exactly that, contradicting the
        // comment directly above.
        target_os = "redox"
    ))
))]
compile_error!(
    "no atomic no-replace rename primitive on this target; see FLUX_FULL_UPDATED_SPEC_V16.md \
     §241.5 -- check-then-rename is not an acceptable substitute"
);

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

    #[cfg(unix)]
    fn metadata(&self, path: &Path) -> Result<Metadata> {
        // `symlink_metadata`, NOT `metadata`: the latter dereferences a symlink and
        // would report the TARGET as a regular file, so a symlink would sail past
        // the SPECIAL_FILE_UNSUPPORTED refusal and be copied as its target.
        let m = std::fs::symlink_metadata(path).map_err(FsError::from_io)?;
        Ok(Metadata {
            len: m.len(),
            file_type: type_of(&m),
            permissions: Some(perms_of(&m)),
            modified: m.modified().ok(),
            // Free on Unix: `st_dev` and `st_ino` are already in the stat buffer, so
            // there is no second open to eliminate here and nothing to change.
            identity: identity_of(&m),
        })
    }

    #[cfg(windows)]
    fn metadata(&self, path: &Path) -> Result<Metadata> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        // ONE open, so len, type and identity all describe the SAME object. Two
        // opens left a window in which the path could be replaced between them.
        //
        // The flags are not a free choice; std's own source settles all three.
        // BACKUP_SEMANTICS "allows opening directories" - without it a directory
        // cannot be opened at all. OPEN_REPARSE_POINT "opens a link instead of its
        // target" - without it this would read a symlink's TARGET identity,
        // silently corrupting cycle detection. access_mode(0): "No read or write
        // permissions are necessary", which avoids both a sharing violation and a
        // denial on a file this process may not read.
        let f = OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(FsError::from_io)?;
        let m = f.metadata().map_err(FsError::from_io)?;
        Ok(Metadata {
            len: m.len(),
            file_type: type_of(&m),
            permissions: Some(perms_of(&m)),
            modified: m.modified().ok(),
            identity: identity_of_handle(&f),
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
        // Refuse a read-only destination, on every platform.
        //
        // `std::fs::rename` checks the DIRECTORY's write permission, not the target
        // file's, so on Unix it happily replaces a file the user marked read-only --
        // `cp -f` semantics where plain `cp` refuses with "Permission denied".
        // Windows refuses already, because its rename does consult the file's
        // read-only attribute. Without this guard the same command silently destroyed
        // a protection on one platform and failed on the other; both were measured.
        //
        // Clearing the attribute and retrying was considered and rejected: it weakens
        // a protection the user deliberately set, and if the retry then fails for any
        // other reason it stays weakened. Deleting the target first is worse still --
        // on Windows a target held open with FILE_SHARE_DELETE enters pending-delete,
        // the rename fails, and the destination is lost when the reader closes it.
        if destination_is_write_protected(to) {
            return Err(FsError::new(
                flux_fs::Code::PermissionDenied,
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "destination is read-only",
                ),
            ));
        }
        std::fs::rename(from, to).map_err(FsError::from_io)
    }

    /// Publish without replacing, atomically.
    ///
    /// §241.5 requires a target planned as new be published "with a primitive that
    /// refuses to replace an existing entry", and `:10876` forbids the alternative by
    /// name: "check-then-rename is never used as a substitute." The previous body was
    /// exactly that substitute -- `symlink_metadata` then `rename` -- so two processes
    /// could both see a free name and one silently won.
    ///
    /// The contract and the error are UNCHANGED: an occupied target is still
    /// `Code::IoError` carrying `ErrorKind::AlreadyExists`. Only the atomicity is new,
    /// and the observable difference is confined to the concurrent case that used to
    /// lose silently.
    #[cfg(unix)]
    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()> {
        // ONE arm for Linux and macOS, because rustix already abstracts the two
        // primitives §241.5 names separately: `RenameFlags::NOREPLACE` is
        // `RENAME_NOREPLACE` on Linux and `RENAME_EXCL` on Apple (rustix
        // src/backend/libc/fs/types.rs, the `#[cfg(apple)]` bitflags block), and
        // `renameat_with` is gated `any(apple, linux_kernel, redox)`.
        use rustix::fs::{CWD, RenameFlags, renameat_with};

        // `std::io::Error::from(Errno)` is `from_raw_os_error`, so `raw_os_error()`
        // SURVIVES the conversion and `FsError::from_io` can still classify -- verified
        // at rustix-1.1.4/src/io/errno.rs:58-63. That matters here specifically: the
        // comment on `FsError::source` records that rebuilding an error as
        // `Error::other(..)` destroys `raw_os_error()`, and that doing so was MEASURED
        // to turn `DiskFull` into `IoError`. Do not "improve" this line by adding
        // context to the error.
        let published = renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)));

        // THE TWO UNIXES DISAGREE ABOUT A NAME PUBLISHED ONTO ITSELF, and only CI could
        // find it: `rename_no_replace_refuses_the_source_itself` was the ONE test of 171
        // that failed on macos-latest while Linux and Windows were green. Linux's
        // `RENAME_NOREPLACE` answers EEXIST. Apple's `RENAME_EXCL` does not -- it keeps
        // POSIX `rename`'s rule that old and new naming the SAME existing file is a
        // success that does nothing. Hard links are unaffected: macOS refuses those, and
        // that test passed there.
        //
        // The name is occupied, so `AlreadyExists` is the answer on every platform, and
        // this is the same veto the Windows arm carries for the same reason.
        //
        // CHECKED AFTER THE CALL, deliberately, and this is the half worth keeping. It
        // costs no extra syscall, and it CANNOT invert the error priority the way a
        // pre-check would -- a mistake already made and fixed once on the Windows arm.
        // A missing source still answers ENOENT from the kernel rather than from us,
        // because the kernel ran first. Letting it run is free precisely because the
        // operation it performs in this case is nothing: renaming a name onto itself
        // destroys nothing, so there is no damage to undo by the time we look.
        if published.is_ok() && from == to {
            return Err(FsError::new(
                flux_fs::Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        published
    }

    /// See the Unix twin for why this is atomic rather than check-then-act.
    #[cfg(windows)]
    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, MoveFileExW,
        };

        // No MOVEFILE_REPLACE_EXISTING is the whole point: without that flag
        // MoveFileExW fails rather than replacing, which is the guarantee this method
        // has always promised and until now only approximated.
        //
        // The interior-NUL rejection is not decoration, and leaving it out was a
        // REGRESSION this method introduced. `encode_wide` emits an interior NUL
        // happily, and `MoveFileExW` stops reading at the first one -- so a `to` of
        // "pub\0lish" was MEASURED to publish at "pub" and return Ok, writing to a
        // path the caller never asked for. `std::fs` never had that hole: its own
        // `to_u16s` rejects the same input with InvalidInput ("strings passed to
        // WinAPI cannot contain NULs"), and the check-then-act body this replaced got
        // that for free by going through `std`. Going direct to the FFI means
        // re-establishing it here. The Unix arm needs no equivalent: rustix rejects
        // the same path with EINVAL, MEASURED.
        let wide = |p: &Path| -> std::result::Result<Vec<u16>, std::io::Error> {
            use std::os::windows::ffi::OsStrExt;
            let mut w: Vec<u16> = p.as_os_str().encode_wide().collect();
            if w.contains(&0) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "paths passed to the Windows API cannot contain interior NULs",
                ));
            }
            w.push(0);
            Ok(w)
        };
        let wfrom = wide(from).map_err(FsError::from_io)?;
        let wto = wide(to).map_err(FsError::from_io)?;

        // A SAME-OBJECT VETO, and deliberately NOT a destination pre-check.
        //
        // MoveFileExW without MOVEFILE_REPLACE_EXISTING does not refuse a target that
        // IS the source. MEASURED: `from == to` returned Ok where Linux answers EEXIST,
        // and -- worse -- two hardlinks to one file returned Ok having CONSUMED the
        // source name. In that second case `to` is a genuinely distinct, existing
        // directory entry, so §241.5's "refuses to replace an existing entry" was
        // simply not kept. It is not a philosophical question about whether one object
        // under two names counts as a collision.
        //
        // WHY THIS IS NOT THE SUBSTITUTE :10876 FORBIDS. That ban is on using a probe
        // to AUTHORIZE a rename, which is what opens the TOCTOU window: check the name
        // is free, then rename, and a concurrent publisher slips between the two. This
        // probe only ever VETOES. Every path out of this block that is not an outright
        // refusal falls through to MoveFileExW, which still decides atomically -- a
        // publisher that creates `to` after the open below fails is caught there
        // exactly as before. Nothing here can permit a rename the primitive would have
        // refused, which is the property the ban exists to protect.
        //
        // Opening `to` FIRST is what bounds the cost. The common case -- publishing to
        // a free name -- pays one FAILED open and stops, rather than the two successful
        // opens plus two info queries an unconditional comparison would cost on every
        // published file.
        //
        // The flags are the ones `metadata` documents above: `access_mode(0)` avoids
        // both a sharing violation and a denial on a file this process may not read,
        // and OPEN_REPARSE_POINT opens a link rather than its target.
        //
        // THE LEXICAL CHECK IS NOT REDUNDANT WITH THE IDENTITY ONE, and an earlier
        // draft of this method had only the identity half. On a filesystem with no
        // `FILE_ID_INFO` -- FAT32, exFAT -- `identity_of_handle` answers `Unavailable`,
        // the comparison below falls through by design, and `MoveFileExW` then returns
        // Ok for `from == to`, restoring the exact bug the veto exists to stop. A
        // string comparison costs nothing, opens no handle, and works everywhere.
        //
        // It is deliberately EXACT rather than case-insensitive. A case-only change on
        // Windows resolves to the same object and is refused by the identity half
        // below, which matches both the check-then-act body this replaced (its
        // `symlink_metadata` saw the target and refused -- MEASURED) and macOS, where
        // `RENAME_EXCL` on case-insensitive APFS refuses it too. Renaming a name onto
        // itself in a different case is `rename_replace`'s job, not this method's.
        //
        // WHAT THE TWO HALVES DO NOT COVER, stated because an earlier draft of this
        // comment claimed they were "complete in practice" and that was an overclaim.
        // They are complete only where a filesystem without `FILE_ID_INFO` also has no
        // second name for one object. That holds for FAT32 and exFAT, which have no
        // hard links -- but NOT for an SMB share whose server supports links while the
        // protocol negotiation drops the id, and NOT for two lexically different paths
        // to one object (a `subst` drive, a junction) on such a mount. There the names
        // differ, the identity is unavailable, and `MoveFileExW` consumes the source
        // name as it always did. That is a documented limit of the degraded path, not
        // a reason to drop the guard: removing it would restore the `from == to` hole
        // on every identity-less filesystem, which is strictly worse.
        //
        // `access_mode(0)` is load-bearing HERE for a second reason beyond the one
        // `metadata` gives, and `rename_no_replace_vetoes_a_same_object_target_held_
        // _open_exclusively` goes red if it is widened. Asking for no access means an
        // exclusively-held target still PROBES, so the veto below fires rather than
        // being skipped. The other way a probe can fail on an existing name is a deny
        // ACE -- and there `MoveFileExW` fails too, PermissionDenied, raw 5, MEASURED,
        // because a rename needs DELETE on the source. Those two failures being
        // correlated is what makes the nesting below safe.
        let probe = |p: &Path| {
            OpenOptions::new()
                .access_mode(0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(p)
        };

        // BOTH halves live under this probe of `to`, and the nesting is load-bearing
        // rather than tidiness. A `to` that does not open is either ABSENT or
        // UNREACHABLE -- NOT necessarily free, as the deny-ACE case above shows -- and
        // in both cases the KERNEL is the right one to answer rather than this method.
        //
        // WHAT it answers depends on `from`, and that dependence is the whole argument
        // for not pre-empting it. With a valid `from`, an absent `to` is a SUCCESS --
        // `rename_no_replace_succeeds_when_the_name_is_free` pins exactly that. With
        // `from == to`, an absent `to` means `from` is absent too and NotFound is the
        // honest reply. One condition, two opposite right answers, and only the kernel
        // knows which applies. An earlier draft ran the lexical comparison
        // first and unconditionally, which made `rename_no_replace(absent, absent)`
        // answer AlreadyExists on Windows while Linux answered NotFound: MEASURED, and
        // an inversion of exactly the error priority that
        // `rename_no_replace_reports_a_missing_source_before_an_occupied_target` exists
        // to pin. A guard that front-runs the filesystem inherits the obligation to be
        // right about it.
        // BOTH probes gate the comparison, and `from`'s is what preserves the error
        // order: a missing source with an occupied target must answer NotFound from the
        // kernel, not AlreadyExists from here.
        if let Ok(dst) = probe(to)
            && let Ok(src) = probe(from)
        {
            let same = from == to
                || match (identity_of_handle(&src), identity_of_handle(&dst)) {
                    (FileIdentity::Strong(a), FileIdentity::Strong(b)) => a == b,
                    // IDENTITY CANNOT ANSWER, SO REFUSE -- and the previous revision of
                    // this arm did the opposite, on a reason that does not survive being
                    // read carefully. It said refusing here "would fail CLOSED on every
                    // ordinary publish" on such a filesystem. It would not, because this
                    // branch is only reached when the target ALREADY EXISTS: an ordinary
                    // publish goes to a free name, `probe(to)` fails, and the comparison
                    // is never reached at all.
                    //
                    // Given a target that exists, refusing is right whichever way the
                    // unanswerable question would have gone. A DIFFERENT object is one
                    // `MoveFileExW` would itself refuse with ERROR_ALREADY_EXISTS, so the
                    // answer is unchanged; the SAME object is the case where it returns
                    // Ok and CONSUMES the source name, which is the defect. Refusing
                    // costs a correct answer nothing and buys back the only wrong one.
                    //
                    // That matters on a mount with no `FILE_ID_INFO` that still has a
                    // second name for one object -- an SMB share whose server supports
                    // hard links while the negotiation drops the id, or a `subst` drive
                    // or junction over one. There the paths differ, so the lexical half
                    // misses it, and identity is `Unavailable`, so this half used to fall
                    // through. It is very likely a REGRESSION against the check-then-act
                    // body this method replaced, which refused ANY existing target on
                    // EVERY filesystem.
                    //
                    // MEASURED ON A REAL FAT32 VOLUME, not reasoned. `zz_identity_probe`
                    // against a mounted FAT32 disk reports `identity=Unavailable`, and
                    // the same probe run either side of this arm gives:
                    //
                    //   without it   case_only_refused=false   <- the defect, reproduced
                    //   with it      case_only_refused=true
                    //
                    // with `free_name_succeeds=true` in BOTH, which is what settles the
                    // old objection: publishing to a free name never reaches this arm, so
                    // refusing here costs the ordinary case nothing.
                    //
                    // Still unmeasured: the HARD LINK half. FAT32 has none
                    // (`hard_links=false`), so the originally reported SMB scenario needs
                    // a server that supports links while dropping the id, and no such
                    // mount was available. NTFS and ReFS both answer `Strong`, so the arm
                    // above decides there and their behaviour does not move.
                    _ => true,
                };
            if same {
                return Err(FsError::new(
                    flux_fs::Code::IoError,
                    std::io::Error::from(std::io::ErrorKind::AlreadyExists),
                ));
            }
        }

        // SAFETY: both buffers are NUL-terminated UTF-16 built immediately above and
        // live for the duration of the call.
        let ok = unsafe { MoveFileExW(wfrom.as_ptr(), wto.as_ptr(), 0) };
        if ok == 0 {
            return Err(FsError::from_io(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        std::fs::remove_file(path).map_err(FsError::from_io)
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path).map_err(FsError::from_io)? {
            let entry = entry.map_err(FsError::from_io)?;
            // `DirEntry::file_type` does NOT follow a symlink, and on Windows the
            // reparse tag arrives in `wfd.dwReserved0` as part of the enumeration
            // itself, so this costs no extra syscall there.
            let t = entry.file_type().map_err(FsError::from_io)?;
            out.push(DirEntry {
                name: entry.file_name(),
                file_type: if t.is_symlink() {
                    FileType::Symlink
                } else if t.is_dir() {
                    FileType::Dir
                } else if t.is_file() {
                    FileType::File
                } else {
                    FileType::Other
                },
            });
        }
        Ok(out)
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        std::fs::create_dir(path).map_err(FsError::from_io)
    }
}

/// Resolving `DEST` is the one path-based call in the writer, and it DOES follow
/// links -- §149.7 exempts it, because a user who points DEST at a symlink has
/// chosen that destination. Everything BELOW it goes through a handle.
#[cfg(unix)]
impl flux_fs::DestinationRoot for StdFileSystem {
    type Dir = crate::StdDir;

    fn destination_root(&self, path: &Path) -> Result<Self::Dir> {
        use rustix::fs::{CWD, Mode, OFlags, openat};
        let fd =
            openat(CWD, path, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())
                .map_err(|e| FsError::from_io(std::io::Error::from(e)))?;
        Ok(crate::StdDir::from_fd(fd))
    }
}

/// See the Unix twin: DEST itself is resolved by path and DOES follow links,
/// because §149.7 exempts it. Everything below goes through a handle.
#[cfg(windows)]
impl flux_fs::DestinationRoot for StdFileSystem {
    type Dir = crate::StdDir;

    fn destination_root(&self, path: &Path) -> Result<Self::Dir> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        let f = OpenOptions::new()
            .access_mode(0x80 | 0x1 | 0x0010_0000)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .map_err(FsError::from_io)?;

        // THE ROOT MUST BE A DIRECTORY, and `FILE_FLAG_BACKUP_SEMANTICS` does not say
        // so. It permits opening a directory; it does not REQUIRE one, and a file
        // opens just as happily. MEASURED, before this check: `destination_root` on a
        // plain file returned Ok and handed back a `DirHandle` wrapping a file, and
        // the failure only surfaced later as a masked error on the first child
        // operation. The Unix twin cannot make this mistake because `OFlags::DIRECTORY`
        // states the requirement to the kernel, which answers `ENOTDIR`; this arm has
        // to ask afterwards. The kind matches what POSIX returns, so a caller can
        // branch on one answer.
        let m = f.metadata().map_err(FsError::from_io)?;
        if !m.is_dir() {
            return Err(FsError::new(
                flux_fs::Code::IoError,
                std::io::Error::new(
                    std::io::ErrorKind::NotADirectory,
                    "a destination root must be a directory",
                ),
            ));
        }
        Ok(crate::StdDir::from_handle(f.into()))
    }
}

#[cfg(unix)]
pub(crate) fn perms_of(m: &std::fs::Metadata) -> Perms {
    use std::os::unix::fs::PermissionsExt;
    Perms::UnixMode(m.permissions().mode())
}

#[cfg(not(unix))]
pub(crate) fn perms_of(m: &std::fs::Metadata) -> Perms {
    Perms::ReadOnly(m.permissions().readonly())
}

/// Map std's `FileType` to ours. Symlink FIRST: on Windows std defines `is_dir()` as
/// `!is_symlink && is_directory`, so a junction or directory symlink is already
/// excluded there -- but testing symlink first makes that independent of std's
/// definition rather than reliant on it.
pub(crate) fn type_of(m: &std::fs::Metadata) -> flux_fs::FileType {
    let t = m.file_type();
    if t.is_symlink() {
        flux_fs::FileType::Symlink
    } else if t.is_dir() {
        flux_fs::FileType::Dir
    } else if t.is_file() {
        flux_fs::FileType::File
    } else {
        flux_fs::FileType::Other
    }
}

/// Unix identity costs NOTHING extra: `metadata` already calls `symlink_metadata`,
/// and `st_dev` / `st_ino` come off that same struct. No second syscall.
#[cfg(unix)]
fn identity_of(m: &std::fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    identity_of_raw(m.dev(), m.ino())
}

/// The §107 zero-inode rule, factored out so the path-based `identity_of` above
/// and the handle-based `metadata_from_stat` below share ONE copy of it rather
/// than risking two that drift.
///
/// §107: "The platform adapter must never treat `object_id == 0` as a valid
/// universal object identity."
#[cfg(unix)]
fn identity_of_raw(dev: u64, ino: u64) -> FileIdentity {
    let index = u128::from(ino);
    if index == 0 {
        return FileIdentity::Unavailable;
    }
    FileIdentity::Strong(ObjectId { volume: dev, index })
}

/// The handle-based twin of `FileSystem::metadata` above, for
/// `DirHandle::metadata` (§149.7): the caller already has a `statat` result --
/// taken relative to an open directory handle rather than by path -- and this
/// builds the same `Metadata` from it without a second, path-based stat.
///
/// Shares `identity_of_raw` for the §107 rule; the file-type mapping goes through
/// rustix's own `FileType::from_raw_mode` rather than reimplementing the
/// `S_IFMT` bit test a second time.
#[cfg(unix)]
pub(crate) fn metadata_from_stat(st: &rustix::fs::Stat) -> Metadata {
    use rustix::fs::FileType as RawFileType;
    // Field widths differ by platform: macOS has `st_mode: u16` and `st_dev: i32`,
    // Linux `u32` and `u64`, so each cast is a no-op on one and required on the
    // other. They are std's own casts (`MetadataExt::mode`/`dev`), and `dev` must
    // stay so: this identity is compared with the path-based one from
    // `identity_of`, and a different `st_dev` conversion would make one object
    // look like two.
    #[allow(clippy::unnecessary_cast)]
    let (mode, dev) = (st.st_mode as u32, st.st_dev as u64);
    Metadata {
        len: st.st_size as u64,
        file_type: match RawFileType::from_raw_mode(st.st_mode) {
            RawFileType::Directory => FileType::Dir,
            RawFileType::Symlink => FileType::Symlink,
            RawFileType::RegularFile => FileType::File,
            _ => FileType::Other,
        },
        permissions: Some(Perms::UnixMode(mode)),
        modified: mtime_from_stat(st),
        identity: identity_of_raw(dev, st.st_ino),
    }
}

/// `st_mtime`/`st_mtime_nsec` to `SystemTime`. POSIX keeps `st_*_nsec` a
/// non-negative FORWARD offset even when the seconds field is negative (a
/// timestamp before the epoch), so the two fields are combined as one signed
/// total before splitting back into a `Duration` either side of `UNIX_EPOCH`,
/// rather than risking a plain `Duration::new(negative_as_u64, nsec)` on the
/// pre-epoch side.
#[cfg(unix)]
fn mtime_from_stat(st: &rustix::fs::Stat) -> Option<SystemTime> {
    let total_nanos = i128::from(st.st_mtime) * 1_000_000_000 + i128::from(st.st_mtime_nsec);
    let abs = total_nanos.unsigned_abs();
    let d = Duration::new((abs / 1_000_000_000) as u64, (abs % 1_000_000_000) as u32);
    if total_nanos >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(d)
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(d)
    }
}

/// The identity half of `metadata`, reading an ALREADY-OPEN handle.
///
/// Split out from `identity_of(path)` so `metadata` opens once. The flags that
/// handle must carry are documented at its one call site above; this function
/// cannot enforce them, which is why it is private and takes a `&File` rather than
/// a path.
#[cfg(windows)]
pub(crate) fn identity_of_handle(file: &File) -> FileIdentity {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let mut info = FILE_ID_INFO { VolumeSerialNumber: 0, FileId: Default::default() };
    // SAFETY: `info` is a live, correctly sized `FILE_ID_INFO`, and the handle stays
    // open for the duration of the call because `file` outlives it.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as _,
            FileIdInfo,
            (&raw mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok == 0 {
        // Not every filesystem implements this info class.
        return FileIdentity::Unavailable;
    }

    let index = u128::from_le_bytes(info.FileId.Identifier);
    // §107: an id of zero is never a valid universal identity.
    if index == 0 {
        return FileIdentity::Unavailable;
    }
    FileIdentity::Strong(ObjectId { volume: info.VolumeSerialNumber, index })
}
