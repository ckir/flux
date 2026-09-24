# Directory walker and recursive copy — design

**Date:** 2026-09-23
**Status:** approved, not yet planned
**Scope:** the ordered directory walk, the filesystem primitives it needs, portable object identity,
and `copy_tree` driving the existing `copy_file`.

Delivered as **five pull requests**, and this document is the design for all five. It began as three;
the last was split at the engine boundary once PR 2 merged, and an atomic-publication prerequisite was
inserted ahead of the engine during the engine's own design review, when §241.5 and the
check-then-rename prohibition at `FLUX_FULL_UPDATED_SPEC_V16.md:10873` turned out to make it mandatory
rather than optional.

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
    /// AS MERGED IN PR 2, this replaced the `is_file: bool` this section originally
    /// proposed. `is_file == false` is equally true of a directory and of a symlink,
    /// while `read_dir` FOLLOWS a symlink to a directory — so `!is_file` cannot answer
    /// "is this a directory", which the anchor check and the `AlreadyExists` recovery
    /// both need it to answer.
    pub file_type: FileType,
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

**Neither PR 4 nor PR 5 consumes `DirEnd`; the walk emits it** — see "Directory metadata" below.

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
design rather than an omission from the implementation. It was found while scoping PR 4, after PR 1 and
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

**The position is load-bearing, not merely tidy, and the symlinked-anchor case shows why.** The staging
temporary is created in the DESTINATION's own directory — `temp_path` ends in
`target.with_file_name(name)` at `crates/flux-fs/src/options.rs:74-79`, and `copy_file` builds it from
`dst` at `crates/flux-core/src/copy.rs:146`. So when a destination path resolves back into the source
tree, as it does through a symlinked anchor, the temporary is created INSIDE THE TREE BEING READ. Any
temporary that lands in a directory the walk has not yet snapshotted is then enumerated and copied as if
it were source content.

A gate placed after Step 3 would refuse only after that had already happened. Refusing at Step 2a is
what makes the whole class of "the destination is really the source" reach zero filesystem mutations,
which is a stronger property than avoiding a broken hardlink and is the actual reason this gate earns
its cost.

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
| the stat fails for any **other** reason | Treat as a DEGRADED comparison, not as a clean pass: proceed by default and count it in the `Unavailable` bucket, refuse under strict. A destination that could not be inspected is a comparison that did not happen, which is the same thing a weak identity means. |

**The rows are evaluated IN ORDER, and `NotFound` short-circuits first.** This is not a presentational
detail. "Strict refuses whenever identity is not `Strong` on both sides" is true only of destinations
that EXIST: a `NotFound` destination has no identity at all, so read without the ordering the rule would
make `--safety=strict` refuse every copy into a fresh destination — and on FAT32 the source is not
`Strong` either, so it would refuse the ordinary removable-media backup twice over, turning the flag
into something nobody can leave on. `NotFound` is not a degraded comparison; it is a comparison that is
not needed, because there is no object to alias.

So the gate reads: **absent → proceed silently, under every `Safety` setting.** Everything below that
row concerns destinations that exist.

**One thing the gate cannot close, stated rather than implied.** The gate stats at Step 2a and the
rename happens at Step 7, so a destination that becomes an alias of the source *in between* is not
caught — the same check-then-act shape this document faults `rename_no_replace` for, and it would be
dishonest to name it there and pass over it here. The window is narrow and the damage is bounded to the
Q1 harm: a broken hardlink, not lost data, because the bytes published were read from the source before
the destination was touched. Closing it needs an identity comparison against the open destination handle
at publish time, which the current `rename_replace` surface does not expose. Not closed in this cut, and
not hidden either.

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

One consequence of the split is worth naming so PR 4 does not write a promise it cannot keep: the
warning's wording points the user at `--safety=strict`, and that flag does not exist until PR 5. PR 4
captures the FACT of the degradation and its counts; it composes no sentence. The sentence, and the
flag it names, land together.

#### Where `--safety=strict` lives

`CopyOptions` has no safety field today — it carries `preserve_times`, `preserve_permissions`,
`durability`, `publish` and `operation_id`. The engine cut adds one.

An earlier revision justified that by saying `copy_tree` "takes a single options parameter and that is
the only channel a caller has". That premise expired when the signature grew a failure sink, and it was
never the real reason anyway. **The real reason is that the field has to reach `copy_file`**, because
Q1's answer makes strict mode change single-file behaviour at the Step 2a gate, and `CopyOptions` is
the only thing `copy_file` receives. A parameter on `copy_tree` could not reach it.

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
both are updated in PR 4.

**The FLAG is PR 5; the enum and its behaviour are PR 4.** Wherever this document says
`--safety=strict`, it names the behaviour `Safety::Strict` selects, not a command-line surface PR 4
builds — PR 4 has no CLI. The flag that sets it is part of the CLI cut, and until then `Safety::Strict`
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

### A symlinked destination anchor

**The rule, first.** The CLI canonicalizes the source root and the destination anchor before calling
`copy_tree`. The engine then refuses outright, with `Code::SafetyRejected` and before anything is
created, if the destination anchor's `file_type` is `Symlink`. Canonicalization RESOLVES and is a
property of paths, so it lives in the CLI; refusal GUARDS and is a property of the operation, so it
lives in the engine. Neither does the other's job, and the engine's safety never depends on the caller
having been careful.

**Why a guard is needed at all.** `FileSystem::metadata` is `symlink_metadata`-based — that is the whole
surface, there is no following variant — so for `flux copy /data /backup` where `/backup` is a symlink
to `/data`, the pre-flight compares `/data`'s identity against the identity of the *link object*
`/backup`. Those never match, so it passes. The lexical floor does not save it either, because
`/backup` is not lexically inside `/data`. Without a rule here the operation begins, `create_dir` writes
THROUGH the link back into the source tree, and every file is copied onto itself — caught one at a time
by the Step 2a gate, which turns a copy that should have been refused once into one refusal per file.

**Why canonicalization is required, and is not optional.** §128 "Destination Safety"
(`FLUX_FULL_UPDATED_SPEC_V16.md:6095`) sits immediately before the §129 this design already cites:

> using both `canonical/normalized path analysis` and filesystem identity checks where available.
>
> Mount points, bind mounts, junctions, and symlink traversal must not invalidate the containment
> decision.
>
> The scanner must not be allowed to recursively discover Flux's own destination tree through an alias.

So identity is only the second of two required mechanisms, and "symlink traversal must not invalidate
the containment decision" names this exact case. Resolving `/backup` to `/data` makes the containment
test succeed lexically, with no identity comparison needed.

**Why canonicalization is in the CLI and not on the trait.** Three reasons, each of which independently
rules the trait out:

1. **It cannot be applied to the destination root, which usually does not exist.** Canonicalization
   resolves symlinks, which requires the path to be there. What is canonicalized is the **nearest
   existing ancestor** — the anchor the §129 pre-flight already uses — with the absent remainder
   appended lexically. That still satisfies §128, because components that do not exist cannot be
   symlinks.
