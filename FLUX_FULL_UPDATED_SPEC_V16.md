# Flux --- Full Updated Engineering Specification

**Project:** Flux\
**Revision:** V16\
**Status:** Implementation-ready architectural specification\
**Language:** Rust\
**Platforms:** Linux, macOS, Windows\
**Primary command:** `flux copy`

------------------------------------------------------------------------

# Revision Notice

This is revision **V16** of the specification.

Revision history:

-   **V13** integrated the Architecture Review Panel protocol-completeness
    resolutions: Section 222 (deterministic orphaned operation-workspace
    collection predicates), Section 218 (durable location and lifecycle
    of `cleanup_pending` for isolated single-file transfers), Section 206
    (normative default retry limit of 3 retries), and Section 113.1
    (trigger for subtracting reclaimed capacity from the Stage A
    forecast). Its invariants follow Section 256.
-   **V14** closed scheduler, retry, and bounded-memory ambiguities in the
    "V14 Normative Integration" section after Section 258.
-   **V15** closed implementation-blocking ambiguities in Section 259;
    its status is Section 260.
-   **V16** resolves contradictions found in a full read of V15. Unlike
    earlier revisions, V16 amends the affected sections **in place**, so
    no section still states a superseded rule:
    1.  Path ordering: `FluxPathKey` order is component-wise, encoded as
        path components joined by `0x00` (Sections 2, 7, 70, 103, 241,
        245).
    2.  The single-file state artifact is named
        `target.flux-state.<operation-id>` everywhere (Sections 18.1,
        213, 214, 218, 220, 234, 239).
    3.  `TopologyState` is payload-bearing and attempt-fenced, and
        `Failed` is terminal (Sections 90, 91, 92, 123, 142, 147.2,
        201--206, 253.3, 259.1, V14.3.1).
    4.  Fallback materialization is limited to path-scoped failures and a
        shared per-group retry budget; the failed canonical is linked to
        the anchor; terminal failure waits until no further member can
        appear (Sections 89, 152, 206, 207, 253, 256, 259.2).
    5.  The standalone catalog lives in the target's parent directory,
        and `flux cleanup` is non-recursive (Sections 24.4, 250, 251,
        257, 258).
    6.  Error-code names are unified, and Section 55 is the normative
        error-code registry.
    7.  Version labels are corrected, and options used elsewhere are
        listed in Sections 5 and 60.
    8.  Multiple source roots share one key space: each root maps to a
        distinct destination prefix, keys are destination-relative, and
        the manifest records one mapping per root (Sections 7.2, 18.3,
        19, 103, 121).

    V16 adds acceptance tests 31--45 to Section 259.14.

Where sections conflict, later closure layers control earlier ones, and
payload-bearing definitions control state-name summaries (Section
V14.4).

These rules are normative. Implementation choices may vary only where
they do not alter the stated invariants or recovery semantics.

------------------------------------------------------------------------

# 1. Purpose

Flux is a high-performance, safe, topology-aware filesystem transfer
engine written in Rust. It is inspired by FastCopy but uses a new
architecture designed around correctness, bounded resident memory,
persistent operation state, deterministic traversal, resumability, and
filesystem-native acceleration.

Primary goals:

-   high throughput
-   correctness before performance
-   bounded resident RAM
-   deterministic behavior
-   scalable persistent planning state
-   hardlink topology preservation
-   robust resumability
-   source-mutation detection
-   atomic publication where feasible
-   filesystem-native optimizations
-   clear human and machine-readable reporting
-   cross-platform behavior with explicit capability detection

The conceptual pipeline is:

``` text
SOURCE
  │
  ▼
DETERMINISTIC SCANNER
  │
  ▼
SELECTION / FILTERING
  │
  ▼
BOUNDED DISCOVERY STREAM
  │
  ▼
TOPOLOGY + PLANNING STATE
  │
  ▼
BOUNDED TRANSFER QUEUE
  │
  ▼
PARALLEL WORKERS
  │
  ▼
STREAMING HASH / VERIFICATION
  │
  ▼
COMMIT / ATOMIC RENAME
  │
  ▼
REPORT
```

------------------------------------------------------------------------

# 2. Foundational Invariants

The following are architectural invariants rather than optional
implementation details.

1.  Excluded paths never become transfer actions.
2.  Hardlink topology is considered only among selected entries.
3.  Hardlinks are identified using filesystem object identity, never
    path strings alone.
4.  The default scanner is deterministic.
5.  The canonical hardlink member is the first selected path encountered
    for an object identity.
6.  Because the default scanner is deterministically ordered, the
    canonical member is the smallest selected normalized relative path
    in `FluxPathKey` order (component-wise; Sections 7.2 and 103).
7.  Flux does not buffer an entire hardlink group in RAM merely to
    determine its canonical member.
8.  Persistent topology state may spill to disk.
9.  Resident RAM remains bounded by configured queues, workers, buffers,
    and bounded caches.
10. Scanner backpressure is mandatory.
11. The scanner never creates an unbounded global list of discovered
    files.
12. Atomic capacity planning does not require scanning the entire source
    tree first.
13. `--atomic=always` never silently falls back to in-place replacement.
14. Resume never trusts destination size alone.
15. Timestamp equality alone is insufficient for strong resume
    validation.
16. Chunk/checkpoint validation is available for robust resumability.
17. Source mutation must never silently result in a successful atomic
    copy.
18. Copy-time hashing occurs during the streaming copy.
19. Required verification completes before final publication.
20. Hardlink dependents cannot execute before their canonical target
    exists.
21. Symlink targets never contribute to topology unless explicit
    link-following behavior is enabled.
22. Safety checks use filesystem identity and object identity where
    available, not only lexical path comparisons.
23. Platform optimizations have safe fallbacks or explicit strict
    failure.
24. The core transfer engine is independent of CLI presentation.
25. Persistent operation state is destination-local, operation-scoped,
    versioned, crash-persistent, lock-protected, and
    garbage-collectable.
26. Completed operations leave no Flux control state inside the copied
    destination namespace unless explicitly configured otherwise.

------------------------------------------------------------------------

# 3. Workspace

Recommended Rust workspace:

``` text
flux/
├── Cargo.toml
├── crates/
│   ├── flux-core/
│   ├── flux-cli/
│   ├── flux-fs/
│   ├── flux-platform/
│   ├── flux-hash/
│   └── flux-ui/
├── tests/
├── benches/
├── docs/
├── examples/
└── README.md
```

## 3.1 `flux-core`

Contains:

-   scanner
-   selection
-   filesystem identity
-   topology
-   persistent operation state
-   planner
-   scheduler
-   workers
-   copy
-   hardlinks
-   reflinks
-   sparse files
-   verification
-   resume
-   statistics
-   errors

## 3.2 `flux-fs`

Portable filesystem abstractions.

## 3.3 `flux-platform`

Linux/macOS/Windows implementations.

## 3.4 `flux-hash`

Streaming hashing. MVP algorithm: BLAKE3.

## 3.5 `flux-ui`

Progress and terminal presentation.

## 3.6 `flux-cli`

CLI parsing and command dispatch.

------------------------------------------------------------------------

# 4. CLI

Initial commands:

``` text
flux copy
flux verify
flux benchmark
flux cleanup
flux version
```

Future:

``` text
flux sync
flux restore
```

Basic copy:

``` bash
flux copy SOURCE DEST
```

------------------------------------------------------------------------

# 5. Copy Options

Planned interface:

``` text
--recursive
--no-recursive

--workers <N|auto>

--exclude <PATTERN>                  (Section 14)

--overwrite
--update
--skip-existing

--verify[=<ALGORITHM>]

--preserve-times
--preserve-permissions
--preserve

--hardlinks=<auto|preserve|copy>

--reflink=<auto|always|never>

--sparse=<auto|always|never>

--atomic=<auto|always|never>

--resume
--resume-verify=<metadata|chunks|full>
--retries=<N|unlimited>              (Section 206; default 3)

--durability=<normal|strict>         (Sections 141, 165)

--links=<copy|follow|skip>

--special-files=<skip|strict>        (Section 233; default skip)

--cross-filesystems

--heartbeat-interval <duration>      (Section 101; future)
--lease-timeout <duration>           (Section 101; future)

--dry-run

--json

--quiet
-v
-vv
-vvv
```

Not every option must be exposed in Flux 0.1, but internal APIs must be
designed so these semantics can be added without redesigning the engine.

------------------------------------------------------------------------

# 6. Deterministic Scanner

The scanner emits selected paths in deterministic order.

The ordering key is the normalized relative path.

Path ordering must be:

-   locale-independent
-   byte/code-unit deterministic
-   independent of the host user's locale
-   independent of directory enumeration order

Filesystem case-sensitivity semantics must remain separate from ordering
semantics.

------------------------------------------------------------------------

# 7. Canonical Path Ordering

Flux must explicitly define a platform-independent deterministic
ordering.

## 7.1 Normalization

The ordering representation must:

-   use `/` as the internal relative-path separator for display and
    destination mapping (the ordering key uses its own separator;
    Section 103)
-   preserve the actual filename bytes/code points where possible
-   not perform Unicode normalization implicitly
-   not perform locale-specific case folding
-   not treat filesystem case-insensitivity as a sorting transformation

## 7.2 Comparison

Primary ordering:

``` text
lexicographic comparison of normalized relative path components
```

Components are compared as unsigned byte strings (Unix: raw filename
bytes; Windows: canonical WTF-8, Section 241.2). A path whose component
sequence is a prefix of another's sorts first, so a directory sorts
before its own contents.

This is **not** the same as a flat byte comparison of the `/`-joined
path. Many legal filename bytes sort below `/` (0x2F), including space,
`-` (0x2D), and `.` (0x2E). For the paths `a/x`, `a/y/z`, `a-b`, `a0`:

``` text
component-wise (normative):   a/x   a/y/z   a-b   a0
flat '/'-joined bytes (NOT):  a-b   a/x     a/y/z a0
```

A depth-first scanner that sorts each directory's entries by name alone
emits component-wise order without buffering. `FluxPathKey` (Section
103) encodes this order so that plain bytewise comparison of keys equals
component-wise comparison. With multiple source roots, paths are ordered
by their destination-relative path and roots are visited in prefix order
(Section 18.3).

For platforms with native Unicode strings, comparison is performed on
the deterministic normalized representation.

## 7.3 Windows case tie-breaker

On Windows, Flux must **not** use NTFS case-insensitivity as the
ordering rule.

If two representable paths differ only by case, the deterministic
ordering uses a stable binary comparison of their normalized path
representation as the tie-breaker.

For example, if:

``` text
Archive.tar
archive.tar
```

are both present in a namespace where that is possible, the ordering
must not depend on locale or Windows collation.

The exact implementation should use the platform-independent encoded
representation selected by the path-ordering module.

The ordering contract must be covered by cross-platform tests.

The important invariant is:

> Filesystem equality semantics and Flux ordering semantics are separate
> concepts.

------------------------------------------------------------------------

# 8. Ordered and Unordered Discovery

## 8.1 Ordered discovery

Default.

Guarantee:

``` text
first selected member of an identity
=
lexicographically smallest selected member
```

This makes deterministic hardlink canonicalization possible without
buffering the entire hardlink group.

## 8.2 Unordered discovery

A future optimization may use unordered discovery.

If unordered discovery is used, Flux must not claim the
lexicographically smallest canonical member unless a separate mechanism
establishes that fact.

Possible implementations:

1.  persistent topology ordering
2.  external sorting
3.  explicit first-discovered semantics

Default Flux behavior remains deterministic.

------------------------------------------------------------------------

# 9. Scanner Backpressure

Pipeline:

``` text
Filesystem Scanner
        │
        ▼
Selection / Filtering
        │
        ▼
Bounded Discovery Queue
        │
        ▼
Topology Planner
        │
        ▼
Bounded Transfer Queue
        │
        ▼
Workers
```

All queues are finite.

When downstream capacity is exhausted:

``` text
scanner blocks/awaits
```

rather than accumulating paths.

The scanner must never construct an in-memory list proportional to the
total number of files.

------------------------------------------------------------------------

# 10. Bounded RAM vs Persistent Planning State

A filesystem may contain an arbitrary number of objects and arbitrary
hardlink topology. Therefore it is impossible to guarantee both:

``` text
all topology information available
```

and:

``` text
fixed-size total storage independent of input size
```

Flux therefore distinguishes:

## 10.1 Resident RAM

Bounded by:

-   queue capacities
-   worker count
-   I/O buffer sizes
-   bounded topology cache
-   bounded metadata cache
-   scheduler state

## 10.2 Persistent planning state

May grow with operation size.

Topology and resume state must be able to spill to disk.

This is a deliberate architecture, not an implementation leak.

------------------------------------------------------------------------

# 11. Filesystem Object Identity

Define:

``` rust
struct FileIdentity {
    filesystem_id: FilesystemId,
    object_id: ObjectId,
}
```

Unix implementations should use suitable device/inode information.

Windows implementations should use native file identity mechanisms.

The identity must distinguish:

``` text
same pathname
```

from:

``` text
same filesystem object
```

and must support hardlink detection.

------------------------------------------------------------------------

# 12. Hardlink Topology

Hardlinks are first-class filesystem relationships.

Example:

``` text
A/file ─────┐
B/file ─────┼── same filesystem object
C/file ─────┘
```

With preservation enabled:

``` text
COPY A/file
LINK B/file → A/file
LINK C/file → A/file
```

Only one physical data transfer is required.

------------------------------------------------------------------------

# 13. Canonical Hardlink Rule

The canonical member is:

> The first selected path encountered by the deterministic scanner for a
> given filesystem identity.

Because the default scanner is deterministically ordered:

``` text
canonical =
lexicographically smallest selected relative path
```

## 13.1 Critical consequence

Flux can begin copying:

``` text
A/file
```

without waiting for:

``` text
M/file
Z/file
```

to be discovered.

When later members arrive, they become dependent link actions.

This resolves the canonical-hardlink paradox without requiring
whole-tree buffering.

------------------------------------------------------------------------

# 14. Hardlink Topology and Exclusions

Topology is preserved over the **selected namespace**, not the entire
source namespace.

Example:

``` text
A/file       ─────┐
B/file       ─────┼── same object
private/file ─────┘
```

Command:

``` bash
flux copy /source /dest --exclude private/**
```

Expected:

``` text
COPY A/file
LINK B/file → A/file
EXCLUDE private/file
```

Flux must never copy an excluded member solely to preserve topology.

Rules:

-   excluded paths never become transfer actions
-   excluded members do not affect canonical selection
-   one selected member is copied normally
-   multiple selected members may be linked
-   strict hardlink preservation fails if required topology cannot be
    created

------------------------------------------------------------------------

# 15. Hardlink Modes

``` bash
--hardlinks=auto
--hardlinks=preserve
--hardlinks=copy
```

## `auto`

Preserve when possible.

If preservation is impossible, Flux may degrade to ordinary copies and
must report the degradation.

## `preserve`

Topology preservation is mandatory.

Failure to create a required hardlink is an operation failure.

## `copy`

Never create destination hardlinks.

Default:

``` text
auto
```

------------------------------------------------------------------------

# 16. Hardlink Dependencies

Represent:

``` text
COPY canonical
      │
      ├── LINK member 2
      ├── LINK member 3
      └── LINK member N
```

A dependent link cannot execute until its canonical destination object
exists.

The scheduler must understand this dependency.

Resume state tracks:

``` text
canonical materialization
```

separately from:

``` text
dependent link materialization
```

------------------------------------------------------------------------

# 17. Topology Store

A pure in-memory map is not sufficient for arbitrarily large operations.

Define:

``` rust
trait TopologyStore {
    fn lookup(identity: FileIdentity) -> Result<Option<TopologyRecord>>;
    fn insert(identity: FileIdentity, record: TopologyRecord) -> Result<()>;
}
```

The production implementation should provide:

``` text
bounded memory cache
        +
disk-backed index
```

Requirements:

-   deterministic
-   versioned
-   bounded resident RAM
-   efficient identity lookup
-   crash-aware
-   resumable
-   safely removable
-   isolated per operation

------------------------------------------------------------------------

# 18. Persistent Operation Workspace

Flux state is **destination-local and operation-scoped**.

It must never depend on a global system-wide Flux database.

## 18.1 Single-file transfer

For:

``` bash
flux copy source.iso /dest/source.iso
```

Flux may use adjacent temporary/state files:

``` text
/dest/
├── source.iso.flux-partial.<operation-id>
└── source.iso.flux-state.<operation-id>
```

The name structure `<target>.flux-partial.<operation-id>` and
`<target>.flux-state.<operation-id>` is normative (Section 218). The
textual form of `<operation-id>` is implementation-defined but must be
collision-resistant and deterministic enough for discovery.

## 18.2 Directory transfer

For:

``` bash
flux copy /data /backup
```

Flux uses:

``` text
/backup/
└── .flux/
    └── operations/
        └── <operation-id>/
            ├── manifest
            ├── state.db
            ├── topology.db
            ├── checkpoints/
            └── lock
```

The `.flux` directory is control state, not copied source content.

## 18.3 Multiple-source operations

Use one operation workspace:

``` text
DEST/.flux/operations/<operation-id>/
```

The manifest maps each source root to its destination mapping.

Each source root `R` maps to a **destination prefix**: the path, relative
to the destination root, under which `R`'s content is placed.

-   A single root that maps onto the destination root itself has an
    empty prefix.
-   With multiple roots, each root maps to `DEST/<name>`, where `<name>`
    is the root's final path component. Every prefix is then exactly one
    component, and prefixes must be pairwise distinct. Two roots with
    the same prefix are rejected with `DESTINATION_NAMESPACE_COLLISION`
    before any transfer begins.

A path's `FluxPathKey` is its destination-relative path: its root's
prefix components followed by its components relative to that root
(Section 103). All roots therefore share one key space and one order.

The scanner visits roots in `FluxPathKey` order of their prefixes, and
each root depth-first as in Section 7.2. Because prefixes are distinct
single components, this emits global key order across all roots without
buffering. A hardlink group whose members span roots on the same source
filesystem therefore has one canonical member: the smallest
destination-relative key (Section 13). Members on different source
filesystems are never grouped (Section 230).

The order depends only on the prefixes, not on the order in which roots
appear on the command line.

------------------------------------------------------------------------

# 19. Operation Manifest

Each persistent operation has a manifest.

Conceptual Rust structure:

``` rust
struct OperationManifest {
    format_version: u32,
    operation_id: OperationId,

    destination_root: PathBuf,
    // One entry per source root (Section 18.3), stored in
    // FluxPathKey order of destination_prefix.
    roots: Vec<RootMapping>,

    created_at: Timestamp,
    last_checkpoint: Timestamp,

    state: OperationState,

    configuration_fingerprint: Hash,
}

struct RootMapping {
    source_root: PathBuf,
    // Empty for a single root mapped onto destination_root itself.
    destination_prefix: FluxPathKey,
}
```

The configuration fingerprint prevents accidental resume with
incompatible semantics.

Examples of potentially incompatible changes:

``` text
--hardlinks=preserve → --hardlinks=copy
chunk size changed
verification policy changed
atomic policy changed
reflink policy changed
sparse policy changed
metadata policy changed
```

Flux must either support explicit migration or reject incompatible
resume attempts.

------------------------------------------------------------------------

# 20. Operation Lifecycle

Persistent operation states:

``` text
CREATED
SCANNING
TRANSFERRING
PAUSED
FAILED
COMMITTING
COMPLETED
ABANDONED
```

Normal completion:

``` text
COMPLETED
    ↓
remove temporary files
    ↓
remove checkpoint state
    ↓
remove topology store
    ↓
remove operation workspace
```

Abrupt termination:

``` text
TRANSFERRING
    ↓
SIGKILL / power loss
    ↓
workspace remains
    ↓
next --resume discovers operation
```

------------------------------------------------------------------------

# 21. Resume Discovery

A new invocation:

``` bash
flux copy /data /backup --resume
```

must not scan arbitrary `.flux-state` files across the filesystem.

It first checks:

``` text
/backup/.flux/operations/
```

and loads operation manifests.

The requested source/destination pair is matched against the manifest.

Resume is allowed only when:

``` text
source mapping matches
destination mapping matches
operation state is resumable
format is supported
configuration is compatible
```

------------------------------------------------------------------------

# 22. Single-file Resume Discovery

For:

``` bash
flux copy source.iso /dest/source.iso --resume
```

Flux checks the destination-side operation metadata for that exact
target.

The state file must contain:

``` text
operation_id
source identity
destination target
partial-file identity where available
state version
```

Flux must not infer validity merely because a filename happens to look
like a Flux partial file.

------------------------------------------------------------------------

# 23. Partial File Mapping

A partial file and state record form a pair.

The state records:

``` text
operation_id
logical destination
temporary path
source identity
source metadata
bytes validated
chunk size
checkpoint information
```

On discovery:

``` text
state → temporary file
```

must be validated.

A missing temporary file causes:

``` text
RESUME_INVALID
```

unless the state indicates the operation had already reached a committed
stage.

------------------------------------------------------------------------

# 24. Orphan Topology Store Management

Power loss or `SIGKILL` can leave:

``` text
.flux/operations/<operation-id>/
```

behind.

Flux therefore requires conservative orphan management.

## 24.1 Lock

Every active operation holds an exclusive operation lock.

## 24.2 Heartbeat / lease

The operation periodically updates a heartbeat or lease record.

## 24.3 Garbage collection

Automatic GC may remove an operation only when:

1.  the operation lock is not held,
2.  the lease/heartbeat has expired,
3.  the operation has exceeded the retention policy,
4.  it is not actively selected for resume.

Default recommended retention:

``` text
7 days
```

The retention period should be configurable.

## 24.4 Manual cleanup

Command:

``` bash
flux cleanup DEST
```

The `DEST` argument is required unless `--target PATH` is given
(Section 251). It lists stale operations before deletion unless forced.

Example future interface:

``` bash
flux cleanup /backup
flux cleanup /backup --older-than 30d
flux cleanup /backup --force
```

GC must never delete active operations.

------------------------------------------------------------------------

# 25. Symlink Isolation

Symlinks are a separate namespace relationship.

Default:

``` text
copy the symlink itself
```

A symlink target is **not** resolved for hardlink topology.

Example:

``` text
A/file       inode 123
B/link ─────→ A/file
```

Topology store contains:

``` text
inode 123 → A/file
```

and the symlink action is:

``` text
SYMLINK("A/file")
```

It does not become another member of inode 123.

Invariant:

> Only filesystem objects directly discovered as objects by the scanner
> contribute identities to `TopologyStore`.

Following symlinks is opt-in.

------------------------------------------------------------------------

# 26. Symlink Follow Mode

Future:

``` bash
--links=follow
```

requires:

-   cycle detection
-   source-root containment checks
-   mount awareness
-   filesystem identity checks
-   explicit topology semantics
-   protection against symlink labyrinths

The default must remain:

``` text
--links=copy
```

------------------------------------------------------------------------

# 27. Capacity Planning

Atomic replacement can require additional physical allocation.

Example:

``` text
destination file = 500 GB
free space       = 200 GB
```

An atomic temporary copy may be impossible.

Flux supports:

``` bash
--atomic=auto
--atomic=always
--atomic=never
```

## `auto`

Prefer atomic replacement.

If reliable capacity information indicates atomic replacement is
infeasible, Flux may use an in-place strategy where correctness
semantics allow it.

The decision must be reported.

## `always`

Fail rather than fall back.

## `never`

Use in-place replacement.

------------------------------------------------------------------------

# 28. Capacity Planning and Scanner Backpressure

Capacity planning does not require complete source discovery.

Capacity decisions occur when a concrete transfer action becomes
executable.

For a large overwrite, inspect:

``` text
source metadata
destination metadata
filesystem free space
filesystem allocation information
atomic policy
resume state
sparse/reflink policy
```

The scanner can remain backpressured independently.

There is no requirement to know the total size of the complete tree
before copying an individual file.

------------------------------------------------------------------------

# 29. Capacity Reservations

Where supported, Flux may maintain an internal accounting model for
expected temporary allocations.

If the filesystem cannot provide reliable reservation semantics,
estimates are advisory.

Actual allocation failures such as:

``` text
ENOSPC
ERROR_DISK_FULL
```

must always be handled as runtime conditions.

------------------------------------------------------------------------

# 30. Atomic Copy Pipeline

Preferred pipeline:

``` text
STAT SOURCE
    ↓
OPEN TEMP DEST
    ↓
STREAM COPY
    ├── source hash
    └── destination write
    ↓
EOF
    ↓
STAT SOURCE AGAIN
    ↓
SOURCE MUTATION CHECK
    ↓
OPTIONAL DESTINATION VERIFICATION
    ↓
APPLY METADATA
    ↓
OPTIONAL FLUSH
    ↓
ATOMIC RENAME
```

A destination must not appear complete until required verification and
metadata stages have succeeded.

------------------------------------------------------------------------

## 30.1 Atomic Directory Publication

For a directory operation using:

``` text
--atomic=always
```

Flux MUST distinguish directory-tree publication from per-file atomic
replacement.

Where the filesystem provides the required capability, Flux uses:

``` text
source tree
    ↓
complete temporary destination tree
    ↓
copy/verify all descendants
    ↓
apply final directory metadata
    ↓
flush required data/metadata
    ↓
PREPARE_COMMIT
    ↓
WAL durability
    ↓
lock revalidation
    ↓
single atomic root-directory publication
    ↓
destination directory durability
    ↓
COMMIT
```

The directory is therefore published as one completed tree.

Flux MUST NOT describe a sequence of independent file renames as atomic
directory publication.

For an existing non-empty destination directory, an ordinary POSIX-style
directory rename does not universally provide atomic replacement of the
existing tree. If the filesystem does not provide a safe atomic
directory replacement primitive for the requested operation:

``` text
--atomic=always
```

MUST fail rather than deleting the old tree first or silently falling
back to per-file atomic renames.

A suitable failure classification is:

``` text
ATOMIC_DIRECTORY_REPLACE_UNSUPPORTED
```

`--atomic=auto` MAY select an explicitly documented non-atomic or
per-file fallback.

Directory recovery MUST distinguish at least:

``` text
DIRECTORY_STAGING
DIRECTORY_READY_TO_PUBLISH
DIRECTORY_PUBLISHING
DIRECTORY_PUBLISHED
DIRECTORY_METADATA_FINALIZED
COMMITTED
```

If durable evidence cannot establish whether the root publication
occurred:

``` text
COMMIT_STATE_UNCERTAIN
```

must be recorded and the staged/adjacent artifacts preserved until
recovery can reconcile the namespace.

## 30.2 Directory Metadata Ordering

Directory metadata that is sensitive to child mutations MUST NOT be
finalized before all required child creations, removals, renames, and
hardlink creations are complete.

The publication order is:

``` text
child namespace mutations
        ↓
final directory metadata
        ↓
required durability
        ↓
publication/final commit
```

Flux must distinguish:

``` text
published, metadata not yet finalized/durable
```

from:

``` text
published and metadata finalized/durable
```

An unpublished staging directory is never treated as the published
destination merely because its contents are complete.

# 31. Copy-Time Hashing

Hashing is performed during the source read:

``` text
read source buffer
      ↓
hash.update(buffer)
      ↓
write destination buffer
```

At EOF:

``` text
source_digest = finalize()
```

This avoids an unnecessary second source read for source-stream
verification.

Copy-time hashing does not eliminate the option of independent
destination verification.

------------------------------------------------------------------------

# 32. Verification Levels

Conceptual:

``` text
none
source-stream
destination
full
```

## `none`

No content verification.

## `source-stream`

Hash source bytes while copying.

## `destination`

Independently hash destination after writing.

## `full`

Perform the strongest configured verification, potentially including
both source and destination validation.

The CLI must document exactly what guarantee each level provides.

------------------------------------------------------------------------

# 33. Source Mutation Detection

Before copy:

``` text
identity
size
mtime
```

After source reading:

``` text
identity
size
mtime
```

If meaningful identity or metadata changes:

``` text
SOURCE_CHANGED
```

The temporary destination must not be published.

------------------------------------------------------------------------

# 34. Resume State

Resume state is versioned.

Conceptual fields:

``` text
format version
operation id

source filesystem identity
source file identity

source size
source mtime
source change/creation time where available

destination identity where available
destination size

bytes known valid

chunk size
hash algorithm
checkpoint hashes

operation options
configuration fingerprint
Flux version
```

------------------------------------------------------------------------

# 35. Resume Temporal Integrity

Size and mtime can be insufficient because filesystem timestamp
resolution may be coarse.

Therefore Flux uses layered validation.

## 35.1 Metadata validation

Check:

``` text
source exists
identity matches where meaningful
size matches
mtime matches
change/creation time where available
```

## 35.2 Chunk validation

