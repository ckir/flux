// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Single-file copy. The algorithm lives here once; platforms supply primitives.

use crate::checkpoint::{
    Discard, FileSink, Gate, Hook, ResumeVerify, Start, Verdict, decide, decide_full, mtime_pair,
};
use flux_fs::{
    Code, CopyOptions, DestinationRoot, DirHandle, Durability, FileHandle, FileIdentity, FileType,
    FsError, Metadata, MetadataFailure, MetadataItem, OperationId, Outcome, Preserve, Publish,
    Safety, VerifyLevel, temp_path,
};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

/// Where in `copy_file_at` (or its `copy_file` wrapper) a copy failed. A caller that
/// must tell a failure at the exclusive create from one at the publish - both can say
/// `AlreadyExists` - reads this instead of guessing from the error kind (cut 4b, F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyStep {
    /// Resolving the destination: the lexical Step 0 refusal, splitting the path,
    /// opening the parent. Also any failure that belongs to no copy step.
    Resolve,
    /// Step 2: the source's metadata, its type, opening it.
    Source,
    /// Step 2a: the destination identity gate.
    Gate,
    /// Step 3: the exclusive create of the staging temporary.
    Create,
    /// Step 4: streaming the bytes.
    Stream,
    /// Step 5: strict durability.
    Durability,
    /// Step 6: applying metadata under `Preserve::Strict`.
    Metadata,
    /// Step 7: the source re-check.
    Recheck,
    /// Step 7: the publishing rename.
    Publish,
    /// Cut 7b: refreshing the destination lock's heartbeat (§101). Its failure stops the copy at that point, and a
    /// tree's whole walk.
    Heartbeat,
    /// Cut 8b: the claim made before the temporary is created.
    Claim,
    /// Cut 9f-1: a checkpoint or its writer failed.
    Checkpoint,
    /// Cut 13: the copy-side verification (the re-read of the temporary or of the source).
    Verify,
}

/// Cut 8b: runs after the identity gate and the policy decision and before the temporary is created. Its `Err` stops
/// the copy there, with nothing created.
pub type BeforeCreate<'g> = dyn Fn() -> std::result::Result<(), CopyError> + 'g;

/// A `BeforeCreate` that always passes: the lockless copy and the tree (until its claims land) make no claim.
pub(crate) fn no_before_create() -> std::result::Result<(), CopyError> {
    Ok(())
}

/// Cut 9c: the object about to be published, as `copy_file_guarded` hands it to `before_publish`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishIntent {
    pub temp: std::ffi::OsString,
    pub identity: FileIdentity,
}

/// Cut 9c: runs once per copy that reaches the publish, after the source recheck and BEFORE the final heartbeat and guard.
/// Its `Err` stops the copy there: the temporary is discarded (as any pre-rename failure) and the error is returned as is.
pub type BeforePublish<'g> = dyn Fn(&PublishIntent) -> std::result::Result<(), CopyError> + 'g;

/// A `BeforePublish` that always passes: every caller until the commit-recovery note lands.
pub(crate) fn no_before_publish(_: &PublishIntent) -> std::result::Result<(), CopyError> {
    Ok(())
}

/// What the Step 2a identity gate read: the weaker identity when the comparison degraded, and the destination's
/// metadata (`None` when it is absent), so the policy decision needs no second stat.
pub(crate) struct GateRead {
    pub degraded: Option<FileIdentity>,
    pub existing: Option<Metadata>,
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
    /// Which step failed. Set where the failure happens; see `CopyStep`.
    pub step: CopyStep,
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
    pub(crate) fn at(step: CopyStep, cause: FsError) -> Self {
        CopyError { cause, leftover: None, step }
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

/// §99's check before a destination mutation (cut 7a Part 3b): `Ok` while the run still owns the destination's lock.
/// The run's guard fails with `Code::TargetLockBusy`, and the copy then makes no further mutation. A copy that holds no
/// lock passes `&unguarded`.
pub type Guard<'g> = dyn Fn() -> flux_fs::Result<()> + 'g;

/// The largest read buffer of the copy loop. Measured 2026-10 on Linux ext4 (one 4 GiB file): pooled over 18 runs per
/// side the 64 KiB buffer's median wall time was about 33% higher than 256 KiB's (7.02 s versus 5.28 s), with slow
/// outliers (7 of 18 runs over 8 s versus none); 1 MiB and 4 MiB were no better. The buffer is allocated per file, so
/// a larger fixed size would also cost every small copy a zero-filled allocation, and multiply memory under parallel
/// copy.
pub(crate) const COPY_BUF_MAX: usize = 256 * 1024;

/// The smallest read buffer: an empty or tiny source still needs a usable buffer. The source can grow while it is
/// copied (the post-copy re-check catches that), so the buffer is never sized to a length of 0.
pub(crate) const COPY_BUF_MIN: usize = 4 * 1024;

/// The copy loop's buffer length for a source of `src_len` bytes: the length clamped into
/// `COPY_BUF_MIN..=COPY_BUF_MAX`. Sized from the source because the buffer is allocated per file, so small files must
/// not pay for a large one. The clamp is done in `u64` before the cast, so it is exact on 32-bit targets.
pub(crate) fn copy_buffer_len(src_len: u64) -> usize {
    src_len.clamp(COPY_BUF_MIN as u64, COPY_BUF_MAX as u64) as usize
}

/// The guard of a copy that holds no lock.
pub(crate) fn unguarded() -> flux_fs::Result<()> {
    Ok(())
}

/// §101's heartbeat (cut 7b): called before every guarded destination mutation and after every chunk written (at
/// most `COPY_BUF_MAX` bytes). The run refreshes its lock record there once the interval has passed; its failure
/// stops the copy at `CopyStep::Heartbeat`. A copy that holds no lock passes `&no_heartbeat`.
pub type Heartbeat<'g> = dyn Fn() -> flux_fs::Result<()> + 'g;

/// The heartbeat of a copy that holds no lock.
pub(crate) fn no_heartbeat() -> flux_fs::Result<()> {
    Ok(())
}

/// Remove the temporary after a failure and build the error to return.
///
/// The removal is never discarded: if the temporary survives, its path goes into the
/// error, because a leftover the caller is never told about is a leak the user cannot
/// even find. Only called where the temporary is known to exist.
///
/// The leftover is recorded RELATIVE to `parent` -- the only frame `copy_file_at`
/// has. `copy_file` joins it onto the parent path it resolved, so its callers see
/// the same full path as before.
pub(crate) fn discard<D: DirHandle>(
    parent: &D,
    temp: &OsStr,
    step: CopyStep,
    code: Code,
    source: std::io::Error,
    guard: &Guard<'_>,
) -> CopyError {
    // §99: an unlink is a mutation too (spec:4858-4870). Without the lock the temporary stays, reported as a
    // leftover, with the guard's failure as the reason it was kept.
    if let Err(lost) = guard() {
        return CopyError {
            cause: FsError::new(code, source),
            leftover: Some((std::path::PathBuf::from(temp), lost.source)),
            step,
        };
    }
    match parent.remove_file(temp) {
        Ok(()) => CopyError::at(step, FsError::new(code, source)),
        // NotFound means it is already gone -- something else removed it, or it was
        // never created. Reporting a leftover here would send someone hunting a file
        // that does not exist.
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => {
            CopyError::at(step, FsError::new(code, source))
        }
        // Carry the removal's own error too, in `leftover` and NOT over `source`:
        // wrapping the primary failure in `Error::other(format!(..))` destroyed
        // `raw_os_error()` and, MEASURED on Linux, turned `classify` from `DiskFull`
        // into `IoError`.
        Err(why) => CopyError {
            cause: FsError::new(code, source),
            leftover: Some((std::path::PathBuf::from(temp), why.source)),
            step,
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
pub(crate) fn split_destination(dst: &Path) -> std::result::Result<(&Path, &OsStr), CopyError> {
    let Some(name) = dst.file_name() else {
        return Err(CopyError::at(
            CopyStep::Resolve,
            FsError::new(
                Code::DestinationError,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the destination names no file",
                ),
            ),
        ));
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
pub(crate) fn identity_gate<D: DirHandle>(
    parent: &D,
    name: &OsStr,
    src: &Metadata,
    safety: Safety,
    publish: Publish,
) -> std::result::Result<GateRead, CopyError> {
    let refuse = |why: &'static str| {
        CopyError::at(
            CopyStep::Gate,
            FsError::new(Code::SafetyRejected, std::io::Error::other(why)),
        )
    };
    let degraded = |weaker: FileIdentity| match safety {
        Safety::Default => Ok(Some(weaker)),
        Safety::Strict => Err(refuse(
            "the destination exists and its identity cannot be compared with full confidence",
        )),
    };
    match parent.metadata(name) {
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => {
            Ok(GateRead { degraded: None, existing: None })
        }
        Err(_) => Ok(GateRead { degraded: degraded(FileIdentity::Unavailable)?, existing: None }),
        Ok(m) if m.file_type == FileType::Dir => Err(refuse("the destination is a directory")),
        Ok(m) => {
            let verdict = match (src.identity, m.identity) {
                (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b => {
                    return Err(refuse("the destination is the source itself, by identity"));
                }
                (FileIdentity::Strong(_), FileIdentity::Strong(_)) => None,
                (s, d) => degraded(weaker(s, d))?,
            };
            // F3 (cut 4b): a publish that never replaces refuses an EXISTING destination
            // here, after every identity row and before the temporary exists, instead of
            // copying every byte and failing at the rename. Same code and kind the
            // rename gives (IO_ERROR, AlreadyExists); `copy_tree` maps it to
            // DESTINATION_NAMESPACE_COLLISION. A symlink occupying the name reaches this
            // row too - the handle stat never follows it - and is left untouched.
            if publish == Publish::NoReplace {
                return Err(CopyError::at(
                    CopyStep::Gate,
                    FsError::new(
                        Code::IoError,
                        std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            "the destination exists and this publish never replaces",
                        ),
                    ),
                ));
            }
            Ok(GateRead { degraded: verdict, existing: Some(m) })
        }
    }
}

/// Steps 2, 2a and 2b of `stage_file`, read-only: the source's metadata and type, the identity gate, and (single
/// file) the existing-destination policy. Nothing on disk changes. `parent` is `None` only for a dry run under a
/// destination directory that does not exist yet: the gate is then not run (nothing is at the name).
pub(crate) enum FilePlan {
    Proceed { src_meta: Metadata, degraded: Option<FileIdentity>, replaces: bool },
    Skip { reason: crate::dry_run::SkipReason, degraded: Option<FileIdentity> },
}

pub(crate) fn plan_file<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    parent: Option<&F::Dir>,
    name: &OsStr,
    opts: &CopyOptions,
) -> std::result::Result<FilePlan, CopyError> {
    // 2. source, captured for the step-7 re-check
    // Safe to `?`: nothing has been created yet, so there is nothing to leak.
    let src_meta = fs.metadata(src).map_err(|e| CopyError::at(CopyStep::Source, e))?;
    if src_meta.file_type != FileType::File {
        return Err(CopyError::at(
            CopyStep::Source,
            FsError::new(Code::SpecialFileUnsupported, std::io::Error::other("not a regular file")),
        ));
    }

    let Some(parent) = parent else {
        return Ok(FilePlan::Proceed { src_meta, degraded: None, replaces: false });
    };

    // 2a. the destination must not BE the source. Before the temporary exists, so a
    //     refusal changes nothing on disk.
    let gate = identity_gate(parent, name, &src_meta, opts.safety, opts.publish)?;

    // 2b. cut 8b, the existing-destination policy (single file): a skip changes nothing on disk. A directory at the
    //     name was already refused by the gate.
    if let Some(dest) = &gate.existing {
        let skip = match opts.existing {
            flux_fs::ExistingPolicy::Overwrite => None,
            flux_fs::ExistingPolicy::SkipExisting => Some(crate::dry_run::SkipReason::Existing),
            // Newer source or a different size; with either time unavailable, the lengths alone decide.
            flux_fs::ExistingPolicy::Update => {
                let proceed = src_meta.len != dest.len
                    || matches!((src_meta.modified, dest.modified), (Some(s), Some(d)) if s > d);
                (!proceed).then_some(crate::dry_run::SkipReason::UpToDate)
            }
        };
        if let Some(reason) = skip {
            return Ok(FilePlan::Skip { reason, degraded: gate.degraded });
        }
    }
    Ok(FilePlan::Proceed { src_meta, degraded: gate.degraded, replaces: gate.existing.is_some() })
}

/// The less trustworthy of two identities, for the warning's aggregation key.
/// `Unavailable` is weaker than `Weak`; between two `Weak`, the DESTINATION's volume
/// keys it, because that is the side the user did not name (walker design,
/// "Stand-downs").
pub(crate) fn weaker(src: FileIdentity, dst: FileIdentity) -> FileIdentity {
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
        return Err(CopyError::at(
            CopyStep::Resolve,
            FsError::new(
                Code::SafetyRejected,
                std::io::Error::other("source and destination are the same path"),
            ),
        ));
    }
    let (parent_path, name) = split_destination(dst)?;
    // The one path-based call on the write side. §149.7 exempts resolving the
    // destination itself: it may follow links, because the user named it.
    let parent =
        fs.destination_root(parent_path).map_err(|e| CopyError::at(CopyStep::Resolve, e))?;
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

/// B1 (cut 7a Part 3b): `copy_file`'s checks that need no lock - Step 0, the source's type (Step 2) and the identity
/// gate (Step 2a) - made by the run before it takes the destination's lock, so a refusal there creates nothing. The
/// copy makes Steps 2 and 2a again under the lock. Returns DEST's parent, its path, and the target's name, and the source's
/// identity, for the single-file record (cut 7b).
pub(crate) fn prepare_file<'d, F: DestinationRoot>(
    fs: &F,
    src: &Path,
    dst: &'d Path,
    opts: &CopyOptions,
) -> std::result::Result<(F::Dir, &'d Path, &'d OsStr, FileIdentity), CopyError> {
    if src == dst {
        return Err(CopyError::at(
            CopyStep::Resolve,
            FsError::new(
                Code::SafetyRejected,
                std::io::Error::other("source and destination are the same path"),
            ),
        ));
    }
    let (parent_path, name) = split_destination(dst)?;
    let parent =
        fs.destination_root(parent_path).map_err(|e| CopyError::at(CopyStep::Resolve, e))?;
    let src_meta = fs.metadata(src).map_err(|e| CopyError::at(CopyStep::Source, e))?;
    if src_meta.file_type != FileType::File {
        return Err(CopyError::at(
            CopyStep::Source,
            FsError::new(Code::SpecialFileUnsupported, std::io::Error::other("not a regular file")),
        ));
    }
    identity_gate(&parent, name, &src_meta, opts.safety, opts.publish)?;
    Ok((parent, parent_path, name, src_meta.identity))
}

/// Copy `src` to `name` inside `parent`, writing ONLY through `parent`, holding no lock: `copy_file_guarded` with a
/// guard that always passes and no heartbeat.
pub fn copy_file_at<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    parent: &F::Dir,
    name: &OsStr,
    opts: &CopyOptions,
) -> std::result::Result<Outcome, CopyError> {
    copy_file_guarded(
        fs,
        src,
        parent,
        name,
        opts,
        &unguarded,
        &no_heartbeat,
        &no_before_create,
        &no_before_publish,
    )
}

/// What `stage_file` produced: a temporary on disk, ready to be renamed to its name. At `SyncAt::Staging` it is closed;
/// at `SyncAt::Barrier` its writer is still open in `writer` and the flush closes it after the barrier.
pub(crate) struct Staged<W> {
    pub temp: OsString,
    pub identity: FileIdentity,
    pub bytes_copied: u64,
    pub metadata_failures: Vec<MetadataFailure>,
    pub identity_degraded: Option<FileIdentity>,
    /// Cut 9e: the temporary's open writer, `Some` only for a file staged at `SyncAt::Barrier` (whose data sync the
    /// caller owes, once per batch, and closes after the barrier); `None` at `SyncAt::Staging`, which has already synced
    /// the writer and dropped it on return.
    pub writer: Option<W>,
    /// Cut 9f-1: bytes proven by a validated prefix and not re-copied (0 for a fresh copy).
    pub reused: u64,
    /// Cut 9f-1: how a resumed partial's prefix was decided; `None` when nothing was discarded.
    pub discard: Option<Discard>,
    /// Cut 13: a `destination` or `full` comparison passed for this file (false for `none` and `source-stream`).
    pub verified: bool,
}

/// Cut 9f-1 (task 9): what a published checkpointed file reports beyond the public `Outcome`: the bytes its validated
/// prefix proved (not re-copied) and why its checkpoints were discarded, if they were. Taken from `Staged` before
/// `publish_staged` consumes it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Reuse {
    pub reused: u64,
    pub discard: Option<Discard>,
}

impl Reuse {
    /// Moves the discard out of `staged` (which is about to be published).
    pub(crate) fn take<W>(staged: &mut Staged<W>) -> Reuse {
        Reuse { reused: staged.reused, discard: staged.discard.take() }
    }
}

/// What ends the streaming step early: remove the partial (`Discard`) or keep it (`Keep`, a WAL failure).
enum StreamFail {
    Discard(CopyStep, Code, std::io::Error),
    Keep(CopyError),
}

fn sink_fail(e: crate::checkpoint::SinkError, name: &OsStr) -> StreamFail {
    use crate::checkpoint::SinkError;
    match e {
        SinkError::Heartbeat(e) => StreamFail::Discard(CopyStep::Heartbeat, e.code, e.source),
        SinkError::Data(f) => StreamFail::Discard(
            CopyStep::Durability,
            Code::StrictDurabilityUnavailable,
            std::io::Error::new(f.kind, f.message),
        ),
        SinkError::Wal(f) => {
            let name = name.to_string_lossy();
            StreamFail::Keep(CopyError {
                cause: FsError::new(
                    Code::WalWriteFailed,
                    std::io::Error::other(format!(
                        "checkpoints for {name} could not be written ({}); resume state is NOT trustworthy for this file",
                        f.message
                    )),
                ),
                leftover: None,
                step: CopyStep::Checkpoint,
            })
        }
    }
}

/// Cut 9e: where `stage_file` pays a Strict file's data `fsync`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SyncAt {
    /// Inline, after the metadata step; the writer is dropped on return (the single copy, and the unbatched path).
    Staging,
    /// Not at all: the open writer is handed out in `Staged.writer` for the caller's barrier.
    Barrier,
}

/// The result of `stage_file`: the step-2b skip (changes nothing on disk), or a staged temporary.
pub(crate) enum Stage<W> {
    Skipped(Outcome),
    Staged(Staged<W>),
}

/// Decides what a checkpointed file does with a partial at `temp` (spec decision 7). Reads the log's state for the
/// file, the partial's metadata and its bytes, and the source prefix; changes NOTHING on disk. Returns the verdict
/// and, for a resume, the partial's identity, which the caller needs to reopen it for writing.
///
/// Under `--resume-verify=chunks`: no checkpoints for the file is a plain fresh copy (`discard: None`) that touches
/// nothing. A log that cannot be read, a partial that is not a regular file, or one that cannot be opened for reading
/// is a fresh copy with `Discard::Unreadable`; a partial that is not there is `decide`'s `PartialMissing`.
///
/// Under `full` the log never decides validity: an absent, unreadable or empty log still salvages the partial, proven
/// chunk by chunk against the source by `decide_full`. The log is read only to choose the report when there is no
/// partial: `PartialMissing` iff it began the file, nothing otherwise (an unreadable log with no partial reports
/// nothing). A partial that is not a regular file is `Unreadable` under both levels.
/// Under `Hook::LogFree` it runs as `full` and, with no partial, reports nothing.
fn plan_resume<D: DirHandle + Send + 'static, R: std::io::Read>(
    hook: Hook<'_, '_, D>,
    parent: &D,
    temp: &OsStr,
    src_meta: &Metadata,
    reader: &mut R,
    beat: &Heartbeat<'_>,
) -> std::result::Result<(Verdict, Option<FileIdentity>), CopyError> {
    let fresh =
        |discard| Verdict { start: Start::Fresh, carry: Vec::new(), discard, proven: Vec::new() };
    let unreadable = |why: String| Ok((fresh(Some(Discard::Unreadable(why))), None));
    // A log-free hook has no log to consult: it behaves as `full`.
    let (level, logged) = match hook {
        Hook::Logged(fc) => (fc.run.resume_verify(), Some(fc)),
        Hook::LogFree { .. } => (ResumeVerify::Full, None),
    };
    // `chunks` trusts the recorded digests; `full` consults no log to decide validity (the source is the oracle).
    let state = match level {
        ResumeVerify::Chunks => {
            let fc = logged.expect("`chunks` is reachable only through a logged hook");
            match fc.run.load(&fc.file_id) {
                Ok(Some(state)) => Some(state),
                Ok(None) => return Ok((fresh(None), None)),
                Err(e) => return unreadable(format!("{e}")),
            }
        }
        ResumeVerify::Full => None,
    };
    let partial_meta = match parent.metadata(temp) {
        // No identity means `open_existing` must refuse it by contract: recopy rather than wedge on every later run.
        Ok(m) if m.file_type == FileType::File && m.identity == FileIdentity::Unavailable => {
            return unreadable("the partial's identity is unavailable".to_string());
        }
        // A second name for the object: reopening it for writing would write through into the other name's file. The
        // sweep unlinks only THIS name and the fresh create makes a new object; an unknown count proves nothing.
        Ok(m) if m.file_type == FileType::File && m.nlink != Some(1) => {
            return unreadable("the partial has more than one link".to_string());
        }
        Ok(m) if m.file_type == FileType::File => Some(m),
        Ok(m) => {
            return unreadable(format!("the partial is not a regular file ({:?})", m.file_type));
        }
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return unreadable(format!("the partial's metadata: {}", e.source)),
    };
    let mut partial_reader = if partial_meta.is_some() {
        match parent.open_read(temp) {
            Ok(r) => Some(r),
            Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return unreadable(format!("the partial cannot be read: {}", e.source)),
        }
    } else {
        None
    };
    // A heartbeat failure inside `decide` or `decide_full` is the lost lock; any other failure is reading the source.
    let beat_failed = std::cell::Cell::new(false);
    let watched = || beat().inspect_err(|_| beat_failed.set(true));
    let identity = crate::state::identity_text(src_meta.identity).into_bytes();
    let gate =
        Gate { len: src_meta.len, mtime: src_meta.modified.map(mtime_pair), identity: &identity };
    let partial_len = partial_reader.as_ref().and(partial_meta.as_ref()).map(|m| m.len);
    // `state` is `Some` exactly under `chunks`.
    let decided = match (state, partial_len, partial_reader.as_mut()) {
        (Some(state), partial_len, partial) => {
            decide(&state, &gate, partial_len, reader, partial, &watched)
        }
        (None, Some(len), Some(p)) => decide_full(src_meta.len, len, reader, p, &watched),
        // `full` with no partial: the log is consulted only to choose the report (a log that began the file says
        // the partial went missing; no log at all is a plain fresh copy).
        (None, _, _) => {
            // A log that cannot be read reports nothing either: under `full` validity never comes from the log.
            let discard = match logged.map(|fc| fc.run.load(&fc.file_id)) {
                Some(Ok(Some(_))) => Some(Discard::PartialMissing),
                Some(Ok(None) | Err(_)) | None => None,
            };
            return Ok((fresh(discard), None));
        }
    };
    let verdict = decided.map_err(|e| {
        let step = if beat_failed.get() { CopyStep::Heartbeat } else { CopyStep::Source };
        CopyError::at(step, e)
    })?;
    Ok((verdict, partial_meta.map(|m| m.identity)))
}

