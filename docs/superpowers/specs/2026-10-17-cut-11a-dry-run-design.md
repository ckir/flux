# Cut 11a: `flux copy --dry-run` (design)

Status: DRAFT r2 for owner review (r1 folded the builder check and agy panel round 1; section 9). Written against `origin/main` 5b0a622 (PR #107 open, touching only `crates/flux-platform` and docs). Master spec: `FLUX_FULL_UPDATED_SPEC_V16.md` section 5.2 (lines 794-828), invariants 14, 38, 57, 109, acceptance tests 59, 79, 110, 124, 136. The owner chose, after AGY-FIRST (brief `.clavity/seams/dry-run-fork-1.md`, reply `.clavity/scratch/dry-run-1/r1-reply.md`, ALIGNED): mechanism M4 (a dry mode inside the shared engine), scope S1 (fresh-plan preview only), output O1 (per-entry lines, a summary and `--json`).

Line anchors were read at 5b0a622 and are for orientation; the plan re-reads every one before using it.

## 1. Purpose, goals, non-goals

Purpose: `flux copy --dry-run SOURCE DEST` reports, for every entry the real run would handle, the action the real run would take, and changes nothing: no lock, no workspace, no state, no claim, no partial file, no destination change (section 5.2, invariant 14, acceptance test 59).

Goals:
- G1. The dry run and the real run that follows it make the same decisions. The decision code is shared, not copied (a separate planner would duplicate about six enforced checks: reserved control-plane paths, the section 129 cycle checks, source mount events, safety degradation accounting, the non-directory-at-a-directory-name refusals, single-file target resolution).
- G2. A tree copy and a single-file copy are both covered.
- G3. Resident memory is bounded as a real run's walk is (acceptance test 59): no claim table, no per-file state, no per-entry list; the only per-directory state is the one `NameIndex` the real run also builds for the directory being processed.
- G4. The output states what would happen per entry, a summary, and `--json`, in the existing formats extended minimally.

Non-goals (each is a later cut or a stated limit, never silent):
- N1. `--dry-run --resume`, `--dry-run --restart`, `--dry-run --break-lock` (spec lines 810-824, tests 79 and 136) are cut 11b. In 11a they are a usage error (exit 2) naming the later cut, as `--resume-verify=metadata` is refused today (`crates/flux-cli/src/main.rs:271-275`).
- N2. Predicting `DESTINATION_NAMESPACE_COLLISION` between targets of the same run (spec lines 826-827, invariant 109).
- N3. Predicting failures that only occur while bytes move (a read error mid-file, a full disk, a source changing during the copy). The dry run reads metadata, never file content.
- N4. `--dry-run` for `flux cleanup` (exists) or any other subcommand.
- N5. Several sources and `--exclude` (separate cuts).

## 2. Invariants

- I1 (read-only). A dry run performs no destination mutation and takes no lock. The destination-side methods it may call are exactly these, by name (the call-log test asserts the set of called methods is a subset of this list, and that nothing outside it is called):
  - `DestinationRoot` / `FileSystem`: `destination_root`, `metadata`, `read_dir`, `read_link`, `mount_root`, `canonical_path`, `identity`.
  - `DirHandle`: `open_dir`, `metadata`, `read_dir`, `read_link`, `identity`, `mount_root`, `canonical_path`, `lock_capability`, `read_file` (only for prior-operation state records, section 3.5).
  Never called: `create_dir`, `create_new`, `create_symlink`, `create_lock`, `open_lock`, `create_claim_store`, `open_claim_store`, `open_existing`, `open_read` on a destination entry, `rename_*`, `remove_*`, `sync`, any writer method. Test: the call-log assertion on the fake plus a before/after recursive snapshot on the real filesystem.
- I2 (agreement). For every fixture without a same-run namespace collision, the dry run's exit code, counters, failure list (code and path) and per-entry actions equal the real run's on the same untouched inputs, whatever the real run's counters are (including how it counts an overwrite and, for a single file, that `bytes_skipped` stays 0, `crates/flux-cli/src/report.rs:114`). The paired test (section 6) is the oracle.
- I3 (no content reads). A dry run opens no source file for reading and hashes nothing. Every planned copy or overwrite stats its source (`metadata`) exactly as the real run's first read does, so a vanished or non-regular source is the same `CopyStep::Source` failure, and `bytes_*` come from the source length.
- I4 (no lock probing). A dry run takes no OS lock, not even a trial `try_lock` (`crates/flux-core/src/lock/classify.rs:39` does, which is why the classifier is not used).
- I5 (live runs). A dry run may run while a real operation is live; its report is a snapshot (spec lines 805-808). It does not look at the lock.
- I6 (flag isolation). `dry_run` is not part of `CopyOptions`, `state::Options`, the fingerprint, or any on-disk record: a dry run writes none.
- I7 (exit parity at the edges). A real run counts a DEST it created in `directories_created` only after the walk and only if no rollback removed it (`crates/flux-core/src/run/mod.rs:476-482`); an abort that created nothing is rolled back and exits 3 (`TreeAbort::refused_unchanged`). The dry run reproduces this: a planned DEST creation is counted after the walk, and a walk abort with no other planned directory counts nothing and exits 3.

## 3. Design

### 3.1 CLI and configuration
- `CopyArgs` (`crates/flux-cli/src/main.rs:55-115`) gains `--dry-run` (bool). `flux cleanup` already has its own.
- `--dry-run` with `--resume`, `--restart` or `--break-lock`: usage error, exit 2, message `--dry-run does not combine with --resume, --restart or --break-lock in this version (cut 11b)`. Everything else combines (`--existing`, `--links`, `--safety`, `--cross-filesystems`, `--preserve-*`, `--json`); `--durability` and `--resume-verify` are accepted and have no effect.
- The engine switch is `RunConfig.dry_run: bool` (`crates/flux-core/src/run/mod.rs:55`), not a `CopyOptions` field: it is a property of the invocation like `resume` (I6). Every `RunConfig` literal gains the field; the compiler names them, and the plan lists them. (The `dry_run: true` literals in `main.rs` and `run/tests.rs` belong to `flux cleanup`'s config; they are not `RunConfig`.)

### 3.2 Entry and pre-flight
`run::tree` and `run::file` (`run/mod.rs:227`, `:488`) branch on `cfg.dry_run` after the read-only pre-flight stages, reused verbatim. They are, as the code stands:
- tree: `prepare_source`; `locate_tree` (returns `dest: None` while DEST does not exist, without creating it); `check_capability`; `prior::check_control_plane(dest, ..)` when DEST exists; `LockSite::directory` (pure, in-memory: it refuses a DEST name whose lock name exceeds `NAME_LIMIT` with `PathComponentInvalid`, exit 3, which a dry run must report too).
- file: `prepare_file` (it calls `destination_root` on the parent); `file_names_fit`; `check_capability`; `LockSite::file`. A single-file run has no `check_control_plane` stage (`run::file` never calls it).
- the CLI's destination resolution (`resolve::job`, `crates/flux-cli/src/resolve.rs:33-67`, with the `DEST/` trailing-slash handling inline at `:51-64`) is unchanged and read-only.
The plan verifies each stage is read-only against the I1 method list; one that is not is split so its read-only half is reused. The branch then skips lock acquisition, `open_operation`, the workspace (and with it `probe_no_replace`), the claim store, the WAL, checkpoints, the batch writer threads, cleanup of priors, and the release. `Run.stop` stays `None`; refusals come from the pre-flight exactly as a real run's do.

### 3.3 The engine in dry mode (`crates/flux-core/src/tree.rs`)
One code path, parameterised by a mode carried in the copy context (`Shared`): `Live` (today) or `Dry`.
- Claims. Dry mode does NOT pass `claims: None`. That path (`tree.rs:1072-1092`, `:1527-1545`) is the cut 4b lockless fallback: it forces `Overwrite` plus `NoReplace`, turns every existing destination name into a collision, never runs `ExistingPolicy`, `update_replaces` or `same_link`, and skips the weak-directory-identity handling (`replace_degraded`, the `--safety=strict` refusal, `tree.rs:602-619`, `:975-994`). Instead dry mode runs the claim-present path with a null claim store: a production type holding no state whose `get` answers absent, `insert_if_absent` answers inserted, `upgrade_own_claim`, `prepare`, `commit_prepared`, `apply_recovery` do nothing, `supports_prepared` is false and `count` is 0. Same-run collisions are then invisible, which is exactly N2, and `claim_parent` is computed from the directory's identity strength exactly as a Live run's, so weak identities degrade and Strict refuses as they do. (The fake `FaultClaimStore` is O(entries) and is not used.)
- Names. A pre-existing destination directory gets the `NameIndex` a Live run builds for it (its listing, so a stored spelling can be resolved on a case-folding filesystem); this is the per-directory bound of G3. A directory that would be created gets an empty index and is `fresh`. `record_publication` has no effect to observe and is skipped.
- Frames. `Frame::Live.dir` becomes `Option<D>` (about 16 `Frame::Live` references, `tree.rs:308` and its matches; the plan counts them). `None` means "this destination directory does not exist yet and would be created"; such a frame is `fresh` (every target is New, no destination lookup, as for a directory a Live run just created). Entering a child of a `None` frame pushes a `None` frame. Entering a child of a `Some(parent)` frame calls `parent.open_dir(name)`: `Ok(child)` continues as for a pre-existing directory (the section 129, source-alias, mount and identity checks run unchanged); an error of kind `NotFound` plans a creation and pushes a `None` frame; any other error is the same `CreateDir` failure a real run reports, with the same code (`SafetyRejected` for a link at the name, `DestinationError` for a non-directory), and the subtree is `Frame::Skipped`.
- Root identity when DEST is absent. A Live run creates DEST first and compares source subdirectories against the new directory's identity. A dry run has no such handle: its root identity is "never equal to any source directory's identity, with the strength of the holder's own identity" (`locate_tree` already reads the holder's identity for the same purpose, `run/place.rs` near `:183-188`). The plan must not substitute `Unavailable`, which would degrade every subdirectory and abort a `--safety=strict` dry run a real run would pass.
- Per file and link (`copy_one`, `link_one`): the same source `metadata` / `read_link` and type check (I3), the same identity gate, the same `ExistingPolicy` / `same_link` decisions, then stop before staging. The decision functions (`update_replaces`, the skip rule, `same_link`) are the existing ones, called from both modes. Under a `None` parent the target is New and nothing is asked of the destination.
- Counters mean "would": `files_copied`, `bytes_copied`, `directories_created`, `files_overwritten`, `files_skipped`, `bytes_skipped`, `symlinks_skipped`, `mounts_skipped`, `special_files_skipped`, `files_degraded`, `replace_degraded` and the warnings are filled by the same code; the output marks them as a dry run (3.6).
- No batch: nothing is staged, so there is no flush, no worker thread, no `fsync`.
- DEST's own creation is counted per I7.

### 3.4 Single file (`crates/flux-core/src/copy.rs`)
`copy_file`'s target decision (exists / `--existing` policy / identity gate) is shared; in dry mode it returns the planned action instead of staging. `run::file` has no `on_report` callback and returns `flux_fs::Outcome`, which has no room for an action; the plan defines how the planned action and its object type reach the CLI (a field set only in dry mode, or a sibling entry point), and the CLI prints one stream line and the summary. A single file's `directories_created` is 0 and an absent parent directory is the pre-flight refusal a real run gets (`prepare_file`). Counters follow the real single-file report (`bytes_skipped` stays 0).

### 3.5 Prior operations (the S1 boundary, owner decision D2)
A real run first classifies prior operations at DEST and may refuse (`RESUMABLE_OPERATION_EXISTS`, `TARGET_LOCK_UNCERTAIN`, a live owner: spec 21.1, lines 1596-1640). A dry run cannot classify ownership without probing the lock (I4). In 11a it therefore reads prior state read-only and only reports it:
- tree: `prior::scan_tree(dest, ..)` (`crates/flux-core/src/prior.rs:39`) when DEST exists (when DEST is absent there is nothing to scan); single file: `prior::scan_file(parent, target_name)` (`:114`). Both read through `open_dir`, `read_dir`, `metadata` and `read_file` only.
- `Scan.resumable` priors: one note each, `note: a prior operation <id> in state <STATE> exists at DEST; --dry-run does not inspect its owner, so what a real run does with it (it may be refused with RESUMABLE_OPERATION_EXISTS) is not previewed`.
- `Scan.completed` priors: one note each, `note: an earlier completed operation <id> exists at DEST; a real run would finish its recorded cleanup`. (A dead COMPLETED prior is never refused, spec 21.1.)
- A scan that returns `Err` (`STATE_CORRUPT`, `INCOMPATIBLE_STATE`, a read error): one note naming the code and the record; the fresh plan still prints. Whether a real run refuses depends on the lock classification that comes first, which a dry run does not do. In 11a every scan outcome is a note and the exit code is unchanged.
This is a stated deviation from section 5.2's "or the refusal it would get", closed by cut 11b.

### 3.6 Reporting and output (O1)
- A new non-failure cause `TreeFailureCause::Planned { object_type, action }` (precedent: `SpecialFileSkipped`, `SymlinkSkipped`, `MountSkipped`, `tree.rs:177-206`), emitted only in dry mode, once per entry, through the existing `on_report` callback. `object_type` is `file | dir | symlink`; `action` is `CreateDir | Copy | Overwrite | Skip(SkipReason) | CreateSymlink`; `SkipReason` is `Existing` (`--skip-existing`) or `UpToDate` (`--update`). The existing skip causes keep their own causes (symlink skipped, special file, mount). It is not a failure and not counted by `FailureTally`, which is fixed-size and has no list, so a million entries cost nothing there. Exhaustive matches that must name it: `FailureTally::count` (`tree.rs`, "EXHAUSTIVE, with no wildcard arm"), `record_lines` and `first_detail` in `crates/flux-cli/src/report.rs`. The CLI's `on_report` consumer only formats lines (`crates/flux-cli/src/main.rs:326-331`); it counts nothing and does not touch the exit code.
- Stream lines (stderr, the existing `<path>: object_type=<..> action=<..>` style, `report.rs:187-214`): `action` in `would_create | would_copy | would_overwrite | would_skip | would_create_symlink`, a `would_skip` followed by its reason in parentheses. The line volume is O(entries); there is no quiet flag in 11a, and `--json` consumers read stdout.
- Summary (last line, stderr): `dry run: would copy N files (B bytes), overwrite O, skip S, create D directories; E errors`, where `E` is the report's `errors` (`report.rs:314`, which includes walk and create-dir failures), not `files_failed`. The real summary line is not printed.
- `--json` (stdout, section 53): the same object as a real run, plus `"dry_run": true` and, when non-empty, `"unprobed": [...]`. Both keys are omitted when false/empty, so the live 18-key object and its test (`report.rs:576`) are unchanged. The pre-engine stops in `main.rs:283-297` also carry `dry_run`. The counters keep their names with "would" meaning.
- Unprobed primitive (spec lines 10938-10942, test 124): a real TREE run always probes `rename_no_replace` in its workspace before copying (`run/place.rs:413`, the only caller) and is refused with `NOREPLACE_PUBLISH_UNAVAILABLE` if it is missing. A dry run that passes pre-flight prints once: `note: the no-replace publication primitive was not probed under --dry-run; a real run may be refused with NOREPLACE_PUBLISH_UNAVAILABLE`, and sets `"unprobed": ["noreplace_publish"]`. A single-file run has no probe, so its dry run prints no such note. Owner decision D3.
- Warnings and degraded groups (`DegradedGroup`) render as in a real run.

### 3.7 Exit codes
As the real run on the same predicted outcome (I7): 0 with no predicted failure; 1 when the plan contains a per-target failure; 2 for usage; 3 for a pre-flight refusal or a walk refusal that created nothing. Owner decision D4.

## 4. Edge cases and decisions made explicitly

- E1. DEST absent: the whole tree plans New; DEST counts per I7; no destination calls after pre-flight.
- E2. DEST exists and is a file: the pre-flight refusal a real run gets (`locate_tree`).
- E3. A directory where a file would go, or a file where a directory would go: the same `Gate` / `CreateDir` failures a real run reports.
- E4. Case-insensitive destinations: stored spellings resolve through the listing-based `NameIndex` as in Live; same-run folds are not predicted (N2): two source names that fold equal plan as two Creates where a real run reports one collision (spec lines 826-827).
- E5. Source errors that need only metadata (unreadable directory, `DirectoryChangedDuringScan`, an unsupported type) are reported as a real run reports them.
- E6. Reserved names (`.flux`), destination-inside-source and source-inside-destination, mounts (`Stay` vs `Cross`), links (`copy|skip`) are the same code.
- E7. A dry run against a destination holding a live operation does not look at it (I5).
- E8. A huge tree: one report per entry, O(1) memory per entry, streamed.
- E9. SIGINT during a dry run: nothing to clean up.
- E10. A 250-byte DEST name: `LockSite::directory` refuses with `PathComponentInvalid`, exit 3, as a real run does (3.2).

## 5. Failure table (what a dry run predicts)

| Real run outcome | Dry run |
|---|---|
| New target | `would_create` / `would_copy`, counted |
| Existing, `--existing=overwrite` | `would_overwrite` |
| Existing, `--skip-existing` | `would_skip (existing)` |
| Existing, `--update`, newer or different size | `would_overwrite`; else `would_skip (up to date)` |
| Link, `--links=skip` | the existing `SymlinkSkipped` cause |
| Source mount root, `Stay` | the existing `MountSkipped` cause; the empty directory counted in `directories_created` |
| Unreadable or vanished source | the real `Walk` / `CopyStep::Source` failure |
| Reserved name, containment, identity, weak-identity refusals | the real refusals (same code) |
| Namespace collision between two targets of the run | NOT predicted (N2) |
| Read error mid-file, disk full, source changed while copying | NOT predicted (N3) |
| Prior operation at DEST | a note (3.5); not previewed |
| `NOREPLACE_PUBLISH_UNAVAILABLE` (tree) | not probed; noted (3.6) |

## 6. Tests

The oracle for agreement is the real engine, run on the same inputs.
1. Paired test (the drift guard), on the fake and on the real filesystem under `crates/flux-core/tests/`: a corpus of `(source, initial destination, options)` over every `ExistingPolicy` (including `Update` with older, newer, equal-mtime and different-size files), `Links`, `Safety` with WEAK-identity destination directories (Default degrades and `replace_degraded` is counted; Strict refuses), mount events, reserved paths, a file where a directory goes and the reverse, an absent DEST under `--safety=strict`. For each: run dry, assert the destination snapshot is unchanged, run for real, then assert equal exit code, counters, failure list (code and path), and that the set of paths planned `would_create|would_copy|would_overwrite` equals the set of entries created or changed between the snapshots. Mutants the corpus must catch: ignore `Update`'s time rule in dry mode; treat every directory as pre-existing; skip the reserved-path check; pass `claims: None` (the existing-file fixtures go red); substitute `Unavailable` for the absent-DEST root identity (the strict fixture goes red); drop the source `metadata` call (`bytes_copied` and a vanished-source fixture go red).
2. Zero mutation and method set (I1): on the fake, a call-log assertion that every destination method called is in the I1 list and none outside it; on the real filesystem, a recursive snapshot (paths, types, sizes, mtimes, link payloads) unchanged, with no `.flux*` entry created.
3. No content reads and no lock (I3, I4): no `open_read` of a source file, no `try_lock`, no `create_lock`.
4. Bounded memory: a generated tree of many thousand entries; the dry run's peak tracked allocation is independent of the entry count (the plan names the measurement).
5. `None` frames: DEST absent; a nested absent subtree; a pre-existing directory containing an absent subdirectory; a link or file at a directory name (`SafetyRejected` / `DestinationError`).
6. Exit parity at the edges (I7): a walk abort on the first entry with DEST absent exits 3 in both modes and counts no DEST; a run that completes counts DEST; a 250-byte DEST name exits 3 in both.
7. `Planned` is not a failure: `FailureTally::total()` is 0 after a clean dry run; the three exhaustive matches name the new cause.
8. CLI: per-entry lines and the dry-run summary, never the real summary; `--json` carries `dry_run: true`, omits `unprobed` when the plan is for a single file, and a live run's JSON is byte-for-byte what it was (the existing 18-key test); exit codes 0/1/3; `--dry-run --resume|--restart|--break-lock` exits 2; a pre-engine stop under `--dry-run --json` carries `dry_run`.
9. Prior notes: a destination with a resumable prior, a completed prior, and a corrupt prior record each print their note and the fresh plan, exit unchanged.
10. Unprobed note: printed for a tree dry run, absent for a single file.
11. Single file: new, existing under each policy, `DEST/` trailing slash, a directory at the target, an absent parent (refusal), the planned action reaching the stream line.
12. State isolation (I6): the pinned fingerprint test stays green; no `state` file changes.

## 7. Decisions made (D) for the owner to check

- D1. One code path with a mode (M4), claim-present decisions run against a null claim store (3.3); not `claims: None`, not an overlay, not a separate planner.
- D2. Plain `--dry-run` with prior operations present prints notes and the fresh plan; every `scan_*` outcome is a note, exit unchanged. Cut 11b previews the real run's handling.
- D3. The unprobed note is printed for every tree dry run that passes pre-flight, never for a single file.
- D4. Exit codes follow the predicted real outcome (0/1/3), 2 for usage, including the rollback edge (I7).
- D5. Output reuses the stream line style and the section 53 object; `dry_run` and `unprobed` are emitted only when set; counters keep their names with "would" meaning; the summary's error count is the report's `errors`.
- D6. `RunConfig.dry_run`, not a `CopyOptions` field.
- D7. `--dry-run` with `--resume`, `--restart`, `--break-lock` is a usage error in 11a.
- D8. No quiet flag; one stderr line per entry.

## 8. Exhaustiveness self-audit

- Every section 5.2 sentence maps: no lock/workspace/catalog/partial/destination change (I1, 3.2); private temp for spilled planning (not needed: no planning state to spill, G3); runs while a real operation is live (I5); checks needing locks reported not performed (3.5, 3.6); `--resume` / `--restart` previews (N1, cut 11b); no collision prediction (N2).
- Acceptance tests: 59 (I1, G3, tests 2 and 4); 124 (3.6, test 10); 79 and 136 (cut 11b); 110 (`--atomic`, not built).
- Placeholders: none. Left to the PLAN (named, none changes a contract above): the exact `Shared` field and enum for the mode; the module holding `PlannedAction` and the null claim store; how the single-file planned action reaches the CLI (3.4); which pre-flight stage, if any, needs a read-only split; the bounded-memory measurement; how `parent: &D` parameters become `Option<&D>` for `None` frames.
- Risks recorded: mode branches in the 6,600-line `tree.rs` hot path (guarded by the paired test and its mutants); `Option<D>` frames touch about 16 references; the null claim store must satisfy the whole `ClaimStore` trait (the plan reads its methods).

## 9. Review record

- Builder check (fresh Sonnet, 2026-10-17): six questions; folded: the `claims: None` path is not the claim-present path (3.3, D1); the `NameIndex` bound (G3, 3.3); per-entry report volume (3.6, D8); counters follow the real run (I2); the I1 method list and `read_file` (I1, 3.5); single-file absent parent and `directories_created` (3.4). Anchor corrections: `tree.rs:407` is the field, the behaviour is at `:1072-1092`.
- Agy panel round 1 (`.clavity/scratch/dry-run-panel/r1-reply.md`, `PANEL VERDICT: FINDINGS`): Part A 42 citations, 6 do not hold, all re-measured by the driver and fixed: `prior::find_in_tree` / `find_for_file` do not exist (the functions are `scan_tree` / `scan_file`, an error carried over from the consult reply without checking); `resolve::resolve_file_dest` does not exist (inline in `resolve::job`); `CopyArgs` is at `main.rs:55-115`; `run::file` has no `check_control_plane`; the I1 list was incomplete; the claims claim (above). Findings folded: AB-1 (critical), AB-2, MG-1, MG-2, PP-1, PP-2, CA-1, BS-1, BA-1 (3.2, 3.3, 3.4, 3.5, 3.6, I1, I3, I7, tests). Rejected after measurement: the claim that `out.failures.recent` grows with the tree (there is no such field; `FailureTally` is fixed-size, `tree.rs:208-217`); the prediction that an empty `NameIndex` makes every existing entry `Unresolvable` is true only for a weak identity (`names.rs:99-112` lists the directory for a strong one), which is why dry mode keeps the Live index rather than an empty one. Discarded below the floor: the line count of `tree.rs`; naming a counter field next to an enum variant in the failure table.
