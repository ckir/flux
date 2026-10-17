# Cut 11b: `flux copy --dry-run` with `--resume`, `--restart` and `--break-lock` (design, r3 in progress: F2 reopened)

Branch `spec/cut-11b-dry-run-prior`. Follows cut 11a (`docs/superpowers/specs/2026-10-17-cut-11a-dry-run-design.md`, merged in PR #108), whose scope S1 deferred every prior-operation preview to this cut (N1, D2). Master spec: section 5.2 (`FLUX_FULL_UPDATED_SPEC_V16.md:794-828`), 21.1 (`:1595-1660`), 240.1-240.5 (`:10604-10760`), acceptance tests 79 (`:13350`) and 136 (`:13542`).

## 1. Purpose, goals, non-goals

A dry run combined with `--resume`, `--restart` or `--restart --break-lock` previews THAT invocation at a destination holding prior operation state: which prior would be adopted or superseded, which refusal the real run would get, what the walk would then do entry by entry, and what would be discarded, while changing nothing and taking no lock.

Goals:
- G1. One engine path (cut 11a's dry mode, mechanism M4) extended with the prior-operation arms of `open_operation` (`crates/flux-core/src/run/session.rs:142-160`) re-expressed read-only. No second preview engine.
- G2. Per-entry truth under `--resume`: an entry a real resume would count as `files_resumed` prints `would_resume`; the counters equal the real run's under the stated limits.
- G3. Every outcome the real run decides by a lock or a write is named as unprobed, with the real run's possible outcomes, never guessed (owner decision, agy-first round 1).
- G4. Plain `--dry-run` with a resumable prior now predicts the real run's `RESUMABLE_OPERATION_EXISTS` refusal, closing cut 11a's stated deviation from section 5.2 (11a spec section 3.5, "closed by cut 11b").

Non-goals:
- N1. A half-copied file's continuation (`plan_resume`, `files_partially_resumed`, `FileResume`): the checkpoint log is not read and no partial is hashed. The partial is reported present with its size and the entry prints `would_copy` (3.4). Previewing the reused prefix is a later cut (it needs a read-only log scan, `crates/flux-core/src/wal/replay.rs:127`).
- N2. `flux cleanup --dry-run`.
- N3. `--resume-verify=metadata` stays refused as today (`crates/flux-cli/src/main.rs:275-278`).
- N4. The `<lock-name>.broken.*` leftovers of an interrupted recovery (section 240.3) are not classified; the real run's acquirer handles them (`crates/flux-core/src/lock/acquire.rs`). A dry run that sees one prints a note (4).
- N5. Predicting a live owner's liveness. The real run decides it by `try_lock` (`crates/flux-core/src/lock/classify.rs:40`); a dry run never does (I4).

## 2. Invariants (cut 11a's I1-I7 continue; these are added)

- I8. The lock record is READ, never opened as a lock: `DirHandle::read_file(lock_name, RECORD_LEN)` (`crates/flux-fs/src/fs.rs:445`), decoded by `lock::record::decode` (`crates/flux-core/src/lock/record.rs:84`). `open_lock`, `create_lock` and `try_lock` stay on the fake's deny list (`crates/flux-core/src/run/tests.rs:7747-7765`).
- I9. The prior's claim store is opened READ-ONLY through a new `DirHandle::open_claim_store_read_only(name)` whose store cannot write (redb 4.3.0 `ReadOnlyDatabase`, `~/.cargo/registry/src/*/redb-4.3.0/src/db.rs:2223`; the fake's wrapper answers every write method with `Code::SafetyRejected`). A refused open because a writer holds the file (`DatabaseError::DatabaseAlreadyOpen`, db.rs:2213, non-blocking) is a LIVE-OWNER signal, 3.3.
- I10. Agreement under the dead-owner hypothesis: with the lock absent or its owner dead, the dry run's counters, per-entry actions, notes, refusal code and exit code equal the real invocation's on the same untouched inputs, except N1, cut 11a's stated limits, and the snapshot gap (a real run racing the preview).
- I11. Nothing is written, as 11a: no lock, no ABANDONED write, no sweep, no state removal, no store open for writing, no WAL. The fake's `forbidden` allow-list is the proof, extended by `open_claim_store_read_only` (`read_file` is already allowed; the tests now assert its argument is a state record or the lock name) and by a deny on every write method of the read-only store (the fake's wrapper records them).

