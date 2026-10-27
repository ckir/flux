# `flux copy --restart` (and `--break-lock`) with several sources - design (cut 17)

Status: DRAFT for the owner. Author calls are marked **(call)**. Owner decisions are marked **(owner)**.
Builds on `docs/superpowers/specs/2026-10-22-several-sources-design.md` (cut 11c; N1 and L1 there) and spec section 21.1.

## 1. Intent and why

`flux copy a b c DEST` refuses `--restart`, `--break-lock`, `--resume` and `--dry-run` today (`crates/flux-cli/src/main.rs:646-662`,
`several_flag_gate`). After an aborted several-source run the operator's only way out is `flux cleanup --force DEST` or a one-source
`flux copy --restart` (`TODO.md:1134`, L1). This cut lets `--restart` supersede an aborted several-source run, so one command both
clears the old run and copies again, exactly as it does for one source.

**(owner)** `--restart` must work immediately after a crash. Measured basis: a copy run's lock acquisition has no lease wait. A crashed
run's lock is `Dead` (OS lock released, workspace present) and `obtain` recovers it at once (`crates/flux-core/src/lock/obtain.rs`, the
`Classified::Dead` arm of `obtain`); the lease threshold exists only in `obtain_cleanup_lock` (same file), which is why
`flux cleanup --force` waits about 30 seconds (`TODO.md:1138`). A crashed several-source run's prefix lock records `workspace_path = none`
and classifies `Dead` too (`crates/flux-core/src/lock/site.rs:150-156`, where a `none` workspace is `Trusted`; pinned for a cleanup lock by `classify.rs:215`; several-sources design ruling R7).

What "immediately" covers **(call)**: a crash with the prior's workspace still on disk, which is every crash during a copy. A crash in the narrow window after the prior's workspace was retired but before its lock was released leaves an `Orphan` lock, which `obtain` refuses with `orphan_refusal` (`lock/obtain.rs`) pointing to `flux cleanup DEST`; that is the same for one source today and stays out of scope.

## 2. Goals, non-goals, anti-goals

Goals
- G1. `flux copy --restart a b c DEST` supersedes every resumable prior at DEST (a several-source prior, a one-source prior, or both), then runs as a fresh several-source copy.
- G2. `--break-lock` is accepted with `--restart` and several sources and applies to the destination lock, as for one source (spec 240.5).
- G3. The refusal texts that today send the operator to `flux cleanup --force` for a several-source prior say `--restart` instead.
- G4. One-source commands behave exactly as today. State format 5 and format 4 are unchanged byte for byte.

Non-goals (each stays in `TODO.md`)
- N1. `--resume` and `--dry-run` with several sources (later cuts, in the order agy and the owner chose: dry-run, then resume).
- N2. `--break-lock` for the prefix locks (**owner**, P1): an unreadable prefix lock is refused with an honest message. P2 (take over and record each prefix takeover) is its own later cut because `state.takeover` is one optional slot (`crates/flux-core/src/state.rs:369`, `deny_unknown_fields`) and widening it risks a format 6 against several-sources goal G5.
- N3. Any change to the lease rule or to `flux cleanup`.
- N4. `--restart` with `--dry-run` for several sources (it stays the `--dry-run` usage error).

Anti-goals (a failure even if the tests pass)
- A1. A restart that releases this run's own prefix locks while it supersedes a prior (the defect at `multi.rs:300-309`, see 3.2): the copy would then run with no namespace guard.
- A2. A message that promises `--break-lock` where `--break-lock` cannot help.
- A3. Superseding a live run: `--restart` still refuses `TARGET_LOCK_BUSY` when another run holds the destination or a prefix lock.
- A4. Any residue beyond the stale prefix lock files of L6 (3.3) that a one-source restart over the same prior would not also leave.

## 3. Design

### 3.1 CLI (`crates/flux-cli/src/main.rs`)
- `several_flag_gate` keeps `--resume` and `--dry-run` and loses `--restart` and its "run flux cleanup --force" hint. With `--restart --dry-run` the `--dry-run` message is the one printed.
- `--break-lock` already `requires = "restart"` (`main.rs:158`), so it is accepted exactly when `--restart` is.
- The `Copy` doc comment at `main.rs:39` and the `--restart` and `--break-lock` help are rewritten: several sources now work with `--restart` and `--break-lock`; `--resume` and `--dry-run` need one source.

