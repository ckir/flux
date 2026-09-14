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
const EXPECTED: &str = "models/lockproto/expected.toml";
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
        if !(ch == b'`' || ch == b'~') || len < 3 {
            return None;
        }
        // CommonMark: a backtick fence's info string may not itself contain a backtick (it would
        // be ambiguous with inline code spans); a tilde fence's info string has no such limit.
        if ch == b'`' && text[len..].contains('`') {
            return None;
        }
        Some(Fence { ch, len })
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

/// A heading's number (design Section 9): the first whitespace-separated word after its `#`
/// marks, with at most one trailing `.` removed, when what remains is digits separated by single
/// dots (`96`, `96.1`, `21.1`). A heading whose first word doesn't fit that shape has no number.
fn heading_number(heading: &str) -> Option<String> {
    let hashes = heading.bytes().take_while(|b| *b == b'#').count();
    let rest = heading[hashes..].trim_start();
    let word = rest.split_whitespace().next()?;
    let word = word.strip_suffix('.').unwrap_or(word);
    let is_number = !word.is_empty()
        && word.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    is_number.then(|| word.to_string())
}

/// A Setext underline (design Section 9, family rule only): up to 3 leading spaces, then one or
/// more of only `=` or only `-`, trailing spaces allowed.
fn is_setext_underline(line: &str) -> bool {
    let text = line.trim_end_matches(' ');
    let leading = text.bytes().take_while(|b| *b == b' ').count();
    if leading > 3 {
        return false;
    }
    let core = &text[leading..];
    !core.is_empty() && (core.bytes().all(|b| b == b'=') || core.bytes().all(|b| b == b'-'))
}

/// A heading line for the family rule only (design Section 9): every ATX heading outside a
/// fenced block, plus every Setext heading outside a fenced block, each carrying its own number
/// or, lacking one, the number of the nearest heading above it that has one (design point 4). This
/// is a separate pass from `sections()`/`units()`, which must not change.
struct HeadingLine {
    text: String,
    number: Option<String>,
}

