# Directory walk (walker PR 2 of 3) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended)
> or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax
> for tracking.

**Goal:** Deliver the ordered, non-terminal-error directory walk — `FileType`, `DirEntry`, `read_dir`,
`create_dir`, `Code::DirectoryChangedDuringScan`, the `FaultFs` widening, and `Walk` with its depth cap
and ancestor-set cycle detection — with no `copy_tree` and no CLI.

**Architecture:** A pull iterator over an explicit stack of directory frames. Each frame holds one
directory's entries, sorted by `name.as_encoded_bytes()`. Descent happens only on `FileType::Dir`, is
gated by a depth cap and an ancestor set of `ObjectId`s, and yields a pre-order `Dir` event and a
post-order `DirEnd`. An `Err` item skips one subtree and the walk continues.

**Tech Stack:** Rust 2024, `flux-fs` (trait + types), `flux-core` (walk + `FaultFs`), `flux-platform`
(`StdFileSystem`). Gate is `just check`.

---

## Provenance — read this before changing any decision below

Every citation in this plan was verified against `origin/main` at **`ec58829`** on 2026-09-24, which is
the merge of walker PR 1. Line numbers are real, not remembered.

**14 tasks, 13 commits.** Every task ends at a green commit except Task 7, which is committed together
with Task 8 because the `Walk` type does not build until the iterator that reads its fields exists —
measured, and explained at Task 7 Step 5. Every other task boundary is a bisectable, green commit on
both platforms.

The same `dead_code` rule moved one other thing: the depth-cap CHECK lives in Task 8 rather than Task
10, because `Walk` stores `max_depth` and a field written but never read is itself `dead_code`. Task 10
keeps the tests. Both displacements come from one measured fact, and they are the only two.

**Measured baseline on this worktree, `just check` green:**

```
119 tests run: 119 passed, 1 skipped
flux-core 37 | flux-fs 10 | flux-platform::std_fs 13 (Windows) | flux-platform::fs_semantics 9
flux::model_stamp 45 | flux::dev_tooling 2 | flux::integration 1 | flux-cli::copy 2
```

On Linux `flux-platform::std_fs` is 15, not 13 — two tests are Unix-only.

**Nine design forks were settled before this plan was written**, across three AGY-FIRST consult rounds
(`.clavity/seams/pr2-design-forks.md`, `pr2-design-negotiation.md`, `pr2-implementation-forks.md`):

| Fork | Decision | Who was right |
|---|---|---|
| A | `FaultFs` inner maps keyed by `PathBuf`; helpers take `impl AsRef<Path>` | agreed |
| A-sub | The call log stays `Vec<String>` built with `display()` | driver; peer conceded |
| B | `WalkEvent::Dir` carries `identity`; PR 2 has no warning channel | agreed |
| C | `create_dir` lands in PR 2 and builds the walk tests' fixtures | driver; peer conceded |
| D | `DEFAULT_MAX_DEPTH` + `walk_with_depth`, not a `WalkOptions` struct | driver; peer conceded |
| E | `WalkError.cause` stays `FsError` | **peer; driver was wrong** |
| F1 | Cycle refusal is `Code::IoError` + `Error::other` | driver; peer's refinement refuted |
| F2 | `DirectoryChangedDuringScan` is produced by the type-mismatch check | agreed |
| F3 | `FaultFs` replaces `not_files` with `types: HashMap<PathBuf, FileType>` | agreed |
| F4 | **`Metadata.is_file` is REPLACED by `file_type: FileType`** | **peer; driver was wrong** |

Two of those are worth restating because they are the ones a future reader will want to re-litigate.

**F4 — why `Metadata` gains a breaking change.** MEASURED on both platforms with a standalone probe:

```
Windows (junction)              Linux (symlink)
read_dir(link) -> ["marker.txt"]    read_dir(link) -> ["marker.txt"]
  FOLLOWS THE LINK = true             FOLLOWS THE LINK = true
symlink_metadata(link): is_dir=false is_file=false is_symlink=true   (identical on both)
```

`is_file == false` is true for a directory AND for a symlink, so a `Metadata` carrying only `is_file`
cannot tell them apart. `std::fs::read_dir` FOLLOWS a symlink to a directory. Therefore a walk that
checks only `!is_file` before calling `read_dir` will, if the object under a name is swapped from a
directory to a symlink between the listing and the stat, enumerate the symlink's TARGET and copy files
from outside the tree. That is a reachable tree escape, not a theoretical one, and it is exactly what
§149.4 asks the scanner to verify against.

**State this honestly and do not overclaim it:** `file_type` NARROWS the window from
`read_dir`-to-`metadata` down to `metadata`-to-`read_dir`. It does not CLOSE it. Closing it requires
`openat`-style relative traversal with `O_NOFOLLOW`, which `TODO.md` already records as the fix for
walker TOCTOU. Task 12's comment must say this. A comment claiming the race is eliminated would be a
false safety promise, which is worse than the race.

