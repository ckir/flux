# Accepted test boundaries

Behaviours deliberately NOT covered through a given harness, because covering them there needs a brittle
mock or a seam that does not exist, and something else compensates. Written by AGY-TEST-AUDIT; each entry
is do-not-re-raise ONLY while its compensation still exists - a later audit re-checks the anchor first,
and an entry whose compensation is gone becomes a live gap again. Closed entries are removed. Deferred
gaps do not live here (they ride `.clavity/local-anomalies.md` to a tracked item).

One section per audited change, newest at the bottom.

## Cut 5 - the `flux copy` CLI for trees (test audit 2026-09-26)

- **The weak-identity warning lines are not wired end-to-end.** `crates/flux-cli/src/main.rs` prints
  `report::warning_lines(&outcome.warnings)` after a tree copy and `report::file_warning(..)` after a
  single-file copy; deleting either loop leaves every test green (measured: 45/45 on Windows). An
  end-to-end test needs a filesystem that reports a weak or unavailable identity, and the build host's
  filesystems report Strong ones; reaching it otherwise needs a seam that lets `main` run against
  `FaultFs`, which this cut does not have.
  **Compensation:** the text of both forms is pinned by `report.rs`
  `one_warning_per_weak_volume_and_one_for_unavailable` and `a_single_file_warning_names_the_target`;
  the wiring is one `for` loop and one `if let` in `copy`, each writing through the same `err` as every
  pinned line. Owner-accepted 2026-09-26.

## Cut 7a Part 1 - the destination lock's primitives (test audit 2026-09-29)

- **`O_NONBLOCK` on the Unix `open_lock` is not pinned.** `crates/flux-platform/src/dir_unix.rs` (`open_lock`) opens
  with `OFlags::NONBLOCK` so that a FIFO planted at a lock name cannot hang the open; removing the flag leaves every
  test green (measured under `just check-linux`: `open_lock_refuses_a_fifo_without_hanging` still passes), because
  an `O_RDWR` open of a FIFO does not block on Linux or macOS, the only Unix systems CI runs.
  **Compensation:** `open_lock_refuses_a_fifo_without_hanging` pins the refusal itself (a FIFO is
  `DestinationError`), and the flag carries a comment naming the case it guards. Owner-accepted 2026-09-29.
- **`write_at_start`'s short-write error is not reached.** `crates/flux-platform/src/lock_file.rs` returns
  `short_write()` when the one write call writes fewer bytes than asked; no test can make a regular file on a local
  filesystem short-write 4096 bytes without fault injection below `std::fs::File`, which the platform crate has no
  seam for.
  **Compensation:** the check is one comparison against `bytes.len()` on each arm, and the fake's `write_at_start`
  gives Part 2's protocol tests their own fault injection (`write_at_start` and `lock_set_len` are recorded calls).
  Owner-accepted 2026-09-29.

## Cut 7b Part 2 - the heartbeat (test audit 2026-10-03)

- **A heartbeat failure is not driven end to end through the `flux` binary.** A real filesystem cannot be made to
  fail one lock write on demand, and a debug hook in `RunConfig` would inject the failure instead of the real write
  (`Held::heartbeat`), so an end-to-end test could pass while the real failure path was broken. Agreed with the peer
  after one negotiation turn, owner-approved.
  **Compensation:** the error itself - its code, its step, the lock path and the wording the operator reads - is
  pinned by `run::tests::two_failed_heartbeat_writes_stop_the_copy_naming_the_lock_and_the_run_records_failed` and
  `run::tests::a_heartbeat_failure_message_names_the_lock` (`crates/flux-core/src/run/tests.rs`); the binary printing a
  single-file copy error and exiting 1 by `a_missing_source_exits_1_and_counts_one_error`
  (`crates/flux-cli/tests/copy.rs`); the next run after an unreadable lock, the torn case, by
  `an_empty_lock_is_uncertain_until_restart_break_lock_takes_it_over` (`crates/flux-cli/tests/run.rs`); and how a
  `--restart` heartbeat failure prints, a `RunError::Failed` at the lock step, by `report.rs`
  `a_refusal_names_what_it_could_not_remove_and_a_failure_names_its_step_and_path`. Owner-accepted 2026-10-03.

## Benchmark publishing (test audit 2026-10-03)

