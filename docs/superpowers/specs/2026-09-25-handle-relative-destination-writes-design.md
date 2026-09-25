# Handle-relative destination writes (§149.7)

**Status:** design, approved in outline by the owner 2026-09-25. Not a plan: none of this code exists,
so there are no line numbers to cite. The line-level plan waits until the trait lands.

**Scope:** `flux-fs` (a new trait), `flux-platform` (two real implementations), `flux-core` (the fake).
No engine, no CLI. This is a prerequisite for walker cut 4, not part of it.

---

## Why this exists, and why now

`FLUX_FULL_UPDATED_SPEC_V16.md` §149.7 binds the **writer**, as §149.3–149.5 bind the scanner:

> `DEST` itself is resolved once, at start. Below it, the writer creates and opens every destination
> entry relative to a directory handle it holds for the parent, reached by walking down from `DEST`'s
> root without following links (POSIX: `openat` with `O_NOFOLLOW` and `O_DIRECTORY`; Windows:
> handle-relative opens that do not follow reparse points).
>
> If a component under `DEST` is a symlink, junction, or other reparse point that this operation did not
> create, that path is rejected: its actions fail with `SAFETY_REJECTED` and are reported, and the rest
> of the operation continues (exit status 1, Section 55). Symlinks this operation creates are leaves;
> the writer never walks through them. Any other error on the walk (a component that is missing or not a
> directory, or an I/O or permission error) fails that path's actions with the matching code
> (`DESTINATION_ERROR`, `IO_ERROR`, or `PERMISSION_DENIED`, Section 55), `path_scoped` (Section 207).

The walker design currently substitutes a weaker mechanism — capture each destination directory's
identity, re-verify before writing inside it. That narrows the TOCTOU window without closing it, because
check and write remain two operations against a path the kernel re-resolves between them.

**This lands BEFORE cut 4 rather than after, and the reason is not tidiness.** The identity-recheck
substitute is cut 4's work, and the anomaly recording it says that when the handle-relative surface
arrives the substitute should be **deleted rather than kept alongside**. Building it first means writing,
reviewing and then removing a mechanism everyone already agrees is temporary — and on this project's
evidence a mechanism of that size attracts several rounds of review before it is deleted.

The alternative considered and declined: ship cut 4 with **no** mitigation and document the open window.
It saves the same throwaway work, but leaves cut 4 merged with item 114's attack live — replacing a
Flux-created directory with a symlink to `/etc` between creation and descent, writing outside `DEST`.
That breaks the property the engine-boundary split exists for: *every merged state is both safe and
coherent*.

---

## What was measured before this was written

Every design decision below rests on one of these. They were measured, not reasoned, because the two
platforms turn out to behave in **opposite** ways and an assumption of symmetry would have produced a
wrong spec.

### POSIX — the open refuses

`openat(dirfd, name, O_RDONLY | O_NOFOLLOW | O_DIRECTORY)`, measured on Linux:

| child | result |
|---|---|
| real directory | **opens** |
| symlink to a directory, in-tree | `ENOTDIR` |
| symlink escaping to `/etc` | `ENOTDIR` |
| plain file | `ENOTDIR` |
| missing | `ENOENT` |

**`ELOOP` is not what happens**, despite being the error POSIX associates with `O_NOFOLLOW`: with
`O_DIRECTORY` also set, Linux reports the type mismatch first.

**The consequence: `ENOTDIR` conflates a symlink with an ordinary non-directory**, and §149.7 needs those
reported differently — `SAFETY_REJECTED` versus `DESTINATION_ERROR`. `fstatat` with
`AT_SYMLINK_NOFOLLOW` separates them (`S_ISLNK` true for the symlink, false for the plain file), so the
POSIX arm needs one extra stat **on the error path only**. The happy path is unchanged.

### Windows — the open SUCCEEDS

`NtCreateFile` with `OBJECT_ATTRIBUTES.RootDirectory` set to the parent handle, and
`FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT`, measured unelevated:

| child | NTSTATUS | |
|---|---|---|
| real directory | `0x00000000` | SUCCESS |
| plain file | `0xC0000103` | `STATUS_NOT_A_DIRECTORY` |
| **junction** | **`0x00000000`** | **SUCCESS — it opens** |
| missing | `0xC0000034` | `STATUS_OBJECT_NAME_NOT_FOUND` |

