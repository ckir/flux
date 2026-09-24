# Atomic No-Replace Publication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `rename_no_replace` refuse an occupied destination atomically instead of by
check-then-act.

**Architecture:** `flux-platform` only — no engine, no CLI, no new trait method, and no path-conversion
layer. `StdFileSystem::rename_no_replace` loses its `symlink_metadata`-then-`rename` body and gains two
platform arms: `rustix::fs::renameat_with` with `RenameFlags::NOREPLACE` on Unix, `MoveFileExW` without
`MOVEFILE_REPLACE_EXISTING` on Windows. `flux-fs` gains three `Code` variants; `FaultFs` gains one
configuration method.

**Tech Stack:** Rust 2024, MSRV 1.85. `rustix` 1.1 (`features = ["fs"]`, already a `cfg(unix)`
dependency of this crate). `windows-sys` 0.61 (already a `cfg(windows)` dependency, with every symbol
this plan needs already enabled — no manifest change). `tempfile` for tests.

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

**Note the version, because the citation and the build do not agree.** `Cargo.lock` pins `rustix`
**1.1.5**; only **1.1.4** was in the local registry cache, so every `rustix` quotation in this plan is
from 1.1.4. A patch bump does not normally move a `#[cfg]` or rename a flag, but that is an expectation
rather than a measurement. **Re-run the grep against the version your `Cargo.lock` actually resolves**
before relying on it, and say so if anything has moved:

```
rg -n "cfg\(apple\)" -A 14 "$CARGO_HOME/registry/src/index.crates.io-*/rustix-1.1.*/src/backend/libc/fs/types.rs"
```

**2. `renameat_with` does not exist on every Unix.** Its cfg excludes FreeBSD, NetBSD, Solaris and others. This repository's CI matrix is `[ubuntu-latest, macos-latest, windows-latest]`, so every supported target is covered — but a bare `#[cfg(unix)]` arm would fail to compile on an unsupported Unix with a confusing error about a missing function. Task 3 adds an explicit `compile_error!` so that failure names its own cause. **Do not add a check-then-act fallback for such a platform** — `FLUX_FULL_UPDATED_SPEC_V16.md:10876` forbids check-then-rename as a substitute by name.

**LINE NUMBERS DRIFT AS TASKS LAND, AND EVERY CITATION BELOW IS AS-OF THE BASE COMMIT.** Task 1 adds
lines to `error.rs`; Tasks 2 through 6 add lines to three more files. A citation like `:264-276` is a
description of where something was when this plan was written, not an instruction to edit those line
numbers. **Locate every target by NAME — the function, the struct, the test — and use the line range
only to confirm you found the right one.** This project has already paid for this once: a previous
plan's Task 3 citations had to be corrected mid-execution after Task 1 shortened the file by a single
line.

**Windows long paths are already handled, and an earlier draft of this plan spent two tasks on them.**

`std::fs` on Windows routes every path argument through `maybe_verbatim`, whose own doc comment reads
*"Returns a UTF-16 encoded path capable of bypassing the legacy `MAX_PATH` limits"* — verified in the
toolchain source at
`E:\.rustup	oolchains\stable-x86_64-pc-windows-msvc\lib
ustlib\src
ust\library\std\src\sys\path\windows.rs:78-84`.
It is reached from `with_native_path` at `:43-48`, the generic entry every Windows filesystem operation
uses for its path arguments, and directly from `File::open`, `mkdir`, `readdir` and `symlink` in
`sys/fs/windows.rs`. `get_long_path` at `:94` prepends the verbatim prefix for anything at or above 248
UTF-16 code units.

**So acceptance item 103's first half is satisfied by the standard library**, and the two tasks that
built an `extended_length` helper, an `os_path` shim and converted nine call sites have been deleted.
They were not merely redundant:

- `std::fs::canonicalize`, which the helper used on each path's parent, calls
  `GetFinalPathNameByHandleW` — an open-and-close handle cycle per call, paid per file in a walker.
  std's own conversion is `GetFullPathNameW`, a lexical string operation that touches no disk.
