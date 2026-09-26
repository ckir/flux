//! The recommended-tools list and the tooling doc must agree.
//!
//! `CONTRIBUTING.md` tells a contributor that the tooling table in `docs/dev-tooling.md` "is
//! machine-checked from `.claude/recommended-tools.json`". Until this test existed, that sentence
//! was not true: nothing in the justfile, the workflows or the test suite read that JSON, and four
//! tools it declares (`java`, `python3`, `tla2tools.jar`, `gh`) had drifted out of the doc entirely.
//!
//! The JSON is the machine-readable source - the SessionStart hook reads it to tell an agent which
//! prerequisites are missing - so the invariant is one-directional: **every tool the JSON declares
//! must appear in the doc**. The reverse is deliberately NOT checked, because the doc legitimately
//! covers more: components that ship with the toolchain (`rustfmt`, `clippy`), the installer that
//! fetches the rest (`cargo-binstall`), and candidates that are merely installed and not required.
//! Demanding a JSON entry for those would push noise into the file the hook acts on.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[derive(serde::Deserialize)]
struct Tool {
    name: String,
    #[serde(default)]
    why: String,
    #[serde(default)]
    install: String,
    #[serde(default)]
    in_path: Option<String>,
    #[serde(default)]
    file_exists: Option<FileExists>,
}

/// The hook accepts `file_exists` as one path or a list of paths.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum FileExists {
    One(String),
    Many(Vec<String>),
}

/// Every name the doc documents: the backticked tokens in the first cell of each table row.
///
/// One cell can name several tools (`` `rustup` / `rustc` / `cargo` ``) or carry a parenthetical
/// (`` `typos` (typos-cli) ``), so every backticked span in the cell counts. Header and separator
/// rows carry no backticks and so drop out without a special case.
fn documented(doc: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for line in doc.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let Some(cell) = line[1..].split('|').next() else {
            continue;
        };
        let mut rest = cell;
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('`') else { break };
            let name = after[..close].trim();
            if !name.is_empty() {
                found.insert(name.to_string());
            }
            rest = &after[close + 1..];
        }
    }
    found
}

/// The generated block's first line. The markers are the only part of the doc the generator owns.
const BEGIN: &str = "<!-- tools:begin - generated from .claude/recommended-tools.json by `just tools-doc`; do not edit by hand -->";
const END: &str = "<!-- tools:end -->";

/// One markdown code span, refusing a value that would break out of it. A table cell cannot hold
/// a raw `|`, so it is escaped; a backtick would end the span early, and there is no value in the
/// JSON today that needs one, so it is refused rather than half-handled.
fn code(value: &str) -> String {
    assert!(
        !value.contains('`'),
        "recommended-tools.json value {value:?} contains a backtick, which the generated table \
         cannot render inside a code span"
    );
    // A line break inside a cell ends the table row, and with it the table (capstone round 1).
    assert!(
        !value.contains(['\n', '\r']),
        "recommended-tools.json value {value:?} contains a line break, which would end the \
         generated table mid-row"
    );
    format!("`{}`", value.replace('|', "\\|"))
}

/// The generated block, markers included, with LF line endings - exactly what `just tools-doc`
/// writes. Columns are only what the JSON factually holds and the SessionStart hook enforces.
fn render_table(tools: &[Tool]) -> String {
    let mut out = String::new();
    out.push_str(BEGIN);
    out.push('\n');
    out.push_str("| Tool | Install | Checked by |\n|---|---|---|\n");
    for t in tools {
        let mut checks = Vec::new();
        if let Some(cmd) = &t.in_path {
            checks.push(format!("on `PATH` as {}", code(cmd)));
        }
        match &t.file_exists {
            Some(FileExists::One(p)) => checks.push(format!("file {}", code(p))),
            Some(FileExists::Many(ps)) => {
                checks.extend(ps.iter().map(|p| format!("file {}", code(p))))
            }
            None => {}
        }
        let checked =
            if checks.is_empty() { "not checked".to_string() } else { checks.join(" or ") };
        out.push_str(&format!("| {} | {} | {} |\n", code(&t.name), code(&t.install), checked));
    }
    out.push_str(END);
    out.push('\n');
    out
}

/// Where the generated block sits: byte offsets of its start and of the first byte after it (its
/// trailing newline included). Refuses no marker pair, more than one, or an end before a begin -
/// a check that silently found no block would compare clean against nothing.
fn block_span(doc: &str) -> Result<(usize, usize), String> {
    let (begins, ends) = (doc.matches(BEGIN).count(), doc.matches(END).count());
    if begins != 1 || ends != 1 {
        return Err(format!(
            "docs/dev-tooling.md must contain exactly one generated-table marker pair; found \
             {begins} begin and {ends} end markers"
        ));
    }
    let (b, e) = (doc.find(BEGIN).unwrap(), doc.find(END).unwrap());
    if e < b {
        return Err("docs/dev-tooling.md's tools:end marker comes before tools:begin".to_string());
    }
    let mut after = e + END.len();
    if doc[after..].starts_with('\n') {
        after += 1;
    }
    Ok((b, after))
}

