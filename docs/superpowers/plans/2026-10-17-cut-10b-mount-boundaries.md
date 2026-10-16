# Cut 10b: Source Mount Boundaries Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy` stops descending into a source directory that is the root of another mount (section 42): the destination gets an empty directory at its name, the skip is reported and counted, and `--cross-filesystems` restores today's behaviour exactly.

**Architecture:** A required path-based `FileSystem::mount_root(path, parent)` joins `flux-fs`, implemented on Linux with `statx(STATX_ATTR_MOUNT_ROOT)` plus a device-number fallback (the logic the destination arm already has, shared through two pure helpers in `flux-platform`), by device comparison on macOS, and as `No` on Windows. The walk asks it for every directory below the root, before the cycle check, and yields two new events (`MountSkipped`, `MountUnknown`) between a directory's `Dir` and its `DirEnd`; the engine creates the empty directory through `enter_dir` as for any directory, reports the skip as a not-failure record, and treats `Unknown` as a grouped warning (Default) or a whole-operation refusal (Strict). `--cross-filesystems` is a `CopyOptions` bool that joins the resume configuration, written only when true.

**Tech Stack:** Rust 2024 workspace (`flux-fs`, `flux-core`, `flux-platform`, `flux-cli`), `rustix` 1.1 (`statx`, `AtFlags::NO_AUTOMOUNT`), `clap`, `serde`, the `FaultFs` test fake, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-10-17-cut-10b-mount-boundaries-design.md` (committed `8864e03` on `spec/cut-10b-mount-boundaries`, panel GREEN after three rounds). Section numbers below are the spec's. Every line number in this plan was read at main `a563613` (the branch adds only docs).

## Global Constraints

- Nothing below a skipped mount is listed, stat'ed or yielded (M1); the walk root is never skipped (M2); the empty directory goes through `enter_dir` like any directory and is counted in `directories_created`, never in a `files_*` counter (M3).
- With `--cross-filesystems` the walk makes the same calls and yields the same events as before this cut (M4). A tree with no mount below its root produces the same stderr, JSON and on-disk state as before, and a default-mode state file is byte-identical to the pre-cut format (M5).
- `Unknown` is never treated as "not a mount" without a trace (M7): a grouped warning under `Safety::Default`, a whole-operation `SafetyRejected` refusal under `Safety::Strict` (exit 1, because something was already created; `crates/flux-cli/src/exit_code.rs:16-23` and its pin at `:125`).
- No new third-party crate; the walk stays path-based (N4). The Linux query passes `AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT` (both exist in `rustix` 1.1.5, `src/backend/linux_raw/fs/types.rs:38,50`).
- The repo gate, as is: `cargo fmt`; `TMPDIR=$HOME/ff_tmp just check` (expected `Summary [...] N tests run: N passed, 6 skipped`, N grows with this cut; 1558 today); `cargo clippy --workspace --all-targets --release -- -D warnings` clean (read the exit code). Tasks that touch `cfg` code also run the Windows compile check: `TMPDIR=$HOME/ff_tmp cargo check -p flux-platform -p flux-core -p flux-cli --tests --target x86_64-pc-windows-gnu` and `cargo clippy -p flux-platform --tests --target x86_64-pc-windows-gnu -- -D warnings` (the target is installed on this box).
- Commit messages end with the session's standing attribution trailer, `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` as of execution (the controller commits subagent work with it); nothing under `.clavity/` or `.superpowers/` is committed.
- Every `CopyOptions { .. }` literal without a `..` spread must name the new `cross_filesystems` field; the compiler is the oracle; the files are pre-listed in Task 1 and checked on both axes (`git status --short`, `git log --stat`) after.
- Timing (Task 6) is run by the controller at top level on an idle machine, only after the owner's explicit OK for that run, never by a subagent.

## Review Focus

1. Two sibling mounts under one directory: two `MountSkipped` records, `mounts_skipped == 2`, two empty directories, `files_total` unchanged. Pinned in Task 4 (test 7).
2. A mount directory at a depth beyond the cap: the depth cap's error wins and no `source_mount_root` call is made for it (the check order of 3.3). Pinned in Task 3 (test 3-7).
3. A mount under a `Frame::Skipped` parent (a reserved-name refusal above it): no record, no count, no directory (M6). Pinned in Task 4 (test 8).
4. `Unknown` under Strict for a directory under a `Frame::Skipped` parent: no abort, because the arm acts only when `live`. Pinned in Task 4 (test 11).
5. `--cross-filesystems` given on a tree without mounts: JSON still has no `mounts_skipped` key and the same key count as today (M5 holds with the flag on too). Pinned in Task 5 (test 15).

---

### Task 1: The trait method, the option, every implementer, and the platform arms

**Files:**
- Modify: `crates/flux-fs/src/fs.rs` (`FileSystem::mount_root` after `read_link` at `:232`, before the trait's closing brace at `:233`; `MountRoot` is already at `:476`; the test `NullFs` impl at `:550-607` gains the method).
- Modify: `crates/flux-fs/src/options.rs` (`CopyOptions` at `:85-94` gains `pub cross_filesystems: bool` after `links`).
- Modify (every full `CopyOptions` literal gains `cross_filesystems: false`; the compiler names them; literals with a `..` spread need nothing): `crates/flux-cli/src/main.rs:151`, `crates/flux-core/src/copy.rs:1258`, `crates/flux-core/src/run/resume.rs:151`, `crates/flux-core/src/run/tests.rs:47`, `crates/flux-core/src/state.rs:1838` and `:1988`, `crates/flux-core/src/tree.rs:2789`, `crates/flux-core/tests/replace_std_fs.rs:13`, `crates/flux-core/tests/safety_std_fs.rs:12`, `crates/flux-core/tests/tree_std_fs.rs:13`, `benches/copy.rs:27`, `tests/integration/support/mod.rs:12`.
- Modify: `crates/flux-core/src/fault_fs.rs` (`FileSystem::mount_root` in the `impl FileSystem for FaultFs` block at `:1418`, beside `metadata` at `:1486`; the `mount_roots` map at `:45` and `set_mount_root` at `:1187` are reused unchanged).
- Modify: `crates/flux-core/src/walk.rs` (the test double `EscapingFs` at `:781` gains the method; nothing else in this task).
- Create: `crates/flux-platform/src/mount.rs` (`#[cfg(unix)]`, two `pub(crate)` pure helpers plus their unit tests); declare it in `crates/flux-platform/src/lib.rs` with `#[cfg(unix)] mod mount;` beside `mod dir_unix;`.
- Modify: `crates/flux-platform/src/dir_unix.rs` (`DirHandle::mount_root` at `:519-556` calls the helpers; behaviour unchanged).
- Modify: `crates/flux-platform/src/std_fs.rs` (`FileSystem::mount_root` in `impl FileSystem for StdFileSystem` at `:242`, one `#[cfg(unix)]` body and one `#[cfg(windows)]` body, as `metadata` at `:260-310` is split).

