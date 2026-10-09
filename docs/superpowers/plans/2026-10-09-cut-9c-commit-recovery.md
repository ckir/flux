# Cut 9c: recoverable publication under strict durability - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Under `--durability=strict`, a tree publication leaves a durable `prepared` note before its rename and turns it into the claim in the same
transaction after it, so `--resume` can tell from evidence whether an interrupted publication happened; everything else (Normal durability, single
files) behaves exactly as cut 9a.

**Architecture:**
- **Store:** the claim store (`state.db`, redb) goes to `meta.format` 2 with a second table `prepared` (`PreparedRecord`), written and removed in the
  same transaction as the claim row; format-1 stores stay readable and are never upgraded in place.
- **Publication:** `copy_file_guarded` gains a `BeforePublish` hook called once per copy after the source recheck and before the final heartbeat and
  guard; under Strict the tree's hook writes the note (one synced commit), the post-rename claim write becomes `commit_prepared` (one synced commit),
  and any failure after the note calls `discard_prepared`.
- **Recovery:** `TreePlace::adopt` runs `run/recover.rs` before the walk: phase 1 classifies every note by the spec's matrix (read-only, link-refusing
  handles), phase 2 applies every verdict in ONE synced transaction (`apply_recovery`) or refuses the adoption with `COMMIT_STATE_UNCERTAIN` (exit 3,
  nothing changed).
- **Reporting:** `ResumeNote::Adopted` carries `recovered`, printed as `recovered <k> interrupted publications`; the V16 spec is amended (appendix of the
  spec) and `TODO.md` records the limits.

**Tech Stack:** Rust 2024 (MSRV 1.98.1), `redb` 4.3.x (two tables in one file, `Durability::Immediate` for every note commit), the `FaultFs` fake,
nextest via `just`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-09-cut-9c-commit-recovery-design.md` (approved 2026-10-09 at `53a73f5`). Executors read it with this plan;
where the two differ the spec wins and the executor reports the conflict.

## Global Constraints

- Rust edition 2024; `rust-version = "1.98.1"` (`Cargo.toml:11`). The gate is `just check` = `cargo fmt --check` +
  `cargo clippy --workspace --all-targets -- -D warnings` + `typos` + `cargo nextest run --workspace --no-tests=pass` +
  `cargo test --doc --workspace`. Push after every task and READ CI on that task's commit (Windows-only failures surface only there; never infer green
  from an older commit).
- Notes are written ONLY under `Durability::Strict` and ONLY by tree publications (spec decisions 1 and 2). Under Normal the call log of a copy must show
  no `claim_prepare` call; a format-1 store never receives a note (`supports_prepared()` false).
- `prepare`, `commit_prepared` and `apply_recovery` commits are always `Immediate` (spec "Units"). `commit_prepared` is one transaction: claim and note
  change together or not at all.
- Recovery phase 1 touches the filesystem only through link-refusing `DirHandle`s opened component by component (the `artifacts::remove_validated`
  pattern, `crates/flux-core/src/cleanup/artifacts.rs`), reads only, and validates every note before any filesystem access (spec decision 5).
- An UNCERTAIN note refuses the whole adoption before any mutation: `LockCode::CommitStateUncertain`, exit 3, `changed: false`. Phase 2 never runs
  when any note is UNCERTAIN.
- The copy's report, its 18 JSON keys (`crates/flux-cli/src/report.rs:401`, `the_json_has_every_section_53_field_in_order`) and its exit codes are
  unchanged except for the new refusal; the resume note gains one line.
- Message texts are the spec's: the refusal detail `<path>: cannot tell whether this file was published (<evidence>); the operation's state is
  preserved. Move or delete the destination ENTRY you do not want (not the temporary file) so it reads as absent (the next resume then redoes the
  file), or run again with --restart to supersede this operation`; the report line `recovered <k> interrupted publications`. Tests pin them by
  substring.
- Every new test goes red under a one-line mutant of the code it guards (the implementer names and RUNS the mutant; the controller measures a sample).
  Tests that read the fake's call log go through the file's separator-normalising `calls()` helper (Windows); unix-only setups are `#[cfg(unix)]`.
- Commit messages: `feat:` / `test:` / `docs:` prefixes, one commit per task. Every reviewer and implementer dispatch forbids `git checkout`,
  `git switch`, `git stash`, `git reset`.

## Decisions this plan makes (the spec's "Left to the plan")

1. **The hook is a ninth parameter** of `copy_file_guarded` (`before_publish: &BeforePublish<'_>`, after `before_create`); the function already carries
   `#[allow(clippy::too_many_arguments)]`. The lockless `copy_file_at` and every existing caller pass `&no_before_publish`.
2. **Conformance split:** the new cases live in `flux_fs::claims::conformance::run_prepared_all`, separate from `run_all`, so Task 1 compiles with stubbed
   implementors and Tasks 2 and 3 switch each implementor on.
3. **Stray temp-name removal failure in recovery row 1** is reported as `RunWarning::PartialKept { path: <temp path>, error, kept: <manifest path> }`
   (the existing shape, as the spec says); the note is removed regardless.
4. **A store error in phase 2** maps through `place.rs::state_error(&db, e)` (refusal for `StateCorrupt`/`IncompatibleState`, `Failed` at
   `RunStep::State` otherwise), exactly like `count()`.
