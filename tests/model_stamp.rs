//! Spec-drift stamp and traceability check for the lock-protocol model.
//!
//! Design: docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md, Sections 9
//! and 9.1. `models/lockproto/spec-sections.stamp` names the spec file and the headings the
//! model encodes; `models/lockproto/trace.toml` maps every unit of those headings to the
//! PlusCal labels that implement it (or to a reason it is not modelled) and carries each
//! unit's BLAKE3 hash. This test fails when the spec text, the map, or the model labels
//! drift apart. `just model-stamp` runs the ignored `rewrite_unit_hashes` test after a green
//! `just model`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const STAMP: &str = "models/lockproto/spec-sections.stamp";
const TRACE: &str = "models/lockproto/trace.toml";
const MODELS: &str = "models/lockproto";

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ------------------------------------------------------------------------------------------
// Spec text

/// A heading line of the spec and the lines of its own text (up to the next heading).
struct Section {
    heading: String,
    lines: Vec<String>,
}

fn is_heading(line: &str) -> bool {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ')
}

/// An open fenced code block, as CommonMark defines it: a run of at least three backticks or
/// tildes opens it, and only a run of at least as many of the same character with nothing after it
/// closes it. Indentation before a marker is ignored.
#[derive(Clone, Copy)]
struct Fence {
    ch: u8,
    len: usize,
}

impl Fence {
    fn opened_by(line: &str) -> Option<Fence> {
        let text = line.trim_start();
        let ch = *text.as_bytes().first()?;
        let len = text.bytes().take_while(|b| *b == ch).count();
        ((ch == b'`' || ch == b'~') && len >= 3).then_some(Fence { ch, len })
    }

    fn closed_by(self, line: &str) -> bool {
        let text = line.trim_start();
        let run = text.bytes().take_while(|b| *b == self.ch).count();
        run >= self.len && text[run..].trim().is_empty()
    }
}

/// Advance the fence state over one line; true if the line is a fence marker (opens or closes a
/// fence). A marker-like line inside a fence that does not close it is ordinary text.
fn fence_marker_line(fence: &mut Option<Fence>, line: &str) -> bool {
    match *fence {
        Some(open) if open.closed_by(line) => {
            *fence = None;
            true
        }
        Some(_) => false,
        None => {
            *fence = Fence::opened_by(line);
            fence.is_some()
        }
    }
}

/// Split the spec into sections. Heading lines inside fenced code blocks (indented or not) are text,
/// not headings.
fn sections(spec: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut fence: Option<Fence> = None;
    for line in spec.lines() {
        let in_fence = fence.is_some();
        if !fence_marker_line(&mut fence, line) && !in_fence && is_heading(line) {
            out.push(Section { heading: line.to_string(), lines: Vec::new() });
            continue;
        }
        if let Some(section) = out.last_mut() {
            section.lines.push(line.to_string());
        }
    }
    out
}

/// A numbered step: optional indentation, digits, a dot, and a space (`1. Open`, `    2.  Take`).
fn is_step(line: &str) -> bool {
    let text = line.trim_start();
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && text[digits..].starts_with(". ")
}

