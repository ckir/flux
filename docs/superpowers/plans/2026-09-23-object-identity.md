# Object Identity Implementation Plan (walker PR 1 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `flux_fs::Metadata` a portable filesystem object identity, with its reliability
classification, implemented on both platforms and in the test fake.

**Architecture:** A new `ObjectId { volume, index }` pairs a filesystem id with an object id, because
spec §109 forbids comparing either alone. A new three-state `FileIdentity` carries the reliability
classification §107 requires. `Metadata` gains one field. On Unix the values come free from the
`symlink_metadata` call `metadata()` already makes; on Windows they need a handle and
`GetFileInformationByHandleEx(FileIdInfo)`, because the 64-bit index std exposes is not unique on ReFS.

**Tech Stack:** Rust 2024, MSRV 1.85. `std::os::unix::fs::MetadataExt` on Unix — no new dependency.
`windows-sys 0.61` on Windows, already pinned in `[workspace.dependencies]` with the required
`Win32_Storage_FileSystem` feature, currently a dev-dependency of `flux-platform` and promoted to a
real one here.

**Spec:** `docs/superpowers/specs/2026-09-23-directory-walker-design.md` at `e299b5b`, sections
"The trait surface after this work" and "Object identity".

---

## Why this is PR 1

It carries the only genuine platform risk in the walker design, and both §149.4's mandatory
directory-identity check and §129's containment safety depend on it. Landing it alone means the walker
is never blocked on Windows identity debugging.

**No walker code is in this PR.** No `read_dir`, no `create_dir`, no walk, no `copy_tree`.

## File Structure

| File | Change |
|---|---|
| `crates/flux-fs/src/fs.rs` | Add `ObjectId`, `FileIdentity`; add `Metadata.identity`; update the `NullFs` test stub |
| `crates/flux-fs/src/lib.rs` | Export the two new types |
| `crates/flux-platform/src/std_fs.rs` | Populate `identity` — a `cfg(unix)` and a `cfg(windows)` helper |
| `crates/flux-platform/Cargo.toml` | Promote `windows-sys` from dev-dependency to dependency |
| `crates/flux-core/src/fault_fs.rs` | Store identities, add `set_identity`, populate `identity` |
| `crates/flux-platform/tests/std_fs.rs` | Behavioural tests against the real filesystem |

## Baseline, measured before starting

`cargo test --workspace` is green at `e299b5b` on Windows: `flux-core` lib **30**, `flux-fs` lib **9**,
`flux-platform` `tests/std_fs.rs` **7** and `tests/fs_semantics.rs` **9**, root `tests/copy.rs` **2**,
`tests/dev_tooling.rs` **2**, `tests/integration` **1**, `tests/model_stamp.rs` **45** (1 ignored).

Every expected count below is that baseline plus the tests the task adds.

## Repo gate

`just check` runs `fmt-check`, `clippy`, `typos`, `test`. Use it as-is; do not invent stricter flags.
Commit messages end with the `Co-Authored-By` line the repository already uses.

---

### Task 1: The identity types

**Files:**
- Modify: `crates/flux-fs/src/fs.rs` (insert after the `Perms` enum, which ends at line 15)
- Modify: `crates/flux-fs/src/lib.rs:16`

- [ ] **Step 1: Write the failing test**

Append inside the existing `mod tests` in `crates/flux-fs/src/fs.rs`:

```rust
    #[test]
    fn identity_distinguishes_strength_and_object() {
        let a = ObjectId { volume: 1, index: 2 };
        let b = ObjectId { volume: 1, index: 3 };

        // Same object, different reliability, is NOT the same identity. This is the
        // whole reason §107 makes strength part of the value rather than a sidecar.
        assert_ne!(FileIdentity::Strong(a), FileIdentity::Weak(a));
        // Different objects never compare equal.
        assert_ne!(FileIdentity::Strong(a), FileIdentity::Strong(b));
        assert_eq!(FileIdentity::Strong(a), FileIdentity::Strong(a));
        assert_eq!(FileIdentity::Unavailable, FileIdentity::Unavailable);

        // §109: both halves participate. An equal index on another volume is a
        // different object, and nothing may compare on the index alone.
        assert_ne!(ObjectId { volume: 1, index: 2 }, ObjectId { volume: 9, index: 2 });
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p flux-fs identity_distinguishes`
Expected: FAIL — `cannot find type 'ObjectId' in this scope`.

