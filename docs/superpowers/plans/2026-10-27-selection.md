# Selection (`--exclude`, `--recursive` / `--no-recursive`, `excluded` count) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy` and `flux verify` accept `--exclude PATTERN` (repeatable) and `--recursive` / `--no-recursive`, select through the shared walker, refuse a resume whose selection changed, and report one `excluded` count.

**Architecture:** A pure matcher (`Selection`) in `flux-fs` is compiled once (CLI, or state decode) and carried in `CopyOptions` / `VerifyOptions`. `Walk` consults it before dispatching each entry, so an excluded directory is never entered. The count rides out of the walk in `copy_tree_frames` and in verify's source `Side`.

**Tech Stack:** Rust workspace (`flux-fs`, `flux-core`, `flux-cli`); clap derive; `just check` is the gate.

**Spec:** `docs/superpowers/specs/2026-10-27-selection-design.md` (design, panel GREEN at 4a4fc5bb1946d25d260076e375ee023a6819155c). Plan base: main 67a03b8f6d9520e6d0cb5cdce7806bfb4b778620; every `file:line` below was read at that code.

## Plan rulings (deviations from the design text, ledgered)

- **P1. Placement.** Design 5.3 puts `select.rs` in `flux-core` and keeps raw `Vec<String>` + `bool` in `CopyOptions`, compiled by the engine. The design's raw-strings shape was implementable, but it forces a fallible compile inside the engine (copy and verify). To keep the engine infallible this plan puts the compiled type where `CopyOptions` lives: `flux-fs` (which `flux-core` depends on). It therefore puts `flux_fs::Selection` (compiled, `Clone`, `Default`) in `CopyOptions` and `VerifyOptions` as ONE field `selection`, so the engine has no fallible compile. Compilation happens in the CLI (usage error, exit 2) and in state decode (`Corrupt`). Every behaviour in the design is unchanged.
- **P2. Walker plumbing.** Design 5.3 puts a handle in `WalkOptions`. That struct is `Copy` and built at ~8 sites, so this plan leaves it alone and adds `Walk::set_selection(&mut self, &'a Selection)`, called after `prepare_source` at its three production sites.
- **P4. Design amendments.** Task 7 amends design 5.3, R10 (matcher in `flux-fs`) and the section 6 mutant "validate the pattern in the engine" (replaced by "validate after the first write").
- **P3. Verify options.** `VerifyOptions` gains `selection: Selection` and loses `Copy` (the compiler names every copy site).

## Global Constraints

- Gate: `just check` (fmt-check, clippy `-D warnings`, typos, unused-deps, doc-check, license, test). `just test` is `cargo nextest run --workspace --no-tests=pass` then `cargo test --doc --workspace`. Expected pass line: nextest `Summary ... N passed ... 0 failed`, exit 0.
- Typos gate: never write a short git sha in prose or commit bodies; use `fil` (not a word the spell checker corrects) in pattern examples.
- Licence header on every new `.rs` file (copy the first 5 lines of `crates/flux-core/src/walk.rs`).
- Grammar and limits, verbatim from design 5.1: pattern <= 1024 UTF-8 bytes, <= 64 segments; across the command line <= 64 patterns and <= 16384 bytes of pattern text; control characters are Unicode Cc (U+0000-U+001F, U+007F-U+009F); excluded literal characters `/ \ * ? ! [ ]`; `**` only as a whole segment.
- Every pre-existing fingerprint, manifest and JSON key set stays byte-identical when the selection is the default (no patterns, recursive).
- Commit trailer: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. Never `just pr` before every gate is done.

## Review Focus

Inputs the spec implies and no task's happy path exercises; each has its test in the named task.
1. A name that is not valid UTF-8 (Linux bytes; Windows lone surrogate) against `?` and literals: byte mode, no panic, no wrong match (Task 1).
2. 64 patterns of 1024 bytes: the CLI refuses at the aggregate cap, so a manifest never exceeds `STATE_LIMIT`; a stored list that is unsorted or fails to compile reads as `Corrupt` (Tasks 1, 4, 6).
3. `--exclude '**'` and a tree whose every entry is excluded: copy exits 0 with `excluded` = top-level entries and an empty destination; verify exits 0 (Tasks 3, 5).
4. An unreadable, cyclic or too-deep directory UNDER an excluded directory raises no walk error and no depth error (Task 2).
5. A hardlink group whose first-walked (canonical) member is excluded, with `--hardlinks=preserve`: the remaining member copies normally and nothing fails (Task 3).

## File Structure

- Create `crates/flux-fs/src/select.rs`: `Pattern`, `Selection`, `Verdict`, `SelectError`, limits. One responsibility: decide, from a relative path and a kind, whether an entry is kept.
- Modify `crates/flux-fs/src/{lib.rs,options.rs}`: export; `CopyOptions.selection`.
- Modify `crates/flux-core/src/walk.rs`: filter in `Walk::next`, `set_selection`, `excluded()`.
- Modify `crates/flux-core/src/{tree.rs,run/mod.rs,run/multi.rs}`: attach the selection; `TreeOutcome.excluded`; count in `copy_tree_frames`.
- Modify `crates/flux-core/src/{state.rs,run/resume.rs}`: fingerprint, manifest, refusal.
- Modify `crates/flux-core/src/verify.rs` (and `verify_proptest.rs`): both sides filtered, `Summary.excluded`.
- Modify `crates/flux-cli/src/{main.rs,report.rs,verify_report.rs}` plus tests, README, ROADMAP, TODO.

