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

#[test]
fn every_recommended_tool_is_documented() {
    let root = repo();
    let json = std::fs::read_to_string(root.join(".claude/recommended-tools.json"))
        .expect("read .claude/recommended-tools.json");
    let tools: Vec<Tool> = serde_json::from_str(&json).expect("parse recommended-tools.json");
    assert!(!tools.is_empty(), "recommended-tools.json declares no tools");

    let doc_path = root.join("docs/dev-tooling.md");
    let doc = std::fs::read_to_string(&doc_path).expect("read docs/dev-tooling.md");
    let documented = documented(&doc);
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
