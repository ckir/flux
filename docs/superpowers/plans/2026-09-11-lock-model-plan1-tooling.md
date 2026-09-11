# Lock-Protocol Model Check, Plan 1 (Tooling) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the tooling of the lock-protocol model check (the TLC runner and its self-test, the spec-drift stamp and
traceability test, the filesystem probes FS-1 to FS-11, the `just` recipes, and the CI workflow) so that plan 2 can
build the protocol model on top of it.

**Architecture:** `models/lockproto/run.py` (Python 3.11+, standard library only) reads `expected.toml`, runs TLC from a
pinned `tla2tools.jar` for each run, parses TLC's `-tool` output, and judges each run. A tiny `Smoke.tla` model forms a
`selftest` scenario that exercises every judging path. Two Rust test files run in the existing `just check` and CI
test job: `tests/model_stamp.rs` (stamp and traceability) and `crates/flux-platform/tests/fs_semantics.rs` (probes).
`.github/workflows/model.yml` runs the TLC scenarios in a matrix built from `expected.toml`, behind an always-reporting
`model-gate` job.

**Tech Stack:** Python 3.11+ (`tomllib`, `unittest`), TLA+ / TLC 2.19 (`tla2tools.jar` v1.7.4, Java 21), Rust 2024
(MSRV 1.85; crates `blake3`, `toml` 1.1, `serde`, `tempfile`, `rustix` 1.1, `windows-sys` 0.61), `just`, GitHub Actions.

---

## Context

- **Design:** `docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md` at commit `022a565` (approved;
  aligned with the measurements below). Section numbers in this plan refer to it.
- **Branch:** `model/lock-protocol`. Work in place on that branch; do not push.
- **Sequence:** this is plan 1 of 3 (owner decision, 2026-09-11). Plan 2 (`FsModel.tla` + `LockProtocol.tla`) and plan 3
  (`Claims.tla`) are written after this plan lands. After this plan, `expected.toml` has only the `selftest` scenario,
  the stamp lists no spec headings, and `trace.toml` has no units; plan 2 fills them.
- **Every file below was built and run before this plan was written**, on Windows 11 with Java 21 and Python 3.14;
  the runner tests also ran under Python 3.12, and the probes also ran on Linux (tmpfs and ext4, through WSL). On macOS
  the probes were only compiled and linted (`cargo clippy --target aarch64-apple-darwin`); their first macOS run is
  the existing CI test job. `just check` and `cargo deny check` were green. The file contents in this plan are those
  files, copied verbatim.

### What exists now (verified by reading the files)

- Root `Cargo.toml`: the root package `flux` has no lib or bin, declares `[[test]] name = "integration"` and one bench,
  and keeps test auto-discovery on (no `autotests = false`), so a new `tests/model_stamp.rs` becomes a test target.
  Its `[dev-dependencies]` holds only `criterion = { workspace = true }`. `[workspace.dependencies]` pins `blake3 =
  "1.8"`, `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`, `tempfile = "3"`; there is no
  `toml`, `rustix`, or `windows-sys`.
- `crates/flux-platform/Cargo.toml` has only `[package]`; `crates/flux-platform/src/lib.rs` is a doc-comment scaffold.
- `justfile`: `check: fmt-check clippy typos test`; `test` runs `cargo nextest run --workspace --no-tests=pass` and
  `cargo test --doc --workspace`; `clippy` runs `cargo clippy --workspace --all-targets -- -D warnings`. There is no
  `set shell`, so on Windows recipes run under `sh` (Git Bash).
- `clippy.toml`: `msrv = "1.85"`. `rustfmt.toml`: `max_width = 100`, `use_small_heuristics = "Max"`.
- `.github/workflows/ci.yml`: a `test` job on ubuntu, macos and windows runs `cargo nextest run --workspace
  --no-tests=pass --no-fail-fast`, so the two new Rust test files run there with no CI change.
- `.gitignore` ignores `target` (so `target/tla/` is ignored) and `.clavity/`, but not `__pycache__/`.
- `_typos.toml` ignores hex runs of 32+ characters, so BLAKE3 hashes in `trace.toml` pass `typos`.
- `deny.toml` allows MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, CC0-1.0; every new crate is covered.

### Measured facts this plan relies on (TLC 2.19, `tla2tools.jar` v1.7.4)

