# Cut 8b: replacement, claims and the policy flags

Status: draft for review (2026-10-07). Branch `spec/cut-8b`, from `main` dc951e5 (cut 8a merged).

## Why

Spec section 5.1 makes `--overwrite` the default for an existing destination file. Directories do not meet it today:
`copy_tree` publishes every file with `Publish::NoReplace` (`walk_into` in `crates/flux-core/src/tree.rs`) and reports an
existing file `DESTINATION_NAMESPACE_COLLISION` (`TODO.md`, "A tree copy never replaces an existing destination file").
Replacement needs section 241.5's claim records, and those need the operation's durable `state.db` (section 119). Cut 8a
made the merge safe; cut 8b makes it replace.

Cut 8b delivers four things:

| Part | What |
|---|---|
| R | Replacement of an existing destination file by a directory copy, through `rename_replace`. |
| C | The claim records of section 241.5 in a durable `state.db`, behind a `ClaimStore` interface. |
| N | Name resolution: how the engine learns the stored name of an existing entry on a destination that folds case or normalization. |
| F | The policy flags `--overwrite` (default), `--update`, `--skip-existing` and the report counts they fill. |

## Decisions

Each fork went to agy first (scoping consult, two negotiation rounds) and the owner approved the result; the Q8 fork was
settled by a measured spike on Linux, macOS and Windows. The reasoning is under "Design record".

1. **Scope: one cut.** A minimal versioned `state.db` holding only claims, replacement and the three flags. The WAL
   (`PREPARE_COMMIT` / `COMMIT`, sections 182-184), commit recovery and resume stay for the later WAL cut. Until then a
   crash between a publish rename and its claim write leaves a published entry with no claim, and the filesystem's own
   no-replace refusal is the only protection for that entry.
2. **Store: a `ClaimStore` interface** with an indexed on-disk implementation behind it. A flat append-only file is
   rejected (no key index, so lookups scan or the memory bound of section 10.1 breaks). The backend is chosen by the rule
   in "The store and its backend".
3. **Claim protocol:** an insert-if-absent claim of the existing entry before the rename; after the rename the same
   record is upgraded to "created"; when the planned name differs from the stored name a second "created" claim is added
   for the planned name. Claims are never released.
4. **Replacement mechanics:** a regular file is replaced with `rename_replace`; a symlink at the name is replaced as the
   link itself; a directory where a file is expected, or the reverse, fails that target; a read-only file stays refused
   (`rename_replace` already refuses it). No unlink-then-rename.
5. **Flags:** all three, mutually exclusive (exit 2); `--update` compares metadata only; `files_overwritten`,
   `files_skipped` and `bytes_skipped` are filled for directories.
6. **Lifecycle:** `state.db` is created in the unpublished workspace `<id>.creating`, removed with the workspace and by
   every unwind; a kept workspace keeps it.
7. **Verification:** one conformance suite run against the in-memory fake and the real store; the TLA+ `claims` scenario
   stays planned.
8. **Name resolution (Q8):** see "Name resolution".
9. **Durability:** a claim write is committed without a sync under `Durability::Normal` and synced under
   `Durability::Strict`.
10. **The lockless `copy_tree` stays no-replace.** It has no workspace, so it cannot hold claims.
11. **Single-file copies honour the flags** (section 5.1: the policy applies per file target).
12. **`FaultFs` gains a case-insensitive mode**, so name resolution is testable without a real filesystem.

## Components

| Unit | Crate | Responsibility |
|---|---|---|
| `ExistingPolicy` (`Overwrite`, `Update`, `SkipExisting`) | `flux-fs` (`CopyOptions`) | The per-file policy. Default `Overwrite`. |
| `ClaimKey`, `ClaimRecord`, `ClaimStatus`, `ClaimOutcome`, `ClaimStore` | `flux-fs` | The data model and the interface (below). |
| `DirHandle::Claims` and `DirHandle::create_claim_store` | `flux-fs` trait; implemented in `flux-platform` and by the fake | Create `state.db` through the workspace handle and return a store. |
| The real store | `flux-platform` (new module) | The indexed on-disk store over the file the handle created. |
| The in-memory store | `crates/flux-core/src/fault_fs.rs` | Deterministic, fault-injectable store for engine tests. |
| `NameIndex` (per-directory frame state) | `flux-core` (`tree.rs` or a new `names.rs`) | Name resolution and the identity table. |
| The replacement path | `crates/flux-core/src/tree.rs` (`copy_one` and its callers) | Policy, claim, copy, publish, claim upgrade. |
| Workspace changes | `crates/flux-core/src/state.rs`, `run/place.rs` | Create and remove `state.db`. |
| Flags and report | `crates/flux-cli/src/main.rs`, `report.rs` | The three flags and the report fields. |