/// The units of a heading's own text (design Section 9.1): blocks separated by blank lines or
/// fence-marker lines (which belong to no unit), except that each numbered step starts a new
/// unit and keeps the indented lines that follow it, even across blank lines. A unit's text is
/// its lines joined by '\n'. The numbered steps of Sections 21.1 and 240.1 sit inside fences, so
/// a fence does not stop a step from being its own unit.
fn units(lines: &[String]) -> Vec<String> {
    let mut units: Vec<Vec<&str>> = Vec::new();
    let mut in_step = false;
    let mut after_blank = true;
    // A section's lines start outside any fence: sections() splits only outside fences.
    let mut fence: Option<Fence> = None;
    for line in lines {
        if fence_marker_line(&mut fence, line) || line.trim().is_empty() {
            after_blank = true;
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let step = is_step(line);
        let continues = !step && if in_step { indented } else { !after_blank };
        if !continues || units.is_empty() {
            units.push(Vec::new());
            in_step = step;
        }
        if let Some(unit) = units.last_mut() {
            unit.push(line);
        }
        after_blank = false;
    }
    units.into_iter().map(|u| u.join("\n")).collect()
}

fn unit_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ------------------------------------------------------------------------------------------
// Stamp, trace map, and model labels

struct Stamp {
    spec: String,
    headings: Vec<String>,
}

fn parse_stamp(text: &str) -> Result<Stamp, String> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let spec = lines
        .next()
        .and_then(|l| l.strip_prefix("spec: "))
        .ok_or("the stamp's first line must be 'spec: <path>'")?
        .trim()
        .to_string();
    let headings: Vec<String> = lines.map(str::to_string).collect();
    if let Some(bad) = headings.iter().find(|h| !is_heading(h)) {
        return Err(format!("stamp line is not a heading line: {bad:?}"));
    }
    Ok(Stamp { spec, headings })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Trace {
    #[serde(default)]
    unit: Vec<TraceUnit>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceUnit {
    heading: String,
    ordinal: usize,
    quote: String,
    hash: String,
    #[serde(default)]
    labels: Vec<String>,
    not_modelled: Option<String>,
}

fn is_label_name(token: &str) -> bool {
    let rest = match token.strip_prefix('S') {
        Some(rest) => rest,
        None => return false,
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && rest[digits..].starts_with('_') && rest.len() > digits + 1
}

/// PlusCal labels named after spec steps (`S<section>_<step>:`, design Section 6.1).
fn labels_in(tla: &str) -> BTreeSet<String> {
    let bytes = tla.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut found = BTreeSet::new();
    let mut i = 0;
    while i < bytes.len() {
        if !is_word(bytes[i]) || (i > 0 && is_word(bytes[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_word(bytes[i]) {
            i += 1;
        }
        let token = &tla[start..i];
        let mut j = i;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        let colon = bytes.get(j) == Some(&b':');
        let next = bytes.get(j + 1).copied();
        if is_label_name(token) && colon && next != Some(b'=') && next != Some(b':') {
            found.insert(token.to_string());
        }
    }
    found
}

/// Everything the check reads, so the logic can be tested without touching the repository.
struct Inputs<'a> {
    spec: &'a str,
    stamp: &'a Stamp,
    trace: &'a str,
    tla: &'a [String],
}

fn check(inputs: &Inputs<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    let all = sections(inputs.spec);
    let trace: Trace = match toml::from_str(inputs.trace) {
        Ok(t) => t,
        Err(e) => return vec![format!("{TRACE} does not parse: {e}")],
    };

    let mut stamped: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for heading in &inputs.stamp.headings {
        let matches: Vec<&Section> = all.iter().filter(|s| &s.heading == heading).collect();
        if stamped.contains_key(heading.as_str()) {
            problems.push(format!("the stamp lists {heading:?} twice"));
        } else if matches.len() != 1 {
            problems
                .push(format!("the spec has {} heading lines equal to {heading:?}", matches.len()));
        } else {
            stamped.insert(heading, units(&matches[0].lines));
        }
    }

    let mut seen: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    let mut mapped_labels = BTreeSet::new();
    for entry in &trace.unit {
        let at = format!("{} unit {}", entry.heading, entry.ordinal);
        let Some(units) = stamped.get(entry.heading.as_str()) else {
            problems.push(format!(
                "{at}: heading is not listed in the stamp (or not found in the spec)"
            ));
            continue;
        };
        if !seen.entry(entry.heading.as_str()).or_default().insert(entry.ordinal) {
            problems.push(format!("{at}: listed twice"));
        }
        let Some(text) = entry.ordinal.checked_sub(1).and_then(|i| units.get(i)) else {
            problems.push(format!("{at}: no such unit; ordinals run from 1 to {}", units.len()));
            continue;
        };
        if entry.quote.trim().is_empty() || !squash(text).contains(&squash(&entry.quote)) {
            problems.push(format!("{at}: quote {:?} is not in the unit", entry.quote));
        }
        if entry.hash != unit_hash(text) {
            problems.push(format!(
                "{at}: spec text changed; re-check labels [{}] against it, then run `just model-stamp`",
                entry.labels.join(", ")
            ));
        }
        match (entry.labels.is_empty(), &entry.not_modelled) {
            (false, None) => mapped_labels.extend(entry.labels.iter().cloned()),
            (true, Some(reason)) if !reason.trim().is_empty() => {}
            _ => problems
                .push(format!("{at}: give either non-empty labels or a not_modelled reason")),
        }
    }
    for (heading, units) in &stamped {
        let have = seen.get(heading).cloned().unwrap_or_default();
        let missing: Vec<usize> = (1..=units.len()).filter(|n| !have.contains(n)).collect();
        if !missing.is_empty() {
            problems.push(format!("{heading}: units {missing:?} have no trace.toml entry"));
        }
    }

    let model_labels: BTreeSet<String> = inputs.tla.iter().flat_map(|t| labels_in(t)).collect();
    for label in model_labels.difference(&mapped_labels) {
        problems.push(format!("label {label} is in a model file but not in trace.toml"));
    }
    for label in mapped_labels.difference(&model_labels) {
        problems.push(format!("label {label} is in trace.toml but in no model file"));
    }
    problems
}

/// Replace each unit's `hash = "..."` line with the hash of the current spec text.
fn rewrite_hashes(trace_text: &str, spec: &str) -> Result<String, String> {
    let trace: Trace = toml::from_str(trace_text).map_err(|e| e.to_string())?;
    let all = sections(spec);
    let hashes: Vec<Option<String>> = trace
        .unit
        .iter()
        .map(|entry| {
            let section = all.iter().find(|s| s.heading == entry.heading)?;
            let text = units(&section.lines).into_iter().nth(entry.ordinal.checked_sub(1)?)?;
            Some(unit_hash(&text))
        })
        .collect();
    let mut out = String::new();
    let mut index: Option<usize> = None;
    for line in trace_text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("[[unit]]") {
            index = Some(index.map_or(0, |i| i + 1));
        }
        let is_hash = trimmed.strip_prefix("hash").is_some_and(|r| r.trim_start().starts_with('='));
        if let (true, Some(Some(hash))) = (is_hash, index.and_then(|i| hashes.get(i))) {
            writeln!(out, "hash = \"{hash}\"").map_err(|e| e.to_string())?;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    // The edit is line-based, so prove it changed the hashes and nothing else (a multi-line
    // string with a line starting `hash =` would otherwise be rewritten too).
    let rewritten: Trace = toml::from_str(&out).map_err(|e| format!("rewritten map: {e}"))?;
    let only_hashes = rewritten.unit.len() == trace.unit.len()
        && rewritten.unit.iter().zip(&trace.unit).zip(&hashes).all(|((new, old), hash)| {
            new.heading == old.heading
                && new.ordinal == old.ordinal
                && new.quote == old.quote
                && new.labels == old.labels
                && new.not_modelled == old.not_modelled
                && Some(&new.hash) == hash.as_ref().or(Some(&old.hash))
        });
    if !only_hashes {
        return Err(
            "rewriting the hash lines would change more than the hashes; fix trace.toml by hand"
                .into(),
        );
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------
// The repository check

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn model_files(root: &Path) -> Vec<String> {
    let mut texts = Vec::new();
    let dir = root.join(MODELS);
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display())).path();
        if path.extension().is_some_and(|e| e == "tla") {
            texts.push(read(&path));
        }
    }
    texts
}

#[test]
fn model_trace_matches_spec() {
    let root = repo();
    let stamp = parse_stamp(&read(&root.join(STAMP))).unwrap();
    let spec_path = root.join(&stamp.spec);
    assert!(
        spec_path.is_file(),
        "{STAMP} names {}, which does not exist; update the stamp's 'spec:' line",
        stamp.spec
    );
    let problems = check(&Inputs {
        spec: &read(&spec_path),
        stamp: &stamp,
        trace: &read(&root.join(TRACE)),
        tla: &model_files(&root),
    });
    assert!(problems.is_empty(), "model traceability problems:\n  {}", problems.join("\n  "));
}

#[test]
#[ignore = "rewrites trace.toml; run through `just model-stamp` after a green `just model`"]
fn rewrite_unit_hashes() {
    let root = repo();
    let stamp = parse_stamp(&read(&root.join(STAMP))).unwrap();
    let spec = read(&root.join(&stamp.spec));
    let trace_path = root.join(TRACE);
    let rewritten = rewrite_hashes(&read(&trace_path), &spec).unwrap();
    std::fs::write(&trace_path, rewritten).unwrap();
}

// ------------------------------------------------------------------------------------------
// Tests of the check itself, on small made-up inputs

const SPEC: &str = "\
# 1. Top

## 1.1 Alpha

First paragraph of alpha
continues here.

Second paragraph.

``` text
## 9.9 Not a heading
still fenced

after a blank line in the fence
```

## 1.2 Steps

Intro before the steps:

1. Open the file.
   Continue step one.
2. Take the lock.

   Step two after a blank line.
3. Write.
Not indented, so a new unit.

## 1.2.1 Child
Child text.

## 1.3 Fenced steps

In this order:

``` text
1. acquire the lock
   without waiting
2. mark the prior operation
```

After the fence.

## 1.4 Indented list

    1.  First item
        more of it
    2.  Second item
";

fn stamp(headings: &[&str]) -> Stamp {
    Stamp {
        spec: "SPEC.md".to_string(),
        headings: headings.iter().map(|h| h.to_string()).collect(),
    }
}

fn section_units(heading: &str) -> Vec<String> {
    let all = sections(SPEC);
    let section = all.iter().find(|s| s.heading == heading).unwrap();
    units(&section.lines)
}

fn entry(heading: &str, ordinal: usize, quote: &str, labels: &[&str]) -> String {
    let text = &section_units(heading)[ordinal - 1];
    let labels: Vec<String> = labels.iter().map(|l| format!("{l:?}")).collect();
    format!(
        "[[unit]]\nheading = {heading:?}\nordinal = {ordinal}\nquote = {quote:?}\nhash = \"{}\"\nlabels = [{}]\n\n",
        unit_hash(text),
        labels.join(", ")
    )
}

#[test]
fn fenced_heading_is_not_a_heading() {
    let headings: Vec<String> = sections(SPEC).into_iter().map(|s| s.heading).collect();
    assert_eq!(
        headings,
        [
            "# 1. Top",
            "## 1.1 Alpha",
            "## 1.2 Steps",
            "## 1.2.1 Child",
            "## 1.3 Fenced steps",
            "## 1.4 Indented list"
        ]
    );
}

#[test]
fn indented_fence_hides_a_heading_line() {
    // The fence is indented under a list item; the line inside it starts at column 0.
    let spec = "## 2.1 Parent

1. A step with code:

   ``` text
## 2.9 Not a heading
   ```

## 2.2 Next
Text.
";
    let headings: Vec<String> = sections(spec).into_iter().map(|s| s.heading).collect();
    assert_eq!(headings, ["## 2.1 Parent", "## 2.2 Next"]);
}

#[test]
fn longer_fence_is_not_closed_by_a_shorter_marker() {
    // A four-backtick fence showing a Markdown sample: the inner ``` lines are text, so the
    // heading-like line between them stays text and is hashed with its unit.
    let spec = "## 3.1 Parent
Before.

````markdown
```text
## 3.9 Not a heading
```
````

After.

## 3.2 Next
Text.
";
    let parts = sections(spec);
    let headings: Vec<&str> = parts.iter().map(|s| s.heading.as_str()).collect();
    assert_eq!(headings, ["## 3.1 Parent", "## 3.2 Next"]);
    assert_eq!(units(&parts[0].lines), ["Before.", "```text\n## 3.9 Not a heading\n```", "After."]);
}

#[test]
fn marker_with_trailing_text_does_not_close_a_fence() {
    let spec = "## 4.1 Parent
```
```text
## 4.9 Not a heading
```

## 4.2 Next
";
    let headings: Vec<String> = sections(spec).into_iter().map(|s| s.heading).collect();
    assert_eq!(headings, ["## 4.1 Parent", "## 4.2 Next"]);
}

#[test]
fn units_split_at_blank_lines_and_fences() {
    assert_eq!(
        section_units("## 1.1 Alpha"),
        [
            "First paragraph of alpha\ncontinues here.",
            "Second paragraph.",
            "## 9.9 Not a heading\nstill fenced",
            "after a blank line in the fence",
        ]
    );
}

#[test]
fn numbered_steps_inside_a_fence_are_units() {
    assert_eq!(
        section_units("## 1.3 Fenced steps"),
        [
            "In this order:",
            "1. acquire the lock\n   without waiting",
            "2. mark the prior operation",
            "After the fence.",
        ]
    );
}

#[test]
fn indented_numbered_steps_are_units() {
    assert_eq!(
        section_units("## 1.4 Indented list"),
        ["    1.  First item\n        more of it", "    2.  Second item"]
    );
}

#[test]
fn numbered_steps_are_units_and_keep_indented_lines() {
    let got = section_units("## 1.2 Steps");
    assert_eq!(
        got,
        [
            "Intro before the steps:",
            "1. Open the file.\n   Continue step one.",
            "2. Take the lock.\n   Step two after a blank line.",
            "3. Write.",
            "Not indented, so a new unit.",
        ]
    );
}

#[test]
fn child_heading_text_is_not_part_of_its_parent() {
    assert!(section_units("## 1.2 Steps").iter().all(|u| !u.contains("Child text")));
    assert_eq!(section_units("## 1.2.1 Child"), ["Child text."]);
}

#[test]
fn labels_are_found_and_near_misses_are_not() {
    let tla = "S240_5_s6a: x := 1;\nS97_1_ancestor:\n  y := S99_check;\nS12_x := 3;\nS1_a::\nStep: z := 0;\nXS1_a: w := 1;";
    let got: Vec<String> = labels_in(tla).into_iter().collect();
    assert_eq!(got, ["S240_5_s6a", "S97_1_ancestor"]);
}

#[test]
fn complete_map_passes() {
    let stamp = stamp(&["## 1.2 Steps"]);
    let mut trace = String::new();
    trace += &entry("## 1.2 Steps", 1, "Intro before", &["S1_2_intro"]);
    trace += &entry("## 1.2 Steps", 2, "Open the file. Continue", &["S1_2_s1"]);
    trace += &entry("## 1.2 Steps", 3, "Take the lock.", &["S1_2_s2"]);
    trace += &entry("## 1.2 Steps", 4, "3. Write.", &["S1_2_s3"]);
    trace += "[[unit]]\nheading = \"## 1.2 Steps\"\nordinal = 5\nquote = \"Not indented\"\n";
    let _ = writeln!(trace, "hash = \"{}\"", unit_hash(&section_units("## 1.2 Steps")[4]));
    trace += "not_modelled = \"prose only\"\n";
    let tla =
        vec!["S1_2_intro: a := 1; S1_2_s1: b := 1; S1_2_s2: c := 1; S1_2_s3: d := 1;".to_string()];
    let problems = check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &tla });
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn missing_unit_changed_text_and_stray_labels_are_reported() {
    let stamp = stamp(&["## 1.1 Alpha"]);
    let mut trace = entry("## 1.1 Alpha", 1, "First paragraph", &["S1_1_first"]);
    trace = trace.replacen("hash = \"", "hash = \"00", 1);
    let tla = vec!["S1_1_first: a := 1; S1_1_extra: b := 1;".to_string()];
    let problems = check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &tla });
    let text = problems.join("\n");
    assert!(text.contains("units [2, 3, 4] have no trace.toml entry"), "{text}");
    assert!(text.contains("unit 1: spec text changed; re-check labels [S1_1_first]"), "{text}");
    assert!(text.contains("label S1_1_extra is in a model file but not in trace.toml"), "{text}");
}

#[test]
fn quote_must_come_from_its_unit() {
    let stamp = stamp(&["## 1.2.1 Child"]);
    let trace = entry("## 1.2.1 Child", 1, "Take the lock", &["S1_2_1_x"]);
    let tla = vec!["S1_2_1_x: a := 1;".to_string()];
    let problems = check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &tla });
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].contains("is not in the unit"));
}

#[test]
fn stamp_heading_must_exist_once() {
    let problems = check(&Inputs {
        spec: SPEC,
        stamp: &stamp(&["## 9.9 Not a heading", "## 1.2.1 Child", "## 1.2.1 Child"]),
        trace: "",
        tla: &[],
    });
    let text = problems.join("\n");
    assert!(
        text.contains("the spec has 0 heading lines equal to \"## 9.9 Not a heading\""),
        "{text}"
    );
    assert!(text.contains("the stamp lists \"## 1.2.1 Child\" twice"), "{text}");
}

#[test]
fn labels_and_not_modelled_are_exclusive() {
    let stamp = stamp(&["## 1.2.1 Child"]);
    let trace = entry("## 1.2.1 Child", 1, "Child text", &[]);
    let problems = check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &[] });
    assert!(
        problems.iter().any(|p| p.contains("either non-empty labels or a not_modelled reason")),
        "{problems:#?}"
    );
}

#[test]
fn stamp_format() {
    let parsed = parse_stamp("spec: FLUX.md\n## 96.1 Locks\n").unwrap();
    assert_eq!(parsed.spec, "FLUX.md");
    assert_eq!(parsed.headings, ["## 96.1 Locks"]);
    assert!(parse_stamp("## 96.1 Locks\n").is_err());
    assert!(parse_stamp("spec: FLUX.md\nnot a heading\n").is_err());
}

#[test]
fn rewrite_refuses_to_touch_a_multi_line_string() {
    let trace = "[[unit]]\nheading = \"## 1.2.1 Child\"\nordinal = 1\n\
                 quote = \"\"\"Child\nhash = \"x\"\n\"\"\"\nhash = \"stale\"\nlabels = [\"S1_2_1_x\"]\n";
    assert!(rewrite_hashes(trace, SPEC).is_err());
}

#[test]
fn rewrite_updates_only_hash_lines() {
    let mut trace = entry("## 1.2.1 Child", 1, "Child text", &["S1_2_1_x"]);
    let good = unit_hash("Child text.");
    trace = trace.replace(&good, "stale");
    let rewritten = rewrite_hashes(&format!("# map\n{trace}"), SPEC).unwrap();
    assert!(rewritten.starts_with("# map\n[[unit]]\n"));
    assert!(rewritten.contains(&format!("hash = \"{good}\"")));
    assert!(!rewritten.contains("stale"));
}