- Forcing `\?\` unconditionally would have changed behaviour on SHORT paths too. std omits the prefix
  below 248 characters, so Windows normalizes those — stripping trailing spaces and dots. With the
  prefix always applied, a name like `file.txt ` becomes creatable. §106 permits that latitude, but it
  is a behaviour change nobody asked for.

What does NOT follow, and was claimed in an earlier draft: that the helper would have broken the
symlink-dependent safety checks. It would not have. Both approaches leave the FINAL component
unresolved, so `symlink_metadata` still reports a link as a link, and for a path beneath a symlinked
component lexical and physical resolution reach the same object because the kernel follows the link
anyway. The case against the helper is redundancy, I/O cost and short-path normalization — not a safety
break.

**3. `rename_no_replace` is NOT `#[cfg]`-gated today** — it is one plain method in a non-gated `impl` block, at `crates/flux-platform/src/std_fs.rs:264-276`. Task 3 splits it. The file already mixes cfg styles (whole gated free functions, gated methods inside a non-gated `impl`, gated match arms), so a gated pair of methods matches the established pattern — `fn metadata` is already exactly that, at `:167-182` (unix) and `:184-213` (windows).

### What this cut does NOT do

- **No new trait method.** `supports_no_replace_publish` was proposed and dropped: there is no side-effect-free way to ask a filesystem whether it supports the flag, because reaching the filesystem's rename implementation requires passing path resolution, and a non-existent source returns `ENOENT` first. If you find yourself adding a capability query, **STOP and report `SHAPE_DIVERGENCE`**.
- **No probe.** The spec's `noreplace-probe` writes inside the operation workspace, and the workspace is out of scope for every cut in this design. Deferred by owner ruling; booked as tracked debt.
- **No change to `rename_no_replace`'s contract or its error.** It already promises to fail rather than replace. What changes is that it keeps that promise atomically. On an occupied target it must still return `Code::IoError` carrying `ErrorKind::AlreadyExists`, because `crates/flux-platform/tests/std_fs.rs:97-107` and `:150-169` pin that and the engine cut maps it, not this one.

- **A filesystem that lacks the flag at RUNTIME is a third outcome, and it is not `AlreadyExists`.** The
  `compile_error!` in Task 3 covers targets where `renameat_with` does not EXIST. It does not cover a
  Linux kernel whose particular filesystem — some NFS configurations, some FUSE mounts — rejects
  `RENAME_NOREPLACE` at call time with `EINVAL` or `ENOSYS`. That arrives through `FsError::from_io` as
  whatever `classify` makes of it, and it arrives whether or not the target was occupied, because the
  kernel rejects the flag before evaluating occupancy.

  **That is the designed path, not a gap.** The engine "learns at the first publish" precisely by
  receiving such an error — it is what the deferred probe would otherwise have discovered up front.
  **Do not add a fallback that retries without the flag:** that is the check-then-rename substitute
  `FLUX_FULL_UPDATED_SPEC_V16.md:10876` forbids by name. It is stated here so an implementer who meets
  it on a network mount recognises it rather than "fixing" it.

### The shell these commands assume

**Every command block in this plan is POSIX shell, not PowerShell.** The `git commit -m "$(cat <<'EOF'
... EOF)"` form used for every commit is a heredoc, and `<<` is a parser error in PowerShell. On
Windows, run these through the Bash tool rather than the PowerShell one. If you only have PowerShell,
write the commit message to a file and use `git commit -F <file>` — do not try to translate the heredoc
inline, because the message bodies contain characters PowerShell will interpret.

### The tools these commands assume

`just`, `cargo nextest`, `typos` and WSL. This repository declares its prerequisites in
`.claude/recommended-tools.json`, which names each tool, why it is needed and its install command — read
that file rather than guessing if a command is not found. A `command not found` from `just`, `typos` or
`cargo nextest` is an ENVIRONMENT problem, not a defect in the code you just wrote, and the distinction
matters because the gate failing for that reason looks identical to the gate failing for a real one.

If WSL is absent, the cross-check steps cannot run. **Say so and stop rather than skipping them
silently** — they are the only thing in this plan that exercises the `#[cfg(unix)]` arm, which is where
the atomic primitive actually lives on two of the three supported platforms.

### Use `cargo nextest run` for every filtered test command, never `cargo test`