### 3.2 Engine: `MultiPlace::remove` (`crates/flux-core/src/run/multi.rs:300-309`)
Today `remove(id)` releases this run's prefix locks (`release_cell(&self.prefix)`) whatever `id` is. `supersede` (`crates/flux-core/src/run/restart.rs:40-45`) calls `place.remove(&prior.state.operation_id)` for each superseded prior, so a restart would drop this run's prefix locks before any file is copied (A1). Fix: `remove(id)` releases the prefix locks only when `id` equals this run's operation id (`self.prefix_id.operation_id`); for any other id it only retires that workspace through `self.tree.remove(id)`. **(call)** The comparison lives inside `MultiPlace::remove` so the `Place` trait does not change. The `debug_assert!(!cfg.restart && !cfg.resume, ..)` at `multi.rs:443-446` is narrowed to `!cfg.resume`, and its comment is rewritten.

### 3.3 Order of a several-source restart (no new machinery)
`open_operation` (`crates/flux-core/src/run/session.rs`) already does: obtain the destination lock (`Dead` prior lock recovered, or taken over under `--break-lock`); `place.scan` (every resumable prior, of any kind and format); `place.create` (for several sources this acquires the prefix locks, recovering dead leftovers of names this run uses, then writes this run's state); write this run's record; `supersede` (set each prior ABANDONED with `superseded_by`, sweep partials, retire each prior workspace). Verified, not changed: `sweep_partials` walks DEST for `<name>.flux-partial.<id>` without reading the prior's roots (`crates/flux-core/src/run/sweep.rs`, `TreePlace::sweep` at `run/place.rs:495`), so it works for a several-root prior; `MultiPlace::sweep` delegates to it (`multi.rs:291`). A dead prefix lock of a name this run does not use stays as a stale file (several-sources limitation L6, and the one residue a several-source restart leaves that a one-source restart does not). **(call)** This cut accepts that: `supersede`, `sweep` and `remove` keep touching only the prior's workspace and partials, because clearing prefix locks is a new engine step on the lock path with its own ownership questions. It is logged in `TODO.md` under L6 as a follow-up. A later run that uses the name recovers the lock, and `flux cleanup` does not touch it.

### 3.3a Safety order under `--restart`
Spec 21.1 steps are unchanged: `still_owned` is checked before every mutation in `supersede` (`checked(locked)`), and a lost lock stops the run (`S21_1_s3_refuse_close`).

### 3.4 `--break-lock` and the prefix locks (owner, P1)
`PrefixLocks::take` (`crates/flux-core/src/run/prefix.rs:108`) keeps `Mode::Plain`, so `Obtained::Claimed` stays unreachable there. The destination lock takeover is unchanged: `open_operation` computes `mode` from `cfg.break_lock` (`session.rs:133`) and records `state.takeover` (`session.rs:~272`). When `obtain` returns `TARGET_LOCK_UNCERTAIN` for a PREFIX lock, its detail today says "run again with --restart --break-lock" (`uncertain_refusal`, `crates/flux-core/src/lock/obtain.rs`), which cannot help. `PrefixLocks::take` replaces that detail, keeping the code `TARGET_LOCK_UNCERTAIN` and the holder field, with a text of this form: `the prefix lock <DEST>/<name>.flux-lock has an unreadable record; if no Flux run is active on this destination, remove that file and run again` **(call: exact wording is the builder's, but it must name the file and must not contain "--break-lock")**. A prefix lock left readable by a crash is `Dead` and needs no flag. The refusal applies only to a prefix lock of a name THIS run uses, because only those are acquired; an unreadable prefix lock of a name this run does not use is never opened. `--restart --break-lock` with an unreadable destination lock and an unreadable prefix lock takes over the destination lock and then refuses at the prefix lock with this text; the CLI prints no extra warning.

### 3.5 Messages that name `flux cleanup --force` for a several-source prior
`resumable_refusal` (`crates/flux-core/src/prior.rs:176-221`): the one-prior several-source arm (lines 182-188) and the several-prior arm's suffix (lines 217-219) are rewritten to say `--restart` supersedes it. **(call)** A several-source prior may be superseded by any `--restart` whose scan reads DEST's `operations` directory: a several-source run or a one-source TREE run. A one-source FILE copy scans only the adjacent `<target>.flux-state.<id>` records (`TreePlace::scan` at `run/place.rs:474`, `FilePlace::scan` at `:712` via `scan_file`, `prior.rs:120`), so it never sees a several-source prior and its messages do not change. `--resume` still cannot continue a several-source prior. The cleanup hint stays in `--help` as an alternative only where it is true.

### 3.6 Reporting
No new flag, report field or JSON key. A restart reports as a one-source restart does today.

### 3.7 Documentation
`README.md` several-sources section, the `--help` texts, `TODO.md` L1 (close the `--restart` and `--break-lock` part, keep `--resume` and `--dry-run`), `ROADMAP.md` where it lists several sources, and `docs/superpowers/specs/2026-10-22-several-sources-design.md` (a dated note under N1 and L1, not a rewrite).

## 4. Approaches considered
- **A. Narrow engine fix (chosen; agy and the author agree).** Lift the CLI refusal, make `MultiPlace::remove` id-aware, keep `--break-lock` on the destination lock, fix the messages. About one cut (agy's estimate, 350-450 lines, unmeasured).
- **B. Restart as "cleanup, then fresh run" inside the CLI.** Rejected: two steps that are not atomic, it drops the ABANDONED and `superseded_by` record and the `still_owned` checks of 21.1, and it would inherit cleanup's lease wait, which the owner ruled out.
- **C. Split `--break-lock` from `--restart`.** Rejected: the CLI requires `--restart` for it (`main.rs:158`) and 21.1 couples them.
- **Break-lock reach.** P1 (destination lock only, chosen by the owner), P2 (prefix locks too, recorded; a later cut), P4 (refuse `--break-lock` with several sources; rejected, it leaves a torn destination lock unrecoverable).

## 5. Tests (each proven non-vacuous with a logic mutant, as every cut)
Engine, `crates/flux-core/src/run/multi.rs` `mod tests` and `crates/flux-core/src/run/tests.rs`:
- T1. `remove_of_a_prior_keeps_this_runs_prefix_locks` and `remove_of_this_run_releases_them` (the A1 regression; mutant: ignore the id).
- T2. A several-source run is aborted after its state exists; a second run with `--restart` supersedes it: the copy completes, the prior's partials and workspace are gone, the prior was set ABANDONED with `superseded_by` before removal, and this run's prefix locks were held through the copy.
- T3. Restart right after a crash: the prior's lock record has a heartbeat of "now"; `--restart` succeeds without waiting (the owner requirement; mutant: refuse a recent heartbeat).
- T4. An unreadable PREFIX lock refuses with `TARGET_LOCK_UNCERTAIN`, names the file and does not contain `--break-lock`; an unreadable DESTINATION lock with `--break-lock` is taken over and `state.takeover` is recorded.
- T5. A live run holding a prefix lock still refuses the restart with `TARGET_LOCK_BUSY` (A3).
- T6. A one-source TREE `--restart` over a several-source prior, and a several-source `--restart` over a one-source tree prior, both supersede.
- T7. A several-source `--restart` whose sources differ from the prior's leaves the prior's unused prefix lock file in DEST (L6) and the run succeeds; the test names the residue so a later change to it is deliberate.
CLI, `crates/flux-cli/tests/several_sources.rs`: the existing `resume_restart_and_dry_run_exit_2_naming_the_flag` loses its `--restart` arm and gains one for `--restart --dry-run` (exit 2 naming `--dry-run`); a new test restarts over an aborted several-source run and exits 0; the refusal text of a plain run over a several-source prior names `--restart`.

## 6. Constraints
Linux, Windows and macOS. Windows-only behaviour (recovering a dead prefix lock on a real NTFS handle) is shown only by the Windows CI job; the cut says so. No dependency added. No state format change. `just check` is the gate as is.

## 7. Left to the builder
Where each test lives; the wording of the prefix-lock refusal (within 3.4); whether the CLI abort fixture reuses the kill harness of `crates/flux-cli/tests/run.rs` or an engine-level abort; the exact `--help` prose.
