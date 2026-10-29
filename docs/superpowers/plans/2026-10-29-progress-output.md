# Progress Output Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy` shows a live progress display on a terminal (spec section 52), with no change to anything it prints when nobody is looking.

**Architecture:** A `Progress` bundle of relaxed atomics lives in `flux-core`; the engine ticks it from the places it already counts (`TreeOutcome` methods, the chunk loop). A `Display` in `flux-ui` samples it every 250 ms and draws with `indicatif`; a small `Live` type in `flux-cli` decides whether to show anything and routes record lines above the bar.

**Tech Stack:** Rust 1.98.1 workspace, `indicatif` 0.18 (read at tag 0.18.6), `cargo nextest`, the `FaultFs` fake for engine tests.

**Spec:** `docs/superpowers/specs/2026-10-28-progress-output-design.md` (panel GREEN; read its sections 3, 4, 6 and 7 first).

## Global Constraints

- The gate is `just check` (fmt-check, clippy `-D warnings`, typos, unused-deps (cargo shear), doc-check, licence headers, nextest). Run `cargo fmt` before it. Long runs go in the background. CI also runs `cargo deny check` (`just deny`).
- Every new `.rs` file starts with the five-line header used by `crates/flux-cli/src/lib.rs` (copyright, licence, URL), or the `license` recipe fails.
- Never write a short git sha in a document or commit body (the typos gate); use full 40-hex shas.
- A run whose stderr is not a terminal, every `--json` run and every `--dry-run` must print exactly what it prints today (spec G2, G3). Every existing test is the oracle for that and must stay green unedited, apart from the literal and signature edits this plan names.
- Tests are non-vacuous: each new test is proven red against a logic mutant of the guarded line (named in the task), never a signature break.
- Nextest: `-p <crate> --no-fail-fast` (nextest stops at the first failure otherwise).
- Windows and macOS are read from CI after the push; nothing here is `cfg`-gated except what a task says.

## Rulings made while planning (each with the cost if wrong)

- PR-1. The carrier of the three file counters is `TreeOutcome` (a `progress` field plus `note_*` methods), NOT `Shared` as spec 3.3 says: `finish_copy` (28 callers) and the walk's `files_total` sites hold `out`, not `cx`. Bytes ride a trailing `progress: Option<&Progress>` parameter on `stage_file` and `copy_file_checkpointed`, read from `out`. Cost if wrong: a later parallel engine that splits `out` per worker must move the handle to a shared context.
- PR-2. The spec lists one `files_resumed` site (`tree.rs:1918`); there are two (`tree.rs:3021`, in `link_one`, a resumed symlink). Both tick `resumed`.
- PR-3. `render` draws the single-file bar itself as a text line (`[#####...] 25%`); `indicatif` only draws and suspends. Cost if wrong: the glyph bar is replaced by an indicatif style later, `render` unchanged elsewhere.
- PR-4. `Live::start` takes a `plain: fn(&str)` (the CLI passes its `err`) so the disabled path is unit-testable. The spec's signature had no such parameter.
- PR-5. The single-file path's non-checkpoint branch calls `copy_file_checkpointed(.., None, ..)` mapped to the outcome instead of `copy_file_guarded`, which is that call by definition (`copy.rs:1175-1187`) and has 15 callers this keeps untouched.
- PR-6. `Display::start` returns `Option<Display>` (`None` when the target is hidden or the thread cannot start), and `Display::refresh` is `pub` so tests redraw without sleeping.

## Review Focus (spec silences the plan closes with a test)

1. A failed write in the middle of a large file leaves the bytes already written counted (Task 3, test T5): the display must not claim nothing moved.
2. A `--resume` counts resumed files as `resumed`, not `copied`, and adds no resumed bytes (Task 2, T3 resume).
3. A zero-length file, zero elapsed time, zero bytes: no divide by zero, no percent (Task 4, T1).
4. A hidden (non-terminal) target builds no display and prints record lines exactly as today (Task 5, T7/T8).
5. Several sources: counters keep advancing across roots and equal the sum of the roots (Task 2, T3 several).

