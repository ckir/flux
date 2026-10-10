//! Recursive copy (cut 4b): `copy_tree` drives the ordered walk and `copy_file_at`,
//! writing only through destination directory handles (§149.7).
//!
//! Design authority: `docs/superpowers/specs/2026-09-26-cut-4b-copy-tree-design.md`.

use crate::copy::{
    BeforeCreate, BeforePublish, CopyError, CopyStep, Guard, Heartbeat, PublishIntent, Stage,
    Staged, copy_file_guarded, discard, no_before_create, no_before_publish, no_heartbeat,
    publish_staged, split_destination, stage_file, unguarded, weaker,
};
use crate::names::{NameIndex, Resolved};
use crate::state::{FLUX_DIR, RESERVED_DIRS, identity_text, native_hex};
use crate::walk::{Walk, WalkEvent, walk};
use flux_fs::{
    ClaimKey, ClaimOutcome, ClaimRecord, ClaimStatus, ClaimStore, Code, CopyOptions,
    DestinationRoot, DirHandle, Durability, ExistingPolicy, FileIdentity, FileSystem, FileType,
    FluxPathKey, FsError, Metadata, MetadataFailure, MountRoot, ObjectId, PreparedRecord, Preserve,
    Publish, RecoveryOp, Safety,
};
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
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
    /// Part M: pre-existing destination directories whose mount-root query could not tell, merged into under
    /// `Safety::Default`. The example is relative to the source root, as `warnings` are.
    pub mount_unknown: Option<DegradedGroup>,
    /// Part A: the canonical-path query was unsupported, so containment was checked lexically only. The path
    /// whose query failed, as the operator gave it (DEST, its parent, or the source root).
    pub containment_degraded: Option<PathBuf>,
    /// Cut 8b: existing files replaced (a claimed name already present at the destination).
    pub files_overwritten: u64,
    /// Cut 8b: files skipped (not copied, not failures).
    pub files_skipped: u64,
    /// Cut 8b: the bytes of the files counted in `files_skipped`.
    pub bytes_skipped: u64,
    /// Cut 8b: directories where claims could not be keyed, so existing files there are reported as collisions.
    /// The example is relative to the source root.
    pub replace_degraded: Option<DegradedGroup>,
    /// Cut 9a: files the prior run completed, verified and skipped. Not in `files_skipped`; the CLI adds them.
    pub files_resumed: u64,
    /// Cut 9a: the source bytes of the files in `files_resumed`.
    pub bytes_resumed: u64,
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
    /// The file IS published and counted; its claim was not recorded (for a directory flush failure the
    /// path is the directory). Counted in `FailureTally::total()` and `claim_not_recorded` but NOT in the report's
    /// `files_failed`, because the file was published and counted as copied.
    ClaimNotRecorded(FsError),
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
    pub claim_not_recorded: u64,
}

