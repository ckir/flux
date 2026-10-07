# Cut 8a: safety before replacement

Status: draft for review (2026-10-03). Branch `spec/cut-8a`, from `main` 02c09b3.

## Why

Cut 8 makes a directory copy replace existing destination files (spec section 5.1, `--overwrite` by default). The
owner split it in two:

- **8a (this spec):** the safety prerequisites.
- **8b (later):** `state.db`, the section 241.5 claim records, replacement and the policy flags.

8a ships first because today's guarantee rests on no-replace publication. Every file in a tree publishes with
`Publish::NoReplace` (`crates/flux-core/src/tree.rs`, the walk loop), so a copy that wrongly merges into a
directory can add files there but never overwrite one. Once 8b lets it replace, the same mistake destroys data.

8a closes three gaps:

| Part | Gap today | Debt entry |
|---|---|---|
| P | The engine learns at the first publish that the destination has no no-replace primitive, instead of refusing before anything changes (spec item 113). | `TODO.md`, "Item 113's up-front no-replace probe is not built" |
| M | A mount inside the destination can show a source subdirectory under a destination name, and the copy merges into it, so new files land in the source. | `TODO.md`, section 42 entry, "The destination half too" |
| A | The CLI resolves a link in a parent component of DEST before the engine runs (`flux-cli/src/resolve.rs`), so the lexical floor refuses it there. An engine caller that does not canonicalize, or a link swapped between the CLI's path-based resolution and the engine's open of DEST's parent, passes the pre-flight, and files are written into the source before the walk's own check stops the run. | none yet (found in this cut's design consult) |

## Decisions

Each design fork went to agy first; the owner chose. The reasoning lives under "Design record".

1. **Scope:** P, M and A. The case where DEST or an ancestor of it is a same-filesystem bind mount of a source subdirectory is a
   known limit, recorded and not built (see "Known limits").
2. **M on finding a mount point:** fail only that subtree and continue, reported as `SAFETY_REJECTED`.
3. **M's detection:**
   - **Where it lives:** a new `DirHandle` query.
   - **Linux:** `statx`'s `STATX_ATTR_MOUNT_ROOT` (kernel 5.8 or later). Where it is not reported, compare the
     device numbers of the parent and the child.
   - **macOS:** compare the device numbers.
   - **Windows:** nothing new is needed.
4. **A's mechanism:** canonicalise the source root and the destination from their open handles, then test
   containment component by component. When the query is unsupported, the default warns and `--safety strict`
   refuses; any other failure aborts.
5. **P:** exactly as spec section 241.5 states it. It runs inside `<id>.creating`, before the manifest is written and the
   workspace published, and before the record and `--restart`'s supersede, on every run.
6. **Tests:** the in-memory fake for every rule, plus a real bind mount on Linux CI and a real junction on Windows CI.

## Part P: the no-replace probe

The normative text is spec section 241.5 (`FLUX_FULL_UPDATED_SPEC_V16.md`, around lines 10874-10887).

**When it runs.** Inside `<id>.creating`, the unpublished workspace directory, which `TreePlace::create`
(`crates/flux-core/src/run/place.rs`) makes with `begin_workspace` (`state.rs`): after that directory is created and
before its manifest is written and it is published (`publish_workspace`). The lock is this run's (just obtained), the
record is not yet written, so the probe is guarded as the manifest write is. `--restart`'s supersede (`supersede`,
`run/restart.rs`) comes later still, so a refusal leaves every prior operation exactly as it was, still resumable.
The probe runs on every run, `--restart` included: a restart may meet a different filesystem under the same name.

The probe cannot run after the workspace is published: `publish_workspace` itself publishes `<id>.creating` onto
`<id>` with `rename_no_replace`, so on a destination without the primitive the run would stop there with a plain
failure, never reaching a probe placed after it (see the Design record, "Part P's placement").

