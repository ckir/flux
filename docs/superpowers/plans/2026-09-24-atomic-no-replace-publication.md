# Atomic No-Replace Publication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `rename_no_replace` refuse an occupied destination atomically instead of by check-then-act, and route every Windows filesystem call through an extended-length `\\?\` path.

**Architecture:** `flux-platform` only — no engine, no CLI, no new trait method. `StdFileSystem::rename_no_replace` loses its `symlink_metadata`-then-`rename` body and gains two platform arms: `rustix::fs::renameat_with` with `RenameFlags::NOREPLACE` on Unix, `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` on Windows. A private path-conversion helper prefixes `\\?\` on the Windows arm by canonicalizing the PARENT and appending the final component verbatim, so the final component is never resolved and symlink semantics survive. `flux-fs` gains two `Code` variants; `FaultFs` gains one configuration method.

**Tech Stack:** Rust 2024, MSRV 1.85. `rustix` 1.1 (`features = ["fs"]`, already a `cfg(unix)` dependency of this crate). `windows-sys` 0.61 (already a `cfg(windows)` dependency; this plan adds one feature). `tempfile` for tests.

---

## Before you start: verified ground truth

Every citation below was grep-verified against the tree at commit `f498329` on branch `feat/copy-tree` in worktree `E:\Rust\flux-walk2`. If any does not match what you see, **STOP and report `STATE_MISMATCH: <what differs>`** rather than adapting — a drifted citation means this plan was written against a different tree and the rest of it cannot be trusted.

**The design is `docs/superpowers/specs/2026-09-23-directory-walker-design.md`, Delivery item 3.** Read it before Task 1. It is the oracle for every behavioural question this plan does not answer.

### Three discoveries that shape this plan

**1. The "three primitives" ship as TWO code arms.** The design names `renameat2(RENAME_NOREPLACE)` for Linux and `renamex_np(RENAME_EXCL)` for macOS as separate primitives. `rustix` already abstracts them. Verified in the vendored source at
`E:\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f\rustix-1.1.4\src\backend\libc\fs\types.rs:527-544`:

```rust
#[cfg(apple)]
bitflags! {
    pub struct RenameFlags: ffi::c_uint {
        /// `RENAME_SWAP`
        const EXCHANGE = bitcast!(c::RENAME_SWAP);

        /// `RENAME_EXCL`
        const NOREPLACE = bitcast!(c::RENAME_EXCL);
        ...
    }
}
```

So `RenameFlags::NOREPLACE` *is* `RENAME_EXCL` on Apple and `RENAME_NOREPLACE` on Linux, and `rustix::fs::renameat_with` is gated `#[cfg(any(apple, linux_kernel, target_os = "redox"))]` (`rustix-1.1.4/src/fs/at.rs:292`). **One `cfg(unix)` arm covers both platforms.** Do not write two.

**2. `renameat_with` does not exist on every Unix.** Its cfg excludes FreeBSD, NetBSD, Solaris and others. This repository's CI matrix is `[ubuntu-latest, macos-latest, windows-latest]`, so every supported target is covered — but a bare `#[cfg(unix)]` arm would fail to compile on an unsupported Unix with a confusing error about a missing function. Task 5 adds an explicit `compile_error!` so that failure names its own cause. **Do not add a check-then-act fallback for such a platform** — `FLUX_FULL_UPDATED_SPEC_V16.md:10876` forbids check-then-rename as a substitute by name.

**3. `rename_no_replace` is NOT `#[cfg]`-gated today** — it is one plain method in a non-gated `impl` block, at `crates/flux-platform/src/std_fs.rs:264-276`. Task 5 splits it. The file already mixes cfg styles (whole gated free functions, gated methods inside a non-gated `impl`, gated match arms), so a gated pair of methods matches the established pattern — `fn metadata` is already exactly that, at `:167-182` (unix) and `:184-213` (windows).

### What this cut does NOT do

- **No new trait method.** `supports_no_replace_publish` was proposed and dropped: there is no side-effect-free way to ask a filesystem whether it supports the flag, because reaching the filesystem's rename implementation requires passing path resolution, and a non-existent source returns `ENOENT` first. If you find yourself adding a capability query, **STOP and report `SHAPE_DIVERGENCE`**.
- **No probe.** The spec's `noreplace-probe` writes inside the operation workspace, and the workspace is out of scope for every cut in this design. Deferred by owner ruling; booked as tracked debt.
- **No change to `rename_no_replace`'s contract or its error.** It already promises to fail rather than replace. What changes is that it keeps that promise atomically. On an occupied target it must still return `Code::IoError` carrying `ErrorKind::AlreadyExists`, because `crates/flux-platform/tests/std_fs.rs:97-107` and `:150-169` pin that and the engine cut maps it, not this one.

### The gate

`just check` = `fmt-check` → `clippy` → `typos` → `test`, verified at `justfile:46`. Expanded:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
typos
cargo nextest run --workspace --no-tests=pass
cargo test --doc --workspace
```

**`just check` runs on Windows only in this worktree.** Any task touching `#[cfg]` code MUST also be cross-checked under WSL before its commit:

```
wsl -e bash -lc 'cd /mnt/e/Rust/flux-walk2 && cargo nextest run --workspace --no-tests=pass'
```

This is not optional and it is not theatre — a previous cut shipped a `#[cfg(windows)]`-only helper called from an ungated test, and both non-Windows CI legs failed to compile while the local gate stayed green.

**Never run `just model`.** TLC and the heavy Java model checks are CI-only.

### Baseline test counts

Counted statically by `#[test]` attribute, NOT by running the suite. Verify by running before you start; if your numbers differ, the tree has moved and you should report `STATE_MISMATCH`.

| Crate | `#[test]` count |
|---|---|
| `flux-fs` | 12 |
| `flux-core` | 69 |
| `flux-platform` | 34 (23 in `tests/std_fs.rs`, 11 in `tests/fs_semantics.rs`) |

Some tests are `#[cfg]`-gated, so the number `cargo nextest` reports on Windows will be lower than the static count. Record what you actually observe as your baseline and use the delta.

---

## File structure

