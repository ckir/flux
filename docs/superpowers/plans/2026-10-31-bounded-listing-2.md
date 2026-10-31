# Bounded directory listings, plan 2 of 3: the scratch space and the walk's spill and merge (structure A)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `walk` holds a bounded number of entries of any directory in memory and still yields them in byte order, with its spill in a scratch file that no failure leaves behind, a heartbeat that keeps beating through a long listing, and a fatal error path that stops the operation instead of skipping a subtree.

**Architecture:** A `ScratchSpace` in `flux-core` owns one unlinked run file, created on first spill through a new `DirHandle::create_scratch` that `flux-platform` implements. `list_sorted` replaces both `sorted(read_dir(..))` calls in `walk.rs`: a directory of at most `inline_entries` is sorted in memory as today, a larger one is written as sorted runs into a stack-allocated region of the run file and read back through a k-way merge. The copy's source root is listed when the copy starts (inside `copy_tree_frames`), not when its walk is built, so a run's spill lands in its own workspace.

**Tech Stack:** Rust 2024 (`rust-version = "1.98.1"`), `std` positional file I/O (`std::os::unix::fs::FileExt`, `std::os::windows::fs::FileExt`), `rustix` and `windows-sys` inside `flux-platform` only, `nextest`, `proptest` (already a `flux-core` dev-dependency), `FaultFs`.

**Spec:** `docs/superpowers/specs/2026-10-30-bounded-listing-design.md`, sections 3 (C1-C7), 6.1, 6.2, 6.5, 6.6, 7 (GATE-A, GATE-C, GATE-X), 8. Plan 1 (`docs/superpowers/plans/2026-10-30-bounded-listing-1.md`) is done: `read_dir_each` and `DirEntry.ino` exist (`crates/flux-fs/src/fs.rs:48-57`, `:232`, `:502`).

## What this plan does not cover, and why

Structure B (the hybrid destination index, spec 6.3), `MAX_CANDIDATES`, `INDEX_*`, `PUBLICATION_COMMIT`, `FaultFs` giving each entry an `ino` with a switch that makes it wrong (spec 6.6), GATE-B and the 6.3 measurement task are **plan 3**. Plan 3 reuses this plan's run record, sort and merge engine (Task 3) and `ScratchSpace` (Task 2); plan discipline forbids writing its line-level steps before that code exists. `Thresholds` therefore carries only the five fields this plan reads; plan 3 adds its own (an unread field fails `clippy -D warnings`). All three plans ship on `feat/bounded-listing` as one pull request.

`flux cleanup` sweeps partials with the same walk (`crates/flux-core/src/cleanup/delete.rs:570`) but has no `--scratch-dir` and no spec section: it passes `scratch: None` in this plan (a walk with no scratch never spills, so its memory stays as today; this is NOT an unplaced space, which refuses to spill) and Task 8 records it in `TODO.md` as a known limit.

## Spec deviations taken by this plan (Task 8 amends the spec and ledgers each)

1. **The tick is an argument of `Walk::next_with`, not a `WalkOptions` field** (spec 6.2 "Liveness", 6.6). The copy builds its walk in `prepare_source` (`crates/flux-core/src/tree.rs:483`) long before the beat closure exists (`crates/flux-core/src/run/mod.rs:552-558`), so a `&'a dyn Fn` stored in `Walk<'a, F>` cannot borrow it. `Iterator::next` calls `next_with` with a tick that always succeeds.
2. **The copy's source root is listed when the copy starts, not when `walk_with` is called** (spec 6.1 C4 against the B1 order). `tree_with` builds the walk before the lock (`run/mod.rs:327`, comment "B1"), and a root above `inline_entries` would spill before the workspace the spec names exists. `prepare_source` builds an UNSTARTED walk (metadata and type checks only, so `a_mistyped_source_creates_nothing`, `crates/flux-core/src/run/tests.rs:833-839`, still holds); `copy_tree_frames` (`tree.rs:1097`, the one funnel of every copy variant: `tree.rs:769`, `:947`, `run/mod.rs:1530`) starts it. Consequence the owner must accept: a source root that exists but cannot be LISTED now fails after the lock (state FAILED, `CopyStep::Source`, same exit code), and with several sources an unlistable later root is met after the earlier roots were copied (today `multi.rs:852` lists every root before the lock; the comment at `multi.rs:3586-3587` becomes false for a live run and is updated). Consulted with agy (`.clavity/seams/bounded-listing-plan2-scratch-order.md`, `[VERDICT: ALIGNED - O1]`). `walk`, `walk_with` and `walk_with_depth` stay eager, so `walk_refuses_a_root_whose_read_dir_fails` (`walk.rs:1384-1396`), `verify` and the sweep keep today's root-failure shape.
3. **Scratch files are created through `DirHandle::create_scratch`** (new trait method) rather than by path in `flux-core` (spec 6.1 "create, open, and unlink at once"). A path-built file under DEST would bypass the handle-relative discipline of section 149.7 that every other DEST write follows (`crates/flux-platform/src/dir_unix.rs:165-176`).
4. **The allocation assertion of spec 6.5 counts peak LIVE bytes, not allocations.** Every yielded `DirEntry` owns an `OsString` (`fs.rs:49`), so allocations per entry cannot be constant; peak live bytes can, and that is what a leak would raise.
5. **Names corrected:** spec 6.2 calls the verify side `MergedSide::advance` and the loop `verify_trees`; the code has `Side::advance` (`crates/flux-core/src/verify.rs:276`) and `walk_checked` (`verify.rs:597`). Spec 6.2 cites `run/multi.rs:955` for the dry restart preview; it is `multi.rs:1062`.

## Global Constraints

- The repository gate is `just check`. Run it as it is; do not add flags.
- Tests: `cargo nextest run --workspace --no-fail-fast` (never read "N of M tests run" as a pass).
- No `unsafe` in `flux-core` (C7). Platform calls live in `flux-platform`.
- No new dependency. `tempfile` is already a `flux-core` dev-dependency (`crates/flux-core/Cargo.toml`).
- Every new `.rs` file starts with the licence header of `crates/flux-core/src/walk.rs:1-5`.
- Visit order, `--json` objects, failure texts and exit codes do not change (spec N1, N2). The new failure lines are a scratch error (exit 1) and the `tmpfs` refusal (exit 1).
- Constants (spec 6.2): `inline_entries = 8_192`, `run_entries = 262_144`, `merge_fan_in = 384`, `merge_buffer = 16_384` bytes, `tick_entries = 65_536`.
- Run record (spec 6.2 item 7): `u16` name length (little-endian), `u8` file type (`0` File, `1` Dir, `2` Symlink, `3` Other), then the bytes of `name.as_encoded_bytes()`. A name longer than `u16::MAX` bytes is a fatal scratch error, never a panic.
- Comparison: an 8-byte big-endian prefix of the name bytes (zero-padded) first, then the full bytes (spec 6.2). The result must equal `a.as_encoded_bytes().cmp(b.as_encoded_bytes())` for every pair.
- Peak RSS is not a timing. No timing is measured by this plan.
- `dir_windows.rs` cannot be built on the dev box: check it with `cargo check --target x86_64-pc-windows-gnu -p flux-fs -p flux-platform -p flux-core --tests` (worked for plan 1, `.superpowers/sdd/2026-10-30-bounded-listing-1/progress.md`); CI after a push is the real test, and a push needs the owner's go-ahead.

## Review Focus