5. **Fault keys of the fake:** `claim_prepare`, `claim_commit_prepared`, `claim_discard_prepared`, `claim_apply_recovery`, `claim_prepared` (the read),
   each recorded as `<key>(<name>)`.
6. **`PreparedRecord.identity` and `dir_path` are strings on the wire** (`identity_text` / `native_hex`), decoded with `parse_identity` / `from_native_hex`
   (`crates/flux-core/src/state.rs:591-622, 629-664`). Because `flux-fs` cannot depend on `flux-core`, the record keeps them as `String` fields and the
   core does the parsing.
7. **Test fixtures for notes** are written through the store API (`prepare`) on the fake and through redb directly (`TableDefinition::new("prepared")`)
   on the real filesystem, never by hand-encoding bytes except in the codec tests.
8. **The e2e kill point for "note written, rename not done"** is a file's publish guard under Strict (the hook precedes that guard), measured as in
   `crates/flux-cli/tests/run.rs:700-731` (three guarded mutations per file: sweep, create, publish).

## Review Focus

1. A format-1 store (a 9a/9b operation) resumed by this binary under Strict: no note is written and the run behaves as 9a (Task 5,
   `a_format_1_store_under_strict_writes_no_note`).
2. A note whose directory identity no longer equals the key's parent (the directory was replaced): UNCERTAIN, never RENAMED, nothing touched (Task 6,
   `a_note_whose_directory_identity_changed_is_uncertain`).
3. A replacement crash after the rename on a case-folding destination (`planned_name != name`): recovery writes BOTH claims (Task 6,
   `recovery_of_a_renamed_replacement_writes_the_planned_claim_too`).
4. `prepare` failing (the store refuses) under Strict: that file fails before its rename with `CopyStep::Claim`, nothing is published, no note remains,
   the walk continues (Task 5, `a_prepare_failure_fails_the_file_before_the_rename`).
5. One RENAMED and one UNCERTAIN note: the store is byte-identical after the refusal and the exit code is 3 (Task 6,
   `one_uncertain_note_refuses_the_adoption_and_changes_nothing`; Task 7 end to end).

---

### Task 1: `PreparedRecord`, `RecoveryOp` and the `ClaimStore` surface (`flux-fs`)

**Files:**
- Modify: `crates/flux-fs/src/claims.rs:68-120` (types and the trait), `:123-137` (`conformance`: new `run_prepared_all`), `:300-340` (codec tests)
- Modify: `crates/flux-fs/src/lib.rs:20` (exports)
- Modify (stubs only, replaced in Tasks 2 and 3): `crates/flux-platform/src/claims.rs:160-230` (`impl ClaimStore for RedbClaimStore`),
  `crates/flux-core/src/fault_fs.rs:158-219` (`impl ClaimStore for FakeClaimStore`)
- Test: `crates/flux-fs/src/claims.rs`

**Interfaces:**
- Produces:
  ```rust
  // claims.rs
  /// A publication's note (cut 9c, spec decision 5), keyed like the claim it becomes.
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub struct PreparedRecord {
      pub target: FluxPathKey,
      pub temp_name: Vec<u8>,
      /// `identity_text` of the temporary's identity, parsed by the core.
      pub identity: String,
      /// `native_hex` of the entry's directory relative to DEST; empty for DEST itself.
      pub dir_path: String,
      pub name: Vec<u8>,
      /// Empty when equal to `name`.
      pub planned_name: Vec<u8>,
      pub replacement: bool,
  }
  impl PreparedRecord {
      /// Version byte 1; then target, temp_name, identity, dir_path, name, planned_name each as u32 BE length + bytes; then replacement (0/1).
      pub fn encode(&self) -> Vec<u8>;
      /// `None` for another version, a short or over-long field, trailing bytes, or a replacement byte other than 0/1.
      pub fn decode(bytes: &[u8]) -> Option<Self>;
  }
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub enum RecoveryOp {
      Commit { key: ClaimKey, target: FluxPathKey, planned: Option<ClaimKey> },
      Discard { key: ClaimKey },
  }
  pub trait ClaimStore { /* existing five methods unchanged */
      /// False for a store whose format has no `prepared` table (format 1).
      fn supports_prepared(&self) -> bool;
      /// Inserts the note; a note already at `key` is `Code::IoError`. Always an Immediate commit.
      fn prepare(&mut self, key: &ClaimKey, record: &PreparedRecord) -> Result<()>;
      /// ONE transaction: the claim at `key` becomes `Created` for `target` (inserted if absent, upgraded if `Existing` of the same target);
      /// when `planned` is given a `Created` claim for it is inserted (an existing claim there owned by the same target is accepted); the note at
      /// `key` is deleted. A claim owned by another target at `key` or `planned` is `Code::IoError` and nothing changes. Always Immediate.
      fn commit_prepared(&mut self, key: &ClaimKey, target: &FluxPathKey, planned: Option<&ClaimKey>) -> Result<()>;
      /// Removes the note; a key with no note is `Ok` (idempotent). Always Immediate.
      fn discard_prepared(&mut self, key: &ClaimKey) -> Result<()>;
      /// Every note, in the byte order of `ClaimKey::encode` (redb's own order; the fake sorts its map the same way).
      fn prepared(&self) -> Result<Vec<(ClaimKey, PreparedRecord)>>;
      /// Every `op` in ONE Immediate transaction, each with `commit_prepared`'s / `discard_prepared`'s rules; nothing changes when any errors.
      fn apply_recovery(&mut self, ops: &[RecoveryOp]) -> Result<()>;
  }
  pub mod conformance { pub fn run_prepared_all<S: ClaimStore>(new_store: impl Fn() -> S); /* calls the cases below; run_all is UNCHANGED */ }
  ```