/// `doc` (line endings normalized to LF) with its generated block replaced by `block`.
fn splice(doc: &str, block: &str) -> Result<String, String> {
    let doc = doc.replace("\r\n", "\n");
    let (b, after) = block_span(&doc)?;
    Ok(format!("{}{}{}", &doc[..b], block, &doc[after..]))
}

/// `doc` with the generated block removed, so the one-way check reads ONLY the hand tables.
fn without_generated(doc: &str) -> Result<String, String> {
    splice(doc, "")
}

/// Why the SessionStart hook would silently SKIP this entry, if it would. The hook drops an entry
/// whose `name`, `why` or `install` is empty ("required string fields"), and one with neither an
/// `in_path` nor a non-empty `file_exists` ("not-evaluable"; an empty `in_path` string counts as
/// none) - `recommended-tooling-check.sh`. The table must not list such an entry at all: the hook
/// ignores it, so the JSON line is dead weight that reads as a requirement (capstone rounds 1-2).
fn hook_skips(t: &Tool) -> Option<&'static str> {
    let has_in_path = t.in_path.as_deref().is_some_and(|p| !p.is_empty());
    let has_file = match &t.file_exists {
        Some(FileExists::One(_)) => true,
        Some(FileExists::Many(ps)) => !ps.is_empty(),
        None => false,
    };
    match () {
        _ if t.name.is_empty() => Some("no `name`"),
        _ if t.why.is_empty() => Some("no `why`"),
        _ if t.install.is_empty() => Some("no `install`"),
        _ if !has_in_path && !has_file => Some("neither `in_path` nor `file_exists`"),
        _ => None,
    }
}

fn load_tools() -> Vec<Tool> {
    let json = std::fs::read_to_string(repo().join(".claude/recommended-tools.json"))
        .expect("read .claude/recommended-tools.json");
    let tools: Vec<Tool> = serde_json::from_str(&json).expect("parse recommended-tools.json");
    assert!(!tools.is_empty(), "recommended-tools.json declares no tools");
    let skipped: Vec<String> = tools
        .iter()
        .filter_map(|t| hook_skips(t).map(|why| format!("{:?} ({why})", t.name)))
        .collect();
    assert!(
        skipped.is_empty(),
        "the SessionStart hook silently skips these recommended-tools.json entries, so neither the \
         hook nor this doc may treat them as checked: {}",
        skipped.join(", ")
    );
    tools
}

#[test]
fn every_recommended_tool_is_documented() {
    let root = repo();
    let tools = load_tools();

    let doc_path = root.join("docs/dev-tooling.md");
    let doc = std::fs::read_to_string(&doc_path).expect("read docs/dev-tooling.md");
    // The generated table names every JSON tool in its first cell; reading it here would make this
    // check pass while checking nothing about the hand-written tables.
    let hand = without_generated(&doc).unwrap_or_else(|e| panic!("{e}"));
    let documented = documented(&hand);
    assert!(
        !documented.is_empty(),
        "no backticked tool names found in any table row of docs/dev-tooling.md - the table shape \
         this test reads has changed, so the check is no longer checking anything"
    );

    let missing: Vec<&str> =
        tools.iter().map(|t| t.name.as_str()).filter(|name| !documented.contains(*name)).collect();

    assert!(
        missing.is_empty(),
        "these tools are declared in .claude/recommended-tools.json but no table row in \
         docs/dev-tooling.md names them: {}\n\
         Add a row for each, or drop it from the JSON. CONTRIBUTING.md promises a contributor that \
         these two files agree.",
        missing.join(", ")
    );
}

/// The promise, not just the thing promised.
///
/// `README.md` sends a reader to `docs/dev-tooling.md`, and `CONTRIBUTING.md` tells them that file
/// "is machine-checked from `.claude/recommended-tools.json`". Renaming or moving either file would
/// leave those sentences pointing at nothing while this test above still passed - the prose would go
/// quietly false again, which is the exact failure this file exists to end. So pin the promise to
/// the paths the check actually reads.
#[test]
fn the_prose_still_points_at_the_files_this_checks() {
    let root = repo();
    for (doc, must_mention) in [
        ("README.md", &["docs/dev-tooling.md"][..]),
        ("CONTRIBUTING.md", &["docs/dev-tooling.md", ".claude/recommended-tools.json"][..]),
    ] {
        let text =
            std::fs::read_to_string(root.join(doc)).unwrap_or_else(|e| panic!("read {doc}: {e}"));
        for path in must_mention {
            assert!(
                text.contains(path),
                "{doc} no longer mentions `{path}`, but tests/dev_tooling.rs still checks that \
                 file. Either restore the reference or update this test - do not leave the two \
                 disagreeing."
            );
        }
    }
}