## 3. Design

### 3.1 CLI and configuration (`crates/flux-cli/src/main.rs`)
- The usage error at `main.rs:281-285` (`--dry-run does not combine with --resume, --restart or --break-lock in this version (cut 11b)`) is removed. `--resume` with `--restart` stays clap's conflict (`:87`), `--break-lock` still requires `--restart` (`:95`); `--resume-verify` requires `--resume` (`:92`) and is accepted under `--dry-run` (its mode is reported as unperformed, 3.6).
- `RunConfig` is unchanged: `dry_run`, `resume`, `restart`, `break_lock` already exist (`crates/flux-core/src/run/mod.rs`, `tests.rs:26-40`); the engine's dry branches now read the last three.

### 3.2 The lock record (fork F1-a, owner-approved)
`dry_lock(site) -> LockPreview`, in `crates/flux-core/src/run/mod.rs` beside `dry_prior_notes` (`:1060`), run by both dry branches before the prior scan. It mirrors `classify` (`lock/classify.rs:28-60`) minus `try_lock`:

| `read_file(lock_name, RECORD_LEN)` then `decode` | `LockPreview` | What the preview prints and predicts |
|---|---|---|
| `NotFound` | `Absent` | nothing; the real run acquires fresh |
| `SafetyRejected` or `DestinationError` (a link, a directory, a special file) | `Foreign` | refusal `CONTROL_PLANE_NAMESPACE_CONFLICT` predicted (as `obtain.rs:84-90`), exit 3 |
| `Decoded::Foreign` | `Foreign` | the same |
| `Decoded::Uncertain(why)`, plain, `--resume`, or `--restart` without `--break-lock` (all `Mode::Plain`, `session.rs:127`) | `Uncertain(why)` | refusal `TARGET_LOCK_UNCERTAIN` predicted (`obtain.rs:110-118`), exit 3; note: an empty or torn record may also be a holder inside its first write (`classify.rs:45-46` reports that as busy), which the dry run cannot tell |
| `Decoded::Uncertain(why)`, `--restart --break-lock` | `Takeover(why)` | section 240.5 step 1 from `lock_capability` (allowed): a capability without strong identity and native locks predicts `TARGET_LOCK_UNCERTAIN`; otherwise the takeover is previewed: `note: --break-lock would take over the lock, whose record is unreadable (<why>); a real run refuses with TARGET_LOCK_BUSY if the OS lock is held`, `unprobed` gains `lock_takeover`, the preview continues as if the owner were gone (test 136). (The four holder fields are never readable here: an `Uncertain` record has no fields; 240.5's "reports the lock as unreadable" applies.) |
| `Decoded::Record(r)`, `site.workspace(&r)` (`lock/site.rs:142`, read-only) `Untrusted` or `Missing` | `Orphaned(r)` | refusal `ARTIFACT_OWNERSHIP_UNCERTAIN` predicted (`obtain.rs:91-108`, the file-site wording for a single file), exit 3 |
| `Decoded::Record(r)`, workspace `Trusted` | `Recorded(r)` | the DUAL OUTCOME: note `note: a lock record for operation <id> exists (owner instance <owner_instance_id>, boot session <boot_session_id>, last heartbeat <age> ago, workspace <workspace_path>); a real run tries the OS lock: if it is held, TARGET_LOCK_BUSY; if not, the preview below applies, and this exit code reflects that case`; `unprobed` gains `owner_liveness`; the preview continues under the dead-owner hypothesis (the real run would move the lock aside, 240.3, and proceed) |
| any other read error | `Error(e)` | the real run's `from_lock(e, RunStep::Lock, ..)` error predicted, exit 1 |

A predicted lock refusal or error ends the preview there: the real run never reaches step 4, so no prior note, no plan and no summary follow it (3.6). `<age>` is `now - last_heartbeat_wall_time` (both nanoseconds, `record.rs:21-22`) formatted in whole seconds, `in the future` when negative (clock skew, test-named). The record's fields are printed exactly as section 240.5 lists them. The workspace check is `LockSite::workspace` (`lock/site.rs:142-171`): `metadata` and `open_dir` only, for both site kinds. A `<lock-name>.broken.<id>` sibling found in one `read_dir` of the holder directory (allowed) prints `note: an interrupted lock recovery left <name>; a real run classifies it` (N4).

### 3.3 Prior classification (step 4 read-only; `Place::scan` as 11a)
The arms of `session.rs:142-160`, re-expressed; `Err(e)` from the scan is the real run's `RunStep::Inspect` refusal predicted (`STATE_CORRUPT` and its kin), exit 3:

| Invocation | `scan.resumable` | Preview |
|---|---|---|
| plain `--dry-run` | empty | 11a's fresh plan (unchanged) |
| plain `--dry-run` | one or more | the 11a prior notes, then refusal `RESUMABLE_OPERATION_EXISTS` predicted with `resumable_refusal`'s text (`session.rs:151`), exit 3; `prior_operation` leaves `unprobed` (G4, D1) |
| `--resume` | empty | fresh plan; line `no prior operation; starting new` (`report.rs:517`) |
| `--resume` | exactly one | the resume preview, 3.4 |
| `--resume` | two or more | refusal `RESUMABLE_OPERATION_EXISTS` predicted, exit 3 (`session.rs:150-155`) |
| `--restart` | any | the restart preview, 3.5, then the fresh plan |

`scan.completed` priors print 11a's note (`a real run would finish its recorded cleanup`); `scan.debris` prints one note naming each `<id>.creating` / `<id>.removing` directory a real run would remove (`finish_priors`, `session.rs:295`). Neither changes the exit code.

### 3.4 The resume preview (fork F2-a, owner-approved)
In order, each step's refusal predicted exactly as the live adoption's (`session.rs:175-206`):
1. `resume::validate(&prior, &place.roots(), opts, warnings)` (`run/resume.rs:32`, pure): `Err` is the predicted refusal (`INCOMPATIBLE_STATE`, or the corrupt-fingerprint `STATE_CORRUPT`), exit 3. Its warnings print as live.
2. The store, trees only (`place.adopt`, `run/place.rs:94-105`, the open at `:590`): `open_claim_store_read_only("state.db")` in the prior's workspace (opened by `open_dir`, allowed). `NotFound`, zero-length or `StateCorrupt` while the prior is `CREATED` previews `note: would create a fresh claim store for operation <id>` and continues with `NullClaimStore` (every `get` answers `None`), as live recreates (`place.rs:97-99`); any other open failure is the live adoption's error or refusal predicted with its own code and exit (`StateCorrupt` and `IncompatibleState` are refusals, exit 3; `NotFound` for a `TRANSFERRING` prior is the live `IoError`, exit 1). A refused open because a writer holds the file (I9; the platform answers `Code::TargetLockBusy`, `crates/flux-fs/src/error.rs:45`) is a live owner: refusal `TARGET_LOCK_BUSY` predicted, exit 3, the dual outcome of 3.2 collapsed to its held side, `owner_liveness` dropped from `unprobed`. The inference holds because the store's only opener outside tests is `adopt` itself (`place.rs:590`), which runs under the obtained lock.
3. Commit recovery, read-only (`recover::classify`, `run/recover.rs:130-171`, over `store.prepared()`): each `Renamed` note counts as `would recover`; `NotRenamed` and `Gone` as notes a real run discards; any `Uncertain` is the live refusal `COMMIT_STATE_UNCERTAIN` predicted (`place.rs:627-632`, `LockCode::CommitStateUncertain`, `uncertain_detail`), exit 3. Nothing is applied to the destination.
4. The walk: cut 11a's `copy_tree_frames` with `Shared { resume: true, claims: Some(&readonly_store), ckpt: None, dry_run: true, .. }`. The resume arms (`tree.rs:1386-1400`, `:1880-1895`) only `get` claims, so a `Created` claim whose target matches prints `would_resume` (new `PlannedAction::Resume`, stream word `would_resume`) and counts `files_resumed` / `bytes_resumed`; every other entry falls through to 11a's planning. The store's write methods are never reached (I11; the fake's call log proves it).
5. Partials (N1): when planning a file under `--resume`, one `metadata` of the entry's temporary name on the destination directory (`copy::temp_path` over the planned name, `copy.rs:253`, with the PRIOR's operation id); present, the line is `would_copy (partial of <len> bytes present; its continuation is not previewed)` and the summary's `partial_resume` count rises. The counters count the entry as copied, where the real run may count it partially resumed (I10's stated limit).
6. Single file: step 1, then the partial check of step 5 on `<target>.flux-partial.<id>`; no store, no recovery (`adopt` returns `None` for a file).
Lines before the plan, mirroring `resume_lines` (`report.rs:513-531`): `would resume operation <id> (<n> entries claimed)` (`store.count()`), `would recover <r> interrupted publications` when `r > 0`; after the walk `would resume <files_resumed> already-complete files` when non-zero. JSON folds resumed entries as live does (`report.rs:102-105`: into `files_skipped` and `bytes_skipped`), so the live key set is unchanged.

