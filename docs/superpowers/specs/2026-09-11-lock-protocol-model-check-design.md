# Lock-protocol model check — design

Date: 2026-09-11. Status: approved design, revised after adversarial review round 1; awaiting implementation plan.
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
| Filesystem semantics | Two configurations of one filesystem model: POSIX and Windows (plus a weak-identity variant, Section 5). |
| Method | TLA+/PlusCal checked with TLC, plus Rust tests that confirm the filesystem assumptions on real operating systems. Chosen independently by the owner's two reviewers (Claude and agy). |

## 3. Spec sections the model encodes

These sections are the model's contract. The drift stamp (Section 9) hashes exactly this list.

| Section | Rule | Model |
|---|---|---|
| 96.1 | Target lock file created exclusively; classification when creation fails (another operation's record, this operation's record for a different spelling, unreadable, foreign); OS-native lock for liveness; in-place overwrite only by a takeover | LockProtocol (the different-spelling branch is "not modelled": LockProtocol uses one spelling per target; that branch is a deterministic comparison with no interleaving and is pinned by spec acceptance test 54) |
| 96.2 | A lock refusal reports the holder | LockProtocol (report content is not checked, only that the refusal happens) |
| 97.1 | Nested destination roots: ancestor check after creating one's own lock; descendant check before creating or writing into a directory | LockProtocol |
| 99 | Commit-time revalidation: "still owned" for every lock held; failure outcomes; supersession checked before cancellation | LockProtocol, Claims |
| 21.1 | `--restart` steps 1-5; the lock never released between steps 1 and 5 | LockProtocol |
| 240.1-240.5 | Recovery decision order; live owner; dead-owner move-aside (240.3 steps 1-5); uncertain ownership; `--break-lock` in-place takeover (240.5 steps 1-6) and its cleanup variant | LockProtocol |
| 251.1, 251.2 | Cleanup classification of locks, orphan locks, moved locks, cleanup locks | LockProtocol |
| 259.6 | Lock record: fixed size, checksum, cleanup lock record | LockProtocol |
| 120 | Accepted `workspace_path` values, including `none` | LockProtocol |
| 241.5 | Directory-entry claims; created-entry claim with COMMIT; aliasing; hardlink dependents | Claims |
| 182, 183 | Per-file commit pipeline, including its lock revalidation; recovery of PREPARE_COMMIT without COMMIT | Claims |

## 4. Layout and tooling

```text
models/lockproto/
    FsModel.tla            abstract filesystem shared by both models (Section 5)
    LockProtocol.tla       PlusCal: lock acquisition, replacement, revalidation, cleanup, nesting (Section 6.1)
    Claims.tla             PlusCal: claims and COMMIT for one operation (Section 6.2)
    configs/*.cfg          one per run (Section 12): check runs, liveness runs, seeded runs
    expected.toml          the run list and each run's expected result (format below)
    trace.toml             the traceability map: every label and every spec step (Section 9.1)
    run.py                 the runner (Python 3, standard library only)
    spec-sections.stamp    drift stamp (Section 9)
    README.md              traceability table, probe table, bounds, unverified assumptions, how to run
tests/model_stamp.rs     the drift-stamp test (Section 9)
crates/flux-platform/tests/fs_semantics.rs    the filesystem probes (Section 10)
.github/workflows/model.yml                   the model CI workflow
```

`expected.toml` holds one `[[run]]` table per TLC run with these fields:

| Field | Meaning |
|---|---|
| `name` | unique run name, used in CI and in the runner's report |
| `module` | `LockProtocol` or `Claims` |
| `config` | path of the `.cfg` file |
| `scenario` | the Section 12 scenario the run belongs to; CI groups runs by it |
| `kind` | `check`, `liveness`, or `seeded` |
| `violated` | for `check`: the exact set of witness invariants that must be reported violated; for `seeded`: the one invariant or property that must be the first violation; absent for `liveness` |
| `timeout_minutes` | the run's time limit |

How each kind of run is judged:

- `check`: TLC runs with `-continue`, so it explores the whole reachable state space and reports every violated
  invariant. The run passes only if the set reported equals `violated` exactly: every witness violated, no safety
  invariant violated. One run per configuration therefore checks both safety and reachability.
- `liveness`: TLC checks the configuration's temporal properties with no symmetry reduction (symmetry and liveness
  checking together are unsound in TLC). Passes only with no violation.
- `seeded`: TLC runs without `-continue`. Passes only if TLC stops with a violation of exactly the named invariant or
  property; a different violation, or none, fails the run.

