# Logging and `--quiet` (cut 21): `-v`, `-vv`, `-vvv`, `--quiet` on `flux copy`, `flux verify` and `flux cleanup`

Spec section 54 (`FLUX_FULL_UPDATED_SPEC_V16.md` lines 2845-2872) names `tracing`, `tracing-subscriber`, five levels and the flags `-v`, `-vv`, `-vvv`, `--quiet`, and nothing else: not what is logged, not where it goes, not what `--quiet` silences, and not how a level maps to a flag. This document fills that in. Date: 2026-10-29. Branch: `feat/logging-quiet`, off `main` at the cut 20 merge (4e80acd).

## 1. Intent

An operator of a file copier wants three things the tool does not give today:

1. To see **what it is doing, file by file**, without reading the source: `-v`.
2. To see **why**, when a run does something surprising: `-vv`, `-vvv`.
3. To make it **say nothing unless something needs attention**, so it can sit in a script or a cron job: `--quiet`.

Cut 20 left a known gap on the third: the live progress display has no off switch except redirecting stderr (progress spec N3, `TODO.md` "Logging and `--quiet` are not built"). This cut closes it, and adds a small polish of the summary line that the owner asked for while looking at the output (section 6).

## 2. Goals, non-goals, anti-goals

Goals
- G1. `-v` prints one line per file, directory and link as the engine finishes it, in the engine's own order, before the unchanged failure lines and the summary.
- G2. `-vv` and `-vvv` print engine diagnostics (plan decisions, identities, chunk and claim events) through `tracing`.
- G3. `--quiet` prints only what an operator needs: the cause of a non-zero exit, warnings, a refusal, and the complete output of a `--dry-run`.
- G4. One mechanism for all three commands and for the single-file path: the engine emits `tracing` events; the CLI installs one subscriber with one formatter.
- G5. The flags do not change exit codes, the `--json` objects, or the text of failure lines.
- G6. The summary line of `flux copy` reads correctly for one file and one directory, shows an elapsed time, and shows human-scaled sizes.

Non-goals (the owner decided or I propose; each is a later cut if wanted)
- N1. No `--log-file`, no JSON log format, no `RUST_LOG` or `FLUX_LOG` environment filter, no timestamps, no colour. The `json` and `env-filter` features of `tracing-subscriber` pinned in `Cargo.toml:68` stay unused.
- N2. No percent or `n/m files` for a folder: that needs a pre-scan, which the progress spec rejected (its N2).
- N3. The failure line text stays `CODE: path: message` (for example `PERMISSION_DENIED: locked: Permission denied (os error 13)`). The codes are the registry's normative identifiers (spec section 55) and about a dozen tests and any user script match them.
- N4. `flux verify` and `flux cleanup` keep their record and table texts and their summaries. For `verify`, `-v` is exactly today's `--verbose` (the `match` records on stdout), `-vv` and `-vvv` add `tracing` diagnostics on stderr and never change the stdout records, and `verify` has no verb lines. For `cleanup`, `-v` adds `Removed` lines on stderr; its table stays on stdout.
- N5. The progress display's frame is unchanged.
- N6. No two-tier quiet (`-qq` silencing the warnings too). D3 chose warnings printing under `--quiet`; a stricter tier is a later cut if operators ask (a cron job that must be silent even with a warning would use the exit code and `--json`).

Anti-goals (a failure even if everything else works)
- A1. A verbosity flag changes an exit code, a `--json` byte, or hides the cause of a non-zero exit.
- A2. A log call panics, blocks or fails the copy because stderr is closed or slow.
- A3. A file name with a control or escape sequence reaches a terminal unescaped (it could erase the progress bar or forge a line). This covers every line written through the display, including the existing failure lines (section 5.6).
- A4. With logging off, the copy gets measurably slower (spec section 76: performance is measured). Checkable: on the many-small-files case of `benches/copy.rs`, two warm runs with and without the logging code compiled in give overlapping ranges (the project's timing rules: owner's go-ahead, idle machine, range quoted, uncontrolled load stated).
- A5. `--quiet` leaves an operator unable to tell why a run exited non-zero.

## 3. Decisions the owner made (in conversation, 2026-10-29)

