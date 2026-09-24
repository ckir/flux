# Directory walker and recursive copy — design

**Date:** 2026-09-23
**Status:** approved, not yet planned
**Scope:** the ordered directory walk, the filesystem primitives it needs, portable object identity,
and `copy_tree` driving the existing `copy_file`. Delivered as four pull requests — three at first, with the last split at the engine boundary after
PR 2 merged; this document is
the design for all three.

## Why now

`copy_file` landed in PR #32 and the walker decision closed in PR #35, so the Phase 2 blocker is the
traversal itself. The `FileSystem` trait has **no traversal method at all** — that absence is why the
walker blocks Phase 2 rather than Phase 3, and it is why the decision had to precede the code.

The decision itself is settled and is not reopened here: no third-party walker crate, a single-level
`read_dir` on the trait, and the ordered depth-first walk in `flux-core` over an explicit stack.
`TODO.md` at `10bf2a0` carries the measured grounds.

## What this design had to settle, and how

Every item below was consulted with the agy peer under AGY-FIRST across three rounds, and every factual
claim was verified by measurement before folding. The full record, including three claims of the peer's
that measurement refuted, is at `.clavity/scratch/walker-design/converged-design.md`.

Two things are worth stating plainly because they shaped the result:

- **Three apparent design forks turned out to be settled by the spec**, not open. Symlink policy
  (§25, §26), the ordering representation (§7.2, §241), and the API shape (§9) all have normative
  answers. Reading the spec closed them faster than designing would have.
- **One agreed answer was wrong, and only re-checking it caught that.** Both parties concluded that
  object identity could be deferred to the hardlink phase, on the premise that hardlinks are its only
  consumer. That premise is false — see "Object identity" below.

## The trait surface after this work

```rust
/// What a directory listing reports per entry.
///
/// Deliberately NOT `Metadata`: the entry type is what the OS supplies during
/// enumeration, and a `Metadata` per entry would re-stat every child, which is the
/// cost this primitive exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Dir,
    Symlink,
    Other,
}

/// One entry of one directory. The NAME, not a path: the walk already holds the
/// parent on its stack, and a `PathBuf` per entry would allocate a full path for
/// every child of every directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: std::ffi::OsString,
    pub file_type: FileType,
}

/// Portable filesystem object identity (§109).
///
/// BOTH fields, never one: §109 says "Never compare only inode" or "only file ID",
/// and requires `filesystem_id + object_id`, so that unrelated objects on different
/// filesystems are not merged.
///
/// `index` is `u128` because Windows needs it: the 64-bit file index is not unique
/// on ReFS, so a strong identity requires `FILE_ID_INFO`'s 128-bit file id. Unix's
/// `st_ino` is a `u64` and is zero-extended into the same field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectId {
    /// Unix `st_dev`; Windows `VolumeSerialNumber`.
    pub volume: u64,
    /// Unix `st_ino`, zero-extended; Windows the 128-bit `FileId`.
    pub index: u128,
}

/// §107: "A `FileIdentity` must therefore be accompanied by its reliability
/// classification." Three states, not two.
///
/// This is deliberately NOT `Option<ObjectId>`. An `Option` collapses *weak* into
/// *strong*, which is the exact failure §107 exists to prevent: FAT32, exFAT, and
/// some SMB and NFS configurations produce an identity that EXISTS and cannot be
/// trusted. Everything that acts on identity below acts only on `Strong`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileIdentity {
    Strong(ObjectId),
    Weak(ObjectId),
    Unavailable,
}

pub struct Metadata {
    pub len: u64,
    pub is_file: bool,
    pub permissions: Option<Perms>,
    pub modified: Option<SystemTime>,
    /// §149.4 asks for identity "where strongly supported", and §107 requires the
    /// strength to travel with it. One field, three states, no second way to say
    /// "absent".
    pub identity: FileIdentity,
}

pub trait FileSystem: Send + Sync {
    // ... the eight existing methods, unchanged ...

    /// ONE level. Returns every entry with its file type, in whatever order the OS
    /// gave them — ordering is the caller's job (§7.2), because only the caller knows
    /// the comparison rule.
    ///
    /// Returns a `Vec`, not an iterator, and that is deliberate: §7.2 requires each
    /// directory to be sorted, sorting requires the whole directory in hand, so a lazy
    /// return shape would promise a laziness the caller cannot use. Materialising also
    /// drops the directory handle before the walk recurses, so only one is ever open.
    ///
    /// A plain `Vec<DirEntry>` with an `OsString` per name, deliberately: a packed
    /// layout was measured at 1.91x smaller and rejected, because it would put a
    /// custom representation in this trait's public return type to win a constant
    /// factor that no realistic directory notices. See "What stays unresolved".
    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>>;

    /// Creates one directory. Fails if the parent is missing; the walk creates
    /// ancestors in order, so it never needs the recursive form.
    fn create_dir(&self, path: &Path) -> Result<()>;
}
```

`FileType` has **no `Unknown` variant.** Linux `readdir` can return `DT_UNKNOWN`, but Rust's std
resolves it transparently — `library/std/src/sys/fs/unix.rs:1149` is
`_ => self.metadata().map(|m| m.file_type())` — so `DirEntry::file_type` never surfaces "unknown" to a
caller. The variant would be unconstructible.

The cost this hides must be stated rather than asserted away: on a filesystem that supplies no
`d_type`, that fallback is a real `lstat` per entry. This repository measurably uses such a mount —
`/mnt/c` under WSL is v9fs. So `read_dir` is "one syscall per directory **plus** a stat per entry the OS
could not type", not "no stats". On Windows there is no such cost: the reparse tag arrives in
`wfd.dwReserved0` as part of the enumeration itself (`sys/fs/windows.rs:1143`).

There is a second cost, and it is the honest limit of "the file type avoids a re-stat": the ancestor-set
check below needs each directory's `ObjectId`, which `DirEntry` does not carry, so the walk calls
`metadata` **once per directory** before descending. Files cost nothing extra, which is the case that
dominates a real tree, and it remains true that no stat is spent merely to learn whether an entry is a
file. But the claim is "no stat per FILE", not "no stats at all", and the difference is worth stating
where a reader would otherwise be surprised by the syscall count.

## The walk

### Ordering

Sort each directory's entries by `name.as_encoded_bytes()`, ascending. Nothing else.