---

### Task 1: The `Progress` counters (`flux-core`)

**Files:**
- Create: `crates/flux-core/src/progress.rs`
- Modify: `crates/flux-core/src/lib.rs` (add `pub mod progress;` after `pub mod prior;`, line 17)

**Interfaces:**
- Produces:
```rust
#[derive(Debug, Default)]
pub struct Progress { /* six AtomicU64: discovered, copied, resumed, errors, bytes, expected_bytes */ }
impl Progress {
    pub fn new() -> std::sync::Arc<Progress>;
    pub fn add_discovered(&self);
    pub fn add_copied(&self);
    pub fn add_resumed(&self);
    pub fn add_error(&self);
    pub fn add_bytes(&self, n: u64);
    pub fn set_expected_bytes(&self, n: u64);   // 0 means "unknown"
    pub fn snapshot(&self) -> Snapshot;          // each field loaded once, Relaxed
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub discovered: u64, pub copied: u64, pub resumed: u64, pub errors: u64, pub bytes: u64,
    pub expected_bytes: Option<u64>,             // None when never set or set to 0
}
```
Every store and load is `Ordering::Relaxed`.

- [ ] **Step 1: Write the failing tests** in `progress.rs` `mod tests`:
  `a_new_progress_is_all_zero` (`snapshot() == Snapshot::default()`); `each_add_moves_only_its_own_field` (a table of five closures `add_discovered .. add_error` plus `add_bytes(7)`; after one call the snapshot equals the expected struct, every other field 0); `add_bytes_accumulates` (3 then 4 gives 7); `a_zero_expected_size_is_unknown` (`set_expected_bytes(0)` gives `None`, `set_expected_bytes(9)` gives `Some(9)`); `counters_are_shared_across_threads` (four threads each call `add_discovered` 1000 times on one `Arc`; the snapshot says 4000).
- [ ] **Step 2:** `cargo nextest run -p flux-core --no-fail-fast -E 'test(progress::)'` fails to compile (module absent).
- [ ] **Step 3: Implement** the type above in `progress.rs` with the licence header and a doc comment saying the engine bumps and a display samples. Add the module line to `lib.rs`.
- [ ] **Step 4:** the same command; expected `5 tests run: 5 passed`. Mutants, each turns its named test red and is reverted: `add_copied` bumping `resumed`; `snapshot` mapping `0` to `Some(0)`.
- [ ] **Step 5: Commit** `feat: Progress counters for a live display`.

---

### Task 2: Tick the file counters from the engine (`flux-core`)