| # | Decision |
|---|---|
| D1 | `-v` line style "A", action verbs, in Cargo style: the verb capitalised and right-aligned, then the path: `   Created sub`, `    Copied a (2 B)`, `    Linked lnk -> a`. |
| D2 | On a terminal, `-v` keeps the live progress bar at the bottom and prints the lines above it (cut 20's `Live::record` and `ProgressBar::println`). The flicker risk is the accepted B8 of the progress spec; it is to be measured on a large tree before the plan is final (the numbers are recorded in the plan; the owner decides whether they are acceptable, and the fallback is a rate-limited batch of `-v` lines, not removing the bar). |
| D3 | `--quiet` still prints **warnings** (hardlink degradation, weak identity, metadata complaints) and the failure lines, refusals and abort hints; it keeps the complete `--dry-run` output. |
| D4 | Scope: `flux copy`, `flux verify` and `flux cleanup`. `flux verify --verbose` becomes an alias of `-v`. |
| D5 | Approach 1 (section 4): `tracing` everywhere, with a custom formatter in `flux-cli`. |
| D6 | The summary polish: singular and plural, elapsed time, human-scaled sizes. |
| D7 | The failure-line prefix stays `CODE:`; no folder percent (above, N2, N3). |

## 4. Approaches considered

Three ways to produce the `-v` lines and the `-vv`/`-vvv` detail. agy (a peer reviewer) and I consulted on this fork (`.clavity/seams/logging-quiet-approach.md`; reply `.clavity/scratch/logging-quiet-approach/r1-reply.md`); the claims below were measured by reading the code.

- **Approach 1, `tracing` everywhere (chosen).** The engine emits `tracing` events; `flux-cli` installs a subscriber with a custom formatter that renders the `flux::op` events as the verb lines. Cost: no function signature changes; about sixteen emission sites (measured by their counters in `tree.rs`: `note_copied` 1, `note_resumed` 2, `note_skipped` 2, `files_overwritten += 1` 6, `files_hardlinked += 1` 2, plus `enter_dir`, `link_one` and the cleanup deletion; the plan fixes the list); one subscriber setup. Fits `run::file`, `verify` and `cleanup` uniformly.
- **Approach 2, hybrid.** `-v` lines as typed success records through the existing `on_report: &mut dyn FnMut(TreeFailure)` stream; `tracing` only for `-vv`/`-vvv`. Cost: `on_report` appears 201 times and 38 function signatures take it in `crates/flux-core/src/tree.rs`; `run::file` and `cleanup` have no such stream and would need new APIs. Benefit: tests need no subscriber. Rejected on cost, since the scope covers three commands.
- **Approach 3, an event channel in `RunConfig`.** A `Sender<EngineEvent>` the CLI drains. Cost: a new mechanism beside `tracing`, a thread or a poll, and still two output paths. Rejected.

Claims in agy's reply that did not hold when measured, and are not reasons for this choice: that spec section 54 maps `-v` to `info` (the spec gives no mapping; the mapping in section 5 is this document's); that `on_report` being `!Send` blocks the WAL and hashing threads (the `-v` moments happen on the walk thread, and `report()` is synchronous there); that Approach 1 has a single writer to the bar (failure lines still go through `on_report`, in both approaches; they share one thread, so their order is deterministic). Tests in Approach 1 use a scoped subscriber (`tracing::subscriber::with_default`), which is thread-local; events emitted on the WAL or hashing threads are not captured by it and must not be relied on in a test.

## 5. Behaviour

### 5.1 Flags

`-v` is a count (`-v`, `-vv`, `-vvv`, also `-v -v`); `--quiet` (short `-q`) is a flag. They are global (clap `global = true`): accepted by `copy`, `verify` and `cleanup`, defined once, and accepted before or after the subcommand (`flux -v copy ...` and `flux copy -v ...`). No short flag exists in `main.rs` today, so `-v` and `-q` collide with nothing. `--verbose` is the long form of the same count (so `flux verify --verbose` is `-v`, and is `-v` on the other two commands too); the field `VerifyArgs.verbose` and the parse test that reads it (`!bare.verbose`, `all.verbose`) are replaced by the shared verbosity value. `-v` with `--quiet` is a usage error: exit 2, a message naming both. Clap's `conflicts_with` is not enough: measured with clap 4.6.7, it refuses `flux copy -v -q` and `flux verify -vq` but ACCEPTS `flux -q copy -v` (the two flags sit at different levels), so `main` checks `verbosity > 0 && quiet` itself right after parsing and exits 2 with the message (a test pins the cross-position case). The builder must NOT also put `conflicts_with` on the clap attributes: that would make `copy -v -q` fail with clap's own message and `-q copy -v` with ours; `main`'s check is the only one, so the message is the same in every position. `-vvvv` and more are accepted (clap counts 4) and treated as `-vvv`. They are not stored in any operation state; spec line 5833 already lets them change on a `--resume`, and a test pins that a resume with different verbosity is accepted.

