# Hardlink preservation (cut 12) - design

Status: draft for approval. Date: 2026-10-26. Roadmap: Phase 4 (deterministic topology), first cut.

## Intent and why

The README lists "Hardlink topology preservation, including exclusion-aware dependencies" as a goal
(`README.md:29`) and the 0.1 scope names hardlink preservation (`ROADMAP.md`). Today nothing implements it: there is
no `--hardlinks` flag, no call that creates a hardlink on `DirHandle` (`crates/flux-fs/src/fs.rs:280`), and
`Code` (`crates/flux-fs/src/error.rs:17`) has no hardlink codes. A source file with several names is copied once per
name, as separate files, with no warning, and the JSON report's `files_hardlinked` is always 0
(`crates/flux-cli/src/report.rs:35,100`). On a tree with many links (a build cache, a package store, a
snapshot-style backup) the destination is larger than the source and the relationships are gone.

This cut makes Flux preserve hardlink groups over the selected namespace, for fresh copies and for `--resume`, without
a new persisted format, and reports every case where it could not.

## What exists today (read before designing)

- The walk is sequential and deterministic (each directory's entries sorted by name bytes, depth first,
  `crates/flux-core/src/walk.rs:153-162`); the canonical member of a group (spec Section 13, lines 1124-1160) is
  therefore always met before its dependents. Sections 91 and 94 (compare-and-swap claims, a hold queue; lines
  4314-4560) exist for parallel workers, which Flux does not have (`ROADMAP.md`, Phase 3).
- Every walked entry carries `Metadata.identity: FileIdentity` (`Strong`, `Weak` or `Unavailable`,
  `crates/flux-fs/src/fs.rs:94`) and `Metadata.nlink: Option<u64>` (`fs.rs:121-124`; both std adapters fill it,
  `crates/flux-platform/src/std_fs.rs:324,356,853`).
- Writing through a file with more than one link is refused: `crates/flux-platform/src/dir_unix.rs:235-240` and
  `dir_windows.rs:817-825` (`Code::SafetyRejected`), and a resumed partial with `nlink != Some(1)` is discarded and
  recopied (`crates/flux-core/src/copy.rs:657-659`). Flux never writes bytes through a link; keep it that way.
- `--resume` revisits every source file. At `crates/flux-core/src/tree.rs:1697-1710` it has both the source metadata
  and the destination metadata, and for a file the prior run completed (claim `Created` plus `resumed_complete`,
  `tree.rs:1522`: a regular file of equal length whose modification times differ by at most 2 s) it counts
  `files_resumed` and calls `names.record_publication` (`crates/flux-core/src/names.rs:178`).
- A published file gets a claim record: `record_claim(.., ClaimStatus::Created)` (`tree.rs:1888`), or
  `commit_prepared` under strict durability (`tree.rs:1886`). `Plan::Replace` claims the stored name first
  (`tree.rs:1897`). The default existing-file policy is overwrite (`crates/flux-cli/src/main.rs:43`).
- The redb claim store (`state.db`, `crates/flux-platform/src/claims.rs`, `FORMAT = 2`, 16 MiB page cache at line 22)
  and the JSON operation manifest (`crates/flux-core/src/state.rs:23` `FORMAT_VERSION = 4`, `:27`
  `MULTI_ROOT_FORMAT = 5`) are the only persisted formats. Several sources are one operation with a roots list.
- `files_degraded` exists (`report.rs:37`) and counts the weak-identity safety gate (`report.rs:158`); the human summary
  line prints no degradations at all (`report.rs:409-414`).
