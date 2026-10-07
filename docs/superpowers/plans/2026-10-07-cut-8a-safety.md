# Cut 8a: safety before replacement - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Before cut 8b lets a directory copy replace files, close three gaps: refuse a destination with no
no-replace primitive before anything changes (Part P); never merge into a pre-existing destination directory that is
the root of a mount (Part M); refuse a destination whose resolved location lies inside the source before the lock is
taken (Part A).

**Architecture:**
- **`flux-fs`:** two new `DirHandle` queries, `mount_root` (child against its parent: `Yes` / `No` / `Unknown`) and
  `canonical_path` (the path of the directory the handle holds, from the handle). Implemented by the fake (settable
  per directory), the POSIX arm (Linux `statx` / `/proc/self/fd`; macOS device numbers / `F_GETPATH`) and the
  Windows arm (never a mount root; `GetFinalPathNameByHandleW`).
- **The walk (`tree.rs`):** `enter_dir` owns the whole decision for a `Dir` event: the source-root identity check
  (moved in from the walk loop, unchanged in what it checks) FIRST, then, for a pre-existing directory only, the
  mount-root query. "Cannot tell" follows the weak-identity rule: a new aggregated warning under `Safety::Default`,
  a skipped subtree under `Safety::Strict`.
- **The run (`run/place.rs`):** `locate_tree` compares the canonical path of the anchor handle it hands on with the
  canonical path of the source root, component by component, after each branch's pre-flight and before the lock.
- **The run (`run/place.rs`, `state.rs`):** Part P. `create_workspace` is split into `begin_workspace` (make
  `<id>.creating`) and `publish_workspace` (manifest, rename onto `<id>`, flush); `TreePlace::create` probes
  between them: an empty `noreplace-probe.tmp` published onto `noreplace-probe` with `rename_no_replace` and
  removed again. "Primitive unavailable" refuses with `NOREPLACE_PUBLISH_UNAVAILABLE` after removing `.creating`,
  the empty control directories and a DEST this run made (exit 3); a probe file that stays is named (`changed`,
  exit 1, or a warning when the run goes on).
- **The CLI (`flux-cli`):** two new warning lines, rendered beside the weak-identity ones; no new exit rule.

**Tech Stack:** Rust 2024, `rustix` 1.1 (`fs`), `windows-sys` 0.61, the `FaultFs` fake, nextest via `just`.

**Spec:** `docs/superpowers/specs/2026-10-03-cut-8a-safety-design.md` (approved at `eee22c0`, panel GREEN). Normative:
`FLUX_FULL_UPDATED_SPEC_V16.md` section 241.5 (lines 10874-10887), section 129, section 55.

**Worktree:** `~/Development/Rust/flux-engine`, branch `spec/cut-8a`. Every line number below was read on `eee22c0`.

## Global Constraints

- Rust edition 2024; `cargo clippy --workspace --all-targets -- -D warnings` is the gate (`just check` = fmt-check +
  clippy + typos + test). No new dependency and no new feature (spec, "Dependencies").
- Every tree file still publishes with `Publish::NoReplace` (`tree.rs:369`); nothing in this cut replaces anything.
- A `DirHandle` never follows a link (section 149.7). The new queries read the handle, never a path.
- Exit codes (section 55): a refusal with nothing changed is 3; anything changed, or any streamed failure, is 1.
- `SAFETY_REJECTED` and `NOREPLACE_PUBLISH_UNAVAILABLE` are the only codes this cut raises (`flux_fs::Code`, `error.rs:19`, `:28`).
- No in-memory structure proportional to the tree (spec line 997); the new warnings are a count plus one example.
- Real-system tests skip with a stated reason locally and FAIL on CI (`CI` set in the environment) when their facility
  is missing (spec, "Testing").
- Commit messages: `feat:` / `test:` / `docs:` prefixes as the branch uses; one commit per task step that says
  "Commit".

## Decisions this plan makes (each traced to the spec or to measured code)

1. **Part P runs inside `<id>.creating`, before the workspace is published (owner, 2026-10-07, after AGY-FIRST).**
   MEASURED on the fake (`fake()` + `set_no_replace_support(false)` + `run_tree`): as approved, the run would stop
   at `create_workspace`'s own `rename_no_replace` (`state.rs:619`) with `RunError::Failed { step: State, .. }`,
   exit 1, `<id>.creating` left behind, BEFORE the spec's probe placement ("once this run's workspace exists"). On
   the motivating filesystem (WSL 9p, `TODO.md:247-262`, `dir_unix.rs:140-172`: `EINVAL` for a free name) the
   probe as approved was unreachable. agy's consult (`.clavity/seams/cut8a-probe-reachability.md`) confirmed the
   trace and named the placement adopted here; agy itself recommended folding the probe into the workspace publish
   (no `noreplace-probe` file), which the owner declined in favour of keeping section 241.5's letter. Task 6
   carries the design; Task 8 amends the spec's "When it runs" paragraph and the last two rows of its table.
2. **The two queries are `DirHandle` methods without defaults.** A default answering `No` / `Unsupported` would let a
   fourth implementor silently weaken Part M and Part A. The three implementors (`dir_unix.rs:68`,
   `dir_windows.rs:113`, `fault_fs.rs:1056`) are all touched in Tasks 1 and 2.
3. **The platform maps "unsupported" to `ErrorKind::Unsupported`; the engine tests only the kind.** Linux
   `read_link` of `/proc/self/fd/<n>` answering `NotFound` means `/proc` is not mounted (an open descriptor always
   has that entry otherwise); `ENOSYS` and Windows `ERROR_INVALID_FUNCTION` (= 1, `windows-sys` 0.61.2
   `Foundation/mod.rs:2752`) are the other unsupported answers. The engine never inspects raw OS errors for Part A.
4. **The identity re-check of a canonical path (spec, Part A) needs `Strong` on both sides.** The spec says the
   object at the returned path "must have the handle's own identity" and a mismatch "counts as any other error",
   but says nothing about a weak identity. This plan: `Strong` and equal passes; `Strong` on both sides and unequal
   is the mismatch, an abort with `Code::DestinationError` at `CopyStep::Resolve`; anything weaker on either side
   cannot confirm the path and is treated as UNSUPPORTED (warn under default, refuse under strict), the rule this
   cut applies to every check it cannot make with full confidence.
5. **The new warnings are two fields on `TreeOutcome`, not members of `WeakIdentityWarnings`.** The spec puts them
   "beside" the weak-identity warnings. `mount_unknown: Option<DegradedGroup>` (count + first path relative to the
   source root, as `DegradedGroup` already is) and `containment_degraded: Option<PathBuf>` (the path whose query
   failed, as the operator gave it). `locate_tree` therefore takes `out: &mut TreeOutcome` instead of
   `warnings: &mut WeakIdentityWarnings` (`place.rs:74-80`; the one caller is `run/mod.rs:168`).
6. **Part A asks the anchor's canonical path first, then the source root's.** The spec fixes neither order. With
   the fake's one-shot fault (`fail_kind("canonical_path", ..)`) this makes the anchor the one whose query fails in
   the unit tests, and the warning then names DEST (or its parent), the path the operator can act on.
7. **The source root's handle for Part A is `fs.destination_root(src_root)`.** The walk reads the source root by
   path (`walk.rs:150-155`, `fs.metadata(root)` then `fs.read_dir(root)`), which follows a link AT the root; so does
   `destination_root` (`fs.rs:352-355`). The handle is dropped as soon as its path is read.
8. **`enter_dir` gains `src_identity` and owns the source-root check for BOTH arms** (created and pre-existing),
   which is exactly what the walk loop checks today on every live frame (`tree.rs:405-417`): "what it checks is not
   changed; where it runs is". The mount query runs only on the pre-existing arm, after that check.
9. **DEST itself is never queried for a mount root** (known limit 1: `/mnt/usb` is an ordinary target). Only
   directories `enter_dir` opens below it are.
10. **No CI workflow change.** `ubuntu-latest` has passwordless `sudo`; `CI=true` is set by GitHub Actions; the test
    job runs `cargo nextest run --workspace --no-tests=pass --no-fail-fast` on all three runners (`ci.yml:122-141`).
11. **The lock-protocol model is not touched.** Part P's write is one more mutation under the lock, made where the
    manifest write already is; no new state is introduced.
12. **Part A's reachable value is narrower than the spec's "Why" row says, and it is still worth building.** The
    CLI canonicalizes both roots before the engine sees them (`flux-cli/src/resolve.rs:47-48`: `canonicalize` for
    the source, `canonical_with_remainder` for DEST, which resolves every existing ancestor), so a junction or
    symlink in a parent component of DEST is resolved lexically at the CLI and the lexical floor refuses it there.
    What Part A closes: (a) any caller of `flux_core::run::tree` that does not canonicalize (the engine's own
    contract), and (b) the check-then-use window between the CLI's path-based canonicalize and the engine's open
    of DEST's parent, since Part A reads the handle the run then writes through. Task 7's tests therefore drive the
    engine directly, and Task 8 corrects the spec's "Why" row.

## Review Focus

Input classes the spec implies but no task's tests exercise. Each line's test is added to the task named.

1. **A pre-existing destination directory that is a mount root AND is also a directory this run created under
   another source name (a fold).** Expected: the fold is reported `DESTINATION_NAMESPACE_COLLISION` as today; the
   mount query is never reached. Pinned in Task 3 (`a_folded_directory_is_a_collision_before_any_mount_query`).
2. **The mount query itself fails (permission denied on `statx`).** Expected: that subtree fails and is reported
   like a failed `open_dir`, with the query's own code; siblings are copied. Task 3
   (`a_failed_mount_query_fails_that_subtree_like_a_failed_open`).
3. **A canonical path that resolves to an object other than the handle's (the handle's directory was renamed away
   and something else now holds the name).** Expected: an abort with `DESTINATION_ERROR` before the lock, never a
   silent pass. Task 4 (`a_canonical_path_naming_another_object_aborts_before_the_lock`).
