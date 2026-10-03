# Cut 6 Part 2: the branch-granular union and M7's remaining halves - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close cut 6's items 1, 2, 3 and 6: make the suite prove that two arms INSIDE the takeover's labels run, and
that a host crash can revert an unflushed lock record, each with a mutant CI kills.

**Architecture:** A top-level `branches` list in `models/lockproto/expected.toml` names an `IF` arm of the GENERATED
`LockProtocol.tla`, which `--check-translation` pins. The runner resolves each entry at load time to a source span.
`--union-from` counts that span's first TLC cost node (message 2221) over the logs of runs that model the protocol as
specified; a zero sum fails. Item 6 has no node to count, so it gets a single-Owner witness run instead. The mutant
manifest gains a third expectation, `expect_zero_branch`.

**Tech Stack:** Python 3.14 (`run.py`, `unittest`), TLA+/PlusCal (`algorithm.txt`, `invariants.txt`, `FsModel.tla`),
TLC 2.19 on GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-09-26-cut-6-lock-model-debt-design.md`, items 1-3 and 6. Part 1:
`docs/superpowers/plans/2026-09-27-cut-6-part-1-lock-model-debt.md` (its revision-3 rules apply here). Task 1's fixture
findings: `docs/superpowers/plans/2026-09-27-cut-6-arm-coverage-measurement.md`.

**Written against:** `spec/seq-after-cut5` at `d0fc11e`. Every code block below was applied to a scratch copy of
`models/lockproto` at that commit and run there: the new tests pass, the existing suite passes, and
`run.py --union-from` over the real logs of CI Model run 36506185161 prints the two `BRANCH` lines quoted in Task 3.

---

## What the measurements showed (the plan rests on these)

From CI Model run 36506185161 (commit `0e4e2cc`; the generated module is unchanged in the lines below at `d0fc11e`)
and the nightly extended run 36406126977:

- **Item 2** - `S240_5_s6`'s `IF ident # tobj[self]` THEN arm (`LockProtocol.tla:2224-2231`), whose first node is
  `:2225` col 58. Its count is 0 in every strong-capability run. It is above 0 in `breaklock-remote-posix-check`
  (58,956), `breaklock-remote-posix-plain-check` (19,620) and `mixed-remote-posix-check` (25,470): 104,046 in total
  over the runs that count.
- **Item 3** - `S240_5_s5`'s `IF IsRecord(seen)` (`:2108`) ELSE arm, the non-record continue, whose first node is
  `:2119` col 58: 836,528 over the runs that count.
- **Item 6** - no node exists for any line of `FsHostCrash`'s `LET newContent(o)` or its `CASE` arms
  (`FsModel.tla:312-318`), at either call site (`algorithm.txt:1026-1027`), in any of the 8 extended host-crash
  check logs. TLC reports the whole `[ fs EXCEPT ... ]` (`FsModel.tla:319-327`) as one node. So no branch entry can
  see those arms.
- **The FIX flag lives in the config.** `breaklock-remote-posix-fixed-check`'s `constants` in expected.toml do not
  include `FIX_REMOTE_LEASE_SPEC`; its config sets it (`configs/breaklock-remote-posix-fixed-check.cfg:53`). A filter
  over `constants` counted the three FIX runs (157,452 instead of 104,046). The filter reads the config.

## Where this plan refines the spec (declared, not silent)

1. **Item 6 is closed by a WITNESS, not a branch entry** (owner, 2026-09-29, after three AGY-FIRST negotiation rounds;
   agy held RESIDUE for two rounds and agreed WITNESS at the third). The arms have no coverage node (above). Pairing:
   one Owner and no other actor, `HostCrashes = TRUE`. Witness: `NeverRevertedOwnWrite`, which must be violated. It is
   sound only with a single Owner. `Acquire` is called once, from `own_start` (`algorithm.txt:715-732`); it is the only
   creator in this pairing, and creation is the only writer of `EmptyFile` (`FsModel.tla:128-137`). The Owner never
   flushes the lock file: the one `FsFlushFile` is the takeover's, `algorithm.txt:535`. The spec's other-module entries
   (`module`/`operator`), the `tier` key and the new extended-tier union job are dropped: nothing would use them. The
   torn arm stays a residue (owner, Part 1).
2. **An entry is `{ name, label, guard, side, reason }`, plus an optional `module`** (default `LockProtocol`),
   instead of `{ label, arm, reason }` (owner-approved, agy aligned):
   - `guard` is the WHOLE condition of the `IF`, as pcal.trans printed it. `IsRecord(seen)` is also a PREFIX of
     `:2104`'s condition, so a prefix match would be ambiguous.
   - `side` says which arm is counted.
   - `name` lets a mutant refer to the entry.
3. **Runs that count:** not seeded, and not a run whose config sets a `FIX_*` flag TRUE. Check, liveness and witness
   runs count.
4. **`parse_cost_nodes` keeps its 5-tuple.** An arm is counted by the first node whose START lies inside the arm's
   span, so a node's end position is never needed (the spec asked for the full span).
5. **Item 2's mutant swallows the arm through the first `IF`:** `if (ident = NoObj \/ ident # tobj)`. The spec's
   `IF ident # tobj[self] /\ FALSE` changes the anchored guard text, so the entry would fail at load: an error, not a
   kill.
6. **A branch mutant is killed only by a run that FINISHES with the arm at zero.** A run that halts at a violation
   has a prefix of the coverage, and a zero in a prefix proves nothing.

## Ground rules (Part 1, revision 3)

- Worktree `E:\Rust\flux-engine`, branch `spec/seq-after-cut5`. No push until Task 7 (the owner approved branch
  pushes, no PR).
- **No local TLC** (`java ... tlc2.TLC`, `run.py --scenario`, `run.py --mutants`). The only Java allowed locally is
  `pcal.trans` (regenerate below). CI judges every run and mutant.
- **Step 0 - state verification.** Before each edit, confirm the quoted current text. If it differs, STOP and report
  `STATE_MISMATCH: <file>: <what differs>`.
- **Shape-divergence stop.** A name, key, value, message or signature different from this plan: STOP and report
  `[plan] -> [yours] because <reason>`.
- **Regenerate** after editing `algorithm.txt` or `invariants.txt`, from `models/lockproto` in Git Bash:
  `cat LockProtocol.head algorithm.txt invariants.txt > LockProtocol.tla && java -cp ../../target/tla/tla2tools.jar pcal.trans LockProtocol.tla && rm -f LockProtocol.cfg LockProtocol.old`,
  then `python run.py --check-translation` prints `run.py: LockProtocol.tla matches its sources`.
- **Local gates** (from `E:/Rust/flux-engine`):
  - `python models/lockproto/run.py --check-translation`, exit 0;
  - `python models/lockproto/run.py --list-jobs`, exit 0 (loading validates `branches`);
  - `python models/lockproto/run.py --list-mutants models/lockproto/mutants.toml`, exit 0;
  - `just model-test`, final line `OK`;
  - `just check`, exit 0, before any commit that touches docs.