- The sweep deletes every `<name>.flux-partial.<id>` file under DEST for a prior operation id
  (`crates/flux-core/src/run/sweep.rs:64-100`), which also covers a hardlink temporary named that way (the "link
  temporary" comment at `sweep.rs:93` is about symlinks, cut 10a). `FaultFs` already tracks per-path link counts
  (`crates/flux-core/src/fault_fs.rs:1554`).
- There is no hardlink-creating code anywhere in `flux-platform` or `flux-core`. The `link()`+`unlink()` fallback
  that `crates/flux-core/src/run/tests.rs:5033` mentions does not exist in the engine (its own comment says so); the
  primitive is new on all three platforms.

## Goals

1. `--hardlinks=auto|preserve|copy` (spec Section 15, lines 1202-1235), default `auto`. `copy` is today's behaviour.
2. A group of source names for one `Strong` object, over the selected namespace, is recreated as one destination
   object with several names: one physical transfer, the rest links (spec Section 12).
3. `--resume` produces the same destination topology an uninterrupted run would, including after a crash between
   any two steps of a link (the four crash points in "Resume").
4. Resident memory stays bounded however many groups the tree has (spec Section 10.2, line 1034, and principle 8,
   line 507).
5. Every group Flux could not preserve is reported in the run report, the human summary line and the JSON, never
   silently.
6. Linux, macOS and Windows, all three gated by CI.

## Non-goals

- A durable `topology.db` and the dependent-link state machine (spec Sections 16.1 states, 17, 90): deferred until
  parallel workers exist (recorded deviation, below).
- Attempt fencing, compare-and-swap claims and the hold queue (Sections 91-94): dormant in a sequential engine.
- Fallback promotion of another member when the canonical fails (Section 253.7): a later cut.
- Hardlinked symlinks and special files (Section 12.1, `TODO.md:241`): a later cut; this cut treats a symlink or a
  special file as it does today.
- `--exclude` and selection filters: separate cut (Section 14 holds by construction, see "Exclusions").

## Anti-goals (a failure even if it technically works)

- Linking two different source objects (a weak or unavailable identity treated as proof of equivalence).
- Silent loss of topology: any member copied independently instead of linked is counted and reported.
- Registering a destination object this run did not create as the anchor of a group.
- Writing bytes through a link, or replacing a name so that another name changes.
- Copying an excluded member to preserve topology (Section 14).

## Constraints

- No change to any persisted format: `state.rs` `FORMAT_VERSION`/`MULTI_ROOT_FORMAT`, claim store `FORMAT`.
- The engine stays sequential; nothing here may require parallel workers.
- Per-file work for a file with `nlink <= 1` must not change: ordinary trees pay one integer comparison.
- Linking follows spec Section 16.1 steps 0-4 (lines 1284-1304): remove this operation's leftover temporary, create
  the link at `<target>.flux-partial.<operation-id>`, rename it over the target (refusing to replace a target planned
  as new, Section 241.5), remove the temporary if the rename left it (a POSIX rename between two names of one file
  does nothing), record the claim. Re-running a link is therefore safe.

## Behaviour (the contract)

### The index

A per-run, disk-backed map from a source `FileIdentity` (Strong) to one of two values: `Anchor(destination path, the
anchor's destination ObjectId)` or `Failed(Code)` (the group's first member failed to copy; see the table). The
destination path is the destination-relative claim key, `target` (`FluxPathKey::from_relative(&cx.keyed(&path))`,
`tree.rs:1597`), which carries the root prefix in a several-source run. It is never the walk-relative `path`. The ObjectId is read when the anchor is registered (the fresh path already carries it: `o.published_identity`,
`tree.rs:1877`), so that just before linking the run can check that the
anchor is still the object it registered (spec Section 16.1: "each step revalidates ownership as Section 99
requires"); a deleted or replaced anchor must never become a link to a stranger.

- Created lazily the first time a file with `nlink > 1` and `Strong` identity is met, in the operation's own directory
  under `DEST/.flux/operations/<id>/` (name: builder's choice), a `redb` file with a fixed page cache like
  `claims.rs:22`, `Durability::None`, no fsync. Resident memory is the cache, not the group count.
- Removed by the run when it ends, success or failure, except when the run lost the destination's lock (it then owns
  nothing there; plan ruling 15). A crash leaves it behind: the workspace retire path removes only the names it knows
  (`crates/flux-core/src/state.rs:1003-1040`, `finish_retire`; a stranger keeps the workspace), so the index's name
  is added to that list (plan ruling 1; an earlier draft of this paragraph said the existing cleanup "already" removed
  it, which was wrong). No run ever reads a previous run's index, and
  that is enforced, not assumed: a `--resume` continues the prior operation in the same operation directory
  (`main.rs:97`), so the run removes any existing index file of that name before it creates a fresh one.
- Only identities of files with `nlink > 1` enter it. The key is the full source `ObjectId` (volume and index), so it
  is scoped by filesystem identity (Section 109); the aggregated warnings are per source filesystem.
- One index per operation. With several sources (one operation), a group whose members sit under different roots can
  link across roots; the canonical is the first member in processing order.

### Fresh copy

For a file with `nlink > 1` and `Strong` identity, under `auto` or `preserve`:
1. Not in the index: copy it as today. After it is published and its claim recorded, insert
   `Anchor(target, its destination ObjectId)`. This happens after ANY successful publication of a regular file,
   whichever plan produced it: `Plan::New` (claim at `tree.rs:1886-1888`) and `Plan::Replace` (claim at
   `tree.rs:2005-2012`, the upgrade of its own claim). A canonical that overwrites an existing file is an anchor
   like any other. If the copy FAILS, insert `Failed(code)` instead.
   Later members that find `Failed` are not copied; each is reported `BLOCKED_BY_CANONICAL_FAILURE` (Section 92).
2. In the index: do not transfer data. Apply the existing-destination policy to the member's target exactly as for a
   copy (overwrite replaces it by the link; `--skip-existing` and `--update` may skip it, Section 16.1 `Skipped`). If
   the policy proceeds, check that the anchor is still the registered object, then run the Section 16.1 link steps
   against the anchor's destination path. Between that check and the link the anchor's name could be swapped for
   another object (author's addition, a hardlink-specific race): so after step 1 (the link at
   `<target>.flux-partial.<operation-id>`) read the temporary's ObjectId and continue to the rename only if it equals
   the registered ObjectId; otherwise remove the temporary and treat the anchor as changed (the "anchor is gone" row).
   The final name therefore only ever receives the registered object. On success count `files_hardlinked`.

Topology is preserved over the selected namespace only (Section 14): a file whose other names lie outside the walk
is an ordinary copy, not a degradation, and the destination's link count equals the number of selected members
linked, not the source's `nlink`. A single-file copy has one name, so it has no group. With several sources (one operation), `--resume` and `--dry-run`
remain usage errors (`crates/flux-cli/src/main.rs:37`), so a group that spans roots only arises for fresh copies.

### Modes and failures (the table)

| Situation | `copy` | `auto` | `preserve` |
|---|---|---|---|
| `nlink <= 1` on a file with `Strong` identity | as today | as today | as today |
| `nlink` unknown (`None`) on a file with `Strong` identity | as today | not a candidate; not linked; one aggregated warning per operation, and counted in `hardlinks_unchecked` so a script reading the JSON can see it (a share that reports no link counts would otherwise leave only one stderr line) | same as `auto` (author's call: the platform state is exotic, and failing every file would make `preserve` unusable) |
| The anchor is gone, or is no longer the object registered, or cannot be checked (the destination identity is not `Strong`), when a member is about to link | not attempted | copy that member independently; counted in `hardlinks_degraded` | fail the member: `HARDLINK_UNAVAILABLE` |
| A file with `nlink > 1` whose source identity is `Weak` or `Unavailable` (Section 108, lines 5242-5285; a file with `nlink <= 1` is never affected by identity strength, so `preserve` on FAT or NFS still copies ordinary files) | as today | copy independently, never infer equivalence; one aggregated warning per filesystem/operation; counted in `hardlinks_degraded` | fail the member: `HARDLINK_IDENTITY_UNAVAILABLE` |
| Creating the link fails (no hardlink support on the destination, different filesystem, link limit, permission) | not attempted | copy that member independently; counted in `hardlinks_degraded` | fail the member: `HARDLINK_UNAVAILABLE` (Section 15) |
| The first member's target is skipped by the existing-destination policy (`--skip-existing`, or `--update` finding it up to date): no `Created` claim | not attempted | not registered; the first member whose target is NOT skipped becomes the anchor (spec Section 91, lines 4339-4340: "the first member whose target the existing-destination policy does not skip"); the skipped member keeps what it was; counted skipped, plus one `hardlinks_degraded` per member that stays separate from the anchor | same as `auto` (the policy, not Flux, chose the split; reported) |
| The canonical's copy failed | not attempted | the group is FAILED for this run: later members are not copied and are reported `BLOCKED_BY_CANONICAL_FAILURE` (Section 92) | same |

`copy` performs no identity lookup (Section 108, last paragraph) and reports nothing about links.

The no-claims path (`tree.rs:1559-1561`: a lockless copy, or an identity that cannot key claims) does not link: with
no claim there is no `Created` record to make an anchor. Under `auto` every member of a group is copied independently
and counted `hardlinks_degraded`; under `preserve` the members of a group fail with `HARDLINK_UNAVAILABLE`.

### Resume (R2: rebuild from the walk)

`--resume` revisits every source file in the same order. At the skip site (`tree.rs:1697-1710`), when a completed
file has `nlink > 1` and `Strong` identity, insert `identity -> target` into the index before returning. Later members
then link exactly as in a fresh run. The four crash points:

- before the canonical is published: the partial is continued or recopied, published, registered; all members link;
- after the canonical, before any link: the canonical is skipped and registered; all members link;
- after some links: the linked members are skipped (`Created`), the canonical is registered; the rest link;
- after a link was renamed into place and before its claim: the member has no claim; under the default overwrite
  policy it is replaced by an identical link (Section 16.1: "an existing correct link never causes a failure"); under
  `--skip-existing` or `--update` it is left, and it is already a correct link.

"Created by this operation" means: this operation holds a `Created` claim whose target is the entry's path. A resume
continues the prior operation as itself (`main.rs:97`), so its earlier `Created` claims count; an `Existing` claim, or
none (a file skipped by `--skip-existing` or `--update`, or an unrelated pre-existing file), never makes an anchor.

At the skip site, for a source file with `nlink > 1` and `Strong` identity, the order is:
1. no index entry yet: this member is the anchor; register `(its target, its destination ObjectId)` and skip it as
   today;
2. an entry exists and the member's destination `ObjectId` equals the anchor's: it is already linked; skip it;
3. an entry exists and the ObjectIds differ (an independent copy from an earlier `--hardlinks=copy` run, or a member
   linked to the OLD object of a canonical that was recopied because it failed `resumed_complete`): do not skip; fall
   through to the link path. The existing-destination policy applies there first, exactly as for a copy: under the
   default overwrite the member is replaced by a link to the anchor (an atomic rename of the same bytes under the
   claim protocol); under `--skip-existing`, or `--update` finding the member up to date, the member is left as it is
   (the user's policy wins over healing) and is counted in `hardlinks_degraded`. A healed member counts
   `files_hardlinked`, not `files_resumed`.

A canonical that fails `resumed_complete` (length or time mismatch; with `--preserve-times` off it never passes) is
recopied as a new object and registered with its new ObjectId, so every earlier member is healed by rule 3. The walk is
sorted, so a resumed run picks the same canonical as the first run unless the source tree changed in between; if it
changed, rule 3 still converges. `--restart` starts a fresh operation; it never reads the prior index.

### `--dry-run`

A dry run must not touch the destination, so its index lives in the system temporary directory (author's call), is
removed at the end, and never under `DEST/.flux`. A fresh dry run goes through `dry_stop_file` (`tree.rs:1563`): it
registers the source identity and the planned target path (there is no destination object, so no ObjectId). It
reports how many links it would create and the members that would be copied independently for reasons known without
I/O (weak identity, an anchor that no claim could make). A dry run cannot predict a link-creation failure or a
vanished anchor, and does not report them. A dry `--resume` follows the real resume registration (`dry_resumed`,
`tree.rs:1710`), keyed by the destination objects that exist.

### Report

- `files_hardlinked` (exists, `report.rs:35`) counts links created, including relinked members.
- New: `hardlinks_degraded` (members copied independently instead of linked) in the JSON report, and
  `summary_line` (`report.rs:409`) appends `; N hardlinks degraded` when non-zero. `files_degraded` keeps its current
  meaning (the weak-identity safety gate).
- New: `hardlinks_unchecked` (files whose link count the platform could not report, so no link was looked for), in the
  JSON report, with `; N hardlinks unchecked` appended to the summary line when non-zero.
- Aggregated stderr warnings use the Section 108 wording. Under `preserve`, a failed member is an ordinary member
  failure with its code.
- New codes in `Code` (`error.rs:17`): `HardlinkUnavailable` and `HardlinkIdentityUnavailable` (spec Sections 15, 108),
  mapped to exit codes by the existing mapping.

### The platform call

A new `DirHandle` method (builder names it): create a name in this directory that is a new link to the object named by
another directory handle and name, never replacing an existing name, never following a symlink, handle-relative.
Unix: `linkat` through `rustix` (as `dir_unix.rs` already does). Windows: a handle-based link; the builder measures
the call on NTFS and ReFS. `FaultFs` gains link creation with fault points between link, rename and claim (the four
crash points). Failure mapping: no hardlink support, cross-filesystem, link limit and permission map to
`HardlinkUnavailable`; disk full stays `DiskFull`. Nothing in the repository creates a hardlink today, so the
builder measures the call on each platform before relying on it.

### Exclusions

The index is fed by the walk, so an excluded member never enters it and is never copied to preserve topology
(Section 14). Nothing to build now; the invariant is stated so the `--exclude` cut keeps it.

## Approaches considered

1. Sequencing of the work (agy-first consult, `[VERDICT: NEGOTIATE]` then `[VERDICT: ALIGNED]` after a second
   round). (X) fresh copies first with an in-memory map: rejected, the spec requires topology state to be able to
   spill to disk (Section 10.2, line 1034; principle 8, line 507; Section 17, line 1309) and the number of distinct groups is unbounded. (Y) full
   durable store and resume at once: rejected, it builds machinery for parallel workers into a sequential engine.
   (Z) detect-and-report only first: rejected, delivers no preservation. (W, chosen) fresh copies with a per-run
   disk-backed index that is never persisted, so no format ships that a later cut must rewrite.
2. Default value (agy-first, `ALIGNED`). `auto` now (spec Section 15, line 1230) versus `copy` until a later cut.
   Chosen `auto`: both defaults flip behaviour exactly once, the spec says `auto`, and `auto` is defined as "degrade
   and report". agy's table counted two flips for `copy`; that count was wrong and is not relied on.
3. Resume (agy-first, `ALIGNED`). (R1) degrade on resume; (R2) rebuild the index from the walk; (R3) R1 then R2.
   Chosen R2: the walk already visits completed files with both identities in hand (`tree.rs:1697-1710`), so
   rebuilding is one registration call, preserves topology across all four crash points, and removes the need for a
   durable store in a sequential engine.
   The owner chose R2 on 2026-10-26, replacing the earlier "degrade on resume".
4. Where the index lives. A second redb file in the operation directory (chosen), versus a table in `state.db` (a
   claim-store format bump to 3 that a later cut would extend again) or a sorted spill file (new code with no
   precedent in the repository).

## Deviations from the spec (recorded)

- Section 17 asks for a crash-aware, resumable topology store; deferred until parallel workers need one. The walk
  rebuilds the same information deterministically.
- Sections 91-94: not implemented; sequential, the canonical always precedes its dependents.
- Section 92: only the "terminally failed" half is implemented; no fallback candidate (Section 253.7).
- Section 12.1 (hardlinked symlinks) is not implemented.
- A correction to an earlier statement in this design's own brainstorm: Section 108 (weak identity) is NOT deferred.
  With `auto` as the default it is required in this cut, otherwise a copy from FAT or NFS could link two different
  files.

## Decided by the owner

Cut order with a per-run disk-backed index and no persisted format; `--hardlinks=auto` as the default; R2 resume.

## Calls made by the author (not the owner - check these)

- Index file in the operation directory, lazily created, fixed cache, removed at the end of the run and by the sweep.
- Only `nlink > 1` and `Strong` identity files enter the index; an unknown `nlink` is a candidate for one aggregated
  warning, not an error.
- Link publication goes through the existing claim protocol, as a copy does.
- A failed canonical blocks the group (Section 92) rather than promoting the next member.
- A completed member that is not linked to the anchor is relinked (healing), not left.
- `--dry-run` reports planned links and degraded members.
- New report field `hardlinks_degraded` and the summary-line suffix; new codes `HardlinkUnavailable` and
  `HardlinkIdentityUnavailable`.
- Groups may link across several source roots in one operation.
- Name: cut 12, on branch `spec/hardlinks`.

### Plan rulings (2026-10-26, refinements made while planning; the plan's "Plan rulings" section is the full list)

- The index file is named `hardlinks.db` and is a known name of the workspace retire path (correction above).
- A link candidate (`nlink > 1`, `Strong`) is never staged into a Strict-durability directory batch: a batched
  canonical would publish after its dependents are met. Such files publish one by one, as symlinks do.
- The tree reads the source metadata once per file and shares it with the checkpoint, resume and policy readers, so
  an ordinary file in a checkpointing run pays no extra system call.
- `--hardlinks` is not recorded in the manifest; a `--resume` may change the mode.
- The index's failed entry names the canonical (its claim key) so the dependent's record can say which member failed;
  `BLOCKED_BY_CANONICAL_FAILURE` is a `Code` (it is in the spec's error catalogue) and counts as a failed file.
- An anchor entry's destination object is optional: absent for a dry run and for an anchor whose destination
  identity is not `Strong`; a live member meeting an absent object treats the anchor as unverifiable.
- The anchor's directory is opened from a second handle on DEST, obtained by resolving DEST once more (the
  §149.7 exemption) and checked against the walk's root identity.
- A linked member is counted in `files_hardlinked` only, never in `files_copied`; the summary line also gains
  `; N hardlinked`.
- A member skipped by the existing-file policy before its group has an anchor is recorded in the index as skipped,
  never linked to, and counted as degraded only when a later member becomes the anchor (the table's "stays separate
  from the anchor"); a destination that is already correctly linked, copied under `--skip-existing`, reports nothing.
- Index I/O failures, an unopenable workspace and a missing anchor root are all "cannot check": `auto` degrades and
  counts, `preserve` fails the member; nothing aborts the run. The non-weak degradations carry one aggregated warning
  with an example (`hardlinks_unlinked`), beside the §108 weak-identity warning.

## Left to the builder

- The exact `DirHandle` method name and signature; the Windows call; the index file name and `redb` settings.
- How the run obtains a handle to the anchor's parent directory from the destination path stored in the index
  (opened handle-relatively from the destination root, never by a re-walked absolute path).
- How a link publication slots into `Plan::New`/`Plan::Replace` and the strict-durability prepared protocol
  (`tree.rs:1886-1925`), and the interplay with `names.record_publication` for hardlink aliases on a
  case-insensitive destination (`TODO.md:363`).
- The wording of the aggregated warnings (use Section 108's) and the JSON field placement.
- The stress tests of Section 69 (lines 3356-3376) scaled to CI: one pair, ten members, a large single group, many
  distinct groups with a deliberately tiny cache to prove resident memory does not grow with group count.
- Updates to `ROADMAP.md` (Phase 4 text), `README.md` (a `--hardlinks` paragraph) and `TODO.md` (items 241 and 363).
- The JSON field list is pinned by tests (`crates/flux-cli/src/report.rs:858` and `crates/flux-cli/tests/copy.rs:38`):
  adding `hardlinks_degraded` means updating them. The doc comment at `report.rs:25-27` ("nothing hardlinks,
  reflinks or verifies, so those stay 0") becomes false and must change.

## Stand-downs

None yet.
