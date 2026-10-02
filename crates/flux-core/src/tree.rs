//! Recursive copy (cut 4b): `copy_tree` drives the ordered walk and `copy_file_at`,
//! writing only through destination directory handles (§149.7).
//!
//! Design authority: `docs/superpowers/specs/2026-09-26-cut-4b-copy-tree-design.md`.

use crate::copy::{
    CopyError, CopyStep, Guard, Heartbeat, copy_file_guarded, no_heartbeat, split_destination,
    unguarded, weaker,
};
use crate::state::{FLUX_DIR, RESERVED_DIRS};
use crate::walk::{Walk, WalkEvent, walk};
use flux_fs::{
    Code, CopyOptions, DestinationRoot, DirHandle, FileIdentity, FileSystem, FsError,
    MetadataFailure, ObjectId, Publish, Safety,
};
use std::collections::{BTreeMap, HashSet};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// What a tree copy did, counted. Individual failures are STREAMED to the sink as
/// they happen, never accumulated here, so a tree with a million failures costs what a
/// clean one does.
#[derive(Debug, Default)]
pub struct TreeOutcome {
    pub files_copied: u64,
    pub bytes_copied: u64,
    /// Directories THIS operation created, the root included when it made it. A
    /// pre-existing directory merged into is not counted.
    pub directories_created: u64,
    pub failures: FailureTally,
    /// Identity comparisons that were SKIPPED because a side was not `Strong`. Not
    /// failures: the copies happened. The engine captures; cut 5's CLI renders.
    pub warnings: WeakIdentityWarnings,
    /// Every non-directory entry the walk yielded - file, symlink or special - including
    /// those under a skipped subtree (the walk still yields them).
    pub files_total: u64,
    /// Files whose Step 2a identity comparison was skipped for a weak side. Directories'
    /// skipped comparisons are in `warnings` only; §51's field is "files degraded".
    pub files_degraded: u64,
    /// Special files skipped (§233.1). Not failures.
    pub special_files_skipped: u64,
}

/// The operation stopped as a whole (cut 5, K1). `outcome` holds what was counted
/// before it stopped, so a caller can report it and tell §55's exit 3 ("refused before
/// changing anything") from exit 1.
#[derive(Debug)]
pub struct TreeAbort {
    pub error: CopyError,
    pub outcome: TreeOutcome,
}

impl TreeAbort {
    /// Whether the destination may differ from before the run: a directory this
    /// operation created, a file it published, or a temporary it could not remove.
    pub fn changed(&self) -> bool {
        self.outcome.directories_created > 0
            || self.outcome.files_copied > 0
            || self.error.leftover.is_some()
    }

    /// §55's exit-3 rule for an abort: a refusal code, after nothing changed and no streamed failure. The CLI's exit
    /// code and the run's rollback of a refused copy (cut 7a Part 3b, Q-I) decide by this one rule.
    pub fn refused_unchanged(&self) -> bool {
        !self.changed()
            && self.outcome.failures.is_empty()
            && matches!(self.error.code(), Code::SafetyRejected | Code::NoReplacePublishUnavailable)
    }
}

/// One streamed record: every failure, plus the one non-failure `SpecialFileSkipped`.
#[derive(Debug)]
pub struct TreeFailure {
    /// Relative to the source root, as the walk reports it.
    pub path: PathBuf,
    pub cause: TreeFailureCause,
}

/// Each variant is defined by WHERE the failure happened, so no failure can be
/// expressed two ways.
#[derive(Debug)]
pub enum TreeFailureCause {
    /// The walk could not read or enter something.
    Walk(FsError),
    /// A destination directory could not be created or entered - including a
    /// `DESTINATION_NAMESPACE_COLLISION` between two source names the destination folds
    /// into one. Its subtree is skipped and reported once, here.
    CreateDir(FsError),
    /// One file's copy failed. `CopyError` intact, so a leftover temporary is still
    /// reported per file.
    Copy(CopyError),
    /// A symlink: §25's default is to copy it AS a link, which this version cannot do,
    /// so the action fails (the CLI renders `SYMLINK_CREATION_UNAVAILABLE`).
    Symlink,
    /// NOT a failure: a device, socket or FIFO, skipped as §233.1 prescribes ("SKIP +
    /// DURABLE WARNING") and streamed so each one is reported by its own record. The
    /// one variant `FailureTally` does not count; `TreeOutcome` counts it in
    /// `special_files_skipped`.
    SpecialFileSkipped,
    /// The file IS at the destination; some of its metadata could not be applied.
    /// Published with complaints, which exits 1 like the single-file case.
    PublishedWithComplaints(Vec<MetadataFailure>),
}

/// One counter per `TreeFailureCause` variant. Fixed size: it costs the same on a
/// clean tree and on one where every entry failed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FailureTally {
    pub walk: u64,
    pub create_dir: u64,
    pub copy: u64,
    pub symlink: u64,
    pub published_with_complaints: u64,
}