`open_operation` serves the single-file run too, which spec section 241.5 exempts ("Single-file operations are
unaffected"). So the probe is part of the `Place` trait's `create` (`run/place.rs`): the tree's place probes inside
`<id>.creating`, and the single-file place does nothing. The function is `probe_no_replace`.

**What it does.** The probe's writes are NOT under the section 99 guard: `place.create` runs before the lock record
is written (`run/session.rs`), so the guard has no record to check. The only protection is that the lock is held,
just obtained by this run; the first guarded mutation is later (the record write and the supersede).
1. It stages a temporary (`noreplace-probe.tmp`) in `<id>.creating`.
2. It publishes the temporary onto the fixed name `noreplace-probe` with `DirHandle::rename_no_replace`, the same
   primitive every tree file publishes with.
3. It removes `noreplace-probe`, or, if the publish failed, the staged temporary: the probe removes whatever it
   wrote.

**Its outcomes:**

| Outcome | Result |
|---|---|
| The publish succeeds and the removal succeeds | The run continues. |
| The publish fails as "primitive unavailable" (the same classification `primitive_unavailable` in `tree.rs` applies at `CopyStep::Publish`) | The run is refused with `NOREPLACE_PUBLISH_UNAVAILABLE` (`refuse_no_replace`). `<id>.creating` is removed again (`unwind_creating`), then the control directories it emptied, and DEST if this run created it; the lock is released. Exit 3 (section 55: created and removed again does not count as a change). The stop is a refusal carrying the code, with `changed: false` when everything was removed again. When a file the probe wrote could not be removed, `not_removed` names it and `changed` is `true`, which exits 1: the pairing `give_back` in `run/session.rs` already uses for a lock it could not remove (`for_stop` reads only `changed`). When the lock itself also cannot be discarded, `give_back` makes the lock the `not_removed` entry and moves the displaced entry into `refusal.detail` ("also not removed: ..."). |
| The publish fails for any other reason | The run fails with that error (`RunError::Failed`, step `Probe`), through the run's existing failure path; `<id>.creating` and the control directories are removed best-effort. |
| A probe file stays after a SUCCESSFUL publish (`noreplace-probe` could not be removed) | `RunWarning::ProbeNotRemoved` naming the file at its published path, and the run continues. The workspace's retirement then fails to remove the non-empty directory, reported as `RunWarning::NotRemoved` (after COMPLETED nothing fails the run). On a successful run the retire leaves `<id>.removing` with the file inside; cleaning up `<id>.removing` is a later cut. The CLI's warning text says the file "goes when the operation's state is removed"; that wording is not accurate for this case and is noted here rather than changed in this cut. |
| The publish was REFUSED and a probe file stays (the staged temporary could not be removed) | The stop stays `NOREPLACE_PUBLISH_UNAVAILABLE` with `changed: true` and `not_removed` naming the temporary under `<id>.creating`, so it exits 1 (spec section 55) and the report names both the code and the file (when the lock cannot be discarded either, `give_back` moves the file's entry into `refusal.detail`, as in the row above). `<id>.creating` stays (the section 21.1 scan passes it over, `prior.rs:47-49`); the control directories and DEST stay since they are not empty; the lock is released. |

**`--dry-run`.** Flux has no `--dry-run` today, so the spec's dry-run clause has nothing to attach to. It applies
when that flag lands, and the probe then writes nothing and reports "unprobed".

**The backstop stays.** The walk keeps classifying a primitive failure at the first publish. With M in place, every
directory the walk writes into is either created by this run (on its parent's filesystem) or a pre-existing
directory that is not a mount root. So a probe in DEST's workspace speaks for the whole tree, apart from the
degraded cases under "Known limits".

## Part M: no merging into a destination mount point

**The rule.** When the walk is about to merge into a destination directory that already existed (`enter_dir`'s
"pre-existing: merge into it" arm, `tree.rs`), and that directory is the root of a mount, the subtree is not copied.
It is reported once, as a `TreeFailureCause::CreateDir` with code `SAFETY_REJECTED`, and its frame becomes
`Frame::Skipped`, as for a reserved path. The rest of the tree is copied. A run with any reported failure already
exits 1.

A directory this run created is never checked: it cannot be a mount root.

**The query.** A new `DirHandle` method, answered from the open handle of the child and of its parent. It returns
an error, reported as that subtree's failure exactly as a failed `open_dir` is, or one of three answers:

- "a mount root";
- "not a mount root";
- "cannot tell".

| Platform | "A mount root" when | "Cannot tell" when |
|---|---|---|
| Linux | `statx` on the child reports `STATX_ATTR_MOUNT_ROOT` in its attributes; or, where the attribute mask does not include it, the child's device number differs from the parent's (the volume inside each handle's own identity) | the mask does not include the attribute and the device numbers are equal |
| macOS | the child's device number differs from the parent's | never |
| Windows | never: a mounted-volume folder is a name-surrogate reparse point, which `open_dir` already refuses | never |

macOS has no same-filesystem bind mount of its own, and its mounts (including FUSE and `nullfs`) present a
different device number.

**"Cannot tell".** It follows the weak-identity rule that `enter_dir` already applies (`WeakIdentityWarnings`):

- **Default:** a new warning in the tree's outcome, beside `WeakIdentityWarnings` and rendered the same way: one
  group with a count and the first path, relative to the source root. Then merge.
- **`--safety strict`:** the subtree is refused as above.

**The source-root check stays.** The walk's existing check that a destination directory is not the source root,
by identity, keeps aborting the whole operation. It is a stronger statement than a mount point, and what it checks is not changed; where it runs is.
It runs FIRST: on a pre-existing directory, the source-root identity check comes before the mount-root query, so a
destination mount that presents the source root itself still aborts the operation instead of becoming a skipped
subtree (today that check runs only on a live frame, `tree.rs` walk loop, so the order has to be moved, not kept).

## Part A: a destination inside the source, through a link in a parent component

The CLI resolves a link in a parent component before the engine runs; Part A closes the engine API (a caller that does not canonicalize) and the check-then-use window between that resolution and the engine's open of DEST's parent.

**The rule.** Before anything is created, `locate_tree` (`crates/flux-core/src/run/place.rs`) obtains the canonical
paths of two directories:

- the source root;
- the destination anchor: DEST if it exists, otherwise DEST's parent.

If the anchor equals the source root or lies inside it, compared component by component as `lexically_within`
does, the run is refused with `SAFETY_REJECTED` before the lock is taken. This holds in every safety mode. The check
runs inside `locate_tree`, on BOTH of its paths: the filesystem-root branch (DEST is a root, `holder` is DEST) and
the named-DEST branch. On each it runs after that branch's own `preflight` call and before that branch returns.

**How the paths are obtained.** From open handles, never by resolving the path a second time:

| Platform | Query |
|---|---|
| Linux | `readlink` of `/proc/self/fd/<n>` |
| macOS | `fcntl(F_GETPATH)` |
| Windows | `GetFinalPathNameByHandleW`, which gives both paths in the same form and the same letter case |

The query is asked of the very handle the run then writes through: the anchor handle `locate_tree` opens and hands
on (DEST, or the holder that creates DEST). A path swapped after the check therefore changes nothing the run
touches. The source root is opened for the query the way the walk opens it, and closed after it. A new query is
added to the platform traits for each. A returned path is checked before it is used: the object found at it, without
following a final link, must have the handle's own identity. A mismatch (on Linux, for example, a removed
directory, whose `readlink` text gains a `" (deleted)"` suffix that a real name may also carry) counts as "any other
error" below. The text is never inspected for that suffix.

**When the query fails:**

| Failure | Default | `--safety strict` |
|---|---|---|
| Unsupported by the filesystem or the system (`ErrorKind::Unsupported`, `ENOSYS`, Windows `ERROR_INVALID_FUNCTION`, `/proc` not mounted) | the lexical floor (which already ran) is the only containment check; a new warning in the tree's outcome says the containment check was degraded, naming the path whose query failed; the run continues | refused with `SAFETY_REJECTED` |
| Any other error (access denied, the directory vanished) | the run aborts with that error at the resolve step | the same |

**What does not change.** The lexical floor (`prepare_source`), the identity pre-flight (`preflight`) and the walk's
dynamic check (`enter_dir`) all stay. Part A adds a check; it removes none.

## Dependencies

None new, and no new feature. Measured against the locked versions: `rustix` 1.1.5 (already a Unix dependency of
`flux-platform`, feature `fs`) has `statx` with `StatxAttributes::MOUNT_ROOT` and, on Apple targets,
`rustix::fs::getpath` (`F_GETPATH`). `windows-sys` 0.61 with the workspace's `Win32_Storage_FileSystem` feature has
`GetFinalPathNameByHandleW`. Linux's `/proc/self/fd/<n>` is read with `std::fs::read_link`; the identity re-check of a returned path uses
the existing `FileSystem::metadata` (`crates/flux-platform/src/std_fs.rs`), which reads identity without following a
final link on every platform: `symlink_metadata` on Unix, and on Windows a handle opened with
`FILE_FLAG_OPEN_REPARSE_POINT` and read for `FILE_ID_INFO` (std's own `Metadata` carries no file id there).
`GetFinalPathNameByHandleW` needs no access beyond what the directory handles already have: they are read for
identity (`FILE_ID_INFO`) today.

## Known limits

Each is recorded in `TODO.md` by this cut.

1. **DEST, or any of its ancestors, is a same-filesystem bind mount of a source subdirectory** (for example DEST
   `/dst/b/out`, with `/dst/b` a bind mount of `/src/sub`). M checks only directories the copy merges into below
   DEST, and A's canonical paths show the mount's own path, not its target. Refusing every DEST or ancestor that is
   a mount root would refuse ordinary targets such as `/mnt/usb`. Detecting this case needs the source
   identities that the memory bound forbids (spec line 997: no in-memory list proportional to the tree).
2. **Linux without `STATX_ATTR_MOUNT_ROOT`** (kernels before 5.8, or a filesystem that does not report it): a
   same-filesystem bind mount is "cannot tell", so the default warns and merges.
3. **Source-side mounts** (section 42's walk rule and `--cross-filesystems`) stay with the dedicated mount-boundary
   cut.

## Testing

Every rule gets a test that fails under a mutant of the code it guards. The in-memory fake
(`crates/flux-core/src/fault_fs.rs`) gains three settings:

- a per-directory mount-root answer;
- a canonical path per directory;
- a failure for the canonical-path query (unsupported or other).

**Unit tests (fake), on every platform:**

- **P:**
  - a probe that succeeds leaves no `noreplace-probe`;
  - an unavailable primitive refuses with `NOREPLACE_PUBLISH_UNAVAILABLE`, nothing written outside the workspace,
    and `<id>.creating`, the control directories it emptied and a DEST this run made are removed again;
  - a probe file that cannot be removed gives the warning when the run continues (the workspace is later left as
    `<id>.removing`); on a refusal it exits 1, the stop is still `NOREPLACE_PUBLISH_UNAVAILABLE` (not a removal
    failure), `not_removed` names the temporary, `<id>.creating`, the control directories and DEST stay, and the
    lock is released;
  - another publish error fails the run;
  - `--restart` probes again, and a refusal there leaves the prior operations un-superseded and resumable.
- **M:**
  - a pre-existing mount root is skipped and reported, and its siblings are copied;
  - a directory this run created is not queried;
  - "cannot tell" warns under default and refuses the subtree under strict;
  - the source-root alias still aborts, even when that directory is also a mount root (the order).
- **A:**
  - a destination whose canonical anchor lies inside the source is refused before the lock;
  - a merely similar name (`/database` against `/data`) is not;
  - unsupported warns under default and refuses under strict;
  - another error aborts.

**Real-system tests:**

- **Linux CI job:**
  - **M:** `sudo mount --bind` of a source subdirectory inside the destination. The copy skips it, reports it, and
    leaves the source unchanged.
  - **Linux query:** checked against the same mount.
- **Windows CI (the gate):** A, with a junction in a parent component of DEST pointing into a source
  subdirectory. A junction needs no administrator rights. The fixture puts a file in the source that the walk
  reaches BEFORE the overlapping directory, so a run without Part A would write it into the source before the walk's
  own check stops it. The test asserts the refusal names Part A's containment check, that no lock or workspace was
  ever created under DEST, and that the source is byte-for-byte unchanged.
- **macOS CI job:** the device-number comparison through the platform test of the new query, against a known
  mount (the system's own `/dev` is a separate mount).

Locally, a real-system test skips with a stated reason where the facility is missing (no sudo, no junction
support). On CI (`CI` set in the environment) a missing facility FAILS the test instead, so a runner change cannot
turn the job into a permanent silent skip.

## Out of scope

- Everything in 8b: `state.db`, claim records, replacement, `--update`, `--skip-existing`.
- `--dry-run`.
- `--cross-filesystems` and the source-side section 42 rule.

## Stand-downs

Panel findings rejected, recorded so they are not raised again:

- Round 2 (agy): "exit 1 instead of 3 masks `NOREPLACE_PUBLISH_UNAVAILABLE`". The exit code is REJECTED as a change:
  spec section 241.5 fixes it ("if the operation is being refused, it exits 1 instead of 3"). The visibility half is
  folded: the report names the code.
- Round 1 (agy): "`GetFinalPathNameByHandleW`'s `\\?\` prefix makes the containment test miss". REJECTED: Part A
  canonicalises BOTH the source root and the anchor through the same query, so both carry the same prefix; the
  scenario compared a canonical anchor with a raw source path, which the spec never does.

- Round 3 (agy): "use `openat2` with `RESOLVE_BENEATH` instead of canonical paths". REJECTED: it confines a
  resolution to below a directory handle, which is not Part A's question (whether DEST's already-resolved location
  lies inside the SOURCE), and it exists only on Linux.

- Round 4 (agy): "canonical string paths reintroduce check-then-use races". REJECTED: the paths come from the
  handles the run then writes through and are identity-checked (Part A), the option agy itself proposed in the F3
  negotiation; a path swapped afterwards changes nothing the run touches.

## Design record

- **The guarantee (round 1).** Two framings were weighed:
  - "never write into the source, by any alias";
  - "never merge into a destination mount point".

  agy judged the first impossible within the memory bound: a bind mount has the same device and inode as its target
  and no attribute marking it, so recognising one as a source directory needs every source identity. The driver
  agreed. The owner chose the structural rule plus Part A, which agy's own answer had led the driver to measure.
  agy's example, `flux copy /src /src/dst`, is already refused by the lexical floor. The parent-symlink variant is
  not.
- **M's Linux detection (round 2 and one counter-turn).** agy first proposed comparing device numbers only. Pointed
  at `TODO.md`'s measurement (a bind mount reports the same device as its target), it agreed this misses the
  motivating case, and named `STATX_ATTR_MOUNT_ROOT` with the warn/strict fallback adopted here.
- **A's failure policy (negotiation, one round).** The positions were:
  - agy: always refuse;
  - driver: the weak-identity rule.

  agy showed that the driver's third reason was weak (the existing checks do not stop the sibling writes). It then
  proposed handle-based canonicalisation, which removes the permission failures the driver had worried about. The
  common option, adopted by the owner, is in Part A.
- **Part P's placement (plan, 2026-10-07).** Measured on the fake (`set_no_replace_support(false)` and a tree run):
  with the probe placed as this spec first approved it, after the workspace exists, the run stops earlier, at
  `publish_workspace`'s own `rename_no_replace` (`state.rs`), with a plain `RunError::Failed` (step `State`, exit 1)
  and `<id>.creating` left behind. On the motivating filesystem (WSL 9p, `EINVAL` for a free name) the probe as
  approved was therefore unreachable. agy weighed four options:
  - literal: keep the placement and the `noreplace-probe` file as written (unreachable, as measured);
  - the workspace publish is the probe: no `noreplace-probe` file, `publish_workspace`'s own refusal is the probe;
  - both: the publish as the probe, plus the file;
  - a tolerant `rename_replace` for the workspace publish.

  agy recommended folding the probe into the workspace publish (no `noreplace-probe` file). It also named a fifth
  placement, the probe inside `<id>.creating`. The owner declined the recommendation and adopted that placement,
  keeping section 241.5's letter (a `noreplace-probe` file) while making it reachable. Built as `probe_no_replace`,
  called from `TreePlace::create`, with `refuse_no_replace` and `unwind_creating` for the refusal. A consequence the
  table records: a probe file that stays after a successful run leaves `<id>.removing` non-empty, cleaned by a later
  cut.
- **Part A's reachable value (plan, 2026-10-07).** The CLI canonicalizes both roots before the engine sees them, so
  the parent-link case is refused lexically at the CLI. Part A still closes the engine API and the check-then-use
  window; its tests drive the engine directly.
- **Rejected test ideas.** Merging into `/sys` or `/dev/shm`, and a Windows junction for M: none exercises the new
  code (Windows junctions are already refused).

End of the cut 8a design.