For strong resume:

``` text
completed source chunk → checkpoint hash
completed destination chunk → checkpoint hash
```

Before resuming, Flux validates already-completed source regions and the
corresponding partial destination regions.

This catches mutations that occur inside timestamp-resolution windows.

## 35.3 Full validation

Optional complete source/destination hashing.

If validation fails:

``` text
RESUME_INVALID
```

Flux must not silently continue.

------------------------------------------------------------------------

# 36. Resume Modes

``` bash
--resume-verify=metadata
--resume-verify=chunks
--resume-verify=full
```

Recommended default for large resumable files:

``` text
chunks
```

------------------------------------------------------------------------

# 37. Atomic + Resume

Temporary destination state records:

``` text
operation
source identity
destination target
bytes valid
checkpoint hashes
```

Resume sequence:

``` text
validate state
     ↓
validate source
     ↓
validate partial destination
     ↓
continue copy
     ↓
verify
     ↓
commit
```

After successful publication:

``` text
temporary state removed
```

------------------------------------------------------------------------

# 38. Sparse Files

Options:

``` bash
--sparse=auto
--sparse=always
--sparse=never
```

Default:

``` text
auto
```

Preserve logical holes where supported.

Tests should compare:

``` text
logical size
```

and, where available:

``` text
physical allocation
```

------------------------------------------------------------------------

# 39. Reflinks / CoW

Options:

``` bash
--reflink=auto
--reflink=always
--reflink=never
```

Default:

``` text
auto
```

`auto` attempts CoW and safely falls back.

`always` fails when unavailable.

`never` uses ordinary copying.

------------------------------------------------------------------------

# 40. Special Files

MVP:

``` text
regular files
directories
symlinks
```

Future:

``` text
FIFOs
sockets
device nodes
Windows reparse points
xattrs
ACLs
alternate data streams
```

Flux must never silently convert a special filesystem object into an
ordinary regular file.

------------------------------------------------------------------------

# 41. Filesystem Safety

Path strings alone are insufficient.

Safety checks must consider:

-   filesystem identity
-   object identity
-   ancestor relationships
-   mount transitions
-   symlink traversal
-   destination containment

Reject:

``` text
source == destination
```

and dangerous cases such as:

``` text
/data
/data/backup
```

where the destination is nested inside the source.

The default behavior should favor rejection rather than complicated
self-exclusion.

------------------------------------------------------------------------

# 42. Mount Boundaries

Recommended default:

``` text
do not cross filesystem boundaries
```

Future:

``` bash
--cross-filesystems
```

Filesystem identity must be used to evaluate boundaries.

------------------------------------------------------------------------

# 43. Paths

Use:

``` rust
std::path::Path
std::ffi::OsStr
```

Never require UTF-8 internally.

Tests must cover:

-   Unicode
-   emoji
-   spaces
-   quotes
-   newlines
-   long paths
-   unusual valid Unix filenames

Windows should use native wide-character APIs where appropriate.

------------------------------------------------------------------------

# 44. Metadata

Define:

``` rust
trait MetadataProvider {
    fn read_metadata(path: &Path) -> Result<FileMetadata>;
    fn apply_metadata(path: &Path, metadata: &FileMetadata) -> Result<()>;
}
```

MVP:

``` text
timestamps
permissions where portable
```

Future:

``` text
owner/group
ACL
xattrs
file flags
DOS attributes
alternate data streams
```

------------------------------------------------------------------------

# 45. Transfer Action IR

The planner produces explicit actions:

``` rust
enum TransferAction {
    CreateDirectory { /* ... */ },
    CopyFile { /* ... */ },
    LinkFile { /* ... */ },
    ReflinkFile { /* ... */ },
    SetMetadata { /* ... */ },
    VerifyFile { /* ... */ },
    Skip { /* ... */ },
}
```

Actions support dependencies.

------------------------------------------------------------------------

# 46. Transfer Operation Interface

``` rust
pub trait TransferOperation {
    fn plan(&self) -> Result<TransferPlan>;
    fn execute(&self, plan: TransferPlan) -> Result<TransferReport>;
}
```

Future operations:

``` text
CopyOperation
SyncOperation
VerifyOperation
RestoreOperation
```

The implementation must avoid an API that requires a complete in-memory
`TransferPlan` for arbitrarily large trees. `TransferPlan` should be
streamable or backed by persistent state.

------------------------------------------------------------------------

# 47. Streaming Planner

For scalability, the planner should operate as a stream:

``` text
discovery record
      ↓
topology lookup
      ↓
action decision
      ↓
action emission
```

It should not require:

``` text
all discovery records
```

to exist simultaneously.

Persistent operation state acts as the durable backing store for
information that must survive process failure or be revisited later.

------------------------------------------------------------------------

# 48. Worker Architecture

Never use:

``` text
one thread per file
one unbounded task per file
```

Use:

``` text
Scanner
  ↓
bounded queue
  ↓
Planner
  ↓
bounded queue
  ↓
Worker pool
```

Worker execution remains parallel while topology dependencies are
respected.

------------------------------------------------------------------------

# 49. Scheduling

Initial file classes:

``` text
tiny      < 64 KiB
small     < 1 MiB
medium    < 64 MiB
large     < 1 GiB
huge      >= 1 GiB
```

Future scheduler may adapt to:

-   file size
-   storage latency
-   queue depth
-   observed throughput
-   worker utilization
-   filesystem behavior

------------------------------------------------------------------------

# 50. Small Files

Future:

``` text
SmallFileBatcher
```

Possible strategies:

``` text
direct
batched
packed
```

Packing must not alter individual-file correctness semantics.

------------------------------------------------------------------------

# 51. Statistics

Track:

``` text
files discovered
files selected
files copied
files skipped
files overwritten
files hardlinked
files reflinked
files degraded
files verified
files mismatched
files failed

bytes discovered
bytes copied
bytes skipped

elapsed time
current throughput
average throughput
peak throughput
```

For hardlinks distinguish:

``` text
logical files
physical bytes transferred
links created
```

------------------------------------------------------------------------

# 52. Progress UI

Example:

``` text
Flux 0.1.0

Copying 12,842 files
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ 63%

Transferred   487.2 GiB / 771.4 GiB
Speed         2.84 GiB/s
ETA           1m 41s
Files         8,321 / 12,842
Skipped       312
Hardlinks     1,284
Degraded          0
Errors            0
```

Progress must not require the entire file list in RAM.

When total work is not yet known:

``` text
Discovered: 123,481
Processed:   98,412
```

------------------------------------------------------------------------

# 53. JSON Output

``` bash
--json
```

Example:

``` json
{
  "files_total": 12842,
  "files_copied": 8321,
  "files_skipped": 312,
  "files_hardlinked": 1284,
  "files_reflinked": 0,
  "files_degraded": 0,
  "bytes_total": 828124124124,
  "bytes_copied": 523124124124,
  "errors": 0,
  "duration_ms": 183421,
  "average_bytes_per_second": 2841241241
}
```

JSON goes to stdout.

Human progress/logging goes to stderr.

------------------------------------------------------------------------

# 54. Logging

Use:

``` text
tracing
tracing-subscriber
```

Levels:

``` text
error
warn
info
debug
trace
```

CLI:

``` bash
-v
-vv
-vvv
--quiet
```

------------------------------------------------------------------------

# 55. Error Model

Suggested exit codes:

``` text
0 = complete success
1 = transfer/verification errors
2 = usage/configuration errors
```

Error code registry (normative). Every failure or result code used in
this specification appears here. A change that introduces a new code
MUST add it to this table, and MUST NOT introduce a synonym for an
existing code.

| Code | Meaning | Defined in |
|------|---------|------------|
| `ARTIFACT_OWNERSHIP_UNCERTAIN` | An adjacent artifact's state record is missing, malformed, or unverifiable; the artifact is preserved, never adopted or deleted. | 239.3, 249.4 |
| `ATOMIC_DIRECTORY_REPLACE_UNSUPPORTED` | `--atomic=always` on an existing directory, with no safe whole-tree replacement primitive. | 30.1, 259.9 |
| `BLOCKED_BY_CANONICAL_FAILURE` | A hardlink dependent was not linked because its group reached terminal `Failed`. | 92, 205 |
| `CANONICAL_RETRY_EXHAUSTED` | The per-group attempt budget is spent; recorded as the cause in `Failed.error_code`. | 206, 253.5 |
| `COMMIT_STATE_UNCERTAIN` | Recovery cannot establish whether a publication or rename happened; state is preserved for reconciliation. | 30.1, 259.8 |
| `CONTROL_PLANE_NAMESPACE_CONFLICT` | The destination control-plane path (`.flux`) holds a foreign object that Flux does not own. | 250, 259.3 |
| `CONTROL_STATE_DURABILITY_FAILURE` | The emergency control reserve is unavailable, exhausted, or corrupt, so a pause or failure cannot be durably recorded. | 231.5 |
| `COPY_FAILED` | The content copy of an independent file, or a canonical attempt, failed. | 92, 133 |
| `DESTINATION_ERROR` | A destination-side failure not covered by a more specific code. | 55 |
| `DESTINATION_NAMESPACE_COLLISION` | Two distinct source paths, or two source roots, map to the same destination object or prefix. | 18.3, 241.5 |
| `DIRECTORY_CHANGED_DURING_SCAN` | An existing directory's identity changed while it was being entered. | 149.4 |
| `DISK_FULL` | A destination data allocation failed for lack of space (`ENOSPC`, `ERROR_DISK_FULL`). | 29, 55 |
| `FAILED_ATOMIC_CAPACITY` | Required atomic temporary capacity provably exceeds the maximum recoverable capacity; terminal for the action. | 254.3 |
| `HARDLINK_GROUP_UNMATERIALIZABLE` | Reported outcome of a hardlink group that reached terminal `Failed`. | 253.5 |
| `HARDLINK_IDENTITY_UNAVAILABLE` | `--hardlinks=preserve` was requested but reliable object identity is unavailable. | 108 |
| `HARDLINK_UNAVAILABLE` | A required destination hardlink cannot be created. | 15, 55 |
| `INCOMPATIBLE_STATE` | Resume state is incompatible with the requested options or format and cannot be migrated. | 121 |
| `IO_ERROR` | An I/O failure not covered by a more specific code. | 55 |
| `LEASE_AGE_UNCERTAIN` | The wall clock moved backward, so lease staleness cannot be concluded. | 229.5 |
| `OPERATION_LOCKED` | Another process holds the operation lock. | 58 |
| `PATH_COMPONENT_INVALID` | A path component contains `0x00`; no `FluxPathKey` is constructed. | 103 |
| `PERMISSION_DENIED` | The operating system denied access. | 55 |
| `REMOTE_LOCK_UNSAFE` | No trustworthy exclusive lock contract can be established on a remote filesystem. | 235.4 |
| `RESUME_INVALID` | Resume validation failed: missing partial, state mismatch, or checkpoint mismatch. | 23, 35.3 |
| `SAFETY_REJECTED` | The source/destination containment or self-copy check rejected the operation. | 129 |
| `SOURCE_CHANGED` | Source identity or metadata changed during the copy; the result is not published. | 33 |
| `SPECIAL_FILE_UNSUPPORTED` | An unsupported special file: skipped with a durable warning, or a failure under strict policy. | 233 |
| `STATE_CORRUPT` | The manifest, `state.db`, or `topology.db` is corrupt; the workspace is preserved. | 140 |
| `STRICT_DURABILITY_UNAVAILABLE` | `--durability=strict` cannot be established for the filesystem. | 169 |
| `SYMLINK_CREATION_UNAVAILABLE` | A symlink cannot be created (for example, a missing Windows privilege); action-scoped. | 127, 259.11 |
| `TARGET_LOCK_BUSY` | A live owner holds the destination target lock. | 96, 240.2, 252.2 |
| `TARGET_LOCK_KEY_COLLISION` | Two distinct complete lock or catalog keys share a digest; the targets are never aliased. | 250, 259.6 |
| `TARGET_LOCK_UNCERTAIN` | Flux cannot distinguish a dead lock owner from a stalled one. | 240.4, 252.4 |
| `VERIFY_MISMATCH` | A verification digest did not match. | 55, 135 |
| `WAL_CORRUPT` | WAL corruption found before the trailing record. | 174, 191 |
| `WAL_FORMAT_UNSUPPORTED` | The WAL format is unknown. | 191 |
| `WAL_SEQUENCE_CONFLICT` | Conflicting payloads were found for one WAL sequence number. | 175, 191 |
| `WAL_SPACE_EXHAUSTED` | WAL growth cannot be relieved by compaction. | 190 |

Not error codes, and not listed above: state names (for example
`CAPACITY_WAIT`, `DIRECTORY_STAGING`), WAL record types (for example
`PREPARE_COMMIT`), WAL corruption classes (Section 191), cleanup
classifications (Sections 131, 251.1), and operating-system error names.

Do not keep an unbounded in-memory error list.

------------------------------------------------------------------------

# 56. Crash Recovery

Handle:

``` text
SIGINT
process termination
machine crash
power loss
I/O failure
```

Temporary files must be distinguishable from completed files.

Persistent state must be versioned.

Successful operations must clean up their operation workspace.

Abandoned operations remain resumable until explicitly or automatically
garbage-collected.

------------------------------------------------------------------------

# 57. Operation Identity

Each operation receives a unique identifier.

Used for:

-   temporary files
-   resume state
-   topology store
-   diagnostics
-   logs
-   lock ownership

UUID or equivalent collision-resistant identifier is recommended.

Temporary names must never be mistaken for completed user data.

------------------------------------------------------------------------

# 58. Operation Locking

Each persistent operation has an exclusive lock.

Only one process may actively mutate:

``` text
operation state
topology state
partial files
```

at a time.

A second invocation attempting to resume the same operation must
receive:

``` text
OPERATION_LOCKED
```

rather than concurrently modifying the same state.

------------------------------------------------------------------------

# 59. Heartbeat and Lease

Active operations update:

``` text
last_heartbeat
```

at a bounded interval.

The lease is used only for stale-operation detection.

A lease timeout must be conservative enough to avoid treating a paused
process as dead.

Lock ownership remains the primary protection against concurrent
mutation.

------------------------------------------------------------------------

# 60. Cleanup Command

Required architecture:

``` bash
flux cleanup DEST
```

Behavior:

1.  enumerate operation manifests
2.  inspect locks
3.  inspect heartbeat/lease
4.  classify active, resumable, stale
5.  display candidates
6.  remove only explicitly eligible operations

Future options:

``` bash
--target <PATH>          # single target, no DEST needed (Sections 234.1, 251.2)
--older-than <duration>
--dry-run
--force
```

Automatic cleanup should be conservative.

------------------------------------------------------------------------

# 61. Benchmark Command

``` bash
flux benchmark PATH
```

Future:

``` bash
flux benchmark --source /mnt/ssd1 --target /mnt/ssd2
```

Measure:

-   sequential read
-   sequential write
-   directory traversal
-   small-file throughput
-   mixed workloads

Potential comparisons:

``` text
cp
rsync
FastCopy
Flux
```

Datasets:

``` text
A: 1 × 100 GB
B: 1,000 × 100 MB
C: 100,000 × 100 KB
D: 1,000,000 × 4 KB
E: mixed tree
F: large hardlink topology
```

Measure:

``` text
wall time
throughput
CPU
resident RAM
IOPS
physical allocation
```

------------------------------------------------------------------------

# 62. Platform Capabilities

Expose:

``` rust
struct FsCapabilities {
    reflink: bool,
    sparse: bool,
    hardlink: bool,
    atomic_replace: bool,
    xattrs: bool,
    acl: bool,
}
```

Planner decisions use capabilities rather than hard-coded assumptions.

------------------------------------------------------------------------

# 63. Linux Fast Paths

Potential operations:

``` text
copy_file_range
FICLONE
sparse extent APIs
xattrs
```

Implement only when semantics are equivalent to the portable path.

------------------------------------------------------------------------

# 64. macOS Fast Paths

Potential operations:

``` text
clonefile
sparse files
xattrs
```

------------------------------------------------------------------------

# 65. Windows Fast Paths

Potential operations:

``` text
native copy facilities
reparse points
alternate data streams
file attributes
security metadata
native file identity
```

Windows APIs must preserve the distinction between:

``` text
case-insensitive lookup
```

and:

``` text
Flux deterministic ordering
```

------------------------------------------------------------------------

# 66. Fallback Policy

Every optimized filesystem operation must have one of:

``` text
safe portable fallback
```

or:

``` text
explicit strict failure
```

Never silently produce weaker semantics when the user requested strict
preservation.

------------------------------------------------------------------------

# 67. Dependencies

Candidate crates:

``` text
clap
rayon
crossbeam-channel
blake3
indicatif
tracing
tracing-subscriber
thiserror
anyhow
serde
serde_json
walkdir
jwalk
```

Final dependency selection must consider:

-   performance
-   correctness
-   portability
-   maintenance
-   licensing
-   API stability

------------------------------------------------------------------------

# 68. Testing Strategy

## 68.1 Unit tests

Test:

-   path normalization
-   deterministic ordering
-   Windows case tie-breaking
-   selection
-   update logic
-   overwrite logic
-   file identity
-   hardlink canonicalization
-   topology store
-   operation manifest
-   operation fingerprint
-   capacity calculations
-   resume validation
-   source mutation
-   statistics
-   error mapping

## 68.2 Integration tests

Required:

``` text
single file
directory
nested directory
multiple sources
existing destination
overwrite
update
skip-existing
verification
timestamps
permissions
symlinks
Unicode
spaces
newlines
zero-byte files
large files
sparse files
hardlinks
excluded hardlink members
resume
interrupted copy
reflink
dry-run
JSON
destination-inside-source
mount boundaries
disk full
stale operation cleanup
operation lock contention
```

------------------------------------------------------------------------

# 69. Hardlink Stress Tests

Mandatory:

## One pair

One physical copy and one link.

## Ten members

One physical copy and nine links.

## Million-member group

Resident RAM remains bounded.

Persistent topology state may be used.

## Huge tree

Hardlink members are spread across a massive directory tree.

The canonical member remains the lexicographically smallest selected
member.

## Partial selection

One selected member means ordinary copy.

## Excluded members

Excluded members never force copying.

## Cross-filesystem destination

Test:

``` text
--hardlinks=auto
--hardlinks=preserve
--hardlinks=copy
```

------------------------------------------------------------------------

# 70. Canonicalization Acceptance Test

Create:

``` text
Z/large
A/large
M/large
```

with all three sharing one filesystem identity.

Expected discovery:

``` text
A/large
M/large
Z/large
```

Expected canonical:

``` text
A/large
```

The copy of `A/large` must be allowed to start before `M/large` and
`Z/large` are discovered.

This validates the central bounded-memory hardlink architecture.

Separator-ordering case: create a directory `foo/` containing `bar`,
and a sibling file `foo.txt`, with `foo/bar` and `foo.txt` sharing one
filesystem identity.

Expected discovery and canonical:

``` text
foo/bar     canonical
foo.txt     dependent
```

A flat byte comparison of `/`-joined paths would pick `foo.txt`
(0x2E < 0x2F); that result is non-conforming. The test must also assert
that the scanner's emission order equals the order of the emitted
`FluxPathKey` values sorted bytewise.

------------------------------------------------------------------------

# 71. Windows Ordering Acceptance Test

Where the test environment can represent case-distinct entries in the
relevant namespace, test:

``` text
Archive.tar
archive.tar
```

The expected order must be generated by Flux's deterministic path
comparator, not Windows locale collation and not filesystem
case-insensitive lookup behavior.

The same logical ordering function must produce identical ordering on
all supported platforms for the same normalized path representation.

------------------------------------------------------------------------

# 72. Symlink / Hardlink Collision Test

Create:

``` text
real/file
link → real/file
```

and ensure:

``` text
TopologyStore:
    real/file identity

Symlink action:
    link → real/file
```

The symlink must not appear as a hardlink group member.

Repeat with multiple hardlinks and symlinks.

------------------------------------------------------------------------

# 73. Resume Stress Tests

Required:

``` text
interrupted large copy
truncated destination
corrupted destination
source changed within timestamp resolution
source size changed
source replaced
state corrupted
old state version
partial hardlink group
process crash
disk full
missing partial file
incompatible operation options
operation lock contention
```

Especially test:

``` text
mutate source bytes
restore apparent metadata
resume
```

Chunk validation must detect the mutation.

------------------------------------------------------------------------

# 74. Fault Injection

Simulate:

``` text
read failure
write failure
permission failure
disk full
destination disappearance
source mutation
hash mismatch
process interruption
rename failure
hardlink creation failure
reflink failure
metadata failure
state-store corruption
topology-store corruption
```

Results must be deterministic and recoverable where possible.

------------------------------------------------------------------------

# 75. Memory Acceptance

Test at least:

``` text
1,000,000 files
```

and very large hardlink groups.

The implementation must demonstrate that resident RAM is not
proportional to total file count within the configured topology-cache
policy.

Persistent state may grow with operation size.

------------------------------------------------------------------------

# 76. Performance Acceptance

Do not promise universal throughput.

Require:

-   bounded queues
-   no full-tree buffering
-   no unbounded task creation
-   no unnecessary source rereads
-   no unnecessary global locks
-   native fast paths where beneficial
-   benchmarks before/after optimizations

Performance regressions must be benchmarked rather than guessed.

------------------------------------------------------------------------

# 77. Flux 0.1

Implement:

``` text
recursive copy
multiple sources
parallel workers
bounded scanner queues
deterministic discovery
hardlink preservation
hardlink exclusions
overwrite
update
skip-existing
progress
statistics
dry-run
BLAKE3 source-stream hashing
timestamp preservation
portable permissions
symlink copying
source mutation detection
filesystem-aware safety
cross-platform builds
operation manifests
operation locking
persistent topology store
```

Architecture must already accommodate:

``` text
resume
atomic replacement
reflink
sparse files
```

------------------------------------------------------------------------

# 78. Flux 0.2

Implement:

``` text
resume
chunk/checkpoint validation
atomic replacement
capacity estimation
sparse files
reflinks
JSON output
cleanup
stale-operation management
```

------------------------------------------------------------------------

# 79. Flux 0.3

Implement:

``` text
adaptive scheduler
small-file optimization
benchmark command
advanced metadata
xattrs
ACLs where practical
```

------------------------------------------------------------------------

# 80. Flux 0.4+

Potential:

``` text
sync
delete
restore
network transfers
snapshots
advanced recovery
packed small-file transfers
```

------------------------------------------------------------------------

# 81. Implementation Order

## Phase 1 --- Workspace

Create crates and compile.

## Phase 2 --- Portable copy

Implement:

``` text
files
directories
metadata
errors
statistics
```

## Phase 3 --- Streaming

Implement:

``` text
scanner
selection
bounded queues
planner
workers
```

Demonstrate bounded memory.

## Phase 4 --- Deterministic topology

Implement:

``` text
ordered traversal
FileIdentity
hardlink canonicalization
persistent topology store
excluded topology members
dependency-aware links
```

## Phase 5 --- Persistent operation state

Implement:

``` text
operation manifest
operation ID
operation lock
heartbeat
state database
partial-file mapping
crash recovery
cleanup
```

## Phase 6 --- Verification

Implement:

``` text
BLAKE3
source-stream hashing
destination verification
verify command
```

## Phase 7 --- Safety

Implement:

``` text
filesystem identity
destination nesting
mount boundaries
symlink policy
source mutation
```

## Phase 8 --- UX

Implement:

``` text
progress
JSON
logging
dry-run
statistics
```

## Phase 9 --- Advanced filesystem operations

Implement:

``` text
resume
checkpoint validation
atomic replacement
capacity planning
sparse
reflink
```

## Phase 10 --- Performance

Benchmark and optimize:

``` text
worker count
queue depth
buffer size
filesystem APIs
metadata calls
topology lookup
small-file scheduling
```

------------------------------------------------------------------------

# 82. Example Workflows

## Normal

``` bash
flux copy /data /backup
```

## Verified high-throughput

``` bash
flux copy \
  --workers auto \
  --verify=blake3 \
  --hardlinks=auto \
  --reflink=auto \
  --sparse=auto \
  /data /backup
```

## Strict topology

``` bash
flux copy \
  --hardlinks=preserve \
  --reflink=always \
  /data /backup
```

## Resume

``` bash
flux copy \
  --resume \
  --resume-verify=chunks \
  /data \
  /backup
```

## Strict atomic replacement

``` bash
flux copy \
  --atomic=always \
  /data \
  /backup
```

## Dry run

``` bash
flux copy --dry-run /data /backup
```

## Machine-readable

``` bash
flux copy --json /data /backup
```

## Cleanup

``` bash
flux cleanup /backup
```

------------------------------------------------------------------------

# 83. Final Architecture

``` text
                         SOURCE
                           │
                           ▼
                ┌────────────────────┐
                │ Deterministic      │
                │ Ordered Scanner    │
                └─────────┬──────────┘
                          │
                          ▼
                ┌────────────────────┐
                │ Selection / Filter │
                └─────────┬──────────┘
                          │
                    bounded stream
                          │
                          ▼
                ┌────────────────────┐
                │ Topology Mapper    │
                │                    │
                │ bounded RAM cache  │
                │        +           │
                │ persistent store   │
                └─────────┬──────────┘
                          │
                          ▼
                ┌────────────────────┐
                │ Transfer Planner   │
                │                    │
                │ streaming actions │
                └─────────┬──────────┘
                          │
                    bounded queue
                          │
                          ▼
                ┌────────────────────┐
                │ Worker Pool        │
                └─────────┬──────────┘
                          │
                   stream copy/hash
                          │
                          ▼
                ┌────────────────────┐
                │ Mutation /         │
                │ Verification       │
                └─────────┬──────────┘
                          │
                          ▼
                ┌────────────────────┐
                │ Commit / Rename    │
                └─────────┬──────────┘
                          │
                          ▼
                        REPORT


       DESTINATION-LOCAL CONTROL PLANE

       .flux/operations/<operation-id>/
                    │
          ┌─────────┼──────────┐
          ▼         ▼          ▼
       manifest   state.db   topology.db
          │         │          │
          └─────────┴──────────┘
                    │
               lock + lease
                    │
                    ▼
              crash recovery
                    │
                    ▼
                 cleanup
```

------------------------------------------------------------------------

# 84. Core Design Resolution

The hardlink canonicalization problem is resolved by combining three
properties:

``` text
deterministic ordered discovery
        +
streaming planner
        +
persistent topology state
```

The scanner does **not** need to discover every member of a hardlink
group.

For:

``` text
A/file ─────┐
M/file ─────┼── same object
Z/file ─────┘
```

the deterministic scanner discovers:

``` text
A/file
```

first.

Therefore:

``` text
A/file = canonical
```

and the transfer can begin immediately.

Later:

``` text
M/file → LINK to A/file
Z/file → LINK to A/file
```

are emitted as dependent actions.

The topology store allows this relationship to survive:

``` text
large scale
process interruption
resume
bounded RAM
```

without buffering the entire tree.

------------------------------------------------------------------------

# 85. Core State-Management Resolution

Flux deliberately separates:

``` text
data plane
```

from:

``` text
control plane
```

The data plane contains user files.

The control plane contains:

``` text
operation manifest
resume state
topology state
locks
heartbeats
```

The control plane is:

``` text
destination-local
operation-scoped
versioned
crash-persistent
discoverable
resumable
garbage-collectable
```

Single-file operations may use adjacent state for convenience.

Directory operations use:

``` text
DEST/.flux/operations/<operation-id>/
```

This avoids both extremes:

``` text
one state file per source file
```

and:

``` text
one global database for every Flux operation
```

------------------------------------------------------------------------

# 86. Core Symlink Resolution

Flux maintains a strict distinction between:

``` text
object identity
```

and:

``` text
symlink target
```

Default behavior:

``` text
symlink = directory entry whose payload is a path
```

not:

``` text
symlink = alias to the target object's topology
```

Therefore a symlink never enters the hardlink topology graph unless
explicit link-following semantics are enabled.

------------------------------------------------------------------------

# 87. Core Safety Principle

Flux should prefer:

``` text
explicit refusal
```

over:

``` text
ambiguous best effort
```

when a filesystem operation could violate user expectations.

Examples:

``` text
cannot preserve required hardlink
cannot establish safe destination containment
resume state incompatible
source changed
atomic replacement cannot satisfy --atomic=always
```

These conditions must become explicit errors.

------------------------------------------------------------------------

# 88. Product Direction

Flux should ultimately be:

> **A fast, safe, topology-aware data movement engine for modern
> filesystems.**

The central engineering principle is:

> **Never require the engine to hold the whole filesystem in memory
> merely to make a correct decision about one file.**

For hardlinks:

> **Deterministic discovery establishes the canonical member; persistent
> topology state preserves relationships without buffering entire
> hardlink groups.**

