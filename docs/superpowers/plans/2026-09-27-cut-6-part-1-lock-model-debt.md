# Cut 6 Part 1: the lock-model debt - measurements and the model's net

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close cut 6's items whose code does not depend on an unmeasured TLC output shape, and take the measurements that Part 2 (the branch-granular union and its entries) is written against.

**Architecture:** Every change is to the TLA+ model's checking net in `models/lockproto/` - seeds, invariants, witnesses, runs - plus one new coverage fixture under `testdata/`. No protocol behaviour changes. `LockProtocol.tla` is GENERATED from `LockProtocol.head` + `algorithm.txt` + `invariants.txt`, so every source change regenerates it.

**Tech Stack:** TLA+/PlusCal (TLC 2.19, `tla2tools.jar`), Python 3.11+ (`run.py`, `unittest`), Java 21, `just`.

**Design authority:** `docs/superpowers/specs/2026-09-26-cut-6-lock-model-debt-design.md` (owner-approved; adversarial panel GREEN at round 3, `0c995db`). Every citation below was read at `0c995db`.

---

## Why a Part 1 and a Part 2

The spec's union-based closures (items 1, 2, 3, 6) rest on what TLC prints for arms inside actions - never recorded in this repository. Writing their code now would mean inventing that output. So:

- **Part 1 (this plan):** the arm-coverage MEASUREMENT (Task 1), the constant migration, and every item whose closure is a seed, a state predicate, a witness, a negative control or a comment - items 4, 5, 6 (control, `FsInvariants`, past seed), 7, 8, 9, 10, 11, 12, 13 - plus the `TODO.md`/README bookkeeping.
- **Part 2 (written after Task 1 lands, against its recorded output):** `parse_cost_nodes`' full span and action grouping, the `branches` schema and resolution, the two tiers' union judging (including the new extended-tier CI job), and the `branches` entries for items 2, 3 and 6. Part 1's Task 1 writes the measurement Part 2 needs; if the measurement triggers one of the spec's stated consequences, the affected item returns to the owner before Part 2 is written.

## Where this plan refines the spec (declared, not silent)

1. **The arm fixture is a NEW module (`ArmFixture.tla` + `ArmOps.tla`), not an extension of `CoverageFixture.tla`.** `coverage.out` is recorded from `CoverageFixture` and pinned by the existing label-coverage tests; changing that module re-records it and churns tests unrelated to this cut.
2. **The negative control's guard is a DURABLE-STATE predicate, not a `pc` predicate.** The spec named `pc` "with each role's start label"; whether PlusCal accepts `pc` in an algorithm body was never verified, and a `define` block precedes `pc`. The guard `FsDurable(fs)` - nothing unflushed and every directory's entries equal to its durable entries - says exactly what the spec's argument needs: a host crash on such a state cannot change the lock, so the correct model HOLDS `NeverHostCrashChangedLock`, and the flipped comparison fires on the first such crash. It is not `live`-based (the spec's stated trap).
3. **Item 11 needs no "measure first".** A Breaker enters `Recover` only through `brk_recover` (`algorithm.txt:908`, `:912`), unseeded only when `Replaceable` holds, i.e. an owner judged dead, so `JudgedUncertain` is false; the ghost is set for a Breaker only under `SEED_MOVE_ASIDE_FOR_UNCERTAIN`, and `breaklock-posix-seeded-SEED_MOVE_ASIDE_FOR_UNCERTAIN` (Breakers only, `expected.toml:419-425`) already expects `PlainNeverOwnsUncertain` violated. That is the measurement: the invariant covers Breakers; only its comment is wrong.

## Ground rules for every task

- **Worktree:** `E:\Rust\flux-engine`, branch `spec/seq-after-cut5`. NEVER push; publishing is the owner's `just pr` after the capstone and test audit.
- **Step 0 - state verification.** Confirm each quoted "current" text before editing. If it differs, STOP and report `STATE_MISMATCH: <file>: <what differs>`. Do not adapt.
- **Shape-divergence stop.** If making a change work needs a name, constant, invariant, run name, `expected.toml` key or value different from this plan, STOP and report `[plan] -> [yours] because <reason>`.
- **Regenerate after every source edit** of `LockProtocol.head`, `algorithm.txt` or `invariants.txt`, from `models/lockproto` in Git Bash:
  ```bash
  cat LockProtocol.head algorithm.txt invariants.txt > LockProtocol.tla && \
  java -cp ../../target/tla/tla2tools.jar pcal.trans LockProtocol.tla && rm -f LockProtocol.cfg
  ```
  then `python run.py --check-translation` must print nothing and exit 0. (The jar is fetched by the first `run.py` run; run `python run.py --check-translation` once first if `target/tla/tla2tools.jar` is missing.)