**Interfaces:**
- Produces (exact):
  ```rust
  // crates/flux-fs/src/fs.rs, on FileSystem (REQUIRED, no default body)
  /// Whether the directory at `path` is the root of a mount, judged against `parent` (the directory it was listed
  /// in). Never follows a link at `path`. `Unknown` only where the platform cannot tell (Linux without
  /// `STATX_ATTR_MOUNT_ROOT` and equal device numbers); Windows answers `No`: a mounted-volume folder is a
  /// reparse point the walk never descends into. An `Err` is the query's own failure.
  fn mount_root(&self, path: &Path, parent: &Path) -> Result<MountRoot>;
  // crates/flux-fs/src/options.rs
  pub struct CopyOptions { /* existing fields */, pub cross_filesystems: bool }
  // crates/flux-platform/src/mount.rs (pub(crate))
  #[cfg(target_os = "linux")]
  pub(crate) fn from_statx_attributes(mask: rustix::fs::StatxAttributes, attrs: rustix::fs::StatxAttributes) -> Option<MountRoot>; // None when `mask` lacks MOUNT_ROOT
  pub(crate) fn from_devices(child_dev: u64, parent_dev: u64) -> MountRoot; // differ: Yes; equal: No on macOS, Unknown elsewhere
  ```
  Fake: `FaultFs` answers from `mount_roots` keyed by `self.norm(path)` (default `No`), records `source_mount_root(<path>)`, fault key `"source_mount_root"` for `fail`/`fail_nth`/`fail_always`.

