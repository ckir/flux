# Progress output for `flux copy` (cut 20, B1)

Date: 2026-10-28. Status: draft for panel review. Spec reference: `FLUX_FULL_UPDATED_SPEC_V16.md` section 52 (lines 2768-2802). Tracker: `TODO.md:242`.

## 1. Intent

A long `flux copy` runs silently today: per-record failure lines as they happen and one summary at the end (`TODO.md:242`). On a terminal, the operator should see the copy advance, including inside one very large file, without the display ever changing what the copy does or what it prints when nobody is looking.

### Goals

- G1. On a terminal, files and bytes advance on screen at least every 250 ms during a copy, including inside ONE large file.
- G2. A run whose stderr is not a terminal, every `--json` run and every `--dry-run` produces output byte-identical to today (stdout, stderr and exit code).
- G3. The final summary, the record lines, the `--json` object and the exit codes are unchanged.
- G4. The display only observes. No copy-correctness path changes behaviour; no lock, WAL, heartbeat or claim path waits on the display.
- G5. Memory stays bounded: no file list is kept (spec section 52, "Progress must not require the entire file list in RAM").
- G6. It works for one file, one folder, several sources, and a `--resume` of any of them.
- G7. It is testable without a terminal.

### Non-goals (each is a later cut, not an oversight)

- N1. `flux verify` has no progress display.
- N2. No pre-scan of the sources, so no percent or ETA for a folder (a single file has both, section 3.4).
- N3. No `--progress` flag and no `--quiet` flag. The spec names neither `--progress` anywhere, and `--quiet` belongs to the logging section (spec section 54, lines 2860-2871; also line 750 and 5833), which is a separate unbuilt cut. The display follows one automatic rule (section 3.5).
- N4. The `Hardlinks`, `Degraded` and `Capacity wait` lines of spec section 52 are not shown. Capacity planning is not built (`ROADMAP.md` phase 9). `Hardlinks` and `Degraded` are counted at `tree.rs:2391`, `2393` and `4330`, and a live copy of those would be three more sites for lines that are zero in almost every run.
- N5. No smoothed or windowed speed; no parallel-worker handling (the engine is sequential; the counters are atomics so a later parallel engine needs no redesign).
- N6. No `tracing` setup, no log levels.
- N7. No handling of Ctrl-C beyond what the process does today (section 6, B3).

### Anti-goals (a pass on the tests that still fails the cut)

- A1. Any byte on stdout or stderr that today's non-terminal run does not print.
- A2. A copy that fails, stalls or panics because the terminal is closed, full or resized.
- A3. A display that shows motion that is not real (a counter that moves while nothing is copied).

## 2. Approaches (the owner's call on 2026-10-28 was "dive in"; the forks below were consulted with agy first, review-only)

The consult files are `.clavity/seams/progress-fork.md` (round 1, `[VERDICT: ALIGNED]`) and `.clavity/seams/progress-negotiate.md` (the scope negotiation the owner asked for, `[VERDICT: ALIGNED]`). Cites below were read and measured by the driver, not taken from the peer.

| Fork | Chosen | Alternatives and why not |
|---|---|---|
| F1 totals | No pre-scan. A folder shows running counters; a single file shows a bar with percent and ETA (its size is one `metadata` call). | A pre-scan walk before the copy gives percent and ETA for a folder at the price of a whole extra metadata pass and a total that goes stale if the tree changes; a background counter races the copy. Deferred (N2). |
| F2 plumbing | Shared atomic counters (`Arc<Progress>`), bumped with `Ordering::Relaxed`, sampled by a UI thread every 250 ms. The engine never learns a display exists. | A callback per file and per chunk puts display latency in the copy loop and needs a thread-safe multiplexer once workers exist. A channel of events lets a slow terminal back-pressure the copy. |
| F3 rendering | `indicatif` 0.18 (already in the workspace manifest), with a hidden draw target off a terminal. | A hand-rolled `\r` redraw: no dependency, but terminal width, resize, Windows consoles and interleaving with record lines all become ours. |
| F4 when shown | Automatic only (section 3.5). | `--progress=auto\|always\|never` and `--quiet`: agy first recommended both, then withdrew them after reading the spec (N3). I proposed `--progress` and withdrew it for the same reason. |
| F5 slicing | One cut: `copy` (one or several sources, `--resume`). | `verify` as well (N1); counters without in-file bytes: fails G1. |

