# Lock-protocol model check — design

Date: 2026-09-11. Status: approved design, revised after adversarial review rounds 1 to 6; awaiting implementation plan.
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
| Scope | Lock replacement core; crashes at every step and torn lock records; nested destination roots; claims with COMMIT; and (added by the owner after review round 2) Section 96.1's directory-lock fallback and its announce-then-check exclusion. |
| Filesystem semantics | Two configurations of one filesystem model: POSIX and Windows (plus weak-identity and weak-lock-capability variants, Section 5). |
| Method | TLA+/PlusCal checked with TLC, plus Rust tests that confirm the filesystem assumptions on real operating systems. Chosen independently by the owner's two reviewers (Claude and agy). |

## 3. Spec sections the model encodes

These sections are the model's contract. The drift stamp and the traceability check (Section 9) cover exactly this
list.

| Section | Rule | Model |
|---|---|---|
| 96.1 | Target lock file created exclusively; classification when creation fails (another operation's record, this operation's record for a different spelling, unreadable, foreign); OS-native lock for liveness; in-place overwrite only by a takeover; the directory-lock fallback `P/.flux-dir.lock` and its announce-then-check exclusion with per-name locks; the root lock `T/.flux-root.lock` | LockProtocol. Two parts are "not modelled": the different-spelling branch (LockProtocol uses one spelling per target; the branch is a deterministic comparison with no interleaving, pinned by spec acceptance test 54) and the root lock (the same exclusive creation at a different path, which adds no interleaving to an abstract lock path) |
| 96.2 | A lock refusal reports the holder | LockProtocol (report content is not checked, only that the refusal happens) |
| 97.1 | Nested destination roots: ancestor check after creating one's own lock; descendant check before creating or writing into a directory | LockProtocol |
| 99 | Commit-time revalidation: "still owned" for every lock held; failure outcomes; supersession checked before cancellation | LockProtocol, Claims |
| 99.1 | Weak or unavailable filesystem identity does not invalidate the name-based target lock; a lock is no proof of object continuity | LockProtocol (the `IdentityStrength = weak` variants) |
| 21.1 | `--restart` steps 1-5; the lock never released between steps 1 and 5 | LockProtocol |
| 240.1-240.5 | Recovery decision order; live owner; dead-owner move-aside (240.3 steps 1-5); uncertain ownership; `--break-lock` in-place takeover (240.5 steps 1-6) and its cleanup variant | LockProtocol |
| 251.1, 251.2 | Cleanup classification of locks, orphan locks, moved locks, cleanup locks | LockProtocol |
| 259.6 | Lock record: fixed size, checksum, cleanup lock record | LockProtocol |
| 120 | Accepted `workspace_path` values, including `none` | LockProtocol |
| 235.1 | `RemoteUnverified` and `Unsupported` lock capabilities refuse every operation needing target exclusivity (`REMOTE_LOCK_UNSAFE`) | LockProtocol (the `LockCapability = weak` variant) |
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
    run.py                 the runner (Python 3.11 or later, standard library only: it reads TOML with tomllib)
    spec-sections.stamp    drift stamp (Section 9)
    README.md              traceability table, probe table, bounds, unverified assumptions, how to run