**A `cargo test` filter that matches nothing exits 0.** MEASURED in this worktree:

```
cargo test        -p flux-fs definitely_no_such_test_9f3a
  -> cargo test: 0 passed, 12 filtered out (1 suite, 0.00s)      EXIT 0

cargo nextest run -p flux-fs definitely_no_such_test_9f3a
  -> error: no tests to run  (hint: use `--no-tests` to customize)   EXIT 4
```

Every verification step below selects tests by name, so a renamed test, a typo, or a test that was
never written would, under `cargo test`, produce a green step that ran no assertion at all. Under
`nextest` it fails loudly. **This matters most at the MUTANT steps**, where the expected outcome is a
FAILURE: a zero-match filter there reports success, which reads as "the mutant did not kill the test",
and the implementer would then correctly follow the plan's instruction to stop and report the test as
vacuous — a false alarm caused entirely by the runner.

An earlier draft used `cargo test` in Tasks 1 and 2 and `nextest` elsewhere. It is `nextest`
throughout now, which also matches what `just check` runs. `cargo test --doc` is a separate thing and
stays as it is.

**Read the count, not just the colour.** `nextest` prints `Starting N tests` — check `N` is the number
you expect. A step that says "Expected: PASS, 2 tests" means two, and one is a failure even though
nothing was red.

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
| `crates/flux-platform/src/std_fs.rs` | The real adapter | Modify: split `rename_no_replace` into two cfg arms |
| `crates/flux-platform/tests/std_fs.rs` | Adapter behaviour against a real filesystem | Modify: new tests for atomicity, the path helper, and long paths |

No files are created, no manifest changes (verified: `MoveFileExW` needs no feature this workspace does not already enable), and no file is split — `std_fs.rs` is 388 lines and stays coherent.

---

### Task 1: The three new `Code` variants

**Files:**
- Modify: `crates/flux-fs/src/error.rs:9-25` (the enum), `:27-42` (`as_str`), `:90-102` (the taxonomy test)

All three codes belong to the engine and CLI cuts, but the vocabulary is a `flux-fs` concern and adding
it here keeps one feature's taxonomy in one cut. Nothing in this cut returns them.

**`DestinationError` is the third, and it comes from the half of §105 that survives.** §105's second
sentence — `FLUX_FULL_UPDATED_SPEC_V16.md:5174-5177` — says *"A name or path the destination still
refuses as too long fails that action with `DESTINATION_ERROR` (`path_scoped`, Section 207)."* The
first sentence's requirement is met by the standard library (see the ground-truth section); this one is
a taxonomy obligation and is not met by anything, because the `Code` enum has no such variant.

The spec's registry is normative about the strings. Verified at `FLUX_FULL_UPDATED_SPEC_V16.md:2927` and `:2938`:

```
| `DESTINATION_NAMESPACE_COLLISION` | Two distinct source paths, or two source roots, map to the same destination object or prefix. | 16.1, 18.3, 241.5 |
| `NOREPLACE_PUBLISH_UNAVAILABLE` | No no-replace publication primitive is available on the destination for a directory operation; refused before anything changes. | 241.5 |
```

- [ ] **Step 0: Record the base commit**

```bash
git tag -f cut3-base HEAD
git rev-parse --short cut3-base
```

**A git tag, not a shell variable.** An earlier draft used `export CUT3_BASE=...`, which does not
survive: under subagent-driven execution each task runs in its own shell, so by Task 5 the variable is
empty and `git diff --stat ..HEAD` silently compares nothing against nothing and prints an empty diff —
which reads exactly like "no unexpected files" and is the most misleading possible failure. A tag lives
in the repository and every later task sees it.

Task 5 diffs against this tag and then deletes it.

**Do NOT use `main` as that base.** In this worktree the local `main` ref is stale — it points at
`bcbd4ac` while `origin/main` is at `489b947`, a gap of well over a hundred commits — so
`git diff main..HEAD` prints a wall of unrelated files and tells you nothing. `origin/main` is no good
either: this branch already carries the design commits and a `TODO.md` promote that are not part of
cut 3. The commit you are starting from is the only honest base.

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
        assert_eq!(Code::DestinationError.as_str(), "DESTINATION_ERROR");
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo nextest run -p flux-fs every_code_has_the_spec_string`

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
    /// §105, §207. A destination-side failure with no more specific code -- including
    /// a name or path the destination refuses as too long even through the
    /// extended-length call path. Path-scoped: it fails that ACTION, not the operation.
    DestinationError,
```