- **`benches/publish/measure.py`'s `main()` is not unit-tested.** It builds the run's header from the CI environment
  (`GITHUB_SHA`, `GITHUB_RUN_ID`/`_ATTEMPT`, `ImageOS`/`ImageVersion`, `git rev-parse` for `source`), checks which
  comparators exist on the runner, and reads Defender's state. Testing it needs a refactor to separate those reads,
  which would reopen the capstone; the owner chose a boundary instead.
  **Compensation:** `test_every_header_field_reaches_the_result` pins that every header field reaches the result
  unchanged, and the dry run on all three hosted runners (Bench run 37112559659) produced complete headers: commit,
  run, a three-id `source`, image, cache, Defender (off on the Windows runner) and tool versions. Owner-accepted
  2026-10-03.

## Cut 9f-1 plan 1 - the WAL and the handle primitives (test audit 2026-10-12)

- **A file-level name-surrogate reparse point is not planted on Windows for `open_read`.** `dir_windows.rs:545` returns
  `SafetyRejected` for a name-surrogate tag found on a handle that opened as a file. The runner cannot plant a file
  symlink (it needs a privilege the CI runner lacks), and the one surrogate it can plant, a junction, is answered
  earlier as `IsADirectory` by `FILE_NON_DIRECTORY_FILE`, so `open_read_answers_a_junction_as_a_directory_and_leaves_its_target_alone`
  (`dir_handle.rs:1613`) never reaches the tag check. Compensation: the predicate itself is unit-tested
  (`dir_windows.rs:1755`: a junction tag is a surrogate, 0 and the OneDrive tag are not), the same check guards the
  other handle opens, and the POSIX arm pins its symlink analogue (`open_read_refuses_a_symlink_as_safety_rejected`,
  `dir_handle.rs:583`). Re-check that all three still exist before honouring this entry.

## Cut 10a - symlink copying (2026-10-16)

- **The Windows symlink success path runs on the hosted Windows runner, which (unlike the 2026-10 premise of the entry above) CAN create symlinks**: `create_symlink_round_trips_file_and_dir_links_when_the_runner_can`, `rename_replace_replaces_a_directory_link_with_a_regular_file`, `rename_replace_still_replaces_a_file_with_a_directory_link`, `rename_replace_over_a_junction_is_still_refused` (`crates/flux-platform/tests/dir_handle.rs`, `mod windows_arm`) printed `skipped` only where `can_create_symlinks` is false. They skip on a runner without the right; the no-right path is pinned by `create_symlink_without_the_right_is_unavailable_and_creates_nothing` (1314 maps to `SYMLINK_CREATION_UNAVAILABLE`) and the junction refusal by `read_link_on_a_junction_is_special_file_unsupported`. Measured by the owner on Windows 11 (2026-10-16, Developer Mode and a plain user; evidence in the cut 10a spec section 9): the probe results the arm is built on. Not yet run by the owner against the final arm: a Windows `flux copy` of a tree with file, directory and relative links (a smoke test the final review asked for). Re-check that these tests still exist, and that the runner image still creates links, before honouring this entry.
- **`symlinkat` on Linux vfat/exFAT is unmeasured** (the owner's vfat probe was not run). The POSIX arm maps `EPERM` to `SYMLINK_CREATION_UNAVAILABLE`, which is what a filesystem without symlink support answers through the VFS. Re-check if links are ever copied onto a vfat or exFAT volume on Linux.
- **The identity re-check before `CreateSymbolicLinkW` is skipped when the directory handle's identity is unavailable** (FAT32/exFAT: no file ids; ruling R12). Those volumes cannot hold a symlink in practice, so the call fails with the unavailable code instead of a false refusal. The one path-based write the Windows arm makes (`create_symlink`, via the handle's canonical path) is otherwise guarded by the identity equality. The `rename_at` retry (`FileRenameInformationEx` + POSIX semantics) is limited to the symlink reparse tag because the owner MEASURED (Windows 11, 2026-10-16) that the same rename also replaces a junction with a regular file, which a plain rename refuses; a directory link over a directory link needs the retry and succeeds with it. MEASURED by the owner on a real FAT32 volume (Windows, 2026-10-16, `create_symlink_on_a_fat32_volume_is_unavailable_and_creates_nothing`): the directory identity is `Unavailable`, `create_symlink` returns `SymlinkCreationUnavailable` ("the destination filesystem cannot hold a symlink") and creates nothing.