fn heading_lines(spec: &str) -> Vec<HeadingLine> {
    let lines: Vec<&str> = spec.lines().collect();
    let mut out = Vec::new();
    let mut fence: Option<Fence> = None;
    let mut current_number: Option<String> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let in_fence_before = fence.is_some();
        let marker = fence_marker_line(&mut fence, line);
        if !marker && !in_fence_before {
            if is_heading(line) {
                if let Some(n) = heading_number(line) {
                    current_number = Some(n);
                }
                out.push(HeadingLine { text: line.to_string(), number: current_number.clone() });
            } else if !line.trim().is_empty() {
                if let Some(next) = lines.get(i + 1) {
                    if is_setext_underline(next) {
                        if let Some(n) = heading_number(line) {
                            current_number = Some(n);
                        }
                        out.push(HeadingLine {
                            text: line.to_string(),
                            number: current_number.clone(),
                        });
                        i += 1; // also consume the underline line
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// The family root of a number (design Section 9): drop the last dotted part when there is more
/// than one part; a one-part number is its own root.
fn family_root(number: &str) -> String {
    number.rsplit_once('.').map_or_else(|| number.to_string(), |(head, _)| head.to_string())
}

/// Whether `number` is in the family rooted at `root`: equal to it, or beginning `root.`.
fn in_family(number: &str, root: &str) -> bool {
    number == root || number.starts_with(&format!("{root}."))
}

/// How many times `text` occurs, among heading lines in `all`, within the family rooted at `root`.
fn occurrences_in_family(all: &[HeadingLine], root: &str, text: &str) -> usize {
    all.iter()
        .filter(|h| h.text == text && h.number.as_deref().is_some_and(|n| in_family(n, root)))
        .count()
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
    #[serde(default)]
    heading: Vec<TraceHeadingEntry>,
    #[serde(default)]
    planned_scenarios: Vec<String>,
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
    #[serde(default)]
    pending: Vec<String>,
}

/// A `[[heading]]` entry (design Section 9): a heading in a stamped heading's family that the
/// model does not encode itself. Never carries labels — a heading whose text the model encodes is
/// stamped, never listed here.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceHeadingEntry {
    heading: String,
    family: String,
    count: usize,
    not_modelled: Option<String>,
    #[serde(default)]
    pending: Vec<String>,
}

/// The fields of `expected.toml` the check needs (design Section 9.1). `run.py` owns that file's
/// full schema, so this does not `deny_unknown_fields`.
#[derive(Deserialize, Default)]
struct Expected {
    #[serde(default)]
    scenarios: Vec<String>,
    #[serde(default)]
    never_reached: Vec<NeverReached>,
    #[serde(default)]
    deferred: Vec<Deferred>,
}

#[derive(Deserialize)]
struct NeverReached {
    label: String,
}

/// A `[[deferred]]` entry (design Section 4): a reachable label that no BUILT scenario covers yet.
/// Unlike `never_reached`, a deferred label MAY appear in `trace.toml` units - it keeps its trace.
#[derive(Deserialize)]
struct Deferred {
    label: String,
    scenario: String,
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
    expected: &'a str,
}

fn check(inputs: &Inputs<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    let all = sections(inputs.spec);
    let trace: Trace = match toml::from_str(inputs.trace) {
        Ok(t) => t,
        Err(e) => return vec![format!("{TRACE} does not parse: {e}")],
    };
    let expected: Expected = match toml::from_str(inputs.expected) {
        Ok(e) => e,
        Err(e) => {
            problems.push(format!("expected.toml does not parse: {e}"));
            Expected::default()
        }
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

    let never_reached: BTreeSet<&str> =
        expected.never_reached.iter().map(|n| n.label.as_str()).collect();

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
        let not_modelled_given = entry.not_modelled.as_ref().is_some_and(|r| !r.trim().is_empty());
        let pending_given = !entry.pending.is_empty();
        if entry.labels.is_empty() && !not_modelled_given && !pending_given {
            problems.push(format!(
                "{at}: give either non-empty labels or a not_modelled reason (or a non-empty pending array)"
            ));
        } else if !entry.labels.is_empty() {
            mapped_labels.extend(entry.labels.iter().cloned());
        }
        for label in &entry.labels {
            if never_reached.contains(label.as_str()) {
                problems.push(format!(
                    "{at}: label {label} is in expected.toml's never_reached and must not be used"
                ));
            }
        }
        for scenario in &entry.pending {
            if !trace.planned_scenarios.iter().any(|s| s == scenario) {
                problems.push(format!(
                    "{at}: pending scenario {scenario:?} is not in trace.toml's planned_scenarios"
                ));
            }
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

    // Family rule (design Section 9): every heading in a stamped heading's family, other than a
    // stamped one, must be covered by exactly one `[[heading]]` entry.
    let all_headings = heading_lines(inputs.spec);
    let mut families: BTreeMap<String, &str> = BTreeMap::new();
    for heading in stamped.keys().copied() {
        let Some(found) = all_headings.iter().find(|h| h.text == heading) else { continue };
        let Some(number) = &found.number else { continue };
        families.entry(family_root(number)).or_insert(heading);
    }

    let mut heading_entries: BTreeMap<(&str, &str), &TraceHeadingEntry> = BTreeMap::new();
    for h in &trace.heading {
        let key = (h.heading.as_str(), h.family.as_str());
        if heading_entries.insert(key, h).is_some() {
            problems.push(format!(
                "[[heading]] entry for {:?} in family {} is listed more than once",
                h.heading, h.family
            ));
        }
    }

    for (root, representative) in &families {
        let mut members: BTreeSet<&str> = BTreeSet::new();
        for h in &all_headings {
            if h.number.as_deref().is_some_and(|n| in_family(n, root)) {
                members.insert(h.text.as_str());
            }
        }
        for text in members {
            if stamped.contains_key(text) {
                continue;
            }
            if !heading_entries.contains_key(&(text, root.as_str())) {
                problems.push(format!(
                    "{text:?} is in the family of {representative:?} but has no matching trace.toml [[heading]] entry"
                ));
            }
        }
    }

    for h in &trace.heading {
        if !families.contains_key(&h.family) {
            problems.push(format!(
                "[[heading]] entry names family {:?}, which is not the family of any stamped heading",
                h.family
            ));
        }
        if stamped.contains_key(h.heading.as_str()) {
            problems.push(format!(
                "[[heading]] entry names {:?}, which is itself a stamped heading",
                h.heading
            ));
        }
        let occurrences = occurrences_in_family(&all_headings, &h.family, &h.heading);
        if occurrences == 0 {
            problems.push(format!(
                "[[heading]] entry for {:?} does not occur in family {}",
                h.heading, h.family
            ));
        } else if occurrences != h.count {
            problems.push(format!(
                "[[heading]] entry for {:?} (family {}) says count = {}, but it occurs {occurrences} times",
                h.heading, h.family, h.count
            ));
        }
        let not_modelled_given = h.not_modelled.as_ref().is_some_and(|r| !r.trim().is_empty());
        let pending_given = !h.pending.is_empty();
        if not_modelled_given == pending_given {
            problems.push(format!(
                "[[heading]] entry for {:?} (family {}) must give exactly one of a not_modelled reason or a non-empty pending array",
                h.heading, h.family
            ));
        }
        for scenario in &h.pending {
            if !trace.planned_scenarios.iter().any(|s| s == scenario) {
                problems.push(format!(
                    "[[heading]] entry for {:?} (family {}): pending scenario {scenario:?} is not in trace.toml's planned_scenarios",
                    h.heading, h.family
                ));
            }
        }
    }

    for scenario in &trace.planned_scenarios {
        if expected.scenarios.iter().any(|s| s == scenario) {
            problems.push(format!(
                "planned_scenarios lists {scenario:?}, which is also in expected.toml's scenarios"
            ));
        }
    }

    // expected.toml's `deferred` list (Section 4, Section 9.1): each entry's scenario must still
    // be planned (not yet built), and its label must exist in some model file. A deferred label
    // MAY appear in trace.toml units - unlike never_reached, it stays reachable and keeps its trace.
    for d in &expected.deferred {
        if !trace.planned_scenarios.iter().any(|s| s == &d.scenario) {
            problems.push(format!(
                "deferred label {} names scenario {:?}, which is not in trace.toml's planned_scenarios",
                d.label, d.scenario
            ));
        }
        if !model_labels.contains(&d.label) {
            problems.push(format!("deferred label {} is not a label in any model file", d.label));
        }
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
        && rewritten.heading.len() == trace.heading.len()
        && rewritten.planned_scenarios == trace.planned_scenarios
        && rewritten.unit.iter().zip(&trace.unit).zip(&hashes).all(|((new, old), hash)| {
            new.heading == old.heading
                && new.ordinal == old.ordinal
                && new.quote == old.quote
                && new.labels == old.labels
                && new.not_modelled == old.not_modelled
                && new.pending == old.pending
                && Some(&new.hash) == hash.as_ref().or(Some(&old.hash))
        })
        && rewritten.heading.iter().zip(&trace.heading).all(|(new, old)| {
            new.heading == old.heading
                && new.family == old.family
                && new.count == old.count
                && new.not_modelled == old.not_modelled
                && new.pending == old.pending
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
        expected: &read(&root.join(EXPECTED)),
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

/// `[[heading]]` entries covering, with a generic `not_modelled` reason, every heading in the
/// family of any of `headings` (SPEC's headings are deliberately numbered as a family tree, e.g.
/// `1.2` under `1`), other than `headings` themselves. For tests that use SPEC to exercise
/// unrelated behaviour and don't want the family rule (Section 9) to add unrelated problems.
fn cover_families(headings: &[&str]) -> String {
    let all = heading_lines(SPEC);
    let stamped: BTreeSet<&str> = headings.iter().copied().collect();
    let mut roots: BTreeSet<String> = BTreeSet::new();
    for heading in headings {
        if let Some(found) = all.iter().find(|h| h.text == *heading) {
            if let Some(number) = &found.number {
                roots.insert(family_root(number));
            }
        }
    }
    let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for h in &all {
        let Some(number) = &h.number else { continue };
        for root in &roots {
            if in_family(number, root) {
                *counts.entry((h.text.as_str(), root.as_str())).or_default() += 1;
            }
        }
    }
    let mut out = String::new();
    for ((text, root), count) in counts {
        if stamped.contains(text) {
            continue;
        }
        let _ = write!(
            out,
            "[[heading]]\nheading = {text:?}\nfamily = {root:?}\ncount = {count}\nnot_modelled = \"test fixture\"\n\n"
        );
    }
    out
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

/// A Windows checkout can give the spec CRLF line endings; its headings, units, and so its hashes
/// must be the same as in an LF checkout, or trace.toml would pass on one platform only.
#[test]
fn crlf_spec_splits_like_lf() {
    let crlf = SPEC.replace('\n', "\r\n");
    let lf: Vec<(String, Vec<String>)> =
        sections(SPEC).into_iter().map(|s| (s.heading, units(&s.lines))).collect();
    let got: Vec<(String, Vec<String>)> =
        sections(&crlf).into_iter().map(|s| (s.heading, units(&s.lines))).collect();
    assert_eq!(got, lf);
    assert!(got.iter().flat_map(|(_, u)| u).all(|u| !u.contains('\r')));
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
    trace += &cover_families(&["## 1.2 Steps"]);
    let tla =
        vec!["S1_2_intro: a := 1; S1_2_s1: b := 1; S1_2_s2: c := 1; S1_2_s3: d := 1;".to_string()];
    let problems =
        check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn missing_unit_changed_text_and_stray_labels_are_reported() {
    let stamp = stamp(&["## 1.1 Alpha"]);
    let mut trace = entry("## 1.1 Alpha", 1, "First paragraph", &["S1_1_first"]);
    trace = trace.replacen("hash = \"", "hash = \"00", 1);
    let tla = vec!["S1_1_first: a := 1; S1_1_extra: b := 1;".to_string()];
    let problems =
        check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    let text = problems.join("\n");
    assert!(text.contains("units [2, 3, 4] have no trace.toml entry"), "{text}");
    assert!(text.contains("unit 1: spec text changed; re-check labels [S1_1_first]"), "{text}");
    assert!(text.contains("label S1_1_extra is in a model file but not in trace.toml"), "{text}");
}

#[test]
fn quote_must_come_from_its_unit() {
    let stamp = stamp(&["## 1.2.1 Child"]);
    let mut trace = entry("## 1.2.1 Child", 1, "Take the lock", &["S1_2_1_x"]);
    trace += &cover_families(&["## 1.2.1 Child"]);
    let tla = vec!["S1_2_1_x: a := 1;".to_string()];
    let problems =
        check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
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
        expected: "",
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
    let problems =
        check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &[], expected: "" });
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

// ------------------------------------------------------------------------------------------
// Tests of the CommonMark fence fix, heading numbers, the family rule, and the new trace.toml
// shapes (design Sections 9 and 9.1), on small made-up inputs.

#[test]
fn backtick_fence_with_a_backtick_in_its_info_string_does_not_open() {
    // CommonMark: a backtick fence's info string may not itself contain a backtick, so this line
    // does not open a fence; without the fix it would, hiding "## 5.2 Next" until EOF.
    let spec = "## 5.1 Parent\n```rust` x\nStill text.\n\n## 5.2 Next\nMore.\n";
    let headings: Vec<String> = sections(spec).into_iter().map(|s| s.heading).collect();
    assert_eq!(headings, ["## 5.1 Parent", "## 5.2 Next"]);
}

#[test]
fn tilde_fence_with_a_backtick_in_its_info_string_still_opens() {
    // The restriction is backtick-specific; a tilde fence's info string may contain a backtick.
    let spec = "## 6.1 Parent\n~~~rust` x\n## 6.9 Not a heading\n~~~\n\n## 6.2 Next\n";
    let headings: Vec<String> = sections(spec).into_iter().map(|s| s.heading).collect();
    assert_eq!(headings, ["## 6.1 Parent", "## 6.2 Next"]);
}

#[test]
fn family_sibling_with_no_heading_entry_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.

## 96.2 Beta
Beta text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    let text = problems.join("\n");
    assert!(text.contains("\"## 96.2 Beta\""), "{text}");
    assert!(text.contains("\"## 96.1 Alpha\""), "{text}");
}

#[test]
fn unnumbered_heading_inherits_a_number_and_joins_the_family() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.

## Extra
Extra text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    let text = problems.join("\n");
    assert!(text.contains("\"## Extra\""), "{text}");
}

#[test]
fn setext_heading_falls_in_the_family() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.

Sibling Setext
--------------
Setext text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash1 = unit_hash("Alpha text.");
    let hash2 = unit_hash("Sibling Setext\n--------------\nSetext text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash1}\"\nlabels = [\"S96_1_a\"]\n\n\
         [[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 2\nquote = \"Setext text\"\nhash = \"{hash2}\"\nnot_modelled = \"prose only\"\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    let text = problems.join("\n");
    assert!(text.contains("\"Sibling Setext\""), "{text}");
}

#[test]
fn numbered_setext_heading_takes_its_own_number_not_the_one_above() {
    // "97 Gamma" is a Setext heading whose own first word is a number; it must take that number
    // and so is not in family 96, and must not be reported as an unlisted 96 sibling.
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.

97 Gamma
========
Gamma text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    let text = problems.join("\n");
    assert!(!text.contains("\"97 Gamma\""), "{text}");
}

#[test]
fn heading_after_a_numbered_setext_heading_inherits_its_number() {
    // The Setext heading "97 Gamma" carries its own number 97; the unnumbered heading that
    // follows it inherits 97, landing in family 97, not family 96.
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.

97 Gamma
========
Gamma text.

## Delta
Delta text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    let text = problems.join("\n");
    // "## Delta" is not in family 96 (it inherited 97 from the Setext heading above it), so it
    // must not be reported against 96.1 Alpha's family.
    assert!(!text.contains("\"## Delta\""), "{text}");
}

#[test]
fn heading_entry_with_wrong_count_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.

## Normal
First.

## Normal
Second.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n\n\
         [[heading]]\nheading = \"## Normal\"\nfamily = \"96\"\ncount = 1\nnot_modelled = \"not modelled\"\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    let text = problems.join("\n");
    assert!(text.contains("count = 1") && text.contains("occurs 2 times"), "{text}");
}

#[test]
fn duplicate_heading_entry_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.

## Normal
First.

## Normal
Second.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n\n\
         [[heading]]\nheading = \"## Normal\"\nfamily = \"96\"\ncount = 2\nnot_modelled = \"not modelled\"\n\n\
         [[heading]]\nheading = \"## Normal\"\nfamily = \"96\"\ncount = 2\nnot_modelled = \"not modelled\"\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    let text = problems.join("\n");
    assert!(text.contains("is listed more than once"), "{text}");
}

#[test]
fn heading_entry_for_a_stamped_heading_is_rejected() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n\n\
         [[heading]]\nheading = \"## 96.1 Alpha\"\nfamily = \"96\"\ncount = 1\nnot_modelled = \"bogus\"\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    assert!(problems.iter().any(|p| p.contains("is itself a stamped heading")), "{problems:#?}");
}

#[test]
fn pending_name_missing_from_planned_scenarios_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\npending = [\"mixed\"]\n"
    );
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &[], expected: "" });
    assert!(
        problems.iter().any(|p| p.contains("\"mixed\"") && p.contains("planned_scenarios")),
        "{problems:#?}"
    );
}

#[test]
fn scenario_in_both_planned_and_expected_is_rejected() {
    let trace = "planned_scenarios = [\"mixed\"]\n";
    let expected = "scenarios = [\"mixed\"]\n";
    let stamp = stamp(&[]);
    let problems = check(&Inputs { spec: "# Title\n", stamp: &stamp, trace, tla: &[], expected });
    assert!(
        problems.iter().any(|p| p.contains("\"mixed\"") && p.contains("expected.toml")),
        "{problems:#?}"
    );
}

#[test]
fn never_reached_label_used_by_a_unit_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let expected = "never_reached = [{ label = \"S96_1_a\", reason = \"cannot be reached\" }]\n";
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected });
    assert!(
        problems.iter().any(|p| p.contains("S96_1_a") && p.contains("never_reached")),
        "{problems:#?}"
    );
}

#[test]
fn unit_with_labels_and_not_modelled_passes() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\nnot_modelled = \"partly modelled\"\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn deferred_scenario_missing_from_planned_scenarios_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let expected =
        "[[deferred]]\nlabel = \"S96_1_a\"\nscenario = \"mixed\"\nreason = \"not built yet\"\n";
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected });
    assert!(
        problems.iter().any(|p| p.contains("S96_1_a")
            && p.contains("\"mixed\"")
            && p.contains("planned_scenarios")),
        "{problems:#?}"
    );
}