§7.2 is explicit that this suffices: *"A depth-first scanner that sorts each directory's entries by name
alone emits component-wise order without buffering."* Verified against the spec's own normative example
— depth-first plus a per-directory byte sort gives `a/x  a/y/z  a-b  a0`, the component-wise order,
where a flat `/`-joined sort gives `a-b  a/x  a/y/z  a0`, the order §7.2 calls wrong.

`as_encoded_bytes()` is exactly the §241 representation on both platforms, with no conversion and no
allocation: `library/std/src/sys/os_str/mod.rs` selects `mod wtf8` for `target_os = "windows"` and
`mod bytes` elsewhere, and `wtf8.rs:21-22` is `Slice { inner: Wtf8 }`.

**`OsStrExt::encode_wide()` must never be used for ordering.** UTF-16 code-unit order and WTF-8 byte
order disagree above the BMP, because surrogates (`0xD800`–`0xDBFF`) sort below the private-use area in
UTF-16 while their UTF-8 encodings sort above it. Measured:

| character | WTF-8 bytes | UTF-16BE code units |
|---|---|---|
| `U+E000` | `EE 80 80` | `E0 00` |
| `U+10000` | `F0 90 80 80` | `D8 00 DC 00` |

so WTF-8 puts `U+E000` first and UTF-16 puts `U+10000` first. A test pins this pair.

No `FluxPathKey` (§103) is materialised. It is an ordering and identity key for persistent indexes and
for multi-root operations (§18.3), and this work has a single root and no index. Its `0x00` separator
and the `PATH_COMPONENT_INVALID` code it requires arrive with the phase that builds keys.

### Symlinks

Never followed. The walk descends only where `file_type == Dir`.

§25: the default is `copy the symlink itself`, and *"Following symlinks is opt-in"*. §26 puts
`--links=follow` in **Future**, requiring cycle detection, containment checks, mount awareness and
identity checks before it can exist.

This costs nothing on either platform. On Unix a symlink is not a directory. On Windows, junctions and
directory symlinks carry the name-surrogate reparse bit, and `sys/fs/windows.rs:1200-1211` computes
`is_symlink` from `reparse_tag & 0x20000000`, with `is_dir()` defined as `!self.is_symlink &&
self.is_directory` — so an `is_dir()`-gated descent already skips them. Reparse points that are *not*
name surrogates, such as deduplication and cloud placeholders, correctly remain ordinary files and
directories.

Symlinks are still **reported** as `Symlink` events. This work does not recreate them; `copy_tree`
reports `SPECIAL_FILE_UNSUPPORTED` for each, which is what `copy_file` already does for a symlink
source today.

### The item type

```rust
pub enum WalkEvent {
    /// Pre-order: the directory exists and is about to be descended into.
    Dir { path: PathBuf, identity: FileIdentity },
    File { path: PathBuf },
    Symlink { path: PathBuf },
    Other { path: PathBuf },
    /// Post-order: every descendant of this directory has been yielded.
    DirEnd { path: PathBuf },
}

pub struct WalkError {
    pub path: PathBuf,
    pub cause: FsError,
}
```

`path` is **relative to the walk root**, so a consumer joins it onto the source root to read and onto
the destination root to write. That is the §7/§103 framing and it keeps destination mapping trivial.

`DirEnd` is not speculative generality. §30.2 requires that *"Directory metadata that is sensitive to
child mutations MUST NOT be finalized before all required child creations, removals, renames, and
hardlink creations are complete"* — a directory is therefore created pre-order but has its metadata
finalized **post-order**, because writing children mutates the parent's mtime. A plain pre-order entry
stream cannot express "leaving this directory", so a consumer would have to re-derive it by comparing
path prefixes, which is fragile exactly where §7.2's component-wise ordering is subtle. The walker
already knows when it pops its stack, so emitting the event costs nothing.

**Neither PR 3 nor PR 4 consumes `DirEnd`; the walk emits it** — see "Directory metadata" below.

### Iteration and errors

```rust
/// Borrows the filesystem for the life of the walk; holds the explicit stack, the
/// ancestor set, and the root it makes paths relative to.
pub struct Walk<'a, F: FileSystem> { /* private */ }

/// `root` must name a directory. The walk yields no event for the root itself —
/// its first event is the root's first child — because the root is not part of
/// the tree being copied INTO the destination, it IS the destination mapping.
pub fn walk<'a, F: FileSystem>(fs: &'a F, root: &Path) -> Result<Walk<'a, F>>;

impl<'a, F: FileSystem> Iterator for Walk<'a, F> {
    type Item = std::result::Result<WalkEvent, WalkError>;
}
```

`walk` is fallible before it yields anything: it reads the root's metadata, and returns
`Err(FsError)` if the root does not exist, or `Code::SpecialFileUnsupported` if the root exists but is
not a directory. Those are failures of the whole operation, not per-entry errors, so they are reported
through `Result` rather than as a first `Err` item.

A pull iterator, because §9 requires that when downstream capacity is exhausted the
*"scanner blocks/awaits"* rather than accumulating paths. A consumer that stops pulling **is** the
backpressure, so the bounded queue Phase 3 adds sits between this iterator and the workers without the
walker changing.

**An `Err` item is not terminal.** The iterator yields the error, skips that subtree, and continues with
the next sibling. This is the single easiest contract here to get wrong, because most fallible Rust
iterators stop. It is required by spec item 83, quoted verbatim:

> `DIRECTORY_CHANGED_DURING_SCAN` has one outcome: the directory's subtree is not transferred, the
> error is reported, and the operation exits 1. There is no configured mutation policy or rescan
> alternative.

A directory that vanishes, changes identity, or cannot be read between listing and entry therefore
produces one `Err`, no descent, and a continued walk. The operation's exit status is 1 even though the
walk completed. A test asserts that a walk over a tree with one unreadable directory still yields every
entry of its siblings.

`Code` gains one variant, `DirectoryChangedDuringScan`, rendering as
`"DIRECTORY_CHANGED_DURING_SCAN"` (§2 item 83, §149.4, taxonomy line 2928).

An unreadable directory that is *not* an identity change — a permission denial, say — is reported the
same way: the error carries its own classified code, the subtree is skipped, the walk continues. The
spec mandates this shape only for identity changes; applying it to every per-directory failure is a
design decision, taken because the alternative is aborting a whole tree copy over one denied
subdirectory.

