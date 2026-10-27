# Several sources for `flux verify` (cut 14b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux verify S1 S2 ... DEST` compares each source with `DEST/<name>` as copy places it, in prefix order, with one summary and one exit status, leaving the one-source command unchanged.

**Architecture:** The merge engine in `crates/flux-core/src/verify.rs` gains a three-way destination filter (`ControlFilter`), a split of the one-source entry point into a check phase and a walk phase, and a new `verify_several` that plans the roots with the copy's `plan_roots`, probes and checks every root before any listing, refuses aliases and nested roots through structured `VerifyError` variants, then walks the roots one after another re-rooting every record as `<name>/<path>`. The CLI reuses `resolve::several`, prints per-root stderr lines and the refusal lines through new `verify_report` helpers, and the JSON summary carries `sources` instead of `source` for the several case.

**Tech Stack:** Rust (edition and toolchain as the workspace), clap (derive), serde_json, the `FaultFs` fake for engine tests, the `flux` binary and `tempfile` for CLI tests.

**Spec:** `docs/superpowers/specs/2026-10-27-verify-several-sources-design.md` (the design; section numbers below refer to it). Every `file:line` here was read at `9e703cfb17dd171334f689a6bcb267a4ffa750da` (main `613da2ca05caf9d04415e832bdd18419d112bb01` plus the design commit); line numbers drift as tasks land, so an implementer locates by the quoted text when a number is off.

## Global Constraints

- The gate is `just check` (fmt, clippy with `-D warnings`, typos, the workspace nextest run, doc tests); run it before every commit that touches code. Expected tail: `Summary [ ... ] N tests run: N passed, 6 skipped` with no failure and exit 0.
- Never write a short git sha in prose, a comment or a commit body (the typos gate reads a seven-character sha that happens to end in the letters e-d-e as a misspelling of "edge"); full shas only, or none.
- Windows CI: a path printed or compared in a test goes through `display_path` or a `/`-normalising helper, never a raw `Path::join` string; the fake lists only what `create_dir` made.
- Commit per task, each message ending with the attribution trailer the executing session's system reminder prescribes; stage the task's files EXPLICITLY (`git add <the files the task names>`, never `-a`, so an unrelated modification never rides along); never push, never `just pr`. The design amendment and this plan are committed together BEFORE Task 1 (the preamble step below).
- The one-source command's records, summary (human and JSON, keys and values), stderr lines and exit codes are byte-for-byte unchanged (design section 2); every existing engine fixture in `verify.rs`'s `mod tests` and every existing CLI test in `tests/verify.rs` stays green WITHOUT edits (the compiler and nextest are the oracle for their number; no count is pinned here); the only existing tests any task edits are the `summary_json` call of `a_summary_json_has_exactly_the_eleven_fields` (`verify_report.rs:283`, Task 5, new signature, same expected text) and the eight `several(` call sites in `resolve.rs`'s unit tests (`:314, 340, 352, 367, 380, 381, 391, 400`, Task 5b, a new argument, same expectations).
- Message texts are the design's, verbatim (6.2, 6.5, 6.7); the oracle for every test is the design or the cut 14 design; no test is edited to match the code.
- Exit codes: 0, 1, 2 only (design 6.6).

## Review Focus

1. A source whose name differs only by case from another on a case-sensitive host (Linux CI): both roots run and both are reported; nothing is refused. Pinned in Task 4b's alias test, arm 2 (two directories with their own identities, no refusal).
2. A root whose source is unreadable AND whose `DEST/<name>` aliases another root: no identity, so no refusal, and the root is `unreadable`. Pinned in Task 4b (test 22, arm 4).
3. `flux verify a b DEST` where `DEST/a` is a symlink to `DEST/b`: the probe does not follow, so `a` is `mismatched` (`destination symlink`) and `b` runs normally. Pinned in Task 4a (test 19, the symlink arm).
4. A several-source run under `--json` whose second source is missing: the stopped summary carries `sources` as typed and `"exit": 1`, nothing else on stdout. Pinned in Task 6 (test 13).
5. One positional (`flux verify a`): exit 2, nothing on stdout, `error` on stderr, from clap. Pinned in Task 6 (test 15).

---

### Task 0: commit the reviewed documents

- [ ] `git add docs/superpowers/specs/2026-10-27-verify-several-sources-design.md docs/superpowers/plans/2026-10-27-verify-several-sources.md` and commit as `docs: implementation plan for several sources in flux verify (cut 14b)`, so the working tree is clean before any code task.

### Task 1: `ControlFilter` replaces the destination boolean

**Files:**
- Modify: `crates/flux-core/src/verify.rs` (the `Side` struct near line 134 with its `control_state: bool`, `Side::new` near 146-160, `Side::empty` near 163, the `advance` filter near 262-266, the `is_control_state` doc comment near 198-200, `verify_tree_notifying` near 385-396)
- Test: `crates/flux-core/src/verify.rs` `mod tests` (near line 692; helpers `opts`, `tree`, `run`, `rec`, `mm`, constants `ZEROS` near 912)

**Interfaces:**
- Produces: `pub enum ControlFilter { None, Root, NestedRoot }` (`Debug, Clone, Copy, PartialEq, Eq`) in `verify.rs`; `Side::new(fs, root, mounts, skip_links: bool, filter: ControlFilter)`; `Side::is_control_state(item, filter) -> bool` where `None` returns false, `Root` is today's function, `NestedRoot` is only the partial-temporary rule (the last `matches!` of the function); `pub(crate) fn verify_tree_filtered(fs, src, dst, opts, filter: ControlFilter, on_absent: &mut dyn FnMut(), sink) -> Result<Summary, VerifyError>` with `verify_tree_notifying` delegating to it with `ControlFilter::Root`.

- [ ] **Step 1: Write the failing test** (design test 2), in `mod tests`:

```rust
#[test]
fn a_users_dot_flux_inside_a_nested_root_is_compared_and_a_partial_temporary_is_hidden() {
    let fs = FaultFs::new();
    fs.create_dir(Path::new("/s")).unwrap();
    fs.create_dir(Path::new("/d")).unwrap();
    tree(&fs, "/s/a", &[".flux"], &[(".flux/x", b"x"), (".flux-root.lock", b"l"), ("f", b"f")]);
    let partial = format!("f.flux-partial.{ZEROS}");
    tree(&fs, "/d/a", &[".flux"], &[(".flux/x", b"x"), (".flux-root.lock", b"l"), ("f", b"f"), (partial.as_str(), b"p")]);
    let mut rows = Vec::new();
    let s = verify_tree_filtered(&fs, Path::new("/s/a"), Path::new("/d/a"), &opts(), ControlFilter::NestedRoot, &mut || {}, &mut |r| rows.push(row(&r))).unwrap();
    assert_eq!(rows, vec![
        rec(".flux", Outcome::Match, Kind::Dir),
        rec(".flux/x", Outcome::Match, Kind::File),
        rec(".flux-root.lock", Outcome::Match, Kind::File),
        rec("f", Outcome::Match, Kind::File),
    ]);
    assert_eq!((s.matched, s.extra, s.missing), (4, 0, 0));
}
```