TLC's deadlock check is turned off (`-deadlock`) in every run, because actors legitimately terminate; progress is
checked by the witnesses and the liveness properties instead.

The runner, `run.py`:

- downloads a pinned `tla2tools.jar` (exact release chosen in the plan) into `target/tla/` and verifies its SHA-256
  before every use, including a cached copy; on a mismatch it deletes the file and downloads once more, and if the
  hash still does not match it stops with exit code 2;
- runs the selected runs (all, or one scenario with `--scenario NAME`), each under its `timeout_minutes`, killing TLC
  on timeout;
- prints one line per run (name, expected, observed, states found, duration) and, for every unexpected result, the
  first 60 states of TLC's trace, saving the full TLC output under `target/tla/out/<name>.log`;
- exits 0 when every run matched, 1 when any run's result did not match, and 2 for a tooling failure (Java missing,
  download or checksum failure, TLC crash, out of memory, or timeout), naming the cause. A run that times out is
  reported as `TIMEOUT` with the states explored so far, never as a witness that was not violated, because a partial
  state space cannot show that a path is unreachable. When both happen, a mismatch outranks a tooling failure: the exit
  code is 1 if any run completed with an unexpected result, otherwise 2 if any run hit a tooling failure.

Recipes and CI:

- `just model` runs every run; `just model scenario=<name>` runs one scenario; `just model-stamp` rewrites the stamp
  (Section 9). None of these is part of `just check`, which stays Java-free.
- `.github/workflows/model.yml` runs on pushes and pull requests to `main` that touch `models/**`,
  `FLUX_FULL_UPDATED_SPEC_V16.md`, `crates/flux-platform/tests/fs_semantics.rs`, or the workflow itself, and on
  manual dispatch. It has one matrix job per Section 12 scenario, each installing Java with `actions/setup-java`
  (Temurin 21) and running `just model scenario=<name>`; the job uploads `target/tla/out/` when it fails.
- The drift-stamp test and the filesystem probes are ordinary Rust tests, so `just check` and the existing
  Linux/macOS/Windows test job run them.
- `.claude/recommended-tools.json` gains entries for Java and Python 3, the two non-Rust tools `just model` needs.
- Whether the `model` job becomes a required status check is left to the owner and is outside this design.

## 5. Filesystem model (`FsModel.tla`)

State:

| Variable | Meaning |
|---|---|
| `entries` | per directory, entry name → file object id; two names are the same entry when `Fold` maps them to the same class |
| `content` | object id → `Record(op, kind)` (kind: `operation` or `cleanup`), `Torn`, or `Foreign`: what a reader sees now |
| `durable` | object id → the content that survives a crash |
| `handles` | set of `[proc, obj, shareDelete]` |
| `oslock` | object id → the process holding its OS-native lock, or none |
| `inflight` | set of filesystem calls issued by a process and not yet complete (Section 5.2) |

### 5.1 Operations

Each row is grounded by a filesystem probe (Section 10) where a real platform can confirm it.

| Operation | POSIX | Windows |
|---|---|---|
| exclusive create at a name | fails if an entry of the same class exists; otherwise creates an empty object and a handle | same |
| open existing without create | fails if absent; otherwise adds a handle with the chosen `shareDelete` | same; a new handle without delete-sharing cannot be opened while the object has a pending delete |
| close a handle | removes the handle; releases its OS-native lock if held | same; lifts the sharing restriction that handle imposed |
| non-blocking OS-native lock | fails if another process holds it | same |
| release an OS-native lock | clears `oslock` | same |
| rename, no-replace | fails if the target class exists; succeeds even if the object is open and OS-locked | fails if the target class exists; succeeds only if every open handle on the object has `shareDelete` |
| unlink | succeeds even if the object is open and OS-locked; open handles keep writing to the now-unnamed object | succeeds only if every open handle has `shareDelete` |
| write a lock record | two steps: `WriteBegin` sets `content` to `Torn`, `WriteEnd` sets it to the new record; a reader in between sees `Torn` | same |
| flush | copies `content` to `durable` | same |
| identity of a name | the object id the name maps to; under `IdentityStrength = Weak` it may instead be the id of an object that name held earlier | same |

