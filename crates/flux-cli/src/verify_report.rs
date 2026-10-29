// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! What `flux verify` prints (cut 14): record lines, NDJSON, the summary and the notices. Pure: every function
//! returns text, and `main` writes it.

use flux_core::verify::{MOUNT_NOT_ENTERED, Outcome, Record, Summary};
use serde::Serialize;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Design 6.8, verbatim, for `--help`: one paragraph per bullet.
pub const GUARANTEES: &str = "What a pass guarantees:

- `match` means the bytes the operating system returned for both files, at the moment each was read, hash equal. The destination read may be served from the page cache, so it proves the destination returns those bytes, not that they are on stable storage (the same boundary as `copy --verify=destination`).

- Hardlinks are not compared. Two names of one source file copied as two independent files still `match`. Use `copy --hardlinks` to preserve them.

- `extra` paths are reported but do not change the exit status. A script that needs an exact replica must read the summary (`extra` is a field of it).

- The result is a snapshot with no lock; a destination another run is changing can give stale results.

- A name that differs only in case on a case-insensitive destination is a `missing` plus an `extra`, not a `match`. The same holds for two Unicode normalisations of one name (macOS NFD against NFC): names are compared as raw bytes.

- A mount point the walker did not enter (no `--cross-filesystems`) is reported `match` for its presence only; its contents were NOT compared. The summary's `not_entered` count and the notice line say so, and exit 0 does not mean those contents were verified.

- Entries removed by `--exclude`, and subdirectories left out by `--no-recursive`, are not compared on either side: they are neither reported nor counted as `missing` or `extra`. A directory that was not entered is therefore not only a mount point, and exit 0 does not mean its contents were verified.

- With several sources, only `DEST/<name>` for each source is compared; other entries of DEST are neither reported nor counted.";

/// Append `s` with the backslash, the control characters (U+0000-U+001F, U+007F) escaped.
fn escape_chars(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
}

/// One path component, escaped (see `display_path`).
#[cfg(unix)]
fn escape_component(name: &std::ffi::OsStr, out: &mut String) {
    use std::os::unix::ffi::OsStrExt;
    let mut rest = name.as_bytes();
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                escape_chars(valid, out);
                return;
            }
            Err(e) => {
                let (valid, bad) = rest.split_at(e.valid_up_to());
                escape_chars(std::str::from_utf8(valid).expect("valid up to"), out);
                let n = e.error_len().unwrap_or(bad.len());
                for b in &bad[..n] {
                    out.push_str(&format!("\\x{b:02x}"));
                }
                rest = &bad[n..];
            }
        }
    }
}

/// Non-unix platforms: the lossy text, then the control and backslash rules. The platform-specific branch can only be
/// exercised by CI.
#[cfg(not(unix))]
fn escape_component(name: &std::ffi::OsStr, out: &mut String) {
    escape_chars(&name.to_string_lossy(), out);
}

/// The path of a record as shown (design 6.6): components joined with `/`, `.` for the empty path. The identity for
/// an ordinary name; otherwise `\\`, control characters (`\n`, `\r`, `\t`, else `\xNN`) and every byte that is not part of
/// valid UTF-8 (`\xNN`, lowercase hex) are escaped, so two distinct names never print alike and a name never splits a
/// line.
pub fn display_path(path: &Path) -> String {
    let mut out = String::new();
    for (i, c) in path.components().enumerate() {
        if i > 0 {
            out.push('/');
        }
        escape_component(c.as_os_str(), &mut out);
    }
    if out.is_empty() { ".".to_owned() } else { out }
}

/// `{outcome}: {path}`, then `: {CODE}` when there is a code and `: {detail}` when there is a detail.
pub fn record_line(r: &Record) -> String {
    let mut line = format!("{}: {}", r.outcome.as_str(), display_path(&r.path));
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
        path: display_path(&r.path),
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
    if s.excluded > 0 {
        line.push_str(&format!("; {} excluded", s.excluded));
    }
    line
}

