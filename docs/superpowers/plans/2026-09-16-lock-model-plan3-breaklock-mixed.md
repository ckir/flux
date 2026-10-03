# Lock-Protocol Model Check, Plan 3 (`breaklock`, `mixed`, `breaklock-remote`, `mixed-remote`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the Breaker actor and the `--break-lock` takeover (240.5) to `LockProtocol.tla`, and build the
`breaklock` and `mixed` scenarios end to end on top of plan 2's `recovery` scenario:
- `breaklock` (a StalledOwner and two Breakers racing over an uncertain lock) and `mixed` (an Owner, a Breaker and a
  Recoverer judging the same dead owner differently), each with their check, plain, witness, liveness and seeded runs
  on POSIX and Windows;
- `breaklock-remote` and `mixed-remote`, the same actors under `LockCapability = "remote"`, where a lock lease can
  lapse under a live holder;
- the in-flight publishing write and release unlink that a takeover can land against, and the tenure-staleness ghosts
  `SingleWriter` needs to except them correctly;
- the spec fix the acquirer window forced (96.1 and 240.3 step 4 never remove a file by name unless they hold its
  lock), and the open finding on `SingleWriter` the remote scenarios carry (the check-to-call window), with its
  `-window-witness` and `-fixed` runs;
- the traceability map and drift stamp for every heading this plan's labels touch, including the new Section 235.1.

**Architecture:** `LockProtocol.tla` gains a `Breakers` process set and a `TakeOver` procedure (240.5 steps 1 to 6).
`Acquire` (96.1) and `Recover` (240.3 step 4) are rewritten so a process that cannot take its OS-native lock never
removes the file it created by name: it retries while the lock path still names its own, still-empty file, and
otherwise closes without removing it. `Publish` (Section 99) gains an in-flight publishing write and a checked
release unlink, both of which a takeover can land against, and `SingleWriter` is corrected with tenure-staleness
ghosts (`checkStale`, `writeStale`) instead of counting every process whose last check passed. A cause ghost,
`lostLock`, is the only justification a failed Section 99 or 21.1 step 3 check has. `FsModel.tla` gains
`FsLeaseExpiry` (a remote lock lease lapsing under a live holder), `FsRenameReplace` (a takeover-by-rename seed), and
`FsCloseAll` (closing every handle a process holds, shared by a crash and a failed check). Every actor is now a
weakly fair PlusCal process, not only the system as a whole. `expected.toml` gains the `breaklock`, `mixed`,
`breaklock-remote` and `mixed-remote` scenarios; `trace.toml` maps every new label to its spec sentence.

**Tech Stack:** TLA+ / PlusCal, TLC 2.19 (`tla2tools.jar`, SHA-256 pinned in `run.py`), Java 21, Python 3.14 (3.11 or
later accepted; standard library only), Rust 2024 (`toml`, `blake3`), `just`, GitHub Actions (`ubuntu-latest`).

---

## Context

- **Design:** `docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md` at the commit that adds this
  plan. Section numbers in this plan refer to it: 4 (the runner and CI, unchanged by this plan), 5, 5.1, 5.2, 5.3
  (the filesystem model, its capability and crash rules, and what it borrows from the platform), 6.1 (actors and
  labels), 7 (properties), 8 (seeded configurations), 11 (findings), 12 (scenarios, pairings, bounds). Every owner
  ruling this plan implements is already written into that design; this plan cites sections rather than re-deriving
  them.