Constants: `Platform ∈ {"posix", "windows"}`; `Fold`, the name-equivalence map (identity, or case folding);
`IdentityStrength ∈ {"strong", "weak"}`, where `weak` stands for FAT32/exFAT-like identity that CI cannot probe;
`ShareMode`, either `"any"` or a record giving the `shareDelete` value Flux uses at each open site (owner lock handle,
classification read, recoverer, breaker, cleanup). With `"any"`, every open chooses `shareDelete` nondeterministically,
so TLC explores every combination; the spec does not yet state these values, and the first Windows run is expected
to find which combinations are unsafe (Section 11). Once the spec states them, the configurations switch to the
record.

### 5.2 In-flight calls and crashes

A data write or rename that an operation issues as part of publishing is two steps: issue, then complete. Other
actors' steps can run between them, and the call's effect lands at completion. This is how the model represents a
stalled owner whose call "can still complete" (Section 240.5).

A crash of a process:

- releases its handles, their sharing restrictions, and its OS-native locks;
- for each object it wrote since its last flush, sets `durable` nondeterministically to the old durable content, the
  new content, or `Torn`; a crash during a flush has the same outcomes;
- lands or drops, nondeterministically, each of its in-flight calls;
- loses its in-memory state; a crashed invocation never continues, and later work is done by other actors.

At most two crashes happen in one run, counted across all actors.

## 6. Actors

### 6.1 `LockProtocol.tla`

Each PlusCal label is named `S<section>_<step>` after the spec step it implements, with dots in the section number
written as underscores. Where the spec numbers its steps, `<step>` is `s` and the number: Section 240.5 step 6 is
`S240_5_s6`. Where the spec states a rule without numbered steps (96.1, 97.1, 99, 120, 241.5), `<step>` is a short
name defined in `trace.toml` next to the spec sentence it implements, for example `S97_1_ancestor`. A step that needs
more than one atomic action gets suffixes `a`, `b`, and so on (`S240_5_s6a`).

Encodings: a lock record is a TLA+ record `[op |-> <operation id>, kind |-> "operation" | "cleanup"]`; `Torn`,
`Foreign`, and `Empty` are distinct model values; operation ids are model values drawn from the actors present.

| Actor | Behaviour |
|---|---|
| Owner | A normal operation: acquires the target lock (96.1) and holds its OS-native lock, runs its ancestor check (97.1 a), publishes with a Section 99 check before each write, releases the lock at completion; may crash |
| StalledOwner | An owner that stops making progress at any point, may later resume (its next Section 99 check then runs), may release its lock, or may have a call in flight that completes later |
| PlainRun | A new invocation without flags: classifies what it finds (21.1 table, 96.1, 120, 240) and acts or refuses |
| Recoverer | A new invocation that finds a dead owner's lock and runs 240.3 steps 1-5 |
| Breaker | `flux copy --restart --break-lock`: 240.5 steps 1-6, then 21.1 steps 2-5 |
| CleanupBreaker | `flux cleanup --target PATH --break-lock`: 240.5 steps 1-6 with a cleanup lock record, deletes artifacts, deletes its lock; may crash |
| Cleanup | `flux cleanup DEST`: classifies and removes orphan, dead, and cleanup locks via 240.3 |
| NestedOwner | An owner whose destination is a child of another owner's destination (97.1) |

Classification of a lock's owner combines an OS-native lock attempt with an oracle:

- If the owner holds the OS-native lock, a non-blocking attempt by another process fails, and the classifier sees
  the owner as live. An owner releases its OS-native lock only by crashing or by releasing the lock at completion.
- Otherwise the oracle decides, consistent with the truth: live only if the owner is alive, dead only if it has
  crashed, uncertain in either case. Two classifiers may disagree at different moments, as Section 240 allows.

Ghost variables, used only by the properties: `classified[p]`, the last classification each process made of the lock
path (live, dead, uncertain, cleanup lock, foreign, empty), and `lastRecord[path]`, the record most recently present
at each lock path.

### 6.2 `Claims.tla`

One directory operation publishes into one destination directory on a case-insensitive filesystem (`Fold` is case
folding), so aliasing happens. Targets:

| Target | Role |
|---|---|
| `A` and `a` | two independent source files whose names fold to one entry; an existing destination entry `A` may or may not be present |
| `H1`, `H2` | a hardlink group: canonical `H1`, dependent `H2` whose name maps to a different entry |
| `D1`, `d1` | a hardlink group whose dependent's name folds onto the canonical's entry |

Each target runs the Section 182 pipeline: plan with its existing-entry claim, PREPARE_COMMIT, revalidate locks
(Section 99), rename, then COMMIT together with its created-entry claim in one durable transaction. Dependents run the
Section 16.1 link procedure. An environment action, `LockLost`, can take the operation's lock over at any point (a
`--break-lock` from outside), after which every worker's next revalidation fails and it stops. A crash can happen
between any two steps; a resume then runs Section 183 recovery before continuing. Two workers interleave freely.