- [ ] **Step 3: Add the types**

Insert into `crates/flux-fs/src/fs.rs` immediately after the `Perms` enum (after line 15):

```rust
/// Portable filesystem object identity (§109).
///
/// BOTH fields, always. §109 says "Never compare only inode" and "only file ID",
/// and requires `filesystem_id + object_id`, so that two unrelated objects on
/// different filesystems are never merged into one identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectId {
    /// Unix `st_dev`; Windows `VolumeSerialNumber`.
    pub volume: u64,
    /// Unix `st_ino`, zero-extended; Windows the 128-bit `FileId`.
    ///
    /// `u128` because Windows needs it: the 64-bit file index std exposes is not
    /// unique on ReFS, and this repository's own working tree is a ReFS Dev Drive.
    pub index: u128,
}

/// An `ObjectId` together with how far it can be trusted (§107).
///
/// §107: "A `FileIdentity` must therefore be accompanied by its reliability
/// classification." Three states, and deliberately NOT `Option<ObjectId>`: an
/// `Option` collapses *weak* into *strong*, which is precisely the failure §107
/// exists to prevent. FAT32, exFAT and some SMB and NFS configurations produce an
/// identity that exists and cannot be trusted.
///
/// Callers that act on identity for SAFETY must act on `Strong` alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileIdentity {
    Strong(ObjectId),
    Weak(ObjectId),
    Unavailable,
}
```

- [ ] **Step 4: Export them**

In `crates/flux-fs/src/lib.rs`, replace line 16:

```rust
pub use fs::{FileHandle, FileIdentity, FileSystem, Metadata, ObjectId, Perms};
```

- [ ] **Step 5: Run the test**

Run: `cargo test -p flux-fs`
Expected: PASS, `test result: ok. 10 passed` (baseline 9 + 1).

- [ ] **Step 6: Commit**

```bash
git add crates/flux-fs/src/fs.rs crates/flux-fs/src/lib.rs
git commit -m "feat(flux-fs): portable object identity with its reliability class"
```

---

### Task 2: Widen `Metadata`

This is the breaking change. It is its own task so that the compile break and its three fixes land
together and nothing else is in the diff.

**Files:**
- Modify: `crates/flux-fs/src/fs.rs:19-25` (the struct) and `:119` (the `NullFs` stub)
- Modify: `crates/flux-platform/src/std_fs.rs:173-178`
- Modify: `crates/flux-core/src/fault_fs.rs:329-334`

- [ ] **Step 1: Add the field**

In `crates/flux-fs/src/fs.rs`, the `Metadata` struct becomes:

```rust
/// Only what single-file copy and the walk read. Widened when a consumer needs more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub len: u64,
    /// False for directories, symlinks, devices, FIFOs and sockets.
    pub is_file: bool,
    pub permissions: Option<Perms>,
    pub modified: Option<SystemTime>,
    /// §149.4 asks the scanner to obtain identity "where strongly supported", and
    /// §107 requires the strength to travel with it. One field, three states, and no
    /// second way to say "absent".
    pub identity: FileIdentity,
}
```

- [ ] **Step 2: Run the build and read the three errors**

Run: `cargo build --workspace --all-targets`
Expected: FAIL with `missing field 'identity' in initializer of 'Metadata'` at exactly three sites —
`flux-fs/src/fs.rs:119`, `flux-platform/src/std_fs.rs:173`, `flux-core/src/fault_fs.rs:329`.