2. **On Windows the canonical form gains a `\\?\` prefix the input did not have.** A containment test
   between one canonicalized path and one raw path would fail unconditionally, and would fail in the
   SAFE-LOOKING direction, since a failed containment test reads as "not contained". Both sides are
   canonicalized, or neither is.
3. **`FaultFs` could not implement it honestly.** It is an in-memory fake with no filesystem beneath it,
   and a `canonicalize` returning its input unchanged would make every test built on it pass while
   proving nothing. Implementing it properly means making the test double a path resolver with cycle
   detection.

**Why the engine still guards.** Putting canonicalization in the CLI repairs the CLI path and would
otherwise leave the LIBRARY path worse than before: `copy_tree` called directly with an unresolved
symlinked anchor passes the floor, passes the identity comparison, and proceeds — the exact O(N)
failure §129 requires be one refusal *before transfer begins*. The `Symlink` refusal closes that,
costs nothing because the anchor is already stat'd for the identity comparison, needs no
`canonicalize` on the trait, and is unit-testable against `FaultFs`, which can already produce a
`Symlink` anchor via `add_symlink`. A CLI user never sees the refusal, because the path was resolved
before the engine saw it.

Refusing satisfies §128 rather than evading it: declining to make a containment decision it cannot make
is not the same as making the wrong one.

**Rejected: treating a symlinked anchor as DEGRADED** — warning and continuing under the default,
refusing only under `Safety::Strict`. §128 rules it out directly, because warning and continuing is
precisely letting symlink traversal invalidate the containment decision.


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
means PR 4 has no printer, so "warn once per filesystem" has to survive as data until PR 5 can display
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

**`copy_file` needs a channel to report its own degradation, and today it has none.** The gate that
detects a weak identity lives in `copy_file`, but `copy_file` returns
`Outcome { bytes_copied, metadata_failures }` — nothing there can carry "the comparison was skipped".
Without a channel, Q1's "degrade and warn" would degrade silently, which is the fail-open behaviour the
warning exists to prevent. So `Outcome` is widened:

```rust
pub struct Outcome {
    pub bytes_copied: u64,
    /// Empty on a clean copy. Non-empty means published-with-complaints.
    pub metadata_failures: Vec<MetadataFailure>,
    /// `Some` when the Step 2a gate could not compare and fell back to the lexical
    /// floor. Carries the WEAKER of the two sides, which is exactly what the
    /// aggregation keys on: `Weak(id)` contributes `id.volume`, `Unavailable`
    /// contributes to the catch-all. `None` on the normal path AND when the
    /// destination was `NotFound`, which is not a degradation.
    pub identity_degraded: Option<FileIdentity>,
}
```

`copy_tree` folds each file's `identity_degraded` into `TreeOutcome::warnings`, so the aggregation lives
where the counting can happen and the detection stays where the stat is. This also gives the SINGLE-file
path a warning it can render in PR 5 — `flux copy a b` onto removable media degrades too, and there is
no `TreeOutcome` in that call.

This widens a public struct, so every construction site must name the new field. There are exactly two:
`crates/flux-core/src/copy.rs:245` and the test at `crates/flux-fs/src/options.rs:95`.

**`DegradedGroup::example` is relative to the source root**, the same frame `TreeFailure::path` uses, so
a renderer never has to ask which of two conventions a path came from. For the single-file path there is
no root to be relative to, so PR 5 renders the path the user supplied.

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
/// What a tree copy did, counted. Individual failures are STREAMED to the sink as
/// they happen, not accumulated here — see "Failures must not grow without limit".
///
/// A tree copy does not stop at the first failure, so unlike `copy_file` it reports
/// a SUMMARY rather than returning at the first error. A non-empty `failures`
/// tally is what makes the CLI exit 1.
#[derive(Debug)]
pub struct TreeOutcome {
    pub files_copied: u64,
    pub bytes_copied: u64,
    pub directories_created: u64,
    /// Failures broken down by cause — one counter per `TreeFailureCause`
    /// variant. Bounded by construction: counters, never a list, so a tree with
    /// millions of failures costs the same as a clean one.
    ///
    /// The TYPE has to survive even though the records do not, because §2 item 83
    /// makes `DirectoryChangedDuringScan` an exit-1 condition specifically — a
    /// single undifferentiated total could not answer that.
    ///
    /// There is deliberately NO separate `failures: u64` field beside this. An
    /// earlier revision had both, which is denormalized state that two code paths
    /// can disagree about; the total is `tally.total()`, derived, with one source
    /// of truth.
    pub failures: FailureTally,
    /// Identity comparisons that were SKIPPED because a side was not `Strong`,
    /// aggregated for one warning apiece. Not failures: the copies succeeded.
    /// The engine captures; PR 5's CLI renders. Empty on the normal path.
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

/// One counter per `TreeFailureCause` variant. Fixed size, so it costs the same
/// on a clean tree and on one where every entry failed.
///
/// It is incremented by an EXHAUSTIVE `match` on `TreeFailureCause` with no
/// wildcard arm. That is load-bearing rather than stylistic: with a `_ =>` arm,
/// adding a fifth cause later would compile and silently count nothing, and the
/// miscount would show up as a wrong exit code rather than as an error. Without
/// one, the compiler names every site that must be updated.
#[derive(Debug, Default)]
pub struct FailureTally {
    pub walk: u64,
    pub create_dir: u64,
    pub copy: u64,
    pub unsupported: u64,
}

impl FailureTally {
    /// The only total. Derived, never stored alongside the parts.
    pub fn total(&self) -> u64 {
        self.walk + self.create_dir + self.copy + self.unsupported
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

pub fn copy_tree<F: FileSystem>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<TreeOutcome, CopyError>;
```

The outer `Err` is reserved for failures of the operation as a whole — the source root missing or not a
directory, the §129 safety rejection, and `NOREPLACE_PUBLISH_UNAVAILABLE` from the pre-flight probe —
because those mean no transfer happened at all. Everything else goes to `on_failure` and the walk
continues.

**`Code::SafetyRejected` now arrives from two levels, and they resolve differently.** The per-file gate
returns the same code as the pre-flight, so the code alone no longer says whether the operation should
stop:

- the **pre-flight and the per-directory** rejection abort the whole operation as the outer `Err`. They
  mean the roots themselves overlap, so every remaining file is suspect and continuing would compound
  the damage.
- a **per-file** rejection is ONE file's failure. It goes to `on_failure` as
  `TreeFailureCause::Copy(CopyError)`, increments the tally, and the walk continues, like any other
  per-file error.

That asymmetry is deliberate. A single aliased file inside an otherwise sound tree is a reason to skip
that file and report it, not to abandon a copy that may be most of the way through ten thousand others —
and the file is left untouched either way, so continuing destroys nothing. The rule to implement is
therefore positional, not code-based: **what aborts is WHERE the rejection came from, never its `Code`.**

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

**CORRECTION — the paragraph that stood here was wrong, and wrong in the way that matters most.** It
argued `NoReplace` was merely "the more natural tree default", cited **§18.1** for "planned as new", and
concluded that passing through was safe because nothing in this cut would select `NoReplace`.

§18.1 says no such thing. It is `FLUX_FULL_UPDATED_SPEC_V16.md:1348-1367` and is *exclusively* about
naming — `<target>.flux-partial.<operation-id>` and `<target>.flux-state.<operation-id>` — with no
publication semantics in it at all. The authority is **§241.5 Collision Detection** at `:10799`, and it
does not offer a preference. It requires:

> A target planned as new (absent at planning) is published with a primitive that refuses to replace an
> existing entry: `renameat2(RENAME_NOREPLACE)`, `renamex_np(RENAME_EXCL)`, `MoveFileEx` without
> `MOVEFILE_REPLACE_EXISTING`, or `link()` to the target followed by `unlink()` of the temporary name.
> If the name exists by then, the entry appeared after planning ... and Flux reports
> `DESTINATION_NAMESPACE_COLLISION` without replacing it.

The single-file pipeline carries the same requirement inline at `:1990` — *"ATOMIC RENAME (no-replace
when the target was planned as new; Section 241.5)"*.

**So `NoReplace` is not a taste call for a tree copy — it is mandatory.** Every file of a copy into a
fresh destination is absent at planning. Three things follow that the previous text got backwards:

1. `copy_tree` must publish new targets with a refusing primitive. Pass-through to whatever the caller
   set, which in practice is `Replace`, is not compliant.
2. A name that exists at publish time is **not** an ordinary overwrite. It is
   `DESTINATION_NAMESPACE_COLLISION`, a code this design did not previously mention anywhere.
3. §241.5 names the platform primitives itself, and its list independently confirms the platform
   correction made elsewhere in this document: `renameat2(RENAME_NOREPLACE)` and
   `renamex_np(RENAME_EXCL)` are listed as *different* primitives, so treating the Linux flag as
   covering "Unix" was wrong against the normative text as well as against macOS.

What remains genuinely open is the IMPLEMENTATION, not the semantics — see the fork recorded below.
Closing the RACE properly needs an atomic primitive, and §241.5 names **three** of them, not two:

- **Linux:** `renameat2` with `RENAME_NOREPLACE`, reachable as `rustix::fs::renameat_with` with
  `RenameFlags::NOREPLACE`.
- **macOS:** NOT that. macOS is a Unix platform without `renameat2`; the equivalent is `renamex_np` with
  `RENAME_EXCL`. An earlier draft of this paragraph said the `rustix` call covers "Unix", which is the
  same `cfg(unix)`-means-Linux mistake that already cost this project a macOS CI failure once — see the
  non-UTF-8 filename case, where `cfg(unix)` was wrongly read as "permits arbitrary bytes" and macOS
  refused the name outright. NOT independently verified here: this machine has no macOS to measure on,
  so whoever implements it confirms the primitive before relying on this line.
- **Windows:** `FileRenameInfoEx` without `REPLACE_IF_EXISTS`, which `std` does not expose.

**The open fork, for the owner.** The SEMANTICS are settled by §241.5 and not in question. What is open
is how PR 4 supplies them, because the trait method that expresses them today is not atomic:
`rename_no_replace` tests `symlink_metadata(to).is_ok()` and then renames
(`crates/flux-platform/src/std_fs.rs:264-276`), so two processes can both see a free name.

- **(a) is STRUCK — the spec forbids it by name.** It was: use `NoReplace` now on the existing
  check-then-act implementation and document the window. `FLUX_FULL_UPDATED_SPEC_V16.md:10873-10876`
  closes it: *"Before a directory operation changes anything, Flux probes the destination for one of
  these primitives. If none is available, the operation is refused with `NOREPLACE_PUBLISH_UNAVAILABLE`
  (exit code 3); check-then-rename is never used as a substitute."*
- **(b) Implement the atomic primitive in this cut.** Fully compliant, but it pulls three platform
  implementations into a PR whose subject is tree safety, and `std` exposes none of them.
