//! Single-file copy. The algorithm lives here once; platforms supply primitives.

use flux_fs::{
    Code, CopyOptions, DestinationRoot, DirHandle, Durability, FileHandle, FileIdentity, FileType,
    FsError, Metadata, MetadataFailure, MetadataItem, OperationId, Outcome, Preserve, Publish,
    Safety, temp_path,
};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::Path;

/// The code for a failure inside the streaming loop.
///
/// The loop IS the content copy, and the spec defines `COPY_FAILED` as "the content
/// copy of an independent file, or a canonical attempt, failed", so an unclassified
/// failure here is `COPY_FAILED` rather than the `IO_ERROR` catch-all. A failure the
/// platform layer could classify -- a full disk, a denial -- keeps that code, which is
/// more specific than either.
///
/// `classify` borrows the error rather than consuming it: rebuilding one from
/// `e.kind()` would discard `raw_os_error()`, and ENOSPC would stop reaching
/// `DISK_FULL` on the one path where a full disk actually appears.
fn copy_code(e: &std::io::Error) -> Code {
    match FsError::classify(e) {
        Code::IoError => Code::CopyFailed,
        classified => classified,
    }
}

/// The engine's error: a filesystem failure, plus anything the ENGINE knows that the
/// filesystem layer cannot.
///
/// `flux-fs` defines the portable filesystem contract, and a staging temporary is not
/// part of it -- `metadata` and `open_read` have no notion of one. So the leftover is
/// recorded here, at the boundary that owns the concept, rather than as a field every
/// `FsError` in the workspace would carry and never set.
///
/// `cause` is kept INTACT for the same reason it always was: this used to be reported by
/// replacing the source with `Error::other(format!(..))`, and MEASURED on Linux that
/// turned `classify` from `DiskFull` into `IoError`, because `Error::other` carries no
/// `raw_os_error()`.
#[derive(Debug)]
pub struct CopyError {
    /// The failure that stopped the copy.
    pub cause: FsError,
    /// A staging temporary that outlived the failure because it could not be removed,
    /// with the reason removal failed. STRUCTURED, not pre-rendered: a caller that wants
    /// to sweep it needs the path itself, not a path inside a sentence.
    pub leftover: Option<(std::path::PathBuf, std::io::Error)>,
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.cause)?;
        if let Some((path, why)) = &self.leftover {
            write!(f, "; temporary left at {} ({why})", path.display())?;
        }
        Ok(())
    }
}

/// Hand-written rather than derived: `Display` is already hand-written, and this keeps
/// the `source()` chain pointing at the REAL `FsError` so `anyhow`-style walkers and
/// `{:#}` formatting still reach the underlying `io::Error` and its `raw_os_error()`.
impl std::error::Error for CopyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

impl CopyError {
    fn new(cause: FsError) -> Self {
        CopyError { cause, leftover: None }
    }

    /// The spec code of the failure, which is what nearly every caller wants.
    pub fn code(&self) -> Code {
        self.cause.code
    }
}

// DELIBERATELY NO `impl From<FsError> for CopyError`.
//
// `?` on an `FsError` AFTER the staging temporary exists would return without removing
// it -- the one leak `discard` exists to prevent, which step 7 below has always had to
// guard with a comment asking a human to remember. Withholding the conversion makes that
// mistake fail to COMPILE instead.
//
// This is the guard the error-type split bought: while `copy_file` returned
// `Result<Outcome, FsError>`, `?` compiled by identity and no amount of care could stop
// it. The three sites that legitimately use `?` all run BEFORE the temporary exists, and
// say so explicitly.

/// Remove the temporary after a failure and build the error to return.
///
/// The removal is never discarded: if the temporary survives, its path goes into the
/// error, because a leftover the caller is never told about is a leak the user cannot
/// even find. Only called where the temporary is known to exist.
///
/// The leftover is recorded RELATIVE to `parent` -- the only frame `copy_file_at`
/// has. `copy_file` joins it onto the parent path it resolved, so its callers see
/// the same full path as before.
fn discard<D: DirHandle>(
    parent: &D,
    temp: &OsStr,
    code: Code,
    source: std::io::Error,
) -> CopyError {
    match parent.remove_file(temp) {
        Ok(()) => CopyError::new(FsError::new(code, source)),
        // NotFound means it is already gone -- something else removed it, or it was
        // never created. Reporting a leftover here would send someone hunting a file
        // that does not exist.
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => {
            CopyError::new(FsError::new(code, source))
        }
        // Carry the removal's own error too, in `leftover` and NOT over `source`:
        // wrapping the primary failure in `Error::other(format!(..))` destroyed
        // `raw_os_error()` and, MEASURED on Linux, turned `classify` from `DiskFull`
        // into `IoError`.
        Err(why) => CopyError {
            cause: FsError::new(code, source),
            leftover: Some((std::path::PathBuf::from(temp), why.source)),
        },
    }
}