## The claim model

```text
ClaimKey    { parent: ObjectId, name: Vec<u8> }   // the parent directory's Strong identity; the entry name bytes as stored
ClaimStatus { Existing, Created }
ClaimRecord { target: FluxPathKey, status: ClaimStatus }   // FluxPathKey: section 103
ClaimOutcome{ Inserted, Present(ClaimRecord) }
```

`ClaimStore` (no default methods):

- `insert_if_absent(&mut self, key, record) -> Result<ClaimOutcome>`: writes the record if the key is absent;
  otherwise returns the record already there.
- `get(&self, key) -> Result<Option<ClaimRecord>>`.
- `upgrade_own_claim(&mut self, key, target) -> Result<()>`: the record at `key` must exist and have `target`; its
  status becomes `Created`. A missing key or a foreign owner is an error (an engine bug, never a collision).

"Same target" is a bytewise comparison of `FluxPathKey` (section 103), never an identity (hardlinks share identity,
section 241.5). A target that finds its own claim proceeds, so a resumed target never collides with itself.

A directory whose identity is not `Strong` cannot key claims, so NO claim is written for any target in it, new or
replacing: every step of the per-file protocol below that inserts, upgrades or checks a claim is skipped there, and the
frame's name tracking is skipped too. Its targets use no-replace publication, an existing file is
reported `DESTINATION_NAMESPACE_COLLISION` as today, and `TreeOutcome.replace_degraded` (a `DegradedGroup`, rendered like
the other degraded warnings) counts such directories. Under `--safety strict` that directory's subtree is skipped as
`SAFETY_REJECTED`.

## Interfaces

```rust
// flux-fs
pub enum ExistingPolicy { Overwrite, Update, SkipExisting }          // field `existing: ExistingPolicy` in CopyOptions, default Overwrite
pub trait ClaimStore {
    fn insert_if_absent(&mut self, key: &ClaimKey, record: &ClaimRecord) -> Result<ClaimOutcome>;
    fn get(&self, key: &ClaimKey) -> Result<Option<ClaimRecord>>;
    fn upgrade_own_claim(&mut self, key: &ClaimKey, target: &FluxPathKey) -> Result<()>;
}
pub trait DirHandle {
    type Claims: ClaimStore;
    /// Create `name` exclusively through this handle (read and write, never following a link) and open a store on it.
    /// `Durability::Normal` commits without a sync; `Durability::Strict` syncs every commit.
    fn create_claim_store(&self, name: &OsStr, durability: Durability) -> Result<Self::Claims>;
    // existing methods unchanged
}
pub struct Outcome { /* existing fields */ pub skipped: bool }

// flux-core (copy.rs)
pub type BeforeCreate<'g> = dyn Fn() -> std::result::Result<(), CopyError> + 'g;   // beside Guard and Heartbeat
pub enum CopyStep { /* existing */ Claim }                            // the claim step
// A claim collision is Err(CopyError::at(CopyStep::Claim, FsError::new(Code::DestinationNamespaceCollision, ..)));
// a store error is Err(CopyError::at(CopyStep::Claim, <the store's FsError>)).
```

**Key layout (the real store).** One table of byte-string keys and byte-string values. Key: the parent `ObjectId` as
`volume` (u64, big endian) then `index` (u128, big endian), then the entry name bytes (`OsStr::as_encoded_bytes`).
Value: one status byte (`0` Existing, `1` Created) then the `FluxPathKey` bytes. A second table, `meta`, holds the schema
version (`format` = 1). The fake stores the same keys and values in a `BTreeMap`.

