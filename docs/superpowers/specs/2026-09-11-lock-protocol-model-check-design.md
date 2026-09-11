# Lock-protocol model check — design

Date: 2026-09-11. Status: approved design, awaiting implementation plan.
Branch: `model/lock-protocol`, on top of `spec/v16-resolution`.

## 1. Goal

Flux's V16 specification (`FLUX_FULL_UPDATED_SPEC_V16.md`) defines how operations lock their targets, replace dead
or uncertain locks, revalidate ownership before every write, and claim destination entries at publication. A
six-round adversarial review of that text stopped at its cap still finding defects in exactly these rules each round,
and the last round's fixes are unreviewed (`TODO.md`). This project replaces further prose review of those rules
with an exhaustive, bounded model check that stays in the repository and keeps checking the spec as it changes.

The model must find every reachable violation of the protocol's safety properties within stated bounds, show that
the protocol still makes progress, and stay tied to the spec text.

## 2. Decisions already made

| Decision | Choice |
|---|---|
| What the work leaves behind | A living model in the repository, run by `just` and CI, that must stay green whenever the spec's lock sections change. Findings become spec fixes. |
| Scope | Lock replacement core; crashes at every step and torn lock records; nested destination roots; claims with COMMIT. |
| Filesystem semantics | Two configurations of one filesystem model: POSIX and Windows. |
| Method | TLA+/PlusCal checked with TLC, plus Rust tests that confirm the filesystem assumptions on real operating systems. Chosen independently by the owner's two reviewers (Claude and agy). |

## 3. Spec sections the model encodes

These sections are the model's contract. The drift stamp (Section 9) hashes exactly this list.

| Section | Rule | Model |
|---|---|---|
| 96.1 | Target lock file created exclusively; classification when creation fails (live, uncertain, foreign); OS-native lock for liveness; in-place overwrite only by a takeover | LockProtocol |
| 96.2 | A lock refusal reports the holder | LockProtocol (report content is not checked, only that the refusal happens) |
| 97.1 | Nested destination roots: ancestor check after creating one's own lock; descendant check before creating or writing into a directory | LockProtocol |
| 99 | Commit-time revalidation: "still owned" for every lock held; failure outcomes; supersession checked before cancellation | LockProtocol |
| 21.1 | `--restart` steps 1-5; the lock never released between steps 1 and 5 | LockProtocol |
| 240.1-240.5 | Recovery decision order; live owner; dead-owner move-aside (240.3 steps 1-5); uncertain ownership; `--break-lock` in-place takeover (240.5 steps 1-6) and its cleanup variant | LockProtocol |
| 251.1, 251.2 | Cleanup classification of locks, orphan locks, moved locks, cleanup locks | LockProtocol |
| 259.6 | Lock record: fixed size, checksum, cleanup lock record | LockProtocol |
| 120 | Accepted `workspace_path` values, including `none` | LockProtocol |
| 241.5 | Directory-entry claims; created-entry claim with COMMIT; aliasing; hardlink dependents | Claims |
| 182, 183 | Per-file commit pipeline; recovery of PREPARE_COMMIT without COMMIT | Claims |

## 4. Layout and tooling

```text
models/lockproto/
    FsModel.tla            abstract filesystem shared by both models (Section 5)
    LockProtocol.tla       PlusCal: lock acquisition, replacement, revalidation, cleanup, nesting
    Claims.tla             PlusCal: claims and COMMIT for one operation (Section 6.2)
    base/*.cfg             base configurations: one per scenario and platform (Section 12)
    seeded/*.cfg           defect-seeded configurations (Section 8)
    expected.toml          per configuration: its kind (safety, witness, seeded) and the expected result
    run.py                 the runner (Python 3, standard library only)
    spec-sections.stamp    drift stamp (Section 9)
    README.md              traceability table, probe table, bounds, unverified assumptions, how to run
```

- `just model` runs `run.py`. It fetches a pinned `tla2tools.jar` (exact release chosen in the plan, its SHA-256
  checked before use) into a cache under `target/`, then runs TLC once per entry of `expected.toml`:
  - a safety run checks the base configuration's safety invariants and liveness property and must finish with no
    violation;
  - a witness run checks the same configuration's reachability witnesses with TLC's `-continue` option, so TLC keeps
    exploring after a violation, and every listed witness must be reported violated;
  - a seeded run must stop with a violation of exactly the invariant or property its row names.
  The runner exits non-zero on any mismatch and prints the TLC trace of every unexpected violation.