## 7. Properties

Safety invariants, which must hold in every reachable state:

| Name | Statement |
|---|---|
| `SingleWriter` | For a target, at most one process is inside a publishing step whose last Section 99 check passed. The one exception the spec accepts, a stalled prior owner's in-flight call issued before an operator `--break-lock` and completing after it, is permitted only in that case. |
| `PlainNeverOwnsUncertain` | A process without `--break-lock` never removes, renames, or overwrites a lock it classified as uncertain (`classified`), and creates a lock only at an empty lock path. |
| `RefusalJustified` | Every `TARGET_LOCK_BUSY` refusal happens while the lock path holds, or the refusing process's open handle refers to, a lock whose owner is alive or another takeover's. |
| `ForeignUntouched` | A `Foreign` object at a lock path is never written, renamed, or deleted. |
| `Classifiable` | In every state, the object at each lock path classifies into exactly one case: live, dead, uncertain, cleanup lock, or foreign; a `Torn` record classifies as uncertain. |
| `NestedExclusion` | Two operations whose destinations nest never both write objects under the inner destination. |
| `NoSilentOverwrite` (Claims) | No target overwrites an entry this operation already published. |
| `CommittedHasClaim` (Claims) | Every target with COMMIT has its created-entry claim. |
| `NoSelfCollision` (Claims) | A resumed target never collides with its own claim; a hardlink dependent whose name maps to a different entry never collides with its group's claims; a dependent folded onto its canonical's entry is reported as a collision. |
| `NoPublishAfterLockLost` (Claims) | No rename happens after `LockLost` unless the worker's revalidation ran before `LockLost`. |

Reachability witnesses, which every `check` run must report violated, proving each path is reachable. Each witness
is the negation of a ghost flag that only the named procedure's final label sets (for example, `recovered` is set
only by a Recoverer at `S240_3_s5`), so an end state reached by some other path cannot satisfy it.

| Witness | Reached when |
|---|---|
| `NeverAcquired` | an operation acquires the target lock |
| `NeverRecovered` | a Recoverer completes 240.3 |
| `NeverBrokeLock` | a Breaker or CleanupBreaker completes 240.5 |
| `NeverRecoveredAfterCrash` | a lock left by a crash is later replaced |
| `NeverCleanedUp` | Cleanup removes a lock |
| `NeverTornRead` | a process reads a `Torn` record |
| `NeverInFlightAfterTakeover` | a stalled owner's in-flight call completes after a takeover |
| `NeverClassifiedCleanupLock` | a PlainRun or Recoverer classifies a cleanup lock |
| `NeverCommittedWithClaim` (Claims) | a target commits with its claim |
| `NeverLockLostMidCommit` (Claims) | `LockLost` happens between a PREPARE_COMMIT and its rename |

Each scenario's `check` run lists the witnesses its actors can reach (Section 12).

Liveness, checked in `liveness` runs under weak fairness for every process that has not crashed:

- `DeadLockEventuallyCleared`: `[](DeadOwnerLockAt(t) => <>(~DeadOwnerLockAt(t)))`. A lock whose owner is dead is
  eventually removed or replaced, given a Recoverer or Cleanup actor.
- `UncertainLockEventuallyCleared`: the same for an uncertain owner's lock, given a Breaker or CleanupBreaker actor
  (only an operator action clears it, Section 240.4).

These are linear-time properties TLC can check. The stronger "from every state some action clears it" is branching
time and is not claimed.

## 8. Defect-seeded configurations

Each seeded configuration sets one flag that re-introduces a defect the adversarial review fixed, in the scenario that
can reach it. `just model` requires each to stop with the listed violation; if one passes, the model has lost the
ability to see that defect and the run fails.