- **Commits** add named paths only; `git status --short` shows no `*.old` and no `states/`. Each commit message ends
  with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` (a subagent signs with its own model and says so).
- **Oracle:** the tests in each task are already written - implement until they pass. Never edit a test to make it
  pass; a test that looks wrong is reported, not changed.

## File map

| File | Change |
|---|---|
| `models/lockproto/run.py` | `Branch`, `resolve_branch`, `arm_count`, `_load_branches`, `counts_for_branches`, `judge_branches`; wiring in `load_expected`, `judge_union`, `judge_union_from`, `main`; `expect_zero_branch` in the mutant loader and runner |
| `models/lockproto/test_run.py` | `BranchResolveTests`, `BranchJudgeTests`, two `MutantManifestTests` methods |
| `models/lockproto/expected.toml` | two `[[branches]]` (Task 3); one witness `[[run]]` (Task 5) |
| `models/lockproto/mutants.toml` | four `[[mutant]]` (Tasks 4, 5) |
| `models/lockproto/invariants.txt`, `LockProtocol.tla` | `NeverRevertedOwnWrite` (Task 5) |
| `models/lockproto/configs/recovery-posix-owneronly-hostcrash-witness-NeverRevertedOwnWrite.cfg` | new (Task 5) |
| `TODO.md`, `models/lockproto/README.md` | Task 6 |

---

### Task 1: resolve an `IF` arm of the generated module

**Files:** Modify `models/lockproto/run.py` (two insertions); test `models/lockproto/test_run.py`.

- [ ] **Step 0: verify.** `run.py` has `@dataclass(frozen=True)\nclass Expected:` (~:127) and the line
  `SOURCES = ("LockProtocol.head", "algorithm.txt", "invariants.txt")` (~:639), each once. `test_run.py` has
  `class PartialCoverageTests(unittest.TestCase):` once and imports `contextlib` on its own line.
  `testdata/ArmFixture.tla` and `testdata/arm_coverage.out` exist.

- [ ] **Step 1: add the tests.** In `test_run.py`, after `import contextlib` add `import dataclasses` (used in
  Task 2). Immediately BEFORE `class PartialCoverageTests(unittest.TestCase):` insert:

````python
def text_at(module_text: str, pos: tuple[int, int]) -> str:
    """The module text from a 1-based (line, col) position to the end of that line."""
    line, col = pos
    return module_text.split("\n")[line - 1][col - 1:]


# A generated-module shape with the two traps the resolver must avoid: a guard that is a PREFIX of another
# guard in the same label (A), and the same guard in another label (B); C holds one guard twice.
SYNTHETIC_ACTIONS = "\n".join([
    'A(self) == /\\ pc[self] = "A"',
    "           /\\ IF IsRecord(seen) /\\ seen # s",
    "                 THEN /\\ y' = 1",
    "                 ELSE /\\ IF IsRecord(seen)",
    "                            THEN /\\ y' = 2",
    "                            ELSE /\\ y' = 3",
    "           /\\ UNCHANGED z",
    "",
    "B(self) == /\\ IF IsRecord(seen)",
    "                 THEN /\\ y' = 4",
    "                 ELSE /\\ y' = 5",
    "",
    "C(self) == /\\ IF x = 1",
    "                 THEN /\\ y' = 6",
    "                 ELSE /\\ IF x = 1",
    "                            THEN /\\ y' = 7",
    "                            ELSE /\\ y' = 8",
    "",
])


class BranchResolveTests(unittest.TestCase):
    """resolve_branch()/arm_count() (cut 6 Part 2): an arm of an IF inside a label, counted from TLC's cost nodes.

    The recorded fixture is Task 1's measurement (testdata/arm_coverage.out, from ArmFixture.tla); its counts
    are what TLC printed, not what this code expects."""

    def setUp(self) -> None:
        self.module = (TESTDATA / "ArmFixture.tla").read_text(encoding="utf-8")
        self.nodes = run.parse_cost_nodes(run.parse_messages(fixture("arm_coverage")[1]))
        self.assertIsNotNone(self.nodes)

    def count(self, label: str, guard: str, side: str) -> int | None:
        span = run.resolve_branch(self.module, label, guard, side)
        branch = run.Branch("b", "ArmFixture", label, guard, side, "why", span)
        return run.arm_count(self.nodes, branch)

    def test_the_dead_arm_counts_zero_and_a_live_arm_does_not(self) -> None:
        self.assertEqual(self.count("branchy", "i = 7", "then"), 0, "x := 99 never runs (arm_coverage.out:69)")
        self.assertEqual(self.count("branchy", "i = 1", "then"), 1)

    def test_an_arm_producing_only_duplicate_successors_still_counts(self) -> None:
        # twins: both arms set y' = 5 (arm_coverage.out:96, :102)
        self.assertEqual(self.count("twins", "j = 0", "then"), 3)
        self.assertEqual(self.count("twins", "j # 0", "then"), 3)

    def test_the_translator_fallback_else_counts_its_first_conjunct(self) -> None:
        # `ELSE /\ TRUE` has no node on its own line; its arm's first node is `y' = y` (arm_coverage.out:105)
        self.assertEqual(self.count("twins", "j # 0", "else"), 0)

    def test_the_whole_condition_must_match_not_a_prefix(self) -> None:
        inner = run.resolve_branch(SYNTHETIC_ACTIONS, "A", "IsRecord(seen)", "else")
        self.assertEqual(text_at(SYNTHETIC_ACTIONS, inner[0]), "ELSE /\\ y' = 3")
        outer = run.resolve_branch(SYNTHETIC_ACTIONS, "A", "IsRecord(seen) /\\ seen # s", "then")
        self.assertEqual(text_at(SYNTHETIC_ACTIONS, outer[0]), "THEN /\\ y' = 1")
        self.assertEqual(text_at(SYNTHETIC_ACTIONS, outer[1]), "ELSE /\\ IF IsRecord(seen)")
        with self.assertRaises(run.ExpectedError):
            run.resolve_branch(SYNTHETIC_ACTIONS, "A", "IsRecord", "then")

    def test_the_same_guard_in_another_label_is_not_a_match(self) -> None:
        span = run.resolve_branch(SYNTHETIC_ACTIONS, "B", "IsRecord(seen)", "then")
        self.assertEqual(text_at(SYNTHETIC_ACTIONS, span[0]), "THEN /\\ y' = 4")

    def test_an_else_arm_ends_where_the_action_dedents(self) -> None:
        start, end = run.resolve_branch(SYNTHETIC_ACTIONS, "A", "IsRecord(seen)", "else")
        self.assertEqual(end, (start[0] + 1, 1), "the ELSE arm stops at the next line indented no deeper")

    def test_every_unresolvable_anchor_fails_closed(self) -> None:
        cases = {
            "guard twice in one label": ("C", "x = 1", "then"),
            "no such guard": ("A", "x = 1", "then"),
            "no such label": ("D", "x = 1", "then"),
        }
        for why, (label, guard, side) in cases.items():
            with self.subTest(why):
                with self.assertRaises(run.ExpectedError):
                    run.resolve_branch(SYNTHETIC_ACTIONS, label, guard, side)

    def test_a_log_with_no_node_inside_the_arm_gives_none(self) -> None:
        span = run.resolve_branch(self.module, "branchy", "i = 7", "then")
        branch = run.Branch("b", "ArmFixture", "branchy", "i = 7", "then", "why", span)
        self.assertIsNone(run.arm_count([n for n in self.nodes if n[1] != 51], branch))
        self.assertIsNone(run.arm_count(self.nodes, run.Branch("b", "Other", "branchy", "i = 7", "then", "why", span)),
                          "a node of another module never counts")


````

