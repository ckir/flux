# Hardlink Preservation (cut 12) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy` recreates a hardlink group of the selected namespace as one destination object with several names, for fresh copies and for `--resume`, under `--hardlinks=auto|preserve|copy` (default `auto`), and reports every member it could not link.

**Architecture:** A new `HardlinkIndex` trait in `flux-fs` (one redb file per run in `flux-platform`, an in-memory fake in `FaultFs`) maps a source `ObjectId` to its anchor (`target` claim key plus the anchor's destination `ObjectId`) or to `Failed`. The tree engine's `copy_one` consults it for a file with `nlink > 1` and `Strong` identity after the existing-destination policy has decided: no entry means copy and register; an anchor means link through the Section 16.1 steps with the existing claim protocol; `Failed` means `BLOCKED_BY_CANONICAL_FAILURE`. The `--resume` skip site registers completed canonicals so later members link, and relinks a completed member that is not the anchor's object. A new `DirHandle::create_hardlink` is `linkat` on Unix and `NtSetInformationFile(FileLinkInformation)` on Windows.

**Tech Stack:** Rust 2024, rust-version 1.98.1 (`Cargo.toml:10-11`); `redb` 4.3 (already a `flux-platform` dependency); `rustix` (Unix) and `windows-sys` 0.61 (Windows, `Wdk_Storage_FileSystem` feature already on). No new dependency.

**Spec:** `docs/superpowers/specs/2026-10-26-hardlink-preservation-design.md` ("the design"); its own sections are cited by name, the master spec `FLUX_FULL_UPDATED_SPEC_V16.md` as "master §N". Read the design first; this plan states only what the design leaves to the builder, plus the rulings below.

## Global Constraints

- No persisted format changes: `crates/flux-core/src/state.rs:23` `FORMAT_VERSION = 4`, `:27` `MULTI_ROOT_FORMAT = 5`, `crates/flux-platform/src/claims.rs:31` `FORMAT = 2` stay as they are; the manifest's `Options` (`state.rs:96-112`) does NOT record `--hardlinks` (ruling 4).
- The §53 JSON keeps its **18** keys in order (`crates/flux-cli/src/report.rs:857-876`, `crates/flux-cli/tests/copy.rs:32-51`); new counters are absent at zero, as `mounts_skipped` is (`report.rs:51-53`).
- A file with `nlink <= 1` pays no extra system call over today in a checkpointing run, which every real tree run is (ruling 2); the engine stays sequential.
- Flux never writes bytes through a link (`dir_unix.rs:235-240`, `dir_windows.rs:817-825`, `copy.rs:657-659` stay).
- Linking follows master §16.1 steps 0-4 (lines 1284-1304); a link is published with the same `Publish` the plan chose (`NoReplace` for `Plan::New`, master §241.5).
- Exit codes unchanged: a per-file hardlink failure is a failed file (exit 1); nothing here aborts the operation.
- Gate before every commit that touches Rust: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `typos`, `cargo nextest run --workspace --no-tests=pass --no-fail-fast`, `cargo test --doc --workspace` (`justfile:13-34`, `.github/workflows/ci.yml:57,109,172-175`). Record the baseline pass count before Task 1; it never drops. Always `--no-fail-fast`.
- Commit trailer, every commit: `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Every new assertion is shown red under a one-line logic mutant before it is trusted; the step names the mutant.
- Each task says its model tier (CLAUDE.md gating): Haiku for specified mechanical edits, Sonnet for implementation and tests against this plan; escalate on "can't", not "nicer".

## Plan rulings (refinements of the design; record each in the design's "Calls made by the author" when Task 11 lands)

1. **The workspace retire path knows file names.** `finish_retire` (`state.rs:1003-1040`) removes only `MANIFEST`, its temp, `PROBE`, `PROBE_TEMP`, `STATE_DB` and `wal/`; a stranger keeps the workspace. The design's "the existing sweep/cleanup removes it" is therefore wrong as written: the index file name `hardlinks.db` is added to that list (Task 4), and the run removes the file itself at its end, success or failure.
2. **One source stat per file, shared.** `copy_one` today stats the source in `file_ckpt` (`tree.rs:1797-1805`) for every checkpointing run (every real tree run), again under `--resume` (`:1677`) and under a non-overwrite policy (`:1745`), and `plan_file` stats it once more (`copy.rs:384`). The tree reads the source metadata ONCE, early, when any of these needs it or the run has a hardlink index, and every later reader uses that value. An ordinary file under the default run pays the same two stats as today plus one integer comparison.
3. **Link candidates are never batched.** Under Strict durability a canonical staged into a directory batch (`tree.rs:1824-1851`) would publish after its dependents are met, so a dependent would find no anchor. A candidate (`nlink > 1`, `Strong`) takes the unbatched path, as a symlink does (`tree.rs:2032-2036`). Strict runs with many links lose batching for those files only.
4. **`--hardlinks` is not in the manifest.** A `--resume` may change the mode: `copy` then `auto` heals through the design's resume rule 3; `auto` then `copy` copies the rest and leaves existing links as they are.
5. **`Failed` names the canonical.** The index's failed entry stores the canonical's claim key (`FluxPathKey`), not a `Code`, so a dependent's record reads `BLOCKED_BY_CANONICAL_FAILURE: M/file: its canonical member A/file failed to copy` (master §92 asks the report to tell canonical failures from dependent ones).
6. **`BLOCKED_BY_CANONICAL_FAILURE` is a `Code`.** It is in the master spec's error catalogue (line 2920), so it joins `Code` beside the design's two new codes and is counted as a failed file.
7. **An anchor entry may carry no object.** `Anchor { target, object: Option<ObjectId> }`: `None` for a dry run (no destination object exists) and for a live anchor whose destination identity is not `Strong`; a live member that meets `None` treats the anchor as "cannot be checked" (the design's table row).
8. **The anchor's directory is opened from a second handle on DEST**, `fs.destination_root(dest_shown)` (the one path-based call the writer makes, master §149.7's exemption), whose identity must equal the walk's root; otherwise linking is unavailable for the run (one `RunWarning`, members degrade or fail by mode). A dry run opens none (ruling 10).
9. **The index creation failure is remembered.** A slot that could not create its file answers that error on every lookup: `auto` degrades every candidate (one `RunWarning`), `preserve` fails each with `HARDLINK_UNAVAILABLE`. The same holds when the workspace (or the dry run's temporary directory) cannot be opened at all: the run still carries a slot, in its failed state, so candidacy runs and `preserve` never copies a group silently.
10. **A dry run never does link I/O.** Its index lives in a `TempDir` under `cfg.dry_temp_root` (`run/dry_prior.rs:169-200`, the mechanism a dry `--resume` already uses), named `flux-dry-<operation-id>-hardlinks`; a dependent is reported `would_hardlink` and counted in `files_hardlinked`.
11. **A linked member is not a copied file.** `files_hardlinked` counts it; `files_copied` and `bytes_copied` do not. The summary line appends `; N hardlinked` when non-zero, beside the design's two suffixes.
12. **The fake models the POSIX same-object rename.** `FaultFs::rename_replace` between two names of one object succeeds and changes nothing (plain `rename(2)`), so the engine's step 3 is testable; `rename_no_replace` keeps answering `AlreadyExists` there, as Linux `RENAME_NOREPLACE` and Windows `ReplaceIfExists = false` do (corrected after the Task 3 review); `FaultFs` gains `create_hardlink`, `set_hardlinks_unavailable`, a dynamic link count (names sharing an identity) under the `set_link_count` override, and an in-memory `FakeHardlinkIndex`.
13. **Index I/O errors are "cannot check".** A failing `get`/`insert` degrades the member under `auto` (counted) and fails it with `HARDLINK_UNAVAILABLE` under `preserve`; it never aborts the run.
14. **Name bytes from the index follow `recover::name_of`** (`run/recover.rs:72-82`): on Windows only UTF-8 is accepted; a component that does not decode makes the anchor unresolvable (the "cannot be checked" row).
16. **A skipped member is degraded only once its group has an anchor.** A group whose members are all skipped by the existing-file policy (a destination already copied, linked or not) has lost nothing to Flux, so nothing is counted. The index records such members as `Skipped { object, members }`; when a later member of the group is copied and becomes the anchor, `members` degradations are counted then (the design's row: "one `hardlinks_degraded` per member that stays separate from the anchor"). A `Skipped` entry is never linked to (the design's anti-goal: Flux never anchors on an object it did not create).
15. **A lost lock keeps the index file.** Under `Ended::Lost` (`run/mod.rs:532`) the index is dropped but its file is not removed: the run no longer owns DEST, and the state is left as it is (decision 13 there); the next `--resume` removes the stale file (the design's "Index" section), and `finish_retire` removes it with the workspace.

## Review Focus

Inputs the design implies but no task's tests would otherwise exercise, most likely first. Each has its test in the named task.
1. A group whose canonical is a `Plan::Replace` over a pre-existing file under the default overwrite: the replacement's object is the anchor, and a later member links to it, not to the old object (Task 6 `a_replaced_canonical_is_the_anchor`).
2. Two groups whose members interleave in walk order (`a1 b1 a2 b2`): each links to its own canonical (Task 6 `two_groups_interleaved_link_to_their_own_canonicals`).
3. A member whose target name is occupied by a DIRECTORY: reported `DESTINATION_ERROR` as a file's would be, nothing linked (Task 6 `a_member_whose_target_is_a_directory_fails_like_a_file`).
4. A source whose `nlink > 1` but whose other names are outside the walk: one ordinary copy, nothing counted as degraded (Task 5 `a_lone_selected_name_of_a_multiply_linked_object_is_an_ordinary_copy`).
5. `--resume` after the source tree gained a new first member (walk order changed): the completed old canonical registers when met; the new first member, met earlier, became the anchor; the old canonical is relinked by rule 3 (Task 7 `a_changed_walk_order_still_converges`).

## File structure

| File | Responsibility |
|---|---|
| `crates/flux-fs/src/options.rs` | `Hardlinks` mode, `CopyOptions.hardlinks` |
| `crates/flux-fs/src/error.rs` | three new `Code`s and their strings |
| `crates/flux-fs/src/hardlinks.rs` (create) | `HardlinkEntry`, its encoding, `HardlinkIndex` trait, conformance suite |
| `crates/flux-fs/src/fs.rs` | `DirHandle::{Index, create_hardlink_index, create_hardlink}` |
| `crates/flux-platform/src/hardlinks.rs` (create) | `RedbHardlinkIndex` |
| `crates/flux-platform/src/dir_unix.rs`, `dir_windows.rs` | the two new `DirHandle` methods per platform |
| `crates/flux-platform/tests/hardlink_index.rs` (create), `tests/dir_handle.rs` | conformance, spill, link primitive tests |
| `crates/flux-core/src/fault_fs.rs` | fake link primitive, fake index, same-object rename, dynamic link count |
| `crates/flux-core/src/hardlinks.rs` (create) | `HardlinkSlot` (lazy index, close), `anchor_parent`, candidacy helpers |
| `crates/flux-core/src/state.rs` | `HARDLINK_INDEX` name in `finish_retire` |
| `crates/flux-core/src/tree.rs` | `TreeOutcome` counters, `Shared` fields, `copy_one` candidacy/registration/link path, resume rules, dry predictions |
| `crates/flux-core/src/dry_run.rs` | `PlannedAction::Hardlink` |
| `crates/flux-core/src/run/mod.rs` | slot and anchor root in `drive` and `dry_tree`, two `RunWarning`s |
| `crates/flux-core/tests/hardlinks_std_fs.rs` (create) | real-filesystem engine tests |
| `crates/flux-cli/src/{main,report}.rs`, `tests/copy.rs` | `--hardlinks`, report counters, summary suffixes, warnings |
| `ROADMAP.md`, `README.md`, `TODO.md`, the design | documentation and the rulings |

---

### Task 1: Vocabulary: the mode, the field, the codes

**Tier:** Sonnet (one field reaches every `CopyOptions` literal; the compiler is the oracle).

**Files:**
- Modify: `crates/flux-fs/src/options.rs` (`Links` 82-88 is the model; `CopyOptions` 90-101)
- Modify: `crates/flux-fs/src/error.rs` (`Code` 17-64, `as_str` 67-90, test 140-162)
- Modify: `crates/flux-fs/src/lib.rs` (export `Hardlinks`)
- Modify (every full `CopyOptions {` literal the compiler names; counted 2026-10-26, verify with `rg -n "CopyOptions \{" --type rust`): `crates/flux-fs/src/options.rs`, `crates/flux-cli/src/main.rs:172` (`options()`: `Hardlinks::Auto` for now; Task 9 wires the flag), `benches/copy.rs`, `tests/integration/support/mod.rs`, `crates/flux-core/src/run/tests.rs`, `run/resume.rs`, `run/multi.rs`, `copy.rs`, `walk.rs`, `tree.rs`, `state.rs`, `cleanup/delete.rs`, `crates/flux-core/tests/{safety_std_fs,replace_std_fs,tree_std_fs,dry_run_std_fs,dry_run_prior_std_fs,dry_run_memory}.rs`. Literals that spread (`..opts()`) need nothing.

**Interfaces:**
- Produces: `pub enum Hardlinks { Auto, Preserve, Copy }` (`Debug, Clone, Copy, PartialEq, Eq`); `CopyOptions.hardlinks: Hardlinks`; `Code::HardlinkUnavailable` → `"HARDLINK_UNAVAILABLE"`, `Code::HardlinkIdentityUnavailable` → `"HARDLINK_IDENTITY_UNAVAILABLE"`, `Code::BlockedByCanonicalFailure` → `"BLOCKED_BY_CANONICAL_FAILURE"`.

- [ ] **Step 1: Write failing tests** in `error.rs` tests: extend `every_code_has_the_spec_string` with the three strings (master §15 line 1226, §108 line 5274, line 2920); in `options.rs` tests `hardlinks_default_is_not_a_bool` asserting the three variants are pairwise distinct.
- [ ] **Step 2: Run** `cargo nextest run -p flux-fs --no-fail-fast`; FAIL (unknown variants).
- [ ] **Step 3: Implement** the enum, the field (every literal in `fault_fs`/`run/tests`/`tree` tests and fixtures takes `Hardlinks::Auto` so the default path is what the suite exercises; `crates/flux-core/tests/*` fixtures likewise), the codes and strings. Doc comments: `Hardlinks` cites master §15; `Copy` is "today's behaviour".
- [ ] **Step 4: Run** the whole gate; PASS, pass count unchanged plus the new tests. **Mutant:** swap two strings in `as_str`; the string test goes red; revert.
- [ ] **Step 5: Gate and commit** (`feat(fs): hardlink mode, option field and codes (cut 12)`).

---

### Task 2: The hardlink index: trait, redb file, fake

**Tier:** Sonnet.

**Files:**
- Create: `crates/flux-fs/src/hardlinks.rs`; export from `lib.rs` (`HardlinkEntry`, `HardlinkIndex`, `encode_object`)
- Modify: `crates/flux-fs/src/fs.rs` (`DirHandle` 280: `type Index`, `create_hardlink_index`; models `create_claim_store` 406-413)
- Create: `crates/flux-platform/src/hardlinks.rs`; export `RedbHardlinkIndex` from `lib.rs`
- Modify: `crates/flux-platform/src/dir_unix.rs` (`type Index`; `create_hardlink_index` models `create_claim_store` 382-396), `dir_windows.rs` (`type Index`; models 444-453 via `open_file_at(&self.0, name, FILE_CREATE)`)
- Modify: `crates/flux-core/src/fault_fs.rs` (`FakeHardlinkIndex`, `create_hardlink_index`; model `FakeClaimStore` 176-200 and 2132-2165)
- Create: `crates/flux-platform/tests/hardlink_index.rs`

**Interfaces:**
- Produces, in `flux_fs`:
  ```rust
  pub enum HardlinkEntry {
      Anchor { target: FluxPathKey, object: Option<ObjectId> },
      Failed { canonical: FluxPathKey },
      /// Members the existing-file policy skipped before the group had an anchor (ruling 16): how many, the destination object of the first of them, and that member's claim key (the example a later degradation names).
      Skipped { object: Option<ObjectId>, members: u64, example: FluxPathKey },
  }
  impl HardlinkEntry { pub fn encode(&self) -> Vec<u8>; pub fn decode(bytes: &[u8]) -> Option<Self>; }
  /// `volume` u64 BE ++ `index` u128 BE: 24 bytes, the index key.
  pub fn encode_object(id: &ObjectId) -> [u8; 24];
  pub trait HardlinkIndex {
      fn get(&self, id: &ObjectId) -> Result<Option<HardlinkEntry>>;
      /// Upsert: a re-registered canonical (resume rule 3's recopied anchor) replaces the entry.
      fn insert(&mut self, id: &ObjectId, entry: &HardlinkEntry) -> Result<()>;
      fn count(&self) -> Result<u64>;
  }
  pub mod conformance { pub fn run_all<I: HardlinkIndex>(new_index: impl Fn() -> I); }
  ```
  Encoding: tag byte `0` Anchor without object ++ target bytes; `1` Anchor with object ++ 24 object bytes ++ target bytes; `2` Failed ++ canonical bytes; `3` Skipped without object ++ `members` u64 BE ++ example bytes; `4` Skipped with object ++ 24 object bytes ++ `members` u64 BE ++ example bytes; `decode` is `None` for any other tag, a tag-1 value shorter than 25 bytes, a tag-3 value shorter than 9 bytes, a tag-4 value shorter than 33 bytes.
- `DirHandle`: `type Index: crate::HardlinkIndex;` and `fn create_hardlink_index(&self, name: &OsStr) -> Result<Self::Index>` (exclusive create, `AlreadyExists` if taken, never through a link; `check_component` first).
- `flux_platform::RedbHardlinkIndex { pub fn create_file(file: File) -> Result<Self>; pub fn with_cache_size(file: File, cache_bytes: usize) -> Result<Self>; }`: one table `anchors` (`TableDefinition<&[u8], &[u8]>`), `Builder::new().set_cache_size(..)` as `claims.rs:111`, every write transaction `set_durability(redb::Durability::None)` (`claims.rs:191` is the precedent), error mapping through the same four `*_err` helpers (make them `pub(crate)` in `claims.rs`).
- `FaultFs`: `FakeHardlinkIndex` over `Arc<Mutex<BTreeMap<Vec<u8>, Vec<u8>>>>` using the real encodings; the handle records `create_hardlink_index(<path>)` and writes a marker file into `files` (as `FAKE_STORE_MARKER` does) so retire and cleanup see a file; fault keys `hardlink_index_get`, `hardlink_index_insert` through `record`; the test seam `FaultFs::hardlink_index_entries(&self, path: impl AsRef<Path>) -> Option<Vec<(Vec<u8>, HardlinkEntry)>>` (decoded, in key order; `None` when no index was created at `path`).

- [ ] **Step 1: Write failing tests.** `flux-fs` unit tests: `entry_round_trips_every_variant`, `decode_rejects_an_unknown_tag_and_a_short_object` (tag 5; tag 1 with 10 bytes; tag 3 with 8 bytes; tag 4 with 32 bytes), `object_key_is_volume_then_index_big_endian` (volume 1, index 2 → `[0,0,0,0,0,0,0,1]` ++ 15 zeros ++ `[2]`). Conformance cases: `a_fresh_index_is_empty`, `insert_then_get_returns_the_entry`, `insert_replaces_an_earlier_entry_for_the_same_object`, `objects_on_different_volumes_are_different_keys`, `count_follows_inserts_and_replacements`, `many_entries_round_trip` (5000). `crates/flux-platform/tests/hardlink_index.rs`: `redb_index_passes_the_conformance_suite`; `entries_spill_to_the_file_under_a_small_cache` (`with_cache_size(file, 64 * 1024)`, insert 20_000 anchors with distinct 24-byte keys and a 40-byte target, assert `count() == 20_000`, every 97th key reads back, and the file's length is greater than the cache size: the entries are on disk, not in RAM). `fault_fs.rs` tests: `fake_index_passes_the_conformance_suite`, `create_hardlink_index_refuses_a_taken_name` (model 3838).
- [ ] **Step 2: Run** `cargo nextest run -p flux-fs -p flux-platform -p flux-core --no-fail-fast`; FAIL (missing trait, method, types).
- [ ] **Step 3: Implement** the interfaces above. The `DirHandle` addition compiles only once all three implementors (Unix, Windows, fake) have the type and method: do them in this task.
- [ ] **Step 4: Run** the three crates' tests; PASS. **Mutant:** make `insert` ignore an existing key (insert-if-absent); `insert_replaces...` goes red; revert. **Mutant 2:** set the redb durability to `Immediate`; the spill test still passes (it proves placement, not durability), so note that durability is pinned by reading `set_durability` in review, not by a test.
- [ ] **Step 5: Gate and commit** (`feat(fs,platform): per-run hardlink index (redb) and its fake`).

---

### Task 3: The link primitive on every platform

**Tier:** Sonnet; the Windows call needs measurement on the hosted runner (NTFS) and, by the owner, on ReFS.

**Files:**
- Modify: `crates/flux-fs/src/fs.rs` (`DirHandle`: `create_hardlink`; doc contract below)
- Modify: `crates/flux-platform/src/dir_unix.rs` (model `create_symlink` 303-318; `rename_no_replace` 346-351)
- Modify: `crates/flux-platform/src/dir_windows.rs` (model `rename_at` 1478-1600 for the handle-relative open and the information buffer; `map_symlink_error` 115-137 for the mapping)
- Modify: `crates/flux-core/src/fault_fs.rs` (`create_hardlink`; `set_hardlinks_unavailable`; `rename_replace`/`rename_no_replace` 1582-1630 same-object rule; link count at 1554; `move_object` 564-625 already moves `identities`)
- Test: `crates/flux-platform/tests/dir_handle.rs`; `fault_fs.rs` tests

**Interfaces:**
- `DirHandle::fn create_hardlink(&self, name: &OsStr, other: &Self, existing: &OsStr) -> Result<()>`: creates `name` in this directory as a new name of the object `existing` names in `other`; never replaces (`AlreadyExists` when `name` is taken by anything, a dangling link included); never follows a symlink at `existing` (the link object itself gets the new name); `check_component` on both names first. Maps "this destination cannot hold the link" to `Code::HardlinkUnavailable`: Unix `EPERM`, `EXDEV`, `EMLINK`, `EOPNOTSUPP`/`ENOTSUP`; Windows `STATUS_NOT_SAME_DEVICE` (0xC00000D4), `STATUS_INVALID_DEVICE_REQUEST` (0xC0000010), `STATUS_NOT_SUPPORTED` (0xC00000BB), `STATUS_TOO_MANY_LINKS` (0xC0000265); `STATUS_OBJECT_NAME_COLLISION` (0xC0000035) is `AlreadyExists`. Everything else goes through `FsError::from_io` (`EEXIST` → `AlreadyExists`, `EACCES` → `PermissionDenied`, `ENOSPC` → `DiskFull`, `ENOENT` → `IoError`/`NotFound`).
- Unix: `rustix::fs::linkat(&other.0, existing, &self.0, name, AtFlags::empty())`.
- Windows: open `existing` relative to `other.0` with `NtCreateFile` (`FILE_OPEN_REPARSE_POINT`, as `rename_at` opens `from`; start with `FILE_READ_ATTRIBUTES | SYNCHRONIZE` access, fall back to what `rename_at` requests if the link call answers `STATUS_ACCESS_DENIED`, and record the measured access in the doc comment), then `NtSetInformationFile(.., FileLinkInformation)` with a `FILE_LINK_INFORMATION` whose `RootDirectory` is `self.0`, `ReplaceIfExists` false, name and length as `rename_at` builds its `FILE_RENAME_INFORMATION` (same `u64` buffer argument). `windows_sys::Wdk::Storage::FileSystem::{FILE_LINK_INFORMATION, FileLinkInformation}` exist in 0.61 (verified in the registry source).
- Fake: `create_hardlink` records `create_hardlink(<child>)` (fault key `create_hardlink`); `hardlinks_unavailable` flag → `HardlinkUnavailable`; occupied `name` → `AlreadyExists`; missing `existing` → `IoError`/`NotFound`; a directory at `existing` → `DestinationError`/`IsADirectory`; otherwise copies `files`, `types`, `symlinks`, `times`, `perms` from `existing` to the child and sets `identities[child] = identities[existing]` (no `mint_identity`). `Inner.links` becomes `HashMap<PathBuf, Option<u64>>`: `set_link_count(path, n)` stores `Some(n)`, the new `set_link_count_unknown(path)` stores `None`; `metadata(p).nlink` is the override when one is present, else `Some(number of paths in files whose identity equals p's)` (at least 1). `rename_replace(from, to)` with both present and equal identities: record the call, return `Ok(())`, change nothing (POSIX; master §16.1 step 3's reason); `rename_no_replace` is unchanged and answers `AlreadyExists` for an occupied target whatever its identity.

- [ ] **Step 1: Write failing tests.** `tests/dir_handle.rs` (every OS): `create_hardlink_makes_a_second_name_of_the_same_object` (write `a`, link `b`; `metadata(a).identity == metadata(b).identity`, both `Strong`; both `nlink == Some(2)`; `std::fs::read(b) == a`'s bytes), `create_hardlink_refuses_an_occupied_name_including_a_dangling_symlink` (`AlreadyExists` for a file and for a dangling link at `name`), `create_hardlink_links_a_symlink_object_itself_never_its_target` (Unix: `read_link(b)` equals `read_link(a)`'s payload; Windows: same through the arm's `read_link`), `create_hardlink_refuses_a_bad_component` (`a/b` on either side), `create_hardlink_of_a_missing_source_is_not_found`, `create_hardlink_across_two_directories` (`other` is a sibling directory handle; link lands in `self`), `a_rename_between_two_names_of_one_object_keeps_the_target` (write `a`, link `t`; `rename_replace(t, root, a)`: afterwards `a` exists with the bytes and the identity; on Unix `t` still exists (POSIX's no-op, master §16.1 step 3); on Windows assert only `a`, and record in the test's comment whether `t` survived on the hosted runner: the engine's step 3 handles both). `fault_fs.rs`: `fake_hardlink_aliases_identity_bytes_and_counts_links`, `fake_hardlink_unavailable_is_the_code` (`set_hardlinks_unavailable(true)` → `Code::HardlinkUnavailable`), `a_rename_between_two_names_of_one_object_is_a_no_op` (both names remain, bytes unchanged), `removing_one_name_lowers_the_others_link_count`.
- [ ] **Step 2: Run** `cargo nextest run -p flux-platform -p flux-core --no-fail-fast`; FAIL.
- [ ] **Step 3: Implement** the three arms as specified. The Windows arm is MEASURED on the hosted runner by pushing the branch (a draft PR, auto-merge off) and reading the `Test (windows-latest)` job; a status not in the mapping list is added with its measured value and a comment, never guessed.
- [ ] **Step 4: Run** the tests; PASS on Linux locally and on all three CI arms. **Mutant:** map `EXDEV` through `from_io` instead; a Linux test that links across a tmpfs boundary is not portable, so the mapping is pinned by `create_hardlink_unavailable_maps_the_errno` in `dir_unix.rs` unit tests using the mapping helper directly on `Errno::XDEV`, `Errno::PERM`, `Errno::MLINK` (make the mapping a `fn map_link_errno(e: Errno) -> FsError`); the mutant turns it red; revert.
- [ ] **Step 5: Gate and commit** (`feat(platform): DirHandle::create_hardlink on Unix, Windows and the fake`).

---

### Task 4: The slot, the anchor's directory, the retire name, the run plumbing

**Tier:** Sonnet.

**Files:**
- Create: `crates/flux-core/src/hardlinks.rs`; `pub(crate) mod hardlinks;` in `lib.rs`
- Modify: `crates/flux-core/src/state.rs` (`STATE_DB` 851: add `pub const HARDLINK_INDEX: &str = "hardlinks.db";`; `finish_retire` known list 1007-1013; tests 1453-1483)
- Modify: `crates/flux-core/src/run/recover.rs` (`name_of` 72-82 becomes `pub(crate)`)
- Modify: `crates/flux-core/src/tree.rs` (`TreeOutcome` 39-98; `Shared` 410-440 and `for_root` 446-468; `run_tree`'s literal 634-649)
- Modify: `crates/flux-core/src/run/mod.rs` (`RunWarning` 214; `drive` 440-580: after `open_operation`, before the walk; `dry_tree` 1278-1380)
- Modify: `crates/flux-cli/src/report.rs` (`run_warning_line`, the compiler names the match)
- Test: `hardlinks.rs` unit tests with `FaultFs`; `run/tests.rs`

**Interfaces:**
- `crates/flux-core/src/hardlinks.rs`:
  ```rust
  pub(crate) struct HardlinkSlot<D: DirHandle> { dir: D, state: RefCell<SlotState<D::Index>> }
  enum SlotState<I> { Unopened, Open(I), Failed(Code) }
  impl<D: DirHandle> HardlinkSlot<D> {
      pub(crate) fn new(dir: D) -> Self;
      /// A slot with no directory, already failed: the workspace or temp directory could not be opened (ruling 9). `with` answers `code`; `close` returns `None`.
      pub(crate) fn unavailable(code: Code) -> Self;
      /// Lazily removes a stale `HARDLINK_INDEX` and creates a fresh one on the first call; a creation failure is kept and returned on every call (ruling 9).
      pub(crate) fn with<R>(&self, f: impl FnOnce(&mut D::Index) -> flux_fs::Result<R>) -> flux_fs::Result<R>;
      /// Drops the index; with `remove`, removes the file too (a `NotFound` is fine; ruling 15 passes `false` on a lost lock). Returns the directory (`None` for an `unavailable` slot).
      pub(crate) fn close(self, remove: bool) -> Option<D>;
      /// The creation failure, if one was recorded (the run turns it into `RunWarning::HardlinkIndexUnavailable`).
      pub(crate) fn failure(&self) -> Option<Code>;
  }
  /// The anchor's directory and name, opened component by component from `dest` with `open_dir` (never a path), from the claim-key bytes of `target`; `None` when a component does not decode (`recover::name_of`) or an open fails.
  pub(crate) fn anchor_parent<D: DirHandle>(dest: &D, target: &FluxPathKey) -> Option<(D, OsString)>;
  /// `Some(id)` for `FileIdentity::Strong(id)`, else `None`: the object an anchor entry records.
  pub(crate) fn strong(identity: FileIdentity) -> Option<ObjectId>;
  ```
- `TreeOutcome`: `files_hardlinked: u64`, `hardlinks_degraded: u64` (every member copied independently, whatever the reason), `hardlinks_unchecked: Option<DegradedGroup>`, `hardlink_weak: WeakIdentityWarnings` (the design's §108 aggregation; `WeakIdentityWarnings::record` 302-315 is reused as is), `hardlinks_unlinked: BTreeMap<UnlinkedWhy, DegradedGroup>` with `pub enum UnlinkedWhy { LinkRefused, AnchorUnavailable, PolicySplit, IndexFailure }` (`Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord`): the degradations that are NOT weak identity, one count and first example per cause, for one warning line per cause present.
- `Shared`: `hardlinks: Option<&'c HardlinkSlot<F::Dir>>` (None: `copy` mode, lockless `copy_tree`), `anchor_root: Option<&'c F::Dir>` (None: dry run, lockless, or ruling 8's mismatch). `for_root` copies both.
- `RunWarning::HardlinkIndexUnavailable { path: PathBuf, code: Code }` (`path` is `operations_shown().join(id).join(HARDLINK_INDEX)`, or the dry run's temp path; pushed once by the run from `slot.failure()` after the walk, and at once when the workspace or temp directory could not be opened) and `RunWarning::HardlinkRootUnavailable { error: FsError }` (ruling 8). `report::run_warning_line` renders them: `warning: the hardlink index <path> could not be created (<error>); hardlinks were copied independently - --hardlinks=preserve fails them instead` and `warning: DEST could not be reopened for linking (<error>); hardlinks were copied independently - ...`.
- `drive` (run/mod.rs): when `opts.hardlinks != Hardlinks::Copy`, `let slot = match place.tree_place().open_workspace(id) { Ok(w) => HardlinkSlot::new(w), Err(e) => { push HardlinkIndexUnavailable; HardlinkSlot::unavailable(e.code) } }` and `let anchor_root = fs.destination_root(&place.tree_place().dest_shown)` whose `identity()` must equal the walk root's (`place.dest` identity, read before `place.dest.take()` in the closure at 383: read it in `drive` from `place.tree_place().dest.as_ref()`); after the walk, once `ended` is known (532-540) and before `finish`: `slot.close(!matches!(ended, Ended::Lost))` (ruling 15), push `HardlinkIndexUnavailable` if `slot.failure()` was `Some`, then `drop(anchor_root)`. Windows: both handles are closed before the workspace is retired.
- `dry_tree`: when `opts.hardlinks != Copy`, `TempDir::make(fs, &temp_root, &format!("{}-hardlinks", cfg.operation_id))` (`dry_prior.rs:182-185`) holds the slot's directory (a `make` failure gives `HardlinkSlot::unavailable` plus the warning); `anchor_root: None`; closed with `remove: true` after the walk, before the `TempDir` drops.

- [ ] **Step 1: Write failing tests.** `hardlinks.rs`: `the_slot_creates_the_index_on_first_use_and_close_removes_it` (FaultFs dir; before `with`, no `hardlinks.db`; after, the file exists; after `close(true)`, gone), `close_without_remove_keeps_the_file` (ruling 15), `a_stale_index_is_removed_before_a_fresh_one_is_created` (pre-write `hardlinks.db` with bytes; after `with`, the fake index is empty and the call log shows `remove_file(…/hardlinks.db)` before `create_hardlink_index(…)`), `a_creation_failure_is_answered_on_every_call` (`fs.fail_always("create_hardlink_index", Code::IoError)`; two `with` calls both `Err` with `IoError`; exactly one `create_hardlink_index` call in the log; `failure() == Some(Code::IoError)`), `an_unavailable_slot_answers_its_code_and_closes_to_nothing`, `anchor_parent_opens_components_from_dest_and_refuses_a_link_component` (dest with `d/e/f`; `anchor_parent(dest, key("d\0e\0f"))` gives a handle whose `metadata("f")` is the file; with `d/e` replaced by a symlink to a directory, `None`). `state.rs`: extend `finish_retire_removes_only_the_known_names_and_keeps_a_directory_with_a_stranger` with `HARDLINK_INDEX` among the known names. `run/tests.rs`: `a_run_under_auto_with_no_hardlinks_leaves_no_index_file` (default fixture; after the run `!fs.exists("/p/dest/.flux/operations/<id>/hardlinks.db")` and no `create_hardlink_index` call: laziness), `copy_mode_opens_the_workspace_once_less_than_auto` (the count of `open_dir(/p/dest/.flux/operations/<id>)` calls under `Hardlinks::Copy` is the count under `Auto` minus one: the slot's handle), `a_lost_lock_keeps_the_index_file` (the `TargetLockBusy` abort fixture the lost-lock tests use, with a pair so the index exists: after the run `hardlinks.db` is still there and no `remove_file(…/hardlinks.db)` is in the log). The dry-run parking test is Task 8's (nothing creates the index before Task 6).
- [ ] **Step 2: Run** `cargo nextest run -p flux-core --no-fail-fast`; FAIL.
- [ ] **Step 3: Implement** the interfaces. `copy_tree` (`run_tree`) passes `None, None`.
- [ ] **Step 4: Run**; PASS. **Mutant:** make `close(true)` skip `remove_file`; the first test goes red; revert. **Mutant 2:** drop the stale removal; the stale test goes red; revert. **Mutant 3:** pass `remove: true` on `Ended::Lost`; `a_lost_lock_keeps_the_index_file` goes red; revert.
- [ ] **Step 5: Gate and commit** (`feat(core): per-run hardlink slot, anchor directory, index name in retire, run plumbing`).

---

### Task 5: Candidacy and the degradations

**Tier:** Sonnet.

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`copy_one` 1546-1596 no-claims path; 1597-1628 before the batching pre-check; `file_ckpt` 1797-1805; resume stat 1677; policy stat 1745)
- Modify: `crates/flux-core/src/hardlinks.rs` (the candidacy helper)
- Test: `run/tests.rs`

**Interfaces:**
- `hardlinks.rs`:
  ```rust
  pub(crate) enum Candidacy { NotCandidate, Unchecked, Weak(FileIdentity), Candidate(ObjectId) }
  /// `nlink > 1` and `Strong` is a candidate; `nlink == None` is `Unchecked`; `nlink > 1` with a `Weak`/`Unavailable` identity is `Weak(identity)`; a regular file with `nlink <= 1` is not a candidate. Non-regular files are never candidates.
  pub(crate) fn candidacy(meta: &Metadata) -> Candidacy;
  ```
- `copy_one`: a new local `src_known: Option<Metadata>` filled once at the top of the claims path by `cx.fs.metadata(&src)` when `cx.hardlinks.is_some() || cx.ckpt.is_some() || cx.resume || cx.opts.existing != ExistingPolicy::Overwrite` (a stat error is reported as `CopyStep::Source` and the entry returns, exactly as 1680-1682 does today); `file_ckpt` and the two later stat sites read `src_known` instead of calling `metadata`. In the no-claims path (1560-1596) the same stat runs when `cx.hardlinks.is_some()`.
- Degradations, decided from `candidacy(&src_meta)` when `cx.hardlinks.is_some()` and the file is a regular file:
  - `Unchecked`: `DegradedGroup::note(&mut out.hardlinks_unchecked, &path)`; the file copies as today.
  - `Weak(id)`: `auto` → `out.hardlinks_degraded += 1; out.hardlink_weak.record(id, &path)`; copies as today. `preserve` → report `TreeFailureCause::Copy(CopyError::at(CopyStep::Gate, FsError::new(Code::HardlinkIdentityUnavailable, other("the source filesystem reports no reliable identity for a multiply-linked file"))))` and return `Ok(())`; nothing is created.
  - `Candidate(_)` in the no-claims path: `auto` → `hardlinks_degraded += 1`, copy as today; `preserve` → `HardlinkUnavailable` at `CopyStep::Claim` with "no claim store keys this directory, so no anchor can be recorded", return.
  - `Candidate(_)` with claims: Task 6.
- `Hardlinks::Copy` makes `cx.hardlinks` `None` (Task 4), so this task adds no work to that mode.

- [ ] **Step 1: Write failing tests** (`run/tests.rs`, `FaultFs` fixture `fake()` plus `set_link_count` / `set_link_count_unknown` (Task 3) / `set_identity` from 1195): `a_weak_identity_group_member_is_copied_independently_and_counted_under_auto` (`set_link_count("/src/a", 2)`, `set_identity("/src/a", Weak(..))`; outcome `hardlinks_degraded == 1`, `hardlink_weak.weak[&volume].count == 1` with `example == "a"`, `files_copied == 2`, no failure), `a_weak_identity_group_member_fails_under_preserve` (`code_of == HardlinkIdentityUnavailable`, `!fs.exists("/p/dest/a")`, no `create_new` for it), `an_unknown_link_count_is_counted_unchecked_and_copied` (`set_link_count_unknown("/src/a")`, Strong identity; `hardlinks_unchecked == Some(DegradedGroup { count: 1, example: "a".into() })`, `files_copied == 2`; the same under `preserve`), `a_lone_selected_name_of_a_multiply_linked_object_is_an_ordinary_copy` (`set_link_count("/src/a", 2)`, Strong identity, only one name in the tree: `hardlinks_degraded == 0`, `files_hardlinked == 0`, `files_copied == 2`), `copy_mode_counts_and_warns_nothing` (the weak and unknown fixtures under `Hardlinks::Copy`: every new counter zero, no `hardlink_index_get` call), `an_ordinary_file_pays_no_extra_stat` (default fixture: the number of `metadata(/src/a)` calls under `Auto` equals the number under `Copy`: ruling 2), `an_ordinary_file_never_touches_the_index` (default fixture under `Auto`: no `hardlink_index_get` or `create_hardlink_index` call).
- [ ] **Step 2: Run** `cargo nextest run -p flux-core --no-fail-fast`; the new tests FAIL.
- [ ] **Step 3: Implement** as specified.
- [ ] **Step 4: Run**; PASS. **Mutant:** `candidacy` treats `nlink == Some(1)` as `Weak`-free candidate material (returns `Candidate`); `an_ordinary_file_never_touches_the_index` goes red once Task 6 consults the index, so in THIS task pin it with a unit test on `candidacy` (`candidacy_of_a_single_name_is_not_a_candidate`); red; revert. **Mutant 2:** `Unchecked` counts into `hardlinks_degraded`; `an_unknown_link_count...` red; revert.
- [ ] **Step 5: Gate and commit** (`feat(core): hardlink candidacy, weak-identity and unknown-link-count degradations`).

---

### Task 6: Fresh copies: register the canonical, link the members, block on a failed canonical

**Tier:** Sonnet; the largest task. Review it against the design's "Fresh copy" and "Modes and failures" sections line by line.

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`copy_one` after `plan` is decided, 1780-2030; the `batching` guard 1612-1615 and 1824, 1933; the skip point 1760-1777; new `link_member`, modelled on `publish_link` 2438-2490, `create_link` 2686-2698, `publish_created_link` 2700-2735)
- Test: `run/tests.rs`, `run/multi.rs` tests

**Interfaces:**
- In `copy_one`, with claims and `Candidate(src_id)`: `let entry = cx.hardlinks.map(|h| h.with(|i| i.get(&src_id)))` evaluated ONCE, right after candidacy, before the batching pre-check; an `Err` is `LookupFailed`. `batching` is additionally `&& !candidate` (ruling 3).
- After the plan is decided and the dry-run branch (1782-1788) is passed (dry predictions are Task 8), right after `let Some(parent) = parent` (1789-1791) and before the `own` closure (1792), for a candidate:
  - `Ok(Some(Failed { canonical }))`: report `Copy(CopyError::at(CopyStep::Gate, FsError::new(Code::BlockedByCanonicalFailure, other(format!("its canonical member {} failed to copy", shown(canonical))))))` and return `Ok(())` (`shown` renders the key's components joined by `/`, lossily). Both modes.
  - `Ok(Some(Anchor { target: anchor, object }))`: `link_member(..)`, below. Its `Degrade(step, why, text)` result under `auto` falls through to the ordinary copy arms after `degrade(out, &path, why)`; under `preserve` it is reported as `Copy(CopyError::at(step, FsError::new(Code::HardlinkUnavailable, other(text))))`. Its `Linked(outcome)` records the claim exactly as the `Plan::New` arm (1884-1894) or the `Plan::Replace` arm (2000-2026) does for a published file (strict: `commit_prepared`; else `record_claim`/`upgrade_own_claim` plus the planned-name claim), then `names.record_publication(&target, stored, name, replaced, outcome.published_identity)`, `out.files_hardlinked += 1`, and for a replacement `out.files_overwritten += 1`. It does NOT call `finish_copy` (ruling 11); complaints cannot arise (no metadata is applied to a link). Its `Err(e)` goes through `finish_copy(.., Err(e), ..)` so stops (`TargetLockBusy`, heartbeat) propagate and other failures are reported.
  - `Err(_)` (the lookup failed, ruling 13): `preserve` → report `HardlinkUnavailable` at `CopyStep::Claim` ("the hardlink index could not be read") and return; `auto` → `degrade(out, &path, IndexFailure)` (below) and copy through the existing arms with NO registration afterwards.
  - `Ok(None)` or `Ok(Some(Skipped { members, example, .. }))`: copy through the existing arms (this member is the canonical); after `finish_copy` returns `Ok(())` for the New or Replace arm, if `published` is `Some(identity)`, `h.with(|i| i.insert(&src_id, &Anchor { target: target.clone(), object: strong(identity) }))` and, for a `Skipped` entry, `out.hardlinks_degraded += members` and the `PolicySplit` group's count grows by `members` with `example` (decoded through `recover::name_of` per component, joined by `/`) as its example when the group was empty (the skipped ones stay separate from the anchor, ruling 16); if the copy failed (the `result` was `Err` and `finish_copy` returned `Ok`), `insert(&src_id, &Failed { canonical: target.clone() })`. An insert error: `preserve` → report `Copy(CopyError::at(CopyStep::Claim, e))` for this member (it was published; the record says its group cannot be linked); `auto` → `degrade(out, &path, IndexFailure)` for THIS member only. The later members then find no entry and form their own group around the next of them, so exactly one extra object results, and it is the one counted.
  - `degrade(out: &mut TreeOutcome, path: &Path, why: UnlinkedWhy)` is `out.hardlinks_degraded += 1; DegradedGroup::note(out.hardlinks_unlinked.entry(why)...)` (adapt `note` to a `&mut Option` obtained from the map, or give the map's slot the same count-and-first-example rule); every non-weak degradation in this task and Task 7 goes through it (Task 5's weak path counts `hardlinks_degraded` and `hardlink_weak` instead). `link_member`'s `Degrade` carries its `why`: anchor checks → `AnchorUnavailable`, a refused link → `LinkRefused`.
  - The skip point (1760-1777), for a candidate, by `entry` (ruling 16): `None` → `insert(&src_id, &Skipped { object: strong(meta.identity), members: 1, example: target.clone() })`, nothing counted; `Skipped { object, members, example }` → when `object.is_some() && strong(meta.identity) == object` upsert `members + 1` (same `example`) and count nothing (the skipped members are one object among themselves), else `degrade(out, &path, PolicySplit)` (it is separate even from them); `Anchor { object: Some(a), .. }` with `meta.identity == Strong(a)` → nothing (already a correct link); any other `Anchor` → `degrade(out, &path, PolicySplit)`; `Failed` → nothing (the group failed; this member was never Flux's to copy). An index error at the skip point counts nothing under `auto` and is a `HardlinkUnavailable` failure under `preserve`, as ruling 13 says.
- ```rust
  enum LinkStep { Linked(flux_fs::Outcome), Degrade(CopyStep, UnlinkedWhy, &'static str) }
  #[allow(clippy::too_many_arguments)]
  fn link_member<F: DestinationRoot>(cx: &Shared<'_, F>, parent: &F::Dir, names: &mut NameIndex, batch: &mut Batch<F::Writer>, out: &mut TreeOutcome, on_report: &mut dyn FnMut(TreeFailure), name: &OsStr, anchor: &FluxPathKey, object: Option<ObjectId>, publish: Publish, before_create: &BeforeCreate<'_>, before_publish: &BeforePublish<'_>) -> Result<Result<LinkStep, CopyError>, CopyError>
  ```
  Order (the outer `Err` is a flush stop, as `publish_link`): (1) `object` `None` → `Degrade(Gate, AnchorUnavailable, "the anchor's destination identity is not strong")`; `cx.anchor_root` `None` → `Degrade(Gate, AnchorUnavailable, "DEST could not be reopened for linking")`; `anchor_parent(root, anchor)` `None` → `Degrade(Gate, AnchorUnavailable, "the anchor's directory could not be opened")`; `anchor_dir.metadata(&anchor_name)` not `Strong(object)` → `Degrade(Gate, AnchorUnavailable, "the anchor is gone or is another object")`. (2) `temp = temp_name(name, &cx.opts.operation_id)`; the pending-batch flush and fresh beat/guard exactly as `publish_link` 2456-2466; `let _ = parent.remove_file(&temp)` (step 0). (3) beat, guard, `before_create()` (the Replace claim), `parent.create_hardlink(&temp, &anchor_dir, &anchor_name)` at `CopyStep::Create` with the `AlreadyExists`-with-pending-batch retry as 2473-2484; `HardlinkUnavailable` → `Degrade(Create, LinkRefused, "the destination cannot hold the link")`; any other error → `Err`. (4) `parent.metadata(&temp).identity` must be `Strong(object)`; otherwise `let _ = parent.remove_file(&temp)` and `Degrade(Create, AnchorUnavailable, "the anchor changed while linking")`. (5) `before_publish(&PublishIntent { temp, identity })` and `publish_staged(parent, name, Staged { temp, identity, bytes_copied: 0, metadata_failures: vec![], identity_degraded: None, writer: None, reused: 0, discard: None }, publish, cx.guard, cx.beat)` as `publish_created_link` 2714-2734 does. (6) step 3: if `parent.metadata(&temp).is_ok()`, `let _ = parent.remove_file(&temp)`. `Linked(outcome)`.
- Replace plan for a link: `before_create` is the arm's `insert_if_absent(.., Existing)` closure (1901-1919); the replaced object (`meta.identity` Strong) feeds `record_publication`'s `replaced` as 2021-2025.

- [ ] **Step 1: Write failing tests** (`run/tests.rs`; a group is made with a `FakeDirHandle` on `/src`: `fs.destination_root("/src").create_hardlink("a2", &same, "a")`, giving `a` and `a2` one identity and `nlink == Some(2)`; dest identity read with `strong(&fs, ..)`):
  `one_pair_is_one_copy_and_one_link` (`files_copied == 2` (`a`, `sub/b`), `files_hardlinked == 1`, `bytes_copied == 3`; `strong(dest a) == strong(dest a2)`; `metadata(dest a).nlink == Some(2)`; both claims `Created` with their own targets; log: exactly one `create_new` under `/p/dest/a`, one `create_hardlink(/p/dest/a2.flux-partial.<id>)`, then a rename of it onto `/p/dest/a2`, and no temporary left),
  `ten_members_are_one_copy_and_nine_links` (`a0..a9`; `files_hardlinked == 9`; all ten identities equal; `nlink == Some(10)`),
  `the_first_member_in_walk_order_is_the_one_copied` (members `b` and `a` in one directory: the physical copy is `a` (`create_new` under `/p/dest/a` happens, none under `/p/dest/b`); master §13),
  `two_groups_interleaved_link_to_their_own_canonicals` (`a1,a2` one object, `b1,b2` another, walk order `a1 a2 b1 b2` and also a layout `a b x y` with `a~x`, `b~y`: `a` and `x` share an identity, `b` and `y` share another, the two differ),
  `a_group_across_directories_links_up_the_tree` (`a` at the root, `sub/a2` below; `anchor_parent` is exercised: the link call's `other` is the root handle),
  `a_replaced_canonical_is_the_anchor` (pre-existing `/p/dest/a` with other bytes; `files_overwritten == 1`; the link's identity equals the NEW `a`'s),
  `a_member_whose_target_is_a_directory_fails_like_a_file` (`/p/dest/a2` is a directory; `code_of == DestinationError`; nothing linked; `files_hardlinked == 0`, `hardlinks_degraded == 0`),
  `a_failed_canonical_blocks_the_group` (`fs.fail_nth("rename_no_replace", 1, Code::IoError, Other)` so `a` fails to publish; `a2`, `a3` each reported `BlockedByCanonicalFailure`, the message names `a`; no `create_hardlink` call; `files_failed`-equivalent `failures.copy == 3`),
  `a_link_the_destination_cannot_hold_degrades_under_auto` (`set_hardlinks_unavailable(true)`: `files_copied` counts both, `hardlinks_degraded == 1`, `hardlinks_unlinked[&LinkRefused].count == 1`, the two destination identities differ),
  `a_link_the_destination_cannot_hold_fails_under_preserve` (`code_of == HardlinkUnavailable` for `a2`; `a` copied; `!exists(dest a2)`),
  `an_anchor_replaced_before_the_link_degrades` (a `before_mutation` hook (as 1210-1226) that, right before the second guarded mutation after `a` is published, replaces `/p/dest/a` by writing a new object at that path (`fs.remove_file` + `write_file`): `hardlinks_degraded == 1`, `a2` is a copy with its own identity, no link created),
  `a_temporary_that_is_not_the_anchor_is_removed_and_the_member_degrades` (a `before_mutation` hook that, at the guarded mutation right after `create_hardlink` (the publish of the link), gives the temporary another Strong identity with `set_identity`: the log shows `remove_file(<temp>)` and no rename of it; `hardlinks_degraded == 1`; `a2` is then an independent copy),
  `strict_durability_links_without_batching` (`Durability::Strict`, `BatchPolicy` default: the pair links; the claim store holds `Created` for both; no prepared note is left; the link's `create_hardlink` call precedes `sub/b`'s staging in the log),
  `a_lookup_failure_degrades_under_auto_and_fails_under_preserve` (`fs.fail_always("hardlink_index_get", Code::IoError)`: both members' lookups fail; `auto` → `hardlinks_degraded == 2`, both copied, two objects; `preserve` → both reported `HardlinkUnavailable`, neither created),
  `a_skipped_first_member_leaves_the_next_as_anchor` (`--skip-existing` with `/p/dest/a` pre-existing (its own object), `a2`, `a3` fresh: `files_skipped == 1`, `hardlinks_degraded == 1` (the skipped `a`, counted when `a2` became the anchor), `hardlinks_unlinked[&PolicySplit] == DegradedGroup { count: 1, example: "a".into() }` and no other cause present, `files_hardlinked == 1` (`a3` → `a2`), `files_copied` counts `a2` and `sub/b`),
  `a_destination_that_is_already_linked_is_skipped_without_degradations` (ruling 16; `--skip-existing`; `/p/dest/a` and `/p/dest/a2` pre-built as one object through the fake's `create_hardlink`: `files_skipped == 2`, `hardlinks_degraded == 0`, `files_hardlinked == 0`, the destination identities unchanged),
  `a_pre_linked_destination_with_a_new_third_member_counts_the_two_separate_ones` (as above plus a fresh `a3`: `a3` copies and becomes the anchor; `hardlinks_degraded == 2`, `files_hardlinked == 0`),
  `two_skipped_members_that_are_separate_objects_count_one_degradation_at_once` (`--skip-existing`; `/p/dest/a` and `/p/dest/a2` pre-existing as two objects: `hardlinks_degraded == 1` with example `a2`),
  `run/multi.rs`: `a_group_across_two_roots_links_in_one_operation` (roots `/s1` and `/s2`, `/s1/a` and `/s2/a` one object: `files_hardlinked == 1`, `strong(/p/dest/s1/a) == strong(/p/dest/s2/a)`).
  The POSIX same-object rename (step 3) is exercised in Task 7's crash-point-(d) test, the only live path that renames a temporary onto another name of its own object.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core --no-fail-fast`; FAIL.
- [ ] **Step 3: Implement** as specified. Keep `copy_one` readable: the registration after the two arms is one helper `register_outcome(cx, src_id, &target, published, failed, out)`; the candidate branch before the arms is one `match`.
- [ ] **Step 4: Run**; PASS. **Mutants:** (a) skip step (4)'s identity check: `a_temporary_that_is_not_the_anchor...` red; (b) register `Failed` as `Anchor`: `a_failed_canonical_blocks_the_group` red; (c) drop `&& !candidate` from `batching`: `strict_durability_links_without_batching` red; (d) count a linked member in `files_copied`: `one_pair...` red; (e) degrade at the skip point when `entry` is `None`: `a_destination_that_is_already_linked_is_skipped_without_degradations` red. Revert each.
- [ ] **Step 5: Gate and commit** (`feat(core): link hardlink group members to their canonical for fresh copies`).

---

### Task 7: Resume: rebuild the index from the walk, heal, converge

**Tier:** Sonnet.

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (the skip site 1696-1714; the policy skip 1740-1777 reads `entry`)
- Test: `run/tests.rs` (crash simulation: a panicking `before_mutation` hook under `catch_unwind`, as 1228-1246; then a run with `RunConfig { resume: true, .. }`)

**Interfaces:**
- At the skip site, when `r.status == Created && resumed_complete(..)` and the file is a `Candidate(src_id)` with `entry` as Task 6 computed it:
  1. `entry` is `None` or `Skipped { members, example, .. }` (or a lookup error under `auto`, which registers nothing): `h.with(|i| i.insert(&src_id, &Anchor { target, object: strong(meta.identity) }))`, counting `members` `PolicySplit` degradations with `example` as Task 6's registration does, then skip as today (`files_resumed`).
  2. `Anchor { object: Some(a), .. }` and `meta.identity == Strong(a)`: skip as today.
  3. otherwise (`Anchor` with another or no object, or `Failed`): do NOT return; fall through to the policy decision. With `Overwrite` the plan becomes `Replace` and Task 6's consult links (`Anchor`) or blocks (`Failed`); with `SkipExisting`, or `Update` finding it up to date, the member is skipped and counted `hardlinks_degraded` (Task 6's skip-point rule already does this).
  A member healed by rule 3 counts `files_hardlinked`, not `files_resumed`.
- Nothing else changes: a canonical that fails `resumed_complete` is recopied by the existing path and re-registered (upsert) with its new object by Task 6's registration.

- [ ] **Step 1: Write failing tests.** Fixture: a source pair `a`, `a2` plus `sub/b`; a first run interrupted by a hook that panics at the N-th guarded mutation (count the mutations of a full run first in the test and pick N), then `--resume`. Every crash test asserts the pre-resume destination state it relies on (which names exist, which are one object, which claims are `Created`) before it resumes, so N is verified rather than assumed. `crash_before_the_canonical_is_published_then_resume_links_everything` (N = `a`'s publish; after resume: one object behind `a` and `a2`, `files_hardlinked == 1`, `files_resumed == 0`), `crash_after_the_canonical_before_the_link_then_resume_links` (N = the link's create; after resume: `files_resumed == 1` (`a`), `files_hardlinked == 1`), `crash_after_some_links_then_resume_links_the_rest` (three members; N after the first link's publish; resume: `files_resumed == 2`, `files_hardlinked == 1`), `crash_between_the_link_rename_and_its_claim_then_resume_relinks_identically` (N = the claim write after the rename: inject `fs.fail("claim_insert", ..)`? A panic in the hook cannot sit between rename and claim since the claim is not a guarded mutation; instead model the state by hand: after a complete first run, delete the claim for `a2` from the fake store (`FakeClaimStore` test seam, or run the first copy with `fs.fail_nth("claim_insert", k, ..)` so `a2`'s claim is `ClaimNotRecorded`); resume: `a2` has no claim, is resolved as an existing file, the default overwrite replaces it by an identical link (log: `create_hardlink`, rename, step-3 `remove_file`), `files_hardlinked == 1`, both names one object), `a_completed_independent_copy_is_relinked_on_resume` (first run under `Hardlinks::Copy` completes; a second run with `resume: true` under `Auto` (the state is COMPLETED so a resume adopts nothing: build the prior as RESUMABLE instead: interrupt the first `Copy`-mode run after `a2` is published, then resume under `Auto`): `a` registers at the skip site, `a2` differs → relinked: `files_hardlinked == 1`, one object), `the_users_policy_wins_over_healing` (same prior, resume with `SkipExisting`: `a2` is left, `files_skipped == 1`, `hardlinks_degraded == 1`, `files_hardlinked == 0`), `a_recopied_canonical_heals_its_earlier_members` (prior: `a` and `a2` linked and completed; make the source `a` longer so `resumed_complete` fails; resume: `a` recopied (new object), `a2` met later differs → relinked to the new object: `files_hardlinked == 1`, `files_copied == 1`), `a_changed_walk_order_still_converges` (prior: `m` and `m2` linked and completed, interrupted at `sub/b`; before the resume the source gains `a`, a new first member of the same group: `a` copies first and registers; `m` (completed, another object) relinks to `a` by rule 3, and so does `m2`: `files_hardlinked == 2`, one object behind all three), `a_skipped_member_that_already_links_to_the_anchor_is_not_degraded` (prior: `a` and `a2` linked and completed, interrupted at `sub/b`; resume with `SkipExisting`: `a` registers at the skip site, `a2` meets rule 2: `files_resumed == 2`, `hardlinks_degraded == 0`, `files_skipped == 0`), `restart_never_reads_the_prior_index` (first run interrupted leaves `hardlinks.db`; a `restart: true` run: the log shows `remove_file(…/<prior id>/hardlinks.db)` only inside the prior's retirement, and the new operation's own `create_hardlink_index` under its own id), `a_strict_note_for_a_link_whose_temporary_still_names_the_object_is_committed_by_adoption` (built by hand as `renamed_with_a_stray_temp` does at `run/tests.rs:5033-5043`, but with the stray temporary a real alias of the target through the fake's `create_hardlink`: adoption commits the claim and removes the temporary, as recovery row 1 already does for a same-object stray name).
- [ ] **Step 2: Run**; FAIL.
- [ ] **Step 3: Implement** the three skip-site rules.
- [ ] **Step 4: Run**; PASS. **Mutant:** rule 2 compares identities with `!=`; `crash_after_the_canonical_before_the_link...` red (the canonical would relink to itself). **Mutant 2:** rule 1 registers `object: None`; `a_completed_independent_copy_is_relinked_on_resume` red (degrade instead of heal). Revert.
- [ ] **Step 5: Gate and commit** (`feat(core): --resume rebuilds the hardlink index from the walk and relinks independent members`).

---

### Task 8: Dry run

**Tier:** Sonnet.

**Files:**
- Modify: `crates/flux-core/src/dry_run.rs` (`PlannedAction` 56-68, `as_str` 70-82: `Hardlink` → `"would_hardlink"`)
- Modify: `crates/flux-core/src/tree.rs` (the dry branch 1782-1788; `dry_stop_file` 2497-2541; the resume skip site's `dry_resumed`)
- Modify: `crates/flux-cli/src/report.rs` (`planned_line` 486-494 needs nothing new; the `PlannedAction` match at 303-311 and `dry_run_summary_line` 417-421)
- Test: `run/tests.rs` dry tests; `crates/flux-core/tests/dry_run_std_fs.rs`

**Interfaces:**
- In the dry branch, for a `Candidate(src_id)` with `entry`: `Failed` → the blocked report, as live; `Anchor { .. }` → `out.files_hardlinked += 1`, `files_overwritten += 1` when `Plan::Replace`, report `Planned { object: File, action: Hardlink }` (via `dry_planned` with a new parameter, or a sibling `dry_planned_link`); `None` → `dry_stop_file` as today, then `insert(&src_id, &Anchor { target, object: None })`. `Weak` and `Unchecked` are Task 5's (no I/O). A dry run never calls `anchor_parent` or `create_hardlink`.
- The dry `--resume` skip site registers with `meta.identity` exactly as live (Task 7 rule 1 runs under `cx.dry_run` too; `dry_resumed` follows).
- `dry_run_summary_line` appends `, hardlink {n}` after `overwrite {}` when `files_hardlinked > 0`.

- [ ] **Step 1: Write failing tests.** `run/tests.rs`: `a_dry_run_predicts_one_copy_and_one_link` (`files_copied == 2`, `files_hardlinked == 1`, records: `a` `would_copy`, `a2` `would_hardlink`; no `create_hardlink`, no `create_new` under `/p/dest`), `a_dry_run_parks_its_index_under_the_temp_root` (`cfg.dry_temp_root = Some("/tmp-fake")` as the dry tests set it; the log shows `create_hardlink_index(/tmp-fake/flux-dry-<id>-hardlinks/hardlinks.db)` and nothing under `/p/dest/.flux`; the temp directory is gone afterwards), `a_dry_index_entry_carries_no_object` (`fs.hardlink_index_entries(<that path>)` read before the temp directory goes, through a `before_mutation`-free seam: run the dry walk with `dry_temp_root` pointing at a directory the test keeps, and assert the one entry is `Anchor { object: None, .. }`), `a_dry_run_reports_a_weak_group_degraded_without_io`, `a_dry_run_blocks_nothing_it_cannot_know` (a destination that cannot hold links: `set_hardlinks_unavailable(true)`; the dry run still predicts `would_hardlink`: the design says it cannot predict a link failure), `a_dry_resume_predicts_links_against_completed_canonicals` (prior interrupted after `a`; dry resume: `a` `would_resume`, `a2` `would_hardlink`). `report.rs` test: `planned_lines_carry_the_new_words` gains `would_hardlink`; `dry_summary_names_links` (`, hardlink 3`).
- [ ] **Step 2: Run**; FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run**; PASS. **Mutant:** the dry branch registers `object: Some(src_id)`; `a_dry_index_entry_carries_no_object` goes red; revert. **Mutant 2:** the dry dependent calls `dry_stop_file`; `a_dry_run_predicts_one_copy_and_one_link` red (`files_copied` would be 3); revert.
- [ ] **Step 5: Gate and commit** (`feat(core): dry run predicts hardlinks without destination I/O`).

---

### Task 9: Command line and report

**Tier:** Sonnet.

**Files:**
- Modify: `crates/flux-cli/src/main.rs` (`CopyArgs` 68-135: `--hardlinks` after `--links` 123-126; `LinksArg` is the model; `options()` 168-199; `tree_output` 421-470)
- Modify: `crates/flux-cli/src/report.rs` (doc 25-27; `Report` 29-66; `zero` 94-117; `tree` 122-143; `summary_line` 409-414; `tree_warning_lines` 382-386; tests 857-876 untouched)
- Test: `crates/flux-cli/src/report.rs` tests, `crates/flux-cli/tests/copy.rs`

**Interfaces:**
- `#[arg(long, value_enum, default_value_t = HardlinksArg::Auto)] hardlinks: HardlinksArg` with `enum HardlinksArg { Auto, Preserve, Copy }`; doc: "How a file with several names in the tree is copied: `auto` (the default) copies it once and links the other selected names to it, copying independently and reporting where a link is impossible; `preserve` fails a member that cannot be linked (`HARDLINK_UNAVAILABLE`, `HARDLINK_IDENTITY_UNAVAILABLE`); `copy` copies every name separately (§15)."
- `Report`: `pub files_hardlinked` (exists) ← `out.files_hardlinked`; new `#[serde(skip_serializing_if = "is_zero")] pub hardlinks_degraded: u64` ← `out.hardlinks_degraded`; new `#[serde(skip_serializing_if = "is_zero")] pub hardlinks_unchecked: u64` ← `out.hardlinks_unchecked.map_or(0, |g| g.count)`. Both placed after `mounts_skipped`. Doc comment 25-27 reads: "nothing reflinks or verifies, so those stay 0 and `verify_level` is `"none"`; `files_hardlinked` counts cut 12's links."
- `summary_line`: after `{} skipped` append `; {n} hardlinked` when `files_hardlinked > 0`, `; {n} hardlinks degraded` when `hardlinks_degraded > 0`, `; {n} hardlinks unchecked` when `hardlinks_unchecked > 0`, in that order.
- `hardlink_warning_lines(out: &TreeOutcome) -> Vec<String>`, appended in `tree_warning_lines` after `safety_warning_lines`: per weak volume `warning: reliable filesystem object identity unavailable on volume {volume:#x}; hardlink preservation degraded to ordinary copies ({count} files, e.g. {example}) - --hardlinks=preserve fails them instead`; the unavailable group with scope `where the filesystem reports no identity`; then one line per `hardlinks_unlinked` cause present, in `UnlinkedWhy` order: `warning: {count} hardlink members were copied independently (e.g. {example}): {cause} - --hardlinks=preserve fails them instead` with `{cause}` one of `the destination refused the link`, `the anchor could not be found or checked`, `the existing-file policy kept them apart from their anchor`, `the hardlink index could not be read or written`; then the unchecked group `warning: the filesystem reports no link counts ({count} files, e.g. {example}); hardlinks among them were not looked for`. Master §108's two lines are quoted verbatim inside the first.
- `several_flag_gate` unchanged (`--hardlinks` works with several sources).

- [ ] **Step 1: Write failing tests.** `report.rs`: `the_json_has_every_section_53_field_in_order` stays as is; `hardlink_counters_are_absent_at_zero_and_present_when_set` (serialize a `Report` with both counters 2 and 3: keys present after `mounts_skipped`... assert via `serde_json::Value` object; at zero: absent); `summary_line_names_links_and_degradations` (`copied 2 files (3 bytes), created 1 directories; 0 failed, 0 skipped; 1 hardlinked; 2 hardlinks degraded; 1 hardlinks unchecked`); `hardlink_warning_lines_follow_section_108` (one weak volume 0x7 count 2 example `a`, unavailable count 1 example `d`, `LinkRefused` count 4 example `l`, `PolicySplit` count 1 example `p`, unchecked count 3 example `u`: five lines in that order with the exact texts). `main.rs` tests (near 744): `options(&parse(&[])).hardlinks == Hardlinks::Auto`, `--hardlinks=preserve` → `Preserve`, `copy` → `Copy`. `tests/copy.rs` (real filesystem, every OS): `a_hardlink_pair_is_copied_once_and_linked` (`std::fs::hard_link`; `flux copy src dst --json`: `files_hardlinked == 1`, `files_copied` counts the rest; the two destination names share `std::fs::metadata` identity (`MetadataExt::ino` on Unix; on Windows compare `std::fs::read` and the `nlink` from `metadata` via `flux_platform::StdFileSystem.metadata(..).nlink == Some(2)`); stderr's summary contains `; 1 hardlinked`), `hardlinks_copy_mode_copies_every_name` (`--hardlinks=copy`: `files_hardlinked == 0`, two objects), `a_dry_run_says_would_hardlink`.
- [ ] **Step 2: Run** `cargo nextest run -p flux-cli --no-fail-fast`; FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run**; PASS. **Mutant:** map `hardlinks_degraded` from `files_degraded`; `summary_line_names...` stays green (a pure-report test), so pin the mapping in `tree_maps_the_hardlink_counters` (a `TreeOutcome` with `files_hardlinked 1, hardlinks_degraded 2, files_degraded 5` → the report's three values); red under the mutant; revert.
- [ ] **Step 5: Gate and commit** (`feat(cli): --hardlinks, hardlink counters, summary suffixes and warnings`).

---

### Task 10: Real-filesystem engine tests and the scaled stress tests

**Tier:** Sonnet.

**Files:**
- Create: `crates/flux-core/tests/hardlinks_std_fs.rs` (model `tree_std_fs.rs:1-25`, but through `flux_core::run::tree` with a `RunConfig` as `dry_run_std_fs.rs` builds one, so the slot and claims exist)
- Test only.

- [ ] **Step 1: Write the tests** (master §69 scaled to CI): `one_pair` (one object, `nlink == Some(2)` read through `StdFileSystem.metadata`), `ten_members` (`nlink == Some(10)`, `files_hardlinked == 9`, `bytes_copied` equals one file's length), `a_huge_tree_of_two_thousand_pairs_links_every_pair` (2000 directories `d0000..d1999`, each with `f` and `g` linked: `files_hardlinked == 2000`; every pair one object; runtime budget: the test completes under the suite's existing slowest test, measure once and note the figure in the test's comment), `a_group_across_directories_is_one_object_on_a_real_filesystem` (`z/f` and `a/f` linked: `files_copied == 1` for the group, both destination names one object), `a_resume_after_an_interrupted_run_links_the_rest` (first run through `run::tree` with a `before_mutation` hook that panics after the canonical's publish, under `catch_unwind`; second run `resume: true`: one object, `files_hardlinked == 1`), `the_symlink_sibling_of_a_group_is_untouched` (a group plus a symlink in the tree: cut 10a behaviour unchanged). A destination that cannot hold hardlinks has no portable real-filesystem fixture in CI; that row is covered by the fake (Task 6) and recorded as an accepted boundary in Task 11.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core --test hardlinks_std_fs --no-fail-fast`; PASS on Linux; push the draft PR and read the macOS and Windows jobs: PASS. A Windows failure here is a real finding (the Windows arm of Task 3 or an NTFS semantic); fix in the task that owns the code, re-run.
- [ ] **Step 3: Commit** (`test(core): real-filesystem hardlink tests (section 69 scaled)`).

---

### Task 11: Documentation and the rulings

**Tier:** Haiku (specified text edits), Sonnet for the design's ledger wording.

**Files:**
- Modify: `ROADMAP.md:43` (Phase 4: "hardlink canonicalization and dependency-aware links for fresh copies and `--resume` are done (cut 12, PR #N); the persistent topology store is deferred until parallel workers; excluded members wait for `--exclude`")
- Modify: `README.md` (Commands section 55-62: a `flux copy --hardlinks=auto|preserve|copy` paragraph under the `flux copy` line: default `auto`, what each does, the report counters; `README.md:29` stays)
- Modify: `TODO.md:241` (note: "the walker now has hardlink topology for regular files (cut 12); a hardlinked symlink is still N independent links"), `TODO.md:363` item 8 (note: "cut 12's link path records the alias's claim through the same protocol; the case-insensitive alias case is unchanged and untested")
- Modify: the design: a "Plan rulings" subsection under "Calls made by the author" listing rulings 1-14 above, the correction of the sweep/cleanup claim (ruling 1) in "What exists today", and "Stand-downs" if any arose.
- Modify: `docs/accepted-boundaries.md`: one entry, "a destination that cannot hold hardlinks (FAT, some shares) is exercised through `FaultFs::set_hardlinks_unavailable` only; no CI runner offers such a destination" with the Task 6 tests as the compensation.

- [ ] **Step 1: Edit** the files as specified.
- [ ] **Step 2: Run** `typos` and the whole gate; PASS.
- [ ] **Step 3: Check scope on both axes:** `git status --short` (nothing untracked that is not meant to land) and `git log --stat <base>..HEAD` (every file in the File structure table, nothing else).
- [ ] **Step 4: Commit** (`docs: hardlink preservation (cut 12) roadmap, README, TODO, plan rulings`).

---

## Stand-downs

None. Panel record (2026-10-26): round 1 (solo seats plus agy) folded one finding raised through an open question (the `Skipped` entry, ruling 16); round 2 (agy, Blindspot Auditor rotated in) landed `PANEL VERDICT: GREEN` with two operator-facing observations, both folded afterwards (one warning line per degradation cause; the skipped member as the example). Those two folds were not re-reviewed by the peer.
