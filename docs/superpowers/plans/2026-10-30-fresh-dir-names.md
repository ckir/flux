# Fresh-directory name bookkeeping (bounded listing, cut A) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop recording, for every file published into a directory this run created, a name that nothing will ever read, so the full-copy memory slope falls from about 577 B/entry to about 95 B/entry (the source-walk listing, cut B, is the rest).

**Architecture:** `NameIndex` (`crates/flux-core/src/names.rs`) gains a tracking flag. A directory this run created (or, dry, one that does not exist) gets an UNTRACKED index whose `record_publication` returns at once; every other directory keeps today's behaviour. The three places that build the index for a frame decide with one helper, `NameIndex::for_dir(fresh, listed)`, so the decision is written once and unit-tested.

**Tech Stack:** Rust (workspace), `cargo nextest`, `/usr/bin/time -v`, the Phase 3 fixture generator `benches/phase3/make_tree.py`.

**Spec:** `FLUX_FULL_UPDATED_SPEC_V16.md` lines 505-515 (resident RAM bounded; the scanner never builds an unbounded list) and section 10.1. Evidence: `docs/phase3-measurements.md` ("Peak memory against directory size"); AGY-FIRST consult `.clavity/seams/bounded-listing.md` (reply `.clavity/scratch/bounded-listing/r1-reply.md`); owner decision 2026-10-30: "two cuts, measure first", bounded listing before Cut 1. The driver's own measurement (below) is the reason for this cut.

**Measured basis (2026-10-30, driver, peak RSS, flat zero-byte files, 3 runs each, medians in KiB):** current 56324 at 100,000 entries and 281700 at 500,000 (577.0 B/entry); the same build with `record_publication` skipped for new names 16604 and 53856 (95.4 B/entry). The skip was an ablation of every new-name publication, valid there because the destination was absent so every directory was fresh.

## Global Constraints

- Branch `feat/fresh-dir-names` off `main` (`bb444fb`). `just pr` arms auto-merge, so every gate runs before it; the owner gives the go.
- Behaviour must not change: no test is edited to make it pass; the existing `flux-core` suite is the oracle. The gate is `just check` (fmt, clippy, typos, unused-deps, doc-check, license, test); do not add stricter flags.
- A directory is UNTRACKED only when its frame is `fresh`. `fresh` is set once when a frame is built (`tree.rs:1174`, `:1232`, `:1697`) and never assigned afterwards (checked: no `fresh =` in `tree.rs`).
- `NameIndex::resolve` and `NameIndex::owner` are reached only when the frame is NOT fresh (`tree.rs:1926-1931`, `:1956`, `:3065-3070`, `:3095`).
- No new dependency. No change to `Frame`, `RootState` or any public item.
- Memory is measured at the top level only (the controller), sequentially, machine otherwise quiet; it is a peak-RSS count, not a wall-clock figure, and no timing is claimed.

## Review Focus

- A pre-existing directory with no claim key (`claim_parent` is `None`, `tree.rs:1683` `_ =>` arm today) must keep tracking: it is not fresh, `resolve` runs on it with an empty listing, and `owner` guards case-folded collisions. Pinned by Task 1 test `a_dirs_index_that_is_not_fresh_still_records` and by the mutant in Task 2.
- A dry run's planned frames (`root.is_none()`, `tree.rs:1174`; the reserved-name frame, `tree.rs:1230-1233`) are fresh and untracked. Pinned by the `for_dir` truth-table test (fresh with no handle).
- A case-folded collision inside a directory this run created is still refused, by the no-replace publish and not by the name index (`tree.rs:423-427`). Pinned by the existing tests `two_source_directories_folded_into_one_destination_are_a_collision` (`tree.rs:5210`) and `a_link_and_a_file_folding_to_one_name_collide` (`tree.rs:5662`), run in Task 2.
- `--resume` into a pre-existing destination reads the index (`tree.rs:1990-2002` records with `stored`); unchanged because that destination is not fresh. Pinned by the existing resume tests (whole suite).
- The strict batched path flushes through `flush_batch` into `record_publication` (`tree.rs:4000-4007`); an untracked index must not break the batch. Pinned by the whole suite (`names_that_fold_in_one_directory_flush_before_the_second_is_staged`, `tree.rs:7774`).

---

### Task 1: `NameIndex` tracking flag and `for_dir`

**Files:**
- Modify: `crates/flux-core/src/names.rs` (struct `NameIndex` at `:17-31`, `for_new_dir` at `:70-79`, `for_existing` at `:82-87`, `owner` at `:89`, `resolve` at `:100`, `record_publication` at `:178`; tests module at `:209`, helpers `fs_with`, `key` at `:217` and `:240`)
- Test: the same file's `mod tests`.

**Interfaces:**
- Produces (all `pub(crate)`, in `impl NameIndex`):
  - `fn for_created_dir() -> Self`: empty listing, UNTRACKED.
  - `fn for_dir<D: DirHandle>(fresh: bool, listed: Option<&D>) -> flux_fs::Result<Self>`: `fresh` → `for_created_dir()`, no read; not fresh with `Some(dir)` → `for_existing(dir)`; not fresh with `None` → `for_new_dir()`, no read.
  - `fn is_tracked(&self) -> bool`.
  - `for_new_dir()` and `for_existing()` keep their signatures and stay TRACKED.