| Flag | Defect re-introduced (review finding) | Scenario | Must fail |
|---|---|---|---|
| `SEED_RECOVERER_IDENTITY_ONLY` | 240.3 step 3 checks identity only, not the record (round 6, IMC-4) | `mixed` | `SingleWriter` |
| `SEED_EMPTY_PATH_BUSY` | 240.5 step 6 refuses when the lock path is empty instead of retrying (round 6, IMC-3) | `breaklock` | `RefusalJustified` |
| `SEED_RESTART_RELEASES` | 21.1 step 5 releases the lock before the new operation starts (round 3) | `breaklock` | `SingleWriter` |
| `SEED_MOVE_ASIDE_FOR_UNCERTAIN` | an uncertain owner's lock is moved aside, leaving the path empty (round 5) | `breaklock` | `PlainNeverOwnsUncertain` |
| `SEED_RENAME_OVER_TAKEOVER` | takeover by renaming a new record over the lock (round 3) | `breaklock` | `SingleWriter` |
| `SEED_NO_IDENTITY_RECHECK` | 240.5 step 6 skips its identity check after the write (round 6) | `breaklock` | `SingleWriter` |
| `SEED_CLEANUP_LOCK_UNVERIFIABLE` | Section 120 rejects `workspace_path = none` (round 6) | `cleanup-crash` | `DeadLockEventuallyCleared` |
| `SEED_TORN_AS_FOREIGN` | a checksum-failing record is treated as a foreign object (round 5) | `recovery` | `UncertainLockEventuallyCleared` |
| `SEED_NO_ANCESTOR_CHECK` | 97.1 (a) skipped | `nested` | `NestedExclusion` |
| `SEED_DESCENDANT_EXISTING_ONLY` | 97.1 (b) checks only directories that already exist (round 2) | `nested` | `NestedExclusion` |
| `SEED_CLAIM_AFTER_COMMIT` | created-entry claim written after COMMIT (round 6, CPE-4) | `claims` | `CommittedHasClaim` |
| `SEED_CLAIM_BY_OBJECT_ID` | claims keyed by object identity (round 3) | `claims` | `NoSelfCollision` |
| `SEED_NO_PUBLICATION_CLAIM` | publications do not claim the entry they create (round 2) | `claims` | `NoSilentOverwrite` |
| `SEED_NO_REVALIDATE_BEFORE_RENAME` | Section 182 renames without revalidating locks | `claims` | `NoPublishAfterLockLost` |

A seeded run whose "must fail" entry is a liveness property is a `liveness`-kind run with the seed flag set, run
without symmetry. Every later fix that the model drives adds a row here.

## 9. Spec-drift stamp

`spec-sections.stamp` has one line per spec heading the model encodes, listed individually (96.1, 96.2, 97.1, 99,
21.1, 240.1, 240.2, 240.3, 240.4, 240.5, 251.1, 251.2, 259.6, 120, 241.5, 182, 183): the BLAKE3 hash of that
heading's own text in lowercase hex, two spaces, then the heading line exactly as it appears in the spec. A heading's
own text runs from its heading line up to, not including, the next heading line of any level, so a subsection the
model does not encode (for example 99.1) is not hashed with its parent. Heading lines are ATX headings (`#` to
`######`) outside fenced code blocks; line endings are normalised to LF before hashing. A subsection the model does
encode is listed as its own line.

`tests/model_stamp.rs`, an auto-discovered test target of the root package (so `just check` runs it), recomputes the
hashes. It fails, naming each section, if a hash differs, a listed heading is missing, or a heading appears more than
once. `blake3` is added to the root package's `[dev-dependencies]` from the workspace. The same file holds an
`#[ignore]`d test that rewrites the stamp; `just model-stamp` runs only that test. The workflow after a spec change:
re-check the model against the changed text, update the model, run `just model`, then run `just model-stamp`.

### 9.1 Traceability check

`trace.toml` is the single traceability map. Its unit is a spec unit of a stamped heading's own text (Section 9): a
numbered step where the heading has numbered steps, otherwise each block of that text separated by blank lines
(fenced blocks included, split at their blank lines too). Each entry names its heading and the unit's ordinal, quotes
a sentence from that unit, and gives either the labels that implement it or `not_modelled = "<reason>"`. The same
test file checks, without Java:

- every unit of every stamped heading has exactly one entry, and each quoted sentence occurs in its unit;
- every label in `LockProtocol.tla` and `Claims.tla` (matched by `S\d+(_\d+)*_\w+:`) appears in `trace.toml`;
- every label `trace.toml` names exists in a model file.

Because units are counted from the spec text, an entry cannot be left out, and a spec edit that adds a unit fails the
check until the map covers it. The README renders the map for readers but is not the source of truth.

## 10. Filesystem probes