| File | Responsibility | Change |
|---|---|---|
| `crates/flux-fs/src/error.rs` | The spec error-code vocabulary | Modify: 2 new `Code` variants, 2 new `as_str` arms, 2 new assertions |
| `crates/flux-core/src/fault_fs.rs` | The in-memory fake | Modify: 1 field, 1 configuration method, 1 branch in `rename_no_replace` |
| `crates/flux-platform/src/std_fs.rs` | The real adapter | Modify: split `rename_no_replace` into two cfg arms; add the Windows path helper; route Windows calls through it |
| `crates/flux-platform/tests/std_fs.rs` | Adapter behaviour against a real filesystem | Modify: new tests for atomicity, the path helper, and long paths |

No files are created, no manifest changes (verified: `MoveFileExW` needs no feature this workspace does not already enable), and no file is split — `std_fs.rs` is 388 lines and stays coherent.

---

### Task 1: The two new `Code` variants

**Files:**
- Modify: `crates/flux-fs/src/error.rs:9-25` (the enum), `:27-42` (`as_str`), `:90-102` (the taxonomy test)

The two codes belong to the engine and CLI cuts, but the vocabulary is a `flux-fs` concern and adding it here keeps one feature's taxonomy in one cut. Nothing in this cut returns them.

The spec's registry is normative about the strings. Verified at `FLUX_FULL_UPDATED_SPEC_V16.md:2927` and `:2938`:

```
| `DESTINATION_NAMESPACE_COLLISION` | Two distinct source paths, or two source roots, map to the same destination object or prefix. | 16.1, 18.3, 241.5 |
| `NOREPLACE_PUBLISH_UNAVAILABLE` | No no-replace publication primitive is available on the destination for a directory operation; refused before anything changes. | 241.5 |
```

- [ ] **Step 1: Verify the state matches this plan**

Open `crates/flux-fs/src/error.rs` and confirm lines 9-25 are exactly:

```rust
/// A spec error code. Only codes single-file copy can actually produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    CopyFailed,
    MetadataApplyFailed,
    DiskFull,
    PermissionDenied,
    SourceChanged,
    StrictDurabilityUnavailable,
    SpecialFileUnsupported,
    SafetyRejected,
    /// §2 item 83, §149.4. A DIRECTORY whose object changed under the walk -- not to
    /// be confused with `SourceChanged`, which is a FILE whose length or mtime moved
    /// under `copy_file` (§33). Different objects, different producers, no overlap.
    DirectoryChangedDuringScan,
    IoError,
}
```

If it differs, STOP and report `STATE_MISMATCH: <what differs>`.

- [ ] **Step 2: Write the failing assertions**

In `crates/flux-fs/src/error.rs`, in the test `every_code_has_the_spec_string` (currently at `:90-102`), add two lines immediately before the closing `}` of the function:

```rust
        assert_eq!(Code::NoReplacePublishUnavailable.as_str(), "NOREPLACE_PUBLISH_UNAVAILABLE");
        assert_eq!(Code::DestinationNamespaceCollision.as_str(), "DESTINATION_NAMESPACE_COLLISION");
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p flux-fs every_code_has_the_spec_string`

Expected: FAIL to COMPILE, with `error[E0599]: no variant or associated item named `NoReplacePublishUnavailable` found for enum `Code``.

A compile failure is the correct failure here. This test cannot fail at runtime for a missing variant, which is exactly why Step 5 adds the variants to an exhaustive `match` rather than relying on this test alone.

- [ ] **Step 4: Add the variants**

In `crates/flux-fs/src/error.rs`, insert into the `Code` enum immediately after the `DirectoryChangedDuringScan` variant and before `IoError`:

```rust
    /// §241.5. No no-replace publication primitive is available on the destination
    /// for a DIRECTORY operation, which is refused before anything changes. Nothing
    /// in this crate returns it: the engine raises it and the CLI maps it to exit 3.
    /// The vocabulary lands here because a `Code` variant is a `flux-fs` concern.
    NoReplacePublishUnavailable,
    /// §241.5, §2 item 83. Two distinct source paths map to the same destination
    /// object or prefix -- two names a case-insensitive destination folds into one,
    /// or two source roots colliding. Published one, refused the other, overwrote
    /// neither. Raised by the engine, not here.
    DestinationNamespaceCollision,
```

- [ ] **Step 5: Add the `as_str` arms**

In the `match self` of `Code::as_str` (`:29-40`), insert immediately after the `Code::DirectoryChangedDuringScan` arm:

```rust
            Code::NoReplacePublishUnavailable => "NOREPLACE_PUBLISH_UNAVAILABLE",
            Code::DestinationNamespaceCollision => "DESTINATION_NAMESPACE_COLLISION",
```

This `match` has no wildcard arm, which is what forces every future variant to be given a string. `crates/flux-core/src/copy.rs:22-25` also matches on `Code` but has a binding catch-all (`classified => classified`), so it compiles unchanged — do not touch it.

- [ ] **Step 6: Run the gate**

Run: `just check`

Expected: all four stages pass. `flux-fs` test count rises by 0 (the assertions went into an existing test); the workspace total is unchanged.

- [ ] **Step 7: Commit**

```bash
git add crates/flux-fs/src/error.rs
git commit -m "$(cat <<'EOF'
feat(flux-fs): add the no-replace publication and namespace collision codes

Both belong to the engine and CLI cuts, which are what raise and render them.
The vocabulary lands here because a Code variant is a flux-fs concern, and
adding it with the primitive keeps one feature's taxonomy in one cut rather
than scattering it across two.

The strings are normative: FLUX_FULL_UPDATED_SPEC_V16.md:2927 and :2938 give
the registry entries, and the registry says a change introducing a new code
MUST add it there and MUST NOT introduce a synonym.

as_str's match has no wildcard, so both variants had to be given strings to
compile -- which is the mechanism that keeps the taxonomy honest, since the
test that asserts each string would silently omit a variant nobody added to it.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: `FaultFs::set_no_replace_support`

**Files:**
- Modify: `crates/flux-core/src/fault_fs.rs` — the `Inner` struct (`:14-68`), a new method near `set_identity` (`:293-298`), and `rename_no_replace` (`:502-518`)

The engine cut needs to test its behaviour against a destination whose filesystem lacks the primitive, without owning such a filesystem. This is the switch that makes that possible. It defaults to `true` so no existing test changes.

- [ ] **Step 1: Verify the state matches this plan**

Confirm `crates/flux-core/src/fault_fs.rs:502-518` is exactly:

```rust
    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()> {
        let (f, t) = (from.to_path_buf(), to.to_path_buf());
        self.record(
            format!("rename_no_replace({} -> {})", f.display(), t.display()),
            "rename_no_replace",
        )?;
        let mut g = self.inner.lock().unwrap();
        missing_source(&g, &f)?;
        if g.files.contains_key(&t) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        move_object(&mut g, &f, &t);
        Ok(())
    }
