# Cut 8b: replacement, claims and the policy flags - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A directory copy replaces existing destination files as spec section 5.1 requires, under the durable claim
records of section 241.5, with the three policy flags.

**Architecture:**
- **Store:** a `ClaimStore` interface in `flux-fs`; the real store is `redb` over a file the workspace `DirHandle`
  created (`flux-platform`); the test fake is in-memory (`FaultFs`).
- **Walk:** each destination directory's `Frame::Live` carries a `NameIndex` (listing, identity table). `copy_one`
  resolves the planned name, applies the policy, claims, copies and publishes with `rename_replace`, then upgrades the
  claim. The claim is written from a new `before_create` callback in the copy path, after the section 129 gate, a
  heartbeat and the section 99 guard.
- **Lifecycle:** `state.db` is created in the unpublished workspace after the cut 8a probe; syncs happen at directory
  ends and in the store (cap), with a guarded `flush`.

**Tech Stack:** Rust 2024 (MSRV 1.98.1), `redb` 4.3.x, `clap`, the `FaultFs` fake, nextest via `just`.

**Spec:** `docs/superpowers/specs/2026-10-07-cut-8b-replacement-design.md` (approved; amended at `48a8a4a` for "Claim
syncing"). Executors read it with this plan.

## Global Constraints

- Rust edition 2024; `rust-version = "1.98.1"` (`Cargo.toml:11`). The gate is `just check` = fmt-check + `cargo clippy
  --workspace --all-targets -- -D warnings` + typos + nextest and doctests. `cargo deny check` must pass
  (`deny.toml` allows MIT, Apache-2.0, BSD-2/3-Clause, ISC, MPL-2.0, Zlib, Unicode-3.0, CC0-1.0).
- Writes under DEST go through `DirHandle`s (section 149.7); `state.db` is created by `DirHandle::create_claim_store`,
  exclusively, read and write, never following a link, and handed to `redb` as an open `File`.
- Section 99: a heartbeat call then the ownership guard precede every mutation under DEST, a `state.db` write and a
  synced commit included. The claim goes AFTER the section 129 identity gate.
- Resident memory is bounded (section 10.1): no claim set and no whole-tree structure in memory; a directory's listing
  and identity table live only for its `Frame::Live`. `redb`'s cache is a small fixed size (`CACHE_BYTES = 16 MiB`).
- A claim is never released. A claim key is `(parent directory Strong ObjectId, entry name bytes as stored)`; a
  directory whose identity is not `Strong` gets no claims.
- Exit codes (section 55): a skipped file is not a failure; a `ClaimNotRecorded` failure and a collision exit 1; clap
  usage errors exit 2.
- The lockless `copy_tree` stays no-replace. Every tree file is published with `rename_replace` only after a claim.
- Every new test goes red under a one-line mutant of the code it guards (measured by the driver). Real-system tests skip
  locally with a stated reason and FAIL on CI (`CI` set) when their facility is missing.
- Commit messages: `feat:` / `test:` / `docs:` prefixes, one commit per task's "Commit" step.

## Decisions this plan makes

1. **Task 2 is a gate.** If its acceptance test fails after two honest fix attempts, the implementer stops with
   `ACCEPTANCE_FAILED` and the output; the controller brings the owner a new decision. No later task starts first.
2. **The conformance suite lives in `flux-fs`** as a public module `claims::conformance` (always compiled; a few
   small generic functions) so `flux-platform` and `flux-core` tests can both call it without a feature flag.
3. **`FluxPathKey`** does not exist in code yet; Task 1 defines it (section 103: components joined by `0x00`).
4. **Claims reach the walk through `Shared`:** `Shared.claims: Option<&RefCell<<F::Dir as DirHandle>::Claims>>`
   (`None` for the lockless `copy_tree`). `TreePlace` owns the store between its creation and the walk
   (`claims: Option<...>`); `run::tree` takes it out like `place.dest.take()`.
5. **The tree decides the policy for a replacement target and tells the copy path `ExistingPolicy::Overwrite`**, so the
   copy path does not decide twice; the single-file path decides inside `copy_file_guarded` from the CLI's policy.
6. **A created directory skips name resolution** (`NameIndex::for_new_dir`): every target in it is planned as new and a
   case-fold between two source names is caught by the no-replace rename (`AlreadyExists` is a collision, step 2).
7. **The identity gate returns the destination `Metadata` it read** (`GateRead`), so single-file `Update` and `Skip`
   reuse it; the tree's `Update` decision uses the metadata `resolve` read.
8. **Existing tests that pin the old behaviour are updated, not weakened:** `crates/flux-cli/tests/copy.rs`
   `a_folder_merges_into_an_existing_folder_and_reports_each_collision` (a folder over an existing folder now replaces
   by default; Task 11 rewrites it to the new truth and keeps a `--skip-existing` variant that still reports skipped
   files). `crates/flux-core/tests/tree_std_fs.rs` `an_existing_destination_file_is_left_intact_and_reported` is about
   the lockless `copy_tree` and stays.
9. **Line numbers cite `48a8a4a`.** Later tasks shift them; executors re-locate by symbol.

## Review Focus

1. **Two source names that differ only in case, over an existing differently-cased destination entry on a
   case-insensitive filesystem.** The second must be `DESTINATION_NAMESPACE_COLLISION`, never a silent overwrite of the
   first. Pinned in Task 10 (`two_source_names_folding_onto_one_existing_entry_collide`) and Task 6/7.
2. **A symlink, a directory, or a read-only file at the destination name.** Expected: link replaced as a link; directory
   in the way fails only that target; read-only refused per target. Task 10.
3. **A directory with more than 1000 claims.** Expected: the store's cap sync runs and memory stays flat. Task 2
   (`the_cap_syncs_every_1000_unsynced_commits`).
4. **A weak-identity destination directory.** Expected: no claims, no replacement, the old collision report, one aggregated
   warning. Task 10.
5. **A run that fails or is killed halfway.** Expected: claims synced to the last directory end survive in a kept
   workspace; a clean run leaves no `state.db` and no `<id>.removing`. Tasks 9, 11, 13.

---

### Task 1: The claim model, `FluxPathKey` and the conformance suite (`flux-fs`)

**Files:**
- Create: `crates/flux-fs/src/claims.rs`
- Modify: `crates/flux-fs/src/lib.rs` (add `pub mod claims;` and the re-exports)
- Test: `crates/flux-fs/src/claims.rs` (`#[cfg(test)] mod tests`: encoding tests only; the conformance functions are
  exercised by Tasks 2 and 3)

**Interfaces:**
- Produces, in `flux_fs` (re-exported from `claims`):
  ```rust
  pub struct FluxPathKey(pub Vec<u8>);   // section 103
  impl FluxPathKey { pub fn from_relative(rel: &Path) -> Result<Self> }   // Normal components' encoded bytes joined by 0x00;
                                         // an empty path is the empty key; a component containing 0x00 is Err(Code::DestinationError)
  #[derive(Clone, Debug, PartialEq, Eq)] pub struct ClaimKey { pub parent: ObjectId, pub name: Vec<u8> }
  impl ClaimKey { pub fn new(parent: ObjectId, name: &OsStr) -> Self; pub fn encode(&self) -> Vec<u8> }
        // encode: parent.volume (u64 BE) ++ parent.index (u128 BE) ++ name bytes (OsStr::as_encoded_bytes)
  #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum ClaimStatus { Existing, Created }
  #[derive(Clone, Debug, PartialEq, Eq)] pub struct ClaimRecord { pub target: FluxPathKey, pub status: ClaimStatus }
  impl ClaimRecord { pub fn encode(&self) -> Vec<u8>; pub fn decode(bytes: &[u8]) -> Option<Self> }
        // encode: one status byte (0 Existing, 1 Created) ++ target bytes
  pub enum ClaimOutcome { Inserted, Present(ClaimRecord) }
  pub trait ClaimStore {
      fn insert_if_absent(&mut self, key: &ClaimKey, record: &ClaimRecord) -> Result<ClaimOutcome>;
      fn get(&self, key: &ClaimKey) -> Result<Option<ClaimRecord>>;
      fn upgrade_own_claim(&mut self, key: &ClaimKey, target: &FluxPathKey) -> Result<()>;
      fn flush(&mut self) -> Result<()>;
  }
  pub mod conformance {
      pub fn run_all<S: ClaimStore>(new_store: impl Fn() -> S);   // calls every case below with a fresh store
      pub fn insert_if_absent_inserts_then_reports_present<S: ClaimStore>(s: S);
      pub fn an_own_claim_reads_back_as_present_with_the_same_target<S: ClaimStore>(s: S);
      pub fn a_foreign_claim_is_returned_not_replaced<S: ClaimStore>(s: S);
      pub fn upgrade_own_claim_sets_created_and_keeps_the_target<S: ClaimStore>(s: S);
      pub fn upgrade_of_a_missing_key_or_a_foreign_owner_is_an_error<S: ClaimStore>(s: S);
      pub fn the_same_name_under_different_parents_are_different_keys<S: ClaimStore>(s: S);
      pub fn names_differing_only_in_case_are_different_keys<S: ClaimStore>(s: S);
      pub fn flush_is_idempotent_and_keeps_every_claim<S: ClaimStore>(s: S);
      pub fn many_keys_round_trip<S: ClaimStore>(s: S);   // 5000 distinct keys inserted then all read back
  }
  ```
- Consumes: `ObjectId` (`flux-fs/src/fs.rs`), `Result`, `Code`, `FsError`.

- [ ] **Step 1: Write the failing encoding tests** in `claims.rs`'s `mod tests`:
  - `flux_path_key_joins_components_with_nul`: `FluxPathKey::from_relative(Path::new("a/b/c")).unwrap().0 == b"a\0b\0c"`;
    the empty path gives `vec![]`; `Path::new("./a")` gives `b"a"` (CurDir dropped).
  - `claim_key_encodes_volume_then_index_then_name_big_endian`: `ClaimKey::new(ObjectId { volume: 1, index: 2 },
    OsStr::new("x")).encode()` equals `[0,0,0,0,0,0,0,1]` ++ 15 zero bytes ++ `[2]` ++ `b"x"` (assert the exact 8 + 16 + 1
    byte layout).
  - `claim_record_round_trips_and_rejects_an_empty_or_unknown_status`: `decode(&r.encode()) == Some(r)` for both
    statuses; `decode(&[]) == None`; `decode(&[7, 1])` is `None`.
- [ ] **Step 2: Run them to verify they fail to compile**

Run: `cargo nextest run -p flux-fs claims`
Expected: compile error naming `FluxPathKey`.

- [ ] **Step 3: Implement the types, encodings and the trait** as the Interfaces block gives them. The conformance
  functions take a store by value and `assert!` with messages naming the case; `run_all` calls each with `new_store()`.
  `upgrade_of_a_missing_key_or_a_foreign_owner_is_an_error` asserts both calls return `Err`.
- [ ] **Step 4: Run the tests and the gate**

Run: `cargo nextest run -p flux-fs` then `just check`
Expected: 0 failed.
- [ ] **Step 5: Commit**

```bash
git add crates/flux-fs
git commit -m "feat: the claim model, FluxPathKey, the ClaimStore trait and its conformance suite (cut 8b)"
```

---

### Task 2: The `redb` store and the ACCEPTANCE GATE (`flux-platform`)

**This task gates the plan (decision 1).** It is a gate even though the later tasks look independent.

**Files:**
- Modify: `Cargo.toml` (`[workspace.dependencies]`: `redb = "4.3"`), `crates/flux-platform/Cargo.toml` (`redb = { workspace = true }`), `Cargo.lock`
- Create: `crates/flux-platform/src/claims.rs`, `crates/flux-platform/tests/claims_kill.rs`
- Modify: `crates/flux-platform/src/lib.rs` (`mod claims; pub use claims::RedbClaimStore;`)
- Test: `crates/flux-platform/tests/claims_store.rs` (conformance on the real store), `claims_kill.rs` (the gate)

**Interfaces:**
- Consumes: Task 1's `ClaimStore`, `ClaimKey`, `ClaimRecord`, `ClaimOutcome`, `ClaimStatus`, `FluxPathKey`, `conformance`;
  `flux_fs::Durability`.
- Produces:
  ```rust
  pub const CACHE_BYTES: usize = 16 * 1024 * 1024;
  pub const SYNC_CAP: u32 = 1000;
  pub struct RedbClaimStore { /* redb::Database, Durability, unsynced: u32 */ }
  impl RedbClaimStore {
      /// `file` must be empty (a fresh `state.db`) or a valid database. Writes the `meta` entry `format` = 1 durably.
      pub fn from_file(file: std::fs::File, durability: flux_fs::Durability) -> flux_fs::Result<Self>;
  }
  impl flux_fs::ClaimStore for RedbClaimStore { /* as Task 1 */ }
  ```
  Behaviour: one table `claims` (`TableDefinition<&[u8], &[u8]>`, key = `ClaimKey::encode`, value = `ClaimRecord::encode`)
  and one table `meta`. `Builder::new().set_cache_size(CACHE_BYTES).create_file(file)`. Under `Durability::Normal` every
  claim commit uses redb `Durability::None`, except the commit that makes the unsynced count reach `SYNC_CAP`, which is
  `Immediate` and resets the count; `flush` makes an empty `Immediate` commit when the count is above zero and resets it.
  Under `Durability::Strict` every commit is `Immediate` and `flush` does nothing. A redb error becomes
  `FsError::new(Code::IoError, io::Error::other(e))`. `upgrade_own_claim` reads the record, requires `target` to equal
  its target, and rewrites it with `Created`.

- [ ] **Step 1: Write the conformance test** `crates/flux-platform/tests/claims_store.rs`:
  `real_store_passes_the_conformance_suite_normal` and `..._strict`: each makes a store from a fresh `tempfile` opened
  with `OpenOptions::new().read(true).write(true).create_new(true)` and calls `flux_fs::claims::conformance::run_all`.
  Plus `the_cap_syncs_every_1000_unsynced_commits`: under Normal insert 2500 keys, then read the in-process unsynced
  counter through a `#[cfg(test)]`-free public accessor `RedbClaimStore::unsynced(&self) -> u32` and assert it is 500
  (2500 mod 1000), and after `flush()` it is 0.
- [ ] **Step 2: Write the gate** `crates/flux-platform/tests/claims_kill.rs`:
  - `#[test] #[ignore] fn child_writer()`: reads env `FLUX_KILL_FILE` (path), `FLUX_KILL_MODE` (`normal` | `strict`),
    creates the file with `create_new`, builds a `RedbClaimStore`, then loops `i` from 0 forever: inserts the claim whose
    key name is `i` as 8 big-endian bytes under one parent; after the insert returns `Ok` prints `C <i>` and flushes
    stdout; in `normal` mode every 50th claim (`i % 50 == 49`) calls `flush()` and on `Ok` prints `F <i>`.
  - `#[test] fn killed_mid_stream_the_store_reopens_with_a_prefix()`: for each of 24 iterations (kill thresholds taken
    from the list `[1, 2, 3, 7, 49, 50, 51, 99, 100, 101, 149, 150, 333, 1000, 1001, 1500, 2000, 3, 60, 120, 240, 480, 960, 1920]`)
    and for each mode: spawn `std::env::current_exe()` with args `--exact child_writer --ignored --nocapture`
    (and `--test-threads=1`) with the two env vars, read its stdout lines until the number of `C` lines reaches the
    threshold, then `Child::kill()` and `wait()`. Reopen the file with `Builder::new().create_file(OpenOptions` read
    and write`)` (it must return `Ok`: this is the "the file must open" assertion), read every key of the `claims`
    table, and assert: the present indices are exactly `0..m` for some `m` (a PREFIX, no holes); and `m` is at least
    the count implied by the last `F <i>` line seen (normal: `i + 1`) or by the last `C <i>` line seen (strict: `i + 1`).
    Print the mode, threshold, `m`, and the last `F`/`C` seen for every iteration (a `--no-capture` run reads as a table).
  - `#[test] fn a_store_builds_from_an_already_open_file()`: create a file through `std::fs`, build the store from the open
    `File` (not from a path), insert a claim, drop the store, reopen the file by path with plain redb and read it back.
- [ ] **Step 3: Run them to verify they fail to compile**

Run: `cargo nextest run -p flux-platform --test claims_store --test claims_kill`
Expected: compile error naming `RedbClaimStore`.

- [ ] **Step 4: Implement `RedbClaimStore`** in `claims.rs` as the Interfaces block gives it, add the dependency, run
  `cargo deny check`.
- [ ] **Step 5: Run the conformance and the gate locally**

Run: `cargo nextest run -p flux-platform --test claims_store --test claims_kill`, then `cargo deny check`, then `just check`
(the `#[ignore]` child is started by the parent test; do not pass `--run-ignored`).
Expected: PASS on Linux.
- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/flux-platform
git commit -m "feat: the redb claim store and its crash acceptance test (cut 8b)"
```

- [ ] **Step 7: Push the branch and read all three CI legs** (the gate must hold on macOS and Windows):

Run: `git push origin spec/cut-8b`, then `gh run list --branch spec/cut-8b --limit 1`, then read the `Test (macos-latest)` and
`Test (windows-latest)` jobs for `claims_kill` and `claims_store`.
Expected: both green. If any kill iteration shows a hole in the prefix, or the reopen fails, or `a_store_builds_from_an_already_open_file`
fails on any system: STOP and report `ACCEPTANCE_FAILED` with the printed table and the failing job's log. Do not start Task 3.

---

### Task 3: `DirHandle::create_claim_store` on every implementor

**Files:**
- Modify: `crates/flux-fs/src/fs.rs` (the `DirHandle` trait, near `remove_dir` / `sync`)
- Modify: `crates/flux-platform/src/dir_unix.rs` (`impl DirHandle for StdDir`), `crates/flux-platform/src/dir_windows.rs`
- Modify: `crates/flux-core/src/fault_fs.rs` (`impl DirHandle for FakeDirHandle`, `Inner`, new `FakeClaimStore`, FaultFs accessors)
- Test: `crates/flux-platform/tests/dir_handle.rs`, `crates/flux-core/src/fault_fs.rs` tests

**Interfaces:**
- Consumes: Task 2's `RedbClaimStore::from_file`, Task 1's `ClaimStore` and `conformance`.
- Produces, on `DirHandle`:
  ```rust
  type Claims: crate::ClaimStore;
  /// Create `name` exclusively through this handle (read and write, never following a link) and open a claim store on
  /// it. `Durability::Normal` commits without a sync; `Durability::Strict` syncs every commit. Fails with
  /// `ErrorKind::AlreadyExists` if the name is taken.
  fn create_claim_store(&self, name: &std::ffi::OsStr, durability: Durability) -> Result<Self::Claims>;
  ```
  On `FaultFs`: `pub fn claim_count(&self) -> usize` (claims across stores created from this fake),
  `pub fn claim(&self, parent: ObjectId, name: &str) -> Option<ClaimRecord>`; the fake store records
  `claim_insert(<name lossy>)`, `claim_upgrade(<name lossy>)`, `claim_flush` in the call log under the fault keys
  `claim_insert`, `claim_upgrade`, `claim_flush` (so `fail`, `fail_kind`, `fail_nth`, `on_nth` work on them); the fake
  `create_claim_store` records `create_claim_store(<path>)` and creates an EMPTY FILE `state.db` entry at the path (so the
  workspace's `read_dir` and removal tests see it).

- [ ] **Step 1: Write the failing tests.**
  - `crates/flux-core/src/fault_fs.rs` tests: `the_fake_claim_store_passes_the_conformance_suite` (a `FaultFs` with `/d`,
    `d.create_claim_store("state.db", Durability::Normal)` per case via `run_all`); `create_claim_store_refuses_a_taken_name`
    (kind `AlreadyExists`); `a_claim_fault_is_injected_and_consumed` (`fail("claim_insert", Code::IoError)`: first insert
    `Err`, second `Ok`); `claims_are_visible_through_the_fake_accessors`.
  - `crates/flux-platform/tests/dir_handle.rs`, `mod posix` and `mod windows_arm`:
    `create_claim_store_makes_a_store_through_the_handle` (a fresh `TempDir`, `root.create_claim_store("state.db",
    Durability::Normal)`, insert and `flush`, the file `state.db` exists on disk and a second call with the same name is
    `AlreadyExists`); `create_claim_store_never_follows_a_link` (unix: a symlink named `state.db` pointing at a missing file
    -> `AlreadyExists` or `SafetyRejected`, and nothing is created at the target; Windows: a junction named `state.db`
    is refused, mirroring `a_junction_is_refused_as_a_safety_rejection`); `the_real_store_passes_the_conformance_suite_through_the_handle`
    (`run_all` with a closure that makes a store in a fresh directory each time).
- [ ] **Step 2: Run them to verify they fail to compile**

Run: `cargo nextest run -p flux-core fault_fs && cargo nextest run -p flux-platform --test dir_handle`
Expected: compile error naming `create_claim_store`.
- [ ] **Step 3: Implement** the trait method and the three implementations. Unix: `check_component(name)?`, then the same
  `openat` flags `create_lock` uses (`RDWR | CREATE | EXCL | CLOEXEC`, plus `NOFOLLOW`, mode `0o666`) and
  `RedbClaimStore::from_file(File::from(fd), durability)`. Windows: reuse the open path `create_lock` uses
  (`open_lock_at(&self.0, name, FILE_CREATE)` in `dir_windows.rs`), factored so the underlying `File` is available (the
  lock wrapper `StdLock::new(file)` is `lock_file.rs:21`); keep the junction/surrogate refusal. Fake: `Claims =
  FakeClaimStore` over `Arc<Mutex<BTreeMap<Vec<u8>, Vec<u8>>>>` registered in `Inner`; same key and value encodings as
  Task 1.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed. Push for CI after the commit and read the macOS and Windows test jobs.
- [ ] **Step 5: Commit**

```bash
git add crates/flux-fs crates/flux-platform crates/flux-core
git commit -m "feat: DirHandle::create_claim_store on the fake, POSIX and Windows (cut 8b)"
```

---

### Task 4: `ExistingPolicy`, `Outcome.skipped` and the three flags

**Files:**
- Modify: `crates/flux-fs/src/options.rs` (`CopyOptions`, `Outcome`), `crates/flux-fs/src/lib.rs`
- Modify: every `CopyOptions { ... }` construction (add `existing: ExistingPolicy::Overwrite`): `crates/flux-core/src/run/tests.rs:44`,
  `crates/flux-core/src/copy.rs:591`, `crates/flux-core/src/tree.rs:863` (tests' `opts()`),
  `crates/flux-core/tests/safety_std_fs.rs:12`, `crates/flux-core/tests/tree_std_fs.rs:13`, `crates/flux-cli/src/main.rs:88`
  (`options`); the `..opts()` / `..cx.opts.clone()` uses need no change.
- Modify: every `Outcome { ... }` construction (add `skipped: false`): `crates/flux-fs/src/options.rs:118` (its test),
  `crates/flux-cli/src/report.rs:420,443,722`, `crates/flux-cli/src/exit_code.rs:128,182`, `crates/flux-core/src/copy.rs:581`.
- Modify: `crates/flux-cli/src/main.rs` (`CopyArgs`, `options`, the `Copy` doc comment)
- Test: `crates/flux-cli/src/main.rs` tests, `crates/flux-cli/tests/copy.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum ExistingPolicy { Overwrite, Update, SkipExisting }
  // CopyOptions gains:  pub existing: ExistingPolicy,
  // Outcome gains:      pub skipped: bool,   // true: the existing destination was left untouched; bytes_copied == 0
  ```
  CLI: `--overwrite`, `--update`, `--skip-existing` as bool flags in one clap `ArgGroup` (`id = "existing"`, `multiple =
  false`); `options()` maps them (none given = `Overwrite`).

- [ ] **Step 1: Write the failing tests.**
  - `main.rs` tests (beside `safety_strict_reaches_the_options`): `existing_policy_defaults_to_overwrite`,
    `update_and_skip_existing_reach_the_options` (parse `["flux","copy","a","b","--update"]` etc. through the existing `parse`
    helper and assert `options(..).existing`), `giving_two_policies_is_a_usage_error` (`Cli::try_parse_from` with
    `--update --skip-existing` is `Err` and `err.exit_code() == 2`).
  - `crates/flux-cli/tests/copy.rs`: `two_policy_flags_exit_2` (run the binary with `--overwrite --update`: exit code 2).
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-cli`
Expected: compile errors, then failures.
- [ ] **Step 3: Implement** the enum, the two fields, the clap group and the mapping; update every construction site listed
  above mechanically. Update the `Copy` doc comment: it states a folder copy never replaces; replace it with the section
  5.1 policy summary (the behaviour arrives in Task 10, the text may land now).
- [ ] **Step 4: Run the gate**

Run: `just check`
Expected: 0 failed.
- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "feat: ExistingPolicy, Outcome.skipped and the --overwrite/--update/--skip-existing flags (cut 8b)"
```

---

### Task 5: The copy path: `before_create`, `CopyStep::Claim`, single-file policy

**Files:**
- Modify: `crates/flux-core/src/copy.rs` (`CopyStep`, new `BeforeCreate` alias and `no_before_create`, `identity_gate`,
  `copy_file_guarded`, `copy_file_at`, `copy_file`, `prepare_file`)
- Modify: callers of `copy_file_guarded`: `crates/flux-core/src/tree.rs` (`copy_one`), `crates/flux-core/src/run/mod.rs:355`
  (`file`), and the test calls in `copy.rs` (about lines 1394-1560, seven calls: pass `&no_before_create`)
- Modify: `crates/flux-cli/src/report.rs` (`Report::file`, `Report::file_run`), `crates/flux-cli/src/exit_code.rs` if a match needs the field
- Test: `crates/flux-core/src/copy.rs` tests, `crates/flux-core/src/run/tests.rs`, `crates/flux-cli/tests/copy.rs`

**Interfaces:**
- Produces:
  ```rust
  pub type BeforeCreate<'g> = dyn Fn() -> std::result::Result<(), CopyError> + 'g;
  pub(crate) fn no_before_create() -> std::result::Result<(), CopyError> { Ok(()) }
  // CopyStep gains the variant  Claim  (doc: "Cut 8b: the claim made before the temporary is created").
  pub fn copy_file_guarded<F: DestinationRoot>(fs: &F, src: &Path, parent: &F::Dir, name: &OsStr, opts: &CopyOptions,
      guard: &Guard<'_>, beat: &Heartbeat<'_>, before_create: &BeforeCreate<'_>) -> Result<Outcome, CopyError>;
  pub(crate) struct GateRead { pub degraded: Option<FileIdentity>, pub existing: Option<Metadata> }
  // identity_gate now returns Result<GateRead, CopyError>; `existing` is the destination Metadata it read (None when absent).
  ```
  Order inside `copy_file_guarded` (Step numbers are the file's): Step 2a gate (unchanged refusals) -> NEW policy decision
  (below) -> open the source reader -> the existing `beat()`, `guard()` -> NEW `before_create()?` -> `create_new(&temp)`.
  `before_create`'s `Err` is returned as is (nothing created yet, so no leftover).
  Policy decision (single-file, from `opts.existing` and `GateRead.existing`): `Overwrite` or no existing destination:
  continue; `SkipExisting` with an existing destination that is not a directory: return `Ok(Outcome { skipped: true,
  bytes_copied: 0, metadata_failures: vec![], identity_degraded: gate.degraded, published_identity:
  FileIdentity::Unavailable })` before any temporary; `Update`: continue only when `src.modified > dest.modified` or
  `src.len != dest.len`; when either modification time is `None`, compare lengths only (equal: skip as above).
  `run::file` treats a skipped outcome like a published one except that it records no `published` identity
  (`Ended::Completed { published: None, .. }`).
  `Report::file`: `files_skipped = u64::from(o.skipped)`, `files_copied = u64::from(!o.skipped)`,
  `files_overwritten = u64::from(target_existed && !o.skipped)`, `bytes_skipped` stays 0 for a single file (the engine does
  not stat the source separately: leave a comment).

- [ ] **Step 1: Write the failing tests.**
  - `copy.rs` tests (fake; helpers `opts()`, `FaultFs`): `a_before_create_error_stops_the_copy_before_the_temporary_exists`
    (callback returns `Err(CopyError::at(CopyStep::Claim, ..))`: result is that error, `!fs.called("create_new")`, no
    `*.flux-partial.*` entry); `before_create_runs_after_the_gate_and_before_the_create` (call log: `metadata(dest)` before the
    callback's recorded call before `create_new`; the callback pushes a marker into the log through a captured `Cell`
    counter compared with `fs.calls().len()` at the time it runs); `the_identity_gate_refusal_skips_before_create` (dest is
    the source by identity: `before_create` never runs); `skip_existing_leaves_the_destination_untouched` (`Outcome.skipped`,
    `bytes_copied == 0`, dest content unchanged, `!fs.called("create_new")`); `skip_existing_with_no_destination_copies`;
    `update_replaces_only_when_the_source_is_newer_or_the_size_differs` (four cases: newer same size replaces; older same size
    skips; same time different size replaces; either mtime unavailable and equal size skips - use `FaultFs` times/`write_file`
    lengths and the fake's `set_times`-style hooks that exist, reading `fault_fs.rs` first for the exact setter).
  - `run/tests.rs`: `a_skipped_single_file_run_completes_and_records_no_published_identity` (`run_file` with
    `ExistingPolicy::SkipExisting` over an existing target: `r.stop.is_none()`, the record is removed, target content unchanged).
  - `report.rs` tests: `a_skipped_single_file_reports_one_skipped_and_none_copied`.
- [ ] **Step 2: Run them to verify they fail to compile**

Run: `cargo nextest run -p flux-core copy::tests && cargo nextest run -p flux-cli`
Expected: compile errors naming `BeforeCreate` / `Claim`.
- [ ] **Step 3: Implement** the signature change, the gate return type, the order above and the policy decision; update every
  caller (the tree passes `&no_before_create` for now; Task 10 replaces it).
- [ ] **Step 4: Run the gate**

Run: `just check`
Expected: 0 failed.
- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "feat: the copy path's before_create callback and the single-file existing-destination policy (cut 8b)"
```

---

### Task 6: `FaultFs` case-insensitive mode (test support)

**Files:**
- Modify: `crates/flux-core/src/fault_fs.rs` (`Inner`, the settings block near `set_no_replace_support`, the path-level
  `FileSystem` methods `metadata`, `create_new`, `rename_replace`, `rename_no_replace`, `remove_file`, `create_dir`, `read_dir`)
- Test: `crates/flux-core/src/fault_fs.rs` tests

**Interfaces:**
- Produces on `FaultFs`:
  ```rust
  /// Name lookup folds ASCII case (the fake's stand-in for NTFS/APFS/casefold): every path argument of `metadata`,
  /// `create_new`, `create_dir`, `rename_*` (both paths), `remove_file` and `read_dir` has each component replaced by the
  /// STORED spelling of an existing sibling that matches it case-insensitively. Default false.
  pub fn set_case_insensitive(&self, on: bool);
  /// With `set_case_insensitive(true)`: a `rename_replace` over an entry stored under another spelling makes the entry take
  /// the NEW spelling (Windows). Default false: the OLD spelling is kept (Linux and macOS, measured).
  pub fn set_replace_renames(&self, on: bool);
  ```
  `read_dir` always returns stored spellings. Without the mode nothing changes (every existing test stays as is).

- [ ] **Step 1: Write the failing tests** (fault_fs.rs tests): `a_lookup_by_another_case_finds_the_entry_with_the_same_identity`
  (insensitive: `metadata("/d/file.txt")` equals `metadata("/d/FILE.TXT")` identity; sensitive default: `NotFound`);
  `create_new_over_another_case_is_already_exists`; `rename_no_replace_onto_another_case_is_already_exists`;
  `rename_replace_over_another_case_keeps_the_old_spelling_by_default` (listing is `FILE.TXT`, content new);
  `rename_replace_renames_when_asked` (`set_replace_renames(true)`: listing is the new spelling);
  `read_dir_returns_stored_spellings`; `the_mode_is_off_by_default` (a case-sensitive fake still has two entries `a` and `A`).
- [ ] **Step 2: Run them to verify they fail to compile**

Run: `cargo nextest run -p flux-core fault_fs`
Expected: compile error naming `set_case_insensitive`.
- [ ] **Step 3: Implement** `Inner.case_insensitive: bool`, `Inner.replace_renames: bool` and a private
  `fn stored(g: &Inner, path: &Path) -> PathBuf` applied at the top of each listed method (before the `record` call so the
  call log shows the NORMALIZED path; document that in a comment). `move_object` for the renaming case re-keys to the
  requested spelling.
- [ ] **Step 4: Run the gate**

Run: `just check`
Expected: 0 failed (every existing test unchanged).
- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/fault_fs.rs
git commit -m "test: FaultFs case-insensitive and replace-renames modes (cut 8b)"
```

---

### Task 7: `NameIndex`: name resolution and the identity table

**Files:**
- Create: `crates/flux-core/src/names.rs`; Modify: `crates/flux-core/src/lib.rs` (`mod names;` crate-private)
- Test: `crates/flux-core/src/names.rs` `mod tests` (the fake, case-insensitive mode)

**Interfaces:**
- Consumes: `DirHandle::{read_dir, metadata}`, `FileIdentity`, `ObjectId`, `FluxPathKey` (Task 1), Task 6's modes.
- Produces (all `pub(crate)`):
  ```rust
  pub(crate) struct NameIndex { /* listing: BTreeSet<OsString>; table: Option<HashMap<ObjectId, Vec<OsString>>>;
                                  owners: HashMap<OsString, FluxPathKey> (spelling -> the target of this run that wrote it) */ }
  pub(crate) enum Resolved { Absent, Entry { stored: OsString, meta: Metadata } }
  pub(crate) enum NameError { Unresolvable }          // maps to DESTINATION_ERROR "cannot determine the stored name"
  impl NameIndex {
      /// A directory this run created: empty listing; `resolve` is not used for it (decision 6).
      pub(crate) fn for_new_dir() -> Self;
      /// A pre-existing directory: reads `dir.read_dir()` once.
      pub(crate) fn for_existing<D: DirHandle>(dir: &D) -> flux_fs::Result<Self>;
      pub(crate) fn resolve<D: DirHandle>(&mut self, dir: &D, planned: &OsStr) -> Result<Result<Resolved, NameError>, FsError>;
      /// After a target published: `stored` is the pre-publication stored name (`None` for a new file), `planned` the name
      /// published, `replaced: Option<ObjectId>` the replaced object's identity, `published` its new identity.
      pub(crate) fn record_publication(&mut self, target: &FluxPathKey, stored: Option<&OsStr>, planned: &OsStr,
                                       replaced: Option<ObjectId>, published: FileIdentity);
  }
  ```
  `resolve` rules (spec "Name resolution"): exact listing hit -> `metadata(planned)` -> `Entry { stored: planned, meta }`;
  otherwise `metadata(planned)`: `NotFound` -> `Absent`; `Ok(meta)` -> identity table (built lazily once: `metadata` of every
  listing entry, mapping each `Strong` identity to its spellings; run-recorded spellings are merged in) -> exactly one
  spelling -> `Entry { stored, meta }`; several spellings are ONE entry only when every one is in `owners` with the SAME
  target, else `Unresolvable`; none -> `Unresolvable`. A `Weak`/`Unavailable` identity of `meta` -> `Unresolvable`.
  `record_publication`: inserts `planned` (and `stored`) into the listing, sets `owners[...] = target` for both, removes
  `replaced` from the table, adds `published` (when `Strong`) mapped to every spelling written (`stored` and `planned`).

- [ ] **Step 1: Write the failing tests** (module tests, fake with `set_case_insensitive(true)`; build the directory by
  `FaultFs::create_dir` / `write_file`; `d = fs.destination_root("/d")`):
  `an_exact_name_resolves_to_itself`; `an_absent_name_is_absent`; `another_spelling_resolves_by_identity_to_the_stored_name`
  (`FILE.TXT` stored, planned `file.txt` -> `stored == "FILE.TXT"`); `the_table_is_built_once_per_directory` (call log: the
  `metadata` calls for the listing entries appear once across two resolutions); `a_hardlink_alias_in_the_listing_is_unresolvable`
  (two entries with equal identity via `set_identity`, planned a third spelling); `a_weak_identity_is_unresolvable`;
  `after_a_replace_the_new_object_resolves_through_both_spellings` (Windows-style: `record_publication(target, Some("FILE.TXT"),
  "File.txt", Some(old), Strong(new))`; then planned `file.txt` with the new identity resolves, and two spellings with the same
  owner are one entry); `two_spellings_of_different_targets_are_ambiguous`; `a_new_dirs_index_has_an_empty_listing_and_no_reads`
  (`for_new_dir` makes no `read_dir` call).
- [ ] **Step 2: Run them to verify they fail to compile**

Run: `cargo nextest run -p flux-core names`
Expected: compile error naming `NameIndex`.
- [ ] **Step 3: Implement** `names.rs` per the Interfaces block.
- [ ] **Step 4: Run the gate**

Run: `just check`
Expected: 0 failed.
- [ ] **Step 5: Commit**

```bash
git add crates/flux-core
git commit -m "feat: NameIndex, name resolution by listing and identity (cut 8b)"
```

---

### Task 8: Outcome counts, `ClaimNotRecorded` and the report

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`TreeOutcome`, `TreeFailureCause`, `FailureTally` and its `count`, `lib.rs` re-exports)
- Modify: `crates/flux-cli/src/report.rs` (`record_lines`, `Report::tree`, `tree_warning_lines` / `safety_warning_lines`), `crates/flux-cli/src/exit_code.rs` (tests)
- Test: `crates/flux-core/src/tree.rs` tests, `crates/flux-cli/src/report.rs` tests

**Interfaces:**
- Produces:
  ```rust
  // TreeOutcome gains:
  pub files_overwritten: u64, pub files_skipped: u64, pub bytes_skipped: u64, pub replace_degraded: Option<DegradedGroup>,
  // TreeFailureCause gains:
  ClaimNotRecorded(FsError),     // the file IS published and counted; its claim was not recorded
  // FailureTally gains:  pub claim_not_recorded: u64   (total() includes it; count() has an arm; NOT in files_failed)
  ```
  Rendering: `record_lines` for `ClaimNotRecorded(e)`: `"{CODE}: {path}: {source}; the file was published but its claim was not recorded"`
  (`CODE` = `e.code.as_str()`; for a directory flush failure the path is the directory, the empty path renders as `.`).
  `Report::tree`: `files_skipped = out.special_files_skipped + out.files_skipped`, `files_overwritten = out.files_overwritten`,
  `bytes_skipped = out.bytes_skipped`; `errors` already uses `failures.total()`. `safety_warning_lines` gains a third line for
  `replace_degraded`: `"warning: could not key claims in {count} directories (e.g. {example}); existing files there are reported as collisions - --safety=strict skips them instead"`,
  order in `tree_warning_lines`: identity, mount-root, containment, replace-degraded.

- [ ] **Step 1: Write the failing tests.** `tree.rs`: update `the_tally_counts_each_cause_in_its_own_field` to include
  `ClaimNotRecorded` (total 6; `claim_not_recorded == 1`). `report.rs`: `each_record_renders_its_code_first` gains a
  `ClaimNotRecorded` case (exact string above); `a_tree_report_counts_skipped_overwritten_and_skipped_bytes`
  (`TreeOutcome` with the three counts and a `special_files_skipped = 1`: `files_skipped == 1 + n`, `files_overwritten`,
  `bytes_skipped`); `both_safety_warnings_render_after_the_identity_ones` extended to three lines and the order above;
  `a_claim_not_recorded_failure_exits_1` in `exit_code.rs` (an `Ok(out)` with `failures.claim_not_recorded = 1` is `FAILED`).
- [ ] **Step 2: Run them to verify they fail to compile**

Run: `cargo nextest run -p flux-core tree::tests && cargo nextest run -p flux-cli`
Expected: compile errors, then failures.
- [ ] **Step 3: Implement** the fields, the cause, the exhaustive arms (the compiler lists every `match`), the tally counter and the report text.
- [ ] **Step 4: Run the gate**

Run: `just check`
Expected: 0 failed.
- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "feat: replacement counts, ClaimNotRecorded and the report fields (cut 8b)"
```

---

### Task 9: The workspace creates, passes and removes `state.db`

**Files:**
- Modify: `crates/flux-core/src/state.rs` (`pub const STATE_DB: &str = "state.db";` beside `PROBE`; `retire_workspace` removes it)
- Modify: `crates/flux-core/src/run/place.rs` (`TreePlace` fields `claims`, `durability`; `TreePlace::create`; `unwind_creating`; the probe-error path)
- Modify: `crates/flux-core/src/run/mod.rs` (`tree`: `TreePlace` construction, taking the store out, `Shared`; drop before `finish`)
- Modify: `crates/flux-core/src/tree.rs` (`Shared` gains `claims`; `run_tree` passes `None`)
- Test: `crates/flux-core/src/run/tests.rs`, `crates/flux-core/src/state.rs` tests

**Interfaces:**
- Produces:
  ```rust
  // Shared<'c, F> gains:
  pub(crate) claims: Option<&'c std::cell::RefCell<<F::Dir as DirHandle>::Claims>>,
  // TreePlace gains:
  pub(crate) claims: Option<<F::Dir as DirHandle>::Claims>,
  pub(crate) durability: Durability,       // set by run::tree from opts.durability
  ```
  `TreePlace::create` order: `begin_workspace`, the probe (unchanged), then `building.create_claim_store(STATE_DB, self.durability)`
  stored in `self.claims` (an error is `failed(RunStep::State, &shown_creating.join(STATE_DB), e)` after the same best-effort
  `unwind_creating` the probe-error arm does), then `publish_workspace`. `unwind_creating` and the probe-error path remove
  `STATE_DB` from `<id>.creating` first (the store is dropped first: set `self.claims = None`). `retire_workspace` removes
  `STATE_DB` beside the manifest and the probe names. `run::tree` takes `place.claims` out, wraps it in a `RefCell`, passes
  `Some(&cell)` in `Shared`, and drops the cell before `finish` (so the file closes before removal).

- [ ] **Step 1: Write the failing tests** (`run/tests.rs`, fake): `a_tree_run_creates_state_db_in_the_workspace_and_removes_it_with_it`
  (call log: `create_claim_store(.../<id>.creating/state.db)` after the probe's `rename_no_replace` and before the workspace's
  publishing `rename_no_replace`; after a clean run `!fs.exists("/p/dest/.flux")` and no `.removing`);
  `a_refused_probe_removes_nothing_it_did_not_make` (`set_no_replace_support(false)`: no `create_claim_store` call);
  `a_failed_state_db_creation_unwinds_and_names_the_file` (`fail("create_claim_store", Code::PermissionDenied)`: `RunError::Failed
  { step: State, path ending state.db }`, `<id>.creating`, control dirs and a DEST this run made are gone, lock released);
  `a_kept_workspace_keeps_state_db` (a leftover temporary as in `a_temporary_the_copy_could_not_remove_keeps_the_completed_state`:
  `<id>/state.db` still exists); `retire_removes_state_db` (`state.rs`: a workspace with `state.db` retires without
  `DirectoryNotEmpty`); `the_store_is_dropped_before_the_workspace_is_removed` (the fake store's drop is observable through a
  `FaultFs` flag `claim_stores_open() == 0` after `tree(...)` returns).
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core run::tests state::tests`
Expected: compile errors, then failures.
- [ ] **Step 3: Implement** as above.
- [ ] **Step 4: Run the gate**

Run: `just check`
Expected: 0 failed. The cut 8a tests that count `create_new` / `remove_file` call indices may shift again; update call-index
numbers only, never an assertion's meaning (list the shifted tests in the report).
- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "feat: the workspace creates, passes to the walk and removes state.db (cut 8b)"
```

---

### Task 10: The replacement protocol in the walk

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`Frame::Live`, `enter_dir`, `walk_into`'s `Dir` and `File` arms, `copy_one`)
- Test: `crates/flux-core/src/run/tests.rs` (through `run::tree` on the fake), `crates/flux-core/src/tree.rs` tests (lockless unchanged)

**Interfaces:**
- Consumes: Tasks 5 (`BeforeCreate`, `GateRead`), 6 (modes), 7 (`NameIndex`), 8 (counts), 9 (`Shared.claims`).
- Produces: `Frame::Live { dir, created, names: NameIndex, claim_parent: Option<ObjectId> }` (`claim_parent` is
  `Some(id)` when the directory's identity is `Strong(id)` and `Shared.claims` is `Some`, else `None`; the root frame's `names` is `for_existing` when DEST pre-existed else `for_new_dir`,
  built in `copy_tree_at`). `enter_dir` builds the child's index: `NameIndex::for_new_dir()` for a created directory,
  `NameIndex::for_existing(&child)` for a pre-existing one (a `read_dir` error is that subtree's `CreateDir` failure, like a failed
  `open_dir`). A weak-identity directory: `claim_parent = None`, `DegradedGroup::note(&mut out.replace_degraded, path)` under
  `Safety::Default`; under `Safety::Strict` the subtree is reported `SAFETY_REJECTED` and skipped (like the mount "cannot tell"
  strict arm).
  `copy_one` when the frame's `claim_parent` is `None`: exactly today's behaviour (`NoReplace`).
  Otherwise, with `P = name`, `S = fs.metadata(src)` (only when the destination exists), `T = FluxPathKey::from_relative(&path)`:
  1. created-directory frame: plan as new.
  2. else `names.resolve(dir, P)`: `Err(e)` -> `CreateDir`-style per-target `Copy` failure with `e`; `Unresolvable` -> `Copy`
     failure `DESTINATION_ERROR`; `Absent` -> plan as new.
  3. plan as new: `copy_file_guarded` with `publish = NoReplace`, `existing = Overwrite`, `before_create = no_before_create`;
     on `Ok`: `claims.insert_if_absent((D.identity, P), { T, Created })` (an `Err` or a returned foreign record ->
     `ClaimNotRecorded`, the file counted), `names.record_publication(T, None, P, None, o.published_identity)`.
  4. `Entry { stored: E, meta: M }`: directory -> `Copy` failure `DESTINATION_ERROR` ("the destination is a directory");
     file or symlink -> decide: `SkipExisting` -> `files_skipped += 1`, `bytes_skipped += S.len`, no claim; `Update` -> the same
     comparison as Task 5 against `M`; `Overwrite` -> replace.
  5. replace: `copy_file_guarded` with `publish = Replace`, `existing = Overwrite` and a `before_create` closure that does
     `claims.borrow_mut().insert_if_absent((D.identity, E), { T, Existing })`: `Present(r)` with `r.target != T` ->
     `Err(CopyError::at(CopyStep::Claim, FsError::new(Code::DestinationNamespaceCollision, ..)))`; an `Err(e)` ->
     `Err(CopyError::at(CopyStep::Claim, e))`. After `Ok`: `upgrade_own_claim((D.identity, E), &T)`, and when `P != E`
     `insert_if_absent((D.identity, P), { T, Created })`; any claim error or a foreign record there -> `ClaimNotRecorded`
     (file counted); `files_copied += 1`, `files_overwritten += 1`, `names.record_publication(T, Some(E), P, replaced, published)` where `replaced` is
     `Some(id)` when `M.identity` is `Strong(id)` and `None` otherwise.
  `copy_one`'s error mapping gains: `e.step == CopyStep::Claim` -> reported as `Copy(e)` (a collision keeps its code).

- [ ] **Step 1: Write the failing tests** (`run/tests.rs`; fixtures `fake()`, `run_tree`, `opts()`; destination `/p/dest` created
  with `fs.create_dir`; `existing(policy)` helper sets `CopyOptions.existing`):
  - `an_existing_file_is_replaced_by_default` (`/p/dest/a` = `old` -> `A`; `files_overwritten == 1`, `files_copied == 2`, no failures;
    claim `(dest identity, "a")` has status `Created`; `fs.claim_count() == 3`: `a` and `sub/b` plus... assert the exact count).
  - `the_claim_comes_after_the_gate_and_before_the_temporary` (call log order: `metadata(/p/dest/a)` ... `claim_insert(a)` ...
    `create_new(.../a.flux-partial.<id>)` ... `rename_replace` ... `claim_upgrade(a)`).
  - `skip_existing_skips_and_claims_nothing`; `update_replaces_only_a_newer_or_different_size_file`; `a_new_file_is_claimed_created`.
  - `two_source_names_folding_onto_one_existing_entry_collide` (case-insensitive fake: source `/src/File.txt` and `/src/file.txt`,
    destination `FILE.TXT`; one replaced, the other `DESTINATION_NAMESPACE_COLLISION`, the destination holds the FIRST source's content);
    the same with `set_replace_renames(true)` (Windows-style); and with the pre-existing entry stored lowercase.
  - `a_directory_in_the_way_fails_only_that_target`; `a_symlink_at_the_destination_is_replaced_as_a_link` (`add_symlink("/p/dest/a")`:
    after the run `/p/dest/a` is a regular file, the link target untouched); `a_read_only_destination_fails_that_target_with_permission_denied`
    (the fake's perms hook, reading how cut 4 tests set read-only).
  - `a_weak_parent_identity_degrades_to_no_replace` (`set_identity("/p/dest", Weak)`): existing file reported collision, no claims,
    `replace_degraded` counted; strict: subtree `SAFETY_REJECTED`.
  - `a_claim_store_error_before_the_rename_fails_that_target_only` (`fail("claim_insert", ..)`: nothing published for it, siblings copied);
    `a_claim_store_error_after_the_rename_is_claim_not_recorded_and_the_file_is_counted` (`fail_nth("claim_upgrade", 1, ..)`:
    `files_overwritten == 1`, `failures.claim_not_recorded == 1`, exit 1); `a_target_that_finds_its_own_claim_proceeds`
    (pre-seed the fake store with `(dest, "a") -> { T, Existing }`).
  - `the_lockless_copy_tree_still_reports_a_collision` stays green (`tree_std_fs.rs`).
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core run::tests`
Expected: failures.
- [ ] **Step 3: Implement** `Frame::Live` fields, `enter_dir`/`copy_tree_at` index construction, `copy_one` as above.
- [ ] **Step 4: Run the gate**

Run: `just check`
Expected: 0 failed. Update `crates/flux-cli/tests/copy.rs` `a_folder_merges_into_an_existing_folder_and_reports_each_collision`
(decision 8): the default run now exits 0 and replaces; add `a_folder_over_a_folder_with_skip_existing_reports_skipped_files`
(exit 0, `files_skipped` in `--json`, content unchanged).
- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "feat: a directory copy replaces existing files under claims, with skip and update (cut 8b)"
```

---

### Task 11: Claim syncing: the directory-end flush and the final sync

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`walk_into`'s `DirEnd` arm; `copy_tree_at` end)
- Modify: `crates/flux-core/src/run/mod.rs` (`tree`: the final sync before `finish`)
- Test: `crates/flux-core/src/run/tests.rs`

**Interfaces:**
- Consumes: Task 1's `ClaimStore::flush`, Task 9's `Shared.claims`, `TreeFailureCause::ClaimNotRecorded`.
- Produces: at a `Frame::Live` `DirEnd` (popped frame has `claim_parent: Some(_)`): `(cx.beat)()` then `(cx.guard)()` (failures abort like a
  directory creation: `CopyStep::Heartbeat` / `CopyStep::Create`), then `claims.borrow_mut().flush()`; an `Err` is reported
  `ClaimNotRecorded(e)` at the directory's relative path and the walk continues. A `Frame::Skipped` `DirEnd` does nothing.
  Final sync (in `run::tree`, after `copy_tree_at` returns, before `finish`, the store still open) runs only for
  `Ended::Failed` (heartbeat intact: `!locked.pulse.failed()`, and `owned(&locked.held, &locked.lock_shown)` is `Ok`; best
  effort, its error ignored) and `Ended::Completed` with non-empty `leftovers` (`beat` + `guard` first; an error is a streamed
  `ClaimNotRecorded` at the empty path, counted before `finish`). Never for a clean `Completed`, `RefusedUnchanged` or `Lost`.

- [ ] **Step 1: Write the failing tests** (fake call log keys `claim_flush`, `beat`/`guard` via `before_mutation` hook counts):
  `a_directory_end_flushes_after_the_heartbeat_and_the_guard` (source `/src/sub/b`: the log has `claim_flush` after the last claim of
  `sub` and the guard hook ran immediately before it); `a_skipped_directory_flushes_nothing`; `a_flush_error_is_claim_not_recorded_for_that_directory_and_the_walk_continues`
  (`fail_nth("claim_flush", 1, ..)`: failure path `sub`, `failures.claim_not_recorded == 1`, sibling directories copied);
  `a_lost_lock_aborts_at_the_directory_end_flush` (guard fails at that point: run is `Ended::Lost`, no further `claim_flush`);
  `a_clean_run_makes_no_final_sync` (the number of `claim_flush` calls equals the number of directories, root excluded);
  `a_kept_workspace_after_a_failure_gets_a_final_sync` (copy fails so `Ended::Failed`: one extra `claim_flush`, none when the
  heartbeat failed - reuse `fail_heartbeat`); `a_completed_run_with_leftovers_gets_a_final_sync_and_reports_its_failure`
  (the `a_temporary_the_copy_could_not_remove...` fixture plus `fail_nth("claim_flush", <last>, ..)`); `no_final_sync_after_a_refusal_or_a_lost_lock`.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core run::tests`
Expected: failures.
- [ ] **Step 3: Implement** the `DirEnd` hook and the final sync as above.
- [ ] **Step 4: Run the gate**

Run: `just check`
Expected: 0 failed.
- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "feat: guarded directory-end and final claim syncs (cut 8b)"
```

---

### Task 12: End-to-end CLI tests

**Files:**
- Modify: `crates/flux-cli/tests/copy.rs`, `crates/flux-cli/tests/run.rs`
- Test: the same

- [ ] **Step 1: Write the tests** (real filesystem, the built binary; fixtures `tree_in`):
  `a_folder_over_an_existing_folder_replaces_changed_files` (destination `a`=`old`; after the run `a` = `A`, exit 0, `--json`
  `files_overwritten == 1`, `files_copied == 2`); `skip_existing_keeps_the_destination` (exit 0, `files_skipped`, content `old`);
  `update_keeps_a_newer_destination_and_replaces_an_older_one` (set mtimes with `filetime`-free std: `File::set_modified`);
  `a_run_leaves_no_state_db_or_removing_directory` (after a clean run `!dest.join(".flux").exists()`);
  `two_policies_exit_2` (from Task 4, moved here if absent); `a_single_file_skip_existing_exits_0_and_counts_one_skipped`.
- [ ] **Step 2: Run them**

Run: `cargo nextest run -p flux-cli`
Expected: PASS (the behaviour exists after Tasks 4-11); any failure is a bug in an earlier task: report it, do not paper over it.
- [ ] **Step 3: Commit**

```bash
git add crates/flux-cli
git commit -m "test: end-to-end replacement, skip and update through the CLI (cut 8b)"
```

---

### Task 13: Real-system and crash tests

**Files:**
- Create: `crates/flux-core/tests/replace_std_fs.rs`
- Test: the same

**Interfaces:** consumes `flux_core::run::{tree, RunConfig}`, `flux_platform::StdFileSystem`, `flux_core::ids::new_id`, the
`cfg`/`opts` helpers copied from `crates/flux-core/tests/safety_std_fs.rs` (copy, do not share).

- [ ] **Step 1: Write the tests.**
  - `replacement_works_on_the_default_filesystem` (every OS): destination `a` = `old`, source `a` = `A`: after `run::tree` the file
    is `A`; `state.db` is gone; the claims were recorded (assert through a run that fails after the copy so the workspace is kept,
    or read the kept `state.db` of a run with a leftover; use the stall approach below instead if simpler).
  - `a_case_differing_source_name_over_an_existing_entry_on_a_case_insensitive_filesystem` (`#[cfg(any(windows, target_os =
    "macos"))]`): destination `FILE.TXT`, source `file.txt`; replaced, listing shows ONE entry; and the two-source fold case
    (`File.txt` + `file.txt` in the source, only on a source filesystem that holds both: skip with a stated reason on
    case-insensitive sources).
  - `a_vfat_loopback_destination_folds_case` (Linux): like `safety_std_fs.rs`'s bind-mount test: `sudo -n` mkfs.vfat image mounted
    with `-o loop,uid=,gid=`, the two-source fold case: second is a collision, the destination holds the first's content; skip
    locally without sudo or `mkfs.vfat`, FAIL when `CI` is set; always unmount in `Drop`.
  - `a_file_over_a_junction_at_the_destination_name` (Windows): `mklink /J` then replace a file over it: record the outcome; never
    write through the target (the junction's target directory is unchanged); no skip (see the cut 8a junction test).
  - `a_killed_run_leaves_the_claims_up_to_the_last_sync` (all OS, CLI binary with the stall hook, in
    `crates/flux-cli/tests/run.rs` next to `a_killed_run_leaves_a_resumable_operation_that_restart_supersedes`): copy 300 files over
    an existing tree with `FLUX_TEST_STALL_AT=<k>`, kill, open the kept `DEST/.flux/operations/<id>/state.db` with `redb` read-only
    (`flux-cli` gains `redb` as a `[dev-dependencies]` entry via the workspace), and assert: the file opens; every record decodes (`ClaimRecord::decode` is
    `Some`) with a status of `Existing` or `Created`; the claim count is at least 1 and at most the number of files; and no key
    is repeated.
- [ ] **Step 2: Run them**

Run: `cargo nextest run -p flux-core --test replace_std_fs && cargo nextest run -p flux-cli --test run`, then push and read the three CI legs.
Expected: PASS on Linux; green on macOS and Windows in CI.
- [ ] **Step 3: Commit**

```bash
git add crates
git commit -m "test: replacement on real filesystems, vfat folding, a junction, and a killed run's claims (cut 8b)"
```

---

### Task 14: Docs and debt

**Files:** `TODO.md` (close "A tree copy never replaces an existing destination file", add the 241.5 clarification entry and the
unsynced-window limit), `crates/flux-cli/src/main.rs` (help text final check), the spec (`Status:` line, "Design record" gets an
"Implemented" bullet naming the commits), `README` section on flags if present (`rg -n "skip-existing|overwrite" README.md docs`).

- [ ] **Step 1: Edit** as listed (follow the file's own convention for resolved entries; read the nearest `[x]` entry).
- [ ] **Step 2: Run the gate**

Run: `just check`
Expected: 0 failed (typos covers the Markdown).
- [ ] **Step 3: Commit**

```bash
git add TODO.md crates/flux-cli docs
git commit -m "docs: cut 8b closes the replacement debt; the section 241.5 clarifications are filed (cut 8b)"
```

---

## Self-review (run before the handoff)

- **Spec coverage:** R (Tasks 5, 10), C (Tasks 1-3, 9-11), N (Tasks 6, 7, 10), F (Tasks 4, 5, 8, 12); claim model and interfaces
  (Task 1); store, cap and acceptance (Task 2); handle creation (Task 3); claim syncing (Tasks 2, 11); lifecycle and cleanup (Task 9);
  protocol steps 1-7 (Task 10); `ClaimNotRecorded` and report fields (Task 8); weak parent (Task 10); crash table rows (Tasks 11, 13);
  testing section (Tasks 1, 2, 6, 10, 13); known limits and out of scope: no task, by design.
- **Open items with an owner:** the `rename_replace`-over-a-junction outcome on Windows is measured by Task 13, not decided here;
  `redb` on a 9p mount is unmeasured (spec known limit 3).
- **Type consistency:** `ClaimKey::new`/`encode`, `ClaimRecord::encode`/`decode`, `FluxPathKey::from_relative`, `NameIndex::{for_new_dir,
  for_existing, resolve, record_publication}`, `BeforeCreate`, `GateRead`, `TreeFailureCause::ClaimNotRecorded`,
  `FailureTally::claim_not_recorded`, `TreePlace::{claims, durability}`, `Shared::claims` are used with the same names in every task.

End of the cut 8b implementation plan.
