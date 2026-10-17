# Cut 11a: `flux copy --dry-run` (design)

Status: DRAFT for owner review. Written against `origin/main` 5b0a622 (PR #107 open, touching only `crates/flux-platform` and docs). Master spec: `FLUX_FULL_UPDATED_SPEC_V16.md` section 5.2 (lines 794-828), invariants 14, 38, 57, 109, acceptance tests 59, 79, 110, 124, 136. The owner chose, after AGY-FIRST (brief `.clavity/seams/dry-run-fork-1.md`, reply `.clavity/scratch/dry-run-1/r1-reply.md`, ALIGNED): mechanism M4 (a dry mode inside the shared engine), scope S1 (fresh-plan preview only), output O1 (per-entry lines, a summary and `--json`).

Line anchors below were read at 5b0a622 and are for orientation; the plan re-reads every one before using it.

## 1. Purpose, goals, non-goals

Purpose: `flux copy --dry-run SOURCE DEST` reports, for every entry the real run would handle, the action the real run would take, and changes nothing: no lock, no workspace, no state, no claim, no partial file, no destination change (section 5.2, invariant 14, acceptance test 59).

Goals:
- G1. The dry run and the real run that follows it make the same decisions. The decision code is shared, not copied (the AGY-FIRST consult found about six enforced checks a separate planner would duplicate: reserved control-plane paths, the section 129 cycle checks, source mount events, safety degradation accounting, the non-directory-at-a-directory-name refusals, single-file target resolution).
- G2. A tree copy and a single-file copy are both covered.
- G3. Resident memory is bounded exactly as a real run's walk is (acceptance test 59): the dry run holds no claim table, no name index beyond the one directory being listed, and no per-file state.
- G4. The output states what would happen per entry, a summary, and `--json`, in the existing formats extended minimally.

Non-goals (each is a later cut or a stated limit, never silent):
- N1. `--dry-run --resume`, `--dry-run --restart`, `--dry-run --break-lock` (spec lines 810-824, tests 79 and 136) are cut 11b. In 11a they are a usage error (exit 2) naming the later cut, as `--resume-verify=metadata` is refused today (`crates/flux-cli/src/main.rs:271-275`).
- N2. Predicting `DESTINATION_NAMESPACE_COLLISION` (spec lines 826-827, invariant 109).
- N3. Predicting failures that only occur while bytes move (a read error mid-file, a full disk, a source changing during the copy). The dry run reads metadata, never file content.
- N4. `--dry-run` for `flux cleanup` (already exists) and for any other subcommand.
- N5. Several sources and `--exclude` (separate cuts); a dry run is defined for what `flux copy` accepts today.

## 2. Invariants

- I1 (read-only). A dry run performs no destination mutation and takes no lock. It calls only read operations of `FileSystem` / `DirHandle` on the destination: `metadata`, `open_dir`, `read_dir`, `read_link`, `destination_root`. It never calls `create_dir`, `create_new`, `create_lock`, `rename*`, `remove*`, `create_claim_store`, `open_claim_store`, `open_lock`, `open_read` on a destination file, or any write. Test: a call-log assertion on the fake and a before/after tree snapshot on the real filesystem.
- I2 (agreement). For every fixture without a same-run namespace collision, the dry run's exit code, counters, failure list and per-entry actions equal the real run's on the same untouched inputs (section 6, the paired test).
- I3 (no content reads). A dry run opens no source file for reading and hashes nothing; `bytes_*` counters come from source metadata lengths.
- I4 (no lock probing). A dry run takes no OS lock, not even a trial `try_lock` (`crates/flux-core/src/lock/classify.rs:39` does, which is why it is not reused).
- I5 (live runs). A dry run may run while a real operation is live and its report is a snapshot (spec lines 805-808). It does not look at the lock.
- I6 (flag isolation). `dry_run` is not part of `CopyOptions`' persisted subset, `state::Options`, the fingerprint, or any on-disk record: a dry run writes none.

## 3. Design

### 3.1 CLI and configuration
- `CopyArgs` (`crates/flux-cli/src/main.rs:34-97`) gains `--dry-run` (bool). `flux cleanup` already has its own.
- `--dry-run` with `--resume`, `--restart` or `--break-lock`: usage error, exit 2, message `--dry-run does not combine with --resume, --restart or --break-lock in this version (cut 11b)`. Everything else combines (`--existing`, `--links`, `--safety`, `--cross-filesystems`, `--durability`, `--json`, `--preserve-*`): options that change what a run decides are honoured; options that only change how bytes are written (`--durability`, `--resume-verify`) are accepted and have no effect.
- The engine switch is `RunConfig.dry_run: bool` (`crates/flux-core/src/run/mod.rs:55`), not a `CopyOptions` field, because it is a property of the invocation like `resume`, not of the copy's persisted configuration (I6). Every `RunConfig` literal gains the field (the compiler finds them; the plan lists them).

### 3.2 Entry and pre-flight
`run::tree` and `run::file` (`run/mod.rs:227`, `:488`) branch on `cfg.dry_run` after the read-only pre-flight stages, which are reused verbatim and are read-only: tree `prepare_source`, `locate_tree` (returns `dest: None` while DEST does not exist, without creating it), `check_capability`, `check_control_plane`; file `prepare_file`, `fs.destination_root`, `check_capability`, `check_control_plane`, `file_names_fit`. The plan verifies each is read-only before relying on it; one that is not is split so its read-only half is reused. The branch then skips lock acquisition, `open_operation`, the workspace, the claim store, the WAL, checkpoints, the batch writer threads, cleanup of priors, and the release. `Run.stop` stays `None`; refusals come from the pre-flight exactly as a real run's do (exit 3 / 1 as today).

### 3.3 The engine in dry mode (`crates/flux-core/src/tree.rs`)
One code path, parameterised by a mode carried in the copy context (`Cx`): `Live` (today) or `Dry`.
- Claims and names: in `Dry` mode `cx.claims` is `None` (the engine already runs without claims: `claims: Option<&RefCell<..>>`, `tree.rs:407`) and no `NameIndex` is populated beyond what the decision code needs for the one directory being processed.
- Frames: `Frame::Live.dir` becomes `Option<D>`; `None` means "this destination directory does not exist yet and would be created". A frame with `dir: None` is `fresh` (every target is planned New, no destination lookup, as the engine already treats a directory this run created). Entering a child of a `None` frame pushes a `None` frame; entering a child of a `Some(parent)` frame calls `parent.open_dir(name)`: `Ok(child)` continues as for a pre-existing directory (the section 129, source-alias, mount and identity checks run unchanged); an error whose kind is `NotFound` plans a creation and pushes a `None` frame; any other error is the same `CreateDir` failure a real run reports, with the same code (`SafetyRejected` for a link at the name, `DestinationError` for a non-directory), and the subtree is `Frame::Skipped`.
- Per file and link (`copy_one`, `link_one`): run the same source `metadata` / `read_link`, the same identity gate and the same `ExistingPolicy` / `same_link` decisions, then stop before staging. The decision functions (`update_replaces`, the skip rule, `same_link`) are the existing ones, called from both modes. Where the destination parent is `None` the target is New.
- Counters in `Dry` mode mean "would": `files_copied`, `bytes_copied`, `directories_created`, `files_overwritten`, `files_skipped`, `bytes_skipped`, `symlinks_skipped`, `mounts_skipped`, `special_files_skipped`, `files_degraded`, `replace_degraded` and the warnings are filled by the same code. The JSON and summary say `dry_run` so a reader never mistakes them for done work (section 3.6).
- The batch: no staging, so no batch, no flush, no worker threads, no `fsync` (the `Batch` stays empty).
- `directories_created` counts DEST itself when it would be created (a real run counts it, `tree.rs:35`).

### 3.4 Single file (`crates/flux-core/src/copy.rs`)
`copy_file`'s target decision (exists / `--existing` policy / identity gate) is shared; in `Dry` mode it returns the planned action instead of staging. A `DEST/` trailing-slash target is resolved by `resolve::resolve_file_dest` as today (read-only).

### 3.5 Prior operations (the S1 boundary, owner decision D2)
A real run first classifies prior operations at DEST and may refuse (`RESUMABLE_OPERATION_EXISTS`, `TARGET_LOCK_UNCERTAIN`, a live owner: spec 21.1, lines 1596-1640). A dry run cannot classify ownership without probing the lock (I4). In 11a it therefore:
- reads prior state read-only with `prior::find_in_tree` / `prior::find_for_file` (`crates/flux-core/src/prior.rs:146`, `:219`; they use `open_dir` and `read_file` only);
- if any prior operation exists, prints one note per prior (`note: a prior operation <id> in state <STATE> exists at DEST; --dry-run does not inspect its owner, so a real run's handling of it (it may be refused with RESUMABLE_OPERATION_EXISTS) is not previewed`) and still prints the fresh plan;
- does not change the exit code for it.
This is a stated deviation from section 5.2's "or the refusal it would get", closed by cut 11b. A prior-state read error (corrupt record) is reported as the real run's `STATE_CORRUPT` refusal would be, because that needs no lock; the plan decides whether the read-only half is cheap enough for 11a, otherwise it is a note too.

### 3.6 Reporting and output (O1)
- A new non-failure cause `TreeFailureCause::Planned(PlannedAction)` (precedent: `SpecialFileSkipped`, `SymlinkSkipped`, `MountSkipped`, `tree.rs:177-206`), emitted only in `Dry` mode, once per entry, through the existing `on_report` callback. `PlannedAction` is a closed enum: `CreateDir`, `Copy`, `Overwrite`, `Skip(SkipReason)`, `CreateSymlink`, plus the existing skip causes keep their own causes (symlink skipped, special file, mount). `SkipReason` is `Existing` (`--skip-existing`), `UpToDate` (`--update`). Not counted by `FailureTally`.
- Stream lines (stderr, the existing `<path>: object_type=<..> action=<..>` style of `crates/flux-cli/src/report.rs:187-214`): `object_type` in `file|dir|symlink|mount|special`; `action` in `would_create|would_copy|would_overwrite|would_skip|would_create_symlink|skipped`. A `would_skip` carries its reason in parentheses.
- Summary (last line, stderr): `dry run: would copy N files (B bytes), overwrite O, skip S, create D directories; F would fail`. The real summary line is not printed.
- `--json` (stdout, section 53 fields, `crates/flux-cli/src/report.rs:550-569`): the same object as a real run plus `"dry_run": true`; the counters carry their "would" meaning; `duration_ms` is the dry run's.
- Unprobed primitive (spec lines 10938-10942, test 124): when the plan contains at least one planned publication that a real run publishes with the no-replace primitive (every New target, and `--skip-existing`), one line: `note: the no-replace publication primitive was not probed under --dry-run; a real run may be refused with NOREPLACE_PUBLISH_UNAVAILABLE`. In JSON: `"unprobed": ["noreplace_publish"]` (omitted when empty). Owner decision D3 (conservative: always when a publication is planned).
- Warnings and degraded groups (`DegradedGroup`) render as in a real run.

### 3.7 Exit codes
The dry run exits as the real run would on the same predicted outcome: 0 with no predicted failure; 1 when the plan contains a per-target failure (a real run's `files_failed > 0` or any predicted `CreateDir` failure); 2 for usage; 3 for a pre-flight refusal. This is what makes the paired test (I2) compare exit codes. Owner decision D4.

## 4. Edge cases and decisions made explicitly

- E1. DEST absent: the whole tree plans New; DEST counts in `directories_created`; no parent lookups after the first `None` frame.
- E2. DEST exists and is a file: the pre-flight refusal a real run gets (`locate_tree`).
- E3. A destination entry that is a directory where a file would go, or a file where a directory would go: the same `Gate` / `CreateDir` failures a real run reports.
- E4. Case-insensitive destinations: the decision uses the destination's own `metadata`/`open_dir` answers (the OS folds); no `NameIndex` is built (no collision prediction, N2). A dry run may therefore plan two source names that fold equal as two Creates where a real run reports one collision (spec lines 826-827, stated, not a defect).
- E5. Source errors that need only metadata (unreadable directory, `DirectoryChangedDuringScan`, an unsupported type) are reported exactly as a real run reports them.
- E6. Reserved names (`.flux`), the destination-inside-source / source-inside-destination checks, mounts (`Stay` vs `Cross`), links (`copy|skip`) behave as a real run's; they are the same code.
- E7. A dry run against a destination holding a live operation does not look at it (I5).
- E8. An enormous tree: one `Planned` report per entry is O(1) memory per entry (streamed). The summary line has fixed size.
- E9. `--durability` and `--resume-verify`: accepted, no effect.
- E10. SIGINT during a dry run: nothing to clean up; the process exits as it does today.

## 5. Failure table (what a dry run predicts)

| Real run outcome | Dry run |
|---|---|
| New target | `would_create` / `would_copy`, counted |
| Existing, `--existing=overwrite` | `would_overwrite` |
| Existing, `--skip-existing` | `would_skip (existing)` |
| Existing, `--update`, newer or different size | `would_overwrite`; else `would_skip (up to date)` |
| Link, `--links=skip` | the existing `symlinks_skipped` cause |
| Source mount root, `Stay` | the existing `MountSkipped` cause, empty directory counted in `directories_created` |
| Unreadable source directory | the real `Walk` failure |
| Reserved name, containment, identity refusals | the real refusals (same code) |
| Namespace collision between two targets | NOT predicted (N2) |
| Read error mid-file, disk full, source changed while copying | NOT predicted (N3) |
| Prior operation at DEST | a note (3.5); not previewed |
| `NOREPLACE_PUBLISH_UNAVAILABLE` | not probed; noted (3.6) |

## 6. Tests

The oracle for agreement is the real engine, run on the same inputs.
1. Paired test (the drift guard), on the fake and on the real filesystem in `crates/flux-core/tests/`: a corpus of `(source, initial destination, options)` over every `ExistingPolicy` (including `Update` with older, newer, equal-mtime and different-size files), `Links`, `Safety` (with weak identities), mount events, reserved paths, a file where a directory goes and the reverse. For each: run dry, assert the destination snapshot is unchanged, run for real, then assert equal exit code, counters, failure list (code and path), and that the set of paths planned `would_create|would_copy|would_overwrite` equals the set of entries created or changed between the snapshots. Mutants: in `Dry` mode ignore `Update`'s time rule; treat every directory as pre-existing; skip the reserved-path check.
2. Zero mutation (I1): a call-log test on the fake that no mutating method is called, and a real-filesystem recursive snapshot (paths, types, sizes, mtimes, link payloads) unchanged, with no `.flux*` entry anywhere.
3. No content reads and no lock (I3, I4): the call log has no `open_read` of a source file and no `try_lock`.
4. Bounded memory: a generated tree of many thousand entries; the dry run's peak tracked allocation is independent of the entry count (the plan names the measurement; same style as the walk's bounded-memory test if one exists).
5. `None` frames: DEST absent; a nested absent subtree; a pre-existing directory containing an absent subdirectory; a link or file at a directory name (`SafetyRejected` / `DestinationError`).
6. CLI: `--dry-run` prints per-entry lines and the summary, never the real summary; `--json` carries `dry_run: true`; exit codes 0/1/3; `--dry-run --resume|--restart|--break-lock` exits 2.
7. Prior note: a destination with a resumable prior prints the note and still the plan, exit unchanged.
8. Unprobed note: present when a publication is planned, absent when the plan has none.
9. Single file: new, existing + each policy, `DEST/` trailing slash, a directory at the target.
10. State isolation (I6): no `state`/fingerprint code changes; the pinned fingerprint test stays green.

## 7. Decisions made (D) for the owner to check

- D1. One code path with a mode (M4) instead of a planner (M3) or an overlay (M1/M2): from the consult, verified against `tree.rs` (claims already optional; `fresh` frames exist).
- D2. Plain `--dry-run` with a prior operation present prints a note and the fresh plan; it does not preview the real run's refusal (cut 11b does).
- D3. The unprobed note is printed whenever a publication is planned.
- D4. Exit codes follow the predicted real outcome (0/1/3), 2 for usage.
- D5. Output reuses the stream line style and the section 53 JSON object, extended by `dry_run` and `unprobed`; counters are reused with "would" meaning.
- D6. `RunConfig.dry_run`, not a `CopyOptions` field.
- D7. `--dry-run` combined with `--resume`, `--restart`, `--break-lock` is a usage error in 11a.

## 8. Exhaustiveness self-audit

- Every spec 5.2 sentence maps: no lock/workspace/catalog/partial/destination change (I1, 3.2); private temp for spilled planning (not needed: no planning state, G3; stated here so it is not read as a gap); runs while a real operation is live (I5); checks needing locks reported not performed (3.5, 3.6); `--resume` / `--restart` previews (N1, cut 11b); no collision prediction (N2).
- Acceptance tests: 59 (I1, G3, tests 2 and 4); 124 (3.6, test 8); 79 and 136 (cut 11b); 110 (`--atomic`, not built; out of scope).
- Placeholders: none. Items resolved in the PLAN, not here: the exact `Cx` field for the mode, the `PlannedAction` module location, which pre-flight stages need a read-only split, the bounded-memory measurement, whether a corrupt prior record is a refusal or a note.
- Risks recorded: mode branches in the 6,600-line `tree.rs` hot path (guarded by the paired test); `Option<D>` frames touch the 13 uses of `Frame::Live`.