- Consumes: nothing new.

- [ ] **Step 1: Write the failing tests**
  - Codec tests in `claims.rs` `mod tests`: `prepared_record_round_trips` (every field non-empty; empty `planned_name` and empty `dir_path` too);
    `prepared_record_rejects_a_foreign_version_truncation_trailing_bytes_and_a_bad_flag` (version byte 2 -> None; a length larger than the rest
    -> None; one trailing byte -> None; replacement byte 2 -> None; empty input -> None); `prepared_record_layout_is_the_specs` (the encoded bytes of
    a small record equal a hand-written `vec![1, 0,0,0,1, b'a', ...]`).
  - Conformance cases in `run_prepared_all` (each a `pub fn <name><S: ClaimStore>(mut s: S)` with the `key`/`rec` helpers; `note(target)` builds a
    `PreparedRecord` with `temp_name = b"n.flux-partial.x"`, `identity = "strong:1:9"`, `dir_path = ""`, `name = b"n"`, empty planned, `replacement
    = false`): `supports_prepared_is_true_for_a_fresh_store`; `prepare_then_prepared_lists_the_note_and_a_second_prepare_is_an_error`;
    `commit_prepared_writes_a_created_claim_and_removes_the_note` (claim absent before: afterwards `get` is `Created` for the target, `prepared()` is
    empty); `commit_prepared_upgrades_an_own_existing_claim` (insert `Existing` first); `commit_prepared_with_a_planned_key_inserts_the_second_claim_and_accepts_its_own_repeat`
    (call twice with the same planned key: second is `Ok`, still exactly two claims); `commit_prepared_of_a_foreign_claim_changes_nothing` (foreign
    claim at `key`, or at `planned`: `Err`, note still listed, claims unchanged); `discard_prepared_is_idempotent`; `apply_recovery_applies_every_op_or_none`
    (two notes; ops `[Commit for the first, Discard for the second]` -> both applied; then ops `[Commit for a fresh note, Commit whose key holds a
    foreign claim]` -> `Err`, the fresh note still listed, no claim added); `notes_and_claims_share_keys_but_not_tables` (a claim at `k` and a note at
    `k` coexist until commit).
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-fs`
Expected: compile errors naming `PreparedRecord`, `prepare`.

- [ ] **Step 3: Implement** the types, codec, trait methods, `run_prepared_all` and the derives. Stub the six methods in `RedbClaimStore` and
  `FakeClaimStore` as `Err(FsError::new(Code::IoError, io::Error::from(ErrorKind::Unsupported)))` (`supports_prepared` -> `false`, `prepared` ->
  `Ok(Vec::new())`) so the workspace compiles; do NOT add `run_prepared_all` to any caller yet.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-fs crates/flux-platform/src/claims.rs crates/flux-core/src/fault_fs.rs
git commit -m "feat: PreparedRecord, RecoveryOp and the prepared-note surface of ClaimStore (cut 9c)"
```

---

### Task 2: Claim-store format 2 and the `prepared` table (`flux-platform`)

**Files:**
- Modify: `crates/flux-platform/src/claims.rs:18-24` (constants), `:76-125` (`create_file`/`open_file` format check), `:160-230` (the impl), tests
- Modify: `crates/flux-platform/tests/claims_store.rs` (conformance + format tests), `crates/flux-platform/tests/claims_kill.rs` (one kill case)
- Test: those two files

**Interfaces:**
- Produces:
  ```rust
  pub const FORMAT: u64 = 2;            // written by create_file
  const PREPARED: TableDefinition<&[u8], &[u8]> = TableDefinition::new("prepared");  // key ClaimKey::encode, value PreparedRecord::encode
  impl RedbClaimStore { pub fn format(&self) -> u64 }   // 1 or 2, read at open
  ```
  `open_file` accepts `format` 1 or 2 (anything else `IncompatibleState` as today); a format-2 store must have both tables (a missing `prepared`
  table is `StateCorrupt`); `supports_prepared()` is `format == 2`; on a format-1 store the five note methods return `Code::IncompatibleState`
  ("the claim store has no prepared table") and `prepared()` returns an empty list. `prepare`/`commit_prepared`/`discard_prepared`/`apply_recovery`
  set `redb::Durability::Immediate` and do not touch `unsynced`.
- Consumes: Task 1.