- [ ] **Step 1: Add the trait method and the option** in `flux-fs`; add `cross_filesystems: false` to the twelve literals listed above (run `cargo check --workspace --all-targets` and fix exactly the sites the compiler names; report any site not in the list as `ANOMALY`).
- [ ] **Step 2: Implement the fakes**: `NullFs` (`Ok(MountRoot::No)`), `EscapingFs` (`Ok(MountRoot::No)`), `FaultFs` as the Interfaces block says (mirror `metadata` at `fault_fs.rs:1486-1488`: `let p = self.norm(path); self.record(format!("source_mount_root({})", p.display()), "source_mount_root")?;` then `g.mount_roots.get(&p).copied().unwrap_or(MountRoot::No)`; `parent` is unused by the fake).
- [ ] **Step 3: Write the helper unit tests in `mount.rs`** (`#[cfg(test)] mod tests`): `different_devices_are_a_mount_root` (`from_devices(1, 2) == Yes`); `equal_devices_cannot_tell_on_linux_and_are_no_on_macos` (`from_devices(3, 3)` is `Unknown` under `cfg!(target_os = "linux")`, `No` under `cfg!(target_os = "macos")`); `#[cfg(target_os = "linux")] statx_decides_only_when_the_kernel_reports_the_attribute` (`from_statx_attributes(StatxAttributes::empty(), StatxAttributes::MOUNT_ROOT) == None`; `(MOUNT_ROOT, MOUNT_ROOT) == Some(Yes)`; `(MOUNT_ROOT, empty()) == Some(No)`).
- [ ] **Step 4: Run them to see them fail**: `cargo test -p flux-platform --lib mount::` → compile error (the module does not exist yet).
- [ ] **Step 5: Create `mount.rs`** with the two helpers; rewrite `dir_unix.rs:519-556` to call them (`statx` branch: `if let Some(a) = crate::mount::from_statx_attributes(st.stx_attributes_mask, st.stx_attributes) { return Ok(a); }`; fallback: `Ok(crate::mount::from_devices(child_dev, parent_dev))`), keeping its `Errno::NOSYS | Errno::NOTSUP` arm and its error mapping.
- [ ] **Step 6: Implement `StdFileSystem::mount_root`**. Unix body: on Linux `rustix::fs::statx(rustix::fs::CWD, path, AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT, StatxFlags::empty())`: `Ok(st)` with `Some(a)` from the helper returns `Ok(a)`; `Ok(_)` and `Err(Errno::NOSYS | Errno::NOTSUP)` fall through; any other `Err(e)` is `Err(FsError::from_io(std::io::Error::from(e)))`. The fallback (every Unix): `std::fs::symlink_metadata(path)` and `symlink_metadata(parent)` (each `map_err(FsError::from_io)`), `std::os::unix::fs::MetadataExt::dev()` of both into `from_devices`. Windows body: `Ok(MountRoot::No)` with the doc comment from `dir_windows.rs:152-153`.
- [ ] **Step 7: Run**: `cargo test -p flux-platform --lib mount::` → `3 passed` on Linux (`2 passed` on macOS); `cargo nextest run -p flux-platform --test dir_handle` → unchanged (the Part M handle tests pin the refactor); `TMPDIR=$HOME/ff_tmp just check` → every test passes (N unchanged plus 3); the Windows compile check and Windows clippy (Global Constraints) exit 0.
- [ ] **Step 8: Commit** `feat(fs): FileSystem::mount_root and CopyOptions.cross_filesystems, with the shared mount-root helpers (cut 10b)`.

### Task 2: Platform tests for the real arms

**Files:**
- Modify: `crates/flux-platform/tests/std_fs.rs` (free functions with `TempDir` and `StdFileSystem`, as `:7-14` do; `#[cfg(unix)]` tests at `:66-96` show the attribute style).

**Interfaces:**
- Consumes: Task 1's `FileSystem::mount_root`, `DirHandle::mount_root` (`DestinationRoot::destination_root` and `DirHandle::open_dir`, as `crates/flux-platform/tests/dir_handle.rs` uses them).

- [ ] **Step 1: Write the tests**: `#[cfg(unix)] mount_root_is_no_for_a_plain_subdirectory_and_agrees_with_the_handle_query` (a `TempDir` `d`, `create_dir(d/sub)`; `StdFileSystem.mount_root(&d/sub, d) == Ok(MountRoot::No)`; `let root = StdFileSystem.destination_root(d)`, `let sub = root.open_dir("sub")`, `sub.mount_root(&root) == Ok(MountRoot::No)`); `#[cfg(target_os = "linux")] mount_root_is_yes_for_a_real_mount_point_asked_without_privilege` (when `Path::new("/proc/self").is_dir()`: `mount_root("/proc", "/") == Ok(Yes)`; when `/dev/shm` is a directory: `mount_root("/dev/shm", "/dev") == Ok(Yes)`; each half prints `skipped: <path> is absent` and returns when its path is absent, and the test panics if BOTH halves were skipped); `#[cfg(unix)] mount_root_reports_a_missing_path_as_not_found` (`mount_root(d/none, d)` is `Err` with `source.kind() == NotFound`); `#[cfg(windows)] mount_root_is_always_no_on_windows` (a `TempDir` child: `Ok(No)`).
- [ ] **Step 2: Run**: `cargo nextest run -p flux-platform --test std_fs` → the new tests pass on Linux (the `/proc` test runs; `/dev/shm` runs on this box). Mutant check (restore after): replace the Linux `statx` branch's answer with `No` → `mount_root_is_yes_for_a_real_mount_point...` goes red; replace `from_devices`'s differ arm with `No` → `different_devices_are_a_mount_root` goes red.
- [ ] **Step 3: Commit** `test(platform): mount_root on a plain directory, a real mount point and Windows (cut 10b)`.

### Task 3: The walk: policy, the two events, the check order

**Files:**
- Modify: `crates/flux-core/src/walk.rs` (`WalkEvent` at `:16-47`; `Frame` at `:59-68`; `Walk` at `:72-85`; `walk` at `:135` and `walk_with_depth` at `:140-182`; `Iterator::next` at `:204-277`; `enter` at `:284-375`; the `paths` test helper at `:440-451`; new tests in `mod tests`).