## The store and its backend

- The store is one file, `state.db`, in the operation workspace. A backend that needs auxiliary files is ineligible.
- **The file is created through the workspace `DirHandle`**, exclusively, read and write, never following a link
  (`DirHandle::create_claim_store`), and handed to the backend as an already-open file. This keeps the engine's rule
  that writes under DEST go through directory handles (section 149.7). A backend that can only open a file by path is
  ineligible: this rules out SQLite, which opens by path and keeps a journal beside the database.
- The first candidate is `redb` (pure Rust, copy-on-write B-tree, accepts an open file). It is the default unless the
  first task of the plan shows it fails the acceptance test: a child process makes claim commits under `Durability::Normal`
  and is killed (`std::process::Child::kill`: SIGKILL on Unix, `TerminateProcess` on Windows) at several points, including
  in the middle of a commit; after each kill the store is reopened and must open, and the claims present must be a
  PREFIX of the commit sequence (if commit k is present, all commits before k are), with every commit the child reported
  complete before the kill present. The test runs on the Linux, macOS and Windows CI legs. The same first task confirms
  that the pinned `redb` version builds a database from an already-open `std::fs::File` (not only from a path) on all
  three systems; that cannot be determined from here. If it fails, the
  plan stops and brings the owner a new fork (a purpose-built indexed store, or a different backend); it does not
  silently lower the durability claim.
- Resident memory is bounded (section 10.1): the backend's cache is set to a small fixed size, and the engine holds no
  claim set in memory.
- A schema version is stored in the file (a `meta` entry); `create_claim_store` writes it and a later open checks it.
  Nothing reopens a store in this cut (no resume), but the version is written now.
- `cargo deny` must pass (licence MIT or Apache-2.0 is allowed); `Cargo.lock` gains the dependency; the CI legs on
  all three systems build it.

## Name resolution

Claims are keyed by the entry name as the filesystem stores it (section 241.5). On a destination that folds case or
normalization a planned name can resolve to an entry stored under another spelling. Measured: the stored name cannot be
read from an open handle on any system (Linux returns a cached spelling; macOS `F_GETPATH` was wrong after a replace);
the directory listing always shows stored names; the name an entry has after `rename_replace` is the OLD name on Linux
and macOS and the NEW name on Windows. So:

1. **Listing.** When the walk enters a PRE-EXISTING destination directory it reads the listing (`DirHandle::read_dir`) into
   the directory's frame (`Frame::Live`); a directory this run created needs none (it is empty). The listing is dropped
   at the directory's `DirEnd`; memory is bounded by one directory's width, as the walk already bounds the source side.
2. **Resolve a planned name P:**
   - P is an exact entry of the listing: the stored name is P.
   - Otherwise `metadata(P)` is `NotFound`: P is absent (planned as new).
   - Otherwise P exists under another spelling: resolve by IDENTITY. The frame holds an identity-to-stored-name table,
     built lazily at the first such hit (one `metadata` call per listing entry, once per directory). A unique match gives
     the stored name. No match, or several (hardlink aliases in one directory), makes THAT target fail
     (`DESTINATION_ERROR`, "cannot determine the stored name"); the run continues.
3. **Keep the frame current.** After a target publishes, the frame records the published entry's spelling(s) in the
   listing set and ties them to the target that wrote them, and updates the identity table:
   - the replaced object's identity (the old `M.identity`) is removed from the table (that object is gone);
   - the published object's identity (`Outcome.published_identity`) is added, mapped to every spelling this target
     claimed: the stored name `E` and, when different, the planned name `P` (on Windows only `P` exists on disk
     afterwards, but the frame cannot know that without a query, so it keeps both).
   When `published_identity` is `Unavailable` the table gains nothing, and a later target that needs that entry fails
   safely (below).
   **Resolution of several matches.** An identity that maps to several spellings is ambiguous (hardlink aliases) EXCEPT
   when every one of the spellings is tied to a single target of this run: then they are one entry, any of them resolves
   it, and the target's claim on any of them makes the later target a collision. Any other several-match makes that target
   fail with `DESTINATION_ERROR`, as before.
