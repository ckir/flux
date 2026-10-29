# Logging and `--quiet` (cut 21) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `-v`, `-vv`, `-vvv` and `--quiet` on `flux copy`, `flux verify` and `flux cleanup`, built on `tracing`, plus the polished summary line of `flux copy`.

**Architecture:** The engine (`flux-core`) emits `tracing` events through one small module (`oplog`): `INFO` events with target `flux::op` for the `-v` verb lines, `DEBUG`/`TRACE` events for `-vv`/`-vvv`. `flux-cli` installs one global subscriber whose formatter renders the verb lines and whose writer routes lines to the live display (`Display::println`) or stderr. `--quiet` is a gate on the CLI's existing direct prints plus a level filter; the existing prints are not moved onto `tracing`.

**Tech Stack:** Rust workspace; `tracing` 0.1, `tracing-subscriber` 0.3 (minimal features), `clap` 4.6 derive, `indicatif` 0.18 (via `flux-ui`), `cargo nextest`, `just check` and `just deny` as the gates.

**Spec:** `docs/superpowers/specs/2026-10-29-logging-quiet-design.md` (approved 2026-10-29, commit e309394). Section numbers below (S5.3 and so on) refer to it.

## Global Constraints

- Failure line text stays `CODE: path: message`; exit codes and every `--json` byte are unchanged by `-v`/`-q` (G5, N3).
- A formatter or writer panic aborts the shipped binary (`Cargo.toml:110`, `panic = "abort"`): no `unwrap`, `expect` or indexing in them (S5.5).
- `-v`/`-q` conflict is checked by `main` after parsing, NOT by clap `conflicts_with` (S5.1); exit code 2 (`exit_code::USAGE`).
- Verb column: `{verb:>10} {path}` (S5.2). Sizes: `indicatif::HumanBytes`. Elapsed: `0.35s` / `12.3s` / `2m 03s` / `1h 02m 03s` (S6).
- Only events whose target starts with `flux` are rendered (S5.1). No timestamps, colour, log file or `RUST_LOG` (N1).
- The workspace entry `Cargo.toml:68` itself is changed (members cannot remove features); `just check` and `just deny` must pass at every commit that touches dependencies.
- Timing measurements (Task 7) need the owner's go-ahead, an idle machine, the main thread only and no agents.

## Rulings I made while planning (cost if wrong)

- PR-1. Verb selection: `finish_copy` (`tree.rs:4360`, 28 occurrences of the name) serves files, symlinks and replaced files and has no kind argument. A `pub(crate) verb: Verb` hint on `TreeOutcome`, set by the few call sites that are not a plain copy and consumed (reset to `Copied`) by `finish_copy`, avoids touching 28 call sites. Cost if wrong: a missed `set` prints `Copied` for a link or replace; Task 4's tests pin each.
- PR-2. A single file's `path` is the destination path as `file_with` receives it (`resolve::job` output), not the string the user typed: `flux-core` never sees the typed form (S5.2 says "as typed"). Cost: a longer path on one line.
- PR-3. The capture helper for tests is a hand-written `tracing::Subscriber` in `oplog::testing`, so `flux-core` needs no `tracing-subscriber` dev-dependency.
- PR-4. `Verbosity` lives in a process-global `OnceLock` (set once in `main`), read by `note()`/`note_lines()`; tests of pure functions pass it as an argument. Cost: a test that sets it twice fails loudly.
- PR-6. A single-file copy prints `Copied` for a published file and `Skipped` for a skipped one; a replaced single file also prints `Copied` in this cut, because `file_with` does not learn whether the target existed (the CLI does, in `Job::File.target_existed`, `main.rs:509`). Cost: one verb less precise on the one-file path; a follow-up can pass the flag in.
- PR-5. Control characters are escaped in `Live::record`'s display branch (the one place lines reach `Display::println`), not inside `flux-ui`.

## Review Focus