Setting | tracing level shown | meaning
---|---|---
`--quiet` | `warn` and `error` | section 5.3
(default) | `warn` and `error` | today's output, unchanged except section 6
`-v` | `info` | adds the verb lines
`-vv` | `debug` | adds engine decisions
`-vvv` | `trace` | adds chunk and claim events

The existing direct prints (failure lines, warnings, the summary, resume notes, the dry-run plan) stay direct prints; they are not routed through `tracing`. The verbosity value gates them where section 5.3 says so. The engine emits no `warn` or `error` events in this cut, so the `warn` filter of the first two rows shows nothing from `tracing`; the value is reserved. The subscriber renders only events whose target starts with `flux` (so a future dependency that uses `tracing` cannot leak into the output).

### 5.2 The `-v` verb lines

Each is one `tracing` `INFO` event with target `flux::op` and fields `action`, `path` and, when it applies, `bytes` and `to`. `path` is relative to the destination root, `/`-separated, as `report::rel` writes it: for one source tree it equals the path below the source root; in a several-source run it starts with the root's name (`alpha/a`), so two roots never print the same path; for a single file it is the destination path as the user typed it. The formatter renders `{verb:>10} {path}` followed by the detail; the verb column is 10 wide (the longest verb, `Hardlinked`).

| `action` | Verb | Line | When the engine emits it |
|---|---|---|---|
| created | `Created` | `   Created sub` | a destination directory was created (not when it already existed) |
| copied | `Copied` | `    Copied a (2 B)` | a file was published (the Ok arm that calls `note_copied`) |
| linked | `Linked` | `    Linked lnk -> a` | a symlink was published |
| hardlinked | `Hardlinked` | `Hardlinked b -> a` | a hardlink member was linked to its anchor |
| replaced | `Replaced` | `  Replaced a (2 B)` | an existing file was atomically replaced (`--overwrite`) |
| resumed | `Resumed` | `   Resumed a (2 B)` | a resume kept a file or symlink a prior run completed (the two `note_resumed` sites) |
| skipped | `Skipped` | `   Skipped a` | the existing-file policy left a file (`note_skipped` sites); a symlink, special file or mount skipped by policy keeps its existing record line |
| removed | `Removed` | `   Removed .flux-x` | `flux cleanup` deleted an entry |

Sizes use the human scale of section 6 (`2 B`, `1.42 GiB`). The root destination directory is not announced; `Created` is for directories below it. The order is the engine's order: sorted by name, so `a`, `lnk`, `locked`, `sub`, `sub/b`, `sub/c` for the sample tree (measured with `--dry-run`).