- `just model-stamp` rewrites `spec-sections.stamp` after the model has been re-checked against changed spec text.
- CI: a new `model` job on `ubuntu-latest` with Java runs `just model`. The drift-stamp test and the filesystem
  probes are ordinary Rust tests, so the existing `just check` and the Linux/macOS/Windows test job run them without
  Java.
- `.claude/recommended-tools.json` gains an entry for Java (needed by `just model`), per the repository's
  right-tool convention.

## 5. Filesystem model (`FsModel.tla`)

State:

| Variable | Meaning |
|---|---|
| `entries` | directory entry name → file object id, per directory |
| `content` | object id → `Record(op, kind)` (kind: `operation` or `cleanup`), `Torn`, or `Foreign` |
| `durable` | object id → the content that survives a crash (content becomes durable only on flush) |
| `handles` | set of `[proc, obj, shareDelete]`; `shareDelete` matters only on Windows |
| `oslock` | object id → holding process, or none |

Operations and their platform rules. Each row is grounded by a filesystem probe (Section 10).

| Operation | POSIX | Windows |
|---|---|---|
| exclusive create at a name | fails if the name exists | fails if the name exists |
| open existing without create | fails if absent | fails if absent |
| rename a name, no-replace | fails if the target name exists; succeeds even if the object is open and OS-locked | succeeds only if every open handle on the object has `shareDelete`; fails if the target name exists |
| unlink a name | succeeds even if the object is open and OS-locked; open handles keep writing to the now-unnamed object | succeeds only if every open handle has `shareDelete` |
| non-blocking OS-native lock | fails if another process holds it | fails if another process holds it |
| write record in one call | sets `content` | sets `content` |
| flush | copies `content` to `durable` | copies `content` to `durable` |
| identity of a name | the object id the name maps to | the object id the name maps to |

`Platform` is a constant. Name equivalence is a constant too: `Fold` maps each name to its equivalence class, the
identity on a case-sensitive destination and case folding on a case-insensitive one; two names with the same class
are the same directory entry.

The spec does not yet state whether Flux opens lock files with delete-sharing on Windows. The model makes it a
constant, `FluxShareDelete`, and the Windows scenarios run with both values. Whichever value lets an invariant fail
is the finding that forces the spec to state the other (Section 11).

Crash of a process: its handles and OS locks are released; each object it wrote but did not flush nondeterministically
keeps the new content, keeps the old durable content, or becomes `Torn`; its in-memory state is lost. A crashed
invocation does not continue; later work is done by new actors.

## 6. Actors

### 6.1 `LockProtocol.tla`

Each PlusCal label is named after its spec step, for example `S240_5_s6` for Section 240.5 step 6.

| Actor | Behaviour |
|---|---|
| Owner | A normal operation: acquires the target lock (96.1), runs its ancestor check (97.1 a), publishes with a Section 99 check before each write, releases the lock at completion |
| StalledOwner | An owner that stops making progress at any point, may later resume (its next Section 99 check then runs), may release its lock, or may already be inside a filesystem call that completes later |
| PlainRun | A new invocation without flags: classifies what it finds (21.1 table, 96.1, 240) and acts or refuses |
| Recoverer | A new invocation that finds a dead owner's lock and runs 240.3 steps 1-5 |
| Breaker | `flux copy --restart --break-lock`: 240.5 steps 1-6, then 21.1 steps 2-5 |
| CleanupBreaker | `flux cleanup --target PATH --break-lock`: 240.5 steps 1-6 with a cleanup lock record, deletes artifacts, deletes its lock |
| Cleanup | `flux cleanup DEST`: classifies and removes orphan, dead, and cleanup locks via 240.3 |
| NestedOwner | An owner whose destination is a child of another owner's destination (97.1) |

Owner liveness evidence is an oracle consistent with the truth: another process may classify an owner as live only
if it is alive, as dead only if it is dead, and as uncertain in either case. Two processes may classify the same
owner differently at different moments, as Section 240 allows.

### 6.2 `Claims.tla`

One directory operation publishes into one destination directory on a case-insensitive filesystem (`Fold` is case
folding), so aliasing happens. Targets:

| Target | Role |
|---|---|
| `A` and `a` | two independent source files whose names fold to one entry; an existing destination entry `A` may or may not be present |
| `H1`, `H2` | a hardlink group: canonical `H1`, dependent `H2` whose name maps to a different entry |
| `D1`, `d1` | a hardlink group whose dependent's name folds onto the canonical's entry |

Each target runs the Section 182 pipeline (plan with its existing-entry claim, PREPARE_COMMIT, rename, COMMIT with
its created-entry claim); dependents run the Section 16.1 link procedure. A crash can happen between any two steps;
a resume then runs Section 183 recovery before continuing. Workers interleave freely.

## 7. Properties

Safety invariants, which must hold in every reachable state of the base configurations:

| Name | Statement |
|---|---|
| `SingleWriter` | For a target, at most one process is inside a publishing step whose last Section 99 check passed. The one exception the spec accepts, a stalled prior owner's call started before an operator `--break-lock` and completing after it, is represented explicitly and is permitted only after such a takeover. |
| `PlainNeverOwnsUncertain` | A process without `--break-lock` creates a lock only at an empty lock path, and never when the lock that was last at that path belongs to a process that is alive and has not released it. |
| `RefusalJustified` | Every `TARGET_LOCK_BUSY` refusal happens while the lock path holds, or the refusing process's open handle refers to, a lock whose owner is alive or another takeover's. |
| `ForeignUntouched` | A `Foreign` object at a lock path is never written, renamed, or deleted. |
| `Classifiable` | In every state, the object at each lock path classifies into exactly one case: live, dead, uncertain, cleanup lock, or foreign. |
| `NestedExclusion` | Two operations whose destinations nest never both write objects under the inner destination. |
| `NoSilentOverwrite` (Claims) | No target overwrites an entry this operation already published. |
| `CommittedHasClaim` (Claims) | Every target with COMMIT has its created-entry claim. |
| `NoSelfCollision` (Claims) | A resumed target never collides with its own claim; a hardlink dependent whose name maps to a different entry never collides with its group's claims. |

Reachability witnesses: invariants that must be violated in the base configurations, which proves the path is
reachable. `NeverAcquired`, `NeverRecovered` (240.3 completes), `NeverBrokeLock` (240.5 completes),
`NeverRecoveredAfterCrash`, `NeverCleanedUp`, `NeverCommittedWithClaim`.

Liveness, checked as temporal properties under weak fairness for every process that has not crashed and a bounded
number of crashes:

- `DeadLockEventuallyCleared`: `[](DeadOwnerLockAt(t) => <>(~DeadOwnerLockAt(t)))`. A lock whose owner is dead is
  eventually removed or replaced, given a Recoverer or Cleanup actor with fairness.
- `UncertainLockEventuallyCleared`: the same for an uncertain owner's lock, given a Breaker or CleanupBreaker actor
  with fairness (only an operator action clears it, Section 240.4).

These are linear-time properties TLC can check; the stronger "from every state some action clears it" is branching
time and is not claimed.

## 8. Defect-seeded configurations

Each seeded configuration sets one flag that re-introduces a defect the adversarial review fixed. `just model`
requires each to fail with the listed invariant; if one passes, the model has lost the ability to see that defect
and the build fails.

| Flag | Defect re-introduced (review finding) | Must fail |
|---|---|---|
| `SEED_RECOVERER_IDENTITY_ONLY` | 240.3 step 3 checks identity only, not the record (round 6, IMC-4) | `SingleWriter` |
| `SEED_EMPTY_PATH_BUSY` | 240.5 step 6 refuses when the lock path is empty instead of retrying (round 6, IMC-3) | `RefusalJustified` |
| `SEED_RESTART_RELEASES` | 21.1 step 5 releases the lock before the new operation starts (round 3) | `SingleWriter` |
| `SEED_MOVE_ASIDE_FOR_UNCERTAIN` | an uncertain owner's lock is moved aside, leaving the path empty (round 5) | `PlainNeverOwnsUncertain` |
| `SEED_RENAME_OVER_TAKEOVER` | takeover by renaming a new record over the lock (round 3) | `SingleWriter` |
| `SEED_NO_ANCESTOR_CHECK` | 97.1 (a) skipped | `NestedExclusion` |
| `SEED_DESCENDANT_EXISTING_ONLY` | 97.1 (b) checks only directories that already exist (round 2) | `NestedExclusion` |
| `SEED_CLAIM_AFTER_COMMIT` | created-entry claim written after COMMIT (round 6, CPE-4) | `CommittedHasClaim` |
| `SEED_CLAIM_BY_OBJECT_ID` | claims keyed by object identity (round 3) | `NoSelfCollision` |
| `SEED_NO_PUBLICATION_CLAIM` | publications do not claim the entry they create (round 2) | `NoSilentOverwrite` |

