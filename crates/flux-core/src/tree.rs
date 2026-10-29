// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Recursive copy (cut 4b): `copy_tree` drives the ordered walk and `copy_file_at`,
//! writing only through destination directory handles (§149.7).
//!
//! Design authority: `docs/superpowers/specs/2026-09-26-cut-4b-copy-tree-design.md`.

pub use crate::checkpoint::Discard;
use crate::checkpoint::{Checkpoints, FileCkpt, Hook};
use crate::copy::{
    BeforeCreate, BeforePublish, CopyError, CopyStep, FilePlan, Guard, Heartbeat, PublishIntent,
    Reuse, Stage, Staged, SyncAt, copy_file_checkpointed, discard, identity_gate, no_before_create,
    no_before_publish, no_heartbeat, plan_file, publish_staged, split_destination, stage_file,
    sync_staged_many, unguarded, weaker,
};
use crate::dry_run::{PlannedAction, PlannedEntry, PlannedObject, SkipReason};
use crate::hardlinks::{Candidacy, anchor_parent, candidacy, shown, strong};
use crate::names::{NameIndex, Resolved};
use crate::state::{FLUX_DIR, RESERVED_DIRS, identity_text, native_hex};
use crate::walk::{DEFAULT_MAX_DEPTH, MountPolicy, Walk, WalkEvent, WalkOptions, walk_with};
use flux_fs::{
    ClaimKey, ClaimOutcome, ClaimRecord, ClaimStatus, ClaimStore, Code, CopyOptions,
    DestinationRoot, DirHandle, Durability, ExistingPolicy, FileIdentity, FileSystem, FileType,
    FluxPathKey, FsError, HardlinkEntry, HardlinkIndex, Hardlinks, Links, Metadata,
    MetadataFailure, MountRoot, ObjectId, PreparedRecord, Preserve, Publish, RecoveryOp, Safety,
};
use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// What a tree copy did, counted. Individual failures are STREAMED to the sink as
/// they happen, never accumulated here, so a tree with a million failures costs what a
/// clean one does.
#[derive(Debug, Default)]
pub struct TreeOutcome {
    pub files_copied: u64,
    /// Copy verification: files of `files_copied` whose bytes were re-read and compared before publication.
    pub files_verified: u64,
    /// Copy verification: files that failed with `VerifyMismatch` (also counted in `failures.copy`).
    pub files_mismatched: u64,
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
    /// Cut 10a: symlinks left out under `--links=skip` (§124). Not failures; the CLI adds them into `files_skipped`.
    pub symlinks_skipped: u64,
    /// Cut 10b: source mount roots created as empty directories and not descended into (section 42). Not failures; the
    /// CLI reports them as `mounts_skipped`, apart from `files_skipped`.
    pub mounts_skipped: u64,
    /// Cut 16: entries the `--exclude` patterns removed; `--no-recursive` removals are not counted.
    pub excluded: u64,
    /// Part M: pre-existing destination directories whose mount-root query could not tell, merged into under
    /// `Safety::Default`. The example is relative to the source root, as `warnings` are.
    pub mount_unknown: Option<DegradedGroup>,
    /// Cut 10b: source directories whose mount-root query could not tell, descended into under `Safety::Default`
    /// (`Safety::Strict` refuses). The example is relative to the source root.
    pub source_mount_unknown: Option<DegradedGroup>,
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
    /// Cut 9e: the longest data-sync barrier of a Strict tree's batches (wall time from the first worker's start to the
    /// last result). Zero when no batch synced.
    pub barrier_max: std::time::Duration,
    /// Cut 9e: batches flushed because the open-writer cap was reached (`BatchPolicy::open_writers`): at it, the
    /// shallowest pending batch flushes before the next file is staged. A cap flush that stops the run still counts.
    pub cap_flushes: u64,
    /// Cut 9f-1: the bytes proven by validated checkpoints and not re-copied, summed over the published files.
    pub bytes_reused: u64,
    /// Cut 9f-1: published files that reused a validated prefix (`reused > 0`); `partial_resumes.len()`.
    pub files_partially_resumed: u64,
    /// Cut 9f-1: each partially resumed file with the bytes it reused and the bytes this run copied for it.
    pub partial_resumes: Vec<PartialResume>,
    /// Cut 9f-1: every published file whose checkpoints were discarded, and why (nothing is discarded silently).
    pub checkpoint_discards: Vec<CheckpointDiscard>,
    /// Cut 12: files recreated as a hardlink to their group's anchor. Not in `files_copied` or `bytes_copied`.
    pub files_hardlinked: u64,
    /// Cut 12: members of a hardlink group copied independently, whatever the reason.
    pub hardlinks_degraded: u64,
    /// Cut 12: members whose anchor could not be checked (the design's "cannot be checked" row): count and first example.
    pub hardlinks_unchecked: Option<DegradedGroup>,
    /// Cut 12: degradations caused by a weak or unavailable identity, aggregated as `warnings` is.
    pub hardlink_weak: WeakIdentityWarnings,
    /// Cut 12: the degradations that are NOT weak identity, one count and first example per cause.
    pub hardlinks_unlinked: BTreeMap<UnlinkedWhy, DegradedGroup>,
    /// The live counters a display samples; `None` outside the CLI's terminal case.
    pub progress: Option<std::sync::Arc<crate::progress::Progress>>,
}

impl TreeOutcome {
    /// Count one more entry the walk yielded, and tick the live view with it.
    pub(crate) fn note_discovered(&mut self) {
        self.files_total += 1;
        if let Some(p) = &self.progress {
            p.add_discovered();
        }
    }

    /// Count one more file copied, and tick the live view with it.
    pub(crate) fn note_copied(&mut self) {
        self.files_copied += 1;
        if let Some(p) = &self.progress {
            p.add_copied();
        }
    }

    /// Count one more entry resumed, and tick the live view with it.
    pub(crate) fn note_resumed(&mut self) {
        self.files_resumed += 1;
        if let Some(p) = &self.progress {
            p.add_resumed();
        }
    }
}

/// Cut 12: why a hardlink group member was copied independently when its identity was fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnlinkedWhy {
    /// The destination refused the link (`HARDLINK_UNAVAILABLE`).
    LinkRefused,
    /// The anchor's directory could not be opened for linking.
    AnchorUnavailable,
    /// The existing-file policy split the group.
    PolicySplit,
    /// The hardlink index failed to answer or record.
    IndexFailure,
}

/// Cut 9f-1: a file resumed from a validated prefix. `path` is relative to the source root, as the walk reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialResume {
    pub path: PathBuf,
    pub reused: u64,
    pub recopied: u64,
}

/// Cut 9f-1: a file whose checkpoints were discarded (wholly or from an offset on). `path` is relative to the source
/// root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointDiscard {
    pub path: PathBuf,
    pub reason: Discard,
}

/// Cut 9f-1: `copy_file_checkpointed`'s result split into the `Outcome` the tree counts and, for a published file, its
/// reuse with the bytes this run copied for it.
fn split_reuse(
    result: std::result::Result<(flux_fs::Outcome, Reuse), CopyError>,
) -> (std::result::Result<flux_fs::Outcome, CopyError>, Option<(Reuse, u64)>) {
    match result {
        Ok((o, reuse)) => {
            let recopied = o.bytes_copied;
            (Ok(o), Some((reuse, recopied)))
        }
        Err(e) => (Err(e), None),
    }
}

/// Cut 9f-1: records a published file's reuse and discard in the outcome.
fn note_reuse(out: &mut TreeOutcome, path: &Path, reuse: Reuse, recopied: u64) {
    if reuse.reused > 0 {
        out.bytes_reused += reuse.reused;
        out.files_partially_resumed += 1;
        out.partial_resumes.push(PartialResume {
            path: path.to_path_buf(),
            reused: reuse.reused,
            recopied,
        });
    }
    if let Some(reason) = reuse.discard {
        out.checkpoint_discards.push(CheckpointDiscard { path: path.to_path_buf(), reason });
    }
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
            || self.outcome.files_hardlinked > 0
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

/// One streamed record: every failure, plus the non-failure skips.
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
    /// NOT a failure: a device, socket or FIFO, skipped as §233.1 prescribes ("SKIP +
    /// DURABLE WARNING") and streamed so each one is reported by its own record. Not
    /// counted by `FailureTally`; `TreeOutcome` counts it in `special_files_skipped`.
    SpecialFileSkipped,
    /// NOT a failure (cut 10a, §124): a symlink left out under `--links=skip`, nothing read from it and nothing
    /// created. Not counted by `FailureTally`; `TreeOutcome` counts it in `symlinks_skipped`. A link that fails to be
    /// copied is a `Copy` failure like a file's.
    SymlinkSkipped,
    /// NOT a failure (cut 10b, section 42): a source directory that is a mount root, created at the destination as an
    /// empty directory and not descended into. Not counted by `FailureTally`; `TreeOutcome` counts it in
    /// `mounts_skipped`.
    MountSkipped,
    /// The file IS at the destination; some of its metadata could not be applied.
    /// Published with complaints, which exits 1 like the single-file case.
    PublishedWithComplaints(Vec<MetadataFailure>),
    /// The file IS published and counted; its claim was not recorded (for a directory flush failure the
    /// path is the directory). Counted in `FailureTally::total()` and `claim_not_recorded` but NOT in the report's
    /// `files_failed`, because the file was published and counted as copied.
    ClaimNotRecorded(FsError),
    /// NOT a failure (cut 11a): what a dry run would do with this entry. Dry mode only; not counted by
    /// `FailureTally`.
    Planned(crate::dry_run::PlannedEntry),
}

/// One counter per `TreeFailureCause` variant. Fixed size: it costs the same on a
/// clean tree and on one where every entry failed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FailureTally {
    pub walk: u64,
    pub create_dir: u64,
    pub copy: u64,
    pub published_with_complaints: u64,
    pub claim_not_recorded: u64,
}

impl FailureTally {
    /// The only total. Derived, never stored beside the parts.
    pub fn total(&self) -> u64 {
        self.walk
            + self.create_dir
            + self.copy
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
            // Not a failure (§233.1): `report` counts it in
            // `TreeOutcome::special_files_skipped`; named here so the match stays exhaustive.
            TreeFailureCause::SpecialFileSkipped => {}
            // Not a failure (§124): `report` counts it in `TreeOutcome::symlinks_skipped`.
            TreeFailureCause::SymlinkSkipped => {}
            // Not a failure (section 42): `report` counts it in `TreeOutcome::mounts_skipped`.
            TreeFailureCause::MountSkipped => {}
            TreeFailureCause::PublishedWithComplaints(_) => self.published_with_complaints += 1,
            TreeFailureCause::ClaimNotRecorded(_) => self.claim_not_recorded += 1,
            // Not a failure (cut 11a): a dry run's plan line; named here so the match stays exhaustive.
            TreeFailureCause::Planned(_) => {}
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
enum Frame<D: DirHandle> {
    Live {
        /// `None` only in dry mode: a directory that does not exist yet. Such a frame is `fresh` and holds no batch.
        dir: Option<D>,
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
        batch: Batch<D::Writer>,
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
    mounts: MountPolicy,
) -> std::result::Result<Source<'a, F>, CopyError> {
    // 1. The source root. `walk` refuses a missing or non-directory root: the whole
    //    operation failing, before any destination call.
    let events = walk_with(fs, src_root, WalkOptions { max_depth: DEFAULT_MAX_DEPTH, mounts })
        .map_err(|e| CopyError::at(CopyStep::Source, e))?;
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
    /// The run's claim store, open for the walk (cut 8b); `None` for a copy that holds no lock; a dry run passes its
    /// null store or, under `--resume`, the copy of the prior's store (cut 11b).
    pub(crate) claims: Option<&'c std::cell::RefCell<dyn flux_fs::ClaimStore>>,
    /// Cut 9a: `--resume`: an own `Created` claim may skip its target.
    pub(crate) resume: bool,
    /// Cut 9d: when a Strict tree's per-directory batch flushes.
    pub(crate) batch: BatchPolicy,
    /// Cut 9f-1: the run's checkpoint log; `None` for a copy that does not checkpoint: a single-file copy, a run whose
    /// WAL could not be opened (a warning, and every large file recopies), and tests.
    pub(crate) ckpt: Option<&'c Checkpoints<F::Dir>>,
    /// Spec cut 11a: every destination write is skipped, the decisions are not.
    pub(crate) dry_run: bool,
    /// Cut 11b: the adopted prior's id under a dry `--resume`, for the partial check.
    pub(crate) dry_prior: Option<&'c str>,
    /// Cut 11b: counts the partials a dry `--resume` found beside their targets.
    pub(crate) dry_partials: Option<&'c std::cell::Cell<u64>>,
    /// Several sources: the root's destination name, prefixed to every path this walk KEYS (the claim target, the
    /// prepared note's directory, a leftover's path): those are relative to DEST, while the walk is relative to the
    /// root. `None` for a single-root run, whose keys stay byte-identical to the root-relative path. `TreeFailure.path`
    /// stays root-relative (display only).
    pub(crate) key_prefix: Option<&'c OsStr>,
    /// Cut 12: the run's hardlink index slot; `None` for `--hardlinks=copy` and a lockless `copy_tree`.
    pub(crate) hardlinks: Option<&'c crate::hardlinks::HardlinkSlot<F::Dir>>,
    /// Cut 12: a second handle on DEST for opening an anchor's directory; `None` for a dry run, a lockless copy, or a
    /// root that did not match the walk's (plan ruling 8).
    pub(crate) anchor_root: Option<&'c F::Dir>,
}

impl<'c, F: DestinationRoot> Shared<'c, F> {
    /// The same shared state for one more root of a several-source run: its source root and identity, and the
    /// destination name its keys are prefixed with. Everything else (the guard, the claim store, the batch policy) is
    /// the run's and is shared.
    pub(crate) fn for_root(
        &self,
        src_root: &'c Path,
        src_identity: FileIdentity,
        key_prefix: Option<&'c OsStr>,
    ) -> Shared<'c, F> {
        Shared {
            fs: self.fs,
            src_root,
            src_identity,
            opts: self.opts,
            guard: self.guard,
            beat: self.beat,
            claims: self.claims,
            resume: self.resume,
            batch: self.batch,
            ckpt: self.ckpt,
            dry_run: self.dry_run,
            dry_prior: self.dry_prior,
            dry_partials: self.dry_partials,
            key_prefix,
            hardlinks: self.hardlinks,
            anchor_root: self.anchor_root,
        }
    }

    /// `rel` (relative to the root) as a path relative to DEST: under the root's prefix when it has one.
    pub(crate) fn keyed(&self, rel: &Path) -> PathBuf {
        keyed_under(self.key_prefix, rel)
    }
}

/// `prefix` joined to `rel`, or `rel` itself without a prefix (a single-root run keys exactly the root-relative path).
fn keyed_under(prefix: Option<&OsStr>, rel: &Path) -> PathBuf {
    match prefix {
        Some(p) => Path::new(p).join(rel),
        None => rel.to_path_buf(),
    }
}

/// Cut 9d (spec decision 5): a batch flushes at this many staged files...
pub(crate) const BATCH_FILES: usize = 64;
/// ...or this many staged source bytes (section 148.3's threshold); a file at least this long is staged alone...
pub(crate) const BATCH_BYTES: u64 = 64 << 20;
/// ...or when, at the next staging, the batch is at least this old.
pub(crate) const BATCH_AGE: std::time::Duration = std::time::Duration::from_secs(1);
/// `BatchPolicy::DEFAULT.files`: batching is on. (1 would be the unbatched cut 9c path, plan ruling 1; tests reach it
/// with an explicit policy.)
const DEFAULT_FILES: usize = BATCH_FILES;
/// Cut 9e: the data-sync barrier's worker threads (spec decision 2): at most this many `fsync`s in flight at once.
pub(crate) const SYNC_THREADS: usize = 16;
/// Cut 9e (spec decision 7): the most staged files whose writers are held open at once across the walk's whole frame
/// stack; each pending entry holds one descriptor until its batch's barrier.
pub(crate) const OPEN_WRITERS_MAX: usize = 256;

/// Cut 9e: the open-writer cap for a soft descriptor limit: a quarter of it (the rest is left for the walk's directory
/// handles, the claim store, the source reads and the barrier), at most `OPEN_WRITERS_MAX`, at least 1. `None` (no limit
/// known, as on Windows) is `OPEN_WRITERS_MAX`.
pub(crate) fn open_writers_cap(soft_limit: Option<u64>) -> usize {
    match soft_limit {
        None => OPEN_WRITERS_MAX,
        Some(limit) => usize::try_from(limit / 4).unwrap_or(usize::MAX).clamp(1, OPEN_WRITERS_MAX),
    }
}

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
    /// Cut 9e: the barrier's worker threads (`SYNC_THREADS`); a test uses 1 so the sync order is the entries' order.
    pub sync_threads: usize,
    /// Cut 9e: how often the waiting main thread wakes to beat during the barrier: half the run's heartbeat interval
    /// (`Pulse::beat` writes only once a full interval has passed since its last write). Never zero by construction:
    /// `DEFAULT` is a non-zero constant and `batch_policy` clamps the halved interval to at least 1 ms.
    pub beat_every: std::time::Duration,
    /// Cut 9e: the most writers held open across the frame stack (`open_writers_cap`); at it, the shallowest pending
    /// batch flushes before the next file is staged. Never zero.
    pub open_writers: usize,
}

impl BatchPolicy {
    pub(crate) const DEFAULT: BatchPolicy = BatchPolicy {
        files: DEFAULT_FILES,
        bytes: BATCH_BYTES,
        age: BATCH_AGE,
        now: std::time::Instant::now,
        fold_precheck: true,
        sync_threads: SYNC_THREADS,
        beat_every: std::time::Duration::from_millis(
            crate::run::HEARTBEAT_INTERVAL.as_secs() * 500,
        ),
        open_writers: OPEN_WRITERS_MAX,
    };
}

/// Cut 9d: one staged file waiting in its directory's batch for the flush that notes and publishes it.
struct Pending<W> {
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
    staged: Staged<W>,
    target: FluxPathKey,
    /// `Plan::Replace`'s `stored` and `meta`; `None` for a new file.
    replaced: Option<(OsString, Metadata)>,
}

/// Cut 9d: a directory's staged files (spec decision 3), the staged source bytes, when the first was staged, and the
/// ASCII-folded names of the pending entries (decision 6).
struct Batch<W> {
    pending: Vec<Pending<W>>,
    bytes: u64,
    since: Option<std::time::Instant>,
    folded: HashSet<Vec<u8>>,
}

// Manual, so that no `W: Default` bound appears (the writer is never constructed here).
impl<W> Default for Batch<W> {
    fn default() -> Self {
        Batch { pending: Vec::new(), bytes: 0, since: None, folded: HashSet::new() }
    }
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
    let mut source = prepare_source(fs, src_root, dst_root, MountPolicy::of(opts))?;
    source.events.set_selection(&opts.selection);

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
        ckpt: None,
        dry_run: false,
        dry_prior: None,
        dry_partials: None,
        key_prefix: None,
        hardlinks: None,
        anchor_root: None,
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
    let (root, walked) = copy_tree_frames(
        cx,
        events,
        Some(root),
        RootIdentity::Existing(root_identity),
        root_created,
        out,
        on_report,
    );
    (root.expect("a live root frame keeps its handle"), walked)
}

/// How one root of a several-source run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootStatus {
    /// Walked to the end, with no failure counted over it (the `out.failures.total()` delta is zero) and no abort.
    Completed,
    /// Walked (or tried to) and any failure was counted over it (`FailureTally::total()`: a file, a walk or
    /// directory error, a metadata complaint, an unrecorded claim), as a one-source run exits 1 on any of them, or the
    /// root itself could not be created or entered; a root that aborts after beginning is `Failed`.
    Failed,
    /// Nothing of this root was begun: an earlier root aborted the operation, or the heartbeat or guard check before
    /// this root failed.
    NotStarted,
}

/// One root of a several-source run, ready to copy.
pub(crate) enum RootInput<'a, F: FileSystem> {
    /// A directory: `DEST/<name>` is made (or merged into) and the source tree walks into it.
    Folder { name: OsString, src_root: PathBuf, source: Source<'a, F> },
    /// A file: copied to `DEST/<name>` through the per-file path of a tree, with DEST as the parent directory.
    File { name: OsString, src_parent: PathBuf, identity: FileIdentity },
}

impl<F: FileSystem> RootInput<'_, F> {
    fn name(&self) -> &OsStr {
        match self {
            RootInput::Folder { name, .. } | RootInput::File { name, .. } => name,
        }
    }
}

/// What one root did: the deltas of the run's counters over that root.
pub(crate) struct RootDone {
    pub name: OsString,
    pub status: RootStatus,
    pub files_copied: u64,
    pub files_failed: u64,
}

/// Several sources, spec 3.6: the roots in the order given (the caller sorts them by prefix key), each into DEST under
/// its name. Before each root `beat` and `guard` run, as before a directory. A failed heartbeat or guard before a root leaves that root and every
/// later one `NotStarted`. Any other abort-class error (a lost lock mid-walk, a root that cannot be created) stops the
/// loop: that root, which had begun, is `Failed` and every later one `NotStarted`. ANY counted failure (the `out.failures.total()` delta over the root) marks its root `Failed` and the loop goes
/// on; an abort marks it `Failed` too. `TreeFailure.path` is relative to
/// DEST for a folder root's entries (`<name>/<path>`) and is the name itself for a file root. `dest` is
/// `RootsDest::Existing` in a live run; `RootsDest::Planned(anchor)` only in a dry run whose DEST is absent, where every
/// root is planned against `anchor`'s identity.
pub(crate) fn copy_roots_at<F: DestinationRoot>(
    base: &Shared<'_, F>,
    dest: RootsDest<'_, F::Dir>,
    dest_created: bool,
    roots: Vec<RootInput<'_, F>>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> (Vec<RootDone>, std::result::Result<(), CopyError>) {
    let mut done = Vec::with_capacity(roots.len());
    let mut abort: Option<CopyError> = None;
    // The DEST-level state the file roots share (one frame's worth), made at the first file root.
    let mut files: Option<RootState> = None;
    for root in roots {
        let name = root.name().to_os_string();
        if abort.is_some() {
            done.push(RootDone {
                name,
                status: RootStatus::NotStarted,
                files_copied: 0,
                files_failed: 0,
            });
            continue;
        }
        // The pre-root heartbeat and guard: a failure here means the root has begun nothing.
        if let Err(e) = beat_and_guard(base) {
            done.push(RootDone {
                name,
                status: RootStatus::NotStarted,
                files_copied: 0,
                files_failed: 0,
            });
            abort = Some(e);
            continue;
        }
        let (copied, failed, any) = (out.files_copied, out.failures.copy, out.failures.total());
        let result = match root {
            RootInput::Folder { name, src_root, source } => {
                folder_root(base, &dest, &name, &src_root, source, out, on_report)
            }
            RootInput::File { name, src_parent, identity } => file_root(
                base,
                &dest,
                dest_created,
                &mut files,
                &name,
                &src_parent,
                identity,
                out,
                on_report,
            ),
        };
        let files_failed = out.failures.copy - failed;
        let status = if result.is_err() || out.failures.total() > any {
            RootStatus::Failed
        } else {
            RootStatus::Completed
        };
        done.push(RootDone { name, status, files_copied: out.files_copied - copied, files_failed });
        if let Err(e) = result {
            abort = Some(e);
        }
    }
    (done, abort.map_or(Ok(()), Err))
}

/// `beat`, then `guard`, as `walk_into` runs them before a directory; `copy_roots_at` runs them before each root.
fn beat_and_guard<F: DestinationRoot>(base: &Shared<'_, F>) -> std::result::Result<(), CopyError> {
    (base.beat)().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
    (base.guard)().map_err(|e| CopyError::at(CopyStep::Create, e))
}

/// The DEST `copy_roots_at` copies into: the directory itself, or (a dry run only) one that does not exist yet,
/// carrying the identity its roots are judged with (the holder's, kept for its strength only).
pub(crate) enum RootsDest<'a, D> {
    Existing(&'a D),
    Planned(FileIdentity),
}

/// A dry run's folder root: `DEST/<name>` looked at, never made. A name that is not there is a planned directory,
/// counted and streamed under `<name>` itself; the walk then plans its entries against what exists.
fn dry_folder_root<F: DestinationRoot>(
    base: &Shared<'_, F>,
    dest: &RootsDest<'_, F::Dir>,
    name: &OsStr,
    src_root: &Path,
    source: Source<'_, F>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    let (dir, identity) = match dest {
        RootsDest::Existing(d) => match d.open_dir(name) {
            Ok(dir) => {
                let id = dir.identity().map_err(|e| CopyError::at(CopyStep::Resolve, e))?;
                (Some(dir), RootIdentity::Existing(id))
            }
            Err(e) if e.source.kind() == ErrorKind::NotFound => {
                let id = d.identity().map_err(|e| CopyError::at(CopyStep::Resolve, e))?;
                (None, RootIdentity::Planned(id))
            }
            Err(e) => return Err(CopyError::at(CopyStep::Create, e)),
        },
        RootsDest::Planned(anchor) => (None, RootIdentity::Planned(*anchor)),
    };
    if dir.is_none() {
        out.directories_created += 1;
        report(
            out,
            on_report,
            PathBuf::from(name),
            TreeFailureCause::Planned(PlannedEntry {
                object: PlannedObject::Dir,
                action: PlannedAction::CreateDir,
            }),
        );
    }
    let cx = base.for_root(src_root, source.identity, Some(name));
    let mut under_name = |mut f: TreeFailure| {
        f.path = Path::new(name).join(&f.path);
        on_report(f);
    };
    copy_tree_frames(&cx, source.events, dir, identity, false, out, &mut under_name).1
}

/// A folder root: `DEST/<name>` made or merged into, then the tree walk, its failures reported under `<name>`.
fn folder_root<F: DestinationRoot>(
    base: &Shared<'_, F>,
    dest: &RootsDest<'_, F::Dir>,
    name: &OsStr,
    src_root: &Path,
    source: Source<'_, F>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    if base.dry_run {
        return dry_folder_root(base, dest, name, src_root, source, out, on_report);
    }
    let RootsDest::Existing(dest) = dest else {
        unreachable!("a planned DEST is a dry run's");
    };
    let (dir, created) = match dest.create_dir(name) {
        Ok(dir) => {
            out.directories_created += 1;
            (dir, true)
        }
        // A copy merges onto an existing folder, as a tree onto an existing DEST.
        Err(e) if e.source.kind() == ErrorKind::AlreadyExists => {
            (dest.open_dir(name).map_err(|e| CopyError::at(CopyStep::Create, e))?, false)
        }
        Err(e) => return Err(CopyError::at(CopyStep::Create, e)),
    };
    let cx = base.for_root(src_root, source.identity, Some(name));
    let mut under_name = |mut f: TreeFailure| {
        f.path = Path::new(name).join(&f.path);
        on_report(f);
    };
    copy_tree_at(&cx, source.events, dir, created, out, &mut under_name).1
}

/// A file root: the file `src_parent/<name>` copied to `DEST/<name>` by the per-file path of a tree, DEST being the
/// parent directory. The walk-relative path is the single component `<name>`, and the keys carry no prefix: the file's
/// name IS the key (`FluxPathKey::from_relative("<name>")`).
// One input per piece of the root's state, as `copy_one`'s.
#[allow(clippy::too_many_arguments)]
fn file_root<F: DestinationRoot>(
    base: &Shared<'_, F>,
    dest: &RootsDest<'_, F::Dir>,
    dest_created: bool,
    state: &mut Option<RootState>,
    name: &OsStr,
    src_parent: &Path,
    identity: FileIdentity,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    let state = match state {
        Some(state) => state,
        None => match dest {
            RootsDest::Existing(dest) => {
                let dest_identity =
                    dest.identity().map_err(|e| CopyError::at(CopyStep::Resolve, e))?;
                state.insert(root_state(
                    base,
                    Some(dest),
                    RootIdentity::Existing(dest_identity),
                    dest_created,
                    out,
                )?)
            }
            RootsDest::Planned(anchor) => state.insert(root_state(
                base,
                None,
                RootIdentity::Planned(*anchor),
                dest_created,
                out,
            )?),
        },
    };
    let parent = match dest {
        RootsDest::Existing(dest) => Some(*dest),
        RootsDest::Planned(_) => None,
    };
    let cx = base.for_root(src_parent, identity, None);
    let RootState { names, claim_parent, fresh } = state;
    let mut batch = Batch::default();
    out.note_discovered();
    let path = PathBuf::from(name);
    let walked = if reserved_path(&path) {
        let conflict = CopyError::at(CopyStep::Gate, reserved_conflict());
        report(out, on_report, path, TreeFailureCause::Copy(conflict));
        Ok(())
    } else {
        copy_one(&cx, parent, names, &mut batch, *claim_parent, *fresh, path, out, on_report)
    };
    // The root has no `DirEnd`, so its batch is drained here, as `copy_tree_frames` drains its frames. A planned DEST
    // is a dry run's, which never batches.
    let mut mode = drain_mode(&walked);
    let mut first = None;
    if let Some(dest) = parent {
        drain_one(
            &cx,
            dest,
            names,
            &mut batch,
            out,
            on_report,
            walked.is_err(),
            &mut mode,
            &mut first,
        );
    }
    walked?;
    if let Some(e) = first {
        return Err(e);
    }
    // ...and its claims are flushed as a `DirEnd` flushes a directory's: after §101 and §99.
    if claim_parent.is_some()
        && let Some(claims) = cx.claims
    {
        beat_and_guard(&cx)?;
        let flushed = claims.borrow_mut().flush();
        if let Err(e) = flushed {
            report(out, on_report, PathBuf::from(name), TreeFailureCause::ClaimNotRecorded(e));
        }
    }
    Ok(())
}

/// What the dynamic section 129 check compares a source subdirectory with (cut 11a).
#[derive(Clone, Copy)]
pub(crate) enum RootIdentity {
    /// The destination root exists: equality and strength both count.
    Existing(FileIdentity),
    /// Dry mode, the destination root does not exist: the identity is its parent's, kept for its STRENGTH only, and
    /// nothing equals a directory that does not exist.
    Planned(FileIdentity),
}

impl RootIdentity {
    /// The identity itself, whichever way it was obtained.
    fn inner(self) -> FileIdentity {
        match self {
            RootIdentity::Existing(i) | RootIdentity::Planned(i) => i,
        }
    }
}

/// `copy_tree_at`'s body with an optional root: `None` is a dry run's destination root that does not exist yet (its
/// frame is `fresh` and holds no handle). The root handle is handed back as it came.
pub(crate) fn copy_tree_frames<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    mut events: Walk<'_, F>,
    root: Option<F::Dir>,
    root_identity: RootIdentity,
    root_created: bool,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> (Option<F::Dir>, std::result::Result<(), CopyError>) {
    let state = match root_state(cx, root.as_ref(), root_identity, root_created, out) {
        Ok(state) => state,
        Err(e) => return (root, Err(e)),
    };
    let mut stack = vec![Frame::Live {
        dir: root,
        created: HashSet::new(),
        names: state.names,
        claim_parent: state.claim_parent,
        fresh: state.fresh,
        batch: Batch::default(),
    }];
    let walked = walk_into(cx, &mut events, root_identity, &mut stack, out, on_report);
    // Cut 16 (section 5.9): counted here, before the drain, so every exit path of the walk carries it.
    out.excluded += events.excluded();
    // Cut 9d, spec decision 5(e): every live frame's batch is flushed whatever the walk returned; the walk's own error
    // wins over a stop of the drain.
    let mode = drain_mode(&walked);
    let drained = drain_batches(cx, &mut stack, out, on_report, walked.is_err(), mode);
    let walked = walked.and(drained);
    // The walk emits no `Dir` for the root, so no `DirEnd` pops it, and a frame is skipped only when pushed.
    match stack.into_iter().next() {
        Some(Frame::Live { dir, .. }) => (dir, walked),
        _ => unreachable!("the root frame is never popped, and never skipped"),
    }
}

/// What a root frame is made of: the names of the directory (listed once when it pre-exists), the identity claims
/// under it are keyed with, and whether this run created it. Shared by `copy_tree_frames` and the file roots of
/// `copy_roots_at`, which keep their DEST-level state without a frame because DEST is borrowed, not owned.
struct RootState {
    names: NameIndex,
    claim_parent: Option<ObjectId>,
    fresh: bool,
}

/// `copy_tree_frames`' set-up of its root frame, unchanged: `root` is `None` for a dry run's directory that does not
/// exist yet. An `Err` is an abort.
fn root_state<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    root: Option<&F::Dir>,
    root_identity: RootIdentity,
    root_created: bool,
    out: &mut TreeOutcome,
) -> std::result::Result<RootState, CopyError> {
    let claim_parent = match (cx.claims, root_identity.inner()) {
        (None, _) => None,
        (Some(_), FileIdentity::Strong(id)) => Some(id),
        (Some(_), _) => match cx.opts.safety {
            Safety::Default => {
                DegradedGroup::note(&mut out.replace_degraded, Path::new(""));
                None
            }
            Safety::Strict => {
                return Err(refuse(
                    "the destination's identity is not strong enough to key claims, which --safety strict refuses",
                ));
            }
        },
    };
    // A pre-existing root is listed once; a root this run made (or, dry, one that does not exist) is `fresh` and needs
    // no listing.
    let names = match root {
        Some(dir) if claim_parent.is_some() && !root_created => {
            NameIndex::for_existing(dir).map_err(|e| CopyError::at(CopyStep::Resolve, e))?
        }
        _ => NameIndex::for_new_dir(),
    };
    Ok(RootState { names, claim_parent, fresh: root_created || root.is_none() })
}

/// How the drain behaves after a walk that ended as `walked`.
fn drain_mode(walked: &std::result::Result<(), CopyError>) -> DrainMode {
    match walked {
        // Tested first: a lost lock noticed at a heartbeat step is still a lost lock.
        Err(e) if e.code() == Code::TargetLockBusy => DrainMode::KeepAll(copy_of(&e.cause)),
        Err(e) if e.step == CopyStep::Heartbeat => DrainMode::RemoveAll(copy_of(&e.cause)),
        // Cut 9d decision 3: a destination without a no-replace primitive stops the operation with the lock held.
        Err(e) if e.code() == Code::NoReplacePublishUnavailable => {
            DrainMode::RemoveAll(copy_of(&e.cause))
        }
        _ => DrainMode::Publish,
    }
}