With `--dry-run`, `-v` adds no verb lines (nothing is created, copied or removed; the plan lines are the output), and `flux cleanup --dry-run -v` adds no `Removed` lines; `-vv` and `-vvv` diagnostics still print. The help text of the shared flag says that on `flux verify` `-v` also prints the `match` records on stdout (today's `--verbose`), and on `copy` and `cleanup` it adds the per-item lines on stderr, so the one difference between the commands is documented where the operator looks.

A failure keeps its existing line, unindented, at the point the engine reports it. Example, the sample tree with one unreadable file, `-v`, stderr:

```text
    Copied a (2 B)
    Linked lnk -> a
PERMISSION_DENIED: locked: Permission denied (os error 13)
   Created sub
    Copied sub/b (3 B)
    Copied sub/c (2 B)
copied 4 files (7 B), created 2 directories in 0.35s; 1 failed, 0 skipped
```

### 5.3 What `--quiet` prints

One rule, stated once: **`--quiet` prints only (a) the cause of a non-zero exit, (b) warnings, (c) the whole output of a `--dry-run`, and (d) a `--json` object on stdout.** Everything else is hidden. As a table:

| Output | default | `--quiet` |
|---|---|---|
| Failure lines (`counts_as_failure` records), refusals, abort hints, `stop_lines`, usage errors | shown | shown |
| Warnings (`tree_warning_lines`: weak identity, hardlink degradation, metadata) | shown | shown |
| `--dry-run`: plan lines, `note:` lines, the dry-run summary | shown | shown |
| `--json` object (stdout) | shown | shown |
| Skip records (symlink, special file, mount skipped by policy; exit 0) | shown | hidden |
| Resume notes (`resume_lines`), the partial-resume lines (`resumed X: reused n bytes, recopied m bytes`), the checkpoint advisory note, and the `--resume-verify=full` level line | shown | hidden |
| `checkpoints discarded for X: ...` (`checkpoint_lines`, `file_checkpoint_lines`): a resume lost reusable work | shown | shown (a warning) |
| The summary line of `copy`, and in a several-source run the per-root lines and the total | shown | hidden |
| `flux verify`, human output (stdout): `match`, `missing`, `mismatched`, `extra` records and the summary | records shown (`match` only with `-v`); summary shown | exit 0: nothing, including `extra` records (`extra` never fails a verify, `verify.rs` `Summary::failed`); exit 1: the `missing`, `mismatched` and unreadable records only, no summary |
| `flux verify` stderr notices (mount not entered, unreadable extra, absent DEST) | shown | shown (they are warnings) |
| `flux cleanup`: the table (stdout) | shown | when the exit is 0 and not `--dry-run`: hidden; otherwise shown |
| The live progress bar | drawn on a terminal | not drawn |

(Streams do not change with `--quiet`: `copy` writes its human output to stderr, while `verify` and `cleanup` write their results to stdout, as they do today. So `flux verify --quiet > /dev/null` discards the explaining records exactly as `flux verify > /dev/null` does; rule (a) is read as "the records that explain a non-zero exit, on the stream the command already uses", and a clean run prints nothing, like a clean `copy`.) With `--json` (any command), `--quiet` changes nothing on stdout: the whole JSON output stays, including the records and the `{"summary": ...}` object of `verify --json`; stderr carries (a) and (b) only. A test enumerates every non-zero exit path of the three commands and asserts that `--quiet` still prints at least one cause line for each (A5).

### 5.4 `-v` on a terminal and the progress bar

`-v`, `-vv` and `-vvv` keep the bar (D2). There is one display per process run (`copy`, `copy_several` and the single-file path each build one `Live` through `Live::for_copy`, shared by all roots). The subscriber is installed first, so its writer reads a process-global slot that `Live::for_copy` fills with a shareable handle to the display and `live.finish()` empties; while the slot is full the writer hands each rendered line to `Display::println`, and before and after that window it writes straight to stderr. The writer discards I/O errors (A2). `MakeWriter` hands a writer arbitrary byte chunks, and `Display::println` takes one line and redraws the bar, so the formatter emits each event as exactly one newline-terminated line in a single write and the writer buffers until it has whole lines, hands each (without its newline) to the display, and never splits one event across two redraws. The writer takes the slot's handle under a lock held only to clone it, never while writing; a line that arrives while `live.finish()` runs, from the walk thread or from the WAL or hashing threads, goes either through the display or straight to stderr, is never lost, never panics and never blocks `finish`; a test drives that from two threads. `--json` and `--dry-run` never draw the bar (cut 20's rule), but `-v` lines still print, plain, to stderr.

### 5.5 `-vv` and `-vvv`

Plain lines on stderr: `{level:>5} {module}: {message} key=value ...`, no timestamp, no colour (N1), for example `debug tree: copy_one path=a size=2 plan=New`. The formatter and the writer never panic: the shipped binary is built with `panic = "abort"` (`Cargo.toml:110`), so a panic there ends the process; no `unwrap`, `expect` or indexing in them, and a test drives them with a failing writer and hostile names. The exact set of events is the plan's to fix, within these rules: every event that concerns a file carries `path` and the same escaping as section 5.6; `trace` events inside the per-chunk copy loop (`copy_file_checkpointed`) are guarded with `tracing::enabled!` so the loop pays one inline check with logging off (A4); no event is emitted while the claim store, WAL or destination lock is held across an I/O that can block on stderr; the exact bytes printed by `-vv`/`-vvv` are not a stable interface and are not pinned by tests beyond "the lines exist and carry `path`".

### 5.6 Escaping

Every path and symlink target that reaches a log line is escaped: control characters, `\x1b`, `\r`, `\n` and a non-UTF-8 byte are written as `\xNN` or `\u{..}` escapes, printable Unicode is left as is. A tracing field cannot carry raw bytes, and `Display` on a path is lossy, so the engine records each path through one helper that returns the already-escaped text (so the non-UTF-8 byte is still visible as an escape, not as U+FFFD), and the formatter escapes control characters in any field value again as a second guard. The same helper serves the verb lines and the `-vv`/`-vvv` lines. Separately, every line written through the display (`Display::println`) has its control characters escaped whatever its origin, which covers the existing failure lines (built with `report::rel`, lossy and unescaped today) while stderr is a terminal; a name without control characters prints byte for byte as before, and a piped stderr is unchanged.