If any OTHER site appears, STOP and report `STATE_MISMATCH: <the extra site>` — the plan's claim that
there are exactly three construction sites was verified with
`rg 'Metadata \{' crates/` and a fourth means the tree moved.

- [ ] **Step 3: Fix all three with the honest placeholder**

In `crates/flux-fs/src/fs.rs:119`, the `NullFs` stub. Note that in the file this initializer is a
SINGLE line — `Ok(Metadata { len: 0, is_file: true, permissions: None, modified: None })` — so this is
a rewrite of that one line into the form below, not a patch of a multi-line block:

```rust
        fn metadata(&self, _: &Path) -> crate::Result<Metadata> {
            Ok(Metadata {
                len: 0,
                is_file: true,
                permissions: None,
                modified: None,
                // The stub exists to prove the trait compiles. Inventing an identity
                // here would let a test pass against a fake that never had one.
                identity: FileIdentity::Unavailable,
            })
        }
```

In `crates/flux-platform/src/std_fs.rs:173`, add `identity: FileIdentity::Unavailable,` as the last
field. In `crates/flux-core/src/fault_fs.rs:329`, add the same. Both are replaced with real values in
Tasks 3 to 5; `Unavailable` is correct in the meantime because it is exactly what "this implementation
does not supply identity" means.

- [ ] **Step 4: Verify green**

Run: `cargo test --workspace`
Expected: PASS, every count at its baseline (`flux-fs` 10 after Task 1, `flux-core` 30,
`flux-platform` 7 and 9).

- [ ] **Step 5: Commit**

```bash
git add crates/flux-fs/src/fs.rs crates/flux-platform/src/std_fs.rs crates/flux-core/src/fault_fs.rs
git commit -m "feat(flux-fs): Metadata carries a file identity"
```

---

### Task 3: Unix identity

**Files:**
- Modify: `crates/flux-platform/src/std_fs.rs`
- Test: `crates/flux-platform/tests/std_fs.rs`

- [ ] **Step 1: Add the import, then write the failing test**

`crates/flux-platform/tests/std_fs.rs` currently imports only `flux_fs::FileSystem`,
`flux_platform::StdFileSystem`, `std::io::Write` and `tempfile::TempDir` (lines 1-4). Without this the
tests below fail to COMPILE rather than failing their assertion, which is a different and much less
useful signal. Change line 1 to:

```rust
use flux_fs::{FileIdentity, FileSystem};
```

Then append to the same file:

```rust
#[cfg(unix)]
#[test]
fn two_hardlinks_to_one_file_share_an_identity() {
    // The property that makes identity worth having: the SAME object reached by two
    // different paths reports the SAME id, which no amount of path comparison can tell.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::write(&a, b"x").unwrap();
    std::fs::hard_link(&a, &b).unwrap();

    let fs = StdFileSystem;
    let ia = fs.metadata(&a).unwrap().identity;
    let ib = fs.metadata(&b).unwrap().identity;

    assert!(matches!(ia, FileIdentity::Strong(_)), "local fs must be strong, got {ia:?}");
    assert_eq!(ia, ib, "two links to one inode are one object");
}

#[cfg(unix)]
#[test]
fn two_distinct_files_have_distinct_identities() {
    // The control for the test above. Without it, an implementation returning one
    // constant id for everything would pass.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::write(&a, b"x").unwrap();
    std::fs::write(&b, b"x").unwrap();

    let fs = StdFileSystem;
    assert_ne!(fs.metadata(&a).unwrap().identity, fs.metadata(&b).unwrap().identity);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run (on Unix): `cargo test -p flux-platform --test std_fs two_hardlinks`
Expected: FAIL — the assertion `local fs must be strong` fails with `Unavailable`.

- [ ] **Step 3: Implement**

Add to `crates/flux-platform/src/std_fs.rs`, near the other `cfg`-gated helpers:

```rust
/// Unix identity costs NOTHING extra: `metadata` already calls `symlink_metadata`,
/// and `st_dev` / `st_ino` come off that same struct. No second syscall.
#[cfg(unix)]
fn identity_of(m: &std::fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    let index = u128::from(m.ino());
    // §107: "The platform adapter must never treat `object_id == 0` as a valid
    // universal object identity."
    if index == 0 {
        return FileIdentity::Unavailable;
    }
    FileIdentity::Strong(ObjectId { volume: m.dev(), index })
}
```

- [ ] **Step 4: Call it**

In `metadata`, replace the placeholder so the initializer reads `identity: identity_of(&m),`.

- [ ] **Step 5: Verify**

Run (on Unix): `cargo test -p flux-platform`
Expected: PASS, `tests/std_fs.rs` at 9 (baseline 7 + 2).

- [ ] **Step 6: Commit**

```bash
git add crates/flux-platform/src/std_fs.rs crates/flux-platform/tests/std_fs.rs
git commit -m "feat(flux-platform): Unix object identity from the metadata already read"
```

---

### Task 4: Windows identity

**Files:**
- Modify: `crates/flux-platform/Cargo.toml`
- Modify: `crates/flux-platform/src/std_fs.rs`
- Test: `crates/flux-platform/tests/std_fs.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/flux-platform/tests/std_fs.rs` — the same two properties as Task 3, gated for
Windows, plus the one that matters on ReFS:

```rust
#[cfg(windows)]
#[test]
fn two_hardlinks_to_one_file_share_an_identity() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::write(&a, b"x").unwrap();
    std::fs::hard_link(&a, &b).unwrap();

    let fs = StdFileSystem;
    let ia = fs.metadata(&a).unwrap().identity;
    let ib = fs.metadata(&b).unwrap().identity;

    assert!(matches!(ia, FileIdentity::Strong(_)), "NTFS/ReFS must be strong, got {ia:?}");
    assert_eq!(ia, ib, "two links to one file are one object");
}

#[cfg(windows)]
#[test]
fn two_distinct_files_have_distinct_identities() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::write(&a, b"x").unwrap();
    std::fs::write(&b, b"x").unwrap();

    let fs = StdFileSystem;
    assert_ne!(fs.metadata(&a).unwrap().identity, fs.metadata(&b).unwrap().identity);
}