/// `copy_tree_at`'s loop. Every `?` here is an abort.
fn walk_into<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    events: &mut Walk<'_, F>,
    root_identity: RootIdentity,
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
                } else if dry_top_level_flux(cx, stack, &path) {
                    // Cut 11a (ruling P11): the live run's own workspace already made DEST/.flux, so a source directory
                    // of that name at depth 1 MERGES live: no count, no record. Its children plan under a directory
                    // that is not looked at (a planned, fresh frame), their reserved names refused as ever.
                    let claim_parent = match stack.last() {
                        Some(Frame::Live { claim_parent, .. }) => *claim_parent,
                        _ => None,
                    };
                    Frame::Live {
                        dir: None,
                        created: HashSet::new(),
                        names: NameIndex::for_new_dir(),
                        claim_parent,
                        fresh: true,
                        batch: Batch::default(),
                    }
                } else if reserved_path(&path) {
                    let conflict = TreeFailureCause::CreateDir(reserved_conflict());
                    report(out, on_report, path.clone(), conflict);
                    Frame::Skipped
                } else {
                    // Cut 9d (decision 6, ASCII fold): a pending file whose name this directory's folds onto publishes
                    // first, so the directory meets it as an unbatched copy's would, instead of winning the name.
                    if let Some(Frame::Live { dir: Some(dir), names, batch, .. }) = stack.last_mut()
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
                        cx.dry_run,
                        out,
                        on_report,
                    )?
                };
                stack.push(frame);
            }
            WalkEvent::File { path } => {
                out.note_discovered();
                if dry_top_level_flux(cx, stack, &path) {
                    dry_flux_entry_failure(stack, path, out, on_report);
                    continue;
                }
                // Cut 9e (decision 7): the writers held open across every live frame stay under the cap. At it, the
                // shallowest frame with a pending entry flushes (the longest-waiting batch, and the one the walk is
                // least likely to add to), through the same `flush_batch` a `DirEnd` uses; each flush empties one batch.
                debug_assert!(cx.batch.open_writers >= 1);
                while pending_total(stack) >= cx.batch.open_writers {
                    // A frame with no directory (dry) holds no batch, so the one found here has its handle.
                    let Some(Frame::Live { dir: Some(dir), names, batch, .. }) =
                        stack.iter_mut().find(
                            |f| matches!(f, Frame::Live { batch, .. } if !batch.pending.is_empty()),
                        )
                    else {
                        break;
                    };
                    // Counted before the `?`: a cap flush that stops the run was still a cap flush.
                    out.cap_flushes += 1;
                    flush_batch(cx, dir, names, batch, out, on_report)?;
                }
                if let Some(Frame::Live { dir, names, claim_parent, fresh, batch, .. }) =
                    stack.last_mut()
                {
                    if reserved_path(&path) {
                        let conflict = CopyError::at(CopyStep::Gate, reserved_conflict());
                        report(out, on_report, path, TreeFailureCause::Copy(conflict));
                    } else {
                        copy_one(
                            cx,
                            dir.as_ref(),
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
                out.note_discovered();
                if dry_top_level_flux(cx, stack, &path) && cx.opts.links != Links::Skip {
                    dry_flux_entry_failure(stack, path, out, on_report);
                    continue;
                }
                // Cut 10a: a link holds no writer, so the open-writers cap above does not apply to it.
                if let Some(Frame::Live { dir, names, claim_parent, fresh, batch, .. }) =
                    stack.last_mut()
                {
                    link_one(
                        cx,
                        dir.as_ref(),
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
            WalkEvent::Other { path } => {
                out.note_discovered();
                if live {
                    report(out, on_report, path, TreeFailureCause::SpecialFileSkipped);
                }
            }
            WalkEvent::MountSkipped { path } => {
                if live {
                    report(out, on_report, path, TreeFailureCause::MountSkipped);
                }
            }
            WalkEvent::MountUnknown { path } => {
                if live {
                    match cx.opts.safety {
                        Safety::Default => {
                            DegradedGroup::note(&mut out.source_mount_unknown, &path)
                        }
                        Safety::Strict => {
                            return Err(refuse(
                                "whether a source directory is a mount root cannot be told with full confidence",
                            ));
                        }
                    }
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
                    match &dir {
                        Some(dir) => flush_batch(cx, dir, &mut names, &mut batch, out, on_report)?,
                        // A directory that does not exist yet (dry) staged nothing.
                        None => debug_assert!(batch.pending.is_empty()),
                    }
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

/// Cut 11a (ruling P11), dry mode only: `path` is the root's direct child named `.flux` (ASCII case-insensitive, as
/// `reserved_path` compares), met while only the root frame is on the stack, AND `.flux` does not exist at the
/// destination (DEST absent, or `open_dir` answers `NotFound`). The live run's workspace has already made
/// `DEST/.flux`, which a dry run never does, so such an entry is judged against what live meets there. When
/// `DEST/.flux` exists, the ordinary dry path applies: it opens the real directory and its entries resolve against
/// the real listing, as live's do.
fn dry_top_level_flux<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    stack: &[Frame<F::Dir>],
    path: &Path,
) -> bool {
    // A root of several sources has a prefix: its `.flux` is below `DEST/<name>`, not DEST's control directory.
    if !cx.dry_run || stack.len() != 1 || cx.key_prefix.is_some() {
        return false;
    }
    let mut parts = path.components();
    let named = matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(only)), None)
            if only.as_encoded_bytes().eq_ignore_ascii_case(FLUX_DIR.as_bytes())
    );
    if !named {
        return false;
    }
    match stack.last() {
        Some(Frame::Live { dir: None, .. }) => true,
        Some(Frame::Live { dir: Some(root), .. }) => {
            let name = path.file_name().expect("a walk path ends in a name");
            matches!(root.open_dir(name), Err(e) if e.source.kind() == ErrorKind::NotFound)
        }
        _ => false,
    }
}

/// A top-level source FILE or LINK named `.flux`, dry: live fails it because DEST/.flux is a directory. DEST present
/// means `copy_one` resolves a directory entry (`DESTINATION_ERROR`); DEST absent means a fresh root plans it New and
/// the identity gate refuses it (`SAFETY_REJECTED`). Counted in `failures.copy` as live's is; no `Planned` record.
fn dry_flux_entry_failure<D: DirHandle>(
    stack: &[Frame<D>],
    path: PathBuf,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) {
    let dest_exists = matches!(stack.last(), Some(Frame::Live { dir: Some(_), .. }));
    let code = if dest_exists { Code::DestinationError } else { Code::SafetyRejected };
    let cause = target_failure(CopyStep::Gate, code, "the destination is a directory");
    report(out, on_report, path, cause);
}

/// Cut 9e: the writers held open across the stack, one per pending entry of every live frame.
fn pending_total<D: DirHandle>(stack: &[Frame<D>]) -> usize {
    stack
        .iter()
        .map(|f| match f {
            Frame::Live { batch, .. } => batch.pending.len(),
            _ => 0,
        })
        .sum()
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

/// A `Dir` event under a live frame: the dynamic §129 check, then decision 8. In dry mode (cut 11a) nothing is created:
/// a directory that is absent, or sits under a frame with no directory, becomes a planned child with no handle.
// The signature is the plan's: ten inputs, each a distinct piece of walk state.
#[allow(clippy::too_many_arguments)]
fn enter_dir<D: DirHandle>(
    stack: &mut [Frame<D>],
    path: &Path,
    identity: FileIdentity,
    root_identity: RootIdentity,
    src_identity: FileIdentity,
    opts: &CopyOptions,
    claims_on: bool,
    dry_run: bool,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<Frame<D>, CopyError> {
    // The dynamic half of §129 / §149.6: a directory reached mid-walk that IS the
    // destination root means the source reached it by an alias. Checked before
    // anything is created for it. A planned root exists nowhere, so nothing equals it.
    match (identity, root_identity) {
        (FileIdentity::Strong(a), RootIdentity::Existing(FileIdentity::Strong(b))) if a == b => {
            return Err(refuse("a source directory is the destination itself, by identity"));
        }
        (
            FileIdentity::Strong(_),
            RootIdentity::Existing(FileIdentity::Strong(_))
            | RootIdentity::Planned(FileIdentity::Strong(_)),
        ) => {}
        (s, d) => match opts.safety {
            Safety::Default => out.warnings.record(weaker(s, d.inner()), path),
            Safety::Strict => {
                return Err(refuse(
                    "a source directory cannot be compared with the destination with full confidence",
                ));
            }
        },
    }

    let Some(Frame::Live { dir: parent, created, claim_parent: parent_claim, .. }) =
        stack.last_mut()
    else {
        unreachable!("enter_dir is called only under a live frame");
    };
    let name = path.file_name().expect("a walk path ends in a name");
    let (child, child_identity, pre_existing) = if dry_run {
        // Dry: look, never create. A name that is not there (or any name under a directory that is not there) is a
        // planned child; anything else the destination says is the live failure.
        let opened = match parent.as_ref() {
            None => None,
            Some(p) => match p.open_dir(name) {
                Ok(child) => Some(child),
                Err(e) if e.source.kind() == ErrorKind::NotFound => None,
                Err(e) => {
                    report(out, on_report, path.to_path_buf(), TreeFailureCause::CreateDir(e));
                    return Ok(Frame::Skipped);
                }
            },
        };
        match opened {
            Some(child) => {
                let child_identity = child.identity();
                (Some(child), child_identity, true)
            }
            None => {
                out.directories_created += 1;
                report(
                    out,
                    on_report,
                    path.to_path_buf(),
                    TreeFailureCause::Planned(PlannedEntry {
                        object: PlannedObject::Dir,
                        action: PlannedAction::CreateDir,
                    }),
                );
                // The identity a created child would have is unknowable; it takes the strength its parent keys claims
                // with, so a planned subtree is judged exactly as a created one is.
                let child_identity = Ok(match *parent_claim {
                    Some(id) => FileIdentity::Strong(id),
                    None => FileIdentity::Unavailable,
                });
                (None, child_identity, false)
            }
        }
    } else {
        let Some(parent) = parent.as_ref() else {
            unreachable!("only a dry run has a frame without a directory");
        };
        // Decision 8: create first; open on AlreadyExists. Neither call traverses a link.
        match parent.create_dir(name) {
            Ok(child) => {
                out.directories_created += 1;
                let child_identity = child.identity();
                if let Ok(FileIdentity::Strong(id)) = child_identity {
                    created.insert(id);
                }
                (Some(child), child_identity, false)
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
                        _ => (Some(child), child_identity, true),
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
        }
    };

    // A destination directory about to be entered must not BE the source root (owner, cut 4b capstone round 1):
    // a bind mount inside the destination can present the source under a destination name, and `open_dir`
    // refuses only name-surrogates. Aliases of source SUBdirectories stay the section 42 mount cut's. Before the
    // mount query, so a mount presenting the source root aborts rather than becoming a skipped subtree. A planned
    // child (dry) is nothing at the name, so there is nothing to compare.
    if child.is_some()
        && let (Some(FileIdentity::Strong(a)), FileIdentity::Strong(b)) =
            (child_identity.as_ref().ok().copied(), src_identity)
        && a == b
    {
        return Err(refuse("a destination directory is the source root itself, by identity"));
    }

    // Part M: a pre-existing destination directory that is the root of a mount is never merged into.
    if pre_existing && let (Some(child), Some(parent)) = (child.as_ref(), parent.as_ref()) {
        let refusal = match child.mount_root(parent) {
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
    let names = match &child {
        Some(dir) if pre_existing && claim_parent.is_some() => match NameIndex::for_existing(dir) {
            Ok(names) => names,
            Err(e) => {
                report(out, on_report, path.to_path_buf(), TreeFailureCause::CreateDir(e));
                return Ok(Frame::Skipped);
            }
        },
        _ => NameIndex::for_new_dir(),
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

/// The source stat `copy_one` shares between the hardlink candidacy, the checkpoint context, `--resume` and the
/// existing-file policy (plan ruling 2): one stat, made once, only when one of them needs it. The no-claims path
/// (`claims == false`) forces `Overwrite` and has no checkpoint or resume reader, so only the hardlink run needs it.
fn source_stat<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    src: &Path,
    claims: bool,
) -> std::result::Result<Option<Metadata>, CopyError> {
    let needed = cx.hardlinks.is_some()
        || (claims
            && (cx.ckpt.is_some() || cx.resume || cx.opts.existing != ExistingPolicy::Overwrite));
    if !needed {
        return Ok(None);
    }
    cx.fs.metadata(src).map(Some).map_err(|e| CopyError::at(CopyStep::Source, e))
}

/// Cut 12: the degradations that need no index, decided from the source's candidacy for a regular file when the run
/// has a hardlink slot. `true`: the entry is finished (a failure was reported, nothing is created); `false`: the file
/// copies as an ordinary one. `has_claims` is false in the no-claims path, where no anchor can be recorded.
fn hardlink_degradation<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    src_meta: Option<&Metadata>,
    has_claims: bool,
    path: &Path,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> bool {
    let (Some(_), Some(meta)) = (cx.hardlinks, src_meta) else { return false };
    let preserve = cx.opts.hardlinks == Hardlinks::Preserve;
    match candidacy(meta) {
        Candidacy::NotCandidate => false,
        Candidacy::Unchecked => {
            DegradedGroup::note(&mut out.hardlinks_unchecked, path);
            false
        }
        Candidacy::Weak(identity) => {
            if preserve {
                let cause = target_failure(
                    CopyStep::Gate,
                    Code::HardlinkIdentityUnavailable,
                    "the source filesystem reports no reliable identity for a multiply-linked file",
                );
                report(out, on_report, path.to_path_buf(), cause);
                return true;
            }
            out.hardlinks_degraded += 1;
            out.hardlink_weak.record(identity, path);
            false
        }
        Candidacy::Candidate(_) if !has_claims => {
            if preserve {
                let cause = target_failure(
                    CopyStep::Claim,
                    Code::HardlinkUnavailable,
                    "no claim store keys this directory, so no anchor can be recorded",
                );
                report(out, on_report, path.to_path_buf(), cause);
                return true;
            }
            degrade(out, path, UnlinkedWhy::AnchorUnavailable);
            false
        }
        // A candidate with claims meets its group's index entry in `copy_one` (`hardlink_group`, `meet_group`).
        Candidacy::Candidate(_) => false,
    }
}

/// A `File` event under a live frame. See the design, "The per-file protocol".
///
/// The tree decides what an existing destination means and tells the copy path `ExistingPolicy::Overwrite`, so the
/// copy path never skips on its own and never produces `Outcome.skipped` here; the tree counts skips itself.
// One input per piece of frame state the protocol reads.
#[allow(clippy::too_many_arguments)]
fn copy_one<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: Option<&F::Dir>,
    names: &mut NameIndex,
    batch: &mut Batch<F::Writer>,
    claim_parent: Option<ObjectId>,
    fresh: bool,
    path: PathBuf,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    let name_owned = path.file_name().expect("a walk path ends in a name").to_os_string();
    let name = name_owned.as_os_str();
    let src = cx.src_root.join(&path);
    // Taken once, before any closure that makes the copy call: the keyless branch and both copies below share it.
    let progress = out.progress.clone();
    let (Some(parent_id), Some(claims)) = (claim_parent, cx.claims) else {
        // No claims for this directory (a lockless copy, or an identity that cannot key them): exactly the cut 4b
        // behaviour. An existing name is a collision.
        let src_known = match source_stat(cx, &src, false) {
            Ok(m) => m,
            Err(e) => {
                report(out, on_report, path, TreeFailureCause::Copy(e));
                return Ok(());
            }
        };
        if hardlink_degradation(cx, src_known.as_ref(), false, &path, out, on_report) {
            return Ok(());
        }
        if cx.dry_run {
            return dry_stop_file(
                cx,
                parent,
                name,
                &src,
                path,
                Publish::NoReplace,
                false,
                out,
                on_report,
            )
            .map(|_| ());
        }
        let Some(parent) = parent else {
            unreachable!("only a dry run has a planned directory");
        };
        let opts = CopyOptions {
            existing: ExistingPolicy::Overwrite,
            publish: Publish::NoReplace,
            ..cx.opts.clone()
        };
        let published = copy_file_checkpointed(
            cx.fs,
            &src,
            parent,
            name,
            &opts,
            cx.guard,
            cx.beat,
            &no_before_create,
            &no_before_publish,
            None,
            progress.as_deref(),
        )
        .map(|(outcome, _)| outcome);
        return finish_copy(cx.key_prefix, path, published, out, on_report);
    };
    let target = match FluxPathKey::from_relative(&cx.keyed(&path)) {
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

    // Plan ruling 2: the source is stat'ed once, here, for every reader below.
    let src_known = match source_stat(cx, &src, true) {
        Ok(m) => m,
        Err(e) => {
            report(out, on_report, path, TreeFailureCause::Copy(e));
            return Ok(());
        }
    };
    // Cut 12: a link candidate meets its group's index entry here, once; it is never batched (plan ruling 3: a canonical
    // staged into a batch would publish after its dependents are met).
    let group = hardlink_group(cx, src_known.as_ref());

    // Cut 9d, spec decision 2: a Strict publication into a store with notes is staged into the directory's batch, unless
    // the policy turns batching off (plan ruling 1: `files == 1` is the per-file cut 9c path below).
    let batching = !cx.dry_run
        && group.is_none()
        && cx.batch.files > 1
        && cx.opts.durability == Durability::Strict
        && claims.borrow().supports_prepared();
    // Decisions 5(d) and 5(f): a batch older than the age, or a pending name this one folds onto, publishes first, so
    // the resolution below sees what an unbatched copy would.
    if batching
        && !batch.pending.is_empty()
        && let Some(parent) = parent
    {
        let aged = batch
            .since
            .is_some_and(|t| (cx.batch.now)().saturating_duration_since(t) >= cx.batch.age);
        if aged || (cx.batch.fold_precheck && batch.folded.contains(&folded(name))) {
            flush_batch(cx, parent, names, batch, out, on_report)?;
        }
    }

    // 1-2. Resolve the planned name to the entry it denotes.
    // A directory this run created plans every target as new (plan decision 6), and so does a planned one (dry).
    let resolved = if fresh {
        Ok(Ok(Resolved::Absent))
    } else {
        let Some(parent) = parent else { unreachable!("a planned directory is fresh") };
        names.resolve(parent, name)
    };
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
                return finish_copy(cx.key_prefix, path, Err(e), out, on_report);
            }
            // Cut 9a: `--resume`. The branch mutates nothing at the destination, so it neither beats nor guards.
            if cx.resume {
                let Some(src_meta) = src_known.as_ref() else {
                    unreachable!("a resuming run reads the source up front");
                };
                let found = claims.borrow().get(&ClaimKey::new(parent_id, &stored));
                match found {
                    Err(e) => {
                        return finish_copy(
                            cx.key_prefix,
                            path,
                            Err(CopyError::at(CopyStep::Claim, e)),
                            out,
                            on_report,
                        );
                    }
                    Ok(Some(r)) if r.target == target => {
                        if r.status == ClaimStatus::Created
                            && resumed_complete(src_meta, &meta, cx.opts.preserve_times)
                            && resume_keeps(cx, &group, &target, &meta, &path, out, on_report)
                        {
                            out.note_resumed();
                            out.bytes_resumed += src_meta.len;
                            names.record_publication(
                                &target,
                                Some(&stored),
                                name,
                                None,
                                meta.identity,
                            );
                            if cx.dry_run {
                                dry_resumed(out, on_report, path, PlannedObject::File);
                            }
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
                        return finish_copy(cx.key_prefix, path, Err(e), out, on_report);
                    }
                    Ok(None) => {}
                }
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
                    let Some(src_meta) = src_known.as_ref() else {
                        unreachable!("a non-overwrite policy reads the source up front");
                    };
                    let replace =
                        policy == ExistingPolicy::Update && update_replaces(src_meta, &meta);
                    (!replace).then_some(src_meta.len)
                }
            };
            if let Some(len) = skip {
                out.files_skipped += 1;
                out.bytes_skipped += len;
                if let Some(group) = &group {
                    skipped_member(cx, group, &target, &meta, &path, out);
                }
                if cx.dry_run {
                    // A skipped target never reaches the identity gate, live or dry.
                    let reason = if cx.opts.existing == ExistingPolicy::SkipExisting {
                        SkipReason::Existing
                    } else {
                        SkipReason::UpToDate
                    };
                    let skipped = PlannedEntry {
                        object: PlannedObject::File,
                        action: PlannedAction::Skip(reason),
                    };
                    report(out, on_report, path, TreeFailureCause::Planned(skipped));
                }
                return Ok(());
            }
            Plan::Replace { stored, meta }
        }
    };

    // Cut 12: a file that will be copied meets the degradations that need no index (a skipped one is untouched).
    if hardlink_degradation(cx, src_known.as_ref(), true, &path, out, on_report) {
        return Ok(());
    }
    // The group's entry decides a candidate's fate in a dry run too (cut 12 Task 8): a dependent is predicted a link
    // with no destination I/O, a canonical is planned as a copy and registered without a destination object.
    let mut meet = meet_group(cx, group, &path, out, on_report);
    if matches!(meet, Meet::Finished) {
        return Ok(());
    }
    if cx.dry_run {
        let (publish, replace) = match plan {
            Plan::New => (Publish::NoReplace, false),
            Plan::Replace { .. } => (Publish::Replace, true),
        };
        if matches!(meet, Meet::Link { .. }) {
            // A dry run cannot see a vanished anchor or a refused link, and does not predict them (the design).
            return dry_link(cx, name, path, replace, out, on_report);
        }
        let planned =
            dry_stop_file(cx, parent, name, &src, path.clone(), publish, replace, out, on_report)?;
        // No destination object exists in a dry run, so the anchor carries none; a canonical that failed to plan
        // blocks its dependents as a failed live copy does.
        let published = planned.then_some(FileIdentity::Unavailable);
        register_outcome(cx, meet, &target, published, &path, out, on_report);
        return Ok(());
    }
    let Some(parent) = parent else {
        unreachable!("only a dry run has a planned directory");
    };
    let own = |status| ClaimRecord { target: target.clone(), status };
    // Cut 9f-1: the file's checkpoint identity is the claim key of the TARGET name (never the stored name of a
    // replacement), built only for a run that checkpoints.
    // Only a file at or above the run's threshold gets a context (its length is read here, since the New path has not
    // looked at the source): a smaller file takes the unchanged path, with no extra guard. `stage_file` filters again.
    let file_ckpt = |key: &ClaimKey| {
        let run = cx.ckpt?;
        let len = src_known.as_ref()?.len;
        (len >= run.min_len()).then(|| FileCkpt {
            run,
            file_id: key.encode(),
            destination: cx.keyed(&path).as_os_str().as_encoded_bytes().to_vec(),
        })
    };
    // Cut 9c: a Strict publication into a store with the `prepared` table writes a note before its rename (the hook),
    // commits it with the claim after it, and discards it when the copy fails after the note. Otherwise: cut 9a.
    let strict = cx.opts.durability == Durability::Strict && claims.borrow().supports_prepared();
    let wrote = std::cell::Cell::new(false);
    match plan {
        // 3. Plan as new.
        Plan::New => {
            let key = ClaimKey::new(parent_id, name);
            let fc = file_ckpt(&key);
            let template = PreparedRecord {
                target: target.clone(),
                temp_name: Vec::new(),
                identity: String::new(),
                dir_path: note_dir(&cx.keyed(&path)),
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
                    cx,
                    parent,
                    names,
                    batch,
                    &src,
                    path,
                    publish,
                    no_claim,
                    fc.as_ref(),
                    out,
                    on_report,
                    pending,
                );
            }
            let hook = |intent: &PublishIntent| write_note(claims, &key, &template, &wrote, intent);
            let before_publish: &BeforePublish<'_> =
                if strict { &hook } else { &no_before_publish };
            if let Some((anchor, object)) = take_link(&mut meet) {
                let Some(src_meta) = src_known.as_ref() else {
                    unreachable!("a candidate's source was read up front");
                };
                let step = link_member(
                    cx,
                    parent,
                    names,
                    batch,
                    out,
                    on_report,
                    name,
                    src_meta,
                    &anchor,
                    object,
                    Publish::NoReplace,
                    &no_before_create,
                    before_publish,
                )?;
                match settle_link(cx, claims, &key, &wrote, &path, step, out, on_report)? {
                    Settled::Linked(o) => {
                        let identity = o.published_identity;
                        record_new(
                            claims, &key, &target, strict, names, name, identity, &path, out,
                            on_report,
                        );
                        count_linked(out, &o, &path);
                        return Ok(());
                    }
                    Settled::Finished => return Ok(()),
                    Settled::Copy => {}
                }
            }
            let opts = CopyOptions {
                existing: ExistingPolicy::Overwrite,
                publish: Publish::NoReplace,
                ..cx.opts.clone()
            };
            let result = copy_file_checkpointed(
                cx.fs,
                &src,
                parent,
                name,
                &opts,
                cx.guard,
                cx.beat,
                &no_before_create,
                before_publish,
                fc.as_ref().map(Hook::Logged),
                progress.as_deref(),
            );
            let (result, reuse) = split_reuse(result);
            let result = discard_note_on_failure(
                claims, &key, &wrote, cx.guard, &path, result, out, on_report,
            );
            let published = match &result {
                Ok(o) => Some(o.published_identity),
                Err(_) => None,
            };
            finish_copy(cx.key_prefix, path.clone(), result, out, on_report)?;
            if let Some((reuse, recopied)) = reuse {
                note_reuse(out, &path, reuse, recopied);
            }
            if let Some(identity) = published {
                record_new(
                    claims, &key, &target, strict, names, name, identity, &path, out, on_report,
                );
            }
            register_outcome(cx, meet, &target, published, &path, out, on_report);
            Ok(())
        }
        // 5. Replace, under a claim on the stored name made after the gate and the section 99 checks.
        Plan::Replace { stored, meta } => {
            let key_stored = ClaimKey::new(parent_id, &stored);
            let fc = file_ckpt(&ClaimKey::new(parent_id, name));
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
                dir_path: note_dir(&cx.keyed(&path)),
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
                    fc.as_ref(),
                    out,
                    on_report,
                    pending,
                );
            }
            let hook =
                |intent: &PublishIntent| write_note(claims, &key_stored, &template, &wrote, intent);
            let before_publish: &BeforePublish<'_> =
                if strict { &hook } else { &no_before_publish };
            if let Some((anchor, object)) = take_link(&mut meet) {
                let Some(src_meta) = src_known.as_ref() else {
                    unreachable!("a candidate's source was read up front");
                };
                let step = link_member(
                    cx,
                    parent,
                    names,
                    batch,
                    out,
                    on_report,
                    name,
                    src_meta,
                    &anchor,
                    object,
                    Publish::Replace,
                    &before_create,
                    before_publish,
                )?;
                match settle_link(cx, claims, &key_stored, &wrote, &path, step, out, on_report)? {
                    Settled::Linked(o) => {
                        let identity = o.published_identity;
                        record_replace(
                            claims,
                            parent_id,
                            &stored,
                            name,
                            &target,
                            strict,
                            meta.identity,
                            names,
                            identity,
                            &path,
                            out,
                            on_report,
                        );
                        count_linked(out, &o, &path);
                        return Ok(());
                    }
                    Settled::Finished => return Ok(()),
                    Settled::Copy => {}
                }
            }
            let opts = CopyOptions {
                existing: ExistingPolicy::Overwrite,
                publish: Publish::Replace,
                ..cx.opts.clone()
            };
            let result = copy_file_checkpointed(
                cx.fs,
                &src,
                parent,
                name,
                &opts,
                cx.guard,
                cx.beat,
                &before_create,
                before_publish,
                fc.as_ref().map(Hook::Logged),
                progress.as_deref(),
            );
            let (result, reuse) = split_reuse(result);
            let result = discard_note_on_failure(
                claims,
                &key_stored,
                &wrote,
                cx.guard,
                &path,
                result,
                out,
                on_report,
            );
            let published = match &result {
                Ok(o) => Some(o.published_identity),
                Err(_) => None,
            };
            finish_copy(cx.key_prefix, path.clone(), result, out, on_report)?;
            if let Some((reuse, recopied)) = reuse {
                note_reuse(out, &path, reuse, recopied);
            }
            if let Some(identity) = published {
                record_replace(
                    claims,
                    parent_id,
                    &stored,
                    name,
                    &target,
                    strict,
                    meta.identity,
                    names,
                    identity,
                    &path,
                    out,
                    on_report,
                );
            }
            register_outcome(cx, meet, &target, published, &path, out, on_report);
            Ok(())
        }
    }
}

/// Cut 12: records a published file's claim after the `Plan::New` arm's copy or link: `Created` (Strict: the prepared
/// note committed), then the name published for this target.
#[allow(clippy::too_many_arguments)]
fn record_new<C: ClaimStore + ?Sized>(
    claims: &std::cell::RefCell<C>,
    key: &ClaimKey,
    target: &FluxPathKey,
    strict: bool,
    names: &mut NameIndex,
    name: &OsStr,
    identity: FileIdentity,
    path: &Path,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) {
    let recorded = if strict {
        claims.borrow_mut().commit_prepared(key, target, None)
    } else {
        let created = ClaimRecord { target: target.clone(), status: ClaimStatus::Created };
        record_claim(claims, key, &created)
    };
    if let Err(e) = recorded {
        report(out, on_report, path.to_path_buf(), TreeFailureCause::ClaimNotRecorded(e));
    }
    names.record_publication(target, None, name, None, identity);
}

/// Cut 12: `record_new` for the `Plan::Replace` arm: counts the overwrite, upgrades the claim on the stored name (and
/// claims the planned one when it differs), and publishes the name, with the object it replaced.
#[allow(clippy::too_many_arguments)]
fn record_replace<C: ClaimStore + ?Sized>(
    claims: &std::cell::RefCell<C>,
    parent_id: ObjectId,
    stored: &OsStr,
    name: &OsStr,
    target: &FluxPathKey,
    strict: bool,
    replaced: FileIdentity,
    names: &mut NameIndex,
    identity: FileIdentity,
    path: &Path,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) {
    out.files_overwritten += 1;
    // The entry now holds the new object under its stored name (and, on some systems, the planned one).
    let key_stored = ClaimKey::new(parent_id, stored);
    let planned = (name != stored).then(|| ClaimKey::new(parent_id, name));
    let recorded = if strict {
        claims.borrow_mut().commit_prepared(&key_stored, target, planned.as_ref())
    } else {
        let mut recorded = claims.borrow_mut().upgrade_own_claim(&key_stored, target);
        if let Some(planned) = &planned {
            let created = ClaimRecord { target: target.clone(), status: ClaimStatus::Created };
            recorded = recorded.and(record_claim(claims, planned, &created));
        }
        recorded
    };
    if let Err(e) = recorded {
        report(out, on_report, path.to_path_buf(), TreeFailureCause::ClaimNotRecorded(e));
    }
    names.record_publication(target, Some(stored), name, strong(replaced), identity);
}

/// A linked member is counted `files_hardlinked`, never copied; the weaker comparison its gate made is recorded as a
/// copy's is.
fn count_linked(out: &mut TreeOutcome, linked: &flux_fs::Outcome, path: &Path) {
    out.files_hardlinked += 1;
    if let Some(weaker) = linked.identity_degraded {
        out.files_degraded += 1;
        out.warnings.record(weaker, path);
    }
}

/// Cut 12: how a hardlink candidate meets the copy arms, decided from its group's index entry.
enum Meet {
    /// Not a group member, or one that copies on its own without registering (an index failure under `auto`).
    Ordinary,
    /// A failure was reported; nothing more to do.
    Finished,
    /// Link to the anchor (`object` is its destination object when it was strong).
    Link { anchor: FluxPathKey, object: Option<ObjectId> },
    /// Copy; the result becomes the group's entry. `skipped`: how many members the existing-file policy skipped before
    /// this one, and the first of them.
    Canonical { id: ObjectId, skipped: Option<(u64, FluxPathKey)> },
}

/// The link a candidate is to make, taken out of `meet` (which stays as it is for any other case).
fn take_link(meet: &mut Meet) -> Option<(FluxPathKey, Option<ObjectId>)> {
    match std::mem::replace(meet, Meet::Ordinary) {
        Meet::Link { anchor, object } => Some((anchor, object)),
        other => {
            *meet = other;
            None
        }
    }
}

/// A candidate's source object and what the index said about it.
struct Group {
    id: ObjectId,
    entry: Lookup,
}

enum Lookup {
    /// The lookup failed (or the index never opened): the member cannot be checked (plan ruling 13).
    Unreadable,
    Found(Option<HardlinkEntry>),
}

/// The index lookup for a link candidate: `Some` for a regular file with `nlink > 1` and a `Strong` identity in a run
/// that links. A dry run looks up its own temporary index (cut 12 Task 8), so it predicts what a live run would do.
fn hardlink_group<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    meta: Option<&Metadata>,
) -> Option<Group> {
    let (Some(slot), Some(meta)) = (cx.hardlinks, meta) else { return None };
    let Candidacy::Candidate(id) = candidacy(meta) else { return None };
    let entry = match slot.with(|index| index.get(&id)) {
        Ok(found) => Lookup::Found(found),
        Err(_) => Lookup::Unreadable,
    };
    Some(Group { id, entry })
}

/// The `--resume` skip site for a completed destination (design "Resume (R2)"). `true`: skip it as resumed. `false`:
/// a candidate whose destination is not the anchor's object falls through to the policy decision and the link path.
#[allow(clippy::too_many_arguments)]
fn resume_keeps<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    group: &Option<Group>,
    target: &FluxPathKey,
    dest: &Metadata,
    path: &Path,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> bool {
    let Some(Group { id, entry }) = group else { return true };
    let skipped = match entry {
        // Plan ruling 13: an unreadable index is "cannot check". Under `preserve` the member is not skipped: the policy
        // and link path run and `meet_group` reports `HardlinkUnavailable`. Under `auto` it is skipped and counted. A dry
        // run's index is its temporary one, so an unreadable index is real there too and predicts the same.
        Lookup::Unreadable if cx.opts.hardlinks == Hardlinks::Preserve => {
            return false;
        }
        Lookup::Unreadable => {
            degrade(out, path, UnlinkedWhy::IndexFailure);
            return true;
        }
        Lookup::Found(None) => None,
        Lookup::Found(Some(HardlinkEntry::Skipped { members, example, .. })) => {
            Some((*members, example.clone()))
        }
        // A dry run decides these exactly as live does (its anchors carry the destination object when a resumed
        // canonical registered them, none when the walk planned them): a completed member that is another object than
        // the anchor falls through and is predicted `would_hardlink`.
        Lookup::Found(Some(HardlinkEntry::Anchor { object: Some(a), .. })) => {
            return strong(dest.identity) == Some(*a);
        }
        Lookup::Found(Some(HardlinkEntry::Anchor { object: None, .. })) => return false,
        Lookup::Found(Some(HardlinkEntry::Failed { .. })) => return false,
    };
    let meet = Meet::Canonical { id: *id, skipped };
    register_outcome(cx, meet, target, Some(dest.identity), path, out, on_report);
    true
}

/// `hardlinks_degraded` and the cause's count and first example, for `members` members at once.
fn degrade_many(out: &mut TreeOutcome, why: UnlinkedWhy, members: u64, example: &Path) {
    out.hardlinks_degraded += members;
    out.hardlinks_unlinked
        .entry(why)
        .or_insert_with(|| DegradedGroup { count: 0, example: example.to_path_buf() })
        .count += members;
}

/// One member copied independently for `why` (a cause that is not a weak identity).
fn degrade(out: &mut TreeOutcome, path: &Path, why: UnlinkedWhy) {
    degrade_many(out, why, 1, path);
}

/// The existing-file policy skipped this candidate (`dest` is what it left in place): the group's entry says whether
/// that is a degradation yet (plan ruling 16). Nothing is counted for the skip itself. A failed index write counts
/// the member `IndexFailure` under `auto` (its split can no longer be told); under `preserve` it counts nothing: the
/// member was skipped, not copied or failed.
fn skipped_member<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    group: &Group,
    target: &FluxPathKey,
    dest: &Metadata,
    path: &Path,
    out: &mut TreeOutcome,
) {
    let (Some(slot), Lookup::Found(entry)) = (cx.hardlinks, &group.entry) else { return };
    let here = strong(dest.identity);
    let upsert = |out: &mut TreeOutcome, members, example: FluxPathKey| {
        let skipped = HardlinkEntry::Skipped { object: here, members, example };
        // A failed write loses the later accounting of this member; under `auto` the split is counted now instead
        // (plan ruling 13). Under `preserve` the member stays a policy skip: it was never copied, so it has no
        // failure to report.
        if slot.with(|index| index.insert(&group.id, &skipped)).is_err()
            && cx.opts.hardlinks != Hardlinks::Preserve
        {
            degrade(out, path, UnlinkedWhy::IndexFailure);
        }
    };
    match entry {
        None => upsert(out, 1, target.clone()),
        Some(HardlinkEntry::Skipped { object, members, example }) => {
            if object.is_some() && *object == here {
                upsert(out, members + 1, example.clone());
            } else {
                degrade(out, path, UnlinkedWhy::PolicySplit);
            }
        }
        Some(HardlinkEntry::Anchor { object: Some(anchor), .. }) if here == Some(*anchor) => {}
        Some(HardlinkEntry::Anchor { .. }) => degrade(out, path, UnlinkedWhy::PolicySplit),
        Some(HardlinkEntry::Failed { .. }) => {}
    }
}

/// What a candidate that will be copied or linked does, by its group's entry (after the plan, the directory check and
/// the dry-run branch). A reported failure answers `Meet::Finished`.
fn meet_group<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    group: Option<Group>,
    path: &Path,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> Meet {
    let Some(Group { id, entry }) = group else { return Meet::Ordinary };
    match entry {
        Lookup::Found(None) => Meet::Canonical { id, skipped: None },
        Lookup::Found(Some(HardlinkEntry::Skipped { members, example, .. })) => {
            Meet::Canonical { id, skipped: Some((members, example)) }
        }
        Lookup::Found(Some(HardlinkEntry::Anchor { target: anchor, object })) => {
            Meet::Link { anchor, object }
        }
        Lookup::Found(Some(HardlinkEntry::Failed { canonical })) => {
            let why =
                format!("its canonical member {} failed to copy", shown(&canonical).display());
            let e = FsError::new(Code::BlockedByCanonicalFailure, std::io::Error::other(why));
            let cause = TreeFailureCause::Copy(CopyError::at(CopyStep::Gate, e));
            report(out, on_report, path.to_path_buf(), cause);
            Meet::Finished
        }
        Lookup::Unreadable if cx.opts.hardlinks == Hardlinks::Preserve => {
            let cause = target_failure(
                CopyStep::Claim,
                Code::HardlinkUnavailable,
                "the hardlink index could not be read",
            );
            report(out, on_report, path.to_path_buf(), cause);
            Meet::Finished
        }
        Lookup::Unreadable => {
            degrade(out, path, UnlinkedWhy::IndexFailure);
            Meet::Ordinary
        }
    }
}

/// After a canonical's copy (`published` is the new object's identity, `None` when the copy failed and was reported):
/// record the group's entry, and with an anchor the members the policy had skipped are now degradations (ruling 16).
fn register_outcome<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    meet: Meet,
    target: &FluxPathKey,
    published: Option<FileIdentity>,
    path: &Path,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) {
    let (Meet::Canonical { id, skipped }, Some(slot)) = (meet, cx.hardlinks) else { return };
    // Ruled acceptable at the fresh and the resume site alike: under `preserve` a failed index write both reports a
    // failure and counts the file (copied or resumed); the file on disk is correct, the failure concerns the group.
    // Ruled acceptable: the anchor is registered even when the canonical's claim was not recorded (that loss is already
    // reported as `ClaimNotRecorded`, and `--resume` heals the members by rule 3), and an error inserting `Failed` is
    // ignored (the canonical's own failure is the report; later members find no entry and form a group of their own).
    let entry = match published {
        Some(identity) => {
            HardlinkEntry::Anchor { target: target.clone(), object: strong(identity) }
        }
        None => HardlinkEntry::Failed { canonical: target.clone() },
    };
    match slot.with(|index| index.insert(&id, &entry)) {
        Ok(()) => {
            if let (Some(_), Some((members, example))) = (published, skipped) {
                degrade_many(out, UnlinkedWhy::PolicySplit, members, &shown(&example));
            }
        }
        // A failed canonical needs no second report: the later members simply find no entry.
        Err(_) if published.is_none() => {}
        Err(e) => {
            if cx.opts.hardlinks == Hardlinks::Preserve {
                let cause = TreeFailureCause::Copy(CopyError::at(CopyStep::Claim, e));
                report(out, on_report, path.to_path_buf(), cause);
            } else {
                degrade(out, path, UnlinkedWhy::IndexFailure);
            }
        }
    }
}

/// What `settle_link` made of a link attempt.
enum Settled {
    /// The member is a link; the arm records its claim.
    Linked(flux_fs::Outcome),
    /// The attempt failed, or was refused under `preserve`, and was reported.
    Finished,
    /// Degraded under `auto` (counted): the arm copies it.
    Copy,
}

/// A `link_member` answer as the arm needs it: a degradation is counted (`auto`) or reported (`preserve`), and a
/// failure goes through `discard_note_on_failure` and `finish_copy`, so a stop propagates and anything else is reported.
#[allow(clippy::too_many_arguments)]
fn settle_link<F: DestinationRoot, C: ClaimStore + ?Sized>(
    cx: &Shared<'_, F>,
    claims: &std::cell::RefCell<C>,
    key: &ClaimKey,
    wrote: &std::cell::Cell<bool>,
    path: &Path,
    step: std::result::Result<LinkStep, CopyError>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<Settled, CopyError> {
    match step {
        Ok(LinkStep::Linked(outcome)) => Ok(Settled::Linked(outcome)),
        Ok(LinkStep::Degrade(step, why, text)) => {
            if cx.opts.hardlinks == Hardlinks::Preserve {
                let cause = target_failure(step, Code::HardlinkUnavailable, text);
                report(out, on_report, path.to_path_buf(), cause);
                Ok(Settled::Finished)
            } else {
                degrade(out, path, why);
                Ok(Settled::Copy)
            }
        }
        Err(e) => {
            let failed =
                discard_note_on_failure(claims, key, wrote, cx.guard, path, Err(e), out, on_report);
            finish_copy(cx.key_prefix, path.to_path_buf(), failed, out, on_report)?;
            Ok(Settled::Finished)
        }
    }
}

/// Cut 12: `link_member`'s answer when the link was made and published, or why this member cannot be linked.
enum LinkStep {
    Linked(flux_fs::Outcome),
    /// Not linked, nothing of ours left on disk: the step it failed at, why, and the text for `preserve`'s failure.
    Degrade(CopyStep, UnlinkedWhy, &'static str),
}

/// Cut 12 (design, "Fresh copy" step 2): a group member that is NOT the canonical: no data is transferred. The anchor
/// is checked to still be the registered object, then (master §16.1 as `publish_link`) the member's temporary is made
/// with `create_hardlink`, its identity must be the registered object's, and it is published with the plan's `publish`.
/// The outer `Err` is a stop from a flush, which the entry passes on unreported; the inner result is the entry's, for
/// `settle_link`.
#[allow(clippy::too_many_arguments)]
fn link_member<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    names: &mut NameIndex,
    batch: &mut Batch<F::Writer>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
    name: &OsStr,
    src_meta: &Metadata,
    anchor: &FluxPathKey,
    object: Option<ObjectId>,
    publish: Publish,
    before_create: &BeforeCreate<'_>,
    before_publish: &BeforePublish<'_>,
) -> std::result::Result<std::result::Result<LinkStep, CopyError>, CopyError> {
    let degraded = |step, why, text| Ok(Ok(LinkStep::Degrade(step, why, text)));
    // 1. The anchor is still the object that was registered.
    let Some(object) = object else {
        return degraded(
            CopyStep::Gate,
            UnlinkedWhy::AnchorUnavailable,
            "the anchor's destination identity is not strong",
        );
    };
    let Some(root) = cx.anchor_root else {
        return degraded(
            CopyStep::Gate,
            UnlinkedWhy::AnchorUnavailable,
            "DEST could not be reopened for linking",
        );
    };
    let Some((anchor_dir, anchor_name)) = anchor_parent(root, anchor) else {
        return degraded(
            CopyStep::Gate,
            UnlinkedWhy::AnchorUnavailable,
            "the anchor's directory could not be opened",
        );
    };
    let anchor_dir: &F::Dir = &anchor_dir;
    if !anchor_dir.metadata(&anchor_name).is_ok_and(|m| strong(m.identity) == Some(object)) {
        return degraded(
            CopyStep::Gate,
            UnlinkedWhy::AnchorUnavailable,
            "the anchor is gone or is another object",
        );
    }
    // The identity gate a copy runs at this name (a replacement must not be the source itself; `Safety::Strict` refuses
    // a target that cannot be compared). A refusal is the member's failure, not a degradation.
    let gate = match identity_gate(parent, name, src_meta, cx.opts.safety, publish) {
        Ok(g) => g,
        Err(e) => return Ok(Err(e)),
    };
    // 2. As `publish_link`: heartbeat and guard before the first mutation; with entries pending and something at this
    // operation's temporary name the batch is flushed first, and the leftover at the name removed.
    let temp = flux_fs::temp_path(Path::new(name), &cx.opts.operation_id).into_os_string();
    if let Err(e) = (cx.beat)() {
        return Ok(Err(CopyError::at(CopyStep::Heartbeat, e)));
    }
    if let Err(e) = (cx.guard)() {
        return Ok(Err(CopyError::at(CopyStep::Create, e)));
    }
    if !batch.pending.is_empty() && parent.metadata(&temp).is_ok() {
        flush_batch(cx, parent, names, batch, out, on_report)?;
        if let Err(e) = (cx.beat)() {
            return Ok(Err(CopyError::at(CopyStep::Heartbeat, e)));
        }
        if let Err(e) = (cx.guard)() {
            return Ok(Err(CopyError::at(CopyStep::Create, e)));
        }
    }
    let _ = parent.remove_file(&temp);
    // 3. The link, at the temporary: heartbeat, guard, `before_create` (the Replace claim), then the exclusive create.
    let create = || -> std::result::Result<(), CopyError> {
        (cx.beat)().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
        (cx.guard)().map_err(|e| CopyError::at(CopyStep::Create, e))?;
        before_create()?;
        parent
            .create_hardlink(&temp, anchor_dir, &anchor_name)
            .map_err(|e| CopyError::at(CopyStep::Create, e))
    };
    let created = match create() {
        // The second backstop, as `publish_link`: the name may be a pending sibling's temporary.
        Err(e)
            if e.step == CopyStep::Create
                && e.cause.source.kind() == ErrorKind::AlreadyExists
                && !batch.pending.is_empty() =>
        {
            flush_batch(cx, parent, names, batch, out, on_report)?;
            create()
        }
        created => created,
    };
    match created {
        Ok(()) => {}
        Err(e) if e.step == CopyStep::Create && e.cause.code == Code::HardlinkUnavailable => {
            return degraded(
                CopyStep::Create,
                UnlinkedWhy::LinkRefused,
                "the destination cannot hold the link",
            );
        }
        // A failed create made nothing of ours.
        Err(e) => return Ok(Err(e)),
    }
    // 4. The anchor's name could have been swapped between the check and the link: only the registered object may be
    // published. Anything else is removed (while this run holds the lock) and the member degrades.
    let identity = match parent.metadata(&temp) {
        Ok(m) => m.identity,
        Err(e) => {
            return Ok(Err(discard(parent, &temp, CopyStep::Create, e.code, e.source, cx.guard)));
        }
    };
    if strong(identity) != Some(object) {
        if let Err(lost) = (cx.guard)() {
            let kept =
                std::io::Error::other("kept: this run no longer holds the destination's lock");
            return Ok(Err(CopyError {
                leftover: Some((PathBuf::from(&temp), kept)),
                ..CopyError::at(CopyStep::Create, lost)
            }));
        }
        let _ = parent.remove_file(&temp);
        return degraded(
            CopyStep::Create,
            UnlinkedWhy::AnchorUnavailable,
            "the anchor changed while linking",
        );
    }
    // 5. The note (Strict), then the publish itself.
    let published = publish_identified(
        cx,
        parent,
        name,
        temp.clone(),
        identity,
        gate.degraded,
        publish,
        before_publish,
    );
    // 6. A same-object rename (POSIX `rename(2)` of a name onto another name of one object) leaves the temporary.
    match published {
        Ok(outcome) => {
            if parent.metadata(&temp).is_ok() {
                if let Err(lost) = (cx.guard)() {
                    let kept = std::io::Error::other(
                        "kept: this run no longer holds the destination's lock",
                    );
                    return Ok(Err(CopyError {
                        leftover: Some((PathBuf::from(&temp), kept)),
                        ..CopyError::at(CopyStep::Publish, lost)
                    }));
                }
                let _ = parent.remove_file(&temp);
            }
            Ok(Ok(LinkStep::Linked(outcome)))
        }
        Err(e) => Ok(Err(e)),
    }
}

/// Cut 10a: a `Symlink` event under a live frame (spec 3.2): a file entry with no bytes. It runs `copy_one`'s
/// protocol (resolve, plan New or Replace, claim, publish through the temporary name, record) with the streaming
/// replaced by one `create_symlink`, and its result goes through `finish_copy` exactly as a file's does, so the stops,
/// the publish-failure mappings and the leftover collection are a file's. A link is never batched and never
/// checkpointed. The link's target is never opened or stat'ed, on either side (§126).
// One input per piece of frame state the protocol reads, as `copy_one`.
#[allow(clippy::too_many_arguments)]
fn link_one<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: Option<&F::Dir>,
    names: &mut NameIndex,
    batch: &mut Batch<F::Writer>,
    claim_parent: Option<ObjectId>,
    fresh: bool,
    path: PathBuf,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    // 1. A reserved control path is refused as a file's is, before anything is read.
    if reserved_path(&path) {
        let conflict = CopyError::at(CopyStep::Gate, reserved_conflict());
        report(out, on_report, path, TreeFailureCause::Copy(conflict));
        return Ok(());
    }
    // 2. `--links=skip` (§124): no source read, no destination touch.
    if cx.opts.links == Links::Skip {
        report(out, on_report, path, TreeFailureCause::SymlinkSkipped);
        return Ok(());
    }
    let name_owned = path.file_name().expect("a walk path ends in a name").to_os_string();
    let name = name_owned.as_os_str();
    let src = cx.src_root.join(&path);

    // 3. The payload, never followed. A junction or mounted-volume folder is a special file (D7).
    let link = match cx.fs.read_link(&src) {
        Ok(link) => link,
        Err(e) if e.code == Code::SpecialFileUnsupported => {
            report(out, on_report, path, TreeFailureCause::SpecialFileSkipped);
            return Ok(());
        }
        Err(e) => {
            return finish_copy(
                cx.key_prefix,
                path,
                Err(CopyError::at(CopyStep::Source, e)),
                out,
                on_report,
            );
        }
    };

    // 4. Resolve and plan, as `copy_one` does.
    let (Some(parent_id), Some(claims)) = (claim_parent, cx.claims) else {
        // No claims for this directory: published as new, an existing name is a collision (as a file's). No batch
        // exists without claims.
        if cx.dry_run {
            return dry_stop_link(
                cx,
                parent,
                name,
                &src,
                path,
                Publish::NoReplace,
                false,
                out,
                on_report,
            );
        }
        let Some(parent) = parent else {
            unreachable!("only a dry run has a planned directory");
        };
        let published = publish_link(
            cx,
            parent,
            names,
            batch,
            out,
            on_report,
            name,
            &src,
            &link,
            Publish::NoReplace,
            &no_before_create,
            &no_before_publish,
        )?;
        return finish_copy(cx.key_prefix, path, published, out, on_report);
    };
    let target = match FluxPathKey::from_relative(&cx.keyed(&path)) {
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
    // The same aged/folded pre-check a file runs before resolving while a batch is pending (decisions 5(d), 5(f)).
    let batching = !cx.dry_run
        && cx.batch.files > 1
        && cx.opts.durability == Durability::Strict
        && claims.borrow().supports_prepared();
    if batching
        && !batch.pending.is_empty()
        && let Some(parent) = parent
    {
        let aged = batch
            .since
            .is_some_and(|t| (cx.batch.now)().saturating_duration_since(t) >= cx.batch.age);
        if aged || (cx.batch.fold_precheck && batch.folded.contains(&folded(name))) {
            flush_batch(cx, parent, names, batch, out, on_report)?;
        }
    }

    let resolved = if fresh {
        Ok(Ok(Resolved::Absent))
    } else {
        let Some(parent) = parent else { unreachable!("a planned directory is fresh") };
        names.resolve(parent, name)
    };
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
        Ok(Ok(Resolved::Entry { stored, meta })) => {
            // An entry is resolved only under a directory that exists.
            let Some(parent) = parent else { unreachable!("a planned directory is fresh") };
            // §241.5, before any policy decision.
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
                return finish_copy(cx.key_prefix, path, Err(e), out, on_report);
            }
            // `--resume` (spec 3.4). The branch mutates nothing at the destination, so it neither beats nor guards.
            if cx.resume {
                let found = claims.borrow().get(&ClaimKey::new(parent_id, &stored));
                match found {
                    Err(e) => {
                        return finish_copy(
                            cx.key_prefix,
                            path,
                            Err(CopyError::at(CopyStep::Claim, e)),
                            out,
                            on_report,
                        );
                    }
                    Ok(Some(r)) if r.target == target => {
                        // Resumed only when the claim says `Created` AND the destination is an equal link; anything
                        // else falls through to the policy and republishes.
                        if r.status == ClaimStatus::Created && same_link(parent, &stored, &link) {
                            out.note_resumed();
                            names.record_publication(
                                &target,
                                Some(&stored),
                                name,
                                None,
                                meta.identity,
                            );
                            if cx.dry_run {
                                dry_resumed(out, on_report, path, PlannedObject::Symlink);
                            }
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
                        return finish_copy(cx.key_prefix, path, Err(e), out, on_report);
                    }
                    Ok(None) => {}
                }
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
            // §5.1 for a link (spec 3.3): sizes and times say nothing about a link, so `update_replaces` is not used.
            let skip = match cx.opts.existing {
                ExistingPolicy::Overwrite => None,
                ExistingPolicy::SkipExisting => Some(SkipReason::Existing),
                // Skips an equal destination link only; a file, another link or an unreadable one is replaced.
                ExistingPolicy::Update => {
                    same_link(parent, &stored, &link).then_some(SkipReason::UpToDate)
                }
            };
            if let Some(reason) = skip {
                out.files_skipped += 1;
                if cx.dry_run {
                    let skipped = PlannedEntry {
                        object: PlannedObject::Symlink,
                        action: PlannedAction::Skip(reason),
                    };
                    report(out, on_report, path, TreeFailureCause::Planned(skipped));
                }
                return Ok(());
            }
            Plan::Replace { stored, meta }
        }
    };

    if cx.dry_run {
        let (publish, replace) = match plan {
            Plan::New => (Publish::NoReplace, false),
            Plan::Replace { .. } => (Publish::Replace, true),
        };
        return dry_stop_link(cx, parent, name, &src, path, publish, replace, out, on_report);
    }
    let Some(parent) = parent else {
        unreachable!("only a dry run has a planned directory");
    };

    // 5-6. Claim and publish, as `copy_one`'s unbatched plan arms do.
    let own = |status| ClaimRecord { target: target.clone(), status };
    let strict = cx.opts.durability == Durability::Strict && claims.borrow().supports_prepared();
    let wrote = std::cell::Cell::new(false);
    match plan {
        Plan::New => {
            let key = ClaimKey::new(parent_id, name);
            let template = PreparedRecord {
                target: target.clone(),
                temp_name: Vec::new(),
                identity: String::new(),
                dir_path: note_dir(&cx.keyed(&path)),
                name: name.as_encoded_bytes().to_vec(),
                planned_name: Vec::new(),
                replacement: false,
            };
            let hook = |intent: &PublishIntent| write_note(claims, &key, &template, &wrote, intent);
            let before_publish: &BeforePublish<'_> =
                if strict { &hook } else { &no_before_publish };
            let result = publish_link(
                cx,
                parent,
                names,
                batch,
                out,
                on_report,
                name,
                &src,
                &link,
                Publish::NoReplace,
                &no_before_create,
                before_publish,
            )?;
            let result = discard_note_on_failure(
                claims, &key, &wrote, cx.guard, &path, result, out, on_report,
            );
            let published = result.as_ref().ok().map(|o| o.published_identity);
            finish_copy(cx.key_prefix, path.clone(), result, out, on_report)?;
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
                dir_path: note_dir(&cx.keyed(&path)),
                name: stored.as_encoded_bytes().to_vec(),
                planned_name: if name != stored {
                    name.as_encoded_bytes().to_vec()
                } else {
                    Vec::new()
                },
                replacement: true,
            };
            let hook =
                |intent: &PublishIntent| write_note(claims, &key_stored, &template, &wrote, intent);
            let before_publish: &BeforePublish<'_> =
                if strict { &hook } else { &no_before_publish };
            let result = publish_link(
                cx,
                parent,
                names,
                batch,
                out,
                on_report,
                name,
                &src,
                &link,
                Publish::Replace,
                &before_create,
                before_publish,
            )?;
            let result = discard_note_on_failure(
                claims,
                &key_stored,
                &wrote,
                cx.guard,
                &path,
                result,
                out,
                on_report,
            );
            let published = result.as_ref().ok().map(|o| o.published_identity);
            finish_copy(cx.key_prefix, path.clone(), result, out, on_report)?;
            if let Some(identity) = published {
                out.files_overwritten += 1;
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

/// Cut 10a: `link_one`'s gate and publish (spec 3.2, the end of step 4 and step 5). The source link's own metadata
/// (never its target's) feeds the same `identity_gate` a file runs, every row. Then, in `stage_file`'s and
/// `publish_staged`'s order: heartbeat and guard before every mutation; with entries pending and something at this
/// operation's temporary name (on a folding destination, a pending sibling's temporary), the batch flushed first, since
/// nothing there is removed while entries are pending; the leftover at the temporary name removed; heartbeat and guard,
/// `before_create` (the Replace claim); the exclusive `create_symlink` at the temporary (refused `AlreadyExists` with
/// entries pending: the batch flushed and the create made once more); its identity (a failure discards it);
/// `before_publish` (the Strict note; a failure discards it); and the final heartbeat, guard and rename through
/// `publish_staged` itself, so a heartbeat failure or a failed rename discards the temporary and a lost lock keeps it as
/// the leftover. The outer `Err` is a stop from a flush (`flush_batch`'s), which the entry passes on unreported; the inner
/// result is the entry's, for `finish_copy`.
#[allow(clippy::too_many_arguments)]
fn publish_link<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    names: &mut NameIndex,
    batch: &mut Batch<F::Writer>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
    name: &std::ffi::OsStr,
    src: &Path,
    link: &flux_fs::Link,
    publish: Publish,
    before_create: &BeforeCreate<'_>,
    before_publish: &BeforePublish<'_>,
) -> std::result::Result<std::result::Result<flux_fs::Outcome, CopyError>, CopyError> {
    let (degraded, temp) = match gate_link(cx, parent, name, src, publish) {
        Ok(g) => g,
        Err(e) => return Ok(Err(e)),
    };
    if !batch.pending.is_empty() && parent.metadata(&temp).is_ok() {
        flush_batch(cx, parent, names, batch, out, on_report)?;
        // The gate's heartbeat and guard predate the flush: take fresh ones before the removal below, as `stage_file`
        // guards right before its own (review SC-1). Without a flush, the gate's pair is the one just before it.
        if let Err(e) = (cx.beat)() {
            return Ok(Err(CopyError::at(CopyStep::Heartbeat, e)));
        }
        if let Err(e) = (cx.guard)() {
            return Ok(Err(CopyError::at(CopyStep::Create, e)));
        }
    }
    // A leftover under this operation's own id (a crashed earlier attempt under `--resume`), as `stage_file` step 1
    // removes it: `remove_file` unlinks a link and refuses a directory, which the create below then fails on. The batch
    // is empty here whenever something was at `temp`.
    let _ = parent.remove_file(&temp);

    let create = || create_link(cx, parent, &temp, link, before_create);
    let created = match create() {
        // The second backstop (as `stage_into_batch`'s): the name may be a pending sibling's temporary.
        Err(e)
            if e.step == CopyStep::Create
                && e.cause.source.kind() == ErrorKind::AlreadyExists
                && !batch.pending.is_empty() =>
        {
            flush_batch(cx, parent, names, batch, out, on_report)?;
            create()
        }
        created => created,
    };
    if let Err(e) = created {
        // A failed create made nothing of ours.
        return Ok(Err(e));
    }
    Ok(publish_created_link(cx, parent, name, temp, degraded, publish, before_publish))
}

/// Cut 11a: where a dry run stops a file target, in place of the publish: the read-only half of `stage_file`
/// (`plan_file`, told `Overwrite` so it never skips: the tree decided the policy), counted exactly as a published file
/// is, and one `Planned` record. `parent` is `None` under a directory that does not exist yet. `replace` is the tree's
/// plan (`Plan::Replace`).
#[allow(clippy::too_many_arguments)]
fn dry_stop_file<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: Option<&F::Dir>,
    name: &std::ffi::OsStr,
    src: &Path,
    path: PathBuf,
    publish: Publish,
    replace: bool,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<bool, CopyError> {
    if let Err(e) = dry_temp_name_fits(cx, name) {
        return finish_copy(cx.key_prefix, path, Err(e), out, on_report).map(|()| false);
    }
    let opts = CopyOptions { existing: ExistingPolicy::Overwrite, publish, ..cx.opts.clone() };
    let planned = match plan_file(cx.fs, src, parent, name, &opts) {
        Ok(p) => p,
        Err(e) => return finish_copy(cx.key_prefix, path, Err(e), out, on_report).map(|()| false),
    };
    let (src_meta, degraded) = match planned {
        FilePlan::Proceed { src_meta, degraded, replaces } => {
            // The gate sees an entry at the name only when the tree planned a replacement: under `NoReplace` an
            // existing name is the gate's own refusal, which came back as `Err` above.
            debug_assert!(replace || !replaces, "the gate found an entry the tree planned as new");
            (src_meta, degraded)
        }
        // `existing` is `Overwrite`, which never skips.
        FilePlan::Skip { reason, .. } => {
            unreachable!("a dry stop plans with Overwrite, which never skips ({reason:?})")
        }
    };
    let outcome = flux_fs::Outcome {
        bytes_copied: src_meta.len,
        metadata_failures: vec![],
        identity_degraded: degraded,
        published_identity: FileIdentity::Unavailable,
        skipped: false,
        verified: false,
    };
    finish_copy(cx.key_prefix, path.clone(), Ok(outcome), out, on_report)?;
    let partial = dry_partial_len(cx, parent, name);
    dry_planned(out, on_report, path, PlannedObject::File, replace, partial);
    Ok(true)
}

/// Cut 12 Task 8: a dry run's dependent of a hardlink group. No destination I/O: the entry counts as linked (never
/// copied: ruling 11), a replacement as overwritten, and one `Planned` record says `would_hardlink`. Only the name's
/// own length is checked (the live link makes a temporary of that name); the gate, the anchor and the link itself are
/// the live run's to meet.
fn dry_link<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    name: &std::ffi::OsStr,
    path: PathBuf,
    replace: bool,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    if let Err(e) = dry_temp_name_fits(cx, name) {
        return finish_copy(cx.key_prefix, path, Err(e), out, on_report);
    }
    out.files_hardlinked += 1;
    if replace {
        out.files_overwritten += 1;
    }
    let entry = PlannedEntry { object: PlannedObject::File, action: PlannedAction::Hardlink };
    report(out, on_report, path, TreeFailureCause::Planned(entry));
    Ok(())
}

/// `dry_stop_file` for a symlink target: `gate_link_read` (the source stat is inside it) and no bytes.
#[allow(clippy::too_many_arguments)]
fn dry_stop_link<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: Option<&F::Dir>,
    name: &std::ffi::OsStr,
    src: &Path,
    path: PathBuf,
    publish: Publish,
    replace: bool,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    if let Err(e) = dry_temp_name_fits(cx, name) {
        return finish_copy(cx.key_prefix, path, Err(e), out, on_report);
    }
    let degraded = match gate_link_read(cx, parent, name, src, publish) {
        Ok(d) => d,
        Err(e) => return finish_copy(cx.key_prefix, path, Err(e), out, on_report),
    };
    // A live link publishes 0 bytes.
    let outcome = flux_fs::Outcome {
        bytes_copied: 0,
        metadata_failures: vec![],
        identity_degraded: degraded,
        published_identity: FileIdentity::Unavailable,
        skipped: false,
        verified: false,
    };
    finish_copy(cx.key_prefix, path.clone(), Ok(outcome), out, on_report)?;
    dry_planned(out, on_report, path, PlannedObject::Symlink, replace, None);
    Ok(())
}

/// Cut 11a (ruling P12): live creates the entry's temporary (`temp_path`, the name plus a suffix) before a byte moves,
/// and a name over the limit fails that create with `IO_ERROR` (ENAMETOOLONG). The name alone predicts it, so a dry run
/// reports it, at the live step, before the gate.
fn dry_temp_name_fits<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    name: &std::ffi::OsStr,
) -> std::result::Result<(), CopyError> {
    let temp = flux_fs::temp_path(Path::new(name), &cx.opts.operation_id);
    let too_long = temp
        .file_name()
        .is_some_and(|n| crate::lock::site::name_len(n) > crate::lock::site::NAME_LIMIT);
    if too_long {
        return Err(CopyError::at(
            CopyStep::Create,
            FsError::new(
                Code::IoError,
                std::io::Error::new(
                    ErrorKind::InvalidFilename,
                    "the temporary's name would exceed the name limit",
                ),
            ),
        ));
    }
    Ok(())
}

/// Cut 11b: under a dry `--resume`, the length of the prior's half-copied temporary beside `name`, counted in
/// `dry_partials`. One metadata call; a missing temporary, or any error, is no partial (nothing is previewed from it).
fn dry_partial_len<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: Option<&F::Dir>,
    name: &std::ffi::OsStr,
) -> Option<u64> {
    let (id, parent) = (cx.dry_prior?, parent?);
    let temp = flux_fs::temp_path(Path::new(name), &flux_fs::OperationId::new(id));
    let m = parent.metadata(temp.file_name()?).ok()?;
    if let Some(count) = cx.dry_partials {
        count.set(count.get() + 1);
    }
    Some(m.len)
}