---

### Task 1: The matcher (`flux_fs::select`)

**Files:**
- Create: `crates/flux-fs/src/select.rs` (tests in its `mod tests`)
- Modify: `crates/flux-fs/src/lib.rs` (add `pub mod select;` and `pub use select::{Pattern, Selection, SelectError, Verdict, MAX_PATTERNS, MAX_PATTERN_BYTES, MAX_PATTERN_SEGMENTS, MAX_TOTAL_PATTERN_BYTES};`)

**Interfaces:**
- Produces:
  - `pub const MAX_PATTERN_BYTES: usize = 1024; MAX_PATTERN_SEGMENTS: usize = 64; MAX_PATTERNS: usize = 64; MAX_TOTAL_PATTERN_BYTES: usize = 16 * 1024;`
  - `pub enum SelectError` (`Debug, Clone, PartialEq, Eq`, `impl Display + Error`), one variant per rejection with the pattern and the reason in its message (a backslash says "use `/` as the separator").
  - `pub struct Pattern` with `pub fn parse(s: &str) -> Result<Pattern, SelectError>` and `pub fn as_str(&self) -> &str`.
  - `pub enum Verdict { Keep, Excluded, Skipped }` (`Debug, Clone, Copy, PartialEq, Eq`).
  - `pub struct Selection` (`Debug, Clone, PartialEq, Eq`, `Default` = no patterns, recursive) with `pub fn compile(patterns: &[String], recursive: bool) -> Result<Selection, SelectError>` (parses each, enforces `MAX_PATTERNS` and `MAX_TOTAL_PATTERN_BYTES` on the list AS GIVEN, before deduplication, then sorts and deduplicates the typed strings), `pub fn patterns(&self) -> &[String]` (sorted, deduplicated, as typed), `pub fn recursive(&self) -> bool`, `pub fn is_default(&self) -> bool`, `pub fn select(&self, rel: &std::path::Path, ft: crate::FileType) -> Verdict`, and `#[cfg(test)] pub(crate) fn select_counted(&self, rel: &Path, ft: FileType, steps: &mut u64) -> Verdict` (the generic counted entry point; `select` and it share one private function generic over a counter, so `Selection` stays `Sync`).

Behaviour the signatures do not determine (design 5.1, authoritative): strip ONE trailing `/` first (sets dir-only), then a leading `/` (sets anchored); anchored also when an inner `/` remains; `select` reads `rel.components()` (`Component::Normal` only) and NEVER splits path bytes; unanchored patterns see only the LAST component; anchored patterns must match the whole path; interior or leading `**` matches zero or more components, trailing `**` (including bare `**`) matches one or more; `*`/`?` match a leading `.`; `?` is one Unicode scalar when the component is valid UTF-8 (`OsStr::to_str`), else one byte of its encoded form, decided once per component; case-sensitive, no normalisation; a dir-only pattern matches only `FileType::Dir`; patterns are tested before recursion: if any pattern matches -> `Excluded`, else if `!recursive && ft == Dir` -> `Skipped`, else `Keep`. Wildcard matching is the iterative two-pointer form (backtrack only to the last star), so cost is `components x segments x glob length`.