- **(c) CHOSEN by the owner — a prerequisite PR before this one**, scoped exclusively to atomic
  no-replace publication in `flux-platform`: the three primitives, their platform tests, and the probe.
  The tree engine then rebases onto it and simply calls `rename_no_replace`, knowing it is atomic.

  Three reasons it wins over (b). It repeats a shape this project has already executed successfully —
  PR 1 landed the object-identity primitive before the consumer that needed it, and that sequencing is
  why PR 2 and this design could be written against something real rather than something planned. It
  fixes a defect that exists **today**: `rename_no_replace` is racy right now, and the fix benefits the
  single-file path whether or not a tree copy is ever built on it. And the engine cut has grown
  materially during review — it now carries the Step 2a gate, the `Safety` enum, the warning
  aggregation, the probe, exit 3 and the capped report — so adding three FFI implementations with no
  `std` exposure is what would make it unreviewable.

Passing `Replace` through is no longer among the options either. The previous text reasoned that "no
caller in this cut selects `NoReplace`, so PR 4 neither fixes nor exposes it" — true about the code, and
irrelevant, because §241.5 makes selecting it mandatory rather than optional.

**Two obligations fall out of that spec text regardless of which option is chosen, and this design
carried neither of them.**

1. **A PROBE, before anything changes.** `copy_tree` must establish that the destination supports a
   no-replace primitive before it creates so much as the destination root, and refuse with
   `NOREPLACE_PUBLISH_UNAVAILABLE` and **exit 3** when it does not. That is a third outer-`Err` cause
   alongside the missing source root and the §129 rejection, and exit 3 is a code this design had never
   mentioned — the CLI contract so far was 0 and 1.
2. **The blast radius is DIRECTORY operations only.** Item 113 ends *"a single-file operation against
   the same destination is unaffected"*, so `copy_file` called on its own keeps exactly today's
   behaviour and needs no probe. The obligation attaches to `copy_tree`, which is what makes it PR 4's
   problem rather than a change rippling through the merged single-file path.

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

**How the subtree is skipped, since `Walk` has no pruning API.** This needs saying, because "skip the
subtree" reads like a call to a method that does not exist: `crates/flux-core/src/walk.rs` exposes
exactly two public functions, `walk` and `walk_with_depth`, both constructors. There is no
`skip_current_dir`, and the loop above is a `for` loop, which moves the iterator into a hidden local the
body cannot name — so even adding one would not help without also rewriting the loop as `while let`.

`copy_tree` therefore skips **consumer-side**: it keeps a set of relative prefixes whose directory
failed, and drops any later event whose path starts with one of them, popping a prefix when its `DirEnd`
arrives. The walker still descends and still reads those source directories; what is prevented is the
copy attempt.

That is the point. Without it, a `create_dir` that failed because a FILE occupies the destination path
would let every descendant reach `copy_file`, and each one would fail against a non-directory parent —
one failure per descendant, all of them far from the real cause, for a fault the design says is reported
once. The wasted `read_dir` calls are the price of not widening a just-merged public API, and they are
bounded by the failed subtree rather than the tree.

Adding a pruning method to `Walk` is the alternative. It is not taken here: `Walk`'s surface was settled
and reviewed in PR 2, this is the only consumer that would use pruning, and a consumer-side skip needs
no trait change, no new test surface in three implementors, and no second way for a walk to end.

`copy_tree` returns a summary — files copied, bytes copied, and failure counts — and streams each
failure to the sink rather than stopping at the first. The CLI exits 1 if any failure was reported,
matching §2 item 83 and the existing `metadata_failures` behaviour.

### The destination may rename what you asked it to create

**§241.4 "Destination Normalization" (`FLUX_FULL_UPDATED_SPEC_V16.md:10778-10797`) binds this design and
was never cited:**

> A destination filesystem may itself normalize names. ... source `FluxPathKey` → destination native
> path → destination filesystem namespace is a mapping, not a promise that source pathname bytes
> survive physically unchanged.
>
> After creation, Flux must validate the destination object using destination filesystem identity and
> namespace lookup rather than assuming byte-for-byte pathname preservation.

A tree copy assumes throughout that `dst_root.join(rel)` names the thing it just created. On a
destination that normalizes — macOS decomposing to NFD, a case-insensitive volume folding `File.txt`
and `file.txt` — that assumption is false, and §241.4 says so in as many words.

**An earlier revision claimed directories already satisfied this via the item-114 capture. They do
not.** The capture stats `dst_root.join(rel)` — the INTENDED path — so it is still asking the
destination about bytes it assumed survived. §241.4 forbids exactly that assumption. The two
requirements look alike and are not the same: item 114 asks "is this still the object I created?", and
§241.4 asks "is the object I created reachable by the name I used?". The capture answers the first and
presupposes the second.

**What actually satisfies it is that the presupposition is CHECKED rather than trusted**, and the two
ways it can fail need different treatment:

- **The name was normalized and no longer resolves** — the post-creation stat returns `NotFound`. That
  is the discrepancy detected, not an assumption vindicated: it fails that entry with
  `DESTINATION_ERROR` rather than proceeding on a path that names nothing.
- **The name was FOLDED onto an existing entry** — the destination is case-insensitive, `file.txt` is
  already there, and creating `File.txt` resolves to it. The stat succeeds and returns the OTHER
  object's identity. Nothing is `NotFound` and nothing looks wrong.

The second is the dangerous one, and it is item 83's case seen from the directory side. For FILES the
no-replace primitive catches it: the second rename finds the name taken and fails rather than replacing,
which is what makes that primitive load-bearing for correctness rather than only for concurrency.
**For DIRECTORIES there was no such guard**, because `create_dir` returning `AlreadyExists` is treated
as ordinary — a tree copy onto an existing tree is normal, and the design continues if the occupant is
a directory. That rule is right for a genuine pre-existing directory and wrong for two source
directories the destination folds into one, which would silently merge their contents.

So the `AlreadyExists` path gains a discriminator: **if the occupying directory was created by THIS
operation, the names collided**, and that entry fails with `DESTINATION_NAMESPACE_COLLISION` rather
than merging. The set of directories this operation created is already held — the item-114 capture
keeps exactly that, one identity per created directory — so the check costs a lookup in a set that
exists for another reason. A directory present before the operation started is not in that set and is
treated as before.

**The discriminator is `Strong`-only, like every other identity comparison here.** A `Weak` id is by
definition one that may not distinguish two objects, so a set keyed on weak ids can report a
pre-existing directory as one this operation created — turning an ordinary copy onto an existing tree
into a spurious collision and abandoning a subtree that was fine. `Unavailable` carries no id at all
and cannot be in the set. So a directory whose identity is not `Strong` on both sides is not entered
into the created set and is not tested against it: `AlreadyExists` keeps its old meaning and the copy
merges, with the degradation recorded on the warning channel.

**What that costs is worth naming, because it is the least comfortable trade in this design.** Weak
identity and case-insensitivity correlate — FAT32 and exFAT are both, and they head §107's list — so
precisely where folding is most likely, the guard is least able to fire. Two source directories
differing only in case then merge silently on a FAT32 destination. `Safety::Strict` refuses instead,
which is the honest lever, and it is the same lever every other degraded check here offers. Pretending
a weak id could carry this check would be worse: it would fail in the opposite direction, aborting
subtrees at random on the filesystems people actually back up to.

**For FILES it is not satisfied**, and the case is item 83's: copying a directory holding `File.txt` and
`file.txt` to a case-insensitive destination publishes one and must report
`DESTINATION_NAMESPACE_COLLISION` for the other, with nothing overwritten. Publishing with a no-replace
primitive is what detects it — the second rename finds the name taken and fails rather than replacing —
so the mechanism is already in the design, arriving with the atomic-publication cut. What was missing is
the statement that this is WHY the no-replace primitive is load-bearing for correctness and not only for
concurrency: without it, two source names that the destination folds into one would silently become one
file, the second overwriting the first.

That also answers a question the deferral leaves open. Until the atomic primitive lands, a
case-insensitive destination is a correctness hazard, not merely a concurrency one — which is a second
reason the primitive precedes the engine rather than shipping alongside it.

### Long destination paths on Windows

Acceptance item 103 (`FLUX_FULL_UPDATED_SPEC_V16.md:13369-13371`): *"on Windows, a destination path
longer than 260 characters succeeds through the extended-length call path; a path the destination still
refuses as too long fails that action with `DESTINATION_ERROR`."*

This binds a tree copy specifically, because a tree copy is what GENERATES long destination paths — the
user types two roots and the walk produces every path beneath them. The design discussed `PATH_MAX` at
length for the depth cap, but that measurement was made on Linux and answers a different question. It
said nothing about Windows `MAX_PATH`, which is 260.

Two halves, and only one of them is already handled:

- **The extended-length call path, and it belongs in the ADAPTER, not the CLI.** An earlier revision
  said the CLI's canonicalization satisfies this because `std::fs::canonicalize` returns a
  `\\?\`-prefixed path, and accepted that a library caller passing a raw path "does not get it". **§105
  forbids that.** `FLUX_FULL_UPDATED_SPEC_V16.md:5172-5174`: *"Flux uses extended-length paths (`\\?\`)
  for **every** filesystem call on Windows, so the legacy 260-character path limit never applies."*

  Every call, not every CLI call. So `StdFileSystem` is what ensures the prefix, on the Windows arm,
  for every path it touches — which is the right home regardless of the spec, because it makes the
  property true for all five cuts and every caller rather than for one entry point. The CLI's
  canonicalization still produces such paths, and that is now a harmless overlap rather than the
  mechanism.

  **The adapter must do this LEXICALLY, and must not reach for `canonicalize`.** That is the obvious
  implementation and it would break this design at its foundation: `std::fs::canonicalize` RESOLVES
  symlinks, so a `metadata` call routed through it would report a link's target instead of the link,
  destroying the `symlink_metadata` semantics that the Step 2a gate, the symlinked-anchor refusal and
  the item-114 capture all depend on. It would also make `rename_no_replace` act on a target rather
  than the name it promises to leave alone.

  **But a purely LEXICAL conversion is also wrong, and the previous revision of this paragraph asked
  for one.** It said to "resolve `.` and `..` textually". Textual `..` is not semantics-preserving:
  if `link` points at `/tmp`, then `link/../b` physically names `/b`, while popping `link` lexically
  yields `./b`. And a `\\?\` path *disables* the kernel's own parsing, so the wrong answer is then
  taken literally instead of being corrected. Asking for a lexical resolve would have produced silent
  reads and writes of the wrong file whenever `..` followed a link.

  **The correct conversion resolves the PARENT and appends the final component verbatim.**
  `canonicalize` the parent directory — physically correct, so `..` and intermediate links resolve the
  way the kernel would resolve them, and the result already carries `\\?\` — then push the final
  component unchanged. That preserves exactly what this design needs: the final component is never
  resolved, so `metadata` still reports a link as a link, `rename_no_replace` still judges the name
  rather than its target, and the Step 2a gate, the symlinked-anchor refusal and the item-114 capture
  all keep their meaning. A path already prefixed is left alone.

  **Three preconditions make that rule total rather than mostly-right, and the first two were missing
  from an earlier revision:**

  1. **Make the path ABSOLUTE first.** `Path::new("foo").parent()` is `Some("")`, and canonicalizing
     the empty string fails — so a single-component relative path would have failed unconditionally.
     Joining the current directory first is a pure textual step that resolves no links.
  2. **A path with no parent, or whose final component is `.` or `..`, is canonicalized WHOLE.** A root
     has no final component to protect. And a trailing `..` must not be appended verbatim: a `\\?\`
     path suppresses the kernel's parsing, so the filesystem would be asked for a literal entry named
     `..` and answer `InvalidName`. Neither `.` nor `..` names an object whose link-ness matters, so
     resolving them costs nothing this rule exists to protect.
  3. **A path already carrying `\\?\` or `\\.\` is returned unchanged**, which makes the conversion
     idempotent and safe to apply at every call site without tracking whether it has run.

  The parent must exist, which it does at every call site: the engine creates parents before children
  and the roots are resolved at startup. Where it does not, the call was going to fail with `NotFound`
  and it still does — the error names the parent rather than the target, which is more accurate rather
  than less.

  `rename_no_replace` converts both endpoints independently. They share a parent in every use this
  design makes of it, so an implementation may convert once and reuse; that is an optimisation, not a
  requirement, and correctness does not depend on noticing it.

  So the rule is narrow enough to state in one line: **resolve the parent physically, never the final
  component.** An implementer who canonicalizes the WHOLE path silently disables three safety checks;
  one who resolves it purely lexically silently retargets paths containing `..`. Both produce the
  right-looking string.

  This also removes the caveat the previous text was honest about but should not have needed: there is
  no longer a class of caller that silently gets the 260-character limit.
- **The refusal.** A path the destination still rejects as too long must fail THAT ACTION with
  `DESTINATION_ERROR` — one entry's failure, not the operation's. The design had never named that code.
  It maps as `TreeFailureCause::Copy` or `CreateDir` carrying it, so the walk continues, which is what
  "fails that action" requires.

### Mount boundaries are not crossed by default

**§42 "Mount Boundaries" (`FLUX_FULL_UPDATED_SPEC_V16.md:2495-2505`) is normative and this design had
never cited it:**

> Default: `do not cross filesystem boundaries`
>
> Crossing is off unless `--cross-filesystems` is given (Section 5).
>
> Filesystem identity must be used to evaluate boundaries.

Acceptance item 101 (`:13361-13363`) repeats it as a default: *"with no options given, `--atomic`
behaves as auto, `--durability` as normal, `--resume-verify` as chunks, and filesystem boundaries are
not crossed."*

**As written, the walk violates this.** It descends into anything whose `file_type` is `Dir`, and a
mount point inside the source is a directory. A copy of `/` would descend into `/proc`, `/sys`, every
removable device and every network mount — which is not a corner case, it is what happens the first
time someone copies a home directory containing a mounted volume.

**The check is one comparison and the primitive is already there.** `ObjectId` carries `volume` —
`st_dev` on Unix, `VolumeSerialNumber` on Windows — so the walk records the ROOT's volume at startup
and, before descending into a directory, compares. A different volume means a mount boundary. §42's
closing line asks for exactly this: *"Filesystem identity must be used to evaluate boundaries"*, which
is the primitive cut 1 delivered and that this design had not connected to the rule requiring it.

- **Default: do not descend.** The directory itself is still created at the destination — it exists in
  the source and the namespace should match — but its contents are not traversed.

  **It is REPORTED but it is NOT a failure, and an earlier revision got this wrong** by routing it to
  `on_failure`. That would make every backup of a home directory containing a mounted volume exit 1,
  which is the operation correctly obeying its own default policy being reported as having gone wrong.
  §55 settles it: exit 0 is *"success, including outcomes degraded under an auto policy (they are
  reported, Section 51)"* — reported, and still success.

  So it travels on the same channel as the weak-identity warning rather than the failure sink:
  `TreeOutcome` carries a bounded `boundaries_skipped` — a count plus one example path, the
  `DegradedGroup` shape, bounded by construction — and the exit code is untouched. `--cross-filesystems`
  makes the count zero by descending.
- **`--cross-filesystems` turns it off**, per §42 and §5.
- **Where identity is not `Strong` on both sides**, the volume cannot be compared and the boundary
  cannot be evaluated. It degrades by the rule already established — descend, warn once per
  filesystem — because refusing to traverse anything on a FAT32 source would be worse than crossing a
  boundary. `Safety::Strict` refuses, as everywhere else.

This is a per-directory check on a value the walk already holds, so it costs nothing beyond the
comparison.

**And it inherits a window that lets it be bypassed, which has to be said here because this section is
where a reader will assume the boundary is enforced.** The walk stats a directory and then calls
`read_dir` on it, and those are two operations. PR 2's own code says so at
`crates/flux-core/src/walk.rs:314-321`: *"This NARROWS the race, it does not close it: the window moves
from read_dir-to-metadata down to metadata-to-read_dir."*

What that comment does not say is the CONSEQUENCE, and the consequence is worse than a wrongly typed entry:

1. the walk stats `a`, finds a `Dir` on the root's volume — the boundary check passes;
2. the attacker replaces `a` with a symlink to `/etc`, needing no privilege beyond write access to the
   source;
3. the walk calls `read_dir` on it, which FOLLOWS the link and enumerates `/etc`;
4. the walk yields `File("a/passwd")`, and `copy_file` stats `src_root.join("a/passwd")`. **`lstat`
   spares only the FINAL component** — the intermediate `a` is resolved — so it stats `/etc/passwd`,
   sees an ordinary file, and copies it into the destination.

Flux reads outside the source root and publishes what it finds, having checked the boundary against an
object that no longer exists by the time it traverses it.

**Mitigation, and it is the same shape as the destination-side fix.** §149.4 requires that *"the
scanner must verify that the object being entered remains consistent with the planned directory
identity"* — *remains*, which is a before-and-after claim, not a single stat. So the walk re-verifies
the directory's identity AFTER `read_dir` and before yielding any of its entries; on a mismatch it
discards the listing entirely and raises `DirectoryChangedDuringScan`, which is exactly the outcome
§149.4 and §2 item 83 already prescribe for that code. Because `ObjectId` carries `volume`, the same
re-check covers the mount boundary at no extra cost.

**Where the re-check sits is load-bearing, so it is stated rather than left to the implementer.** It
goes inside `Walk::enter`, after `read_dir` and **before the frame is pushed and before the `Dir` event
is returned** — `crates/flux-core/src/walk.rs:372-374` pushes the frame with `emit_end: true` and then
returns `WalkEvent::Dir`, in that order.

That ordering is what makes the discard safe. A review round objected that discarding a listing would
desynchronise the consumer's path stack, because `copy_tree` pushes on `Dir` and pops on `DirEnd`, so a
`Dir` with no matching `DirEnd` would send every later file into the wrong directory. The objection is
correct about the hazard and wrong about this design: failing inside `enter` means no frame is pushed
and no `Dir` is ever emitted, so there is nothing to pop and no `DirEnd` is owed. The consumer never
learns the directory existed.

Putting the re-check anywhere later — after the `Dir` event, say — would make the objection right.

**What this does and does not buy, stated as plainly as the code comment states it.** It narrows the
window from "stat, then trust indefinitely" to "stat, list, re-stat" — an attacker must now win a race
twice, against a check that discards its own work on suspicion. It does not close it. Closing it needs
handle-relative traversal — `openat` with `O_NOFOLLOW`, listing through a directory file descriptor
that cannot be re-pointed — which `TODO.md` already records as the fix for walker TOCTOU and which is
not in these cuts. The destination side has the identical residual for the identical reason, and both
are closed by the same change.

### A destination directory swapped after Flux created it

**This is a reachable way to make Flux write OUTSIDE the destination root, and the spec names it.**
`FLUX_FULL_UPDATED_SPEC_V16.md:13411-13414`, item 114:

> replacing a directory that Flux created under DEST with a symlink to a location outside DEST, between
> its creation and the writing of its descendants, makes those actions fail with `SAFETY_REJECTED`;
> nothing is written outside DEST, and the rest of the operation continues.

The attack needs no privileges beyond write access to the destination tree:

1. the walk emits `Dir("a")`, and `copy_tree` creates `dst_root/a`;
2. the attacker removes `dst_root/a` and puts a symlink to `/etc` in its place;
3. the walk emits `File("a/payload")`, and `copy_tree` computes `dst_root.join("a/payload")`;
4. every check passes. Step 2a stats the destination, finds `NotFound` — correctly, nothing is there —
   and proceeds. Step 3 creates the staging temporary in `/etc`, and Step 7 publishes `/etc/payload`.

Nothing in the design as written stops it. The §129 pre-flight ran before the walk and compared roots
that were correct at the time. The lexical floor compares the path Flux *intended*, and that path is
inside `dst_root`; the kernel resolves the symlink afterwards. The Step 2a gate compares the source
against whatever is at the destination, and at the destination there is nothing. **Every guard this
design has is asking a question the attack does not answer falsely.**

**The mechanism: a destination directory's identity is captured when Flux creates or accepts it, and
re-verified before anything is written inside it.** `copy_tree` already tracks the current directory —
it pops on `DirEnd` and keeps failed prefixes — so it holds the `ObjectId` of each destination
directory on the same stack. Before publishing into a directory, it stats that directory and compares.
On a mismatch the write fails with `Code::SafetyRejected` and nothing is created; by the positional
rule this is a per-entry failure, so it goes to `on_failure` and **the walk continues**, which is what
item 114's closing clause requires.

**The capture asserts the TYPE as well as recording the id, and that is load-bearing.** A review round
argued this fix defeats itself: swap the directory for a symlink in the instant between `create_dir`
and the capturing stat, and Flux would record the symlink TARGET's identity as the expected one, then
match it forever after. That does not happen, and the reason is a property already measured in this
design — `FileSystem::metadata` does not follow links. It is `symlink_metadata` on Unix
(`crates/flux-platform/src/std_fs.rs:172`, whose comment says the following variant "would report the
TARGET as a regular file") and opens with `FILE_FLAG_OPEN_REPARSE_POINT` on Windows. So a symlink
swapped in before the capture is typed `Symlink`, carrying the LINK's identity, not the target's.

The capture therefore requires `file_type == Dir`. Anything else at that moment IS the item-114 attack,
caught one step earlier than the re-verification would have caught it, and refused the same way. Recording
an id without checking the type is what would have made the objection correct.

Identity is what makes this checkable at all — a path comparison cannot see the swap, because the path
did not change. Where identity is not `Strong` on both sides the check degrades by the rule already
established, and `Safety::Strict` refuses; that is the same trade §107 forces everywhere else, and it
is stated rather than hidden.

**This mitigation is a DIVERGENCE from the specified mechanism, not a complete implementation of it.**
Earlier revisions of this section called handle-relative traversal "the complete fix" and placed it
outside the cut, as though it were optional hardening that `TODO.md` might get to. It is not optional.
**§149.7 "Destination-Side Resolution" (`FLUX_FULL_UPDATED_SPEC_V16.md:7190-7210`) specifies it
outright:**

> `DEST` itself is resolved once, at start. Below it, the writer creates and opens every destination
> entry relative to a directory handle it holds for the parent, reached by walking down from `DEST`'s
> root without following links (POSIX: `openat` with `O_NOFOLLOW` and `O_DIRECTORY`; Windows:
> handle-relative opens that do not follow reparse points).
>
> If a component under `DEST` is a symlink, junction, or other reparse point that this operation did
> not create, that path is rejected: its actions fail with `SAFETY_REJECTED` and are reported, and the
> rest of the operation continues (exit status 1, Section 55).

The second paragraph is what this design already does — per-path rejection with `SAFETY_REJECTED`, the
walk continuing, exit 1 — so the OUTCOME is right. The first paragraph is the MECHANISM, and this
design substitutes a different one: stat the parent, remember its identity, re-verify before writing.

**The owner has ruled to accept that divergence**, on the same grounds as the probe deferral: a
handle-relative surface means `openat`-style APIs across all three `FileSystem` implementors plus
Windows handle-relative opens, reshaping a trait every existing consumer depends on, and that is a
larger and riskier body of work than the engine cut it would sit beneath.

**What it costs, stated rather than buried.** Identity re-verification is strictly weaker than a
directory handle. A handle cannot be re-pointed, so the window closes; a re-check narrows the window
and no more, because the check and the write remain two operations against a path the kernel re-resolves
each time. An attacker must now win a race rather than simply not be looked for, which is a real
improvement and is not compliance. **§149.7 is recorded as unmet tracked debt** alongside item 113, and
when the handle-relative surface lands, this mitigation is deleted rather than kept — two mechanisms
for one guarantee would leave a reader unsure which is authoritative.

### What all of this costs

Fifteen rounds of adding guards earns an accounting, because a reader who cannot tell which check is
load-bearing will delete the wrong one.

**Per DIRECTORY:** one source stat before entering, one source re-verify after `read_dir`, one
`create_dir`, and one destination stat capturing the created directory's identity. Plus a volume
comparison, which is arithmetic on a value already in hand.

**Per FILE:** one source stat (Step 2), one destination stat (Step 2a), one destination-parent stat
before publishing, the `create_new`, the stream, and the publish-path stat the adapter already
performed before any of this design existed. On Windows, one parent resolution per adapter call.

**The walk does NOT stat files, and a proposed "redundant pair" rested on believing it does.** It types
non-directory entries from `read_dir`'s `DirEntry` and yields them unstatted —
`crates/flux-core/src/walk.rs:259` — so `copy_file`'s Step 2 is the first and only source stat. Even if
the walk did stat, the re-stat would be load-bearing rather than redundant: the whole type-consistency
argument in this design is that a type from a listing is a claim about the past.

**Two real costs, stated rather than optimised away:**

- **The destination-parent stat is per FILE, not per directory.** A directory holding ten thousand
  files is stat'd ten thousand times. Hoisting it to once per directory is the obvious saving and is
  wrong: the check exists because the parent can be swapped at any moment, so a per-directory check
  would leave every file after the first one unguarded, which is the hole item 114 describes. The cost
  is the price of the guarantee and is paid deliberately.
- **The Windows parent resolution is per adapter CALL**, so a single file pays it several times over
  for the same parent. This one IS safely cacheable, because the conversion is a pure function of the
  path and the parent's resolution cannot change the answer for a directory the operation is actively
  writing into — and if it did, the item-114 check is what notices. An implementation may memoise it
  per directory. Correctness does not depend on doing so.

Five review rounds passed over `failures: Vec<TreeFailure>`, as it then was, without noticing that it
is an unbounded in-memory accumulator, and the spec forbids one. **Foundational invariant 9** (`:508-509`): *"Resident
RAM remains bounded by configured queues, workers, buffers, and bounded caches."* **Invariant 11**
(`:511-512`) adds that the scanner *"never creates an unbounded global list of discovered files"*.

A tree copy is exactly where this bites. A destination mounted read-only, or a source the user cannot
read, produces one failure PER ENTRY — a five-million-file tree yields five million `TreeFailure`
records, each carrying a `PathBuf` and an `FsError`, and the process grows until it dies. The failure
mode is worst precisely when the operation is going worst, which is when the report matters most.

**Capping by DROPPING was the first answer here, and it is withdrawn — it breaks §2 item 83.** That
item (`FLUX_FULL_UPDATED_SPEC_V16.md:279-282`) says `DIRECTORY_CHANGED_DURING_SCAN` has one outcome:
*"the directory's subtree is not transferred, the error **is reported**, and the operation exits 1"*. A
failure dropped once a cap is reached is not reported. Worse, an aggregate count of dropped failures
erases their TYPE, so a caller could no longer tell whether what it discarded was an exit-1 condition
or an ordinary per-file error — the cap would have quietly taken the exit-code contract with it.

**The failures are STREAMED instead, which is bounded by construction rather than by a limit.**
`copy_tree` takes a sink and reports each failure as it happens:

```rust
pub fn copy_tree<F: FileSystem>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<TreeOutcome, CopyError>;
```

`TreeOutcome` then carries **counts, not records** — a per-cause tally, four numbers because
`TreeFailureCause` has four variants, with the total derived from them rather than stored beside them. Nothing is dropped, nothing is truncated,
resident memory does not grow with the number of failures, and the exit code is derivable from the
tally without consulting a list that may have been capped.

This is a change to a signature this document had called settled, and it is made deliberately: the
signature was fixed before invariant 9 and item 83 were brought to bear on it, and no cap can satisfy
both "bounded memory" and "the error is reported". Streaming satisfies both, and it is the shape §1's
pipeline uses anyway, so it moves the engine toward Phase 3 rather than away from it.

The CLI passes a sink that prints or accumulates as it chooses, which is the same engine-captures /
CLI-renders division the weak-identity warning already uses, and it means a long-running copy reports
its failures as they occur rather than only at the end.

**Rejected: a `Sender<TreeFailure>` channel instead of a closure**, on the reasoning that a channel is
`Send` and so ready for Phase 3's parallel workers. It fails on the very invariant that produced this
section. `copy_tree` is synchronous and nothing drains the receiver while it runs, so an UNBOUNDED
channel is an unbounded in-memory list of failures — invariant 9 and 11 again, reintroduced by the fix
for them — while a BOUNDED channel with no concurrent receiver blocks forever once it fills. Either
the memory problem returns or the engine deadlocks. A closure has neither property because it runs the
caller's code inline, at which point the failure is handled and gone.

The `Send` concern is real but belongs to Phase 3, where the workers and the queue arrive together and
the sink can be whatever that architecture needs. Choosing a channel now would pay a cost today for an
engine this document says will be rewritten, and would pay it by breaking a requirement that is in
force today.

`WeakIdentityWarnings` stays as it is, and the contrast is the point: it is bounded by the number of
VOLUMES rather than the number of files, so aggregating it loses nothing. Failures are per-entry, so
they cannot be aggregated the same way without losing exactly what a reader needs.

The same reasoning is why `WeakIdentityWarnings` was already the right shape: it is bounded by the
number of volumes plus one, never by the number of files, which is why it stores a count and one
example rather than a list.

This is a cap on what is REPORTED, not on what is attempted — the walk still continues, and every
failure still counts toward the exit code. Nothing is silently skipped.

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

## Reading this document against merged code

**PR 1 and PR 2 are merged. Where a code block in this document disagrees with the code, THE CODE WINS.**

This document was written before any of it was implemented, and implementing it settled details it had
left open or had guessed differently. Those settlements live in the code and in PR review, not here, and
this document was not reconciled as they landed. A plan author who reads a block here as current will
cite a shape that no longer exists — which is the fabricated-precision failure this project has already
paid for once.

So: **author PR 4's plan against `main`, and grep-verify every type, field, signature and line number
before writing it down.** Use this document for intent and rationale, which are still current, not for
shapes.

The drifts found so far, recorded because each one was reached for and found wrong:

- `Metadata.is_file: bool` became `Metadata.file_type: FileType` in PR 2. The block above is corrected
  in place. `!is_file` cannot distinguish a directory from a symlink, and two checks in this design need
  exactly that distinction.
- **A directory cycle is reported as `Code::IoError`, not `Code::SafetyRejected`.** This document names
  a code for overlap and for a changed directory but never for a cycle. PR 2 decided it, and
  `crates/flux-core/src/walk.rs:340-351` records why: `SafetyRejected` aborts the whole operation under
  §129 while a cycle explicitly does not, so reusing it would make one code carry two severities.
  (`ErrorKind::FilesystemLoop` would be more precise but is unstable on the pinned toolchain, E0658.)
- **The destination-anchor comparison belongs to `copy_tree`, NOT to the walk.** The rules for it appear
  above inside the walk's description, which reads as though the walker performs it — but the merged
  signature is `walk(fs, root)` and carries no destination. The walk owns ANCESTOR-SET cycle detection,
  which needs only the source tree. `copy_tree` owns the anchor comparison and performs it on each `Dir`
  event, where it already has both the anchor and the entry. Nothing about `Walk`'s surface changes for
  PR 4.

## Delivery

Five pull requests, in order. Each plan is written only once its predecessor has merged, because a plan
citing line numbers is a set of claims about code that must already exist.

**This numbered list is the authority for the ordinals used elsewhere in this document, and the
ordinals have already shifted once.** The engine cut was PR 3 and is now PR 4; the CLI cut was PR 4 and
is now PR 5, because the atomic-publication prerequisite was inserted ahead of both. If prose anywhere
disagrees with this list, the list wins — and prefer reading the cuts by NAME (identity, walk, atomic
publication, engine, CLI), since a sixth insertion would shift them again.

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
3. **Atomic no-replace publication.** `flux-platform` only, no engine and no CLI.

   The three primitives §241.5 names — `renameat2(RENAME_NOREPLACE)` on Linux,
   `renamex_np(RENAME_EXCL)` on macOS, `MoveFileEx` without `MOVEFILE_REPLACE_EXISTING` on Windows —
   replacing `rename_no_replace`'s present check-then-act body. **No new trait method** — see the
   capability-query bullet below, which explains why the one this cut originally proposed was removed.

   Also in this cut, because §105 makes it an adapter property rather than a caller's: **extended-length
   `\\?\` paths on the Windows arm, for every call.**

   **Specified concretely, because this is the cut that gets planned next and intent is not a plan:**

   - **`rename_no_replace`'s contract does not change**, and that is the point: it already promises to
     fail rather than replace. What changes is that it now keeps that promise atomically. The
     observable difference is confined to the concurrent case that previously lost silently.
   - **Its error on an occupied target stays what it is today** — `Code::IoError` carrying
     `ErrorKind::AlreadyExists` — so existing callers and tests are unaffected. Mapping that to
     `DESTINATION_NAMESPACE_COLLISION` is the ENGINE's job in the next cut, where a tree copy knows
     that the target was planned as new; a single-file caller that asked for `NoReplace` is not in a
     collision, it simply lost a name it did not reserve.
   - **`FaultFs` gets `set_no_replace_support(bool)`**, defaulting to `true` so existing tests need no
     change. Setting it `false` makes `rename_no_replace` report the platform's unsupported error, which
     is what lets the engine's behaviour on such a destination be tested in the next cut without a
     filesystem that genuinely lacks the primitive. `NullFs` needs no change, since no trait method is
     added.
   - **The code taxonomy is added HERE**, both `NoReplacePublishUnavailable` and
     `DestinationNamespaceCollision`, even though the engine is what returns them. A `Code` variant is
     a `flux-fs` concern and adding it with the capability keeps the vocabulary in one cut; the
     alternative scatters one feature's taxonomy across two.
   - **How the adapter answers — and this is normatively fixed, not a design choice.** An earlier
     revision of this bullet said the probe is attempt-based and "needs no temporary file and therefore
     has no cleanup to get wrong". That is wrong. `FLUX_FULL_UPDATED_SPEC_V16.md:10877-10884` specifies
     the probe in detail:

     > The probe writes only inside the operation's workspace, under the fixed name `noreplace-probe`,
     > and removes what it wrote. A file it cannot remove is reported as a warning and goes with the
     > workspace when cleanup removes it; if the operation is being refused, it exits 1 instead of 3.
     > Under `--dry-run` it writes nothing at all; a primitive it cannot establish without writing is
     > reported as unprobed, and the preview says that a real run may be refused with
     > `NOREPLACE_PUBLISH_UNAVAILABLE`.

     So: it WRITES, under a fixed name, in a fixed place; it cleans up; an uncleanable probe file is a
     warning rather than a failure; a refusal that had to probe exits **1, not 3**; and `--dry-run`
     writes nothing and reports **unprobed** rather than guessing. Acceptance items 97 and 142 repeat
     the name and the workspace constraint.

   - **This collides with a scope boundary, and the collision is real rather than editorial.** The
     probe must write "inside the operation's workspace" — and the workspace is the `.flux` control
     directory of §18.2, which this design lists as OUT OF SCOPE for every one of these cuts. There is
     no workspace to write into.

     **The owner's decision: cut 3 ships the PRIMITIVE and the trait method only. The probe defers,
     with the workspace.** Two alternatives were weighed and declined — defining a minimal probe
     location outside the workspace, which buys item 113 on time at the price of a deliberate
     divergence from a normative constraint and of writing a control file into the user's destination
     tree that invariant 26 is sensitive about; and pulling a minimum viable workspace forward, which
     turns a focused platform cut into one carrying operation-scoped, destination-local control state
     that §18.2 and invariant 25 specify in detail.

     **What that costs, stated rather than buried.** Item 113 is UNMET until the workspace exists. The
     engine cut publishes atomically and correctly, but it cannot pre-refuse a destination that lacks
     the primitive with `NOREPLACE_PUBLISH_UNAVAILABLE` before changing anything — it discovers the
     lack at the first publish and fails that action instead. The difference is a whole operation
     refused up front versus every file failing one at a time, which is real and is exactly what item
     113 exists to prevent. **This is tracked debt, not an oversight**, recorded on the anomalies
     conveyor for triage rather than left in prose here.

     **`supports_no_replace_publish` is DROPPED from cut 3, and the reason is that it cannot be
     implemented.** An earlier revision kept it as a "capability hint" the adapter could answer "from
     the primitive's own error surface without writing anything — `renameat2` returning `ENOSYS` or
     `EINVAL` is an answer". That is not true on Linux. To learn whether a filesystem supports
     `RENAME_NOREPLACE` you must reach that filesystem's rename implementation, and to reach it you
     must get past path resolution — so with a source that does not exist the kernel returns `ENOENT`
     first, and `ENOENT` does not distinguish "unsupported" from "not there". **There is no
     side-effect-free probe.** That is precisely why the spec's probe WRITES.

     So the trait gains nothing here, and the honest consequence of the deferral stands on its own:
     **the engine learns at the first publish.** At that moment a staging temporary exists, so
     `renameat2` reaches the filesystem and returns `EINVAL` or `ENOSYS` truthfully — no extra write,
     no invented API, and no method whose contract could not be honoured. An unimplementable trait
     method would have been worse than the gap it was papering over, because three implementors would
     have had to fake an answer.

     When the workspace lands, the probe is implemented as specified — `noreplace-probe`, written
     inside the workspace, removed, an uncleanable file reported as a warning, exit 1 rather than 3 on
     a refusal that had to probe, and `--dry-run` writing nothing and reporting **unprobed** so the
     absence is honest rather than silent.
   - **Testing, given the gate runs on Windows only.** Each platform arm is exercised on its own CI
     leg; locally only the Windows arm runs, which is the standing constraint recorded in `TODO.md`
     rather than something this cut can fix. What is asserted everywhere, against `FaultFs`, is the
     TRAIT-level contract: that `rename_no_replace` fails on an occupied target and succeeds on a free
     one, and that a fake lacking the primitive reports it. What is asserted per platform,
     in `crates/flux-platform/tests/`, is that the real adapter refuses a real occupied target.

   **The query, not the refusal.** An earlier revision of this item claimed the cut also delivers "the
   destination PROBE and `NOREPLACE_PUBLISH_UNAVAILABLE` with exit 3", which contradicted its own
   first line. A crate with no concept of a directory operation cannot decide to refuse one, and a
   crate with no CLI cannot return an exit code. The three pieces separate cleanly along the cut
   boundaries and each is testable where it lands:

   - **this cut** answers whether the capability exists, and is tested by asking it on a real
     filesystem;
   - **the engine cut** calls it before creating anything and refuses with
     `NOREPLACE_PUBLISH_UNAVAILABLE`, tested against `FaultFs` with the capability forced either way;
   - **the CLI cut** maps that refusal to **exit 3**, tested end to end.

   This is the same division the weak-identity warning already uses — the engine decides, the CLI
   renders — and it is what invariant 24 asks for: *"The core transfer engine is independent of CLI
   presentation."* The capability query is not stranded by landing first: it is a `flux-platform`
   function with its own tests, exactly as `identity_of` was in cut 1, which also had no consumer until
   the cut after it.

   **This PR was added during the design review of the cut that follows it, and it is a prerequisite
   rather than a nice-to-have.** The spec forbids check-then-rename as a substitute by name, so the
   tree engine cannot publish compliantly until this exists. It is placed here rather than inside the
   engine cut for the reason PR 1 was placed before PR 2: a platform primitive with three
   implementations and its own test surface is reviewable on its own terms, and unreviewable buried
   inside a PR about tree safety.

   It also fixes a defect reachable **today**, independently of any tree copy: `rename_no_replace` is
   check-then-act in the shipped single-file path. Item 113 says a single-file operation against a
   destination lacking the primitive is *unaffected*, so the probe and its refusal attach to directory
   operations only — but the atomicity itself benefits both.

4. **`copy_tree`, the safe engine.** The driver, the lexical containment floor, the §129 pre-flight and
   dynamic identity checks, the `Safety` enum on `CopyOptions`, the CAPTURE of the aggregated
   weak-identity warning into `TreeOutcome::warnings`, and the per-file identity check described under
   "The hole the directory checks do not cover" above. **No CLI**, so nothing here is rendered — the
   warning is captured as data and displayed in PR 5.

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

5. **The CLI.** `flux copy` dispatching a directory source to `copy_tree`, the reporting of
   `TreeOutcome` — the counts and the per-entry failures — the rendering of
   `TreeOutcome::warnings` as one line per affected volume plus one for the `Unavailable` bucket, and
   the `--safety=strict` flag that sets `Safety::Strict`. A thin surface over an engine whose safety is
   already settled and reviewed.

## Out of scope

Following symlinks (§26, Future), recreating symlinks at the destination, hardlink topology (§12, §13),
directory metadata preservation, multiple source roots (§18.3), `FluxPathKey` materialisation (§103),
parallel workers and the bounded transfer queue (Phase 3), resume, and the `.flux` control directory
(§18.2).

## Stand-downs

Findings a panel round raised and stood down rather than folded, recorded so a later round does not
re-derive them and a reader can see what was consciously not fixed.

- `DISCARDED-BELOW-FLOOR: the per-file gate's extra destination stat is paid N times for a tree of N
  files, and in the dominant fresh-destination case every one of them returns NotFound.` Unavoidable at
  this layer: "absent" and "aliased" are not distinguishable without stat'ing, and the alternative —
  reusing the stat `destination_is_write_protected` takes at `crates/flux-platform/src/std_fs.rs:62`
  (Unix) / `:109` (Windows) — happens inside the adapter at publish time, which is Step 7, far too late
  to refuse before the staging temporary exists.
- `DISCARDED-BELOW-FLOOR: WeakIdentityWarnings needs an empty/default constructor because TreeOutcome
  always carries one.` A construction detail with no behavioural consequence; the implementer derives
  `Default` or writes `new()` as the surrounding code prefers.
- `DISCARDED-BELOW-FLOOR: when BOTH sides are Weak, which volume keys the aggregation is unspecified.`
  Affects only which of two warning buckets a degradation lands in, never whether it is reported. The
  rule is the destination's, since that is the side the user did not name.
- `DISCARDED-BELOW-FLOOR: TreeFailureCause::Unsupported carries a FileType while the rendered code is
  always SPECIAL_FILE_UNSUPPORTED.` Intended, and stated as such where the variant is defined: the
  variant carries what the entry WAS so the report can say so, while the code stays single-valued.
- `DISCARDED-BELOW-FLOOR: the pre-flight's own weak-identity warning does not say whether its example
  path is the source root or the destination root.` It is the destination anchor, by the same rule that
  settles the both-sides-Weak tie-break above: the side the user did not name is the informative one.
  Named here rather than fixed in place because it follows from a rule already stated.
- `REJECTED: "an implementer must invent a Metadata field to check for a directory."` The field exists
  and is merged — `pub file_type: FileType` at crates/flux-fs/src/fs.rs:87. The document's own block was
  stale, which is the finding that was folded; the implementer invents nothing.
- `REJECTED: "the walk signature must be broken to pass the destination anchor."` The walk never needed
  it. `copy_tree` holds the anchor and compares on each `Dir` event; the walk owns only ancestor-set
  cycle detection, which needs the source tree alone.
- `REJECTED: "FaultFs cannot represent a symlink, so the anchor rule is too loosely specified to unit
  test."` Refuted by measurement: `FileType::Symlink` is a merged variant (crates/flux-fs/src/fs.rs:33),
  `FaultFs::add_symlink` exists (crates/flux-core/src/fault_fs.rs:313), `set_type` at :222, and a test
  already uses it at :784. The claim rested on the stale `Metadata.is_file` block that the same review
  had just found and that is now corrected — a premise carried forward instead of re-read.