1. A walk that fails or is dropped mid-directory (an injected scratch write error, a failed tick, a consumer that stops iterating) leaves the run file at length zero once the walk is dropped.
2. A child directory that spills while its parent's merge reader is open: the parent's remaining entries are unchanged after the child's region is truncated (LIFO regions, positional reads).
3. A name of exactly `u16::MAX` bytes, a Unix name with bytes `0x80..0xFF`, and two names equal in their first 8 bytes: order equals `sorted()`.
4. A heartbeat that fails during a 10-run merge: the copy stops with `CopyStep::Heartbeat`, not one skipped directory.
5. `--dry-run` with `/tmp` on `tmpfs` and no `--scratch-dir`: the spill goes to the cache directory, never into DEST, and a dry run that never spills creates nothing anywhere.

---

## Task 1: `DirHandle::create_scratch` and `DirHandle::memory_backed`

**Files:**
- Modify: `crates/flux-fs/src/fs.rs` (trait `DirHandle`, `:306` onwards; the test double at `:632`)
- Modify: `crates/flux-platform/src/dir_unix.rs` (`impl DirHandle for StdDir`, `:132`)
- Modify: `crates/flux-platform/src/dir_windows.rs` (`impl DirHandle for StdDir`, `:156`)
- Modify: `crates/flux-core/src/fault_fs.rs` (`impl DirHandle for FakeDirHandle`, `:1972`)
- Test: `crates/flux-platform/tests/dir_control.rs`, `crates/flux-core/src/fault_fs.rs` tests

**Interfaces:**
- Produces:
  - `fn create_scratch(&self) -> Result<std::fs::File>` on `DirHandle`, no default body. Contract (doc comment): a new regular file inside this directory, opened for reading and writing, that has NO name by the time the call returns (Linux `O_TMPFILE` where the filesystem supports it, else an exclusive create of `.flux-scratch-<random hex>` then `unlinkat`; macOS the create-then-unlink path; Windows `FILE_DELETE_ON_CLOSE` with no sharing for delete, so the name disappears when the last handle closes, including at process death). Never follows a link.
  - `fn memory_backed(&self) -> Result<Option<bool>>` on `DirHandle` with the default body `Ok(None)` ("cannot tell"). Linux overrides: `fstatfs` `f_type` is `0x0102_1994` (tmpfs) or `0x8584_58f6` (ramfs) gives `Some(true)`, anything else `Some(false)`, the same cast as `crates/flux-platform/src/lock_file.rs:178-182`.
- `FakeDirHandle::create_scratch` returns `tempfile::tempfile()` (an anonymous real file; the fake has no bytes of its own to give), recorded as `create_scratch(<dir>)` in `calls()` and failing through the existing `fail("create_scratch", code)` hook (`fault_fs.rs:1124`). `FakeDirHandle` adds a test switch `fs.set_memory_backed(path, Option<bool>)` that its `memory_backed` answers (default `Some(false)`).

