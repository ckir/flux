# Verify-merge property test (cut 14a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A proptest model-comparison test that runs `verify_tree` on generated pairs of trees over the test fake and checks its ordered records and summary against an isolated non-streaming reference, plus the one fake primitive and the one fixture the design calls for.

**Architecture:** One new `#[cfg(test)]` module `crates/flux-core/src/verify_proptest.rs` holds the generated `Case` types, the two-side derivation, the fake materialisation, the pure reference, the census test and the property. The fake gains a path-targeted listing fault. No engine code changes; `verify.rs` gains one fixture.

**Tech Stack:** Rust, `proptest` 1.11 (workspace-pinned with default features off; the member enables `std`), the in-crate `FaultFs` fake, `flux_hash` for digests, `flux_fs::FluxPathKey` for order.

**Spec:** `docs/superpowers/specs/2026-10-26-verify-property-test-design.md` (cited as "design N"). Rules under test: `docs/superpowers/specs/2026-10-26-verify-command-design.md` sections 5 and 6.2 to 6.5. Each task below restates every rule its implementer needs; the design is the authority when they differ (report the conflict, do not guess).

## Global Constraints

- No engine source changes: nothing outside `mod tests` in `crates/flux-core/src/verify.rs` changes; `fault_fs.rs` is the test fake (already `#[cfg(test)]`, `lib.rs:29`) and may gain the one primitive of Task 1 only.
- The new module is registered `#[cfg(test)] mod verify_proptest;` in `crates/flux-core/src/lib.rs` directly after line 29's `pub mod fault_fs;` (design 6.1). No file under `crates/flux-core/tests/` (the fake is invisible there).
- Dependency lines, exactly: root `Cargo.toml` line 95 becomes `proptest = { version = "1.11", default-features = false }`; `crates/flux-core/Cargo.toml` `[dev-dependencies]` gains `proptest = { workspace = true, features = ["std"] }`. (Measured on cargo 1.99.1: a member-level `default-features = false` on an inherited dependency also works, but putting the opt-out on the workspace entry is the form every Cargo version documents.) `Cargo.lock` changes with it and is committed in the same commit; `cargo deny check` stays green.
- Every new `.rs` file starts with the repository licence header (`header.txt`; `just license` runs `addlicense -check`).
- The repo gate is `just check` (`fmt-check clippy typos doc-check license test`), exit code read directly, never through a pipe. The typos step reads a short git sha as a word: write no short sha anywhere.
- Paths are built with `Path::join`, never string concatenation, so Windows CI sees the same fake paths as Linux. Fake paths are absolute under the fixed roots `/s` and `/d`; relative paths in `Case`, `sides` and `reference` never carry a root.
- The reference calls no engine code: not `Side`, `verify_tree`, `compare_pair`, `is_control_state` (design anti-goals). It may use `FluxPathKey::from_relative`, `flux_hash`, `Kind`, `Outcome`, `Code`, `MOUNT_NOT_ENTERED`, `SPECIAL_NOT_COMPARED`.
- The property uses the `proptest!` macro and `ProptestConfig::default()` (design 6.7); records are compared in emission order, never sorted.
- Fault codes and kinds come in two fixed pairs: `Code::PermissionDenied` with `io::ErrorKind::PermissionDenied`, `Code::IoError` with `io::ErrorKind::Other`. A fault's record text is `std::io::Error::from(kind).to_string()`.
- Commit per task; subagents never push; commit trailers per the repository convention.

## Review Focus

