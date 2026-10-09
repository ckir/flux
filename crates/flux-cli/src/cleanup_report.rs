//! What `flux cleanup` prints (cut 9b): the table, the action lines, the summary and the JSON report. Pure: every
//! function returns text or a value, and `main` writes it.

use flux_core::cleanup::{Action, CleanupReport, Entry, Refused};
use serde::Serialize;
use std::path::Path;

/// Column widths of the header; the last column (NOTE) is unpadded.
const W_STATUS: usize = 23;
const W_ELIGIBLE: usize = 10;
const W_KIND: usize = 11;
const W_ID: usize = 20;
const W_STATE: usize = 14;
const W_AGE: usize = 6;

/// `-` when unknown, otherwise `<n>d`, `<n>h`, `<n>m` or `<n>s`: the largest unit with a non-zero count.
pub fn age_text(seconds: Option<u64>) -> String {
    let Some(s) = seconds else { return "-".to_owned() };
    for (unit, size) in [('d', 86_400), ('h', 3_600), ('m', 60)] {
        if s >= size {
            return format!("{}{unit}", s / size);
        }
    }
    format!("{s}s")
}

/// `text` padded to `width`; a longer text is never cut, and is still followed by one space.
fn pad(text: &str, width: usize) -> String {
    format!("{text:<width$}{}", if text.chars().count() >= width { " " } else { "" })
}

pub fn header_line() -> String {
    format!(
        "{}{}{}{}{}{}NOTE",
        pad("STATUS", W_STATUS),
        pad("ELIGIBLE", W_ELIGIBLE),
        pad("KIND", W_KIND),
        pad("ID", W_ID),
        pad("STATE", W_STATE),
        pad("AGE", W_AGE),
    )
}

pub fn entry_line(e: &Entry) -> String {
    format!(
        "{}{}{}{}{}{}{}",
        pad(e.status.as_str(), W_STATUS),
        pad(if e.eligible { "yes" } else { "no" }, W_ELIGIBLE),
        pad(e.kind.as_str(), W_KIND),
        pad(&e.id, W_ID),
        pad(e.state.map_or("-", |s| s.as_str()), W_STATE),
        pad(&age_text(e.age_seconds), W_AGE),
        e.note,
    )
}

pub fn action_line(a: &Action) -> String {
    match a {
        Action::Removed(path) => format!("removed {}", path.display()),
        Action::Kept { path, error } => format!("kept {}: {error}", path.display()),
        Action::Skipped { id, reason } => format!("skipped {id}: {reason}"),
    }
}

pub fn summary_line(r: &CleanupReport, dry_run: bool) -> String {
    let s = r.summary();
    if dry_run {
        format!("cleanup (dry run): {} entries, {} eligible", s.entries, s.eligible)
    } else {
        format!(
            "cleanup: {} entries, {} removed, {} kept, {} skipped",
            s.entries, s.removed, s.kept, s.skipped
        )
    }
}

pub fn refused_line(r: &Refused) -> String {
    format!("{}: {}", r.code, r.detail)
}

/// The cleanup command's own report. Fields are declared in the spec's order: serde emits them in it (a
/// `serde_json::Value` would sort the keys). Absent values are `null`, never omitted.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CleanupJson {
    pub destination: String,
    pub dry_run: bool,
    pub entries: Vec<EntryJson>,
    pub actions: Vec<ActionJson>,
    pub summary: SummaryJson,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct EntryJson {
    pub status: &'static str,
    pub eligible: bool,
    pub kind: &'static str,
    pub id: String,
    pub state: Option<&'static str>,
    pub age_seconds: Option<u64>,
    pub note: String,
}