Agy and I diverged on one line in the negotiation: I wanted a `Skipped` line for resumed files; agy's final list omits it. Ruling R1 (section 7) keeps it, shown only when non-zero, at the cost of one extra counter site.

## 3. Design

### 3.1 The counters (`flux-core`, new module `progress`)

```rust
/// What a running copy has done so far. The engine bumps it; a display samples it. Never blocks, never errors.
#[derive(Debug, Default)]
pub struct Progress { /* AtomicU64 fields */ }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub discovered: u64,        // non-directory entries the walk has yielded
    pub copied: u64,            // files published by this run
    pub resumed: u64,           // files a prior run completed and this one verified and skipped
    pub errors: u64,            // records counted as failures (3.3)
    pub bytes: u64,             // bytes this run has written, per chunk
    pub expected_bytes: Option<u64>, // Some only for a single-file copy that is not a resume (3.4)
}
```

Methods: `Progress::new() -> Arc<Progress>`, `add_discovered()`, `add_copied()`, `add_resumed()`, `add_error()`, `add_bytes(n: u64)`, `set_expected_bytes(n: u64)`, `snapshot() -> Snapshot`. Every store and load is `Relaxed`. A snapshot reads each field once and is NOT a consistent cut; the renderer clamps (3.2).

`Processed` (the spec's word for the unknown-total shape) is `copied + resumed`, clamped to `discovered`.

### 3.2 The renderer (`flux-ui`, new module)

- `render(snapshot: &Snapshot, elapsed: Duration) -> Frame` is a pure function. `Frame` holds the lines to show; it is the only code with display text, so it is the unit-tested part.
- Folder / several sources / unknown total (spec section 52, lines 2796-2801):

```text
Discovered   123,481
Processed     98,412
Transferred   4.2 GiB
Speed         312 MiB/s
Resumed       1,204        (only when resumed > 0)
Errors        3            (only when errors > 0)
```

- Single file with a known size (section 3.4): `indicatif`'s bar with `Transferred`, percent, `Speed`, `ETA`.
- Speed is `bytes / elapsed` over the whole run so far (N5). With `elapsed == 0` or `bytes == 0` the speed line shows `-`. `Transferred` counts bytes THIS run wrote, never bytes a resume skipped (so a resume's speed does not jump).
- Clamps: `processed = min(copied + resumed, discovered)`; a single-file percent is clamped to 100.
- `Display` owns an `indicatif::ProgressBar` and one sampler thread. `Display::start(progress: Arc<Progress>, target: ProgressDrawTarget) -> Display`; `Display::println(&self, line: &str)` prints one record line above the bar (`ProgressBar::suspend`, which runs the closure even when the target is hidden); `Display::finish(self)` clears the bar and joins the thread. `Drop` does the same, so an unwind or an early return cannot leave the thread or the bar behind.
- The sampler wakes every 250 ms, reads `snapshot()`, renders, and updates the bar. Every draw error is ignored (indicatif discards them: `state.rs:69,89,149,176,185,189`, `progress_bar.rs:443` at tag 0.18.6), so a closed or full stderr never fails the copy (A2).

### 3.3 Where the engine bumps the counters (`flux-core`)

`RunConfig` gains `pub progress: Option<Arc<Progress>>` (`None` outside the CLI's terminal case). The copy context (`Shared`, `tree.rs:441`) and the file copy function (`copy.rs`, the chunk loop at `copy.rs:943-967`) carry a `Option<&Progress>`.

| Counter | Site (verified 2026-10-28) | Rule |
|---|---|---|
| `discovered` | the four statements `out.files_total += 1;` (`tree.rs:976, 1211, 1255, 1278`) | bump together with `files_total`, through one helper, so the two cannot drift |
| `bytes` | `bytes_copied += n as u64;` (`copy.rs:957`) | per buffer (at most `COPY_BUF_MAX`), after the write succeeds |
| `copied` | `out.files_copied += 1;` in `finish_copy` (`tree.rs:4324`) | on the `Ok` arm only |
| `resumed` | `out.files_resumed += 1;` (`tree.rs:1918`) | beside it |
| `errors` | the CLI's `on_report` closure (`main.rs:492, 694`), not the engine | a record counts when `FailureTally` counts it: `Walk`, `CreateDir`, `Copy`, `PublishedWithComplaints`, `ClaimNotRecorded`; not `SpecialFileSkipped`, `SymlinkSkipped`, `MountSkipped` |

`bytes` counts a partial that a later failure discards, so the live `Transferred` can exceed the final `bytes_copied` after a failure. It is exact in a clean run (G6, test T3). Hardlinked, symlinked, excluded and special entries are in `discovered` and in no other counter, so `Processed` can end below `Discovered`; the display is cleared before the summary, which is the authority.

### 3.4 A single file

`run::file` (the single-file path) sets `expected_bytes` from the source's length, which it already reads, and only when `cfg.resume` is false: a resumed single file writes fewer bytes than its length, so a percent against the full length would never reach 100. A resumed single file shows the counters shape.

### 3.5 When the display runs (the CLI, `copy` and `copy_several`)

The display is created in exactly one condition: `!args.dry_run && !args.json && stderr is a terminal`, AFTER the pre-engine checks (`selection_of`, `resolve::job` / `resolve::several`) and immediately before the engine call, so a usage error or a refusal before the engine never starts one. The terminal test is `ProgressDrawTarget::stderr()`'s own (hidden when stderr is not a terminal or `TERM` is `dumb`, `draw_target.rs:80`); when that target is hidden the CLI treats the display as absent and takes the plain path, so the non-terminal run is today's code path, not a hidden bar.

- Record lines: the `on_report` closure calls `display.println(&line)` when a display exists, else `err(&line)` as today. The closure also calls `progress.add_error()` for a counted record.
- The display is finished (bar cleared, thread joined) BEFORE `tree_output` prints the summary and the warning lines, so those print exactly as today.
- Several sources: one `Progress` for the whole operation, held across roots, so the counters advance monotonically; `cfg.progress` is set once.

### 3.6 What does not change

The `--json` object, the stdout records, the exit codes, the summary text, `flux verify`, the engine's behaviour with `progress: None` (all existing tests), and the WAL, claim, lock and heartbeat paths.

## 4. Tests

- T1 (`flux-ui`, headless): `render` over a table of snapshots: empty; folder shape; `resumed` and `errors` lines appear only when non-zero; `processed` clamps to `discovered`; speed `-` at zero elapsed; single-file shape with percent clamped to 100; thousands separators. Each row asserts the exact lines.
- T2 (`flux-ui`, `indicatif` `InMemoryTerm`, feature `in_memory`, dev-dependency only): `Display` over an in-memory terminal draws the folder frame after one tick; `println` puts the line above the bar; `finish` leaves the bar cleared; a hidden target runs `println` through (the closure still runs) and draws nothing.
- T3 (`flux-core`, fake filesystem): a clean tree copy with a `Progress` ends with `discovered == files_total`, `copied == files_copied`, `bytes == bytes_copied`, and the same for a several-source copy (sum over roots) and for a `--resume` (`resumed == files_resumed`, `bytes` excludes the resumed bytes). Mutants: dropping any one bump site turns a named assertion red.
- T4 (`flux-core`): the `None` path: the existing suites are the oracle (nothing changes).
- T5 (`flux-core`): in-file motion: a large single file's `bytes` is non-zero BEFORE `finish_copy` runs (read from inside the fault-fs hook between two chunk writes), which is G1's in-file clause.
- T6 (`flux-cli`): with stderr piped (every CLI test), no bar escape codes and no extra lines appear (the existing `copy`, `several_sources` and `run` suites are the oracle); `--json` and `--dry-run` runs construct no display.
- T7 (`flux-cli`, unit): the display-condition function over (dry_run, json, is_terminal) truth table.
- Non-vacuity: every test is proven red against a logic mutant of the guarded line (drop the bump, swap `copied` and `resumed`, remove the clamp, remove the `finish` before the summary).
- Not testable here: a real terminal and the Windows console. Covered by the CI matrix for the compile and the headless tests; the on-screen look is an owner check.

## 5. Files and dependencies

- `crates/flux-core/src/progress.rs` (new), `lib.rs` (module), `run/mod.rs` (`RunConfig.progress`), `tree.rs`, `copy.rs`, `run/multi.rs` as needed to carry the handle. Adding a field to `RunConfig` touches every literal of it (about 100 in `run/tests.rs`, `run/multi.rs` and `crates/flux-core/tests/*`); the compiler names them, and the plan lists them.
- `crates/flux-ui/Cargo.toml` (dependencies `flux-core`, `indicatif`; dev-dependency `indicatif` with feature `in_memory`), `src/lib.rs` and new modules.
- `crates/flux-cli/Cargo.toml` (`flux-ui`), `main.rs`.
- Workspace manifest: `indicatif` leaves the `ignored` list of the unused-dependency check; `flux-ui` too. `cargo deny` must pass; the new transitive crates (and `vt100` for the dev feature) are listed by the plan from `cargo tree`, not here.
- Docs: `README.md`, `ROADMAP.md` (phase 8 row), `TODO.md:242` (closed), `docs/accepted-boundaries.md` if a boundary is accepted.

## 6. Accepted boundaries

- B1. The live `Transferred` can exceed the final `bytes_copied` after a failed file (a discarded partial was written). Exact in a clean run.
- B2. `Processed` ends below `Discovered` when links, symlinks, special files or excluded entries are present.
- B3. Ctrl-C and `SIGKILL` end the process as today; the bar's last frame may stay on the terminal. No signal handler is added.
- B4. The speed is an average over the whole run (N5).
- B5. The `Discovered` counter races the final `files_total` by design: it is sampled while the walk runs.
- B6. A resumed single file shows the counters shape, not a percent (3.4).

## 7. Rulings made while writing this (each with the cost if wrong)

- R1. Keep a `Resumed` line (non-zero only), against agy's final list. Cost if wrong: one extra counter and one conditional line, removable without touching the engine's other counters.
- R2. `Errors` is counted in the CLI closure, not the engine, so the engine gains no error hook. Cost if wrong: an error raised without an `on_report` record would not show live; the final summary still reports it.
- R3. `Display` is hidden-aware: the CLI builds no display when the target is hidden, so the non-terminal path is today's code. Cost if wrong: a divergence in how a hidden bar prints records, which G2's byte-identical tests would catch.
- R4. The spec's `Processed` is `copied + resumed` (not including errors). Cost if wrong: the label of one line.

## 8. Exhaustiveness self-audit (writing-plans checklist, run by the author)

- Contracts and shapes: `Progress`, `Snapshot`, `Display`, `render`, the `RunConfig` field, the display condition: all given in sections 3.1-3.5. Field defaults: `Progress::default()` is all zeros; `Snapshot.expected_bytes` is `None`.
- Placeholders: none left as TBD. Items for the PLAN (line-level, written against code after this spec is merged to the branch): the exact `RunConfig` literal list, the exact signatures through `Shared` and the copy function, the `FailureTally`-matching predicate for `errors`, the terminal-width behaviour of the bar template, and indicatif behaviours stated here as facts but to be re-read at the plan stage with `path:line` at the installed tag (the draw-error discards, `suspend` on a hidden target, `InMemoryTerm`).
- Cases: empty source (no entries: shows zeros, then clears); a source that is one empty file; several sources with a refusing root (the display is finished on the error path by `Drop`); `--resume` with every file done (counters show `Resumed` only); `--restart`; `--dry-run` (no display, G2); `--json` on a terminal (no display); a stderr that closes mid-run (A2); a one-column terminal (indicatif truncates; no panic is claimed beyond its own, to be checked at the plan stage).
- Requirement map: G1 -> 3.2, 3.3, T5; G2 -> 3.5, T6, T7; G3 -> 3.5, 3.6; G4 -> 3.1 (Relaxed, no locks), T4; G5 -> 3.1; G6 -> 3.3, 3.4, T3; G7 -> T1, T2.