## Cut 10b - source mount boundaries (2026-10-17)

- **autofs trigger directories are unverified.** Whether an unmounted autofs trigger reads as a mount root, or `read_dir` mounts it on descent, was not measured (no autofs here). Compensation: the mount query passes `AT_NO_AUTOMOUNT`, so the query itself never mounts anything.
- **A Windows mounted-volume folder under a copied tree is unverified** (no such volume on the machine used). Compensation: `read_dir` types a mounted-volume folder as a link (reparse classification, `crates/flux-platform/src/std_fs.rs`, `read_dir`) and `read_link` refuses it as a special file, so the walk never descends into it; the Windows `mount_root` always answers `No`.
- **`STATX_ATTR_MOUNT_ROOT` on a real same-filesystem bind mount is unverified** (creating one needs privilege). Compensation: the kernel attribute is the same one the destination arm uses and measured; the platform tests ask about the real mount roots `/proc` and `/dev/shm` without privilege.
- **The cost of the extra `statx` per directory is not detectable:** measured 2026-10-17 at the top level, owner-approved, CPU about 87% idle with background Xorg, opencode and Claude uncontrolled. A local, uncommitted variant of `small_1000x4kib` with one directory per file (1000 directories) ran twice per arm (mount check on vs `--cross-filesystems`). normal: 2.05 s and 2.16 s on vs 1.87 s and 2.03 s off; strict: 8.36 s and 8.61 s on vs 9.15 s and 8.83 s off. Each run's own interval is about +-10%, criterion p = 0.07 to 0.68, and the sign is not consistent, so no cost is detectable; the reasoned bound is 1000 `statx` calls (a few microseconds each, not measured directly) against about 2 s, at most 0.2%. The committed bench has only 20 directories and cannot resolve it.

## Cut 11a - dry run (2026-10-17)

- **A half-copied file's continuation is not previewed (cut 11b, N1):** a `--dry-run --resume` reports a partial beside its target as `would_copy (partial of N bytes present; its continuation is not previewed)` and counts it; the checkpoint log is not read and no partial is hashed.
- **A live owner of the destination lock is not detected (cut 11b, D7/P2):** the dry run reads the lock record and prints both outcomes (TARGET_LOCK_BUSY if the OS lock is held, the preview otherwise) with `owner_liveness` in `unprobed`; a writer holding the prior's claim store is not probed either (on Linux and macOS a torn copy prints the "could not be read consistently" note; on Windows redb's mandatory whole-file lock fails the copy's read with OS error 33, and the dry run stops at the state step naming `state.db`, exit 1). Compensation: the lock record's holder fields and heartbeat age are printed; the real run decides by `try_lock`.
- **Same-run namespace collisions are not predicted:** two source names that fold onto one destination entry plan as two creates where a real run reports one collision (spec 5.2).
- **Failures that occur only while bytes move are not predicted:** a mid-file read error, a full disk, a source changed during the copy, metadata-apply complaints, claim-store write failures.
- **A top-level `.flux` source entry mirrors the live run (ruling P11, retiring P6):** when `.flux` does not yet exist at the destination (DEST absent, or `open_dir` answers not-found) a directory merges without a count or a record and a file or link fails with the live code (`DESTINATION_ERROR` into an existing DEST, `SAFETY_REJECTED` into an absent one); when `DEST/.flux` already exists the ordinary path opens it and resolves its entries against the real listing, as live does; the reserved subdirectories refuse identically in both modes. The name is matched ASCII case-insensitively (as `reserved_path` folds it), so a source spelled `.FLUX` is treated the same way even on a case-sensitive destination where a live run would create a distinct `.FLUX`: the dry run then predicts a merge or a failure that live would not produce (ruling P13; the safe direction, and an exotic name).
- **An unreadable source file is predicted as `would_copy`:** its open is refused only while bytes move, I3 forbids opening the source, and permission bits are not a portable predictor.
- **The lock and the operation state are reported as unperformed (ruling P10):** every dry run lists `target_lock` in the JSON's `unprobed` and prints one note; `prior_operation` joins it when a prior was seen. A real run may be refused or fail at the lock.
- **The unprobed `NOREPLACE_PUBLISH_UNAVAILABLE` note prints on every tree dry run** that reaches the walk, because the dry run never opens the operation and so never probes (D3). It is never printed for a single file, which has no probe.
- **The fake filesystem does not inherit identity strength on `create_dir`** (ruling P4), so a fake-backed live run notes `replace_degraded` once where the dry run notes the planned child too. Compensation: the real-filesystem paired test (`crates/flux-core/tests/dry_run_std_fs.rs`), where both modes note the same count, and the dry test's explicit count of 2.