4. **After a replacement** the engine claims both the stored name used before the publish and the planned name (one key when
   `P` equals `E`, two keys otherwise, on every platform: a differing planned spelling is claimed too, so a later target
   that resolves to either spelling collides), and does not query the name afterwards.

No case-folding or Unicode-normalization table is used, so no dependency is added; the cost on a case-sensitive
destination is the one listing per pre-existing directory.

## The per-file protocol

For a file target with planned name P in destination directory D (a `Frame::Live`), source metadata `S`:

1. **Resolve P** (above). Result: absent, or an existing entry with stored name `E` and metadata `M`.
2. **Absent:** plan as new. Publish with `rename_no_replace`; `AlreadyExists` is `DESTINATION_NAMESPACE_COLLISION`
   exactly as in cut 4b. After the rename, when D has a `Strong` identity, insert a `Created` claim for `(D.identity, P)` and update the frame
   (Name resolution, step 3); in a replace-degraded directory (see "The claim model") neither happens.
   A failure of that claim write is reported as a claim failure (below): the file IS published and is counted.
3. **Existing, a directory:** fail the target (`DESTINATION_ERROR`), whatever the policy: nothing can be placed there.
4. **Existing, a file or symlink:** apply the policy.
   - `SkipExisting`: no claim; the target is skipped (`files_skipped += 1`, `bytes_skipped += S.len`, the SOURCE length: the bytes the engine chose not to transfer).
   - `Update`: replace only when `S.modified > M.modified` or `S.len != M.len`; when either modification time is
     unavailable, compare lengths only (equal lengths: skip). Otherwise skipped as above.
   - `Overwrite`: replace.
5. **Replace.** The order follows the existing copy path (`copy_file_guarded`): the section 129 identity gate (Step 2a)
   runs first, the heartbeat and the section 99 guard run before every mutation under DEST, and the claim is a mutation
   of `state.db` under DEST, so it comes AFTER the gate and after one heartbeat and guard call. The copy path therefore
   gains a `before_create` callback (beside `guard` and `beat`), called after the gate and the heartbeat and guard that
   precede the exclusive create, and before that create; the single-file path passes a no-op.
   1. The identity gate runs (unchanged). A refusal stops the target with nothing claimed.
   2. Heartbeat, then the section 99 guard (unchanged). A failure stops the whole operation as today, with nothing claimed.
   3. `before_create`: `insert_if_absent((D.identity, E), { target, Existing })`. A returned record with another target:
      `DESTINATION_NAMESPACE_COLLISION`, nothing copied, nothing published. A record with this target: proceed.
   4. The temporary is created and written, metadata applied, the source re-checked (unchanged).
   5. `rename_replace(temp, D, P)`.
   6. `upgrade_own_claim((D.identity, E), target)`; if `P != E` also `insert_if_absent((D.identity, P),
      { target, Created })` (a returned foreign record here is an engine bug and is a claim failure).
   7. `files_copied += 1`, `files_overwritten += 1`; update the frame (Name resolution, step 3).
7. **A later target that resolves to an entry some earlier target claimed** hits step 5.3 and is reported
   `DESTINATION_NAMESPACE_COLLISION`.

The claim (5.3) precedes the copy so a colliding target copies no bytes. If a later step of the same target fails
(a metadata or heartbeat failure, the source re-check, the rename), the claim stays: it is never released, and a later
target that resolves to the same entry is a collision.