- `DISCARDED-BELOW-FLOOR: the §109 quote is reflowed from the spec's multi-line code block.`
  Semantically identical, stylistically altered; not a factual error.
- `DEFERRED-TO-ANOMALIES: §241.5's second bullet requires a target planned as a REPLACEMENT to claim the
  existing directory entry with an insert-if-absent write in the operation's state store. * n/a *
  2026-09-24` Reachable only once persistent operation state exists, which is Phase 3 and explicitly out
  of scope here; recorded so the omission is tracked rather than forgotten. Named by the round 4 peer.
- `REJECTED: "stale PR 3 / PR 4 prose references are a blockable contradiction."` Stood down by the
  round-6 peer itself, and correctly: the Delivery list is declared authoritative and the ordinals were
  renumbered mechanically. Recorded because the ordinals have now shifted twice.
- `DISCARDED-BELOW-FLOOR: how the CLI renders streamed failures against the summary tally.` Presentation,
  settled in the CLI cut, no correctness consequence.
- `REJECTED: "a failed copy leaves its .flux-partial temporary on disk, violating invariant 26."`
  Refuted by measurement. `copy_file` routes EVERY post-temporary failure through `discard()`
  (crates/flux-core/src/copy.rs:177, 180, 189, 199, 208, 223, 225, 229, 240), and a comment at :82
  warns against `?` after the temporary exists for exactly this reason. Three tests pin it, including
  `a_full_disk_does_not_publish_and_leaves_no_temporary` at :333. The single case where REMOVAL itself
  fails reports the surviving path in `CopyError.leftover`, which `TreeFailureCause::Copy` carries
  intact.