4. **DEST is a filesystem root (`locate_tree`'s first branch).** Expected: Part A runs there too. Task 4
   (`a_root_destination_whose_resolved_location_lies_inside_the_source_is_refused`).
5. **Both new warnings in one run.** Expected: both rendered, in a fixed order after the weak-identity lines, and the
   exit stays 0. Task 5 (`both_safety_warnings_render_after_the_identity_ones`).

---

## File structure

| File | Change |
|---|---|
| `crates/flux-fs/src/fs.rs` | `MountRoot` enum; `DirHandle::mount_root`, `DirHandle::canonical_path` |
| `crates/flux-fs/src/lib.rs` | re-export `MountRoot` |
| `crates/flux-platform/src/dir_unix.rs` | both queries, Linux and macOS arms |
| `crates/flux-platform/src/dir_windows.rs` | both queries |
| `crates/flux-platform/tests/dir_handle.rs` | platform tests of both queries |
| `crates/flux-core/src/fault_fs.rs` | per-directory mount-root answer and canonical path; both queries recorded and faultable |
| `crates/flux-core/src/tree.rs` | Part M in `enter_dir`; `TreeOutcome.mount_unknown`, `.containment_degraded`; `containment` (Part A's check); `lexically_within`, `primitive_unavailable` made `pub(crate)` |
| `crates/flux-core/src/state.rs` | `create_workspace` split into `begin_workspace` / `publish_workspace` |
| `crates/flux-core/src/run/place.rs` | Part A in `locate_tree`; Part P: `probe_no_replace`, `TreePlace::create` |
| `crates/flux-core/src/run/session.rs` | `create` call gains `warnings` |
| `crates/flux-core/src/run/mod.rs` | `RunStep::Probe`, `RunWarning::ProbeNotRemoved` |
| `crates/flux-core/src/lock/error.rs` | `LockCode::NoReplacePublishUnavailable` |
| `crates/flux-core/src/run/tests.rs` | Part A and Part P tests on the fake |
| `crates/flux-core/tests/safety_std_fs.rs` (new) | Linux bind mount (M); junction / symlink in a parent component (A) |
| `crates/flux-cli/src/report.rs`, `main.rs` | the two warning lines |
| `TODO.md`, `dir_unix.rs` comment, the spec's design record | the debt entries and known limits |

---

### Task 1: `DirHandle::mount_root` on every implementor

**Files:**
- Modify: `crates/flux-fs/src/fs.rs:344-349` (after `remove_dir`, before `sync`), `crates/flux-fs/src/lib.rs`
- Modify: `crates/flux-platform/src/dir_unix.rs:68-366`
- Modify: `crates/flux-platform/src/dir_windows.rs:113-300`
- Modify: `crates/flux-core/src/fault_fs.rs` (`Inner` at `:19-113`, settings at `:459-640`, `impl DirHandle for FakeDirHandle` at `:1056`)
- Test: `crates/flux-platform/tests/dir_handle.rs`, `crates/flux-core/src/fault_fs.rs` tests

**Interfaces:**
- Produces, in `flux_fs`:
  ```rust
  /// Whether a directory is the root of a mount (cut 8a, Part M), asked of the child with its parent's handle.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum MountRoot { Yes, No, Unknown }
  ```
  and on `DirHandle`:
  ```rust
  /// Whether the directory this handle holds is the root of a mount, judged from this handle and `parent`'s (the
  /// directory it was opened from), never from a path. `Unknown` only where the platform cannot tell (Linux without
  /// `STATX_ATTR_MOUNT_ROOT` and equal device numbers); Windows answers `No`: a mounted-volume folder is a
  /// name-surrogate reparse point, which `open_dir` already refuses. An `Err` is the query's own failure.
  fn mount_root(&self, parent: &Self) -> Result<MountRoot>;
  ```
- Produces, on `FaultFs`: `pub fn set_mount_root(&self, path: impl AsRef<Path>, answer: MountRoot)` (default for
  every directory: `No`); the fake records `mount_root(<child path>)` under key `mount_root`, so `fail("mount_root",
  code)` / `fail_kind` inject a failure.

- [ ] **Step 1: Write the failing fake tests** in `fault_fs.rs`'s test module (beside
  `create_dir_then_read_dir_round_trips_through_the_trait`, `:1518`):

```rust
#[test]
fn mount_root_answers_no_by_default_and_what_a_test_set() {
    let fs = FaultFs::new();
    fs.create_dir(Path::new("/d")).unwrap();
    fs.create_dir(Path::new("/d/m")).unwrap();
    fs.create_dir(Path::new("/d/n")).unwrap();
    fs.set_mount_root("/d/m", MountRoot::Yes);
    let d = fs.destination_root(Path::new("/d")).unwrap();
    let m = d.open_dir(OsStr::new("m")).unwrap();
    let n = d.open_dir(OsStr::new("n")).unwrap();
    assert_eq!(m.mount_root(&d).unwrap(), MountRoot::Yes);
    assert_eq!(n.mount_root(&d).unwrap(), MountRoot::No);
    assert!(fs.calls().iter().any(|c| c.replace('\\', "/") == "mount_root(/d/m)"), "{:?}", fs.calls());
}

#[test]
fn mount_root_takes_an_injected_fault() {
    let fs = FaultFs::new();
    fs.create_dir(Path::new("/d")).unwrap();
    fs.create_dir(Path::new("/d/m")).unwrap();
    fs.fail("mount_root", Code::PermissionDenied);
    let d = fs.destination_root(Path::new("/d")).unwrap();
    let m = d.open_dir(OsStr::new("m")).unwrap();
    assert_eq!(m.mount_root(&d).unwrap_err().code, Code::PermissionDenied);
    assert_eq!(m.mount_root(&d).unwrap(), MountRoot::No, "consumed on use");
}
```

- [ ] **Step 2: Run them to verify they fail to compile** (no `MountRoot`, no `mount_root`):

Run: `cargo nextest run -p flux-core mount_root_`
Expected: a compile error naming `MountRoot` / `mount_root`.

- [ ] **Step 3: Add `MountRoot` and the trait method** in `fs.rs` after `remove_dir` (`:344`), with the doc comment
  above; `pub use fs::MountRoot` in `lib.rs` beside the other `fs::` exports.

- [ ] **Step 4: Implement the fake.** `Inner` gains `mount_roots: HashMap<PathBuf, MountRoot>` (keyed by the
  directory's snapshot path, like `identities`). `set_mount_root` inserts. `FakeDirHandle::mount_root(&self, _parent:
  &Self)`: `let path = self.my_path(); self.fs().record(format!("mount_root({})", path.display()), "mount_root")?;`
  then the map lookup, default `MountRoot::No`.

- [ ] **Step 5: Implement the POSIX arm** in `dir_unix.rs`:
  - Linux (`#[cfg(target_os = "linux")]`): `rustix::fs::statx(&self.0, "", AtFlags::EMPTY_PATH, StatxFlags::empty())`.
    `Ok(st)`: if `st.stx_attributes_mask.contains(StatxAttributes::MOUNT_ROOT)` answer `Yes` / `No` from
    `st.stx_attributes.contains(StatxAttributes::MOUNT_ROOT)`; else fall through to the device comparison with
    `Unknown` when equal. `Err(Errno::NOSYS)`: the device comparison. Any other `Err`: `FsError::from_io`.
  - The device comparison, shared: `fstat` both handles; `child.st_dev != parent.st_dev` is `Yes`; equal is
    `Unknown` on Linux and `No` on macOS (`#[cfg(target_os = "macos")]`: the spec, "macOS has no same-filesystem bind
    mount of its own").
  - `st_dev` is `i32` on macOS and `u64` on Linux: compare them as the same type with the `#[allow(clippy::unnecessary_cast)]` pattern `metadata_from_stat` uses (`std_fs.rs:690-700`).
  - The rustix symbols exist in the locked 1.1.5: `statx` (`src/fs/statx.rs:205`), `StatxAttributes::MOUNT_ROOT`
    (`:151`), `StatxFlags` (`:73`).

- [ ] **Step 6: Implement the Windows arm** in `dir_windows.rs`: `fn mount_root(&self, _parent: &Self) ->
  Result<MountRoot> { Ok(MountRoot::No) }` with the spec's reason as its doc comment (a mounted-volume folder is a
  name-surrogate reparse point, refused by `open_dir`, `dir_windows.rs:180-197`).

- [ ] **Step 7: Write the platform tests** in `crates/flux-platform/tests/dir_handle.rs`:

In `mod posix` (`:1-455`):
```rust
#[test]
fn a_subdirectory_is_not_a_mount_root_and_the_dev_mount_is() {
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("sub")).unwrap();
    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let sub = root.open_dir(OsStr::new("sub")).unwrap();
    assert_eq!(sub.mount_root(&root).unwrap(), flux_fs::MountRoot::No);
    // `/dev` is its own mount on Linux (devtmpfs) and on macOS (devfs): a different device from `/`.
    let slash = StdFileSystem.destination_root(std::path::Path::new("/")).unwrap();
    let dev = slash.open_dir(OsStr::new("dev")).unwrap();
    assert_eq!(dev.mount_root(&slash).unwrap(), flux_fs::MountRoot::Yes);
}
```
In `mod windows_arm` (`:456`):
```rust
#[test]
fn mount_root_is_never_reported_on_windows() {
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("sub")).unwrap();
    let root = StdFileSystem.destination_root(d.path()).unwrap();
    let sub = root.open_dir(OsStr::new("sub")).unwrap();
    assert_eq!(sub.mount_root(&root).unwrap(), flux_fs::MountRoot::No);
}
```

- [ ] **Step 8: Run the gate**

Run: `just check`
Expected: fmt, clippy, typos clean; every test passes (`cargo nextest` summary line ends `0 failed`).

- [ ] **Step 9: Commit**

```bash
git add crates/flux-fs crates/flux-platform crates/flux-core/src/fault_fs.rs
git commit -m "feat: DirHandle::mount_root on the fake, POSIX and Windows (cut 8a, Part M's query)"
```

---

### Task 2: `DirHandle::canonical_path` on every implementor

**Files:**
- Modify: `crates/flux-fs/src/fs.rs` (beside `mount_root`)
- Modify: `crates/flux-platform/src/dir_unix.rs`, `crates/flux-platform/src/dir_windows.rs`
- Modify: `crates/flux-core/src/fault_fs.rs`
- Test: `crates/flux-platform/tests/dir_handle.rs`, `crates/flux-core/src/fault_fs.rs` tests

**Interfaces:**
- Produces, on `DirHandle`:
  ```rust
  /// The path of the directory this handle holds, obtained FROM the handle (never by resolving a path again):
  /// Linux `readlink` of `/proc/self/fd/<n>`; macOS `fcntl(F_GETPATH)`; Windows `GetFinalPathNameByHandleW`
  /// (`FILE_NAME_NORMALIZED | VOLUME_NAME_DOS`, so it carries the `\\?\` prefix). The text is reported as the
  /// platform gives it; the caller checks the object at it against this handle's identity before trusting it.
  /// Where the system or filesystem cannot answer (`/proc` not mounted, `ENOSYS`, `ERROR_INVALID_FUNCTION`) the
  /// error's kind is `ErrorKind::Unsupported`; every other failure keeps its own kind.
  fn canonical_path(&self) -> Result<PathBuf>;
  ```
- Produces, on `FaultFs`: `pub fn set_canonical_path(&self, path: impl AsRef<Path>, canonical: impl AsRef<Path>)`
  (default: the directory's own snapshot path); recorded as `canonical_path(<path>)` under key `canonical_path`.

- [ ] **Step 1: Write the failing fake tests** in `fault_fs.rs`'s test module:

```rust
#[test]
fn canonical_path_is_the_handles_own_path_unless_a_test_set_one() {
    let fs = FaultFs::new();
    fs.create_dir(Path::new("/d")).unwrap();
    fs.create_dir(Path::new("/d/x")).unwrap();
    fs.set_canonical_path("/d/x", "/elsewhere/x");
    let d = fs.destination_root(Path::new("/d")).unwrap();
    let x = d.open_dir(OsStr::new("x")).unwrap();
    assert_eq!(d.canonical_path().unwrap(), PathBuf::from("/d"));
    assert_eq!(x.canonical_path().unwrap(), PathBuf::from("/elsewhere/x"));
}

#[test]
fn canonical_path_takes_an_injected_fault_with_its_kind() {
    let fs = FaultFs::new();
    fs.create_dir(Path::new("/d")).unwrap();
    fs.fail_kind("canonical_path", Code::IoError, std::io::ErrorKind::Unsupported);
    let d = fs.destination_root(Path::new("/d")).unwrap();
    let e = d.canonical_path().unwrap_err();
    assert_eq!(e.source.kind(), std::io::ErrorKind::Unsupported);
    assert_eq!(d.canonical_path().unwrap(), PathBuf::from("/d"), "consumed on use");
}
```

- [ ] **Step 2: Run them to verify they fail to compile**

Run: `cargo nextest run -p flux-core canonical_path_`
Expected: a compile error naming `canonical_path`.

- [ ] **Step 3: Add the trait method** in `fs.rs` with the doc comment above.

- [ ] **Step 4: Implement the fake.** `Inner` gains `canonical: HashMap<PathBuf, PathBuf>`; `set_canonical_path`
  inserts; `FakeDirHandle::canonical_path`: record `canonical_path(<my_path>)` under key `canonical_path`, then the
  map lookup, default `self.my_path()`.

- [ ] **Step 5: Implement the POSIX arm.**
  - Linux: `std::fs::read_link(format!("/proc/self/fd/{}", self.0.as_raw_fd()))`; map an error of kind `NotFound`
    to `FsError::new(Code::IoError, io::Error::new(ErrorKind::Unsupported, "/proc is not mounted: <original>"))`,
    `raw_os_error() == Some(38)` (`ENOSYS`) likewise; the rest `FsError::from_io`. The returned text is used as is
    (never inspected for a `" (deleted)"` suffix: the spec's identity re-check is what catches a vanished
    directory).
  - macOS: `rustix::fs::getpath(&self.0)` (`rustix-1.1.5/src/fs/getpath.rs:12`, exported at `fs/mod.rs:100`) gives
    a `CString`; `PathBuf::from(OsString::from_vec(c.into_bytes()))` (`std::os::unix::ffi::OsStringExt`). Map
    `Errno::NOSYS | Errno::NOTSUP | Errno::OPNOTSUPP` to kind `Unsupported`; the rest `from_io`.

- [ ] **Step 6: Implement the Windows arm.** `GetFinalPathNameByHandleW(self.0.as_raw_handle() as HANDLE,
  buf.as_mut_ptr(), buf.len() as u32, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS)` (both flags are `0`, `windows-sys`
  0.61.2 `Storage/FileSystem/mod.rs:1663`, `:4139`). Start with a 512-`u16` buffer; a return value `>= buf.len()`
  is the needed length (including the terminator): grow to it and call again; `0` is a failure, read with
  `std::io::Error::last_os_error()`, and `raw_os_error() == Some(1)` (`ERROR_INVALID_FUNCTION`) maps to kind
  `Unsupported`. The result is `OsString::from_wide(&buf[..n])` (`std::os::windows::ffi::OsStringExt`) into a
  `PathBuf`. Add `GetFinalPathNameByHandleW`, `FILE_NAME_NORMALIZED` and `VOLUME_NAME_DOS` to the existing
  `windows_sys::Win32::Storage::FileSystem` import list (`dir_windows.rs:29-31`).

- [ ] **Step 7: Write the platform tests** in `tests/dir_handle.rs`:

In `mod posix`:
```rust
#[test]
fn canonical_path_names_the_directory_the_handle_holds_even_through_a_link() {
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("real")).unwrap();
    std::os::unix::fs::symlink(d.path().join("real"), d.path().join("alias")).unwrap();
    // `destination_root` follows a link AT the root (section 149.7 exempts it); the handle holds `real`.
    let through = StdFileSystem.destination_root(&d.path().join("alias")).unwrap();
    let direct = StdFileSystem.destination_root(&d.path().join("real")).unwrap();
    let expected = std::fs::canonicalize(d.path().join("real")).unwrap();
    assert_eq!(through.canonical_path().unwrap(), expected);
    assert_eq!(direct.canonical_path().unwrap(), expected);
}
```
In `mod windows_arm` (uses the file's `junction` helper, `:463-476`):
```rust
#[test]
fn canonical_path_names_the_junctions_target_in_the_same_form_as_canonicalize() {
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("real")).unwrap();
    if !junction(&d.path().join("real"), &d.path().join("junc")) {
        panic!("could not create a junction; mklink /J requires no privilege");
    }
    let through = StdFileSystem.destination_root(&d.path().join("junc")).unwrap();
    let direct = StdFileSystem.destination_root(&d.path().join("real")).unwrap();
    let expected = std::fs::canonicalize(d.path().join("real")).unwrap();
    assert_eq!(through.canonical_path().unwrap(), expected);
    assert_eq!(direct.canonical_path().unwrap(), expected);
    assert!(expected.to_string_lossy().starts_with(r"\\?\"), "{expected:?}");
}
```

- [ ] **Step 8: Run the gate**

Run: `just check`
Expected: clean; `0 failed`.

- [ ] **Step 9: Commit**

```bash
git add crates/flux-fs crates/flux-platform crates/flux-core/src/fault_fs.rs
git commit -m "feat: DirHandle::canonical_path from the open handle on every platform (cut 8a, Part A's query)"
```

---

### Task 3: Part M in `enter_dir`

**Files:**
- Modify: `crates/flux-core/src/tree.rs:24-41` (`TreeOutcome`), `:154-159` (`DegradedGroup`), `:359-470`
  (`walk_into`), `:476-540` (`enter_dir`)
- Test: `crates/flux-core/src/tree.rs` test module (`:670` onward; helpers `tree()` `:758`, `identity_of` `:768`,
  `run` `:732`, `strict()` `:726`)

**Interfaces:**
- Consumes: `MountRoot`, `DirHandle::mount_root` (Task 1).
- Produces:
  ```rust
  // on TreeOutcome
  /// Part M: pre-existing destination directories whose mount-root query could not tell, merged into under
  /// `Safety::Default`. The example is relative to the source root, as `warnings` are.
  pub mount_unknown: Option<DegradedGroup>,

  impl DegradedGroup {
      /// Count one more degraded check into `slot`, keeping the first example.
      pub(crate) fn note(slot: &mut Option<DegradedGroup>, example: &Path)
  }

  fn enter_dir<D: DirHandle>(
      stack: &mut [Frame<D>],
      path: &Path,
      identity: FileIdentity,
      root_identity: FileIdentity,
      src_identity: FileIdentity,
      opts: &CopyOptions,
      out: &mut TreeOutcome,
      on_report: &mut dyn FnMut(TreeFailure),
  ) -> std::result::Result<Frame<D>, CopyError>
  ```
  Refusal texts (the CLI prints `CODE: <path>: <source>`):
  - mount root: `"a pre-existing destination directory is the root of a mount, which a copy never merges into"`
  - cannot tell, strict: `"whether a pre-existing destination directory is a mount root cannot be told with full confidence"`
  - source root (moved, unchanged text): `"a destination directory is the source root itself, by identity"`

- [ ] **Step 1: Write the failing tests** in `tree.rs`'s test module, after
  `a_destination_directory_that_is_the_source_root_aborts_the_operation` (`:1191`):

```rust
#[test]
fn a_pre_existing_mount_root_is_skipped_and_reported_and_its_siblings_are_copied() {
    let fs = tree();
    fs.write_file("/src/c", b"C");
    fs.create_dir(Path::new("/dst")).unwrap();
    fs.create_dir(Path::new("/dst/sub")).unwrap();
    fs.set_mount_root("/dst/sub", flux_fs::MountRoot::Yes);

    let (r, got) = run(&fs, "/src", "/dst", &opts());

    let out = r.unwrap();
    assert_eq!((out.files_copied, out.failures.create_dir), (2, 1), "{got:?}");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].path, PathBuf::from("sub"));
    let TreeFailureCause::CreateDir(e) = &got[0].cause else { panic!("{:?}", got[0].cause) };
    assert_eq!(e.code, Code::SafetyRejected);
    assert!(fs.exists("/dst/a") && fs.exists("/dst/c"));
    assert!(!fs.exists("/dst/sub/b"), "nothing was written into the mount");
}

#[test]
fn a_directory_this_run_created_and_dest_itself_are_never_queried() {
    let fs = tree();
    fs.create_dir(Path::new("/dst")).unwrap();
    // DEST may be a mount root (`/mnt/usb`): known limit 1, never checked. `sub` is created by this run.
    fs.set_mount_root("/dst", flux_fs::MountRoot::Yes);
    let (r, got) = run(&fs, "/src", "/dst", &opts());
    assert!(r.is_ok() && got.is_empty(), "{got:?}");
    assert!(!fs.called("mount_root("), "{:?}", fs.calls());
}

#[test]
fn a_mount_query_that_cannot_tell_warns_under_default_and_refuses_the_subtree_under_strict() {
    let lax = tree();
    lax.create_dir(Path::new("/dst")).unwrap();
    lax.create_dir(Path::new("/dst/sub")).unwrap();
    lax.set_mount_root("/dst/sub", flux_fs::MountRoot::Unknown);
    let (r, got) = run(&lax, "/src", "/dst", &opts());
    let out = r.unwrap();
    assert!(got.is_empty(), "{got:?}");
    assert_eq!(out.mount_unknown, Some(DegradedGroup { count: 1, example: PathBuf::from("sub") }));
    assert!(lax.exists("/dst/sub/b"), "merged anyway");

    let tight = tree();
    tight.create_dir(Path::new("/dst")).unwrap();
    tight.create_dir(Path::new("/dst/sub")).unwrap();
    tight.set_mount_root("/dst/sub", flux_fs::MountRoot::Unknown);
    let (r, got) = run(&tight, "/src", "/dst", &strict());
    let out = r.unwrap();
    assert_eq!(out.failures.create_dir, 1, "{got:?}");
    assert!(out.mount_unknown.is_none());
    assert!(!tight.exists("/dst/sub/b"));
}

#[test]
fn the_source_root_check_runs_before_the_mount_query() {
    // A mount inside DEST presenting the SOURCE ROOT: the whole operation aborts (section 129), it does not
    // become a skipped subtree, and the mount query is never asked.
    let fs = tree();
    fs.create_dir(Path::new("/dst")).unwrap();
    fs.create_dir(Path::new("/dst/sub")).unwrap();
    fs.set_identity("/dst/sub", identity_of(&fs, "/src"));
    fs.set_mount_root("/dst/sub", flux_fs::MountRoot::Yes);

    let (r, got) = run(&fs, "/src", "/dst", &opts());

    assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
    assert!(got.is_empty(), "an abort, not a streamed failure: {got:?}");
    assert!(!fs.called("mount_root("), "{:?}", fs.calls());
}

#[test]
fn a_failed_mount_query_fails_that_subtree_like_a_failed_open() {
    let fs = tree();
    fs.create_dir(Path::new("/dst")).unwrap();
    fs.create_dir(Path::new("/dst/sub")).unwrap();
    fs.fail("mount_root", Code::PermissionDenied);
    let (r, got) = run(&fs, "/src", "/dst", &opts());
    let out = r.unwrap();
    assert_eq!((out.files_copied, out.failures.create_dir), (1, 1));
    let TreeFailureCause::CreateDir(e) = &got[0].cause else { panic!("{:?}", got[0].cause) };
    assert_eq!(e.code, Code::PermissionDenied);
    assert!(!fs.exists("/dst/sub/b"));
}

#[test]
fn a_folded_directory_is_a_collision_before_any_mount_query() {
    // Two source names folding onto one destination directory (Review Focus 1), staged exactly as
    // `two_source_directories_folded_into_one_destination_are_a_collision` (`:948`) stages it: `/dst/a` pre-exists
    // carrying the identity `/dst/A` gets when this run makes it. The fold is reported as today, and the mount
    // query is never reached for it even though `/dst/a` is marked a mount root.
    let fs = FaultFs::new();
    for d in ["/src", "/src/A", "/src/a", "/dst", "/dst/a"] {
        fs.create_dir(Path::new(d)).unwrap();
    }
    fs.write_file("/src/A/x", b"x");
    fs.write_file("/src/a/y", b"y");
    let folded = FileIdentity::Strong(ObjectId { volume: 1, index: 9_003 });
    fs.set_identity("/dst/A", folded);
    fs.set_identity("/dst/a", folded);
    fs.set_mount_root("/dst/a", flux_fs::MountRoot::Yes);
    let (r, got) = run(&fs, "/src", "/dst", &opts());
    let out = r.unwrap();
    assert_eq!(out.failures.create_dir, 1, "{got:?}");
    let TreeFailureCause::CreateDir(e) = &got[0].cause else { panic!("{:?}", got[0].cause) };
    assert_eq!(e.code, Code::DestinationNamespaceCollision);
    assert!(!fs.called("mount_root("), "{:?}", fs.calls());
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core tree::tests`
Expected: the six new tests fail (`set_mount_root` compiles after Task 1; `mount_unknown` does not exist; the mount
answers are ignored).

- [ ] **Step 3: Add `mount_unknown` to `TreeOutcome`** and `DegradedGroup::note`.

- [ ] **Step 4: Restructure `enter_dir`** (`tree.rs:476-540`) and delete the walk loop's check (`:405-417`), passing
  `cx.src_identity` from the `Dir` arm (`:402`). The body, in order:
  1. The dynamic section-129 check against `root_identity`, unchanged (`:485-497`).
  2. Decision 8: `create_dir`, and on `AlreadyExists` `open_dir` and the fold check, unchanged in what they do
     (`:503-532`); but instead of returning a `Frame` inside the match, bind `(child, pre_existing: bool)`; a
     reported failure still returns `Ok(Frame::Skipped)` as today.
  3. The source-root check, on both arms, on ONE `child.identity()` read per directory (the created arm already
     reads it for `created`; keep that value): `if let (Ok(FileIdentity::Strong(a)), FileIdentity::Strong(b)) =
     (child_identity, src_identity) && a == b { return Err(refuse("a destination directory is the source root itself, by identity")) }`.
  4. Only if `pre_existing`: `match child.mount_root(parent)`: `Ok(MountRoot::Yes)` reports
     `TreeFailureCause::CreateDir(FsError::new(Code::SafetyRejected, io::Error::other(<mount text>)))` and returns
     `Skipped`; `Ok(MountRoot::No)` continues; `Ok(MountRoot::Unknown)` under `Safety::Default` calls
     `DegradedGroup::note(&mut out.mount_unknown, path)` and continues, under `Safety::Strict` reports `CreateDir`
     with `Code::SafetyRejected` and the strict text and returns `Skipped`; `Err(e)` reports `CreateDir(e)` and
     returns `Skipped`.
  5. `Ok(Frame::Live { dir: child, created: HashSet::new() })`.
  The borrow of `parent` and `created` from `stack.last_mut()` must end before step 3 reads `child`; take the
  mount answer while `parent` is still borrowed, or re-borrow `stack.last()` for it.

- [ ] **Step 5: Run the tree tests**

Run: `cargo nextest run -p flux-core tree::tests`
Expected: all pass, including `a_destination_directory_that_is_the_source_root_aborts_the_operation` (`:1191`),
which pins that the moved check still aborts.

- [ ] **Step 6: Run the gate**

Run: `just check`
Expected: clean; `0 failed`.

- [ ] **Step 7: Commit**

```bash
git add crates/flux-core/src/tree.rs
git commit -m "feat: never merge into a pre-existing destination mount root (cut 8a, Part M)"
```

---

### Task 4: Part A in `locate_tree`

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`TreeOutcome`; new `containment` beside `preflight` at `:616`;
  `lexically_within` `:644` made `pub(crate)`)
- Modify: `crates/flux-core/src/run/place.rs:74-125` (`locate_tree`), `crates/flux-core/src/run/mod.rs:168`
  (the call)
- Test: `crates/flux-core/src/run/tests.rs` (helpers: `fake()` `:54`, `run_tree` `:64`, `aborted` `:584`,
  `LOCK` `:18`)

**Interfaces:**
- Consumes: `DirHandle::canonical_path` (Task 2), `DirHandle::identity`, `FileSystem::metadata`.
- Produces, in `tree.rs`:
  ```rust
  // on TreeOutcome
  /// Part A: the canonical-path query was unsupported, so containment was checked lexically only. The path
  /// whose query failed, as the operator gave it (DEST, its parent, or the source root).
  pub containment_degraded: Option<PathBuf>,

  /// Part A: the resolved destination anchor must not be the source root or lie inside it. `anchor` is the handle
  /// the run then writes through; `anchor_shown` names it in the warning. Runs after the identity pre-flight.
  pub(crate) fn containment<F: DestinationRoot>(
      fs: &F,
      src_root: &Path,
      anchor: &F::Dir,
      anchor_shown: &Path,
      safety: Safety,
      out: &mut TreeOutcome,
  ) -> std::result::Result<(), CopyError>

  /// The canonical path of `dir`, identity-checked (decision 4): `Ok(None)` when the query is unsupported or the
  /// identities cannot confirm it; `Err` for any other failure, including a path that holds another object.
  fn canonical_of<F: DestinationRoot>(fs: &F, dir: &F::Dir) -> std::result::Result<Option<PathBuf>, FsError>
  ```
  and `locate_tree`'s new signature:
  ```rust
  pub(crate) fn locate_tree<F: DestinationRoot>(
      fs: &F,
      src_root: &Path,
      dst_root: &Path,
      src_identity: FileIdentity,
      safety: Safety,
      out: &mut TreeOutcome,
  ) -> Result<LocatedTree<F::Dir>, CopyError>
  ```
  Refusal text: `"the destination's resolved location is the source or lies inside it"` (`Code::SafetyRejected`,
  `CopyStep::Resolve`, via `refuse`). Strict-mode refusal when degraded: `"the destination's location cannot be
  resolved from its handle, so containment cannot be checked with full confidence"`. Mismatch error:
  `FsError::new(Code::DestinationError, io::Error::other("the path reported for an open directory now holds a different object"))`.

- [ ] **Step 1: Write the failing tests** in `run/tests.rs`, after `a_file_at_dest_is_a_destination_error_before_anything_is_made` (`:611`):

```rust
/// `run_tree` with the destination chosen by the test.
fn run_tree_to(fs: &FaultFs, dst: &str) -> (Run<Result<TreeOutcome, TreeAbort>>, Vec<TreeFailure>) {
    let mut got = Vec::new();
    let r = tree(fs, Path::new("/src"), Path::new(dst), &opts(), &cfg(), &mut |f| got.push(f));
    (r, got)
}

#[test]
fn a_destination_whose_resolved_anchor_lies_inside_the_source_is_refused_before_the_lock() {
    // DEST is absent, so the anchor is its parent `/p`, which (through a link in a parent component) IS `/src/sub`:
    // its canonical path says so, and the object at that path has the handle's identity.
    let fs = fake();
    fs.set_canonical_path("/p", "/src/sub");
    fs.set_identity("/src/sub", fs.metadata(Path::new("/p")).unwrap().identity);
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.stop.is_none(), "{:?}", r.stop);
    let a = aborted(&r);
    assert_eq!((a.error.code(), a.error.step), (Code::SafetyRejected, CopyStep::Resolve));
    assert!(a.refused_unchanged());
    assert!(a.error.to_string().contains("resolved location"), "{}", a.error);
    assert!(!fs.called("create_lock"), "{:?}", calls(&fs));
    assert!(!fs.exists("/p/dest"));
}

#[test]
fn an_existing_destination_whose_resolved_path_lies_inside_the_source_is_refused() {
    let fs = fake();
    fs.create_dir(Path::new("/p/dest")).unwrap();
    fs.set_canonical_path("/p/dest", "/src/sub");
    fs.set_identity("/src/sub", fs.metadata(Path::new("/p/dest")).unwrap().identity);
    let (r, _) = run_tree(&fs, &cfg());
    assert_eq!(aborted(&r).error.code(), Code::SafetyRejected);
    assert!(!fs.called("create_lock") && !fs.exists("/p/dest/.flux"));
}

#[test]
fn a_root_destination_whose_resolved_location_lies_inside_the_source_is_refused() {
    // `locate_tree`'s filesystem-root branch (Review Focus 4).
    let fs = fake();
    fs.set_canonical_path("/", "/src/sub");
    fs.set_identity("/src/sub", fs.destination_root(Path::new("/")).unwrap().identity().unwrap());
    let (r, _) = run_tree_to(&fs, "/");
    assert_eq!(aborted(&r).error.code(), Code::SafetyRejected);
    assert!(!fs.called("create_lock"));
}

#[test]
fn a_merely_similar_resolved_name_is_not_inside_the_source() {
    let fs = fake();
    fs.create_dir(Path::new("/srcs")).unwrap();
    fs.set_canonical_path("/p", "/srcs");
    fs.set_identity("/srcs", fs.metadata(Path::new("/p")).unwrap().identity);
    let (r, _) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(out.files_copied, 2);
    assert!(out.containment_degraded.is_none());
}

#[test]
fn an_unsupported_canonical_path_query_warns_under_default_and_refuses_under_strict() {
    let lax = fake();
    lax.fail_kind("canonical_path", Code::IoError, std::io::ErrorKind::Unsupported);
    let (r, _) = run_tree(&lax, &cfg());
    let out = ok(&r);
    assert_eq!(out.containment_degraded.as_deref(), Some(Path::new("/p")), "the anchor's query failed");

    let tight = fake();
    tight.fail_kind("canonical_path", Code::IoError, std::io::ErrorKind::Unsupported);
    let mut got = Vec::new();
    let strict = CopyOptions { safety: Safety::Strict, ..opts() };
    let r = tree(&tight, Path::new("/src"), Path::new("/p/dest"), &strict, &cfg(), &mut |f| got.push(f));
    assert_eq!(aborted(&r).error.code(), Code::SafetyRejected);
    assert!(!tight.called("create_lock") && !tight.exists("/p/dest"));
}

#[test]
fn another_canonical_path_error_aborts_before_the_lock() {
    let fs = fake();
    fs.fail_kind("canonical_path", Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let a = aborted(&r);
    assert_eq!((a.error.code(), a.error.step), (Code::PermissionDenied, CopyStep::Resolve));
    assert!(!fs.called("create_lock"));
}

#[test]
fn a_canonical_path_naming_another_object_aborts_before_the_lock() {
    // The handle's directory was renamed away and another directory holds the reported name (Review Focus 3).
    let fs = fake();
    fs.set_canonical_path("/p", "/src/sub"); // `/src/sub` keeps its own identity: not `/p`'s
    let (r, _) = run_tree(&fs, &cfg());
    let a = aborted(&r);
    assert_eq!((a.error.code(), a.error.step), (Code::DestinationError, CopyStep::Resolve));
    assert!(!fs.called("create_lock"));
}

#[test]
fn a_weak_identity_at_the_resolved_path_is_treated_as_unsupported() {
    let fs = fake();
    fs.set_canonical_path("/p", "/src/sub");
    fs.set_identity("/src/sub", FileIdentity::Weak(ObjectId { volume: 1, index: 77 }));
    let (r, _) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!(out.containment_degraded.as_deref(), Some(Path::new("/p")));
}
```
  (`ObjectId` and `FileIdentity` come from `flux_fs`; add them to the file's `use flux_fs::{..}` at `:10-12`.)

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core run::tests`
Expected: compile error on `containment_degraded`; after adding the field as a stub, the new tests fail on their
assertions.

- [ ] **Step 3: Implement `canonical_of` and `containment` in `tree.rs`** beside `preflight`; make `lexically_within`
  `pub(crate)`. `containment`: `canonical_of(fs, anchor)` first; on `Ok(None)` apply the degraded rule with
  `anchor_shown`; then `let src = fs.destination_root(src_root)` and `canonical_of(fs, &src)`, degraded rule with
  `src_root`; the handle is dropped there; `if lexically_within(&dest, &src) { return Err(refuse(..)) }`. The
  degraded rule: `Safety::Default` sets `out.containment_degraded = Some(shown.to_path_buf())` (first one wins) and
  returns `Ok(())`; `Safety::Strict` returns `Err(refuse(<strict text>))`. Every `Err` from `canonical_of` and from
  `destination_root` is `CopyError::at(CopyStep::Resolve, e)`.
  `canonical_of`: `dir.canonical_path()`: `Err(e) if e.source.kind() == ErrorKind::Unsupported` is `Ok(None)`; other
  `Err` is `Err(e)`; `Ok(path)`: `fs.metadata(&path)?.identity` against `dir.identity()?`: both `Strong` and equal
  is `Ok(Some(path))`, both `Strong` and unequal is the mismatch `Err`, anything else `Ok(None)`.

- [ ] **Step 4: Change `locate_tree`** (`place.rs:74-125`): the signature above; both `preflight` calls pass
  `&mut out.warnings`; on the root branch, after `preflight`, `containment(fs, src_root, &holder, dst_root, safety, out)?`;
  on the named branch, after `preflight`, `containment(fs, src_root, anchor_handle, anchor_shown, safety, out)?`
  where `anchor_handle` is `dest` if `Some`, else `holder`, and `anchor_shown` is `dst_root` or `parent_path`
  accordingly. Update the caller (`run/mod.rs:168`): `locate_tree(fs, src_root, dst_root, source.identity, opts.safety, &mut out)`.

- [ ] **Step 5: Run the run tests**

Run: `cargo nextest run -p flux-core run::tests`
Expected: all pass.

- [ ] **Step 6: Run the gate**

Run: `just check`
Expected: clean; `0 failed`.

- [ ] **Step 7: Commit**

```bash
git add crates/flux-core/src/tree.rs crates/flux-core/src/run
git commit -m "feat: refuse a destination whose resolved location lies inside the source, before the lock (cut 8a, Part A)"
```

---

### Task 5: the CLI renders the two new warnings

**Files:**
- Modify: `crates/flux-cli/src/report.rs:200-232` (beside `warning_lines`), `crates/flux-cli/src/main.rs:205-207`
- Test: `crates/flux-cli/src/report.rs` tests (`:474-490` is the model)

**Interfaces:**
- Consumes: `TreeOutcome.mount_unknown`, `TreeOutcome.containment_degraded` (Tasks 3, 4).
- Produces:
  ```rust
  /// Cut 8a's two safety warnings, after the identity ones: the mount-root checks that could not tell, then the
  /// containment check that fell back to the lexical floor. Empty when neither degraded.
  pub fn safety_warning_lines(out: &TreeOutcome) -> Vec<String>
  ```
  Exact copy:
  - `format!("warning: could not tell whether a pre-existing destination directory is a mount root ({count} checks, e.g. {example}); merged anyway - --safety=strict refuses instead")` with `example` through `rel`.
  - `format!("warning: the location of {path} could not be resolved from its handle; containment was checked lexically only - --safety=strict refuses instead")` with `path.display()`.

- [ ] **Step 1: Write the failing tests** in `report.rs`'s test module after `one_warning_per_weak_volume_and_one_for_unavailable` (`:474`):

```rust
#[test]
fn both_safety_warnings_render_after_the_identity_ones() {
    let mut out = TreeOutcome::default();
    out.mount_unknown = Some(DegradedGroup { count: 2, example: PathBuf::from("sub") });
    out.containment_degraded = Some(PathBuf::from("/p"));
    let v = safety_warning_lines(&out);
    assert_eq!(v.len(), 2);
    for needle in ["mount root", "2 checks", "e.g. sub", "--safety=strict"] {
        assert!(v[0].contains(needle), "{needle}: {}", v[0]);
    }
    for needle in ["/p", "lexically", "--safety=strict"] {
        assert!(v[1].contains(needle), "{needle}: {}", v[1]);
    }
    assert!(safety_warning_lines(&TreeOutcome::default()).is_empty());
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p flux-cli both_safety_warnings`
Expected: compile error naming `safety_warning_lines`.

- [ ] **Step 3: Implement `safety_warning_lines`** and, in `main.rs` after the `warning_lines` loop (`:205-207`),
  `for line in report::safety_warning_lines(outcome) { err(&line); }`.

- [ ] **Step 4: Run the gate**

Run: `just check`
Expected: clean; `0 failed`.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-cli
git commit -m "feat: render the mount-root and containment warnings (cut 8a)"
```

---

### Task 6: Part P, the no-replace probe inside the unpublished workspace

The owner's decision (2026-10-07, after the AGY-FIRST consult in `.clavity/seams/cut8a-probe-reachability.md`):
the probe runs INSIDE `<id>.creating`, after that directory is created and BEFORE its manifest is written and it
is published onto `operations/<id>`. It is therefore the run's first no-replace rename; on a filesystem without
the primitive it refuses with nothing published, and the clean-up is `remove_file` + `remove_dir`, never a rename.
Section 241.5's letter holds (the fixed name `noreplace-probe`, inside the operation's workspace, removed again).
What changes against the 8a spec's "When it runs" paragraph: the probe runs where the manifest write runs (the
lock just obtained, the record not yet written), not after the record; Task 8 amends the spec.

**Files:**
- Modify: `crates/flux-core/src/state.rs:604-620` (`create_workspace` split in two)
- Modify: `crates/flux-core/src/run/place.rs:27-61` (`Place::create` gains `warnings`), `:184-209`
  (`TreePlace::create`), `:355-357` (`FilePlace::create`)
- Modify: `crates/flux-core/src/run/session.rs:148` (the one `create` call)
- Modify: `crates/flux-core/src/run/mod.rs:101-126` (`RunStep::Probe`), `:131-146` (`RunWarning::ProbeNotRemoved`)
- Modify: `crates/flux-core/src/lock/error.rs:8-35` (`LockCode::NoReplacePublishUnavailable`)
- Modify: `crates/flux-core/src/tree.rs:609` (`primitive_unavailable` made `pub(crate)`)
- Modify: `crates/flux-cli/src/report.rs:275-300` (`run_warning_line`), its test at `:580-596`
- Test: `crates/flux-core/src/run/tests.rs`, `crates/flux-core/src/state.rs` tests, `crates/flux-core/src/lock/error.rs` tests

**Interfaces:**
- Produces, in `state.rs`:
  ```rust
  /// `<id>.creating`.
  pub fn creating_name(id: &str) -> OsString
  /// Step 1 of `create_workspace`: `<id>.creating`, empty, as a handle.
  pub fn begin_workspace<D: DirHandle>(operations: &D, id: &str) -> flux_fs::Result<D>
  /// Steps 2-3: the manifest written into `building` crash-safely, `building` closed, `<id>.creating` renamed onto
  /// `<id>` without replacing, `operations/` flushed; returns the published workspace's handle.
  pub fn publish_workspace<D: DirHandle>(operations: &D, building: D, state: &OperationState) -> flux_fs::Result<D>
  /// `begin_workspace` then `publish_workspace` (the tests' and the single-call form).
  pub fn create_workspace<D: DirHandle>(operations: &D, state: &OperationState) -> flux_fs::Result<D>
  ```
- Produces, in `place.rs`:
  ```rust
  /// Section 241.5's fixed name, and its staging temporary.
  pub(crate) const PROBE: &str = "noreplace-probe";
  pub(crate) const PROBE_TEMP: &str = "noreplace-probe.tmp";

  /// What the probe found. A `leftover` is a file of the probe's that could not be removed again, by name.
  pub(crate) enum Probe {
      /// The primitive is there; `leftover` is `noreplace-probe` (a warning, it goes with the workspace).
      Available { leftover: Option<(OsString, FsError)> },
      /// The primitive is missing (`primitive_unavailable`); `leftover` is the staged temporary.
      Unavailable { leftover: Option<(OsString, FsError)> },
  }

  /// Part P: stage `PROBE_TEMP` in `workspace` (an empty file), publish it onto `PROBE` with `rename_no_replace`,
  /// then remove what was written. `Err` is any failure other than "primitive unavailable" of the publish, or a
  /// failed create; the temporary is removed best-effort before returning it.
  pub(crate) fn probe_no_replace<D: DirHandle>(workspace: &D) -> flux_fs::Result<Probe>
  ```
  and the trait change:
  ```rust
  /// Step 5: create this operation's state, CREATED; for a tree, DEST first where it is absent (E1), then the
  /// section 241.5 probe inside the unpublished workspace (cut 8a, Part P), then the workspace published.
  fn create(&mut self, state: &OperationState, warnings: &mut Vec<RunWarning>) -> Result<(), RunError>;
  ```
- Produces, in `run/mod.rs`: `RunStep::Probe` with `as_str` `"probing the destination for no-replace publication"`;
  `RunWarning::ProbeNotRemoved { path: PathBuf, error: FsError }`, rendered by the CLI as
  `format!("warning: the no-replace probe {} could not be removed ({}); it goes when the operation's state is removed", path.display(), error.source)`.
- Produces, in `lock/error.rs`: `LockCode::NoReplacePublishUnavailable` -> `"NOREPLACE_PUBLISH_UNAVAILABLE"`, with
  a doc line: raised by the run (section 241.5), not by the lock protocol, as `PathComponentInvalid` already is.
- The refusal `TreePlace::create` returns:
  ```rust
  RunError::Refused {
      refusal: Box::new(Refusal {
          code: LockCode::NoReplacePublishUnavailable,
          holder: None,
          detail: format!(
              "{}: the destination has no atomic no-replace publication primitive, which a directory copy needs so that it never replaces a file (probed at {})",
              self.dest_shown.display(), <operations_shown>/<id>.creating/noreplace-probe
          ),
      }),
      changed: not_removed.is_some(),
      not_removed, // the probe's leftover, else the first failed removal of `.creating` / the control dirs / DEST
  }
  ```

- [ ] **Step 1: Write the failing tests.** In `run/tests.rs`, after `restart_stops_when_ownership_is_lost_and_deletes_nothing_after` (`:469`):

```rust
/// `/p/dest/.flux/operations/<ID>.creating/<name>`, as the call log shows it.
fn creating(name: &str) -> String {
    format!("/p/dest/.flux/operations/{ID}.creating/{name}")
}

#[test]
fn the_probe_is_the_runs_first_no_replace_rename_and_leaves_nothing_in_the_workspace() {
    let fs = fake();
    let (r, _) = run_tree(&fs, &cfg());
    ok(&r);
    let c = calls(&fs);
    let probe = at(&c, &format!("rename_no_replace({} -> {})", creating("noreplace-probe.tmp"), creating("noreplace-probe")));
    let removed = at(&c, &format!("remove_file({})", creating("noreplace-probe")));
    let manifest = at(&c, &format!("create_new({})", creating("manifest.tmp")));
    let published = at(&c, &format!("rename_no_replace(/p/dest/.flux/operations/{ID}.creating -> /p/dest/.flux/operations/{ID})"));
    assert!(probe < removed && removed < manifest && manifest < published, "{c:?}");
    assert_eq!(c.iter().filter(|x| x.starts_with("rename_no_replace(") && x.contains("noreplace-probe")).count(), 1, "once per run");
}

#[test]
fn a_missing_no_replace_primitive_is_refused_with_nothing_changed() {
    let fs = fake();
    fs.set_no_replace_support(false);
    let (r, got) = run_tree(&fs, &cfg());
    assert!(r.copy.is_none() && got.is_empty(), "{:?}", r.copy);
    let Some(RunError::Refused { refusal, changed, not_removed }) = &r.stop else { panic!("{:?}", r.stop) };
    assert_eq!((refusal.code, *changed, not_removed.is_none()), (LockCode::NoReplacePublishUnavailable, false, true));
    assert!(refusal.detail.contains("noreplace-probe"), "{}", refusal.detail);
    assert!(!fs.exists("/p/dest"), "DEST this run made is removed again");
    assert!(!fs.exists(LOCK));
    assert!(!fs.called("create_new(/p/dest/.flux/operations"), "no manifest was ever staged: {:?}", calls(&fs));
}

#[test]
fn a_probe_file_that_cannot_be_removed_is_a_warning_and_the_run_continues() {
    let fs = fake();
    // The probe's removal of `noreplace-probe` is the run's first `remove_file` (no lock exists to clean up and the
    // probe precedes the manifest's `.tmp` sweep). If the call log says otherwise, use `fail_nth` with the index
    // the log shows.
    fs.fail("remove_file", Code::PermissionDenied);
    let (r, got) = run_tree(&fs, &cfg());
    let out = ok(&r);
    assert_eq!((out.files_copied, got.len()), (2, 0));
    let kept = format!("/p/dest/.flux/operations/{ID}/noreplace-probe");
    assert!(
        r.warnings.iter().any(|w| matches!(w, RunWarning::ProbeNotRemoved { path, .. } if path.to_string_lossy().replace('\\', "/") == kept)),
        "{:?}", r.warnings
    );
    assert!(r.warnings.iter().any(|w| matches!(w, RunWarning::NotRemoved { .. })), "the workspace cannot go either: {:?}", r.warnings);
    assert!(fs.exists(format!("/p/dest/.flux/operations/{ID}.removing/noreplace-probe")), "retired, not removed");
    assert!(!fs.exists(LOCK), "the lock is still released");
}

#[test]
fn a_refused_probe_whose_temporary_cannot_be_removed_names_it_and_is_exit_1() {
    let fs = fake();
    fs.set_no_replace_support(false);
    fs.fail("remove_file", Code::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    let Some(RunError::Refused { refusal, changed, not_removed }) = &r.stop else { panic!("{:?}", r.stop) };
    assert_eq!((refusal.code, *changed), (LockCode::NoReplacePublishUnavailable, true));
    let (path, _) = not_removed.as_ref().expect("the temporary is named");
    assert_eq!(path.to_string_lossy().replace('\\', "/"), creating("noreplace-probe.tmp"));
    assert!(fs.exists(creating("noreplace-probe.tmp")) && fs.exists("/p/dest"), "left where they are");
    assert!(!fs.exists(LOCK), "the lock is still released");
}

#[test]
fn another_probe_publish_error_fails_the_run_at_the_probe_step() {
    let fs = fake();
    // The probe's publish is the run's first `rename_no_replace`.
    fs.fail("rename_no_replace", Code::PermissionDenied);
    let (r, _) = run_tree(&fs, &cfg());
    assert!(r.copy.is_none());
    let (step, path) = failed_at(&r.stop);
    assert_eq!((step, path), (RunStep::Probe, creating("noreplace-probe")));
    assert!(!fs.exists(creating("noreplace-probe.tmp")), "the temporary is removed best-effort");
    assert!(!fs.exists(LOCK));
}

#[test]
fn restart_probes_again_and_a_refusal_there_leaves_the_priors_resumable() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    fs.set_no_replace_support(false);
    let (r, _) = run_tree(&fs, &restart());
    assert_eq!(refused(&r.stop), (LockCode::NoReplacePublishUnavailable, false));
    assert_eq!(manifest(&fs, &id(5)).state, OpState::Failed, "not superseded");
    assert!(!fs.exists(format!("/p/dest/.flux/operations/{ID}.creating")) && !fs.exists(LOCK));
    assert!(fs.exists("/p/dest"), "DEST was the operator's");
}

#[test]
fn a_single_file_run_never_probes() {
    let fs = fake();
    fs.set_no_replace_support(false);
    let r = run_file(&fs, &cfg());
    assert!(r.stop.is_none() && r.copy.as_ref().is_some_and(|c| c.is_ok()), "{r:?}");
    assert!(!calls(&fs).iter().any(|c| c.contains("noreplace-probe")));
}
```
  `run_file` is at `:490`; `prior` at `:101`; `manifest` at `:93`; `refused` at `:86`; `failed_at` at `:591`.

  In `state.rs`'s test module, beside the `create_workspace` tests (`:931`, `:954`, `:1023`, `:1038`):
```rust
#[test]
fn begin_then_publish_is_create_workspace() {
    // `dest()`, `operations_dir`, `created()` and `id(1)` as `a_workspace_appears_only_with_its_manifest` (`:928`) uses them.
    let (fs, d) = dest();
    let ops = operations_dir(&d, Path::new("D")).unwrap();
    let path = format!("/p/dest/.flux/operations/{}", id(1));
    let building = begin_workspace(&ops, &created().operation_id).unwrap();
    assert!(fs.exists(format!("{path}.creating")) && !fs.exists(format!("{path}.creating/manifest")));
    let ws = publish_workspace(&ops, building, &created()).unwrap();
    assert_eq!(read_state(&ws, OsStr::new(MANIFEST)).unwrap(), Ok(created()));
    assert!(fs.exists(format!("{path}/manifest")) && !fs.exists(format!("{path}.creating")));
}
```
  In `lock/error.rs`'s test (`:78-88`): `assert_eq!(LockCode::NoReplacePublishUnavailable.as_str(), "NOREPLACE_PUBLISH_UNAVAILABLE");`.
  In `report.rs`'s `every_run_warning_is_one_warning_line_naming_its_path` (`:580`): add
  `RunWarning::ProbeNotRemoved { path: p(), error: io() }` to `all`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core run::tests state::tests lock::error && cargo nextest run -p flux-cli`
Expected: compile errors on `RunStep::Probe`, `RunWarning::ProbeNotRemoved`, `LockCode::NoReplacePublishUnavailable`,
`begin_workspace`; once those exist as stubs, the seven run tests fail on their first assertion.

- [ ] **Step 3: Split `create_workspace`** in `state.rs` into `creating_name`, `begin_workspace`, `publish_workspace`,
  with `create_workspace` composing them. Behaviour of the composition is byte-identical to today (`:604-620`).

- [ ] **Step 4: Add the three enum variants** (`RunStep::Probe`, `RunWarning::ProbeNotRemoved`,
  `LockCode::NoReplacePublishUnavailable`) and the CLI line; make `primitive_unavailable` `pub(crate)`.

- [ ] **Step 5: Implement `probe_no_replace`** in `place.rs`:
  `workspace.create_new(temp)?` (drop the writer at once: nothing is written); then `match
  workspace.rename_no_replace(temp, workspace, name)`: `Ok(())` -> `Available { leftover: remove(workspace, name) }`;
  `Err(e) if primitive_unavailable(&e.source)` -> `Unavailable { leftover: remove(workspace, temp) }`; `Err(e)` ->
  `let _ = workspace.remove_file(temp); Err(e)`. `remove` is `ws.remove_file(n).err().map(|e| (n.to_os_string(), e))`.

- [ ] **Step 6: Rewrite `TreePlace::create`** (`place.rs:184-209`) with the new signature:
  1. DEST and `operations_dir` as today.
  2. `let building = begin_workspace(&operations, id).map_err(|e| failed(RunStep::State, &shown_creating, e))?`
     where `shown_creating = self.operations_shown().join(creating_name(id))`.
  3. `match probe_no_replace(&building)`:
     - `Ok(Probe::Available { leftover: None })`: nothing.
     - `Ok(Probe::Available { leftover: Some((name, error)) })`: `warnings.push(RunWarning::ProbeNotRemoved { path: self.operations_shown().join(id).join(name), error })`
       (the file's path once the workspace is published).
     - `Ok(Probe::Unavailable { leftover })`: `drop(building)`; `return Err(self.refuse_no_replace(operations, id, leftover))`.
     - `Err(error)`: `drop(building)`; `let _ = operations.remove_dir(&creating_name(id))`; `return
       Err(failed(RunStep::Probe, &shown_creating.join(PROBE), error))`.
  4. `let workspace = publish_workspace(&operations, building, state).map_err(|e| failed(RunStep::State, &self.shown(id), e))?; drop(workspace); self.operations = Some(operations); Ok(())`.

  `refuse_no_replace(&mut self, operations: F::Dir, id: &str, leftover: Option<(OsString, FsError)>) -> RunError`:
  `let shown = self.operations_shown().join(creating_name(id)); let mut not_removed = leftover.map(|(n, e)| (shown.join(n), e));`
  then `operations.remove_dir(&creating_name(id))`, recording its error into `not_removed` only if none is recorded
  yet; `self.operations = Some(operations); self.remove_control_dirs(true)` likewise (it sets `self.operations =
  None` first, so each handle closes before its directory goes, `place.rs:286-298`); then the `RunError::Refused`
  from the Interfaces block. `remove_control_dirs` tolerates a non-empty `operations/` (`state.rs:655-680`), and a
  non-empty DEST fails its `remove_dir`, which the leftover case ignores.

- [ ] **Step 7: Thread `warnings` through** `Place::create` (trait `:41`, `FilePlace` `:355` ignores it) and the
  call in `open_operation` (`session.rs:148`: `place.create(&state, warnings)`). `place.rs` needs
  `use crate::lock::{LockCode, Refusal}` beside its existing `LockResult` import (`:9`), and `use
  crate::tree::primitive_unavailable`. `give_back` (`:231`) already
  handles a `Refused` from `create`: it removes the lock this run created and names it if that fails.

- [ ] **Step 8: Run the tests**

Run: `cargo nextest run -p flux-core && cargo nextest run -p flux-cli`
Expected: all pass, including the existing `a_tree_run_copies_and_leaves_no_state_behind` (`:124`) and
`the_lock_comes_first_then_the_state_then_the_record_naming_it` (`:135`).

- [ ] **Step 9: Run the gate**

Run: `just check`
Expected: clean; `0 failed`.

- [ ] **Step 10: Commit**

```bash
git add crates/flux-core crates/flux-cli
git commit -m "feat: probe the destination for no-replace publication inside the unpublished workspace (cut 8a, Part P)"
```

---

### Task 7: real-system tests

**Files:**
- Create: `crates/flux-core/tests/safety_std_fs.rs`
- Reference: `crates/flux-core/tests/tree_std_fs.rs:12-36` (its `opts`, `run`, `names` helpers are the model;
  copy them, do not share: integration tests are separate crates)

**Interfaces:**
- Consumes: `flux_core::copy_tree`, `flux_core::run::{tree, RunConfig}`, `flux_core::ids::new_id`,
  `flux_platform::StdFileSystem`, `flux_fs::{MountRoot, DirHandle, DestinationRoot}`.

- [ ] **Step 1: Write the Linux bind-mount test (M, and the query against the same mount)**

```rust
#[cfg(target_os = "linux")]
mod linux_mount {
    use super::*;

    /// `sudo -n mount --bind src dst`, unmounted on drop. `None` when the facility is missing locally; on CI
    /// (`CI` set) a missing facility is a failure, never a skip.
    struct Bind(std::path::PathBuf);

    fn bind(src: &Path, dst: &Path) -> Option<Bind> {
        let ok = std::process::Command::new("sudo")
            .args(["-n", "mount", "--bind"])
            .arg(src)
            .arg(dst)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(Bind(dst.to_path_buf()));
        }
        assert!(std::env::var_os("CI").is_none(), "CI must be able to `sudo -n mount --bind`");
        eprintln!("skipped: no passwordless sudo for `mount --bind`");
        None
    }

    impl Drop for Bind {
        fn drop(&mut self) {
            let _ = std::process::Command::new("sudo").args(["-n", "umount"]).arg(&self.0).status();
        }
    }

    #[test]
    fn a_bind_mount_of_a_source_subdirectory_inside_the_destination_is_skipped_and_the_source_is_untouched() {
        let d = TempDir::new().unwrap();
        let src = tree_in(d.path());
        let dst = d.path().join("dst");
        std::fs::create_dir_all(dst.join("sub")).unwrap();
        let Some(_mount) = bind(&src.join("sub"), &dst.join("sub")) else { return };

        // The query, against this mount.
        let root = StdFileSystem.destination_root(&dst).unwrap();
        let sub = root.open_dir(std::ffi::OsStr::new("sub")).unwrap();
        assert_eq!(sub.mount_root(&root).unwrap(), MountRoot::Yes);
        drop(sub);
        drop(root);

        let (r, got) = run(&src, &dst);
        let out = r.unwrap();
        assert_eq!((out.files_copied, out.failures.create_dir), (1, 1), "{got:?}");
        assert_eq!(got[0].path, Path::new("sub"));
        let TreeFailureCause::CreateDir(e) = &got[0].cause else { panic!("{:?}", got[0].cause) };
        assert_eq!(e.code, Code::SafetyRejected);
        assert_eq!(names(&src.join("sub")), vec!["b"], "nothing landed in the source through the mount");
        assert_eq!(names(&dst), vec!["a", "sub"]);
    }
}
```
  `tree_in` is `tests/run.rs`'s fixture (`flux-cli/tests/run.rs:19-25`), copied: `src/a` (1 byte), `src/sub/b`
  (2 bytes). The `TempDir` is declared before the `Bind`, so the mount is undone before the directory is removed.

- [ ] **Step 2: Write the parent-component link tests (A)**, Windows with a junction (the gate) and unix with a symlink:

```rust
fn run_cfg() -> RunConfig {
    RunConfig {
        restart: false,
        break_lock: false,
        operation_id: flux_core::ids::new_id(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: "test".to_string(),
        before_mutation: None,
        heartbeat_interval: std::time::Duration::from_secs(3600),
    }
}

/// `src/0-first` (reached by the walk BEFORE `sub`), `src/sub/b`; `holder/link` -> `src/sub`; DEST = `holder/link/out`.
/// Without Part A the run would create `out` inside `src/sub` and write `0-first` there before the walk's own
/// check could stop it.
fn link_fixture(d: &Path, link: impl FnOnce(&Path, &Path) -> bool) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let src = d.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("0-first"), b"F").unwrap();
    std::fs::write(src.join("sub").join("b"), b"BB").unwrap();
    let holder = d.join("holder");
    std::fs::create_dir(&holder).unwrap();
    if !link(&src.join("sub"), &holder.join("link")) {
        return None;
    }
    Some((src, holder.join("link").join("out")))
}