- [ ] **Step 5: Add the `as_str` arms**

In the `match self` of `Code::as_str` (`:29-40`), insert immediately after the `Code::DirectoryChangedDuringScan` arm:

```rust
            Code::NoReplacePublishUnavailable => "NOREPLACE_PUBLISH_UNAVAILABLE",
            Code::DestinationNamespaceCollision => "DESTINATION_NAMESPACE_COLLISION",
            Code::DestinationError => "DESTINATION_ERROR",
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

Run: `cargo nextest run -p flux-core no_replace_support`

Expected: FAIL to COMPILE with `error[E0599]: no method named `set_no_replace_support` found`. A
compile failure, not a test failure — nothing runs, which is the correct outcome here and the one case
in this plan where a zero-test run is expected.

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

Run: `cargo nextest run -p flux-core no_replace_support`

Expected: PASS, and `Starting 2 tests` — if it says 1, one of the two test names does not match the
filter and you are only running half of what you think you are.

- [ ] **Step 8: Prove the new tests are not vacuous**

**Two mutants, because one cannot kill both tests and an earlier draft of this step named the wrong
one.** The field is `Option<bool>` and `FaultFs::new()` leaves it `None`, so a mutant must be chosen
against that three-valued state rather than against a boolean. Work through each before running it — the
point of a mutant is that you can predict which test dies, and a mutant whose victim you cannot name in
advance is proving nothing.

**Mutant A — kills `a_destination_without_the_primitive_reports_it`.** Change the guard from
`== Some(false)` to `== Some(true)`.

Run: `cargo nextest run -p flux-core no_replace_support`

Expected: `a_destination_without_the_primitive_reports_it` FAILS. With the field set to `Some(false)`,
`Some(false) == Some(true)` is false, so the guard is skipped, the rename succeeds and the test's
`unwrap_err()` panics. `no_replace_support_is_on_by_default` stays GREEN, because `None == Some(true)`
is also false and the default path was always meant to succeed.

Revert, and confirm both pass.

**Mutant B — kills `no_replace_support_is_on_by_default`.** Change the guard from `== Some(false)` to
`!= Some(true)`.

Run: `cargo nextest run -p flux-core no_replace_support`

Expected: `no_replace_support_is_on_by_default` FAILS. With the field `None`, `None != Some(true)` is
true, so the guard now fires on the default path and the rename errors where the test expects success.
`a_destination_without_the_primitive_reports_it` stays GREEN, since `Some(false) != Some(true)` is also
true and that test wanted an error anyway.

Revert, and confirm both pass.

Confirm the SPECIFIC named test went red each time, not merely that the suite returned non-zero. If
either mutant fails to kill its named test, the test is not pinning what it claims — say so rather than
proceeding.

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

### Task 3: The atomic primitive

**Files:**
- Modify: `crates/flux-platform/src/std_fs.rs:264-276` (split into two cfg arms)

This is the cut's reason to exist. `FLUX_FULL_UPDATED_SPEC_V16.md:10873-10876` forbids the current body by name: *"check-then-rename is never used as a substitute."*

**No dependency on a path-conversion helper, and an earlier draft of this plan had one.** The Windows
arm below passes its paths to `MoveFileExW` directly. See "Windows long paths are already handled" in
the ground-truth section for why no `\?\` conversion is needed here or anywhere else.

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
- `MOVEFILE_REPLACE_EXISTING` is `1u32` at `:2163`, which Step 3 of Task 4 uses as its mutant.

The workspace pin, verified at the root `Cargo.toml`, is:

```toml
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_Storage_FileSystem", "Win32_System_IO"] }
```

Run the grep yourself before relying on this — if your `windows-sys` patch version differs and the symbol has moved behind a feature, add it to the workspace pin and say which:

```
rg -n "fn MoveFileExW" "$CARGO_HOME/registry/src/index.crates.io-1949cf8c6b5b557f/windows-sys-0.61.2/src/Windows/Win32/Storage/FileSystem/mod.rs"
```

- [ ] **Step 4: Replace the body with two cfg arms**

In `crates/flux-platform/src/std_fs.rs`, replace the whole of `fn rename_no_replace` with the two arms
below. **Find it by NAME, not by the line range** — see the standing note on line drift at the top of
this plan. It is unmodified by Tasks 1-4, so it should still read exactly as quoted in "Before you
start", and if it does not, something earlier went wrong and you should report `STATE_MISMATCH` rather
than editing around it.

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

        // `std::io::Error::from(Errno)` is `from_raw_os_error`, so `raw_os_error()`
        // SURVIVES the conversion and `FsError::from_io` can still classify -- verified
        // at rustix-1.1.4/src/io/errno.rs:58-63. That matters here specifically: the
        // comment on `FsError::source` records that rebuilding an error as
        // `Error::other(..)` destroys `raw_os_error()`, and that doing so was MEASURED
        // to turn `DiskFull` into `IoError`. Do not "improve" this line by adding
        // context to the error.
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
        let wide = |p: &Path| {
            use std::os::windows::ffi::OsStrExt;
            p.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>()
        };
        let (wfrom, wto) = (wide(from), wide(to));

        // SAFETY: both buffers are NUL-terminated UTF-16 built immediately above and
        // live for the duration of the call.
        let ok = unsafe { MoveFileExW(wfrom.as_ptr(), wto.as_ptr(), 0) };
        if ok == 0 {
            return Err(FsError::from_io(std::io::Error::last_os_error()));
        }
        Ok(())
    }
```