**This is the inversion.** `FILE_OPEN_REPARSE_POINT` means *"open the reparse point itself rather than
following it"*, so it hands back a handle **to the junction**. Windows cannot rely on the open failing.

**Nor is dropping the flag an alternative:** without it `NtCreateFile` **follows** the junction, which is
precisely the traversal §149.7 forbids. There is no Win32 or NT flag that makes the open fail on a
reparse point.

So the Windows arm must **open, then inspect**: having opened with `FILE_OPEN_REPARSE_POINT`, query the
handle and reject if the object carries `FILE_ATTRIBUTE_REPARSE_POINT`. The safety is the same; the
mechanism is not.

### Privileges — a non-issue

`SeChangeNotifyPrivilege` ("bypass traverse checking") is **Enabled** for an ordinary unprivileged user;
Windows grants it to Everyone by default. The probe above ran unelevated. This was raised as a risk
during design review and is retired by measurement.

### Dependencies

`NtCreateFile` and `OBJECT_ATTRIBUTES` are already in the pinned `windows-sys` 0.61.2 — no new crate.
Three features must be enabled that the workspace does not enable today: `Win32_Security`,
`Wdk_Foundation`, `Wdk_Storage_FileSystem`. Verified to compile and link.

On POSIX, `rustix` is already a `cfg(unix)` dependency and already exposes what is needed.
**`rename_no_replace` is ALREADY shaped for this**: it calls
`renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE)`, and those `CWD` arguments are exactly where
real directory file descriptors belong.

---

## The shape: a new trait, and `FileSystem` untouched

Three shapes were weighed. The chosen one was proposed during the design consult and is not the one this
design started with.

| | |
|---|---|
| **Widen `FileSystem`** with handle-relative variants of its seven destination methods | roughly doubles a 10-method trait and forces all **four** implementors to answer — including `NullFs` and `EscapingFs`, two test stubs with no business holding handles |
| **Replace** the path-based methods | smallest surface, but breaks every implementor AND every existing caller, for a property only the writer needs |
| **A separate trait** ← chosen | `FileSystem` is untouched; only the writer opts in |

The reason the third wins is that §149.7 binds **the writer**, not the filesystem abstraction. A reader
walking the source has no use for handle-relative opens, and `NullFs` exists to prove the trait compiles.
Making two stubs implement a handle API to satisfy a property neither is used to test is cost with no
return.

```rust
/// An open directory handle under DEST. Every destination entry is created or opened
/// relative to one of these, never by path.
pub trait DirHandle: Sized {
    /// Open a child DIRECTORY, refusing to traverse a symlink, junction or other
    /// reparse point. This is the operation §149.7 is about.
    fn open_dir(&self, name: &OsStr) -> Result<Self>;

    /// Create a child directory and return a handle to it.
    fn create_dir(&self, name: &OsStr) -> Result<Self>;

    /// Create a child file exclusively, as `create_new` does by path.
    fn create_new(&self, name: &OsStr) -> Result<Self::Writer>;

    /// Metadata for a child, judging the NAME and never its target.
    fn metadata(&self, name: &OsStr) -> Result<Metadata>;

    fn remove_file(&self, name: &OsStr) -> Result<()>;

    /// Publish `from` in this directory onto `to` in `other`, atomically and without
    /// replacing. The two-handle form is what makes staging-then-publish expressible.
    fn rename_no_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()>;
    fn rename_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()>;

    type Writer: FileHandle;
}

/// Resolving DEST once, at start, is the only path-based operation in the writer.
pub trait DestinationRoot: FileSystem {
    type Dir: DirHandle<Writer = Self::Writer>;
    fn destination_root(&self, path: &Path) -> Result<Self::Dir>;
}
```

`StdFileSystem` implements both traits. `FaultFs` implements both. `NullFs` and `EscapingFs` implement
neither and are untouched.

**Names take `&OsStr`, not `&Path`, and that is load-bearing.** A handle-relative call accepts ONE
component. Taking a `Path` would invite `join`ed multi-component values, and a caller that passes
`"a/b"` would silently reintroduce kernel path resolution for the middle component — the exact hole this
design closes. `OsStr` does not prevent a caller embedding a separator, so implementations reject any
name containing one, or `.` or `..`, with `SAFETY_REJECTED` before touching the filesystem.

---

## Error mapping

The two platforms reach the same outcomes by different routes. This table is the contract; the
per-platform sections below say how each gets there.