impl FailureTally {
    /// The only total. Derived, never stored beside the parts.
    pub fn total(&self) -> u64 {
        self.walk + self.create_dir + self.copy + self.symlink + self.published_with_complaints
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// EXHAUSTIVE, with no wildcard arm: a new cause must name its counter here or the
    /// crate does not compile. A `_ =>` arm would let a new cause count nothing, and the
    /// miscount would surface only as a wrong exit code.
    pub(crate) fn count(&mut self, cause: &TreeFailureCause) {
        match cause {
            TreeFailureCause::Walk(_) => self.walk += 1,
            TreeFailureCause::CreateDir(_) => self.create_dir += 1,
            TreeFailureCause::Copy(_) => self.copy += 1,
            TreeFailureCause::Symlink => self.symlink += 1,
            // Not a failure (§233.1): `report` counts it in
            // `TreeOutcome::special_files_skipped`; named here so the match stays exhaustive.
            TreeFailureCause::SpecialFileSkipped => {}
            TreeFailureCause::PublishedWithComplaints(_) => self.published_with_complaints += 1,
        }
    }
}

/// Skipped identity comparisons, aggregated for one warning apiece (walker design,
/// "When identity is weak or unavailable").
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WeakIdentityWarnings {
    /// One group per affected volume (`ObjectId::volume`), in stable order.
    pub weak: BTreeMap<u64, DegradedGroup>,
    /// Everything whose identity was `Unavailable`, which names no volume to group by.
    pub unavailable: Option<DegradedGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradedGroup {
    pub count: u64,
    /// The first path that degraded, relative to the source root. The EMPTY path is the
    /// source root itself (the §129 pre-flight).
    pub example: PathBuf,
}

impl WeakIdentityWarnings {
    pub fn is_empty(&self) -> bool {
        self.weak.is_empty() && self.unavailable.is_none()
    }

    /// Record one comparison that was skipped. `weaker` is the weaker side, as
    /// `copy::weaker` computes it; `Strong` is not a degradation and is ignored.
    pub(crate) fn record(&mut self, weaker: FileIdentity, example: &Path) {
        let group = match weaker {
            FileIdentity::Strong(_) => return,
            FileIdentity::Weak(id) => self
                .weak
                .entry(id.volume)
                .or_insert_with(|| DegradedGroup { count: 0, example: example.to_path_buf() }),
            FileIdentity::Unavailable => self
                .unavailable
                .get_or_insert_with(|| DegradedGroup { count: 0, example: example.to_path_buf() }),
        };
        group.count += 1;
    }
}

/// The directory one frame of the walk writes into, or a marker that its subtree was
/// refused. One entry per `Dir` the walk emits, popped at its `DirEnd`: `walk.rs`'s
/// `enter` pushes a frame of its own before it returns `Dir` and none on `Err`, and
/// `next` emits `DirEnd` for every such frame, so this stack cannot drift from the walk.
enum Frame<D> {
    Live {
        dir: D,
        /// `Strong` identities of the directories this operation created INSIDE `dir`.
        /// A fold is two source names in ONE source directory landing on one
        /// destination name, so both creations happen here; the set is dropped with the
        /// frame, which bounds it like the walk's one-directory listing (spec line 997,
        /// invariant 11).
        created: HashSet<ObjectId>,
    },
    /// Its directory failed; everything below it is skipped, and was reported once.
    Skipped,
}

/// Copy the tree at `src_root` into `dst_root`, writing only through directory
/// handles. See the design for the full contract; in brief:
///
/// - The outer `Err` means the operation did not happen or had to stop: the source
///   root missing or not a directory; the lexical floor; the §129 pre-flight (an
///   identity match, or a degraded comparison under `Safety::Strict`); a directory
///   reached mid-walk that is the destination by identity, or whose comparison is
///   degraded under `Strict`; a destination directory about to be entered that IS the
///   source root by identity; a destination root that cannot be resolved or created;
///   and the first publish reporting that the no-replace primitive is unavailable.
/// - Every other failure, and every skipped special file (§233.1, not a failure), goes
///   to `on_report`, and the walk continues.
/// - Every file is published with `Publish::NoReplace` whatever `opts.publish` says
///   (§241.5): an existing destination file is never replaced; it is reported
///   `DESTINATION_NAMESPACE_COLLISION`.
// `TreeAbort` carries the whole partial `TreeOutcome` by design (cut 5, K1); it is
// returned once per operation, never in a loop, so its size costs nothing.
#[allow(clippy::result_large_err)]
pub fn copy_tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<TreeOutcome, TreeAbort> {
    let mut out = TreeOutcome::default();
    match run_tree(fs, src_root, dst_root, opts, &mut out, on_report) {
        Ok(()) => Ok(out),
        Err(error) => Err(TreeAbort { error, outcome: out }),
    }
}

/// The source side of a tree copy, checked before anything at the destination is touched.
pub(crate) struct Source<'a, F: FileSystem> {
    pub(crate) events: Walk<'a, F>,
    pub(crate) identity: FileIdentity,
}

/// Steps 1-2 of `copy_tree`, which touch no destination: the source root and its identity, and the lexical floor. The
/// run (cut 7a Part 3b, B1) calls this before it takes the destination's lock, so a mistyped source creates nothing.
pub(crate) fn prepare_source<'a, F: FileSystem>(
    fs: &'a F,
    src_root: &'a Path,
    dst_root: &Path,
) -> std::result::Result<Source<'a, F>, CopyError> {
    // 1. The source root. `walk` refuses a missing or non-directory root: the whole
    //    operation failing, before any destination call.
    let events = walk(fs, src_root).map_err(|e| CopyError::at(CopyStep::Source, e))?;
    let identity = fs.metadata(src_root).map_err(|e| CopyError::at(CopyStep::Source, e))?.identity;

    // 2. The lexical floor, before any destination call (§129's own example, `/data`
    //    into `/data/backup`). Runs at every identity strength.
    if lexically_within(dst_root, src_root) {
        return Err(refuse("the destination is the source or lies inside it"));
    }
    Ok(Source { events, identity })
}

/// What every entry of one tree copy shares.
pub(crate) struct Shared<'c, F: DestinationRoot> {
    pub(crate) fs: &'c F,
    pub(crate) src_root: &'c Path,
    pub(crate) src_identity: FileIdentity,
    pub(crate) opts: &'c CopyOptions,
    /// §99's check before each destination mutation; `&unguarded` for a copy that holds no lock.
    pub(crate) guard: &'c Guard<'c>,
    /// §101's heartbeat (cut 7b), beside the guard; `&no_heartbeat` for a copy that holds no lock.
    pub(crate) beat: &'c Heartbeat<'c>,
}

/// `copy_tree`'s body. Every `?` here is an abort; `copy_tree` pairs it with `out`,
/// which holds whatever was counted before it.
fn run_tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    let source = prepare_source(fs, src_root, dst_root)?;

    // 3-5. Resolve the destination, compare it with the source, create the root.
    let resolve = |e| CopyError::at(CopyStep::Resolve, e);
    let root = match fs.destination_root(dst_root) {
        Ok(root) => {
            preflight(
                source.identity,
                root.identity().map_err(resolve)?,
                opts.safety,
                &mut out.warnings,
            )?;
            root
        }
        Err(e) if e.source.kind() == ErrorKind::NotFound => {
            let (parent_path, name) = split_destination(dst_root)?;
            let parent = fs.destination_root(parent_path).map_err(resolve)?;
            preflight(
                source.identity,
                parent.identity().map_err(resolve)?,
                opts.safety,
                &mut out.warnings,
            )?;
            // Decision 8 on the root, every failure fatal. Nothing else is created on
            // `parent`, so there is no sibling to fold against.
            match parent.create_dir(name) {
                Ok(root) => {
                    out.directories_created += 1;
                    root
                }
                Err(e) if e.source.kind() == ErrorKind::AlreadyExists => {
                    parent.open_dir(name).map_err(resolve)?
                }
                Err(e) => return Err(resolve(e)),
            }
        }
        Err(e) => return Err(resolve(e)),
    };
    let cx = Shared {
        fs,
        src_root,
        src_identity: source.identity,
        opts,
        guard: &unguarded,
        beat: &no_heartbeat,
    };
    copy_tree_at(&cx, source.events, root, out, on_report).1
}

/// Step 6 of `copy_tree`: the walk, writing through `root` and only below it, and `root` handed back with the result
/// for a caller that goes on writing through it (the run's finish).
///
/// `cx.guard` runs before every destination mutation (§99, cut 7a Part 3b): before each directory is created here, and
/// inside `copy_file_guarded` for each file. A failed guard aborts the whole copy with `TARGET_LOCK_BUSY`. A source
/// entry whose destination is a reserved control path (`reserved_path`) fails with `CONTROL_PLANE_NAMESPACE_CONFLICT`
/// and the rest continues. `cx.beat` runs before each of those guard calls; its failure aborts the whole copy too
/// (cut 7b).
pub(crate) fn copy_tree_at<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    events: Walk<'_, F>,
    root: F::Dir,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> (F::Dir, std::result::Result<(), CopyError>) {
    let root_identity = match root.identity() {
        Ok(i) => i,
        Err(e) => return (root, Err(CopyError::at(CopyStep::Resolve, e))),
    };
    let mut stack = vec![Frame::Live { dir: root, created: HashSet::new() }];
    let walked = walk_into(cx, events, root_identity, &mut stack, out, on_report);
    // The walk emits no `Dir` for the root, so no `DirEnd` pops it, and a frame is skipped only when pushed.
    match stack.into_iter().next() {
        Some(Frame::Live { dir, .. }) => (dir, walked),
        _ => unreachable!("the root frame is never popped, and never skipped"),
    }
}