**Files:**
- Modify: `crates/flux-core/src/run/mod.rs` (`RunConfig` at line 75, its `Debug` impl at line 107, `tree_with`'s `let mut out = TreeOutcome::default();` at line 321), `crates/flux-core/src/run/multi.rs` (`let mut out = TreeOutcome::default();` at line 726; the `RunConfig` literal at line 1428), `crates/flux-core/src/tree.rs` (`TreeOutcome` at line 40; the sites below), `crates/flux-core/src/run/tests.rs` (the `RunConfig` literal at line 33, new tests)
- Modify (one literal each, add `progress: None`): `crates/flux-core/tests/replace_std_fs.rs:37`, `dry_run_several_std_fs.rs:44`, `dry_run_prior_std_fs.rs:57`, `dry_run_std_fs.rs:44`, `hardlinks_std_fs.rs:50`, `dry_run_memory.rs:89`, `safety_std_fs.rs:137`, and `crates/flux-cli/src/main.rs:359` (`progress: None` for now; Task 5 sets it). The compiler is the oracle for any literal this list missed; a literal with a `..base` needs nothing.

**Interfaces:**
- Consumes: `Progress` (Task 1).
- Produces: `RunConfig.progress: Option<Arc<Progress>>` (`pub`; the manual `Debug` prints `progress: bool`); `TreeOutcome.progress: Option<Arc<Progress>>` (`pub`, so the CLI's `..TreeOutcome::default()` literals still compile); and on `TreeOutcome` three `pub(crate)` methods `note_discovered(&mut self)` (does `files_total += 1`), `note_copied(&mut self)` (`files_copied += 1`), `note_resumed(&mut self)` (`files_resumed += 1`), each also calling the matching `Progress::add_*` when the handle is `Some`.

The bump sites (verified 2026-10-29): `out.files_total += 1;` at `tree.rs:976, 1211, 1255, 1278` become `out.note_discovered();`; `out.files_resumed += 1;` at `tree.rs:1918` (a file) and `tree.rs:3021` (a symlink in `link_one`) become `out.note_resumed();`; `out.files_copied += 1;` at `tree.rs:4324` (the `Ok` arm of `finish_copy`) becomes `out.note_copied();`. `bytes_resumed` and every other counter line stay as they are. `tree_with` and `trees` create their `TreeOutcome` as `TreeOutcome { progress: cfg.progress.clone(), ..TreeOutcome::default() }`; `copy_tree` (`tree.rs:405`) and `verify.rs:738` keep `default()`.

- [ ] **Step 1: Write the failing tests** (`run/tests.rs`, using `fake()` (line 73), `run_tree(&fs, &c)` (line 83), `ok(&r)`; for the config `RunConfig { progress: Some(p.clone()), ..cfg() }`):
  `a_clean_tree_copy_ticks_the_counters_to_the_outcome` (after the run, `p.snapshot()` has `discovered == out.files_total`, `copied == out.files_copied`, `resumed == 0`, and both are greater than 0); `a_resume_ticks_resumed_not_copied` (model it on the test asserting `(out.files_resumed, out.bytes_resumed) == (1, 1)` at `run/tests.rs:3734`: `resumed == out.files_resumed`, `copied == out.files_copied`); `a_resumed_symlink_ticks_resumed` (the symlink resume case of `link_one`; if no existing fixture resumes a link, build one from the neighbouring `link_one` resume test and say so in the report); and in `run/multi.rs` `trees_tests` `several_roots_tick_one_shared_progress_to_the_sum` (use `crashed_late()`/`two()`/`go_with`: a fresh two-root run ends `discovered == files_total` and `copied == files_copied`; a `resume_cfg` resume of `crashed_late()` ends `resumed == files_resumed >= 1` and `copied + resumed == the number of files in the tree`). Add `TreeOutcome` counters are untouched without a handle: `a_run_without_a_handle_counts_as_before` (default `cfg()`, the outcome's counters unchanged and no panic).
- [ ] **Step 2:** `cargo nextest run -p flux-core --no-fail-fast -E 'test(ticks) | test(without_a_handle)'` fails to compile.
- [ ] **Step 3: Implement** the field, the `Debug` line, the three methods, the seven site edits, the two `TreeOutcome` constructions and the literal edits above.
- [ ] **Step 4:** the same command passes; then `cargo nextest run -p flux-core -p flux-cli --no-fail-fast` and expect every test passing (the count is the number before this task plus the new tests; the baseline is 1993 tests at the start of cut 20). Mutants, each red on a named new test and reverted: `note_discovered` not ticking `Progress`; `note_resumed` at `tree.rs:3021` replaced by a bare `out.files_resumed += 1;`; `note_copied` ticking `add_resumed`; `trees` setting no handle.
- [ ] **Step 5: Commit** `feat: the engine ticks a live Progress from its own counters`.

---

### Task 3: Bytes inside a file, and a single file's size (`flux-core`)

**Files:**
- Modify: `crates/flux-core/src/copy.rs` (`stage_file` at line 797, the chunk loop at lines 943-967, `copy_file_checkpointed` at line 1193, its call of `stage_file` at line 1205, `copy_file_guarded`'s call of `copy_file_checkpointed` at line 1175; tests' calls at `copy.rs` lines 1463, 1583, 1797, 3899, 4527, 4593, 5431 (`stage_file`) and 4160, 4432 (`copy_file_checkpointed`)), `crates/flux-core/src/tree.rs` (`stage_file` call at line 3647; `copy_file_checkpointed` calls at lines 2124 and 2271), `crates/flux-core/src/run/mod.rs` (`file_with`, the `copied` block at lines 805-860)

**Interfaces:**
- Consumes: `Progress` (Task 1), `TreeOutcome.progress` and `RunConfig.progress` (Task 2).
- Produces: a trailing `progress: Option<&Progress>` parameter on `stage_file` and on `copy_file_checkpointed`; `copy_file_guarded` and `copy_file` keep their signatures and pass `None`. In the chunk loop, directly after `bytes_copied += n as u64;` (line 957): `if let Some(p) = progress { p.add_bytes(n as u64); }`.

Call sites pass: the tree's calls (`tree.rs:3647`, `2124`, `2271`) `out.progress.as_deref()` (clone the `Arc` once per file if the borrow checker needs it); every test call `None`; `copy_file_guarded` `None`; `run::file`: see below.

`run::file` (single file): in `file_with`, before the `copied` block, read the source length with `fs.metadata(src)` (the `log_free` test reads it only for an adopted prior, so read it here regardless); when `cfg.progress` is `Some(p)` and `!cfg.resume` and the length is greater than 0, call `p.set_expected_bytes(len)`. Both branches then call `copy_file_checkpointed(.., cfg.progress.as_deref())`: the log-free branch as it does now with `Some(Hook::LogFree{..})`, and the other branch as `copy_file_checkpointed(.., None, cfg.progress.as_deref()).map(|(outcome, _)| outcome)` in place of `copy_file_guarded` (PR-5).

- [ ] **Step 1: Write the failing tests:**
  in `copy.rs` tests (use `stage_with`'s pattern at line 1457 and `fail_write_nth`, as `no_job_for_a_buffer_whose_write_failed_body` at line 1923 does): `a_failed_write_leaves_the_bytes_already_written_counted` (a source of several buffers; `fs.fail_write_nth("flux-partial", K, StorageFull)`; after the error, `p.snapshot().bytes == (K - 1) as u64 * copy_buffer_len(len) as u64` and `copied == 0`); `a_clean_stage_counts_every_byte` (`bytes == len`);
  in `run/tests.rs`: `a_single_file_copy_ticks_bytes_and_sets_the_expected_size` (`run::file` through the existing single-file fixture with a handle: `bytes == len`, `expected_bytes == Some(len)`, `discovered == copied == resumed == 0`); `a_zero_length_single_file_has_no_expected_size` (`expected_bytes == None`); `a_resumed_single_file_has_no_expected_size` (`cfg.resume` true over an adopted prior: `expected_bytes == None`); and a tree check inside `a_clean_tree_copy_ticks_the_counters_to_the_outcome` (Task 2) extended with `bytes == out.bytes_copied`.
- [ ] **Step 2:** `cargo nextest run -p flux-core --no-fail-fast -E 'test(bytes_already_written) | test(counts_every_byte) | test(single_file_copy_ticks) | test(expected_size)'` fails to compile.
- [ ] **Step 3: Implement** the parameter, the bump, the call-site edits, and the `file_with` change above.
- [ ] **Step 4:** the same command passes, then `cargo nextest run -p flux-core -p flux-cli --no-fail-fast` all green. Mutants (red on a named test, reverted): the bump moved after the loop (red on the failed-write test: `bytes` 0); the bump counting `n` twice; `set_expected_bytes` called on a resume; `copy_file_checkpointed`'s `stage_file` call passing `None` (red on the single-file test).
- [ ] **Step 5: Commit** `feat: count bytes per chunk and a single file's size for the live display`.

---

### Task 4: The renderer and `Display` (`flux-ui`)

**Files:**
- Modify: `crates/flux-ui/Cargo.toml`, `crates/flux-ui/src/lib.rs`; `Cargo.toml` (root: remove `"flux-ui"` and `"indicatif"` from `[workspace.metadata.cargo-shear] ignored` at line 118, drop the comment lines above it that explain them, and drop `ignored-paths` if `just unused-deps` then passes without it)
- Create: `crates/flux-ui/src/render.rs`, `crates/flux-ui/src/display.rs`

**Interfaces:**
- Consumes: `flux_core::progress::{Progress, Snapshot}`.
- Produces (in `flux-ui`): `pub use indicatif::ProgressDrawTarget;`
```rust
pub fn render(s: &Snapshot, elapsed: std::time::Duration) -> Vec<String>;
pub struct Display { /* bar, progress, started, stop flag, sampler thread */ }
impl Display {
    pub fn start(progress: std::sync::Arc<Progress>, target: ProgressDrawTarget) -> Option<Display>;
    pub fn refresh(&self);               // render now and force a draw
    pub fn println(&self, line: &str);   // one record line above the bar (ProgressBar::suspend)
    pub fn finish(self);                 // stop and join the sampler; clear the bar
}
```
Dependencies: `flux-ui` gets `flux-core = { path = "../flux-core" }` and `indicatif = { workspace = true }`; dev-dependency `indicatif = { workspace = true, features = ["in_memory"] }`.

`render` rules (exact, spec 3.2). A line is `format!("{label:<14}{value}")`. Counts use `indicatif::HumanCount` (`123,481`), bytes `indicatif::HumanBytes` (`4.2 GiB`), speed `format!("{}/s", HumanBytes(bytes_per_second))` with `bytes_per_second = bytes / elapsed_seconds` and the value `-` when `elapsed` is zero or `bytes` is zero. When `expected_bytes` is `Some(total)` the frame is `Transferred` (`{done} / {total}`), `Progress` (`[` 20 cells of `#` then `.` `]` and ` {pct}%`, `pct = min(bytes * 100 / total, 100)`), `Speed`, `ETA` (`-` when speed is `-`, else `(total - min(bytes, total)) / speed` seconds written as `1h 2m 3s` / `2m 3s` / `3s`). Otherwise the frame is `Discovered`, `Processed` (`min(copied + resumed, discovered)`), `Transferred`, `Speed`, then `Resumed` only when `resumed > 0`, then `Errors` only when `errors > 0`.

`Display::start` returns `None` when `target.is_hidden()`. Otherwise it builds `ProgressBar::with_draw_target(None, target)` with the template `{msg}`, draws one frame synchronously, spawns a thread named `flux-progress` that every 250 ms calls `refresh` until the stop flag is set (wake it with `Thread::unpark` on `finish`), and returns `None` if the spawn fails. `finish` and `Drop` set the flag, unpark, join and discard a join error, then `finish_and_clear`.

- [ ] **Step 1: Write the failing tests** (`render.rs`, `display.rs`): T1 `render_shapes` (table of snapshots with their exact expected lines: all-zero; a folder with `discovered 123481, copied 90000, resumed 8412, bytes 4.2 GiB worth, errors 0` at 14 s; `resumed`/`errors` lines absent at 0 and present at 1; `processed` clamped to `discovered` when `copied + resumed` exceeds it; speed `-` at zero elapsed and at zero bytes; the single-file frame at 0, 25 and 100 percent, `bytes > total` clamped to 100, and the ETA text for 3 s, 123 s and 3723 s); T2 with `indicatif::InMemoryTerm::new(20, 80)` and `ProgressDrawTarget::term_like_with_hz(Box::new(term.clone()), 255)`: `start_draws_the_folder_frame` (`term.contents()` contains `Discovered` after `start`), `refresh_shows_new_counts` (bump the `Progress`, `refresh()`, contents contain the new `Discovered` value), `println_puts_the_line_above_the_bar` (contents have the record line before `Discovered`), `finish_clears_the_bar` (after `finish`, contents contain no `Discovered`), `a_hidden_target_starts_no_display` (`start(.., ProgressDrawTarget::hidden())` is `None`).
- [ ] **Step 2:** `cargo nextest run -p flux-ui --no-fail-fast` fails to compile.
- [ ] **Step 3: Implement** as specified, with the licence header on both new files and `lib.rs` declaring the two modules and the re-export.
- [ ] **Step 4:** `cargo nextest run -p flux-ui --no-fail-fast` all green (`render_shapes` plus the five display tests, 6 tests); `just unused-deps` passes; `cargo deny check` passes or its only new finding is named in the report. Mutants (red on a named test, reverted): the `processed` clamp removed; the percent clamp removed; `Resumed` shown at zero; `finish` not clearing; `start` returning `Some` on a hidden target.
- [ ] **Step 5: Commit** `feat: flux-ui renders and draws the live progress display`.

---

### Task 5: `Live` and the CLI wiring (`flux-cli`)

**Files:**
- Create: `crates/flux-cli/src/live.rs`
- Modify: `crates/flux-cli/src/lib.rs` (add `pub mod live;` in order), `crates/flux-cli/Cargo.toml` (`flux-ui = { path = "../flux-ui" }`; dev-dependency `indicatif = { workspace = true, features = ["in_memory"] }`), `crates/flux-cli/src/main.rs` (`copy` at line 443, `copy_several` at line 650, `run_config` at line 358)

**Interfaces:**
- Consumes: `flux_ui::{Display, ProgressDrawTarget}`, `flux_core::progress::Progress`.
- Produces:
```rust
pub fn display_wanted(dry_run: bool, json: bool) -> bool;          // !dry_run && !json
pub struct Live { /* Option<Display>, Option<Arc<Progress>>, plain: fn(&str) */ }
impl Live {
    pub fn start(enabled: bool, target: ProgressDrawTarget, plain: fn(&str)) -> Live;
    pub fn progress(&self) -> Option<std::sync::Arc<Progress>>;     // None when no display runs
    pub fn record(&self, line: &str, counted_failure: bool);        // display.println else plain(line); a counted failure also add_error()
    pub fn finish(self);
}
pub fn counts_as_failure(cause: &flux_core::TreeFailureCause) -> bool;
```
`counts_as_failure` is true for `Walk`, `CreateDir`, `Copy`, `PublishedWithComplaints` and `ClaimNotRecorded`, false for `SpecialFileSkipped`, `SymlinkSkipped`, `MountSkipped` and `Planned` (the variants of `tree.rs:214-245`, all exported as `flux_core::TreeFailureCause`).
`Live::start` makes a `Progress`, then `Display::start`; if `enabled` is false or the display is `None` it is the plain Live (no handle, no thread).

Wiring in `main.rs`: `run_config` gains a `progress: Option<Arc<Progress>>` parameter or the caller sets `cfg.progress = live.progress()` (either; the caller must pass `Live::start(display_wanted(args.dry_run, args.json), ProgressDrawTarget::stderr(), err)`), created AFTER the pre-engine checks and immediately before the engine call in `copy` (tree job and file job) and `copy_several`. The `on_report` closures at `main.rs:492` and `main.rs:694` call `live.record(&line, counted)` for each line of `report::record_lines(&f)`, where `counted` is `counts_as_failure(&f.cause)` for the first line of a record and false for any further line of the same record. `live.finish()` runs immediately after the engine call returns and BEFORE `tree_output` (or the file job's summary lines). The file job's `err(...)` lines after the run stay `err`.

- [ ] **Step 1: Write the failing tests** (`live.rs` `mod tests`, with a `fn plain(line: &str)` pushing into a `static Mutex<Vec<String>>` and `InMemoryTerm` targets): T7 `display_wanted_truth_table` (the four combinations: only `(false, false)` is true); T8 `record_prints_above_the_bar_and_counts_a_failure` (an enabled Live over `term_like_with_hz(.., 255)`: after `record("x", true)` the term contents contain `x`, and `progress().unwrap().snapshot().errors == 1`; `record("y", false)` leaves `errors` at 1), `finish_clears_the_bar_before_the_next_write` (after `finish`, contents have no `Discovered`), `a_disabled_live_writes_exactly_the_lines_it_is_given` (`start(false, ..)`: `progress()` is `None`, `record` calls `plain` once per call with the exact line, no thread), `a_hidden_target_is_the_plain_path` (`start(true, ProgressDrawTarget::hidden(), plain)`: `progress()` is `None`, records go to `plain`), and `counts_as_failure_matches_the_failure_tally` (one assertion per `TreeFailureCause` variant above; build the error values the way the `report.rs` tests do).
- [ ] **Step 2:** `cargo nextest run -p flux-cli --no-fail-fast -E 'test(live::)'` fails to compile.
- [ ] **Step 3: Implement** `live.rs` and the wiring above.
- [ ] **Step 4:** `cargo nextest run -p flux-cli -p flux-core -p flux-ui --no-fail-fast` all green (every CLI test runs with a piped stderr, so none builds a display: that is T6, and its proof is that no CLI test was edited). Mutants (red on a named test, reverted): `display_wanted` ignoring `json`; `record` not adding the error; `finish` called after the summary in `copy_several` (verified by reading the diff, since no CLI test has a terminal; state that in the report); `Live::start` ignoring `enabled`; `counts_as_failure` true for `ClaimNotRecorded`'s neighbour `MountSkipped`.
- [ ] **Step 5: Commit** `feat: flux copy shows live progress on a terminal`.

---

### Task 6: Documents, the full gate, the real-terminal check

**Files:**
- Modify: `README.md` (the `flux-ui` row and the copy section), `ROADMAP.md` (phase 8 row: progress for `copy` done, logging and `verify` progress not built), `TODO.md` (close the item at line 242 with the cut; add the follow-ups: the verifying-phase counter (spec B7), the `--quiet`/logging cut, the `verify` display, `Hardlinks`/`Degraded` lines), `docs/accepted-boundaries.md` (B1-B8 of the spec if the file's convention takes them)

- [ ] **Step 1:** edit the four documents; keep the tracker-status format (`docs/tracker-format.md`).
- [ ] **Step 2:** `cargo fmt && just check` in the background; expected exit 0. Then `just deny`.
- [ ] **Step 3:** the owner's terminal check (not an agent's): `flux copy` of a large tree and of one large file on a real terminal; confirm the frame refreshes, a failure line appears above the bar, and the summary prints after the bar clears. Windows console and macOS are read from CI.
- [ ] **Step 4: Commit** `docs: progress output for flux copy`.

---

## Self-review (run by the author)

- **Spec coverage:** G1 -> Tasks 3, 4 (T5, refresh); G2 -> Task 5 (T6 by absence of edits, T7, T8); G3 -> Task 5 wiring order; G4 -> Task 1 (Relaxed) and Task 2 (no waits); G5 -> Task 1; G6 -> Tasks 2, 3 (one, several, resume, single file); G7 -> Tasks 4, 5. Spec 3.3 table rows: discovered/copied/resumed -> Task 2, bytes -> Task 3, errors -> Task 5. Spec 3.4 -> Task 3. Spec 3.5/3.5a -> Task 5. B1-B8 -> Task 6 documents. T1-T8 map to Tasks 4, 5, 2, 3.
- **Deviations from the spec, all in the rulings above:** PR-1 (carrier), PR-2 (second resumed site), PR-3 (text bar), PR-4 (`plain` parameter), PR-5, PR-6. The spec's Stand-downs and boundaries are unchanged by them; the spec gets a one-line note under section 3.3 in Task 6 pointing here.
- **Type consistency:** `Progress`/`Snapshot` names and the `add_*`/`set_expected_bytes`/`snapshot` methods are used identically in Tasks 2-5; `Display::{start, refresh, println, finish}` and `Live::{start, progress, record, finish}` match their callers.
- **Placeholders:** none. Items left to the implementer by design and bounded by the compiler: the exact `RunConfig`/`stage_file` call-site edits beyond the named lists (compiler oracle), the borrow shape of `out.progress.as_deref()` at `tree.rs:3647`, and the construction of error values in the `counts_as_failure` test.
- **Proportion:** the plan is shorter than the code it describes; bodies appear only for `render`'s exact formats and the call wiring.