```

If it differs, STOP and report `STATE_MISMATCH: <what differs>`.

- [ ] **Step 2: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block at the end of `crates/flux-core/src/fault_fs.rs`:

```rust
    #[test]
    fn no_replace_support_is_on_by_default() {
        // Every existing test was written before this switch existed and must keep
        // passing untouched, which is only true if the default is the old behaviour.
        let fs = FaultFs::new();
        fs.write_file("/from", b"new");
        assert!(fs.rename_no_replace(Path::new("/from"), Path::new("/to")).is_ok());
    }

    #[test]
    fn a_destination_without_the_primitive_reports_it() {
        let fs = FaultFs::new();
        fs.write_file("/from", b"new");
        fs.set_no_replace_support(false);

        let err = fs.rename_no_replace(Path::new("/from"), Path::new("/to")).unwrap_err();

        // NOT AlreadyExists: the name is free. The filesystem cannot make the promise
        // at all, which is a different fact and the one the engine cut branches on.
        assert_eq!(err.code, Code::IoError);
        assert_eq!(err.source.kind(), std::io::ErrorKind::Unsupported);
        assert!(
            !fs.exists("/to"),
            "an unsupported primitive must not fall back to a plain rename"
        );
        assert!(fs.exists("/from"), "the source must be untouched");
    }
```

**Note the accessor shape, which is easy to get wrong.** `FsError` has PUBLIC FIELDS, not accessor methods — `pub code: Code` and `pub source: std::io::Error`, verified at `crates/flux-fs/src/error.rs:44-56`. So it is `err.code` and `err.source.kind()`, with no parentheses on the first. The `err.code()` form does exist, but on `CopyError`, a different type in `flux-core` (`crates/flux-core/src/copy.rs:76`), and using it here will not compile. The established idiom in this very test file is `assert_eq!(err.code, flux_fs::Code::PermissionDenied);` at `crates/flux-platform/tests/std_fs.rs:50`.

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p flux-core no_replace_support`

Expected: FAIL to COMPILE with `error[E0599]: no method named `set_no_replace_support` found`.

- [ ] **Step 4: Add the field**

In the `Inner` struct in `crates/flux-core/src/fault_fs.rs`, add immediately after the `directories: HashSet<PathBuf>,` field and its doc comment:

```rust
    /// Does this fake's `rename_no_replace` have an atomic no-replace primitive?
    ///
    /// `true` by default, because every test written before this switch existed
    /// assumes it. Setting it `false` is how the engine cut exercises a destination
    /// whose filesystem cannot make the promise, without owning such a filesystem.
    no_replace_support: Option<bool>,
```

`Option<bool>` rather than `bool` because `Inner` derives `Default` (verified at `:14`), and `bool::default()` is `false` — the wrong default. `None` reads as "not configured", which the method below treats as `true`.

- [ ] **Step 5: Add the configuration method**

Add immediately after `set_identity` (which ends at `:298`), matching its established shape — doc comment stating rationale, one locked mutation, no return value:

```rust
    /// Whether this fake's `rename_no_replace` has an atomic no-replace primitive.
    /// Defaults to `true`; set `false` to make it report the platform's unsupported
    /// error, which is how a caller's behaviour on such a destination is tested
    /// without a filesystem that genuinely lacks one.
    pub fn set_no_replace_support(&self, supported: bool) {
        self.inner.lock().unwrap().no_replace_support = Some(supported);
    }
```

- [ ] **Step 6: Consult it in `rename_no_replace`**

In `crates/flux-core/src/fault_fs.rs:502-518`, insert immediately after the `missing_source(&g, &f)?;` line and before the `if g.files.contains_key(&t)` block:

```rust
        // Checked BEFORE the occupancy test, deliberately: a filesystem that cannot
        // refuse-on-replace cannot answer the occupancy question atomically either,
        // so reporting AlreadyExists here would claim a guarantee this fake is
        // modelling the absence of.
        if g.no_replace_support == Some(false) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "no atomic no-replace publication primitive",
                ),
            ));
        }
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p flux-core no_replace_support`

Expected: PASS, 2 tests.

- [ ] **Step 8: Prove the new tests are not vacuous**

Temporarily change the guard you just added from `== Some(false)` to `== Some(true)` — a LOGIC mutant, not a structural one.

Run: `cargo test -p flux-core no_replace_support`

Expected: `no_replace_support_is_on_by_default` FAILS (it now errors where it should succeed). Confirm **that specific test** went red, not merely that the suite is non-zero. Then revert the mutant and confirm both pass again.

If the mutant does not turn that test red, the test is not pinning what it claims and you should say so rather than proceeding.

- [ ] **Step 9: Run the gate**

Run: `just check`

Expected: all four stages pass. `flux-core` rises from 69 to 71 static `#[test]`s.

- [ ] **Step 10: Commit**

```bash
git add crates/flux-core/src/fault_fs.rs
git commit -m "$(cat <<'EOF'
test(flux-core): let FaultFs model a filesystem with no no-replace primitive

The engine cut has to exercise what happens on a destination whose filesystem
cannot publish without replacing, and owning such a filesystem in a test is not
practical. This is the switch.

Defaults to true, so every test written before it exists passes untouched. The
field is Option<bool> rather than bool because Inner derives Default and
bool::default() is false, which is the wrong default and would have silently
inverted the behaviour of every existing rename_no_replace test.

The guard is consulted BEFORE the occupancy test. A filesystem that cannot
refuse-on-replace cannot answer the occupancy question atomically either, so
returning AlreadyExists there would claim exactly the guarantee this fake is
modelling the absence of.

Non-vacuousness proven by mutating the guard's comparison and confirming the
default-on test specifically went red.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: The Windows extended-length path helper

**Files:**
- Modify: `crates/flux-platform/src/std_fs.rs` — add a `#[cfg(windows)]` free function and its unit tests