Both arms pass their paths straight through, with no conversion of any kind. `\\?\` is meaningless on
Unix, and on Windows `std` already applies it — see "Windows long paths are already handled" above.

- [ ] **Step 5: Add the unsupported-platform guard**

`renameat_with` does not exist on FreeBSD, NetBSD, Solaris or other Unixes. A bare `#[cfg(unix)]` would fail there with a confusing missing-function error. Add immediately before the `impl FileSystem for StdFileSystem` block at `:149`:

```rust
// `rustix::fs::renameat_with` is gated `any(apple, linux_kernel, target_os = "redox")`,
// so a Unix outside that set has no atomic no-replace primitive reachable from here.
// Fail at BUILD time naming the reason, rather than at link time naming a missing
// function -- and do NOT add a check-then-act fallback, which
// FLUX_FULL_UPDATED_SPEC_V16.md:10876 forbids as a substitute by name.
#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        // rustix's own gate includes redox, and redox sets target_family = "unix",
        // so omitting it here would refuse to compile on a platform the dependency
        // fully supports. An earlier draft did exactly that, contradicting the
        // comment directly above.
        target_os = "redox"
    ))
))]
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

- [ ] **Step 8: Prove the new body is what refuses**

Every test in this task passes both before and after the change, which is honest but means nothing here
yet demonstrates that the replacement does the work. Prove it with a LOGIC mutant now rather than
leaving the only proof in a later task: a run that stopped after this task would otherwise have
committed the cut's central change with no evidence at all.

**On Windows**, change the Windows arm's `MoveFileExW(..., 0)` third argument to
`MOVEFILE_REPLACE_EXISTING` — import it from the same module; it is `1u32`, verified at
`windows-sys-0.61.2/.../FileSystem/mod.rs:2163`.

Run: `cargo nextest run -p flux-platform rename_no_replace`

Expected: `rename_no_replace_refuses_an_existing_target` and
`rename_no_replace_refuses_a_directory_occupying_the_name` go RED. Confirm THOSE SPECIFIC tests went
red, not merely that the suite returned non-zero. Revert the mutant and confirm they pass again.

**On Unix**, the equivalent mutant is `RenameFlags::NOREPLACE` → `RenameFlags::empty()`, checked with
the WSL command from Step 7. `rename_no_replace_refuses_a_dangling_symlink` is the test that must go red
there.

**REVERT THE UNIX MUTANT, and then re-run the WSL command to prove you did.** This is the one mutant in
this plan that the local gate cannot protect you from. `just check` runs on Windows only, so a
`RenameFlags::empty()` left in the `#[cfg(unix)]` arm passes Step 9 cleanly and gets committed — and it
would then surface in Task 4's WSL cross-check, one commit later, as a pre-existing Unix test failing
inside a task that only added happy-path tests. That is a long way to debug back from.