- [ ] **Step 1: Write the failing tests**
  - `claims_store.rs`: `real_store_passes_the_prepared_conformance_suite_normal` and `_strict` (`conformance::run_prepared_all`); `a_fresh_store_is_format_2`;
    `a_format_1_store_opens_with_supports_prepared_false_and_refuses_notes` (build a store with redb directly: tables `claims` and `meta`
    `format=1`, open with `open_file`: `format() == 1`, `supports_prepared()` false, `prepare` is `Err` with `Code::IncompatibleState`, `prepared()`
    empty, `insert_if_absent` still works, and after dropping the store `meta.format` is STILL 1 and no `prepared` table exists: never upgraded in
    place); `a_format_3_store_is_refused` (meta `format=3` -> `IncompatibleState`, existing behaviour kept);
    `a_format_2_store_without_a_prepared_table_is_corrupt`; `note_commits_are_immediate_under_normal` (Normal store: 5 `prepare` calls then
    `unsynced() == 0`, and after one `insert_if_absent` it is 1).
  - `claims_kill.rs`: `a_killed_writer_keeps_every_prepared_note_under_normal` (the pattern of `killed_mid_stream_the_store_reopens_with_a_prefix`
    (`:61`): the child writes notes with `prepare` under Normal, commits every other one with `commit_prepared`, and is killed; the parent reopens and for
    every key the child reported: either the note is listed and the claim is absent, or the note is absent and the claim is `Created`; never both,
    never neither (a note is Immediate, a commit is one transaction)).
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-platform`
Expected: the new tests fail (`Unsupported`, format 1).

- [ ] **Step 3: Implement** the table, the format logic, the five methods (`commit_prepared` and `apply_recovery` share one private
  `fn apply_in(tx: &WriteTransaction, op: &RecoveryOp) -> Result<()>`).
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-platform
git commit -m "feat: claim store format 2 with the prepared table; note commits are immediate (cut 9c)"
```

---

### Task 3: The fake's notes and formats (`flux-core` `fault_fs.rs`)

**Files:**
- Modify: `crates/flux-core/src/fault_fs.rs:118-126` (`Inner`: a `prepared` map per store; `claim_formats` default), `:130-219` (`FakeClaimStore`),
  `:684-690` (`set_claim_store_format`), `:1488-1560` (`create_claim_store` sets format 2; `open_claim_store` accepts 1 and 2), tests at `:2558-2600`
- Test: `crates/flux-core/src/fault_fs.rs`

**Interfaces:**
- Produces: `FakeClaimStore` implements the six methods with the fault keys of decision 5 (`claim_prepare(<name>)`, `claim_commit_prepared(<name>)`,
  `claim_discard_prepared(<name>)`, `claim_prepared`, `claim_apply_recovery`), `supports_prepared()` = the store's format is 2; a store created by
  `create_claim_store` is format 2; `set_claim_store_format(path, 1)` makes it format 1 (notes refused `IncompatibleState`, `prepared()` empty);
  `FaultFs::prepared_count(&self) -> usize` (every store's notes, like `claim_count`). `apply_recovery` is all-or-nothing on the map (apply to a clone,
  swap on success).
- Consumes: Task 1.

- [ ] **Step 1: Write the failing tests** (in `fault_fs.rs` `mod tests`): `the_fake_store_passes_the_prepared_conformance_suite`
  (`conformance::run_prepared_all` over `create_claim_store`); `a_format_1_fake_store_refuses_notes`; `prepare_records_its_call_and_honours_a_fault`
  (`fs.fail("claim_prepare", Code::IoError)` -> `prepare` errs, no note; call log has `claim_prepare(n)`); `apply_recovery_on_the_fake_is_all_or_nothing`.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core fault_fs::`
Expected: fail on `Unsupported` / format.

- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed; every 9a/9b test still passes (`open_claim_store` now accepts format 2 as the default).

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/fault_fs.rs
git commit -m "feat: the fake claim store's prepared notes, formats and fault keys (cut 9c)"
```

---

### Task 4: The `BeforePublish` hook in `copy_file_guarded` (`flux-core` `copy.rs`)

**Files:**
- Modify: `crates/flux-core/src/copy.rs:59-68` (types beside `BeforeCreate`), `:426-434` (`copy_file_at` passes `&no_before_publish`), `:451-470`
  (doc + signature), `:600-650` (the call site: after the source recheck, before the final `beat()`/`guard()`), tests `:1725-1830`
- Modify (callers): `crates/flux-core/src/tree.rs` (two `copy_file_guarded` calls, pass `&no_before_publish` for now), `crates/flux-core/src/run/mod.rs`
  (the single-file call, `&no_before_publish`), every test caller in `copy.rs` and `tree.rs` tests
- Test: `crates/flux-core/src/copy.rs`

**Interfaces:**
- Produces:
  ```rust
  /// Cut 9c: the object about to be published, as `copy_file_guarded` hands it to `before_publish`.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct PublishIntent { pub temp: std::ffi::OsString, pub identity: FileIdentity }
  /// Cut 9c: runs once per copy that reaches the publish, after the source recheck and BEFORE the final heartbeat and guard.
  /// Its `Err` stops the copy there: the temporary is discarded (as any pre-rename failure) and the error is returned as is.
  pub type BeforePublish<'g> = dyn Fn(&PublishIntent) -> std::result::Result<(), CopyError> + 'g;
  pub(crate) fn no_before_publish(_: &PublishIntent) -> std::result::Result<(), CopyError> { Ok(()) }
  pub fn copy_file_guarded<F: DestinationRoot>(fs, src, parent, name, opts, guard, beat, before_create, before_publish: &BeforePublish<'_>) -> Result<Outcome, CopyError>;
  ```
  `published_identity` (today read at `:643`) moves BEFORE the hook call and is what `PublishIntent.identity` carries; the same value is still
  returned in `Outcome`.
