//! What `flux copy` prints (cut 5): one stderr line per streamed record, the
//! aggregated warnings, a summary line, and the §53 JSON report. Pure: every function
//! returns text, and `main` writes it.

use flux_core::copy::CopyError;
use flux_core::dry_run::{PlannedAction, PlannedEntry};
use flux_core::run::{ResumeNote, Run, RunError, RunWarning};
use flux_core::{
    Discard, TreeAbort, TreeFailure, TreeFailureCause, TreeOutcome, WeakIdentityWarnings,
};
use flux_fs::{Code, FileIdentity, MetadataFailure, MetadataItem, Outcome};
use serde::Serialize;
use std::path::Path;

/// Why a symlink given as SOURCE is refused (K7). A symlink inside a tree is copied as a link (cut 10a).
pub const SYMLINK_WHY: &str =
    "a symlink SOURCE is not followed; copy its parent folder, or name its target";

/// §53's complete field list, in its order. Every value is true for what this version
/// does: nothing hardlinks, reflinks or verifies, so those stay 0 and `verify_level`
/// is `"none"`.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Report {
    pub files_total: u64,
    pub files_copied: u64,
    pub files_skipped: u64,
    pub files_overwritten: u64,
    pub files_hardlinked: u64,
    pub files_reflinked: u64,
    pub files_degraded: u64,
    pub files_verified: u64,
    pub files_mismatched: u64,
    pub files_failed: u64,
    pub bytes_total: u64,
    pub bytes_copied: u64,
    pub bytes_skipped: u64,
    pub errors: u64,
    pub duration_ms: u64,
    pub average_bytes_per_second: u64,
    pub verify_level: &'static str,
    pub hash_algorithm: &'static str,
    /// Cut 9f-2: `"full"` when `--resume-verify=full` re-read a partial; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_verify: Option<&'static str>,
    /// Cut 10b: source mount roots created as empty directories and not descended into; absent at zero.
    #[serde(skip_serializing_if = "is_zero")]
    pub mounts_skipped: u64,
    /// Cut 11a: `true` for a `--dry-run`; absent otherwise.
    #[serde(skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// Cut 11a: checks a dry run could not perform; absent when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unprobed: Vec<&'static str>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Report {
    fn zero() -> Self {
        Report {
            files_total: 0,
            files_copied: 0,
            files_skipped: 0,
            files_overwritten: 0,
            files_hardlinked: 0,
            files_reflinked: 0,
            files_degraded: 0,
            files_verified: 0,
            files_mismatched: 0,
            files_failed: 0,
            bytes_total: 0,
            bytes_copied: 0,
            bytes_skipped: 0,
            errors: 0,
            duration_ms: 0,
            average_bytes_per_second: 0,
            verify_level: "none",
            hash_algorithm: "blake3",
            resume_verify: None,
            mounts_skipped: 0,
            dry_run: false,
            unprobed: Vec::new(),
        }
    }

    /// A tree copy, finished or aborted. `bytes_total` is `bytes_copied`: the walk never
    /// stats a file, so a failed file's size is unknown (owner, cut 5). A skipped special
    /// file is a skipped target (§233.1 `action=skipped`).
    pub fn tree(out: &TreeOutcome, aborted: bool, duration_ms: u64) -> Self {
        let mut r = Self::zero();
        r.files_total = out.files_total;
        r.files_copied = out.files_copied;
        r.files_skipped = out.special_files_skipped
            + out.symlinks_skipped
            + out.files_skipped
            + out.files_resumed;
        r.mounts_skipped = out.mounts_skipped;
        r.files_overwritten = out.files_overwritten;
        r.bytes_skipped = out.bytes_skipped + out.bytes_resumed;
        r.files_degraded = out.files_degraded;
        // Only `copy` failures are failed FILES (a failed link is one, cut 10a). A `ClaimNotRecorded` failure is
        // counted in `FailureTally::total()` (so in `errors`) and in `claim_not_recorded`, but not here: the file was
        // published and counted as copied.
        r.files_failed = out.failures.copy;
        r.bytes_total = out.bytes_copied;
        r.bytes_copied = out.bytes_copied;
        r.errors = out.failures.total() + u64::from(aborted);
        r.timed(duration_ms)
    }

    /// A single-file copy. `target_existed` was read once at resolution and only feeds
    /// `files_overwritten`.
    pub fn file(r: &Result<Outcome, CopyError>, target_existed: bool, duration_ms: u64) -> Self {
        let mut rep = Self::zero();
        rep.files_total = 1;
        match r {
            Ok(o) => {
                rep.files_skipped = u64::from(o.skipped);
                rep.files_copied = u64::from(!o.skipped);
                rep.files_overwritten = u64::from(target_existed && !o.skipped);
                // `bytes_skipped` stays 0: the engine does not stat the source separately for a single file.
                rep.files_degraded = u64::from(o.identity_degraded.is_some());
                rep.bytes_total = o.bytes_copied;
                rep.bytes_copied = o.bytes_copied;
                rep.errors = u64::from(!o.metadata_failures.is_empty());
            }
            Err(_) => {
                rep.files_failed = 1;
                rep.errors = 1;
            }
        }
        rep.timed(duration_ms)
    }

    /// A failure before the engine ran. A symlink SOURCE (K7) is one known entry that
    /// failed; a missing SOURCE or a resolution failure counts nothing.
    pub fn pre_engine(symlink_source: bool) -> Self {
        let mut r = Self::zero();
        r.errors = 1;
        if symlink_source {
            r.files_total = 1;
            r.files_failed = 1;
        }
        r
    }

    /// A tree copy under the destination's lock (cut 7a, Part 3b-2 decision 1): §53's fields only. The copy's counts,
    /// plus one error for a stop of the run outside the copy; a run that stopped before its copy is that one error.
    pub fn tree_run(run: &Run<Result<TreeOutcome, TreeAbort>>, duration_ms: u64) -> Self {
        let mut rep = match &run.copy {
            Some(Ok(out)) => Self::tree(out, false, duration_ms),
            Some(Err(a)) => Self::tree(&a.outcome, true, duration_ms),
            None => return Self::pre_engine(false),
        };
        rep.errors += u64::from(run.stop.is_some());
        rep
    }

    /// `tree_run`, for a single file.
    pub fn file_run(
        run: &Run<Result<Outcome, CopyError>>,
        target_existed: bool,
        duration_ms: u64,
    ) -> Self {
        let mut rep = match &run.copy {
            Some(result) => Self::file(result, target_existed, duration_ms),
            None => return Self::pre_engine(false),
        };
        rep.errors += u64::from(run.stop.is_some());
        rep.resume_verify = run.file_resume.as_ref().map(|_| "full");
        rep
    }

    fn timed(mut self, duration_ms: u64) -> Self {
        self.duration_ms = duration_ms;
        self.average_bytes_per_second = self.bytes_copied.saturating_mul(1000) / duration_ms.max(1);
        self
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self)
            .expect("a flat struct of integers and static strings always serializes")
    }
}

/// A tree-relative path with `/` separators (§233.1); the root itself is `.`.
pub fn rel(path: &Path) -> String {
    let parts: Vec<_> = path.components().map(|c| c.as_os_str().to_string_lossy()).collect();
    if parts.is_empty() { ".".to_owned() } else { parts.join("/") }
}

/// The stderr lines for one streamed record: one line, except a file published with
/// several metadata complaints, which gets one per complaint.
pub fn record_lines(f: &TreeFailure) -> Vec<String> {
    let p = rel(&f.path);
    match &f.cause {
        TreeFailureCause::Walk(e) | TreeFailureCause::CreateDir(e) => {
            vec![format!("{}: {p}: {}", e.code.as_str(), e.source)]
        }
        TreeFailureCause::Copy(e) => vec![copy_line(&p, e)],
        TreeFailureCause::SpecialFileSkipped => vec![format!(
            "{}: {p}: object_type=special action=skipped",
            Code::SpecialFileUnsupported.as_str()
        )],
        // Cut 10a (D11): neither an error nor a warning, so no code; §124 names the fields.
        TreeFailureCause::SymlinkSkipped => {
            vec![format!("{p}: object_type=symlink action=skipped (--links=skip)")]
        }
        TreeFailureCause::MountSkipped => vec![format!(
            "{p}: object_type=mount action=skipped (pass --cross-filesystems to copy its contents)"
        )],
        TreeFailureCause::PublishedWithComplaints(v) => {
            v.iter().map(|m| complaint_line(&p, m)).collect()
        }
        TreeFailureCause::ClaimNotRecorded(e) => vec![format!(
            "{}: {p}: {}; the file was published but its claim was not recorded",
            e.code.as_str(),
            e.source
        )],
        // Cut 11a: neither an error nor a warning, so no code.
        TreeFailureCause::Planned(pe) => match pe.action {
            PlannedAction::Skip(r) => vec![format!(
                "{p}: object_type={} action=would_skip ({})",
                pe.object.as_str(),
                r.as_str()
            )],
            a => vec![format!("{p}: object_type={} action={}", pe.object.as_str(), a.as_str())],
        },
    }
}