Rust tests in `crates/flux-platform/tests/fs_semantics.rs`, run on Linux, macOS, and Windows by the existing test
job. They use `rustix` on Unix (`flock`/`fcntl`, `fstat`) and `windows-sys` on Windows (`CreateFileW` share flags,
`LockFileEx`, `GetFileInformationByHandle`) as `flux-platform` dev-dependencies, subject to `deny.toml`; no standard
library API newer than the workspace's `rust-version` (1.85) is used, because clippy's MSRV lint would fail the gate.
`tempfile` (already pinned in the workspace) provides each probe's scratch directory. Each probe is compiled only on
the platforms its row lists (`#[cfg]`), and each platform runs at least four probes. Each probe first asserts its own
precondition, so it cannot pass without exercising what it claims: FS-4, for example, confirms the OS-native lock is
held (a second handle's non-blocking attempt fails) before it renames.

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
| FS-9 | closing a handle releases its OS-native lock | all |

A failing probe means the model's assumption for that platform is wrong: the model is fixed, not the probe.
`IdentityStrength = weak` and case-folding behaviour of FAT32/exFAT cannot run on CI runners; the README lists them
as unverified assumptions.

## 11. Handling findings

A counterexample in a `check` or `liveness` run is saved with its trace and triaged:

- Model defect: fixed in the model.
- Spec defect: fixed in the spec on the spec branch. A design fork goes through an agy-first consult and then to the
  owner. The fix gets a new seeded configuration (Section 8), and the stamp is updated.

The expected first finding is the Windows sharing mode: with `ShareMode = "any"`, some combination of open sites
without delete-sharing should break an invariant, which forces the spec to state the mode at each open site.

## 12. Scenarios, bounds, and time budget

Each configuration is one scenario on one platform variant. A scenario names the actors that run together; keeping
them apart keeps each state space small.

| Scenario | Actors | Variants | Witnesses its `check` run expects |
|---|---|---|---|
| `recovery` | Owner (crashes), 2 Recoverers, PlainRun, Cleanup | POSIX, Windows, POSIX weak-identity | `NeverAcquired`, `NeverRecovered`, `NeverRecoveredAfterCrash`, `NeverCleanedUp`, `NeverTornRead` |
| `breaklock` | StalledOwner, 2 Breakers, PlainRun | POSIX, Windows | `NeverAcquired`, `NeverBrokeLock`, `NeverInFlightAfterTakeover`, `NeverTornRead` |
| `mixed` | Owner (crashes; its lock is dead), Breaker (may see it uncertain), Recoverer (may see it dead), PlainRun | POSIX, Windows, POSIX weak-identity | `NeverRecovered`, `NeverBrokeLock` |
| `cleanup` | StalledOwner, CleanupBreaker, Breaker | POSIX, Windows | `NeverBrokeLock`, `NeverInFlightAfterTakeover` |
| `cleanup-crash` | CleanupBreaker (crashes), PlainRun, Recoverer, Cleanup | POSIX, Windows | `NeverClassifiedCleanupLock`, `NeverCleanedUp` |
| `nested` | Owner on the parent destination, NestedOwner on the child, PlainRun | POSIX | `NeverAcquired` |
| `claims` | Claims model, 2 workers, `LockLost`, one crash and resume | not platform-specific | `NeverCommittedWithClaim`, `NeverLockLostMidCommit` |

Liveness runs: `DeadLockEventuallyCleared` in `recovery` and `cleanup-crash`; `UncertainLockEventuallyCleared` in
`breaklock` and `cleanup`; POSIX variant only, without symmetry.

Common bounds: one target lock path (plus the parent's for `nested`); at most two crashes per run in total; symmetry
over interchangeable actors of the same kind in `check` and `seeded` runs only. Each TLC run's `timeout_minutes` is
10; each CI matrix job (one scenario) should finish within about 20 minutes. A run that does not fit gets tighter
bounds, and the tighter bounds are written into the README, never raised silently.

## 13. Success criteria

1. Every seeded run stops with its named violation.
2. Every `check` run reports exactly its listed witnesses violated and no safety invariant violated.
3. Every `liveness` run passes.
4. Every run finishes within its time limit on CI.
5. The traceability check (Section 9.1) passes: every label maps to a spec step, and every spec step of the stamped
   headings maps to a label or to a `not_modelled` reason.
6. The drift-stamp test, the traceability check, and FS-1 to FS-9 pass in the existing test job.

## 14. Out of scope

- Concurrency testing of the Rust implementation (loom, shuttle, or madsim), once lock code exists; recorded in
  `TODO.md`.
- Filesystems beyond the modelled variants (SMB, NFS), and clock behaviour beyond the liveness oracle: listed in the
  README as unverified assumptions.
- The content of refusal reports (96.2) and exit codes: the model checks which outcome happens, not its wording.
- Whether the `model` CI job is a required status check.
