# Cut 9a: resuming an interrupted copy, file by file - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy --resume` continues an interrupted tree or single-file copy as the SAME operation, skipping every
file the prior run published and claimed, with no write-ahead log.

**Architecture:**
- **State:** manifest format 3 (`state.rs`) adds `roots`, `options` and `configuration_fingerprint` (BLAKE3 of the
  exact-match options). Formats 1 and 2 still decode and are never rewritten upward.
- **Adoption:** `open_operation` (`run/session.rs`) gets a resume arm: exactly one resumable format-3 prior is
  validated (`run/resume.rs`: format, fingerprint, roots, options, durability) and adopted through a new
  `Place::adopt`, which opens or recreates `state.db` (`DirHandle::open_claim_store`, `RedbClaimStore::open_file`),
  then the lock record and the manifest are written under the prior's id.
- **Walk:** `copy_one` (`tree.rs`) gets a resume branch before the policy decision: an own `Created` claim whose
  destination entry is a regular file of the source's size (and mtime within 2 s when times are preserved) is skipped
  and counted as resumed.
- **CLI:** `--resume` (conflicts with `--restart`), the `resuming operation` / `no prior operation` note, the
  `resumed <n> already-complete files` line, and the end-to-end kill-and-resume tests.

**Tech Stack:** Rust 2024 (MSRV 1.98.1), `redb` 4.3.x, `blake3` 1.8 (already a `flux-core` dependency), `clap`,
`serde_json`, the `FaultFs` fake, nextest via `just`.

**Spec:** `docs/superpowers/specs/2026-10-08-cut-9a-resume-design.md` (approved 2026-10-08 at `91c34c3`). Executors
read it with this plan; where the two differ the spec wins and the executor reports the conflict.

## Global Constraints

- Rust edition 2024; `rust-version = "1.98.1"` (`Cargo.toml:11`). The gate is `just check` = `cargo fmt --check` +
  `cargo clippy --workspace --all-targets -- -D warnings` + `typos` + `cargo nextest run --workspace --no-tests=pass`
  + `cargo test --doc --workspace`. Push after every task and read CI (Windows-only failures surface only there).
- Writes under DEST go through `DirHandle`s (section 149.7). `state.db` is opened by `DirHandle::open_claim_store`,
  read and write, never following a link, never creating.
- Section 99: a heartbeat then the ownership guard precede every mutation under DEST; the resume skip performs NO
  mutation and so calls neither.
- A claim is never released. The operation id never changes across resumes. Formats 1 and 2 are never upgraded.
- Exit codes (section 55): a resumed skip is not a failure; `INCOMPATIBLE_STATE`, `STATE_CORRUPT` and
  `RESUMABLE_OPERATION_EXISTS` refusals that changed nothing exit 3; `--resume --restart` exits 2 (clap).
- The section 53 JSON stays at its 18 keys (`crates/flux-cli/src/report.rs:404`, `the_json_has_every_section_53_field_in_order`).
- Every new test goes red under a one-line mutant of the code it guards (the implementer names the mutant in the
  commit message or the report; the controller measures a sample).
- Message texts are the spec's, verbatim (the "Messages and exit codes" table). Tests pin them by substring.
- Commit messages: `feat:` / `test:` / `docs:` prefixes, one commit per task's "Commit" step.

## Decisions this plan makes (the spec's "Open points for the plan")

1. **Corrupt versus incompatible at `open_claim_store`:** two new `flux_fs::Code` variants, `StateCorrupt`
   (`"STATE_CORRUPT"`) and `IncompatibleState` (`"INCOMPATIBLE_STATE"`). The run maps them onto the existing
   `LockCode::StateCorrupt` / `LockCode::IncompatibleState` refusals; any other code is the run's `Failed` at
   `RunStep::State`.
2. **`RunConfig` carries `resume: bool`.** The options to compare are the `CopyOptions` the run already receives;
   `open_operation` gains an `opts: &CopyOptions` parameter.
3. **`Place::adopt` shares nothing with `create`:** adoption never creates a workspace. The shared part (format,
   fingerprint, roots, options, durability) is `resume::validate`, called by `open_operation` before `adopt`.
4. **The format-3 fields live in one struct `Config`** (`roots`, `options`, `configuration_fingerprint`) on
   `OperationState.config: Option<Config>`, `Some` exactly for format 3, serialized flat like `Cleanup` and `FileFields`.
   Option values are strings with the spec's spellings and are validated on decode.
5. **The stored source root** is `state::absolute_lexical(path)`: `std::path::absolute` then `.` dropped and `..`
   popped lexically (links not resolved).
6. **The run reports resume through `Run.resumed: Option<ResumeNote>`** (`StartedNew` or
   `Adopted { operation_id, claims: Option<u64> }`), printed by the CLI after the copy's records and the run lines,
   before the summary. `TreeOutcome` gains `files_resumed` and `bytes_resumed`; `Report::tree` folds them into
   `files_skipped` / `bytes_skipped`.
7. **A single-file adoption** keeps `creation_wall_time`, `source_identity`, `target_identity` and `target_path_key`;
   it sets `attempt_id` to the run's new id, `artifact_generation += 1`, `owner_instance_id` / `boot_session_id` to
   this run's, and `last_heartbeat_wall_time` to the run's `now`.
8. **The fake's `open_claim_store`** reopens the in-memory map created at that path; a file at the path that no store
   made is `StateCorrupt` (zero-length or not), and `FaultFs::set_claim_store_format(path, n)` makes the next open of
   that path `IncompatibleState` when `n != 1`.
9. **Line numbers cite `91c34c3`.** Later tasks shift them; executors re-locate by symbol.
10. **Two or more resumable priors** are refused by ONE message naming all of them, with or without `--resume`;
    `--restart` still supersedes all (unchanged).

## Review Focus

1. **A resumed run whose source file was replaced by a same-size file during the downtime (preserve_times Default).**
   Expected: the mtime check (2 s tolerance) catches it and the normal policy re-copies it. Task 7
   (`an_mtime_outside_two_seconds_is_recopied_and_two_seconds_counts_as_equal`).
2. **`--resume` after the destination directory was recreated (new directory identities).** Expected: every claim
   misses, the copy proceeds under the policy, nothing is resumed, exit 0. Task 7
   (`a_claim_keyed_by_another_directory_identity_resumes_nothing_and_copies_safely`).
3. **A killed run whose `state.db` is zero-length or missing with the manifest already `TRANSFERRING`.** Expected:
   `STATE_CORRUPT`, exit 3, nothing changed, the lock released. Task 6
   (`a_transferring_or_failed_manifest_without_a_usable_state_db_is_state_corrupt`).
4. **The adopted id reaching the partial names.** Expected: the killed run's `<name>.flux-partial.<id>` is removed by
   the step-1 sweep of the resumed run. Task 6 (`the_adopted_id_names_the_partials_and_the_old_partial_is_swept`) and
   Task 9 end to end.
5. **`--resume` with a prior from an older binary (format 1 or 2).** Expected: `INCOMPATIBLE_STATE` naming the format
   and `--restart`, exit 3; without `--resume` the refusal says an older version made it. Task 6
   (`a_format_1_or_2_prior_is_refused_with_the_older_version_messages`).

---

### Task 1: `Code::StateCorrupt`, `Code::IncompatibleState`, `ClaimStore::count` and `DirHandle::open_claim_store` (`flux-fs`)

**Files:**
- Modify: `crates/flux-fs/src/error.rs:11-71` (two variants and their strings), `:117-141` (the test)
- Modify: `crates/flux-fs/src/claims.rs:109-119` (the trait), `:137-147` (`run_all`), new conformance case
- Modify: `crates/flux-fs/src/fs.rs:317-322` (the trait method beside `create_claim_store`; no test module in
  `flux-fs` implements `DirHandle`, verified)
- Test: `crates/flux-fs/src/error.rs`, `crates/flux-fs/src/claims.rs`

**Interfaces:**
- Produces:
  ```rust
  // error.rs
  pub enum Code { /* existing variants unchanged */ StateCorrupt, IncompatibleState }
  // as_str: Code::StateCorrupt => "STATE_CORRUPT", Code::IncompatibleState => "INCOMPATIBLE_STATE"
  // claims.rs
  pub trait ClaimStore {
      fn insert_if_absent(&mut self, key: &ClaimKey, record: &ClaimRecord) -> Result<ClaimOutcome>;
      fn get(&self, key: &ClaimKey) -> Result<Option<ClaimRecord>>;
      fn upgrade_own_claim(&mut self, key: &ClaimKey, target: &FluxPathKey) -> Result<()>;
      fn flush(&mut self) -> Result<()>;
      /// The number of claims in the store.
      fn count(&self) -> Result<u64>;
  }
  pub mod conformance { pub fn count_follows_inserts_and_upgrades<S: ClaimStore>(s: S); /* added to run_all */ }
  // fs.rs, in DirHandle, after create_claim_store:
  /// Open the EXISTING claim store `name` (cut 9a): read and write, never following a link, never creating.
  /// A missing name is `Code::IoError` kind `NotFound`; a link `Code::SafetyRejected`; a directory
  /// `Code::DestinationError` kind `IsADirectory`; a zero-length or undecodable file `Code::StateCorrupt`; a
  /// store of another format `Code::IncompatibleState`.
  fn open_claim_store(&self, name: &std::ffi::OsStr, durability: crate::Durability) -> Result<Self::Claims>;
  ```