Inputs the spec implies and no ordinary test would reach, most likely first; each has its test in the task named.
1. A file named with `\x1b[2K`, `\r`, `\n` or a non-UTF-8 byte, copied under `-v` on a terminal and on a pipe: no raw control byte reaches the stream (Task 5).
2. stderr a pipe whose reader has gone (every write fails with EPIPE): the copy finishes, exit code unchanged, no panic (Tasks 5 and 6).
3. `flux -q copy -v`, `flux copy -v -q`, `flux verify -vq`, `flux -v copy -q`: all exit 2 with the same message (Task 2).
4. Every non-zero exit still prints a cause under `--quiet`: unreadable file (1), `SAFETY_REJECTED` containment (3), usage error (2), `verify` mismatch (1), `cleanup` refused (3) (Task 3).
5. One file, one directory, zero files, zero bytes, and a 4 GiB size in the summary line (Task 1); a `--dry-run -v` that adds no verb lines (Task 6).

---

### Task 1: Summary helpers and the polished summary line

**Files:**
- Modify: `crates/flux-ui/src/lib.rs:15` (re-export `HumanBytes`), `crates/flux-cli/src/report.rs` (`summary_line` at 483, `dry_run_summary_line` just below it, the tests that pin the old text: 13 matches in this file), `crates/flux-cli/tests/copy.rs` (1 match), `crates/flux-cli/tests/dry_run.rs` (5 matches), `README.md` (any quoted summary line).
- Test: `crates/flux-cli/src/report.rs` (`mod tests`), the two test files above.

**Interfaces:**
- Produces: `flux_ui::HumanBytes`; in `report.rs`: `pub fn count_noun(n: u64, one: &str, many: &str) -> String` (`1 file`, `0 files`), `pub fn human_bytes(n: u64) -> String`, `pub fn elapsed_text(ms: u64) -> String`.

- [ ] **Step 1: Write failing tests** in `report.rs` `mod tests`: `count_noun_is_singular_only_for_one` (`count_noun(1,"file","files") == "1 file"`, `0 -> "0 files"`, `2 -> "2 files"`); `human_bytes_matches_the_progress_display` (`human_bytes(15) == "15 B"`, `human_bytes(1_500) == "1.46 KiB"`, and for 0 and `4 << 30` equal to `HumanBytes(n).to_string()`); `elapsed_text_formats` with exactly these rows: `0 -> "0.00s"`, `350 -> "0.35s"`, `9_994 -> "9.99s"`, `12_300 -> "12.3s"`, `59_949 -> "59.9s"`, `59_950 -> "1m 00s"`, `60_000 -> "1m 00s"`, `123_000 -> "2m 03s"`, `3_599_500 -> "1h 00m 00s"`, `3_723_000 -> "1h 02m 03s"`. The rules: under 10 000 ms two decimals; from 10 000 to 59 949 ms one decimal; otherwise round to whole seconds first, then `"{m}m {s:02}s"` when that is under 3600 s, else `"{h}h {m:02}m {s:02}s"`. Update the expected text in the 13 existing summary tests and add `summary_line_reads_correctly_for_one_file_and_one_directory` (`copied 1 file (2 B), created 1 directory in 0.01s; 0 failed, 0 skipped`) and `summary_line_with_failures_and_suffixes_keeps_the_order` (suffix clauses unchanged after `; N failed, M skipped`).
- [ ] **Step 2: Run** `cargo nextest run -p flux-cli report::` — expect FAIL (helpers undefined).
- [ ] **Step 3: Implement** the three helpers and rewrite `summary_line(r: &Report, directories_created: u64) -> String` to `copied {files} ({size}), created {dirs} in {elapsed}; {failed} failed, {skipped} skipped` plus the unchanged suffix loop; elapsed comes from `r.duration_ms`. Rewrite `dry_run_summary_line` with `count_noun` and `human_bytes` only (no time; `overwrite`, `skip`, `errors` labels unchanged). Add `pub use indicatif::HumanBytes;` to `flux-ui/src/lib.rs`.
- [ ] **Step 4: Fix the CLI-level expectations** found by `rg -n "bytes\), created|would copy [0-9]+ files" crates README.md` (copy.rs, dry_run.rs, README) and run `cargo nextest run -p flux-cli` — expect PASS.
- [ ] **Step 5: Commit** `feat: the summary line reads correctly, shows time and human sizes`.

