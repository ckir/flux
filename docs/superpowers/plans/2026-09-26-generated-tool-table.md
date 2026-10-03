# Generated tool table Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `docs/dev-tooling.md` carries a table generated from `.claude/recommended-tools.json` (Tool | Install | Checked by), kept current by a test, rewritten by `just tools-doc`.

**Architecture:** All in `tests/dev_tooling.rs`: a pure `render_table` and `splice`, a check test that fails when the doc's marker block differs from the rendering (CRLF-insensitive, fail-closed on an empty tool list or a bad marker pair), and an `#[ignore]`d rewrite test that `just tools-doc` runs - the pattern of `tests/model_stamp.rs` (`rewrite_unit_hashes`, `just model-stamp`). The hand-written provenance tables stay; the existing one-way check keeps reading ONLY them.

**Tech Stack:** Rust integration test in the root crate (`flux`), `serde` / `serde_json` (already dev-dependencies), `just`, `cargo nextest`.

**Design authority:** owner-approved 2026-09-26 after an AGY-FIRST consult and one AGY-NEGOTIATE round (agy moved from "no generator" to accepting this design; brief `.clavity/seams/tool-table-negotiate1.md`). Every citation below was read at `60586e0` (`main` after PR #50).

---

## Ground rules

- **Worktree:** `E:\Rust\flux-engine`, branch `chore/tool-table` (off `origin/main` at `60586e0`, no upstream). NEVER run a bare `git push`; publishing is `just pr` after the capstone, with owner approval.
- **Step 0 — state verification.** Confirm each quoted "current" text; if it differs, STOP with `STATE_MISMATCH: <file>: <what>`.
- **Shape-divergence stop.** If making it compile changes a type, name, marker string, column or message shown here, STOP and report `[plan] -> [yours] because <reason>`.
- **Tests are the oracle** — the tests in this plan are already written; implement until they pass, never edit them to match the code.
- **Run commands from Git Bash** in the worktree. Write files with the Write/Edit tools (the Rust contains `\\|`).
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## The self-vacuity trap this plan designs out

`documented()` collects every backticked name in the first cell of ANY table row. The generated table lists every JSON tool in its first cell, so once it exists, `every_recommended_tool_is_documented` would find every tool THERE and pass while checking nothing about the hand tables. The existing check must therefore read the doc with the generated block removed (`without_generated`), and Task 1 Step 5 proves it still fails when a hand-table row is deleted.

---

### Task 1: the generator, its check, and the first generated block

**Files:** Modify `tests/dev_tooling.rs`, `docs/dev-tooling.md`, `justfile`.

- [ ] **Step 0: Verify state.** `tests/dev_tooling.rs` contains:

```rust
#[derive(serde::Deserialize)]
struct Tool {
    name: String,
}
```

and `every_recommended_tool_is_documented` contains `let documented = documented(&doc);`. `docs/dev-tooling.md` has the heading `## Gate commands (the contract)` and, under `## Keeping this file honest`, a paragraph beginning `Every tool named here that an agent or a fresh checkout has to *install* is also declared in`. `justfile` has the recipe `model-stamp:` whose second line is `    cargo test --test model_stamp -- --ignored rewrite_unit_hashes --exact`, followed by a blank line and `# Mutation testing over the engine`.

- [ ] **Step 1: Replace the `Tool` struct** with:

```rust
#[derive(serde::Deserialize)]
struct Tool {
    name: String,
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
```

- [ ] **Step 2: Add the generator** (after `fn documented`):

```rust
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
            Some(FileExists::Many(ps)) => checks.extend(ps.iter().map(|p| format!("file {}", code(p)))),
            None => {}
        }
        let checked = if checks.is_empty() { "not checked".to_string() } else { checks.join(" or ") };
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

fn load_tools() -> Vec<Tool> {
    let json = std::fs::read_to_string(repo().join(".claude/recommended-tools.json"))
        .expect("read .claude/recommended-tools.json");
    let tools: Vec<Tool> = serde_json::from_str(&json).expect("parse recommended-tools.json");
    assert!(!tools.is_empty(), "recommended-tools.json declares no tools");
    tools
}
```

- [ ] **Step 3: Make the existing check read only the hand tables.** In `every_recommended_tool_is_documented`, replace `let documented = documented(&doc);` with:

```rust
    // The generated table names every JSON tool in its first cell; reading it here would make this
    // check pass while checking nothing about the hand-written tables.
    let hand = without_generated(&doc).unwrap_or_else(|e| panic!("{e}"));
    let documented = documented(&hand);
```

- [ ] **Step 4: Add the tests** (at the end of the file). They are the oracle.

```rust
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
```

- [ ] **Step 5: Run — expect exactly one failure.** `cargo nextest run --test dev_tooling --no-fail-fast` → every test passes EXCEPT `the_generated_tool_table_is_current` and `every_recommended_tool_is_documented`, both failing with the marker-pair message (the doc has no markers yet). Record the output. Any other failure: STOP and report.

- [ ] **Step 6: Add the markers and the section.** In `docs/dev-tooling.md`, insert immediately BEFORE the line `## Gate commands (the contract)`:

```markdown
## Required tools

What `.claude/recommended-tools.json` declares: the tools a fresh checkout has to install, how, and how
the SessionStart hook decides each one is present. Generated - edit the JSON, then run `just tools-doc`;
the tables above carry the reasons.

<!-- tools:begin - generated from .claude/recommended-tools.json by `just tools-doc`; do not edit by hand -->
<!-- tools:end -->

```

- [ ] **Step 7: Add the recipe.** In `justfile`, directly after the `model-stamp:` recipe's last line (`    cargo test --test model_stamp -- --ignored rewrite_unit_hashes --exact`) and its blank line, insert:

```just
# Regenerate the "Required tools" table in docs/dev-tooling.md from .claude/recommended-tools.json.
# `just check` fails while the two disagree (tests/dev_tooling.rs, the_generated_tool_table_is_current).
tools-doc:
    cargo test --test dev_tooling -- --ignored rewrite_tool_table --exact

```

- [ ] **Step 8: Generate and go green.** `just tools-doc`, then `cargo nextest run --test dev_tooling --no-fail-fast` → all pass. `git diff docs/dev-tooling.md` shows the new section with 14 generated rows (one per JSON entry, in JSON order) and nothing else changed.
- [ ] **Step 9: Update the prose.** In `docs/dev-tooling.md`'s "Keeping this file honest" paragraph, after its first sentence (ending `... to report what is missing.`), insert: `The "Required tools" table is generated from that JSON by \`just tools-doc\`, and \`just check\` fails while it is stale.` Then `just tools-doc` again (must be a no-op: `git diff` unchanged by it).
- [ ] **Step 10: Non-vacuous proofs** (each temporary; restore and confirm `git diff` shows only the intended changes):
  1. In `.claude/recommended-tools.json`, change `"cargo binstall -y just"` to `"cargo binstall -y just-x"` → `the_generated_tool_table_is_current` FAILS with `Run: just tools-doc`. Restore.
  2. In `docs/dev-tooling.md`, delete the hand-table row whose first cell is `` `just` `` (in "Added for Flux" or "Carried over") → `every_recommended_tool_is_documented` FAILS naming `just`, even though the generated table still lists it. Restore.
  3. In `docs/dev-tooling.md`, delete the `<!-- tools:end -->` line → both checks FAIL with the marker-pair message. Restore.
- [ ] **Step 11: Gates.** `just check` → exit 0; `just check-linux` → exit 0 (the root crate's tests run there; `check-mac` excludes the root crate, so it does not exercise this file).
- [ ] **Step 12: Commit** — `feat(docs): generate the required-tools table from recommended-tools.json`, with Step 10's three results in the body.

### Task 2: final verification (no publishing)

- [ ] `just check` and `just check-linux` → exit 0; `git status --short` clean; `git log --oneline origin/main..HEAD` lists this plan's commit and Task 1's. STOP: AGY-CAPSTONE next, then owner-approved `just pr`.

## Self-review (done by the plan author)

- **Design coverage:** columns Tool | Install | Checked by (`in_path` / `file_exists`), no `why`, no JSON fields added → Steps 1-2; hand tables kept and the one-way check reading only them → Step 3 + the self-vacuity test + proof 2; CRLF-insensitive compare, empty-list and marker-pair refusals → `load_tools`, `block_span`, the check test, proofs 1 and 3; `just tools-doc` via an ignored rewrite test → Steps 4, 7; no `jq`, no new CI job → runs inside the existing test jobs.
- **Placeholders:** none; every code step shows the code.
- **Consistency:** `BEGIN` / `END` strings are identical in the constant, Step 6's markdown, and the tests; `rewrite_tool_table` is named identically in the test and the recipe.