/// The source must not have changed under us (Section 33): its length and mtime still equal `src_meta`'s. On any
/// difference the temporary is discarded. Run before the verification re-read (cut 13, C13) and again at step 7.
///
/// A `match` rather than `?` on the metadata call: a `?` would return with the temporary still on disk, which is the
/// one leak `discard` exists to prevent.
fn recheck_source<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    src_meta: &Metadata,
    parent: &F::Dir,
    temp: &OsStr,
    guard: &Guard<'_>,
) -> std::result::Result<(), CopyError> {
    let now = match fs.metadata(src) {
        Ok(m) => m,
        // Gone is the most complete form of "changed under us", and the taxonomy has
        // a code for that. Reporting IO_ERROR here would hide it among unrelated
        // failures.
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => {
            let gone = std::io::Error::other("source disappeared during the copy");
            return Err(discard(parent, temp, CopyStep::Recheck, Code::SourceChanged, gone, guard));
        }
        Err(e) => return Err(discard(parent, temp, CopyStep::Recheck, e.code, e.source, guard)),
    };
    if now.len != src_meta.len || now.modified != src_meta.modified {
        let changed = std::io::Error::other("source changed");
        return Err(discard(parent, temp, CopyStep::Recheck, Code::SourceChanged, changed, guard));
    }
    Ok(())
}

/// Why a verification re-read stopped before its digest: the heartbeat failed, or the read itself did.
enum ReadFail {
    Beat(FsError),
    Read(std::io::Error),
}

/// Cut 13: hash `reader` to EOF in `buf`-sized reads, beating once per chunk like the stream loop.
fn hash_stream<R: std::io::Read>(
    reader: &mut R,
    buf: &mut [u8],
    beat: &Heartbeat<'_>,
) -> std::result::Result<(flux_hash::Digest, u64), ReadFail> {
    let mut beat_failure = None;
    let result = flux_hash::hash_reader(reader, buf, &mut || {
        beat().map_err(|e| {
            beat_failure = Some(e);
            std::io::Error::other("heartbeat failed during verification")
        })
    });
    match (result, beat_failure) {
        (Ok(v), _) => Ok(v),
        (Err(_), Some(e)) => Err(ReadFail::Beat(e)),
        (Err(e), None) => Err(ReadFail::Read(e)),
    }
}

/// Discard the temporary after a verification re-read failed. A failed beat is a `Heartbeat` failure with its own code;
/// a read error keeps the read's own code under `Verify`, and is NOT a `VerifyMismatch` (design C3).
fn discard_read_fail<D: DirHandle>(
    parent: &D,
    temp: &OsStr,
    fail: ReadFail,
    guard: &Guard<'_>,
) -> CopyError {
    match fail {
        ReadFail::Beat(e) => discard(parent, temp, CopyStep::Heartbeat, e.code, e.source, guard),
        ReadFail::Read(e) => discard(parent, temp, CopyStep::Verify, copy_code(&e), e, guard),
    }
}

/// Steps 1-7 of the copy, up to and including the source recheck and the read of the temporary's identity: everything
/// before the final heartbeat, guard and publishing rename. At `SyncAt::Staging` the writer is dropped on return, so no
/// descriptor is held; at `SyncAt::Barrier` it is NOT: the open writer rides in `Staged.writer` and the flush closes it
/// after the barrier (the descriptor is held until then, which is what the open-writer cap bounds). On every error path
/// after the temporary exists it has been discarded exactly as `copy_file_guarded` always did, with two deliberate
/// exceptions: a WAL failure while streaming a checkpointed file (`StreamFail::Keep`) and a failure to reopen a
/// partial for resume (`open_existing` / `truncate_to`) leave the partial in place, because it is the state a later
/// `--resume` judges.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stage_file<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    parent: &F::Dir,
    name: &OsStr,
    opts: &CopyOptions,
    guard: &Guard<'_>,
    beat: &Heartbeat<'_>,
    before_create: &BeforeCreate<'_>,
    sync: SyncAt,
    ckpt: Option<Hook<'_, '_, F::Dir>>,
) -> std::result::Result<Stage<F::Writer>, CopyError> {
    let temp = temp_name(name, &opts.operation_id);

    // 1. a leftover partial under this operation's name (§18.1). Live under `--resume`, which carries the prior
    //    run's operation id, so the name IS the prior run's partial. A run that does not checkpoint removes it here; a
    //    checkpointing run defers the sweep until after the claim (below), because the partial may be resumed rather
    //    than recopied, and sweeps it only when no validated checkpoint set lets it continue.
    beat().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
    guard().map_err(|e| CopyError::at(CopyStep::Create, e))?;
    // Cut 9f-1: a checkpointing run sweeps after the claim instead (below), where it gets its own guard.
    if ckpt.is_none() {
        let _ = parent.remove_file(&temp);
    }

    // 2, 2a, 2b: the read-only prelude, `plan_file`. A skip changes nothing on disk.
    let (src_meta, identity_degraded) = match plan_file(fs, src, Some(parent), name, opts)? {
        FilePlan::Skip { degraded, .. } => {
            // A hook deferred the early sweep, and a skip returns before the Fresh branch's: sweep here, under its
            // own section 99 check (a removal is a destination mutation).
            if ckpt.is_some() {
                guard().map_err(|e| CopyError::at(CopyStep::Create, e))?;
                let _ = parent.remove_file(&temp);
            }
            return Ok(Stage::Skipped(Outcome {
                skipped: true,
                verified: false,
                bytes_copied: 0,
                metadata_failures: vec![],
                identity_degraded: degraded,
                published_identity: FileIdentity::Unavailable,
            }));
        }
        FilePlan::Proceed { src_meta, degraded, .. } => (src_meta, degraded),
    };

    let mut reader = fs.open_read(src).map_err(|e| CopyError::at(CopyStep::Source, e))?;

    // 3. exclusive create (FS-1)
    // Still safe: if this FAILS, this call is precisely what did not create the
    // temporary, so there is nothing of ours on disk.
    // §99 before the temporary exists: a failure here has created nothing.
    beat().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
    guard().map_err(|e| CopyError::at(CopyStep::Create, e))?;
    // Cut 8b: the claim, after the gate and the section 99 check, before the temporary exists.
    before_create()?;
    // Cut 9f-1: a checkpointing run decides what to do with an existing partial, after the claim (a refused claim
    // reads nothing). Only a file large enough to be checkpointed can resume; every other verdict is a fresh copy.
    let ckpt_file = ckpt.filter(|c| src_meta.len >= c.min_len());
    let (verdict, partial_identity) = match ckpt_file {
        Some(hook) => plan_resume(hook, parent, &temp, &src_meta, &mut reader, beat)?,
        None => (
            Verdict { start: Start::Fresh, carry: Vec::new(), discard: None, proven: Vec::new() },
            None,
        ),
    };
    let Verdict { start, carry, discard: why, proven } = verdict;
    // Cut 13 (C9): a resumed file's prefix was never hashed by this run, so a digest of it would prove nothing. Refused
    // before `open_existing`, so the partial is left exactly as the crashed run left it.
    if opts.verify != VerifyLevel::None && matches!(start, Start::Resume { .. }) {
        return Err(CopyError::at(
            CopyStep::Verify,
            FsError::new(
                Code::CopyFailed,
                std::io::Error::other("verification of a resumed file is not supported"),
            ),
        ));
    }
    // Cut 13: the digest of the source bytes as they are read (C8). `None` does no hashing at all.
    let mut hasher = (opts.verify != VerifyLevel::None).then(flux_hash::Hasher::new);
    let (mut writer, reused) = match (start, partial_identity) {
        (Start::Resume { offset }, Some(identity)) => {
            // A destination mutation, so its own section 99 check. The partial is reopened for writing by the identity
            // `decide` validated, then cut: bytes past `offset` are never trusted.
            guard().map_err(|e| CopyError::at(CopyStep::Create, e))?;
            let mut w = parent
                .open_existing(&temp, &identity)
                .map_err(|e| CopyError::at(CopyStep::Create, e))?;
            w.truncate_to(offset).map_err(|e| CopyError::at(CopyStep::Create, e))?;
            (w, offset)
        }
        (Start::Resume { .. }, None) => {
            unreachable!("a resume is decided only over an existing partial")
        }
        (Start::Fresh, _) => {
            // The sweep of a checkpointing run: a removal is a destination mutation, so it has its own section 99
            // check.
            if ckpt.is_some() {
                guard().map_err(|e| CopyError::at(CopyStep::Create, e))?;
                let _ = parent.remove_file(&temp);
            }
            let w = parent.create_new(&temp).map_err(|e| CopyError::at(CopyStep::Create, e))?;
            (w, 0)
        }
    };

    // 4. stream. The sink (checkpointed files only) is declared before the funnel so the one `abort` below covers
    //    every exit between its `begin` and the end of the loop.
    let mut bytes_copied = 0u64;
    let mut buf = vec![0u8; copy_buffer_len(src_meta.len)];
    let mut sink: Option<FileSink<'_, F::Dir>> = None;
    let streamed: std::result::Result<(), StreamFail> = 'stream: {
        if let Some(Hook::Logged(fc)) = ckpt.filter(|c| src_meta.len >= c.min_len()) {
            let dup = if fc.run.durability() == Durability::Strict {
                match writer.duplicate() {
                    Ok(d) => Some(d),
                    Err(e) => {
                        break 'stream Err(StreamFail::Discard(
                            CopyStep::Durability,
                            Code::StrictDurabilityUnavailable,
                            e.source,
                        ));
                    }
                }
            } else {
                None
            };
            match FileSink::begin(fc, &src_meta, reused, dup, beat) {
                Ok(s) => sink = Some(s),
                Err(e) => break 'stream Err(sink_fail(e, name)),
            }
            // `full` proved this prefix against the source; re-record it so the log covers what the file holds.
            if !proven.is_empty()
                && let Some(s) = sink.as_mut()
                && let Err(e) = s.record_proven(&proven, beat)
            {
                break 'stream Err(sink_fail(e, name));
            }
        }
        // The source bytes `decide` consumed are written first: `reader` already sits past them.
        if !carry.is_empty() {
            if let Some(h) = hasher.as_mut() {
                h.update(&carry);
            }
            if let Err(e) = writer.write_all(&carry) {
                break 'stream Err(StreamFail::Discard(CopyStep::Stream, copy_code(&e), e));
            }
            bytes_copied += carry.len() as u64;
            if let Some(s) = sink.as_mut()
                && let Err(e) = s.wrote(&carry, beat)
            {
                break 'stream Err(sink_fail(e, name));
            }
        }
        loop {
            let n = match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    break 'stream Err(StreamFail::Discard(CopyStep::Stream, copy_code(&e), e));
                }
            };
            if let Some(h) = hasher.as_mut() {
                h.update(&buf[..n]);
            }
            if let Err(e) = writer.write_all(&buf[..n]) {
                break 'stream Err(StreamFail::Discard(CopyStep::Stream, copy_code(&e), e));
            }
            bytes_copied += n as u64;
            // Cut 9h (spec 3.4): only a buffer already written is handed over (I1); the sink returns one at full
            // length to read into next. Without a sink `buf` is never taken.
            if let Some(s) = sink.as_mut() {
                match s.submit(std::mem::take(&mut buf), n, beat) {
                    Ok(next) => buf = next,
                    Err(e) => break 'stream Err(sink_fail(e, name)),
                }
            }
            // §101 (cut 7b): the heartbeat, once per chunk (at most COPY_BUF_MAX bytes).
            if let Err(e) = beat() {
                break 'stream Err(StreamFail::Discard(CopyStep::Heartbeat, e.code, e.source));
            }
        }
        if let Some(s) = sink.as_mut()
            && let Err(e) = s.complete(beat)
        {
            break 'stream Err(sink_fail(e, name));
        }
        Ok(())
    };
    if streamed.is_err()
        && let Some(s) = sink.as_mut()
    {
        s.abort(beat);
    }
    match streamed {
        Ok(()) => {}
        Err(StreamFail::Keep(e)) => return Err(e),
        Err(StreamFail::Discard(step, code, source)) => {
            return Err(discard(parent, &temp, step, code, source, guard));
        }
    }

    // 5b. cut 13: verification. The source digest `S` covers what the loop read; `destination` and `full` compare it
    //     with a re-read of the temporary (and, for `full`, of the source) BEFORE anything is published.
    let mut verified = false;
    if matches!(opts.verify, VerifyLevel::Destination | VerifyLevel::Full)
        && let Some(source_digest) = hasher.as_ref().map(flux_hash::Hasher::finalize)
    {
        // C13: the cheap source recheck first, so a source that already changed costs no read-back.
        recheck_source(fs, src, &src_meta, parent, &temp, guard)?;
        let streamed_len = reused + bytes_copied;
        let mut vbuf = vec![0u8; copy_buffer_len(src_meta.len)];

        let mut temp_reader = match parent.open_read(&temp) {
            Ok(r) => r,
            Err(e) => {
                return Err(discard(parent, &temp, CopyStep::Verify, e.code, e.source, guard));
            }
        };
        let (digest, len) = match hash_stream(&mut temp_reader, &mut vbuf, beat) {
            Ok(v) => v,
            Err(fail) => return Err(discard_read_fail(parent, &temp, fail, guard)),
        };
        drop(temp_reader);
        if digest != source_digest {
            let text = format!(
                "destination bytes differ from the source stream (source stream {streamed_len} bytes, destination re-read {len} bytes)"
            );
            return Err(discard(
                parent,
                &temp,
                CopyStep::Verify,
                Code::VerifyMismatch,
                std::io::Error::other(text),
                guard,
            ));
        }

        if opts.verify == VerifyLevel::Full {
            // C11: an independent read of the source, by path, as the first read was.
            let mut second = match fs.open_read(src) {
                Ok(r) => r,
                Err(e) => {
                    return Err(discard(parent, &temp, CopyStep::Verify, e.code, e.source, guard));
                }
            };
            let (again, len) = match hash_stream(&mut second, &mut vbuf, beat) {
                Ok(v) => v,
                Err(fail) => return Err(discard_read_fail(parent, &temp, fail, guard)),
            };
            if again != source_digest {
                let text = format!(
                    "source bytes differ between the copy and the second read (first read {streamed_len} bytes, second read {len} bytes)"
                );
                return Err(discard(
                    parent,
                    &temp,
                    CopyStep::Verify,
                    Code::VerifyMismatch,
                    std::io::Error::other(text),
                    guard,
                ));
            }
        }
        verified = true;
    }

    // 6. metadata, on the temporary, BEFORE publication (§44.1)
    let mut metadata_failures = Vec::new();

    if opts.preserve_times != Preserve::Off
        && let Err(e) = fs.set_times(&writer, src_meta.modified)
    {
        if opts.preserve_times == Preserve::Strict {
            return Err(discard(
                parent,
                &temp,
                CopyStep::Metadata,
                Code::MetadataApplyFailed,
                e.source,
                guard,
            ));
        }
        metadata_failures.push(MetadataFailure { item: MetadataItem::Times, error: e });
    }

    if opts.preserve_permissions != Preserve::Off
        && let Err(e) = fs.set_permissions(&writer, src_meta.permissions)
    {
        if opts.preserve_permissions == Preserve::Strict {
            return Err(discard(
                parent,
                &temp,
                CopyStep::Metadata,
                Code::MetadataApplyFailed,
                e.source,
                guard,
            ));
        }
        metadata_failures.push(MetadataFailure { item: MetadataItem::Permissions, error: e });
    }

    // 6b. durability (cut 9e: after the metadata, so the one sync covers the times and permissions as well as the data;
    //     at `SyncAt::Barrier` the caller syncs the returned writer, once per batch). Error precedence (owner ruling,
    //     2026-10-12): the first failing step reports its own code, in step order, so a Strict metadata failure
    //     returns before this sync runs and `MetadataApplyFailed` wins over `StrictDurabilityUnavailable`.
    if sync == SyncAt::Staging
        && opts.durability == Durability::Strict
        && let Err(e) = writer.sync_all()
    {
        return Err(discard(
            parent,
            &temp,
            CopyStep::Durability,
            Code::StrictDurabilityUnavailable,
            e.source,
            guard,
        ));
    }

    // 7. the source must not have changed under us (Section 33), then publish.
    //    Note the `match` rather than `?`: a `?` here would return with the temporary
    //    still on disk, which is the one leak the `discard` helper exists to prevent.
    recheck_source(fs, src, &src_meta, parent, &temp, guard)?;

    // Cut 7b: the object about to be published, read from its own handle - a rename keeps it - so the run records the
    // target's identity without a look-up by name after the rename.
    let identity = writer.identity().unwrap_or(FileIdentity::Unavailable);
    let writer = match sync {
        SyncAt::Staging => None,
        SyncAt::Barrier => Some(writer),
    };
    Ok(Stage::Staged(Staged {
        temp,
        identity,
        bytes_copied,
        metadata_failures,
        identity_degraded,
        writer,
        reused,
        discard: why,
        verified,
    }))
}

/// Copy `src` to `name` inside `parent`, writing ONLY through `parent`.
///
/// Nothing below re-resolves a destination path: the staging temporary, the
/// metadata, the publish and the cleanup all go through the handle, so a parent
/// swapped for a link after it was opened cannot redirect the write (item 114).
///
/// `guard` runs before every destination mutation - the step-1 sweep, the exclusive create, the publishing rename and a
/// failed copy's removal of its temporary - which is §99's `S99_check` before each `S99_write` (cut 7a Part 3b). A
/// failed guard stops the copy at that point: before the create it creates nothing, and at the publish or a removal
/// the temporary stays, reported as `leftover`.
///
/// `beat` runs immediately before each of those guard calls except the removal's, and after every chunk written (cut
/// 7b; at most `COPY_BUF_MAX` bytes). Its failure is `CopyStep::Heartbeat`: before the create it creates nothing;
/// after it, the temporary is removed as for any other failure.
///
/// `before_create` (cut 8b) is called exactly once per copy that reaches the create: after the heartbeat and guard that
/// precede the exclusive create, and before the temporary exists. Its error stops the copy there, creating nothing.
/// A copy that is refused by the gate or skipped by the policy never calls it.
///
/// Step 2b is the existing-destination policy decision, taken after the identity gate (which already refused a
/// directory at the name): `Overwrite` proceeds, `SkipExisting` returns a skipped outcome changing nothing on disk,
/// and `Update` proceeds when the source is newer or the sizes differ (sizes alone when either time is unavailable).
///
/// `before_publish` (cut 9c) is called exactly once per copy that reaches the publish: after the source recheck and
/// before the final heartbeat and guard, with the temporary's name and identity. Its error discards the temporary and
/// is returned as is; a copy that fails the recheck never calls it.
///
/// Nine parameters: the signature is the cut 8b plan's contract (guard, beat, before_create arrive together), plus
/// cut 9c's `before_publish`.
#[allow(clippy::too_many_arguments)]
pub fn copy_file_guarded<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    parent: &F::Dir,
    name: &OsStr,
    opts: &CopyOptions,
    guard: &Guard<'_>,
    beat: &Heartbeat<'_>,
    before_create: &BeforeCreate<'_>,
    before_publish: &BeforePublish<'_>,
) -> std::result::Result<Outcome, CopyError> {
    copy_file_checkpointed(
        fs,
        src,
        parent,
        name,
        opts,
        guard,
        beat,
        before_create,
        before_publish,
        None,
    )
    .map(|(outcome, _)| outcome)
}

/// `copy_file_guarded` plus cut 9f-1's checkpoint hook (`ckpt`, `None` for an uncheckpointed copy, which is then
/// exactly `copy_file_guarded`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn copy_file_checkpointed<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    parent: &F::Dir,
    name: &OsStr,
    opts: &CopyOptions,
    guard: &Guard<'_>,
    beat: &Heartbeat<'_>,
    before_create: &BeforeCreate<'_>,
    before_publish: &BeforePublish<'_>,
    ckpt: Option<Hook<'_, '_, F::Dir>>,
) -> std::result::Result<(Outcome, Reuse), CopyError> {
    let mut staged = match stage_file(
        fs,
        src,
        parent,
        name,
        opts,
        guard,
        beat,
        before_create,
        SyncAt::Staging,
        ckpt,
    )? {
        Stage::Skipped(outcome) => return Ok((outcome, Reuse::default())),
        Stage::Staged(staged) => staged,
    };
    let reuse = Reuse::take(&mut staged);
    // Cut 9c: the hook sees the temporary and its identity before the final heartbeat and guard. Its error is returned
    // as is, after the temporary is discarded like any other pre-rename failure.
    let intent = PublishIntent { temp: staged.temp.clone(), identity: staged.identity };
    if let Err(e) = before_publish(&intent) {
        let CopyError { cause, leftover, step } = e;
        let mut out = discard(parent, &staged.temp, step, cause.code, cause.source, guard);
        if out.leftover.is_none() {
            out.leftover = leftover;
        }
        return Err(out);
    }
    publish_staged(parent, name, staged, opts.publish, guard, beat).map(|outcome| (outcome, reuse))
}