### Task 2: `Verbosity` and the shared flags

**Files:**
- Create: `crates/flux-cli/src/verbosity.rs`; Modify `crates/flux-cli/src/lib.rs` (add `pub mod verbosity;`), `crates/flux-cli/src/main.rs`: `Cli` (27-31), `VerifyArgs.verbose` (field at 83, doc line above it), `main()` (291-298), the two `route_record(&r, args.json, args.verbose)` calls (853 in `verify_one`, 944 in `verify_several_cli`), the parse test that reads `bare.verbose` (1071) and `all.verbose` (1084).
- Test: `crates/flux-cli/src/verbosity.rs` (`mod tests`), `main.rs` `mod tests`.

**Interfaces:**
- Produces: `pub struct Verbosity { pub level: u8, pub quiet: bool }` (`Copy`), `Verbosity::from_flags(verbose: u8, quiet: bool) -> Result<Verbosity, String>` (level clamped to 3; `Err("-v/--verbose and -q/--quiet cannot be used together")` when both), `Verbosity::shows_notes(self, dry_run: bool) -> bool` (`!quiet || dry_run`), `pub fn set(v: Verbosity)`, `pub fn get() -> Verbosity` (default `{level: 0, quiet: false}` when unset).

- [ ] **Step 1: Failing tests.** `verbosity.rs`: `from_flags_clamps_to_three`, `from_flags_rejects_both`, `shows_notes_truth_table` (4 rows). `main.rs`: `v_and_q_conflict_in_every_position` — `Cli::try_parse_from` is accepted by clap for `["flux","-q","copy","-v","a","b"]`, so assert via the new `fn verbosity_of(&Cli) -> Result<Verbosity,String>` that these all give `Err`: `copy -v -q`, `-q copy -v`, `-v copy -q`, `verify -vq`, `cleanup -v -q`; `flags_are_global_both_positions` (`-v copy a b` and `copy -v a b` give level 1); `verbose_long_form_is_the_same_count` (`--verbose` twice = 2); `vvvv_is_accepted_as_three`; update the `verify` parse test to read `verbosity.level`.
- [ ] **Step 2: Run** `cargo nextest run -p flux-cli verbosity main::tests` — FAIL.
- [ ] **Step 3: Implement.** In `Cli` add `#[arg(short = 'v', long = "verbose", action = ArgAction::Count, global = true)] verbose: u8` and `#[arg(short = 'q', long = "quiet", global = true)] quiet: bool` (NO `conflicts_with`: S5.1); remove `VerifyArgs.verbose`; `main()` parses, calls `verbosity_of`, prints `error: {msg}` through `err` and returns `exit_code::USAGE` on `Err`, otherwise `verbosity::set` and dispatches. Replace the two `args.verbose` reads with `verbosity::get().level >= 1`. Help text of `-v` per S5.2 (verify prints match records on stdout; copy and cleanup add per-item lines on stderr).
- [ ] **Step 4: Run** `cargo nextest run -p flux-cli` — PASS.
- [ ] **Step 5: Commit** `feat: -v, -vv, -vvv and --quiet flags shared by the three commands`.

### Task 3: `--quiet` gating

**Files:**
- Modify: `crates/flux-cli/src/verify_report.rs` (`route_record` at 242 gains the `quiet` argument; its eleven test calls at 532, 534, 540, 548, 549, 550, 552, 553, 561, 567 and 572 get `, false`), `crates/flux-cli/src/live.rs` (`display_wanted` at 18, `Live` struct, `for_copy` at 63, `record_failure` at 97, the tests), `crates/flux-cli/src/main.rs` (`copy` 444, `tree_output` 589, `copy_several` 658, `cleanup` 758 with `cleanup_stdout` 737, `verify_one` 816, `verify_several_cli` 903; helpers beside `err` at 1019).
- Create: `crates/flux-cli/tests/quiet.rs`.