/// `copy_tree_at`'s loop. Every `?` here is an abort.
fn walk_into<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    events: Walk<'_, F>,
    root_identity: FileIdentity,
    stack: &mut Vec<Frame<F::Dir>>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    // 6. The walk. NoReplace is mandatory in a tree (§241.5): every target is planned
    //    as new, and an existing one is refused at Step 2a (F3).
    let opts = CopyOptions { publish: Publish::NoReplace, ..cx.opts.clone() };
    let cx = Shared {
        fs: cx.fs,
        src_root: cx.src_root,
        src_identity: cx.src_identity,
        opts: &opts,
        guard: cx.guard,
        beat: cx.beat,
    };
    for item in events {
        let live = matches!(stack.last(), Some(Frame::Live { .. }));
        let event = match item {
            Ok(event) => event,
            // Under a skipped subtree the walk still reads; its errors there belong to
            // a failure already reported once.
            Err(e) => {
                if live {
                    report(out, on_report, e.path, TreeFailureCause::Walk(e.cause));
                }
                continue;
            }
        };
        match event {
            WalkEvent::Dir { path, identity } => {
                let frame = if !live {
                    Frame::Skipped
                } else if reserved_path(&path) {
                    let conflict = TreeFailureCause::CreateDir(reserved_conflict());
                    report(out, on_report, path.clone(), conflict);
                    Frame::Skipped
                } else {
                    // §101, then §99, before the directory's creation.
                    (cx.beat)().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
                    (cx.guard)().map_err(|e| CopyError::at(CopyStep::Create, e))?;
                    enter_dir(stack, &path, identity, root_identity, cx.opts, out, on_report)?
                };
                // A destination directory about to be entered must not BE the source root
                // (owner, cut 4b capstone round 1): a bind mount inside the destination can
                // present the source under a destination name, and `open_dir` refuses only
                // name-surrogates. Aliases of source SUBdirectories stay the §42 mount cut's.
                if let Frame::Live { dir, .. } = &frame
                    && let (Ok(FileIdentity::Strong(a)), FileIdentity::Strong(b)) =
                        (dir.identity(), cx.src_identity)
                    && a == b
                {
                    return Err(refuse(
                        "a destination directory is the source root itself, by identity",
                    ));
                }
                stack.push(frame);
            }
            WalkEvent::File { path } => {
                out.files_total += 1;
                if let Some(Frame::Live { dir, .. }) = stack.last() {
                    if reserved_path(&path) {
                        let conflict = CopyError::at(CopyStep::Gate, reserved_conflict());
                        report(out, on_report, path, TreeFailureCause::Copy(conflict));
                    } else {
                        copy_one(&cx, dir, path, out, on_report)?;
                    }
                }
            }
            WalkEvent::Symlink { path } => {
                out.files_total += 1;
                if live {
                    report(out, on_report, path, TreeFailureCause::Symlink);
                }
            }
            WalkEvent::Other { path } => {
                out.files_total += 1;
                if live {
                    report(out, on_report, path, TreeFailureCause::SpecialFileSkipped);
                }
            }
            WalkEvent::DirEnd { .. } => {
                stack.pop();
            }
        }
    }
    Ok(())
}

/// P3-F: a path below the source root, so below DEST, that IS a reserved control directory - `.flux/operations`,
/// `.flux/standalone`, `.flux/atomic` - or lies below one. ASCII case-insensitive, because a case-folding destination
/// folds `.FLUX/Operations` onto the reserved name. Everything else under `.flux` is ordinary data (§259.3).
pub(crate) fn reserved_path(path: &Path) -> bool {
    let mut parts = path.components();
    let (Some(Component::Normal(first)), Some(Component::Normal(second))) =
        (parts.next(), parts.next())
    else {
        return false;
    };
    let same =
        |a: &std::ffi::OsStr, b: &str| a.as_encoded_bytes().eq_ignore_ascii_case(b.as_bytes());
    same(first, FLUX_DIR) && RESERVED_DIRS.iter().any(|r| same(second, r))
}

fn reserved_conflict() -> FsError {
    FsError::new(
        Code::ControlPlaneNamespaceConflict,
        std::io::Error::other(
            "its destination is a reserved Flux control path (DEST/.flux/operations, standalone or atomic); not copied",
        ),
    )
}

