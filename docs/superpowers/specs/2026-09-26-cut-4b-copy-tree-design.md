# Cut 4b: `copy_tree`, the safe engine - design

**Status:** owner-approved 2026-09-26 (brainstorming, section by section). **Base:** `main` at `4449857`
(PRs #48 cut 4a, #50 prep, #51 tool table merged). **Branch:** `spec/cut-4b`.

**Authorities, in order.** This document settles what the two below leave open and wins where it speaks.
`2026-09-25-cut-4-after-handle-relative-writes.md` (the amendment; decisions 2, 3, 7, 8 are this cut's)
wins over `2026-09-23-directory-walker-design.md` (sections "Object identity" through "The destination may
rename what you asked it to create", and Delivery item 4). Where all three are silent, the merged code wins
on shape. What 4b does NOT take on is fixed by `2026-09-26-sequencing-after-cut-4a-design.md`.

## The four forks this cut settles

Each was put to the agy peer (AGY-FIRST, brief `.clavity/seams/cut4b-forks.md`) and decided by the owner.

| # | Fork | Decision | Peer |
|---|---|---|---|
| F1 | How `copy_tree` learns a failure came from the publish step | `CopyError` carries the failing step (`CopyStep`) | same pick |
| F2 | A file published with metadata complaints inside a tree | a new cause `PublishedWithComplaints`, streamed and tallied | same pick; warned "failure" could read as "file missing", answered by the name |
| F3 | A destination file that already exists | never replaced: `DESTINATION_NAMESPACE_COLLISION`, refused at Step 2a before any bytes are copied | same pick |
| F4 | Where the `DirHandle` identity method lands | inside 4b, as the plan's FIRST task with its own commit and per-platform tests | converged after one AGY-NEGOTIATE round (peer first picked a separate PR) |

**Why F3 refuses rather than replaces.** §5.1's default is `--overwrite`, but §241.5 serializes a target
"planned as a replacement" with a durable claim in `state.db`, the operation workspace no cut builds yet.
Replacing without claims lets two source names that fold onto one existing entry replace each other
silently. Until the claim store exists, 4b never modifies an existing destination file, and says so.
`flux copy dir existing-dir` therefore reports one collision per pre-existing file (exit 1 in cut 5).

## API (`crates/flux-core/src/tree.rs`, new; exported from `flux-core`)

```rust
pub fn copy_tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<TreeOutcome, CopyError>;
```

- **Outer `Err`** = the operation as a whole did not happen or had to stop: the source root missing or not
  a directory; the lexical floor; the §129 pre-flight; a per-directory anchor match; a destination root that
  cannot be resolved or created; and decision 3's first publish reporting the primitive unavailable. Every
  other failure goes to `on_failure` and the walk continues.
- **`TreeOutcome`**: `files_copied: u64`, `bytes_copied: u64`, `directories_created: u64`,
  `failures: FailureTally`, `warnings: WeakIdentityWarnings`. `WeakIdentityWarnings` and `DegradedGroup`
  are exactly the walker design's (grouped by `ObjectId::volume` for `Weak`, one bucket for `Unavailable`,
  one example path each, source-relative).
- **`TreeFailure { path: PathBuf, cause: TreeFailureCause }`**, `path` relative to the source root.
- **`TreeFailureCause`**: `Walk(FsError)`, `CreateDir(FsError)`, `Copy(CopyError)`,
  `Unsupported(FileType)`, **`PublishedWithComplaints(Vec<MetadataFailure>)`** (F2: the file IS at the
  destination; its metadata was not all applied). One failure, one representation: each variant is defined
  by where it happened, as in the walker design.
- **`FailureTally`**: one counter per variant (`walk`, `create_dir`, `copy`, `unsupported`,
  `published_with_complaints`), incremented by an exhaustive `match` with NO wildcard arm; `total()` is
  derived, never stored.
- **`CopyError` gains `step: CopyStep`** (F1), set by `copy_file_at` where it fails:
  `Resolve` (the wrapper's destination handling), `Source` (Step 2 source metadata / type / open),
  `Gate` (Step 2a), `Create` (Step 3), `Stream` (Step 4), `Durability` (Step 5), `Metadata` (Step 6),
  `Recheck` (Step 7's source re-check), `Publish` (Step 7's rename). A construction site that has no step
  (the wrapper's lexical Step 0) uses `Resolve`. The exact variant list is final here; the plan may not add
  or merge variants without coming back to this document.
- **`DirHandle::identity(&self) -> Result<FileIdentity>`** (decision 2, F4): POSIX `fstat` on the descriptor
  through the existing `metadata_from_stat`; Windows `GetFileInformationByHandleEx(FileIdInfo)` through the
  existing `identity_of_handle`, generalized to borrow any raw handle; `FaultFs` from the node. Returns the
  identity of the directory the handle holds, never of a path.

## The algorithm

1. **Source root.** `walk(fs, src_root)` refuses a missing or non-directory root; that error is the outer
   `Err`, before any destination call.
2. **Lexical floor, before any filesystem call on the destination.** If `dst_root` equals `src_root` or is
   lexically inside it (component-wise, no filesystem access), refuse with `Code::SafetyRejected`. Runs at
   every identity strength.
3. **Resolve the destination.**
   - `dst_root` exists: `root = fs.destination_root(dst_root)` (it may follow a link: the user named it). A
     non-directory is the outer `Err` carrying `destination_root`'s error unchanged: `Code::IoError` with
     `ErrorKind::NotADirectory` on every arm (`crates/flux-platform/src/std_fs.rs`, the Windows
     `destination_root`'s is-directory check; POSIX's `ENOTDIR`; `FaultFs` likewise). The engine does not
     remap it. The anchor is `root.identity()`.
   - `dst_root` absent (`destination_root` fails with `e.source.kind() == ErrorKind::NotFound`): split it as `copy_file` does (a root or a name
     ending in `..` is `DESTINATION_ERROR`; an empty parent is `.`), `parent = destination_root(parent)`,
     anchor `parent.identity()`. The root is created in step 5, after the pre-flight.
4. **§129 pre-flight.** Compare the source root's identity with the anchor's. Both `Strong` and equal:
   outer `Err`, `SafetyRejected`. Either side not `Strong`: record the weaker side in `warnings` and
   continue under `Safety::Default`; outer `Err` under `Safety::Strict`. Nothing has been created yet.
   Because the anchor is the RESOLVED destination's identity, a destination symlinked to the source is
   caught here by identity; the walker design's "refuse a `Symlink` anchor" rule is obsolete (decision 2).
5. **Create the root if absent** by decision 8 on `parent` (below), with every failure the outer `Err`. On
   success the root handle's identity joins the fold-collision set if `Strong`, and `directories_created`
   counts it only if `create_dir` made it.
6. **Walk**, keeping a stack of destination handles parallel to the walk (decision 7), the root at the
   bottom, and a set of skipped relative prefixes.
   - **`Dir { path, identity }`** (skipped if under a skipped prefix, which is then pushed for its
     `DirEnd`):
     - If `identity` and the destination ROOT's identity are both `Strong` and equal: outer `Err`,
       `SafetyRejected` (the dynamic half of §129/§149.6). Either not `Strong`: record a warning, or outer
       `Err` under `Strict`.
     - Create it on the top-of-stack handle by **decision 8**: `create_dir(name)` succeeds → push the
       returned handle, `directories_created += 1`, its identity into the fold-collision set if `Strong`;
       `AlreadyExists` → `open_dir(name)`: a directory whose `Strong` identity is in the set is
       `DESTINATION_NAMESPACE_COLLISION` (fail the entry, skip the subtree), otherwise merge into it and
       push; `open_dir` `SAFETY_REJECTED` (a link) or `DESTINATION_ERROR` (a file, or the directory vanished)
       fail the entry and skip the subtree. Any other `create_dir` failure: fail the entry
       (`CreateDir`), skip the subtree.
   - **`File { path }`** (skipped under a skipped prefix): `copy_file_at(fs, src_root.join(path),
     top_of_stack, file_name, &opts')` with `opts'` = `opts` with `publish = Publish::NoReplace` (mandatory,
     §241.5).
     - `Ok(outcome)`: `files_copied += 1`, `bytes_copied += outcome.bytes_copied`; a
       `Some(identity_degraded)` is folded into `warnings` with this path as a candidate example; a
       non-empty `metadata_failures` is reported as `PublishedWithComplaints`.
     - `Err(e)` with `e.step` `Gate` or `Publish` and `e.cause.source.kind() == AlreadyExists`: report
       `Copy` with the code remapped to `Code::DestinationNamespaceCollision`.
     - `Err(e)` with `e.step == Publish` and a kind of `Unsupported`, or raw OS error `EINVAL` / `ENOSYS` on
       unix: outer `Err` with `Code::NoReplacePublishUnavailable` (decision 3). The file's temporary is
       already discarded; `CopyError::leftover` is carried if it was not.
     - Any other `Err(e)`: report `Copy(e)`. This includes a per-file `SAFETY_REJECTED` from the Step 2a
       identity gate (the file IS the source by identity): ONE entry's failure, and the walk continues.
       What aborts is where a rejection came from (steps 2, 4 and 6's `Dir` rule), never its `Code` -
       the walker design's positional rule.
     - A reported leftover path is rebuilt destination-relative: the entry's relative parent joined with
       the temporary's name.
   - **`Symlink` / `Other`**: `Unsupported(FileType)`; `copy_file_at` is never called for them.
   - **`DirEnd`**: pop the handle (or the skipped prefix).
   - **Walk `Err`**: report `Walk`, continue. The walk's depth cap (256) still bounds descent.
7. **F3's gate refinement in `copy_file_at`.** At Step 2a, when `opts.publish == Publish::NoReplace` and the
   destination exists as a non-directory, refuse with `Code::IoError` carrying `ErrorKind::AlreadyExists` and
   `step: Gate`. It runs AFTER every existing identity row and only on a destination the stat found: an
   identity-equal destination is still `SAFETY_REJECTED`, a directory still refused as one, and a degraded
   comparison under `Strict` still refused as degraded; under `Default` a degraded comparison of an EXISTING
   non-directory then meets this refusal. A destination whose stat failed (the "cannot be inspected" row)
   proceeds as before and is caught at publish, where the no-replace rename finds the name taken
   (`step: Publish`, the same mapping). A symlink occupying the name is a non-directory (the handle stat
   never follows it), so it is refused here and left untouched. Single-file `Replace` callers (the CLI) are unaffected. A
   single-file `NoReplace` caller gets the same code and kind it got at publish (`IoError`,
   `AlreadyExists`), now with `step: Gate` instead of `Publish` and without the wasted copy; no temporary
   is created, so none can be left over. The existing test `no_replace_refuses_an_existing_target_and_cleans_up`
   (`crates/flux-core/src/copy.rs`) asserts exactly the code and the absence of a temporary, and so passes
   unmodified.
8. **Directory metadata** is not applied (walker design, "Directory metadata"): directories keep fresh
   timestamps.

## Testing

**Engine, against `FaultFs`** (`crates/flux-core/src/tree.rs` tests), one test per rule, each with a
mutation check that turns it red:

- a fresh tree copies every file and directory, counts right, `warnings` empty;
- merging into an existing destination directory;
- an existing destination FILE: byte-identical afterwards, reported `DESTINATION_NAMESPACE_COLLISION`, and
  no `create_new` recorded for it (the bytes were never copied);
- a fold collision: two source directories whose second `create_dir` meets `AlreadyExists` on a directory
  this operation created (the plan chooses how the fake produces the fold; `repoint_for_test` exists);
- the lexical floor (equal, and nested), refused before any destination call;
- the identity pre-flight, including a destination that resolves to the source;
- the per-directory abort when a directory reached mid-walk is the destination root by identity;
- weak identity on a directory, on the anchor, and per file, grouped by volume, `Unavailable` in its bucket;
  and each refused under `Strict`;
- decision 3: `set_no_replace_support(false)` aborts at the first publish with
  `NOREPLACE_PUBLISH_UNAVAILABLE`; and an `EINVAL`-class failure at `Create` does NOT abort;
- a metadata complaint reported as `PublishedWithComplaints`, the file present;
- a symlink reported `Unsupported`, never copied;
- a walk error reported and the walk continuing;
- a failed directory's descendants never reaching `copy_file_at`;
- a leftover temporary reported by its destination-relative path;
- every `FailureTally` counter, and `total()`.

**`CopyStep`**: each step's failure in `copy_file_at` carries its variant (existing fault hooks drive each).

**Platform** (`crates/flux-platform/tests/dir_handle.rs`): `DirHandle::identity` on a real
`destination_root` handle and a real `open_dir` handle equals the path-based `metadata(..).identity`, on
POSIX and Windows locally and on macOS in CI.

**Real filesystem.** At least one `copy_tree` test against `StdFileSystem`: a fresh tree, an existing-file
collision left intact, and a destination symlinked to the source refused before anything is created. It
runs on all three CI legs. `FaultFs` is `#[cfg(test)]`-only and the root crate does not depend on
`flux-core` or `flux-platform` today, so this needs one new dev-dependency; the plan chooses where, with the
constraint that it creates no dependency cycle.

## Out of 4b

The CLI, the rendering of `TreeOutcome`, and the `--safety=strict` flag (cut 5); directory metadata;
replacing existing files (the §241.5 claim store); §42 mount boundaries; item 113's up-front probe;
`FaultFs::move_object`; recreating symlinks; hardlink topology; and every item in the sequencing spec's
"Out of 4b" table.

## Residues, stated

- **Step 2a to publish** is a window: a destination that becomes an alias in between is not caught (as in
  4a). Harm bounded to a broken hardlink.
- **A per-directory abort happens mid-walk**: files copied before the aliased directory is reached stay
  copied. The lexical floor catches the lexical case up front; only the aliased case is dynamic.
- **Folds on weak-identity volumes merge silently** (FAT32, exFAT: weak AND case-insensitive).
  `Safety::Strict` is the lever.
- **A leftover temporary's reported path** is built from source names, so on a normalizing destination it
  can differ from the name on disk.
- **A symlinked source root is refused by the library** (the walk types the root with the non-following
  `metadata`, so a link is not a directory), while a symlinked DESTINATION is followed (the user named it,
  §149.7). Safe, and asymmetric: cut 5's CLI resolves the source root before calling the engine, as the
  walker design already assigns it.
- **A source nested inside the destination** (`flux copy /a/b /a`) is not refused as a whole; §129 names
  only the destination-inside-source case. Each file is still covered by the Step 2a gate.

## Consult record

AGY-FIRST brief `.clavity/seams/cut4b-forks.md`, reply `.clavity/scratch/cut4b/forks.md`; F4 negotiation
`.clavity/seams/cut4b-f4-negotiate1.md` and `.clavity/scratch/cut4b/f4-negotiate1.md` (local, gitignored);
agy cascade `dab4b353-0c93-4e13-b2e1-cca9ee269d73`. The peer also raised whether a destination that renames
a directory on creation breaks the handle stack; it does not - the stack holds handles, failures are
reported by source-relative path, and folds are caught by decision 8's set - and the one residue (the
leftover path) is stated above.