- Consumes: nothing from other tasks.

- [ ] **Step 0 (state verification):** open `names.rs` and confirm: the struct has exactly the fields `listing, table, owners, pending, learned, next_seq`; `for_new_dir` builds them empty; `for_existing` calls `for_new_dir` then fills `listing`; `record_publication` begins with `let mut spellings = Vec::with_capacity(2);`. If any differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: Write the failing tests** in `mod tests` (they already have `fs_with`, `key`, `calls_norm`). Names and assertions, complete:
  - `a_created_dirs_index_records_nothing`: `let mut idx = NameIndex::for_created_dir();` call `idx.record_publication(&key("a"), None, OsStr::new("a"), None, FileIdentity::Unavailable)` for names `a`, `b`, `c`; assert `idx.listing.is_empty()`, `idx.owners.is_empty()`, `idx.pending.is_empty()`, `!idx.is_tracked()`.
  - `a_dirs_index_that_is_not_fresh_still_records`: the same three calls on `NameIndex::for_new_dir()`; assert `idx.listing.len() == 3`, `idx.owner(OsStr::new("b")) == Some(&key("b"))`, `idx.pending.len() == 3`, `idx.is_tracked()`.
  - `for_dir_decides_by_fresh_and_never_reads_for_a_fresh_or_unlisted_dir`: with `fs_with(&["x", "y"])` and a handle `d` for `/d` (obtain it as `an_exact_name_resolves_to_itself` at `:252` does: `let d = fs.destination_root(Path::new("/d")).unwrap();`): `for_dir(true, Some(&d))` is untracked with an empty listing; `for_dir(false, None::<&_>)` is tracked with an empty listing; `for_dir(false, Some(&d))` is tracked with `listing == {"x","y"}`; and `calls_norm(&fs)` holds exactly ONE `read_dir(` call (the third).
  - `#[cfg(debug_assertions)] #[should_panic] a_created_dirs_index_is_never_resolved`: `NameIndex::for_created_dir().resolve(&d, OsStr::new("x"))` panics.

- [ ] **Step 2: Run, expect FAIL to compile or fail:** `cargo nextest run -p flux-core names::` → the new tests fail (items missing).

- [ ] **Step 3: Implement.** Add `tracked: bool` to the struct (doc comment: an index for a directory this run created is not tracked because nothing reads it, `resolve` and `owner` are reached only for a non-fresh frame). `for_new_dir` sets `tracked: true`; `for_created_dir` is `Self { tracked: false, ..Self::for_new_dir() }`; `for_existing` is unchanged (it starts from `for_new_dir`). `is_tracked` returns the flag. `record_publication` begins `if !self.tracked { return; }`. `resolve` and `owner` begin `debug_assert!(self.tracked, "an untracked index is never read")`. `for_dir` is the three-way match above; its doc comment states the truth table. Update the doc comment of `for_new_dir` ("A directory this run created") to say what it is now: a directory that is NOT fresh but has no listing to read.

- [ ] **Step 4: Run:** `cargo nextest run -p flux-core names::` → all `names::` tests pass, including the four new ones and every existing one unchanged.

- [ ] **Step 5: Non-vacuity (logic mutants, on a scratch copy or reverted after; never committed).** M1: delete the `if !self.tracked { return; }` line → `a_created_dirs_index_records_nothing` must be the test that goes red. M2: make `for_new_dir` set `tracked: false` → `a_dirs_index_that_is_not_fresh_still_records` must go red. M3: in `for_dir`, change `if fresh` to `if !fresh` → `for_dir_decides_by_fresh...` must go red. Report which test went red for each; a mutant that goes red a different test or none is reported, not adapted around.

- [ ] **Step 6: Commit** `git add crates/flux-core/src/names.rs` / `git commit -m "feat: an index for a directory the run created records nothing"` (trailer `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`).

### Task 2: build the index with `for_dir` at the three frame sites

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`root_state` names at `:1166-1174`; the reserved-name planned frame at `:1228-1235`; `enter_dir` names at `:1681-1699`)
- Test: the existing suite is the oracle (no new test file); plus Task 1's tests.

**Interfaces:**
- Consumes: `NameIndex::for_dir`, `NameIndex::for_created_dir` from Task 1.
- Produces: no new item.