### 3.5 The restart preview (fork F3-a, owner-approved)
Mirrors `restart::supersede` (`run/restart.rs:18-42`) and `sweep_partials` (`run/sweep.rs:19`) without writing:
1. For each resumable prior, `note: would supersede operation <id> (state <STATE>)` and the line `.flux/operations/<id>: object_type=state action=would_discard` (tree) or `<target>.flux-state.<id>: object_type=state action=would_discard` (single file).
2. A read-only walk of DEST by path (`walk`, as `sweep_partials` does, skipping the reserved control directories through `reserved_path`), listing every `<name>.flux-partial.<prior id>` as `<path>: object_type=partial action=would_discard`. A directory the walk cannot list prints the live `PartialKept`-shaped warning and continues (`sweep.rs:34-40`). DEST absent: nothing to discard.
3. Then cut 11a's fresh plan. `files_total` and the counters are the fresh plan's; `would_discard` lines are counted in the summary's `discard` count only. They are streamed like every planned line, as `TreeFailureCause::Planned(PlannedEntry { object: State | Partial, action: Discard })` with the DEST-relative path, so the CLI's one line-printer serves them.
With `--break-lock`, 3.2's `Takeover` row applies first. A completed prior is never superseded (section 21.1) and keeps 11a's note.

### 3.6 Reporting and output (`crates/flux-cli/src/report.rs`)
- Stream lines: `<path>: object_type=<file|dir|symlink|state|partial> action=<would_create|would_copy|would_overwrite|would_skip|would_create_symlink|would_resume|would_discard>` with 11a's ` (<reason>)` suffix; the two new object words and two new action words extend `dry_run.rs`'s `as_str` tables.
- Notes, in this order before the summary: the lock note (3.2), the prior notes (3.3), the resume or restart lines (3.4, 3.5), then 11a's `noreplace_publish` and `target_lock` notes, then, always under `--resume`, `note: --resume-verify=<mode> would be applied by a real run; nothing was verified` (`<mode>` is the effective mode, `chunks` by default).
- Summary: 11a's line unchanged, followed when non-zero by `dry run: would resume R already-complete files`, `dry run: would discard P partials and S states`, `dry run: P partial files present, continuation not previewed`.
- JSON: `dry_run: true`; `unprobed` in this fixed order, each present only when it applies: `noreplace_publish`, `target_lock`, `owner_liveness`, `lock_takeover`, `prior_operation` (now only for completed priors and debris, since resumable ones are previewed), `resume_verify`. No new top-level key.
- Predicted refusals print exactly the real run's stderr line (the code and the detail, `exit_code.rs`), then nothing else: as in 11a's pre-flight refusals, no plan follows a predicted refusal.