// The declaration order is the JSON key order and is part of the output contract.
#[derive(Serialize)]
struct SummaryBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    sources: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    dest: String,
    hash: &'static str,
    matched: u64,
    missing: u64,
    mismatched: u64,
    extra: u64,
    unreadable: u64,
    not_entered: u64,
    #[serde(skip_serializing_if = "is_zero")]
    excluded: u64,
    bytes_hashed: u64,
    exit: u8,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Serialize)]
struct SummaryJson {
    summary: SummaryBody,
}

/// The source side of the summary: one `source` string, or a `sources` array for a several-source run.
pub enum Sources<'a> {
    One(&'a Path),
    Several(&'a [PathBuf]),
}

/// The final NDJSON object (no trailing newline).
pub fn summary_json(
    sources: Sources<'_>,
    dest: &Path,
    hash: &'static str,
    s: &Summary,
    exit: u8,
) -> String {
    let (sources, source) = match sources {
        Sources::One(p) => (None, Some(p.display().to_string())),
        Sources::Several(ps) => (Some(ps.iter().map(|p| p.display().to_string()).collect()), None),
    };
    let j = SummaryJson {
        summary: SummaryBody {
            sources,
            source,
            dest: dest.display().to_string(),
            hash,
            matched: s.matched,
            missing: s.missing,
            mismatched: s.mismatched,
            extra: s.extra,
            unreadable: s.unreadable,
            not_entered: s.not_entered,
            excluded: s.excluded,
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
        display_path(path)
    )
}

/// The stderr warning for a destination-only directory that could not be listed: an `extra` record that has a code.
pub fn unreadable_extra_warning_line(r: &Record) -> Option<String> {
    (r.outcome == Outcome::Extra && r.code.is_some()).then(|| {
        format!(
            "warning: could not read destination-only directory: {}: {}",
            display_path(&r.path),
            r.detail
        )
    })
}

/// Where a record goes: at most one stdout line, and any stderr lines.
#[derive(Debug, PartialEq, Eq)]
pub struct Routed {
    pub stdout: Option<String>,
    pub stderr: Vec<String>,
}

/// The routing of one record (design 6.6): `--json` prints every record to stdout, the human form hides `match`
/// unless `verbose`; a mount point not entered and an unlistable destination-only directory also say so on stderr.
/// Under `quiet` (without `json`) a `match` or `extra` record is not printed either: neither implies a non-zero exit.
pub fn route_record(r: &Record, json: bool, verbose: bool, quiet: bool) -> Routed {
    let stdout = if json {
        Some(record_json(r))
    } else if quiet && matches!(r.outcome, Outcome::Match | Outcome::Extra) {
        None
    } else if r.outcome != Outcome::Match || verbose {
        Some(record_line(r))
    } else {
        None
    };
    let mut stderr = Vec::new();
    if r.detail == MOUNT_NOT_ENTERED {
        stderr.push(mount_notice_line(&r.path));
    }
    stderr.extend(unreadable_extra_warning_line(r));
    Routed { stdout, stderr }
}

/// The stderr line before the walk when DEST does not exist.
pub fn dest_absent_line(dest: &Path) -> String {
    format!(
        "destination does not exist: {} (every source path is reported missing)",
        dest.display()
    )
}

/// One root's counts in a several-source run (stderr).
pub fn root_line(name: &OsStr, s: &Summary) -> String {
    format!(
        "root {}: {} matched, {} missing, {} mismatched, {} extra, {} unreadable",
        display_path(Path::new(name)),
        s.matched,
        s.missing,
        s.mismatched,
        s.extra,
        s.unreadable
    )
}

/// The stderr line before the walk when `DEST/<name>` does not exist.
pub fn root_absent_line(dst: &Path, name: &OsStr) -> String {
    format!(
        "destination does not exist: {}/{} (every source path is reported missing)",
        dst.display(),
        display_path(Path::new(name))
    )
}

/// The refusal line for one root of a several-source run.
pub fn refused_root_line(name: &OsStr, dst: &Path, reason: &str) -> String {
    let n = display_path(Path::new(name));
    format!("error: root {n}: {}/{n}: {reason}; nothing was verified", dst.display())
}

/// The refusal line for two roots that are one directory on the destination.
pub fn refused_alias_line(a: &OsStr, b: &OsStr) -> String {
    format!(
        "error: roots {} and {} are one directory on this filesystem; nothing was verified",
        display_path(Path::new(a)),
        display_path(Path::new(b))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::verify::{Kind, MOUNT_NOT_ENTERED, Outcome, Record, Summary};
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
            excluded: 0,
            bytes_hashed: 7,
        };
        let text = summary_json(Sources::One(Path::new("/s")), Path::new("/d"), "blake3", &s, 1);
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
    fn summary_json_for_several_sources_omits_source_and_lists_sources() {
        let s = Summary { matched: 1, bytes_hashed: 7, ..Summary::default() };
        let roots = [PathBuf::from("/s/a"), PathBuf::from("/s/b")];
        let text = summary_json(Sources::Several(&roots), Path::new("/d"), "blake3", &s, 0);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let want = [
            "sources",
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
        assert!(v["summary"].get("source").is_none(), "{text}");
        assert_eq!(v["summary"]["sources"], serde_json::json!(["/s/a", "/s/b"]));
    }

    #[test]
    fn root_line_formats_the_five_counts() {
        let s = Summary {
            matched: 1,
            missing: 2,
            mismatched: 3,
            extra: 4,
            unreadable: 5,
            ..Summary::default()
        };
        assert_eq!(
            root_line(std::ffi::OsStr::new("a"), &s),
            "root a: 1 matched, 2 missing, 3 mismatched, 4 extra, 5 unreadable"
        );
    }

    #[test]
    fn refused_root_line_renders_the_reason_under_the_destination() {
        assert_eq!(
            refused_root_line(
                std::ffi::OsStr::new("b"),
                Path::new("/d"),
                "the destination is the source or lies inside it"
            ),
            "error: root b: /d/b: the destination is the source or lies inside it; nothing was verified"
        );
        assert_eq!(
            root_absent_line(Path::new("/d"), std::ffi::OsStr::new("b")),
            "destination does not exist: /d/b (every source path is reported missing)"
        );
        assert_eq!(
            refused_alias_line(std::ffi::OsStr::new("a"), std::ffi::OsStr::new("b")),
            "error: roots a and b are one directory on this filesystem; nothing was verified"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stderr_lines_escape_the_root_name() {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::OsStr::new("x\ny");
        let raw = std::ffi::OsStr::from_bytes(b"z\xffw");
        let s = Summary::default();
        let lines = [
            root_line(name, &s),
            root_absent_line(Path::new("/d"), name),
            refused_root_line(name, Path::new("/d"), "r"),
            refused_alias_line(name, raw),
        ];
        for l in &lines {
            assert!(!l.contains('\n'), "{l:?}");
            assert!(l.contains("x\\ny"), "{l:?}");
        }
        assert!(lines[3].contains("z\\xffw"), "{:?}", lines[3]);
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

    #[test]
    fn the_summary_json_has_excluded_only_when_non_zero_between_not_entered_and_bytes_hashed() {
        let mut s = Summary { matched: 1, not_entered: 2, bytes_hashed: 7, ..Summary::default() };
        let text = summary_json(Sources::One(Path::new("/s")), Path::new("/d"), "blake3", &s, 0);
        assert!(!text.contains("\"excluded\""), "{text}");
        s.excluded = 4;
        let text = summary_json(Sources::One(Path::new("/s")), Path::new("/d"), "blake3", &s, 0);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["summary"]["excluded"], 4);
        let at = |k: &str| text.find(&format!("\"{k}\":")).unwrap();
        assert!(
            at("not_entered") < at("excluded") && at("excluded") < at("bytes_hashed"),
            "{text}"
        );
    }

    #[test]
    fn the_summary_line_has_the_excluded_clause_after_the_mount_clause() {
        let mut s = Summary { matched: 3, bytes_hashed: 9, ..Summary::default() };
        assert!(!summary_line(&s).contains("excluded"));
        s.excluded = 5;
        assert_eq!(
            summary_line(&s),
            "verified: 3 matched, 0 missing, 0 mismatched, 0 extra, 0 unreadable; 9 bytes hashed; 5 excluded"
        );
        s.not_entered = 2;
        assert_eq!(
            summary_line(&s),
            "verified: 3 matched, 0 missing, 0 mismatched, 0 extra, 0 unreadable; 9 bytes hashed; 2 mount points not entered; 5 excluded"
        );
    }

    fn mount(path: &str) -> Record {
        rec(path, Outcome::Match, None, MOUNT_NOT_ENTERED)
    }

    fn lost(path: &str) -> Record {
        rec(path, Outcome::Extra, Some(Code::PermissionDenied), "Permission denied (os error 13)")
    }

    #[test]
    fn a_mount_point_not_entered_always_gets_its_stderr_notice() {
        let m = mount("m");
        let notice = mount_notice_line(Path::new("m"));
        let human = route_record(&m, false, false, false);
        assert_eq!(human, Routed { stdout: None, stderr: vec![notice.clone()] });
        let verbose = route_record(&m, false, true, false);
        assert_eq!(
            verbose.stdout.as_deref(),
            Some(format!("match: m: {MOUNT_NOT_ENTERED}").as_str())
        );
        assert_eq!(verbose.stderr, std::slice::from_ref(&notice));
        let json = route_record(&m, true, false, false);
        assert_eq!(json.stdout, Some(record_json(&m)));
        assert_eq!(json.stderr, [notice]);
    }

    #[test]
    fn quiet_drops_match_and_extra_stdout_but_keeps_the_rest() {
        for outcome in [Outcome::Match, Outcome::Extra] {
            let r = rec("a", outcome, None, "");
            assert_eq!(route_record(&r, false, true, true).stdout, None);
            assert_eq!(route_record(&r, true, false, true).stdout, Some(record_json(&r)));
        }
        for outcome in [Outcome::Missing, Outcome::Mismatched] {
            let r = rec("a", outcome, None, "");
            assert_eq!(route_record(&r, false, false, true).stdout, Some(record_line(&r)));
        }
        let u = rec("u", Outcome::Unreadable, Some(Code::PermissionDenied), "x");
        assert_eq!(route_record(&u, false, false, true).stdout, Some(record_line(&u)));
        let l = lost("lost+found");
        let q = route_record(&l, false, false, true);
        assert_eq!(q.stdout, None);
        assert_eq!(q.stderr.len(), 1, "the unreadable-extra warning is kept");
        assert_eq!(route_record(&mount("m"), false, false, true).stderr.len(), 1);
    }

    #[test]
    fn a_match_is_hidden_in_human_mode_unless_verbose_and_always_shown_as_json() {
        let m = rec("a", Outcome::Match, None, "");
        assert_eq!(route_record(&m, false, false, false), Routed { stdout: None, stderr: vec![] });
        assert_eq!(route_record(&m, false, true, false).stdout.as_deref(), Some("match: a"));
        assert_eq!(route_record(&m, true, false, false).stdout, Some(record_json(&m)));
        let x = rec("b", Outcome::Missing, None, "");
        assert_eq!(route_record(&x, false, false, false).stdout.as_deref(), Some("missing: b"));
        assert!(route_record(&x, false, false, false).stderr.is_empty());
    }

    #[test]
    fn an_unlistable_destination_only_directory_is_an_extra_line_plus_a_stderr_warning() {
        let r = lost("lost+found");
        let want = "warning: could not read destination-only directory: lost+found: Permission denied (os error 13)";
        assert_eq!(unreadable_extra_warning_line(&r).as_deref(), Some(want));
        let human = route_record(&r, false, false, false);
        assert_eq!(
            human.stdout.as_deref(),
            Some("extra: lost+found: PERMISSION_DENIED: Permission denied (os error 13)")
        );
        assert_eq!(human.stderr, [want]);
        assert_eq!(route_record(&r, true, false, false).stderr, [want]);
        // A plain extra, and an unreadable that has a code, get no such warning.
        assert_eq!(unreadable_extra_warning_line(&rec("e", Outcome::Extra, None, "")), None);
        let u = rec("u", Outcome::Unreadable, Some(Code::PermissionDenied), "x");
        assert_eq!(unreadable_extra_warning_line(&u), None);
        assert!(route_record(&u, false, false, false).stderr.is_empty());
    }

    #[cfg(unix)]
    fn raw(bytes: &[u8]) -> PathBuf {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }

    #[test]
    fn an_ordinary_name_is_shown_unchanged() {
        for name in ["a", "sub/b.txt", "sp ace", "x\u{e9}/\u{4e2d}", "a-b.c"] {
            assert_eq!(display_path(Path::new(name)), name);
        }
        assert_eq!(display_path(Path::new("")), ".");
        assert_eq!(display_path(Path::new(".")), ".");
    }

    #[cfg(unix)]
    #[test]
    fn names_differing_only_in_an_invalid_byte_are_shown_differently() {
        let a = raw(b"bad\xff\nname");
        let b = raw(b"bad\xfe\nname");
        assert_eq!(display_path(&a), "bad\\xff\\nname");
        assert_eq!(display_path(&b), "bad\\xfe\\nname");
        let ra = rec_path(a);
        let rb = rec_path(b);
        assert_ne!(record_line(&ra), record_line(&rb));
        let ja: serde_json::Value = serde_json::from_str(&record_json(&ra)).unwrap();
        let jb: serde_json::Value = serde_json::from_str(&record_json(&rb)).unwrap();
        assert_ne!(ja["path"], jb["path"]);
        assert_eq!(ja["path"], "bad\\xff\\nname");
    }

    #[cfg(unix)]
    fn rec_path(path: PathBuf) -> Record {
        Record { path, ..rec("", Outcome::Extra, None, "") }
    }

    #[cfg(unix)]
    #[test]
    fn a_newline_or_other_control_character_never_splits_a_line() {
        let r = rec_path(raw(b"a\nb\rc\td\x01e\x7ff/g\nh"));
        let line = record_line(&r);
        assert_eq!(line, "extra: a\\nb\\rc\\td\\x01e\\x7ff/g\\nh");
        assert_eq!(line.lines().count(), 1);
        let json = record_json(&r);
        assert_eq!(json.lines().count(), 1);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["path"], "a\\nb\\rc\\td\\x01e\\x7ff/g\\nh");
    }

    #[cfg(unix)]
    #[test]
    fn a_backslash_is_doubled_so_the_escaped_form_is_unambiguous() {
        // A literal backslash-n and a real newline must not print alike.
        let lit = rec_path(raw(b"a\\nb"));
        let nl = rec_path(raw(b"a\nb"));
        assert_eq!(record_line(&lit), "extra: a\\\\nb");
        assert_eq!(record_line(&nl), "extra: a\\nb");
        // serde escapes the backslash once more, which is correct: the value parses back to the display form.
        let json = record_json(&lit);
        assert!(json.contains("a\\\\\\\\nb"), "{json}");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["path"], "a\\\\nb");
    }

    #[cfg(unix)]
    #[test]
    fn a_multibyte_name_cut_short_escapes_only_the_broken_bytes() {
        // U+00E9 is C3 A9: the lone C3 is invalid, a valid pair after it is kept.
        assert_eq!(display_path(&raw(b"x\xc3y\xc3\xa9")), "x\\xc3y\u{e9}");
        assert_eq!(display_path(&raw(b"end\xe2\x82")), "end\\xe2\\x82");
    }

    #[cfg(unix)]
    #[test]
    fn the_notices_use_the_escaped_path_too() {
        let p = raw(b"m\nn");
        assert_eq!(
            mount_notice_line(&p),
            "mount point not entered: m\\nn (use --cross-filesystems to verify its contents)"
        );
        let mut r = lost("x");
        r.path = p;
        assert!(unreadable_extra_warning_line(&r).unwrap().contains("m\\nn"));
    }
}