## Object identity

Both parties initially agreed to defer `dev`/`ino` to the hardlink phase. That was wrong, and the
premise behind it — that hardlinks are the only consumer — is false. There are **three consumers inside
this work**:

1. **§149.4 binds the scanner directly.** *"The scanner must verify that the object being entered
   remains consistent with the planned directory identity."* A walker that never obtains identity has no
   baseline to compare against and cannot raise `DIRECTORY_CHANGED_DURING_SCAN` at all.
2. **§129 and §149.6 need it for safety.** `flux copy /data /data/backup` must be `SAFETY_REJECTED`
   *before transfer begins*, Flux *"must not rely on `exclude destination` as the primary safety
   mechanism"*, and must never scan the destination as part of the source *"even if filesystem namespace
   relationships change after startup"*. That final clause cannot be satisfied by a lexical prefix test.
   `copy_file`'s existing self-copy check is lexical only (`copy.rs:139`) and `TODO.md` already records
   that as a gap against foundational invariant 22.
3. **Directory cycles are reachable without symlinks.** Measured on WSL Ubuntu-26.04:

   ```
   mkdir -p a/b && mount --bind a a/b
   control, before the mount:  a/b/b exists? NO
   after:                      a/b/b exists? YES
   is symlink (-L): NO         is dir (-d): YES
   stat a   : dev=120 ino=13
   stat a/b : dev=120 ino=13     identical=YES
   ```

   A bind mount is neither a symlink nor a reparse point, so an `is_dir()`-gated walk descends into it.
   The self-reference measured is **finite** — two levels, because the mount attaches to the path dentry
   rather than the inode, so `a/b/b` resolves to the underlying empty mount point — so the reachable harm
   is **duplication**, the subtree being copied twice, not a hang. Identical `dev`+`ino` means identity
   detects it either way.

### How the checks are structured

