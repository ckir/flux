//! What `flux copy` prints (cut 5): one stderr line per streamed record, the
//! aggregated warnings, a summary line, and the §53 JSON report. Pure: every function
//! returns text, and `main` writes it.

use flux_core::copy::CopyError;
use flux_core::run::{Run, RunError, RunWarning};
use flux_core::{TreeAbort, TreeFailure, TreeFailureCause, TreeOutcome, WeakIdentityWarnings};
use flux_fs::{Code, FileIdentity, MetadataFailure, MetadataItem, Outcome};
use serde::Serialize;
use std::path::Path;

/// Why a symlink fails, shared by the tree record and the SOURCE refusal (K7).
pub const SYMLINK_WHY: &str =
    "a symlink is copied as a link (§25), which this version cannot create";

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
        }
    }

    /// A tree copy, finished or aborted. `bytes_total` is `bytes_copied`: the walk never
    /// stats a file, so a failed file's size is unknown (owner, cut 5). A skipped special
    /// file is a skipped target (§233.1 `action=skipped`).
    pub fn tree(out: &TreeOutcome, aborted: bool, duration_ms: u64) -> Self {
        let mut r = Self::zero();
        r.files_total = out.files_total;
        r.files_copied = out.files_copied;
        r.files_skipped = out.special_files_skipped + out.files_skipped;
        r.files_overwritten = out.files_overwritten;
        r.bytes_skipped = out.bytes_skipped;
        r.files_degraded = out.files_degraded;
        // Only `copy` and `symlink` are failed FILES. A `ClaimNotRecorded` failure is counted in
        // `FailureTally::total()` (so in `errors`) and in `claim_not_recorded`, but not here: the file was published
        // and counted as copied.
        r.files_failed = out.failures.copy + out.failures.symlink;
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
        TreeFailureCause::Symlink => {
            vec![format!("{}: {p}: {SYMLINK_WHY}", Code::SymlinkCreationUnavailable.as_str())]
        }
        TreeFailureCause::SpecialFileSkipped => vec![format!(
            "{}: {p}: object_type=special action=skipped",
            Code::SpecialFileUnsupported.as_str()
        )],
        TreeFailureCause::PublishedWithComplaints(v) => {
            v.iter().map(|m| complaint_line(&p, m)).collect()
        }
        TreeFailureCause::ClaimNotRecorded(e) => vec![format!(
            "{}: {p}: {}; the file was published but its claim was not recorded",
            e.code.as_str(),
            e.source
        )],
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
    }
}

/// A run's own stop, then its warnings (cut 7a): the stderr lines that follow the copy's and precede the summary.
pub fn run_lines<T>(run: &Run<T>) -> Vec<String> {
    let mut v = run.stop.as_ref().map(stop_lines).unwrap_or_default();
    v.extend(run.warnings.iter().map(run_warning_line));
    v
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
        out.failures.copy = 1;
        out.failures.symlink = 1;
        out.failures.walk = 1;
        out.failures.published_with_complaints = 1;

        let r = Report::tree(&out, false, 10);

        assert_eq!((r.files_total, r.files_copied, r.files_skipped), (7, 2, 1));
        assert_eq!(r.files_degraded, 1);
        assert_eq!(r.files_failed, 2, "copy + symlink: walk and complaints are not failed files");
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
    fn each_record_renders_its_code_first() {
        let line =
            |path: &str, cause| record_lines(&TreeFailure { path: PathBuf::from(path), cause });
        assert_eq!(
            line("dev", TreeFailureCause::SpecialFileSkipped),
            ["SPECIAL_FILE_UNSUPPORTED: dev: object_type=special action=skipped"]
        );
        assert_eq!(
            line("l", TreeFailureCause::Symlink),
            [
                "SYMLINK_CREATION_UNAVAILABLE: l: a symlink is copied as a link (§25), which this version cannot create"
            ]
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
        out.containment_degraded = Some(PathBuf::from("/p"));
        out.replace_degraded = Some(DegradedGroup { count: 4, example: PathBuf::from("d") });
        let v = safety_warning_lines(&out);
        assert_eq!(v.len(), 3);
        for needle in ["mount root", "2 checks", "e.g. sub", "--safety=strict"] {
            assert!(v[0].contains(needle), "{needle}: {}", v[0]);
        }
        for needle in ["/p", "lexically", "--safety=strict"] {
            assert!(v[1].contains(needle), "{needle}: {}", v[1]);
        }
        assert_eq!(
            v[2],
            "warning: could not key claims in 4 directories (e.g. d); existing files there are reported as collisions - --safety=strict skips them instead"
        );
        assert!(safety_warning_lines(&TreeOutcome::default()).is_empty());

        // The order is pinned where it is decided: identity lines first, then these two.
        out.warnings.unavailable = Some(DegradedGroup { count: 1, example: PathBuf::new() });
        let all = tree_warning_lines(&out);
        assert_eq!(all.len(), 4);
        assert!(
            all[0].contains("identity")
                && all[1].contains("mount root")
                && all[2].contains("lexically")
                && all[3].contains("could not key claims"),
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
        ];
        for w in &all {
            let line = run_warning_line(w);
            assert!(line.starts_with("warning: ") && line.contains("the-path"), "{line}");
            assert!(!line.contains('\n'), "{line}");
        }
        assert!(run_warning_line(&all[3]).contains("X/kept"));
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
    fn a_runs_lines_are_its_stop_then_every_warning() {
        let run: Run<()> =
            Run { copy: None, stop: Some(refused()), warnings: vec![kept(), kept()] };
        let lines = run_lines(&run);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].starts_with("STATE_CORRUPT: "), "{lines:?}");
        assert!(
            lines[1..].iter().all(|l| l.starts_with("warning: ") && l.contains("kept-state")),
            "{lines:?}"
        );
        let quiet: Run<()> = Run { copy: None, stop: None, warnings: vec![kept()] };
        assert_eq!(run_lines(&quiet).len(), 1, "warnings print without a stop too");
    }

    #[test]
    fn a_tree_runs_report_counts_a_stop_as_one_more_error() {
        let run = |copy, stop| Run { copy, stop, warnings: Vec::new() };
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
        let run = |copy, stop| Run { copy, stop, warnings: Vec::new() };
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
}