- `REJECTED: "canonicalizing the destination anchor makes symlink targets contribute to topology,
  against invariant 21."` Invariant 2 defines topology as HARDLINK topology among selected entries;
  destination containment is not topology, and the peer hedged the claim itself.
- `DISCARDED-BELOW-FLOOR: the failure sink returns () so a caller cannot abort the walk early.`
  Aborting early on a per-entry failure is forbidden anyway — §2 item 83 requires the walk continue.
- `REJECTED: "remove FailureTally, since failures are streamed and the caller can count them."` Three
  reasons. It breaks the symmetry of `TreeOutcome`, which counts successes (`files_copied`,
  `bytes_copied`, `directories_created`) — a summary that counts what went right but not what went
  wrong is a strange object. It forces every caller, including each test, to reimplement counting in
  order to learn whether anything failed at all. And it is four `u64`s. The harmful duplication here
  was a total stored beside its parts, and that was already removed. Note also that the same peer
  argued the opposite one round earlier, requiring that `TreeOutcome` keep TYPED counts so the exit
  code could distinguish an item-83 condition.
- `REJECTED: "remove the volume bucketing in WeakIdentityWarnings; one bucket for everything is
  simpler."` §108's wording is "one aggregated warning per affected filesystem/operation where
  practical", so grouping by volume is the specified shape and a single bucket is not. The same peer
  argued FOR this grouping during the design negotiation, on the grounds that collapsing multiple weak
  filesystems into one warning discards a volume that is trustworthy even where its index is not. The
  claimed per-file `BTreeMap` cost is also wrong: the map is touched only on a DEGRADED entry, and
  holds at most one row per volume.