impl FailureTally {
    /// The only total. Derived, never stored beside the parts.
    pub fn total(&self) -> u64 {
        self.walk
            + self.create_dir
            + self.copy
            + self.symlink
            + self.published_with_complaints
            + self.claim_not_recorded
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
            TreeFailureCause::ClaimNotRecorded(_) => self.claim_not_recorded += 1,
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

impl DegradedGroup {
    /// Count one more degraded check into `slot`, keeping the first example.
    pub(crate) fn note(slot: &mut Option<DegradedGroup>, example: &Path) {
        slot.get_or_insert_with(|| DegradedGroup { count: 0, example: example.to_path_buf() })
            .count += 1;
    }
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
// `Skipped` is a marker on a stack that holds one frame per directory DEPTH; the size of `Live` costs nothing there.
#[allow(clippy::large_enum_variant)]
enum Frame<D> {
    Live {
        dir: D,
        /// `Strong` identities of the directories this operation created INSIDE `dir`.
        /// A fold is two source names in ONE source directory landing on one
        /// destination name, so both creations happen here; the set is dropped with the
        /// frame, which bounds it like the walk's one-directory listing (spec line 997,
        /// invariant 11).
        created: HashSet<ObjectId>,
        /// Cut 8b: this directory's names (its listing, the identity table, who wrote which spelling), dropped with the
        /// frame. A directory this run created has an empty listing.
        names: NameIndex,
        /// Cut 8b: the directory's `Strong` identity when the run has a claim store, the key every claim under it is
        /// made with; `None` for a copy without claims and for a directory whose identity is not `Strong`
        /// (`replace_degraded`), where every target is planned as new and an existing file is a collision.
        claim_parent: Option<ObjectId>,
        /// Cut 8b: this run CREATED the directory, so it is empty of anything but this run's own entries and every
        /// target in it is planned as new (no name resolution), published with the no-replace primitive. A fold between
        /// two source names is then refused by that rename (`AlreadyExists`, a collision), whether or not the first
        /// target's claim was recorded.
        fresh: bool,
        /// Cut 9d: the files staged in `dir` and not yet published, flushed through `dir` before it is dropped.
        batch: Batch,
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
    /// The run's claim store, open for the walk (cut 8b); `None` for a copy that holds no lock.
    pub(crate) claims: Option<&'c std::cell::RefCell<<F::Dir as DirHandle>::Claims>>,
    /// Cut 9a: `--resume`: an own `Created` claim may skip its target.
    pub(crate) resume: bool,
    /// Cut 9d: when a Strict tree's per-directory batch flushes.
    pub(crate) batch: BatchPolicy,
}

/// Cut 9d (spec decision 5): a batch flushes at this many staged files...
// Referenced by `DEFAULT_FILES` once Task 5 turns batching on; until then only the tests read it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const BATCH_FILES: usize = 64;
/// ...or this many staged source bytes (section 148.3's threshold); a file at least this long is staged alone...
pub(crate) const BATCH_BYTES: u64 = 64 << 20;
/// ...or when, at the next staging, the batch is at least this old.
pub(crate) const BATCH_AGE: std::time::Duration = std::time::Duration::from_secs(1);
/// `BatchPolicy::DEFAULT.files`. 1 is the unbatched cut 9c path (plan ruling 1). Task 5 sets this to BATCH_FILES.
const DEFAULT_FILES: usize = 1;

/// Cut 9d: the flush thresholds of a Strict tree's per-directory batch, and the clock the age is read from. Carried in
/// `Shared` so tests inject small thresholds and a fake clock (plan ruling 4). `files == 1` never batches.
#[derive(Clone, Copy)]
pub(crate) struct BatchPolicy {
    pub files: usize,
    pub bytes: u64,
    pub age: std::time::Duration,
    pub now: fn() -> std::time::Instant,
    /// Decision 6's ASCII fold pre-check; a test turns it off so the fake's ASCII fold reaches the temporary probe.
    pub fold_precheck: bool,
}

impl BatchPolicy {
    pub(crate) const DEFAULT: BatchPolicy = BatchPolicy {
        files: DEFAULT_FILES,
        bytes: BATCH_BYTES,
        age: BATCH_AGE,
        now: std::time::Instant::now,
        fold_precheck: true,
    };
}

/// Cut 9d: one staged file waiting in its directory's batch for the flush that notes and publishes it.
struct Pending {
    /// Relative to the source root, as the walk reports it.
    path: PathBuf,
    /// The planned name, the rename's destination.
    name: OsString,
    /// The note's and the claim's key: the planned name for a new file, the stored name for a replacement.
    key: ClaimKey,
    /// A case-variant replacement's second claim key, on the planned name (as `commit_prepared`'s `planned`).
    planned: Option<ClaimKey>,
    /// The note without the temporary's name and identity (`write_note`'s template).
    template: PreparedRecord,
    staged: Staged,
    target: FluxPathKey,
    /// `Plan::Replace`'s `stored` and `meta`; `None` for a new file.
    replaced: Option<(OsString, Metadata)>,
}

/// Cut 9d: a directory's staged files (spec decision 3), the staged source bytes, when the first was staged, and the
/// ASCII-folded names of the pending entries (decision 6).
#[derive(Default)]
struct Batch {
    pending: Vec<Pending>,
    bytes: u64,
    since: Option<std::time::Instant>,
    folded: HashSet<Vec<u8>>,
}

/// Decision 6's fold: ASCII lowercase of the name's encoded bytes, the fold `reserved_path` compares with.
fn folded(name: &std::ffi::OsStr) -> Vec<u8> {
    name.as_encoded_bytes().to_ascii_lowercase()
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
            containment(fs, src_root, &root, dst_root, opts.safety, out)?;
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
            containment(fs, src_root, &parent, parent_path, opts.safety, out)?;
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
        claims: None,
        resume: false,
        batch: BatchPolicy::DEFAULT,
    };
    copy_tree_at(&cx, source.events, root, false, out, on_report).1
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
    root_created: bool,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> (F::Dir, std::result::Result<(), CopyError>) {
    let root_identity = match root.identity() {
        Ok(i) => i,
        Err(e) => return (root, Err(CopyError::at(CopyStep::Resolve, e))),
    };
    let claim_parent = match (cx.claims, root_identity) {
        (None, _) => None,
        (Some(_), FileIdentity::Strong(id)) => Some(id),
        (Some(_), _) => match cx.opts.safety {
            Safety::Default => {
                DegradedGroup::note(&mut out.replace_degraded, Path::new(""));
                None
            }
            Safety::Strict => {
                return (
                    root,
                    Err(refuse(
                        "the destination's identity is not strong enough to key claims, which --safety strict refuses",
                    )),
                );
            }
        },
    };
    // A pre-existing root is listed once; a root this run made is `fresh` and needs no listing.
    let names = if claim_parent.is_some() && !root_created {
        match NameIndex::for_existing(&root) {
            Ok(names) => names,
            Err(e) => return (root, Err(CopyError::at(CopyStep::Resolve, e))),
        }
    } else {
        NameIndex::for_new_dir()
    };
    let mut stack = vec![Frame::Live {
        dir: root,
        created: HashSet::new(),
        names,
        claim_parent,
        fresh: root_created,
        batch: Batch::default(),
    }];
    let walked = walk_into(cx, events, root_identity, &mut stack, out, on_report);
    // Cut 9d, spec decision 5(e): every live frame's batch is flushed whatever the walk returned; the walk's own error
    // wins over a stop of the drain.
    let drained = drain_batches(cx, &mut stack, out, on_report, walked.is_err());
    let walked = walked.and(drained);
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
    // 6. The walk. Every target is published by `copy_one`, which picks the publish primitive per target: no-replace
    //    for a new name and for any directory that cannot key claims (§241.5), replace only under a claim (cut 8b).
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
                    // Cut 9d (decision 6, ASCII fold): a pending file whose name this directory's folds onto publishes
                    // first, so the directory meets it as an unbatched copy's would, instead of winning the name.
                    if let Some(Frame::Live { dir, names, batch, .. }) = stack.last_mut()
                        && !batch.pending.is_empty()
                        && let Some(name) = path.file_name()
                        && batch.folded.contains(&folded(name))
                    {
                        flush_batch(cx, dir, names, batch, out, on_report)?;
                    }
                    // §101, then §99, before the directory's creation.
                    (cx.beat)().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
                    (cx.guard)().map_err(|e| CopyError::at(CopyStep::Create, e))?;
                    enter_dir(
                        stack,
                        &path,
                        identity,
                        root_identity,
                        cx.src_identity,
                        cx.opts,
                        cx.claims.is_some(),
                        out,
                        on_report,
                    )?
                };
                stack.push(frame);
            }
            WalkEvent::File { path } => {
                out.files_total += 1;
                if let Some(Frame::Live { dir, names, claim_parent, fresh, batch, .. }) =
                    stack.last_mut()
                {
                    if reserved_path(&path) {
                        let conflict = CopyError::at(CopyStep::Gate, reserved_conflict());
                        report(out, on_report, path, TreeFailureCause::Copy(conflict));
                    } else {
                        copy_one(
                            cx,
                            dir,
                            names,
                            batch,
                            *claim_parent,
                            *fresh,
                            path,
                            out,
                            on_report,
                        )?;
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
            WalkEvent::DirEnd { path } => {
                // Cut 8b, "Claim syncing": a directory that keyed claims is flushed at its end. A synced commit is a
                // write under DEST, so §101 and §99 come first, as before a directory's creation. A skipped
                // directory flushed nothing. Cut 9d, spec decision 5(c): the popped frame's batch is published first,
                // through the frame's own handle, which drops only after both.
                if let Some(Frame::Live { dir, mut names, mut batch, claim_parent, .. }) =
                    stack.pop()
                {
                    flush_batch(cx, &dir, &mut names, &mut batch, out, on_report)?;
                    if claim_parent.is_some()
                        && let Some(claims) = cx.claims
                    {
                        (cx.beat)().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
                        (cx.guard)().map_err(|e| CopyError::at(CopyStep::Create, e))?;
                        // Held in a local so the `RefMut` is dropped before the report callback runs.
                        let r = claims.borrow_mut().flush();
                        if let Err(e) = r {
                            report(out, on_report, path, TreeFailureCause::ClaimNotRecorded(e));
                        }
                    }
                }
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
// The signature is the plan's: nine inputs, each a distinct piece of walk state.
#[allow(clippy::too_many_arguments)]
fn enter_dir<D: DirHandle>(
    stack: &mut [Frame<D>],
    path: &Path,
    identity: FileIdentity,
    root_identity: FileIdentity,
    src_identity: FileIdentity,
    opts: &CopyOptions,
    claims_on: bool,
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

    let Some(Frame::Live { dir: parent, created, .. }) = stack.last_mut() else {
        unreachable!("enter_dir is called only under a live frame");
    };
    let name = path.file_name().expect("a walk path ends in a name");
    // Decision 8: create first; open on AlreadyExists. Neither call traverses a link.
    let (child, child_identity, pre_existing) = match parent.create_dir(name) {
        Ok(child) => {
            out.directories_created += 1;
            let child_identity = child.identity();
            if let Ok(FileIdentity::Strong(id)) = child_identity {
                created.insert(id);
            }
            (child, child_identity, false)
        }
        Err(e) if e.source.kind() == ErrorKind::AlreadyExists => match parent.open_dir(name) {
            Ok(child) => {
                let child_identity = child.identity();
                match child_identity {
                    // Created by THIS operation moments ago, under another source name.
                    Ok(FileIdentity::Strong(id)) if created.contains(&id) => {
                        let failure = FsError::new(
                            Code::DestinationNamespaceCollision,
                            std::io::Error::other(
                                "this operation already created that directory under another source name",
                            ),
                        );
                        report(
                            out,
                            on_report,
                            path.to_path_buf(),
                            TreeFailureCause::CreateDir(failure),
                        );
                        return Ok(Frame::Skipped);
                    }
                    // Pre-existing: merge into it.
                    _ => (child, child_identity, true),
                }
            }
            // SAFETY_REJECTED (a link) or DESTINATION_ERROR (a file, or it vanished).
            Err(e) => {
                report(out, on_report, path.to_path_buf(), TreeFailureCause::CreateDir(e));
                return Ok(Frame::Skipped);
            }
        },
        Err(e) => {
            report(out, on_report, path.to_path_buf(), TreeFailureCause::CreateDir(e));
            return Ok(Frame::Skipped);
        }
    };

    // A destination directory about to be entered must not BE the source root (owner, cut 4b capstone round 1):
    // a bind mount inside the destination can present the source under a destination name, and `open_dir`
    // refuses only name-surrogates. Aliases of source SUBdirectories stay the section 42 mount cut's. Before the
    // mount query, so a mount presenting the source root aborts rather than becoming a skipped subtree.
    if let (Some(FileIdentity::Strong(a)), FileIdentity::Strong(b)) =
        (child_identity.as_ref().ok().copied(), src_identity)
        && a == b
    {
        return Err(refuse("a destination directory is the source root itself, by identity"));
    }

    // Part M: a pre-existing destination directory that is the root of a mount is never merged into.
    if pre_existing {
        let refusal = match child.mount_root(&*parent) {
            Ok(MountRoot::No) => None,
            Ok(MountRoot::Yes) => Some(FsError::new(
                Code::SafetyRejected,
                std::io::Error::other(
                    "a pre-existing destination directory is the root of a mount, which a copy never merges into",
                ),
            )),
            Ok(MountRoot::Unknown) => match opts.safety {
                Safety::Default => {
                    DegradedGroup::note(&mut out.mount_unknown, path);
                    None
                }
                Safety::Strict => Some(FsError::new(
                    Code::SafetyRejected,
                    std::io::Error::other(
                        "whether a pre-existing destination directory is a mount root cannot be told with full confidence",
                    ),
                )),
            },
            Err(e) => Some(e),
        };
        if let Some(failure) = refusal {
            report(out, on_report, path.to_path_buf(), TreeFailureCause::CreateDir(failure));
            return Ok(Frame::Skipped);
        }
    }

    // Cut 8b: a directory that cannot key claims gets none, and its targets are planned as new (no replacement).
    let claim_parent = match (claims_on, child_identity) {
        (false, _) => None,
        (true, Ok(FileIdentity::Strong(id))) => Some(id),
        (true, _) => match opts.safety {
            Safety::Default => {
                DegradedGroup::note(&mut out.replace_degraded, path);
                None
            }
            Safety::Strict => {
                let failure = FsError::new(
                    Code::SafetyRejected,
                    std::io::Error::other(
                        "a destination directory whose identity is not strong enough to key claims is never merged into under --safety strict",
                    ),
                );
                report(out, on_report, path.to_path_buf(), TreeFailureCause::CreateDir(failure));
                return Ok(Frame::Skipped);
            }
        },
    };
    // A directory this run created is empty; a pre-existing one is listed once, here.
    let names = if pre_existing && claim_parent.is_some() {
        match NameIndex::for_existing(&child) {
            Ok(names) => names,
            Err(e) => {
                report(out, on_report, path.to_path_buf(), TreeFailureCause::CreateDir(e));
                return Ok(Frame::Skipped);
            }
        }
    } else {
        NameIndex::for_new_dir()
    };
    Ok(Frame::Live {
        dir: child,
        created: HashSet::new(),
        names,
        claim_parent,
        fresh: !pre_existing,
        batch: Batch::default(),
    })
}

/// What the tree decided for one file target.
enum Plan {
    /// Not at the destination (or nothing to resolve): publish with no replacement.
    New,
    /// An existing file or link stored as `stored` (metadata `meta`) is replaced, under a claim on `stored`.
    Replace { stored: OsString, meta: Metadata },
}

/// `Update`'s rule, the same as the copy path's: a newer source or a different size; with either modification time
/// unavailable, the lengths alone decide.
fn update_replaces(src: &Metadata, dest: &Metadata) -> bool {
    src.len != dest.len || matches!((src.modified, dest.modified), (Some(s), Some(d)) if s > d)
}

/// Cut 9a, decision 2: the destination entry is a regular file of the source's length and, with times preserved
/// (`preserve_times != Preserve::Off`), both modification times are known and differ by at most 2 seconds.
/// With `Preserve::Off`, or either time unknown, never true.
fn resumed_complete(src: &Metadata, dest: &Metadata, preserve_times: Preserve) -> bool {
    if dest.file_type != FileType::File || src.len != dest.len || preserve_times == Preserve::Off {
        return false;
    }
    match (src.modified, dest.modified) {
        (Some(s), Some(d)) => {
            let gap =
                s.duration_since(d).or_else(|e| Ok::<_, ()>(e.duration())).unwrap_or_default();
            gap <= std::time::Duration::from_secs(2)
        }
        _ => false,
    }
}

fn target_failure(step: CopyStep, code: Code, why: &'static str) -> TreeFailureCause {
    TreeFailureCause::Copy(CopyError::at(step, FsError::new(code, std::io::Error::other(why))))
}

/// A `File` event under a live frame. See the design, "The per-file protocol".
///
/// The tree decides what an existing destination means and tells the copy path `ExistingPolicy::Overwrite`, so the
/// copy path never skips on its own and never produces `Outcome.skipped` here; the tree counts skips itself.
// One input per piece of frame state the protocol reads.
#[allow(clippy::too_many_arguments)]
fn copy_one<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    names: &mut NameIndex,
    batch: &mut Batch,
    claim_parent: Option<ObjectId>,
    fresh: bool,
    path: PathBuf,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    let name_owned = path.file_name().expect("a walk path ends in a name").to_os_string();
    let name = name_owned.as_os_str();
    let src = cx.src_root.join(&path);
    let (Some(parent_id), Some(claims)) = (claim_parent, cx.claims) else {
        // No claims for this directory (a lockless copy, or an identity that cannot key them): exactly the cut 4b
        // behaviour. An existing name is a collision.
        let opts = CopyOptions {
            existing: ExistingPolicy::Overwrite,
            publish: Publish::NoReplace,
            ..cx.opts.clone()
        };
        let published = copy_file_guarded(
            cx.fs,
            &src,
            parent,
            name,
            &opts,
            cx.guard,
            cx.beat,
            &no_before_create,
            &no_before_publish,
        );
        return finish_copy(path, published, out, on_report);
    };
    let target = match FluxPathKey::from_relative(&path) {
        Ok(t) => t,
        Err(e) => {
            report(
                out,
                on_report,
                path,
                TreeFailureCause::Copy(CopyError::at(CopyStep::Resolve, e)),
            );
            return Ok(());
        }
    };

    // Cut 9d, spec decision 2: a Strict publication into a store with notes is staged into the directory's batch, unless
    // the policy turns batching off (plan ruling 1: `files == 1` is the per-file cut 9c path below).
    let batching = cx.batch.files > 1
        && cx.opts.durability == Durability::Strict
        && claims.borrow().supports_prepared();
    // Decisions 5(d) and 5(f): a batch older than the age, or a pending name this one folds onto, publishes first, so
    // the resolution below sees what an unbatched copy would.
    if batching && !batch.pending.is_empty() {
        let aged = batch
            .since
            .is_some_and(|t| (cx.batch.now)().saturating_duration_since(t) >= cx.batch.age);
        if aged || (cx.batch.fold_precheck && batch.folded.contains(&folded(name))) {
            flush_batch(cx, parent, names, batch, out, on_report)?;
        }
    }

    // 1-2. Resolve the planned name to the entry it denotes.
    // A directory this run created plans every target as new (plan decision 6).
    let resolved = if fresh { Ok(Ok(Resolved::Absent)) } else { names.resolve(parent, name) };
    let plan = match resolved {
        Err(e) => {
            report(
                out,
                on_report,
                path,
                TreeFailureCause::Copy(CopyError::at(CopyStep::Resolve, e)),
            );
            return Ok(());
        }
        Ok(Err(_)) => {
            let cause = target_failure(
                CopyStep::Resolve,
                Code::DestinationError,
                "cannot determine the stored name of the destination entry",
            );
            report(out, on_report, path, cause);
            return Ok(());
        }
        Ok(Ok(Resolved::Absent)) => Plan::New,
        // 4. An existing entry.
        Ok(Ok(Resolved::Entry { stored, meta })) => {
            // §241.5: an entry another target of this run already published (even if its claim was not recorded)
            // is that target's. Refused before any policy decision, without consulting or writing the store.
            if names.owner(&stored).is_some_and(|t| t != &target) {
                let e = CopyError::at(
                    CopyStep::Claim,
                    FsError::new(
                        Code::DestinationNamespaceCollision,
                        std::io::Error::other(
                            "another target of this operation already wrote that destination entry",
                        ),
                    ),
                );
                return finish_copy(path, Err(e), out, on_report);
            }
            // Cut 9a: `--resume`. The branch mutates nothing at the destination, so it neither beats nor guards.
            let mut src_known: Option<Metadata> = None;
            if cx.resume {
                let src_meta = match cx.fs.metadata(&src) {
                    Ok(m) => m,
                    Err(e) => {
                        let cause = TreeFailureCause::Copy(CopyError::at(CopyStep::Source, e));
                        report(out, on_report, path, cause);
                        return Ok(());
                    }
                };
                let found = claims.borrow().get(&ClaimKey::new(parent_id, &stored));
                match found {
                    Err(e) => {
                        return finish_copy(
                            path,
                            Err(CopyError::at(CopyStep::Claim, e)),
                            out,
                            on_report,
                        );
                    }
                    Ok(Some(r)) if r.target == target => {
                        if r.status == ClaimStatus::Created
                            && resumed_complete(&src_meta, &meta, cx.opts.preserve_times)
                        {
                            out.files_resumed += 1;
                            out.bytes_resumed += src_meta.len;
                            names.record_publication(
                                &target,
                                Some(&stored),
                                name,
                                None,
                                meta.identity,
                            );
                            return Ok(());
                        }
                    }
                    Ok(Some(_)) => {
                        let e = CopyError::at(
                            CopyStep::Claim,
                            FsError::new(
                                Code::DestinationNamespaceCollision,
                                std::io::Error::other(
                                    "another target of this operation already claimed that destination entry",
                                ),
                            ),
                        );
                        return finish_copy(path, Err(e), out, on_report);
                    }
                    Ok(None) => {}
                }
                src_known = Some(src_meta);
            }
            if meta.file_type == FileType::Dir {
                let cause = target_failure(
                    CopyStep::Gate,
                    Code::DestinationError,
                    "the destination is a directory",
                );
                report(out, on_report, path, cause);
                return Ok(());
            }
            let skip = match cx.opts.existing {
                ExistingPolicy::Overwrite => None,
                policy => {
                    let src_meta = match src_known {
                        Some(m) => m,
                        None => match cx.fs.metadata(&src) {
                            Ok(m) => m,
                            Err(e) => {
                                let cause =
                                    TreeFailureCause::Copy(CopyError::at(CopyStep::Source, e));
                                report(out, on_report, path, cause);
                                return Ok(());
                            }
                        },
                    };
                    let replace =
                        policy == ExistingPolicy::Update && update_replaces(&src_meta, &meta);
                    (!replace).then_some(src_meta.len)
                }
            };
            if let Some(len) = skip {
                out.files_skipped += 1;
                out.bytes_skipped += len;
                return Ok(());
            }
            Plan::Replace { stored, meta }
        }
    };

    let own = |status| ClaimRecord { target: target.clone(), status };
    // Cut 9c: a Strict publication into a store with the `prepared` table writes a note before its rename (the hook),
    // commits it with the claim after it, and discards it when the copy fails after the note. Otherwise: cut 9a.
    let strict = cx.opts.durability == Durability::Strict && claims.borrow().supports_prepared();
    let wrote = std::cell::Cell::new(false);
    match plan {
        // 3. Plan as new.
        Plan::New => {
            let key = ClaimKey::new(parent_id, name);
            let template = PreparedRecord {
                target: target.clone(),
                temp_name: Vec::new(),
                identity: String::new(),
                dir_path: note_dir(&path),
                name: name.as_encoded_bytes().to_vec(),
                planned_name: Vec::new(),
                replacement: false,
            };
            if batching {
                let pending = |path, staged| Pending {
                    path,
                    name: name_owned.clone(),
                    key,
                    planned: None,
                    template,
                    staged,
                    target,
                    replaced: None,
                };
                let publish = Publish::NoReplace;
                let no_claim: &BeforeCreate<'_> = &no_before_create;
                return stage_into_batch(
                    cx, parent, names, batch, &src, path, publish, no_claim, out, on_report,
                    pending,
                );
            }
            let hook = |intent: &PublishIntent| write_note(claims, &key, &template, &wrote, intent);
            let before_publish: &BeforePublish<'_> =
                if strict { &hook } else { &no_before_publish };
            let opts = CopyOptions {
                existing: ExistingPolicy::Overwrite,
                publish: Publish::NoReplace,
                ..cx.opts.clone()
            };
            let result = copy_file_guarded(
                cx.fs,
                &src,
                parent,
                name,
                &opts,
                cx.guard,
                cx.beat,
                &no_before_create,
                before_publish,
            );
            discard_note_on_failure(claims, &key, &wrote, &result);
            let published = match &result {
                Ok(o) => Some(o.published_identity),
                Err(_) => None,
            };
            finish_copy(path.clone(), result, out, on_report)?;
            if let Some(identity) = published {
                let recorded = if strict {
                    claims.borrow_mut().commit_prepared(&key, &target, None)
                } else {
                    record_claim(claims, &key, &own(ClaimStatus::Created))
                };
                if let Err(e) = recorded {
                    report(out, on_report, path, TreeFailureCause::ClaimNotRecorded(e));
                }
                names.record_publication(&target, None, name, None, identity);
            }
            Ok(())
        }
        // 5. Replace, under a claim on the stored name made after the gate and the section 99 checks.
        Plan::Replace { stored, meta } => {
            let key_stored = ClaimKey::new(parent_id, &stored);
            let before_create = || -> std::result::Result<(), CopyError> {
                let claimed =
                    claims.borrow_mut().insert_if_absent(&key_stored, &own(ClaimStatus::Existing));
                match claimed {
                    Ok(ClaimOutcome::Inserted) => Ok(()),
                    // A target that finds its own claim proceeds.
                    Ok(ClaimOutcome::Present(r)) if r.target == target => Ok(()),
                    Ok(ClaimOutcome::Present(_)) => Err(CopyError::at(
                        CopyStep::Claim,
                        FsError::new(
                            Code::DestinationNamespaceCollision,
                            std::io::Error::other(
                                "another target of this operation already claimed that destination entry",
                            ),
                        ),
                    )),
                    Err(e) => Err(CopyError::at(CopyStep::Claim, e)),
                }
            };
            let template = PreparedRecord {
                target: target.clone(),
                temp_name: Vec::new(),
                identity: String::new(),
                dir_path: note_dir(&path),
                name: stored.as_encoded_bytes().to_vec(),
                planned_name: if name != stored {
                    name.as_encoded_bytes().to_vec()
                } else {
                    Vec::new()
                },
                replacement: true,
            };
            if batching {
                let planned = (name != stored).then(|| ClaimKey::new(parent_id, name));
                let pending = |path, staged| Pending {
                    path,
                    name: name_owned.clone(),
                    key: key_stored.clone(),
                    planned,
                    template,
                    staged,
                    target: target.clone(),
                    replaced: Some((stored.clone(), meta.clone())),
                };
                return stage_into_batch(
                    cx,
                    parent,
                    names,
                    batch,
                    &src,
                    path,
                    Publish::Replace,
                    &before_create,
                    out,
                    on_report,
                    pending,
                );
            }
            let hook =
                |intent: &PublishIntent| write_note(claims, &key_stored, &template, &wrote, intent);
            let before_publish: &BeforePublish<'_> =
                if strict { &hook } else { &no_before_publish };
            let opts = CopyOptions {
                existing: ExistingPolicy::Overwrite,
                publish: Publish::Replace,
                ..cx.opts.clone()
            };
            let result = copy_file_guarded(
                cx.fs,
                &src,
                parent,
                name,
                &opts,
                cx.guard,
                cx.beat,
                &before_create,
                before_publish,
            );
            discard_note_on_failure(claims, &key_stored, &wrote, &result);
            let published = match &result {
                Ok(o) => Some(o.published_identity),
                Err(_) => None,
            };
            finish_copy(path.clone(), result, out, on_report)?;
            if let Some(identity) = published {
                out.files_overwritten += 1;
                // The entry now holds the new object under its stored name (and, on some systems, the planned one).
                let recorded = if strict {
                    let planned = (name != stored).then(|| ClaimKey::new(parent_id, name));
                    claims.borrow_mut().commit_prepared(&key_stored, &target, planned.as_ref())
                } else {
                    let mut recorded = claims.borrow_mut().upgrade_own_claim(&key_stored, &target);
                    if name != stored {
                        let planned = record_claim(
                            claims,
                            &ClaimKey::new(parent_id, name),
                            &own(ClaimStatus::Created),
                        );
                        recorded = recorded.and(planned);
                    }
                    recorded
                };
                if let Err(e) = recorded {
                    report(out, on_report, path, TreeFailureCause::ClaimNotRecorded(e));
                }
                let replaced = match meta.identity {
                    FileIdentity::Strong(id) => Some(id),
                    _ => None,
                };
                names.record_publication(&target, Some(&stored), name, replaced, identity);
            }
            Ok(())
        }
    }
}

/// Cut 9d: `copy_one` for a batching target, after its plan: stage it into `batch` (spec decision 3), flushing first
/// when it is itself at least `bytes` long (decision 5(b)) and after it when the batch reaches `files` or `bytes`
/// (5(a)). An exclusive create refused with `AlreadyExists` while entries are pending flushes them and stages ONCE more
/// (decision 6's backstop). `pending` completes the entry from its path and its staged temporary.
// One input per piece of frame state the protocol reads, as `copy_one`.
#[allow(clippy::too_many_arguments)]
fn stage_into_batch<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    names: &mut NameIndex,
    batch: &mut Batch,
    src: &Path,
    path: PathBuf,
    publish: Publish,
    before_create: &BeforeCreate<'_>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
    pending: impl FnOnce(PathBuf, Staged) -> Pending,
) -> std::result::Result<(), CopyError> {
    // Decision 6: anything at this entry's temporary name (on a folding destination, a pending sibling's temporary,
    // which `stage_file`'s step-1 sweep would delete) publishes the pending entries first.
    if !batch.pending.is_empty() {
        let name = path.file_name().expect("a walk path ends in a name");
        let temp = flux_fs::temp_path(Path::new(name), &cx.opts.operation_id);
        if parent.metadata(temp.as_os_str()).is_ok() {
            flush_batch(cx, parent, names, batch, out, on_report)?;
        }
    }
    // 5(b). The stat is paid only when something is pending.
    if !batch.pending.is_empty() {
        let len = match cx.fs.metadata(src) {
            Ok(m) => m.len,
            Err(e) => {
                let cause = TreeFailureCause::Copy(CopyError::at(CopyStep::Source, e));
                report(out, on_report, path, cause);
                return Ok(());
            }
        };
        if len >= cx.batch.bytes {
            flush_batch(cx, parent, names, batch, out, on_report)?;
        }
    }
    let name = path.file_name().expect("a walk path ends in a name");
    let opts = CopyOptions { existing: ExistingPolicy::Overwrite, publish, ..cx.opts.clone() };
    let stage = || stage_file(cx.fs, src, parent, name, &opts, cx.guard, cx.beat, before_create);
    let staged = match stage() {
        Err(e)
            if e.step == CopyStep::Create
                && e.cause.source.kind() == ErrorKind::AlreadyExists
                && !batch.pending.is_empty() =>
        {
            flush_batch(cx, parent, names, batch, out, on_report)?;
            stage()
        }
        staged => staged,
    };
    match staged {
        Ok(Stage::Staged(s)) => {
            batch.bytes += s.bytes_copied;
            batch.since.get_or_insert((cx.batch.now)());
            batch.folded.insert(folded(name));
            batch.pending.push(pending(path, s));
        }
        Ok(Stage::Skipped(_)) => {
            unreachable!("the tree stages with ExistingPolicy::Overwrite, which never skips")
        }
        // No note exists yet, and `stage_file` already removed its temporary: nothing else to undo.
        Err(e) => return finish_copy(path, Err(e), out, on_report),
    }
    if batch.pending.len() >= cx.batch.files || batch.bytes >= cx.batch.bytes {
        flush_batch(cx, parent, names, batch, out, on_report)?;
    }
    Ok(())
}

/// Cut 9d, spec decision 4: publish `batch` through `parent`, its directory, and reset it. One `prepare_many` for every
/// note, each entry's rename in order (counted at once, decision 4.6), then one `apply_recovery` committing the renamed
/// and discarding the failed. A per-entry failure is reported and the rest go on (decision 7's per-file fallbacks).
/// `Err` only for a stop (decision 8): the opening heartbeat or guard failing; a lost lock at a publish (STOP-LOST:
/// nothing more is written or removed); a heartbeat failure or a missing primitive at a publish with the lock still
/// held (STOP-HELD: the renamed are committed, the rest discarded).
fn flush_batch<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    names: &mut NameIndex,
    batch: &mut Batch,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    let entries = std::mem::take(batch).pending;
    if entries.is_empty() {
        return Ok(());
    }
    let Some(claims) = cx.claims else {
        unreachable!("only a copy with a claim store stages a batch");
    };
    // 4.1: sections 101 and 99, as before any synced write under DEST. No note exists yet.
    if let Err(e) = (cx.beat)() {
        // The lock is held: every temporary is removed (guarded); one that stays is reported as a leftover.
        for p in entries {
            let source = copy_of(&e).source;
            let failed =
                discard(parent, &p.staged.temp, CopyStep::Heartbeat, e.code, source, cx.guard);
            if failed.leftover.is_some() {
                report_leftover(p.path, failed, out, on_report);
            }
        }
        return Err(CopyError::at(CopyStep::Heartbeat, e));
    }
    if let Err(lost) = (cx.guard)() {
        // Nothing is written or removed: entry 0's temporary rides in the returned error, the others' are reported
        // (plan ruling 2).
        let mut rest = entries.into_iter();
        let p0 = rest.next().expect("the batch is not empty");
        for p in rest {
            report_kept(p.path, &p.staged.temp, &lost, out, on_report);
        }
        return Err(kept(&p0.path, &p0.staged.temp, lost));
    }
    // 4.2: every note in ONE transaction, each built as `write_note` builds it.
    let notes: Vec<(ClaimKey, PreparedRecord)> = entries
        .iter()
        .map(|p| {
            let record = PreparedRecord {
                temp_name: p.staged.temp.as_encoded_bytes().to_vec(),
                identity: identity_text(p.staged.identity),
                ..p.template.clone()
            };
            (p.key.clone(), record)
        })
        .collect();
    // Held in a local so the `RefMut` is dropped before any report callback runs.
    let prepared = claims.borrow_mut().prepare_many(&notes);
    let entries = match prepared {
        Ok(()) => entries,
        // Decision 7: all-or-nothing, so no note was written. Each entry is noted on its own, so a store error fails only
        // the files it hits: such an entry's temporary is removed and it is reported at the claim step, as when
        // `write_note` fails; the others go on.
        Err(_) => {
            let mut noted = Vec::with_capacity(entries.len());
            for (p, (key, record)) in entries.into_iter().zip(&notes) {
                let r = claims.borrow_mut().prepare(key, record);
                match r {
                    Ok(()) => noted.push(p),
                    Err(e) => {
                        let failed = discard(
                            parent,
                            &p.staged.temp,
                            CopyStep::Claim,
                            e.code,
                            e.source,
                            cx.guard,
                        );
                        finish_copy(p.path, Err(failed), out, on_report)?;
                    }
                }
            }
            noted
        }
    };
    // 4.3-4.6. `paths[i]` is the entry `ops[i]` settles, for the per-op fallback's report.
    let mut ops = Vec::with_capacity(entries.len());
    let mut paths = Vec::with_capacity(entries.len());
    let mut rest = entries.into_iter();
    while let Some(p) = rest.next() {
        let Pending { path, name, key, planned, template: _, staged, target, replaced } = p;
        let publish = if replaced.is_some() { Publish::Replace } else { Publish::NoReplace };
        match publish_staged(parent, &name, staged, publish, cx.guard, cx.beat) {
            Ok(o) => {
                let identity = o.published_identity;
                // Counted at the rename, before any later entry is tried (decision 4.6). Never `Err` for an `Ok`.
                finish_copy(path.clone(), Ok(o), out, on_report)?;
                match &replaced {
                    None => names.record_publication(&target, None, &name, None, identity),
                    Some((stored, meta)) => {
                        out.files_overwritten += 1;
                        let replaced_id = match meta.identity {
                            FileIdentity::Strong(id) => Some(id),
                            _ => None,
                        };
                        names.record_publication(
                            &target,
                            Some(stored.as_os_str()),
                            &name,
                            replaced_id,
                            identity,
                        );
                    }
                }
                ops.push(RecoveryOp::Commit { key, target, planned });
                paths.push(path);
            }
            Err(e) => {
                // `finish_copy` classifies (one place): `Ok` is a failure of this entry alone, already reported.
                let Err(stop) = finish_copy(path.clone(), Err(e), out, on_report) else {
                    ops.push(RecoveryOp::Discard { key });
                    paths.push(path);
                    continue;
                };
                if stop.code() == Code::TargetLockBusy {
                    // STOP-LOST: write and remove nothing more. The renamed keep their notes (no `apply_recovery`),
                    // this entry's temporary rides in `stop`, the later ones' are reported (plan ruling 2).
                    for q in rest {
                        report_kept(q.path, &q.staged.temp, &stop.cause, out, on_report);
                    }
                    return Err(stop);
                }
                // STOP-HELD: this entry's temporary is already discarded; every later one is removed BEFORE the
                // transaction that discards the notes (a crash in between leaves notes with no temporary, which
                // recovery classifies GONE; the reverse order could orphan temporaries). Best effort: a failed
                // `apply_recovery` leaves the notes to recovery at `--resume`.
                ops.push(RecoveryOp::Discard { key });
                for q in rest {
                    let source = copy_of(&stop.cause).source;
                    let failed =
                        discard(parent, &q.staged.temp, stop.step, stop.code(), source, cx.guard);
                    if failed.leftover.is_some() {
                        report_leftover(q.path, failed, out, on_report);
                    }
                    ops.push(RecoveryOp::Discard { key: q.key });
                }
                let _ = claims.borrow_mut().apply_recovery(&ops);
                return Err(stop);
            }
        }
    }
    // 4.4-4.5: one transaction settles the batch, skipped only when it would carry no op.
    if !ops.is_empty() {
        let settled = claims.borrow_mut().apply_recovery(&ops);
        if settled.is_err() {
            // Decision 7: nothing changed, so each op is retried on its own. A commit that fails is reported at its
            // entry, its note left for recovery; a discard is best effort.
            for (op, path) in ops.iter().zip(paths) {
                match op {
                    RecoveryOp::Commit { key, target, planned } => {
                        let r = claims.borrow_mut().commit_prepared(key, target, planned.as_ref());
                        if let Err(e) = r {
                            report(out, on_report, path, TreeFailureCause::ClaimNotRecorded(e));
                        }
                    }
                    RecoveryOp::Discard { key } => {
                        let _ = claims.borrow_mut().discard_prepared(key);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Plan ruling 2: `cause`, with the entry's temporary kept as the leftover (relative to DEST, as `finish_copy` rebuilds
/// it), at the publish step, the shape of `publish_staged`'s lost guard.
fn kept(path: &Path, temp: &std::ffi::OsStr, cause: FsError) -> CopyError {
    CopyError {
        cause,
        leftover: Some((
            path.with_file_name(temp),
            std::io::Error::other("kept: this run no longer holds the destination's lock"),
        )),
        step: CopyStep::Publish,
    }
}

/// Plan ruling 2: an entry after the one a lost lock stopped at is reported as a `TargetLockBusy` copy failure whose
/// leftover is its temporary. (`finish_copy` cannot report it: it returns a `TargetLockBusy` failure as a stop.)
fn report_kept(
    path: PathBuf,
    temp: &std::ffi::OsStr,
    lost: &FsError,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) {
    let cause = FsError::new(
        Code::TargetLockBusy,
        std::io::Error::new(lost.source.kind(), lost.source.to_string()),
    );
    let e = kept(&path, temp, cause);
    report(out, on_report, path, TreeFailureCause::Copy(e));
}

/// A temporary a stop's cleanup could not remove: reported with its leftover rebuilt relative to DEST, as
/// `finish_copy` rebuilds one (which would return a stop's failure instead of reporting it).
fn report_leftover(
    path: PathBuf,
    mut e: CopyError,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) {
    if let Some((p, _)) = e.leftover.as_mut() {
        *p = path.with_file_name(&*p);
    }
    report(out, on_report, path, TreeFailureCause::Copy(e));
}

/// Cut 9d, spec decision 5(e): flush every live frame's batch, from the top of the stack down, WITHOUT popping (the
/// root frame goes back to `copy_tree_at`'s caller). The first stop is returned, unless the walk already failed or an
/// earlier frame stopped: such a later stop is dropped (spec decision 8), or reported at its leftover's path when it
/// carries one (plan ruling 2).
fn drain_batches<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    stack: &mut [Frame<F::Dir>],
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
    walk_failed: bool,
) -> std::result::Result<(), CopyError> {
    let mut first: Option<CopyError> = None;
    for frame in stack.iter_mut().rev() {
        let Frame::Live { dir, names, batch, .. } = frame else {
            continue;
        };
        if let Err(e) = flush_batch(cx, dir, names, batch, out, on_report) {
            if walk_failed || first.is_some() {
                // Spec decision 8: the stop is dropped (the returned error already says it), unless it carries a
                // temporary, which must still reach the run as a leftover (plan ruling 2).
                if let Some((path, _)) = &e.leftover {
                    let path = path.clone();
                    report(out, on_report, path, TreeFailureCause::Copy(e));
                }
            } else {
                first = Some(e);
            }
        }
    }
    first.map_or(Ok(()), Err)
}

/// One store error, reported for several entries: `FsError` is not `Clone`, so its code, kind and message are copied.
fn copy_of(e: &FsError) -> FsError {
    FsError::new(e.code, std::io::Error::new(e.source.kind(), e.source.to_string()))
}

/// Cut 9c: `native_hex` of the entry's directory relative to DEST; empty for DEST itself (spec decision 5).
fn note_dir(path: &Path) -> String {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => native_hex(dir),
        _ => String::new(),
    }
}

/// Cut 9c: `before_publish` for a Strict publication: `template` completed with the temporary's name and identity,
/// written as the note at `key`. A store error fails the copy at `CopyStep::Claim` (the temporary is discarded);
/// `wrote` remembers a written note, so a failure after it discards it.
fn write_note<C: ClaimStore>(
    claims: &std::cell::RefCell<C>,
    key: &ClaimKey,
    template: &PreparedRecord,
    wrote: &std::cell::Cell<bool>,
    intent: &PublishIntent,
) -> std::result::Result<(), CopyError> {
    let record = PreparedRecord {
        temp_name: intent.temp.as_encoded_bytes().to_vec(),
        identity: identity_text(intent.identity),
        ..template.clone()
    };
    claims.borrow_mut().prepare(key, &record).map_err(|e| CopyError::at(CopyStep::Claim, e))?;
    wrote.set(true);
    Ok(())
}

/// Cut 9c: a copy that failed after its note was written (the heartbeat, the guard or the rename) did not publish, so
/// the note is discarded at once, before `finish_copy` can stop the run. Best effort: a note left behind is decided by
/// the recovery matrix at the next resume.
fn discard_note_on_failure<C: ClaimStore>(
    claims: &std::cell::RefCell<C>,
    key: &ClaimKey,
    wrote: &std::cell::Cell<bool>,
    result: &std::result::Result<flux_fs::Outcome, CopyError>,
) {
    if wrote.get() && result.is_err() {
        let _ = claims.borrow_mut().discard_prepared(key);
    }
}

/// Insert `record` at `key`; an existing record of the SAME target is fine (a resumed target), another target's is an
/// engine defect, reported as a claim that could not be recorded.
fn record_claim<C: ClaimStore>(
    claims: &std::cell::RefCell<C>,
    key: &ClaimKey,
    record: &ClaimRecord,
) -> std::result::Result<(), FsError> {
    match claims.borrow_mut().insert_if_absent(key, record)? {
        ClaimOutcome::Inserted => Ok(()),
        ClaimOutcome::Present(r) if r.target == record.target => Ok(()),
        ClaimOutcome::Present(_) => Err(FsError::new(
            Code::IoError,
            std::io::Error::other("the claim for this name belongs to another target"),
        )),
    }
}

/// Count what `copy_file_guarded` did for one target, or report and map its failure. `Err` only when the whole
/// operation must stop.
fn finish_copy(
    path: PathBuf,
    result: std::result::Result<flux_fs::Outcome, CopyError>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    match result {
        Ok(o) => {
            debug_assert!(!o.skipped, "the tree never lets the copy path skip");
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
            // Cut 8b: a claim failure (a store error, or a collision with another target's claim) fails this target
            // only, with the code it carries.
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
pub(crate) fn primitive_unavailable(e: &std::io::Error) -> bool {
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

/// A canonical path from a handle; `confirmed` is true when the object at it was seen to have the handle's own
/// `Strong` identity (decision 4).
struct Canonical {
    path: PathBuf,
    confirmed: bool,
}

/// The canonical path of `dir`: `Ok(None)` when the query is unsupported; `Err` for any other failure, including a
/// path holding another `Strong` object (the mismatch).
fn canonical_of<F: DestinationRoot>(
    fs: &F,
    dir: &F::Dir,
) -> std::result::Result<Option<Canonical>, FsError> {
    let path = match dir.canonical_path() {
        Ok(p) => p,
        Err(e) if e.source.kind() == ErrorKind::Unsupported => return Ok(None),
        Err(e) => return Err(e),
    };
    let there = fs.metadata(&path)?.identity;
    let confirmed = match (dir.identity()?, there) {
        (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b => true,
        (FileIdentity::Strong(_), FileIdentity::Strong(_)) => {
            return Err(FsError::new(
                Code::DestinationError,
                std::io::Error::other(
                    "the path reported for an open directory now holds a different object",
                ),
            ));
        }
        _ => false,
    };
    Ok(Some(Canonical { path, confirmed }))
}

/// Part A: the resolved destination anchor must not be the source root or lie inside it. `anchor` is the handle
/// the run then writes through; `anchor_shown` names it in the warning. Runs after the identity pre-flight.
pub(crate) fn containment<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    anchor: &F::Dir,
    anchor_shown: &Path,
    safety: Safety,
    out: &mut TreeOutcome,
) -> std::result::Result<(), CopyError> {
    let resolve = |e| CopyError::at(CopyStep::Resolve, e);
    let degraded = |shown: &Path, out: &mut TreeOutcome| match safety {
        Safety::Default => {
            out.containment_degraded.get_or_insert_with(|| shown.to_path_buf());
            Ok(())
        }
        Safety::Strict => Err(refuse(
            "the destination's location cannot be resolved from its handle, so containment cannot be checked with full confidence",
        )),
    };
    let Some(a) = canonical_of(fs, anchor).map_err(resolve)? else {
        return degraded(anchor_shown, out);
    };
    let src = fs.destination_root(src_root).map_err(resolve)?;
    let Some(s) = canonical_of(fs, &src).map_err(resolve)? else {
        return degraded(src_root, out);
    };
    drop(src);
    if lexically_within(&a.path, &s.path) {
        return Err(refuse("the destination's resolved location is the source or lies inside it"));
    }
    if !(a.confirmed && s.confirmed) {
        return degraded(anchor_shown, out);
    }
    Ok(())
}

/// `inner` equals `outer` or lies inside it, compared component by component with `.`
/// dropped and no filesystem access. `..` is not resolved (that needs the
/// filesystem): a spelling through `..` is compared as written, and identity is the
/// check that sees through it. Paths are compared as given; cut 5's CLI canonicalizes
/// both roots before calling the engine.
pub(crate) fn lexically_within(inner: &Path, outer: &Path) -> bool {
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
pub(crate) fn report(
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
    use std::ffi::OsStr;

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
        t.count(&TreeFailureCause::ClaimNotRecorded(e()));
        assert_eq!(
            t,
            FailureTally {
                walk: 1,
                create_dir: 1,
                copy: 1,
                symlink: 1,
                published_with_complaints: 1,
                claim_not_recorded: 1
            }
        );
        assert_eq!(t.total(), 6, "a skipped special file is not a failure");
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
            existing: flux_fs::ExistingPolicy::Overwrite,
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
        // A real filesystem has its root; the fake only stats it once it is made (Part A reads the parent's identity).
        fs.create_dir(Path::new("/")).unwrap();
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
    fn an_existing_destination_resolving_inside_the_source_is_refused_before_anything_is_created() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_canonical_path("/dst", "/src/sub");
        fs.set_identity("/src/sub", identity_of(&fs, "/dst"));
        let n = fs.calls().len();

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let e = r.unwrap_err();
        assert_eq!((e.code(), e.step), (Code::SafetyRejected, CopyStep::Resolve));
        assert!(e.to_string().contains("resolved location"), "{e}");
        assert!(got.is_empty());
        assert!(!calls_since(&fs, n).iter().any(|c| c.starts_with("create")), "{:?}", fs.calls());
        assert!(!fs.exists("/dst/sub"));
    }

    #[test]
    fn an_absent_destination_whose_parent_resolves_inside_the_source_is_refused_before_anything_is_created()
     {
        let fs = tree();
        fs.create_dir(Path::new("/out")).unwrap();
        fs.set_canonical_path("/out", "/src/sub");
        fs.set_identity("/src/sub", identity_of(&fs, "/out"));
        let n = fs.calls().len();

        let (r, _) = run(&fs, "/src", "/out/copy", &opts());

        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(!fs.exists("/out/copy"));
        assert!(!calls_since(&fs, n).iter().any(|c| c.starts_with("create")), "{:?}", fs.calls());
    }

    #[test]
    fn a_merely_similar_resolved_name_is_not_inside_the_source() {
        let fs = tree();
        fs.create_dir(Path::new("/srcs")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_canonical_path("/dst", "/srcs");
        fs.set_identity("/srcs", identity_of(&fs, "/dst"));

        let (r, _) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(out.files_copied, 2);
        assert!(out.containment_degraded.is_none());
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
        // metadata calls in order: walk's root stat (1), copy_tree's source identity (2), Part A's canonical
        // stats of DEST (3) and of the source (4), copy_file_at's source stat (5), the Step 2a gate's
        // destination stat (6).
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/src/a", b"A");
        fs.fail_nth("metadata", 6, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        let stats: Vec<_> = fs.calls().into_iter().filter(|c| c.starts_with("metadata(")).collect();
        assert!(stats[5].contains("dst"), "the 6th stat is the gate's: {stats:?}");
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
        fs.create_dir(Path::new("/")).unwrap();
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
    fn a_pre_existing_mount_root_is_skipped_and_reported_and_its_siblings_are_copied() {
        let fs = tree();
        fs.write_file("/src/c", b"C");
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/sub")).unwrap();
        fs.set_mount_root("/dst/sub", flux_fs::MountRoot::Yes);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!((out.files_copied, out.failures.create_dir), (2, 1), "{got:?}");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, PathBuf::from("sub"));
        let TreeFailureCause::CreateDir(e) = &got[0].cause else { panic!("{:?}", got[0].cause) };
        assert_eq!(e.code, Code::SafetyRejected);
        assert!(fs.exists("/dst/a") && fs.exists("/dst/c"));
        assert!(!fs.exists("/dst/sub/b"), "nothing was written into the mount");
    }

    #[test]
    fn a_directory_this_run_created_and_dest_itself_are_never_queried() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        // DEST may be a mount root (`/mnt/usb`): known limit 1, never checked. `sub` is created by this run.
        fs.set_mount_root("/dst", flux_fs::MountRoot::Yes);
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        assert!(r.is_ok() && got.is_empty(), "{got:?}");
        assert!(!fs.called("mount_root("), "{:?}", fs.calls());
    }

    #[test]
    fn a_mount_query_that_cannot_tell_warns_under_default_and_refuses_the_subtree_under_strict() {
        let lax = tree();
        lax.create_dir(Path::new("/dst")).unwrap();
        lax.create_dir(Path::new("/dst/sub")).unwrap();
        lax.set_mount_root("/dst/sub", flux_fs::MountRoot::Unknown);
        let (r, got) = run(&lax, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(
            out.mount_unknown,
            Some(DegradedGroup { count: 1, example: PathBuf::from("sub") })
        );
        assert!(lax.exists("/dst/sub/b"), "merged anyway");

        let tight = tree();
        tight.create_dir(Path::new("/dst")).unwrap();
        tight.create_dir(Path::new("/dst/sub")).unwrap();
        tight.set_mount_root("/dst/sub", flux_fs::MountRoot::Unknown);
        let (r, got) = run(&tight, "/src", "/dst", &strict());
        let out = r.unwrap();
        assert_eq!(out.failures.create_dir, 1, "{got:?}");
        assert!(out.mount_unknown.is_none());
        assert!(!tight.exists("/dst/sub/b"));
    }

    #[test]
    fn a_strict_cannot_tell_mount_root_is_refused_with_safety_rejected() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/sub")).unwrap();
        fs.set_mount_root("/dst/sub", flux_fs::MountRoot::Unknown);

        let (r, got) = run(&fs, "/src", "/dst", &strict());

        assert!(r.is_ok(), "{r:?}");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("sub"));
        let TreeFailureCause::CreateDir(e) = &got[0].cause else { panic!("{:?}", got[0].cause) };
        assert_eq!(e.code, Code::SafetyRejected);
        assert!(e.source.to_string().contains("cannot be told"), "{}", e.source);
        assert!(!fs.exists("/dst/sub/b"), "nothing was written into the unknown directory");
    }

    #[test]
    fn two_cannot_tell_mount_roots_are_one_group_with_count_two_and_the_first_example() {
        // `tree()` has a FILE `/src/a`, so this builds its own source.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/")).unwrap();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.create_dir(Path::new("/src/a")).unwrap();
        fs.create_dir(Path::new("/src/b")).unwrap();
        fs.write_file("/src/a/x", b"X");
        fs.write_file("/src/b/y", b"Y");
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/a")).unwrap();
        fs.create_dir(Path::new("/dst/b")).unwrap();
        fs.set_mount_root("/dst/a", flux_fs::MountRoot::Unknown);
        fs.set_mount_root("/dst/b", flux_fs::MountRoot::Unknown);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(
            out.mount_unknown,
            Some(DegradedGroup { count: 2, example: PathBuf::from("a") })
        );
        assert!(fs.exists("/dst/a/x") && fs.exists("/dst/b/y"), "both subtrees were merged");
    }

    #[test]
    fn the_source_root_check_runs_before_the_mount_query() {
        // A mount inside DEST presenting the SOURCE ROOT: the whole operation aborts (section 129), it does not
        // become a skipped subtree, and the mount query is never asked.
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/sub")).unwrap();
        fs.set_identity("/dst/sub", identity_of(&fs, "/src"));
        fs.set_mount_root("/dst/sub", flux_fs::MountRoot::Yes);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(got.is_empty(), "an abort, not a streamed failure: {got:?}");
        assert!(!fs.called("mount_root("), "{:?}", fs.calls());
    }

    #[test]
    fn a_failed_mount_query_fails_that_subtree_like_a_failed_open() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/sub")).unwrap();
        fs.fail("mount_root", Code::PermissionDenied);
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert_eq!((out.files_copied, out.failures.create_dir), (1, 1));
        let TreeFailureCause::CreateDir(e) = &got[0].cause else { panic!("{:?}", got[0].cause) };
        assert_eq!(e.code, Code::PermissionDenied);
        assert!(!fs.exists("/dst/sub/b"));
    }

    #[test]
    fn a_folded_directory_is_a_collision_before_any_mount_query() {
        // Two source names folding onto one destination directory (Review Focus 1), staged exactly as
        // `two_source_directories_folded_into_one_destination_are_a_collision` (`:948`) stages it: `/dst/a` pre-exists
        // carrying the identity `/dst/A` gets when this run makes it. The fold is reported as today, and the mount
        // query is never reached for it even though `/dst/a` is marked a mount root.
        let fs = FaultFs::new();
        for d in ["/src", "/src/A", "/src/a", "/dst", "/dst/a"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/src/A/x", b"x");
        fs.write_file("/src/a/y", b"y");
        let folded = FileIdentity::Strong(ObjectId { volume: 1, index: 9_003 });
        fs.set_identity("/dst/A", folded);
        fs.set_identity("/dst/a", folded);
        fs.set_mount_root("/dst/a", flux_fs::MountRoot::Yes);
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert_eq!(out.failures.create_dir, 1, "{got:?}");
        let TreeFailureCause::CreateDir(e) = &got[0].cause else { panic!("{:?}", got[0].cause) };
        assert_eq!(e.code, Code::DestinationNamespaceCollision);
        assert!(!fs.called("mount_root("), "{:?}", fs.calls());
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
        fs.create_dir(Path::new("/")).unwrap();
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
            claims: None,
            resume: false,
            batch: BatchPolicy::DEFAULT,
        };
        let mut out = TreeOutcome::default();
        let (_root, r) = copy_tree_at(&cx, source.events, root, false, &mut out, &mut |_| {});
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
            claims: None,
            resume: false,
            batch: BatchPolicy::DEFAULT,
        };
        let mut out = TreeOutcome::default();
        let mut reported = 0;
        let (_root, r) =
            copy_tree_at(&cx, source.events, root, false, &mut out, &mut |_| reported += 1);
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
            claims: None,
            resume: false,
            batch: BatchPolicy::DEFAULT,
        };
        let mut out = TreeOutcome::default();
        let mut reported = 0;
        let (_root, r) =
            copy_tree_at(&cx, source.events, root, false, &mut out, &mut |_| reported += 1);
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
            claims: None,
            resume: false,
            batch: BatchPolicy::DEFAULT,
        };
        let mut out = TreeOutcome::default();
        let (back, r) = copy_tree_at(&cx, source.events, root, false, &mut out, &mut |_| {});
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

    // Cut 9d: the per-directory batch. The default policy is OFF (`files == 1`), so every test here passes its own.

    fn policy(files: usize) -> BatchPolicy {
        BatchPolicy { files, ..BatchPolicy::DEFAULT }
    }

    /// `/` and `/src` holding `entries` (paths relative to `/src`, parent directories made as needed). `/dst` absent.
    fn sources(entries: &[(&str, &[u8])]) -> FaultFs {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/")).unwrap();
        fs.create_dir(Path::new("/src")).unwrap();
        for (rel, bytes) in entries {
            let full = Path::new("/src").join(rel);
            let parent = full.parent().unwrap();
            if !fs.exists(parent) {
                fs.create_dir(parent).unwrap();
            }
            fs.write_file(&full, bytes);
        }
        fs
    }

    /// `f0`..`f{n-1}` in `/src`, each holding its own name.
    fn n_files(n: usize) -> FaultFs {
        let names: Vec<String> = (0..n).map(|i| format!("f{i}")).collect();
        let entries: Vec<(&str, &[u8])> =
            names.iter().map(|n| (n.as_str(), n.as_bytes())).collect();
        sources(&entries)
    }

    /// `copy_tree_at` from `/src` into a pre-made `/dst` with a claim store at `/dst/state.db`, both the copy and the
    /// store at `durability`, under `policy` (the pattern of `run/tests.rs`'s `a_target_that_finds_its_own_claim_proceeds`).
    fn batched(
        fs: &FaultFs,
        policy: BatchPolicy,
        durability: Durability,
    ) -> (std::result::Result<(), CopyError>, TreeOutcome, Vec<TreeFailure>) {
        batched_with(fs, policy, durability, &unguarded, &no_heartbeat)
    }

    /// `batched` with the run's guard and heartbeat injected.
    fn batched_with(
        fs: &FaultFs,
        policy: BatchPolicy,
        durability: Durability,
        guard: &Guard<'_>,
        beat: &Heartbeat<'_>,
    ) -> (std::result::Result<(), CopyError>, TreeOutcome, Vec<TreeFailure>) {
        let source = prepare_source(fs, Path::new("/src"), Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let store = root.create_claim_store(OsStr::new("state.db"), durability).unwrap();
        let claims = std::cell::RefCell::new(store);
        let mut o = opts();
        o.durability = durability;
        let cx = Shared {
            fs,
            src_root: Path::new("/src"),
            src_identity: source.identity,
            opts: &o,
            guard,
            beat,
            claims: Some(&claims),
            resume: false,
            batch: policy,
        };
        let mut out = TreeOutcome::default();
        let mut got = Vec::new();
        let (_root, r) =
            copy_tree_at(&cx, source.events, root, false, &mut out, &mut |f| got.push(f));
        (r, out, got)
    }

    /// The call log with `/` separators.
    fn log(fs: &FaultFs) -> Vec<String> {
        fs.calls().into_iter().map(|c| c.replace('\\', "/")).collect()
    }

    fn count(c: &[String], prefix: &str) -> usize {
        c.iter().filter(|x| x.starts_with(prefix)).count()
    }

    /// Every index of a call starting with `prefix`, in order.
    fn positions(c: &[String], prefix: &str) -> Vec<usize> {
        c.iter().enumerate().filter(|(_, x)| x.starts_with(prefix)).map(|(i, _)| i).collect()
    }

    fn first(c: &[String], prefix: &str) -> usize {
        c.iter()
            .position(|x| x.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} in {c:?}"))
    }

    /// The first exclusive create of `rel`'s temporary under `/dst`.
    fn created(c: &[String], rel: &str) -> usize {
        first(c, &format!("create_new(/dst/{rel}.flux-partial.op1)"))
    }

    /// The publishing rename of `rel`'s temporary under `/dst`.
    fn renamed(c: &[String], rel: &str) -> usize {
        first(c, &format!("rename_no_replace(/dst/{rel}.flux-partial.op1 -> /dst/{rel})"))
    }

    /// The `prepare_many` calls, in order.
    fn prepares(c: &[String]) -> Vec<&str> {
        c.iter().filter(|x| x.starts_with("claim_prepare_many(")).map(String::as_str).collect()
    }

    fn strong_id(fs: &FaultFs, p: &str) -> ObjectId {
        match identity_of(fs, p) {
            FileIdentity::Strong(id) => id,
            other => panic!("{p} is not strong: {other:?}"),
        }
    }

    /// A report as `(path, what)`: `TreeFailure` has no `PartialEq`.
    fn shape(got: &[TreeFailure]) -> Vec<(PathBuf, String)> {
        got.iter()
            .map(|f| {
                let what = match &f.cause {
                    TreeFailureCause::Copy(e) => format!("Copy {:?} at {:?}", e.code(), e.step),
                    TreeFailureCause::ClaimNotRecorded(e) => {
                        format!("ClaimNotRecorded {:?}", e.code)
                    }
                    other => format!("{other:?}"),
                };
                (f.path.clone(), what)
            })
            .collect()
    }

    #[test]
    fn n_files_in_one_directory_make_ceil_n_over_batch_calls() {
        let fs = n_files(10);
        let (r, out, got) = batched(&fs, policy(4), Durability::Strict);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        let c = log(&fs);
        assert_eq!(count(&c, "claim_prepare_many("), 3, "{c:?}");
        assert_eq!(count(&c, "claim_apply_recovery"), 3, "{c:?}");
        assert_eq!(count(&c, "claim_prepare("), 0, "{c:?}");
        assert_eq!(count(&c, "claim_commit_prepared("), 0, "{c:?}");
        let dst = strong_id(&fs, "/dst");
        for i in 0..10 {
            let n = format!("f{i}");
            assert_eq!(fs.read_file(format!("/dst/{n}")).as_deref(), Some(n.as_bytes()), "{n}");
            assert_eq!(fs.claim(dst, &n).map(|r| r.status), Some(ClaimStatus::Created), "{n}");
        }
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(out.files_copied, 10);
    }

    #[test]
    fn the_boundary_of_a_batch() {
        let fs = n_files(4);
        batched(&fs, policy(4), Durability::Strict).0.unwrap();
        let c = log(&fs);
        assert_eq!(prepares(&c), ["claim_prepare_many(4)"], "{c:?}");

        let fs = n_files(5);
        batched(&fs, policy(4), Durability::Strict).0.unwrap();
        let c = log(&fs);
        assert_eq!(prepares(&c), ["claim_prepare_many(4)", "claim_prepare_many(1)"], "{c:?}");
        assert!(
            renamed(&c, "f3") < created(&c, "f4"),
            "the 4th publishes before the 5th is staged: {c:?}"
        );
    }

    #[test]
    fn normal_durability_never_batches() {
        let fs = n_files(5);
        let (r, out, _) = batched(&fs, policy(4), Durability::Normal);
        r.unwrap();
        let c = log(&fs);
        assert_eq!(count(&c, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(count(&c, "claim_apply_recovery"), 0, "{c:?}");
        assert_eq!(out.files_copied, 5);
    }

    #[test]
    fn a_format_1_store_never_batches() {
        let fs = n_files(5);
        fs.set_claim_store_format("/dst/state.db", 1);
        let (r, out, got) = batched(&fs, policy(4), Durability::Strict);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        let c = log(&fs);
        assert_eq!(count(&c, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(out.files_copied, 5);
    }

    #[test]
    fn files_one_is_the_unbatched_path() {
        let fs = n_files(3);
        let (r, out, got) = batched(&fs, policy(1), Durability::Strict);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        let c = log(&fs);
        assert_eq!(count(&c, "claim_prepare("), 3, "{c:?}");
        assert_eq!(count(&c, "claim_commit_prepared("), 3, "{c:?}");
        assert_eq!(count(&c, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(out.files_copied, 3);
    }

    #[test]
    fn bytes_trigger() {
        let fs = sources(&[("a", b"aaaaaa"), ("b", b"bbbbbb"), ("c", b"cccccc")]);
        let p = BatchPolicy { files: 64, bytes: 10, ..BatchPolicy::DEFAULT };
        let (r, out, _) = batched(&fs, p, Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        assert_eq!(prepares(&c), ["claim_prepare_many(2)", "claim_prepare_many(1)"], "{c:?}");
        assert!(renamed(&c, "b") < created(&c, "c"), "12 >= 10 flushes before c is staged: {c:?}");
        assert_eq!(out.files_copied, 3);
    }

    #[test]
    fn a_large_file_flushes_the_pending_small_ones_first_and_is_staged_alone() {
        let fs = sources(&[("a", b"aa"), ("b", &[b'b'; 20]), ("c", b"cc")]);
        let p = BatchPolicy { files: 64, bytes: 10, ..BatchPolicy::DEFAULT };
        let (r, out, _) = batched(&fs, p, Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        assert!(renamed(&c, "a") < created(&c, "b"), "a publishes before b is staged: {c:?}");
        assert_eq!(
            prepares(&c),
            ["claim_prepare_many(1)", "claim_prepare_many(1)", "claim_prepare_many(1)"],
            "{c:?}"
        );
        let p = positions(&c, "claim_prepare_many(");
        assert!(created(&c, "b") < p[1] && p[1] < created(&c, "c"), "b is flushed alone: {c:?}");
        assert!(renamed(&c, "b") < created(&c, "c"), "{c:?}");
        assert!(created(&c, "c") < p[2], "c is separate: {c:?}");
        assert_eq!(out.files_copied, 3);
    }

    #[test]
    fn a_directory_end_flushes_the_child_batch_before_the_parents() {
        let fs = sources(&[("a0", b"a0"), ("m/x", b"x"), ("z", b"z")]);
        let (r, out, got) = batched(&fs, policy(64), Durability::Strict);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        let c = log(&fs);
        let commit = first(&c, "claim_apply_recovery");
        assert!(renamed(&c, "m/x") < commit && commit < renamed(&c, "a0"), "{c:?}");
        assert_eq!(prepares(&c), ["claim_prepare_many(1)", "claim_prepare_many(2)"], "{c:?}");
        let parents = first(&c, "claim_prepare_many(2)");
        assert!(commit < parents, "{c:?}");
        assert!(parents < renamed(&c, "a0") && renamed(&c, "a0") < renamed(&c, "z"), "{c:?}");
        assert!(created(&c, "z") < parents, "z joined the parent's pending batch: {c:?}");
        for (p, bytes) in [("/dst/a0", &b"a0"[..]), ("/dst/m/x", b"x"), ("/dst/z", b"z")] {
            assert_eq!(fs.read_file(p).as_deref(), Some(bytes), "{p}");
        }
        assert_eq!((out.files_copied, out.directories_created), (3, 1));
    }

    thread_local! {
        static CLOCK: (std::time::Instant, std::cell::Cell<std::time::Duration>) =
            (std::time::Instant::now(), std::cell::Cell::new(std::time::Duration::ZERO));
    }

    /// The injected clock: a fixed base plus a settable offset.
    fn fake_now() -> std::time::Instant {
        CLOCK.with(|c| c.0 + c.1.get())
    }

    #[test]
    fn the_age_trigger_uses_the_injected_clock() {
        let fs = sources(&[("a", b"a"), ("b", b"b"), ("c", b"c")]);
        // t = 2 s from b's exclusive create on: after a set the batch's start, before c's staging is decided.
        fs.on_nth("create_new", 2, |_| {
            CLOCK.with(|c| c.1.set(std::time::Duration::from_secs(2)));
        });
        let p = BatchPolicy {
            files: 64,
            age: std::time::Duration::from_secs(1),
            now: fake_now,
            ..BatchPolicy::DEFAULT
        };
        let (r, out, _) = batched(&fs, p, Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        assert!(renamed(&c, "a") < created(&c, "c"), "{c:?}");
        assert!(renamed(&c, "b") < created(&c, "c"), "{c:?}");
        assert_eq!(prepares(&c), ["claim_prepare_many(2)", "claim_prepare_many(1)"], "{c:?}");
        assert_eq!(out.files_copied, 3);
    }

    #[test]
    fn the_walk_end_flushes_the_root_batch() {
        let fs = n_files(3);
        let (r, out, got) = batched(&fs, policy(64), Durability::Strict);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        for i in 0..3 {
            let n = format!("f{i}");
            assert_eq!(fs.read_file(format!("/dst/{n}")).as_deref(), Some(n.as_bytes()), "{n}");
        }
        let c = log(&fs);
        assert_eq!(prepares(&c), ["claim_prepare_many(3)"], "{c:?}");
        assert_eq!(out.files_copied, 3);
        assert_eq!(fs.prepared_count(), 0);
    }

    /// `File` and `file` in one source directory, onto a case-folding destination.
    fn folding_pair() -> FaultFs {
        let fs = sources(&[("File", b"upper"), ("file", b"lower")]);
        fs.set_case_insensitive(true);
        fs
    }

    #[test]
    fn names_that_fold_in_one_directory_flush_before_the_second_is_staged() {
        let fs = folding_pair();
        let (r, out, got) = batched(&fs, policy(64), Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        let published = renamed(&c, "File");
        // No exclusive create but `File`'s own precedes its rename (the second is refused before or at its create).
        let creates = positions(&c, "create_new(");
        assert_eq!(creates[0], created(&c, "File"), "{c:?}");
        assert!(creates[1..].iter().all(|&p| p > published), "{c:?}");
        assert_eq!(fs.read_file("/dst/File").as_deref(), Some(&b"upper"[..]));
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("file"));
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(e.code(), Code::DestinationNamespaceCollision);

        let fs = folding_pair();
        let (r1, out1, got1) = batched(&fs, policy(1), Durability::Strict);
        r1.unwrap();
        assert_eq!(shape(&got), shape(&got1), "the same code and step as the unbatched run");
        assert_eq!(
            (out.files_copied, out.failures.total()),
            (out1.files_copied, out1.failures.total())
        );
    }

    #[test]
    fn a_fold_equal_pending_temporary_is_never_swept() {
        let fs = folding_pair();
        let p = BatchPolicy { fold_precheck: false, ..policy(64) };
        let (r, out, got) = batched(&fs, p, Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        assert_eq!(fs.read_file("/dst/File").as_deref(), Some(&b"upper"[..]), "{c:?}");
        // `file` reaches no exclusive create once `File` is published (its gate refuses it), so: no create but `File`'s
        // own precedes `File`'s rename.
        let creates = positions(&c, "create_new(");
        assert_eq!(creates[0], created(&c, "File"), "{c:?}");
        assert!(creates[1..].iter().all(|&p| p > renamed(&c, "File")), "{c:?}");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("file"));
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(e.code(), Code::DestinationNamespaceCollision, "{e:?}");
        assert_eq!(out.files_copied, 1);
    }

    #[test]
    fn an_exclusive_create_collision_with_pending_entries_flushes_and_retries_once() {
        let fs = sources(&[("a", b"A"), ("b", b"B")]);
        fs.fail_nth("create_new", 2, Code::IoError, ErrorKind::AlreadyExists);
        let (r, out, got) = batched(&fs, policy(64), Durability::Strict);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        let c = log(&fs);
        let b_creates = positions(&c, "create_new(/dst/b.flux-partial.op1)");
        assert_eq!(b_creates.len(), 2, "{c:?}");
        assert!(renamed(&c, "a") < b_creates[1], "a is flushed before b's retry: {c:?}");
        assert_eq!(count(&c, "create_new("), 3, "{c:?}");
        assert_eq!(fs.read_file("/dst/b").as_deref(), Some(&b"B"[..]));
        assert_eq!(out.files_copied, 2);
    }

    #[test]
    fn an_exclusive_create_collision_that_persists_is_retried_once_only() {
        let fs = sources(&[("a", b"A"), ("b", b"B")]);
        // Every exclusive create from b's first on answers AlreadyExists.
        fs.on_nth("create_new", 2, |fs| {
            fs.fail_kind("create_new", Code::IoError, ErrorKind::AlreadyExists);
            fs.fail_always("create_new", Code::IoError);
        });
        let (r, out, got) = batched(&fs, policy(64), Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        assert_eq!(positions(&c, "create_new(/dst/b.flux-partial.op1)").len(), 2, "{c:?}");
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"A"[..]));
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("b"));
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        // What an unbatched copy reports for a create refused `AlreadyExists`: `finish_copy` maps only Gate and Publish.
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Create), "{e:?}");
        assert_eq!(out.files_copied, 1);
    }

    /// 130 files in two directories, 65 each.
    fn two_directories() -> FaultFs {
        let owned: Vec<(String, Vec<u8>)> = ["d1", "d2"]
            .iter()
            .flat_map(|d| {
                (0..65).map(move |i| (format!("{d}/f{i:03}"), format!("{d}{i}").into_bytes()))
            })
            .collect();
        let entries: Vec<(&str, &[u8])> =
            owned.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        sources(&entries)
    }

    #[test]
    fn equivalence_of_batched_and_unbatched() {
        let on = two_directories();
        let (r_on, out_on, got_on) = batched(&on, policy(64), Durability::Strict);
        r_on.unwrap();
        let off = two_directories();
        let (r_off, out_off, got_off) = batched(&off, policy(1), Durability::Strict);
        r_off.unwrap();
        for d in ["d1", "d2"] {
            let (id_on, id_off) =
                (strong_id(&on, &format!("/dst/{d}")), strong_id(&off, &format!("/dst/{d}")));
            for i in 0..65 {
                let n = format!("f{i:03}");
                let p = format!("/dst/{d}/{n}");
                assert!(on.read_file(&p).is_some(), "{p}");
                assert_eq!(on.read_file(&p), off.read_file(&p), "{p}");
                assert!(on.claim(id_on, &n).is_some(), "{p}");
                assert_eq!(on.claim(id_on, &n), off.claim(id_off, &n), "{p}");
            }
        }
        assert_eq!(
            (out_on.files_copied, out_on.bytes_copied, out_on.directories_created),
            (out_off.files_copied, out_off.bytes_copied, out_off.directories_created)
        );
        assert_eq!(out_on.files_copied, 130);
        assert_eq!(shape(&got_on), shape(&got_off));
        let (c_on, c_off) = (log(&on), log(&off));
        assert!(count(&c_on, "claim_prepare_many(") > 0 && count(&c_on, "claim_prepare(") == 0);
        assert!(count(&c_off, "claim_prepare_many(") == 0 && count(&c_off, "claim_prepare(") > 0);
    }

    #[test]
    fn batch_policy_default_matches_the_spec() {
        assert_eq!(BATCH_FILES, 64);
        assert_eq!(BATCH_BYTES, 64 << 20);
        assert_eq!(BATCH_AGE, std::time::Duration::from_secs(1));
        assert_eq!(BatchPolicy::DEFAULT.bytes, BATCH_BYTES);
        assert_eq!(BatchPolicy::DEFAULT.age, BATCH_AGE);
    }

    // Cut 9d Task 4: failure isolation (spec decision 7), the stops (decision 8), the abort drain (5(e)).

    /// The temporary of `rel`, relative to DEST, as a leftover names it.
    fn temp_of(rel: &str) -> PathBuf {
        PathBuf::from(format!("{rel}.flux-partial.op1"))
    }

    /// Every leftover temporary named by a `Copy` report in `got` and by `err`, sorted.
    fn leftovers(got: &[TreeFailure], err: Option<&CopyError>) -> Vec<PathBuf> {
        let mut all: Vec<PathBuf> = got
            .iter()
            .filter_map(|f| match &f.cause {
                TreeFailureCause::Copy(e) => e.leftover.as_ref().map(|(p, _)| p.clone()),
                _ => None,
            })
            .chain(err.and_then(|e| e.leftover.as_ref().map(|(p, _)| p.clone())))
            .collect();
        all.sort();
        all
    }

    /// `f`'s claim under `/dst` is `Created`.
    fn claimed(fs: &FaultFs, f: &str) -> bool {
        fs.claim(strong_id(fs, "/dst"), f).map(|r| r.status) == Some(ClaimStatus::Created)
    }

    /// `f` is published under `/dst` with its own name as its bytes (the `n_files` fixture).
    fn published(fs: &FaultFs, f: &str) -> bool {
        fs.read_file(format!("/dst/{f}")).as_deref() == Some(f.as_bytes())
    }

    #[test]
    fn a_failing_prepare_many_falls_back_to_per_entry_prepare() {
        let fs = n_files(3);
        fs.fail("claim_prepare_many", Code::IoError);
        let (r, out, got) = batched(&fs, policy(4), Durability::Strict);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        let c = log(&fs);
        let many = first(&c, "claim_prepare_many(3)");
        let each = positions(&c, "claim_prepare(");
        assert_eq!(each.len(), 3, "{c:?}");
        assert!(each.iter().all(|&p| p > many), "{c:?}");
        for f in ["f0", "f1", "f2"] {
            assert!(published(&fs, f) && claimed(&fs, f), "{f}: {c:?}");
        }
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(out.files_copied, 3);
    }

    #[test]
    fn one_entry_own_prepare_failing_fails_only_that_file() {
        let fs = n_files(3);
        fs.fail_always("claim_prepare_many", Code::IoError);
        fs.fail_nth("claim_prepare", 2, Code::IoError, ErrorKind::Other);
        let (r, out, got) = batched(&fs, policy(4), Durability::Strict);
        r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("f1"));
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(e.step, CopyStep::Claim, "{e:?}");
        assert!(!fs.exists("/dst/f1.flux-partial.op1") && !fs.exists("/dst/f1"));
        assert!(fs.claim(strong_id(&fs, "/dst"), "f1").is_none());
        assert_eq!(fs.prepared_count(), 0, "no note for it, and the others committed");
        for f in ["f0", "f2"] {
            assert!(published(&fs, f) && claimed(&fs, f), "{f}");
        }
        assert_eq!(out.files_copied, 2);
    }

    #[test]
    fn a_failing_apply_recovery_falls_back_to_per_entry_commits() {
        let fs = n_files(3);
        fs.fail("claim_apply_recovery", Code::IoError);
        let (r, out, got) = batched(&fs, policy(4), Durability::Strict);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        let c = log(&fs);
        let settle = first(&c, "claim_apply_recovery");
        let each = positions(&c, "claim_commit_prepared(");
        assert_eq!(each.len(), 3, "{c:?}");
        assert!(each.iter().all(|&p| p > settle), "{c:?}");
        for f in ["f0", "f1", "f2"] {
            assert!(published(&fs, f) && claimed(&fs, f), "{f}");
        }
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(out.files_copied, 3);
    }

    #[test]
    fn one_entry_own_commit_failing_is_claim_not_recorded() {
        let fs = n_files(3);
        fs.fail_always("claim_apply_recovery", Code::IoError);
        fs.fail_nth("claim_commit_prepared", 2, Code::IoError, ErrorKind::Other);
        let (r, out, got) = batched(&fs, policy(4), Durability::Strict);
        r.unwrap();
        assert!(published(&fs, "f1"), "the file is published");
        assert_eq!(
            got.len(),
            1,
            "no file is both published and reported failed but this one: {got:?}"
        );
        assert_eq!(got[0].path, PathBuf::from("f1"));
        assert!(matches!(got[0].cause, TreeFailureCause::ClaimNotRecorded(_)), "{got:?}");
        assert_eq!(fs.prepared_count(), 1, "its note is left for recovery");
        assert!(fs.claim(strong_id(&fs, "/dst"), "f1").is_none());
        for f in ["f0", "f2"] {
            assert!(published(&fs, f) && claimed(&fs, f), "{f}");
        }
        assert_eq!((out.files_copied, out.failures.copy), (3, 0));
    }

    #[test]
    fn a_failed_rename_discards_its_note_inside_the_one_apply_recovery() {
        let fs = n_files(3);
        fs.fail_nth("rename_no_replace", 2, Code::IoError, ErrorKind::Other);
        let (r, out, got) = batched(&fs, policy(4), Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        // One transaction carries the two commits and the discard: no per-entry call settles anything.
        assert_eq!(count(&c, "claim_apply_recovery"), 1, "{c:?}");
        assert_eq!(count(&c, "claim_discard_prepared("), 0, "{c:?}");
        assert_eq!(count(&c, "claim_commit_prepared("), 0, "{c:?}");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("f1"));
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(e.step, CopyStep::Publish, "{e:?}");
        assert!(!fs.exists("/dst/f1.flux-partial.op1") && !fs.exists("/dst/f1"));
        assert_eq!(fs.prepared_count(), 0, "the failed entry's note was discarded");
        for f in ["f0", "f2"] {
            assert!(published(&fs, f) && claimed(&fs, f), "{f}");
        }
        assert_eq!(out.files_copied, 2);
    }

    #[test]
    fn the_rename_collision_is_a_namespace_collision() {
        let fs = n_files(3);
        // Between staging and the renames, someone else writes f1.
        fs.on_nth("claim_prepare_many", 1, |fs| fs.write_file("/dst/f1", b"theirs"));
        let (r, out, got) = batched(&fs, policy(4), Durability::Strict);
        r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("f1"));
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(e.code(), Code::DestinationNamespaceCollision, "{e:?}");
        assert_eq!(fs.read_file("/dst/f1").as_deref(), Some(&b"theirs"[..]));
        for f in ["f0", "f2"] {
            assert!(published(&fs, f) && claimed(&fs, f), "{f}");
        }
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(out.files_copied, 2);
    }

    #[test]
    fn a_lost_lock_at_entry_k_writes_and_removes_nothing() {
        let fs = n_files(4);
        let calls = std::cell::Cell::new(0);
        // Staging guards twice per file (1-8), the flush's step 1 once (9), entries 0 and 1 once each (10, 11): entry
        // 2's publish guard (12) is the first to fail.
        let guard = failing_after(4 * 2 + 1 + 2, &calls);
        let (r, out, got) = batched_with(&fs, policy(4), Durability::Strict, &guard, &no_heartbeat);
        let error = r.unwrap_err();
        assert_eq!(error.code(), Code::TargetLockBusy, "{error:?}");
        assert!(error.leftover.is_some());
        assert_eq!(error.leftover.as_ref().map(|(p, _)| p.clone()), Some(temp_of("f2")));
        assert_eq!(out.files_copied, 2);
        let c = log(&fs);
        assert_eq!(count(&c, "rename_"), 2, "entries 0 and 1 renamed: {c:?}");
        assert!(published(&fs, "f0") && published(&fs, "f1"));
        assert_eq!(fs.prepared_count(), 4, "every note stays");
        assert!(
            fs.claim(strong_id(&fs, "/dst"), "f0").is_none(),
            "no claim committed after the loss"
        );
        let lost = renamed(&c, "f1");
        for after in &c[lost + 1..] {
            assert!(
                !after.starts_with("claim_apply_recovery")
                    && !after.starts_with("claim_discard_prepared(")
                    && !after.starts_with("remove_file("),
                "nothing written or removed after the loss: {c:?}"
            );
        }
        assert!(fs.exists("/dst/f2.flux-partial.op1") && fs.exists("/dst/f3.flux-partial.op1"));
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("f3"));
        let TreeFailureCause::Copy(e3) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(e3.code(), Code::TargetLockBusy);
        assert_eq!(leftovers(&got, Some(&error)), [temp_of("f2"), temp_of("f3")]);
        let abort = TreeAbort { error, outcome: out };
        assert!(abort.changed());
        assert!(!abort.refused_unchanged());
    }

    #[test]
    fn a_lost_lock_at_the_step_one_guard_leaves_every_temporary() {
        let fs = n_files(4);
        let calls = std::cell::Cell::new(0);
        // Staging guards 1-8; the flush's first guard (9) fails.
        let guard = failing_after(4 * 2, &calls);
        let (r, out, got) = batched_with(&fs, policy(4), Durability::Strict, &guard, &no_heartbeat);
        let error = r.unwrap_err();
        assert_eq!((error.code(), error.step), (Code::TargetLockBusy, CopyStep::Publish));
        let c = log(&fs);
        assert_eq!(count(&c, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(count(&c, "rename_"), 0, "{c:?}");
        for f in ["f0", "f1", "f2", "f3"] {
            assert!(fs.exists(format!("/dst/{f}.flux-partial.op1")), "{f}");
        }
        let all: Vec<PathBuf> = ["f0", "f1", "f2", "f3"].iter().map(|f| temp_of(f)).collect();
        assert_eq!(leftovers(&got, Some(&error)), all, "{got:?}");
        assert_eq!(out.files_copied, 0);
        let abort = TreeAbort { error, outcome: out };
        assert!(abort.changed(), "because of the leftover");
    }

    #[test]
    fn a_heartbeat_failure_with_the_lock_held_commits_the_renamed_and_discards_the_rest() {
        let fs = n_files(4);
        let calls = std::cell::Cell::new(0);
        // Staging beats three times per file (sweep, create, one chunk: 1-12), the flush's step 1 once (13), entries 0
        // and 1 once each (14, 15): entry 2's publish heartbeat (16) fails.
        let beat = || {
            calls.set(calls.get() + 1);
            if calls.get() > 4 * 3 + 1 + 2 {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        let (r, out, got) = batched_with(&fs, policy(4), Durability::Strict, &unguarded, &beat);
        assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
        let c = log(&fs);
        assert_eq!(count(&c, "rename_"), 2, "entries 0 and 1 renamed: {c:?}");
        for f in ["f0", "f1"] {
            assert!(published(&fs, f) && claimed(&fs, f), "{f}: {c:?}");
        }
        for f in ["f2", "f3"] {
            assert!(!fs.exists(format!("/dst/{f}.flux-partial.op1")), "{f}: {c:?}");
            assert!(!fs.exists(format!("/dst/{f}")), "{f}");
        }
        assert_eq!(fs.prepared_count(), 0, "the unrenamed entries' notes are discarded");
        // Every temporary goes before the transaction that discards the notes.
        let settle = first(&c, "claim_apply_recovery");
        for f in ["f2", "f3"] {
            let removed =
                *positions(&c, &format!("remove_file(/dst/{f}.flux-partial.op1)")).last().unwrap();
            assert!(removed > renamed(&c, "f1") && removed < settle, "{f}: {c:?}");
        }
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(out.files_copied, 2);
    }

    #[test]
    fn a_stage_failure_mid_batch_does_not_lose_the_pending_entries() {
        let names: Vec<String> = (0..40).map(|i| format!("f{i:02}")).collect();
        let entries: Vec<(&str, &[u8])> =
            names.iter().map(|n| (n.as_str(), n.as_bytes())).collect();
        let fs = sources(&entries);
        // The 30th exclusive create is f29's.
        fs.fail_nth("create_new", 30, Code::DiskFull, ErrorKind::Other);
        let (r, out, got) = batched(&fs, policy(64), Durability::Strict);
        r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("f29"));
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!((e.code(), e.step), (Code::DiskFull, CopyStep::Create), "{e:?}");
        let c = log(&fs);
        assert_eq!(prepares(&c), ["claim_prepare_many(39)"], "{c:?}");
        assert!(
            created(&c, "f39") < first(&c, "claim_prepare_many("),
            "published after the directory ends"
        );
        for n in names.iter().filter(|n| *n != "f29") {
            assert!(published(&fs, n) && claimed(&fs, n), "{n}");
        }
        assert!(!fs.exists("/dst/f29"));
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(out.files_copied, 39);
    }

    #[test]
    fn an_abort_drains_every_frame() {
        let fs = sources(&[("a", b"a"), ("b", b"b"), ("m/x", b"x")]);
        let calls = std::cell::Cell::new(0);
        // a and b staged in the root (1-4), m's creation (5), x staged in m (6-7): m's `DirEnd` flush guard (8) fails
        // while the root holds a and b.
        let guard = failing_after(2 * 2 + 1 + 2, &calls);
        let (r, out, got) =
            batched_with(&fs, policy(64), Durability::Strict, &guard, &no_heartbeat);
        let error = r.unwrap_err();
        assert_eq!(error.code(), Code::TargetLockBusy, "{error:?}");
        // The walk's error is the one returned: the child's, at m's flush.
        assert_eq!(error.leftover.as_ref().map(|(p, _)| p.clone()), Some(temp_of("m/x")));
        let c = log(&fs);
        assert_eq!(count(&c, "rename_"), 0, "{c:?}");
        assert_eq!(count(&c, "claim_prepare_many("), 0, "{c:?}");
        // No pending entry is silently dropped: each staged temporary is still there and named as a leftover.
        for t in ["/dst/a.flux-partial.op1", "/dst/b.flux-partial.op1", "/dst/m/x.flux-partial.op1"]
        {
            assert!(fs.exists(t), "{t}");
        }
        assert_eq!(
            leftovers(&got, Some(&error)),
            [temp_of("a"), temp_of("b"), temp_of("m/x")],
            "{got:?}"
        );
        assert_eq!(out.files_copied, 0);
    }

    #[test]
    fn a_heartbeat_abort_drains_the_pending_temporaries_and_reports_nothing() {
        let fs = sources(&[("a", b"a"), ("b", b"b"), ("m/x", b"x")]);
        let calls = std::cell::Cell::new(0);
        // a and b staged in the root (beats 1-6); m's creation's heartbeat (7) fails, and every later one.
        let beat = || {
            calls.set(calls.get() + 1);
            if calls.get() > 2 * 3 {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        let (r, out, got) = batched_with(&fs, policy(64), Durability::Strict, &unguarded, &beat);
        assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
        let c = log(&fs);
        assert!(created(&c, "a") < created(&c, "b"), "both staged before the abort: {c:?}");
        // The drain's flush fails its opening heartbeat with the lock held: no note exists, so each temporary goes.
        for t in ["/dst/a.flux-partial.op1", "/dst/b.flux-partial.op1"] {
            assert!(!fs.exists(t), "{t}: {c:?}");
        }
        assert_eq!(count(&c, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(fs.prepared_count(), 0);
        assert!(!fs.exists("/dst/m"));
        // An abort, never a per-file failure: the drain's own stop (no leftover) is dropped (spec decision 8).
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_copied, out.failures.total()), (0, 0));
    }

    #[test]
    fn the_count_is_taken_at_the_rename() {
        let fs = n_files(4);
        let calls = std::cell::Cell::new(0);
        // Entry 2's publish guard (8 staging + 1 + 2 = 12th) fails.
        let guard = failing_after(4 * 2 + 1 + 2, &calls);
        let (r, out, _) = batched_with(&fs, policy(4), Durability::Strict, &guard, &no_heartbeat);
        assert_eq!(r.unwrap_err().code(), Code::TargetLockBusy);
        assert_eq!(count(&log(&fs), "rename_"), 2);
        assert_eq!(out.files_copied, 2);
    }

    /// A file `M` and a directory `m/` (holding `x`) in one source directory, onto a case-folding destination.
    fn file_then_folding_dir() -> FaultFs {
        let fs = sources(&[("M", b"upper"), ("m/x", b"x")]);
        fs.set_case_insensitive(true);
        fs
    }

    #[test]
    fn a_directory_whose_name_folds_to_a_pending_file_flushes_first() {
        let fs = file_then_folding_dir();
        let (r, out, got) = batched(&fs, policy(64), Durability::Strict);
        r.unwrap();
        let fs1 = file_then_folding_dir();
        let (r1, out1, got1) = batched(&fs1, policy(1), Durability::Strict);
        r1.unwrap();
        assert!(!got1.is_empty(), "the fixture folds: {got1:?}");
        assert_eq!(
            shape(&got),
            shape(&got1),
            "the same reports, code and step, as the unbatched run"
        );
        assert_eq!(fs.read_file("/dst/M"), fs1.read_file("/dst/M"));
        assert_eq!(
            (out.files_copied, out.directories_created, out.failures.total()),
            (out1.files_copied, out1.directories_created, out1.failures.total())
        );
    }
}
