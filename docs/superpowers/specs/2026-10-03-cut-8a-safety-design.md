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
| A | A destination reached through a symlink in a parent component into a source subdirectory passes the pre-flight, so files are written into the source before the walk's own check stops the run. | none yet (found in this cut's design consult) |

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
5. **P:** exactly as spec section 241.5 states it. It runs once the workspace exists and before the walk, on every
   run.
6. **Tests:** the in-memory fake for every rule, plus a real bind mount on Linux CI and a real junction on Windows CI.

## Part P: the no-replace probe

The normative text is spec section 241.5 (`FLUX_FULL_UPDATED_SPEC_V16.md`, around lines 10874-10887).

**When it runs.** In `run::tree` (`crates/flux-core/src/run/mod.rs`), after `open_operation` has made the workspace
and written the lock record, and before step 6 (`copy_tree_at`). Before that point the workspace the probe must
write into does not exist. After it, the copy has started. It runs on every run, `--restart` included: a restart may
meet a different filesystem under the same name.

**What it does.** The run's section 99 guard runs before its first write, as before every destination mutation.
1. It stages a temporary in the operation's workspace.
2. It publishes the temporary onto the fixed name `noreplace-probe` with `DirHandle::rename_no_replace`, the same
   primitive every tree file publishes with.
3. It removes `noreplace-probe`, or, if the publish failed, the staged temporary: the probe removes whatever it
   wrote.

**Its outcomes:**

| Outcome | Result |
|---|---|
| The publish succeeds and the removal succeeds | The run continues. |
| The publish fails as "primitive unavailable" (the same classification `primitive_unavailable` in `tree.rs` applies at `CopyStep::Publish`) | The run is refused with `NOREPLACE_PUBLISH_UNAVAILABLE` and ends `Ended::RefusedUnchanged`: rollback removes the workspace, the lock, and DEST if this run created it. Exit 3. |
| The publish fails for any other reason | The run fails with that error, through the run's existing failure path. |
| What the probe wrote (`noreplace-probe`, or the temporary) cannot be removed | A warning naming it. The file goes with the workspace when cleanup removes it. If the run is also being refused, it exits 1 instead of 3 (spec section 55). |

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
| Linux | `statx` on the child reports `STATX_ATTR_MOUNT_ROOT` in its attributes; or, where the attribute mask does not include it, the child's device number differs from the parent's | the mask does not include the attribute and the device numbers are equal |
| macOS | the child's device number differs from the parent's | never |
| Windows | never: a mounted-volume folder is a name-surrogate reparse point, which `open_dir` already refuses | never |

macOS has no same-filesystem bind mount of its own, and its mounts (including FUSE and `nullfs`) present a
different device number.

**"Cannot tell".** It follows the weak-identity rule that `enter_dir` already applies (`WeakIdentityWarnings`):

- **Default:** a new warning in the tree's outcome, beside `WeakIdentityWarnings` and rendered the same way: one
  group with a count and the first path, relative to the source root. Then merge.
- **`--safety strict`:** the subtree is refused as above.

**The source-root check stays.** The walk's existing check that a destination directory is not the source root,
by identity, keeps aborting the whole operation. It is a stronger statement than a mount point and is not changed.

## Part A: a destination inside the source, through a link in a parent component

**The rule.** Before anything is created, `locate_tree` (`crates/flux-core/src/run/place.rs`) obtains the canonical
paths of two directories:

- the source root;
- the destination anchor: DEST if it exists, otherwise DEST's parent.

If the anchor equals the source root or lies inside it, compared component by component as `lexically_within`
does, the run is refused with `SAFETY_REJECTED` before the lock is taken. This holds in every safety mode.

**How the paths are obtained.** From open handles, never by resolving the path a second time:

| Platform | Query |
|---|---|
| Linux | `readlink` of `/proc/self/fd/<n>` |
| macOS | `fcntl(F_GETPATH)` |
| Windows | `GetFinalPathNameByHandleW`, which gives both paths in the same form and the same letter case |

The query is asked of the very handle the run then writes through: the anchor handle `locate_tree` opens and hands
on (DEST, or the holder that creates DEST). A path swapped after the check therefore changes nothing the run
touches. The source root is opened for the query the way the walk opens it, and closed after it. A new query is
added to the platform traits for each. On Linux, a `readlink` result ending in `" (deleted)"` means the directory
was removed; it counts as "any other error" below.

**When the query fails:**

| Failure | Default | `--safety strict` |
|---|---|---|
| Unsupported by the filesystem or the system (`ErrorKind::Unsupported`, `ENOSYS`, Windows `ERROR_INVALID_FUNCTION`, `/proc` not mounted) | the lexical floor (which already ran) is the only containment check; a new warning in the tree's outcome says the containment check was degraded, naming the path whose query failed; the run continues | refused with `SAFETY_REJECTED` |
| Any other error (access denied, the directory vanished) | the run aborts with that error at the resolve step | the same |

**What does not change.** The lexical floor (`prepare_source`), the identity pre-flight (`preflight`) and the walk's
dynamic check (`enter_dir`) all stay. Part A adds a check; it removes none.

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
    and rolls back;
  - a probe file that cannot be removed gives the warning, and exit 1 on a refusal;
  - another publish error fails the run;
  - `--restart` probes again.
- **M:**
  - a pre-existing mount root is skipped and reported, and its siblings are copied;
  - a directory this run created is not queried;
  - "cannot tell" warns under default and refuses the subtree under strict;
  - the source-root alias still aborts.
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
  subdirectory. A junction needs no administrator rights. The copy is refused and the source is unchanged.
- **macOS CI job:** the device-number comparison through the platform test of the new query, against a known
  mount (the system's own `/dev` is a separate mount).

The real-system tests skip with a stated reason where the facility is missing (no sudo, no junction support),
rather than passing silently.

## Out of scope

- Everything in 8b: `state.db`, claim records, replacement, `--update`, `--skip-existing`.
- `--dry-run`.
- `--cross-filesystems` and the source-side section 42 rule.

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
- **Rejected test ideas.** Merging into `/sys` or `/dev/shm`, and a Windows junction for M: none exercises the new
  code (Windows junctions are already refused).

End of the cut 8a design.
