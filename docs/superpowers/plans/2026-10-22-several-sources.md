# Several Sources Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy SRC1 SRC2 ... DEST` (fresh copies, files and folders) lands each source at `DEST/<name>` under one operation.

**Architecture:** A new `run::trees` entry mirrors `run::tree_with` but takes N roots: one destination-root lock, one workspace and claim store, one version-5 manifest listing N roots, one prefix lock per root (record written at once, `workspace_path` `none`), and a per-root driver in `tree.rs` that runs the existing tree walk once per folder root and a DEST-level frame for file roots. The CLI takes `SOURCE... DEST`, gates the resume family, and reports one aggregated report with an optional `roots` array.

**Tech Stack:** Rust 2024, rust-version 1.98.1 (`Cargo.toml:10-11`). No new dependency.

**Spec:** `docs/superpowers/specs/2026-10-22-several-sources-design.md` (cited "spec §N"; section numbers are the spec's own, "master spec §N" is `FLUX_FULL_UPDATED_SPEC_V16.md`). Read it first; this plan states only what the spec leaves to the implementer.

## Global Constraints

- One-source behaviour is unchanged: a one-source run writes state format **4** byte-for-byte as today; the report keeps its **18** JSON keys in order and gains `roots` only for several sources (spec G4, §3.5, §3.8).
- State format **5** is written only when `roots` has two or more entries; each version-5 root has `kind` (`"tree"`/`"file"`) and a non-empty `destination_prefix` (spec §3.5, R3).
- Exit codes: 0 success; 1 any failure or abort; 2 usage; 3 refusal before any change (`exit_code.rs:8-11`).
- `--resume`, `--restart`, `--dry-run` with several sources: exit 2, message names the flag; `--restart` and prior-operation refusals name `flux cleanup --force DEST` (spec §3.1, L1).
- Refused names (group 1): `.flux`, `.flux-dir.lock`, `.flux-root.lock` (ASCII case-insensitive) and any name ending `.flux-lock` → `CONTROL_PLANE_NAMESPACE_CONFLICT`; recovery name too long (`name_len(<name>.flux-lock) + 40 > 255`) → `PATH_COMPONENT_INVALID`; on Windows device names and trailing dot/space → `PATH_COMPONENT_INVALID` (spec §3.2, §3.4, R6).
- Source cap: a quarter of the soft descriptor limit, at most 256, at least 1; no limit known → 256 (modelled on `open_writers_cap`, `tree.rs:449`); over the cap → exit 2 (spec §3.4, L5).
- A `Held` has no `Drop`: every path releases prefix locks with `Held::release` (spec §3.4).
- Every persisted DEST-relative value (note `dir_path`, leftover paths, claim/prepared `target`) carries the root prefix (spec §3.6 Keys).
- Gate, run before every commit that touches Rust: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `typos`, `cargo nextest run --workspace --no-tests=pass --no-fail-fast`, `cargo test --doc --workspace`. Record the baseline pass count before Task 1; it must never drop. Use `--no-fail-fast` always (a cancelled run proves nothing).
- Commit trailer, every commit: `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Every new assertion is shown red under a one-line logic mutant before it is trusted (spec §7); the step says which mutant.

## Review Focus

Inputs the spec implies but no task's tests would otherwise exercise, most likely first. Each line has its test in the named task.
1. A source named with a trailing separator, `.` or `../x`: its name is the canonical final component (Task 8 `resolve` tests).
2. Two sources differing only in case: both copy on a case-sensitive destination, collide on a case-insensitive one (Task 3 fake, Task 8 real-fs probe).
3. A file source where `DEST/<name>` is a directory, or a folder source where it is a file: refused before any copy, DEST not created (Task 6, Task 8).
4. A failing file in the first root: the second root still runs, exit 1, both roots reported (Task 7, Task 8).
5. The source count at the cap and one over it (Task 5 `source_cap`, Task 8 `resolve::several`).

## File structure

| File | Responsibility |
|---|---|
| `crates/flux-core/src/state.rs` | version 5, `RootKind`, `Root.kind`, validator and golden bytes |
| `crates/flux-core/src/lock/error.rs`, `lock/site.rs` | two new `LockCode`s, `prefix_lock_fits` |
| `crates/flux-core/src/fault_fs.rs` | case-insensitive `create_lock`/`open_lock` |
| `crates/flux-core/src/run/prefix.rs` (create) | `PrefixLocks`: acquire, alias detection, release |
| `crates/flux-core/src/run/roots.rs` (create) | `SourceSpec`, `plan_roots`, `source_cap` (group 1) |
| `crates/flux-core/src/tree.rs` | `Shared::for_root`, key prefix, `copy_roots_at` driver, file-root copy |
| `crates/flux-core/src/run/place.rs` | `TreePlace::create` split into `create_dest` and `create_workspace` |
| `crates/flux-core/src/run/multi.rs` (create) | `MultiPlace`, `trees`, `MultiRun`, `RootReport` |
| `crates/flux-core/src/prior.rs` | hint for a version-5 prior |
| `crates/flux-cli/src/{main,resolve,report,exit_code}.rs` | `SOURCE... DEST`, gating, aggregation, `roots` |
| `crates/flux-cli/tests/several_sources.rs` (create) | real-filesystem tests |
| `TODO.md`, `ROADMAP.md`, `docs/accepted-boundaries.md` | documentation |

---

### Task 1: State format 5

**Files:**
- Modify: `crates/flux-core/src/state.rs` (constants 17-23; `Root` 62-71; `created` 409-422; `decode` 466-486; `validate` 563-589; doc 304)
- Modify (fix every `Root {` literal the compiler names; it is the oracle): `state.rs` tests, `run/place.rs:568,809`, `run/resume.rs:167`, `run/tests.rs:2858,3494`, `flux-cli/tests/cleanup.rs:198`, `flux-cli/tests/dry_run.rs:146,265,491`
- Modify (version 5 is no longer "incompatible"; these pins move to 6): `state.rs:1115-1119,1982-1987`, `prior.rs:436-452,510-518`, `flux-cli/tests/run.rs:348-363`
- Test: `crates/flux-core/src/state.rs` (tests module)

**Interfaces:**
- Produces: `pub const MULTI_ROOT_FORMAT: u64 = 5;` `pub enum RootKind { Tree, File }` (serde lowercase, `Debug, Clone, Copy, PartialEq, Eq`); `Root.kind: Option<RootKind>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`; `OperationState::created` writes `MULTI_ROOT_FORMAT` iff `config.roots.len() >= 2`, else `FORMAT_VERSION`.

- [ ] **Step 1: Record the golden.** Before changing anything, add test `a_version_4_record_encodes_to_the_pinned_bytes` that builds `OperationState::created(...)` with fixed id/time/roots exactly as the existing test at `state.rs:1865-1866` does, and asserts `encode()` equals a byte literal. Obtain the literal by running the test once with `assert_eq!(String::from_utf8(bytes).unwrap(), "")`, copying the actual output into the test, and re-running. Keys are alphabetical (`serde_json` map without `preserve_order`, `Cargo.lock`), so do not hand-order them.
- [ ] **Step 2: Write the failing tests** (all in `state.rs` tests): `a_version_5_record_with_two_roots_round_trips_with_a_kind_per_root`; `a_version_5_record_with_one_root_is_rejected`; `a_version_4_record_with_two_roots_is_rejected`; `a_version_5_root_without_kind_is_rejected`; `a_version_4_root_with_kind_is_rejected`; `a_version_5_root_with_an_empty_prefix_is_rejected`; `a_version_5_record_without_wal_is_rejected`; `created_writes_version_5_only_for_two_or_more_roots`; `version_6_is_incompatible`. Each asserts the exact `Unusable::Corrupt`/`Incompatible` variant and, for corrupt, a message substring naming the field.
- [ ] **Step 3: Run** `cargo nextest run -p flux-core state:: --no-fail-fast`. Expected: the new tests FAIL (compile error first: `RootKind` undefined).
- [ ] **Step 4: Implement** the interface above. Decode: allow 5 (`state.rs:466`); `v4 = version >= FORMAT_VERSION` (`:471`) so versions 4 and 5 carry `wal_generation`. Validate: replace `(format_version == FORMAT_VERSION) != wal.is_some()` (`:565`) with `>=`; replace the `roots.len() != 1` rule (`:572-574`) with: version >= 5 requires `len >= 2`, every `kind` `Some`, every prefix non-empty and hex; lower versions require `len == 1` and `kind == None`. Fix the stale doc at `state.rs:304`.
- [ ] **Step 5: Move the old pins.** Change the three groups of "`format_version` 5 is incompatible" tests listed under Files to 6. Run the same command; all pass, including the golden from Step 1 unchanged.
- [ ] **Step 6: Mutant check.** Temporarily change `>= MULTI_ROOT_FORMAT` in the roots rule to `>`; confirm `a_version_5_record_with_two_roots_round_trips...` goes red; revert.
- [ ] **Step 7: Gate and commit.** `git add -A crates && git commit -m "feat(core): state format 5 for several roots"` with the trailer.

### Task 2: Lock vocabulary and the fake's case-insensitive locks

**Files:**
- Modify: `crates/flux-core/src/lock/error.rs` (enum 8-24, `as_str` 27-42), `lock/site.rs` (constants 8-18, `name_len` 208-217), `crates/flux-core/src/fault_fs.rs` (`create_lock` 2103-2122, `open_lock` 2250-2298, doc 1233-1242)
- Test: `lock/site.rs`, `fault_fs.rs` tests

**Interfaces:**
- Produces: `LockCode::DestinationNamespaceCollision` (`"DESTINATION_NAMESPACE_COLLISION"`), `LockCode::SafetyRejected` (`"SAFETY_REJECTED"`); `pub fn prefix_lock_fits(name: &OsStr) -> bool` in `lock/site.rs`; re-export both from `lock/mod.rs` if `LockCode` is.

- [ ] **Step 1: Write failing tests.** `as_str` returns the two strings; `prefix_lock_fits_is_false_when_the_recovery_name_exceeds_255` with names of length 215 (true) and 216 (false) for `<name>.flux-lock` + 40 (build the length from `"x".repeat(n)`; 215 + 10 + 40 = 265 > 255, so find the boundary by `255 - 10 - 40 = 205`: names of 205 units fit, 206 do not); `broken_name_length_is_lock_name_plus_forty` asserting `LockSite::broken_name(&crate::ids::new_id()).len() == lock_name.len() + 40` (ties the literal 40 to `ids::new_id`); fake tests `create_lock_in_a_case_insensitive_fake_refuses_a_second_spelling` (`Data.flux-lock` then `data.flux-lock` → `AlreadyExists`) and `open_lock_in_a_case_insensitive_fake_finds_the_stored_spelling`; and the control `without_the_mode_both_spellings_create`.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core lock:: fault_fs:: --no-fail-fast`. Expected: FAIL.
- [ ] **Step 3: Implement** the two variants and `as_str` arms (fix every exhaustive `match` on `LockCode` the compiler names, in the CLI and the report); `prefix_lock_fits` = `name_len(name + LOCK_SUFFIX) + ".broken.".len() + 32 <= NAME_LIMIT`. In the fake, fold ASCII case for `create_lock` and `open_lock` with the same `stored()` helper the other operations use (read `fault_fs.rs` near the `set_case_insensitive` doc); update the doc comment to remove them from "NOT normalized".
- [ ] **Step 4: Run** the same command; PASS. **Mutant:** make `prefix_lock_fits` use `<` instead of `<=`; the boundary test goes red; revert.
- [ ] **Step 5: Gate and commit** (`feat(core): lock codes for several sources; fake folds lock names`).

### Task 3: PrefixLocks

**Files:**
- Create: `crates/flux-core/src/run/prefix.rs`; add `mod prefix;` in `run/mod.rs`
- Test: same file (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `LockSite::directory(&'a D, &OsStr) -> LockResult<LockSite<'a,D>>`; `obtain(&LockSite, LockCapability, Mode, &str) -> LockResult<Obtained<'a,D>>` (`lock/obtain.rs:101`); `Held::write_record`/`release`; `LockRecord` pub fields (`lock/record.rs:12-23`); `LockSite::complete_lock_key() -> LockResult<String>`, `target_path_key() -> String` (`site.rs:116,121`).
- Produces:
```rust
pub(crate) struct PrefixId<'a> { pub operation_id: &'a str, pub owner_instance_id: &'a str, pub boot_session_id: &'a str, pub now: u64 }
pub(crate) struct PrefixIdOwned { pub operation_id: String, pub owner_instance_id: String, pub boot_session_id: String, pub now: u64 }
impl PrefixIdOwned { pub(crate) fn as_ref(&self) -> PrefixId<'_> { /* field-wise borrow */ } }
pub(crate) enum PrefixRefusal { Collision { name: OsString, other: OsString }, Lock(LockError) }
pub(crate) struct PrefixLocks<'a, D: DirHandle> { /* Vec<(OsString, Held<'a, D>)> */ }
impl<'a, D: DirHandle> PrefixLocks<'a, D> {
    pub(crate) fn acquire(dir: &'a D, cap: LockCapability, names: &[OsString], id: &PrefixId<'_>)
        -> Result<(Self, Vec<OsString> /* broken leftovers */), PrefixRefusal>;
    pub(crate) fn release_all(self) -> Vec<(OsString, LockError)>; // failures only
}
```

- [ ] **Step 1: Write failing tests** (fake `FaultFs`, `set_case_insensitive` where stated): `each_lock_gets_its_record_before_the_next_is_created` (read `fs.calls()`: for names `a`,`b`, the index of `create_lock(.../a.flux-lock)` < `write_at_start(.../a.flux-lock)` < `create_lock(.../b.flux-lock)`; decode the file: `workspace_path == "none"`, `operation_id` equals the id); `an_alias_is_a_collision_not_a_busy_lock` (case-insensitive fake, names `Data`,`data` → `PrefixRefusal::Collision`, afterwards neither lock file exists); `a_lock_held_by_another_operation_is_busy` (acquire `a` with id X, then acquire `a`,`b` with id Y → `Lock` with `TargetLockBusy`, and `b`'s file does not exist); `a_failure_releases_the_locks_already_taken` (X holds `b`; Y acquires `a`,`b` → `a.flux-lock` removed); `a_dead_prefix_lock_with_a_none_record_is_recovered` (acquire then drop the `PrefixLocks` without releasing, then acquire again with a new id → `Ok`, record now names the new id); `release_all_removes_every_file`; `a_live_prefix_lock_with_an_old_heartbeat_is_busy_and_the_same_lock_unheld_is_dead` (write the record with `last_heartbeat_wall_time` far in the past while the first `PrefixLocks` is alive: a second acquire is `TargetLockBusy`, not recovered; after `release_all` is NOT called but the first value is dropped, the second acquire recovers it; spec V4). The spec §7 clause about `Unavailable` file identities cannot be exercised on the fake (`FakeLock::path` resolves by a Strong identity, `fault_fs.rs:904-912`); the two mutants below carry that guarantee instead.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core run::prefix --no-fail-fast`; expected FAIL (module absent).
- [ ] **Step 3: Implement** `acquire` per spec §3.4: for each name in order, `LockSite::directory`, `obtain(.., Mode::Plain, id.operation_id)`; on `Obtained::Held { mut held, leftover_broken }` build the record by hand (`workspace_path: "none"`, both times `id.now`, key fields from the site) and `held.write_record(rec)` before the next name; on `Err(LockError::Refused(r))` with `r.code == TargetLockBusy` and `r.holder.as_ref().is_some_and(|h| h.operation_id == id.operation_id)` return `Collision`, naming the held lock whose `target_path_key` equals the holder's; anything else is `Lock`. On any error, `release_all` what is held (a `Held` with a record must use `release`, never `discard`: `held.rs:127`). `Obtained::Claimed` cannot occur in `Mode::Plain` (`obtain.rs:126`): `unreachable!` with that citation.
- [ ] **Step 4: Run**; PASS. **Mutants** (each must turn the named test red, then revert): move `write_record` after the loop → `each_lock_gets_its_record...` and `an_alias_is_a_collision...` red; delete the `operation_id` comparison → `an_alias_is_a_collision...` red; classify by heartbeat age instead of the OS lock (edit a local copy of the busy decision) → the liveness test is red.
- [ ] **Step 5: Gate and commit** (`feat(core): prefix locks for several sources`).

### Task 4: Per-root key prefix in the tree engine

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`Shared` 404; `FluxPathKey::from_relative` calls 1314 and 1829; `note_dir` uses 1535, 1640, 2000, 2065 and def 3017; `finish_copy` leftover 3154)
- Test: `tree.rs` tests module (neighbours of the fixtures near 7400)

**Interfaces:**
- Produces: `Shared.key_prefix: Option<&'c OsStr>` (all existing constructors pass `None`); `impl Shared { fn for_root(&self, src_root: &'c Path, src_identity: FileIdentity, key_prefix: Option<&'c OsStr>) -> Shared<'c, F>; fn keyed(&self, rel: &Path) -> PathBuf }` where `keyed` is `prefix.join(rel)` when `Some`, else `rel`.

- [ ] **Step 1: Write failing tests** (Strict durability so the `prepared` table is on, a tree `x/f` copied with `key_prefix = Some("a")`): `claim_and_prepared_targets_carry_the_root_prefix` (stored `ClaimRecord.target` equals `FluxPathKey::from_relative("a/x/f")`), `a_note_records_dir_path_relative_to_dest` (`PreparedRecord.dir_path == native_hex("a/x")`), `a_leftover_is_relative_to_dest` (inject a failed temp removal as the neighbouring leftover tests do; leftover path is `a/x/f.flux-partial.<id>`), and the control `without_a_prefix_nothing_changes` (same three assertions with `None` equal today's values).
- [ ] **Step 2: Run** `cargo nextest run -p flux-core tree:: --no-fail-fast`; the three prefix tests FAIL, the control passes.
- [ ] **Step 3: Implement** the interface; route the two `from_relative` sites, every `note_dir(&path)` and the leftover rebuild through `keyed`. `TreeFailure.path` stays root-relative (it is display; spec §3.6).
- [ ] **Step 4: Run** the whole `tree::` set; PASS (existing tests prove `None` is behaviour-neutral). **Mutant:** pass `rel` instead of `keyed(rel)` at the claim target site; `claim_and_prepared_targets...` goes red; revert.
- [ ] **Step 5: Gate and commit** (`refactor(core): tree engine can key a walk under a root prefix`).

### Task 5: Group 1: root planning and the source cap

**Files:**
- Create: `crates/flux-core/src/run/roots.rs`; `mod roots;` and `pub use roots::{SourceKind, SourceSpec, source_cap};` in `run/mod.rs`
- Test: same file

**Interfaces:**
- Consumes: `LockCode`, `prefix_lock_fits` (Task 2); `flux_fs::check_component`; `FluxPathKey::from_relative(&Path) -> Result<FluxPathKey>` (`flux-fs/src/claims.rs:20`; the key is `pub Vec<u8>`, no `Ord` derive, so compare `.0`).
- Produces:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum SourceKind { Folder, File }
#[derive(Debug, Clone)] pub struct SourceSpec { pub src: PathBuf, pub name: OsString, pub kind: SourceKind }
pub(crate) fn plan_roots(specs: Vec<SourceSpec>) -> Result<Vec<SourceSpec>, RunError>; // prefix-key order
pub fn source_cap(descriptor_limit: Option<u64>) -> usize;
```
Every refusal is `RunError::Refused { refusal: Box<Refusal{code, holder: None, detail}>, changed: false, not_removed: None }`.

- [ ] **Step 1: Write failing tests** (table-driven): sorted output for `["c","a","b"]` is `a,b,c`; byte-equal names → `DestinationNamespaceCollision` naming both sources; `.flux`, `.FLUX`, `.flux-dir.lock`, `.flux-root.lock`, `x.flux-lock`, `X.FLUX-LOCK` → `ControlPlaneNamespaceConflict`; a 206-unit name → `PathComponentInvalid`, a 205-unit name accepted; `a/b` and `""` (via `check_component`) → `PathComponentInvalid`; `#[cfg(windows)]` `CON`, `nul`, `a.`, `a ` → `PathComponentInvalid`; `source_cap` table: `None`→256, `Some(1024)`→256, `Some(256)`→64, `Some(3)`→1, `Some(0)`→1.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core run::roots --no-fail-fast`; FAIL.
- [ ] **Step 3: Implement**; `source_cap` is `open_writers_cap` (`tree.rs:449`) with the same shape. Refusal detail names the offending source path.
- [ ] **Step 4: Run**; PASS. **Mutant:** drop the `.flux-lock` suffix rule; its table row goes red; revert.
- [ ] **Step 5: Gate and commit** (`feat(core): plan several roots and cap their number`).

### Task 6: MultiPlace and the split of `TreePlace::create`

**Files:**
- Modify: `crates/flux-core/src/run/place.rs` (`TreePlace::create` 385-460)
- Create: `crates/flux-core/src/run/multi.rs` (place part); `mod multi;` in `run/mod.rs`
- Test: `multi.rs` tests; existing `run/tests.rs` is the regression net for the split

**Interfaces:**
- Consumes: `Place<D>` trait (`place.rs:32-100`, every method listed there), `TreePlace` fields (`place.rs:245-266`), Tasks 1, 3, 5.
- Produces: `TreePlace::create_dest(&mut self) -> Result<(), RunError>` (steps 390-404: makes DEST, sets `created_dest`); `TreePlace::create_workspace(&mut self, state: &OperationState, warnings: &mut Vec<RunWarning>, before_rollback: &mut dyn FnMut()) -> Result<(), RunError>` (steps 405-456; it calls `before_rollback()` at the START of each of the three error arms that roll back: the `Probe::Unavailable` arm, the probe `Err` arm and the claim-store `Err` arm, because `refuse_no_replace` (`place.rs:319`) and `unwind_creating` (`:305`) make the `remove_control_dirs(true)` call inside themselves. The early `?` returns (`operations_dir`, `begin_workspace`, `publish_workspace`) do no rollback today and keep that behaviour); `create` = both with `&mut || {}`.
```rust
pub(crate) struct RootPlan { pub name: OsString, pub kind: RootKind, pub src: PathBuf, pub identity: FileIdentity }
pub(crate) struct MultiPlace<'p, F: DestinationRoot> {
    pub(crate) tree: TreePlace<'p, F>,
    pub(crate) roots: Vec<RootPlan>,            // prefix order
    pub(crate) lock_dir: &'p std::cell::OnceCell<F::Dir>, // owned by the caller; outlives the place
    pub(crate) capability: LockCapability,
    pub(crate) prefix_id: PrefixIdOwned,         // defined in Task 3
    pub(crate) prefix: Option<PrefixLocks<'p, F::Dir>>,
}
impl MultiPlace { pub(crate) fn release_prefix_locks(&mut self) -> Vec<(OsString, LockError)>; }
```
`impl Place<F::Dir> for MultiPlace`: every method delegates to `self.tree` except `roots()` (one `Root` per `RootPlan`: `source_root: native_hex(&absolute_lexical(&src))`, `source_identity: identity_text(identity)`, `destination_prefix: native_hex(Path::new(&name))`, `kind: Some(kind)`), `create`, `adopt` (returns `Refused` `IncompatibleState`, "resuming several sources is not supported in this version") and `remove` (calls `release_prefix_locks()` first, then `self.tree.remove(id)`).

- [ ] **Step 1: Prove the split changes nothing.** Run `cargo nextest run -p flux-core run:: --no-fail-fast`; record the pass count. Do the split (behaviour-preserving) and re-run; the count and results are identical.
- [ ] **Step 2: Write failing tests** (fake): `create_orders_dest_then_locks_then_checks_then_workspace` (call log: `create_dir(<dest>)` < every `create_lock` < the first `.creating` workspace call); `a_group_2_refusal_makes_no_workspace_and_removes_a_dest_it_made` (a file root whose `DEST/<name>` is a directory; assert `Refused{changed:false}` with `DestinationNamespaceCollision`, `!fs.called("create_dir(<dest>/.flux")`, DEST absent, no `*.flux-lock`); `a_target_that_is_the_source_is_refused_safety_rejected` (file root whose `DEST/<name>` has the source's identity); `an_alias_refuses_and_leaves_nothing` (case-insensitive fake, `Data`/`data`); `a_claim_store_failure_rolls_back_dest_and_the_prefix_locks` (`fail` on `create_claim_store`; assert DEST absent and no lock files — this is what `before_rollback` is for); `an_early_workspace_failure_still_releases_the_prefix_locks` (`fail` on creating `<id>.creating`; `begin_workspace` returns through `?` with no rollback, so DEST stays as `TreePlace` leaves it, but no `*.flux-lock` remains — this is what the catch-all release is for); `roots_records_kind_prefix_and_absolute_source_per_root`; `kind_is_tree_and_workspace_path_is_operations_id`. Take the assertion-after-index style from `run/tests.rs:143-153`, asserting absence only after a named log index.
- [ ] **Step 3: Run** `cargo nextest run -p flux-core run::multi --no-fail-fast`; FAIL.
- [ ] **Step 4: Implement** `MultiPlace::create`: `tree.create_dest()?`; obtain the lock directory (`tree.holder` when DEST is a filesystem root, else `tree.holder.open_dir(name)` stored with `lock_dir.set(..)`, then `lock_dir.get()`); `PrefixLocks::acquire(..)`; group 2 over `roots` in order using the opened DEST handle (`tree.dest`): `metadata(name)` wrong kind → `DestinationNamespaceCollision`; for a file root, equal `Strong` identity to the source → `SafetyRejected`; on any failure release the locks, then roll back exactly as `TreePlace` does (`remove_control_dirs(true)`); then `tree.create_workspace(state, warnings, &mut || release locks)` and, whenever it returns `Err`, release whatever prefix locks are still held (`self.prefix.take()`; releasing twice is a no-op), which covers the early `?` returns. Push `RunWarning::BrokenLeftover` for each leftover. Convert `PrefixRefusal` with `from_lock(.., RunStep::Lock, ..)` shape (`session.rs:494`): `Collision` → `Refused{DestinationNamespaceCollision, changed:false}`.
- [ ] **Step 5: Run**; PASS. **Mutants:** run group 2 after `create_workspace` → `a_group_2_refusal_makes_no_workspace...` red; pass `&mut || {}` instead of the release closure → `a_claim_store_failure...` red.
- [ ] **Step 6: Gate and commit** (`feat(core): MultiPlace for several roots`).

### Task 7: The per-root driver and `run::trees`

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (add the driver; it must live here because `Batch`, `flush_batch`, `drain_batches`, `copy_one` are private); `crates/flux-core/src/run/multi.rs` (add `trees`)
- Test: `multi.rs` tests (fake)

**Interfaces:**
- Consumes: Tasks 3-6; `copy_tree_at` (`tree.rs:607`), `copy_one` (`:1263`), `copy_tree_frames` root set-up (`:661-691`), `drain_batches` (`:2943`), `prepare_source` (`:383`), `locate_dest` (`place.rs:200`), `preflight` (`tree.rs:3192`), `containment` (`:3251`), `open_operation` (`session.rs:117`), `finish` (`run/mod.rs:923`).
- Produces:
```rust
// tree.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum RootStatus { Completed, Failed, NotStarted } // re-exported as run::RootStatus
pub(crate) enum RootInput<'a, F: FileSystem> {
    Folder { name: OsString, src_root: PathBuf, source: Source<'a, F> },
    File { name: OsString, src_parent: PathBuf, identity: FileIdentity },
}
pub(crate) struct RootDone { pub name: OsString, pub status: RootStatus, pub files_copied: u64, pub files_failed: u64 }
pub(crate) fn copy_roots_at<F: DestinationRoot>(base: &Shared<'_, F>, dest: &F::Dir, dest_created: bool,
    roots: Vec<RootInput<'_, F>>, out: &mut TreeOutcome, on_report: &mut dyn FnMut(TreeFailure))
    -> (Vec<RootDone>, Result<(), CopyError>);
// multi.rs
pub struct RootReport { pub name: OsString, pub status: RootStatus, pub files_copied: u64, pub files_failed: u64 }
pub struct MultiRun { pub run: Run<Result<TreeOutcome, TreeAbort>>, pub roots: Vec<RootReport> }
pub fn trees<F: DestinationRoot>(fs: &F, specs: Vec<SourceSpec>, dst_root: &Path, opts: &CopyOptions,
    cfg: &RunConfig, on_report: &mut dyn FnMut(TreeFailure)) -> MultiRun;
```

- [ ] **Step 1: Write failing tests** (fake): `two_folders_and_a_file_land_at_dest_name` (bytes equal, `directories_created` counts DEST and each `DEST/<name>`); `roots_are_copied_in_prefix_order_whatever_the_argument_order` (sources `a b c` and `c b a`: the first write into each `DEST/<name>` follows the last write into the previous, from `calls()`; mutant: copy in argument order → red); `an_existing_name_follows_the_existing_policy_for_a_file_root` (`--overwrite`, `--update`, `--skip-existing` × absent/older/newer, Strong identity; same bytes, decision and counters as `run::file` on the same fixture); `a_weak_destination_identity_makes_a_file_root_a_collision_like_a_tree_file` and `a_skip_never_reaches_the_identity_gate_for_a_file_root` (the two accepted differences, spec §3.6); `a_failing_file_in_root_one_does_not_stop_root_two` (statuses Failed then Completed); `a_lost_lock_between_roots_aborts_and_names_the_rest` (use `cfg.before_mutation` or `fs.on_nth` to remove the lock after root one; root two and three `NotStarted`, workspace kept, `copy` is `Err` with `TargetLockBusy`); `prefix_lock_files_are_gone_after_success_failure_and_refusal`; `a_held_prefix_lock_on_the_last_root_refuses_and_nothing_is_copied` (another operation holds the LAST root's lock; `TARGET_LOCK_BUSY`, exit-3 shape `Refused{changed:false}`, no write into any `DEST/<name>`; a mutant that locks each root just before copying it is red); `counters_are_sums_over_roots_and_a_file_root_counts_as_one_file` (one folder root with a failing file plus one file root: `files_total`, `files_copied`, `files_failed`, `bytes_copied`, `directories_created`, `errors` each equal the sum); `nested_roots_copy_twice` (`a`, `a/sub` → `DEST/a/sub` equals `DEST/sub`, manifest has two roots); `source_containing_dot_flux_operations_is_refused_as_for_one_source` (pins `reserved_path` on the walk-relative path, spec §3.6); `the_workspace_is_under_dest_and_nothing_control_is_inside_a_root` (A1: no `.flux`, `*.flux-lock`, `*.flux-state.*` under `DEST/<name>`; `DEST/.flux/operations/<id>/manifest` decodes to format 5 with roots in key order while the run is in flight — read it from a `before_mutation` hook).
- [ ] **Step 2: Run** `cargo nextest run -p flux-core run::multi --no-fail-fast`; FAIL.
- [ ] **Step 3: Implement `copy_roots_at`.** For each root in order: `(base.beat)()` then `(base.guard)()`, as `walk_into` does before a directory (`tree.rs:780-781`). Folder root: create `DEST/<name>` with `dest.create_dir(name)` (count `out.directories_created += 1`) or on `AlreadyExists` `dest.open_dir(name)` (merge, `created = false`), then `copy_tree_at(&base.for_root(src_root, source.identity, Some(name)), source.events, root_dir, created, out, on_report)`. File roots: build one DEST-level frame exactly as `copy_tree_frames` builds the root (`:661-691`; extract that block into a helper both use), then per file `out.files_total += 1`, the `reserved_path` check, `copy_one` with `src_root` the file's folder and `path` the single component; finish with `drain_batches` and an explicit claim flush (`beat`, `guard`, `claims.flush()`) because the root frame has no `DirEnd` (`tree.rs:905-909`). An abort-class error stops the loop; every later root is `NotStarted`. Per-root counters are deltas of `out`. Wrap `on_report` so each `TreeFailure.path` is `name.join(path)` for display.
- [ ] **Step 4: Implement `trees`** mirroring `tree_with` (`run/mod.rs:277-531`) with these differences: `debug_assert!(!cfg.dry_run)` and `debug_assert!(specs.len() >= 2)` (a one-root manifest would be version 4 with a `kind`, which Task 1 rejects); call `plan_roots`; for each folder root `prepare_source(fs, &src, &dst_root.join(name), MountPolicy::of(&opts))` (a failure is `run.copy = Some(Err(TreeAbort))` as `:299-305`), for each file root read its `identity` and refuse a lexical `DEST/<name>` equal to the source; `locate_dest` once, then `preflight` and `containment` per folder root as `locate_tree` does (`place.rs:183-193`); build `lock_dir: OnceCell` before the `MultiPlace`; before `finish`, call `place.release_prefix_locks()` and push any failure as `RunWarning::NotRemoved`; compute `ended` as `tree_with` does (`:476-485`) and `RootReport`s from `RootDone`. Reuse the claims flush, WAL end and `finish` blocks unchanged. Build the embedded `TreePlace` as `tree_with` does (`run/mod.rs:341-354`), filling `src_root` and `src_identity` with the first root's: they are read only by `TreePlace::roots`, which `MultiPlace` overrides.
- [ ] **Step 5: Run**; PASS; run `cargo nextest run -p flux-core --no-fail-fast` to confirm the existing suite is untouched.
- [ ] **Step 6: Gate and commit** (`feat(core): run::trees copies several roots under one operation`).

### Task 8: Command line, resolution, report

**Files:**
- Modify: `crates/flux-cli/src/main.rs` (`CopyArgs` 53-118; `copy` 275; uses at 282, 292, 499, 502; doc 26-33), `resolve.rs`, `report.rs` (`Report` 23-54, new `roots`), `exit_code.rs` (none expected)
- Create: `crates/flux-cli/tests/several_sources.rs`
- Test: `resolve.rs` tests, `report.rs` tests, the new file

**Interfaces:**
- Consumes: `flux_core::run::{trees, MultiRun, RootStatus, SourceSpec, SourceKind, source_cap}`.
- Produces: `CopyArgs.paths: Vec<PathBuf>` (`#[arg(required = true, num_args = 2..)]`; the last is DEST); `resolve::several(sources: &[PathBuf], dest: &Path, cap: usize) -> Result<(Vec<SourceSpec>, PathBuf), Stop>`; `Report.roots: Option<Vec<RootJson>>` (`#[serde(skip_serializing_if = "Option::is_none")]`, last field) with `RootJson { name: String, status: &'static str, files_copied: u64, files_failed: u64 }`; `report::root_lines(&[RootReport]) -> Vec<String>`; `Report::several_run(&MultiRun, ms) -> Report` (= `tree_run` plus `roots`).

- [ ] **Step 1: Write failing `resolve` tests:** `several_names_come_from_the_canonical_source` (`.`, `../x`, `dir/` inside a tempdir); `a_symlink_source_is_refused_before_the_name_is_taken` (`Stop::SymlinkSource`, as `resolve.rs:36`); `a_missing_source_is_failed_naming_it`; `dest_that_is_a_file_is_a_usage_error`; `over_the_cap_is_a_usage_error_and_at_the_cap_is_accepted`; `a_source_without_a_final_component_fails` (`/`).
- [ ] **Step 2: Write failing `report` tests:** `several_run_report_has_a_roots_array_in_prefix_order_with_statuses`; `a_one_source_report_has_no_roots_key_and_the_same_18_keys` (key list asserted verbatim from `report.rs:24-41` order); `a_refusal_before_transfer_has_no_roots` (`Report::pre_engine`); `root_lines_name_the_not_started_roots`.
- [ ] **Step 3: Write failing real-filesystem tests** in `several_sources.rs` (helper `flux()` as in `tests/copy.rs:5`): `three_positionals_copy_each_source_to_dest_name` (two folders and a file; compare trees; assert A1: no `.flux`/`*.flux-lock` under any `DEST/<name>`, and no `DEST/.flux` left); `the_result_does_not_depend_on_argument_order`; `nested_sources_copy_twice`; `dest_is_created_when_missing`; `dest_that_is_a_file_exits_2`; `resume_restart_and_dry_run_exit_2_naming_the_flag` (and `--restart`'s message contains `flux cleanup --force`); `byte_equal_names_exit_3_with_the_code_and_dest_untouched`; `a_dot_flux_source_exits_3`; `a_missing_source_exits_1`; `json_has_roots_for_several_sources_and_not_for_one`; `a_failing_file_in_root_one_still_copies_root_two_and_exits_1` (`#[cfg(unix)]`, skip when running as root); `an_alias_is_refused_on_a_case_insensitive_destination` (probe case sensitivity by creating `p` then testing `P`; skip the assertion pair when sensitive and assert both copy instead); `a_file_source_over_an_existing_directory_exits_3`.
- [ ] **Step 4: Run** `cargo nextest run -p flux-cli --no-fail-fast`; FAIL.
- [ ] **Step 5: Implement.** Replace `source`/`destination` with `paths`; with exactly two the existing flow runs unchanged on `(&paths[0], &paths[1])`; with more, `copy_several`: gate `--resume`/`--restart`/`--dry-run` first (exit 2, `error: --X with several sources is not supported yet`; `--restart` appends `; run flux cleanup --force DEST to remove an aborted several-source run`; usage errors print no JSON, as `main.rs:284-287`), then `resolve::several` with `source_cap(cfg.descriptor_limit)`, then `trees`, then the same printing as the `Job::Tree` arm (`main.rs:312-379`) plus `root_lines`. Rewrite the `Copy` doc comment; update `--help` text for the two accepted differences. Resolution order per source: `symlink_metadata` on the path as given, then kind, then `canonicalize` for the name (spec §3.2). Make `Stop::Usage` carry the cap and DEST-is-file messages.
- [ ] **Step 6: Run**; PASS. **Mutants:** drop the `--restart` hint → its test red; sort `roots` by argument order → the order test red; revert each.
- [ ] **Step 7: Gate and commit** (`feat(cli): flux copy takes several sources`).

### Task 9: Prior-operation hint, and the later-cut compatibility tests

**Files:**
- Modify: `crates/flux-core/src/prior.rs` (`resumable_refusal` around 174-202)
- Test: `prior.rs`, `run/resume.rs` (tests near 222-290), `cleanup/delete.rs` (tests 890-944), `run/multi.rs`

**Interfaces:** consumes Tasks 1 and 7.

- [ ] **Step 1: Write failing tests:** `prior.rs`: `a_version_5_prior_says_to_run_cleanup_force_not_resume_or_restart` (the message contains `flux cleanup --force` and not `--resume to continue`); `resume.rs`: `a_one_root_run_refuses_a_two_root_prior_as_a_source_mismatch` (stored 2 roots vs now 1; the refusal text is the existing `source_mismatch`); `cleanup/delete.rs`: `an_aborted_version_5_workspace_is_listed_and_force_deletes_it_and_its_partials` (build with the module's `workspace()` helper and a two-root `Config`); `multi.rs`: `a_one_source_restart_supersedes_an_aborted_several_source_run` (abort a `trees` run as in Task 7, then `run::tree` with `cfg.restart`: the prior is ABANDONED and the copy succeeds) and `stale_prefix_locks_of_a_crashed_run_are_recovered_by_the_next_trees_run` (run, drop the place without releasing via the abort path, run again with the same names → succeeds).
- [ ] **Step 2: Run** `cargo nextest run -p flux-core --no-fail-fast`; the hint test FAILS, the others PASS if Tasks 1-7 hold (if one fails, fix the code, not the test).
- [ ] **Step 3: Implement** the hint branch: when `op.state.format_version >= MULTI_ROOT_FORMAT` use the cleanup wording at `prior.rs:178`'s position.
- [ ] **Step 4: Run**; PASS. **Mutant:** make the branch test `>` instead of `>=`; the hint test goes red; revert.
- [ ] **Step 5: Gate and commit** (`feat(core): refusal hint for a several-source prior; compatibility tests`).

### Task 10: Documentation, closing checks

**Files:**
- Modify: `TODO.md` (231 closed with the cut reference; 1119 narrowed to the resume cut; add the open items below), `ROADMAP.md` (phase 2/3 wording: several sources done), `docs/accepted-boundaries.md` (L1 to L6, the two file-root corners, ruling R1 and R7, nested sources copied twice), spec status line

- [ ] **Step 1: Add to `TODO.md`** (each as one item, not part of this cut): a source cap per platform measured on Windows; a walk skip for stray `*.flux-lock` files (V11); `LockSite::named` accepts a lock name its recovery rename cannot hold (already in `.clavity/local-anomalies.md`, promote it); read-after-write across two handles on Windows (spec V10) if CI does not show it.
- [ ] **Step 2: Run the full gate** and compare the pass count with the baseline recorded before Task 1; it is higher by exactly the number of new tests and nothing is skipped that was not before.
- [ ] **Step 3: Cross-platform:** push the branch only when the owner says so; the Windows and macOS jobs must show the alias test, the Windows device-name test and V10 read-after-write; a macOS runner outage is a rerun, not a defect (earlier cuts' note).
- [ ] **Step 4: Commit** (`docs: several sources shipped; limits recorded`).
- [ ] **Step 5:** Hand over for the capstone and test-audit disciplines the owner's rules require; do not run `just pr` (it arms auto-merge) without the owner's word.

## Stand-downs

Plan panel (2026-10-22). Round 1 (agy): every seat "no new findings", census all OK, but its quoted `run/place.rs` 385-460 block was invented (identifiers such as `paths.dest_root`, `RunStep::Place` and `TreePlaceMode` exist nowhere under `run/`); the round is NOT credited and the observation is captured as an agy-learn heuristic. My own read of the real `TreePlace::create` then found two flaws, both FOLDED in 9c3bcc7: the rollback calls sit inside `refuse_no_replace` and `unwind_creating`, so the hook goes at the start of the three rolling-back arms; and the early `?` returns (`operations_dir`, `begin_workspace`, `publish_workspace`) roll nothing back, so a catch-all release of the prefix locks and a test were added. Round 2 (agy, evidence-hardened: each quote with its `sed -n` command): every probe quote and the list of early returns were compared with the file and match; the mutant-to-assertion table and the per-task forward-dependency list ("none" for all ten) raised no finding, and its pick for the step most likely to be guessed wrong (Task 7 Step 3, the explicit claim flush for DEST-level file roots) is already spelled out in that step. Discarded below the floor: none. Unverified-accepted: none. Open obligation carried into the plan: read-after-write across two handles on Windows (spec V10) is shown by the Windows CI job, not assumed.

