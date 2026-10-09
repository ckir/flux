# Cut 9b: the cleanup lifecycle - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux cleanup DEST` lists and removes what earlier directory operations left under `DEST/.flux/operations/` and beside DEST, and a
normal `flux copy` finishes a prior COMPLETED operation's recorded cleanup, both without ever deleting anything a live or ambiguous owner
might still need.

**Architecture:**
- **Classification** is a pure table (`cleanup/status.rs`) over facts gathered lock-free (`cleanup/discover.rs`); the only touch of the lock
  while classifying is one `try_lock` that is dropped at once (spec decision 5).
- **Deletion** (`cleanup/delete.rs`) takes DEST's root lock through a cleanup variant of the acquire path (`lock::obtain_cleanup_lock`, which
  reclaims a crash-orphaned lock by the section 240.3 move-aside), re-classifies each row under the lock, and deletes in the section 223 order:
  leftovers first, the workspace last; a STALE operation is marked ABANDONED and its partials are swept by the J1 walk `--restart` already uses.
- **The copy** reuses the same deletion for COMPLETED priors and debris inside `open_operation`, under the lock it already holds; every failure
  is a warning.
- **The CLI** gains `Commands::Cleanup` with `--dry-run`, `--force`, `--json`, a table renderer, and exit codes 0/1/2/3.

**Tech Stack:** Rust 2024 (MSRV 1.98.1), `clap`, `serde_json`, the `FaultFs` fake, nextest via `just`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-08-cut-9b-cleanup-design.md` (approved 2026-10-09 at `5fafeff`). Executors read it with this plan;
where the two differ the spec wins and the executor reports the conflict.

## Global Constraints

- Rust edition 2024; `rust-version = "1.98.1"` (`Cargo.toml:11`). The gate is `just check` = `cargo fmt --check` +
  `cargo clippy --workspace --all-targets -- -D warnings` + `typos` + `cargo nextest run --workspace --no-tests=pass` +
  `cargo test --doc --workspace`. Push after every task and read CI (Windows-only failures surface only there).
- Every write under DEST or beside it goes through `DirHandle`s, one component at a time, never following a link (section 149.7; the existing
  `remove_below` in `run/place.rs:552` is the pattern).
- Section 99 before every destructive step: the heartbeat, then the ownership check (`session::owned`); a check that cannot tell counts as
  lost and stops the pass. Classification and `--dry-run` make no destructive step and write nothing.
- Never deleted: a `LIVE`, `UNCERTAIN` or `CORRUPT` row; anything whose name is not in a known list (no recursive deletion of a workspace);
  a `cleanup_pending_artifacts` entry that fails the artifact rules (spec "Artifact rules" 1-4).
- `--force` bypasses retention only. The 30 s lease gate and the clock rule are never bypassed.
- Constants: `DEFAULT_RETENTION` = 7 days, `LEASE_THRESHOLD` = 30 s, both in `cleanup/mod.rs`; no flag and no environment variable sets
  either (spec decision 3). Tests set `CleanupConfig` fields.
- The copy's report, its 18 JSON keys (`crates/flux-cli/src/report.rs:401`, `the_json_has_every_section_53_field_in_order`) and its exit code
  never change because of a prior's cleanup.
- Message texts are the spec's where it gives them ("Output", "What `flux copy` does"); tests pin them by substring.
- Every new test goes red under a one-line mutant of the code it guards (the implementer names the mutant in the report; the controller
  measures a sample).
- Commit messages: `feat:` / `test:` / `docs:` prefixes, one commit per task's "Commit" step. Every reviewer and implementer dispatch forbids
  `git checkout`, `git switch`, `git stash`, `git reset`.

## Decisions this plan makes (the spec leaves them to the plan)

1. **`Facts` and the pure table.** `cleanup::status::classify(&Facts, &Thresholds) -> Verdict` with the structs of Task 2. Ages are whole
   seconds (`u64`); the thresholds are seconds too (`Thresholds { retention_secs: 604_800, lease_secs: 30 }` from the config's durations).
2. **The J1 walk is extracted**, not copied: `run/sweep.rs::sweep_partials` is the body of today's `TreePlace::sweep`
   (`run/place.rs:406-470`) with its check and its reporting abstracted; `TreePlace::sweep` becomes a six-line call. The existing `--restart`
   tests (`run/tests.rs:388`, `:410`, `:1005`, `:1024`, `:2757`) are the oracle for the extraction.
3. **`finish_retire` is extracted** from `state::retire_workspace` (`state.rs:862-880`): the "remove the known names, then the directory"
   half, used for `<id>.removing` and `<id>.creating` debris and by `retire_workspace` itself. The known names are exactly
   `[MANIFEST, temp_name(MANIFEST), PROBE, PROBE_TEMP, STATE_DB]` (what `retire_workspace` lists today). A directory still not empty after
   that is `kept` with its error; nothing else inside is ever removed.
4. **`locate_dest` is extracted** from `run/place.rs::locate_tree` (`:128-188`): the holder/name/dest resolution without the source
   pre-flight and containment, so `flux cleanup` opens DEST and its parent exactly as `flux copy` does.
5. **`Classified::Orphan`**: `lock::classify` returns a new variant `Orphan { lock, identity, record }` for `Workspace::Missing` (today folded
   into `Untrusted(record)`, `classify.rs:50`), keeping the handle and the OS lock as `Dead` does. `obtain` maps it to the existing
   `ARTIFACT_OWNERSHIP_UNCERTAIN` refusal with the pointer sentence appended; `obtain_cleanup_lock` recovers it under the lease gate.
6. **`session::checked` is split**: `checked_held(held, lock_shown, pulse) -> Result<(), Fault>` does the work; `checked(locked)` delegates.
   The cleanup pass owns a `CleanupLock { held, lock_shown, pulse }` and uses `checked_held`.
7. **The copy reports through two new `RunWarning` variants**: `PriorCleaned { operation_id: String, removed: u64 }` (rendered as the spec's
   `note:` line) and `PriorCleanupFailed { operation_id: String, path: PathBuf, error: FsError }` (the spec's `warning:` line). `Run` gains
   no field.
8. **A `.broken.*` file beside the lock** is classified by its own record: free, decodable and past the lease gate is `STALE` and eligible
   whether or not the workspace its record names still exists (that workspace, if present, is its own row); busy is `LIVE`; undecodable is
   `UNCERTAIN`. (Spec row 11 names "the workspace it names is missing" for the root lock itself, which is reclaimed by the acquisition;
   a moved-aside file is never the live lock path.)
9. **`CleanupConfig` carries `now: u64`** (nanoseconds since the epoch, `state::wall_time_ns()` in production) and the run-style ids and hooks
   (`operation_id`, `owner_instance_id`, `boot_session_id`, `before_mutation`, `heartbeat_interval`), so the cleanup's lock record and the
   kill test's stall hook work exactly as `RunConfig`'s do (`flux-cli/src/main.rs:150-190`).
10. **Exit 1 is derived**: `CleanupReport::failed()` is true when any action is `Kept`, when a listing failed, or when the lock was lost
    mid-pass. A busy lock is exit 0 with every eligible row `skipped`.
11. **Discovery order** is the sorted listing of `operations/`, then the root lock, then its `.broken.*` files sorted by name; the report
    prints entries in that order and actions in the order taken (spec "Order inside a pass").
12. **A symlink at DEST** is refused as `locate_tree` refuses it (`SAFETY_REJECTED`, `run/place.rs:155-162`), exit 3; the spec names only
    `DESTINATION_ERROR` for "not a directory" and is silent on a link.

## Review Focus

1. A COMPLETED prior whose artifact list names `a.flux-partial.<OTHER id>` or a plain user file: nothing is deleted and the row is `kept`
   (Task 1, `an_artifact_naming_another_operations_partial_or_a_user_file_is_refused`).
2. A RESUMABLE operation whose manifest mtime is 7 days old but whose lock record (free, dead owner) has a heartbeat 10 s old: `UNCERTAIN`
   is wrong and `STALE` is wrong; it is `RESUMABLE`, never eligible, because the lease is 10 s (Task 2,
   `a_young_lease_keeps_an_old_workspace_resumable`).
3. `flux cleanup DEST` while a `flux copy` into DEST is running: the operation is `LIVE`, nothing is deleted, exit 0, and the copy is not
   disturbed (Task 7, `a_cleanup_during_a_live_copy_lists_it_live_and_deletes_nothing`).
4. `flux cleanup` killed between the ABANDONED write and the workspace retirement: the next cleanup lists the row `STALE` and finishes it
   (Task 9, `a_cleanup_killed_after_abandoning_finishes_on_the_next_run`).
5. A copy whose prior's leftover cannot be removed: the copy still exits 0, its JSON has 18 keys, and the warning names the operation
   (Task 6, `a_prior_leftover_that_cannot_be_removed_warns_and_the_copy_succeeds`).

---

### Task 1: The artifact rules (`flux-core/src/cleanup/artifacts.rs`)

**Files:**
- Create: `crates/flux-core/src/cleanup/mod.rs` (the module skeleton, constants and `CleanupConfig`; the rest of its items land in later
  tasks), `crates/flux-core/src/cleanup/artifacts.rs`
- Modify: `crates/flux-core/src/lib.rs:3-11` (add `pub mod cleanup;`)
- Test: `crates/flux-core/src/cleanup/artifacts.rs` (unit tests, `FaultFs`)

**Interfaces:**
- Consumes: `state::from_native_hex` (`state.rs:646`), `state::PARTIAL_INFIX` (`:733`), `state::id_after` (`:739`), `DirHandle` (`flux-fs/src/fs.rs:226`).
- Produces:
  ```rust
  // cleanup/mod.rs
  pub mod artifacts;
  pub const DEFAULT_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
  pub const LEASE_THRESHOLD: Duration = Duration::from_secs(30);
  pub struct CleanupConfig {
      pub retention: Duration,
      pub lease_threshold: Duration,
      /// Nanoseconds since the Unix epoch, UTC (`state::wall_time_ns()` in production; tests pin it).
      pub now: u64,
      pub force: bool,
      pub dry_run: bool,
      pub operation_id: String,
      pub owner_instance_id: String,
      pub boot_session_id: String,
      pub before_mutation: Option<crate::run::BeforeMutation>,
      pub heartbeat_interval: Duration,
  }
  // cleanup/artifacts.rs
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum ArtifactProblem { NotHex, Absolute, BadComponent, NotThisOperationsPartial }
  /// Rules 1 and 2: a relative path of normal components whose last is `<name>.flux-partial.<operation_id>`.
  pub fn validate(hex: &str, operation_id: &str) -> Result<PathBuf, ArtifactProblem>;
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Removed { Deleted, AlreadyGone }
  /// Rules 3 and 4: open each directory component with `open_dir`, check the last is a regular file by `metadata`,
  /// `remove_file` it. `NotFound` anywhere is `AlreadyGone`. A link, a directory or anything else is the error.
  pub fn remove_validated<D: DirHandle>(dest: &D, rel: &Path) -> flux_fs::Result<Removed>;
  ```

- [ ] **Step 1: Write the failing tests** in `artifacts.rs`:
  - `a_relative_partial_of_this_operation_validates`: `validate(&native_hex(Path::new("sub/b.flux-partial.<ID>")), ID)` is
    `Ok(PathBuf::from("sub/b.flux-partial.<ID>"))`.
  - `an_absolute_path_is_refused`: `native_hex(Path::new("/etc/passwd.flux-partial.<ID>"))` gives `Err(ArtifactProblem::Absolute)`
    (on Windows use `C:\\x.flux-partial.<ID>`; gate with `cfg`).
  - `dot_dot_and_dot_and_empty_components_are_refused`: `../b.flux-partial.<ID>`, `sub/../b.flux-partial.<ID>` and `./b.flux-partial.<ID>` each
    give `Err(BadComponent)`; a hand-built hex with an empty component (`"2f2f"` style, two separators) gives `Err(NotHex)` (`from_native_hex`
    returns `None`).
  - `an_artifact_naming_another_operations_partial_or_a_user_file_is_refused`: `b.flux-partial.<OTHER>` and `report.doc` give
    `Err(NotThisOperationsPartial)`.
  - `remove_validated_deletes_through_handles_and_tolerates_absence`: on a `FaultFs` with `/dest/sub/b.flux-partial.<ID>`, `remove_validated`
    returns `Deleted` and the file is gone; a second call returns `AlreadyGone`; the call log shows `open_dir(/dest/sub)` then
    `remove_file(/dest/sub/b.flux-partial.<ID>)` and never a `remove_file(/dest/sub/...)` by full path from `/dest` (the handle walk).
  - `remove_validated_refuses_a_link_component_and_a_directory_leaf`: `fs.add_symlink("/dest/sub")` makes it an error whose code is
    `SafetyRejected` or `DestinationError`, and the target stays; a directory at the leaf name is an error and stays.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core cleanup::artifacts`