For resumability:

> **Operation-local persistent state makes interrupted transfers
> discoverable without a global database.**

For safety:

> **Filesystem identity, object identity, and explicit lifecycle state
> take precedence over path-string assumptions.**

For performance:

> **Streaming execution and bounded queues are maintained even when
> correctness requires persistent state.**

------------------------------------------------------------------------

# 89. Protocol Finalization --- Axiom/Cascade/Blindspot Resolution

This section is normative and overrides any earlier ambiguous wording in
this document.

The following decisions are mandatory for the implementation:

1.  Canonical hardlink identity is immutable for the lifetime of an
    operation.
2.  A failed canonical hardlink member is never dynamically replaced as
    canonical. Fallback materialization (Section 253) may select a
    different materialization anchor without changing `canonical_path`.
3.  Dependent hardlink actions are held outside the worker execution
    queue until their canonical object is materialized.
4.  Single-file destination targets require destination-path locking in
    addition to operation locking.
5.  Path ordering uses a platform-independent, byte-preserving
    `FluxPathKey`.
6.  Weak or unavailable filesystem object identity cannot establish
    hardlink equivalence.
7.  `--hardlinks=auto` degrades to ordinary copies with a warning when
    reliable identity is unavailable.
8.  `--hardlinks=preserve` fails when reliable identity is unavailable.
9.  Heartbeats are advisory; OS/process locks are authoritative.
10. Default heartbeat interval is 5 seconds.
11. Default stale lease threshold is 30 seconds.
12. Destructive/publishing operations must revalidate ownership
    immediately before committing.
13. Monotonic time must be used for lease-duration calculations where
    available.
14. Metadata-only source mutation checks are not claimed to provide an
    immutable-source guarantee.
15. Strongest source-consistency modes must use a stable-read/snapshot
    facility when the platform provides one.
16. Copy-time hashing verifies the bytes actually read, not necessarily
    an immutable historical source.
17. Topology state transitions must be atomic.
18. A topology record must explicitly represent unresolved, copying,
    materialized, and failed states.
19. Operation state, target locks, and topology state must have a
    defined lock/CAS ordering.
20. A stale operation must never be deleted solely because its
    wall-clock heartbeat is old.

------------------------------------------------------------------------

# 90. Hardlink Canonical Failure State Machine

A hardlink topology record is modeled as:

``` rust
enum TopologyState {
    Unresolved,
    Copying {
        operation_id: OperationId,
        canonical_path: RelativePath,
        current_attempt_id: AttemptId,
        attempt_number: u64,
        candidate_path: RelativePath,
    },
    Materialized {
        destination_path: RelativePath,
        attempt_id: AttemptId,
    },
    Failed {
        error_code: ErrorCode,
        last_attempt_id: AttemptId,
    },
}
```

This is the authoritative payload-bearing definition (Section V14.4).
Field semantics:

-   `canonical_path` is the immutable deterministic representative
    (Sections 13, 204). It never changes.
-   `candidate_path` is the member the current attempt is
    materializing. It equals `canonical_path` until fallback
    materialization selects another candidate (Sections 253, 259.2). A
    candidate never becomes canonical.
-   `current_attempt_id` identifies the authoritative current execution
    attempt (Section 202). It is the value that Section V14.3 attempt
    fencing compares against.
-   `attempt_number` is the ordinal of the current attempt within the
    whole hardlink group, counting retries and fallback candidates
    alike, starting at 1 for the initial attempt. It is durable and
    survives restart, and it is bounded by the per-group retry budget
    (Section 206).
-   `Materialized.destination_path` is the `materialization_anchor`
    (Section 259.2): the destination path of the member that actually
    created the object. It may differ from `canonical_path`.
-   `Failed` is terminal: every viable candidate has been exhausted or
    proven unusable (Sections 253.5, 259.2).

The valid transitions are:

``` text
Unresolved
    │
    │ claim canonical (creates attempt 1, candidate = canonical_path)
    ▼
Copying { current_attempt_id = A1 }
    │
    ├── attempt succeeds ──────────────────────► Materialized { anchor, attempt }
    │
    ├── attempt fails; a retry or fallback
    │   candidate remains ────────────────────► Copying { current_attempt_id = A2 }
    │
    └── attempt fails; no viable candidate
        remains ──────────────────────────────► Failed { error, last attempt }
```

Retries and fallback attempts never leave `Copying`. A failed attempt is
recorded in its immutable execution history (Section 202), and a new
attempt becomes `current_attempt_id`.

There is no:

``` text
Failed → any other state
Materialized → Copying      (during normal execution)
Copying → Unresolved        (including crash recovery; Section 123)
```

transition. `Failed` is terminal.

Every transition out of `Copying` names the expected
`current_attempt_id` and is applied by compare-and-swap. A transition
whose expected attempt is not the current attempt changes nothing
(Section 91).

This makes canonical selection immutable and prevents concurrent workers,
or stale work from a superseded attempt, from changing the topology
decision.

------------------------------------------------------------------------

# 91. Atomic Topology Claim

Workers must not implement topology state with a non-atomic:

``` text
lookup
if unresolved:
    insert copying
```

sequence.

That sequence has a race.

Instead the topology store must provide an atomic claim operation and
attempt-fenced transitions:

``` rust
trait TopologyStore {
    fn lookup(
        &self,
        identity: FileIdentity,
    ) -> Result<Option<TopologyRecord>>;

    // Unresolved → Copying; creates attempt 1 with
    // candidate_path = canonical_path.
    fn claim_canonical(
        &self,
        identity: FileIdentity,
        operation_id: OperationId,
        canonical_path: RelativePath,
    ) -> Result<ClaimResult>;

    // Current attempt Running → Failed. The record stays Copying.
    fn record_attempt_failure(
        &self,
        identity: FileIdentity,
        attempt_id: AttemptId,
        error_code: ErrorCode,
    ) -> Result<TransitionResult>;

    // Retry or fallback: requires the expected current attempt to be
    // Failed and the retry/candidate budget to allow a new attempt.
    // Creates the new attempt and makes it current.
    fn begin_attempt(
        &self,
        identity: FileIdentity,
        expected_attempt_id: AttemptId,
        candidate_path: RelativePath,
    ) -> Result<TransitionResult>;

    // Copying → Materialized { destination_path = anchor }.
    fn mark_materialized(
        &self,
        identity: FileIdentity,
        attempt_id: AttemptId,
        destination_path: RelativePath,
    ) -> Result<TransitionResult>;

    // Copying → Failed (terminal). Requires the current attempt to be
    // Failed and no viable candidate to remain.
    fn mark_failed(
        &self,
        identity: FileIdentity,
        attempt_id: AttemptId,
        error_code: ErrorCode,
    ) -> Result<TransitionResult>;
}
```

`claim_canonical` and every transition must be implemented as a
transactional compare-and-swap or equivalent database transaction. The
attempt record (Section 202) and the topology record are updated in the
same transaction.

Possible results:

``` rust
enum ClaimResult {
    Claimed {
        attempt_id: AttemptId,
    },
    AlreadyCopying {
        current_attempt_id: AttemptId,
    },
    AlreadyMaterialized {
        destination_path: RelativePath,
    },
    AlreadyFailed {
        error_code: ErrorCode,
    },
}

enum TransitionResult {
    Applied,
    // The same transition was already applied by the same attempt;
    // an idempotent no-op (duplicate delivery, Section V14.7.4).
    AlreadyApplied,
    // The named attempt is not the current attempt; nothing changed.
    StaleAttempt {
        current_attempt_id: AttemptId,
    },
    // The record is not in a state that permits this transition
    // (for example Unresolved, or already terminal); nothing changed.
    InvalidState,
}
```

Only one worker may successfully claim `Unresolved`.

A transition carrying a superseded `attempt_id` MUST return
`StaleAttempt` and MUST NOT change the topology record, the execution
history, or any dependent state. This fence applies at the store, not
only to scheduler events (Section V14.3).

------------------------------------------------------------------------

# 92. Hardlink Group Failure Semantics

When the canonical copy fails:

``` text
canonical
    ↓
FAILED
```

all dependents transition to:

``` text
BLOCKED_BY_CANONICAL_FAILURE
```

They must not execute `link()`.

Here `FAILED` is the terminal `TopologyState::Failed` (Section 90): no
retry or fallback candidate remains. While a retry or fallback candidate
can still materialize the object, the record stays `Copying` and
dependents stay held (Section 94); they are not blocked.

The final operation report must distinguish:

``` text
canonical failure
dependent failures
```

rather than reporting every dependent as an independent I/O failure.

Example:

``` text
A/file  COPY_FAILED
M/file  BLOCKED_BY_CANONICAL_FAILURE
Z/file  BLOCKED_BY_CANONICAL_FAILURE
```

This prevents misleading error storms for very large hardlink groups.

------------------------------------------------------------------------

# 93. Optional Retry Semantics

Retries may retry the **same canonical path**.

Example:

``` text
A/file
   │
   ├── retry
   ├── retry
   └── failed
```

Retries must never change:

``` text
A/file
```

to:

``` text
M/file
```

as canonical.

A retry may replace or reconstruct the partial destination according to
the configured resume policy.

------------------------------------------------------------------------

# 94. Dependent Hardlink Hold Queue

Dependent hardlink actions must not enter the general worker-ready queue
while their canonical object is unresolved.

Use:

``` text
DISCOVERY
    ↓
PLANNER
    ├── READY QUEUE
    └── HARDLINK HOLD INDEX
```

The hold index is keyed by canonical topology identity.

Example:

``` text
identity 123
    canonical: A/file
    dependents:
        M/file
        Z/file
        Q/file
```

When canonical state becomes:

``` text
Materialized
```

the dependents are released into the ready queue.

When canonical state becomes:

``` text
Failed
```

the dependents are completed as blocked failures without consuming
worker slots.

The hold index itself must be bounded in RAM and may spill to persistent
state.

------------------------------------------------------------------------

# 95. Worker Starvation Invariant

Workers must never synchronously wait for another worker to finish a
canonical hardlink copy.

Forbidden:

``` rust
worker.execute_link();
worker.wait_for_canonical();
```

Required:

``` text
link action → HOLD
```

and later:

``` text
canonical materialized → RELEASE
```

This prevents a bounded worker pool from deadlocking on a large hardlink
topology.

------------------------------------------------------------------------

# 96. Destination Target Locks

Operation locks protect operation state.

Destination target locks protect destination objects and destination
namespace targets.

Both are required where the operation performs destination publication
or destructive replacement.

For a single-file destination:

``` text
DEST/target.flux-lock
```

or an equivalent native locking primitive protects:

``` text
DEST/target
```

The target lock identity is derived from the normalized destination
target, including its canonical:

``` text
FluxPathKey
```

and is not derived from the operation ID.

Therefore:

``` text
flux copy A /dest/file
flux copy B /dest/file
```

must contend for the same target lock.

The second operation must receive:

``` text
TARGET_LOCK_BUSY
```

unless an explicit future wait/retry mode is enabled.

A target lock establishes namespace exclusivity. It does not, by itself,
establish object identity or object continuity.

------------------------------------------------------------------------

# 97. Directory Target Locking

Directory operations must prevent incompatible concurrent modifications
without requiring one lock per source file to remain held indefinitely.

At minimum, an operation must acquire a lock protecting its destination
namespace.

Where safe concurrency is desirable, the implementation may use
hierarchical or per-target locks.

Required invariant:

> Two Flux operations may not concurrently publish or destructively
> replace the same destination object.

The target-lock key MUST remain stable for the normalized destination
namespace/path even when the filesystem's object identity is weak.

------------------------------------------------------------------------

# 98. Lock Acquisition Order

To prevent lock-order deadlocks, the implementation must use this order:

``` text
1. operation lock
2. destination namespace/target lock
3. topology CAS/transaction
```

A topology operation must never acquire an operation lock after entering
a topology transaction.

If platform constraints require another order internally, the
implementation must establish an equivalent globally consistent ordering
and document it.

The operation lock is lifecycle ownership. It MUST NOT be converted into
a global data-plane mutex held across filesystem I/O, network I/O,
hashing, WAL latency, or other unbounded work.

Topology coordination remains fine-grained.

------------------------------------------------------------------------

# 99. Commit-Time Lock Revalidation

Immediately before:

``` text
write
truncate
rename
unlink
hardlink creation
metadata replacement
```

the worker must verify that its required ownership remains valid.

At minimum:

``` text
operation still active
operation lock still owned
destination target lock still owned
operation not cancelled
```

For atomic publication:

``` text
source validation
verification
lock revalidation
rename
```

must occur without an intervening operation-state transition that would
invalidate ownership.

## 99.1 Weak Filesystem Identity and Target Locks

A weak or unavailable filesystem identity does NOT by itself invalidate
a strong, authoritative, path-scoped target lock.

Target-lock identity is based on the authoritative destination namespace
and canonical `FluxPathKey` of the target.

Therefore:

``` text
Strong filesystem identity + strong target lock
    → normal operation

Weak filesystem identity + strong target lock
    → namespace locking remains valid;
      object-continuity guarantees remain restricted

Unavailable filesystem identity + strong target lock
    → namespace locking remains valid;
      object-continuity guarantees remain restricted

Any filesystem identity + unverified/unsupported target lock
    → unsafe for operations requiring target exclusivity
```

A target lock MUST NOT be interpreted as proof that:

``` text
object observed before lock
        ==
object observed after replacement/remount/recovery
```

Strong filesystem/object identity remains mandatory wherever Flux needs
to prove:

``` text
object continuity
hardlink equivalence
safe object reuse during recovery
```

If authoritative target-lock semantics cannot be established, the
operation requiring target exclusivity MUST fail rather than fall back
to PID/heartbeat heuristics.

A visible lock file may contain owner metadata, PID, instance ID,
boot-session ID, and heartbeat information, but such metadata is
descriptive unless the underlying filesystem contract provides
authoritative exclusive ownership.

# 100. Sleep and Clock-Drift Safety

Heartbeats are advisory.

A machine may:

``` text
sleep
hibernate
lose network connectivity
pause a process
experience wall-clock adjustment
```

The lease system must therefore not use wall-clock age alone to
authorize destructive cleanup.

Use a monotonic clock for elapsed-time calculations when available.

For persistent timestamps, record wall-clock time for diagnostics, but
do not use it as the sole proof that an operation is dead.

If an operation lock is still valid:

``` text
cleanup MUST NOT delete it
```

even if:

``` text
last_heartbeat > 30 seconds old
```

The 30-second value only makes an operation eligible for stale-state
inspection.

------------------------------------------------------------------------

# 101. Heartbeat Defaults

Normative defaults:

``` text
heartbeat interval = 5 seconds
lease/stale threshold = 30 seconds
```

The heartbeat must not be emitted more frequently than necessary to
maintain the lease.

Recommended implementation:

``` text
heartbeat every 5 seconds
```

with a small scheduling tolerance.

The thresholds may become configurable later:

``` bash
--heartbeat-interval <duration>
--lease-timeout <duration>
```

but changing them must not weaken active-operation locking.

------------------------------------------------------------------------

# 102. Stale Operation Classification

An operation is classified as stale only after all of:

``` text
1. operation is not currently locked
2. heartbeat/lease is older than threshold
3. operation is not actively held by a live Flux process
```

The implementation should perform a final lock acquisition/check
immediately before deletion.

If lock acquisition succeeds unexpectedly because the original process
has disappeared, the operation may be classified as stale.

If lock acquisition fails:

``` text
operation is active
```

and cleanup must skip it.

------------------------------------------------------------------------

# 103. FluxPathKey

Define:

``` rust
struct FluxPathKey(Vec<u8>);
```

`FluxPathKey` is an internal ordering representation.

It is not necessarily the filesystem path encoding.

Normative encoding:

``` text
FluxPathKey = c1 0x00 c2 0x00 ... 0x00 cn
```

where `c1 .. cn` are the components of the path relative to the
operation's destination root: the source root's destination prefix
(Section 18.3) followed by the path's components relative to that source
root. Each component is in its exact platform byte form (Sections 104,
105, 241). A source root itself encodes as its prefix, which is the empty
key when the prefix is empty. For a single root mapped onto the
destination root, the key is simply the source-relative path.

The byte `0x00` cannot occur inside a component: POSIX filenames cannot
contain NUL, and Windows/NTFS filenames cannot contain U+0000 (whose
WTF-8 encoding is the only way to produce `0x00`). A platform adapter that
nevertheless observes a component containing `0x00` MUST report the entry
as `PATH_COMPONENT_INVALID` and MUST NOT construct a key for it.

Because `0x00` is smaller than every byte that can occur in a component,
plain bytewise comparison of `FluxPathKey` values (the derived `Ord` of
`Vec<u8>`) is exactly the component-wise order of Section 7.2. Persistent
indexes with byte-ordered keys may therefore store keys directly, without
a custom comparator.

`FluxPathKey` is an ordering and identity key, not a display string
(Section 245). The `/`-joined relative path remains the display and
destination-mapping representation.

Properties:

``` text
deterministic
locale-independent
case-folding-independent
byte-preserving where the source namespace permits
stable
lexicographically comparable
```

Flux must not use:

``` text
locale collation
Windows Explorer ordering
filesystem case folding
Unicode locale collation
```

for canonical hardlink selection.

------------------------------------------------------------------------

# 104. Unix Path Encoding

Unix paths are arbitrary byte sequences.

The Unix implementation should obtain the raw filename bytes and
construct the `FluxPathKey` without requiring UTF-8.

Therefore filenames such as:

``` text
0xFF 0xFE 0x41
```

remain representable.

No lossy:

``` text
UTF-8 conversion
replacement character conversion
```

is permitted in the canonical ordering key.

------------------------------------------------------------------------

# 105. Windows Path Encoding

Windows native paths are represented using UTF-16 code units.

Flux must serialize the Windows path into its canonical `FluxPathKey`
representation using a deterministic encoding specified by the path
module.

The implementation must not rely on:

``` text
current Windows locale
```

or:

``` text
case-insensitive filename comparison
```

for ordering.

Where a byte-preserving WTF-8-style representation is used internally,
it must be specified and tested as an encoding of Windows UTF-16 code
units, not assumed to be equivalent to arbitrary Unix byte strings.

------------------------------------------------------------------------

# 106. Cross-Platform Ordering Contract

Flux guarantees:

> The same sequence of `FluxPathKey` values sorts identically on Linux,
> macOS, and Windows.

Flux does not guarantee that an arbitrary byte sequence that is legal on
Linux necessarily has an equivalent legal filename representation on
Windows.

This distinction must appear in developer documentation.

------------------------------------------------------------------------

# 107. Weak FileIdentity

Identity strength:

``` rust
enum IdentityStrength {
    Strong,
    Weak,
    Unavailable,
}
```

A `FileIdentity` must therefore be accompanied by its reliability
classification.

Examples that may produce weak/unavailable identity:

``` text
FAT32
exFAT
some SMB configurations
some NFS configurations
network filesystems with unstable inode-like values
virtual filesystems
```

The platform adapter must never treat:

``` text
object_id == 0
```

as a valid universal object identity.

------------------------------------------------------------------------

# 108. Hardlink Policy Under Weak Identity

## `--hardlinks=auto`

If identity is not strong enough:

``` text
do not infer hardlink equivalence
```

Instead:

``` text
copy files independently
```

and emit one aggregated warning per affected filesystem/operation where
practical.

Example:

``` text
warning: reliable filesystem object identity unavailable;
hardlink preservation degraded to ordinary copies
```

## `--hardlinks=preserve`

Fail:

``` text
HARDLINK_IDENTITY_UNAVAILABLE
```

Do not guess.

## `--hardlinks=copy`

No identity lookup is required for preservation.

------------------------------------------------------------------------

# 109. Identity Scope

Object identity must be scoped by filesystem identity.

Never compare only:

``` text
inode
```

or only:

``` text
file ID
```

Use:

``` text
filesystem_id + object_id
```

where both are reliable.

This prevents unrelated objects on different filesystems from being
merged into one topology group.

## 109.1 Durable Filesystem Identity

`filesystem_id` is a persistent semantic identity, not merely an
ephemeral runtime observation.

The persisted representation MUST include:

``` text
identity_scheme
identity_scheme_version
canonical_identity_value
identity_strength
```

where:

``` text
identity_strength =
    Strong
    Weak
    Unavailable
```

A strong filesystem identity may be used for persistent recovery, object
identity comparison, hardlink inference, and object reuse only when the
implementation can establish that the identity is stable across the
lifecycle for which the claim is being made, including reboot where
applicable.

An ephemeral runtime value such as:

``` text
st_dev
```

MAY be used as an implementation signal but MUST NOT by itself be
treated as the authoritative persistent filesystem identity.

If a previously persisted strong identity cannot be re-established with
compatible scheme and strength, Flux MUST NOT silently treat a different
or uncertain identity as equivalent.

Instead:

``` text
rediscover identity
        ↓
reconcile according to recovery policy
        ↓
preserve state or fail when identity continuity is required
```

Weak or uncertain filesystem identity MUST NOT authorize:

``` text
hardlink equivalence
object reuse based solely on identity
persistent claims of object continuity
```

This rule is independent of path-scoped target locking. A weak
filesystem identity may still coexist with a strong namespace lock.

# 110. Source Mutation Guarantee Levels

Flux must explicitly distinguish three guarantees.

## Level 1 --- Metadata detection

Default.

Check:

``` text
identity
size
mtime
change/creation metadata where available
```

before and after copying.

This detects common mutations.

It does not guarantee immutability against an adversarial or
timestamp-obscuring writer.

## Level 2 --- Chunk-consistent resume

Use chunk checkpoints for interrupted transfers.

This validates previously copied regions before resuming.

It detects many silent mutations even when coarse timestamp resolution
would not.

## Level 3 --- Stable source snapshot

Where the platform/filesystem provides a stable snapshot or equivalent
read-consistency primitive, Flux may copy from that stable view.

This provides the strongest source-consistency guarantee.

The specification must never describe metadata comparison alone as an
immutable snapshot guarantee.

------------------------------------------------------------------------

# 111. Torn-Read Window

The following is explicitly considered possible:

``` text
t0 source metadata recorded
t1 source bytes begin changing
t2 Flux reads mixed old/new content
t3 source metadata appears unchanged
t4 copy completes
```

Therefore:

``` text
mtime + size
```

must not be described as proof that all copied bytes came from one
immutable version.

Copy-time hashing proves the digest of the bytes Flux actually read.

It does not prove that the source represented one stable historical
version.

------------------------------------------------------------------------

# 112. Source Snapshot Capability

Platform layer:

``` rust
trait StableSourceReader {
    fn open_stable(path: &Path) -> Result<Box<dyn Read>>;
}
```

Capability detection:

``` rust
struct FsCapabilities {
    stable_snapshot_read: bool,
    reflink: bool,
    sparse: bool,
    hardlink: bool,
    atomic_replace: bool,
    xattrs: bool,
    acl: bool,
}
```

If stable snapshot reading is unavailable, Flux must report the actual
guarantee level rather than implying stronger semantics.

------------------------------------------------------------------------

# 113. Stage A Capacity Forecast

Stage A performs an early, conservative capacity forecast before
execution.

The forecast considers, where applicable:

``` text
destination free space
known destination allocation
source metadata
incoming allocation estimates
atomic temporary allocation
reclaimable capacity
capacity released by operations that are durably completed
```

Stage A is a planning estimate.

Definitive per-action capacity checks remain mandatory immediately
before allocations that can fail because of insufficient capacity.

Stage A MUST NOT assume that a planned deletion has already released
space.

## 113.1 Sliding / Delta Forecast and Reclaimed Capacity

For long-running operations, Stage A may use a sliding/delta forecast
rather than retaining a complete immutable reservation estimate.

The running forecast MAY subtract destination allocation that has become
reclaimable as a result of completed replacements.

The normative trigger is the **successful atomic namespace
publication**:

``` text
atomic rename succeeds
        ↓
replacement is physically published
        ↓
old destination allocation is eligible to be treated as released/reclaimable
        ↓
Stage A running forecast may subtract the actually reclaimable amount
```

A WAL append, WAL flush, state update, `PREPARE_COMMIT`, or other
control-plane durability event MUST NOT by itself cause the allocation
to be subtracted from the running capacity forecast.

Durability records what Flux knows about the transition; they do not by
themselves release filesystem allocation.

Accordingly:

``` text
planned deletion
        → does not reclaim capacity

WAL PREPARE_COMMIT
        → does not reclaim capacity

WAL COMMIT
        → does not by itself reclaim capacity

successful atomic rename
        → publication has occurred

confirmed filesystem-reclaimable allocation
        → may reduce Stage A forecast
```

The amount subtracted MUST be limited to allocation that the filesystem
semantics make actually reclaimable. Flux MUST NOT assume that a logical
replacement immediately returns an arbitrary byte count to the
free-space pool.

The per-file definitive capacity check remains mandatory immediately
before atomic execution. Stage A is a forecast and MUST NOT replace the
definitive check.

For whole-tree atomic publication, capacity occupied by the old
published tree remains part of the live allocation requirement until the
filesystem operation that replaces/removes it has actually made the
corresponding allocation reclaimable.

# 114. Atomic Preflight Policy

For:

``` text
--atomic=always
```

Flux should perform a preflight scan that computes at least:

``` text
sum of additional worst-case temporary allocations
```

without storing the complete file list in RAM.

The result may be stored in the persistent operation state.

The scanner streams metadata into a persistent aggregate:

``` text
required_temp_bytes
known_source_bytes
estimated_allocation
```

This reconciles preflight capacity planning with bounded memory.

Flux may then refuse to start if the conservative requirement cannot be
satisfied.

------------------------------------------------------------------------

# 115. Atomic Capacity Reservation

Where reliable filesystem reservation is unavailable, the preflight is
still only an estimate.

Flux must retain the per-file definitive check.

If the filesystem reports:

``` text
ENOSPC
```

the operation fails or pauses according to policy.

For:

``` text
--atomic=always
```

it must never silently switch to in-place replacement.

For:

``` text
--atomic=auto
```

it may fall back according to the documented fallback policy.

------------------------------------------------------------------------

# 116. Atomic Fallback Semantics

## `--atomic=always`

``` text
insufficient capacity
        ↓
fail
```

## `--atomic=never`

``` text
copy in place
```

## `--atomic=auto`

``` text
prefer atomic
     │
     ├── feasible → atomic
     │
     └── infeasible → safe in-place fallback
```

The report must say:

``` text
atomic replacement: degraded
```

when fallback occurred.

------------------------------------------------------------------------

# 117. Atomic Publication Invariant

A destination target is considered successfully replaced only after:

``` text
copy
source validation
required verification
metadata application
flush policy
lock revalidation
atomic rename
```

If any pre-publication step fails:

``` text
existing destination remains untouched
```

when the atomic strategy is in use.

------------------------------------------------------------------------

# 118. Persistent State and Atomic Temporary Files

The operation state must map:

``` text
logical destination
```

to:

``` text
temporary destination
```

explicitly.

Never infer this mapping solely from filename patterns.

Example:

``` rust
struct PartialTarget {
    logical_destination: PathBuf,
    temporary_path: PathBuf,
    operation_id: OperationId,
}
```

------------------------------------------------------------------------

# 119. Operation Workspace Final Form

## Single file

``` text
DEST/
├── target
├── target.flux-partial.<operation-id>
├── target.flux-state.<operation-id>
└── target.flux-lock
```

The lock may be implemented using an OS-native lock primitive rather
than a persistent visible file.

## Directory

``` text
DEST/
└── .flux/
    └── operations/
        └── <operation-id>/
            ├── manifest
            ├── state.db
            ├── topology.db
            ├── checkpoints/
            └── lock
```

The exact on-disk database format is implementation-defined but must be
versioned.

------------------------------------------------------------------------

# 120. Resume Discovery Rules

A fresh invocation must identify resumable work using:

``` text
source mapping
destination mapping
operation manifest
```

not by blindly matching arbitrary filenames.

For a single target:

``` text
target lock
target state
operation ID
partial target
```

must form a validated relationship.

For directory operations:

``` text
destination/.flux/operations/
```

is the authoritative discovery location.

------------------------------------------------------------------------

# 121. Resume Compatibility

Resume requires compatibility of:

``` text
source mapping
destination mapping
hardlink mode
atomic mode
verification policy
sparse policy
reflink policy
metadata policy
chunk size
hash algorithm
state format
```

Changes that cannot be safely migrated produce:

``` text
INCOMPATIBLE_STATE
```

Source and destination mappings are compared as the set of
`(source_root, destination_prefix)` pairs recorded in the manifest
(Section 19). Listing the same roots in a different command-line order
is compatible; adding, removing, or remapping a root is not.

------------------------------------------------------------------------

# 122. Topology Store Recovery