- **Gates:** `just model-test` (the runner's unit tests; expect `OK`), `python models/lockproto/run.py --check-translation` (exit 0), and the named runs of each task. A run subset is `python models/lockproto/run.py --scenario <s> --platform <p>`; it prints one line per run and exits 0 only if every run met its expectation.
- **A mutant is killed only for its EXPECTED reason** (spec, "Verification"). Each mutant step names the reason; a red for another reason - a load error, a timeout, "cannot judge the union" - is not a kill. Record mutant, run, expected reason and the observed line in the task's report. Revert every mutant and confirm `git diff` shows only the task's intended change.
- **Do not time anything.** Runs are for outcomes and state counts, never durations (timing is the owner's, top-level only).
- Write files with the Write/Edit tools; commit messages may use a heredoc. Commits end with:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`

## File map

| File | Change |
|---|---|
| `models/lockproto/testdata/ArmFixture.tla`, `ArmOps.tla`, `ArmFixture.cfg` | NEW measurement fixture (Task 1) |
| `models/lockproto/testdata/record_fixtures.py` | one `COVERAGE_CASES` entry (Task 1) |
| `models/lockproto/testdata/arm_coverage.out` | NEW recorded output (Task 1) |
| `docs/superpowers/plans/2026-09-27-cut-6-arm-coverage-measurement.md` | NEW findings, Part 2's input (Task 1) |
| `models/lockproto/LockProtocol.head` | ten new constants (Task 2) |
| `models/lockproto/algorithm.txt` | Tasks 3-11 |
| `models/lockproto/invariants.txt` | Tasks 3, 4, 10, 12 |
| `models/lockproto/LockProtocol.tla` | regenerated each time |
| `models/lockproto/configs/*.cfg` | constant migration (Task 2), new and retired runs (Tasks 4-12) |
| `models/lockproto/expected.toml`, `expected-extended.toml` | same |
| `TODO.md`, `models/lockproto/README.md`, `docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md` | Task 13 |

---

### Task 1: measure TLC's coverage of arms (the fixture Part 2 is written against)

**Files:** Create `models/lockproto/testdata/ArmFixture.tla`, `ArmOps.tla`, `ArmFixture.cfg`, `arm_coverage.out`, `docs/superpowers/plans/2026-09-27-cut-6-arm-coverage-measurement.md`; modify `models/lockproto/testdata/record_fixtures.py`.

- [ ] **Step 0: verify.** `record_fixtures.py` has `COVERAGE_CASES = {` with exactly the three entries `"coverage"`, `"smoke_check_coverage"`, `"smoke_seeded_coverage"`, and `main()` records each by name.

- [ ] **Step 1: write `testdata/ArmOps.tla`** - the second module, holding the `LET`/`CASE` shape of `FsModel.tla`'s `FsHostCrash` with one dead arm and a constant-level body:

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
              [] OTHER -> Dead.tag = "dead"
    IN out(v)
====
```

- [ ] **Step 2: write `testdata/ArmFixture.tla`** with only the PlusCal source; the translator fills in the rest. It exercises: `with` plus `if`/`else if`/`else` inside ONE label with one arm dead; two identical-bodied arms (item 2's shape); the second module's operator called at TWO sites in one action (item 6's shape):

```tla
---- MODULE ArmFixture ----
\* Coverage-shape fixture for cut 6 (design 2026-09-26-cut-6, item 1). Not part of the lock-protocol
\* model; testdata/record_fixtures.py records its TLC output as arm_coverage.out.
EXTENDS Naturals, ArmOps

(* --algorithm ArmFixture {
  variables
    x = 0, y = 0, z = TRUE;

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
      with (w \in {"old", "new"}) {
        z := Pick(x, w) = Pick(y, "old");
      };
  }
}
*)
====
```

- [ ] **Step 3: translate** from `models/lockproto/testdata` (Git Bash): `java -cp ../../../target/tla/tla2tools.jar pcal.trans ArmFixture.tla && rm -f ArmFixture.cfg`. Expected: exit 0 and a `\* BEGIN TRANSLATION` block appended to `ArmFixture.tla`. (`pcal.trans` also writes an `ArmFixture.cfg`; it is replaced in Step 4.)

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

  and in `main()`, before `build_two_block_fixture(...)`, add:

```python
    record_coverage_case(jar, "arm_coverage", *COVERAGE_CASES["arm_coverage"])
```

- [ ] **Step 6: record only this case** (the other fixtures must not change): from `models/lockproto`,

```bash
python -c "import sys; sys.path.insert(0, 'testdata'); import record_fixtures as r, run; r.record_coverage_case(run.ensure_jar(), 'arm_coverage', *r.COVERAGE_CASES['arm_coverage'])"
```

  Expected: `arm_coverage: exit 0`, and `git status --short models/lockproto/testdata` shows ONLY the new files and `record_fixtures.py` - no existing `.out` modified.

- [ ] **Step 7: read the recorded 2221 lines against the generated `ArmFixture.tla` and answer each question with the quoted lines as evidence** in `docs/superpowers/plans/2026-09-27-cut-6-arm-coverage-measurement.md`:
  1. Does each arm of `branchy`'s `if`/`else if`/`else if`/`else` get its OWN node (a distinct span), and is the dead arm (`i = 7`, body `x := 99`) at count 0 while the live arms are > 0?
  2. Is the arm's GUARD a separate node from its BODY, and which of them is at 0 for the dead arm? (The spec's anchors must count BODIES.)
  3. Under `with (i \in ...)`: are counts per evaluation of each `i`, or per action?
  4. `twins`: both bodies `y := 5` yield the same successor. Does each body node count > 0?
  5. `Pick`'s `CASE` in the second module: does each arm body get a node in module `ArmOps`, is `"never"`'s at 0, and does the constant-level `OTHER -> Dead.tag = "dead"` body get a node at all?
  6. `two_sites` calls `Pick` twice: are the `ArmOps` nodes printed once per call site (nested under each call), or once in total?
  Then state, for each of the spec's stated consequences (item 1, "Each outcome has a consequence"), whether it is triggered, and so which of items 2, 3 and 6 proceed to Part 2 and which return to the owner.

- [ ] **Step 8:** `just model-test` → `OK` (nothing existing changed).
- [ ] **Step 9: commit.**

```bash
git add models/lockproto/testdata/ArmFixture.tla models/lockproto/testdata/ArmOps.tla \
        models/lockproto/testdata/ArmFixture.cfg models/lockproto/testdata/arm_coverage.out \
        models/lockproto/testdata/record_fixtures.py \
        docs/superpowers/plans/2026-09-27-cut-6-arm-coverage-measurement.md
git commit -F - <<'EOF'
test(model): record TLC's coverage of arms inside actions (cut 6, item 1)

A new fixture, ArmFixture/ArmOps, with a dead if-arm, two identical-bodied arms, a second
module's LET/CASE with a dead arm and a constant body, and one operator at two call sites.
The findings note answers the spec's six questions from the recorded 2221 lines; Part 2
is written against it.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

- [ ] **Step 10: STOP and report the findings to the driver.** The driver decides, with the owner where the spec says so, which union closures go to Part 2. Tasks 2-13 do not depend on this answer and may proceed.

---

### Task 2: declare the new constants everywhere (the scripted migration)

**Files:** Modify `models/lockproto/LockProtocol.head`, all 81 LockProtocol configs in `models/lockproto/configs/`, `expected.toml`, `expected-extended.toml`; regenerate `LockProtocol.tla`.

The ten constants, all `FALSE` in every existing run: `SEED_RECOVER_LIVE`, `SEED_FS_PAST_UNALLOCATED`, `SEED_FS_DOUBLE_HANDLE`, `SEED_FS_ENTRY_UNALLOCATED`, `SEED_FS_ALIEN_RECORD`, `SEED_RECOVERER_GIVES_UP`, `SEED_RELEASE_CHECK_REFUSES_UNTOUCHED`, `SEED_RESTART_CHECK_REFUSES_UNTOUCHED`, `SEED_TAKEOVER_FOREIGN`, `HostCrashAtDurableOnly`.

(`HostCrashAtDurableOnly` replaces the spec's name `HostCrashAtStartOnly`, because refinement 2 changes what it means; the name says what the guard is.)

- [ ] **Step 0: verify.** `LockProtocol.head` has the line `    SEED_CHECK_REFUSES_UNTOUCHED, \* a Section 99 check that refuses a holder nobody dispossessed` followed by `    FIX_REMOTE_LEASE_SPEC  \* fix flag ...`. Exactly 81 files in `configs/` contain a line beginning `    SEED_CHECK_REFUSES_UNTOUCHED = ` (`rg -l '^    SEED_CHECK_REFUSES_UNTOUCHED = ' configs | wc -l` → 81). `rg -c 'SEED_CHECK_REFUSES_UNTOUCHED = (true|false) \}' expected.toml expected-extended.toml` → 72 and 9.

- [ ] **Step 1: header.** In `LockProtocol.head`, replace the `SEED_CHECK_REFUSES_UNTOUCHED` line with:

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


- [ ] **Step 2: migrate configs and constants with a one-off script** written to `.clavity/scratch/cut6/migrate_constants.py` (gitignored; not committed):

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

  Run from the repo root: `python .clavity/scratch/cut6/migrate_constants.py models/lockproto` → expected `configs=81 constants_lines=81`.

- [ ] **Step 3: regenerate** `LockProtocol.tla` (ground rules) and `python models/lockproto/run.py --check-translation` → exit 0.
- [ ] **Step 4: load check.** `python models/lockproto/run.py --list-jobs` → exit 0 (loading validates every config against its `constants`, `run.py:859-868`). `just model-test` → `OK`.
- [ ] **Step 5: one quick run proves the graph is unchanged** (every new constant is FALSE and read nowhere yet): `python models/lockproto/run.py --scenario recovery --platform posix`; expected exit 0, and each run's state counts identical to `README.md`'s recorded ones for that pairing.
- [ ] **Step 6: commit** (`git add models/lockproto`), message `model: declare cut 6's ten constants, FALSE in every run`.

---

### Task 3: M5 - `NeverTornRead` becomes a state predicate (item 4); the stale omission comment (item 3)

**Files:** `algorithm.txt`, `invariants.txt`, regenerate.

- [ ] **Step 0: verify.** `algorithm.txt:35` is `       tornRead = FALSE,                          \* witness: a process read a torn record`; `:144` is `             if (seen = Torn) { tornRead := TRUE; };`; `:487` begins `           \* A torn record read HERE is deliberately not recorded in \`tornRead\`, unlike the classify read.` (read `:487-491` whole); `invariants.txt` has `NeverTornRead == ~tornRead`. `rg -n tornRead algorithm.txt invariants.txt` shows only those four places.
- [ ] **Step 1:** delete `algorithm.txt:35` and `:144`.
- [ ] **Step 2:** in `invariants.txt` replace `NeverTornRead == ~tornRead` with:

```tla
\* A classify read found a torn record and judged it uncertain (240.4). A state predicate, not a ghost:
\* the judgement is protocol state, so a misplaced flag cannot make the witness violated for free
\* (cut 6, item 4). It covers the CLASSIFY read only; a takeover's read at S240_5_s5 writes neither
\* seenRec nor classified.
NeverTornRead == ~\E p \in Procs : seenRec[p] = Torn /\ classified[p] = "uncertain"
```

- [ ] **Step 3:** replace the comment at `algorithm.txt:487-491` (the lines that explain the omission through `tornRead`) with:

```tla
           \* A non-record read here (torn, empty, or foreign) continues to step 6, which judges by identity.
           \* No witness distinguishes a torn read from an empty one at this label (cut 6, item 3: a residue).
```

- [ ] **Step 4:** regenerate; `--check-translation` → exit 0.
- [ ] **Step 5: the five `NeverTornRead` witnesses still violate:** `python models/lockproto/run.py --scenario recovery --platform posix`, `... --scenario recovery --platform windows`, `... --scenario breaklock --platform posix`, `... --scenario breaklock --platform windows`, `... --scenario breaklock-remote --platform posix` → each exits 0. Record each changed state count (the ghost's removal can shrink the graphs); Task 13 writes them into `README.md`.
- [ ] **Step 6: mutants.**
  - M-a: in `S240_1_read`'s uncertain arm, change `classified[self] := "uncertain";` to `classified[self] := IF seen = Torn THEN "foreign" ELSE "uncertain";` (a torn record judged foreign, the `SEED_TORN_AS_FOREIGN` shape; do NOT route Torn into the owner-judging `else` arm, which reads `seen.op` and fails with an evaluation error instead). Regenerate; `--scenario recovery --platform posix`. Expected red reason: `recovery-posix-witness-NeverTornRead` reports that `NeverTornRead` was NOT violated.
  - M-b: delete `seenRec[self] := seen;` in `S240_1_read`. Same run, same expected reason.
  Revert each, regenerate, `git diff --stat` shows only Steps 1-3's files.
- [ ] **Step 7: commit** `model: NeverTornRead is a state predicate; the tornRead ghost is gone (cut 6, items 3-4)`.

---

### Task 4: M6 - the always-true guard becomes a safety claim (item 5)

**Files:** `algorithm.txt`, `invariants.txt`, every check config (script), `expected.toml`, `configs/recovery-posix-seeded-SEED_RECOVER_LIVE.cfg` (new), the five `*-witness-NeverRecoveredAfterCrash.cfg` (deleted).

- [ ] **Step 0: verify.** `algorithm.txt:34` is the `recoveredAfterCrash = FALSE,` declaration; `:374` is `           if (victim \in Procs) { if (crashed[victim]) { recoveredAfterCrash := TRUE; }; };`; `Replaceable(p)` is the five-disjunct definition at `:101-106`; `invariants.txt` has `NeverRecoveredAfterCrash == ~recoveredAfterCrash`; `ls configs/*witness-NeverRecoveredAfterCrash.cfg` lists five files, and `expected.toml` has five runs named `*-witness-NeverRecoveredAfterCrash`.
- [ ] **Step 1: MEASURE FIRST - can a live owner's lock reach `S240_3_s5`?** Temporarily (do not commit) add the disjunct and a set-and-halt check: in `Replaceable(p)` add `\/ (SEED_RECOVER_LIVE /\ ownerLive[p] = "live")`; at `S240_3_s5` REPLACE the existing line with `if (victim \in Procs) { if (~crashed[victim]) { recoveredAfterCrash := TRUE; }; };` (replace, not add: the existing line fires on every dead-owner recovery and would make the probe violate for the wrong reason). Regenerate. Copy `configs/recovery-posix-witness-NeverRecoveredAfterCrash.cfg` to `.clavity/scratch/cut6/live-probe.cfg`, set `SEED_RECOVER_LIVE = TRUE` in the copy, and run TLC directly from `models/lockproto`:
  ```bash
  java -XX:+UseParallelGC -cp ../../target/tla/tla2tools.jar tlc2.TLC -workers auto \
       -config ../../.clavity/scratch/cut6/live-probe.cfg LockProtocol
  ```
  Expected if reachable: `Invariant NeverRecoveredAfterCrash is violated.` with a trace whose last state has the victim not crashed. If TLC finishes with no violation, STOP: the seed cannot reach step 5 past a live owner's OS-native lock, and item 5 returns to the owner (report the trace or its absence). Revert all temporary edits.
- [ ] **Step 2:** replace `algorithm.txt:34` with `       replacedLive = FALSE,                      \* ghost: 240.3 replaced a lock whose owner had not crashed`.
- [ ] **Step 3:** replace `:374` with `           if (victim \in Procs) { if (~crashed[victim]) { replacedLive := TRUE; }; };`.
- [ ] **Step 4:** in `Replaceable(p)`, after the `SEED_RECOVER_FOREIGN` disjunct, add:

```tla
                         \/ (SEED_RECOVER_LIVE /\ ownerLive[p] = "live")
```

- [ ] **Step 5:** in `invariants.txt` replace `NeverRecoveredAfterCrash == ~recoveredAfterCrash` with:

```tla
\* 240.3 replaces only a DEAD owner's lock: the lock it moves aside and deletes never names an operation
\* that has not crashed (cut 6, item 5). This replaces the witness NeverRecoveredAfterCrash, whose guard
\* was true wherever it ran and so only restated S240_3_s5's label coverage.
ReplacedOnlyDead == ~replacedLive
```

- [ ] **Step 6: add `ReplacedOnlyDead` to every check run** with a one-off script `.clavity/scratch/cut6/add_invariant.py` (not committed): for each run in `expected.toml` and `expected-extended.toml` whose `kind = "check"` and `module = "LockProtocol"`, open its `config` and insert the line `    ReplacedOnlyDead` after the line `    RefusalJustified` inside its `INVARIANTS` section; assert exactly one insertion per file; print the count. Expected: the count equals `rg -c 'kind = "check"' expected.toml expected-extended.toml` minus the selftest check (verify the arithmetic in the report).
- [ ] **Step 7: retire the five witness runs:** `git rm configs/*-witness-NeverRecoveredAfterCrash.cfg` and delete their five `[[run]]` blocks from `expected.toml`.
- [ ] **Step 8: the seeded run.** Create `configs/recovery-posix-seeded-SEED_RECOVER_LIVE.cfg` as a copy of `configs/recovery-posix-seeded-SEED_CHECK_REFUSES_UNTOUCHED.cfg` with: its header comment replaced by
  ```
  \* recovery scenario, POSIX: SEEDED - 240.3 replaces a live owner's lock (cut 6, item 5), which is what
  \* ReplacedOnlyDead forbids. This run is what proves the claim can fail.
  ```
  `SEED_CHECK_REFUSES_UNTOUCHED = FALSE`, `SEED_RECOVER_LIVE = TRUE`, and `ReplacedOnlyDead` added to its `INVARIANTS` as in Step 6. Add to `expected.toml`, after the `recovery-posix-seeded-SEED_CHECK_REFUSES_UNTOUCHED` block, a copy of that block with `name = "recovery-posix-seeded-SEED_RECOVER_LIVE"`, `config = "configs/recovery-posix-seeded-SEED_RECOVER_LIVE.cfg"`, `violated = ["ReplacedOnlyDead"]`, and in `constants` `SEED_CHECK_REFUSES_UNTOUCHED = false`, `SEED_RECOVER_LIVE = true`.
- [ ] **Step 9:** regenerate; `--check-translation`; `just model-test`; run `--scenario recovery --platform posix` and `--platform windows`, `--scenario mixed --platform posix` and `--platform windows`, `--scenario mixed-remote --platform posix` → all exit 0 (the retired witnesses no longer run; `ReplacedOnlyDead` holds in every check; the seeded run reports it violated).
- [ ] **Step 10: mutants.** M-a: change Step 3's guard to `if (victim \in Procs) { replacedLive := TRUE; };` - expected red: every recovery check reports `ReplacedOnlyDead` violated. M-b: delete Step 3's line - expected red: `recovery-posix-seeded-SEED_RECOVER_LIVE` reports `ReplacedOnlyDead` NOT violated. Revert; regenerate.
- [ ] **Step 11: commit** `model: ReplacedOnlyDead replaces the always-true recoveredAfterCrash witness (cut 6, item 5)`.

---

### Task 5: M7 - the negative control, the `FsInvariants` confirmation, the past seed (item 6)

**Files:** `algorithm.txt`, `invariants.txt` (none), `configs/recovery-posix-hostcrash-control-check.cfg` (new), `configs/recovery-posix-seeded-SEED_FS_PAST_UNALLOCATED.cfg` (new), `expected.toml`.

- [ ] **Step 0: verify.** The host-crash branch begins `             \* The whole host goes down.` then `             await HostCrashes /\ crashes < MaxCrashes;` (`algorithm.txt:1014-1015`). `FsModel.tla` defines `Unflushed(fs)` and `fs.dentries`. `configs/recovery-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg` exists.
- [ ] **Step 1: the guard.** Replace `             await HostCrashes /\ crashes < MaxCrashes;` with:

```tla
             \* HostCrashAtDurableOnly (cut 6, item 6's negative control): only when nothing is unflushed,
             \* so the crash cannot change which object the lock path names.
             await HostCrashes /\ crashes < MaxCrashes
                   /\ (~HostCrashAtDurableOnly \/ (Unflushed(fs) = {} /\ \A d \in Dirs : fs.dentries[d] = fs.entries[d]));
```

  If the translator rejects the two-line `await` (TLC error 2109 is the known hazard for a disjunct continued on the next line, `algorithm.txt:1021-1022`), put the whole condition on one line.
- [ ] **Step 2: the control run.** Create `configs/recovery-posix-hostcrash-control-check.cfg` from `configs/recovery-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg`: header comment
  ```
  \* recovery scenario, POSIX, host crashes only on a durable state: the NEGATIVE CONTROL for the
  \* hostCrashChangedLock ghost (cut 6, item 6). A crash here cannot change the lock, so
  \* NeverHostCrashChangedLock must HOLD; a comparison flipped to fire on every crash violates it.
  ```
  `HostCrashAtDurableOnly = TRUE`, and an `INVARIANTS` section equal to `recovery-posix-check.cfg`'s (the six safety invariants, `ReplacedOnlyDead` if Task 4 landed) plus `NeverHostCrashChangedLock`; keep its `SYMMETRY` line if the witness has one. Add an `expected.toml` block: `name = "recovery-posix-hostcrash-control-check"`, `config` accordingly, `scenario = "recovery"`, `kind = "check"`, `constants` copied from the witness's with `HostCrashAtDurableOnly = true`, `symmetry` copied, `unreached` copied from `recovery-posix-check`, `timeout_minutes = 30`.
- [ ] **Step 3:** regenerate; `--check-translation`; `just model-test`; `python models/lockproto/run.py --scenario recovery --platform posix` → exit 0. Record the control's distinct-state count: expected within a small factor of `recovery-posix-check`'s. If its coverage gate reports labels not covered that `recovery-posix-check` covers, copy that run's `unreached` exactly; if different labels are uncovered, STOP and report.
- [ ] **Step 4: mutant.** Flip `#` to `=` in `hostCrashChangedLock`'s update (`algorithm.txt:1023`). Regenerate; same run. Expected red: `recovery-posix-hostcrash-control-check` reports `NeverHostCrashChangedLock` violated. Revert.
- [ ] **Step 5: `FsInvariants == TRUE` is already killed (confirm, no change).** In `FsModel.tla` change `FsInvariants(fs) == NoDoubleOpen(fs) /\ LockImpliesHandle(fs) /\ IdsNotReused(fs)` to `FsInvariants(fs) == TRUE`; run `--scenario recovery --platform posix`. Expected red: `recovery-posix-seeded-SEED_FS_LOCK_WITHOUT_HANDLE` reports `FsOk` NOT violated. Revert.
- [ ] **Step 6: the past seed.** In `algorithm.txt`, after the `SEED_FS_ALIEN_CONTENT` `or { ... }` block of the environment, add:

```tla
           or {
             \* SEEDED ONLY: a name's past gains an id never allocated, which IdsNotReused's past conjunct
             \* forbids (cut 6, items 6-7). Inert to the protocol under strong identity: `past` is read only
             \* when IdentityStrength = "weak" (FsModel.tla, FsIdentityChoices).
             await SEED_FS_PAST_UNALLOCATED /\ fs.next < MaxObjs;
             fs := [fs EXCEPT !.past[P][ClassOf(LockName)] = @ \cup {fs.next + 1}];
           }
```

- [ ] **Step 7: its run.** Create `configs/recovery-posix-seeded-SEED_FS_PAST_UNALLOCATED.cfg` from `recovery-posix-seeded-SEED_FS_LOCK_WITHOUT_HANDLE.cfg` with `SEED_FS_LOCK_WITHOUT_HANDLE = FALSE`, `SEED_FS_PAST_UNALLOCATED = TRUE` and header `\* recovery, POSIX: SEEDED - an unallocated id in a name's past (cut 6); FsOk must catch it.`; `expected.toml` block copied from `recovery-posix-seeded-SEED_FS_LOCK_WITHOUT_HANDLE` with the name, config, and the two constants changed, `violated = ["FsOk"]`.
- [ ] **Step 8:** regenerate; checks; `--scenario recovery --platform posix` → exit 0.
- [ ] **Step 9: mutant.** In `FsModel.tla`'s `IdsNotReused`, delete the conjunct `/\ \A d \in Dirs, c \in Classes : fs.past[d][c] \subseteq 1..fs.next`. Expected red: `recovery-posix-seeded-SEED_FS_PAST_UNALLOCATED` reports `FsOk` NOT violated. Revert.
- [ ] **Step 10: commit** `model: a negative control for the host-crash ghost, and a seed for IdsNotReused's past half (cut 6, item 6)`.

---

### Task 6: `FsOk`'s other conjuncts (item 7)

**Files:** `algorithm.txt`, two new configs, `expected.toml`.

- [ ] **Step 0: verify** the `SEED_FS_PAST_UNALLOCATED` block from Task 5 is the last seeded `or` block before `\* Nothing crashes after all`.
- [ ] **Step 1:** after it, add:

```tla
           or {
             \* SEEDED ONLY: a second handle record for a process and object it already has open,
             \* differing only in `del`, which NoDoubleOpen forbids (cut 6, item 7). On POSIX the
             \* protocol cannot produce this: `share` forces del = TRUE, so a real double open collapses
             \* into one record (a residue in TODO.md).
             await SEED_FS_DOUBLE_HANDLE;
             with (h \in fs.handles) {
               fs := [fs EXCEPT !.handles = @ \cup {[h EXCEPT !.del = ~h.del]}];
             };
           }
           or {
             \* SEEDED ONLY: an entry names an id never allocated, which IdsNotReused's entries conjunct
             \* forbids (cut 6, item 7).
             await SEED_FS_ENTRY_UNALLOCATED /\ fs.next < MaxObjs;
             fs := [fs EXCEPT !.entries[P][ClassOf(LockName)] = fs.next + 1];
           }
```

- [ ] **Step 2: runs.** Two configs from `recovery-posix-seeded-SEED_FS_LOCK_WITHOUT_HANDLE.cfg` - `recovery-posix-seeded-SEED_FS_DOUBLE_HANDLE.cfg` and `recovery-posix-seeded-SEED_FS_ENTRY_UNALLOCATED.cfg` - each with only its own seed TRUE and a one-line header naming it; two `expected.toml` blocks, `violated = ["FsOk"]`.
- [ ] **Step 3:** regenerate; checks; `--scenario recovery --platform posix` → exit 0.
- [ ] **Step 4: mutants.** M-a: `FsInvariants(fs) == LockImpliesHandle(fs) /\ IdsNotReused(fs)` - expected red: `...-SEED_FS_DOUBLE_HANDLE` reports `FsOk` NOT violated. M-b: in `IdsNotReused` delete the entries conjunct - expected red: `...-SEED_FS_ENTRY_UNALLOCATED` NOT violated. Revert each.
- [ ] **Step 5: commit** `model: one seed per FsOk conjunct (cut 6, item 7)`.

---

### Task 7: `Classifiable`'s seed pins the seed (item 8)

- [ ] **Step 0: verify** the `SEED_FS_ALIEN_CONTENT` block (`fs := [fs EXCEPT !.content[o] = NoProc];`).
- [ ] **Step 1:** after the Task 6 blocks add:

```tla
           or {
             \* SEEDED ONLY: a record-shaped content that is not a record the protocol writes: IsRecord
             \* accepts it, Records does not contain it (cut 6, item 8). A separate run from
             \* SEED_FS_ALIEN_CONTENT, because a seeded run halts at the first violation.
             await SEED_FS_ALIEN_RECORD;
             with (o \in {x \in Objs : fs.content[x] # NoContent}) {
               fs := [fs EXCEPT !.content[o] = [tag |-> "record", op |-> NoProc, kind |-> "operation"]];
             };
           }
```

- [ ] **Step 2:** config `recovery-posix-seeded-SEED_FS_ALIEN_RECORD.cfg` and `expected.toml` block as in Task 6, `violated = ["Classifiable"]`.
- [ ] **Step 3:** regenerate; checks; the recovery POSIX runs → exit 0. If TLC reports an evaluation error comparing records of different shapes, STOP and report the message (the fix is a model decision).
- [ ] **Step 4: mutants.** M-a: `Classifiable == \A o \in Objs : fs.content[o] # NoProc` - expected red: `...-SEED_FS_ALIEN_RECORD` reports `Classifiable` NOT violated. M-b: `Classifiable == \A o \in Objs : fs.content[o] = NoContent \/ fs.content[o] \in {Torn, Foreign, EmptyFile} \/ IsRecord(fs.content[o])` - same expected red. Revert each.
- [ ] **Step 5: commit** `model: a record-shaped alien seed, so Classifiable cannot be gutted to its NoProc case (cut 6, item 8)`.

---

### Task 8: M8 - the liveness consequent (item 9)

- [ ] **Step 0: verify** `rec_decide`'s line `           if (Replaceable(self)) { goto rec_recover; }` (`algorithm.txt:810`) and `configs/recovery-posix-liveness.cfg` (one `PROPERTY DeadLockEventuallyCleared`, no `SYMMETRY`).
- [ ] **Step 1:** replace that line with:

```tla
           \* SEED_RECOVERER_GIVES_UP (cut 6, item 9): it judged the lock replaceable and then does nothing,
           \* refusing nothing - the dead lock stays and no uncertainty is reported.
           if (Replaceable(self)) { if (SEED_RECOVERER_GIVES_UP) { goto rec_end; } else { goto rec_recover; }; }
```

- [ ] **Step 2: the seeded temporal run.** `configs/recovery-posix-seeded-SEED_RECOVERER_GIVES_UP.cfg` = a copy of `configs/recovery-posix-liveness.cfg` with `SEED_RECOVERER_GIVES_UP = TRUE` and header `\* recovery, POSIX: SEEDED - a Recoverer gives up on a dead lock (cut 6, item 9); DeadLockEventuallyCleared must fail. No SYMMETRY (liveness).`. `expected.toml` block after `recovery-posix-liveness`: `name = "recovery-posix-seeded-SEED_RECOVERER_GIVES_UP"`, `kind = "seeded"`, `violated = ["DeadLockEventuallyCleared"]`, `constants` = the liveness run's with `SEED_RECOVERER_GIVES_UP = true`, no `symmetry`, no `unreached`, `timeout_minutes = 30`.
- [ ] **Step 3:** regenerate; checks; `--scenario recovery --platform posix` → exit 0, the new run reporting the property violated. If it reports it NOT violated, the second Recoverer (`r2`) may be clearing the lock: report the result - the spec's "measure" point - and STOP.
- [ ] **Step 4: mutants.** M-a: `DeadLockEventuallyCleared == [](DeadOwnerLock /\ EnvQuiet /\ PendingMover => <>TRUE)`; M-b: `UncertainReported == TRUE`. Expected red for each: the seeded run reports `DeadLockEventuallyCleared` NOT violated. Revert each.
- [ ] **Step 5: commit** `model: a Recoverer that gives up must violate DeadLockEventuallyCleared (cut 6, item 9)`.

---

### Task 9: `RefusalJustified`'s two unseeded sites (item 10)

- [ ] **Step 0: verify** `S99_release_check`'s `           if (~StillOwned(self)) {` (`algorithm.txt:666`) and `S21_1_s3`'s `           if (~StillOwned(self)) {` (`:930`); `configs/breaklock-posix-seeded-SEED_RESTART_RELEASES.cfg` exists (a breaklock seeded template with Breakers).
- [ ] **Step 1:** `:666` → `           if (~StillOwned(self) \/ SEED_RELEASE_CHECK_REFUSES_UNTOUCHED) {`; `:930` → `           if (~StillOwned(self) \/ SEED_RESTART_CHECK_REFUSES_UNTOUCHED) {`.
- [ ] **Step 2: runs.** `recovery-posix-seeded-SEED_RELEASE_CHECK_REFUSES_UNTOUCHED` from `recovery-posix-seeded-SEED_CHECK_REFUSES_UNTOUCHED` (config and block; that seed FALSE, the new one TRUE); `breaklock-posix-seeded-SEED_RESTART_CHECK_REFUSES_UNTOUCHED` from `breaklock-posix-seeded-SEED_RESTART_RELEASES` (that seed FALSE, the new one TRUE). Both `violated = ["RefusalJustified"]`, one-line headers naming the site.
- [ ] **Step 3:** regenerate; checks; `--scenario recovery --platform posix` and `--scenario breaklock --platform posix` → exit 0.
- [ ] **Step 4: mutants.** Hardwire `refusedOk[self] := TRUE;` at `S99_release_check`'s refusal (was `:= lostLock[self]`): expected red `...-SEED_RELEASE_CHECK_REFUSES_UNTOUCHED` NOT violated. The same at `S21_1_s3`: expected red `...-SEED_RESTART_CHECK_REFUSES_UNTOUCHED`. Revert each.
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

- [ ] **Step 2:** regenerate; `--check-translation` → exit 0 (comment only; no run changes).
- [ ] **Step 3: commit** `model: PlainNeverOwnsUncertain claims only the move-aside it instruments (cut 6, item 11)`.

---

### Task 11: `ForeignUntouched`'s content half (item 12)

- [ ] **Step 0: verify** `S21_1_restart_decide`'s `           else if (classified[self] = "foreign") { refused[self] := "CONTROL_PLANE_NAMESPACE_CONFLICT"; }` (`algorithm.txt:915`), and that the breaklock scenario's Init can start with a `Foreign` object at the lock path: `rg -n "Foreign" algorithm.txt | head` and read the Init choice of `fs`; if breaklock cannot start from a `Foreign` lock, STOP and report (the seed cannot reach its target).
- [ ] **Step 1:** replace that line with:

```tla
           \* SEED_TAKEOVER_FOREIGN (cut 6, item 12): --break-lock takes over an object that is not a lock.
           else if (classified[self] = "foreign") {
             if (SEED_TAKEOVER_FOREIGN) { goto brk_takeover; }
             else { refused[self] := "CONTROL_PLANE_NAMESPACE_CONFLICT"; };
           }
```

- [ ] **Step 2: run** `breaklock-posix-seeded-SEED_TAKEOVER_FOREIGN` from `breaklock-posix-seeded-SEED_RESTART_RELEASES` (that seed FALSE, this TRUE), `violated = ["ForeignUntouched"]`.
- [ ] **Step 3:** regenerate; checks; `--scenario breaklock --platform posix` → exit 0. If the run reports another invariant violated first, report which and its trace - the seed then needs a narrower placement (driver decision).
- [ ] **Step 4: mutant.** `ForeignUntouched == foreignObj # NoObj => LockObj = foreignObj` (content conjunct dropped): expected red, the seeded run reports `ForeignUntouched` NOT violated. Revert.
- [ ] **Step 5: commit** `model: a measured seed for ForeignUntouched's content half (cut 6, item 12)`.

---

### Task 12: weak identity - runs and the `NeverSecondPastId` witness (item 13)

- [ ] **Step 0: verify** `configs/recovery-posix-check.cfg` and `configs/mixed-posix-check.cfg` exist with `IdentityStrength = "strong"`, and `FsModel.tla:135` adds to `past` under weak identity.
- [ ] **Step 1: the witness.** In `invariants.txt`, after `NeverHostCrashChangedLock`, add:

```tla
\* Weak identity was exercised: some name's past holds a second id, which only a re-created name under
\* IdentityStrength = "weak" produces (FsCreate, FsModel.tla). Its weak runs must stop with it violated;
\* without it, gutting `past` to {} would reduce weak identity to strong and change nothing any run checks
\* (cut 6, item 13).
NeverSecondPastId == \A d \in Dirs, c \in Classes : Cardinality(fs.past[d][c]) <= 1
```

- [ ] **Step 2: configs.** `recovery-posix-weak-check.cfg` and `mixed-posix-weak-check.cfg` from the two strong checks with `IdentityStrength = "weak"`; `recovery-posix-weak-witness-NeverSecondPastId.cfg` from `recovery-posix-witness-NeverTornRead.cfg`, and `mixed-posix-weak-witness-NeverSecondPastId.cfg` from `mixed-posix-witness-NeverUncertainLockWithPendingBreaker.cfg` (keep each template's CONSTANTS and SYMMETRY; replace its invariant section with `INVARIANT NeverSecondPastId` only; drop any PROPERTY), `IdentityStrength = "weak"`.
- [ ] **Step 3: expected entries** for the four runs (check: `kind = "check"`, copy the strong check's `constants`, `symmetry` and `unreached` with `IdentityStrength = "weak"`, `timeout_minutes = 60`; witness: `kind = "witness"`, `violated = ["NeverSecondPastId"]`, `timeout_minutes = 30`).
- [ ] **Step 4: MEASURE FIRST.** Regenerate; checks; run `--scenario recovery --platform posix` then `--scenario mixed --platform posix`. For each weak check record distinct states and whether it finished within its timeout; for each witness, that it stops violated. If a weak check times out, STOP and report its last progress line - the spec's split-or-narrow decision is the owner's. If a weak check's coverage differs from its strong twin's, set its `unreached` to what the run reports and list the difference in the report.
- [ ] **Step 5: mutant.** In `FsModel.tla`, change all three `IF IdentityStrength = "weak" THEN @ \cup {o} ELSE @` to `@`. Expected red: both `*-weak-witness-NeverSecondPastId` report it NOT violated. Revert.
- [ ] **Step 6: commit** `model: weak-identity checks and the NeverSecondPastId witness (cut 6, item 13)`.

---

### Task 13: bookkeeping - `TODO.md`, the README, the design doc

- [ ] **Step 1: `TODO.md`.** Close (`[x]`, one line each, naming the commit) the entries: M5, M6, M7, M8, "`tornRead` is set at one of the two labels", "`FsOk`'s other two conjuncts", "`Classifiable`'s seed pins the seed", "Two of `RefusalJustified`'s three `lostLock` sites", "`PlainNeverOwnsUncertain`'s ghost is wired to one label" (as narrowed), "`ForeignUntouched`'s content half", "`IdentityStrength = "weak"` is dead" - EXCEPT any item a Task STOPPED on, whose entry is rewritten with what the measurement showed and left open. Add residues: POSIX double open invisible to `NoDoubleOpen`; M8 widening admitting only `"RESTART"`; `PlainNeverOwnsUncertain` covers only the move-aside; torn versus empty at takeover. Leave items 1, 2 and 6's union entries for Part 2.
- [ ] **Step 2: `models/lockproto/README.md`.** Replace every state count Tasks 3-12 changed with the measured value; correct `:144`'s "identical state graph ... no name in this scenario is reused" with Task 12's measurement; list the new and retired runs where the README lists runs; fix `:90-91`'s `deferred` sentence to name only `S96_1_backoff` (verified anomaly).
- [ ] **Step 3: the design doc.** In `docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md`, correct the weak-identity rationale near `:1125` and the "FsOk and Classifiable are exempt" sentence near `:775` (both verified anomalies), each with one line citing cut 6.
- [ ] **Step 4:** `just check` → exit 0 (the typos gate covers these files); `just model-test` → `OK`.
- [ ] **Step 5: commit** `docs: cut 6 Part 1 bookkeeping - TODO closures and residues, README counts, design doc corrections`.

---

### Task 14: Part 1 verification (no publishing)

- [ ] **Step 1:** `python models/lockproto/run.py --check-translation` → exit 0; `just model-test` → `OK`; `just check` → exit 0.
- [ ] **Step 2:** a full local `python models/lockproto/run.py` is long; instead push NOTHING and ask the driver to trigger the CI Model run on the branch (the driver owns publishing). Its run ID and every changed state count go into the execution notes.
- [ ] **Step 3: STOP.** Part 2 is written next, against Task 1's findings.

## Self-review (done by the plan author)

- **Spec coverage:** item 1 → Task 1 (measurement; the rest is Part 2); item 2 → Part 2; item 3 → Task 3 (comment, narrowing) + Part 2 (entry); item 4 → Task 3; item 5 → Task 4; item 6 → Task 5 (control, `FsInvariants`, past seed) + Part 2 (extended-tier entries and job); item 7 → Task 6; item 8 → Task 7; item 9 → Task 8; item 10 → Task 9; item 11 → Task 10; item 12 → Task 11; item 13 → Task 12; residues → Task 13; "every closure proven by its mutant for the expected reason" → each task's mutant step; "scripted edit of every new constant" → Task 2.
- **Placeholders:** none; the two one-off scripts are given in full or specified by exact rule and asserted counts.
- **Consistency:** constant names are the ten of Task 2 wherever used; run names follow `<scenario>-<variant>-<kind>[-<SEED>]`; `ReplacedOnlyDead`, `NeverSecondPastId`, `HostCrashAtDurableOnly` are spelled the same in every task.
