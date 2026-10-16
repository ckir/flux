# Cut 10b: source mount boundaries (design)

Status: draft for the adversarial panel. Written 2026-10-17 against `main` at `a563613` (cut 10a merged). Every `file:line` below was read at that commit.
Master spec: `FLUX_FULL_UPDATED_SPEC_V16.md` section 42 (lines 2496-2506: "do not cross filesystem boundaries", "Crossing is off unless `--cross-filesystems` is given (Section 5)", "Filesystem identity must be used to evaluate boundaries"), section 5 option list (line 741), item 72 (line 223), item 70 (line 1482, `--cross-filesystems` is part of the resume selection), axiom 22 (line 528).
Prior art in the repo: `TODO.md:265-273` (the open item this cut closes: "the walk descends into a directory on another volume ... would read `/proc` inside a copied tree"), `TODO.md:276-290` (the destination half, cut 8a Part M), `TODO.md:56-64` (the measured bind mount `mount --bind a a/b`: `a` and `a/b` share `dev` and `ino`).
Consult record: AGY-FIRST `.clavity/seams/next-cut-fork-2.md` (which cut), `10b-fork-1.md` (detection and destination behaviour), AGY-NEGOTIATE `10b-neg-1.md` (the destination behaviour, settled by measurement); the owner approved the design on 2026-10-17.

## 1. Purpose, goals, non-goals

`flux copy SRC DEST` today descends into every directory it finds, including a directory that is the root of another mount (`/proc`, `/sys`, an NFS share, a bind mount). Section 42 makes "stay on the source's filesystem" the default. This cut adds that rule to the SOURCE walk and the `--cross-filesystems` opt-out.

Goals:
- G1. A source directory that is the root of a mount is NOT descended into by default: nothing under it is listed or stat'ed.
- G2. The destination gets an EMPTY directory at the mount point's name, as `cp -x`, `rsync -x` and `tar --one-file-system` produce (MEASURED 2026-10-17 on `/dev/shm`, a tmpfs mounted inside the devtmpfs `/dev`: all three leave an existing empty `shm`; `man rsync` documents it under `--one-file-system`, "it includes an empty directory at each mount-point it encounters (using the attributes of the mounted directory ...)", and `info cp` says "mount point directories _are_ copied"). The directory's own attributes are whatever flux gives ANY directory it creates today: a tree copy applies no directory metadata (`docs/superpowers/specs/2026-09-26-cut-4b-copy-tree-design.md:155`, "directories keep fresh timestamps"), so the created directory has the process defaults. When directory metadata is built (a later cut), the source for a mount is the mounted root's, which is what rsync documents; that is outside this cut.
- G3. A skipped mount is reported, never silent: one stream line per mount and a counter, and it is not a failure (exit 0).
- G4. `--cross-filesystems` restores today's behaviour exactly.
- G5. The detection uses the kernel's mount knowledge, not a device-number comparison alone, because a same-filesystem bind mount shares `st_dev` with its parent (`TODO.md:58-63`) and a btrfs subvolume has a different `st_dev` without being a mount (`TODO.md:285-288`).