| condition | code | scope |
|---|---|---|
| component is a reparse point this operation did not create | `SafetyRejected` | path-scoped, operation continues |
| component missing, or not a directory | `DestinationError` | path-scoped |
| permission denied | `PermissionDenied` | path-scoped |
| any other I/O failure | `IoError` | path-scoped |
| a name with a separator, `.` or `..` | `SafetyRejected` | refused before any syscall |

**Nothing here aborts the operation.** §149.7 says the rest continues at exit 1, which matches the
positional rule cut 4 already uses for per-entry failures.

### POSIX

`openat(O_RDONLY | O_NOFOLLOW | O_DIRECTORY)`. On `ENOTDIR` — which, measured, covers BOTH a symlink and
an ordinary non-directory — issue `fstatat(AT_SYMLINK_NOFOLLOW)` and branch on `S_ISLNK`:
`SafetyRejected` if it is a link, `DestinationError` if it is not. `ENOENT` is `DestinationError`,
`EACCES`/`EPERM` is `PermissionDenied`, everything else is `IoError`.

The extra stat costs nothing on the happy path: it runs only where the open already failed.

### Windows

Its own section, below, because the mechanism differs rather than just the syscall.

---

## The Windows arm

`NtCreateFile` with `OBJECT_ATTRIBUTES.RootDirectory = parent`, `ObjectName` a `UNICODE_STRING` holding
the single component, and `CreateOptions` of
`FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT`.

**Because the open SUCCEEDS on a junction (measured), success is not sufficient.** After a successful
open, query the handle's attributes and reject if `FILE_ATTRIBUTE_REPARSE_POINT` is set. The sequence is
open-then-inspect, and the inspection is not optional:

1. `NtCreateFile` handle-relative with the flags above. **`DesiredAccess` is
   `FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE`** and **`ShareAccess` is
   `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`**. Neither is a free choice: a
   `ShareAccess` of 0 takes the directory EXCLUSIVELY, so every directory the writer walks would
   lock out any other process merely reading it, and the walker would fail on an ordinary machine.
