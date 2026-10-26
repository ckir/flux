// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! What `flux verify` prints (cut 14): record lines, NDJSON, the summary and the notices. Pure: every function
//! returns text, and `main` writes it.

use crate::report::rel;
use flux_core::verify::{Record, Summary};
use serde::Serialize;
use std::path::Path;

/// Design 6.8, verbatim, for `--help`: one paragraph per bullet.
pub const GUARANTEES: &str = "What a pass guarantees:

- `match` means the bytes the operating system returned for both files, at the moment each was read, hash equal. The destination read may be served from the page cache, so it proves the destination returns those bytes, not that they are on stable storage (the same boundary as `copy --verify=destination`).

- Hardlinks are not compared. Two names of one source file copied as two independent files still `match`. Use `copy --hardlinks` to preserve them.

- `extra` paths are reported but do not change the exit status. A script that needs an exact replica must read the summary (`extra` is a field of it).

- The result is a snapshot with no lock; a destination another run is changing can give stale results.

- A name that differs only in case on a case-insensitive destination is a `missing` plus an `extra`, not a `match`. The same holds for two Unicode normalisations of one name (macOS NFD against NFC): names are compared as raw bytes.

- A mount point the walker did not enter (no `--cross-filesystems`) is reported `match` for its presence only; its contents were NOT compared. The summary's `not_entered` count and the notice line say so, and exit 0 does not mean those contents were verified.";

/// `{outcome}: {path}`, then `: {CODE}` when there is a code and `: {detail}` when there is a detail.
pub fn record_line(r: &Record) -> String {
    let mut line = format!("{}: {}", r.outcome.as_str(), rel(&r.path));
    if let Some(code) = r.code {
        line.push_str(": ");
        line.push_str(code.as_str());
    }
    if !r.detail.is_empty() {
        line.push_str(": ");
        line.push_str(&r.detail);
    }
    line
}

#[derive(Serialize)]
struct RecordJson<'a> {
    path: String,
    outcome: &'static str,
    kind: &'static str,
    code: Option<&'static str>,
    detail: &'a str,
}

/// One NDJSON line (no trailing newline).
pub fn record_json(r: &Record) -> String {
    let j = RecordJson {
        path: rel(&r.path),
        outcome: r.outcome.as_str(),
        kind: r.kind.as_str(),
        code: r.code.map(|c| c.as_str()),
        detail: &r.detail,
    };
    serde_json::to_string(&j).expect("a record serialises")
}

/// The human summary line.
pub fn summary_line(s: &Summary) -> String {
    let mut line = format!(
        "verified: {} matched, {} missing, {} mismatched, {} extra, {} unreadable; {} bytes hashed",
        s.matched, s.missing, s.mismatched, s.extra, s.unreadable, s.bytes_hashed
    );
    if s.not_entered > 0 {
        line.push_str(&format!("; {} mount points not entered", s.not_entered));
    }
    line
}

#[derive(Serialize)]
struct SummaryBody {
    source: String,
    dest: String,
    hash: &'static str,
    matched: u64,
    missing: u64,
    mismatched: u64,
    extra: u64,
    unreadable: u64,
    not_entered: u64,
    bytes_hashed: u64,
    exit: u8,
}

#[derive(Serialize)]
struct SummaryJson {
    summary: SummaryBody,
}

/// The final NDJSON object (no trailing newline).
pub fn summary_json(
    source: &Path,
    dest: &Path,
    hash: &'static str,
    s: &Summary,
    exit: u8,
) -> String {
    let j = SummaryJson {
        summary: SummaryBody {
            source: source.display().to_string(),
            dest: dest.display().to_string(),
            hash,
            matched: s.matched,
            missing: s.missing,
            mismatched: s.mismatched,
            extra: s.extra,
            unreadable: s.unreadable,
            not_entered: s.not_entered,
            bytes_hashed: s.bytes_hashed,
            exit,
        },
    };
    serde_json::to_string(&j).expect("a summary serialises")
}

/// The stderr notice for a mount point the walkers did not enter.
pub fn mount_notice_line(path: &Path) -> String {
    format!(
        "mount point not entered: {} (use --cross-filesystems to verify its contents)",
        path.display()
    )
}