§105 is normative: *"Flux uses extended-length paths (`\\?\`) for every filesystem call on Windows, so the legacy 260-character path limit never applies"* (`FLUX_FULL_UPDATED_SPEC_V16.md:5172-5174`).

**This helper is where the cut is most easily got wrong, so read this before writing it.** Two obvious implementations are both wrong:

- **`canonicalize` the whole path** resolves symlinks, so `metadata` would report a link's TARGET instead of the link. That silently disables the Step 2a identity gate, the symlinked-anchor refusal and the item-114 capture, all of which depend on `symlink_metadata` semantics. It produces the right-looking string.
- **Resolve `.` and `..` lexically** is not semantics-preserving: if `link` points at `C:\tmp`, then `link\..\b` physically names `C:\b`, while popping `link` textually yields `.\b`. A `\\?\` path *disables* the kernel's own parsing, so the wrong answer is then taken literally rather than corrected.

The rule is: **canonicalize the PARENT, append the final component verbatim** — with three preconditions that make it total.

- [ ] **Step 1: Write the failing tests**

Add to `crates/flux-platform/tests/std_fs.rs`:

```rust
#[cfg(windows)]
#[test]
fn extended_length_conversion_leaves_the_final_component_unresolved() {
    // The whole point. `canonicalize` on the full path would resolve the link and
    // the adapter would then stat its TARGET, silently disabling every check that
    // depends on a link being reported as a link.
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("target");
    std::fs::write(&target, b"x").unwrap();
    let link = d.path().join("link");
    if std::os::windows::fs::symlink_file(&target, &link).is_err() {
        eprintln!("SKIPPED: creating a symlink needs Developer Mode or elevation");
        return;
    }

    let fs = StdFileSystem;
    let m = fs.metadata(&link).expect("the link must be stattable");
    assert_eq!(m.file_type, FileType::Symlink, "the link must be reported as a link");
}

#[cfg(windows)]
#[test]
fn a_destination_path_longer_than_260_characters_works() {
    // Item 103. A tree copy is what GENERATES long destination paths, so this is
    // the property the whole \\?\ conversion exists for.
    let d = tempfile::tempdir().unwrap();
    let mut deep = d.path().to_path_buf();
    // 12 components of 30 characters each clears 260 comfortably.
    for _ in 0..12 {
        deep.push("a".repeat(30));
        std::fs::create_dir(&deep).expect("each level must be creatable");
    }
    assert!(
        deep.as_os_str().len() > 260,
        "the probe must actually exceed MAX_PATH, got {}",
        deep.as_os_str().len()
    );

    let target = deep.join("file.txt");
    let fs = StdFileSystem;
    let mut f = fs.create_new(&target).expect("create_new must work past MAX_PATH");
    f.write_all(b"deep").unwrap();
    drop(f);

    let m = fs.metadata(&target).expect("metadata must work past MAX_PATH");
    assert_eq!(m.len, 4);
    assert_eq!(m.file_type, FileType::File);
}
```

**These tests need `FileType`, which the file does not currently import.** Line 1 of
`crates/flux-platform/tests/std_fs.rs` is exactly:

```rust
use flux_fs::{FileIdentity, FileSystem};
```

Change it to:

```rust
use flux_fs::{FileIdentity, FileSystem, FileType};
```

`rustfmt.toml` sets `reorder_imports = true`, so keep the list alphabetical or `cargo fmt --check` will fail.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-platform a_destination_path_longer_than_260`

Expected: FAIL. The `create_new` or `metadata` call errors with an OS error about the path being too long, because nothing prefixes it yet.

If it unexpectedly PASSES, the machine has Windows long-path support enabled system-wide (`LongPathsEnabled` in the registry) and this test cannot distinguish the two states. Report that and use the symlink test as the only pin for this task, noting the limitation.

- [ ] **Step 3: Write the helper**

Add to `crates/flux-platform/src/std_fs.rs`, immediately after the `#[cfg(windows)] fn destination_is_write_protected` block (which ends at `:147`):

```rust
/// Convert a path to its Windows extended-length (`\\?\`) form.
///
/// §105 is normative: "Flux uses extended-length paths (`\\?\`) for every filesystem
/// call on Windows, so the legacy 260-character path limit never applies." Every call
/// means every call, not every CLI call, so this belongs to the adapter rather than to
/// any one caller.
///
/// **Canonicalize the PARENT, never the whole path.** `canonicalize` resolves symlinks,
/// so routing `metadata` through it would report a link's TARGET as a regular file and
/// silently disable every check in this project that depends on a link being reported
/// as a link. Appending the final component verbatim keeps `symlink_metadata`
/// semantics exactly as they were.
///
/// **And never resolve `.` or `..` textually.** If `link` points at `C:\tmp`, then
/// `link\..\b` physically names `C:\b` while popping `link` lexically yields `.\b` --
/// and a `\\?\` path SUPPRESSES the kernel's parsing, so the wrong answer would be
/// taken literally instead of corrected.
///
/// Three preconditions make the rule total:
///
/// 1. absolute first, because `Path::new("foo").parent()` is `Some("")` and
///    canonicalizing the empty string fails;
/// 2. a path with no parent, or whose final component is `.` or `..`, is canonicalized
///    WHOLE -- neither names an object whose link-ness matters, and a trailing `..`
///    appended verbatim would be asked of the filesystem literally;
/// 3. an already-prefixed path is returned unchanged, so this is idempotent and safe
///    to apply at every call site without tracking whether it has run.
///
/// Falls back to the path as given when the parent cannot be resolved. The call was
/// going to fail anyway, and failing in the caller with its own error is better than
/// failing here with a different one.
#[cfg(windows)]
fn extended_length(path: &Path) -> std::path::PathBuf {
    use std::path::Component;

    // (3) Idempotent. `Prefix` covers both `\\?\` (Verbatim*) and the plain forms; only
    // the verbatim ones are already extended-length.
    if matches!(
        path.components().next(),
        Some(Component::Prefix(p))
            if matches!(
                p.kind(),
                std::path::Prefix::VerbatimDisk(_)
                    | std::path::Prefix::VerbatimUNC(_, _)
                    | std::path::Prefix::Verbatim(_)
            )
    ) {
        return path.to_path_buf();
    }

    // (1) Absolute first, textually -- no link is resolved by joining.
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => return path.to_path_buf(),
        }
    };

    // (2) Nothing to protect: canonicalize whole.
    let final_is_name = matches!(absolute.components().next_back(), Some(Component::Normal(_)));
    if !final_is_name {
        return std::fs::canonicalize(&absolute).unwrap_or(absolute);
    }

    let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) else {
        return std::fs::canonicalize(&absolute).unwrap_or(absolute);
    };

    match std::fs::canonicalize(parent) {
        Ok(p) => p.join(name),
        Err(_) => absolute,
    }
}
```