- [ ] **Step 1: Write the failing tests** in `select.rs`. A helper `fn rel(parts: &[&str]) -> PathBuf` builds with `PathBuf::join` (never a `/` literal, so Windows CI sees `\`). Tests:
  - `fn the_design_table_is_decided_exactly()`: a table of `(pattern, rel, FileType, expected Verdict)` asserting: `private/**` vs `private/file` File => Excluded, vs `private` Dir => Keep, vs `private/a/b` => Excluded; `private` and `private/` vs Dir `private` => Excluded; `private/` vs File `private` => Keep; `*.tmp` vs `cache.tmp` and vs `sub/deep/cache.tmp` => Excluded; `sub/*.tmp` vs `sub/x.tmp` => Excluded, vs `other/sub/x.tmp` => Keep, vs `sub/x.tmp/y` => Keep; `/foo` vs `foo` => Excluded, vs `a/foo` => Keep; `foo` vs `a/b/foo` => Excluded; `docs/**/notes.txt` vs `docs/notes.txt` and `docs/2026/10/notes.txt` => Excluded; `build/*` vs `build/x/y` => Keep; `**` and `/**` vs any entry => Excluded; `**/` vs Dir `d` => Excluded and vs File `f` => Keep; `**/foo` vs `foo` and `a/foo` => Excluded; `foo/` vs Dir `a/b/foo` => Excluded and vs File => Keep; `*` vs `.hidden` => Excluded; `README` vs `readme` => Keep; `É` vs `é` => Keep; `fil?` vs `filé` => Excluded; `fil??` vs `filé` => Keep.
  - `fn question_mark_counts_scalars_for_valid_names_and_bytes_otherwise()`: `€` and an emoji each match one `?`; on `cfg(unix)` use `OsStr::from_bytes` for the invalid name `fil\xff`: `fil?` => Excluded and the invalid pair `\xe2\x82` against `??` => Excluded; the mode is per component: against the path `é` / `fil\xff`, the anchored pattern `?/fil?` matches (the valid component is read in scalars, the invalid one in bytes).
  - `fn recursion_skips_directories_after_patterns()`: `Selection::compile(&[], false)`: Dir => Skipped, File/Symlink/Other => Keep; with pattern `build/` and Dir `build` => Excluded (patterns first); a Symlink named like a directory under `--no-recursive` => Keep.
  - `fn invalid_patterns_are_refused_with_a_reason()`: `""`, `/`, `a//b`, `..`, `a/../b`, `./a`, `a\\b` (message contains "use `/`"), `a!b`, `[a]`, `a**`, `**a`, `***`, `a*b**`, `**/**`, one containing `\n`, `\t`, U+0085, NUL; a 1025-byte pattern; a 65-segment pattern => `Err`; a 1024-byte pattern and a 64-segment pattern => `Ok`.
  - `fn the_aggregate_caps_apply()`: 65 DISTINCT patterns => Err and 64 distinct => Ok; 65 copies of one pattern => Err (the cap is before dedup); 17 distinct patterns of 1024 bytes (17408 > 16384) => Err; 16 => Ok.
  - `fn compile_sorts_and_deduplicates()`: `["b","a","a"]` and `["a","b"]` give equal `Selection`s with `patterns() == ["a","b"]`; `Selection::default().is_default()`; `compile(&[], false)` is not default.
  - `fn a_pathological_pattern_stays_within_a_step_bound()`: the pattern `a/**` repeated 32 times then `/b` (64 segments), a non-matching `rel` of 200 components all `a`, via `select_counted`: `steps <= 200 * 64 * 8`; and `a*a*a*a*a*a*a*a*a*b` vs a long `aaaa...` component stays within `len * 40`.
  - `fn one_entry_two_patterns_is_one_verdict()`: `Verdict::Excluded` (the count is the walker's job, Task 2).
- [ ] **Step 2: Run to verify failure.** `pub mod select;` and the exports are added in Step 1 together with stub signatures that `todo!()`, so the tests compile. Run: `cargo nextest run -p flux-fs select`. Expected: the new tests FAIL (panic at `todo!()`), not zero tests run.
- [ ] **Step 3: Implement** the types and functions above in `select.rs`; export in `lib.rs`.
- [ ] **Step 4: Run to verify pass.** Run: `cargo nextest run -p flux-fs select` => all pass; `cargo clippy -p flux-fs --all-targets -- -D warnings` => clean.
- [ ] **Step 5: Mutants (by hand, then restore).** Each must turn a named test red: make trailing `**` zero-or-more; make `?` always a byte; invert `dir_only`; test recursion before patterns; split `rel` on the byte `\` as well as `/`; drop the dedup; allow `a**`. Record the test that went red in the ledger.
- [ ] **Step 6: Commit.** `git add crates/flux-fs && git commit -m "feat: the --exclude matcher in flux-fs (cut 16 task 1)"` (with the trailer).

---

### Task 2: The walker filter and its count

**Files:**
- Modify: `crates/flux-core/src/walk.rs` (struct `Walk` ~line 120, `walk_with` ~line 199, `Walk::next` ~lines 266-349; tests in its `mod tests`)

**Interfaces:**
- Consumes: `flux_fs::{Selection, Verdict}` (Task 1).
- Produces: `impl<'a, F: FileSystem> Walk<'a, F> { pub fn set_selection(&mut self, selection: &'a Selection); pub fn excluded(&self) -> u64 }`. `Walk` gains private `selection: Option<&'a Selection>` and `excluded: u64`, both initialised in `walk_with` (`None`, `0`). `WalkOptions`, `walk`, `walk_with`, `walk_with_depth` are unchanged.

In `Walk::next`, after the entry's path and kind are known (`frame.rel.join(&e.name)`, `walk.rs:326`) and BEFORE the `match step` (`walk.rs:330`): when a selection is set, call `select(&path, file_type)`; `Excluded` => `self.excluded += 1` and `continue` the loop; `Skipped` => `continue`; `Keep` => proceed. The check must precede `enter`, so no `metadata`, `mount_root` or `read_dir` happens for a dropped directory, and the depth check never runs for it.

- [ ] **Step 1: Write the failing tests** (use `FaultFs` as the neighbouring tests at `walk.rs:560-600` do; reuse their `paths` helper style, adding `paths_selected(fs, &Selection) -> Vec<String>`):
  - `fn an_excluded_file_symlink_and_other_entry_are_dropped_and_counted()`: names matched by `*.tmp`; the list omits them; `excluded() == 3`.
  - `fn an_excluded_directory_is_never_entered()`: tree `/r/private/{f, deep/g}`, pattern `private`; no event for `private` or below; `excluded() == 1`; the `FaultFs` call log has no `read_dir`/`metadata` for `/r/private` (use the call-log accessor the neighbouring mount tests use).
  - `fn a_directory_at_depth_two_is_pruned_too()`: `/r/a/b/skip/x`, pattern `skip`; `skip` and `x` absent, `excluded() == 1`.
  - `fn private_star_star_keeps_the_directory_and_filters_its_children()`: pattern `private/**`; `D private` and `E private` present, children absent, `excluded() == 2` for two children.
  - `fn two_patterns_on_one_entry_count_once_and_two_entries_in_one_directory_count_two()`.
  - `fn no_recursive_yields_only_top_level_non_directories_without_a_depth_error()`: selection `compile(&[], false)`; `excluded() == 0`; no `Err` item; a top-level symlink to a directory is yielded as `L`.
  - `fn a_directory_removed_by_both_a_pattern_and_no_recursive_counts_once()`.
  - `fn exclusions_in_the_root_and_at_depth_two_sum()` (kills a per-frame counter).
  - `fn nothing_under_an_excluded_directory_can_fail_the_walk()`: inject a `read_dir` fault under `skip`, pattern `skip`; the walk yields no `Err`. `fn an_excluded_directory_beyond_the_depth_cap_raises_no_depth_error()`: a chain deeper than the cap (via `walk_with_depth(.., 2)`) whose third-level directory is excluded; no `Err` item (kills a filter placed after the depth check).
  - `fn an_excluded_mount_directory_emits_no_note()`: with `MountPolicy::Stay` and a mount root named `m` excluded: no `MountSkipped`/`MountUnknown`.
  - `fn the_root_is_never_filtered()`: pattern `**` leaves no entry but yields no error; `excluded()` equals the number of top-level entries.
  - `fn an_anchored_multi_component_pattern_matches_at_depth_two()`: pattern `sub/*.tmp` against the real `Walk` output (this is the test that exercises the OS separator on Windows CI).
- [ ] **Step 2: Run to verify failure.** Run: `cargo nextest run -p flux-core walk::` => the new tests fail to compile.
- [ ] **Step 3: Implement** the two fields, `set_selection`, `excluded`, and the filter in `next`.
- [ ] **Step 4: Run to verify pass.** Run: `cargo nextest run -p flux-core walk::` => all pass (existing walker tests unchanged).
- [ ] **Step 5: Mutants.** Drop the `continue` for `Excluded`; count `Skipped`; move the filter inside `enter` after `metadata`; run it after the depth check; reset `excluded` per frame. Each must turn a named test red.
- [ ] **Step 6: Commit** (`feat: the walker filters by selection (cut 16 task 2)`).

---

### Task 3: `CopyOptions.selection` (mechanical), then the copy path

This task is two commits. Step group A is mechanical and may go to a Haiku-class implementer; B is Sonnet.

**Files (A):**
- Modify: `crates/flux-fs/src/options.rs` (`CopyOptions` at line 119: add `pub selection: crate::Selection,`)
- Modify, one line each (`selection: flux_fs::Selection::default(),` or `Default::default()` inside the literal): `benches/copy.rs:33`, `tests/integration/support/mod.rs:18`, `crates/flux-core/src/tree.rs:4521`, `crates/flux-core/src/run/tests.rs:55`, `crates/flux-core/src/state.rs` (1897 inside `options_default`, 2207), `crates/flux-core/src/cleanup/delete.rs:985`, `crates/flux-core/src/run/resume.rs:163`, `crates/flux-core/src/walk.rs:485`, `crates/flux-core/src/run/multi.rs:1005`, `crates/flux-core/src/copy.rs:1432`, `crates/flux-core/tests/{safety_std_fs,dry_run_memory,hardlinks_std_fs,dry_run_std_fs,verify_std_fs,tree_std_fs,replace_std_fs,dry_run_prior_std_fs}.rs`, `crates/flux-cli/src/main.rs:269`. The compiler is the oracle: `cargo check --workspace --all-targets` names any literal this list missed; add the line there too. Struct-update literals (`..opts()`, `..cx.opts.clone()`, `..verifying(..)`) need NOTHING and must NOT get the line (a `selection:` placed before `..cx.opts.clone()` would overwrite the cloned selection): this covers `tree.rs:1696/2026/2173/5250`, `run/tests.rs:10630` and `run/multi.rs:1402`. `Selection` needs a HAND-WRITTEN `Default` (a derive would give `recursive == false`).

- [ ] **A1: Add the field and fix every literal.** Run: `cargo check --workspace --all-targets` => exit 0. Run `just test` => unchanged pass count (2114 passed, 6 skipped before this cut, plus Tasks 1-2's new tests). Commit (`refactor: CopyOptions carries a selection (cut 16 task 3a)`). No behaviour changes.

**Files (B):**
- Modify: `crates/flux-core/src/tree.rs` (`TreeOutcome` ~line 40, add `pub excluded: u64` after `mounts_skipped`, doc "Cut 16: entries the `--exclude` patterns removed; `--no-recursive` removals are not counted"; `prepare_source` call at `tree.rs:630`; `copy_tree_frames` at `tree.rs:950`; `walk_into` at `tree.rs:1043`)
- Modify: `crates/flux-core/src/run/mod.rs:320` and `crates/flux-core/src/run/multi.rs:476` (after `prepare_source`)
- Modify: `crates/flux-cli/src/report.rs` (`Report` ~line 53, `Report::zero` line 104, `Report::tree` line 135, `summary_line` line 478, `dry_run_summary_line` line 499)
- Test: `crates/flux-core/src/run/tests.rs`, `crates/flux-cli/src/report.rs` `mod tests`

**Interfaces:**
- Consumes: `Walk::set_selection`, `Walk::excluded` (Task 2); `CopyOptions.selection` (A1).
- Produces: `TreeOutcome.excluded: u64`; `Report.excluded: u64` (`#[serde(skip_serializing_if = "is_zero")]`, declared immediately after `mounts_skipped`); the text suffix `; N excluded`.

Steps B (decisions): at each of the three production `prepare_source` sites make the returned `Source` mutable (`Ok(mut source)` in the `multi.rs` match arm) and call `source.events.set_selection(&opts.selection)` (`opts` is the local clone; it outlives the `Source`). `walk_into`'s `events` parameter becomes `&mut Walk<'_, F>`; `copy_tree_frames` takes `mut events`, passes `&mut events`, and writes `out.excluded += events.excluded();` immediately after `let walked = walk_into(..)` at `tree.rs:971` and before `drain_mode`, so every exit path, including an abort, carries the count (callers move the same `out` into `TreeAbort`). `Report::tree` sets `r.excluded = out.excluded`. `summary_line`: append `(r.excluded, "excluded")` as the LAST entry of the existing suffix array; `dry_run_summary_line`: append `; N excluded` at the end only when non-zero. No per-root count in `RootJson`/`root_lines`; resume lines unchanged; `files_skipped` never includes it.

- [ ] **B1: Write the failing tests.**
  - `crates/flux-core/src/run/tests.rs` (fixtures as the cut 12 hardlink tests near line 9307 and the `opts()` helper at line 55 do):
    - `fn excluded_entries_are_absent_from_the_destination_and_counted()`: tree `a, b.tmp, sub/c.tmp, sub/d`; `--exclude *.tmp`; destination has `a`, `sub/d`; `excluded == 2`; `files_total == 2`.
    - `fn a_pruned_directory_counts_one_whatever_its_subtree()`.
    - `fn exclude_double_star_counts_the_top_level_entries_and_copies_nothing()`: `excluded == top-level count` including a directory; destination exists and is empty; `files_total == 0`; exit-0 path (`Ok(outcome)`).
    - `fn private_star_star_creates_the_empty_directory()`: `directories_created` counts `private`; no child exists.
    - `fn a_hardlink_group_with_its_first_member_excluded_copies_the_other_normally()`: names `a1` (excluded, walked first) and `a2` share an inode, `--hardlinks=preserve`: `a2` copied, `files_hardlinked == 0`, `hardlinks_degraded == 0`, no failure; and the mirror case excluding the last member; and two selected members still link.
    - `fn the_count_survives_an_abort_mid_walk()`: an excluded entry sorts before a file whose `copy` is made to fail with an abort (use the fault the neighbouring abort tests use); the `TreeAbort.outcome.excluded` is 1.
    - `fn a_root_that_fails_before_its_walk_counts_zero_and_roots_sum()` (several sources: first root excludes 2, second 3 => 5; first fails before walking => later roots 0; first excludes and the second does not, and both do).
    - `fn dry_run_live_and_resume_report_the_same_nonzero_count()`; the resume case has already-completed files that pass the filter and must not add to `excluded`.
    - `fn a_file_operand_is_copied_whatever_the_patterns_say()` (R7) and adds 0.
    - `fn no_recursive_creates_no_subdirectory_and_counts_zero()`; `fn no_recursive_with_a_pattern_counts_only_the_pattern_removals()`; `fn a_directory_removed_by_both_counts_once()`.
  - `crates/flux-cli/src/report.rs` `mod tests` (style of the `mounts_skipped` test at line 1029): `fn excluded_is_absent_from_json_and_text_at_zero()` (the pinned `KEYS` length 18 holds), `fn excluded_follows_mounts_skipped_and_precedes_hardlinks_degraded_in_json()` (pin next to line 2380: `at("mounts_skipped") < at("excluded") < at("hardlinks_degraded")`), `fn the_summary_and_dry_run_lines_end_with_an_excluded_suffix_only_when_non_zero()`, `fn excluded_never_enters_files_skipped()`; update the `Report::zero()` literal.
- [ ] **B2: Run to verify failure.** Run: `cargo nextest run -p flux-core run::tests::excluded -p flux-cli report::` => FAIL.
- [ ] **B3: Implement** per the decisions above.
- [ ] **B4: Run to verify pass.** Run: `cargo nextest run -p flux-core -p flux-cli` => all pass.
- [ ] **B5: Mutants.** Skip the `out.excluded +=` on abort (add it only when `walked.is_ok()`); assign `=` instead of `+=` in the multi-root sum; add `files_resumed` into `excluded`; leave `excluded` in `files_skipped`; drop `set_selection` at the `multi.rs` site. Each must turn a named test red.
- [ ] **B6: Commit** (`feat: copy selects through the walker and counts what it excluded (cut 16 task 3b)`).

---

### Task 4: Fingerprint, manifest and resume refusal

**Files:**
- Modify: `crates/flux-core/src/state.rs` (`Options` line 96, `Options::of` line 128, `canonical_bytes` line 165, `decode` validation ~line 622; tests near 2253-2290)
- Modify: `crates/flux-core/src/run/resume.rs` (compare array at lines 104-120; tests near 370-399)
- Modify, literal sites (these are `state::Options { .. }` literals; `state.rs:1896` is a `CopyOptions` literal handled in Task 3A):  `crates/flux-cli/tests/cleanup.rs:210`, `crates/flux-cli/tests/dry_run.rs` (158, 282, 505): add `exclude: Vec::new(), recursive: true,`.

**Interfaces:**
- Consumes: `flux_fs::Selection` (`patterns()`, `recursive()`, `compile`).
- Produces: `Options.exclude: Vec<String>` and `Options.recursive: bool`, both `#[serde(default ...)]`: `exclude` with `default` and `skip_serializing_if = "Vec::is_empty"`; `recursive` with `default = "recursive_true"` and `skip_serializing_if = "is_true"`.

Decisions: `Options::of` copies `opts.selection.patterns().to_vec()` and `opts.selection.recursive()`. `canonical_bytes` appends, after the existing optional lines and in this order, one `exclude=<pattern>\n` per pattern, sorted and deduplicated by `canonical_bytes` itself (so a struct literal and `Options::of` agree), then `recursive=false\n` only when false. `decode` (in the validation that already runs `one_of` checks) additionally rejects (`Unusable::Corrupt` with the message) an `exclude` that is not strictly sorted, or that `Selection::compile(&o.exclude, o.recursive)` refuses. `validate` in `resume.rs` extends the compare array with `("exclude", &then_ex, &now_ex)` and `("recursive", &then_rec, &now_rec)` AFTER `cross_filesystems`, `then_ex = then.exclude.join(" ")`, `now_ex` likewise, booleans via `to_string()`; the existing refusal text and `INCOMPATIBLE_STATE` / exit 3 are reused unchanged. Fix the stale comments at `state.rs:164` ("the four covered options" => name all) and `run/resume.rs:35` ("the four exact-match options" => eight).

- [ ] **Step 1: Write the failing tests.**
  - `state.rs`: `fn the_default_selection_adds_no_line_and_keeps_the_pre_cut_fingerprint()` (pin the exact historical hash of a fixed `Options`, copied from the existing pin near `state.rs:2260-2270`, and the `canonical_bytes` bytes); `fn exclude_lines_follow_the_existing_ones_sorted()` (`exclude=a\nexclude=b\n`, then `recursive=false\n`); `fn the_fingerprint_is_independent_of_flag_order_and_duplicates()` (built via `Options::of` AND via a struct literal); `fn a_pre_cut_manifest_decodes_and_resumes()` (JSON without the two keys decodes to empty/true); `fn a_default_selection_serialises_without_the_new_keys()`; `fn an_unsorted_or_uncompilable_stored_list_reads_as_corrupt()` (`["b","a"]`, `["a\\b"]`, 65 entries, a list over 16 KiB); `fn a_manifest_with_the_largest_accepted_selection_is_under_state_limit()` (64 patterns x 1024 bytes is refused at 16 KiB, but 16 x 1024 bytes encodes to < `STATE_LIMIT`; assert `encode(..).len() < STATE_LIMIT`).
  - `resume.rs` (style of the `crossing` tests at lines 370-399): `fn a_different_exclude_set_is_refused_as_incompatible_state()` and `fn a_different_recursion_mode_is_refused()` (each varies only that option; message names `exclude` / `recursive` with stored and requested values), `fn a_run_differing_in_both_reports_exclude_first()`, `fn the_same_selection_resumes()`.
- [ ] **Step 2: Run to verify failure.** Run: `cargo nextest run -p flux-core state:: run::resume` => FAIL.
- [ ] **Step 3: Implement** per the decisions.
- [ ] **Step 4: Run to verify pass.** Run: `cargo nextest run -p flux-core -p flux-cli` => all pass.
- [ ] **Step 5: Mutants.** Remove the sort/dedup inside `canonical_bytes` (the struct-literal case must go red); omit `#[serde(default)]` on either field (the pre-cut decode test must go red); drop the `recursive` compare; skip the sorted check in `decode`.
- [ ] **Step 6: Commit** (`feat: the selection is part of the resume fingerprint (cut 16 task 4)`).

---

### Task 5: `flux verify` selects both sides and counts

**Files:**
- Modify: `crates/flux-core/src/verify.rs` (`Summary` line 88; `VerifyOptions` line 108; `Side` line 153, `Side::new` line 167, `Side::empty` line 187; `walk_checked` line 585; the several-source fold lines 540-548; tests near 928, 2372)
- Modify: `crates/flux-core/src/verify_proptest.rs` (literals 904, 970, 1309; the two reproducer strings 660 and 938; the model's own running sum near 329)
- Modify: `crates/flux-cli/src/verify_report.rs` (`SummaryBody` ~line 138, `summary_json`, `summary_line` ~line 128, the literal in a test at ~339, the `want` key lists ~350-395)
- Test: `crates/flux-core/src/verify.rs` `mod tests`; `crates/flux-cli/tests/verify.rs`

**Interfaces:**
- Consumes: `Walk::set_selection/excluded`; `flux_fs::Selection`.
- Produces: `Summary.excluded: u64` (after `not_entered`, before `bytes_hashed`); `VerifyOptions.selection: flux_fs::Selection` (type becomes `Debug, Clone`, not `Copy`); `Side::new(.., selection: &'a Selection)`; `Side::excluded(&self) -> u64` (0 when `walk` is `None`).

Decisions: `Side::new` calls `walk.set_selection(selection)` on its walk for BOTH sides (`opts.selection`, `verify.rs:607` and `:617`); `walk_checked` adds `out.summary.excluded += a.excluded()` (the SOURCE side only) once the merge loop ends; the several-source fold adds `total.excluded += root.excluded` next to `not_entered` (`verify.rs:540-548`, NOT compiler-visible), as do the test folds near `verify.rs:2372` and `verify.rs:2508` and the proptest model sum near `verify_proptest.rs:329`. `SummaryBody.excluded` is `#[serde(skip_serializing_if = "is_zero")]` (define a local `fn is_zero`), declared after `not_entered`; the text clause is `; N excluded` after the `mount points not entered` clause, only when non-zero. `verify_file` is untouched.

- [ ] **Step 1: Write the failing tests.**
  - `verify.rs` (fixtures and `opts()` at line 928): `fn an_excluded_name_at_the_destination_is_not_extra()`; `fn an_excluded_name_at_the_source_is_not_missing()`; `fn no_recursive_hides_destination_subdirectories_and_exits_clean()` (matching top level => `!failed()`); `fn a_top_level_mismatch_still_fails_under_no_recursive()`; `fn a_source_file_against_a_destination_directory_under_a_dir_only_pattern_is_missing()` and the mirror `extra` case; `fn the_count_is_source_side_only_with_asymmetric_sides()` (source 1, destination 2 => 1); `fn two_roots_sum_their_counts()`; `fn an_absent_destination_still_reports_the_source_count()` (`Side::empty`); `fn exclusions_under_a_pruned_mount_subtree_still_count()`.
  - `verify_report.rs`: `fn the_summary_json_has_excluded_only_when_non_zero_between_not_entered_and_bytes_hashed()`; `fn the_summary_line_has_the_excluded_clause_after_the_mount_clause()`; update the explicit `Summary { .. }` literal at ~339 (add `excluded: 0`) and keep `a_summary_json_has_exactly_the_eleven_fields` green at zero.
  - `crates/flux-cli/tests/verify.rs`: after Task 6 lands the flags, `SUMMARY_KEYS` / `SEVERAL_SUMMARY_KEYS` (lines 55, 562) stay valid at zero.
- [ ] **Step 2: Run to verify failure.** Run: `cargo nextest run -p flux-core verify:: -p flux-cli verify_report` => FAIL to compile (field absent).
- [ ] **Step 3: Implement.** The compiler names every `VerifyOptions` copy site and literal (`main.rs:739`, `verify.rs:929`, `verify_proptest.rs:904/970/1309`); give each `selection: Default::default()`, including `main.rs:739` so the workspace still builds (Task 6 replaces that one). Update the two reproducer strings so a printed reproducer still compiles.
- [ ] **Step 4: Run to verify pass.** Run: `cargo nextest run -p flux-core -p flux-cli` => all pass, including the verify property tests.
- [ ] **Step 5: Mutants.** Drop the destination-side `set_selection`; drop the source-side one; count both sides; forget the several-source fold; always serialise `excluded`. Each must turn a named test red.
- [ ] **Step 6: Commit** (`feat: verify selects both sides and counts what it excluded (cut 16 task 5)`).

---

### Task 6: CLI flags, validation and end-to-end tests

**Files:**
- Modify: `crates/flux-cli/src/main.rs` (`VerifyArgs` line 63, `CopyArgs` line 99, `options` line 265, `copy` line 396, `copy_several` line 617, `verify_options` line 737, `verify` line 752; tests at 1030-1036)
- Modify: `crates/flux-cli/tests/verify.rs:168-175` (the loop that pins `--exclude` as an error: keep `--resume` only)
- Test: `crates/flux-cli/tests/copy.rs`, `crates/flux-cli/tests/verify.rs`, `crates/flux-cli/tests/several_sources.rs`

**Interfaces:**
- Consumes: `flux_fs::{Pattern, Selection}` (Task 1), `CopyOptions.selection`, `VerifyOptions.selection`.
- Produces on both `CopyArgs` and `VerifyArgs`: `exclude: Vec<String>` (`#[arg(long, value_name = "PATTERN", action = ArgAction::Append, value_parser = parse_exclude)]`), `recursive: bool` and `no_recursive: bool` (`recursive` has `conflicts_with = "no_recursive"`); `fn parse_exclude(s: &str) -> Result<String, String>` (runs `Pattern::parse`, returns the error text); `fn selection_of(exclude: &[String], no_recursive: bool) -> Result<Selection, String>`.

Decisions: `copy()`, `copy_several()` and `verify()` begin with `selection_of(..)`; on `Err` print `error: <message>` to stderr and return `exit_code::USAGE` (2) before any state or destination is touched (this is where the aggregate caps surface). `options()` and `verify_options()` set `selection: selection_of(..).expect("validated at the top of copy/verify")`. `--help` text (doc comments): the grammar in a few lines, `=-foo` for a leading dash, reserved characters reached with `?`/`*`, that a file operand is not filtered, and that `--no-recursive` copies only the top level. `--recursive` alone is accepted.

- [ ] **Step 1: Write the failing tests.**
  - `main.rs` unit tests: `fn exclude_is_repeatable_and_parses()`; `fn recursive_and_no_recursive_conflict()`; `fn a_bad_pattern_is_a_parse_error()`; replace the `--exclude` assertion at `main.rs:1033` with an `--exclude x` success and keep the `--resume` rejection.
  - `tests/copy.rs` (helpers `flux()`, `tree_in`, `json`, `stderr`; KEYS pin at line 288/315 stays valid): `fn exclude_skips_matching_files_and_reports_the_count()` (JSON `excluded` present and equal to N; stderr summary has `; N excluded`); `fn without_a_match_there_is_no_excluded_key_or_text()`; `fn a_bad_pattern_exits_2_names_the_reason_and_creates_nothing()` (destination and `.flux` state absent); `fn too_many_patterns_exit_2()` (65, and 17 x 1024 bytes); `fn no_recursive_copies_only_the_top_level()`; `fn recursive_and_no_recursive_together_exit_2()`; `fn recursive_alone_is_accepted()`; `fn exclude_equals_dash_foo_is_accepted_and_dash_foo_alone_is_refused()`; `fn a_file_operand_is_copied_despite_a_matching_pattern()`; `fn exclude_star_star_copies_nothing_and_exits_0()`; `fn dry_run_with_exclude_reports_the_same_count()`.
  - `tests/verify.rs`: `fn verify_accepts_exclude_and_ignores_excluded_names_on_both_sides()`; `fn verify_no_recursive_checks_only_the_top_level()`; `fn the_summary_json_has_excluded_only_when_non_zero()`; `fn verify_with_a_bad_pattern_exits_2_with_empty_stdout()`; adjust the loop at line 168 to `--resume` only.
  - `tests/several_sources.rs`: `fn exclude_applies_to_each_source_root()` (pattern `*.tmp`, two roots; counts sum; a file root among them is copied).
- [ ] **Step 2: Run to verify failure.** Run: `cargo nextest run -p flux-cli` => the new tests fail.
- [ ] **Step 3: Implement** per the decisions.
- [ ] **Step 4: Run to verify pass.** Run: `cargo nextest run -p flux-cli` => all pass; `cargo run -q -p flux -- copy --help` shows `--exclude <PATTERN>`.
- [ ] **Step 5: Mutants.** Move the `selection_of` check below the first state/destination write in `copy()` (the "creates nothing" test must go red); let `--no-recursive` win silently; drop the aggregate-cap check.
- [ ] **Step 6: Commit** (`feat: --exclude and --recursive/--no-recursive on copy and verify (cut 16 task 6)`).

---

### Task 7: Docs, stand-downs and the gate

**Files:**
- Modify: `README.md` (verify synopsis line 90; option prose lines 105-106; add `--exclude` / `--recursive` / `--no-recursive` to the copy option list), `ROADMAP.md` (rows at lines 42, 43, 45), `TODO.md` (line 1214 closed; new items below), `crates/flux-cli/src/verify_report.rs` (GUARANTEES text, lines 26-30: add that selected-out entries, including subdirectories under `--no-recursive`, are not compared and that a directory not entered is not only a mount), `docs/superpowers/specs/2026-10-27-selection-design.md` (amend 5.3 with plan ruling P1, R10's location, and the section 6 engine-validation mutant, per P4).

New TODO items (one line each, per the design's Stand-downs): a per-entry `EXCLUDE` record or listing; `--exclude-from FILE`; negation and character classes (reserved today); `--exclude-ignore-case`; `--max-depth`; a per-pattern or per-entry selection summary in the `--json` report; `--exclude` matching of a file operand (R7).

- [ ] **Step 1:** Make the edits. **Step 2:** Run `just check` => exit 0 (all gates), `just test` => pass line `... passed, 6 skipped`, 0 failed. **Step 3:** Run the whole-feature smoke: build a tree with `a`, `b.tmp`, `sub/c.tmp`, `sub/d`; `flux copy SRC DEST --exclude '*.tmp' --json` shows `"excluded":2`; `flux verify SRC DEST --exclude '*.tmp'` exits 0. **Step 4: Commit** (`docs: selection options, stand-downs (cut 16 task 7)`).

---

## Self-Review

- **Spec coverage:** grammar and limits (T1, T6), `--no-recursive` and Verdict (T1, T2), walker prune and zero I/O (T2), copy/dry/resume/several (T3), count semantics and reporting (T3, T5), fingerprint/manifest/refusal/serde defaults/caps/decode (T4), verify both sides, kind-dependent cases, source-only count, `Side::empty`, fold (T5), CLI parse/conflict/exit 2/help/R7 (T6), stale docs, stand-downs, panel-record amendment (T7). R1-R11: R1/R2/R3 T1, R4 T3/T5, R5 T2/T3, R6 T5, R7 T3/T6, R8 T3, R9 T3, R10 T1 (no crate), R11 T1/T2.
- **Types consistent:** `Selection::{compile, patterns, recursive, is_default, select}`, `Verdict::{Keep, Excluded, Skipped}`, `Walk::{set_selection, excluded}`, `TreeOutcome.excluded`, `Report.excluded`, `Summary.excluded`, `Side::excluded`, `Options.{exclude, recursive}` are named identically in every task.
- **Proportion:** code appears only as signatures and test assertions; matcher internals and plumbing bodies are the builder's.
- **Open to the plan panel:** P1-P3; whether Task 3A may run on Haiku; the exact call-log accessor name for the "no `read_dir`" assertion (the builder reads the neighbouring mount tests at `walk.rs:560-600`).