/// `<name>.flux-partial.<operation-id>` (§18.1), as a single component for a handle.
/// Built by `temp_path` so the normative name has exactly one definition.
fn temp_name(name: &OsStr, id: &OperationId) -> OsString {
    temp_path(Path::new(name), id).into_os_string()
}

/// Split `dst` into the directory to open and the one component to write there.
///
/// A path with no file name -- a root, or one ending in `..` -- names nothing to
/// write, so it is a destination error. A bare name's parent is the EMPTY path,
/// which names the working directory but which `destination_root` cannot open, so
/// it becomes `.`.
fn split_destination(dst: &Path) -> std::result::Result<(&Path, &OsStr), CopyError> {
    let Some(name) = dst.file_name() else {
        return Err(CopyError::new(FsError::new(
            Code::DestinationError,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "the destination names no file"),
        )));
    };
    let parent = match dst.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    Ok((parent, name))
}

/// Step 2a (§129, foundational invariant 22): refuse a destination that IS the source.
///
/// Evaluated IN ORDER, and the order is load-bearing: `NotFound` short-circuits first,
/// because a destination that does not exist has no identity to alias, so neither
/// `Safety` setting may refuse it. Every other row concerns a destination that exists.
///
/// | destination            | result                                              |
/// |------------------------|-----------------------------------------------------|
/// | `NotFound`             | proceed, not degraded                               |
/// | other stat failure     | degraded `Unavailable` (strict: refuse)             |
/// | a directory            | refuse, naming it                                   |
/// | both `Strong`, equal   | refuse                                              |
/// | both `Strong`, differ  | proceed                                             |
/// | either side not Strong | degraded, the weaker side (strict: refuse)          |
///
/// The stat goes through the HANDLE and never follows a link, so a symlink at the
/// destination is judged as the NAME being replaced, never as its target.
///
/// Residual, stated rather than hidden: this stats at Step 2a and publishes at Step 7,
/// so a destination that becomes an alias in between is not caught. The harm is
/// bounded to a broken hardlink, not lost data -- the published bytes were read from
/// the source before the destination was touched.
fn identity_gate<D: DirHandle>(
    parent: &D,
    name: &OsStr,
    src: &Metadata,
    safety: Safety,
) -> std::result::Result<Option<FileIdentity>, CopyError> {
    let refuse = |why: &'static str| {
        CopyError::new(FsError::new(Code::SafetyRejected, std::io::Error::other(why)))
    };
    let degraded = |weaker: FileIdentity| match safety {
        Safety::Default => Ok(Some(weaker)),
        Safety::Strict => Err(refuse(
            "the destination exists and its identity cannot be compared with full confidence",
        )),
    };
    match parent.metadata(name) {
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => degraded(FileIdentity::Unavailable),
        Ok(m) if m.file_type == FileType::Dir => Err(refuse("the destination is a directory")),
        Ok(m) => match (src.identity, m.identity) {
            (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b => {
                Err(refuse("the destination is the source itself, by identity"))
            }
            (FileIdentity::Strong(_), FileIdentity::Strong(_)) => Ok(None),
            (s, d) => degraded(weaker(s, d)),
        },
    }
}

/// The less trustworthy of two identities, for the warning's aggregation key.
/// `Unavailable` is weaker than `Weak`; between two `Weak`, the DESTINATION's volume
/// keys it, because that is the side the user did not name (walker design,
/// "Stand-downs").
fn weaker(src: FileIdentity, dst: FileIdentity) -> FileIdentity {
    match (src, dst) {
        (FileIdentity::Unavailable, _) | (_, FileIdentity::Unavailable) => {
            FileIdentity::Unavailable
        }
        (_, d @ FileIdentity::Weak(_)) => d,
        (s, _) => s,
    }
}