- Consumes: nothing new.

- [ ] **Step 1: Write the failing tests**
  - `error.rs` `every_code_has_the_spec_string`: add `assert_eq!(Code::StateCorrupt.as_str(), "STATE_CORRUPT");` and
    `assert_eq!(Code::IncompatibleState.as_str(), "INCOMPATIBLE_STATE");`.
  - `claims.rs` conformance `count_follows_inserts_and_upgrades`: a fresh store counts 0; after inserting keys
    `key(1,"a")`, `key(1,"b")`, `key(2,"a")` it counts 3; a re-insert of `key(1,"a")` (Present) keeps 3;
    `upgrade_own_claim(&key(1,"a"), &target)` keeps 3; `flush` keeps 3. Add it to `run_all`.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-fs`
Expected: compile errors naming `StateCorrupt` and `count`.

- [ ] **Step 3: Implement** the variants, the trait method, the conformance case, and `open_claim_store` on the
  trait. Every `DirHandle` implementor in the workspace must compile: add `open_claim_store` to any test-only
  implementor in `flux-fs` (returning `Err(FsError::new(Code::IoError, io::Error::from(ErrorKind::Unsupported)))`
  where the implementor is a stub). Real implementors are Tasks 2 and 3; until then `cargo build --workspace` fails
  for `flux-platform` and `flux-core`, which is expected at the END of this task only if those crates' implementors
  are not stubbed: to keep every commit green, add the method to `FaultFs` (`crates/flux-core/src/fault_fs.rs:1466`,
  as a stub returning `Err(Code::IoError, Unsupported)` with `count` on `FakeClaimStore` returning the map's length)
  and to `flux-platform` (`dir_unix.rs:209`, `dir_windows.rs:300`, as the same stub; `RedbClaimStore::count` as
  `Err(IoError, Unsupported)`). Tasks 2 and 3 replace the stubs.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-fs crates/flux-core/src/fault_fs.rs crates/flux-platform/src
git commit -m "feat: Code::StateCorrupt and IncompatibleState, ClaimStore::count, DirHandle::open_claim_store (cut 9a)"
```

---

### Task 2: `RedbClaimStore::open_file`, `count`, and the platform `open_claim_store` (`flux-platform`)

**Files:**
- Modify: `crates/flux-platform/src/claims.rs:32-72` (rename `from_file` to `create_file`, add `open_file`, the error
  mapping, `count`), `:138-175` (tests)
- Modify: `crates/flux-platform/src/dir_unix.rs:209-222` (callers of `from_file`; add `open_claim_store` after it)
- Modify: `crates/flux-platform/src/dir_windows.rs:300-309` (same)
- Modify: `crates/flux-platform/tests/claims_kill.rs:39,105` and `claims_store.rs:11-18` (`from_file` -> `create_file`)
- Test: `crates/flux-platform/tests/claims_store.rs` (open cases), `crates/flux-platform/tests/claims_kill.rs`
  (reopen-and-continue), `crates/flux-platform/tests/dir_handle.rs` (link / directory / missing refusals)

**Interfaces:**
- Produces:
  ```rust
  impl RedbClaimStore {
      /// `file` must be empty (a fresh `state.db`). Writes `meta.format` = 1 durably. (Today's `from_file`, renamed.)
      pub fn create_file(file: File, durability: Durability) -> Result<Self>;
      /// Open an EXISTING store. Rejects a zero-length file (`Code::StateCorrupt`) before redb sees it, then
      /// `Builder::create_file` (which opens and repairs an existing database), then reads `meta.format`:
      /// absent `meta` or `claims` table -> `Code::StateCorrupt`; `format` other than 1 -> `Code::IncompatibleState`.
      pub fn open_file(file: File, durability: Durability) -> Result<Self>;
  }
  // error mapping (private fns), by KIND never by byte count:
  //   DatabaseError::Storage(StorageError::Corrupted(_)) | DatabaseError::RepairAborted => Code::StateCorrupt
  //   DatabaseError::UpgradeRequired(_)                                                  => Code::IncompatibleState
  //   DatabaseError::Storage(StorageError::Io(_) | StorageError::PreviousIo)             => Code::IoError
  //   DatabaseError::DatabaseAlreadyOpen | DatabaseError::TransactionInProgress           => Code::IoError
  //   every other StorageError                                                            => Code::IoError
  //   TableError::TableDoesNotExist(_)                                                   => Code::StateCorrupt
  //   TableError::Storage(e)                                                              => as StorageError above
  //   every other TableError                                                              => Code::StateCorrupt
  // ClaimStore::count: begin_read, open_table(CLAIMS), ReadableTableMetadata::len, errors through the same mapping.
  // dir_unix.rs: open_claim_store = check_component; openat(RDWR | NOFOLLOW | CLOEXEC, Mode::empty()) mapping
  //   Errno::LOOP -> SafetyRejected, Errno::ISDIR -> DestinationError kind IsADirectory (as open_lock does);
  //   fstat regular-file check (as open_lock); then RedbClaimStore::open_file.
  // dir_windows.rs: open_claim_store = check_component; open_file_at(&self.0, name, FILE_OPEN); then open_file.
  ```
- Consumes: Task 1's `Code` variants and trait method.

- [ ] **Step 1: Write the failing tests**
  - `claims_store.rs`:
    - `a_store_reopens_with_its_claims_and_count`: create a store with `create_file`, insert 3 claims, `flush`, drop
      it; `open_file` on the same path (`OpenOptions::new().read(true).write(true).open`) reads all 3 back
      (`get`) and `count() == 3`; a 4th insert then `count() == 4`.
    - `a_zero_length_file_is_state_corrupt_on_open`: an empty file -> `open_file(...).unwrap_err().code == Code::StateCorrupt`.
    - `a_file_that_is_not_a_store_is_state_corrupt_on_open`: a file holding `b"not a redb file at all, long enough"`
      (> 64 bytes of text) -> `Code::StateCorrupt`.
    - `a_store_whose_format_is_not_1_is_incompatible_on_open`: build a redb database by hand (`Builder::new().create_file`,
      open `meta` and `claims` tables, insert `format` = 2, commit), drop, `open_file` -> `Code::IncompatibleState`.
    - `a_store_missing_its_meta_table_is_state_corrupt_on_open`: a redb database with only a `claims` table ->
      `Code::StateCorrupt`.
  - `claims_kill.rs` `killed_mid_stream_the_store_reopens_with_a_prefix`: after the existing assertions, drop `db`,
    reopen through `RedbClaimStore::open_file` (not `Builder` directly), assert `count()` equals `m`, insert one more
    claim with `key_for(u64::MAX)`, assert `count() == m + 1`. (The existing `Builder::create_file` reopen stays as
    the raw-redb check; the `open_file` reopen is the engine-path check.)
  - `tests/dir_handle.rs` (add beside the `open_lock` cases; find them with `rg -n "open_lock" crates/flux-platform/tests/dir_handle.rs`):
    `open_claim_store_refuses_a_link_a_directory_and_a_missing_name`: a symlink (Unix) / junction (Windows, skip
    where the test file already skips junction creation) at the name -> `Code::SafetyRejected`; a directory ->
    `Code::DestinationError` with kind `IsADirectory`; a missing name -> kind `NotFound`; a real store made by
    `create_claim_store`, dropped, reopened by `open_claim_store` with `count() == 0`.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-platform`
Expected: compile errors naming `open_file`, `create_file`.

- [ ] **Step 3: Implement** as the Interfaces block gives it. `open_file`: `file.metadata()?.len() == 0` ->
  `StateCorrupt` ("zero-length claim store"); `Builder::new().set_cache_size(CACHE_BYTES).create_file(file)` mapped;
  `begin_read` mapped; `open_table(META)` mapped; `meta.get("format")?` is `Some(1)` else `None` -> `StateCorrupt`
  ("no format"), `Some(n)` -> `IncompatibleState` ("claim store format {n}, this binary reads 1"); `open_table(CLAIMS)`
  mapped (its absence is `TableDoesNotExist` -> `StateCorrupt`). `with_cache_size` keeps creating (used by the cache
  test); `open_file` may take a private `open_with_cache_size` for symmetry. `unsynced` starts at 0 on open.
- [ ] **Step 4: Run the tests and the gate**

Run: `cargo nextest run -p flux-platform` then `just check`
Expected: 0 failed (the kill test takes about a minute on Linux).

- [ ] **Step 5: Commit**

```bash
git add crates/flux-platform
git commit -m "feat: RedbClaimStore::open_file and count, open_claim_store on both platforms (cut 9a)"
```

---

### Task 3: The fake's `open_claim_store` and `count` (`flux-core` `fault_fs.rs`)

**Files:**
- Modify: `crates/flux-core/src/fault_fs.rs:118-126` (`Inner` fields), `:131-200` (`FakeClaimStore`, `count`),
  `:1466-1486` (`create_claim_store` records the path), the stub from Task 1 (replace), `:605-700` (the helper
  `set_claim_store_format`), and its `mod tests` beside `create_claim_store_refuses_a_taken_name` (`:2493`)
- Test: `crates/flux-core/src/fault_fs.rs` `mod tests`

**Interfaces:**
- Produces:
  ```rust
  // Inner gains:
  //   claim_files: HashMap<PathBuf, ClaimMap>,   // path -> the map create_claim_store made there
  //   claim_formats: HashMap<PathBuf, u64>,      // set_claim_store_format; absent means 1
  impl FaultFs {
      /// The next `open_claim_store` of `path` answers `Code::IncompatibleState` when `format != 1`.
      pub fn set_claim_store_format(&self, path: impl AsRef<Path>, format: u64);
  }
  // FakeDirHandle::open_claim_store(name, durability):
  //   check_component; record "open_claim_store(<path>)" with fault key "open_claim_store";
  //   missing -> IoError NotFound; a symlink (types == Symlink) -> SafetyRejected; a directory -> DestinationError
  //   kind IsADirectory; claim_formats[path] != 1 -> IncompatibleState; claim_files[path] -> a FakeClaimStore on
  //   that map (claim_stores_open += 1); a file with no map -> StateCorrupt (message "zero-length claim store" when
  //   its bytes are empty, "not a claim store" otherwise).
  // FakeClaimStore::count -> Ok(map.len() as u64), recording "claim_count" with fault key "claim_count".
  ```
- Consumes: Task 1.

- [ ] **Step 1: Write the failing tests** in `fault_fs.rs` `mod tests`:
  - `open_claim_store_reopens_what_create_made`: `create_claim_store("state.db")`, insert 2 claims, drop; `open_claim_store("state.db")`
    reads both (`get`) and `count() == 2`; `claim_stores_open()` is 1 while open and 0 after drop.
  - `open_claim_store_refuses_what_is_not_a_store`: a missing name -> kind `NotFound`; `write_file(.., b"")` ->
    `Code::StateCorrupt`; `write_file(.., b"garbage")` -> `Code::StateCorrupt`; a directory at the name ->
    `Code::DestinationError`; `add_symlink` at the name -> `Code::SafetyRejected`; after `set_claim_store_format(path, 2)`
    a real store -> `Code::IncompatibleState`.
  - `open_claim_store_is_fault_injectable`: `fs.fail("open_claim_store", Code::PermissionDenied)` makes the open fail
    with that code.
  - Add `count` to the fake's conformance run (find `conformance::run_all` in `fault_fs.rs` tests; it now includes
    `count_follows_inserts_and_upgrades` from Task 1).
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core fault_fs`
Expected: failures naming `open_claim_store` / `set_claim_store_format`.