`std::fs::canonicalize` on Windows returns a `\\?\`-prefixed path, which is why no manual prefixing is needed — the prefix comes from the canonicalization of the parent and the verbatim final component inherits it.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p flux-platform extended_length_conversion`

Expected: the symlink test PASSES (or prints `SKIPPED` without Developer Mode). The long-path test still fails, because nothing calls the helper yet — Task 4 wires it in.

- [ ] **Step 5: Confirm the helper compiles away on non-Windows**

Run: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-walk2 && cargo clippy -p flux-platform --all-targets -- -D warnings'`

Expected: clean. The helper is `#[cfg(windows)]` and so is every test above it, so Linux sees none of it. A `dead_code` warning here would mean the cfg is wrong.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-platform/src/std_fs.rs crates/flux-platform/tests/std_fs.rs
git commit -m "$(cat <<'EOF'
feat(flux-platform): add the Windows extended-length path conversion

Section 105 is normative -- Flux uses \\?\ paths for EVERY filesystem call on
Windows, so the legacy 260-character limit never applies. Every call means
every call rather than every CLI call, which is why this is an adapter property
and not a caller's.

The conversion canonicalizes the PARENT and appends the final component
verbatim. Canonicalizing the whole path is the obvious implementation and would
break this project at its foundation: it resolves symlinks, so metadata routed
through it reports a link's target instead of the link, silently disabling the
identity gate, the symlinked-anchor refusal and the directory-swap capture at
once. Resolving . and .. textually is the other obvious implementation and is
equally wrong, because a \\?\ path suppresses the kernel's own parsing and the
wrong answer is then taken literally rather than corrected.

Three preconditions make the rule total: absolute first, since a
single-component relative path has parent "" and canonicalizing that fails;
whole-path canonicalization when there is no parent or the final component is
. or .., neither of which names an object whose link-ness matters; and
idempotence on an already-prefixed path, so it is safe to apply at every call
site without tracking whether it has run.

Nothing calls it yet. The next commit wires it in.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Route every Windows call through the helper

**Files:**
- Modify: `crates/flux-platform/src/std_fs.rs` — the `impl FileSystem for StdFileSystem` block at `:149-309` and the two `#[cfg(windows)]` free functions

Mechanical but wide. Every method that takes a `&Path` and reaches the filesystem must convert first, on Windows only.

- [ ] **Step 1: Enumerate the call sites**

The complete list of `StdFileSystem` methods taking a path, verified at `:149-309`. There are no others:

| Method | Lines | Paths to convert |
|---|---|---|
| `open_read` | 153-155 | `path` |
| `create_new` | 157-165 | `path` |
| `metadata` (windows arm) | 184-213 | `path` |
| `rename_replace` | 237-262 | `from`, `to` |
| `rename_no_replace` | 264-276 | `from`, `to` |
| `remove_file` | 278-280 | `path` |
| `read_dir` | 282-304 | `path` |
| `create_dir` | 306-308 | `path` |

Plus the `#[cfg(windows)]` free function `destination_is_write_protected` at `:108-147`, which opens `to` directly.

The `#[cfg(unix)]` `metadata` at `:167-182` is NOT converted — `\\?\` is a Windows concept.

- [ ] **Step 2: Add the conversion shim**

Add immediately after the `extended_length` function from Task 3:

```rust
/// The path as the OS should see it. A no-op on every platform but Windows, so call
/// sites stay single-form instead of sprouting a `#[cfg]` each.
#[cfg(windows)]
fn os_path(path: &Path) -> std::borrow::Cow<'_, Path> {
    std::borrow::Cow::Owned(extended_length(path))
}

/// The path as the OS should see it. See the Windows twin: only Windows needs the
/// extended-length form, so everywhere else this borrows and costs nothing.
#[cfg(not(windows))]
fn os_path(path: &Path) -> std::borrow::Cow<'_, Path> {
    std::borrow::Cow::Borrowed(path)
}
```

A `Cow` rather than two cfg-gated call sites per method: the Unix arm borrows and allocates nothing, the Windows arm owns, and each method body stays one line longer instead of doubling.

- [ ] **Step 3: Convert each call site**

In each method in the table above, bind the converted path first and use it for the filesystem call. For example `create_dir` (`:306-308`) becomes:

```rust
    fn create_dir(&self, path: &Path) -> Result<()> {
        std::fs::create_dir(os_path(path).as_ref()).map_err(FsError::from_io)
    }
```

and `remove_file` (`:278-280`):

```rust
    fn remove_file(&self, path: &Path) -> Result<()> {
        std::fs::remove_file(os_path(path).as_ref()).map_err(FsError::from_io)
    }
```

Apply the same shape to every row. **Two rules:**

- **`read_dir` converts the directory it lists, and must NOT convert the entry names it returns.** The names are bare components, and the walk panics on a name that is not one bare component (`crates/flux-core/src/walk.rs:222`). Returning a prefixed absolute path as an entry name would trip that panic.
- **`rename_replace` converts both `from` and `to`, and must also convert the path it hands `destination_is_write_protected`** — otherwise the guard consults an unprefixed path while the rename uses a prefixed one, and a long destination would be checked and renamed inconsistently.

- [ ] **Step 4: Run the long-path test**

Run: `cargo nextest run -p flux-platform a_destination_path_longer_than_260`

Expected: PASS. This is the test that failed at the end of Task 3.

- [ ] **Step 5: Run the whole platform suite**

Run: `cargo nextest run -p flux-platform`

Expected: every previously-passing test still passes. This is the step that catches an over-eager conversion — if `read_dir` entry names or the `metadata` symlink typing broke, it shows here.

- [ ] **Step 6: Cross-check under WSL**

Run: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-walk2 && cargo nextest run --workspace --no-tests=pass'`