The topology store is part of the operation's durable state.

On restart:

``` text
manifest
   ↓
state.db
   ↓
topology.db
   ↓
recover unresolved/copying records
```

A record left in:

``` text
Copying(OperationId)
```

after a crash must be reconciled with:

``` text
partial destination state
operation state
lock state
```

It must not automatically be assumed to be materialized.

------------------------------------------------------------------------

# 123. Recovery of `Copying`

After crash:

``` text
TopologyState::Copying
```

becomes a recovery candidate.

Recovery must determine whether:

``` text
destination canonical object was committed
```

using durable state and filesystem inspection.

If committed:

``` text
→ Materialized
```

If not committed, the record stays `Copying` with the same
`current_attempt_id`:

``` text
attempt safely recoverable
    → resume the same attempt (a crash does not create a new attempt)

attempt not safely recoverable
    → durably record the attempt as Failed, then apply the normal
      retry / fallback / terminal rules (Sections 90, 206, 253)
```

Recovery MUST NOT return a record to `Unresolved`. Re-claiming would
discard the durable attempt count and create implicit retries (Section
206).

The canonical path itself does not change.

------------------------------------------------------------------------

# 124. Symlink Semantics

Default:

``` text
--links=copy
```

A symlink is copied as a symlink.

Its target is never entered into hardlink topology.

Example:

``` text
real/file
alias -> real/file
```

produces:

``` text
COPY real/file
SYMLINK alias -> real/file
```

Topology state tracks only the identity of `real/file`.

------------------------------------------------------------------------

# 125. Symlink Payload Preservation

Under:

``` text
--links=copy
```

Flux preserves the symlink object and its payload.

Both relative and absolute symlink payloads are copied **verbatim by
default**.

Examples:

``` text
link -> ../../etc/passwd
link -> /etc/passwd
link -> /var/lib/application/data
```

are recreated with the same link payload, subject only to the
destination filesystem's ability to represent the requested symlink.

Flux does not rewrite the target merely because it:

``` text
is outside the source tree
is absolute
resolves outside the destination tree
```

Flux MUST NOT:

``` text
follow the symlink
dereference the target
convert an absolute payload to a relative payload
rewrite the payload to remain inside the source tree
reject the symlink merely because the payload is absolute
```

The destination link is not required to resolve to an equivalent object.

This behavior preserves filesystem semantics rather than silently
changing user data.

An absolute symlink is therefore treated as user data, not as a request
to copy the object named by that payload.

------------------------------------------------------------------------

# 126. Symlink Safety

Copying a symlink payload does not follow it.

Therefore:

``` text
link -> ../../etc/passwd
link -> /etc/passwd
```

does not cause the referenced object to be opened or copied.

This is safe under default:

``` text
--links=copy
```

A future:

``` text
--links=follow
```

mode requires explicit containment, traversal, and cycle rules and must
be specified separately.

The implementation must preserve the distinction:

``` text
symlink payload
        !=
target object
```

at every stage of scanning, planning, execution, verification, recovery,
and garbage collection.

# 127. Windows Symlink Policy

Windows may require privileges or policy support to create symbolic
links.

Flux must not silently convert a requested symlink into:

``` text
junction
```

or:

``` text
hardlink
```

because those objects have different semantics.

Policy:

## `--links=copy`

If symlink creation fails because required Windows capability/privilege
is unavailable:

``` text
SYMLINK_CREATION_UNAVAILABLE
```

for strict behavior.

`auto` may be introduced later, but any degradation must be explicit and
must not silently change the object type.

No elevation prompt is performed by the core engine.

The CLI may provide a platform-specific diagnostic explaining the
required Windows privilege/policy.

------------------------------------------------------------------------

# 128. Destination Safety

Before an operation begins, Flux must determine whether:

``` text
destination == source
```

or:

``` text
destination is nested inside source
```

using both:

``` text
canonical/normalized path analysis
```

and filesystem identity checks where available.

Mount points, bind mounts, junctions, and symlink traversal must not
invalidate the containment decision.

The scanner must not be allowed to recursively discover Flux's own
destination tree through an alias.

------------------------------------------------------------------------

# 129. Directory Self-Copy Protection

For:

``` text
flux copy /data /data/backup
```

default behavior:

``` text
SAFETY_REJECTED
```

before transfer begins.

Flux must not rely on:

``` text
exclude destination
```

as the primary safety mechanism.

Explicit opt-in self-referential behavior is outside the initial scope.

------------------------------------------------------------------------

# 130. Persistent State Garbage Collection

Automatic cleanup:

``` text
heartbeat = 5s
stale threshold = 30s
default retention = 7 days
```

But retention and staleness are separate concepts.

An operation may be:

``` text
stale but retained
```

or:

``` text
active and never eligible
```

Deletion requires:

``` text
stale
+
older than retention
+
lock not held
+
final lock/check succeeds
```

------------------------------------------------------------------------

# 131. Cleanup Safety

`flux cleanup` must display classification:

``` text
ACTIVE
RESUMABLE
STALE
DELETE-CANDIDATE
```

No active operation may be deleted.

Manual force mode must still respect the active OS lock.

A `--force` option may bypass retention, but it must not bypass
active-operation ownership.

------------------------------------------------------------------------

# 132. Cancellation

On normal cancellation:

``` text
RUNNING
   ↓
PAUSING
   ↓
PAUSED
```

The engine should finish the smallest safe atomic unit before stopping.

For an atomic file copy, the temporary file remains resumable.

For a hardlink canonical copy:

``` text
canonical remains Copying
```

until the worker has durably recorded the paused/recoverable state.

------------------------------------------------------------------------

# 133. Failure Propagation

Errors propagate according to dependency type.

Independent file:

``` text
COPY_FAILED
```

Hardlink dependent:

``` text
BLOCKED_BY_CANONICAL_FAILURE
```

Operation-wide safety error:

``` text
operation cancellation/failure
```

Metadata-only failure:

``` text
metadata failure
```

must not be confused with content-copy failure.

Strictness is controlled by the metadata policy.

------------------------------------------------------------------------

# 134. Verification Pipeline

The streaming copy path is:

``` text
source read
   ↓
hash update
   ↓
destination write
   ↓
repeat
```

At EOF:

``` text
source digest finalized
```

Then:

``` text
source mutation validation
   ↓
destination verification if requested
   ↓
metadata
   ↓
flush
   ↓
lock revalidation
   ↓
commit
```

Verification is therefore not a separate conceptual contradiction: the
hash computation occurs during the copy phase, while the **decision to
publish** occurs only after the required verification gates complete.

------------------------------------------------------------------------

# 135. Destination Verification

If independent destination verification is enabled:

``` text
temporary destination
       ↓
read destination
       ↓
hash destination
       ↓
compare expected digest
```

Only then may atomic publication occur.

The source-stream digest remains useful even when destination
verification is enabled.

------------------------------------------------------------------------

# 136. Resume Chunk Model

A file is divided into fixed-size chunks:

``` text
chunk_size = configurable
```

Each completed chunk may store:

``` text
chunk index
source digest
destination digest where available
completion marker
```

Checkpoint storage must be persistent.

Chunk records may be compacted or garbage-collected after successful
file publication.

------------------------------------------------------------------------

# 137. Chunk Validation

Before resuming:

``` text
for each completed chunk:
    validate source
    validate partial destination
```

If mismatch:

``` text
invalidate checkpoint from mismatch onward
```

and resume from the earliest invalid region.

For maximum safety:

``` text
--resume-verify=full
```

may discard all partial progress and perform complete verification.

------------------------------------------------------------------------

# 138. Disk Full During Resume

If resume encounters:

``` text
ENOSPC
```

the operation remains recoverable where possible.

The partial state must be checkpointed consistently before returning
failure.

No state record may claim bytes are valid if their durable destination
content is not known to be valid.

------------------------------------------------------------------------

# 139. Durable Checkpoint Ordering

For a completed chunk:

``` text
write chunk data
    ↓
required flush policy
    ↓
write/update checkpoint
    ↓
checkpoint durable according to policy
```

Never mark a chunk complete before the destination data is sufficiently
durable for the configured crash-consistency guarantee.

------------------------------------------------------------------------

# 140. State Database Corruption

If:

``` text
manifest corrupt
state.db corrupt
topology.db corrupt
```

Flux must not guess.

Return:

``` text
STATE_CORRUPT
```

and preserve the workspace for diagnosis.

A future repair command may be introduced.

------------------------------------------------------------------------

# 141. Crash Consistency Levels

Flux should document:

``` text
logical correctness
```

separately from:

``` text
power-loss durability
```

Without explicit flush guarantees, successful `write()` does not
necessarily mean data survives sudden power loss.

The implementation may provide:

``` text
--durability=normal
--durability=strict
```

in a future release.

------------------------------------------------------------------------

# 142. Required Rust Interfaces

Core abstractions should include:

``` rust
trait Scanner {
    fn next(&mut self) -> Result<Option<DiscoveryRecord>>;
}

// Full definition, results, and fencing rules: Section 91.
trait TopologyStore {
    fn lookup(&self, identity: FileIdentity)
        -> Result<Option<TopologyRecord>>;

    fn claim_canonical(
        &self,
        identity: FileIdentity,
        operation_id: OperationId,
        canonical_path: RelativePath,
    ) -> Result<ClaimResult>;

    fn record_attempt_failure(
        &self,
        identity: FileIdentity,
        attempt_id: AttemptId,
        error_code: ErrorCode,
    ) -> Result<TransitionResult>;

    fn begin_attempt(
        &self,
        identity: FileIdentity,
        expected_attempt_id: AttemptId,
        candidate_path: RelativePath,
    ) -> Result<TransitionResult>;

    fn mark_materialized(
        &self,
        identity: FileIdentity,
        attempt_id: AttemptId,
        destination_path: RelativePath,
    ) -> Result<TransitionResult>;

    fn mark_failed(
        &self,
        identity: FileIdentity,
        attempt_id: AttemptId,
        error_code: ErrorCode,
    ) -> Result<TransitionResult>;
}

trait OperationStore {
    fn load_manifest(&self, id: OperationId)
        -> Result<OperationManifest>;

    fn checkpoint(&self, checkpoint: Checkpoint)
        -> Result<()>;
}

trait TargetLock {
    fn acquire(&self) -> Result<()>;
    fn verify_owned(&self) -> Result<()>;
    fn release(&self) -> Result<()>;
}
```

Exact signatures may evolve, but the semantics must remain.

------------------------------------------------------------------------

# 143. Implementation Checklist

Before implementing the first production worker, the coding agent must
have tests for:

``` text
[ ] deterministic path comparator
[ ] FluxPathKey
[ ] non-UTF-8 Unix names
[ ] Windows path ordering
[ ] FileIdentity strength
[ ] weak identity fallback
[ ] topology CAS
[ ] canonical claim
[ ] canonical failure propagation
[ ] dependent hold queue
[ ] target locking
[ ] operation locking
[ ] lock ordering
[ ] heartbeat
[ ] monotonic lease calculation
[ ] stale cleanup
[ ] commit-time lock verification
[ ] source mutation levels
[ ] chunk checkpoints
[ ] operation manifest
[ ] configuration fingerprint
[ ] partial-file mapping
[ ] atomic preflight
[ ] per-file capacity check
[ ] atomic commit
[ ] symlink isolation
[ ] Windows symlink failure
[ ] crash recovery
```

------------------------------------------------------------------------

# 144. Implementation Anti-Patterns

The coding agent must reject designs that introduce:

``` text
unbounded Vec<PathBuf> for the whole tree
one async task per file
one mutex around the entire topology database
blocking worker waits for canonical copies
dynamic canonical promotion
object_id == 0 treated as valid identity
wall-clock lease as sole ownership authority
filename-pattern-only resume discovery
metadata equality described as immutable-source proof
atomic=always silently falling back to in-place copy
symlink target resolution in default copy mode
silent Windows symlink-to-junction conversion
```

------------------------------------------------------------------------

# 145. Final Protocol Summary

Flux's finalized execution model is:

``` text
                 DETERMINISTIC SCANNER
                         │
                         ▼
                BOUNDED DISCOVERY
                         │
                         ▼
               TOPOLOGY CAS / STORE
                    │          │
             canonical      dependent
                    │          │
                    ▼          ▼
                 READY       HOLD
                    │          │
                    ▼          │
                COPY WORKER    │
                    │          │
              stream + hash    │
                    │          │
              verify/mutate    │
                    │          │
                    ▼          │
              MATERIALIZED ────┘
                    │
                    ▼
                 LINKS
                    │
                    ▼
              TARGET LOCK
                    │
              final recheck
                    │
                    ▼
                COMMIT
```

The durable control plane is:

``` text
DEST/.flux/operations/<operation-id>/
    ├── manifest
    ├── state.db
    ├── topology.db
    ├── checkpoints/
    └── lock
```

The single-file convenience layout is:

``` text
DEST/
    target
    target.flux-partial.<operation-id>
    target.flux-state.<operation-id>
    target.flux-lock
```

The resulting guarantees are:

``` text
bounded resident RAM
deterministic canonical hardlinks
no canonical promotion
no dependent-worker starvation
persistent topology
crash-resumable state
destination collision protection
weak-identity safety
explicit source-consistency guarantees
atomic capacity preflight
per-file capacity validation
commit-time lock validation
safe stale-state cleanup
strict symlink isolation
cross-platform deterministic ordering
```

This specification is considered the implementation baseline for Flux
0.1/0.2 architecture.

------------------------------------------------------------------------

# 146. Finalized I/O Scheduling and Pipeline Mechanics

This section resolves the remaining mechanical ambiguities identified
after V3 review. It is normative and overrides any earlier wording that
conflicts with it.

## 146.1 Atomic Preflight Does Not Block Streaming

`--atomic=always` must not impose a complete-tree preflight barrier
before the first byte can be transferred.

Flux uses concurrent advisory capacity planning:

``` text
                         ┌───────────────┐
                         │  Deterministic│
                         │    Scanner    │
                         └───────┬───────┘
                                 │
                    persistent capacity aggregate
                                 │
                                 ▼
                         ┌───────────────┐
                         │ Capacity      │
                         │ Planner       │
                         └───────────────┘
                                 ▲
                                 │
                                 │
Scanner ───────────────► Bounded Planner ───────────────► Workers
                                                          │
                                                          ▼
                                                   File execution
```

The capacity preflight and transfer pipeline run concurrently.

The scanner continuously contributes to a persistent aggregate such as:

``` rust
struct CapacityForecast {
    discovered_source_bytes: u128,
    estimated_atomic_temp_bytes: u128,
    completed_temp_bytes: u128,
    released_temp_bytes: u128,
}
```

The forecast is advisory until an individual file becomes executable.

### 146.2 Time-to-First-Byte Requirement

The implementation must not wait for complete namespace discovery solely
to establish the global atomic-capacity forecast.

A small operation should be capable of reaching:

``` text
scan
→ plan
→ capacity check
→ copy
```

without waiting for unrelated later directory entries to be scanned.

### 146.3 Definitive Per-File Capacity Gate

Before starting an atomic replacement for a file, Flux performs a
definitive capacity check based on the actual current filesystem state.

``` text
READY
  │
  ▼
definitive capacity check
  │
  ├── sufficient ───────► atomic copy
  │
  └── insufficient ─────► policy handling
```

This check is mandatory even when the global forecast says sufficient
capacity exists.

### 146.4 Capacity Shortfall During Streaming

If the aggregate forecast later indicates that continuing all currently
planned atomic operations may exceed the available capacity, the
scheduler may stop admitting additional large atomic actions.

Already-running operations are allowed to reach their next safe
checkpoint or completion/failure boundary.

Flux must not delete valid destination data merely to satisfy the
forecast.

### 146.5 Atomic Policy

For:

``` text
--atomic=always
```

an individual file that cannot satisfy the atomic capacity requirement
must fail rather than silently switch to in-place replacement.

For:

``` text
--atomic=auto
```

Flux may use the documented safe fallback to in-place replacement.

For:

``` text
--atomic=never
```

no temporary atomic replacement is required.

------------------------------------------------------------------------

# 147. Event-Driven Hardlink Hold Queue

## 147.1 Normal Wakeup Path

Canonical completion is event-driven.

When a canonical worker successfully materializes a hardlink target:

``` text
worker
  │
  ├── persist Materialized
  │
  └── emit CanonicalMaterialized(identity)
                    │
                    ▼
               Scheduler
                    │
                    ▼
             release dependents
```

The scheduler must not poll the topology store in the normal execution
path.

## 147.2 Event Semantics

The scheduler consumes an internal event:

``` rust
// Attempt fields and fencing rules: Section V14.3.
enum SchedulerEvent {
    CanonicalMaterialized {
        identity: FileIdentity,
        destination_path: RelativePath,
        operation_id: OperationId,
        attempt_id: AttemptId,
        attempt_number: u64,
    },

    CanonicalFailed {
        identity: FileIdentity,
        destination_path: RelativePath,
        operation_id: OperationId,
        attempt_id: AttemptId,
        attempt_number: u64,
        error_code: ErrorCode,
    },

    OperationStateChanged {
        operation_id: OperationId,
    },
}
```

The event itself is not the authoritative state.

The authoritative state remains in `topology.db`.

Therefore the scheduler may safely lose an event without corrupting
correctness.

## 147.3 Event Loss Recovery

After:

``` text
process restart
scheduler restart
worker crash
```

the scheduler reconstructs pending hardlink dependencies from persistent
topology state.

Recovery algorithm:

``` text
load topology records
      │
      ├── Materialized → release dependents
      │
      ├── Failed → block dependents
      │
      └── Copying/Unresolved → retain hold
```

No polling loop is required during normal operation.

## 147.4 Bounded Event Memory

Scheduler events are bounded by queue capacity.

The implementation must not create one unbounded event object per
hardlink dependent.

Large release operations may be processed incrementally:

``` text
Materialized
    ↓
release batch
    ↓
release batch
    ↓
release batch
```

This preserves the bounded-memory invariant.

------------------------------------------------------------------------

# 148. Checkpoint WAL and Batching

## 148.1 Per-Chunk Synchronous Writes Are Forbidden by Default

Flux must not perform a fully synchronous persistent database
transaction for every completed chunk.

For example:

``` text
500 GiB / 1 MiB
≈ 500,000 chunks
```

must not imply approximately 500,000 independent durable database
commits.

## 148.2 Write-Ahead Checkpoint Stream

Workers append checkpoint progress to an in-memory/WAL-backed checkpoint
stream.

Conceptually:

``` text
copy chunk
   ↓
destination data write
   ↓
checkpoint record appended
   ↓
batch/WAL flush
   ↓
durable checkpoint boundary
```

The exact WAL implementation is platform/database dependent.

## 148.3 Default Flush Policy

Normative defaults:

``` text
checkpoint flush interval = 1 second
checkpoint batch threshold = 64 MiB of completed source ranges
```

A flush occurs when either threshold is reached.

The implementation may flush earlier because of:

``` text
memory pressure
pause/cancel request
file completion
operation checkpoint
explicit sync request
```

## 148.4 Durability Ordering

A checkpoint may only be considered durable after the corresponding
destination data has crossed the configured durability boundary.

Required ordering:

``` text
destination data
      ↓
required data flush
      ↓
checkpoint WAL/database update
      ↓
checkpoint durability boundary
```

The exact strength of "data flush" depends on the selected durability
policy.

## 148.5 Crash Semantics

After a crash, Flux may redo work represented by checkpoint records that
were not durably committed.

It must never trust a checkpoint that was not durably recorded.

Therefore:

``` text
lost checkpoint
    → redo data

missing durable data
    → never claim completed
```

Correctness is preferred over minimizing redo work.

## 148.6 Checkpoint Compaction

Completed chunk ranges should be represented compactly.

Instead of:

``` text
500,000 independent rows
```

the store may represent contiguous ranges:

``` text
chunks 0..8191 complete
chunks 8192..16383 complete
...
```

This reduces metadata I/O and state size.

------------------------------------------------------------------------

# 149. Destination Containment --- Startup and Dynamic Enforcement

Destination safety uses both an initial physical-boundary check and
dynamic traversal checks.

## 149.1 Startup Boundary Establishment

Before scanning:

``` text
source
destination
```

are normalized and resolved as far as safely possible.

Flux records:

``` rust
struct SafetyBoundary {
    source_identity: Option<FileIdentity>,
    destination_identity: Option<FileIdentity>,
    source_physical_root: PhysicalPath,
    destination_physical_root: PhysicalPath,
}
```

For an existing destination, Flux resolves the existing path to its
physical representation.

For a destination that does not yet exist, Flux resolves the nearest
existing ancestor and records the unresolved suffix.

## 149.2 Initial Containment Check

The operation is rejected if the destination is physically within the
source tree under the applicable safety rules.

Example:

``` text
source      = /data
destination = /data/backup
```

must be rejected before transfer.

## 149.3 Dynamic Traversal Check

Startup resolution alone is insufficient because the filesystem can
change during execution.

Whenever traversal encounters a namespace boundary capable of changing
physical resolution, Flux must revalidate.

Examples:

``` text
mount point
bind mount
junction
reparse point
traversal-enabled symlink
directory replacement
filesystem transition
```

## 149.4 Existing Directory Identity

When entering an existing directory, Flux should obtain its
filesystem/object identity where strongly supported.

The scanner must verify that the object being entered remains consistent
with the planned directory identity.

If it changes unexpectedly:

``` text
DIRECTORY_CHANGED_DURING_SCAN
```

is raised or the directory is rescanned according to the configured
mutation policy.

## 149.5 New Destination Directories

If Flux creates destination directories during the operation, their
physical identities become part of the destination namespace state.

The scanner must never follow newly created destination paths as source
paths.

## 149.6 Safety Invariant

The combined invariant is:

> Flux must never recursively scan the destination as part of the source
> tree, even if filesystem namespace relationships change after startup.

Therefore:

``` text
startup check
      +
dynamic checks
      +
identity checks
```

are all part of the safety model.

------------------------------------------------------------------------

# 150. Final Scheduler Architecture

The finalized scheduler is:

``` text
                         ┌────────────────────┐
                         │ Deterministic      │
                         │ Scanner            │
                         └─────────┬──────────┘
                                   │
                            bounded discovery
                                   │
                                   ▼
                         ┌────────────────────┐
                         │ Planner            │
                         └─────────┬──────────┘
                                   │
                     ┌─────────────┴─────────────┐
                     │                           │
                     ▼                           ▼
                READY QUEUE                HOLD INDEX
                     │                           │
                     ▼                           │
                  Workers                        │
                     │                           │
          ┌──────────┴──────────┐                │
          │                     │                │
       canonical             independent         │
          │                     │                │
          ▼                     │                │
     Topology CAS               │                │
          │                     │                │
     copy + hash                │                │
          │                     │                │
          ▼                     │                │
    Materialized ── event ──────┴───────────────►│
                                                  │
                                            release batch
                                                  │
                                                  ▼
                                             READY QUEUE
```

The scheduler must not:

``` text
wait synchronously for canonical completion
poll topology continuously
allocate one task per discovered file
persist one synchronous database transaction per chunk
```

------------------------------------------------------------------------

# 151. Final Timing and Queue Defaults

Unless overridden by future configuration, the implementation baseline
is:

``` text
heartbeat interval:          5 s
stale lease threshold:       30 s
checkpoint flush interval:   1 s
checkpoint batch threshold:  64 MiB
```

These are implementation defaults, not correctness assumptions.

Correctness must remain valid if scheduling is delayed.

------------------------------------------------------------------------

# 152. Final Correctness Invariants

The implementation must preserve all of the following:

``` text
1. Canonical hardlink selection is deterministic and immutable.
2. Canonical failure never promotes another member to canonical (fallback anchors: Section 253).
3. Dependents never consume worker slots while waiting.
4. Canonical completion releases dependents through an event-driven path.
5. Persistent topology remains authoritative after event loss.
6. Atomic preflight never creates a complete-tree startup barrier.
7. Every atomic file receives a definitive capacity check.
8. --atomic=always never silently degrades.
9. Chunk checkpoints are batched/WAL-backed.
10. Unflushed checkpoint progress may be redone after crash.
11. A checkpoint never claims data that was not sufficiently durable.
12. Destination containment is checked at startup.
13. Destination containment is revalidated during sensitive traversal.
14. Physical identity is preferred over path text for safety decisions.
15. Active operation ownership is established by OS/process locks.
16. Heartbeats and leases are advisory stale-state mechanisms.
17. Commit operations revalidate ownership immediately before publication.
18. Weak filesystem identity never creates false hardlink groups.
19. Symlink targets are never entered into hardlink topology under --links=copy.
20. Memory usage remains bounded independently of total file count.
```

------------------------------------------------------------------------

# 153. Final Implementation Acceptance Tests

The coding agent must add tests covering:

``` text
[ ] atomic preflight does not delay first eligible file
[ ] concurrent preflight and copying
[ ] capacity forecast grows during scanning
[ ] capacity forecast stops new admissions safely
[ ] per-file atomic capacity failure
[ ] atomic=always refuses fallback
[ ] canonical materialization event
[ ] dependent release without polling
[ ] lost wakeup recovered from topology.db
[ ] large dependent release is batched
[ ] 500,000-chunk checkpoint workload
[ ] checkpoint batching
[ ] checkpoint WAL recovery
[ ] crash between data flush and checkpoint
[ ] crash between checkpoint append and durability
[ ] contiguous checkpoint range compaction
[ ] destination realpath startup resolution
[ ] destination with nonexistent suffix
[ ] mount transition during traversal
[ ] junction/reparse transition on Windows
[ ] directory replacement race
[ ] destination becomes physically contained after startup
[ ] source/destination identity collision
```

------------------------------------------------------------------------

# 154. Implementation Baseline Status

With Sections 146--153 incorporated, the Flux specification is
considered mechanically complete for the initial Rust implementation.

The implementation baseline now explicitly defines:

``` text
architecture
data structures
state machines
hardlink topology
canonical failure
bounded queues
scanner backpressure
event-driven scheduling
persistent recovery
checkpoint WAL/batching
source consistency levels
atomic replacement
atomic capacity planning
destination locking
operation locking
lease semantics
filesystem identity
cross-platform path ordering
symlink semantics
Windows fallbacks
destination containment
crash recovery
garbage collection
```

Any implementation deviation from a normative invariant must be treated
as a protocol change rather than an internal optimization.

------------------------------------------------------------------------

# 155. Durability & WAL Protocol

This section is normative and resolves the remaining persistence,
crash-consistency, filesystem-flush, WAL lifecycle, and multi-worker
checkpoint ambiguities.

The durability architecture is:

``` text
Workers
   │
   │ checkpoint messages
   ▼
bounded checkpoint channel
   │
   ▼
single WAL writer
   │
   ├── append
   ├── batch
   └── durability flush
   │
   ▼
persistent state / compacted checkpoint state
```

Workers must not independently perform arbitrary persistent checkpoint
transactions.

------------------------------------------------------------------------

# 156. WAL Design Goals

The Flux WAL must provide:

``` text
ordered checkpoint records
crash recovery
torn-write detection
duplicate detection
bounded replay
generation tracking
safe compaction
safe rotation
```

The WAL is an append-only journal of durable progress.

It is not the sole long-term representation of operation state.

After successful compaction, the durable state database contains the
compact representation required for resume, while the WAL contains only
records newer than the durable compaction boundary.

------------------------------------------------------------------------

# 157. Single WAL Writer

Each operation has exactly one authoritative WAL writer.

Workers communicate through a bounded checkpoint channel:

``` text
worker 1 ─┐
worker 2 ─┤
worker 3 ─┼──► bounded channel ───► WAL writer
worker N ─┘
```

This prevents:

``` text
multiple workers
    ↓
concurrent WAL mutation
    ↓
ordering ambiguity
```

The WAL writer assigns the authoritative sequence number.

Workers must not choose global WAL sequence numbers themselves.

------------------------------------------------------------------------

# 158. Checkpoint Message

Conceptually:

``` rust
struct CheckpointMessage {
    operation_id: OperationId,
    file_id: FileId,
    generation: u64,
    range_start: u64,
    range_end: u64,
    digest: Option<Digest>,
}
```

The exact representation may evolve.

The message represents progress observed by a worker.

The WAL writer determines when that progress becomes durable.

------------------------------------------------------------------------

# 159. WAL Record Format

Each WAL record must contain enough information to detect truncation,
corruption, duplication, and ordering errors.

Minimum logical structure:

``` text
magic
format_version
operation_id
generation
sequence_number
record_type
payload_length
payload
checksum
commit marker
```

Binary encoding is implementation-defined but must be versioned.

The checksum must cover all record content necessary to detect
corruption, including:

``` text
header
sequence
type
payload
```

The checksum algorithm must be explicitly versioned.