- [ ] **Step 3: Implement** as the Interfaces block gives it. `create_claim_store` inserts the map into `claim_files`
  as well as `claim_stores`. `remove_file` of a `state.db` keeps its existing bookkeeping; also remove the
  `claim_files` entry so a later `write_file` + open sees "not a store".
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/fault_fs.rs
git commit -m "feat: the fake's open_claim_store, count and set_claim_store_format (cut 9a)"
```

---

### Task 4: Manifest format 3 - `Config`, `Root`, `Options`, the fingerprint and `absolute_lexical` (`state.rs`)

**Files:**
- Modify: `crates/flux-core/src/state.rs:15-48` (constants and key lists), `:142-163` (`OperationState`), `:174-238`
  (constructors and `encode`), `:240-298` (`decode`), `:307-393` (`validate`), `:751-1393` (tests; the helpers
  `tree_v2` / `file_v2` at `:1157-1163` switch to `created_v2`)
- Modify: `crates/flux-core/src/run/session.rs:147` and `crates/flux-core/src/run/tests.rs:445,1298` (callers of
  `created` -> `created_v2` in the tests; `session.rs` is rewired in Task 6, so for THIS task change its call to
  `created_v2` to keep the build green)
- Test: `crates/flux-core/src/state.rs` `mod tests`

**Interfaces:**
- Produces (all `pub` in `flux_core::state`):
  ```rust
  pub const FORMAT_VERSION: u64 = 3;   // the version this binary writes
  pub const V2: u64 = 2;               // cut 7b's version: read, rewritten as itself
  pub const V1: u64 = 1;               // unchanged
  const CONFIG_KEYS: [&str; 3] = ["roots", "options", "configuration_fingerprint"];   // version 3's keys, both kinds
  pub const FINGERPRINT_HEADER: &str = "flux-config-v1\n";

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
  pub struct Root { pub source_root: String, pub source_identity: String, pub destination_prefix: String }
      // source_root: native_hex of the absolute source root; source_identity: identity_text; destination_prefix:
      // "" for a tree, native_hex of the target's name for a single file
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
  pub struct Options { pub preserve_times: String, pub preserve_permissions: String, pub durability: String,
                       pub safety: String, pub existing: String }
  impl Options {
      /// The run's options as the manifest spells them: Preserve Off|Default|Strict -> "off"|"default"|"strict";
      /// Durability Normal|Strict -> "normal"|"strict"; Safety Default|Strict -> "default"|"strict";
      /// ExistingPolicy Overwrite|Update|SkipExisting -> "overwrite"|"update"|"skip-existing".
      pub fn of(opts: &flux_fs::CopyOptions) -> Self;
      /// FINGERPRINT_HEADER then "preserve_times=<v>\n", "preserve_permissions=<v>\n", "safety=<v>\n",
      /// "existing=<v>\n" in that order. `durability` is NOT covered (compared by rule).
      pub fn canonical_bytes(&self) -> Vec<u8>;
      /// Lowercase hex of blake3::hash(canonical_bytes()).
      pub fn fingerprint(&self) -> String;
  }
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
  pub struct Config { pub roots: Vec<Root>, pub options: Options, pub configuration_fingerprint: String }
  impl Config { pub fn new(roots: Vec<Root>, options: Options) -> Self /* fingerprint computed */ }

  pub struct OperationState { /* existing fields */ #[serde(skip)] pub config: Option<Config> }
      // `Some` exactly when format_version == 3

  impl OperationState {
      pub fn created_v1(..) -> Self;                                   // unchanged
      /// Today's `created`, renamed: a version-2 record as a 7b binary wrote it (tests stage it).
      pub fn created_v2(operation_id: &str, kind: Kind, destination_root: &Path, created_at: u64,
                        file: Option<FileFields>) -> Self;
      /// A new operation's state as THIS binary writes it: version 3, CREATED.
      pub fn created(operation_id: &str, kind: Kind, destination_root: &Path, created_at: u64,
                     file: Option<FileFields>, config: Config) -> Self;
  }
  /// `std::path::absolute(path)`, then `.` components dropped and each `..` popping the previous Normal component
  /// (a `..` at the root is dropped). Links are not resolved.
  pub fn absolute_lexical(path: &Path) -> PathBuf;
  ```
- `decode`: `version != V1 && version != V2 && version != FORMAT_VERSION` -> `Incompatible`; expected keys: `KEYS`,
  plus `CLEANUP_KEYS` when version >= 2, plus `FILE_KEYS` when version >= 2 and kind is `file`, plus `CONFIG_KEYS` when
  version == 3. `config` is taken with `take(&CONFIG_KEYS)` and `from_value::<Config>`.
- `validate` additions: `config.is_some() == (format_version == FORMAT_VERSION)`; `cleanup.is_some() ==
  (format_version >= V2)`; `file.is_some() == (format_version >= V2 && kind == File)`; with a config: `roots.len() ==
  1`; every `source_root` has `from_native_hex(..).is_some()`; every `source_identity` has `parse_identity(..).is_some()`;
  `destination_prefix` is empty or `from_native_hex(..).is_some()`; each option value is in its set above;
  `configuration_fingerprint` is exactly 64 lowercase hex characters. (The fingerprint's VALUE is checked at resume,
  Task 5, not here.)
- `encode`: version 3 extends the map with `object(config)` after `cleanup` and `file`.
- Consumes: `blake3` (already `flux-core`'s dependency, `crates/flux-core/Cargo.toml:14`), `flux_fs::{CopyOptions,
  Preserve, Durability, Safety, ExistingPolicy}`.

- [ ] **Step 1: Write the failing tests** in `state.rs` `mod tests` (helpers: `options_default()` = `Options::of` of a
  `CopyOptions` with every default; `config()` = `Config::new(vec![Root { source_root: native_hex(Path::new("/s")),
  source_identity: "strong:3:9".into(), destination_prefix: String::new() }], options_default())`; `tree_v3()` and
  `file_v3()` via `created(.., config())`):
  - `a_version_3_tree_state_carries_the_config_keys_and_round_trips`: `keys(&tree_v3())` == sorted `KEYS` +
    `CLEANUP_KEYS` + `CONFIG_KEYS`; `decode(&s.encode()) == Ok(s)`; `s.format_version == 3`.
  - `a_version_3_file_record_carries_the_file_and_config_keys_and_round_trips`: keys == `KEYS` + `CLEANUP_KEYS` +
    `FILE_KEYS` + `CONFIG_KEYS`; round trip.
  - `created_v2_still_writes_version_2`: `tree_v2().format_version == 2`, `config.is_none()`, keys unchanged (the
    existing version-2 tests keep passing as they are, with the helpers renamed).
  - `options_of_spells_every_value_as_the_spec_does`: assert the full mapping (all three `Preserve` values for both
    preserve fields, both `Durability`, both `Safety`, all three `ExistingPolicy`).
  - `the_fingerprint_is_blake3_of_the_canonical_lines_and_ignores_durability`: `options_default().canonical_bytes()
    == b"flux-config-v1\npreserve_times=default\npreserve_permissions=default\nsafety=default\nexisting=overwrite\n"`;
    `fingerprint() == blake3::hash(&canonical_bytes()).to_hex().to_string()`; the same options with
    `durability = "strict"` fingerprint identically; with `existing = "update"` differently.
  - `version_3_consistency_rules_are_state_corrupt`: each of these decodes to `Err(Unusable::Corrupt(_))`: a
    version-3 record missing `roots` ("missing key roots"); a version-2 record carrying `roots` ("unknown key
    roots"); `roots: []`; two roots; a `source_root` that is not native hex (`"zz"`); a `source_identity` of
    `"strong:x"`; a `destination_prefix` of `"zz"`; `existing: "keep"`; `preserve_times: "yes"`; `durability:
    "fast"`; `safety: "loose"`; a fingerprint of 63 characters; an uppercase-hex fingerprint.
  - `a_version_4_record_is_incompatible`: `{"format_version":4}` -> `Err(Unusable::Incompatible(4))` (update
    `format_version_is_judged_before_any_other_key` at `:800`, `prior.rs:345` and `:419`, and
    `crates/flux-cli/tests/run.rs:312` (`unreadable_or_newer_state_is_refused_and_preserved`), which all stage
    `{"format_version":3}` as "newer" today, to stage 4).
  - `absolute_lexical_normalizes_dot_and_dot_dot_without_resolving_links`: `absolute_lexical(Path::new("x/./y/../z"))
    == std::env::current_dir().unwrap().join("x").join("z")`; an already-absolute `/a/b/../c` (Windows:
    `C:\a\b\..\c`) gives `/a/c`; `/..` gives `/`.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core state`
Expected: compile errors naming `Config`, `created_v2`, `absolute_lexical`.

- [ ] **Step 3: Implement** as the Interfaces block gives it. Rename `created` -> `created_v2` and add the new
  `created`; update `state.rs:1157-1163`, `run/tests.rs:445,1298` and `session.rs:147` to `created_v2` (the last one
  temporarily; Task 6 replaces it).
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed. The run tests still pass: a real run still writes version 2 until Task 6.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/state.rs crates/flux-core/src/run crates/flux-core/src/prior.rs crates/flux-cli/tests/run.rs
git commit -m "feat: manifest format 3 - roots, options and the configuration fingerprint (cut 9a)"
```

---

### Task 5: `resume::validate` - format, fingerprint, roots, options and durability (`run/resume.rs`)

**Files:**
- Create: `crates/flux-core/src/run/resume.rs`
- Modify: `crates/flux-core/src/run/mod.rs:11-15` (`mod resume;`), `:136-151` (`RunWarning::ResumeMappingByPath`)
- Modify: `crates/flux-core/src/prior.rs:193-202` (`corrupt` becomes `pub(crate)`)
- Modify: `crates/flux-cli/src/report.rs:327-359` (render the new warning; `:743-760` test array gains it)
- Test: `crates/flux-core/src/run/resume.rs` `mod tests`

**Interfaces:**
- Produces:
  ```rust
  // run/mod.rs
  pub enum RunWarning { /* existing */
      /// Cut 9a: the source root's identity could not decide the mapping check (a side is not Strong), so the
      /// stored path decided, byte for byte. `path` is the source root this run names.
      ResumeMappingByPath(PathBuf),
  }
  // run/resume.rs
  /// The spec's Compatibility table, in this order: format, fingerprint, roots, the four exact-match options,
  /// durability. `Ok` is the `Config` the adopted manifest will carry (the stored one, `durability` set to
  /// "strict" on a normal -> strict resume). Every `Err` is `RunError::Refused { changed: false, not_removed: None }`.
  pub(crate) fn validate(prior: &PriorOp, roots: &[Root], opts: &CopyOptions, warnings: &mut Vec<RunWarning>)
      -> Result<Config, RunError>;
  ```
- Refusal details (exact):
  - format 1 or 2: `LockCode::IncompatibleState`, `operation <id> was created by an older version (format <n>) and has no options to compare; run again with --restart`
  - fingerprint: `prior::corrupt(&prior.shown, "the configuration fingerprint does not match the stored options")`
  - roots: `LockCode::IncompatibleState`, `the source root differs: the operation copies <stored source_root as from_native_hex display, or the hex when it does not decode> (<stored identity>), this run names <roots[0] path display> (<identity>); run again with --restart to supersede it`;
    destination prefix: `LockCode::IncompatibleState`, `the destination differs: the operation writes <stored prefix display> and this run names <prefix display>; run again with --restart to supersede it`
  - an option: `LockCode::IncompatibleState`, `<option>: the operation runs with <stored>, this run asks <now>; run again with the same option, or with --restart to supersede it`
    where `<option>` is one of `preserve_times`, `preserve_permissions`, `safety`, `existing` (checked in that order)
  - durability: `LockCode::IncompatibleState`, `durability: the operation runs with strict durability; pass --durability strict`
- The roots rule: `roots.len() != stored.len()` is a source-root mismatch; for the one pair, parse both identities;
  when both are `FileIdentity::Strong`, equal identities pass (the path is ignored) and different ones refuse;
  otherwise the `source_root` strings must be equal byte for byte, and a pass pushes `ResumeMappingByPath`.
  `destination_prefix` must always be equal.
- CLI: `run_warning_line(ResumeMappingByPath(p))` = `warning: the source root's identity could not confirm the mapping, so {p} was matched by its path alone`.
- Consumes: Task 4's `Config`, `Root`, `Options`; `PriorOp` (`prior.rs:20`); `LockCode` (`lock/error.rs`);
  `prior::corrupt`.

- [ ] **Step 1: Write the failing tests** in `resume.rs` `mod tests` (helpers: `prior3(state_edit)` builds a
  `PriorOp` whose state is `created(id, Tree, "/d", 1, None, Config::new(roots_now(), Options::of(&opts())))` with
  `shown` = `/d/.flux/operations/<id>/manifest`; `roots_now()` = one `Root` with `source_root: native_hex("/s")`,
  `source_identity: "strong:3:9"`, empty prefix; `refused(r)` extracts `(LockCode, detail)`):
  - `a_matching_prior_validates_to_its_stored_config_with_no_warning`.
  - `a_format_1_or_2_prior_is_incompatible_naming_its_format`: both versions; detail contains `format 2` /
    `format 1` and `--restart`.
  - `a_tampered_fingerprint_is_state_corrupt`: `configuration_fingerprint` replaced by 64 `0`s ->
    `LockCode::StateCorrupt`, detail contains `fingerprint` and `remove it by hand`.
  - `strong_identities_decide_and_the_path_is_ignored`: stored `source_root` = hex of `/elsewhere`, same strong
    identity -> `Ok`, no warning; different strong identity, same path -> refused, detail contains `source root`.
  - `a_weak_side_falls_back_to_the_path_and_warns`: stored identity `weak:3:9` and now `strong:3:9`, same path ->
    `Ok` with exactly one `ResumeMappingByPath(PathBuf::from("/s"))`; different path -> refused.
  - `a_different_destination_prefix_is_refused`.
  - `each_exact_match_option_is_compared_and_named`: for each of the four options, a prior whose stored value
    differs (`preserve_times: "strict"`, `preserve_permissions: "strict"`, `safety: "strict"`, `existing: "update"`)
    against the default run: refused, detail starts with `<option>:`. The stored fingerprint must be recomputed for
    the edited options so this test is about the option, not the fingerprint.
  - `durability_normal_to_strict_is_recorded_and_strict_to_normal_is_refused`: prior `normal`, run strict ->
    `Ok(config)` with `config.options.durability == "strict"`; prior `strict`, run normal -> refused with the exact
    detail `durability: the operation runs with strict durability; pass --durability strict`.
  - `report.rs` `every_run_warning_is_one_warning_line_naming_its_path`: add `RunWarning::ResumeMappingByPath(p())`
    to the array.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core resume`
Expected: compile errors naming `validate`.

- [ ] **Step 3: Implement** `validate` and the warning as the Interfaces block gives them.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/run crates/flux-core/src/prior.rs crates/flux-cli/src/report.rs
git commit -m "feat: resume::validate - the compatibility check for --resume (cut 9a)"
```

---

### Task 6: Adoption in the run - `RunConfig::resume`, `Place::adopt`, the refusal messages, the effective id (`run/`)

**Files:**
- Modify: `crates/flux-core/src/run/mod.rs:43-60` (`RunConfig::resume`), `:79-87` (`Run::resumed`, `ResumeNote`),
  `:154-228` (`tree`: `TreePlace` gains `src_root` / `src_identity`; the options are rebuilt from
  `locked.state.operation_id` after `open_operation`; `run.resumed`), `:322-385` (`file`: `FilePlace` gains `src`;
  same rebuild)
- Modify: `crates/flux-core/src/run/session.rs:21-29` (`Locked::resumed`), `:99-238` (`open_operation`: the `opts`
  parameter, the scan match, the adopt branch, the effective id), `:277-293` (`record_for` takes the id)
- Modify: `crates/flux-core/src/run/place.rs:26-66` (`Place::roots`, `Place::adopt`), `:178-197` (`TreePlace`
  fields; its literal is `run/mod.rs:209-220`), `:251-448` (`TreePlace` impl), `:463-553` (`FilePlace` fields and
  impl; its literal is `run/mod.rs:371-377`)
- Modify: `crates/flux-core/src/prior.rs:151-163` (`resumable_refusal(&[PriorOp])`), `:486-497` (its test)
- Modify: `crates/flux-core/src/run/tests.rs:26-37` (`cfg()` gains `resume: false`), `:103-115` (`prior` stays v1;
  add `prior3`), `:686-693` (`file_prior`; add `file_prior3`), and the new tests
- Test: `crates/flux-core/src/run/tests.rs`

**Interfaces:**
- Produces:
  ```rust
  // run/mod.rs
  pub struct RunConfig { /* existing */ /// `--resume`: continue the one resumable prior operation as itself.
                         pub resume: bool }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum ResumeNote { /// `--resume` found no resumable prior: a new operation started.
                        StartedNew,
                        /// The prior adopted; `claims` is its claim count (a tree), `None` for a single file.
                        Adopted { operation_id: String, claims: Option<u64> } }
  pub struct Run<T> { pub copy: Option<T>, pub stop: Option<RunError>, pub warnings: Vec<RunWarning>,
                      /// Cut 9a: set once the run adopted or started under `--resume`; `None` without the flag or
                      /// when the run stopped before step 5.
                      pub resumed: Option<ResumeNote> }
  // run/session.rs
  pub(crate) struct Locked<'a, D> { /* existing */ pub(crate) resumed: Option<ResumeNote> }
  pub(crate) fn open_operation<'a, D: DirHandle, P: Place<D>>(site: &LockSite<'a, D>, capability: LockCapability,
      place: &mut P, cfg: &RunConfig, opts: &CopyOptions, warnings: &mut Vec<RunWarning>) -> Result<Locked<'a, D>, RunError>;
  fn record_for<D: DirHandle>(site: &LockSite<'_, D>, operation_id: &str, cfg: &RunConfig, workspace_path: String, now: u64) -> LockResult<LockRecord>;
  // run/place.rs
  pub(crate) trait Place<D: DirHandle> { /* existing methods unchanged */
      /// Cut 9a: the one root this run records (format 3 `roots`).
      fn roots(&self) -> Vec<Root>;
      /// Cut 9a: adopt a validated prior. A tree opens `operations/<id>/state.db` (`open_claim_store`), or, while
      /// `prior.state` is `CREATED` and the file is absent, zero-length or `Code::StateCorrupt`, removes what is
      /// there and creates a fresh store; returns the claim count. A single file opens nothing and returns `None`.
      fn adopt(&mut self, prior: &OperationState, durability: Durability) -> Result<Option<u64>, RunError>;
  }
  pub(crate) struct TreePlace<'p, F> { /* existing */ pub(crate) src_root: PathBuf, pub(crate) src_identity: FileIdentity }
  pub(crate) struct FilePlace<'p, D> { /* existing */ pub(crate) src: PathBuf }
  // prior.rs
  /// `RESUMABLE_OPERATION_EXISTS` for the resumable priors found (at least one), with the way out.
  pub fn resumable_refusal(ops: &[PriorOp]) -> LockError;
  ```
- `resumable_refusal` details (exact, from the spec):
  - one prior at format 3: `operation <id> is <STATE> (<shown>); run again with --resume to continue it or --restart to supersede it (its partials and its recorded progress are deleted)`
  - one prior at format 1 or 2: `operation <id> is <STATE> (<shown>), created by an older version, so --resume cannot continue it; run again with --restart to supersede it (its partials and its recorded progress are deleted)`
  - two or more: `operations <id1> (<STATE1>, <shown1>), <id2> (<STATE2>, <shown2>)[, ...]; --resume needs exactly one; run again with --restart to supersede all of them (their partials and their recorded progress are deleted)`
- `TreePlace::roots`: `vec![Root { source_root: native_hex(&absolute_lexical(&self.src_root)), source_identity:
  identity_text(self.src_identity), destination_prefix: String::new() }]`. `FilePlace::roots`: the same from
  `self.src` / `self.source_identity`, `destination_prefix: native_hex(Path::new(&self.target))`.
- `TreePlace::adopt` errors (every refusal `changed: false`, `not_removed: None`; `<db>` = `operations_shown().join(id).join(STATE_DB)`):
  - `open_claim_store` `Ok(store)` -> `store.count()` (an `Err` is `failed(RunStep::State, &<db>, e)`), keep the store in `self.claims`.
  - `Err` with kind `NotFound`, or `code == Code::StateCorrupt`, while `prior.state == OpState::Created`: remove
    `STATE_DB` through the workspace handle (`NotFound` tolerated; another error is `failed(RunStep::State, &<db>, e)`),
    then `create_claim_store` (an `Err` is `failed(RunStep::State, &<db>, e)`); count 0.
  - `Err` with kind `NotFound` otherwise: `Refused { LockCode::StateCorrupt }` detail `<db>: the workspace has no state.db; Flux never deletes state it cannot read (§249.4): inspect it, and remove it by hand if it is not needed`.
  - `Err` with `code == Code::StateCorrupt` otherwise: `Refused { LockCode::StateCorrupt }` detail `<db>: <e.source>; Flux never deletes state it cannot read (§249.4): inspect it, and remove it by hand if it is not needed`.
  - `Err` with `code == Code::IncompatibleState`: `Refused { LockCode::IncompatibleState }` detail `<db>: <e.source>; use the Flux version that wrote it, or run again with --restart to supersede it`.
  - any other `Err`: `failed(RunStep::State, &<db>, e)`.
  `self.operations` is set from `operations_dir(dest, &self.dest_shown)` (DEST exists: the scan found the prior
  under it).
- `open_operation`'s step 4, complete:
  ```rust
  let own_id = cfg.operation_id.as_str();
  let (priors, adopt): (Vec<PriorOp>, Option<PriorOp>) = match place.scan(own_id) {
      Ok(scan) if cfg.restart => (scan.resumable, None),
      Ok(scan) if scan.resumable.is_empty() => (Vec::new(), None),
      Ok(mut scan) if cfg.resume && scan.resumable.len() == 1 => (Vec::new(), Some(scan.resumable.remove(0))),
      Ok(scan) => {
          let refused = resumable_refusal(&scan.resumable);
          let error = from_lock(refused, RunStep::Inspect, place.destination(), made.is_some());
          return Err(give_back(obtained, error, &lock_shown));
      }
      Err(e) => {
          let error = from_lock(e, RunStep::Inspect, place.destination(), made.is_some());
          return Err(give_back(obtained, error, &lock_shown));
      }
  };
  ```
  then, while `made.is_none()`: with `Some(prior)`, `resume::validate(&prior, &place.roots(), opts, warnings)` (an
  `Err` is given back), `place.adopt(&prior.state, opts.durability)` (an `Err` is given back), the adopted state =
  `prior.state.clone()` with `config = Some(config)` and, when `file` is `Some`, `attempt_id = attempt_id.clone()`,
  `artifact_generation += 1`, `owner_instance_id = cfg.owner_instance_id.clone()`, `boot_session_id =
  cfg.boot_session_id.clone()`, `last_heartbeat_wall_time = now.to_string()`; `resumed = Some(Adopted { operation_id,
  claims })`. With `None` and `cfg.resume`: `resumed = Some(StartedNew)`. The new-state branch calls
  `OperationState::created(own_id, place.kind(), place.destination(), now, file, Config::new(place.roots(), Options::of(opts)))`.
  From here on `id` is `state.operation_id` (the effective id): `record_for(site, id, cfg, place.workspace_path(id), now)`,
  `place.shown(id)`. `supersede` runs with `priors` (empty under `--resume`). The TRANSFERRING write is unchanged.
- `run::tree`: `TreePlace { .., src_root: src_root.to_path_buf(), src_identity: source.identity }`; after
  `open_operation`: `let opts = CopyOptions { operation_id: OperationId::new(&locked.state.operation_id), ..opts };`
  before `Shared` is built; `run.resumed = locked.resumed.clone()` right after `open_operation` returns `Ok`.
  `run::file`: `FilePlace { .., src: src.to_path_buf() }`; the same rebuild before `copy_file_guarded`.
- Consumes: Tasks 1-5.

- [ ] **Step 1: Write the failing tests** in `run/tests.rs`. Helpers: `resume() -> RunConfig { resume: true, ..cfg() }`;
  `prior3(fs, n, state) -> OperationState` writes a version-3 tree manifest for `id(n)` under `/p/dest` whose
  `config` is `Config::new(vec![Root { source_root: native_hex(&absolute_lexical(Path::new("/src"))),
  source_identity: identity_text(fs.metadata("/src").identity), destination_prefix: "" }], Options::of(&opts()))`,
  returning the state; `prior_store(fs, n) -> FakeClaimStore` = `fs.destination_root("/p/dest/.flux/operations/<id(n)>").create_claim_store("state.db", Normal)`;
  `file_prior3(fs, n)` writes a version-3 single-file record for target `t` with `FileFields` as `file_prior` writes
  them (generation 1) and a config whose root is `/src/a`'s identity and `destination_prefix: native_hex("t")`;
  `lock_record_during(fs, nth_create_new) -> Arc<Mutex<Option<LockRecord>>>` reads `LOCK` on the n-th `create_new`.
  - `resume_with_no_prior_starts_a_new_operation_and_says_so`: `run_tree(&fs, &resume())` -> `ok`, `r.resumed ==
    Some(ResumeNote::StartedNew)`, the manifest written is version 3 (hook: read `manifest(fs, ID)` on the 1st
    `create_new` of a partial; `config.unwrap().roots[0].source_identity == identity_text(/src)`), nothing left behind.
  - `a_new_tree_manifest_is_version_3_with_roots_options_and_fingerprint`: via `kept_tree_manifest`: `format_version
    == 3`, `config.roots.len() == 1`, `destination_prefix == ""`, `options == Options::of(&opts())`,
    `configuration_fingerprint == options.fingerprint()`. (Update `a_tree_manifest_is_version_2_with_cleanup_keys_and_no_file_fields`
    at `:1216` to assert `FORMAT_VERSION` - it already does - and rename it `..._is_the_current_version_...`.)
  - `resume_adopts_a_prior_in_each_resumable_state_under_its_own_id`: for `Created`, `Transferring`, `Failed`:
    `prior3` + `prior_store` (empty, dropped), `run_tree(&fs, &resume())` -> `ok`; `r.resumed == Some(Adopted {
    operation_id: id(5), claims: Some(0) })`; `calls` contain no `create_dir(/p/dest/.flux/operations/{ID}.creating)`
    and no `create_claim_store(`; the lock record read during the copy has `operation_id == id(5)` and
    `workspace_path == "operations/<id5>"`; afterwards `!exists(/p/dest/.flux)` and `!exists(LOCK)`; `files_copied == 2`.
  - `the_adopted_id_names_the_partials_and_the_old_partial_is_swept`: `fs.write_file("/p/dest/a.flux-partial.<id5>", b"half")`;
    after the resumed run `calls` contain `remove_file(/p/dest/a.flux-partial.<id5>)` before
    `create_new(/p/dest/a.flux-partial.<id5>)`, and no partial named `{ID}` was ever created.
  - `the_adopted_manifest_goes_transferring_before_the_copy`: hook on the 1st partial `create_new`: `manifest(fs,
    id5).state == Transferring` and its `format_version == 3`, `config` unchanged.
  - `two_resumable_priors_are_refused_naming_both_with_and_without_resume`: `prior3(5)`, `prior3(6)`; for `cfg()`
    and `resume()`: `refused == (ResumableOperationExists, false)`, detail contains `id(5)`, `id(6)`,
    `--resume needs exactly one`; the lock removed; both manifests untouched.
  - `a_single_format_3_prior_without_resume_advertises_resume`: `prior3(5)` + `cfg()`: detail contains
    `--resume to continue it` and `--restart`.
  - `a_format_1_or_2_prior_is_refused_with_the_older_version_messages`: `prior(fs, 5, Failed)` (v1) and a v2
    prior (`created_v2`): without `--resume`: `ResumableOperationExists`, detail contains `created by an older
    version` and `--restart`, not `--resume to continue`; with `--resume`: `(IncompatibleState, false)`, detail
    contains `format 1` / `format 2` and `--restart`; the lock removed; the manifest untouched.
  - `an_incompatible_option_is_refused_naming_it_and_nothing_changes`: `prior3` then edit its options to `existing:
    "update"` (recompute the fingerprint) -> `run_tree(&fs, &resume())`: `(IncompatibleState, false)`, detail starts
    `existing:`; the manifest's state unchanged; `!exists(LOCK)`; no `open_claim_store(` call.
  - `durability_normal_to_strict_is_recorded_on_adoption`: `prior3` (normal) resumed with `CopyOptions {
    durability: Strict, ..opts() }` and the copy made to fail (`fs.fail("open_read", Code::IoError)`): the kept
    manifest has `state == Failed` and `config.options.durability == "strict"`; the reverse (prior strict, run
    normal) is `(IncompatibleState, false)` with the exact spec detail.
  - `a_mapping_mismatch_is_refused_and_strong_identities_decide`: (a) prior root identity `strong:7:7` (not
    `/src`'s) -> `(IncompatibleState, false)`, detail contains `source root`; (b) prior `source_root` =
    `native_hex("/elsewhere")` with `/src`'s identity -> adopted, no warning; (c) `fs.set_identity("/src",
    FileIdentity::Weak(..))` with the prior's identity text `weak:..` and the same path -> adopted with
    `RunWarning::ResumeMappingByPath(PathBuf::from("/src"))`.
  - `a_tampered_fingerprint_is_state_corrupt_on_resume`: `(StateCorrupt, false)`, the lock removed.
  - `a_created_manifest_without_a_usable_state_db_resumes_with_a_fresh_store`: `prior3(5, Created)` with (a) no
    `state.db`, (b) `write_file(state.db, b"")`, (c) `write_file(state.db, b"garbage")`: adopted with `claims:
    Some(0)`; `calls` contain `open_claim_store(/p/dest/.flux/operations/<id5>/state.db)` then
    `create_claim_store(...<id5>/state.db)`; in (b) and (c) a `remove_file(...<id5>/state.db)` between them.
  - `a_transferring_or_failed_manifest_without_a_usable_state_db_is_state_corrupt`: for `Transferring` and `Failed`
    with (a), (b), (c) above: `(StateCorrupt, false)`; `!exists(LOCK)`; the manifest untouched; no
    `create_claim_store(` call.
  - `an_incompatible_state_db_is_refused`: `prior_store` then `fs.set_claim_store_format(path, 2)` ->
    `(IncompatibleState, false)`, detail contains `state.db`.
  - `a_state_db_that_cannot_be_opened_fails_the_run_at_the_state_step`: `fs.fail("open_claim_store",
    Code::PermissionDenied)` -> `failed_at(&r.stop) == (RunStep::State, path ending state.db)`; `!exists(LOCK)`.
  - `the_claim_count_is_reported_on_adoption`: `prior_store` with 2 claims inserted (and `flush`), dropped ->
    `claims: Some(2)`.
  - `a_resume_that_fails_before_transferring_can_be_resumed_again`: `fs.fail_nth("rename_replace", 1,
    Code::PermissionDenied, PermissionDenied)` makes the TRANSFERRING write fail: `r.stop` is `Failed` at
    `RunStep::State`; the manifest still holds the prior's state; a second `run_tree(&fs, &resume())` on the same
    fake (clear the fault; `fail_nth` is one-shot) completes with `Adopted { id(5), .. }`.
  - `a_single_file_resume_adopts_the_record_bumping_its_generation`: `file_prior3(fs, 5)` + `write_file("/p/t.flux-partial.<id5>", b"half")`;
    `run_file(&fs, &resume())` completes; `r.resumed == Some(Adopted { operation_id: id(5), claims: None })`; the
    record read on the 1st `create_new` has `artifact_generation == 2`, a new `attempt_id` (an id, not the prior's),
    `owner_instance_id == id(0xee)`, `creation_wall_time` unchanged, `state == Transferring`; the partial was removed
    before the new one was created; `/p/t == b"A"`; no record and no lock remain.
  - `a_single_file_record_for_another_source_is_refused`: `file_prior3` edited to `source_identity` / root identity
    `strong:7:7` -> `(IncompatibleState, false)`; `!exists(T_LOCK)`; the record untouched.
  - `restart_still_supersedes_everything_and_ignores_resume_semantics`: `prior3(5)`, `prior3(6)` + `restart()` ->
    both ABANDONED then removed, exactly as `restart_supersedes_a_prior_deleting_its_partials_then_its_workspace`
    does (keep that test; this one asserts two version-3 priors).
  - `prior.rs` `a_resumable_refusal_names_the_operation_and_the_way_out`: update to the slice signature; add the
    older-version and the two-priors variants (detail substrings as above).
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core run::tests`
Expected: compile errors naming `resume`, `resumed`, `adopt`.

- [ ] **Step 3: Implement** as the Interfaces block gives it, in this order: `prior.rs` messages; `RunConfig` /
  `Run` / `ResumeNote` / `Locked`; `Place::roots` and `Place::adopt`; `open_operation`; the two runners. Every
  existing full `RunConfig { .. }` literal gains `resume: false`: `run/tests.rs:27` and `:1635`, `crates/flux-cli/src/main.rs:133`,
  `crates/flux-core/tests/safety_std_fs.rs:125`, `crates/flux-core/tests/replace_std_fs.rs:25` and `:110` (the
  `..cfg()` / `..restart()` literals need nothing).
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed. Push and read CI: the Windows leg exercises `open_file_at(.., FILE_OPEN)` for real only in Task 9,
but the fake tests must be green on all three systems.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core crates/flux-cli/src/main.rs
git commit -m "feat: --resume adopts the one resumable prior as the same operation (cut 9a)"
```

---

### Task 7: The walk's resume branch (`tree.rs`)

**Files:**
- Modify: `crates/flux-core/src/tree.rs:27-62` (`TreeOutcome::files_resumed`, `bytes_resumed`), `:311-324`
  (`Shared::resume`), `:374-382` (the lockless site: `resume: false`), `:736-750` (a `resumed_complete` helper beside
  `update_replaces`), `:826-874` (the `Resolved::Entry` arm of `copy_one`), and the four test `Shared` literals at
  `:2103,2176,2212,2238` (`resume: false`)
- Modify: `crates/flux-core/src/run/mod.rs:241-249` (`resume: cfg.resume`), `crates/flux-core/src/run/tests.rs:2285`
  (`resume: false`)
- Modify: `crates/flux-core/src/fault_fs.rs` `FakeClaimStore::get` records `claim_get(<name>)` with fault key
  `claim_get` (so a lookup failure is injectable)
- Test: `crates/flux-core/src/run/tests.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct TreeOutcome { /* existing */
      /// Cut 9a: files the prior run completed, verified and skipped. Not in `files_skipped`; the CLI adds them.
      pub files_resumed: u64,
      /// Cut 9a: the source bytes of the files in `files_resumed`.
      pub bytes_resumed: u64 }
  pub(crate) struct Shared<'c, F> { /* existing */ /// Cut 9a: `--resume`: an own `Created` claim may skip its target.
                                     pub(crate) resume: bool }
  /// Cut 9a, decision 2: the destination entry is a regular file of the source's length and, with times preserved
  /// (`preserve_times != Preserve::Off`), both modification times are known and differ by at most 2 seconds.
  /// With `Preserve::Off`, or either time unknown, never true.
  fn resumed_complete(src: &Metadata, dest: &Metadata, preserve_times: Preserve) -> bool;
  ```
- The branch, inserted in `copy_one` right after the `names.owner(&stored)` fold check (`tree.rs:829-840`) and
  before the `meta.file_type == FileType::Dir` guard (`:841`), run only when `cx.resume`:
  0. `cx.fs.metadata(&src)`: `Err(e)` -> `report(out, on_report, path, TreeFailureCause::Copy(CopyError::at(CopyStep::Source, e)))`, return `Ok(())`.
  1. `claims.borrow().get(&ClaimKey::new(parent_id, &stored))`: `Err(e)` -> `finish_copy(path, Err(CopyError::at(CopyStep::Claim, e)), out, on_report)`.
  2. `Some(r)` with `r.target == target && r.status == ClaimStatus::Created`: if `resumed_complete(&src_meta, &meta,
     cx.opts.preserve_times)`: `out.files_resumed += 1; out.bytes_resumed += src_meta.len;
     names.record_publication(&target, Some(&stored), name, None, meta.identity); return Ok(())`. Otherwise fall
     through to the directory guard and the policy decision unchanged.
  3. `Some(r)` with `r.target == target` and `Existing`: fall through unchanged.
  4. `Some(r)` with another target: `finish_copy(path, Err(CopyError::at(CopyStep::Claim, FsError::new(Code::DestinationNamespaceCollision, io::Error::other("another target of this operation already claimed that destination entry")))), out, on_report)`.
  5. `None`: fall through unchanged.
  The `src_meta` read here is reused by the policy decision below it (the existing `cx.fs.metadata(&src)` call at
  `:852` becomes "the one already read, or read now when the branch did not run").
- Consumes: Task 6 (`RunConfig::resume`, the adopted store), Task 3 (`claim_get`).

- [ ] **Step 1: Write the failing tests** in `run/tests.rs`. Helpers: `claimed(fs, n, name, status, target)` inserts
  a claim `(strong(fs, "/p/dest"), name) -> ClaimRecord { target: key(target), status }` into `prior_store(fs, n)`
  (flushed, dropped); `src_at(fs, path, bytes, secs)` = `put` for a source file; `dest_at` likewise. Every test
  uses `prior3(fs, 5, Transferring)`, `run_tree_with(&fs, &resume(), &with_policy(..))` and sources `/src/a` (1 byte)
  and `/src/sub/b`.
  - `an_own_created_claim_with_matching_size_and_mtime_is_skipped_as_resumed`: `/src/a` at mtime 100, `/p/dest/a`
    1 byte at mtime 101, `claimed(.., "a", Created, "a")`: `ok`, `files_resumed == 1`, `bytes_resumed == 1`,
    `files_copied == 1`, `files_overwritten == 0`, no `create_new(/p/dest/a.flux-partial.`, no `claim_insert(a)` and
    no `claim_upgrade(a)`; `/p/dest/a` keeps its old bytes; `r.resumed == Some(Adopted { .., claims: Some(1) })`.
  - `an_mtime_outside_two_seconds_is_recopied_and_two_seconds_counts_as_equal`: dest mtime 102 -> resumed; 103 ->
    `files_overwritten == 1`, `files_resumed == 0` (overwrite policy); src newer by 3 s likewise recopied.
  - `a_size_mismatch_goes_to_the_normal_path_under_each_policy`: dest `b"old!"` (4 bytes), mtime equal: overwrite ->
    `files_overwritten == 1`; update -> `files_overwritten == 1` (size differs); skip-existing -> `files_skipped == 1`,
    `files_resumed == 0`, the file untouched.
  - `a_missing_or_unknown_mtime_never_resumes`: dest written with `write_file` (mtime unavailable in the fake) ->
    recopied under overwrite; `opts.preserve_times = Preserve::Off` with equal mtimes -> recopied.
  - `an_absent_destination_with_an_own_created_claim_takes_the_new_file_path`: claim present, no `/p/dest/a` ->
    `files_copied == 2`, the claim stays `Created` for `a`, no failure.
  - `a_directory_at_a_claimed_name_fails_that_target_as_a_destination_error`: `/p/dest/a` is a directory, claim
    `Created` -> one failure with `Code::DestinationError` for `a`, `sub/b` copied.
  - `an_own_existing_claim_is_redone`: status `Existing`, dest equal in size and mtime -> `files_overwritten == 1`,
    `files_resumed == 0`.
  - `a_foreign_claim_on_resume_is_a_collision`: `claimed(.., "a", Created, "other")` -> one failure
    `DestinationNamespaceCollision` for `a` at `CopyStep::Claim`; `/p/dest/a` untouched.
  - `a_claim_lookup_error_fails_that_target_only`: `fs.fail("claim_get", Code::IoError)` -> `a` fails with
    `Code::IoError` at `CopyStep::Claim`; `sub/b` copied (its lookup happens after the one-shot fault).
  - `a_resumed_skip_registers_the_owner_so_a_fold_onto_it_is_refused`: `fs.set_case_insensitive(true)`; sources
    `/src/B` and `/src/b` (both 1 byte, mtime 100); dest `/p/dest/B` 1 byte mtime 100 with `claimed(.., "B", Created,
    "B")`: `B` is resumed (`files_resumed == 1`) and `b` fails `DestinationNamespaceCollision` whose source message
    contains `already wrote that destination entry` (the owner check, not the claim check: a mutant that drops
    `record_publication` makes it `already claimed` instead).
  - `a_claim_keyed_by_another_directory_identity_resumes_nothing_and_copies_safely`: the claim inserted under
    `ObjectId { volume: 1, index: 999 }` -> `files_resumed == 0`, `files_overwritten == 1`, no failure.
  - `a_weak_identity_directory_reports_existing_files_as_collisions_on_resume`: `fs.set_identity("/p/dest",
    FileIdentity::Weak(ObjectId { volume: 1, index: 1 }))` and `/p/dest/a` present -> `a` fails
    `DestinationNamespaceCollision`, `files_resumed == 0`, `replace_degraded.is_some()`.
  - `a_source_that_became_unreadable_between_runs_fails_that_target`: `fs.fail_nth("metadata", k, Code::PermissionDenied, PermissionDenied)`
    with `k` chosen so the n-th `metadata` call is the resume branch's stat of `/src/a` (the implementer finds `k`
    from `calls()` and pins it with a comment); `a` fails at `CopyStep::Source`.
  - `the_lockless_copy_tree_never_resumes`: `crate::copy_tree` over a destination holding a matching file: no
    `claim_get` call, `files_resumed == 0` (uses the lockless runner's `resume: false`).
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core run::tests`
Expected: compile errors naming `files_resumed` / `resume`.

- [ ] **Step 3: Implement** as the Interfaces block gives it.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core
git commit -m "feat: the walk skips files the prior run completed and claimed (cut 9a)"
```

---

### Task 8: The CLI - `--resume`, the resume lines, the report fold, and the known limits (`flux-cli`, `TODO.md`)

**Files:**
- Modify: `crates/flux-cli/src/main.rs:37-80` (`CopyArgs::resume`), `:132-141` (`run_config`), `:222-290`
  (printing the resume lines in both branches), `:304-388` (tests)
- Modify: `crates/flux-cli/src/report.rs:68-86` (`Report::tree` folds the resumed counts), `:361-365` (new
  `resume_lines`), tests
- Modify: `TODO.md:398` (insert `## Cut 9a known limits` before `## Scaffolding follow-ups`), `TODO.md:330-331`
  (8b limit 2: append `(file-granular resume landed in cut 9a; the rest stays)`)
- Test: `crates/flux-cli/src/main.rs`, `crates/flux-cli/src/report.rs`

**Interfaces:**
- Produces:
  ```rust
  // main.rs CopyArgs
  /// Continue the one resumable prior operation on DEST as itself: files it completed are verified and skipped,
  /// the rest are copied under the existing-file policy. With no prior operation a new one starts.
  #[arg(long, conflicts_with = "restart")]
  resume: bool,
  // report.rs
  /// Cut 9a: the run's resume note, then `resumed <n> already-complete files` when `files_resumed > 0`.
  pub fn resume_lines<T>(run: &Run<T>, files_resumed: u64) -> Vec<String>;
  //   StartedNew                                   -> "no prior operation; starting new"
  //   Adopted { operation_id, claims: Some(n) }    -> "resuming operation <id> (<n> entries claimed)"
  //   Adopted { operation_id, claims: None }       -> "resuming operation <id>"
  //   then, when files_resumed > 0:                   "resumed <n> already-complete files"
  // Report::tree: r.files_skipped = out.special_files_skipped + out.files_skipped + out.files_resumed;
  //               r.bytes_skipped = out.bytes_skipped + out.bytes_resumed;
  ```
- `main.rs` `copy`: in the `Job::Tree` branch, after `lines(report::run_lines(&run))` and before `summary_line`:
  `lines(report::resume_lines(&run, outcome.files_resumed))`; in the `Job::File` branch the same with `0`.
- `TODO.md` section text: the heading `## Cut 9a known limits`, one line `Recorded by cut 9a
  (\`docs/superpowers/specs/2026-10-08-cut-9a-resume-design.md\`, "Known limits"; the numbers are the spec's).`,
  then the spec's ten items as `- [ ] **N. <title>** <text>` entries, copied from the spec's "Known limits" section
  verbatim (titles are the first clause of each).
- Consumes: Tasks 6 and 7.

- [ ] **Step 1: Write the failing tests**
  - `main.rs`: `resume_and_restart_together_is_a_usage_error` (`Cli::try_parse_from([.., "--resume", "--restart"])`
    errors with `exit_code() == 2`); `resume_reaches_the_config` (`run_config(&parse(&["--resume"])).resume` and
    `!run_config(&parse(&[])).resume`); `break_lock_needs_restart_and_each_run_gets_fresh_ids` unchanged.
  - `report.rs`: `resume_lines_name_the_operation_and_the_counts`: the three notes above with `files_resumed = 0`
    give exactly one line each, verbatim; `Adopted { .., Some(3) }` with `files_resumed = 7` gives two lines, the
    second `resumed 7 already-complete files`; `run.resumed = None` with `files_resumed = 0` gives none.
    `a_tree_report_folds_resumed_files_into_skipped`: `files_skipped = 3, files_resumed = 4, special = 1,
    bytes_skipped = 10, bytes_resumed = 5` -> `r.files_skipped == 8`, `r.bytes_skipped == 15`.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-cli --lib`
Expected: compile errors naming `resume` / `resume_lines`.

- [ ] **Step 3: Implement** the flag, the lines, the fold and the `TODO.md` section.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-cli TODO.md
git commit -m "feat: flux copy --resume, the resume report lines and the cut 9a known limits (cut 9a)"
```

---

### Task 9: End to end on real filesystems - kill, resume, kill again (`crates/flux-cli/tests/run.rs`)

**Files:**
- Modify: `crates/flux-cli/tests/run.rs:109-125` (the existing killed-run test also asserts `--resume to continue`),
  `:193-227` (unchanged), new tests after `:227`
- Test: `crates/flux-cli/tests/run.rs`

**Interfaces:**
- Consumes: the `Stalled` harness (`run.rs:48-93`), `names`, `tree_in`, `copy`; `redb::Database::open` as
  `a_killed_run_leaves_the_claims_up_to_the_last_sync` uses it (`:163`). New helper `same_bytes(src: &Path, dst: &Path)`
  walks `src` with a small recursive `std::fs::read_dir` (no `walkdir`: the crate's dev-dependencies are `redb` and
  `tempfile`, `crates/flux-cli/Cargo.toml:23-25`) and asserts every file exists in `dst` with equal bytes and that
  `dst` holds no extra file outside `.flux`.

- [ ] **Step 1: Write the failing tests** (each builds the 300-file, 3-directory source of
  `a_killed_run_leaves_the_claims_up_to_the_last_sync` but with an EMPTY destination, so every file is new; a file is
  three guarded mutations and a directory one):
  - `a_killed_run_leaves_a_resumable_operation_that_restart_supersedes` (existing): also assert the exit-3 stderr
    contains `--resume to continue it`.
  - `a_killed_tree_copy_resumes_as_the_same_operation`: stall at 500, kill; `id` = the one name under
    `dst/.flux/operations`; `copy(["--resume", "--json", src, dst])`: exit 0; stderr contains `resuming operation
    <id> (` and `resumed ` and ` already-complete files`; the JSON (stdout) has `files_total == 300`,
    `files_copied + files_skipped == 300`, `files_skipped >= 100` (the first directory's claims were synced at its
    end), `files_copied <= 200`; `same_bytes(&src, &dst)`; `dst/.flux` and the lock are gone.
  - `a_resume_killed_again_resumes_a_third_time`: stall at 500, kill; `--resume` stalled at 200, kill (`Stalled`
    needs a variant taking extra args: add `start_args(args: &[&OsStr], ..)` or pass `--resume` through `start_env`'s
    builder - extend the harness with an `args` parameter); `names(ops) == [id]` still; `Database::open(state.db)`
    `claims.len() >= 100`; the manifest decodes with `format_version == 3` and `state == TRANSFERRING`; a third
    `--resume` exits 0 with `same_bytes`.
  - `resume_against_an_incompatible_option_exits_3_and_changes_nothing`: after a kill, `--resume --skip-existing`:
    exit 3, stderr contains `INCOMPATIBLE_STATE` and `existing:`; the workspace and the manifest bytes are
    unchanged; then `--resume` (no policy flag) exits 0.
  - `resume_with_nothing_to_resume_starts_new`: fresh `dst`, `--resume`: exit 0, stderr contains `no prior
    operation; starting new`, `same_bytes`.
  - `resume_and_restart_together_is_a_usage_error`: exit 2.
  - `a_killed_single_file_copy_resumes_keeping_its_id_and_bumping_its_generation`: `Stalled::start(&src.join("a"),
    &t, 3, ..)`, kill; read the record: `id`, `attempt_id`, `artifact_generation == 1`; `--resume` stalled at 2
    (after the sweep: the old partial is gone, no new one yet), kill: exactly one record with the same id,
    `artifact_generation == 2`, a different `attempt_id`, `state == TRANSFERRING`, no `t.flux-partial.` left; a
    third run `--resume` exits 0, `t == b"A"`, `names(d) == ["src", "t"]`.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-cli --test run`
Expected: the new tests fail on the old stderr texts (the flag now exists, so they run).

- [ ] **Step 3: Implement** nothing new in the product: fix the harness helpers only (`Stalled` args, `same_bytes`).
  If a test exposes a product defect, fix it in the crate that owns it and name the fix in the commit.
- [ ] **Step 4: Run the tests and the gate, then push and read CI on all three systems**

Run: `just check`
Expected: 0 failed locally; CI green on ubuntu, windows and macOS.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-cli/tests/run.rs
git commit -m "test: kill-and-resume end to end for a tree and a single file (cut 9a)"
```