/// The final heartbeat, guard and publishing rename for a `Staged` temporary, with the exact error handling the single
/// copy always had: a heartbeat failure discards the temporary, a lost guard keeps it as the leftover, and a failed
/// rename discards it. `staged.writer` is `None` on every live path (the flush closes it after the barrier, before this
/// runs); a `Some` is dropped defensively, first.
pub(crate) fn publish_staged<D: DirHandle>(
    parent: &D,
    name: &OsStr,
    staged: Staged<D::Writer>,
    publish: Publish,
    guard: &Guard<'_>,
    beat: &Heartbeat<'_>,
) -> std::result::Result<Outcome, CopyError> {
    let Staged {
        temp,
        identity: published_identity,
        bytes_copied,
        metadata_failures,
        identity_degraded,
        writer,
        reused: _,
        discard: _,
        verified,
    } = staged;
    // Cut 9e: a file published at the barrier was synced there; either way no descriptor is held across the rename.
    drop(writer);
    // §99 (`S99_check`, then the `S99_write` below): publish only while the lock is still this run's. Otherwise the
    // temporary stays - removing it would be a mutation too - and is reported as the leftover.
    if let Err(e) = beat() {
        return Err(discard(parent, &temp, CopyStep::Heartbeat, e.code, e.source, guard));
    }
    if let Err(lost) = guard() {
        return Err(CopyError {
            leftover: Some((
                std::path::PathBuf::from(&temp),
                std::io::Error::other("kept: this run no longer holds the destination's lock"),
            )),
            ..CopyError::at(CopyStep::Publish, lost)
        });
    }
    let published = match publish {
        Publish::Replace => parent.rename_replace(&temp, parent, name),
        Publish::NoReplace => parent.rename_no_replace(&temp, parent, name),
    };
    if let Err(e) = published {
        // Keep whatever the platform layer mapped. A publish failure is NOT the content
        // copy -- that already succeeded -- so it must not be relabelled COPY_FAILED,
        // and an unclassified one stays IO_ERROR, the declared catch-all.
        return Err(discard(parent, &temp, CopyStep::Publish, e.code, e.source, guard));
    }

    // A successful rename consumed the temporary; there is nothing left to remove.

    Ok(Outcome {
        bytes_copied,
        metadata_failures,
        identity_degraded,
        published_identity,
        skipped: false,
        verified,
    })
}

/// Cut 9e: the next wake on the barrier's absolute grid (anchored at the barrier's start, stepped by `beat_every`):
/// the first grid point strictly after `now`. Advancing on the grid, never from `now`, is what keeps a stream of
/// results from resetting the timer; stepping until the point is past `now` is what keeps a stalled main thread from
/// timing out once per missed step on resume.
fn next_wake(mut deadline: Instant, now: Instant, beat_every: Duration) -> Instant {
    while deadline <= now {
        deadline += beat_every;
    }
    deadline
}

/// What the cut 9e barrier found: one result per writer, in input order, and the heartbeat failure if one ended the wait.
pub(crate) struct SyncOutcome {
    pub results: Vec<Result<(), FsError>>,
    pub heartbeat: Option<FsError>,
    /// Worker threads that actually ran: 0 on the main-thread path (chosen by `n == 1`, by `threads == 0`, or by the
    /// spawn fallback). Read by the tests only; `flush_batch` ignores it.
    #[cfg_attr(not(test), expect(dead_code))]
    pub workers: usize,
}