- [ ] **Step 2: run them - they fail.**
  `cd models/lockproto && python -m unittest test_run.BranchResolveTests` → errors: `module 'run' has no attribute 'resolve_branch'`.

- [ ] **Step 3: implement.** In `run.py`, immediately BEFORE `@dataclass(frozen=True)\nclass Expected:` insert:

````python
BRANCH_KEYS = frozenset({"name", "module", "label", "guard", "side", "reason"})


@dataclass(frozen=True)
class Branch:
    """One arm the suite must be seen to run (cut 6 Part 2), resolved at load time against the generated module."""
    name: str
    module: str
    label: str
    guard: str
    side: str  # "then" or "else"
    reason: str
    span: tuple[tuple[int, int], tuple[int, int]]


````

  and immediately BEFORE `SOURCES = ("LockProtocol.head", "algorithm.txt", "invariants.txt")` insert:

````python
# One arm of a translated `IF` inside a label's action (cut 6 Part 2). pcal.trans prints `THEN` and `ELSE`
# three columns right of their `IF`, which is what locates an arm without parsing TLA+.
_IF_CONDITION = re.compile(r"(?<![A-Za-z0-9_])IF (.*\S)\s*$")


def resolve_branch(module_text: str, label: str, guard: str, side: str) -> tuple[tuple[int, int], tuple[int, int]]:
    """Where one arm of an `IF` in `label`'s action lies in the GENERATED module: (start, end) as 1-based
    (line, col) positions, end exclusive - the coordinates of TLC's cost nodes (message 2221).

    `guard` is the WHOLE condition of the `IF`, exactly as pcal.trans printed it, so `IsRecord(seen)` does not
    match `IsRecord(seen) /\\ seen # seenRec[self]`; only the named label's action is searched, so the same
    guard in another label is no match. The THEN arm runs to its ELSE; the ELSE arm to the first line indented
    no deeper than that ELSE. A label that is not exactly one action, a guard that is not exactly one `IF`, or
    no THEN/ELSE where the translator puts them: ExpectedError - an anchor never resolves to nothing or to two
    places."""
    lines = module_text.split("\n")
    heads = [i for i, text in enumerate(lines) if re.match(rf"{re.escape(label)}(\(self\))? ==", text)]
    if len(heads) != 1:
        raise ExpectedError(f"label {label!r}: {len(heads)} action definitions in the generated module (must be one)")
    start = heads[0]
    end = next((i for i in range(start + 1, len(lines)) if lines[i][:1] not in ("", " ")), len(lines))
    hits = [(i, m.start()) for i in range(start, end)
            for m in _IF_CONDITION.finditer(lines[i]) if m.group(1) == guard]
    if len(hits) != 1:
        raise ExpectedError(f"label {label!r}: guard {guard!r} matches {len(hits)} IFs (must be exactly one)")
    line_if, col_if = hits[0]
    col_kw = col_if + 3

    def indent(text: str) -> int:
        return len(text) - len(text.lstrip())

    def keyword(word: str, after: int) -> int | None:
        for j in range(after + 1, end):
            if lines[j][:col_kw].strip() == "" and lines[j][col_kw:].startswith(word + " "):
                return j
            if lines[j].strip() and indent(lines[j]) <= col_if:
                return None
        return None

    line_then = keyword("THEN", line_if)
    line_else = keyword("ELSE", line_then) if line_then is not None else None
    if line_then is None or line_else is None:
        raise ExpectedError(f"label {label!r}: guard {guard!r} has no THEN/ELSE at column {col_kw + 1}")
    if side == "then":
        return (line_then + 1, col_kw + 1), (line_else + 1, col_kw + 1)
    after = next((j for j in range(line_else + 1, end) if lines[j].strip() and indent(lines[j]) <= col_kw), end)
    return (line_else + 1, col_kw + 1), (after + 1, 1)


def arm_count(nodes: list[tuple[str, int, int, int, int]], branch: Branch) -> int | None:
    """The count of the FIRST cost node inside the branch's arm - how often the arm was entered - or None when
    this log has no node there. First by position; a child node sharing its parent's start comes after it."""
    start, end = branch.span
    inside = [n for n in nodes if n[0] == branch.module and start <= (n[1], n[2]) < end]
    if not inside:
        return None
    return min(inside, key=lambda n: (n[1], n[2]))[3]


````

- [ ] **Step 4: run them - they pass.** `python -m unittest test_run.BranchResolveTests` → `Ran 8 tests ... OK`.
  Then `just model-test` → `OK`.
- [ ] **Step 5: commit** `models/lockproto/run.py`, `models/lockproto/test_run.py`:
  `model: resolve an IF arm of the generated module for the branch union (cut 6 Part 2, item 1)`.

---

### Task 2: `branches` in expected.toml, loaded fail-closed and judged by the union

**Files:** Modify `models/lockproto/run.py`; test `models/lockproto/test_run.py`.