/// Cut 11b: a dry `--resume` entry the claim store resumes: one `Planned(object, Resume)` record (the counters
/// already counted it).
fn dry_resumed(
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
    path: PathBuf,
    object: PlannedObject,
) {
    let entry = PlannedEntry { object, action: PlannedAction::Resume };
    report(out, on_report, path, TreeFailureCause::Planned(entry));
}

/// What a dry stop does after its entry counted as copied: a replacement counts as overwritten, and the record says
/// which it was.
fn dry_planned(
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
    path: PathBuf,
    object: PlannedObject,
    replace: bool,
    partial: Option<u64>,
) {
    let action = match (replace, object) {
        (true, _) => PlannedAction::Overwrite,
        (false, PlannedObject::Symlink) => PlannedAction::CreateSymlink,
        // The walk plans files, directories and links; a restart preview's `State` and `Partial` entries (cut 11b)
        // are streamed directly and never pass here.
        (false, _) => {
            partial.map_or(PlannedAction::Copy, |len| PlannedAction::CopyOverPartial { len })
        }
    };
    if replace {
        out.files_overwritten += 1;
    }
    report(out, on_report, path, TreeFailureCause::Planned(PlannedEntry { object, action }));
}

/// `publish_link`'s gate: the source link's own metadata through `identity_gate`, then the heartbeat and the guard
/// before the first mutation. Nothing of ours is on disk yet. Answers the gate's degraded identity and the temporary name.
fn gate_link<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    name: &std::ffi::OsStr,
    src: &Path,
    publish: Publish,
) -> std::result::Result<(Option<FileIdentity>, OsString), CopyError> {
    let degraded = gate_link_read(cx, Some(parent), name, src, publish)?;
    let temp = flux_fs::temp_path(Path::new(name), &cx.opts.operation_id).into_os_string();
    (cx.beat)().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
    (cx.guard)().map_err(|e| CopyError::at(CopyStep::Create, e))?;
    Ok((degraded, temp))
}

/// `gate_link`'s read-only half: the source link's own metadata and `identity_gate`. The stat always runs; with
/// `parent: None` (a dry run under a destination directory that does not exist yet) no gate runs and the answer is
/// `Ok(None)`.
fn gate_link_read<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: Option<&F::Dir>,
    name: &std::ffi::OsStr,
    src: &Path,
    publish: Publish,
) -> std::result::Result<Option<FileIdentity>, CopyError> {
    let src_meta = cx.fs.metadata(src).map_err(|e| CopyError::at(CopyStep::Source, e))?;
    let Some(parent) = parent else { return Ok(None) };
    let gate = identity_gate(parent, name, &src_meta, cx.opts.safety, publish)?;
    Ok(gate.degraded)
}

/// `publish_link`'s create, `stage_file`'s step-3 order: heartbeat, guard, `before_create`, the exclusive create.
fn create_link<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    temp: &std::ffi::OsStr,
    link: &flux_fs::Link,
    before_create: &BeforeCreate<'_>,
) -> std::result::Result<(), CopyError> {
    (cx.beat)().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
    (cx.guard)().map_err(|e| CopyError::at(CopyStep::Create, e))?;
    before_create()?;
    parent.create_symlink(temp, link).map_err(|e| CopyError::at(CopyStep::Create, e))
}

/// `publish_link` after the create: the created link's identity, `before_publish`, then `publish_staged`.
fn publish_created_link<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    name: &std::ffi::OsStr,
    temp: OsString,
    degraded: Option<FileIdentity>,
    publish: Publish,
    before_publish: &BeforePublish<'_>,
) -> std::result::Result<flux_fs::Outcome, CopyError> {
    // The created link's own identity: the handle stat judges the name and never follows it.
    let identity = match parent.metadata(&temp) {
        Ok(m) => m.identity,
        Err(e) => {
            return Err(discard(parent, &temp, CopyStep::Create, e.code, e.source, cx.guard));
        }
    };
    publish_identified(cx, parent, name, temp, identity, degraded, publish, before_publish)
}

/// `publish_created_link` with the created object's identity in hand: `before_publish`, then `publish_staged`.
#[allow(clippy::too_many_arguments)]
fn publish_identified<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    name: &std::ffi::OsStr,
    temp: OsString,
    identity: FileIdentity,
    degraded: Option<FileIdentity>,
    publish: Publish,
    before_publish: &BeforePublish<'_>,
) -> std::result::Result<flux_fs::Outcome, CopyError> {
    let intent = PublishIntent { temp: temp.clone(), identity };
    if let Err(e) = before_publish(&intent) {
        let CopyError { cause, leftover, step } = e;
        let mut out = discard(parent, &temp, step, cause.code, cause.source, cx.guard);
        if out.leftover.is_none() {
            out.leftover = leftover;
        }
        return Err(out);
    }
    let staged = Staged {
        temp,
        identity,
        bytes_copied: 0,
        metadata_failures: vec![],
        identity_degraded: degraded,
        writer: None,
        reused: 0,
        discard: None,
        verified: false,
    };
    publish_staged(parent, name, staged, publish, cx.guard, cx.beat)
}