- `DISCARDED-BELOW-FLOOR: a source directory replaced by a symlink between listing and descent.`
  Already guarded: the walk stats before entering and a type change from `Dir` yields
  `DirectoryChangedDuringScan` rather than a descent, which is what PR 2's type-consistency check exists
  for. Named because it is the source-side mirror of item 114 and a reader will look for it.
- `DISCARDED-BELOW-FLOOR: the attacker deletes the staging temporary mid-stream.` Fails closed — Step 6
  or Step 7 gets `NotFound` and the copy fails without publishing, which is the desired outcome.
- `REJECTED: "the identity capture defeats itself — swap the directory for a symlink between create_dir
  and the capturing stat, and Flux records the TARGET's identity and matches it forever."` Refuted by
  measurement on both arms: `FileSystem::metadata` does not follow links — `symlink_metadata` at
  crates/flux-platform/src/std_fs.rs:172, `FILE_FLAG_OPEN_REPARSE_POINT` at :188. A symlink swapped in
  before the capture is typed `Symlink` carrying the LINK's id. The finding did expose a real
  underspecification, now folded: the capture ASSERTS `file_type == Dir` rather than merely recording
  an id.
- `REJECTED: "the design attributes the DIRECTORY_CHANGED_DURING_SCAN rule to the wrong item 83; the real item 83
  is about case-insensitive namespace collisions."` The specification has TWO item 83s. `§2 item 83` at
  :279-282 is the exit-1 rule this design cites and quotes verbatim, correctly. The acceptance-list
  item 83 at :13300 is the namespace-collision one. Both exist; the citation names the section.