#[test]
fn deferred_label_absent_from_model_files_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "planned_scenarios = [\"mixed\"]\n\n\
         [[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let expected = "[[deferred]]\nlabel = \"S96_1_missing\"\nscenario = \"mixed\"\nreason = \"not built yet\"\n";
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected });
    assert!(
        problems
            .iter()
            .any(|p| p.contains("S96_1_missing") && p.contains("not a label in any model file")),
        "{problems:#?}"
    );
}

#[test]
fn deferred_label_may_also_appear_in_a_unit() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "planned_scenarios = [\"mixed\"]\n\n\
         [[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let expected =
        "[[deferred]]\nlabel = \"S96_1_a\"\nscenario = \"mixed\"\nreason = \"not built yet\"\n";
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected });
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn unit_with_none_of_labels_not_modelled_pending_fails() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\n"
    );
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &[], expected: "" });
    assert!(
        problems.iter().any(|p| p.contains("either non-empty labels or a not_modelled reason")),
        "{problems:#?}"
    );
}

// ------------------------------------------------------------------------------------------
// Coverage gaps closed against the mutation harness (R1-R5b).

#[test]
fn invalid_trace_toml_is_reported() {
    let stamp = stamp(&[]);
    let trace = "heading = \"unterminated";
    let problems =
        check(&Inputs { spec: "# Title\n", stamp: &stamp, trace, tla: &[], expected: "" });
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].contains("does not parse"), "{problems:#?}");
}