/// Cut 10a (spec 3.3, 3.4): the one link equality rule, shared by `--update` and `--resume`: the destination entry at
/// `stored` is a symlink whose `payload` AND `kind` equal `link`'s. A failing read (not a link, a junction, an I/O
/// error) is "not equal", never a failed entry.
fn same_link<D: DirHandle>(parent: &D, stored: &std::ffi::OsStr, link: &flux_fs::Link) -> bool {
    parent.read_link(stored).is_ok_and(|l| l.payload == link.payload && l.kind == link.kind)
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
    batch: &mut Batch<F::Writer>,
    src: &Path,
    path: PathBuf,
    publish: Publish,
    before_create: &BeforeCreate<'_>,
    fc: Option<&FileCkpt<'_, F::Dir>>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
    pending: impl FnOnce(PathBuf, Staged<F::Writer>) -> Pending<F::Writer>,
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
    // Cloned before the closure: `stage` runs twice with `flush_batch(.., out, ..)` between the runs.
    let progress = out.progress.clone();
    let stage = || {
        stage_file(
            cx.fs,
            src,
            parent,
            name,
            &opts,
            cx.guard,
            cx.beat,
            before_create,
            // Cut 9e: a Strict batch defers the data sync to the flush's barrier; a Normal batch never reaches here
            // (it syncs nothing), the branch is for completeness.
            if cx.opts.durability == Durability::Strict {
                SyncAt::Barrier
            } else {
                SyncAt::Staging
            },
            fc.map(Hook::Logged),
            progress.as_deref(),
        )
    };
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
        Err(e) => return finish_copy(cx.key_prefix, path, Err(e), out, on_report),
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
    batch: &mut Batch<F::Writer>,
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
    // Cut 9e, spec decision 1: the data barrier. Every staged file's `fsync` runs here, on up to `sync_threads` workers
    // while this thread keeps the lock's heartbeat alive, BEFORE the opening heartbeat and guard (so those see a lock
    // that was alive after the longest wait of the batch) and before any note exists. `indices[j]` is the entry whose
    // writer is `writers[j]`; an entry with no writer was synced at staging.
    let mut entries = entries;
    let mut writers: Vec<&F::Writer> = Vec::with_capacity(entries.len());
    let mut indices: Vec<usize> = Vec::with_capacity(entries.len());
    for (i, p) in entries.iter().enumerate() {
        if let Some(w) = p.staged.writer.as_ref() {
            writers.push(w);
            indices.push(i);
        }
    }
    let started = std::time::Instant::now();
    let synced = sync_staged_many(&writers, cx.batch.sync_threads, cx.beat, cx.batch.beat_every);
    out.barrier_max = out.barrier_max.max(started.elapsed());
    drop(writers);
    // Close every handle now, on every path below (a failed sync's too).
    for p in &mut entries {
        p.staged.writer = None;
    }
    let mut failed: Vec<Option<FsError>> = Vec::with_capacity(entries.len());
    failed.resize_with(entries.len(), || None);
    for (j, r) in synced.results.into_iter().enumerate() {
        if let Err(e) = r {
            failed[indices[j]] = Some(e);
        }
    }
    // A heartbeat that failed during the wait is handled as the opening one below, and not detected again (plan ruling 4).
    if let Some(e) = synced.heartbeat {
        return Err(stop_held_before_notes(cx, parent, entries, failed, e, out, on_report));
    }
    // 4.1: sections 101 and 99, as before any synced write under DEST. No note exists yet.
    if let Err(e) = (cx.beat)() {
        return Err(stop_held_before_notes(cx, parent, entries, failed, e, out, on_report));
    }
    if let Err(lost) = (cx.guard)() {
        // Nothing is written or removed: entry 0's temporary rides in the returned error, the others' are reported
        // (plan ruling 2). A failed-sync entry is reported once, with its sync failure and its temporary as leftover,
        // in place of the lost-lock report; as entry 0 its report comes first and the returned error carries no
        // leftover (plan ruling 9).
        let mut rest = entries.into_iter().zip(failed);
        let (p0, f0) = rest.next().expect("the batch is not empty");
        if let Some(f) = &f0 {
            report_sync_kept(cx.key_prefix, p0.path.clone(), &p0.staged.temp, f, out, on_report);
        }
        for (p, f) in rest {
            match f {
                Some(f) => {
                    report_sync_kept(cx.key_prefix, p.path, &p.staged.temp, &f, out, on_report)
                }
                None => report_kept(cx.key_prefix, p.path, &p.staged.temp, &lost, out, on_report),
            }
        }
        let stop = match f0 {
            Some(_) => CopyError::at(CopyStep::Publish, lost),
            None => kept(cx.key_prefix, &p0.path, &p0.staged.temp, lost),
        };
        return Err(stop);
    }
    // Cut 9e, plan ruling 9: a failed sync is discarded and reported BEFORE the notes, so no note ever names it.
    let mut ok = Vec::with_capacity(entries.len());
    let mut bad = Vec::new();
    for (p, f) in entries.into_iter().zip(failed) {
        match f {
            Some(f) => bad.push((p, f)),
            None => ok.push(p),
        }
    }
    for (p, f) in bad {
        let failed_copy = discard(
            parent,
            &p.staged.temp,
            CopyStep::Durability,
            Code::StrictDurabilityUnavailable,
            f.source,
            cx.guard,
        );
        // A stop cannot fire here today: `discard` keeps the sync's code (`StrictDurabilityUnavailable`) and reports a lost
        // lock as a kept leftover. If it ever does, the flush stops and the remaining entries' temporaries are left for
        // the resume sweep, like any crash before the notes.
        finish_copy(cx.key_prefix, p.path, Err(failed_copy), out, on_report)?;
    }
    if ok.is_empty() {
        return Ok(());
    }
    let entries = ok;
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
            let mut rest = entries.into_iter().zip(&notes);
            while let Some((p, (key, record))) = rest.next() {
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
                        // A stop (an error carrying `TargetLockBusy`) drops no entry: as at STOP-LOST, the noted keep
                        // their notes and temporaries, the rest their temporaries, each reported (plan ruling 2).
                        if let Err(stop) =
                            finish_copy(cx.key_prefix, p.path, Err(failed), out, on_report)
                        {
                            for q in noted.into_iter().chain(rest.map(|(q, _)| q)) {
                                report_kept(
                                    cx.key_prefix,
                                    q.path,
                                    &q.staged.temp,
                                    &stop.cause,
                                    out,
                                    on_report,
                                );
                            }
                            return Err(stop);
                        }
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
        let Pending { path, name, key, planned, template: _, mut staged, target, replaced } = p;
        let publish = if replaced.is_some() { Publish::Replace } else { Publish::NoReplace };
        // Cut 9f-1: taken before `publish_staged` consumes the staged file.
        let reuse = Reuse::take(&mut staged);
        match publish_staged(parent, &name, staged, publish, cx.guard, cx.beat) {
            Ok(o) => {
                let identity = o.published_identity;
                let recopied = o.bytes_copied;
                // Counted at the rename, before any later entry is tried (decision 4.6). Never `Err` for an `Ok`.
                finish_copy(cx.key_prefix, path.clone(), Ok(o), out, on_report)?;
                note_reuse(out, &path, reuse, recopied);
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
                let Err(stop) = finish_copy(cx.key_prefix, path.clone(), Err(e), out, on_report)
                else {
                    ops.push(RecoveryOp::Discard { key });
                    paths.push(path);
                    continue;
                };
                if stop.code() == Code::TargetLockBusy {
                    // STOP-LOST: write and remove nothing more. The renamed keep their notes (no `apply_recovery`),
                    // this entry's temporary rides in `stop`, the later ones' are reported (plan ruling 2).
                    for q in rest {
                        report_kept(
                            cx.key_prefix,
                            q.path,
                            &q.staged.temp,
                            &stop.cause,
                            out,
                            on_report,
                        );
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
                        report_leftover(cx.key_prefix, q.path, failed, out, on_report);
                    }
                    ops.push(RecoveryOp::Discard { key: q.key });
                }
                // Cut 11c (spec 4.3 site 2): the lock may have gone during the discards above. Then nothing more is
                // written: the renamed keep their notes (recovery row 1), the stop that began this arm is reported for
                // its entry (with the leftover it carries, already relative to DEST), and the lost lock is the stop,
                // with no leftover of its own.
                if let Err(lost) = (cx.guard)() {
                    report(out, on_report, path, TreeFailureCause::Copy(stop));
                    return Err(CopyError::at(CopyStep::Publish, lost));
                }
                let _ = claims.borrow_mut().apply_recovery(&ops);
                return Err(stop);
            }
        }
    }
    // 4.4-4.5: one transaction settles the batch, skipped only when it would carry no op.
    if !ops.is_empty() {
        // Cut 11c (spec 4.3 site 3): a batch with a failed entry re-checks the lock before it is settled; a batch with
        // no failure does not (N5: the same inherent window as an unbatched commit after its rename).
        let any_failed = ops.iter().any(|op| matches!(op, RecoveryOp::Discard { .. }));
        if any_failed && let Err(lost) = (cx.guard)() {
            return Err(CopyError::at(CopyStep::Publish, lost));
        }
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
fn kept(prefix: Option<&OsStr>, path: &Path, temp: &std::ffi::OsStr, cause: FsError) -> CopyError {
    CopyError {
        cause,
        leftover: Some((
            keyed_under(prefix, &path.with_file_name(temp)),
            std::io::Error::other("kept: this run no longer holds the destination's lock"),
        )),
        step: CopyStep::Publish,
    }
}

/// Plan ruling 2: an entry after the one a lost lock stopped at is reported as a `TargetLockBusy` copy failure whose
/// leftover is its temporary. (`finish_copy` cannot report it: it returns a `TargetLockBusy` failure as a stop.)
fn report_kept(
    prefix: Option<&OsStr>,
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
    let e = kept(prefix, &path, temp, cause);
    report(out, on_report, path, TreeFailureCause::Copy(e));
}

/// Cut 9e, plan ruling 9: an entry whose data sync failed, when the lock was lost before its discard: reported once as
/// its own `StrictDurabilityUnavailable` failure at `CopyStep::Durability`, with the sync's error as the cause and its
/// kept temporary (relative to DEST) as the leftover. `report_kept`'s shape, for a sync failure.
fn report_sync_kept(
    prefix: Option<&OsStr>,
    path: PathBuf,
    temp: &std::ffi::OsStr,
    sync: &FsError,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) {
    let e = CopyError {
        cause: FsError::new(
            Code::StrictDurabilityUnavailable,
            std::io::Error::new(sync.source.kind(), sync.source.to_string()),
        ),
        leftover: Some((
            keyed_under(prefix, &path.with_file_name(temp)),
            std::io::Error::other("kept: this run no longer holds the destination's lock"),
        )),
        step: CopyStep::Durability,
    };
    report(out, on_report, path, TreeFailureCause::Copy(e));
}

/// Cut 9e: STOP-HELD before any note exists (the heartbeat failed, during the barrier or at the flush's opening pair):
/// the lock is held, so every temporary of the batch is removed (guarded). A failed-sync entry is reported as
/// `StrictDurabilityUnavailable` at `CopyStep::Durability` (with a leftover only if its removal failed); any other entry
/// only when its removal left a leftover. Returns the stop, at `CopyStep::Heartbeat`.
fn stop_held_before_notes<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    parent: &F::Dir,
    entries: Vec<Pending<F::Writer>>,
    failed: Vec<Option<FsError>>,
    e: FsError,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> CopyError {
    for (p, f) in entries.into_iter().zip(failed) {
        match f {
            Some(f) => {
                let r = discard(
                    parent,
                    &p.staged.temp,
                    CopyStep::Durability,
                    Code::StrictDurabilityUnavailable,
                    f.source,
                    cx.guard,
                );
                report_leftover(cx.key_prefix, p.path, r, out, on_report);
            }
            None => {
                let source = copy_of(&e).source;
                let r =
                    discard(parent, &p.staged.temp, CopyStep::Heartbeat, e.code, source, cx.guard);
                if r.leftover.is_some() {
                    report_leftover(cx.key_prefix, p.path, r, out, on_report);
                }
            }
        }
    }
    CopyError::at(CopyStep::Heartbeat, e)
}

/// A temporary a stop's cleanup could not remove: reported with its leftover rebuilt relative to DEST, as
/// `finish_copy` rebuilds one (which would return a stop's failure instead of reporting it).
fn report_leftover(
    prefix: Option<&OsStr>,
    path: PathBuf,
    mut e: CopyError,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) {
    if let Some((p, _)) = e.leftover.as_mut() {
        *p = keyed_under(prefix, &path.with_file_name(&*p));
    }
    report(out, on_report, path, TreeFailureCause::Copy(e));
}

/// Cut 9e, owner ruling B2': what the drain does after the walk, by how the walk ended.
enum DrainMode {
    /// The walk finished, or failed in an ordinary way: every batch is flushed and published (cut 9d decision 5(e)).
    Publish,
    /// The walk stopped with a lost lock: nothing is written or removed (spec decision 8); every pending temporary is
    /// kept and reported with this error.
    KeepAll(FsError),
    /// The walk stopped with the lock held and nothing more to publish, by either of two triggers: a failed heartbeat
    /// (the copy's failure, cut 7b), or a destination without a no-replace primitive (cut 9d decision 3: the operation
    /// stops instead of failing every file). Every pending temporary is removed (guarded), with no barrier and no note.
    RemoveAll(FsError),
}

/// Cut 9d, spec decision 5(e): flush every live frame's batch, from the top of the stack down, WITHOUT popping (the
/// root frame goes back to `copy_tree_at`'s caller). The first stop is returned, unless the walk already failed or an
/// earlier frame stopped: such a later stop is dropped (spec decision 8), or reported at its leftover's path when it
/// carries one (plan ruling 2).
///
/// Cut 9e, owner ruling B2' (2026-10-10): after a lost-lock, heartbeat-failure or missing-publish-primitive stop the
/// drain performs that stop's cleanup only, with no data-sync barrier, no note and no rename. A failed heartbeat is the
/// copy's failure (cut 7b), and a missing primitive stops the operation (cut 9d decision 3), so the drain no longer
/// publishes after either. `mode` comes from how the walk ended OR from the first stop the drain itself hits: a stop
/// of any of the three classes at one frame's flush switches the mode for ALL the frames not yet drained (capstone
/// rounds 5 and 6).
fn drain_batches<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    stack: &mut [Frame<F::Dir>],
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
    walk_failed: bool,
    mode: DrainMode,
) -> std::result::Result<(), CopyError> {
    let mut mode = mode;
    let mut first: Option<CopyError> = None;
    for frame in stack.iter_mut().rev() {
        // A frame with no directory (dry) holds no batch.
        let Frame::Live { dir: Some(dir), names, batch, .. } = frame else {
            continue;
        };
        drain_one(cx, dir, names, batch, out, on_report, walk_failed, &mut mode, &mut first);
    }
    first.map_or(Ok(()), Err)
}

/// `drain_batches`' step for one directory's batch: `mode` and `first` carry what the earlier directories left.
// The signature is the loop body's state: the directory, its two pieces of frame state, and the drain's own.
#[allow(clippy::too_many_arguments)]
fn drain_one<F: DestinationRoot>(
    cx: &Shared<'_, F>,
    dir: &F::Dir,
    names: &mut NameIndex,
    batch: &mut Batch<F::Writer>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
    walk_failed: bool,
    mode: &mut DrainMode,
    first: &mut Option<CopyError>,
) {
    match &*mode {
        DrainMode::Publish => {}
        DrainMode::KeepAll(lost) => {
            for mut p in std::mem::take(batch).pending {
                p.staged.writer = None;
                report_kept(cx.key_prefix, p.path, &p.staged.temp, lost, out, on_report);
            }
            return;
        }
        DrainMode::RemoveAll(cause) => {
            let mut entries = std::mem::take(batch).pending;
            for p in &mut entries {
                p.staged.writer = None;
            }
            if !entries.is_empty() {
                let failed: Vec<Option<FsError>> = entries.iter().map(|_| None).collect();
                // The returned stop is the walk's own error, already returned; its cleanup reported what it left.
                let _ = stop_held_before_notes(
                    cx,
                    dir,
                    entries,
                    failed,
                    copy_of(cause),
                    out,
                    on_report,
                );
            }
            return;
        }
    }
    if let Err(e) = flush_batch(cx, dir, names, batch, out, on_report) {
        // A stop that first appears here switches the mode for the frames not yet drained (owner ruling B2').
        if e.code() == Code::TargetLockBusy {
            *mode = DrainMode::KeepAll(copy_of(&e.cause));
        } else if e.step == CopyStep::Heartbeat || e.code() == Code::NoReplacePublishUnavailable {
            *mode = DrainMode::RemoveAll(copy_of(&e.cause));
        }
        if walk_failed || first.is_some() {
            // Spec decision 8: the stop is dropped (the returned error already says it), unless it carries a
            // temporary, which must still reach the run as a leftover (plan ruling 2).
            if let Some((path, _)) = &e.leftover {
                // The leftover is relative to DEST (under the root's prefix); `TreeFailure.path` is relative to the root.
                let path = match cx.key_prefix {
                    Some(prefix) => path.strip_prefix(prefix).unwrap_or(path).to_path_buf(),
                    None => path.clone(),
                };
                report(out, on_report, path, TreeFailureCause::Copy(e));
            }
        } else {
            *first = Some(e);
        }
    }
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
fn write_note<C: ClaimStore + ?Sized>(
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

/// Cut 9c, amended by cut 11c (spec 4.2): a copy that failed after its note was written (the heartbeat, the guard or
/// the rename) did not publish, so the note is discarded at once, before `finish_copy` can stop the run, but only while
/// this run still holds the destination lock. Best effort: a note left behind is decided by the recovery matrix at the
/// next resume. Returns the copy's result, replaced by a `TargetLockBusy` stop when the guard fails.
///
/// - `Ok`, or no note written: the result is returned unchanged, with no guard call.
/// - A `TargetLockBusy` failure: the lock is already known lost; returned as is, no discard, no re-check.
/// - Any other failure with the guard `Ok`: the note is discarded and the failure returned (cut 9c).
/// - Any other failure with the guard failing: the note is kept for recovery; the original failure is reported as the
///   entry's own (with its collision remap), and the stop is the lost lock, carrying the original's step and leftover.
#[allow(clippy::too_many_arguments)]
fn discard_note_on_failure<C: ClaimStore + ?Sized>(
    claims: &std::cell::RefCell<C>,
    key: &ClaimKey,
    wrote: &std::cell::Cell<bool>,
    guard: &Guard<'_>,
    path: &Path,
    result: std::result::Result<flux_fs::Outcome, CopyError>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<flux_fs::Outcome, CopyError> {
    let Err(mut e) = result else { return result };
    if !wrote.get() || e.code() == Code::TargetLockBusy {
        return Err(e);
    }
    match guard() {
        Ok(()) => {
            let _ = claims.borrow_mut().discard_prepared(key);
            Err(e)
        }
        Err(lost) => {
            let step = e.step;
            let leftover = e.leftover.take();
            remap_collision(&mut e);
            report(out, on_report, path.to_path_buf(), TreeFailureCause::Copy(e));
            Err(CopyError { cause: lost, leftover, step })
        }
    }
}

/// F3 and §241.5: an `AlreadyExists` at the gate or the publish is a namespace collision - the name was taken by a
/// file that already existed (refused at Step 2a) or one that appeared before publish (the no-replace rename refused
/// it). `source` is kept, so `raw_os_error` survives.
fn remap_collision(e: &mut CopyError) {
    if matches!(e.step, CopyStep::Gate | CopyStep::Publish)
        && e.cause.source.kind() == ErrorKind::AlreadyExists
    {
        e.cause.code = Code::DestinationNamespaceCollision;
    }
}

/// Insert `record` at `key`; an existing record of the SAME target is fine (a resumed target), another target's is an
/// engine defect, reported as a claim that could not be recorded.
fn record_claim<C: ClaimStore + ?Sized>(
    claims: &std::cell::RefCell<C>,
    key: &ClaimKey,
    record: &ClaimRecord,
) -> std::result::Result<(), FsError> {
    // Bound first: the store's borrow ends here, before an upgrade borrows it again.
    let found = claims.borrow_mut().insert_if_absent(key, record)?;
    match found {
        ClaimOutcome::Inserted => Ok(()),
        // Cut 10a (spec 3.2 step 5): a present own `Existing` claim, when `record` is `Created`, is upgraded, so a New
        // publish that finds its own `Existing` claim (an earlier attempt's) leaves `Created` for the next `--resume`.
        ClaimOutcome::Present(r)
            if r.target == record.target
                && r.status == ClaimStatus::Existing
                && record.status == ClaimStatus::Created =>
        {
            claims.borrow_mut().upgrade_own_claim(key, &record.target)
        }
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
    prefix: Option<&OsStr>,
    path: PathBuf,
    result: std::result::Result<flux_fs::Outcome, CopyError>,
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    match result {
        Ok(o) => {
            debug_assert!(!o.skipped, "the tree never lets the copy path skip");
            out.note_copied();
            if o.verified {
                out.files_verified += 1;
            }
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
            // the tree's frame, relative to the destination root (under the root's prefix, for several sources).
            if let Some((p, _)) = e.leftover.as_mut() {
                *p = keyed_under(prefix, &path.with_file_name(&*p));
            }
            // §99 (cut 7a Part 3b): this run no longer owns the destination's lock; or §101 (cut 7b): its heartbeat
            // failed; or (cut 9f-1) a checkpoint or its writer failed. The whole operation stops.
            if e.code() == Code::TargetLockBusy
                || e.code() == Code::WalWriteFailed
                || e.step == CopyStep::Heartbeat
            {
                return Err(e);
            }
            // Decision 3: this destination lacks a no-replace primitive. Found at the
            // FIRST publish, so the whole operation stops instead of failing every file.
            if e.step == CopyStep::Publish && primitive_unavailable(&e.cause.source) {
                e.cause.code = Code::NoReplacePublishUnavailable;
                return Err(e);
            }
            remap_collision(&mut e);
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

/// Which operand of a containment check a failure was about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainmentTarget {
    /// The open handle passed as `anchor`.
    Anchor,
    /// The open handle passed as `other`.
    Other,
}

/// A containment failure and the operand its failing query was about. A refusal (`SafetyRejected`) and a strict
/// degraded exit carry `Anchor`: they are about the check as a whole, and every caller discards the tag for them.
#[derive(Debug)]
pub(crate) struct ContainmentFailure {
    pub target: ContainmentTarget,
    pub error: CopyError,
}

/// Part A on two OPEN handles: the resolved `anchor` must not be `other` or lie inside it. `anchor_shown` and
/// `other_shown` name them in the degraded warning. Runs after the identity pre-flight. Each `canonical_of` failure is
/// tagged with the handle it was about, which is what `flux verify` needs to name the right root.
pub(crate) fn containment_sided<F: DestinationRoot>(
    fs: &F,
    anchor: &F::Dir,
    anchor_shown: &Path,
    other: &F::Dir,
    other_shown: &Path,
    safety: Safety,
    out: &mut TreeOutcome,
) -> std::result::Result<(), ContainmentFailure> {
    let fail =
        |target, e| ContainmentFailure { target, error: CopyError::at(CopyStep::Resolve, e) };
    let degraded = |target: ContainmentTarget, shown: &Path, out: &mut TreeOutcome| match safety {
        Safety::Default => {
            out.containment_degraded.get_or_insert_with(|| shown.to_path_buf());
            Ok(())
        }
        Safety::Strict => Err(ContainmentFailure {
            target,
            error: refuse(
                "the destination's location cannot be resolved from its handle, so containment cannot be checked with full confidence",
            ),
        }),
    };
    let Some(a) = canonical_of(fs, anchor).map_err(|e| fail(ContainmentTarget::Anchor, e))? else {
        return degraded(ContainmentTarget::Anchor, anchor_shown, out);
    };
    let Some(s) = canonical_of(fs, other).map_err(|e| fail(ContainmentTarget::Other, e))? else {
        return degraded(ContainmentTarget::Other, other_shown, out);
    };
    if lexically_within(&a.path, &s.path) {
        return Err(ContainmentFailure {
            target: ContainmentTarget::Anchor,
            error: refuse("the destination's resolved location is the source or lies inside it"),
        });
    }
    if !(a.confirmed && s.confirmed) {
        return degraded(ContainmentTarget::Anchor, anchor_shown, out);
    }
    Ok(())
}

/// Part A for the copy callers: the resolved destination anchor must not be the source root or lie inside it. `anchor`
/// is the handle the run then writes through; `anchor_shown` names it in the warning. Opens `src_root`, runs
/// `containment_sided` with it as `other`, and drops the tag. Opening `src_root` fails as `CopyStep::Resolve`.
pub(crate) fn containment<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    anchor: &F::Dir,
    anchor_shown: &Path,
    safety: Safety,
    out: &mut TreeOutcome,
) -> std::result::Result<(), CopyError> {
    let src = fs.destination_root(src_root).map_err(|e| CopyError::at(CopyStep::Resolve, e))?;
    containment_sided(fs, anchor, anchor_shown, &src, src_root, safety, out).map_err(|f| f.error)
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
    if matches!(cause, TreeFailureCause::SymlinkSkipped) {
        out.symlinks_skipped += 1;
    }
    if matches!(cause, TreeFailureCause::MountSkipped) {
        out.mounts_skipped += 1;
    }
    if matches!(&cause, TreeFailureCause::Copy(e) if e.code() == Code::VerifyMismatch) {
        out.files_mismatched += 1;
    }
    out.failures.count(&cause);
    on_report(TreeFailure { path, cause });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use flux_fs::{Code, Durability, FileSystem, LinkKind, Links, ObjectId, OperationId, Preserve};
    use std::ffi::OsStr;

    #[test]
    fn the_tally_counts_each_cause_in_its_own_field() {
        let e = || FsError::new(Code::IoError, std::io::Error::other("x"));
        let mut t = FailureTally::default();
        t.count(&TreeFailureCause::Walk(e()));
        t.count(&TreeFailureCause::CreateDir(e()));
        t.count(&TreeFailureCause::Copy(CopyError::at(crate::copy::CopyStep::Create, e())));
        t.count(&TreeFailureCause::PublishedWithComplaints(Vec::new()));
        t.count(&TreeFailureCause::SpecialFileSkipped);
        t.count(&TreeFailureCause::SymlinkSkipped);
        t.count(&TreeFailureCause::MountSkipped);
        t.count(&TreeFailureCause::ClaimNotRecorded(e()));
        t.count(&TreeFailureCause::Planned(crate::dry_run::PlannedEntry {
            object: crate::dry_run::PlannedObject::File,
            action: crate::dry_run::PlannedAction::Copy,
        }));
        assert_eq!(
            t,
            FailureTally {
                walk: 1,
                create_dir: 1,
                copy: 1,
                published_with_complaints: 1,
                claim_not_recorded: 1
            }
        );
        assert_eq!(t.total(), 5, "a skipped special file or link is not a failure");
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
            links: Links::Copy,
            hardlinks: flux_fs::Hardlinks::Auto,
            cross_filesystems: false,
            verify: flux_fs::VerifyLevel::None,
            hash: flux_fs::HashAlgorithm::Blake3,
            selection: flux_fs::Selection::default(),
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

    /// A source directory `path` that is a mount root: it and every missing ancestor are created, one file `inside`
    /// is written in it, and the fake is told it is a mount root.
    fn mounted(fs: &FaultFs, path: &str) {
        let mut cur = PathBuf::new();
        for c in Path::new(path).components() {
            cur.push(c);
            if !fs.exists(&cur) {
                fs.create_dir(&cur).unwrap();
            }
        }
        fs.write_file(format!("{path}/inside"), b"x");
        fs.set_mount_root(path, flux_fs::MountRoot::Yes);
    }

    #[test]
    fn a_mount_is_an_empty_directory_reported_once_and_not_a_failure() {
        let fs = tree();
        mounted(&fs, "/src/m1");
        mounted(&fs, "/src/m2");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        for m in ["/dst/m1", "/dst/m2"] {
            assert!(fs.exists(m), "{m}");
            assert!(fs.read_dir(Path::new(m)).unwrap().is_empty(), "{m}");
        }
        assert!(!fs.exists("/dst/m1/inside"));
        assert_eq!(out.mounts_skipped, 2);
        assert_eq!(out.directories_created, 4);
        assert_eq!(out.files_total, 2);
        assert_eq!(out.files_copied, 2);
        assert_eq!(out.failures.total(), 0);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().all(|f| matches!(f.cause, TreeFailureCause::MountSkipped)), "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("m1"));
        assert_eq!(got[1].path, PathBuf::from("m2"));
        assert!(!fs.called("read_dir(/src/m1)"), "{:?}", fs.calls());
    }

    #[test]
    fn crossing_descends_into_a_mount_and_reports_nothing_skipped() {
        let fs = tree();
        mounted(&fs, "/src/m");
        let o = CopyOptions { cross_filesystems: true, ..opts() };

        let (r, got) = run(&fs, "/src", "/dst", &o);

        let out = r.unwrap();
        assert!(fs.exists("/dst/m/inside"));
        assert_eq!(out.mounts_skipped, 0);
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(out.files_copied, 3);
        assert!(!fs.called("source_mount_root("), "{:?}", fs.calls());
    }

    #[test]
    fn a_mount_whose_directory_cannot_be_created_reports_the_failure_only() {
        let fs = tree();
        mounted(&fs, "/src/m");
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/m", b"x");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("m"));
        assert!(matches!(got[0].cause, TreeFailureCause::CreateDir(_)), "{got:?}");
        assert_eq!(out.mounts_skipped, 0);
        assert!(!fs.exists("/dst/m/inside"));
    }

    #[test]
    fn a_mount_under_a_skipped_subtree_is_silent() {
        let fs = tree();
        mounted(&fs, "/src/.flux/operations/m");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from(".flux/operations"));
        assert!(matches!(got[0].cause, TreeFailureCause::CreateDir(_)), "{got:?}");
        assert_eq!(out.mounts_skipped, 0);
        assert!(!fs.exists("/dst/.flux/operations/m"));
    }

    #[test]
    fn a_mount_at_a_reserved_name_is_refused_by_the_reserved_arm() {
        let fs = tree();
        mounted(&fs, "/src/.flux/operations");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from(".flux/operations"));
        match &got[0].cause {
            TreeFailureCause::CreateDir(e) => {
                assert_eq!(e.code, Code::ControlPlaneNamespaceConflict)
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(out.mounts_skipped, 0);
        assert!(!fs.exists("/dst/.flux/operations"));
    }

    #[test]
    fn a_second_run_merges_into_the_empty_mount_directory() {
        // Only the mount: a plain `run` keys no claims, so a second run would report every file as a collision.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/")).unwrap();
        fs.create_dir(Path::new("/src")).unwrap();
        mounted(&fs, "/src/m");
        let (r, _) = run(&fs, "/src", "/dst", &opts());
        r.unwrap();

        let (r, got) = run(&fs, "/src", "/dst", &with_existing(flux_fs::ExistingPolicy::Update));

        let out = r.unwrap();
        assert_eq!(out.mounts_skipped, 1);
        assert_eq!(out.directories_created, 0);
        assert_eq!(out.failures.total(), 0, "{got:?}");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("m"));
        assert!(matches!(got[0].cause, TreeFailureCause::MountSkipped), "{got:?}");
        assert!(!fs.exists("/dst/m/inside"));
    }

    #[test]
    fn an_unknown_source_mount_warns_under_default_and_refuses_the_operation_under_strict() {
        let fs = tree();
        fs.set_mount_root("/src/sub", flux_fs::MountRoot::Unknown);
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(
            out.source_mount_unknown,
            Some(DegradedGroup { count: 1, example: PathBuf::from("sub") })
        );
        assert!(fs.exists("/dst/sub/b"));

        let fs = tree();
        fs.set_mount_root("/src/sub", flux_fs::MountRoot::Unknown);
        let (r, _) = run_full(&fs, "/src", "/dst", &strict());
        let abort = r.unwrap_err();
        assert_eq!(abort.error.code(), Code::SafetyRejected);
        assert!(abort.error.to_string().contains("cannot be told"), "{}", abort.error);
        assert_eq!(abort.outcome.directories_created, 2);
        assert!(!abort.refused_unchanged(), "the destination was changed, so the exit is 1");
        assert!(fs.exists("/dst/a"));

        // Under a reserved refusal the directory is not live, so the Unknown answer is ignored.
        let fs = tree();
        fs.create_dir(Path::new("/src/.flux")).unwrap();
        fs.create_dir(Path::new("/src/.flux/operations")).unwrap();
        fs.write_file("/src/.flux/operations/f", b"x");
        fs.set_mount_root("/src/.flux/operations", flux_fs::MountRoot::Unknown);
        let (r, _) = run(&fs, "/src", "/dst", &strict());
        let out = r.unwrap();
        assert!(out.source_mount_unknown.is_none());
    }

    #[test]
    fn a_skipped_mount_that_is_the_destination_itself_is_refused_by_identity() {
        let fs = tree();
        mounted(&fs, "/src/m");
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_identity("/src/m", identity_of(&fs, "/dst"));

        let (r, _) = run_full(&fs, "/src", "/dst", &opts());

        let abort = r.unwrap_err();
        assert_eq!(abort.error.code(), Code::SafetyRejected);
        assert!(abort.error.to_string().contains("the destination itself"), "{}", abort.error);
        assert_eq!(abort.outcome.mounts_skipped, 0);
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

    /// Cut 10a: `/` and `/src` holding only the links `(name, payload)`, kind `Unspecified`. `/dst` absent.
    fn links(entries: &[(&str, &str)]) -> FaultFs {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/")).unwrap();
        fs.create_dir(Path::new("/src")).unwrap();
        for (name, payload) in entries {
            fs.add_symlink_to(Path::new("/src").join(name), *payload, LinkKind::Unspecified);
        }
        fs
    }

    /// Cut 10a: the payload and kind of the link at `p`.
    fn link_at(fs: &FaultFs, p: &str) -> flux_fs::Link {
        FileSystem::read_link(fs, Path::new(p)).unwrap_or_else(|e| panic!("{p}: {e:?}"))
    }

    // Copy verification, task 4: the counters.

    fn verifying(level: flux_fs::VerifyLevel) -> CopyOptions {
        CopyOptions { verify: level, ..opts() }
    }

    /// The ordinal (1-based, among `open_read` calls) of the re-read of `temp` in a reference run's log.
    fn reread_ordinal(calls: &[String], temp: &str) -> u32 {
        let reads: Vec<&String> = calls.iter().filter(|c| c.starts_with("open_read(")).collect();
        let at = reads
            .iter()
            .position(|c| **c == format!("open_read({temp})"))
            .unwrap_or_else(|| panic!("no re-read of {temp} in {calls:?}"));
        at as u32 + 1
    }

    #[test]
    fn a_verified_file_is_counted_where_it_is_published() {
        let fs = sources(&[("f0", b"zero"), ("f1", b"one"), ("f2", b"two")]);
        let (r, got) = run(&fs, "/src", "/dst", &verifying(flux_fs::VerifyLevel::Destination));
        let out = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(out.files_copied, 3);
        assert_eq!(out.files_verified, 3);
        assert_eq!(out.files_mismatched, 0);
    }

    #[test]
    fn a_mismatch_fails_that_file_and_the_tree_goes_on() {
        let entries: [(&str, &[u8]); 4] =
            [("f0", b"zero"), ("f1", b"one"), ("f2", b"two"), ("f3", b"three")];
        let o = verifying(flux_fs::VerifyLevel::Destination);
        let refer = sources(&entries);
        run(&refer, "/src", "/dst", &o).0.unwrap();
        let n = reread_ordinal(&log(&refer), "/dst/f1.flux-partial.op1");
        assert_eq!(n, 4, "f1's re-read is the fourth open_read of the walk: {:?}", refer.calls());

        let fs = sources(&entries);
        fs.on_nth("open_read", n, |fs| fs.write_file("/dst/f1.flux-partial.op1", b"XXX"));
        // A fourth file whose temporary cannot be created: a Copy failure that is NOT a mismatch.
        let create = log(&refer)
            .iter()
            .filter(|c| c.starts_with("create_new("))
            .position(|c| c.starts_with("create_new(/dst/f3.flux-partial"));
        let create = create.expect("f3 is created in the reference run") as u32 + 1;
        fs.fail_nth("create_new", create, Code::IoError, std::io::ErrorKind::Other);

        let (r, got) = run(&fs, "/src", "/dst", &o);
        let out = r.unwrap();
        assert_eq!(out.files_copied, 2);
        assert_eq!(out.files_verified, 2);
        assert_eq!(out.files_mismatched, 1, "the IoError of f3 is not a mismatch");
        assert_eq!(out.failures.copy, 2);
        assert_eq!(got.len(), 2, "{got:?}");
        let f1 = got.iter().find(|f| f.path == Path::new("f1")).expect("f1 failed");
        let TreeFailureCause::Copy(e) = &f1.cause else { panic!("{f1:?}") };
        assert_eq!(e.code(), Code::VerifyMismatch);
        let f3 = got.iter().find(|f| f.path == Path::new("f3")).expect("f3 failed");
        let TreeFailureCause::Copy(e) = &f3.cause else { panic!("{f3:?}") };
        assert_eq!(e.code(), Code::IoError);
        assert!(fs.exists("/dst/f0") && fs.exists("/dst/f2"));
        assert!(!fs.exists("/dst/f1") && !fs.exists("/dst/f3"));
        assert!(temps_in(&fs, "/dst").is_empty(), "{:?}", temps_in(&fs, "/dst"));
    }

    #[test]
    fn none_and_source_stream_count_no_verified_files() {
        for level in [flux_fs::VerifyLevel::None, flux_fs::VerifyLevel::SourceStream] {
            let fs = sources(&[("f0", b"zero"), ("f1", b"one")]);
            let (r, got) = run(&fs, "/src", "/dst", &verifying(level));
            let out = r.unwrap();
            assert!(got.is_empty(), "{got:?}");
            assert_eq!((out.files_copied, out.files_verified), (2, 0), "{level:?}");
        }
    }

    #[test]
    fn a_skipped_file_is_not_verified() {
        let fs = sources(&[("f0", b"zero")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/f0", b"zero");
        let o = CopyOptions {
            existing: flux_fs::ExistingPolicy::SkipExisting,
            ..verifying(flux_fs::VerifyLevel::Destination)
        };
        let (r, out, got) = link_run(&fs, &o, false, policy(1), &unguarded, no_seed);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_skipped, out.files_copied, out.files_verified), (1, 0, 0));
    }

    #[test]
    fn a_barrier_sync_failure_uncounts_only_that_entry() {
        let fs = n_files(3);
        fs.on_nth("create_new", 2, |fs| fs.fail("sync_all", Code::StrictDurabilityUnavailable));
        let (r, out, got) = batched_verifying(
            &fs,
            one_worker(),
            Durability::Strict,
            &unguarded,
            &no_heartbeat,
            None,
            flux_fs::VerifyLevel::Destination,
        );
        r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(out.files_copied, 2);
        assert_eq!(out.files_verified, 2);
        assert_eq!(out.files_mismatched, 0);
    }

    /// Cut 10a: the names under `dir` holding a temporary (`.flux-partial.`).
    fn temps_in(fs: &FaultFs, dir: &str) -> Vec<OsString> {
        fs.read_dir(Path::new(dir))
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .filter(|n| n.to_string_lossy().contains(".flux-partial."))
            .collect()
    }

    /// Cut 10a: the one `Copy` record in `got`, for `path`.
    fn only_copy_failure<'a>(got: &'a [TreeFailure], path: &str) -> &'a CopyError {
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new(path));
        match &got[0].cause {
            TreeFailureCause::Copy(e) => e,
            other => panic!("expected Copy, got {other:?}"),
        }
    }

    #[test]
    fn links_are_recreated_with_their_payloads_and_count_as_copied_with_no_bytes() {
        // Spec test 1 (I1, I2). Mutant: the payload altered on create.
        let fs = links(&[("rel", "a/b"), ("abs", "/x/y"), ("dangling", "nowhere")]);
        fs.write_file("/src/f", b"FILE");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        for n in ["rel", "abs", "dangling"] {
            assert_eq!(
                link_at(&fs, &format!("/dst/{n}")),
                link_at(&fs, &format!("/src/{n}")),
                "{n}"
            );
        }
        assert_eq!(link_at(&fs, "/dst/rel").payload, "a/b");
        assert_eq!(fs.read_file("/dst/f").as_deref(), Some(&b"FILE"[..]));
        assert_eq!(out.files_copied, 4);
        assert_eq!(out.bytes_copied, 4, "only the file's bytes");
        assert_eq!(out.files_total, 4);
        assert!(out.failures.is_empty());
        assert!(temps_in(&fs, "/dst").is_empty());
        // I2: no target is ever opened or stat'ed, on either side.
        let c = log(&fs);
        for t in ["/src/a/b", "/x/y", "/src/nowhere", "/dst/a/b", "/dst/nowhere"] {
            assert!(
                !c.iter()
                    .any(|x| x == &format!("open_read({t})") || x == &format!("metadata({t})")),
                "{t} was followed: {c:?}"
            );
        }
    }

    #[test]
    fn an_existing_file_at_a_links_name_is_replaced_under_overwrite() {
        // Spec test 5, first row.
        let fs = links(&[("n", "payload")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/n", b"old");

        let (r, out, got) = batched(&fs, policy(1), Durability::Normal);

        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(link_at(&fs, "/dst/n").payload, "payload");
        assert_eq!((out.files_copied, out.files_overwritten), (1, 1));
        assert_eq!(
            fs.claim(strong_id(&fs, "/dst"), "n").map(|r| r.status),
            Some(ClaimStatus::Created)
        );
        assert!(temps_in(&fs, "/dst").is_empty());
    }

    #[test]
    fn a_directory_at_a_links_name_is_refused() {
        let fs = links(&[("n", "payload")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/n")).unwrap();
        fs.write_file("/dst/n/keep", b"K");

        let (r, out, got) = batched(&fs, policy(1), Durability::Normal);

        r.unwrap();
        let e = only_copy_failure(&got, "n");
        assert_eq!(e.code(), Code::DestinationError);
        assert_eq!(fs.read_file("/dst/n/keep").as_deref(), Some(&b"K"[..]), "the dir untouched");
        assert!(FileSystem::read_link(&fs, Path::new("/dst/n")).is_err());
        assert!(!fs.called("create_symlink("));
        assert_eq!(out.files_copied, 0);
    }

    /// Cut 10a test audit: a file entry and a link entry are one protocol, so the same scenario is run over both and
    /// must end the same way. Each arm of `link_one` duplicates `copy_one`'s; a fix made to one and not the other turns
    /// the arm for the other kind red here.
    #[test]
    fn a_file_and_a_link_entry_end_alike_at_each_step() {
        #[derive(Clone, Copy, Debug)]
        enum Entry {
            File,
            Link,
        }
        let seed = |entry: Entry| -> FaultFs {
            match entry {
                Entry::File => {
                    let fs = FaultFs::new();
                    fs.create_dir(Path::new("/")).unwrap();
                    fs.create_dir(Path::new("/src")).unwrap();
                    fs.write_file("/src/n", b"N");
                    fs
                }
                Entry::Link => links(&[("n", "payload")]),
            }
        };
        for entry in [Entry::File, Entry::Link] {
            // Success: one entry published, no temporary left.
            let fs = seed(entry);
            let (r, got) = run(&fs, "/src", "/dst", &opts());
            let out = r.unwrap();
            assert!(got.is_empty(), "{entry:?}: {got:?}");
            assert_eq!(out.files_copied, 1, "{entry:?}");
            assert!(fs.exists("/dst/n"), "{entry:?}");
            assert!(temps_in(&fs, "/dst").is_empty(), "{entry:?}");

            // A refused publish: reported at Publish, the temporary removed, nothing at the name.
            let fs = seed(entry);
            fs.fail("rename_no_replace", Code::IoError);
            let (r, got) = run(&fs, "/src", "/dst", &opts());
            let out = r.unwrap();
            let e = only_copy_failure(&got, "n");
            assert_eq!(e.step, CopyStep::Publish, "{entry:?}");
            assert!(e.leftover.is_none(), "{entry:?}: the temporary was removed");
            assert!(!fs.exists("/dst/n"), "{entry:?}");
            assert!(temps_in(&fs, "/dst").is_empty(), "{entry:?}");
            assert_eq!(out.files_copied, 0, "{entry:?}");

            // A directory at the name: refused, the directory untouched, nothing staged.
            let fs = seed(entry);
            fs.create_dir(Path::new("/dst")).unwrap();
            fs.create_dir(Path::new("/dst/n")).unwrap();
            fs.write_file("/dst/n/keep", b"K");
            let (r, got) = run(&fs, "/src", "/dst", &opts());
            let out = r.unwrap();
            let e = only_copy_failure(&got, "n");
            // Under `Publish::Replace` and `Overwrite` the identity gate refuses a directory first, for both kinds.
            assert_eq!(
                (e.code(), e.step),
                (Code::SafetyRejected, CopyStep::Gate),
                "{entry:?}: {e:?}"
            );
            assert_eq!(fs.read_file("/dst/n/keep").as_deref(), Some(&b"K"[..]), "{entry:?}");
            assert!(temps_in(&fs, "/dst").is_empty(), "{entry:?}");
            assert_eq!(out.files_copied, 0, "{entry:?}");
        }
    }

    #[test]
    fn a_link_and_a_file_folding_to_one_name_collide() {
        // Spec test 6, §241.5.
        let fs = links(&[("a", "payload")]);
        fs.write_file("/src/A", b"upper");
        fs.set_case_insensitive(true);

        let (r, out, got) = batched(&fs, policy(1), Durability::Normal);

        r.unwrap();
        let e = only_copy_failure(&got, "a");
        assert_eq!(e.code(), Code::DestinationNamespaceCollision);
        assert_eq!(fs.read_file("/dst/A").as_deref(), Some(&b"upper"[..]));
        let objects: Vec<OsString> = fs
            .read_dir(Path::new("/dst"))
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .filter(|n| n != "state.db")
            .collect();
        assert_eq!(objects, [OsString::from("A")], "exactly one object at the destination");
        assert!(!fs.called("create_symlink("));
        assert_eq!(out.files_copied, 1);
    }

    #[test]
    fn a_failed_read_link_is_a_source_step_failure() {
        let fs = tree();
        fs.add_symlink_to("/src/l", "payload", LinkKind::Unspecified);
        fs.fail("read_link", Code::IoError);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        let e = only_copy_failure(&got, "l");
        assert_eq!(e.step, CopyStep::Source);
        assert!(!fs.exists("/dst/l"));
        assert!(!fs.called("create_symlink("));
        assert_eq!((out.files_copied, out.failures.copy), (2, 1));
    }

    #[test]
    fn a_failed_create_symlink_leaves_nothing() {
        let fs = tree();
        fs.add_symlink_to("/src/l", "payload", LinkKind::Unspecified);
        fs.fail("create_symlink", Code::IoError);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        let e = only_copy_failure(&got, "l");
        assert_eq!(e.step, CopyStep::Create);
        assert!(e.leftover.is_none());
        assert!(!fs.exists("/dst/l"));
        assert!(temps_in(&fs, "/dst").is_empty());
        assert_eq!((out.files_copied, out.failures.copy), (2, 1));
    }

    #[test]
    fn an_unavailable_creation_is_reported_with_its_code_and_the_files_still_copy() {
        // Spec test 4. Mutant: the code mapped to IO_ERROR.
        let fs = links(&[("l1", "a"), ("l2", "/b"), ("l3", "c/d")]);
        fs.write_file("/src/f", b"F");
        fs.set_symlinks_unavailable(true);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
        for f in &got {
            let TreeFailureCause::Copy(e) = &f.cause else { panic!("{got:?}") };
            assert_eq!(e.code(), Code::SymlinkCreationUnavailable, "{:?}", f.path);
            assert_eq!(e.step, CopyStep::Create);
        }
        assert_eq!(out.failures.copy, 3);
        assert_eq!(out.files_copied, 1);
        assert_eq!(fs.read_file("/dst/f").as_deref(), Some(&b"F"[..]));
        assert!(temps_in(&fs, "/dst").is_empty(), "no temp names left");
        for n in ["l1", "l2", "l3"] {
            assert!(!fs.exists(format!("/dst/{n}")), "{n}");
        }
    }

    #[test]
    fn a_failed_rename_discards_the_temporary() {
        let fs = links(&[("n", "payload")]);
        fs.fail("rename_no_replace", Code::IoError);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        let e = only_copy_failure(&got, "n");
        assert_eq!(e.step, CopyStep::Publish);
        assert!(e.leftover.is_none(), "the temporary was removed");
        assert!(!fs.exists("/dst/n.flux-partial.op1"));
        assert!(!fs.exists("/dst/n"));
        assert!(
            fs.called("create_symlink(/dst/n.flux-partial.op1)"),
            "precondition: it was created"
        );
        assert_eq!(out.files_copied, 0);
    }

    #[test]
    fn a_reserved_root_link_is_refused_before_it_is_read() {
        // Spec test 10.
        let fs = links(&[]);
        fs.create_dir(Path::new("/src/.flux")).unwrap();
        fs.add_symlink_to("/src/.flux/operations", "/elsewhere", LinkKind::Unspecified);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        r.unwrap();
        let e = only_copy_failure(&got, ".flux/operations");
        assert_eq!(e.step, CopyStep::Gate);
        assert_eq!(e.code(), Code::ControlPlaneNamespaceConflict);
        assert!(!fs.called("read_link(/src/.flux/operations)"), "{:?}", fs.calls());
        assert!(!fs.called("create_symlink("));
    }

    #[test]
    fn a_root_dot_flux_link_follows_the_rule_a_root_dot_flux_file_follows() {
        // R8: only `.flux/operations`, `.flux/standalone`, `.flux/atomic` and below are reserved; a root `.flux` entry
        // is ordinary data, so the same name as a file and as a link is copied the same way.
        let file = links(&[]);
        file.write_file("/src/.flux", b"data");
        let (r, got) = run(&file, "/src", "/dst", &opts());
        let as_file = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(file.read_file("/dst/.flux").as_deref(), Some(&b"data"[..]));

        let link = links(&[(".flux", "/elsewhere")]);
        let (r, got) = run(&link, "/src", "/dst", &opts());
        let as_link = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(link_at(&link, "/dst/.flux").payload, "/elsewhere");
        assert!(link.called("read_link(/src/.flux)"), "not refused before the read");

        assert_eq!(
            (as_link.files_copied, as_link.failures.total()),
            (as_file.files_copied, as_file.failures.total()),
            "a link follows the file rule"
        );
        assert_eq!(as_link.files_copied, 1);
    }

    #[test]
    fn a_link_named_dot_flux_below_the_root_is_ordinary_data() {
        // Review Focus 4.
        let fs = tree();
        fs.add_symlink_to("/src/sub/.flux", "payload", LinkKind::Unspecified);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(link_at(&fs, "/dst/sub/.flux").payload, "payload");
        assert_eq!(out.files_copied, 3);
    }

    #[test]
    fn a_junction_classified_link_is_a_skipped_special_file() {
        // Spec test 13, D7.
        let fs = tree();
        fs.add_symlink_to("/src/j", "payload", LinkKind::Dir);
        fs.fail_kind("read_link", Code::SpecialFileUnsupported, std::io::ErrorKind::Other);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new("j"));
        assert!(matches!(got[0].cause, TreeFailureCause::SpecialFileSkipped));
        assert_eq!(out.special_files_skipped, 1);
        assert_eq!(out.failures.copy, 0, "files_failed is the copy failures");
        assert!(out.failures.is_empty());
        assert!(!fs.exists("/dst/j"));
        assert_eq!(out.files_copied, 2);
    }

    #[test]
    fn a_destination_that_is_the_source_link_by_identity_is_refused() {
        // Spec test 11.
        let fs = links(&[("l", "payload")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/l", b"keep");
        let same = FileIdentity::Strong(ObjectId { volume: 77, index: 7 });
        fs.set_identity("/src/l", same);
        fs.set_identity("/dst/l", same);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        let e = only_copy_failure(&got, "l");
        assert_eq!(e.code(), Code::SafetyRejected);
        assert_eq!(e.step, CopyStep::Gate);
        assert_eq!(fs.read_file("/dst/l").as_deref(), Some(&b"keep"[..]));
        assert!(!fs.called("create_symlink("));
        assert_eq!(out.files_copied, 0);
    }

    /// Cut 10a task 4: `batched_with` under the copy options `o` (durability included), `--resume` as `resume`, and
    /// `seed` run on the fresh claim store, with `/dst`'s id, before the walk.
    fn link_run(
        fs: &FaultFs,
        o: &CopyOptions,
        resume: bool,
        policy: BatchPolicy,
        guard: &Guard<'_>,
        seed: impl FnOnce(&mut crate::fault_fs::FakeClaimStore, ObjectId),
    ) -> (std::result::Result<(), CopyError>, TreeOutcome, Vec<TreeFailure>) {
        let source =
            prepare_source(fs, Path::new("/src"), Path::new("/dst"), MountPolicy::Cross).unwrap();
        if !fs.exists(Path::new("/dst")) {
            fs.create_dir(Path::new("/dst")).unwrap();
        }
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let mut store = root.create_claim_store(OsStr::new("state.db"), o.durability).unwrap();
        seed(&mut store, strong_id(fs, "/dst"));
        let claims = std::cell::RefCell::new(store);
        let cx = Shared {
            fs,
            src_root: Path::new("/src"),
            src_identity: source.identity,
            opts: o,
            guard,
            beat: &no_heartbeat,
            claims: Some(&claims),
            resume,
            batch: policy,
            ckpt: None,
            dry_run: false,
            dry_prior: None,
            dry_partials: None,
            key_prefix: None,
            hardlinks: None,
            anchor_root: None,
        };
        let mut out = TreeOutcome::default();
        let mut got = Vec::new();
        let (_root, r) =
            copy_tree_at(&cx, source.events, root, false, &mut out, &mut |f| got.push(f));
        (r, out, got)
    }

    fn no_seed(_: &mut crate::fault_fs::FakeClaimStore, _: ObjectId) {}

    fn with_existing(policy: flux_fs::ExistingPolicy) -> CopyOptions {
        CopyOptions { existing: policy, ..opts() }
    }

    fn key_of(rel: &str) -> FluxPathKey {
        FluxPathKey::from_relative(Path::new(rel)).unwrap()
    }

    #[test]
    fn update_skips_an_equal_link_and_replaces_a_different_one_or_a_file() {
        // Spec 3.3 and test 5. Mutant: `--update` comparing payload lengths (`diff` has an equal-length payload).
        let fs = links(&[("diff", "abcd"), ("eq", "same/x"), ("file", "p"), ("kind", "k")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.add_symlink_to("/dst/eq", "same/x", LinkKind::Unspecified);
        fs.add_symlink_to("/dst/diff", "wxyz", LinkKind::Unspecified);
        fs.write_file("/dst/file", b"old");
        fs.add_symlink_to("/dst/kind", "k", LinkKind::File);
        let eq_identity = identity_of(&fs, "/dst/eq");

        let o = with_existing(flux_fs::ExistingPolicy::Update);
        let (r, out, got) = link_run(&fs, &o, false, policy(1), &unguarded, no_seed);

        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(out.files_skipped, 1, "only the equal link");
        assert_eq!(out.bytes_skipped, 0);
        assert_eq!((out.files_copied, out.files_overwritten), (3, 3));
        assert_eq!(link_at(&fs, "/dst/eq").payload, "same/x");
        assert_eq!(identity_of(&fs, "/dst/eq"), eq_identity, "the equal link untouched");
        assert!(!fs.called("create_symlink(/dst/eq.flux-partial.op1)"), "{:?}", fs.calls());
        assert_eq!(link_at(&fs, "/dst/diff").payload, "abcd", "equal length, different payload");
        assert_eq!(link_at(&fs, "/dst/file").payload, "p", "a file at the name is replaced");
        assert_eq!(
            link_at(&fs, "/dst/kind"),
            flux_fs::Link { payload: "k".into(), kind: LinkKind::Unspecified },
            "the same payload with another kind is different"
        );
        assert!(temps_in(&fs, "/dst").is_empty());
    }

    #[test]
    fn update_treats_an_unreadable_destination_link_as_different() {
        // Review Focus 2: a failed `read_link` of the destination is "not equal", never a failed entry.
        let fs = links(&[("n", "p")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.add_symlink_to("/dst/n", "p", LinkKind::Unspecified);
        // The source read is the first `read_link`, the destination's the second.
        fs.fail_nth("read_link", 2, Code::SpecialFileUnsupported, ErrorKind::Other);

        let o = with_existing(flux_fs::ExistingPolicy::Update);
        let (r, out, got) = link_run(&fs, &o, false, policy(1), &unguarded, no_seed);

        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        let reads: Vec<String> =
            log(&fs).into_iter().filter(|c| c.starts_with("read_link(")).collect();
        assert_eq!(reads[..2], ["read_link(/src/n)", "read_link(/dst/n)"], "{reads:?}");
        assert_eq!((out.files_skipped, out.files_copied, out.files_overwritten), (0, 1, 1));
        assert!(fs.called("create_symlink(/dst/n.flux-partial.op1)"));
        assert_eq!(link_at(&fs, "/dst/n").payload, "p");
    }

    #[test]
    fn skip_existing_skips_whatever_holds_the_name() {
        // Spec 3.3: a file, a link and a dangling link at the name.
        let fs = links(&[("d", "p"), ("f", "p"), ("l", "p")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/f", b"old");
        fs.add_symlink_to("/dst/l", "other", LinkKind::Unspecified);
        fs.add_symlink_to("/dst/d", "/nowhere/at/all", LinkKind::Unspecified);

        let o = with_existing(flux_fs::ExistingPolicy::SkipExisting);
        let (r, out, got) = link_run(&fs, &o, false, policy(1), &unguarded, no_seed);

        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_skipped, out.files_copied, out.bytes_skipped), (3, 0, 0));
        assert!(!fs.called("create_symlink("));
        assert_eq!(fs.read_file("/dst/f").as_deref(), Some(&b"old"[..]));
        assert_eq!(link_at(&fs, "/dst/l").payload, "other");
        assert_eq!(link_at(&fs, "/dst/d").payload, "/nowhere/at/all");
    }

    #[test]
    fn a_created_link_claim_with_the_same_payload_is_resumed_and_a_different_one_republished() {
        // Spec 3.4 and test 7. Mutant: resume trusting the claim alone.
        let seeded = |status| {
            move |s: &mut crate::fault_fs::FakeClaimStore, dst: ObjectId| {
                let record = ClaimRecord { target: key_of("n"), status };
                s.insert_if_absent(&ClaimKey::new(dst, OsStr::new("n")), &record).unwrap();
            }
        };

        let fs = links(&[("n", "p")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.add_symlink_to("/dst/n", "p", LinkKind::Unspecified);
        let (r, out, got) =
            link_run(&fs, &opts(), true, policy(1), &unguarded, seeded(ClaimStatus::Created));
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_resumed, out.bytes_resumed, out.files_copied), (1, 0, 0));
        assert!(!fs.called("create_symlink("), "{:?}", fs.calls());

        let fs = links(&[("n", "p")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.add_symlink_to("/dst/n", "q", LinkKind::Unspecified);
        let (r, out, got) =
            link_run(&fs, &opts(), true, policy(1), &unguarded, seeded(ClaimStatus::Created));
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_resumed, out.files_copied, out.files_overwritten), (0, 1, 1));
        assert_eq!(link_at(&fs, "/dst/n").payload, "p", "republished");

        // An `Existing` claim (an earlier attempt that never published) is not resumed, even with an equal link.
        let fs = links(&[("n", "p")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.add_symlink_to("/dst/n", "p", LinkKind::Unspecified);
        let (r, out, got) =
            link_run(&fs, &opts(), true, policy(1), &unguarded, seeded(ClaimStatus::Existing));
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_resumed, out.files_copied), (0, 1));
    }

    #[test]
    fn a_resumed_link_owns_its_name_for_the_rest_of_the_directory() {
        // Spec 3.4: `names.record_publication` on resume, so a later entry folding to the name meets §241.5's
        // `names.owner` refusal before it reads the claim store (`A` sorts before `a`).
        let fs = links(&[("A", "p")]);
        fs.write_file("/src/a", b"lower");
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.add_symlink_to("/dst/A", "p", LinkKind::Unspecified);
        fs.set_case_insensitive(true);
        let seed = |s: &mut crate::fault_fs::FakeClaimStore, dst: ObjectId| {
            let record = ClaimRecord { target: key_of("A"), status: ClaimStatus::Created };
            s.insert_if_absent(&ClaimKey::new(dst, OsStr::new("A")), &record).unwrap();
        };
        let (r, out, got) = link_run(&fs, &opts(), true, policy(1), &unguarded, seed);
        r.unwrap();
        assert_eq!(out.files_resumed, 1);
        let e = only_copy_failure(&got, "a");
        assert_eq!((e.code(), e.step), (Code::DestinationNamespaceCollision, CopyStep::Claim));
        let c = log(&fs);
        assert_eq!(count(&c, "claim_get(A)"), 1, "the file is refused by the owner check: {c:?}");
        assert_eq!(link_at(&fs, "/dst/A").payload, "p");
        assert_eq!(out.files_copied, 0);
    }

    #[test]
    fn a_strict_run_notes_the_link_before_the_rename_and_commits_after() {
        // Spec test 8. Mutant: commit without the note.
        let fs = links(&[("n", "p")]);
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let at_rename = std::sync::Arc::clone(&seen);
        fs.on_nth("rename_no_replace", 1, move |fs| {
            at_rename.store(fs.prepared_count(), std::sync::atomic::Ordering::SeqCst);
        });
        let o = CopyOptions { durability: Durability::Strict, ..opts() };

        let (r, out, got) = link_run(&fs, &o, false, policy(1), &unguarded, no_seed);

        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the note precedes the rename"
        );
        assert_eq!(fs.prepared_count(), 0, "committed after");
        assert_eq!(
            fs.claim(strong_id(&fs, "/dst"), "n").map(|r| r.status),
            Some(ClaimStatus::Created)
        );
        let c = log(&fs);
        assert!(
            first(&c, "claim_prepare(") < renamed(&c, "n")
                && renamed(&c, "n") < first(&c, "claim_commit_prepared("),
            "{c:?}"
        );
        assert_eq!(out.files_copied, 1);

        let fs = links(&[("n", "p")]);
        fs.fail_nth("rename_no_replace", 1, Code::IoError, ErrorKind::Other);
        let (r, out, got) = link_run(&fs, &o, false, policy(1), &unguarded, no_seed);
        r.unwrap();
        let e = only_copy_failure(&got, "n");
        assert_eq!(e.step, CopyStep::Publish);
        assert!(fs.called("claim_prepare("), "precondition: the note was written");
        assert_eq!(fs.prepared_count(), 0, "the note is discarded");
        assert!(temps_in(&fs, "/dst").is_empty(), "the temporary is discarded");
        assert!(!fs.exists("/dst/n"));
        assert_eq!(out.files_copied, 0);
    }

    #[test]
    fn a_pending_batch_is_flushed_before_a_link_touches_an_occupied_temp_name() {
        // Spec test 4c: a batched file `A` pending, then a link `a` on a folding destination, with the ASCII pre-check
        // off (as `a_fold_equal_pending_temporary_is_never_swept`), so only the temp probe can flush. Mutant: the probe's
        // flush removed (the link's leftover removal unlinks the file's temporary and its publish fails).
        let fs = sources(&[("A", b"upper")]);
        fs.add_symlink_to("/src/a", "p", LinkKind::Unspecified);
        fs.set_case_insensitive(true);
        let o = CopyOptions { durability: Durability::Strict, ..opts() };
        let p = BatchPolicy { fold_precheck: false, ..policy(64) };

        let (r, out, got) = link_run(&fs, &o, false, p, &unguarded, no_seed);

        r.unwrap();
        let c = log(&fs);
        assert_eq!(fs.read_file("/dst/A").as_deref(), Some(&b"upper"[..]), "{c:?}");
        let e = only_copy_failure(&got, "a");
        assert_eq!(e.code(), Code::DestinationNamespaceCollision, "{e:?}");
        // Between the file's staging and its publish, nothing removes a temporary under either spelling.
        let (staged, published) = (created(&c, "A"), renamed(&c, "A"));
        for removal in
            ["remove_file(/dst/A.flux-partial.op1)", "remove_file(/dst/a.flux-partial.op1)"]
        {
            assert!(
                positions(&c, removal).iter().all(|&i| i < staged || i > published),
                "{removal}: {c:?}"
            );
        }
        assert!(published < first(&c, "create_symlink(/dst/a.flux-partial.op1)"), "{c:?}");
        assert_eq!(out.files_copied, 1);
        assert!(temps_in(&fs, "/dst").is_empty(), "{c:?}");
    }

    #[test]
    fn a_link_create_refused_already_exists_with_pending_entries_flushes_and_retries_once() {
        // Spec 3.2 step 5, the second backstop.
        let fs = sources(&[("a", b"A")]);
        fs.add_symlink_to("/src/b", "p", LinkKind::Unspecified);
        fs.fail_nth("create_symlink", 1, Code::IoError, ErrorKind::AlreadyExists);
        let o = CopyOptions { durability: Durability::Strict, ..opts() };

        let (r, out, got) = link_run(&fs, &o, false, policy(64), &unguarded, no_seed);

        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        let c = log(&fs);
        let creates = positions(&c, "create_symlink(/dst/b.flux-partial.op1)");
        assert_eq!(creates.len(), 2, "{c:?}");
        assert!(creates[0] < renamed(&c, "a") && renamed(&c, "a") < creates[1], "{c:?}");
        assert_eq!(link_at(&fs, "/dst/b").payload, "p");
        assert_eq!(out.files_copied, 2);

        // A collision that persists is retried once only, and fails the entry at `Create`.
        let fs = sources(&[("a", b"A")]);
        fs.add_symlink_to("/src/b", "p", LinkKind::Unspecified);
        fs.fail_kind("create_symlink", Code::IoError, ErrorKind::AlreadyExists);
        fs.fail_always("create_symlink", Code::IoError);
        let (r, out, got) = link_run(&fs, &o, false, policy(64), &unguarded, no_seed);
        r.unwrap();
        let c = log(&fs);
        assert_eq!(positions(&c, "create_symlink(/dst/b.flux-partial.op1)").len(), 2, "{c:?}");
        let e = only_copy_failure(&got, "b");
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Create), "{e:?}");
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"A"[..]));
        assert_eq!(out.files_copied, 1);

        // With nothing pending there is no retry.
        let fs = links(&[("b", "p")]);
        fs.fail_nth("create_symlink", 1, Code::IoError, ErrorKind::AlreadyExists);
        let (r, _, got) = link_run(&fs, &o, false, policy(64), &unguarded, no_seed);
        r.unwrap();
        assert_eq!(count(&log(&fs), "create_symlink("), 1);
        assert_eq!(only_copy_failure(&got, "b").step, CopyStep::Create);
    }

    #[test]
    fn a_weak_source_identity_on_a_replace_is_degraded_or_refused_under_strict_safety() {
        // Spec test 4b (the gate's `NotFound` row never degrades, so the name is taken). Mutant: the gate called with
        // only its equality row.
        let weak = FileIdentity::Weak(ObjectId { volume: 5, index: 9 });
        let fs = links(&[("n", "p")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/n", b"old");
        fs.set_identity("/src/n", weak);
        let (r, out, got) = link_run(&fs, &opts(), false, policy(1), &unguarded, no_seed);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_copied, out.files_degraded), (1, 1));
        assert_eq!(out.warnings.weak[&5], DegradedGroup { count: 1, example: PathBuf::from("n") });
        assert_eq!(link_at(&fs, "/dst/n").payload, "p");

        let fs = links(&[("n", "p")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/n", b"old");
        fs.set_identity("/src/n", weak);
        let (r, out, got) = link_run(&fs, &strict(), false, policy(1), &unguarded, no_seed);
        r.unwrap();
        let e = only_copy_failure(&got, "n");
        assert_eq!(e.code(), Code::SafetyRejected, "{e:?}");
        assert_eq!(fs.read_file("/dst/n").as_deref(), Some(&b"old"[..]));
        assert!(!fs.called("create_symlink("));
        assert_eq!((out.files_copied, out.files_degraded), (0, 0));
    }

    #[test]
    fn record_claim_upgrades_an_own_existing_claim_to_created() {
        // Spec 3.2 step 5 (the record bullet) and test 8c. Mutant: the upgrade removed.
        let fs = links(&[("n", "p")]);
        let seed = |s: &mut crate::fault_fs::FakeClaimStore, dst: ObjectId| {
            let record = ClaimRecord { target: key_of("n"), status: ClaimStatus::Existing };
            s.insert_if_absent(&ClaimKey::new(dst, OsStr::new("n")), &record).unwrap();
        };
        let (r, out, got) = link_run(&fs, &opts(), false, policy(1), &unguarded, seed);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(out.files_copied, 1);
        assert_eq!(
            fs.claim(strong_id(&fs, "/dst"), "n"),
            Some(ClaimRecord { target: key_of("n"), status: ClaimStatus::Created })
        );

        // A foreign claim is still refused: the link is published, the claim is not recorded and stays foreign.
        let fs = links(&[("n", "p")]);
        let seed = |s: &mut crate::fault_fs::FakeClaimStore, dst: ObjectId| {
            let record = ClaimRecord { target: key_of("other"), status: ClaimStatus::Existing };
            s.insert_if_absent(&ClaimKey::new(dst, OsStr::new("n")), &record).unwrap();
        };
        let (r, out, got) = link_run(&fs, &opts(), false, policy(1), &unguarded, seed);
        r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(matches!(got[0].cause, TreeFailureCause::ClaimNotRecorded(_)), "{got:?}");
        assert_eq!(out.files_copied, 1);
        assert_eq!(
            fs.claim(strong_id(&fs, "/dst"), "n"),
            Some(ClaimRecord { target: key_of("other"), status: ClaimStatus::Existing })
        );

        // `record_claim` itself: the upgrade only for own `Existing` -> `Created`; same status is a no-op.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let root = fs.destination_root(Path::new("/d")).unwrap();
        let store = root.create_claim_store(OsStr::new("s.db"), Durability::Normal).unwrap();
        let claims = std::cell::RefCell::new(store);
        let id = ObjectId { volume: 1, index: 1 };
        let rec = |t: &str, status| ClaimRecord { target: key_of(t), status };
        let k = |n: &str| ClaimKey::new(id, OsStr::new(n));
        for (n, present, writing, after) in [
            ("up", ClaimStatus::Existing, ClaimStatus::Created, ClaimStatus::Created),
            ("cc", ClaimStatus::Created, ClaimStatus::Created, ClaimStatus::Created),
            ("ee", ClaimStatus::Existing, ClaimStatus::Existing, ClaimStatus::Existing),
            ("ce", ClaimStatus::Created, ClaimStatus::Existing, ClaimStatus::Created),
        ] {
            claims.borrow_mut().insert_if_absent(&k(n), &rec(n, present)).unwrap();
            record_claim(&claims, &k(n), &rec(n, writing)).unwrap();
            assert_eq!(claims.borrow().get(&k(n)).unwrap(), Some(rec(n, after)), "{n}");
        }
        assert_eq!(count(&log(&fs), "claim_upgrade("), 1, "only the Existing -> Created case");
        claims.borrow_mut().insert_if_absent(&k("f"), &rec("g", ClaimStatus::Existing)).unwrap();
        assert!(record_claim(&claims, &k("f"), &rec("f", ClaimStatus::Created)).is_err());
        assert_eq!(claims.borrow().get(&k("f")).unwrap(), Some(rec("g", ClaimStatus::Existing)));
    }

    #[test]
    fn a_lost_lock_before_the_publish_keeps_the_temporary_and_stops() {
        // Spec test 9's guard case. Mutant: the guard check before the rename removed (publishes under a lost lock).
        let fs = links(&[("n", "p")]);
        let lost = || -> flux_fs::Result<()> {
            if fs.called("create_symlink(") {
                Err(FsError::new(Code::TargetLockBusy, std::io::Error::other("lock lost")))
            } else {
                Ok(())
            }
        };

        let (r, out, _) = link_run(&fs, &opts(), false, policy(1), &lost, no_seed);

        let e = r.unwrap_err();
        assert_eq!(e.code(), Code::TargetLockBusy, "{e:?}");
        let (left, why) = e.leftover.as_ref().expect("the temporary is reported");
        assert_eq!(left, Path::new("n.flux-partial.op1"));
        assert!(why.to_string().contains("no longer holds"), "{why}");
        assert!(fs.exists("/dst/n.flux-partial.op1"), "the temporary is kept");
        assert!(!fs.exists("/dst/n"), "nothing published under a lost lock");
        let c = log(&fs);
        let created = first(&c, "create_symlink(/dst/n.flux-partial.op1)");
        assert!(
            positions(&c, "remove_file(/dst/n.flux-partial.op1)").iter().all(|&i| i < created),
            "no removal after the create: {c:?}"
        );
        assert_eq!(count(&c, "rename_"), 0, "{c:?}");
        assert_eq!(out.files_copied, 0);
    }

    /// Cut 10a final review: the guard calls a single link `n` into an empty `/dst` makes, in order: 1 `gate_link`'s,
    /// 2 `create_link`'s, 3 `publish_staged`'s (no flush, so no pair after a probe).
    const LINK_GUARD_AT_GATE: u32 = 1;
    const LINK_GUARD_AT_CREATE: u32 = 2;

    #[test]
    fn t1_link_publish_guard_failure_after_the_note_keeps_it() {
        // The two link callers (New and Replace) share the helper; one pin for a Strict link: gate (1), create (2) pass, publish (3) fails.
        let fs = links(&[("n", "p")]);
        let calls = std::cell::Cell::new(0);
        let guard = failing_after(LINK_GUARD_AT_CREATE, &calls);
        let o = CopyOptions { durability: Durability::Strict, ..opts() };
        let (r, out, got) = link_run(&fs, &o, false, policy(1), &guard, no_seed);
        let error = r.unwrap_err();
        assert_eq!(error.code(), Code::TargetLockBusy);
        assert_eq!(calls.get(), 3);
        assert_eq!(count(&log(&fs), "claim_discard_prepared("), 0);
        assert_eq!(fs.prepared_count(), 1, "the link's note stays");
        assert!(got.is_empty(), "a stop, not a per-entry failure: {got:?}");
        assert_eq!(out.files_copied, 0);
        assert_eq!(leftovers(&got, Some(&error)), [temp_of("n")], "reported once: {got:?}");
    }

    #[test]
    fn t2f_replace_link_rename_failure_then_a_lost_lock_keeps_the_note() {
        // Cut 11c site 1, the Replace link caller. Mutant: that caller's `discard_note_on_failure` always discards.
        let fs = links(&[("n", "p")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/n", b"old");
        fs.fail("rename_replace", Code::IoError);
        let calls = std::cell::Cell::new(0);
        // gate (1), create (2), publish (3) pass; the rename fails; the discard guard (4) fails.
        let guard = failing_after(3, &calls);
        let o = CopyOptions { durability: Durability::Strict, ..opts() };
        let (r, _, got) = link_run(&fs, &o, false, policy(1), &guard, no_seed);
        let error = r.unwrap_err();
        assert_eq!(error.code(), Code::TargetLockBusy, "{error:?}");
        assert_eq!(calls.get(), 5, "the re-check ran once");
        let c = log(&fs);
        assert_eq!(count(&c, "claim_prepare("), 1, "precondition: the note was written: {c:?}");
        assert_eq!(count(&c, "claim_discard_prepared("), 0, "{c:?}");
        assert_eq!(fs.prepared_count(), 1, "the link's note stays");
        let e = only_copy_failure(&got, "n");
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Publish), "{e:?}");
    }

    #[test]
    fn t2f_held_replace_link_rename_failure_discards_the_note() {
        let fs = links(&[("n", "p")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/n", b"old");
        fs.fail("rename_replace", Code::IoError);
        let o = CopyOptions { durability: Durability::Strict, ..opts() };
        let (r, _, got) = link_run(&fs, &o, false, policy(1), &unguarded, no_seed);
        r.unwrap();
        let c = log(&fs);
        assert_eq!(count(&c, "claim_prepare("), 1, "precondition: the note was written: {c:?}");
        assert_eq!(count(&c, "claim_discard_prepared("), 1, "{c:?}");
        assert_eq!(fs.prepared_count(), 0);
        only_copy_failure(&got, "n");
    }

    #[test]
    fn a_lost_lock_before_the_create_is_a_stop_and_creates_nothing() {
        // Mutant: the guard in `create_link`'s pair removed (the link is created and only the publish guard stops it,
        // keeping the temporary).
        let fs = links(&[("n", "p")]);
        let calls = std::cell::Cell::new(0);
        let guard = failing_after(LINK_GUARD_AT_CREATE - 1, &calls);

        let (r, out, _) = link_run(&fs, &opts(), false, policy(1), &guard, no_seed);

        let e = r.unwrap_err();
        assert_eq!(e.code(), Code::TargetLockBusy, "{e:?}");
        assert_eq!(calls.get(), LINK_GUARD_AT_CREATE, "stopped at the create's guard");
        let c = log(&fs);
        assert!(
            fs.called("remove_file(/dst/n.flux-partial.op1)"),
            "precondition: past the gate: {c:?}"
        );
        assert!(!fs.called("create_symlink("), "{c:?}");
        assert!(temps_in(&fs, "/dst").is_empty(), "{c:?}");
        assert!(!fs.exists("/dst/n"));
        assert_eq!(out.files_copied, 0);
    }

    #[test]
    fn a_lost_lock_at_the_gate_removes_and_creates_nothing() {
        // Mutant: the guard in `gate_link`'s pair removed (the leftover removal runs under a lost lock).
        let fs = links(&[("n", "p")]);
        let calls = std::cell::Cell::new(0);
        let guard = failing_after(LINK_GUARD_AT_GATE - 1, &calls);

        let (r, out, _) = link_run(&fs, &opts(), false, policy(1), &guard, no_seed);

        let e = r.unwrap_err();
        assert_eq!(e.code(), Code::TargetLockBusy, "{e:?}");
        assert_eq!(calls.get(), LINK_GUARD_AT_GATE, "stopped at the gate's guard");
        let c = log(&fs);
        assert!(fs.called("read_link(/src/n)"), "precondition: the link was read: {c:?}");
        assert!(!fs.called("remove_file("), "{c:?}");
        assert!(!fs.called("create_symlink("), "{c:?}");
        assert!(!fs.exists("/dst/n"));
        assert_eq!(out.files_copied, 0);
    }

    #[test]
    fn a_strict_replace_link_writes_a_replacement_note_and_discards_it_on_a_failed_rename() {
        // Mutant: `replacement: false` in the Replace template.
        let notes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let fs = links(&[("n", "p")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/n", b"old");
        let seen = std::sync::Arc::clone(&notes);
        fs.on_nth("rename_replace", 1, move |fs| {
            let root = fs.destination_root(Path::new("/dst")).unwrap();
            let store = root.open_claim_store(OsStr::new("state.db"), Durability::Strict).unwrap();
            *seen.lock().unwrap() = store.prepared().unwrap();
        });
        let o = CopyOptions { durability: Durability::Strict, ..opts() };

        let (r, out, got) = link_run(&fs, &o, false, policy(1), &unguarded, no_seed);

        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        let pending = notes.lock().unwrap().clone();
        assert_eq!(pending.len(), 1, "the note is pending at the rename: {pending:?}");
        let (key, note) = &pending[0];
        assert_eq!(key.name, b"n");
        assert!(note.replacement, "a Replace note says replacement: {note:?}");
        assert_eq!(note.target, key_of("n"));
        assert_eq!(fs.prepared_count(), 0, "committed after");
        assert_eq!(link_at(&fs, "/dst/n").payload, "p");
        assert_eq!((out.files_copied, out.files_overwritten), (1, 1));

        let fs = links(&[("n", "p")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/n", b"old");
        fs.fail("rename_replace", Code::IoError);
        let (r, out, got) = link_run(&fs, &o, false, policy(1), &unguarded, no_seed);
        r.unwrap();
        let e = only_copy_failure(&got, "n");
        assert_eq!(e.step, CopyStep::Publish, "{e:?}");
        assert!(fs.called("claim_prepare("), "precondition: the note was written");
        assert_eq!(fs.prepared_count(), 0, "the note is discarded");
        assert!(temps_in(&fs, "/dst").is_empty(), "the temporary is discarded");
        assert_eq!(fs.read_file("/dst/n").as_deref(), Some(&b"old"[..]));
        assert_eq!((out.files_copied, out.files_overwritten), (0, 0));
    }

    #[test]
    fn a_link_to_an_existing_target_never_opens_the_target_or_the_link_path() {
        // I2, with targets that EXIST (outside the tree, absolute and relative) and one that does not.
        let fs =
            links(&[("abs", "/outside/t"), ("gone", "/outside/none"), ("rel", "../outside/t")]);
        fs.create_dir(Path::new("/outside")).unwrap();
        fs.write_file("/outside/t", b"target");
        let setup = fs.calls().len();

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(out.files_copied, 3);
        let c = calls_since(&fs, setup);
        assert!(!c.iter().any(|x| x.contains("outside")), "a target was touched: {c:?}");
        for n in ["abs", "gone", "rel"] {
            for p in [format!("/src/{n}"), format!("/dst/{n}")] {
                assert!(!c.contains(&format!("open_read({p})")), "{p} was opened: {c:?}");
            }
        }
        assert_eq!(fs.read_file("/outside/t").as_deref(), Some(&b"target"[..]));
    }

    #[test]
    fn the_names_owner_check_refuses_a_folded_name_before_any_claim() {
        // §241.5's `names.owner` refusal, not the claim backstop: no claim insertion is attempted for the link.
        let fs = links(&[("a", "payload")]);
        fs.write_file("/src/A", b"upper");
        fs.set_case_insensitive(true);

        let (r, _, got) = batched(&fs, policy(1), Durability::Normal);

        r.unwrap();
        let e = only_copy_failure(&got, "a");
        assert_eq!((e.code(), e.step), (Code::DestinationNamespaceCollision, CopyStep::Claim));
        let c = log(&fs);
        assert_eq!(count(&c, "claim_insert(A)"), 1, "the file's own claim only: {c:?}");
        assert_eq!(count(&c, "claim_insert(a)"), 0, "{c:?}");
        assert_eq!(count(&c, "read_link(/dst/"), 0, "{c:?}");
    }

    #[test]
    fn a_reserved_link_under_links_skip_is_still_a_gate_failure() {
        // Spec 3.2: step 1 (reserved) precedes step 2 (`--links=skip`).
        let fs = links(&[]);
        fs.create_dir(Path::new("/src/.flux")).unwrap();
        fs.add_symlink_to("/src/.flux/operations", "/elsewhere", LinkKind::Unspecified);
        let o = CopyOptions { links: Links::Skip, ..opts() };

        let (r, got) = run(&fs, "/src", "/dst", &o);

        let out = r.unwrap();
        let e = only_copy_failure(&got, ".flux/operations");
        assert_eq!((e.step, e.code()), (CopyStep::Gate, Code::ControlPlaneNamespaceConflict));
        assert_eq!(out.symlinks_skipped, 0);
        assert!(!fs.called("read_link("));
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
        // Cut 10a: under `--links=skip` the link is one record and copies nothing, as the removed failure was.
        let mut o = opts();
        o.links = Links::Skip;

        let (r, got) = run(&fs, "/src", "/dst", &o);

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
        let source =
            prepare_source(fs, Path::new("/src"), Path::new("/dst"), MountPolicy::Cross).unwrap();
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
            batch: BatchPolicy { age: NO_AGE, ..BatchPolicy::DEFAULT },
            ckpt: None,
            dry_run: false,
            dry_prior: None,
            dry_partials: None,
            key_prefix: None,
            hardlinks: None,
            anchor_root: None,
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
    fn a_wal_write_failed_copy_stops_the_walk() {
        let mut out = TreeOutcome::default();
        let mut reported = 0;
        let failed = CopyError::at(
            CopyStep::Checkpoint,
            FsError::new(Code::WalWriteFailed, std::io::Error::other("wal")),
        );
        let r =
            finish_copy(None, PathBuf::from("a"), Err(failed), &mut out, &mut |_| reported += 1);
        let e = r.unwrap_err();
        assert_eq!((e.code(), e.step), (Code::WalWriteFailed, CopyStep::Checkpoint));
        assert_eq!((reported, out.failures.total()), (0, 0), "a stop, never a per-file failure");
    }

    #[test]
    fn a_failed_heartbeat_aborts_the_whole_tree() {
        let fs = tree();
        let source =
            prepare_source(&fs, Path::new("/src"), Path::new("/dst"), MountPolicy::Cross).unwrap();
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
            batch: BatchPolicy { age: NO_AGE, ..BatchPolicy::DEFAULT },
            ckpt: None,
            dry_run: false,
            dry_prior: None,
            dry_partials: None,
            key_prefix: None,
            hardlinks: None,
            anchor_root: None,
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
        let source =
            prepare_source(&fs, Path::new("/src"), Path::new("/dst"), MountPolicy::Cross).unwrap();
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
            batch: BatchPolicy { age: NO_AGE, ..BatchPolicy::DEFAULT },
            ckpt: None,
            dry_run: false,
            dry_prior: None,
            dry_partials: None,
            key_prefix: None,
            hardlinks: None,
            anchor_root: None,
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
        let source =
            prepare_source(&fs, Path::new("/src"), Path::new("/dst"), MountPolicy::Cross).unwrap();
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
            batch: BatchPolicy { age: NO_AGE, ..BatchPolicy::DEFAULT },
            ckpt: None,
            dry_run: false,
            dry_prior: None,
            dry_partials: None,
            key_prefix: None,
            hardlinks: None,
            anchor_root: None,
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

    // Cut 9d: the per-directory batch. Every test here passes its own policy (small thresholds, or `files == 1` for the
    // unbatched path) rather than the default (64 files).

    /// An age no test run reaches: a unit test that counts calls must not let the real clock (the default's `now`)
    /// fire an age flush on a slow machine. Only `the_age_trigger_uses_the_injected_clock` tests the age.
    const NO_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

    fn policy(files: usize) -> BatchPolicy {
        BatchPolicy { files, age: NO_AGE, ..BatchPolicy::DEFAULT }
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
        batched_keyed(fs, policy, durability, guard, beat, None)
    }

    /// `batched_with` walking under `key_prefix` (several sources).
    fn batched_keyed(
        fs: &FaultFs,
        policy: BatchPolicy,
        durability: Durability,
        guard: &Guard<'_>,
        beat: &Heartbeat<'_>,
        key_prefix: Option<&OsStr>,
    ) -> (std::result::Result<(), CopyError>, TreeOutcome, Vec<TreeFailure>) {
        batched_verifying(
            fs,
            policy,
            durability,
            guard,
            beat,
            key_prefix,
            flux_fs::VerifyLevel::None,
        )
    }

    /// `batched_keyed` at a verification level.
    fn batched_verifying(
        fs: &FaultFs,
        policy: BatchPolicy,
        durability: Durability,
        guard: &Guard<'_>,
        beat: &Heartbeat<'_>,
        key_prefix: Option<&OsStr>,
        verify: flux_fs::VerifyLevel,
    ) -> (std::result::Result<(), CopyError>, TreeOutcome, Vec<TreeFailure>) {
        let source =
            prepare_source(fs, Path::new("/src"), Path::new("/dst"), MountPolicy::Cross).unwrap();
        if !fs.exists(Path::new("/dst")) {
            fs.create_dir(Path::new("/dst")).unwrap();
        }
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let store = root.create_claim_store(OsStr::new("state.db"), durability).unwrap();
        let claims = std::cell::RefCell::new(store);
        let mut o = opts();
        o.durability = durability;
        o.verify = verify;
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
            ckpt: None,
            dry_run: false,
            dry_prior: None,
            dry_partials: None,
            key_prefix,
            hardlinks: None,
            anchor_root: None,
        };
        let mut out = TreeOutcome::default();
        let mut got = Vec::new();
        let (_root, r) =
            copy_tree_at(&cx, source.events, root, false, &mut out, &mut |f| got.push(f));
        (r, out, got)
    }

    // Several sources, task 4: a walk keyed under a root prefix. The tree is `x/f`; the record paths are relative to
    // DEST, so `a/x/f` under the prefix `a` and `x/f` without one.

    /// Copy `x/f` under `prefix` at Strict durability: the claim it stored, and the note it wrote before the rename.
    fn keyed_records(prefix: Option<&str>) -> (FluxPathKey, PreparedRecord) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let fs = sources(&[("x/f", b"data")]);
        let slot = std::sync::Arc::clone(&seen);
        fs.on_nth("rename_no_replace", 1, move |fs| {
            let root = fs.destination_root(Path::new("/dst")).unwrap();
            let store = root.open_claim_store(OsStr::new("state.db"), Durability::Strict).unwrap();
            let pending = store.prepared().unwrap();
            assert_eq!(pending.len(), 1, "one note is pending at the rename: {pending:?}");
            *slot.lock().unwrap() = Some(pending[0].clone());
        });

        let prefix = prefix.map(OsString::from);
        let (r, _, got) = batched_keyed(
            &fs,
            policy(1),
            Durability::Strict,
            &unguarded,
            &no_heartbeat,
            prefix.as_deref(),
        );

        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        let (key, note) = seen.lock().unwrap().take().expect("the hook ran at the publish");
        // The claim is committed with the note's discard, after the rename: read it once the run is over.
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let store = root.open_claim_store(OsStr::new("state.db"), Durability::Strict).unwrap();
        let claim = store.get(&key).unwrap().expect("the claim is stored");
        (claim.target, note)
    }

    /// The leftover of `x/f` whose temporary could not be removed, under `prefix`.
    fn keyed_leftover(prefix: Option<&str>) -> PathBuf {
        let fs = sources(&[("x/f", b"data")]);
        fs.fail("rename_no_replace", Code::PermissionDenied);
        fs.fail_always("remove_file", Code::PermissionDenied);
        let prefix = prefix.map(OsString::from);

        let (r, _, got) = batched_keyed(
            &fs,
            policy(1),
            Durability::Normal,
            &unguarded,
            &no_heartbeat,
            prefix.as_deref(),
        );

        r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new("x/f"), "TreeFailure.path stays root-relative");
        match &got[0].cause {
            TreeFailureCause::Copy(e) => {
                e.leftover.as_ref().expect("the leftover is reported").0.clone()
            }
            other => panic!("expected Copy, got {other:?}"),
        }
    }

    #[test]
    fn claim_and_prepared_targets_carry_the_root_prefix() {
        let (claim_target, note) = keyed_records(Some("a"));

        assert_eq!(claim_target.0, key_of("a/x/f").0);
        assert_eq!(note.target.0, key_of("a/x/f").0);
    }

    #[test]
    fn a_note_records_dir_path_relative_to_dest() {
        let (_, note) = keyed_records(Some("a"));

        assert_eq!(note.dir_path, native_hex(Path::new("a/x")));
    }

    #[test]
    fn a_leftover_is_relative_to_dest() {
        assert_eq!(keyed_leftover(Some("a")), Path::new("a/x/f.flux-partial.op1"));
    }

    #[test]
    fn without_a_prefix_nothing_changes() {
        let (claim_target, note) = keyed_records(None);

        assert_eq!(claim_target.0, key_of("x/f").0);
        assert_eq!(note.target.0, key_of("x/f").0);
        assert_eq!(note.dir_path, native_hex(Path::new("x")));
        assert_eq!(keyed_leftover(None), Path::new("x/f.flux-partial.op1"));
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
    fn t1_unbatched_publish_guard_failure_after_the_note_keeps_it_and_stops() {
        // Spec 4.2 bullet 1: a `TargetLockBusy` copy failure is returned as is: no discard and NO re-check.
        let fs = n_files(2);
        let calls = std::cell::Cell::new(0);
        // f0: sweep (1), create (2) pass; its publish guard (3) fails, after `write_note`.
        let guard = failing_after(2, &calls);
        let (r, out, got) = batched_with(&fs, policy(1), Durability::Strict, &guard, &no_heartbeat);
        let error = r.unwrap_err();
        assert_eq!(
            (error.code(), error.step),
            (Code::TargetLockBusy, CopyStep::Publish),
            "{error:?}"
        );
        assert_eq!(calls.get(), 3, "no re-check after a TargetLockBusy failure");
        assert!(got.is_empty(), "a stop, not a per-file failure: {got:?}");
        let c = log(&fs);
        assert_eq!(count(&c, "claim_prepare("), 1, "{c:?}");
        assert_eq!(count(&c, "claim_discard_prepared("), 0, "{c:?}");
        assert_eq!(fs.prepared_count(), 1, "the note stays for recovery");
        assert!(fs.exists("/dst/f0.flux-partial.op1") && !fs.exists("/dst/f0"));
        assert!(!fs.called("create_new(/dst/f1"), "no later file is attempted: {c:?}");
        assert_eq!(leftovers(&got, Some(&error)), [temp_of("f0")], "reported once: {got:?}");
        assert_eq!(out.files_copied, 0);
    }

    #[test]
    fn t2_unbatched_rename_failure_then_a_lost_lock_keeps_the_note_reports_the_failure_and_stops() {
        // Spec 4.2 bullets 2-4: the re-check. Mutant: drop the re-check (today's unconditional discard).
        let fs = n_files(2);
        let calls = std::cell::Cell::new(0);
        fs.fail_nth("rename_no_replace", 1, Code::IoError, ErrorKind::Other);
        // f0: sweep (1), create (2), publish (3) pass; the rename fails; the copy path's discard guard (4) fails (the
        // temporary is kept); the re-check (5) fails.
        let guard = failing_after(3, &calls);
        let (r, out, got) = batched_with(&fs, policy(1), Durability::Strict, &guard, &no_heartbeat);
        let error = r.unwrap_err();
        assert_eq!(
            (error.code(), error.step),
            (Code::TargetLockBusy, CopyStep::Publish),
            "{error:?}"
        );
        assert_eq!(
            error.leftover.as_ref().map(|(p, _)| p.clone()),
            Some(temp_of("f0")),
            "relative to DEST"
        );
        assert_eq!(calls.get(), 5, "the re-check ran once");
        let c = log(&fs);
        assert_eq!(count(&c, "claim_discard_prepared("), 0, "{c:?}");
        assert_eq!(fs.prepared_count(), 1, "the note stays for recovery");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("f0"));
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(
            (e.code(), e.step),
            (Code::IoError, CopyStep::Publish),
            "the original failure: {e:?}"
        );
        assert!(e.leftover.is_none(), "the leftover travels in the stop only: {e:?}");
        assert_eq!(leftovers(&got, Some(&error)), [temp_of("f0")], "once: {got:?}");
        assert!(fs.exists("/dst/f0.flux-partial.op1"));
        assert!(!fs.called("create_new(/dst/f1"), "no later file is attempted: {c:?}");
        assert_eq!(out.files_copied, 0);
    }

    #[test]
    fn t2e_replace_copy_rename_failure_then_a_lost_lock_keeps_the_note() {
        // Cut 11c site 1, the Replace copy caller. Mutant: that caller's `discard_note_on_failure` always discards.
        let fs = n_files(1);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/f0", b"old");
        let calls = std::cell::Cell::new(0);
        fs.fail_nth("rename_replace", 1, Code::IoError, ErrorKind::Other);
        // f0: sweep (1), create (2), publish (3) pass; the rename fails; the discard guard (4) fails.
        let guard = failing_after(3, &calls);
        let (r, _, got) = batched_with(&fs, policy(1), Durability::Strict, &guard, &no_heartbeat);
        let error = r.unwrap_err();
        assert_eq!(error.code(), Code::TargetLockBusy, "{error:?}");
        assert_eq!(calls.get(), 5, "the re-check ran once");
        let c = log(&fs);
        assert_eq!(count(&c, "claim_prepare("), 1, "precondition: the note was written: {c:?}");
        assert_eq!(count(&c, "claim_discard_prepared("), 0, "{c:?}");
        assert_eq!(fs.prepared_count(), 1, "the note stays for recovery");
        assert_eq!(got.len(), 1, "{got:?}");
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Publish), "{e:?}");
        assert_eq!(leftovers(&got, Some(&error)).len(), 1, "reported once: {got:?}");
        assert_eq!(fs.read_file("/dst/f0").as_deref(), Some(&b"old"[..]));
    }

    #[test]
    fn t2e_held_replace_copy_rename_failure_discards_the_note() {
        let fs = n_files(1);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/f0", b"old");
        fs.fail_nth("rename_replace", 1, Code::IoError, ErrorKind::Other);
        let (r, _, got) = batched(&fs, policy(1), Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        assert_eq!(count(&c, "claim_prepare("), 1, "precondition: the note was written: {c:?}");
        assert_eq!(count(&c, "claim_discard_prepared("), 1, "{c:?}");
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(got.len(), 1, "{got:?}");
    }

    #[test]
    fn t2b_the_collision_remap_survives_the_lost_lock_report() {
        // Review focus 2. Between f0's create and its rename someone writes /dst/f0; the no-replace rename refuses it.
        let fs = n_files(2);
        let calls = std::cell::Cell::new(0);
        fs.on_nth("rename_no_replace", 1, |fs| fs.write_file("/dst/f0", b"theirs"));
        let guard = failing_after(3, &calls);
        let (r, _, got) = batched_with(&fs, policy(1), Durability::Strict, &guard, &no_heartbeat);
        assert_eq!(r.unwrap_err().code(), Code::TargetLockBusy);
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(e.code(), Code::DestinationNamespaceCollision, "{e:?}");
        assert_eq!(fs.read_file("/dst/f0").as_deref(), Some(&b"theirs"[..]));
        assert_eq!(count(&log(&fs), "claim_discard_prepared("), 0);
        assert_eq!(fs.prepared_count(), 1, "the note stays");
    }

    #[test]
    fn t2c_a_heartbeat_failure_after_the_note_under_a_lost_lock_is_reported_and_the_stop_is_the_lost_lock()
     {
        // Review focus 1 (spec 4.2, "whatever the original's code"). f0 beats sweep (1), create (2), chunk (3); its
        // publish beat (4) fails before the publish guard; the discard guard (3) fails; the re-check (4) fails.
        let fs = n_files(2);
        let calls = std::cell::Cell::new(0);
        let beats = std::cell::Cell::new(0);
        let beat = || {
            beats.set(beats.get() + 1);
            if beats.get() > 3 {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        let guard = failing_after(2, &calls);
        let (r, _, got) = batched_with(&fs, policy(1), Durability::Strict, &guard, &beat);
        let error = r.unwrap_err();
        assert_eq!(
            (error.code(), error.step),
            (Code::TargetLockBusy, CopyStep::Heartbeat),
            "the original's step: {error:?}"
        );
        assert_eq!((beats.get(), calls.get()), (4, 4));
        assert_eq!(count(&log(&fs), "claim_discard_prepared("), 0);
        assert_eq!(fs.prepared_count(), 1);
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Heartbeat), "{e:?}");
        assert_eq!(leftovers(&got, Some(&error)), [temp_of("f0")]);
    }

    #[test]
    fn t2d_a_heartbeat_failure_after_the_note_with_the_lock_held_discards_it() {
        // Closes "Cut 9c debt" item 1. Held: the discard is legal and `finish_copy` stops on the heartbeat as today.
        let fs = n_files(2);
        let beats = std::cell::Cell::new(0);
        let beat = || {
            beats.set(beats.get() + 1);
            if beats.get() > 3 {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        let (r, _, got) = batched_with(&fs, policy(1), Durability::Strict, &unguarded, &beat);
        assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
        assert!(got.is_empty(), "a stop, not a per-file failure: {got:?}");
        assert_eq!(count(&log(&fs), "claim_discard_prepared("), 1);
        assert_eq!(fs.prepared_count(), 0);
        assert!(!fs.exists("/dst/f0.flux-partial.op1"));
    }

    #[test]
    fn t5_unbatched_rename_failure_with_the_lock_held_still_discards_the_note() {
        // Spec T5: the re-check does not suppress a legal write. Mutant: skip the discard whenever the guard is consulted.
        let fs = n_files(2);
        fs.fail_nth("rename_no_replace", 1, Code::IoError, ErrorKind::Other);
        let (r, out, got) = batched(&fs, policy(1), Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        assert_eq!(count(&c, "claim_discard_prepared("), 1, "{c:?}");
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(got.len(), 1, "{got:?}");
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Publish));
        assert!(e.leftover.is_none() && !fs.exists("/dst/f0.flux-partial.op1"));
        assert!(published(&fs, "f1") && claimed(&fs, "f1"));
        assert_eq!(out.files_copied, 1);
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
        let p = BatchPolicy { files: 64, bytes: 10, age: NO_AGE, ..BatchPolicy::DEFAULT };
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
        let p = BatchPolicy { files: 64, bytes: 10, age: NO_AGE, ..BatchPolicy::DEFAULT };
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
        assert_eq!(BatchPolicy::DEFAULT.files, BATCH_FILES);
        assert_eq!(BatchPolicy::DEFAULT.bytes, BATCH_BYTES);
        assert_eq!(BatchPolicy::DEFAULT.age, BATCH_AGE);
        // Cut 9e test audit: the cap's default is pinned too (mutant: `open_writers: 1` in `DEFAULT`).
        assert_eq!(BatchPolicy::DEFAULT.open_writers, OPEN_WRITERS_MAX);
        assert_eq!(OPEN_WRITERS_MAX, 256);
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

    /// Final-review hardening: a STOP from the per-entry `prepare` fallback (an error carrying `TargetLockBusy`, which
    /// `finish_copy` returns as a stop) drops no entry: the already-noted keep their notes and temporaries, the not yet
    /// noted keep their temporaries, and each is reported with its temporary as the leftover.
    #[test]
    fn a_stop_in_the_per_entry_prepare_fallback_reports_every_other_entry() {
        let fs = n_files(3);
        fs.fail_always("claim_prepare_many", Code::IoError);
        fs.fail_nth("claim_prepare", 2, Code::TargetLockBusy, ErrorKind::Other);
        let (r, out, got) = batched(&fs, policy(4), Durability::Strict);
        let stop = r.unwrap_err();
        assert_eq!(stop.code(), Code::TargetLockBusy, "{stop:?}");
        assert_eq!(stop.step, CopyStep::Claim, "{stop:?}");
        let paths: Vec<&Path> = got.iter().map(|f| f.path.as_path()).collect();
        assert_eq!(paths, [Path::new("f0"), Path::new("f2")], "{got:?}");
        for (f, name) in got.iter().zip(["f0", "f2"]) {
            let TreeFailureCause::Copy(e) = &f.cause else { panic!("{got:?}") };
            assert_eq!(e.code(), Code::TargetLockBusy, "{e:?}");
            let (leftover, _) = e.leftover.as_ref().expect("the temporary is reported");
            assert_eq!(leftover, &PathBuf::from(format!("{name}.flux-partial.op1")));
            assert!(fs.exists(format!("/dst/{name}.flux-partial.op1")), "{name}: kept");
            assert!(!fs.exists(format!("/dst/{name}")), "{name}: not published");
        }
        assert!(
            !fs.exists("/dst/f1.flux-partial.op1"),
            "the failed entry's temporary is discarded"
        );
        assert_eq!(fs.prepared_count(), 1, "f0's note stays for recovery");
        assert_eq!(out.files_copied, 0);
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
    fn t4_a_lost_lock_before_the_settling_transaction_writes_nothing_when_an_entry_failed() {
        // Spec 4.3 site 3. Mutant: drop the re-check (apply_recovery commits f0 and f2 and discards f1 after the loss).
        let fs = n_files(3);
        let calls = std::cell::Cell::new(0);
        fs.fail_nth("rename_no_replace", 2, Code::IoError, ErrorKind::Other);
        // Guards: staging (1-6), the flush's opening guard (7), f0's publish (8), f1's publish (9), f1's rename fails,
        // f1's discard (10, held: removed), f2's publish (11); the settling re-check (12) fails.
        let guard = failing_after(11, &calls);
        let (r, out, got) = batched_with(&fs, policy(4), Durability::Strict, &guard, &no_heartbeat);
        let error = r.unwrap_err();
        assert_eq!(
            (error.code(), error.step),
            (Code::TargetLockBusy, CopyStep::Publish),
            "{error:?}"
        );
        assert!(error.leftover.is_none(), "{error:?}");
        assert_eq!(calls.get(), 12);
        let c = log(&fs);
        for none in ["claim_apply_recovery", "claim_discard_prepared(", "claim_commit_prepared("] {
            assert_eq!(count(&c, none), 0, "{none}: {c:?}");
        }
        assert_eq!(fs.prepared_count(), 3, "every note stays");
        for f in ["f0", "f2"] {
            assert!(published(&fs, f) && !claimed(&fs, f), "{f}: renamed, finalized at --resume");
        }
        assert!(!fs.exists("/dst/f1.flux-partial.op1") && !fs.exists("/dst/f1"));
        assert_eq!(got.len(), 1, "{got:?}");
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(got[0].path, Path::new("f1"));
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Publish));
        assert!(leftovers(&got, Some(&error)).is_empty(), "{got:?}");
        assert_eq!(out.files_copied, 2);
    }

    #[test]
    fn t4_no_failed_entry_means_no_settling_re_check() {
        // N5. Mutant: re-check on every batch. A green 3-file batch: staging (1-6), opening (7), three publishes (8-10).
        let fs = n_files(3);
        let calls = std::cell::Cell::new(0);
        let guard = failing_after(u32::MAX, &calls);
        let (r, out, _) = batched_with(&fs, policy(4), Durability::Strict, &guard, &no_heartbeat);
        r.unwrap();
        assert_eq!((calls.get(), out.files_copied), (10, 3));
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
    fn t3_a_lost_lock_inside_the_stop_held_loop_writes_nothing_and_keeps_every_later_frame() {
        // Spec 4.3 site 2. Mutant: drop the re-check (apply_recovery runs; drain_batches removes the root's batch).
        // Two frames: `a` pending in the root; m's DirEnd flush holds x, y, z.
        // Each file holds its own relative name, so `published` (which compares bytes to the name) applies.
        let fs = sources(&[("a", b"a"), ("m/x", b"m/x"), ("m/y", b"m/y"), ("m/z", b"m/z")]);
        let calls = std::cell::Cell::new(0);
        let beats = std::cell::Cell::new(0);
        // Beats: a staged (1-3), m created (4), x y z staged (5-13), m's flush opening beat (14), x's publish (15):
        // y's publish beat (16) fails with the lock held.
        let beat = || {
            beats.set(beats.get() + 1);
            if beats.get() > 15 {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        // Guards: a staged (1-2), m created (3), x y z staged (4-9), m's flush opening guard (10), x's publish (11),
        // y's discard (12, held: removed); z's discard (13) fails (kept); the re-check (14) fails.
        let guard = failing_after(12, &calls);
        let (r, out, got) = batched_with(&fs, policy(64), Durability::Strict, &guard, &beat);
        let error = r.unwrap_err();
        assert_eq!(
            (error.code(), error.step),
            (Code::TargetLockBusy, CopyStep::Publish),
            "{error:?}"
        );
        assert!(error.leftover.is_none(), "{error:?}");
        assert_eq!((beats.get(), calls.get()), (16, 14), "the events the indices name");
        let c = log(&fs);
        assert_eq!(count(&c, "claim_apply_recovery"), 0, "{c:?}");
        assert_eq!(count(&c, "claim_discard_prepared("), 0, "{c:?}");
        assert_eq!(fs.prepared_count(), 3, "x, y, z keep their notes");
        assert!(published(&fs, "m/x"), "renamed");
        assert!(
            fs.claim(strong_id(&fs, "/dst/m"), "x").is_none(),
            "not committed; finalized at --resume"
        );
        assert!(!fs.exists("/dst/m/y.flux-partial.op1"), "removed while the lock was held");
        assert!(
            fs.exists("/dst/m/z.flux-partial.op1") && fs.exists("/dst/a.flux-partial.op1"),
            "kept"
        );
        // y: the original stop, reported (no leftover); z: kept in the loop; a: the root's batch, KeepAll in the drain.
        let y = got.iter().find(|f| f.path.as_path() == Path::new("m/y")).expect("y reported");
        let TreeFailureCause::Copy(e) = &y.cause else { panic!("{got:?}") };
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Heartbeat), "{e:?}");
        assert!(e.leftover.is_none());
        assert_eq!(leftovers(&got, Some(&error)), [temp_of("a"), temp_of("m/z")], "{got:?}");
        assert_eq!(out.files_copied, 1);
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
    fn a_metadata_failure_with_pending_entries_reports_only_that_file() {
        let fs = sources(&[("a", b"a"), ("b", b"b"), ("c", b"c")]);
        // The green run's call log, counting `metadata` calls: /src, /src, /dst/a, /src/a, /dst/a, /src/a, /dst/b,
        // /dst/b's temporary (decision 6's probe), then /src/b: the 9th is the 5(b) stat of b, paid because a is pending
        // (a's own stat is skipped: its batch is empty).
        fs.fail_nth("metadata", 9, Code::PermissionDenied, ErrorKind::PermissionDenied);
        let (r, out, got) = batched(&fs, policy(64), Durability::Strict);
        r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, PathBuf::from("b"));
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(e.step, CopyStep::Source, "{e:?}");
        let c = log(&fs);
        let stats = positions(&c, "metadata(");
        assert_eq!(c[stats[8]], "metadata(/src/b)", "the fault hit b's 5(b) stat: {c:?}");
        assert_eq!(count(&c, "create_new(/dst/b"), 0, "nothing was created for b: {c:?}");
        for f in ["a", "c"] {
            assert!(published(&fs, f) && claimed(&fs, f), "{f}");
        }
        assert!(!fs.exists("/dst/b"));
        assert!(!fs.exists("/dst/b.flux-partial.op1"));
        assert_eq!(prepares(&c), ["claim_prepare_many(2)"], "{c:?}");
        assert_eq!(out.files_copied, 2);
        assert_eq!(fs.prepared_count(), 0);
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

    /// Capstone round 5: root holds pending `a`, `b`; `m` holds pending `x`; the walk then ends with an ORDINARY abort
    /// (the guard fails with `IoError` before `m/z` is created), so the drain runs in `Publish` with two live frames.
    /// The drain's first flush (`m`) is where the stop appears.
    fn two_pending_frames_then_an_ordinary_abort() -> FaultFs {
        sources(&[("a", b"a"), ("b", b"b"), ("m/x", b"x"), ("m/z/y", b"y")])
    }

    #[test]
    fn a_lost_lock_during_the_drain_keeps_the_remaining_frames_without_a_barrier() {
        let fs = two_pending_frames_then_an_ordinary_abort();
        let calls = std::cell::Cell::new(0);
        // a, b staged (guards 1-4), m's creation (5), x staged (6-7), z's creation (8) fails ordinarily: the walk's
        // error is no stop. The drain's first flush (m) then meets a lost lock at its guard (9), and every one after.
        let guard = || {
            calls.set(calls.get() + 1);
            match calls.get() {
                1..=7 => Ok(()),
                8 => Err(FsError::new(Code::IoError, std::io::Error::other("walk"))),
                _ => Err(FsError::new(Code::TargetLockBusy, std::io::Error::other("lost"))),
            }
        };
        let (r, out, got) =
            batched_with(&fs, policy(64), Durability::Strict, &guard, &no_heartbeat);
        let error = r.unwrap_err();
        assert_eq!(error.code(), Code::IoError, "the walk's own error wins: {error:?}");
        let c = log(&fs);
        assert_eq!(count(&c, "sync_all("), 1, "only m's barrier ran, before the stop: {c:?}");
        assert_eq!(count(&c, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(count(&c, "rename_"), 0, "{c:?}");
        assert_eq!(calls.get(), 9, "the drain's first flush met the lost lock at its guard");
        for t in ["/dst/a.flux-partial.op1", "/dst/b.flux-partial.op1"] {
            assert!(fs.exists(t), "{t}: {c:?}");
        }
        for f in ["a", "b"] {
            let kept = got.iter().any(|g| {
                matches!(&g.cause, TreeFailureCause::Copy(e)
                    if e.code() == Code::TargetLockBusy
                        && e.leftover.as_ref().map(|(p, _)| p) == Some(&temp_of(f)))
            });
            assert!(kept, "{f} is reported as lost-lock with its temporary: {got:?}");
        }
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(out.files_copied, 0);
    }

    #[test]
    fn a_heartbeat_failure_during_the_drain_removes_the_remaining_frames_without_a_barrier() {
        let fs = two_pending_frames_then_an_ordinary_abort();
        let guard_calls = std::cell::Cell::new(0);
        // The walk's ordinary abort: the guard before z's creation (8th call) fails with a code that is no stop.
        let guard = || {
            guard_calls.set(guard_calls.get() + 1);
            if guard_calls.get() == 8 {
                Err(FsError::new(Code::IoError, std::io::Error::other("walk")))
            } else {
                Ok(())
            }
        };
        let beats = std::cell::Cell::new(0);
        // a, b staged (beats 1-6), m's creation (7), x staged (8-10), z's creation (11); the drain's first beat (12)
        // fails, exactly once: a Pulse reports a failed beat once and Ok afterwards, with the guard still owning the lock.
        let beat = || {
            beats.set(beats.get() + 1);
            if beats.get() == 12 {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        let (r, out, got) = batched_with(&fs, policy(64), Durability::Strict, &guard, &beat);
        assert_eq!(r.unwrap_err().code(), Code::IoError, "the walk's own error wins");
        let c = log(&fs);
        assert_eq!(count(&c, "sync_all("), 1, "only m's barrier, before the stop: {c:?}");
        assert_eq!(count(&c, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(count(&c, "rename_"), 0, "{c:?}");
        assert_eq!(beats.get(), 12, "the drain's first beat failed");
        for f in ["a", "b", "m/x"] {
            assert!(!fs.exists(format!("/dst/{f}.flux-partial.op1")), "{f}: {c:?}");
            assert!(!fs.exists(format!("/dst/{f}")), "{f} was not published: {c:?}");
        }
        assert_eq!(fs.prepared_count(), 0);
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_copied, out.failures.total()), (0, 0));
    }

    #[test]
    fn a_missing_publish_primitive_during_the_drain_removes_the_remaining_frames_without_a_barrier()
    {
        let fs = two_pending_frames_then_an_ordinary_abort();
        // `primitive_unavailable` recognises `ErrorKind::Unsupported`. The drain's first flush (m) renames x first.
        fs.fail_nth("rename_no_replace", 1, Code::IoError, ErrorKind::Unsupported);
        let calls = std::cell::Cell::new(0);
        // The walk's ordinary abort: the guard before z's creation (8th call) fails with a code that is no stop.
        let guard = || {
            calls.set(calls.get() + 1);
            if calls.get() == 8 {
                Err(FsError::new(Code::IoError, std::io::Error::other("walk")))
            } else {
                Ok(())
            }
        };
        let (r, out, got) =
            batched_with(&fs, policy(64), Durability::Strict, &guard, &no_heartbeat);
        let error = r.unwrap_err();
        // The walk's own ordinary error wins over the drain's stop (spec decision 8); the stop is seen in the log.
        assert_eq!(error.code(), Code::IoError, "{error:?}");
        let c = log(&fs);
        let at = positions(&c, "rename_no_replace(");
        assert_eq!(at.len(), 1, "only the failed rename ran: {c:?}");
        let after = &c[at[0] + 1..];
        assert_eq!(count(after, "sync_all("), 0, "{c:?}");
        assert_eq!(count(after, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(count(after, "rename_"), 0, "{c:?}");
        for f in ["a", "b", "m/x"] {
            assert!(!fs.exists(format!("/dst/{f}.flux-partial.op1")), "{f}: {c:?}");
            assert!(!fs.exists(format!("/dst/{f}")), "{f} was not published: {c:?}");
        }
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(out.files_copied, 0);
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn a_missing_publish_primitive_in_the_walk_removes_the_pending_frames_without_a_barrier() {
        let fs = sources(&[("a", b"a"), ("b", b"b"), ("m/x", b"x")]);
        // m's `DirEnd` flush renames x first, while the root still holds a and b.
        fs.fail_nth("rename_no_replace", 1, Code::IoError, ErrorKind::Unsupported);
        let (r, out, got) =
            batched_with(&fs, policy(64), Durability::Strict, &unguarded, &no_heartbeat);
        let error = r.unwrap_err();
        assert_eq!(error.code(), Code::NoReplacePublishUnavailable, "{error:?}");
        let c = log(&fs);
        let at = positions(&c, "rename_no_replace(");
        assert_eq!(at.len(), 1, "only the failed rename ran: {c:?}");
        let after = &c[at[0] + 1..];
        assert_eq!(count(after, "sync_all("), 0, "{c:?}");
        assert_eq!(count(after, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(count(after, "rename_"), 0, "{c:?}");
        for f in ["a", "b", "m/x"] {
            assert!(!fs.exists(format!("/dst/{f}.flux-partial.op1")), "{f}: {c:?}");
            assert!(!fs.exists(format!("/dst/{f}")), "{f} was not published: {c:?}");
        }
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(out.files_copied, 0);
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn a_drain_time_stop_switches_the_mode_for_every_remaining_frame() {
        let fs = sources(&[("a", b"a"), ("m/c", b"c"), ("m/n/y", b"y"), ("m/n/z/q", b"q")]);
        let calls = std::cell::Cell::new(0);
        // a staged (guards 1-2), m's creation (3), c staged (4-5), n's creation (6), y staged (7-8), z's creation (9)
        // fails ordinarily: three frames are pending. The drain's first flush (m/n) meets a lost lock at its guard
        // (10), and every one after.
        let guard = || {
            calls.set(calls.get() + 1);
            match calls.get() {
                1..=8 => Ok(()),
                9 => Err(FsError::new(Code::IoError, std::io::Error::other("walk"))),
                _ => Err(FsError::new(Code::TargetLockBusy, std::io::Error::other("lost"))),
            }
        };
        let (r, out, got) =
            batched_with(&fs, policy(64), Durability::Strict, &guard, &no_heartbeat);
        let error = r.unwrap_err();
        assert_eq!(error.code(), Code::IoError, "the walk's own error wins: {error:?}");
        assert_eq!(calls.get(), 10, "the drain's first flush met the lost lock at its guard");
        let c = log(&fs);
        // Only m/n's barrier ran, before the stop at its guard: none for m (c) or the root (a) after it.
        assert_eq!(count(&c, "sync_all("), 1, "{c:?}");
        assert_eq!(count(&c, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(count(&c, "rename_"), 0, "{c:?}");
        for f in ["a", "m/c"] {
            assert!(fs.exists(format!("/dst/{f}.flux-partial.op1")), "{f}: {c:?}");
            let kept = got.iter().any(|g| {
                matches!(&g.cause, TreeFailureCause::Copy(e)
                    if e.code() == Code::TargetLockBusy
                        && e.leftover.as_ref().map(|(p, _)| p) == Some(&temp_of(f)))
            });
            assert!(kept, "{f} is reported as lost-lock with its temporary: {got:?}");
        }
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(out.files_copied, 0);
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

    // Cut 9e: the data-sync barrier inside `flush_batch`. `K(n)` and `G(n)` below are the heartbeat and guard calls that
    // precede the first flush of an `n`-file batch (`pinned_pre_flush_call_counts` asserts them, so a shift is visible).

    /// `policy(64)` with one sync worker: the barrier's order is then the entries' order.
    fn one_worker() -> BatchPolicy {
        BatchPolicy { sync_threads: 1, ..policy(64) }
    }

    /// `drop_writer(` calls of the log, in order.
    fn drops(c: &[String]) -> Vec<usize> {
        positions(c, "drop_writer(")
    }

    /// The heartbeat calls before the first flush of an `n`-file batch (the walk's, the stagings' and the root's).
    const fn k(n: u32) -> u32 {
        K_BASE + K_PER_FILE * n
    }

    /// The guard calls before the first flush of an `n`-file batch.
    const fn g(n: u32) -> u32 {
        G_BASE + G_PER_FILE * n
    }

    const K_BASE: u32 = 0;
    const K_PER_FILE: u32 = 3;
    const G_BASE: u32 = 0;
    const G_PER_FILE: u32 = 2;

    #[test]
    fn pinned_pre_flush_call_counts() {
        for n in [2usize, 3] {
            let fs = n_files(n);
            let beats = std::cell::RefCell::new(Vec::new());
            let guards = std::cell::RefCell::new(Vec::new());
            let beat = || {
                beats.borrow_mut().push(fs.calls().len());
                Ok(())
            };
            let guard = || {
                guards.borrow_mut().push(fs.calls().len());
                Ok(())
            };
            let (r, _, _) = batched_with(&fs, one_worker(), Durability::Strict, &guard, &beat);
            r.unwrap();
            let c = log(&fs);
            let last_sync = *positions(&c, "sync_all(").last().unwrap();
            let before = |v: &std::cell::RefCell<Vec<usize>>| {
                v.borrow().iter().filter(|&&len| len <= last_sync).count() as u32
            };
            assert_eq!(before(&beats), k(n as u32), "heartbeat calls before the barrier, n={n}");
            assert_eq!(before(&guards), g(n as u32), "guard calls before the barrier, n={n}");
        }
    }

    #[test]
    fn every_sync_of_a_batch_precedes_its_prepare_many_which_precedes_its_first_rename() {
        let fs = n_files(3);
        let (r, out, _) = batched(&fs, one_worker(), Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        let syncs = positions(&c, "sync_all(");
        let prepare = positions(&c, "claim_prepare_many(")[0];
        assert_eq!(syncs.len(), 3, "{c:?}");
        assert!(syncs.iter().all(|&s| s < prepare), "{c:?}");
        assert!(prepare < renamed(&c, "f0"), "{c:?}");
        assert!(
            syncs.iter().all(|&s| s > created(&c, "f2")),
            "no sync before the last staging: {c:?}"
        );
        assert_eq!(out.files_copied, 3);
    }

    #[test]
    fn a_failed_sync_discards_and_reports_that_entry_and_the_rest_are_noted_and_published() {
        let fs = n_files(3);
        fs.on_nth("create_new", 2, |fs| fs.fail("sync_all", Code::StrictDurabilityUnavailable));
        let (r, out, got) = batched(&fs, one_worker(), Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        assert_eq!(prepares(&c), ["claim_prepare_many(2)"], "{c:?}");
        assert_eq!(out.files_copied, 2);
        assert_eq!(got.len(), 1);
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(
            (e.code(), e.step, e.leftover.is_none()),
            (Code::StrictDurabilityUnavailable, CopyStep::Durability, true)
        );
        assert!(!fs.exists("/dst/f1") && !fs.exists("/dst/f1.flux-partial.op1"));
        let remove = first(&c, "remove_file(/dst/f1.flux-partial.op1)");
        let prepare = positions(&c, "claim_prepare_many(")[0];
        assert!(remove < prepare, "the discard precedes the notes: {c:?}");
    }

    #[test]
    fn a_batch_whose_every_sync_fails_writes_no_note_and_the_walk_goes_on() {
        let fs = sources(&[("a/x", b"x"), ("a/y", b"y"), ("b/z", b"z")]);
        // `on_nth` keeps ONE hook per call name, so the second fault is armed from inside the first.
        fs.on_nth("create_new", 1, |fs| {
            fs.fail("sync_all", Code::IoError);
            fs.on_nth("create_new", 2, |fs| fs.fail("sync_all", Code::IoError));
        });
        let (r, out, got) = batched(&fs, one_worker(), Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        assert_eq!(prepares(&c), ["claim_prepare_many(1)"], "only b's batch is noted: {c:?}");
        assert_eq!((out.files_copied, got.len()), (1, 2));
        assert!(fs.exists("/dst/b/z") && !fs.exists("/dst/a/x") && !fs.exists("/dst/a/y"));
    }

    #[test]
    fn the_writers_are_closed_before_the_post_barrier_heartbeat_on_every_path() {
        for scenario in ["ok", "heartbeat", "lost"] {
            let fs = n_files(2);
            let beats = std::cell::RefCell::new(Vec::new());
            let nbeat = std::cell::Cell::new(0u32);
            let nguard = std::cell::Cell::new(0u32);
            let beat = || {
                beats.borrow_mut().push(fs.calls().len());
                nbeat.set(nbeat.get() + 1);
                if scenario == "heartbeat" && nbeat.get() == k(2) + 1 {
                    return Err(FsError::new(Code::IoError, std::io::Error::other("beat")));
                }
                Ok(())
            };
            let guard = || {
                nguard.set(nguard.get() + 1);
                if scenario == "lost" && nguard.get() > g(2) {
                    return Err(FsError::new(Code::TargetLockBusy, std::io::Error::other("lost")));
                }
                Ok(())
            };
            let (_, _, _) = batched_with(&fs, one_worker(), Durability::Strict, &guard, &beat);
            let c = log(&fs);
            let last_sync = *positions(&c, "sync_all(").last().unwrap();
            let first_beat_after = *beats
                .borrow()
                .iter()
                .find(|&&len| len > last_sync)
                .expect("a heartbeat after the barrier");
            assert!(drops(&c).iter().all(|&d| d < first_beat_after), "{scenario}: {c:?}");
            assert_eq!(drops(&c).len(), 2, "{scenario}: {c:?}");
        }
    }

    #[test]
    fn a_heartbeat_failure_after_the_barrier_removes_every_temporary_and_writes_no_note() {
        let fs = n_files(3);
        let calls = std::cell::Cell::new(0);
        let beat = failing_after(k(3), &calls);
        let (r, _, got) = batched_with(&fs, one_worker(), Durability::Strict, &unguarded, &beat);
        assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
        assert_eq!(fs.prepared_count(), 0);
        for i in 0..3 {
            assert!(
                !fs.exists(format!("/dst/f{i}.flux-partial.op1"))
                    && !fs.exists(format!("/dst/f{i}"))
            );
        }
        assert!(got.is_empty(), "every removal succeeded, nothing to report: {got:?}");
    }

    #[test]
    fn a_heartbeat_failure_during_the_barrier_is_handled_as_the_post_barrier_one_and_not_re_detected()
     {
        let fs = n_files(3);
        fs.delay("sync_all", std::time::Duration::from_millis(20));
        let p = BatchPolicy { beat_every: std::time::Duration::from_millis(10), ..one_worker() };
        let calls = std::cell::Cell::new(0u32);
        let failed_at = std::cell::Cell::new(None);
        let beat = || {
            calls.set(calls.get() + 1);
            if calls.get() == k(3) + 1 {
                failed_at.set(Some(fs.calls().len()));
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        let (r, _, _) = batched_with(&fs, p, Durability::Strict, &unguarded, &beat);
        assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(calls.get(), k(3) + 1, "no heartbeat call after the one that failed");
        // Mutant: the flush hands the barrier `&no_heartbeat`; the failing call then lands on the pair's own heartbeat.
        // The failure must have happened inside the barrier: before the last sync was even begun.
        let c = log(&fs);
        let last_sync = *positions(&c, "sync_all(").last().expect("the barrier synced");
        let at = failed_at.get().expect("the heartbeat failed");
        assert!(at < last_sync, "failed at call {at}, the last sync is call {last_sync}: {c:?}");
        for i in 0..3 {
            assert!(!fs.exists(format!("/dst/f{i}.flux-partial.op1")));
        }
    }

    /// Cut 9e, owner ruling B2': `a0`, `m/x`, `m/y`, `z`; the guard calls and the heartbeats made up to and including the
    /// last of each before `m/y`'s temporary is created, read from an unfailing run's log. `a0` and `m/x` are then
    /// pending (in the root's and in `m`'s batch) when a failure at that call stops the walk.
    fn stop_points_at_the_staging_of_my() -> (u32, u32) {
        let fs = sources(&[("a0", b"a"), ("m/x", b"x"), ("m/y", b"y"), ("z", b"z")]);
        let guards = std::cell::RefCell::new(Vec::new());
        let beats = std::cell::RefCell::new(Vec::new());
        let guard = || {
            guards.borrow_mut().push(fs.calls().len());
            Ok(())
        };
        let beat = || {
            beats.borrow_mut().push(fs.calls().len());
            Ok(())
        };
        let (r, _, _) = batched_with(&fs, one_worker(), Durability::Strict, &guard, &beat);
        r.unwrap();
        let c = log(&fs);
        let at = created(&c, "m/y");
        let before = |v: &std::cell::RefCell<Vec<usize>>| {
            v.borrow().iter().filter(|&&len| len <= at).count() as u32
        };
        (before(&guards), before(&beats))
    }

    #[test]
    fn a_lost_lock_mid_walk_drains_by_keeping_and_reporting_every_pending_temporary_without_a_sync()
    {
        // Mutant: a drain mode that is always `Publish` syncs, notes and publishes the pending files after the loss.
        let (guard_calls, _) = stop_points_at_the_staging_of_my();
        let fs = sources(&[("a0", b"a"), ("m/x", b"x"), ("m/y", b"y"), ("z", b"z")]);
        let calls = std::cell::Cell::new(0);
        // The guard call that guards `m/y`'s temporary is the last one before its creation.
        let guard = failing_after(guard_calls - 1, &calls);
        let (r, out, got) =
            batched_with(&fs, one_worker(), Durability::Strict, &guard, &no_heartbeat);
        let error = r.unwrap_err();
        assert_eq!(error.code(), Code::TargetLockBusy, "{error:?}");
        let c = log(&fs);
        assert_eq!(
            count(&c, "create_new(/dst/m/y"),
            0,
            "the loss hit before m/y was staged: {c:?}"
        );
        assert_eq!(count(&c, "sync_all("), 0, "no barrier after the loss: {c:?}");
        assert_eq!(count(&c, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(count(&c, "rename_"), 0, "{c:?}");
        for t in ["/dst/a0.flux-partial.op1", "/dst/m/x.flux-partial.op1"] {
            assert!(fs.exists(t), "{t} is kept");
        }
        assert!(!fs.exists("/dst/a0") && !fs.exists("/dst/m/x"));
        let mut kept: Vec<PathBuf> = leftovers(&got, Some(&error));
        kept.sort();
        assert_eq!(kept, [temp_of("a0"), temp_of("m/x")], "{got:?} {error:?}");
        for f in &got {
            let TreeFailureCause::Copy(e) = &f.cause else { panic!("{got:?}") };
            assert_eq!(e.code(), Code::TargetLockBusy, "{e:?}");
        }
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(fs.prepared_count(), 0);
        assert_eq!(out.files_copied, 0);
    }

    #[test]
    fn a_heartbeat_failure_mid_walk_drains_by_removing_every_pending_temporary_without_a_sync() {
        let (_, beat_calls) = stop_points_at_the_staging_of_my();
        let fs = sources(&[("a0", b"a"), ("m/x", b"x"), ("m/y", b"y"), ("z", b"z")]);
        let calls = std::cell::Cell::new(0);
        // Pulse-shaped (`Pulse::beat`): the heartbeat before `m/y`'s temporary fails ONCE, every later call is `Ok`.
        let beat = || {
            calls.set(calls.get() + 1);
            if calls.get() == beat_calls {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        let (r, out, got) = batched_with(&fs, one_worker(), Durability::Strict, &unguarded, &beat);
        assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
        let c = log(&fs);
        assert_eq!(
            count(&c, "create_new(/dst/m/y"),
            0,
            "the failure hit before m/y was staged: {c:?}"
        );
        assert!(
            count(&c, "create_new(/dst/m/x") > 0 && count(&c, "create_new(/dst/a0") > 0,
            "{c:?}"
        );
        assert_eq!(count(&c, "sync_all("), 0, "no barrier after the failure: {c:?}");
        assert_eq!(count(&c, "claim_prepare_many("), 0, "{c:?}");
        assert_eq!(count(&c, "rename_"), 0, "{c:?}");
        assert_eq!(count(&c, "rename_no_replace("), 0, "no rename after the failure: {c:?}");
        for t in ["/dst/a0.flux-partial.op1", "/dst/m/x.flux-partial.op1"] {
            assert!(!fs.exists(t), "{t} is removed: {c:?}");
        }
        assert!(!fs.exists("/dst/a0") && !fs.exists("/dst/m/x"));
        assert_eq!(fs.prepared_count(), 0);
        assert!(got.is_empty(), "every removal succeeded: {got:?}");
        assert_eq!(out.files_copied, 0);
    }

    #[test]
    fn a_lost_lock_after_the_barrier_keeps_every_temporary_and_writes_no_note() {
        // Mutant: drop the pair (or move the barrier after it): this test then renames and notes.
        let fs = n_files(3);
        let calls = std::cell::Cell::new(0);
        let guard = failing_after(g(3), &calls);
        let (r, _, got) =
            batched_with(&fs, one_worker(), Durability::Strict, &guard, &no_heartbeat);
        let e = r.unwrap_err();
        assert_eq!(e.code(), Code::TargetLockBusy);
        assert!(e.leftover.is_some(), "entry 0's temporary rides in the error");
        assert_eq!(got.len(), 2, "entries 1 and 2 are reported kept: {got:?}");
        assert_eq!(fs.prepared_count(), 0);
        // The stagings' step-1 sweeps remove (a missing name); the flush removes nothing, so none follows the barrier.
        let c = log(&fs);
        let first_sync = first(&c, "sync_all(");
        assert!(positions(&c, "remove_file(").iter().all(|&r| r < first_sync), "{c:?}");
        for i in 0..3 {
            assert!(fs.exists(format!("/dst/f{i}.flux-partial.op1")));
        }
    }

    #[test]
    fn a_failed_sync_entry_is_still_reported_when_the_lock_is_lost_at_the_pair() {
        let fs = n_files(2);
        fs.on_nth("create_new", 1, |fs| fs.fail("sync_all", Code::StrictDurabilityUnavailable));
        let calls = std::cell::Cell::new(0);
        let guard = failing_after(g(2), &calls);
        let (r, _, got) =
            batched_with(&fs, one_worker(), Durability::Strict, &guard, &no_heartbeat);
        let e = r.unwrap_err();
        assert_eq!(e.code(), Code::TargetLockBusy);
        assert!(
            e.leftover.is_none(),
            "entry 0 is the failed-sync entry: its report carries the leftover, not the error (ruling 9)"
        );
        let reported: Vec<(Code, bool)> = got
            .iter()
            .map(|f| match &f.cause {
                TreeFailureCause::Copy(e) => (e.code(), e.leftover.is_some()),
                c => panic!("{c:?}"),
            })
            .collect();
        assert_eq!(
            reported,
            [(Code::StrictDurabilityUnavailable, true), (Code::TargetLockBusy, true)],
            "{got:?}"
        );
    }

    #[test]
    fn a_failed_sync_entry_under_a_heartbeat_failure_is_reported_once_with_the_sync_code() {
        let fs = n_files(2);
        fs.on_nth("create_new", 1, |fs| fs.fail("sync_all", Code::StrictDurabilityUnavailable));
        let calls = std::cell::Cell::new(0);
        let beat = failing_after(k(2), &calls);
        let (r, _, got) = batched_with(&fs, one_worker(), Durability::Strict, &unguarded, &beat);
        assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
        assert_eq!(got.len(), 1, "{got:?}");
        let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
        assert_eq!(
            (e.code(), e.leftover.is_none()),
            (Code::StrictDurabilityUnavailable, true),
            "removed, so no leftover"
        );
        assert!(!fs.exists("/dst/f0.flux-partial.op1") && !fs.exists("/dst/f1.flux-partial.op1"));
    }

    #[test]
    fn one_and_sixteen_threads_give_the_same_outcome() {
        let run = |threads: usize| {
            let fs = n_files(70);
            let (r, out, got) = batched(
                &fs,
                BatchPolicy { sync_threads: threads, ..policy(64) },
                Durability::Strict,
            );
            r.unwrap();
            (out.files_copied, out.bytes_copied, got.len(), fs.claim_count(), fs.prepared_count())
        };
        let a = run(1);
        let b = run(16);
        assert_eq!(a, b);
        assert_eq!(a.0, 70);
        // This test cannot tell 16 threads from 1 (a one-worker mutant passes it); Task 2's `sixteen_threads_sync_concurrently` does.
    }

    #[test]
    fn barrier_max_records_the_longest_barrier() {
        let fs = n_files(2);
        fs.delay("sync_all", std::time::Duration::from_millis(50));
        let (r, out, _) = batched(&fs, one_worker(), Durability::Strict);
        r.unwrap();
        // Mutant: a hardcoded zero.
        assert!(out.barrier_max >= std::time::Duration::from_millis(50), "{:?}", out.barrier_max);
    }

    #[test]
    fn the_default_policy_syncs_from_sixteen_threads_at_half_the_heartbeat_interval() {
        assert_eq!(
            (BatchPolicy::DEFAULT.sync_threads, BatchPolicy::DEFAULT.beat_every),
            (16, crate::run::HEARTBEAT_INTERVAL / 2)
        );
    }

    // Cut 9e (spec decision 7): the open-writer cap across the frame stack.

    #[test]
    fn open_writers_cap_is_a_quarter_of_the_soft_limit_capped_at_256_and_at_least_1() {
        assert_eq!(open_writers_cap(None), 256);
        assert_eq!(open_writers_cap(Some(1024)), 256);
        assert_eq!(open_writers_cap(Some(4096)), 256);
        assert_eq!(open_writers_cap(Some(64)), 16);
        assert_eq!(open_writers_cap(Some(3)), 1);
    }

    #[test]
    fn the_cap_flushes_the_shallowest_pending_batch_first_and_never_holds_more_writers() {
        let fs = sources(&[("a0", b"a"), ("m/x", b"x"), ("m/y", b"y"), ("m/z", b"z"), ("w", b"w")]);
        let p = BatchPolicy { open_writers: 2, ..one_worker() };
        let (r, out, _) = batched(&fs, p, Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        // Running count of live writers over the log: +1 at create_new, -1 at drop_writer; never above 2.
        let (mut live, mut peak) = (0i64, 0i64);
        for x in &c {
            if x.starts_with("create_new(") {
                live += 1;
                peak = peak.max(live);
            } else if x.starts_with("drop_writer(") {
                live -= 1;
            }
        }
        assert_eq!(peak, 2, "{c:?}");
        assert!(
            renamed(&c, "a0") < created(&c, "m/y"),
            "the parent's batch flushes before m/y is staged: {c:?}"
        );
        assert_eq!(out.files_copied, 5);
        assert!(out.cap_flushes >= 1, "{}", out.cap_flushes);
    }

    #[test]
    fn many_small_directories_never_trip_the_cap() {
        let entries: Vec<(String, Vec<u8>)> =
            (0..40).map(|d| (format!("d{d:02}/f"), b"f".to_vec())).collect();
        let refs: Vec<(&str, &[u8])> =
            entries.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        let fs = sources(&refs);
        let (r, out, _) = batched(&fs, one_worker(), Durability::Strict);
        r.unwrap();
        assert_eq!((out.files_copied, out.cap_flushes), (40, 0));
    }

    // Cut 9e test audit: five closed gaps, each red under the named mutant.

    #[test]
    fn a_cap_flush_that_stops_the_run_is_counted_and_the_remaining_frames_are_kept() {
        let entries: [(&str, &[u8]); 5] =
            [("a0", b"a"), ("m/x", b"x"), ("m/y", b"y"), ("m/z", b"z"), ("w", b"w")];
        let p = BatchPolicy { open_writers: 2, ..one_worker() };
        // Dry run: the 1-based number of the first guard call after the first (cap) flush's barrier.
        let dry = sources(&entries);
        let at = std::cell::RefCell::new(Vec::new());
        let note = || {
            at.borrow_mut().push(dry.calls().len());
            Ok(())
        };
        let (r, _, _) = batched_with(&dry, p, Durability::Strict, &note, &no_heartbeat);
        r.unwrap();
        let first_sync = first(&log(&dry), "sync_all(");
        let n = at
            .borrow()
            .iter()
            .position(|&len| len > first_sync)
            .expect("a guard call in the flush")
            + 1;

        let fs = sources(&entries);
        let calls = std::cell::Cell::new(0);
        let guard = failing_after(u32::try_from(n - 1).unwrap(), &calls);
        let (r, out, got) = batched_with(&fs, p, Durability::Strict, &guard, &no_heartbeat);
        let err = r.unwrap_err();
        assert_eq!(err.code(), Code::TargetLockBusy);
        // Mutant: the increment after the `?` in `walk_into`'s cap loop.
        assert_eq!(out.cap_flushes, 1);
        let c = log(&fs);
        assert_eq!(count(&c, "sync_all("), 1, "only the stopped flush's own barrier: {c:?}");
        assert!(positions(&c, "claim_prepare_many(").is_empty(), "{c:?}");
        assert!(positions(&c, "rename_").is_empty(), "{c:?}");
        for rel in ["a0", "m/x"] {
            assert!(fs.exists(format!("/dst/{rel}.flux-partial.op1")), "{rel}: {c:?}");
            assert!(!fs.exists(format!("/dst/{rel}")), "{rel}");
        }
        assert!(!fs.exists("/dst/m/y.flux-partial.op1"), "m/y was never staged");
        assert!(
            got.iter().all(|f| matches!(&f.cause, TreeFailureCause::Copy(e) if e.code() == Code::TargetLockBusy)),
            "{got:?}"
        );
        assert_eq!(got.len(), 1, "m/x is kept and reported; a0 rides in the error: {got:?}");
        assert_eq!(leftovers(&got, Some(&err)), [temp_of("a0"), temp_of("m/x")]);
        assert_eq!((out.files_copied, fs.prepared_count()), (0, 0));
    }

    #[test]
    fn the_cap_flushes_batches_holding_replace_entries_and_overwrites_them() {
        // Mutant: delete `out.files_overwritten += 1;` in `flush_batch`'s Replace branch.
        let rels = ["a0", "a1", "m/x", "m/y", "m/z"];
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/")).unwrap();
        for d in ["/src", "/src/m", "/dst", "/dst/m"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        for rel in rels {
            fs.write_file(Path::new("/src").join(rel), format!("new-{rel}").as_bytes());
            fs.write_file(Path::new("/dst").join(rel), b"old");
        }
        let p = BatchPolicy { open_writers: 2, ..one_worker() };
        let (r, out, got) = batched(&fs, p, Durability::Strict);
        r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        for rel in rels {
            assert_eq!(
                fs.read_file(Path::new("/dst").join(rel)).as_deref(),
                Some(format!("new-{rel}").as_bytes()),
                "{rel}"
            );
        }
        assert_eq!(out.files_overwritten, 5);
        assert_eq!(out.files_copied, 5);
        assert!(out.cap_flushes >= 1, "{}", out.cap_flushes);
        let c = log(&fs);
        let (mut live, mut peak) = (0i64, 0i64);
        for x in &c {
            if x.starts_with("create_new(") {
                live += 1;
                peak = peak.max(live);
            } else if x.starts_with("drop_writer(") {
                live -= 1;
            }
        }
        assert!((1..=2).contains(&peak), "{peak}: {c:?}");
        assert_eq!(fs.prepared_count(), 0);
        let dst = strong_id(&fs, "/dst");
        let dst_m = strong_id(&fs, "/dst/m");
        for rel in ["a0", "a1"] {
            assert_eq!(fs.claim(dst, rel).map(|r| r.status), Some(ClaimStatus::Created), "{rel}");
        }
        for rel in ["x", "y", "z"] {
            assert_eq!(
                fs.claim(dst_m, rel).map(|r| r.status),
                Some(ClaimStatus::Created),
                "m/{rel}"
            );
        }
    }

    #[test]
    fn barrier_max_keeps_the_longest_barrier_across_batches() {
        let fs = n_files(4);
        let slow = std::time::Duration::from_millis(60);
        fs.delay("sync_all", slow);
        // The first batch's notes are written after its barrier: the delay is off for the second batch.
        fs.on_nth("claim_prepare_many", 1, |fs| fs.delay("sync_all", std::time::Duration::ZERO));
        let (r, out, _) = batched(&fs, policy(2), Durability::Strict);
        r.unwrap();
        let c = log(&fs);
        assert_eq!(prepares(&c).len(), 2, "two batches: {c:?}");
        assert_eq!(count(&c, "sync_all("), 4, "{c:?}");
        // Mutant: `out.barrier_max = started.elapsed()` (the fast second batch overwrites).
        assert!(out.barrier_max >= slow, "{:?}", out.barrier_max);
    }

    // ---- Cut 11a, Task 3: the engine's dry mode ----

    use crate::dry_run::{NullClaimStore, PlannedAction, PlannedEntry, PlannedObject, SkipReason};

    thread_local! {
        /// The length of the call log when `dry_run` started: the fixtures' own `create_dir`s precede it.
        static DRY_BASE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// A dry `copy_tree_frames` over `src` into `dst_root` (opened, `RootIdentity::Existing`) or, with `None`, into a
    /// destination that does not exist (`/dst`, whose parent `/` is the planned root's identity).
    fn dry_run(
        fs: &FaultFs,
        src: &str,
        dst_root: Option<&str>,
        o: &CopyOptions,
    ) -> (std::result::Result<TreeOutcome, TreeAbort>, Vec<TreeFailure>) {
        DRY_BASE.set(fs.calls().len());
        let source =
            prepare_source(fs, Path::new(src), Path::new("/dst"), MountPolicy::of(o)).unwrap();
        let null = std::cell::RefCell::new(NullClaimStore);
        let cx = Shared {
            fs,
            src_root: Path::new(src),
            src_identity: source.identity,
            opts: o,
            guard: &unguarded,
            beat: &no_heartbeat,
            claims: Some(&null),
            resume: false,
            batch: BatchPolicy::DEFAULT,
            ckpt: None,
            dry_run: true,
            dry_prior: None,
            dry_partials: None,
            key_prefix: None,
            hardlinks: None,
            anchor_root: None,
        };
        let (root, identity) = match dst_root {
            Some(d) => {
                let root = fs.destination_root(Path::new(d)).unwrap();
                let id = root.identity().unwrap();
                (Some(root), RootIdentity::Existing(id))
            }
            None => {
                let parent = fs.destination_root(Path::new("/")).unwrap();
                (None, RootIdentity::Planned(parent.identity().unwrap()))
            }
        };
        let mut out = TreeOutcome::default();
        let mut got = Vec::new();
        let (_root, r) =
            copy_tree_frames(&cx, source.events, root, identity, false, &mut out, &mut |f| {
                got.push(f)
            });
        let r = match r {
            Ok(()) => Ok(out),
            Err(error) => Err(TreeAbort { error, outcome: out }),
        };
        (r, got)
    }

    /// Every `Planned` record in `got`, in order.
    fn planned(got: &[TreeFailure]) -> Vec<(PathBuf, PlannedEntry)> {
        got.iter()
            .filter_map(|f| match &f.cause {
                TreeFailureCause::Planned(e) => Some((f.path.clone(), *e)),
                _ => None,
            })
            .collect()
    }

    /// `planned`, with the path spelled `/` and the entry split.
    fn plan_of(got: &[TreeFailure]) -> Vec<(String, PlannedObject, PlannedAction)> {
        planned(got)
            .into_iter()
            .map(|(p, e)| (p.to_string_lossy().replace('\\', "/"), e.object, e.action))
            .collect()
    }

    fn pl(
        path: &str,
        object: PlannedObject,
        action: PlannedAction,
    ) -> (String, PlannedObject, PlannedAction) {
        (path.to_string(), object, action)
    }

    /// True for a call that names no path, or one under a destination or source root of these tests (`/p`, `/src`, and
    /// the `/d`, `/dst`, `/out` roots this module's fixtures also use) or below.
    /// A call that names a claim key, not a path, is always in scope.
    fn in_scope(call: &str) -> bool {
        let name = call.split('(').next().unwrap_or(call);
        if name.starts_with("claim_")
            || ["try_lock", "write_at_start", "lock_sync_all"].contains(&name)
        {
            return true;
        }
        let Some((_, rest)) = call.split_once('(') else {
            return true;
        };
        let arg = rest.rsplit_once(')').map_or(rest, |(a, _)| a).replace('\\', "/");
        ["/p", "/src", "/d", "/dst", "/out", "/outside"]
            .iter()
            .any(|root| arg == *root || arg.starts_with(&format!("{root}/")))
    }

    /// A dry run wrote nothing and asked nothing of the destination's write side, and did stat the source, under `/p` or
    /// `/src`; a dry resume's `<temp>` lives elsewhere and is asserted by its own tests.
    fn forbidden(fs: &FaultFs) {
        let all = fs.calls();
        let calls = &all[DRY_BASE.get()..];
        let scoped: Vec<&String> = calls.iter().filter(|c| in_scope(c)).collect();
        for banned in [
            "create_dir(",
            "create_new(",
            "create_symlink(",
            "rename_no_replace(",
            "rename_replace(",
            "remove_file(",
            "remove_dir(",
            "open_read(",
            "set_times(",
            "set_permissions(",
            "create_lock(",
            "open_lock(",
            "open_claim_store(",
            "create_claim_store(",
            "try_lock",
            "write_at_start",
            "lock_sync_all",
            "claim_",
        ] {
            assert!(!scoped.iter().any(|c| c.starts_with(banned)), "{banned} in {calls:?}");
        }
        assert!(calls.iter().any(|c| c.starts_with("metadata(")), "no source stat: {calls:?}");
        // The allow-list (the fake's own record names for I1's read-only methods): any other call after the watermark is
        // a destination-side effect or ask the dry mode must not make.
        const ALLOWED: [&str; 9] = [
            "open_dir",
            "metadata",
            "read_dir",
            "read_link",
            "mount_root",
            "source_mount_root",
            "canonical_path",
            "lock_capability",
            "read_file",
        ];
        for c in scoped {
            let name = c.split('(').next().unwrap_or(c);
            assert!(ALLOWED.contains(&name), "{c} is not an allowed dry-run call: {:?}", calls);
        }
    }

    /// `/src/a` only (1 byte). `/dst` absent.
    fn flat() -> FaultFs {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/")).unwrap();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.write_file("/src/a", b"A");
        fs
    }

    fn weak_id() -> FileIdentity {
        FileIdentity::Weak(ObjectId { volume: 7, index: 1 })
    }

    /// The five counters the dry run must share with the live one.
    fn tally(out: &TreeOutcome) -> (u64, u64, u64, u64, u64) {
        (
            out.files_copied,
            out.files_overwritten,
            out.files_skipped,
            out.bytes_skipped,
            out.failures.total(),
        )
    }

    #[test]
    fn dry_a_fresh_destination_plans_every_entry_and_writes_nothing() {
        let fs = tree();
        let (r, got) = dry_run(&fs, "/src", None, &opts());
        let out = r.unwrap();
        assert_eq!(
            plan_of(&got),
            vec![
                pl("a", PlannedObject::File, PlannedAction::Copy),
                pl("sub", PlannedObject::Dir, PlannedAction::CreateDir),
                pl("sub/b", PlannedObject::File, PlannedAction::Copy),
            ]
        );
        assert_eq!(
            (out.files_copied, out.bytes_copied, out.directories_created, out.files_total),
            (2, 3, 1, 2)
        );
        assert_eq!(out.failures.total(), 0);
        assert!(!fs.exists("/dst"));
        forbidden(&fs);

        // The same under Strict: a planned root borrows its parent's strength, so it is not refused.
        let fs = tree();
        let (r, got) = dry_run(&fs, "/src", None, &strict());
        let out = r.unwrap();
        assert_eq!(planned(&got).len(), 3, "{got:?}");
        assert_eq!(out.files_copied, 2);
        forbidden(&fs);
    }

    #[test]
    fn dry_an_existing_destination_decides_each_policy_as_the_live_run() {
        use flux_fs::ExistingPolicy::{Overwrite, SkipExisting, Update};
        let fixture = || {
            let fs = tree();
            fs.create_dir(Path::new("/dst")).unwrap();
            fs.create_dir(Path::new("/dst/sub")).unwrap();
            fs.write_file("/dst/a", b"X");
            fs.write_file("/dst/sub/b", b"BBBB");
            let t = |s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s);
            fs.set_modified("/src/a", t(100));
            fs.set_modified("/dst/a", t(200));
            fs
        };
        let file = PlannedObject::File;
        let cases = [
            (
                Overwrite,
                vec![
                    pl("a", file, PlannedAction::Overwrite),
                    pl("sub/b", file, PlannedAction::Overwrite),
                ],
            ),
            (
                SkipExisting,
                vec![
                    pl("a", file, PlannedAction::Skip(SkipReason::Existing)),
                    pl("sub/b", file, PlannedAction::Skip(SkipReason::Existing)),
                ],
            ),
            (
                Update,
                vec![
                    pl("a", file, PlannedAction::Skip(SkipReason::UpToDate)),
                    pl("sub/b", file, PlannedAction::Overwrite),
                ],
            ),
        ];
        for (policy, want) in cases {
            let o = with_existing(policy);
            let dry = fixture();
            let (r, got) = dry_run(&dry, "/src", Some("/dst"), &o);
            let dry_out = r.unwrap();
            assert_eq!(plan_of(&got), want, "{policy:?}");
            forbidden(&dry);
            assert_eq!(dry.read_file("/dst/a").as_deref(), Some(&b"X"[..]), "{policy:?}");

            let live = fixture();
            let (r, live_out, _) = link_run(&live, &o, false, policy_of(1), &unguarded, no_seed);
            r.unwrap();
            assert_eq!(tally(&dry_out), tally(&live_out), "{policy:?}");
        }
    }

    /// `policy(files)` under a name the policy loop above does not shadow.
    fn policy_of(files: usize) -> BatchPolicy {
        policy(files)
    }

    #[test]
    fn dry_a_planned_subdirectory_inherits_its_parents_claim_path() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/a", b"X");
        let o = with_existing(flux_fs::ExistingPolicy::SkipExisting);
        let (r, got) = dry_run(&fs, "/src", Some("/dst"), &o);
        let out = r.unwrap();
        assert_eq!(
            plan_of(&got),
            vec![
                pl("a", PlannedObject::File, PlannedAction::Skip(SkipReason::Existing)),
                pl("sub", PlannedObject::Dir, PlannedAction::CreateDir),
                pl("sub/b", PlannedObject::File, PlannedAction::Copy),
            ]
        );
        assert_eq!(out.replace_degraded, None);
        assert_eq!((out.files_skipped, out.files_copied, out.directories_created), (1, 1, 1));
        forbidden(&fs);
    }

    #[test]
    fn dry_a_planned_subdirectory_under_a_weak_parent_notes_replace_degraded() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_identity("/dst", weak_id());
        let (r, got) = dry_run(&fs, "/src", Some("/dst"), &opts());
        let out = r.unwrap();
        assert_eq!(
            plan_of(&got),
            vec![
                pl("a", PlannedObject::File, PlannedAction::Copy),
                pl("sub", PlannedObject::Dir, PlannedAction::CreateDir),
                pl("sub/b", PlannedObject::File, PlannedAction::Copy),
            ]
        );
        // The weak root notes itself (at ""), and the planned child, whose identity is `Unavailable` under a parent
        // that cannot key claims, notes again: two directories, the first as the example.
        assert_eq!(
            out.replace_degraded,
            Some(DegradedGroup { count: 2, example: PathBuf::from("") })
        );
        forbidden(&fs);

        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_identity("/dst", weak_id());
        let (r, _) = dry_run(&fs, "/src", Some("/dst"), &strict());
        let abort = r.unwrap_err();
        assert_eq!(abort.error.code(), Code::SafetyRejected);
    }

    #[test]
    fn dry_a_file_or_link_at_a_directory_name_is_the_live_create_dir_failure() {
        let create_dir_failures = |got: &[TreeFailure]| -> Vec<(PathBuf, Code)> {
            got.iter()
                .filter_map(|f| match &f.cause {
                    TreeFailureCause::CreateDir(e) => Some((f.path.clone(), e.code)),
                    _ => None,
                })
                .collect()
        };
        for (is_link, code) in [(false, Code::DestinationError), (true, Code::SafetyRejected)] {
            let fs = tree();
            fs.create_dir(Path::new("/dst")).unwrap();
            if is_link {
                fs.add_symlink("/dst/sub");
            } else {
                fs.write_file("/dst/sub", b"f");
            }
            let (r, got) = dry_run(&fs, "/src", Some("/dst"), &opts());
            let out = r.unwrap();
            assert_eq!(create_dir_failures(&got), vec![(PathBuf::from("sub"), code)], "{got:?}");
            assert_eq!(out.failures.create_dir, 1);
            assert_eq!(
                plan_of(&got),
                vec![pl("a", PlannedObject::File, PlannedAction::Copy)],
                "sub/b yields nothing: {got:?}"
            );
            forbidden(&fs);
        }
    }

    #[test]
    fn dry_a_skipped_target_never_reaches_the_gate() {
        let fixture = || {
            let fs = flat();
            fs.set_identity("/src/a", weak_id());
            fs.create_dir(Path::new("/dst")).unwrap();
            fs.write_file("/dst/a", b"old");
            fs
        };
        let o = with_existing(flux_fs::ExistingPolicy::SkipExisting);
        let dry = fixture();
        let (r, got) = dry_run(&dry, "/src", Some("/dst"), &o);
        let dry_out = r.unwrap();
        assert_eq!(
            plan_of(&got),
            vec![pl("a", PlannedObject::File, PlannedAction::Skip(SkipReason::Existing))]
        );
        assert_eq!(dry_out.files_degraded, 0);
        assert!(dry_out.warnings.is_empty(), "{:?}", dry_out.warnings);

        let live = fixture();
        let (r, live_out, _) = link_run(&live, &o, false, policy(1), &unguarded, no_seed);
        r.unwrap();
        assert_eq!(live_out.files_degraded, 0);
        assert!(live_out.warnings.is_empty());
        assert_eq!(tally(&dry_out), tally(&live_out));
    }

    #[test]
    fn dry_a_hardlink_alias_of_the_source_is_refused_as_live_does() {
        let fixture = || {
            let fs = flat();
            fs.create_dir(Path::new("/dst")).unwrap();
            fs.write_file("/dst/a", b"old");
            fs.set_identity("/dst/a", identity_of(&fs, "/src/a"));
            fs
        };
        let o = with_existing(flux_fs::ExistingPolicy::Overwrite);
        let dry = fixture();
        let (r, got) = dry_run(&dry, "/src", Some("/dst"), &o);
        let dry_out = r.unwrap();
        let e = only_copy_failure(&got, "a");
        assert_eq!(e.code(), Code::SafetyRejected);
        assert_eq!((dry_out.files_copied, dry_out.failures.copy), (0, 1));
        forbidden(&dry);

        let live = fixture();
        let (r, _, live_got) = link_run(&live, &o, false, policy(1), &unguarded, no_seed);
        r.unwrap();
        assert_eq!(only_copy_failure(&live_got, "a").code(), Code::SafetyRejected);
    }

    #[test]
    fn dry_a_weak_source_overwriting_an_existing_file_is_degraded_in_both_modes() {
        let fixture = || {
            let fs = flat();
            fs.set_identity("/src/a", weak_id());
            fs.create_dir(Path::new("/dst")).unwrap();
            fs.write_file("/dst/a", b"old");
            fs
        };
        let o = with_existing(flux_fs::ExistingPolicy::Overwrite);
        let dry = fixture();
        let (r, got) = dry_run(&dry, "/src", Some("/dst"), &o);
        let dry_out = r.unwrap();
        assert_eq!(dry_out.files_degraded, 1);
        assert_eq!(plan_of(&got), vec![pl("a", PlannedObject::File, PlannedAction::Overwrite)]);
        let live = fixture();
        let (r, live_out, _) = link_run(&live, &o, false, policy(1), &unguarded, no_seed);
        r.unwrap();
        assert_eq!(live_out.files_degraded, 1);

        let so = CopyOptions { safety: Safety::Strict, ..o.clone() };
        let dry = fixture();
        let (r, got) = dry_run(&dry, "/src", Some("/dst"), &so);
        r.unwrap();
        assert_eq!(only_copy_failure(&got, "a").code(), Code::SafetyRejected);
        let live = fixture();
        let (r, _, live_got) = link_run(&live, &so, false, policy(1), &unguarded, no_seed);
        r.unwrap();
        assert_eq!(only_copy_failure(&live_got, "a").code(), Code::SafetyRejected);
    }

    #[test]
    fn dry_links_plan_create_symlink_overwrite_and_skip_and_links_skip_is_unchanged() {
        use flux_fs::ExistingPolicy::{Overwrite, SkipExisting, Update};
        let sym = PlannedObject::Symlink;

        // A new link, into a destination that does not exist.
        let fs = links(&[("l", "x")]);
        let (r, got) = dry_run(&fs, "/src", None, &opts());
        let out = r.unwrap();
        assert_eq!(plan_of(&got), vec![pl("l", sym, PlannedAction::CreateSymlink)]);
        assert_eq!((out.files_copied, out.bytes_copied, out.files_overwritten), (1, 0, 0));
        assert_eq!(out.files_total, 1);
        forbidden(&fs);

        let with_dst = |payload: &str| {
            let fs = links(&[("l", "x")]);
            fs.create_dir(Path::new("/dst")).unwrap();
            fs.add_symlink_to("/dst/l", payload, LinkKind::Unspecified);
            fs
        };
        // A differing link, replaced.
        let fs = with_dst("y");
        let (r, got) = dry_run(&fs, "/src", Some("/dst"), &with_existing(Overwrite));
        let out = r.unwrap();
        assert_eq!(plan_of(&got), vec![pl("l", sym, PlannedAction::Overwrite)]);
        assert_eq!((out.files_copied, out.files_overwritten, out.bytes_copied), (1, 1, 0));
        assert_eq!(link_at(&fs, "/dst/l").payload, "y");
        forbidden(&fs);
        // An equal link under Update, and any link under SkipExisting.
        let fs = with_dst("x");
        let (r, got) = dry_run(&fs, "/src", Some("/dst"), &with_existing(Update));
        let out = r.unwrap();
        assert_eq!(plan_of(&got), vec![pl("l", sym, PlannedAction::Skip(SkipReason::UpToDate))]);
        assert_eq!((out.files_skipped, out.files_copied, out.bytes_skipped), (1, 0, 0));
        let fs = with_dst("y");
        let (r, got) = dry_run(&fs, "/src", Some("/dst"), &with_existing(SkipExisting));
        let out = r.unwrap();
        assert_eq!(plan_of(&got), vec![pl("l", sym, PlannedAction::Skip(SkipReason::Existing))]);
        assert_eq!(out.files_skipped, 1);

        // `--links skip`: the unchanged cause, and no planned record.
        let fs = links(&[("l", "x")]);
        let o = CopyOptions { links: Links::Skip, ..opts() };
        let (r, got) = dry_run(&fs, "/src", None, &o);
        let out = r.unwrap();
        assert!(planned(&got).is_empty(), "{got:?}");
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0].cause, TreeFailureCause::SymlinkSkipped), "{got:?}");
        assert_eq!(out.symlinks_skipped, 1);
    }

    #[test]
    fn dry_a_source_that_fails_to_list_is_the_live_walk_failure() {
        let fs = tree();
        fs.fail_nth("read_dir", 2, Code::PermissionDenied, ErrorKind::PermissionDenied);
        let (r, got) = dry_run(&fs, "/src", None, &opts());
        let out = r.unwrap();
        let walks: Vec<&TreeFailure> =
            got.iter().filter(|f| matches!(f.cause, TreeFailureCause::Walk(_))).collect();
        assert_eq!(walks.len(), 1, "{got:?}");
        assert_eq!(walks[0].path, PathBuf::from("sub"));
        assert_eq!(out.failures.walk, 1);
        forbidden(&fs);
    }

    #[test]
    fn dry_reserved_and_mount_and_special_entries_report_as_live() {
        let fs = tree();
        fs.create_dir(Path::new("/src/.flux")).unwrap();
        fs.create_dir(Path::new("/src/.flux/operations")).unwrap();
        fs.write_file("/src/.flux/operations/x", b"x");
        mounted(&fs, "/src/m");
        fs.add_special("/src/z");
        let (r, got) = dry_run(&fs, "/src", None, &opts());
        let out = r.unwrap();

        let reserved: Vec<_> = got
            .iter()
            .filter_map(|f| match &f.cause {
                TreeFailureCause::CreateDir(e) => Some((f.path.clone(), e.code)),
                _ => None,
            })
            .collect();
        assert_eq!(
            reserved,
            vec![(PathBuf::from(".flux/operations"), Code::ControlPlaneNamespaceConflict)],
            "{got:?}"
        );
        assert!(
            !plan_of(&got).iter().any(|(p, _, _)| p.starts_with(".flux/operations/")),
            "{got:?}"
        );
        // The mount: planned like any directory, then reported skipped.
        let at = got.iter().position(|f| f.path == Path::new("m")).expect("m is reported");
        assert!(
            matches!(
                got[at].cause,
                TreeFailureCause::Planned(PlannedEntry {
                    object: PlannedObject::Dir,
                    action: PlannedAction::CreateDir
                })
            ),
            "{got:?}"
        );
        assert!(matches!(got[at + 1].cause, TreeFailureCause::MountSkipped), "{got:?}");
        assert_eq!(out.mounts_skipped, 1);
        assert!(
            got.iter().any(|f| f.path == Path::new("z")
                && matches!(f.cause, TreeFailureCause::SpecialFileSkipped)),
            "{got:?}"
        );
        assert_eq!(out.special_files_skipped, 1);
        forbidden(&fs);
    }

    /// A live run over `fs` into `/dst` as the run makes it: the root is `root_created` (DEST was absent) or not, and
    /// `/dst/.flux` exists already, because the run's own workspace made it before the walk.
    fn live_with_workspace(
        fs: &FaultFs,
        o: &CopyOptions,
        root_created: bool,
    ) -> (TreeOutcome, Vec<TreeFailure>) {
        let source =
            prepare_source(fs, Path::new("/src"), Path::new("/dst"), MountPolicy::Cross).unwrap();
        if !fs.exists(Path::new("/dst")) {
            fs.create_dir(Path::new("/dst")).unwrap();
        }
        if !fs.exists(Path::new("/dst/.flux")) {
            fs.create_dir(Path::new("/dst/.flux")).unwrap();
        }
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let store = root.create_claim_store(OsStr::new("state.db"), o.durability).unwrap();
        let claims = std::cell::RefCell::new(store);
        let cx = Shared {
            fs,
            src_root: Path::new("/src"),
            src_identity: source.identity,
            opts: o,
            guard: &unguarded,
            beat: &no_heartbeat,
            claims: Some(&claims),
            resume: false,
            batch: BatchPolicy::DEFAULT,
            ckpt: None,
            dry_run: false,
            dry_prior: None,
            dry_partials: None,
            key_prefix: None,
            hardlinks: None,
            anchor_root: None,
        };
        let mut out = TreeOutcome::default();
        let mut got = Vec::new();
        let (_root, r) =
            copy_tree_at(&cx, source.events, root, root_created, &mut out, &mut |f| got.push(f));
        r.unwrap();
        (out, got)
    }

    /// Every `Copy` or `CreateDir` failure of `got`: path, code, step (the step only a copy carries).
    fn failure_codes(got: &[TreeFailure]) -> Vec<(PathBuf, Code, Option<CopyStep>)> {
        got.iter()
            .filter_map(|f| match &f.cause {
                TreeFailureCause::Copy(e) => Some((f.path.clone(), e.code(), Some(e.step))),
                TreeFailureCause::CreateDir(e) => Some((f.path.clone(), e.code, None)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn dry_a_top_level_flux_directory_merges_like_live() {
        // Cut 11a, ruling P11. Mutant: drop the `.flux` special case in `walk_into` (the directory is then planned
        // and counted as a created one, and `.flux/other/x` is no longer judged under a fresh frame alike).
        let fixture = || {
            let fs = tree();
            fs.create_dir(Path::new("/src/.flux")).unwrap();
            fs.create_dir(Path::new("/src/.flux/operations")).unwrap();
            fs.write_file("/src/.flux/operations/x", b"x");
            fs.create_dir(Path::new("/src/.flux/other")).unwrap();
            fs.write_file("/src/.flux/other/x", b"xx");
            fs
        };
        for dest_exists in [false, true] {
            let dry = fixture();
            if dest_exists {
                dry.create_dir(Path::new("/dst")).unwrap();
            }
            let (r, got) = dry_run(&dry, "/src", dest_exists.then_some("/dst"), &opts());
            let dry_out = r.unwrap();
            let plan = plan_of(&got);
            assert!(!plan.iter().any(|(p, _, _)| p == ".flux"), "{dest_exists}: {plan:?}");
            assert!(plan.contains(&pl(
                ".flux/other",
                PlannedObject::Dir,
                PlannedAction::CreateDir
            )));
            assert!(plan.contains(&pl(".flux/other/x", PlannedObject::File, PlannedAction::Copy)));
            assert!(!plan.iter().any(|(p, _, _)| p.starts_with(".flux/operations")), "{plan:?}");
            forbidden(&dry);

            let live = fixture();
            let (live_out, live_got) = live_with_workspace(&live, &opts(), !dest_exists);
            assert_eq!(
                failure_codes(&got),
                vec![(
                    PathBuf::from(".flux/operations"),
                    Code::ControlPlaneNamespaceConflict,
                    None
                )],
                "{dest_exists}"
            );
            assert_eq!(failure_codes(&got), failure_codes(&live_got), "{dest_exists}");
            assert_eq!(dry_out.directories_created, live_out.directories_created, "{dest_exists}");
            assert_eq!(tally(&dry_out), tally(&live_out), "{dest_exists}");
            assert!(live.exists("/dst/.flux/other/x"), "live copied the other child");
        }
    }

    #[test]
    fn dry_a_top_level_flux_file_fails_like_live() {
        // Cut 11a, ruling P11: live fails a root-level FILE or LINK named `.flux` because DEST/.flux is a directory.
        // DEST present: `copy_one`'s resolve (DESTINATION_ERROR); DEST absent (a fresh root): the identity gate
        // (SAFETY_REJECTED). Mutant: drop the `.flux` special case.
        let file_fixture = || {
            let fs = flat();
            fs.write_file("/src/.flux", b"f");
            fs
        };
        let link_fixture = || {
            let fs = links(&[(".flux", "x")]);
            fs.write_file("/src/a", b"A");
            fs
        };
        type Fixture<'a> = &'a dyn Fn() -> FaultFs;
        let fixtures: [(&str, Fixture<'_>); 2] = [("file", &file_fixture), ("link", &link_fixture)];
        for (what, fixture) in fixtures {
            for (dest_exists, code) in
                [(true, Code::DestinationError), (false, Code::SafetyRejected)]
            {
                let dry = fixture();
                if dest_exists {
                    dry.create_dir(Path::new("/dst")).unwrap();
                }
                let (r, got) = dry_run(&dry, "/src", dest_exists.then_some("/dst"), &opts());
                let dry_out = r.unwrap();
                let want = vec![(PathBuf::from(".flux"), code, Some(CopyStep::Gate))];
                assert_eq!(failure_codes(&got), want, "{what} {dest_exists}");
                assert!(!plan_of(&got).iter().any(|(p, _, _)| p == ".flux"), "{got:?}");
                assert_eq!(dry_out.failures.copy, 1);
                forbidden(&dry);

                let live = fixture();
                let (live_out, live_got) = live_with_workspace(&live, &opts(), !dest_exists);
                assert_eq!(failure_codes(&live_got), want, "live {what} {dest_exists}");
                assert_eq!(tally(&dry_out), tally(&live_out), "{what} {dest_exists}");
                assert_eq!(dry_out.files_total, live_out.files_total, "{what} {dest_exists}");
            }
        }
    }

    #[test]
    fn dry_a_temporary_name_over_the_limit_is_a_predicted_create_failure() {
        // Cut 11a, ruling P12: live fails `create_new` of an over-long temporary with IO_ERROR before any byte moves.
        // The fake enforces no name limit, so only the dry side is asserted here; the real-fs corpus pairs it with
        // a live run. Mutant: drop the length check in `dry_stop_file` / `dry_stop_link`.
        let long = "n".repeat(240);
        let fixture = || {
            let fs = links(&[(long.as_str(), "x")]);
            fs.write_file(format!("/src/{}", "f".repeat(240)), b"F");
            fs.write_file("/src/ok", b"K");
            fs
        };
        for dest_exists in [false, true] {
            let fs = fixture();
            if dest_exists {
                fs.create_dir(Path::new("/dst")).unwrap();
            }
            let (r, got) = dry_run(&fs, "/src", dest_exists.then_some("/dst"), &opts());
            let out = r.unwrap();
            let mut failures = failure_codes(&got);
            failures.sort_by(|a, b| a.0.cmp(&b.0));
            let mut want = vec![
                (PathBuf::from("f".repeat(240)), Code::IoError, Some(CopyStep::Create)),
                (PathBuf::from(&long), Code::IoError, Some(CopyStep::Create)),
            ];
            want.sort_by(|a, b| a.0.cmp(&b.0));
            assert_eq!(failures, want, "{dest_exists}");
            assert_eq!(plan_of(&got), vec![pl("ok", PlannedObject::File, PlannedAction::Copy)]);
            assert_eq!((out.failures.copy, out.files_copied, out.files_total), (2, 1, 3));
            forbidden(&fs);
        }
        // The edge: the longest name whose temporary still fits plans.
        let fs = flat();
        let temp_extra =
            flux_fs::temp_path(Path::new("x"), &opts().operation_id).file_name().unwrap().len() - 1;
        let fits = "m".repeat(crate::lock::site::NAME_LIMIT - temp_extra);
        fs.write_file(format!("/src/{fits}"), b"F");
        let (r, got) = dry_run(&fs, "/src", None, &opts());
        r.unwrap();
        assert!(failure_codes(&got).is_empty(), "{got:?}");
    }

    #[test]
    fn dry_an_existing_flux_directory_at_dest_resolves_its_entries_like_live() {
        // Cut 11a, ruling P11 refined: the special case holds only while `DEST/.flux` is absent. Mutant: apply it
        // unconditionally (`.flux/other/x` would plan as new and `.flux/other` as created).
        use flux_fs::ExistingPolicy::{Overwrite, Update};
        let fixture = || {
            let fs = tree();
            fs.create_dir(Path::new("/src/.flux")).unwrap();
            fs.create_dir(Path::new("/src/.flux/other")).unwrap();
            fs.write_file("/src/.flux/other/x", b"N");
            fs.create_dir(Path::new("/dst")).unwrap();
            fs.create_dir(Path::new("/dst/.flux")).unwrap();
            fs.create_dir(Path::new("/dst/.flux/other")).unwrap();
            fs.write_file("/dst/.flux/other/x", b"O");
            let t = |s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s);
            fs.set_modified("/src/.flux/other/x", t(100));
            fs.set_modified("/dst/.flux/other/x", t(200));
            fs
        };
        for (policy, action, skipped, overwritten) in [
            (Update, PlannedAction::Skip(SkipReason::UpToDate), 1, 0),
            (Overwrite, PlannedAction::Overwrite, 0, 1),
        ] {
            let o = with_existing(policy);
            let dry = fixture();
            let (r, got) = dry_run(&dry, "/src", Some("/dst"), &o);
            let dry_out = r.unwrap();
            let plan = plan_of(&got);
            assert!(plan.contains(&pl(".flux/other/x", PlannedObject::File, action)), "{plan:?}");
            assert!(!plan.iter().any(|(p, _, _)| p == ".flux" || p == ".flux/other"), "{plan:?}");
            forbidden(&dry);

            let live = fixture();
            let (live_out, _) = live_with_workspace(&live, &o, false);
            for out in [&dry_out, &live_out] {
                assert_eq!((out.files_skipped, out.files_overwritten), (skipped, overwritten));
            }
            assert_eq!(dry_out.directories_created, live_out.directories_created, "{policy:?}");
            assert_eq!(tally(&dry_out), tally(&live_out), "{policy:?}");
        }
    }

    // Several sources, task 7 part A: the per-root driver. `/src/<dir>` are folder roots; a file root is
    // `/src/<name>` itself, whose parent folder is `/src`.

    /// What `copy_roots_at` reports, with the run's outcome and the streamed failures.
    struct RootsRun {
        done: Vec<RootDone>,
        result: std::result::Result<(), CopyError>,
        out: TreeOutcome,
        got: Vec<TreeFailure>,
    }

    /// `copy_roots_at` into a pre-made `/dst` (claim store at `/dst/state.db`, Normal durability), the roots in the
    /// order given: `(name, source path, is a file)`.
    fn roots_run(fs: &FaultFs, specs: &[(&str, &str, bool)], guard: &Guard<'_>) -> RootsRun {
        roots_run_with(fs, specs, guard, Durability::Normal, policy(1))
    }

    /// `roots_run` at `durability` under `policy` (Strict with a batch writes prepared notes).
    fn roots_run_with(
        fs: &FaultFs,
        specs: &[(&str, &str, bool)],
        guard: &Guard<'_>,
        durability: Durability,
        policy: BatchPolicy,
    ) -> RootsRun {
        if !fs.exists(Path::new("/dst")) {
            fs.create_dir(Path::new("/dst")).unwrap();
        }
        let dest = fs.destination_root(Path::new("/dst")).unwrap();
        let store = dest.create_claim_store(OsStr::new("state.db"), durability).unwrap();
        let claims = std::cell::RefCell::new(store);
        let mut o = opts();
        o.durability = durability;
        let base = Shared {
            fs,
            src_root: Path::new("/src"),
            src_identity: FileIdentity::Unavailable,
            opts: &o,
            guard,
            beat: &no_heartbeat,
            claims: Some(&claims),
            resume: false,
            batch: policy,
            ckpt: None,
            dry_run: false,
            dry_prior: None,
            dry_partials: None,
            key_prefix: None,
            hardlinks: None,
            anchor_root: None,
        };
        // A source borrows its path for the whole walk, so the paths outlive the roots.
        let paths: Vec<PathBuf> = specs
            .iter()
            .map(|(_, src, is_file)| {
                let src = PathBuf::from(src);
                if *is_file { src.parent().unwrap().to_path_buf() } else { src }
            })
            .collect();
        let roots = specs
            .iter()
            .zip(&paths)
            .map(|((name, _, is_file), path)| {
                let source =
                    prepare_source(fs, path, Path::new("/dst"), MountPolicy::Cross).unwrap();
                let name = OsString::from(name);
                if *is_file {
                    RootInput::File { name, src_parent: path.clone(), identity: source.identity }
                } else {
                    RootInput::Folder { name, src_root: path.clone(), source }
                }
            })
            .collect();
        let mut out = TreeOutcome::default();
        let mut got = Vec::new();
        let (done, result) =
            copy_roots_at(&base, RootsDest::Existing(&dest), false, roots, &mut out, &mut |f| {
                got.push(f)
            });
        RootsRun { done, result, out, got }
    }

    fn statuses(run: &RootsRun) -> Vec<RootStatus> {
        run.done.iter().map(|d| d.status).collect()
    }

    /// The index of the first and of the last recorded call that mentions `needle`.
    fn first_and_last(calls: &[String], needle: &str) -> (usize, usize) {
        let at: Vec<usize> =
            calls.iter().enumerate().filter(|(_, c)| c.contains(needle)).map(|(i, _)| i).collect();
        (*at.first().expect(needle), *at.last().expect(needle))
    }

    fn lock_lost() -> FsError {
        FsError::new(Code::TargetLockBusy, std::io::Error::other("lost"))
    }

    const A_B_F: [(&str, &str, bool); 3] =
        [("a", "/src/a", false), ("b", "/src/b", false), ("f", "/src/f", true)];

    #[test]
    fn roots_land_at_dest_name_for_two_folders_and_a_file() {
        let fs = sources(&[("a/x", b"1"), ("b/k", b"2"), ("b/y/z", b"4"), ("f", b"3")]);

        let run = roots_run(&fs, &A_B_F, &unguarded);

        run.result.as_ref().unwrap();
        assert!(run.got.is_empty(), "{:?}", run.got);
        for (path, bytes) in [("a/x", "1"), ("b/k", "2"), ("b/y/z", "4"), ("f", "3")] {
            assert_eq!(
                fs.read_file(Path::new("/dst").join(path)).as_deref(),
                Some(bytes.as_bytes()),
                "{path}"
            );
        }
        // `DEST/a`, `DEST/b` and `DEST/b/y`, each once; DEST itself is the caller's.
        assert_eq!(run.out.directories_created, 3);
        assert_eq!((run.out.files_total, run.out.files_copied), (4, 4));
        assert_eq!(statuses(&run), [RootStatus::Completed; 3]);
        let per_root: Vec<_> = run.done.iter().map(|d| (d.files_copied, d.files_failed)).collect();
        assert_eq!(per_root, [(1, 0), (2, 0), (1, 0)]);
    }

    #[test]
    fn a_file_root_is_keyed_by_its_name_alone() {
        let fs = sources(&[("f", b"3")]);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let slot = std::sync::Arc::clone(&seen);
        fs.on_nth("rename_no_replace", 1, move |fs| {
            let root = fs.destination_root(Path::new("/dst")).unwrap();
            let store = root.open_claim_store(OsStr::new("state.db"), Durability::Strict).unwrap();
            *slot.lock().unwrap() = Some(store.prepared().unwrap());
        });

        let run = roots_run_with(
            &fs,
            &[("f", "/src/f", true)],
            &unguarded,
            Durability::Strict,
            policy(64),
        );

        run.result.as_ref().unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let store = root.open_claim_store(OsStr::new("state.db"), Durability::Strict).unwrap();
        let identity = root.identity().unwrap();
        let FileIdentity::Strong(parent) = identity else { panic!("{identity:?}") };
        let claim = store
            .get(&ClaimKey::new(parent, OsStr::new("f")))
            .unwrap()
            .expect("the claim is stored under DEST and the file's name");
        assert_eq!(claim.target.0, key_of("f").0);
        let pending = seen.lock().unwrap().take().expect("the hook ran at the publish");
        assert_eq!(pending.len(), 1, "one note is pending at the rename: {pending:?}");
        assert_eq!(pending[0].1.target.0, key_of("f").0, "the prepared target is the name alone");
    }

    #[test]
    fn roots_are_copied_in_the_order_given() {
        let fs = sources(&[("a/x", b"1"), ("b/k", b"2"), ("b/y/z", b"4"), ("f", b"3")]);

        // Not the sorted order: the driver does not sort.
        let run = roots_run(
            &fs,
            &[("f", "/src/f", true), ("b", "/src/b", false), ("a", "/src/a", false)],
            &unguarded,
        );

        run.result.as_ref().unwrap();
        let calls = fs.calls();
        let (_, f_last) = first_and_last(&calls, "/dst/f");
        let (b_first, b_last) = first_and_last(&calls, "/dst/b");
        let (a_first, _) = first_and_last(&calls, "/dst/a");
        assert!(f_last < b_first, "f, then b: {calls:?}");
        assert!(b_last < a_first, "b, then a: {calls:?}");
        let names: Vec<_> = run.done.iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, ["f", "b", "a"].map(OsString::from));
    }

    #[test]
    fn a_failing_file_in_root_one_does_not_stop_root_two() {
        let fs = sources(&[("a/x", b"1"), ("b/k", b"2")]);
        fs.fail("create_new", Code::PermissionDenied);

        let run = roots_run(&fs, &[("a", "/src/a", false), ("b", "/src/b", false)], &unguarded);

        run.result.as_ref().unwrap();
        assert_eq!(statuses(&run), [RootStatus::Failed, RootStatus::Completed]);
        let failed: Vec<_> = run.done.iter().map(|d| (d.files_copied, d.files_failed)).collect();
        assert_eq!(failed, [(0, 1), (1, 0)]);
        assert_eq!(fs.read_file("/dst/b/k").as_deref(), Some(&b"2"[..]));
        assert_eq!(run.got.len(), 1, "{:?}", run.got);
        assert_eq!(run.got[0].path, Path::new("a/x"), "reported under its root's name");
    }

    #[test]
    fn an_abort_in_root_two_marks_root_three_not_started() {
        let fs = sources(&[("a/x", b"1"), ("b/k", b"2"), ("c/y", b"3")]);
        // Root two's directory is made, then the lock is lost: its first file meets it.
        let guard = || if fs.called("create_dir(/dst/b)") { Err(lock_lost()) } else { Ok(()) };

        let run = roots_run(
            &fs,
            &[("a", "/src/a", false), ("b", "/src/b", false), ("c", "/src/c", false)],
            &guard,
        );

        assert_eq!(run.result.as_ref().unwrap_err().code(), Code::TargetLockBusy);
        assert_eq!(
            statuses(&run),
            [RootStatus::Completed, RootStatus::Failed, RootStatus::NotStarted],
            "the root that triggers the abort is Failed"
        );
        assert!(!fs.exists("/dst/c"), "root three was not started: {:?}", fs.calls());
        assert!(fs.exists("/dst/a/x"), "roots finished before the abort stay on disk");
        assert_eq!(run.done[2].files_copied + run.done[2].files_failed, 0);
    }

    #[test]
    fn a_guard_that_fails_before_a_root_aborts_without_creating_it() {
        let fs = sources(&[("a/x", b"1"), ("b/k", b"2")]);
        let guard = || if fs.called("create_dir(/dst/a)") { Err(lock_lost()) } else { Ok(()) };

        let run = roots_run(&fs, &[("a", "/src/a", false), ("b", "/src/b", false)], &guard);

        assert_eq!(run.result.as_ref().unwrap_err().code(), Code::TargetLockBusy);
        // `a` is made and its file meets the lost lock; `b` is never begun, so its directory is never made.
        assert_eq!(statuses(&run), [RootStatus::Failed, RootStatus::NotStarted]);
        assert!(!fs.exists("/dst/b"));
    }

    #[test]
    fn counters_are_sums_over_roots() {
        let fs = sources(&[("a/x", b"1"), ("b/k", b"2"), ("b/y/z", b"4"), ("f", b"3")]);
        fs.fail("create_new", Code::PermissionDenied);

        let run = roots_run(&fs, &A_B_F, &unguarded);

        run.result.as_ref().unwrap();
        assert_eq!(
            statuses(&run),
            [RootStatus::Failed, RootStatus::Completed, RootStatus::Completed]
        );
        let out = &run.out;
        assert_eq!(out.files_total, 4);
        assert_eq!(out.files_copied, 3);
        assert_eq!(out.bytes_copied, 3);
        // `a` and `b` made by the loop, `b/y` by the walk.
        assert_eq!(out.directories_created, 3);
        assert_eq!((out.failures.copy, out.failures.total()), (1, 1));
        let copied: u64 = run.done.iter().map(|d| d.files_copied).sum();
        let failed: u64 = run.done.iter().map(|d| d.files_failed).sum();
        assert_eq!((copied, failed), (out.files_copied, out.failures.copy));
        assert_eq!(copied + failed, out.files_total, "every file is one or the other");
    }

    #[test]
    fn a_root_with_only_a_walk_or_create_dir_failure_is_failed() {
        // `a/sub` cannot be created (the second `create_dir`; the first is `DEST/a`): a `create_dir` failure, no
        // `copy` failure. A one-source run exits 1 on it, so the root is Failed, with no failed file.
        let fs = sources(&[("a/w", b"0"), ("a/sub/x", b"1"), ("b/k", b"2")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        // The count includes the fixture's own `create_dir` calls.
        let made =
            u32::try_from(fs.calls().iter().filter(|c| c.starts_with("create_dir(")).count());
        let second = made.unwrap() + 2;
        fs.fail_nth(
            "create_dir",
            second,
            Code::PermissionDenied,
            std::io::ErrorKind::PermissionDenied,
        );

        let run = roots_run(&fs, &[("a", "/src/a", false), ("b", "/src/b", false)], &unguarded);

        run.result.as_ref().unwrap();
        assert_eq!((run.out.failures.create_dir, run.out.failures.copy), (1, 0), "{:?}", run.got);
        assert_eq!(statuses(&run), [RootStatus::Failed, RootStatus::Completed]);
        assert_eq!(run.done[0].files_failed, 0, "files_failed stays the copy failures");
        assert_eq!(fs.read_file("/dst/b/k").as_deref(), Some(&b"2"[..]), "the next root ran");
    }

    #[test]
    fn a_per_file_failure_of_a_file_root_does_not_stop_the_loop() {
        // A per-file failure of a file root marks that root Failed and the loop goes on. The failure is made with a
        // file root named `.flux` over a `.flux` directory this test makes in DEST (standing for the workspace), which
        // fails it like a tree's top-level `.flux` file. (`reserved_path` needs two components, `.flux/operations/...`,
        // and a file root's path is one, so it cannot fire here; the helper still calls it, as a tree's file event does.)
        let fs = sources(&[(".flux", b"1"), ("g", b"2")]);
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/.flux")).unwrap();

        let run =
            roots_run(&fs, &[(".flux", "/src/.flux", true), ("g", "/src/g", true)], &unguarded);

        run.result.as_ref().unwrap();
        assert_eq!(statuses(&run), [RootStatus::Failed, RootStatus::Completed]);
        assert_eq!((run.out.failures.copy, run.out.files_copied), (1, 1), "{:?}", run.got);
        assert_eq!(run.got[0].path, Path::new(".flux"));
        assert!(fs.exists("/dst/.flux") && fs.exists("/dst/g"));
    }

    #[test]
    fn a_dropped_stop_with_a_leftover_reports_a_root_relative_path_under_a_prefix() {
        // `a_lost_lock_during_the_drain_keeps_the_remaining_frames_without_a_barrier`, walked under the prefix `p`:
        // the drain's flush stops after the walk failed, the stop is dropped, and its leftover (relative to DEST,
        // `p/...`) is reported. `TreeFailure.path` stays relative to the root, so a caller that prefixes the root's
        // name does not prefix twice.
        let fs = two_pending_frames_then_an_ordinary_abort();
        let calls = std::cell::Cell::new(0);
        let guard = || {
            calls.set(calls.get() + 1);
            match calls.get() {
                1..=7 => Ok(()),
                8 => Err(FsError::new(Code::IoError, std::io::Error::other("walk"))),
                _ => Err(lock_lost()),
            }
        };

        let prefix = OsString::from("p");
        let (_, _, got) = batched_keyed(
            &fs,
            policy(64),
            Durability::Strict,
            &guard,
            &no_heartbeat,
            Some(prefix.as_os_str()),
        );

        let mut checked = 0;
        for g in &got {
            if let TreeFailureCause::Copy(e) = &g.cause
                && let Some((leftover, _)) = &e.leftover
            {
                assert!(
                    leftover.starts_with("p"),
                    "the leftover is relative to DEST: {leftover:?}"
                );
                assert!(!g.path.starts_with("p"), "TreeFailure.path is root-relative: {g:?}");
                // The dropped stop is reported AT its leftover's path (a temporary's name); the others at their entry's.
                if g.path.to_string_lossy().contains("flux-partial") {
                    assert_eq!(g.path, leftover.strip_prefix("p").unwrap());
                    checked += 1;
                }
            }
        }
        assert!(checked >= 1, "{got:?}");
    }
}
