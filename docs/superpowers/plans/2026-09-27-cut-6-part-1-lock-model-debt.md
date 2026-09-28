# Cut 6 Part 1: the lock-model debt - measurements and the model's net

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close cut 6's items whose code does not depend on an unmeasured TLC output shape, and take the measurement that Part 2 (the branch-granular union and its entries) is written against.

**Architecture:** Every change is to the TLA+ model's checking net in `models/lockproto/` - seeds, invariants, witnesses, runs - plus one new coverage fixture under `testdata/`. No protocol behaviour changes. `LockProtocol.tla` is GENERATED from `LockProtocol.head` + `algorithm.txt` + `invariants.txt`, so every source change regenerates it.

**Tech Stack:** TLA+/PlusCal (TLC 2.19, `tla2tools.jar`), Python 3.11+ (`run.py`, `unittest`), Java 21, `just`.

**Design authority:** `docs/superpowers/specs/2026-09-26-cut-6-lock-model-debt-design.md` (owner-approved; adversarial panel GREEN at round 3, `0c995db`). Every citation below was read at `f3bd5fa`. This is revision 2, after the plan's own panel round 1 (an independent reviewer; 14 findings, all folded) and one AGY-FIRST consult (item 12's split, aligned).

---

## Why a Part 1 and a Part 2

The spec's union-based closures (items 1, 2, 3, 6) rest on what TLC prints for arms inside actions - never recorded in this repository. Writing their code now would mean inventing that output. So:

- **Part 1 (this plan):** the arm-coverage MEASUREMENT (Task 1), the constant migration, and every item whose closure is a seed, a state predicate, a witness, a negative control or a comment - items 4, 5, 6 (control, `FsInvariants`, past seed), 7, 8, 9, 10, 11, 12, 13 - plus the bookkeeping.
- **Part 2 (written after Task 1 lands, against its recorded output):** `parse_cost_nodes`' full span and action grouping, the `branches` schema and resolution, the two tiers' union judging (including the new extended-tier CI job), and the `branches` entries for items 2, 3 and 6 (the latter's two `FsHostCrash` arms). If Task 1's measurement triggers one of the spec's stated consequences, the affected item returns to the owner before Part 2 is written.

## Where this plan refines the spec (declared, not silent)

1. **The arm fixture is a NEW module (`ArmFixture.tla` + `ArmOps.tla`), not an extension of `CoverageFixture.tla`.** `coverage.out` is recorded from `CoverageFixture` and pinned by the existing label-coverage tests; changing that module re-records it and churns unrelated tests.
2. **The negative control's guard is a DURABLE-STATE predicate, not a `pc` predicate** (constant `HostCrashAtDurableOnly`, replacing the spec's name `HostCrashAtStartOnly`). Whether PlusCal accepts `pc` in an algorithm body was never verified. The guard - nothing unflushed and every directory's entries equal to its durable entries - is what the spec's argument needs, and it is not `live`-based (the spec's stated trap). Why the correct model holds it: the ghost compares the lock over `LandUnlink(fs, lander, c)` (`algorithm.txt:1023`), and a landing unlink needs a holder with a pending release, which needs an earlier create; that create made `entries` differ from `dentries`, and nothing flushes a directory, so the guard is already false by then. In practice the control's host crashes fall before any create or write.
3. **Item 11 needs no "measure first".** A Breaker enters `Recover` only through `brk_recover` (`algorithm.txt:908`, `:912`); unseeded, only when `Replaceable` holds, which implies `JudgedUncertain` is false (`:98`, `:101-106`); under `SEED_MOVE_ASIDE_FOR_UNCERTAIN` it is set, and `breaklock-posix-seeded-SEED_MOVE_ASIDE_FOR_UNCERTAIN` (Breakers only, `expected.toml:419-425`) already expects `PlainNeverOwnsUncertain`. The invariant covers Breakers; only its comment is wrong.
4. **Item 12 splits `ForeignUntouched`** (owner, agy aligned, after the plan's panel round 1). With one invariant, the seeded takeover breaks its content half and the Breaker then goes on to release the lock, whose unlink breaks its identity half on the same trace; the runner records only the invariant's NAME, so gutting the content half left the seeded run green. Split into `ForeignStaysAtLockPath` and `ForeignContentUntouched`, the gutted run reports the OTHER name - a kill. Checking both halves side by side is equivalent to checking their conjunction, so no existing run loses coverage.
5. **Seeds are one-shot and never touch the foreign start object.** Every run may start from a `Foreign` lock (`algorithm.txt:18`); a seed that rewrites `foreignObj` also breaks `ForeignStaysAtLockPath`/`ForeignContentUntouched`, and one that points the lock path at a `NoContent` object sends a classifier into `seen.op` (`algorithm.txt:159`), an evaluation error. And a seed enabled in every state turns a gutted invariant's run into an unbounded exploration. So each new environment seed guards itself to fire once, excludes `foreignObj`, and the entries seed writes `DirLockName`'s class, whose only reader diverts to the backoff label (`algorithm.txt:205`).
6. **Mutant runs are TARGETED TLC runs, not scenario runs.** A scenario run executes every run of the scenario (breaklock POSIX includes a 17-million-state check, `algorithm.txt:558`); the plan runs the ONE config a mutant is about, directly with TLC, and leaves full-scenario measurement to CI (Task 14).

## Ground rules for every task

- **Worktree:** `E:\Rust\flux-engine`, branch `spec/seq-after-cut5`. NEVER push; publishing is the owner's decision after the capstone and test audit.
- **Step 0 - state verification.** Confirm each quoted "current" text before editing. If it differs, STOP and report `STATE_MISMATCH: <file>: <what differs>`. Do not adapt.
- **Shape-divergence stop.** If a change needs a name, constant, invariant, run name, `expected.toml` key or value different from this plan, STOP and report `[plan] -> [yours] because <reason>`.
- **On any STOP:** do not start the next task; leave the working tree as it is; report.
- **Regenerate after every edit** of `LockProtocol.head`, `algorithm.txt` or `invariants.txt`, from `models/lockproto` in Git Bash:
  ```bash
  cat LockProtocol.head algorithm.txt invariants.txt > LockProtocol.tla && \
  java -cp ../../target/tla/tla2tools.jar pcal.trans LockProtocol.tla && rm -f LockProtocol.cfg LockProtocol.old
  ```
  then `python run.py --check-translation` must exit 0 (it prints `run.py: LockProtocol.tla matches its sources`). (If `target/tla/tla2tools.jar` is missing, `python run.py --check-translation` fetches it.)
- **A targeted TLC run** (from `models/lockproto`; always with a scratch `-metadir`, so no `states/` directory lands in the model):
  ```bash
  java -XX:+UseParallelGC -cp ../../target/tla/tla2tools.jar tlc2.TLC -workers auto \
       -metadir ../../.clavity/scratch/cut6/states -config configs/<run>.cfg LockProtocol \
       > ../../.clavity/scratch/cut6/<run>.log 2>&1
  grep -E "is violated|Temporal properties were violated|No error has been found|Error:|distinct states found" \
       ../../.clavity/scratch/cut6/<run>.log
  ```
  The full output, trace included, stays in `.clavity/scratch/cut6/<run>.log` (gitignored); a step that asks for a
  trace quotes it from there.
  Never add `-continue` for a seeded or witness config. A run that has not finished after 60 minutes: stop it and STOP the task.
- **What counts as a kill.** For a seeded or witness config, the mutant is killed when TLC no longer reports the run's expected invariant (or property) as violated - it reports a DIFFERENT one, or `No error has been found`. For a check config, it is killed when TLC reports the named invariant violated. A TLC evaluation error, a parse or translation failure, or a timeout is NOT a kill: STOP and report the message. Record mutant, config, expected report and the observed lines. Revert every mutant, regenerate, and confirm `git diff` shows only the task's intended change.
- **Gates per task:** `python models/lockproto/run.py --check-translation` (exit 0), `python models/lockproto/run.py --list-jobs` (exit 0: loading validates every config against its `constants`, `run.py:859-868`), `just model-test` (`OK`), and the task's named runs.
- **Do not time anything.** Runs are for outcomes and state counts, never durations.
- **Commits** add named paths only - never `git add models/lockproto` - and before committing `git status --short` must show no `*.old` file and no `states/` directory. Write files with the Write/Edit tools; commit messages may use a heredoc. Commits end with:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
- **Copying an `expected.toml` block** means copying the WHOLE `[[run]]` block of the named template - every key, `module` included - and changing only the keys the step lists. A seeded run never carries `unreached` (`run.py:775`); copy `symmetry` only if the template has it.

## File map

| File | Change |
|---|---|
| `models/lockproto/testdata/ArmFixture.tla`, `ArmOps.tla`, `ArmFixture.cfg`, `arm_coverage.out` | NEW measurement fixture (Task 1) |
| `models/lockproto/testdata/record_fixtures.py` | one `COVERAGE_CASES` entry (Task 1) |
| `docs/superpowers/plans/2026-09-27-cut-6-arm-coverage-measurement.md` | NEW findings, Part 2's input (Task 1) |
| `models/lockproto/LockProtocol.head` | ten new constants (Task 2) |
| `models/lockproto/algorithm.txt`, `invariants.txt` | Tasks 3-12 |
| `models/lockproto/LockProtocol.tla` | regenerated each time |
| `models/lockproto/configs/*.cfg` | constant migration (Task 2); new, retired and renamed-invariant configs (Tasks 4-12) |
| `models/lockproto/expected.toml`, `expected-extended.toml` | same |
| `TODO.md`, `models/lockproto/README.md`, `docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md` | Tasks 13 and 15 |

---

### Task 1: measure TLC's coverage of arms (the fixture Part 2 is written against)

**Files:** Create `models/lockproto/testdata/ArmFixture.tla`, `ArmOps.tla`, `ArmFixture.cfg`, `arm_coverage.out`, `docs/superpowers/plans/2026-09-27-cut-6-arm-coverage-measurement.md`; modify `models/lockproto/testdata/record_fixtures.py`.

- [ ] **Step 0: verify.** `record_fixtures.py` has `COVERAGE_CASES = {` with exactly the entries `"coverage"`, `"smoke_check_coverage"`, `"smoke_seeded_coverage"`, and `main()` records each by name.

- [ ] **Step 1: write `testdata/ArmOps.tla`:**

```tla
---- MODULE ArmOps ----
\* Coverage-shape fixture for cut 6 (design 2026-09-26-cut-6, item 1): an operator in a SECOND module
\* with a LET whose CASE has a dead arm and a constant-level body, like FsModel.tla's FsHostCrash.
EXTENDS Naturals

Dead == [tag |-> "dead"]            \* a constant-level CASE body, like FsModel.tla's Torn

Pick(v, which) ==
    LET out(x) ==
            CASE which = "old" -> x
              [] which = "new" -> x + 1
              [] which = "never" -> 1000
              [] OTHER -> IF Dead.tag = "dead" THEN 7 ELSE 8
    IN out(v)
====
```

- [ ] **Step 2: write `testdata/ArmFixture.tla`** (the translator appends the rest):

```tla
---- MODULE ArmFixture ----
\* Coverage-shape fixture for cut 6 (design 2026-09-26-cut-6, item 1). Not part of the lock-protocol
\* model; testdata/record_fixtures.py records its TLC output as arm_coverage.out.
EXTENDS Naturals, ArmOps

(* --algorithm ArmFixture {
  variables
    x = 0, y = 0, z = 0;

  process (p = "p")
  {
    branchy:
      with (i \in {0, 1, 2}) {
        if (i = 0) { x := 1; }
        else if (i = 1) { x := 2; }
        else if (i = 7) { x := 99; }
        else { x := 3; };
      };
    twins:
      with (j \in {0, 1}) {
        if (j = 0) { y := 5; }
        else if (j # 0) { y := 5; };
      };
    two_sites:
      with (w \in {"old", "new", "other"}) {
        z := Pick(x, w) + Pick(y, "old");
      };
  }
}
*)
====
```

- [ ] **Step 3: translate** from `models/lockproto/testdata`: `java -cp ../../../target/tla/tla2tools.jar pcal.trans ArmFixture.tla && rm -f ArmFixture.cfg ArmFixture.old`. Expected: exit 0 and a `\* BEGIN TRANSLATION` block appended to `ArmFixture.tla`.

- [ ] **Step 4: write `testdata/ArmFixture.cfg`:**

```
SPECIFICATION Spec
CHECK_DEADLOCK FALSE
```

- [ ] **Step 5: register the case.** In `record_fixtures.py` add to `COVERAGE_CASES`, after the `"smoke_seeded_coverage"` entry:

```python
    # Cut 6, item 1: arms inside actions, a second module's LET/CASE, one operator at two call sites.
    "arm_coverage": ("ArmFixture", HERE / "ArmFixture.cfg", HERE, True),
```

  and in `main()`, before `build_two_block_fixture(...)`:

```python
    record_coverage_case(jar, "arm_coverage", *COVERAGE_CASES["arm_coverage"])
```

- [ ] **Step 6: record only this case** from `models/lockproto`:

```bash
python -c "import sys; sys.path.insert(0, 'testdata'); import record_fixtures as r, run; r.record_coverage_case(run.ensure_jar(), 'arm_coverage', *r.COVERAGE_CASES['arm_coverage'])"
```

  Expected: `arm_coverage: exit 0`; `git status --short models/lockproto/testdata` shows ONLY the new files and `record_fixtures.py` (no existing `.out` modified, no `.old`, no `states`).

- [ ] **Step 7: answer, with the quoted 2221 lines of `arm_coverage.out` as evidence**, in `docs/superpowers/plans/2026-09-27-cut-6-arm-coverage-measurement.md`:
  1. Does each arm of `branchy` get its OWN node, with the dead arm (`i = 7`, body `x := 99`) at 0 and the live arms > 0?
  2. Are an arm's GUARD and BODY separate nodes, and which is 0 for the dead arm?
  3. Under `with (i \in ...)`: are counts per value of `i`, or per action?
  4. `twins`: both bodies yield the same successor. Does each body node count > 0?
  5. `Pick`'s `CASE` in module `ArmOps`: does each arm body get a node, is `"never"`'s 0, and does the `OTHER` body (built from the constant `Dead`) get a node?
  6. `two_sites` calls `Pick` twice: are `ArmOps`'s nodes printed once per call site, or once in total?
  Then, for each consequence the spec states in item 1 ("Each outcome has a consequence"), say whether it is triggered, and so which of items 2, 3 and 6 go to Part 2 and which return to the owner.
- [ ] **Step 8:** `just model-test` → `OK`.
- [ ] **Step 9: commit** the six paths from "Files", message:

```
test(model): record TLC's coverage of arms inside actions (cut 6, item 1)

A new fixture, ArmFixture/ArmOps: a dead if-arm, two identical-bodied arms, a second
module's LET/CASE with a dead arm and a constant body, and one operator at two call
sites. The findings note answers the spec's six questions from the recorded 2221 lines;
Part 2 is written against it.
```

- [ ] **Step 10: STOP and report the findings.** The driver decides, with the owner where the spec says so, what goes to Part 2. Tasks 2-13 do not depend on the answer.

---

### Task 2: declare the new constants everywhere (the scripted migration)

**Files:** `models/lockproto/LockProtocol.head`, the 81 LockProtocol configs in `configs/`, `expected.toml`, `expected-extended.toml`; regenerate `LockProtocol.tla`.

The ten constants, `FALSE` in every existing run: `SEED_RECOVER_LIVE`, `SEED_FS_PAST_UNALLOCATED`, `SEED_FS_DOUBLE_HANDLE`, `SEED_FS_ENTRY_UNALLOCATED`, `SEED_FS_ALIEN_RECORD`, `SEED_RECOVERER_GIVES_UP`, `SEED_RELEASE_CHECK_REFUSES_UNTOUCHED`, `SEED_RESTART_CHECK_REFUSES_UNTOUCHED`, `SEED_TAKEOVER_FOREIGN`, `HostCrashAtDurableOnly`.

- [ ] **Step 0: verify.** `LockProtocol.head` has the line `    SEED_CHECK_REFUSES_UNTOUCHED, \* a Section 99 check that refuses a holder nobody dispossessed` followed by `    FIX_REMOTE_LEASE_SPEC  \* fix flag ...`. `rg -l '^    SEED_CHECK_REFUSES_UNTOUCHED = ' configs | wc -l` → 81. `rg -c 'SEED_CHECK_REFUSES_UNTOUCHED = (true|false) \}' expected.toml expected-extended.toml` → 72 and 9.

- [ ] **Step 1: header.** Replace the `SEED_CHECK_REFUSES_UNTOUCHED` line with:

```tla
    SEED_CHECK_REFUSES_UNTOUCHED, \* a Section 99 check that refuses a holder nobody dispossessed
    \* Cut 6 (design 2026-09-26-cut-6). One seed per conjunct or site; FALSE outside their own run.
    SEED_RECOVER_LIVE,                     \* 240.3 replaces a live owner's lock (item 5)
    SEED_FS_PAST_UNALLOCATED,              \* a name's past holds an id never allocated (items 6, 7)
    SEED_FS_DOUBLE_HANDLE,                 \* two handle records for one process and object (item 7)
    SEED_FS_ENTRY_UNALLOCATED,             \* an entry names an id never allocated (item 7)
    SEED_FS_ALIEN_RECORD,                  \* a record-shaped content outside Records (item 8)
    SEED_RECOVERER_GIVES_UP,               \* a Recoverer that judged a lock dead does nothing (item 9)
    SEED_RELEASE_CHECK_REFUSES_UNTOUCHED,  \* S99_release_check refuses an undispossessed holder (item 10)
    SEED_RESTART_CHECK_REFUSES_UNTOUCHED,  \* S21_1_s3 refuses an undispossessed holder (item 10)
    SEED_TAKEOVER_FOREIGN,                 \* --break-lock takes over a foreign object (item 12)
    HostCrashAtDurableOnly,                \* the host may crash only when nothing is unflushed (item 6)
```

- [ ] **Step 2: the one-off script** `.clavity/scratch/cut6/migrate_constants.py` (gitignored, not committed):

```python
"""Cut 6 Task 2: add the ten new constants as FALSE to every LockProtocol config and constants line."""
from pathlib import Path
import re, sys

BASE = Path(sys.argv[1])  # models/lockproto
NEW = ["SEED_RECOVER_LIVE", "SEED_FS_PAST_UNALLOCATED", "SEED_FS_DOUBLE_HANDLE", "SEED_FS_ENTRY_UNALLOCATED",
       "SEED_FS_ALIEN_RECORD", "SEED_RECOVERER_GIVES_UP", "SEED_RELEASE_CHECK_REFUSES_UNTOUCHED",
       "SEED_RESTART_CHECK_REFUSES_UNTOUCHED", "SEED_TAKEOVER_FOREIGN", "HostCrashAtDurableOnly"]
ANCHOR = re.compile(r"^(    SEED_CHECK_REFUSES_UNTOUCHED = (?:TRUE|FALSE))(\r?\n)", re.M)
cfgs = 0
for cfg in sorted((BASE / "configs").glob("*.cfg")):
    text = cfg.read_bytes().decode("utf-8")
    new, n = ANCHOR.subn(lambda m: m.group(1) + m.group(2) + "".join(f"    {c} = FALSE{m.group(2)}" for c in NEW), text)
    if n == 0:
        continue
    assert n == 1, cfg
    cfg.write_bytes(new.encode("utf-8"))
    cfgs += 1
tail = re.compile(r"(SEED_CHECK_REFUSES_UNTOUCHED = (?:true|false))( \})")
lines = 0
for name in ("expected.toml", "expected-extended.toml"):
    path = BASE / name
    text = path.read_bytes().decode("utf-8")
    new, n = tail.subn(lambda m: m.group(1) + "".join(f", {c} = false" for c in NEW) + m.group(2), text)
    path.write_bytes(new.encode("utf-8"))
    lines += n
print(f"configs={cfgs} constants_lines={lines}")
```

  From the repo root: `python .clavity/scratch/cut6/migrate_constants.py models/lockproto` → `configs=81 constants_lines=81`.
- [ ] **Step 3:** regenerate; `--check-translation` → exit 0; `--list-jobs` → exit 0; `just model-test` → `OK`.
- [ ] **Step 4: the graph is unchanged.** Targeted run of `configs/recovery-posix-check.cfg`; its `distinct states found` must equal the figure `README.md` records for `recovery-posix-check` (the constants are FALSE and read nowhere yet).
- [ ] **Step 5: commit** `models/lockproto/LockProtocol.head models/lockproto/LockProtocol.tla models/lockproto/configs models/lockproto/expected.toml models/lockproto/expected-extended.toml`, message `model: declare cut 6's ten constants, FALSE in every run`.

---

### Task 3: M5 - `NeverTornRead` becomes a state predicate (item 4); the stale takeover comment (item 3)

**Files:** `algorithm.txt`, `invariants.txt`; regenerate.

- [ ] **Step 0: verify.** `algorithm.txt:35` is `       tornRead = FALSE,                          \* witness: a process read a torn record`; `:144` is `             if (seen = Torn) { tornRead := TRUE; };`; `:487-491` is the comment beginning `           \* A torn record read HERE is deliberately not recorded in \`tornRead\``; `invariants.txt` has `NeverTornRead == ~tornRead`. `rg -n "tornRead" algorithm.txt invariants.txt` shows exactly those places (plus `:235`'s mention of the `NeverTornRead` witness run, which stays true).
- [ ] **Step 1:** delete `algorithm.txt:35` and `:144`.
- [ ] **Step 2:** replace `NeverTornRead == ~tornRead` with:

```tla
\* A classify read found a torn record and judged it uncertain (240.4). A state predicate, not a ghost:
\* the judgement is protocol state, so a misplaced flag cannot make the witness violated for free
\* (cut 6, item 4). It covers the CLASSIFY read only; a takeover's read at S240_5_s5 writes neither
\* seenRec nor classified.
NeverTornRead == ~\E p \in Procs : seenRec[p] = Torn /\ classified[p] = "uncertain"
```

- [ ] **Step 3:** replace the comment at `algorithm.txt:487-491` with:

```tla
           \* A non-record read here (torn, empty, or foreign) continues to step 6, which judges by identity.
           \* No witness distinguishes a torn read from an empty one at this label (cut 6, item 3: a residue).
```

- [ ] **Step 4:** regenerate; gates.
- [ ] **Step 5: the five witnesses still violate.** Targeted runs of `configs/recovery-posix-witness-NeverTornRead.cfg`, `recovery-windows-witness-NeverTornRead.cfg`, `breaklock-posix-witness-NeverTornRead.cfg`, `breaklock-windows-witness-NeverTornRead.cfg`, `breaklock-remote-posix-witness-NeverTornRead.cfg`: each reports `Invariant NeverTornRead is violated`.
- [ ] **Step 6: mutants** (targeted run of `recovery-posix-witness-NeverTornRead.cfg`; kill = it no longer reports `NeverTornRead` violated):
  - M-a: in `S240_1_read`'s uncertain arm, `classified[self] := "uncertain";` → `classified[self] := IF seen = Torn THEN "foreign" ELSE "uncertain";`.
  - M-b: delete `seenRec[self] := seen;` in `S240_1_read`.
- [ ] **Step 7: commit** `algorithm.txt`, `invariants.txt`, `LockProtocol.tla`: `model: NeverTornRead is a state predicate; the tornRead ghost is gone (cut 6, items 3-4)`.

---

### Task 4: M6 - the always-true guard becomes a safety claim (item 5)

**Files:** `algorithm.txt`, `invariants.txt`, every LockProtocol check config (script), `expected.toml`, `configs/recovery-posix-seeded-SEED_RECOVER_LIVE.cfg` (new), the five `configs/*-witness-NeverRecoveredAfterCrash.cfg` (deleted).

- [ ] **Step 0: verify.** `algorithm.txt:34` is the `recoveredAfterCrash = FALSE,` declaration; `:374` is `           if (victim \in Procs) { if (crashed[victim]) { recoveredAfterCrash := TRUE; }; };`; `Replaceable(p)` is the definition at `:101-106`; `invariants.txt` has `NeverRecoveredAfterCrash == ~recoveredAfterCrash`, the comment at `:6-11` beginning `\* Reachability is proved two ways`, and `\* The ghost witnesses, for the witness runs.`; `ls configs/*witness-NeverRecoveredAfterCrash.cfg` lists five files and `expected.toml` has five runs so named.
- [ ] **Step 1: MEASURE FIRST - can a live owner's lock reach `S240_3_s5`?** Temporarily (never committed): in `Replaceable(p)` add the disjunct `\/ (SEED_RECOVER_LIVE /\ ownerLive[p] = "live")`, and REPLACE `:374` with `           if (victim \in Procs) { if (~crashed[victim]) { recoveredAfterCrash := TRUE; }; };`. Regenerate. Copy `configs/recovery-posix-witness-NeverRecoveredAfterCrash.cfg` to `.clavity/scratch/cut6/live-probe.cfg` with `SEED_RECOVER_LIVE = TRUE`, and run it as a targeted run but with `-config ../../.clavity/scratch/cut6/live-probe.cfg`. Expected if reachable: `Invariant NeverRecoveredAfterCrash is violated`. If TLC reports `No error has been found`, STOP: item 5 returns to the owner. Revert both temporary edits and regenerate.
- [ ] **Step 2:** `:34` → `       replacedLive = FALSE,                      \* ghost: 240.3 replaced a lock whose owner had not crashed`.
- [ ] **Step 3:** `:374` → `           if (victim \in Procs) { if (~crashed[victim]) { replacedLive := TRUE; }; };`.
- [ ] **Step 4:** in `Replaceable(p)`, after the `SEED_RECOVER_FOREIGN` disjunct: `                         \/ (SEED_RECOVER_LIVE /\ ownerLive[p] = "live")`.
- [ ] **Step 5: invariants.** Replace `NeverRecoveredAfterCrash == ~recoveredAfterCrash` with:

```tla
\* 240.3 replaces only a DEAD owner's lock: the lock it moves aside and deletes never names an operation
\* that has not crashed (cut 6, item 5). It replaces the witness NeverRecoveredAfterCrash, whose guard was
\* true wherever it ran and so only restated S240_3_s5's label coverage.
ReplacedOnlyDead == ~replacedLive
```

  Replace the comment `invariants.txt:6-11` (from `\* Reachability is proved two ways` through `\* invariant and no \`-continue\`.`) with:

```tla
\* Reachability is proved two ways: a step a label performs is proved by that label's TLC coverage
\* count (design Section 4), and a fact no single label states is checked as a WITNESS in its own small
\* run without `-continue`, which stops at the first violation and prints one trace - a state predicate
\* where the state carries the fact (NeverTornRead, NeverSecondPastId), a ghost flag where it does not
\* (NeverHostCrashChangedLock, NeverInflightLandedAfterTakeover). To get a trace for any other path,
\* re-run the configuration with that witness as an invariant and no `-continue`.
```

  and `\* The ghost witnesses, for the witness runs.` with `\* The witnesses, for the witness runs.`
- [ ] **Step 6: add `ReplacedOnlyDead` to every LockProtocol check config** with a one-off script `.clavity/scratch/cut6/add_invariant.py`: for each `[[run]]` in `expected.toml` and `expected-extended.toml` with `kind = "check"` and `module = "LockProtocol"`, insert the line `    ReplacedOnlyDead` after the line `    RefusalJustified` in its config; assert exactly one insertion per file; print the file count and the list. Report the count.
- [ ] **Step 7: retire the five witnesses:** `git rm configs/*-witness-NeverRecoveredAfterCrash.cfg` and delete their five `[[run]]` blocks from `expected.toml`.
- [ ] **Step 8: the seeded run.** `configs/recovery-posix-seeded-SEED_RECOVER_LIVE.cfg` = a copy of `configs/recovery-posix-seeded-SEED_CHECK_REFUSES_UNTOUCHED.cfg` with its header comment replaced by

```
\* recovery scenario, POSIX: SEEDED - 240.3 replaces a live owner's lock (cut 6, item 5), which is what
\* ReplacedOnlyDead forbids. This run is what proves the claim can fail.
```

  `SEED_CHECK_REFUSES_UNTOUCHED = FALSE`, `SEED_RECOVER_LIVE = TRUE`, and `    ReplacedOnlyDead` added after `    RefusalJustified`. `expected.toml`: after the `recovery-posix-seeded-SEED_CHECK_REFUSES_UNTOUCHED` block, copy it and change `name = "recovery-posix-seeded-SEED_RECOVER_LIVE"`, `config = "configs/recovery-posix-seeded-SEED_RECOVER_LIVE.cfg"`, `violated = ["ReplacedOnlyDead"]`, and in `constants` `SEED_CHECK_REFUSES_UNTOUCHED = false`, `SEED_RECOVER_LIVE = true`.
- [ ] **Step 9:** regenerate; gates. Targeted runs: `recovery-posix-check.cfg` → `No error has been found`; `recovery-posix-seeded-SEED_RECOVER_LIVE.cfg` → `Invariant ReplacedOnlyDead is violated`.
- [ ] **Step 10: mutants.** M-a: Step 3's line → `if (victim \in Procs) { replacedLive := TRUE; };`; targeted `recovery-posix-check.cfg`; kill = `Invariant ReplacedOnlyDead is violated`. M-b: delete Step 3's line; targeted seeded config; kill = it no longer reports `ReplacedOnlyDead` (expected observed: `Invariant SingleWriter is violated`, reasoned, or `No error has been found`).
- [ ] **Step 11: commit** the changed and deleted paths: `model: ReplacedOnlyDead replaces the always-true recoveredAfterCrash witness (cut 6, item 5)`.

---

### Task 5: M7 - the negative control, the `FsInvariants` confirmation, the past seed (item 6)

**Files:** `algorithm.txt`, `configs/recovery-posix-hostcrash-control-check.cfg` (new), `configs/recovery-posix-seeded-SEED_FS_PAST_UNALLOCATED.cfg` (new), `expected.toml`.

- [ ] **Step 0: verify.** `algorithm.txt:1014-1015` are `             \* The whole host goes down.` and `             await HostCrashes /\ crashes < MaxCrashes;`; `configs/recovery-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg` exists; the environment's last seeded block is the `SEED_FS_ALIEN_CONTENT` `or { ... }`, followed by `\* Nothing crashes after all`.
- [ ] **Step 1: the guard.** Replace `             await HostCrashes /\ crashes < MaxCrashes;` with (ONE line - a disjunct continued on a following line splits the translated action, TLC error 2109):

```tla
             \* HostCrashAtDurableOnly (cut 6, item 6's negative control): crash only when nothing is unflushed.
             await HostCrashes /\ crashes < MaxCrashes /\ (~HostCrashAtDurableOnly \/ (Unflushed(fs) = {} /\ \A d \in Dirs : fs.dentries[d] = fs.entries[d]));
```

- [ ] **Step 2: the control run.** `configs/recovery-posix-hostcrash-control-check.cfg` = a copy of `configs/recovery-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg` with header

```
\* recovery scenario, POSIX, host crashes only on a durable state: the NEGATIVE CONTROL for the
\* hostCrashChangedLock ghost (cut 6, item 6). A crash here cannot change the lock, so
\* NeverHostCrashChangedLock must HOLD; a comparison flipped to fire on every crash violates it.
```

  `HostCrashAtDurableOnly = TRUE`, and its invariant section replaced by an `INVARIANTS` list equal to `configs/recovery-posix-check.cfg`'s followed by `    NeverHostCrashChangedLock`. `expected.toml`: copy the `recovery-posix-hostcrash-witness-NeverHostCrashChangedLock` block and change `name = "recovery-posix-hostcrash-control-check"`, `config` accordingly, `kind = "check"`, delete `violated`, `HostCrashAtDurableOnly = true`, add the `unreached` list copied verbatim (labels and reasons) from `recovery-posix-check`, `timeout_minutes = 30`.
- [ ] **Step 3:** regenerate; gates. Targeted run of the control → `No error has been found`; record its distinct states (expected near `recovery-posix-check`'s). Then `python models/lockproto/run.py --scenario recovery --platform posix` → exit 0 (the recovery POSIX scenario is small; this checks the coverage gate). If the control's gate reports a label uncovered that `recovery-posix-check` covers, STOP and report.
- [ ] **Step 4: mutant.** Flip `#` to `=` in `hostCrashChangedLock`'s update (`algorithm.txt:1023`); targeted control run; kill = `Invariant NeverHostCrashChangedLock is violated`.
- [ ] **Step 5: `FsInvariants == TRUE` is already killed (confirm, no change).** In `FsModel.tla`, `FsInvariants(fs) == NoDoubleOpen(fs) /\ LockImpliesHandle(fs) /\ IdsNotReused(fs)` → `FsInvariants(fs) == TRUE`; targeted run of `recovery-posix-seeded-SEED_FS_LOCK_WITHOUT_HANDLE.cfg`; kill = no longer reports `FsOk`.
- [ ] **Step 6: the past seed.** After the `SEED_FS_ALIEN_CONTENT` block add:

```tla
           or {
             \* SEEDED ONLY: a name's past gains an id never allocated, which IdsNotReused's past conjunct
             \* forbids (cut 6, items 6-7). Fires once (it disables itself), and is inert to the protocol under
             \* strong identity: `past` is read only when IdentityStrength = "weak" (FsModel.tla).
             await SEED_FS_PAST_UNALLOCATED /\ fs.next < MaxObjs /\ fs.past[P][ClassOf(LockName)] \subseteq 1..fs.next;
             fs := [fs EXCEPT !.past[P][ClassOf(LockName)] = @ \cup {fs.next + 1}];
           }
```

- [ ] **Step 7: its run.** Config from `recovery-posix-seeded-SEED_FS_LOCK_WITHOUT_HANDLE.cfg`: header `\* recovery, POSIX: SEEDED - an unallocated id in a name's past (cut 6); FsOk must catch it.`, `SEED_FS_LOCK_WITHOUT_HANDLE = FALSE`, `SEED_FS_PAST_UNALLOCATED = TRUE`. `expected.toml` block copied from that run's, changing name, config and the two constants.
- [ ] **Step 8:** regenerate; gates; targeted seeded run → `Invariant FsOk is violated`.
- [ ] **Step 9: mutant.** In `IdsNotReused` (`FsModel.tla`) delete `/\ \A d \in Dirs, c \in Classes : fs.past[d][c] \subseteq 1..fs.next`; targeted seeded run; kill = no longer reports `FsOk`.
- [ ] **Step 10: commit** `model: a negative control for the host-crash ghost, and a seed for IdsNotReused's past half (cut 6, item 6)`.

---

### Task 6: `FsOk`'s other conjuncts (item 7)

- [ ] **Step 0: verify** the Task 5 past-seed block is followed by `\* Nothing crashes after all`; `rg -n "DirLockName" algorithm.txt` shows only `:205`'s `FsLookup(fs, P, DirLockName)` (its entry is read nowhere else). If it shows another read, STOP.
- [ ] **Step 1:** after the past-seed block add:

```tla
           or {
             \* SEEDED ONLY: a second handle record for a process and object it already has open, differing
             \* only in `del`, which NoDoubleOpen forbids (cut 6, item 7). Fires once. On POSIX the protocol
             \* cannot produce this: `share` forces del = TRUE, so a real double open collapses into one record
             \* (a residue in TODO.md).
             await SEED_FS_DOUBLE_HANDLE /\ NoDoubleOpen(fs);
             with (h \in fs.handles) {
               fs := [fs EXCEPT !.handles = @ \cup {[h EXCEPT !.del = ~h.del]}];
             };
           }
           or {
             \* SEEDED ONLY: an entry names an id never allocated, which IdsNotReused's entries conjunct forbids
             \* (cut 6, item 7). Fires once. DirLockName's entry, because its only reader diverts to the backoff
             \* label: the lock path and any Foreign start object are untouched.
             await SEED_FS_ENTRY_UNALLOCATED /\ fs.next < MaxObjs /\ fs.entries[P][ClassOf(DirLockName)] = NoObj;
             fs := [fs EXCEPT !.entries[P][ClassOf(DirLockName)] = fs.next + 1];
           }
```

- [ ] **Step 2: runs.** `recovery-posix-seeded-SEED_FS_DOUBLE_HANDLE.cfg` and `recovery-posix-seeded-SEED_FS_ENTRY_UNALLOCATED.cfg`, each from `recovery-posix-seeded-SEED_FS_LOCK_WITHOUT_HANDLE.cfg` with only its own seed TRUE and header `\* recovery, POSIX: SEEDED - <what the seed does> (cut 6, item 7); FsOk must catch it.`; two `expected.toml` blocks copied from that run's, `violated = ["FsOk"]`.
- [ ] **Step 3:** regenerate; gates; each targeted seeded run → `Invariant FsOk is violated`.
- [ ] **Step 4: mutants.** M-a: `FsInvariants(fs) == LockImpliesHandle(fs) /\ IdsNotReused(fs)`; targeted `...-SEED_FS_DOUBLE_HANDLE.cfg`; kill = no longer reports `FsOk`. M-b: delete `IdsNotReused`'s entries conjunct (`/\ \A d \in Dirs, c \in Classes : fs.entries[d][c] \in (1..fs.next) \cup {NoObj}`); targeted `...-SEED_FS_ENTRY_UNALLOCATED.cfg`; same kill.
- [ ] **Step 5: commit** `model: one seed per FsOk conjunct (cut 6, item 7)`.

---

### Task 7: `Classifiable`'s seed pins the seed (item 8)

- [ ] **Step 0: verify** the `SEED_FS_ALIEN_CONTENT` block (`fs := [fs EXCEPT !.content[o] = NoProc];`) and `Contents == Records \cup {Torn, Foreign, EmptyFile}` (`FsModel.tla:52`).
- [ ] **Step 1:** after the Task 6 blocks add:

```tla
           or {
             \* SEEDED ONLY: record-shaped content that is not a record the protocol writes - IsRecord accepts it,
             \* Records does not contain it (cut 6, item 8). A separate run from SEED_FS_ALIEN_CONTENT, because a
             \* seeded run halts at its first violation. Fires once; never touches a Foreign start object; its
             \* `op` is a real process, so a classifier that reads it evaluates `crashed[seen.op]` normally.
             await SEED_FS_ALIEN_RECORD /\ \A x \in Objs : fs.content[x] = NoContent \/ fs.content[x] \in Contents;
             with (o \in {x \in Objs : fs.content[x] # NoContent /\ x # foreignObj}, q \in Procs) {
               fs := [fs EXCEPT !.content[o] = [tag |-> "record", op |-> q, kind |-> "alien"]];
             };
           }
```

- [ ] **Step 2:** config `recovery-posix-seeded-SEED_FS_ALIEN_RECORD.cfg` (from `recovery-posix-seeded-SEED_FS_ALIEN_CONTENT.cfg`, only this seed TRUE, header `\* recovery, POSIX: SEEDED - record-shaped alien content (cut 6, item 8); Classifiable must catch it.`) and its `expected.toml` block, `violated = ["Classifiable"]`.
- [ ] **Step 3:** regenerate; gates; targeted seeded run → `Invariant Classifiable is violated`. An evaluation error: STOP and report.
- [ ] **Step 4: mutants** (targeted seeded run; kill = no longer reports `Classifiable`): M-a `Classifiable == \A o \in Objs : fs.content[o] # NoProc`; M-b `Classifiable == \A o \in Objs : fs.content[o] = NoContent \/ fs.content[o] \in {Torn, Foreign, EmptyFile} \/ IsRecord(fs.content[o])`.
- [ ] **Step 5: commit** `model: a record-shaped alien seed, so Classifiable cannot be gutted to its NoProc case (cut 6, item 8)`.

---

### Task 8: M8 - the liveness consequent (item 9)

- [ ] **Step 0: verify** `rec_decide`'s `           if (Replaceable(self)) { goto rec_recover; }` (`algorithm.txt:810`) and `configs/recovery-posix-liveness.cfg` (one `PROPERTY DeadLockEventuallyCleared`, no `SYMMETRY`).
- [ ] **Step 1:** replace that line with:

```tla
           \* SEED_RECOVERER_GIVES_UP (cut 6, item 9): it judged the lock replaceable and then does nothing,
           \* refusing nothing - the dead lock stays and no uncertainty is reported.
           if (Replaceable(self)) { if (SEED_RECOVERER_GIVES_UP) { goto rec_end; } else { goto rec_recover; }; }
```

- [ ] **Step 2: the seeded temporal run.** `configs/recovery-posix-seeded-SEED_RECOVERER_GIVES_UP.cfg` = a copy of `configs/recovery-posix-liveness.cfg` with `SEED_RECOVERER_GIVES_UP = TRUE` and header `\* recovery, POSIX: SEEDED - a Recoverer gives up on a dead lock (cut 6, item 9); DeadLockEventuallyCleared must fail. No SYMMETRY (liveness).`. `expected.toml`: copy the `recovery-posix-liveness` block and change `name = "recovery-posix-seeded-SEED_RECOVERER_GIVES_UP"`, `config`, `kind = "seeded"`, add `violated = ["DeadLockEventuallyCleared"]`, delete `unreached`, `SEED_RECOVERER_GIVES_UP = true`, `timeout_minutes = 30`.
- [ ] **Step 3:** regenerate; gates; targeted seeded run → `Temporal properties were violated`. If `No error has been found`, the other Recoverer may be clearing the lock: report it (the spec's "measure" point) and STOP.
- [ ] **Step 4: mutants** (targeted seeded run; kill = no longer `Temporal properties were violated`): M-a `DeadLockEventuallyCleared == [](DeadOwnerLock /\ EnvQuiet /\ PendingMover => <>TRUE)`; M-b `UncertainReported == TRUE`; M-c `UncertainReported == \E p \in Recoverers \cup Cleanups : refused[p] \in {"TARGET_LOCK_UNCERTAIN", "none"}` (the widening that admits "none"; a widening admitting only "RESTART" is the residue).
- [ ] **Step 5: commit** `model: a Recoverer that gives up must violate DeadLockEventuallyCleared (cut 6, item 9)`.

---

### Task 9: `RefusalJustified`'s two unseeded sites (item 10)

- [ ] **Step 0: verify** `S99_release_check`'s `           if (~StillOwned(self)) {` (`algorithm.txt:666`), `S21_1_s3`'s (`:930`), and that `configs/breaklock-posix-seeded-SEED_RESTART_RELEASES.cfg` exists.
- [ ] **Step 1:** `:666` → `           if (~StillOwned(self) \/ SEED_RELEASE_CHECK_REFUSES_UNTOUCHED) {`; `:930` → `           if (~StillOwned(self) \/ SEED_RESTART_CHECK_REFUSES_UNTOUCHED) {`.
- [ ] **Step 2: runs.** `recovery-posix-seeded-SEED_RELEASE_CHECK_REFUSES_UNTOUCHED` from `recovery-posix-seeded-SEED_CHECK_REFUSES_UNTOUCHED` (config and block: that seed FALSE, the new one TRUE); `breaklock-posix-seeded-SEED_RESTART_CHECK_REFUSES_UNTOUCHED` from `breaklock-posix-seeded-SEED_RESTART_RELEASES` (that seed FALSE, the new one TRUE). Both `violated = ["RefusalJustified"]`, header `\* <scenario>, POSIX: SEEDED - <site> refuses a holder nobody dispossessed (cut 6, item 10).`.
- [ ] **Step 3:** regenerate; gates; both targeted seeded runs → `Invariant RefusalJustified is violated`.
- [ ] **Step 4: mutants** (kill = no longer reports `RefusalJustified`): at `S99_release_check`'s refusal, `refusedOk[self] := lostLock[self];` → `refusedOk[self] := TRUE;`, targeted recovery seeded run; the same at `S21_1_s3`, targeted breaklock seeded run.
- [ ] **Step 5: commit** `model: seed RefusalJustified's release-check and restart-check sites (cut 6, item 10)`.

---

### Task 10: `PlainNeverOwnsUncertain` - narrow its stated claim (item 11)

- [ ] **Step 0: verify** `invariants.txt:39-41` (the two-line comment and `PlainNeverOwnsUncertain == ~touchedUncertain`) and `algorithm.txt:308`.
- [ ] **Step 1:** replace the comment with:

```tla
\* No process moves aside (240.3 step 2) a lock it judged uncertain: a plain process refuses such a lock,
\* and a --break-lock process takes it over in place (240.5). The ghost is set at the move-aside only
\* (S240_3_s2), so removing, renaming, overwriting or creating are NOT covered - a residue (cut 6,
\* item 11). breaklock-posix-seeded-SEED_MOVE_ASIDE_FOR_UNCERTAIN, whose actors are Breakers, violates it.
```

- [ ] **Step 2:** regenerate; `--check-translation` → exit 0.
- [ ] **Step 3: commit** `model: PlainNeverOwnsUncertain claims only the move-aside it instruments (cut 6, item 11)`.

---

### Task 11: split `ForeignUntouched`; seed its content half (item 12)

**Files:** `invariants.txt`, `algorithm.txt`, the 44 configs that list `ForeignUntouched` (script), `expected.toml`, `configs/breaklock-posix-seeded-SEED_TAKEOVER_FOREIGN.cfg` (new).

- [ ] **Step 0: verify.** `invariants.txt:46` is `ForeignUntouched == foreignObj # NoObj => LockObj = foreignObj /\ fs.content[foreignObj] = Foreign`; `rg -l "ForeignUntouched" configs | wc -l` → 44; `expected.toml:231` is `violated = ["ForeignUntouched"]` in the `recovery-posix-seeded-SEED_RECOVER_FOREIGN` block; `S21_1_restart_decide`'s `           else if (classified[self] = "foreign") { refused[self] := "CONTROL_PLANE_NAMESPACE_CONFLICT"; }` is `algorithm.txt:915`; `algorithm.txt:18` is `       fs \in {FsInit, FsWith(P, LockName, Foreign)},`.
- [ ] **Step 1: split.** Replace `invariants.txt:46` (and keep its preceding comment, adjusted) with:

```tla
\* A `Foreign` object at a lock path is never written, renamed, or deleted (Section 7). Two invariants, not one
\* (cut 6, item 12): a seed that breaks only the content half is followed on the same trace by a release that
\* moves the lock path, and a single invariant would report that as the same name. State predicates, so no
\* misplaced ghost can make either vacuous.
ForeignStaysAtLockPath == foreignObj # NoObj => LockObj = foreignObj
ForeignContentUntouched == foreignObj # NoObj => fs.content[foreignObj] = Foreign
```

- [ ] **Step 2: rename in every config** with a one-off script `.clavity/scratch/cut6/split_foreign.py`: in each file of `configs/` containing the line `    ForeignUntouched`, replace that line with `    ForeignStaysAtLockPath` followed by `    ForeignContentUntouched` (same line ending); assert one replacement per file; print the count → 44.
- [ ] **Step 3:** `expected.toml:231` → `violated = ["ForeignStaysAtLockPath"]` (`SEED_RECOVER_FOREIGN` moves the foreign object aside; its content stays `Foreign`).
- [ ] **Step 4: the seed.** Replace `algorithm.txt:915` with:

```tla
           \* SEED_TAKEOVER_FOREIGN (cut 6, item 12): --break-lock takes over an object that is not a lock.
           else if (classified[self] = "foreign") {
             if (SEED_TAKEOVER_FOREIGN) { goto brk_takeover; }
             else { refused[self] := "CONTROL_PLANE_NAMESPACE_CONFLICT"; };
           }
```

- [ ] **Step 5: its run.** `breaklock-posix-seeded-SEED_TAKEOVER_FOREIGN` from `breaklock-posix-seeded-SEED_RESTART_RELEASES` (config and block; that seed FALSE, this TRUE), header `\* breaklock, POSIX: SEEDED - --break-lock takes over a foreign object (cut 6, item 12); ForeignContentUntouched must catch it.`, `violated = ["ForeignContentUntouched"]`.
- [ ] **Step 6:** regenerate; gates. Targeted runs: `recovery-posix-seeded-SEED_RECOVER_FOREIGN.cfg` → `Invariant ForeignStaysAtLockPath is violated`; `breaklock-posix-seeded-SEED_TAKEOVER_FOREIGN.cfg` → `Invariant ForeignContentUntouched is violated`; `recovery-posix-check.cfg` → `No error has been found`. If the new seeded run reports another invariant first, STOP and report which, with its trace.
- [ ] **Step 7: mutant.** `ForeignContentUntouched == TRUE`; targeted `breaklock-posix-seeded-SEED_TAKEOVER_FOREIGN.cfg`; kill = it no longer reports `ForeignContentUntouched` (expected: `Invariant ForeignStaysAtLockPath is violated`, once the Breaker's release moves the lock path).
- [ ] **Step 8: commit** `model: split ForeignUntouched and seed its content half (cut 6, item 12)`.

---

### Task 12: weak identity - runs and the `NeverSecondPastId` witness (item 13)

- [ ] **Step 0: verify** `configs/recovery-posix-check.cfg`, `configs/mixed-posix-check.cfg`, `configs/recovery-posix-witness-NeverTornRead.cfg` and `configs/mixed-posix-witness-NeverUncertainLockWithPendingBreaker.cfg` exist with `IdentityStrength = "strong"`, and `FsModel.tla:135` adds to `past` under weak identity.
- [ ] **Step 1: the witness.** In `invariants.txt`, after `NeverHostCrashChangedLock`:

```tla
\* Weak identity was exercised: some name's past holds a second id, which only a re-created name under
\* IdentityStrength = "weak" produces (FsCreate, FsModel.tla). Its weak runs must stop with it violated;
\* without it, gutting `past` to {} would reduce weak identity to strong and change nothing any run checks
\* (cut 6, item 13).
NeverSecondPastId == \A d \in Dirs, c \in Classes : Cardinality(fs.past[d][c]) <= 1
```

- [ ] **Step 2: configs.** `recovery-posix-weak-check.cfg` and `mixed-posix-weak-check.cfg` from the two strong checks; `recovery-posix-weak-witness-NeverSecondPastId.cfg` from `recovery-posix-witness-NeverTornRead.cfg` and `mixed-posix-weak-witness-NeverSecondPastId.cfg` from `mixed-posix-witness-NeverUncertainLockWithPendingBreaker.cfg` (keep CONSTANTS and SYMMETRY; replace the invariant section with `INVARIANT NeverSecondPastId`; drop any `PROPERTY`). All four: `IdentityStrength = "weak"`, header `\* <scenario>, POSIX, WEAK identity (cut 6, item 13). State counts: README.` replacing the template's header (whose "Measured: N distinct states" line does not describe this run).
- [ ] **Step 3: `expected.toml`.** Copy each template's block; change `name`, `config`, `IdentityStrength = "weak"`; for the witnesses `violated = ["NeverSecondPastId"]` and `timeout_minutes = 60`; for the checks `timeout_minutes = 60`, `unreached` copied verbatim (labels and reasons) from the strong twin.
- [ ] **Step 4: MEASURE FIRST.** Regenerate; gates; targeted run of each of the four. Checks: record distinct states. Witnesses: `Invariant NeverSecondPastId is violated`. STOP and report if: a weak check reports ANY invariant violated (a possible protocol finding under weak identity - the owner's); a weak check exceeds 60 minutes (the spec's split-or-narrow decision); or `python models/lockproto/run.py --scenario recovery --platform posix` reports a weak check's coverage differing from its strong twin's (report the labels).
- [ ] **Step 5: mutant.** In `FsModel.tla`, change all three `IF IdentityStrength = "weak" THEN @ \cup {o} ELSE @` to `@`; targeted run of each weak witness; kill = `No error has been found`.
- [ ] **Step 6: commit** `model: weak-identity checks and the NeverSecondPastId witness (cut 6, item 13)`.

---

### Task 13: bookkeeping (everything but state counts)

- [ ] **Step 1: `TODO.md`.** Close (`[x]`, one line each naming the commit): M5; M6; M8; "`tornRead` is set at one of the two labels" (narrowed); "`FsOk`'s other two conjuncts"; "`Classifiable`'s seed pins the seed"; "Two of `RefusalJustified`'s three `lostLock` sites"; "`PlainNeverOwnsUncertain`'s ghost is wired to one label" (narrowed); "`ForeignUntouched`'s content half"; "`IdentityStrength = "weak"` is dead". M7 stays OPEN, rewritten to name only its two remaining halves ("always keep new content", "`Unflushed == {}`"), which Part 2's extended-tier union closes. Any item a task STOPPED on is rewritten with what the measurement showed and left open. Add residues: POSIX double open invisible to `NoDoubleOpen`; M8 widening admitting only `"RESTART"`; `PlainNeverOwnsUncertain` covers only the move-aside; torn versus empty at takeover. Fix `TODO.md`'s stale `algorithm.txt:434` citation to `:443` (verified anomaly).
- [ ] **Step 2: `models/lockproto/README.md`.** Replace the invariant name `ForeignUntouched` at `:119-121` with the two new names; list the new and retired runs where the README lists runs; fix `:90-91`'s `deferred` sentence to name only `S96_1_backoff` (verified anomaly). Do not touch state counts (Task 15).
- [ ] **Step 3: the design doc** `docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md`: the witness table rows at `:683` (`NeverTornRead` - now a state predicate) and `:685` (`NeverRecoveredAfterCrash` - retired, replaced by the invariant `ReplacedOnlyDead`); the weak-identity rationale near `:1125`; the "FsOk and Classifiable are exempt" sentence near `:775`; each with one line citing cut 6.
- [ ] **Step 4:** `just check` → exit 0 (typos covers these files); `just model-test` → `OK`.
- [ ] **Step 5: commit** `docs: cut 6 Part 1 bookkeeping - TODO closures and residues, README and design-doc corrections`.

---

### Task 14: Part 1 verification

- [ ] **Step 1:** `--check-translation`, `--list-jobs`, `just model-test`, `just check` → all pass; `git status --short` clean.
- [ ] **Step 2: STOP.** The full Model workflow (every scenario, both tiers) needs a pushed ref; pushing is the owner's decision. Report ready for the CI run.

### Task 15: state counts (after the CI run)

- [ ] **Step 1:** from the CI Model run's logs, write every changed and new distinct-state count into `models/lockproto/README.md`, including `:144`'s weak-identity sentence (replace its "identical state graph" claim with the measured weak counts).
- [ ] **Step 2:** in every config whose header carries a "Measured: ... distinct states" line (`rg -l "distinct states" configs`), replace the figure with the new one, or with `State counts: README.` where the run is new.
- [ ] **Step 3:** `just check` → exit 0; commit `docs: cut 6 Part 1 state counts from CI run <id>`.

## Self-review (done by the plan author)

- **Spec coverage:** item 1 → Task 1 (measurement; the rest is Part 2); item 2 → Part 2; item 3 → Task 3 (comment, narrowing) + Part 2; item 4 → Task 3; item 5 → Task 4; item 6 → Task 5 + Part 2 (the two `FsHostCrash` arms; M7 stays open); item 7 → Task 6; item 8 → Task 7; item 9 → Task 8 (all three spec mutants); item 10 → Task 9; item 11 → Task 10; item 12 → Task 11 (split, declared refinement 4); item 13 → Task 12 + Task 15; residues → Task 13; mutant-for-reason → the ground rules' kill definition and each task's mutant step; scripted constants → Task 2.
- **Panel round 1 (independent reviewer), each finding's fold:** MG-1 → refinement 4, Task 11; MG-2 → refinement 5, Task 7 (one-shot, excludes `foreignObj`, `op` in `Procs`); MG-3 → Task 6's `DirLockName` entry seed; MG-4 → the kill definition (any other report, or none); MG-5 → one-shot seeds, 60-minute weak witnesses, the 60-minute targeted-run stop; LI-1 → `-metadir` scratch, `.old` removal, named-path commits; LI-2 → "copying a block" rule and per-step changed-key lists; LI-3 → Task 12 Step 4's STOP on a violation; SC-1 → counts moved to Task 15 after CI, config headers included, `invariants.txt:6-11` and `:64` in Task 4; SC-2 → refinement 6, targeted runs; AB-1 → Task 8 M-c; AB-2 → M7 stays open (Task 13); CA-1 → "On any STOP" rule; below-floor (2109 wording, CI needs a push) → Task 5 Step 1, Task 14.
- **Consistency:** the ten constants are Task 2's wherever used; run names follow `<scenario>-<variant>-<kind>[-<SEED>]`; `ReplacedOnlyDead`, `NeverSecondPastId`, `HostCrashAtDurableOnly`, `ForeignStaysAtLockPath`, `ForeignContentUntouched` are spelled the same in every task.
