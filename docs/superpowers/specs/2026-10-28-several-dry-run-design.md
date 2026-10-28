# `flux copy --dry-run` with several sources (cut 18)

Status: draft for owner approval. Not built. Branch `feat/several-dry-run` off main `d59cd83e6790b8ec18e0da7774094f964b96581f`.

## 1. Intent

`flux copy a b c DEST` refuses `--dry-run` today (`crates/flux-cli/src/main.rs:650`, `several_flag_gate`; the engine entry `crates/flux-core/src/run/multi.rs:459` asserts `!cfg.dry_run`). `--restart` and `--break-lock` work with several sources since cut 17. This cut lets an operator preview a several-source copy, and a several-source `--restart`, the way cut 11a and 11b let them preview a one-source copy: read everything a real run would read, say what it would do and which refusal it would meet, change nothing.

Owner decision (2026-10-28): the output is ONE combined plan with a per-root breakdown, mirroring the real run's report (the `roots` array and the summed counters). Owner decisions on the three forks (agy and the author agreed on each): slicing S2, engine shape A, prefix-lock disposition P3 (section 3).

## 2. Goals, non-goals, anti-goals

Goals
- G1. `flux copy --dry-run a b c DEST` prints the plan of a fresh several-source copy and exits as the one-source dry run does: 0 when the plan completes, the real run's exit code for a refusal it can predict.
- G2. `flux copy --dry-run --restart a b c DEST` and `--dry-run --restart --break-lock a b c DEST` preview what the real invocation would do at a DEST holding prior state: the priors superseded, the half-copied files and state directories discarded, a destination-lock takeover (cut 11b's vocabulary, over format-5 priors).
- G3. A dry run writes nothing under DEST (or its parent), takes no OS lock, creates no lock file and no state directory. This is the one-source dry run's property (spec of cut 11a, `docs/superpowers/specs/2026-10-17-cut-11a-dry-run-design.md`).
- G4. Every refusal the real run raises from information a READ can decide is predicted with the same error code: a same-name collision, a source that is not a regular file or folder, DEST inside a source, an object of the wrong kind at `DEST/<name>`, an unreadable, foreign or untrusted prefix lock.
- G5. The per-root counters sum to the totals, as in the real report.

Non-goals
- N1. `--resume` with several sources, with or without `--dry-run` (the next cut). `--dry-run --resume a b c DEST` stays exit 2 naming `--resume`.
- N2. Predicting that a prefix lock is HELD. A held and a dead (L6 residue) prefix lock carry the same bytes (`workspace_path` = `none`), and only trying the OS lock tells them apart (`dry_run` reads files; `dry_lock` states "It never opens the lock", `crates/flux-core/src/run/dry_prior.rs:51`). A readable trusted prefix lock is reported as the dual outcome the destination lock already is: `owner_liveness` in `unprobed`.
- N3. Predicting a same-name collision that exists only after case folding on a filesystem where NEITHER name exists yet at DEST (neither the target `DEST/<name>` nor its lock file `DEST/<name>.flux-lock`). The real run finds it by creating the second lock file, which opens the first one's file and meets its own record (`PrefixRefusal::Collision`, `prefix.rs`); a read cannot. When a target or a lock file of either name EXISTS, the dry run does predict it, by the pairwise identity check of section 4.2 step 2. (`check_targets` compares each target only with its own source, and `plan_roots` compares prefix keys byte for byte, so neither catches an alias between two roots.)
- N4. Any change to the one-source dry run's output.

Anti-goals
- A1. A dry run that reports a plan the real run would not follow. Where the dry run cannot know (N2, N3) it says so (the `unprobed` list), it never guesses a favourable outcome.
- A2. Streaming `would_discard` records for a run that then refuses: the live order is destination lock, scan, prefix locks and target check (`place.create`), then supersede (`session.rs`); the preview keeps that order (section 4.2).
- A3. A new `unprobed` key or a changed JSON key set for the one-source dry run.

## 3. Approaches considered (the owner chose the first of each)

Slicing: S2 (this design): the fresh plan plus `--restart` and `--restart --break-lock` previews, because the real invocation after an aborted several-source run is `--restart` and cut 11b's `dry_start` and `dry_restart` already work over format-5 priors through `Place::scan` (`MultiPlace::scan` delegates to `TreePlace::scan`). S1 (fresh plan only) keeps the change smaller but builds the dry driver once and re-opens it for the prior previews, and needs its own gate for `--dry-run --restart`.

Engine shape: A (this design): `trees()` branches to a dedicated `dry_trees` just before `drive`, after the same Group 1 steps the real run takes (names, sources, `locate_dest`, preflight and containment, capability, control plane), as `tree_with` branches to `dry_tree` (`run/mod.rs:377`). B (generalise `dry_tree` over `Place`) would put multi-root walk types on the `Place` trait. C (the CLI calls the one-source dry run per source) cannot work: it scans `DEST/<name>` for priors instead of `DEST`, applies the one-file policy to file roots, walks in argument order with separate hardlink slots, and counts an absent DEST once per source.

Prefix locks: P3 (this design): `dry_lock` per prefix. P2 (do not look) misses refusals that certainly stop the real run. A naive "any readable record is BUSY" refuses where the real run recovers L6 residue.

## 4. Design

### 4.1 CLI (`crates/flux-cli/src/main.rs`, `report.rs`)

- `several_flag_gate` refuses only `--resume` (message unchanged: "error: --resume with several sources is not supported yet; run again with --restart to supersede an aborted several-source run"); `--dry-run` alone and with `--restart` / `--break-lock` pass. `--dry-run --resume` prints the `--resume` message (the first arm, as today).
- `copy_several`: the two `Report::pre_engine` arms (`main.rs:696`, `:703`) set `r.dry_run = args.dry_run`, as the one-source arms do (`:470-471`, `:478-479`).
- The dry output of a several-source run: the streamed `would_*` records (`<name>/...` paths, as failures are), then per-root lines, the dry summary line and the notes. `Report::several_run` + `tree_output` already take `Run`; a dry run sets `rep.dry_run`, `rep.unprobed` from `unprobed_keys` and zeroes `average_bytes_per_second`, as `copy` does for one source (`main.rs:506-511`).
- Per-root line in a dry run (author's call R2, amended by panel round 1): `root <name>: would copy <n>, <m> would fail` (the real run's `root <name>: <n> copied, <m> failed` would read as if something had been copied). `<m>` is the SAME counter the real line prints, `files_failed` (copy failures only, `report.rs` `Report::tree`); the summary's `errors` counts every failure class, so a root that failed on a walk or create error shows `would fail 0` and status `failed`. `root_lines` gains a dry flag (`root_lines(roots, dry)`) and `not started: ...` is unchanged.
- `--json`: the dry report carries `"dry_run": true` and the same `roots` array and keys as a real several-source report; a root's `status` is `completed` when its plan reached the end, `failed` when it counted an error, `not_started` after an abort. No new key.
- `--help` of `copy` and the doc comment at `main.rs:39`: "--resume needs one source" only.

### 4.2 Engine (`crates/flux-core/src/run/multi.rs`, `run/mod.rs`, `tree.rs`)

`trees()` returns `dry_trees(...)` when `cfg.dry_run`, right where `drive` is called today, with the `MultiPlace` already built (its `tree.dest` may be `None`). `dry_trees` is `dry_tree` for several roots:

1. `dry_start` (run/mod.rs:1291) gains one parameter, `create_preview: &mut dyn FnMut(&mut Vec<RunWarning>) -> Result<bool, RunError>`. It is called (a) in the `cfg.restart` branch BEFORE `restart_preview`, and (b) in the "no resumable prior" branch; not in the refusal branch (the real run refuses at the scan before `create`). A refusal returns `DryStart::Stop`; `Ok(true)` means a prefix lock record was readable and trusted, and sets `dry.owner_liveness = true`. `dry_tree` and `dry_file` pass a closure that returns `Ok(false)`. This is the ordering of anti-goal A2.
2. The multi `create_preview`: if DEST is absent, `Ok(false)` (nothing is inside it). Otherwise, for each root name in prefix order, `dry_lock(&LockSite::directory(dest, name), false, capability, now, warnings)` (break-lock is false: `--break-lock` covers the destination lock only, cut 17 P1): `Refused` and `Error` become `PrefixRefusal::Lock { name, error }` through `MultiPlace::prefix_error` (the same text as the real refusal, the lock file named once); `Recorded` sets the return to `true`; `Takeover` cannot occur with `break_lock` false. Then the pairwise alias check: for every two roots in prefix order, `metadata` of `DEST/<a>` and `DEST/<b>`, and of `DEST/<a>.flux-lock` and `DEST/<b>.flux-lock`; two entries that exist with equal `Strong` identities refuse with `DestinationNamespaceCollision` through `prefix_error(PrefixRefusal::Collision { name, other })`, the real run's code and text (a `Weak` or `Unavailable` identity never decides, as in `check_targets`). Then `check_targets` (made to take the DEST handle as an `Option`, `None` = skip) so wrong-kind and alias targets refuse with the real codes. `.broken.*` leftovers are noted by `dry_lock`.
3. The walk: one `Shared` base with the dry settings of `dry_tree` (`dry_run: true`, unguarded, no heartbeat, `NullClaimStore`, the deferred hardlink slot in the temp directory shared by ALL roots, as the real run shares one slot), `for_root` per root. `copy_roots_at` (tree.rs:770) takes `dest: Option<&F::Dir>`, `None` only in a dry run with DEST absent. For a folder root in a dry run the root directory is `dest.open_dir(name)`: present, it is `Some(dir)` with `RootIdentity::Existing`; absent (or DEST absent), it is `None` with `RootIdentity::Planned(identity of the nearest existing ancestor)`, `directories_created += 1`, and a `<name>` `would_create` record is streamed (R1: a root folder is a directory the real run creates, as a subdirectory is; DEST itself is counted, not streamed, as in one source). The walk is `copy_tree_frames` (what `dry_tree` calls), not `copy_tree_at`. For a file root with DEST absent, `root_state` takes `None` and `RootIdentity::Planned`, `copy_one` takes `parent: None`, and no batch is drained. A DEST that was absent adds one to `directories_created` after the walk unless the walk refused with nothing changed (`dry_tree`'s I7).
4. `dry_top_level_flux` (tree.rs:1252) treats a top-level child named `.flux` as DEST's workspace directory. In a several-source folder root the walk's top level is `DEST/<name>`, never DEST, and a real run copies `<name>/.flux` as ordinary content. The function returns `false` when `cx.key_prefix` is `Some` (every folder root of a several-source run sets it). Pinned by a paired dry/real test (section 5).
5. The outcome: `MultiRun` with `run.dry = Some(dry)` and `roots` built from the same per-root diff `copy_roots_at` computes. `dry.unprobed_noreplace = true`, as in one source.

Unchanged: the lock-free property. `dry_trees` never calls `drive`, `PrefixLocks::acquire`, `create_dest`, `supersede`, `finish`.

### 4.3 What the output says where a read cannot decide

`unprobed` keeps the one-source tree dry run's vocabulary and order (`noreplace_publish`, `target_lock`, `owner_liveness`, `lock_takeover`, `prior_operation`; `report.rs:532`). A readable trusted prefix lock sets `owner_liveness`. `RunWarning::DryRunLockRecord` carries no lock path (`report.rs:778`), and k prefix locks of one crashed run share one operation id, so k identical notes would not say which lock they describe: the prefix locks print ONE new note instead, `RunWarning::DryRunPrefixLocks { names }`: "note: the prefix lock records of <names> are readable; their owners' liveness was not probed; a real run refuses with TARGET_LOCK_BUSY for each whose OS lock is held" (author's call R6). The destination lock's existing note and variant are unchanged. N3 (absent case-folded names) has no key and no line: it is recorded in `docs/accepted-boundaries.md` and the `--help` text of `--dry-run` (author's call R3: a new key would change the contract, anti-goal A3).

## 5. Tests (the oracle for the plan)

Engine (`FaultFs`, `crates/flux-core/src/run/multi.rs` tests):
- T1. Two folder roots and a file root into a fresh DEST: the plan, per-root counts, totals equal the sum, `directories_created` counts DEST once plus each folder root, root `would_create` records present.
- T2. Pairing: for the same fixture, the dry plan's counters equal the real run's (copied, skipped, overwritten, directories, errors), and the dry call log contains no write, create, rename, remove or lock operation (the allow-list style of cut 11a).
- T3. DEST present with existing targets: overwrite/skip/up-to-date per root as the one-source dry run.
- T4. Refusal parity, dry vs real: wrong-kind target (file root over a directory, folder root over a file), source inside DEST's target, an unreadable prefix lock (`TARGET_LOCK_UNCERTAIN`), an orphan prefix lock (`ARTIFACT_OWNERSHIP_UNCERTAIN`), a foreign one; each has the same code and the same detail text.
- T5. A readable trusted prefix lock: the plan completes, `owner_liveness` is set and the `DryRunPrefixLocks` note names exactly that root; a stale L6 residue (dead prefix lock) does not refuse, as the real run recovers it. Negative control: DEST present with NO prefix lock file leaves `owner_liveness` unset and prints no prefix note.
- T6. `--restart` over a crashed several-source prior (cut 17's `crashed_ab`): `DryPrior::Restart` lists the prior, its partials and state directory, the `would_discard` records stream, nothing changes on the fake. Ordering: with a wrong-kind target at DEST, the dry `--restart` refuses with the real code and has streamed NO `would_discard` record (A2).
- T7. `--restart --break-lock` with an empty destination lock: `lock_takeover` is set and the plan continues; with an unreadable prefix lock too, the prefix refusal is returned (cut 17's T4b, dry).
- T8. A root containing a regular file `.flux` directly under it: the dry plan agrees with the real run (copied, no `SAFETY_REJECTED`); and a `.flux` directory.
- T9. Hardlink groups spanning two roots: one shared slot, the dry counts match the real run's `files_hardlinked`.
- T10. `--exclude` and `--no-recursive` per root: per-root `excluded` counts sum to the total.
- T11. DEST absent: no prefix-lock reads, no `check_targets`, every root planned new.
- T13. A plain dry run (no `--restart`) over a crashed several-source prior refuses with `RESUMABLE_OPERATION_EXISTS` naming `--restart`, as the real run does, and streams nothing; a dry `--restart` with DEST absent previews an empty supersede and the fresh plan.
- T12. An abort-class failure in root 2 (the builder picks a trigger the fake can inject, for example a failing `open_dir` of root 2's existing target; a source read error is NOT abort-class: `walk_into` counts it as a walk failure and continues): root 1 `completed`, root 2 `failed`, later roots `not_started`. T12b: a counted non-abort failure (a source read error in root 2): root 2 `failed`, root 3 `completed`, the totals and `errors` include it.
- T14. Alias between two roots: with `Data` and `data` as sources and an existing `DEST/Data` (or `DEST/Data.flux-lock`) on a name-folding filesystem, the dry run refuses with `DESTINATION_NAMESPACE_COLLISION`, the real run's code and text. If `FaultFs` cannot model two names with one identity, this test runs on a real case-insensitive volume only and is skipped elsewhere, like `an_alias_is_refused_on_a_case_insensitive_destination` (`crates/flux-cli/tests/several_sources.rs`).
- T15. DEST's creation count: DEST absent and the plan completes: `directories_created` counts DEST once plus each absent folder root; DEST absent and the walk refuses with nothing changed (`TreeAbort::refused_unchanged`, whichever trigger the fake can inject): DEST is not counted. Both ways pinned (dry_tree's I7).
CLI (`crates/flux-cli/tests/several_sources.rs`): the existing gate test loses its `--dry-run` arm and keeps `--resume` and `--dry-run --resume` (exit 2 naming `--resume`); a dry run exits 0 with the dry summary and the per-root lines; `--json` carries `dry_run: true`, `roots` and `unprobed`; `--json` on a missing and on a symlink source carries `dry_run: true` (the pre-engine arms); a dry `--restart` over a killed run (the `Stalled` fixture of cut 17) prints the discards and the real tree is byte-identical afterwards; `--dry-run --restart` on a fresh DEST exits 0.
Real filesystem (`crates/flux-core/tests/dry_run_std_fs.rs` style): snapshot before/after unchanged for a several-source dry run, and the dry counters equal a real run's on a copy of the tree.

## 6. Constraints and accepted limits

- Windows: path separators in the streamed `<name>/<path>` records follow the one-source rule; the plan's string matches go through `calls()` normalisation. NTFS and macOS behaviour is verified only by CI after the push.
- The cost of the dry mode on the live path is unchanged (one `dry_run` branch per entry, already there).
- Accepted limits N2 and N3 go to `docs/accepted-boundaries.md`; TODO L1 loses "and `--dry-run`" (keeps `--resume`); ROADMAP lines 41 and 47 and the older several-sources and several-restart designs get dated notes, as cut 17 did.

## 7. Author's calls for the owner to review

- R1. A root folder absent at DEST streams a `<name>` `would_create` record and counts one directory.
- R2. The dry per-root line reads `root <name>: would copy <n>, <m> errors`.
- R3. N3 has no `unprobed` key; it is documented only.
- R4. `create_preview` is a `dry_start` parameter (not a second pass after `dry_start`), so the A2 ordering is one place, not two.
- R6. Prefix locks print one aggregated `DryRunPrefixLocks` note, not one `DryRunLockRecord` each (panel round 1: the existing note names no lock).
- R5. `dry_top_level_flux` is gated on `cx.key_prefix` being `None`, not on a new flag in `Shared`.

## 8. Left to the builder

The exact signature changes to `copy_roots_at`, `folder_root`, `file_root` and `check_targets` that carry `Option<&Dir>`; where `dry_trees` lives (it may sit in `multi.rs` beside `trees` or in a new `multi_dry.rs`); the wording of the `--help` sentence; the names of the new tests.