fn assert_refused_before_the_lock(src: &Path, dst: &Path) {
    let mut got = Vec::new();
    let r = flux_core::run::tree(&StdFileSystem, src, dst, &opts(), &run_cfg(), &mut |f| got.push(f));
    assert!(r.stop.is_none(), "{:?}", r.stop);
    let Some(Err(a)) = &r.copy else { panic!("{:?}", r.copy) };
    assert_eq!(a.error.code(), Code::SafetyRejected);
    assert!(a.error.to_string().contains("resolved location"), "{}", a.error);
    assert!(a.refused_unchanged());
    assert_eq!(names(&src.join("sub")), vec!["b"], "no `out`, no lock, no workspace in the source");
    assert_eq!(std::fs::read(src.join("0-first")).unwrap(), b"F");
    assert!(got.is_empty());
}

#[cfg(unix)]
#[test]
fn a_symlink_in_a_parent_component_of_dest_into_a_source_subdirectory_is_refused_before_the_lock() {
    let d = TempDir::new().unwrap();
    let (src, dst) = link_fixture(d.path(), |t, l| std::os::unix::fs::symlink(t, l).is_ok()).unwrap();
    assert_refused_before_the_lock(&src, &dst);
}

#[cfg(windows)]
#[test]
fn a_junction_in_a_parent_component_of_dest_into_a_source_subdirectory_is_refused_before_the_lock() {
    let d = TempDir::new().unwrap();
    let junction = |t: &Path, l: &Path| {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", &l.display().to_string(), &t.display().to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    // Not a skip: `mklink /J` needs no privilege (`flux-platform/tests/dir_handle.rs`, the junction tests).
    let (src, dst) = link_fixture(d.path(), junction).expect("could not create a junction; mklink /J requires no privilege");
    assert_refused_before_the_lock(&src, &dst);
}
```

- [ ] **Step 3: Run them**

Run: `cargo nextest run -p flux-core --test safety_std_fs --no-capture`
Expected on a Linux box with passwordless sudo: 2 passed; without it: the mount test prints `skipped: ...` and
passes, the symlink test passes. On WSL without sudo the skip line must appear.

- [ ] **Step 4: Run the gate, then push for CI** (the Windows and macOS legs run only there)

Run: `just check` then `git push`; watch `gh run list --branch spec/cut-8a --limit 1` until the three `Test (...)`
jobs are green.
Expected: `Test (ubuntu-latest)` runs the bind-mount test for real (no `skipped:` line in its log); `Test
(windows-latest)` runs the junction test; `Test (macos-latest)` runs the `/dev` query test from Task 1.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/tests/safety_std_fs.rs
git commit -m "test: a real bind mount inside DEST is skipped; a link in a parent component of DEST is refused (cut 8a)"
```

---

### Task 8: the debt entries, the known limits and the design record

**Files:**
- Modify: `TODO.md:247-262` (item 113), `TODO.md:268-279` (section 42, "The destination half too")
- Modify: `crates/flux-platform/src/dir_unix.rs:164-170` (the rename_no_replace comment's last paragraph)
- Modify: `docs/superpowers/specs/2026-10-03-cut-8a-safety-design.md`, "Design record"

- [ ] **Step 1: `TODO.md`.** Item 113's entry: mark done the way the file marks a resolved entry (read how the
  nearest `[x]` entry is phrased), naming the commit of Task 6 and what the probe is (per the chosen option). The
  section 42 entry: replace "an alias of a source SUBdirectory is still merged into, and closing it needs the source
  identities this cut does not track" with: cut 8a refuses a pre-existing destination mount root (Part M), and the
  remaining case is known limit 1. Add the three known limits from the spec, verbatim, as three `- [ ]` entries
  under a new heading `## Cut 8a known limits`.
- [ ] **Step 2: `dir_unix.rs`.** Replace "That probe belongs to the engine, which does not exist yet, and is
  tracked" with a sentence saying where the probe now lives (Task 6's function) and that this measurement remains
  its justification.
- [ ] **Step 3: The spec's Part P.** Rewrite "When it runs" to the placement built in Task 6: inside
  `<id>.creating`, after that directory is created and before its manifest is written and it is published; the
  lock is this run's (just obtained), the record is not yet written, so the probe is guarded as the manifest write
  is; `--restart`'s supersede comes later still, so a refusal leaves every prior untouched. Rewrite the table's
  last two rows: a probe file that stays after a SUCCESSFUL publish is `RunWarning::ProbeNotRemoved` and the
  workspace later fails its retirement (`RunWarning::NotRemoved`); after a REFUSED probe the stop stays
  `NOREPLACE_PUBLISH_UNAVAILABLE` with `changed: true` and `not_removed` naming the temporary under
  `<id>.creating`, which stays (the section 21.1 scan passes it over, `prior.rs:47-49`), the control directories
  and DEST stay since they are not empty, and the lock is released. Add a "Design record" bullet "Part P's
  placement (plan, 2026-10-07)" with decision 1's measurement, the four options agy weighed, agy's recommendation
  and the owner's choice. Correct the "Why" table's Part A row and the Part A section's first sentence per
  decision 12: the CLI resolves a link in a parent component before the engine runs; Part A closes the engine
  API and the check-then-use window between that resolution and the engine's open.
- [ ] **Step 4: `just check`** (typos runs over Markdown). Expected: clean.
- [ ] **Step 5: Commit**

```bash
git add TODO.md crates/flux-platform/src/dir_unix.rs docs/superpowers/specs/2026-10-03-cut-8a-safety-design.md
git commit -m "docs: cut 8a closes item 113 and the destination half of section 42; known limits recorded"
```

---

## Exhaustiveness self-audit (run before the handoff)

- **Spec coverage:** P -> Task 6 (pending the fork); M -> Tasks 1, 3; A -> Tasks 2, 4; "cannot tell" / degraded
  warnings -> Tasks 3, 4, 5; "the source-root check runs first" -> Task 3 (`the_source_root_check_runs_before_the_mount_query`);
  Testing, fake -> Tasks 3, 4, 6; Testing, real system -> Tasks 1 (macOS `/dev`), 7; Known limits -> Task 8;
  Dependencies -> none added (Tasks 1, 2 use the locked crates); Out of scope -> untouched.
- **Open in the plan:** nothing; the P fork is decided (decision 1). Every step decides one thing.
- **Type consistency:** `MountRoot::{Yes, No, Unknown}`; `mount_root(&self, parent: &Self)`;
  `canonical_path(&self) -> Result<PathBuf>`; `TreeOutcome::{mount_unknown, containment_degraded}`;
  `locate_tree(fs, src_root, dst_root, src_identity, safety, out)`; `containment(fs, src_root, anchor, anchor_shown, safety, out)`;
  `safety_warning_lines(&TreeOutcome)`; the fake's `set_mount_root`, `set_canonical_path`, keys `mount_root`,
  `canonical_path`.