The walk maintains an **ancestor set** of the `ObjectId`s of directories on the current path — not a
global visited set. This matters: a global set would grow with the total number of directories, which
spec line 997 forbids (*"The scanner must never construct an in-memory list proportional to the total
number of files"*). An ancestor set is bounded by depth, and it is sufficient, because a cycle by
definition re-enters an ancestor.

**The walk root is in the ancestor set from the start**, even though no event is emitted for it. This is
load-bearing rather than tidy: the measured bind-mount case is `mount --bind a a/b`, where the
re-entered directory *is the root itself*. A walk that only added directories it emitted events for
would descend into `a/b`, compare it against an empty set, and copy the whole tree twice — the exact
defect the check exists to prevent. Obtaining the root's `ObjectId` is part of `walk`'s fallible
initialisation, alongside its "is this a directory" check.

Before descending into a directory `D`:

- if the depth would exceed the cap → report, skip the subtree, continue. This runs at every identity
  strength and is the only guard that does.
- the lexical containment test runs at every identity strength too — see item 1 of "When identity is
  weak or unavailable".
- if `D`'s identity and the destination anchor's are BOTH `Strong`, then:
  - if its `ObjectId` is already in the ancestor set → a cycle. Report, skip the subtree, continue.
  - if its `ObjectId` equals the **destination anchor's** → `SAFETY_REJECTED`, abort the whole
    operation. This is the dynamic half of §149.6.
- otherwise the identity checks are skipped for this entry and the operation warns once per affected
  filesystem, unless `--safety=strict` was given, in which case it refuses.
- otherwise push it and descend, popping at `DirEnd`.

### The hole the directory checks do not cover

Everything above compares DIRECTORIES — an entry against the ancestor set, and an entry against the
destination anchor. **Nothing compares a FILE against its destination**, and that is a gap in this
design rather than an omission from the implementation. It was found while scoping PR 3, after PR 1 and
PR 2 had merged.

`copy_tree` delegates every file to `copy_file`, which is what `TreeFailureCause::Copy` records. And
`copy_file`'s own self-copy refusal is `if src == dst` — a LEXICAL comparison. So:

```
flux copy /a /b        where /b/x is a hardlink to /a/x
```

The roots differ, lexically and by identity, so no tree-level check fires. `copy_file(/a/x, /b/x)` sees
two different paths, so the lexical check does not fire either. The file is copied onto its own object.

A junction or a bind mount reaches the same place by a different route. This is exactly what invariant
22 forbids — *"not **only** lexical path comparisons"* — and what §129 means by not relying on
`exclude destination` as the primary mechanism.

**The hole is reachable in the shipped single-file copy today, and a tree copy is what makes it
routine.** An earlier draft of this section claimed it was "unreachable before a recursive copy exists,
because a single-file copy's source and destination are named by the user". That is false, and
`TODO.md` records the counter-example against this very item: `flux copy a ./a` names two paths that are
lexically different and the same object, so the Step 0 check passes and the file is copied onto itself.
A hardlink or a junction given directly to `flux copy` reaches the same place. What a TREE copy changes
is not reachability but SCALE and CONSENT — it generates destination pairs the user never typed, one per
file, so a single mistyped root can hit the case a thousand times over.

That distinction matters for scoping. The check is not speculative work for a feature that does not
exist yet; it is a live defect in shipped behaviour whose recorded blocker — a portable identity
primitive — PR 1 removed.

`TODO.md` has carried this as "the self-copy refusal compares paths, not filesystem identity" since the
single-file cut. PR 1 delivered the primitive, so the blocker is gone.

#### Where the per-file check lives, exactly

`copy_file` **owns** the check, and `copy_tree` does not pre-screen. Putting it in `copy_tree` would
leave `flux copy a ./a` — the single-file case that is reachable now — still unguarded, and would make
the guarantee a property of one caller rather than of the operation. Every caller of `copy_file` gets it.

It is a NEW gate, and **Step 0 is left exactly as it is**. The existing lexical refusal at
`crates/flux-core/src/copy.rs:139` promises to refuse before touching the filesystem, and that promise
is pinned by `a_self_copy_is_refused_before_anything_is_touched`, which asserts the recorded call list
is EMPTY. An identity comparison cannot keep that promise, because it must stat the destination. So the
two coexist rather than one replacing the other, and the pinning test needs no edit — which is the
outcome to prefer, since rewriting a pinning test to accommodate new code is the move that needs the
most justification.

The new gate sits **after Step 2 and before Step 3** — after the source metadata is read, before the
exclusive `create_new` of the staging temporary. That position is what makes the refusal cost nothing
that has to be cleaned up: no temporary exists yet, so a refusal leaves the filesystem exactly as it
found it, and the source metadata the gate needs is already in hand from Step 2. The only new syscall is
one `metadata` on the destination.

The revised step order:

| Step | What happens |
|-----:|--------------|
| 0 | lexical self-copy refusal, before touching the filesystem — **unchanged** |
| 1 | leftover staging sweep |
| 2 | source `metadata` (also captured for the Step 7 re-check) |
| **2a** | **identity gate on the destination — NEW** |
| 3 | exclusive `create_new` of the temporary |
| 4 | stream |
| 5 | durability |
| 6 | metadata on the temporary |
| 7 | source-unchanged re-check, then publish |

#### What the gate does, case by case

The destination stat goes through `FileSystem::metadata` on the trait, never `std::fs` directly, so
`FaultFs` can fake every branch below and the behaviour is testable without a real hardlink.

That trait method is `symlink_metadata`-based on both arms — `std_fs.rs` uses `symlink_metadata` on Unix
and opens with `FILE_FLAG_OPEN_REPARSE_POINT` on Windows — and that is the semantic this gate needs:

- a **hardlink** destination shares the source's inode, so the gate sees the same `ObjectId` and refuses.
  This is the case the gate exists for.
- a **symlink or junction** destination is judged as the NAME being replaced, not as what it points at,
  so it is not refused. Replacing it is intended behaviour, pinned by
  `rename_replace_allows_a_symlink_whose_target_is_read_only`
  (`crates/flux-platform/tests/std_fs.rs:68`), whose own reasoning is that judging the target instead of
  the name is "a false positive: a copy that should succeed and does not". Following the link here would
  reintroduce exactly that.

  That test is `#[cfg(unix)]`, so it pins the INTENT rather than the Windows behaviour — a plan must not
  cite it as a Windows oracle. The Windows arm reaches the same place by a different route:
  `metadata` opens with `FILE_FLAG_OPEN_REPARSE_POINT`, so a junction is likewise typed as the link and
  not as its target.

| Destination state | What the gate does |
|---|---|
| `NotFound` | **Proceed.** No object exists, so no alias is possible. This is the dominant case for a tree copy into a fresh destination, and it must cost nothing: no refusal, no warning, not even a diagnostic. |
| exists, both identities `Strong`, `ObjectId` equal | **Refuse** with `Code::SafetyRejected` — the same code Step 0 already returns for the lexical case, because it is the same class of refusal reached by a stronger test. |
| exists, both `Strong`, `ObjectId` differs | Proceed. |
| exists, is a **directory** | **Refuse** with `Code::SafetyRejected`. The stat is already taken, so the `FileType` is free, and refusing here names the real reason. Without it the copy proceeds to Step 7 and fails at the rename with a platform error that does not say "the destination is a directory". |
| exists, either side not `Strong` | See below — degrade by default, refuse under `--safety=strict`. |
| the stat itself fails for another reason | Proceed. The gate is a guard, not the operation; a destination that cannot be inspected still meets the ordinary failure paths at Step 3 and Step 7. |

#### The degraded case for files, which fails OPEN

For directories, degrading to the lexical floor is bounded: the depth cap stops a runaway descent at
256. **A file has no such bound.** If identity degrades, the aliased overwrite simply happens — the
check fails open, and nothing downstream catches it.

The design accepts that by default and gives it an off switch, rather than refusing outright:

- **Default: degrade to the lexical floor, and warn.** What is actually lost is the HARDLINK
  relationship, not the data — `copy_file` stages through a distinct temporary, so the bytes published
  are the source's own, read before the destination was touched. `TODO.md` records this same reading.
- **`--safety=strict`: refuse** whenever identity is not `Strong` on both sides.

Refusing by default was considered and rejected. §107 names FAT32 and exFAT first among weak-identity
filesystems, and those are most removable media — so a refuse-by-default rule would block the most
ordinary consumer backup there is, in exchange for preventing link-breakage rather than data loss. That
is §108's precedent applied unchanged: `--hardlinks=auto` degrades with an aggregated warning while
`preserve` refuses and is told "do not guess".

#### Where `--safety=strict` lives

`CopyOptions` has no safety field today — it carries `preserve_times`, `preserve_permissions`,
`durability`, `publish` and `operation_id`. PR 3 adds one, because `copy_tree(fs, src, dst, opts)` takes
a single options parameter and that is the only channel a caller has.

```rust
/// Whether a safety check that cannot be made with full confidence refuses or degrades.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    /// Degrade to the lexical floor when identity is not `Strong`, and warn.
    Default,
    /// Refuse whenever identity is not `Strong` on both sides.
    Strict,
}
```

Added to `CopyOptions` as `safety: Safety`. A two-variant enum rather than a `strict_safety: bool`,
because that file already prefers named states over booleans — `Preserve`, `Durability` and `Publish`
are all enums, and `Preserve` exists as three states precisely because a bool could not express the
distinction it needed. A third safety mode is easy to imagine; a second bool is not.

A separate `TreeOptions` parameter was considered and rejected: it would split one concept across two
types, and **the field has to reach `copy_file` anyway**, since Q1's answer makes strict mode change
single-file behaviour at the Step 2a gate. `CopyOptions` is already the type `copy_file` receives.

`CopyOptions` has no `Default` impl and does not gain one here — every construction site names all its
fields, which is what makes adding a field a compile error at each of them rather than a silent
inheritance of a default nobody chose. There are exactly two sites today,
`crates/flux-cli/src/main.rs:31` and the `opts()` test helper at `crates/flux-core/src/copy.rs:254`, and
both are updated in PR 3.

**The FLAG is PR 4; the enum and its behaviour are PR 3.** Wherever this document says
`--safety=strict`, it names the behaviour `Safety::Strict` selects, not a command-line surface PR 3
builds — PR 3 has no CLI. The flag that sets it is part of the CLI cut, and until then `Safety::Strict`
is reachable only by a library caller, which is exactly what makes it testable before it is exposed.

The **destination anchor** resolves the case §129 has to handle before anything is created. A tree copy
is usually given a destination that does not exist yet, so it has no identity to compare against:

- if `dst_root` **exists**, the anchor is `dst_root` itself;
- if it **does not**, the anchor is its nearest existing ancestor — in practice its parent, which must
  exist for `create_dir` to succeed.

That is sufficient, because a destination nested inside the source implies its nearest existing ancestor
is also inside the source, or is the source root. Anchoring on the parent is therefore the check that
can run *"before transfer begins"* as §129 demands, rather than after the destination has been created.
A pre-flight comparison of `src_root` against the anchor runs before the walk starts; the per-directory
comparison above is what catches a namespace change *after* startup.

**When the ANCHOR's identity is weak, the pre-flight degrades by the same rule** — no special case, and
no stricter treatment for being the earliest check. The reasoning is worth spelling out, because "the
one check §129 requires before transfer begins" invites making it the exception:

The pre-flight guards **overlap** — copying `/data` into `/data/backup` — which is a different hazard
from the cycles the walk guards, and the depth cap does not bound it. A cap of 256 limits how DEEP a
runaway descent goes; it does nothing about the duplication of copying a tree into itself, which at 256
levels could be enormous. So the cap is not what makes degrading here safe.

What makes it safe is that **the lexical containment floor always runs, at every identity strength**.
§129's own named example is lexically detectable, so the case the specification puts its name to is
still caught when identity degrades. What degrading loses is only the ALIASED overlap — a destination
reached through a junction, a bind mount or a hardlinked ancestor — which is precisely the case
`--safety=strict` exists to cover. That keeps the pre-flight consistent with the per-file rule instead
of resting on a guard aimed at a different hazard.

### When identity is weak or unavailable

`Weak` and `Unavailable` behave identically, because a value that cannot be trusted is worth no more
than a value that is absent. On both target platforms `Strong` is the normal case — Unix always,
Windows via `FILE_ID_INFO` on NTFS and ReFS — but the degraded path is not exotic: §107 names FAT32 and
exFAT, which is most removable media, and some SMB and NFS configurations, which is most network
storage. Backups go to exactly those places.

The design does not turn the checks off. It gives each one a fallback that is always available, and
bounds what remains. The general rule is foundational invariant 23, *"Platform optimizations have safe
fallbacks or explicit strict failure"*, and the concrete precedent is §108, where `--hardlinks=auto`
degrades with an aggregated warning while `--hardlinks=preserve` fails outright and is told "Do not
guess".

**1. The lexical containment test always runs, at every identity strength.** Invariant 22 asks that
safety checks use identity *"not **only** lexical path comparisons"* — lexical is a floor that identity
is added to, not a substitute it degrades into. §129's own example, `flux copy /data /data/backup`, is
lexically detectable, so containment is never unchecked.

**2. Identity checks run in addition, and only where BOTH sides are `Strong`.** A comparison is no
stronger than its weaker operand, so a `Strong` source directory compared against a `Weak` destination
anchor yields no guarantee. Asymmetry is the normal case in practice — a local ext4 source copied to an
SMB destination — so this is the common path, not a corner.

**3. A depth cap bounds any runaway descent, at every identity strength.** MEASURED on WSL
Ubuntu-26.04: creating nested directories with relative paths and `chdir` reached **5000 levels** (the
probe's own cap, not the OS's) at a path length of **55,020 bytes**, over thirteen times the
`getconf PATH_MAX` of 4096. The kernel enforces no cumulative depth limit.

The walk is therefore bounded today only because it addresses entries as `src_root.join(rel)` —
absolute paths, where each single argument hits `PATH_MAX` at roughly 370 levels of ten-character
names. That is an accident of path construction, not a guard, and `TODO.md` already contemplates moving
to `openat`-style relative traversal to close walker TOCTOU, which would silently remove it.

**The cap is 256, configurable.** Chosen to sit *below* the `PATH_MAX`-implied bound rather than above
it, so the guard is the operative one both now and after any move to relative traversal — the behaviour
does not change out from under a user when the traversal does. Real trees are an order of magnitude
shallower. Exceeding it reports a failure for that subtree and skips it; it does not abort the
operation.

**4. The default degrades with one aggregated warning; `--safety=strict` fails instead.** One warning
per affected filesystem per operation, following §108's shape and its example wording rather than one
line per path, which would bury it. `--safety=strict` refuses when identity is not `Strong` on both
sides, which is invariant 23's "explicit strict failure" and §108's `preserve` arm.

**The engine CAPTURES the warning; the CLI renders it.** Splitting the last cut at the engine boundary
means PR 3 has no printer, so "warn once per filesystem" has to survive as data until PR 4 can display
it. `TreeOutcome` therefore carries the aggregation, not a formatted string.

Aggregating is not uniform, because the two degraded states carry different amounts of information:

- **`Weak(ObjectId)` carries a volume, and that volume is trustworthy even when the index is not.** On
  FAT32 and exFAT it is the object INDEX that is unstable; the mount identifier is not. So `Weak` groups
  by `volume`, and two different removable drives produce two warnings rather than collapsing into one.
- **`Unavailable` carries nothing**, so there is no key to group by. It gets a single catch-all bucket.

```rust
/// Why identity comparison was skipped, aggregated for one warning apiece.
#[derive(Debug)]
pub struct WeakIdentityWarnings {
    /// One entry per affected volume, keyed by `ObjectId::volume` — a bare `u64`
    /// (Unix `st_dev`, Windows `VolumeSerialNumber`), since there is no newtype for it.
    /// `BTreeMap` rather than `HashMap` so the warnings render in a stable order.
    pub weak: BTreeMap<u64, DegradedGroup>,
    /// Everything whose identity was `Unavailable`, which names no volume to group by.
    pub unavailable: Option<DegradedGroup>,
}

#[derive(Debug)]
pub struct DegradedGroup {
    pub count: u64,
    /// One path, so the warning can point at something concrete without listing all of them.
    pub example: std::path::PathBuf,
}
```

An earlier draft aggregated purely by REASON — two variants, `Weak` and `Unavailable`, each with a count
and one example — on the grounds that it never asserts a filesystem it cannot name. That is a real
property, but it bought it by discarding information that `Weak` actually has: two distinct weak volumes
became one warning with one example path, which is the bury-it failure §108's "per filesystem" wording
exists to prevent. The hybrid keeps the property exactly where it is needed, on the variant that has no
volume to name.

**What remains, stated rather than implied.** Under the default, a cycle on a weak-identity filesystem
is copied up to 256 times before the cap stops it. That is a bounded, reported, cleanable mess — time,
bandwidth and destination space — where without the cap it would be unbounded and silent. `--safety=strict`
is what prevents the transfer entirely, and the warning names it.

**Rejected: varying the default by mount topology.** Degrade for network mounts, refuse for local ones,
on the reasoning that local weak identity implies a deeper fault. §107's own list refutes the premise —
FAT32 and exFAT head it, and both are ordinary local removable media — so this would refuse the most
common consumer backup there is. Detecting "is this a network mount" portably is also the same class of
unreliable platform introspection that produced the problem.

One adapter rule, from §107: *"The platform adapter must never treat `object_id == 0` as a valid
universal object identity."* An adapter that reads an index of zero reports `Unavailable`, never
`Strong(ObjectId { index: 0, .. })`.

## Composition with `copy_file`

`copy_tree` lives in `flux-core` beside `copy_file` and drives it.

**`copy_file` IS modified in this cut**, by the one change described under "Where the per-file check
lives, exactly": the new identity gate at Step 2a, and the `safety` field it reads. An earlier draft of
this section said `copy_file` was "not modified — it passed a capstone in PR #32 and its contract is
right as it stands". The capstone half is true and the conclusion does not follow: a capstone certifies
that the code behaves as designed, not that the design covered every case. This gap is one it did not
cover, and `TODO.md` had it recorded as open debt throughout. Nothing else about `copy_file` changes —
the step order above the gate, the staging contract, and the Step 7 re-check are all untouched.

```rust
/// What a tree copy did, and everything that went wrong while doing it.
///
/// A tree copy does not stop at the first failure, so unlike `copy_file` it reports
/// a SUMMARY rather than returning at the first error. `failures` being non-empty is
/// what makes the CLI exit 1.
#[derive(Debug)]
pub struct TreeOutcome {
    pub files_copied: u64,
    pub bytes_copied: u64,
    pub directories_created: u64,
    pub failures: Vec<TreeFailure>,
    /// Identity comparisons that were SKIPPED because a side was not `Strong`,
    /// aggregated for one warning apiece. Not failures: the copies succeeded.
    /// The engine captures; PR 4's CLI renders. Empty on the normal path.
    pub warnings: WeakIdentityWarnings,
}

#[derive(Debug)]
pub struct TreeFailure {
    /// Relative to the source root, as the walk reports it.
    pub path: std::path::PathBuf,
    pub cause: TreeFailureCause,
}

#[derive(Debug)]
pub enum TreeFailureCause {
    /// The walk itself could not read or enter something.
    Walk(FsError),
    /// A directory could not be created at the destination.
    CreateDir(FsError),
    /// One file's copy failed. Carries `CopyError` intact, so a leftover staging
    /// temporary is still reported per file.
    Copy(CopyError),
    /// A symlink or other non-regular entry, which this cut does not recreate.
    /// Carries WHAT it was, so the report can say so; the rendered code is always
    /// `SPECIAL_FILE_UNSUPPORTED`.
    Unsupported(FileType),
}

pub fn copy_tree<F: FileSystem>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
) -> std::result::Result<TreeOutcome, CopyError>;
```

The outer `Err` is reserved for failures of the operation as a whole — the source root missing or not a
directory, and the §129 safety rejection — because those mean no transfer happened at all. Everything
else lands in `failures` and the walk continues.

`opts.operation_id` is shared across every file, so each file's staging temporary is named from the same
id. That is what makes §18.1's leftover sweep work per file without the tree needing its own scheme.

**`opts.publish` passes through unchanged, and `copy_tree` does not force a mode.** The design
previously left this unstated, which is a gap rather than a detail, because the two modes differ in
whether a known race is reachable:

- `Publish::Replace` publishes through `rename_replace`. This is what both existing construction sites
  set, so it is the shipped behaviour.
- `Publish::NoReplace` publishes through `rename_no_replace`, which is **check-then-act**:
  `StdFileSystem` tests `std::fs::symlink_metadata(to).is_ok()` and then calls `std::fs::rename`
  (`crates/flux-platform/src/std_fs.rs:264-276`), so two processes can both see a free name and one
  silently wins — the very thing that method's contract forbids.

  The test is deliberately `symlink_metadata` rather than `exists()`, because `exists()` follows the
  link and a dangling symlink would report false, letting the method replace the very name it promises
  to leave alone. That fix is already in; it addresses a different defect and does not close the race,
  because any check followed by a separate rename has a window between them.

There is an argument for `NoReplace` being the more natural tree default, since §18.1 says a target
"planned as new" publishes that way, and every file of a copy into a fresh destination is planned as
new. It is not adopted here, and the reason is sequencing rather than taste: forcing `NoReplace` would
make that race **reachable for the first time**, and PR 3 would be the change that exposed it. Closing
it properly needs an atomic primitive — `rustix::fs::renameat_with` with `RenameFlags::NOREPLACE` on
Unix, `FileRenameInfoEx` without `REPLACE_IF_EXISTS` on Windows, which `std` does not expose — and that
is its own piece of work, recorded in `TODO.md` and deliberately out of this cut.

Passing through therefore keeps the race dormant exactly as it is today: no caller in this cut selects
`NoReplace`, so PR 3 neither fixes nor exposes it. When that primitive lands, a tree copy can adopt
`NoReplace` without a second decision.

**One failure, one representation.** Each `TreeFailureCause` variant is defined by WHERE the failure
happened, and no failure may be expressible two ways:

- `Walk` — the traversal could not read or enter something. Only the walk produces it.
- `CreateDir` — `create_dir` failed, or the path was occupied by a non-directory.
- `Copy` — `copy_file` returned an error. Only `copy_file` produces it.
- `Unsupported` — the walk typed the entry as something this cut does not recreate. `copy_file` is
  never called for it, so it can never arrive as `Copy(SpecialFileUnsupported)`.

That last rule is the one worth stating, because `copy_file` *would* return `SPECIAL_FILE_UNSUPPORTED`
for a symlink source if it were called — the walk already knows the type, so it is not called, and the
failure has exactly one home.

Two `Code` variants also sit close enough to confuse: `SourceChanged` is a FILE whose length or mtime
moved under `copy_file` (§33), while `DirectoryChangedDuringScan` is a DIRECTORY whose identity changed
under the walk (§149.4). Different objects, different producers, no overlap.

```
create dst_root if absent            // the walk emits no event for the root
for event in walk(fs, src_root) {
    Dir      -> fs.create_dir(dst_root.join(rel))        // pre-order
    File     -> copy_file(fs, src_root.join(rel), dst_root.join(rel), opts)
    Symlink  -> record Unsupported(Symlink)
    Other    -> record Unsupported(Other)
    DirEnd   -> nothing in this cut
    Err(e)   -> report, continue
}
```

The destination root is created by `copy_tree`, not by the walk, precisely because the walk emits no
event for its own root. Missing that would leave `flux copy /data /backup` writing every child into a
`/backup` that was never created. It is created **after** the §129 pre-flight check and before the walk
begins, so nothing is written to a destination that the safety check would have rejected.

A directory that already exists at the destination is not an error, because a tree copy onto an existing
tree is ordinary — but `AlreadyExists` **must not be treated as success on its own**. MEASURED:

```
mkdir on an existing FILE      -> "File exists"   exit 1
mkdir on an existing DIRECTORY -> "File exists"   exit 1
```

`EEXIST`, and so `ErrorKind::AlreadyExists`, is identical in both cases: the error alone cannot tell
you whether a directory or a *file* occupies the path. Treating it as success where a file sits there
would copy the whole subtree's children into a path that is not a directory, failing once per child,
far from the real cause.

So on `AlreadyExists`, `copy_tree` calls `metadata` on the destination path and continues only if it is
a directory. If it is not, that is one failure for that subtree, reported at the directory where it
actually went wrong, and the subtree is skipped. Any other `create_dir` failure skips the subtree and is
reported the same way.

`copy_tree` returns a summary — files copied, bytes copied, and every per-entry failure — rather than
stopping at the first failure. The CLI exits 1 if any failure was recorded, matching item 83 and the
existing `metadata_failures` behaviour.

### Directory metadata

**Not preserved in this cut.** Directories are created and keep their own fresh timestamps.

§30.2 constrains only *when* directory metadata may be finalized, so not applying it is vacuous rather
than violated. Applying it would need a path- or directory-handle-based `set_times`, and today
`set_times` and `set_permissions` take `&Self::Writer` — a file handle — by a deliberate design in PR
#32 that makes writing to a source a compile error. Widening that surface is a separate decision from
the walk, and it is the reason `DirEnd` is emitted now: when directory metadata arrives, the walker does
not change.

## The test fake

`FaultFs` cannot express this feature's tests as it stands. `fault_fs.rs:16` is
`files: HashMap<String, Vec<u8>>`:

- **`String` keys cannot hold a non-UTF-8 name**, which is precisely the §241 ordering case the whole
  ordering contract is about.
- **There is no directory concept at all.** Line 28's `not_files` set lumps every non-file together and
  line 331 reads `is_file: !g.not_files.contains(&p)`, so an empty directory is indistinguishable from a
  device node — and an empty directory is exactly what a tree copy must handle.

So `FaultFs` gains:

- keys widened from `String` to `PathBuf` across its inner maps,
- an explicit `directories: HashSet<PathBuf>` recording which paths are directories,
- `read_dir` yielding the immediate children found in `files` and `directories`, empty when a directory
  has none,
- `set_identity(path, FileIdentity)`, so cycle detection and the §129 check are testable without a real
  bind mount or any privilege — two paths are simply given the same `ObjectId`,
- fault injection for `read_dir` and `create_dir`, reusing the existing `fail` / `fail_kind` /
  `fail_nth` machinery.

The existing `fail_nth` exists because a one-shot fault aimed at a method the algorithm calls more than
once is eaten by the first call — that made a test pass vacuously once, and `read_dir` is called once
per directory, so the same trap is live here.

Ordering over non-UTF-8 names is also covered by a real-filesystem integration test on Unix, because a
fake that agrees with the implementation about encoding proves nothing about the OS.

## What stays unresolved

- **A single enormous directory — SETTLED: do the simplest thing.** `read_dir` returns an ordinary
  `Vec<DirEntry>` holding an `OsString` per entry. No packed representation, no spill to disk.

  MEASURED on 200,000 real files in one ext4 directory: `readdir` 448 ms, 3,699,984 total name bytes
  (18.5 per entry), bytewise sort 81 ms. A packed layout — all names in one buffer plus 8 bytes per
  entry — would cost 5.3 MB against 10.1 MB for the `OsString` form, a factor of **1.91**, plus it
  avoids 200,000 separate allocations.

  That factor was rejected as a reason to build it. At any realistic directory size the absolute
  numbers are irrelevant (10,000 entries is half a megabyte either way), and it only starts to matter at
  sizes where a constant factor no longer decides anything. Against that, packing would push a custom
  representation into the *trait's public return type*, which three implementors must satisfy and every
  future one after them. The simplest thing that works for every real directory is the right thing, and
  the measurement is recorded here so that a future change is an informed one rather than a rediscovery.

  **The justification for holding a directory at all is invariant 11 — and NOT §7.2.** Invariant 11
  forbids "an unbounded global list of discovered files"; one directory is not the total, so holding it
  is within the letter. It is tempting to read §7.2's "emits component-wise order without buffering" as
  blessing this, and that reading is wrong: that sentence is about ORDERING — it says no global reorder
  buffer is needed to achieve component-wise order — not about memory bounds. The distinction matters
  because §7.2 would equally appear to bless something genuinely unbounded.

  What stays genuinely unknown is where the cost stops being linear. The straight-line extrapolation to
  ten million entries (~505 MB, ~22 s) assumes linear scaling in both memory and `readdir`, and that
  assumption is untested; it is recorded as an extrapolation, not a measurement. The peer argued that
  kernel dentry-cache pressure and ext4's htree limits would break before Rust's allocator does, which
  is plausible and is NOT measured here either. Neither claim changes the decision, because the decision
  is to build nothing.
- **Mount-based cycles are FINITE — SETTLED, and the open question is closed.** MEASURED on kernel
  `6.6.87.2-microsoft-standard-WSL2`, with `mount --bind`, `mount --rbind`, and `--make-shared`
  followed by `--rbind`: all three reach depth 2 against a probe cap of 60, with source and mounted
  directory sharing a `dev:ino`, and no leftover mounts.

  A first probe could not distinguish "the mount is not re-applied" from "the inner directory happened
  to be empty", which the peer caught. Re-run with a real five-deep chain and a marker file inside the
  mount point: `a/b` lists `b`, while `a/b/b` lists the marker and the real chain, and `a/b/b/b` does
  not exist. So the underlying directory is reached with its own content, the mount is demonstrably not
  re-applied inside itself, and the recursion stops for that reason rather than for want of tree.

  This does NOT cover a server-side construction such as an SMB wide link, where a remote server
  resolves each request and can present an unbounded tree of ordinary directories. No local command can
  reach that case. It is precisely the weak-identity situation, where the depth cap is the guard.
- **`PATH_COMPONENT_INVALID`** (§103) arrives with the phase that constructs `FluxPathKey`s.

## Delivery

Four pull requests, in order. Each plan is written only once its predecessor has merged, because a plan
citing line numbers is a set of claims about code that must already exist.

1. **Object identity.** `ObjectId`, `FileIdentity`, `Metadata.identity`, and implementations in
   `StdFileSystem`, `FaultFs` and the `NullFs` test stub at `fs.rs:104`. No walker code. This is first
   because it carries the only genuine platform risk in the design — Windows `FILE_ID_INFO`, on a
   repository that lives on a ReFS Dev Drive where the 64-bit index is not unique — and both §149.4 and
   §129 depend on it.

   Adding a field to `Metadata` is a **breaking change to a public struct**: every construction site
   must name the new field. There are exactly three, all updated in this PR — `fs.rs:119`,
   `std_fs.rs:173` and `fault_fs.rs:329` — and `crates/flux-core/src/copy.rs` constructs none, so
   `copy_file` is untouched. The crate is pre-1.0 and unpublished, so no external consumer exists.

   **Windows.** `GetFileInformationByHandleEx(FileIdInfo)` needs a HANDLE, while `metadata` takes a
   `&Path`, so the adapter opens one itself. The flags are not a free choice, and std's own source
   (`sys/fs/windows.rs:1398-1404, 1501-1504`) settles all three:

   - `FILE_FLAG_BACKUP_SEMANTICS` — std's comment is "allows opening directories". Without it a
     directory cannot be opened at all, and directories are the entries the ancestor check exists for.
   - `FILE_FLAG_OPEN_REPARSE_POINT` — "opens a link instead of its target". Without it the adapter
     would read a symlink's TARGET identity, silently corrupting cycle detection with an identity that
     belongs to a different object.
   - `access_mode(0)` — std notes "No read or write permissions are necessary", which avoids both a
     sharing violation and a denial on a file this process may not read.

   `FILE_ID_INFO` yields `VolumeSerialNumber: u64` and `FileId: [u8; 16]`; the latter becomes `index`
   via `u128::from_le_bytes`. A volume that cannot supply it reports `Unavailable`, not a zero id.

   **Unix.** `st_dev` is `volume`; `st_ino` is a `u64` zero-extended into the `u128` `index`. Identity
   is `Strong` on ordinary local filesystems. It is reported `Weak` where §107 names the filesystem as
   unreliable and that is detectable, and `Unavailable` where `st_ino` is 0.

   **`NullFs`** returns `FileIdentity::Unavailable` — the stub exists to prove the trait compiles, and
   inventing an identity there would let a test pass against a fake that never had one.

   **`FaultFs`** gets `set_identity(path, FileIdentity)` so a test can give two paths the SAME
   `ObjectId` and reproduce the bind-mount cycle with no mount and no privileges. Paths with nothing set
   default to a distinct `Strong` id generated per path, so ordinary tests need no setup and no two
   unrelated paths collide by accident.
2. **The walk.** `FileType`, `DirEntry`, `read_dir`, `create_dir`, `Code::DirectoryChangedDuringScan`,
   the `FaultFs` widening, and the ordered walk with its event type, non-terminal errors, the depth cap,
   and ancestor-set cycle detection gated on both-sides-`Strong`.

   `FaultFs::set_identity` is what makes the degraded paths testable: giving two paths the same
   `ObjectId` reproduces a cycle, and giving one a `Weak` identity exercises the fallback — neither
   needs a mount, a privilege, or a particular filesystem under the test runner.
3. **`copy_tree`, the safe engine.** The driver, the lexical containment floor, the §129 pre-flight and
   dynamic identity checks, the `Safety` enum on `CopyOptions`, the CAPTURE of the aggregated
   weak-identity warning into `TreeOutcome::warnings`, and the per-file identity check described under
   "The hole the directory checks do not cover" above. **No CLI**, so nothing here is rendered — the
   warning is captured as data and displayed in PR 4.

   Split from the CLI after PR 2, with the owner's agreement. The first split proposed was traversal
   first and safety second, and it was rejected on its own consequence: a cut carrying the CLI without
   the §129 pre-flight would ship a user-reachable recursive copy that will happily copy `/data` into
   `/data/backup`, while a cut without the CLI ships nothing a user can run — which is what PR 2 already
   is. Splitting at the ENGINE boundary instead means every merged state is both safe and coherent: this
   one ends at a library that cannot be driven into an unsafe copy, and the next one exposes it.

   **Rejected: extracting the per-file identity check into its own PR before this one.** The argument
   for it is real — the check changes shipped SINGLE-FILE behaviour, and isolating a behaviour change is
   ordinarily right. It does not survive the coupling, because the check's degraded case is *defined* by
   `--safety=strict` and `Safety` lives on `CopyOptions` as part of this cut. An extracted PR would
   therefore have to either ship the check with no strict mode to control it, or ship a safety enum
   whose only meaningful consumer — the tree engine — does not exist yet. Both leave a merged state that
   is incoherent on its own terms, which is the exact property the engine-boundary split was chosen to
   guarantee.

4. **The CLI.** `flux copy` dispatching a directory source to `copy_tree`, the reporting of
   `TreeOutcome` — the counts and the per-entry failures — the rendering of
   `TreeOutcome::warnings` as one line per affected volume plus one for the `Unavailable` bucket, and
   the `--safety=strict` flag that sets `Safety::Strict`. A thin surface over an engine whose safety is
   already settled and reviewed.

## Out of scope

Following symlinks (§26, Future), recreating symlinks at the destination, hardlink topology (§12, §13),
directory metadata preservation, multiple source roots (§18.3), `FluxPathKey` materialisation (§103),
parallel workers and the bounded transfer queue (Phase 3), resume, and the `.flux` control directory
(§18.2).