#[test]
fn invalid_expected_toml_is_reported() {
    let stamp = stamp(&[]);
    let expected = "scenarios = [";
    let problems =
        check(&Inputs { spec: "# Title\n", stamp: &stamp, trace: "", tla: &[], expected });
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].contains("expected.toml does not parse"), "{problems:#?}");
}

#[test]
fn trace_label_absent_from_model_files_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n"
    );
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &[], expected: "" });
    assert!(
        problems.iter().any(|p| p == "label S96_1_a is in trace.toml but in no model file"),
        "{problems:#?}"
    );
}

#[test]
fn heading_entry_with_neither_reason_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.

## Normal
First.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n\n\
         [[heading]]\nheading = \"## Normal\"\nfamily = \"96\"\ncount = 1\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    assert!(
        problems.iter().any(|p| p.contains(
            "must give exactly one of a not_modelled reason or a non-empty pending array"
        )),
        "{problems:#?}"
    );
}

#[test]
fn heading_entry_with_both_reasons_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.

## Normal
First.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "planned_scenarios = [\"extra\"]\n\n\
         [[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n\n\
         [[heading]]\nheading = \"## Normal\"\nfamily = \"96\"\ncount = 1\nnot_modelled = \"reason\"\npending = [\"extra\"]\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    assert!(
        problems.iter().any(|p| p.contains(
            "must give exactly one of a not_modelled reason or a non-empty pending array"
        )),
        "{problems:#?}"
    );
}