(The order is the oracle: `FluxPathKey` joins components with a 0x00 separator (`flux-fs/src/claims.rs:15-45`), and 0x00 sorts below `-` (0x2d), so `.flux/x` precedes `.flux-root.lock`.)

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p flux-core -E 'test(a_users_dot_flux_inside_a_nested_root)'`
Expected: a compile error naming `ControlFilter` / `verify_tree_filtered`.

- [ ] **Step 3: Implement the enum, thread it through `Side`, add `verify_tree_filtered`**

`Side.control_state: bool` becomes `filter: ControlFilter`; `Side::new`'s last parameter likewise; `Side::empty` uses `ControlFilter::None`; the `advance` condition becomes `Self::is_control_state(&item, self.filter)`; `verify_tree_notifying`'s body moves into `verify_tree_filtered` with `filter` passed to the destination `Side::new` (the source side stays `ControlFilter::None`), and `verify_tree_notifying` calls it with `ControlFilter::Root`. The doc comment of `is_control_state` (near 198-200) names the three modes.

- [ ] **Step 4: Run the new test and the whole crate**

Run: `cargo nextest run -p flux-core --no-fail-fast`
Expected: all green, the new test included; the control-state fixtures (`control_state_at_the_destination_root_is_never_reported`, `a_top_level_dot_flux_file_at_the_destination_is_user_data`, `a_source_dot_flux_directory_is_missing_when_dest_has_only_control_state`) unchanged and green (design test 10).

- [ ] **Step 5: Mutant proof, then restore**: change the `NestedRoot` arm to behave like `Root`; the new test must go red and nothing else; restore.

- [ ] **Step 6: `just check`, then commit**

```bash
git add crates/flux-core/src/verify.rs
git commit -m "feat(core): a three-way control-state filter for the verify destination side"
```

### Task 2: the one-source entry point splits into a check phase and a walk phase

**Files:**
- Modify: `crates/flux-core/src/verify.rs` (`verify_tree_filtered` from Task 1; `check_roots` near 470-500 and `RootsCheck` near 428-436)
- Test: the existing fixtures (`a_read_dir_order` style pin: the fixture asserting the source walk is built before the destination's, search `read_dir` order in `mod tests`), plus one new unit test below.

**Interfaces:**
- Produces: `enum RootVerdict { Absent, Unreadable { side: &'static str, error: FsError }, Checked }` (crate-private; Task 4 extends it with `Checked { dest_identity: Option<ObjectId> }` and `WrongType(Kind)`, so no field is introduced here that nothing reads, which `-D warnings` would reject as dead code); `check_roots` (`verify.rs:470-500`) changes its success value from `bool` to `enum Presence { Absent, Present(FileIdentity) }`, returning the `di` it already reads at `:486` (no second `identity()` call anywhere, so the fake's call log and the ordinals Task 4a counts stay stable); `fn check_pair<F: DestinationRoot>(fs, src, dst) -> Result<RootVerdict, String>` maps `Absent` to `RootVerdict::Absent` and `Present(_)` to `Checked` (Task 4b keeps the identity), and `Err(reason)` is a refusal (the existing clause strings, `REFUSED_DST_IN_SRC` etc., UNCHANGED, tail included); `fn walk_checked<F: DestinationRoot>(fs, src, dst, opts, filter, verdict: RootVerdict, out: &mut Emitter<'_>)` which, for `Absent`, drains the source against `Side::empty()`; for `Unreadable`, emits the `.` record through `root_unreadable`'s logic; for `Checked`, builds both sides and runs the merge loop. `verify_tree_filtered` = `check_pair` then `on_absent` when absent then `walk_checked`. The check phase never lists a directory (`check_roots` and `containment_sided` call `destination_root`, `canonical_path`, `metadata` and `identity` only), which Task 4's tests rely on.

- [ ] **Step 1: Write the failing unit test**: `check_pair_classifies_a_checked_an_absent_and_a_refused_pair` on `FaultFs` with two directories `/s` and `/d`: `check_pair` returns `Checked`; with `/d` absent, `Absent`; with `/d` inside `/s` (`/s/d`), `Err(reason)` where `reason` starts with `REFUSED_DST_IN_SRC`; with `/s` absent, `Unreadable { side: SOURCE, .. }`.

- [ ] **Step 2: Run it to verify it fails** (compile error on `check_pair`).

- [ ] **Step 3: Implement the split** without changing any emitted record, count or call order (the fixture that pins the `read_dir` order: the source walk is still built before the destination's inside `walk_checked`).

- [ ] **Step 4: Run the crate**: `cargo nextest run -p flux-core --no-fail-fast`, all green, no fixture edited.

- [ ] **Step 5: `just check`, then commit**

```bash
git add crates/flux-core/src/verify.rs
git commit -m "refactor(core): split the verify root check from the walk"
```

### Task 3: `verify_several`: roots, order, re-rooting, callbacks

**Files:**
- Modify: `crates/flux-core/src/verify.rs` (new `pub fn verify_several`, new `VerifyError` variants); `crates/flux-core/src/run/mod.rs:57` (`roots` is a PRIVATE module, `mod roots;` at `:24`, so `verify.rs` cannot name `crate::run::roots::plan_roots`: add `pub(crate) use roots::plan_roots;` beside the existing `pub use roots::{SourceKind, SourceSpec, source_cap};`); `crates/flux-cli/src/main.rs:820-823` (the `match result` is exhaustive over `VerifyError` with one arm, so the crate stops compiling once variants are added: replace the arm with one shared function `fn refusal_line(e: &VerifyError, dst: &Path) -> String` in `main.rs`, exhaustive over the enum, with the FINAL renderings written inline in this task: `RefusedRoots { reason }` and `RefusedNames { reason }` give `error: {reason}` (`dst` unused); `RefusedRoot { name, reason }` gives `error: root {n}: {d}/{n}: {reason}; nothing was verified` where `n` is `verify_report::display_path(Path::new(&name))` and `d` is `dst.display()` (design 6.2: the canonical DEST, the escaped name, the tail appended once); `RefusedAlias { a, b }` gives `error: roots {a} and {b} are one directory on this filesystem; nothing was verified` with both names through `display_path`; the one-source path calls `refusal_line(&e, &dst)` with the `dst` it already holds and returns `exit_code::USAGE`; Task 5 then replaces the two inline bodies with its `refused_root_line` and `refused_alias_line` helpers (a step of Task 5), and Task 6's several path calls the same function)
- Test: `crates/flux-core/src/verify.rs` `mod tests`

**Interfaces:**
- Consumes: `check_pair`, `walk_checked`, `ControlFilter::NestedRoot` (Tasks 1, 2); `crate::run::{SourceSpec, SourceKind}` (`run/mod.rs:57`) and `crate::run::plan_roots` (the re-export this task adds); `verify_file` (`:659`).
- Produces:

```rust
pub enum VerifyError {
    RefusedRoots { reason: String },                 // unchanged, one-source
    RefusedRoot { name: OsString, reason: String },  // several: a root check refused; reason WITHOUT the "; nothing was compared" tail, by-path-alone note kept
    RefusedNames { reason: String },                 // several: plan_roots' detail
    RefusedAlias { a: OsString, b: OsString },       // several: Task 4b
}
pub fn verify_several<F: DestinationRoot>(
    fs: &F, specs: Vec<SourceSpec>, dest: &Path, opts: &VerifyOptions,
    on_absent: &mut dyn FnMut(&OsStr), on_root_done: &mut dyn FnMut(&OsStr, &Summary),
    sink: &mut dyn FnMut(Record),
) -> Result<Summary, VerifyError>
```

Behaviour in this task (design 6.2 to 6.5, without the probe and alias rule, which Task 4 adds): `plan_roots(specs)` (a `RunError::Refused` becomes `RefusedNames { reason: refusal.detail }`); then for every FOLDER root in the returned order `check_pair(fs, &spec.src, &dest.join(&spec.name))`, an `Err(reason)` becoming `RefusedRoot { name, reason: strip the "; nothing was compared" suffix, keeping any " (decided by path alone: ...)" that follows it }` and ending the run before any walk; then per root in order: a folder root walks with `ControlFilter::NestedRoot` through `walk_checked` into an `Emitter` whose sink re-roots (`.` to `<name>`, else `name.join(path)`), firing `on_absent(name)` first when the verdict is `Absent`; a file root runs `verify_file(fs, &spec.src, &dest.join(&spec.name), opts, sink)` with no re-rooting; after each root `on_root_done(name, &root_summary)` and the counts are added into the total (every field, `bytes_hashed` and `not_entered` included). The strip of the tail: `reason.strip_suffix("; nothing was compared")` when the clause ends with it, else split at the first occurrence of `"; nothing was compared"` and rejoin the remainder (the by-path-alone form is `REFUSED_DST_IN_SRC` + `BY_PATH_ALONE`, so the tail is in the middle).

- [ ] **Step 1: Write the failing tests** (design tests 1, 3, 4, 8, 9, 21), each as its own `#[test]`, with a helper `fn spec(src: &str, name: &str, kind: SourceKind) -> SourceSpec` and `fn several(fs, specs, dest, o) -> (Vec<Row>, Vec<(String, Summary)>, Vec<String>, Summary)` (rows, root summaries by name, absent notices by name, total):
  - `several_roots_stream_in_prefix_order_with_re_rooted_paths`: specs named `b`, `a-b`, `a` given in that order, each a folder holding one file `x` matching: the rows are exactly `a/x`, `a-b/x`, `b/x`, all `match` of kind file, in that ORDER (a healthy root emits no record of its own; `.` appears only for an unreadable root); `on_root_done` names arrive as `a`, `a-b`, `b`.
  - `a_file_root_is_one_record_named_after_the_root`: a file spec `f` against `DEST/f` present: one row `f` match; absent: one row `f` missing (kind file).
  - `an_absent_dest_reports_every_root_missing_and_notifies_each_folder_root`: two folder roots and a file root, DEST absent: absent notices `[a, b]` (folder roots only, in order), every row `missing`, total `missing` equals the file count plus the file root.
  - `a_mount_notice_carries_the_re_rooted_path`: a mount root at `/s/b/m` and `/d/b/m`, `MountPolicy::Stay`: a row `b/m` match with detail `MOUNT_NOT_ENTERED`, total `not_entered` 1.
  - `per_root_summaries_sum_to_the_total`: roots whose summaries together put a non-zero value in EVERY one of the seven `Summary` fields (`verify.rs:84-94`): `a` holds one mismatch and one destination-only file (`mismatched`, `extra`), `b` holds one missing file and a mount under `Stay` (`missing`, `not_entered`, `matched` for the mount record), `c` holds two matching NON-EMPTY files (`matched`, `bytes_hashed`), and `d`'s source is absent (`unreadable`); the per-root summaries from `on_root_done`, in order, sum field-wise to the returned total, and each field of the total is non-zero.
  - `a_source_inside_another_source_is_its_own_root`: specs `a` (folder holding `f` and `g`) and `a/f` (file, name `f`); DEST holds `a/f`, `a/g`, `f`: rows `a/f`, `a/g`, `f`, all match.
  - `a_users_dot_flux_inside_a_root_is_compared_through_verify_several` (the call-site pin of design C4; Task 1 tests the filter only directly): one folder root `a` holding `.flux/x` and `.flux-root.lock` on both sides and a `f.flux-partial.<ZEROS>` on the destination only, run through `verify_several`: rows `a/.flux`, `a/.flux/x`, `a/.flux-root.lock`, all `match`, and no row for the partial.
  - `a_root_check_refusal_on_a_later_root_prints_nothing` (design test 6): roots `a` fine and `b` whose source IS `DEST` (spec src `/d`): `Err(RefusedRoot { name: "b", reason })` with `reason == "the destination is the source or lies inside it"` (no tail) and the sink saw nothing.
  - `two_sources_with_one_name_are_refused_before_any_read` (design test 5): two specs with name `x`: `Err(RefusedNames { .. })`, `fs.calls()` holds no `read_dir(` and no `open_read(`.

- [ ] **Step 2: Run them to verify they fail** (compile error on `verify_several`).

- [ ] **Step 3: Implement `verify_several` and the variants** as specified above. Re-rooting lives in a small closure wrapping `sink`; the per-root `Emitter` is fresh per root (its `summary` is the root summary).

- [ ] **Step 4: Run the crate**: `cargo nextest run -p flux-core --no-fail-fast`, all green.

- [ ] **Step 5: Mutant proofs, restore after each**: (a) iterate `specs` in command-line order instead of `plan_roots`' order: the order test goes red; (b) re-root file roots too (`f/f`): the file-root test goes red; (c) omit `bytes_hashed` from the total: the sum test goes red; (c') omit `extra` from the total: the sum test goes red; (c'') omit `unreadable` from the total: the sum test goes red; (d) build the destination side with `ControlFilter::Root` instead of `NestedRoot` in `verify_several`: the call-site pin goes red.

- [ ] **Step 6: `just check`, then commit**

```bash
git add crates/flux-core/src/verify.rs crates/flux-core/src/run/mod.rs crates/flux-cli/src/main.rs
git commit -m "feat(core): verify_several runs each root under DEST/<name> in prefix order; the CLI maps every VerifyError through one refusal_line"
```

### Task 4a: the wrong-type probe, probe errors, unreadable roots

**Files:**
- Modify: `crates/flux-core/src/verify.rs` (`verify_several` from Task 3)
- Test: `crates/flux-core/src/verify.rs` `mod tests`

**Interfaces:**
- Consumes: `RootVerdict` (Task 2), `FaultFs::fail_nth` (`fault_fs.rs:1118`), `FaultFs::fail_read_dir_of` (`:1319`), `FaultFs::add_symlink_to` (`:1384`).
- Produces: `RootVerdict` gains `WrongType(Kind)` (the identity field comes with Task 4b). Inside `verify_several`, before `check_pair` of a folder root: `fs.metadata(&dest.join(name))`: `Ok(m)` with `m.file_type != FileType::Dir` records a `WrongType(Kind)` verdict (`file_kind(m.file_type)`, the existing helper `verify_file` uses); `Err(NotFound)` proceeds to `check_pair` (which yields `Absent`); any other `Err(e)` records `Unreadable { side: DESTINATION, error: e }`. Walk phase: `WrongType(k)` emits exactly one record `Record { path: name, outcome: Mismatched, kind: Dir, code: Some(Code::VerifyMismatch), detail: format!("source dir, destination {}", k.as_str()) }` and `on_root_done` with `mismatched` 1; `Unreadable` emits one `unreadable` record at `<name>` with the side prefix (as `root_unreadable`) and `on_root_done` with `unreadable` 1; a walk-phase failure of `Side::new` is the same `unreadable` record.

- [ ] **Step 1: Write the failing tests** (design tests 7, 19):
  - `a_root_whose_destination_has_the_wrong_type_is_one_mismatched_record_and_the_run_goes_on`: folder root `a` with `/d/a` a FILE, folder root `c` with `/d/c` a symlink (`add_symlink_to("/d/c", "/d/b", LinkKind::Directory)`) and root `b` fine: rows `a` (`mm("a", Kind::Dir, "source dir, destination file")`), then `b/...` matches, then `mm("c", Kind::Dir, "source dir, destination symlink")`; `!fs.called("read_dir(/d/a)")` and `!fs.called("read_dir(/d/c)")`; `on_root_done` fired for `a`, `b`, `c` in that order with `a`'s and `c`'s summaries exactly `Summary { mismatched: 1, ..Default::default() }`, and the returned total is the field-wise sum of the three (design test 9); a file root `f` with `/d/f` a directory: `mm("f", Kind::File, "source file, destination dir")` and `on_root_done("f")` with `mismatched` 1.
  - `an_unreadable_root_is_one_record_at_its_turn_and_the_others_run`: three arms, each a fresh fixture with roots `a` and `b`: (i) CHECK-phase failure: the source `/s/a` does not exist in the fake (never created) while `/d/a` does: the check's `destination_root(src)` fails with NotFound (the fake consults only its directory set there, `fault_fs.rs:1890-1894`; `fail_read_dir_of` does NOT reach it, since `read_dir_faults` is read only by `read_dir`, `:1802`): record `a` `unreadable`, detail led by `source: `, then `b/...` complete; (ii) PROBE failure: `fail_nth("metadata", n, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied)` where `n` is the 1-based position of the FIRST `metadata(/d/a)` entry among the log entries whose text starts with `metadata(`, counted from the fake's creation and including the fixture's setup calls (the fake's per-key counter `call_counts["metadata"]` is compared with `nth` at each `metadata` call, `fault_fs.rs:1490-1508`, and `metadata(<path>)` is recorded under the key `metadata`, `:1623`; the first occurrence is the probe's, the second is `canonical_of`'s inside the check, `tree.rs:4333`); the test reads `fs.calls()` from an unfaulted run of the same fixture to compute `n` before arming the fault: record `a` `unreadable`, detail led by `destination: `; (iii) WALK-phase failure: `fail_read_dir_of("/s/a", Code::PermissionDenied, PermissionDenied)`: the check passes (`destination_root` ignores listing faults) and `Side::new` fails, because `walk_with` lists the root at construction (`walk.rs:215`): exactly one `unreadable` record `a`, then `b/...` complete, and `on_root_done("a")` reports `unreadable` 1. In every arm `on_root_done` fires for both roots.

- [ ] **Step 2: Run them to verify they fail.**

- [ ] **Step 3: Implement** the probe and the `WrongType` and unreadable verdicts as in Interfaces (the alias rule is Task 4b).

- [ ] **Step 4: Run the crate**, all green.

- [ ] **Step 5: Mutant proofs, restore after each**: (a) probe with a following stat (treat `Symlink` as `Dir`): the symlink arm of test 19 goes red; (c) drop the `NotFound` arm (treat it as unreadable): the absent-DEST test of Task 3 goes red; (d) skip `on_root_done` for a wrong-type root: test 19 goes red.

- [ ] **Step 6: `just check`, then commit**

```bash
git add crates/flux-core/src/verify.rs
git commit -m "feat(core): verify_several probes each root's type and reports an unreadable root at its turn"
```

### Task 4b: the alias rule and the precedence of refusals

**Files:**
- Modify: `crates/flux-core/src/verify.rs` (`RootVerdict::Checked` gains `dest_identity: Option<ObjectId>`, which `check_pair` fills from the `Presence::Present(identity)` Task 2 made `check_roots` return, as `Some(id)` only for `FileIdentity::Strong(id)`, no new `identity()` call; `verify_several` gains the alias map; Task 2's unit test is updated to match `Checked { .. }`)
- Test: `crates/flux-core/src/verify.rs` `mod tests`

**Interfaces:**
- Produces: after the check loop, a `HashMap<ObjectId, OsString>` (the first root's name per id) filled from `Checked { dest_identity: Some(id) }` in prefix order; a repeated id returns `Err(VerifyError::RefusedAlias { a: stored, b: current })`; the loop's own `RefusedRoot` is raised inside the loop, so it wins; any `Err` returns before the walk phase, so remembered verdicts are never emitted.

- [ ] **Step 1: Write the failing tests** (design tests 22, 23):
  - `two_root_names_that_alias_on_the_destination_are_refused_before_any_read`: the fake does NOT fold case in `destination_root` (`fault_fs.rs:1897` tests the raw path), so the alias is modelled with `set_identity`: two distinct directories `/d/Data` and `/d/data` (case-sensitive fake), both given `FileIdentity::Strong(ObjectId { volume: 1, index: 999 })` through `fs.set_identity` (a directory handle's `identity()` reads that map, `:1994-2009`); roots `Data` and `data`: `Err(RefusedAlias { a: "Data", b: "data" })` (prefix order: `Data` < `data` as bytes), no `read_dir(` of either source in the log, sink empty; arm 2 (Review Focus 1): the same two directories with their own minted identities: no refusal, both run; arm 3: both set to the same `FileIdentity::Weak(ObjectId { volume: 1, index: 999 })`: no refusal, both run (the `Strong`-only rule); arm 4: both `Strong` and equal, and root `Data`'s SOURCE absent: no refusal (no identity was obtained for it), `Data` is `unreadable`, `data` runs; arm 5: root `other` whose `/d/other` is absent beside the aliasing pair: the pair is still refused.
  - `a_refused_root_check_wins_over_an_alias_and_discards_remembered_verdicts`: roots `a` (source absent), `Data` and `data` built EXACTLY as arm 1 above (two directories, one `Strong` identity), and `z` whose source is `/d`: `Err(RefusedRoot { name: "z", .. })`, sink empty; control: the same fixture without `z` returns `RefusedAlias`.

- [ ] **Step 2: Run them to verify they fail.**
- [ ] **Step 3: Implement** the identity field and the alias map as in Interfaces.
- [ ] **Step 4: Run the crate**, all green.
- [ ] **Step 5: Mutant proofs, restore after each**: (b) fill the alias map from `Weak` identities too: arm 3 of test 22 goes red; (b') compare before the loop ends (refuse `Data`/`data` before checking `z`): test 23 goes red.
- [ ] **Step 6: `just check`, then commit**

```bash
git add crates/flux-core/src/verify.rs
git commit -m "feat(core): verify_several refuses two roots that are one directory on the destination"
```

### Task 5: report helpers and the several-source JSON summary

**Files:**
- Modify: `crates/flux-cli/src/verify_report.rs` (`SummaryBody` near 139-152, `summary_json` near 159-182, `mount_notice_line` near 185, `dest_absent_line` near 229-234; `mod tests` near 237, the eleven-field test near 283)
- Test: `crates/flux-cli/src/verify_report.rs` `mod tests`

**Interfaces:**
- Produces: `pub enum Sources<'a> { One(&'a Path), Several(&'a [PathBuf]) }`; `pub fn summary_json(sources: Sources<'_>, dest: &Path, hash: &'static str, s: &Summary, exit: u8) -> String` whose body has `#[serde(skip_serializing_if = "Option::is_none")] sources: Option<Vec<String>>` FIRST, then `#[serde(skip_serializing_if = "Option::is_none")] source: Option<String>`, then `dest, hash, ...` as today (so the one-source text is byte-identical: `source` first, no `sources`; the several text: `sources` first, no `source`); `pub fn root_line(name: &OsStr, s: &Summary) -> String` = `root <display_path(name)>: <matched> matched, <missing> missing, <mismatched> mismatched, <extra> extra, <unreadable> unreadable`; `pub fn root_absent_line(dst: &Path, name: &OsStr) -> String` = `destination does not exist: <dst.display()>/<display_path(name)> (every source path is reported missing)`; `pub fn refused_root_line(name: &OsStr, dst: &Path, reason: &str) -> String` = `error: root <display_path(name)>: <dst.display()>/<display_path(name)>: <reason>; nothing was verified`; `pub fn refused_alias_line(a: &OsStr, b: &OsStr) -> String` = `error: roots <display_path(a)> and <display_path(b)> are one directory on this filesystem; nothing was verified`. `display_path` takes a `Path`: pass `Path::new(name)`.

- [ ] **Step 1: Write the failing tests** (design tests 17, 18 and the helper lines): `summary_json_for_several_sources_omits_source_and_lists_sources` (keys in text order exactly `sources, dest, hash, matched, missing, mismatched, extra, unreadable, not_entered, bytes_hashed, exit`; `sources` equals the given strings in order); the existing eleven-field test still passes with `Sources::One` and its text unchanged (assert the exact string of today's test before the change and keep it as the oracle); `root_line_formats_the_five_counts` (`root a: 1 matched, 2 missing, 3 mismatched, 4 extra, 5 unreadable`); `stderr_lines_escape_the_root_name` on unix: a name with a newline renders `\n` in `root_line`, `root_absent_line`, `refused_root_line` and `refused_alias_line`, and each result is one line; `refused_root_line` renders `error: root b: /d/b: the destination is the source or lies inside it; nothing was verified`.

- [ ] **Step 2: Run them to verify they fail** (compile errors on the new names and the new `summary_json` signature).

- [ ] **Step 3: Implement** as in Interfaces; update `main.rs`'s two `summary_json` call sites (near 758-767 and 827) to `Sources::One(&args.source)` / `Sources::One(&src)` so the crate compiles (the CLI change proper is Task 6); replace the inline `RefusedRoot` and `RefusedAlias` bodies of `refusal_line` (Task 3) with `refused_root_line(&name, dst, &reason)` and `refused_alias_line(&a, &b)`, whose rendered text is identical.

- [ ] **Step 4: Run** `cargo nextest run -p flux-cli --no-fail-fast`: all green, the existing JSON CLI tests unchanged.

- [ ] **Step 5: Mutant proof**: swap the field order (`source` before `sources`): the several-source key-order test goes red; restore.

- [ ] **Step 6: `just check`, then commit**

```bash
git add crates/flux-cli/src/verify_report.rs crates/flux-cli/src/main.rs
git commit -m "feat(cli): report helpers for a several-source verify and a sources array in its summary"
```

### Task 5b: the resolver takes the usage verb

**Files:**
- Modify: `crates/flux-cli/src/resolve.rs:76-121` (`several`; the literal at `:115`), its unit tests (the eight `several(` call sites at `:314, 340, 352, 367, 380, 381, 391, 400`), `crates/flux-cli/src/main.rs:629` (copy's call)

**Interfaces:**
- Produces: `pub fn several(sources: &[PathBuf], dest: &Path, cap: usize, verb: &str) -> Result<(Vec<SourceSpec>, PathBuf), Stop>` whose file-DEST usage message is `format!("several sources cannot be {verb} the existing file {}", dest.display())`; copy passes `"copied onto"` (so its text is byte-identical to today's `several sources cannot be copied onto the existing file <DEST>`), verify will pass `"verified against"`.

- [ ] **Step 1: Pin copy's wording first**: in the unit test at `resolve.rs:367` (the file-DEST case) add the assertion that the `Stop::Usage` message starts with `several sources cannot be copied onto the existing file`; run it green BEFORE the change.
- [ ] **Step 2: Add the parameter**, update the eight test call sites and `main.rs:629` with `"copied onto"`.
- [ ] **Step 3: Run** `cargo nextest run -p flux-cli --no-fail-fast`: all green, the pinned wording unchanged.
- [ ] **Step 4: `just check`, then commit**

```bash
git add crates/flux-cli/src/resolve.rs crates/flux-cli/src/main.rs
git commit -m "refactor(cli): the several-source resolver takes its usage verb"
```

### Task 6: the CLI: `flux verify SOURCE... DEST`

**Files:**
- Modify: `crates/flux-cli/src/main.rs` (`Verify` doc comment near 52-56; `VerifyArgs` near 61-65; `fn verify` near 755-837; the `Commands::Verify` dispatch near 258), `crates/flux-cli/src/verify_report.rs` (`GUARANTEES` near 7-19, one new bullet)
- Test: `crates/flux-cli/tests/verify.rs` (helpers near 11-84; `SUMMARY_KEYS` near 55)

**Interfaces:**
- Consumes: `resolve::several(sources, dest, cap, verb)` (Task 5b), `resolve::first_symlink` (`resolve.rs:125`), `flux_core::verify::{verify_several, VerifyError}` (Tasks 3, 4), `crate::verify_report::{Sources, root_line, root_absent_line, refused_root_line, refused_alias_line, summary_json}` (Task 5), `exit_code::{USAGE, FAILED, for_verify}`.
- Produces: `VerifyArgs { #[arg(value_name = "SOURCE... DEST", required = true, num_args = 2..)] paths: Vec<PathBuf>, links, cross_filesystems, hash, json, verbose }`; `fn verify(args) -> u8` dispatching on `args.paths.len()`: two paths call today's body (which reads `args.source` and `args.dest` in several places; those reads become `&args.paths[0]` and `&args.paths[1]`, or the body takes the two paths as parameters); more call `fn verify_several_cli(args: &VerifyArgs) -> u8`, which: calls `resolve::several(&paths[..n-1], &paths[n-1], usize::MAX, "verified against")` (Task 5b), so a file DEST reads `several sources cannot be verified against the existing file <DEST>`; maps `Stop::Usage` to exit 2 + `error: <msg>`, `Stop::SymlinkSource` and `Stop::Failed` to exit 1 with the one-source lines and, under `--json`, `summary_json(Sources::Several(&paths[..n-1]), &paths[n-1], "blake3", &Summary::default(), 1)`; collects the canonical sources in command-line order (`specs.iter().map(|s| s.src.clone()).collect::<Vec<PathBuf>>()`) BEFORE the call, because `verify_several` takes `specs` by value and reorders them; runs `verify_several` with `on_absent` printing `root_absent_line(&dst, name)`, `on_root_done` printing `root_line`, the sink routing through `route_record` as today; maps every `Err(e)` through `refusal_line(&e, &dst)` (Task 3, rewired to the report helpers in Task 5), exit 2; on `Ok(total)` prints `summary_line(&total)` or `summary_json(Sources::Several(&collected_sources), &dst, "blake3", &total, code)` and returns `for_verify(&total)`. The `Verify` doc comment's exit-2 clause becomes "2: a usage error, roots that are the same or nested, a refused source name, two sources that take one name". `GUARANTEES` gains the bullet "- With several sources, only `DEST/<name>` for each source is compared; other entries of DEST are neither reported nor counted."

- [ ] **Step 1: Write the failing CLI tests** (design tests 11 to 16, 20, 24), in `tests/verify.rs`, one `#[test]` each, with a helper `fn two_roots(d: &Path) -> (PathBuf, PathBuf, PathBuf)` making `a/` (file `x` = `A`), `b/` (file `y` = `BB`) and `dest/{a/x, b/y}` identical (the existing `verify(&[&Path])` helper takes paths only; a test that passes `--json` or `--help` builds its command from `flux()` directly, as `a_file_source_maps_like_copy` does):
  - `three_positionals_verify_each_root_under_dest`: `flux verify a b dest` exits 0 and stdout's last line is `verified: 2 matched, 0 missing, 0 mismatched, 0 extra, 0 unreadable; 6 bytes hashed`; after flipping `dest/b/y` to `BX`, exit 1 and a stdout line `mismatched: b/y: VERIFY_MISMATCH: ` prefix.
  - `dest_strangers_and_copy_residue_are_invisible`: add `dest/c/z`, `dest/a.flux-lock`, `dest/.flux/operations/<ZEROS>/state`: exit 0, no stdout line starts with `extra:`.
  - `json_several_summary_has_sources_and_no_source`: `--json` on the clean pair, with the sources TYPED non-canonically as `<dir>/./a` and `<dir>/b/../b` and DEST as `<dir>/./dest`: the last line's `summary` keys in text order are `sources, dest, hash, matched, missing, mismatched, extra, unreadable, not_entered, bytes_hashed, exit`, `sources` is the two canonical paths (`std::fs::canonicalize` of `a` and `b`) in that order and differs from the typed strings, `dest` is the canonical `dest`; with the second source absent: exit 1, exactly one stdout line, `sources` equals the two strings as typed, `"exit": 1`; the one-source `--json` summary still has `source` and no `sources`.
  - `per_root_stderr_lines_and_the_absent_dest_lines`: stderr contains `root a: 1 matched, 0 missing, 0 mismatched, 0 extra, 0 unreadable` then the `root b:` line, in human mode and under `--json`; with `dest` absent: exit 1 and stderr has `destination does not exist: <dest>/a (every source path is reported missing)` and the `b` twin, and stdout has `missing: a/x` and `missing: b/y`.
  - `refusals_match_copy`: one positional exits 2, stdout empty, stderr contains `error`; a missing source together with a file DEST exits 1 (source failures first); a symlink source exits 1 with `SYMLINK_CREATION_UNAVAILABLE: <the link as typed>: a symlink SOURCE is not followed; copy its parent folder, or name its target`; two sources with one name (`x/a` and `y/a`) exit 2 with `error:` and empty stdout; a DEST that is a file exits 2 with `several sources cannot be verified against the existing file`; a source named `q.flux-lock` exits 2.
  - `help_shows_the_several_source_synopsis_and_guarantee`: `--help` contains `SOURCE... DEST` and `With several sources, only`.
  - `a_root_name_with_a_newline_or_invalid_bytes_prints_escaped_on_stderr` (unix): a source directory named `a\nb`: the `root a\\nb:` line is one line.
  - `a_refused_root_check_and_a_refused_alias_exit_2_with_nothing_on_stdout`: sources `a` and `d/b` where `b` is `dest` itself (`flux verify a dest dest`, the second source IS the DEST folder): exit 2, stdout empty, stderr contains `error: root dest: <dest>/dest: the destination is the source or lies inside it` and ends with `; nothing was verified` (the by-path-alone note may sit between); the alias line is pinned at the report level (Task 5).

- [ ] **Step 2: Run them to verify they fail** (`cargo nextest run -p flux-cli --no-fail-fast -E 'binary(verify)'`; the new ones fail on exit codes or parse errors).

- [ ] **Step 3: Implement** as in Interfaces.

- [ ] **Step 4: Run** `cargo nextest run -p flux-cli --no-fail-fast`: all 17 old tests plus the 8 new green; the copy tests that pin `several sources cannot be copied onto` unchanged.

- [ ] **Step 5: Mutant proofs, restore after each**: (a) print `root_line` only in human mode: the per-root test's `--json` arm goes red; (b) map `RefusedRoot` to exit 1: test 24 goes red; (c) pass the typed paths into the completed `sources`: the JSON test goes red.

- [ ] **Step 6: `just check`, then commit**

```bash
git add crates/flux-cli/src/main.rs crates/flux-cli/src/verify_report.rs crates/flux-cli/tests/verify.rs
git commit -m "feat(cli): flux verify takes several sources, each compared with DEST/<name>"
```

### Task 7: documents and boundaries

**Files:**
- Modify: `README.md` (the `### Verify` section near 88-100: the synopsis becomes `flux verify SOURCE... DEST [...]`, one sentence after the first: "With several sources each is compared with `DEST/<name>`, as `flux copy` places it; what else DEST holds is not examined."), `TODO.md` (near 1212: the cut 14b item moves to `## Closed` with the commit; a new item under "Cut 14 open": "A stderr advisory for DEST's top-level entries that belong to no root of a several-source verify (design 2026-10-27 section 5, option O3 rejected as records; an advisory line is the candidate)"), `docs/accepted-boundaries.md` (new section `## Cut 14b - several sources for flux verify (2026-10-27)` with the five entries of design section 11, one line each, each with its anchor: `verify.rs` `verify_several`, `run/roots.rs:87-134`, `resolve.rs:76-121`).

- [ ] **Step 1: Make the three edits.**
- [ ] **Step 2: Run `typos` and `just check`** (the README and TODO are read by typos; no code changes).
- [ ] **Step 3: Commit**

```bash
git add README.md TODO.md docs/accepted-boundaries.md
git commit -m "docs: several sources for flux verify (cut 14b)"
```

### Self-review

- Spec coverage: 6.1 (Task 6), 6.2 (Tasks 3, 4), 6.3 (Task 3), 6.4 (Task 1), 6.5 (Tasks 3, 5, 6), 6.6 (Task 6), 6.7 (Tasks 6, 7), section 8 interfaces (Tasks 1 to 6), section 9 tests 1 to 24 (1,3,4,5,6,8,9,21 and the call-site pin of C4 in Task 3; 2,10 in Task 1; 7,19 in Task 4a; 22,23 in Task 4b; 11 to 16,20,24 in Task 6; 17,18 in Task 5), section 11 (Task 7).
- Type consistency: `ControlFilter`, `RootVerdict`, `check_pair`, `walk_checked`, `verify_several`, `VerifyError` variants, `Sources`, the five report helpers are named identically across tasks.
- Proportion: 9 tasks (0 to 7, with 4a, 4b and 5b) for a 208-line design; test bodies are given for the one test whose record order is the subtle part, the rest as names with their assertions.
- Review Focus: five lines, each pinned in the task that owns the code.
- Open items left to the builder (design section 10): helper naming (`Sources` lives in `verify_report.rs`, Task 5). (The three unreadable arms of Task 4 were traced against the fake: absent source for the check, `fail_nth("metadata")` for the probe, `fail_read_dir_of` on the source root for the walk.)

## Stand-downs (adversarial panel on the plan)

Round 1 (solo five-seat panel: twelve findings, four blocking; agy escalation: two debt, both also raised by the solo panel). Folded: placeholder CLI arms for the new `VerifyError` variants land with Task 3 so every commit compiles (AB-1); the resolver's verb is its own Task 5b with the copy wording pinned first (AB-2, agy F2); `Sources` lives in `verify_report.rs` (AB-3); `RootVerdict` gains `WrongType` and the identity field in Task 4, not Task 2, so no dead field precedes its reader (AB-4, LI-3); Task 4 asserts `on_root_done` and the total for wrong-type and probe-error roots (CA-1); the three unreadable arms use the fake's real seams, an absent source for the check, `fail_nth("metadata")` for the probe, `fail_read_dir_of` for the walk, since `walk_with` lists the root at construction and the check phase never lists (CA-2, agy F1); the alias test models one directory as two directories given one `Strong` identity through `set_identity`, because the fake's `destination_root` does not fold case, and the design's test 22 is amended to say so (CA-3); the Task 1 test's record order follows the 0x00 separator and creates the parents (LI-1); the two edited existing tests are enumerated (LI-2); the alias test gains a `Weak` arm and the mutant that proves `Strong`-only (MG-1); the JSON test types non-canonical paths so the canonical rule is separable (MG-2).
- DISCARDED-BELOW-FLOOR (solo): the `RefusedAlias` CLI arm has no CLI-level test; the design's test 24 accepts the report-level pin, and the host filesystems are case-sensitive.
- DISCARDED-BELOW-FLOOR (solo): `SUMMARY_KEYS` in the CLI tests is listed without saying it stays unedited; the inline key list says so.
- DISCARDED-BELOW-FLOOR (agy): whether the engine strips the refusal tail with `strip_suffix` or a split is the builder's.

Round 2 (solo: Axiom Breaker, Cascade Analyst, Literal Implementer, Mechanism Gamer, Activation Auditor rotated in; agy round 2 GREEN on the round-1 folds). Folded: test 23 builds its alias as test 22 does and carries a control (AB-1); the design's test 7 is amended to the plan's three arms (AB-2); one shared `refusal_line` function serves both CLI paths, no placeholder arms (AB-3); `plan_roots` is re-exported `pub(crate)` from the private `roots` module and `run/mod.rs` is in Task 3's files (LI-1); the commit trailer is the executing session's, not a hard-coded name (LI-2); every commit stages its files explicitly and a Task 0 commits the reviewed documents first, so no code commit sweeps the design amendment (AA-1); Task 4 split into 4a (probe, wrong type, unreadable arms) and 4b (identity, alias, precedence).
- DISCARDED-BELOW-FLOOR (solo r2): Task 1's mutant list names only `NestedRoot`-as-`Root`; the test's partial-temporary case would also redden a `NestedRoot`-as-`None` mutant.
- DISCARDED-BELOW-FLOOR (solo r2): no dedicated arm for a destination-side walk-phase failure (`destination: ` prefix); the one-source fixtures cover that path (`verify.rs:1393-1402`); the Task 1 test name differs from the design's test 2 name (the design names behaviours, the plan names tests).

Round 3 (solo: Axiom Breaker, Cascade Analyst, Literal Implementer, Mechanism Gamer, Blindspot Auditor rotated in). Folded: the alias tests left in Task 4a by the round-2 split are removed, 4a's Step 3 names only the probe and verdicts, the Review Focus pointers name 4a and 4b (AB-1); `refusal_line` takes the destination path and Task 3 writes the four final renderings inline, Task 5 rewires two of them to its helpers in a named step, Task 6 only calls it (LI-1); Task 3's first test bullet states the rows once (LI-2); Task 6 collects the canonical sources before the by-value call (LI-3); Task 3's commit message names the CLI change (the Blindspot seat's finding).
Round 3 (agy): GREEN on the text before the round-3 folds; its two below-floor notes (the inline renderings of `refusal_line` before Task 5, the `fail_nth` ordinal read from the log) are the plan's intended staging.
- DISCARDED-BELOW-FLOOR (solo r3): the mutant lettering of Tasks 4a and 4b is non-contiguous; cosmetic.
- DISCARDED-BELOW-FLOOR (solo r3): Task 6's mutant (b) is artificial since the caller hard-codes exit 2 after `refusal_line`; test 24 still reddens on the text; harmless.

Round 4 (closing solo pass over Tasks 3 to 6: Axiom Breaker, Cascade Analyst, Literal Implementer, Mechanism Gamer; the first two clean, including a trace of test 24 through the absent-DEST branch of `check_roots`). Folded: the `fail_nth` ordinal is defined as the fake counts it, per key from creation, first occurrence (LI-1); the summing test puts a non-zero value in all seven `Summary` fields and two more mutants pin `extra` and `unreadable` (MG-1); the Review Focus 1 pointer and Task 6's "today's body" note corrected (below-floor nits).

Round 5 (solo: Axiom Breaker, Cascade Analyst, Literal Implementer, Mechanism Gamer, Resource Vampire rotated in; agy round 5 GREEN). Folded: `check_roots` returns the identity it already reads, so `check_pair` never adds a second `identity()` call and the fake's ordinals stay stable (LI-1); an engine test runs a user's `.flux` through `verify_several` itself, pinning the `NestedRoot` choice at the call site, with mutant (d) (MG-1); the fixture count is no longer pinned in prose and the CLI tests that pass flags build their command directly (below-floor notes). Five rounds in all: solo findings 12, 6, 3, 2, 2 and agy findings 2, 0, 0, 0 (no agy round 4), 0; agy GREEN on rounds 2, 3 and 5. The hard round cap is reached; the owner decides whether to continue or ship.