**F1 — why not the peer's `ErrorKind::FilesystemLoop`.** The peer's reasoning was sound ("`IoError` is
not a meaningless catch-all if you use the correct `ErrorKind`") but its concrete fix does not compile:

```
error[E0658]: use of unstable library feature `io_error_more`
 --> k.rs:2:47  |  std::io::ErrorKind::FilesystemLoop
 = note: see issue #86442
rustc 1.98.0 (88d9e12ae 2026-08-18)
```

So cycles use `Code::IoError` with `std::io::Error::other("...")`, like the depth cap. If
`io_error_more` stabilises, revisiting this is a one-line change and a good idea.

---

## File Structure

| File | Change | Responsibility after this PR |
|---|---|---|
| `crates/flux-fs/src/fs.rs` | modify | `FileType`, `DirEntry`, `Metadata.file_type`, `read_dir`/`create_dir` on the trait, `NullFs` |
| `crates/flux-fs/src/error.rs` | modify | gains `Code::DirectoryChangedDuringScan` |
| `crates/flux-fs/src/lib.rs` | modify | re-export `FileType`, `DirEntry` |
| `crates/flux-core/src/walk.rs` | **create** | `WalkEvent`, `WalkError`, `Walk`, `walk`, `walk_with_depth`, `DEFAULT_MAX_DEPTH` |
| `crates/flux-core/src/lib.rs` | modify | `pub mod walk;` + re-exports |
| `crates/flux-core/src/fault_fs.rs` | modify | `PathBuf` keys, `types` map, `directories`, `read_dir`, `create_dir` |
| `crates/flux-core/src/copy.rs` | modify | one line: the `SPECIAL_FILE_UNSUPPORTED` check |
| `crates/flux-platform/src/std_fs.rs` | modify | `read_dir`, `create_dir`, single-open `metadata` |
| `crates/flux-platform/tests/std_fs.rs` | modify | real-filesystem tests for the above |

The walk goes in its **own file**, `walk.rs`, not into `copy.rs` — `copy.rs` is already 696 lines and
the walk shares no code with it.

---

## Task 1: Reattach the orphaned doc comments in `fault_fs.rs`

Found while reading merged `main` for this plan, and recorded in `.clavity/local-anomalies.md`. Three
doc comments belonging to three different functions are stacked above ONE of them; two functions have
none. Six capstone rounds, including a Comment Drift Auditor seat, missed it. Do this first so the heavy
edits in Task 4 land on a clean file.

**Files:** Modify `crates/flux-core/src/fault_fs.rs`

- [ ] **Step 1: Confirm the defect is still there**

Run: `sed -n '55,72p;79p;89p;201,210p;223p;264,273p' crates/flux-core/src/fault_fs.rs`

Expected: lines 55-71 are three stacked doc comments ending at `fn missing_source` on line 72; `fn
mint_identity` (79) and `fn move_object` (89) have no doc comment; lines 201-209 stack three comments
above `fn fail_kind` (210) while `fn fail` (223) has none; lines 264-268 stack two above `set_identity`
(269) while `add_special` (273) has none.

If it does NOT look like this, STOP and report `STATE_MISMATCH: <what differs>`.

- [ ] **Step 2: Move each doc comment onto the function it describes**

Three moves, and NO wording changes — every line below already exists in the file, only its position
changes:

1. `/// Move a name's content AND its metadata. ...` (currently 55-57) → immediately above
   `fn move_object` (currently 89).
2. `/// Give the object now at `path` a fresh identity ... its test stayed green.` (currently 58-65) →
   immediately above `fn mint_identity` (currently 79).
3. Leave `/// `NotFound` when the rename's source does not exist ... `metadata` panicked.` (currently
   66-71) where it is; it describes `missing_source` and is already correctly placed.
4. `/// Make the next call to `name` fail with `code`.` and `/// Fail the next call to `name`, once.`
   (currently 201-202) → these two describe `fail`; move BOTH above `fn fail` (currently 223) and
   collapse them to the single line `/// Make the next call to `name` fail with `code`, once.` — they
   are two drafts of one sentence.
5. `/// Make `metadata` report `path` as something other than a regular file ... had no test that
   produced it.` (currently 264-266) → immediately above `fn add_special` (currently 273).

- [ ] **Step 3: Verify nothing but comments moved**

Run: `git diff --stat` — expect only `crates/flux-core/src/fault_fs.rs` changed.

Run: `git diff -U0 crates/flux-core/src/fault_fs.rs | grep -E '^[+-]' | grep -vE '^[+-]{3}' | grep -vcE '^[+-]\s*///'`

Expected output: `0`. Every changed line is a doc comment. If it is not 0, a non-comment line moved —
STOP and report.

- [ ] **Step 4: Gate**

Run: `just check`
Expected: exit 0, `119 tests run: 119 passed, 1 skipped`. Counts are UNCHANGED — this task adds no tests.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/fault_fs.rs
git commit -m "docs: reattach three doc comments to the functions they describe

Three doc comments for move_object, mint_identity and missing_source were
all stacked above missing_source, and two functions carried none. Same
drift above fail_kind and set_identity. Comments only; no code moved.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 2: `FileType` and `DirEntry`

**Files:** Modify `crates/flux-fs/src/fs.rs`, `crates/flux-fs/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/flux-fs/src/fs.rs`:

```rust
    #[test]
    fn encoded_bytes_order_is_the_ordering_contract_not_utf16() {
        // §7.2 sorts by `as_encoded_bytes()`. WTF-8 and UTF-16 DISAGREE above the
        // BMP: surrogates (0xD800-0xDBFF) sort below the private-use area in UTF-16
        // while their UTF-8 encodings sort above it. If anyone ever "fixes" the sort
        // to use `encode_wide()` on Windows, this test is what catches it.
        //
        //   U+E000  WTF-8 EE 80 80     UTF-16BE E0 00
        //   U+10000 WTF-8 F0 90 80 80  UTF-16BE D8 00 DC 00
        let pua = std::ffi::OsString::from("\u{E000}");
        let astral = std::ffi::OsString::from("\u{10000}");

        assert!(
            pua.as_encoded_bytes() < astral.as_encoded_bytes(),
            "WTF-8 puts U+E000 first; got {:?} vs {:?}",
            pua.as_encoded_bytes(),
            astral.as_encoded_bytes()
        );

        // And the other direction, so the test cannot pass by both being equal.
        let utf16_pua: Vec<u16> = "\u{E000}".encode_utf16().collect();
        let utf16_astral: Vec<u16> = "\u{10000}".encode_utf16().collect();
        assert!(utf16_astral < utf16_pua, "UTF-16 puts U+10000 first: this is the disagreement");
    }

    #[test]
    fn a_dir_entry_carries_a_name_and_a_type() {
        let e = DirEntry { name: std::ffi::OsString::from("a"), file_type: FileType::Dir };
        assert_eq!(e.file_type, FileType::Dir);
        assert_ne!(FileType::Dir, FileType::Symlink);
        assert_ne!(FileType::File, FileType::Other);
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p flux-fs --lib 2>&1 | tail -20`
Expected: FAILS to compile — `cannot find type `DirEntry` in this scope`.

- [ ] **Step 3: Add the types**

Insert into `crates/flux-fs/src/fs.rs` immediately after the `Perms` enum (ends line 15):

```rust
/// What a directory listing reports per entry.
///
/// Deliberately NOT `Metadata`: the entry type is what the OS supplies during
/// enumeration, and a `Metadata` per entry would re-stat every child, which is the
/// cost this primitive exists to avoid.
///
/// There is no `Unknown` variant. Linux `readdir` can return `DT_UNKNOWN`, but std
/// resolves it transparently -- `library/std/src/sys/fs/unix.rs:1149` is
/// `_ => self.metadata().map(|m| m.file_type())` -- so the variant would be
/// unconstructible. The cost is real and worth naming: on a filesystem with no
/// `d_type` (this repository measurably uses one, `/mnt/c` under WSL is v9fs) that
/// fallback is an `lstat` per entry.
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
```

- [ ] **Step 4: Export them**

In `crates/flux-fs/src/lib.rs`, change line 16 from:

```rust
pub use fs::{FileHandle, FileIdentity, FileSystem, Metadata, ObjectId, Perms};
```

to:

```rust
pub use fs::{DirEntry, FileHandle, FileIdentity, FileSystem, FileType, Metadata, ObjectId, Perms};
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p flux-fs --lib`
Expected: PASS, `12 passed` (baseline 10 + 2).

- [ ] **Step 6: Gate and commit**

Run: `just check` — expected exit 0, `121 tests run: 121 passed, 1 skipped`.

```bash
git add crates/flux-fs/src/fs.rs crates/flux-fs/src/lib.rs
git commit -m "feat(fs): add FileType and DirEntry

One directory entry as enumeration supplies it: a name and a type, with no
Metadata per entry. FileType has no Unknown variant because std resolves
DT_UNKNOWN before a caller sees it.

A test pins the ordering contract to as_encoded_bytes() by showing WTF-8 and
UTF-16 disagree on U+E000 against U+10000.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 3: Replace `Metadata.is_file` with `file_type: FileType` (fork F4b)

This is a breaking change to a public struct, and the blast radius is exactly nine lines — enumerated
below, all of them verified present at `ec58829`. **Do not add `file_type` alongside `is_file`.** Two
fields encoding overlapping facts can disagree, and the whole reason for this task is that one boolean
could not express the difference between a directory and a symlink.

**Files:** Modify `crates/flux-fs/src/fs.rs`, `crates/flux-platform/src/std_fs.rs`,
`crates/flux-core/src/fault_fs.rs`, `crates/flux-core/src/copy.rs`,
`crates/flux-platform/tests/std_fs.rs`

- [ ] **Step 1: Verify the blast radius before touching anything**

Run: `rg -n "is_file" crates/ --glob '!target'`

Expected exactly these nine, and nothing else:

```
crates/flux-fs/src/fs.rs:54:    pub is_file: bool,
crates/flux-fs/src/fs.rs:157:                is_file: true,
crates/flux-fs/src/fs.rs:212:        assert!(fs.metadata(Path::new("x")).unwrap().is_file);
crates/flux-platform/tests/std_fs.rs:136:    assert!(!m.is_file, "a symlink must not read as a regular file, or the refusal is bypassed");
crates/flux-platform/tests/std_fs.rs:143:    assert!(!fs.metadata(d.path()).unwrap().is_file);
crates/flux-platform/tests/std_fs.rs:431:    assert!(!m.is_file, "a reparse point is not a regular file");
crates/flux-platform/src/std_fs.rs:179:            is_file: m.is_file(),
crates/flux-core/src/fault_fs.rs:408:            is_file: !g.not_files.contains(&p),
crates/flux-core/src/copy.rs:157:    if !src_meta.is_file {
```

(`std_fs.rs:168`, `std_fs.rs:420` and `fault_fs.rs:270` also match but are prose inside comments, not
code.)

**These are post-Task-1 line numbers.** Task 1 moved doc comments in `fault_fs.rs` and shortened it by
one line, so its construction site is at **408**, not the 409 this plan carried before Task 1 ran.

If the set differs, STOP and report `STATE_MISMATCH: <what differs>`.

- [ ] **Step 2: Change the field**

`crates/flux-fs/src/fs.rs`, replace lines 52-54:

```rust
pub struct Metadata {
    pub len: u64,
    /// False for directories, symlinks, devices, FIFOs and sockets.
    pub is_file: bool,
```

with:

```rust
pub struct Metadata {
    pub len: u64,
    /// The object's own type, NOT its target's -- adapters stat with
    /// `symlink_metadata`.
    ///
    /// A `bool` here was not enough, and the reason is a measured tree escape rather
    /// than tidiness. `is_file == false` is equally true of a directory and of a
    /// symlink, while `std::fs::read_dir` FOLLOWS a symlink to a directory --
    /// MEASURED identically on Windows (junction) and Linux (symlink). So a walk
    /// that checked only `!is_file` before descending would, on an object swapped
    /// from directory to symlink between the listing and the stat, enumerate the
    /// link's TARGET and copy files from outside the tree.
    pub file_type: FileType,
```

- [ ] **Step 3: Fix the three construction sites**

`crates/flux-fs/src/fs.rs:157` — inside `NullFs::metadata`, change `is_file: true,` to
`file_type: FileType::File,`.

`crates/flux-platform/src/std_fs.rs:179` — change `is_file: m.is_file(),` to `file_type: type_of(&m),`
and add this free function. **Placement matters:** `perms_of` is cfg-gated into TWO definitions,
`#[cfg(unix)]` at `:254-258` and `#[cfg(not(unix))]` at `:260-263`. `type_of` is NOT platform-specific
and must be a SINGLE ungated function — put it after both, at line 264, before the `identity_of` doc
comment that begins at `:265`:

```rust
/// Map std's `FileType` to ours. Symlink FIRST: on Windows std defines `is_dir()` as
/// `!is_symlink && is_directory`, so a junction or directory symlink is already
/// excluded there -- but testing symlink first makes that independent of std's
/// definition rather than reliant on it.
fn type_of(m: &std::fs::Metadata) -> flux_fs::FileType {
    let t = m.file_type();
    if t.is_symlink() {
        flux_fs::FileType::Symlink
    } else if t.is_dir() {
        flux_fs::FileType::Dir
    } else if t.is_file() {
        flux_fs::FileType::File
    } else {
        flux_fs::FileType::Other
    }
}
```

`crates/flux-core/src/fault_fs.rs:408` — change `is_file: !g.not_files.contains(&p),` to
`file_type: if g.not_files.contains(&p) { FileType::Other } else { FileType::File },`. Task 4 replaces
this line again; this interim form only has to compile and keep the existing tests green.

**`FileType` is NOT in scope in that file.** Line 7 currently reads

```rust
use flux_fs::{Code, FileHandle, FileSystem, FsError, Metadata, Perms, Result};
```

Add `FileType` and ONLY `FileType`:

```rust
use flux_fs::{Code, FileHandle, FileSystem, FileType, FsError, Metadata, Perms, Result};
```

Without it the step does not compile — `cannot find type FileType in this scope`.

**Do NOT add `DirEntry` yet**, tempting as it is: nothing in this task names it, and an unused import is
an `unused_imports` warning, which `clippy -D warnings` makes fatal. Task 5 adds it at the point it is
first used. Both review panels found the missing `FileType` independently, and a third round caught this
plan pre-importing `DirEntry` alongside it — which is why the two are now separated by hand.

- [ ] **Step 4: Fix the one production consumer**

`crates/flux-core/src/copy.rs:157`, change:

```rust
    if !src_meta.is_file {
```

to:

```rust
    if src_meta.file_type != FileType::File {
```

and add `FileType` to that file's `flux_fs::{...}` import list.

**ORACLE:** `copy.rs`'s behaviour must not change. The pinning tests are
`copy::tests::*` — all 37 `flux-core` tests must stay green, and in particular any test asserting
`SPECIAL_FILE_UNSUPPORTED`. If making this compile would change WHICH inputs are refused, STOP and
report `[original] -> [yours] because <reason>`.

- [ ] **Step 5: Fix the four test assertions**

- `crates/flux-fs/src/fs.rs:212`: `assert!(fs.metadata(Path::new("x")).unwrap().is_file);` →
  `assert_eq!(fs.metadata(Path::new("x")).unwrap().file_type, FileType::File);`
- `crates/flux-platform/tests/std_fs.rs:136`: → `assert_eq!(m.file_type, flux_fs::FileType::Symlink,
  "a symlink must report as a symlink, or the refusal is bypassed");`
- `crates/flux-platform/tests/std_fs.rs:143`: → `assert_eq!(fs.metadata(d.path()).unwrap().file_type,
  flux_fs::FileType::Dir);`
- `crates/flux-platform/tests/std_fs.rs:431`: → `assert_eq!(m.file_type, flux_fs::FileType::Symlink, "a
  name-surrogate reparse point reports as a symlink");`

Those last three are STRENGTHENED, not merely translated: `!is_file` was satisfied by four different
types, `== Symlink` and `== Dir` by exactly one each.

- [ ] **Step 6: Run the gate**

Run: `just check`
Expected: exit 0, `121 tests run: 121 passed, 1 skipped`. No count change — this task rewrites
assertions, it adds none.

- [ ] **Step 7: Prove the strengthened assertions are non-vacuous**

Temporarily change `type_of` so the symlink arm returns `FileType::Other` instead of `Symlink`.

Run: `cargo test -p flux-platform --test std_fs 2>&1 | tail -15`
Expected: `metadata_reports_a_symlink_as_not_a_file` and `metadata_reports_a_reparse_point_as_not_a_file`
FAIL; the rest pass. Under the OLD `!is_file` assertion both would have stayed GREEN, which is the point.

**Revert the mutant surgically** — restore only that one arm. Do NOT `git checkout --` the file.

- [ ] **Step 8: Commit**

```bash
git add crates/flux-fs/src/fs.rs crates/flux-platform/src/std_fs.rs \
        crates/flux-core/src/fault_fs.rs crates/flux-core/src/copy.rs \
        crates/flux-platform/tests/std_fs.rs
git commit -m "feat(fs)!: Metadata carries file_type instead of is_file

is_file cannot distinguish a directory from a symlink, and std::fs::read_dir
follows a symlink to a directory - measured identically on Windows via a
junction and on Linux via a symlink. A walk gated on !is_file would therefore
enumerate a swapped link's target and copy files from outside the tree.

Three construction sites and one consumer; three test assertions get stronger
as a side effect, since != File names one type where !is_file named four.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 4: `FaultFs` — `PathBuf` keys and a single `types` map (forks A2, F3b)

**Files:** Modify `crates/flux-core/src/fault_fs.rs`

The fake keys every inner map by `path.display().to_string()`, which is LOSSY: two distinct non-UTF-8
names collapse to one key and corrupt the fake's state. That is the §241 case the ordering contract is
about.

**Zero call-site churn is expected, and it is MEASURED, not hoped for:** all 68 call sites across
`copy.rs` (47) and `fault_fs.rs` (21) pass plain string literals, so `impl AsRef<Path>` accepts every one
unchanged.

- [ ] **Step 1: Confirm the starting state**

Run: `rg -n "HashMap<String|HashSet<String|display\(\).to_string\(\)" crates/flux-core/src/fault_fs.rs | wc -l`
Expected: a non-zero count. Run without `| wc -l` and read it; these are the lines this task rewrites.

If `fault_fs.rs` already uses `PathBuf` keys, STOP and report `STATE_MISMATCH: keys already widened`.

- [ ] **Step 2: Widen `Inner`**

In `struct Inner`, change every `HashMap<String, T>` to `HashMap<PathBuf, T>` and every
`HashSet<String>` to `HashSet<PathBuf>`, EXCEPT these **five**, which are keyed by a CALL NAME, not a
path, and must stay `String`:

```rust
    faults: HashMap<String, Code>,
    always: HashMap<String, Code>,
    fault_kinds: HashMap<String, std::io::ErrorKind>,
    nth_faults: HashMap<String, (u32, Code, std::io::ErrorKind)>,
    call_counts: HashMap<String, u32>,
```

Those five are call-name keyed. `calls: Vec<String>` also stays — see Step 6.

Then REPLACE `not_files: std::collections::HashSet<String>` with:

```rust
    /// path -> what `metadata` reports it as. One map, not three overlapping sets:
    /// `directories`, `symlinks` and `not_files` as separate sets could put one path
    /// in two of them at once and express a state no filesystem has. A path absent
    /// from this map but present in `files` is a regular file.
    types: HashMap<PathBuf, flux_fs::FileType>,
```

**Fix the imports in the same step, or Task 5 will not compile.** Lines 8 and 10 currently read

```rust
use std::collections::HashMap;
use std::path::Path;
```

`HashSet` is NOT imported — the two existing uses at `:26` and `:28` are fully qualified as
`std::collections::HashSet<String>`. Task 5 adds `directories: HashSet<PathBuf>` unqualified, so import
it now and convert those two to the short form while you are in the struct:

```rust
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
```

- [ ] **Step 3: Widen the helper signatures**

Every one of these ten changes its first parameter from `path: &str` to `path: impl AsRef<Path>`, and
its body starts by binding `let path = path.as_ref();`:

`write_file`, `exists`, `read_file`, `set_identity`, `permissions`, `modified`, `set_file_perms`,
`add_special`, `vanish_on_second_metadata`, `grow_on_second_metadata`

`write_file` also keeps its second parameter `bytes: &[u8]` unchanged. Example, for `write_file`:

```rust
    /// Seed a source file.
    pub fn write_file(&self, path: impl AsRef<Path>, bytes: &[u8]) {
        let path = path.as_ref();
        let mut g = self.inner.lock().unwrap();
        g.files.insert(path.to_path_buf(), bytes.to_vec());
        mint_identity(&mut g, path);
    }
```

`add_special` additionally records a type. Change its body to:

```rust
    pub fn add_special(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        let mut g = self.inner.lock().unwrap();
        g.files.insert(path.to_path_buf(), Vec::new());
        g.types.insert(path.to_path_buf(), flux_fs::FileType::Other);
        mint_identity(&mut g, path);
    }
```

and add its sibling, which Task 5's walk tests need:

```rust
    /// Seed a symlink. The fake never follows it -- the walk must REPORT symlinks
    /// and never descend into them, so a target would be unused state.
    pub fn add_symlink(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        let mut g = self.inner.lock().unwrap();
        g.files.insert(path.to_path_buf(), Vec::new());
        g.types.insert(path.to_path_buf(), flux_fs::FileType::Symlink);
        mint_identity(&mut g, path);
    }
```

- [ ] **Step 4: Widen the three free functions**

`mint_identity`, `missing_source` and `move_object` take `&str` path arguments today. Change each to
`&Path`, and inside them replace `path.to_string()` / `to.to_string()` with `path.to_path_buf()` /
`to.to_path_buf()`. Their logic is UNCHANGED — the identity-replacement semantics in `move_object` and
the early return in `mint_identity` are load-bearing and were each pinned by a test in PR 1.

**ORACLE:** `fault_fs::tests::writing_over_an_existing_path_preserves_its_identity`,
`an_identity_follows_the_object_through_a_rename`,
`a_rename_replaces_the_destination_rather_than_merging_with_it`,
`removing_a_path_does_not_leak_its_state_to_a_later_file`,
`renaming_a_missing_source_fails_instead_of_conjuring_the_destination`,
`a_path_recreated_after_a_rename_is_a_different_object`. All six must stay green. If a change would alter
what any of them observes, STOP and report — the tests win, do not edit them to match.

- [ ] **Step 5: Widen the trait methods' key derivation**

In `impl FileSystem for FaultFs`, replace each `let p = path.display().to_string();` with
`let p = path.to_path_buf();`, and in `rename_replace`/`rename_no_replace` replace
`let (f, t) = (from.display().to_string(), to.display().to_string());` with
`let (f, t) = (from.to_path_buf(), to.to_path_buf());`.

Then `metadata`'s `file_type` line (the interim form from Task 3) becomes:

```rust
            file_type: g.types.get(&p).copied().unwrap_or(flux_fs::FileType::File),
```

- [ ] **Step 6: Leave the call log alone, and say why in the file**

`calls: Vec<String>` stays, and the `record` call sites keep `format!("metadata({})", p.display())`.
Add this comment above the `calls` field:

```rust
    /// The ORDER of calls, which is what every assertion on it checks. Formatted with
    /// the lossy `display()` deliberately: this is an order oracle, not a path oracle,
    /// and no test distinguishes two paths by this string. The MAPS above are keyed by
    /// `PathBuf` because a lossy key there would merge two distinct objects, which is
    /// a correctness bug; a lossy LOG is only ever a less precise assertion message.
    /// If a test ever needs to tell two non-UTF-8 names apart HERE, add a lossless
    /// accessor then -- it is not needed now.
    calls: Vec<String>,
```

- [ ] **Step 7: Run the gate — the churn claim is falsifiable here**

Run: `just check`
Expected: exit 0, `121 tests run: 121 passed, 1 skipped`, and **no edits to `copy.rs`**.

Run: `git status --short`
Expected: ONLY `crates/flux-core/src/fault_fs.rs` modified. If `copy.rs` had to change, the "all 68 call
sites are literals" measurement was wrong — STOP and report it rather than editing `copy.rs` quietly.

- [ ] **Step 8: Commit**

```bash
git add crates/flux-core/src/fault_fs.rs
git commit -m "refactor(core): key FaultFs by PathBuf and unify its type state

display().to_string() is lossy, so two distinct non-UTF-8 names collapsed to
one key and merged two objects' state - the exact case the ordering contract
exists for. Helpers take impl AsRef<Path>, so all 68 existing call sites
compile unchanged.

not_files becomes types: HashMap<PathBuf, FileType>, because three overlapping
sets could put one path in two of them and express a state no filesystem has.

The call log stays Vec<String>: it is an order oracle, not a path oracle.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 5: `read_dir` and `create_dir` across the trait and all three implementors

A trait method with no default breaks every implementor, so the trait change and all three
implementations land in ONE commit. PR 1 measured this the hard way: splitting them broke the Windows
build mid-plan.

**Files:** Modify `crates/flux-fs/src/fs.rs`, `crates/flux-platform/src/std_fs.rs`,
`crates/flux-core/src/fault_fs.rs`, `crates/flux-platform/tests/std_fs.rs`

- [ ] **Step 1: Write the failing real-filesystem tests**

Append to `crates/flux-platform/tests/std_fs.rs`:

```rust
#[test]
fn read_dir_lists_children_with_their_types() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("sub")).unwrap();
    std::fs::write(d.path().join("f.txt"), b"x").unwrap();

    let fs = StdFileSystem;
    let mut got: Vec<(String, flux_fs::FileType)> = fs
        .read_dir(d.path())
        .unwrap()
        .into_iter()
        .map(|e| (e.name.to_string_lossy().into_owned(), e.file_type))
        .collect();
    got.sort();

    assert_eq!(
        got,
        vec![
            ("f.txt".to_string(), flux_fs::FileType::File),
            ("sub".to_string(), flux_fs::FileType::Dir),
        ]
    );
}

#[test]
fn read_dir_on_an_empty_directory_is_empty_not_an_error() {
    // An empty directory is exactly what a tree copy must handle, and the old fake
    // could not even express one.
    let d = tempfile::tempdir().unwrap();
    assert!(StdFileSystem.read_dir(d.path()).unwrap().is_empty());
}

#[test]
fn read_dir_on_a_missing_path_fails() {
    let d = tempfile::tempdir().unwrap();
    assert!(StdFileSystem.read_dir(&d.path().join("nope")).is_err());
}

#[test]
fn create_dir_makes_one_level_and_refuses_a_missing_parent() {
    let d = tempfile::tempdir().unwrap();
    let fs = StdFileSystem;

    fs.create_dir(&d.path().join("a")).unwrap();
    assert_eq!(fs.metadata(&d.path().join("a")).unwrap().file_type, flux_fs::FileType::Dir);

    // Fails if the parent is missing: the walk creates ancestors in order, so it
    // never needs the recursive form, and silently creating them would hide a bug.
    assert!(fs.create_dir(&d.path().join("missing/b")).is_err());
    // And refuses to replace something that is already there.
    assert!(fs.create_dir(&d.path().join("a")).is_err());
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p flux-platform --test std_fs 2>&1 | tail -20`
Expected: FAILS to compile — `no method named `read_dir` found`.

- [ ] **Step 3: Add the two trait methods**

In `crates/flux-fs/src/fs.rs`, inside `pub trait FileSystem`, after `remove_file` (line 108):

```rust
    /// ONE level. Returns every entry with its file type, in whatever order the OS
    /// gave them -- ordering is the caller's job (§7.2), because only the caller
    /// knows the comparison rule.
    ///
    /// Returns a `Vec`, not an iterator, and that is deliberate: §7.2 requires each
    /// directory to be sorted, sorting requires the whole directory in hand, so a
    /// lazy return shape would promise a laziness the caller cannot use.
    /// Materialising also drops the directory handle before the walk recurses, so
    /// only one is ever open.
    ///
    /// This is within invariant 11, which forbids an unbounded GLOBAL list -- not
    /// §7.2, which is about ordering and would equally appear to bless something
    /// genuinely unbounded. Measured at 200,000 entries in one ext4 directory:
    /// readdir 448 ms, 10.1 MB, bytewise sort 81 ms.
    ///
    /// State the bound precisely, because "one directory" is the easy thing to say
    /// and it is wrong: only one directory HANDLE is ever open, but the walk's stack
    /// retains the un-yielded entries of every directory on the CURRENT PATH, so the
    /// live bound is one directory per level, capped by `DEFAULT_MAX_DEPTH`. That is
    /// still bounded by DEPTH rather than by the total number of files, which is
    /// what invariant 11 actually forbids -- but it is not "one".
    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>>;

    /// Creates ONE directory. Fails if the parent is missing; the walk creates
    /// ancestors in order, so it never needs the recursive form.
    fn create_dir(&self, path: &Path) -> Result<()>;
```

- [ ] **Step 4: Implement on `NullFs`**

In `mod tests` in the same file, add to `impl FileSystem for NullFs`:

```rust
        fn read_dir(&self, _: &Path) -> crate::Result<Vec<DirEntry>> {
            Ok(Vec::new())
        }

        fn create_dir(&self, _: &Path) -> crate::Result<()> {
            Ok(())
        }
```

- [ ] **Step 5: Implement on `StdFileSystem`**

In `crates/flux-platform/src/std_fs.rs`, inside `impl FileSystem for StdFileSystem`:

```rust
    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path).map_err(FsError::from_io)? {
            let entry = entry.map_err(FsError::from_io)?;
            // `DirEntry::file_type` does NOT follow a symlink, and on Windows the
            // reparse tag arrives in `wfd.dwReserved0` as part of the enumeration
            // itself, so this costs no extra syscall there.
            let t = entry.file_type().map_err(FsError::from_io)?;
            out.push(DirEntry {
                name: entry.file_name(),
                file_type: if t.is_symlink() {
                    FileType::Symlink
                } else if t.is_dir() {
                    FileType::Dir
                } else if t.is_file() {
                    FileType::File
                } else {
                    FileType::Other
                },
            });
        }
        Ok(out)
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        std::fs::create_dir(path).map_err(FsError::from_io)
    }
```

Add `DirEntry` and `FileType` to that file's `flux_fs::{...}` import (line 8).

- [ ] **Step 6: Implement on `FaultFs`**

In `crates/flux-core/src/fault_fs.rs`, first extend the `flux_fs` import — this is the task where
`DirEntry` is finally named, and Task 3 deliberately left it out to avoid an `unused_imports` failure:

```rust
use flux_fs::{Code, DirEntry, FileHandle, FileSystem, FileType, FsError, Metadata, Perms, Result};
```

Then add `directories: HashSet<PathBuf>` to `Inner` (alongside `types`), and inside
`impl FileSystem for FaultFs`:

```rust
    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let p = path.to_path_buf();
        self.record(format!("read_dir({})", p.display()), "read_dir")?;
        let g = self.inner.lock().unwrap();
        if !g.directories.contains(&p) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        // Immediate children only, from both maps. Unsorted deliberately: the trait
        // says ordering is the caller's job, and a fake that pre-sorted would let a
        // walk with NO sort pass its ordering test.
        let mut out = Vec::new();
        for key in g.files.keys().chain(g.directories.iter()) {
            if key.parent() != Some(p.as_path()) {
                continue;
            }
            let Some(name) = key.file_name() else { continue };
            let file_type = if g.directories.contains(key) {
                FileType::Dir
            } else {
                g.types.get(key).copied().unwrap_or(FileType::File)
            };
            out.push(DirEntry { name: name.to_os_string(), file_type });
        }
        Ok(out)
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        let p = path.to_path_buf();
        self.record(format!("create_dir({})", p.display()), "create_dir")?;
        let mut g = self.inner.lock().unwrap();
        if g.directories.contains(&p) || g.files.contains_key(&p) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        // Fails if the parent is missing, as the trait says and `std::fs::create_dir`
        // does. The walk root itself has no parent in the fake, so a root-level
        // create is allowed.
        if let Some(parent) = p.parent()
            && !parent.as_os_str().is_empty()
            && parent != Path::new("/")
            && !g.directories.contains(parent)
        {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        g.directories.insert(p.clone());
        g.types.insert(p.clone(), FileType::Dir);
        mint_identity(&mut g, &p);
        Ok(())
    }
```

`metadata` must now also answer for directories, which are NOT in `files`. Change its length lookup from:

```rust
        let len = g.files.get(&p).map(|b| b.len() as u64).ok_or_else(|| {
```

to:

```rust
        let len = match g.files.get(&p) {
            Some(b) => b.len() as u64,
            // A directory has no bytes, and is not in `files`.
            None if g.directories.contains(&p) => 0,
            None => {
                return Err(FsError::new(
                    Code::IoError,
                    std::io::Error::from(std::io::ErrorKind::NotFound),
                ));
            }
        };
```

(keeping the surrounding `Ok(Metadata { ... })` unchanged), and `exists` becomes:

```rust
    pub fn exists(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        let g = self.inner.lock().unwrap();
        g.files.contains_key(path) || g.directories.contains(path)
    }
```

- [ ] **Step 7: Add the fake's own tests**

Append to `mod tests` in `fault_fs.rs`:

```rust
    #[test]
    fn create_dir_then_read_dir_round_trips_through_the_trait() {
        // C1: the walk's fixtures are built with create_dir, so it has a consumer in
        // this PR rather than waiting for copy_tree.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/a")).unwrap();
        fs.create_dir(Path::new("/a/sub")).unwrap();
        fs.write_file("/a/f", b"x");
        fs.add_symlink("/a/link");

        let mut got: Vec<(String, FileType)> = fs
            .read_dir(Path::new("/a"))
            .unwrap()
            .into_iter()
            .map(|e| (e.name.to_string_lossy().into_owned(), e.file_type))
            .collect();
        got.sort();

        assert_eq!(
            got,
            vec![
                ("f".to_string(), FileType::File),
                ("link".to_string(), FileType::Symlink),
                ("sub".to_string(), FileType::Dir),
            ]
        );
    }

    #[test]
    fn an_empty_directory_is_distinguishable_from_a_special_file() {
        // The design document names this as the reason the fake had to change: the
        // old `not_files` set made an empty directory and a device node identical.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        fs.add_special("/dev");

        assert!(fs.read_dir(Path::new("/d")).unwrap().is_empty());
        assert_eq!(fs.metadata(Path::new("/d")).unwrap().file_type, FileType::Dir);
        assert_eq!(fs.metadata(Path::new("/dev")).unwrap().file_type, FileType::Other);
        assert!(fs.read_dir(Path::new("/dev")).is_err());
    }

    #[test]
    fn create_dir_refuses_a_missing_parent_and_an_occupied_name() {
        let fs = FaultFs::new();
        assert!(fs.create_dir(Path::new("/a/b")).is_err(), "parent /a does not exist");
        fs.create_dir(Path::new("/a")).unwrap();
        assert!(fs.create_dir(Path::new("/a")).is_err(), "already a directory");
        fs.write_file("/f", b"x");
        assert!(fs.create_dir(Path::new("/f")).is_err(), "occupied by a file");
    }
```

- [ ] **Step 8: Gate**

Run: `just check`
Expected: exit 0, `128 tests run: 128 passed, 1 skipped` (121 + 4 std_fs + 3 flux-core).
`flux-core` 40, `flux-platform::std_fs` 17 on Windows.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "feat(fs): add read_dir and create_dir across the trait and all implementors

One commit, because a trait method with no default breaks every implementor
and splitting it broke the Windows build mid-plan in PR 1.

read_dir returns one level as an unsorted Vec: ordering is the caller's job
per 7.2, and a fake that pre-sorted would let a walk with no sort pass its
own ordering test. FaultFs gains a directories set, so an empty directory is
finally distinguishable from a device node.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 6: `Code::DirectoryChangedDuringScan`

**Files:** Modify `crates/flux-fs/src/error.rs`

- [ ] **Step 1: Extend the pinning test**

In `error.rs`'s `mod tests`, add one line to `every_code_has_the_spec_string`:

```rust
        assert_eq!(Code::DirectoryChangedDuringScan.as_str(), "DIRECTORY_CHANGED_DURING_SCAN");
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p flux-fs --lib every_code_has_the_spec_string 2>&1 | tail -10`
Expected: FAILS to compile — `no variant named `DirectoryChangedDuringScan``.

- [ ] **Step 3: Add the variant**

In `crates/flux-fs/src/error.rs`, add to `enum Code` after `SafetyRejected` (line 19):

```rust
    /// §2 item 83, §149.4. A DIRECTORY whose object changed under the walk -- not to
    /// be confused with `SourceChanged`, which is a FILE whose length or mtime moved
    /// under `copy_file` (§33). Different objects, different producers, no overlap.
    DirectoryChangedDuringScan,
```

and to `as_str`'s match, after the `SafetyRejected` arm:

```rust
            Code::DirectoryChangedDuringScan => "DIRECTORY_CHANGED_DURING_SCAN",
```

- [ ] **Step 4: Gate and commit**

Run: `just check` — expected exit 0, `128 tests run: 128 passed, 1 skipped` (no new test, one extended).

```bash
git add crates/flux-fs/src/error.rs
git commit -m "feat(fs): add Code::DirectoryChangedDuringScan

Spec item 83 and 149.4. Its producer arrives in Task 12; the variant lands
here so the taxonomy test covers it from the start.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 7: `WalkEvent`, `WalkError`, and fallible `walk` initialisation

**Files:** Create `crates/flux-core/src/walk.rs`; modify `crates/flux-core/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/flux-core/src/walk.rs` with ONLY this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;

    #[test]
    fn walk_refuses_a_root_that_is_not_a_directory() {
        let fs = FaultFs::new();
        fs.write_file("/f", b"x");
        let e = walk(&fs, Path::new("/f")).unwrap_err();
        assert_eq!(e.code, Code::SpecialFileUnsupported);
    }

    #[test]
    fn walk_refuses_a_root_that_does_not_exist() {
        let fs = FaultFs::new();
        assert!(walk(&fs, Path::new("/nope")).is_err());
    }

    #[test]
    fn an_empty_root_yields_nothing() {
        // No event for the root itself: the root IS the destination mapping, not
        // part of the tree being copied into it.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();
        let got: Vec<_> = walk(&fs, Path::new("/r")).unwrap().collect();
        assert!(got.is_empty(), "got {got:?}");
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p flux-core --lib walk 2>&1 | tail -10`
Expected: FAILS — `walk.rs` is not a module yet.

- [ ] **Step 3: Register the module**

In `crates/flux-core/src/lib.rs`, add `pub mod walk;` and
`pub use walk::{DEFAULT_MAX_DEPTH, Walk, WalkError, WalkEvent, walk, walk_with_depth};`.

- [ ] **Step 4: Write the types and the constructor**

Prepend to `crates/flux-core/src/walk.rs`, above the test module:

```rust
//! The ordered directory walk (§7.2, §9, §149.4).

use flux_fs::{
    Code, DirEntry, FileIdentity, FileSystem, FileType, FsError, ObjectId, Result,
};
use std::path::{Path, PathBuf};

/// Chosen to sit BELOW the `PATH_MAX`-implied bound of roughly 370 levels rather
/// than above it, so this guard is the operative one both now and after any move to
/// relative traversal -- the behaviour does not change out from under a user when
/// the traversal does. MEASURED on WSL Ubuntu-26.04: nested directories built with
/// relative paths and `chdir` reached 5000 levels at a path length of 55,020 bytes,
/// over thirteen times `getconf PATH_MAX`, so the kernel enforces no cumulative
/// depth limit of its own. Real trees are an order of magnitude shallower than 256.
pub const DEFAULT_MAX_DEPTH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkEvent {
    /// Pre-order: the directory exists and is about to be descended into.
    Dir { path: PathBuf, identity: FileIdentity },
    File { path: PathBuf },
    Symlink { path: PathBuf },
    Other { path: PathBuf },
    /// Post-order: every descendant of this directory has been yielded.
    ///
    /// Not speculative generality. §30.2 requires that directory metadata sensitive
    /// to child mutations is not finalized before the children are written, so a
    /// directory is created pre-order and finalized post-order. A pre-order-only
    /// stream cannot express "leaving this directory", so a consumer would have to
    /// re-derive it by comparing path prefixes -- fragile exactly where §7.2's
    /// component-wise ordering is subtle.
    ///
    /// THIS walk emits it. PR 3 does NOT consume it, because directory metadata is
    /// not preserved in this cut -- `set_times` takes a `&Self::Writer`, a file
    /// handle, and widening that surface is a separate decision. It is emitted now
    /// precisely so the walker does not change when directory metadata arrives.
    DirEnd { path: PathBuf },
}

/// One failed entry. NOT terminal: the walk reports it, skips that subtree, and
/// continues with the next sibling.
#[derive(Debug)]
pub struct WalkError {
    /// Relative to the walk root, like every path in a `WalkEvent`.
    pub path: PathBuf,
    pub cause: FsError,
}

/// One directory being enumerated.
struct Frame {
    /// Relative to the walk root. Empty for the root frame.
    rel: PathBuf,
    entries: std::vec::IntoIter<DirEntry>,
    /// Whether this frame pushed onto `ancestors`, so the pop stays balanced when
    /// the identity was not `Strong` and nothing was pushed.
    pushed_ancestor: bool,
    /// False for the root frame, which emits no event of any kind.
    emit_end: bool,
}

/// Borrows the filesystem for the life of the walk; holds the explicit stack, the
/// ancestor set, and the root it makes paths relative to.
pub struct Walk<'a, F: FileSystem> {
    fs: &'a F,
    root: PathBuf,
    max_depth: usize,
    stack: Vec<Frame>,
    /// The `ObjectId`s of the directories on the CURRENT path -- not a global
    /// visited set, which would grow with the total number of directories and is
    /// forbidden by spec line 997. An ancestor set is bounded by depth and is
    /// sufficient, because a cycle by definition re-enters an ancestor.
    ///
    /// Only `Strong` identities are ever pushed, so "both sides Strong" holds by
    /// construction rather than by a second check at comparison time.
    ancestors: Vec<ObjectId>,
}

/// Sort one directory by `name.as_encoded_bytes()`, ascending. Nothing else.
///
/// §7.2: "A depth-first scanner that sorts each directory's entries by name alone
/// emits component-wise order without buffering." Depth-first plus this gives
/// `a/x  a/y/z  a-b  a0`; a flat `/`-joined sort gives `a-b  a/x  a/y/z  a0`, which
/// §7.2 calls wrong.
///
/// `as_encoded_bytes()` is exactly the §241 representation on both platforms, with
/// no conversion and no allocation. `OsStrExt::encode_wide()` must NEVER be used
/// here -- see the test in `flux-fs`.
fn sorted(mut v: Vec<DirEntry>) -> Vec<DirEntry> {
    v.sort_by(|a, b| a.name.as_encoded_bytes().cmp(b.name.as_encoded_bytes()));
    v
}

/// `root` must name a directory. The walk yields NO event for the root itself --
/// its first event is the root's first child -- because the root is not part of the
/// tree being copied INTO the destination, it IS the destination mapping.
pub fn walk<'a, F: FileSystem>(fs: &'a F, root: &Path) -> Result<Walk<'a, F>> {
    walk_with_depth(fs, root, DEFAULT_MAX_DEPTH)
}

/// As `walk`, with the depth cap chosen explicitly.
pub fn walk_with_depth<'a, F: FileSystem>(
    fs: &'a F,
    root: &Path,
    max_depth: usize,
) -> Result<Walk<'a, F>> {
    // Fallible BEFORE anything is yielded: a missing root, or a root that is not a
    // directory, is a failure of the whole operation rather than a per-entry error,
    // so it is reported through `Result` and not as a first `Err` item.
    let m = fs.metadata(root)?;
    if m.file_type != FileType::Dir {
        return Err(FsError::new(
            Code::SpecialFileUnsupported,
            std::io::Error::other("walk root is not a directory"),
        ));
    }
    let entries = sorted(fs.read_dir(root)?);

    // The root goes into the ancestor set even though no event is emitted for it.
    // Load-bearing, not tidy: the measured bind-mount case is `mount --bind a a/b`,
    // where the re-entered directory IS the root, so a walk that only tracked
    // directories it emitted events for would copy the whole tree twice.
    let mut ancestors = Vec::new();
    let pushed_ancestor = match m.identity {
        FileIdentity::Strong(id) => {
            ancestors.push(id);
            true
        }
        _ => false,
    };

    Ok(Walk {
        fs,
        root: root.to_path_buf(),
        max_depth,
        stack: vec![Frame {
            rel: PathBuf::new(),
            entries: entries.into_iter(),
            pushed_ancestor,
            emit_end: false,
        }],
        ancestors,
    })
}
```

- [ ] **Step 5: Do NOT gate or commit yet. Go straight to Task 8.**

**Tasks 7 and 8 are ONE commit, and this is forced, not stylistic.** An earlier draft of this plan put a
placeholder `fn next(&mut self) -> Option<Self::Item> { None }` here so Task 7 could stand alone. That
does not build. A `next` that reads no field leaves every field of `Walk` unread, and `dead_code` is
`deny`-level under the repo's `clippy -D warnings` gate. MEASURED with a reduced case:

```
error: fields `fs`, `root`, `max_depth`, `stack`, and `ancestors` are never read
 --> dead.rs:3:5
  = note: `#[deny(dead_code)]` implied by `#[deny(warnings)]`
```

So `just check` FAILS at the end of Task 7 in isolation. The struct is not lintable until something
reads it, and the only thing that reads it is the iterator.

Leave the tree uncommitted and continue into Task 8, which supplies `next`, runs the gate, and commits
both tasks together. Task 7's three tests are authored now and first RUN in Task 8 — in particular
`an_empty_root_yields_nothing`, which against a placeholder would have passed vacuously and against the
real iterator is load-bearing.

This is the only place in the plan where a task does not end at a green commit, and it is called out
here rather than discovered at the gate.

---

## Task 8: The ordered walk

**Files:** Modify `crates/flux-core/src/walk.rs`

- [ ] **Step 1: Write the failing tests**

Add to `walk.rs`'s `mod tests`:

```rust
    /// `/r/a/x`, `/r/a/y/z`, `/r/a-b`, `/r/a0` -- the design document's own
    /// normative §7.2 example, which distinguishes component-wise order from a flat
    /// `/`-joined sort.
    fn component_order_tree() -> FaultFs {
        let fs = FaultFs::new();
        for d in ["/r", "/r/a", "/r/a/y"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/r/a/x", b"");
        fs.write_file("/r/a/y/z", b"");
        fs.write_file("/r/a-b", b"");
        fs.write_file("/r/a0", b"");
        fs
    }

    /// Render a relative path with `/` on EVERY platform.
    ///
    /// `Display` must NOT be used for this. MEASURED on Windows:
    /// `PathBuf::new().join("a").join("y").join("z").display().to_string()` is
    /// `"a\\y\\z"`, so an assertion written against `"a/y/z"` fails on the primary
    /// dev platform while passing on Linux. Joining COMPONENTS sidesteps the
    /// separator entirely and was measured to give `"a/y/z"` on both.
    ///
    /// This is only about the ASSERTION's spelling. `Path` itself compares, hashes
    /// and takes `parent()` separator-insensitively on Windows -- also measured --
    /// so the fake's `PathBuf` keys are fine either way.
    fn rel(p: &Path) -> String {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    }

    fn paths(fs: &FaultFs) -> Vec<String> {
        walk(fs, Path::new("/r"))
            .unwrap()
            .map(|r| match r.unwrap() {
                WalkEvent::Dir { path, .. } => format!("D {}", rel(&path)),
                WalkEvent::File { path } => format!("F {}", rel(&path)),
                WalkEvent::Symlink { path } => format!("L {}", rel(&path)),
                WalkEvent::Other { path } => format!("O {}", rel(&path)),
                WalkEvent::DirEnd { path } => format!("E {}", rel(&path)),
            })
            .collect()
    }

    #[test]
    fn it_emits_component_wise_order() {
        let got = paths(&component_order_tree());
        assert_eq!(
            got,
            vec![
                "D a", "F a/x", "D a/y", "F a/y/z", "E a/y", "E a",
                "F a-b", "F a0",
            ],
            "component-wise order: a/* sorts before a-b and a0, because the sort is\n\
             per-directory and NOT over `/`-joined paths"
        );
    }

    #[test]
    fn paths_are_relative_to_the_root_and_the_root_emits_nothing() {
        let got = paths(&component_order_tree());
        assert!(!got.iter().any(|p| p.contains("/r")), "paths are relative; got {got:?}");
        assert!(!got.is_empty());
    }

    #[test]
    fn every_dir_is_closed_by_a_dir_end_in_post_order() {
        let got = paths(&component_order_tree());
        let opened: Vec<&String> = got.iter().filter(|p| p.starts_with("D ")).collect();
        let closed: Vec<&String> = got.iter().filter(|p| p.starts_with("E ")).collect();
        assert_eq!(opened.len(), closed.len(), "every Dir needs a DirEnd; got {got:?}");

        // `E a` must come AFTER `F a/y/z`, or a consumer would finalize a parent's
        // metadata before writing its children -- the §30.2 violation DirEnd exists
        // to make expressible.
        let end_a = got.iter().position(|p| p == "E a").unwrap();
        let child = got.iter().position(|p| p == "F a/y/z").unwrap();
        assert!(end_a > child, "DirEnd is post-order; got {got:?}");
    }

    #[test]
    fn symlinks_are_reported_and_never_descended_into() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();
        fs.add_symlink("/r/link");
        // A REAL directory alongside it, with a child. Without this the test could
        // not tell "never descends into a symlink" from "never descends at all",
        // and a walk with the Dir arm deleted entirely would pass.
        fs.create_dir(Path::new("/r/real")).unwrap();
        fs.write_file("/r/real/f", b"");

        // `link` produces ONE event and it is a Symlink -- no Dir, no DirEnd --
        // while `real` produces the full Dir/child/DirEnd triple. The contrast is
        // the assertion; an exhaustive equality carries it without a second
        // redundant assert, which would be true whenever this one is.
        assert_eq!(paths(&fs), vec!["L link", "D real", "F real/f", "E real"]);
    }
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p flux-core --lib walk 2>&1 | tail -25`
Expected: FAILS to compile — `Walk<'_, FaultFs> is not an iterator`, because Task 7 deliberately left
the `Iterator` impl out. (Do not run `just check` here; Task 7's note explains why the tree does not
gate until this task lands.)

- [ ] **Step 3: Add the `Iterator` implementation**

```rust
impl<'a, F: FileSystem> Iterator for Walk<'a, F> {
    type Item = std::result::Result<WalkEvent, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.stack.is_empty() {
                return None;
            }
            // Take the next entry and release the borrow before `enter` needs
            // `&mut self`. `join` allocates the path we were going to need anyway.
            let step = {
                let frame = self.stack.last_mut().expect("stack checked non-empty");
                frame.entries.next().map(|e| (frame.rel.join(&e.name), e.file_type))
            };

            match step {
                Some((path, FileType::File)) => return Some(Ok(WalkEvent::File { path })),
                Some((path, FileType::Symlink)) => return Some(Ok(WalkEvent::Symlink { path })),
                Some((path, FileType::Other)) => return Some(Ok(WalkEvent::Other { path })),
                Some((path, FileType::Dir)) => return Some(self.enter(path)),
                None => {
                    // This directory is exhausted. Unwind it, keeping the ancestor
                    // set balanced, and close it post-order.
                    let f = self.stack.pop().expect("stack checked non-empty");
                    if f.pushed_ancestor {
                        self.ancestors.pop();
                    }
                    if f.emit_end {
                        return Some(Ok(WalkEvent::DirEnd { path: f.rel }));
                    }
                    // The root frame emits nothing; the loop ends on the next pass.
                }
            }
        }
    }
}

impl<'a, F: FileSystem> Walk<'a, F> {
    /// Descend into `rel`, or refuse it. On `Ok` a frame has been pushed and the
    /// pre-order `Dir` event is returned; on `Err` nothing was pushed, the subtree
    /// is skipped, and the walk continues with the next sibling.
    fn enter(&mut self, rel: PathBuf) -> std::result::Result<WalkEvent, WalkError> {
        // The cap runs at EVERY identity strength, and is the only guard that does.
        // `stack.len()` counts frames, and the root frame is one, so a child of the
        // root is depth 1.
        //
        // It lands HERE, with the iterator, rather than in its own later task, and
        // that is forced for the same reason Tasks 7 and 8 are one commit: `Walk`
        // stores `max_depth`, and a field that is written but never READ is
        // `dead_code`, which is deny-level under `clippy -D warnings`. Task 10 adds
        // the TESTS that pin this check; the check itself cannot wait for them.
        if self.stack.len() > self.max_depth {
            return Err(WalkError {
                path: rel,
                cause: FsError::new(
                    Code::IoError,
                    // No OS error exists for "too deep", and no spec code names it.
                    // `IoError` plus a message, rather than misusing a spec code:
                    // `SafetyRejected` aborts the whole operation and this does not.
                    std::io::Error::other("directory depth cap exceeded"),
                ),
            });
        }

        let abs = self.root.join(&rel);

        let m = match self.fs.metadata(&abs) {
            Ok(m) => m,
            Err(cause) => return Err(WalkError { path: rel, cause }),
        };

        let entries = match self.fs.read_dir(&abs) {
            Ok(v) => sorted(v),
            Err(cause) => return Err(WalkError { path: rel, cause }),
        };

        self.stack.push(Frame {
            rel: rel.clone(),
            entries: entries.into_iter(),
            pushed_ancestor: false,
            emit_end: true,
        });
        Ok(WalkEvent::Dir { path: rel, identity: m.identity })
    }
}
```

Tasks 10, 11 and 12 each insert one check into `enter`. The FINAL order in the body, which is what
matters and is not the order the tasks are written in, is:

1. **depth cap** (Task 10) — first, before even the `metadata` call, so a runaway descent costs no stat.
2. `metadata`
3. **type consistency** (Task 12) — the object is still a directory.
4. **cycle** (Task 11) — its `ObjectId` is not already an ancestor.
5. `read_dir`

Each task states where its own check goes; this list is the invariant they must add up to.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p flux-core --lib walk`
Expected: PASS, and `flux-core` is now 47.

- [ ] **Step 5: Prove the ordering test is non-vacuous**

Temporarily change `sorted` to return `v` unmodified (delete the `sort_by` line).

Run: `cargo test -p flux-core --lib it_emits_component_wise_order 2>&1 | tail -15`
Expected: FAILS. `FaultFs::read_dir` iterates a `HashMap`, so its order is arbitrary — which is exactly
why it must not pre-sort.

**Revert the mutant surgically.**

- [ ] **Step 6: Gate and commit**

Run: `just check` — expected exit 0, `135 tests run: 135 passed, 1 skipped`.

This commit covers **Tasks 7 AND 8** — see Task 7 Step 5 for why they cannot be split.

```bash
git add crates/flux-core/src/walk.rs crates/flux-core/src/lib.rs
git commit -m "feat(core): the ordered depth-first walk

WalkEvent, WalkError, the fallible walk initialisation, the iterator and the
depth-cap check, in one commit: a Walk whose next() reads no field leaves every field dead, and
dead_code is deny-level under the repo's clippy gate, so the type does not
build until the iterator that reads it exists. The depth-cap check rides along
for the same reason: Walk stores max_depth, and a field written but never read
is dead_code too. Its tests follow in their own commit.

Per-directory byte sort plus depth-first gives component-wise order, pinned
against the spec's own normative example: a/x a/y/z a-b a0, which a flat
slash-joined sort would order differently.

A missing root, or a root that is not a directory, fails the whole operation
through Result rather than as a first Err item. The root's ObjectId enters
the ancestor set immediately although no event is emitted for it: the
measured bind-mount cycle re-enters the root itself.

Dir is pre-order and DirEnd post-order, so a consumer can obey 30.2 without
re-deriving \"leaving this directory\" from path prefixes. Symlinks are
reported and never descended into.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 9: An `Err` item is not terminal

This is the single easiest contract here to get wrong, because most fallible Rust iterators stop.

**Files:** Modify `crates/flux-core/src/walk.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn an_unreadable_directory_does_not_end_the_walk() {
        // Spec item 83: the directory's subtree is not transferred, the error is
        // reported, and the operation exits 1 -- but the WALK completes. Every
        // sibling of the failing directory must still be yielded.
        let fs = FaultFs::new();
        for d in ["/r", "/r/aaa", "/r/bbb", "/r/ccc"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/r/aaa/f", b"");
        fs.write_file("/r/bbb/f", b"");
        fs.write_file("/r/ccc/f", b"");

        // `read_dir` is called once per directory, so a one-shot fault aimed at it
        // would be eaten by the FIRST call -- the root's. `fail_nth` targets the
        // second call, which is /r/aaa. This trap made a test pass vacuously once.
        fs.fail_nth("read_dir", 2, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let mut errors = Vec::new();
        let mut files = Vec::new();
        for item in walk(&fs, Path::new("/r")).unwrap() {
            match item {
                Ok(WalkEvent::File { path }) => files.push(rel(&path)),
                Err(e) => errors.push((rel(&e.path), e.cause.code)),
                Ok(_) => {}
            }
        }

        assert_eq!(errors.len(), 1, "exactly one error; got {errors:?}");
        assert_eq!(errors[0].0, "aaa");
        assert_eq!(errors[0].1, Code::PermissionDenied);
        // The siblings AFTER the failure are the whole point.
        assert_eq!(files, vec!["bbb/f".to_string(), "ccc/f".to_string()]);
    }
```

- [ ] **Step 2: Run it**

Run: `cargo test -p flux-core --lib an_unreadable_directory 2>&1 | tail -15`
Expected: **PASSES** against Task 8's code, because `enter` already returns `Err` without pushing a
frame and `next` loops on.

This is a characterisation test, not a red-then-green one, and that is the honest framing. Its value is
in Step 3.

- [ ] **Step 3: Prove it is non-vacuous**

Temporarily change `next`'s `Dir` arm from `return Some(self.enter(path))` to:

```rust
                Some((path, FileType::Dir)) => match self.enter(path) {
                    Ok(ev) => return Some(Ok(ev)),
                    Err(e) => return Some(Err(e)),  // and then stop
                },
```

...no — that is the same behaviour. Use the mutant that actually expresses "terminal": make `enter`'s
`read_dir` failure clear the stack before returning:

```rust
            Err(cause) => {
                self.stack.clear();
                return Err(WalkError { path: rel, cause });
            }
```

Run: `cargo test -p flux-core --lib an_unreadable_directory 2>&1 | tail -15`
Expected: FAILS — `files` is empty, because the walk stopped at the error.

**Revert the mutant surgically.**

- [ ] **Step 4: Document the contract where it is easiest to break**

Add above `impl Iterator for Walk`:

```rust
/// **An `Err` item is NOT terminal.** The iterator yields the error, skips that
/// subtree, and continues with the next sibling. Most fallible Rust iterators stop,
/// which is exactly why this is stated here.
///
/// Spec item 83, verbatim: "`DIRECTORY_CHANGED_DURING_SCAN` has one outcome: the
/// directory's subtree is not transferred, the error is reported, and the operation
/// exits 1. There is no configured mutation policy or rescan alternative." The
/// operation's exit status is 1 even though the walk COMPLETED.
///
/// The spec mandates this shape only for identity changes; applying it to every
/// per-directory failure is a design decision, taken because the alternative is
/// aborting a whole tree copy over one denied subdirectory.
///
/// A pull iterator, because §9 requires that when downstream capacity is exhausted
/// the "scanner blocks/awaits" rather than accumulating paths. A consumer that stops
/// pulling IS the backpressure, so Phase 3's bounded queue sits between this
/// iterator and the workers without the walker changing.
```

- [ ] **Step 5: Gate and commit**

Run: `just check` — expected exit 0, `136 tests run: 136 passed, 1 skipped`.

```bash
git add crates/flux-core/src/walk.rs
git commit -m "test(core): pin that an Err item does not end the walk

Item 83: the subtree is skipped, the error reported, the walk continues.
Proven non-vacuous against a mutant that clears the stack on failure, which
turns the test red with no files yielded after the error.

fail_nth targets read_dir's SECOND call, because a one-shot fault would be
eaten by the root's own listing - the trap that made a test pass vacuously
once already.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 10: Pin the depth cap (fork D1, error shape E4)

The CHECK landed in Task 8 -- see Task 8 Step 3. This task supplies the tests that pin it and the
mutant that proves they are worth having.

**Files:** Modify `crates/flux-core/src/walk.rs`

- [ ] **Step 1: Write the tests**

```rust
    #[test]
    fn the_depth_cap_skips_the_subtree_and_continues() {
        let fs = FaultFs::new();
        for d in ["/r", "/r/a", "/r/a/b", "/r/a/b/c"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/r/a/b/c/deep", b"");
        fs.write_file("/r/sibling", b"");

        // Cap of 2: `a` (1) and `a/b` (2) are entered; `a/b/c` is refused.
        let mut errors = Vec::new();
        let mut files = Vec::new();
        for item in walk_with_depth(&fs, Path::new("/r"), 2).unwrap() {
            match item {
                Ok(WalkEvent::File { path }) => files.push(rel(&path)),
                Err(e) => errors.push(rel(&e.path)),
                Ok(_) => {}
            }
        }

        assert_eq!(errors, vec!["a/b/c".to_string()]);
        assert!(!files.iter().any(|f| f.contains("deep")), "subtree skipped; got {files:?}");
        // Not an abort: the sibling after the refusal still arrives.
        assert_eq!(files, vec!["sibling".to_string()]);
    }

    #[test]
    fn the_default_cap_is_256() {
        assert_eq!(DEFAULT_MAX_DEPTH, 256);
    }
```

- [ ] **Step 2: Run them — they PASS, and that is expected**

Run: `cargo test -p flux-core --lib the_depth_cap 2>&1 | tail -15`
Expected: **PASS.** The check itself already landed in Task 8, because `Walk` stores `max_depth` and a
field that is written but never read is `dead_code`, which is deny-level under `clippy -D warnings`. The
check could not wait for this task.

So these are CHARACTERISATION tests, exactly like Task 9's, and they are worth precisely what Step 3's
mutant proves. Do not skip Step 3 on the grounds that they are green.

- [ ] **Step 3: Prove they are non-vacuous**

Temporarily change `>` to `>=` in the cap check.

Run: `cargo test -p flux-core --lib the_depth_cap_skips 2>&1 | tail -15`
Expected: FAILS — the error path becomes `a/b`, not `a/b/c`, so the off-by-one is caught.

**Revert the mutant surgically.**

- [ ] **Step 4: Gate and commit**

Run: `just check` — expected exit 0, `138 tests run: 138 passed, 1 skipped`.

```bash
git add crates/flux-core/src/walk.rs
git commit -m "feat(core): bound descent with a configurable depth cap

256 by default, chosen to sit below the PATH_MAX-implied bound of roughly 370
levels so the guard stays operative after any move to relative traversal.
Exceeding it skips that subtree and continues; it does not abort.

A scalar cap via walk_with_depth rather than a WalkOptions struct: copy_tree's
signature is fixed at one options parameter, and a second options type would
have to be reconciled away in PR 3.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 11: Ancestor-set cycle detection (fork F1a)

**Files:** Modify `crates/flux-core/src/walk.rs`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn a_directory_that_re_enters_an_ancestor_is_refused() {
        // The measured case is `mount --bind a a/b`, where `a` and `a/b` share a
        // dev+ino. `set_identity` reproduces it with no mount and no privilege.
        let fs = FaultFs::new();
        for d in ["/r", "/r/a", "/r/a/b"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/r/a/marker", b"");
        let shared = ObjectId { volume: 1, index: 9999 };
        fs.set_identity("/r/a", FileIdentity::Strong(shared));
        fs.set_identity("/r/a/b", FileIdentity::Strong(shared));

        let mut errors = Vec::new();
        let mut dirs = Vec::new();
        for item in walk(&fs, Path::new("/r")).unwrap() {
            match item {
                Ok(WalkEvent::Dir { path, .. }) => dirs.push(rel(&path)),
                Err(e) => errors.push(rel(&e.path)),
                _ => {}
            }
        }

        assert_eq!(dirs, vec!["a".to_string()], "b is never entered; got {dirs:?}");
        assert_eq!(errors, vec!["a/b".to_string()]);
    }

    #[test]
    fn the_root_is_in_the_ancestor_set_from_the_start() {
        // `mount --bind a a/b` where the re-entered directory IS the root. A walk
        // that only tracked directories it emitted events for would copy the whole
        // tree twice -- the exact defect the check exists to prevent.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();
        fs.create_dir(Path::new("/r/loop")).unwrap();
        let shared = ObjectId { volume: 1, index: 7 };
        fs.set_identity("/r", FileIdentity::Strong(shared));
        fs.set_identity("/r/loop", FileIdentity::Strong(shared));

        let errors: Vec<String> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.err())
            .map(|e| rel(&e.path))
            .collect();
        assert_eq!(errors, vec!["loop".to_string()]);
    }

    #[test]
    fn a_weak_identity_is_not_compared_at_all() {
        // A comparison is no stronger than its weaker operand. Weak and Unavailable
        // behave identically: a value that cannot be trusted is worth no more than
        // one that is absent. PR 3 owns the warning; PR 2 yields the identity on the
        // Dir event and lets the consumer decide.
        let fs = FaultFs::new();
        for d in ["/r", "/r/a", "/r/a/b"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        let shared = ObjectId { volume: 1, index: 5 };
        fs.set_identity("/r/a", FileIdentity::Weak(shared));
        fs.set_identity("/r/a/b", FileIdentity::Weak(shared));

        let errors: Vec<String> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.err())
            .map(|e| rel(&e.path))
            .collect();
        assert!(errors.is_empty(), "weak identity is not compared; got {errors:?}");
    }

    #[test]
    fn the_dir_event_carries_the_identity_for_pr_3_to_act_on() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();
        fs.create_dir(Path::new("/r/w")).unwrap();
        fs.set_identity("/r/w", FileIdentity::Unavailable);

        let got: Vec<FileIdentity> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.ok())
            .filter_map(|e| match e {
                WalkEvent::Dir { identity, .. } => Some(identity),
                _ => None,
            })
            .collect();
        assert_eq!(got, vec![FileIdentity::Unavailable]);
    }
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p flux-core --lib ancestor 2>&1 | tail -20`
Expected: `a_directory_that_re_enters_an_ancestor_is_refused` FAILS — `dirs` is `["a", "a/b"]`.

- [ ] **Step 3: Add the check and the balanced push**

In `enter`, between the `metadata` call and the `read_dir` call:

```rust
        // Only `Strong` is ever pushed OR compared, so "both sides Strong" holds by
        // construction. §107: a Weak identity exists and cannot be trusted, and a
        // comparison is no stronger than its weaker operand. Asymmetry is the normal
        // case -- a local ext4 source against an SMB destination -- so this is the
        // common path, not a corner.
        let mut pushed_ancestor = false;
        if let FileIdentity::Strong(id) = m.identity {
            if self.ancestors.contains(&id) {
                return Err(WalkError {
                    path: rel,
                    cause: FsError::new(
                        Code::IoError,
                        // Not `SafetyRejected`: that aborts the whole operation
                        // under §129 and a cycle explicitly does not, so the same
                        // code would carry two different severities.
                        // `ErrorKind::FilesystemLoop` would be the precise kind but
                        // is unstable on the pinned toolchain (E0658, issue #86442).
                        std::io::Error::other("directory cycle: already an ancestor"),
                    ),
                });
            }
            self.ancestors.push(id);
            pushed_ancestor = true;
        }
```

Then the `read_dir` failure arm must unwind that push:

```rust
        let entries = match self.fs.read_dir(&abs) {
            Ok(v) => sorted(v),
            Err(cause) => {
                // Balance the set: this frame is never pushed, so nothing will pop.
                if pushed_ancestor {
                    self.ancestors.pop();
                }
                return Err(WalkError { path: rel, cause });
            }
        };
```

and the frame carries it: `pushed_ancestor,` instead of `pushed_ancestor: false,`.

- [ ] **Step 4: Run, then prove non-vacuousness twice**

Run: `cargo test -p flux-core --lib` — expected PASS, `flux-core` 54.

**Mutant A — the gate.** Change `if let FileIdentity::Strong(id) = m.identity` to also accept `Weak`
(match both arms into `id`).
Expected: `a_weak_identity_is_not_compared_at_all` FAILS. Revert surgically.

**Mutant B — the balance.** Delete the `if pushed_ancestor { self.ancestors.pop(); }` from the
`read_dir` error arm.

Run: `cargo test -p flux-core --lib 2>&1 | tail -20`
Expected: **this may well stay GREEN**, because no current test both pushes an ancestor and then fails
`read_dir`. If it does stay green, that is a real coverage gap — add this test rather than shrugging:

```rust
    #[test]
    fn a_read_dir_failure_after_pushing_an_ancestor_leaves_the_set_balanced() {
        // Without the unwind, `a`'s id stays in the ancestor set forever and its
        // SIBLING `b`, which shares no id, is still fine -- but a later directory
        // that legitimately reuses the id would be refused as a false cycle.
        let fs = FaultFs::new();
        for d in ["/r", "/r/a", "/r/b"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        let id = ObjectId { volume: 1, index: 42 };
        fs.set_identity("/r/a", FileIdentity::Strong(id));
        fs.set_identity("/r/b", FileIdentity::Strong(id));
        // Fail read_dir on its SECOND call, which is /r/a.
        fs.fail_nth("read_dir", 2, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let errors: Vec<String> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.err())
            .map(|e| rel(&e.path))
            .collect();
        // `a` fails on read_dir. `b` must NOT then be refused as a cycle.
        assert_eq!(errors, vec!["a".to_string()], "b must not be a false cycle; got {errors:?}");
    }
```

Re-run Mutant B with this test present: it must FAIL. Then revert the mutant surgically.

- [ ] **Step 5: Gate and commit**

Run: `just check` — expected exit 0, `143 tests run: 143 passed, 1 skipped`.

```bash
git add crates/flux-core/src/walk.rs
git commit -m "feat(core): ancestor-set cycle detection gated on Strong identity

An ancestor set, not a global visited set: the latter grows with the total
number of directories, which spec line 997 forbids, and a cycle by definition
re-enters an ancestor. Only Strong identities are pushed or compared, so
both-sides-Strong holds by construction.

A cycle is IoError plus a message, not SafetyRejected - that code aborts the
whole operation under 129 and a cycle does not. ErrorKind::FilesystemLoop
would be exact but is unstable on the pinned toolchain.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 12: Detect a directory that stopped being one (fork F2c)

**Files:** Modify `crates/flux-core/src/walk.rs`, `crates/flux-platform/tests/std_fs.rs`

- [ ] **Step 1: Write the failing tests**

In `walk.rs`:

```rust
    #[test]
    fn an_entry_listed_as_a_directory_that_is_no_longer_one_is_refused() {
        // §149.4: "the scanner must verify that the object being entered remains
        // consistent with the planned directory identity." read_dir typed this as a
        // Dir; metadata disagrees; therefore the object changed under the scan.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();
        fs.create_dir(Path::new("/r/swap")).unwrap();
        // The listing still says Dir, because `directories` still holds it, while
        // `types` now reports a symlink -- exactly the split the swap produces.
        fs.set_type("/r/swap", FileType::Symlink);

        let errors: Vec<(String, Code)> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.err())
            .map(|e| (rel(&e.path), e.cause.code))
            .collect();
        assert_eq!(errors, vec![("swap".to_string(), Code::DirectoryChangedDuringScan)]);
    }
```

In `crates/flux-platform/tests/std_fs.rs`, the measurement that motivates the whole check:

```rust
#[test]
fn read_dir_follows_a_symlink_to_a_directory_which_is_why_file_type_is_checked() {
    // This is the reachable tree escape. MEASURED on Windows via a junction and on
    // Linux via a symlink, identically: read_dir on the link enumerates the TARGET.
    // So a walk that descended on `!is_file` alone would copy files from outside the
    // tree. `Metadata.file_type` is what lets the walk refuse.
    let d = tempfile::tempdir().unwrap();
    let real = d.path().join("real");
    let link = d.path().join("link");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("marker.txt"), b"x").unwrap();
    if make_dir_reparse_point(&real, &link).is_err() {
        eprintln!("skipped: cannot create a directory reparse point here");
        return;
    }

    let fs = StdFileSystem;
    let names: Vec<String> = fs
        .read_dir(&link)
        .unwrap()
        .into_iter()
        .map(|e| e.name.to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"marker.txt".to_string()), "read_dir FOLLOWS the link");

    // And the refusal that makes it safe: the link reports as a Symlink, not a Dir.
    assert_eq!(fs.metadata(&link).unwrap().file_type, flux_fs::FileType::Symlink);
}
```

`make_dir_reparse_point` already exists at `crates/flux-platform/tests/std_fs.rs:377`.

- [ ] **Step 2: Add the `FaultFs` seam the first test needs**

```rust
    /// Override what `metadata` reports a path as, WITHOUT changing what `read_dir`
    /// lists it as. That split is the whole point: it is what an object swapped
    /// between the listing and the stat looks like from inside the walk.
    pub fn set_type(&self, path: impl AsRef<Path>, file_type: flux_fs::FileType) {
        let path = path.as_ref();
        self.inner.lock().unwrap().types.insert(path.to_path_buf(), file_type);
    }
```

Note `read_dir` types an entry `Dir` when `directories` contains it, checked BEFORE `types` — so
`set_type` does not disturb the listing. Confirm that ordering is what Task 5 wrote; if `types` is
consulted first, this test cannot express the swap and you must fix `read_dir`, not the test.

- [ ] **Step 3: Run and watch them fail**

Run: `cargo test -p flux-core --lib an_entry_listed_as_a_directory 2>&1 | tail -15`
Expected: FAILS — `errors` is empty.

- [ ] **Step 4: Add the check**

In `enter`, immediately after the `metadata` call and BEFORE the cycle check:

```rust
        // §149.4. `read_dir` typed this entry `Dir`; if `metadata` disagrees, the
        // object under the name changed between the listing and the stat.
        //
        // This NARROWS the race, it does not close it: the window moves from
        // read_dir-to-metadata down to metadata-to-read_dir, and something could
        // still be swapped inside it. Closing it needs `openat`-style relative
        // traversal with `O_NOFOLLOW`, which `TODO.md` already records as the fix
        // for walker TOCTOU. Do not let this comment claim more than that.
        if m.file_type != FileType::Dir {
            return Err(WalkError {
                path: rel,
                cause: FsError::new(
                    Code::DirectoryChangedDuringScan,
                    std::io::Error::other("entry listed as a directory is no longer one"),
                ),
            });
        }
```

- [ ] **Step 5: Run and prove non-vacuousness**

Run: `cargo test -p flux-core --lib && cargo test -p flux-platform --test std_fs`
Expected: PASS. `flux-core` 56, `flux-platform::std_fs` 18 on Windows.

Temporarily change the condition to `if false`.
Expected: `an_entry_listed_as_a_directory_that_is_no_longer_one_is_refused` FAILS.
**Revert surgically.**

- [ ] **Step 6: Gate and commit**

Run: `just check` — expected exit 0, `145 tests run: 145 passed, 1 skipped`.

```bash
git add crates/flux-core/src/walk.rs crates/flux-core/src/fault_fs.rs \
        crates/flux-platform/tests/std_fs.rs
git commit -m "feat(core): refuse an entry that stopped being a directory

149.4 asks the scanner to verify the object being entered is still the
directory it planned to enter. read_dir typed it Dir; if metadata disagrees,
it changed under the scan, and that is DIRECTORY_CHANGED_DURING_SCAN.

A real-filesystem test records why this matters: read_dir FOLLOWS a symlink
to a directory on both platforms, so a walk gated on the old is_file boolean
would have enumerated a swapped link's target. The check narrows that window
rather than closing it; openat traversal is what closes it.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 13: One Windows open in `metadata`, not two

Recorded as a correctness fix during PR 1's capstone and deferred to PR 2 on the condition that it lands
before a walk consumes identity. That condition is now due.

Today `metadata` calls `std::fs::symlink_metadata(path)` (which opens a handle internally) and then
`identity_of(path)` (which opens a second). The two facts are not atomic, so `len`/`file_type` can
describe one object while `identity` describes another.

**Files:** Modify `crates/flux-platform/src/std_fs.rs`

- [ ] **Step 1: Name the oracle before touching anything**

These six tests pin the behaviour this task must NOT change. Run them and record that they pass:

Run: `cargo test -p flux-platform --test std_fs 2>&1 | rg "metadata_reports|identity|reached_two_ways"`

- `metadata_reports_a_symlink_as_not_a_file` (:126)
- `metadata_reports_a_directory_as_not_a_file` (:140)
- `metadata_reports_a_reparse_point_as_not_a_file` (:413)
- `a_symlink_reports_its_own_identity_not_its_targets` (:328)
- `a_directory_has_an_identity` (:298)
- `one_directory_reached_two_ways_is_one_object` (:311)

**If this task would change what ANY of them observes, STOP and report.** The tests win. This is the
highest-risk task in the plan: `File::metadata()` on a handle opened with `FILE_FLAG_OPEN_REPARSE_POINT`
is not obviously identical to `symlink_metadata` on the path, and the difference would land squarely on
the `SPECIAL_FILE_UNSUPPORTED` refusal.

- [ ] **Step 2: Restructure the Windows arm**

**Read this before writing anything.** `metadata` is TODAY a SINGLE function whose body carries inner
`#[cfg]` attributes on two `let identity = ...` lines (`std_fs.rs:173-176`). This task SPLITS it into
two whole cfg-gated methods. Both must exist, or the platform you did not write loses the trait method
and fails to compile with `not all trait items implemented`. Write the Unix arm FIRST so the tree is
never one-armed:

```rust
    #[cfg(unix)]
    fn metadata(&self, path: &Path) -> Result<Metadata> {
        // `symlink_metadata`, NOT `metadata`: the latter dereferences a symlink and
        // would report the TARGET as a regular file, so a symlink would sail past
        // the SPECIAL_FILE_UNSUPPORTED refusal and be copied as its target.
        let m = std::fs::symlink_metadata(path).map_err(FsError::from_io)?;
        Ok(Metadata {
            len: m.len(),
            file_type: type_of(&m),
            permissions: Some(perms_of(&m)),
            modified: m.modified().ok(),
            // Free on Unix: `st_dev` and `st_ino` are already in the stat buffer, so
            // there is no second open to eliminate here and nothing to change.
            identity: identity_of(&m),
        })
    }
```

Then replace the `#[cfg(windows)] fn identity_of(path: &Path)` with a function that takes an
already-open handle, and have the Windows `metadata` open ONCE:

```rust
    #[cfg(windows)]
    fn metadata(&self, path: &Path) -> Result<Metadata> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        // ONE open, so len, type and identity all describe the SAME object. Two
        // opens left a window in which the path could be replaced between them.
        //
        // The flags are not a free choice; std's own source settles all three.
        // BACKUP_SEMANTICS "allows opening directories" - without it a directory
        // cannot be opened at all. OPEN_REPARSE_POINT "opens a link instead of its
        // target" - without it this would read a symlink's TARGET identity,
        // silently corrupting cycle detection. access_mode(0): "No read or write
        // permissions are necessary", which avoids both a sharing violation and a
        // denial on a file this process may not read.
        let f = OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(FsError::from_io)?;
        let m = f.metadata().map_err(FsError::from_io)?;
        Ok(Metadata {
            len: m.len(),
            file_type: type_of(&m),
            permissions: Some(perms_of(&m)),
            modified: m.modified().ok(),
            identity: identity_of_handle(&f),
        })
    }
```

The Unix arm is the block above, unchanged in substance. `identity_of_handle` is the existing Windows
`identity_of` (`std_fs.rs:291-330`) with the `OpenOptions` block deleted and the handle passed in. Do
not infer its signature — it is exactly this, and the two `use` lines the old body carried must move
with it:

```rust
/// The identity half of `metadata`, reading an ALREADY-OPEN handle.
///
/// Split out from `identity_of(path)` so `metadata` opens once. The flags that
/// handle must carry are documented at its one call site above; this function
/// cannot enforce them, which is why it is private and takes a `&File` rather than
/// a path.
#[cfg(windows)]
fn identity_of_handle(file: &File) -> FileIdentity {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let mut info = FILE_ID_INFO { VolumeSerialNumber: 0, FileId: Default::default() };
    // SAFETY: `info` is a live, correctly sized `FILE_ID_INFO`, and the handle stays
    // open for the duration of the call because `file` outlives it.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as _,
            FileIdInfo,
            (&raw mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok == 0 {
        // Not every filesystem implements this info class.
        return FileIdentity::Unavailable;
    }

    let index = u128::from_le_bytes(info.FileId.Identifier);
    // §107: an id of zero is never a valid universal identity.
    if index == 0 {
        return FileIdentity::Unavailable;
    }
    FileIdentity::Strong(ObjectId { volume: info.VolumeSerialNumber, index })
}
```

Three things move rather than being rewritten, and each matters: the `access_mode(0)` open moves UP into
`metadata` (that is the whole point of the task); `OpenOptionsExt` moves with it, because `metadata` is
now the caller of `.access_mode`/`.custom_flags`; and the `Ok(file) = ... else { return Unavailable }`
arm becomes `metadata`'s `?`, so a path that cannot be opened is now an `Err` rather than a successful
`Metadata` with `identity: Unavailable`.

**That last one is a behaviour change, and it is the shape divergence to watch.** Today an unopenable
path still yields `Metadata` with `Unavailable`; afterwards it yields `Err`. Verify against the oracle in
Step 1 — if any of those six tests changes what it observes, STOP and report rather than adjusting them.

- [ ] **Step 3: Run the oracle**

Run: `cargo test -p flux-platform --test std_fs`
Expected: all 18 pass, INCLUDING the six named in Step 1.

**If `metadata_reports_a_symlink_as_not_a_file` or `metadata_reports_a_reparse_point_as_not_a_file`
fails, that is the shape divergence this task was warned about.** Do NOT adjust the test. Report
`SHAPE_DIVERGENCE: File::metadata on an OPEN_REPARSE_POINT handle reports <x> where symlink_metadata
reported <y>` and stop. The likely cause is that std populates the reparse tag on a path-based stat but
not a handle-based one, which would make `type_of` return `Other` instead of `Symlink`.

If that happens, the fallback is to keep BOTH calls but derive the identity from the handle and the type
from `symlink_metadata` — a smaller win, still one fewer race than today — and record the reason.

- [ ] **Step 4: Gate and commit**

Run: `just check` — expected exit 0, `145 tests run: 145 passed, 1 skipped`. No count change.

**`just check` only proves the WINDOWS arm here, and this is the one task that splits a method by
platform.** A mangled Unix arm would commit clean and stay hidden until Task 14. Cross-check before
committing, not after:

Run: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-walk2 && cargo nextest run --workspace --no-tests=pass 2>&1 | tail -5'`

Expected: green, with `flux-platform::std_fs` two higher than Windows. If the Unix arm does not compile,
fix it HERE — do not carry a one-armed tree into Task 14.

```bash
git add crates/flux-platform/src/std_fs.rs
git commit -m "fix(platform): derive Windows metadata and identity from one handle

symlink_metadata opened the path and identity_of opened it again, so len,
type and identity could describe two different objects if the path was
replaced between the calls. One open closes that.

Recorded during PR 1's capstone as a correctness fix due before a walk
consumes identity, which it now does.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 14: Ordering over non-UTF-8 names, on a real filesystem

A fake that agrees with the implementation about encoding proves nothing about the OS. This is the test
the `PathBuf` widening in Task 4 exists to make possible.

**Files:** Modify `crates/flux-core/src/walk.rs`, `crates/flux-platform/tests/std_fs.rs`

- [ ] **Step 1: The fake-level test**

In `walk.rs`:

```rust
    #[test]
    fn non_utf8_names_sort_by_their_encoded_bytes() {
        // The whole reason FaultFs was rekeyed from String to PathBuf: under
        // `display()` these two names collapsed to one key.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();

        // 0xFF is not valid UTF-8 in either direction.
        let hi = os_from_bytes(&[b'z', 0xFF]);
        let lo = os_from_bytes(&[b'a', 0xFF]);
        fs.write_file(Path::new("/r").join(&lo), b"");
        fs.write_file(Path::new("/r").join(&hi), b"");

        let got: Vec<std::ffi::OsString> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.ok())
            .filter_map(|e| match e {
                WalkEvent::File { path } => path.file_name().map(|n| n.to_os_string()),
                _ => None,
            })
            .collect();
        assert_eq!(got, vec![lo, hi], "ascending by encoded bytes");
    }
```

with this helper in the test module:

```rust
    /// Build an `OsString` from raw bytes. On Unix this is free; on Windows
    /// `OsString` is WTF-8 and the unsafe constructor is the only way in, which is
    /// sound here because the bytes came from an `OsStr` view in the first place.
    #[cfg(unix)]
    fn os_from_bytes(b: &[u8]) -> std::ffi::OsString {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(b.to_vec())
    }

    #[cfg(windows)]
    fn os_from_bytes(b: &[u8]) -> std::ffi::OsString {
        // A lone 0xFF is not valid WTF-8, so build the name from a valid but
        // non-ASCII sequence instead -- the ORDERING property is what is under test,
        // not the invalidity itself.
        std::ffi::OsString::from(String::from_utf8_lossy(b).into_owned())
    }
```

**If the Windows arm makes the test assert something weaker than the Unix arm, say so in a comment
rather than pretending the coverage is equal.** Windows names are WTF-16 at the OS level and cannot hold
an arbitrary byte; the honest claim is that the Unix arm tests non-UTF-8 ordering and the Windows arm
tests non-ASCII ordering.

- [ ] **Step 2: The real-filesystem test, Unix only**

In `crates/flux-platform/tests/std_fs.rs`:

```rust
#[cfg(unix)]
#[test]
fn read_dir_reports_non_utf8_names_intact() {
    // A fake that agrees with the implementation about encoding proves nothing
    // about the OS. §241: the on-disk name is bytes, and it must survive read_dir
    // without a lossy conversion.
    use std::os::unix::ffi::OsStrExt;
    let d = tempfile::tempdir().unwrap();
    let raw = std::ffi::OsStr::from_bytes(&[b'x', 0xFF, b'y']);
    std::fs::write(d.path().join(raw), b"").unwrap();

    let got = StdFileSystem.read_dir(d.path()).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name.as_bytes(), &[b'x', 0xFF, b'y'], "name survived intact");
}
```

- [ ] **Step 3: Run**

Run: `just check`
Expected: exit 0. Windows `146 tests run: 146 passed, 1 skipped`; Linux gains the extra `#[cfg(unix)]`
test on top of its own baseline.

- [ ] **Step 4: Cross-check on Linux**

Windows-only verification has missed a real divergence in this repository before — PR 1 shipped a bug
that only the WSL cross-check caught, and only because the counts differed.

Run: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-walk2 && cargo nextest run --workspace --no-tests=pass 2>&1 | tail -5'`

Expected: green, with `flux-platform::std_fs` two higher than Windows plus the new Unix-only test.
Record BOTH counts in the commit message.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/walk.rs crates/flux-platform/tests/std_fs.rs
git commit -m "test: pin ordering over non-UTF-8 names on the fake and on a real FS

The PathBuf rekeying in the fake exists for this case: under display() these
names collapsed to one key. A real-filesystem test on Unix proves the name
survives read_dir intact, because a fake that shares the implementation's
idea of encoding proves nothing about the OS.

The Windows arm tests non-ASCII ordering rather than non-UTF-8: an OS name
there is WTF-16 and cannot hold an arbitrary byte. Stated, not papered over.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## After the last task

- [ ] **Run the full gate on both platforms** and record the counts.
- [ ] **AGY-CAPSTONE.** Review the COMMITTED implementation over `ec58829..HEAD`, rounds until green.
      The defect class to hunt by name, because all four instances in PR 1 were real: **naive Windows
      system boundaries** — POSIX mental models applied to Windows. Task 13 is where it will be.
- [ ] **AGY-TEST-AUDIT** once the capstone gate passes.
- [ ] Push and open the PR — **both are outward actions and need explicit user approval.**

## Deliberately NOT in this PR

`copy_tree`, the destination anchor, the lexical containment floor, the §129 pre-flight, the aggregated
weak-identity warning, `--safety=strict`, and the CLI. All PR 3.

Directory metadata preservation stays out of all three cuts: `set_times` takes a `&Self::Writer`, and
widening that surface is a separate decision from the walk. `DirEnd` is emitted now precisely so the
walker does not change when it arrives.

## Self-review

**Spec coverage.** PR 2's Delivery bullet names `FileType` (T2), `DirEntry` (T2), `read_dir` (T5),
`create_dir` (T5), `Code::DirectoryChangedDuringScan` (T6), the `FaultFs` widening (T4), the ordered walk
(T8), its event type (T7), non-terminal errors (T9), the depth cap (T10) and ancestor-set cycle detection
gated on both-sides-`Strong` (T11). All eleven have a task. Tasks 1, 3, 12, 13 and 14 are additions: a
defect found in merged code, the F4b widening the forks produced, its producer, recorded debt now due,
and the §241 test the widening exists for.

**Placeholder scan.** No step says "add error handling", "similar to Task N", or "write tests for the
above". Every code step carries the code.

**Type consistency.** `FileType` is spelled the same in all four files. `WalkError.cause` is `FsError`
everywhere (fork E). `walk_with_depth` takes `(fs, root, max_depth)` in Task 7 and is called that way in
Task 10. `set_type` and `add_symlink` are defined in Tasks 12 and 4 before their uses in Tasks 12 and 5
— note Task 5 uses `add_symlink`, so Task 4 must define it, and it does.

**One known softness, stated rather than hidden.** Task 9's test passes against Task 8's code, so it is a
characterisation test and its value rests entirely on the mutant in Step 3. That is called out in the
task rather than dressed up as red-green.

## Stand-downs

Findings raised during the adversarial panel review of this plan that were NOT folded, each with the
measurement or guard that settled it. Recorded here because this discipline reviews a pre-implementation
artifact and may produce no commit of its own, so the artifact is the only durable record.

- `REJECTED` — "Tasks 4 and 5 change `Inner`'s fields but never update the `FaultFs::new()` initializer,
  so `cargo check` fails." MEASURED: there is no field-enumerating initializer to update.
  `fault_fs.rs:14` is `#[derive(Default)]` on `struct Inner`, `fault_fs.rs:125` is `#[derive(Default)]`
  on `struct FaultFs`, and `fault_fs.rs:186` is `pub fn new() -> Self { Self::default() }`. The only
  `Inner {` in the file is the struct definition itself. Adding or removing a field needs no initializer
  change. The peer's answer to "which task fails first" rested on this same false premise.
- `REJECTED` — "mixed `/` and `\` separators will break the fake's `PathBuf` map lookups on Windows."
  MEASURED: `Path::new("/r").join("a")` is textually `/r\a`, yet `==` against `Path::new("/r/a")` is
  `true`, a `HashMap<PathBuf, _>` keyed by one is found by the other, and `.parent()` matches
  `Some(Path::new("/r"))`. `Path` compares, hashes and decomposes separator-insensitively on Windows.
  Only the ASSERTION spelling was affected, which is what the `rel()` helper in Task 8 fixes.
- `DISCARDED-BELOW-FLOOR` — `FaultFs::read_dir` scans every key in `files` and `directories` on each
  call, so a walk over the fake is O(n²) in the fake's total entry count. Unreachable as a real cost: the
  guard is that `FaultFs` is a test double whose trees are a handful of entries, and the trait's real
  implementation (`std_fs.rs`, `StdFileSystem::read_dir`) does one `std::fs::read_dir` per directory.

None of these is an `UNVERIFIED-ACCEPTED`: every one was settled by measurement rather than by judgement.

Added after panel round 2:

- `DISCARDED-BELOW-FLOOR` — "`os_from_bytes`'s `#[cfg(unix)]` / `#[cfg(windows)]` pair will fail to
  compile on macOS." The guard is that `cfg(unix)` natively encompasses macOS, so the two arms are
  exhaustive for every supported target. The peer raised and correctly discarded this itself.

Added after panel round 4:

- `REJECTED` — "Task 5's `if let Some(parent) = p.parent() && ...` needs the unstable `let_chains`
  feature and fails with E0658 on stable." MEASURED: merged, CI-green code already uses exactly this
  form at `crates/flux-core/src/fault_fs.rs:305-307` (`if let Some(&(nth, code, kind)) = ... && n ==
  nth`) and `:394-397` (`if reads == 2 && let Some(extra) = ...`). Let-chains are stable in edition 2024
  from Rust 1.88; this toolchain is `rustc 1.98.0`.
- `REJECTED` — "Task 11's `if let FileIdentity::Strong(id) = m.identity` moves `m.identity`, so its
  later use in `WalkEvent::Dir` is a use-after-move." MEASURED: `crates/flux-fs/src/fs.rs:42` is
  `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`, so `FileIdentity` is `Copy` and the binding copies.
  The peer flagged this one as "I do not know" rather than asserting it, which is the right behaviour.

Added after panel round 5:

- `REJECTED` — "Task 4 adds `pub fn add_symlink` with no caller until Task 5, so `dead_code` fails the
  gate at the end of Task 4." MEASURED against merged, CI-green code: `FaultFs::modified`
  (`fault_fs.rs:260`) is a `pub fn` with ZERO call sites anywhere in the workspace, and the baseline
  `just check` is green at 119 passed. An uncalled `pub` method on `FaultFs` does not trip `dead_code`;
  a never-READ private FIELD does, which is the different rule that moved Tasks 7/8 and the depth cap.
- `REJECTED` — "Task 13's Unix arm passes `&m` to an `identity_of` that takes a `&Path`, a type
  mismatch." MEASURED: the Unix arm ALREADY calls `identity_of(&m)` at `std_fs.rs:174` against
  `#[cfg(unix)] fn identity_of(m: &std::fs::Metadata)` at `:268`. The `identity_of(path)` form is the
  WINDOWS one at `:176`. The plan's Unix block reproduces what is there.
- `SURFACED TO THE OWNER, not resolved here` — "`WalkEvent::DirEnd` is speculative generality and
  should not be built: the plan itself says PR 3 does not consume it." This challenges a decision the
  owner already settled in the approved design document, whose counter-argument is that a pre-order-only
  stream cannot express "leaving this directory", so a consumer would have to re-derive it by comparing
  path prefixes — fragile exactly where §7.2's component-wise ordering is subtle, and §30.2 requires the
  post-order point to exist. Recorded rather than folded or dismissed, because only the owner may
  re-open a settled fork.