Every later fix that the model drives adds a row here.

## 9. Spec-drift stamp

`spec-sections.stamp` lists each section of Section 3 by its heading, with the BLAKE3 hash (the workspace's
`blake3` dependency) of its text: from its heading line to the next heading of the same or higher level, with line
endings normalised to LF. A Rust test (in the root `tests/` target, run by `just check`) recomputes the hashes and
fails, naming each changed section, if any differs. The fix is to re-check the model against the changed text,
update it, run `just model`, then run `just model-stamp`.

## 10. Filesystem probes

Rust tests in `crates/flux-platform/tests/fs_semantics.rs`, run on Linux, macOS, and Windows by the existing test
job. Each confirms one assumption of Section 5 on the real platform; the README maps each to the model operation it
grounds.

| Probe | Assumption | Platforms |
|---|---|---|
| FS-1 | a second exclusive create of the same name fails | all |
| FS-2 | a no-replace rename fails when the target name exists | all |
| FS-3 | a non-blocking OS-native lock fails while another handle holds it | all |
| FS-4 | renaming an open, OS-locked file succeeds | Linux, macOS |
| FS-5 | unlinking an open file succeeds and the open handle still writes to the unnamed object | Linux, macOS |
| FS-6 | renaming or deleting a file open without delete-sharing fails | Windows |
| FS-7 | renaming or deleting a file open with delete-sharing succeeds | Windows |
| FS-8 | a name replaced by a new file reports a different file identity | all |

A failing probe means the model's assumption for that platform is wrong: the model is fixed, not the probe.

## 11. Handling findings

A counterexample in a base configuration is saved with its trace and triaged:

- Model defect: fixed in the model.
- Spec defect: fixed in the spec on the spec branch. A design fork goes through an agy-first consult and then to the
  owner. The fix gets a new seeded configuration (Section 8), and the stamp is updated.

The expected early finding is the Windows sharing mode for lock-file handles, which Section 240 relies on but never
states.

## 12. Bounds and time budget

Each base configuration is one scenario on one platform. A scenario names the actors that run together; keeping
them apart keeps each state space small.

| Scenario | Actors | Platforms |
|---|---|---|
| `recovery` | Owner (crashes), 2 Recoverers, PlainRun, Cleanup | POSIX, Windows (both `FluxShareDelete` values) |
| `breaklock` | StalledOwner, 2 Breakers, PlainRun | POSIX, Windows (both values) |
| `mixed` | StalledOwner, Breaker, Recoverer, PlainRun (the round-6 two-owner race) | POSIX, Windows (both values) |
| `cleanup` | StalledOwner, CleanupBreaker, Breaker, Cleanup | POSIX, Windows (both values) |
| `nested` | Owner on the parent destination, NestedOwner on the child, PlainRun | POSIX |
| `claims` | Claims model, 2 workers, one crash and resume | not platform-specific |

Common bounds: one target lock path (plus the parent's for `nested`), at most two crashes per run, symmetry over
interchangeable actors of the same kind. Each TLC run should finish in about five minutes on a CI runner, and the
whole `model` job in about 30 minutes, with runs in parallel where the runner allows. A run that does not fit gets
tighter bounds, and the tighter bounds are written into the README, never raised silently.

## 13. Success criteria

1. Every seeded configuration fails with its named invariant.
2. Every base configuration passes its safety invariants and liveness property, and violates every reachability
   witness.
3. Every configuration finishes within the time budget on CI.
4. Every PlusCal label maps to a spec step in the README table, and every spec step in Section 3 maps to a label or
   to an explicit "not modelled" entry with its reason.
5. The drift-stamp test and FS-1 to FS-8 pass in the existing test job.

## 14. Out of scope

- Concurrency testing of the Rust implementation (loom, shuttle, or madsim), once lock code exists; recorded in
  `TODO.md`.
- Filesystems beyond the two configurations (FAT32, exFAT, SMB, NFS), and clock behaviour beyond the liveness oracle:
  listed in the README as unverified assumptions.
- The content of refusal reports (96.2) and exit codes: the model checks which outcome happens, not its wording.