```
wsl -e bash -lc 'cd /mnt/e/Rust/flux-walk2 && cargo nextest run --workspace --no-tests=pass'
```

Expected: PASS. Every Windows mutant in this plan is caught by the very next `just check`, because the
tests it kills run on Windows. This one is not, which is why it gets its own revert-and-verify step
rather than a trailing sentence.

- [ ] **Step 9: Run the gate**

Run: `just check`

Expected: all four stages pass.

- [ ] **Step 10: Commit**

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

### Task 4: Close the `rename_no_replace` coverage gap

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

Expected: PASS, and **`Starting 4 tests` on Windows, `Starting 5 tests` under WSL.**

**The count is platform-dependent and an earlier draft of this step said 5 flatly.** Of the two
pre-existing `rename_no_replace` tests, `rename_no_replace_refuses_a_dangling_symlink`
(`crates/flux-platform/tests/std_fs.rs:151-152`) is `#[cfg(unix)]`, so Windows never sees it. The four
on Windows are: the pre-existing occupied-target test, the directory test from Task 3, and the two
added here. A literal implementer following the old line would have seen 4, concluded they had failed
to write a test, and halted — the count discipline causing the exact false stop it was added to
prevent.

- [ ] **Step 3: Confirm the new tests behave correctly under the Task 3 mutant**

Re-apply the same `MOVEFILE_REPLACE_EXISTING` mutant from Task 3 Step 8.

Run: `cargo nextest run -p flux-platform rename_no_replace`

Expected: the two refusal tests go RED as before, and **`rename_no_replace_succeeds_when_the_name_is_free`
STAYS GREEN.** That is the correct outcome and the point of running it again: the happy-path test does
not pin the no-replace property, it pins that the method works at all, and a test that went red under
every mutant would be pinning nothing in particular. Revert the mutant afterwards.

- [ ] **Step 4: Run the gate and cross-check**

Run: `just check`
Then: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-walk2 && cargo nextest run --workspace --no-tests=pass'`

Expected: both pass. `flux-platform` rises from 34 to 37 static `#[test]`s — Task 3 added 1, Task 4 adds 2.

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

### Task 5: Final verification

**Files:** none modified.

- [ ] **Step 1: Run the full gate on Windows**

Run: `just check`

Expected: four stages pass.

- [ ] **Step 2: Run the full suite under WSL**

Run: `wsl -e bash -lc 'cd /mnt/e/Rust/flux-walk2 && cargo nextest run --workspace --no-tests=pass'`

Expected: pass, with a higher test count than Windows because `#[cfg(unix)]` tests run.

- [ ] **Step 3: Confirm nothing outside the named files changed**

```bash
# The tag created in Task 1 Step 0, NOT `main`.
git diff --stat cut3-base..HEAD
```

Expected: **exactly four files.**

```
crates/flux-fs/src/error.rs
crates/flux-core/src/fault_fs.rs
crates/flux-platform/src/std_fs.rs
crates/flux-platform/tests/std_fs.rs
```

A fifth, `crates/flux-platform/Cargo.toml`, appears only if your `windows-sys` patch version moved
`MoveFileExW` behind a feature the workspace does not already enable — which the pinned 0.61.2 does not,
so four is the outcome to expect and five is a signal to say why.

Anything else is out of scope and should be reported, not committed.

Anything else is out of scope and should be reported, not committed.

- [ ] **Step 4: Confirm the forbidden shapes are absent**

```bash
rg -n "supports_no_replace_publish|noreplace-probe|extended_length|os_path" crates/
```

Expected: no matches. All four were deliberately excluded — the first two because the probe defers with
the workspace, the last two because `std` already applies extended-length paths. A match means the plan
was over-implemented.

- [ ] **Step 5: Delete the base tag**

```bash
git tag -d cut3-base
```

It was scaffolding for Step 3's diff and should not outlive the cut.