## Several sources - `flux copy a b c DEST` (2026-10-22)

These are limits and rulings of the cut, not test gaps. Each is stated as the spec's section 6, 3.4 and 3.6 state it (`docs/superpowers/specs/2026-10-22-several-sources-design.md`).

- **L1. An aborted several-source run is not resumed.** After an aborted or crashed several-source run, re-running the same command is refused by the prior-operation scan (spec 21.1). With several sources `--restart` is refused in this cut, so the way forward is `flux cleanup --force DEST`, or `flux copy --restart` with one source. The resume cut removes this.
- **L2. The pre-transfer phase stops at the first bad source,** not all of them.
- **L3. A several-source run is not atomic across roots.** Roots finished before an abort stay on disk.
- **L4. A root with a very long name is refused.** A root whose name is so long that `<name>.flux-lock` exceeds the directory's limit is refused. Spec 96.1's directory-lock fallback is not used by several-source runs.
- **L5. The number of sources has a cap.** The plan sets it from a measurement, at run time, from the open-file limit.
- **L6. A dead prefix lock can stay behind.** A dead prefix lock for a name no later run uses stays in DEST as a stale file until that name is used again (ruling R7).
- **A skipped existing file does not reach the identity check in a tree, but does in a single-file copy.** A file root follows the tree rule (spec 3.6). Pinned by `a_skip_never_reaches_the_identity_gate_for_a_file_root` in `crates/flux-core/src/run/multi.rs`.
- **A destination directory whose identity is not Strong makes an existing file a collision in a tree,** where a single-file copy honours the policy. A file root follows the tree rule. MEASURED LIVE during implementation. Pinned by `a_weak_parent_identity_degrades_to_no_replace` in `crates/flux-core/src/run/tests.rs` and `a_weak_destination_identity_makes_a_file_root_a_collision_like_a_tree_file` in `crates/flux-core/src/run/multi.rs`.
- **R1. One destination-root lock plus one prefix lock per root.** All are held for the whole run. The workspace lives under `DEST/.flux/`, and the root lock serialises every operation on DEST. The prefix locks add alias detection and protect each `DEST/<name>` against a single-source tree run onto DEST.
- **R7. Prefix lock records carry `workspace_path` `none`.** This extends master spec 259.6, which defines `none` only for cleanup locks. The operation lock already names the real workspace. A dead prefix lock is then trusted and recovered by the next acquirer of that name. Owner decision.
- **Nested sources copy shared content twice (R5).** `flux copy a a/sub DEST` copies the content under `a/sub` to `DEST/a/sub` and to `DEST/sub`. The prefixes are distinct, which is all spec 18.3 requires.

## Cut 12 - hardlink preservation (2026-10-26)

- **A destination that cannot hold hardlinks (FAT, some network shares) is exercised through `FaultFs::set_hardlinks_unavailable` only.**
  No CI runner offers such a destination, so the real `linkat` / `NtSetInformationFile` refusal on one is not reached.
  **Compensation:** the engine tests `a_link_the_destination_cannot_hold_degrades_under_auto` and
  `a_link_the_destination_cannot_hold_fails_under_preserve` (`crates/flux-core/src/run/tests.rs`) pin the outcome of the
  refusal (degrade under `auto`, `HARDLINK_UNAVAILABLE` under `preserve`), and the Unix errno mapping of the primitive is
  unit-pinned (`map_link_errno`, `crates/flux-platform/src/dir_unix.rs`). Owner-accepted 2026-10-26.

## Cut 14b - several sources for flux verify (2026-10-27)