------------------------------------------------------------------------

# 160. WAL Record Atomicity

A record is considered valid only when:

``` text
header valid
+
payload length valid
+
payload complete
+
checksum valid
+
commit marker valid
```

If the final WAL record is torn because of:

``` text
power loss
process crash
partial write
filesystem failure
```

recovery must truncate the WAL at the last valid record boundary.

Flux must never attempt to interpret a partially valid final record as a
completed checkpoint.

------------------------------------------------------------------------

# 161. WAL Sequence Numbers

Sequence numbers are monotonically increasing within an operation
generation:

``` text
generation 7:
    seq 1
    seq 2
    seq 3
    ...
```

A sequence number must never be reused within the same generation.

Recovery must reject impossible sequence regressions or conflicting
duplicate records.

------------------------------------------------------------------------

# 162. Operation Generations

A new recovery epoch increments the operation generation.

Example:

``` text
generation 1
    crash
generation 2
    resume
```

This prevents stale WAL records from an abandoned execution epoch from
being confused with records generated after recovery.

The operation manifest records the current generation durably.

------------------------------------------------------------------------

# 163. WAL Append Ordering

The WAL writer must serialize records in sequence order.

Required conceptual order:

``` text
receive checkpoint
      ↓
assign sequence
      ↓
append record
      ↓
batch
      ↓
flush WAL
      ↓
checkpoint becomes durable
```

A worker receiving acknowledgement that a checkpoint is durable may rely
on that checkpoint during future resume.

------------------------------------------------------------------------

# 164. Data-before-Checkpoint Rule

A checkpoint must never become durable before the corresponding
destination data has crossed the configured durability boundary.

Required ordering:

``` text
destination write
      ↓
destination durability operation
      ↓
WAL append
      ↓
WAL durability operation
      ↓
durable checkpoint acknowledgement
```

This ordering applies to each batch.

For multiple files/chunks in one batch, all corresponding data must
satisfy the configured data-durability boundary before the WAL batch is
acknowledged as durable.

------------------------------------------------------------------------

# 165. Durability Modes

Flux defines:

``` text
--durability=normal
--durability=strict
```

## Normal

Optimizes throughput while maintaining Flux's documented crash-recovery
invariants.

The filesystem's normal flush semantics are used.

## Strict

Requests the strongest durability primitives available on the platform
and filesystem.

If the platform cannot establish the requested durability guarantee,
Flux must report the limitation rather than falsely claiming strict
durability.

------------------------------------------------------------------------

# 166. Linux Durability Mapping

On Linux, the platform layer must distinguish:

``` text
fsync(fd)
fdatasync(fd)
directory fsync
```

as separate capabilities.

For strict file durability, the implementation should use the strongest
appropriate primitive supported by the operation.

For atomic rename/publish sequences, directory durability must also be
considered.

The implementation must not assume that flushing a file descriptor alone
establishes durable directory-entry persistence.

------------------------------------------------------------------------

# 167. macOS Durability Mapping

The macOS backend must explicitly define its mapping from Flux
durability levels to available filesystem synchronization primitives.

The implementation must not equate a successful generic `fsync()` call
with an unconditional power-loss durability guarantee across all macOS
filesystems.

Where stronger platform-specific synchronization is required for the
selected durability mode, it must be used when available.

------------------------------------------------------------------------

# 168. Windows Durability Mapping

On Windows, the platform layer must define the mapping to:

``` text
FlushFileBuffers
```

and the appropriate directory/rename durability behavior supported by
the filesystem.

The implementation must distinguish:

``` text
file content durability
```

from:

``` text
directory entry / rename durability
```

A successful flush must not automatically be interpreted as a universal
guarantee across network filesystems.

------------------------------------------------------------------------

# 169. Network Filesystem Policy

SMB, NFS, and similar remote filesystems require explicit capability
classification.

Flux must distinguish:

``` text
local filesystem
remote filesystem with known flush semantics
remote filesystem with uncertain durability semantics
```

For `--durability=strict`, Flux must either:

``` text
establish a supported durability contract
```

or report:

``` text
STRICT_DURABILITY_UNAVAILABLE
```

It must not silently claim local power-loss-equivalent guarantees for an
unverified remote filesystem.

------------------------------------------------------------------------

# 170. WAL and Destination Filesystem

The WAL must be stored in the destination operation workspace.

Flux must not assume that:

``` text
source filesystem
```

and:

``` text
destination filesystem
```

share durability semantics.

The WAL and destination data should normally reside in the same
destination durability domain.

If the implementation supports an alternate state location, that
location becomes part of the durability contract and must be explicitly
validated.

------------------------------------------------------------------------

# 171. WAL Flush Batching

The V4 defaults remain:

``` text
checkpoint flush interval = 1 second
checkpoint batch threshold = 64 MiB
```

The WAL writer flushes when either threshold is reached.

It may flush earlier for:

``` text
file completion
operation pause
operation cancellation
shutdown
memory pressure
explicit durability request
```

------------------------------------------------------------------------

# 172. Backpressure from WAL

The checkpoint channel is bounded.

If workers generate checkpoint records faster than the WAL writer can
persist them:

``` text
workers
   ↓
bounded channel full
   ↓
checkpoint producer backpressure
```

The engine must not allocate unlimited checkpoint messages.

Workers may temporarily coalesce contiguous ranges before submitting
them.

The copy data path must not require one persistent record per byte or
per I/O operation.

------------------------------------------------------------------------

# 173. Checkpoint Coalescing

Adjacent completed ranges should be merged before WAL submission when
safe.

Example:

``` text
0..1 MiB
1..2 MiB
2..3 MiB
```

may become:

``` text
0..3 MiB
```

provided the digest/checkpoint semantics remain valid.

The implementation must preserve enough information to determine the
correct resume verification behavior.

------------------------------------------------------------------------

# 174. WAL Replay

Recovery:

``` text
open WAL
    ↓
scan sequentially
    ↓
validate framing/checksum
    ↓
stop at first invalid trailing record
    ↓
replay valid records
    ↓
reconstruct checkpoint state
```

A corruption detected in the middle of the WAL, rather than merely at
the physical end, must not be silently skipped.

Return:

``` text
WAL_CORRUPT
```

unless a future explicit repair mechanism establishes a safe recovery
strategy.

------------------------------------------------------------------------

# 175. Duplicate WAL Records

Recovery may encounter duplicate logical records due to crash timing or
replay.

Records are identified by:

``` text
operation_id
generation
sequence_number
```

Duplicate replay of an already-applied sequence must be idempotent.

Conflicting payloads for the same sequence number are corruption:

``` text
WAL_SEQUENCE_CONFLICT
```

------------------------------------------------------------------------

# 176. WAL Compaction

The operation state database periodically materializes the effective
checkpoint state.

Conceptually:

``` text
WAL
 │
 ├── seq 1
 ├── seq 2
 ├── ...
 └── seq N
       │
       ▼
 durable checkpoint snapshot
       │
       ▼
 WAL truncation boundary
```

The snapshot must become durable before WAL records covered by it are
reclaimed.

------------------------------------------------------------------------

# 177. WAL Truncation Ordering

Never perform:

``` text
truncate WAL
    ↓
write checkpoint snapshot
```

The required order is:

``` text
write snapshot
    ↓
flush snapshot
    ↓
write durable compaction marker
    ↓
flush marker
    ↓
truncate/rotate obsolete WAL
```

After crash, recovery determines the last durable compaction boundary.

Anything after that boundary remains replayable.

------------------------------------------------------------------------

# 178. WAL Rotation

Large WALs must be rotated into segments.

Example:

``` text
wal.000001
wal.000002
wal.000003
```

A segment may be deleted only after all records it contains are covered
by a durable state snapshot and compaction marker.

A crash during rotation must leave at least one recoverable
representation of every acknowledged checkpoint.

------------------------------------------------------------------------

# 179. Compaction Generation

Each durable checkpoint snapshot receives a generation/epoch identifier:

``` text
snapshot_generation
```

The operation manifest records the latest durable snapshot generation.

Recovery uses:

``` text
manifest
+
snapshot
+
WAL
```

to determine the correct replay boundary.

------------------------------------------------------------------------

# 180. Crash Matrix

The implementation must explicitly test:

``` text
1. crash after destination write
2. crash during destination flush
3. crash after destination flush
4. crash during WAL append
5. crash after WAL append
6. crash during WAL flush
7. crash after WAL flush
8. crash during snapshot creation
9. crash after snapshot write but before snapshot flush
10. crash after snapshot flush
11. crash during compaction marker write
12. crash after compaction marker durability
13. crash during WAL truncation
14. crash during WAL rotation
15. crash immediately before atomic rename
16. crash immediately after atomic rename
```

For every case, recovery must produce either:

``` text
known durable progress
```

or:

``` text
safe redo
```

Never:

``` text
false completion
```

------------------------------------------------------------------------

# 181. Atomic Rename Durability

Atomic namespace publication has two independent properties:

``` text
atomicity
durability
```

A successful atomic rename guarantees the namespace transition according
to filesystem semantics.

It does not automatically prove that the rename survives power loss.

For strict durability, the platform adapter must apply the required
post-rename directory synchronization where supported.

------------------------------------------------------------------------

# 182. WAL and Atomic Commit Ordering

For a file whose successful publication must be recoverable:

``` text
copy temporary file
      ↓
verify
      ↓
apply metadata
      ↓
flush data as required
      ↓
record PREPARE_COMMIT
      ↓
flush WAL
      ↓
revalidate locks
      ↓
atomic rename
      ↓
flush destination directory as required
      ↓
record COMMIT
      ↓
flush WAL
```

The exact optimization may collapse records, but recovery semantics must
remain equivalent.

------------------------------------------------------------------------

# 183. Commit Recovery

If recovery finds:

``` text
PREPARE_COMMIT
```

without:

``` text
COMMIT
```

it must inspect the destination namespace.

Possible outcomes:

``` text
rename occurred
    → finalize committed state

rename did not occur
    → resume/retry publication safely
```

It must never assume either outcome solely from the presence of
`PREPARE_COMMIT`.

------------------------------------------------------------------------

# 184. Rename Identity Validation

Where supported, recovery should validate the destination object
identity before interpreting an ambiguous commit state.

This protects against confusing:

``` text
old destination
```

with:

``` text
new Flux temporary object
```

after a crash.

------------------------------------------------------------------------

# 185. WAL Ownership and Locks

The WAL writer runs under the operation lock.

The operation lock must remain valid for:

``` text
WAL append
WAL flush
state snapshot
compaction
```

Before destructive WAL operations such as:

``` text
truncate
rotate
delete
```

the process must revalidate ownership.

------------------------------------------------------------------------

# 186. Shutdown Protocol

Normal shutdown:

``` text
stop accepting new work
      ↓
finish safe worker boundaries
      ↓
drain checkpoint channel
      ↓
flush WAL
      ↓
persist final operation state
      ↓
release operation/target locks
```

The WAL writer must not exit while checkpoint messages remain
unprocessed.

------------------------------------------------------------------------

# 187. Cancellation Protocol

Cancellation must distinguish:

``` text
immediate cancellation request
```

from:

``` text
durable cancellation state
```

Before reporting the operation as safely paused/cancelled, Flux must
ensure that all checkpoint messages necessary to resume safely have been
drained and durably persisted.

------------------------------------------------------------------------

# 188. Memory Bound

WAL batching must preserve the global bounded-memory invariant.

The implementation must bound:

``` text
checkpoint channel
WAL writer pending batch
coalescing buffers
```

by explicit limits.

A huge file must not cause unlimited checkpoint metadata to accumulate
in RAM.

------------------------------------------------------------------------

# 189. WAL Failure Semantics

If the WAL becomes unwritable:

``` text
disk full
I/O error
permission failure
filesystem corruption
```

Flux must not continue indefinitely while claiming resumable progress.

Policy:

``` text
stop admitting new checkpoint-dependent work
drain what can safely be persisted
enter operation failure/paused state
```

The final state must accurately indicate whether resume state is
trustworthy.

------------------------------------------------------------------------

# 190. WAL Disk-Full Handling

WAL capacity must be included in operation workspace capacity planning.

If WAL growth approaches a configured safety threshold:

``` text
trigger compaction
```

If compaction cannot free sufficient space:

``` text
WAL_SPACE_EXHAUSTED
```

The engine must fail or pause safely according to operation policy.

It must not delete the only durable checkpoint representation merely to
free space.

------------------------------------------------------------------------

# 191. WAL Corruption Handling

Corruption classification:

``` text
TRAILING_TORN_RECORD
    → truncate to last valid boundary

MIDDLE_RECORD_CORRUPTION
    → WAL_CORRUPT

SEQUENCE_CONFLICT
    → WAL_SEQUENCE_CONFLICT

UNKNOWN_FORMAT
    → WAL_FORMAT_UNSUPPORTED
```

The implementation must never silently skip an invalid middle record.

------------------------------------------------------------------------

# 192. WAL Security and Integrity

The WAL is local operation state, not a cryptographic security boundary.

Nevertheless:

``` text
permissions
ownership
directory ACLs
```

must follow the destination operation workspace security policy.

The implementation must avoid exposing source data unnecessarily in
checkpoint records.

Checkpoint records should contain hashes/metadata rather than raw file
contents.

------------------------------------------------------------------------

# 193. State Format Versioning

The WAL, checkpoint snapshot, and manifest formats must carry
independent format versions.

Example:

``` text
manifest_format = 2
wal_format = 3
checkpoint_format = 2
```

A future Flux release must be able to reject incompatible formats
cleanly.

No heuristic parsing of unknown formats is permitted.

------------------------------------------------------------------------

# 194. WAL Recovery Algorithm

Normative recovery order:

``` text
1. acquire operation lock
2. load manifest
3. identify current operation generation
4. load latest durable checkpoint snapshot
5. identify durable WAL compaction boundary
6. open WAL segments
7. validate records sequentially
8. truncate only a torn trailing record
9. reject middle corruption
10. replay records after snapshot boundary
11. reconstruct effective checkpoint state
12. reconcile filesystem state
13. resume operation
```

Filesystem inspection is required where WAL state alone cannot
distinguish an interrupted namespace commit.

------------------------------------------------------------------------

# 195. WAL Performance Requirements

The WAL implementation must be designed so that checkpoint persistence
is not normally the bottleneck for large sequential transfers.

The following are explicit anti-requirements:

``` text
no fsync per 1 MiB chunk by default
no database transaction per chunk by default
no unbounded checkpoint queue
no per-worker WAL file
no synchronous metadata update for every read syscall
```

The hot data path should remain dominated by:

``` text
source read
destination write
```

rather than metadata persistence.

------------------------------------------------------------------------

# 196. Platform Capability Interface

The filesystem abstraction should expose durability capabilities:

``` rust
struct DurabilityCapabilities {
    file_flush: bool,
    data_only_flush: bool,
    directory_flush: bool,
    atomic_rename: bool,
    rename_durability: bool,
    reliable_remote_flush: bool,
}
```

The capability result must be associated with the relevant filesystem.

A single global platform boolean is insufficient.

------------------------------------------------------------------------

# 197. Final Persistence Architecture

The complete durable control plane is:

``` text
                    ┌──────────────────┐
                    │ Operation Lock   │
                    └────────┬─────────┘
                             │
Workers ──► bounded checkpoint channel
                             │
                             ▼
                    ┌──────────────────┐
                    │ Single WAL Writer│
                    └────────┬─────────┘
                             │
                    batch / coalesce
                             │
                             ▼
                       WAL segments
                             │
                      durable flush
                             │
                             ▼
                    checkpoint snapshot
                             │
                      compaction marker
                             │
                             ▼
                       state.db
```

This architecture preserves:

``` text
bounded memory
high sequential throughput
ordered durable progress
crash recovery
safe replay
portable durability policy
```

------------------------------------------------------------------------

# 198. Final V5 Acceptance Criteria

The implementation is not considered persistence-complete until it
passes:

``` text
[ ] torn trailing WAL record recovery
[ ] middle WAL corruption detection
[ ] sequence conflict detection
[ ] duplicate replay idempotence
[ ] generation isolation
[ ] single WAL writer enforcement
[ ] bounded checkpoint channel
[ ] checkpoint coalescing
[ ] 1-second/64-MiB batching
[ ] snapshot-before-truncation
[ ] safe WAL rotation
[ ] crash during WAL append
[ ] crash during WAL flush
[ ] crash during snapshot
[ ] crash during compaction
[ ] crash during truncation
[ ] crash immediately before rename
[ ] crash immediately after rename
[ ] atomic commit recovery
[ ] Linux durability mapping
[ ] macOS durability mapping
[ ] Windows durability mapping
[ ] network filesystem classification
[ ] strict durability refusal when unsupported
[ ] WAL disk-full handling
[ ] WAL corruption handling
[ ] normal shutdown drain
[ ] cancellation drain
[ ] memory-bound enforcement
[ ] operation-lock validation during WAL lifecycle
```

------------------------------------------------------------------------

# 199. Final Flux Specification Status

With Sections 155--198 incorporated, the Flux specification is now the
implementation baseline for:

``` text
scanner
planner
bounded execution
hardlink topology
canonical selection
dependent hold queues
event-driven scheduling
source mutation detection
resume checkpoints
atomic replacement
capacity planning
destination safety
target locking
operation locking
lease management
filesystem identity
cross-platform path ordering
symlink handling
persistent operation state
WAL durability
checkpoint batching
crash recovery
WAL compaction
WAL rotation
platform-specific flushing
network filesystem durability
```

The remaining implementation choices should be treated as engineering
details unless they alter one of the normative invariants above.

Flux's persistence architecture is explicitly designed around the
principle:

> **Workers move bytes; the WAL writer records durable progress;
> recovery reconstructs truth from durable state and the filesystem,
> never from assumptions about what probably happened.**

------------------------------------------------------------------------

# 200. Canonical Execution Attempts and Retry Semantics

This section supersedes any earlier interpretation that a canonical
topology record must transition directly from `Failed` back to
`Copying`.

Flux separates:

``` text
logical topology state
```

from:

``` text
individual execution attempt state
```

The canonical relative path remains immutable.

------------------------------------------------------------------------

# 201. Topology State

The logical topology state is:

``` rust
enum TopologyState {
    Unresolved,
    Copying,
    Materialized,
    Failed,
}
```

This state describes the current logical disposition of the canonical
object.

It does not represent the complete history of execution attempts.

This list names the logical states only. The authoritative
payload-bearing definition, including the current attempt and the
materialization anchor, is Section 90 (Section V14.4).

------------------------------------------------------------------------

# 202. Canonical Execution Attempt

Each attempt has a unique identifier:

``` rust
struct CanonicalExecution {
    attempt_id: AttemptId,
    operation_id: OperationId,
    canonical_path: RelativePath,
    // Member this attempt materializes; equals canonical_path
    // unless this is a fallback attempt (Section 253).
    candidate_path: RelativePath,
    // Ordinal within the hardlink group, across retries and fallback
    // candidates; 1 = initial attempt (Section 206).
    attempt_number: u64,
    state: ExecutionState,
    started_at: Timestamp,
    finished_at: Option<Timestamp>,
    error: Option<ErrorCode>,
}
```

where:

``` rust
enum ExecutionState {
    Running,
    Failed,
    Succeeded,
}
```

Every retry creates a new execution attempt.

------------------------------------------------------------------------

# 203. Retry Lifecycle

The legal lifecycle is:

``` text
Topology:                 Current attempt:
    Unresolved
       ↓
    Copying               Attempt 1  Running → Failed
       │                     │ retry
       │                     ▼
    Copying               Attempt 2  Running → Succeeded
       ↓
    Materialized
```

A retry never leaves `Copying`. It is implemented by recording the
failed attempt and creating a **new execution attempt** that becomes
`current_attempt_id` (Section 90), not by mutating the historical failed
attempt. `TopologyState::Failed` is terminal and is reached only when no
retry or fallback candidate remains.

Example:

``` text
Attempt 1:
    Running → Failed

Attempt 2:
    Running → Succeeded
```

The topology record can therefore represent:

``` text
current state      = Copying
current_attempt_id = A2
attempt_number     = 2
```

while retaining the immutable history of Attempt 1.

------------------------------------------------------------------------

# 204. Canonical Identity Is Immutable

Retry must never change the canonical member.

If:

``` text
canonical = A/file
```

then all retries remain:

``` text
canonical = A/file
```

Flux must never promote:

``` text
M/file
Z/file
```

merely because `A/file` failed.

Deterministic canonical selection therefore remains independent of
transient execution failures.

------------------------------------------------------------------------

# 205. Dependent Hardlink Behavior During Retry

If a canonical attempt fails and a retry or fallback candidate remains:

``` text
attempt N Failed
    ↓
record stays Copying
    ↓
attempt N+1 Running
```

dependents stay held (Section 94) until an attempt reaches:

``` text
Materialized
```

Only then may they be released.

A retry must not prematurely release dependents merely because copying
has restarted.

Dependents become:

``` text
BLOCKED_BY_CANONICAL_FAILURE
```

only when the record reaches the terminal `TopologyState::Failed`.

A `CanonicalFailed` scheduler event reports a failed attempt. After
attempt fencing (Section V14.3), the scheduler either begins a retry or
fallback attempt (`begin_attempt`) or, when no viable candidate remains,
applies the terminal `mark_failed`. Only the latter blocks dependents.

------------------------------------------------------------------------

# 206. Retry Limits

The operation must support an explicit retry policy:

``` text
--retries=N
```

The **normative default is 3 retries**.

Unless the user explicitly requests another policy:

``` text
default retries = 3
```

The value `3` is part of the FLUX protocol semantics and MUST NOT be
silently changed by an implementation, platform adapter, build profile,
or deployment configuration.

An explicit finite value:

``` text
--retries=N
```

overrides the default for that operation.

An explicit unlimited policy (`--retries=unlimited`) may be requested by
the user and is the only case in which the retry count is unbounded.

Retry count semantics are:

``` text
--retries=0  → no automatic retry after the initial execution attempt
--retries=1  → at most one retry after the initial attempt
--retries=3  → at most three retries after the initial attempt
```

Therefore, by default a file, or a whole hardlink group, may use up to
four execution attempts: the initial attempt plus three retries.

For a hardlink group the budget is shared: retries of one candidate and
attempts on fallback candidates (Section 253) all draw on the same
`initial + N` attempts. Falling back never grants a fresh budget.

Budget exhaustion produces:

``` text
CANONICAL_RETRY_EXHAUSTED
```

and the topology state becomes `Failed`. A group can also become
`Failed` before the budget is spent: when the failure is file-wide, or
when no candidate can remain (Section 253.5). Dependents then become:

``` text
BLOCKED_BY_CANONICAL_FAILURE
```

and are not copied as ordinary independent files.

The retry counter is durable operation state and MUST survive restart. A
crash MUST NOT reset the retry count and thereby create additional
implicit retries.

Every retry still creates a new execution attempt; retry numbering does
not mutate the historical failed attempt.

------------------------------------------------------------------------

# 207. Retry Classification

Not every error should automatically be retried.

Errors should be classified:

``` text
retryable
non_retryable
operator_action_required
source_mutation
capacity_failure
lock_conflict
filesystem_identity_failure
```

Examples of generally retryable conditions:

``` text
transient I/O failure
temporary resource exhaustion
recoverable network filesystem interruption
```

Examples of generally non-retryable conditions:

``` text
permission denied
invalid source object
unsupported filesystem operation
permanent path conflict
```

The classification is implementation-defined but must be deterministic
and observable.

Independently, every failure of a hardlink candidate is classified by
scope:

``` text
path_scoped
    tied to one member's path; another member of the same object
    may succeed. Examples: invalid or colliding destination name
    (Section 241.5), destination-side permission on the member's
    parent directory, path too long for the destination.

object_scoped
    tied to the source object itself; every member reads the same
    object and would fail the same way. Examples: source read I/O
    error, source mutation, capacity failure, filesystem identity
    failure.
```

Only `path_scoped` failures permit fallback materialization (Section
253.2). When the scope cannot be established, the failure is treated as
`object_scoped`.

------------------------------------------------------------------------

# 208. Retry State Persistence

Attempt creation and topology-state updates must be persisted
transactionally with respect to the operation's durable state model.

After a crash, Flux must be able to distinguish:

``` text
attempt never started
attempt started
attempt failed
retry requested
retry running
retry succeeded
```

No retry may be inferred solely from an absent temporary file.

------------------------------------------------------------------------

# 209. Lossless Checkpoint Coalescing

Checkpoint coalescing is a transport/storage optimization.

It must never destroy information required by the selected
resume-verification policy.

The logical checkpoint model remains:

``` rust
struct ChunkCheckpoint {
    offset: u64,
    length: u64,
    digest: Digest,
}
```

A coalesced record may contain:

``` rust
struct CheckpointRange {
    start: u64,
    end: u64,
    chunks: Vec<ChunkCheckpoint>,
}
```

------------------------------------------------------------------------

# 210. Chunk Digest Retention

For:

``` text
--resume-verify=chunks
```

every constituent chunk digest must remain individually recoverable.

Example:

``` text
chunk 0 → H0
chunk 1 → H1
chunk 2 → H2
```

may be serialized as one WAL record:

``` text
range = 0..3MiB

[
    {0..1MiB, H0},
    {1..2MiB, H1},
    {2..3MiB, H2}
]
```

but Flux must not replace this with only:

``` text
range = 0..3MiB
hash = H012
```

because the latter cannot support granular chunk validation.

------------------------------------------------------------------------

# 211. Coalescing Rules by Resume Policy

The information-retention requirement depends on the selected resume
policy.

## Strict Chunk Verification

For:

``` text
--resume-verify=chunks
```

retain:

``` text
offset
length
digest
```

for every completed chunk until the operation is successfully finalized.

## Weaker Verification

For weaker policies, Flux may compact granular checkpoint information if
the resulting representation still provides the exact guarantees
required by that policy.

The implementation must document the point at which individual chunk
digests cease to be recoverable.

------------------------------------------------------------------------

# 212. Checkpoint Compaction Invariant

Compaction must be information-preserving with respect to the active
resume policy.

Formally:

``` text
Verify(compacted_state, policy)
```

must provide no weaker guarantee than:

``` text
Verify(original_checkpoint_state, policy)
```

for the same operation.

A compaction algorithm that makes strict chunk verification impossible
is prohibited while that verification policy is active.

------------------------------------------------------------------------

# 213. Adjacent Artifact Garbage Collection

Flux uses two distinct cleanup domains.

## Managed Control Plane

Central operation artifacts under:

``` text
DEST/.flux/
```

may be enumerated and cleaned according to the operation lifecycle.

This includes:

``` text
operations/
topology/
WAL/
state/
manifests/
```

## Adjacent Single-File Artifacts

Single-file transfers may use:

``` text
target.flux-lock
target.flux-partial.<operation-id>
target.flux-state.<operation-id>
```

These files are intentionally outside the centralized control-plane
directory.

They are therefore subject to a stricter ownership test before deletion.

------------------------------------------------------------------------

# 214. GC Scope Rule

`flux cleanup` must **not** recursively search arbitrary filesystem
trees for names matching:

``` text
*.flux-lock
*.flux-partial.*
*.flux-state.*
```

Filename pattern matching alone is insufficient evidence of Flux
ownership.

This prohibition exists to prevent accidental deletion of:

``` text
unrelated user files
files belonging to another Flux operation
files belonging to another installation
manually created files with similar names
```

------------------------------------------------------------------------

# 215. Adjacent Artifact Ownership Record

Each adjacent artifact must contain or be associated with verifiable
operation metadata sufficient to establish:

``` text
operation_id
target_identity
source_identity
artifact_type
format_version
creation_time
```

The exact binary/sidecar representation is implementation-defined.

GC must validate the artifact before deletion.

------------------------------------------------------------------------

# 216. Adjacent Artifact GC Preconditions

An adjacent artifact may be deleted only when all applicable conditions
hold:

``` text
1. artifact is positively identified as a Flux artifact
2. artifact metadata is structurally valid
3. operation identity is valid
4. target identity matches recorded ownership
5. operation is stale or completed
6. no live operation lock exists
7. no valid target lock is held
8. artifact is not required by a recoverable operation
```

These checks are necessary but are not sufficient if they become stale
before the destructive operation.

If ownership cannot be established:

``` text
DO NOT DELETE
```

GC must treat uncertainty as preservation.

------------------------------------------------------------------------

# 217. Adjacent Artifact Live-Lock Revalidation

Before deleting an adjacent artifact, GC MUST establish authoritative
absence of the locks protecting the artifact's operation and destination
target.

The final lock/ownership validation MUST occur immediately before the
destructive operation and MUST NOT rely solely on an earlier
observation.

The required semantic sequence is:

``` text
inspect artifact
      ↓
validate ownership
      ↓
establish GC/exclusion authority where required
      ↓
revalidate operation lock
      ↓
revalidate target lock
      ↓
revalidate artifact generation/ownership
      ↓
delete artifact
      ↓
release GC/exclusion authority
```

The exact synchronization primitive is platform-specific, but the
semantic invariant is mandatory.

A stale heartbeat, stale timestamp, or missing heartbeat is never
sufficient evidence that an artifact is safe to delete.

A live process that owns the authoritative lock protects its artifacts
even if:

``` text
clock time is unusual
system was suspended
heartbeat was delayed
network connectivity was interrupted
```

If authoritative lock state cannot be determined reliably:

``` text
DO NOT DELETE
```

If ownership or lock state changes before deletion can safely occur, GC
MUST abort the deletion.

GC MUST NOT delete a partial or state artifact belonging to an operation
that can still establish authoritative ownership of the corresponding
operation or destination target.

The existing operation lock and target-lock protocols remain
authoritative.

# 218. Completed Operation Cleanup

After successful finalization:

``` text
temporary partial
WAL
operation state
topology store
```

may be removed according to the cleanup lifecycle.

For isolated single-file operations, the authoritative durable record
for post-commit cleanup status is the adjacent target state record:

``` text
target.flux-state.<operation-id>
```

This is the only on-disk name for the single-file state record. An
unsuffixed `target.flux-state` is not a conforming representation, even
if it stores the operation identity internally.

The centralized managed `.flux` control plane MUST NOT be required
merely to remember post-commit cleanup status for an isolated
single-file transfer.

For a successfully committed single-file transfer, Flux MUST durably
record the committed operation state before treating cleanup as
non-essential.

If immediate deletion of any disposable adjacent artifact fails, the
adjacent state record MUST durably record:

``` text
operation_state = COMPLETED
cleanup_pending = true
```

and MUST identify the outstanding cleanup work sufficiently to permit
safe later rediscovery. At minimum, the durable cleanup-pending record
must retain:

``` text
operation_id
attempt_id
target_identity
target_path_key
source_identity
cleanup_pending
cleanup_pending_artifacts
artifact_generation
```

The state record MUST be durably persisted before the operation releases
its final ownership/lock state.

The lifecycle is:

``` text
target commit becomes durable
        ↓
update adjacent state:
    operation_state = COMPLETED
    cleanup_pending = true
        ↓
durably flush adjacent state
        ↓
revalidate ownership and locks
        ↓
delete disposable adjacent artifacts
        ↓
if all cleanup succeeds:
    clear/remove cleanup state
        ↓
if cleanup fails:
    retain cleanup_pending = true
```

A failed cleanup MUST NOT invalidate an otherwise successful transfer.

Subsequent cleanup MUST rediscover the cleanup-pending state through the
exact target/operation association and MUST revalidate operation and
target lock state immediately before each destructive deletion.

If the cleanup-pending state record is missing, corrupt, or ownership
cannot be established, Flux MUST NOT infer cleanup authority from
filename patterns alone and MUST NOT delete the associated artifact.

A cleanup-pending record is therefore recovery/GC metadata, not evidence
that the transfer itself failed.

------------------------------------------------------------------------

# 219. Interrupted Operation Retention

After:

``` text
SIGKILL
power loss
kernel panic
machine restart
```

Flux must retain adjacent artifacts necessary for resume.

`flux cleanup` may remove them only after the stale-operation and
ownership checks succeed.

The default policy must favor recoverability over aggressive cleanup.

------------------------------------------------------------------------

# 220. Cleanup Safety Matrix

  Artifact                               Normal GC Scope         Ownership Required   Live Lock Check
  -------------------------------------- ----------------------- -------------------- -----------------
  `.flux/operations/*`                   Managed control plane   Yes                  Yes
  `.flux/WAL/*`                          Managed control plane   Yes                  Yes
  `.flux/topology/*`                     Managed control plane   Yes                  Yes
  `target.flux-lock`                     Adjacent target scope   Yes                  Yes
  `target.flux-partial.<operation-id>`   Adjacent target scope   Yes                  Yes
  `target.flux-state.<operation-id>`     Adjacent target scope   Yes                  Yes
  Arbitrary `*.flux-*` elsewhere         Never by default        N/A                  N/A

------------------------------------------------------------------------

# 221. Cleanup Discovery

For adjacent artifacts, discovery must be bounded.

Flux may inspect:

``` text
the target's parent directory
```

for artifacts whose metadata can be associated with the target.

It must not perform filesystem-wide scans merely to find abandoned Flux
artifacts.

This preserves:

``` text
bounded cleanup time
bounded memory
safe ownership boundaries
```

------------------------------------------------------------------------

# 222. Orphan Topology and WAL Garbage Collection

The existing persistent topology/WAL garbage collection policy remains
applicable.

Orphaned operation workspaces are collectible **only when all applicable
conditions below are satisfied**:

``` text
1. the operation is durably marked COMPLETED, ABANDONED, or another
   terminal state that proves no further recovery is required;

2. no live operation lock exists;

3. no valid operation lease/ownership record identifies a live owner;

4. workspace ownership metadata is structurally valid and matches the
   operation_id represented by the workspace;

5. the workspace is not currently selected by an active resume or
   recovery operation;

6. no WAL, topology, checkpoint, manifest, or other state in the
   workspace remains necessary to reconstruct a recoverable operation;

7. any required cleanup intent has been durably recorded before
   destructive cleanup begins; and

8. ownership and lock state are revalidated immediately before
   destructive deletion.
```

The following are explicitly insufficient by themselves:

``` text
old heartbeat
expired-looking timestamp
missing heartbeat
old workspace modification time
absence of a temporary file
filename pattern matching
process identifier stored in metadata
```

A stale heartbeat is evidence for investigation, not authoritative proof
that the workspace is abandoned.

The legal collection decision is therefore:

``` text
terminal/recoverably-abandoned state
        +
no live operation ownership
        +
valid workspace ownership
        +
no active recovery selection
        +
no required recovery state
        +
durable cleanup intent where required
        +
final ownership/lock revalidation
        ↓
collectible
```

If any required condition cannot be established reliably:

``` text
DO NOT DELETE
```

GC MUST prefer retention over uncertain deletion.

Cleanup MUST use the crash-safe ordering defined by Section 223:

``` text
establish terminal/abandoned state
        ↓
persist cleanup intent
        ↓
revalidate ownership / locks
        ↓
remove derived artifacts
        ↓
remove primary workspace
```

A crash at any point MUST leave either recoverable state or a provably
completed/abandoned state; it MUST NOT silently destroy the only
evidence required for safe recovery.

------------------------------------------------------------------------

# 223. Cleanup Crash Safety

GC itself must be crash-safe.

Deletion should proceed in an order that prevents loss of the only
recovery metadata before the operation is known to be safely
unrecoverable.

Where necessary:

``` text
mark stale
    ↓
persist cleanup intent
    ↓
remove derived artifacts
    ↓
remove primary state
```

A crash during cleanup must result in either:

``` text
recoverable operation
```

or:

``` text
provably completed/abandoned operation
```

not ambiguous silent data loss.

------------------------------------------------------------------------

# 224. Canonical Retry and WAL Integration

Every canonical retry must be represented in the WAL/state model.

Conceptually:

``` text
ATTEMPT_STARTED
      ↓
checkpoint records
      ↓
ATTEMPT_FAILED
      ↓
RETRY_STARTED
      ↓
checkpoint records
      ↓
ATTEMPT_SUCCEEDED
      ↓
CANONICAL_MATERIALIZED
```

The old attempt's checkpoint ranges must not automatically be treated as
valid progress for the new attempt unless the source/destination
validation rules explicitly establish that reuse is safe.

------------------------------------------------------------------------

# 225. Retry Checkpoint Isolation

Checkpoint state is associated with:

``` text
operation_id
file_id
attempt_id
generation
```

This prevents stale progress from Attempt 1 being accidentally
interpreted as progress from Attempt 2.

A retry may reuse verified destination data only when the resume
validation policy explicitly permits it.

Otherwise, the retry starts from a safe verified boundary.

------------------------------------------------------------------------

# 226. Finalized Hardlink Correctness Invariants

The following are normative:

``` text
1. Canonical path selection is deterministic and immutable.
2. A failed execution attempt is never mutated into a successful attempt.
3. Every retry creates a new execution attempt.
4. Retry never promotes another hardlink member to canonical status.
5. Dependents remain blocked until a canonical attempt materializes.
6. Canonical attempt history survives restart according to retention policy.
7. Coalescing never destroys information required by the active resume policy.
8. Strict chunk verification retains every constituent chunk digest.
9. Adjacent artifact GC is target-scoped, not filesystem-wide.
10. Filename matching alone can never authorize artifact deletion.
11. GC requires ownership validation and live-lock revalidation.
12. Interrupted operations retain resume artifacts until safely abandoned.
13. Retry checkpoints are isolated by attempt identity/generation.
14. Cleanup cannot delete the only recovery state before abandonment is established.
```

------------------------------------------------------------------------

# 227. Final V6 Acceptance Tests

The implementation must add:

``` text
[ ] canonical failure followed by retry
[ ] multiple failed attempts followed by success
[ ] retry exhaustion
[ ] retry never changes canonical path
[ ] dependents remain blocked during retry
[ ] restart during retry
[ ] stale attempt cannot satisfy new attempt
[ ] coalesced WAL record retains all chunk digests
[ ] strict chunk verification after WAL compaction
[ ] checkpoint compaction preserves verification semantics
[ ] adjacent artifact ownership validation
[ ] cleanup does not delete foreign .flux-* files
[ ] cleanup refuses ambiguous artifact ownership
[ ] cleanup checks live target lock
[ ] cleanup after SIGKILL
[ ] cleanup after power-loss simulation
[ ] adjacent artifact discovery remains target-directory scoped
[ ] orphan topology cleanup
[ ] orphan WAL cleanup
[ ] crash during cleanup
[ ] retry + WAL recovery
[ ] retry + checkpoint replay
[ ] retry + atomic publication recovery
```

------------------------------------------------------------------------

# 228. Final Flux Specification Status --- V6

With Sections 200--227 incorporated, the Flux specification explicitly
separates:

``` text
logical filesystem topology
execution attempts
durable checkpoint history
WAL transport/storage optimization
persistent operation state
garbage-collection ownership
```

This resolves the canonical retry contradiction while preserving
deterministic hardlink topology.

The final conceptual model is:

``` text
                Logical Topology
                       │
                 canonical path
                       │
                       ▼
                ┌──────────────┐
                │ TopologyState│
                └──────┬───────┘
                       │
                 execution attempt
                       │
          ┌────────────┴────────────┐
          ▼                         ▼
      Attempt 1                 Attempt 2
      Failed                    Running
                                    │
                                    ▼
                               Materialized
                                    │
                                    ▼
                              release links
```

Checkpoint coalescing remains an implementation optimization:

``` text
many logical chunk checkpoints
             ↓
      coalesced WAL record
             ↓
   all required digests retained
```

And garbage collection remains ownership-scoped:

``` text
managed .flux workspace
        +
validated adjacent target artifacts
        +
live-lock protection
```

The governing principle is:

> **Retries create new execution attempts; coalescing changes
> representation, never verification semantics; garbage collection
> deletes only artifacts Flux can prove it owns and no live operation
> can require.**

------------------------------------------------------------------------

# 229. Temporal Lease Recovery Across Reboots

This section supersedes any interpretation that a persisted monotonic
timestamp may be used to calculate lease age after a machine reboot.

## 229.1 Clock Domains

Flux distinguishes:

``` text
monotonic clock
    → valid only within the current boot/session

wall clock
    → persisted observation anchor

boot/session identifier
    → identifies the clock epoch
```

A persisted monotonic timestamp must never be subtracted from a
monotonic timestamp obtained after a reboot.

## 229.2 Lock Record

A persistent operation/target lock record should contain:

``` rust
struct LeaseRecord {
    operation_id: OperationId,
    owner_instance_id: InstanceId,
    boot_session_id: BootSessionId,

    last_heartbeat_wall: Timestamp,

    // Diagnostic/current-session data only.
    last_heartbeat_monotonic: Option<Duration>,
}
```

The monotonic value is never treated as portable across boots.

## 229.3 Normal Runtime Lease Validation

While the owning process remains in the same boot/session:

``` text
current_monotonic - last_heartbeat_monotonic
```

is the preferred lease-duration measurement.

Wall-clock timestamps are retained for diagnostics and recovery.

## 229.4 Post-Reboot Lease Validation

After reboot, Flux must use:

``` text
wall-clock age
+
boot/session mismatch
+
OS/process ownership validation
+
lock acquisition semantics
```

rather than a persisted monotonic duration.

## 229.5 Clock Rollback

If the wall clock has moved substantially backward, Flux must not
conclude that an operation is stale solely from wall-clock age.

The safe response is:

``` text
LEASE_AGE_UNCERTAIN
```

and require explicit recovery/cleanup or another authoritative ownership
determination.

Flux must prefer leaving an apparently stale operation untouched over
stealing a lock that might still belong to a live owner.

## 229.6 Boot Identity

The platform layer should expose a boot/session identifier where
reliably available.

If unavailable, Flux must use the strongest available combination of:

``` text
process ownership
lock primitive
wall-clock observation
operation instance identity
```

and must not fabricate continuity for a monotonic clock.

------------------------------------------------------------------------

# 230. Hardlink Identity Across Source Filesystems

Filesystem identity is a mandatory component of hardlink identity.

## 230.1 No Cross-Filesystem Hardlink Merging

If two source objects have:

``` text
different filesystem identity
```

they are different hardlink groups even if:

``` text
same size
same content hash
same relative metadata
```

Example:

``` text
FS-A /tree/A/file → object X
FS-B /tree/B/file → object Y
```

produces:

``` text
FileIdentity(FS-A, X)
FileIdentity(FS-B, Y)
```

and these must never be merged.

## 230.2 Destination Convergence

If independent source hardlink groups map into the same destination
filesystem, Flux preserves each source group's internal relationships
independently.

Example:

``` text
Source FS-A:
    A/file
    A/link → hardlink

Source FS-B:
    B/file
    B/link → hardlink
```

Destination result:

``` text
A/file == A/link
B/file == B/link
```

but:

``` text
A/file != B/file
```

as hardlink relationships.

## 230.3 Content Equality Is Not Identity

Cryptographic equality is not a substitute for filesystem object
identity.

Flux must never infer a hardlink relationship from:

``` text
SHA-256 equality
```

alone.

------------------------------------------------------------------------

# 231. Emergency Control-Space Reservation

The persistence subsystem must remain capable of recording a minimal
safe failure/pause state when the normal data volume reaches capacity.

## 231.1 Control Reservation

Before substantial transfer begins, Flux should establish a small
reserved control-space budget for:

``` text
pause/failure state
minimal WAL record
operation manifest update
recovery marker
```

The reservation is separate from ordinary copy-capacity accounting.

## 231.2 Preallocation

Where supported, Flux should preallocate the emergency journal/control
file.

The reservation must be physically established before ordinary transfer
consumes the available space.

## 231.3 Emergency Path

If the normal WAL encounters:

``` text
ENOSPC
```

the engine enters the emergency persistence path:

``` text
normal WAL
    ↓
ENOSPC
    ↓
emergency control journal
    ↓
minimal PAUSED/FAILED state
```

The emergency path must not depend on discovering additional free space.

## 231.4 Zero-Free-Space Invariant

Flux must never assume that:

``` text
free space == 0
```

can still be repaired by writing an arbitrary new database transaction.

The emergency representation must have been reserved/preallocated in
advance.

## 231.5 Emergency State Failure

If the emergency reserve is unavailable, exhausted, or corrupted:

``` text
CONTROL_STATE_DURABILITY_FAILURE
```

must be reported.

Flux must not claim that the operation is durably paused when it cannot
persist that fact.

------------------------------------------------------------------------

# 232. Persistent Hardlink Hold Index

The complete dependent hardlink set is persistent from the moment it is
discovered.

There is no RAM threshold at which an initially in-memory hold queue
suddenly spills to disk.

## 232.1 Persistent-First Design

The authoritative structure is:

``` text
topology.db
    canonical identity
    canonical state
    dependent paths
    dependent action state
```

RAM contains only bounded scheduler metadata.

## 232.2 Canonical In-Flight Example

For a million-member group:

``` text
topology.db
    1 canonical
    999,999 dependents

RAM
    bounded ready/wakeup metadata
```

The scheduler must not allocate 999,999 waiting action objects.

## 232.3 Materialization Release

When the canonical materializes:

``` text
CanonicalMaterialized(identity)
        ↓
persistent dependent lookup
        ↓
bounded release batch
        ↓
ready queue
```

The release may continue over multiple scheduler iterations.

## 232.4 No Scanner Backpressure from Held Dependents

A large unresolved hardlink group must not cause the scanner to retain
every dependent in RAM.

The persistent topology index absorbs this pressure.

------------------------------------------------------------------------

# 233. Unsupported Special Files

Flux 0.1/0.2 does not implement full copying of:

``` text
FIFO
Unix domain socket
block device
character device
other unsupported special filesystem objects
```

## 233.1 Default Behavior

Default behavior is:

``` text
SKIP + DURABLE WARNING
```

The operation may otherwise complete successfully.

The result must explicitly report:

``` text
SPECIAL_FILE_UNSUPPORTED
```

with:

``` text
relative_path
object_type
action=skipped
```

## 233.2 Strict Behavior

A strict option may convert unsupported special files into a hard
operation failure.

Conceptually:

``` text
--special-files=strict
```

produces:

``` text
SPECIAL_FILE_UNSUPPORTED
```

as an operation failure.

## 233.3 No Silent Skipping

Unsupported special files must never disappear from the operation
summary without an explicit record.

------------------------------------------------------------------------

# 234. Adjacent Artifact Discovery and Cleanup

Single-file artifacts intentionally reside beside the target:

``` text
target
target.flux-lock
target.flux-partial.<operation-id>
target.flux-state.<operation-id>
```

They are not part of the centralized `.flux/operations/` tree.

## 234.1 Target-Scoped Discovery

Flux may inspect the target's parent directory when performing explicit
cleanup for a known target.

Example:

``` text
flux cleanup --target /path/to/target
```

The cleanup process may identify candidate Flux artifacts in:

``` text
/path/to/
```

but must validate their embedded ownership metadata.

## 234.2 No Filesystem-Wide Search

Default cleanup must not recursively search an arbitrary destination
filesystem for:

``` text
*.flux-lock
*.flux-partial.*
*.flux-state.*
```

Filename matching alone is never sufficient for deletion.

## 234.3 Unknown Target

If the original target path is unknown, Flux must not perform broad
destructive discovery merely to find possible artifacts.

An operator may provide a target path explicitly.

## 234.4 Artifact Validation

Before deleting an adjacent artifact, GC must validate:

``` text
Flux artifact format
operation_id
target identity
artifact type
format version
ownership metadata
```

and confirm that the associated operation is safely stale/finished.

------------------------------------------------------------------------

# 235. Remote Filesystem Lock Capability

Flux must not assume that local locking semantics automatically apply to
SMB, NFS, or other remote filesystems.

## 235.1 Capability Classes

The filesystem adapter exposes:

``` rust
enum LockCapability {
    LocalStrong,
    RemoteStrong,
    RemoteUnverified,
    Unsupported,
}
```

## 235.2 Required Strong-Lock Properties

A lock mechanism used for safe concurrent destination mutation must
establish:

``` text
atomic acquisition
exclusive ownership
owner identity
release
stale-owner recovery
```

## 235.3 O_EXCL

`O_CREAT | O_EXCL` may be used where the filesystem contract establishes
reliable atomic exclusive creation.

Flux must not universally declare `O_EXCL` sufficient for all SMB/NFS
configurations.

A successful syscall alone is not proof that the remote deployment
provides the required crash/ownership semantics.

## 235.4 Remote Uncertainty

If the platform cannot establish a trustworthy exclusive lock contract:

``` text
REMOTE_LOCK_UNSAFE
```

must be returned for operations requiring concurrent-safety guarantees.

Flux must prefer refusing the operation over risking two writers
corrupting the same destination.

## 235.5 Lock Revalidation

Even after acquisition, commit operations must revalidate ownership
immediately before:

``` text
write
rename
unlink
metadata publication
```

where required by the operation protocol.

------------------------------------------------------------------------

# 236. Final V7 Persistence and Safety Invariants

The following are normative:

``` text
1. Persisted monotonic timestamps are never used across reboots.
2. Post-reboot lease recovery uses conservative wall-clock/ownership validation.
3. Clock rollback cannot cause unsafe stale-lock acquisition.
4. Filesystems are part of hardlink identity.
5. Cross-filesystem source hardlink groups are never merged.
6. Content equality never creates hardlink identity.
7. Emergency control space is reserved before normal transfer.
8. ENOSPC must have a pre-established minimal recovery path.
9. Failure to persist the emergency state is explicitly reported.
10. The complete hardlink hold index is persistent from discovery.
11. RAM contains only bounded waiting/release metadata.
12. Special files are explicitly skipped and logged by default.
13. Strict special-file policy may convert unsupported objects into failure.
14. Adjacent artifact GC is target-directory scoped.
15. Arbitrary filesystem-wide .flux-* deletion is prohibited.
16. Adjacent artifact deletion requires positive ownership validation.
17. Remote locks require a demonstrated capability contract.
18. O_EXCL alone is not a universal remote-lock guarantee.
19. Unsafe remote locking causes refusal, not silent degradation.
```

------------------------------------------------------------------------

# 237. Final V7 Acceptance Tests

The implementation must add:

``` text
[ ] monotonic lease within one boot
[ ] reboot with stale operation
[ ] reboot with potentially live operation
[ ] wall-clock rollback during recovery
[ ] unavailable boot-session identifier
[ ] cross-mount hardlink groups
[ ] identical content across different filesystems remains separate
[ ] destination convergence of independent hardlink groups
[ ] emergency control reservation
[ ] WAL ENOSPC transition
[ ] zero-normal-space emergency pause
[ ] emergency-state durability failure
[ ] million-member persistent hardlink hold group
[ ] bounded RAM while canonical runs for hours
[ ] batched dependent release
[ ] FIFO default skip
[ ] socket default skip
[ ] device-node default skip
[ ] strict special-file failure
[ ] target-scoped adjacent artifact cleanup
[ ] foreign .flux-* artifact preserved
[ ] ambiguous artifact ownership preserved
[ ] stale adjacent artifact removed safely
[ ] remote lock capability detection
[ ] unsafe SMB/NFS locking refusal
[ ] lock ownership revalidation before commit
```

------------------------------------------------------------------------

# 238. Final Flux Specification Status --- V7

With Sections 229--237 incorporated, Flux explicitly defines:

``` text
clock-domain boundaries
reboot-safe lease recovery
cross-filesystem hardlink semantics
emergency ENOSPC persistence
persistent-first hardlink holding
unsupported special-file behavior
adjacent artifact discovery
remote filesystem locking capability
```

The resulting architectural boundaries are:

``` text
                    FLUX SAFETY MODEL

     ┌─────────────────────────────────────────┐
     │          Filesystem Identity            │
     │  source FS + object ID = hardlink group │
     └───────────────────┬─────────────────────┘
                         │
                         ▼
                Persistent Topology
                         │
             ┌───────────┴───────────┐
             │                       │
         canonical              dependents
             │                       │
             ▼                       │
       execution attempts            │
             │                       │
             ▼                       │
        durable WAL                 │
             │                       │
             └───────────┬───────────┘
                         ▼
                  bounded scheduler
```

Persistence failure is handled independently:

``` text
normal WAL
    │
    ├── capacity available → normal operation
    │
    └── ENOSPC
          ↓
    emergency reserved space
          ↓
    PAUSED / FAILED
```

Lease recovery is explicitly split by clock epoch:

``` text
same boot:
    monotonic duration

after reboot:
    wall-clock observation
    +
ownership validation
    +
conservative recovery
```

Cleanup remains ownership-scoped:

``` text
central .flux workspace
        +
known target parent directory
        +
validated artifact metadata
        +
live-lock validation
```

The governing V7 principles are:

> **A clock measurement is only valid within its clock epoch.**

> **Filesystem identity, not content equality, defines hardlink
> identity.**

> **Crash recovery must have a reserved path even when ordinary storage
> is exhausted.**

> **The complete waiting topology belongs on disk; RAM is only a bounded
> execution cache.**

> **Unsupported filesystem objects are never silently lost.**

> **Remote locking must be proven safe by capability, not assumed safe
> from syscall success.**

------------------------------------------------------------------------

# 239. Adjacent Single-File Artifact Lifecycle

Single-file transfers may place recovery artifacts adjacent to the
destination target.

For:

``` text
/dest/foo.iso
```

Flux may create:

``` text
/dest/foo.iso.flux-lock
/dest/foo.iso.flux-partial.<operation-id>
/dest/foo.iso.flux-state.<operation-id>
```

The `.flux-state` record is the authoritative association between the
target and the operation.

It must contain sufficient metadata to establish:

``` text
operation_id
target_identity
source_identity
artifact_generation
owner_instance_id
boot_session_id
creation_wall_time
last_heartbeat_wall_time
format_version
```

Partial and lock artifacts must reference the same operation identity,
directly or through a verifiable state record.

## 239.1 Normal Lifecycle

``` text
acquire target lock
       ↓
create durable state
       ↓
create partial destination
       ↓
copy/checkpoint
       ↓
verify
       ↓
durably publish target
       ↓
remove partial/state/lock
```

Failure during cleanup does not invalidate an otherwise successful
transfer.

## 239.2 Crash Lifecycle

After:

``` text
SIGKILL
power loss
kernel panic
machine restart
```

adjacent artifacts must remain available for recovery.

A subsequent invocation must:

``` text
discover target-scoped artifacts
        ↓
validate metadata
        ↓
validate target identity
        ↓
evaluate lock ownership
        ↓
resume / recover / report conflict
```

It must never infer ownership solely from filenames.

## 239.3 Missing State

If a partial or lock artifact exists but the associated state record is
missing, malformed, or unverifiable:

``` text
ARTIFACT_OWNERSHIP_UNCERTAIN
```

must be reported.

The artifact must not be automatically deleted or adopted.

------------------------------------------------------------------------

# 240. Adjacent Lock Staleness and Liveness

Heartbeat age alone does not establish that a process is dead.

A stale-looking adjacent lock is therefore a **candidate stale lock**,
not automatically an abandoned lock.

## 240.1 Recovery Decision Order

When a new invocation encounters an existing adjacent lock:

``` text
1. inspect lock metadata
2. identify owner instance
3. consult native lock/ownership mechanism
4. determine whether owner is demonstrably alive
5. use lease age as a secondary recovery signal
6. recover only when abandonment is sufficiently established
```

## 240.2 Live Owner

If the native locking mechanism or platform ownership information
demonstrates that the owner is alive:

``` text
TARGET_LOCK_BUSY
```

must be returned.

Flux must not steal the lock because the heartbeat happens to be old.

## 240.3 Demonstrably Abandoned Owner

If the platform can establish that the lock owner no longer exists and
the lock has been released/invalidated according to the platform's
semantics, recovery may proceed.

## 240.4 Uncertain Ownership

If Flux cannot distinguish:

``` text
dead owner
```

from:

``` text
alive but stalled owner
```

then:

``` text
TARGET_LOCK_UNCERTAIN
```

must be reported.

The safe default is to preserve the artifacts.

Explicit operator-directed recovery may override this condition after
suitable confirmation.

------------------------------------------------------------------------

# 241. FluxPathKey and Unicode Normalization

Path identity and filesystem object identity are separate concepts.

`FluxPathKey` identifies a namespace path. It must not use Unicode
normalization as an identity operation.

## 241.1 Unix

On Unix-like systems:

``` text
each FluxPathKey component = exact raw filename byte sequence
components joined by 0x00 (Section 103)
```

No UTF-8 lossy conversion is permitted for identity.

No NFC/NFD normalization is applied.

## 241.2 Windows

Windows paths are represented internally using a canonical lossless
encoding suitable for deterministic cross-platform serialization.

The specification uses:

``` text
canonical WTF-8 byte representation of each component,
components joined by 0x00 (Section 103)
```

for `FluxPathKey` serialization and deterministic ordering.

The representation must preserve distinctions that matter to the Windows
namespace.

## 241.3 No Unicode Normalization for Identity

Flux must not normalize:

``` text
NFC → NFD
NFD → NFC
```

when constructing identity keys.

For example, visually equivalent names represented by different Unicode
sequences must not be merged merely because they normalize to the same
Unicode form.

## 241.4 Destination Normalization

A destination filesystem may itself normalize names.

Therefore:

``` text
source FluxPathKey
        ↓
destination native path
        ↓
destination filesystem namespace
```