## 6. The summary line (D6)

The summary of `flux copy` and the dry-run summary use one set of helpers (singular/plural, human size, elapsed). Examples, replacing the old forms:

| Old | New |
|---|---|
| `copied 4 files (7 bytes), created 2 directories; 1 failed, 0 skipped` | `copied 4 files (7 B), created 2 directories in 0.35s; 1 failed, 0 skipped` |
| `copied 1 files (2 bytes), created 0 directories; 0 failed, 0 skipped` | `copied 1 file (2 B), created 0 directories in 0.01s; 0 failed, 0 skipped` |
| `dry run: would copy 5 files (14 bytes), overwrite 0, skip 0, create 2 directories; 0 errors` | `dry run: would copy 5 files (14 B), overwrite 0, skip 0, create 2 directories; 0 errors` |

Rules:
- Singular for exactly 1: `1 file`, `1 directory`; plural for 0 and 2+. The count labels `failed`, `skipped`, `errors` do not change; the suffix clauses (`hardlinked`, `verified`, ...) do not change.
- Sizes use the same scale as the progress display (`indicatif::HumanBytes`: `7 B`, `42.50 KiB`, `1.42 GiB`). The exact count stays in `--json` (`bytes_copied`).
- Elapsed time follows the directories clause: `0.35s` under a minute (two decimals under 10 s, one decimal from 10 s), `2m 03s` under an hour, `1h 02m 03s` after. The dry run shows no time (nothing moved). The value is the same wall clock `duration_ms` carries.
- Several sources: the per-root lines (`root alpha: 2 copied, 0 failed`) and the total line follow the same rules; the per-root lines are otherwise unchanged.
- The `flux verify` and `flux cleanup` summaries are unchanged (N4).

## 7. Parts and where they live

| Part | Where | What |
|---|---|---|
| Dependencies | `Cargo.toml` (workspace: `tracing`, `tracing-subscriber` already pinned, but absent from `Cargo.lock` today, so the new crates enter the lock), `crates/flux-core/Cargo.toml`, `crates/flux-cli/Cargo.toml`; remove both from the `ignored` list at `Cargo.toml:117` | `flux-core` depends on `tracing` only; `flux-cli` on both. A member crate inherits the workspace entry's features and cannot remove them, so the **workspace entry itself** (`Cargo.toml:68`, today `features = ["env-filter", "json"]`) is changed to `default-features = false` plus only the features the custom formatter needs (no `env-filter`, no `json`, no `tracing-log`: N1); `just check` and `just deny` must pass |
| Emission | `crates/flux-core/src/tree.rs` (the `note_copied`, `note_resumed`, `note_skipped` sites, `enter_dir`, `link_one`, the hardlink and replace paths), `run/mod.rs` (`file`), `copy.rs`, `cleanup/delete.rs` | `INFO` `flux::op` events (5.2); `DEBUG`/`TRACE` events (5.5) |
| Flags | `crates/flux-cli/src/main.rs` (`Cli`, global args; `VerifyArgs.verbose` replaced) | `-v` count, `--verbose`, `-q`/`--quiet`, the conflict error |
| Verbosity value | `crates/flux-cli/src/live.rs` or a new module `crates/flux-cli/src/verbosity.rs` | `Quiet`, `Normal`, `Verbose(n)`; gating of the direct prints |
| Subscriber and formatter | new module in `flux-cli` (for example `logging.rs`) | installs the global subscriber once in `main`; renders section 5.2 and 5.5; routes to the display or stderr; discards errors |
| Summary helpers | `crates/flux-cli/src/report.rs` (`summary_line`, `dry_run_summary_line`) | section 6 |
| Docs | `README.md` (commands section, the verify `--verbose` line), `ROADMAP.md`, `TODO.md`, the progress spec's N3 note | |

The plan (not this document) fixes the file-level wiring and line numbers, after the code it depends on is read (the plan-versus-spec discipline).

## 8. Constraints and risks

- The subscriber is global and set once in `main`; library code and tests never call `set_global_default`. Tests use a scoped dispatcher.
- Events from the WAL writer and hashing threads reach the same subscriber; their relative order with the walk thread's events is not defined and is not tested.
- A `tracing` call in the per-file path costs one callsite check with logging off. This is **to be measured** (benchmark `benches/copy.rs` before and after on a many-small-files tree) because the figure in the consult was a guess; a timing measurement needs the owner's go-ahead and an idle machine (the project's timing rules).
- The flicker of per-line redraws on a large tree (D2, progress boundary B8) is to be measured the same way before the plan is final; if it is bad the fallback is a rate-limited batch of `-v` lines, not removing the bar.
- Changing the summary line changes the tests that pin it (19 matches of the old text today: 13 in the `report.rs` unit tests, 1 in `tests/copy.rs`, 5 in `tests/dry_run.rs`) and the README; the plan finds them by search.