/// Copy one file by path. A wrapper: it resolves the destination's PARENT once, as a
/// directory handle, and hands the algorithm to `copy_file_at`, so a single-file copy
/// writes through a handle exactly as a tree copy does (§149.7).
pub fn copy_file<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    dst: &Path,
    opts: &CopyOptions,
) -> std::result::Result<Outcome, CopyError> {
    // 0. refuse a LEXICAL self-copy BEFORE touching the filesystem. This must precede
    //    every call below, because `a_self_copy_is_refused_before_anything_is_touched`
    //    asserts the recorded call list is empty. The IDENTITY half of the same
    //    refusal -- `flux copy a b` where `b` is `a` by another name -- is Step 2a in
    //    `copy_file_at`, which has to stat the destination and so cannot keep this
    //    step's promise; the two coexist rather than one replacing the other.
    if src == dst {
        return Err(CopyError::new(FsError::new(
            Code::SafetyRejected,
            std::io::Error::other("source and destination are the same path"),
        )));
    }
    let (parent_path, name) = split_destination(dst)?;
    // The one path-based call on the write side. §149.7 exempts resolving the
    // destination itself: it may follow links, because the user named it.
    let parent = fs.destination_root(parent_path).map_err(CopyError::new)?;
    copy_file_at(fs, src, &parent, name, opts).map_err(|mut e| {
        // Rebuilt from `dst`, NOT joined onto `parent_path`: for a bare name the
        // opened parent is a synthesized `.`, and joining would report
        // `./b.flux-partial.x` where the user's own spelling gives `b.flux-partial.x`.
        if let Some((path, _)) = e.leftover.as_mut() {
            *path = dst.with_file_name(&*path);
        }
        e
    })
}