### 3.7 Exit codes (owner decision: exit per the preview)
0 when the preview under the dead-owner hypothesis is clean; 1 when it contains failures or a read error; 2 for the clap conflicts; 3 for every predicted refusal (`TARGET_LOCK_UNCERTAIN`, `CONTROL_PLANE_NAMESPACE_CONFLICT`, `ARTIFACT_OWNERSHIP_UNCERTAIN`, `RESUMABLE_OPERATION_EXISTS`, `INCOMPATIBLE_STATE`, `STATE_CORRUPT`, `TARGET_LOCK_BUSY` from a held store). A `Recorded` lock with a live owner is NOT exit 3 (it is `owner_liveness` in `unprobed`); scripts read the JSON.

## 4. Edge cases and decisions made explicitly
- Lock present, no prior state: `Recorded` whose workspace is `Missing` is `Orphaned` (3.2); a `Trusted` workspace whose manifest the scan cannot read is the scan's refusal (3.3).
- Prior state present, lock absent: the real run acquires fresh and adopts or supersedes; the preview does the same with no lock note.
- A dry run racing a live resume: the snapshot gap (agy-first (a)); every read is one pass, the note names the record as read, nothing is re-read.
- A record whose heartbeat is in the future prints `in the future` and is otherwise `Recorded` (section 240.2: age never decides).
- The store open refused by a writer in the same process (tests) or another (CI) behaves the same (`DatabaseAlreadyOpen` on Linux, macOS, Windows, db.rs:2213-2215).
- Windows: every open uses `FILE_SHARE_READ|WRITE|DELETE` (`dir_windows.rs:220,509,636`), so reading a held lock file succeeds (measured in review; agy's contrary claim refuted).
- `--restart` with completed priors only: nothing superseded, the fresh plan, 11a's notes.
- DEST absent with `--resume`: `no prior operation; starting new`, the fresh plan, exit as 11a; with `--restart`: the fresh plan.
- A single-file target under `--restart`: its `<target>.flux-state.<id>` records and `<target>.flux-partial.<id>` beside it, through `scan_file` (`prior.rs:114`) and `metadata`.
- A prior of an older format: `validate`'s `INCOMPATIBLE_STATE` text ("run again with --restart"), predicted verbatim.

## 5. Failure table (what a dry run predicts)
| Condition | Real run | Dry run |
|---|---|---|
| lock held by a live owner, record readable | `TARGET_LOCK_BUSY`, 3 | dual outcome note, `owner_liveness`, preview under the dead hypothesis, exit per the preview |
| lock held, store open by the owner (tree, `--resume`) | `TARGET_LOCK_BUSY`, 3 | `TARGET_LOCK_BUSY` predicted, 3 |
| record torn/empty/checksum, plain, `--resume` or `--restart` without `--break-lock` | `TARGET_LOCK_UNCERTAIN`, 3 | the same, predicted |
| the same, `--restart --break-lock`, strong capability | takeover, then proceeds | takeover previewed, `lock_takeover`, exit per the preview |
| the same, weak capability | `TARGET_LOCK_UNCERTAIN`, 3 | the same, predicted |
| not a Flux lock | `CONTROL_PLANE_NAMESPACE_CONFLICT`, 3 | the same, predicted |
| dead owner, workspace missing or untrusted | `ARTIFACT_OWNERSHIP_UNCERTAIN`, 3 | the same, predicted |
| resumable prior, plain or several under `--resume` | `RESUMABLE_OPERATION_EXISTS`, 3 | the same, predicted (G4) |
| prior record corrupt | `STATE_CORRUPT`, 3 | the same, predicted |
| options differ | `INCOMPATIBLE_STATE`, 3 | the same, predicted |
| an uncertain prepared note | `COMMIT_STATE_UNCERTAIN`, 3 | the same, predicted |
| a half-copied file | partially resumed | `would_copy (partial of K bytes present; ...)` (N1) |
| a `.broken.*` leftover | classified by the acquirer | note only (N4) |

## 6. Tests
- Fake (`run/tests.rs`, `tree.rs`): the `forbidden` allow-lists gain `open_claim_store_read_only`, and assert every `read_file` argument is a state record (`manifest`, `scan_tree` at `prior.rs:77`) or the lock name (I11); a paired corpus over the destination-state matrix of the agy-first brief (lock absent / live via `live_lock` / dead via `dead_lock` / torn / foreign; priors none / one / two / completed / corrupt; `--resume`, `--restart`, `--restart --break-lock`, plain), each case: snapshot DEST, dry, assert no change, live on a copy, compare counters, per-entry records, refusal code and exit, under the dead hypothesis; the live-owner cases assert the dual note and `owner_liveness`; a held store (the fake's `FakeClaimStore` opened read-write by the fixture) asserts `TARGET_LOCK_BUSY` predicted.
- Real fs (`crates/flux-core/tests/dry_run_std_fs.rs` extended, or a new `dry_run_prior_std_fs.rs`): a resumable prior built by a real interrupted run (the `crash` fixtures of `run/tests.rs` pattern), then dry `--resume` versus live `--resume` on two copies: `files_resumed` equal; a real `--restart` versus the dry list of `would_discard` paths: equal sets; a real `RedbClaimStore` held open by the test, then the dry run's read-only open refused: `TARGET_LOCK_BUSY` predicted (the one place `DatabaseAlreadyOpen` is measured).
- CLI (`crates/flux-cli/tests/dry_run.rs`): test 79 (`--dry-run --resume` and `--restart` report the resume or the discard and fresh plan, take no lock, change nothing) and test 136 (`--resume` with no prior: fresh plan; `--restart --break-lock` against an uncertain lock: the takeover previewed; nothing changed); the usage error's removal; the JSON `unprobed` order.
- Memory: `dry_run_memory.rs` unchanged (the read-only store is the prior's own file; no new per-entry state).
- Cut 11a tests whose expectations change under D1 (a resumable prior now predicts the refusal): `a_dry_run_notes_each_prior_and_still_plans` and `a_dry_run_says_whether_it_saw_a_prior_through_both_entry_points` (`run/tests.rs:7882-7955`) for their `TRANSFERRING` priors; the completed-prior cases there and the CLI `a_dry_run_notes_a_completed_prior_and_still_plans` are unchanged. Live-owner rows of the paired corpus compare the dry preview against the real run's refusal only (the real run cannot proceed), the dead-owner rows against the real run's full outcome.
- Docs the cut amends: the 11a spec's P10 vocabulary and section 3.5 (superseded by 3.3-3.6 here), `TODO.md` "Prior operations at DEST are noted, not previewed" (closed), `docs/accepted-boundaries.md`'s cut 11a prior bullet (replaced by this cut's limits N1, N4), `ROADMAP.md` row 8.

## 7. Decisions made (D) for the owner to check
- D1. Plain `--dry-run` with a resumable prior now predicts `RESUMABLE_OPERATION_EXISTS` (exit 3) instead of 11a's notes-and-plan; 11a's D2 is retired and its `TODO.md` "Prior operations at DEST are noted, not previewed" limit is closed.
- D2. The dual outcome is one note and one `unprobed` word; the preview continues (owner-approved).
- D3. A refused read-only store open is reported as `TARGET_LOCK_BUSY` predicted (not as a new word): it is what the real run gets.
- D4. Partials are reported, never continued (N1); the `partial_resume` count is a summary line, not an `unprobed` word.
- D5. `would_discard` lines use two new object words, `state` and `partial`; they are not counted in `files_total`.
- D6. `prior_operation` in `unprobed` narrows to completed priors and debris.
- D7. The new `DirHandle::open_claim_store_read_only` is the only new platform surface, and it carries a fork the PLAN settles (AGY-FIRST at plan time): redb 4.3.0's read-only constructors are PATH-based only (`ReadOnlyDatabase::open(path)`, db.rs:676; `Builder::open_read_only(path)`, :2223; `ReadOnlyDatabase::new` is private, :680), while Flux opens every file through a directory handle without following links (`open_claim_store` at `dir_unix.rs:392`, `dir_windows.rs:449`). Options: (a) a path open of `<workspace canonical path>/state.db` followed by an identity check of the opened file against the handle's `metadata("state.db")` (the 240.5 step 4 pattern), refusing on mismatch; (b) `Builder::create_with_backend` (db.rs:2255, public) over a `StorageBackend` wrapper on a handle-opened read-only `File` whose write methods fail and whose lock methods take SHARED locks, so a live writer's exclusive lock refuses the open as `DatabaseAlreadyOpen` does. The fake's wrapper (`FakeClaimStore` behind write-refusing methods) is the same task.

## 8. Exhaustiveness self-audit
- Under-specified: the exact wording of the two new notes and the `would_discard` lines is given (3.2, 3.5); the `unprobed` order is fixed (3.6); the age format is fixed (3.2). Open and owned by the plan: the redb file-backed read-only constructor (D7); whether `recover::classify` needs a `dest` handle under a planned (absent) DEST (it cannot: a prior implies DEST exists).
- Placeholders: none.
- Cases: the matrix of the agy-first brief maps to 3.2, 3.3, 5 and 6; single-file rows are named in 3.2, 3.4 step 6, 3.5 step 1, 4.
- Requirements: master 5.2's four sentences map to 3.3-3.5 and 3.7; 21.1's classification to 3.2-3.3; 240.5 to 3.2's `Takeover` row; tests 79 and 136 to 6.

## 9. Review record
- agy-first round 1 (`.clavity/seams/11b-fork-1.md`, `.clavity/scratch/11b-fork/r1-reply.md`, `[VERDICT: ALIGNED]`): F1-a, F2-a, F3-a recommended by both; refinements folded: the store open refused by a writer as the live-owner signal (I9, D3), partials from metadata only (N1), the exit-per-preview rule (3.7). Refuted by measurement and not folded: `fcntl(F_GETLK)` as a probe (Flux locks with `flock`, `lock_file.rs:3,106`); Windows sharing violations on a held lock (every open shares, `dir_windows.rs:220`); `plan_resume` hashing the source in `chunks` mode (it reads the destination partial, `copy.rs:633,663`).
- Owner decisions 2026-10-17: the shape F1-a + F2-a + F3-a; exit per the preview.
- Panel round 1, solo (r2 folds, each measured): the `Takeover` note cannot carry holder fields (an `Uncertain` record has none); a predicted lock refusal ends the preview before the prior notes (the real run never scans); the empty store is `NullClaimStore`; the partial's name comes from `copy::temp_path` with the prior's id; the refused store open answers `Code::TargetLockBusy` and the inference rests on `adopt` being the store's only opener (`place.rs:590`); discard lines travel as `Planned` entries; `resume_verify` always under `--resume`; the dual note says the exit reflects the unheld case; `read_file` was already allowed (I11 reworded); the 11a tests that change are named; redb's read-only constructor is path-only (D7 reframed as a plan fork). Below the floor: the `.broken.*` listing costs one `read_dir` of the holder on every dry run; a dry run racing a takeover reads a torn record (already `Uncertain`, 3.2).