**Interfaces:**
- Consumes: Task 2 (`verbosity::get`, `Verbosity::shows_notes`).
- Produces: `verify_report::route_record(r: &Record, json: bool, verbose: bool, quiet: bool) -> Routed` (callers `main.rs:853` and `:944`); `live::display_wanted(dry_run: bool, json: bool, quiet: bool) -> bool`; `Live::for_copy(dry_run: bool, json: bool, quiet: bool, target: ProgressDrawTarget, plain: fn(&str), progress: &mut Option<Arc<Progress>>) -> Live`; `fn note(line: &str, dry_run: bool)` and `fn note_lines(v: Vec<String>, dry_run: bool)` in `main.rs` (print through `err` unless `!verbosity::get().shows_notes(dry_run)`).

Classification applied to the existing prints (S5.3; `K` keep, `H` hide under `--quiet` unless `--dry-run`): `err(&a.error...)`, `abort_hint_lines`, `run_lines`, `tree_warning_lines`, complaint/identity-warning lines, `checkpoint_lines`'s `checkpoints discarded` lines, stop and usage errors, `Report`/JSON: **K**. `resume_lines`, `file_resume_verify_lines`, `resume_verify_lines`, the partial-resume lines and the advisory note of `checkpoint_lines`/`file_checkpoint_lines`, `root_lines`, `summary_line`: **H**. `dry_*` lines, `unprobed_lines`, `planned_file_line`: **K**. Because `checkpoint_lines` (`report.rs:874`, used at `main.rs:627`) and `file_checkpoint_lines` (`report.rs:895`, used at `main.rs:556` on the single-file path) mix kinds, split each: `report::checkpoint_warning_lines` / `report::checkpoint_note_lines` and `report::file_checkpoint_warning_lines` / `report::file_checkpoint_note_lines` (the `checkpoints discarded` lines are the warnings; the partial-resume lines and the advisory note are the notes); keep the two old functions as the concatenation of their halves so their tests (`report.rs:1677-1815`) stand. Streamed records: `Live::record_failure` hides `SymlinkSkipped`, `SpecialFileSkipped`, `MountSkipped` under `--quiet`, keeps failure causes and `Planned`. `verify` (decided per streamed record, so no buffering is needed): under `--quiet`, `match` and `extra` records are not printed; `missing`, `mismatched` and unreadable records print as they stream (each implies exit 1); the human summary, per-root lines and `dest_absent`/`root_absent` informational lines are not printed except where they explain a non-zero exit; stderr notices (mount not entered, unreadable extra) are kept; `--json` is untouched. `cleanup`: table hidden when exit is 0 and not `--dry-run` and not `--json`.

- [ ] **Step 1: Failing tests.** `live.rs`: update `display_wanted_truth_table` (`live.rs:145-148`) to 8 rows including `quiet`, update `Live::start(display_wanted(dry_run, json), ...)` inside `for_copy` (`live.rs:70`) and the existing test call `Live::for_copy(dry_run, json, target(&term), plain, &mut slot)` (`live.rs:227`) to the new signature; `quiet_live_hides_skip_records_and_keeps_failures` (`record_failure` of `SymlinkSkipped` prints nothing, of a `Copy` failure prints and counts, of `Planned` prints). `quiet.rs` (process tests, piped stderr, fixtures as in `tests/copy.rs`): `quiet_clean_copy_prints_nothing`, `quiet_failed_copy_prints_the_cause_not_the_summary` (unreadable file: stderr is exactly the one `PERMISSION_DENIED:` line, exit 1; skip when running as root), `quiet_keeps_the_dry_run_output`, `quiet_hides_skip_records_and_resume_notes`, `quiet_keeps_warnings`, `quiet_with_json_leaves_stdout_unchanged`, `quiet_verify_clean_prints_nothing`, `quiet_verify_mismatch_prints_the_records_only`, `quiet_verify_ignores_extra_records_at_exit_0`, `quiet_cleanup_clean_prints_nothing`, `every_nonzero_exit_prints_a_cause_under_quiet` (the five scenarios of Review Focus 4).
- [ ] **Step 2: Run** `cargo nextest run -p flux-cli quiet live::` — FAIL.
- [ ] **Step 3: Implement** the signatures above and route each site through `err`/`note`/`note_lines` per the classification. `--quiet` also turns the bar off through `display_wanted`.
- [ ] **Step 4: Run** `cargo nextest run -p flux-cli` — PASS.
- [ ] **Step 5: Commit** `feat: --quiet prints only causes, warnings and dry-run output`.