**Claim failures.** A store error BEFORE the rename (step 5.3) fails that target with the store's error, nothing
published. A store error AFTER the rename (step 2's created claim; steps 5.6) leaves the file published: it is counted
as copied (and as overwritten for a replacement) and reported as a new failure cause,
`TreeFailureCause::ClaimNotRecorded(FsError)`, with its own `FailureTally` counter; it makes the run exit 1 like any
streamed failure, and the report's `errors` includes it. The destination is not misreported as unchanged.

The run continues after any single target's claim failure.

## Policy and flags

- `ExistingPolicy` lives in `CopyOptions`; the default is `Overwrite`.
- The CLI adds `--overwrite`, `--update`, `--skip-existing` as a clap group that allows at most one (clap's usage error
  exits 2, section 5.1). `--atomic` does not exist yet, so the section 5.1 combination error is not implemented here.
- A single-file copy applies the policy through the same decision (step 4): `Skip` leaves the target untouched and
  `Outcome` gains `skipped: bool` (true: `bytes_copied` is 0 and nothing was published). `Report::file` sets
  `files_skipped` from it and `files_overwritten` from "existed and was replaced".
- `TreeOutcome` gains `files_overwritten`, `files_skipped`, `bytes_skipped`, `replace_degraded`. `Report::tree` maps
  them to the existing section 53 fields. A skipped file is not a failure and does not change the exit status.
- The lockless `copy_tree` ignores the policy for replacement and keeps no-replace; the CLI always goes through
  `run::tree`.

## Workspace lifecycle

- `TreePlace::create` (`run/place.rs`) order: `begin_workspace`, the cut 8a probe, `create_claim_store("state.db")`, then
  `publish_workspace`. The store handle lives in the run's `Locked` state and is passed to the walk through `Shared`
  (`None` for the lockless copy).
- The store is dropped (closed) BEFORE any removal of the file (Windows).
- `retire_workspace`, `unwind_creating` and the probe's error path remove `state.db` (a constant beside the probe names
  in `state.rs`). A kept workspace (FAILED, a leftover temporary) keeps it. `--restart`'s supersede retires a prior
  operation's workspace, which removes its `state.db`.
- `crates/flux-core/src/prior.rs` ignores the file (it reads the manifest); the manifest does not change.

## Durability and crash semantics

| Event | Result |
|---|---|
| Process crash under `Normal` | Committed claims survive (to be proven by the acceptance test above). |
| Power loss under `Normal` | Un-synced claims may be lost, like the un-synced data. |
| Any crash under `Strict` | Every claim write is synced; a claim never trails its rename by more than the rename-to-claim window. |
| Crash between a NEW target's publish rename and its created claim | A published entry with no claim, until the WAL cut reconciles it. Documented limit. |
| Crash between a REPLACEMENT's `rename_replace` and its claim upgrade | The key holds the target's `Existing` claim (and the planned-name claim, if it differs, is absent): the entry holds the new content, a later target that resolves to it still collides through the `Existing` key, and the missing `Created` status is reconciled by the WAL cut. |
| Store I/O error on a claim after the rename | The file is published and counted; the run reports `ClaimNotRecorded` and exits 1. |
| Store I/O error on a claim before the rename | That target fails with the store's error; nothing was published; the run continues. |
| Store corruption on open | Not reachable in this cut (the store is always created fresh). |

## Testing

- **Conformance suite:** one set of tests against the `ClaimStore` trait, run against the in-memory fake and the real
  store: insert-if-absent, present, own claim, foreign claim, upgrade, upgrade of a missing key, the same name bytes with
  different parents, a large number of keys without unbounded growth.
- **`FaultFs` case-insensitive mode** (`set_case_insensitive(true)`): name lookup folds case; a rename over an entry
  keeps the old stored name; a listing returns stored names; used for every name-resolution test, including the
  Windows-style new-name outcome (`set_replace_renames(true)`).
- **Engine tests (fake):** replace; skip; update (newer, older, same size, different size, unavailable mtime); two
  source names folding onto one existing entry (the second is a collision); a later target after a Windows-style
  rename; a directory in the way; a read-only destination; a weak parent identity; a claim write failure; a store
  that fails the upgrade.
- **Real-system tests:** replacement on the default filesystem of every CI system; on Linux CI a loopback vfat image
  (`sudo mount`, as the cut 8a bind-mount test does; a missing facility fails on CI and skips locally).
- **Windows junction at the destination name:** a real-system test replaces a file over a junction at the same name and
  records the outcome (failure of that target, or replacement of the junction itself); it must never write through
  the junction's target.
- **Crash test:** the CLI's existing stall hook (`FLUX_TEST_STALL_AT`) kills a run after k files, the test reopens the
  store and counts claims.
