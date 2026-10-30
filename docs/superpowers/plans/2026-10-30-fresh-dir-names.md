# Fresh-directory name bookkeeping (bounded listing, cut A) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop recording, for every file published into a directory this run created, a name that nothing will ever read, so the full-copy memory slope falls from about 577 B/entry to about 95 B/entry (the source-walk listing, cut B, is the rest). NOT bounded by this cut: a directory that already existed when the run started (an empty `mkdir`-ed destination, or a `--resume` over an interrupted run) is not fresh, so it keeps today's ~577 B/entry; Task 3 records that limitation in `TODO.md`.

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

- A pre-existing directory is not fresh and keeps a tracked index: `resolve` and `owner` run on it (`tree.rs:1930`, `:1956`, `:3069`, `:3095`) whenever it has a claim key. A directory with NO claim key (`claim_parent` is `None`) never touches its index at all: `copy_one` and `link_one` leave through the keyless branch before any `resolve`, `owner` or `record_publication` (`tree.rs:1827-1876`, `:2999-3035`), so `for_dir(false, None)` returning a tracked empty index is harmless and kept only to leave that case exactly as it is today. Pinned by Task 1 test `a_dirs_index_that_is_not_fresh_still_records` and by the new tree test T3 in Task 2.
- A dry run's planned frames (`root.is_none()`, `tree.rs:1174`; the reserved-name frame, `tree.rs:1230-1233`) are fresh and untracked. Pinned by the `for_dir` truth-table test (fresh with no handle).
- A case-folded collision inside a directory this run created is still refused, by the no-replace publish and not by the name index (`tree.rs:423-427`). NO existing test pins this: the helpers `batched` and `batched_verifying` pass `root_created = false` to `copy_tree_at` (`tree.rs:7244`) and pre-create `/dst`, so the existing fold tests (`tree.rs:5210`, `:5662`, `:6470`, `:7774`) all run in a tracked root. Pinned by the new tree tests T1 and T2 in Task 2, written against the unchanged code first.
- `--resume` into a pre-existing destination reads the index (`tree.rs:1990-2002` records with `stored`); unchanged because that destination is not fresh. Pinned by the existing resume tests (whole suite).
- The strict batched path flushes through `flush_batch` into `record_publication` (`tree.rs:4000-4007`); an untracked index must not break the batch, and a fold inside a created directory under a batch must still be refused. Pinned by the new tree test T2 (Strict, batch policy 64). The existing `names_that_fold_in_one_directory_flush_before_the_second_is_staged` (`tree.rs:7774`) runs in the tracked root, so it does not pin this.

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
  - `for_dir_decides_by_fresh_and_never_reads_for_a_fresh_or_unlisted_dir`: with `fs_with(&["x", "y"])` and a handle `d` for `/d` (obtain it as `an_exact_name_resolves_to_itself` at `:252` does: `let d = fs.destination_root(Path::new("/d")).unwrap();`): `for_dir(true, Some(&d))` is untracked with an empty listing; `for_dir(false, Some(&d).filter(|_| false))` (the form the production call sites use, so `D` is inferred) is tracked with an empty listing; `for_dir(false, Some(&d))` is tracked with `listing == {"x","y"}`; and `calls_norm(&fs)` holds exactly ONE `read_dir(` call (the third).
  - `#[cfg(debug_assertions)] #[should_panic] a_created_dirs_index_is_never_resolved`: `NameIndex::for_created_dir().resolve(&d, OsStr::new("x"))` panics.
  - `#[cfg(debug_assertions)] #[should_panic] a_created_dirs_index_is_never_asked_for_an_owner`: `NameIndex::for_created_dir().owner(OsStr::new("x"))` panics.

- [ ] **Step 2: Run, expect FAIL to compile or fail:** `cargo nextest run -p flux-core names::` → the new tests fail (items missing).

