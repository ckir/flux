//! What `flux copy` prints (cut 5): one stderr line per streamed record, the
//! aggregated warnings, a summary line, and the §53 JSON report. Pure: every function
//! returns text, and `main` writes it.

use flux_core::copy::CopyError;
use flux_core::{TreeFailure, TreeFailureCause, TreeOutcome, WeakIdentityWarnings};
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
        r.files_skipped = out.special_files_skipped;
        r.files_degraded = out.files_degraded;
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
                rep.files_copied = 1;
                rep.files_overwritten = u64::from(target_existed);
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

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::DegradedGroup;
    use flux_core::copy::CopyStep;
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
    fn a_single_file_report() {
        let ok: Result<Outcome, CopyError> = Ok(Outcome {
            bytes_copied: 5,
            metadata_failures: Vec::new(),
            identity_degraded: Some(FileIdentity::Unavailable),
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
        let ok: Result<Outcome, CopyError> =
            Ok(Outcome { bytes_copied: 5, metadata_failures: Vec::new(), identity_degraded: None });
        let r = Report::file(&ok, false, 0);
        assert_eq!((r.files_copied, r.files_overwritten), (1, 0));
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
        assert!(
            line("l", TreeFailureCause::Symlink)[0]
                .starts_with("SYMLINK_CREATION_UNAVAILABLE: l: ")
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
}