Expected: PASS. `os_path` borrows on Linux, so behaviour is unchanged there; this proves the `cfg(not(windows))` arm compiles and that no call site accidentally assumes an owned value.

- [ ] **Step 7: Run the gate**

Run: `just check`

Expected: all four stages pass.

- [ ] **Step 8: Commit**

```bash
git add crates/flux-platform/src/std_fs.rs
git commit -m "$(cat <<'EOF'
feat(flux-platform): route every Windows filesystem call through \\?\

Section 105 says every call, so every method of StdFileSystem that takes a path
now converts it first: open_read, create_new, the Windows metadata arm,
rename_replace, rename_no_replace, remove_file, read_dir and create_dir, plus
the write-protection guard that opens the destination directly.

The conversion goes through a Cow-returning shim that is a borrow on every
platform but Windows, so call sites stay one line instead of sprouting a cfg
each and the Unix arm allocates nothing.

Two call sites needed care. read_dir converts the directory it lists but NOT
the entry names it returns, which are bare components -- the walk panics on a
name that is not one bare component, so returning a prefixed absolute path
there would trip it. And rename_replace converts the path it hands the
write-protection guard as well as both rename endpoints, so the guard cannot
consult an unprefixed path while the rename uses a prefixed one.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: The atomic primitive

**Files:**
- Modify: `crates/flux-platform/src/std_fs.rs:264-276` (split into two cfg arms)

This is the cut's reason to exist. `FLUX_FULL_UPDATED_SPEC_V16.md:10873-10876` forbids the current body by name: *"check-then-rename is never used as a substitute."*

- [ ] **Step 1: Write the failing test**

The existing tests (`:97-107`, `:150-169`) already pin refusal, and they will keep passing with either implementation — that is exactly the problem, and it is why they cannot be the oracle for this change. What distinguishes atomic from check-then-act is the concurrent case, which a test cannot reliably schedule.

So pin what IS observable: that the refusal comes from the OS rather than from a pre-check, by removing the pre-check's only possible source of truth. Add to `crates/flux-platform/tests/std_fs.rs`:

```rust
#[test]
fn rename_no_replace_refuses_a_directory_occupying_the_name() {
    // A directory at the target is a case the old check-then-act body got right only
    // by accident -- `symlink_metadata` says "something is there" without saying what.
    // The atomic primitives refuse it as the OS's own answer, and on every platform.
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    fs.create_new(&from).unwrap();
    std::fs::create_dir(&to).unwrap();

    let err = fs.rename_no_replace(&from, &to).expect_err("a directory occupies the name");
    assert_eq!(err.code, flux_fs::Code::IoError);

    assert!(std::fs::metadata(&to).unwrap().is_dir(), "the directory must survive");
    assert!(from.exists(), "the source must be untouched");
}
```

- [ ] **Step 2: Run it against the current implementation**

Run: `cargo nextest run -p flux-platform rename_no_replace_refuses_a_directory`

Expected: PASS already, on the check-then-act body. Record that it passes.

This test is a REGRESSION pin, not a red-then-green cycle: it exists so that replacing the body cannot quietly change behaviour on a case the old code handled. Not every change has a test that fails first, and pretending otherwise by writing a test that fails for an unrelated reason would be worse.

- [ ] **Step 3: Confirm no manifest change is needed**

**No `Cargo.toml` change is required.** Verified against the vendored crate at
`E:\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f\windows-sys-0.61.2`:

- `MoveFileExW` is declared at `src/Windows/Win32/Storage/FileSystem/mod.rs:273` with **no `#[cfg(feature = ...)]` attribute above it**, so it needs only the `Win32_Storage_FileSystem` feature — already enabled by the workspace pin.
- Its flags parameter is typed `MOVE_FILE_FLAGS`, which is `pub type MOVE_FILE_FLAGS = u32;` at `:2165`. A bare `0` therefore compiles without a cast.
- `MOVEFILE_REPLACE_EXISTING` is `1u32` at `:2163`, which Step 3 of Task 6 uses as its mutant.

The workspace pin, verified at the root `Cargo.toml`, is:

```toml
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_Storage_FileSystem", "Win32_System_IO"] }
```

Run the grep yourself before relying on this — if your `windows-sys` patch version differs and the symbol has moved behind a feature, add it to the workspace pin and say which:

```
rg -n "fn MoveFileExW" "$CARGO_HOME/registry/src/index.crates.io-1949cf8c6b5b557f/windows-sys-0.61.2/src/Windows/Win32/Storage/FileSystem/mod.rs"
```

- [ ] **Step 4: Replace the body with two cfg arms**

In `crates/flux-platform/src/std_fs.rs`, replace the whole of `fn rename_no_replace` (`:264-276`) with:

```rust
    /// Publish without replacing, atomically.
    ///
    /// §241.5 requires a target planned as new be published "with a primitive that
    /// refuses to replace an existing entry", and `:10876` forbids the alternative by
    /// name: "check-then-rename is never used as a substitute." The previous body was
    /// exactly that substitute -- `symlink_metadata` then `rename` -- so two processes
    /// could both see a free name and one silently won.
    ///
    /// The contract and the error are UNCHANGED: an occupied target is still
    /// `Code::IoError` carrying `ErrorKind::AlreadyExists`. Only the atomicity is new,
    /// and the observable difference is confined to the concurrent case that used to
    /// lose silently.
    #[cfg(unix)]
    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()> {
        // ONE arm for Linux and macOS, because rustix already abstracts the two
        // primitives §241.5 names separately: `RenameFlags::NOREPLACE` is
        // `RENAME_NOREPLACE` on Linux and `RENAME_EXCL` on Apple (rustix
        // src/backend/libc/fs/types.rs, the `#[cfg(apple)]` bitflags block), and
        // `renameat_with` is gated `any(apple, linux_kernel, redox)`.
        use rustix::fs::{CWD, RenameFlags, renameat_with};

        renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE)
            .map_err(|e| FsError::from_io(std::io::Error::from(e)))
    }

    /// See the Unix twin for why this is atomic rather than check-then-act.
    #[cfg(windows)]
    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()> {
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

        // No MOVEFILE_REPLACE_EXISTING is the whole point: without that flag
        // MoveFileExW fails rather than replacing, which is the guarantee this method
        // has always promised and until now only approximated.
        let (from, to) = (os_path(from), os_path(to));
        let wide = |p: &Path| {
            use std::os::windows::ffi::OsStrExt;
            p.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>()
        };
        let (wfrom, wto) = (wide(from.as_ref()), wide(to.as_ref()));

        // SAFETY: both buffers are NUL-terminated UTF-16 built immediately above and
        // live for the duration of the call.
        let ok = unsafe { MoveFileExW(wfrom.as_ptr(), wto.as_ptr(), 0) };
        if ok == 0 {
            return Err(FsError::from_io(std::io::Error::last_os_error()));
        }
        Ok(())
    }