- **DEST's top level is not examined under several sources.** `flux verify S1 S2 DEST` compares each source with `DEST/<name>` and never walks DEST's other children, so an entry at DEST's top level that belongs to no root is not reported. **Compensation:** the README states the boundary, and a stderr advisory for those entries is a TODO follow-up (`TODO.md`, Cut 14 open). Anchor: `verify_several` in `crates/flux-core/src/verify.rs`. Recorded 2026-10-27; owner acceptance pending.
- **A user file named like a partial temporary is hidden inside a nested root as it is at a one-source root.** **Compensation:** the same rule the one-source root has had since cut 14 (a copy's temporary and a user file of that name cannot be told apart); the engine fixture `a_users_dot_flux_inside_a_nested_root_is_compared_and_a_partial_temporary_is_hidden` in `crates/flux-core/src/verify.rs` pins that ONLY the partial-temporary rule applies under `ControlFilter::NestedRoot`. Anchor: `ControlFilter::NestedRoot` in `crates/flux-core/src/verify.rs`. Recorded 2026-10-27; owner acceptance pending.
- **The resolver's source cap is bypassed for verify (`usize::MAX`).** **Compensation:** verify holds nothing across roots: the check phase keeps one verdict and at most one identity per root and no directory handle, and the roots are walked one after another, so the descriptor limit copy's cap protects is never approached (design section 6.2). Anchor: `verify_several_cli` in `crates/flux-cli/src/main.rs`. Recorded 2026-10-27; owner acceptance pending.
- **Copy's `LockCode` for a refused name is not surfaced by verify.** **Compensation:** the refusal is a usage error by design ruling A1 (verify has only exit codes 0, 1 and 2); the CLI test `refusals_match_copy` in `crates/flux-cli/tests/verify.rs` pins exit 2 with an empty stdout and an `error:` line for a duplicate name and for a name ending in `.flux-lock`, and `verify_several_cli` in `crates/flux-cli/src/main.rs` maps every `VerifyError` through `refusal_line`. Anchor: `refusal_line` in `crates/flux-cli/src/main.rs`. Recorded 2026-10-27; owner acceptance pending.
- **An earlier one-source copy's control state inside `DEST/<name>` is reported `extra` under several-source verify.** A several-source copy's control state (`DEST/.flux`, `DEST/<name>.flux-lock`) is never seen. **Compensation:** `extra` never changes the exit status (`Summary::failed` in `crates/flux-core/src/verify.rs` counts `missing + mismatched + unreadable` only, and the cut 14 design `docs/superpowers/specs/2026-10-26-verify-command-design.md`), so the residue is reported but costs nothing; a several-source copy never puts control state inside `DEST/<name>`; the engine fixture named in entry 2 pins that a user's `.flux` there is compared, which is the behaviour this boundary trades for. Anchor: `ControlFilter::NestedRoot` in `crates/flux-core/src/verify.rs`. Recorded 2026-10-27; owner acceptance pending.
- **A refused NAME prints the planner's detail, which embeds the source path unescaped, as copy prints it; two FILE roots that alias on a case-insensitive destination, or a file root against a folder root of the same folded name, are not detected; on a destination whose identities are `Weak` or `Unavailable`, two aliasing folder roots run and report each other's files.** **Compensation:** the planner quotes the NAME with `{:?}` (`crates/flux-core/src/run/roots.rs:87-134`), so only the source path the user typed is printed raw, exactly as `flux copy` prints the same refusal; every line verify builds itself (`root <name>:`, the absent-DEST line, the refused-root and refused-alias lines) escapes the name through `display_path` (`crates/flux-cli/src/verify_report.rs`); exit 2 carries the meaning the code would have carried. A copy can never produce such a DEST (copy detects an alias by its lock-file names, `crates/flux-core/src/run/multi.rs`, so only a hand-made layout reaches this); the engine test `two_root_names_that_alias_on_the_destination_are_refused_before_any_read` in `crates/flux-core/src/verify.rs` pins the `Strong`-only rule and the no-identity cases. Anchor: `crates/flux-core/src/run/roots.rs:87-134` and `verify_several`'s alias rule in `crates/flux-core/src/verify.rs`. Recorded 2026-10-27; owner acceptance pending.
- **A root checked absent and then created before its walk reports `missing`; the check-to-walk window is not re-probed.** **Compensation:** a stale absent verdict reports `missing`, which fails the run loudly rather than passing silently; the one-source run has the same two-open shape between its check and its walk; the engine test `an_unreadable_root_is_one_record_at_its_turn_and_the_others_run` in `crates/flux-core/src/verify.rs` pins the walk-phase reopen failure only; the stale-absent case (a root checked absent and created before its walk reports `missing`) is NOT pinned by any test, a test-audit candidate. Anchor: `verify_several`'s check and walk loops in `crates/flux-core/src/verify.rs`. Recorded 2026-10-27; owner acceptance pending.