#[test]
fn heading_entry_for_an_unknown_family_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.

## 50.1 Other
Other text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n\n\
         [[heading]]\nheading = \"## 50.1 Other\"\nfamily = \"50\"\ncount = 1\nnot_modelled = \"reason\"\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    assert!(
        problems.iter().any(|p| p.contains("which is not the family of any stamped heading")),
        "{problems:#?}"
    );
}

#[test]
fn heading_entry_for_a_heading_absent_from_its_family_is_reported() {
    let spec = "\
# Title

## 96.1 Alpha
Alpha text.
";
    let stamp = stamp(&["## 96.1 Alpha"]);
    let hash = unit_hash("Alpha text.");
    let trace = format!(
        "[[unit]]\nheading = \"## 96.1 Alpha\"\nordinal = 1\nquote = \"Alpha text\"\nhash = \"{hash}\"\nlabels = [\"S96_1_a\"]\n\n\
         [[heading]]\nheading = \"## Nonexistent\"\nfamily = \"96\"\ncount = 1\nnot_modelled = \"reason\"\n"
    );
    let tla = vec!["S96_1_a: x := 1;".to_string()];
    let problems = check(&Inputs { spec, stamp: &stamp, trace: &trace, tla: &tla, expected: "" });
    assert!(problems.iter().any(|p| p.contains("does not occur in family")), "{problems:#?}");
}