```

**Do not convert the paths in the Unix arm** — `os_path` is a borrow there and `\\?\` is meaningless on Unix, but passing it would still be harmless noise; leaving it out keeps the arm honest about what it does.

- [ ] **Step 5: Add the unsupported-platform guard**

`renameat_with` does not exist on FreeBSD, NetBSD, Solaris or other Unixes. A bare `#[cfg(unix)]` would fail there with a confusing missing-function error. Add immediately before the `impl FileSystem for StdFileSystem` block at `:149`:

```rust
// `rustix::fs::renameat_with` is gated `any(apple, linux_kernel, target_os = "redox")`,
// so a Unix outside that set has no atomic no-replace primitive reachable from here.
// Fail at BUILD time naming the reason, rather than at link time naming a missing
// function -- and do NOT add a check-then-act fallback, which
// FLUX_FULL_UPDATED_SPEC_V16.md:10876 forbids as a substitute by name.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))))]
compile_error!(
    "no atomic no-replace rename primitive on this target; see FLUX_FULL_UPDATED_SPEC_V16.md \
     §241.5 -- check-then-rename is not an acceptable substitute"
);
```

- [ ] **Step 6: Run the platform suite on Windows**

Run: `cargo nextest run -p flux-platform`

Expected: every test passes, including `rename_no_replace_refuses_an_existing_target` (`:97-107`) and the new directory test. The error on an occupied target must still classify as `Code::IoError` — if it does not, `FsError::from_io` is not mapping `ERROR_ALREADY_EXISTS`/`ERROR_FILE_EXISTS` the way the old explicit construction did, and you should report that rather than changing the test.

- [ ] **Step 7: Cross-check under WSL — this is the load-bearing one**

Run: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-walk2 && cargo nextest run --workspace --no-tests=pass'`

Expected: PASS, including `rename_no_replace_refuses_a_dangling_symlink` (`:150-169`, `#[cfg(unix)]`), which the Windows gate cannot run at all.

That test is the one most likely to break: `RENAME_NOREPLACE` must refuse a dangling symlink occupying the name, and if the kernel resolved the link instead it would see a free name and succeed. If it fails, **STOP** — that is a real behavioural regression and not a test to adjust.

- [ ] **Step 8: Run the gate**

Run: `just check`

Expected: all four stages pass.

- [ ] **Step 9: Commit**

```bash
git add crates/flux-platform/src/std_fs.rs crates/flux-platform/tests/std_fs.rs
git commit -m "$(cat <<'EOF'
feat(flux-platform): publish without replacing atomically

FLUX_FULL_UPDATED_SPEC_V16.md:10876 forbids the previous body by name --
"check-then-rename is never used as a substitute" -- and that is exactly what it
was: symlink_metadata, then rename, so two processes could both see a free name
and one silently won.

The three primitives §241.5 names ship as TWO arms, not three, because rustix
already abstracts the split. RenameFlags::NOREPLACE is RENAME_NOREPLACE on
Linux and RENAME_EXCL on Apple -- verified in the vendored source, the
#[cfg(apple)] bitflags block of src/backend/libc/fs/types.rs -- and
renameat_with is gated any(apple, linux_kernel, redox). Writing two Unix arms
would have duplicated an abstraction the dependency already provides.

That gate also excludes FreeBSD, NetBSD and Solaris, where a bare cfg(unix) arm
would fail with a confusing missing-function error. A compile_error! names the
reason instead, and deliberately offers no fallback: a check-then-act path for
an unsupported platform is the substitute the spec forbids.

The contract and the error are unchanged. An occupied target is still
Code::IoError carrying AlreadyExists, which the existing tests pin and which the
engine cut, not this one, maps to DESTINATION_NAMESPACE_COLLISION.

Verified under WSL as well as the Windows gate, because the dangling-symlink
refusal is cfg(unix) and the local gate cannot run it. That test is the one at
real risk here: the primitive must refuse a dangling symlink occupying the name,
which it only does if the kernel declines to resolve it.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Close the `rename_no_replace` coverage gap

**Files:**
- Modify: `crates/flux-platform/tests/std_fs.rs`

`rename_replace` has three refusal tests — a read-only attribute case (`:29-64`), a Unix root-owned case (`:171-227`) and a Windows ACL case (`:229-267`). `rename_no_replace` has two tests total and no refusal-path counterpart. This cut rewrites `rename_no_replace`, so its coverage is in scope by the touched surface.

- [ ] **Step 1: Write the tests**

Add to `crates/flux-platform/tests/std_fs.rs`:

```rust
#[test]
fn rename_no_replace_succeeds_when_the_name_is_free() {
    // The happy path had no test at all: both existing tests assert refusal, so an
    // implementation that refused EVERYTHING would have passed them both.
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    let mut f = fs.create_new(&from).unwrap();
    f.write_all(b"payload").unwrap();
    drop(f);

    fs.rename_no_replace(&from, &to).expect("a free name must be claimable");

    assert_eq!(std::fs::read(&to).unwrap(), b"payload");
    assert!(!from.exists(), "the source name must be gone after a rename");
}