Expected: compile errors naming `cleanup`.

- [ ] **Step 3: Implement** `mod.rs` (skeleton, constants, `CleanupConfig`), `validate` (decode with `from_native_hex`; reject
  `Component::RootDir | Prefix | CurDir | ParentDir`; the last component must satisfy `id_after(name, PARTIAL_INFIX) == Some(operation_id)`),
  `remove_validated` (the handle walk; `NotFound` from `open_dir`, `metadata` or `remove_file` is `AlreadyGone`; a leaf whose
  `file_type != FileType::File` is `Err(FsError::new(Code::SafetyRejected, ..))`).
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/lib.rs crates/flux-core/src/cleanup
git commit -m "feat: cleanup artifact rules - validated relative partial names, deleted through handles (cut 9b)"
```

---

### Task 2: The status table (`flux-core/src/cleanup/status.rs`)

**Files:**
- Create: `crates/flux-core/src/cleanup/status.rs`
- Modify: `crates/flux-core/src/cleanup/mod.rs` (`pub mod status;`, the shared enums below)
- Test: `crates/flux-core/src/cleanup/status.rs` (pure, no filesystem)

**Interfaces:**
- Consumes: `state::OpState` (`state.rs:173`).
- Produces:
  ```rust
  // cleanup/mod.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Status { Live, Resumable, Stale, CompletedButUnclean, Uncertain, Corrupt }
  impl Status { pub fn as_str(self) -> &'static str } // "LIVE" "RESUMABLE" "STALE" "COMPLETED_BUT_UNCLEAN" "UNCERTAIN" "CORRUPT"
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum EntryKind { Operation, Debris, RootLock }
  impl EntryKind { pub fn as_str(self) -> &'static str } // "operation" "debris" "root-lock"
  // cleanup/status.rs
  /// A reading of a time against `now`.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Age { Seconds(u64), Future, Unreadable }
  /// The one probe of the lock (spec decision 5).
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum LockProbe {
      Absent,
      /// `try_lock` succeeded and was dropped; `record` is the decoded record if the bytes were one.
      Free { record: Option<LockRecordFacts> },
      /// Another process holds it; `names` is the record's `operation_id` when readable.
      Busy { names: Option<String> },
      /// Torn, empty, newer-versioned or not a Flux record, and nobody holds it.
      Unreadable,
  }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct LockRecordFacts { pub operation_id: String, pub workspace_missing: bool, pub lease_age: Age }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum ManifestProblem { Missing, NotRegular(String), Corrupt(String), WrongOperation, UnknownFormat(u64) }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Subject {
      Operation { id: String, manifest: Result<OpState, ManifestProblem>, manifest_age: Age },
      Debris,
      /// The root lock itself (`moved_aside: false`), or a `<lock>.broken.*` file (`true`); its record facts travel in `LockProbe`.
      RootLock { moved_aside: bool },
  }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Facts { pub subject: Subject, pub lock: LockProbe, pub force: bool }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct Thresholds { pub retention_secs: u64, pub lease_secs: u64 }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Verdict { pub status: Status, pub eligible: bool, pub note: String, pub age_seconds: Option<u64> }
  pub fn classify(facts: &Facts, t: &Thresholds) -> Verdict;
  ```
  The lease age of an `Operation` is `record.lease_age` when `lock` is `Free { record: Some(r) }` with `r.operation_id == id`, else
  `manifest_age`. "Lock free" for eligibility is `LockProbe::Absent | Free { .. }`. When `lock` is `Busy { names }` and `names != Some(id)`
  (or `None`), every verdict is ineligible and its note is `destination lock held by <names or "unknown">`. `age_seconds` is the retention
  age (`manifest_age`) for an operation and the lease age for a `RootLock`, `None` otherwise or when not `Seconds`.

- [ ] **Step 1: Write the failing tests**, one per spec table row, each with its named distractor, all through a helper
  `verdict(subject, lock, force) -> Verdict` using `Thresholds { retention_secs: 604_800, lease_secs: 30 }`:
  - `row_1_a_busy_lock_naming_this_operation_is_live`: status `Live`, not eligible; distractor: busy naming another id gives the table's
    status for the manifest with `eligible == false` and the note `destination lock held by <other>`. A `RootLock` subject with `Busy { .. }`
    is `Live` too (the lock's own row, section 251.1: "LIVE when the owner is alive").
  - `row_2_a_bad_manifest_is_corrupt`: each `ManifestProblem` except `UnknownFormat` gives `Corrupt`, never eligible, with `force` too.
  - `row_3_an_unknown_format_is_uncertain`: `UnknownFormat(4)` gives `Uncertain` with note `format 4`.
  - `row_4_completed_is_unclean_and_eligible_when_the_lock_is_free`: `Completed` + `Free`/`Absent` eligible; + `Busy{names: Some(other)}`
    ineligible; age is irrelevant (`Age::Unreadable` still eligible).
  - `row_5_abandoned_is_stale`: `Abandoned` gives `Stale`, eligible when free, with `Age::Unreadable` too.
  - `row_6_a_future_or_unreadable_age_is_uncertain`: `Transferring` with `manifest_age: Future` or `Unreadable`, or a free record for this id
    with `lease_age: Future`, gives `Uncertain` with note `LEASE_AGE_UNCERTAIN`, never eligible, `force` too.
  - `row_7_a_young_lease_is_resumable_never_eligible`: `Failed`, `manifest_age: Seconds(604_800)`, record for this id `lease_age: Seconds(29)`
    gives `Resumable`, ineligible with `force`. (This is Review Focus 2, `a_young_lease_keeps_an_old_workspace_resumable`: same assertions,
    keep both names or one test with both asserts.) Distractor: `Seconds(30)` with the same ages gives `Stale`.
  - `row_8_old_and_past_the_lease_is_stale`: `Created`, `manifest_age: Seconds(604_800)`, no record: `Stale`, eligible when free; distractor:
    `Seconds(604_799)` gives `Resumable`.
  - `row_9_resumable_is_eligible_only_with_force`: `Transferring`, `manifest_age: Seconds(3600)`: `Resumable`, `eligible == force`, and with
    `Busy{..}` ineligible even with force.
  - `row_10_debris_is_stale`: `Subject::Debris` gives `Stale`, note `debris`, eligible when free.
  - `row_11_an_orphan_lock_past_the_lease_is_stale`: `RootLock { moved_aside: false }` + `Free { record: Some { workspace_missing: true,
    lease_age: Seconds(30) } }` gives `Stale`, eligible, note `orphan lock`; `RootLock { moved_aside: true }` with `workspace_missing: false`
    and the same lease gives `Stale`, eligible, note `moved-aside lock` (Decision 8).
  - `row_12_a_young_or_future_orphan_lock_is_uncertain`: `Seconds(29)` gives `Uncertain` with note `lease younger than 30 s`; `Future`
    gives `Uncertain` with note `LEASE_AGE_UNCERTAIN`; neither eligible.
  - `row_13_an_unreadable_lock_is_uncertain`: `RootLock` + `Unreadable` gives `Uncertain`, note contains `--break-lock`; an `Operation`
    with `lock: Unreadable` is classified by its manifest but ineligible (the lock cannot be acquired).
  - `row_order_first_match_wins`: a `Completed` manifest with a busy lock naming this id is `Live` (row 1 beats row 4).
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core cleanup::status`
Expected: compile errors naming `classify`.

- [ ] **Step 3: Implement** `classify` as the spec's table, rows in order, first match wins; the eligibility and note rules above.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/cleanup
git commit -m "feat: the cleanup status table (cut 9b)"
```

---

### Task 3: Discovery - the lock probe, the listing and the facts (`flux-core/src/cleanup/discover.rs`, `run/place.rs::locate_dest`)

**Files:**
- Create: `crates/flux-core/src/cleanup/discover.rs`
- Modify: `crates/flux-core/src/run/place.rs:128-188` (`locate_tree` split: `locate_dest` does lines 138-139 and 150-168 (holder, name, dest);
  `locate_tree` calls it, then the pre-flight and containment as today; `LocatedTree` and `locate_dest` become `pub(crate)`),
  `crates/flux-core/src/cleanup/mod.rs` (`pub mod discover;`, `Entry`, `Refused`)
- Test: `crates/flux-core/src/cleanup/discover.rs` (`FaultFs`), existing `run/tests.rs` (the oracle for the `locate_tree` split: every test there
  still passes)

**Interfaces:**
- Consumes: `lock::check_capability` (`lock/obtain.rs:36`), `LockSite::{directory, root}` (`lock/site.rs:47,57`), `LockSite::broken_name`
  (`:108`), `prior::check_control_plane` (`prior.rs:~200`), `state::{read_state, MANIFEST, OPERATIONS_DIR, FLUX_DIR, CREATING_SUFFIX,
  REMOVING_SUFFIX, Unusable, Kind}`, `lock::record::{decode, Decoded, RECORD_LEN}`, `LockFile::{read_all, try_lock}` (`flux-fs/src/lock.rs:38,44`),
  `cleanup::status::*` (Task 2), `ids::is_id`.
- Produces:
  ```rust
  // cleanup/mod.rs
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Entry {
      pub kind: EntryKind,
      /// The operation id; `<id>.creating` / `<id>.removing`; or the lock file's name (`<name>.flux-lock`, `.flux-root.lock`, `<lock>.broken.<id>`).
      pub id: String,
      pub status: Status,
      pub eligible: bool,
      pub state: Option<OpState>,
      pub age_seconds: Option<u64>,
      pub note: String,
  }
  /// Exit 3: nothing was examined.
  #[derive(Debug)]
  pub struct Refused { pub code: &'static str, pub detail: String }
  // cleanup/discover.rs
  pub(crate) struct Discovered<D: DirHandle> {
      pub located: LocatedTree<D>,          // holder, holder_shown, name, dest (run/place.rs:118)
      pub capability: LockCapability,
      pub entries: Vec<(Entry, Facts)>,     // Decision 11's order
      /// A directory that had to be listed and could not be: exit 1 (spec "Exit codes").
      pub listing_failed: Option<(PathBuf, FsError)>,
  }
  pub(crate) fn discover<F: DestinationRoot>(fs: &F, dst_root: &Path, cfg: &CleanupConfig) -> Result<Discovered<F::Dir>, Refused>;
  /// One probe: `open_lock`, `read_all(RECORD_LEN)`, `decode`, `try_lock`, drop. Never writes.
  pub(crate) fn probe_lock<D: DirHandle>(dir: &D, name: &OsStr, site: &LockSite<'_, D>, now: u64) -> flux_fs::Result<LockProbe>;
  /// `Age::Future` when `then_ns > now_ns`, else whole seconds.
  pub(crate) fn age_ns(now_ns: u64, then_ns: u64) -> Age;
  pub(crate) fn age_of(now_ns: u64, modified: Option<SystemTime>) -> Age;   // `None` is `Unreadable`
  ```
  `discover`: `locate_dest` (a DEST that exists and is not a directory, a symlink at DEST, or a parent that cannot be opened is
  `Refused { code: "DESTINATION_ERROR" / "SAFETY_REJECTED" as `locate_tree` reports them, detail: the error }`); `check_capability(&holder)`
  (`Refused { code: "REMOTE_LOCK_UNSAFE" }`); `check_control_plane(dest)` when DEST exists (`Refused { code: "CONTROL_PLANE_NAMESPACE_CONFLICT" }`);
  then the root-lock probe (`workspace_missing` = `site.workspace(&record)? == Workspace::Missing`; `LockSite::workspace` and `Workspace` are
  `pub(crate)` already). The root lock is its OWN entry only when its record names a missing workspace (any probe result: busy is `Live`,
  free is rows 11-12), or when it is `Unreadable`, or busy with no readable record; a lock whose record names an existing operation is
  not a row: it feeds that operation's facts (section 251.1 "classified with the operation its record names"). the `operations/` listing (absent `.flux` or `operations/` is an
  empty listing, not an error; a `read_dir` failure sets `listing_failed`), each `<id>` directory's manifest (`read_state`; `NotFound` is
  `ManifestProblem::Missing`; `SafetyRejected`/`DestinationError` is `NotRegular`; `Unusable::Corrupt(w)` is `Corrupt(w)`;
  `Unusable::Incompatible(v)` is `UnknownFormat(v)`; an id or kind mismatch is `WrongOperation`) and the manifest file's `metadata(..).modified`
  for `manifest_age`, each `<id>.creating` / `<id>.removing` directory as `Debris`, then every `<lock_name>.broken.<id>` in the holder's
  listing (sorted) probed like the lock. Rows are classified with `status::classify` and `Thresholds` from `cfg`.

- [ ] **Step 1: Write the failing tests** in `discover.rs` on a `FaultFs` with `/p/dest/.flux/operations/<id>/manifest` written by
  `OperationState::created(..).encode()` (see `run/tests.rs:2646` `prior3_with` for the shape) and `cfg()` with `now` = `2_000_000_000_000_000_000`
  (2033-05-18; every test pins `set_modified` relative to it):
  - `a_lock_naming_an_existing_operation_is_not_a_row`: a dead lock naming `A` (present): no `root-lock` entry, and `A`'s lease comes from
    the record.
  - `a_free_lock_is_probed_once_and_released`: a dead lock (`lock::test_support::dead_lock`) whose record names id A with a missing
    workspace: `probe_lock` returns `Free { record: Some { operation_id: A, workspace_missing: true, lease_age: Seconds(n) } }`, and afterwards
    a fresh `open_lock(..).try_lock()` succeeds (nothing is held); the call log has exactly one `try_lock(` for the probe.
  - `a_held_lock_is_busy_and_names_its_owner`: `live_lock` held in the test gives `Busy { names: Some(A) }`.
  - `a_torn_or_empty_free_lock_is_unreadable`: `dead_lock(.., b"")` gives `Unreadable`.
  - `an_absent_lock_is_absent`.
  - `the_listing_yields_operations_debris_and_the_lock_in_order`: `/p/dest/.flux/operations/{A, B.creating, C.removing, D (a FILE named by
    an id), notes.txt}` plus a dead orphan root lock (its record names a missing workspace `X`): entries are `[A operation, B.creating debris, C.removing debris, dest.flux-lock root-lock]`; `D` and
    `notes.txt` are not entries.
  - `a_manifest_problem_is_corrupt_and_an_unknown_format_is_uncertain`: `A` with no manifest is `Corrupt`; `B` with `format_version: 9` is
    `Uncertain` with note `format 9`; a manifest naming another id is `Corrupt`.
  - `ages_come_from_the_manifest_mtime_and_the_record_heartbeat`: `A` `TRANSFERRING` with the manifest's mtime 8 days before `now` is
    `Stale`; with a dead root lock naming `A` whose `last_heartbeat_wall_time` is `now - 10 s` it is `Resumable` (the lease wins); with no
    `set_modified` at all it is `Uncertain` (`LEASE_AGE_UNCERTAIN`).
  - `a_dest_without_operations_or_without_dest_still_examines_the_lock`: `/p/dest` absent but `/p/dest.flux-lock` dead and orphaned: exactly
    one `root-lock` entry, `Stale`; `/p/dest` present with no `.flux`: no entries and no `listing_failed`.
  - `refusals_before_any_probe`: `/p/dest` a FILE gives `Refused { code: "DESTINATION_ERROR" }`; `/p/dest/.flux` a file gives
    `CONTROL_PLANE_NAMESPACE_CONFLICT`; `fs.set_lock_capability(<a variant `allows_exclusive` rejects: anything but `LocalStrong` /
    `RemoteStrong`, `flux-fs/src/lock.rs:21`>) gives `REMOTE_LOCK_UNSAFE`; in each case the call log has no `try_lock(`.
  - `a_listing_that_fails_is_reported_not_fatal`: `fs.fail("read_dir", Code::PermissionDenied)` on `operations/` sets `listing_failed` and
    the root lock is still examined.
  - `a_broken_file_is_an_entry_by_its_own_record`: `/p/dest.flux-lock.broken.<X>` dead with a record naming `X`, heartbeat 60 s ago:
    a `root-lock` entry with `id == "dest.flux-lock.broken.<X>"`, `Stale`, eligible; the same with heartbeat 5 s ago: `Uncertain`.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core cleanup::discover`
Expected: compile errors naming `discover`.

- [ ] **Step 3: Implement** `locate_dest` (move the lines, keep `locate_tree`'s behaviour byte for byte), `probe_lock`, `age_ns`, `age_of`,
  `discover`. The manifest's mtime is `workspace.metadata(OsStr::new(MANIFEST))?.modified`. The probe reads BEFORE `try_lock` (spec decision 5).
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed; every `run::tests` test still passes (the `locate_tree` split).

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/cleanup crates/flux-core/src/run/place.rs
git commit -m "feat: cleanup discovery - the transient lock probe, the listing, the facts (cut 9b)"
```

---

### Task 4: `Classified::Orphan`, `obtain_cleanup_lock`, `checked_held`, `finish_retire`, `sweep_partials` (the lock, state and run seams)

**Files:**
- Modify: `crates/flux-core/src/lock/classify.rs:9-23` (the enum), `:44-52` (the `Dead` arm), `:114-123` (the test)
- Modify: `crates/flux-core/src/lock/obtain.rs:53-109` (`obtain`: the `Orphan` arm; the new `obtain_cleanup_lock`), `:255-265` (the test)
- Modify: `crates/flux-core/src/lock/mod.rs:19` (export `obtain_cleanup_lock`, `CleanupObtained`)
- Modify: `crates/flux-core/src/run/session.rs:382-388` (`checked` split)
- Modify: `crates/flux-core/src/state.rs:859-880` (`retire_workspace` split into `finish_retire`)
- Create: `crates/flux-core/src/run/sweep.rs`; Modify: `crates/flux-core/src/run/place.rs:406-470` (`TreePlace::sweep` delegates),
  `crates/flux-core/src/run/mod.rs` (`mod sweep;`)
- Test: `lock/classify.rs`, `lock/obtain.rs`, `state.rs` tests, `run/tests.rs` (existing `--restart` tests are the oracle for `sweep_partials`:
  `restart_supersedes_a_prior_deleting_its_partials_then_its_workspace` (`:388`), `a_partial_restart_cannot_delete_keeps_its_prior_abandoned`
  (`:410`), `a_directory_the_restart_sweep_cannot_read_keeps_every_prior` (`:1005`), `a_partial_inside_a_reserved_control_directory_is_never_swept`
  (`:1024`), `the_adopted_id_names_the_partials_and_the_old_partial_is_swept` (`:2757`))

**Interfaces:**
- Produces:
  ```rust
  // lock/classify.rs
  pub(crate) enum Classified<D: DirHandle> {
      Vanished,
      Busy(Option<LockRecord>),
      Uncertain(Uncertain),
      Foreign,
      /// A dead owner whose recorded workspace is not one Flux trusts: `ARTIFACT_OWNERSHIP_UNCERTAIN`.
      Untrusted(LockRecord),
      /// Cut 9b: a dead owner whose trusted-shaped workspace path names nothing on disk. The handle and its OS-native lock are KEPT
      /// so that cleanup can recover it (section 240.3); `obtain` refuses it as `Untrusted`.
      Orphan { lock: D::Lock, identity: FileIdentity, record: LockRecord },
      Dead { lock: D::Lock, identity: FileIdentity, record: LockRecord },
  }
  // lock/obtain.rs
  pub struct CleanupObtained<'a, D: DirHandle> {
      pub held: Held<'a, D>,                       // no record yet: the caller writes one with workspace_path "none"
      pub leftover_broken: Option<OsString>,
      /// The orphan record the acquisition reclaimed (reported as the `root-lock` row's action).
      pub reclaimed: Option<LockRecord>,
  }
  /// Cut 9b: `obtain` for cleanup. Differs in exactly one arm: an `Orphan` whose heartbeat is not in the future and is at least
  /// `lease_threshold_ns` old is recovered (section 240.3); younger, or future, it is refused `ARTIFACT_OWNERSHIP_UNCERTAIN` with the detail
  /// "the dead owner's lock is younger than the lease threshold". `Uncertain` is always refused (`TARGET_LOCK_UNCERTAIN`; no --break-lock).
  pub fn obtain_cleanup_lock<'a, D: DirHandle>(site: &LockSite<'a, D>, capability: LockCapability, operation_id: &str, now_ns: u64, lease_threshold_ns: u64) -> LockResult<CleanupObtained<'a, D>>;
  // run/session.rs
  pub(crate) fn checked_held<D: DirHandle>(held: &Held<'_, D>, lock_shown: &Path, pulse: &Pulse) -> Result<(), Fault>;
  pub(crate) fn checked<D: DirHandle>(locked: &Locked<'_, D>) -> Result<(), Fault> { checked_held(&locked.held, &locked.lock_shown, &locked.pulse) }
  // state.rs
  /// The second half of `retire_workspace`: remove the known names inside `operations/<name>`, then the directory.
  pub fn finish_retire<D: DirHandle>(operations: &D, name: &OsStr) -> flux_fs::Result<()>;
  pub fn retire_workspace<D: DirHandle>(operations: &D, id: &str) -> flux_fs::Result<()>; // rename, then finish_retire
  // run/sweep.rs
  /// J1: delete every `<name>.flux-partial.<id>` for `ids` under `dest`, found by a walk of `dest_shown` that skips the reserved control
  /// directories, through handles one component at a time. `check` runs before every deletion. Returns the ids whose partials are all gone;
  /// a partial that stays is reported through `kept` with the path, the error and `shown(id)`.
  pub(crate) fn sweep_partials<F: DestinationRoot>(
      fs: &F, dest: &F::Dir, dest_shown: &Path, operations_shown: &Path, ids: &BTreeSet<&str>,
      shown: &dyn Fn(&str) -> PathBuf, check: &mut dyn FnMut() -> Result<(), Fault>, warnings: &mut Vec<RunWarning>,
  ) -> Result<Vec<String>, Fault>;
  ```
  `obtain`'s `Orphan` arm: the same refusal as `Untrusted` with `"; flux cleanup DEST removes a dead owner's lock whose workspace is gone"`
  appended to the detail (the existing substring stays intact: `obtain.rs:263` and `flux-cli/tests/run.rs:381-405` pin the code and the
  holder lines, not the sentence).

- [ ] **Step 1: Write the failing tests**
  - `classify.rs`: rename `a_dead_owner_whose_workspace_is_missing_is_untrusted_and_released` (`:114`) to
    `a_dead_owner_whose_workspace_is_missing_is_an_orphan_and_the_lock_is_kept`: `Classified::Orphan { record, .. }` with `record == rec`, and
    while the variant lives another `try_lock` fails (as `the_dead_classification_keeps_the_os_lock_while_it_lives` (`:99`) checks for `Dead`).
    A record whose `workspace_path` is `"garbage"` is still `Untrusted`.
  - `obtain.rs`: `a_dead_owner_with_a_missing_workspace_is_artifact_ownership_uncertain` (`:255`) additionally asserts the detail contains
    `flux cleanup`. New: `cleanup_reclaims_an_orphan_lock_past_the_lease_gate`: dead orphan record with `last_heartbeat_wall_time: 1`,
    `obtain_cleanup_lock(&site, STRONG, &me(), now = 31_000_000_000, 30_000_000_000)` returns `CleanupObtained { reclaimed: Some(rec), .. }`,
    the held lock's identity differs from the old one, and no `.broken.*` remains. `cleanup_refuses_a_young_or_future_orphan_lock`:
    heartbeat `now - 29 s` and heartbeat `now + 1` each give `ArtifactOwnershipUncertain`, and the file is byte-identical afterwards.
    `cleanup_refuses_an_uncertain_lock_and_is_busy_on_a_live_one`: `dead_lock(.., b"")` gives `TargetLockUncertain`; `live_lock` gives
    `TargetLockBusy`. `cleanup_acquires_a_free_path_like_a_run`: no lock file: `Ok` with `reclaimed: None`.
  - `state.rs`: `finish_retire_removes_only_the_known_names_and_keeps_a_directory_with_a_stranger`: `operations/<id>.removing/` with
    `manifest`, `manifest.tmp`, `state.db` and `extra`: `finish_retire` fails with `DirectoryNotEmpty`, the three known names are gone,
    `extra` stays; without `extra` it succeeds and the directory is gone.
  - `run/tests.rs`: no new test; the five named tests are the oracle.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core lock:: state::`
Expected: compile errors naming `Orphan`, `obtain_cleanup_lock`, `finish_retire`.

- [ ] **Step 3: Implement.** `classify`: `Workspace::Missing => Classified::Orphan { lock, identity: lock.identity()?, record }`. `obtain`: the
  `Orphan { record, .. }` arm drops the lock and refuses as above. `obtain_cleanup_lock`: the loop of `obtain` with `Mode::Plain`, except
  `Orphan`: `age_ns(now, record.last_heartbeat_wall_time)` (inline: future or `< lease_threshold_ns` refuses; else `recover(site, lock,
  identity, &record, operation_id)` as the `Dead` arm does, remembering `reclaimed = Some(record)`); `Dead` recovers as `obtain` does;
  `Uncertain` refuses with `TargetLockUncertain`. `checked_held` / `checked`. `finish_retire` / `retire_workspace`. `sweep_partials`: move the
  body of `TreePlace::sweep` (`place.rs:412-470`) with `checked(locked)?` replaced by `check()?` and `self.shown(prior)` by `shown(prior)`;
  `TreePlace::sweep` builds `ids`, `shown = |id| self.shown(id)`, `check = || checked(locked)` and calls it. `reserved_path` stays in
  `tree.rs:553` (`pub(crate)`).
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed; the five oracle tests pass unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/lock crates/flux-core/src/run crates/flux-core/src/state.rs
git commit -m "feat: Classified::Orphan, obtain_cleanup_lock, checked_held, finish_retire, sweep_partials (cut 9b seams)"
```

---

### Task 5: The deletion pass and `cleanup::cleanup` (`flux-core/src/cleanup/delete.rs`, `cleanup/mod.rs`)

**Files:**
- Create: `crates/flux-core/src/cleanup/delete.rs`
- Modify: `crates/flux-core/src/cleanup/mod.rs` (`pub mod delete;`, `Action`, `CleanupReport`, `pub fn cleanup`)
- Test: `crates/flux-core/src/cleanup/delete.rs` (`FaultFs`; the fixtures of Task 3's tests, moved to a `cleanup/test_support.rs` if both files
  need them)

**Interfaces:**
- Consumes: Tasks 1-4; `state::{write_state, retire_workspace, finish_retire, remove_empty_control_dirs, operations_dir}` (`state.rs:772, 862,
  890, 853`), `lock::record::LockRecord`, `Held::{write_record, release}` (`lock/held.rs:45,115`), `run::session::{Pulse, checked_held, owned, Fault}`,
  `run::sweep::sweep_partials`, `RunWarning::PartialKept`.
- Produces:
  ```rust
  // cleanup/mod.rs
  #[derive(Debug)]
  pub enum Action {
      Removed(PathBuf),
      Kept { path: PathBuf, error: FsError },
      Skipped { id: String, reason: String },
  }
  #[derive(Debug, Default)]
  pub struct CleanupReport {
      pub entries: Vec<Entry>,
      pub actions: Vec<Action>,
      pub listing_failed: Option<(PathBuf, FsError)>,
      /// The pass lost the lock (`Fault`) at this path: exit 1.
      pub lost: Option<(PathBuf, FsError)>,
  }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
  pub struct Summary { pub entries: u64, pub eligible: u64, pub removed: u64, pub kept: u64, pub skipped: u64 }
  impl CleanupReport { pub fn summary(&self) -> Summary; /// Exit 1 (Decision 10). pub fn failed(&self) -> bool }
  /// `flux cleanup DEST`: discover, classify, and unless `cfg.dry_run` run one deletion pass. `Err` is exit 3.
  pub fn cleanup<F: DestinationRoot>(fs: &F, dst_root: &Path, cfg: &CleanupConfig) -> Result<CleanupReport, Refused>;
  // cleanup/delete.rs
  pub(crate) struct CleanupLock<'a, D: DirHandle> { pub held: Held<'a, D>, pub lock_shown: PathBuf, pub pulse: Pulse }
  /// The deletion pass over `discovered` (Task 3), under a lock this function acquires and releases (spec "Locking").
  pub(crate) fn run_pass<F: DestinationRoot>(fs: &F, discovered: &Discovered<F::Dir>, cfg: &CleanupConfig, report: &mut CleanupReport);
  /// One COMPLETED operation's recorded leftovers, then its workspace (spec "Deletion procedure"). `check` runs before every removal.
  /// Returns the number of leftovers removed when the workspace was retired, `None` when something was kept.
  pub(crate) fn finish_completed<D: DirHandle>(dest: &D, dest_shown: &Path, operations: &D, operations_shown: &Path, state: &OperationState, check: &mut dyn FnMut() -> Result<(), Fault>, actions: &mut Vec<Action>) -> Result<Option<u64>, Fault>;
  /// `<id>.creating` / `<id>.removing`: `finish_retire`, reported as one action.
  pub(crate) fn remove_debris<D: DirHandle>(operations: &D, operations_shown: &Path, name: &OsStr, check: &mut dyn FnMut() -> Result<(), Fault>, actions: &mut Vec<Action>) -> Result<(), Fault>;
  /// A STALE (or forced RESUMABLE) operation: ABANDONED, the J1 sweep for its id, then the workspace only when every partial is gone.
  pub(crate) fn abandon_and_sweep<F: DestinationRoot>(fs: &F, dest: &F::Dir, dest_shown: &Path, operations: &F::Dir, operations_shown: &Path, state: &OperationState, check: &mut dyn FnMut() -> Result<(), Fault>, actions: &mut Vec<Action>) -> Result<(), Fault>;
  ```
  `run_pass`: if no entry is eligible, return without touching the lock. Else `obtain_cleanup_lock`; `TargetLockBusy` -> every eligible row
  `Skipped { reason: "destination lock busy" }`; any other refusal -> every eligible row skipped with the refusal's detail; an orphan
  reclaimed -> `Removed(<lock path>)` for the `root-lock` row (and `Kept` for a `leftover_broken`). Write the record (`LockRecord` with
  `workspace_path: "none"`, the cfg ids, `now`). `check = || { if let Some(h) = &cfg.before_mutation { h() }; checked_held(..) }`. For each
  eligible row in Decision 11's order: re-read (manifest via `read_state`, mtime, and for `.broken.*` the record) and re-classify with
  `lock: LockProbe::Free { record: None }` (the lock is ours now; a row that was eligible only because of a record's lease uses the re-read
  record facts); not eligible any more -> `Skipped { reason: "changed since listing" }`; else the row's procedure. `Fault::Lost` /
  `Heartbeat` / `Io` -> `report.lost = Some(..)`, the remaining eligible rows `Skipped { reason: "lock lost" }`, stop. At the end
  `remove_empty_control_dirs(dest)` (a failure is `Kept`), then `held.release()` (a failure is `Kept` at the lock path).
  `finish_completed`: for each artifact `validate(hex, id)` -> `Err` is `Kept { path: dest_shown.join(<decoded or the hex>), error:
  FsError::new(Code::SafetyRejected, "not a recognized leftover") }` and the function returns `Ok(None)` without touching the workspace;
  `Ok(rel)` -> `check()?` then `remove_validated(dest, &rel)`: `Deleted` and `AlreadyGone` are both `Removed(dest_shown.join(rel))` and count;
  an error is `Kept`, and the function stops at the first kept artifact and returns `Ok(None)` (the workspace must stay as the record; the
  remaining artifacts are retried by the next cleanup). Then `check()?`, `retire_workspace(operations, id)` ->
  `Removed(operations_shown.join(id))` or `Kept`. A format-1 record (no `cleanup` field) has no artifacts (spec row 4).
  `abandon_and_sweep`: `check()?`, `write_state(workspace, MANIFEST, &OperationState { state: Abandoned, ..state.clone() })` (a failure is
  `Kept` at the manifest path and returns); `sweep_partials` with `ids = {id}`, translating each `RunWarning::PartialKept { path, error, .. }` into
  `Kept { path, error }` and each deleted partial into `Removed` (`sweep_partials` does not report successes today: add an `on_removed:
  &mut dyn FnMut(PathBuf)` parameter to it in this task, called with the deleted path; `TreePlace::sweep` passes a no-op); if the id is in the
  returned "gone" list -> `check()?`, `retire_workspace` -> `Removed` / `Kept`; else nothing more (the ABANDONED workspace is the record).

- [ ] **Step 1: Write the failing tests** in `delete.rs` (fixtures as Task 3; a COMPLETED prior with leftovers is written by hand:
  `OperationState::created(..)` with `state: Completed`, `cleanup: Some(Cleanup { cleanup_pending: true, cleanup_pending_artifacts: vec![native_hex(..)] })`,
  and the leftover file created under `/p/dest`):
  - `a_completed_operation_loses_its_leftovers_then_its_workspace`: two leftovers (`a.flux-partial.<A>` and `sub/b.flux-partial.<A>`): actions are
    `Removed` x2 then `Removed(/p/dest/.flux/operations/A)`; `/p/dest/.flux` is gone (empty control dirs removed); the call log has
    `remove_file(` for both leftovers BEFORE `rename_no_replace(/p/dest/.flux/operations/A -> .../A.removing)`; the lock file is gone afterwards.
  - `a_leftover_that_cannot_be_removed_keeps_the_workspace`: `fs.fail_always("remove_file", Code::PermissionDenied)` on the first leftover:
    actions `Kept` and no `Removed` for the workspace; the manifest is still COMPLETED; `report.failed()`.
  - `a_refused_artifact_keeps_everything`: an artifact `report.doc`: one `Kept` whose error source contains `not a recognized leftover`; the
    workspace and the file stay.
  - `an_already_deleted_leftover_counts_as_removed`: the artifact's file is absent: `Removed`, the workspace is retired.
  - `a_format_1_completed_workspace_is_retired_with_no_artifacts`: `created_v1(..)` with `state: Completed`: `Removed(workspace)` only.
  - `debris_is_finished_and_a_stranger_inside_keeps_it`: `B.creating` with `manifest.tmp` and `C.removing` with `state.db`: both `Removed`;
    `D.removing` with `extra`: `Kept` with `DirectoryNotEmpty`, `extra` stays.
  - `a_stale_operation_is_abandoned_swept_and_retired_last`: `A` `FAILED`, manifest mtime 8 days ago, partials `x.flux-partial.<A>` and
    `sub/y.flux-partial.<A>` plus `z.flux-partial.<OTHER>`: the manifest is written ABANDONED before any `remove_file`; the two partials are
    `Removed`, `z` stays, the workspace is `Removed` last; `superseded_by` is `None`.
  - `a_partial_that_stays_keeps_the_abandoned_workspace`: `fail_always("remove_file", ..)`: `Kept`, the manifest reads ABANDONED, the
    workspace exists, `report.failed()`.
  - `a_resumable_operation_is_deleted_only_with_force`: `A` `TRANSFERRING`, mtime 1 h ago, lease by mtime: without `force` no lock is taken
    (no `create_lock(` in the log) and no action; with `force` the row is `Removed`.
  - `revalidation_skips_a_row_that_changed`: `fs.on_nth("create_lock", 1, |fs| rewrite A's manifest to TRANSFERRING with mtime = now)`:
    the action is `Skipped { reason: "changed since listing" }`, nothing removed.
  - `a_busy_lock_skips_every_eligible_row_with_exit_0`: a live lock held by the test (`live_lock`) naming another id: the completed row is
    `COMPLETED_BUT_UNCLEAN`, ineligible, note `destination lock held by <other>`, no actions, `!report.failed()`;
    then with the lock becoming busy only AFTER discovery (`fs.on_nth("create_lock", 1, ..)` making the path exist and held): `Skipped {
    reason: "destination lock busy" }` and `!report.failed()`.
  - `a_lost_lock_stops_the_pass`: two eligible rows; `fs.on_nth("remove_file", 1, |fs| fs.repoint_for_test(LOCK, ..)` or the fake's way to make
    `still_owned` false (see `run/tests.rs:472` `restart_stops_when_ownership_is_lost_and_deletes_nothing_after` for the idiom): the second row
    is `Skipped { reason: "lock lost" }`, `report.lost.is_some()`, `report.failed()`.
  - `an_orphan_root_lock_is_reclaimed_by_the_acquisition`: dead lock naming `X` with a missing workspace, heartbeat 60 s ago: the `root-lock`
    row is `Stale` eligible; the actions hold `Removed(/p/dest.flux-lock)`; the old file's identity is gone, no `.broken.*` remains, and
    after `cleanup` returns no lock file exists (the pass's own lock was released).
  - `a_failure_mid_retire_leaves_removing_that_the_next_pass_finishes`: `fs.fail_nth("remove_dir", 1, PermissionDenied, ..)` during a
    completed row's retirement: `Kept`, `operations/A.removing` exists, `report.failed()`; a second `cleanup` lists it as debris `Stale` and
    removes it.
  - `a_broken_file_is_removed_under_the_lock`: `/p/dest.flux-lock.broken.<X>` (dead, 60 s): `Removed`; with heartbeat 5 s ago: `Uncertain`, stays.
  - `dry_run_takes_no_lock_and_removes_nothing`: eligible rows present, `dry_run: true`: no `create_lock(`, no `remove_file(`, no
    `rename_no_replace(` in the call log; entries are classified; `summary().eligible` counts them.
  - `the_summary_counts_actions_not_rows`: one completed row with two leftovers: `entries 1, eligible 1, removed 3, kept 0, skipped 0`.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core cleanup::delete`
Expected: compile errors naming `run_pass`.

- [ ] **Step 3: Implement** `delete.rs` and `cleanup::cleanup` (discover, then `run_pass` unless `dry_run`; `listing_failed` copied into the
  report).
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/cleanup crates/flux-core/src/run
git commit -m "feat: the cleanup deletion pass - revalidate under the lock, leftovers first, workspace last (cut 9b)"
```

---

### Task 6: The copy finishes a prior's cleanup (`prior.rs`, `run/place.rs`, `run/session.rs`, `run/mod.rs`)

**Files:**
- Modify: `crates/flux-core/src/prior.rs:27-31` (`Scan` gains `completed`, `debris`), `:45-92` (`scan_tree` collects them), `:95-150`
  (`scan_file` collects COMPLETED records)
- Modify: `crates/flux-core/src/run/place.rs:28-77` (two `Place` methods), the `TreePlace` and `FilePlace` impls
- Modify: `crates/flux-core/src/run/session.rs:124-140` (keep `scan.completed` / `scan.debris`), `:255-270` (the call after `supersede`)
- Modify: `crates/flux-core/src/run/mod.rs:153-171` (`RunWarning` variants)
- Modify: `crates/flux-cli/src/report.rs:327-363` (`run_warning_line` arms)
- Test: `crates/flux-core/src/run/tests.rs`, `crates/flux-cli/src/report.rs`

**Interfaces:**
- Consumes: Task 5's `finish_completed`, `remove_debris`; `state::{record_name, remove_record}`, `copy::temp_path`.
- Produces:
  ```rust
  // prior.rs
  pub struct Scan {
      pub resumable: Vec<PriorOp>,
      /// Cut 9b: COMPLETED priors (`cleanup_pending`), to finish. ABANDONED ones are still passed over.
      pub completed: Vec<PriorOp>,
      /// Cut 9b: `<id>.creating` and `<id>.removing` directories (trees only), by name.
      pub debris: Vec<OsString>,
  }
  // run/place.rs, in `Place`
  /// Cut 9b: finish `prior`'s recorded cleanup under this run's lock; `Ok(Some(n))` removed n leftovers and the state, `Ok(None)` kept
  /// something (already reported as a warning).
  fn finish_completed(&self, prior: &PriorOp, locked: &Locked<'_, D>, warnings: &mut Vec<RunWarning>) -> Result<Option<u64>, Fault>;
  /// Cut 9b: remove one debris directory; a tree only (a single file has none).
  fn remove_debris(&self, name: &OsStr, locked: &Locked<'_, D>, warnings: &mut Vec<RunWarning>) -> Result<(), Fault>;
  // run/mod.rs
  pub enum RunWarning { /* existing variants unchanged */
      /// Cut 9b: `note: finished cleaning up the earlier operation <id> (<n> leftovers removed)`.
      PriorCleaned { operation_id: String, removed: u64 },
      /// Cut 9b: `warning: could not finish the cleanup of the earlier operation <id>: <path>: <error>`.
      PriorCleanupFailed { operation_id: String, path: PathBuf, error: FsError },
  }
  ```
  `TreePlace::finish_completed` calls `delete::finish_completed` with `check = || checked(locked)` and translates `Action::Kept` into one
  `PriorCleanupFailed` (the first kept path) and a retired workspace into `PriorCleaned`. `FilePlace::finish_completed`: the artifact must
  equal `temp_path(Path::new(&self.target), &OperationId::new(id))`'s file name (spec "Artifact rules", single file), removed with
  `self.dir.remove_file` (`NotFound` counts as removed), then `remove_record`; anything else is `PriorCleanupFailed`. `FilePlace::remove_debris`
  is `Ok(())`. In `open_operation`: after `supersede` succeeds and before the TRANSFERRING write, for each `completed` prior and each debris
  name, call the method; a `Fault` is handled exactly as `supersede`'s (`session.rs:255-266`), with `RunStep::Restart` replaced by a new
  `RunStep::PriorCleanup` whose `as_str` is `"finishing an earlier operation's cleanup"`. Under `--resume` the adopted prior is in `resumable`,
  never in `completed`.

- [ ] **Step 1: Write the failing tests**
  - `run/tests.rs`: `a_copy_finishes_a_completed_priors_leftovers_and_notes_it`: a COMPLETED prior `A` (hand-written as in Task 5) with a
    leftover `a.flux-partial.<A>` under `/p/dest`: after `run_tree(&fs, &cfg())` the leftover and `operations/A` are gone, the run `ok`, and
    `warnings` holds `PriorCleaned { operation_id: A, removed: 1 }`; the call log shows the leftover's `remove_file(` AFTER this run's
    `write_at_start(` (the record exists first) and BEFORE the TRANSFERRING manifest write.
  - `a_prior_leftover_that_cannot_be_removed_warns_and_the_copy_succeeds` (Review Focus 5): `fail_always("remove_file", PermissionDenied)` on the
    leftover's path only (use `on_nth` + path check, or `fail_nth` with the counted index named in a comment): the copy is `ok`, `stop` is `None`,
    `warnings` holds `PriorCleanupFailed { operation_id: A, .. }`, `operations/A` still exists with a COMPLETED manifest.
  - `a_copy_removes_creating_and_removing_debris`: `B.creating` (with `manifest.tmp`) and `C.removing` under `operations/`: both gone after a
    plain run; no warning.
  - `a_copy_leaves_abandoned_and_resumable_priors_to_cleanup`: an ABANDONED prior stays; a FAILED prior still refuses with
    `RESUMABLE_OPERATION_EXISTS` (existing behaviour, assert it still holds with a COMPLETED prior beside it, which is NOT finished because the
    run refused before step 5).
  - `a_single_file_copy_finishes_its_targets_completed_record`: `/p/t.flux-state.<A>` COMPLETED with artifact `t.flux-partial.<A>` and that file
    present: after `run_file`, both are gone, `PriorCleaned { removed: 1 }`; an artifact naming `u.flux-partial.<A>` is `PriorCleanupFailed` and
    the record stays.
  - `prior_cleanup_runs_after_a_restart_supersede`: `restart()` with a FAILED prior and a COMPLETED prior: both end gone; the ABANDONED write
    of the FAILED one precedes the COMPLETED one's removal in the call log.
  - `prior_cleanup_checks_ownership_before_each_removal`: as `restart_heartbeats_and_checks_ownership_before_removing_a_superseded_state`
    (`run/tests.rs:1584`) does: the `metadata(/p/dest.flux-lock)` check precedes the leftover's `remove_file(`.
  - `report.rs`: `prior_cleanup_lines_are_the_specs`: `PriorCleaned { A, 2 }` renders `note: finished cleaning up the earlier operation <A> (2
    leftovers removed)`; `PriorCleanupFailed` renders `warning: could not finish the cleanup of the earlier operation <A>: <path>: <error>`.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core run::tests::a_copy_finishes -p flux-cli`
Expected: compile errors naming `completed`, `PriorCleaned`.

- [ ] **Step 3: Implement** the scan fields, the `Place` methods, the `open_operation` call, the variants, the report lines.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed; `the_json_has_every_section_53_field_in_order` unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src crates/flux-cli/src/report.rs
git commit -m "feat: a copy finishes a prior operation's recorded cleanup and removes workspace debris (cut 9b)"
```

---

### Task 7: `flux cleanup DEST` - the command, the report, the exit codes (`flux-cli`)

**Files:**
- Create: `crates/flux-cli/src/cleanup_report.rs`
- Modify: `crates/flux-cli/src/lib.rs:5-7` (`pub mod cleanup_report;`), `crates/flux-cli/src/main.rs:24-35` (`Commands::Cleanup(CleanupArgs)`),
  `:96-99` (`main` dispatches), new `fn cleanup(args: &CleanupArgs) -> u8` and `fn cleanup_config(args) -> CleanupConfig`,
  `crates/flux-cli/src/exit_code.rs:8-47` (`for_cleanup`)
- Test: `crates/flux-cli/src/cleanup_report.rs`, `crates/flux-cli/src/exit_code.rs`, `crates/flux-cli/src/main.rs` (the clap tests at `:157-260`)

**Interfaces:**
- Consumes: `flux_core::cleanup::{cleanup, CleanupConfig, CleanupReport, Entry, Action, Summary, Refused, DEFAULT_RETENTION, LEASE_THRESHOLD}`,
  `flux_core::state::wall_time_ns`, `resolve::canonical_with_remainder` (`resolve.rs:83`), `debug_hook()` and `heartbeat_interval()` (`main.rs`).
- Produces:
  ```rust
  // main.rs
  enum Commands { Copy(CopyArgs), /// Classify and remove what earlier operations left at DEST (section 24.4, section 251).
                  Cleanup(CleanupArgs) }
  #[derive(Args)]
  struct CleanupArgs {
      destination: PathBuf,
      /// Classify and report eligibility; delete nothing and take no lock.
      #[arg(long)] dry_run: bool,
      /// Also delete RESUMABLE operations (bypasses retention only; never a live or uncertain owner).
      #[arg(long)] force: bool,
      /// Print the report as one JSON object on stdout.
      #[arg(long)] json: bool,
  }
  // cleanup_report.rs
  pub fn age_text(seconds: Option<u64>) -> String;              // "-", "45s", "3m", "7h", "2d" (largest non-zero unit; 60/60/24)
  pub fn header_line() -> String;                                // "STATUS                 ELIGIBLE  KIND       ID                  STATE         AGE   NOTE"
  pub fn entry_line(e: &Entry) -> String;                        // columns padded to the header's widths; ID is never truncated
  pub fn action_line(a: &Action) -> String;                      // "removed <path>" | "kept <path>: <error>" | "skipped <id>: <reason>"
  pub fn summary_line(r: &CleanupReport, dry_run: bool) -> String;
  pub fn refused_line(r: &Refused) -> String;                    // "<CODE>: <detail>"
  pub fn json(destination: &Path, dry_run: bool, r: &CleanupReport) -> serde_json::Value;  // the spec's object; "summary" has the five keys
  // exit_code.rs
  pub fn for_cleanup(r: &CleanupReport) -> u8;                   // FAILED when r.failed(), else SUCCESS
  ```
  `main.rs::cleanup`: `canonical_with_remainder(&args.destination)` (a failure is `DESTINATION_ERROR: <path>: <error>`, exit 3); build the
  config (`now: wall_time_ns()`, fresh ids, `boot_session_id: flux_platform::boot_session_id()`, `before_mutation: debug_hook()`,
  `heartbeat_interval: heartbeat_interval()`, `retention: DEFAULT_RETENTION`, `lease_threshold: LEASE_THRESHOLD`); run
  `flux_core::cleanup::cleanup(&flux_platform::StdFileSystem, &dst, &cfg)` (as `copy` does at `main.rs:213`); on `Err(refused)` print `refused_line` to
  stderr and return `exit_code::REFUSED`; else print the text or the JSON on stdout, the `listing_failed` / `lost` lines
  (`error: could not list <path>: <error>` / `error: the destination's lock was lost at <path>: <error>`) on stderr, and return `for_cleanup`.

- [ ] **Step 1: Write the failing tests**
  - `cleanup_report.rs`: `age_text_picks_the_largest_unit` (`None` -> `-`, `0` -> `0s`, `59` -> `59s`, `60` -> `1m`, `3_599` -> `59m`, `3_600` -> `1h`,
    `86_400` -> `1d`, `604_800` -> `7d`); `an_entry_line_has_the_specs_columns` (a `COMPLETED_BUT_UNCLEAN` row with a 32-hex id renders with the
    header's column starts, checked by index); `action_lines_are_the_specs_three_shapes`; `the_summary_line_counts_and_dry_run_differs`
    (`cleanup: 2 entries, 3 removed, 1 kept, 0 skipped` and `cleanup (dry run): 2 entries, 1 eligible`); `the_json_has_the_specs_keys`:
    top-level keys exactly `destination, dry_run, entries, actions, summary` in order; an entry's keys `status, eligible, kind, id, state,
    age_seconds, note`; an action's `action, path, reason` with `null` where absent; `summary` keys `entries, eligible, removed, kept, skipped`.
  - `exit_code.rs`: `cleanup_exits_1_only_on_a_kept_action_a_failed_listing_or_a_lost_lock`: four reports.
  - `main.rs` clap tests: `cleanup_parses_its_three_flags` and `a_bare_cleanup_is_a_usage_error` (`Cli::try_parse_from(["flux", "cleanup"])`
    exits 2).
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-cli`
Expected: compile errors naming `cleanup_report`.

- [ ] **Step 3: Implement** the three files.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-cli/src
git commit -m "feat: flux cleanup DEST - the command, its report and exit codes (cut 9b)"
```

---

### Task 8: End to end on the real filesystem (`crates/flux-cli/tests/cleanup.rs`)

**Files:**
- Create: `crates/flux-cli/tests/cleanup.rs` (its own `flux()`, `Stalled` and `names` helpers copied from `tests/run.rs:10-110`; the stall
  hook works for cleanup because its `before_mutation` is the same `debug_hook()`)
- Test: that file

**Interfaces:**
- Consumes: the `flux` binary (`env!("CARGO_BIN_EXE_flux")`), `flux_core::state::{OperationState, Cleanup, native_hex, decode}`,
  `flux_core::lock::record::LockRecord`, `serde_json`.

- [ ] **Step 1: Write the tests** (they are written against the finished binary, so they fail until Tasks 1-7 are merged; run them last):
  - `a_killed_copy_is_listed_resumable_and_dry_run_changes_nothing`: `Stalled::start(src, dst, 3, marks)` then `kill` (as
    `a_killed_run_leaves_a_resumable_operation_that_restart_supersedes`, `tests/run.rs:127`); `flux cleanup dst --dry-run` exits 0, stdout has
    one row with `RESUMABLE` and `no`, and a byte-for-byte snapshot of `dst` and its parent is unchanged; `flux cleanup dst` (no force) exits 0,
    changes nothing; `flux cleanup dst --force` exits 0, stdout has `removed`, `dst/.flux` is gone and no `*.flux-partial.*` remains under `dst`.
  - `a_hand_written_completed_record_with_a_leftover_is_finished_by_cleanup_and_by_copy`: write `dst/.flux/operations/<A>/manifest` =
    `OperationState::created(..)` with `state: Completed`, `cleanup_pending: true`, one artifact `sub/b.flux-partial.<A>`, and that file: `flux
    cleanup dst` exits 0 with `COMPLETED_BUT_UNCLEAN  yes` in stdout and both are gone. Then the same fixture again and `flux copy src dst`:
    exit 0, stderr has `note: finished cleaning up the earlier operation <A> (1 leftovers removed)`, the JSON (`--json`) has 18 keys.
  - `a_cleanup_during_a_live_copy_lists_it_live_and_deletes_nothing` (Review Focus 3): `Stalled` copy alive; `flux cleanup dst` exits 0, the
    row is `LIVE`, no `removed`; kill the copy; `flux cleanup dst --force` then removes it.
  - `a_cleanup_killed_after_abandoning_finishes_on_the_next_run` (Review Focus 4): a killed copy's workspace (as above) with its partial;
    `flux cleanup dst --force` started with `FLUX_TEST_STALL_AT=2` (the ABANDONED write is guarded mutation 1, the partial's removal 2) and
    killed at its stall; the manifest now reads `ABANDONED`; a second `flux cleanup dst` (no force) exits 0, lists the row `STALE  yes` and
    removes it.
  - `an_orphan_root_lock_blocks_a_copy_and_cleanup_reclaims_it`: the fixture of `a_dead_owners_record_naming_a_missing_workspace_is_artifact_ownership_uncertain`
    (`tests/run.rs:381-397`) with `last_heartbeat_wall_time: 1`: `flux copy` exits 3 and its stderr contains `flux cleanup`; `flux cleanup dst
    --dry-run` lists `root-lock ... STALE  yes`; `flux cleanup dst` exits 0, `removed`, and `dst.flux-lock` and any `dst.flux-lock.broken.*` are gone.
  - `exit_codes_and_json_shape`: a missing `dst` with no lock: exit 0, `--json` gives `{"destination":..,"dry_run":false,"entries":[],"actions":[],"summary":{..}}`;
    a FILE at `dst`: exit 3, stderr starts with `DESTINATION_ERROR`; `dst/.flux` a file: exit 3 `CONTROL_PLANE_NAMESPACE_CONFLICT`; `flux cleanup`
    with no argument: exit 2.
  - `a_leftover_that_cannot_be_removed_exits_1` (`#[cfg(unix)]`): the completed fixture with the leftover in a directory made read-only
    (`std::fs::set_permissions(sub, 0o555)`): exit 1, stdout has `kept`, the workspace stays; restore permissions in a guard so `TempDir` drops.
- [ ] **Step 2: Run them**

Run: `cargo nextest run -p flux-cli --test cleanup`
Expected: all pass.

- [ ] **Step 3: Run the gate, push, read CI** (Windows and macOS runtime differences surface only there).

Run: `just check && git push`
Expected: 0 failed locally; CI green on ubuntu, windows, macOS.

- [ ] **Step 4: Commit**

```bash
git add crates/flux-cli/tests/cleanup.rs
git commit -m "test: flux cleanup end to end - dry run, force, a live copy, a kill mid-pass, an orphan lock (cut 9b)"
```

---

### Task 9: Documentation and the known limits (`TODO.md`, `README.md`, the spec's status)

**Files:**
- Modify: `TODO.md:399-448` (the "Cut 9a known limits" item 4 reworded to the narrowed limit; "Cut 9a debt" item 1 (the artifact validation)
  moved to "Closed"), new sections `## Cut 9b known limits` (the spec's eight, verbatim) and `## Cut 9b debt` (anything deferred during
  execution; empty if nothing) placed after "Cut 9a debt"
- Modify: `README.md:51-57` (the `flux cleanup` line gets its one-line description beside `flux copy`'s, if `flux copy` has one; otherwise the
  "Commands" block stays and a short "Cleanup" paragraph follows it: `flux cleanup DEST [--dry-run] [--force] [--json]` and what the six statuses
  mean, ten lines at most)
- Modify: `docs/superpowers/specs/2026-10-08-cut-9b-cleanup-design.md:3` (append `; implemented by docs/superpowers/plans/2026-10-09-cut-9b-cleanup.md`)

- [ ] **Step 1: Make the edits.** No code changes.
- [ ] **Step 2: Run the gate** (typos checks the prose)

Run: `just check`
Expected: 0 failed.

- [ ] **Step 3: Commit**

```bash
git add TODO.md README.md docs/superpowers/specs/2026-10-08-cut-9b-cleanup-design.md
git commit -m "docs: cut 9b known limits, the cleanup command in the README, the 9a limit narrowed"
```