Non-goals (each a deliberate boundary):
- N1. The destination side: cut 8a's Part M (`tree.rs:911-937`, a pre-existing destination mount root is never merged into) is unchanged.
- N2. No `-xx`-style mode that omits mount points entirely (rsync's repeated `-x`); a future item.
- N3. A symlink or junction that POINTS into another mount is not a mount boundary (a link is not followed at all, cut 10a).
- N4. No new third-party crate. No change to the walk's path-based design (the TOCTOU between the listing and the stat is the existing, recorded one).
- N5. Windows mounted-volume folders are not newly handled: the source walk already classifies them as links/special files (section 6.3).

## 2. Invariants

- M1 (nothing read under a mount): for a skipped mount directory `D`, the walk performs no `read_dir` and no `metadata` of any path below `D`, and yields no event for any such path.
- M2 (the root is the anchor): the walk root is never skipped, whether or not it is itself a mount root, and the boundary is evaluated for every directory strictly below it, each against its own parent.
- M3 (an ordinary empty directory): the destination entry for a skipped mount is created through the SAME code as any directory (`enter_dir`, claims, reserved-name refusal, `directories_created`), counted in `directories_created` and never in any `files_*` counter; it has the attributes any created directory has (G2).
- M4 (opt-out is identity): with `--cross-filesystems` the walk performs the same calls, yields the same events and the engine produces the same report as before this cut, including the existing `directory cycle: already an ancestor` error for `mount --bind a a/b`.
- M5 (nothing else changes, differential): a tree with no mount below its root produces the same stderr, the same JSON and the same on-disk state as before. A copy-mode state file written without the new key is byte-identical to the pre-cut format.
- M6 (one outcome per mount): each skipped mount yields exactly one `MountSkipped` record and one `mounts_skipped` increment, and none when its directory could not be created (the failure is already reported once, as for a skipped subtree).
- M7 (no silent degradation): a mount query that cannot tell (`Unknown`) is either reported (default) or refused (`--safety=strict`); it is never treated as "not a mount" without a trace.

## 3. Design

### 3.1 The trait method (`flux-fs`)

`crates/flux-fs/src/fs.rs` gains, on `FileSystem` (the path-based trait the walk uses; `metadata` is at `fs.rs:166`):

`fn mount_root(&self, path: &Path, parent: &Path) -> Result<MountRoot>`

It reuses the existing `MountRoot { Yes, No, Unknown }` (`fs.rs:476`). `path` is a directory just stat'ed (`FileType::Dir`, never a link); `parent` is its containing directory as a path. The method NEVER follows a final-component link. It is REQUIRED (no default): a default of `No` would silently disable the rule on an implementer that forgot it, and the compiler is the oracle for the four implementers (`StdFileSystem`, `FaultFs`, the walk tests' `EscapingFs`, the `NullFs` in `fs.rs`'s tests). The doc comment says what the destination method's does: `Unknown` only where the platform cannot tell.

### 3.2 Platform implementations

- Linux (`crates/flux-platform/src/std_fs.rs`, `impl FileSystem for StdFileSystem`, line 242): `statx(CWD, path, AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT)`; the query must never trigger an automount, since deciding to skip a mount must not mount it. An autofs trigger directory is UNMEASURED in this cut (section 6.1). When the returned `stx_attributes_mask` contains `STATX_ATTR_MOUNT_ROOT` the answer is its `stx_attributes` bit (`Yes`/`No`). Otherwise (kernel before 5.8, or a filesystem that does not report it, or `ENOSYS`/`ENOTSUP`) compare `st_dev` of `lstat(path)` with `lstat(parent)`: different is `Yes`, equal is `Unknown`. This is the logic of `DirHandle::mount_root` (`dir_unix.rs:519-556`) re-expressed on paths; the two are kept in step by a shared helper if the plan finds one that does not widen visibility, else by tests that pin both (section 7, test 13). Other `statx` errors are the query's own failure (`Err`).
- macOS: `lstat` device comparison: different is `Yes`, equal is `No` (macOS has no same-filesystem bind mount; the destination arm says the same, `dir_unix.rs:548`).
- Windows: always `Ok(MountRoot::No)`, with the reason the destination arm gives (`dir_windows.rs:152-156`): a mounted-volume folder is a reparse point, which the SOURCE side already never descends into either (section 6.3).
- `FaultFs` (`crates/flux-core/src/fault_fs.rs`): answers from the existing `mount_roots` map (`fault_fs.rs:45`, set by `set_mount_root`, `fault_fs.rs:1186`) keyed by the normalised path of the directory, default `No`, and records the call as `source_mount_root(<path>)` so a test can tell it from the destination method's `mount_root(<path>)` (`fault_fs.rs:1825`).

### 3.3 The walk (`crates/flux-core/src/walk.rs`)

`Walk` gains a boundary policy. New public items:
- `pub enum MountPolicy { Cross, Stay { unknown: UnknownMount } }` and `pub enum UnknownMount { Descend, Refuse }`.
- `pub fn walk_with(fs, root, WalkOptions { max_depth, mounts })`; `walk` and `walk_with_depth` keep their signatures and mean `MountPolicy::Cross`, so every existing caller (the destination sweep at `run/sweep.rs:36` and the walk's own tests) is unchanged (M4, M5).

`Walk::enter` (`walk.rs:284`) order, after the depth cap and the `metadata`/type check (the `DirectoryChangedDuringScan` refusal) and BEFORE the identity/ancestor logic:
1. With `MountPolicy::Stay`, ask `fs.mount_root(&abs, &abs.parent())` (never for the root: `enter` is only called for descendants, and the root frame is built in `walk_with`, M2).
2. `Err(e)`: the query failed, which is the directory's own failure: `Err(WalkError { path: rel, cause: e })`, subtree skipped, as a failed `read_dir` is (`walk.rs:357-366`).
3. `Ok(No)`: continue exactly as today.
4. `Ok(Yes)`: do NOT push the ancestor, do NOT call `read_dir`. Push a frame with no entries and a flag `mount: Skipped`, `emit_end: true`, and return `Dir { path, identity }` with the MOUNTED ROOT's identity, as for any directory. `Walk::next` then yields `MountSkipped { path }` once, then (the frame being empty) `DirEnd { path }`.
5. `Ok(Unknown)`: with `UnknownMount::Descend` continue as today and additionally yield `MountUnknown { path }` right after `Dir`; with `Refuse`, `Err(WalkError { path: rel, cause: SafetyRejected "whether this directory is a mount root cannot be told with full confidence" })`, the subtree skipped.

Running the check before the ancestor logic is load-bearing: for `mount --bind a a/b` the directory `a/b` has `a`'s identity, so today it is a `directory cycle` error; with the check first it is a skipped mount (an empty directory), which is the correct reading of a bind mount; with `--cross-filesystems` the check is not made and the cycle error is unchanged (M4).

`WalkEvent` gains two variants, `MountSkipped { path: PathBuf }` and `MountUnknown { path: PathBuf }`, both emitted only after the directory's own `Dir` event and before its `DirEnd`. The compiler names every `match` on `WalkEvent` (the engine, the walk's tests, any helper).

### 3.4 The engine (`crates/flux-core/src/tree.rs`)

- One helper, `MountPolicy::of(opts: &CopyOptions) -> MountPolicy` (in `walk.rs`, so there is a single place that maps the option and `Safety` onto the policy): `Cross` when `cross_filesystems`, else `Stay { unknown: Refuse }` under `Safety::Strict` and `Stay { unknown: Descend }` under `Safety::Default`.
- `prepare_source` (`tree.rs:364`) takes the policy (`MountPolicy::of(opts)` at both production callers) and builds the walk with `walk_with`; its callers are `run_tree` (`tree.rs:513`), `run/mod.rs:259`, and six test sites (`tree.rs:3622, 4582, 4662, 4701, 4741, 4836`, `run/tests.rs:2499`).
- The event loop (`tree.rs:656`) gets two arms. `WalkEvent::MountSkipped { path }`: if the top-of-stack frame is `Frame::Live` (the mount's own directory was created, i.e. `live` as computed at the top of the loop), `report(out, on_report, path, TreeFailureCause::MountSkipped)`; otherwise nothing. `WalkEvent::MountUnknown { path }`: if live, `DegradedGroup::note(&mut out.source_mount_unknown, path)`.
- `TreeFailureCause::MountSkipped` is a new NOT-failure variant, exactly as `SymlinkSkipped` (`tree.rs:188`, counted at `tree.rs:2731`): `FailureTally::count` (`tree.rs:235`) names it with an empty arm so the match stays exhaustive, and `report` increments the new `TreeOutcome::mounts_skipped`.
- `TreeOutcome` gains `mounts_skipped: u64` and `source_mount_unknown: Option<DegradedGroup>` (beside `mount_unknown`, `tree.rs:54`, which keeps meaning the destination query).
- The empty directory is created by `enter_dir` (`tree.rs:821`) from the `Dir` event, so the reserved-name refusal, claims, the destination Part M and A checks and `directories_created` apply unchanged (M3). At `DirEnd` the frame's empty batch flushes (`tree.rs:757-770`); no directory metadata is applied, as for every directory today (G2).

### 3.5 Options, CLI, state

- `flux-fs`: `CopyOptions` (`options.rs:85`) gains `pub cross_filesystems: bool` ( every struct literal is named by the compiler: this cut's plan pre-lists them and checks both axes after, as cut 10a's did).
- CLI (`crates/flux-cli/src/main.rs`): `--cross-filesystems`, a boolean flag, default off, documented "Descend into directories that are other mounts (section 42); by default each is created empty and skipped". Mapped to `CopyOptions.cross_filesystems` beside `links` (`main.rs:164`). The policy is derived by `MountPolicy::of` (section 3.4).
- State (`crates/flux-core/src/state.rs`): `Options` gains `cross_filesystems: bool` with `#[serde(default, skip_serializing_if = "is_false")]` (`state.rs:75-87` is the `links` pattern), so a state written without it reads `false` and a default-mode state is byte-identical to the pre-cut format (M5; the pre-cut binary's `deny_unknown_fields` still reads it). `Options::of` (`state.rs:100`) sets it; `canonical_bytes` (`state.rs:137`) appends `cross_filesystems=true\n` ONLY when true, so every existing fingerprint is unchanged (cut 10a's `links=skip` rule). `validate` (`state.rs:590` is where `links` is checked) needs no new check for a bool.
- Resume (`run/resume.rs:96-103`; master spec item 70, `FLUX_FULL_UPDATED_SPEC_V16.md:13324-13326`: "resuming with a different ... --cross-filesystems ... is INCOMPATIBLE_STATE"): one row `("cross_filesystems", then, now)` in the comparison list, rendered "true"/"false", so `--resume` with a different value is `INCOMPATIBLE_STATE` naming `cross_filesystems` (spec line 1482: the selection options are part of the resume identity).

### 3.6 Reporting (`crates/flux-cli/src/report.rs`)

- Stream line for `TreeFailureCause::MountSkipped` (beside the `SymlinkSkipped` arm, `report.rs:190`; no error code, neither an error nor a warning): `<path>: object_type=mount action=skipped (pass --cross-filesystems to copy its contents)`. The fields follow section 124's pattern for a skipped object (`FLUX_FULL_UPDATED_SPEC_V16.md:5968-5972`).
- JSON `Report` gains `mounts_skipped`, serialised only when non-zero (`#[serde(skip_serializing_if = ...)]`, as `resume_verify` is, `report.rs:42`), so a tree without mounts has byte-identical JSON (M5). It is NOT added into `files_skipped` (`report.rs:76-80`), because a mount is a directory.
- Warning for `source_mount_unknown`, beside the destination's (`report.rs:241-246`): `warning: could not tell whether a source directory is a mount root (N checks, e.g. PATH); descended into it - --safety=strict refuses instead`.
- Exit code: a skipped mount is not a failure (`FailureTally` does not count it): exit 0. `Unknown` under strict is a `Walk` failure on that directory: exit 1 as any refused subtree.

## 4. Edge cases and decisions made explicitly

- The walk root is a mount root (`flux copy /mnt/usb DEST`, `flux copy / DEST`): copied normally (M2). Mounts BELOW it are skipped.
- A mount nested inside a skipped mount is never seen (M1).
- A mount whose point is a directory the destination already has: the existing `enter_dir` pre-existing path applies (Part M asks about the DESTINATION directory, which is an ordinary directory on the target filesystem, so it answers `No` and the empty directory is merged into); `--update` and `--resume` leave it untouched, there are no claims or notes for it.
- A mount point whose mounted root is unreadable or has odd permissions: the directory create or finalise can fail like any directory's; that is a normal `CreateDir` failure.
- `--links=skip`/`copy` and mounts are independent. A symlink to a mount is a link (cut 10a); a bind-mounted file (a file mount) is not a directory and is not in scope (the walk only asks about directories).
- `reserved_path`: a mount at `.flux/operations` is refused by the existing `Dir` arm before any mount event matters (`tree.rs:673`).
- Case-folding destinations, long names, ordering: unchanged (the mount directory is an ordinary entry in the sorted listing).

## 5. Failure table

| Situation | Walk | Engine | Report | Exit |
|---|---|---|---|---|
| Mount below the root, default | `Dir`, `MountSkipped`, `DirEnd`; no listing below | empty dir created, `mounts_skipped` + 1 | one stream line | 0 |
| Same, `--cross-filesystems` | as before | as before | none new | as before |
| Mount, its directory create fails | `Dir`, `MountSkipped`, `DirEnd` | `CreateDir` failure; no `MountSkipped` report | the create failure | 1 |
| Mount query errors (`Err`) | `WalkError` for that path | `Walk` failure; subtree skipped | the error | 1 |
| Query `Unknown`, safety default | `Dir`, `MountUnknown`, children | `source_mount_unknown` noted | warning | 0 |
| Query `Unknown`, strict | `WalkError` (SafetyRejected) | `Walk` failure; subtree skipped | the refusal | 1 |
| Bind mount of an ancestor (`a/b`), default | skipped mount (no cycle error) | empty dir | one stream line | 0 |
| Resume with a different `--cross-filesystems` | n/a | `INCOMPATIBLE_STATE` naming the option | the refusal | 3 |

## 6. Platform notes

6.1 Linux: autofs/automount points are not measured (no privilege to build one here): with `AT_NO_AUTOMOUNT` the query does not mount, and whether an unmounted trigger directory then reads as a mount root, or `read_dir` mounts it on descent, is recorded as an unmeasured boundary. The kernel's `STATX_ATTR_MOUNT_ROOT` (5.8+) makes a same-filesystem bind mount detectable; the fallback cannot see one (`Unknown`), and on a pre-5.8 kernel a btrfs subvolume reads as `Yes` (a known false positive, `TODO.md:285-288`), so its subtree is skipped and reported.
6.2 macOS: `st_dev` comparison.
6.3 Windows: `std::fs::read_dir` types a junction or volume mount point as a link (`std_fs.rs:612-624`), `read_link` answers `SpecialFileUnsupported` for it (cut 10a, D7), and the engine reports it as a skipped special file; so a Windows source mount point is already never descended into and the new method is `No`. NOT MEASURED in this cut: a mounted-volume folder under a copied tree on a real Windows machine. Recorded as an unmeasured boundary.

## 7. Tests

Walk (`walk.rs` `mod tests`, with `FaultFs::set_mount_root`):
1. A mount below the root yields `Dir`, `MountSkipped`, `DirEnd` in that order and the call log has no `read_dir` or `metadata` of anything below it (M1). Mutant: the walk still lists the mount.
2. The root marked as a mount is walked (M2). Mutant: the check applied to the root.
3. `Cross` policy: the same fixture produces today's events and no `source_mount_root` call (M4).
4. The bind-mount fixture (`set_identity` reproduces `mount --bind a a/b`, as the existing cycle test does, `walk.rs:572`): with `Stay` it is a skipped mount; with `Cross` it is the cycle error. Mutant: the mount check after the ancestor logic.
5. `Unknown` with `Descend` yields `MountUnknown` after `Dir` and then the children; with `Refuse` a `WalkError` and no children.
6. A failing `mount_root` is a `WalkError` and the walk continues with the next sibling.
Engine (`tree.rs` `mod tests`):
7. The empty directory exists at the destination, `directories_created` counts it, `mounts_skipped == 1`, `files_total` unchanged, the stream has one `MountSkipped`, exit-relevant `failures.total() == 0` (M3, M6). Mutant: `report` does not count it.
8. A mount whose directory could not be created yields no `MountSkipped` report (M6). A mount below a Skipped frame is silent.
9. A mount at `.flux/operations` is refused by the reserved arm.
10. `--update`/second run: the existing empty directory is merged into, nothing changes, no new claim.
11. `source_mount_unknown` is noted under Default and is a failure under Strict (end to end through `copy_tree`).
State and resume:
12. `cross_filesystems` omitted from a default-mode state's JSON; written when true; a literal pre-cut `state.json` fixture still resumes; the pre-cut fingerprint is unchanged; `--resume --cross-filesystems` after a default run is `INCOMPATIBLE_STATE` naming the option, and the reverse.
Platform (`crates/flux-platform/tests/`, Linux/macOS):
13. `StdFileSystem::mount_root` answers `No` for an ordinary directory (a `TempDir` child, against its parent) and agrees with `DirHandle::mount_root` on the same directory. A real mount point needs no privilege to ASK about: on Linux `/proc` is a mount root and `/` is its parent, so `mount_root("/proc", "/")` is asserted `Yes` (skipped with a stated reason where `/proc` is absent), and `/dev/shm` against `/dev` likewise where present. Mutant: the `STATX_ATTR_MOUNT_ROOT` branch and the device fallback each replaced by `No` (each must turn a test red; the fallback is exercised by a test double of the attribute-less answer in the helper, or by `Unknown` on an equal-device pair).
14. Windows: `mount_root` is `No` (compile-checked and run on the Windows job).
CLI (`crates/flux-cli/tests/copy.rs`): 15. `--cross-filesystems` is accepted; JSON has `mounts_skipped` only when non-zero; the stderr line text; exit 0. A real-process mount is not creatable without privilege, so this uses the fake only through the library tests (tests 7-11); the CLI test pins the flag, the line text through `report::` unit tests, and the JSON omission.
Differential: 16. The existing tree corpus passes unchanged (M5).

Performance: the default policy adds one `statx` per directory (never per file) to a walk that already does one `lstat` and one `read_dir` per directory. Section 76 of the master spec requires a regression to be benchmarked before and after; the plan carries one owner-approved timing comparison (`--cross-filesystems` on versus off on the many-directories bench), run by the controller at top level on an idle machine, never delegated.

## 8. Decisions made (D) for the owner to check

- D1. Scope: the SOURCE walk only; Part M stays (N1).
- D2. A new REQUIRED `FileSystem::mount_root(path, parent)`; reuses `MountRoot`; not a device-number comparison alone (G5).
- D3. A skipped mount is an EMPTY directory, measured against three tools (G2); agy first recommended creating nothing, withdrew it after the measurement. Its attributes are the process defaults, like every directory flux creates until directory metadata exists.
- D4. Two new `WalkEvent` variants, emitted between `Dir` and `DirEnd`; `walk`/`walk_with_depth` keep their meaning (`Cross`).
- D5. The mount check runs before the ancestor logic, so a bind mount of an ancestor is a skipped mount, not a cycle error (with the flag, unchanged).
- D6. `Unknown` mirrors Part M: Default descends and warns, Strict refuses.
- D7. `--cross-filesystems` is a bool in `state::Options`, written only when true, in the fingerprint only when true, and a resume mismatch is `INCOMPATIBLE_STATE`.
- D8. Report: a stream line without an error code, a JSON `mounts_skipped` only when non-zero (an additive field the master spec's JSON example, `FLUX_FULL_UPDATED_SPEC_V16.md:2821`, does not list: recorded in section 9 at merge), not added to `files_skipped`; exit 0. No human summary line in this cut: the stream line and the JSON field are the report.
- D9. Windows `No` by the existing classification; unmeasured for mounted-volume folders.
- D10. Out of scope: `-xx` omit mode; destination-side bind mounts.

## 9. Deviations and amendments recorded at merge

(to be filled at merge)

## 10. Exhaustiveness self-audit

- What: the trait method and its four implementers; the platform logic per OS; the policy type and `walk_with`; the check order; both new events and their position; the engine arms, the new cause, the two outcome fields; the option, CLI flag, state key, fingerprint rule and resume row; the stream line, the JSON field, the warning; the exit code; the failure table; the tests.
- Defaults stated: `--cross-filesystems` off; `UnknownMount::Descend` under Default safety, `Refuse` under Strict; `mounts_skipped` omitted from JSON at zero.
- Placeholders: none. Open MECHANISM left to the plan: whether the Linux path helper can be shared with `dir_unix.rs` without widening visibility (section 3.2); the exact summary text for the human report (not a contract; only the stream line and the JSON field are).
- Cases: sections 4 and 5 cover the root, nesting, an existing destination directory, a failed create, a failed query, `Unknown` both ways, the bind mount, reserved names, resume, case-folding.
- Goals map: G1 to 3.3 and 3.4, G2 to 3.4 and D3, G3 to 3.6, G4 to 3.3 (`Cross`) and M4, G5 to 3.2.
- Not verified from a file: kernel behaviour of `STATX_ATTR_MOUNT_ROOT` for a bind mount (reasoned from the destination arm's use of it and the kernel documentation, and not measured here without privilege); Windows mounted-volume folders (section 6.3); macOS device semantics (taken from the destination arm).