#[test]
fn rename_no_replace_reports_a_missing_source() {
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("absent"), d.path().join("to"));
    let fs = StdFileSystem;

    let err = fs.rename_no_replace(&from, &to).expect_err("there is nothing to rename");
    assert_eq!(err.code, flux_fs::Code::IoError);
    assert!(!to.exists(), "nothing may appear at the target");
}
```

- [ ] **Step 2: Run them**

Run: `cargo nextest run -p flux-platform rename_no_replace`

Expected: PASS, 5 tests total for `rename_no_replace` (2 pre-existing, 1 from Task 5, 2 here).

- [ ] **Step 3: Prove the happy-path test is not vacuous**

Temporarily change the Windows arm's `MoveFileExW(..., 0)` third argument to `MOVEFILE_REPLACE_EXISTING` (import it from the same module; it is `1u32`, verified at `windows-sys-0.61.2/.../FileSystem/mod.rs:2163`) — a logic mutant.

Run: `cargo nextest run -p flux-platform rename_no_replace`

Expected: `rename_no_replace_refuses_an_existing_target` and `rename_no_replace_refuses_a_directory_occupying_the_name` go RED; the happy-path test stays green, which is correct — it does not pin the no-replace property, it pins that the method works at all. Confirm which specific tests went red, then revert.

On Unix, mutate `RenameFlags::NOREPLACE` to `RenameFlags::empty()` for the equivalent proof and run the WSL command.

- [ ] **Step 4: Run the gate and cross-check**

Run: `just check`
Then: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-walk2 && cargo nextest run --workspace --no-tests=pass'`

Expected: both pass. `flux-platform` rises from 34 to 39 static `#[test]`s (Task 3 added 2, Task 5 added 1, Task 6 adds 2).

- [ ] **Step 5: Commit**

```bash
git add crates/flux-platform/tests/std_fs.rs
git commit -m "$(cat <<'EOF'
test(flux-platform): cover rename_no_replace's success and missing-source paths

rename_replace has three refusal tests; rename_no_replace had two tests and no
counterpart for either. More pointedly, BOTH of its existing tests assert
refusal, so an implementation that refused everything would have passed the
whole suite -- there was no test that the method works at all.

This cut rewrites that method, so its coverage is in scope by the surface
touched rather than by who wrote the gap.

The happy-path test is deliberately not a no-replace pin: it stays green under a
MOVEFILE_REPLACE_EXISTING mutant, which is correct, because what it pins is that
a free name is claimable. The refusal tests are what go red under that mutant,
and that was confirmed rather than assumed.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Final verification

**Files:** none modified.

- [ ] **Step 1: Run the full gate on Windows**

Run: `just check`

Expected: four stages pass.

- [ ] **Step 2: Run the full suite under WSL**

Run: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-walk2 && cargo nextest run --workspace --no-tests=pass'`

Expected: pass, with a higher test count than Windows because `#[cfg(unix)]` tests run.

- [ ] **Step 3: Confirm nothing outside the named files changed**

```bash
git diff --stat main..HEAD -- . ':(exclude)docs'
```

Expected: exactly five files — `crates/flux-fs/src/error.rs`, `crates/flux-core/src/fault_fs.rs`, `crates/flux-platform/src/std_fs.rs`, `crates/flux-platform/tests/std_fs.rs`, and `crates/flux-platform/Cargo.toml` — and that last one only if your `windows-sys` patch version moved `MoveFileExW` behind a feature, which the pinned 0.61.2 does not. Four files is the expected outcome.

Anything else is out of scope and should be reported, not committed.

- [ ] **Step 4: Confirm the forbidden shapes are absent**

```bash
rg -n "supports_no_replace_publish|noreplace-probe" crates/
```

Expected: no matches. Both were deliberately excluded; a match means the plan was over-implemented.

- [ ] **Step 5: STOP**

Pushing and opening a pull request are outward actions. **Do not push. Do not open a PR.** Report completion and wait for explicit approval.

---

## Self-review

**Spec coverage.** Delivery item 3 names five deliverables. Each maps to a task: the three primitives → Task 5 (as two arms, with the reason recorded); extended-length paths for every Windows call → Tasks 3 and 4; the two `Code` variants → Task 1; `FaultFs::set_no_replace_support` → Task 2; "no new trait method, no probe" → enforced by Task 7 Step 4. The contract-unchanged requirement is pinned by the pre-existing tests that Task 5 Step 6 requires to keep passing.

**Placeholder scan.** No TBDs and no conditionals. Two steps were conditional in an earlier draft and both have since been resolved by verification rather than left to the implementer: `FsError` exposes PUBLIC FIELDS (`err.code`, `err.source.kind()`) and not accessor methods, verified at `crates/flux-fs/src/error.rs:44-56` and against the idiom at `crates/flux-platform/tests/std_fs.rs:50`; and `MoveFileExW` needs no new feature, verified in the vendored `windows-sys-0.61.2` source. Each still tells the implementer how to re-check, because a plan that says "verified" without saying against what is asking to be believed rather than read.

**Type consistency.** `os_path` is introduced in Task 4 Step 2 and used in Task 4 Step 3 and Task 5 Step 4 under that name. `extended_length` is introduced in Task 3 and called only by `os_path`. `set_no_replace_support` has the same signature in Task 2's test, method and commit message. `no_replace_support` is `Option<bool>` throughout.

**Known gaps, stated rather than hidden.**

1. **Atomicity itself is not directly tested.** No test schedules two processes into the window. The tests pin the observable contract — refusal, on the cases the old body handled — and the mutants prove they discriminate. This is a real limit: a future body that reintroduced check-then-act would pass this suite. The compile-time structure is the guard, not the tests.
2. **The long-path test is environment-sensitive.** On a machine with `LongPathsEnabled` set system-wide it may pass before Task 4 wires the helper in. Task 3 Step 2 says what to do if that happens.
3. **The symlink test skips without Developer Mode.** It prints `SKIPPED` rather than failing, following the precedent set by the non-UTF-8 filename test, which skips where the filesystem refuses the name rather than gating to one OS and silently dropping the check elsewhere.
4. **Item 113 remains unmet** and is tracked debt, not a gap in this plan. The compliant probe needs the operation workspace, which is out of scope for every cut in this design.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-09-24-atomic-no-replace-publication.md`. Two execution options:

**1. Subagent-Driven (recommended)** — a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.
