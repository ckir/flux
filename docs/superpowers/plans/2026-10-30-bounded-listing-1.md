# Bounded directory listings, plan 1 of 2: measure, the streaming read, and the filter-only scans

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the three pieces of the Cut 23 design that rest on code that exists now: the baseline measurements, a streaming directory read (`read_dir_each` and `DirEntry.ino`) on the `flux-fs` traits and every implementation, and the four filter-only scans of a user directory (structure D) rewritten on top of it.

**Architecture:** One new default method on `FileSystem` and on `DirHandle` that visits entries instead of collecting them; the real implementations override it, the fakes inherit the default. Structure D then keeps only the bounded state it needs inside a visitor. Nothing in this plan changes visit order, a failure text, a `--json` object or an exit code.

**Tech Stack:** Rust (workspace, edition per `Cargo.toml`), `rustix` for the Unix listing, `windows-sys` for the Windows listing, `nextest`, `FaultFs` as the fake.

**Spec:** `docs/superpowers/specs/2026-10-30-bounded-listing-design.md` (approved by the owner 2026-10-30; panel round 6 CLEAN; amended 2026-10-30 while writing this plan: structure D gained three scans and the fold probe's second read became lazy, see section 4.1, 6.4 and N3 of the spec).

## What this plan does not cover, and why

The spec's walk spill-and-merge (A), the destination index above the threshold (B), the scratch space, `--scratch-dir`, `Thresholds`, `WalkError.fatal` and the gates GATE-A, B, C, X are NOT planned here. They consume `read_dir_each` and `DirEntry.ino` from Task 2 of this plan, and plan discipline forbids writing line-level steps against code that does not exist yet. **Plan 2** is written after Tasks 2 and 3 are committed, against the code as it then stands, and Task 1's numbers shape it. Also left to plan 2: `FaultFs` giving each entry an `ino` and a switch that makes it wrong (spec 6.6), which only the destination-index tests use; in this plan every fake returns `ino: None`. Both plans ship on the one branch `feat/bounded-listing` as one pull request (the owner chose one cut, 2026-10-30).

## Global Constraints

- The repository gate is `just check` (fmt, clippy `-D warnings`, typos, unused deps, doc-check, license, tests). Run it as it is; do not add flags.
- Tests run under nextest: `cargo nextest run --no-fail-fast`. Never read "N of M tests run" as a pass (fail-fast truncates a mutant run).
- No new `unsafe` outside `flux-platform`. `flux-core` has none.
- No new dependency in this plan.
- Every `.rs` file keeps its licence header.
- A comment states a reason, not a restatement; match the surrounding density.
- Visit order, failure texts, `--json` objects and exit codes do not change (spec N1, N2).
- Peak RSS is not a wall-clock figure. Any timing is measured by the controller at top level, idle, with the owner's go-ahead (Task 1 step 5 only).
- The Windows implementation (`dir_windows.rs`) cannot be compiled on the Linux dev box. It is checked by CI after a push; a push needs the owner's go-ahead.

## Review Focus

The spec is silent on these and a user would meet them:
1. A directory listing that fails half-way: `read_dir_each` must return the error and must not call the visitor again (a visitor that already ran has seen a prefix).
2. A visitor that returns `Break` on the first entry of a million-entry directory: the read must stop, not drain.
3. A name that is not one component, or `.` and `..`: the existing contract still holds in the streaming path (the walk enforces it, `walk.rs`; the streaming read must not weaken it).
4. A directory whose smallest eight names have case twins while many other names have none (the `B1..B8`, `b1..b8`, `a1..a8` fixture that broke the first single-pass idea, Task 3).
5. `scan_file` on a directory where thousands of unrelated names sit beside the records: the answer must equal today's and the visitor must keep only matches.

---

## Task 1: Baseline measurements (controller, top level; no product code)

**Files:**
- Modify: `docs/phase3-measurements.md` (new section "Cut 23 baseline")

**Interfaces:**
- Consumes: the release binary `~/.cache/flux-b-target/release/flux` (built from `main` at `c286819`, still on disk), the fixtures `~/.cache/flux-rss-flat-{100000,500000,1000000,2000000}`.
- Produces: the pre-change slopes that plan 2's gates compare against: structure B (copy into a pre-existing destination), `verify`, a single-file copy into a large directory, and the store micro-benchmark result.

This task is run by the controller, not by a subagent: it measures, and only the top level can guarantee an idle machine.

- [ ] **Step 1: Verify the state.** Run `git rev-parse --short HEAD` (expect the branch `feat/bounded-listing`, parent `c286819`) and `ls ~/.cache/flux-b-target/release/flux ~/.cache | grep rss-flat`. If the binary or a fixture is missing, STOP and report `STATE_MISMATCH`; rebuild with `CARGO_TARGET_DIR=$HOME/.cache/flux-b-target cargo build --release -p flux-cli` and regenerate fixtures with `python3 benches/phase3/make_tree.py flat --files N ~/.cache/flux-rss-flat-N`.

- [ ] **Step 2: Field check.** `ps -eo pid,pcpu,comm --sort=-pcpu | head -8`; kill any stale measurement process. State that background load is uncontrolled. Prove the probe sees load: run `yes > /dev/null &` for two seconds, confirm it shows above 90 percent, kill it.

- [ ] **Step 3: Structure B baseline (peak RSS).** For N in 500000, 1000000, 2000000, two runs each, destination `~/.cache/rss-dst`:
  `rm -rf ~/.cache/rss-dst; flux copy SRC ~/.cache/rss-dst >/dev/null 2>&1` (first copy, unmeasured), then
  `/usr/bin/time -v flux copy SRC ~/.cache/rss-dst >/dev/null 2>/tmp/rss.err` (the SECOND copy, onto the now pre-existing directory; default policy `--overwrite`), and read `Maximum resident set size (kbytes)` from the time output. Repeat with `--resume` instead of overwrite after a first copy. Record KiB and the marginal slope 1M to 2M in bytes per entry.

- [ ] **Step 4: verify and single-file baselines (peak RSS).** `flux verify SRC DST` over the identical 1M and 2M trees (both sides the same fixture), and `flux copy SRC/f000000 ~/.cache/rss-dst2/` (a single-file copy into a destination directory that already holds N entries: create it by copying the fixture there first). Two runs each; record KiB and slope.

- [ ] **Step 5: Store micro-benchmark (OWNER GO-AHEAD REQUIRED; it is a timing).** Ask the owner for the go-ahead; without it, record "not measured" and stop at this step. With it: write a throwaway program OUTSIDE the repository (`/tmp/claude-1001/.../scratchpad/store-bench/`, its own `Cargo.toml` depending on `redb = "=4.4.0"`, the version in `Cargo.lock`), go idle, and measure at 10,000,000 and 100,000,000 names (24-byte random names, a 25-byte fixed key prefix as spec 6.3 states): ingest rate in names per second with a 16 MiB cache and peak RSS, and the median lookup time over 100,000 random lookups. The pre-registered criterion is spec 6.3: ingest at least 100,000 names per second at both sizes, peak RSS bounded, lookup median under 1 ms. Two runs each; quote the range. If the criterion is missed, STOP the plan and return to the owner with the numbers.

- [ ] **Step 6: Record.** Add the section "Cut 23 baseline" to `docs/phase3-measurements.md`: method, binary and commit, fixtures, the raw KiB, the slopes, the micro-benchmark result or "not measured", and what was not controlled. Run `typos docs/phase3-measurements.md`. Commit: `git add docs/phase3-measurements.md && git commit -m "docs: baseline slopes for the bounded-listing cut"` with the `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>` trailer.

---

## Task 2: A streaming directory read and the listing's own inode number

**Files:**
- Modify: `crates/flux-fs/src/fs.rs` (`DirEntry`, `FileSystem::read_dir_each`, `DirHandle::read_dir_each`, the test at `fs.rs:703-710`)
- Modify: `crates/flux-platform/src/dir_unix.rs` (`read_dir` at `dir_unix.rs:515-548`)
- Modify: `crates/flux-platform/src/dir_windows.rs` (`read_dir` at `dir_windows.rs:489`, `read_dir_at` at `dir_windows.rs:550-612`)
- Modify: `crates/flux-platform/src/std_fs.rs` (`FileSystem::read_dir` at `std_fs.rs:662-682`)
- Modify, only as the compiler demands, to add `ino: None` to a `DirEntry` literal: `crates/flux-core/src/fault_fs.rs:1821`, `crates/flux-core/src/walk.rs:1203`, `crates/flux-core/src/run/tests.rs:8378`
- Test: `crates/flux-platform/tests/dir_control.rs`, `crates/flux-platform/tests/std_fs.rs`, `crates/flux-core/src/fault_fs.rs` (tests module near `fault_fs.rs:4532`), `crates/flux-fs/src/fs.rs` (tests module)

**Interfaces:**
- Consumes: nothing from an earlier task.
- Produces, used by Task 3 and by plan 2:
  - `pub struct DirEntry { pub name: OsString, pub file_type: FileType, pub ino: Option<u64> }` (derives unchanged: `Debug, Clone, PartialEq, Eq`).
  - `FileSystem::read_dir_each(&self, path: &Path, visit: &mut dyn FnMut(DirEntry) -> Result<ControlFlow<()>>) -> Result<()>` with a default body.
  - `DirHandle::read_dir_each(&self, visit: &mut dyn FnMut(DirEntry) -> Result<ControlFlow<()>>) -> Result<()>` with a default body.
  - `ControlFlow` is `std::ops::ControlFlow`.

Shape of the contract (write it in the doc comments): entries arrive in no particular order; each `DirEntry::name` is exactly ONE bare component, never `.` or `..`, as `read_dir` already promises; the first `Err` from the visitor is returned and no further entry is visited; `Break` stops the read and returns `Ok(())`; a failure of the listing itself is returned and the visitor is never called after it. `DirEntry::ino` is the object number the listing itself carries (Unix `d_ino`), obtained without a `stat`; it is `None` where the platform's listing has none (Windows) and for the fakes. It is a HINT: it can differ from `ObjectId::index` (a mount-point entry, overlayfs, a btrfs subvolume root, NFS) and must be verified with `metadata` before it identifies anything.

Tests are written first and fail first. The implementer receives them as already written ("implement until they pass"); do NOT rewrite a test to match the code. The oracle for every expectation below is the spec section 6.6 and the existing `read_dir` behaviour pinned by `dir_control.rs` and `fault_fs.rs`.

- [ ] **Step 0: Verify the state.** Open each file above and confirm: `DirEntry` has exactly `name` and `file_type` (`fs.rs:47-50`); `FileSystem::read_dir` is at `fs.rs:217` and `DirHandle::read_dir` at `fs.rs:469`; the seven `DirEntry {` literals are at `fault_fs.rs:1821`, `walk.rs:1203`, `run/tests.rs:8378`, `fs.rs:705`, `std_fs.rs:670`, `dir_unix.rs:545`, `dir_windows.rs:600`; `rustix::fs::DirEntry::ino()` exists (`rustix-1.1.5/src/backend/linux_raw/fs/dir.rs:351`). If any differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: Write the failing tests.**

  In `crates/flux-core/src/fault_fs.rs`, inside the tests module that holds `read_dir_returns_stored_spellings`, add (names are exact; `FaultFs`, `FileSystem`, `DestinationRoot`, `DirHandle` are imported in that module or via a local `use`):
  - `read_dir_each_visits_every_entry_once_like_read_dir`: build `/d` with files `a`, `b`, a directory `c`; collect the visited `(name, file_type)` pairs sorted, and assert they equal the sorted `read_dir` result, and that every visited `ino` is `None`.
  - `read_dir_each_stops_when_the_visitor_breaks`: five files; the visitor counts calls and returns `Break` on the first; assert the call count is 1 and the result is `Ok(())`.
  - `read_dir_each_returns_the_visitors_error_and_stops`: five files; the visitor returns `Err(FsError::new(Code::IoError, std::io::Error::other("x")))` on the second call; assert the result is that error (`err.code == Code::IoError`, `code` being a public field of `FsError` (`flux-fs/src/error.rs:114-116`), and the message contains `x`) and the call count is 2.
  - `read_dir_each_records_exactly_one_read_dir_call`: after one `read_dir_each` over `/d`, assert `fs.calls().iter().filter(|c| c.starts_with("read_dir(")).count() == 1` (the streaming read must not list twice).
  - `a_failed_listing_never_calls_the_visitor`: `fs.fail_nth("read_dir", 1, Code::IoError, ErrorKind::PermissionDenied)` (the helper the multi-source tests use); assert the result is `Err` and the visitor call count is 0.
  - The same five behaviours through the handle: one test, `read_dir_each_on_a_handle_matches_read_dir`, with `fs.destination_root(Path::new("/d"))` and `DirHandle::read_dir_each`.

  In `crates/flux-platform/tests/dir_control.rs` add:
  - `read_dir_each_yields_exactly_the_entries_that_exist`: in `dir()`'s scratch directory create the file `a`, the subdirectory `sub` and the symlink `link`; assert the sorted `(name, file_type)` of `read_dir_each` equals the literal `[("a", File), ("link", Symlink), ("sub", Dir)]` and its length is 3. It must NOT be compared with `read_dir`/`listing(&d)`: `read_dir` collects through `read_dir_each`, so a bug in the shared loop would hide.
  - `read_dir_each_stops_on_break_and_returns_a_visitor_error`: three files; `Break` on the first gives one call and `Ok`; an `Err` on the second gives that error and two calls.
  - `#[cfg(unix)] read_dir_ino_is_the_object_index_of_the_entry` (add `use flux_fs::{FileIdentity, ObjectId};` to the file): for every entry, `entry.ino == Some(index as u64)` where `index` comes from `d.metadata(&entry.name)` as `FileIdentity::Strong(ObjectId { index, .. })` (the scratch directory is a plain tempdir, so `d_ino` equals `st_ino` there). Assert the directory has at least three entries so the loop is not vacuous.

  In `crates/flux-platform/tests/std_fs.rs` add the same pair at the `FileSystem` level (`StdFileSystem.read_dir_each(path, ..)`): `std_read_dir_each_yields_exactly_the_entries_that_exist` (the same literal expectation, through `StdFileSystem.read_dir_each(path, ..)`), and `#[cfg(unix)] std_read_dir_ino_is_the_object_index_of_the_entry`.

  In `crates/flux-fs/src/fs.rs` tests, change `a_dir_entry_carries_a_name_and_a_type` (`fs.rs:703`) to build `DirEntry { name, file_type, ino: None }` and add `assert_eq!(e.ino, None)`.

- [ ] **Step 2: Run them to verify they fail.** `cargo nextest run -p flux-fs -p flux-platform -p flux-core --no-fail-fast read_dir_each read_dir_ino a_dir_entry_carries` Expected: the build fails (no `ino`, no `read_dir_each`). That is the red state.

- [ ] **Step 3: Add the field and the two trait methods in `crates/flux-fs/src/fs.rs`.**
  - `DirEntry` gains `pub ino: Option<u64>` after `file_type`, with the doc comment from the contract above.
  - `FileSystem` gains `read_dir_each` with this default body (the doc comment carries the contract): `for entry in self.read_dir(path)? { if visit(entry)?.is_break() { break; } } Ok(())`. `DirHandle` gains the same method without the path, calling `self.read_dir()`. Add `use std::ops::ControlFlow;` at the top of the file.
  - Add `ino: None` to every literal the compiler names: the seven listed in step 0 (the real ones are rewritten in steps 4 to 6).

- [ ] **Step 4: Override on Unix, `dir_unix.rs`.** Rewrite `fn read_dir` so it collects through a new `fn read_dir_each` (same loop as today: skip `.` and `..`, the `statat` fallback for `FileType::Unknown`, the same type mapping) that builds `flux_fs::DirEntry { name, file_type, ino: Some(entry.ino()) }` and returns `Ok(())` when `visit(..)?` breaks. `read_dir` is `let mut out = Vec::new(); self.read_dir_each(&mut |e| { out.push(e); Ok(ControlFlow::Continue(())) })?; Ok(out)`. Keep the existing comment about `read_from` opening a descriptor of its own.

- [ ] **Step 5: Override in `std_fs.rs`.** `FileSystem::read_dir_each` over `std::fs::read_dir(path)` with the same type mapping as `read_dir` (`std_fs.rs:662-682`), `ino: Some(std::os::unix::fs::DirEntryExt::ino(&entry))` under `#[cfg(unix)]` and `None` otherwise; `read_dir` collects through it exactly as in step 4.

- [ ] **Step 6: Override on Windows, `dir_windows.rs`.** Rename `read_dir_at` to `read_dir_each_at(p: &OwnedHandle, visit: &mut dyn FnMut(flux_fs::DirEntry) -> Result<ControlFlow<()>>) -> Result<()>`: the same buffer loop, `visit(DirEntry { name, file_type: entry_type(attributes, tag), ino: None })?` per record (skipping `.` and `..` as today), returning `Ok(())` on `Break`. In `impl DirHandle for StdDir` (`dir_windows.rs:489`) add `fn read_dir_each(&self, visit: &mut dyn FnMut(flux_fs::DirEntry) -> Result<ControlFlow<()>>) -> Result<()> { read_dir_each_at(&self.0, visit) }` (without this override Windows would inherit the default, which collects, and the streaming read would be a no-op there), and make `read_dir` collect through it. This file cannot be built on the dev box: keep the change mechanical and let CI be the check.

- [ ] **Step 7: Run the tests.** `cargo nextest run -p flux-fs -p flux-platform -p flux-core --no-fail-fast` Expected: `Summary ... N tests run: N passed, 0 skipped` with no failure, including every new test of step 1.

- [ ] **Step 8: Non-vacuity (logic mutants, one at a time, restore each).** Each must turn the named test red, and no other new test red for an unrelated reason:
  - the default `read_dir_each` ignores `Break` (drops the `is_break` check): `read_dir_each_stops_when_the_visitor_breaks` red.
  - the default swallows the visitor's error (`let _ = visit(entry);`): `read_dir_each_returns_the_visitors_error_and_stops` red.
  - `dir_unix.rs` sets `ino: None`: `read_dir_ino_is_the_object_index_of_the_entry` red.
  - `dir_unix.rs` `read_dir_each` skips the first entry: `read_dir_each_yields_exactly_the_entries_that_exist` red (the literal expectation, not a comparison with `read_dir`, is what makes this mutant visible).
  Report the red test names. Use `--no-fail-fast`.

- [ ] **Step 9: Gate and commit.** `just check` Expected exit 0. Then `git add -A crates && git commit -m "feat: stream a directory listing and carry the listing's inode number"` with the trailer.

---

## Task 3: The filter-only scans of a user directory (structure D)

**Files:**
- Modify: `crates/flux-core/src/run/multi.rs` (`folds_ascii_case` at `multi.rs:85-113`, `dry_create_preview`-time listing at `multi.rs:249` and `multi.rs:331-336`, `dry_fold` at `multi.rs:369-390`)
- Modify: `crates/flux-core/src/run/dry_prior.rs` (`broken_leftovers` at `dry_prior.rs:113-127`, `dry_lock` at `dry_prior.rs:131-143`)
- Modify: `crates/flux-core/src/prior.rs` (`scan_file` at `prior.rs:120-139`)
- Modify: `crates/flux-core/src/cleanup/discover.rs` (the lock-holder scan at `discover.rs:325-345`)
- Test: `crates/flux-core/src/run/multi.rs` (tests module), `crates/flux-core/src/run/tests.rs`, `crates/flux-core/src/prior.rs` tests, `crates/flux-core/src/cleanup/discover.rs` tests

**Interfaces:**
- Consumes: `DirHandle::read_dir_each` and `ControlFlow` from Task 2.
- Produces: nothing a later task needs by name (plan 2 does not call these).

Oracle: every existing test in `flux-core` passes UNCHANGED, in particular `the_fold_probe_decides_from_the_listing_of_a_directory` (`multi.rs:4222`), whose `parent_listings(&log)` counts pin how many times a directory is listed, and `broken_leftovers_notes_exactly_the_entries_of_one_lock` (`run/tests.rs:8376`). If one of them needs editing to pass, STOP and report `[original] -> [yours] because ...`; the oracle wins.

- [ ] **Step 0: Verify the state.** Open the four files and confirm the cited code: `folds_ascii_case(dir, entries: &[DirEntry])` builds a `HashSet` of every name at `multi.rs:94`; `dry_create_preview` calls `dest.read_dir().ok()` at `multi.rs:249`, uses it at `multi.rs:283` (the fold probe) and `multi.rs:331-336` (`broken_leftovers` per root); `dry_fold(&self, listing: Option<&[DirEntry]>)` is at `multi.rs:369`; `dry_lock` lists `site.dir().read_dir()` at `dry_prior.rs:139`; `scan_file` collects `parent.read_dir()?.iter().filter_map(id_after ...)` at `prior.rs:127-131`; `discover.rs:325` lists `located.holder.read_dir()`, sorts it and keeps `FileType::File` names with the `<lock>.broken.` prefix. If any differs, STOP: `STATE_MISMATCH`.

- [ ] **Step 1: Write the failing tests.**
  - In `multi.rs` tests, the oracle. Keep the old function verbatim under `#[cfg(test)] fn folds_ascii_case_of_slice<D: DirHandle>(dir: &D, entries: &[DirEntry]) -> Option<bool>` (body from `multi.rs:85-113`). Then three tests, each comparing the FULL `Option<bool>` of `folds_ascii_case(&dir, &scan_dir(&dir, &[]).unwrap())` with `folds_ascii_case_of_slice(&dir, &dir.read_dir().unwrap())` on the same `FaultFs` directory:
    - `the_lazy_fold_scan_agrees_with_the_slice_oracle_exhaustively`: the universe `["a","A","b","B","ab","Ab","1","_","c","C"]` (ten names); EVERY subset (1,024) crossed with folding off and on (entries created with folding off as `case_dest` does at `multi.rs:4054-4062`, then `fs.set_case_insensitive(true)`), and with three identity modes (every entry `Strong` as the fake gives by default, every entry `Unavailable`, every entry `Weak`, set with `fs.set_identity(path, ..)` as `multi.rs:4259` and `multi.rs:3309` do): 6,144 directory states. The old and the new function read the SAME `FaultFs` state, so no state needs skipping, however odd. Assert the count compared is exactly 6,144.
    - `the_lazy_fold_scan_agrees_with_the_slice_oracle_on_larger_directories`: 3,000 directories from a fixed-seed linear congruential generator (no new dependency; the seed is in the failure message), each 0 to 40 names built from families `a`, `b`, `c`, `d` crossed with suffixes `""`, `"1"`, `"2"`, `"x"` and both cases of the first letter, folding off and on, identity mode drawn per directory. This test covers sizes above eight candidates; the exhaustive one covers the branches.
    - `a_directory_whose_small_names_have_no_twin_still_finds_the_twins_of_the_candidates`: the fixture that broke the single-pass idea: `B1..B8`, `b1..b8`, `a1..a8` with folding off; the oracle says the candidates `B1..B8` each have their twin present (`Some(false)`), and the scan must say the same.
  - `the_fold_scan_keeps_a_bounded_state`: 10,000 names in one directory; assert `scan.candidates.len() <= FOLD_CANDIDATES` (a `#[cfg(test)]` accessor is not needed: the field is read in the module's own tests).
  - `a_directory_with_no_case_twin_is_read_once`: a case-sensitive directory whose smallest candidate has no twin; `scan_dir` then `folds_ascii_case` produce exactly one `read_dir(` call in `fs.calls()`.
  - `a_directory_that_folds_is_read_twice_and_no_more`: a folding directory; exactly two `read_dir(` calls (read 1 and the lazy read 2), and the answer is `Some(true)`.
  - `leftovers_are_kept_per_root_in_stream_order`: three roots `a`, `b`, `c` and a directory holding `a.flux-lock.broken.2`, `b.flux-lock.broken.1`, `a.flux-lock.broken.1`, `c.flux-lock`, and 5,000 unrelated names; assert the scan's per-root leftovers equal what `broken_leftovers` over the slice reports for each root, in the same order, and that the scan kept only the 3 matches.
  - In `run/tests.rs`: `broken_leftovers_in_agrees_with_the_slice_version`: for a `FaultFs` directory holding the four names of the existing test `broken_leftovers_notes_exactly_the_entries_of_one_lock` plus 1,000 unrelated names, the warnings from the streaming `broken_leftovers_in(&dir, OsStr::new("a.flux-lock"), &mut w)` equal those of the slice `broken_leftovers`.
  - In `prior.rs` tests: `scan_file_keeps_only_the_records_among_many_names`: 5,000 unrelated names and two record names beside a target; assert the `Scan` equals the one a directory with only the two record names gives, and exactly one `read_dir(` call is logged.
  - In `cleanup/discover.rs` tests: `lock_discovery_keeps_only_the_broken_files_of_the_lock`: a holder with 5,000 unrelated names, two `<lock>.broken.<id>` files and one directory with that prefix; assert the same entries a holder with only those names gives (the existing helper the discovery tests use), one `read_dir(` call.

- [ ] **Step 2: Run them to verify they fail.** `cargo nextest run -p flux-core --no-fail-fast a_streamed_fold_scan the_fold_scan leftovers_are_kept broken_leftovers_in scan_file_keeps lock_discovery_keeps` Expected: build failure (no `scan_dir`, no `broken_leftovers_in`) or red.

- [ ] **Step 3: The fold scan, `multi.rs`.** Spec 6.4 is the design; read it first (the lazy second read, and why a single pass is impossible).
  Add, next to `folds_ascii_case`:
  - `struct DirScan { candidates: Vec<String>, leftovers: Vec<Vec<OsString>> }`: `candidates` are the up-to-`FOLD_CANDIDATES` smallest usable names in ascending order; `leftovers[i]` holds, in stream order, the names that start with `broken_prefix(lock_names[i])`, the helper of step 5 (`multi.rs:331-334` builds the lock name by pushing `.flux-lock` onto the root name, and the prefix adds `.broken.`; one helper builds it for both callers).
  - `fn scan_dir<D: DirHandle>(dir: &D, lock_names: &[OsString]) -> Option<DirScan>` (best-effort: a listing error gives `None`, as `dest.read_dir().ok()` did). One `dir.read_dir_each` pass keeps the bounded max-heap of the 8 smallest usable names (usable means `fold_candidate`: valid UTF-8 with an ASCII letter) and the per-root leftover matches.
  - Rewrite `folds_ascii_case(dir: &D, scan: &DirScan) -> Option<bool>`: the same candidate loop as today (`multi.rs:95-111`) with the listed-variant check made lazy as spec 6.4 describes: for each candidate compute the metadata outcome first; return `Some(false)` when the variant is `NotFound`; otherwise consult a lazily-filled, cached `listed: Option<HashSet<OsString>>` built by ONE more `dir.read_dir_each` that collects the names among the (at most eight) variants of `scan.candidates` and returns `Break` once every variant has been FOUND (an absent variant cannot be known absent before the stream ends, so the read runs to the end unless all are found); an `Err` from this second read makes `folds_ascii_case` return `None` (undecided, the best-effort answer; never fall back to an empty set, which could turn a coexisting pair into a false `Some(true)`); if the variant is listed return `Some(false)`, else the metadata outcome.
  - Keep the old slice function as `#[cfg(test)] folds_ascii_case_of_slice` (the oracle of step 1), body verbatim from `multi.rs:85-113`.
  - `dry_fold(&self, listing: Option<&DirScan>)`: DEST's scan is used when `scan.candidates` is non-empty (today: `entries.iter().any(|e| fold_candidate(e).is_some())`); the two holder reads (`multi.rs:372`, `multi.rs:383`) become `let scan = scan_dir(holder, &[])?; folds_ascii_case(holder, &scan)`, keeping the `?`-on-`None` shape of today's `holder.read_dir().ok()?`.
  - `dry_create_preview` (`multi.rs:249`): `let listing = scan_dir(dest, &lock_names)` where `lock_names` are the roots' names with `.flux-lock` appended; the leftover warnings (`multi.rs:331-336`) are produced per root from `scan.leftovers[i]` with `RunWarning::DryRunBrokenLeftover { name: name.to_string_lossy().into_owned() }`, in root order then stream order, exactly as `broken_leftovers` produced them.
  - The existing fold-probe tests pin `parent_listings(&log)`. In the cases that now read a directory twice (a folding directory, or one holding both spellings, spec 6.4 "Cost") the pinned count rises by exactly one. For each failing pin: if the ONLY difference is that +1 in such a case, update the expected number and add the comment `// the lazy second read of the fold scan (spec 6.4)`; list every pin you changed in the report. Any other failure, or a difference in a case with no twin (which must still be one read), is a STOP: `[original] -> [yours] because ...`.

- [ ] **Step 4: Run the oracle tests first.** `cargo nextest run -p flux-core --no-fail-fast the_lazy_fold_scan a_directory_whose_small_names a_directory_with_no_case_twin a_directory_that_folds the_fold_scan_keeps` Expected: all pass; the exhaustive test's failure message format includes the number of directories compared (assert it is above 1,000). If the oracle disagrees anywhere, do NOT change the oracle: the old function is the definition of correct; report the smallest failing directory.

- [ ] **Step 5: `dry_prior.rs`.** Add `pub(crate) fn broken_prefix(lock_name: &OsStr) -> Vec<u8>` (the two lines at `dry_prior.rs:118-119`), have `broken_leftovers` use it, and add `pub(crate) fn broken_leftovers_in<D: DirHandle>(dir: &D, lock_name: &OsStr, warnings: &mut Vec<RunWarning>)`: one `dir.read_dir_each` pass that pushes `RunWarning::DryRunBrokenLeftover { name }` for each matching name in stream order, ignoring a listing error (best-effort, as `dry_lock`'s comment says). `dry_lock` (`dry_prior.rs:139-141`) calls it instead of `read_dir` then `broken_leftovers`. `broken_leftovers(&[DirEntry], ..)` stays (the existing test uses it; it is also the oracle of step 1).

- [ ] **Step 6: `prior.rs` `scan_file`.** Replace the `parent.read_dir()?.iter().filter_map(|e| id_after(&e.name, RECORD_INFIX)).filter(|id| *id != own_id).map(str::to_string).collect()` with a `read_dir_each` visitor that pushes the same ids into the same `Vec<String>`; keep the `ids.sort(); ids.dedup();` that follow. The `?` on the listing keeps converting an `FsError` into the same `LockResult` error.

- [ ] **Step 7: `cleanup/discover.rs`.** Replace `located.holder.read_dir()` + `listing.sort_by(..)` + the `FileType::File` and prefix filter (`discover.rs:325-336`) with a `read_dir_each` visitor that keeps the matching names, then sort the matches by name and continue into the existing `probe_lock` loop unchanged. A listing error stays `listing_failed = listing_failed.or(Some((located.holder_shown.clone(), e)))`.

- [ ] **Step 8: Run the tests.** `cargo nextest run -p flux-core --no-fail-fast` Expected: all pass, the new tests of step 1 included, and every pre-existing test unchanged.

- [ ] **Step 9: Non-vacuity.** One mutant at a time; the named new test must go red:
  - `folds_ascii_case` never consults the lazy listed set (skips read 2): the exhaustive oracle test red (a directory holding both `Ab` and `ab` gives `None` instead of `Some(false)`), and `a_directory_whose_small_names_have_no_twin_still_finds_the_twins_of_the_candidates` red.
  - `folds_ascii_case` consults the listed set BEFORE the metadata outcome, always (the eager order): `a_directory_with_no_case_twin_is_read_once` red.
  - `scan_dir` keeps every name in `candidates` (drop the bounded pop): `the_fold_scan_keeps_a_bounded_state` red.
  - the lazy read 2 does not `Break` and does not cache (re-reads per candidate): `a_directory_that_folds_is_read_twice_and_no_more` red.
  - `broken_leftovers_in` matches on `starts_with(lock_name)` without `.broken.`: `broken_leftovers_in_agrees_with_the_slice_version` red.
  - `scan_file` drops the `!= own_id` filter: the existing `scan_file` tests red (a pre-existing test) AND `scan_file_keeps_only_the_records_among_many_names` when the fixture contains the own id.
  - `discover.rs` skips the sort: the discovery test that pins order red.
  Run each with `--no-fail-fast`, report the red names, restore the file byte-identical.

- [ ] **Step 10: Gate and commit.** `just check` Expected exit 0. `git add -A crates && git commit -m "feat: scan a user directory for names without listing it whole"` with the trailer.

---

## Self-audit (done by the plan's author)

- Spec coverage: structure D and G3 (Task 3), the streaming read and `ino` of 6.6 (Task 2), the baselines of section 7 and spec 9 R1 (Task 1). Not covered, deliberately: A, B, scratch, `--scratch-dir`, `Thresholds`, fatal errors, GATE-A/B/C/X (plan 2).
- Placeholders: none. Values I chose are the test seed (fixed), the 3,000 cases, the 5,000 and 10,000 name fixtures; the plan author may be told to change them.
- Shape risk: `DirEntry` gains a field and is `pub`: every literal in the workspace changes; the compiler is the oracle for the list (step 0 lists seven; a repo-wide search is the check, excluding signatures).
- The one subtle piece, the lazy listed-variant check, is equivalent to today's order by the case analysis in spec 6.4 and is checked against the old implementation exhaustively over a ten-name universe (6,144 directory states: 1,024 subsets, folding off and on, three identity modes), on 3,000 larger random directories, and on the fixture that broke the earlier single-pass design.
- Cannot be verified here: the Windows build of `dir_windows.rs` (CI after a push, with the owner's go-ahead).