- `DISCARDED-BELOW-FLOOR: a path component ABOVE the destination anchor replaced after canonicalization
  but before a write.` Real, and already covered by the residual window stated under item 114 — the
  check and the write remain two operations against a path the kernel re-resolves, and only
  handle-relative traversal closes it.
- `DISCARDED-BELOW-FLOOR: the staging temporary's name is predictable from the process id.`
  Pre-creating it makes Step 3's `create_new` fail with `AlreadyExists` and the copy aborts without
  publishing, which is the safe outcome; the sweep at Step 1 removes the name, following no link.
- `REJECTED: "the acceptance list was swept exhaustively, items 1 through 141."` The list runs to at
  least 147 (spec :13505-13517). The uninspected tail is where item 142 names the no-replace probe file
  `noreplace-probe` -- which overturned this document's own account of how the probe works. An
  exhaustiveness claim that stops short of the end is worth checking before it is relied on.
- `DEFERRED-TO-ANOMALIES: spec item 113's NOREPLACE_PUBLISH_UNAVAILABLE pre-refusal is unmet until the
  operation workspace exists, because the normative probe must write inside it. * spec:13406 *
  2026-09-24` Owner-ruled: cut 3 ships the primitive only and the probe defers with the workspace.
  Until then the engine discovers a missing primitive at the first publish rather than refusing the
  operation before anything changes.
- `RESOLVED: the acceptance list's boundary.` It ends at item **147** (spec :13516-13517), verified
  independently by both the driver and the round-11 peer. Items 143-147 are Phase 3 locking, worker
  state and dead-owner recovery, and bind nothing here. Items 124 and 131 corroborate the probe's file
  and its cleanup, already folded as tracked debt. The list is now swept end to end.
- `REJECTED: "discarding a listing after read_dir desynchronises the consumer's path stack, because a
  Dir with no matching DirEnd sends every later file into the wrong directory."` Correct about the
  hazard, wrong about this design. `Walk::enter` pushes the frame and THEN returns the `Dir` event
  (crates/flux-core/src/walk.rs:372-374), so a failure inside `enter` emits no `Dir` at all and owes no
  `DirEnd`. The objection did earn a precision now folded: the re-check's position inside `enter` is
  stated explicitly, because putting it after the `Dir` event would make the objection right.
- `RESOLVED: the section sweep's boundary.` 260 top-level `# N.` sections, 416 headings including
  subsections. Uncited sections that bind and are SATISFIED: §6 deterministic ordering, §40 special
  files, §41 filesystem safety, §43 paths, §110 source-mutation levels, §111 torn-read window -- all
  now cited. §55 and §105 were NOT satisfied and are folded above. §149.7 was missed entirely by the
  sweep and is the subject of its own open decision.
- `DEFERRED-TO-ANOMALIES: spec 149.7's handle-relative destination writes are unmet; the design
  substitutes identity capture plus re-verification, which narrows the window rather than closing it.
  * FLUX_FULL_UPDATED_SPEC_V16.md:7190 * 2026-09-24` Owner-ruled divergence. The OUTCOME 149.7
  prescribes -- per-path SAFETY_REJECTED, the walk continuing, exit 1 -- is met; the MECHANISM is not.
- `REJECTED: "Code::IoError for a directory cycle is wrong because SAFETY_REJECTED aborts the whole
  operation."` That was PR 2's stated reason and 149.7 undercuts it: SAFETY_REJECTED's registry entry
  (spec:2946) cites both 129 and 149.7 and says "(that path only)", so the code already carries both an
  operation-wide and a per-path severity. The CHOICE still stands on a better footing -- 55 says
  IO_ERROR is used "only when no more specific code applies", and the registry has no cycle code, while
  SAFETY_REJECTED's definition covers containment, self-copy and unexpected link components, none of
  which is a cycle. Recorded so the weaker argument is not re-cited.
- `REJECTED: "149.1 is contradicted -- the design does not resolve the nearest existing ancestor or
  record the unresolved suffix."` It does both. The canonicalization section resolves the nearest
  existing ancestor and appends "the non-existent remainder lexically afterwards", which is 149.1's
  unresolved suffix under another name. 149.1 is uncited rather than unmet, and is now cited.
- `RESOLVED: the subsection sweep.` 30 subsections across the families whose parents this design
  touches. Fourteen bind and were uncited; twelve are satisfied and now cited -- 7.1, 7.3, 30.1, 149.1,
  149.2, 149.3, 149.5, 233.1, 233.2, 233.3, 241.1, 241.2, 241.3. Two were NOT satisfied: 241.4, folded
  above, and 5.2's dry-run rule, which belongs to the deferred probe and is covered by that debt entry.
- `DISCARDED-BELOW-FLOOR: a mount-boundary skip could increment both boundaries_skipped and failures
  under --safety=strict.` It cannot: strict affects identity comparisons that cannot be made, while a
  boundary skip is a comparison that succeeded and said "different volume". A boundary whose volume
  could not be read is the degraded case and lands on the warning channel only, as stated.
- `REJECTED: "the Walk source stat and copy_file's pre-copy source stat ask the same question twice."`
  The walk does not stat files. It types non-directory entries from read_dir's DirEntry and yields them
  unstatted (crates/flux-core/src/walk.rs:259); only FileType::Dir goes through enter(), which stats.
  So copy_file's Step 2 is the first source stat, not the second. And were it the second it would still
  be load-bearing, since this design's whole type-consistency argument is that a type taken from a
  listing is a claim about the past.