- [ ] **Step 0: Verify the state.** Confirm `DirHandle` has `create_hardlink_index` (`fs.rs:465`) and no `create_scratch`; the three `impl DirHandle for` blocks are at `fault_fs.rs:1972`, `dir_unix.rs:132`, `dir_windows.rs:156`, plus the test double near `fs.rs:632`; `capability_of` calls `fstatfs` at `lock_file.rs:177`. Else STOP: `STATE_MISMATCH: <what>`.
- [ ] **Step 1: Write the failing tests.**
  - `dir_control.rs`: `create_scratch_leaves_no_name_in_the_directory` (create, write 3 bytes, `read_at` them back, assert `listing(&d)` is unchanged from before the call); `two_scratch_files_are_independent` (write different bytes to two, read each back); `#[cfg(target_os = "linux")] memory_backed_says_false_for_the_test_tempdir_and_true_for_dev_shm` (the test tempdir answers `Some(false)` unless the runner's temp dir is itself tmpfs, in which case the test asserts `Some(true)` for it; `/dev/shm` opened with `StdFileSystem.destination_root` answers `Some(true)`; skip the `/dev/shm` half with an `eprintln!` when it does not exist).
  - `fault_fs.rs` tests: `create_scratch_is_logged_and_can_fail` (one call appears as `create_scratch(/d)`; after `fs.fail("create_scratch", Code::DiskFull)` the next call is `Err` with that code); `memory_backed_answers_the_switch`.
- [ ] **Step 2: Run them to see the build fail.** `cargo nextest run -p flux-platform -p flux-core --no-fail-fast create_scratch memory_backed` Expected: compile error (no method).
- [ ] **Step 3: Implement** the two trait methods, the Unix and Windows overrides, and the fake. Unix `O_TMPFILE`: `openat(dir, ".", OFlags::RDWR | OFlags::TMPFILE | OFlags::CLOEXEC, 0o600)`; on `EOPNOTSUPP`, `EISDIR` or `EINVAL` fall back to the exclusive create with `OFlags::NOFOLLOW` and `unlinkat`. The fallback name's suffix is 16 hex digits of `SystemTime::now()` nanoseconds since the epoch XOR `std::process::id()` XOR an attempt counter, retried on `EEXIST` up to 16 times (`flux-platform` does not depend on `flux-core`, so `ids::new_id` is not available).
- [ ] **Step 4: Windows check.** `cargo check --target x86_64-pc-windows-gnu -p flux-fs -p flux-platform -p flux-core --tests` Expected: exit 0.
- [ ] **Step 5: Run.** `cargo nextest run --workspace --no-fail-fast` Expected: every test passes.
- [ ] **Step 6: Gate and commit.** `just check` exit 0; `git add -A crates && git commit -m "feat: a nameless scratch file in a directory, and whether it is memory-backed"` with the trailer.

---

## Task 2: `Thresholds`, `ScratchSpace` and the region guard

**Files:**
- Create: `crates/flux-core/src/scratch.rs`
- Modify: `crates/flux-core/src/lib.rs` (add `pub mod scratch;` beside `pub mod walk;`, `:25`)

**Interfaces:**
- Consumes: `DirHandle::create_scratch`, `DirHandle::memory_backed` (Task 1).
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct Thresholds { pub inline_entries: usize, pub run_entries: usize, pub merge_fan_in: usize, pub merge_buffer: usize, pub tick_entries: usize }`, `impl Default` with the constants of Global Constraints, and `pub fn new(inline_entries, run_entries, merge_fan_in, merge_buffer, tick_entries) -> Result<Thresholds, &'static str>` that refuses `inline_entries > run_entries`, `merge_fan_in < 2`, `run_entries == 0` and `merge_buffer < 4`.
  - `pub struct ScratchSpace<'h>` with:
    - `pub fn unplaced(thresholds: Thresholds, explicit: Option<PathBuf>) -> ScratchSpace<'h>`: no home yet; `explicit` is the operator's `--scratch-dir`, carried by the space so every placement site (including the sweep inside `open_operation`) sees it without threading a path through `Place`. A spill attempted while unplaced is a fatal scratch error with the text `scratch space used before it was placed` (a programming error surfaced as an error, not a panic, because a release build aborts on panic).
    - `pub fn explicit(&self) -> Option<&Path>`.
    - `pub fn place_or_remember<D: DirHandle + 'h>(&self, home: Result<(D, PathBuf), FsError>)`: `Ok` is `place`; `Err` is remembered and returned by the first `begin` as the fatal scratch error, so a run that never spills never fails on a location it did not need (the dry run and `verify` both use this with `temporary_home`).
    - `pub fn place<D: DirHandle + 'h>(&self, home: D, shown: PathBuf)`: sets the home once (a second call is ignored); nothing is created until the first write. `DirHandle` is `Sized` with five associated types (`crates/flux-fs/src/fs.rs:306-319`) and cannot be a trait object, so the home is stored ERASED as `Box<dyn Fn() -> flux_fs::Result<std::fs::File> + 'h>` built as `Box::new(move || home.create_scratch())`.
    - `pub fn thresholds(&self) -> Thresholds`.
    - `pub(crate) fn begin(&self) -> Result<Region<'_, 'h>, FsError>`: a region starting at the file's current length; creates the file on the first call (through `home.create_scratch()`), so a run that never spills creates nothing.
    - `pub(crate) fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), FsError>`, `pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError>`, `pub(crate) fn len(&self) -> u64` (the logical end, held in a `Cell<u64>`, not a `metadata()` call).
    - `pub(crate) fn poisoned(&self) -> bool`; every `read_at`/`write_at`/`begin` on a poisoned space returns the fatal error `scratch space is unusable after a failed truncate`.
    - `#[cfg(test)] pub(crate) fn fail_writes_after(&self, n: u64)`: the `n+1`-th `write_at` fails with `Code::DiskFull`.
    - `pub fn close(&self)`: drops the file (on Windows the `FILE_DELETE_ON_CLOSE` handle, without which the workspace directory cannot be removed). A later `begin` is a fatal error. Called before a run's finish removes the workspace.
    - INVARIANT (doc comment): one ACTIVE walk per space. Regions are LIFO, so two walks iterated in alternation on one space (as `verify` iterates its two sides) would truncate each other's runs. A `Cell<usize>` counts live regions per owner token; `begin` from a second walk while the first has a live region is a fatal error `one scratch space is shared by two walks`, not silent corruption. The owner token is a `u64` each `Walk` takes from `ScratchSpace::next_owner()` when it is built.
  - `pub(crate) struct Region<'s, 'h>`: `start()`, and `Drop` that truncates the file to `start` (`File::set_len`) and lowers the logical end; a failed truncate poisons the space. Interior mutability throughout (`RefCell`, `Cell`, `OnceCell`): the walk holds `&ScratchSpace` while a parent region and a child region coexist.
  - Every scratch error is `FsError` with `Code::IoError` or the classified code of the underlying I/O error (`FsError::from_io`, `crates/flux-fs/src/error.rs:142`), its message naming `shown`.

- [ ] **Step 0: Verify the state.** `crates/flux-core/src/scratch.rs` does not exist; `lib.rs:25` is `pub mod walk;`. Else STOP.
- [ ] **Step 1: Write the failing tests** (in `scratch.rs`, `FaultFs` as home):
  - `thresholds_refuse_an_inline_limit_above_the_run_size`: `Thresholds::new(9, 8, 2, 4, 1)` is `Err`; `Thresholds::new(8, 8, 2, 4, 1)` is `Ok`; `Thresholds::default()` equals the five constants.
  - `nothing_is_created_until_the_first_region`: place on `/s`, assert no `create_scratch(` in `fs.calls()`; `begin()` once; assert exactly one.
  - `an_unplaced_space_refuses_to_spill`: `begin()` on an unplaced space is `Err` whose message contains `before it was placed`.
  - `nested_regions_truncate_in_lifo_order`: region A writes 10 bytes, region B writes 5 more; drop B: `len() == 10` and A's bytes read back unchanged; drop A: `len() == 0`.
  - `an_injected_write_failure_is_returned_and_the_file_ends_empty`: `fail_writes_after(1)`; the second `write_at` is `Err` with `Code::DiskFull`; after the region drops `len() == 0`.
  - `a_second_walk_cannot_begin_while_the_first_holds_a_region`: owner 1 begins a region; owner 2's `begin` is `Err` with `shared by two walks`; after owner 1's region drops, owner 2's `begin` is `Ok`.
  - `a_closed_space_refuses_to_spill`: after `close()`, `begin()` is `Err`.
  - `a_failed_creation_is_returned_on_every_attempt`: `fs.fail("create_scratch", Code::PermissionDenied)` then two `begin()` calls are both `Err` with that code, and `create_scratch(` appears once (the failure is remembered, as `HardlinkSlot` remembers its own, `crates/flux-core/src/hardlinks.rs:73-91`).
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core --no-fail-fast scratch::` Expected: compile error.
- [ ] **Step 3: Implement** `scratch.rs` to the interfaces above. A poisoned-path test is not writable without a failing `set_len`; add `#[cfg(test)] fn fail_truncate(&self)` and the test `a_failed_truncate_poisons_every_later_call`.
- [ ] **Step 4: Run.** `cargo nextest run -p flux-core --no-fail-fast scratch::` Expected: all pass.
- [ ] **Step 5: Non-vacuity.** One mutant at a time, restore each: `Region::drop` does nothing (`nested_regions_truncate_in_lifo_order` red); `begin` creates eagerly in `place` (`nothing_is_created_until_the_first_region` red); the poisoned check removed from `read_at` (`a_failed_truncate_poisons_every_later_call` red). Report the red names.
- [ ] **Step 6: Gate and commit.** `just check` exit 0; commit `feat: a scratch space whose regions are truncated in stack order`.

---

## Task 3: The run record, the prefix sort and the k-way merge

**Files:**
- Create: `crates/flux-core/src/spill.rs` (add `mod spill;` to `lib.rs`; `pub(crate)` items only)

**Interfaces:**
- Consumes: `ScratchSpace`, `Region`, `Thresholds` (Task 2).
- Produces:
  - `pub(crate) fn encode(entry: &DirEntry, out: &mut Vec<u8>) -> Result<(), FsError>` and `pub(crate) fn decode(bytes: &[u8]) -> Option<(DirEntry, usize)>` (the record of Global Constraints; `decode` returns the entry with `ino: None` and the record length; `None` on a short buffer). The name is rebuilt WITHOUT `unsafe`: Unix `OsStringExt::from_vec`; Windows `OsStringExt::from_wide(&wtf8_to_wide(bytes))`.
  - `pub(crate) fn wtf8_to_wide(bytes: &[u8]) -> Vec<u16>`: compiled on EVERY platform (so Linux tests it), decoding generalized UTF-8 including 3-byte encoded lone surrogates (`ED A0 80`..`ED BF BF`) into UTF-16 units, with supplementary scalars split into a surrogate pair.
  - `pub(crate) fn key_cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering`: the 8-byte big-endian prefix (zero-padded) compared as `u64`, then the full slices.
  - `pub(crate) fn sort_entries(v: &mut [DirEntry])`: `sort_unstable_by` over cached prefixes (a `Vec<(u64, u32)>` of prefix and index, or an equivalent that compares the `u64` before touching the bytes); stable order is not needed (spec 6.2 item 8: no two names are equal).
  - `pub(crate) fn write_run(space: &ScratchSpace, entries: &[DirEntry]) -> Result<RunSpan, FsError>` where `pub(crate) struct RunSpan { pub start: u64, pub len: u64, pub count: u64 }`, appended at `space.len()` through one buffer of at most `merge_buffer * 4` bytes flushed as it fills.
  - `pub(crate) struct MergeReader<'s, 'h>`: `pub(crate) fn new(space: &'s ScratchSpace<'h>, runs: Vec<RunSpan>, region: Region<'s, 'h>) -> Result<Self, FsError>`, `Iterator<Item = Result<DirEntry, FsError>>`. One `merge_buffer`-byte buffer per run, refilled with `read_at`; a `BinaryHeap` of `(Reverse(prefix), Reverse(run index))` heads compared by `key_cmp`. It owns the `Region`, so dropping the reader truncates the directory's runs.
  - `pub(crate) fn cascade(space: &ScratchSpace, runs: Vec<RunSpan>, tick: &mut dyn FnMut(u64) -> Result<(), FsError>) -> Result<Vec<RunSpan>, FsError>`: while `runs.len() > merge_fan_in`, merge consecutive groups of `merge_fan_in` into new runs appended at the end of the file (inputs stay until the region drops, spec 6.2 item 5); returns at most `merge_fan_in` runs. `tick` is called with the number of entries merged so far every `tick_entries` entries, and once per pass with `tracing::debug!(pass, passes, bytes, "merge pass")`.

- [ ] **Step 0: Verify the state.** `DirEntry` is `{ name, file_type, ino }` (`crates/flux-fs/src/fs.rs:48-57`); `FileType` has exactly `File, Dir, Symlink, Other` (`fs.rs:36-41`); `walk.rs:166-169` sorts by `as_encoded_bytes()`. Else STOP.
- [ ] **Step 1: Write the failing tests** (in `spill.rs`):
  - `a_record_round_trips` for each `FileType`, an empty-looking one-byte name, a 255-byte name, and on Unix `[0x80, 0xFF, b'a']`.
  - `a_name_longer_than_u16_is_an_error_not_a_panic`: a 65,536-byte name (Unix only; `OsString::from_vec`) makes `encode` return `Err`.
  - `wtf8_decodes_ascii_bmp_supplementary_and_lone_surrogates`: `"a"` gives `[0x61]`; `"é"` gives `[0xE9]`; `"😀"` gives `[0xD83D, 0xDE00]`; `[0xED, 0xA0, 0x80]` gives `[0xD800]`; `[0xED, 0xBF, 0xBF]` gives `[0xDFFF]`.
  - `key_cmp_agrees_with_byte_order` (proptest, 10,000 cases): for random byte vectors of length 0 to 20 drawn from a 4-byte alphabet `{0x00, 0x61, 0x62, 0xFF}` (so equal 8-byte prefixes are common), `key_cmp(a, b) == a.cmp(b)`.
  - `a_spilled_sequence_equals_sorted` (proptest, 256 cases): 0 to 300 distinct names (Unix: arbitrary bytes except `/` and `0`; Windows: arbitrary `String` without `\\`, `/` and `\0`), `Thresholds::new(4, 8, 2, 16, 3)`, written as runs of `run_entries` sorted with `sort_entries`, cascaded, merged: the names equal `sorted()` of the set and the file types ride with them.
  - `a_cascade_makes_the_expected_number_of_passes`: 100 entries, `run_entries = 4`, `merge_fan_in = 2` gives 25 runs, then passes leaving 13, 7, 4, 2: four passes, observed through the count of `tick` pass calls.
  - `dropping_a_reader_truncates_its_runs`: after a merge reader over 3 runs drops, `space.len()` equals the region's start.
  - `a_read_error_mid_merge_is_returned_and_the_reader_ends`: `fail_writes_after` is for writes; add `#[cfg(test)] ScratchSpace::fail_reads_after(n)` in this task and assert the iterator yields one `Err` then `None`.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core --no-fail-fast spill::` Expected: compile error.
- [ ] **Step 3: Implement** `spill.rs`.
- [ ] **Step 4: Run.** `cargo nextest run -p flux-core --no-fail-fast spill::` Expected: all pass.
- [ ] **Step 5: Non-vacuity.** One at a time: `key_cmp` compares only the prefix (`key_cmp_agrees_with_byte_order` red); the heap pops the LARGEST head (`a_spilled_sequence_equals_sorted` red); `cascade` stops at `<= 2 * merge_fan_in` (`a_cascade_makes_the_expected_number_of_passes` red); the WTF-8 decoder treats `ED A0 80` as invalid and emits U+FFFD (`wtf8_decodes_ascii_bmp_supplementary_and_lone_surrogates` red).
- [ ] **Step 6: Gate and commit.** `just check` exit 0; commit `feat: sorted runs of directory entries and their k-way merge`.

---

## Task 4: The walk lists through `list_sorted`; fatal errors; the tick; the unstarted walk

**Files:**
- Modify: `crates/flux-core/src/walk.rs` (`WalkOptions` `:84-87`, `WalkError` `:98-103`, `Frame` `:106-117`, `Walk` `:121-139`, `sorted` `:166-169`, `walk`/`walk_with_depth`/`walk_with` `:189-251`, `next` `:270-370`, `enter` `:388-504`)
- Modify: every `WalkOptions {` and `WalkError {` literal the compiler names (today: `tree.rs:483`, `verify.rs:179`, and the walk tests)
- Test: `crates/flux-core/src/walk.rs` tests

**Interfaces:**
- Consumes: Tasks 2 and 3.
- Produces:
  - `pub struct WalkOptions { pub max_depth: usize, pub mounts: MountPolicy }` unchanged in shape; the thresholds come from the `ScratchSpace`.
  - `pub struct WalkError { pub path: PathBuf, pub cause: FsError, pub fatal: bool }`: `fatal` is `true` for a failed tick and for any scratch error, `false` for everything the walk reports today (doc comment: a consumer that sees `fatal` must stop the operation; the walk yields nothing after a fatal error).
  - `pub type Tick<'t> = dyn FnMut() -> Result<(), FsError> + 't;`
  - Lifetimes: `Walk<'a, F>` holds `Option<&'a ScratchSpace<'h>>` with `'h: 'a` (the space's home may outlive the walk; written `ScratchSpace<'a>` below for brevity where the two coincide; the compiler is the oracle for the exact bounds).
  - `pub fn spilled_directories(&self) -> u64`: how many directories this walk spilled (GATE-A and Task 7 read it).
  - `pub fn walk_with<'a, F: FileSystem>(fs: &'a F, root: &Path, options: WalkOptions, scratch: Option<&'a ScratchSpace<'a>>) -> Result<Walk<'a, F>>`: EAGER, as today (the root is listed before it returns; a listing error is the `Err`). `None` means "never spill" (today's behaviour exactly: every directory sorted in memory).
  - `pub fn walk_unstarted<'a, F: FileSystem>(fs, root, options, scratch: Option<&'a ScratchSpace<'a>>) -> Result<Walk<'a, F>>`: the metadata and directory checks of `walk.rs:213-219` and the ancestor push, NO listing.
  - `pub fn start(&mut self, tick: &mut Tick<'_>) -> std::result::Result<(), WalkError>`: lists the root (idempotent: a started walk returns `Ok(())`); its error has `path: PathBuf::new()`.
  - `pub fn next_with(&mut self, tick: &mut Tick<'_>) -> Option<std::result::Result<WalkEvent, WalkError>>`; `impl Iterator` calls it with a tick that returns `Ok(())`. Calling `next_with` on an unstarted walk starts it first and yields the start error as the first item (so a consumer cannot skip the root by accident).
  - `walk(fs, root)` and `walk_with_depth(fs, root, depth)` keep their signatures and pass `None`.
  - `fn list_sorted(&self, dir: &Path, tick: &mut Tick<'_>) -> Result<Entries<'a>, ListError>` (private): spec 6.2 items 1-6 with `read_dir_each`; `Entries` is `enum { Inline(std::vec::IntoIter<DirEntry>), Spilled(MergeReader<'a, 'a>) }`; `ListError` distinguishes the listing's own error (non-fatal, today's `WalkError`) from a scratch or tick error (fatal). `tick` is called every `tick_entries` entries read, written or merged. One `tracing::debug!(entries, runs, "listing spilled")` per spilled directory.
  - `Frame.entries: Entries<'a>`; a merge reader's `Err` item becomes a fatal `WalkError` for that frame's path, after which `next_with` returns `None` forever.
- `DirEntry` names from the merge reader go through the same `is_one_component` check (`walk.rs:301`) as today.

- [ ] **Step 0: Verify the state.** `sorted` is called at `walk.rs:220` and `:486`; `Frame.entries` is `std::vec::IntoIter<DirEntry>` (`:109`); `WalkError` has `path` and `cause` only (`:98-103`); `walk_refuses_a_root_whose_read_dir_fails` is at `:1384`. Else STOP.
- [ ] **Step 1: Write the failing tests** (walk tests module; a `fn spilled(fs: &FaultFs, t: Thresholds) -> (ScratchSpace, ...)` helper placing the space on a `FaultFs` directory `/scratch`):
  - `a_spilled_walk_yields_the_same_events_as_an_inline_walk`: the tree `/r` with 3 subdirectories of 0, 9 and 40 files plus `component_order_tree`'s names; the event list of `walk_with(.., Some(&space))` with `Thresholds::new(4, 8, 2, 16, 3)` equals `paths(&fs)` (the existing inline helper, `walk.rs:593`), and `create_scratch(` appears once.
  - `a_directory_of_exactly_inline_entries_never_spills`: 4 files, inline 4: no `create_scratch(` call.
  - `a_child_spill_does_not_disturb_the_parents_merge`: `/r` holds 20 files and a directory `m` (sorting in the middle) of 20 files; spilled events equal inline events.
  - `the_run_file_is_empty_after_a_walk_ends_and_after_it_is_dropped_early`: after a full walk `space.len() == 0`; a second walk dropped after 5 events also leaves `len() == 0`.
  - `a_scratch_write_failure_is_fatal_and_ends_the_walk`: `fail_writes_after(2)`; some item is `Err` with `fatal == true`, no item follows it, and `space.len() == 0` after the drop.
  - `a_failing_tick_is_fatal`: a tick that fails on its second call over a 40-entry spilled directory with `tick_entries = 8`: the walk yields a fatal error carrying the tick's code.
  - `the_tick_runs_at_least_once_per_tick_entries`: count calls over a 100-entry spilled directory with `tick_entries = 8`: at least 12.
  - `an_unstarted_walk_lists_nothing_until_started`: `walk_unstarted` makes no `read_dir(` call; `start` makes exactly one; a second `start` makes none.
  - `an_unstarted_walk_whose_root_cannot_be_listed_fails_at_start`: `fs.fail_nth("read_dir", 1, Code::PermissionDenied, PermissionDenied)`: `walk_unstarted` is `Ok`, `start` is `Err` with `path` empty, `fatal == false`, code `PermissionDenied`.
  - `walk_refuses_a_root_whose_read_dir_fails` (existing, `walk.rs:1384`) passes unchanged.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core --no-fail-fast walk::` Expected: compile error.
- [ ] **Step 3: Implement.** Fix each `WalkError {` and `WalkOptions`/`walk_with` call the compiler names with `fatal: false` and `None`; production callers change in Tasks 5 and 6.
- [ ] **Step 4: Run.** `cargo nextest run --workspace --no-fail-fast` Expected: all pass (every existing walk, tree, verify and run test included: the default path never spills).
- [ ] **Step 5: Non-vacuity.** One at a time: `list_sorted` spills only when the stream exceeds `run_entries` instead of `inline_entries` (`a_spilled_walk_yields_the_same_events_as_an_inline_walk` red at 9 files); the merge reader is not moved into the frame, so the region drops at the end of `list_sorted` (`a_child_spill_does_not_disturb_the_parents_merge` or the first test red); `fatal` left `false` for scratch errors (`a_scratch_write_failure_is_fatal_and_ends_the_walk` red).
- [ ] **Step 6: Gate and commit.** `just check` exit 0; commit `feat: the walk spills a large directory to sorted runs and merges them`.

---

## Task 5: The copy places its scratch, starts its walk and beats through a listing

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`prepare_source` `:475-493`, `run_tree` `:679-747`, `copy_tree_frames` `:1097-1131`, `walk_into` `:1189-1211`, `Shared` `:496`)
- Modify: `crates/flux-core/src/run/mod.rs` (`RunConfig` `:76-105`, `tree_with` `:305-410`, `drive` `:434-606`, `dry_tree` `:1426`, the file dry path near `:1520-1535`)
- Modify: `crates/flux-core/src/run/multi.rs` (the several-source run around `:852`, `:995`, `:1155`; the comment at `:3586-3587`)
- Modify: `crates/flux-core/src/scratch.rs` (add `temporary_home`)
- Modify: `crates/flux-core/src/state.rs` (`finish_retire`, `:1039-1075`)
- Test: `crates/flux-core/src/run/tests.rs`, `crates/flux-core/src/tree.rs` tests

**Interfaces:**
- Consumes: Tasks 1, 2, 4.
- Produces:
  - `RunConfig` gains `pub scratch_dir: Option<PathBuf>` (`--scratch-dir`; `None` uses the spec 6.1 chain). Every `RunConfig {` literal the compiler names gains `scratch_dir: None` (today 11 files, `rg -l 'RunConfig \{' crates`).
  - `Source` gains nothing; `prepare_source` calls `walk_unstarted(fs, src_root, WalkOptions { .. }, scratch)` with a new parameter `scratch: Option<&'a ScratchSpace<'a>>`.
  - `Shared` is unchanged: the walk already holds the scratch.
  - `tree_with` becomes a thin wrapper over `pub(crate) fn tree_with_thresholds(.., thresholds: Thresholds)`, which tests call with small values (`RunConfig` is public, so no test-only field goes on it).
  - `copy_tree_frames` calls `events.start(&mut tick)` before `root_state` (`tree.rs:1106`); on `Err(e)` it returns `(root, Err(CopyError::at(CopyStep::Source, e.cause)))` when `!e.fatal`, else `(root, Err(<the fatal mapping below>))`, matching its return type `(Option<F::Dir>, Result<(), CopyError>)` (`tree.rs:1105`).
  - `walk_into` drives the walk with `events.next_with(&mut tick)` where `tick` runs `beat_and_guard(cx)` (`tree.rs:893-896`) and, on `Err(copy_error)`, stores it in a local `Option<CopyError>` and returns `Err(FsError::new(code, std::io::Error::other(message)))` built from the stored error's code and message (`FsError` is not `Clone`; the walk only needs to know the tick failed). On a `WalkError` with `fatal == true` `walk_into` returns the stored `CopyError` if one was stored, else `CopyError::at(CopyStep::Resolve, e.cause)` (a scratch error; `CopyStep::Resolve` is "any failure that belongs to no copy step", `crates/flux-core/src/copy.rs:47-49`). This replaces, for fatal errors only, the report-and-continue arm at `tree.rs:1205-1210`.
  - `pub(crate) fn temporary_home<F: DestinationRoot>(fs: &F, explicit: Option<&Path>, temp_root: &Path) -> Result<(F::Dir, PathBuf), FsError>` in `scratch.rs`: `explicit` wins with no `tmpfs` check; else `temp_root` unless `memory_backed()` is `Some(true)`; else `$XDG_CACHE_HOME`, then `$HOME/.cache` (Windows `%LOCALAPPDATA%`), made with `create_dir` if absent and checked the same way; else `Err` with `Code::IoError` and the message `no disk-backed scratch location (the temporary directory is memory-backed); pass --scratch-dir PATH`. On Windows the chosen directory gets a `flux-scratch` subdirectory, and on first use every file in it older than 24 hours is removed (best effort; errors ignored).
  - `drive` (`run/mod.rs:434`) gains the parameter `scratch: &ScratchSpace<'_>`; its two callers (`tree_with`, `run/mod.rs:388`, and the several-source run, `multi.rs` near `:980`) pass the space they created. `Place::sweep` (`run/place.rs:63-68`) gains `scratch: Option<&ScratchSpace<'_>>` (Task 6 uses it for the walk; here for placement).
  - Placement: `pub(crate) fn place_scratch<F: DestinationRoot>(fs: &F, place: &TreePlace<'_, F>, id: &str, scratch: &ScratchSpace<'_>) -> flux_fs::Result<()>` places the scratch on `scratch.explicit()` (opened with `fs.destination_root`) when given, else directly on the workspace `DEST/.flux/operations/<id>/` (opened with `open_workspace`, `run/place.rs:448-450`; no subdirectory is made, so a `--resume` that reuses the prior's workspace needs nothing extra, and there is no directory to drain). It is idempotent (`place` ignores a second call) and is called in TWO places, because the `--restart` sweep runs INSIDE `open_operation` (`supersede` at `run/session.rs:286`, after `place.create` at `:225`), before `drive` regains control: (1) at the top of `TreePlace::sweep` (`run/place.rs:495`, with `locked.state.operation_id`), and (2) in `drive` right after `open_operation` returns (`run/mod.rs:460`, with `locked.state.operation_id`). A failure in (2) stops the run as `CopyError::at(CopyStep::Resolve, e)` before the walk; a failure in (1) is the sweep's `Fault::Io`.
  - `drive` calls `scratch.close()` after the walk closure returns and before the WAL thread ends (`run/mod.rs:607`), so no scratch handle is open when `finish` removes the workspace (Windows refuses to remove a directory holding a delete-pending file). A dry run and `run_tree` (the lockless library copy, `tree.rs:679`) call `scratch.place_or_remember(temporary_home(..))` at once (a `temporary_home` error surfaces only if a directory actually spills); tests that call `prepare_source` directly pass `None`. `finish_retire` (`crates/flux-core/src/state.rs:1039-1075`) gains one step: after the known flat names (`:1042-1051`), it lists the workspace and removes every regular FILE whose name starts with `.flux-scratch-` (normally none: the file is nameless, but the create-then-unlink fallback of Task 1 leaves such a name if the process dies between the two calls), exactly as it removes the quarantined WAL segments by prefix (`:1059-1064`). Every other entry is a stranger and stays, so the workspace stays (owner ruling 23, the comment at `:1052-1053`). Tests in `state.rs`: `retire_removes_a_scratch_crash_leftover` and `retire_keeps_a_stranger_beside_a_scratch_leftover` (the workspace survives, the leftover is gone).
  - `tree_with` and the several-source run create `let scratch = ScratchSpace::unplaced(thresholds, cfg.scratch_dir.clone());` BEFORE `prepare_source`, so it outlives the walk.
- `multi.rs:3586-3587`'s comment is rewritten: the listing is made when the root's copy begins; a dry run starts it when its copy begins too.

- [ ] **Step 0: Verify the state.** The lines above; `prepare_source` has 10 direct test callers in `tree.rs` (`rg -n 'prepare_source\(' crates/flux-core/src/tree.rs`); `finish_retire`'s known names are at `state.rs:1042-1049` with the one subdirectory `wal` at `:1054`. Else STOP.
- [ ] **Step 1: Write the failing tests.**
  - `run/tests.rs`: `a_copy_whose_root_spills_puts_its_scratch_in_the_workspace`: a `FaultFs` source of 20 files through `tree_with_thresholds` with `Thresholds::new(4, 8, 2, 16, 3)`: the call log has `create_scratch(/p/dest/.flux/operations/<ID>)` AFTER the first `create_lock(`, the copy completes with 20 files, and `/p/dest/.flux` is gone after the finish.
  - `run/tests.rs`: `a_copy_whose_root_cannot_be_listed_fails_after_the_lock_with_the_source_step`: `fs.fail_read_dir_of("/src", Code::PermissionDenied, PermissionDenied)`: `r.copy` is `Err` with `CopyStep::Source`, the state was written FAILED and the lock released (the shape of `a_failed_single_file_copy_leaves_a_failed_record_and_releases_the_lock`, `run/tests.rs:842-852`), nothing under `/p/dest` but `.flux` state.
  - `run/tests.rs`: `a_mistyped_source_creates_nothing` (existing, `:833`) passes unchanged.
  - `run/tests.rs`: `a_failed_heartbeat_during_a_spilled_listing_stops_the_copy`: `beating()` config (`run/tests.rs:1545`), a 40-file subdirectory with `tick_entries = 8`, the heartbeat failing through `fail_heartbeat` (`:1562`) at a write index that lands inside the listing (find it with the `nth_of` pattern the multi tests use); assert `(e.step, e.code()) == (CopyStep::Heartbeat, Code::IoError)` and that no file of that subdirectory was published.
  - `run/tests.rs`: `a_restart_sweep_over_a_spilling_dest_uses_the_new_workspace`: a prior operation's state plus 20 files in DEST, `restart()` config, small thresholds: `create_scratch(` is logged on the NEW operation's workspace directory and the sweep completes (without placement in `TreePlace::sweep` it fails with "before it was placed").
  - `run/tests.rs`: `a_hardlink_group_straddling_run_boundaries_keeps_its_canonical_member` (spec R5): a source directory of 20 files where `f03` and `f17` are one object (`FaultFs` hardlink), thresholds `(4, 8, 2, 16, 3)` so they land in different runs: the published tree, the canonical member (the byte-order-first name, `f03`) and the link are the same as with `Thresholds::default()` on the same fixture.
  - `run/tests.rs`: `a_dry_run_that_never_spills_creates_no_scratch`: `create_scratch(` absent from the log of a dry run over `fake()`.
  - `run/multi.rs` tests: `an_unlistable_second_root_stops_the_run_after_the_first_root`: roots `a`, `b`, `c`; `/src/b`'s listing fails: root `a` is `Completed`, `b` is `Failed`, `c` is `NotStarted`, the run aborts with `CopyStep::Source` (the owner-visible behaviour change of deviation 2, pinned).
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core --no-fail-fast spills_puts root_cannot_be_listed during_a_spilled_listing never_spills_creates unlistable_second_root` Expected: compile error or red.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** `cargo nextest run --workspace --no-fail-fast` Expected: all pass. A pre-existing test whose `read_dir(` call order or count changed because the root is now listed later is a STOP: report `[original] -> [yours] because ...`; the owner decides (deviation 2 is the only intended order change).
- [ ] **Step 5: Non-vacuity.** One at a time: `copy_tree_frames` skips `start` (the walk then starts inside `next_with`, so the source error lands as a reported `Walk` failure: `a_copy_whose_root_cannot_be_listed_fails_after_the_lock_with_the_source_step` red); `walk_into` reports a fatal error and continues (`a_failed_heartbeat_during_a_spilled_listing_stops_the_copy` red); `prepare_source` builds its walk with the eager `walk_with` instead of `walk_unstarted` (the root then spills before any placement: `a_copy_whose_root_spills_puts_its_scratch_in_the_workspace` red, the run stopping with "before it was placed"); placement in `TreePlace::sweep` removed (`a_restart_sweep_over_a_spilling_dest_uses_the_new_workspace` red).
- [ ] **Step 6: Gate and commit.** `just check` exit 0; commit `feat: a copy spills into its workspace and beats while it lists`.

---

## Task 6: `verify`, the destination sweep and the dry restart preview stop on a fatal walk error

**Files:**
- Modify: `crates/flux-core/src/verify.rs` (`VerifyOptions` `:111-117`, `VerifyError` `:121-131`, `Side::new` `:171-190`, `Side::advance` `:276-313`, `walk_checked` `:597-669`, `verify_tree_filtered` `:422-439`, `verify_several` `:455-564`)
- Modify: `crates/flux-core/src/run/sweep.rs` (`sweep_partials` `:25-62`, `partials_under` `:69-120`)
- Modify: `crates/flux-core/src/run/place.rs` (`TreePlace::sweep`, `:495-517`)
- Modify: `crates/flux-core/src/run/dry_prior.rs` (`dry_restart`, `:183-210`)
- Modify: `crates/flux-core/src/run/mod.rs` (`dry_start`'s `restart_preview` type `:1332`, its use `:1373`, the closures at `:1441-1447` and `:1587`), `crates/flux-core/src/run/multi.rs` (`:1062-1068`)
- Modify: `crates/flux-core/src/cleanup/delete.rs:570` (passes `None`)

**Interfaces:**
- `VerifyOptions` gains `pub scratch_dir: Option<PathBuf>` (literals: `crates/flux-cli/src/main.rs`, `verify.rs`, `verify_proptest.rs`; the compiler is the oracle). `verify_tree_filtered` and `verify_several` create TWO `ScratchSpace`s per call, one for each side (the two walks are iterated in alternation, so one shared space would break the LIFO regions; Task 2's invariant check would turn that into a fatal error), each given `place_or_remember(temporary_home(fs, opts.scratch_dir.as_deref(), &std::env::temp_dir()))` (Task 2), so a verify that never spills never fails on an unusable location.
- `VerifyError` gains `Failed { error: FsError }` (doc: exit 1, an operation-level failure after records may have been emitted). `Side` gains `fatal: Option<FsError>`; `Side::advance` on a fatal `WalkError` stores it, sets `done`, and returns `None`. `walk_checked` checks `a.fatal` and `b.fatal` after every `peek` and before emitting, and returns the first as `Err(VerifyError::Failed { .. })`; `walk_checked` therefore returns `Result<(), VerifyError>` and both callers propagate it.
- `partials_under` gains `scratch: Option<&ScratchSpace<'_>>` and `tick: &mut Tick<'_>` and returns `Result<(Vec<(PathBuf, String)>, bool), FsError>`; it drives `walk_with` (eager, scratch `scratch`) with `next_with(tick)`; a fatal `WalkError` returns `Err(e.cause)`.
- `sweep_partials` gains `scratch: Option<&ScratchSpace<'_>>`; its tick wraps `check()` and stashes the `Fault`; on `Err` from `partials_under` it returns the stashed `Fault` if any, else `Fault::Io(dest_shown.to_path_buf(), e)` (`run/session.rs:414-421`). `TreePlace::sweep` passes the run's scratch (the `ScratchSpace` must reach `Place::sweep`: add it as a parameter of `sweep` in the `Place` trait, `run/place.rs:63-68`, and of every implementor the compiler names); `cleanup/delete.rs:570` passes `None`.
- `dry_restart` gains `scratch: Option<&ScratchSpace<'_>>` and returns `Result<DryPrior, FsError>` (a no-op tick). `dry_start`'s `restart_preview` becomes `&mut dyn FnMut(&[PriorOp], &mut Vec<RunWarning>) -> Result<DryPrior, RunError>`; at `run/mod.rs:1373` an `Err` returns `DryStart::Stop(e, dry)` (the variant at `:1400`). The closures map the `FsError` with `failed(RunStep::State, &operations_shown, e)` (the helper used at `run/place.rs:387`); the file dry path (`:1587`) wraps its value in `Ok`.

- [ ] **Step 0: Verify the state.** The cited lines; `Side::advance` maps every `WalkError` to an item at `verify.rs:285`; `partials_under` returns `(Vec, bool)` (`sweep.rs:75`). Else STOP.
- [ ] **Step 1: Write the failing tests.**
  - `verify.rs` tests: `a_fatal_scratch_error_ends_verify_as_failed_without_missing_records`: source and destination each hold 40 identical files, small thresholds through a `#[cfg(test)]` constructor `verify_tree_with_thresholds`, the source side's scratch failing writes after 2: the result is `Err(VerifyError::Failed { .. })` and no emitted record has outcome `Extra` (the early-ended source side must not make the destination look extra).
  - `verify.rs` tests: `both_sides_spilling_match_as_when_neither_spills`: source and destination of 40 identical files plus one differing file, small thresholds: the records equal those of the default thresholds.
  - `verify.rs` tests: `a_verify_that_never_spills_needs_no_scratch_location`: `scratch_dir: Some("/nonexistent")` over the existing small trees: `Ok` with the same summary as `None`.
  - `sweep.rs` tests: `a_fatal_walk_error_in_the_sweep_returns_the_fault`: a DEST of 40 files with small thresholds and a scratch failing writes: `sweep_partials` returns `Err(Fault::Io(..))`, and with a `check` that fails `Fault::Lost` on its second call it returns `Err(Fault::Lost)`.
  - `dry_prior.rs` or `run/tests.rs`: `a_dry_restart_whose_sweep_walk_fails_fatally_stops_the_preview`: the dry run's `stop` is `Some`, nothing is reported as `Restart`.
- [ ] **Step 2: Run them to see them fail.** Expected: compile error or red.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** `cargo nextest run --workspace --no-fail-fast` Expected: all pass.
- [ ] **Step 5: Non-vacuity.** One at a time: `walk_checked` ignores `b.fatal` (`a_fatal_scratch_error_ends_verify_as_failed_without_missing_records` red when the failure is moved to the destination side: write the test for both sides); the sweep's tick drops the stashed fault (`Fault::Lost` half red).
- [ ] **Step 6: Gate and commit.** `just check` exit 0; commit `feat: verify, the sweep and the dry restart stop on a fatal walk error`.

---

## Task 7: `--scratch-dir`, the error lines and a CI-size integration test

**Files:**
- Modify: `crates/flux-cli/src/main.rs` (`VerifyArgs` `:76-108`, `CopyArgs` `:126-197`, `run_config` `:382-405`, `verify_options` `:830`, `refusal_line` `:1051`, the verify paths `:847-1050`)
- Modify: `crates/flux-cli/src/exit_code.rs` only if `VerifyError::Failed` needs a mapping there
- Create: `crates/flux-core/tests/walk_spill_std_fs.rs`
- Test: `crates/flux-cli/tests/copy.rs`, `crates/flux-cli/tests/verify.rs`

**Interfaces:**
- `--scratch-dir PATH` on `copy` (all of `copy`, `copy --dry-run`, several sources) and `verify`: doc line "Where a directory too large to sort in memory is spilled (default: the operation's workspace for a copy, else the system temporary directory, or the user cache directory when that is memory-backed). Never refused as memory-backed when given." `run_config` copies it into `RunConfig::scratch_dir`; `verify_options` into `VerifyOptions::scratch_dir`.
- `VerifyError::Failed { error }` prints `error: <code>: <message>` (the format of the other verify error lines) and exits `exit_code::FAILED`, not `USAGE`; with `--json` the summary object is printed as `stopped()` does (`main.rs:862-874`).
- `crates/flux-core/tests/walk_spill_std_fs.rs` (its own binary): `a_flat_directory_of_300k_entries_walks_in_order_with_the_production_constants`: 300,000 zero-byte files made with `std::fs::File::create` in a `TempDir`, `walk_with(&StdFileSystem, root, .., Some(&space))` with `Thresholds::default()` and the space placed on a second `TempDir` through `temporary_home(.., Some(that), ..)`; the yielded names equal the sorted list, `walk.spilled_directories() == 1` (Task 4), and the scratch file length is 0 after the walk. Spec 6.5 asks for this on the CI runners.

- [ ] **Step 0: Verify the state.** The cited CLI lines; `run_config` sets `dry_temp_root: Some(std::env::temp_dir())` at `main.rs:402`. Else STOP.
- [ ] **Step 1: Write the failing tests.**
  - `crates/flux-cli/tests/copy.rs`: `scratch_dir_is_used_by_a_spilling_copy`: a source directory of 9,000 empty files (above `inline_entries`, so the root spills with the production constants) copied with `--scratch-dir <tempdir>/missing` (a path that does not exist): exit 1 and stderr names `<tempdir>/missing` (proves the flag reaches the engine; a flag that is ignored would spill into the workspace and exit 0). A second run with `--scratch-dir <tempdir>` (existing, empty): exit 0, 9,000 files copied, and `<tempdir>` is empty afterwards (the file was nameless).
  - `crates/flux-cli/tests/copy.rs`: `a_dry_run_that_does_not_spill_ignores_an_unusable_scratch_dir`: a 3-file tree with `--dry-run --scratch-dir <tempdir>/missing`: exit 0 (the location is needed only by a spill).
  - `crates/flux-cli/tests/verify.rs`: `verify_uses_scratch_dir_when_it_spills`: two identical 9,000-file trees with `--scratch-dir <tempdir>/missing`: exit 1 naming the path; with an existing directory: exit 0, all matched.
  - The integration test above.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-cli -p flux-core --no-fail-fast scratch_dir 300k` Expected: red (unknown argument; missing test target).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** `cargo nextest run --workspace --no-fail-fast` Expected: all pass; note the 300k test's wall time in the report as UNVERIFIED (no timing is claimed).
- [ ] **Step 5: Gate and commit.** `just check` exit 0; commit `feat: --scratch-dir for copy and verify`.

---

## Task 8: Gates, Windows, docs (controller; GATE runs at top level)

**Files:**
- Modify: `docs/phase3-measurements.md` (new section "Cut 23 plan 2: the walk's spill")
- Modify: `docs/superpowers/specs/2026-10-30-bounded-listing-design.md` (the five deviations above, each with "AMENDED 2026-10-31 by plan 2" and its reason)
- Modify: `TODO.md` (`flux cleanup`'s sweep walks with no scratch; the Windows G5 confirmation if CI has not run)
- Create: `crates/flux-core/tests/walk_cascade_memory.rs` (GATE-C)

This task is run by the controller: it measures, and only the top level can guarantee nothing else runs (`~/.claude/CLAUDE.md`, timing discipline; RSS is not timing, but a concurrent build would still inflate the box's memory pressure).

- [ ] **Step 1: GATE-C (library).** `walk_cascade_memory.rs`, its own binary with the counting allocator of `crates/flux-core/tests/dry_run_memory.rs:19-68`: one `#[test] #[ignore]` (2,000,000 files is too slow for CI) that builds directories of 1,000,000 and 2,000,000 zero-byte files, walks each with `Thresholds::new(512, 4_096, 8, 16_384, 65_536)` (488 runs at 2M, two cascade levels), asserts the names equal the sorted list, and asserts `peak_2m <= peak_1m + 1 MiB` and that `LIVE` is back at its pre-walk baseline once the walk and the space are dropped (a leak on drop does not move the peak). Run: `cargo nextest run -p flux-core --run-ignored only --no-fail-fast walk_cascade` Expected: pass; record both peaks.
- [ ] **Step 2: Build the release binary** of this branch into `~/.cache/flux-c23-target`: `CARGO_TARGET_DIR=$HOME/.cache/flux-c23-target cargo build --release -p flux-cli`.
- [ ] **Step 3: Field check** as plan 1 Task 1 step 2 (`ps`, kill stale measurements, prove the probe sees a `yes` process).
- [ ] **Step 4: GATE-A.** For N in 500,000, 1,000,000, 2,000,000 (fixtures `~/.cache/flux-rss-flat-N`; make a `--dirs 1` variant with `python3 benches/phase3/make_tree.py small --files N --dirs 1 --bytes 0 ~/.cache/flux-rss-dirs1-N`): `--dry-run`, a fresh `copy`, and `verify` over identical trees, each `/usr/bin/time -v` twice, plus `RssAnon`/`RssFile` from `/proc/<pid>/smaps_rollup` sampled at the peak (a 50 ms sampler loop on the pid). Pass: marginal slope 1M to 2M at most 5 B per entry, `RssAnon` slope at most 1 B per entry, and the `-vv` "listing spilled" line counted (at least one per run). If any slope fails, STOP and return to the owner with the numbers (spec section 7 stop rule).
- [ ] **Step 5: GATE-X.** One `--dry-run` over a 10,000,000-entry flat fixture: peak RSS within 64 MiB of the 2M figure; 39 runs reported at `-vv`.
- [ ] **Step 6: Windows.** Ask the owner for a push. With it: push, read the `Test (windows-latest)` job of `ci.yml:171-185`, and add the G5 confirmation (a `#[cfg(windows)]` test in `dir_control.rs` that drops the handle and asserts the directory listing is unchanged) if the job is green. Without it, record "Windows not run" in the report and in `TODO.md`.
- [ ] **Step 7: Record and commit.** The measurement section (method, binary and commit, fixtures, raw KiB, slopes, spill counts, what was not controlled), the spec amendments, the `TODO.md` lines; `typos` on the three files; commit `docs: gates and spec amendments for the walk's spill` with the trailer.

---

## Self-audit (done by the plan's author)

- Spec coverage: 6.1 (location chain, nameless file, `tmpfs` refusal, write failure fatal): Tasks 1, 2, 5. 6.2 (items 1-8, tick, fatal, progress line): Tasks 3, 4, 5, 6. 6.5 (tiny thresholds, run-file-empty assertion, allocation assertion as peak live bytes, CI-size test): Tasks 2-4, 7, 8. 6.6 (`flux-fs`, `walk`, CLI, `FaultFs` scratch fault): Tasks 1, 4, 7, 2. 7 (GATE-A, C, X): Task 8. 8 (failure table rows 1, 2, 4, 5, 7): Tasks 2, 5, 6, 1. Deferred to plan 3: 6.3, 6.6 `names` and `FaultFs.ino`, GATE-B. GATE-D needs no new work (plan 1 bounded structure D) and is measured with GATE-A in Task 8 step 4 only if the owner asks; it is listed here so its absence is visible.
- Placeholders: none. Choices of mine, open to the owner's veto: little-endian length, the file-type byte values, `CopyStep::Resolve` for a scratch error, the 1 MiB GATE-C bound, `--scratch-dir`'s doc line, the Windows 24-hour sweep folded into `temporary_home`.
- Type consistency: `Thresholds` (Task 2) is read by Tasks 3-8; `ScratchSpace<'h>`/`Region` (Task 2) by 3-6; `Tick<'t>` and `WalkError.fatal` (Task 4) by 5 and 6; `temporary_home` (Task 5) by 6 and 7.
- Open risk: deviation 2 changes when an unlistable source root fails (after the lock). It is pinned by two tests in Task 5 and needs the owner's acceptance before Task 5 runs.

## Stand-downs

- DISCARDED-BELOW-FLOOR (panel round 2, agy Q3): a spilled directory of 8,193 to 65,535 entries makes no tick call. Unreachable as a lapsed lock, because `Session::beat` only writes after `HEARTBEAT_INTERVAL` of 5 s (`crates/flux-core/src/run/mod.rs:108`, `run/session.rs:80`), and `walk_into` beats before every directory (`tree.rs:1247`), so the gap is one listing of under 65,536 entries plus its sort.
- DISCARDED-BELOW-FLOOR (panel round 2, agy Blindspot): a scratch error in a copy is reported at step `resolve`. The message itself names the scratch location (Task 2, every scratch error carries `shown`), so the operator is not misled about where it failed. `CopyStep::Resolve` is documented as "any failure that belongs to no copy step" (`crates/flux-core/src/copy.rs:47-49`).