### Task 4: Dependencies and the engine's events

**Files:**
- Modify: `Cargo.toml` (line 68 entry to `default-features = false` plus the features Task 5's formatter needs, at least `fmt` and `std`; remove `"tracing"`, `"tracing-subscriber"` from `ignored` at 117), `crates/flux-core/Cargo.toml` (+`tracing = { workspace = true }`), `crates/flux-cli/Cargo.toml` (+`tracing`, `tracing-subscriber`), `crates/flux-core/src/lib.rs` (`pub mod oplog;`), `crates/flux-core/src/tree.rs` (`TreeOutcome` and its `note_*` block at 120-150; `enter_dir` 1457, the `Ok(child)` arm at ~1542; `copy_one` 1781: `note_resumed` 1959, `note_skipped` 2010, the Replace arm's `finish_copy` at 2341 then `record_replace` at ~2346; `count_linked` 2433 and its callers 2153 and 2301; `link_one` 2896: `note_resumed` 3064, `note_skipped` 3112, replaced link 3253, its `finish_copy` return at 2974; `flush_batch` 3748: `finish_copy` at 3922, `files_overwritten` 3927; `finish_copy` 4360 with `note_copied` at 4370), `crates/flux-core/src/run/mod.rs` (`file_with` 729, just before `run.copy = Some(copied)` at 895), `crates/flux-core/src/cleanup/delete.rs` (`Action::Removed` pushes at 252, 324, 503, 515, 536 and the callback at 578).
- Create: `crates/flux-core/src/oplog.rs`.
- Test: `crates/flux-core/src/oplog.rs` (`mod tests`), `crates/flux-core/src/run/tests.rs`.

**Interfaces:**
- Produces: `pub enum Verb { Created, Copied, Linked, Hardlinked, Replaced, Resumed, Skipped, Removed }` with `fn as_str(self) -> &'static str` (lowercase action word); `pub const TARGET: &str = "flux::op"`; `pub fn emit(verb: Verb, path: &Path, bytes: Option<u64>, to: Option<&Path>)` (an `INFO` event, target `flux::op`, fields `action`, `path` (already escaped), `bytes`, `to` (escaped)); `pub fn escape_path(p: &Path) -> String` (control characters, `\x1b`, `\r`, `\n` as `\u{..}`; non-UTF-8 bytes as `\xNN`; printable Unicode kept; `/` separators); `pub mod testing { pub struct OpEvent { pub action: String, pub path: String, pub bytes: Option<u64>, pub to: Option<String> } pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<OpEvent>) }` (`#[cfg(test)]`, a hand-written `tracing::Subscriber`, PR-3). `TreeOutcome` (which derives `Default`, `tree.rs:39`) gains `pub(crate) verb: Verb`; `Verb` therefore derives `Default` with `Copied` as the default variant (no `TreeOutcome` struct literal needs touching).