- [ ] **Step 6: STOP**

Pushing and opening a pull request are outward actions. **Do not push. Do not open a PR.** Report completion and wait for explicit approval.

---

## Self-review

**Spec coverage.** Delivery item 3's deliverables each map to a task: the three primitives → Task 3 (as
two arms, with the reason recorded); the `Code` variants → Task 1, now three of them since §105's second
sentence needed `DestinationError`; `FaultFs::set_no_replace_support` → Task 2; "no new trait method, no
probe" → enforced by Task 5 Step 4. Extended-length Windows paths need no task at all: `std` applies them,
which is recorded in the ground-truth section. The contract-unchanged requirement is pinned by the
pre-existing tests that Task 3 Step 6 requires to keep passing.

**Placeholder scan.** No TBDs and no conditionals. Two steps were conditional in an earlier draft and both have since been resolved by verification rather than left to the implementer: `FsError` exposes PUBLIC FIELDS (`err.code`, `err.source.kind()`) and not accessor methods, verified at `crates/flux-fs/src/error.rs:44-56` and against the idiom at `crates/flux-platform/tests/std_fs.rs:50`; and `MoveFileExW` needs no new feature, verified in the vendored `windows-sys-0.61.2` source. Each still tells the implementer how to re-check, because a plan that says "verified" without saying against what is asking to be believed rather than read.

**Type consistency.** `set_no_replace_support` has the same signature in Task 2's test, method and
commit message. `no_replace_support` is `Option<bool>` throughout. No path-conversion helper is
referenced anywhere, the two tasks that defined one having been deleted.

**Known gaps, stated rather than hidden.**

1. **Atomicity itself is not directly tested.** No test schedules two processes into the window. The tests pin the observable contract — refusal, on the cases the old body handled — and the mutants prove they discriminate. This is a real limit: a future body that reintroduced check-then-act would pass this suite. The compile-time structure is the guard, not the tests.
2. **Long-path behaviour is not tested at all.** The tasks that tested it were deleted once `std` was
   shown to provide it, and nothing replaced them. That is a deliberate gap rather than an oversight:
   a test here would assert the standard library's behaviour, not Flux's. The risk it leaves is that a
   future change introducing a hand-rolled path layer would not be caught, which is why the design
   records why the machinery is absent.
3. **The symlink test skips without Developer Mode.** It prints `SKIPPED` rather than failing, following the precedent set by the non-UTF-8 filename test, which skips where the filesystem refuses the name rather than gating to one OS and silently dropping the check elsewhere.
4. **Item 113 remains unmet** and is tracked debt, not a gap in this plan. The compliant probe needs the operation workspace, which is out of scope for every cut in this design.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-09-24-atomic-no-replace-publication.md`. Two execution options:

**1. Subagent-Driven (recommended)** — a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

---

## Stand-downs

Findings raised during review and stood down rather than folded, recorded so a later round does not
re-derive them.

- `REJECTED: "the Errno-to-io::Error conversion in the Unix arm loses classification."` Refuted by
  measurement: `impl From<Errno> for std::io::Error` is `Self::from_raw_os_error(err.raw_os_error())` at
  `rustix-1.1.4/src/io/errno.rs:58-63`, so `raw_os_error()` survives and `FsError::from_io` classifies
  normally. The concern was worth having -- `error.rs` records that rebuilding an error as
  `Error::other(..)` destroys it, MEASURED to turn `DiskFull` into `IoError` -- which is why the code
  comment now names the citation.
- `DISCARDED-BELOW-FLOOR: whether windows-sys 0.61 defines PCWSTR as a raw pointer or a newtype, which
  would decide if .as_ptr() needs a cast.` Unreachable as a plan defect: `MoveFileExW`'s declaration at
  `windows-sys-0.61.2/.../FileSystem/mod.rs:273` takes `windows_sys::core::PCWSTR`, and this crate
  already passes raw pointers to `windows-sys` functions in the `#[cfg(windows)]` `identity_of_handle`
  at `crates/flux-platform/src/std_fs.rs:359-388`. If it does not compile, the compiler says so
  immediately and the fix is one `as` -- it cannot reach a commit, because Task 3 runs the gate first.