/// A `Dir` event under a live frame: the dynamic §129 check, then decision 8.
fn enter_dir<D: DirHandle>(
    stack: &mut [Frame<D>],
    path: &Path,
    identity: FileIdentity,
    root_identity: FileIdentity,
    opts: &CopyOptions,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<Frame<D>, CopyError> {
    // The dynamic half of §129 / §149.6: a directory reached mid-walk that IS the
    // destination root means the source reached it by an alias. Checked before
    // anything is created for it.
    match (identity, root_identity) {
        (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b => {
            return Err(refuse("a source directory is the destination itself, by identity"));
        }
        (FileIdentity::Strong(_), FileIdentity::Strong(_)) => {}
        (s, d) => match opts.safety {
            Safety::Default => out.warnings.record(weaker(s, d), path),
            Safety::Strict => {
                return Err(refuse(
                    "a source directory cannot be compared with the destination with full confidence",
                ));
            }
        },
    }

    let Some(Frame::Live { dir: parent, created }) = stack.last_mut() else {
        unreachable!("enter_dir is called only under a live frame");
    };
    let name = path.file_name().expect("a walk path ends in a name");
    // Decision 8: create first; open on AlreadyExists. Neither call traverses a link.
    let failure = match parent.create_dir(name) {
        Ok(child) => {
            out.directories_created += 1;
            if let Ok(FileIdentity::Strong(id)) = child.identity() {
                created.insert(id);
            }
            return Ok(Frame::Live { dir: child, created: HashSet::new() });
        }
        Err(e) if e.source.kind() == ErrorKind::AlreadyExists => match parent.open_dir(name) {
            Ok(child) => match child.identity() {
                // Created by THIS operation moments ago, under another source name.
                Ok(FileIdentity::Strong(id)) if created.contains(&id) => FsError::new(
                    Code::DestinationNamespaceCollision,
                    std::io::Error::other(
                        "this operation already created that directory under another source name",
                    ),
                ),
                // Pre-existing: merge into it.
                _ => return Ok(Frame::Live { dir: child, created: HashSet::new() }),
            },
            // SAFETY_REJECTED (a link) or DESTINATION_ERROR (a file, or it vanished).
            Err(e) => e,
        },
        Err(e) => e,
    };
    report(out, on_report, path.to_path_buf(), TreeFailureCause::CreateDir(failure));
    Ok(Frame::Skipped)
}

/// A `File` event under a live frame.
fn copy_one<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    path: PathBuf,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    let name = path.file_name().expect("a walk path ends in a name");
    match copy_file_guarded(
        cx.fs,
        &cx.src_root.join(&path),
        parent,
        name,
        cx.opts,
        cx.guard,
        cx.beat,
    ) {
        Ok(o) => {
            out.files_copied += 1;
            out.bytes_copied += o.bytes_copied;
            if let Some(weaker) = o.identity_degraded {
                out.files_degraded += 1;
                out.warnings.record(weaker, &path);
            }
            if !o.metadata_failures.is_empty() {
                report(
                    out,
                    on_report,
                    path,
                    TreeFailureCause::PublishedWithComplaints(o.metadata_failures),
                );
            }
            Ok(())
        }
        Err(mut e) => {
            // `copy_file_at` records a leftover relative to its handle; rebuild it in
            // the tree's frame, relative to the destination root.
            if let Some((p, _)) = e.leftover.as_mut() {
                *p = path.with_file_name(&*p);
            }
            // §99 (cut 7a Part 3b): this run no longer owns the destination's lock; or §101 (cut 7b): its heartbeat
            // failed. The whole operation stops.
            if e.code() == Code::TargetLockBusy || e.step == CopyStep::Heartbeat {
                return Err(e);
            }
            // Decision 3: this destination lacks a no-replace primitive. Found at the
            // FIRST publish, so the whole operation stops instead of failing every file.
            if e.step == CopyStep::Publish && primitive_unavailable(&e.cause.source) {
                e.cause.code = Code::NoReplacePublishUnavailable;
                return Err(e);
            }
            // F3 and §241.5: the name was taken - by a file that already existed
            // (refused at Step 2a) or one that appeared before publish (the no-replace
            // rename refused it). `source` is kept, so `raw_os_error` survives.
            if matches!(e.step, CopyStep::Gate | CopyStep::Publish)
                && e.cause.source.kind() == ErrorKind::AlreadyExists
            {
                e.cause.code = Code::DestinationNamespaceCollision;
            }
            report(out, on_report, path, TreeFailureCause::Copy(e));
            Ok(())
        }
    }
}

/// "Primitive unavailable" (decision 3). `Unsupported` covers ENOSYS and EOPNOTSUPP
/// (std maps both there) and the fake's `set_no_replace_support(false)`; a RAW `EINVAL`
/// on unix is the measured WSL 9p answer. An `InvalidInput` with no raw OS error is
/// one Flux raised itself (an interior NUL), not the filesystem. Sound only for a
/// failure at `CopyStep::Publish`: the temporary's name contains the target's, so a
/// name the filesystem rejects fails at `Create` first.
fn primitive_unavailable(e: &std::io::Error) -> bool {
    e.kind() == ErrorKind::Unsupported
        || (cfg!(unix) && e.kind() == ErrorKind::InvalidInput && e.raw_os_error().is_some())
}

/// The §129 pre-flight: the source root against the RESOLVED destination anchor
/// (decision 2). A destination symlinked to the source is caught here, by identity.
pub(crate) fn preflight(
    src: FileIdentity,
    anchor: FileIdentity,
    safety: Safety,
    warnings: &mut WeakIdentityWarnings,
) -> std::result::Result<(), CopyError> {
    match (src, anchor) {
        (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b => {
            Err(refuse("the destination resolves to the source, by identity"))
        }
        (FileIdentity::Strong(_), FileIdentity::Strong(_)) => Ok(()),
        (s, d) => match safety {
            Safety::Default => {
                warnings.record(weaker(s, d), Path::new(""));
                Ok(())
            }
            Safety::Strict => Err(refuse(
                "the source cannot be compared with the destination with full confidence",
            )),
        },
    }
}

/// `inner` equals `outer` or lies inside it, compared component by component with `.`
/// dropped and no filesystem access. `..` is not resolved (that needs the
/// filesystem): a spelling through `..` is compared as written, and identity is the
/// check that sees through it. Paths are compared as given; cut 5's CLI canonicalizes
/// both roots before calling the engine.
fn lexically_within(inner: &Path, outer: &Path) -> bool {
    fn parts(p: &Path) -> Vec<Component<'_>> {
        p.components().filter(|c| !matches!(c, Component::CurDir)).collect()
    }
    let (i, o) = (parts(inner), parts(outer));
    i.len() >= o.len() && i[..o.len()] == o[..]
}

fn refuse(why: &'static str) -> CopyError {
    CopyError::at(CopyStep::Resolve, FsError::new(Code::SafetyRejected, std::io::Error::other(why)))
}

/// Count the record, then stream it. The only place either happens.
fn report(
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
    path: PathBuf,
    cause: TreeFailureCause,
) {
    if matches!(cause, TreeFailureCause::SpecialFileSkipped) {
        out.special_files_skipped += 1;
    }
    out.failures.count(&cause);
    on_report(TreeFailure { path, cause });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use flux_fs::{Code, Durability, FileSystem, ObjectId, OperationId, Preserve};

    #[test]
    fn the_tally_counts_each_cause_in_its_own_field() {
        let e = || FsError::new(Code::IoError, std::io::Error::other("x"));
        let mut t = FailureTally::default();
        t.count(&TreeFailureCause::Walk(e()));
        t.count(&TreeFailureCause::CreateDir(e()));
        t.count(&TreeFailureCause::Copy(CopyError::at(crate::copy::CopyStep::Create, e())));
        t.count(&TreeFailureCause::Symlink);
        t.count(&TreeFailureCause::PublishedWithComplaints(Vec::new()));
        t.count(&TreeFailureCause::SpecialFileSkipped);
        assert_eq!(
            t,
            FailureTally {
                walk: 1,
                create_dir: 1,
                copy: 1,
                symlink: 1,
                published_with_complaints: 1
            }
        );
        assert_eq!(t.total(), 5, "a skipped special file is not a failure");
        assert!(!t.is_empty());
        assert!(FailureTally::default().is_empty());
    }

    #[test]
    fn warnings_group_weak_by_volume_and_keep_the_first_example() {
        let weak = |volume| FileIdentity::Weak(ObjectId { volume, index: 1 });
        let mut w = WeakIdentityWarnings::default();
        w.record(weak(7), Path::new("a"));
        w.record(weak(7), Path::new("b"));
        w.record(weak(9), Path::new("c"));
        w.record(FileIdentity::Unavailable, Path::new("d"));
        w.record(FileIdentity::Strong(ObjectId { volume: 1, index: 1 }), Path::new("e"));

        assert_eq!(w.weak[&7], DegradedGroup { count: 2, example: PathBuf::from("a") });
        assert_eq!(w.weak[&9], DegradedGroup { count: 1, example: PathBuf::from("c") });
        assert_eq!(w.unavailable, Some(DegradedGroup { count: 1, example: PathBuf::from("d") }));
        assert!(!w.is_empty());
        assert!(WeakIdentityWarnings::default().is_empty());
    }

    fn opts() -> CopyOptions {
        CopyOptions {
            preserve_times: Preserve::Default,
            preserve_permissions: Preserve::Default,
            durability: Durability::Normal,
            publish: Publish::Replace,
            safety: Safety::Default,
            operation_id: OperationId::new("op1"),
        }
    }

    fn strict() -> CopyOptions {
        let mut o = opts();
        o.safety = Safety::Strict;
        o
    }

    fn run(
        fs: &FaultFs,
        src: &str,
        dst: &str,
        o: &CopyOptions,
    ) -> (std::result::Result<TreeOutcome, CopyError>, Vec<TreeFailure>) {
        let (r, got) = run_full(fs, src, dst, o);
        (r.map_err(|a| a.error), got)
    }

    /// `run`, keeping the whole `TreeAbort` (cut 5).
    fn run_full(
        fs: &FaultFs,
        src: &str,
        dst: &str,
        o: &CopyOptions,
    ) -> (std::result::Result<TreeOutcome, TreeAbort>, Vec<TreeFailure>) {
        let mut got = Vec::new();
        let r = copy_tree(fs, Path::new(src), Path::new(dst), o, &mut |f| got.push(f));
        (r, got)
    }

    /// `/src/a` (1 byte) and `/src/sub/b` (2 bytes). `/dst` absent.
    fn tree() -> FaultFs {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.create_dir(Path::new("/src/sub")).unwrap();
        fs.write_file("/src/a", b"A");
        fs.write_file("/src/sub/b", b"BB");
        fs
    }

    fn identity_of(fs: &FaultFs, p: &str) -> FileIdentity {
        fs.metadata(Path::new(p)).unwrap().identity
    }

    fn calls_since(fs: &FaultFs, n: usize) -> Vec<String> {
        fs.calls()[n..].to_vec()
    }

    #[test]
    fn lexically_within_compares_components() {
        assert!(lexically_within(Path::new("/data"), Path::new("/data")));
        assert!(lexically_within(Path::new("/data/backup"), Path::new("/data")));
        assert!(lexically_within(Path::new("/data/./backup"), Path::new("/data")));
        assert!(!lexically_within(Path::new("/database"), Path::new("/data")));
        assert!(!lexically_within(Path::new("/other"), Path::new("/data")));
    }

    #[test]
    fn a_missing_source_root_fails_the_whole_operation_before_the_destination() {
        let fs = FaultFs::new();
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let e = r.unwrap_err();
        assert_eq!(e.step, CopyStep::Source);
        assert!(got.is_empty());
        assert!(!fs.called("create_dir"));
    }

    #[test]
    fn a_destination_inside_the_source_is_refused_lexically_before_any_call() {
        for dst in ["/src", "/src/backup"] {
            let fs = tree();
            // A WEAK source identity, so the identity pre-flight cannot catch the overlap
            // (it degrades under Default and proceeds): the lexical floor is then the only
            // guard, and a mutant that disables it creates `/src/backup` and goes red.
            // Without this, the pre-flight refuses both spellings on its own (the anchor IS
            // `/src`) and the test could not tell the floor from the pre-flight (plan
            // panel round 1).
            fs.set_identity("/src", FileIdentity::Weak(ObjectId { volume: 5, index: 1 }));
            let n = fs.calls().len();
            let (r, _) = run(&fs, "/src", dst, &opts());
            assert_eq!(r.unwrap_err().code(), Code::SafetyRejected, "{dst}");
            assert!(
                !calls_since(&fs, n).iter().any(|c| c.starts_with("create_dir")),
                "{dst}: nothing created"
            );
        }
    }

    #[test]
    fn a_destination_that_is_the_source_by_identity_is_refused_before_anything_is_created() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_identity("/dst", identity_of(&fs, "/src"));
        let n = fs.calls().len();

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let e = r.unwrap_err();
        assert_eq!(e.code(), Code::SafetyRejected);
        assert_eq!(e.step, CopyStep::Resolve, "a tree-level refusal belongs to no copy step");
        assert!(got.is_empty());
        assert!(!calls_since(&fs, n).iter().any(|c| c.starts_with("create")));
    }

    #[test]
    fn an_absent_destination_is_anchored_on_its_parent() {
        let fs = tree();
        fs.create_dir(Path::new("/out")).unwrap();
        fs.set_identity("/out", identity_of(&fs, "/src"));

        let (r, _) = run(&fs, "/src", "/out/copy", &opts());

        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(!fs.exists("/out/copy"));
    }

    #[test]
    fn a_file_at_the_destination_root_is_refused_as_not_a_directory() {
        let fs = tree();
        fs.write_file("/dst", b"a file");
        let (r, _) = run(&fs, "/src", "/dst", &opts());
        let e = r.unwrap_err();
        assert_eq!(e.cause.source.kind(), std::io::ErrorKind::NotADirectory);
        // Test audit, cut 4b: without this, mislabelling the destination-resolution errors
        // (the `resolve` closure) as another step left the whole suite green.
        assert_eq!(e.step, CopyStep::Resolve);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"a file"[..]));
    }

    #[test]
    fn a_weak_preflight_warns_under_default_and_refuses_under_strict() {
        let weak = FileIdentity::Weak(ObjectId { volume: 9, index: 1 });

        let lax = tree();
        lax.set_identity("/src", weak);
        let (r, _) = run(&lax, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert_eq!(out.warnings.weak[&9].example, PathBuf::new(), "the root is the empty path");

        let tight = tree();
        tight.set_identity("/src", weak);
        let (r, _) = run(&tight, "/src", "/dst", &strict());
        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(!tight.exists("/dst"));
    }

    #[test]
    fn a_fresh_tree_is_copied_through_handles_with_no_replace() {
        let fs = tree();
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_copied, out.bytes_copied, out.directories_created), (2, 3, 2));
        assert!(out.failures.is_empty());
        assert!(out.warnings.is_empty());
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"A"[..]));
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]));
        assert!(fs.called("rename_no_replace"), "NoReplace is mandatory in a tree");
        assert!(!fs.called("rename_replace"), "the caller's Replace is overridden");
    }

    #[test]
    fn an_existing_destination_directory_is_merged_and_not_counted() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/sub")).unwrap();
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_copied, out.directories_created), (2, 0));
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]));
    }

    #[test]
    fn an_existing_destination_file_is_left_intact_and_reported_as_a_collision() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/a", b"old");

        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new("a"));
        match &got[0].cause {
            TreeFailureCause::Copy(e) => {
                assert_eq!(e.code(), Code::DestinationNamespaceCollision);
                assert_eq!(e.step, CopyStep::Gate);
            }
            other => panic!("expected Copy, got {other:?}"),
        }
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"old"[..]));
        assert!(
            !fs.calls().iter().any(|c| c.starts_with("create_new") && c.contains("a.flux-partial")),
            "the bytes were never copied"
        );
        assert_eq!(out.failures.copy, 1);
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]), "the walk continued");
    }

    #[test]
    fn a_file_that_is_the_source_by_identity_fails_alone_and_the_walk_continues() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/a", b"A");
        fs.set_identity("/dst/a", identity_of(&fs, "/src/a"));

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        match &got[0].cause {
            TreeFailureCause::Copy(e) => assert_eq!(e.code(), Code::SafetyRejected),
            other => panic!("expected Copy, got {other:?}"),
        }
        assert_eq!(out.files_copied, 1);
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]));
    }

    #[test]
    fn two_source_directories_folded_into_one_destination_are_a_collision() {
        // `A` and `a` in one source directory; the destination "folds" them: `/dst/a`
        // pre-exists carrying the identity `/dst/A` will get when this operation makes it.
        let fs = FaultFs::new();
        for d in ["/src", "/src/A", "/src/a", "/dst", "/dst/a"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/src/A/x", b"x");
        fs.write_file("/src/a/y", b"y");
        let folded = FileIdentity::Strong(ObjectId { volume: 1, index: 9_003 });
        fs.set_identity("/dst/A", folded);
        fs.set_identity("/dst/a", folded);

        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new("a"));
        match &got[0].cause {
            TreeFailureCause::CreateDir(e) => {
                assert_eq!(e.code, Code::DestinationNamespaceCollision)
            }
            other => panic!("expected CreateDir, got {other:?}"),
        }
        assert!(fs.exists("/dst/A/x"));
        assert!(!fs.exists("/dst/a/y"), "the folded subtree was not merged");
        assert_eq!(out.failures.create_dir, 1);
    }

    #[test]
    fn a_source_directory_that_is_the_destination_aborts_the_operation() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_identity("/src/sub", identity_of(&fs, "/dst"));

        let (r, _) = run(&fs, "/src", "/dst", &opts());

        let e = r.unwrap_err();
        assert_eq!(e.code(), Code::SafetyRejected);
        assert_eq!(e.step, CopyStep::Resolve);
        assert!(!fs.exists("/dst/sub/b"));
    }

    #[test]
    fn a_special_file_is_skipped_reported_and_not_a_failure() {
        // §233.1: SKIP + DURABLE WARNING, reported per record; the run may still succeed.
        let fs = tree();
        fs.add_special("/src/dev");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new("dev"));
        assert!(matches!(got[0].cause, TreeFailureCause::SpecialFileSkipped));
        assert!(!fs.exists("/dst/dev"));
        assert!(out.failures.is_empty(), "skipped, not failed");
        assert_eq!(out.special_files_skipped, 1);
        assert_eq!(out.files_copied, 2, "the rest of the tree still copies");
    }

    #[test]
    fn a_weak_directory_warns_under_default_and_aborts_under_strict() {
        let weak = FileIdentity::Weak(ObjectId { volume: 7, index: 1 });

        let lax = tree();
        lax.set_identity("/src/sub", weak);
        let (r, got) = run(&lax, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert!(got.is_empty());
        assert_eq!(
            out.warnings.weak[&7],
            DegradedGroup { count: 1, example: PathBuf::from("sub") }
        );
        assert_eq!(out.files_degraded, 0, "a directory's skipped comparison is not a file's");

        let tight = tree();
        tight.set_identity("/src/sub", weak);
        let (r, _) = run(&tight, "/src", "/dst", &strict());
        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
    }

    #[test]
    fn a_file_whose_destination_cannot_be_inspected_lands_in_the_unavailable_bucket() {
        // metadata calls in order: walk's root stat (1), copy_tree's source identity (2),
        // copy_file_at's source stat (3), the Step 2a gate's destination stat (4).
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/src/a", b"A");
        fs.fail_nth("metadata", 4, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        let stats: Vec<_> = fs.calls().into_iter().filter(|c| c.starts_with("metadata(")).collect();
        assert!(stats[3].contains("dst"), "the 4th stat is the gate's: {stats:?}");
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(out.files_copied, 1);
        assert_eq!(out.files_degraded, 1);
        assert_eq!(
            out.warnings.unavailable,
            Some(DegradedGroup { count: 1, example: PathBuf::from("a") })
        );
    }

    #[test]
    fn a_destination_without_the_no_replace_primitive_aborts_at_the_first_publish() {
        let fs = tree();
        fs.set_no_replace_support(false);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let e = r.unwrap_err();
        assert_eq!(e.code(), Code::NoReplacePublishUnavailable);
        assert_eq!(e.step, CopyStep::Publish);
        assert!(got.is_empty());
        assert!(!fs.exists("/dst/a.flux-partial.op1"), "the temporary was discarded");
        assert!(!fs.exists("/dst/sub"), "stopped at the first file, before `sub`");
    }

    #[test]
    fn a_create_failure_is_one_failure_not_an_abort_even_if_it_says_unsupported() {
        // Decision 3 is sound only at the PUBLISH step: `Unsupported` at `create_new`
        // is one file's failure. Without the step guard this would abort the tree.
        let fs = tree();
        fs.fail_kind("create_new", Code::IoError, std::io::ErrorKind::Unsupported);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        match &got[0].cause {
            TreeFailureCause::Copy(e) => assert_eq!(e.step, CopyStep::Create),
            other => panic!("expected Copy, got {other:?}"),
        }
        assert_eq!(out.files_copied, 1);
    }

    #[test]
    fn primitive_unavailable_is_unsupported_or_a_raw_einval_only() {
        assert!(primitive_unavailable(&std::io::Error::from(std::io::ErrorKind::Unsupported)));
        assert!(!primitive_unavailable(&std::io::Error::from(std::io::ErrorKind::InvalidInput)));
        assert!(!primitive_unavailable(&std::io::Error::from(std::io::ErrorKind::AlreadyExists)));
        #[cfg(unix)]
        assert!(primitive_unavailable(&std::io::Error::from_raw_os_error(22)), "EINVAL");
    }

    #[test]
    fn a_metadata_complaint_is_reported_with_the_file_published() {
        let fs = tree();
        fs.fail("set_times", Code::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        match &got[0].cause {
            TreeFailureCause::PublishedWithComplaints(v) => assert_eq!(v.len(), 1),
            other => panic!("expected PublishedWithComplaints, got {other:?}"),
        }
        assert!(fs.exists(Path::new("/dst").join(&got[0].path)), "the file is there");
        assert_eq!(out.files_copied, 2);
        assert_eq!(out.failures.published_with_complaints, 1);
    }

    #[test]
    fn a_symlink_is_a_failure_and_never_copied() {
        let fs = tree();
        fs.add_symlink("/src/link");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, Path::new("link"));
        assert!(matches!(got[0].cause, TreeFailureCause::Symlink));
        assert!(!fs.exists("/dst/link"));
        assert_eq!(out.failures.symlink, 1);
        assert_eq!(out.special_files_skipped, 0);
    }

    #[test]
    fn a_walk_error_is_reported_and_the_walk_continues() {
        let fs = tree();
        // read_dir calls: the walk's root listing (1), then `sub` (2).
        fs.fail_nth("read_dir", 2, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, Path::new("sub"));
        assert!(matches!(got[0].cause, TreeFailureCause::Walk(_)));
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"A"[..]));
        assert!(!fs.exists("/dst/sub"), "the walk emitted no Dir for it");
        assert_eq!(out.failures.walk, 1);
    }

    #[test]
    fn a_failed_directory_is_reported_once_and_its_descendants_never_copied() {
        let fs = tree();
        fs.create_dir(Path::new("/src/sub/deeper")).unwrap();
        fs.write_file("/src/sub/deeper/c", b"c");
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/sub", b"a file where a directory belongs");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1, "reported once: {got:?}");
        assert_eq!(got[0].path, Path::new("sub"));
        assert!(matches!(got[0].cause, TreeFailureCause::CreateDir(_)));
        assert!(
            !fs.calls().iter().any(|c| c.contains("flux-partial") && !c.contains("a.flux-partial")),
            "nothing under `sub` reached copy_file_at: {:?}",
            fs.calls()
        );
        assert_eq!(out.files_copied, 1);
    }

    #[test]
    fn a_leftover_temporary_is_reported_by_its_destination_relative_path() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.create_dir(Path::new("/src/sub")).unwrap();
        fs.write_file("/src/sub/b", b"BB");
        fs.fail("rename_no_replace", Code::PermissionDenied);
        fs.fail_always("remove_file", Code::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        r.unwrap();
        match &got[0].cause {
            TreeFailureCause::Copy(e) => {
                let (path, _) = e.leftover.as_ref().expect("the leftover is reported");
                assert_eq!(path, Path::new("sub/b.flux-partial.op1"));
            }
            other => panic!("expected Copy, got {other:?}"),
        }
    }

    #[test]
    fn a_destination_directory_that_is_the_source_root_aborts_the_operation() {
        // A bind mount inside the destination can present the source root under a
        // destination name; open_dir refuses only name-surrogates, so the merge must check
        // identity (cut 4b capstone round 1, owner).
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/sub")).unwrap();
        fs.set_identity("/dst/sub", identity_of(&fs, "/src"));

        let (r, _) = run(&fs, "/src", "/dst", &opts());

        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(!fs.exists("/dst/sub/b"), "nothing was written through the alias");
    }

    #[test]
    fn an_abort_before_anything_is_created_carries_an_empty_outcome() {
        // The lexical floor (a weak source identity keeps the pre-flight out of it).
        let fs = tree();
        fs.set_identity("/src", FileIdentity::Weak(ObjectId { volume: 5, index: 1 }));

        let (r, _) = run_full(&fs, "/src", "/src/backup", &opts());

        let a = r.unwrap_err();
        assert_eq!(a.error.code(), Code::SafetyRejected);
        assert_eq!((a.outcome.files_copied, a.outcome.directories_created), (0, 0));
        assert!(!a.changed());
    }

    #[test]
    fn an_abort_into_an_existing_root_with_nothing_published_is_unchanged() {
        // The exit-3 case of decision 3: the root pre-exists, the first publish aborts,
        // and its temporary was removed.
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_no_replace_support(false);

        let (r, _) = run_full(&fs, "/src", "/dst", &opts());

        let a = r.unwrap_err();
        assert_eq!(a.error.code(), Code::NoReplacePublishUnavailable);
        assert!(a.error.leftover.is_none());
        assert!(!a.changed());
    }

    #[test]
    fn an_abort_after_the_root_was_created_reports_a_change() {
        // `/dst` is absent, so step 5 creates it; the first publish then aborts.
        let fs = tree();
        fs.set_no_replace_support(false);

        let (r, _) = run_full(&fs, "/src", "/dst", &opts());

        let a = r.unwrap_err();
        assert_eq!(a.error.code(), Code::NoReplacePublishUnavailable);
        assert_eq!((a.outcome.directories_created, a.outcome.files_copied), (1, 0));
        assert!(a.changed(), "the root this operation created is a change");
    }

    #[test]
    fn an_abort_mid_walk_carries_what_was_copied_before_it() {
        // `a` sorts before `sub`: it is copied, then `sub` (the destination by identity)
        // aborts the operation.
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_identity("/src/sub", identity_of(&fs, "/dst"));

        let (r, _) = run_full(&fs, "/src", "/dst", &opts());

        let a = r.unwrap_err();
        assert_eq!(a.error.code(), Code::SafetyRejected);
        assert_eq!(
            (a.outcome.files_copied, a.outcome.bytes_copied, a.outcome.directories_created),
            (1, 1, 0)
        );
        assert!(a.changed());
    }

    #[test]
    fn an_abort_that_leaves_a_temporary_reports_a_change() {
        // Nothing created, nothing published - but the aborted publish's temporary could
        // not be removed, so the destination differs (§55: exit 1, not 3).
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_no_replace_support(false);
        fs.fail_always("remove_file", Code::PermissionDenied);

        let (r, _) = run_full(&fs, "/src", "/dst", &opts());

        let a = r.unwrap_err();
        assert_eq!((a.outcome.directories_created, a.outcome.files_copied), (0, 0));
        assert!(a.error.leftover.is_some(), "precondition: the temporary stayed");
        assert!(a.changed());
    }

    #[test]
    fn files_total_counts_every_non_directory_entry_even_under_a_skipped_subtree() {
        let fs = tree(); // /src/a, /src/sub/b
        fs.add_symlink("/src/link");
        fs.add_special("/src/sub/dev");
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/sub", b"a file where a directory belongs");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        // `a` and `link`, and - under the skipped `sub` - `b` and `dev`.
        assert_eq!(out.files_total, 4);
        assert_eq!(out.files_copied, 1);
        assert_eq!(out.special_files_skipped, 0, "nothing under a skipped subtree is reported");
        assert_eq!(got.len(), 2, "the symlink, and `sub` once: {got:?}");
    }

    #[test]
    fn a_reserved_control_path_is_matched_case_insensitively_and_nothing_else_is() {
        for p in [".flux/operations", ".flux/standalone/x", ".FLUX/Atomic", ".Flux/OPERATIONS/a/b"]
        {
            assert!(reserved_path(Path::new(p)), "{p}");
        }
        for p in [
            ".flux",
            ".flux/other",
            ".flux/operationsx",
            "x/.flux/operations",
            "flux/operations",
            "a",
        ] {
            assert!(!reserved_path(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn a_source_entry_landing_in_a_reserved_control_path_fails_and_the_rest_is_copied() {
        let fs = FaultFs::new();
        for d in ["/src", "/src/.flux", "/src/.flux/operations", "/src/.flux/other", "/src/.FLUX"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.create_dir(Path::new("/src/.FLUX/Standalone")).unwrap();
        fs.write_file("/src/.flux/operations/x", b"x");
        fs.write_file("/src/.flux/atomic", b"y");
        fs.write_file("/src/.flux/other/z", b"z");
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();
        let mut conflicts: Vec<String> = got
            .iter()
            .filter(|f| match &f.cause {
                TreeFailureCause::CreateDir(e) => e.code == Code::ControlPlaneNamespaceConflict,
                TreeFailureCause::Copy(e) => e.code() == Code::ControlPlaneNamespaceConflict,
                _ => false,
            })
            .map(|f| f.path.to_string_lossy().replace('\\', "/"))
            .collect();
        conflicts.sort();
        assert_eq!(conflicts, [".FLUX/Standalone", ".flux/atomic", ".flux/operations"]);
        assert_eq!(
            fs.read_file("/dst/.flux/other/z").as_deref(),
            Some(&b"z"[..]),
            "the rest of .flux is data"
        );
        assert!(!fs.exists("/dst/.flux/operations") && !fs.exists("/dst/.flux/atomic"));
        assert!(!fs.exists("/dst/.FLUX/Standalone"));
        assert_eq!(out.failures.total(), 3);
    }

    /// `copy_tree_at` into a fresh `/dst`, with `guard`.
    fn guarded(
        fs: &FaultFs,
        guard: &Guard<'_>,
    ) -> (std::result::Result<(), CopyError>, TreeOutcome) {
        let source = prepare_source(fs, Path::new("/src"), Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let o = opts();
        let cx = Shared {
            fs,
            src_root: Path::new("/src"),
            src_identity: source.identity,
            opts: &o,
            guard,
            beat: &no_heartbeat,
        };
        let mut out = TreeOutcome::default();
        let (_root, r) = copy_tree_at(&cx, source.events, root, &mut out, &mut |_| {});
        (r, out)
    }

    fn failing_after(
        owned: u32,
        calls: &std::cell::Cell<u32>,
    ) -> impl Fn() -> flux_fs::Result<()> + '_ {
        move || {
            calls.set(calls.get() + 1);
            if calls.get() <= owned {
                Ok(())
            } else {
                Err(FsError::new(Code::TargetLockBusy, std::io::Error::other("lost")))
            }
        }
    }

    #[test]
    fn a_failed_guard_aborts_the_whole_tree_with_target_lock_busy() {
        let fs = tree();
        let calls = std::cell::Cell::new(0);
        // `a` first: its sweep (1) passes, its create (2) fails.
        let (r, out) = guarded(&fs, &failing_after(1, &calls));
        assert_eq!(r.unwrap_err().code(), Code::TargetLockBusy);
        assert_eq!(
            (out.files_copied, out.failures.total()),
            (0, 0),
            "an abort, never a per-file failure"
        );
        assert!(!fs.exists("/dst/a") && !fs.exists("/dst/sub"));
    }

    #[test]
    fn the_guard_runs_before_each_directory_is_created() {
        let fs = tree();
        let calls = std::cell::Cell::new(0);
        // `a`: sweep, create, publish (1-3); then `sub`'s creation (4) fails.
        let (r, out) = guarded(&fs, &failing_after(3, &calls));
        assert_eq!(r.unwrap_err().code(), Code::TargetLockBusy);
        assert_eq!(calls.get(), 4);
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"A"[..]));
        assert!(!fs.exists("/dst/sub"));
        assert_eq!(out.directories_created, 0);
    }

    #[test]
    fn a_failed_heartbeat_aborts_the_whole_tree() {
        let fs = tree();
        let source = prepare_source(&fs, Path::new("/src"), Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let o = opts();
        let calls = std::cell::Cell::new(0);
        // `a`'s sweep, create, one chunk and publish pass (1-4); `sub`'s creation's (5) fails.
        let beat = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 5 {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        let cx = Shared {
            fs: &fs,
            src_root: Path::new("/src"),
            src_identity: source.identity,
            opts: &o,
            guard: &unguarded,
            beat: &beat,
        };
        let mut out = TreeOutcome::default();
        let mut reported = 0;
        let (_root, r) = copy_tree_at(&cx, source.events, root, &mut out, &mut |_| reported += 1);
        assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
        assert_eq!(calls.get(), 5, "no heartbeat after the failed one");
        assert_eq!((reported, out.failures.total()), (0, 0), "an abort, never a per-file failure");
        assert!(fs.exists("/dst/a") && !fs.exists("/dst/sub"));
    }

    #[test]
    fn a_heartbeat_failure_inside_a_file_aborts_the_whole_tree() {
        let fs = tree();
        let source = prepare_source(&fs, Path::new("/src"), Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let o = opts();
        let calls = std::cell::Cell::new(0);
        // `a`'s sweep (1) and create (2) pass; its chunk's (3) fails.
        let beat = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 3 {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        let cx = Shared {
            fs: &fs,
            src_root: Path::new("/src"),
            src_identity: source.identity,
            opts: &o,
            guard: &unguarded,
            beat: &beat,
        };
        let mut out = TreeOutcome::default();
        let mut reported = 0;
        let (_root, r) = copy_tree_at(&cx, source.events, root, &mut out, &mut |_| reported += 1);
        assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
        assert_eq!((reported, out.files_copied), (0, 0), "the walk stops at the failed file");
        assert!(!fs.exists("/dst/a") && !fs.exists("/dst/sub"));
    }

    #[test]
    fn copy_tree_at_hands_the_root_back() {
        let fs = tree();
        let (r, _) = guarded(&fs, &unguarded);
        r.unwrap();
        let source = prepare_source(&fs, Path::new("/src"), Path::new("/dst")).unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let o = opts();
        let cx = Shared {
            fs: &fs,
            src_root: Path::new("/src"),
            src_identity: source.identity,
            opts: &o,
            guard: &unguarded,
            beat: &no_heartbeat,
        };
        let mut out = TreeOutcome::default();
        let (back, r) = copy_tree_at(&cx, source.events, root, &mut out, &mut |_| {});
        assert!(r.is_ok());
        assert_eq!(back.identity().unwrap(), identity_of(&fs, "/dst"), "the same directory");
    }

    #[test]
    fn a_refused_unchanged_abort_is_exactly_the_exit_3_rule() {
        let abort = |code, f: fn(&mut TreeOutcome)| {
            let mut outcome = TreeOutcome::default();
            f(&mut outcome);
            TreeAbort {
                error: CopyError::at(
                    CopyStep::Resolve,
                    FsError::new(code, std::io::Error::other("x")),
                ),
                outcome,
            }
        };
        assert!(abort(Code::SafetyRejected, |_| {}).refused_unchanged());
        assert!(abort(Code::NoReplacePublishUnavailable, |_| {}).refused_unchanged());
        assert!(!abort(Code::IoError, |_| {}).refused_unchanged());
        assert!(!abort(Code::TargetLockBusy, |_| {}).refused_unchanged());
        assert!(!abort(Code::SafetyRejected, |o| o.files_copied = 1).refused_unchanged());
        assert!(!abort(Code::SafetyRejected, |o| o.failures.walk = 1).refused_unchanged());
    }
}