- [ ] **Step 1: Failing tests.** `oplog.rs`: `escape_path_escapes_controls_and_non_utf8` (`"a\x1b[2K"` -> `a\u{1b}[2K`; `"a\nb"` -> `a\u{a}b`; the byte string `b"a\xffb"` on Unix -> `a\xffb`; `"é/x"` unchanged); `emit_is_one_info_event_with_target_flux_op` (via `capture`). `run/tests.rs`, on the fixtures that file already has: `fake()` (`run/tests.rs:74`: `/src/a` = `A`, `/src/sub/b` = `BB`), `run_tree(&fs, &cfg)` and `fs.add_symlink_to(link, target, flux_fs::LinkKind::Unspecified)` (used at `:430`, `:1438`): `tree_copy_emits_created_then_copied_in_walk_order` (on `fake()` plus a symlink `/src/lnk`: events in the order `a` (copied, 1 byte), `lnk` (linked), `sub` (created), `sub/b` (copied, 2 bytes)), `a_symlink_emits_linked_with_its_target` (`to` equals the target string given to `add_symlink_to`), `a_replaced_file_emits_replaced_not_copied` (an `--overwrite` run over an existing file: exactly one event for the path, action `replaced`), `a_replaced_symlink_emits_replaced`, `a_hardlink_member_emits_hardlinked_with_the_anchor` (`to` = the anchor's relative path), `a_resume_emits_resumed_for_a_file_and_a_symlink`, `an_update_skip_emits_skipped`, `a_dry_run_emits_no_op_events`, `a_single_file_copy_emits_copied_with_the_destination_path` (PR-2), `a_failed_file_emits_no_copied_event`. `cleanup`: `cleanup_emits_removed_per_entry_and_none_on_dry_run` in the cleanup tests.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core oplog run::tests::tree_copy_emits cleanup` — FAIL (module and fields missing).
- [ ] **Step 3: Implement** `oplog.rs`; add `verb` to `TreeOutcome` and one `emit` in each site above: `finish_copy`'s Ok arm emits `std::mem::replace(&mut out.verb, Verb::Copied)` with `o.bytes_copied`; the link publication path (`link_one`'s `finish_copy` return) sets `out.verb = Verb::Linked` and its `to` is the link target; the Replace arm in `copy_one` and the `replaced.is_some()` branch in `flush_batch` set `Verb::Replaced` before `finish_copy`; `count_linked` gains `anchor: &Path` (both callers have `anchor` in scope: bindings at 2127 and 2265) and emits `Hardlinked`; `enter_dir` emits `Created` next to `out.directories_created += 1`; the `note_resumed`/`note_skipped` sites emit `Resumed`/`Skipped`; `file_with` emits `Copied` (`Skipped` when `o.skipped`; PR-6) just before `run.copy = Some(copied)` when the result is `Ok`; `delete.rs` emits `Removed` beside each `Action::Removed` push. No emission in `dry_*` functions (`dry_link`, `dry_planned`).
- [ ] **Step 4: Run** `cargo nextest run -p flux-core` and `just check && just deny` — PASS (the lockfile gains `tracing` and `tracing-subscriber`; `deny` licenses must pass).
- [ ] **Step 5: Commit** `feat: the engine emits tracing events for each file, directory, link and removal`.

### Task 5: Formatter, writer, subscriber and escaping on the display