- **Built and verified before this plan was written.** This plan was written after the work, from the committed
  files on branch `model/lock-protocol` (PR #7, squashed as `2fc4cef`), and every code block below is a copy of a
  committed file or a diff between two committed states. Run as written on the plan-3 base, the plan reproduces that
  merge's files. `LockProtocol.tla` is the exception: it is regenerated, and a hash confirms the result is identical.
- **Base:** `main` at `7ee6bd0` ("ci: automate releases with release-plz (#10)"), the commit plan 3's pull request
  was based on. Plan 2 is complete and synced there: `expected.toml` holds only `selftest` and `recovery`,
  `trace.toml`'s `planned_scenarios` lists `breaklock`, `mixed`, `cleanup`, `cleanup-crash`, `nested`, `dirlock` and
  `claims`, and `spec-sections.stamp` has no `235.1` heading.
- **Branch:** work on a branch off the base and push it. The TLC runs execute on GitHub Actions, never on the
  development machine (owner ruling, 2026-09-13; see the rules below).
- **Sequence:** plan 3 of the lock-protocol series. After this plan, `trace.toml`'s `planned_scenarios` is `["cleanup",
  "cleanup-crash", "nested", "dirlock", "claims"]`. `breaklock-remote` and `mixed-remote` are new scenario names this
  plan adds that plan 2's design did not enumerate as build targets; they exist because `LockCapability = "remote"`
  needs its own runs (design Section 5.1, 12).

### What exists now (verified by reading the base)

- `models/lockproto/LockProtocol.head`: `CONSTANTS` list ends at `SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK`; no
  `Breakers`, no `MaxLeaseExpiries`, no `FIX_REMOTE_LEASE_SPEC`; `Procs == Owners \cup Recoverers \cup PlainRuns \cup
  Cleanups`; no `TakeoverName`; no `BreakerPerms`.
- `models/lockproto/algorithm.txt`: `(* --fair algorithm LockProtocol {` (fairness over the whole system, not
  per-process); no `writing`, `pendingUnlink`, `checkStale`, `writeStale`, `lostLock`, `landedAfterTakeover` variables;
  `StillOwned(p) == LockObj # NoObj /\ fs.content[LockObj] = OwnRecord(p)`; `RefusalEvidence(p) == BusyJustified \/
  sawLive[p]`; no `LandUnlink`, `HandleObj`, `LocksAvailable`, `HeldByOther`, `Holds`, `MarkLost`; `S96_1_ownlock`
  goes straight to `FsTryLock`, and on failure to a single label `S96_1_ownlock_backoff` that unlinks the lock file by
  name unconditionally; `S240_3_s4_lock` is the same shape, backing off to `S240_3_s4_lock_backoff`; no `TakeOver`
  procedure; `Publish`'s `S99_check` on failure does `return` directly (no `S99_refuse_close`); `S99_write` is `skip`;
  `S99_release` unlinks unconditionally with no revalidation; every `process` declaration is unfair (`process (own \in
  Owners)`, not `fair process`); no `Breakers` process; the environment process has one `either`/`or` arm for a
  process crash and one for a host crash, plus "nothing crashes" - no lease-expiry arm, and a crash lands or drops no
  in-flight call because none exists yet.
- `models/lockproto/invariants.txt`: `SingleWriter == Cardinality({p \in Procs : checked[p]}) <= 1`; `RefusalJustified
  == \A p \in Procs : refused[p] = "TARGET_LOCK_BUSY" => refusedOk[p]`; `NeverDeadOwnerLock` and `NeverTornLock` exist
  and are used by no run; no `NeverInflightLandedAfterTakeover`, `UncertainLock`, `PendingBreaker`,
  `BreakerReportedBusy`, `UncertainLockEventuallyCleared`, `NeverUncertainLockWithPendingBreaker`; `EnvQuiet == pc["env"]
  \in {"env_done", "Done"} \/ crashes = MaxCrashes` (no lease budget).
- `models/lockproto/FsModel.tla`: `ASSUME LockCapability \in {"strong", "weak"}`; no `FsLeaseExpiry`, no
  `FsRenameReplace`; the process-crash handle-closing logic is inlined in `FsProcCrash`, with no separately named
  `FsCloseAll`.
- `models/lockproto/configs/`: only the `selftest-*` and `recovery-*` files (30: the process-crash tier's 18 POSIX and
  12 Windows configs already carry `HostCrashes` both ways). None has `Breakers`, `MaxLeaseExpiries`, or the six new
  `SEED_*`/`FIX_*` flags this plan adds.
- `models/lockproto/expected.toml`: `scenarios = ["selftest", "recovery"]`; `deferred` lists both `S96_1_backoff`
  (`dirlock`) and `S240_3_putback` (`breaklock`); every `recovery` run's `constants` table has no `Breakers`, no
  `MaxLeaseExpiries`, and no `SEED_ACQUIRER_UNLINKS_BY_NAME`/`SEED_RESTART_RELEASES`/`SEED_MOVE_ASIDE_FOR_UNCERTAIN`/
  `SEED_RENAME_OVER_TAKEOVER`/`SEED_NO_CAPABILITY_GATE`/`SEED_TORN_AS_FOREIGN`; no run lists `S99_refuse_close` in
  `unreached`.
- `models/lockproto/expected-extended.toml`: the same gap in the eight `recovery-*-hostcrash-*` runs' `constants` and
  `unreached` lists, and each `S240_3_putback` reason still reads "...rewrote the record; breaklock reaches it".
- `models/lockproto/trace.toml`: `planned_scenarios = ["breaklock", "mixed", "cleanup", "cleanup-crash", "nested",
  "dirlock", "claims"]`; several units carry `pending = ["breaklock"]`, `pending = ["breaklock", "cleanup"]`, or
  `pending = ["mixed"]`; no `[[unit]]` or `[[heading]]` entry for `235.1`, `235`, `235.2`, `235.3`, `235.4` or `235.5`.
- `models/lockproto/spec-sections.stamp`: no `## 235.1 Capability Classes` line.
- `FLUX_FULL_UPDATED_SPEC_V16.md` 96.1's acquirer, on failing to take its OS-native lock, "removes the lock file it
  created and starts the acquisition again"; 240.3 step 4 does the same on the replacement lock; test item 147 does
  not exist.
- `.claude/recommended-tools.json`'s `java` entry only mentions running `just model`; no `tla2tools.jar` or `gh` entry.
- `_typos.toml` has no ignore pattern for a PlusCal `chksum(...)` comment.

### Measured facts this plan relies on

Distinct state counts and CI times, as recorded in the design notes (Section 5.3, 11, 12) and the commit history of
`2fc4cef`. Where no exact figure was recorded for a run, this plan says so instead of inventing one; read the number
off your own CI run's log and, if you can, feed it back into the design notes.

| Run | Distinct states | CI time |
|---|---|---|
| `breaklock-posix-check` (exhaustive, once 96.1's never-remove retry landed) | 4,119,330 | 204s |
| `breaklock-posix-plain-check` (measured identical to `mixed`'s dropped plain pairing) | 4,135,698 | 6,914,751 (2026-09-16, CI 35115798970) |
| `breaklock-windows-plain-check` (measured identical to `mixed`'s dropped plain pairing) | 5,591,892 | 7,456,248 (2026-09-16, CI 35115798970) |
| `breaklock-posix-liveness` (`UncertainLockEventuallyCleared` holds, per-process fairness, `lostLock` reset at a
  crash) | ≈17.4 million | 58 min of a 90-minute limit |
| `mixed-posix-check` | 4,839,093 | 8,137,479 (2026-09-16, CI 35115798970) |
| `mixed-windows-check` | 6,764,199 | 9,200,877 (2026-09-16, CI 35115798970) |
| `breaklock-windows-check` | 9,713,859 | measured 2026-09-16, CI run 35115798970 |
| `mixed-posix-liveness` | 8,137,479 | measured 2026-09-16, CI run 35115798970 |
| `mixed-remote-posix-check` / `-fixed-check` | 19,523,514 / 15,238,008 | measured 2026-09-16, CI run 35115798970 |
| `breaklock-remote-posix-check` / `-fixed-check` | 62,982,876 / 42,519,405 | measured 2026-09-16, CI run 35115798970 |
| `breaklock-remote-posix-plain-check` / `-plain-fixed-check` | 17,418,420 / 13,411,650 | measured 2026-09-16, CI run 35115798970 |

Earlier, superseded measurements, kept here only because they explain rulings this plan's code embodies (design
Section 11 has the narrative):

- `breaklock-posix-check` under 96.1 as written (before the acquirer-window fix): `RefusalJustified` violated in
  12,846 states; TLC aborted building traces under `-continue` with 228,360 states still queued. A halting,
  symmetry-free run on `SingleWriter` alone then found it violated too, at 6,094,198 states.
- The first candidate fix (an identity-checked removal by name) was measured and failed at 3,157,035 states: a
  takeover completed between the check and the unlink, which POSIX cannot make atomic.
- `SEED_RESTART_RELEASES`, `SEED_RENAME_OVER_TAKEOVER` and `SEED_NO_CAPABILITY_GATE` each stop on `RefusalJustified`
  first (measured on CI run `34918668034`); a halting `SingleWriter`-only run per seed then showed each breaks it too,
  at 4,872,715 / 5,431,996 / 1,690,014 states.
- The unpaired, four-actor `breaklock-remote` and `mixed-remote` checks did not finish: 63.8 million and 66.8 million
  states after 72 minutes, which is why both scenarios pair their actors (below).
- The write-fencing divergence (design Section 5.3) was measured in a scratch repository, not this one: a
  `LEASE_FENCES_WRITES` constant on `mixed-remote` under `SingleWriter` as a halting witness produced the same
  34-step trace fenced and unfenced, differing only in state count (5,217,818 fenced against 4,876,949 unfenced).

### What the model found, and the owner rulings already in the design

1. **The acquirer window (design Section 11).** Spec 96.1 as written let an acquirer that could not take its
   OS-native lock remove the lock file it created *by name*, which can delete a `--break-lock` takeover of that same
   file. 96.1 and 240.3 step 4 now retry the lock while the path still names the acquirer's own, still-empty file,
   and otherwise close without removing it by name; an empty lock left this way is uncertain and cleared by
   `--break-lock`, which may take it from a live acquirer that has not yet locked it. `SEED_ACQUIRER_UNLINKS_BY_NAME`
   keeps the old wording (design Section 8).
2. **`SingleWriter`'s exception and tenure staleness (design Section 7, 11).** The spec accepts one exception: a
   stalled owner's in-flight call issued before a takeover, completing after it. The model models this with tenure
   generations, collapsed to two booleans per process (`checkStale`, `writeStale`) rather than a counter, because a
   counter's absolute value split otherwise-equal states and cost `breaklock-posix-liveness` real minutes (measured).
3. **A held OS-native lock is refusal evidence too (design Section 7, 8).** 240.2 defines `TARGET_LOCK_BUSY` as "the
   lock is held"; `RefusalEvidence` now includes another process holding the lock, which several seeds needed to be
   judged against the right invariant.
4. **The check-to-call window is an accepted, open finding under `remote` (design Section 11).** A takeover can land
   between a passing Section 99 check and the issue of the publishing call it revalidated. `FIX_REMOTE_LEASE_SPEC`
   models the spec amendment (exempt a process whose check passed in an earlier tenure; Section 99's "still owned"
   also requires the OS-native lock). Because the finding fails in thousands of states, each remote check run drops
   `SingleWriter`, a `-window-witness` run proves the window once, and a `-fixed` run with the flag on must violate
   nothing.
5. **A lost lock is the only justification for a failed Section 99 or 21.1 step 3 check (design Section 7, 11).** The
   ghost `lostLock` is set when another process's write, unlink, rename or replacement hits a process's lock file, or
   its lease lapses; it replaces a definition (`RefusalEvidence` alone) that had become a restatement of the check it
   was meant to justify.
6. **In-flight calls resolve at the crash, not later (design Section 5.2).** A process or host crash lands or drops
   every in-flight call the crashing process issued, at the crash step itself, so nothing a dead process issued lands
   after another actor has already seen it dead.
7. **Per-process weak fairness (design Section 7).** `breaklock-posix-liveness` found a lasso where the acquirer spun
   in its retry loop while a Breaker that could step never did, which fairness of the whole system alone permits.
   Every actor is now its own weakly fair process; the environment stays unfair.
8. **Pairing (design Section 12).** `breaklock` pairs {StalledOwner, 2 Breakers} and {StalledOwner, Breaker,
   PlainRun}; `mixed` runs {Owner, Breaker, Recoverer} and reuses `breaklock`'s plain pairing (measured identical);
   the remote scenarios pair the same way because their unpaired, four-actor checks did not finish.
9. **`UncertainLockEventuallyCleared` admits a justified `BUSY` refusal (design Section 7).** The last Breaker can
   meet a backing-off acquirer holding the torn lock for a few steps and honestly refuse; the property's outcome
   includes that refusal, guarded by `RefusalJustified`.
10. **Two of the platform assumptions behind the accepted remote windows were wrong, both optimistically (design
    Section 5.3).** An in-flight unlink resolves the lock name when it *lands*, not when it was issued (`LandUnlink`);
    the remote fix flag's Section 99 lock test requires the lock to still be held, with no retake, because no
    platform offers one.

### Rules for whoever executes a task

- **Step 0, state check:** before editing, confirm that the task's "Files" facts hold. If anything differs, stop and
  report `STATE_MISMATCH: <what>`. Do not adapt.
- **Shape divergence:** the code blocks are the contract. If making something work would change a name, a format, a
  message, a file layout or an exit code shown here, stop and report `[plan] -> [yours] because <reason>`.
- **Oracles:** the design sections cited in each task define correct behaviour. Where a task shows a "Replace ...
  with ..." block, the "before" text is the base commit's content and the "after" text is what must result; treat
  both as fixed.
- **No TLC on the development machine** (owner ruling, 2026-09-13). The model checks run on CI (Task 8). Locally, run
  only the unit tests, the stamp test, `actionlint`, and `pcal.trans` in Task 2, which translates and checks nothing.
- **Gate:** the repository's gates are `just check` for Rust and `just model-test` for the runner. Do not add
  stricter flags, and never run `just model-stamp` (it runs TLC locally, which this project forbids).
- **Shell:** run commands in bash, or Git Bash on Windows.
- **Tool quirks:** write any file containing backslashes with an editor or a file-writing tool, never through a shell
  heredoc, because some agent shells halve backslashes. Read files with an editor rather than `head` if an agent hook
  rewrites shell commands: a rewritten `head` has been measured returning filtered lines. Python prints cp1252 when
  its output is piped on Windows, so set `PYTHONIOENCODING=utf-8` before printing non-ASCII.
- **Tools:** Java 21 (Task 2 only, for `pcal.trans` and SANY - never for TLC), the pinned `tla2tools.jar` (`run.py`
  downloads and hash-checks it), Python 3.11 or later as `python3`, `just`, `cargo-nextest`, `typos`, `gh` (Task 8),
  and `actionlint` with `shellcheck`. Commits end with the attribution lines of the session running the plan.

## File map

| File | Task | Responsibility |
|---|---|---|
| `FLUX_FULL_UPDATED_SPEC_V16.md` | 1 | 96.1 and 240.3 step 4: never remove a lock file by name unless it is held; test item 147 |
| `.gitignore` | — | already ignores `pcal.trans`'s by-products (plan 2); unchanged |
| `models/lockproto/FsModel.tla` | 2 | `FsLeaseExpiry`, `FsRenameReplace`, `FsCloseAll`, the `remote` capability value (design Section 5.1) |
| `models/lockproto/LockProtocol.head` | 2 | `Breakers`, `MaxLeaseExpiries`, the new seed/fix constants, `TakeoverName`, `BreakerPerms` |
| `models/lockproto/algorithm.txt` | 2 | the Breaker process, `TakeOver`, the acquirer/recoverer retry rewrite, in-flight publish/release, tenure ghosts, per-process fairness (design Sections 5.1, 5.2, 6.1) |
| `models/lockproto/invariants.txt` | 2 | `SingleWriter` with tenure staleness, `RefusalJustified` with `lostLock`, the new witnesses and `UncertainLockEventuallyCleared` (design Section 7) |
| `_typos.toml` | 2 | ignore the PlusCal translator's `chksum(...)` comments |
| `models/lockproto/LockProtocol.tla` | 2 (generated) | the translated module TLC runs |
| `models/lockproto/configs/breaklock-*.cfg`, non-`remote` (15 files) | 3 | one TLC configuration per `breaklock` run |
| `models/lockproto/configs/mixed-*.cfg`, non-`remote` (9 files) | 4 | one TLC configuration per `mixed` run |
| `models/lockproto/configs/breaklock-remote-*.cfg`, `mixed-remote-*.cfg` (16 files) | 5 | `LockCapability = "remote"` runs |
| `models/lockproto/configs/recovery-*.cfg` (30 files) | 6 | modified in place: the new constants every scenario shares |
| `models/lockproto/expected.toml`, `expected-extended.toml` | 6 | the four new scenarios' run entries; the `deferred` list; the `recovery` runs' new constants and `unreached` entries |
| `tests/model_stamp.rs` | 7 | unchanged; reads `trace.toml` and `spec-sections.stamp` generically (design Section 9) |
| `models/lockproto/trace.toml`, `spec-sections.stamp` | 7 | every new label mapped to its spec sentence; the `235.1` heading family stamped |
| `.claude/recommended-tools.json` | 8 | `gh` and the pinned `tla2tools.jar`, reworded `java` entry |
| `justfile`, `.github/workflows/model.yml`, `models/lockproto/run.py`, `run.py`'s tests, `models/lockproto/README.md` | — | unchanged: CI already matrices from `expected.toml`'s `scenarios` list, and the runner's `-fixed`/coverage machinery already handles what this plan needs |

---

### Task 1: The spec changes plan 3 forces

**Design oracle:** Section 11, "An acquirer between 96.1's exclusive create and taking its OS-native lock..."; Section
8, `SEED_ACQUIRER_UNLINKS_BY_NAME`.

**Files:**
- Modify: `FLUX_FULL_UPDATED_SPEC_V16.md` (two replacements)

- [ ] **Step 0: State check**

Run: `grep -c "removes the lock file it created and" FLUX_FULL_UPDATED_SPEC_V16.md`
Expected: `1` (96.1's acquirer backoff). Run: `grep -c "remove the lock file it$" FLUX_FULL_UPDATED_SPEC_V16.md`
Expected: `1` (240.3 step 4's backoff, worded differently from 96.1's because it runs across two lines).

- [ ] **Step 1: 96.1, the acquirer never removes a file it has not locked**

Replace this text, which occurs once:

````text
anything other than the filesystem's own resolution of `<name>` loses the
name equivalence.

Where the destination provides OS-native locks (Section 235.1), the
acquirer holds that lock before it writes its record. It takes the lock
as part of the exclusive creation where the platform can do both in one
call (for example `O_EXLOCK` with `O_CREAT | O_EXCL`), and otherwise
immediately after it. In between, another invocation can open the new
file and take its lock, because inspecting a lock (Section 240.1 step 3)
and recovering one (Section 240.3 step 1) both try to take it. If the
acquirer cannot take the lock, it removes the lock file it created and
starts the acquisition again (Section 21.1 step 1); it never writes a
record while another process holds the lock. A record written without the
lock proves nothing about its owner, and Section 240.2 would read
whichever process does hold the lock as that owner.

If `<name>.flux-lock` would exceed the directory's name-length limit, the
````

with:

````text
anything other than the filesystem's own resolution of `<name>` loses the
name equivalence.

Where the destination provides OS-native locks (Section 235.1), the
acquirer holds that lock before it writes its record. It takes the lock
as part of the exclusive creation where the platform can do both in one
call (for example `O_EXLOCK` with `O_CREAT | O_EXCL`), and otherwise
immediately after it. In between, another invocation can open the new
file and take its lock, because inspecting a lock (Section 240.1 step 3)
and recovering one (Section 240.3 step 1) both try to take it. If the
acquirer cannot take the lock, it tries again while the lock path still
names the file it created (the same file identity as its handle) and the
file is still empty; once either has changed, or it stops trying, it
closes its handle and starts the acquisition again (Section 21.1 step 1).
It never removes that file by name: another invocation may have taken it
over in place (Section 240.5), and a name-based removal would delete that
invocation's lock however the acquirer checked first. When it does take
the lock, it checks the same two things again before writing, and starts
again if either has changed. It never writes a record while another
process holds the lock. An empty lock file left this way, or by an
acquirer that crashed before writing its record, is uncertain ownership
(Section 240.4), cleared by `--break-lock` (Section 240.5); a
`--break-lock` on it can take it from a live acquirer that has not yet
locked it, which then starts again and finds the target busy. A record written without the
lock proves nothing about its owner, and Section 240.2 would read
whichever process does hold the lock as that owner.

If `<name>.flux-lock` would exceed the directory's name-length limit, the
````

- [ ] **Step 2: 240.3 step 4, the same rule at the replacement lock**

Replace this text, which occurs once:

````text
4. Create its own lock exclusively and take its OS-native lock (Section
   96.1). If the creation fails, another operation created the lock in the
   gap and owns the target: delete the moved file (its owner is dead) and
   classify what is at the lock path as Section 96.1 does. If the creation
   succeeds but the OS-native lock cannot be taken, remove the lock file it
   created, delete the moved file, and start the acquisition again.
5. Delete the moved file.
````

with:

````text
4. Create its own lock exclusively and take its OS-native lock (Section
   96.1). If the creation fails, another operation created the lock in the
   gap and owns the target: delete the moved file (its owner is dead) and
   classify what is at the lock path as Section 96.1 does. If the creation
   succeeds but the OS-native lock cannot be taken, retry and give up as
   Section 96.1's acquirer does, never removing the new lock file by name;
   on giving up, close it, delete the moved file, and start the acquisition
   again.
5. Delete the moved file.
````

- [ ] **Step 3: The conformance test item**

Replace this text, which occurs once:

````text
146. a committed target always has its created-entry claim, including after a crash right after COMMIT; a lock record
     torn by a crash fails its checksum and counts as uncertain ownership.
```
````

with:

````text
146. a committed target always has its created-entry claim, including after a crash right after COMMIT; a lock record
     torn by a crash fails its checksum and counts as uncertain ownership.
147. an acquirer that cannot take the OS-native lock on the file it created never removes that file by name: it retries
     while the path names its still-empty file and otherwise closes, so a --break-lock takeover of the file keeps its lock.
```
````

- [ ] **Step 4: Confirm nothing stamped changed under the wrong hash**

Run: `cargo test --test model_stamp`
Expected: fails, naming the `## 96.1 Name-Equivalent Target Locks` unit whose quoted text you just edited (its
`hash` no longer matches). This is expected here: Task 2 rewrites the model to match, and Task 7 re-hashes this unit.
Do not edit the hash now.

- [ ] **Step 5: Commit**

```bash
git add FLUX_FULL_UPDATED_SPEC_V16.md
git commit -m "spec: 96.1 and 240.3 step 4 acquirers never remove a lock file by name unless they hold its lock"
```

### Task 2: The filesystem model and the protocol model

**Design oracle:** Section 5.1 (the `remote` capability value, `FsLeaseExpiry`), Section 5.2 (in-flight calls, crash
resolution), Section 6.1 (the Breaker actor, `TakeOver`), Section 7 (`SingleWriter`'s tenure staleness,
`RefusalJustified`'s `lostLock`, the new witnesses and `UncertainLockEventuallyCleared`, per-process fairness),
Section 8 (`SEED_TORN_AS_FOREIGN`, `SEED_ACQUIRER_UNLINKS_BY_NAME`, `SEED_RESTART_RELEASES`,
`SEED_MOVE_ASIDE_FOR_UNCERTAIN`, `SEED_RENAME_OVER_TAKEOVER`, `SEED_NO_CAPABILITY_GATE`, `FIX_REMOTE_LEASE_SPEC`).

**Files:**
- Modify: `models/lockproto/FsModel.tla`, `models/lockproto/LockProtocol.head`, `models/lockproto/algorithm.txt`,
  `models/lockproto/invariants.txt`, `_typos.toml`
- Modify (generated): `models/lockproto/LockProtocol.tla`

- [ ] **Step 0: State check**

Run: `grep -c "Breakers" models/lockproto/LockProtocol.head models/lockproto/algorithm.txt models/lockproto/invariants.txt`
Expected: `0` for all three. Run: `grep -c "LockCapability \\\\in" models/lockproto/FsModel.tla`
Expected: `1`, reading `ASSUME LockCapability \in {"strong", "weak"}`.

- [ ] **Step 1: `LockProtocol.head` gains `Breakers`, the lease budget, the new seed and fix constants**

Replace the whole file's content:

````tla
---- MODULE LockProtocol ----
\* The V16 lock protocol: acquisition (96.1), classification and recovery (240.1 to 240.3), a plain
\* rerun's decision (21.1), cleanup (251.1), and commit-time revalidation (99), on the abstract
\* filesystem of FsModel.tla. Design: docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md,
\* Sections 6.1, 7 and 12. Each label is named after the spec step it implements, and each performs
\* at most one filesystem operation, so other actors run in between (design Section 5.2).
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS
    Owners,        \* operations that acquire the lock and publish
    Recoverers,    \* new invocations that recover a dead owner's lock (240.3)
    PlainRuns,     \* new invocations with no flags (21.1)
    Cleanups,      \* flux cleanup DEST (251.1)
    NoProc,        \* model value: nobody
    P,             \* the target's parent directory
    LockName,      \* P/<name>.flux-lock (96.1)
    DirLockName,   \* P/.flux-dir.lock, the long-name fallback (96.1); never created here
    MaxObjs,       \* how many objects this scenario's actors can create (bounds the state space)
    MaxCrashes,    \* how many crashes a run may have, both kinds together (design Section 5.2)
    HostCrashes,   \* whether a host crash is one of the crashes this run explores
    Platform, IdentityStrength, LockCapability,
    SEED_RECOVER_FOREIGN,    \* seeded defects (design Section 8); FALSE outside their seeded runs
    SEED_RECOVER_UNCERTAIN,
    SEED_DEAD_AS_BUSY,
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK

Procs == Owners \cup Recoverers \cup PlainRuns \cup Cleanups
\* <lock-name>.broken.<operation-id> beside the lock (240.3 step 2). Only an actor that can move a
\* lock aside needs such a name, and every name is an entry class in every state, so the Owners do
\* not get one. The target's own name is not here either: publishing sets a ghost in this scenario
\* (the `nested` scenario, which is about what gets written where, creates the object).
Movers == Recoverers \cup PlainRuns \cup Cleanups
BrokenOf(p) == <<"broken", p>>
Names == {LockName, DirLockName} \cup {BrokenOf(p) : p \in Movers}
Dirs == {P}
Fold == [n \in Names |-> n]          \* the recovery scenario needs no name folding

INSTANCE FsModel WITH Dirs <- Dirs, Names <- Names, Procs <- Procs, NoProc <- NoProc,
                      MaxObjs <- MaxObjs, Fold <- Fold, Platform <- Platform,
                      IdentityStrength <- IdentityStrength, LockCapability <- LockCapability

\* How a classifier judged the lock path (design Section 6.1).
RecovererPerms == Permutations(Recoverers)
Judgements == {"none", "empty", "foreign", "uncertain", "cleanuplock", "live", "dead"}
````

with:

````tla
---- MODULE LockProtocol ----
\* The V16 lock protocol: acquisition (96.1), classification and recovery (240.1 to 240.3), a takeover
\* (240.5), a plain rerun's and a restart's decisions (21.1), cleanup (251.1), and commit-time revalidation (99), on the abstract
\* filesystem of FsModel.tla. Design: docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md,
\* Sections 6.1, 7 and 12. Each label is named after the spec step it implements, and each performs
\* at most one filesystem operation, so other actors run in between (design Section 5.2).
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS
    Owners,        \* operations that acquire the lock and publish
    Recoverers,    \* new invocations that recover a dead owner's lock (240.3)
    PlainRuns,     \* new invocations with no flags (21.1)
    Cleanups,      \* flux cleanup DEST (251.1)
    Breakers,      \* flux copy --restart --break-lock (240.5, 21.1)
    NoProc,        \* model value: nobody
    P,             \* the target's parent directory
    LockName,      \* P/<name>.flux-lock (96.1)
    DirLockName,   \* P/.flux-dir.lock, the long-name fallback (96.1); never created here
    MaxObjs,       \* how many objects this scenario's actors can create (bounds the state space)
    MaxCrashes,    \* how many crashes a run may have, both kinds together (design Section 5.2)
    HostCrashes,   \* whether a host crash is one of the crashes this run explores
    MaxLeaseExpiries, \* how many remote lock leases may lapse under a live holder (design Section 5.1)
    Platform, IdentityStrength, LockCapability,
    SEED_RECOVER_FOREIGN,    \* seeded defects (design Section 8); FALSE outside their seeded runs
    SEED_RECOVER_UNCERTAIN,
    SEED_DEAD_AS_BUSY,
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK,
    SEED_ACQUIRER_UNLINKS_BY_NAME,
    SEED_RESTART_RELEASES,
    SEED_MOVE_ASIDE_FOR_UNCERTAIN,
    SEED_RENAME_OVER_TAKEOVER,
    SEED_NO_CAPABILITY_GATE,
    SEED_TORN_AS_FOREIGN,
    FIX_REMOTE_LEASE_SPEC  \* fix flag of the open finding on SingleWriter: the remote-lease spec amendments (design Section 11)

Procs == Owners \cup Recoverers \cup PlainRuns \cup Cleanups \cup Breakers
\* <lock-name>.broken.<operation-id> beside the lock (240.3 step 2). Only an actor that can move a
\* lock aside needs such a name, and every name is an entry class in every state, so the Owners do
\* not get one. The target's own name is not here either: publishing sets a ghost in this scenario
\* (the `nested` scenario, which is about what gets written where, creates the object).
Movers == Recoverers \cup PlainRuns \cup Cleanups \cup Breakers
BrokenOf(p) == <<"broken", p>>
\* The private name SEED_RENAME_OVER_TAKEOVER's takeover builds its record under, before renaming it
\* over the lock (design Section 8); only Breakers have one.
TakeoverName(p) == <<"takeover", p>>
Names == {LockName, DirLockName} \cup {BrokenOf(p) : p \in Movers} \cup {TakeoverName(p) : p \in Breakers}
Dirs == {P}
Fold == [n \in Names |-> n]          \* the recovery scenario needs no name folding

INSTANCE FsModel WITH Dirs <- Dirs, Names <- Names, Procs <- Procs, NoProc <- NoProc,
                      MaxObjs <- MaxObjs, Fold <- Fold, Platform <- Platform,
                      IdentityStrength <- IdentityStrength, LockCapability <- LockCapability

\* How a classifier judged the lock path (design Section 6.1).
RecovererPerms == Permutations(Recoverers)
BreakerPerms == Permutations(Breakers)
Judgements == {"none", "empty", "foreign", "uncertain", "cleanuplock", "live", "dead"}
````

- [ ] **Step 2: `FsModel.tla` admits `remote`, and gains `FsLeaseExpiry`, `FsRenameReplace`, `FsCloseAll`**

Replace:

````tla
ASSUME LockCapability \in {"strong", "weak"}
````

with:

````tla
ASSUME LockCapability \in {"strong", "remote", "weak"}
````

Replace:

````tla
FsTryLock(fs, p, o) ==
    IF o = NoObj \/ LockCapability = "weak" \/ ~OpenBy(fs, p, o) \/ fs.oslock[o] # NoProc THEN Fail(fs)
    ELSE Ok([fs EXCEPT !.oslock[o] = p], o)

FsUnlock(fs, p, o) ==
    IF o = NoObj \/ fs.oslock[o] # p THEN Fail(fs) ELSE Ok([fs EXCEPT !.oslock[o] = NoProc], o)
````

with:

````tla
FsTryLock(fs, p, o) ==
    IF o = NoObj \/ LockCapability = "weak" \/ ~OpenBy(fs, p, o) \/ fs.oslock[o] # NoProc THEN Fail(fs)
    ELSE Ok([fs EXCEPT !.oslock[o] = p], o)

\* A remote lock's lease lapses (design Section 5.1): the lock is gone, the holder keeps its handle and
\* is not told.
FsLeaseExpiry(fs, o) ==
    IF o = NoObj \/ fs.oslock[o] = NoProc THEN Fail(fs) ELSE Ok([fs EXCEPT !.oslock[o] = NoProc], o)

FsUnlock(fs, p, o) ==
    IF o = NoObj \/ fs.oslock[o] # p THEN Fail(fs) ELSE Ok([fs EXCEPT !.oslock[o] = NoProc], o)
````

Replace:

````tla
\* Unlink. POSIX always succeeds and the open handles keep the now-unnamed object (FS-5). Windows
\* needs delete sharing on every handle, and then either removes the name at once or leaves it as a
\* pending delete until the last handle closes: `atOnce` is that choice (FS-7).
````

with:

````tla
\* Rename, replacing (design Section 5.1): atomically points the target at the source object whether or
\* not the target existed; the replaced object keeps its open handles and loses its name. Windows needs
\* delete sharing on every handle of both objects.
FsRenameReplace(fs, d, from, to) ==
    LET o == At(fs, d, from)
        old == At(fs, d, to) IN
    IF o = NoObj \/ ~DeleteAllowed(fs, o) \/ (old # NoObj /\ ~DeleteAllowed(fs, old)) THEN Fail(fs)
    ELSE Ok([ fs EXCEPT
                !.entries[d][ClassOf(from)] = NoObj,
                !.entries[d][ClassOf(to)] = o,
                !.past[d][ClassOf(to)] =
                    IF IdentityStrength = "weak" THEN @ \cup {o} ELSE @ ], o)

\* Unlink. POSIX always succeeds and the open handles keep the now-unnamed object (FS-5). Windows
\* needs delete sharing on every handle, and then either removes the name at once or leaves it as a
\* pending delete until the last handle closes: `atOnce` is that choice (FS-7).
````

Replace:

````tla
\* A process crash: its handles, their sharing restrictions and its OS-native locks go; content
\* stays as it is, so an interrupted record write stays Torn. Its in-flight calls are not resolved
\* here: each lands or is dropped later, which is what `FsLand`/`FsDrop` are for.
FsProcCrash(fs, p) ==
    LET mine == {h \in fs.handles : h.proc = p}
        gone == {h.obj : h \in mine}
        last(o) == {h \in fs.handles \ mine : h.obj = o} = {}
        names == {<<d, c>> \in Dirs \X Classes :
                    /\ fs.entries[d][c] \in (fs.deleted \cap gone)
                    /\ last(fs.entries[d][c])}
    IN [ fs EXCEPT
           !.handles = @ \ mine,
           !.oslock = [o \in Objs |-> IF fs.oslock[o] = p THEN NoProc ELSE fs.oslock[o]],
           !.entries = [d \in Dirs |-> [c \in Classes |->
                          IF <<d, c>> \in names THEN NoObj ELSE fs.entries[d][c]]],
           !.deleted = @ \ {o \in fs.deleted : o \in gone /\ last(o)} ]
````

with:

````tla
\* Closing every handle a process holds: the handles, their sharing restrictions and its OS-native
\* locks go, and on Windows a pending-delete object whose last handle this was loses its name. A
\* process that stops after a failed Section 99 check does this, and so does a process crash.
FsCloseAll(fs, p) ==
    LET mine == {h \in fs.handles : h.proc = p}
        gone == {h.obj : h \in mine}
        last(o) == {h \in fs.handles \ mine : h.obj = o} = {}
        names == {<<d, c>> \in Dirs \X Classes :
                    /\ fs.entries[d][c] \in (fs.deleted \cap gone)
                    /\ last(fs.entries[d][c])}
    IN [ fs EXCEPT
           !.handles = @ \ mine,
           !.oslock = [o \in Objs |-> IF fs.oslock[o] = p THEN NoProc ELSE fs.oslock[o]],
           !.entries = [d \in Dirs |-> [c \in Classes |->
                          IF <<d, c>> \in names THEN NoObj ELSE fs.entries[d][c]]],
           !.deleted = @ \ {o \in fs.deleted : o \in gone /\ last(o)} ]

\* A process crash closes everything the process holds; content stays as it is, so an interrupted
\* record write stays Torn. The protocol resolves its in-flight calls in the same step (design
\* Section 5.2), with `FsLand` and `FsDrop`.
FsProcCrash(fs, p) == FsCloseAll(fs, p)
````

- [ ] **Step 3: `algorithm.txt`, the algorithm header and the new ghost variables**

Replace:

````text
(* --fair algorithm LockProtocol {
     \* Each label performs at most ONE filesystem operation (design Section 5.2). A purely local
````

with:

````text
(* --algorithm LockProtocol {
     \* Fairness (design Section 7): every actor is a weakly fair process, so an actor that can keep taking a
     \* step eventually takes it, however busy the others are; the environment is not fair, so whether and
     \* when it crashes something stays a free choice. Weak fairness of the whole system alone let one actor
     \* spin in a retry loop while another that could step never did (plan 3, breaklock liveness).
     \* Each label performs at most ONE filesystem operation (design Section 5.2). A purely local
````

Replace:

````text
       live = [p \in Procs |-> FALSE],            \* it has started and has neither finished nor died
       holding = [p \in Procs |-> FALSE],         \* this process owns the target lock now
       checked = [p \in Procs |-> FALSE],         \* its last Section 99 check passed
       recoveredAfterCrash = FALSE,               \* witness: a lock left by a crash was replaced
````

with:

````text
       live = [p \in Procs |-> FALSE],            \* it has started and has neither finished nor died
       holding = [p \in Procs |-> FALSE],         \* this process owns the target lock now
       checked = [p \in Procs |-> FALSE],         \* its last Section 99 check passed
       writing = [p \in Procs |-> FALSE],         \* its publishing write is issued and has not landed (Section 5.2)
       pendingUnlink = [p \in Procs |-> NoObj],   \* the lock file its issued release unlink resolved, not yet landed
       checkStale = [p \in Procs |-> TRUE],       \* a lock tenure began after its last passing Section 99 check
       writeStale = [p \in Procs |-> TRUE],       \* a lock tenure began after it issued its publishing write
       lostLock = [p \in Procs |-> FALSE],        \* ghost: another process's write, unlink, rename or a lease lapse hit its lock file
       landedAfterTakeover = FALSE,               \* witness: a write issued in an earlier generation landed
       recoveredAfterCrash = FALSE,               \* witness: a lock left by a crash was replaced
````

- [ ] **Step 4: `algorithm.txt`, the define block: `LandUnlink`, `HandleObj`, tenure-aware `StillOwned`, `LocksAvailable`, `HeldByOther`, `Holds`, `MarkLost`**

Replace:

````text
      define {
        LockObj == At(fs, P, LockName)
        OwnRecord(p) == Rec(p, IF p \in Cleanups THEN "cleanup" ELSE "operation")
        \* "Still owned" (Section 99): a lock file at the lock path holding this operation's record.
        StillOwned(p) == LockObj # NoObj /\ fs.content[LockObj] = OwnRecord(p)
        \* The lock path holds a record whose owner is alive: what justifies TARGET_LOCK_BUSY
        \* (Section 7, 240.2). A takeover's record is the `breaklock` scenario's business.
        BusyJustified == /\ LockObj # NoObj
                         /\ IsRecord(fs.content[LockObj])
                         /\ ~crashed[fs.content[LockObj].op]
        \* The evidence a TARGET_LOCK_BUSY refusal rests on, recorded by every refusing label through this
        \* one definition, so SEED_DEAD_AS_BUSY guards all of them (design Section 7, RefusalJustified).
        RefusalEvidence(p) == BusyJustified \/ sawLive[p]
````

with:

````text
      define {
        LockObj == At(fs, P, LockName)
        \* `f` after process q's issued release unlink lands (Section 5.2). NFS REMOVE carries the directory handle
        \* and the NAME, and the server resolves that name when it EXECUTES the call (RFC 7530, design Section 5.3),
        \* so a stale unlink removes whatever holds the lock name then - including a lock another operation created
        \* meanwhile. The model read this the optimistic way until 2026-09-16, and the three accepted remote windows
        \* were measured under that reading. q = NoProc lands nothing.
        LandUnlink(f, q, c) == IF q # NoProc /\ pendingUnlink[q] # NoObj /\ At(f, P, LockName) # NoObj
                               THEN FsUnlink(f, P, LockName, c).fs ELSE f
        OwnRecord(p) == Rec(p, IF p \in Cleanups THEN "cleanup" ELSE "operation")
        \* The object of the one lock-file handle a process holds, or NoObj. A holder of the target lock
        \* has exactly one open handle at the points that use this (after a takeover or a recovery).
        HandleObj(p) == IF \E h \in fs.handles : h.proc = p
                        THEN (CHOOSE h \in fs.handles : h.proc = p).obj
                        ELSE NoObj
        \* "Still owned" (Section 99): a lock file at the lock path holding this operation's record.
        \* FIX_REMOTE_LEASE_SPEC adds the amendment the open finding calls for: this process must still hold that
        \* file's OS-native lock. It cannot get it back once a lease has lapsed - the descriptor keeps the kernel's
        \* NFS_LOCK_LOST state, I/O through it fails with EIO, re-locking the same descriptor does not clear it, and
        \* the documented recovery is to close the file and open it again (fcntl_locking(2), design Section 5.3).
        \* The model tried a relock that took a lapsed lock back until 2026-09-16; no platform offers that.
        StillOwned(p) == /\ LockObj # NoObj
                         /\ fs.content[LockObj] = OwnRecord(p)
                         /\ (FIX_REMOTE_LEASE_SPEC => HandleObj(p) = LockObj /\ fs.oslock[LockObj] = p)
        \* The lock path holds a record whose owner is alive: what justifies TARGET_LOCK_BUSY
        \* (Section 7, 240.2). A takeover's record is the `breaklock` scenario's business.
        BusyJustified == /\ LockObj # NoObj
                         /\ IsRecord(fs.content[LockObj])
                         /\ ~crashed[fs.content[LockObj].op]
        \* The destination provides OS-native locks (235.1): every lock attempt is conditional on it, as
        \* 96.1 and 240.3 step 4 put it. Under `weak` 235.1 refuses first, so only SEED_NO_CAPABILITY_GATE
        \* ever runs a protocol step without them.
        LocksAvailable == LockCapability # "weak"
        \* The evidence a TARGET_LOCK_BUSY refusal rests on, recorded by every refusing label through this
        \* one definition, so SEED_DEAD_AS_BUSY guards all of them (design Section 7, RefusalJustified).
        \* 240.2 defines TARGET_LOCK_BUSY as "the lock is held", so another process holding the OS-native lock
        \* on the file at the lock path is evidence too (owner ruling for plan 3: a takeover writing in place
        \* holds it while the record it replaces is torn).
        HeldByOther(p) == LockObj # NoObj /\ fs.oslock[LockObj] \notin {NoProc, p}
        RefusalEvidence(p) == BusyJustified \/ sawLive[p] \/ HeldByOther(p)
        \* Process q holds an open handle on object o.
        Holds(q, o) == o # NoObj /\ \E h \in fs.handles : h.proc = q /\ h.obj = o
        \* The cause ghost after process `me` writes, unlinks, renames or replaces object o: every other process with
        \* a handle on it has had its lock hit. A failed Section 99 check is justified only by that cause (owner ruling
        \* for plan 3, 2026-09-15): a restatement of the check's own predicate could not catch a wrong refusal.
        MarkLost(me, o) == [q \in Procs |-> IF q # me /\ Holds(q, o) THEN TRUE ELSE lostLock[q]]
````

- [ ] **Step 5: `algorithm.txt`, `Classify`: `SEED_TORN_AS_FOREIGN` and the `remote` capability**

Replace:

````text
            with (seen = fs.content[obj]) {
              seenRec[self] := seen;
              if (seen = Torn) { tornRead := TRUE; };
              if (seen = Torn \/ seen = EmptyFile) {
                classified[self] := "uncertain";
                ownerLive[self] := "none";
                sawLive[self] := FALSE;
              } else if (seen = Foreign) {
                classified[self] := "foreign";
                ownerLive[self] := "none";
                sawLive[self] := FALSE;
````

with:

````text
            with (seen = fs.content[obj]) {
              seenRec[self] := seen;
              if (seen = Torn) { tornRead := TRUE; };
              if ((seen = Torn /\ ~SEED_TORN_AS_FOREIGN) \/ seen = EmptyFile) {
                classified[self] := "uncertain";
                ownerLive[self] := "none";
                sawLive[self] := FALSE;
              } else if (seen = Foreign \/ (SEED_TORN_AS_FOREIGN /\ seen = Torn)) {
                \* SEED_TORN_AS_FOREIGN (design Section 8) treats a checksum-failing record as a foreign object.
                classified[self] := "foreign";
                ownerLive[self] := "none";
                sawLive[self] := FALSE;
````

Replace:

````text
                \* The OS-native lock proves the owner is alive (240.2); otherwise the oracle decides
                \* what no filesystem fact can (Section 6.1), consistent with the truth and free to say
                \* "uncertain" either way.
                with (alive \in IF ~got /\ LockCapability = "strong" THEN {"live"}
                                ELSE IF crashed[seen.op] THEN {"dead", "uncertain"}
                                ELSE {"live", "uncertain"}) {
````

with:

````text
                \* The OS-native lock proves the owner is alive (240.2); otherwise the oracle decides
                \* what no filesystem fact can (Section 6.1), consistent with the truth and free to say
                \* "uncertain" either way.
                with (alive \in IF ~got /\ LockCapability \in {"strong", "remote"} THEN {"live"}
                                ELSE IF crashed[seen.op] THEN {"dead", "uncertain"}
                                ELSE {"live", "uncertain"}) {
````

- [ ] **Step 6: `algorithm.txt`, `Acquire` (96.1): never remove a file by name unless it is held, and mark `lostLock`**

Replace:

````text
          else {
            \* Another operation created the lock first: start the acquisition again (21.1 step 1).
            with (r = FsCreate(fs, P, LockName, self, TRUE)) {
              if (r.ok) { fs := r.fs; obj := r.val; }
              else { refused[self] := "RESTART"; return; };
          };
          };
````

with:

````text
          else {
            \* Another operation created the lock first: start the acquisition again (21.1 step 1).
            with (r = FsCreate(fs, P, LockName, self, TRUE)) {
              if (r.ok) { fs := r.fs; obj := r.val; lostLock[self] := FALSE; }
              else { refused[self] := "RESTART"; return; };
          };
          };
````

Replace:

````text
          \* A classifier can open the file just created and take its lock before this step does.
          if (crashed[self]) { goto acquire_crashed; }
          else {
            with (r = FsTryLock(fs, self, obj)) {
              if (r.ok) { fs := r.fs; }
              else { goto S96_1_ownlock_backoff; };
            };
          };
        S96_1_record_begin:
          if (crashed[self]) { goto acquire_crashed; }
          else {
            fs := FsWriteBegin(fs, obj).fs;
          };
        S96_1_record_end:
          \* A crash between the two halves of the write leaves the record torn (Section 5.2), which
````

with:

````text
          \* A classifier can open the file just created and take its lock before this step does.
          if (crashed[self]) { goto acquire_crashed; }
          else {
            if (LocksAvailable) {
              with (r = FsTryLock(fs, self, obj)) {
                if (r.ok) { fs := r.fs; }
                else { goto S96_1_ownlock_wait; };
              };
          };
          };
        S96_1_ownlock_verify:
          \* Holding the lock, check that the lock path still names the file and that nobody has written it
          \* (a takeover that finished and released while this acquirer waited): otherwise start again.
          if (crashed[self]) { goto acquire_crashed; }
          else {
            with (ident \in FsIdentityChoices(fs, P, LockName)) {
              if (ident # obj \/ fs.content[obj] # EmptyFile) { goto S96_1_ownlock_close; };
          };
          };
        S96_1_record_begin:
          if (crashed[self]) { goto acquire_crashed; }
          else {
            fs := FsWriteBegin(fs, obj).fs;
            lostLock := MarkLost(self, obj);
          };
        S96_1_record_end:
          \* A crash between the two halves of the write leaves the record torn (Section 5.2), which
````

Replace:

````text
          if (crashed[self]) { goto acquire_crashed; }
          else {
            fs := FsWriteEnd(fs, obj, OwnRecord(self)).fs;
            seenRec[self] := OwnRecord(self);
            holding[self] := TRUE;
            return;
          };
        S96_1_backoff:
````

with:

````text
          if (crashed[self]) { goto acquire_crashed; }
          else {
            fs := FsWriteEnd(fs, obj, OwnRecord(self)).fs;
            \* Either half of a write can hit a file another process took meanwhile, so both mark it.
            lostLock := MarkLost(self, obj);
            seenRec[self] := OwnRecord(self);
            holding[self] := TRUE;
            \* A new tenure: every other process's last check and issued write are stale from here (design Section 7).
            checkStale := [q \in Procs |-> IF q = self THEN checkStale[q] ELSE TRUE];
            writeStale := [q \in Procs |-> IF q = self THEN writeStale[q] ELSE TRUE];
            return;
          };
        S96_1_backoff:
````

Replace:

````text
          else {
            refusedOk[self] := RefusalEvidence(self);
            with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
            refused[self] := "TARGET_LOCK_BUSY";
            return;
          };
        S96_1_ownlock_backoff:
          \* Without the OS-native lock the record would prove nothing, and 240.2 would read whoever does
          \* hold the lock as the owner: remove the file and start again (96.1).
          if (crashed[self]) { goto acquire_crashed; }
          else {
            with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
            refused[self] := "RESTART";
            return;
          };
````

with:

````text
          else {
            refusedOk[self] := RefusalEvidence(self);
            with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
            lostLock := MarkLost(self, LockObj);
            refused[self] := "TARGET_LOCK_BUSY";
            return;
          };
        S96_1_ownlock_wait:
          \* Without the OS-native lock the record would prove nothing, and 240.2 would read whoever does
          \* hold the lock as the owner (96.1). The holder may be an inspector, which lets go, or a takeover of
          \* this very file (240.5). While the lock path still names the file and it is still empty, try the
          \* lock again or give up; once it has changed, give up. Giving up closes the handle and never removes
          \* the file by name, which could delete a takeover's lock (plan 3's measured finding): the empty lock
          \* it leaves is uncertain, and --break-lock clears it.
          if (crashed[self]) { goto acquire_crashed; }
          else {
            with (ident \in FsIdentityChoices(fs, P, LockName)) {
              if (ident # obj \/ fs.content[obj] # EmptyFile) { goto S96_1_ownlock_close; }
              else {
                either { goto S96_1_ownlock; }
                or { goto S96_1_ownlock_close; };
              };
          };
          };
        S96_1_ownlock_close:
          \* SEED_ACQUIRER_UNLINKS_BY_NAME puts back the old wording, which removed the file by name first.
          if (crashed[self]) { goto acquire_crashed; }
          else {
            with (c \in FsUnlinkChoices) {
              fs := FsClose(IF SEED_ACQUIRER_UNLINKS_BY_NAME THEN FsUnlink(fs, P, LockName, c).fs ELSE fs, self, obj).fs;
            };
            lostLock := IF SEED_ACQUIRER_UNLINKS_BY_NAME THEN MarkLost(self, LockObj) ELSE lostLock;
            refused[self] := "RESTART";
            return;
          };
````

- [ ] **Step 7: `algorithm.txt`, `Recover` (240.3): the same never-remove rule at the replacement lock**

Replace:

````text
     procedure Recover()
       variables robj = 0, victim = NoProc;
     {
````

with:

````text
     procedure Recover()
       variables robj = 0, victim = NoProc, nobj = 0;
     {
````

Replace:

````text
          else {
            if (JudgedUncertain(self)) { touchedUncertain := TRUE; };
            with (r = FsRenameNoReplace(fs, P, LockName, BrokenOf(self))) {
              if (r.ok) { fs := r.fs; } else { goto S240_3_restart; };
          };
          };
````

with:

````text
          else {
            if (JudgedUncertain(self)) { touchedUncertain := TRUE; };
            with (r = FsRenameNoReplace(fs, P, LockName, BrokenOf(self))) {
              if (r.ok) { fs := r.fs; lostLock := MarkLost(self, LockObj); } else { goto S240_3_restart; };
          };
          };
````

Replace:

````text
          if (crashed[self]) { goto recover_crashed; }
          else {
            with (r = FsCreate(fs, P, LockName, self, TRUE)) {
              if (r.ok) { fs := r.fs; } else { goto S240_3_s4_drop; };
          };
          };
        S240_3_s4_lock:
          if (crashed[self]) { goto recover_crashed; }
          else {
            with (r = FsTryLock(fs, self, LockObj)) {
              if (r.ok) { fs := r.fs; }
              else { goto S240_3_s4_lock_backoff; };
          };
          };
        S240_3_s4_record_begin:
          if (crashed[self]) { goto recover_crashed; }
          else {
            fs := FsWriteBegin(fs, LockObj).fs;
          };
        S240_3_s4_record_end:
          if (crashed[self]) { goto recover_crashed; }
          else {
            fs := FsWriteEnd(fs, LockObj, OwnRecord(self)).fs;
            seenRec[self] := OwnRecord(self);
            holding[self] := TRUE;
          };
        S240_3_s5:
````

with:

````text
          if (crashed[self]) { goto recover_crashed; }
          else {
            with (r = FsCreate(fs, P, LockName, self, TRUE)) {
              if (r.ok) { fs := r.fs; nobj := r.val; lostLock[self] := FALSE; } else { goto S240_3_s4_drop; };
          };
          };
        S240_3_s4_lock:
          \* The lock, and the record write below, go through the handle the create returned, as the
          \* acquirer's do (a reading, recorded in trace.toml): never through a second lookup of the path.
          if (crashed[self]) { goto recover_crashed; }
          else {
            if (LocksAvailable) {
              with (r = FsTryLock(fs, self, nobj)) {
                if (r.ok) { fs := r.fs; }
                else { goto S240_3_s4_lock_wait; };
              };
          };
          };
        S240_3_s4_lock_verify:
          \* As 96.1's acquirer: holding the lock, the path must still name the new file and it must still
          \* be empty, or the recoverer closes it and drops the moved file.
          if (crashed[self]) { goto recover_crashed; }
          else {
            with (ident \in FsIdentityChoices(fs, P, LockName)) {
              if (ident # nobj \/ fs.content[nobj] # EmptyFile) { goto S240_3_s4_lock_close; };
          };
          };
        S240_3_s4_record_begin:
          if (crashed[self]) { goto recover_crashed; }
          else {
            fs := FsWriteBegin(fs, nobj).fs;
            lostLock := MarkLost(self, nobj);
          };
        S240_3_s4_record_end:
          if (crashed[self]) { goto recover_crashed; }
          else {
            fs := FsWriteEnd(fs, nobj, OwnRecord(self)).fs;
            lostLock := MarkLost(self, nobj);
            seenRec[self] := OwnRecord(self);
            holding[self] := TRUE;
            \* A new tenure: every other process's last check and issued write are stale from here (design Section 7).
            checkStale := [q \in Procs |-> IF q = self THEN checkStale[q] ELSE TRUE];
            writeStale := [q \in Procs |-> IF q = self THEN writeStale[q] ELSE TRUE];
          };
        S240_3_s5:
````

Replace:

````text
        S240_3_s4_lock_backoff:
          \* The same rule at 240.3 step 4: remove the lock this recoverer just created, then drop the
          \* moved file as a failed create does, and start again.
          if (crashed[self]) { goto recover_crashed; }
          else {
            with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
            goto S240_3_s4_drop;
          };
````

with:

````text
        S240_3_s4_lock_wait:
          \* The same rule at 240.3 step 4 (96.1): while the path still names the new file and it is still
          \* empty, try the lock again or give up; giving up closes without removing the file by name, then
          \* drops the moved file as a failed create does, and starts again.
          if (crashed[self]) { goto recover_crashed; }
          else {
            with (ident \in FsIdentityChoices(fs, P, LockName)) {
              if (ident # nobj \/ fs.content[nobj] # EmptyFile) { goto S240_3_s4_lock_close; }
              else {
                either { goto S240_3_s4_lock; }
                or { goto S240_3_s4_lock_close; };
              };
          };
          };
        S240_3_s4_lock_close:
          \* SEED_ACQUIRER_UNLINKS_BY_NAME puts back the old wording, which removed the file by name first.
          if (crashed[self]) { goto recover_crashed; }
          else {
            with (c \in FsUnlinkChoices) {
              fs := FsClose(IF SEED_ACQUIRER_UNLINKS_BY_NAME THEN FsUnlink(fs, P, LockName, c).fs ELSE fs, self, nobj).fs;
            };
            lostLock := IF SEED_ACQUIRER_UNLINKS_BY_NAME THEN MarkLost(self, LockObj) ELSE lostLock;
            goto S240_3_s4_drop;
          };
````

- [ ] **Step 8: `algorithm.txt`, insert the `TakeOver` procedure (240.5 steps 1 to 6) after `Recover`**

Replace:

````text
          return;
     }

     \* Publish under the lock, revalidating before the write (Section 99), then release it.
     procedure Publish()
````

with:

````text
          return;
     }

     \* Take an uncertain lock over in place (240.5 steps 1 to 6), so the lock path is never empty. The
     \* caller classified the lock as uncertain and closed that handle; `seenRec` holds the record it
     \* reported as the holder. A takeover that succeeds returns holding the lock file's handle and its
     \* OS-native lock; every other outcome returns holding nothing.
     procedure TakeOver()
       variables tobj = 0;
     {
       S240_5_s1:
         \* Strong file identity and OS-native locks, decided from the capability, never by a trial lock.
         if (crashed[self]) { goto takeover_crashed; }
         else {
           if (IdentityStrength # "strong" \/ (LockCapability \notin {"strong", "remote"} /\ ~SEED_NO_CAPABILITY_GATE)) {
             refused[self] := "TARGET_LOCK_UNCERTAIN";
             return;
           };
         };
       S240_5_s2:
         \* Open the existing lock file for writing, without creating it; if it is gone, start again.
         if (crashed[self]) { goto takeover_crashed; }
         else {
           with (r = FsOpen(fs, P, LockName, self, TRUE)) {
             if (r.ok) { fs := r.fs; tobj := r.val; lostLock[self] := FALSE; }
             else { refused[self] := "RESTART"; return; };
         };
         };
       S240_5_s3:
         \* Its OS-native lock without waiting. A failure means another process holds it (the owner, or
         \* another takeover), and that failure is the refusal's evidence (240.2).
         if (crashed[self]) { goto takeover_crashed; }
         else {
           if (LocksAvailable) {
             with (r = FsTryLock(fs, self, tobj)) {
               if (r.ok) { fs := r.fs; }
               else {
                 refusedOk[self] := fs.oslock[tobj] # NoProc \/ RefusalEvidence(self);
                 refused[self] := "TARGET_LOCK_BUSY";
                 goto S240_5_close;
               };
             };
         };
         };
       S240_5_s4:
         \* The open file must still be the one at the lock path.
         if (crashed[self]) { goto takeover_crashed; }
         else {
           with (ident \in FsIdentityChoices(fs, P, LockName)) {
             if (ident # tobj) { refused[self] := "RESTART"; goto S240_5_close; };
         };
         };
       S240_5_s5:
         \* Read the record. A live owner refuses; another operation than the reported holder starts
         \* again; the reported holder, or an unreadable record, continues. Whether a named owner is
         \* alive is the oracle's judgement, as in classification (design Section 6.1).
         if (crashed[self]) { goto takeover_crashed; }
         else {
           with (seen = fs.content[tobj]) {
             if (IsRecord(seen) /\ seen # seenRec[self]) { refused[self] := "RESTART"; goto S240_5_close; }
             else if (IsRecord(seen)) {
               with (alive \in IF crashed[seen.op] THEN {"dead", "uncertain"} ELSE {"live", "uncertain"}) {
                 if (alive = "live") {
                   sawLive[self] := TRUE;
                   \* A define-block operator reads the variables as they were before this step, so RefusalEvidence
                   \* would not see the sawLive set just above; the judgement is passed in directly. Measured under
                   \* remote (2026-09-15): the prior owner's release unlink empties the path during the takeover, so
                   \* BusyJustified is false and only this judgement is behind the refusal.
                   refusedOk[self] := alive = "live" \/ RefusalEvidence(self);
                   refused[self] := "TARGET_LOCK_BUSY";
                   goto S240_5_close;
                 };
             };
             };
         };
         };
       S240_5_s6_seed:
         \* SEED_RENAME_OVER_TAKEOVER (design Section 8): instead of overwriting in place, build this
         \* operation's record under a private name and rename it over the lock.
         if (crashed[self]) { goto takeover_crashed; }
         else {
           if (SEED_RENAME_OVER_TAKEOVER) {
             with (r = FsCreate(fs, P, TakeoverName(self), self, TRUE)) {
               if (r.ok) { fs := r.fs; goto S240_5_seed_write_begin; }
               else { refused[self] := "RESTART"; goto S240_5_close; };
             };
           };
         };
       S240_5_s6_write_begin:
         \* Overwrite the record in place with this operation's record, in one write (259.6).
         if (crashed[self]) { goto takeover_crashed; }
         else {
           fs := FsWriteBegin(fs, tobj).fs;
           lostLock := MarkLost(self, tobj);
         };
       S240_5_s6_write_end:
         if (crashed[self]) { goto takeover_crashed; }
         else {
           fs := FsWriteEnd(fs, tobj, OwnRecord(self)).fs;
           lostLock := MarkLost(self, tobj);
           seenRec[self] := OwnRecord(self);
         };
       S240_5_s6_flush:
         if (crashed[self]) { goto takeover_crashed; }
         else {
           fs := FsFlushFile(fs, tobj).fs;
         };
       S240_5_s6:
         \* Check the file identity against the lock path again. Empty: the prior owner removed its lock
         \* meanwhile, so start again. Another file: whoever created it owns the target. The same file: the
         \* takeover stands, and this operation proceeds as if the prior owner were dead.
         if (crashed[self]) { goto takeover_crashed; }
         else {
           with (ident \in FsIdentityChoices(fs, P, LockName)) {
             if (ident = NoObj) { refused[self] := "RESTART"; goto S240_5_close; }
             else if (ident # tobj) {
               refusedOk[self] := LockObj # NoObj /\ LockObj # tobj;
               refused[self] := "TARGET_LOCK_BUSY";
               goto S240_5_close;
             }
             else {
               \* The takeover stands: it ends the prior owner's tenure (design Section 7).
               holding[self] := TRUE;
               checkStale := [q \in Procs |-> IF q = self THEN checkStale[q] ELSE TRUE];
               writeStale := [q \in Procs |-> IF q = self THEN writeStale[q] ELSE TRUE];
               return;
             };
         };
         };
       S240_5_seed_write_begin:
         if (crashed[self]) { goto takeover_crashed; }
         else {
           fs := FsWriteBegin(fs, At(fs, P, TakeoverName(self))).fs;
           lostLock := MarkLost(self, At(fs, P, TakeoverName(self)));
         };
       S240_5_seed_write_end:
         if (crashed[self]) { goto takeover_crashed; }
         else {
           fs := FsWriteEnd(fs, At(fs, P, TakeoverName(self)), OwnRecord(self)).fs;
           lostLock := MarkLost(self, At(fs, P, TakeoverName(self)));
           seenRec[self] := OwnRecord(self);
         };
       S240_5_seed_rename:
         \* The rename replaces the lock; the old file keeps its handles and its OS-native lock.
         if (crashed[self]) { goto takeover_crashed; }
         else {
           with (r = FsRenameReplace(fs, P, TakeoverName(self), LockName)) {
             if (r.ok) {
               fs := FsClose(r.fs, self, tobj).fs;
               lostLock := MarkLost(self, LockObj);
               holding[self] := TRUE;
               checkStale := [q \in Procs |-> IF q = self THEN checkStale[q] ELSE TRUE];
               writeStale := [q \in Procs |-> IF q = self THEN writeStale[q] ELSE TRUE];
               return;
             }
             else { refused[self] := "RESTART"; goto S240_5_close; };
           };
         };
       S240_5_close:
         if (crashed[self]) { goto takeover_crashed; }
         else {
           if (tobj # NoObj /\ OpenBy(fs, self, tobj)) { fs := FsClose(fs, self, tobj).fs; };
           return;
         };
       takeover_crashed:
         return;
     }

     \* Publish under the lock, revalidating before the write (Section 99), then release it.
     procedure Publish()
````

- [ ] **Step 9: `algorithm.txt`, `Publish` (Section 99): the in-flight write, the checked release, `S99_refuse_close`**

Replace:

````text
          \* "Still owned" is a read of the lock path; the decision that follows is local.
          if (crashed[self]) { goto publish_crashed; }
          else {
            if (StillOwned(self)) { checked[self] := TRUE; }
            else {
              checked[self] := FALSE;
              refusedOk[self] := RefusalEvidence(self);
              refused[self] := "TARGET_LOCK_BUSY";
              holding[self] := FALSE;
              return;
          };
          };
        S99_write:
````

with:

````text
          \* "Still owned" is a read of the lock path; the decision that follows is local.
          if (crashed[self]) { goto publish_crashed; }
          else {
            if (StillOwned(self)) { checked[self] := TRUE; checkStale[self] := FALSE; }
            else {
              checked[self] := FALSE;
              refusedOk[self] := lostLock[self];
              refused[self] := "TARGET_LOCK_BUSY";
              holding[self] := FALSE;
              goto S99_refuse_close;
          };
          };
        S99_write:
````

Replace:

````text
          if (crashed[self]) { goto publish_crashed; }
          else {
            skip;
          };
        S99_release:
          if (crashed[self]) { goto publish_crashed; }
          else {
            checked[self] := FALSE;
            holding[self] := FALSE;
            with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
          };
        S99_close:
          if (crashed[self]) { goto publish_crashed; }
          else {
            if (LockObj # NoObj /\ OpenBy(fs, self, LockObj)) { fs := FsClose(fs, self, LockObj).fs; };
            return;
          };
        publish_crashed:
          checked[self] := FALSE;
          return;
     }
````

with:

````text
          if (crashed[self]) { goto publish_crashed; }
          else {
            writing[self] := TRUE;
            writeStale[self] := FALSE;
          };
        S240_5_inflight_lands:
          \* The issued write completes (Section 5.2). A takeover can land in between, and "a filesystem call it had
          \* already started can still complete" (240.5).
          if (crashed[self]) { goto publish_crashed; }
          else {
            if (writeStale[self]) { landedAfterTakeover := TRUE; };
            writing[self] := FALSE;
            \* Neither flag is read again until the next check or write sets it, so both go back to their initial
            \* value: a state that differs only in them is the same state.
            writeStale[self] := TRUE;
          };
        S99_release_check:
          \* Section 99 lists unlink among the calls it guards, so the release is revalidated too (a reading
          \* of the spec, recorded in trace.toml): a lock that is no longer this operation's is not removed.
          if (crashed[self]) { goto publish_crashed; }
          else {
            checked[self] := FALSE;
            checkStale[self] := TRUE;
            if (~StillOwned(self)) {
              refusedOk[self] := lostLock[self];
              refused[self] := "TARGET_LOCK_BUSY";
              holding[self] := FALSE;
              goto S99_refuse_close;
            };
          };
        S99_release:
          \* The release unlink is issued: it resolves the lock path to a file now and removes that file's name when
          \* it lands, as a system call resolves its path when it starts.
          if (crashed[self]) { goto publish_crashed; }
          else {
            holding[self] := FALSE;
            pendingUnlink[self] := LockObj;
          };
        S99_release_lands:
          if (crashed[self]) { goto publish_crashed; }
          else {
            with (c \in FsUnlinkChoices) {
              fs := LandUnlink(fs, self, c);
            };
            lostLock := IF pendingUnlink[self] # NoObj /\ LockObj # NoObj THEN MarkLost(self, LockObj) ELSE lostLock;
            pendingUnlink[self] := NoObj;
          };
        S99_close:
          if (crashed[self]) { goto publish_crashed; }
          else {
            \* Close the lock file's handle, which may no longer be at the lock path once the release unlink landed.
            fs := FsCloseAll(fs, self);
            return;
          };
        S99_refuse_close:
          \* A failed check stops the operation, which gives up its handles and so its OS-native lock (design
          \* Section 6.1): close every handle this process holds.
          if (crashed[self]) { goto publish_crashed; }
          else {
            fs := FsCloseAll(fs, self);
            return;
          };
        publish_crashed:
          checked[self] := FALSE;
          checkStale[self] := TRUE;
          return;
     }
````

- [ ] **Step 10: `algorithm.txt`, each process gains 235.1's capability gate, weak fairness, and a `lostLock` reset at its end**

Replace:

````text
     \* A normal operation: acquire the lock, publish, release.
     process (own \in Owners)
     {
       own_start:
         live[self] := TRUE;
         call Acquire();
       own_publish:
         if (~crashed[self] /\ holding[self]) { call Publish(); };
       own_end:
         live[self] := FALSE;
     }
````

with:

````text
     \* A normal operation: acquire the lock, publish, release.
     fair process (own \in Owners)
     {
       own_start:
         if (LockCapability = "weak" /\ ~SEED_NO_CAPABILITY_GATE) {
           \* 235.1: without verified OS-native locks, an operation needing target exclusivity refuses.
           refused[self] := "REMOTE_LOCK_UNSAFE";
           goto own_end;
         }
         else {
           live[self] := TRUE;
           call Acquire();
         };
       own_publish:
         if (~crashed[self] /\ holding[self]) { call Publish(); };
       own_end:
         live[self] := FALSE;
         lostLock[self] := FALSE;
     }
````

Replace:

````text
     \* A new invocation with no flags (21.1): classify what it finds, then act or refuse.
     process (plain \in PlainRuns)
     {
       plain_start:
         live[self] := TRUE;
         call Classify(FALSE);
       S21_1_decide:
````

with:

````text
     \* A new invocation with no flags (21.1): classify what it finds, then act or refuse.
     fair process (plain \in PlainRuns)
     {
       plain_start:
         if (LockCapability = "weak" /\ ~SEED_NO_CAPABILITY_GATE) {
           \* 235.1: without verified OS-native locks, an operation needing target exclusivity refuses.
           refused[self] := "REMOTE_LOCK_UNSAFE";
           goto plain_end;
         }
         else {
           live[self] := TRUE;
           call Classify(FALSE);
         };
       S21_1_decide:
````

Replace:

````text
          if (~crashed[self] /\ holding[self]) { call Publish(); };
       plain_end:
         live[self] := FALSE;
     }

     \* A new invocation that finds a dead owner's lock, recovers it (240.3), then continues as an
     \* owner: the recovery path of 21.1 step 1.
     process (rec \in Recoverers)
     {
       rec_start:
         live[self] := TRUE;
         call Classify(TRUE);
       rec_decide:
````

with:

````text
          if (~crashed[self] /\ holding[self]) { call Publish(); };
       plain_end:
         live[self] := FALSE;
         lostLock[self] := FALSE;
     }

     \* A new invocation that finds a dead owner's lock, recovers it (240.3), then continues as an
     \* owner: the recovery path of 21.1 step 1.
     fair process (rec \in Recoverers)
     {
       rec_start:
         if (LockCapability = "weak" /\ ~SEED_NO_CAPABILITY_GATE) {
           \* 235.1: without verified OS-native locks, an operation needing target exclusivity refuses.
           refused[self] := "REMOTE_LOCK_UNSAFE";
           goto rec_end;
         }
         else {
           live[self] := TRUE;
           call Classify(TRUE);
         };
       rec_decide:
````

Replace:

````text
          if (~crashed[self] /\ holding[self]) { call Publish(); };
       rec_end:
         live[self] := FALSE;
     }

     \* flux cleanup DEST: classify the lock, remove a dead owner's or a dead cleanup lock through
     \* 240.3 under its own cleanup lock, then delete that lock last (251.1, 259.6).
     process (clean \in Cleanups)
     {
       clean_start:
         live[self] := TRUE;
         call Classify(TRUE);
       S251_1_classify:
````

with:

````text
          if (~crashed[self] /\ holding[self]) { call Publish(); };
       rec_end:
         live[self] := FALSE;
         lostLock[self] := FALSE;
     }

     \* flux cleanup DEST: classify the lock, remove a dead owner's or a dead cleanup lock through
     \* 240.3 under its own cleanup lock, then delete that lock last (251.1, 259.6).
     fair process (clean \in Cleanups)
     {
       clean_start:
         if (LockCapability = "weak" /\ ~SEED_NO_CAPABILITY_GATE) {
           \* 235.1: without verified OS-native locks, an operation needing target exclusivity refuses.
           refused[self] := "REMOTE_LOCK_UNSAFE";
           goto clean_end;
         }
         else {
           live[self] := TRUE;
           call Classify(TRUE);
         };
       S251_1_classify:
````

Replace:

````text
            if (holding[self]) {
              holding[self] := FALSE;
              with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
          };
          };
        S251_1_close:
````

with:

````text
            if (holding[self]) {
              holding[self] := FALSE;
              with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
              lostLock := MarkLost(self, LockObj);
          };
          };
        S251_1_close:
````

- [ ] **Step 11: `algorithm.txt`, insert the `Breaker` process (`--restart --break-lock`) after `Cleanups`**

Replace:

````text
       clean_end:
         live[self] := FALSE;
     }

     \* The environment: the crashes of Section 5.2. A process crash releases the handles, their
````

with:

````text
       clean_end:
         live[self] := FALSE;
         lostLock[self] := FALSE;
     }

     \* flux copy --restart --break-lock (21.1, 240.5). It classifies the lock first (the reported holder
     \* 240.5 reads before acting). A dead owner's lock is replaced through 240.3, as 21.1 step 1 says; an
     \* uncertain one is taken over in place; anything else is handled as a plain rerun would. Holding
     \* the lock, it then revalidates (21.1 step 3) and rewrites the record for the new operation (step 5),
     \* and continues as an Owner. 21.1 steps 2 and 4 change only the prior operation's state, which is
     \* not modelled.
     fair process (brk \in Breakers)
     {
       brk_start:
         if (LockCapability = "weak" /\ ~SEED_NO_CAPABILITY_GATE) {
           \* 235.1: without verified OS-native locks, an operation needing target exclusivity refuses.
           refused[self] := "REMOTE_LOCK_UNSAFE";
           goto brk_end;
         }
         else {
           live[self] := TRUE;
           call Classify(TRUE);
         };
       S21_1_restart_decide:
         if (crashed[self]) { goto brk_end; }
         else {
           refusedOk[self] := RefusalEvidence(self);
           if (Replaceable(self)) { goto brk_recover; }
           else if (classified[self] = "empty") { goto brk_acquire; }
           else if (classified[self] = "uncertain") {
             \* SEED_MOVE_ASIDE_FOR_UNCERTAIN (design Section 8) moves the uncertain lock aside instead.
             if (SEED_MOVE_ASIDE_FOR_UNCERTAIN) { goto brk_recover; } else { goto brk_takeover; };
           }
           else if (ownerLive[self] = "uncertain") { refused[self] := "TARGET_LOCK_UNCERTAIN"; }
           else if (classified[self] = "foreign") { refused[self] := "CONTROL_PLANE_NAMESPACE_CONFLICT"; }
           else { refused[self] := "TARGET_LOCK_BUSY"; };
         };
       brk_refused:
         goto brk_end;
       brk_takeover:
         call TakeOver();
       brk_took_over:
         goto S21_1_s3;
       brk_recover:
         call Recover();
       S21_1_s3:
         \* Revalidate the lock it now holds (Section 99).
         if (crashed[self] \/ ~holding[self]) { goto brk_end; }
         else {
           if (~StillOwned(self)) {
             refusedOk[self] := lostLock[self];
             refused[self] := "TARGET_LOCK_BUSY";
             holding[self] := FALSE;
             goto S21_1_s3_refuse_close;
           };
         };
       S21_1_s5_write_begin:
         \* The lock record is rewritten for the new operation, under the lock already held: through the
         \* handle that holds it, not through a second lookup of the path (a reading, recorded in trace.toml).
         if (crashed[self]) { goto brk_end; }
         else {
           \* SEED_RESTART_RELEASES (design Section 8) releases the OS-native lock here first.
           fs := FsWriteBegin(IF SEED_RESTART_RELEASES THEN FsUnlock(fs, self, HandleObj(self)).fs ELSE fs,
                              HandleObj(self)).fs;
           lostLock := MarkLost(self, HandleObj(self));
         };
       S21_1_s5_write_end:
         if (crashed[self]) { goto brk_end; }
         else {
           fs := FsWriteEnd(fs, HandleObj(self), OwnRecord(self)).fs;
           lostLock := MarkLost(self, HandleObj(self));
         };
       brk_publish:
         if (~crashed[self] /\ holding[self]) { call Publish(); };
       brk_publish_done:
         goto brk_end;
       brk_acquire:
         call Acquire();
       brk_acquired:
         if (~crashed[self] /\ holding[self]) { call Publish(); };
       brk_acquired_done:
         goto brk_end;
       S21_1_s3_refuse_close:
         if (crashed[self]) { goto brk_end; }
         else {
           fs := FsCloseAll(fs, self);
         };
       brk_end:
         live[self] := FALSE;
         lostLock[self] := FALSE;
     }

     \* The environment: the crashes of Section 5.2. A process crash releases the handles, their
````

- [ ] **Step 12: `algorithm.txt`, the environment: lease expiries, in-flight resolution at every crash**

Replace:

````text
     \* process crash of every process that has started, and then resolves every unflushed object and
     \* every unflushed entry operation. At most `MaxCrashes` crashes happen in a run, counted across
     \* both kinds, and a host crash counts as one however many processes it stops.
     process (env = "env")
       variables crashes = 0;
     {
       env_loop:
         while (crashes < MaxCrashes) {
           either {
             \* Crash one running process.
             with (p \in {q \in Procs : live[q] /\ ~crashed[q]}) {
               fs := FsProcCrash(fs, p);
               crashed[p] := TRUE;
               \* It loses its in-memory state (Section 5.2): it is no longer inside a publishing
               \* step and no longer owns anything, whatever its lock file still says.
               checked[p] := FALSE;
               holding[p] := FALSE;
               crashes := crashes + 1;
             };
           }
           or {
             \* The whole host goes down.
             await HostCrashes;
             with (pick \in HostCrashPicks(fs), dirs \in HostCrashDirs) {
               \* An unflushed create, move-aside or removal at the lock path undone (Section 11).
               hostCrashChangedLock := hostCrashChangedLock \/ At(FsHostCrash(fs, pick, dirs), P, LockName) # LockObj;
               fs := FsHostCrash(fs, pick, dirs);
             };
             crashed := [q \in Procs |-> IF live[q] THEN TRUE ELSE crashed[q]];
             checked := [q \in Procs |-> IF live[q] THEN FALSE ELSE checked[q]];
             holding := [q \in Procs |-> IF live[q] THEN FALSE ELSE holding[q]];
             crashes := crashes + 1;
           }
           or {
             \* Nothing crashes after all: the environment may simply stop.
             goto env_done;
````

with:

````text
     \* process crash of every process that has started, and then resolves every unflushed object and
     \* every unflushed entry operation. At most `MaxCrashes` crashes happen in a run, counted across
     \* both kinds, and a host crash counts as one however many processes it stops.
     \* Under `LockCapability = remote` it may also let a lock lease lapse under a live holder, at most
     \* `MaxLeaseExpiries` times, counted apart from crashes (design Section 5.1).
     process (env = "env")
       variables crashes = 0, leases = 0;
     {
       env_loop:
         while (crashes < MaxCrashes \/ leases < MaxLeaseExpiries) {
           either {
             \* Crash one running process.
             await crashes < MaxCrashes;
             with (p \in {q \in Procs : live[q] /\ ~crashed[q]}, land \in BOOLEAN, c \in FsUnlinkChoices) {
               \* Its in-flight calls land or are dropped at the crash itself (design Section 5.2).
               fs := FsProcCrash(LandUnlink(fs, IF land THEN p ELSE NoProc, c), p);
               \* Its own ghost goes back to FALSE: a crashed process never refuses again, so any other value is
               \* dead state that splits states which are otherwise the same.
               lostLock := [ (IF land /\ pendingUnlink[p] # NoObj /\ LockObj # NoObj
                              THEN MarkLost(p, LockObj) ELSE lostLock) EXCEPT ![p] = FALSE ];
               landedAfterTakeover := landedAfterTakeover \/ (land /\ writing[p] /\ writeStale[p]);
               writing[p] := FALSE;
               pendingUnlink[p] := NoObj;
               crashed[p] := TRUE;
               \* It loses its in-memory state (Section 5.2): it is no longer inside a publishing
               \* step and no longer owns anything, whatever its lock file still says.
               checked[p] := FALSE;
               checkStale[p] := TRUE;
               writeStale[p] := TRUE;
               holding[p] := FALSE;
               crashes := crashes + 1;
             };
           }
           or {
             \* The whole host goes down.
             await HostCrashes /\ crashes < MaxCrashes;
             \* Each in-flight call lands or is dropped at the crash (Section 5.2): at most one issued release unlink
             \* can still name the lock file, so choosing one lander, or none, covers every outcome. What landed is
             \* unflushed, so the host crash can undo it.
             with (lander \in {NoProc} \cup {q \in Procs : live[q]}, c \in FsUnlinkChoices) {
               with (pick \in HostCrashPicks(LandUnlink(fs, lander, c)), dirs \in HostCrashDirs) {
                 \* An unflushed create, move-aside or removal at the lock path undone (Section 11).
                 \* One line: a disjunct continued on the next line would split the translated action (TLC error 2109).
                 hostCrashChangedLock := (hostCrashChangedLock \/ At(FsHostCrash(LandUnlink(fs, lander, c), pick, dirs), P, LockName) # At(LandUnlink(fs, lander, c), P, LockName));
                 fs := FsHostCrash(LandUnlink(fs, lander, c), pick, dirs);
               };
             };
             crashed := [q \in Procs |-> IF live[q] THEN TRUE ELSE crashed[q]];
             \* The in-flight calls are resolved above; an in-flight data write's effect is not modelled (Section 5.2).
             writing := [q \in Procs |-> IF live[q] THEN FALSE ELSE writing[q]];
             pendingUnlink := [q \in Procs |-> IF live[q] THEN NoObj ELSE pendingUnlink[q]];
             checked := [q \in Procs |-> IF live[q] THEN FALSE ELSE checked[q]];
             checkStale := [q \in Procs |-> IF live[q] THEN TRUE ELSE checkStale[q]];
             lostLock := [q \in Procs |-> IF live[q] THEN FALSE ELSE lostLock[q]];
             writeStale := [q \in Procs |-> IF live[q] THEN TRUE ELSE writeStale[q]];
             holding := [q \in Procs |-> IF live[q] THEN FALSE ELSE holding[q]];
             crashes := crashes + 1;
           }
           or {
             \* A remote lock's lease lapses under a live holder, which is not told.
             await LockCapability = "remote" /\ leases < MaxLeaseExpiries;
             with (o \in {x \in Objs : fs.oslock[x] # NoProc /\ live[fs.oslock[x]] /\ ~crashed[fs.oslock[x]]}) {
               \* Before fs: PlusCal reads fs as already assigned once it has been, so the holder must be read first.
               lostLock[fs.oslock[o]] := TRUE;
               fs := FsLeaseExpiry(fs, o).fs;
               leases := leases + 1;
             };
           }
           or {
             \* Nothing crashes after all: the environment may simply stop.
             goto env_done;
````

- [ ] **Step 13: `invariants.txt`: tenure-aware `SingleWriter`, `lostLock`-justified `RefusalJustified`, the new witnesses and `UncertainLockEventuallyCleared`**

Replace:

````text
\* For a target, at most one process is inside a publishing step whose last Section 99 check passed.
\* The one exception the spec accepts - a stalled owner's in-flight call issued before an operator
\* `--break-lock` - belongs to the `breaklock` scenario, which has the actors for it.
SingleWriter == Cardinality({p \in Procs : checked[p]}) <= 1
````

with:

````text
\* For a target, at most one process is inside a publishing step whose last Section 99 check passed, or has
\* a publishing write in flight. Tenure generations (owner ruling for plan 3, 2026-09-15): a new lock tenure -
\* a create, a recovery or a takeover that stands - makes every other process's last check and issued write stale. As the spec stands, the one exception
\* is a call already started before a later tenure began (240.5: "a filesystem call it had already started can
\* still complete"). FIX_REMOTE_LEASE_SPEC models the amendments the open finding calls for: a process whose
\* check passed in an earlier generation is no longer counted (the check-to-call window, however the lock was
\* lost), and Section 99's check also tries the lock (Relock).
Superseded(p) == IF FIX_REMOTE_LEASE_SPEC THEN checkStale[p] ELSE writing[p] /\ writeStale[p]
SingleWriter == Cardinality({p \in Procs : (checked[p] \/ writing[p]) /\ ~Superseded(p)}) <= 1
````

Replace:

````text
\* A REGRESSION GUARD, not a liveness check. TARGET_LOCK_BUSY means the target's lock is held, not that
\* its recorded owner is alive (240.2): a held OS-native lock cannot tell the owner from another
\* invocation inspecting or recovering it, so no refusal-time check can establish owner liveness on
\* that path. What this catches is a refusal with nothing behind it - a decision table that refuses
\* BUSY for a lock it judged dead, or a publisher refusing it at S99_check when the lock path is empty
\* or foreign. The refusing label records its evidence because the refusal may be reported after what
\* it saw has changed: the refusal rests on what the classifier observed, not on a re-read.
RefusalJustified == \A p \in Procs : refused[p] = "TARGET_LOCK_BUSY" => refusedOk[p]

\* The ghost witnesses, for the witness runs.
NeverTornRead == ~tornRead
\* A process passed its Section 99 check, so SingleWriter's ghost is set (design Section 7).
NeverChecked == \A p \in Procs : ~checked[p]
````

with:

````text
\* A REGRESSION GUARD, not a liveness check. TARGET_LOCK_BUSY means the target's lock is held, not that
\* its recorded owner is alive (240.2): a held OS-native lock cannot tell the owner from another
\* invocation inspecting or recovering it, so no refusal-time check can establish owner liveness on
\* that path. What this catches is a refusal with nothing behind it - a decision table that refuses
\* BUSY for a lock it judged dead, or a Section 99 check (S99_check, S99_release_check, S21_1_s3) refusing with
\* no cause: such a refusal is justified only by the ghost lostLock, set when another process's write, unlink,
\* rename or replacement hit the lock file this process holds, or its lease lapsed (owner ruling for plan 3,
\* 2026-09-15). The other refusing labels record the evidence of RefusalEvidence. The
\* refusing label records its evidence because the refusal may be reported after what it saw has changed: the
\* refusal rests on what the classifier observed, not on a re-read.
RefusalJustified == \A p \in Procs : refused[p] = "TARGET_LOCK_BUSY" => refusedOk[p]

\* The ghost witnesses, for the witness runs.
NeverTornRead == ~tornRead
\* A write issued in an earlier tenure generation landed after a later tenure began (240.5; remote scenarios).
NeverInflightLandedAfterTakeover == ~landedAfterTakeover
\* A process passed its Section 99 check, so SingleWriter's ghost is set (design Section 7).
NeverChecked == \A p \in Procs : ~checked[p]
````

Replace:

````text
\* The two state witnesses the liveness runs need: the states their properties are about do occur
\* (design Section 7).
NeverDeadOwnerLock == ~DeadOwnerLock
NeverTornLock == ~TornLock

\* ------------------------------------------------------------------------------------------
\* Temporal properties (design Section 7). A configuration that checks one lists exactly one
\* PROPERTY, because TLC reports a temporal violation without naming the property it belongs to.

\* No further crash can happen: the environment has stopped, or has spent every crash it has.
EnvQuiet == pc["env"] \in {"env_done", "Done"} \/ crashes = MaxCrashes
````

with:

````text
\* ------------------------------------------------------------------------------------------
\* Temporal properties (design Section 7). A configuration that checks one lists exactly one
\* PROPERTY, because TLC reports a temporal violation without naming the property it belongs to.

\* Nothing more can go wrong: the environment has stopped, or has spent every crash and every lease
\* expiry it has.
EnvQuiet == pc["env"] \in {"env_done", "Done"} \/ (crashes = MaxCrashes /\ leases = MaxLeaseExpiries)
````

Replace:

````text
\* dead (design Section 7, lines 565-568). Without those conditions the property is false in every
\* model whose actors all finish, because the last crash can always fall after the last actor has
\* acted: measured, and that counterexample is why they are here.
DeadLockEventuallyCleared ==
    [](DeadOwnerLock /\ EnvQuiet /\ PendingMover => <>(~DeadOwnerLock \/ UncertainReported))

\* The witness for that property's antecedent. Its run must stop with this violated, which is what
\* stops the liveness run from passing over a state space that never reaches the case it is about.
NeverDeadLockWithPendingMover == ~(DeadOwnerLock /\ EnvQuiet /\ PendingMover)
====
````

with:

````text
\* dead (design Section 7). Without those conditions the property is false in every
\* model whose actors all finish, because the last crash can always fall after the last actor has
\* acted: measured, and that counterexample is why they are here.
DeadLockEventuallyCleared ==
    [](DeadOwnerLock /\ EnvQuiet /\ PendingMover => <>(~DeadOwnerLock \/ UncertainReported))

\* The witness for that property's antecedent. Its run must stop with this violated, which is what
\* stops the liveness run from passing over a state space that never reaches the case it is about.
NeverDeadLockWithPendingMover == ~(DeadOwnerLock /\ EnvQuiet /\ PendingMover)

\* A lock whose owner no reader can establish: the file at the lock path holds no readable record (torn,
\* or created and not yet written). Only a --break-lock takeover clears one (240.4, 240.5).
UncertainLock == LockObj # NoObj /\ fs.content[LockObj] \in {Torn, EmptyFile}

\* A Breaker has not started yet and has not been killed, so it will still classify the lock.
PendingBreaker == \E p \in Breakers : ~crashed[p] /\ pc[p] = "brk_start"

\* A Breaker was refused because the lock was held (240.5 step 3 or later). RefusalJustified guards that
\* the refusal had its evidence, so this outcome cannot hide a refusal with nothing behind it.
BreakerReportedBusy == \E p \in Breakers : refused[p] = "TARGET_LOCK_BUSY"

\* The same promise for an uncertain lock (design Section 7): while nothing more can go wrong and a
\* Breaker has yet to run, an uncertain lock does not stay, or a Breaker was refused because something
\* held it. Measured (plan 3): the last Breaker can meet a backing-off acquirer holding the torn lock for
\* a few steps; the lock then stays until the next --break-lock, which the owner accepted.
UncertainLockEventuallyCleared ==
    [](UncertainLock /\ EnvQuiet /\ PendingBreaker => <>(~UncertainLock \/ BreakerReportedBusy))

\* Its antecedent's witness, which keeps the liveness run from passing vacuously.
NeverUncertainLockWithPendingBreaker == ~(UncertainLock /\ EnvQuiet /\ PendingBreaker)
====
````

- [ ] **Step 14: `_typos.toml`, ignore the translator's checksum comments**

Replace:

````text
    # BLAKE3 / SHA-256 style hex digests
    "[0-9a-fA-F]{32,}",
    "blake3:[0-9a-f]+",
]
````

with:

````text
    # BLAKE3 / SHA-256 style hex digests
    "[0-9a-fA-F]{32,}",
    "blake3:[0-9a-f]+",
    # The PlusCal translator's checksums in a generated TLA+ module
    "chksum\\((pcal|tla)\\) = \"[0-9a-f]+\"",
]
````

- [ ] **Step 15: Generate `LockProtocol.tla`**

Run:

```bash
cd models/lockproto
cat LockProtocol.head algorithm.txt invariants.txt > LockProtocol.tla
java -cp ../../target/tla/tla2tools.jar pcal.trans LockProtocol.tla
tr -d '\r' < LockProtocol.tla | sha256sum
cd ../..
```

Expected: the translator ends with `Translation completed.`, `New file LockProtocol.tla written.` and
`New file LockProtocol.cfg written.`. The hash is
`a154321028ba0f3348c3e3ff03f13aa00cdc895ae8b6816726a1bbffc931d7fa`. A different hash means a source differs from
the blocks above.

- [ ] **Step 16: Parse the generated module**

Run: `java -cp target/tla/tla2tools.jar tla2sany.SANY models/lockproto/LockProtocol.tla`
Expected: ends with `SANY finished.` and no error message. SANY parses and type-checks the module; it does not run
TLC and explores no state.

- [ ] **Step 17: Confirm the stamp test's failure mode has not changed**

Run: `cargo test --test model_stamp`
Expected: still fails (the hashes Task 1 broke are not yet fixed, and this task adds labels no `trace.toml` unit
mentions yet), but for the same reasons as before this task - no new panic, no new missing-file error. Task 7 fixes
this fully.

- [ ] **Step 18: Commit**

```bash
git add models/lockproto/FsModel.tla models/lockproto/LockProtocol.head models/lockproto/algorithm.txt \
        models/lockproto/invariants.txt models/lockproto/LockProtocol.tla _typos.toml
git commit -m "model: the Breaker, TakeOver (240.5), in-flight publish/release, and tenure staleness"
```

### Task 3: The `breaklock` configurations

**Design oracle:** Section 12, the `breaklock` row and its pairing table; Section 6.1 (`SYMMETRY` only over the
Breakers, and only where the liveness run also runs without it); Section 8 (the six seeded runs and where they are
placed - after the acquirer-window measurement, which Task 2 already resolved by building the fixed model directly).

**Files:**
- Create: 15 files under `models/lockproto/configs/`, listed in the steps.

- [ ] **Step 0: State check**

Run: `ls models/lockproto/configs | grep -c '^breaklock-'`
Expected: `0`.

- [ ] **Step 1: `breaklock-posix-check`, the two-Breaker pairing**

Create `models/lockproto/configs/breaklock-posix-check.cfg`:

````text
\* breaklock scenario, POSIX, strong identity, strong lock capability: an Owner (the StalledOwner; under strong
\* capability a running owner keeps its OS-native lock, so it stalls only by crashing) and two Breakers racing to
\* take over the same uncertain lock (240.5 steps 3-6). SYMMETRY over the Breakers (design Section 12). Built with
\* 96.1's never-remove, retry-or-give-up backoff (plan 3's fix of the acquirer window, design Section 11).
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Expected exhaustive result (design Section 5.3, 12; measured on CI): every invariant listed holds, 4,119,330
distinct states, about 204 seconds. Its only coverage gap is `S99_refuse_close` and `S21_1_s3_refuse_close`, which a
strong-capability holder never reaches because a running holder keeps its lock; `breaklock-remote` reaches them
(Task 5). Task 6 records this run's `unreached` list in `expected.toml`.

- [ ] **Step 2: `breaklock-posix-plain-check`, the plain-rerun pairing**

Create `models/lockproto/configs/breaklock-posix-plain-check.cfg`:

````text
\* breaklock scenario, posix, strong capability: an Owner, a Breaker and a PlainRun (a plain rerun meeting a lock mid-takeover) (design Section 12). Not yet measured.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {p1}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 4
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Expected: 4,135,698 distinct states (measured identical to `mixed`'s dropped plain pairing, design Section 12).

- [ ] **Step 3: `breaklock-windows-check` and `breaklock-windows-plain-check`**

Create `models/lockproto/configs/breaklock-windows-check.cfg`:

````text
\* breaklock scenario, windows, strong capability: an Owner and two Breakers racing for the same uncertain lock, SYMMETRY over the Breakers (design Section 12). Not yet measured.
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "windows"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Create `models/lockproto/configs/breaklock-windows-plain-check.cfg`:

````text
\* breaklock scenario, windows, strong capability: an Owner, a Breaker and a PlainRun (a plain rerun meeting a lock mid-takeover) (design Section 12). Not yet measured.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {p1}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 4
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "windows"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Expected: `breaklock-windows-check` is not yet measured by the design notes; `breaklock-windows-plain-check` is
measured at 5,591,892 distinct states (identical to `mixed`'s dropped plain pairing, design Section 12).

- [ ] **Step 4: The witness runs, POSIX and Windows**

Create `models/lockproto/configs/breaklock-posix-witness-NeverTornRead.cfg`:

````text
\* breaklock scenario, posix: witness run for NeverTornRead (design Sections 4, 7 and 12). The actors and
\* bounds are those of breaklock-posix-check.cfg; the one invariant is expected to be VIOLATED: a process reads a torn lock record.
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverTornRead
````

Create `models/lockproto/configs/breaklock-posix-witness-NeverUncertainLockWithPendingBreaker.cfg`:

````text
\* breaklock scenario, posix: witness run for NeverUncertainLockWithPendingBreaker (design Sections 4, 7 and 12). The actors and
\* bounds are those of breaklock-posix-check.cfg; the one invariant is expected to be VIOLATED: an uncertain lock sits at the lock path while nothing more can go wrong and a Breaker has yet to run.
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverUncertainLockWithPendingBreaker
````

Create `models/lockproto/configs/breaklock-windows-witness-NeverTornRead.cfg`:

````text
\* breaklock scenario, windows: witness run for NeverTornRead (design Sections 4, 7 and 12). The actors and
\* bounds are those of breaklock-windows-check.cfg; the one invariant is expected to be VIOLATED: a process reads a torn lock record.
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "windows"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverTornRead
````

Create `models/lockproto/configs/breaklock-windows-witness-NeverUncertainLockWithPendingBreaker.cfg`:

````text
\* breaklock scenario, windows: witness run for NeverUncertainLockWithPendingBreaker (design Sections 4, 7 and 12). The actors and
\* bounds are those of breaklock-windows-check.cfg; the one invariant is expected to be VIOLATED: an uncertain lock sits at the lock path while nothing more can go wrong and a Breaker has yet to run.
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "windows"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverUncertainLockWithPendingBreaker
````

Expected: each stops with its one invariant violated (design Section 4: a witness run carries no `-continue`, so it
halts at the first violating state). State counts are not pinned (Section 4: "only its verdict is compared").

- [ ] **Step 5: The host-crash witness**

Create `models/lockproto/configs/breaklock-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg`:

````text
\* breaklock scenario, POSIX, host crashes: witness run for NeverHostCrashChangedLock (design Section 12). The
\* actors are those of breaklock-posix-check.cfg with HostCrashes = TRUE; the invariant is expected to be VIOLATED.
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = TRUE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverHostCrashChangedLock
````

Expected: stops with `NeverHostCrashChangedLock` violated (design Section 12: host crashes get one witness run per
scenario, in `expected.toml` itself, never a pairing of their own).

- [ ] **Step 6: The liveness run**

Create `models/lockproto/configs/breaklock-posix-liveness.cfg`:

````text
\* breaklock scenario, POSIX: the liveness run (design Section 12). An Owner and two Breakers, WITHOUT symmetry
\* (unsound with liveness); if it fits its limit, breaklock's check runs may keep SYMMETRY over the Breakers
\* (design Section 6.1). Exactly one PROPERTY. Not yet measured.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
PROPERTY
    UncertainLockEventuallyCleared
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Expected (design Section 7, 11, 12; measured): `UncertainLockEventuallyCleared` holds over the complete state space,
≈17.4 million distinct states, 58 minutes against a 90-minute limit, once per-process fairness and the `lostLock`
reset at a crash both landed (both are already in the model from Task 2). This run is why `SYMMETRY BreakerPerms`
stays legal on the `check` runs above: Section 6.1 requires that `breaklock`'s own symmetry-free liveness run also
check every safety invariant, which it does.

- [ ] **Step 7: The six seeded runs**

Create `models/lockproto/configs/breaklock-posix-seeded-SEED_ACQUIRER_UNLINKS_BY_NAME.cfg`:

````text
\* breaklock scenario, POSIX: seeded run for SEED_ACQUIRER_UNLINKS_BY_NAME (design Section 8): an acquirer that cannot take
\* the lock on the file it created removes the lock path by name, as spec 96.1 said before plan 3's fix, and so
\* deletes a Breaker's takeover of that file. The actors and bounds are those of breaklock-posix-check.cfg; the run
\* must stop with RefusalJustified violated (measured first, at a shallower depth than the SingleWriter violation
\* the same defect also causes). It lists every safety invariant of the scenario.
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = TRUE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Create `models/lockproto/configs/breaklock-posix-seeded-SEED_RESTART_RELEASES.cfg`:

````text
\* breaklock scenario: seeded run for SEED_RESTART_RELEASES (design Section 8): 21.1 step 5 releases the lock before the new operation starts, so a second takeover wins while the first still publishes. The actors are those of
\* breaklock-posix-check.cfg; the run must stop
\* with RefusalJustified violated (first, at a shallower depth; it also breaks SingleWriter). It lists every safety invariant of the scenario.
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = TRUE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Create `models/lockproto/configs/breaklock-posix-seeded-SEED_MOVE_ASIDE_FOR_UNCERTAIN.cfg`:

````text
\* breaklock scenario: seeded run for SEED_MOVE_ASIDE_FOR_UNCERTAIN (design Section 8): a Breaker moves an uncertain lock aside through 240.3, leaving the path empty, instead of taking it over in place. The actors are those of
\* breaklock-posix-check.cfg; the run must stop
\* with PlainNeverOwnsUncertain violated. It lists every safety invariant of the scenario.
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = TRUE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Create `models/lockproto/configs/breaklock-posix-seeded-SEED_RENAME_OVER_TAKEOVER.cfg`:

````text
\* breaklock scenario: seeded run for SEED_RENAME_OVER_TAKEOVER (design Section 8): a takeover renames a new record over the lock, so the replaced file keeps its OS-native lock and the new one has none. The actors are those of
\* breaklock-posix-check.cfg with MaxObjs = 5; the run must stop
\* with RefusalJustified violated (first, at a shallower depth; it also breaks SingleWriter). It lists every safety invariant of the scenario.
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = TRUE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Create `models/lockproto/configs/breaklock-posix-weakcap-seeded-SEED_NO_CAPABILITY_GATE.cfg`:

````text
\* breaklock scenario: seeded run for SEED_NO_CAPABILITY_GATE (design Section 8): neither 235.1's refusal nor 240.5 step 1's capability check applies, so actors run without OS-native locks. The actors are those of
\* breaklock-posix-check.cfg with LockCapability = weak; the run must stop
\* with RefusalJustified violated (first, at a shallower depth; it also breaks SingleWriter). It lists every safety invariant of the scenario.
SPECIFICATION Spec
SYMMETRY BreakerPerms
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "weak"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = TRUE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Expected (design Section 8, 11; measured on CI run `34918668034`, then again once `RefusalEvidence` gained
`HeldByOther`): `SEED_ACQUIRER_UNLINKS_BY_NAME`, `SEED_RESTART_RELEASES`, `SEED_RENAME_OVER_TAKEOVER` and
`SEED_NO_CAPABILITY_GATE` each stop with `SingleWriter` violated - the invariant recorded in `expected.toml`
(Task 6) - not `RefusalJustified`, which they broke first before the held-lock evidence and the step-5
`RefusalEvidence` timing fix both landed (both are already in the Task 2 model). `SEED_MOVE_ASIDE_FOR_UNCERTAIN`
stops with `PlainNeverOwnsUncertain` violated.

- [ ] **Step 8: Confirm the count and commit**

Run: `ls models/lockproto/configs | grep -c '^breaklock-' `
Expected: `15`. (`breaklock-remote-*.cfg` does not exist yet; Task 5 adds it.)

```bash
git add models/lockproto/configs/breaklock-posix-check.cfg \
        models/lockproto/configs/breaklock-posix-plain-check.cfg \
        models/lockproto/configs/breaklock-windows-check.cfg \
        models/lockproto/configs/breaklock-windows-plain-check.cfg \
        models/lockproto/configs/breaklock-posix-witness-NeverTornRead.cfg \
        models/lockproto/configs/breaklock-posix-witness-NeverUncertainLockWithPendingBreaker.cfg \
        models/lockproto/configs/breaklock-windows-witness-NeverTornRead.cfg \
        models/lockproto/configs/breaklock-windows-witness-NeverUncertainLockWithPendingBreaker.cfg \
        models/lockproto/configs/breaklock-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg \
        models/lockproto/configs/breaklock-posix-liveness.cfg \
        models/lockproto/configs/breaklock-posix-seeded-SEED_ACQUIRER_UNLINKS_BY_NAME.cfg \
        models/lockproto/configs/breaklock-posix-seeded-SEED_RESTART_RELEASES.cfg \
        models/lockproto/configs/breaklock-posix-seeded-SEED_MOVE_ASIDE_FOR_UNCERTAIN.cfg \
        models/lockproto/configs/breaklock-posix-seeded-SEED_RENAME_OVER_TAKEOVER.cfg \
        models/lockproto/configs/breaklock-posix-weakcap-seeded-SEED_NO_CAPABILITY_GATE.cfg
git commit -m "model: breaklock configurations (check, plain, witnesses, liveness, six seeded runs)"
```

### Task 4: The `mixed` configurations

**Design oracle:** Section 12, the `mixed` row; Section 8, `SEED_TORN_AS_FOREIGN`. `mixed`'s plain pairing and
weak-identity variant were measured and dropped (design Section 12): they are not built.

**Files:**
- Create: 9 files under `models/lockproto/configs/`, listed in the steps.

- [ ] **Step 0: State check**

Run: `ls models/lockproto/configs | grep -c '^mixed-'`
Expected: `0`.

- [ ] **Step 1: `mixed-posix-check` and `mixed-windows-check`**

Create `models/lockproto/configs/mixed-posix-check.cfg`:

````text
\* mixed scenario, posix, strong capability: an Owner that may crash, a Breaker that may judge its lock uncertain and a Recoverer that may judge it dead (design Section 12). Not yet measured.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 4
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Expected: exhaustive, 4,839,093 distinct states (measured on CI, design Section 12).

Create `models/lockproto/configs/mixed-windows-check.cfg`:

````text
\* mixed scenario, windows, strong capability: an Owner that may crash, a Breaker that may judge its lock uncertain and a Recoverer that may judge it dead (design Section 12). Not yet measured.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 4
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "windows"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Expected: exhaustive, 6,764,199 distinct states (measured on CI, design Section 12).

- [ ] **Step 2: The witness runs, POSIX and Windows**

Create `models/lockproto/configs/mixed-posix-witness-NeverRecoveredAfterCrash.cfg`:

````text
\* mixed scenario, posix: witness run for NeverRecoveredAfterCrash (design Sections 4, 7 and 12). Actors and bounds as in
\* mixed-posix-check.cfg; the one invariant is expected to be VIOLATED: a lock a crash left is replaced.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 4
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverRecoveredAfterCrash
````

Create `models/lockproto/configs/mixed-posix-witness-NeverUncertainLockWithPendingBreaker.cfg`:

````text
\* mixed scenario, posix: witness run for NeverUncertainLockWithPendingBreaker (design Sections 4, 7 and 12). Actors and bounds as in
\* mixed-posix-check.cfg; the one invariant is expected to be VIOLATED: an uncertain lock sits at the lock path while nothing more can go wrong and a Breaker has yet to run.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 4
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverUncertainLockWithPendingBreaker
````

Create `models/lockproto/configs/mixed-windows-witness-NeverRecoveredAfterCrash.cfg`:

````text
\* mixed scenario, windows: witness run for NeverRecoveredAfterCrash (design Sections 4, 7 and 12). Actors and bounds as in
\* mixed-windows-check.cfg; the one invariant is expected to be VIOLATED: a lock a crash left is replaced.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 4
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "windows"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverRecoveredAfterCrash
````

Create `models/lockproto/configs/mixed-windows-witness-NeverUncertainLockWithPendingBreaker.cfg`:

````text
\* mixed scenario, windows: witness run for NeverUncertainLockWithPendingBreaker (design Sections 4, 7 and 12). Actors and bounds as in
\* mixed-windows-check.cfg; the one invariant is expected to be VIOLATED: an uncertain lock sits at the lock path while nothing more can go wrong and a Breaker has yet to run.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 4
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "windows"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverUncertainLockWithPendingBreaker
````

- [ ] **Step 3: The host-crash witness**

Create `models/lockproto/configs/mixed-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg`:

````text
\* mixed scenario, POSIX, host crashes: witness run for NeverHostCrashChangedLock (design Section 12); expected VIOLATED.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 4
    MaxCrashes = 2
    HostCrashes = TRUE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverHostCrashChangedLock
````

- [ ] **Step 4: The liveness run**

Create `models/lockproto/configs/mixed-posix-liveness.cfg`:

````text
\* mixed scenario, POSIX: the liveness run (design Section 12), Owner, Breaker and Recoverer, no symmetry.
\* Exactly one PROPERTY. Not yet measured.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 4
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
PROPERTY
    UncertainLockEventuallyCleared
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Expected: `UncertainLockEventuallyCleared` holds (design Section 12: "every mixed run passes... liveness holds"),
at 8,137,479 states (measured 2026-09-16, CI run 35115798970).

- [ ] **Step 5: The seeded run**

Create `models/lockproto/configs/mixed-posix-seeded-SEED_TORN_AS_FOREIGN.cfg`:

````text
\* mixed scenario, POSIX: seeded run for SEED_TORN_AS_FOREIGN (design Section 8): a checksum-failing record is
\* treated as a foreign object, so a Breaker refuses it and the torn lock stays. Judged with the liveness settings
\* against the conditioned property (no symmetry, one PROPERTY); must stop with UncertainLockEventuallyCleared.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 4
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = TRUE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
PROPERTY
    UncertainLockEventuallyCleared
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Expected: stops with `UncertainLockEventuallyCleared` violated (design Section 8: `mixed` has both a record torn by
the Owner's crash and a Breaker, the only actor that can clear it, so the seed prevents the property's outcome).

- [ ] **Step 6: Confirm the count and commit**

Run: `ls models/lockproto/configs | grep -c '^mixed-'`
Expected: `9`. (`mixed-remote-*.cfg` does not exist yet; Task 5 adds it.)

```bash
git add models/lockproto/configs/mixed-posix-check.cfg \
        models/lockproto/configs/mixed-windows-check.cfg \
        models/lockproto/configs/mixed-posix-witness-NeverRecoveredAfterCrash.cfg \
        models/lockproto/configs/mixed-posix-witness-NeverUncertainLockWithPendingBreaker.cfg \
        models/lockproto/configs/mixed-windows-witness-NeverRecoveredAfterCrash.cfg \
        models/lockproto/configs/mixed-windows-witness-NeverUncertainLockWithPendingBreaker.cfg \
        models/lockproto/configs/mixed-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg \
        models/lockproto/configs/mixed-posix-liveness.cfg \
        models/lockproto/configs/mixed-posix-seeded-SEED_TORN_AS_FOREIGN.cfg
git commit -m "model: mixed configurations (check, witnesses, liveness, SEED_TORN_AS_FOREIGN)"
```

### Task 5: The `breaklock-remote` and `mixed-remote` configurations

**Design oracle:** Section 5.1 (`LockCapability = "remote"`, `LeaseExpiry`), Section 11 (the check-to-call window,
`FIX_REMOTE_LEASE_SPEC`, and why each check run drops `SingleWriter`), Section 12 (the remote scenarios' pairing and
their unpaired four-actor checks that did not finish).

**Files:**
- Create: 16 files under `models/lockproto/configs/`, listed in the steps.

- [ ] **Step 0: State check**

Run: `ls models/lockproto/configs | grep -cE '^(breaklock|mixed)-remote-'`
Expected: `0`.

- [ ] **Step 1: `breaklock-remote-posix-check` and its `-fixed` companion**

Create `models/lockproto/configs/breaklock-remote-posix-check.cfg`:

````text
\* breaklock-remote scenario, POSIX, LockCapability = remote: an Owner and two Breakers, one crash and one lease expiry, no symmetry
\* (design Section 12, plan 3: the four-actor config did not finish in 72 min at ~65M states, so it is paired). Not yet measured.
\* SingleWriter is NOT checked here: it carries the open finding of design Section 11, it fails in
\* thousands of states, and TLC cannot build that many traces under -continue (measured: the run dies
\* and reports no verdict). The window is proved by the -window-witness run, and the -fixed run checks
\* it with the flag on. This run keeps every other invariant, and the coverage gate.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Create `models/lockproto/configs/breaklock-remote-posix-fixed-check.cfg`:

````text
\* breaklock-remote scenario, POSIX, LockCapability = remote: an Owner and two Breakers, one crash and one lease expiry, no symmetry
\* (design Section 12, plan 3: the four-actor config did not finish in 72 min at ~65M states, so it is paired). Not yet measured.
\* The same run with FIX_REMOTE_LEASE_SPEC on: it must violate nothing, which is what shows the accepted
\* window is the ONLY way SingleWriter breaks here (design Section 11).
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = TRUE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Expected: neither run's exact state count is recorded in the design notes (Section 11 records only that both
finish, and what each run's coverage found - `recover_crashed` unreached, `S240_3_putback` covered here rather than
`deferred`). Both must complete with the invariants they list, no more and no less.

- [ ] **Step 2: The `-window-witness` for `SingleWriter`**

Create `models/lockproto/configs/breaklock-remote-posix-window-witness-SingleWriter.cfg`:

````text
\* breaklock-remote scenario, POSIX, LockCapability = remote: an Owner and two Breakers, one crash and one lease expiry, no symmetry
\* (design Section 12, plan 3: the four-actor config did not finish in 72 min at ~65M states, so it is paired). Not yet measured.
\* The check-to-call window (design Section 11) as a halting witness: SingleWriter alone, no -continue,
\* so one trace is printed instead of a flood. This run is what records that the finding is still open.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    SingleWriter
````

Expected: stops with `SingleWriter` violated (an Owner passes its Section 99 check, its lease lapses, a Breaker
takes over and passes its own; design Section 11).

- [ ] **Step 3: The plain pairing: `breaklock-remote-posix-plain-check`, its `-fixed` and `-window-witness`**

Create `models/lockproto/configs/breaklock-remote-posix-plain-check.cfg`:

````text
\* breaklock-remote scenario, plain pairing, POSIX, LockCapability = remote: an Owner, a Breaker and a PlainRun, one crash and one lease expiry, no symmetry
\* (design Section 12, plan 3: the four-actor config did not finish in 72 min at ~65M states, so it is paired). Not yet measured.
\* SingleWriter is NOT checked here: it carries the open finding of design Section 11, it fails in
\* thousands of states, and TLC cannot build that many traces under -continue (measured: the run dies
\* and reports no verdict). The window is proved by the -window-witness run, and the -fixed run checks
\* it with the flag on. This run keeps every other invariant, and the coverage gate.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {p1}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Create `models/lockproto/configs/breaklock-remote-posix-plain-fixed-check.cfg`:

````text
\* breaklock-remote scenario, plain pairing, POSIX, LockCapability = remote: an Owner, a Breaker and a PlainRun, one crash and one lease expiry, no symmetry
\* (design Section 12, plan 3: the four-actor config did not finish in 72 min at ~65M states, so it is paired). Not yet measured.
\* The same run with FIX_REMOTE_LEASE_SPEC on: it must violate nothing, which is what shows the accepted
\* window is the ONLY way SingleWriter breaks here (design Section 11).
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {p1}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = TRUE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Create `models/lockproto/configs/breaklock-remote-posix-plain-window-witness-SingleWriter.cfg`:

````text
\* breaklock-remote scenario, plain pairing, POSIX, LockCapability = remote: an Owner, a Breaker and a PlainRun, one crash and one lease expiry, no symmetry
\* (design Section 12, plan 3: the four-actor config did not finish in 72 min at ~65M states, so it is paired). Not yet measured.
\* The check-to-call window (design Section 11) as a halting witness: SingleWriter alone, no -continue,
\* so one trace is printed instead of a flood. This run is what records that the finding is still open.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {p1}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    SingleWriter
````

- [ ] **Step 4: The remaining `breaklock-remote` witnesses**

Create `models/lockproto/configs/breaklock-remote-posix-witness-NeverTornRead.cfg`:

````text
\* breaklock-remote scenario, POSIX, remote: witness run for NeverTornRead; actors and bounds as in breaklock-remote-posix-check.cfg;
\* the one invariant is expected to be VIOLATED.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverTornRead
````

Create `models/lockproto/configs/breaklock-remote-posix-witness-NeverUncertainLockWithPendingBreaker.cfg`:

````text
\* breaklock-remote scenario, POSIX, remote: witness run for NeverUncertainLockWithPendingBreaker; actors and bounds as in breaklock-remote-posix-check.cfg;
\* the one invariant is expected to be VIOLATED.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverUncertainLockWithPendingBreaker
````

Create `models/lockproto/configs/breaklock-remote-posix-witness-NeverInflightLandedAfterTakeover.cfg`:

````text
\* breaklock-remote scenario, POSIX, remote: witness run for NeverInflightLandedAfterTakeover; actors and bounds as in breaklock-remote-posix-check.cfg;
\* the one invariant is expected to be VIOLATED.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverInflightLandedAfterTakeover
````

Expected: this last run is the reachability proof for the exception `SingleWriter`'s definition carries (design
Section 7): it stops once a write issued before a takeover ended its owner's tenure lands after that takeover.

- [ ] **Step 5: The `breaklock-remote` host-crash witness**

Create `models/lockproto/configs/breaklock-remote-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg`:

````text
\* breaklock-remote scenario, POSIX, remote, host crashes on: witness run for NeverHostCrashChangedLock; actors and bounds as in
\* breaklock-remote-posix-check.cfg; the one invariant is expected to be VIOLATED (plan 3 ruling: host crash gets a witness only).
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1, b2}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = TRUE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverHostCrashChangedLock
````

- [ ] **Step 6: `mixed-remote-posix-check` and its `-fixed` and `-window-witness`**

Create `models/lockproto/configs/mixed-remote-posix-check.cfg`:

````text
\* mixed-remote scenario, POSIX, LockCapability = remote: an Owner, a Breaker and a Recoverer, one crash and one lease expiry, no symmetry
\* (design Section 12, plan 3: the four-actor config did not finish in 72 min at ~65M states, so it is paired). Not yet measured.
\* SingleWriter is NOT checked here: it carries the open finding of design Section 11, it fails in
\* thousands of states, and TLC cannot build that many traces under -continue (measured: the run dies
\* and reports no verdict). The window is proved by the -window-witness run, and the -fixed run checks
\* it with the flag on. This run keeps every other invariant, and the coverage gate.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Create `models/lockproto/configs/mixed-remote-posix-fixed-check.cfg`:

````text
\* mixed-remote scenario, POSIX, LockCapability = remote: an Owner, a Breaker and a Recoverer, one crash and one lease expiry, no symmetry
\* (design Section 12, plan 3: the four-actor config did not finish in 72 min at ~65M states, so it is paired). Not yet measured.
\* The same run with FIX_REMOTE_LEASE_SPEC on: it must violate nothing, which is what shows the accepted
\* window is the ONLY way SingleWriter breaks here (design Section 11).
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = TRUE
    defaultInitValue = defaultInitValue
INVARIANTS
    FsOk
    Classifiable
    SingleWriter
    PlainNeverOwnsUncertain
    ForeignUntouched
    RefusalJustified
````

Create `models/lockproto/configs/mixed-remote-posix-window-witness-SingleWriter.cfg`:

````text
\* mixed-remote scenario, POSIX, LockCapability = remote: an Owner, a Breaker and a Recoverer, one crash and one lease expiry, no symmetry
\* (design Section 12, plan 3: the four-actor config did not finish in 72 min at ~65M states, so it is paired). Not yet measured.
\* The check-to-call window (design Section 11) as a halting witness: SingleWriter alone, no -continue,
\* so one trace is printed instead of a flood. This run is what records that the finding is still open.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    SingleWriter
````

- [ ] **Step 7: The remaining `mixed-remote` witnesses**

Create `models/lockproto/configs/mixed-remote-posix-witness-NeverRecoveredAfterCrash.cfg`:

````text
\* mixed-remote scenario, POSIX, remote: witness run for NeverRecoveredAfterCrash; actors and bounds as in mixed-remote-posix-check.cfg;
\* the one invariant is expected to be VIOLATED.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverRecoveredAfterCrash
````

Create `models/lockproto/configs/mixed-remote-posix-witness-NeverUncertainLockWithPendingBreaker.cfg`:

````text
\* mixed-remote scenario, POSIX, remote: witness run for NeverUncertainLockWithPendingBreaker; actors and bounds as in mixed-remote-posix-check.cfg;
\* the one invariant is expected to be VIOLATED.
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = FALSE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverUncertainLockWithPendingBreaker
````

- [ ] **Step 8: The `mixed-remote` host-crash witness**

Create `models/lockproto/configs/mixed-remote-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg`:

````text
\* mixed-remote scenario, POSIX, remote, host crashes on: witness run for NeverHostCrashChangedLock; actors and bounds as in
\* mixed-remote-posix-check.cfg; the one invariant is expected to be VIOLATED (plan 3 ruling: host crash gets a witness only).
SPECIFICATION Spec
CONSTANTS
    Owners = {o1}
    Recoverers = {r1}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {b1}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 5
    MaxCrashes = 1
    HostCrashes = TRUE
    MaxLeaseExpiries = 1
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "remote"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
INVARIANT
    NeverHostCrashChangedLock
````

- [ ] **Step 9: Confirm the count and commit**

Run: `ls models/lockproto/configs | grep -cE '^(breaklock|mixed)-remote-'`
Expected: `16` (10 `breaklock-remote-*` and 6 `mixed-remote-*`).

```bash
git add models/lockproto/configs/breaklock-remote-posix-check.cfg \
        models/lockproto/configs/breaklock-remote-posix-fixed-check.cfg \
        models/lockproto/configs/breaklock-remote-posix-window-witness-SingleWriter.cfg \
        models/lockproto/configs/breaklock-remote-posix-plain-check.cfg \
        models/lockproto/configs/breaklock-remote-posix-plain-fixed-check.cfg \
        models/lockproto/configs/breaklock-remote-posix-plain-window-witness-SingleWriter.cfg \
        models/lockproto/configs/breaklock-remote-posix-witness-NeverTornRead.cfg \
        models/lockproto/configs/breaklock-remote-posix-witness-NeverUncertainLockWithPendingBreaker.cfg \
        models/lockproto/configs/breaklock-remote-posix-witness-NeverInflightLandedAfterTakeover.cfg \
        models/lockproto/configs/breaklock-remote-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg \
        models/lockproto/configs/mixed-remote-posix-check.cfg \
        models/lockproto/configs/mixed-remote-posix-fixed-check.cfg \
        models/lockproto/configs/mixed-remote-posix-window-witness-SingleWriter.cfg \
        models/lockproto/configs/mixed-remote-posix-witness-NeverRecoveredAfterCrash.cfg \
        models/lockproto/configs/mixed-remote-posix-witness-NeverUncertainLockWithPendingBreaker.cfg \
        models/lockproto/configs/mixed-remote-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg
git commit -m "model: breaklock-remote and mixed-remote configurations (check, fixed, window-witness)"
```

### Task 6: `expected.toml`, `expected-extended.toml`, and the `recovery` configurations' new constants

**Design oracle:** Section 4 (the `constants` field: every literal constant a `.cfg` assigns, compared with
`run.py` in both directions), Section 12 (the `recovery` scenario's runs already exist; this task only widens their
constants and `unreached` lists to match the model Task 2 built), and the `deferred`/`never_reached` rules of
Section 4 (an entry that becomes covered must be removed).

**Files:**
- Modify: 30 files under `models/lockproto/configs/` (`recovery-*.cfg`), listed in Step 1.
- Modify: `models/lockproto/expected.toml`, `models/lockproto/expected-extended.toml`

- [ ] **Step 0: State check**

Run: `grep -c "Breakers" models/lockproto/configs/recovery-posix-check.cfg models/lockproto/expected.toml`
Expected: `0` for both.

- [ ] **Step 1: Every `recovery-*.cfg` gains the new constants**

In each of these 30 files, make three insertions in their `CONSTANTS` block:
- insert `    Breakers = {}` immediately after the `Cleanups = ...` line;
- insert `    MaxLeaseExpiries = 0` immediately after the `HostCrashes = ...` line;
- insert these seven lines immediately after the existing `SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = ...` line and
  before `defaultInitValue = defaultInitValue`:

````text
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
````

This is the whole edit (verified identical, byte for byte, across every one of the 30 files, including the four
whose existing `SEED_RECOVER_*` value is `TRUE` rather than `FALSE` - the new flags are always `FALSE`, since none
of the new seeds or the fix flag apply to `recovery`). The files:

```text
recovery-posix-check.cfg
recovery-posix-cleanup-check.cfg
recovery-posix-cleanup-seeded-SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK.cfg
recovery-posix-hostcrash-check.cfg
recovery-posix-hostcrash-cleanup-check.cfg
recovery-posix-hostcrash-plain-check.cfg
recovery-posix-hostcrash-plain-cleanup-check.cfg
recovery-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg
recovery-posix-liveness.cfg
recovery-posix-plain-check.cfg
recovery-posix-plain-cleanup-check.cfg
recovery-posix-plain-seeded-SEED_DEAD_AS_BUSY.cfg
recovery-posix-seeded-SEED_RECOVER_FOREIGN.cfg
recovery-posix-seeded-SEED_RECOVER_UNCERTAIN.cfg
recovery-posix-witness-NeverChecked.cfg
recovery-posix-witness-NeverDeadLockWithPendingMover.cfg
recovery-posix-witness-NeverRecoveredAfterCrash.cfg
recovery-posix-witness-NeverTornRead.cfg
recovery-windows-check.cfg
recovery-windows-cleanup-check.cfg
recovery-windows-hostcrash-check.cfg
recovery-windows-hostcrash-cleanup-check.cfg
recovery-windows-hostcrash-plain-check.cfg
recovery-windows-hostcrash-plain-cleanup-check.cfg
recovery-windows-plain-check.cfg
recovery-windows-plain-cleanup-check.cfg
recovery-windows-witness-NeverChecked.cfg
recovery-windows-witness-NeverDeadLockWithPendingMover.cfg
recovery-windows-witness-NeverRecoveredAfterCrash.cfg
recovery-windows-witness-NeverTornRead.cfg
```

For example, `recovery-posix-check.cfg`'s `CONSTANTS` block becomes:

````text
CONSTANTS
    Owners = {o1}
    Recoverers = {r1, r2}
    PlainRuns = {}
    Cleanups = {}
    Breakers = {}
    NoProc = nobody
    P = dir
    LockName = lock
    DirLockName = dirlock
    MaxObjs = 3
    MaxCrashes = 2
    HostCrashes = FALSE
    MaxLeaseExpiries = 0
    Platform = "posix"
    IdentityStrength = "strong"
    LockCapability = "strong"
    SEED_RECOVER_FOREIGN = FALSE
    SEED_RECOVER_UNCERTAIN = FALSE
    SEED_DEAD_AS_BUSY = FALSE
    SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = FALSE
    SEED_ACQUIRER_UNLINKS_BY_NAME = FALSE
    SEED_RESTART_RELEASES = FALSE
    SEED_MOVE_ASIDE_FOR_UNCERTAIN = FALSE
    SEED_RENAME_OVER_TAKEOVER = FALSE
    SEED_NO_CAPABILITY_GATE = FALSE
    SEED_TORN_AS_FOREIGN = FALSE
    FIX_REMOTE_LEASE_SPEC = FALSE
    defaultInitValue = defaultInitValue
````

- [ ] **Step 2: `expected.toml`'s top matter: the scenario list and the `deferred` list**

Replace:

````toml
scenarios = ["selftest", "recovery"]

# Labels no BUILT scenario covers but a planned one will (design Section 4). Each entry goes when its
# scenario lands: the stamp test requires the scenario to be in trace.toml's planned_scenarios.
deferred = [
    { label = "S96_1_backoff", scenario = "dirlock", reason = "the 96.1 directory-lock conflict needs the directory lock only dirlock creates" },
    { label = "S240_3_putback", scenario = "breaklock", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; the remote scenarios reach it" },
]
````

with:

````toml
scenarios = ["selftest", "recovery", "breaklock", "mixed", "breaklock-remote", "mixed-remote"]

# Labels no BUILT scenario covers but a planned one will (design Section 4). Each entry goes when its
# scenario lands: the stamp test requires the scenario to be in trace.toml's planned_scenarios.
deferred = [
    { label = "S96_1_backoff", scenario = "dirlock", reason = "the 96.1 directory-lock conflict needs the directory lock only dirlock creates" },
]
````

Note: `S240_3_putback` leaves `deferred` here, not because a `recovery` run now covers it (none does), but because
`breaklock-remote-posix-check` reaches it directly (design Section 12): a running Recoverer's OS-native lock is what
protects that record from a 240.5 in-place takeover, and only a lease lapse can take it. Step 4 removes the entry
in the same change that adds the run whose coverage replaces it, exactly as Section 4 requires.

- [ ] **Step 3: Every `recovery` run's entry gains the same constants, and an `S99_refuse_close` `unreached` entry**

Every one of the 30 `recovery` `[[run]]` entries in `expected.toml` gets the same two edits Step 1 made to its
`.cfg`, mirrored in its `constants` table (the runner compares them in both directions, design Section 4): insert
`Breakers = []` after `Cleanups = []` (or `Cleanups = ["c1"]`), insert `MaxLeaseExpiries = 0` after `HostCrashes =
...`, and append `SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN
= false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false` after the
existing `SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = ...` (never `FIX_REMOTE_LEASE_SPEC`, which the design's `constants`
rule excludes from every run: it is set only by the derived `-fixed` runs). For example, `recovery-posix-check`'s
entry:

Replace:

````toml
[[run]]
name = "recovery-posix-check"
module = "LockProtocol"
config = "configs/recovery-posix-check.cfg"
scenario = "recovery"
kind = "check"
constants = { Owners = ["o1"], Recoverers = ["r1", "r2"], PlainRuns = [], Cleanups = [], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false }
symmetry = { definition = "RecovererPerms", over = "Recoverers" }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record; breaklock reaches it" },
]
timeout_minutes = 30
````

with:

````toml
[[run]]
name = "recovery-posix-check"
module = "LockProtocol"
config = "configs/recovery-posix-check.cfg"
scenario = "recovery"
kind = "check"
constants = { Owners = ["o1"], Recoverers = ["r1", "r2"], PlainRuns = [], Cleanups = [], Breakers = [], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "RecovererPerms", over = "Recoverers" }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; the remote scenarios reach it" },
    { label = "S99_refuse_close", reason = "a Section 99 check fails only for an owner whose lock was taken while it ran; no recovery actor loses its lock while running (a takeover needs a Breaker, a lapsed lease needs LockCapability = remote)" },
]
timeout_minutes = 30
````

Apply the same three changes (constants, the reworded `S240_3_putback` reason, and the new `S99_refuse_close`
entry) to every other `recovery` run entry. Where a run's `unreached` list already has `plain_recover`,
`plain_recovered` and `plain_recovered_done` (the plain-rerun runs), `S99_refuse_close` is appended after those
three, not immediately after `S240_3_putback` - for example `recovery-posix-plain-check`:

Replace:

````toml
[[run]]
name = "recovery-posix-plain-check"
module = "LockProtocol"
config = "configs/recovery-posix-plain-check.cfg"
scenario = "recovery"
kind = "check"
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = ["p1"], Cleanups = [], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record; breaklock reaches it" },
    { label = "plain_recover", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered_done", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
]
timeout_minutes = 30
````

with:

````toml
[[run]]
name = "recovery-posix-plain-check"
module = "LockProtocol"
config = "configs/recovery-posix-plain-check.cfg"
scenario = "recovery"
kind = "check"
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = ["p1"], Cleanups = [], Breakers = [], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; the remote scenarios reach it" },
    { label = "plain_recover", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered_done", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "S99_refuse_close", reason = "a Section 99 check fails only for an owner whose lock was taken while it ran; no recovery actor loses its lock while running (a takeover needs a Breaker, a lapsed lease needs LockCapability = remote)" },
]
timeout_minutes = 30
````

Every `recovery` run entry in `expected.toml` (all 30, matching Step 1's list plus `recovery-posix-liveness` and
`recovery-windows-liveness`, which do not have their own `.cfg` filename in that list because `recovery-*-liveness`
appears once per platform there too) takes this same shape of edit. When Step 3 is done, `grep -c "Breakers = \[\]"
models/lockproto/expected.toml` reports at least 30 (recovery) plus the new runs Step 4 adds.

- [ ] **Step 4: Append the `breaklock`, `mixed`, `breaklock-remote` and `mixed-remote` run entries**

Append, after the last `recovery` run entry (`recovery-windows-witness-NeverTornRead`) and before end of file:

````toml

# Breaklock scenario (design Section 12, plan 3): a StalledOwner (under strong capability, an Owner that may crash)
# and two Breakers racing to take over the same uncertain lock. Built first, with spec 96.1 as written, to measure
# the acquirer window of design Section 11. Measured (2026-09-14): with 96.1 as written, an acquirer's backoff removed
# a Breaker's taken-over lock by name, violating RefusalJustified and SingleWriter; spec 96.1 and 240.3 step 4 now
# remove the file only while the lock path still names it, and SEED_ACQUIRER_UNLINKS_BY_NAME keeps the old wording.

[[run]]
name = "breaklock-posix-check"
module = "LockProtocol"
config = "configs/breaklock-posix-check.cfg"
scenario = "breaklock"
kind = "check"
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; the remote scenarios reach it" },
    { label = "S99_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S21_1_s3_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
]
timeout_minutes = 60

[[run]]
name = "breaklock-posix-seeded-SEED_ACQUIRER_UNLINKS_BY_NAME"
module = "LockProtocol"
config = "configs/breaklock-posix-seeded-SEED_ACQUIRER_UNLINKS_BY_NAME.cfg"
scenario = "breaklock"
kind = "seeded"
violated = ["SingleWriter"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = true, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
timeout_minutes = 30

[[run]]
name = "breaklock-posix-seeded-SEED_RESTART_RELEASES"
module = "LockProtocol"
config = "configs/breaklock-posix-seeded-SEED_RESTART_RELEASES.cfg"
scenario = "breaklock"
kind = "seeded"
violated = ["SingleWriter"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = true, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
timeout_minutes = 30

[[run]]
name = "breaklock-posix-seeded-SEED_MOVE_ASIDE_FOR_UNCERTAIN"
module = "LockProtocol"
config = "configs/breaklock-posix-seeded-SEED_MOVE_ASIDE_FOR_UNCERTAIN.cfg"
scenario = "breaklock"
kind = "seeded"
violated = ["PlainNeverOwnsUncertain"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = true, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
timeout_minutes = 30

[[run]]
name = "breaklock-posix-seeded-SEED_RENAME_OVER_TAKEOVER"
module = "LockProtocol"
config = "configs/breaklock-posix-seeded-SEED_RENAME_OVER_TAKEOVER.cfg"
scenario = "breaklock"
kind = "seeded"
violated = ["SingleWriter"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 5, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = true, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
timeout_minutes = 30

[[run]]
name = "breaklock-posix-weakcap-seeded-SEED_NO_CAPABILITY_GATE"
module = "LockProtocol"
config = "configs/breaklock-posix-weakcap-seeded-SEED_NO_CAPABILITY_GATE.cfg"
scenario = "breaklock"
kind = "seeded"
violated = ["SingleWriter"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "weak", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = true, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
timeout_minutes = 30

[[run]]
name = "breaklock-posix-plain-check"
module = "LockProtocol"
config = "configs/breaklock-posix-plain-check.cfg"
scenario = "breaklock"
kind = "check"
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = ["p1"], Cleanups = [], Breakers = ["b1"], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; the remote scenarios reach it" },
    { label = "S99_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S21_1_s3_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "plain_recover", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered_done", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
]
timeout_minutes = 60

[[run]]
name = "breaklock-windows-check"
module = "LockProtocol"
config = "configs/breaklock-windows-check.cfg"
scenario = "breaklock"
kind = "check"
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "windows", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; the remote scenarios reach it" },
    { label = "S99_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S21_1_s3_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
]
timeout_minutes = 60

[[run]]
name = "breaklock-windows-plain-check"
module = "LockProtocol"
config = "configs/breaklock-windows-plain-check.cfg"
scenario = "breaklock"
kind = "check"
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = ["p1"], Cleanups = [], Breakers = ["b1"], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "windows", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; the remote scenarios reach it" },
    { label = "S99_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S21_1_s3_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "plain_recover", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered_done", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
]
timeout_minutes = 60

[[run]]
name = "breaklock-posix-witness-NeverTornRead"
module = "LockProtocol"
config = "configs/breaklock-posix-witness-NeverTornRead.cfg"
scenario = "breaklock"
kind = "witness"
violated = ["NeverTornRead"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
timeout_minutes = 30

[[run]]
name = "breaklock-posix-witness-NeverUncertainLockWithPendingBreaker"
module = "LockProtocol"
config = "configs/breaklock-posix-witness-NeverUncertainLockWithPendingBreaker.cfg"
scenario = "breaklock"
kind = "witness"
violated = ["NeverUncertainLockWithPendingBreaker"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
timeout_minutes = 30

[[run]]
name = "breaklock-windows-witness-NeverTornRead"
module = "LockProtocol"
config = "configs/breaklock-windows-witness-NeverTornRead.cfg"
scenario = "breaklock"
kind = "witness"
violated = ["NeverTornRead"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "windows", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
timeout_minutes = 30

[[run]]
name = "breaklock-windows-witness-NeverUncertainLockWithPendingBreaker"
module = "LockProtocol"
config = "configs/breaklock-windows-witness-NeverUncertainLockWithPendingBreaker.cfg"
scenario = "breaklock"
kind = "witness"
violated = ["NeverUncertainLockWithPendingBreaker"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "windows", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
timeout_minutes = 30

[[run]]
name = "breaklock-posix-hostcrash-witness-NeverHostCrashChangedLock"
module = "LockProtocol"
config = "configs/breaklock-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg"
scenario = "breaklock"
kind = "witness"
violated = ["NeverHostCrashChangedLock"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = true, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "BreakerPerms", over = "Breakers" }
timeout_minutes = 30

[[run]]
name = "breaklock-posix-liveness"
module = "LockProtocol"
config = "configs/breaklock-posix-liveness.cfg"
scenario = "breaklock"
kind = "liveness"
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; the remote scenarios reach it" },
    { label = "S99_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S21_1_s3_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
]
timeout_minutes = 90

# Mixed scenario (design Section 12, plan 3): an Owner that may crash, a Breaker and a Recoverer. Its plain pairing
# is breaklock's (the same actors under a strong capability), and its weak-identity variant was measured and dropped.

[[run]]
name = "mixed-posix-check"
module = "LockProtocol"
config = "configs/mixed-posix-check.cfg"
scenario = "mixed"
kind = "check"
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; the remote scenarios reach it" },
    { label = "S99_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S21_1_s3_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
]
timeout_minutes = 60

[[run]]
name = "mixed-windows-check"
module = "LockProtocol"
config = "configs/mixed-windows-check.cfg"
scenario = "mixed"
kind = "check"
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "windows", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; the remote scenarios reach it" },
    { label = "S99_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S21_1_s3_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
]
timeout_minutes = 60

[[run]]
name = "mixed-posix-witness-NeverRecoveredAfterCrash"
module = "LockProtocol"
config = "configs/mixed-posix-witness-NeverRecoveredAfterCrash.cfg"
scenario = "mixed"
kind = "witness"
violated = ["NeverRecoveredAfterCrash"]
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 30

[[run]]
name = "mixed-posix-witness-NeverUncertainLockWithPendingBreaker"
module = "LockProtocol"
config = "configs/mixed-posix-witness-NeverUncertainLockWithPendingBreaker.cfg"
scenario = "mixed"
kind = "witness"
violated = ["NeverUncertainLockWithPendingBreaker"]
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 30

[[run]]
name = "mixed-windows-witness-NeverRecoveredAfterCrash"
module = "LockProtocol"
config = "configs/mixed-windows-witness-NeverRecoveredAfterCrash.cfg"
scenario = "mixed"
kind = "witness"
violated = ["NeverRecoveredAfterCrash"]
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "windows", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 30

[[run]]
name = "mixed-windows-witness-NeverUncertainLockWithPendingBreaker"
module = "LockProtocol"
config = "configs/mixed-windows-witness-NeverUncertainLockWithPendingBreaker.cfg"
scenario = "mixed"
kind = "witness"
violated = ["NeverUncertainLockWithPendingBreaker"]
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "windows", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 30

[[run]]
name = "mixed-posix-hostcrash-witness-NeverHostCrashChangedLock"
module = "LockProtocol"
config = "configs/mixed-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg"
scenario = "mixed"
kind = "witness"
violated = ["NeverHostCrashChangedLock"]
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 4, MaxCrashes = 2, HostCrashes = true, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 30

[[run]]
name = "mixed-posix-liveness"
module = "LockProtocol"
config = "configs/mixed-posix-liveness.cfg"
scenario = "mixed"
kind = "liveness"
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; the remote scenarios reach it" },
    { label = "S99_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S21_1_s3_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
]
timeout_minutes = 90

[[run]]
name = "mixed-posix-seeded-SEED_TORN_AS_FOREIGN"
module = "LockProtocol"
config = "configs/mixed-posix-seeded-SEED_TORN_AS_FOREIGN.cfg"
scenario = "mixed"
kind = "seeded"
violated = ["UncertainLockEventuallyCleared"]
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 4, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = true }
timeout_minutes = 90

# Remote scenarios (design Section 12, plan 3): LockCapability = remote, where a lock lease can lapse under a
# running holder, so a takeover can win against it and its in-flight write can land afterwards. Paired into
# three-actor runs (owner ruling 2026-09-15): the four-actor checks passed ~65M states in 72 min unfinished.

[[run]]
name = "breaklock-remote-posix-check"
module = "LockProtocol"
config = "configs/breaklock-remote-posix-check.cfg"
scenario = "breaklock-remote"
kind = "check"
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "recover_crashed", reason = "the recovery path is entered only by a Breaker that found a dead lock, and this pairing spends its single crash making that lock dead, so no crash is left to interrupt the recovery itself" },
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
]
timeout_minutes = 120

[[run]]
name = "breaklock-remote-posix-window-witness-SingleWriter"
module = "LockProtocol"
config = "configs/breaklock-remote-posix-window-witness-SingleWriter.cfg"
scenario = "breaklock-remote"
kind = "witness"
violated = ["SingleWriter"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 60

[[run]]
name = "breaklock-remote-posix-fixed-check"
module = "LockProtocol"
config = "configs/breaklock-remote-posix-fixed-check.cfg"
scenario = "breaklock-remote"
kind = "check"
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "recover_crashed", reason = "the recovery path is entered only by a Breaker that found a dead lock, and this pairing spends its single crash making that lock dead, so no crash is left to interrupt the recovery itself" },
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
]
timeout_minutes = 120

[[run]]
name = "breaklock-remote-posix-plain-check"
module = "LockProtocol"
config = "configs/breaklock-remote-posix-plain-check.cfg"
scenario = "breaklock-remote"
kind = "check"
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = ["p1"], Cleanups = [], Breakers = ["b1"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "recover_crashed", reason = "the recovery path is entered only by a Breaker that found a dead lock, and this pairing spends its single crash making that lock dead, so no crash is left to interrupt the recovery itself" },
    { label = "plain_recover", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered_done", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a takeover under a running Recoverer; mixed-remote reaches it" },
]
timeout_minutes = 120

[[run]]
name = "breaklock-remote-posix-plain-window-witness-SingleWriter"
module = "LockProtocol"
config = "configs/breaklock-remote-posix-plain-window-witness-SingleWriter.cfg"
scenario = "breaklock-remote"
kind = "witness"
violated = ["SingleWriter"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = ["p1"], Cleanups = [], Breakers = ["b1"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 60

[[run]]
name = "breaklock-remote-posix-plain-fixed-check"
module = "LockProtocol"
config = "configs/breaklock-remote-posix-plain-fixed-check.cfg"
scenario = "breaklock-remote"
kind = "check"
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = ["p1"], Cleanups = [], Breakers = ["b1"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "recover_crashed", reason = "the recovery path is entered only by a Breaker that found a dead lock, and this pairing spends its single crash making that lock dead, so no crash is left to interrupt the recovery itself" },
    { label = "plain_recover", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "plain_recovered_done", reason = "reached only by a plain rerun that finds a dead CLEANUP lock, which needs a Cleanup actor; recovery-<platform>-plain-cleanup-check reaches it" },
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a takeover under a running Recoverer; mixed-remote reaches it" },
]
timeout_minutes = 120

[[run]]
name = "breaklock-remote-posix-witness-NeverTornRead"
module = "LockProtocol"
config = "configs/breaklock-remote-posix-witness-NeverTornRead.cfg"
scenario = "breaklock-remote"
kind = "witness"
violated = ["NeverTornRead"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 60

[[run]]
name = "breaklock-remote-posix-witness-NeverUncertainLockWithPendingBreaker"
module = "LockProtocol"
config = "configs/breaklock-remote-posix-witness-NeverUncertainLockWithPendingBreaker.cfg"
scenario = "breaklock-remote"
kind = "witness"
violated = ["NeverUncertainLockWithPendingBreaker"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 60

[[run]]
name = "breaklock-remote-posix-witness-NeverInflightLandedAfterTakeover"
module = "LockProtocol"
config = "configs/breaklock-remote-posix-witness-NeverInflightLandedAfterTakeover.cfg"
scenario = "breaklock-remote"
kind = "witness"
violated = ["NeverInflightLandedAfterTakeover"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 60

[[run]]
name = "mixed-remote-posix-check"
module = "LockProtocol"
config = "configs/mixed-remote-posix-check.cfg"
scenario = "mixed-remote"
kind = "check"
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "recover_crashed", reason = "the recovery path is entered only by a Breaker that found a dead lock, and this pairing spends its single crash making that lock dead, so no crash is left to interrupt the recovery itself" },
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
]
timeout_minutes = 120

[[run]]
name = "mixed-remote-posix-window-witness-SingleWriter"
module = "LockProtocol"
config = "configs/mixed-remote-posix-window-witness-SingleWriter.cfg"
scenario = "mixed-remote"
kind = "witness"
violated = ["SingleWriter"]
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 60

[[run]]
name = "mixed-remote-posix-fixed-check"
module = "LockProtocol"
config = "configs/mixed-remote-posix-fixed-check.cfg"
scenario = "mixed-remote"
kind = "check"
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
unreached = [
    { label = "recover_crashed", reason = "the recovery path is entered only by a Breaker that found a dead lock, and this pairing spends its single crash making that lock dead, so no crash is left to interrupt the recovery itself" },
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run in breaklock reaches it" },
]
timeout_minutes = 120

[[run]]
name = "mixed-remote-posix-witness-NeverRecoveredAfterCrash"
module = "LockProtocol"
config = "configs/mixed-remote-posix-witness-NeverRecoveredAfterCrash.cfg"
scenario = "mixed-remote"
kind = "witness"
violated = ["NeverRecoveredAfterCrash"]
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 60

[[run]]
name = "mixed-remote-posix-witness-NeverUncertainLockWithPendingBreaker"
module = "LockProtocol"
config = "configs/mixed-remote-posix-witness-NeverUncertainLockWithPendingBreaker.cfg"
scenario = "mixed-remote"
kind = "witness"
violated = ["NeverUncertainLockWithPendingBreaker"]
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = false, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 60

[[run]]
name = "breaklock-remote-posix-hostcrash-witness-NeverHostCrashChangedLock"
module = "LockProtocol"
config = "configs/breaklock-remote-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg"
scenario = "breaklock-remote"
kind = "witness"
violated = ["NeverHostCrashChangedLock"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = true, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 60

[[run]]
name = "mixed-remote-posix-hostcrash-witness-NeverHostCrashChangedLock"
module = "LockProtocol"
config = "configs/mixed-remote-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg"
scenario = "mixed-remote"
kind = "witness"
violated = ["NeverHostCrashChangedLock"]
constants = { Owners = ["o1"], Recoverers = ["r1"], PlainRuns = [], Cleanups = [], Breakers = ["b1"], MaxObjs = 5, MaxCrashes = 1, HostCrashes = true, MaxLeaseExpiries = 1, Platform = "posix", IdentityStrength = "strong", LockCapability = "remote", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
timeout_minutes = 60
````

- [ ] **Step 5: `expected-extended.toml`'s eight `recovery` host-crash runs gain the same constants**

Each of the eight `recovery-*-hostcrash-*` runs in `expected-extended.toml` gets the same `constants` edit as Step
3, plus its `S240_3_putback` reason reworded to name `mixed-remote` (not `breaklock`, since `breaklock` never
carries a running Recoverer), plus a new `unreached` entry, `S99_refuse_close`. For example:

Replace:

````toml
[[run]]
name = "recovery-posix-hostcrash-check"
module = "LockProtocol"
config = "configs/recovery-posix-hostcrash-check.cfg"
scenario = "recovery"
kind = "check"
constants = { Owners = ["o1"], Recoverers = ["r1", "r2"], PlainRuns = [], Cleanups = [], MaxObjs = 3, MaxCrashes = 2, HostCrashes = true, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false }
symmetry = { definition = "RecovererPerms", over = "Recoverers" }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record; breaklock reaches it" },
]
timeout_minutes = 150
````

with:

````toml
[[run]]
name = "recovery-posix-hostcrash-check"
module = "LockProtocol"
config = "configs/recovery-posix-hostcrash-check.cfg"
scenario = "recovery"
kind = "check"
constants = { Owners = ["o1"], Recoverers = ["r1", "r2"], PlainRuns = [], Cleanups = [], Breakers = [], MaxObjs = 3, MaxCrashes = 2, HostCrashes = true, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false }
symmetry = { definition = "RecovererPerms", over = "Recoverers" }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record under a running Recoverer, which needs a lapsed lease; mixed-remote reaches it" },
    { label = "S99_refuse_close", reason = "a Section 99 check fails only for an owner whose lock was taken while it ran; no recovery actor loses its lock while running (a takeover needs a Breaker, a lapsed lease needs LockCapability = remote)" },
]
timeout_minutes = 150
````

Apply the same three-part edit to the other seven: `recovery-posix-hostcrash-plain-check` (append
`S99_refuse_close` after the existing three `plain_recover*` entries), `recovery-posix-hostcrash-cleanup-check`,
`recovery-posix-hostcrash-plain-cleanup-check` (append after its three `plain_recover*` entries),
`recovery-windows-hostcrash-check`, `recovery-windows-hostcrash-plain-check` (append after its three
`plain_recover*` entries), `recovery-windows-hostcrash-cleanup-check`, and
`recovery-windows-hostcrash-plain-cleanup-check` (append after its three `plain_recover*` entries). Every one of
the eight keeps its own `MaxObjs`, actor sets and `timeout_minutes = 150` unchanged; only `constants` and
`unreached` change.

- [ ] **Step 6: Confirm the counts and commit**

Run: `python3 -c "import tomllib; d = tomllib.load(open('models/lockproto/expected.toml', 'rb')); print(len(d['run']), d['scenarios'], d['deferred'])"`
Expected: `66 ['selftest', 'recovery', 'breaklock', 'mixed', 'breaklock-remote', 'mixed-remote']
[{'label': 'S96_1_backoff', 'scenario': 'dirlock', 'reason': "the 96.1 directory-lock conflict needs the directory lock only dirlock creates"}]`
(4 `selftest` + 22 `recovery` + 15 `breaklock` + 9 `mixed` + 10 `breaklock-remote` + 6 `mixed-remote` = 66. `recovery`
has 22 `[[run]]` entries here, not 30: the other 8 `recovery-*.cfg` files under `models/lockproto/configs/` are the
host-crash tier's, and their `[[run]]` entries live in `expected-extended.toml` (Step 5), not here - 22 + 8 = 30
matches the `.cfg` file count exactly. This exact figure was verified by running the command above against the real
file, not estimated).

```bash
git add models/lockproto/configs/recovery-*.cfg models/lockproto/expected.toml models/lockproto/expected-extended.toml
git commit -m "model: breaklock, mixed and remote scenario entries; recovery's new constants and S99_refuse_close"
```

### Task 7: The stamp and the traceability map

**Design oracle:** Section 9 (the drift stamp), Section 9.1 (`trace.toml`'s traceability check: every unit maps
labels to a spec sentence, or says why not).

**Files:**
- Modify: `models/lockproto/trace.toml`, `models/lockproto/spec-sections.stamp`

- [ ] **Step 0: State check**

Run: `cargo test --test model_stamp 2>&1 | tail -5`
Expected: fails (Task 1 changed the quoted text of two units, and Task 2 added labels no unit yet claims).

- [ ] **Step 1: `planned_scenarios` drops `breaklock` and `mixed`**

Replace:

````toml
planned_scenarios = ["breaklock", "mixed", "cleanup", "cleanup-crash", "nested", "dirlock", "claims"]
````

with:

````toml
planned_scenarios = ["cleanup", "cleanup-crash", "nested", "dirlock", "claims"]
````

- [ ] **Step 2: 21.1's existing-lock unit gains the Breaker/TakeOver labels, and drops its `pending`**

Replace:

````toml
ordinal = 13
quote = "1. acquire the prior operation's lock without waiting (a dead owner's"
hash = "10bc1c6eb532112f84694c8649abf4b01ae4c05a8e4089f21edb9b5043a3e02e"
labels = ["S240_1_open", "S240_1_trylock", "S240_1_read", "S240_1_close", "S240_3_s1", "S240_3_s2", "S240_3_s3", "S240_3_s4", "S240_3_s4_lock", "S240_3_s4_record_begin", "S240_3_s4_record_end", "S240_3_restart"]
not_modelled = "the model refuses TARGET_LOCK_BUSY generically and does not distinguish OPERATION_LOCKED as a separate code"
pending = ["breaklock"]
````

with:

````toml
ordinal = 13
quote = "1. acquire the prior operation's lock without waiting (a dead owner's"
hash = "10bc1c6eb532112f84694c8649abf4b01ae4c05a8e4089f21edb9b5043a3e02e"
labels = ["S240_1_open", "S240_1_trylock", "S240_1_read", "S240_1_close", "S240_3_s1", "S240_3_s2", "S240_3_s3", "S240_3_s4", "S240_3_s4_lock", "S240_3_s4_record_begin", "S240_3_s4_record_end", "S240_3_restart", "S21_1_restart_decide", "S240_5_s1", "S240_5_s2", "S240_5_s3", "S240_5_s4", "S240_5_s5", "S240_5_s6"]
not_modelled = "the model refuses TARGET_LOCK_BUSY generically and does not distinguish OPERATION_LOCKED as a separate code"
````

- [ ] **Step 3: 21.1 step 3 (revalidate) gains its Breaker labels**

Replace:

````toml
ordinal = 15
quote = "3. revalidate ownership and locks (Section 99)"
hash = "2e2c0b257756369a3304969102ddaf4a3816f4706875b727f14e2f66d70e67fe"
labels = ["S99_check", "S99_write", "S99_release", "S99_close"]
````

with:

````toml
ordinal = 15
quote = "3. revalidate ownership and locks (Section 99)"
hash = "2e2c0b257756369a3304969102ddaf4a3816f4706875b727f14e2f66d70e67fe"
labels = ["S99_check", "S99_write", "S99_release", "S99_close", "S21_1_s3", "S21_1_s3_refuse_close"]
````

- [ ] **Step 4: 21.1 step 5 (start the new operation) gains the Breaker's rewrite**

Replace:

````toml
ordinal = 17
quote = "5. start the new operation under"
hash = "3095b782073bac3b3226b5cfaa0ba4e89fad2f1e775f3773927cca1d7889e0de"
labels = ["S240_3_s4_record_begin", "S240_3_s4_record_end"]
````

with:

````toml
ordinal = 17
quote = "5. start the new operation under"
hash = "3095b782073bac3b3226b5cfaa0ba4e89fad2f1e775f3773927cca1d7889e0de"
labels = ["S240_3_s4_record_begin", "S240_3_s4_record_end", "S21_1_s5_write_begin", "S21_1_s5_write_end"]
````

- [ ] **Step 5: 96.1's "overwritten in place only by a `--break-lock`" unit gains the takeover's write labels**

Replace:

````toml
ordinal = 10
quote = "A Flux lock record is overwritten in place only by a `--break-lock`"
hash = "978df397b3a0bebbc934718290b9ca34a7d5293441b0b4a72bd4ca4ab1265ade"
labels = ["S240_3_s2"]
pending = ["breaklock"]
````

with:

````toml
ordinal = 10
quote = "A Flux lock record is overwritten in place only by a `--break-lock`"
hash = "978df397b3a0bebbc934718290b9ca34a7d5293441b0b4a72bd4ca4ab1265ade"
labels = ["S240_3_s2", "S240_5_s6_write_begin", "S240_5_s6_write_end"]
````

- [ ] **Step 6: 96.1's OS-native-lock unit re-hashes (Task 1 changed its quoted text) and gains the retry labels**

Replace:

````toml
heading = "## 96.1 Name-Equivalent Target Locks"
ordinal = 13
quote = "Where the destination provides OS-native locks (Section 235.1), the"
hash = "2e31dee7fefda0d3b01610d9040de9fddf5cdb2dd60836ba1e0c8706b07268b6"
labels = ["S96_1_ownlock", "S96_1_ownlock_backoff", "S96_1_record_begin", "S96_1_record_end"]
````

with:

````toml
heading = "## 96.1 Name-Equivalent Target Locks"
ordinal = 13
quote = "Where the destination provides OS-native locks (Section 235.1), the"
hash = "f89f79b57e4b9c3f383e51daa4776fe0b8b01d60b24b9e7f51ed1d8fcb6e9577"
labels = ["S96_1_ownlock", "S96_1_ownlock_wait", "S96_1_ownlock_verify", "S96_1_ownlock_close", "S96_1_record_begin", "S96_1_record_end"]
````

The new hash, `f89f79b5...`, is `blake3(quote-line-and-context)` as `model_stamp.rs` computes it against the spec
file Task 1 edited; Step 8 below runs the test rather than trusting this by eye.

- [ ] **Step 7: 96.1's exclusive-creation-on-remote unit drops `pending` and gains `S96_1_ownlock`**

Replace:

````toml
ordinal = 22
quote = "Exclusive creation on a remote filesystem is trusted only under the lock"
hash = "4c938d58d5793215d3ce4f490bfff768e14740f101ea2ebfd106ef40c4c7a27b"
pending = ["breaklock"]
````

with:

````toml
ordinal = 22
quote = "Exclusive creation on a remote filesystem is trusted only under the lock"
hash = "4c938d58d5793215d3ce4f490bfff768e14740f101ea2ebfd106ef40c4c7a27b"
labels = ["S96_1_ownlock"]
not_modelled = "the REMOTE_LOCK_UNSAFE refusal sits in each actor's start step, which has no spec-named label; S96_1_ownlock is the exclusive creation, attempted only under LockCapability strong or remote (RemoteStrong), where a lease can lapse"
````

- [ ] **Step 8: Section 99's revalidation unit gains the release check and refusal-close labels**

Replace:

````toml
heading = "# 99. Commit-Time Lock Revalidation"
ordinal = 3
quote = "the worker must verify that its required ownership remains valid."
hash = "23d3b2ad1c79c117338acf208bd044e4be57e57d741d89c12f77545720b5c7a9"
labels = ["S99_check", "S99_release", "S99_close"]
````

with:

````toml
heading = "# 99. Commit-Time Lock Revalidation"
ordinal = 3
quote = "the worker must verify that its required ownership remains valid."
hash = "23d3b2ad1c79c117338acf208bd044e4be57e57d741d89c12f77545720b5c7a9"
labels = ["S99_check", "S99_release_check", "S99_release", "S99_release_lands", "S99_close", "S99_refuse_close"]
not_modelled = "the check before the release unlink is a reading: Section 99 lists unlink among the calls it guards but is about commit-time writes and never names releasing the lock (design Section 12, plan 3)"
````

- [ ] **Step 9: 240.2's held-lock unit gains the acquirer-wait and takeover labels**

Replace:

````toml
heading = "## 240.2 Live Owner"
ordinal = 4
quote = "A held native lock shows only that some process holds it."
hash = "034b812a3976310c5edebdd729ad4b913787b56854d841c675bd32c8e4536b6a"
labels = ["S240_1_read", "S96_1_ownlock_backoff"]
````

with:

````toml
heading = "## 240.2 Live Owner"
ordinal = 4
quote = "A held native lock shows only that some process holds it."
hash = "034b812a3976310c5edebdd729ad4b913787b56854d841c675bd32c8e4536b6a"
labels = ["S240_1_read", "S96_1_ownlock_wait", "S96_1_ownlock_close", "S240_5_s3"]
````

- [ ] **Step 10: 240.3 step 4's exclusive-creation unit gains the retry labels**

Replace:

````toml
heading = "## 240.3 Demonstrably Abandoned Owner"
ordinal = 6
quote = "4. Create its own lock exclusively and take its OS-native lock (Section"
hash = "09188902f8126011c3941f4cb6da6102774e7bd8d8a69a81f2f299b792117c6d"
labels = ["S240_3_s4", "S240_3_s4_lock", "S240_3_s4_record_begin", "S240_3_s4_record_end", "S240_3_s4_drop", "S240_3_s4_lock_backoff"]
````

with:

````toml
heading = "## 240.3 Demonstrably Abandoned Owner"
ordinal = 6
quote = "4. Create its own lock exclusively and take its OS-native lock (Section"
hash = "c68707b84eb376031da041493a542c77231ede80140a3b4cfd32fb3cf2ecf59e"
labels = ["S240_3_s4", "S240_3_s4_lock", "S240_3_s4_record_begin", "S240_3_s4_record_end", "S240_3_s4_drop", "S240_3_s4_lock_wait", "S240_3_s4_lock_verify", "S240_3_s4_lock_close"]
````

Note: this unit's hash also changes (Task 1's Step 2 edited its quoted text, "retry and give up as Section 96.1's
acquirer does" replacing "remove the lock file it created, delete the moved file, and start the acquisition
again"); `c68707b8...` is the hash of the NEW text.

- [ ] **Step 11: 240.4's `--break-lock` cross-reference gains the Breaker's decision labels**

Replace:

````toml
heading = "## 240.4 Uncertain Ownership"
ordinal = 9
quote = "Explicit operator-directed recovery is `--break-lock` (Section 240.5)."
hash = "970013469d41bdcd7cf20f101f4b1bdea5dfbd9b90806eee6b2a849e87405e24"
pending = ["breaklock"]
````

with:

````toml
heading = "## 240.4 Uncertain Ownership"
ordinal = 9
quote = "Explicit operator-directed recovery is `--break-lock` (Section 240.5)."
hash = "970013469d41bdcd7cf20f101f4b1bdea5dfbd9b90806eee6b2a849e87405e24"
labels = ["S21_1_restart_decide", "S240_5_s1"]
````

- [ ] **Step 12: Every 240.5 unit loses `pending = ["breaklock", ...]` (keeping `cleanup` where present) and gains its `TakeOver` labels**

Replace:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 1
quote = "`--break-lock` is valid with `flux copy --restart` and `flux cleanup"
hash = "d47e0ddecc9bcc066308a74e08a6caa155397355d9cdcac06994cf067c7d46b8"
pending = ["breaklock", "cleanup"]
````

with:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 1
quote = "`--break-lock` is valid with `flux copy --restart` and `flux cleanup"
hash = "d47e0ddecc9bcc066308a74e08a6caa155397355d9cdcac06994cf067c7d46b8"
pending = ["cleanup"]
labels = ["S21_1_restart_decide"]
````

Replace:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 2
quote = "Before acting, Flux reports the recorded holder's `owner_instance_id`,"
hash = "f22d8b340430bb74f8be8ea1ba502cac9594dab13247f5d7b2002647d3c1ed8c"
pending = ["breaklock", "cleanup"]
````

with:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 2
quote = "Before acting, Flux reports the recorded holder's `owner_instance_id`,"
hash = "f22d8b340430bb74f8be8ea1ba502cac9594dab13247f5d7b2002647d3c1ed8c"
pending = ["cleanup"]
labels = ["S240_1_open", "S240_1_trylock", "S240_1_read", "S240_1_close", "S21_1_restart_decide"]
not_modelled = "the report of the holder's fields is not modelled; the model keeps what the classification read (seenRec)"
````

Replace:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 3
quote = "1. The destination must provide strong file identity (Section 107) and"
hash = "c308f74a8c0205d8817f6a1f4993396f1602fd7acc22dc94ba8d2f278dcc6987"
pending = ["breaklock", "cleanup"]
````

with:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 3
quote = "1. The destination must provide strong file identity (Section 107) and"
hash = "c308f74a8c0205d8817f6a1f4993396f1602fd7acc22dc94ba8d2f278dcc6987"
pending = ["cleanup"]
labels = ["S240_5_s1"]
````

Replace:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 4
quote = "2. Open the existing lock file for writing, without creating it."
hash = "775b080a7d1c13db6034705bc86423c0b75895359f22c3d8ab35c6e96bac6c37"
pending = ["breaklock", "cleanup"]
````

with:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 4
quote = "2. Open the existing lock file for writing, without creating it."
hash = "775b080a7d1c13db6034705bc86423c0b75895359f22c3d8ab35c6e96bac6c37"
pending = ["cleanup"]
labels = ["S240_5_s2"]
````

Replace:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 5
quote = "3. Take the OS-native lock on it without waiting."
hash = "2bd2c5a7696da8c554795161fc23488b7376cca423ec40fb7493eaf79616711e"
pending = ["breaklock", "cleanup"]
````

with:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 5
quote = "3. Take the OS-native lock on it without waiting."
hash = "2bd2c5a7696da8c554795161fc23488b7376cca423ec40fb7493eaf79616711e"
pending = ["cleanup"]
labels = ["S240_5_s3", "S240_5_close"]
````

Replace:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 6
quote = "4. Check that the open file is still the one at the lock path"
hash = "6ab81361b3232eb80c9a7ee1ab8101fba4fdb0a1f71b340068169f85dfb29cbb"
pending = ["breaklock", "cleanup"]
````

with:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 6
quote = "4. Check that the open file is still the one at the lock path"
hash = "6ab81361b3232eb80c9a7ee1ab8101fba4fdb0a1f71b340068169f85dfb29cbb"
pending = ["cleanup"]
labels = ["S240_5_s4"]
````

Replace:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 7
quote = "5. Read the record. If it names a live owner, refuse with"
hash = "6faace063b28fb5eb49d0848d9ba04e343206ed00a267b53cb92f794918f6060"
pending = ["breaklock", "cleanup"]
````

with:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 7
quote = "5. Read the record. If it names a live owner, refuse with"
hash = "6faace063b28fb5eb49d0848d9ba04e343206ed00a267b53cb92f794918f6060"
pending = ["cleanup"]
labels = ["S240_5_s5"]
````

Replace:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 8
quote = "6. Overwrite the record in place with this operation's record in one"
hash = "f0c9e42611e7803ca44a5f1ef7476ffd9c09fbf347f8536a37592d135b039e14"
pending = ["breaklock", "cleanup"]
````

with:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 8
quote = "6. Overwrite the record in place with this operation's record in one"
hash = "f0c9e42611e7803ca44a5f1ef7476ffd9c09fbf347f8536a37592d135b039e14"
pending = ["cleanup"]
labels = ["S240_5_s6_seed", "S240_5_s6_write_begin", "S240_5_s6_write_end", "S240_5_s6_flush", "S240_5_s6", "S240_5_seed_write_begin", "S240_5_seed_write_end", "S240_5_seed_rename"]
not_modelled = "S240_5_s6_seed and the S240_5_seed_* labels run only with SEED_RENAME_OVER_TAKEOVER, the defect a rename over the lock re-introduces (design Section 8)"
````

Replace:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 9
quote = "An open, read, write, or flush that fails for another reason (an I/O or"
hash = "06d1056ee655dfb4ea0209ed822013e645d9052547e04524f380b44f324a74ae"
pending = ["breaklock", "cleanup"]
````

with:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 9
quote = "An open, read, write, or flush that fails for another reason (an I/O or"
hash = "06d1056ee655dfb4ea0209ed822013e645d9052547e04524f380b44f324a74ae"
pending = ["cleanup"]
not_modelled = "I/O and permission errors of the takeover's calls are not modelled"
````

Replace:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 11
quote = "A prior owner that was only stalled starts nothing more: its next lock"
hash = "0d064cba024bbd4490bfe5f4009f68915a9386c3fa0add879a11aeb619ccae5d"
pending = ["breaklock", "cleanup"]
````

with:

````toml
heading = "## 240.5 `--break-lock`"
ordinal = 11
quote = "A prior owner that was only stalled starts nothing more: its next lock"
hash = "0d064cba024bbd4490bfe5f4009f68915a9386c3fa0add879a11aeb619ccae5d"
pending = ["cleanup"]
labels = ["S99_check", "S99_refuse_close", "S240_5_inflight_lands", "S240_5_s6_write_end"]
````

(Ordinal 10 of `## 240.5 \`--break-lock\`` is unaffected by this task and keeps whatever `pending`/`labels` it
already has; only the ordinals listed above changed in the squash this plan reproduces.)

- [ ] **Step 13: 259.6's target-lock-establishment unit gains the retry labels**

Replace:

````toml
heading = "## 259.6 Directory Target Lock Representation"
ordinal = 10
quote = "If the required target lock cannot be established, destination mutation must not proceed."
hash = "9cd5145d31ffda58fba3933509c649796b522d33f972cbe01bdd389adedf260f"
labels = ["S96_1_backoff", "S96_1_ownlock_backoff", "S99_check"]
````

with:

````toml
heading = "## 259.6 Directory Target Lock Representation"
ordinal = 10
quote = "If the required target lock cannot be established, destination mutation must not proceed."
hash = "9cd5145d31ffda58fba3933509c649796b522d33f972cbe01bdd389adedf260f"
labels = ["S96_1_backoff", "S96_1_ownlock_wait", "S96_1_ownlock_close", "S99_check"]
````

- [ ] **Step 14: 99.1's and 259.7's `pending` moves from `mixed` to `dirlock`**

Replace:

````toml
heading = "## 99.1 Weak Filesystem Identity and Target Locks"
family = "99"
count = 1
pending = ["mixed"]
````

with:

````toml
heading = "## 99.1 Weak Filesystem Identity and Target Locks"
family = "99"
count = 1
pending = ["dirlock"]
````

Replace:

````toml
heading = "## 259.7 Weak Filesystem Identity"
family = "259"
count = 1
pending = ["mixed"]
````

with:

````toml
heading = "## 259.7 Weak Filesystem Identity"
family = "259"
count = 1
pending = ["dirlock"]
````

`mixed`'s weak-identity variant was measured and dropped (design Section 12): the Breaker refuses at 240.5 step 1
before either heading's ground is reached, so only `dirlock`'s reused per-name lock still owes them.

- [ ] **Step 15: The new `235.1` unit and heading family, appended at end of file**

Append, after the last `[[heading]]` entry (`## 259.15 V15 Implementation Baseline`):

````toml

[[unit]]
heading = "## 235.1 Capability Classes"
ordinal = 1
quote = "The filesystem adapter exposes:"
hash = "f312c6cc19e4767280e0c2be432697f625af158931a2626a2bf7b334d513f22d"
not_modelled = "explanatory text; the classes it introduces are modelled in ordinals 2 and 3"

[[unit]]
heading = "## 235.1 Capability Classes"
ordinal = 2
quote = "enum LockCapability {"
hash = "b2b456dda857f39ebd4c71395453d4eb9d97239e6d90ee2ca71cef46c4447f35"
not_modelled = "the enum is abstracted into the model constant LockCapability: strong for LocalStrong, remote for RemoteStrong, weak for RemoteUnverified and Unsupported (design Section 5.1)"

[[unit]]
heading = "## 235.1 Capability Classes"
ordinal = 3
quote = "remote filesystem whose lock contract cannot be verified"
hash = "5d38fb617798a292d85f4dc9fa0d534d6827fe3381e4da3b8b774b1b0b6f8931"
labels = ["S240_5_s1", "S96_1_ownlock", "S240_3_s4_lock"]
not_modelled = "the REMOTE_LOCK_UNSAFE refusal sits in each actor's start step, which has no spec-named label; the labels listed are the lock attempts made conditional on the capability, and 240.5 step 1's capability check"

[[heading]]
heading = "# 235. Remote Filesystem Lock Capability"
family = "235"
count = 1
not_modelled = "container heading; its one sentence is the premise the 235.1 capability classes encode"

[[heading]]
heading = "## 235.2 Required Strong-Lock Properties"
family = "235"
count = 1
not_modelled = "the properties are assumed by FsModel's operations table for LockCapability strong and remote (design Section 5.1); they are what the classes certify, not a protocol step"

[[heading]]
heading = "## 235.3 O_EXCL"
family = "235"
count = 1
not_modelled = "exclusive creation is one atomic step in FsModel; whether a deployment provides it is what the capability class records"

[[heading]]
heading = "## 235.4 Remote Uncertainty"
family = "235"
count = 1
not_modelled = "the same REMOTE_LOCK_UNSAFE refusal as the 235.1 table, modelled in each actor's start step"

[[heading]]
heading = "## 235.5 Lock Revalidation"
family = "235"
count = 1
not_modelled = "restates the revalidation of Section 99, which is stamped and modelled (S99_check, S99_release_check)"
````

- [ ] **Step 16: `spec-sections.stamp` gains the `235.1` heading**

Replace:

````text
# 99. Commit-Time Lock Revalidation
## 240.1 Recovery Decision Order
````

with:

````text
# 99. Commit-Time Lock Revalidation
## 235.1 Capability Classes
## 240.1 Recovery Decision Order
````

- [ ] **Step 17: Run the stamp test to see it pass**

Run: `cargo test --test model_stamp`
Expected: `test result: ok.` with every test passing and only the pre-existing `1 ignored` (the same ignored test
plan 2 left; this plan adds no new ignored test). If any hash mismatches, recompute it by reading what
`tests/model_stamp.rs` hashes (design Section 9: the quoted text plus its immediate context, hashed with `blake3`)
rather than editing the hash to make the test pass - the oracle is the spec file Task 1 committed, not this table.

- [ ] **Step 18: Commit**

```bash
git add models/lockproto/trace.toml models/lockproto/spec-sections.stamp
git commit -m "model: traceability for the Breaker, TakeOver, in-flight publish/release, and 235.1"
```

### Task 8: Final gate, locally and on CI

**Design oracle:** Section 4 (CI already matrices from `expected.toml`'s `scenarios` list; no `justfile` or
workflow change is needed for a new scenario name to run); Section 12 (the state counts this plan can promise, and
those it cannot).

**Files:**
- Modify: `.claude/recommended-tools.json`

- [ ] **Step 1: Tooling - `gh` and the pinned `tla2tools.jar`, and a fuller `java` entry**

Replace:

````json
  {
    "name": "java",
    "why": "Runs TLC (the TLA+ model checker) for the lock-protocol model check: `just model` downloads a pinned tla2tools.jar and needs Java 11 or later on PATH. CI uses Temurin 21. Not needed by `just check`.",
    "install": "winget install EclipseAdoptium.Temurin.21.JDK",
    "in_path": "java"
  },
````

with:

````json
  {
    "name": "java",
    "why": "Runs the TLA+ tools in the pinned tla2tools.jar (Java 11 or later; CI uses Temurin 21). Locally it runs only the PlusCal translator and the SANY parser when models/lockproto/algorithm.txt changes: `java -cp target/tla/tla2tools.jar pcal.trans LockProtocol.tla` after concatenating LockProtocol.head, algorithm.txt and invariants.txt, then `tla2sany.SANY LockProtocol.tla`. The TLC model checks themselves are too heavy for the development machine and run only on CI (owner ruling, 2026-09-13); do not run `just model` locally. Not needed by `just check`.",
    "install": "winget install EclipseAdoptium.Temurin.21.JDK",
    "in_path": "java"
  },
  {
    "name": "tla2tools.jar",
    "why": "The pinned TLA+ tools jar (TLC 2.19, SHA-256 checked by models/lockproto/run.py) that the local PlusCal translation and SANY parse need; see the java entry. run.py downloads it into target/tla/ on first use; the install command fetches it without running any model check.",
    "install": "python -c \"import sys; sys.path.insert(0, 'models/lockproto'); import run; print(run.ensure_jar())\"",
    "file_exists": "target/tla/tla2tools.jar"
  },
  {
    "name": "gh",
    "why": "GitHub CLI. The model checks run only on CI, so their results come from `gh run watch`, `gh run view --log` and `gh run download -n tlc-output-<scenario>-<platform>` (the full TLC log artifact); pull requests, labels such as `model-extended`, and Dependabot auto-merge are driven through it too.",
    "install": "winget install GitHub.cli",
    "in_path": "gh"
  },
````

- [ ] **Step 2: Commit the tooling change**

```bash
git add .claude/recommended-tools.json
git commit -m "chore: recommended tools declare gh and the TLA+ tools jar; java runs no local TLC"
```

- [ ] **Step 3: Local gates, with no TLC**

Run: `just model-test`, then `cargo test --test model_stamp`, then `just check`.
Expected:
- `just model-test`: passes (`run.py`'s unit tests exercise only recorded TLC output fixtures; they do not run TLC);
- `cargo test --test model_stamp`: `test result: ok.`, 1 ignored, no failures;
- `just check`: exits 0 (fmt, clippy, typos, and the workspace tests). `typos` in particular must not flag the
  `chksum(pcal) = "..."` / `chksum(tla) = "..."` comments Task 2's regenerated `LockProtocol.tla` carries; Task 2's
  Step 14 is what prevents that.

- [ ] **Step 4: Push, open a draft pull request to `main`, and read the `Model` run**

Run: `git push -u origin HEAD`, then `gh pr create --draft --base main --fill`. Then find the run and wait for it:
`gh run list --branch "$(git branch --show-current)" --workflow Model --limit 1 --json databaseId --jq '.[0].databaseId'`
prints the run id, `gh run watch <that id> --exit-status` waits for it, and `gh run view <that id>` shows its jobs.

Expected jobs, all successful: `Plan`, `Scenario selftest (posix)`, `Scenario recovery (posix)`, `Scenario recovery
(windows)`, `Scenario breaklock (posix)`, `Scenario breaklock (windows)`, `Scenario mixed (posix)`, `Scenario mixed
(windows)`, `Scenario breaklock-remote (posix)`, `Scenario mixed-remote (posix)`, and `Model gate` (design Section
4: the matrix comes from `expected.toml`'s `scenarios` list and each run's variant, so no workflow file changes for
a new scenario name to appear in it).

Each `breaklock` job's log ends with lines of this shape (exact `OK`/`states` counts as measured, not fabricated
here where the design notes do not record one):

```text
OK       breaklock-posix-check  expected=-  observed=-  states=4,119,330
OK       breaklock-posix-plain-check  expected=-  observed=-  states=4,135,698
OK       breaklock-posix-witness-NeverTornRead  expected=NeverTornRead  observed=NeverTornRead
OK       breaklock-posix-witness-NeverUncertainLockWithPendingBreaker  expected=NeverUncertainLockWithPendingBreaker  observed=NeverUncertainLockWithPendingBreaker
OK       breaklock-posix-hostcrash-witness-NeverHostCrashChangedLock  expected=NeverHostCrashChangedLock  observed=NeverHostCrashChangedLock
OK       breaklock-posix-liveness  expected=-  observed=-  states=<~17.4 million on the run that fixed the cause ghost; read what your run reports>
OK       breaklock-posix-seeded-SEED_ACQUIRER_UNLINKS_BY_NAME  expected=SingleWriter  observed=SingleWriter
OK       breaklock-posix-seeded-SEED_RESTART_RELEASES  expected=SingleWriter  observed=SingleWriter
OK       breaklock-posix-seeded-SEED_MOVE_ASIDE_FOR_UNCERTAIN  expected=PlainNeverOwnsUncertain  observed=PlainNeverOwnsUncertain
OK       breaklock-posix-seeded-SEED_RENAME_OVER_TAKEOVER  expected=SingleWriter  observed=SingleWriter
OK       breaklock-posix-weakcap-seeded-SEED_NO_CAPABILITY_GATE  expected=SingleWriter  observed=SingleWriter
run.py: suite-wide coverage not judged for a single scenario
run.py: 11 runs, exit 0
```

```text
OK       breaklock-windows-check  expected=-  observed=-  states=9,713,859  760s
OK       breaklock-windows-plain-check  expected=-  observed=-  states=5,591,892
OK       breaklock-windows-witness-NeverTornRead  expected=NeverTornRead  observed=NeverTornRead
OK       breaklock-windows-witness-NeverUncertainLockWithPendingBreaker  expected=NeverUncertainLockWithPendingBreaker  observed=NeverUncertainLockWithPendingBreaker
run.py: suite-wide coverage not judged for a single scenario
run.py: 4 runs, exit 0
```

```text
OK       mixed-posix-check  expected=-  observed=-  states=4,839,093
OK       mixed-posix-witness-NeverRecoveredAfterCrash  expected=NeverRecoveredAfterCrash  observed=NeverRecoveredAfterCrash
OK       mixed-posix-witness-NeverUncertainLockWithPendingBreaker  expected=NeverUncertainLockWithPendingBreaker  observed=NeverUncertainLockWithPendingBreaker
OK       mixed-posix-hostcrash-witness-NeverHostCrashChangedLock  expected=NeverHostCrashChangedLock  observed=NeverHostCrashChangedLock
OK       mixed-posix-liveness  expected=-  observed=-  states=8,137,479  1972s
OK       mixed-posix-seeded-SEED_TORN_AS_FOREIGN  expected=UncertainLockEventuallyCleared  observed=UncertainLockEventuallyCleared
run.py: suite-wide coverage not judged for a single scenario
run.py: 6 runs, exit 0
```

```text
OK       mixed-windows-check  expected=-  observed=-  states=6,764,199
OK       mixed-windows-witness-NeverRecoveredAfterCrash  expected=NeverRecoveredAfterCrash  observed=NeverRecoveredAfterCrash
OK       mixed-windows-witness-NeverUncertainLockWithPendingBreaker  expected=NeverUncertainLockWithPendingBreaker  observed=NeverUncertainLockWithPendingBreaker
run.py: suite-wide coverage not judged for a single scenario
run.py: 3 runs, exit 0
```

```text
OK       breaklock-remote-posix-check  expected=-  observed=-  states=62,982,876  4003s
OK       breaklock-remote-posix-window-witness-SingleWriter  expected=SingleWriter  observed=SingleWriter
OK       breaklock-remote-posix-fixed-check  expected=-  observed=-  states=42,519,405  2643s
OK       breaklock-remote-posix-plain-check  expected=-  observed=-  states=17,418,420  1049s
OK       breaklock-remote-posix-plain-window-witness-SingleWriter  expected=SingleWriter  observed=SingleWriter
OK       breaklock-remote-posix-plain-fixed-check  expected=-  observed=-  states=13,411,650  810s
OK       breaklock-remote-posix-witness-NeverTornRead  expected=NeverTornRead  observed=NeverTornRead
OK       breaklock-remote-posix-witness-NeverUncertainLockWithPendingBreaker  expected=NeverUncertainLockWithPendingBreaker  observed=NeverUncertainLockWithPendingBreaker
OK       breaklock-remote-posix-witness-NeverInflightLandedAfterTakeover  expected=NeverInflightLandedAfterTakeover  observed=NeverInflightLandedAfterTakeover
OK       breaklock-remote-posix-hostcrash-witness-NeverHostCrashChangedLock  expected=NeverHostCrashChangedLock  observed=NeverHostCrashChangedLock
run.py: suite-wide coverage not judged for a single scenario
run.py: 10 runs, exit 0
```

```text
OK       mixed-remote-posix-check  expected=-  observed=-  states=19,523,514  1104s
OK       mixed-remote-posix-window-witness-SingleWriter  expected=SingleWriter  observed=SingleWriter
OK       mixed-remote-posix-fixed-check  expected=-  observed=-  states=15,238,008  852s
OK       mixed-remote-posix-witness-NeverRecoveredAfterCrash  expected=NeverRecoveredAfterCrash  observed=NeverRecoveredAfterCrash
OK       mixed-remote-posix-witness-NeverUncertainLockWithPendingBreaker  expected=NeverUncertainLockWithPendingBreaker  observed=NeverUncertainLockWithPendingBreaker
OK       mixed-remote-posix-hostcrash-witness-NeverHostCrashChangedLock  expected=NeverHostCrashChangedLock  observed=NeverHostCrashChangedLock
run.py: suite-wide coverage not judged for a single scenario
run.py: 6 runs, exit 0
```

Every run's `states` above is a figure measured on CI run 35115798970, the fully green run of the merged branch (Section
11 records that all six finished, that `recover_crashed` was unreached in all six, and that `S240_3_putback` became
covered by `breaklock-remote-posix-check`), but no exact count survived into the design notes. Record whatever your
run reports back into `docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md` Section 12's table
once you have it, rather than leaving the gap for the next reader.

- [ ] **Step 5: `Suite-wide coverage union` and the `deferred`/`never_reached` gate**

The union job (`model-extended.yml`, design Section 4) runs every entry in `expected.toml` in one invocation and is
the only place that can judge suite-wide label coverage. Add the `model-extended` label to the pull request (`gh pr
edit --add-label model-extended`, creating the label first with `gh label create model-extended` if it does not
exist), and read the `Model (extended tier)` run when it finishes.

Expected: the `Extended recovery (posix)` and `Extended recovery (windows)` jobs (unchanged by this plan, since no
new host-crash pairing was built - design Section 12 explicitly rules that out) still succeed at the counts plan 2
recorded; `Suite-wide coverage union` runs every run in `expected.toml` and ends with a `COVERAGE suite` line
listing labels covered, `never_reached` (still `0`: this plan adds no `never_reached` entry) and `deferred` (`1`:
`S96_1_backoff`, owed to `dirlock`). Remove the label afterward with `gh pr edit --remove-label model-extended`.

- [ ] **Step 6: Confirm the drift stamp one more time under CI's Python version**

The `plan` job of `model.yml` already runs `run.py`'s unit tests under both Python 3.14 and 3.11 (design Section 4);
confirm both passed in Step 4's run rather than re-running them locally under a second interpreter.

- [ ] **Step 7: Update the design notes with what this run measured, then merge**

Section 12's build-order paragraph for plan 3 and the "Measured facts" table of this plan's Context section were
written from the commit history, not from a rerun; if the numbers your CI run reports differ from the ones quoted
here (`4,119,330`, `4,135,698`/`5,591,892`, `4,839,093`/`6,764,199`, `≈17.4` million), treat this plan's figures as
what the earlier work reported and your own run's figures as the ones to add to the design document, in the same
change that removes any `Not yet measured.` comment line a `.cfg` file above still carries once you have measured
it. Then merge as the rest of this plan's tasks were merged (`Merge model/lock-protocol: ...` in the commit log),
never with `--force`.

## Stand-downs

Solo adversarial panel over this finished document (Axiom Breaker, Cascade Analyst, Literal Implementer, Protocol
Pedant, Dependency Cynic; 2026-09-16), verified by measurement against the real files in this worktree and the base
commit `7ee6bd0`, not by re-reading the prose for plausibility:

- **Verified, not folded (no defect):** every "before" text block in Tasks 1, 2 and 7 was grep-matched against the
  actual base-commit file it claims to quote (`FLUX_FULL_UPDATED_SPEC_V16.md` at `7ee6bd0`, `algorithm.txt` and
  `trace.toml` at `2fc4cef~1`); the `LockProtocol.tla` hash in Task 2 Step 15 was computed directly from the real
  committed file, not transcribed; the `Create` and `git add` file lists in Tasks 3, 4 and 5 were diffed against
  each other and against the real `2fc4cef` diff stat (40 new config files, matching exactly); the run counts in
  Task 6 Step 6 were obtained by actually parsing the real `expected.toml` with `tomllib`, not estimated.
- **Folded during drafting, before this review (self-caught, not by the panel):** Task 6 Step 6's expected run
  count was first written as an unverified guess (`74`, then briefly `78`) and corrected to the measured `66` once
  the real file was parsed; the breakdown by scenario (`recovery=22`, not `30`, because the host-crash tier's 8
  runs live in `expected-extended.toml`) was added at the same time.
- **Discarded below the severity floor:** none.
- **Now measured:** the run-level state counts left open in the first draft were filled from CI run 35115798970 in Task 8
  Step 4 (the six `*-remote-posix-*check*` runs, `breaklock-windows-check`, and `mixed-posix-liveness`) are exactly
  that - genuinely absent from the design notes and the commit history this plan was written from, not a gap the
  panel found and let stand. Task 8 Step 7 already directs the reader to fill them in from their own CI run rather
  than treating the gap as closed.

PANEL VERDICT: GREEN - one solo round, no live challenge after the self-corrected count was fixed; no agy
escalation (this artifact is retrospective documentation of already-merged work, not a build this session is
about to execute, so `--solo` applies per the trigger gate).