/// A failed copy in a tree: its code, the path, the cause, and a leftover temporary
/// when one could not be removed - the path the user needs to clean up.
fn copy_line(path: &str, e: &CopyError) -> String {
    let mut s = format!("{}: {path}: {}", e.code().as_str(), e.cause.source);
    if let Some((left, why)) = &e.leftover {
        s.push_str(&format!("; temporary left at {} ({why})", rel(left)));
    }
    s
}

/// One metadata item that could not be applied to a file that IS published (§44.1).
pub fn complaint_line(path: &str, m: &MetadataFailure) -> String {
    let item = match m.item {
        MetadataItem::Times => "times",
        MetadataItem::Permissions => "permissions",
    };
    format!("{}: {path}: {item}: {}", Code::MetadataApplyFailed.as_str(), m.error.source)
}

/// One line per weak volume, then one for the `Unavailable` bucket (walker design:
/// warn once per filesystem). Empty when nothing degraded.
pub fn warning_lines(w: &WeakIdentityWarnings) -> Vec<String> {
    let mut v: Vec<String> = w
        .weak
        .iter()
        .map(|(volume, g)| weak_line(&format!("on volume {volume:#x}"), g.count, &rel(&g.example)))
        .collect();
    if let Some(g) = &w.unavailable {
        v.push(weak_line(UNAVAILABLE, g.count, &rel(&g.example)));
    }
    v
}

/// Cut 8a's two safety warnings: the mount-root checks that could not tell, then the containment check that fell
/// back to the lexical floor. Empty when neither degraded.
pub fn safety_warning_lines(out: &TreeOutcome) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(g) = &out.mount_unknown {
        v.push(format!(
            "warning: could not tell whether a pre-existing destination directory is a mount root ({} checks, e.g. {}); merged anyway - --safety=strict refuses instead",
            g.count,
            rel(&g.example)
        ));
    }
    if let Some(g) = &out.source_mount_unknown {
        v.push(format!(
            "warning: could not tell whether a source directory is a mount root ({} checks, e.g. {}); descended into it - --safety=strict refuses instead",
            g.count,
            rel(&g.example)
        ));
    }
    if let Some(path) = &out.containment_degraded {
        v.push(format!(
            "warning: the location of {} could not be resolved from its handle; containment was checked lexically only - --safety=strict refuses instead",
            path.display()
        ));
    }
    if let Some(g) = &out.replace_degraded {
        v.push(format!(
            "warning: could not key claims in {} directories (e.g. {}); existing files there are reported as collisions - --safety=strict skips them instead",
            g.count,
            rel(&g.example)
        ));
    }
    v
}

/// Every tree warning in print order: the identity ones (`warning_lines`), then `safety_warning_lines`.
pub fn tree_warning_lines(out: &TreeOutcome) -> Vec<String> {
    let mut v = warning_lines(&out.warnings);
    v.extend(safety_warning_lines(out));
    v
}

/// The single-file form. `degraded` is the weaker side; `target` is the destination
/// after §4.1 mapping. `None` for `Strong`, which is not a degradation.
pub fn file_warning(degraded: &FileIdentity, target: &Path) -> Option<String> {
    let scope = match degraded {
        FileIdentity::Strong(_) => return None,
        FileIdentity::Weak(id) => format!("on volume {:#x}", id.volume),
        FileIdentity::Unavailable => UNAVAILABLE.to_owned(),
    };
    Some(weak_line(&scope, 1, &target.display().to_string()))
}

const UNAVAILABLE: &str = "where the filesystem reports no identity";

fn weak_line(scope: &str, count: u64, example: &str) -> String {
    format!(
        "warning: identity could not be compared with full confidence {scope} ({count} checks, e.g. {example}); copied anyway - --safety=strict refuses instead"
    )
}

/// The last stderr line of a run that reached the engine. Its failure figure is the
/// JSON `errors` value, so an abort counts and a non-zero exit never reads as clean.
pub fn summary_line(r: &Report, directories_created: u64) -> String {
    format!(
        "copied {} files ({} bytes), created {directories_created} directories; {} failed, {} skipped",
        r.files_copied, r.bytes_copied, r.errors, r.files_skipped
    )
}

/// The last stderr line of a `--dry-run` that reached the engine: what a real run would do.
pub fn dry_run_summary_line(r: &Report, directories_created: u64) -> String {
    format!(
        "dry run: would copy {} files ({} bytes), overwrite {}, skip {}, create {directories_created} directories; {} errors",
        r.files_copied, r.bytes_copied, r.files_overwritten, r.files_skipped, r.errors
    )
}

/// A tree dry run reached the walk without probing the no-replace publication primitive (section 3.6).
pub const UNPROBED_NOREPLACE_LINE: &str = "note: the no-replace publication primitive was not probed under --dry-run; a real run may be refused with NOREPLACE_PUBLISH_UNAVAILABLE";

/// A dry run took no lock and wrote no state, so the refusals and failures those would meet are unchecked (master spec
/// 5.2, ruling P10). Printed for a tree and a single file alike.
pub const UNPROBED_TARGET_LOCK_LINE: &str = "note: the destination's lock was not taken and no operation state was written under --dry-run; a real run may be refused or fail there";

/// The `unprobed` vocabulary of a dry run that reached the engine's dry branch, in its fixed order: the no-replace
/// publication primitive (tree only), the target lock (always), and a prior operation seen at DEST (the notes already
/// print, so it has no line of its own).
pub fn unprobed_keys(d: &flux_core::dry_run::DryRun) -> Vec<&'static str> {
    let mut v = Vec::new();
    if d.unprobed_noreplace {
        v.push("noreplace_publish");
    }
    v.push("target_lock");
    if d.prior_seen {
        v.push("prior_operation");
    }
    v
}