/// Cut 9e: the data `fsync` of every pending writer, `threads` at a time (at most one worker per writer). The calling
/// thread does not sync; it keeps the lock's heartbeat alive, calling `beat` whenever a further `beat_every` has passed
/// IN TOTAL since the start (an absolute deadline on the grid `start + k * beat_every`, never a fresh interval per
/// received result). After a beat the deadline advances along that same grid to the first point after now, so a stalled
/// calling thread beats once, not once per missed interval.
///
/// A single writer (`n == 1`) always syncs on the calling thread, whatever `threads` is: a worker would cost a thread
/// spawn and a channel for nothing, and the measurement found that cost 1.6 ms per single-entry batch (one-file-per-
/// directory trees ran 15% slower than cut 9d). `beat` between syncs is moot for one.
///
/// `threads == 0` means no workers: the syncs run on the calling thread (the main-thread path). 0 is the test seam for
/// that path; `BatchPolicy` never passes 0 (`DEFAULT` is 16, the tests use 1 or 16).
/// `beat` is `dyn Fn` without `Send`, so it is only ever called here, never from a worker. After one heartbeat failure
/// no further `beat` is made, but the wait continues until every worker has finished and been joined: every writer
/// still gets its result, and the failure is reported once in `heartbeat`. A worker panic propagates out of the scope.
/// A worker the OS refuses to spawn (thread limits) is skipped, never a panic: the workers that did start share the
/// index counter and so all the work; with none started, the syncs run on the calling thread in index order and `beat`
/// is called between them whenever the deadline has passed (the same deadline rule and single-failure rule).
pub(crate) fn sync_staged_many<W: FileHandle + Sync>(
    writers: &[&W],
    threads: usize,
    beat: &Heartbeat<'_>,
    beat_every: Duration,
) -> SyncOutcome {
    let n = writers.len();
    if n == 0 {
        return SyncOutcome { results: Vec::new(), heartbeat: None, workers: 0 };
    }
    let workers = if n == 1 { 0 } else { threads.min(n) };
    let next = AtomicUsize::new(0);
    let mut slots: Vec<Option<Result<(), FsError>>> = Vec::new();
    slots.resize_with(n, || None);
    let mut heartbeat = None;
    let mut ran = 0;
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<(usize, Result<(), FsError>)>();
        let mut spawned = 0;
        for _ in 0..workers {
            let tx = tx.clone();
            let next = &next;
            let worker = move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    // A closed receiver cannot happen before every index is received; ignore it rather than panic.
                    let _ = tx.send((i, writers[i].sync_all()));
                }
            };
            // `Scope::spawn` panics when the OS refuses a thread, and the binary aborts on panic: use the fallible form.
            if std::thread::Builder::new().spawn_scoped(scope, worker).is_err() {
                break;
            }
            spawned += 1;
        }
        drop(tx);
        let mut beat_if_due = |deadline: &mut Instant| {
            if heartbeat.is_none()
                && let Err(e) = beat()
            {
                heartbeat = Some(e);
            }
            *deadline = next_wake(*deadline, Instant::now(), beat_every);
        };
        let mut deadline = Instant::now() + beat_every;
        if spawned == 0 {
            // No worker (`threads == 0`, or none could be spawned): sync on this thread in index order, beating once the deadline has passed.
            for (i, w) in writers.iter().enumerate() {
                slots[i] = Some(w.sync_all());
                if Instant::now() >= deadline {
                    beat_if_due(&mut deadline);
                }
            }
            return;
        }
        ran = spawned;
        let mut received = 0;
        while received < n {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok((i, r)) => {
                    slots[i] = Some(r);
                    received += 1;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => beat_if_due(&mut deadline),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    let results = slots.into_iter().map(|s| s.expect("every index is sent exactly once")).collect();
    SyncOutcome { results, heartbeat, workers: ran }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::FileCkpt;
    use crate::fault_fs::FaultFs;
    use flux_fs::{Durability, FileSystem, Links, OperationId, Preserve, Publish};

    #[test]
    fn the_next_wake_is_the_first_grid_point_after_now() {
        let start = Instant::now();
        let step = Duration::from_millis(10);
        // Not yet due: unchanged.
        assert_eq!(next_wake(start + step, start, step), start + step);
        // Exactly due: one step on.
        assert_eq!(next_wake(start + step, start + step, step), start + 2 * step);
        // Five steps behind (a stalled main thread): the first point after now, not now + step and not deadline + step.
        let now = start + Duration::from_millis(57);
        assert_eq!(next_wake(start + step, now, step), start + 6 * step);
    }

    fn opts() -> CopyOptions {
        CopyOptions {
            preserve_times: Preserve::Default,
            preserve_permissions: Preserve::Default,
            durability: Durability::Normal,
            publish: Publish::Replace,
            safety: flux_fs::Safety::Default,
            operation_id: OperationId::new("op1"),
            existing: flux_fs::ExistingPolicy::Overwrite,
            links: Links::Copy,
            hardlinks: flux_fs::Hardlinks::Auto,
            cross_filesystems: false,
            verify: flux_fs::VerifyLevel::None,
            hash: flux_fs::HashAlgorithm::Blake3,
        }
    }

    fn stage_hello(
        fs: &FaultFs,
        root: &<FaultFs as DestinationRoot>::Dir,
        sync: SyncAt,
    ) -> Staged<<FaultFs as FileSystem>::Writer> {
        stage_with(fs, root, &opts(), sync)
    }

    fn stage_with(
        fs: &FaultFs,
        root: &<FaultFs as DestinationRoot>::Dir,
        opts: &CopyOptions,
        sync: SyncAt,
    ) -> Staged<<FaultFs as FileSystem>::Writer> {
        match stage_file(
            fs,
            Path::new("/src"),
            root,
            OsStr::new("dst"),
            opts,
            &unguarded,
            &no_heartbeat,
            &no_before_create,
            sync,
            None,
        ) {
            Ok(Stage::Staged(s)) => s,
            Ok(Stage::Skipped(_)) => panic!("unexpectedly skipped"),
            Err(e) => panic!("stage_file failed: {e:?}"),
        }
    }

    #[test]
    fn an_uncheckpointed_copy_is_byte_for_byte_unchanged() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let mut o = opts();
        o.durability = Durability::Strict;
        let staged = stage_with(&fs, &root, &o, SyncAt::Staging);
        assert_eq!(staged.bytes_copied, 5);
        assert_eq!(fs.read_file("/dst.flux-partial.op1").as_deref(), Some(&b"hello"[..]));
        // Captured from the code BEFORE cut 9f-1 plan 2 task 6 touched `stage_file`.
        let golden = [
            "remove_file(/dst.flux-partial.op1)",
            "metadata(/src)",
            "metadata(/dst)",
            "open_read(/src)",
            "create_new(/dst.flux-partial.op1)",
            "set_times(/dst.flux-partial.op1)",
            "set_permissions(/dst.flux-partial.op1)",
            "sync_all(/dst.flux-partial.op1)",
            "metadata(/src)",
            "drop_writer(/dst.flux-partial.op1)",
        ];
        assert_eq!(fs.calls(), golden);
    }

    // ---- cut 9f-1 plan 2, task 6: a fresh copy checkpoints through `ckpt` ----

    use crate::checkpoint::{CheckpointPolicy, Checkpoints};
    use crate::fault_fs::FakeDirHandle;
    use crate::wal::record::{Body, Decoded, decode_next};
    use crate::wal::writer::WalConfig;
    use std::sync::atomic::AtomicBool;

    const MIB: usize = 1 << 20;
    const WAL_PATH: &str = "/ws/wal/000001";
    const PARTIAL: &str = "/dst.flux-partial.op1";

    fn ckpt_policy(
        durability: Durability,
        channel_cap: usize,
        flush_bytes: u64,
    ) -> CheckpointPolicy {
        CheckpointPolicy {
            min_len: 1 << 20,
            range_chunks: 4,
            wal: WalConfig {
                flush_interval: Duration::from_secs(3600),
                flush_bytes,
                channel_cap,
                durability,
            },
            verify: crate::checkpoint::ResumeVerify::Chunks,
        }
    }

    fn ckpt_fs(len: usize) -> (FaultFs, <FaultFs as DestinationRoot>::Dir) {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/ws")).unwrap();
        let src: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        fs.write_file("/src", &src);
        let root = fs.destination_root(Path::new("/")).unwrap();
        (fs, root)
    }

    fn ckpt_run(fs: &FaultFs, policy: CheckpointPolicy) -> Checkpoints<FakeDirHandle> {
        let a = fs.destination_root(Path::new("/ws")).unwrap();
        let b = fs.destination_root(Path::new("/ws")).unwrap();
        Checkpoints::open(a, b, "op", 1, policy).unwrap_or_else(|e| panic!("open: {e:?}"))
    }

    fn stage_ckpt(
        fs: &FaultFs,
        root: &<FaultFs as DestinationRoot>::Dir,
        run: &Checkpoints<FakeDirHandle>,
        durability: Durability,
        beat: &Heartbeat<'_>,
    ) -> std::result::Result<Stage<<FaultFs as FileSystem>::Writer>, CopyError> {
        let mut o = opts();
        o.durability = durability;
        let fc = FileCkpt { run, file_id: b"f".to_vec(), destination: b"dst".to_vec() };
        stage_file(
            fs,
            Path::new("/src"),
            root,
            OsStr::new("dst"),
            &o,
            &unguarded,
            beat,
            &no_before_create,
            SyncAt::Staging,
            Some(Hook::Logged(&fc)),
        )
    }

    fn wal_bodies(fs: &FaultFs) -> Vec<Body> {
        let bytes = fs.read_file(WAL_PATH).expect("the segment exists");
        let mut out = Vec::new();
        let mut at = 0;
        loop {
            match decode_next(&bytes[at..]) {
                Decoded::Record { body, len, .. } => {
                    out.push(body);
                    at += len;
                }
                Decoded::End => return out,
                other => panic!("unexpected {other:?} at {at}"),
            }
        }
    }

    fn count(fs: &FaultFs, prefix: &str) -> usize {
        fs.calls().iter().filter(|c| c.starts_with(prefix)).count()
    }

    #[test]
    fn a_large_file_writes_a_filebegin_and_ranges_that_cover_it() {
        let (fs, root) = ckpt_fs(5 * MIB);
        let run = ckpt_run(&fs, ckpt_policy(Durability::Normal, 64, 1 << 50));
        let Ok(Stage::Staged(staged)) =
            stage_ckpt(&fs, &root, &run, Durability::Normal, &no_heartbeat)
        else {
            panic!("expected a staged file");
        };
        assert_eq!(staged.bytes_copied, 5 * MIB as u64);
        assert_eq!((staged.reused, staged.discard.is_none()), (0, true));
        assert!(run.finish(&no_heartbeat).is_clean());
        let bodies = wal_bodies(&fs);
        let begins: Vec<_> = bodies
            .iter()
            .filter_map(|b| if let Body::FileBegin(b) = b { Some(b) } else { None })
            .collect();
        assert_eq!(begins.len(), 1);
        assert_eq!(begins[0].valid_through, 0);
        assert_eq!(begins[0].source_len, 5 * MIB as u64);
        let ranges: Vec<_> = bodies
            .iter()
            .filter_map(|b| if let Body::Range(r) = b { Some(r) } else { None })
            .collect();
        let src = fs.read_file("/src").unwrap();
        let want: Vec<[u8; 32]> = src.chunks(MIB).map(|c| *blake3::hash(c).as_bytes()).collect();
        let got: Vec<[u8; 32]> = ranges.iter().flat_map(|r| r.digests.clone()).collect();
        assert_eq!(got, want);
        assert_eq!(ranges.first().map(|r| r.range_start), Some(0));
        assert_eq!(ranges.last().map(|r| r.range_end), Some(5 * MIB as u64));
        assert!(ranges.windows(2).all(|w| w[0].range_end == w[1].range_start));
    }

    #[test]
    fn a_file_below_min_len_writes_no_wal_at_all() {
        let (fs, root) = ckpt_fs(512 * 1024);
        let run = ckpt_run(&fs, ckpt_policy(Durability::Strict, 64, 1 << 50));
        let Ok(Stage::Staged(_)) = stage_ckpt(&fs, &root, &run, Durability::Strict, &no_heartbeat)
        else {
            panic!("expected a staged file");
        };
        assert!(run.finish(&no_heartbeat).is_clean());
        assert!(!fs.called("create_dir(/ws/wal"), "{:?}", fs.calls());
        assert!(fs.read_file(WAL_PATH).is_none());
        assert!(!fs.called("duplicate("), "{:?}", fs.calls());
    }

    #[test]
    fn normal_registers_no_duplicate_and_strict_registers_one() {
        let (fs, root) = ckpt_fs(3 * MIB);
        let run = ckpt_run(&fs, ckpt_policy(Durability::Normal, 64, 1 << 50));
        assert!(stage_ckpt(&fs, &root, &run, Durability::Normal, &no_heartbeat).is_ok());
        assert!(run.finish(&no_heartbeat).is_clean());
        assert!(!fs.called("duplicate("), "{:?}", fs.calls());

        let (fs, root) = ckpt_fs(3 * MIB);
        let run = ckpt_run(&fs, ckpt_policy(Durability::Strict, 64, 1 << 50));
        assert!(stage_ckpt(&fs, &root, &run, Durability::Strict, &no_heartbeat).is_ok());
        assert!(run.finish(&no_heartbeat).is_clean());
        assert_eq!(count(&fs, "duplicate("), 1, "{:?}", fs.calls());
    }

    #[test]
    fn an_aborted_file_closes_its_duplicate() {
        let (fs, root) = ckpt_fs(3 * MIB);
        let run = ckpt_run(&fs, ckpt_policy(Durability::Strict, 64, 1 << 50));
        fs.fail_write(std::io::Error::other("the device hiccupped"));
        let e = match stage_ckpt(&fs, &root, &run, Durability::Strict, &no_heartbeat) {
            Err(e) => e,
            Ok(_) => panic!("the write fault must stop the copy"),
        };
        assert_eq!(e.step, CopyStep::Stream);
        run.abandon();
        // The partial's own writer and its one duplicate: every writer opened on the path is closed again.
        assert_eq!(count(&fs, &format!("duplicate({PARTIAL})")), 1, "{:?}", fs.calls());
        assert_eq!(count(&fs, &format!("drop_writer({PARTIAL})")), 2, "{:?}", fs.calls());
    }

    #[test]
    fn a_wal_failure_keeps_the_partial_and_stops() {
        let (fs, root) = ckpt_fs(3 * MIB);
        let run = ckpt_run(&fs, ckpt_policy(Durability::Strict, 64, 1 << 50));
        // The partial is create #1 and the WAL segment create #2.
        fs.fail_nth("create_new", 2, Code::DiskFull, std::io::ErrorKind::Other);
        let e = match stage_ckpt(&fs, &root, &run, Durability::Strict, &no_heartbeat) {
            Err(e) => e,
            Ok(_) => panic!("the WAL fault must stop the copy"),
        };
        assert_eq!(e.step, CopyStep::Checkpoint);
        assert_eq!(e.cause.code, Code::WalWriteFailed);
        assert!(e.leftover.is_none());
        assert!(e.cause.source.to_string().contains("dst"), "{}", e.cause.source);
        assert!(fs.exists(PARTIAL), "the partial is KEPT: {:?}", fs.calls());
        run.abandon();
    }

    #[test]
    fn a_data_sync_failure_discards_the_partial_like_todays_strict_failure() {
        let (fs, root) = ckpt_fs(3 * MIB);
        let run = ckpt_run(&fs, ckpt_policy(Durability::Strict, 64, 1 << 50));
        // The first sync is the duplicate's, at the file's Close.
        fs.fail("sync_all", Code::StrictDurabilityUnavailable);
        let e = match stage_ckpt(&fs, &root, &run, Durability::Strict, &no_heartbeat) {
            Err(e) => e,
            Ok(_) => panic!("the data sync fault must stop the copy"),
        };
        assert_eq!(e.step, CopyStep::Durability);
        assert_eq!(e.cause.code, Code::StrictDurabilityUnavailable);
        assert!(!fs.exists(PARTIAL), "the partial is removed: {:?}", fs.calls());
        run.abandon();
    }

    #[test]
    fn a_full_channel_keeps_the_heartbeat_beating_and_a_heartbeat_failure_stops_the_copy() {
        // One range per chunk and a flush per range, so the WAL thread meets its first duplicate sync after chunk 1.
        let policy = CheckpointPolicy { range_chunks: 1, ..ckpt_policy(Durability::Strict, 2, 1) };
        // 5 MiB is 20 buffers: at most 22 beats happen without a full channel, so 40 beats while the thread is
        // still blocked can only come from `send`'s wait on the full channel.
        const BEATS: usize = 40;
        for fail_the_heartbeat in [false, true] {
            let (fs, root) = ckpt_fs(5 * MIB);
            let run = ckpt_run(&fs, policy);
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let release_tx = std::sync::Mutex::new(release_tx);
            fs.on_nth("sync_all", 1, move |_| {
                release_rx.recv_timeout(Duration::from_secs(60)).expect("released");
            });
            let beats = AtomicUsize::new(0);
            let released = AtomicBool::new(false);
            let beat = || {
                let n = beats.fetch_add(1, Ordering::SeqCst) + 1;
                if n >= BEATS && fail_the_heartbeat {
                    // A lost lock stays lost: the abort's own send must see the failure too.
                    return Err(FsError::new(Code::IoError, std::io::Error::other("lock lost")));
                }
                if n >= BEATS && !released.swap(true, Ordering::SeqCst) {
                    release_tx.lock().unwrap().send(()).unwrap();
                }
                Ok(())
            };
            let result = stage_ckpt(&fs, &root, &run, Durability::Strict, &beat);
            assert!(beats.load(Ordering::SeqCst) >= BEATS);
            if fail_the_heartbeat {
                let e = match result {
                    Err(e) => e,
                    Ok(_) => panic!("a failed heartbeat stops the copy"),
                };
                assert_eq!(e.step, CopyStep::Heartbeat);
                assert!(!fs.exists(PARTIAL), "the partial is removed");
                release_tx.lock().unwrap().send(()).unwrap();
                run.abandon();
            } else {
                assert!(result.is_ok(), "the copy outlives a blocked flush");
                assert!(run.finish(&no_heartbeat).is_clean());
            }
        }
    }

    #[test]
    fn a_checkpointing_run_sweeps_after_the_claim_behind_its_own_guard() {
        for (len, label) in [(512 * 1024, "small"), (3 * MIB, "large")] {
            let (fs, root) = ckpt_fs(len);
            fs.write_file(PARTIAL, b"leftover");
            let run = ckpt_run(&fs, ckpt_policy(Durability::Normal, 64, 1 << 50));
            // Each label records how many fake calls had happened when it fired, so the merge below is exact.
            let marks = std::cell::RefCell::new(Vec::<(usize, &'static str)>::new());
            let mark = |l: &'static str| marks.borrow_mut().push((fs.calls().len(), l));
            let guard = || {
                mark("guard");
                Ok(())
            };
            let beat = || {
                mark("beat");
                Ok(())
            };
            let claim = || {
                mark("claim");
                Ok(())
            };
            let fc = FileCkpt { run: &run, file_id: b"f".to_vec(), destination: b"dst".to_vec() };
            let staged = stage_file(
                &fs,
                Path::new("/src"),
                &root,
                OsStr::new("dst"),
                &opts(),
                &guard,
                &beat,
                &claim,
                SyncAt::Staging,
                Some(Hook::Logged(&fc)),
            );
            assert!(matches!(staged, Ok(Stage::Staged(_))), "{label}");
            run.abandon();
            let calls = fs.calls();
            let mut order: Vec<String> = Vec::new();
            for (i, c) in calls.iter().enumerate() {
                for (at, l) in marks.borrow().iter() {
                    if *at == i {
                        order.push((*l).to_string());
                    }
                }
                order.push(c.clone());
            }
            for (at, l) in marks.borrow().iter() {
                if *at >= calls.len() {
                    order.push((*l).to_string());
                }
            }
            let removes: Vec<usize> = order
                .iter()
                .enumerate()
                .filter(|(_, c)| c.starts_with(&format!("remove_file({PARTIAL})")))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(removes.len(), 1, "{label}: exactly one sweep: {order:?}");
            let remove = removes[0];
            let claim_at = order.iter().position(|c| c == "claim").expect("claimed");
            let create = order
                .iter()
                .position(|c| c.starts_with(&format!("create_new({PARTIAL})")))
                .expect("created");
            assert!(remove > claim_at, "{label}: the sweep follows the claim: {order:?}");
            assert_eq!(order[remove - 1], "guard", "{label}: its own guard: {order:?}");
            assert!(create > remove, "{label}: the create follows the sweep: {order:?}");
            // Step 1 keeps its guard but sweeps nothing: no removal follows the first guard directly.
            let first_guard = order.iter().position(|c| c == "guard").expect("guarded");
            assert!(!order[first_guard + 1].starts_with("remove_file("), "{label}: {order:?}");
        }
    }

    #[test]
    fn staging_at_the_barrier_keeps_the_writer_open_and_does_not_sync() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let mut o = opts();
        o.durability = Durability::Strict; // under `Normal` nothing syncs whatever `sync` is, so the assertion would be vacuous
        let staged = stage_with(&fs, &root, &o, SyncAt::Barrier);
        assert!(staged.writer.is_some());
        assert!(!fs.called("sync_all("), "{:?}", fs.calls());
        assert_eq!(fs.read_file("/dst.flux-partial.op1").as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn staging_inline_syncs_once_after_the_metadata_step_and_returns_no_writer() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let mut o = opts(); // `opts()` already sets `preserve_times: Preserve::Default`, so `set_times` is called
        o.durability = Durability::Strict;
        let staged = stage_with(&fs, &root, &o, SyncAt::Staging);
        assert!(staged.writer.is_none());
        let c = fs.calls();
        let sync = c.iter().position(|x| x.starts_with("sync_all(")).expect("synced");
        let times = c.iter().position(|x| x.starts_with("set_times(")).expect("times set");
        assert!(times < sync, "metadata before the sync: {c:?}");
        assert_eq!(c.iter().filter(|x| x.starts_with("sync_all(")).count(), 1);
    }

    // ---- cut 9h, task 5: the copy loop hands written buffers to the hashing worker ----
    //
    // The worker tests need more than one CPU (`Worker::wanted`: Normal durability and more than one CPU); each
    // asserts `test_seams::spawned() > 0`, so on a one-CPU machine they fail loudly instead of passing without a
    // worker. `FORCE`, the spawn and the job counters are thread-locals: a test that stages on a helper thread sets
    // and reads them there.

    use crate::checkpoint::test_seams;

    /// The fixture of the cut 9h copy tests: 6 MiB, 256 KiB buffers (24 writes), one `Range` per 4 chunks, every
    /// record flushed at once.
    const HASH_SRC: usize = 6 * MIB;

    fn hash_policy() -> CheckpointPolicy {
        ckpt_policy(Durability::Normal, 64, 1)
    }

    /// Runs `f` on a helper thread and waits at most 10 s for it: a hang fails the test instead of the suite. A panic in
    /// `f` is resumed here.
    fn within_deadline<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(v) => v,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("{what}: hung past the 10 s deadline")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
                Err(p) => std::panic::resume_unwind(p),
                Ok(()) => panic!("{what}: the helper thread ended without a result"),
            },
        }
    }

    fn ranges_of(bodies: &[Body]) -> Vec<&crate::wal::record::Range> {
        bodies.iter().filter_map(|b| if let Body::Range(r) = b { Some(r) } else { None }).collect()
    }

    #[test]
    fn no_job_for_a_buffer_whose_write_failed() {
        // On a helper thread (the job counter is its thread-local), so a hang fails the test.
        within_deadline("write fault", no_job_for_a_buffer_whose_write_failed_body);
    }

    fn no_job_for_a_buffer_whose_write_failed_body() {
        // Write 16 to the partial is the buffer that would complete chunk 3 (16 x 256 KiB = 4 MiB).
        const K: u32 = 16;
        let (fs, root) = ckpt_fs(HASH_SRC);
        let run = ckpt_run(&fs, hash_policy());
        fs.fail_write_nth("flux-partial", K, std::io::ErrorKind::StorageFull);
        let (spawned, submitted) = (test_seams::spawned(), test_seams::submitted());
        let e = match stage_ckpt(&fs, &root, &run, Durability::Normal, &no_heartbeat) {
            Err(e) => e,
            Ok(_) => panic!("the failed write stops the copy"),
        };
        assert_eq!((e.step, e.cause.code), (CopyStep::Stream, Code::CopyFailed));
        assert_eq!(e.cause.source.kind(), std::io::ErrorKind::StorageFull);
        assert!(!fs.exists(PARTIAL), "the partial is discarded: {:?}", fs.calls());
        assert!(test_seams::spawned() > spawned, "a flux-hash worker hashed this file");
        assert_eq!(
            test_seams::submitted() - submitted,
            (K - 1) as usize,
            "only the buffers whose write succeeded were handed to the worker"
        );
        assert!(run.finish(&no_heartbeat).is_clean());
        // The `FileBegin` is flushed only with a `Range` and the discard dropped it, so the honest log may be absent.
        // A smoke check only: it passes with or without the mutant. The job count above is the real
        // assertion (R9).
        let bodies = if fs.exists(WAL_PATH) { wal_bodies(&fs) } else { Vec::new() };
        assert!(
            ranges_of(&bodies).iter().all(|r| r.range_end <= 3 * MIB as u64),
            "no Range covers bytes that never reached the partial: {bodies:?}"
        );
    }

    #[test]
    fn a_failed_read_write_or_beat_does_not_hang() {
        #[derive(Debug, Clone, Copy)]
        enum Fault {
            Read,
            Write,
            Beat,
        }
        for fault in [Fault::Read, Fault::Write, Fault::Beat] {
            let (step, code, spawned, live_before, live_after) =
                within_deadline(&format!("{fault:?}"), move || {
                    let (fs, root) = ckpt_fs(HASH_SRC);
                    let run = ckpt_run(&fs, hash_policy());
                    match fault {
                        Fault::Read => fs.fail_read_nth("/src", 8, std::io::ErrorKind::TimedOut),
                        Fault::Write => {
                            fs.fail_write_nth("flux-partial", 8, std::io::ErrorKind::StorageFull)
                        }
                        Fault::Beat => {}
                    }
                    let beats = AtomicUsize::new(0);
                    let beat = || {
                        if matches!(fault, Fault::Beat)
                            && beats.fetch_add(1, Ordering::SeqCst) + 1 == 5
                        {
                            return Err(FsError::new(
                                Code::IoError,
                                std::io::Error::other("lock lost"),
                            ));
                        }
                        Ok(())
                    };
                    let (spawned, live) = (test_seams::spawned(), test_seams::live());
                    let e = match stage_ckpt(&fs, &root, &run, Durability::Normal, &beat) {
                        Err(e) => e,
                        Ok(_) => panic!("{fault:?}: the fault stops the copy"),
                    };
                    assert!(!fs.exists(PARTIAL), "{fault:?}: the partial is discarded");
                    let live_after = test_seams::live();
                    run.abandon();
                    (e.step, e.cause.code, test_seams::spawned() - spawned, live, live_after)
                });
            let want = match fault {
                Fault::Read | Fault::Write => (CopyStep::Stream, Code::CopyFailed),
                Fault::Beat => (CopyStep::Heartbeat, Code::IoError),
            };
            assert_eq!((step, code), want, "{fault:?}");
            assert!(spawned > 0, "{fault:?}: a flux-hash worker hashed this file");
            assert_eq!(
                live_after, live_before,
                "{fault:?}: no flux-hash thread outlives the failed copy"
            );
        }
    }

    #[test]
    fn a_panic_in_the_copy_thread_does_not_leave_a_worker_blocked() {
        let spawned = within_deadline("panic", || {
            let (fs, root) = ckpt_fs(HASH_SRC);
            let run = ckpt_run(&fs, hash_policy());
            let beats = AtomicUsize::new(0);
            let beat = || {
                if beats.fetch_add(1, Ordering::SeqCst) + 1 == 5 {
                    // Let the worker drain its queue first: an idle worker waits on its job channel, the state in
                    // which only the channel's disconnect (not the cancel flag) can wake it.
                    std::thread::sleep(Duration::from_millis(200));
                    panic!("injected panic in the copy thread's heartbeat");
                }
                Ok(())
            };
            // The live count is per spawning thread: this helper thread is the spawner.
            let (spawned, live_before) = (test_seams::spawned(), test_seams::live());
            let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = stage_ckpt(&fs, &root, &run, Durability::Normal, &beat);
            }));
            assert!(unwound.is_err(), "the heartbeat panicked");
            run.abandon();
            let end = std::time::Instant::now() + Duration::from_secs(5);
            while test_seams::live() != live_before {
                assert!(
                    std::time::Instant::now() < end,
                    "a flux-hash thread outlived the panicked copy"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            test_seams::spawned() - spawned
        });
        assert!(spawned > 0, "a flux-hash worker was running when the copy thread panicked");
    }

    #[test]
    fn a_checkpointed_copy_logs_the_same_ranges_with_and_without_the_worker() {
        let logged = |one_cpu: bool| {
            within_deadline(if one_cpu { "inline" } else { "worker" }, move || {
                let _force = one_cpu.then(test_seams::OneCpu::new);
                let spawned = test_seams::spawned();
                let (fs, root) = ckpt_fs(HASH_SRC);
                let run = ckpt_run(&fs, hash_policy());
                let staged =
                    staged_of(stage_ckpt(&fs, &root, &run, Durability::Normal, &no_heartbeat));
                assert_eq!(staged.bytes_copied, HASH_SRC as u64);
                assert!(run.finish(&no_heartbeat).is_clean());
                assert_eq!(
                    fs.read_file(PARTIAL),
                    fs.read_file("/src"),
                    "the partial holds the source"
                );
                (wal_bodies(&fs), test_seams::spawned() - spawned)
            })
        };
        let (inline, inline_spawned) = logged(true);
        let (worker, worker_spawned) = logged(false);
        assert_eq!(inline_spawned, 0, "one CPU hashes inline");
        assert!(worker_spawned > 0, "a flux-hash worker hashed the second copy");
        assert_eq!(ranges_of(&inline).len(), 2, "6 MiB in ranges of 4 chunks: {inline:?}");
        assert_eq!(worker, inline);
    }

    use crate::fault_fs::FakeHandle;
    use std::cell::Cell;
    use std::thread::ThreadId;
    use std::time::Duration;

    fn handles(fs: &FaultFs, n: usize) -> Vec<FakeHandle> {
        (0..n).map(|i| fs.create_new(Path::new(&format!("/t{i}"))).unwrap()).collect()
    }

    /// A heartbeat that counts its calls and remembers the thread of the last one.
    type Calls = std::rc::Rc<Cell<u32>>;
    type Thread = std::rc::Rc<Cell<Option<ThreadId>>>;

    fn counting_beat() -> (impl Fn() -> flux_fs::Result<()>, Calls, Thread) {
        let count = std::rc::Rc::new(Cell::new(0u32));
        let thread = std::rc::Rc::new(Cell::new(None));
        let (c, t) = (std::rc::Rc::clone(&count), std::rc::Rc::clone(&thread));
        let beat = move || {
            c.set(c.get() + 1);
            t.set(Some(std::thread::current().id()));
            Ok(())
        };
        (beat, count, thread)
    }

    #[test]
    fn barrier_results_are_positional_and_every_writer_is_synced_once() {
        let fs = FaultFs::new();
        let ws = handles(&fs, 5);
        let refs: Vec<&FakeHandle> = ws.iter().collect();
        let out = sync_staged_many(&refs, 16, &no_heartbeat, Duration::from_secs(5));
        assert_eq!(out.results.len(), 5);
        assert!(out.results.iter().all(Result::is_ok) && out.heartbeat.is_none());
        assert_eq!(fs.calls().iter().filter(|c| c.starts_with("sync_all(")).count(), 5);
    }

    #[test]
    fn a_single_writer_syncs_on_the_main_thread_without_a_worker() {
        // Mutant: remove the `n == 1` branch -> one worker is spawned -> `workers == 1`.
        let fs = FaultFs::new();
        let ws = handles(&fs, 1);
        let refs: Vec<&FakeHandle> = ws.iter().collect();
        let out = sync_staged_many(&refs, 16, &no_heartbeat, Duration::from_secs(5));
        assert_eq!(out.workers, 0);
        assert_eq!(out.results.len(), 1);
        assert!(out.results[0].is_ok() && out.heartbeat.is_none());
        assert_eq!(fs.calls().iter().filter(|c| c.starts_with("sync_all(")).count(), 1);
    }

    #[test]
    fn two_writers_still_use_workers() {
        // Guards the mutant that routes everything to the main thread.
        let fs = FaultFs::new();
        let ws = handles(&fs, 2);
        let refs: Vec<&FakeHandle> = ws.iter().collect();
        let out = sync_staged_many(&refs, 16, &no_heartbeat, Duration::from_secs(5));
        assert!(out.workers >= 1);
        assert!(out.results.iter().all(Result::is_ok));
        assert_eq!(out.results.len(), 2);
    }

    #[test]
    fn a_faulted_writer_fails_at_its_own_index_only() {
        let fs = FaultFs::new();
        fs.on_nth("create_new", 3, |fs| fs.fail("sync_all", Code::StrictDurabilityUnavailable));
        let ws = handles(&fs, 4);
        let refs: Vec<&FakeHandle> = ws.iter().collect();
        let out = sync_staged_many(&refs, 2, &no_heartbeat, Duration::from_secs(5));
        let failed: Vec<usize> =
            out.results.iter().enumerate().filter(|(_, r)| r.is_err()).map(|(i, _)| i).collect();
        assert_eq!(failed, [2]);
        assert_eq!(out.results[2].as_ref().unwrap_err().code, Code::StrictDurabilityUnavailable);
    }

    #[test]
    fn one_thread_syncs_in_order() {
        let fs = FaultFs::new();
        let ws = handles(&fs, 3);
        let refs: Vec<&FakeHandle> = ws.iter().collect();
        sync_staged_many(&refs, 1, &no_heartbeat, Duration::from_secs(5));
        let synced: Vec<String> =
            fs.calls().into_iter().filter(|c| c.starts_with("sync_all(")).collect();
        assert_eq!(synced, ["sync_all(/t0)", "sync_all(/t1)", "sync_all(/t2)"]);
    }

    #[test]
    fn the_heartbeat_is_called_from_the_main_thread_once_the_interval_has_passed_in_total() {
        // 5 syncs x 20 ms = 100 ms, one worker; a 25 ms deadline must fire at least once. A fresh full timeout per
        // receive (the mutant) would never reach 25 ms, because a result lands every 20 ms.
        let fs = FaultFs::new();
        fs.delay("sync_all", Duration::from_millis(20));
        let ws = handles(&fs, 5);
        let refs: Vec<&FakeHandle> = ws.iter().collect();
        let (beat, count, thread) = counting_beat();
        let out = sync_staged_many(&refs, 1, &beat, Duration::from_millis(25));
        assert!(out.heartbeat.is_none());
        assert!(count.get() >= 1, "no heartbeat during a 100 ms barrier");
        assert_eq!(thread.get(), Some(std::thread::current().id()));
    }

    #[test]
    fn sixteen_threads_sync_concurrently() {
        // Deterministic: every sync waits for a second one to be in flight (or 2 s). With real workers the two meet at
        // once and the peak is >= 2; under the mutant "ignore `threads`, one worker" each sync waits out the 2 s alone and
        // the peak stays 1 (red, and slow: a mutant run, not CI). No sleep, no scheduler race.
        let fs = FaultFs::new();
        fs.rendezvous("sync_all", 2, Duration::from_secs(2));
        let ws = handles(&fs, 8);
        let refs: Vec<&FakeHandle> = ws.iter().collect();
        let started = std::time::Instant::now();
        sync_staged_many(&refs, 16, &no_heartbeat, Duration::from_secs(5));
        assert!(fs.peak_concurrent_syncs() >= 2, "{}", fs.peak_concurrent_syncs());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the syncs met; nobody waited out the timeout"
        );
    }

    #[test]
    fn a_failed_heartbeat_ends_the_wait_after_the_join_and_is_reported_once() {
        let fs = FaultFs::new();
        fs.delay("sync_all", Duration::from_millis(20));
        let ws = handles(&fs, 4);
        let refs: Vec<&FakeHandle> = ws.iter().collect();
        let calls = Cell::new(0u32);
        let beat = || {
            calls.set(calls.get() + 1);
            Err(FsError::new(Code::IoError, std::io::Error::other("torn")))
        };
        let out = sync_staged_many(&refs, 1, &beat, Duration::from_millis(10));
        assert_eq!(out.heartbeat.as_ref().map(|e| e.code), Some(Code::IoError));
        assert_eq!(calls.get(), 1, "no second heartbeat call after a failure");
        assert_eq!(
            out.results.len(),
            4,
            "every writer still has a result: the workers were joined"
        );
        assert_eq!(fs.calls().iter().filter(|c| c.starts_with("sync_all(")).count(), 4);
    }

    #[test]
    fn zero_threads_syncs_on_the_main_thread_in_order_and_beats_between_syncs() {
        // 4 x 20 ms syncs on the main thread with a 10 ms deadline: at least one beat lands between syncs, from the main
        // thread, results are positional and the order is t0..t3. Mutant: remove the beat check from the fallback -> count 0.
        let fs = FaultFs::new();
        fs.delay("sync_all", Duration::from_millis(20));
        let ws = handles(&fs, 4);
        let refs: Vec<&FakeHandle> = ws.iter().collect();
        let (beat, count, thread) = counting_beat();
        let out = sync_staged_many(&refs, 0, &beat, Duration::from_millis(10));
        assert_eq!(out.workers, 0);
        assert_eq!(out.results.len(), 4);
        assert!(out.results.iter().all(Result::is_ok) && out.heartbeat.is_none());
        assert!(count.get() >= 1, "no heartbeat between main-thread syncs");
        assert_eq!(thread.get(), Some(std::thread::current().id()));
        let synced: Vec<String> =
            fs.calls().into_iter().filter(|c| c.starts_with("sync_all(")).collect();
        assert_eq!(synced, ["sync_all(/t0)", "sync_all(/t1)", "sync_all(/t2)", "sync_all(/t3)"]);
    }

    #[test]
    fn zero_threads_reports_a_failed_sync_at_its_index_and_a_failed_heartbeat_once() {
        let fs = FaultFs::new();
        fs.delay("sync_all", Duration::from_millis(20));
        fs.on_nth("create_new", 2, |fs| fs.fail("sync_all", Code::StrictDurabilityUnavailable));
        let ws = handles(&fs, 3);
        let refs: Vec<&FakeHandle> = ws.iter().collect();
        let calls = Cell::new(0u32);
        let beat = || {
            calls.set(calls.get() + 1);
            Err(FsError::new(Code::IoError, std::io::Error::other("torn")))
        };
        let out = sync_staged_many(&refs, 0, &beat, Duration::from_millis(10));
        assert_eq!(out.workers, 0);
        assert_eq!(
            out.results.iter().map(Result::is_err).collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(out.heartbeat.as_ref().map(|e| e.code), Some(Code::IoError));
        assert_eq!(calls.get(), 1, "no second heartbeat call after a failure");
        assert_eq!(
            fs.calls().iter().filter(|c| c.starts_with("sync_all(")).count(),
            3,
            "every writer still synced"
        );
    }

    fn plan_fixture() -> FaultFs {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        fs.create_dir(Path::new("/d")).unwrap();
        put(&fs, "/s/f", b"new", at_secs(20));
        fs
    }

    fn plan_with(
        fs: &FaultFs,
        existing: flux_fs::ExistingPolicy,
    ) -> std::result::Result<FilePlan, CopyError> {
        let root = fs.destination_root(Path::new("/d")).unwrap();
        let mut o = opts();
        o.existing = existing;
        plan_file(fs, Path::new("/s/f"), Some(&root), OsStr::new("f"), &o)
    }

    fn assert_no_writes(fs: &FaultFs) {
        for c in fs.calls() {
            for bad in ["remove_file(", "rename_", "open_read("] {
                assert!(!c.starts_with(bad), "{c}");
            }
            // The fixture's own `put` calls are `create_new(` too; only a destination create is a write here.
            assert!(!c.starts_with("create_new(/d/f.flux-partial"), "{c}");
        }
    }

    #[test]
    fn plan_file_answers_copy_overwrite_and_each_skip_without_writing() {
        use crate::dry_run::SkipReason;
        use flux_fs::ExistingPolicy::{Overwrite, SkipExisting, Update};

        // absent destination
        let fs = plan_fixture();
        match plan_with(&fs, Overwrite).unwrap() {
            FilePlan::Proceed { src_meta, degraded, replaces } => {
                assert_eq!(src_meta.len, 3);
                assert!(degraded.is_none());
                assert!(!replaces);
            }
            FilePlan::Skip { .. } => panic!("absent destination must proceed"),
        }
        assert_no_writes(&fs);

        // existing, older, same length
        let fs = plan_fixture();
        put(&fs, "/d/f", b"old", at_secs(10));
        assert!(matches!(
            plan_with(&fs, Overwrite).unwrap(),
            FilePlan::Proceed { replaces: true, .. }
        ));
        assert!(matches!(
            plan_with(&fs, SkipExisting).unwrap(),
            FilePlan::Skip { reason: SkipReason::Existing, .. }
        ));
        // Same length, source newer (20 > 10): Update proceeds.
        assert!(matches!(
            plan_with(&fs, Update).unwrap(),
            FilePlan::Proceed { replaces: true, .. }
        ));
        assert_no_writes(&fs);

        // existing destination at least as new and the same length: up to date
        let fs = plan_fixture();
        put(&fs, "/d/f", b"old", at_secs(20));
        assert!(matches!(
            plan_with(&fs, Update).unwrap(),
            FilePlan::Skip { reason: SkipReason::UpToDate, .. }
        ));
        assert_no_writes(&fs);

        // existing, shorter
        let fs = plan_fixture();
        put(&fs, "/d/f", b"x", at_secs(20));
        assert!(matches!(
            plan_with(&fs, Update).unwrap(),
            FilePlan::Proceed { replaces: true, .. }
        ));
        assert_no_writes(&fs);
    }

    #[test]
    fn plan_file_without_a_destination_handle_stats_the_source_and_runs_no_gate() {
        // The destination directory does not exist in this fake.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        put(&fs, "/s/f", b"new", at_secs(20));
        let mut o = opts();
        o.existing = flux_fs::ExistingPolicy::Overwrite;
        let plan = plan_file::<FaultFs>(&fs, Path::new("/s/f"), None, OsStr::new("f"), &o).unwrap();
        assert!(matches!(plan, FilePlan::Proceed { degraded: None, replaces: false, .. }));
        let calls = fs.calls();
        assert!(calls.iter().any(|c| c == "metadata(/s/f)"), "{calls:?}");
        assert!(calls.iter().all(|c| !c.contains("/d")), "{calls:?}");
    }

    #[test]
    fn plan_file_refuses_a_special_source_and_a_directory_at_the_name_as_stage_file_did() {
        let fs = plan_fixture();
        fs.set_type("/s/f", FileType::Other);
        let err = plan_with(&fs, flux_fs::ExistingPolicy::Overwrite).err().expect("special source");
        assert_eq!(err.code(), Code::SpecialFileUnsupported);
        assert_eq!(err.step, CopyStep::Source);

        let fs = plan_fixture();
        fs.create_dir(Path::new("/d/f")).unwrap();
        let err = plan_with(&fs, flux_fs::ExistingPolicy::Overwrite)
            .err()
            .expect("directory at the name");
        assert_eq!(err.code(), Code::SafetyRejected);
        assert_eq!(err.step, CopyStep::Gate);
    }

    #[test]
    fn stage_file_leaves_a_closed_temporary_and_no_target() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let staged = stage_hello(&fs, &root, SyncAt::Staging);

        assert_eq!(staged.bytes_copied, 5);
        assert_eq!(staged.temp, OsString::from("dst.flux-partial.op1"));
        assert!(
            fs.calls().iter().any(|c| c.starts_with("create_new(/dst.flux-partial.op1)")),
            "{:?}",
            fs.calls()
        );
        assert!(!fs.called("rename_"), "{:?}", fs.calls());
        assert_eq!(fs.read_file("/dst.flux-partial.op1").as_deref(), Some(&b"hello"[..]));
        assert!(!fs.exists("/dst"));
    }

    #[test]
    fn publish_staged_renames_and_reports_the_outcome() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let staged = stage_hello(&fs, &root, SyncAt::Staging);
        let identity = staged.identity;
        let out = publish_staged(
            &root,
            OsStr::new("dst"),
            staged,
            Publish::NoReplace,
            &unguarded,
            &no_heartbeat,
        )
        .unwrap();

        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
        assert!(!fs.exists("/dst.flux-partial.op1"));
        assert_eq!(out.published_identity, identity);
        assert_eq!(out.bytes_copied, 5);
        assert!(!out.skipped);
        assert!(fs.called("rename_no_replace"), "{:?}", fs.calls());
    }

    #[test]
    fn publish_staged_with_a_lost_guard_keeps_the_temporary_as_leftover() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let staged = stage_hello(&fs, &root, SyncAt::Staging);
        let e = publish_staged(
            &root,
            OsStr::new("dst"),
            staged,
            Publish::Replace,
            &lost,
            &no_heartbeat,
        )
        .unwrap_err();

        assert_eq!((e.code(), e.step), (Code::TargetLockBusy, CopyStep::Publish));
        let (left, _) = e.leftover.as_ref().expect("the kept temporary is reported");
        assert_eq!(left, Path::new("dst.flux-partial.op1"));
        assert!(fs.exists("/dst.flux-partial.op1"), "nothing removed");
        assert!(!fs.exists("/dst"));
        assert!(!fs.called("rename_"), "{:?}", fs.calls());
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
        // The buffer is at most COPY_BUF_MAX; this forces several iterations and a short final read.
        let fs = FaultFs::new();
        let big: Vec<u8> = (0..(COPY_BUF_MAX * 2 + 7)).map(|i| (i % 251) as u8).collect();
        fs.write_file("/src", &big);

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.bytes_copied, big.len() as u64);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&big[..]));
    }

    #[test]
    fn the_copy_buffer_is_the_source_length_clamped_to_4_kib_and_256_kib() {
        let table: [(u64, usize); 10] = [
            (0, 4096),
            (1, 4096),
            (4095, 4096),
            (4096, 4096),
            (10_000, 10_000),
            (262_143, 262_143),
            (262_144, 262_144),
            (262_145, 262_144),
            (4_294_967_296, 262_144),
            (u64::MAX, 262_144),
        ];
        for (len, want) in table {
            assert_eq!(copy_buffer_len(len), want, "source length {len}");
        }
    }

    #[test]
    fn an_empty_source_copies_to_an_empty_file() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"");
        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();
        assert_eq!(out.bytes_copied, 0);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b""[..]));
    }

    #[test]
    fn a_source_of_exactly_the_cap_and_one_byte_over_copy_every_byte() {
        for len in [COPY_BUF_MAX, COPY_BUF_MAX + 1] {
            let fs = FaultFs::new();
            // Non-repeating over the whole length: a short or repeated read changes some byte.
            let data: Vec<u8> = (0..len).map(|i| ((i * 31 + i / 251) % 256) as u8).collect();
            fs.write_file("/src", &data);
            let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();
            assert_eq!(out.bytes_copied, len as u64, "length {len}");
            assert_eq!(fs.read_file("/dst").as_deref(), Some(&data[..]), "length {len}");
        }
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
    fn a_strict_metadata_failure_is_reported_before_the_sync_runs() {
        // Owner ruling 2026-10-12: the first failing step reports its own code, in step order. The sync follows the
        // metadata (cut 9e), so with both failing the metadata code is reported and the sync is never attempted.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("set_times", flux_fs::Code::PermissionDenied);
        fs.fail("sync_all", flux_fs::Code::IoError);

        let mut o = opts();
        o.preserve_times = Preserve::Strict;
        o.durability = Durability::Strict;
        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::MetadataApplyFailed);
        assert!(
            !fs.called("sync_all"),
            "the sync must not run after a Strict metadata failure: {:?}",
            fs.calls()
        );
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

        let err = CopyError::at(
            CopyStep::Resolve,
            FsError::new(Code::IoError, std::io::Error::from_raw_os_error(5)),
        );

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

    #[test]
    fn between_two_weak_identities_the_destination_is_reported() {
        // `weaker`'s tie rule: both sides Weak, the DESTINATION's volume keys the
        // warning, because that is the side the user did not name. (Test audit, cut 4a.)
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"old");
        fs.set_identity("/src", weak(3));
        fs.set_identity("/dst", weak(7));

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.identity_degraded, Some(weak(7)));
    }

    #[test]
    fn every_failure_names_the_step_it_failed_at() {
        let step = |fs: &FaultFs, src: &str, dst: &str, o: &CopyOptions| {
            copy_file(fs, Path::new(src), Path::new(dst), o).unwrap_err().step
        };
        let with_src = || {
            let fs = FaultFs::new();
            fs.write_file("/src", b"hello");
            fs
        };

        // Resolve: the lexical refusal, a destination naming no file, a missing parent.
        assert_eq!(step(&with_src(), "/src", "/src", &opts()), CopyStep::Resolve);
        assert_eq!(step(&with_src(), "/src", "/", &opts()), CopyStep::Resolve);
        assert_eq!(step(&with_src(), "/src", "/missing/dst", &opts()), CopyStep::Resolve);

        // Source: missing, not a regular file, cannot be opened.
        assert_eq!(step(&FaultFs::new(), "/nope", "/dst", &opts()), CopyStep::Source);
        let special = FaultFs::new();
        special.add_special("/src");
        assert_eq!(step(&special, "/src", "/dst", &opts()), CopyStep::Source);
        let unreadable = with_src();
        unreadable.fail("open_read", flux_fs::Code::PermissionDenied);
        assert_eq!(step(&unreadable, "/src", "/dst", &opts()), CopyStep::Source);

        // Gate: the destination is the source by identity.
        let alias = with_src();
        alias.write_file("/dst", b"hello");
        let id = flux_fs::FileIdentity::Strong(flux_fs::ObjectId { volume: 1, index: 9_001 });
        alias.set_identity("/src", id);
        alias.set_identity("/dst", id);
        assert_eq!(step(&alias, "/src", "/dst", &opts()), CopyStep::Gate);

        // Create, Stream, Durability, Metadata, Recheck, Publish.
        let create = with_src();
        create.fail("create_new", flux_fs::Code::PermissionDenied);
        assert_eq!(step(&create, "/src", "/dst", &opts()), CopyStep::Create);

        let stream = with_src();
        stream.fail_write(std::io::Error::other("the device hiccupped"));
        assert_eq!(step(&stream, "/src", "/dst", &opts()), CopyStep::Stream);

        let durable = with_src();
        durable.fail("sync_all", flux_fs::Code::IoError);
        let mut strict_sync = opts();
        strict_sync.durability = Durability::Strict;
        assert_eq!(step(&durable, "/src", "/dst", &strict_sync), CopyStep::Durability);

        let meta = with_src();
        meta.fail("set_times", flux_fs::Code::PermissionDenied);
        let mut strict_times = opts();
        strict_times.preserve_times = Preserve::Strict;
        assert_eq!(step(&meta, "/src", "/dst", &strict_times), CopyStep::Metadata);

        let recheck = with_src();
        recheck.vanish_on_second_metadata("/src");
        assert_eq!(step(&recheck, "/src", "/dst", &opts()), CopyStep::Recheck);

        let publish = with_src();
        publish.fail("rename_replace", flux_fs::Code::PermissionDenied);
        assert_eq!(step(&publish, "/src", "/dst", &opts()), CopyStep::Publish);
    }

    #[test]
    fn under_no_replace_an_existing_destination_is_refused_before_any_bytes_are_copied() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"existing");
        let mut o = opts();
        o.publish = Publish::NoReplace;

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap_err();

        assert_eq!(err.step, CopyStep::Gate);
        assert_eq!(err.code(), flux_fs::Code::IoError);
        assert_eq!(err.cause.source.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(!fs.called("create_new"), "refused before the temporary exists");
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"existing"[..]));
    }

    #[test]
    fn under_no_replace_the_identity_rows_still_come_first() {
        // An identity-equal destination is still SAFETY_REJECTED, not AlreadyExists.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.write_file("/dst", b"hello");
        let id = flux_fs::FileIdentity::Strong(flux_fs::ObjectId { volume: 1, index: 9_002 });
        fs.set_identity("/src", id);
        fs.set_identity("/dst", id);
        let mut o = opts();
        o.publish = Publish::NoReplace;

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap_err();

        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert_eq!(err.step, CopyStep::Gate);
    }

    #[test]
    fn under_replace_an_existing_destination_is_still_replaced() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"new");
        fs.write_file("/dst", b"old");
        copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"new"[..]));
    }

    fn lost() -> flux_fs::Result<()> {
        Err(FsError::new(Code::TargetLockBusy, std::io::Error::other("lost")))
    }

    /// A guard that passes its first `owned` calls and fails every one after, counting them.
    fn guard_failing_after(
        owned: u32,
        calls: &std::cell::Cell<u32>,
    ) -> impl Fn() -> flux_fs::Result<()> + '_ {
        move || {
            calls.set(calls.get() + 1);
            if calls.get() <= owned { Ok(()) } else { lost() }
        }
    }

    #[test]
    fn a_guard_that_fails_first_stops_the_copy_before_anything_is_touched() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let e = copy_file_guarded(
            &fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &opts(),
            &lost,
            &no_heartbeat,
            &no_before_create,
            &no_before_publish,
        )
        .unwrap_err();
        assert_eq!((e.code(), e.step), (Code::TargetLockBusy, CopyStep::Create));
        assert!(!fs.called("remove_file") && !fs.called("create_new"), "{:?}", fs.calls());
        assert!(!fs.exists("/dst"));
    }

    #[test]
    fn a_guard_that_fails_at_the_publish_keeps_the_temporary_and_never_renames() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        // Owned for the step-1 sweep and the create (calls 1 and 2), lost at the publish (call 3).
        let guard = guard_failing_after(2, &calls);
        let e = copy_file_guarded(
            &fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &opts(),
            &guard,
            &no_heartbeat,
            &no_before_create,
            &no_before_publish,
        )
        .unwrap_err();
        assert_eq!((e.code(), e.step), (Code::TargetLockBusy, CopyStep::Publish));
        assert_eq!(calls.get(), 3);
        assert!(!fs.exists("/dst"), "never published");
        assert!(
            fs.exists("/dst.flux-partial.op1"),
            "an unlink is a mutation too: the temporary stays"
        );
        let (left, _) = e.leftover.as_ref().expect("the kept temporary is reported");
        assert_eq!(left, Path::new("dst.flux-partial.op1"));
        assert!(!fs.called("rename_"), "{:?}", fs.calls());
    }

    #[test]
    fn a_guard_that_fails_before_a_discard_keeps_the_temporary_as_a_leftover() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail_write(std::io::Error::other("injected write"));
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        // Owned for the sweep and the create; lost when the failed copy would remove its temporary (call 3).
        let guard = guard_failing_after(2, &calls);
        let e = copy_file_guarded(
            &fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &opts(),
            &guard,
            &no_heartbeat,
            &no_before_create,
            &no_before_publish,
        )
        .unwrap_err();
        assert_eq!(e.step, CopyStep::Stream);
        assert!(e.leftover.is_some(), "the kept temporary is reported");
        assert!(fs.exists("/dst.flux-partial.op1"));
        let removals = fs.calls().iter().filter(|c| c.starts_with("remove_file(")).count();
        assert_eq!(removals, 1, "only the step-1 sweep, never the discard: {:?}", fs.calls());
    }

    /// A heartbeat that counts its calls, and fails the `fail_at`-th and every one after (0: never).
    fn beat_counting(
        fail_at: u32,
        calls: &std::cell::Cell<u32>,
    ) -> impl Fn() -> flux_fs::Result<()> + '_ {
        move || {
            calls.set(calls.get() + 1);
            if fail_at != 0 && calls.get() >= fail_at {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn the_copy_heartbeats_before_each_guarded_mutation_and_every_chunk() {
        let fs = FaultFs::new();
        fs.write_file("/src", &vec![7u8; COPY_BUF_MAX * 2 + 7]);
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        let beat = beat_counting(0, &calls);
        copy_file_guarded(
            &fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &opts(),
            &unguarded,
            &beat,
            &no_before_create,
            &no_before_publish,
        )
        .unwrap();
        // The sweep, the create, three chunks (COPY_BUF_MAX + COPY_BUF_MAX + 7 bytes), the publish.
        assert_eq!(calls.get(), 6);
    }

    #[test]
    fn a_heartbeat_failure_in_the_loop_removes_the_temporary_and_never_publishes() {
        let fs = FaultFs::new();
        fs.write_file("/src", &vec![7u8; COPY_BUF_MAX * 2 + 7]);
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        // The sweep (1) and the create (2) pass; the first chunk's (3) fails.
        let beat = beat_counting(3, &calls);
        let e = copy_file_guarded(
            &fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &opts(),
            &unguarded,
            &beat,
            &no_before_create,
            &no_before_publish,
        )
        .unwrap_err();
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Heartbeat));
        assert_eq!(calls.get(), 3, "no heartbeat after the failed one");
        // Mid-stream, before step 7: the source was stat'ed once (step 2), never re-checked.
        let stats = fs.calls().iter().filter(|c| c.as_str() == "metadata(/src)").count();
        assert_eq!(stats, 1, "{:?}", fs.calls());
        assert!(
            e.leftover.is_none() && !fs.exists("/dst.flux-partial.op1"),
            "the temporary is removed"
        );
        assert!(!fs.exists("/dst") && !fs.called("rename_"), "{:?}", fs.calls());
    }

    #[test]
    fn a_heartbeat_failure_before_the_create_creates_nothing() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        let beat = beat_counting(1, &calls);
        let e = copy_file_guarded(
            &fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &opts(),
            &unguarded,
            &beat,
            &no_before_create,
            &no_before_publish,
        )
        .unwrap_err();
        assert_eq!(e.step, CopyStep::Heartbeat);
        assert!(!fs.called("remove_file") && !fs.called("create_new"), "{:?}", fs.calls());
    }

    #[test]
    fn a_heartbeat_failure_at_the_publish_removes_the_temporary() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        // The sweep (1), the create (2), one chunk (3); the publish's (4) fails.
        let beat = beat_counting(4, &calls);
        let e = copy_file_guarded(
            &fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &opts(),
            &unguarded,
            &beat,
            &no_before_create,
            &no_before_publish,
        )
        .unwrap_err();
        assert_eq!(e.step, CopyStep::Heartbeat);
        assert!(!fs.exists("/dst") && !fs.exists("/dst.flux-partial.op1"), "{:?}", fs.calls());
    }

    #[test]
    fn a_copy_returns_its_published_targets_identity() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        fs.write_file("/s", b"abc");
        let parent = fs.destination_root(Path::new("/d")).unwrap();
        let out = copy_file_at(&fs, Path::new("/s"), &parent, OsStr::new("t"), &opts()).unwrap();
        assert_eq!(out.published_identity, fs.metadata(Path::new("/d/t")).unwrap().identity);
        assert!(matches!(out.published_identity, FileIdentity::Strong(_)));
    }

    /// Write `bytes` at `path` with the given modified time (`write_file` leaves it unavailable).
    fn put(fs: &FaultFs, path: &str, bytes: &[u8], modified: Option<std::time::SystemTime>) {
        let mut w = fs.create_new(Path::new(path)).unwrap();
        std::io::Write::write_all(&mut w, bytes).unwrap();
        fs.set_times(&w, modified).unwrap();
    }

    fn at_secs(s: u64) -> Option<std::time::SystemTime> {
        Some(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(s))
    }

    fn guarded_with(
        fs: &FaultFs,
        o: &CopyOptions,
        before_create: &BeforeCreate<'_>,
    ) -> std::result::Result<Outcome, CopyError> {
        let root = fs.destination_root(Path::new("/")).unwrap();
        copy_file_guarded(
            fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            o,
            &unguarded,
            &no_heartbeat,
            before_create,
            &no_before_publish,
        )
    }

    #[test]
    fn a_before_create_error_stops_the_copy_before_the_temporary_exists() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let claim = || {
            Err(CopyError::at(
                CopyStep::Claim,
                FsError::new(Code::IoError, std::io::Error::other("no claim")),
            ))
        };
        let e = guarded_with(&fs, &opts(), &claim).unwrap_err();
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Claim));
        assert!(!fs.called("create_new"), "{:?}", fs.calls());
        assert!(!fs.exists("/dst") && !fs.exists("/dst.flux-partial.op1"));
        assert!(e.leftover.is_none());
    }

    #[test]
    fn before_create_runs_after_the_gate_and_before_the_create() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let at = std::cell::Cell::new(None);
        let cb = || {
            at.set(Some(fs.calls().len()));
            Ok(())
        };
        guarded_with(&fs, &opts(), &cb).unwrap();
        let ran = at.get().expect("the callback ran");
        let calls = fs.calls();
        let gate = calls.iter().position(|c| c == "metadata(/dst)").expect("the gate stat");
        let create = calls.iter().position(|c| c.starts_with("create_new")).expect("create");
        assert!(gate < ran && ran <= create, "gate {gate}, callback {ran}, create {create}");
    }

    #[test]
    fn the_identity_gate_refusal_skips_before_create() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.write_file("/dst", b"hello");
        let id = FileIdentity::Strong(flux_fs::ObjectId { volume: 1, index: 77 });
        fs.set_identity("/src", id);
        fs.set_identity("/dst", id);
        let ran = std::cell::Cell::new(false);
        let cb = || {
            ran.set(true);
            Ok(())
        };
        let e = guarded_with(&fs, &opts(), &cb).unwrap_err();
        assert_eq!((e.code(), e.step), (Code::SafetyRejected, CopyStep::Gate));
        assert!(!ran.get());
    }

    /// Cut 9c: run the copy with the given guard and `before_publish`.
    fn publishing_with(
        fs: &FaultFs,
        guard: &Guard<'_>,
        before_publish: &BeforePublish<'_>,
    ) -> std::result::Result<Outcome, CopyError> {
        let root = fs.destination_root(Path::new("/")).unwrap();
        copy_file_guarded(
            fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &opts(),
            guard,
            &no_heartbeat,
            &no_before_create,
            before_publish,
        )
    }

    #[test]
    fn before_publish_runs_after_the_recheck_and_before_the_final_guard_with_the_temps_identity() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let seen = std::cell::RefCell::new(Vec::new());
        // The guard leaves a mark in the call log, so its last call (the one directly in front of the rename) is findable.
        let guard = || {
            let _ = fs.metadata(Path::new("/guard-probe"));
            Ok(())
        };
        let hook = |intent: &PublishIntent| {
            seen.borrow_mut().push(intent.clone());
            let _ = fs.metadata(Path::new("/marker"));
            Ok(())
        };
        let out = publishing_with(&fs, &guard, &hook).unwrap();

        let seen = seen.borrow();
        assert_eq!(seen.len(), 1, "called once");
        assert_eq!(seen[0].temp, OsStr::new("dst.flux-partial.op1"));
        assert_eq!(seen[0].identity, fs.metadata(Path::new("/dst")).unwrap().identity);
        assert_eq!(seen[0].identity, out.published_identity);
        assert!(matches!(seen[0].identity, FileIdentity::Strong(_)));

        let calls = fs.calls();
        let marker = calls.iter().position(|c| c == "metadata(/marker)").expect("the hook ran");
        let recheck = calls.iter().rposition(|c| c == "metadata(/src)").expect("the recheck");
        let last_guard =
            calls.iter().rposition(|c| c.starts_with("metadata(/guard-probe")).expect("the guard");
        let rename = calls.iter().position(|c| c.starts_with("rename_")).expect("the rename");
        assert!(recheck < marker, "recheck {recheck}, hook {marker}: {calls:?}");
        assert!(
            marker < last_guard && last_guard < rename,
            "hook {marker}, guard {last_guard}, rename {rename}: {calls:?}"
        );
    }

    #[test]
    fn a_before_publish_error_discards_the_temporary_and_publishes_nothing() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let hook = |_: &PublishIntent| {
            Err(CopyError::at(
                CopyStep::Claim,
                FsError::new(Code::IoError, std::io::Error::other("no note")),
            ))
        };
        let e = publishing_with(&fs, &unguarded, &hook).unwrap_err();
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Claim));
        assert!(!fs.exists("/dst") && !fs.exists("/dst.flux-partial.op1"), "{:?}", fs.calls());
        assert!(!fs.called("rename_"), "{:?}", fs.calls());
        assert!(e.leftover.is_none());
    }

    #[test]
    fn before_publish_is_not_called_when_the_recheck_fails() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.grow_on_second_metadata("/src", b" world");
        let ran = std::cell::Cell::new(0);
        let hook = |_: &PublishIntent| {
            ran.set(ran.get() + 1);
            Ok(())
        };
        let e = publishing_with(&fs, &unguarded, &hook).unwrap_err();
        assert_eq!((e.code(), e.step), (Code::SourceChanged, CopyStep::Recheck));
        assert_eq!(ran.get(), 0);
    }

    #[test]
    fn the_lockless_copy_never_calls_before_publish() {
        // `copy_file_at` passes `no_before_publish`: a completed lockless copy publishes with no hook in the way.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let out = copy_file_at(&fs, Path::new("/src"), &root, OsStr::new("dst"), &opts()).unwrap();
        assert!(fs.exists("/dst"));
        assert!(!out.skipped);
        // The no-op hook is the one `copy_file_at` is built on.
        let intent = PublishIntent { temp: "t".into(), identity: FileIdentity::Unavailable };
        assert!(no_before_publish(&intent).is_ok());
    }

    /// Run the copy with the given guard and heartbeat and a `before_create` that counts its calls.
    fn guarded_counting(
        fs: &FaultFs,
        guard: &Guard<'_>,
        beat: &Heartbeat<'_>,
        calls: &std::cell::Cell<u32>,
    ) -> std::result::Result<Outcome, CopyError> {
        let root = fs.destination_root(Path::new("/")).unwrap();
        let cb = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        copy_file_guarded(
            fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &opts(),
            guard,
            beat,
            &cb,
            &no_before_publish,
        )
    }

    fn lock_lost() -> flux_fs::FsError {
        FsError::new(Code::TargetLockBusy, std::io::Error::other("lost"))
    }

    #[test]
    fn before_create_is_not_called_when_the_guard_or_heartbeat_fails() {
        // Section 99: the heartbeat and the guard come BEFORE the claim. The copy path calls each twice before
        // the claim (the leftover removal, then the exclusive create), so fail the first call and, separately,
        // the SECOND call, which is the one that sits directly in front of `before_create`.
        for nth in [1u32, 2] {
            let fs = FaultFs::new();
            fs.write_file("/src", b"hello");
            let n = std::cell::Cell::new(0u32);
            let guard = || {
                n.set(n.get() + 1);
                if n.get() == nth { Err(lock_lost()) } else { Ok(()) }
            };
            let ran = std::cell::Cell::new(0);
            let e = guarded_counting(&fs, &guard, &no_heartbeat, &ran).unwrap_err();
            assert_eq!(e.code(), Code::TargetLockBusy, "guard failing at call {nth}");
            assert_eq!(ran.get(), 0, "the claim ran after a failed guard (call {nth})");
            assert!(!fs.called("create_new"), "{:?}", fs.calls());

            let fs = FaultFs::new();
            fs.write_file("/src", b"hello");
            let n = std::cell::Cell::new(0u32);
            let beat = || {
                n.set(n.get() + 1);
                if n.get() == nth { Err(lock_lost()) } else { Ok(()) }
            };
            let ran = std::cell::Cell::new(0);
            let e = guarded_counting(&fs, &unguarded, &beat, &ran).unwrap_err();
            assert_eq!(e.step, CopyStep::Heartbeat, "heartbeat failing at call {nth}");
            assert_eq!(ran.get(), 0, "the claim ran after a failed heartbeat (call {nth})");
            assert!(!fs.called("create_new"), "{:?}", fs.calls());
        }
    }

    #[test]
    fn before_create_runs_exactly_once_per_copy() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let ran = std::cell::Cell::new(0);
        guarded_counting(&fs, &unguarded, &no_heartbeat, &ran).unwrap();
        assert_eq!(ran.get(), 1);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn skip_existing_leaves_the_destination_untouched() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.write_file("/dst", b"old");
        let mut o = opts();
        o.existing = flux_fs::ExistingPolicy::SkipExisting;
        let ran = std::cell::Cell::new(false);
        let cb = || {
            ran.set(true);
            Ok(())
        };
        let out = guarded_with(&fs, &o, &cb).unwrap();
        assert!(out.skipped);
        assert_eq!(out.bytes_copied, 0);
        assert_eq!(out.published_identity, FileIdentity::Unavailable);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"old"[..]));
        assert!(!fs.called("create_new") && !fs.called("rename_"), "{:?}", fs.calls());
        assert!(!ran.get(), "a skip claims nothing");
    }

    #[test]
    fn skip_existing_with_no_destination_copies() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let mut o = opts();
        o.existing = flux_fs::ExistingPolicy::SkipExisting;
        let out = guarded_with(&fs, &o, &no_before_create).unwrap();
        assert!(!out.skipped);
        assert_eq!(out.bytes_copied, 5);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn update_replaces_only_when_the_source_is_newer_or_the_size_differs() {
        let mut o = opts();
        o.existing = flux_fs::ExistingPolicy::Update;
        // (source time, source bytes, destination time, destination bytes, replaced)
        let cases: [(_, &[u8], _, &[u8], bool); 4] = [
            (at_secs(20), b"new", at_secs(10), b"old", true),
            (at_secs(10), b"new", at_secs(20), b"old", false),
            (at_secs(10), b"new!", at_secs(10), b"old", true),
            (None, b"new", at_secs(10), b"old", false),
        ];
        for (i, (st, sb, dt, db, replaced)) in cases.into_iter().enumerate() {
            let fs = FaultFs::new();
            put(&fs, "/src", sb, st);
            put(&fs, "/dst", db, dt);
            let out = guarded_with(&fs, &o, &no_before_create).unwrap();
            assert_eq!(out.skipped, !replaced, "case {i}");
            let want = if replaced { sb } else { db };
            assert_eq!(fs.read_file("/dst").as_deref(), Some(want), "case {i}");
            assert_eq!(fs.called("create_new(/dst.flux-partial"), replaced, "case {i}");
        }
    }

    #[test]
    fn skip_existing_over_an_existing_directory_is_refused_by_the_gate_not_skipped() {
        directory_at_the_destination_is_refused(flux_fs::ExistingPolicy::SkipExisting);
    }

    #[test]
    fn update_over_an_existing_directory_is_refused_by_the_gate_not_skipped() {
        directory_at_the_destination_is_refused(flux_fs::ExistingPolicy::Update);
    }

    fn directory_at_the_destination_is_refused(policy: flux_fs::ExistingPolicy) {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.create_dir(Path::new("/dst")).unwrap();
        let mut o = opts();
        o.existing = policy;
        let err = guarded_with(&fs, &o, &no_before_create).unwrap_err();
        assert_eq!(err.code(), flux_fs::Code::SafetyRejected);
        assert!(err.to_string().contains("directory"), "names the reason: {err}");
        assert!(!fs.called("create_new"), "{:?}", fs.calls());
    }

    #[test]
    fn update_replaces_when_an_mtime_is_unavailable_and_the_sizes_differ() {
        let mut o = opts();
        o.existing = flux_fs::ExistingPolicy::Update;
        // (source time, destination time): each side unavailable in turn, then both.
        for (i, (st, dt)) in
            [(None, at_secs(10)), (at_secs(10), None), (None, None)].into_iter().enumerate()
        {
            let fs = FaultFs::new();
            put(&fs, "/src", b"new!", st);
            put(&fs, "/dst", b"old", dt);
            let out = guarded_with(&fs, &o, &no_before_create).unwrap();
            assert!(!out.skipped, "case {i}");
            assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"new!"[..]), "case {i}");
        }
    }

    // ---- cut 9f-1 plan 2, task 7: `stage_file` resumes a partial from its proven prefix ----

    use crate::checkpoint::Discard;
    use std::time::SystemTime;

    /// Every range flushes as soon as the writer handles it: a crashed run's abort discards what is still pending, so the
    /// checkpoints a test expects to be durable must already be written when the crash comes.
    fn resume_policy(durability: Durability) -> CheckpointPolicy {
        CheckpointPolicy { range_chunks: 1, ..ckpt_policy(durability, 64, 1) }
    }

    fn run_gen(
        fs: &FaultFs,
        generation: u64,
        durability: Durability,
    ) -> Checkpoints<FakeDirHandle> {
        let a = fs.destination_root(Path::new("/ws")).unwrap();
        let b = fs.destination_root(Path::new("/ws")).unwrap();
        Checkpoints::open(a, b, "op", generation, resume_policy(durability))
            .unwrap_or_else(|e| panic!("open: {e:?}"))
    }

    fn pattern(len: usize, mul: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * mul) % 251) as u8).collect()
    }

    fn chunk_digests(bytes: &[u8]) -> Vec<[u8; 32]> {
        bytes.chunks(MIB).map(|c| *blake3::hash(c).as_bytes()).collect()
    }

    type Dir = <FaultFs as DestinationRoot>::Dir;
    type StageResult = std::result::Result<Stage<<FaultFs as FileSystem>::Writer>, CopyError>;

    fn stage_full(
        fs: &FaultFs,
        root: &Dir,
        run: &Checkpoints<FakeDirHandle>,
        durability: Durability,
        beat: &Heartbeat<'_>,
        before_create: &BeforeCreate<'_>,
    ) -> StageResult {
        let mut o = opts();
        o.durability = durability;
        let fc = FileCkpt { run, file_id: b"f".to_vec(), destination: b"dst".to_vec() };
        stage_file(
            fs,
            Path::new("/src"),
            root,
            OsStr::new("dst"),
            &o,
            &unguarded,
            beat,
            before_create,
            SyncAt::Staging,
            Some(Hook::Logged(&fc)),
        )
    }

    fn staged_of(r: StageResult) -> Staged<<FaultFs as FileSystem>::Writer> {
        match r {
            Ok(Stage::Staged(s)) => s,
            Ok(Stage::Skipped(_)) => panic!("unexpectedly skipped"),
            Err(e) => panic!("stage_file failed: {e:?}"),
        }
    }

    /// A crashed prior run (generation 1): the copy is stopped by a failing heartbeat once `chunks` MiB of the
    /// partial are written, so the WAL holds the digests of exactly those chunks (one `Range` per chunk) and the
    /// failed copy removes its partial. Tests then put whatever partial they want at the temporary's name.
    fn crash_after(fs: &FaultFs, root: &Dir, chunks: usize) {
        // R11: a kill may lose up to the in-flight pool's Ranges, which spec 3.6 allows and resume
        // re-hashes; this fixture pins resume given an exact log, so its crashing run hashes
        // inline.
        // The worker path is covered by:
        //   copy::tests::a_checkpointed_copy_logs_the_same_ranges_with_and_without_the_worker,
        //   checkpoint::sink::tests::the_worker_log_equals_the_inline_log, and
        //   flux-cli a_killed_large_file_resumes_from_its_validated_prefix.
        let _inline = crate::checkpoint::test_seams::OneCpu::new();
        let run = run_gen(fs, 1, Durability::Normal);
        let beat = || {
            if fs.read_file(PARTIAL).is_some_and(|b| b.len() >= chunks * MIB) {
                Err(FsError::new(Code::IoError, std::io::Error::other("the power went")))
            } else {
                Ok(())
            }
        };
        let e = stage_full(fs, root, &run, Durability::Normal, &beat, &no_before_create)
            .err()
            .expect("the crash stops the copy");
        assert_eq!(e.step, CopyStep::Heartbeat);
        assert!(!fs.exists(PARTIAL));
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    /// The `FILE_BEGIN` valid_through values and the `Range`s the WAL holds, in order.
    fn wal_begin_valid_through(fs: &FaultFs) -> Vec<u64> {
        wal_bodies(fs)
            .iter()
            .filter_map(|b| if let Body::FileBegin(f) = b { Some(f.valid_through) } else { None })
            .collect()
    }

    /// What a later run would replay for file `f`, read through a fresh `Checkpoints::open`.
    fn replayed(fs: &FaultFs, generation: u64) -> crate::wal::replay::FileState {
        let run = run_gen(fs, generation, Durability::Normal);
        let state = run.load(b"f").expect("loads").expect("the file is indexed");
        run.abandon();
        state
    }

    fn resume_fixture(len: usize, crash_chunks: usize) -> (FaultFs, Dir, Vec<u8>) {
        let (fs, root) = ckpt_fs(len);
        let src = fs.read_file("/src").unwrap();
        crash_after(&fs, &root, crash_chunks);
        (fs, root, src)
    }

    #[test]
    fn resume_continues_from_the_last_whole_chunk_and_the_output_equals_the_source() {
        let (fs, root) = ckpt_fs(5 * MIB);
        fs.set_modified("/src", SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 700));
        let src = fs.read_file("/src").unwrap();
        crash_after(&fs, &root, 3);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let (s0, p0) = (fs.bytes_read("/src"), fs.bytes_read(PARTIAL));
        let mark = fs.calls().len();
        let claims = AtomicUsize::new(0);
        let claim = || {
            claims.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let run = run_gen(&fs, 2, Durability::Normal);
        let staged =
            staged_of(stage_full(&fs, &root, &run, Durability::Normal, &no_heartbeat, &claim));
        assert_eq!(staged.reused, 3 * MIB as u64);
        assert_eq!(staged.bytes_copied, 2 * MIB as u64);
        assert_eq!(staged.discard, None);
        assert_eq!(claims.load(Ordering::SeqCst), 1, "the claim is made exactly once");
        // The validated prefix is read once by `decide`, the rest once by the loop: nothing twice, nothing skipped.
        assert_eq!(fs.bytes_read("/src") - s0, 5 * MIB as u64);
        assert_eq!(fs.bytes_read(PARTIAL) - p0, 3 * MIB as u64);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(fs.called(&format!("truncate_to({PARTIAL}, {})", 3 * MIB)), "{:?}", fs.calls());
        let calls = fs.calls();
        assert!(
            calls[mark..].iter().all(|c| c != &format!("remove_file({PARTIAL})")
                && c != &format!("create_new({PARTIAL})")),
            "a resume neither sweeps nor creates: {:?}",
            &calls[mark..]
        );
        assert!(run.finish(&no_heartbeat).is_clean());
        assert_eq!(wal_begin_valid_through(&fs), vec![0, 3 * MIB as u64]);
        let state = replayed(&fs, 3);
        assert_eq!(state.digests.values().copied().collect::<Vec<_>>(), chunk_digests(&src));
    }

    #[test]
    fn resume_truncates_garbage_past_the_last_checkpoint() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        let mut partial = src[..3 * MIB].to_vec();
        partial.extend(vec![0xEE; MIB + MIB / 2]);
        fs.write_file(PARTIAL, &partial);
        let run = run_gen(&fs, 2, Durability::Normal);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        assert_eq!((staged.reused, staged.bytes_copied), (3 * MIB as u64, 2 * MIB as u64));
        assert_eq!(staged.discard, None);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src, "the garbage is cut, never trusted");
        assert!(fs.called(&format!("truncate_to({PARTIAL}, {})", 3 * MIB)), "{:?}", fs.calls());
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    #[test]
    fn resume_recopies_from_the_first_bad_chunk() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        let mut partial = src[..3 * MIB].to_vec();
        partial[MIB + 10] ^= 0xFF;
        fs.write_file(PARTIAL, &partial);
        let (s0, p0) = (fs.bytes_read("/src"), fs.bytes_read(PARTIAL));
        let run = run_gen(&fs, 2, Durability::Normal);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        assert_eq!(staged.discard, Some(Discard::MismatchAt(MIB as u64)));
        assert_eq!(staged.reused, MIB as u64);
        // The carry (source chunk 1) is written by this run, so it counts.
        assert_eq!(staged.bytes_copied, 4 * MIB as u64);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert_eq!(fs.bytes_read("/src") - s0, 5 * MIB as u64, "no source byte twice");
        assert_eq!(fs.bytes_read(PARTIAL) - p0, 2 * MIB as u64);
        assert!(run.finish(&no_heartbeat).is_clean());
        let state = replayed(&fs, 3);
        assert_eq!(state.digests.values().copied().collect::<Vec<_>>(), chunk_digests(&src));
    }

    // Test audit (cut 9f-1 plan 2, round 1). Mutants: `>=` -> `>` at the `src_meta.len >= c.run.min_len()` filter
    // that gates `plan_resume` (the first), and at the one that gates the sink (the second): each leaves a source of
    // exactly `min_len` on the unchanged path (no validation of the partial, no WAL) while the others stay green.
    #[test]
    fn a_source_of_exactly_min_len_is_resumed_and_checkpointed() {
        let (fs, root, src) = resume_fixture(MIB, 1);
        assert_eq!(src.len(), 1 << 20, "the fixture's source is exactly the policy's min_len");
        fs.write_file(PARTIAL, &src);
        let run = run_gen(&fs, 2, Durability::Normal);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        assert_eq!(staged.reused, MIB as u64, "the whole partial is validated and kept");
        assert_eq!(staged.discard, None);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());
        let state = replayed(&fs, 2);
        assert_eq!(state.digests.values().copied().collect::<Vec<_>>(), chunk_digests(&src));
    }

    #[test]
    fn a_changed_source_discards_the_checkpoints_and_recopies() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let new_src = pattern(5 * MIB + 17, 3);
        fs.write_file("/src", &new_src);
        let mark = fs.calls().len();
        let run = run_gen(&fs, 2, Durability::Normal);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        assert!(matches!(staged.discard, Some(Discard::SourceChanged(_))), "{:?}", staged.discard);
        assert_eq!((staged.reused, staged.bytes_copied), (0, new_src.len() as u64));
        let calls = fs.calls();
        let after = &calls[mark..];
        let at =
            |c: &str| after.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c}: {after:?}"));
        assert!(
            at(&format!("remove_file({PARTIAL})")) < at(&format!("create_new({PARTIAL})")),
            "the sweep ran: {after:?}"
        );
        assert!(!fs.called(&format!("open_existing({PARTIAL})")));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), new_src);
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    #[test]
    fn a_missing_partial_recopies() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        assert!(!fs.exists(PARTIAL));
        let run = run_gen(&fs, 2, Durability::Normal);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        assert_eq!(staged.discard, Some(Discard::PartialMissing));
        assert_eq!((staged.reused, staged.bytes_copied), (0, 5 * MIB as u64));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    #[test]
    fn a_fully_validated_partial_copies_nothing_and_still_publishes() {
        // A short last chunk: the resume offset is the source length, which is not a multiple of the chunk size.
        let len = 3 * MIB + 12345;
        let (fs, root) = ckpt_fs(len);
        let src = fs.read_file("/src").unwrap();
        let run = run_gen(&fs, 1, Durability::Strict);
        staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Strict,
            &no_heartbeat,
            &no_before_create,
        ));
        assert!(run.finish(&no_heartbeat).is_clean());
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        let (s0, p0) = (fs.bytes_read("/src"), fs.bytes_read(PARTIAL));
        let mark = fs.calls().len();

        let run = run_gen(&fs, 2, Durability::Strict);
        let fc = FileCkpt { run: &run, file_id: b"f".to_vec(), destination: b"dst".to_vec() };
        let mut o = opts();
        o.durability = Durability::Strict;
        let (out, reuse) = copy_file_checkpointed(
            &fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &o,
            &unguarded,
            &no_heartbeat,
            &no_before_create,
            &no_before_publish,
            Some(Hook::Logged(&fc)),
        )
        .unwrap_or_else(|e| panic!("copy failed: {e:?}"));
        assert_eq!(out.bytes_copied, 0);
        assert_eq!((reuse.reused, reuse.discard), (len as u64, None));
        assert_eq!(fs.read_file("/dst").unwrap(), src, "published");
        assert_eq!(fs.bytes_read("/src") - s0, len as u64, "only `decide` reads the source");
        assert_eq!(fs.bytes_read(PARTIAL) - p0, len as u64);
        let calls = fs.calls();
        let after = &calls[mark..];
        assert!(after.contains(&format!("truncate_to({PARTIAL}, {len})")), "{after:?}");
        assert!(
            after.iter().any(|c| c.starts_with("set_times(")),
            "metadata still runs: {after:?}"
        );
        assert!(
            after.contains(&format!("sync_all({PARTIAL})")),
            "the Strict sync still runs: {after:?}"
        );
        assert!(run.finish(&no_heartbeat).is_clean());
        assert_eq!(wal_begin_valid_through(&fs), vec![0, len as u64]);
        let bodies = wal_bodies(&fs);
        assert!(
            matches!(bodies.last(), Some(Body::FileBegin(_))),
            "no Range follows the resumed FILE_BEGIN: no byte arrived"
        );
    }

    // ---- cut 9f-2 task 4: `--resume-verify=full` in `plan_resume`, the proven prefix re-recorded ----

    fn run_gen_as(
        fs: &FaultFs,
        generation: u64,
        durability: Durability,
        verify: crate::checkpoint::ResumeVerify,
    ) -> Checkpoints<FakeDirHandle> {
        let a = fs.destination_root(Path::new("/ws")).unwrap();
        let b = fs.destination_root(Path::new("/ws")).unwrap();
        let policy = CheckpointPolicy { verify, ..resume_policy(durability) };
        Checkpoints::open(a, b, "op", generation, policy).unwrap_or_else(|e| panic!("open: {e:?}"))
    }

    fn stage_normal(
        fs: &FaultFs,
        root: &Dir,
        run: &Checkpoints<FakeDirHandle>,
    ) -> Staged<<FaultFs as FileSystem>::Writer> {
        staged_of(stage_full(fs, root, run, Durability::Normal, &no_heartbeat, &no_before_create))
    }

    #[test]
    fn full_salvages_the_unrecorded_tail_that_chunks_truncates() {
        use crate::checkpoint::ResumeVerify::{Chunks, Full};
        // Fixture A, the control: `chunks` trusts only the three recorded chunks.
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file(PARTIAL, &src[..4 * MIB]);
        let run = run_gen_as(&fs, 2, Durability::Normal, Chunks);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.reused, 3 * MIB as u64);
        assert_eq!(staged.bytes_copied, 2 * MIB as u64);
        assert_eq!(staged.discard, None);
        assert!(run.finish(&no_heartbeat).is_clean());

        // Fixture B, separate: `full` proves the fourth chunk against the source too.
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file(PARTIAL, &src[..4 * MIB]);
        let run = run_gen_as(&fs, 2, Durability::Normal, Full);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.reused, 4 * MIB as u64);
        assert_eq!(staged.bytes_copied, MIB as u64);
        // Design 3.2 item 4: a partial that ended before the source is `PartialShorter` (the brief said `None`).
        assert_eq!(staged.discard, Some(Discard::PartialShorter { kept: 4 * MIB as u64 }));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(fs.called(&format!("truncate_to({PARTIAL}, {})", 4 * MIB)), "{:?}", fs.calls());
        assert!(run.finish(&no_heartbeat).is_clean());
        let state = replayed(&fs, 3);
        assert_eq!(state.digests.values().copied().collect::<Vec<_>>(), chunk_digests(&src));
    }

    #[test]
    fn full_salvages_a_partial_with_no_log_at_all() {
        use crate::checkpoint::ResumeVerify::{Chunks, Full};
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let mark = fs.calls().len();
        let run = run_gen_as(&fs, 1, Durability::Normal, Full);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.reused, 3 * MIB as u64);
        // Design 3.2 item 4: a partial that ended before the source is `PartialShorter` (the brief said `None`).
        assert_eq!(staged.discard, Some(Discard::PartialShorter { kept: 3 * MIB as u64 }));
        let calls = fs.calls();
        assert!(
            calls[mark..].iter().all(|c| c != &format!("remove_file({PARTIAL})")
                && c != &format!("create_new({PARTIAL})")),
            "a full resume neither sweeps nor creates: {:?}",
            &calls[mark..]
        );
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());
        assert_eq!(wal_begin_valid_through(&fs), vec![3 * MIB as u64]);
        let state = replayed(&fs, 2);
        assert_eq!(state.digests.values().copied().collect::<Vec<_>>(), chunk_digests(&src));

        // The control, on a SEPARATE filesystem: `chunks` finds no log and recopies.
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let run = run_gen_as(&fs, 1, Durability::Normal, Chunks);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.reused, 0);
        assert!(fs.called(&format!("create_new({PARTIAL})")), "{:?}", fs.calls());
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    #[test]
    fn full_records_the_proven_prefix_even_when_the_old_log_already_covers_it() {
        use crate::checkpoint::ResumeVerify::Full;
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let run = run_gen_as(&fs, 2, Durability::Normal, Full);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.reused, 3 * MIB as u64);
        assert!(run.finish(&no_heartbeat).is_clean());
        let bodies = wal_bodies(&fs);
        let begin = bodies
            .iter()
            .position(|b| matches!(b, Body::FileBegin(f) if f.valid_through == 3 * MIB as u64))
            .expect("the generation-2 FileBegin");
        let starts: Vec<u64> = bodies[begin + 1..]
            .iter()
            .filter_map(|b| if let Body::Range(r) = b { Some(r.range_start) } else { None })
            .collect();
        let re_record =
            starts.iter().position(|s| *s == 0).expect("the proven prefix is re-recorded");
        let tail = starts.iter().position(|s| *s == 3 * MIB as u64).expect("the copied tail");
        assert!(re_record < tail, "{starts:?}");
    }

    #[test]
    fn full_ignores_a_log_that_matches_the_partial_but_not_the_source() {
        use crate::checkpoint::ResumeVerify::Full;
        let t = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 700);
        // The source changed under a log that still matches the partial; a `chunks` gate would pass (same time).
        let (fs, root) = ckpt_fs(5 * MIB);
        fs.set_modified("/src", t);
        let src = fs.read_file("/src").unwrap();
        crash_after(&fs, &root, 3);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let mut new_src = src.clone();
        new_src[MIB + 5] ^= 0xFF;
        fs.write_file("/src", &new_src);
        fs.set_modified("/src", t);
        let run = run_gen_as(&fs, 2, Durability::Normal, Full);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.discard, Some(Discard::MismatchAt(MIB as u64)));
        assert_eq!(staged.reused, MIB as u64);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), new_src);
        assert!(run.finish(&no_heartbeat).is_clean());

        // The reverse: the partial's chunk 1 is flipped, the log matches the source and not the partial.
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        let mut partial = src[..3 * MIB].to_vec();
        partial[MIB + 5] ^= 0xFF;
        fs.write_file(PARTIAL, &partial);
        let run = run_gen_as(&fs, 2, Durability::Normal, Full);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.discard, Some(Discard::MismatchAt(MIB as u64)));
        assert_eq!(staged.reused, MIB as u64);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    #[test]
    fn a_chunks_resume_after_a_full_resume_that_crashed_reuses_the_full_prefix() {
        // R11: a kill may lose up to the in-flight pool's Ranges, which spec 3.6 allows and resume
        // re-hashes; this fixture pins resume given an exact log, so its crashing run hashes
        // inline.
        // The worker path is covered by:
        //   copy::tests::a_checkpointed_copy_logs_the_same_ranges_with_and_without_the_worker,
        //   checkpoint::sink::tests::the_worker_log_equals_the_inline_log, and
        //   flux-cli a_killed_large_file_resumes_from_its_validated_prefix.
        let inline = crate::checkpoint::test_seams::OneCpu::new();
        use crate::checkpoint::ResumeVerify::{Chunks, Full};
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let run = run_gen_as(&fs, 1, Durability::Normal, Full);
        let beat = || {
            if fs.read_file(PARTIAL).is_some_and(|b| b.len() >= 4 * MIB) {
                Err(FsError::new(Code::IoError, std::io::Error::other("the power went")))
            } else {
                Ok(())
            }
        };
        let e = stage_full(&fs, &root, &run, Durability::Normal, &beat, &no_before_create)
            .err()
            .expect("the crash stops the copy");
        assert_eq!(e.step, CopyStep::Heartbeat);
        assert!(run.finish(&no_heartbeat).is_clean());
        // Only the crashing run hashes inline; the resuming run below uses the worker.
        drop(inline);
        fs.write_file(PARTIAL, &src[..4 * MIB]);
        let run = run_gen_as(&fs, 2, Durability::Normal, Chunks);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.reused, 4 * MIB as u64);
        assert_eq!(staged.discard, None);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    #[test]
    fn full_with_no_partial_reports_partial_missing_only_when_the_log_began_the_file() {
        use crate::checkpoint::ResumeVerify::Full;
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        assert!(!fs.exists(PARTIAL));
        let run = run_gen_as(&fs, 2, Durability::Normal, Full);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.discard, Some(Discard::PartialMissing));
        assert_eq!(staged.reused, 0);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());

        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        let run = run_gen_as(&fs, 1, Durability::Normal, Full);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.discard, None);
        assert_eq!(staged.reused, 0);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    #[test]
    fn full_with_no_partial_and_an_unreadable_log_reports_nothing() {
        use crate::checkpoint::ResumeVerify::Full;
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        assert!(!fs.exists(PARTIAL));
        let run = run_gen_as(&fs, 2, Durability::Normal, Full);
        // The segment is cut after the run scanned it: `load` cannot read the indexed records.
        let wal = fs.read_file(WAL_PATH).unwrap();
        fs.write_file(WAL_PATH, &wal[..wal.len() / 2]);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.discard, None, "a broken log is not a per-file report under `full`");
        assert_eq!((staged.reused, staged.bytes_copied), (0, 5 * MIB as u64));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        run.abandon();
    }

    #[test]
    fn full_with_a_partial_longer_than_the_source_proves_the_whole_source_under_strict() {
        use crate::checkpoint::ResumeVerify::Full;
        let len = 3 * MIB + 12345;
        let (fs, root) = ckpt_fs(len);
        let src = fs.read_file("/src").unwrap();
        let mut partial = src.clone();
        partial.extend(vec![0xEE; 2 * MIB]);
        fs.write_file(PARTIAL, &partial);
        let run = run_gen_as(&fs, 1, Durability::Strict, Full);
        let fc = FileCkpt { run: &run, file_id: b"f".to_vec(), destination: b"dst".to_vec() };
        let mut o = opts();
        o.durability = Durability::Strict;
        let (out, reuse) = copy_file_checkpointed(
            &fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &o,
            &unguarded,
            &no_heartbeat,
            &no_before_create,
            &no_before_publish,
            Some(Hook::Logged(&fc)),
        )
        .unwrap_or_else(|e| panic!("copy failed: {e:?}"));
        assert_eq!(out.bytes_copied, 0);
        assert_eq!((reuse.reused, reuse.discard), (len as u64, None));
        assert!(fs.called(&format!("truncate_to({PARTIAL}, {len})")), "{:?}", fs.calls());
        assert_eq!(fs.read_file("/dst").unwrap(), src, "published");
        assert!(fs.called(&format!("sync_all({PARTIAL})")), "{:?}", fs.calls());
        assert!(run.finish(&no_heartbeat).is_clean());
        assert_eq!(wal_begin_valid_through(&fs), vec![len as u64]);
    }

    #[test]
    fn full_a_differing_unrecorded_tail_is_reported_as_a_mismatch() {
        use crate::checkpoint::ResumeVerify::Full;
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        let mut partial = src[..3 * MIB].to_vec();
        partial.extend(vec![0xEE; MIB + MIB / 2]);
        fs.write_file(PARTIAL, &partial);
        let run = run_gen_as(&fs, 2, Durability::Normal, Full);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.discard, Some(Discard::MismatchAt(3 * MIB as u64)));
        assert_eq!(staged.reused, 3 * MIB as u64);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    // Test audit (cut 9f-2). Design 7.8 under `full`. Mutants: `>=` -> `>` at either `min_len` filter; `<` -> `<=` at
    // `decide_full`'s `partial_len < start + len` (an exactly-one-chunk partial is then called short and kept as 0).
    #[test]
    fn full_resumes_a_source_of_exactly_min_len() {
        use crate::checkpoint::ResumeVerify::Full;
        let (fs, root, src) = resume_fixture(MIB, 1);
        assert_eq!(src.len(), 1 << 20, "the fixture's source is exactly the policy's min_len");
        fs.write_file(PARTIAL, &src);
        let run = run_gen_as(&fs, 2, Durability::Normal, Full);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.reused, MIB as u64, "the whole partial is proven and kept");
        assert_eq!(staged.bytes_copied, 0);
        assert_eq!(staged.discard, None);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());
        let state = replayed(&fs, 2);
        assert_eq!(state.digests.values().copied().collect::<Vec<_>>(), chunk_digests(&src));
    }

    // Test audit (cut 9f-2). A partial of exactly one chunk, through `plan_resume`, not only `decide_full`'s unit
    // test. Mutants: `<` -> `<=` at `decide_full`'s `partial_len < start + len`; `plan_resume` handing `decide_full`
    // `len - 1`.
    #[test]
    fn full_keeps_a_partial_of_exactly_one_chunk() {
        use crate::checkpoint::ResumeVerify::Full;
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file(PARTIAL, &src[..MIB]);
        let run = run_gen_as(&fs, 1, Durability::Normal, Full);
        let staged = stage_normal(&fs, &root, &run);
        assert_eq!(staged.reused, MIB as u64);
        assert_eq!(staged.bytes_copied, 4 * MIB as u64);
        assert_eq!(staged.discard, Some(Discard::PartialShorter { kept: MIB as u64 }));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());
        let state = replayed(&fs, 2);
        assert_eq!(state.digests.values().copied().collect::<Vec<_>>(), chunk_digests(&src));
    }

    // ---- cut 9g: the log-free hook (a single file): `plan_resume` runs as `full` with no log and no sink ----

    fn stage_log_free(
        fs: &FaultFs,
        root: &Dir,
        durability: Durability,
        beat: &Heartbeat<'_>,
    ) -> StageResult {
        let mut o = opts();
        o.durability = durability;
        stage_log_free_with(fs, root, &o, beat)
    }

    fn stage_log_free_with(
        fs: &FaultFs,
        root: &Dir,
        o: &CopyOptions,
        beat: &Heartbeat<'_>,
    ) -> StageResult {
        stage_file(
            fs,
            Path::new("/src"),
            root,
            OsStr::new("dst"),
            o,
            &unguarded,
            beat,
            &no_before_create,
            SyncAt::Staging,
            Some(Hook::LogFree { min_len: MIB as u64 }),
        )
    }

    // Mutant: `stage_file`'s skip branch does not sweep the partial (a hook defers the early sweep).
    #[test]
    fn log_free_skip_existing_removes_the_stale_partial() {
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file("/dst", b"existing");
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let mut o = opts();
        o.existing = flux_fs::ExistingPolicy::SkipExisting;
        let r = stage_log_free_with(&fs, &root, &o, &no_heartbeat);
        assert!(matches!(r, Ok(Stage::Skipped(_))), "{:?}", r.err());
        assert!(fs.read_file(PARTIAL).is_none(), "the stale partial was removed");
        assert_eq!(fs.read_file("/dst").unwrap(), b"existing");
    }

    // Mutant: as above, for `Update` when the source is neither newer nor of a different size.
    #[test]
    fn log_free_update_that_skips_removes_the_stale_partial() {
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file("/dst", &vec![1u8; 5 * MIB]);
        let at = |n| std::time::UNIX_EPOCH + Duration::from_secs(n);
        fs.set_modified("/src", at(100));
        fs.set_modified("/dst", at(200));
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let mut o = opts();
        o.existing = flux_fs::ExistingPolicy::Update;
        let r = stage_log_free_with(&fs, &root, &o, &no_heartbeat);
        assert!(matches!(r, Ok(Stage::Skipped(_))), "{:?}", r.err());
        assert!(fs.read_file(PARTIAL).is_none(), "the stale partial was removed");
    }

    // Mutant: the skip branch's sweep drops its own section 99 `guard()` (a removal is a destination mutation, so a
    // process that lost the lock must not make it). Guard call 1 is `stage_file`'s opening check, call 2 the sweep's.
    #[test]
    fn log_free_skip_sweep_checks_the_lock_before_removing_the_partial() {
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file("/dst", b"existing");
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let mark = fs.calls().len();
        let mut o = opts();
        o.existing = flux_fs::ExistingPolicy::SkipExisting;
        let calls = std::cell::Cell::new(0u32);
        let guard = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 2 {
                Err(FsError::new(Code::IoError, std::io::Error::other("the lock is lost")))
            } else {
                Ok(())
            }
        };
        let e = stage_file(
            &fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &o,
            &guard,
            &no_heartbeat,
            &no_before_create,
            SyncAt::Staging,
            Some(Hook::LogFree { min_len: MIB as u64 }),
        )
        .err()
        .expect("the lost lock stops the sweep");
        assert_eq!(e.step, CopyStep::Create);
        assert_eq!(calls.get(), 2);
        assert_eq!(fs.read_file(PARTIAL).expect("the partial is still there"), src[..3 * MIB]);
        assert_eq!(fs.read_file("/dst").unwrap(), b"existing");
        let all = fs.calls();
        assert!(
            all[mark..].iter().all(|c| !c.starts_with("remove_file(")),
            "nothing was removed: {:?}",
            &all[mark..]
        );
    }

    // Mutant: `plan_resume` calls `load` for `LogFree` (returns early with no log, so `reused == 0`).
    #[test]
    fn log_free_resumes_the_equal_prefix_of_a_partial_with_no_log() {
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let mark = fs.calls().len();
        let staged = staged_of(stage_log_free(&fs, &root, Durability::Normal, &no_heartbeat));
        assert_eq!(staged.reused, 3 * MIB as u64);
        assert_eq!(staged.bytes_copied, 2 * MIB as u64);
        assert_eq!(staged.discard, Some(Discard::PartialShorter { kept: 3 * MIB as u64 }));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        let calls = fs.calls();
        assert!(
            calls[mark..].iter().all(|c| !c.contains("wal") && !c.starts_with("create_new(")),
            "no log is touched and nothing is created: {:?}",
            &calls[mark..]
        );
    }

    // Mutant: `PartialMissing` reported.
    #[test]
    fn log_free_with_no_partial_is_a_fresh_copy_that_reports_nothing() {
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        let staged = staged_of(stage_log_free(&fs, &root, Durability::Normal, &no_heartbeat));
        assert_eq!(staged.reused, 0);
        assert_eq!(staged.discard, None);
        assert!(fs.called(&format!("create_new({PARTIAL})")), "{:?}", fs.calls());
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
    }

    // Mutant: the partial is trusted past a mismatch.
    #[test]
    fn log_free_reports_a_differing_unrecorded_tail() {
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        let mut partial = src[..3 * MIB].to_vec();
        partial.extend(vec![0xEE; MIB + MIB / 2]);
        fs.write_file(PARTIAL, &partial);
        let staged = staged_of(stage_log_free(&fs, &root, Durability::Normal, &no_heartbeat));
        assert_eq!(staged.discard, Some(Discard::MismatchAt(3 * MIB as u64)));
        assert_eq!(staged.reused, 3 * MIB as u64);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
    }

    #[test]
    fn log_free_with_a_partial_longer_than_the_source_proves_the_whole_source() {
        let len = 3 * MIB + 12345;
        let (fs, root) = ckpt_fs(len);
        let src = fs.read_file("/src").unwrap();
        let mut partial = src.clone();
        partial.extend(vec![0xEE; 2 * MIB]);
        fs.write_file(PARTIAL, &partial);
        let staged = staged_of(stage_log_free(&fs, &root, Durability::Normal, &no_heartbeat));
        assert_eq!(staged.reused, len as u64);
        assert_eq!(staged.bytes_copied, 0);
        assert_eq!(staged.discard, None);
        assert!(fs.called(&format!("truncate_to({PARTIAL}, {len})")), "{:?}", fs.calls());
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
    }

    #[test]
    fn log_free_with_an_empty_partial_recopies_and_reports_it_shorter() {
        let (fs, root) = ckpt_fs(5 * MIB);
        fs.write_file(PARTIAL, &[]);
        let staged = staged_of(stage_log_free(&fs, &root, Durability::Normal, &no_heartbeat));
        assert_eq!(staged.reused, 0);
        assert_eq!(staged.discard, Some(Discard::PartialShorter { kept: 0 }));
    }

    // Mutant: the min_len filter at the `plan_resume` call removed (the partial is read).
    #[test]
    fn log_free_below_min_len_recopies_without_reading_the_partial() {
        let (fs, root) = ckpt_fs(MIB / 2);
        fs.write_file(PARTIAL, &[1, 2, 3, 4]);
        let staged = staged_of(stage_log_free(&fs, &root, Durability::Normal, &no_heartbeat));
        assert_eq!(staged.reused, 0);
        assert_eq!(staged.discard, None);
        assert_eq!(fs.bytes_read(PARTIAL), 0);
    }

    #[test]
    fn log_free_heartbeat_failure_leaves_the_partial_untouched() {
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let mark = fs.calls().len();
        // Beats 1 and 2 are `stage_file`'s own; 3 is `decide_full`'s for chunk 0; the 4th, before chunk 1, fails: one
        // chunk was hashed and nothing was mutated.
        let calls = std::cell::Cell::new(0u32);
        let beat = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 4 {
                Err(FsError::new(Code::IoError, std::io::Error::other("the lock is lost")))
            } else {
                Ok(())
            }
        };
        let e = stage_log_free(&fs, &root, Durability::Normal, &beat)
            .err()
            .expect("the lost lock stops the copy");
        assert_eq!(e.step, CopyStep::Heartbeat);
        let partial = fs.read_file(PARTIAL).expect("the partial is still there");
        assert_eq!(partial.len(), 3 * MIB);
        assert_eq!(partial, src[..3 * MIB]);
        let all = fs.calls();
        assert!(
            all[mark..].iter().all(|c| !c.starts_with("truncate_to(")),
            "the partial is untouched: {:?}",
            &all[mark..]
        );
    }

    // Mutant: the Strict sync is dropped, or runs before the appended bytes are written, or runs under `Normal` too.
    #[test]
    fn log_free_under_strict_syncs_the_partial_after_the_last_append() {
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let mark = fs.calls().len();
        let staged = staged_of(stage_log_free(&fs, &root, Durability::Strict, &no_heartbeat));
        assert_eq!(staged.reused, 3 * MIB as u64);
        assert_eq!(staged.bytes_copied, 2 * MIB as u64);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        let calls = fs.calls();
        let new = &calls[mark..];
        let sync = format!("sync_all({PARTIAL})");
        let at = new.iter().position(|c| c == &sync).unwrap_or_else(|| panic!("no sync: {new:?}"));
        assert_eq!(new.iter().filter(|c| *c == &sync).count(), 1, "{new:?}");
        assert!(
            new[..at].iter().any(|c| c.starts_with("truncate_to(")),
            "the sync follows the truncation to the proven prefix: {new:?}"
        );
        assert!(new.iter().all(|c| !c.contains("wal")), "no log is touched: {new:?}");
    }

    // The distractor of the test above: `Normal` does not sync the partial in `stage_file`.
    #[test]
    fn log_free_under_normal_does_not_sync_the_partial() {
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let mark = fs.calls().len();
        staged_of(stage_log_free(&fs, &root, Durability::Normal, &no_heartbeat));
        let calls = fs.calls();
        assert!(
            !calls[mark..].iter().any(|c| c == &format!("sync_all({PARTIAL})")),
            "{:?}",
            &calls[mark..]
        );
    }

    // Mutant: the stage-level gate `len >= min_len` becomes `len > min_len` (a source of exactly `min_len` resumes).
    #[test]
    fn log_free_at_exactly_min_len_reads_and_keeps_the_partial() {
        let (fs, root) = ckpt_fs(MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file(PARTIAL, &src);
        let staged = staged_of(stage_log_free(&fs, &root, Durability::Normal, &no_heartbeat));
        assert_eq!(staged.reused, MIB as u64);
        assert_eq!(staged.bytes_copied, 0);
        assert!(fs.bytes_read(PARTIAL) > 0, "the partial was read to prove it");
    }

    // The other side of the boundary: one byte under `min_len` never reads the partial.
    #[test]
    fn log_free_one_byte_under_min_len_does_not_read_the_partial() {
        let (fs, root) = ckpt_fs(MIB - 1);
        let src = fs.read_file("/src").unwrap();
        fs.write_file(PARTIAL, &src);
        let staged = staged_of(stage_log_free(&fs, &root, Durability::Normal, &no_heartbeat));
        assert_eq!(staged.reused, 0);
        assert_eq!(fs.bytes_read(PARTIAL), 0);
    }

    #[test]
    fn a_heartbeat_failure_during_full_validation_appends_nothing() {
        use crate::checkpoint::ResumeVerify::Full;
        let (fs, root) = ckpt_fs(5 * MIB);
        let src = fs.read_file("/src").unwrap();
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let run = run_gen_as(&fs, 1, Durability::Normal, Full);
        // Beats 1 and 2 are `stage_file`'s own; 3 is `decide_full`'s for chunk 0; the 4th, before chunk 1, fails.
        let calls = std::cell::Cell::new(0u32);
        let beat = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 4 {
                Err(FsError::new(Code::IoError, std::io::Error::other("the lock is lost")))
            } else {
                Ok(())
            }
        };
        let e = stage_full(&fs, &root, &run, Durability::Normal, &beat, &no_before_create)
            .err()
            .expect("the lost lock stops the copy");
        assert_eq!(e.step, CopyStep::Heartbeat);
        assert!(!fs.called("truncate_to("), "the partial is untouched: {:?}", fs.calls());
        // `finish` flushes whatever was pending (the segment is created lazily on the first flush), so an absent
        // segment or an empty one is the pass case and a pending `FileBegin` or `Range` would show up here.
        assert!(run.finish(&no_heartbeat).is_clean());
        let bodies = if fs.exists(WAL_PATH) { wal_bodies(&fs) } else { Vec::new() };
        assert!(
            !bodies.iter().any(|b| matches!(b, Body::FileBegin(_) | Body::Range(_))),
            "no FileBegin and no Range: {bodies:?}"
        );
    }

    #[test]
    fn a_source_that_changes_during_a_resumed_copy_is_discarded() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let new_src = pattern(2 * MIB + MIB / 2, 5);
        let swap = || {
            fs.write_file("/src", &new_src);
            Ok(())
        };
        let run = run_gen(&fs, 2, Durability::Normal);
        let e = stage_full(&fs, &root, &run, Durability::Normal, &no_heartbeat, &swap)
            .err()
            .expect("the recheck fails");
        assert_eq!((e.step, e.cause.code), (CopyStep::Recheck, Code::SourceChanged));
        assert!(!fs.exists(PARTIAL), "the temporary is removed");
        assert!(run.finish(&no_heartbeat).is_clean());
        // The log now holds the resumed run's digests of the OLD source (chunks 0..4).
        assert_eq!(replayed(&fs, 3).digests.len(), 5);

        let run = run_gen(&fs, 4, Durability::Normal);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        assert!(matches!(staged.discard, Some(Discard::SourceChanged(_))), "{:?}", staged.discard);
        assert!(run.finish(&no_heartbeat).is_clean());
        let state = replayed(&fs, 5);
        assert_eq!(state.begin.source_len, new_src.len() as u64);
        assert_eq!(
            state.digests.values().copied().collect::<Vec<_>>(),
            chunk_digests(&new_src),
            "the new FILE_BEGIN reset the set: no stale digest of chunks 3 and 4 survives"
        );
    }

    #[test]
    fn a_source_that_shrinks_before_it_is_read_fails_the_recheck_and_discards() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        // After the metadata that fixed the length and before the reader is opened: the shrunk file is what is read.
        let shrunk = src[..MIB + MIB / 2].to_vec();
        let run = run_gen(&fs, 2, Durability::Normal);
        // Opening the run scanned the log through `open_read` too, so count from here: the next one is the source's.
        let next = count(&fs, "open_read(") as u32 + 1;
        fs.on_nth("open_read", next, move |fs| fs.write_file("/src", &shrunk));
        let e = stage_full(&fs, &root, &run, Durability::Normal, &no_heartbeat, &no_before_create)
            .err()
            .expect("a shrunk source cannot publish");
        assert_eq!((e.step, e.cause.code), (CopyStep::Recheck, Code::SourceChanged));
        assert!(
            fs.called(&format!("truncate_to({PARTIAL}, {MIB})")),
            "resumed at the last good chunk, no special case: {:?}",
            fs.calls()
        );
        assert!(!fs.exists(PARTIAL));
        run.abandon();
    }

    #[test]
    fn a_planted_symlink_at_the_partial_name_is_refused() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file("/target", b"orig");
        fs.add_symlink(PARTIAL);
        let (s0, p0) = (fs.bytes_read("/src"), fs.bytes_read(PARTIAL));
        let run = run_gen(&fs, 2, Durability::Normal);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        // The reason names the type check, not the fake's refusal to open a link: the partial is never opened.
        let Some(Discard::Unreadable(why)) = &staged.discard else {
            panic!("{:?}", staged.discard)
        };
        assert!(why.contains("not a regular file"), "{why}");
        assert_eq!((staged.reused, staged.bytes_copied), (0, 5 * MIB as u64));
        assert!(!fs.called(&format!("open_existing({PARTIAL})")), "{:?}", fs.calls());
        assert!(fs.called(&format!("remove_file({PARTIAL})")), "the sweep removes the link");
        assert_eq!(fs.read_file("/target").as_deref(), Some(&b"orig"[..]));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert_eq!(fs.bytes_read(PARTIAL) - p0, 0, "a link is never read");
        assert_eq!(fs.bytes_read("/src") - s0, 5 * MIB as u64);
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    #[test]
    fn a_chunk_zero_mismatch_still_publishes_every_byte() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        let mut partial = src[..3 * MIB].to_vec();
        partial[7] ^= 0xFF;
        fs.write_file(PARTIAL, &partial);
        let s0 = fs.bytes_read("/src");
        let run = run_gen(&fs, 2, Durability::Normal);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        assert_eq!(staged.discard, Some(Discard::MismatchAt(0)));
        assert_eq!((staged.reused, staged.bytes_copied), (0, 5 * MIB as u64));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert_eq!(fs.bytes_read("/src") - s0, 5 * MIB as u64, "no byte read twice or skipped");
        assert!(run.finish(&no_heartbeat).is_clean());
        let state = replayed(&fs, 3);
        assert_eq!(
            state.digests[&0],
            *blake3::hash(&src[..MIB]).as_bytes(),
            "the carry was fed to the sink"
        );
        assert_eq!(state.digests.values().copied().collect::<Vec<_>>(), chunk_digests(&src));
    }

    #[test]
    fn deliberately_wrong_digests_recopy() {
        let (fs, root) = ckpt_fs(5 * MIB);
        let other = pattern(5 * MIB, 7);
        fs.write_file("/src", &other);
        crash_after(&fs, &root, 3);
        let src = pattern(5 * MIB, 1);
        fs.write_file("/src", &src);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let run = run_gen(&fs, 2, Durability::Normal);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        assert_eq!(staged.discard, Some(Discard::MismatchAt(0)));
        assert_eq!((staged.reused, staged.bytes_copied), (0, 5 * MIB as u64));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    #[test]
    fn an_unreadable_wal_is_a_fresh_copy_with_the_reason() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let run = run_gen(&fs, 2, Durability::Normal);
        // The segment is cut after the run scanned it: pass two cannot read the indexed records.
        let wal = fs.read_file(WAL_PATH).unwrap();
        fs.write_file(WAL_PATH, &wal[..wal.len() / 2]);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        assert!(matches!(staged.discard, Some(Discard::Unreadable(_))), "{:?}", staged.discard);
        assert_eq!((staged.reused, staged.bytes_copied), (0, 5 * MIB as u64));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        run.abandon();
    }

    #[test]
    fn a_partial_with_an_unavailable_identity_is_recopied_not_wedged() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        fs.set_identity(PARTIAL, FileIdentity::Unavailable);
        let p0 = fs.bytes_read(PARTIAL);
        let run = run_gen(&fs, 2, Durability::Normal);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        let Some(Discard::Unreadable(why)) = &staged.discard else {
            panic!("{:?}", staged.discard)
        };
        assert!(why.contains("identity"), "{why}");
        assert_eq!((staged.reused, staged.bytes_copied), (0, 5 * MIB as u64));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert_eq!(fs.bytes_read(PARTIAL) - p0, 0, "the partial was never read");
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    #[test]
    fn a_hard_linked_partial_is_recopied_never_written_through() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        fs.set_link_count(PARTIAL, 2);
        let p0 = fs.bytes_read(PARTIAL);
        let mark = fs.calls().len();
        let run = run_gen(&fs, 2, Durability::Normal);
        let staged = staged_of(stage_full(
            &fs,
            &root,
            &run,
            Durability::Normal,
            &no_heartbeat,
            &no_before_create,
        ));
        let Some(Discard::Unreadable(why)) = &staged.discard else {
            panic!("{:?}", staged.discard)
        };
        assert!(why.contains("link"), "{why}");
        assert_eq!((staged.reused, staged.bytes_copied), (0, 5 * MIB as u64));
        assert_eq!(fs.bytes_read(PARTIAL) - p0, 0, "the partial was never read");
        let calls = fs.calls();
        let on_partial: Vec<&String> =
            calls[mark..].iter().filter(|c| c.contains(PARTIAL)).collect();
        assert!(
            on_partial
                .iter()
                .all(|c| !c.starts_with("open_existing(") && !c.starts_with("truncate_to(")),
            "the partial was reopened or truncated: {on_partial:?}"
        );
        let removed = format!("remove_file({PARTIAL})");
        let created = format!("create_new({PARTIAL})");
        let at = |what: &String| calls[mark..].iter().position(|c| c == what);
        assert!(
            matches!((at(&removed), at(&created)), (Some(r), Some(c)) if r < c),
            "the name was swept, then a new object created: {on_partial:?}"
        );
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src);
        assert!(run.finish(&no_heartbeat).is_clean());
    }

    #[test]
    fn a_refused_claim_reads_nothing_and_leaves_the_partial_alone() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let (s0, p0) = (fs.bytes_read("/src"), fs.bytes_read(PARTIAL));
        let mark = fs.calls().len();
        let claims = AtomicUsize::new(0);
        let refuse = || {
            claims.fetch_add(1, Ordering::SeqCst);
            Err(CopyError::at(
                CopyStep::Claim,
                FsError::new(Code::IoError, std::io::Error::other("claimed elsewhere")),
            ))
        };
        let run = run_gen(&fs, 2, Durability::Normal);
        let e = stage_full(&fs, &root, &run, Durability::Normal, &no_heartbeat, &refuse)
            .err()
            .expect("the claim is refused");
        assert_eq!(e.step, CopyStep::Claim);
        assert_eq!(claims.load(Ordering::SeqCst), 1);
        assert_eq!(fs.bytes_read("/src") - s0, 0);
        assert_eq!(fs.bytes_read(PARTIAL) - p0, 0);
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src[..3 * MIB]);
        let calls = fs.calls();
        assert!(
            calls[mark..].iter().all(|c| !c.contains(PARTIAL)),
            "nothing touched the partial: {:?}",
            &calls[mark..]
        );
        run.abandon();
    }

    #[test]
    fn the_resume_gate_encodes_the_source_time_as_the_sink_recorded_it() {
        // After the epoch with nanoseconds, and before it (floored seconds): both must round-trip through the WAL.
        for t in [
            SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 700),
            SystemTime::UNIX_EPOCH - Duration::new(5, 250_000_000),
        ] {
            let (fs, root) = ckpt_fs(5 * MIB);
            fs.set_modified("/src", t);
            let src = fs.read_file("/src").unwrap();
            crash_after(&fs, &root, 3);
            fs.write_file(PARTIAL, &src[..3 * MIB]);
            let run = run_gen(&fs, 2, Durability::Normal);
            let staged = staged_of(stage_full(
                &fs,
                &root,
                &run,
                Durability::Normal,
                &no_heartbeat,
                &no_before_create,
            ));
            assert_eq!((staged.reused, staged.discard), (3 * MIB as u64, None), "{t:?}");
            run.abandon();
        }
    }

    // ---- cut 13 task 3: copy-side verification ----

    const TEMP: &str = "/dst.flux-partial.op1";

    fn level(v: flux_fs::VerifyLevel) -> CopyOptions {
        CopyOptions { verify: v, ..opts() }
    }

    fn run_copy(fs: &FaultFs, o: &CopyOptions) -> std::result::Result<Outcome, CopyError> {
        copy_file(fs, Path::new("/src"), Path::new("/dst"), o)
    }

    fn seeded(bytes: &[u8]) -> FaultFs {
        let fs = FaultFs::new();
        fs.write_file("/src", bytes);
        fs
    }

    /// The 1-based ordinal of the first `call` among the calls that start with `kind`, searching from `after`.
    fn ordinal(calls: &[String], kind: &str, call: &str, after: usize) -> u32 {
        let mut n = 0;
        for (i, c) in calls.iter().enumerate() {
            if c.starts_with(kind) {
                n += 1;
                if i >= after && c == call {
                    return n;
                }
            }
        }
        panic!("{call} not found after {after} in {calls:?}");
    }

    /// The call log of a reference run (same seed, same options), whose ordinals the tests assert.
    fn reference(bytes: &[u8], o: &CopyOptions) -> Vec<String> {
        let fs = seeded(bytes);
        run_copy(&fs, o).unwrap();
        fs.calls()
    }

    fn expect_err(r: std::result::Result<Outcome, CopyError>) -> CopyError {
        match r {
            Ok(o) => panic!("expected an error, got {o:?}"),
            Err(e) => e,
        }
    }

    use flux_fs::VerifyLevel::{Destination, Full, SourceStream};

    #[test]
    fn none_adds_no_filesystem_calls() {
        let fs = seeded(b"hello");
        let out = run_copy(&fs, &opts()).unwrap();
        assert!(!out.verified);
        // Recorded from the code BEFORE `stage_file` gained its verification steps.
        let golden: Vec<String> = [
            "remove_file(/dst.flux-partial.op1)",
            "metadata(/src)",
            "metadata(/dst)",
            "open_read(/src)",
            "create_new(/dst.flux-partial.op1)",
            "set_times(/dst.flux-partial.op1)",
            "set_permissions(/dst.flux-partial.op1)",
            "metadata(/src)",
            "drop_writer(/dst.flux-partial.op1)",
            "rename_replace(/dst.flux-partial.op1 -> /dst)",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(fs.calls(), golden);
    }

    #[test]
    fn destination_clean_copy_is_verified_and_published() {
        let fs = seeded(b"hello");
        let out = run_copy(&fs, &level(Destination)).unwrap();
        assert!(out.verified);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
        let calls = fs.calls();
        let create = calls.iter().position(|c| c == &format!("create_new({TEMP})")).unwrap();
        let reread = calls.iter().position(|c| c == &format!("open_read({TEMP})")).unwrap();
        let times = calls.iter().position(|c| c == &format!("set_times({TEMP})")).unwrap();
        assert!(
            create < reread && reread < times,
            "re-read after the stream, before metadata: {calls:?}"
        );
    }

    /// Runs a `Destination` copy of `bytes` over a fresh fake where `hook` fires just before the temporary's re-read.
    fn destination_with_hook_before_reread(
        bytes: &[u8],
        seed_dst: Option<&[u8]>,
        hook: impl FnOnce(&FaultFs) + Send + 'static,
    ) -> (FaultFs, std::result::Result<Outcome, CopyError>) {
        let o = level(Destination);
        let refer = FaultFs::new();
        refer.write_file("/src", bytes);
        if let Some(d) = seed_dst {
            refer.write_file("/dst", d);
        }
        run_copy(&refer, &o).unwrap();
        let calls = refer.calls();
        let n = ordinal(&calls, "open_read(", &format!("open_read({TEMP})"), 0);
        assert_eq!(n, 2, "the temporary's re-read is the second open_read: {calls:?}");
        let fs = seeded(bytes);
        if let Some(d) = seed_dst {
            fs.write_file("/dst", d);
        }
        fs.on_nth("open_read", n, hook);
        let r = run_copy(&fs, &o);
        (fs, r)
    }

    #[test]
    fn destination_mismatch_is_discarded_and_never_published() {
        let (fs, r) = destination_with_hook_before_reread(b"hello", None, |fs| {
            fs.write_file(TEMP, b"HELLX");
        });
        let e = expect_err(r);
        assert_eq!(e.code(), Code::VerifyMismatch);
        assert_eq!(e.step, CopyStep::Verify);
        assert!(e.leftover.is_none());
        assert!(!fs.exists("/dst"));
        assert!(!fs.exists(TEMP));
    }

    #[test]
    fn a_mismatch_over_an_existing_file_leaves_it_untouched() {
        let (fs, r) = destination_with_hook_before_reread(b"hello", Some(b"old"), |fs| {
            fs.write_file(TEMP, b"HELLX");
        });
        assert_eq!(expect_err(r).code(), Code::VerifyMismatch);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"old"[..]));
        assert!(!fs.exists(TEMP));
    }

    #[test]
    fn an_empty_file_verifies() {
        let fs = seeded(b"");
        let out = run_copy(&fs, &level(Destination)).unwrap();
        assert!(out.verified);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b""[..]));
    }

    #[test]
    fn a_multi_buffer_file_verifies_and_a_late_corruption_is_caught() {
        let bytes = pattern(COPY_BUF_MAX * 3 + 17, 3);
        let (fs, r) = destination_with_hook_before_reread(&bytes, None, |_| {});
        assert!(r.unwrap().verified);
        assert_eq!(fs.read_file("/dst").unwrap(), bytes);

        let mut bad = bytes.clone();
        *bad.last_mut().unwrap() ^= 0xFF;
        let (fs, r) = destination_with_hook_before_reread(&bytes, None, move |fs| {
            fs.write_file(TEMP, &bad);
        });
        let e = expect_err(r);
        assert_eq!((e.code(), e.step), (Code::VerifyMismatch, CopyStep::Verify));
        assert!(!fs.exists("/dst") && !fs.exists(TEMP));
    }

    #[test]
    fn a_short_or_long_reread_is_a_mismatch() {
        for (name, reread) in [("short", b"hell".to_vec()), ("long", b"helloX".to_vec())] {
            let (fs, r) = destination_with_hook_before_reread(b"hello", None, move |fs| {
                fs.write_file(TEMP, &reread);
            });
            let e = expect_err(r);
            assert_eq!((e.code(), e.step), (Code::VerifyMismatch, CopyStep::Verify), "{name}");
            assert!(!fs.exists("/dst") && !fs.exists(TEMP), "{name}");
        }
    }

    #[test]
    fn a_reread_io_error_is_not_a_mismatch() {
        let fs = seeded(b"hello");
        fs.fail_read_nth(".flux-partial", 1, std::io::ErrorKind::Other);
        let e = expect_err(run_copy(&fs, &level(Destination)));
        assert_ne!(e.code(), Code::VerifyMismatch);
        assert_eq!(e.step, CopyStep::Verify);
        assert!(e.leftover.is_none());
        assert!(!fs.exists(TEMP) && !fs.exists("/dst"));
    }

    #[test]
    fn full_catches_a_source_that_changes_between_the_reads() {
        let o = level(Full);
        let calls = reference(b"hello", &o);
        let n = ordinal(&calls, "open_read(", "open_read(/src)", calls.len() / 2);
        assert_eq!(n, 3, "full's second source read is the third open_read: {calls:?}");
        let fs = seeded(b"hello");
        fs.on_nth("open_read", n, |fs| fs.write_file("/src", b"HELLX"));
        let e = expect_err(run_copy(&fs, &o));
        assert_eq!((e.code(), e.step), (Code::VerifyMismatch, CopyStep::Verify));
        assert!(
            e.cause
                .source
                .to_string()
                .contains("source bytes differ between the copy and the second read"),
            "{e}"
        );
        assert!(!fs.exists("/dst") && !fs.exists(TEMP));
    }

    #[test]
    fn full_clean_copy_reads_the_source_twice() {
        let fs = seeded(b"hello");
        let out = run_copy(&fs, &level(Full)).unwrap();
        assert!(out.verified);
        let calls = fs.calls();
        assert_eq!(calls.iter().filter(|c| *c == "open_read(/src)").count(), 2, "{calls:?}");
        assert_eq!(calls.iter().filter(|c| *c == &format!("open_read({TEMP})")).count(), 1);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn source_stream_hashes_but_compares_nothing() {
        let o = level(SourceStream);
        let calls = reference(b"hello", &o);
        let n = calls.iter().filter(|c| c.starts_with("set_times(")).count();
        assert_eq!(n, 1);
        let fs = seeded(b"hello");
        // After the stream, before the metadata: a destination that no longer holds the source bytes.
        fs.on_nth("set_times", 1, |fs| fs.write_file(TEMP, b"HELLX"));
        let out = run_copy(&fs, &o).unwrap();
        assert!(!out.verified);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"HELLX"[..]));
        assert!(!fs.called(&format!("open_read({TEMP})")), "{:?}", fs.calls());
        assert_eq!(fs.calls().iter().filter(|c| c.starts_with("open_read(")).count(), 1);
    }

    #[test]
    fn the_early_recheck_runs_before_the_reread() {
        let o = level(Destination);
        let calls = reference(b"hello", &o);
        let create = calls.iter().position(|c| c.starts_with("create_new(")).unwrap();
        let n = ordinal(&calls, "metadata(", "metadata(/src)", create);
        assert_eq!(n, 3, "the early recheck is the third metadata call: {calls:?}");
        let fs = seeded(b"hello");
        fs.on_nth("metadata", n, |fs| fs.write_file("/src", b"hello, but longer"));
        let e = expect_err(run_copy(&fs, &o));
        assert_eq!((e.code(), e.step), (Code::SourceChanged, CopyStep::Recheck));
        assert!(!fs.called(&format!("open_read({TEMP})")), "{:?}", fs.calls());
        assert!(!fs.exists(TEMP) && !fs.exists("/dst"));
    }

    #[test]
    fn a_failed_heartbeat_during_the_reread_discards_the_temporary() {
        fn beats(v: flux_fs::VerifyLevel) -> usize {
            let fs = seeded(b"hello");
            let n = std::cell::Cell::new(0usize);
            let beat = || {
                n.set(n.get() + 1);
                Ok(())
            };
            copy_file_guarded(
                &fs,
                Path::new("/src"),
                &fs.destination_root(Path::new("/")).unwrap(),
                OsStr::new("dst"),
                &level(v),
                &unguarded,
                &beat,
                &no_before_create,
                &no_before_publish,
            )
            .unwrap();
            n.get()
        }
        let (none, dest) = (beats(flux_fs::VerifyLevel::None), beats(Destination));
        assert!(dest > none, "the re-read beats at least once ({none} vs {dest})");
        // The re-read's first beat is the last one the `None` run makes before its final (publish) beat.
        let fail_at = none;
        let fs = seeded(b"hello");
        let n = std::cell::Cell::new(0usize);
        let beat = || {
            n.set(n.get() + 1);
            if n.get() == fail_at {
                Err(FsError::new(Code::IoError, std::io::Error::other("lock lost")))
            } else {
                Ok(())
            }
        };
        let e = expect_err(copy_file_guarded(
            &fs,
            Path::new("/src"),
            &fs.destination_root(Path::new("/")).unwrap(),
            OsStr::new("dst"),
            &level(Destination),
            &unguarded,
            &beat,
            &no_before_create,
            &no_before_publish,
        ));
        assert_eq!(e.step, CopyStep::Heartbeat);
        assert!(
            fs.called(&format!("open_read({TEMP})")),
            "the beat failed inside the re-read: {:?}",
            fs.calls()
        );
        assert!(!fs.called("set_times"), "the re-read comes before the metadata: {:?}", fs.calls());
        assert!(!fs.exists(TEMP) && !fs.exists("/dst"));
    }

    #[test]
    fn verifying_a_resumed_partial_is_refused_before_it_is_touched() {
        let (fs, root, src) = resume_fixture(5 * MIB, 3);
        fs.write_file(PARTIAL, &src[..3 * MIB]);
        let run = run_gen(&fs, 2, Durability::Normal);
        let fc = FileCkpt { run: &run, file_id: b"f".to_vec(), destination: b"dst".to_vec() };
        let mark = fs.calls().len();
        let e = stage_file(
            &fs,
            Path::new("/src"),
            &root,
            OsStr::new("dst"),
            &level(Destination),
            &unguarded,
            &no_heartbeat,
            &no_before_create,
            SyncAt::Staging,
            Some(Hook::Logged(&fc)),
        )
        .err()
        .expect("a verified resume is refused");
        assert_eq!((e.step, e.code()), (CopyStep::Verify, Code::CopyFailed));
        assert_eq!(fs.read_file(PARTIAL).unwrap(), src[..3 * MIB]);
        let after = fs.calls()[mark..].to_vec();
        assert!(
            after.iter().all(|c| !c.starts_with("truncate_to(")
                && !c.starts_with("remove_file(")
                && !c.starts_with("create_new(")),
            "the partial is not touched: {after:?}"
        );
        run.abandon();
    }

    #[test]
    fn publish_staged_reports_verified() {
        for (v, want) in [
            (flux_fs::VerifyLevel::None, false),
            (Destination, true),
            (Full, true),
            (SourceStream, false),
        ] {
            let fs = seeded(b"hello");
            let root = fs.destination_root(Path::new("/")).unwrap();
            let staged = stage_with(&fs, &root, &level(v), SyncAt::Staging);
            assert_eq!(staged.verified, want, "{v:?}");
            let out = publish_staged(
                &root,
                OsStr::new("dst"),
                staged,
                Publish::Replace,
                &unguarded,
                &no_heartbeat,
            )
            .unwrap();
            assert_eq!(out.verified, want, "{v:?}");
        }
    }
}