- [ ] **Step 3: Implement.** Add `tracked: bool` to the struct (doc comment: an index for a directory this run created is not tracked because nothing reads it, `resolve` and `owner` are reached only for a non-fresh frame). `for_new_dir` sets `tracked: true`; `for_created_dir` is `Self { tracked: false, ..Self::for_new_dir() }`; `for_existing` is unchanged (it starts from `for_new_dir`). `is_tracked` returns the flag. `record_publication` begins `if !self.tracked { return; }`. `resolve` and `owner` begin `debug_assert!(self.tracked, "an untracked index is never read")`. `for_dir` is the three-way match above; its doc comment states the truth table. Update the doc comment of `for_new_dir` ("A directory this run created") to say what it is now: a directory that is NOT fresh but has no listing to read.

- [ ] **Step 4: Run:** `cargo nextest run -p flux-core names::` → all `names::` tests pass, including the five new ones and every existing one unchanged.

- [ ] **Step 5: Non-vacuity (logic mutants, on a scratch copy or reverted after; never committed).** M1: delete the `if !self.tracked { return; }` line → `a_created_dirs_index_records_nothing` must be the test that goes red. M2: make `for_new_dir` set `tracked: false` → `a_dirs_index_that_is_not_fresh_still_records` must go red. M3: in `for_dir`, change `if fresh` to `if !fresh` → `for_dir_decides_by_fresh...` must go red. M4: delete the `debug_assert!` from `owner` → `a_created_dirs_index_is_never_asked_for_an_owner` must go red (debug build). Report which test went red for each; a mutant that goes red a different test or none is reported, not adapted around.

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