is a mapping, not a promise that source pathname bytes survive
physically unchanged.

After creation, Flux must validate the destination object using
destination filesystem identity and namespace lookup rather than
assuming byte-for-byte pathname preservation.

## 241.5 Collision Detection

If two distinct source `FluxPathKey` values map to the same destination
namespace object or otherwise collide under destination filesystem
semantics, the operation must not silently overwrite one with the other.

It must report:

``` text
DESTINATION_NAMESPACE_COLLISION
```

unless an explicit, deterministic collision policy has been selected.

## 241.6 Hardlink Independence

Unicode normalization never determines hardlink identity.

Hardlink topology continues to use:

``` text
FileIdentity
```

rather than path spelling.

Thus Unicode normalization differences cannot merge or split a hardlink
group incorrectly.

------------------------------------------------------------------------

# 242. Extended FileIdentity with Object Generation

The persistent identity model is extended to include an object
generation where the platform/filesystem provides a reliable generation
value.

``` rust
struct FileIdentity {
    filesystem_id: FilesystemId,
    object_id: ObjectId,
    generation: Option<ObjectGeneration>,
}
```

## 242.1 Generation Semantics

`generation` identifies a particular incarnation of an object when the
filesystem exposes such a concept.

A changed generation means:

``` text
old object incarnation != new object incarnation
```

even when:

``` text
filesystem_id == same
object_id == same
```

## 242.2 Filesystems Without Generation

If no reliable generation is available, Flux must not pretend that an
object ID is eternally stable.

The implementation must use conservative revalidation based on the
strongest available filesystem observations.

Possible observations include:

``` text
object type
size
mtime
ctime where available
birth/creation time where available
filesystem identity
object ID
```

These observations are corroborating evidence, not a mathematically
perfect substitute for an object generation.

------------------------------------------------------------------------

# 243. Identity Observation and Revalidation

When the scanner discovers a hardlink candidate, it records an identity
observation.

Conceptually:

``` rust
struct IdentityObservation {
    file_identity: FileIdentity,
    file_type: FileType,
    size: u64,
    timestamps: TimestampSet,
}
```

Before an object is committed to an existing hardlink topology group,
Flux must revalidate the identity.

## 243.1 Identity Change

If any authoritative identity component changes:

``` text
filesystem_id
object_id
generation
```

the previous topology association is invalid.

The scanner must treat the newly observed object as a new identity.

## 243.2 Object Disappearance and Recreation

If:

``` text
object X
```

is deleted and a later scan observes:

``` text
object Y
```

with the same numeric object ID but a different generation, the objects
are distinct.

If no generation exists, Flux must apply the conservative fallback
policy rather than assuming continuity.

## 243.3 TopologyStore Cache Invalidation

A cached `FileIdentity → topology group` association must be invalidated
when authoritative identity changes.

The cache must never survive an identity-generation change.

## 243.4 No Path-Only Recovery

A path that previously belonged to a hardlink group does not prove that
a newly observed object at the same path belongs to that group.

Path identity and object identity must be independently revalidated.

------------------------------------------------------------------------

# 244. Inode-Recycling Protection

Flux must explicitly protect against inode/object-ID recycling in
high-churn filesystems.

The following is prohibited:

``` text
old object:
    FS=1, inode=500

deleted

new object:
    FS=1, inode=500

→ automatically reuse old topology record
```

Instead:

``` text
old identity
     ↓
object disappears
     ↓
identity becomes inactive
     ↓
new observation
     ↓
generation/revalidation
     ↓
new topology identity if continuity cannot be proven
```

Topology records must not be resurrected solely because numeric object
IDs happen to match.

------------------------------------------------------------------------

# 245. Cross-Platform Path Mapping Invariants

The following are normative:

``` text
1. FluxPathKey is a namespace identity, not a display string.
2. Unix FluxPathKey preserves the raw filename bytes of every component.
3. Windows FluxPathKey uses lossless canonical WTF-8 serialization of every component.
   In both cases components are joined by 0x00, so bytewise key order is component-wise order.
4. Unicode normalization is never used as a hardlink identity operation.
5. Destination filesystem normalization is treated as a namespace mapping.
6. Destination namespace collisions are detected explicitly.
7. FileIdentity is independent from FluxPathKey.
8. Hardlink identity is based on filesystem/object identity, not pathname spelling.
9. Object generation is used where reliably available.
10. Inode reuse cannot automatically resurrect an old topology association.
11. Cached topology identity is invalidated after authoritative identity changes.
12. Missing generation information triggers conservative revalidation.
```

------------------------------------------------------------------------

# 246. Adjacent Artifact GC Invariants

The following are normative:

``` text
1. Adjacent artifacts are target-scoped.
2. Cleanup must not perform arbitrary filesystem-wide filename searches.
3. Filename pattern matching alone cannot authorize deletion.
4. Artifact ownership must be positively validated.
5. Target identity must match recorded artifact identity.
6. Live native lock ownership takes precedence over heartbeat age.
7. Heartbeat age identifies a stale candidate, not definitive process death.
8. Uncertain ownership causes preservation by default.
9. Explicit target-scoped cleanup may handle orphaned adjacent artifacts.
10. Cleanup must not delete artifacts needed for an active or recoverable operation.
```

------------------------------------------------------------------------

# 247. V8 Acceptance Tests

The implementation must add:

``` text
[ ] adjacent state/partial/lock creation
[ ] crash with all adjacent artifacts present
[ ] crash with state present but partial missing
[ ] crash with partial present but state missing
[ ] malformed adjacent state
[ ] foreign similarly named artifact
[ ] live stalled owner with old heartbeat
[ ] dead owner with abandoned lock
[ ] uncertain remote lock ownership
[ ] explicit target-scoped orphan cleanup
[ ] NFC source filename to NFD-normalizing destination
[ ] NFD source filename to NFC-preserving destination
[ ] distinct normalization forms in the same source namespace
[ ] destination normalization collision
[ ] hardlink group spanning normalization-sensitive names
[ ] inode reuse with generation change
[ ] inode reuse without generation support
[ ] object deletion/recreation during scan
[ ] topology cache invalidation after identity change
[ ] path reused for a different object
[ ] restart recovery after identity churn
```

------------------------------------------------------------------------

# 248. Final Flux Specification Status --- V8

Flux now explicitly separates four identities:

``` text
Path Identity
    = FluxPathKey

Object Identity
    = FileIdentity

Execution Identity
    = OperationId + AttemptId

Artifact Identity
    = OperationId + target identity + artifact generation
```

These identities must never be substituted for one another.

The resulting recovery model is:

``` text
                 TARGET
                   │
                   ▼
             FluxPathKey
                   │
                   ▼
             FileIdentity
             FS + object
             + generation
                   │
                   ▼
             TopologyStore
                   │
                   ▼
          execution attempt
                   │
          ┌────────┴────────┐
          ▼                 ▼
       WAL/state       adjacent artifacts
                            │
                            ▼
                     ownership validation
                            │
                     ┌──────┴──────┐
                     ▼             ▼
                  recover       preserve
```

The governing V8 principles are:

> **A path is not an object, an object is not an execution, and an
> execution artifact is not ownership merely because its filename looks
> familiar.**

> **Unicode normalization is a filesystem namespace behavior, not a Flux
> identity transformation.**

> **An inode number is an observation, not proof of object continuity.**

> **A heartbeat timeout identifies a recovery candidate; authoritative
> lock ownership determines whether recovery is safe.**

> **When identity or ownership cannot be established, Flux preserves
> state rather than guessing.**

------------------------------------------------------------------------

# 249. Adjacent Single-File Artifacts --- Complete Lifecycle

Single-file transfers may place recovery artifacts adjacent to the
destination target. These files are intentionally outside the
centralized directory-operation workspace. They are **independently
self-describing recovery artifacts** and must contain sufficient
metadata to identify their owning operation, target, artifact type,
creation epoch, and ownership state.

For:

``` text
/dest/foo.iso
```

Flux may create:

``` text
/dest/foo.iso.flux-lock
/dest/foo.iso.flux-state.<operation-id>
/dest/foo.iso.flux-partial.<operation-id>
```

The adjacent state record is the authoritative local recovery record for
that target.

## 249.1 Required Artifact Metadata

Each adjacent state record must contain, directly or through a
verifiable authenticated/structured reference:

``` text
format_version
operation_id
attempt_id
target_identity
target_path_key
source_identity
artifact_generation
owner_instance_id
boot_session_id
creation_wall_time
last_heartbeat_wall_time
operation_state
```

The lock and partial artifacts must be associated with the same
operation identity.

Filename patterns alone are never sufficient to establish ownership.

## 249.2 Normal Lifecycle

``` text
acquire target lock
       ↓
create durable state
       ↓
register standalone operation
       ↓
create partial destination
       ↓
copy/checkpoint
       ↓
verify
       ↓
durably publish target
       ↓
mark operation complete
       ↓
remove partial/state/lock
       ↓
remove standalone catalog entry
```

If cleanup fails after successful publication, the transfer remains
successful. The residual artifacts become cleanup candidates.

## 249.3 Crash Lifecycle

After:

``` text
SIGKILL
power loss
kernel panic
machine restart
```

adjacent artifacts must remain available for recovery.

A subsequent Flux invocation must:

``` text
discover registered target
        ↓
inspect adjacent artifacts
        ↓
validate metadata
        ↓
validate target identity
        ↓
evaluate lock ownership
        ↓
resume / recover / report conflict
```

## 249.4 Missing or Corrupt State

If an adjacent artifact exists but its associated state is:

``` text
missing
malformed
corrupt
unreadable
identity-inconsistent
```

Flux must report:

``` text
ARTIFACT_OWNERSHIP_UNCERTAIN
```

and preserve the artifact by default.

Flux must never silently adopt or delete an artifact solely because its
filename resembles a Flux artifact.

------------------------------------------------------------------------

# 250. Standalone Operation Catalog

Because standalone artifacts are intentionally adjacent to arbitrary
target paths, ordinary `flux cleanup` must not recursively scan the
entire destination filesystem.

Flux therefore maintains a persistent **standalone operation catalog**
inside the destination control plane.

For a single-file target `T`, let `P` be its parent directory and `K` the
stable target key of Section 259.6 (physical identity of `P` where
reliably available, plus the canonical `FluxPathKey` of `T` relative to
`P`). The catalog record for `T` is:

``` text
P/.flux/
    standalone/
        <sha256(K)>.record
```

The catalog therefore lives in the target's own parent directory; there
is no user-wide or system-wide catalog (Section 18).

A known target is looked up directly by computing `K`, with no directory
listing. The record stores the complete key `K`; a stored key that
differs from the looked-up key is a collision, handled as in Section
259.6, and the targets must never be aliased. If the record is missing,
for example after a crash between steps 2 and 3 of Section 250.2, Flux
may list `P` non-recursively for that target's adjacent artifacts
(Section 251.2) to reconstruct it.

A pre-existing `P/.flux` that Flux does not own is handled as in Section
259.3: it is never overwritten or reinterpreted.

The catalog is an index, not the authoritative artifact state. Losing a
catalog record never removes cleanup authority, which remains with the
adjacent state record (Section 218).

## 250.1 Catalog Record

A catalog record contains:

``` text
complete_key
target_identity
target_path_key
target_parent_identity
operation_id
attempt_id
artifact names
artifact generation
last known lease
catalog record generation
```

The record must be crash-safe.

## 250.2 Registration

After acquiring the target lock and establishing the adjacent state, the
operation registers itself in the catalog before substantial transfer
begins.

Conceptually:

``` text
1. acquire target lock
2. create durable adjacent state
3. durably register target in standalone catalog
4. create/update partial
5. begin transfer
```

The exact low-level ordering may use a transaction/WAL, but after
recovery Flux must have either:

``` text
complete registration
```

or enough durable local state to reconstruct it safely.

## 250.3 Completion

Successful completion follows:

``` text
1. durably publish destination
2. durably mark operation complete
3. remove adjacent partial/state/lock artifacts
4. remove standalone catalog record
5. remove P/.flux/standalone/ and P/.flux/ if Flux created them and
   they are now empty
```

If the process dies between these steps, the catalog and/or adjacent
state provide recovery information.

Step 5 never removes a directory that still contains any entry, or one
that Flux did not create.

------------------------------------------------------------------------

# 251. `flux cleanup` Discovery Rules

Default cleanup must use the persistent standalone catalog.

It must not perform a filesystem-wide search for:

``` text
*.flux-lock
*.flux-state.*
*.flux-partial.*
```

## 251.1 Default Cleanup

``` text
flux cleanup DEST
```

performs, without recursing into subdirectories of `DEST`:

``` text
DEST/.flux/operations/   directory operations whose destination root is DEST
        +
DEST/.flux/standalone/   single-file operations whose target's parent is DEST
```

The `DEST` argument is required unless `--target PATH` is given
(Section 251.2). A bare `flux cleanup` with neither is a usage error,
because there is no global catalog to enumerate (Section 18).

For each standalone catalog entry:

``` text
locate target parent
        ↓
inspect expected adjacent artifacts
        ↓
validate artifact metadata
        ↓
validate target identity
        ↓
validate lock/lease ownership
        ↓
classify
```

Classification:

``` text
LIVE
RESUMABLE
STALE
COMPLETED_BUT_UNCLEAN
UNCERTAIN
CORRUPT
```

## 251.2 Explicit Target Cleanup

An explicit target-scoped command:

``` text
flux cleanup --target /dest/foo.iso
```

may directly inspect:

``` text
/dest/foo.iso.flux-lock
/dest/foo.iso.flux-state.*
/dest/foo.iso.flux-partial.*
```

even if the standalone catalog entry is missing.

The artifacts must still pass ownership and target-identity validation
before deletion.

## 251.3 No Filesystem-Wide Artifact Hunt

The following is prohibited as the default discovery mechanism:

``` text
recursive filesystem traversal looking for *.flux-*
```

This is necessary because:

``` text
filesystem size
+
arbitrary user files
+
network mounts
+
permission boundaries
```

make such discovery unsafe, unbounded, and contrary to Flux's
bounded-memory architecture.

## 251.4 Missing Catalog Entry

If an operator knows the target path but the catalog record is missing:

``` text
flux cleanup --target PATH
```

is the recovery mechanism.

If neither the catalog nor target path is available, Flux must not
perform destructive global artifact discovery.

------------------------------------------------------------------------

# 252. Adjacent Lock Staleness and Ownership

Heartbeat age alone cannot prove that a process is dead.

A lock with an old heartbeat is therefore a:

``` text
STALE_CANDIDATE
```

not automatically an abandoned lock.

## 252.1 Recovery Decision Order

When a new Flux process encounters an existing adjacent lock:

``` text
1. parse lock metadata
2. identify owner instance
3. inspect native lock/ownership state
4. determine whether owner is demonstrably alive
5. use lease age as a secondary recovery signal
6. recover only when abandonment is sufficiently established
```

## 252.2 Live Owner

If native locking or platform ownership information establishes that the
owner remains alive:

``` text
TARGET_LOCK_BUSY
```

must be returned.

Flux must not steal the lock because the heartbeat is old.

## 252.3 Demonstrably Abandoned Owner

If the platform establishes that the owner no longer exists and the
locking primitive has released/invalidated ownership according to its
contract, recovery may proceed.

## 252.4 Uncertain Ownership

If Flux cannot distinguish:

``` text
dead owner
```

from:

``` text
alive but stalled owner
```

the result is:

``` text
TARGET_LOCK_UNCERTAIN
```

The safe default is preservation.

Explicit operator-directed recovery may override this condition.

------------------------------------------------------------------------

# 253. Hardlink Canonical Failure and Fallback Materialization

A canonical member failing must not automatically make every other
member of a hardlink group impossible to copy.

The topology group remains deterministic, but **canonical execution** is
separated from **canonical ordering**.

## 253.1 Canonical Ordering

The deterministic canonical member remains:

``` text
lexicographically smallest eligible FluxPathKey
```

The topology record must not mutate its canonical ordering merely
because an execution attempt fails.

## 253.2 Materialization Candidates

The topology group maintains a deterministic candidate sequence:

``` text
A/file
B/file
C/file
...
```

The candidate sequence is the group's selected members in `FluxPathKey`
order. Because the scanner emits in that order (Section 7.2), it is also
discovery order: the next candidate is the next selected member of the
group that the scanner has discovered and that has not been attempted.

If:

``` text
A/file → attempt Failed
```

Flux may attempt:

``` text
B/file
```

as the next materialization candidate, subject to all of:

``` text
1. the failure is path_scoped (Section 207); an object_scoped failure
   makes the group terminally Failed without fallback
2. the shared per-group attempt budget is not exhausted (Section 206);
   a path_scoped failure that is not retryable moves straight to the
   next candidate and spends one attempt
3. a next candidate exists
```

If the failure permits fallback but no next candidate has been
discovered yet, the record stays `Copying` with its failed current
attempt, dependents stay held, and the next member the scanner
discovers for this identity becomes the candidate.

This is not a change to the group's identity or canonical ordering.

It is a new:

``` text
MaterializationAttempt
```

for the same topology group.

## 253.3 Candidate State

Conceptually:

``` text
TopologyGroup
    canonical_path = A/file

    candidates:
        A/file → Failed
        B/file → Copying
        C/file → Pending

    group materialization:
        Unresolved / Materializing / Materialized / Failed
```

In the authoritative schema (Section 90), "Materializing" is
`TopologyState::Copying`, `candidate_path` names the candidate currently
being attempted, and per-candidate outcomes are the immutable execution
attempts of Section 202. A candidate shown as `Failed` here is a failed
attempt history, not the terminal `TopologyState::Failed`.

## 253.4 Successful Fallback

If `B/file` successfully materializes the object:

``` text
A/file → FAILED ATTEMPT
B/file → MATERIALIZED OBJECT
C/file → HARDLINK TO MATERIALIZED OBJECT
```

Every failed candidate, including the canonical `A/file`, then becomes an
ordinary dependent link action to the materialization anchor:

``` text
A/file → HARDLINK TO MATERIALIZED OBJECT (reported with its earlier
         failed attempts)
```

This creates nothing that was not observed: `A/file` was established as
a member of the same source object during the scan, and fallback is only
permitted after a `path_scoped` failure, so the object's content was not
the problem. If the link action fails too, for example because the
destination path of `A/file` is itself the cause, `A/file` is reported
failed with both its attempt failures and the link failure. The final
destination topology is then:

``` text
A/file == B/file == C/file     (when the link for A/file succeeds)
B/file == C/file               (when it fails; A/file reported failed)
```

## 253.5 Complete Group Failure

Only when no viable materialization candidate succeeds does the group
become:

``` text
HARDLINK_GROUP_UNMATERIALIZABLE
```

That is the terminal `TopologyState::Failed` (Section 90).
`HARDLINK_GROUP_UNMATERIALIZABLE` is the group's reported outcome;
`Failed.error_code` records its cause, which is either
`CANONICAL_RETRY_EXHAUSTED` (budget exhausted) or the error of the
failure that ended the group. It is reached when any of these holds:

``` text
1. the latest failure is object_scoped (Section 207)
2. the shared per-group attempt budget is exhausted (Section 206)
3. no candidate remains and no further member can appear
```

"No further member can appear" requires that the scan of every source
root is complete, or that the number of distinct selected members seen
for the identity equals the object's link count. Before either holds, a
group whose failure permits fallback stays `Copying` and its dependents
stay held, because a later-discovered member may still materialize the
object.

At that point dependents become:

``` text
BLOCKED_BY_CANONICAL_FAILURE
```

or the equivalent terminal group-failure state.

## 253.6 Topology Preservation Rule

Fallback materialization must never create a hardlink between objects
that Flux has not established as members of the same source hardlink
identity.

------------------------------------------------------------------------

# 254. Atomic Capacity Exhaustion State Machine

`--atomic=always` must never wait indefinitely for capacity that cannot
become available.

## 254.1 Capacity States

The scheduler recognizes:

``` text
CAPACITY_READY
CAPACITY_WAIT
CAPACITY_BLOCKED
CAPACITY_IMPOSSIBLE
```

## 254.2 Temporary Shortage

If an atomic action cannot currently execute but future completion of
other operations can release sufficient temporary space:

``` text
CAPACITY_WAIT
```

The action remains pending.

## 254.3 Definitively Impossible Capacity

If Flux can establish that:

``` text
required atomic temporary capacity
>
maximum recoverable destination capacity
```

then:

``` text
FAILED_ATOMIC_CAPACITY
```

is terminal for the affected operation/action.

Flux must not spin or wait indefinitely.

## 254.4 Atomic Always

For:

``` text
--atomic=always
```

a capacity failure must never silently degrade the affected replacement
into an in-place/non-atomic overwrite.

## 254.5 Atomic Auto

For:

``` text
--atomic=auto
```

the configured fallback policy may allow a non-atomic strategy, subject
to all safety and durability rules.

The fallback must be explicit in the operation record.

------------------------------------------------------------------------

# 255. Capacity Progress and Liveness

A capacity-blocked action must carry enough information for the
scheduler to determine whether waiting can make progress.

Conceptually:

``` rust
struct CapacityWait {
    action_id: ActionId,
    required_bytes: u64,
    current_available: u64,
    reclaimable_bytes: u64,
    max_possible_bytes: u64,
}
```

The scheduler must distinguish:

``` text
waiting for reclaimable temporary space
```

from:

``` text
destination physically incapable of satisfying atomic requirements
```

No scheduler state may remain indefinitely in `CAPACITY_WAIT` without
reevaluation.

------------------------------------------------------------------------

# 256. Final V9 Identity and Recovery Invariants

The following are normative:

``` text
1. Path identity, object identity, execution identity, and artifact identity are distinct.
2. Adjacent artifacts are self-describing recovery artifacts.
3. Artifact filenames alone never establish ownership.
4. Standalone operations are registered in a persistent cleanup catalog.
5. Default cleanup enumerates the standalone catalog rather than the filesystem.
6. Target-scoped cleanup may inspect adjacent artifacts directly.
7. Recursive filesystem-wide *.flux-* discovery is prohibited by default.
8. Missing catalog entries do not authorize destructive deletion.
9. Old heartbeat age alone never proves lock abandonment.
10. Native ownership evidence takes precedence over lease age.
11. Uncertain ownership causes preservation by default.
12. Hardlink canonical ordering is distinct from materialization attempt selection.
13. A failed first canonical candidate may be followed by another deterministic candidate.
14. Hardlink fallback never merges unrelated source identities.
15. A hardlink group is terminally failed only on an object-scoped failure, an exhausted per-group budget, or no viable candidate once no further member can appear (Section 253.5).
16. --atomic=always never silently degrades because of capacity shortage.
17. Capacity waiting must have a progress/recoverability test.
18. Provably impossible atomic capacity transitions to FAILED_ATOMIC_CAPACITY.
19. Capacity states must never deadlock indefinitely.
```

------------------------------------------------------------------------

### V13 Protocol Completeness Invariants

The following are normative V13 invariants:

``` text
1. Orphaned operation workspaces are never deleted solely because a
   heartbeat or timestamp is stale.

2. Workspace collection requires terminal/abandoned state, absence of
   live ownership, valid ownership metadata, no active recovery selection,
   no required recovery state, and final ownership/lock revalidation.

3. For isolated single-file transfers, target.flux-state.<operation-id>
   is the authoritative durable location for cleanup_pending state.

4. cleanup_pending is durably recorded before final ownership/lock release
   when post-commit artifact deletion fails.

5. Cleanup failure never retroactively changes a successful transfer into
   a failed transfer.

6. The normative default retry limit is 3 retries after the initial
   execution attempt.

7. Retry count is durable and survives restart.

8. Reclaimed capacity may enter the Stage A forecast only after successful
   atomic publication and actual filesystem-reclaimable allocation; WAL
   durability alone is not a capacity-release trigger.
```

# 257. V13 Acceptance Tests

The implementation must add:

``` text
[ ] complete adjacent artifact lifecycle
[ ] crash before catalog registration
[ ] crash after catalog registration
[ ] crash after partial creation
[ ] crash after publication but before cleanup
[ ] missing adjacent state
[ ] corrupt adjacent state
[ ] target-scoped orphan cleanup
[ ] catalog-driven cleanup of DEST (non-recursive)
[ ] missing catalog entry with explicit target cleanup
[ ] foreign .flux-* files preserved
[ ] no recursive filesystem-wide cleanup
[ ] stalled live process with old heartbeat
[ ] dead process with abandoned lock
[ ] uncertain remote lock ownership
[ ] canonical member fails and second candidate succeeds
[ ] canonical candidate and all fallback candidates fail
[ ] fallback candidate never links unrelated topology
[ ] temporary atomic capacity shortage
[ ] atomic capacity becomes available
[ ] atomic capacity provably impossible
[ ] --atomic=always refuses non-atomic fallback
[ ] --atomic=auto applies configured fallback
```

------------------------------------------------------------------------

# V13 Final Acceptance Invariants

The implementation is not V13-compliant unless all of the following
hold:

``` text
SYMLINK-PAYLOAD-01
    --links=copy preserves absolute and relative symlink payloads verbatim.

SYMLINK-NOFOLLOW-01
    copying a symlink never follows its payload merely to perform the copy.

GC-OWNERSHIP-01
    adjacent artifact deletion requires positively established ownership.

GC-LOCK-01
    authoritative operation and target lock absence is revalidated immediately
    before destructive adjacent-artifact deletion.

GC-UNCERTAINTY-01
    inability to establish ownership or lock state causes preservation, not
    deletion.

TARGET-LOCK-01
    target-lock identity is namespace/path scoped and stable for the target.

TARGET-LOCK-IDENTITY-01
    weak filesystem identity does not by itself invalidate a strong target lock.

TARGET-OBJECT-01
    target-lock ownership never proves object continuity.

OBJECT-IDENTITY-01
    hardlink equivalence and persistent object reuse require sufficiently strong
    filesystem/object identity.

FS-IDENTITY-01
    persistent filesystem identity records scheme, version, value, and strength.

FS-IDENTITY-02
    ephemeral runtime filesystem identifiers are not authoritative persistent
    identities by themselves.

ATOMIC-DIR-01
    --atomic=always directory publication is whole-tree atomic publication when
    supported, not a claim derived from per-file renames.

ATOMIC-DIR-02
    unsupported safe root-directory replacement causes explicit failure under
    --atomic=always.

CAPACITY-01
    Stage A never counts merely intended deletion as already-reclaimed space.

CAPACITY-02
    whole-tree atomic staging accounts conservatively for the staged tree while
    the old destination remains live.

RECOVERY-01
    crash does not by itself create a new execution attempt.

RECOVERY-02
    an existing attempt is resumed when safely recoverable; a new attempt is
    created only after the prior attempt is durably classified unusable/failed.

UNCERTAINTY-01
    ambiguous commit/publish state is preserved as COMMIT_STATE_UNCERTAIN and
    reconciled before destructive recovery action.
```

# 258. Final Flux Specification Status --- V9

Flux now has explicit protocols for:

``` text
bounded-memory deterministic scanning
persistent hardlink topology
canonical materialization fallback
atomic capacity liveness
WAL/checkpoint durability
reboot-safe leases
adjacent single-file recovery
standalone cleanup discovery
Unicode-normalization boundaries
inode/object-generation recycling
remote filesystem locking
```

The standalone cleanup architecture is (`DEST` is a directory
operation's destination root, or a single-file target's parent
directory; Section 250):

``` text
                    DEST/.flux/
                         │
             ┌───────────┴───────────┐
             ▼                       ▼
        operations/              standalone/
             │                       │
       directory jobs          target records
                                     │
                                     ▼
                              adjacent artifacts
                                     │
                                     ▼
                              target validation
                                     │
                       ┌─────────────┼─────────────┐
                       ▼             ▼             ▼
                     LIVE         STALE         UNCERTAIN
                       │             │             │
                    preserve       remove       preserve
```

The hardlink execution architecture is:

``` text
                 Hardlink Group
                       │
                       ▼
              deterministic candidates
                       │
              ┌────────┼────────┐
              ▼        ▼        ▼
             A/file   B/file   C/file
              │
            FAIL
              │
              ▼
            B/file
              │
        ┌─────┴─────┐
        ▼           ▼
     success      failure
        │           │
        ▼           ▼
   materialize   try C/file
```

The atomic-capacity architecture is:

``` text
               atomic action
                     │
                     ▼
             capacity evaluation
                /           \
               /             \
              ▼               ▼
          sufficient        insufficient
              │                 │
              ▼                 ▼
           execute       recoverable shortage?
                              /       \
                            yes        no
                             │          │
                             ▼          ▼
                       CAPACITY_WAIT  FAILED_ATOMIC_CAPACITY
```

The governing V9 principles are:

> **Standalone artifacts are indexed, not globally hunted.**

