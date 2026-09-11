"""Unit tests for run.py. Standard library only; no Java needed (TLC output comes from testdata/)."""

from __future__ import annotations

import contextlib
import io
import json
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

    def test_check_needs_a_witness(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('violated = ["NeverDone"]', 'violated = []'),
                            "at least one reachability witness")

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

    def test_config_must_stay_inside_the_model_directory(self) -> None:
        for bad in ("/etc/seed.cfg", "../seed.cfg", "configs/../../seed.cfg", "C:/seed.cfg", r"configs\seed.cfg"):
            with self.subTest(config=bad):
                toml_string = '"' + bad.replace("\\", "\\\\") + '"'
                self.assertRejected(GOOD_EXPECTED.replace('"seed.cfg"', toml_string),
                                    "relative path inside the model directory")

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
        with self.assertRaises(run.ToolingError) as ctx:
            run.ensure_jar(self.target, self.fetch_returning(b"bad"), self.good_sha)
        self.assertIn(f"got {run.hashlib.sha256(b'bad').hexdigest()}", str(ctx.exception))
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

    def test_interrupt_kills_tlc_and_propagates(self) -> None:
        procs: list[FakeProc] = []

        class FakeProc:
            def __init__(self, *_args: object, **_kwargs: object) -> None:
                self.killed = False
                procs.append(self)

            def wait(self, timeout: float | None = None) -> int:
                if timeout is not None:
                    raise KeyboardInterrupt
                return -9

            def kill(self) -> None:
                self.killed = True

        self.addCleanup(setattr, run.subprocess, "Popen", run.subprocess.Popen)
        run.subprocess.Popen = FakeProc
        with self.assertRaises(KeyboardInterrupt):
            run.execute(self.check, Path("unused.jar"), self.base, fixed=False)
        self.assertEqual([p.killed for p in procs], [True])

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

    def main_with(self, statuses: dict[tuple[str, bool], str]) -> tuple[int, list[tuple[str, bool]]]:
        """Run main() over GOOD_EXPECTED with execute() stubbed: each (run, fixed) reports its status, default ok."""
        d = ExpectedDir()
        self.addCleanup(d.close)
        run.ensure_jar = lambda: Path("unused.jar")
        calls: list[tuple[str, bool]] = []

        def execute(r: run.Run, _jar: Path, _base: Path, fixed: bool) -> run.Result:
            calls.append((r.name, fixed))
            status = statuses.get((r.name, fixed), "ok")
            if status == "raise":
                raise OSError("cannot write the TLC log")
            return ExitCodeTests.result(status)
        run.execute = execute
        with contextlib.redirect_stdout(io.StringIO()):
            code = run.main(["--expected", str(d.path / "expected.toml")])
        return code, calls

    def test_all_runs_ok_is_exit_0(self) -> None:
        code, calls = self.main_with({})
        self.assertEqual(code, 0)
        # The run with an open finding runs twice: as written, then with its fix flag set.
        self.assertEqual(calls, [("demo-posix-check", False), ("demo-posix-check", True),
                                 ("demo-posix-liveness", False), ("demo-posix-seeded-SEED_X", False)])

    def test_a_mismatch_is_exit_1_even_beside_a_tooling_failure(self) -> None:
        code, _ = self.main_with({("demo-posix-check", True): "tooling", ("demo-posix-liveness", False): "mismatch"})
        self.assertEqual(code, 1)

    def test_a_tooling_failure_alone_is_exit_2(self) -> None:
        code, _ = self.main_with({("demo-posix-seeded-SEED_X", False): "tooling"})
        self.assertEqual(code, 2)

    def test_an_error_inside_one_run_is_exit_2_and_later_runs_still_run(self) -> None:
        code, calls = self.main_with({("demo-posix-check", False): "raise"})
        self.assertEqual(code, 2)
        self.assertEqual(calls[-1], ("demo-posix-seeded-SEED_X", False))

    def test_list_scenarios_prints_the_names_as_json_without_java(self) -> None:
        d = ExpectedDir(GOOD_EXPECTED.replace('scenarios = ["demo"]', 'scenarios = ["demo", "other"]') + """
[[run]]
name = "other-posix-liveness"
module = "M"
config = "live.cfg"
scenario = "other"
kind = "liveness"
timeout_minutes = 5
""")
        self.addCleanup(d.close)
        # The CI plan job lists scenarios before any Java or TLC setup, so listing must touch neither.
        run.shutil.which = lambda _name: None

        def refuse(*_args: object) -> Path:
            raise AssertionError("--list-scenarios must not fetch or run TLC")
        run.ensure_jar = refuse
        run.execute = refuse
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = run.main(["--expected", str(d.path / "expected.toml"), "--list-scenarios"])
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(out.getvalue()), ["demo", "other"])


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