**Interfaces:**
- Consumes: Task 1's `FileSystem::mount_root`, `CopyOptions.cross_filesystems`, `FaultFs::set_mount_root`.
- Produces (exact, all `pub`):
  ```rust
  pub enum MountPolicy { Cross, Stay }                      // Debug, Clone, Copy, PartialEq, Eq
  impl MountPolicy { pub fn of(opts: &flux_fs::CopyOptions) -> MountPolicy }  // Cross when opts.cross_filesystems, else Stay
  pub struct WalkOptions { pub max_depth: usize, pub mounts: MountPolicy }
  pub fn walk_with<'a, F: FileSystem>(fs: &'a F, root: &Path, options: WalkOptions) -> Result<Walk<'a, F>>;
  // walk(fs, root) == walk_with(fs, root, WalkOptions { max_depth: DEFAULT_MAX_DEPTH, mounts: MountPolicy::Cross })
  // walk_with_depth(fs, root, n) == walk_with(fs, root, WalkOptions { max_depth: n, mounts: MountPolicy::Cross })
  pub enum WalkEvent { Dir{..}, File{..}, Symlink{..}, Other{..}, DirEnd{..},
      /// After this directory's `Dir`, before its `DirEnd`: it is the root of a mount and nothing below it was listed.
      MountSkipped { path: PathBuf },
      /// After this directory's `Dir`, before any child: whether it is a mount root could not be told; it was listed.
      MountUnknown { path: PathBuf } }
  ```
  Private: `enum MountNote { Skipped, Unknown }`; `Frame.mount: Option<MountNote>` (taken and yielded by `next` before the frame's first entry); `Walk.mounts: MountPolicy`.

- [ ] **Step 1: Write the failing tests** in `mod tests` (extend `paths` to render `MountSkipped` as `"M <rel>"` and `MountUnknown` as `"U <rel>"`; add `paths_with(fs, policy)` that calls `walk_with` with `DEFAULT_MAX_DEPTH`; fixtures as `component_order_tree` at `:410-421` builds them):
  - `a_mount_below_the_root_is_skipped_and_nothing_under_it_is_read` (3-1): `/r/m` with `/r/m/inside` a file, `/r/z` a file; `set_mount_root("/r/m", Yes)`; `paths_with(.., Stay) == ["D m", "M m", "E m", "F z"]`; `fs.calls()` has no `read_dir(/r/m)`, no `metadata(/r/m/inside)`, and exactly one `source_mount_root(/r/m)`.
  - `the_root_is_never_asked_whether_it_is_a_mount` (3-2): `set_mount_root("/r", Yes)`, a file `/r/f`; `Stay` yields `["F f"]` and no `source_mount_root(` call.
  - `crossing_makes_the_same_calls_and_events_as_before` (3-3): the fixture of 3-1 under `Cross` yields `["D m", "F m/inside", "E m", "F z"]` and no `source_mount_root(` call; `walk(&fs, ..)` yields the same.
  - `a_bind_mount_of_an_ancestor_is_a_skipped_mount_not_a_cycle` (3-4): the fixture of `a_directory_that_re_enters_an_ancestor_is_refused` (`:571-591`) plus `set_mount_root("/r/a/b", Yes)`; `Stay`: events `["D a", "D a/b", "M a/b", "E a/b", "F a/marker", "E a"]` (entries sort by bytes, so `b` precedes `marker`) and no error; `Cross`: one error at `a/b` and `dirs == ["a"]` as today. Mutant: the mount check after the ancestor logic turns the `Stay` half red.
  - `an_unknown_mount_is_announced_and_then_listed` (3-5): `set_mount_root("/r/u", Unknown)`, `/r/u/f`; `Stay` yields `["D u", "U u", "F u/f", "E u"]`; `Cross` yields `["D u", "F u/f", "E u"]`.
  - `a_failing_mount_query_fails_that_directory_and_the_walk_continues` (3-6): `fs.fail("source_mount_root", Code::PermissionDenied)`, `/r/a` and `/r/b` directories, `/r/b/f`; under `Stay` the errors are `["a"]` (the fault is consumed once) and `b`'s events follow; no `read_dir(/r/a)`.
  - `a_too_deep_mount_meets_the_depth_cap_first` (3-7, Review Focus 2): `walk_with(fs, "/r", WalkOptions { max_depth: 1, mounts: Stay })` over `/r/a/b` with `set_mount_root("/r/a/b", Yes)`: the error for `a/b` is the cap's (`"directory depth cap exceeded"`) and `fs.calls()` has no `source_mount_root(/r/a/b)`.
- [ ] **Step 2: Run** `cargo test -p flux-core --lib walk::` → compile errors (`walk_with`, `MountPolicy`, the variants).
- [ ] **Step 3: Implement** in `walk.rs`: the types above; `walk_with` builds the root frame exactly as `walk_with_depth` does today (no query for the root, M2) and stores `mounts`; `walk`/`walk_with_depth` delegate. In `next` (`:204`), right after the empty-stack check and before `taken`: `if let Some(note) = self.stack.last_mut().expect(..).mount.take() { let path = frame.rel.clone(); return Some(Ok(match note { MountNote::Skipped => WalkEvent::MountSkipped { path }, MountNote::Unknown => WalkEvent::MountUnknown { path } })); }`. In `enter` (`:284`), after the `file_type != Dir` refusal (`:322-330`) and BEFORE `let mut pushed_ancestor = false;` and the `FileIdentity::Strong` ancestor block (`:337-355`): with `self.mounts == MountPolicy::Stay`, `let parent = abs.parent().expect("a descendant has a parent")`; `match self.fs.mount_root(&abs, parent)`: `Err(cause) => return Err(WalkError { path: rel, cause })`; `Ok(MountRoot::Yes) => { push Frame { rel: rel.clone(), entries: Vec::new().into_iter(), pushed_ancestor: false, emit_end: true, mount: Some(MountNote::Skipped) }; return Ok(WalkEvent::Dir { path: rel, identity: m.identity }); }`; `Ok(MountRoot::Unknown) => note = Some(MountNote::Unknown)`; `Ok(MountRoot::No) => note = None`; then the existing ancestor logic, `read_dir`, and the frame push with `mount: note`. Every existing `Frame { .. }` literal gains `mount: None`.
- [ ] **Step 4: Keep the crate compiling**: the engine's `match event` at `tree.rs:669-770` has five arms and no wildcard, so this task adds one placeholder arm there, `WalkEvent::MountSkipped { .. } | WalkEvent::MountUnknown { .. } => {}`, with the comment `// Cut 10b: Task 4 gives these their behaviour.` (`run/sweep.rs:51` has `Ok(_) => continue` and needs nothing). Then run `cargo test -p flux-core --lib walk::` → all pass, the seven new ones included; `TMPDIR=$HOME/ff_tmp just check` → every test passes.
- [ ] **Step 5: Commit** `feat(walk): MountPolicy, walk_with, and the MountSkipped/MountUnknown events before the cycle check (cut 10b)`.

### Task 4: The engine: the arms, the cause, the counters, the report lines

**Files:**
- Modify: `crates/flux-core/src/tree.rs` (`TreeOutcome` at `:32-70`: `mounts_skipped: u64` after `symlinks_skipped` at `:51`, `source_mount_unknown: Option<DegradedGroup>` after `mount_unknown` at `:54`; `TreeFailureCause::MountSkipped` after `SymlinkSkipped` at `:188`; `FailureTally::count` at `:235`; `report` at `:2731`; `prepare_source` at `:364-380`; `run_tree` at `:513`; `walk_into`'s loop at `:656-779`, the two placeholder arms from Task 3; the test `the_tally_counts_each_cause_in_its_own_field` at `:2746`; new tests in `mod tests` with the helpers `tree()` `:2830`, `run()` `:2807`, `opts()` `:2788`, `strict()` `:2801`, `run_full()` `:2818`).
- Modify: `crates/flux-core/src/run/mod.rs:259` (`prepare_source` call), and the test call sites `crates/flux-core/src/tree.rs:3622, 4582, 4662, 4701, 4741, 4836` and `crates/flux-core/src/run/tests.rs:2499`.
- Modify: `crates/flux-cli/src/report.rs` (`Report` at `:22-44`: `mounts_skipped: u64` with `#[serde(skip_serializing_if = "is_zero")]` after `resume_verify`; `Report::zero` at `:47`; `Report::tree` at `:74-93`; `record_lines`' match at `:180-200`; `safety_warning_lines` at `:239-252`; tests at `:597-606`, `:700-712`, `:763-775`).

**Interfaces:**
- Consumes: Task 3's `MountPolicy`, `MountPolicy::of`, `walk_with`, the two events.
- Produces (exact): `pub(crate) fn prepare_source<'a, F: FileSystem>(fs: &'a F, src_root: &'a Path, dst_root: &Path, mounts: MountPolicy) -> Result<Source<'a, F>, CopyError>` (builds the walk with `walk_with(fs, src_root, WalkOptions { max_depth: DEFAULT_MAX_DEPTH, mounts })`); `TreeFailureCause::MountSkipped` (a NOT-failure, as `SymlinkSkipped`); `TreeOutcome.mounts_skipped: u64`, `TreeOutcome.source_mount_unknown: Option<DegradedGroup>`; `Report.mounts_skipped: u64` (JSON key `mounts_skipped`, absent when 0); the stream line `"{path}: object_type=mount action=skipped (pass --cross-filesystems to copy its contents)"`; the warning line `"warning: could not tell whether a source directory is a mount root ({n} checks, e.g. {example}); descended into it - --safety=strict refuses instead"`.

- [ ] **Step 1: Write the failing engine tests** in `tree.rs` `mod tests` (a helper `mounted(fs: &FaultFs, path: &str)` that creates the directory, one file inside it and calls `set_mount_root(path, Yes)`):
  - `a_mount_is_an_empty_directory_reported_once_and_not_a_failure` (test 7 + Review Focus 1): `tree()` plus `mounted("/src/m1")`, `mounted("/src/m2")`; `run(&fs, "/src", "/dst", &opts())`: `fs.exists("/dst/m1")` and `"/dst/m2"` as directories, `!fs.exists("/dst/m1/inside")`; `out.mounts_skipped == 2`; `out.directories_created == 4` (root, `sub`, `m1`, `m2`); `out.files_total == 2`; `out.files_copied == 2`; `out.failures.total() == 0`; `got` has exactly two records, both `TreeFailureCause::MountSkipped`, paths `m1` and `m2`; no `read_dir(/src/m1)` call.
  - `a_mount_whose_directory_cannot_be_created_reports_the_failure_only` (test 8): `mounted("/src/m")`, `fs.create_dir("/dst")`, `fs.write_file("/dst/m", b"x")` (a FILE at the name: the fake's `create_dir` answers `AlreadyExists` for an occupied name and its `open_dir` answers `DestinationError` "not a directory", `fault_fs.rs:1888-1893`, which `enter_dir` reports as `CreateDir` and pushes `Frame::Skipped`, `tree.rs:889-892`); `got.len() == 1` with a `CreateDir` cause at `m`; `out.mounts_skipped == 0`; `!fs.exists("/dst/m/inside")`.
  - `a_mount_under_a_skipped_subtree_is_silent` (test 8, Review Focus 3): `mounted("/src/.flux/operations/m")` under `/src/.flux/operations` (the reserved path, refused at `tree.rs:673`); `got.len() == 1` with the reserved `CreateDir` conflict at `.flux/operations`; `out.mounts_skipped == 0`; `!fs.exists("/dst/.flux/operations/m")`.
  - `a_mount_at_a_reserved_name_is_refused_by_the_reserved_arm` (test 9): `mounted("/src/.flux/operations")` itself: `got` is one `CreateDir` record at `.flux/operations` with `Code::ControlPlaneNamespaceConflict`, `mounts_skipped == 0`, nothing created under `/dst/.flux`. (The walk asks `source_mount_root` before the engine refuses, so that call IS in the log; do not assert its absence.)
  - `a_second_run_merges_into_the_empty_mount_directory` (test 10): run once as test 7's fixture with one mount; run again with `with_existing(ExistingPolicy::Update)` (`:3651`): `mounts_skipped == 1` again, `directories_created == 0` for the mount (the directory pre-exists: assert `out.directories_created == 0`), no error, `!fs.exists("/dst/m/inside")`.
  - `an_unknown_source_mount_warns_under_default_and_refuses_the_operation_under_strict` (test 11 + Review Focus 4): `tree()` plus `fs.set_mount_root("/src/sub", Unknown)`; Default: `run` is `Ok`, `got.is_empty()`, `out.source_mount_unknown == Some(DegradedGroup { count: 1, example: "sub" })`, `fs.exists("/dst/sub/b")`; Strict (`strict()`): `run_full` returns `Err(TreeAbort)` whose `error.code() == Code::SafetyRejected`, `error.to_string()` contains `"cannot be told"`, and `outcome.directories_created >= 2` (the root and `sub`: the exit-1 condition pinned at `exit_code.rs:125`); Review Focus 4 half: `fs.set_mount_root("/src/.flux/operations", Unknown)` with `/src/.flux/operations/f` under Strict: `run` is `Ok` (the directory is under a reserved refusal, so not `live`), `source_mount_unknown.is_none()`.
  - `a_skipped_mount_that_is_the_destination_itself_is_refused_by_identity` (test 11b): `mounted("/src/m")`, `fs.create_dir("/dst")`, `fs.set_identity("/src/m", identity_of(&fs, "/dst"))`; `run` is `Err` with `SafetyRejected` and the message of `tree.rs:837`; `out.mounts_skipped == 0`.
  - Extend `the_tally_counts_each_cause_in_its_own_field` (`:2746`) with `t.count(&TreeFailureCause::MountSkipped)` and the unchanged expected tally.
- [ ] **Step 2: Write the failing report tests** in `report.rs`: extend the `line(..)` test at `:700` with `line("m", TreeFailureCause::MountSkipped) == ["m: object_type=mount action=skipped (pass --cross-filesystems to copy its contents)"]`; `mounts_skipped_is_its_own_count_and_absent_from_json_at_zero` (`out.mounts_skipped = 2` → `Report::tree(&out, false, 10).mounts_skipped == 2`, `files_skipped` unchanged by it, `serde_json::to_value(&r)["mounts_skipped"] == 2`; a zero outcome's JSON object has no `mounts_skipped` key and the same key count as `Report::zero()`'s today); extend `both_safety_warnings_render_after_the_identity_ones` (`:763`) with `out.source_mount_unknown = Some(DegradedGroup { count: 3, example: "src-sub" })` and a line containing `"source directory"`, `"3 checks"`, `"e.g. src-sub"`, `"descended"`, `"--safety=strict"`, ordered right after the destination mount warning.
- [ ] **Step 3: Run** `cargo test -p flux-core --lib tree::` and `cargo test -p flux-cli --bin flux report::` → compile errors (the variant, the fields, the signature).
- [ ] **Step 4: Implement**: the `TreeOutcome` fields and the variant with doc comments in the `SymlinkSkipped` style (`:186-188`); `FailureTally::count` gains `TreeFailureCause::MountSkipped => {}` with the comment `// Not a failure (section 42): report counts it in TreeOutcome::mounts_skipped.`; `report` (`:2731`) adds `if matches!(cause, TreeFailureCause::MountSkipped) { out.mounts_skipped += 1; }`; `prepare_source` takes `mounts: MountPolicy` and calls `walk_with`; `run_tree` and `run/mod.rs:259` pass `MountPolicy::of(opts)`; the seven test call sites (`tree.rs:3622, 4582, 4662, 4701, 4741, 4836`, `run/tests.rs:2499`) ALL pass `MountPolicy::Cross`, which keeps each of those tests exactly as it was before this cut (the new mount behaviour is pinned by Task 3's and this task's new tests, not by retrofitting old ones); the two arms in `walk_into` replace Task 3's placeholder: `WalkEvent::MountSkipped { path } => { if live { report(out, on_report, path, TreeFailureCause::MountSkipped); } }` and `WalkEvent::MountUnknown { path } => { if live { match cx.opts.safety { Safety::Default => DegradedGroup::note(&mut out.source_mount_unknown, &path), Safety::Strict => return Err(refuse("whether a source directory is a mount root cannot be told with full confidence")), } } }`. `report.rs`: the field with `fn is_zero(n: &u64) -> bool { *n == 0 }`, `Report::tree` sets `r.mounts_skipped = out.mounts_skipped`, the `record_lines` arm beside `SymlinkSkipped` (`:190`), the warning pushed in `safety_warning_lines` right after the `mount_unknown` block (`:241-247`).
- [ ] **Step 5: Run** `TMPDIR=$HOME/ff_tmp just check` → every test passes; `cargo clippy --workspace --all-targets --release -- -D warnings` clean. Mutant checks (restore after): delete the `out.mounts_skipped += 1` line → test 7 red; make the Strict arm fall through to the Default one → test 11 red.
- [ ] **Step 6: Commit** `feat(core): skipped source mounts as empty directories, MountSkipped records, the Unknown policy, and the report lines (cut 10b)`.

### Task 5: `--cross-filesystems` on the CLI and in the resume configuration

**Files:**
- Modify: `crates/flux-cli/src/main.rs` (`CopyArgs` at `:53-112`: the flag after `links` at `:110-111`; `options()` at `:147-178`: `cross_filesystems: args.cross_filesystems` beside `links`).
- Modify: `crates/flux-core/src/state.rs` (`Options` at `:76-87`: the field after `links`; `Options::of` at `:100-132`; `canonical_bytes` at `:137-146`; tests beside `:2037-2089`).
- Modify: `crates/flux-core/src/run/resume.rs` (the comparison list at `:96-103`; a test beside `each_exact_match_option_is_compared_and_named` at `:292`).
- Modify: `crates/flux-core/src/run/tests.rs` (a test beside `resume_with_a_different_links_setting_is_incompatible` at `:3191`; `a_pre_cut_state_file_resumes_as_copy` at `:3206` is UNCHANGED and is the pre-cut-fixture pin).
- Modify: `crates/flux-cli/tests/copy.rs` (a test beside `links_follow_is_a_usage_error` at `:647`).

**Interfaces:**
- Produces: CLI `--cross-filesystems` (bool, default off; doc: "Descend into directories that are other mounts (section 42); by default each is created empty and skipped"); `state::Options.cross_filesystems: bool` with `#[serde(default, skip_serializing_if = "is_false")]` and `fn is_false(v: &bool) -> bool { !*v }`; `canonical_bytes` appends `cross_filesystems=true\n` after the `links=skip` line only when true; the resume comparison row `("cross_filesystems", then, now)` rendered as `"true"`/`"false"` (the list at `:96-103` holds `&String`s: build the two strings before the loop, or add a second small loop for the bool; the message format at `:104-108` is reused verbatim).

- [ ] **Step 1: Write the failing tests**: `state.rs`: `a_default_state_is_written_without_a_cross_filesystems_key_and_a_crossing_state_with_one` (mirror `:2073-2089`: the default `tree_v3()` JSON options object has no `cross_filesystems` key and still exactly 5 keys; with `cross_filesystems = true` the key is `true`; both round-trip through `decode`); `a_state_without_cross_filesystems_deserializes_as_false` (the literal JSON at `:2060` parses with `cross_filesystems == false` and validates); extend `a_copy_links_fingerprint_is_the_four_line_one_and_skip_adds_a_line` (`:2037`): the default `canonical_bytes` and fingerprint `6814cb38...` are unchanged; `Options { cross_filesystems: true, ..o.clone() }.canonical_bytes()` ends with `b"...existing=overwrite\ncross_filesystems=true\n"` and its fingerprint differs. `resume.rs`: `a_different_cross_filesystems_setting_is_incompatible_and_named` (a prior with `cross_filesystems: true` against `opts()` (false), and the reverse: `LockCode::IncompatibleState`, detail starts with `"cross_filesystems:"`, contains `"--restart"`). `run/tests.rs`: `resume_with_a_different_cross_filesystems_setting_is_incompatible` (mirror `:3191-3200` with `CopyOptions { cross_filesystems: true, ..opts() }`; detail starts with `"cross_filesystems:"`; the manifest is unchanged). `crates/flux-cli/tests/copy.rs`: `cross_filesystems_is_accepted_and_json_omits_the_mount_count_at_zero` (Review Focus 5 + test 15): `flux copy src dst --cross-filesystems --json` exits 0, `files_copied == 2`, the JSON object has no `mounts_skipped` key and `KEYS.len()` keys (as `json_reports_every_section_53_field` at `:273` asserts for the default run).
- [ ] **Step 2: Run** the four test binaries → compile errors / failures.
- [ ] **Step 3: Implement** the flag, the mapping, the state field, `Options::of`, `canonical_bytes`, the resume row.
- [ ] **Step 4: Run** `TMPDIR=$HOME/ff_tmp just check` → every test passes; release clippy clean. Mutant (restore after): remove `skip_serializing_if` → the first state test red.
- [ ] **Step 5: Commit** `feat(cli): --cross-filesystems, recorded in the resume configuration only when set (cut 10b)`.

### Task 6 (OWNER-GATED, controller-run): the timing comparison

**Files:** none committed. A one-line LOCAL edit of `benches/copy.rs:27-36` (`cross_filesystems: true`) for the "on" runs, reverted before anything else.

- [ ] **Step 1: Ask the owner** for the OK to measure, naming the command; do nothing until it arrives.
- [ ] **Step 2 (controller, top level, no other tool calls, no subagents running):** check the field is clear (`pgrep -af "cargo bench|flux"`, kill stale ones); run `cargo bench --bench copy -- small_1000x4kib --warm-up-time 2 --measurement-time 10` TWICE with the bench's `options()` as committed (`cross_filesystems: false`, the default `Stay`), then make the local edit to `true` (`Cross`, the pre-cut path) and run it TWICE; revert the edit (`git checkout -- benches/copy.rs`); quote the WARM figure and the spread per arm, and state that the background load was uncontrolled.
- [ ] **Step 3: Record** the two ranges in the plan's ledger and in spec section 9 (Task 7 commits it). A difference larger than the spread is a finding for the owner, not a reason to change the default.

### Task 7: Documentation and the deviation record

**Files:**
- Modify: `TODO.md:265-273` (the section 42 item: mark its SOURCE half done by this cut, naming `--cross-filesystems` and the `MountSkipped` record; keep the destination-half text and the known limit 1 pointer as they are); `TODO.md` new section `## Cut 10b test debt` listing every reviewer minor the SDD run deferred.
- Modify: `ROADMAP.md:46` (row 7: replace "Mount boundaries during the walk (§42) are not enforced (TODO.md)" with "Source mount boundaries (§42, `--cross-filesystems`) are enforced by the walk (cut 10b); a destination-side bind mount of a source directory stays known limit 1 of cut 8a").
- Modify: `docs/accepted-boundaries.md` (a new `## Cut 10b - source mount boundaries (2026-10-17)` section after `:77`: the three UNVERIFIED-ACCEPTED boundaries of the spec's Stand-downs: autofs trigger directories; a Windows mounted-volume folder under a copied tree; `STATX_ATTR_MOUNT_ROOT` on a real bind mount, each with its compensation (`AT_NO_AUTOMOUNT`; the reparse-point classification at `std_fs.rs:612-634`; the `/proc` and `/dev/shm` platform tests)).
- Modify: `docs/superpowers/specs/2026-10-17-cut-10b-mount-boundaries-design.md` section 9 (every `Ruling:` of the SDD ledger; the timing ranges of Task 6; the JSON `mounts_skipped` field as the additive deviation from the master spec's example at `FLUX_FULL_UPDATED_SPEC_V16.md:2821`, D8).

- [ ] **Step 1: Write the four edits.**
- [ ] **Step 2: Run** `TMPDIR=$HOME/ff_tmp just check` → clean (the gate includes `typos`; there is no separate tracker-format recipe in this repo).
- [ ] **Step 3: Commit** `docs: cut 10b rulings, TODO item closure, roadmap row, accepted boundaries, deviation record`.

## Ledger of rulings (filled during execution)

(Each `Ruling: <what> - <why> - <what it costs if wrong>` is copied here from the SDD ledger at finish.)

## Self-review (writing-plans)

1. Spec coverage: 3.1 → Task 1; 3.2 → Tasks 1-2; 3.3 → Task 3; 3.4 → Task 4; 3.5 → Task 5 (the `CopyOptions` field is Task 1, so Task 3's `MountPolicy::of` compiles); 3.6 → Task 4; section 5's rows → tests 7, 8, 3-6, 11, 11b, 3-4 and the resume test of Task 5; section 7's tests 1-16 → 3-1..3-7, 7-11b, 12 (Task 5), 13-14 (Task 2), 15 (Task 5), 16 (the gate); the performance note → Task 6; section 9 → Task 7.
2. Step scan: every code step names its signature and file; the only bodies given are the `next`/`enter` insertions (an algorithm the tests do not determine on their own: the order and the frame flag) and the two engine arms (the Strict/Default split is a contract).
3. Type consistency: `MountPolicy`, `WalkOptions`, `walk_with`, `MountNote`, `MountSkipped`/`MountUnknown`, `mounts_skipped`, `source_mount_unknown`, `TreeFailureCause::MountSkipped`, `cross_filesystems` are spelled the same in every task; `prepare_source`'s new fourth parameter is `mounts: MountPolicy` in Task 4 and its callers.
4. Review Focus: five items, each pinned in a named test (Tasks 3, 4, 5).
5. Proportion: the plan is about a third of the spec's length; code blocks are signatures and two short insertions.