tests/model_stamp.rs     the drift-stamp test (Section 9)
crates/flux-platform/tests/fs_semantics.rs    the filesystem probes (Section 10)
.github/workflows/model.yml                   the model CI workflow
```

`expected.toml` holds one `[[run]]` table per TLC run with these fields:

| Field | Meaning |
|---|---|
| `name` | unique run name, used in CI and in the runner's report: `<scenario>-<variant>-<kind>`, plus `-<seed flag>` for a seeded run (for example `breaklock-windows-check`, `mixed-posix-seeded-SEED_TORN_AS_FOREIGN`) |
| `module` | `LockProtocol` or `Claims` |
| `config` | path of the `.cfg` file |
| `scenario` | the Section 12 scenario the run belongs to; one of the names in `expected.toml`'s top-level `scenarios` list, which is the only place scenario names are defined; CI builds its matrix from that list |
| `kind` | `check`, `liveness`, or `seeded` |
| `violated` | for `check`: the exact set of witness invariants that must be reported violated; for `seeded`: the one invariant or property that must be the first violation; absent for `liveness` |
| `open_findings` | optional, for `check` and `liveness`: safety invariants or properties that currently fail because of a spec defect not yet fixed, each as `{ name, tracking, fix_flag }`: `tracking` names its `TODO.md` or `.clavity/local-anomalies.md` entry, and `fix_flag` names a model flag that applies the proposed spec fix |
| `timeout_minutes` | the run's time limit |

How each kind of run is judged:

- `check`: TLC runs with `-continue`, so it explores the whole reachable state space and reports every violated
  invariant. The run passes only if the set reported equals `violated` plus the run's `open_findings` exactly: every
  witness violated, every open finding still violated, no other safety invariant violated. One run per configuration
  therefore checks both safety and reachability. An open finding that stops failing also fails the run, so the entry
  is removed together with the spec fix.
- `liveness`: TLC runs with `-continue` and checks the configuration's one temporal property, and every safety
  invariant of the scenario, with no symmetry reduction (symmetry and liveness checking together are unsound in TLC).
  Passes only with no violation other than its `open_findings`. A configuration that checks a temporal property (a
  liveness run, or a seeded run whose "must fail" entry is a property) lists exactly one `PROPERTY`, because TLC
  reports a temporal violation without naming the property (measured against TLC 2.19).
- Fix-flag runs: for every `check` or `liveness` run with open findings, the runner also runs the same configuration
  with all their fix flags set, as a derived run named `<name>-fixed` of the same kind and time limit (it has no
  `expected.toml` entry of its own). It must match with no open finding at all: a new defect that violates the same
  invariant or property as a known one still fails there, and the proposed spec fix is checked before the spec
  changes. A scenario carrying open findings therefore runs longer; Section 12's time budget is for a scenario with
  none.
- `seeded`: TLC runs without `-continue`. Passes only if TLC stops with a violation of exactly the named invariant or
  property; a different violation, or none, fails the run. A seeded run's scenario must have no open finding on the
  same invariant, so that the seed, not the open defect, is what fails it.

TLC's deadlock check stays on. PlusCal's translation already allows the final state in which every process is done,
so a deadlock report means some process is stuck on a condition that can never become true, which is either a model
defect or a real stuck state. To keep that meaning, a crashed process ends at `Done` (a ghost flag records the crash),
and a process that waits for an event that may never happen (a resume that waits for a crash, the `LockLost`
environment action) may also finish without it.

The runner, `run.py`:

- stops with exit code 2 before running anything if Python is older than 3.11, if `expected.toml` is malformed, if a
  run's `scenario` is not in the `scenarios` list, if a listed scenario has no runs, or if `--scenario NAME` names an
  unknown scenario; `--list-scenarios` prints the `scenarios` list as a JSON array;
- downloads `tla2tools.jar` from the TLA+ project's GitHub releases (`github.com/tlaplus/tlaplus/releases`), at a
  release tag and SHA-256 both pinned in `run.py` (exact release chosen in the plan), into `target/tla/`, and verifies
  the hash before every use, including a cached copy; on a mismatch it deletes the file and downloads once more, and
  if the hash still does not match it stops with exit code 2. The pin protects against the download changing later,
  not against a pull request that changes the tag and hash together; such a change is visible in `run.py`'s diff and
  is reviewed as a dependency change;
- runs the selected runs (all, or one scenario with `--scenario NAME`), each under its `timeout_minutes`, killing TLC
  on timeout; a run that fails in any way does not stop the others, so one report covers every selected run;
- runs TLC with `-tool`, whose output marks each message with a numeric message code, and decides each run's result
  from the invariant-violation and property-violation messages and the names they carry; the plan pins those codes
  and TLC's exit statuses against the pinned TLC release and keeps recorded TLC logs as test fixtures for the parser.
  A run is judged only when TLC's output shows that model checking finished (or stopped at a violation) and its exit
  status agrees; a parse or semantic error in a model or configuration, any other error message, or a missing
  completion message is a tooling failure (exit code 2), never a run with no violations;
- prints one line per run (name, expected, observed, states found, duration) and, for every unexpected result, the
  first 60 states of TLC's trace, saving the full TLC output under `target/tla/out/<name>.log`;
- exits 0 when every run matched, 1 when any run's result did not match, and 2 for a tooling failure (Java missing,
  download or checksum failure, TLC crash, out of memory, or timeout), naming the cause. A run that times out is
  reported as `TIMEOUT` with the states explored so far, never as a witness that was not violated, because a partial
  state space cannot show that a path is unreachable. When both happen, a mismatch outranks a tooling failure: the exit
  code is 1 if any run completed with an unexpected result, otherwise 2 if any run hit a tooling failure.

Recipes and CI:

- `just model` runs every run; `just model <scenario>` runs one scenario; `just model-test` runs `run.py`'s unit
  tests (recorded TLC output, no Java); `just model-stamp` runs `just model`
  first and rewrites the unit hashes (Section 9) only if every run matched, counting open findings as matched. None of
  these is part of `just check`, which stays Java-free.
- `.github/workflows/model.yml` runs on every pull request to `main`, on pushes to `main`, and on manual dispatch, with
  `permissions: contents: read` and no secrets, in three jobs:
  1. `plan` checks out the repository with full history (`fetch-depth: 0`), runs `run.py`'s unit tests, lists the
     changed files with `git diff
     --name-only` from the merge base of the pull request's base and head (`base...head`), or from the push's previous
     commit, decides whether they touch
     `models/**`, the spec file (glob `FLUX_FULL_UPDATED_SPEC_V*.md`), `crates/flux-platform/tests/fs_semantics.rs`,
     the `justfile` (its model recipes), or the workflow itself, and outputs `run.py --list-scenarios` as the matrix, or an empty matrix when nothing
     relevant changed. Manual dispatch, and any case where the changed files cannot be determined (a new branch, a
     force push whose previous commit is gone), count as touching, so an unknown change runs every scenario;
  2. `scenario`, one matrix job per scenario name and skipped when the matrix is empty (GitHub rejects an empty
     matrix, so the job carries a condition on `plan`'s output), installs Java with `actions/setup-java` (Temurin 21) and Python
     with `actions/setup-python` (3.11, the oldest version `run.py` supports), runs `just model <name>`, and uploads `target/tla/out/` when it
     fails;
  3. `model-gate` always runs after the others and fails if `plan` failed, if any `scenario` job failed or was
     cancelled, or if `scenario` was skipped although the matrix was not empty; it passes when the matrix was empty.
  Because the matrix comes from `expected.toml`, a run can never belong to a scenario CI does not run.
- The drift-stamp test and the filesystem probes are ordinary Rust tests, so `just check` and the existing
  Linux/macOS/Windows test job run them.
- `.claude/recommended-tools.json` gains entries for Java 21 and Python 3.11 or later, the two non-Rust tools
  `just model` needs.
- The stamp and traceability tests (Section 9) are mechanical: they can be made to pass by rewriting the hashes
  without re-checking the model. Only the TLC runs check the model against the changed text, so the design
  recommends that the owner make `model-gate` a required status check; `model-gate` always reports, so requiring it
  never blocks a pull request that does not touch the model. No mechanism can make a person model a new spec rule
  rather than re-stamp it; what the design does is make the change visible, because the rewritten hashes appear in
  the pull request's diff next to the `trace.toml` entries and labels they cover.

## 5. Filesystem model (`FsModel.tla`)

State:

| Variable | Meaning |
|---|---|
| `entries` | per directory, entry name → file object id; two names are the same entry when `Fold` maps them to the same class. Object ids are never reused; they index `content`, `durable`, `handles`, and `oslock` |
| `content` | object id → `Record(op, kind)` (kind: `operation` or `cleanup`), `Torn`, or `Foreign`: what a reader sees now |
| `durable` | object id → the content that survives a host crash (Section 5.2) |
| `durableEntries` | per directory, the entries that survive a host crash; `entries` differs from it by the entry operations (create, hard link, rename, unlink) made since that directory's last flush |
| `handles` | set of `[proc, obj, shareDelete]` |
| `oslock` | object id → the process holding its OS-native lock, or none |
| `inflight` | set of filesystem calls issued by a process and not yet complete (Section 5.2) |
| `pastObjects` | per directory and name class, the object ids that name has held; used only by weak identity (Section 5.1) |

### 5.1 Operations

Each row is grounded by a filesystem probe (Section 10) where a real platform can confirm it.

| Operation | POSIX | Windows |
|---|---|---|
| exclusive create at a name | fails if an entry of the same class exists; otherwise creates an empty object and a handle | same |
| create a hard link | fails if an entry of the same class exists at the new name; otherwise adds an entry for the new name pointing at the existing object, whose other names stay (Section 16.1) | same |
| open existing without create | fails if absent; otherwise adds a handle (`shareDelete` is recorded as true and never chosen, because POSIX has no delete-sharing) | fails if absent; otherwise adds a handle with the chosen `shareDelete`; a new handle cannot be opened on an object with a pending delete |
| close a handle | removes the handle; releases its OS-native lock if held | same; lifts the sharing restriction that handle imposed, and removes a pending-delete object's name when its last handle closes |
| non-blocking OS-native lock | a lock scoped to the handle that takes it (`flock` or Linux open-file-description locks; never `fcntl` record locks, which the process loses when it closes any handle to the file); fails if another process holds it; unavailable when `LockCapability = weak` | a lock scoped to the handle (`LockFileEx`); fails if another process holds it; unavailable when `LockCapability = weak` |
| release an OS-native lock | clears `oslock` | same |
| rename, no-replace | fails if the target class exists; succeeds even if the object is open and OS-locked | fails if the target class exists; succeeds only if every open handle on the object has `shareDelete` |
| rename, replacing | atomically points the target entry at the source object, whether or not the target existed; the replaced object keeps its open handles and loses its name | same, and succeeds only if every open handle on the source object and on the replaced object has `shareDelete` |
| unlink | succeeds even if the object is open and OS-locked; open handles keep writing to the now-unnamed object | succeeds only if every open handle has `shareDelete`; then either removes the name at once (the POSIX delete semantics recent NTFS uses) or leaves it as a pending delete until the last handle closes, chosen nondeterministically per call |
| write a lock record | two steps: `WriteBegin` sets `content` to `Torn`, `WriteEnd` sets it to the new record; a reader in between sees `Torn` | same |
| flush a file | copies the object's `content` to `durable` | same |
| flush a directory | copies the directory's `entries` to `durableEntries` | same |
| look up a name | reports whether the name has an entry, without opening a handle | same |
| identity of a name | reports an identity value: under `IdentityStrength = strong`, the id of the object the name maps to; under `weak`, either that id or any id in the name's `pastObjects`. Only the reported value can repeat; the objects stay distinct | same |
| list a directory | not atomic: `ListBegin`, then one `ListNext` step per name in the directory's name set, in any order, each reading that name's entry at that moment; an entry present for the whole listing is always returned, one created or removed during it may or may not be | same |

Constants: `Platform ∈ {"posix", "windows"}`; `Fold`, the name-equivalence map (identity, or case folding);
`IdentityStrength ∈ {"strong", "weak"}`, where `weak` stands for FAT32/exFAT-like identity that CI cannot probe;
`LockCapability ∈ {"strong", "weak"}`, where `strong` stands for the spec's `LocalStrong` or `RemoteStrong` and `weak`
for `RemoteUnverified` or `Unsupported` (Section 235.1). Under `weak`, Section 235.1 refuses every operation that needs
target exclusivity with `REMOTE_LOCK_UNSAFE`, so every actor refuses before it acquires anything, and the unavailable
OS-native lock and 240.5 step 1's capability branch are reached only in the seeded run that removes that gate
(Section 8), on POSIX only; `ShareMode`, the `shareDelete` value Windows opens use. The spec does not yet state the
value at each open site (owner lock handle, classification read, recoverer, breaker, cleanup), so this work builds
only `ShareMode = "any"`: every Windows open chooses `shareDelete` nondeterministically and TLC explores every
combination, and the first Windows runs are expected to find which combinations are unsafe (Section 11). The spec fix
that states the values also adds them to the model as a per-site record.

### 5.2 Atomicity, in-flight calls, and crashes

Each row of the operations table is one atomic step, and each PlusCal label performs at most one filesystem
operation. A spec step that makes several filesystem calls (for example open, OS-lock attempt, then read) is
therefore several labels, and other actors can run between them.

A data write or rename that an operation issues as part of publishing is two steps: issue, then complete. Other
actors' steps can run between them, and the call's effect lands at completion. This is how the model represents a
stalled owner whose call "can still complete" (Section 240.5).

There are two kinds of crash.

A process crash (the process dies; the machine keeps running):

- releases its handles, their sharing restrictions, and its OS-native locks;
- leaves `content` as it is, so a record write interrupted between `WriteBegin` and `WriteEnd` stays `Torn` and every
  completed write stays visible, flushed or not;
- lands or drops, nondeterministically, each of its in-flight calls;
- loses its in-memory state; a crashed invocation never continues, and later work is done by other actors.

A host crash (power loss or reboot of the machine all actors run on):

- is a process crash of every process that has started;
- then sets, for every object written since its last flush, both `content` and `durable` to one of: the old durable
  content, the latest content, or `Torn`, chosen nondeterministically per object; a crash during a flush has the same
  outcomes. Objects flushed since their last write keep their content;
- then keeps a prefix, in the order they were made, of the entry operations (create, hard link, rename, unlink) made
  since the last flush that covered them, and loses the rest; each kept operation applies whole (a rename is never
  half applied), and the prefix always includes every operation a directory flush covered. Both `entries` and
  `durableEntries` are set to the result. This is the ordering journaled filesystems give (ext4, APFS, NTFS); a
  filesystem without a journal can lose operations out of order, which the README lists as unverified.

These are the only ways `durable` and `durableEntries` become visible. Processes that had not started when the host
crashed run afterwards as the later invocations. At most two crashes happen in one run, counted across both kinds (one
in `claims`, Section 12); a host crash counts as one crash however many processes it stops.

In `liveness` runs, one actor of each kind a property relies on to make progress (Section 7) never has a process
crash, and a host crash, if one happens, happens before any such actor has started, so those actors run after it as
later invocations; every other actor crashes as in `check` runs. Without that rule a run where every actor able to clear a
lock crashes would violate the property for a reason the spec does not claim to cover (an operator re-running the
command is outside the model), and a host crash that spared a running process would be impossible.

The lock protocol's actors flush only where the spec tells them to (the record write of 240.5 step 6 is flushed); the
spec states no directory flush after creating, moving aside, or removing a lock file, so a host crash may undo any of
those entry operations, and the model explores what the next invocation then finds (Section 11).

## 6. Actors

### 6.1 `LockProtocol.tla`

Each PlusCal label is named `S<section>_<step>` after the spec step it implements, with dots in the section number
written as underscores. Where the spec numbers its steps, `<step>` is `s` and the number: Section 240.5 step 6 is
`S240_5_s6`. Where a heading has no numbered steps (for example 96.1, 97.1, 99, 120, 182, 183, 241.5), `<step>` is a
short name defined in `trace.toml` next to the spec sentence it implements, for example `S97_1_ancestor` or
`S182_prepare`. A step that needs more than one atomic action gets suffixes `a`, `b`, and so on (`S240_5_s6a`).

Encodings: a lock record is a TLA+ record `[op |-> <operation id>, kind |-> "operation" | "cleanup"]`; `Torn`,
`Foreign`, and `Empty` are distinct model values. Each process is a model value, and its operation id is its own
process value, so every value that names an actor (records, ghost variables, handles, in-flight calls) changes
consistently when TLC permutes processes. Symmetry reduction is declared only over interchangeable actors (same kind
and same target), and only in scenarios whose symmetry-free `liveness` runs also check every safety invariant (Section
4): the two Recoverers of `recovery` and the two Breakers of `breaklock`. All other runs use no symmetry; `dirlock` has one actor of
each kind, and `claims` has no liveness run. Every invariant and witness
quantifies over processes rather than naming one.

| Actor | Behaviour |
|---|---|
| Owner | A normal operation: acquires the target lock (96.1: exclusive create of `P/<name>.flux-lock`, then the per-name acquirer's check that `P/.flux-dir.lock` is absent; on a conflict it removes the lock it created, refuses with `TARGET_LOCK_BUSY`, and ends, as 96.1 states, with no retry) and holds its OS-native lock, runs its ancestor check (97.1 a), publishes with a Section 99 check before each write, releases the lock at completion; may crash |
| StalledOwner | An owner that stops making progress at any point and may later resume (its next Section 99 check then runs, and if the check fails it stops, closing its handles), may resume and complete normally, or may have a call in flight that completes later |
| DirOwner | An owner whose target name is too long for a per-name lock: takes `P/.flux-dir.lock` and lists `P` for per-name locks (96.1 announce-then-check); may crash |
| PlainRun | A new invocation without flags: classifies what it finds (21.1 table, 96.1, 120, 240) and acts or refuses; if it acquires the lock it continues as an Owner |
| Recoverer | A new invocation that finds a dead owner's lock and runs 240.3 steps 1-5, then continues as an Owner |
| Breaker | `flux copy --restart --break-lock`: 240.5 steps 1-6, then 21.1 steps 2-5, then continues as an Owner; refuses at 240.5 step 1 when `IdentityStrength = weak` or `LockCapability = weak` (the step requires both, spec lines 10628-10631; under `LockCapability = weak` 235.1 refuses first, so that branch is reached only in the seeded run); may crash, including between the `WriteBegin` and `WriteEnd` of its step 6 overwrite |
| CleanupBreaker | `flux cleanup --target PATH --break-lock`: 240.5 steps 1-6 with a cleanup lock record, deletes artifacts, deletes its lock; may crash |
| Cleanup | `flux cleanup DEST`: classifies and removes orphan, dead, and cleanup locks via 240.3 |
| NestedOwner | An owner whose destination is a child of another owner's destination (97.1) |

A scenario has one actor of each kind it names unless Section 12 gives a count. "Continues as an Owner" means the
actor then runs the Owner's publishing steps under its own operation id, so `SingleWriter` compares it with every
other writer of the target.

Classification of a lock is a sequence of labelled filesystem steps followed by one judgement (Section 5.2): open the
lock file, attempt its OS-native lock without blocking, read the record, then judge by what was read:

| What the steps found | Judgement |
|---|---|
| no entry at the lock path (the open failed because it is absent) | empty |
| `Foreign` | foreign |
| `Torn`, or a read that failed | uncertain (96.1, 259.6), without consulting the oracle |
| a record whose `kind` is `cleanup` | cleanup lock (251.1) |
| a record of an operation, and the OS-native lock attempt failed because another process holds it | live |
| a record of an operation, otherwise | the oracle's judgement of the record's owner, below |

An owner gives up its OS-native lock only by crashing, by completing, or by stopping after a failed Section 99 check
(it closes its handles); none of these removes a lock file whose record is not its own. So while an owner runs, the
fifth row applies to its lock.

In the last row an oracle decides the part of Section 240.2 and 240.3 evidence that is not a filesystem fact (whether
the owner's process or host still exists), consistent with the truth: live only if the owner is alive, dead only if
it has crashed, uncertain in either case. Two classifiers may disagree at different moments, as Section 240 allows.

Because the steps are separate, the lock file can be renamed, unlinked, or rewritten between the open and the read,
and the classifier then judges what its handle reads. A classifier's OS-native lock attempt can succeed; unless the
spec step it is in keeps that handle (240.3 holds the OS lock through the move-aside; 240.5 holds it through the
takeover), the classifier closes the handle, releasing the lock, as its next step after the judgement, so a refusal
never leaves a lock held on another process's file.

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

Besides the filesystem model, `Claims.tla` has one variable for the operation's state database (`state.db` with its
WAL): the set of durable records per target (PREPARE_COMMIT, COMMIT) and the set of claims. A database transaction
is one atomic step and is durable when it completes; a crash of either kind keeps every completed transaction and
none of an incomplete one.

Each target runs the Section 182 pipeline as separate steps: plan with its existing-entry claim (a transaction),
write the temporary file, flush it, PREPARE_COMMIT (a transaction), revalidate locks (Section 99), rename (a
filesystem call, issued then completed, Section 5.2; no-replace for a target planned as new, replacing for one
planned as a replacement, Section 241.5), flush the destination directory, then one transaction that writes COMMIT
and the target's created-entry claim together. Planning uses the look-up operation (Section 5.1) to decide whether a
target is planned as new or as a replacement; a no-replace rename that fails because the name is now taken is reported
as `DESTINATION_NAMESPACE_COLLISION` (241.5) and ends that target. A transaction includes its WAL flush, so it is durable when it
completes. The steps are separate, so a crash can fall between any two; a crash between the rename and COMMIT is the
case Section 183 recovery handles, and a host crash before the directory flush can undo the rename.
Dependents run the Section 16.1 link procedure (a hard link at a partial name, then a rename over the target). An
environment action, `LockLost`, can take the operation's lock over, at most once per run, at any point (a `--break-lock` from outside), after which every worker's next revalidation fails and it stops. A crash
can happen between any two steps; a resume then runs Section 183 recovery before continuing.

The scenario runs one configuration per target group, so that each state space stays small: `A`/`a` (one
configuration whose initial states include both an existing entry `A` and none), `H1`/`H2`, and `D1`/`d1`. In each, two workers take the group's targets from a shared queue
in any order and interleave freely.

## 7. Properties

Safety invariants, which must hold in every reachable state:

| Name | Statement |
|---|---|
| `SingleWriter` | For a target, at most one process is inside a publishing step whose last Section 99 check passed. The one exception the spec accepts, a stalled prior owner's in-flight call issued before an operator `--break-lock` and completing after it, is permitted only in that case. |
| `PlainNeverOwnsUncertain` | A process without `--break-lock` never removes, renames, or overwrites a lock it classified as uncertain (`classified`), and creates a lock only at an empty lock path. |
| `RefusalJustified` | Every `TARGET_LOCK_BUSY` refusal happens while the lock path holds, or the refusing process's open handle refers to, a lock whose owner is alive or another takeover's. |
| `ForeignUntouched` | A `Foreign` object at a lock path is never written, renamed, or deleted. |
| `Classifiable` | In every state, each lock path classifies into exactly one case of the Section 6.1 judgement table: empty, foreign, uncertain, cleanup lock, live, or (by the oracle) dead; a `Torn` record classifies as uncertain. |
| `NestedExclusion` | Two operations whose destinations nest never both write objects under the inner destination. |
| `DirLockExclusion` | A per-name lock holder and a directory-lock holder in the same directory `P` are never both past their 96.1 check. |
| `NoSilentOverwrite` (Claims) | No target overwrites an entry this operation already published. |
| `CommittedHasClaim` (Claims) | Every target with COMMIT has its created-entry claim. |
| `CommittedIsDurable` (Claims) | After any host crash, every target with COMMIT still has its destination entry pointing at the object it published. |
| `NoSelfCollision` (Claims) | A resumed target never collides with its own claim; a hardlink dependent whose name maps to a different entry never collides with its group's claims; a dependent folded onto its canonical's entry is reported as a collision. |
| `NoPublishAfterLockLost` (Claims) | No rename lands after `LockLost` unless it was issued (in flight, Section 5.2) before `LockLost`: the same single exception `SingleWriter` accepts, following 240.5's "a filesystem call it had already started can still complete". A revalidation that passed before `LockLost` does not by itself permit a later rename. |

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
| `NeverDirLockAcquired` | a DirOwner passes its 96.1 check holding the directory lock |
| `NeverDirLockBackoff` | a per-name or directory acquirer backs off on a 96.1 conflict |
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

Each seeded configuration sets one flag that re-introduces a defect the adversarial review fixed, or removes one rule
the spec states (the rows without a review finding), in the scenario that can reach it. `just model` requires each to stop with the listed violation; if one passes, the model has lost the
ability to see that defect and the run fails.

| Flag | Defect re-introduced (review finding) | Scenario | Must fail |
|---|---|---|---|
| `SEED_RECOVERER_IDENTITY_ONLY` | 240.3 step 3 checks identity only, not the record (round 6, IMC-4) | `mixed` | `SingleWriter` |
| `SEED_EMPTY_PATH_BUSY` | 240.5 step 6 refuses when the lock path is empty instead of retrying (round 6, IMC-3) | `breaklock` | `RefusalJustified` |
| `SEED_RESTART_RELEASES` | 21.1 step 5 releases the lock before the new operation starts (round 3) | `breaklock` | `SingleWriter` |
| `SEED_MOVE_ASIDE_FOR_UNCERTAIN` | an uncertain owner's lock is moved aside, leaving the path empty (round 5) | `breaklock` | `PlainNeverOwnsUncertain` |
| `SEED_RENAME_OVER_TAKEOVER` | takeover by renaming a new record over the lock (round 3) | `breaklock` | `SingleWriter` |
| `SEED_NO_IDENTITY_RECHECK` | 240.5 step 6 skips its identity check after the write (round 6) | `breaklock` | `SingleWriter` |
| `SEED_NO_CAPABILITY_GATE` | neither Section 235.1's refusal nor 240.5 step 1's capability check is applied, so operations and takeovers run with no OS-native lock and two Breakers are no longer serialized by it | `breaklock`, weak-capability variant | `SingleWriter` |
| `SEED_CLEANUP_LOCK_UNVERIFIABLE` | Section 120 rejects `workspace_path = none` (round 6) | `cleanup-crash` | `DeadLockEventuallyCleared` |
| `SEED_TORN_AS_FOREIGN` | a checksum-failing record is treated as a foreign object (round 5); `mixed` has both a record torn by the Owner's crash and a Breaker, the only actor that can clear it | `mixed` | `UncertainLockEventuallyCleared` |
| `SEED_NO_ANCESTOR_CHECK` | 97.1 (a) skipped | `nested` | `NestedExclusion` |
| `SEED_DESCENDANT_EXISTING_ONLY` | 97.1 (b) checks only directories that already exist (round 2) | `nested` | `NestedExclusion` |
| `SEED_CHECK_BEFORE_ANNOUNCE` | the directory acquirer lists `P` before creating `P/.flux-dir.lock` (96.1's announce-then-check order reversed) | `dirlock` | `DirLockExclusion` |
| `SEED_PERNAME_SKIPS_DIR_CHECK` | a per-name acquirer does not check that `P/.flux-dir.lock` is absent | `dirlock` | `DirLockExclusion` |
| `SEED_CLAIM_AFTER_COMMIT` | created-entry claim written after COMMIT (round 6, CPE-4) | `claims`, group `A`/`a` | `CommittedHasClaim` |
| `SEED_CLAIM_BY_OBJECT_ID` | claims keyed by object identity (round 3) | `claims`, group `H1`/`H2` | `NoSelfCollision` |
| `SEED_NO_PUBLICATION_CLAIM` | publications do not claim the entry they create (round 2) | `claims`, group `A`/`a` | `NoSilentOverwrite` |
| `SEED_NO_REVALIDATE_BEFORE_RENAME` | Section 182 renames without revalidating locks | `claims`, group `A`/`a` | `NoPublishAfterLockLost` |

A seeded run whose "must fail" entry is a liveness property is still a `seeded` run (its `violated` names the property),
checked with the liveness settings: TLC checks the scenario's temporal properties, run
without symmetry. Every later fix that the model drives adds a row here.

## 9. Spec-drift stamp

`spec-sections.stamp` begins with one line `spec: <path>` naming the spec file relative to the repository root; this
is the only place the model tooling names it, and the test fails if that file does not exist, so renaming the spec
(a V17) fails `just check` until the stamp names the new file. Then it lists, one per line, each spec heading the
model encodes, exactly as the heading line appears in the spec: 96.1, 96.2, 97.1, 99, 99.1, 21.1, 240.1, 240.2,
240.3, 240.4, 240.5, 251.1, 251.2, 259.6, 120, 235.1, 241.5, 182, 183 at the end of the work; a heading is added to
the stamp, with its `trace.toml` entries, when the model starts to encode it. A heading's own text runs from its heading line up
to, not including, the next heading line of any level, so a subsection the model does not encode is not part of its
parent's text; a subsection the model does encode is listed as its own line. Heading lines are ATX headings (`#` to
`######`) outside fenced code blocks; line endings are normalised to LF before hashing.

The hashes live in `trace.toml`, one per unit (Section 9.1): the BLAKE3 hash, in lowercase hex, of the unit's text.
`tests/model_stamp.rs`, an auto-discovered test target of the root package (so `just check` runs it), recomputes
them. It fails if a listed heading is missing or appears more than once, and, for each unit whose hash differs, names
the heading, the unit's ordinal, and the labels that `trace.toml` says implement it, so a spec change points at the
exact model steps to re-check. `blake3` is added to the root package's `[dev-dependencies]` from the workspace, and
`toml` (added to `[workspace.dependencies]`, subject to `deny.toml`) for reading `trace.toml`. The same file holds an
`#[ignore]`d test that rewrites the unit hashes; `just model-stamp` runs it after a green `just model` (Section 4).
The workflow after a spec change: re-check each unit the test names against the model, update the model and
`trace.toml`, run `just model`, then run `just model-stamp`.

### 9.1 Traceability check

`trace.toml` is the single traceability map. Its unit is a piece of a stamped heading's own text (Section 9), and every
text line after the heading line belongs to exactly one unit: each block of lines separated by blank lines or by
fence-marker lines (which belong to no unit) is a unit, except that each numbered step, indented or not, starts a new
unit, and a numbered step together with the indented lines after it is one unit even across blank lines. The numbered
steps of 21.1 and 240.1 sit inside fences, and each is a unit. Prose before, between, or after numbered steps is
therefore covered like any other text. A unit's text, which is what is hashed, is its lines joined by LF. Each entry names its heading and the unit's ordinal, quotes
a sentence from that unit, carries the unit's hash (Section 9), and gives either the labels that implement it or
`not_modelled = "<reason>"`. The same test file checks, without Java:

- every unit of every stamped heading has exactly one entry, each quoted sentence occurs in its unit, and each hash
  matches its unit;
- every label in `LockProtocol.tla` and `Claims.tla` (matched by `S\d+(_\d+)*_\w+:`) appears in `trace.toml`;
- every label `trace.toml` names exists in a model file.

Because units are counted from the spec text, an entry cannot be left out, and a spec edit that adds a unit fails the
check until the map covers it. The README renders the map for readers but is not the source of truth.

## 10. Filesystem probes

Rust tests in `crates/flux-platform/tests/fs_semantics.rs`, run on Linux, macOS, and Windows by the existing test
job. They use `rustix` on Unix (`flock` for the OS-native lock, `fstat`, `renameat2`/`renamex_np` for no-replace
renames) and
`windows-sys` on Windows (`CreateFileW` share flags, `LockFileEx`, `GetFileInformationByHandle`, `DeleteFileW`, and
`MoveFileExW` with and without `MOVEFILE_REPLACE_EXISTING`) as `flux-platform` dev-dependencies, subject to `deny.toml`; no standard
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
| FS-6 | renaming or deleting a file open without delete-sharing fails, and so does a replacing rename onto a file open without delete-sharing | Windows |
| FS-7 | renaming or deleting a file open with delete-sharing succeeds, and so does a replacing rename onto one; whether the deleted name disappears at once or stays pending until the handle closes (the probe prints which; both are allowed by the model) | Windows |

The replacing renames of FS-6 and FS-7 use `std::fs::rename`, which on current Rust performs a POSIX-semantics rename
on Windows; that is the behaviour the model's Windows row describes. Measured on Windows 11 NTFS while plan 1 was
written, `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` refuses any open target even with delete-sharing, so FS-7 prints
what it does. The spec does not name the Windows API for a replacing rename; plan 2 records this as a finding.
| FS-8 | a name replaced by a new file reports a different file identity | all |
| FS-9 | closing a handle releases its OS-native lock | all |
| FS-10 | a directory listing returns every entry that exists for the whole listing, including one created just before it starts | all |
| FS-11 | the OS-native lock is scoped to its handle: a process that holds it, opens a second handle to the same file, and closes that second handle still holds the lock | all |

Each probe prints the filesystem type of its scratch directory (`statfs` on Unix, `GetVolumeInformationW` on Windows),
or `unknown` if that call fails; the call's failure never fails the probe, and `unknown` counts as not native for
triage. A failing probe is never weakened to pass. It is triaged first as environment or platform: if the filesystem type is
not the platform's usual local one (for example an overlay or network filesystem on a runner), the failure is
reproduced on a native local filesystem of that platform before anything changes; if it reproduces, the model's
assumption for that platform is wrong and the model is fixed.
Some model rules no probe can confirm, and the README lists them as unverified assumptions: `IdentityStrength =
weak` and the case-folding behaviour of FAT32/exFAT (no such filesystem on CI runners), including that, having no
journal, they may lose entry operations out of order after a host crash; both crash rules of Section
5.2, including which unflushed writes and entry operations survive a host crash (no probe can cut power; the rules
follow the platforms' documented guarantees, spec Sections 166 to 168); and whether an entry created or removed
during a listing is returned (the model allows either, which covers both).

## 11. Handling findings

A counterexample in a `check` or `liveness` run is saved with its trace and triaged:

- Model defect: fixed in the model.
- Spec defect: recorded in `TODO.md` or `.clavity/local-anomalies.md` and listed in the failing runs'
  `open_findings` (Section 4), so the gates stay green for every other change while it is open; then fixed in the
  spec on the spec branch. A design fork goes through an agy-first consult and then to the owner. The fix removes the
  `open_findings` entries in the same change, gets a new seeded configuration (Section 8), and the unit hashes are
  updated.

Findings the design already expects, each to be confirmed or refuted by the first runs:

- Windows sharing mode: with `ShareMode = "any"`, some combination of open sites without delete-sharing should break
  an invariant, which forces the spec to state the mode at each open site.
- The check-to-call window: Section 99 requires revalidation and rename "without an intervening operation-state
  transition that would invalidate ownership", while Section 240.5 accepts only a call "already started" when a
  takeover lands. A takeover between a passing check and the issue of the call is covered by neither, and no
  implementation can close that window, so `SingleWriter` and `NoPublishAfterLockLost` should report it; the fix
  is a spec statement of the accepted window.
- The directory acquirer lists `P` "for *.flux-lock held by other operations" (96.1), without saying whether a dead or
  uncertain owner's per-name lock counts as held. The model classifies each listed lock as Section 240 does and treats
  live and uncertain as held and dead as not, as 97.1 (a) does for ancestor locks; `trace.toml` records this reading
  as an assumption until the spec states it.
- A `--break-lock` takeover of a per-name lock (Section 240.5) never checks `P/.flux-dir.lock`, which Section 240.5
  does not mention. If a directory acquirer judged that per-name lock dead and proceeded, a Breaker that judges the
  same lock uncertain and takes it over in place would hold it alongside the directory lock, so the `dirlock`
  scenario includes a Breaker and `DirLockExclusion` should report it.
- The spec never flushes the directory after creating, moving aside, or removing a lock file (Section 5.2). A host
  crash can therefore undo a lock's creation, leaving an operation recorded in `state.db` whose lock is gone, or undo
  a move-aside, bringing a replaced lock back; the model explores what the next invocation does in each case.
- Section 96.1 allows "a byte-range lock or `flock`" as the OS-native lock. On POSIX a byte-range lock taken with
  `fcntl` is lost when the process closes any handle to the file, which the model does not allow (Section 5.1, FS-11);
  the expected fix is a spec statement that the lock is scoped to its handle (`flock`, open-file-description locks,
  or `LockFileEx`).

## 12. Scenarios, bounds, and time budget

Each configuration is one scenario on one platform variant. A scenario names the actors that run together; keeping
them apart keeps each state space small. Besides the protocol scenarios below, `expected.toml` has a `selftest`
scenario: a small model (`Smoke.tla`) with a check run carrying a witness and an open finding, a liveness run, and a
seeded run, which exercises every way `run.py` judges a run in a few seconds. It lands with plan 1, before any
protocol model exists, and stays as the runner's own regression check.

| Scenario | Actors | Variants | Witnesses its `check` run expects |
|---|---|---|---|
| `recovery` | Owner (crashes), 2 Recoverers, PlainRun, Cleanup | POSIX, Windows, POSIX weak-identity | `NeverAcquired`, `NeverRecovered`, `NeverRecoveredAfterCrash`, `NeverCleanedUp`, `NeverTornRead` |
| `breaklock` | StalledOwner, 2 Breakers, PlainRun | POSIX, Windows; POSIX weak-capability (seeded run only: with `LockCapability = weak` every actor refuses under 235.1, so the witnesses cannot be reached there) | `NeverAcquired`, `NeverBrokeLock`, `NeverInFlightAfterTakeover`, `NeverTornRead` |
| `mixed` | Owner (crashes; its lock is dead), Breaker (may see it uncertain), Recoverer (may see it dead), PlainRun | POSIX, Windows, POSIX weak-identity | `NeverRecovered`, `NeverBrokeLock`; the weak-identity variant expects only `NeverRecovered`, because the Breaker refuses at 240.5 step 1 there |
| `cleanup` | StalledOwner, CleanupBreaker, Breaker | POSIX, Windows | `NeverBrokeLock`, `NeverInFlightAfterTakeover` |
| `cleanup-crash` | CleanupBreaker (crashes), PlainRun, Recoverer, Cleanup | POSIX, Windows | `NeverClassifiedCleanupLock`, `NeverCleanedUp` |
| `nested` | Owner on the parent destination, NestedOwner on the child, PlainRun | POSIX | `NeverAcquired` |
| `dirlock` | an Owner with a per-name lock in directory `P` (may crash), DirOwner on `P` (may crash), Recoverer, Breaker; one per-name Owner suffices for every `dirlock` witness and seed, and keeps the state space within budget | POSIX, Windows | `NeverAcquired`, `NeverDirLockAcquired`, `NeverDirLockBackoff`, `NeverBrokeLock` |
| `claims` | Claims model, 2 workers, `LockLost` at most once, one crash and resume; one configuration per target group (Section 6.2) | POSIX with case folding (as APFS by default), Windows | `NeverCommittedWithClaim`, `NeverLockLostMidCommit` |

Every variant of a scenario expects the witness set in its row, except where the row says otherwise; a variant that
cannot reach a witness is a change to this table with its reason, never an edit to `expected.toml` alone.

Liveness runs: `DeadLockEventuallyCleared` in `recovery` and `cleanup-crash`; `UncertainLockEventuallyCleared` in
`breaklock`, `cleanup`, and `mixed` (so that `SEED_TORN_AS_FOREIGN` is judged against a passing run of the same
scenario); POSIX variant only, without symmetry.

Common bounds: one target lock path (plus the parent's for `nested`, and the per-name lock path and the directory
lock path for `dirlock`); at most two crashes per run in total, and one in `claims`; `LockLost` at most once; symmetry
only as Section 6.1 states (`recovery` and `breaklock`, in `check` and `seeded` runs). These bounds come from
order-of-magnitude estimates made during review (`dirlock` and the `claims` group `A`/`a` were the two at risk), not
from measured runs; the first runs measure them. Each TLC run's `timeout_minutes` is
10; each CI matrix job (one scenario) should finish within about 20 minutes. A run that does not fit gets tighter
bounds, and the tighter bounds are written into the README, never raised silently.

## 13. Success criteria

A spec defect the model finds is carried as an `open_finding` (Sections 4 and 11) until the spec is fixed; criteria 1
to 7 hold with open findings counted as expected results, and the work is finished when no open finding remains.

1. Every seeded run stops with its named violation.
2. Every `check` run reports exactly its listed witnesses and its open findings violated, and no other safety
   invariant; every fix-flag run reports exactly its witnesses.
3. Every `liveness` run passes.
4. Every run finishes within its time limit on CI.
5. The traceability check (Section 9.1) passes: every label maps to a spec step, and every spec step of the stamped
   headings maps to a label or to a `not_modelled` reason.
6. The drift-stamp test, the traceability check, and FS-1 to FS-11 pass in the existing test job.
7. `model-gate` passes on the pull request that adds the model.

## 14. Out of scope

- Concurrency testing of the Rust implementation (loom, shuttle, or madsim), once lock code exists; recorded in
  `TODO.md`.
- Filesystems beyond the modelled variants (SMB, NFS), and clock behaviour beyond the liveness oracle: listed in the
  README as unverified assumptions.
- The content of refusal reports (96.2) and exit codes: the model checks which outcome happens, not its wording.
- Configuring branch protection: the design recommends requiring `model-gate` (Section 4); the owner sets it.