## 9. Testing

- Formatter: unit tests on an in-memory writer with a scoped subscriber: each verb line byte-for-byte (the table in 5.2), the escaping of a name with `\x1b[2K`, `\r` and a non-UTF-8 byte, the 10-column alignment, a closed writer does not panic.
- Gating: a table test over the section 5.3 matrix (output class x setting), with the exit code.
- Flags: `-v` with `--quiet` exits 2 in every position (`copy -v -q`, `-q copy -v`, `verify -vq`, `-v copy -q`); `--verbose` equals `-v`; `-vvvv` behaves as `-vvv`; `flux -v copy` and `flux copy -v` are the same; a resume with different verbosity is accepted.
- CLI process tests (piped stderr): the S1 and S2 outputs of this document at each setting; `--json` with `-v` and with `--quiet` (stdout unchanged, stderr per 5.3); `--dry-run --quiet` prints the whole dry run.
- Terminal: extend `crates/flux-cli/tests/pty.rs` (cut 20): `-v` on a pty draws the bar and prints `Copied` lines, and the bar is cleared before the summary; `--quiet` on a pty draws no frame and prints nothing on success.
- Summary helpers: unit tests for 0, 1, 2 files and directories, the size scale boundaries (1023 B, 1 KiB), and the elapsed formats at 0.35 s, 12.3 s, 59.99 s, 60 s, 3723 s.
- verify and cleanup: `-v` prints `match` records (the old `--verbose` test, kept); `Removed` lines appear at `-v` on cleanup.
- Mutation: each new test is proven non-vacuous with a logic mutant, as in earlier cuts.

## 10. Calls I made (the owner may overrule any)

1. The verb column is 10 wide, not Cargo's 12 (the longest verb is `Hardlinked`).
2. `-v` with `--quiet` is a usage error (exit 2).
3. `-q` is the short form of `--quiet`; the spec lists only the long form.
4. `--quiet` hides skip records (exit 0) and resume notes, keeps the complete dry-run output, and for `verify` and `cleanup` applies the same one rule (5.3): a clean run prints nothing.
5. `--verbose` is also the long form on `copy` and `cleanup`, not only on `verify`.
6. `-vv`/`-vvv` lines have no timestamp and no colour; their exact bytes are not a stable interface.
7. The summary polish also applies to the dry-run summary (sizes and plurals), without a time.
8. Elapsed-time formats and the use of `indicatif::HumanBytes` (section 6).
9. `verify` and `cleanup` keep their record and summary texts (N4), and `cleanup` gets `Removed` lines at `-v` only.
10. Warnings still print under `--quiet` is the owner's (D3); the choice that skip records are not warnings is mine.
11. Under `--quiet`, `flux verify` hides `extra` records (they never fail a verify) and prints the `missing`, `mismatched` and unreadable ones only when the exit is 1.
12. Under `--quiet`, the per-root and total lines of a several-source run are hidden with the summary.
13. `path` in a `-v` line is relative to the destination root (the root's name leads in a several-source run); for a single file it is the destination as typed.
14. Control characters are escaped on every line written through the display, which also changes how an existing failure line prints for a hostile name (never for an ordinary one).
15. `flux verify -v` has no verb lines; it is today's `--verbose` and nothing more.
16. Under `--quiet`, `checkpoints discarded for X: ...` lines are warnings and stay; the partial-resume lines, the advisory note and the full-level line are notes and are hidden.

## 11. Stand-downs (panel record)

- UNVERIFIED-ACCEPTED: the claim that keeping the bar at `-v` causes severe redraw contention and lowers copy throughput (a second reviewer, round 1). It is unmeasured; the bar at `-v` is the owner's decision D2, the measurement and a rate-limited fallback are in sections 3 and 8.
- Peer suggestion not taken: a two-tier quiet (`-qq`); recorded as non-goal N6.

## 12. What is left to the plan

The exact emission sites and line numbers; the shape of the writer handle shared by the subscriber and `Live`; the `-vv`/`-vvv` event set; the list of tests whose expected summary text changes; the benchmark protocol for the logging-off cost; the README wording.