/// `path` of a `skipped` action holds the row's id (a skipped row has no path of its own).
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ActionJson {
    pub action: &'static str,
    pub path: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SummaryJson {
    pub entries: u64,
    pub eligible: u64,
    pub removed: u64,
    pub kept: u64,
    pub skipped: u64,
}

pub fn json(destination: &Path, dry_run: bool, r: &CleanupReport) -> CleanupJson {
    let s = r.summary();
    CleanupJson {
        destination: destination.display().to_string(),
        dry_run,
        entries: r
            .entries
            .iter()
            .map(|e| EntryJson {
                status: e.status.as_str(),
                eligible: e.eligible,
                kind: e.kind.as_str(),
                id: e.id.clone(),
                state: e.state.map(|s| s.as_str()),
                age_seconds: e.age_seconds,
                note: e.note.clone(),
            })
            .collect(),
        actions: r
            .actions
            .iter()
            .map(|a| match a {
                Action::Removed(p) => ActionJson {
                    action: "removed",
                    path: Some(p.display().to_string()),
                    reason: None,
                },
                Action::Kept { path, error } => ActionJson {
                    action: "kept",
                    path: Some(path.display().to_string()),
                    reason: Some(error.to_string()),
                },
                Action::Skipped { id, reason } => ActionJson {
                    action: "skipped",
                    path: Some(id.clone()),
                    reason: Some(reason.clone()),
                },
            })
            .collect(),
        summary: SummaryJson {
            entries: s.entries,
            eligible: s.eligible,
            removed: s.removed,
            kept: s.kept,
            skipped: s.skipped,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::cleanup::{EntryKind, Status};
    use flux_core::state::OpState;
    use flux_fs::{Code, FsError};
    use std::path::PathBuf;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn entry() -> Entry {
        Entry {
            kind: EntryKind::Operation,
            id: ID.to_owned(),
            status: Status::CompletedButUnclean,
            eligible: true,
            state: Some(OpState::Completed),
            age_seconds: Some(172_800),
            note: "2 leftovers".to_owned(),
        }
    }

    fn kept() -> Action {
        Action::Kept {
            path: PathBuf::from("/d/x"),
            error: FsError::new(Code::IoError, std::io::Error::other("busy")),
        }
    }

    fn report() -> CleanupReport {
        CleanupReport {
            entries: vec![entry(), entry()],
            actions: vec![
                Action::Removed(PathBuf::from("/d/a")),
                Action::Removed(PathBuf::from("/d/b")),
                Action::Removed(PathBuf::from("/d/c")),
                kept(),
            ],
            ..CleanupReport::default()
        }
    }

    #[test]
    fn age_text_picks_the_largest_unit() {
        let t = |s| age_text(Some(s));
        assert_eq!(age_text(None), "-");
        assert_eq!(t(0), "0s");
        assert_eq!(t(59), "59s");
        assert_eq!(t(60), "1m");
        assert_eq!(t(3_599), "59m");
        assert_eq!(t(3_600), "1h");
        assert_eq!(t(86_400), "1d");
        assert_eq!(t(604_800), "7d");
    }

    #[test]
    fn an_entry_line_has_the_specs_columns() {
        let h = header_line();
        assert_eq!(
            h,
            "STATUS                 ELIGIBLE  KIND       ID                  STATE         AGE   NOTE"
        );
        let l = entry_line(&entry());
        for name in ["ELIGIBLE", "KIND", "ID"] {
            let at = h.find(name).unwrap();
            assert!(l.is_char_boundary(at) && !l[..at].ends_with(|c: char| c != ' '));
            assert_ne!(l.as_bytes()[at], b' ', "{name} column starts at {at}: {l}");
        }
        assert!(l.starts_with("COMPLETED_BUT_UNCLEAN  yes       operation  "));
        assert_eq!(l.find(ID), h.find("ID"));
        // A 32-hex id is longer than the ID column and is never cut; the columns after it follow.
        let rest: Vec<&str> = l[h.find("ID").unwrap() + ID.len()..].split_whitespace().collect();
        assert_eq!(rest, ["COMPLETED", "2d", "2", "leftovers"]);
        // A short id keeps every column start.
        let mut short = entry();
        short.id = "abc.creating".to_owned();
        let l = entry_line(&short);
        for name in ["STATE", "AGE"] {
            let at = h.find(name).unwrap();
            assert_ne!(l.as_bytes()[at], b' ', "{name} at {at}: {l}");
            assert_eq!(l.as_bytes()[at - 1], b' ');
        }
        assert_eq!(l.find("2 leftovers"), h.find("NOTE"));
    }

    #[test]
    fn action_lines_are_the_specs_three_shapes() {
        assert_eq!(action_line(&Action::Removed(PathBuf::from("/d/a"))), "removed /d/a");
        assert_eq!(action_line(&kept()), "kept /d/x: IO_ERROR: busy");
        let s = Action::Skipped { id: "i".into(), reason: "gone".into() };
        assert_eq!(action_line(&s), "skipped i: gone");
    }

    #[test]
    fn the_summary_line_counts_and_dry_run_differs() {
        let mut r = report();
        r.actions.pop();
        r.actions.push(kept());
        r.entries[1].eligible = false;
        // 2 entries, 3 removed, 1 kept, 0 skipped
        assert_eq!(summary_line(&r, false), "cleanup: 2 entries, 3 removed, 1 kept, 0 skipped");
        assert_eq!(summary_line(&r, true), "cleanup (dry run): 2 entries, 1 eligible");
    }

    #[test]
    fn a_refusal_prints_its_code_and_detail() {
        let r = Refused { code: "DESTINATION_ERROR", detail: "not a directory".into() };
        assert_eq!(refused_line(&r), "DESTINATION_ERROR: not a directory");
    }

    /// Byte offsets of each key in `s`, which must be strictly increasing.
    fn in_order(s: &str, keys: &[&str]) {
        let mut at = 0;
        for k in keys {
            let found = s[at..]
                .find(&format!("\"{k}\":"))
                .unwrap_or_else(|| panic!("{k} after {at} in {s}"));
            at += found + k.len();
        }
    }

    #[test]
    fn the_json_has_the_specs_keys() {
        let mut r = report();
        r.entries[1].state = None;
        r.entries[1].age_seconds = None;
        r.actions.push(Action::Skipped { id: "i".into(), reason: "gone".into() });
        let s = serde_json::to_string(&json(Path::new("/d"), false, &r)).unwrap();
        assert!(!s.contains('\n'));
        in_order(&s, &["destination", "dry_run", "entries", "actions", "summary"]);
        // The first entry's keys, then the first action's, then the summary's, each in the spec's order.
        let entries = &s[s.find("\"entries\":").unwrap()..s.find("\"actions\":").unwrap()];
        in_order(entries, &["status", "eligible", "kind", "id", "state", "age_seconds", "note"]);
        let actions = &s[s.find("\"actions\":").unwrap()..s.find("\"summary\":").unwrap()];
        in_order(actions, &["action", "path", "reason"]);
        in_order(
            &s[s.find("\"summary\":").unwrap()..],
            &["entries", "eligible", "removed", "kept", "skipped"],
        );

        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["destination"], "/d");
        assert_eq!(v["dry_run"], false);
        assert_eq!(v["entries"][0]["status"], "COMPLETED_BUT_UNCLEAN");
        assert_eq!(v["entries"][0]["state"], "COMPLETED");
        assert_eq!(v["entries"][0]["age_seconds"], 172_800);
        assert!(v["entries"][1]["state"].is_null() && v["entries"][1]["age_seconds"].is_null());
        assert_eq!(v["entries"][0].as_object().unwrap().len(), 7);
        assert!(v["actions"][0]["reason"].is_null());
        assert_eq!(v["actions"][3]["action"], "kept");
        assert_eq!(v["actions"][3]["reason"], "IO_ERROR: busy");
        assert_eq!(v["actions"][4]["action"], "skipped");
        assert_eq!(v["actions"][4]["path"], "i");
        assert_eq!(
            v["summary"],
            serde_json::json!({"entries":2,"eligible":2,"removed":3,"kept":1,"skipped":1})
        );
    }
}