#[cfg(windows)]
#[test]
fn a_directory_has_an_identity() {
    // BACKUP_SEMANTICS is what makes this pass. Without that flag the open fails on
    // a directory and identity would silently degrade to Unavailable for exactly the
    // entries the walk needs it for.
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();

    let id = StdFileSystem.metadata(&sub).unwrap().identity;
    assert!(matches!(id, FileIdentity::Strong(_)), "directories need identity too, got {id:?}");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p flux-platform --test std_fs a_directory_has_an_identity`
Expected: FAIL — `directories need identity too, got Unavailable`.

- [ ] **Step 3: Promote the dependency**

In `crates/flux-platform/Cargo.toml`, add a real dependency section. The existing
`[target.'cfg(windows)'.dev-dependencies]` stays as it is:

```toml
# `FILE_ID_INFO` for object identity in `std_fs.rs`. The 64-bit file index std exposes
# is not unique on ReFS (§107, §109.1), so identity needs the 128-bit id, which is only
# reachable through a handle.
[target.'cfg(windows)'.dependencies]
windows-sys = { workspace = true }
```

- [ ] **Step 4: Implement**

Add to `crates/flux-platform/src/std_fs.rs`:

```rust
/// Windows identity needs a HANDLE, because `GetFileInformationByHandleEx` takes one
/// and the path-based metadata std already read cannot supply the 128-bit id.
///
/// Every flag here is load-bearing, and std's own source says why
/// (`library/std/src/sys/fs/windows.rs:1398-1404, 1501-1504`):
///   - BACKUP_SEMANTICS   "allows opening directories" — without it a directory cannot
///                        be opened at all, and directories are what the walk needs.
///   - OPEN_REPARSE_POINT "opens a link instead of its target" — without it a symlink
///                        would report its TARGET's identity, silently corrupting the
///                        walk's cycle detection with an id belonging to another object.
///   - access_mode(0)     std: "No read or write permissions are necessary" — it avoids
///                        both a sharing violation and a denial on an unreadable file.
#[cfg(windows)]
fn identity_of(path: &Path) -> FileIdentity {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FileIdInfo,
        GetFileInformationByHandleEx,
    };

    let Ok(file) = OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    else {
        return FileIdentity::Unavailable;
    };

    let mut info = FILE_ID_INFO { VolumeSerialNumber: 0, FileId: Default::default() };
    // SAFETY: `info` is a live, correctly sized `FILE_ID_INFO`, and the handle is open
    // for the duration of the call because `file` outlives it.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as _,
            FileIdInfo,
            (&raw mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok == 0 {
        // A volume that cannot supply it — not every filesystem implements this class.
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

- [ ] **Step 5: Call it**

In `metadata`, the Windows arm passes the PATH rather than the metadata struct. Keep the two platforms
behind one call site by giving each helper the argument it needs:

```rust
        let m = std::fs::symlink_metadata(path).map_err(FsError::from_io)?;
        #[cfg(unix)]
        let identity = identity_of(&m);
        #[cfg(windows)]
        let identity = identity_of(path);
        Ok(Metadata {
            len: m.len(),
            is_file: m.is_file(),
            permissions: Some(perms_of(&m)),
            modified: m.modified().ok(),
            identity,
        })
```

- [ ] **Step 6: Verify**

Run: `cargo test -p flux-platform`
Expected: PASS, `tests/std_fs.rs` at 10 (baseline 7 + 3).

- [ ] **Step 7: Commit**

```bash
git add crates/flux-platform/Cargo.toml crates/flux-platform/src/std_fs.rs crates/flux-platform/tests/std_fs.rs
git commit -m "feat(flux-platform): Windows object identity from FILE_ID_INFO"
```

---

### Task 5: The fake

**Files:**
- Modify: `crates/flux-core/src/fault_fs.rs`

- [ ] **Step 1: Create the test module, then write the failing test**

`crates/flux-core/src/fault_fs.rs` has **no `#[cfg(test)]` module at all** — the fake is currently
exercised only through `copy.rs`'s tests. Append one to the end of the file. `Path` is already imported
at line 10, but a test module needs its own `use`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use flux_fs::{FileIdentity, FileSystem, ObjectId};
    use std::path::Path;

    #[test]
    fn identities_default_to_distinct_and_can_be_forced_equal() {
        let fs = FaultFs::new();
        fs.write_file("/a", b"x");
        fs.write_file("/b", b"x");

        // Default: every path is its own object, so an ordinary test needs no setup
        // and two unrelated paths never collide by accident.
        let ia = fs.metadata(Path::new("/a")).unwrap().identity;
        let ib = fs.metadata(Path::new("/b")).unwrap().identity;
        assert!(matches!(ia, flux_fs::FileIdentity::Strong(_)));
        assert_ne!(ia, ib);

        // Forced: this is what makes a cycle testable without a mount or a privilege.
        let shared = flux_fs::ObjectId { volume: 7, index: 7 };
        fs.set_identity("/a", flux_fs::FileIdentity::Strong(shared));
        fs.set_identity("/b", flux_fs::FileIdentity::Strong(shared));
        assert_eq!(
            fs.metadata(Path::new("/a")).unwrap().identity,
            fs.metadata(Path::new("/b")).unwrap().identity
        );

        // And the degraded path is reachable, which is what the walker's fallback needs.
        fs.set_identity("/a", FileIdentity::Unavailable);
        assert_eq!(fs.metadata(Path::new("/a")).unwrap().identity, FileIdentity::Unavailable);
    }
}
```

Inside this module the `flux_fs::` prefixes in the test body are unnecessary — `FileIdentity` and
`ObjectId` are imported above, so write them bare as shown. The trailing `}` closes `mod tests`.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p flux-core identities_default`
Expected: FAIL — `no method named 'set_identity'`.

- [ ] **Step 3: Store them**

Add to `struct Inner` in `crates/flux-core/src/fault_fs.rs` (it ends at line 40, after
`call_counts`):

```rust
    /// path -> the identity `metadata` reports. A path with no entry gets a distinct
    /// `Strong` id derived from its name, so ordinary tests need no setup.
    identities: HashMap<String, flux_fs::FileIdentity>,
```

- [ ] **Step 4: Add the setter**

Add beside the other `pub fn` configurators (`add_special` is at line 197):

```rust
    /// Give a path an identity. Two paths given the SAME `ObjectId` is how a test
    /// reproduces a directory cycle with no mount and no privilege.
    pub fn set_identity(&self, path: &str, identity: flux_fs::FileIdentity) {
        self.inner.lock().unwrap().identities.insert(path.to_string(), identity);
    }
```

- [ ] **Step 5: Report them**

In `metadata`, replace the `identity: FileIdentity::Unavailable,` placeholder from Task 2:

```rust
            identity: g.identities.get(&p).copied().unwrap_or_else(|| {
                // Distinct per path, and deterministic, so a test that does not care
                // about identity still gets sane behaviour and two different paths are
                // never accidentally the same object. Volume 1 is "the fake's volume".
                let mut index: u128 = 0;
                for b in p.as_bytes() {
                    index = index.wrapping_mul(31).wrapping_add(u128::from(*b));
                }
                // Never 0: §107 forbids treating a zero id as valid, and an empty path
                // would otherwise hash to it.
                flux_fs::FileIdentity::Strong(flux_fs::ObjectId { volume: 1, index: index | 1 })
            }),
```

- [ ] **Step 6: Verify**

Run: `cargo test -p flux-core`
Expected: PASS, `test result: ok. 31 passed` (baseline 30 + 1).

- [ ] **Step 7: Commit**

```bash
git add crates/flux-core/src/fault_fs.rs
git commit -m "feat(flux-core): FaultFs carries configurable object identities"
```

---

### Task 6: The property the walker will rely on

The walk's cycle check compares a directory's identity against its ancestors'. That only works if a
directory reached by two different paths reports one identity. Tasks 3 and 4 proved it for FILES via
hard links; directories cannot be hard-linked, so this proves it the only way available.

**Files:**
- Test: `crates/flux-platform/tests/std_fs.rs`

- [ ] **Step 1: Write the test**

```rust
#[test]
fn one_directory_reached_two_ways_is_one_object() {
    // `.` inside a directory is that same directory. If identity did not survive the
    // path spelling, the walker's ancestor check would never fire.
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let same = sub.join(".");

    let fs = StdFileSystem;
    let a = fs.metadata(&sub).unwrap().identity;
    let b = fs.metadata(&same).unwrap().identity;

    assert!(matches!(a, FileIdentity::Strong(_)), "got {a:?}");
    assert_eq!(a, b, "one directory, two spellings, one identity");
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p flux-platform --test std_fs one_directory_reached_two_ways`
Expected: PASS — both platforms already satisfy this; the test pins it so a later change to
`identity_of` cannot break the walker silently.

**What this test does NOT pin.** An earlier draft claimed a failure here would implicate
`OPEN_REPARSE_POINT`. That was wrong: `.` is not a reparse point, so removing that flag would leave
this test passing. It pins one property only — that identity survives the spelling of the path — and
the flag needs its own test, below.

- [ ] **Step 3: Pin `OPEN_REPARSE_POINT` with an actual link**

```rust
#[test]
fn a_symlink_reports_its_own_identity_not_its_targets() {
    // The mutation this catches: dropping FILE_FLAG_OPEN_REPARSE_POINT on Windows, or
    // using `metadata` instead of `symlink_metadata` on Unix. Either makes a link
    // report its TARGET's identity, and the walk would then compare a link against the
    // target's id - taking an ordinary directory for a cycle, or missing a real one.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let link = dir.path().join("link");

    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &link).is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&target, &link).is_ok();

    if !made {
        // Windows needs Developer Mode or SeCreateSymbolicLinkPrivilege. Say so loudly:
        // a silent skip is how a test stops protecting the platform it was written for.
        eprintln!("SKIPPED a_symlink_reports_its_own_identity: could not create a symlink here");
        return;
    }

    let fs = StdFileSystem;
    let target_id = fs.metadata(&target).unwrap().identity;
    let link_id = fs.metadata(&link).unwrap().identity;

    assert!(matches!(target_id, FileIdentity::Strong(_)), "target: {target_id:?}");
    assert!(matches!(link_id, FileIdentity::Strong(_)), "link: {link_id:?}");
    assert_ne!(target_id, link_id, "a symlink is its own object, not its target");
}
```

Run: `cargo test -p flux-platform --test std_fs a_symlink_reports_its_own`
Expected: PASS, or the explicit `SKIPPED` line on a Windows machine without symlink privilege. If it
prints SKIPPED, say so in the task report rather than recording a pass — on Windows this is the ONLY
test covering that flag.

- [ ] **Step 4: Full gate**

Run: `just check`
Expected: fmt, clippy, typos and the whole suite green. Final counts: `flux-fs` 10, `flux-core` 31,
`flux-platform` `tests/std_fs.rs` 12 on Windows (7 + 3 + 2) or 11 on Unix (7 + 2 + 2),
`tests/fs_semantics.rs` 9, everything else at baseline.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-platform/tests/std_fs.rs
git commit -m "test(flux-platform): identity survives the path spelling, and a link is its own object"
```

---

## Recorded for PR 2, not resolved here

**Windows opens a handle twice per `metadata` call where once would do.** Unix identity falls out of
the `symlink_metadata` already being made, at no extra cost. Windows needs a handle, and `copy_file`
calls `metadata` twice per file (`copy.rs:156` and `:216`), so a tree copy of N files gains 2N opens
for a value only the walk uses.

**The fix is to MERGE the two opens, not to split the API.** MEASURED in std's own source: on Windows
`symlink_metadata` is `lstat` → `metadata(path, ReparsePoint::Open)`
(`library/std/src/sys/fs/windows.rs:1484-1485`), which already opens a handle with `access_mode(0)`,
`FILE_FLAG_BACKUP_SEMANTICS` and `FILE_FLAG_OPEN_REPARSE_POINT` (`:1501-1504`) — the very flags
`identity_of` uses. So the adapter opens the same path twice with identical flags, and the real fix is
one open answering both questions via two `GetFileInformationByHandleEx` classes.

That was worth establishing, because the obvious alternative is worse. Moving identity out to its own
trait method would spare `copy_file` the cost, but it would also split one atomic observation into
two: on Unix `is_file` and `dev`/`ino` come from a SINGLE `stat` today, and §149.4 is specifically
about detecting that an object changed underneath you. Two calls reintroduce exactly that window. So
the API stays as it is, and the cost is an implementation detail of the Windows adapter.

Deferred to PR 2 because merging the opens means reimplementing the Windows metadata path over raw
FFI — more `unsafe` for an unmeasured gain, which is the wrong trade in the PR that establishes
correctness. **The measurement that decides it:** time `copy_file` over a few thousand small files on
Windows with `identity_of` returning `Unavailable` early versus returning the real value — top level,
no other tool calls in flight, two runs, quote the range not a point.

## Out of scope

`read_dir`, `create_dir`, `FileType`, `DirEntry`, the walk, the depth cap, `copy_tree`, the §129
containment checks, `--safety=strict`, and `Code::DirectoryChangedDuringScan`. All are PR 2 or PR 3.