2. `STATUS_SUCCESS` → **read the reparse TAG**, not the attribute bit, and reject only a **name
   surrogate** — `IsReparseTagNameSurrogate(tag)`, i.e. `tag & 0x20000000`. Reject → **close the
   handle** and return `SafetyRejected`. Otherwise the handle is the child directory.

   **Testing `FILE_ATTRIBUTE_REPARSE_POINT` alone is WRONG and would break ordinary machines.**
   MEASURED on the development machine: 14 reparse points exist under `C:\` and the user profile, and
   three of them — `OneDrive`, `Dropbox`, `MagentaCLOUD` — are cloud-sync placeholder roots that are
   NEITHER junctions NOR symlinks. Rejecting on the bit would refuse to write into the user's OneDrive
   folder. The tags settle it:

   | object | tag | name surrogate? |
   |---|---|---|
   | `OneDrive` (cloud placeholder) | `0x9000701A` | **no** — traverse it |
   | `Recent` (junction) | `0xA0000003` = `IO_REPARSE_TAG_MOUNT_POINT` | **yes** — reject |

   The surrogate bit is the documented predicate and is preferred over enumerating
   `IO_REPARSE_TAG_SYMLINK` and `IO_REPARSE_TAG_MOUNT_POINT` by name, because it also covers surrogate
   tags that do not exist yet. Deduplication, WOF and AppX stubs are likewise non-surrogates and must
   traverse.
3. `STATUS_NOT_A_DIRECTORY` (`0xC0000103`) → `DestinationError`.
4. `STATUS_OBJECT_NAME_NOT_FOUND` (`0xC0000034`) → `DestinationError`.
5. `STATUS_ACCESS_DENIED` → `PermissionDenied`. Anything else → `IoError`.

Step 2's close matters: a rejected path must leave no handle open, or a later operation on the same tree
inherits a sharing constraint from a directory it already refused.

**`FILE_OPEN_REPARSE_POINT` must never be dropped**, and a comment at the call site should say so.
Removing it does not relax a check — it makes `NtCreateFile` follow the junction, which is the traversal
§149.7 exists to forbid. That is the one edit to this function that looks like a simplification and is a
safety regression.

**This was an open question and is now closed, because it turned out to be a correctness requirement
rather than a reporting nicety.** An earlier draft left the choice between `FileAttributeTagInformation`
and a plain attribute read to the implementer, on the reasoning that both reach the answer. They do not:
only the former yields the TAG, and without the tag the check rejects every cloud-sync placeholder on
the machine. `FileAttributeTagInformation` is required.

**Volume mount points, and a platform asymmetry worth stating rather than discovering.** A volume mount
point carries `IO_REPARSE_TAG_MOUNT_POINT` — the same tag as a junction — so the surrogate test rejects
it. On POSIX a mount point is not a link at all, so `openat(O_NOFOLLOW)` traverses it silently. The two
arms therefore differ on mounts, and that is the intended outcome on each: §149.7 says a reparse point
this operation did not create is rejected, which a Windows volume mount point is; while on POSIX
crossing a filesystem boundary is §42's job, not §149.7's, and §42 already handles it. Same guarantee,
reached by different mechanisms, and neither arm lets a write escape `DEST`.

---

## The fake

`FaultFs` implements both traits with **synthetic** handles — an internal id naming a node in its
in-memory tree.

**A synthetic handle is not a weaker test here, and it is worth saying why**, because a fake that
satisfies an interface without exercising the property behind it is a real risk. The property
handle-relative writes provide is *"the parent cannot be swapped between resolution and use"*. In
`FaultFs` a handle is an id into the tree rather than a name, so re-pointing the NAME leaves the handle
addressing the original node — which is exactly the real behaviour, reproduced without a kernel. The
fake can therefore stage item 114's attack directly: hand out a handle, swap the name, and assert the
write still lands in the original node, or that the swapped path is refused.

What `FaultFs` cannot test is whether the REAL adapters honour the flags. That is what the per-platform
tests in `crates/flux-platform/tests/` are for, and the split matches what cut 3 already does.

---

## Clauses of §149.7 this design must answer explicitly

Written after auditing the spec text clause by clause, because three of these were implied rather than
stated and one is not this cut's to satisfy.

**"`DEST` itself is resolved once, at start."** `destination_root` is the one path-based call in the
writer, and it DOES follow links — resolving `DEST` is exactly the operation §149.7 exempts. A user who
points `DEST` at a symlink has chosen that destination; the clause protects what is *below* it. The
handle it returns is then the only anchor, and no later call re-resolves `DEST`.

**"Symlinks this operation creates are leaves; the writer never walks through them."** **This cut creates
no symlinks at all** — `DirHandle` has no symlink-creation method, because nothing here needs one. The
clause is therefore vacuously satisfied now, and becomes an obligation on whichever cut adds symlink
recreation: it must create them through a handle as leaves and never pass one to `open_dir`. Recorded
here so that cut inherits the constraint rather than rediscovering it.

**A handle whose directory is removed mid-operation.** The handle stays valid and the directory keeps
its identity; on POSIX it becomes an unlinked-but-open inode, and on Windows the delete is deferred
while a handle is open. Writes through it then land in a directory no longer reachable by name. That is
NOT a safety failure — nothing escapes `DEST` — but it is silent, so an operation that wrote into a
removed directory should surface as a per-path failure when the publish fails, rather than being
detected up front. No extra check is specified: adding one would cost a stat per write to catch a case
the publish already reports.

**Relative names only.** Enforced before any syscall, as stated above. This is the clause with no
counterpart in the spec text: §149.7 assumes single components because `openat` does, and an API in Rust
has to say so.

## Out of scope

- **The engine.** `copy_tree` consuming this is cut 4.
- **`copy_file`'s signature.** It stays path-based for now. A handle-taking form is cut 4's decision,
  since that is where the caller that has a handle appears. Changing it here would alter a public entry
  point with no consumer for the new shape.
- **The source side.** §149.3–149.5 bind the scanner and are already satisfied by the walk.
- **Removing the identity-recheck mitigation**, which has not been written and now never will be.

## What this leaves open

- The step-2 attribute query on Windows (above).
- Whether `open_dir` should return the reparse TAG on refusal, for reporting. No consumer yet.
- Nested junction traversal on Windows was not measured — only a single junction directly under the
  parent. The design does not depend on it, because each component is opened one at a time, but it is
  untested ground.

---

## Delivery

One cut, `flux-fs` + `flux-platform` + `flux-core`, no engine and no CLI — the same shape as the atomic
publication cut, and for the same reason: a platform primitive with multiple implementations and its own
test surface is reviewable on its own terms and unreviewable buried inside an engine.

Cut 4's plan is written after this merges, against code that exists.
