//! Model conformance (cut 7a design, "Model conformance"): every protocol label of the lock model maps to the Rust
//! function that implements it - a function that exists and mentions the label - or says why none does; and no code
//! mentions a label marked for a later cut.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const FAMILIES: [&str; 7] =
    ["S96_1_", "S240_1_", "S240_3_", "S240_5_", "S99_", "S21_1_", "S251_1_"];
const STATUSES: [&str; 4] = ["part-3", "seed-only", "model-only", "not-in-7a"];

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Every `<label>:` line of the PlusCal algorithm in a protocol family.
fn model_labels() -> BTreeSet<String> {
    read("models/lockproto/algorithm.txt")
        .lines()
        .filter_map(|line| {
            let label = line.trim().strip_suffix(':')?;
            let ok = FAMILIES.iter().any(|f| label.starts_with(f))
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            ok.then(|| label.to_string())
        })
        .collect()
}

fn entries() -> toml::Table {
    toml::from_str(&read("models/lockproto/impl-map.toml")).expect("impl-map.toml parses")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// `label` appears in `text` as a whole label: a prefix of a longer label (`S240_3_s4_lock` inside
/// `S240_3_s4_lock_close`) does not count (capstone round 2, cut 7a Part 2).
fn mentions_label(text: &str, label: &str) -> bool {
    text.match_indices(label).any(|(i, _)| {
        let after = text[i + label.len()..].chars().next();
        let before = text[..i].chars().next_back();
        let part = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        !part(after) && !part(before)
    })
}

/// The text of `fn name` in `src`: its doc comment and attributes, its signature, and its body up to the closing
/// brace at the same indentation. `None` when the file defines no such function.
fn function_region(src: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = src.lines().collect();
    let def = lines.iter().position(|l| {
        let t = l.trim_start();
        let t = t.strip_prefix("pub(crate) ").or_else(|| t.strip_prefix("pub ")).unwrap_or(t);
        t.starts_with(&format!("fn {name}(")) || t.starts_with(&format!("fn {name}<"))
    })?;
    let indent = &lines[def][..lines[def].len() - lines[def].trim_start().len()];
    let mut start = def;
    while start > 0 {
        let above = lines[start - 1].trim_start();
        if above.starts_with("///") || above.starts_with("#[") {
            start -= 1;
        } else {
            break;
        }
    }
    let closing = format!("{indent}}}");
    let end = (def..lines.len()).find(|&i| lines[i] == closing)?;
    Some(lines[start..=end].join(
        "
",
    ))
}

#[test]
fn every_protocol_label_has_exactly_one_entry() {
    let labels = model_labels();
    assert!(
        labels.len() >= 50,
        "found only {} labels: the label parse no longer matches algorithm.txt",
        labels.len()
    );
    let keys: BTreeSet<String> = entries().keys().cloned().collect();
    let missing: Vec<_> = labels.difference(&keys).collect();
    let extra: Vec<_> = keys.difference(&labels).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "no entry for {missing:?}; no such label for {extra:?}"
    );
}

#[test]
fn every_entry_names_a_function_that_mentions_its_label_or_a_reason() {
    for (label, value) in entries() {
        let t = value.as_table().unwrap_or_else(|| panic!("{label}: not a table"));
        let item = t.get("item").and_then(|v| v.as_str());
        let status = t.get("status").and_then(|v| v.as_str());
        match (item, status) {
            (Some(item), None) => {
                let (file, func) = item
                    .split_once("::")
                    .unwrap_or_else(|| panic!("{label}: item {item} has no ::"));
                let src = read(file);
                let region = function_region(&src, func)
                    .unwrap_or_else(|| panic!("{label}: {file} defines no fn {func}"));
                // The function's own doc comment and body, not merely the file: another function in the same file
                // mentioning the label must not stand in for it (capstone round 1, cut 7a Part 2).
                assert!(
                    mentions_label(&region, &label),
                    "{label}: fn {func} in {file} never mentions the label"
                );
            }
            (None, Some(status)) => {
                assert!(STATUSES.contains(&status), "{label}: unknown status {status}");
                let reason = t.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                assert!(!reason.is_empty(), "{label}: a status needs a reason");
            }
            _ => panic!("{label}: exactly one of `item` or `status`"),
        }
    }
}

#[test]
fn no_code_mentions_a_label_marked_not_in_7a() {
    let later: Vec<String> = entries()
        .into_iter()
        .filter(|(_, v)| v.get("status").and_then(|s| s.as_str()) == Some("not-in-7a"))
        .map(|(k, _)| k)
        .collect();
    assert!(!later.is_empty(), "the check needs at least one not-in-7a label to mean anything");
    let mut files = Vec::new();
    rust_files(&repo().join("crates"), &mut files);
    for file in files {
        let src = std::fs::read_to_string(&file).unwrap();
        for label in &later {
            assert!(
                !src.contains(label.as_str()),
                "{} mentions {label}, which is not in 7a",
                file.display()
            );
        }
    }
}