**Files:**
- Create: `crates/flux-cli/src/logging.rs`; Modify `crates/flux-cli/src/lib.rs`, `crates/flux-cli/src/live.rs` (`Live::record`'s display branch at ~83-90, `for_copy`, `finish`), `crates/flux-cli/src/main.rs` (install in `main()` after `verbosity::set`), `crates/flux-core/src/copy.rs` (per-chunk `trace!` in the loop that already calls `add_bytes`, guarded by `tracing::enabled!`), `crates/flux-core/src/tree.rs` (`debug!` in `copy_one` at the plan decision and in `enter_dir`; `trace!` in `record_new` 2370).
- Test: `crates/flux-cli/src/logging.rs` (`mod tests`).

**Interfaces:**
- Consumes: Task 4 (`oplog::TARGET`, event fields), Task 2 (`Verbosity`).
- Produces: `logging::render_op(action: &str, path: &str, bytes: Option<u64>, to: Option<&str>) -> String` (S5.2: `{verb:>10} {path}` then ` ({size})` for `bytes`, ` -> {to}` for `to`; the verb is the capitalised action, `Hardlinked` fits exactly 10); `logging::escape_control(s: &str) -> String`; `logging::install(v: Verbosity)` (global subscriber once; level filter: quiet and default `WARN`, `-v` `INFO`, `-vv` `DEBUG`, `-vvv` `TRACE`; only targets starting with `flux`); `logging::set_display_sink(sink: Option<Arc<dyn Fn(&str) + Send + Sync>>)` and `logging::LineWriter` (a `MakeWriter` that buffers until `\n`, hands whole lines to the sink or stderr, discards I/O errors, takes the sink under a lock held only for the clone).

- [ ] **Step 1: Failing tests.** `render_op_matches_the_design_table` (every row of S5.2: `   Created sub`, `    Copied a (2 B)`, `    Linked lnk -> a`, `Hardlinked b -> a`, `  Replaced a (2 B)`, `   Resumed a (2 B)`, `   Skipped a`, `   Removed .flux-x`); `escape_control_keeps_printable_and_escapes_the_rest`; `line_writer_hands_whole_lines_to_the_sink` (two writes of one event split mid-line produce one sink call); `line_writer_to_a_closed_stderr_does_not_panic`; `sink_swap_during_finish_loses_no_line` (two threads, one writing 1000 lines while the main thread sets and clears the sink; every line appears exactly once in the sink or in the fallback capture); `debug_and_trace_lines_carry_the_level_and_the_path`; `a_hostile_name_reaches_the_sink_escaped` (name `"x\x1b[2Ky"` through `emit` with a subscriber installed on a scoped dispatcher). `live.rs`: `record_escapes_control_characters_on_a_display_line` (a failure line holding `\x1b[2K` shows `\u{1b}[2K` on the in-memory terminal; the same line through the plain path is unchanged), `for_copy_fills_and_finish_clears_the_display_sink`.
- [ ] **Step 2: Run** `cargo nextest run -p flux-cli logging live::` — FAIL.
- [ ] **Step 3: Implement.** The formatter is a `tracing_subscriber::fmt::FormatEvent` (or a `Layer`) that, for target `flux::op`, reads `action`/`path`/`bytes`/`to` from the event and calls `render_op`; for other `flux*` events writes `{level:>5} {module}: {message} key=value ...`; no `unwrap`/`expect`/indexing anywhere in this file (S5.5). `Live::for_copy` registers the sink (a closure over the display handle: `Display` must be shareable; add `Display::handle(&self) -> DisplayHandle` (`Clone + Send + Sync`) in `crates/flux-ui/src/display.rs` wrapping the `ProgressBar` clone) and `finish` clears it before `display.finish()`.
- [ ] **Step 4: Run** `cargo nextest run -p flux-cli -p flux-core -p flux-ui` and `just check` — PASS.
- [ ] **Step 5: Commit** `feat: render the -v lines, route them above the bar, escape names`.

### Task 6: End-to-end and terminal tests

**Files:** Create `crates/flux-cli/tests/verbosity.rs`; Modify `crates/flux-cli/tests/pty.rs` (cut 20's helpers `open_pty`, `trees_in`, `copy_on_a_terminal`).

- [ ] **Step 1: Failing tests** (`verbosity.rs`, piped stderr). `pty.rs::trees_in` has no symlink and no unreadable file, so `verbosity.rs` builds its own `fn sample(d: &Path, with_locked: bool)`: `src/a` = `A\n` (2 bytes), `src/lnk` -> `a`, `src/sub/b` = `BB\n` (3), `src/sub/c` = `C\n` (2), and, when `with_locked`, `src/locked` = `secret\n` with mode 000 (tests that need it return early when the effective uid is 0). Walk order: `a`, `lnk`, `locked`, `sub`, `sub/b`, `sub/c`. The summary line carries a variable elapsed time, so tests compare it with the time masked by a helper `fn mask_elapsed(line: &str) -> String` (replaces ` in <time>;` by ` in T;`): `v_prints_verb_lines_before_the_summary` (S1: five lines in walk order `a`, `lnk`, `sub`, `sub/b`, `sub/c` plus summary), `v_with_a_failure_keeps_the_failure_line_in_walk_order` (S2 exactly as the design shows, `locked` between `lnk` and `sub`), `vv_and_vvv_add_lines_that_carry_paths`, `json_with_v_keeps_stdout_unchanged`, `dry_run_with_v_adds_no_verb_lines`, `verify_v_prints_match_records_on_stdout_and_verbose_is_the_same`, `cleanup_v_prints_removed_lines`, `v_with_a_dead_stderr_pipe_exits_like_without` (stderr is a pipe whose read end was dropped before the child starts, so every write fails with EPIPE; the exit code and the destination tree equal a normal run's), `resume_with_different_verbosity_is_accepted`. `pty.rs`: `v_on_a_terminal_draws_the_bar_and_prints_the_verb_lines_above_it` (transcript holds `Discovered` and `Copied a` and a clear-line before the summary), `quiet_on_a_terminal_draws_no_frame_and_prints_nothing_on_success`.
- [ ] **Step 2: Run** `cargo nextest run -p flux-cli --test verbosity --test pty` — FAIL where behaviour is missing, otherwise PASS.
- [ ] **Step 3: Fix any gap the tests expose in Tasks 3-5 code** (no new design).
- [ ] **Step 4: Prove non-vacuity** by temporary logic mutants and confirm the named new test goes red: drop `format!("{:>10}", ...)` alignment; `live.finish()` moved after the summary; the `-q` gate removed from `display_wanted`; `escape_control` made the identity; the sink slot not cleared. Restore each.
- [ ] **Step 5: Commit** `test: -v and --quiet through the real binary and a pseudo-terminal`.

### Task 7: Docs, measurements and the capstone hand-off

**Files:** Modify `README.md` (commands section, the verify `--verbose` line, `-v`/`-q`), `ROADMAP.md` (Phase 8 row), `TODO.md` (remove the "Logging and `--quiet` ... are not built" entry at the line that now reads so; keep the open progress items), `docs/superpowers/specs/2026-10-28-progress-output-design.md` (N3 gets a one-line "built in cut 21" note).

- [ ] **Step 1: Docs** as above (`TODO.md:248` is the "Logging and `--quiet` (section 54) are not built" entry; remove it, keep the other progress entries); run `just check`.
- [ ] **Step 2: STOP and ask the owner** for the go-ahead and an idle machine before any timing (project timing rules; main thread only, no subagents, two warm runs, range quoted, uncontrolled load stated).
- [ ] **Step 3: Logging-off cost (A4):** on the many-small-files case of `benches/copy.rs`, two warm runs on `main` and two on this branch; the ranges must overlap. Record both ranges in the PR body and in `docs/agy-capstone-ledger.md`'s row.
- [ ] **Step 4: Flicker at `-v` (D2/B8):** copy a large folder (the owner's choice) on a real terminal with `-v`; record elapsed with and without `-v` and whether the bar tears; if the owner finds it unacceptable the fallback is a rate-limited batch of `-v` lines (a follow-up task, not a removal of the bar).
- [ ] **Step 5: Commit** `docs: logging and --quiet`; then AGY-CAPSTONE over `main..HEAD` (source changed), then AGY-TEST-AUDIT, then finishing.

---

## Self-review

- **Spec coverage:** G1 -> Tasks 4, 5, 6; G2 -> Tasks 4, 5; G3 -> Task 3; G4 -> Tasks 4, 5; G5 -> Tasks 2, 3, 6; G6 -> Task 1; N1 -> Global Constraints and Task 4's `Cargo.toml` step; N3 -> unchanged failure text pinned by the existing tests; A1-A5 -> Tasks 2 (exit 2), 3 (causes kept), 5 (no panic, escaping), 7 (benchmark); S5.4's shutdown rule -> Task 5's `sink_swap_during_finish_loses_no_line`; S9's list is spread over Tasks 1-6; calls 1-16 are implemented where their sections are.
- **Placeholders:** none. Two decisions are left to the implementer with a stated default: the exact `tracing-subscriber` feature list (Task 4 says at least `fmt` and `std`; the formatter decides what else), and the `-vv`/`-vvv` event wording (Task 5 fixes the sites and the line shape).
- **Type consistency:** `Verbosity` (Task 2) is the only verbosity type; `display_wanted`/`for_copy` gain `quiet` in Task 3 and `for_copy` also fills the sink in Task 5 (same signature); `oplog::emit`'s fields are the ones `render_op` reads.
- **Known gaps for the capstone:** the `-vv`/`-vvv` bytes are not stable by design; Windows and macOS behaviour of the pty test is unchanged (Linux only); the single-file path is the engine's, not the typed one (PR-2).