/// The stderr lines of the `unprobed` keys, in the same order; `prior_operation` has none.
pub fn unprobed_lines(keys: &[&'static str]) -> Vec<&'static str> {
    keys.iter()
        .filter_map(|k| match *k {
            "noreplace_publish" => Some(UNPROBED_NOREPLACE_LINE),
            "target_lock" => Some(UNPROBED_TARGET_LOCK_LINE),
            _ => None,
        })
        .collect()
}

/// The single-file counterpart of a `Planned` record's line: `target` in place of the tree-relative path.
pub fn planned_file_line(target: &str, p: PlannedEntry) -> String {
    match p.action {
        PlannedAction::Skip(r) => {
            format!(
                "{target}: object_type={} action=would_skip ({})",
                p.object.as_str(),
                r.as_str()
            )
        }
        a => format!("{target}: object_type={} action={}", p.object.as_str(), a.as_str()),
    }
}

/// A run's stop (cut 7a), as stderr lines. A refusal is `CODE: detail` - the detail names the path and what to do next
/// (the refusal-guidance table) - then one indented line per field of the holder's record where it was readable
/// (§96.2), then what the run created and could not remove again. A failure of the run's own step is
/// `CODE: <step> at <path>: <error>`.
pub fn stop_lines(e: &RunError) -> Vec<String> {
    match e {
        RunError::Refused { refusal, not_removed, .. } => {
            let mut v = vec![format!("{}: {}", refusal.code.as_str(), refusal.detail)];
            if let Some(h) = &refusal.holder {
                v.push(format!("  holder owner_instance_id: {}", h.owner_instance_id));
                v.push(format!("  holder boot_session_id: {}", h.boot_session_id));
                v.push(format!(
                    "  holder last_heartbeat_wall_time: {}",
                    h.last_heartbeat_wall_time
                ));
                v.push(format!("  holder workspace_path: {}", h.workspace_path));
            }
            if let Some((path, why)) = not_removed {
                v.push(format!("  not removed: {} ({})", path.display(), why.source));
            }
            v
        }
        RunError::Failed { step, path, error, not_removed } => {
            let mut v = vec![format!(
                "{}: {} at {}: {}",
                error.code.as_str(),
                step.as_str(),
                path.display(),
                error.source
            )];
            if let Some((path, why)) = not_removed {
                v.push(format!("  not removed: {} ({})", path.display(), why.source));
            }
            v
        }
    }
}

/// One of a run's warnings (F6: never a failure), as one stderr line.
pub fn run_warning_line(w: &RunWarning) -> String {
    match w {
        RunWarning::BrokenLeftover(p) => format!(
            "warning: a dead run's lock was moved aside to {} and could not be deleted; remove it when no Flux run is active",
            p.display()
        ),
        RunWarning::NotRemoved { path, error } => format!(
            "warning: could not remove {} ({}); the copy itself is complete",
            path.display(),
            error.source
        ),
        RunWarning::StateKept(p) => format!(
            "warning: a temporary this run could not remove keeps its state at {}, the record that names it",
            p.display()
        ),
        RunWarning::PartialKept { path, error, kept } => format!(
            "warning: could not delete the superseded partial {} ({}); its operation's state stays at {}",
            path.display(),
            error.source,
            kept.display()
        ),
        RunWarning::OwnershipLostAfterCompletion(p) => format!(
            "warning: the lock {} was taken over after this copy completed; it was left in place",
            p.display()
        ),
        RunWarning::ProbeNotRemoved { path, error } => format!(
            "warning: the no-replace probe {} could not be removed ({}); it goes when the operation's state is removed",
            path.display(),
            error.source
        ),
        RunWarning::ResumeMappingByPath(p) => format!(
            "warning: the source root's identity could not confirm the mapping, so {} was matched by its path alone",
            p.display()
        ),
        RunWarning::PriorCleaned { operation_id, removed } => format!(
            "note: finished cleaning up the earlier operation {operation_id} ({removed} leftovers removed)"
        ),
        RunWarning::PriorCleanupFailed { operation_id, path, error } => format!(
            "warning: could not finish the cleanup of the earlier operation {operation_id}: {}: {}",
            path.display(),
            error.source
        ),
        RunWarning::CheckpointsUnavailable { why } => {
            format!("checkpoints unavailable on this filesystem: {why}; every large file recopies")
        }
        RunWarning::CheckpointsUnreadable { why } => {
            format!("checkpoints unreadable: {why}; every large file recopies")
        }
        RunWarning::WalQuarantined { code, why, set_aside_as } => format!(
            "{code}: the checkpoint log is damaged ({why}); possible storage damage, investigate; it was set aside as wal/{set_aside_as} and a fresh log was started; every large file recopies"
        ),
        RunWarning::WalNotQuarantined { code, why, error } => format!(
            "{code}: the checkpoint log is damaged ({why}) and could not be set aside ({error}); this run copies with no checkpoints"
        ),
        RunWarning::WalTailDiscarded { offset, len, reason } => format!(
            "WAL tail cut: {len} bytes at offset {offset} ({reason}); the checkpoints before it are intact"
        ),
        RunWarning::DryRunPrior { operation_id, state } => format!(
            "note: a prior operation {operation_id} in state {} exists at DEST; --dry-run does not inspect its owner, so what a real run does with it (it may be refused with RESUMABLE_OPERATION_EXISTS) is not previewed",
            state.as_str()
        ),
        RunWarning::DryRunPriorCompleted { operation_id } => format!(
            "note: an earlier completed operation {operation_id} exists at DEST; a real run would finish its recorded cleanup"
        ),
        RunWarning::DryRunPriorUnreadable { detail } => format!(
            "note: a prior operation's record could not be read ({detail}); --dry-run does not preview how a real run handles it"
        ),
        RunWarning::DryRunLockRecord {
            operation_id,
            owner_instance_id,
            boot_session_id,
            heartbeat_age_secs,
            workspace_path,
        } => {
            let age = match heartbeat_age_secs {
                Some(n) => format!("{n} s ago"),
                None => "in the future".to_string(),
            };
            format!(
                "note: a lock record for operation {operation_id} exists (owner instance {owner_instance_id}, boot session {boot_session_id}, last heartbeat {age}, workspace {workspace_path}); a real run tries the OS lock: if it is held, TARGET_LOCK_BUSY; if not, the preview below applies, and this exit code reflects that case"
            )
        }
        RunWarning::DryRunLockTakeover { why } => format!(
            "note: --break-lock would take over the lock, whose record is unreadable ({why}); a real run refuses with TARGET_LOCK_BUSY if the OS lock is held"
        ),
        RunWarning::DryRunBrokenLeftover { name } => {
            format!("note: an interrupted lock recovery left {name}; a real run classifies it")
        }
        RunWarning::DryRunDebris { name } => {
            format!("note: a real run would remove the workspace debris {name}")
        }
        RunWarning::DryRunStoreFresh { operation_id } => {
            format!("note: would create a fresh claim store for operation {operation_id}")
        }
        RunWarning::DryRunStoreTorn => "note: the prior's claim store could not be read consistently; a live run may have been writing it: run the dry run again before deciding on --restart".to_string(),
    }
}

/// A run's own stop, then its warnings (cut 7a): the stderr lines that follow the copy's and precede the summary.
pub fn run_lines<T>(run: &Run<T>) -> Vec<String> {
    let mut v = run.stop.as_ref().map(stop_lines).unwrap_or_default();
    v.extend(run.warnings.iter().map(run_warning_line));
    v
}

/// Cut 9a: the run's resume note, then (cut 9c) `recovered <k> interrupted publications` when `k > 0`, then
/// `resumed <n> already-complete files` when `files_resumed > 0`.
pub fn resume_lines<T>(run: &Run<T>, files_resumed: u64) -> Vec<String> {
    let mut v = Vec::new();
    match &run.resumed {
        None => {}
        Some(ResumeNote::StartedNew) => v.push("no prior operation; starting new".to_string()),
        Some(ResumeNote::Adopted { operation_id, claims, recovered }) => {
            v.push(match claims {
                Some(n) => format!("resuming operation {operation_id} ({n} entries claimed)"),
                None => format!("resuming operation {operation_id}"),
            });
            // Cut 9c: the interrupted publications commit recovery found published.
            if *recovered > 0 {
                v.push(format!("recovered {recovered} interrupted publications"));
            }
        }
    }
    if files_resumed > 0 {
        v.push(format!("resumed {files_resumed} already-complete files"));
    }
    v
}

/// One wording for why checkpoints were discarded, shared by the tree and single-file builders.
fn discard_why(reason: &Discard) -> String {
    match reason {
        Discard::SourceChanged(what) => format!("source changed ({what})"),
        Discard::PartialMissing => "partial missing".to_string(),
        Discard::MismatchAt(at) => format!("mismatch at offset {at}"),
        Discard::PartialShorter { kept } => {
            format!("partial shorter than its checkpoints (kept {kept} bytes)")
        }
        Discard::Unreadable(why) => format!("checkpoints unreadable: {why}"),
    }
}

/// The `--durability=normal` closing note, printed after a `resumed` or `checkpoints discarded` line.
const ADVISORY_NOTE: &str = "note: --durability=normal checkpoints are advisory; every reused chunk was validated against the source and the destination";

/// The level line text of `--resume-verify=full`.
const FULL_LEVEL_LINE: &str = "resume verification: full (source and partial re-read and hashed)";

/// Cut 9f-1 (spec decision 12): what a checkpointed run reused and discarded. One `resumed` line per partially resumed
/// file, one `checkpoints discarded` line per file whose checkpoints were discarded, and, under Normal durability and
/// only when something was reused or discarded, a closing note. An uncheckpointed run prints nothing.
pub fn checkpoint_lines(outcome: &TreeOutcome, normal_durability: bool) -> Vec<String> {
    let mut v = Vec::new();
    for r in &outcome.partial_resumes {
        v.push(format!(
            "resumed {}: reused {} bytes, recopied {} bytes",
            rel(&r.path),
            r.reused,
            r.recopied
        ));
    }
    for d in &outcome.checkpoint_discards {
        let why = discard_why(&d.reason);
        v.push(format!("checkpoints discarded for {}: {why}", rel(&d.path)));
    }
    if normal_durability && !v.is_empty() {
        v.push(ADVISORY_NOTE.to_string());
    }
    v
}

/// Cut 9g: `checkpoint_lines` for a single file's resume; `target` is the path as the user gave it.
pub fn file_checkpoint_lines(
    run: &Run<Result<Outcome, CopyError>>,
    target: &str,
    normal_durability: bool,
) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(fr) = &run.file_resume {
        if fr.reused > 0 {
            v.push(format!(
                "resumed {target}: reused {} bytes, recopied {} bytes",
                fr.reused, fr.recopied
            ));
        }
        if let Some(d) = &fr.discard {
            v.push(format!("checkpoints discarded for {target}: {}", discard_why(d)));
        }
    }
    if normal_durability && !v.is_empty() {
        v.push(ADVISORY_NOTE.to_string());
    }
    v
}

/// Cut 9g: the level line of a single-file resume, printed iff a log-free proof ran.
pub fn file_resume_verify_lines(run: &Run<Result<Outcome, CopyError>>) -> Vec<String> {
    if run.file_resume.is_some() { vec![FULL_LEVEL_LINE.to_string()] } else { Vec::new() }
}

/// Cut 9f-2: whether `--resume-verify=full` did something a user can see: it re-read a partial that was then resumed or
/// whose checkpoints were discarded. Never under `chunks`.
pub fn resume_verify_used(outcome: &TreeOutcome, full: bool) -> bool {
    full && (!outcome.partial_resumes.is_empty() || !outcome.checkpoint_discards.is_empty())
}

/// Cut 9f-2: the level line, printed after `checkpoint_lines` when `resume_verify_used`.
pub fn resume_verify_lines(outcome: &TreeOutcome, full: bool) -> Vec<String> {
    if resume_verify_used(outcome, full) { vec![FULL_LEVEL_LINE.to_string()] } else { Vec::new() }
}

/// Cut 9f-1: the line that follows an aborting error when it is a checkpoint-write failure.
pub fn abort_hint_lines(error: &CopyError) -> Vec<String> {
    if error.code() == Code::WalWriteFailed {
        vec!["run again with --resume to continue from the last durable checkpoint".to_string()]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::DegradedGroup;
    use flux_core::copy::CopyStep;
    use flux_core::lock::{LockCode, Refusal};
    use flux_core::run::{RunError, RunStep, RunWarning};
    use flux_fs::{FsError, ObjectId};
    use std::path::PathBuf;

    /// §53's complete field list, in its order.
    const KEYS: [&str; 18] = [
        "files_total",
        "files_copied",
        "files_skipped",
        "files_overwritten",
        "files_hardlinked",
        "files_reflinked",
        "files_degraded",
        "files_verified",
        "files_mismatched",
        "files_failed",
        "bytes_total",
        "bytes_copied",
        "bytes_skipped",
        "errors",
        "duration_ms",
        "average_bytes_per_second",
        "verify_level",
        "hash_algorithm",
    ];

    fn io(msg: &str) -> std::io::Error {
        std::io::Error::other(msg.to_owned())
    }

    #[test]
    fn the_json_has_every_section_53_field_in_order() {
        let s = Report::pre_engine(false).to_json();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v.as_object().unwrap().len(), KEYS.len(), "{s}");
        let at: Vec<usize> = KEYS.iter().map(|k| s.find(&format!("\"{k}\":")).unwrap()).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "out of order: {s}");
        assert_eq!(v["verify_level"], "none");
        assert_eq!(v["hash_algorithm"], "blake3");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn a_tree_report_is_true_to_the_outcome() {
        let mut out = TreeOutcome::default();
        out.files_total = 7;
        out.files_copied = 2;
        out.bytes_copied = 30;
        out.special_files_skipped = 1;
        out.files_degraded = 1;
        out.failures.copy = 2;
        out.failures.walk = 1;
        out.failures.published_with_complaints = 1;

        let r = Report::tree(&out, false, 10);

        assert_eq!((r.files_total, r.files_copied, r.files_skipped), (7, 2, 1));
        assert_eq!(r.files_degraded, 1);
        assert_eq!(r.files_failed, 2, "copy only: walk and complaints are not failed files");
        assert_eq!((r.bytes_total, r.bytes_copied), (30, 30));
        assert_eq!(r.errors, 4);
        assert_eq!((r.duration_ms, r.average_bytes_per_second), (10, 3000));
        assert_eq!(
            (r.files_overwritten, r.files_hardlinked, r.files_reflinked, r.files_verified),
            (0, 0, 0, 0)
        );
        assert_eq!(Report::tree(&out, true, 10).errors, 5, "an abort counts as one");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn files_skipped_adds_skipped_links() {
        let mut out = TreeOutcome::default();
        out.symlinks_skipped = 2;
        out.special_files_skipped = 3;
        out.files_skipped = 5;
        out.files_resumed = 7;
        let r = Report::tree(&out, false, 10);
        assert_eq!(r.files_skipped, 2 + 3 + 5 + 7);
        assert_eq!((r.files_failed, r.errors), (0, 0), "a skipped link is not a failure");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn mounts_skipped_is_its_own_count_and_absent_from_json_at_zero() {
        let mut out = TreeOutcome::default();
        let before = Report::tree(&out, false, 10).files_skipped;
        out.mounts_skipped = 2;
        let r = Report::tree(&out, false, 10);
        assert_eq!(r.mounts_skipped, 2);
        assert_eq!(r.files_skipped, before, "a skipped mount is not a skipped file");
        assert_eq!(serde_json::to_value(&r).unwrap()["mounts_skipped"], 2);

        let zero = serde_json::to_value(Report::tree(&TreeOutcome::default(), false, 0)).unwrap();
        let obj = zero.as_object().unwrap();
        assert_eq!(obj.len(), 18);
        assert!(!obj.contains_key("mounts_skipped"));
    }

    #[test]
    fn a_claim_not_recorded_failure_is_not_a_failed_file() {
        let mut out = TreeOutcome::default();
        out.failures.claim_not_recorded = 1;
        let r = Report::tree(&out, false, 10);
        assert_eq!(r.files_failed, 0);
        assert_eq!(r.errors, 1);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn a_tree_report_counts_skipped_overwritten_and_skipped_bytes() {
        let mut out = TreeOutcome::default();
        out.special_files_skipped = 1;
        out.files_skipped = 3;
        out.files_overwritten = 2;
        out.bytes_skipped = 99;
        let r = Report::tree(&out, false, 10);
        assert_eq!(r.files_skipped, 4, "special files plus skipped files");
        assert_eq!(r.files_overwritten, 2);
        assert_eq!(r.bytes_skipped, 99);
    }

    #[test]
    fn a_single_file_report() {
        let ok: Result<Outcome, CopyError> = Ok(Outcome {
            bytes_copied: 5,
            metadata_failures: Vec::new(),
            identity_degraded: Some(FileIdentity::Unavailable),
            published_identity: flux_fs::FileIdentity::Unavailable,
            skipped: false,
        });
        let r = Report::file(&ok, true, 0);
        assert_eq!((r.files_total, r.files_copied, r.files_overwritten), (1, 1, 1));
        assert_eq!((r.files_degraded, r.bytes_total, r.errors, r.files_failed), (1, 5, 0, 0));
        assert_eq!(r.average_bytes_per_second, 5000, "a 0 ms run divides by 1");

        let bad: Result<Outcome, CopyError> = Err(CopyError {
            cause: FsError::new(Code::IoError, io("x")),
            leftover: None,
            step: CopyStep::Stream,
        });
        let r = Report::file(&bad, true, 0);
        assert_eq!((r.files_total, r.files_copied, r.files_overwritten), (1, 0, 0));
        assert_eq!((r.files_failed, r.errors), (1, 1));
    }

    #[test]
    fn a_single_file_onto_a_fresh_target_overwrites_nothing() {
        let ok: Result<Outcome, CopyError> = Ok(Outcome {
            bytes_copied: 5,
            metadata_failures: Vec::new(),
            identity_degraded: None,
            published_identity: flux_fs::FileIdentity::Unavailable,
            skipped: false,
        });
        let r = Report::file(&ok, false, 0);
        assert_eq!((r.files_copied, r.files_overwritten), (1, 0));
    }

    #[test]
    fn a_skipped_single_file_reports_one_skipped_and_none_copied() {
        let skipped: Result<Outcome, CopyError> = Ok(Outcome {
            skipped: true,
            bytes_copied: 0,
            metadata_failures: Vec::new(),
            identity_degraded: None,
            published_identity: flux_fs::FileIdentity::Unavailable,
        });
        let r = Report::file(&skipped, true, 0);
        assert_eq!((r.files_total, r.files_skipped, r.files_copied), (1, 1, 0));
        assert_eq!((r.files_overwritten, r.files_failed, r.errors), (0, 0, 0));
        assert_eq!(r.bytes_skipped, 0);
    }

    #[test]
    fn a_pre_engine_report() {
        let r = Report::pre_engine(false);
        assert_eq!((r.files_total, r.files_failed, r.errors), (0, 0, 1));
        let r = Report::pre_engine(true);
        assert_eq!((r.files_total, r.files_failed, r.errors), (1, 1, 1));
    }

    #[test]
    fn paths_are_rendered_with_forward_slashes_and_the_root_as_a_dot() {
        assert_eq!(rel(&Path::new("sub").join("b")), "sub/b");
        assert_eq!(rel(Path::new("")), ".");
    }

    #[test]
    fn a_planned_record_renders_its_object_and_action() {
        use flux_core::dry_run::{PlannedAction::*, PlannedEntry, PlannedObject::*, SkipReason::*};
        let line = |path: &str, (object, action)| {
            record_lines(&TreeFailure {
                path: PathBuf::from(path),
                cause: TreeFailureCause::Planned(PlannedEntry { object, action }),
            })
        };
        assert_eq!(line("sub/a", (File, Copy)), vec!["sub/a: object_type=file action=would_copy"]);
        assert_eq!(line("d", (Dir, CreateDir)), vec!["d: object_type=dir action=would_create"]);
        assert_eq!(
            line("l", (Symlink, CreateSymlink)),
            vec!["l: object_type=symlink action=would_create_symlink"]
        );
        assert_eq!(
            line("a", (File, Overwrite)),
            vec!["a: object_type=file action=would_overwrite"]
        );
        assert_eq!(
            line("a", (File, Skip(Existing))),
            vec!["a: object_type=file action=would_skip (existing)"]
        );
        assert_eq!(
            line("a", (File, Skip(UpToDate))),
            vec!["a: object_type=file action=would_skip (up to date)"]
        );
    }

    #[test]
    fn a_report_carries_dry_run_and_unprobed_only_when_set() {
        let mut r = Report::pre_engine(false);
        assert!(!r.to_json().contains("dry_run"));
        r.dry_run = true;
        r.unprobed = vec!["noreplace_publish"];
        let v: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert_eq!(v["dry_run"], true);
        assert_eq!(v["unprobed"], serde_json::json!(["noreplace_publish"]));
        assert_eq!(v.as_object().unwrap().len(), 20);
    }

    #[test]
    fn each_record_renders_its_code_first() {
        let line =
            |path: &str, cause| record_lines(&TreeFailure { path: PathBuf::from(path), cause });
        assert_eq!(
            line("dev", TreeFailureCause::SpecialFileSkipped),
            ["SPECIAL_FILE_UNSUPPORTED: dev: object_type=special action=skipped"]
        );
        assert_eq!(
            line("l", TreeFailureCause::SymlinkSkipped),
            ["l: object_type=symlink action=skipped (--links=skip)"]
        );
        assert_eq!(
            line("m", TreeFailureCause::MountSkipped),
            ["m: object_type=mount action=skipped (pass --cross-filesystems to copy its contents)"]
        );
        assert_eq!(
            line("s", TreeFailureCause::Walk(FsError::new(Code::PermissionDenied, io("denied")))),
            ["PERMISSION_DENIED: s: denied"]
        );
        let copy = CopyError {
            cause: FsError::new(Code::DestinationNamespaceCollision, io("exists")),
            leftover: Some((Path::new("sub").join("a.flux-partial.1"), io("busy"))),
            step: CopyStep::Gate,
        };
        assert_eq!(
            line("sub/a", TreeFailureCause::Copy(copy)),
            [
                "DESTINATION_NAMESPACE_COLLISION: sub/a: exists; temporary left at sub/a.flux-partial.1 (busy)"
            ]
        );
        let complaint =
            |item| MetadataFailure { item, error: FsError::new(Code::PermissionDenied, io("no")) };
        let v = vec![complaint(MetadataItem::Times), complaint(MetadataItem::Permissions)];
        assert_eq!(
            line("a", TreeFailureCause::PublishedWithComplaints(v)),
            ["METADATA_APPLY_FAILED: a: times: no", "METADATA_APPLY_FAILED: a: permissions: no"]
        );
        assert_eq!(
            line(
                "sub/a",
                TreeFailureCause::ClaimNotRecorded(FsError::new(Code::IoError, io("disk full")))
            ),
            ["IO_ERROR: sub/a: disk full; the file was published but its claim was not recorded"]
        );
        assert_eq!(
            line("", TreeFailureCause::ClaimNotRecorded(FsError::new(Code::IoError, io("sync")))),
            ["IO_ERROR: .: sync; the file was published but its claim was not recorded"]
        );
    }

    #[test]
    fn one_warning_per_weak_volume_and_one_for_unavailable() {
        let mut w = WeakIdentityWarnings::default();
        assert!(warning_lines(&w).is_empty());
        w.weak.insert(0x2a, DegradedGroup { count: 3, example: PathBuf::from("sub") });
        w.unavailable = Some(DegradedGroup { count: 1, example: PathBuf::new() });

        let v = warning_lines(&w);

        assert_eq!(v.len(), 2);
        for needle in ["0x2a", "3 checks", "e.g. sub", "--safety=strict"] {
            assert!(v[0].contains(needle), "{needle}: {}", v[0]);
        }
        assert!(v[1].contains("e.g. ."), "the root renders as a dot: {}", v[1]);
        assert!(v[1].contains("--safety=strict"));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn both_safety_warnings_render_after_the_identity_ones() {
        let mut out = TreeOutcome::default();
        out.mount_unknown = Some(DegradedGroup { count: 2, example: PathBuf::from("sub") });
        out.source_mount_unknown =
            Some(DegradedGroup { count: 3, example: PathBuf::from("src-sub") });
        out.containment_degraded = Some(PathBuf::from("/p"));
        out.replace_degraded = Some(DegradedGroup { count: 4, example: PathBuf::from("d") });
        let v = safety_warning_lines(&out);
        assert_eq!(v.len(), 4);
        for needle in ["mount root", "2 checks", "e.g. sub", "--safety=strict"] {
            assert!(v[0].contains(needle), "{needle}: {}", v[0]);
        }
        for needle in
            ["source directory", "3 checks", "e.g. src-sub", "descended", "--safety=strict"]
        {
            assert!(v[1].contains(needle), "{needle}: {}", v[1]);
        }
        for needle in ["/p", "lexically", "--safety=strict"] {
            assert!(v[2].contains(needle), "{needle}: {}", v[2]);
        }
        assert_eq!(
            v[3],
            "warning: could not key claims in 4 directories (e.g. d); existing files there are reported as collisions - --safety=strict skips them instead"
        );
        assert!(safety_warning_lines(&TreeOutcome::default()).is_empty());

        // The order is pinned where it is decided: identity lines first, then these two.
        out.warnings.unavailable = Some(DegradedGroup { count: 1, example: PathBuf::new() });
        let all = tree_warning_lines(&out);
        assert_eq!(all.len(), 5);
        assert!(
            all[0].contains("identity")
                && all[1].contains("destination directory is a mount root")
                && all[2].contains("source directory is a mount root")
                && all[3].contains("lexically")
                && all[4].contains("could not key claims"),
            "{all:?}"
        );
    }

    #[test]
    fn a_single_file_warning_names_the_target() {
        let weak = FileIdentity::Weak(ObjectId { volume: 0x10, index: 1 });
        let line = file_warning(&weak, Path::new("out.txt")).unwrap();
        assert!(
            line.contains("0x10") && line.contains("out.txt") && line.contains("--safety=strict")
        );
        let strong = FileIdentity::Strong(ObjectId { volume: 1, index: 1 });
        assert_eq!(file_warning(&strong, Path::new("x")), None);
    }

    #[test]
    fn the_summary_counts_an_abort_as_a_failure() {
        let r = Report::tree(&TreeOutcome::default(), true, 0);
        assert_eq!(
            summary_line(&r, 0),
            "copied 0 files (0 bytes), created 0 directories; 1 failed, 0 skipped"
        );
    }

    fn holder() -> flux_core::lock::record::LockRecord {
        flux_core::lock::record::LockRecord {
            complete_lock_key: "k".to_string(),
            operation_id: "1".repeat(32),
            owner_instance_id: "2".repeat(32),
            boot_session_id: "boot".to_string(),
            target_path_key: "t".to_string(),
            workspace_path: "operations/x".to_string(),
            creation_wall_time: 5,
            last_heartbeat_wall_time: 7,
        }
    }

    #[test]
    fn a_refusal_prints_its_code_and_detail_then_one_line_per_holder_field() {
        let e = RunError::Refused {
            refusal: Box::new(Refusal {
                code: LockCode::TargetLockBusy,
                holder: Some(holder()),
                detail: "/p/d.flux-lock: another run holds the lock; wait for it to finish"
                    .to_string(),
            }),
            changed: false,
            not_removed: None,
        };
        assert_eq!(
            stop_lines(&e),
            vec![
                "TARGET_LOCK_BUSY: /p/d.flux-lock: another run holds the lock; wait for it to finish".to_string(),
                format!("  holder owner_instance_id: {}", "2".repeat(32)),
                "  holder boot_session_id: boot".to_string(),
                "  holder last_heartbeat_wall_time: 7".to_string(),
                "  holder workspace_path: operations/x".to_string(),
            ]
        );
    }

    #[test]
    fn a_refusal_names_what_it_could_not_remove_and_a_failure_names_its_step_and_path() {
        let e = RunError::Refused {
            refusal: Box::new(Refusal {
                code: LockCode::StateCorrupt,
                holder: None,
                detail: "D/m: not JSON".to_string(),
            }),
            changed: true,
            not_removed: Some((
                PathBuf::from("P/d.flux-lock"),
                FsError::new(Code::PermissionDenied, std::io::Error::other("denied")),
            )),
        };
        assert_eq!(
            stop_lines(&e),
            vec![
                "STATE_CORRUPT: D/m: not JSON".to_string(),
                "  not removed: P/d.flux-lock (denied)".to_string()
            ]
        );
        let f = RunError::Failed {
            step: RunStep::State,
            path: PathBuf::from("D/m"),
            error: FsError::new(Code::DiskFull, std::io::Error::other("full")),
            not_removed: None,
        };
        assert_eq!(
            stop_lines(&f),
            vec!["DISK_FULL: writing the operation's state at D/m: full".to_string()]
        );
    }

    #[test]
    fn a_failed_run_with_a_leftover_lock_prints_an_also_not_removed_line() {
        let f = RunError::Failed {
            step: RunStep::State,
            path: PathBuf::from("D/m"),
            error: FsError::new(Code::DiskFull, std::io::Error::other("full")),
            not_removed: Some((
                PathBuf::from("P/d.flux-lock"),
                FsError::new(Code::PermissionDenied, std::io::Error::other("denied")),
            )),
        };
        assert_eq!(
            stop_lines(&f),
            vec![
                "DISK_FULL: writing the operation's state at D/m: full".to_string(),
                "  not removed: P/d.flux-lock (denied)".to_string()
            ]
        );
    }

    #[test]
    fn every_run_warning_is_one_warning_line_naming_its_path() {
        let io = || FsError::new(Code::IoError, std::io::Error::other("busy"));
        let p = || PathBuf::from("X/the-path");
        let all = [
            RunWarning::BrokenLeftover(p()),
            RunWarning::NotRemoved { path: p(), error: io() },
            RunWarning::StateKept(p()),
            RunWarning::PartialKept { path: p(), error: io(), kept: PathBuf::from("X/kept") },
            RunWarning::OwnershipLostAfterCompletion(p()),
            RunWarning::ProbeNotRemoved { path: p(), error: io() },
            RunWarning::ResumeMappingByPath(p()),
        ];
        for w in &all {
            let line = run_warning_line(w);
            assert!(line.starts_with("warning: ") && line.contains("the-path"), "{line}");
            assert!(!line.contains('\n'), "{line}");
        }
        assert!(run_warning_line(&all[3]).contains("X/kept"));
    }

    #[test]
    fn a_resume_mapping_by_path_warning_is_this_exact_line() {
        let path = PathBuf::from("X/the-path");
        assert_eq!(
            run_warning_line(&RunWarning::ResumeMappingByPath(path.clone())),
            format!(
                "warning: the source root's identity could not confirm the mapping, so {} was matched by its path alone",
                path.display()
            )
        );
    }

    #[test]
    fn prior_cleanup_lines_are_the_specs() {
        // Mutant: swap the two arms' formats in `run_warning_line`.
        let a = "0123456789abcdef0123456789abcdef".to_string();
        assert_eq!(
            run_warning_line(&RunWarning::PriorCleaned { operation_id: a.clone(), removed: 2 }),
            format!("note: finished cleaning up the earlier operation {a} (2 leftovers removed)")
        );
        let error = FsError::new(Code::IoError, io("busy"));
        assert_eq!(
            run_warning_line(&RunWarning::PriorCleanupFailed {
                operation_id: a.clone(),
                path: PathBuf::from("X/the-path"),
                error
            }),
            format!(
                "warning: could not finish the cleanup of the earlier operation {a}: X/the-path: busy"
            )
        );
    }

    // Part 3b-2 test audit (round 1): each test below was red under the mutant named in its comment.

    fn refused() -> RunError {
        RunError::Refused {
            refusal: Box::new(Refusal {
                code: LockCode::StateCorrupt,
                holder: None,
                detail: "D/m: not JSON".to_string(),
            }),
            changed: true,
            not_removed: None,
        }
    }

    fn kept() -> RunWarning {
        RunWarning::StateKept(PathBuf::from("X/kept-state"))
    }

    #[test]
    fn resume_verify_lines_print_only_for_full_with_a_partial_resume_or_discard() {
        use flux_core::{CheckpointDiscard, PartialResume};
        let line =
            vec!["resume verification: full (source and partial re-read and hashed)".to_string()];
        let resumed = TreeOutcome {
            partial_resumes: vec![PartialResume {
                path: PathBuf::from("big"),
                reused: 1,
                recopied: 1,
            }],
            ..TreeOutcome::default()
        };
        assert_eq!(resume_verify_lines(&resumed, true), line);
        assert!(resume_verify_lines(&resumed, false).is_empty());
        let discarded = TreeOutcome {
            checkpoint_discards: vec![CheckpointDiscard {
                path: PathBuf::from("big"),
                reason: Discard::PartialMissing,
            }],
            ..TreeOutcome::default()
        };
        assert_eq!(resume_verify_lines(&discarded, true), line);
        assert!(resume_verify_lines(&discarded, false).is_empty());
        assert!(resume_verify_lines(&TreeOutcome::default(), true).is_empty());
        assert!(!resume_verify_used(&TreeOutcome::default(), true));
        assert!(resume_verify_used(&resumed, true));
        assert!(!resume_verify_used(&resumed, false));
    }

    #[test]
    fn the_json_carries_resume_verify_only_when_set() {
        let mut r = Report::pre_engine(false);
        assert!(!r.to_json().contains("resume_verify"));
        r.resume_verify = Some("full");
        let s = r.to_json();
        assert!(s.contains("\"resume_verify\":\"full\""), "{s}");
        let last = KEYS.last().unwrap();
        assert!(
            s.find(&format!("\"{last}\":")).unwrap() < s.find("\"resume_verify\"").unwrap(),
            "{s}"
        );
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v.as_object().unwrap().len(), KEYS.len() + 1, "{s}");
        assert!(s.trim_end().ends_with("\"resume_verify\":\"full\"}"), "{s}");
    }

    #[test]
    fn checkpoint_lines_name_each_reuse_and_each_discard() {
        use flux_core::{CheckpointDiscard, PartialResume};
        let d = |p: &str, reason| CheckpointDiscard { path: PathBuf::from(p), reason };
        let out = TreeOutcome {
            partial_resumes: vec![PartialResume {
                path: PathBuf::from("sub/big"),
                reused: 3145728,
                recopied: 2097152,
            }],
            checkpoint_discards: vec![
                d("a", Discard::SourceChanged("length")),
                d("b", Discard::SourceChanged("modified time")),
                d("c", Discard::SourceChanged("identity")),
                d("d", Discard::PartialMissing),
                d("sub/e", Discard::MismatchAt(1048576)),
                d("f", Discard::PartialShorter { kept: 2621440 }),
                d("g", Discard::Unreadable("bad crc".into())),
            ],
            ..TreeOutcome::default()
        };
        let strict = checkpoint_lines(&out, false);
        assert_eq!(
            strict,
            vec![
                "resumed sub/big: reused 3145728 bytes, recopied 2097152 bytes",
                "checkpoints discarded for a: source changed (length)",
                "checkpoints discarded for b: source changed (modified time)",
                "checkpoints discarded for c: source changed (identity)",
                "checkpoints discarded for d: partial missing",
                "checkpoints discarded for sub/e: mismatch at offset 1048576",
                "checkpoints discarded for f: partial shorter than its checkpoints (kept 2621440 bytes)",
                "checkpoints discarded for g: checkpoints unreadable: bad crc",
            ]
        );
        let normal = checkpoint_lines(&out, true);
        assert_eq!(&normal[..strict.len()], &strict[..]);
        assert_eq!(
            &normal[strict.len()..],
            [
                "note: --durability=normal checkpoints are advisory; every reused chunk was validated against the source and the destination"
            ]
        );
    }

    fn file_run_with(
        file_resume: Option<flux_core::run::FileResume>,
    ) -> Run<Result<Outcome, CopyError>> {
        Run {
            copy: Some(Ok(Outcome {
                bytes_copied: 1,
                metadata_failures: Vec::new(),
                identity_degraded: None,
                published_identity: flux_fs::FileIdentity::Unavailable,
                skipped: false,
            })),
            stop: None,
            warnings: Vec::new(),
            resumed: None,
            beat_gap_max: std::time::Duration::ZERO,
            file_resume,
            dry: None,
        }
    }

    #[test]
    fn file_checkpoint_lines_name_the_reuse_and_the_discard() {
        use flux_core::run::FileResume;
        let run = file_run_with(Some(FileResume {
            reused: 3145728,
            recopied: 2097152,
            discard: Some(Discard::PartialShorter { kept: 3145728 }),
        }));
        let strict = file_checkpoint_lines(&run, "t", false);
        assert_eq!(
            strict,
            vec![
                "resumed t: reused 3145728 bytes, recopied 2097152 bytes",
                "checkpoints discarded for t: partial shorter than its checkpoints (kept 3145728 bytes)",
            ]
        );
        let normal = file_checkpoint_lines(&run, "t", true);
        assert_eq!(&normal[..2], &strict[..]);
        assert_eq!(
            &normal[2..],
            [
                "note: --durability=normal checkpoints are advisory; every reused chunk was validated against the source and the destination"
            ]
        );
    }

    #[test]
    fn a_file_discard_with_no_reuse_prints_the_mismatch_offset_and_no_resumed_line() {
        use flux_core::run::FileResume;
        let run = file_run_with(Some(FileResume {
            reused: 0,
            recopied: 9,
            discard: Some(Discard::MismatchAt(7)),
        }));
        assert_eq!(
            file_checkpoint_lines(&run, "t", false),
            vec!["checkpoints discarded for t: mismatch at offset 7"]
        );
    }

    // Mutants: the advisory note keyed on `fr.reused > 0` instead of "any line printed" (a discard-only report loses
    // it), or printed for a `FileResume` that printed nothing, or its durability test inverted.
    #[test]
    fn the_advisory_note_follows_any_printed_line_under_normal_and_never_under_strict() {
        use flux_core::run::FileResume;
        const NOTE: &str = "note: --durability=normal checkpoints are advisory; every reused chunk was validated against the source and the destination";
        let discard_only = file_run_with(Some(FileResume {
            reused: 0,
            recopied: 9,
            discard: Some(Discard::MismatchAt(7)),
        }));
        assert_eq!(
            file_checkpoint_lines(&discard_only, "t", true),
            vec!["checkpoints discarded for t: mismatch at offset 7", NOTE]
        );
        let reuse_only = file_run_with(Some(FileResume { reused: 5, recopied: 4, discard: None }));
        assert_eq!(
            file_checkpoint_lines(&reuse_only, "t", true),
            vec!["resumed t: reused 5 bytes, recopied 4 bytes", NOTE]
        );
        assert_eq!(
            file_checkpoint_lines(&reuse_only, "t", false),
            vec!["resumed t: reused 5 bytes, recopied 4 bytes"]
        );
        let nothing = file_run_with(Some(FileResume { reused: 0, recopied: 9, discard: None }));
        assert!(file_checkpoint_lines(&nothing, "t", true).is_empty());
        assert!(file_checkpoint_lines(&nothing, "t", false).is_empty());
    }

    #[test]
    fn file_checkpoint_lines_print_nothing_without_a_file_resume() {
        let run = file_run_with(None);
        assert!(file_checkpoint_lines(&run, "t", true).is_empty());
        assert!(file_checkpoint_lines(&run, "t", false).is_empty());
    }

    #[test]
    fn the_level_line_and_json_field_appear_exactly_when_a_single_file_resume_ran() {
        use flux_core::run::FileResume;
        let some = file_run_with(Some(FileResume { reused: 1, recopied: 0, discard: None }));
        assert_eq!(
            file_resume_verify_lines(&some),
            vec!["resume verification: full (source and partial re-read and hashed)"]
        );
        assert_eq!(Report::file_run(&some, false, 1).resume_verify, Some("full"));
        let none = file_run_with(None);
        assert!(file_resume_verify_lines(&none).is_empty());
        assert_eq!(Report::file_run(&none, false, 1).resume_verify, None);
    }

    #[test]
    fn an_uncheckpointed_resume_report_is_unchanged() {
        assert!(checkpoint_lines(&TreeOutcome::default(), true).is_empty());
        assert!(checkpoint_lines(&TreeOutcome::default(), false).is_empty());
    }

    #[test]
    fn a_wal_write_failure_adds_one_resume_hint_and_no_other_error_does() {
        let wal = CopyError {
            cause: FsError::new(Code::WalWriteFailed, std::io::Error::other("x")),
            leftover: None,
            step: CopyStep::Checkpoint,
        };
        assert_eq!(
            abort_hint_lines(&wal),
            vec!["run again with --resume to continue from the last durable checkpoint"]
        );
        let other = CopyError {
            cause: FsError::new(Code::IoError, io("x")),
            leftover: None,
            step: CopyStep::Source,
        };
        assert!(abort_hint_lines(&other).is_empty());
    }

    // Owner ruling 23: the exact texts of the two quarantine warnings.
    #[test]
    fn the_wal_quarantine_warnings_have_their_exact_texts() {
        assert_eq!(
            run_warning_line(&RunWarning::WalQuarantined {
                code: "WAL_CORRUPT",
                why: "corrupt log at byte 40: bad".to_string(),
                set_aside_as: "000001.corrupt-2".to_string(),
            }),
            "WAL_CORRUPT: the checkpoint log is damaged (corrupt log at byte 40: bad); possible storage damage, investigate; it was set aside as wal/000001.corrupt-2 and a fresh log was started; every large file recopies"
        );
        assert_eq!(
            run_warning_line(&RunWarning::WalNotQuarantined {
                code: "WAL_SEQUENCE_CONFLICT",
                why: "conflict".to_string(),
                error: "denied".to_string(),
            }),
            "WAL_SEQUENCE_CONFLICT: the checkpoint log is damaged (conflict) and could not be set aside (denied); this run copies with no checkpoints"
        );
    }

    // Owner ruling 23: an unsupported log format aborts the tree with its own code, exits 1 and has no resume hint.
    #[test]
    fn a_wal_format_unsupported_abort_prints_its_code_exits_1_and_adds_no_hint() {
        let error = CopyError {
            cause: FsError::new(
                Code::WalFormatUnsupported,
                std::io::Error::other(
                    "the checkpoint log was written by a newer Flux (unsupported log format 2 (checksum 1)); nothing was copied; run again with --restart to supersede it",
                ),
            ),
            leftover: None,
            step: CopyStep::Checkpoint,
        };
        let line = error.to_string();
        assert!(line.contains("WAL_FORMAT_UNSUPPORTED: "), "{line}");
        assert!(line.contains("run again with --restart to supersede it"), "{line}");
        assert!(abort_hint_lines(&error).is_empty());
        let aborted =
            Err(flux_core::TreeAbort { error, outcome: flux_core::TreeOutcome::default() });
        assert_eq!(crate::exit_code::for_tree(&aborted), crate::exit_code::FAILED);
    }

    #[test]
    fn resume_lines_name_the_operation_and_the_counts() {
        use flux_core::run::ResumeNote;
        let with = |resumed| Run::<()> {
            copy: None,
            stop: None,
            warnings: Vec::new(),
            resumed,
            beat_gap_max: std::time::Duration::ZERO,
            file_resume: None,
            dry: None,
        };
        assert_eq!(
            resume_lines(&with(Some(ResumeNote::StartedNew)), 0),
            vec!["no prior operation; starting new".to_string()]
        );
        let adopted =
            |claims| Some(ResumeNote::Adopted { operation_id: "op1".into(), claims, recovered: 0 });
        assert_eq!(
            resume_lines(&with(adopted(Some(3))), 0),
            vec!["resuming operation op1 (3 entries claimed)".to_string()]
        );
        assert_eq!(
            resume_lines(&with(adopted(None)), 0),
            vec!["resuming operation op1".to_string()]
        );
        assert_eq!(
            resume_lines(&with(adopted(Some(3))), 7),
            vec![
                "resuming operation op1 (3 entries claimed)".to_string(),
                "resumed 7 already-complete files".to_string()
            ]
        );
        assert!(resume_lines(&with(None), 0).is_empty());
        // Cut 9c: the recovered publications follow the note, and precede the resumed files.
        let recovering = |recovered| {
            Some(ResumeNote::Adopted { operation_id: "op1".into(), claims: Some(3), recovered })
        };
        assert_eq!(
            resume_lines(&with(recovering(2)), 0),
            vec![
                "resuming operation op1 (3 entries claimed)".to_string(),
                "recovered 2 interrupted publications".to_string()
            ]
        );
        assert_eq!(
            resume_lines(&with(recovering(0)), 0),
            vec!["resuming operation op1 (3 entries claimed)".to_string()]
        );
        assert_eq!(
            resume_lines(&with(recovering(2)), 7),
            vec![
                "resuming operation op1 (3 entries claimed)".to_string(),
                "recovered 2 interrupted publications".to_string(),
                "resumed 7 already-complete files".to_string()
            ]
        );
    }

    #[test]
    fn a_tree_report_folds_resumed_files_into_skipped() {
        let out = TreeOutcome {
            files_skipped: 3,
            files_resumed: 4,
            special_files_skipped: 1,
            bytes_skipped: 10,
            bytes_resumed: 5,
            ..TreeOutcome::default()
        };
        let r = Report::tree(&out, false, 1);
        assert_eq!((r.files_skipped, r.bytes_skipped), (8, 15));
    }

    #[test]
    fn a_runs_lines_are_its_stop_then_every_warning() {
        let run: Run<()> = Run {
            copy: None,
            stop: Some(refused()),
            warnings: vec![kept(), kept()],
            resumed: None,
            beat_gap_max: std::time::Duration::ZERO,
            file_resume: None,
            dry: None,
        };
        let lines = run_lines(&run);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].starts_with("STATE_CORRUPT: "), "{lines:?}");
        assert!(
            lines[1..].iter().all(|l| l.starts_with("warning: ") && l.contains("kept-state")),
            "{lines:?}"
        );
        let quiet: Run<()> = Run {
            copy: None,
            stop: None,
            warnings: vec![kept()],
            resumed: None,
            beat_gap_max: std::time::Duration::ZERO,
            file_resume: None,
            dry: None,
        };
        assert_eq!(run_lines(&quiet).len(), 1, "warnings print without a stop too");
    }

    #[test]
    fn a_tree_runs_report_counts_a_stop_as_one_more_error() {
        let run = |copy, stop| Run {
            copy,
            stop,
            warnings: Vec::new(),
            resumed: None,
            beat_gap_max: std::time::Duration::ZERO,
            file_resume: None,
            dry: None,
        };
        let clean = Report::tree_run(&run(Some(Ok(TreeOutcome::default())), None), 1);
        assert_eq!(clean.errors, 0);
        let stopped = Report::tree_run(&run(Some(Ok(TreeOutcome::default())), Some(refused())), 1);
        assert_eq!(stopped.errors, 1, "a stop after the copy is an error");
        let mut one_failed = TreeOutcome::default();
        one_failed.failures.copy = 1;
        let both = Report::tree_run(&run(Some(Ok(one_failed)), Some(refused())), 1);
        assert_eq!(both.errors, 2, "a stop adds to the copy's own errors");
        // An abort after one streamed failure: the failure, the abort and the stop are each an error.
        let aborted = || {
            let cause = FsError::new(Code::SafetyRejected, std::io::Error::other("x"));
            let error = CopyError { cause, leftover: None, step: CopyStep::Resolve };
            let mut outcome = TreeOutcome::default();
            outcome.failures.copy = 1;
            Some(Err(TreeAbort { error, outcome }))
        };
        let abort = Report::tree_run(&run(aborted(), None), 1);
        assert_eq!((abort.errors, abort.files_failed), (2, 1), "the abort's own counts are kept");
        assert_eq!(Report::tree_run(&run(aborted(), Some(refused())), 1).errors, 3);
        let before = Report::tree_run(&run(None, Some(refused())), 1);
        assert_eq!((before.errors, before.files_total), (1, 0), "a stop before the copy");
    }

    #[test]
    fn a_file_runs_report_counts_a_stop_as_one_more_error() {
        let run = |copy, stop| Run {
            copy,
            stop,
            warnings: Vec::new(),
            resumed: None,
            beat_gap_max: std::time::Duration::ZERO,
            file_resume: None,
            dry: None,
        };
        let copied = || {
            Ok(Outcome {
                bytes_copied: 1,
                metadata_failures: Vec::new(),
                identity_degraded: None,
                published_identity: flux_fs::FileIdentity::Unavailable,
                skipped: false,
            })
        };
        assert_eq!(Report::file_run(&run(Some(copied()), None), false, 1).errors, 0);
        assert_eq!(Report::file_run(&run(Some(copied()), Some(refused())), false, 1).errors, 1);
        let failed = || {
            let cause = FsError::new(Code::IoError, std::io::Error::other("x"));
            Err(CopyError { cause, leftover: None, step: CopyStep::Resolve })
        };
        let both = Report::file_run(&run(Some(failed()), Some(refused())), false, 1);
        assert_eq!(
            (both.errors, both.files_failed),
            (2, 1),
            "a failed copy and a stop are two errors"
        );
        let before = Report::file_run(&run(None, Some(refused())), false, 1);
        assert_eq!((before.errors, before.files_total), (1, 0), "a stop before the copy");
        // An existing target changes nothing about a run that never reached its copy.
        let over = Report::file_run(&run(None, Some(refused())), true, 1);
        assert_eq!((over.errors, over.files_total, over.files_failed), (1, 0, 0), "over a target");
    }

    // Mutant: the arm's reason or numbers are swapped, or its sentence changed.
    #[test]
    fn the_wal_tail_warning_names_the_cut_and_why_for_each_reason() {
        for reason in
            ["zero-filled", "its declared span reaches the end of the log", "a record cut short"]
        {
            assert_eq!(
                run_warning_line(&RunWarning::WalTailDiscarded { offset: 1014, len: 4096, reason }),
                format!(
                    "WAL tail cut: 4096 bytes at offset 1014 ({reason}); the checkpoints before it are intact"
                )
            );
        }
    }
}