/// Copy `src` to `name` inside `parent`, writing ONLY through `parent`.
///
/// Nothing below re-resolves a destination path: the staging temporary, the
/// metadata, the publish and the cleanup all go through the handle, so a parent
/// swapped for a link after it was opened cannot redirect the write (item 114).
pub fn copy_file_at<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    parent: &F::Dir,
    name: &OsStr,
    opts: &CopyOptions,
) -> std::result::Result<Outcome, CopyError> {
    let temp = temp_name(name, &opts.operation_id);

    // 1. this invocation's own leftover, if any (§18.1). A no-op in practice: the
    //    operation id is generated per invocation and never persisted, so nothing from
    //    an earlier run carries this name. It stays because §18.1 asks the id to be
    //    "deterministic enough for discovery", and a derivable id makes this live.
    let _ = parent.remove_file(&temp);

    // 2. source, captured for the step-7 re-check
    // Safe to `?`: nothing has been created yet, so there is nothing to leak.
    let src_meta = fs.metadata(src).map_err(CopyError::new)?;
    if src_meta.file_type != FileType::File {
        return Err(CopyError::new(FsError::new(
            Code::SpecialFileUnsupported,
            std::io::Error::other("not a regular file"),
        )));
    }

    // 2a. the destination must not BE the source. Before the temporary exists, so a
    //     refusal changes nothing on disk.
    let identity_degraded = identity_gate(parent, name, &src_meta, opts.safety)?;

    let mut reader = fs.open_read(src).map_err(CopyError::new)?;

    // 3. exclusive create (FS-1)
    // Still safe: if this FAILS, this call is precisely what did not create the
    // temporary, so there is nothing of ours on disk.
    let mut writer = parent.create_new(&temp).map_err(CopyError::new)?;

    // 4. stream
    let mut bytes_copied = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match std::io::Read::read(&mut reader, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(discard(parent, &temp, copy_code(&e), e)),
        };
        if let Err(e) = writer.write_all(&buf[..n]) {
            return Err(discard(parent, &temp, copy_code(&e), e));
        }
        bytes_copied += n as u64;
    }

    // 5. durability
    if opts.durability == Durability::Strict
        && let Err(e) = writer.sync_all()
    {
        return Err(discard(parent, &temp, Code::StrictDurabilityUnavailable, e.source));
    }

    // 6. metadata, on the temporary, BEFORE publication (§44.1)
    let mut metadata_failures = Vec::new();

    if opts.preserve_times != Preserve::Off
        && let Err(e) = fs.set_times(&writer, src_meta.modified)
    {
        if opts.preserve_times == Preserve::Strict {
            return Err(discard(parent, &temp, Code::MetadataApplyFailed, e.source));
        }
        metadata_failures.push(MetadataFailure { item: MetadataItem::Times, error: e });
    }

    if opts.preserve_permissions != Preserve::Off
        && let Err(e) = fs.set_permissions(&writer, src_meta.permissions)
    {
        if opts.preserve_permissions == Preserve::Strict {
            return Err(discard(parent, &temp, Code::MetadataApplyFailed, e.source));
        }
        metadata_failures.push(MetadataFailure { item: MetadataItem::Permissions, error: e });
    }

    // 7. the source must not have changed under us (Section 33), then publish.
    //    Note the `match` rather than `?`: a `?` here would return with the temporary
    //    still on disk, which is the one leak the `discard` helper exists to prevent.
    let now = match fs.metadata(src) {
        Ok(m) => m,
        // Gone is the most complete form of "changed under us", and the taxonomy has
        // a code for that. Reporting IO_ERROR here would hide it among unrelated
        // failures.
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => {
            let gone = std::io::Error::other("source disappeared during the copy");
            return Err(discard(parent, &temp, Code::SourceChanged, gone));
        }
        Err(e) => return Err(discard(parent, &temp, e.code, e.source)),
    };
    if now.len != src_meta.len || now.modified != src_meta.modified {
        let changed = std::io::Error::other("source changed");
        return Err(discard(parent, &temp, Code::SourceChanged, changed));
    }

    let published = match opts.publish {
        Publish::Replace => parent.rename_replace(&temp, parent, name),
        Publish::NoReplace => parent.rename_no_replace(&temp, parent, name),
    };
    if let Err(e) = published {
        // Keep whatever the platform layer mapped. A publish failure is NOT the content
        // copy -- that already succeeded -- so it must not be relabelled COPY_FAILED,
        // and an unclassified one stays IO_ERROR, the declared catch-all.
        return Err(discard(parent, &temp, e.code, e.source));
    }

    // A successful rename consumed the temporary; there is nothing left to remove.

    Ok(Outcome { bytes_copied, metadata_failures, identity_degraded })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use flux_fs::{Durability, FileSystem, OperationId, Preserve, Publish};

    fn opts() -> CopyOptions {
        CopyOptions {
            preserve_times: Preserve::Default,
            preserve_permissions: Preserve::Default,
            durability: Durability::Normal,
            publish: Publish::Replace,
            safety: flux_fs::Safety::Default,
            operation_id: OperationId::new("op1"),
        }
    }

    #[test]
    fn a_clean_copy_publishes_and_reports_nothing() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.bytes_copied, 5);
        assert!(out.metadata_failures.is_empty());
        assert_eq!(out.identity_degraded, None);
        assert!(fs.called("rename_replace"));
        // The CONTENT, not just the call. Asserting only that a rename happened
        // passes just as well against a copy that published nothing.
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
        assert!(!fs.exists("/dst.flux-partial.op1"));
    }

    #[test]
    fn metadata_is_applied_before_publication() {
        // The whole design in one assertion: §44.1 can only be honoured if the
        // metadata calls precede the rename.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        let calls = fs.calls();
        let meta = calls.iter().position(|c| c.starts_with("set_times")).expect("set_times");
        let publish = calls.iter().position(|c| c.starts_with("rename_replace")).expect("rename");
        assert!(meta < publish, "metadata must precede publication; got {calls:?}");
    }

    #[test]
    fn the_temporary_carries_the_operation_id() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();
        assert!(fs.called("create_new(/dst.flux-partial.op1)"), "{:?}", fs.calls());
    }

    #[test]
    fn a_strict_metadata_failure_prevents_publication() {
        // §44.1, the rule this whole design exists to honour.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("set_times", flux_fs::Code::PermissionDenied);

        let mut o = opts();
        o.preserve_times = Preserve::Strict;
        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::MetadataApplyFailed);
        assert!(!fs.called("rename_replace"), "must not publish; got {:?}", fs.calls());
        assert!(!fs.exists("/dst"));
        assert!(!fs.exists("/dst.flux-partial.op1"), "temporary must be removed");
    }

    #[test]
    fn a_default_metadata_failure_still_publishes_and_reports() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("set_times", flux_fs::Code::PermissionDenied);

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.metadata_failures.len(), 1);
        assert_eq!(out.metadata_failures[0].item, flux_fs::MetadataItem::Times);
        assert!(fs.called("rename_replace"), "best-effort still publishes");
        assert!(fs.exists("/dst"));
    }

    #[test]
    fn a_full_disk_does_not_publish_and_leaves_no_temporary() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("create_new", flux_fs::Code::DiskFull);

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::DiskFull);
        assert!(!fs.called("rename_replace"));
        assert!(!fs.exists("/dst.flux-partial.op1"));
    }

    #[test]
    fn a_denied_publish_keeps_the_denial_code_and_removes_the_temporary() {
        // The taxonomy says PERMISSION_DENIED is "the operating system denied access"
        // and COPY_FAILED is "the content copy failed". At the publish step the content
        // copy has already succeeded, so a denial here is a denial, not a copy failure.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("rename_replace", flux_fs::Code::PermissionDenied);

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::PermissionDenied);
        assert!(!fs.exists("/dst.flux-partial.op1"));
    }

    #[test]
    fn an_unclassified_publish_failure_stays_io_error() {
        // The publish step is not the content copy, so an unclassified failure here is
        // the IO_ERROR catch-all rather than COPY_FAILED.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("rename_replace", flux_fs::Code::IoError);

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::IoError);
        assert!(!fs.exists("/dst.flux-partial.op1"));
    }

    #[test]
    fn an_unclassified_write_failure_is_copy_failed() {
        // COPY_FAILED's reachable producer, and the spec's own definition of it.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail_write(std::io::Error::other("the device hiccupped"));

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::CopyFailed);
        assert!(!fs.called("rename_replace"), "must not publish");
        assert!(!fs.exists("/dst.flux-partial.op1"));
    }

    #[test]
    fn a_temporary_that_cannot_be_removed_is_named_in_the_error() {
        // `discard` must not swallow a failed cleanup: a leftover nobody is told
        // about is one the user cannot even find.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("rename_replace", flux_fs::Code::PermissionDenied);
        fs.fail_always("remove_file", flux_fs::Code::PermissionDenied);

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        // The REQUIREMENT is unchanged -- a leftover the caller is never told about is
        // one the user cannot find -- but it is no longer met by overwriting `source`.
        // That shape destroyed `raw_os_error()` and with it the DiskFull verdict
        // (measured on Linux: classify went DiskFull -> IoError), so the leftover moved
        // to `temp_left` and `source` now stays intact. The assertion follows the
        // requirement, not the old mechanism.
        let (path, why) = err.leftover.as_ref().expect("the leftover must be reported");
        assert_eq!(path, Path::new("/dst.flux-partial.op1"), "the PATH, not a sentence");
        assert!(why.to_string().contains("injected"), "must carry why removal failed; got {why}");
        assert!(err.to_string().contains("/dst.flux-partial.op1"), "and it must be visible: {err}");
    }

    #[test]
    fn a_missing_source_creates_nothing() {
        let fs = FaultFs::new();
        let err = copy_file(&fs, Path::new("/nope"), Path::new("/dst"), &opts()).unwrap_err();
        assert!(!fs.called("create_new"), "nothing is created before the source is checked");
        assert_eq!(err.code(), flux_fs::Code::IoError);
    }

    #[test]
    fn a_self_copy_is_refused_before_anything_is_touched() {
        let fs = FaultFs::new();
        fs.write_file("/a", b"hello");
        let err = copy_file(&fs, Path::new("/a"), Path::new("/a"), &opts()).unwrap_err();
        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert!(fs.calls().is_empty(), "refuse before any call; got {:?}", fs.calls());
    }

    #[test]
    fn a_source_that_changes_mid_copy_is_not_published() {
        // Section 33: the result is not published.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        // The fake re-reads metadata at step 6; growing the file between the two
        // reads is what a real concurrent writer does.
        fs.grow_on_second_metadata("/src", b" world");

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::SourceChanged);
        assert!(!fs.called("rename_replace"), "must not publish a stale copy");
        assert!(!fs.exists("/dst.flux-partial.op1"));
    }

    #[test]
    fn a_source_deleted_mid_copy_reports_source_changed() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.vanish_on_second_metadata("/src");

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::SourceChanged);
        assert!(!fs.called("rename_replace"), "must not publish");
        assert!(!fs.exists("/dst.flux-partial.op1"));
    }

    #[test]
    fn the_sources_metadata_survives_publication() {
        // §44.1's whole point: metadata is applied to the temporary and must still be
        // on the file after the rename. Asserting the CALLS happened does not show
        // that, because a rename that dropped the metadata would call them just the
        // same.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.set_file_perms("/src", flux_fs::Perms::UnixMode(0o640));

        copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(fs.permissions("/dst"), Some(flux_fs::Perms::UnixMode(0o640)));
    }

    #[test]
    fn preserve_off_does_not_touch_metadata_at_all() {
        // The third state earns its keep here. `Off` is not a lenient `Default`, it is
        // "do not attempt", and nothing else in the suite tells those apart: an
        // implementation that treated `Off` as `Default` would pass every other test.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let mut o = opts();
        o.preserve_times = Preserve::Off;
        o.preserve_permissions = Preserve::Off;

        copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap();

        assert!(!fs.called("set_times"), "Off must not attempt it; got {:?}", fs.calls());
        assert!(!fs.called("set_permissions"), "Off must not attempt it; got {:?}", fs.calls());
        assert!(fs.called("rename_replace"), "the copy itself still publishes");
    }

    #[test]
    fn a_special_file_source_is_refused_before_anything_is_created() {
        // SPECIAL_FILE_UNSUPPORTED's producer. The spec's acceptance criteria require
        // every variant to have a test that produces it.
        let fs = FaultFs::new();
        fs.add_special("/dev/null");

        let err = copy_file(&fs, Path::new("/dev/null"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::SpecialFileUnsupported);
        assert!(!fs.called("open_read"), "must refuse before opening it");
        assert!(!fs.called("create_new"), "and before creating anything");
    }

    #[test]
    fn a_leftover_from_this_invocation_is_swept_first() {
        // Step 1 exists to remove this run's own leftover. Delete that line and this
        // is the only test that notices.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.write_file("/dst.flux-partial.op1", b"junk from a previous attempt");

        copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        let calls = fs.calls();
        assert_eq!(
            calls.first().map(String::as_str),
            Some("remove_file(/dst.flux-partial.op1)"),
            "the sweep must come first; got {calls:?}"
        );
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn normal_durability_does_not_sync_and_strict_does() {
        // Two runs, because the difference between them IS the option's meaning.
        let lax = FaultFs::new();
        lax.write_file("/src", b"hello");
        copy_file(&lax, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();
        assert!(!lax.called("sync_all"), "Normal must not sync; got {:?}", lax.calls());

        let strict = FaultFs::new();
        strict.write_file("/src", b"hello");
        let mut o = opts();
        o.durability = Durability::Strict;
        copy_file(&strict, Path::new("/src"), Path::new("/dst"), &o).unwrap();
        assert!(strict.called("sync_all"), "Strict must sync; got {:?}", strict.calls());
    }

    #[test]
    fn a_source_larger_than_the_buffer_copies_every_byte() {
        // Every other test uses five bytes, so the read loop has only ever run once.
        // The buffer is 64 KiB; this forces several iterations and a short final read.
        let fs = FaultFs::new();
        let big: Vec<u8> = (0..(64 * 1024 * 2 + 7)).map(|i| (i % 251) as u8).collect();
        fs.write_file("/src", &big);

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.bytes_copied, big.len() as u64);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&big[..]));
    }

    #[test]
    fn a_zero_byte_source_copies_and_still_gets_metadata() {
        // Named in TODO.md's integration-test item, so it gets a test of its own.
        let fs = FaultFs::new();
        fs.write_file("/src", b"");
        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.bytes_copied, 0);
        assert!(fs.called("set_times"), "metadata still applies to an empty file");
        assert!(fs.called("rename_replace"));
    }

    #[test]
    fn no_replace_refuses_an_existing_target_and_cleans_up() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.write_file("/dst", b"existing");

        let mut o = opts();
        o.publish = Publish::NoReplace;
        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap_err();

        // A target that already exists is a destination-side refusal, not a
        // content-copy failure: the IO_ERROR catch-all until a later cut adds
        // DESTINATION_ERROR.
        assert_eq!(err.code(), flux_fs::Code::IoError);
        assert!(!fs.exists("/dst.flux-partial.op1"), "temporary must be removed");
    }

    #[test]
    fn two_default_failures_produce_two_entries_and_still_publish() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("set_times", flux_fs::Code::PermissionDenied);
        fs.fail("set_permissions", flux_fs::Code::PermissionDenied);

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.metadata_failures.len(), 2);
        assert!(fs.called("rename_replace"));
    }

    #[test]
    fn strict_durability_failure_is_its_own_code() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("sync_all", flux_fs::Code::IoError);

        let mut o = opts();
        o.durability = Durability::Strict;
        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::StrictDurabilityUnavailable);
        assert!(!fs.called("rename_replace"));
    }

    #[test]
    fn a_temporary_already_gone_is_not_reported_as_left_behind() {
        // `discard`'s NotFound arm. If the temporary is already gone, saying "temporary
        // left at /tmp" sends an operator hunting a file that does not exist.
        //
        // MEASURED by `cargo mutants` before this test existed: replacing that guard
        // with `false` was NOT caught, because `fail` could only inject
        // `ErrorKind::Other` and no test could express a NotFound removal.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("rename_replace", flux_fs::Code::PermissionDenied);
        // The SECOND remove_file: the first is step 1's leftover sweep, and a one-shot
        // fault aimed at "remove_file" is eaten there, leaving this test passing
        // vacuously. Found by watching the sibling test below fail for that reason.
        fs.fail_nth("remove_file", 2, flux_fs::Code::IoError, std::io::ErrorKind::NotFound);

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(
            err.code(),
            flux_fs::Code::PermissionDenied,
            "the publish failure is the verdict"
        );
        assert!(err.leftover.is_none(), "already gone is not left behind: {:?}", err.leftover);
    }

    #[test]
    fn a_left_behind_temporary_does_not_destroy_the_primary_error() {
        // The other arm: removal genuinely failed, so the leftover IS reported -- but in
        // `temp_left`, never by wrapping `source`. MEASURED on Linux: wrapping turned
        // `classify` from DiskFull into IoError, because `Error::other` carries no
        // `raw_os_error()`.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("rename_replace", flux_fs::Code::DiskFull);
        fs.fail_nth("remove_file", 2, flux_fs::Code::IoError, std::io::ErrorKind::Other);

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::DiskFull);
        assert!(err.leftover.is_some(), "a genuine removal failure must be reported");
        assert!(err.to_string().contains("temporary left at"), "{err}");
    }

    #[test]
    fn a_step_seven_failure_that_is_not_notfound_keeps_its_own_code() {
        // `copy_file`'s step-7 guard. Only a VANISHED source is SOURCE_CHANGED; any other
        // failure of the re-check must keep the code the platform layer gave it, or it is
        // hidden among unrelated failures.
        //
        // MEASURED by `cargo mutants` before this test existed: replacing that guard with
        // `true` was NOT caught. `metadata` is called three times -- the source, the
        // destination at the Step 2a gate, and the source again at step 7 -- so the
        // fault targets the third.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail_nth(
            "metadata",
            3,
            flux_fs::Code::PermissionDenied,
            std::io::ErrorKind::PermissionDenied,
        );

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::PermissionDenied, "not SOURCE_CHANGED");
        assert!(!fs.called("rename_replace"), "must not publish");
        assert!(
            fs.called("open_read"),
            "the fault must land on step 7, after the gate let the copy start"
        );
    }

    #[test]
    fn the_source_chain_still_reaches_the_os_error() {
        // The design consult flagged this as the property that must not break: if
        // `source()` returned `None`, `{:#}` walkers and anyhow-style tools would stop
        // at `CopyError` and never reach `raw_os_error()` -- which is exactly what the
        // DiskFull arm of `classify` keys on, and exactly the loss that made `discard`
        // a defect in the first place.
        //
        // MEASURED by `cargo mutants`: replacing that impl's body with `None` was NOT
        // caught until this test existed. The fix for the original defect had quietly
        // introduced an untested line of its own.
        use std::error::Error as _;

        let err = CopyError::new(FsError::new(Code::IoError, std::io::Error::from_raw_os_error(5)));

        let cause = err.source().expect("CopyError must expose its cause");
        let fs_err = cause.downcast_ref::<FsError>().expect("the cause is an FsError");
        assert_eq!(fs_err.source.raw_os_error(), Some(5), "the OS code must survive the chain");
    }

    #[test]
    fn split_destination_separates_parent_and_name() {
        let ok = |d: &str| {
            let (p, n) = split_destination(Path::new(d)).unwrap();
            (p.to_path_buf(), n.to_os_string())
        };
        assert_eq!(ok("/d/b"), (Path::new("/d").into(), "b".into()));
        // A bare name's parent is the EMPTY path; `destination_root("")` would fail.
        assert_eq!(ok("b"), (Path::new(".").into(), "b".into()));
        assert_eq!(ok("/b"), (Path::new("/").into(), "b".into()));
    }

    #[test]
    fn a_destination_naming_no_file_is_refused_before_any_call() {
        for dst in ["/", "/d/.."] {
            let fs = FaultFs::new();
            fs.write_file("/src", b"hello");
            let err = copy_file(&fs, Path::new("/src"), Path::new(dst), &opts()).unwrap_err();
            assert_eq!(err.code(), flux_fs::Code::DestinationError, "{dst}");
            assert!(fs.calls().is_empty(), "{dst}: refuse before any call; got {:?}", fs.calls());
        }
    }

    #[test]
    fn copy_file_at_writes_into_the_directory_it_is_given() {
        use flux_fs::DestinationRoot;
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();

        let out =
            copy_file_at(&fs, Path::new("/src"), &d, std::ffi::OsStr::new("f"), &opts()).unwrap();

        assert_eq!(out.bytes_copied, 5);
        assert_eq!(fs.read_file("/d/f").as_deref(), Some(&b"hello"[..]));
        assert!(!fs.exists("/d/f.flux-partial.op1"));
    }

    #[test]
    fn copy_file_at_reports_a_leftover_relative_to_its_directory() {
        use flux_fs::DestinationRoot;
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        fs.fail("rename_replace", flux_fs::Code::PermissionDenied);
        fs.fail_always("remove_file", flux_fs::Code::PermissionDenied);

        let err = copy_file_at(&fs, Path::new("/src"), &d, std::ffi::OsStr::new("f"), &opts())
            .unwrap_err();

        let (path, _) = err.leftover.as_ref().expect("the leftover must be reported");
        assert_eq!(path, Path::new("f.flux-partial.op1"), "relative to the directory handle");
    }

    fn strong(index: u128) -> flux_fs::FileIdentity {
        flux_fs::FileIdentity::Strong(flux_fs::ObjectId { volume: 1, index })
    }
    fn weak(volume: u64) -> flux_fs::FileIdentity {
        flux_fs::FileIdentity::Weak(flux_fs::ObjectId { volume, index: 1 })
    }
    fn strict() -> CopyOptions {
        let mut o = opts();
        o.safety = flux_fs::Safety::Strict;
        o
    }

    #[test]
    fn a_destination_that_is_the_source_by_identity_is_refused_before_anything_is_created() {
        // The hardlink case (§129, invariant 22): two names, one object. Nothing may
        // be created, and the destination -- which IS the source -- stays intact.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.write_file("/dst", b"hello");
        fs.set_identity("/src", strong(4242));
        fs.set_identity("/dst", strong(4242));

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert!(!fs.called("open_read"), "refused before the source is opened");
        assert!(!fs.called("create_new"), "refused before the temporary exists");
        assert!(!fs.called("rename_replace"));
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn a_fresh_destination_passes_silently_even_under_strict_with_a_weak_source() {
        // NotFound short-circuits FIRST: no object, so no alias. Without the ordering,
        // strict would refuse every copy onto removable media (a weak source).
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.set_identity("/src", weak(9));

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &strict()).unwrap();

        assert_eq!(out.identity_degraded, None, "absent is not a degradation");
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn a_directory_at_the_destination_is_refused_with_the_real_reason() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.create_dir(Path::new("/dst")).unwrap();

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert!(err.to_string().contains("directory"), "names the reason: {err}");
        assert!(!fs.called("create_new"));
    }

    #[test]
    fn a_distinct_strong_destination_is_replaced_without_complaint() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"old");

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &strict()).unwrap();

        assert_eq!(out.identity_degraded, None);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"new"[..]));
    }

    #[test]
    fn a_weak_destination_degrades_and_reports_the_weaker_side() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"old");
        fs.set_identity("/dst", weak(7));

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.identity_degraded, Some(weak(7)));
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"new"[..]), "default still copies");
    }

    #[test]
    fn a_weak_destination_is_refused_under_strict_and_left_untouched() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"old");
        fs.set_identity("/dst", weak(7));

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &strict()).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert!(!fs.called("create_new"));
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"old"[..]));
    }

    #[test]
    fn an_unavailable_side_degrades_to_the_unavailable_bucket() {
        // Unavailable outranks Weak as the weaker side: it names no volume at all.
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"old");
        fs.set_identity("/src", flux_fs::FileIdentity::Unavailable);
        fs.set_identity("/dst", weak(7));

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.identity_degraded, Some(flux_fs::FileIdentity::Unavailable));
    }

    #[test]
    fn a_destination_that_cannot_be_inspected_is_a_degraded_comparison() {
        // A stat that fails for a reason other than NotFound is a comparison that did
        // not happen -- degrade (Unavailable) by default, refuse under strict.
        let lax = FaultFs::new();
        lax.write_file("/src", b"new");
        lax.fail_nth(
            "metadata",
            2,
            flux_fs::Code::PermissionDenied,
            std::io::ErrorKind::PermissionDenied,
        );
        let out = copy_file(&lax, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();
        assert_eq!(out.identity_degraded, Some(flux_fs::FileIdentity::Unavailable));

        let tight = FaultFs::new();
        tight.write_file("/src", b"new");
        tight.fail_nth(
            "metadata",
            2,
            flux_fs::Code::PermissionDenied,
            std::io::ErrorKind::PermissionDenied,
        );
        let err = copy_file(&tight, Path::new("/src"), Path::new("/dst"), &strict()).unwrap_err();
        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert!(!tight.called("create_new"));
    }

    #[test]
    fn a_symlink_at_the_destination_is_judged_as_the_name_and_replaced() {
        // The gate stats the NAME (DirHandle::metadata never follows): a link is its
        // own object, distinct from the source, so it is replaced as intended.
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.add_symlink("/dst");

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.identity_degraded, None);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"new"[..]));
    }
}