/// The stderr line before the walk when DEST does not exist.
pub fn dest_absent_line(dest: &Path) -> String {
    format!(
        "destination does not exist: {} (every source path is reported missing)",
        dest.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::verify::{Kind, Outcome, Record, Summary};
    use flux_fs::Code;
    use std::path::{Path, PathBuf};

    fn rec(path: &str, outcome: Outcome, code: Option<Code>, detail: &str) -> Record {
        Record {
            path: PathBuf::from(path),
            outcome,
            kind: Kind::File,
            code,
            detail: detail.to_owned(),
        }
    }

    #[test]
    fn a_record_line_names_the_outcome_the_path_the_code_and_the_detail() {
        let m =
            rec("sub/b", Outcome::Mismatched, Some(Code::VerifyMismatch), "ab12cd34 != ef56ab78");
        assert_eq!(record_line(&m), "mismatched: sub/b: VERIFY_MISMATCH: ab12cd34 != ef56ab78");
        assert_eq!(record_line(&rec("a", Outcome::Match, None, "")), "match: a");
        assert_eq!(record_line(&rec("", Outcome::Missing, None, "")), "missing: .");
    }

    #[test]
    fn a_record_json_has_exactly_the_five_keys_in_order() {
        let m = rec("sub/b", Outcome::Mismatched, Some(Code::VerifyMismatch), "x != y");
        let v: serde_json::Value = serde_json::from_str(&record_json(&m)).unwrap();
        let text = record_json(&m);
        let at: Vec<usize> = ["path", "outcome", "kind", "code", "detail"]
            .iter()
            .map(|k| text.find(&format!("\"{k}\":")).unwrap())
            .collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert_eq!(v.as_object().unwrap().len(), 5);
        assert_eq!(v["code"], "VERIFY_MISMATCH");
        assert_eq!(v["outcome"], "mismatched");
        assert_eq!(v["kind"], "file");
        let ok: serde_json::Value =
            serde_json::from_str(&record_json(&rec("a", Outcome::Match, None, ""))).unwrap();
        assert!(ok["code"].is_null());
        assert!(!record_json(&m).contains('\n'));
    }

    #[test]
    fn a_summary_json_has_exactly_the_eleven_fields() {
        let s = Summary {
            matched: 1,
            missing: 2,
            mismatched: 3,
            extra: 4,
            unreadable: 5,
            not_entered: 6,
            bytes_hashed: 7,
        };
        let text = summary_json(Path::new("/s"), Path::new("/d"), "blake3", &s, 1);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let top: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(top, ["summary"]);
        let want = [
            "source",
            "dest",
            "hash",
            "matched",
            "missing",
            "mismatched",
            "extra",
            "unreadable",
            "not_entered",
            "bytes_hashed",
            "exit",
        ];
        let at: Vec<usize> =
            want.iter().map(|k| text.find(&format!("\"{k}\":")).unwrap()).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert_eq!(v["summary"].as_object().unwrap().len(), want.len());
        assert_eq!(v["summary"]["source"], "/s");
        assert_eq!(v["summary"]["dest"], "/d");
        assert_eq!(v["summary"]["matched"], 1);
        assert_eq!(v["summary"]["not_entered"], 6);
        assert_eq!(v["summary"]["bytes_hashed"], 7);
        assert_eq!(v["summary"]["exit"], 1);
    }

    #[test]
    fn the_summary_line_mentions_not_entered_only_when_non_zero() {
        let mut s = Summary { matched: 3, missing: 1, ..Summary::default() };
        s.bytes_hashed = 9;
        assert_eq!(
            summary_line(&s),
            "verified: 3 matched, 1 missing, 0 mismatched, 0 extra, 0 unreadable; 9 bytes hashed"
        );
        s.not_entered = 2;
        assert_eq!(
            summary_line(&s),
            "verified: 3 matched, 1 missing, 0 mismatched, 0 extra, 0 unreadable; 9 bytes hashed; 2 mount points not entered"
        );
    }
}