#[test]
fn the_generated_tool_table_is_current() {
    let doc = std::fs::read_to_string(repo().join("docs/dev-tooling.md"))
        .expect("read docs/dev-tooling.md");
    let want = splice(&doc, &render_table(&load_tools())).unwrap_or_else(|e| panic!("{e}"));
    // core.autocrlf=true checks the doc out with CRLF; that alone is not drift.
    assert!(
        doc.replace("\r\n", "\n") == want,
        "docs/dev-tooling.md's generated tool table is out of date with \
         .claude/recommended-tools.json. Run: just tools-doc"
    );
}

#[test]
#[ignore = "rewrites docs/dev-tooling.md; run through `just tools-doc`"]
fn rewrite_tool_table() {
    let path = repo().join("docs/dev-tooling.md");
    let doc = std::fs::read_to_string(&path).expect("read docs/dev-tooling.md");
    let want = splice(&doc, &render_table(&load_tools())).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(&path, want).expect("write docs/dev-tooling.md");
}

fn tool(name: &str, install: &str, in_path: Option<&str>, file: Option<FileExists>) -> Tool {
    Tool {
        name: name.into(),
        why: "a reason".into(),
        install: install.into(),
        in_path: in_path.map(Into::into),
        file_exists: file,
    }
}

#[test]
fn render_table_names_each_tool_its_install_and_its_check() {
    let got = render_table(&[
        tool("just", "cargo binstall -y just", Some("just"), None),
        tool("jar", "fetch it", None, Some(FileExists::One("target/x.jar".into()))),
        tool("both", "a | b", Some("b"), Some(FileExists::Many(vec!["p".into(), "q".into()]))),
        tool("none", "manual", None, None),
    ]);
    let want = format!(
        "{BEGIN}\n| Tool | Install | Checked by |\n|---|---|---|\n\
         | `just` | `cargo binstall -y just` | on `PATH` as `just` |\n\
         | `jar` | `fetch it` | file `target/x.jar` |\n\
         | `both` | `a \\| b` | on `PATH` as `b` or file `p` or file `q` |\n\
         | `none` | `manual` | not checked |\n{END}\n"
    );
    assert_eq!(got, want);
}

#[test]
#[should_panic(expected = "contains a backtick")]
fn render_table_refuses_a_backtick() {
    render_table(&[tool("x", "run `this`", Some("x"), None)]);
}

#[test]
#[should_panic(expected = "contains a line break")]
fn render_table_refuses_a_line_break() {
    render_table(&[tool("x", "first\nsecond", Some("x"), None)]);
}

#[test]
fn an_entry_the_hook_would_skip_is_named() {
    assert_eq!(hook_skips(&tool("x", "i", Some("x"), None)), None);
    assert_eq!(hook_skips(&tool("", "i", Some("x"), None)), Some("no `name`"));
    assert_eq!(hook_skips(&tool("x", "", Some("x"), None)), Some("no `install`"));
    let mut no_why = tool("x", "i", Some("x"), None);
    no_why.why.clear();
    assert_eq!(hook_skips(&no_why), Some("no `why`"));
    let none = Some("neither `in_path` nor `file_exists`");
    assert_eq!(hook_skips(&tool("x", "i", None, None)), none);
    assert_eq!(hook_skips(&tool("x", "i", Some(""), None)), none, "an empty in_path is none");
    assert_eq!(hook_skips(&tool("x", "i", None, Some(FileExists::Many(vec![])))), none);
    assert_eq!(hook_skips(&tool("x", "i", None, Some(FileExists::One("f".into())))), None);
}

#[test]
fn splice_replaces_only_the_block_and_normalizes_crlf() {
    let doc = format!("before\r\n{BEGIN}\r\nstale\r\n{END}\r\nafter\r\n");
    assert_eq!(splice(&doc, "NEW\n").unwrap(), "before\nNEW\nafter\n");
}

#[test]
fn splice_refuses_a_missing_duplicated_or_reversed_marker_pair() {
    for doc in [
        "no markers at all\n".to_string(),
        format!("{BEGIN}\n{END}\n{BEGIN}\n{END}\n"),
        format!("{BEGIN}\nonly a begin\n"),
        format!("{END}\n{BEGIN}\n"),
    ] {
        assert!(splice(&doc, "x\n").is_err(), "must refuse: {doc:?}");
    }
}

#[test]
fn the_hand_table_check_does_not_read_the_generated_block() {
    // A doc whose ONLY mention of `ghost` is inside the generated block: the one-way check must
    // not count it as documented.
    let doc = format!("| `real` | x |\n{BEGIN}\n| `ghost` | y |\n{END}\n");
    let hand = documented(&without_generated(&doc).unwrap());
    assert!(hand.contains("real"));
    assert!(!hand.contains("ghost"), "the generated block leaked into the hand-table check");
}