- [ ] **Step 1b: Write three tree tests FIRST and run them against the UNCHANGED `tree.rs` (they must pass there; they characterise behaviour the change must keep).** Add to `tree.rs`'s `mod tests` near `a_link_and_a_file_folding_to_one_name_collide` (`:5662`), reusing the helpers `batched` (`:7163`), `policy` (`:7133`), `only_copy_failure` (`:5515`). Common setup: `let fs = FaultFs::new();` create `/src`, `/src/sub`, `/dst`; `fs.write_file("/src/sub/A", b"upper"); fs.write_file("/src/sub/a", b"lower"); fs.set_case_insensitive(true);`. The walk visits `A` before `a` (bytewise order), so `A` is published and `a` is the collision.
  - T1 `a_folded_name_inside_a_directory_the_run_created_is_still_a_collision`: `/dst/sub` is ABSENT, so `enter_dir` creates it (fresh). `let (r, out, got) = batched(&fs, policy(1), Durability::Normal);` assert `r.is_ok()`, `only_copy_failure(&got, "sub/a").code() == Code::DestinationNamespaceCollision` and `.step == CopyStep::Gate` (the no-replace gate refuses it, the contrast with T3's `Claim`), `fs.read_file("/dst/sub/A").as_deref() == Some(&b"upper"[..])`, `out.files_copied == 1`, and `/dst/sub` holds exactly one object.
  - T2 `a_folded_name_inside_a_created_directory_is_a_collision_under_a_strict_batch`: the same setup and assertions with `batched(&fs, policy(64), Durability::Strict)`. (Traced by reading: the fold precheck flushes `sub/A` first, then `sub/a` is refused at the `stage_file` gate, `tree.rs:1919-1920`, `:2151`, `:3764`.)
  - T3 `a_folded_name_inside_a_pre_existing_subdirectory_is_refused_by_the_name_index`: additionally `fs.create_dir(Path::new("/dst/sub")).unwrap();` BEFORE `set_case_insensitive`; `batched(&fs, policy(1), Durability::Normal)`; the same assertions, plus `only_copy_failure(&got, "sub/a").step == CopyStep::Claim` (the `names.owner` refusal, as `:6470` asserts for the root).
  - T4 `enter_dir_builds_an_untracked_index_only_for_a_directory_the_run_creates` (optional pin of the wiring; `enter_dir` is a private fn in the same file, `tree.rs:1479`, so `mod tests` can call it): with `FaultFs` holding `/dst` and a `Frame::Live` for it on a one-element stack (the frame's other fields as `copy_tree_frames` builds them, `tree.rs:1108-1116`), call `enter_dir` for a child `sub` ABSENT from `/dst` and assert the returned `Frame::Live` has `fresh == true` and `!names.is_tracked()`; then create `/dst/sub` and call it again with a fresh stack, asserting `fresh == false` and `names.is_tracked()`. If building those arguments needs anything beyond that (a `Shared`, a claim store), STOP and report `SCOPE: T4 needs <what>`; do not build it. The Task 3 one-subdirectory fixture is the fallback pin for `enter_dir`. `root_state` is pinned by the flat fixture.
  If a test fails on the unchanged code because the failing path or step differs from what is written, STOP and report `[original] -> [yours] because <reason>` with the actual code and step; do not weaken the assertion.

- [ ] **Step 2: Run the oracle:** `cargo nextest run -p flux-core` → all pass, same count as before the edit plus the five new `names::` tests and the three tree tests (four with T4) (record the before and after counts in the report).

- [ ] **Step 3: Non-vacuity mutants (on a scratch copy or reverted; never committed).** Record which test goes red for each; if ANY of M6 to M10 leaves every test green, STOP and report `COVERAGE_HOLE: <mutant>` (the controller specifies the test; do not invent one).
  - M6: in `for_dir`, make the not-fresh `Some(dir)` arm return `for_existing(dir)` with `tracked` set to false → T3 and `the_names_owner_check_refuses_a_folded_name_before_any_claim` (`tree.rs:6470`) must go red.
  - M7: in `enter_dir` pass `true` instead of `!pre_existing` → expected red by the `debug_assert!` panic in `resolve` on the first file of any pre-existing subdirectory with claims; this proves the call site is wired to `!pre_existing`, not that tracking is right.
  - M8: in `root_state` pass `true` instead of `fresh` → expected red the same way on the first file of a pre-existing root.
  - M9 (T1): change `publish: Publish::NoReplace,` at `tree.rs:2206` (the unbatched new-file path; `:2151` is inside `if batching`, which needs `files > 1` and Strict, `:1905-1909`, so T1 never runs it) to `Publish::Replace` → T1 must go red (a silent overwrite). If it stays green, find the `NoReplace` T1 really runs and say which.
  - M10 (T2): change `let publish = Publish::NoReplace;` at `tree.rs:2151` to `Publish::Replace` → T2 must go red: the refusal moves from the stage gate to `flush_batch` (`:3986`, `CopyStep::Publish`), so the `.step == CopyStep::Gate` assertion fails. (Mutating `:3986` alone leaves T2 green, because `sub/a` is refused at the gate before it is ever flushed.)
  The opposite wiring error (a FRESH directory built tracked) is red in no test: it is a memory regression, caught only by Task 3's measurement, and the flat fixture runs `root_state` only, so Task 3 ALSO measures a fixture with subdirectories (below) to exercise `enter_dir`. Task 1's `for_dir_decides_by_fresh...` pins the helper.

- [ ] **Step 4: Gate:** `just check` → exit 0.

- [ ] **Step 5: Commit** `git add crates/flux-core/src/tree.rs` / `git commit -m "feat: build a frame's name index by whether the directory is fresh"` (same trailer).

### Task 3: measure the result and record it (controller, top level; no subagent)

**Files:**
- Modify: `docs/phase3-measurements.md` (a new section after "Peak memory against directory size"); `TODO.md` ("Cut 22 open", the bounded-listing item: note cut A done, cut B open).
- No product code.

- [ ] **Step 1: Build the changed binary** with the same flags as the baseline: `RUSTFLAGS="-C force-frame-pointers=yes" CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_TARGET_DIR=$HOME/.cache/flux-fresh-target cargo build --release -p flux-cli`. The baseline binary is `$HOME/.cache/flux-fp-target/release/flux` (built from `main`'s code, whose `crates/` are unchanged by #146 and #147).
- [ ] **Step 2: Census.** With the fixtures `$HOME/.cache/flux-rss-flat-100000` and `-500000` (zero-byte, `make_tree.py flat`), for `n` in 100000, 500000 and run 1-3, alternating baseline and changed: `rm -rf $DST; sync; /usr/bin/time -v <flux> copy <fixture> $DST --durability normal`, record `Maximum resident set size` and the count of entries in `$DST`. Also `--dry-run` (stderr to a file) at BOTH 100000 and 500000, three runs each, for both binaries. Quote medians and the slopes `(median500k - median100k) * 1024 / 400000`, one for the full copy and one for `--dry-run`. If the fixtures or the baseline binary are missing, rebuild them (Step 1 command on `main` for the baseline; `make_tree.py flat <dir> --files <n>` for the fixtures). A SECOND fixture exercises `enter_dir`: `make_tree.py small <dir> --files <n> --bytes 0 --dirs 1` for `n` = 100000 and 500000 (zero-byte files all in ONE created subdirectory `d00`, so one `enter_dir` frame holds every entry at once; with `--dirs 100` each frame is dropped at `DirEnd`, `tree.rs:1360-1366`, and any leak is diluted 100-fold and invisible to the gate), the same full-copy census, three runs each, both binaries, and the slope between the two sizes.
- [ ] **Step 3: Acceptance, written before the data:** PASS if the changed binary's full-copy slope is at most 120 B/entry on the flat fixture AND at most 120 B/entry on the one-subdirectory fixture (the baseline's slope there is expected near 577, the changed binary's near 95) AND every run copied all `n` entries AND the flat `--dry-run` slope is within 5 B/entry of 76.1. The ablation predicted 95.4. A slope above 120 means the cost is not all in `record_publication`: report it as UNRESOLVED and do not claim the cut worked.
- [ ] **Step 4: Write the section and the TODO line; `just check` → exit 0; commit** (`docs: measure the fresh-directory name bookkeeping cut`). The TODO line says: cut A done; a directory that already existed (an empty destination, or `--resume`) still pays ~577 B/entry for its name index; cut B (the source walk listing) open.

---

## Plan self-audit (against the writing-plans checklist)

1. Spec coverage: the goal (bounded resident memory on the full-copy path for a created directory) maps to Tasks 1-3; the part of spec 505-515 about the source listing is cut B and is stated as out of scope here.
2. Placeholders: none. The only conditional is Task 2 Step 3's `COVERAGE_HOLE` stop, owned by the controller.
3. Type consistency: `for_created_dir`, `for_dir`, `is_tracked`, `tracked` are used with the same names in both tasks.
4. Edge cases covered: pre-existing directory and the dead no-claim state (Review Focus 1), dry-run planned frames (2), folded collision in a created directory, normal and strict batch (3, 5; tests T1, T2, T3), resume (4). `owner` or `resolve` on an untracked index is unreachable and enforced by `debug_assert!`, pinned by two `should_panic` tests.
5. Proportion: the plan is shorter than the diff it asks for is wide; no function bodies are given beyond the three-way match.

## Stand-downs

- The first Task 3 subdirectory fixture (`--dirs 100`) could not see an `enter_dir` leak (panel round 3): replaced by `--dirs 1`; T4 added as an optional unit pin.

- Further created-directory scenarios (symlinks and hardlink members, non-ASCII folds, multi-source folder roots; panel round 2, Q2): no extra tests. In a fresh directory the index is never read (`tree.rs:1926`, `:3065`), so no branch they take depends on it, and the `debug_assert!`s would fail a test that tried; the unchanged-behaviour claim for them rests on that and on the whole suite.

- `NameIndex` as an enum (`Untracked` / `Tracked`) instead of a `tracked` flag (panel round 1, Q2): not adopted. It would touch every field access in `names.rs`; the flag, the `debug_assert!`s in `resolve` and `owner`, and the single decision point `for_dir` give the same guarantee that an untracked index is never read, with a smaller diff. Revisit if a third state appears.
- Dropping the `pending` push for a new name in a TRACKED index (panel round 1, Resource Vampire): a different change, to the non-fresh path (resume or a pre-existing destination); not done here, recorded in the Goal as the unbounded remainder and in the Task 3 TODO line.