- Consumes: nothing new.

- [ ] **Step 1: Write the failing tests** (pattern: `guarded_with` at `:1727`, extended with a `before_publish` parameter; keep the existing tests
  passing with `&no_before_publish`): `before_publish_runs_after_the_recheck_and_before_the_final_guard_with_the_temps_identity` (a counting hook that
  records the intent: called once; `intent.temp == "dst.flux-partial.op1"`; `intent.identity` equals the identity the fake minted for the temporary;
  in the call log the hook's own marker (write a file from inside the hook, e.g. `fs.write_file("/marker")`) sits after the recheck's `metadata(/src)`
  and before the last `metadata(` of the guard); `a_before_publish_error_discards_the_temporary_and_publishes_nothing` (error `CopyStep::Claim` ->
  returned as is, `/dst` absent, `/dst.flux-partial.op1` absent, `leftover` none); `before_publish_is_not_called_when_the_recheck_fails` (source
  changed -> hook count 0); `the_lockless_copy_never_calls_before_publish` (`copy_file_at`).
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core copy::`
Expected: compile errors naming `before_publish`.

- [ ] **Step 3: Implement**; update every caller.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed; `before_create_is_not_called_when_the_guard_or_heartbeat_fails` and the 9a/9b suites unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src
git commit -m "feat: copy_file_guarded's before_publish hook carries the temporary's identity (cut 9c)"
```

---

### Task 5: Strict tree publications write, commit and discard notes (`flux-core` `tree.rs`)

**Files:**
- Modify: `crates/flux-core/src/tree.rs:955-993` (the `Plan::New` branch), `:994-1066` (the `Plan::Replace` branch), `:1068-1078` (`record_claim`
  stays for the Normal path)
- Test: `crates/flux-core/src/run/tests.rs` (the resume/claim fixtures at `:2612-2700` and `:3285-3350`: `prior3_with`, `prior_store`, `claimed`,
  `resumable_with`; `cfg()`/`opts()` at `:24-50`)

**Interfaces:**
- Consumes: Tasks 3 and 4; `cx.opts.durability` (`Shared.opts`, `tree.rs:319`); `cx.claims` (`:325`); `native_hex` (`state.rs:629`); `identity_text`
  (`state.rs:591`).
- Produces (behaviour, no new public items): when `cx.opts.durability == Durability::Strict` AND `claims.supports_prepared()`:
  - `Plan::New`: `before_publish` = a hook that calls `prepare(&ClaimKey::new(parent_id, name), &PreparedRecord { target, temp_name: intent.temp,
    identity: identity_text(intent.identity), dir_path: native_hex(path.parent()) ("" for the root), name, planned_name: empty, replacement: false })`,
    mapping an error to `CopyError::at(CopyStep::Claim, e)`; after a successful copy `commit_prepared(&key, &target, None)` replaces today's
    `record_claim`; a failed `commit_prepared` is reported as `TreeFailureCause::ClaimNotRecorded(e)` (as today) and the note stays; a failed copy whose
    error step is `Publish`, `Heartbeat` or the guard's `TargetLockBusy` after a successful hook calls `discard_prepared(&key)` best effort (its error
    is ignored: the matrix is the backstop) - implement by remembering in a `Cell<bool>` whether the hook succeeded.
  - `Plan::Replace`: `before_create` unchanged (the `Existing` claim); the hook writes the note at `key_stored` with `name: stored`, `planned_name:
    name if name != stored else empty`, `replacement: true`; after success `commit_prepared(&key_stored, &target, (name != stored).then(|| ClaimKey::new(parent_id, name)).as_ref())`
    replaces today's `upgrade_own_claim` + `record_claim` pair; discard on failure as above.
  - Otherwise (Normal, or a format-1 store): exactly today's code paths, `&no_before_publish`.
- Produces for tests: none beyond the call log.