> **A heartbeat can indicate staleness, but only authoritative ownership
> evidence can justify lock recovery.**

> **Canonical ordering determines deterministic topology;
> materialization attempts determine execution resilience.**

> **An atomic operation either remains atomic or explicitly fails; it
> never silently becomes non-atomic.**

> **Flux must preserve ambiguous artifacts rather than guessing that
> they are garbage.**

---

# V14 Normative Integration — Scheduler, Retry, and Bounded-Memory Protocol Closure

## V14.1 Purpose

V14 closes the architecture-review ambiguities concerning:

1. retrieval of disk-spilled hardlink dependents under the event-driven scheduler model;
2. stale canonical completion/failure events during retry;
3. the distinction between logical topology-state names and payload-bearing state definitions;
4. bounded coalescing-buffer exhaustion and checkpoint backpressure.

These rules are normative and are intended to remove implementation-level ambiguity without weakening persistent recovery, bounded-memory, event-driven scheduling, or retry correctness.

## V14.2 Event-Triggered Retrieval of Spilled Dependents

The prohibition on scheduler polling does **not** prohibit a targeted persistent-state lookup performed as a direct consequence of receiving a scheduler event.

For purposes of this specification:

- **Polling** means continuously, periodically, or timer-driven querying of the topology/dependency store to discover whether canonical state has changed.
- **Event-triggered retrieval** means a bounded query issued because the scheduler has already received a corresponding authoritative scheduler event or is executing an explicit recovery action.

Therefore, when a `CanonicalMaterialized` event is received, the scheduler MUST be permitted to retrieve dependents that were spilled from RAM to persistent state.

The persistent dependent-hold index is an event-addressable dependency index, not a polling source.

The normal execution path is:

```text
canonical completion
    ->
CanonicalMaterialized(canonical_identity)
    ->
scheduler receives event
    ->
release in-memory dependents
    ->
targeted persistent lookup by canonical_identity
    ->
claim/release spilled dependents
    ->
enqueue resulting work
```

The scheduler MUST NOT implement an alternative timer-driven loop such as:

```text
timer
    ->
query topology.db
    ->
look for newly materialized canonical identities
    ->
repeat
```

for normal execution.

### V14.2.1 Persistent Dependent Index

The bounded dependent-hold implementation MAY use:

```text
RAM hold index
    |
    +-- bounded in-memory dependents

Persistent hold index
    |
    +-- spilled dependents keyed/indexed by canonical_identity
```

When:

```text
CanonicalMaterialized(X)
```

is received, the scheduler MUST process persisted dependents associated with `X` through a targeted persistent lookup.

The lookup MUST be bounded. The scheduler MUST NOT load an unbounded number of spilled dependents into RAM merely because a canonical identity has many dependents.

Implementations SHOULD process persisted dependents in bounded batches, for example:

```text
query dependents for canonical_identity = X
    ordered by durable sequence
    bounded by configured batch size
    ->
process batch
    ->
repeat until no matching records remain
```

The exact query language and storage engine are implementation-defined, but the resulting behavior MUST preserve the RAM bound.

### V14.2.2 Persistent State Is Authoritative

The scheduler event is a wakeup notification and does not by itself constitute authoritative dependency state.

The persistent dependency record remains authoritative for whether a dependent is currently held, releasable, already claimed, completed, failed, or otherwise transitioned.

Consequently:

> **Events cause the scheduler to examine relevant work; persistent state determines which work transition is valid.**

This rule also permits duplicate delivery of a completion event without causing duplicate dependency release.

### V14.2.3 Recovery Exception

Explicit recovery processing MAY perform persistent-store discovery required to reconstruct lost or delayed event state.

Such recovery discovery is not considered normal-path polling.

Normal execution MUST remain event-driven.

---

## V14.3 Attempt-Fenced Canonical Scheduler Events

Canonical execution retries are distinct durable execution attempts.

Each attempt MUST have a unique `attempt_id`.

`attempt_number` identifies the ordinal retry number, but `attempt_id` is the authoritative execution-attempt discriminator.

Canonical scheduler events MUST therefore carry sufficient execution identity to distinguish attempts.

The canonical event model is:

```text
CanonicalMaterialized {
    identity
    destination_path
    operation_id
    attempt_id
    attempt_number
}
```

and:

```text
CanonicalFailed {
    identity
    destination_path
    operation_id
    attempt_id
    attempt_number
    error
}
```

Additional diagnostic fields MAY be carried, but `attempt_id` MUST remain available for authoritative stale-event fencing.

### V14.3.1 Authoritative Attempt Validation

Upon receiving a canonical scheduler event, the scheduler MUST NOT blindly apply the state transition represented by the event.

The scheduler MUST compare the event's `attempt_id` against the authoritative current canonical execution record: `TopologyState::Copying.current_attempt_id`, or the recorded attempt of a `Materialized`/`Failed` record (Section 90). The store applies the same fence to every transition (Section 91).

Conceptually:

```text
event.attempt_id == authoritative.current_attempt_id
```

MUST be true before the event can cause a canonical topology transition or dependent release/block transition.

If the identifiers differ:

```text
event.attempt_id != authoritative.current_attempt_id
```

the event is a **stale event**.

A stale event MUST:

- NOT change canonical topology state;
- NOT mark the current attempt failed;
- NOT mark the current attempt materialized;
- NOT block dependents;
- NOT release dependents;
- NOT initiate a retry;
- NOT overwrite current-attempt error information.

The implementation MAY record a bounded diagnostic/audit record that a stale event was discarded.

### V14.3.2 Retry Race Example

If:

```text
Attempt 1 -> Failed
Attempt 2 -> Copying
```

and a delayed:

```text
CanonicalFailed(attempt_id=A1)
```

arrives after Attempt 2 has become authoritative:

```text
authoritative.current_attempt_id = A2
event.attempt_id = A1
```

then:

```text
A1 != A2
    ->
stale event
    ->
discard for transition purposes
```

The event MUST NOT block dependents of Attempt 2.

Likewise, a delayed `CanonicalMaterialized(A1)` MUST NOT release dependents if `A2` is the authoritative current execution.

### V14.3.3 Event Ordering Is Not an Authority Mechanism

Scheduler correctness MUST NOT depend on event arrival order.

Events MAY be:

- delayed;
- duplicated;
- reordered;
- delivered after a retry has started;
- delivered after a newer attempt has completed.

`attempt_id` fencing against authoritative persistent state is the mechanism that makes these conditions safe.

---

## V14.4 Topology State Definition Consistency

Where a section presents:

```text
Unresolved
Copying
Materialized
Failed
```

as the logical topology states, that presentation is conceptual shorthand for the logical state dimension.

It MUST NOT be interpreted as replacing or narrowing the payload-bearing `TopologyState` definitions established elsewhere in this specification.

In particular, the authoritative payload-bearing representation of:

```text
TopologyState::Copying
```

continues to include its required durable fields, including:

```text
operation_id
canonical_path
```

and any other fields mandated by the authoritative topology schema.

Section-level summaries that list only state names MUST therefore be read as logical-state summaries, not alternative serialized schemas.

Where a conceptual summary and a payload-bearing definition appear to differ, the payload-bearing authoritative topology definition controls.

---

## V14.5 Bounded Coalescing and Checkpoint Backpressure

Checkpoint delivery from stream workers to the authoritative WAL writer MUST remain bounded and lossless.

Coalescing is permitted and SHOULD be used to reduce checkpoint-message pressure, but coalescing MUST NOT become an unbounded memory accumulator.

The pipeline is:

```text
stream worker
    ->
bounded checkpoint channel
    ->
bounded coalescing buffer
    ->
authoritative WAL writer
```

### V14.5.1 Coalescing

Multiple compatible checkpoint records MAY be combined into a bounded `CheckpointRange` representation when doing so preserves all recovery information required by this specification.

For example:

```text
[0, 4 MiB]
[4, 8 MiB]
[8, 12 MiB]
[12, 16 MiB]
```

MAY be represented as:

```text
CheckpointRange [0, 16 MiB]
```

provided the resulting durable checkpoint remains sufficient for correct recovery.

### V14.5.2 Coalescing Capacity Is Explicitly Bounded

The coalescing buffer MUST have explicit limits.

Those limits MUST include sufficient bounds to prevent unbounded growth in:

- number of checkpoint entries;
- aggregate memory consumed by checkpoint ranges;
- retained metadata associated with coalesced checkpoints.

A completely stalled WAL writer MUST NOT permit checkpoint state to grow without bound in RAM.

### V14.5.3 No Silent Checkpoint Dropping

A `ChunkCheckpoint` MUST NOT be silently discarded merely because the checkpoint channel or coalescing buffer is full.

Checkpoint information that is required for resumability MUST NOT be sacrificed to maintain unconstrained data-plane throughput.

The implementation MUST NOT use:

```text
buffer full
    ->
drop checkpoint
    ->
continue copying indefinitely
```

as a normal backpressure policy.

### V14.5.4 Producer Backpressure

When the bounded coalescing representation reaches its configured limits and the WAL writer cannot make sufficient progress, checkpoint producers MUST apply backpressure.

Workers MAY continue until the already-buffered bounded capacity is exhausted, but once the configured checkpoint-memory/channel limits are reached, further checkpoint production MUST block or otherwise exert equivalent bounded backpressure on the producing pipeline.

Physical copying MUST therefore eventually pause as necessary rather than allowing unbounded checkpoint memory growth.

Conceptually:

```text
WAL writer slow
    ->
checkpoint channel fills
    ->
coalescing begins/continues
    ->
coalescing limit reached
    ->
producer backpressure
    ->
stream worker pauses as necessary
    ->
WAL drains
    ->
worker resumes
```

This behavior is intentional.

WAL exhaustion is a pipeline backpressure condition, not permission to:

- exceed the memory bound;
- silently discard required checkpoint state;
- continue copying without durable progress information indefinitely.

### V14.5.5 Progress and Durability

The system MAY trade instantaneous data-plane throughput for bounded memory and durable resumability.

It MUST NOT trade away the latter merely to preserve throughput.

If the WAL is unavailable or permanently unable to accept required checkpoint state, the operation MUST follow the existing WAL/durability failure semantics rather than silently degrading checkpoint guarantees.

---

## V14.6 Combined Scheduler and Retry Invariants

The following invariants are normative:

1. **Event-Driven Dependency Release**  
   A scheduler MAY perform a targeted persistent dependency-index lookup only as a consequence of receiving the corresponding canonical completion event or performing explicit recovery processing. Continuous or timer-driven topology polling is prohibited in the normal execution path.

2. **Bounded Spilled-Dependent Retrieval**  
   Retrieval of disk-spilled dependents MUST be performed in bounded batches or an equivalent bounded-memory mechanism.

3. **Persistent Dependency Authority**  
   The event is a wakeup signal; persistent dependency state is authoritative for the resulting transition.

4. **Attempt-Fenced Events**  
   A canonical scheduler event MAY mutate canonical topology state or release/block dependents only when its `attempt_id` matches the authoritative current canonical execution attempt.

5. **Stale Event Safety**  
   Events belonging to prior attempts MUST NOT affect the current attempt or its dependents.

6. **Logical-State Consistency**  
   Conceptual state enumerations MUST NOT replace payload-bearing authoritative topology schemas.

7. **Bounded Checkpoint Memory**  
   Checkpoint channels and coalescing buffers MUST remain explicitly bounded.

8. **Lossless Required Checkpoints**  
   Required resumability checkpoints MUST NOT be silently dropped because of WAL backpressure.

9. **Backpressure Over Unbounded Growth**  
   When bounded checkpoint capacity is exhausted, producer backpressure MUST eventually pause physical copying as necessary to preserve memory and durability invariants.

10. **No Ordering Assumption**  
    Scheduler correctness MUST NOT depend on FIFO or otherwise ordered delivery of canonical events.

---

## V14.7 Acceptance Tests

An implementation claiming conformance MUST include tests demonstrating at least the following.

### V14.7.1 Spilled Dependent Wakeup

Given a canonical identity with more dependents than the RAM hold-index limit:

```text
dependents > RAM_limit
```

and the excess dependents spilled to persistent state:

```text
CanonicalMaterialized(X)
```

MUST cause all applicable dependents to become releasable without a periodic topology-store polling loop.

The test MUST verify that peak dependent-retrieval memory remains within the configured bound.

### V14.7.2 Delayed Failure From Prior Attempt

Given:

```text
A1 = failed
A2 = current Copying attempt
```

a delayed:

```text
CanonicalFailed(A1)
```

MUST NOT alter the state of A2 or block its dependents.

### V14.7.3 Delayed Completion From Prior Attempt

Given:

```text
A1 = superseded
A2 = current attempt
```

a delayed:

```text
CanonicalMaterialized(A1)
```

MUST NOT release dependents of A2.

### V14.7.4 Duplicate Event

Deliver the same canonical completion event more than once.

The second delivery MUST NOT produce duplicate dependency release, duplicate execution, or an invalid topology transition.

### V14.7.5 Bounded Coalescing

Artificially stall the WAL writer until the coalescing buffer reaches its configured limit.

The test MUST demonstrate that:

- checkpoint memory remains bounded;
- required checkpoint messages are not silently dropped;
- stream workers eventually experience backpressure;
- physical copying can pause;
- copying can resume when WAL capacity becomes available.

### V14.7.6 Topology Payload Preservation

Verify that conceptual references to:

```text
Unresolved
Copying
Materialized
Failed
```

do not cause required payload fields of the authoritative topology representation to be omitted.

---

## V14.8 V14 Closure Statement

With the rules in this section, the scheduler architecture is explicitly defined as:

```text
EVENT = WAKEUP
PERSISTENT STATE = AUTHORITY
ATTEMPT_ID = EVENT FENCE
BOUNDED INDEX LOOKUP = PERMITTED
TIMER-DRIVEN NORMAL-PATH POLLING = PROHIBITED
CHECKPOINT OVERFLOW = BACKPRESSURE
REQUIRED CHECKPOINT LOSS = PROHIBITED
```

This preserves the intended combination of:

- event-driven scheduling;
- persistent dependency spill;
- bounded RAM;
- retry-safe canonical execution;
- stale-event rejection;
- durable resumability;
- and bounded WAL/checkpoint buffering.

------------------------------------------------------------------------

# 259. V15 Normative Closure Revision

This section is the V15 closure layer for implementation-blocking ambiguities identified during expert review. It is normative and controls where earlier wording is ambiguous.

## 259.1 ExecutionState Versus TopologyState

`ExecutionState` remains:

```rust
enum ExecutionState {
    Running,
    Failed,
    Succeeded,
}
```

There is no `Retrying` execution state. A retry creates a new execution attempt; the historical failed attempt remains immutable.

`TopologyState::Failed` is terminal. Retries and fallback attempts happen while the record is `Copying` (Section 90): the failed attempt stays in its immutable execution history and a new attempt becomes `current_attempt_id`. Any earlier shorthand `Failed → Copying` means a failed attempt followed by a new one, never a transition out of `TopologyState::Failed`.

Canonical identity remains unchanged throughout.

## 259.2 Retry Budget and Canonical Materialization

The normative default remains:

```text
--retries=3
```

This means one initial attempt plus at most three retries, for at most four attempts per file or per hardlink group. For a hardlink group the budget is shared across retries and fallback candidates (Section 206). The retry count is durable and survives restart.

`canonical_path` is the immutable deterministic representative. `materialization_anchor` is the destination member that actually succeeds in creating the materialized object.

After a `path_scoped` failure (Section 207), eligible fallback candidates may be attempted according to the deterministic hardlink candidate ordering, within the same budget (Section 253.2). An `object_scoped` failure is terminal. A fallback candidate never becomes canonical.

Example:

```text
canonical_path          = A/file
materialization_anchor = B/file
```

is valid.

The hardlink group becomes terminally unmaterializable only under the conditions of Section 253.5: an `object_scoped` failure, an exhausted per-group budget, or no remaining candidate once no further member can appear. Earlier shorthand that canonical failure immediately blocks all dependents applies only to terminal group failure. Dependents stay held (not blocked) while a retry or fallback candidate can still establish the required materialization.

## 259.3 Source `.flux` Semantics

The basename `.flux` is not globally reserved in source data.

A source tree containing:

```text
source/.flux/
```

must treat that directory as ordinary user source content and scan/copy it normally.

The exclusion applies to the Flux-owned destination control-plane namespace, such as:

```text
DEST/.flux/
```

which must not be recursively discovered as source content.

A scanner must distinguish a user source `.flux` directory from Flux-owned destination control state using the operation's source/destination mapping and control-plane ownership. It must not globally exclude every path whose basename is `.flux`.

If the required destination control-plane path already contains an unrecognized foreign object, Flux must not overwrite or reinterpret it merely because it is named `.flux`; the operation must fail with `CONTROL_PLANE_NAMESPACE_CONFLICT` or use a separately configured control-plane location if supported.

## 259.4 WAL Backpressure and Lock Ordering

The bounded WAL/checkpoint channel is required and must not become a lock inversion mechanism.

A worker must not retain a destination mutation lock or topology lock solely while waiting indefinitely for WAL admission.

The normative durability ordering is:

```text
destination data written
        ↓
required destination durability boundary
        ↓
checkpoint claiming that durability
        ↓
WAL durability boundary
```

The WAL writer must not synchronously wait for a worker's destination flush.

Backpressure remains bounded. Implementations must provide fair admission or equivalent bounded per-worker buffering so that one worker cannot monopolize the checkpoint channel indefinitely. An unbounded global channel is prohibited.

## 259.5 Concurrent Capacity Reservation

Capacity forecasts are advisory. A definitive capacity decision is required immediately before every allocation that can consume destination capacity.

Concurrent workers must not assume the same uncommitted capacity independently. The implementation must maintain bounded reservations or an equivalent admission mechanism:

```text
available capacity
    -
reserved capacity
    -
required safety reserve
    >=
new allocation requirement
```

A reservation must be consumed or released.

Logical deletion is never automatically credited as physically reclaimed capacity merely because a rename or unlink succeeded. If reliable reclamation cannot be established, credited reclamation is zero.

The current filesystem-reported available-space result is authoritative for the definitive gate. Unix-like implementations may use `statvfs`/`statfs` or an equivalent; Windows may use `GetDiskFreeSpaceEx` or an equivalent. CoW, reflink, snapshots, deduplication, quotas, thin provisioning, compression, and delayed reclamation must not be assumed to return logical old-file size immediately.

## 259.6 Directory Target Lock Representation

For directory target `T`, let `P` be its parent. The stable lock key `K` consists of the physical identity of `P` where reliably available plus the canonical `FluxPathKey` of `T` relative to `P`.

When a visible lock representation is used, the normative layout is:

```text
P/.flux-target-locks/<sha256(K)>.lock
```

The lock record must include:

```text
format_version
complete_lock_key
operation_id
owner_instance_id
boot_session_id
target_path_key
creation_time
heartbeat
```

The digest filename is not sufficient ownership proof. A detected collision between distinct complete keys must produce:

```text
TARGET_LOCK_KEY_COLLISION
```

and the targets must never be aliased.

An OS-native lock primitive may replace the visible lock file only when it provides semantically equivalent exclusive ownership, authoritative acquisition, owner validation, and crash/disconnect recovery.

If the required target lock cannot be established, destination mutation must not proceed.

## 259.7 Weak Filesystem Identity

`st_dev` alone must never be treated as a persistent filesystem identity.

Where a persistent filesystem UUID or equivalent authoritative identifier is unavailable, Flux must not invent a value that falsely claims persistence. The identity must instead be recorded at the strength actually supported by the filesystem:

```text
Strong
Weak
Unavailable
```

A documented adapter-specific persistent identity may be used only when its persistence contract is verifiable. An in-memory random identifier is not persistent identity.

Identity strength must never be silently upgraded.

Remote filesystems remain subject to the remote lock capability rules. If safe exclusive ownership and stale-owner recovery cannot be established, an operation requiring concurrent safety must refuse mutation rather than silently degrade.

## 259.8 Commit Recovery With Weak Identity

When `PREPARE_COMMIT` exists without `COMMIT`, recovery must inspect the destination namespace and all durable commit evidence.

Strong object identity should be used when available. When identity is weak or unavailable, path existence, timestamp equality, or size equality alone must never prove that the intended rename completed.

Recovery must evaluate the complete evidence available, including:

```text
prepared destination path
operation_id
target_path_key
durable operation state
artifact/state metadata
lock ownership
filesystem namespace observation
```

If the evidence uniquely establishes the intended publication, recovery may finalize the commit.

If it cannot distinguish completed publication from non-completion, recovery must enter:

```text
COMMIT_STATE_UNCERTAIN
```

(the same classification as Section 30.1).

In that state Flux must not perform destructive compensating mutation that could overwrite or delete an unrelated object. Preservation and operator/resume investigation are the default.

## 259.9 `--atomic=always`: Existing Versus New Directory

For an existing destination directory being replaced, `--atomic=always` requires whole-tree atomic replacement semantics. If the platform cannot provide the required safe replacement primitive:

```text
ATOMIC_DIRECTORY_REPLACE_UNSUPPORTED
```

must be returned.

Flux must not delete the old tree first or silently fall back to per-file replacement.

For a previously nonexistent destination directory, there is no existing tree to replace. Atomic replacement of an old directory is therefore not required merely to create every intermediate directory.

However, a successful `--atomic=always` operation must not expose an unintended partially constructed hierarchy as the final successful publication. The implementation must use its recoverable staging/publication model and must honor any stronger initial-publication visibility guarantee it explicitly declares.

## 259.10 Atomic Directory Staging

Whole-tree atomic staging control state required for recovery must live outside the root being published or replaced.

A preferred structure is:

```text
P/.flux/atomic/<target-key>/<operation-id>/
    manifest
    state.db
    topology.db
    checkpoints/
    staging/
        root/
```

Publishing the staged root must not destroy the control state needed to recover that publication.

`DIRECTORY_STAGING` means recoverable staged state exists; it is not an instruction for immediate deletion.

Cleanup follows:

```text
durably classify failure
        ↓
durably record cleanup intent
        ↓
revalidate ownership and locks
        ↓
attempt cleanup
        ↓
if cleanup fails:
    retain cleanup_pending
```

A crash must not cause staged data to be deleted merely because it is marked `DIRECTORY_STAGING`.

## 259.11 Symlink Failure

`SYMLINK_CREATION_UNAVAILABLE` is an action-scoped failure.

Flux must not silently substitute a junction, hardlink, or regular file for a requested symlink.

Independent ready actions may continue according to scheduler policy. The final operation status must report failure when a required symlink action failed.

A symlink action does not create or alter hardlink topology, so symlink failure must not silently become a hardlink canonical failure.

## 259.12 Cleanup and Orphan Safety

`cleanup_pending` means cleanup remains outstanding after an otherwise successful transfer; it does not mean the transfer failed.

The state must be durably persisted before final ownership release. Destructive cleanup requires final validation of operation ownership, target ownership, lock state, and artifact generation.

Orphan workspaces are collectible only when the complete Section 222 predicate is satisfied: terminal/recoverably-abandoned state, no live operation ownership, valid workspace ownership, no active recovery selection, no required recovery state, durable cleanup intent where required, and final ownership/lock revalidation.

Uncertainty means preservation.

## 259.13 Normative Precedence

Implementation decisions use this precedence:

```text
1. durable operation state
2. durable execution-attempt history
3. authoritative lock/ownership state
4. authoritative filesystem namespace observation
5. filesystem identity at recorded strength
6. advisory forecasts and timestamps
7. filename-pattern inference
```

A lower-precedence signal cannot override a higher-precedence contradiction.

Filename existence does not prove ownership. A stale heartbeat does not prove lock abandonment. Logical deletion does not prove physical capacity reclamation.

## 259.14 V15 Acceptance Tests

A conforming implementation must test at least:

```text
1. Sections 202–222 are present and authoritative.
2. ExecutionState has Running, Failed, Succeeded only.
3. Every retry creates a new attempt.
4. Historical failed attempts remain immutable.
5. --retries=0 permits only the initial attempt.
6. --retries=3 permits at most four total attempts.
7. Retry count survives restart.
8. canonical_path never changes during retry/fallback.
9. fallback materialization does not promote the fallback to canonical.
10. source/.flux is copied as ordinary source data.
11. DEST/.flux control state is not scanned as source.
12. foreign DEST/.flux state is never silently overwritten.
13. directory target lock keys are deterministic.
14. lock-key collisions are detected.
15. unsafe remote locking causes refusal.
16. workers do not wait indefinitely for WAL while holding mutation locks.
17. destination durability precedes durable checkpoint claims.
18. checkpoint backpressure is bounded and fair.
19. concurrent capacity reservations prevent collective overcommit.
20. filesystem available space is the authoritative capacity gate.
21. logical deletion is not assumed to reclaim physical capacity.
22. existing-directory --atomic=always requires atomic replacement.
23. new-directory creation is distinguished from replacement.
24. atomic staging control state survives publication.
25. DIRECTORY_STAGING is not deleted solely after crash.
26. cleanup failure retains cleanup_pending.
27. weak filesystem identity is explicitly represented.
28. weak identity cannot prove rename success from path existence alone.
29. ambiguous commit recovery becomes conservative uncertainty.
30. symlink failure does not silently change object type.
31. a topology store transition carrying a superseded attempt_id returns
    StaleAttempt and changes nothing.
32. crash recovery never returns a Copying record to Unresolved and never
    resets attempt_number.
33. under fallback, Materialized.destination_path is the materialization
    anchor and canonical_path is unchanged.
34. dependents stay held (not blocked) while a retry or fallback candidate
    remains, and become BLOCKED_BY_CANONICAL_FAILURE only on terminal Failed.
35. an object_scoped canonical failure makes the group terminally Failed
    without attempting any fallback candidate.
36. --retries=N bounds a hardlink group to N+1 attempts in total across
    retries and fallback candidates, including after restart.
37. after a fallback anchor materializes, the failed canonical becomes a
    link to the anchor; if that link fails it is reported with both errors.
38. a group whose canonical failed path_scoped before any other member was
    discovered stays Copying with dependents held, and a later-discovered
    member becomes the next candidate.
39. a single-file operation registers at P/.flux/standalone/<sha256(K)>.record
    in the target's parent, and resume finds it without listing P.
40. a crash between adjacent-state creation and catalog registration is
    recovered by a non-recursive listing of P.
41. `flux cleanup DEST` enumerates only DEST/.flux/operations and
    DEST/.flux/standalone; `flux cleanup` with neither DEST nor --target is a
    usage error.
42. after completion, empty P/.flux/standalone/ and P/.flux/ created by Flux
    are removed; a pre-existing foreign P/.flux is left untouched.
43. with roots /x/b and /y/a whose files b/f and a/f are hardlinked on one
    filesystem, the canonical is a/f for either command-line order of the
    roots, and the scanner emits /y/a before /x/b.
44. two source roots with the same final component are rejected with
    DESTINATION_NAMESPACE_COLLISION before any transfer.
45. resuming with the same roots in a different command-line order is
    compatible; adding, removing, or remapping a root is INCOMPATIBLE_STATE.
```

## 259.15 V15 Implementation Baseline

The implementation baseline is:

```text
immutable canonical identity
+
immutable execution history
+
new attempt for every retry
+
deterministic fallback materialization
+
explicit materialization anchor
+
durable retry budget
+
destination-scoped control-plane ownership
+
ordinary source semantics for user .flux directories
+
authoritative target locking
+
conservative weak-identity recovery
+
filesystem-authoritative capacity gates
+
bounded capacity reservations
+
destination durability before durable checkpoint claims
+
bounded fair WAL backpressure
+
external recoverable atomic-directory staging
+
explicit new-directory versus replacement semantics
+
action-scoped symlink failure
+
durable cleanup_pending
+
conservative orphan collection
```

These rules close the implementation-level ambiguities identified by expert review without changing the fundamental FLUX architecture.

------------------------------------------------------------------------

# 260. V15 Closure Status

V15 is the implementation-baseline revision of the FLUX specification.

The V15 closure explicitly resolves:

```text
retry/topology semantics
source .flux namespace handling
WAL lock/backpressure ordering
concurrent capacity reservation
directory target-lock representation
weak filesystem identity behavior
weak-identity commit recovery
atomic initial directory creation semantics
external atomic directory staging
Windows symlink failure semantics
cleanup_pending durability
orphan workspace collection
```

The governing principle is:

```text
uncertainty → preserve state
unsafe ownership → refuse mutation
failed attempt → immutable history
retry → new attempt
canonical identity → immutable
capacity forecast → advisory
filesystem availability → authoritative
cleanup → ownership-validated
atomic publication → never silently downgraded
```

**V15 implementation baseline: CLOSED.**

**V16 implementation baseline:** V15 as amended in place by V16 (see
the Revision Notice at the top of this document).