`tla2tools.jar` v1.7.4 has SHA-256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`. Release v1.8.0
is a rolling prerelease whose asset changes, so it cannot be pinned. With `-tool`, every message is framed as
`@!@!@STARTMSG <code>:<severity> @!@!@` ... `@!@!@ENDMSG <code> @!@!@`, and output mixes CRLF and LF on Windows.

| Situation | Exit status | Message |
|---|---|---|
| no violation | 0 | 2193 "Model checking completed. No error has been found." |
| invariant violations with `-continue` | 0 | 2110 "Invariant X is violated." per violating state, then 2193 |
| invariant violation without `-continue` | 12 | 2110 |
| invariant violated by an initial state | 0 with `-continue` | 2107 "Invariant X is violated by the initial state:" |
| deadlock | 11 | 2114 "Deadlock reached." |
| temporal property violated (also with `-continue`, together with 2110s) | 13 | 2116 "Temporal properties were violated." (names no property) |
| module parse error | 150 | 3002 |
| evaluation error | 75 | 1000 |
| configuration error (unknown name) | 151 | 2229 |
| any run, errors included | | 2186 "Finished in ..." |
| trace states | | 2217 (severity 4); 2121, 2264 frame a trace; 2122 "Back to state" is severity 4 |

Because 2116 names no property, a configuration that checks a temporal property lists exactly one `PROPERTY`
(design Section 4). `-workers auto` works. TLC wrote no trace files beside the model with these flags.

### Deviations from the design text, all already folded into the design at `022a565`

1. `just model <scenario>` instead of `just model scenario=<name>`: `just` accepts `name=value` only as a variable
   override before the recipe name.
2. `liveness` runs also use `-continue`, and a temporal configuration lists exactly one `PROPERTY` (measured above).
3. A `selftest` scenario (`Smoke.tla`) exists so the runner can be tested before any protocol model exists; it stays
   as the runner's regression check.
4. The stamp lists no headings until plan 2 encodes them.
5. FS-6 and FS-7 use `std::fs::rename` for replacing renames: measured on Windows 11 NTFS, `MoveFileExW(MOVEFILE_
   REPLACE_EXISTING)` refuses any open target even with delete-sharing, while `std::fs::rename` (a POSIX-semantics
   rename) behaves as the model's Windows row says. FS-7 prints what `MoveFileExW` does. This is recorded as a spec
   finding for plan 2 (`.clavity/local-anomalies.md`, 2026-09-11).

### Rules for whoever executes a task

- **Step 0, state check:** before editing, confirm the "Files" and "What exists now" facts for the task still hold
  (the files to create do not exist yet; the lines to change read exactly as quoted). If anything differs, stop and
  report `STATE_MISMATCH: <what>`; do not adapt.
- **Shape divergence:** the code blocks are the contract. If making something work would change a name, a format, a
  message, a file layout, or an exit code shown here, stop and report `[plan] -> [yours] because <reason>`.
- **Oracles:** the design sections cited in each task define correct behaviour; the tests in this plan are already
  written. Implement until they pass; never edit a test to match the code. A failing probe is never weakened (design
  Section 10).
- **Gate:** the repository's gate is `just check`. Do not add stricter flags than it uses.
- **Shell:** run every command in a POSIX shell: bash, or Git Bash on Windows (the shell `just` itself uses there).
  The commands use `head`, `$?`, `2>&1`, and `${TMPDIR:-/tmp}`, which PowerShell and cmd do not accept.
- **Tools:** Java 11+ (21 recommended), Python 3.11+ as `python3` (or set `PYTHON`), `just`, `cargo-nextest`, `typos`.
  Commits end with the attribution lines of the session running the plan.

## File map

| File | Created or changed in | Responsibility |
|---|---|---|
| `.gitignore` | Task 1 | ignore `__pycache__/` |
| `models/lockproto/run.py` | Task 1 | the runner (design Section 4) |
| `models/lockproto/test_run.py` | Task 1 | runner unit tests, no Java |
| `models/lockproto/testdata/Fixture.tla`, `Broken.tla` | Task 1 | modules whose TLC output the unit tests replay |
| `models/lockproto/testdata/record_fixtures.py` | Task 1 | records `testdata/*.out` (needs Java) |
| `models/lockproto/testdata/*.out` | Task 1 (generated) | recorded TLC output, first line `exit=<status>` |
| `models/lockproto/Smoke.tla` | Task 1 | runner self-test model |
| `models/lockproto/configs/selftest-*.cfg` | Task 1 | the three self-test configurations |
| `models/lockproto/expected.toml` | Task 1 | the run list (only `selftest` after plan 1) |
| `justfile` | Tasks 2 and 3 | `model`, `model-test`, `model-stamp` |
| `.claude/recommended-tools.json` | Task 2 | Java and Python entries |
| `models/lockproto/README.md` | Task 2 | how to run, files, judging, probes, unverified assumptions |
| `Cargo.toml` | Tasks 3 and 4 | `toml`, `rustix`, `windows-sys` in the workspace; root test dev-dependencies |
| `models/lockproto/spec-sections.stamp` | Task 3 | spec path and stamped headings (none yet) |
| `models/lockproto/trace.toml` | Task 3 | traceability map (no units yet) |
| `tests/model_stamp.rs` | Task 3 | stamp and traceability check (design Sections 9, 9.1) |
| `crates/flux-platform/Cargo.toml` | Task 4 | probe dev-dependencies |
| `crates/flux-platform/tests/fs_semantics.rs` | Task 4 | probes FS-1 to FS-11 (design Section 10) |
| `.github/workflows/model.yml` | Task 5 | `plan`, `scenario`, `model-gate` (design Section 4) |
| `Cargo.lock` | Tasks 3 and 4 (generated) | new crates |

---

### Task 1: The runner, its unit tests, and the self-test scenario

**Design oracle:** Section 4 (the `expected.toml` fields, how each kind of run is judged, the runner's behaviour and
exit codes) and the measured TLC facts above.

**Files:**
- Modify: `.gitignore` (append)
- Create: `models/lockproto/test_run.py`, `models/lockproto/run.py`
- Create: `models/lockproto/Smoke.tla`, `models/lockproto/configs/selftest-check.cfg`,
  `models/lockproto/configs/selftest-liveness.cfg`, `models/lockproto/configs/selftest-seeded-SEED_OVERSHOOT.cfg`,
  `models/lockproto/expected.toml`
- Create: `models/lockproto/testdata/Fixture.tla`, `models/lockproto/testdata/Broken.tla`,
  `models/lockproto/testdata/record_fixtures.py`, and the generated `models/lockproto/testdata/*.out`

- [ ] **Step 0: State check**

Run: `git status --short && ls models 2>&1; java -version 2>&1 | head -1; python3 --version`
Expected: a clean tree on `model/lock-protocol`; `ls: cannot access 'models'`; a Java version line; Python 3.11 or later.

- [ ] **Step 1: Ignore Python bytecode**

Append to `.gitignore`:

```text

# Python bytecode from models/lockproto/run.py and its tests
__pycache__/
```

- [ ] **Step 2: Write the unit tests**

Create `models/lockproto/test_run.py`:

````python
"""Unit tests for run.py. Standard library only; no Java needed (TLC output comes from testdata/)."""

from __future__ import annotations

import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import run  # noqa: E402

TESTDATA = HERE / "testdata"

GOOD_EXPECTED = """
scenarios = ["demo"]

[[run]]
name = "demo-posix-check"
module = "M"
config = "check.cfg"
scenario = "demo"
kind = "check"
violated = ["NeverDone"]
open_findings = [{ name = "Safe", tracking = "TODO.md: demo", fix_flag = "FIX_SAFE" }]
timeout_minutes = 5

[[run]]
name = "demo-posix-liveness"
module = "M"
config = "live.cfg"
scenario = "demo"
kind = "liveness"
timeout_minutes = 5

[[run]]
name = "demo-posix-seeded-SEED_X"
module = "M"
config = "seed.cfg"
scenario = "demo"
kind = "seeded"
violated = ["Other"]
timeout_minutes = 5
"""

CFGS = {
    "check.cfg": "SPECIFICATION Spec\nCONSTANTS\n    FIX_SAFE = FALSE\nINVARIANTS Safe NeverDone\n",
    "live.cfg": "SPECIFICATION Spec\nCONSTANT FIX_SAFE = FALSE\nPROPERTY Eventually\n",
    "seed.cfg": "SPECIFICATION Spec\nCONSTANT FIX_SAFE = FALSE\nINVARIANT Other\n",
}


def fixture(case: str) -> tuple[int, str]:
    text = (TESTDATA / f"{case}.out").read_text(encoding="utf-8")
    first, _, rest = text.partition("\n")
    return int(first.removeprefix("exit=")), rest


class ExpectedDir:
    """A temporary directory holding expected.toml, M.tla and the configs."""

    def __init__(self, expected: str = GOOD_EXPECTED, cfgs: dict[str, str] | None = None) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.path = Path(self._tmp.name)
        (self.path / "M.tla").write_text("---- MODULE M ----\n====\n", encoding="utf-8")
        for name, text in (CFGS if cfgs is None else cfgs).items():
            (self.path / name).write_text(text, encoding="utf-8")
        (self.path / "expected.toml").write_text(textwrap.dedent(expected), encoding="utf-8")

    def load(self) -> tuple[list[str], list[run.Run]]:
        return run.load_expected(self.path / "expected.toml")

    def close(self) -> None:
        self._tmp.cleanup()


class LoadExpectedTests(unittest.TestCase):
    def load(self, expected: str, cfgs: dict[str, str] | None = None) -> tuple[list[str], list[run.Run]]:
        d = ExpectedDir(expected, cfgs)
        self.addCleanup(d.close)
        return d.load()

    def assertRejected(self, expected: str, fragment: str, cfgs: dict[str, str] | None = None) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            self.load(expected, cfgs)
        self.assertIn(fragment, str(ctx.exception))

    def test_good_file_loads(self) -> None:
        scenarios, runs = self.load(GOOD_EXPECTED)
        self.assertEqual(scenarios, ["demo"])
        self.assertEqual([r.kind for r in runs], ["check", "liveness", "seeded"])
        self.assertEqual(runs[0].open_findings, (run.OpenFinding("Safe", "TODO.md: demo", "FIX_SAFE"),))
        self.assertEqual(runs[1].violated, ())

    def test_the_repository_file_loads(self) -> None:
        scenarios, runs = run.load_expected(HERE / "expected.toml")
        self.assertTrue(scenarios)
        self.assertTrue(runs)

    def test_unknown_kind(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('kind = "liveness"', 'kind = "live"'), "kind must be one of")

    def test_scenario_not_listed(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('scenario = "demo"\nkind = "liveness"', 'scenario = "other"\nkind = "liveness"'),
                            "is not in 'scenarios'")

    def test_listed_scenario_without_runs(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('scenarios = ["demo"]', 'scenarios = ["demo", "empty"]'), "has no runs")

    def test_bad_name(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('"demo-posix-liveness"', '"liveness-demo"'), "name must be")

    def test_seeded_name_carries_its_seed(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('"demo-posix-seeded-SEED_X"', '"demo-posix-seeded"'), "exactly when it is seeded")

    def test_only_seeded_names_carry_a_seed(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('"demo-posix-liveness"', '"demo-posix-liveness-SEED_X"'),
                            "exactly when it is seeded")

    def test_duplicate_names(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('"demo-posix-seeded-SEED_X"', '"demo-posix-check"')
                            .replace('kind = "seeded"', 'kind = "check"'), "duplicate run names")

    def test_liveness_with_violated(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('kind = "liveness"', 'kind = "liveness"\nviolated = ["X"]'),
                            "a liveness run has no 'violated'")

    def test_seeded_needs_exactly_one(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('violated = ["Other"]', 'violated = ["Other", "More"]'),
                            "exactly one invariant or property")

    def test_seeded_on_an_open_finding(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('violated = ["Other"]', 'violated = ["Safe"]'),
                            "carries an open finding on Safe")

    def test_open_finding_needs_all_fields(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace(', fix_flag = "FIX_SAFE"', ''), "exactly name, tracking, fix_flag")

    def test_fix_flag_missing_from_config(self) -> None:
        cfgs = dict(CFGS, **{"check.cfg": "SPECIFICATION Spec\nINVARIANTS Safe NeverDone\n"})
        self.assertRejected(GOOD_EXPECTED, "fix flag FIX_SAFE must appear exactly once", cfgs)

    def test_deadlock_check_cannot_be_turned_off(self) -> None:
        cfgs = dict(CFGS, **{"seed.cfg": CFGS["seed.cfg"] + "CHECK_DEADLOCK FALSE\n"})
        self.assertRejected(GOOD_EXPECTED, "must not set CHECK_DEADLOCK", cfgs)

    def test_liveness_needs_one_property(self) -> None:
        cfgs = dict(CFGS, **{"live.cfg": "SPECIFICATION Spec\nPROPERTIES Eventually Always\n"})
        self.assertRejected(GOOD_EXPECTED, "exactly one PROPERTY", cfgs)

    def test_liveness_without_symmetry(self) -> None:
        cfgs = dict(CFGS, **{"live.cfg": CFGS["live.cfg"] + "SYMMETRY Perms\n"})
        self.assertRejected(GOOD_EXPECTED, "symmetry is unsound", cfgs)

    def test_seeded_property_needs_one_property(self) -> None:
        expected = GOOD_EXPECTED.replace('violated = ["Other"]', 'violated = ["Eventually"]')
        cfgs = dict(CFGS, **{"seed.cfg": "SPECIFICATION Spec\nPROPERTY Eventually Always\n"})
        self.assertRejected(expected, "exactly one PROPERTY", cfgs)

    def test_missing_config_file(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('"seed.cfg"', '"missing.cfg"'), "config missing.cfg not found")

    def test_unknown_run_key(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace("timeout_minutes = 5\n\n[[run]]\nname = \"demo-posix-liveness\"",
                                                  "timeout_minutes = 5\nnotes = \"x\"\n\n[[run]]\nname = \"demo-posix-liveness\""),
                            "unknown keys")


class CfgTests(unittest.TestCase):
    def test_properties_ignore_comments(self) -> None:
        text = "\\* PROPERTY Hidden\nSPECIFICATION Spec\n(* PROPERTY AlsoHidden *)\nPROPERTY Real\nINVARIANT I\n"
        self.assertEqual(run.cfg_properties(text), ["Real"])

    def test_fixed_cfg_switches_each_flag(self) -> None:
        text = "CONSTANTS\n    FIX_A = FALSE\n    FIX_B = FALSE\n    SEED_C = FALSE\n"
        fixed = run.fixed_cfg_text(text, ["FIX_A", "FIX_B"])
        self.assertIn("FIX_A = TRUE", fixed)
        self.assertIn("FIX_B = TRUE", fixed)
        self.assertIn("SEED_C = FALSE", fixed)

    def test_keywords_inside_strings_are_values(self) -> None:
        text = 'CONSTANT Kind = "VIEW"\nPROPERTY Real\nCONSTANT Other = "SYMMETRY"\n'
        sections = run.cfg_sections(text)
        self.assertEqual(run.cfg_properties(text), ["Real"])
        self.assertNotIn("VIEW", sections)
        self.assertNotIn("SYMMETRY", sections)

    def test_fixed_cfg_leaves_comments_alone(self) -> None:
        text = "\\* set FIX_A = FALSE to see the defect\n(* FIX_A = FALSE *)\nCONSTANT FIX_A = FALSE\n"
        fixed = run.fixed_cfg_text(text, ["FIX_A"])
        self.assertEqual(fixed, "\\* set FIX_A = FALSE to see the defect\n(* FIX_A = FALSE *)\nCONSTANT FIX_A = TRUE\n")

    def test_fixed_cfg_rejects_a_missing_flag(self) -> None:
        with self.assertRaises(run.ExpectedError):
            run.fixed_cfg_text("CONSTANT FIX_A = TRUE\n", ["FIX_A"])


class InterpretTests(unittest.TestCase):
    def outcome(self, case: str, properties: list[str] | None = None) -> run.Outcome:
        code, output = fixture(case)
        return run.interpret(code, output, properties or [])

    def test_clean_run(self) -> None:
        result = self.outcome("clean")
        self.assertIsNone(result.tooling_error)
        self.assertEqual(result.observed, frozenset())
        self.assertEqual(result.distinct_states, 4)

    def test_continue_reports_every_invariant(self) -> None:
        self.assertEqual(self.outcome("continue_two_invariants").observed, {"NeverTwo", "NeverThree"})

    def test_halting_invariant(self) -> None:
        result = self.outcome("halt_invariant")
        self.assertEqual(result.observed, {"NeverThree"})
        self.assertTrue(result.trace, "a violation carries its trace")

    def test_initial_state_violation(self) -> None:
        self.assertEqual(self.outcome("initial_invariant").observed, {"NotZero"})

    def test_deadlock(self) -> None:
        self.assertEqual(self.outcome("deadlock").observed, {run.DEADLOCK})

    def test_temporal_violation_is_named_from_the_config(self) -> None:
        self.assertEqual(self.outcome("temporal", ["EventuallyFive"]).observed, {"EventuallyFive"})

    def test_temporal_violation_with_two_properties_is_a_tooling_failure(self) -> None:
        self.assertIsNotNone(self.outcome("temporal", ["A", "B"]).tooling_error)

    def test_temporal_and_invariant_together(self) -> None:
        self.assertEqual(self.outcome("temporal_and_invariant", ["EventuallyFive"]).observed,
                         {"NeverTwo", "EventuallyFive"})

    def test_errors_are_tooling_failures(self) -> None:
        for case in ("eval_error", "config_error", "parse_error"):
            with self.subTest(case=case):
                self.assertIsNotNone(self.outcome(case).tooling_error)

    def test_output_cut_short_is_a_tooling_failure(self) -> None:
        code, output = fixture("clean")
        result = run.interpret(code, output[: len(output) // 3], [])
        self.assertEqual(result.tooling_error, "TLC did not finish (no 'Finished' message)")

    def test_exit_status_must_agree_with_messages(self) -> None:
        _, output = fixture("clean")
        self.assertIn("does not match", run.interpret(12, output, []).tooling_error or "")

    def test_crlf_output_parses(self) -> None:
        code, output = fixture("continue_two_invariants")
        self.assertEqual(run.interpret(code, output.replace("\n", "\r\n"), []).observed, {"NeverTwo", "NeverThree"})


class JudgeTests(unittest.TestCase):
    def setUp(self) -> None:
        d = ExpectedDir()
        self.addCleanup(d.close)
        _, runs = d.load()
        self.check, self.liveness, self.seeded = runs

    @staticmethod
    def seen(*names: str) -> run.Outcome:
        return run.Outcome(None, frozenset(names), 10, ())

    def test_check_needs_witnesses_and_open_findings(self) -> None:
        self.assertEqual(run.judge(self.check, self.seen("NeverDone", "Safe"), fixed=False), "ok")
        self.assertEqual(run.judge(self.check, self.seen("NeverDone"), fixed=False), "mismatch")
        self.assertEqual(run.judge(self.check, self.seen("NeverDone", "Safe", "Extra"), fixed=False), "mismatch")

    def test_fixed_run_drops_open_findings(self) -> None:
        self.assertEqual(run.judge(self.check, self.seen("NeverDone"), fixed=True), "ok")
        self.assertEqual(run.judge(self.check, self.seen("NeverDone", "Safe"), fixed=True), "mismatch")

    def test_liveness_passes_only_clean(self) -> None:
        self.assertEqual(run.judge(self.liveness, self.seen(), fixed=False), "ok")
        self.assertEqual(run.judge(self.liveness, self.seen("Eventually"), fixed=False), "mismatch")

    def test_seeded_needs_its_violation(self) -> None:
        self.assertEqual(run.judge(self.seeded, self.seen("Other"), fixed=False), "ok")
        self.assertEqual(run.judge(self.seeded, self.seen(), fixed=False), "mismatch")
        self.assertEqual(run.judge(self.seeded, self.seen(run.DEADLOCK), fixed=False), "mismatch")

    def test_tooling_error_is_never_a_match(self) -> None:
        self.assertEqual(run.judge(self.liveness, run.Outcome("boom", frozenset(), None, ()), fixed=False), "tooling")


class ExitCodeTests(unittest.TestCase):
    @staticmethod
    def result(status: str) -> run.Result:
        return run.Result("r", status, frozenset(), frozenset(), None, 0.0, "", (), None)

    def test_precedence(self) -> None:
        self.assertEqual(run.exit_code([self.result("ok")]), 0)
        self.assertEqual(run.exit_code([self.result("ok"), self.result("tooling")]), 2)
        self.assertEqual(run.exit_code([self.result("tooling"), self.result("mismatch")]), 1)
        self.assertEqual(run.exit_code([]), 0)


class EnsureJarTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.target = Path(self._tmp.name)
        self.good = b"good jar"
        self.good_sha = run.hashlib.sha256(self.good).hexdigest()
        self.calls = 0

    def fetch_returning(self, *payloads: bytes):
        def fetch(url: str, dest: Path) -> None:
            dest.write_bytes(payloads[min(self.calls, len(payloads) - 1)])
            self.calls += 1
        return fetch

    def test_cached_good_jar_is_not_downloaded(self) -> None:
        (self.target / "tla2tools.jar").write_bytes(self.good)
        run.ensure_jar(self.target, self.fetch_returning(b"unused"), self.good_sha)
        self.assertEqual(self.calls, 0)

    def test_bad_cache_is_replaced(self) -> None:
        (self.target / "tla2tools.jar").write_bytes(b"corrupt")
        jar = run.ensure_jar(self.target, self.fetch_returning(self.good), self.good_sha)
        self.assertEqual(jar.read_bytes(), self.good)
        self.assertEqual(self.calls, 1)

    def test_second_download_is_the_last(self) -> None:
        jar = run.ensure_jar(self.target, self.fetch_returning(b"bad", self.good), self.good_sha)
        self.assertEqual(jar.read_bytes(), self.good)
        self.assertEqual(self.calls, 2)

    def failing_then(self, payload: bytes | None):
        def fetch(url: str, dest: Path) -> None:
            self.calls += 1
            if self.calls == 1 or payload is None:
                raise run.ToolingError("connection reset")
            dest.write_bytes(payload)
        return fetch

    def test_failed_download_is_retried_once(self) -> None:
        jar = run.ensure_jar(self.target, self.failing_then(self.good), self.good_sha)
        self.assertEqual(jar.read_bytes(), self.good)
        self.assertEqual(self.calls, 2)

    def test_two_failed_downloads_fail_with_the_cause(self) -> None:
        with self.assertRaises(run.ToolingError) as ctx:
            run.ensure_jar(self.target, self.failing_then(None), self.good_sha)
        self.assertEqual(self.calls, 2)
        self.assertIn("connection reset", str(ctx.exception))

    def test_two_bad_downloads_fail(self) -> None:
        with self.assertRaises(run.ToolingError):
            run.ensure_jar(self.target, self.fetch_returning(b"bad"), self.good_sha)
        self.assertEqual(self.calls, 2)
        self.assertFalse((self.target / "tla2tools.jar").exists())


class ExecuteTests(unittest.TestCase):
    """execute() with TLC replaced by a small Python process, so no Java is needed."""

    def setUp(self) -> None:
        d = ExpectedDir()
        self.addCleanup(d.close)
        self.base = d.path
        _, (self.check, _, _) = d.load()
        target = tempfile.TemporaryDirectory()
        self.addCleanup(target.cleanup)
        self.target = Path(target.name)
        for name, value in (("TARGET", self.target), ("tlc_command", run.tlc_command)):
            self.addCleanup(setattr, run, name, getattr(run, name))
        run.TARGET = self.target
        self.commands: list[list[str]] = []

    def fake_tlc(self, script: str) -> None:
        def command(jar: Path, r: run.Run, cfg: Path, metadir: Path) -> list[str]:
            self.commands.append([str(cfg)])
            return [sys.executable, "-c", script]
        run.tlc_command = command

    def test_replayed_output_is_judged_and_logged(self) -> None:
        self.fake_tlc(f"import sys; text = open({str(TESTDATA / 'clean.out')!r}).read(); "
                      "sys.stdout.write(text.partition(chr(10))[2]); sys.exit(0)")
        clean = run.Run("demo-posix-check", "M", "check.cfg", "demo", "check", (), (), 1)
        result = run.execute(clean, Path("unused.jar"), self.base, fixed=False)
        self.assertEqual((result.status, result.observed, result.distinct_states), ("ok", frozenset(), 4))
        self.assertIsNotNone(result.log)
        assert result.log is not None
        self.assertIn("Model checking completed", result.log.read_text(encoding="utf-8"))

    def test_fixed_run_uses_a_derived_config(self) -> None:
        self.fake_tlc("import sys; sys.exit(0)")
        result = run.execute(self.check, Path("unused.jar"), self.base, fixed=True)
        self.assertEqual(result.name, "demo-posix-check-fixed")
        derived = Path(self.commands[0][0])
        self.assertEqual(derived, self.target / "cfg" / "demo-posix-check-fixed.cfg")
        self.assertIn("FIX_SAFE = TRUE", derived.read_text(encoding="utf-8"))
        self.assertEqual(result.status, "tooling", "a process that prints nothing is never a clean run")

    def test_timeout_is_reported_as_a_tooling_failure(self) -> None:
        self.fake_tlc("import time; time.sleep(30)")
        slow = run.Run("demo-posix-check", "M", "check.cfg", "demo", "check", ("NeverDone",), (), 0)
        result = run.execute(slow, Path("unused.jar"), self.base, fixed=False)
        self.assertEqual(result.status, "tooling")
        self.assertEqual(result.detail, "TIMEOUT after 0 min")


class MainTests(unittest.TestCase):
    """main() keeps its 0/1/2 contract when the environment fails."""

    def setUp(self) -> None:
        for name in ("ensure_jar", "execute"):
            self.addCleanup(setattr, run, name, getattr(run, name))
        self.addCleanup(setattr, run.shutil, "which", run.shutil.which)
        run.shutil.which = lambda _name: "java"

    def test_unwritable_target_is_exit_2(self) -> None:
        def ensure_jar() -> Path:
            raise PermissionError("target/tla is read-only")
        run.ensure_jar = ensure_jar
        self.assertEqual(run.main(["--expected", str(HERE / "expected.toml")]), 2)

    def test_interrupt_is_exit_2(self) -> None:
        run.ensure_jar = lambda: Path("unused.jar")

        def execute(*_args: object) -> run.Result:
            raise KeyboardInterrupt
        run.execute = execute
        self.assertEqual(run.main(["--expected", str(HERE / "expected.toml")]), 2)


class CommandTests(unittest.TestCase):
    def test_check_and_liveness_continue_seeded_halts(self) -> None:
        d = ExpectedDir()
        self.addCleanup(d.close)
        _, (check, liveness, seeded) = d.load()
        jar, cfg, meta = Path("j.jar"), Path("c.cfg"), Path("m")
        self.assertIn("-continue", run.tlc_command(jar, check, cfg, meta))
        self.assertIn("-continue", run.tlc_command(jar, liveness, cfg, meta))
        self.assertNotIn("-continue", run.tlc_command(jar, seeded, cfg, meta))
        self.assertNotIn("-deadlock", run.tlc_command(jar, check, cfg, meta))
        self.assertEqual(run.tlc_command(jar, check, cfg, meta)[-1], "M")


if __name__ == "__main__":
    unittest.main()
````

- [ ] **Step 3: Run them to see them fail**

Run: `python3 -m unittest discover -s models/lockproto -p "test_*.py"`
Expected: an error, `ModuleNotFoundError: No module named 'run'`.

- [ ] **Step 4: Write the runner**

Create `models/lockproto/run.py`:

````python
#!/usr/bin/env python3
"""Runner for the lock-protocol model check.

Design: docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md, Section 4.
Reads expected.toml, runs TLC for each selected run, judges each result against its
expectation, and exits 0 (every run matched), 1 (a run did not match), or 2 (a tooling
failure). Standard library only.
"""

from __future__ import annotations

import sys

if sys.version_info < (3, 11):
    sys.stderr.write("run.py: Python 3.11 or later is required (it reads TOML with tomllib)\n")
    sys.exit(2)

import argparse  # noqa: E402
import hashlib  # noqa: E402
import json  # noqa: E402
import re  # noqa: E402
import shutil  # noqa: E402
import subprocess  # noqa: E402
import time  # noqa: E402
import tomllib  # noqa: E402
import urllib.request  # noqa: E402
from dataclasses import dataclass  # noqa: E402
from pathlib import Path  # noqa: E402
from typing import Callable  # noqa: E402

# tla2tools.jar pin (TLC 2.19). v1.8.0 is a rolling prerelease whose asset changes; do not pin it.
TLA_TAG = "v1.7.4"
TLA_SHA256 = "936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
TLA_URL = f"https://github.com/tlaplus/tlaplus/releases/download/{TLA_TAG}/tla2tools.jar"

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
TARGET = REPO / "target" / "tla"

KINDS = ("check", "liveness", "seeded")
RUN_KEYS = {"name", "module", "config", "scenario", "kind", "violated", "open_findings", "timeout_minutes"}
FINDING_KEYS = {"name", "tracking", "fix_flag"}
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
FIX_FLAG = re.compile(r"FIX_[A-Z0-9_]+")

# TLC -tool message codes, measured against TLC 2.19 (tla2tools v1.7.4).
C_INVARIANT_INITIAL = 2107  # "Invariant X is violated by the initial state:"
C_INVARIANT = 2110  # "Invariant X is violated."
C_DEADLOCK = 2114  # "Deadlock reached."
C_TEMPORAL = 2116  # "Temporal properties were violated." (names no property)
C_BEHAVIOR = 2121  # "The behavior up to this point is:"
C_FINISHED = 2186  # "Finished in ..."; printed at the end of every run, errors included
C_SUCCESS = 2193  # "Model checking completed. No error has been found."
C_STATE = 2217  # one state of a trace (severity 4)
C_COUNTEREXAMPLE = 2264  # "The following behavior constitutes a counter-example:"
ALLOWED_ERROR_CODES = {C_INVARIANT_INITIAL, C_INVARIANT, C_DEADLOCK, C_TEMPORAL, C_BEHAVIOR, C_COUNTEREXAMPLE}
SEVERITY_ERROR = 1

DEADLOCK = "DEADLOCK"
TRACE_STATES_SHOWN = 60

# TLC .cfg keywords, each mapped to its singular form.
CFG_KEYWORDS = {
    "CONSTANT": "CONSTANT", "CONSTANTS": "CONSTANT", "INIT": "INIT", "NEXT": "NEXT",
    "SPECIFICATION": "SPECIFICATION", "INVARIANT": "INVARIANT", "INVARIANTS": "INVARIANT",
    "PROPERTY": "PROPERTY", "PROPERTIES": "PROPERTY", "SYMMETRY": "SYMMETRY",
    "CONSTRAINT": "CONSTRAINT", "CONSTRAINTS": "CONSTRAINT", "ACTION_CONSTRAINT": "ACTION_CONSTRAINT",
    "ACTION_CONSTRAINTS": "ACTION_CONSTRAINT", "VIEW": "VIEW", "CHECK_DEADLOCK": "CHECK_DEADLOCK",
    "POSTCONDITION": "POSTCONDITION", "ALIAS": "ALIAS",
}


class ExpectedError(Exception):
    """expected.toml or a file it names is invalid (exit code 2)."""


class ToolingError(Exception):
    """Java, the jar, or TLC failed in a way that says nothing about the model (exit code 2)."""


@dataclass(frozen=True)
class OpenFinding:
    name: str
    tracking: str
    fix_flag: str


@dataclass(frozen=True)
class Run:
    name: str
    module: str
    config: str
    scenario: str
    kind: str
    violated: tuple[str, ...]
    open_findings: tuple[OpenFinding, ...]
    timeout_minutes: int


@dataclass(frozen=True)
class Message:
    code: int
    severity: int
    text: str


@dataclass(frozen=True)
class Outcome:
    tooling_error: str | None
    observed: frozenset[str]
    distinct_states: int | None
    trace: tuple[str, ...]


@dataclass(frozen=True)
class Result:
    name: str
    status: str  # "ok", "mismatch", or "tooling"
    expected: frozenset[str]
    observed: frozenset[str]
    distinct_states: int | None
    seconds: float
    detail: str
    trace: tuple[str, ...]
    log: Path | None


# ---------------------------------------------------------------------------------------
# TLC configuration files


# A .cfg token: a block comment, a line comment, a string, `<-`, a word, or any other character.
_CFG_TOKEN = re.compile(r'\(\*.*?\*\)|\\\*[^\n]*|"(?:[^"\\\n]|\\.)*"|<-|[A-Za-z0-9_]+|\S', re.S)


def _is_comment(token: str) -> bool:
    return token.startswith(("(*", "\\*"))


def cfg_sections(text: str) -> dict[str, list[str]]:
    """Map each keyword of a TLC .cfg file (singular form) to the tokens that follow it.

    Comments are skipped, and a string literal is a value, never a keyword."""
    sections: dict[str, list[str]] = {}
    current: str | None = None
    for match in _CFG_TOKEN.finditer(text):
        token = match.group(0)
        if _is_comment(token):
            continue
        if token.startswith('"'):
            token = '""'
        if token in CFG_KEYWORDS:
            current = CFG_KEYWORDS[token]
            sections.setdefault(current, [])
        elif current is not None:
            sections[current].append(token)
    return sections


def cfg_properties(text: str) -> list[str]:
    return [t for t in cfg_sections(text).get("PROPERTY", []) if IDENT.fullmatch(t)]


def fixed_cfg_text(text: str, flags: list[str]) -> str:
    """Return the .cfg text with each fix flag switched from FALSE to TRUE (comments are left alone)."""
    for flag in flags:
        comments = [m.span() for m in _CFG_TOKEN.finditer(text) if _is_comment(m.group(0))]
        pattern = re.compile(rf"\b{re.escape(flag)}\s*=\s*FALSE\b")
        hits = [m for m in pattern.finditer(text) if not any(a <= m.start() < b for a, b in comments)]
        if len(hits) != 1:
            raise ExpectedError(f"fix flag {flag} must appear exactly once as '{flag} = FALSE' in the config "
                                "(outside comments)")
        text = text[: hits[0].start()] + f"{flag} = TRUE" + text[hits[0].end():]
    return text


# ---------------------------------------------------------------------------------------
# expected.toml


def _require(cond: bool, message: str) -> None:
    if not cond:
        raise ExpectedError(message)


def _str_list(value: object, where: str) -> tuple[str, ...]:
    _require(isinstance(value, list) and all(isinstance(v, str) and IDENT.fullmatch(v) for v in value),
             f"{where} must be a list of TLA+ identifiers")
    assert isinstance(value, list)
    _require(len(set(value)) == len(value), f"{where} has duplicates")
    return tuple(value)


def load_expected(path: Path) -> tuple[list[str], list[Run]]:
    """Load and validate expected.toml; module and config paths are resolved beside it."""
    base = path.parent
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as err:
        raise ExpectedError(f"cannot read {path}: {err}") from err

    _require(set(data) <= {"scenarios", "run"}, f"unknown top-level keys: {sorted(set(data) - {'scenarios', 'run'})}")
    scenarios = data.get("scenarios")
    _require(isinstance(scenarios, list) and scenarios and all(isinstance(s, str) for s in scenarios),
             "'scenarios' must be a non-empty list of names")
    assert isinstance(scenarios, list)
    _require(len(set(scenarios)) == len(scenarios), "'scenarios' has duplicates")
    for s in scenarios:
        _require(re.fullmatch(r"[a-z][a-z0-9-]*", s) is not None, f"scenario name {s!r} must be lowercase words joined by '-'")

    raw_runs = data.get("run", [])
    _require(isinstance(raw_runs, list), "'run' must be an array of tables")
    runs: list[Run] = []
    for i, raw in enumerate(raw_runs):
        _require(isinstance(raw, dict), f"run #{i + 1} must be a table")
        runs.append(_load_run(raw, i, scenarios, base))

    names = [r.name for r in runs]
    _require(len(set(names)) == len(names), f"duplicate run names: {sorted({n for n in names if names.count(n) > 1})}")
    for s in scenarios:
        _require(any(r.scenario == s for r in runs), f"scenario {s!r} has no runs")
    for run in runs:
        if run.kind == "seeded":
            open_names = {f.name for r in runs if r.scenario == run.scenario for f in r.open_findings}
            _require(run.violated[0] not in open_names,
                     f"{run.name}: its scenario carries an open finding on {run.violated[0]}, so the seed cannot be judged")
    return scenarios, runs


def _load_run(raw: dict, i: int, scenarios: list[str], base: Path) -> Run:
    where = f"run #{i + 1}"
    _require(set(raw) <= RUN_KEYS, f"{where}: unknown keys {sorted(set(raw) - RUN_KEYS)}")
    for key in ("name", "module", "config", "scenario", "kind"):
        _require(isinstance(raw.get(key), str) and raw[key], f"{where}: '{key}' must be a non-empty string")
    name, module, config, scenario, kind = (raw[k] for k in ("name", "module", "config", "scenario", "kind"))
    where = f"run {name!r}"

    _require(kind in KINDS, f"{where}: kind must be one of {KINDS}")
    _require(scenario in scenarios, f"{where}: scenario {scenario!r} is not in 'scenarios'")
    seed = r"-[A-Z][A-Z0-9_]*" if kind == "seeded" else ""
    _require(re.fullmatch(rf"{re.escape(scenario)}-[a-z0-9]+(-[a-z0-9]+)*-{kind}{seed}", name) is not None,
             f"{where}: name must be '<scenario>-<variant>-<kind>', plus '-<SEED_FLAG>' exactly when it is seeded")
    _require(IDENT.fullmatch(module) is not None and (base / f"{module}.tla").is_file(),
             f"{where}: module {module}.tla not found beside expected.toml")
    cfg_path = base / config
    _require(config.endswith(".cfg") and cfg_path.is_file(), f"{where}: config {config} not found")

    timeout = raw.get("timeout_minutes")
    _require(isinstance(timeout, int) and not isinstance(timeout, bool) and timeout >= 1,
             f"{where}: timeout_minutes must be a whole number of minutes, at least 1")

    if kind == "liveness":
        _require("violated" not in raw, f"{where}: a liveness run has no 'violated'")
        violated: tuple[str, ...] = ()
    else:
        _require("violated" in raw, f"{where}: a {kind} run needs 'violated'")
        violated = _str_list(raw["violated"], f"{where}: violated")
        if kind == "seeded":
            _require(len(violated) == 1, f"{where}: a seeded run names exactly one invariant or property")

    findings: list[OpenFinding] = []
    raw_findings = raw.get("open_findings", [])
    _require(isinstance(raw_findings, list), f"{where}: open_findings must be an array of tables")
    _require(kind != "seeded" or not raw_findings, f"{where}: a seeded run has no open_findings")
    for f in raw_findings:
        _require(isinstance(f, dict) and set(f) == FINDING_KEYS,
                 f"{where}: each open finding has exactly name, tracking, fix_flag")
        _require(all(isinstance(f[k], str) and f[k] for k in FINDING_KEYS), f"{where}: open finding fields must be non-empty strings")
        _require(IDENT.fullmatch(f["name"]) is not None, f"{where}: open finding name must be a TLA+ identifier")
        _require(FIX_FLAG.fullmatch(f["fix_flag"]) is not None, f"{where}: fix_flag must look like FIX_NAME")
        _require(f["name"] not in violated, f"{where}: {f['name']} is both expected and an open finding")
        findings.append(OpenFinding(f["name"], f["tracking"], f["fix_flag"]))
    _require(len({f.name for f in findings}) == len(findings), f"{where}: duplicate open findings")

    text = cfg_path.read_text(encoding="utf-8")
    sections = cfg_sections(text)
    _require("CHECK_DEADLOCK" not in sections, f"{where}: the config must not set CHECK_DEADLOCK (deadlock checking stays on)")
    properties = cfg_properties(text)
    temporal_run = kind == "liveness" or (kind == "seeded" and violated[0] in properties)
    if temporal_run:
        _require(len(properties) == 1, f"{where}: a run that checks a temporal property lists exactly one PROPERTY "
                                       "(TLC does not name the property it reports violated)")
        _require("SYMMETRY" not in sections, f"{where}: symmetry is unsound with liveness checking")
    if findings:
        fixed_cfg_text(text, [f.fix_flag for f in findings])  # raises if a flag is missing

    return Run(name, module, config, scenario, kind, violated, tuple(findings), timeout)


# ---------------------------------------------------------------------------------------
# TLC output


_MESSAGE = re.compile(r"@!@!@STARTMSG (\d+):(\d+) @!@!@\n(.*?)@!@!@ENDMSG \1 @!@!@", re.S)
_INVARIANT_NAME = re.compile(r"Invariant (\w+) is violated")
_DISTINCT = re.compile(r"([\d,]+) distinct states? found")


def parse_messages(output: str) -> list[Message]:
    output = output.replace("\r\n", "\n")
    return [Message(int(c), int(s), t.strip()) for c, s, t in _MESSAGE.findall(output)]


def distinct_states(messages: list[Message]) -> int | None:
    for m in reversed(messages):
        found = _DISTINCT.search(m.text)
        if found:
            return int(found.group(1).replace(",", ""))
    return None


def interpret(exit_code: int, output: str, properties: list[str]) -> Outcome:
    """Turn TLC's exit status and -tool output into the set of violations it reported."""
    messages = parse_messages(output)
    states = distinct_states(messages)
    trace = tuple(m.text for m in messages if m.code == C_STATE)[:TRACE_STATES_SHOWN]

    def tooling(reason: str) -> Outcome:
        return Outcome(reason, frozenset(), states, trace)

    codes = {m.code for m in messages}
    if C_FINISHED not in codes:
        return tooling("TLC did not finish (no 'Finished' message)")
    errors = [m for m in messages if m.severity == SEVERITY_ERROR and m.code not in ALLOWED_ERROR_CODES]
    if errors:
        return tooling(f"TLC error {errors[0].code}: {errors[0].text.splitlines()[0] if errors[0].text else ''}")

    observed = set()
    for m in messages:
        if m.code in (C_INVARIANT, C_INVARIANT_INITIAL):
            name = _INVARIANT_NAME.search(m.text)
            if not name:
                return tooling(f"cannot read the invariant name in: {m.text[:80]}")
            observed.add(name.group(1))
    if C_DEADLOCK in codes:
        observed.add(DEADLOCK)
    if C_TEMPORAL in codes:
        if len(properties) != 1:
            return tooling(f"TLC reported a temporal violation but the config has {len(properties)} properties")
        observed.add(properties[0])

    agrees = {
        0: C_SUCCESS in codes and not codes & {C_DEADLOCK, C_TEMPORAL},
        11: C_DEADLOCK in codes,
        12: bool(codes & {C_INVARIANT, C_INVARIANT_INITIAL}),
        13: C_TEMPORAL in codes,
    }
    if not agrees.get(exit_code, False):
        return tooling(f"TLC exit status {exit_code} does not match its messages")
    return Outcome(None, frozenset(observed), states, trace)


def expected_set(run: Run, fixed: bool) -> frozenset[str]:
    names = set(run.violated)
    if not fixed:
        names |= {f.name for f in run.open_findings}
    return frozenset(names)


def judge(run: Run, outcome: Outcome, fixed: bool) -> str:
    if outcome.tooling_error is not None:
        return "tooling"
    return "ok" if outcome.observed == expected_set(run, fixed) else "mismatch"


def exit_code(results: list[Result]) -> int:
    """A mismatch outranks a tooling failure: 1 if any run mismatched, else 2 if any failed, else 0."""
    statuses = {r.status for r in results}
    if "mismatch" in statuses:
        return 1
    if "tooling" in statuses:
        return 2
    return 0


# ---------------------------------------------------------------------------------------
# tla2tools.jar


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def download(url: str, dest: Path) -> None:
    tmp = dest.with_suffix(".part")
    try:
        with urllib.request.urlopen(url, timeout=120) as response, tmp.open("wb") as out:
            shutil.copyfileobj(response, out)
        tmp.replace(dest)
    except OSError as err:
        raise ToolingError(f"cannot download {url} to {dest}: {err}") from err


def ensure_jar(target: Path = TARGET, fetch: Callable[[str, Path], None] = download,
               expected_sha: str = TLA_SHA256) -> Path:
    """Return a tla2tools.jar whose SHA-256 matches the pin, downloading at most twice.

    A download that fails, or that yields the wrong hash, uses up one of the two attempts."""
    target.mkdir(parents=True, exist_ok=True)
    jar = target / "tla2tools.jar"
    if jar.is_file() and sha256(jar) == expected_sha:
        return jar
    mismatch = f"tla2tools.jar {TLA_TAG} does not match its pinned SHA-256"
    failure = mismatch
    for _ in range(2):
        jar.unlink(missing_ok=True)
        try:
            fetch(TLA_URL, jar)
        except ToolingError as err:
            failure = str(err)
            continue
        if jar.is_file() and sha256(jar) == expected_sha:
            return jar
        failure = mismatch
    jar.unlink(missing_ok=True)
    raise ToolingError(f"{failure} (after two download attempts)")


# ---------------------------------------------------------------------------------------
# Running TLC


def tlc_command(jar: Path, run: Run, cfg: Path, metadir: Path) -> list[str]:
    cmd = ["java", "-XX:+UseParallelGC", "-cp", str(jar), "tlc2.TLC", "-tool", "-workers", "auto",
           "-metadir", str(metadir), "-config", str(cfg)]
    if run.kind in ("check", "liveness"):
        cmd.append("-continue")
    cmd.append(run.module)
    return cmd


def execute(run: Run, jar: Path, base: Path, fixed: bool) -> Result:
    name = f"{run.name}-fixed" if fixed else run.name
    cfg = base / run.config
    text = cfg.read_text(encoding="utf-8")
    if fixed:
        text = fixed_cfg_text(text, [f.fix_flag for f in run.open_findings])
        cfg = TARGET / "cfg" / f"{name}.cfg"
        cfg.parent.mkdir(parents=True, exist_ok=True)
        cfg.write_text(text, encoding="utf-8")
    metadir = TARGET / "states" / name
    shutil.rmtree(metadir, ignore_errors=True)
    log = TARGET / "out" / f"{name}.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    expected = expected_set(run, fixed)

    start = time.monotonic()
    with log.open("w", encoding="utf-8") as out:
        proc = subprocess.Popen(tlc_command(jar, run, cfg, metadir), cwd=base, stdout=out,
                                stderr=subprocess.STDOUT)
        try:
            code = proc.wait(timeout=run.timeout_minutes * 60)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
            code = None
        except BaseException:  # KeyboardInterrupt: never leave TLC running
            proc.kill()
            proc.wait()
            raise
    seconds = time.monotonic() - start
    output = log.read_text(encoding="utf-8", errors="replace")

    if code is None:
        states = distinct_states(parse_messages(output))
        return Result(name, "tooling", expected, frozenset(), states, seconds,
                      f"TIMEOUT after {run.timeout_minutes} min", (), log)
    outcome = interpret(code, output, cfg_properties(text))
    status = judge(run, outcome, fixed)
    return Result(name, status, expected, outcome.observed, outcome.distinct_states, seconds,
                  outcome.tooling_error or "", outcome.trace, log)


def report(result: Result) -> None:
    def names(s: frozenset[str]) -> str:
        return ",".join(sorted(s)) or "-"

    states = "?" if result.distinct_states is None else f"{result.distinct_states:,}"
    print(f"{result.status.upper():8} {result.name}  expected={names(result.expected)}  "
          f"observed={names(result.observed)}  states={states}  {result.seconds:.0f}s")
    if result.status != "ok":
        if result.detail:
            print(f"         {result.detail}")
        for state in result.trace:
            print("         " + state.replace("\n", "\n         "))
        if result.log is not None:
            print(f"         full TLC output: {result.log}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Run the lock-protocol model check.")
    parser.add_argument("--expected", type=Path, default=HERE / "expected.toml")
    parser.add_argument("--scenario", help="run only this scenario")
    parser.add_argument("--list-scenarios", action="store_true", help="print the scenario names as JSON")
    args = parser.parse_args(argv)

    try:
        scenarios, runs = load_expected(args.expected)
    except ExpectedError as err:
        print(f"run.py: {err}", file=sys.stderr)
        return 2
    if args.list_scenarios:
        print(json.dumps(scenarios))
        return 0
    if args.scenario is not None and args.scenario not in scenarios:
        print(f"run.py: unknown scenario {args.scenario!r}; known: {', '.join(scenarios)}", file=sys.stderr)
        return 2
    selected = [r for r in runs if args.scenario is None or r.scenario == args.scenario]

    if shutil.which("java") is None:
        print("run.py: java is not on PATH (TLC needs Java 11 or later; CI uses Temurin 21)", file=sys.stderr)
        return 2
    try:
        jar = ensure_jar()
    except (ToolingError, OSError) as err:
        print(f"run.py: {err}", file=sys.stderr)
        return 2

    base = args.expected.resolve().parent
    results: list[Result] = []
    try:
        for run in selected:
            for fixed in (False, True) if run.open_findings else (False,):
                try:
                    result = execute(run, jar, base, fixed)
                except (OSError, ExpectedError) as err:
                    result = Result(run.name, "tooling", frozenset(), frozenset(), None, 0.0, str(err), (), None)
                report(result)
                results.append(result)
    except KeyboardInterrupt:
        print(f"run.py: interrupted after {len(results)} runs", file=sys.stderr)
        return 2
    code = exit_code(results)
    print(f"run.py: {len(results)} runs, exit {code}")
    return code


if __name__ == "__main__":
    sys.exit(main())
````

- [ ] **Step 5: Write the self-test model, its configurations, and the run list**

Create `models/lockproto/Smoke.tla`:

````tla
---- MODULE Smoke ----
\* Runner self-test model. It exercises every way run.py judges a run (a check run with
\* a witness and an open finding, its fix-flag run, a liveness run, and a seeded run)
\* without touching the lock protocol. It is not a protocol scenario.
EXTENDS Naturals

CONSTANTS SEED_OVERSHOOT, FIX_BOUND

VARIABLES x, reachedTwo

vars == <<x, reachedTwo>>

Limit == IF FIX_BOUND THEN 3 ELSE 4

Init == x = 0 /\ reachedTwo = FALSE

Step ==
    /\ x < Limit
    /\ x' = x + 1
    /\ reachedTwo' = (reachedTwo \/ x' = 2)

\* The seeded defect: jump past the limit.
Overshoot ==
    /\ SEED_OVERSHOOT
    /\ x = Limit
    /\ x' = Limit + 2
    /\ UNCHANGED reachedTwo

\* Final stuttering step, so a finished run is not a deadlock.
Finished == x >= Limit /\ UNCHANGED vars

Next == Step \/ Overshoot \/ Finished

Spec == Init /\ [][Next]_vars /\ WF_vars(Step)

\* Safety invariant; the seeded run must violate it.
WithinBound == x <= Limit

\* Safety invariant carried as an open finding; FIX_BOUND makes it hold.
AtMostThree == x <= 3

\* Reachability witness: must be reported violated.
NeverReachedTwo == ~reachedTwo

\* Liveness property.
ReachesLimit == <>(x >= Limit)
====
````

Create `models/lockproto/configs/selftest-check.cfg`:

````text
\* Runner self-test: check run (witness NeverReachedTwo, open finding AtMostThree).
SPECIFICATION Spec
CONSTANTS
    SEED_OVERSHOOT = FALSE
    FIX_BOUND = FALSE
INVARIANTS
    WithinBound
    AtMostThree
    NeverReachedTwo
````

Create `models/lockproto/configs/selftest-liveness.cfg`:

````text
\* Runner self-test: liveness run (one temporal property, no symmetry).
SPECIFICATION Spec
CONSTANTS
    SEED_OVERSHOOT = FALSE
    FIX_BOUND = FALSE
INVARIANT WithinBound
PROPERTY ReachesLimit
````

Create `models/lockproto/configs/selftest-seeded-SEED_OVERSHOOT.cfg`:

````text
\* Runner self-test: seeded run; SEED_OVERSHOOT must make WithinBound fail.
SPECIFICATION Spec
CONSTANTS
    SEED_OVERSHOOT = TRUE
    FIX_BOUND = FALSE
INVARIANT WithinBound
````

Create `models/lockproto/expected.toml`:

````toml
# Runs of the lock-protocol model check and what each must report.
# Format: design Section 4 (docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md).
# `scenarios` is the only place scenario names are defined; CI builds its matrix from it.

scenarios = ["selftest"]

# Runner self-test (Smoke.tla): exercises every way run.py judges a run. Not a protocol scenario.

[[run]]
name = "selftest-posix-check"
module = "Smoke"
config = "configs/selftest-check.cfg"
scenario = "selftest"
kind = "check"
violated = ["NeverReachedTwo"]
open_findings = [
    { name = "AtMostThree", tracking = "selftest: exercises the open-finding and fix-flag path", fix_flag = "FIX_BOUND" },
]
timeout_minutes = 2

[[run]]
name = "selftest-posix-liveness"
module = "Smoke"
config = "configs/selftest-liveness.cfg"
scenario = "selftest"
kind = "liveness"
timeout_minutes = 2

[[run]]
name = "selftest-posix-seeded-SEED_OVERSHOOT"
module = "Smoke"
config = "configs/selftest-seeded-SEED_OVERSHOOT.cfg"
scenario = "selftest"
kind = "seeded"
violated = ["WithinBound"]
timeout_minutes = 2
````

- [ ] **Step 6: Write the fixture modules and the recorder**

Create `models/lockproto/testdata/Fixture.tla`:

````tla
---- MODULE Fixture ----
\* Recorded-output fixture for test_run.py: each case in record_fixtures.py runs TLC on
\* this module (or Broken.tla) and saves the output, so the parser tests need no Java.
EXTENDS Naturals

VARIABLE x

Init == x = 0

Step == x < 3 /\ x' = x + 1

Stop == x = 3 /\ UNCHANGED x

Spec == Init /\ [][Step \/ Stop]_x /\ WF_x(Step)

NeverTwo == x /= 2

NeverThree == x /= 3

NotZero == x /= 0

Bounded == x <= 3

EventuallyFive == <>(x = 5)

EvalError == <<1, 2>>[x + 5] = 1
====
````

Create `models/lockproto/testdata/Broken.tla`:

````tla
---- MODULE Broken ----
\* Recorded-output fixture: a module TLC cannot parse.
EXTENDS Naturals

VARIABLE x

Init == x = 0

Next == x' = x + + 1
====
````

Create `models/lockproto/testdata/record_fixtures.py`:

````python
#!/usr/bin/env python3
"""Record TLC -tool output for test_run.py. Run once after changing the TLC pin; needs Java.

Each case writes testdata/<case>.out: the first line is 'exit=<status>', the rest is TLC's output.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import run  # noqa: E402

# case name -> (module, cfg text, -continue?)
CASES = {
    "clean": ("Fixture", "SPECIFICATION Spec\nINVARIANT Bounded\n", True),
    "continue_two_invariants": ("Fixture", "SPECIFICATION Spec\nINVARIANTS NeverTwo NeverThree\n", True),
    "halt_invariant": ("Fixture", "SPECIFICATION Spec\nINVARIANT NeverThree\n", False),
    "initial_invariant": ("Fixture", "SPECIFICATION Spec\nINVARIANT NotZero\n", True),
    "deadlock": ("Fixture", "INIT Init\nNEXT Step\n", True),
    "temporal": ("Fixture", "SPECIFICATION Spec\nPROPERTY EventuallyFive\n", True),
    "temporal_and_invariant": ("Fixture", "SPECIFICATION Spec\nINVARIANT NeverTwo\nPROPERTY EventuallyFive\n", True),
    "eval_error": ("Fixture", "SPECIFICATION Spec\nINVARIANT EvalError\n", True),
    "config_error": ("Fixture", "SPECIFICATION Spec\nINVARIANT Missing\n", True),
    "parse_error": ("Broken", "INIT Init\nNEXT Next\n", True),
}


def main() -> int:
    jar = run.ensure_jar()
    with tempfile.TemporaryDirectory() as tmp:
        for case, (module, cfg_text, cont) in CASES.items():
            cfg = Path(tmp) / f"{case}.cfg"
            cfg.write_text(cfg_text, encoding="utf-8")
            cmd = ["java", "-XX:+UseParallelGC", "-cp", str(jar), "tlc2.TLC", "-tool", "-workers", "1",
                   "-metadir", str(Path(tmp) / f"states-{case}"), "-config", str(cfg)]
            if cont:
                cmd.append("-continue")
            cmd.append(module)
            proc = subprocess.run(cmd, cwd=HERE, capture_output=True, text=True)
            output = (proc.stdout + proc.stderr).replace("\r\n", "\n")
            (HERE / f"{case}.out").write_text(f"exit={proc.returncode}\n{output}", encoding="utf-8", newline="\n")
            print(f"{case}: exit {proc.returncode}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
````

- [ ] **Step 7: Record the TLC fixtures (needs Java; downloads the pinned jar into `target/tla/`)**

Run: `python3 models/lockproto/testdata/record_fixtures.py`
Expected, exactly these lines:

```text
clean: exit 0
continue_two_invariants: exit 0
halt_invariant: exit 12
initial_invariant: exit 0
deadlock: exit 11
temporal: exit 13
temporal_and_invariant: exit 13
eval_error: exit 75
config_error: exit 151
parse_error: exit 150
```

Then run `ls models/lockproto/testdata` and expect the three source files plus ten `.out` files. If any exit status
differs, stop: the pinned TLC does not behave as measured, and the parser constants in `run.py` must be re-measured.

- [ ] **Step 8: Run the unit tests to see them pass**

Run: `python3 -m unittest discover -s models/lockproto -p "test_*.py"`
Expected: `Ran 55 tests` and `OK`.

- [ ] **Step 9: Run the self-test end to end**

Run: `python3 models/lockproto/run.py`
Expected (durations vary):

```text
OK       selftest-posix-check  expected=AtMostThree,NeverReachedTwo  observed=AtMostThree,NeverReachedTwo  states=5  ...s
OK       selftest-posix-check-fixed  expected=NeverReachedTwo  observed=NeverReachedTwo  states=4  ...s
OK       selftest-posix-liveness  expected=-  observed=-  states=5  ...s
OK       selftest-posix-seeded-SEED_OVERSHOOT  expected=WithinBound  observed=WithinBound  states=6  ...s
run.py: 4 runs, exit 0
```

Then run `python3 models/lockproto/run.py --list-scenarios` (expect `["selftest"]`) and
`python3 models/lockproto/run.py --scenario nope; echo $?` (expect `run.py: unknown scenario 'nope'; known: selftest`
and `2`).

- [ ] **Step 10: Commit**

```bash
git add .gitignore models/lockproto
git commit -m "model: TLC runner with unit tests and the selftest scenario (lock-model plan 1, task 1)"
```

---

### Task 2: `just` recipes, recommended tools, and the model README

**Design oracle:** Section 4 (recipes; `just check` stays Java-free; recommended-tools entries for Java and Python).

**Files:**
- Modify: `justfile` (insert before the `# Mutation testing over the engine` recipe)
- Modify: `.claude/recommended-tools.json` (append two entries)
- Create: `models/lockproto/README.md`

- [ ] **Step 0: State check**

Run: `grep -n "^# Mutation testing over the engine" justfile; grep -c '"name"' .claude/recommended-tools.json`
Expected: one line number, and `9`.

- [ ] **Step 1: Add the recipes**

In `justfile`, insert these lines immediately before the line `# Mutation testing over the engine`:

```text
# --- Lock-protocol model check (docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md)
# Needs Java 11+ and Python 3.11+; not part of `just check`. See models/lockproto/README.md.

python := env_var_or_default("PYTHON", "python3")

# Run the model check: every scenario, or one (`just model selftest`)
model scenario="":
    {{python}} models/lockproto/run.py {{ if scenario == "" { "" } else { "--scenario " + scenario } }}

# Unit tests of the model runner (Python only, no Java)
model-test:
    {{python}} -m unittest discover -s models/lockproto -p "test_*.py"

```

- [ ] **Step 2: Run the recipes**

Run: `just model selftest`, then `just model-test`, then `just model nope; echo $?`
Expected: the four `OK` lines and `run.py: 4 runs, exit 0`; then `Ran 55 tests` and `OK`; then `run.py: unknown
scenario 'nope'; known: selftest`, a `just` error line, and a non-zero status.

- [ ] **Step 3: Add the recommended tools**

In `.claude/recommended-tools.json`, append these two objects to the array, after the `cargo-mutants` entry (keep
the file's two-space indentation and its existing `—` escapes):

```json
  {
    "name": "java",
    "why": "Runs TLC (the TLA+ model checker) for the lock-protocol model check: `just model` downloads a pinned tla2tools.jar and needs Java 11 or later on PATH. CI uses Temurin 21. Not needed by `just check`.",
    "install": "winget install EclipseAdoptium.Temurin.21.JDK",
    "in_path": "java"
  },
  {
    "name": "python3",
    "why": "Runs models/lockproto/run.py (the model-check runner) and its unit tests (`just model`, `just model-test`). Needs Python 3.11 or later, for tomllib. Not needed by `just check`.",
    "install": "winget install Python.Python.3.12",
    "in_path": "python3"
  }
```

Run: `python3 -c "import json; print(len(json.load(open('.claude/recommended-tools.json'))))"`
Expected: `11`.

- [ ] **Step 4: Write the README**

Create `models/lockproto/README.md`:

````markdown
# Lock-protocol model check

A TLA+/PlusCal model of Flux's target-lock protocol, checked with TLC, plus Rust probes that confirm the model's
filesystem assumptions on real operating systems. Design:
[`docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md`](../../docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md).

The work lands in three plans. Plan 1 (this state) provides the tooling: the runner, its self-test, the drift stamp
and traceability check, the filesystem probes, and CI. Plan 2 adds `FsModel.tla` and `LockProtocol.tla`; plan 3 adds
`Claims.tla`.

## Running it

| Command | What it does | Needs |
|---|---|---|
| `just model` | runs every run in `expected.toml` | Java 11+, Python 3.11+ |
| `just model <scenario>` | runs one scenario, for example `just model selftest` | Java 11+, Python 3.11+ |
| `just model-test` | unit tests of `run.py` (recorded TLC output, no Java) | Python 3.11+ |
| `just model-stamp` | runs `just model`, then rewrites the unit hashes in `trace.toml` if every run matched | Java, Python, Rust |

`run.py` downloads `tla2tools.jar` (release and SHA-256 pinned in `run.py`) into `target/tla/`, checks its hash
before every use, and writes each run's full TLC output to `target/tla/out/<run>.log`. It exits 0 when every run
matched its expectation, 1 when one did not, and 2 for a tooling failure (Java missing, download or checksum
failure, a TLC error, or a timeout). `just check` runs the Rust side (the stamp and traceability test and the
filesystem probes) and needs no Java or Python.

To set `PYTHON` to another interpreter: `PYTHON=python just model`. Run one `just model` at a time in a checkout:
runs write their TLC state and logs under `target/tla/`, keyed by run name.

## Files

| File | Purpose |
|---|---|
| `expected.toml` | every TLC run and what it must report (design Section 4) |
| `configs/*.cfg` | one TLC configuration per run |
| `run.py`, `test_run.py` | the runner and its unit tests |
| `testdata/` | recorded TLC output for the unit tests; `record_fixtures.py` re-records it after the TLC pin changes |
| `Smoke.tla` | runner self-test model (the `selftest` scenario); not part of the protocol |
| `spec-sections.stamp` | the spec file and the spec headings the model encodes (design Section 9) |
| `trace.toml` | every unit of those headings, its hash, and the labels that implement it (design Section 9.1) |

## How a run is judged

- `check` runs use `-continue` and must report exactly their `violated` witnesses plus their `open_findings`.
- `liveness` runs check one temporal property (TLC does not name the property it reports violated, so a config
  lists exactly one) and every safety invariant in the config, with no symmetry; they pass with no violation other
  than their `open_findings`.
- `seeded` runs stop at the first violation, which must be the one named.
- A run with open findings is run a second time with the findings' fix flags set (`<run>-fixed`), and must then
  report no open finding.
- TLC's deadlock check always stays on; a config may not set `CHECK_DEADLOCK`.

## Filesystem probes

`crates/flux-platform/tests/fs_semantics.rs` holds FS-1 to FS-11 (design Section 10). Each prints the filesystem type
of its scratch directory. The OS-native lock they probe covers the whole file: `flock` on Unix, and `LockFileEx` over
the full byte range on Windows; an implementation that locks a different range would not be covered by them. A failing probe is never weakened: reproduce it on a native local filesystem of that
platform, and if it still fails, the model's assumption is wrong and the model changes.

Measured while plan 1 was written (Windows 11 NTFS, rustc 1.98): a replacing rename onto a file that is open with
delete-sharing succeeds with `std::fs::rename` (POSIX-semantics rename) but fails with
`MoveFileExW(MOVEFILE_REPLACE_EXISTING)`, which refuses any open target. FS-6 and FS-7 test the POSIX-semantics rename,
the one the model's Windows row describes, and FS-7 prints what `MoveFileExW` does. The spec does not yet say which
API a replacing rename uses; plan 2 records this as a finding.

## Unverified assumptions

No probe can confirm these, so they are listed here rather than tested:

- weak file identity and case folding on FAT32/exFAT (no such filesystem on CI runners), including that, having no
  journal, they may lose entry operations out of order after a host crash;
- both crash rules of design Section 5.2, including which unflushed writes and entry operations survive a host
  crash (no probe can cut power; the rules follow the platforms' documented guarantees, spec Sections 166 to 168);
- whether an entry created or removed during a directory listing is returned (the model allows either);
- filesystems beyond the modelled variants (SMB, NFS), and clock behaviour beyond the liveness oracle.
````

- [ ] **Step 5: Commit**

```bash
git add justfile .claude/recommended-tools.json models/lockproto/README.md
git commit -m "model: just model/model-test recipes, recommended tools, README (lock-model plan 1, task 2)"
```

---

### Task 3: The spec-drift stamp and traceability test

**Design oracle:** Sections 9 and 9.1 (the stamp format, a heading's own text, units, the checks, the rewrite).

**Files:**
- Modify: `Cargo.toml` (workspace `toml`; root dev-dependencies)
- Create: `models/lockproto/spec-sections.stamp`, `models/lockproto/trace.toml`, `tests/model_stamp.rs`
- Modify: `justfile` (add `model-stamp` after `model-test`)
- Changed by cargo: `Cargo.lock`

- [ ] **Step 0: State check**

Run: `grep -n -A1 "^\[dev-dependencies\]" Cargo.toml; grep -n '^serde_json = "1"$' Cargo.toml; ls tests`
Expected: `[dev-dependencies]` followed by `criterion = { workspace = true }`; one `serde_json` line; `integration`.

- [ ] **Step 1: Add the dependencies**

In the root `Cargo.toml`, replace

```toml
[dev-dependencies]
criterion = { workspace = true }
```

with

```toml
[dev-dependencies]
blake3 = { workspace = true }
criterion = { workspace = true }
serde = { workspace = true }
toml = { workspace = true }
```

and in `[workspace.dependencies]`, directly after the line `serde_json = "1"`, add:

```toml
# models/lockproto/trace.toml parsing in tests/model_stamp.rs (lock-model design Section 9)
toml = "1.1"
```

- [ ] **Step 2: Create the stamp and the empty map**

Create `models/lockproto/spec-sections.stamp` with this single line:

````text
spec: FLUX_FULL_UPDATED_SPEC_V16.md
````

Create `models/lockproto/trace.toml`:

````toml
# Traceability map of the lock-protocol model (design Section 9.1).
#
# One [[unit]] per unit of each heading listed in spec-sections.stamp:
#
#   [[unit]]
#   heading = "## 240.5 `--break-lock`"   # the heading line exactly as in the spec
#   ordinal = 2                            # the unit's position in that heading's own text
#   quote = "Open the existing lock file"  # a sentence of the unit
#   hash = "<BLAKE3 hex>"                  # written by `just model-stamp`
#   labels = ["S240_5_s2"]                 # PlusCal labels that implement it, or
#   # not_modelled = "<reason>"            # instead of labels
#
# Headings enter the stamp as the model encodes them (plan 2 onward).
````

- [ ] **Step 3: Write the test**

The check's logic and its own tests live in one integration-test file, so there is no separate failing-test step: the
made-up-input tests at the bottom are the tests, and `model_trace_matches_spec` is the real check.

Create `tests/model_stamp.rs`:

````rust
//! Spec-drift stamp and traceability check for the lock-protocol model.
//!
//! Design: docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md, Sections 9
//! and 9.1. `models/lockproto/spec-sections.stamp` names the spec file and the headings the
//! model encodes; `models/lockproto/trace.toml` maps every unit of those headings to the
//! PlusCal labels that implement it (or to a reason it is not modelled) and carries each
//! unit's BLAKE3 hash. This test fails when the spec text, the map, or the model labels
//! drift apart. `just model-stamp` runs the ignored `rewrite_unit_hashes` test after a green
//! `just model`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const STAMP: &str = "models/lockproto/spec-sections.stamp";
const TRACE: &str = "models/lockproto/trace.toml";
const MODELS: &str = "models/lockproto";

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ------------------------------------------------------------------------------------------
// Spec text

/// A heading line of the spec and the lines of its own text (up to the next heading).
struct Section {
    heading: String,
    lines: Vec<String>,
}

fn is_heading(line: &str) -> bool {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ')
}

fn fence_marker(line: &str) -> Option<&'static str> {
    ["```", "~~~"].into_iter().find(|m| line.starts_with(m))
}

/// Split the spec into sections. Heading lines inside fenced code blocks are text, not headings.
fn sections(spec: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut fence: Option<&str> = None;
    for line in spec.lines() {
        match fence {
            Some(marker) if line.starts_with(marker) => fence = None,
            Some(_) => {}
            None => {
                if let Some(marker) = fence_marker(line) {
                    fence = Some(marker);
                } else if is_heading(line) {
                    out.push(Section { heading: line.to_string(), lines: Vec::new() });
                    continue;
                }
            }
        }
        if let Some(section) = out.last_mut() {
            section.lines.push(line.to_string());
        }
    }
    out
}

/// A numbered step: optional indentation, digits, a dot, and a space (`1. Open`, `    2.  Take`).
fn is_step(line: &str) -> bool {
    let text = line.trim_start();
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && text[digits..].starts_with(". ")
}

/// The units of a heading's own text (design Section 9.1): blocks separated by blank lines or
/// fence-marker lines (which belong to no unit), except that each numbered step starts a new
/// unit and keeps the indented lines that follow it, even across blank lines. A unit's text is
/// its lines joined by '\n'. The numbered steps of Sections 21.1 and 240.1 sit inside fences, so
/// a fence does not stop a step from being its own unit.
fn units(lines: &[String]) -> Vec<String> {
    let mut units: Vec<Vec<&str>> = Vec::new();
    let mut in_step = false;
    let mut after_blank = true;
    for line in lines {
        if line.trim().is_empty() || fence_marker(line.trim_start()).is_some() {
            after_blank = true;
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let step = is_step(line);
        let continues = !step && if in_step { indented } else { !after_blank };
        if !continues || units.is_empty() {
            units.push(Vec::new());
            in_step = step;
        }
        if let Some(unit) = units.last_mut() {
            unit.push(line);
        }
        after_blank = false;
    }
    units.into_iter().map(|u| u.join("\n")).collect()
}

fn unit_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ------------------------------------------------------------------------------------------
// Stamp, trace map, and model labels

struct Stamp {
    spec: String,
    headings: Vec<String>,
}

fn parse_stamp(text: &str) -> Result<Stamp, String> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let spec = lines
        .next()
        .and_then(|l| l.strip_prefix("spec: "))
        .ok_or("the stamp's first line must be 'spec: <path>'")?
        .trim()
        .to_string();
    let headings: Vec<String> = lines.map(str::to_string).collect();
    if let Some(bad) = headings.iter().find(|h| !is_heading(h)) {
        return Err(format!("stamp line is not a heading line: {bad:?}"));
    }
    Ok(Stamp { spec, headings })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Trace {
    #[serde(default)]
    unit: Vec<TraceUnit>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceUnit {
    heading: String,
    ordinal: usize,
    quote: String,
    hash: String,
    #[serde(default)]
    labels: Vec<String>,
    not_modelled: Option<String>,
}

fn is_label_name(token: &str) -> bool {
    let rest = match token.strip_prefix('S') {
        Some(rest) => rest,
        None => return false,
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && rest[digits..].starts_with('_') && rest.len() > digits + 1
}

/// PlusCal labels named after spec steps (`S<section>_<step>:`, design Section 6.1).
fn labels_in(tla: &str) -> BTreeSet<String> {
    let bytes = tla.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut found = BTreeSet::new();
    let mut i = 0;
    while i < bytes.len() {
        if !is_word(bytes[i]) || (i > 0 && is_word(bytes[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_word(bytes[i]) {
            i += 1;
        }
        let token = &tla[start..i];
        let mut j = i;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        let colon = bytes.get(j) == Some(&b':');
        let next = bytes.get(j + 1).copied();
        if is_label_name(token) && colon && next != Some(b'=') && next != Some(b':') {
            found.insert(token.to_string());
        }
    }
    found
}

/// Everything the check reads, so the logic can be tested without touching the repository.
struct Inputs<'a> {
    spec: &'a str,
    stamp: &'a Stamp,
    trace: &'a str,
    tla: &'a [String],
}

fn check(inputs: &Inputs<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    let all = sections(inputs.spec);
    let trace: Trace = match toml::from_str(inputs.trace) {
        Ok(t) => t,
        Err(e) => return vec![format!("{TRACE} does not parse: {e}")],
    };

    let mut stamped: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for heading in &inputs.stamp.headings {
        let matches: Vec<&Section> = all.iter().filter(|s| &s.heading == heading).collect();
        if stamped.contains_key(heading.as_str()) {
            problems.push(format!("the stamp lists {heading:?} twice"));
        } else if matches.len() != 1 {
            problems
                .push(format!("the spec has {} heading lines equal to {heading:?}", matches.len()));
        } else {
            stamped.insert(heading, units(&matches[0].lines));
        }
    }

    let mut seen: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    let mut mapped_labels = BTreeSet::new();
    for entry in &trace.unit {
        let at = format!("{} unit {}", entry.heading, entry.ordinal);
        let Some(units) = stamped.get(entry.heading.as_str()) else {
            problems.push(format!(
                "{at}: heading is not listed in the stamp (or not found in the spec)"
            ));
            continue;
        };
        if !seen.entry(entry.heading.as_str()).or_default().insert(entry.ordinal) {
            problems.push(format!("{at}: listed twice"));
        }
        let Some(text) = entry.ordinal.checked_sub(1).and_then(|i| units.get(i)) else {
            problems.push(format!("{at}: the heading has only {} units", units.len()));
            continue;
        };
        if entry.quote.trim().is_empty() || !squash(text).contains(&squash(&entry.quote)) {
            problems.push(format!("{at}: quote {:?} is not in the unit", entry.quote));
        }
        if entry.hash != unit_hash(text) {
            problems.push(format!(
                "{at}: spec text changed; re-check labels [{}] against it, then run `just model-stamp`",
                entry.labels.join(", ")
            ));
        }
        match (entry.labels.is_empty(), &entry.not_modelled) {
            (false, None) => mapped_labels.extend(entry.labels.iter().cloned()),
            (true, Some(reason)) if !reason.trim().is_empty() => {}
            _ => problems
                .push(format!("{at}: give either non-empty labels or a not_modelled reason")),
        }
    }
    for (heading, units) in &stamped {
        let have = seen.get(heading).cloned().unwrap_or_default();
        let missing: Vec<usize> = (1..=units.len()).filter(|n| !have.contains(n)).collect();
        if !missing.is_empty() {
            problems.push(format!("{heading}: units {missing:?} have no trace.toml entry"));
        }
    }

    let model_labels: BTreeSet<String> = inputs.tla.iter().flat_map(|t| labels_in(t)).collect();
    for label in model_labels.difference(&mapped_labels) {
        problems.push(format!("label {label} is in a model file but not in trace.toml"));
    }
    for label in mapped_labels.difference(&model_labels) {
        problems.push(format!("label {label} is in trace.toml but in no model file"));
    }
    problems
}

/// Replace each unit's `hash = "..."` line with the hash of the current spec text.
fn rewrite_hashes(trace_text: &str, spec: &str) -> Result<String, String> {
    let trace: Trace = toml::from_str(trace_text).map_err(|e| e.to_string())?;
    let all = sections(spec);
    let hashes: Vec<Option<String>> = trace
        .unit
        .iter()
        .map(|entry| {
            let section = all.iter().find(|s| s.heading == entry.heading)?;
            let text = units(&section.lines).into_iter().nth(entry.ordinal.checked_sub(1)?)?;
            Some(unit_hash(&text))
        })
        .collect();
    let mut out = String::new();
    let mut index: Option<usize> = None;
    for line in trace_text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("[[unit]]") {
            index = Some(index.map_or(0, |i| i + 1));
        }
        let is_hash = trimmed.strip_prefix("hash").is_some_and(|r| r.trim_start().starts_with('='));
        if let (true, Some(Some(hash))) = (is_hash, index.and_then(|i| hashes.get(i))) {
            writeln!(out, "hash = \"{hash}\"").map_err(|e| e.to_string())?;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    // The edit is line-based, so prove it changed the hashes and nothing else (a multi-line
    // string with a line starting `hash =` would otherwise be rewritten too).
    let rewritten: Trace = toml::from_str(&out).map_err(|e| format!("rewritten map: {e}"))?;
    let only_hashes = rewritten.unit.len() == trace.unit.len()
        && rewritten.unit.iter().zip(&trace.unit).zip(&hashes).all(|((new, old), hash)| {
            new.heading == old.heading
                && new.ordinal == old.ordinal
                && new.quote == old.quote
                && new.labels == old.labels
                && new.not_modelled == old.not_modelled
                && Some(&new.hash) == hash.as_ref().or(Some(&old.hash))
        });
    if !only_hashes {
        return Err(
            "rewriting the hash lines would change more than the hashes; fix trace.toml by hand"
                .into(),
        );
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------
// The repository check

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn model_files(root: &Path) -> Vec<String> {
    let mut texts = Vec::new();
    for entry in std::fs::read_dir(root.join(MODELS)).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "tla") {
            texts.push(read(&path));
        }
    }
    texts
}

#[test]
fn model_trace_matches_spec() {
    let root = repo();
    let stamp = parse_stamp(&read(&root.join(STAMP))).unwrap();
    let spec_path = root.join(&stamp.spec);
    assert!(
        spec_path.is_file(),
        "{STAMP} names {}, which does not exist; update the stamp's 'spec:' line",
        stamp.spec
    );
    let problems = check(&Inputs {
        spec: &read(&spec_path),
        stamp: &stamp,
        trace: &read(&root.join(TRACE)),
        tla: &model_files(&root),
    });
    assert!(problems.is_empty(), "model traceability problems:\n  {}", problems.join("\n  "));
}

#[test]
#[ignore = "rewrites trace.toml; run through `just model-stamp` after a green `just model`"]
fn rewrite_unit_hashes() {
    let root = repo();
    let stamp = parse_stamp(&read(&root.join(STAMP))).unwrap();
    let spec = read(&root.join(&stamp.spec));
    let trace_path = root.join(TRACE);
    let rewritten = rewrite_hashes(&read(&trace_path), &spec).unwrap();
    std::fs::write(&trace_path, rewritten).unwrap();
}

// ------------------------------------------------------------------------------------------
// Tests of the check itself, on small made-up inputs

const SPEC: &str = "\
# 1. Top

## 1.1 Alpha

First paragraph of alpha
continues here.

Second paragraph.

``` text
## 9.9 Not a heading
still fenced

after a blank line in the fence
```

## 1.2 Steps

Intro before the steps:

1. Open the file.
   Continue step one.
2. Take the lock.

   Step two after a blank line.
3. Write.
Not indented, so a new unit.

## 1.2.1 Child
Child text.

## 1.3 Fenced steps

In this order:

``` text
1. acquire the lock
   without waiting
2. mark the prior operation
```

After the fence.

## 1.4 Indented list

    1.  First item
        more of it
    2.  Second item
";

fn stamp(headings: &[&str]) -> Stamp {
    Stamp {
        spec: "SPEC.md".to_string(),
        headings: headings.iter().map(|h| h.to_string()).collect(),
    }
}

fn section_units(heading: &str) -> Vec<String> {
    let all = sections(SPEC);
    let section = all.iter().find(|s| s.heading == heading).unwrap();
    units(&section.lines)
}

fn entry(heading: &str, ordinal: usize, quote: &str, labels: &[&str]) -> String {
    let text = &section_units(heading)[ordinal - 1];
    let labels: Vec<String> = labels.iter().map(|l| format!("{l:?}")).collect();
    format!(
        "[[unit]]\nheading = {heading:?}\nordinal = {ordinal}\nquote = {quote:?}\nhash = \"{}\"\nlabels = [{}]\n\n",
        unit_hash(text),
        labels.join(", ")
    )
}

#[test]
fn fenced_heading_is_not_a_heading() {
    let headings: Vec<String> = sections(SPEC).into_iter().map(|s| s.heading).collect();
    assert_eq!(
        headings,
        [
            "# 1. Top",
            "## 1.1 Alpha",
            "## 1.2 Steps",
            "## 1.2.1 Child",
            "## 1.3 Fenced steps",
            "## 1.4 Indented list"
        ]
    );
}

#[test]
fn units_split_at_blank_lines_and_fences() {
    assert_eq!(
        section_units("## 1.1 Alpha"),
        [
            "First paragraph of alpha\ncontinues here.",
            "Second paragraph.",
            "## 9.9 Not a heading\nstill fenced",
            "after a blank line in the fence",
        ]
    );
}

#[test]
fn numbered_steps_inside_a_fence_are_units() {
    assert_eq!(
        section_units("## 1.3 Fenced steps"),
        [
            "In this order:",
            "1. acquire the lock\n   without waiting",
            "2. mark the prior operation",
            "After the fence.",
        ]
    );
}

#[test]
fn indented_numbered_steps_are_units() {
    assert_eq!(
        section_units("## 1.4 Indented list"),
        ["    1.  First item\n        more of it", "    2.  Second item"]
    );
}

#[test]
fn numbered_steps_are_units_and_keep_indented_lines() {
    let got = section_units("## 1.2 Steps");
    assert_eq!(
        got,
        [
            "Intro before the steps:",
            "1. Open the file.\n   Continue step one.",
            "2. Take the lock.\n   Step two after a blank line.",
            "3. Write.",
            "Not indented, so a new unit.",
        ]
    );
}

#[test]
fn child_heading_text_is_not_part_of_its_parent() {
    assert!(section_units("## 1.2 Steps").iter().all(|u| !u.contains("Child text")));
    assert_eq!(section_units("## 1.2.1 Child"), ["Child text."]);
}

#[test]
fn labels_are_found_and_near_misses_are_not() {
    let tla = "S240_5_s6a: x := 1;\nS97_1_ancestor:\n  y := S99_check;\nS12_x := 3;\nS1_a::\nStep: z := 0;\nXS1_a: w := 1;";
    let got: Vec<String> = labels_in(tla).into_iter().collect();
    assert_eq!(got, ["S240_5_s6a", "S97_1_ancestor"]);
}

#[test]
fn complete_map_passes() {
    let stamp = stamp(&["## 1.2 Steps"]);
    let mut trace = String::new();
    trace += &entry("## 1.2 Steps", 1, "Intro before", &["S1_2_intro"]);
    trace += &entry("## 1.2 Steps", 2, "Open the file. Continue", &["S1_2_s1"]);
    trace += &entry("## 1.2 Steps", 3, "Take the lock.", &["S1_2_s2"]);
    trace += &entry("## 1.2 Steps", 4, "3. Write.", &["S1_2_s3"]);
    trace += "[[unit]]\nheading = \"## 1.2 Steps\"\nordinal = 5\nquote = \"Not indented\"\n";
    let _ = writeln!(trace, "hash = \"{}\"", unit_hash(&section_units("## 1.2 Steps")[4]));
    trace += "not_modelled = \"prose only\"\n";
    let tla =
        vec!["S1_2_intro: a := 1; S1_2_s1: b := 1; S1_2_s2: c := 1; S1_2_s3: d := 1;".to_string()];
    let problems = check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &tla });
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn missing_unit_changed_text_and_stray_labels_are_reported() {
    let stamp = stamp(&["## 1.1 Alpha"]);
    let mut trace = entry("## 1.1 Alpha", 1, "First paragraph", &["S1_1_first"]);
    trace = trace.replacen("hash = \"", "hash = \"00", 1);
    let tla = vec!["S1_1_first: a := 1; S1_1_extra: b := 1;".to_string()];
    let problems = check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &tla });
    let text = problems.join("\n");
    assert!(text.contains("units [2, 3, 4] have no trace.toml entry"), "{text}");
    assert!(text.contains("unit 1: spec text changed; re-check labels [S1_1_first]"), "{text}");
    assert!(text.contains("label S1_1_extra is in a model file but not in trace.toml"), "{text}");
}

#[test]
fn quote_must_come_from_its_unit() {
    let stamp = stamp(&["## 1.2.1 Child"]);
    let trace = entry("## 1.2.1 Child", 1, "Take the lock", &["S1_2_1_x"]);
    let tla = vec!["S1_2_1_x: a := 1;".to_string()];
    let problems = check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &tla });
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].contains("is not in the unit"));
}

#[test]
fn stamp_heading_must_exist_once() {
    let problems = check(&Inputs {
        spec: SPEC,
        stamp: &stamp(&["## 9.9 Not a heading", "## 1.2.1 Child", "## 1.2.1 Child"]),
        trace: "",
        tla: &[],
    });
    let text = problems.join("\n");
    assert!(
        text.contains("the spec has 0 heading lines equal to \"## 9.9 Not a heading\""),
        "{text}"
    );
    assert!(text.contains("the stamp lists \"## 1.2.1 Child\" twice"), "{text}");
}

#[test]
fn labels_and_not_modelled_are_exclusive() {
    let stamp = stamp(&["## 1.2.1 Child"]);
    let trace = entry("## 1.2.1 Child", 1, "Child text", &[]);
    let problems = check(&Inputs { spec: SPEC, stamp: &stamp, trace: &trace, tla: &[] });
    assert!(
        problems.iter().any(|p| p.contains("either non-empty labels or a not_modelled reason")),
        "{problems:#?}"
    );
}

#[test]
fn stamp_format() {
    let parsed = parse_stamp("spec: FLUX.md\n## 96.1 Locks\n").unwrap();
    assert_eq!(parsed.spec, "FLUX.md");
    assert_eq!(parsed.headings, ["## 96.1 Locks"]);
    assert!(parse_stamp("## 96.1 Locks\n").is_err());
    assert!(parse_stamp("spec: FLUX.md\nnot a heading\n").is_err());
}

#[test]
fn rewrite_refuses_to_touch_a_multi_line_string() {
    let trace = "[[unit]]\nheading = \"## 1.2.1 Child\"\nordinal = 1\n\
                 quote = \"\"\"Child\nhash = \"x\"\n\"\"\"\nhash = \"stale\"\nlabels = [\"S1_2_1_x\"]\n";
    assert!(rewrite_hashes(trace, SPEC).is_err());
}

#[test]
fn rewrite_updates_only_hash_lines() {
    let mut trace = entry("## 1.2.1 Child", 1, "Child text", &["S1_2_1_x"]);
    let good = unit_hash("Child text.");
    trace = trace.replace(&good, "stale");
    let rewritten = rewrite_hashes(&format!("# map\n{trace}"), SPEC).unwrap();
    assert!(rewritten.starts_with("# map\n[[unit]]\n"));
    assert!(rewritten.contains(&format!("hash = \"{good}\"")));
    assert!(!rewritten.contains("stale"));
}
````

- [ ] **Step 4: Run it**

Run: `cargo test --test model_stamp`
Expected: `test result: ok. 16 passed; 0 failed; 1 ignored` (the ignored one is `rewrite_unit_hashes`).

- [ ] **Step 5: Prove the step rule is load-bearing**

Temporarily change the last line of `is_step` from `digits > 0 && text[digits..].starts_with(". ")` to
`false && digits > 0`, run `cargo test --test model_stamp`, and expect exactly four failures:
`complete_map_passes`, `indented_numbered_steps_are_units`, `numbered_steps_are_units_and_keep_indented_lines`, and
`numbered_steps_inside_a_fence_are_units`. Restore the line and rerun: 16 passed.

- [ ] **Step 6: Add the `model-stamp` recipe**

In `justfile`, directly after the `model-test` recipe added in Task 2, add:

```text
# Rewrite trace.toml's unit hashes; runs the whole model check first and stops if it fails
model-stamp:
    {{python}} models/lockproto/run.py
    cargo test --test model_stamp -- --ignored rewrite_unit_hashes --exact

```

Run (`trace.toml` is not committed yet, so compare against a copy rather than with `git diff`):
`cp models/lockproto/trace.toml "${TMPDIR:-/tmp}/trace.before" && just model-stamp && cmp "${TMPDIR:-/tmp}/trace.before" models/lockproto/trace.toml && echo UNCHANGED`
Expected: `run.py: 4 runs, exit 0`, then `test rewrite_unit_hashes ... ok` with `16 filtered out`, then `UNCHANGED`.

- [ ] **Step 7: Format and lint as the gate does**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`
Expected: no output from `fmt`; clippy finishes with no warnings.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock justfile models/lockproto/spec-sections.stamp models/lockproto/trace.toml tests/model_stamp.rs
git commit -m "model: spec-drift stamp and traceability test (lock-model plan 1, task 3)"
```

---

### Task 4: The filesystem probes FS-1 to FS-11

**Design oracle:** Section 10 (the probe table, preconditions, filesystem type, triage) and Section 5.1 (the rows each
probe grounds).

**Files:**
- Modify: `Cargo.toml` (workspace `rustix` and `windows-sys`)
- Modify: `crates/flux-platform/Cargo.toml` (dev-dependencies)
- Create: `crates/flux-platform/tests/fs_semantics.rs`
- Changed by cargo: `Cargo.lock`

- [ ] **Step 0: State check**

Run: `cat crates/flux-platform/Cargo.toml; ls crates/flux-platform/tests 2>&1`
Expected: only the `[package]` table; `No such file or directory`.

- [ ] **Step 1: Add the dependencies**

In the root `Cargo.toml` `[workspace.dependencies]`, directly after the `toml = "1.1"` line added in Task 3, add:

```toml
# crates/flux-platform/tests/fs_semantics.rs probes (lock-model design Section 10)
rustix = { version = "1.1", features = ["fs"] }
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_Storage_FileSystem", "Win32_System_IO"] }
```

Append to `crates/flux-platform/Cargo.toml`:

```toml

# The filesystem probes of the lock-protocol model (tests/fs_semantics.rs,
# lock-model design Section 10).
[dev-dependencies]
tempfile = { workspace = true }

[target.'cfg(unix)'.dev-dependencies]
rustix = { workspace = true }

[target.'cfg(windows)'.dev-dependencies]
windows-sys = { workspace = true }
```

- [ ] **Step 2: Write the probes**

Create `crates/flux-platform/tests/fs_semantics.rs`:

````rust
//! Filesystem probes FS-1 to FS-11 for the lock-protocol model.
//!
//! Design: docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md, Section 10.
//! Each probe confirms one assumption the model's filesystem (models/lockproto/FsModel.tla,
//! Section 5.1) makes about the platform it runs on. A failing probe is never weakened to
//! pass: first check the filesystem type it prints (a runner's overlay or network filesystem is
//! not the platform's native one), reproduce on a native local filesystem, and if it still
//! fails, the model's assumption for that platform is wrong and the model is fixed.
//!
//! The "OS-native lock" is the handle-scoped lock the model assumes: `flock` on Unix and
//! `LockFileEx` on Windows (never `fcntl` record locks, which a process loses when it closes any
//! handle to the file).

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::Path;

use tempfile::TempDir;

fn scratch() -> TempDir {
    let dir = tempfile::tempdir().expect("create a scratch directory");
    println!("scratch filesystem: {}", platform::fs_type(dir.path()));
    dir
}

fn create(path: &Path) -> File {
    OpenOptions::new().read(true).write(true).create_new(true).open(path).expect("create file")
}

#[cfg(unix)]
mod platform {
    use std::fs::{File, OpenOptions};
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use rustix::fs::{CWD, FlockOperation, RenameFlags};

    pub fn open(path: &Path) -> File {
        OpenOptions::new().read(true).write(true).open(path).expect("open file")
    }

    /// Take the OS-native lock without waiting; true if it was granted.
    pub fn try_lock(file: &File) -> bool {
        match rustix::fs::flock(file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => true,
            Err(e) if e == rustix::io::Errno::WOULDBLOCK => false,
            Err(e) => panic!("flock failed unexpectedly: {e}"),
        }
    }

    /// Rename that refuses to replace an existing target.
    pub fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
        rustix::fs::renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE).map_err(Into::into)
    }

    pub fn identity_of_handle(file: &File) -> (u64, u64) {
        let meta = file.metadata().expect("fstat");
        (meta.dev(), meta.ino())
    }

    pub fn identity_of_name(path: &Path) -> (u64, u64) {
        let meta = std::fs::metadata(path).expect("stat");
        (meta.dev(), meta.ino())
    }

    #[cfg(target_os = "linux")]
    pub fn fs_type(path: &Path) -> String {
        match rustix::fs::statfs(path) {
            Ok(st) => format!("statfs f_type 0x{:x}", st.f_type),
            Err(_) => "unknown".to_string(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn fs_type(path: &Path) -> String {
        match rustix::fs::statfs(path) {
            Ok(st) => {
                let name: Vec<u8> =
                    st.f_fstypename.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
                String::from_utf8_lossy(&name).into_owned()
            }
            Err(_) => "unknown".to_string(),
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::fs::{File, OpenOptions};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, DeleteFileW, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, GetFileInformationByHandle, GetVolumeInformationW, GetVolumePathNameW,
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, MOVEFILE_REPLACE_EXISTING,
        MoveFileExW,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    fn wide(path: &Path) -> Vec<u16> {
        OsStr::new(path).encode_wide().chain(Some(0)).collect()
    }

    fn check(ok: i32) -> std::io::Result<()> {
        if ok != 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    }

    /// Open an existing file; `share_delete` chooses whether others may rename or delete it.
    pub fn open_shared(path: &Path, share_delete: bool) -> File {
        let mut share = FILE_SHARE_READ | FILE_SHARE_WRITE;
        if share_delete {
            share |= FILE_SHARE_DELETE;
        }
        OpenOptions::new().read(true).write(true).share_mode(share).open(path).expect("open file")
    }

    pub fn open(path: &Path) -> File {
        open_shared(path, true)
    }

    /// Take the OS-native lock without waiting; true if it was granted. Like `flock`, it covers
    /// the whole file: the full 64-bit byte range from offset 0.
    pub fn try_lock(file: &File) -> bool {
        // SAFETY: an all-zero OVERLAPPED is valid (offset 0, no event); the handle is open.
        let ok = unsafe {
            let mut overlapped: OVERLAPPED = std::mem::zeroed();
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
        ok != 0
    }

    /// Rename; `replace` chooses MOVEFILE_REPLACE_EXISTING.
    pub fn rename(from: &Path, to: &Path, replace: bool) -> std::io::Result<()> {
        let flags = if replace { MOVEFILE_REPLACE_EXISTING } else { 0 };
        // SAFETY: both paths are NUL-terminated UTF-16 buffers that outlive the call.
        check(unsafe { MoveFileExW(wide(from).as_ptr(), wide(to).as_ptr(), flags) })
    }

    pub fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
        rename(from, to, false)
    }

    pub fn delete(path: &Path) -> std::io::Result<()> {
        // SAFETY: the path is a NUL-terminated UTF-16 buffer that outlives the call.
        check(unsafe { DeleteFileW(wide(path).as_ptr()) })
    }

    pub fn identity_of_handle(file: &File) -> (u64, u64) {
        // SAFETY: an all-zero BY_HANDLE_FILE_INFORMATION is valid; the handle is open.
        let info = unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            check(GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info))
                .expect("GetFileInformationByHandle");
            info
        };
        let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
        (u64::from(info.dwVolumeSerialNumber), index)
    }

    pub fn identity_of_name(path: &Path) -> (u64, u64) {
        identity_of_handle(&open(path))
    }

    pub fn fs_type(path: &Path) -> String {
        let mut root = [0u16; 261];
        let mut name = [0u16; 64];
        // SAFETY: the buffers are writable and their lengths are passed; null pointers are
        // allowed for the outputs this call does not need.
        let ok = unsafe {
            GetVolumePathNameW(wide(path).as_ptr(), root.as_mut_ptr(), root.len() as u32) != 0
                && GetVolumeInformationW(
                    root.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    name.as_mut_ptr(),
                    name.len() as u32,
                ) != 0
        };
        if !ok {
            return "unknown".to_string();
        }
        let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
        String::from_utf16_lossy(&name[..end])
    }
}

use platform::{identity_of_handle, identity_of_name, open, rename_no_replace, try_lock};

#[test]
fn fs1_second_exclusive_create_fails() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let _first = create(&path);
    let second = OpenOptions::new().write(true).create_new(true).open(&path);
    assert_eq!(second.err().map(|e| e.kind()), Some(ErrorKind::AlreadyExists));
}

#[test]
fn fs2_no_replace_rename_fails_when_target_exists() {
    let dir = scratch();
    let (from, to) = (dir.path().join("from"), dir.path().join("to"));
    drop(create(&from));
    drop(create(&to));
    assert!(rename_no_replace(&from, &to).is_err(), "no-replace rename replaced an existing name");
    assert!(from.exists() && to.exists());
}

#[test]
fn fs3_lock_fails_while_another_handle_holds_it() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let holder = create(&path);
    assert!(try_lock(&holder), "first lock must be granted");
    let other = open(&path);
    assert!(!try_lock(&other), "second handle got a lock another handle holds");
}

#[cfg(unix)]
#[test]
fn fs4_rename_of_open_locked_file_succeeds() {
    let dir = scratch();
    let (path, moved) = (dir.path().join("a.flux-lock"), dir.path().join("a.flux-lock.moved"));
    let holder = create(&path);
    assert!(try_lock(&holder));
    assert!(!try_lock(&open(&path)), "precondition: the lock is held");
    std::fs::rename(&path, &moved).expect("rename an open, locked file");
    assert!(!path.exists() && moved.exists());
}

#[cfg(unix)]
#[test]
fn fs5_unlink_of_open_file_succeeds_and_handle_still_writes() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;

    let mut handle = create(&path);
    std::fs::remove_file(&path).expect("unlink an open file");
    assert!(!path.exists());
    handle.write_all(b"record").expect("write to the unnamed file");
    handle.seek(SeekFrom::Start(0)).unwrap();
    let mut back = String::new();
    handle.read_to_string(&mut back).unwrap();
    assert_eq!(back, "record");
    assert_eq!(handle.metadata().unwrap().nlink(), 0, "the object has no name left");
}

#[cfg(windows)]
#[test]
fn fs6_file_open_without_delete_sharing_cannot_be_renamed_deleted_or_replaced() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let other = dir.path().join("other");
    drop(create(&path));
    drop(create(&other));
    let _held = platform::open_shared(&path, false);
    let moved = dir.path().join("moved");
    assert!(platform::rename(&path, &moved, false).is_err(), "rename succeeded");
    assert!(platform::delete(&path).is_err(), "delete succeeded");
    assert!(std::fs::rename(&other, &path).is_err(), "replacing rename succeeded");
    assert!(path.exists() && other.exists());
}

#[cfg(windows)]
#[test]
fn fs7_file_open_with_delete_sharing_can_be_renamed_deleted_and_replaced() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let moved = dir.path().join("moved");
    drop(create(&path));
    let held = platform::open_shared(&path, true);
    platform::rename(&path, &moved, false).expect("rename a file open with delete-sharing");
    assert!(!path.exists() && moved.exists());

    platform::delete(&moved).expect("delete a file open with delete-sharing");
    let name_gone = OpenOptions::new().write(true).create_new(true).open(&moved).is_ok();
    println!(
        "delete of an open file: {}",
        if name_gone {
            "name removed at once (POSIX semantics)"
        } else {
            "name pending until close"
        }
    );
    drop(held);

    let target = dir.path().join("target");
    let source = dir.path().join("source");
    drop(create(&target));
    drop(create(&source));
    let _held_target = platform::open_shared(&target, true);
    let legacy = platform::rename(&source, &target, true);
    println!("MoveFileExW(MOVEFILE_REPLACE_EXISTING) onto an open, delete-shared file: {legacy:?}");
    // The model's replacing rename is the POSIX-semantics rename that std::fs::rename uses; the
    // legacy MoveFileExW replace above refuses any open target (measured on Windows 11 NTFS).
    std::fs::rename(&source, &target).expect("replacing rename onto a shared-delete file");
    assert!(!source.exists());
}

#[test]
fn fs8_name_replaced_by_new_file_reports_new_identity() {
    let dir = scratch();
    let (path, fresh) = (dir.path().join("a.flux-lock"), dir.path().join("fresh"));
    let old = create(&path);
    let old_id = identity_of_handle(&old);
    drop(create(&fresh));
    // Keep the old object open so its identity cannot be reused while we compare.
    std::fs::rename(&fresh, &path).expect("replace the name with a new file");
    assert_ne!(identity_of_name(&path), old_id, "a replaced name reported the old identity");
}

#[test]
fn fs9_closing_a_handle_releases_its_lock() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let holder = create(&path);
    assert!(try_lock(&holder));
    assert!(!try_lock(&open(&path)), "precondition: the lock is held");
    drop(holder);
    assert!(try_lock(&open(&path)), "the lock survived its handle's close");
}

#[test]
fn fs10_listing_returns_entries_present_throughout() {
    let dir = scratch();
    for name in ["a.flux-lock", "b.flux-lock"] {
        drop(create(&dir.path().join(name)));
    }
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["a.flux-lock", "b.flux-lock"]);
}

#[test]
fn fs11_lock_is_scoped_to_its_handle() {
    let dir = scratch();
    let path = dir.path().join("a.flux-lock");
    let holder = create(&path);
    assert!(try_lock(&holder));
    drop(open(&path)); // open and close a second handle to the same file
    assert!(!try_lock(&open(&path)), "closing another handle released the lock");
    drop(holder);
}
````

- [ ] **Step 3: Run them on this machine**

Run: `cargo test -p flux-platform --test fs_semantics -- --nocapture`
Expected: `test result: ok. 9 passed` (Linux and macOS run FS-1 to FS-5 and FS-8 to FS-11; Windows runs FS-1 to FS-3
and FS-6 to FS-11). Every probe prints `scratch filesystem: <type>`; on Windows FS-7 also prints how `DeleteFileW`
and `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` behaved. A failure is triaged as design Section 10 says, never by
editing the probe.

- [ ] **Step 4: Lint for the other platforms if their targets are installed**

Run: `rustup target list --installed`, then, for each of `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, and
`x86_64-pc-windows-msvc` that is listed: `cargo clippy -p flux-platform --tests --target <target> -- -D warnings`
Expected: each finishes with no warnings. (CI runs the probes on all three operating systems in any case.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/flux-platform/Cargo.toml crates/flux-platform/tests/fs_semantics.rs
git commit -m "model: filesystem probes FS-1 to FS-11 (lock-model plan 1, task 4)"
```

---

### Task 5: The model CI workflow

**Design oracle:** Section 4 ("Recipes and CI": the `plan`, `scenario`, and `model-gate` jobs; permissions; change
detection; the empty-matrix rule).

**Files:**
- Create: `.github/workflows/model.yml`

- [ ] **Step 0: State check**

Run: `ls .github/workflows`
Expected: `ci.yml`, `dependabot-automerge.yml`, `docs.yml`, `release.yml` (no `model.yml`).

- [ ] **Step 1: Write the workflow**

Create `.github/workflows/model.yml`:

````yaml
name: Model

# Lock-protocol model check: docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md,
# Section 4. Runs on every pull request and push to main so that `model-gate` always reports and
# can be a required check; the scenario jobs run only when the change touches the model, the spec,
# the filesystem probes, or this workflow.

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  workflow_dispatch:

permissions:
  contents: read

jobs:
  plan:
    name: Plan
    runs-on: ubuntu-latest
    outputs:
      matrix: ${{ steps.plan.outputs.matrix }}
    steps:
      - uses: actions/checkout@v7
        with:
          fetch-depth: 0
      - uses: actions/setup-python@v7
        with:
          python-version: "3.12"
      - name: Runner unit tests
        run: python -m unittest discover -s models/lockproto -p "test_*.py"
      - name: Decide which scenarios to run
        id: plan
        env:
          EVENT: ${{ github.event_name }}
          PR_BASE: ${{ github.event.pull_request.base.sha }}
          PR_HEAD: ${{ github.event.pull_request.head.sha }}
          PUSH_BEFORE: ${{ github.event.before }}
        run: |
          set -euo pipefail
          # touching: true (a relevant file changed), false (none did), unknown (cannot tell: run everything)
          touching=unknown
          changed=""
          if [ "$EVENT" = "pull_request" ]; then
            if changed=$(git diff --name-only "$PR_BASE...$PR_HEAD"); then touching=true; fi
          elif [ "$EVENT" = "push" ] && [ -n "$PUSH_BEFORE" ] \
               && [ "$PUSH_BEFORE" != "0000000000000000000000000000000000000000" ] \
               && git cat-file -e "$PUSH_BEFORE^{commit}" 2>/dev/null; then
            if changed=$(git diff --name-only "$PUSH_BEFORE" "$GITHUB_SHA"); then touching=true; fi
          fi
          if [ "$touching" = true ]; then
            pattern='^(models/|FLUX_FULL_UPDATED_SPEC_V[^/]*\.md$|crates/flux-platform/tests/fs_semantics\.rs$|\.github/workflows/model\.yml$)'
            if ! printf '%s\n' "$changed" | grep -Eq "$pattern"; then touching=false; fi
          fi
          if [ "$touching" = false ]; then
            matrix='[]'
          else
            matrix=$(python models/lockproto/run.py --list-scenarios)
          fi
          echo "event=$EVENT touching=$touching matrix=$matrix"
          echo "matrix=$matrix" >> "$GITHUB_OUTPUT"

  scenario:
    name: Scenario ${{ matrix.scenario }}
    needs: plan
    # GitHub rejects an empty matrix, so the job is skipped when there is nothing to run.
    if: needs.plan.outputs.matrix != '[]'
    runs-on: ubuntu-latest
    timeout-minutes: 60
    strategy:
      fail-fast: false
      matrix:
        scenario: ${{ fromJSON(needs.plan.outputs.matrix) }}
    steps:
      - uses: actions/checkout@v7
      - uses: actions/setup-java@v6
        with:
          distribution: temurin
          java-version: "21"
      - uses: actions/setup-python@v7
        with:
          python-version: "3.12"
      - uses: taiki-e/install-action@just
      - name: Run the model check
        env:
          SCENARIO: ${{ matrix.scenario }}
        run: just model "$SCENARIO"
      - name: Upload TLC output
        if: failure()
        uses: actions/upload-artifact@v7
        with:
          name: tlc-output-${{ matrix.scenario }}
          path: target/tla/out/

  model-gate:
    name: Model gate
    needs: [plan, scenario]
    if: always()
    runs-on: ubuntu-latest
    steps:
      - name: Check the model jobs
        env:
          PLAN: ${{ needs.plan.result }}
          SCENARIO: ${{ needs.scenario.result }}
          MATRIX: ${{ needs.plan.outputs.matrix }}
        run: |
          echo "plan=$PLAN scenario=$SCENARIO matrix=$MATRIX"
          if [ "$PLAN" != "success" ]; then
            echo "::error::the plan job did not succeed"; exit 1
          fi
          if [ "$MATRIX" = "[]" ]; then
            if [ "$SCENARIO" = "skipped" ]; then exit 0; fi
            echo "::error::nothing needed to run, yet the scenario job ended as $SCENARIO"; exit 1
          fi
          if [ "$SCENARIO" != "success" ]; then
            echo "::error::a scenario job did not succeed ($SCENARIO)"; exit 1
          fi
````

- [ ] **Step 2: Lint it, if `actionlint` is available**

Run: `actionlint .github/workflows/model.yml`
Expected: no output, exit 0. (If `actionlint` is not installed, download the release binary for your platform from
`github.com/rhysd/actionlint/releases` into a temporary directory and run it from there; do not commit it.)

- [ ] **Step 3: Exercise the `plan` job's change detection locally**

Save this script as `$TMPDIR/plan_step.sh` (it is the workflow's `Decide which scenarios to run` step with `python`
spelled `python3`); it reads the workflow so it cannot drift from it:

```bash
python3 - <<'EOF' > "${TMPDIR:-/tmp}/plan_step.sh"
import re
text = open(".github/workflows/model.yml", encoding="utf-8").read()
body = re.search(r"id: plan\n.*?run: \|\n(.*?)\n\n  scenario:", text, re.S).group(1)
print("\n".join(line[10:] for line in body.splitlines()).replace("matrix=$(python ", "matrix=$(python3 "))
EOF
run_plan() { GITHUB_OUTPUT=/dev/null GITHUB_SHA=${GITHUB_SHA:-$(git rev-parse HEAD)} bash "${TMPDIR:-/tmp}/plan_step.sh" | tail -1; }
EVENT=pull_request PR_BASE=fcbc9e7~1 PR_HEAD=fcbc9e7 run_plan
EVENT=pull_request PR_BASE=46194ab~1 PR_HEAD=46194ab run_plan
EVENT=push PUSH_BEFORE=0000000000000000000000000000000000000000 run_plan
EVENT=push PUSH_BEFORE=fcbc9e7~1 GITHUB_SHA=fcbc9e7 run_plan
EVENT=workflow_dispatch run_plan
```

Expected, in order (`fcbc9e7` changed only a design doc; `46194ab` changed the spec):

```text
event=pull_request touching=false matrix=[]
event=pull_request touching=true matrix=["selftest"]
event=push touching=unknown matrix=["selftest"]
event=push touching=false matrix=[]
event=workflow_dispatch touching=unknown matrix=["selftest"]
```

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/model.yml
git commit -m "model: CI workflow with plan, scenario, and model-gate jobs (lock-model plan 1, task 5)"
```

---

### Task 6: Final gate and hand-off

**Files:** none new.

- [ ] **Step 1: Run the repository gate**

Run: `just check`
Expected: exit 0; nextest's summary reads `26 tests run: 26 passed, 1 skipped` (1 existing integration test, 16
stamp tests, 9 probes; nextest does not count the skipped `rewrite_unit_hashes` as run).

- [ ] **Step 2: Run the dependency audit**

Run: `cargo deny check`
Expected: `advisories ok, bans ok, licenses ok, sources ok` (the existing `license-not-encountered` warnings are
unchanged by this plan).

- [ ] **Step 3: Run the model and its unit tests once more**

Run: `just model && just model-test`
Expected: `run.py: 4 runs, exit 0`, then `OK`.

- [ ] **Step 4: Confirm the tree is clean and list the commits**

Run: `git status --short && git log --oneline -6`
Expected: no changes; the five task commits of this plan are the newest five.

- [ ] **Step 5: Report**

Report the five commit SHAs, the probe output lines that print the filesystem type and the Windows delete and rename
behaviour, and anything that differed from this plan. The `model` workflow first runs on GitHub when the branch is
pushed; pushing and opening a pull request are the owner's decision, not part of this plan. Plan 2 is written next.