- [ ] **Step 1: Write the failing tests** in `run/tests.rs` (a Strict config: `CopyOptions { durability: Durability::Strict, ..opts() }` passed through
  `tree(..)`; the fake's claim store is format 2 by default after Task 3):
  - `a_strict_publication_prepares_before_the_rename_and_commits_after_it`: one new file; the call log (via `calls()`) has `claim_prepare(a)` BEFORE
    `rename_no_replace(` of `a` and `claim_commit_prepared(a)` after it; `fs.prepared_count() == 0` at the end; the claim is `Created`.
  - `a_normal_publication_writes_no_note`: same tree under Normal: no `claim_prepare(` in the log, the claim path is today's (`claim_insert(a)`).
  - `a_format_1_store_under_strict_writes_no_note` (Review Focus 1): `fs.set_claim_store_format(<store path>, 1)` on an adopted prior
    (`resumable_with` + `--resume` under Strict): no `claim_prepare(`, run `ok`.
  - `a_prepare_failure_fails_the_file_before_the_rename` (Review Focus 4): `fs.fail("claim_prepare", Code::IoError)`: the file's failure has
    `CopyStep::Claim`, no `rename_no_replace(` for it, no temporary left, `prepared_count() == 0`, the other file of the tree is still copied.
  - `a_commit_failure_keeps_the_note_and_reports_claim_not_recorded`: `fs.fail("claim_commit_prepared", Code::IoError)`: the file IS published, a
    `ClaimNotRecorded` failure is reported, `prepared_count() == 1`.
  - `a_publish_failure_after_the_note_discards_it`: `fs.fail_nth("rename_no_replace", <n>, Code::IoError, ..)` where `<n>` is the file's publish
    rename (the workspace publish and the no-replace probe use `rename_no_replace` first; measure `<n>` from a green run's call log and state it in
    a comment): the log has `claim_prepare(a)` then `claim_discard_prepared(a)`, `prepared_count() == 0`, the file is reported failed at
    `CopyStep::Publish`.
  - `a_lost_lock_after_the_note_discards_it_and_keeps_the_temporary`: make the publish guard fail after the note (`fs.repoint_for_test` on the lock,
    the idiom of `restart_stops_when_ownership_is_lost_and_deletes_nothing_after` at `:472`, armed from `on_nth("claim_prepare", 1, ..)`): the copy
    fails with `TargetLockBusy` and a `leftover`, `claim_discard_prepared(a)` is in the log, the temporary is still on disk (the guard forbids its
    removal), the run stops as today.
  - `a_strict_replacement_commits_the_planned_claim_too`: `fs.set_case_insensitive(true)` and a destination entry stored as `A` for a source `a`
    (see `a_resumed_skip_under_a_different_stored_spelling_registers_the_owner` at `:3674` for the fixture): after the run both `A` and `a` have
    `Created` claims for the target and `prepared_count() == 0`.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core run::tests::a_strict run::tests::a_normal_publication run::tests::a_format_1 run::tests::a_prepare run::tests::a_commit run::tests::a_publish_failure`
Expected: FAIL (no `claim_prepare` call).

- [ ] **Step 3: Implement** the two branches.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src
git commit -m "feat: strict tree publications leave a prepared note and commit it with the claim (cut 9c)"
```

---

### Task 6: Commit recovery at `--resume` (`flux-core` `run/recover.rs`, `place.rs`, `session.rs`, `mod.rs`, `lock/error.rs`; CLI report)

**Files:**
- Create: `crates/flux-core/src/run/recover.rs`
- Modify: `crates/flux-core/src/run/mod.rs:11-16` (`mod recover;`), `:99-104` (`ResumeNote::Adopted` gains `recovered: u64`)
- Modify: `crates/flux-core/src/run/place.rs:556-610` (`TreePlace::adopt` runs recovery after `count()`; its return becomes `Result<Option<Adopted>, RunError>`
  with `pub(crate) struct AdoptedStore { pub claims: u64, pub recovered: u64 }`, `FilePlace::adopt` returns `Ok(None)` as today)
- Modify: `crates/flux-core/src/run/session.rs:168-185` (the call and the note)
- Modify: `crates/flux-core/src/lock/error.rs:8-21` (`CommitStateUncertain`, `"COMMIT_STATE_UNCERTAIN"`), `:80-94` (the string test)
- Modify: `crates/flux-cli/src/report.rs:372-392` (`resume_lines`: after the note, `recovered <k> interrupted publications` when `k > 0`), `:848-870`
  (its test), `:853` and the other `ResumeNote::Adopted` constructions (`run/tests.rs:2682`, `:3167`)
- Test: `crates/flux-core/src/run/tests.rs`, `crates/flux-cli/src/report.rs`

**Interfaces:**
- Consumes: Tasks 1-3; `parse_identity`, `from_native_hex` (`state.rs`); `check_component` (`flux_fs`); `DirHandle::{open_dir, metadata, remove_file}`;
  `state_error` (`place.rs:226`); `RunWarning::PartialKept`.
- Produces:
  ```rust
  // run/recover.rs
  pub(crate) enum Verdict { Renamed { remove_temp: bool }, NotRenamed, Gone, Uncertain(String) }   // the String is the evidence class
  /// Phase 1 for one note. Read-only. `dest` is DEST's handle; `dest_shown` is for messages.
  pub(crate) fn classify<D: DirHandle>(dest: &D, operation_id: &str, key: &ClaimKey, note: &PreparedRecord) -> Verdict;
  pub(crate) struct Recovered { pub recovered: u64 }
  pub(crate) enum RecoverError { Uncertain(Vec<(PathBuf, String)>), Store(FsError) }
  /// Both phases: classify every note, then `apply_recovery` once, then remove the stray temp names of RENAMED rows.
  pub(crate) fn recover_publications<D: DirHandle, S: ClaimStore>(dest: &D, dest_shown: &Path, operation_id: &str, store: &mut S, warnings: &mut Vec<RunWarning>, manifest_shown: &Path) -> Result<Recovered, RecoverError>;
  ```
  The evidence classes are exactly `identity unavailable`, `destination holds another object`, `unreadable directory`, `invalid note`.
  `TreePlace::adopt` maps `RecoverError::Uncertain(list)` to `refused_state(LockCode::CommitStateUncertain, <the spec's detail, up to ten targets, the
  total>)` and `RecoverError::Store(e)` through `state_error(&db, e)`. Recovery runs only when `store.supports_prepared()` and `prepared()` is
  non-empty, and runs BEFORE `self.claims = Some(store)` is handed to the walk.
- Validation before any filesystem access (spec decision 5): `PreparedRecord` fields: `name` and non-empty `planned_name` pass `check_component`;
  `temp_name == <planned_name or name>.flux-partial.<operation_id>`; `dir_path` is `""` or `from_native_hex` gives a relative path whose every
  component is `Component::Normal`; `parse_identity(identity)` is `Some`. Any failure -> `Uncertain("invalid note")`. Then open the directory
  component by component (`open_dir`; an error -> `Uncertain("unreadable directory")`), compare `dir.identity()` with `key.parent` (both must be
  `Strong` and equal, else `Uncertain("destination holds another object")`), read T (`metadata(temp_name)`: `Present(identity)` for a regular file,
  `Absent` on NotFound, anything else `Uncertain("unreadable directory")`) and D (`metadata(name)`), and apply the spec's matrix rows 1, 2, 3, 3b, 4,
  5 in order.

- [ ] **Step 1: Write the failing tests**
  - `run/tests.rs`, fixtures: a helper `noted(fs, n, name, target, temp_identity: FileIdentity, dir_path: &str, replacement: bool, planned: &str)`
    that calls `prepare` on prior `n`'s store (like `claimed`), and a Strict `resume()` config; destination objects via `put`, identities via
    `fs.set_identity`. One test per matrix row, each with a named distractor:
    - `recovery_row_1_renamed_commits_the_claim` (D holds the temp's Strong identity, T absent): `adopted` with `recovered: 1`, the claim `Created`,
      `prepared_count() == 0`, the walk then skips the file as resumed (`files_resumed == 1`); distractor: D `Weak` with the same numbers -> UNCERTAIN.
    - `recovery_row_1_removes_a_stray_temp_name_of_the_same_object` (T present with the same identity; the engine has no `link()`+`unlink()`
    fallback today - the probe refuses `NOREPLACE_PUBLISH_UNAVAILABLE` - so this row is pinned by a fixture with `fs.set_identity`): `remove_file(<temp>)` in the log; and when
      that removal fails (`fail` on it) a `PartialKept` warning naming the temp and the adoption still succeeds.
    - `recovery_row_2_not_renamed_discards_and_the_walk_redoes` (T present, D absent): `recovered: 0`, note gone, the file copied (`files_copied`).
    - `recovery_row_3_a_replacement_with_the_temp_present_is_not_renamed_whatever_d_is` (replacement true, D `Weak`): discarded, redone.
    - `recovery_row_3b_a_no_replace_note_whose_entry_was_taken_is_not_renamed` (T present `== R`, D Strong `!= R`): discarded.
    - `recovery_row_4_gone_discards` (T absent, D absent; and T absent, D Strong `!= R`): discarded; distractor: T absent, D `Weak` -> UNCERTAIN.
    - `one_uncertain_note_refuses_the_adoption_and_changes_nothing` (Review Focus 5): one RENAMED and one UNCERTAIN note: `stop` is
      `RunError::Refused { refusal.code == CommitStateUncertain, changed: false }`, detail contains the spec text and both the path and the evidence
      class, no `claim_apply_recovery(` in the log, `prepared_count() == 2`, no file written.
    - `a_note_whose_directory_identity_changed_is_uncertain` (Review Focus 2): `fs.set_identity` on the directory to another Strong id.
    - `recovery_of_a_renamed_replacement_writes_the_planned_claim_too` (Review Focus 3): note with `planned_name` and `replacement`, D holds R:
      both claims `Created`.
    - `a_hostile_note_is_uncertain_and_touches_nothing`: `dir_path` absolute, `..`, `temp_name` of another id, unknown version byte (insert raw bytes
      through the fake's map for this one): UNCERTAIN; no `metadata(`/`open_dir` call for that note's path.
    - `recovery_applies_everything_in_one_transaction`: `fs.fail("claim_apply_recovery", Code::IoError)` with two decidable notes: `Failed` at
      `RunStep::State`, both notes still present.
    - `recovery_runs_before_the_walk_plans_anything`: `claim_apply_recovery` precedes the first `create_new(` of the walk in the log.
    - `the_lock_code_string_is_the_specs`: `LockCode::CommitStateUncertain.as_str() == "COMMIT_STATE_UNCERTAIN"` (in `lock/error.rs`).
  - `report.rs`: extend `resume_lines`' test: `adopted(Some(3), recovered 2)` prints the note then `recovered 2 interrupted publications`; `recovered 0`
    prints no such line; the order with `resumed 7 already-complete files` is note, recovered, resumed.
- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core run::tests::recovery lock::error && cargo nextest run -p flux-cli report::`
Expected: compile errors naming `recovered`, `CommitStateUncertain`.

- [ ] **Step 3: Implement** `recover.rs`, the adopt integration, the note, the code, the report line.
- [ ] **Step 4: Run the tests and the gate**

Run: `just check`
Expected: 0 failed; the 18-key JSON test unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src crates/flux-cli/src/report.rs
git commit -m "feat: commit recovery at --resume - classify every note, apply all or refuse COMMIT_STATE_UNCERTAIN (cut 9c)"
```

---

### Task 7: End to end on the real filesystem (`crates/flux-cli/tests/recovery.rs`)

**Files:**
- Create: `crates/flux-cli/tests/recovery.rs` (own copies of `flux()`, `Stalled`, `names`, `big_tree_in`, `the_operation`, `claim_count` from
  `crates/flux-cli/tests/run.rs:10-110, 488-530`; a `prepared_count(dst, id)` reading the `prepared` table; a `note(dst, id, ..)` that inserts a
  `PreparedRecord` through redb directly)
- Test: that file

**Interfaces:**
- Consumes: the `flux` binary (`env!("CARGO_BIN_EXE_flux")`), `flux_fs::{PreparedRecord, ClaimKey, ObjectId}`, `redb`, `flux_core::state::decode`.

- [ ] **Step 1: Write the tests**
  - `a_strict_copy_killed_at_a_publish_guard_leaves_a_note_that_resume_discards_and_redoes`: `Stalled::start_args(&src, &dst, 306, marks,
    &["--durability", "strict"])` on `big_tree_in`: by the measured counts in `run.rs:700-731` (d0's create is guard #1, its 100 files #2..#301 at
    three guards each - sweep, create, publish - its DirEnd flush #302, d1's create #303), #306 is d1/f000's PUBLISH guard, which the hook precedes;
    verify by measurement (the announcement file, `prepared_count == 1`, `d1/f000.flux-partial.<id>` present, `d1/f000` absent) and correct the
    index in a comment if the count differs: after the kill `prepared_count == 1` and the temporary exists; `flux copy --resume --durability strict`
    exits 0, stderr has no `recovered` line, `prepared_count == 0`, every file equals its source, `dst/.flux` is gone.
  - `a_hand_written_uncertain_note_refuses_resume_with_exit_3_and_changes_nothing`: a killed Strict copy (any stall), then insert a note whose
    `identity` is `unavailable` for an existing published file: `--resume` exits 3, stderr contains `COMMIT_STATE_UNCERTAIN`, the spec's guidance
    sentence and the file's path; `state.db` and the manifest are byte-identical before and after; then `--restart` exits 0 and `dst/.flux` is gone.
  - `a_hand_written_renamed_note_is_recovered_and_reported`: insert a note for a published file whose `identity` is that file's real identity (read
    it through `flux_platform::StdFileSystem` metadata so the numbers match what the engine compares): `--resume` prints
    `recovered 1 interrupted publications`, `prepared_count == 0`, the file is skipped as resumed (JSON `files_skipped` counts it).
  - `a_normal_copy_never_writes_a_note`: a killed Normal copy: `prepared_count == 0` and `meta.format == 2`.
  - `an_older_reader_refuses_a_format_2_store`: open the killed run's `state.db` with redb and assert `meta.format == 2` (the refusal of an older
    binary is pinned at the unit level in Task 2 by the format-3 test; here we pin that this binary WRITES 2).
- [ ] **Step 2: Run them**

Run: `cargo nextest run -p flux-cli --test recovery`
Expected: all pass; run the file three times in a row (no flakiness).

- [ ] **Step 3: Run the gate, push, READ CI on this commit** (Windows and macOS runtime differences surface only there).

Run: `just check && git push`
Expected: 0 failed locally; CI green on ubuntu, windows, macOS for THIS commit.

- [ ] **Step 4: Commit**

```bash
git add crates/flux-cli/tests/recovery.rs
git commit -m "test: commit recovery end to end - a killed strict copy, an uncertain note, a renamed note (cut 9c)"
```

---

### Task 8: The V16 amendment, `TODO.md`, the spec's status

**Files:**
- Modify: `FLUX_FULL_UPDATED_SPEC_V16.md` (edits A-H of the spec's appendix at these verified anchors: A after section 182's closing paragraph "The exact optimization may collapse records, but recovery semantics must
  remain equivalent." (lines ~8137-8138); B the sentence at lines 10844-10846 and the acceptance item 121 at lines 418-419; C after section 183's block ending
  line 8169; D line 13031; E the exit-3 example list at line 2892; F section 119's directory tree at lines 5718-5729 (a note that `wal/` and
  `checkpoints/` appear only when a slice writes them) and section 178 at line 7997; G section 193 at lines 8385-8403; H as a new subsection
  "182.2 Resolved gaps" after A)
- Modify: `TODO.md` (between `## Cut 9b debt` (line 481) and `## Scaffolding follow-ups` (line 493): `## Cut 9c known limits` = the spec's nine, numbered, in the 9b style; `## Cut 9c debt`: the
  spec's "Left to the plan" items that remain open after execution, if any)
- Modify: `docs/superpowers/specs/2026-10-09-cut-9c-commit-recovery-design.md:3` (append `; implemented by docs/superpowers/plans/2026-10-09-cut-9c-commit-recovery.md`)

- [ ] **Step 1: Make the edits.** ASCII only; the amendment text is the spec appendix's, verbatim where it quotes.
- [ ] **Step 2: Run the gate** (typos checks the prose)

Run: `just check`
Expected: 0 failed.

- [ ] **Step 3: Commit**

```bash
git add FLUX_FULL_UPDATED_SPEC_V16.md TODO.md docs/superpowers/specs/2026-10-09-cut-9c-commit-recovery-design.md
git commit -m "docs: V16 amendment for the collapsed commit record (182.1), cut 9c known limits"
```