- **Mutant proof:** every new test must go red under a one-line mutant of the code it guards, measured by the driver.

## Known limits

1. A crash between a publish rename and its claim write leaves an unclaimed published entry until the WAL cut.
2. Resume, `PREPARE_COMMIT` / `COMMIT` and commit recovery (sections 182-184), hardlink groups (section 253.7),
   `--atomic`, `--dry-run` and the TLA+ `claims` scenario are not in this cut.
3. WSL 9p and network filesystems: whether the backend's file locking works there is unmeasured.
4. A directory with millions of entries holds its listing in memory while the walk is inside it (the source side
   already does).
5. A replacement reads the destination's metadata twice (the policy decision, then the section 129 gate): one extra stat
   per replaced file, accepted. If an outside process removes the destination entry between those two reads the gate sees
   `NotFound`, the claim is still written for the vanished entry and the file is published as new but counted as an
   overwrite: the same bounded window the gate already documents (`copy.rs`, `identity_gate`). It never overwrites
   anything the engine did not plan to replace.
6. The claim insertion (inside the copy path, `before_create`) and the claim upgrade and frame update (in `tree.rs`, after
   the copy path returns) are in two modules; this is accepted to keep the claim after the gate and guard.
7. On case-insensitive Windows an existing directory reparse point (a junction) at the destination name is reported by
   `metadata` as a link; whether `rename_replace` of a file over it fails or replaces the junction itself is unmeasured.
   The real-system tests measure it (below); the required outcome is a per-target result, never a traversal of the junction.
8. On a case-insensitive destination, a hardlink alias of an existing entry in the same directory makes the
   alias's target fail rather than replace (no unique identity match).

## Out of scope

The WAL and recovery; resume; live progress indicators (not built in the engine yet); `--atomic` (including its usage error with `--update` and `--skip-existing`); dry run; the
cross-filesystem and mount-boundary cut; directory replacement; reflink or hardlink fast paths.

## Design record

- **Scoping consult.** agy picked one cut, a custom append-only file, claim-before and claim-after rename, replace by
  `rename_replace`, all three flags, state.db in `<id>.creating`, a fake plus a shared conformance suite. The driver
  agreed on all but the store and the claim-key detail.
- **Negotiation round 1.** On the store, agy conceded that an append-only file has no index and so breaks section 10.1
  (spec lines for "never held wholesale in memory"), and accepted a `ClaimStore` interface over an existing indexed
  store. On the claim protocol the driver showed that the existing-entry claim and the created-entry claim have the SAME
  key, so the second write is an upgrade and not a second insert-if-absent; agy specified the `Existing` to `Created`
  transition and the second claim for a differing spelling.
- **Negotiation round 2 and the spike.** agy picked a directory listing on entry. The driver found that matching a
  planned name to a listing entry needs a fold rule, and proposed identity matching instead. The spike measured the
  platforms:

  | | Linux (ext4 casefold, vfat, exfat) | macOS (APFS) | Windows (NTFS) |
  |---|---|---|---|
  | Handle-to-path returns the stored casing | no (dentry-cached spelling) | at first lookup; wrong after a replace | yes (also `FindFirstFileW`) |
  | Listing shows stored names | yes | yes | yes |
  | Name after `rename_replace` over another spelling | old kept | old kept | new taken |
  | Lookup by another spelling | succeeds, same identity | succeeds | succeeds |
  | NFC and NFD equal | only ext4 casefold | yes | no (both coexist) |

  Consequence: the handle query is not a portable oracle, so the listing plus identity matching is used, and both
  spellings are claimed after a replace.
- **Backend and file handles.** Writing the spec showed the backend must accept an already-open file so that `state.db`
  is created through the workspace handle (section 149.7). SQLite opens by path and keeps a journal, so it is ineligible;
  `redb` is the first candidate. This narrows the owner-approved "redb or SQLite" to redb plus the acceptance test.
- **Clarifications for section 241.5** (the spec is silent on both): the created-entry claim of a replacement is an
  upgrade of the target's own record, and a differing spelling after publication adds a second claim. Both are proposed
  back to the spec text.

End of the cut 8b design.
