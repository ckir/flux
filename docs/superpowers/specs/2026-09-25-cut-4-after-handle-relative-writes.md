# Walker cut 4 after §149.7: the amendment

The walker design (`2026-09-23-directory-walker-design.md`) was written before handle-relative destination
writes (§149.7, PR #44) merged. This document records what PR #44 changes about cut 4, the eight decisions
that followed, and how cut 4 is now split. **Where this document and the walker design disagree, this one
wins; where this one is silent, the walker design's intent stands and the merged code wins on shape**, as
that design's own "Reading this document against merged code" section already requires.

Each decision below was consulted with the agy peer (AGY-FIRST, then two AGY-NEGOTIATE rounds), converged
with both parties agreeing, and approved by the owner on 2026-09-25. The briefs and replies are under
`.clavity/seams/cut4-forks*.md` and `.clavity/scratch/cut4-forks/` (local, gitignored).

## What PR #44 made obsolete

These walker-design sections described the identity-recheck **substitute** for §149.7. The substitute was
never built and now never will be; handle-relative writes replace it:

- "A destination directory swapped after Flux created it" (item 114): the capture-then-re-verify stack of
  destination identities. A handle cannot be re-pointed, so there is nothing to re-verify.
- The per-FILE destination-PARENT stat in "What all of this costs". It existed only for the re-verify.
- The "item-114 capture" as the source of the fold-collision set. The set survives (decision 8); it is no
  longer a by-product of the capture.

Everything else in the walker design's engine section stands, adapted as below: the lexical containment
floor, the §129 pre-flight, the per-`Dir` anchor comparison, the Step 2a per-file identity gate, `Safety`,
`Outcome.identity_degraded`, the weak-identity aggregation, mandatory `NoReplace` with
`DESTINATION_NAMESPACE_COLLISION`, the streamed failure sink and `FailureTally`, consumer-side subtree
skipping.

## The eight decisions

**1. One copy algorithm, reached through a handle.** `copy_file_at(fs, src, parent: &Dir, name, opts)`
holds the algorithm. `copy_file(fs, src, dst, opts)` becomes a thin wrapper: Step 0's lexical self-copy
refusal (unchanged, before any call), then it splits `dst` into parent and file name, resolves the parent
with `destination_root` (the one path-based call on the write side, which §149.7 exempts), and calls
`copy_file_at`. So single-file copies get §149.7 too. A `dst` with no file name (a root, or ending in `..`)
is refused as `DESTINATION_ERROR` before any call; a bare file name's empty parent becomes `.`.
`copy_file` now requires `F: DestinationRoot`; its only callers are the `FaultFs` tests and the CLI's
`StdFileSystem`, both of which implement it. A leftover temporary is reported relative to the parent by
`copy_file_at` and joined onto the parent path by `copy_file`, so the reported path is unchanged.

**Consequence found while planning, and it is not optional:** the handle `rename_replace` on POSIX is a
bare `renameat` (`crates/flux-platform/src/dir_unix.rs`), without the read-only guard the path version
carries (`destination_is_write_protected`, `crates/flux-platform/src/std_fs.rs`). Routing `copy_file`
through handles without porting that guard would make `flux copy` silently overwrite a read-only
destination on Linux and macOS, which it refuses today. Cut 4a ports the guard to both handle arms.
Whether the Windows NT rename already refuses a read-only or ACL-protected target by itself is measured
first, not assumed: the path guard's own comments record that `MoveFileExW` replaced an ACL-protected file.

**2. Containment uses the RESOLVED destination's identity.** A new `DirHandle` method returns the handle's
own identity (POSIX `fstat` on the descriptor, Windows `FileIdInfo` on the handle, `FaultFs` from the
node). The §129 pre-flight and the per-`Dir` anchor comparison compare against the identity of the
destination root `destination_root` actually opened. This replaces the walker design's "refuse a
`Symlink` anchor" rule, which existed only because `FileSystem::metadata` never follows links while a
path-based engine had nothing else to ask. Delivered in 4b, where it is first consumed.

**3. Item 113's probe stays deferred; the first discovery aborts.** The owner's ruling stands: the
compliant probe writes `noreplace-probe` inside the operation workspace (`FLUX_FULL_UPDATED_SPEC_V16.md:10877`),
which no walker cut builds. New: the FIRST file whose no-replace publish fails with "primitive
unavailable" aborts the whole operation as the outer `Err` with `Code::NoReplacePublishUnavailable`,
instead of failing every file one at a time. "Primitive unavailable" is the rename failing with
`ErrorKind::Unsupported`, or with raw `EINVAL` / `ENOSYS` on unix, AFTER `create_new` of the temporary
succeeded. That ordering is what makes `EINVAL` a sound signal: the temporary is named
`<target>.flux-partial.<id>` (`temp_path`, `crates/flux-fs/src/options.rs`) and so contains the target
name, which means an invalid character or an over-long name fails at `create_new`, before any rename. The
aborted file's temporary is already discarded (`discard`, `crates/flux-core/src/copy.rs`); only the
directory chain above it remains. Exit 1, not 3 (3 promises nothing changed). Tracked debt until the
workspace probe lands. Delivered in 4b.

**4. No `FaultFs` node-id re-key in cut 4.** The engine renames only files (temporary to name), never
directories, so `move_object`'s stranding of a renamed directory's children is unreachable from cut 4.
Engine-level handle tests use `repoint_for_test`. The re-key stays tracked debt for the first cut that
renames a directory.

**5. §42 mount boundaries get their own later cut.** Doing it right changes the merged walk (the walk must
not even READ a mounted subtree such as `/proc`, so consumer-side skipping is wrong) and adds a
`--cross-filesystems` option. Reviewable on its own; tracked in `TODO.md`.

**6. Cut 4 is split.**
- **4a, single-file safety:** decision 1 (`copy_file_at` and the wrapper), the read-only guard on both
  handle arms, `Safety` on `CopyOptions`, the Step 2a destination identity gate, `Outcome.identity_degraded`.
  It fixes a defect live today: `flux copy a b` where `b` is a hardlink of `a`, or the same file by
  another spelling, copies the file onto itself.
- **4b, `copy_tree`:** decisions 2, 3, 7 and 8, the lexical floor, the §129 pre-flight, the per-`Dir`
  anchor comparison, the weak-identity aggregation into `TreeOutcome`, mandatory `NoReplace` with
  `DESTINATION_NAMESPACE_COLLISION`. Its plan is written after 4a merges.

**7. `copy_tree` keeps a stack of destination `DirHandle`s** parallel to the walk: pushed on `Dir`, popped
on `DirEnd`. No destination path below the root is ever re-resolved.

**8. Directories are created first, then opened on `AlreadyExists`.**
1. `parent.create_dir(name)` succeeds: this operation created it. Push the handle; record its identity in
   the fold-collision set if `Strong`.
2. `AlreadyExists`: `parent.open_dir(name)`.
   - Succeeds: a directory is there. If its identity is `Strong` and in the fold-collision set, this
     operation created it moments ago under another source name (a case-folding or normalizing
     destination): fail the entry with `DESTINATION_NAMESPACE_COLLISION` and skip the subtree. Otherwise
     it pre-existed: merge into it.
   - `SAFETY_REJECTED` (a link occupies the name): fail the entry, skip the subtree.
   - `DESTINATION_ERROR` (a file occupies the name, or the directory vanished between the two calls):
     fail the entry, skip the subtree. The race is reported, never followed.
3. Any other `create_dir` failure: fail the entry, skip the subtree.

Create-first costs one call on a fresh destination, the dominant case; open-first would pay a failing open
per directory. Neither call ever traverses a link.