- [ ] **Step 0: verify** these texts occur once each in `run.py`:
  - `    deferred: tuple[tuple[str, str, str], ...]\n\n\n@dataclass(frozen=True)\nclass Message:` (the end of
    `Expected`);
  - `def load_expected(path: Path) -> Expected:`;
  - the two-line top-level-keys `_require` quoted in Step 3;
  - `    return Expected(scenarios, runs, never_reached, deferred)`;
  - `def judge_union(executed:`;
  - `                 deferred: tuple[tuple[str, str, str], ...]) -> bool:` (judge_union's last parameter);
  - `    return union_from_logs(entries, base, never_reached, deferred, partial)` (judge_union's return);
  - in `judge_union_from`, `    failed = union_from_logs(entries, base, expected.never_reached, expected.deferred, partial)`
    followed by `    return 1 if failed else 0`;
  - in `main`, `    elif judge_union(executed, base, expected.never_reached, expected.deferred):`.

- [ ] **Step 1: add the tests.** In `test_run.py`, immediately BEFORE `class PartialCoverageTests(unittest.TestCase):`
  (after Task 1's class) insert:

````python
BRANCHES_TOML = """

[[branches]]
name = "{name}"
module = "ArmFixture"
label = "branchy"
guard = "{guard}"
side = "then"
reason = "a test"
"""


class BranchJudgeTests(unittest.TestCase):
    """`branches` in expected.toml: loaded fail-closed, judged by --union-from over the runs that count."""

    def expected_dir(self, extra: str) -> ExpectedDir:
        d = ExpectedDir(GOOD_EXPECTED + extra)
        self.addCleanup(d.close)
        (d.path / "ArmFixture.tla").write_text((TESTDATA / "ArmFixture.tla").read_text(encoding="utf-8"),
                                                encoding="utf-8")
        return d

    def logs(self, per_run: dict[str, str]) -> Path:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name) / "tlc-output-x-posix"
        root.mkdir()
        for name, text in per_run.items():
            (root / f"{name}.log").write_text(text, encoding="utf-8")
        return root.parent

    def judge(self, d: ExpectedDir, per_run: dict[str, str]) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = run.judge_union_from(d.load(), d.path, self.logs(per_run))
        return code, out.getvalue()

    def test_a_branch_loads_resolved(self) -> None:
        (b,) = self.expected_dir(BRANCHES_TOML.format(name="dead", guard="i = 7")).load().branches
        self.assertEqual((b.name, b.module, b.label, b.side), ("dead", "ArmFixture", "branchy", "then"))
        self.assertEqual(b.span, ((51, 42), (52, 42)))

    def test_each_malformed_branch_fails_at_load(self) -> None:
        good = BRANCHES_TOML.format(name="dead", guard="i = 7")
        cases = {
            "unknown key": good + 'extra = "no"\n',
            "missing reason": good.replace('reason = "a test"\n', ""),
            "bad side": good.replace('side = "then"', 'side = "both"'),
            "bad name": good.replace('name = "dead"', 'name = "Dead Arm"'),
            "missing module": good.replace('module = "ArmFixture"', 'module = "Nope"'),
            "unresolvable guard": good.replace('guard = "i = 7"', 'guard = "i = 9"'),
            "duplicate name": good + good,
        }
        for why, extra in cases.items():
            with self.subTest(why):
                with self.assertRaises(run.ExpectedError):
                    self.expected_dir(extra).load()

    def test_an_arm_one_counted_log_covers_passes(self) -> None:
        d = self.expected_dir(BRANCHES_TOML.format(name="live", guard="i = 1"))
        covered = fixture("arm_coverage")[1]
        zeroed = covered.replace("line 49, col 36 to line 49, col 44 of module ArmFixture: 1",
                                 "line 49, col 36 to line 49, col 44 of module ArmFixture: 0")
        self.assertNotEqual(covered, zeroed, "the replaced node line must exist, or this test asserts nothing")
        names = [r.name for r in d.load().runs]
        code, out = self.judge(d, {n: (covered if n == "demo-posix-check" else zeroed) for n in names})
        self.assertEqual(code, 0, out)
        self.assertIn("BRANCH live  1 entries", out)

    def test_an_arm_no_counted_log_covers_fails(self) -> None:
        d = self.expected_dir(BRANCHES_TOML.format(name="dead", guard="i = 7"))
        code, out = self.judge(d, {r.name: fixture("arm_coverage")[1] for r in d.load().runs})
        self.assertEqual(code, 1)
        self.assertIn("MISMATCH branch dead", out)

    def test_a_seeded_run_covering_the_arm_does_not_count(self) -> None:
        d = self.expected_dir(BRANCHES_TOML.format(name="live", guard="i = 1"))
        covered = fixture("arm_coverage")[1]
        zeroed = covered.replace("line 49, col 36 to line 49, col 44 of module ArmFixture: 1",
                                 "line 49, col 36 to line 49, col 44 of module ArmFixture: 0")
        runs = d.load().runs
        seeded = [r.name for r in runs if r.kind == "seeded"]
        self.assertEqual(len(seeded), 1)
        code, out = self.judge(d, {r.name: (covered if r.kind == "seeded" else zeroed) for r in runs})
        self.assertEqual(code, 1, "only a seeded run reached the arm, and seeded runs do not count")
        self.assertIn("MISMATCH branch live", out)

    def test_which_runs_count(self) -> None:
        d = self.expected_dir("")
        runs = {r.kind: r for r in d.load().runs}
        self.assertTrue(run.counts_for_branches(runs["check"], d.path), "check.cfg sets FIX_SAFE = FALSE")
        self.assertTrue(run.counts_for_branches(runs["witness"], d.path))
        self.assertFalse(run.counts_for_branches(runs["seeded"], d.path))
        # The FIX_* flag is read from the CONFIG: expected.toml's `constants` does not carry it (measured on
        # breaklock-remote-posix-fixed-check, whose constants omit FIX_REMOTE_LEASE_SPEC).
        (d.path / "fixed.cfg").write_text("SPECIFICATION Spec\nCONSTANTS\n    FIX_SAFE = TRUE\nINVARIANT Safe\n",
                                          encoding="utf-8")
        fixed = dataclasses.replace(runs["check"], config="fixed.cfg")
        self.assertFalse(run.counts_for_branches(fixed, d.path), "a run whose config sets FIX_* models a fix")

    def test_no_branches_changes_nothing(self) -> None:
        self.assertFalse(run.judge_branches((), [("any", "not even a log")]))


````

- [ ] **Step 2: run them - they fail** (`python -m unittest test_run.BranchJudgeTests`: `Expected` has no
  `branches`, or `unknown top-level keys: ['branches']`).

- [ ] **Step 3: implement**, in `run.py`:
  1. End of `Expected`: replace
     `    deferred: tuple[tuple[str, str, str], ...]\n\n\n@dataclass(frozen=True)\nclass Message:` with
     `    deferred: tuple[tuple[str, str, str], ...]\n    branches: tuple[Branch, ...] = ()\n\n\n@dataclass(frozen=True)\nclass Message:`.
  2. Immediately BEFORE `def load_expected(path: Path) -> Expected:` insert:

````python
def _load_branches(value: object, base: Path) -> tuple[Branch, ...]:
    """Validate expected.toml's `branches` and resolve each anchor. Unknown or missing keys, a bad name or side,
    a module that does not exist, or an anchor that resolves to nothing or to two places all fail at load."""
    _require(isinstance(value, list), "'branches' must be an array of tables")
    assert isinstance(value, list)
    branches: list[Branch] = []
    for i, e in enumerate(value):
        where = f"'branches' #{i + 1}"
        _require(isinstance(e, dict), f"{where} must be a table")
        assert isinstance(e, dict)
        required = BRANCH_KEYS - {"module"}
        _require(set(e) <= BRANCH_KEYS and required <= set(e),
                 f"{where}: keys are name, label, guard, side, reason and optionally module; got {sorted(e)}")
        for key, v in e.items():
            _require(isinstance(v, str) and v != "", f"{where}: '{key}' must be a non-empty string")
        name, module = e["name"], e.get("module", "LockProtocol")
        where = f"'branches' {name!r}"
        _require(re.fullmatch(r"[a-z0-9]+(-[a-z0-9]+)*", name) is not None,
                 f"{where}: a name is lower-case words joined by '-'")
        _require(e["side"] in ("then", "else"), f"{where}: side must be 'then' or 'else'")
        path = base / f"{module}.tla"
        _require(IDENT.fullmatch(module) is not None and path.is_file(), f"{where}: module {module}.tla not found")
        try:
            span = resolve_branch(path.read_text(encoding="utf-8"), e["label"], e["guard"], e["side"])
        except ExpectedError as err:
            raise ExpectedError(f"{where}: {err}") from err
        branches.append(Branch(name, module, e["label"], e["guard"], e["side"], e["reason"], span))
    names = [b.name for b in branches]
    _require(len(set(names)) == len(names),
             f"'branches': duplicate names {sorted({n for n in names if names.count(n) > 1})}")
    return tuple(branches)


````

  3. In `load_expected`, replace

     ```python
         _require(set(data) <= {"scenarios", "run", "never_reached", "deferred"},
                  f"unknown top-level keys: {sorted(set(data) - {'scenarios', 'run', 'never_reached', 'deferred'})}")
     ```
     with
     ```python
         top = {"scenarios", "run", "never_reached", "deferred", "branches"}
         _require(set(data) <= top, f"unknown top-level keys: {sorted(set(data) - top)}")
     ```
     and `    return Expected(scenarios, runs, never_reached, deferred)` with
     `    return Expected(scenarios, runs, never_reached, deferred, _load_branches(data.get("branches", []), base))`.
  4. Immediately BEFORE `def judge_union(executed:` insert:

````python
def counts_for_branches(run: Run, base: Path) -> bool:
    """Whether a run's coverage counts toward `branches`: it models the protocol AS SPECIFIED. A seeded run
    reaches arms the protocol never does (SEED_TAKEOVER_FOREIGN sends a Foreign read into S240_5_s5's
    continue arm), and a run whose CONFIG sets a FIX_* flag TRUE models a proposed fix - the flag lives in the
    config, not in `constants` (breaklock-remote-posix-fixed-check.cfg). Witness runs halt, but what they did
    reach is reachable, so they count (cut 6 Part 2)."""
    if run.kind == "seeded":
        return False
    flags = cfg_constants((base / run.config).read_text(encoding="utf-8"))
    return not any(k.startswith("FIX_") and v is True for k, v in flags.items())


def judge_branches(branches: tuple[Branch, ...], logs: list[tuple[str, str]]) -> bool:
    """Judge `branches` over (run name, TLC log) pairs of the runs that count; returns whether it failed. Each
    arm's count is summed over the logs; zero fails, and so does an arm no log has a node for."""
    if not branches:
        return False
    parsed: list[list[tuple[str, int, int, int, int]]] = []
    for name, text in logs:
        nodes = parse_cost_nodes(parse_messages(text))
        if nodes is None:
            print(f"run.py: cannot judge branches: {name}'s log carries no complete coverage block")
            return True
        parsed.append(nodes)
    failed = False
    for b in branches:
        counts = [c for c in (arm_count(nodes, b) for nodes in parsed) if c is not None]
        if not counts:
            print(f"MISMATCH branch {b.name}: no coverage node inside its arm in any of {len(parsed)} counted logs")
            failed = True
        elif sum(counts) == 0:
            print(f"MISMATCH branch {b.name}: the {b.side} arm of IF {b.guard} in {b.label} never ran "
                  f"({len(counts)} counted logs)")
            failed = True
        else:
            print(f"BRANCH {b.name}  {sum(counts):,} entries over {len(counts)} counted logs")
    return failed


````

  5. `judge_union`: replace its last parameter line
     `                 deferred: tuple[tuple[str, str, str], ...]) -> bool:` with
     ```python
                      deferred: tuple[tuple[str, str, str], ...],
                      branches: tuple[Branch, ...] = ()) -> bool:
     ```
     and its return `    return union_from_logs(entries, base, never_reached, deferred, partial)` with
     ```python
         failed = union_from_logs(entries, base, never_reached, deferred, partial)
         if not branches:
             return failed
         counted = [(run.name, result.log.read_text(encoding="utf-8", errors="replace"))
                    for run, fixed, result in executed
                    if not fixed and counts_for_branches(run, base) and result.log is not None]
         return judge_branches(branches, counted) or failed
     ```
     (`if not branches` first: the existing union tests pass runs whose configs do not exist, and a union with no
     branches must not read configs at all - measured: without it, 7 existing tests error.)
  6. `judge_union_from`: replace
     ```python
         failed = union_from_logs(entries, base, expected.never_reached, expected.deferred, partial)
         return 1 if failed else 0
     ```
     with
     ```python
         failed = union_from_logs(entries, base, expected.never_reached, expected.deferred, partial)
         if not expected.branches:
             return 1 if failed else 0
         counted = [(r.name, logs[r.name].read_text(encoding="utf-8", errors="replace"))
                    for r in wanted if counts_for_branches(r, base)]
         failed = judge_branches(expected.branches, counted) or failed
         return 1 if failed else 0
     ```
  7. `main`: `    elif judge_union(executed, base, expected.never_reached, expected.deferred):` →
     `    elif judge_union(executed, base, expected.never_reached, expected.deferred, expected.branches):`.

- [ ] **Step 4: run them - they pass.** `python -m unittest test_run.BranchJudgeTests` → `Ran 7 tests ... OK`; then
  `just model-test` → `OK` (every existing union test still passes).
- [ ] **Step 5: commit** `run.py`, `test_run.py`:
  `model: a branches list in expected.toml, judged by the union over the runs that count (cut 6 Part 2, item 1)`.

---

### Task 3: the two committed branch entries (items 2 and 3)

**Files:** Modify `models/lockproto/expected.toml` (append), `models/lockproto/test_run.py`.

- [ ] **Step 0: verify** in the generated `LockProtocol.tla`: the `S240_5_s6(self) ==` action contains exactly
  one line ending `IF ident # tobj[self]` (~:2224), and `S240_5_s5(self) ==` contains exactly one line ending
  `IF IsRecord(seen)` (~:2108). (`IF ident # tobj[self]` also occurs in `S240_5_s4`; that is the scoping the resolver
  handles.)
- [ ] **Step 1: add the test.** Append to `class BranchJudgeTests` (after `test_no_branches_changes_nothing`):

````python
    def test_the_committed_branches_resolve_against_the_generated_module(self) -> None:
        branches = {b.name: b for b in run.load_expected(run.HERE / "expected.toml").branches}
        self.assertEqual(sorted(branches), ["s240-5-s5-non-record", "s240-5-s6-another-file"])
        module = (run.HERE / "LockProtocol.tla").read_text(encoding="utf-8")
        self.assertTrue(text_at(module, branches["s240-5-s6-another-file"].span[0]).startswith("THEN /\\ refused' = "))
        self.assertTrue(text_at(module, branches["s240-5-s5-non-record"].span[0]).startswith("ELSE /\\ pc' = "))
````

  It fails (`[]` != the two names).
- [ ] **Step 2: append to the END of `models/lockproto/expected.toml`** (after the last `[[run]]` block; keep the
  file's line endings):

````toml

# ---------------------------------------------------------------------------------------------------------
# Branches (cut 6 Part 2): arms INSIDE a label that the label-granular gate cannot see. Each names an `IF` of
# the GENERATED module by its whole condition, inside one label's action, and the arm (`then` or `else`)
# whose first cost node must count above zero, summed over the logs of runs that model the protocol as
# specified (not seeded, no FIX_* flag). Judged by --union-from, with the label union.

[[branches]]
name = "s240-5-s6-another-file"
label = "S240_5_s6"
guard = "ident # tobj[self]"
side = "then"
reason = "240.5 step 6: another operation claimed the lock path while the takeover was in flight, so it starts again. Its RESTART equals the empty-path arm's, so only this count tells the two apart (cut 6, item 2). The remote-lease runs reach it."

[[branches]]
name = "s240-5-s5-non-record"
label = "S240_5_s5"
guard = "IsRecord(seen)"
side = "else"
reason = "240.5 step 5: a takeover that reads a non-record (torn, empty or foreign) continues to step 6 (cut 6, item 3, narrowed). Torn versus empty is not told apart: a residue in TODO.md."
````

- [ ] **Step 3: gates.** `--list-jobs` exit 0; `python -m unittest test_run.BranchJudgeTests` OK; `just model-test` OK.
  Measured on the scratch copy: over CI Model run 36506185161's logs, `run.py --union-from` prints
  `BRANCH s240-5-s6-another-file  104,046 entries over 53 counted logs` and
  `BRANCH s240-5-s5-non-record  836,528 entries over 53 counted logs` after the label union's `COVERAGE suite` line.
  (Not re-run locally: it needs the downloaded logs. Task 7 reads it from CI.)
- [ ] **Step 4: commit** `expected.toml`, `test_run.py`:
  `model: branch entries for S240_5_s6's another-file arm and S240_5_s5's non-record arm (cut 6, items 2-3)`.

---

### Task 4: branch mutants (`expect_zero_branch`)

**Files:** Modify `models/lockproto/run.py`, `models/lockproto/test_run.py`, `models/lockproto/mutants.toml`.

- [ ] **Step 0: verify** in `run.py`, once each:
  - `MUTANT_KEYS = frozenset({... "expect_present", "expect_absent", "timeout_minutes"})` (~:704);
  - `Mutant`'s `timeout_minutes: int` as its last field;
  - in `load_mutants`, the two lines `present, absent = ...` / `_require((present is None) != (absent is None), ...)`
    and `mutants.append(Mutant(name, e["file"], e["old"], e["new"], e["config"], present, absent, timeout))`;
  - in `run_mutant`: `log = MUTANTS_OUT / f"{mutant.name}.log"`; the `pcal.trans failed` return; the `cmd = [...`
    ending `"-config", str(config), "LockProtocol"]`; and the final two lines
    `output = log.read_text(...)` / `return judge_mutant(mutant, interpret(code, output, properties))`.
  - In `mutants.toml`, the `old` texts of Step 5 each occur once in their source files.

- [ ] **Step 1: add the tests.** In `test_run.py`, inside `class MutantManifestTests`, immediately BEFORE
  `    def test_the_committed_manifest_is_valid_against_the_current_sources(self) -> None:` insert:

````python
    ZERO_BRANCH = '''
        [[mutant]]
        name = "t9-zero"
        file = "algorithm.txt"
        old = "a"
        new = "b"
        config = "configs/breaklock-posix-plain-check.cfg"
        expect_zero_branch = "{branch}"
        '''

    def _write_real(self, body: str) -> Path:
        """A manifest judged against the REAL model directory: expect_zero_branch names expected.toml's branches."""
        holder = tempfile.TemporaryDirectory()
        self.addCleanup(holder.cleanup)
        manifest = Path(holder.name) / "mutants.toml"
        manifest.write_text(textwrap.dedent(body), encoding="utf-8")
        return manifest

    def test_a_zero_branch_expectation_names_a_committed_branch(self) -> None:
        (m,) = run.load_mutants(self._write_real(self.ZERO_BRANCH.format(branch="s240-5-s5-non-record")), run.HERE)
        self.assertEqual((m.present, m.absent, m.zero_branch), (None, None, "s240-5-s5-non-record"))
        for body in (self.ZERO_BRANCH.format(branch="no-such-branch"),
                     self.ZERO_BRANCH.format(branch="s240-5-s5-non-record") + 'expect_absent = "FsOk"\n'):
            with self.subTest(body=body[-60:]):
                with self.assertRaises(run.ExpectedError):
                    run.load_mutants(self._write_real(body), run.HERE)

    def test_judging_a_zero_branch_mutant(self) -> None:
        m = run.Mutant("t9-zero", "algorithm.txt", "a", "b", "configs/x.cfg", None, None, 60, "arm")
        ok = run.Outcome(None, frozenset(), 10, ())
        self.assertTrue(run.judge_zero_branch(m, ok, 0)[0], "a clean run whose arm counts zero: killed")
        self.assertFalse(run.judge_zero_branch(m, ok, 5)[0], "the arm still ran: survived")
        killed, detail = run.judge_zero_branch(m, run.Outcome(None, frozenset({"SingleWriter"}), 10, ()), 0)
        self.assertFalse(killed, "a run that halted has only a prefix: its zero proves nothing")
        self.assertIn("prefix", detail)
        for outcome, count in ((run.Outcome("TLC error 1000: boom", frozenset(), None, ()), 0), (ok, None)):
            killed, detail = run.judge_zero_branch(m, outcome, count)
            self.assertFalse(killed, "a tooling error or an unreadable arm is never a kill")
            self.assertIn("tooling", detail)

````

  They fail (`Mutant` takes 8 arguments; no `judge_zero_branch`).

- [ ] **Step 2: implement the loader**, in `run.py`:
  1. `MUTANT_KEYS`: add `"expect_zero_branch"` to the set.
  2. `Mutant`: after `    timeout_minutes: int` add
     `    zero_branch: str | None = None  # the mutated run must finish with this `branches` arm at zero (cut 6 Part 2)`.
  3. In `load_mutants`, replace
     ```python
             present, absent = e.get("expect_present"), e.get("expect_absent")
             _require((present is None) != (absent is None), f"{where}: exactly one of expect_present and expect_absent")
     ```
     with
     ```python
             present, absent, zero = e.get("expect_present"), e.get("expect_absent"), e.get("expect_zero_branch")
             _require([present, absent, zero].count(None) == 2,
                      f"{where}: exactly one of expect_present, expect_absent and expect_zero_branch")
             if zero is not None:
                 names = {b.name for b in load_expected(base / "expected.toml").branches}
                 _require(zero in names, f"{where}: expect_zero_branch must name a 'branches' entry of expected.toml")
     ```
     and `mutants.append(Mutant(name, e["file"], e["old"], e["new"], e["config"], present, absent, timeout))` with
     `mutants.append(Mutant(name, e["file"], e["old"], e["new"], e["config"], present, absent, timeout, zero))`.

- [ ] **Step 3: implement the runner**, in `run.py`:
  1. Immediately BEFORE `def run_mutant(mutant: Mutant, jar: Path, base: Path) -> tuple[bool, str]:` insert:

````python
def judge_zero_branch(mutant: Mutant, outcome: Outcome, count: int | None) -> tuple[bool, str]:
    """A branch mutant (cut 6 Part 2) is killed only by a run that FINISHED - a halted run's coverage is a
    prefix, and its zero proves nothing - and whose named arm counted zero."""
    if outcome.tooling_error is not None:
        return False, f"tooling: {outcome.tooling_error}"
    if outcome.observed:
        return False, (f"the run halted at {', '.join(sorted(outcome.observed))}: a prefix cannot show "
                       f"that {mutant.zero_branch} never runs")
    if count is None:
        return False, f"tooling: no coverage node inside {mutant.zero_branch}'s arm"
    return count == 0, f"branch {mutant.zero_branch} ran {count} times"


````

  2. After `    log = MUTANTS_OUT / f"{mutant.name}.log"` add:
     ```python
         branch = None
         if mutant.zero_branch is not None:
             branch = next(b for b in load_expected(base / "expected.toml").branches if b.name == mutant.zero_branch)
     ```
  3. After the `pcal.trans failed` return line add (the span is re-resolved in the MUTATED generated module, whose
     lines may have moved):
     ```python
             if branch is not None:  # the arm's span in the MUTATED generated module
                 try:
                     span = resolve_branch(target.read_text(encoding="utf-8"), branch.label, branch.guard, branch.side)
                 except ExpectedError as err:
                     return False, f"tooling: {err}"
                 branch = Branch(branch.name, branch.module, branch.label, branch.guard, branch.side, branch.reason, span)
     ```
  4. Replace the `cmd`'s last line `               "-metadir", str(work / "states"), "-config", str(config), "LockProtocol"]`
     with
     ```python
                    "-metadir", str(work / "states"), "-config", str(config)]
             if branch is not None:
                 cmd.extend(["-coverage", "1"])  # a branch mutant is judged from the arm's cost node
             cmd.append("LockProtocol")
     ```
  5. Replace the final two lines
     ```python
         output = log.read_text(encoding="utf-8", errors="replace")
         return judge_mutant(mutant, interpret(code, output, properties))
     ```
     with
     ```python
         output = log.read_text(encoding="utf-8", errors="replace")
         outcome = interpret(code, output, properties)
         if branch is not None:
             nodes = parse_cost_nodes(parse_messages(output))
             return judge_zero_branch(mutant, outcome, None if nodes is None else arm_count(nodes, branch))
         return judge_mutant(mutant, outcome)
     ```
- [ ] **Step 4: run** `python -m unittest test_run.MutantManifestTests` → OK.
- [ ] **Step 5: the two manifest entries.** Append to `models/lockproto/mutants.toml`:

````toml

# Cut 6 Part 2, items 2 and 3: branch mutants. Each is killed only if its run FINISHES (a halted run's coverage
# is a prefix) with the named `branches` arm at zero.

[[mutant]]
name = "p2-another-file-swallowed"
file = "algorithm.txt"
old = '             if (ident = NoObj) { refused[self] := "RESTART"; goto S240_5_close; }'
new = '             if (ident = NoObj \/ ident # tobj) { refused[self] := "RESTART"; goto S240_5_close; }'
config = "configs/breaklock-remote-posix-plain-check.cfg"
expect_zero_branch = "s240-5-s6-another-file"

[[mutant]]
name = "p2-non-record-refuses"
file = "algorithm.txt"
old = '             if (IsRecord(seen) /\ seen # seenRec[self]) { refused[self] := "RESTART"; goto S240_5_close; }'
new = '             if (~IsRecord(seen) \/ (IsRecord(seen) /\ seen # seenRec[self])) { refused[self] := "RESTART"; goto S240_5_close; }'
config = "configs/breaklock-posix-plain-check.cfg"
expect_zero_branch = "s240-5-s5-non-record"
````

  Why each is a kill: the first makes `S240_5_s6`'s first arm swallow `ident # tobj`, whose own arm then never runs
  (the guard text is unchanged, so the entry still resolves); both arms set RESTART and go to `S240_5_close`, so the
  run behaves as before and violates nothing. The second makes every non-record read refuse RESTART at the first
  `IF`, so the anchored `ELSE` of `IF IsRecord(seen)` can no longer be reached; RESTART needs no justification
  (`RefusalJustified` covers TARGET_LOCK_BUSY, `invariants.txt:66`). Measured: pcal.trans accepts both, and both anchors
  still resolve in the mutated generated module (`((2225, 50), (2232, 50))`, `((2119, 50), (2123, 1))`). The two runs
  finished on CI in 7.6 and 3.3 minutes unmutated (14.5M and 6.6M states); `timeout_minutes` stays the default 60.
- [ ] **Step 6: gates.** `--list-mutants` exit 0 with 21 names; `--check-translation`; `just model-test` OK.
- [ ] **Step 7: commit** `run.py`, `test_run.py`, `mutants.toml`:
  `model: branch mutants, killed only by a finished run with the arm at zero (cut 6 Part 2, items 2-3)`.

---

### Task 5: item 6 - the single-Owner host-crash witness

**Files:** Modify `models/lockproto/invariants.txt`, `LockProtocol.tla` (regenerated), `expected.toml`,
`mutants.toml`; create `configs/recovery-posix-owneronly-hostcrash-witness-NeverRevertedOwnWrite.cfg`.

- [ ] **Step 0: verify.**
  - `invariants.txt`: `NeverSecondPastId == \A d \in Dirs, c \in Classes : Cardinality(fs.past[d][c]) <= 1` exists
    once.
  - `FsModel.tla`: once each, the line
    `            ELSE CASE pick[o] = "old" -> IF fs.durable[o] = NoContent THEN EmptyFile ELSE fs.durable[o]` (~:315)
    and `Unflushed(fs) == {o \in Objs : fs.content[o] \notin {NoContent, EmptyFile} /\ fs.durable[o] # fs.content[o]}`
    (~:307).
  - `configs/recovery-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg` has `SYMMETRY RecovererPerms` as its
    second non-comment line, `    Recoverers = {r1, r2}`, and an `INVARIANT` section with the one line
    `    NeverHostCrashChangedLock`.
  - `algorithm.txt:715-732` is the Owner process (`own_start` calls `Acquire`; `own_publish`; `own_end`), and the
    only `FsFlushFile` in `algorithm.txt` is at ~:535 (the takeover's).
- [ ] **Step 1: the witness.** In `invariants.txt`, right after the `NeverSecondPastId` line, add (one blank line
  before it):

```tla

\* A host crash reverted a lock record its Owner had written: the lock path still names the Owner's object,
\* which is EmptyFile again after the Owner wrote its record there. Only FsHostCrash's "old" pick can do that,
\* and only for an unflushed object: creation is the one writer of EmptyFile, and in the single-Owner pairing
\* only the Owner creates, once (Acquire, called once from own_start), before writing - and the Owner never
\* flushes the lock file. So "always keep new content" or an empty `Unflushed` leaves this witness unviolated
\* (cut 6, item 6; TLC gives FsHostCrash's CASE arms no coverage node, so no branch entry can see them). Its
\* run, recovery-posix-owneronly-hostcrash-witness-NeverRevertedOwnWrite, must stop with it violated.
NeverRevertedOwnWrite ==
    ~\E p \in Owners : crashed[p] /\ seenRec[p] = OwnRecord(p) /\ LockObj # NoObj /\ fs.content[LockObj] = EmptyFile
```

  Regenerate; `--check-translation` → exit 0.
- [ ] **Step 2: the config**, built from the template by a one-off script
  `.clavity/scratch/cut6/owneronly_cfg.py` (scratch, not committed), run from `models/lockproto`:

```python
from pathlib import Path
src = Path("configs/recovery-posix-hostcrash-witness-NeverHostCrashChangedLock.cfg").read_bytes().decode("utf-8")
nl = "\r\n" if "\r\n" in src else "\n"
body = [l for l in src.split(nl) if not l.startswith("\\*")]
assert body[0] == "SPECIFICATION Spec" and body[1] == "SYMMETRY RecovererPerms", body[:2]
body = [body[0]] + body[2:]
i = body.index("    Recoverers = {r1, r2}"); body[i] = "    Recoverers = {}"
j = body.index("INVARIANT"); assert body[j + 1] == "    NeverHostCrashChangedLock"; body[j + 1] = "    NeverRevertedOwnWrite"
head = ["\\* recovery scenario, POSIX, host crashes on, ONE Owner and no other actor: witness run for",
        "\\* NeverRevertedOwnWrite (cut 6, item 6). Expected VIOLATED: a host crash's \"old\" pick reverts the Owner's",
        "\\* unflushed lock record to EmptyFile. With a second actor another create could make the same state, so the",
        "\\* pairing is the point. No SYMMETRY: there is nothing to permute."]
Path("configs/recovery-posix-owneronly-hostcrash-witness-NeverRevertedOwnWrite.cfg").write_bytes(nl.join(head + body).encode("utf-8"))
```

- [ ] **Step 3: its run.** In `expected.toml`, right after the `recovery-posix-hostcrash-witness-NeverHostCrashChangedLock`
  block, add this block (the template's `constants` with `Recoverers = []`; no `symmetry`):

````toml
[[run]]
name = "recovery-posix-owneronly-hostcrash-witness-NeverRevertedOwnWrite"
module = "LockProtocol"
config = "configs/recovery-posix-owneronly-hostcrash-witness-NeverRevertedOwnWrite.cfg"
scenario = "recovery"
kind = "witness"
violated = ["NeverRevertedOwnWrite"]
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = [], MaxObjs = 3, MaxCrashes = 2, HostCrashes = true, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false, SEED_FS_LOCK_WITHOUT_HANDLE = false, SEED_FS_ALIEN_CONTENT = false, SEED_CHECK_REFUSES_UNTOUCHED = false, SEED_RECOVER_LIVE = false, SEED_FS_PAST_UNALLOCATED = false, SEED_FS_DOUBLE_HANDLE = false, SEED_FS_ENTRY_UNALLOCATED = false, SEED_FS_ALIEN_RECORD = false, SEED_RECOVERER_GIVES_UP = false, SEED_RELEASE_CHECK_REFUSES_UNTOUCHED = false, SEED_RESTART_CHECK_REFUSES_UNTOUCHED = false, SEED_TAKEOVER_FOREIGN = false, HostCrashAtDurableOnly = false }
timeout_minutes = 30

````

  If `--list-jobs` reports a constants mismatch against the config, STOP and report it; do not edit either side to fit.
- [ ] **Step 4: its two mutants.** Append to `mutants.toml`:

````toml

# Cut 6 Part 2, item 6 (M7's remaining halves): each removes the host crash's "old" outcome, so the single-Owner
# witness run no longer finds NeverRevertedOwnWrite violated.

[[mutant]]
name = "p2-old-keeps-new"
file = "FsModel.tla"
old = '            ELSE CASE pick[o] = "old" -> IF fs.durable[o] = NoContent THEN EmptyFile ELSE fs.durable[o]'
new = '            ELSE CASE pick[o] = "old" -> fs.content[o]'
config = "configs/recovery-posix-owneronly-hostcrash-witness-NeverRevertedOwnWrite.cfg"
expect_absent = "NeverRevertedOwnWrite"

[[mutant]]
name = "p2-unflushed-empty"
file = "FsModel.tla"
old = 'Unflushed(fs) == {o \in Objs : fs.content[o] \notin {NoContent, EmptyFile} /\ fs.durable[o] # fs.content[o]}'
new = 'Unflushed(fs) == {}'
config = "configs/recovery-posix-owneronly-hostcrash-witness-NeverRevertedOwnWrite.cfg"
expect_absent = "NeverRevertedOwnWrite"
````

- [ ] **Step 5: gates.** `--check-translation`, `--list-jobs`, `--list-mutants` (23 names), `just model-test` → all pass.
- [ ] **Step 6: commit** `invariants.txt`, `LockProtocol.tla`, the new config, `expected.toml`, `mutants.toml`:
  `model: a single-Owner host-crash witness catches the lost old outcome (cut 6, item 6)`.

---

### Task 6: bookkeeping

**Files:** `TODO.md`, `models/lockproto/README.md`.

- [ ] **Step 1: `TODO.md`** (find each entry by content; one closing line each citing the commit):
  - "**Build the BRANCH-granular coverage union**" → `[x]`, closed by Tasks 1-3.
  - "**`S240_5_s6`'s two refusing branches are indistinguishable to every gate.**" → `[x]`, closed by the
    `s240-5-s6-another-file` entry and its mutant.
  - "**M7, host-crash net holes.**" → `[x]`: the two remaining halves are killed by the single-Owner witness (Task 5).
    Replace its "Part 2's extended-tier branch union closes the rest" with what closed them.
  - Under "**Residues from cut 6.**" add: "`FsHostCrash`'s torn outcome has no net: TLC gives the `CASE` arms inside
    its function constructor no coverage node (measured: no node for `FsModel.tla:312-318` in any extended host-crash
    log), and no state predicate separates a torn crash from a crash mid-write."
  - Leave "**Say which `never_reached` claims an exhaustive run supports**" and "**Commit the union verdict as an
    artifact**" open.
- [ ] **Step 2: `models/lockproto/README.md`** - next to the paragraph on `mutants.toml` (added in Part 1), add one
  paragraph (3-5 lines): `branches` in `expected.toml` names `IF` arms of the generated module; `--union-from` counts
  each arm's first cost node over the runs that model the protocol as specified (not seeded, no `FIX_*` flag); a
  branch mutant (`expect_zero_branch`) is killed only by a finished run with the arm at zero.
- [ ] **Step 3:** `just check` → exit 0; commit `TODO.md`, `models/lockproto/README.md`:
  `docs: cut 6 Part 2 bookkeeping - the branch union, item 2 and M7 closed, the torn-arm residue`.

---

### Task 7: verification on CI (driver)

- [ ] **Step 1:** `git status --short` clean; all local gates pass.
- [ ] **Step 2:** push `spec/seq-after-cut5`. The push runs `Model mutants` (mutants.toml and run.py changed): all
  23 must be KILLED. Dispatch `Model` (`gh workflow run model.yml --ref spec/seq-after-cut5`).
- [ ] **Step 3: read the results.**
  - The union job prints `COVERAGE suite ...` and two `BRANCH` lines, neither a `MISMATCH`.
  - `recovery-posix-owneronly-hostcrash-witness-NeverRevertedOwnWrite` is `OK` with `observed=NeverRevertedOwnWrite`.
    If it reports no violation, the witness is unreachable in the correct model: STOP, back to the owner.
  - `p2-another-file-swallowed`, `p2-non-record-refuses`, `p2-old-keeps-new`, `p2-unflushed-empty` are `KILLED`.
    `NOT KILLED ... the run halted at X` means the mutant made the run violate X: report it and STOP; do not change
    the kill rule.
- [ ] **Step 4:** then Part 1's Task 15 (state counts), from the same green Model run, including the new witness's count.

---

## Self-review

- **Spec coverage:** item 1 → Tasks 1-3 (data model, expectations, which logs count, where judged: the per-PR union
  job, which already runs `--union-from`); item 2 → Tasks 3-4; item 3 → Tasks 3-4; item 6's two halves → Task 5
  (refinement 1); the spec's tests list (arm parsing, two logs one covering, an arm at zero, missing and ambiguous
  anchors, scoping) → `BranchResolveTests` and `BranchJudgeTests`; residues → Task 6.
- **Dropped from the spec, and why:** the extended-tier union job, `module`/`operator` entries and `tier` (refinement 1:
  nothing would use them); the full span in `parse_cost_nodes` (refinement 4).
- **Not closed here:** the `never_reached` claims hygiene item and the committed union verdict (both stay in TODO.md).
- **Type consistency:** `Branch(name, module, label, guard, side, reason, span)`; `resolve_branch(module_text, label, guard, side)`;
  `arm_count(nodes, branch)`; `counts_for_branches(run, base)`; `judge_branches(branches, logs)`;
  `judge_zero_branch(mutant, outcome, count)`; `Mutant(..., timeout_minutes, zero_branch=None)` - the same in every task.

## Stand-downs

- **Panel round 1 (agy, after one errored attempt): GREEN** - Axiom Breaker, Cascade Analyst, Mechanism Gamer,
  Protocol Pedant and Literal Implementer found nothing. Its unverified assumption (`fixture("arm_coverage")`) holds:
  `test_run.py:76`, and the scratch run passed with it.
- DISCARDED-BELOW-FLOOR: "`test_the_committed_branches_resolve_against_the_generated_module` is brittle to translator
  output" - unreachable as a false result, because the pin is deliberate: `--check-translation` fails first on any
  translator change (`run.py:642`, `check_translation`), and the resolver re-pins against the same text.