1. A SOURCE file named `.flux-root.lock` is reported `missing` when the destination's twin is filtered (cut 14 design 6.4): a census category in Task 4.
2. A symlink present on BOTH sides at one path, under `Links::Skip`, is `extra` (the source copy is filtered, the destination's is not): a census category in Task 4.
3. A `Dir { mount: Yes, list_error: Some }` under `MountPolicy::Cross` is LISTED, so its fault applies: a census category in Task 4.
4. Both sides empty after derivation must give zero records and `Summary::default()` without a panic: a unit test in Task 5.
5. Windows separators: `materialise` and the reference both see paths from `Path::join`; the fake's `norm` and `calls()` normalise. Pinned only by CI on `windows-latest`; the implementer cannot run it here.

---

### Task 1: The fake's path-targeted listing fault

**Files:**
- Modify: `crates/flux-core/src/fault_fs.rs` (struct `Inner` at lines 30-31 derives `Default`, so a new field needs no initialiser; add the field after `canonical_faults` at line 56; a new method beside `fail_canonical_path_of` at lines 1301-1312; `read_dir` at lines 1782-1808; a test in `mod tests` at line 2658 onward).

**Interfaces:**
- Produces: `pub fn fail_read_dir_of(&self, path: impl AsRef<Path>, code: Code, kind: std::io::ErrorKind)` on `FaultFs`.

- [ ] **Step 1: Write the failing test** in `fault_fs.rs`'s `mod tests`:

```rust
#[test]
fn fail_read_dir_of_fails_that_directory_only_and_logs_the_call() {
    let fs = FaultFs::new();
    fs.create_dir(Path::new("/d")).unwrap();
    fs.create_dir(Path::new("/d/x")).unwrap();
    fs.create_dir(Path::new("/d/y")).unwrap();
    fs.fail_read_dir_of("/d/x", Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    assert_eq!(fs.read_dir(Path::new("/d")).unwrap().len(), 2);
    for _ in 0..2 {
        let e = fs.read_dir(Path::new("/d/x")).unwrap_err();
        assert_eq!(e.code, Code::PermissionDenied);
        assert_eq!(e.source.kind(), std::io::ErrorKind::PermissionDenied);
    }
    assert!(fs.read_dir(Path::new("/d/y")).is_ok());
    assert_eq!(fs.calls().iter().filter(|c| *c == "read_dir(/d/x)").count(), 2);
}
```

- [ ] **Step 2: Run** `cargo nextest run -p flux-core fail_read_dir_of` ; expected: compile error, no such method.
- [ ] **Step 3: Implement.** `Inner` gains `read_dir_faults: HashMap<PathBuf, (Code, std::io::ErrorKind)>` (doc comment in the style of `canonical_faults`'s). The method computes `let p = self.norm(path.as_ref());` BEFORE `self.inner.lock()` (`norm` locks `inner` itself, line 1344), then inserts. In `read_dir`, after the `directories.contains(&p)` check and before the listing loop: `if let Some(&(code, kind)) = g.read_dir_faults.get(&p) { return Err(FsError::new(code, std::io::Error::from(kind))); }`.
- [ ] **Step 4: Run** `cargo nextest run -p flux-core fault_fs` ; expected: all pass, incl. the new test.
- [ ] **Step 5: Commit** `test(fake): a path-targeted read_dir fault for the verify property test`.

### Task 2: Dependency, module skeleton, the two-side derivation

**Files:**
- Modify: root `Cargo.toml` (line 95), `crates/flux-core/Cargo.toml` (`[dev-dependencies]`, lines 19-21), `crates/flux-core/src/lib.rs` (after line 29), `Cargo.lock` (by cargo; stage it).
- Create: `crates/flux-core/src/verify_proptest.rs`.

**Interfaces:**
- Produces (design 6.3, 8):

```rust
type RelPath = Vec<&'static str>;                 // 1 to 4 components; joined with Path::join
enum Which { Source, Dest }
enum Spec { File { len: usize, fill: u8 }, Dir { mount: MountRoot, list_error: Option<Code> }, Symlink { payload: &'static str }, Other }
enum Edit { KeepBoth, EditContent(Which), ChangeKind(Which, Spec), DropFromDest, DropFromSource }
struct Case { base: Vec<(RelPath, Spec)>, edits: Vec<Edit>, extras: Vec<(Which, RelPath, Spec)>, mounts: MountPolicy, links: Links }
fn sides(case: &Case) -> (BTreeMap<PathBuf, Spec>, BTreeMap<PathBuf, Spec>)   // (source, dest), relative paths
```

- Derivation rules, in this order: (1) deduplicate `base` by path, first wins; the edit of a losing entry is ignored. (2) Both sides start as the deduplicated base. (3) For each surviving `base[i]`, apply `edits[i]`: `KeepBoth` nothing; `EditContent(w)` on side `w`: a `File` with `len == 0` gets `len: 1` and keeps its `fill`, otherwise it gets `fill: fill.wrapping_add(1)`; a `Symlink` gets the other payload (`"t"` to `"u"`, anything else to `"t"`); `Dir` and `Other` unchanged. `ChangeKind(w, spec)`: side `w`'s entry becomes `spec`. `DropFromDest` / `DropFromSource`: remove from that side. (4) Append each extra to its side; an extra whose path already exists on that side is dropped. (5) Parent pruning per side, shallow to deep: the root counts as a `Dir`; every entry whose parent is absent or not a `Dir` on that side is removed (a removed directory removes its descendants). `sides` does NOT apply the walker view of mount roots (the reference does).

- [ ] **Step 1: Add the dependency and the module.** Both Cargo lines per Global Constraints; the `lib.rs` line; create the file with the licence header and the types above; `cargo check -p flux-core --tests` compiles.
- [ ] **Step 2: Write the failing test:**

```rust
#[test]
fn sides_applies_edits_extras_dedupe_and_parent_pruning() {
    let dir = Spec::Dir { mount: MountRoot::No, list_error: None };
    let f = |len, fill| Spec::File { len, fill };
    let link = |p| Spec::Symlink { payload: p };
    let case = Case {
        base: vec![
            (vec!["a"], dir.clone()), (vec!["a", "x"], dir.clone()), (vec!["a", "x", "y"], f(1, b'y')),
            (vec!["b"], f(0, b'b')), (vec!["b"], f(1, b'B')), (vec!["l"], link("t")), (vec!["o"], Spec::Other),
        ],
        edits: vec![
            Edit::ChangeKind(Which::Dest, f(1, b'a')), Edit::KeepBoth, Edit::KeepBoth,
            Edit::EditContent(Which::Source), Edit::DropFromDest, Edit::EditContent(Which::Dest), Edit::EditContent(Which::Source),
        ],
        extras: vec![(Which::Dest, vec!["a", "z"], f(1, b'z')), (Which::Source, vec!["c"], dir.clone()), (Which::Source, vec!["b"], f(9, b'!'))],
        mounts: MountPolicy::Cross, links: Links::Copy,
    };
    let (s, d) = sides(&case);
    let keys = |m: &BTreeMap<PathBuf, Spec>| m.keys().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>();
    assert_eq!(keys(&s), ["a", "a/x", "a/x/y", "b", "c", "l", "o"]);
    assert_eq!(keys(&d), ["a", "b", "l", "o"]);
    assert_eq!(s[&PathBuf::from("b")], f(1, b'b'));            // EditContent on an empty file: len 1, fill unchanged
    assert_eq!(d[&PathBuf::from("b")], f(0, b'b'));            // the dedupe loser's DropFromDest was ignored
    assert_eq!(d[&PathBuf::from("a")], f(1, b'a'));            // ChangeKind on dest; a/x, a/x/y, a/z pruned there
    assert_eq!(d[&PathBuf::from("l")], link("u"));             // EditContent on a symlink swaps the payload
    assert_eq!(s[&PathBuf::from("o")], Spec::Other);           // EditContent is a no-op on Other
    assert_eq!(s[&PathBuf::from("b")].clone(), f(1, b'b'));     // the extra at an existing path was dropped
}
```

(Derive `Clone, Debug, PartialEq, Eq` on `Spec`, `Edit`, `Which`; `MountRoot`, `MountPolicy`, `Links` already derive what is needed, else wrap them.)
- [ ] **Step 3: Run** `cargo nextest run -p flux-core verify_proptest` ; expected: fails (unimplemented).
- [ ] **Step 4: Implement `sides`.** - [ ] **Step 5: Run** the same ; expected PASS.
- [ ] **Step 6: `cargo deny check`** ; expected: `advisories ok, bans ok, licenses ok, sources ok`. If it fails, STOP and report the failing crate and licence to the controller; do not edit `deny.toml`.
- [ ] **Step 7: Commit** `test(core): proptest dev-dependency and the verify property test's case model` (incl. `Cargo.lock`).

### Task 3: Materialisation and the reference

**Files:** Modify `crates/flux-core/src/verify_proptest.rs`.

**Interfaces:**
- Consumes Task 1's `fail_read_dir_of`, Task 2's types and `sides`.
- Produces: `fn materialise(case: &Case) -> (FaultFs, usize)`; `fn reference(case: &Case) -> (Vec<Record>, Summary)`; `type Row = (String, Outcome, Kind, Option<Code>, String)` and `fn row(r: &Record) -> Row` (path components joined with `/`, as `verify::tests::row` does).

**`materialise`** (design 6.4): for each side from `sides(case)`, with root `/s` or `/d`: `create_dir(root)`; every `Dir` parents-first (sort by component count); then `write_file(path, vec![fill; len])`, `add_symlink_to(path, payload, LinkKind::Unspecified)`, `add_special(path)`; `set_mount_root(path, mount)` for every `Dir` whose `mount != No`; `fail_read_dir_of(path, code, kind)` for every NON-ROOT `Dir` with `list_error: Some(code)` (kind per Global Constraints). Return the fake and `fs.calls().len()` after setup.

**`reference`** (design 6.5), over `(src, dst) = sides(case)`:
0. Walker view: when `case.mounts == Stay`, drop from each side every entry strictly below a `Dir { mount: Yes }` of that side.
1. When `case.links == Skip`, drop every source `Symlink`.
2. Destination control state, dropped: a top-level `Dir` named `.flux` and everything below a top-level `.flux`; a top-level `File` named `.flux-root.lock`; any `File` or `Symlink` whose name is `<head>.flux-partial.<32 lowercase hex>` with a non-empty head. Kept: a top-level `File` named `.flux`, a top-level `Dir` named `.flux-root.lock`, a 31-hex or uppercase id, an empty head, a nested `.flux`, a `Dir` named like a partial. The source is never filtered.
3. Key both sides by `FluxPathKey::from_relative(path).unwrap().0` into `BTreeMap<Vec<u8>, (PathBuf, Spec)>`.
4. Merge in key order with `pruned: Option<Vec<u8>>` shared by both sides; an entry is under the prefix when its key equals it or starts with it followed by `0x00`; skip such entries on both sides. A `Dir` FAILED means `list_error: Some` and not (`mounts == Stay` and `mount == Yes`). Then, with `s`/`d` the next unskipped entries: source only (or destination exhausted): failed -> `Unreadable`, kind `Dir`, `Some(code)`, text, set `pruned`; else `Missing`, `None`, `""`. Destination only: failed -> `Extra`, kind `Dir`, `Some(code)`, text, set `pruned`; else `Extra`, `None`, `""`. Equal keys: either failed -> `Unreadable`, kind = the SOURCE entry's kind, `(code, text)` of the source if it failed else of the destination, set `pruned`; else `Dir`/`Dir` -> `Match`, detail `MOUNT_NOT_ENTERED` and `not_entered += 1` and set `pruned` when `mounts == Stay` and either `mount == Yes`, else detail `""`; `File`/`File` -> digest both with `flux_hash::Hasher` over `vec![fill; len]`, `bytes_hashed += len_s + len_d`, equal -> `Match`, else `Mismatched`, `Some(VerifyMismatch)`, `format!("{} != {}", &s.to_hex()[..8], &d.to_hex()[..8])`; `Symlink`/`Symlink` -> payload bytes equal -> `Match`, else `Mismatched`, `Some(VerifyMismatch)`, `"symlink payloads differ"`; `Other`/`Other` -> `Unreadable`, `Some(SpecialFileUnsupported)`, `SPECIAL_NOT_COMPARED`; any other pair -> `Mismatched`, kind = source kind, `Some(VerifyMismatch)`, `format!("source {}, destination {}", ks.as_str(), kd.as_str())`.
5. `Summary`: one count per outcome, `not_entered`, `bytes_hashed`.

- [ ] **Step 1: Write the failing tests.** (a) `the_reference_agrees_with_three_fixtures`: three hand-built `Case`s mirroring `a_type_mismatch_drains_the_children_as_extra_or_missing` (`verify.rs:829` onward), `a_mount_point_on_either_side_is_matched_once_and_pruned` (both halves, `verify.rs:1479-1499`, `MountPolicy::Stay`) and `a_destination_only_directory_that_cannot_be_listed_is_extra_with_its_error_and_never_fails` (`verify.rs:1075-1094`: source file `a`; dest file `a`, dir `lost` with file `x`, file `z`; `lost` has `list_error: Some(PermissionDenied)`); the expected rows are copied from those fixtures' asserted rows; for the third compare `(path, outcome, kind, code)` only. (b) `the_reference_hides_a_mount_roots_children_on_its_side_under_stay`: source file `m`, dest `Dir { mount: Yes }` `m` with child file `x`, `Stay`: exactly `[("m", Mismatched, File, Some(VerifyMismatch), "source file, destination dir")]`, `not_entered == 0`. (c) `the_reference_gives_a_failed_pair_the_source_kind`: source file `p`, dest `Dir { list_error: Some(IoError) }` `p`, `Cross`: `[("p", Unreadable, File, Some(IoError), "other error")]`. (d) `materialise_builds_what_sides_describe`: for EACH of (a)'s three cases, materialise, run `verify_tree(&fs, Path::new("/s"), Path::new("/d"), &VerifyOptions { links, mounts, hash: HashAlgorithm::Blake3 }, &mut |r| rows.push(row(&r)))`, and compare with the reference's rows mapped through `row`.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core verify_proptest` ; expected: the four fail. - [ ] **Step 3: Implement `materialise` and `reference`.** - [ ] **Step 4: Run** ; expected PASS.
- [ ] **Step 5: Commit** `test(core): the verify reference model and the fake materialisation`.

### Task 4: Strategies and the census

**Files:** Modify `crates/flux-core/src/verify_proptest.rs`.

**Interfaces:**
- Produces: `fn case() -> impl Strategy<Value = Case>`.

**Generator** (design 6.3): names from `a`, `a-b`, `a.txt`, `a0`, `b`, `sub`, `z`, plus `.flux-root.lock` usable ONLY as a `File` name on either side; a `RelPath` is 1 to 4 components; `base` has 3 to 10 entries, built so every entry's parent is a `Dir` of the base (grow directories first, then place children under them); `File` sizes from `{0, 1, 17, COPY_BUF_MAX + 1}` with the large one at most one file in eight; `Symlink` payloads from `{"t", "u"}`; `mount` from `{No, Yes, Unknown}` with `Yes` about one in four directories; `list_error` `Some` for about one in five non-root directories, code from the two allowed; `edits` one per base entry, `KeepBoth` with about half the weight and the other four sharing the rest, `Which` even; `extras` 0 to 4, each on a side chosen evenly: half ordinary names with any `Spec` placed under the root or any `Dir` of that side, half (destination side only) from the control-state list: at the TOP LEVEL `.flux` as a `Dir` with a child file, `.flux` as a `File`, `.flux-root.lock` as a `File`, `.flux-root.lock` as a `Dir`; under the root or any `Dir`: `name.flux-partial.<32 lowercase hex>` as a `File` and as a `Symlink`, `name.flux-partial.<31 hex>`, `name.flux-partial.<32 UPPERCASE hex>`, `.flux-partial.<32 lowercase hex>`, a `Dir` named `name.flux-partial.<32 hex>`, and `.flux` as a `Dir` under a `Dir`; `mounts` from `{Stay, Cross}` and `links` from `{Copy, Skip}`, each even.

- [ ] **Step 1: Write the failing test** `generated_cases_reach_every_outcome_and_every_prune_arm` (design 9.3): 512 cases from `TestRunner::new_with_rng(ProptestConfig::default(), TestRng::from_seed(RngAlgorithm::ChaCha, &[7u8; 32]))` and `case().new_tree(&mut runner).unwrap().current()`; run `reference` on each; detect each category from the `Case` (`base`, `edits`, `extras`, `mounts`, `links`), `sides(&case)` and the reference's output, never from the engine; assert at least 5 of each: each of the five outcomes; a source-only failed `Dir`; a destination-only failed `Dir`; a both-sides failed `Dir` with DIFFERENT codes (mutant (p)); a fault on exactly one side of a path both hold; `mount == Yes` on exactly one side of a path both hold; a failed destination `Dir` paired with a source `File`; a mount fold (`not_entered > 0`); a `Dir { mount: Yes }` under `Stay` with children on its side; a pruned directory followed by a later sibling whose name starts with the pruned name's bytes (`a` then `a-b`, `a.txt` or `a0`; mutant (h)); two prunes in one case, each with at least one descendant on some side that survives the reference's steps 0 to 2 and is therefore skipped only by the prune (mutant (n)); a control-state drop (an `extras` entry on the destination that the reference's step 2 removes); a `ChangeKind(Source, ..)` and a `ChangeKind(Dest, ..)` on a surviving entry; a subtree on one side under a non-directory on the other; a source `Symlink` removed by `Links::Skip`; a source `.flux-root.lock` `File` reported `Missing` (Review Focus 1); a `Symlink` on both sides at one path reported `Extra` under `Links::Skip` (Review Focus 2); a `Dir { mount: Yes, list_error: Some }` under `Cross` reported failed (Review Focus 3).
- [ ] **Step 2: Run** ; expected: fails (no strategy). - [ ] **Step 3: Implement `case()`**, tuning weights within the ranges above until every category reaches 5 with that seed. If a category cannot reach 5 within those ranges, STOP and report the category and the best count to the controller (design 7, call 6: the owner rules on weights).
- [ ] **Step 4: Run** ; expected PASS.
- [ ] **Step 5: Commit** `test(core): the verify case generator and its coverage census`.

### Task 5: The property, its invariants, and the pasteable fixture

**Files:** Modify `crates/flux-core/src/verify_proptest.rs`; possibly create `crates/flux-core/proptest-regressions/verify_proptest.txt` (only if proptest writes it).

**Interfaces:**
- Produces: `fn fixture_snippet(case: &Case) -> String` and the property `the_streaming_merge_equals_the_reference`.

**`fixture_snippet`** renders, from `sides(case)`, one block per side in the style of `verify::tests`: `tree(&fs, "/s", &["dir", ..], &[("file", bytes), ..]);` where a file's bytes are the expression `&vec![b'<fill>'; <len>]` (so a 256 KiB file is one short line), then one line each `fs.add_symlink_to("/s/<path>", "<payload>", LinkKind::Unspecified);`, `fs.add_special("/s/<path>");`, `fs.set_mount_root("/s/<path>", MountRoot::<v>);`, `fs.fail_read_dir_of("/s/<path>", Code::<c>, std::io::ErrorKind::<k>);`, then `let o = VerifyOptions { links: Links::<l>, mounts: MountPolicy::<m>, hash: HashAlgorithm::Blake3 };`.

- [ ] **Step 1: Write and pass two unit tests.** `an_empty_pair_of_trees_gives_no_records_and_a_default_summary` (Review Focus 4): a `Case` with empty `base`, `edits`, `extras`; both `reference` and the engine on `materialise` give `[]` and `Summary::default()`. `fixture_snippet_renders_every_kind_of_line`: a hand-built `Case` with one `Dir { mount: Yes, list_error: Some(IoError) }` holding a `File { len: COPY_BUF_MAX + 1, fill: b'q' }`, a `Symlink`, an `Other`, under `Stay`/`Skip`; assert the output contains the `tree(` line for each side, `&vec![b'q'; 262145]`, the four `fs.` lines and the options line (exact substrings).
- [ ] **Step 2: Write the property** inside `proptest! { #![proptest_config(ProptestConfig::default())] #[test] fn the_streaming_merge_equals_the_reference(case in case()) { .. } }`: `let (fs, setup) = materialise(&case);` run `verify_tree(&fs, Path::new("/s"), Path::new("/d"), &VerifyOptions { links: case.links, mounts: case.mounts, hash: HashAlgorithm::Blake3 }, &mut |r| rows.push(r))`; then in this order: `prop_assert_eq!(rows, expected_rows, "{}", fixture_snippet(&case))`; `prop_assert_eq!(summary, expected_summary, "{}", fixture_snippet(&case))`; the rows' keys (`FluxPathKey::from_relative(&r.path).unwrap().0`) strictly increasing; for every logged call starting with `open_read(`, strip the prefix and the root (`/s/` or `/d/`) to get the relative path and assert that path is a `File` on that side in `sides(&case)`; every call at index `>= setup` starts with one of `metadata(`, `read_dir(`, `open_read(`, `read_link(`, `source_mount_root(`, `canonical_path(`, `identity(`.
- [ ] **Step 3: Run** `cargo nextest run -p flux-core the_streaming_merge_equals_the_reference` ; expected PASS. If it FAILS: never touch the engine. Classify from the printed snippet: (i) `sides`, `materialise`, `case()` or `fixture_snippet` wrong: fix in this module; (ii) the reference misreads a design rule: fix the reference and cite the design line in the commit; (iii) the engine and the design disagree, or invariant 3 or 4 fails: STOP and report the snippet to the controller as a finding. If proptest wrote `proptest-regressions/verify_proptest.txt`, `git add` it.
- [ ] **Step 4: `just check`** ; expected exit 0 (`license` and `doc-check` included).
- [ ] **Step 5: Commit** `test(core): the verify merge property against the reference model`.

### Task 6: The fixture the peer's interleaving earned

**Files:** Modify `crates/flux-core/src/verify.rs` `mod tests` only, after `a_mount_point_present_on_one_side_only_is_not_a_match` (line 1502 onward).

- [ ] **Step 1: Write the test** (design 9.5; `mm(p: &str, k: Kind, d: &str) -> Row` is the module's helper at line 916):

```rust
#[test]
fn a_source_file_against_a_destination_mount_point_is_a_mismatch_and_nothing_below_it_is_seen() {
    let o = VerifyOptions { mounts: MountPolicy::Stay, ..opts() };
    let fs = FaultFs::new();
    tree(&fs, "/s", &[], &[("m", b"m")]);
    tree(&fs, "/d", &["m"], &[("m/x", b"x")]);
    fs.set_mount_root("/d/m", MountRoot::Yes);
    let (rows, s) = run(&fs, "/s", "/d", &o);
    assert_eq!(rows, vec![mm("m", Kind::File, "source file, destination dir")]);
    assert_eq!((s.mismatched, s.not_entered, s.extra), (1, 0, 0));
    assert!(!fs.called("read_dir(/d/m)"));
}
```

- [ ] **Step 2: Run** `cargo nextest run -p flux-core a_source_file_against_a_destination_mount_point` ; expected PASS (the engine already behaves so; this pins it). If it FAILS, STOP and report: the design's reading of the walker is wrong.
- [ ] **Step 3: Commit** `test(core): pin a source file against a destination mount point`.

### Mutants the driver runs (not an implementer task; design 2 success criterion)

After Task 6, on the committed tip, each mutant applied alone, `cargo nextest run -p flux-core --no-fail-fast` run, then reverted. "Fixtures" means every `verify::tests` test including Task 6's (40 after Task 6).

| id | change in `crates/flux-core/src/verify.rs` | expected |
|---|---|---|
| (h) | `under_pruned`: `path.as_os_str().as_encoded_bytes().starts_with(p.as_os_str().as_encoded_bytes())` | fixtures green; property RED |
| (n) | `prune`: `self.pruned.get_or_insert_with(\|\| path.to_path_buf());` instead of assignment | fixtures green; property RED |
| (p) | `pair`: `y.failed.as_ref().or(x.failed.as_ref())` | fixtures green; property RED |
| control | Task 6's fixture: `assert!(fs.called("read_dir(/d/m)"))` (negation dropped) | that fixture RED |

Fewer than two of (h), (n), (p) fixture-green-and-property-red is reported to the owner, who rules on the criterion (design 2). The design's "few seconds per OS" anti-goal is not measured by this plan (a timing run needs the owner's approval under the repository's timing rule); the one-in-eight cap on 256 KiB files bounds the hashing work instead.

## Stand-downs (adversarial panel on this plan: a five-seat solo panel and one agy round)

- REJECTED by measurement: "a member-level `default-features = false` on an inherited dependency is a hard error under edition 2024". On cargo 1.99.1 (the `stable` channel CI uses) a throwaway workspace built cleanly with the member opting out and `cargo tree -e features` showed the default feature off, while the defaults-on control showed it on. The workspace-level opt-out was adopted anyway as the documented form.
- DISCARDED-BELOW-FLOOR: the empty-tree unit test's placement (Task 5 rather than Task 3): it is a smoke test before the property, where Review Focus 4 lives.
- DISCARDED-BELOW-FLOOR: step formatting (several steps on one line).
- Everything else the solo panel raised (inline rules for Tasks 3 to 5, the code-to-kind pairs, `s8`/`d8`, the four derivation rules, a stronger Task 2 test, the three mutant-shaped census categories, Review Focus 2 redefined, the `Inner` `Default` derive, the failure branches of Tasks 2, 4 and 5, a `fixture_snippet` test, the `open_read` path mapping, the fixture count, the large-file cap) was folded into the text above. The agy round on the pre-fold text had no findings; its hand trace of the Task 2 test confirmed the expected values, and its note that a type change must be detected from `case.edits` was folded.