- [ ] **Step 0 (state verification):** confirm `tree.rs:1168-1173` is `let names = match root { Some(dir) if claim_parent.is_some() && !root_created => NameIndex::for_existing(dir).map_err(|e| CopyError::at(CopyStep::Resolve, e))?, _ => NameIndex::for_new_dir(), };` followed by `Ok(RootState { names, claim_parent, fresh: root_created || root.is_none() })`; `:1230` is `names: NameIndex::for_new_dir(),` beside `fresh: true,`; `:1683-1691` is `let names = match &child { Some(dir) if pre_existing && claim_parent.is_some() => match NameIndex::for_existing(dir) { Ok(names) => names, Err(e) => { report(out, on_report, path.to_path_buf(), TreeFailureCause::CreateDir(e)); return Ok(Frame::Skipped); } }, _ => NameIndex::for_new_dir(), };` and `:1697` is `fresh: !pre_existing,`. If any differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: Edit the three sites** (the behaviour of a NON-fresh directory must be byte-for-byte what it is today):
  1. `root_state`: compute `let fresh = root_created || root.is_none();` first; `let names = NameIndex::for_dir(fresh, root.filter(|_| claim_parent.is_some())).map_err(|e| CopyError::at(CopyStep::Resolve, e))?;`; return `RootState { names, claim_parent, fresh }`.
  2. The reserved-name frame at `:1230`: `names: NameIndex::for_created_dir(),` (its `fresh: true` stays).
  3. `enter_dir`: `let names = match NameIndex::for_dir(!pre_existing, child.as_ref().filter(|_| claim_parent.is_some())) { Ok(names) => names, Err(e) => { report(...CreateDir(e)); return Ok(Frame::Skipped); } };` keeping the existing error arm text and `fresh: !pre_existing` as it is. Confirm by reading `:1560-1676` that `pre_existing` is true only when `child` is `Some`, so the filter cannot drop a handle the old arm used.
  If the code does not compile as written because of a borrow or type, STOP and report `[original] -> [yours] because <reason>`; do not restructure.

- [ ] **Step 2: Run the oracle:** `cargo nextest run -p flux-core` → all pass, same count as before the edit plus the four new `names::` tests (record the before and after counts in the report).

- [ ] **Step 3: Non-vacuity mutants (on a scratch copy or reverted; never committed).** M4: in `enter_dir` pass `true` instead of `!pre_existing` (every directory untracked) → name the test(s) that go red; the expected oracle is `the_names_owner_check_refuses_a_folded_name_before_any_claim` (`tree.rs:6470`). M5: in `root_state` pass `true` instead of `fresh` → name the test(s) that go red. If NEITHER M4 nor M5 turns any existing test red, STOP and report `COVERAGE_HOLE: <mutant>`; the controller will specify a test, do not invent one.

- [ ] **Step 4: Gate:** `just check` → exit 0.

- [ ] **Step 5: Commit** `git add crates/flux-core/src/tree.rs` / `git commit -m "feat: build a frame's name index by whether the directory is fresh"` (same trailer).

### Task 3: measure the result and record it (controller, top level; no subagent)

**Files:**
- Modify: `docs/phase3-measurements.md` (a new section after "Peak memory against directory size"); `TODO.md` ("Cut 22 open", the bounded-listing item: note cut A done, cut B open).
- No product code.

- [ ] **Step 1: Build the changed binary** with the same flags as the baseline: `RUSTFLAGS="-C force-frame-pointers=yes" CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_TARGET_DIR=$HOME/.cache/flux-fresh-target cargo build --release -p flux-cli`. The baseline binary is `$HOME/.cache/flux-fp-target/release/flux` (built from `main`'s code, whose `crates/` are unchanged by #146 and #147).
- [ ] **Step 2: Census.** With the fixtures `$HOME/.cache/flux-rss-flat-100000` and `-500000` (zero-byte, `make_tree.py flat`), for `n` in 100000, 500000 and run 1-3, alternating baseline and changed: `rm -rf $DST; sync; /usr/bin/time -v <flux> copy <fixture> $DST --durability normal`, record `Maximum resident set size` and the count of entries in `$DST`. Also `--dry-run` once each at 500000 (expect unchanged). Quote medians and the slope `(median500k - median100k) * 1024 / 400000`.
- [ ] **Step 3: Acceptance, written before the data:** PASS if the changed binary's full-copy slope is at most 120 B/entry AND every run copied all `n` entries AND the `--dry-run` slope is within 5 B/entry of 76.1. The ablation predicted 95.4. A slope above 120 means the cost is not all in `record_publication`: report it as UNRESOLVED and do not claim the cut worked.
- [ ] **Step 4: Write the section and the TODO line; `just check` → exit 0; commit** (`docs: measure the fresh-directory name bookkeeping cut`).

---

## Plan self-audit (against the writing-plans checklist)

1. Spec coverage: the goal (bounded resident memory on the full-copy path for a created directory) maps to Tasks 1-3; the part of spec 505-515 about the source listing is cut B and is stated as out of scope here.
2. Placeholders: none. The only conditional is Task 2 Step 3's `COVERAGE_HOLE` stop, owned by the controller.
3. Type consistency: `for_created_dir`, `for_dir`, `is_tracked`, `tracked` are used with the same names in both tasks.
4. Edge cases covered: pre-existing no-claim directory (Review Focus 1), dry-run planned frames (2), folded collision in a fresh directory (3), resume (4), strict batch (5). Not covered by a new test, by design: `names.owner` on a fresh directory (unreachable, enforced by `debug_assert!`).
5. Proportion: the plan is shorter than the diff it asks for is wide; no function bodies are given beyond the three-way match.
