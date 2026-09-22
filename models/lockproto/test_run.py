"""Unit tests for run.py. Standard library only; no Java needed (TLC output comes from testdata/)."""

from __future__ import annotations

import contextlib
import io
import json
import re
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
open_findings = [{ name = "Safe", tracking = "TODO.md: demo", fix_flag = "FIX_SAFE" }]
constants = {}
timeout_minutes = 5

[[run]]
name = "demo-posix-liveness"
module = "M"
config = "live.cfg"
scenario = "demo"
kind = "liveness"
open_findings = [{ name = "LiveWitness", tracking = "TODO.md: demo-live", fix_flag = "FIX_LIVE" }]
constants = {}
timeout_minutes = 5

[[run]]
name = "demo-posix-seeded-SEED_X"
module = "M"
config = "seed.cfg"
scenario = "demo"
kind = "seeded"
violated = ["Other"]
constants = {}
timeout_minutes = 5

[[run]]
name = "demo-posix-witness-Other2"
module = "M"
config = "witness.cfg"
scenario = "demo"
kind = "witness"
violated = ["Other2"]
constants = {}
timeout_minutes = 5
"""

CFGS = {
    "check.cfg": "SPECIFICATION Spec\nCONSTANTS\n    FIX_SAFE = FALSE\nINVARIANTS Safe NeverDone\n",
    "live.cfg": "SPECIFICATION Spec\nCONSTANTS\n    FIX_LIVE = FALSE\nINVARIANT LiveWitness\nPROPERTY Eventually\n",
    "seed.cfg": "SPECIFICATION Spec\nCONSTANT FIX_SAFE = FALSE\nINVARIANT Other Safe\n",
    "witness.cfg": "SPECIFICATION Spec\nCONSTANT FIX_SAFE = FALSE\nINVARIANT Other2\n",
}

DEFAULT_MODULE = "---- MODULE M ----\n====\n"


def fixture(case: str) -> tuple[int, str]:
    text = (TESTDATA / f"{case}.out").read_text(encoding="utf-8")
    first, _, rest = text.partition("\n")
    return int(first.removeprefix("exit=")), rest


class ExpectedDir:
    """A temporary directory holding expected.toml, M.tla and the configs."""

    def __init__(self, expected: str = GOOD_EXPECTED, cfgs: dict[str, str] | None = None,
                 module_text: str | None = None) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.path = Path(self._tmp.name)
        (self.path / "M.tla").write_text(module_text or DEFAULT_MODULE, encoding="utf-8")
        for name, text in (CFGS if cfgs is None else cfgs).items():
            (self.path / name).write_text(text, encoding="utf-8")
        (self.path / "expected.toml").write_text(textwrap.dedent(expected), encoding="utf-8")

    def load(self) -> run.Expected:
        return run.load_expected(self.path / "expected.toml")

    def close(self) -> None:
        self._tmp.cleanup()


class LoadExpectedTests(unittest.TestCase):
    def load(self, expected: str, cfgs: dict[str, str] | None = None, module_text: str | None = None) -> run.Expected:
        d = ExpectedDir(expected, cfgs, module_text)
        self.addCleanup(d.close)
        return d.load()

    def assertRejected(self, expected: str, fragment: str, cfgs: dict[str, str] | None = None,
                        module_text: str | None = None) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            self.load(expected, cfgs, module_text)
        self.assertIn(fragment, str(ctx.exception))

    def test_good_file_loads(self) -> None:
        expected = self.load(GOOD_EXPECTED)
        self.assertEqual(expected.scenarios, ["demo"])
        self.assertEqual([r.kind for r in expected.runs], ["check", "liveness", "seeded", "witness"])
        self.assertEqual(expected.runs[0].open_findings, (run.OpenFinding("Safe", "TODO.md: demo", "FIX_SAFE"),))
        self.assertEqual(expected.runs[2].violated, ("Other",))

    def test_the_repository_file_loads(self) -> None:
        expected = run.load_expected(HERE / "expected.toml")
        self.assertTrue(expected.scenarios)
        self.assertTrue(expected.runs)

    def test_the_extended_repository_file_loads(self) -> None:
        expected = run.load_expected(HERE / "expected-extended.toml")
        # breaklock joined recovery here on 2026-09-22, when the deep liveness run moved off the
        # per-pull-request tier. This list is the file's contents, not a property of the loader.
        self.assertEqual(expected.scenarios, ["breaklock", "recovery"])
        self.assertTrue(expected.runs)

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
                            .replace('kind = "seeded"\nviolated = ["Other"]', 'kind = "check"'), "duplicate run names")

    def test_witness_is_accepted(self) -> None:
        expected = self.load(GOOD_EXPECTED)
        witness = expected.runs[3]
        self.assertEqual(witness.kind, "witness")
        self.assertEqual(witness.violated, ("Other2",))
        self.assertEqual(witness.name, "demo-posix-witness-Other2")

    def test_witness_name_suffix_must_equal_violated(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('"demo-posix-witness-Other2"', '"demo-posix-witness-WrongName"'),
                            "name must be")

    def test_witness_cfg_must_list_exactly_its_invariant(self) -> None:
        cfgs = dict(CFGS, **{"witness.cfg": "SPECIFICATION Spec\nCONSTANT FIX_SAFE = FALSE\nINVARIANT Other2 Extra\n"})
        self.assertRejected(GOOD_EXPECTED, "lists exactly one invariant", cfgs)

    def test_witness_cfg_must_not_declare_a_property(self) -> None:
        cfgs = dict(CFGS, **{"witness.cfg": CFGS["witness.cfg"] + "PROPERTY Eventually\n"})
        self.assertRejected(GOOD_EXPECTED, "must not declare a PROPERTY", cfgs)

    def test_check_with_violated_is_rejected(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('kind = "check"\nopen_findings',
                                                  'kind = "check"\nviolated = ["Anything"]\nopen_findings'),
                            "must not declare 'violated'")

    def test_liveness_with_violated_is_rejected(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('kind = "liveness"\nopen_findings',
                                                  'kind = "liveness"\nviolated = ["Anything"]\nopen_findings'),
                            "must not declare 'violated'")

    def test_open_findings_on_a_witness_is_rejected(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace(
            'kind = "witness"\nviolated = ["Other2"]',
            'kind = "witness"\nviolated = ["Other2"]\n'
            'open_findings = [{ name = "X", tracking = "y", fix_flag = "FIX_X" }]'),
            "only allowed for check and liveness runs")

    def test_seeded_needs_violated(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('kind = "seeded"\nviolated = ["Other"]', 'kind = "seeded"'),
                            "a seeded run needs 'violated'")

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

    def test_state_space_shrinking_sections_are_rejected_in_every_kind(self) -> None:
        # CONSTRAINT, ACTION_CONSTRAINT and VIEW all cut or merge states while every constant still matches
        # expected.toml, so a .cfg could make a failing run pass (design Section 4: the .cfg may only agree).
        for cfg in ("check.cfg", "live.cfg", "seed.cfg"):
            for section in ("CONSTRAINT Safe", "CONSTRAINTS Safe", "ACTION_CONSTRAINT Safe", "ACTION_CONSTRAINTS Safe",
                            "VIEW x", "TYPE Safe", "TYPE_CONSTRAINT Safe"):
                with self.subTest(cfg=cfg, section=section):
                    cfgs = dict(CFGS, **{cfg: CFGS[cfg] + section + "\n"})
                    self.assertRejected(GOOD_EXPECTED, "must not shrink the state space", cfgs)

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

    def test_check_config_must_not_declare_a_property(self) -> None:
        cfgs = dict(CFGS, **{"check.cfg": CFGS["check.cfg"] + "PROPERTY Eventually\n"})
        self.assertRejected(GOOD_EXPECTED, "must not declare a PROPERTY", cfgs)

    def test_unreached_well_formed_is_parsed(self) -> None:
        expected_toml = GOOD_EXPECTED.replace(
            'kind = "check"\nopen_findings',
            'kind = "check"\nunreached = [{ label = "S12_3", reason = "not reached in this variant" }]\nopen_findings')
        expected = self.load(expected_toml, module_text="---- MODULE M ----\nS12_3 == TRUE\n====\n")
        self.assertEqual(expected.runs[0].unreached, (("S12_3", "not reached in this variant"),))

    def test_unreached_must_be_a_list(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('kind = "check"\nopen_findings',
                                                  'kind = "check"\nunreached = "S12_3"\nopen_findings'),
                            "unreached must be an array of tables")

    def test_unreached_entry_needs_exactly_label_and_reason(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('kind = "check"\nopen_findings',
                                                  'kind = "check"\nunreached = [{ label = "S12_3" }]\nopen_findings'),
                            "exactly 'label' and 'reason'")

    def test_unreached_label_must_be_an_identifier(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace(
            'kind = "check"\nopen_findings',
            'kind = "check"\nunreached = [{ label = "not-a-label", reason = "x" }]\nopen_findings'),
            "must be a TLA+ identifier")

    def test_unreached_label_like_plain_recover_is_accepted(self) -> None:
        expected_toml = GOOD_EXPECTED.replace(
            'kind = "check"\nopen_findings',
            'kind = "check"\nunreached = [{ label = "plain_recover", reason = "x" }]\nopen_findings')
        expected = self.load(expected_toml, module_text="---- MODULE M ----\nplain_recover == TRUE\n====\n")
        self.assertEqual(expected.runs[0].unreached, (("plain_recover", "x"),))

    def test_unreached_reason_must_be_non_empty(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace(
            'kind = "check"\nopen_findings',
            'kind = "check"\nunreached = [{ label = "S12_3", reason = "" }]\nopen_findings'),
            "reason must be a non-empty string")

    def test_unreached_labels_must_be_unique(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace(
            'kind = "check"\nopen_findings',
            'kind = "check"\n'
            'unreached = [{ label = "S12_3", reason = "a" }, { label = "S12_3", reason = "b" }]\n'
            'open_findings'),
            "has duplicate labels")

    def test_unreached_is_not_allowed_on_a_seeded_run(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace(
            'violated = ["Other"]',
            'violated = ["Other"]\nunreached = [{ label = "S12_3", reason = "x" }]'),
            "only allowed for check and liveness runs")

    def test_never_reached_well_formed_is_parsed(self) -> None:
        d = ExpectedDir(GOOD_EXPECTED + '\n[[never_reached]]\nlabel = "S99_1"\nreason = "no scenario reaches it"\n',
                         module_text="---- MODULE M ----\nS99_1 == TRUE\n====\n")
        self.addCleanup(d.close)
        expected = d.load()
        self.assertEqual(expected.never_reached, (("S99_1", "no scenario reaches it"),))

    def test_never_reached_label_must_be_in_some_module(self) -> None:
        self.assertRejected(GOOD_EXPECTED + '\n[[never_reached]]\nlabel = "NoSuchLabel"\nreason = "x"\n',
                            "is not in the label universe of any run's module")

    def test_never_reached_label_must_be_an_identifier(self) -> None:
        self.assertRejected(GOOD_EXPECTED + '\n[[never_reached]]\nlabel = "not-an-ident"\nreason = "x"\n',
                            "must be a TLA+ identifier")

    def test_unknown_run_key(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace("timeout_minutes = 5\n\n[[run]]\nname = \"demo-posix-liveness\"",
                                                  "timeout_minutes = 5\nnotes = \"x\"\n\n[[run]]\nname = \"demo-posix-liveness\""),
                            "unknown keys")

    def test_unknown_top_level_key_is_rejected(self) -> None:
        self.assertRejected('bogus = "x"\n' + GOOD_EXPECTED, "unknown top-level keys")

    def test_duplicate_scenarios_are_rejected(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('scenarios = ["demo"]', 'scenarios = ["demo", "demo"]'),
                            "has duplicates")

    def test_timeout_must_be_a_positive_whole_number(self) -> None:
        for literal in ("0", "-1", "true", '"5"'):
            with self.subTest(literal=literal):
                self.assertRejected(GOOD_EXPECTED.replace("timeout_minutes = 5", f"timeout_minutes = {literal}"),
                                    "timeout_minutes must be a whole number")

    def test_duplicate_open_finding_names_are_rejected(self) -> None:
        expected = GOOD_EXPECTED.replace(
            'open_findings = [{ name = "Safe", tracking = "TODO.md: demo", fix_flag = "FIX_SAFE" }]',
            'open_findings = [{ name = "Safe", tracking = "TODO.md: demo", fix_flag = "FIX_SAFE" }, '
            '{ name = "Safe", tracking = "TODO.md: demo2", fix_flag = "FIX_SAFE" }]')
        self.assertRejected(expected, "duplicate open findings")

    def test_unknown_module_is_rejected(self) -> None:
        self.assertRejected(GOOD_EXPECTED.replace('module = "M"', 'module = "NoSuchModule"'),
                            "not found beside expected.toml")


class DeferredLoadTests(unittest.TestCase):
    """Load-time validation of the top-level 'deferred' list (design Section 4)."""

    MODULE_WITH_S99_1 = "---- MODULE M ----\nS99_1 == TRUE\n====\n"

    def load(self, expected: str, module_text: str | None = None) -> run.Expected:
        d = ExpectedDir(expected, module_text=module_text)
        self.addCleanup(d.close)
        return d.load()

    def assertRejected(self, expected: str, fragment: str, module_text: str | None = None) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            self.load(expected, module_text)
        self.assertIn(fragment, str(ctx.exception))

    def test_deferred_well_formed_is_parsed(self) -> None:
        expected = self.load(
            GOOD_EXPECTED + '\n[[deferred]]\nlabel = "S99_1"\nscenario = "future"\nreason = "not built yet"\n',
            module_text=self.MODULE_WITH_S99_1)
        self.assertEqual(expected.deferred, (("S99_1", "future", "not built yet"),))

    def test_duplicate_deferred_label_is_rejected(self) -> None:
        self.assertRejected(
            GOOD_EXPECTED + '\n[[deferred]]\nlabel = "S99_1"\nscenario = "future"\nreason = "a"\n'
                            '\n[[deferred]]\nlabel = "S99_1"\nscenario = "later"\nreason = "b"\n',
            "has duplicate labels", module_text=self.MODULE_WITH_S99_1)

    def test_deferred_label_also_in_never_reached_is_rejected(self) -> None:
        self.assertRejected(
            GOOD_EXPECTED + '\n[[never_reached]]\nlabel = "S99_1"\nreason = "x"\n'
                            '\n[[deferred]]\nlabel = "S99_1"\nscenario = "future"\nreason = "y"\n',
            "is also in 'never_reached'", module_text=self.MODULE_WITH_S99_1)

    def test_unknown_deferred_label_is_rejected(self) -> None:
        self.assertRejected(
            GOOD_EXPECTED + '\n[[deferred]]\nlabel = "NoSuchLabel"\nscenario = "future"\nreason = "x"\n',
            "is not in the label universe of any run's module")

    def test_deferred_scenario_already_built_is_rejected(self) -> None:
        self.assertRejected(
            GOOD_EXPECTED + '\n[[deferred]]\nlabel = "S99_1"\nscenario = "demo"\nreason = "x"\n',
            "is in 'scenarios'", module_text=self.MODULE_WITH_S99_1)

    def test_deferred_empty_reason_is_rejected(self) -> None:
        self.assertRejected(
            GOOD_EXPECTED + '\n[[deferred]]\nlabel = "S99_1"\nscenario = "future"\nreason = ""\n',
            "reason must be a non-empty string", module_text=self.MODULE_WITH_S99_1)

    def test_deferred_missing_reason_is_rejected(self) -> None:
        self.assertRejected(
            GOOD_EXPECTED + '\n[[deferred]]\nlabel = "S99_1"\nscenario = "future"\n',
            "exactly 'label', 'scenario', 'reason'", module_text=self.MODULE_WITH_S99_1)

    def test_deferred_unknown_key_is_rejected(self) -> None:
        self.assertRejected(
            GOOD_EXPECTED + '\n[[deferred]]\nlabel = "S99_1"\nscenario = "future"\nreason = "x"\nextra = "z"\n',
            "exactly 'label', 'scenario', 'reason'", module_text=self.MODULE_WITH_S99_1)


class ConstantsLoadTests(unittest.TestCase):
    """Load-time validation of the 'constants' table against the .cfg's CONSTANT section."""

    CFGS = {
        "constants.cfg": ('SPECIFICATION Spec\nCONSTANTS\n    N = 3\n    FLAG = TRUE\n    LABEL = "x"\n'
                          '    NAMES = {a, b}\n    FIX_X = FALSE\nINVARIANT Safe\n'),
    }

    @staticmethod
    def toml(literal: str) -> str:
        return f"""
scenarios = ["demo"]

[[run]]
name = "demo-posix-check"
module = "M"
config = "constants.cfg"
scenario = "demo"
kind = "check"
constants = {literal}
timeout_minutes = 5
"""

    def load(self, literal: str) -> run.Expected:
        d = ExpectedDir(self.toml(literal), self.CFGS)
        self.addCleanup(d.close)
        return d.load()

    def assertRejected(self, literal: str, fragment: str) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            self.load(literal)
        self.assertIn(fragment, str(ctx.exception))

    def test_matching_constants_are_accepted(self) -> None:
        expected = self.load('{ N = 3, FLAG = true, LABEL = "x", NAMES = ["a", "b"] }')
        self.assertEqual(dict(expected.runs[0].constants),
                         {"N": 3, "FLAG": True, "LABEL": "x", "NAMES": frozenset({"a", "b"})})

    def test_fix_flag_cfg_constant_is_not_required_in_constants(self) -> None:
        expected = self.load('{ N = 3, FLAG = true, LABEL = "x", NAMES = ["a", "b"] }')
        self.assertNotIn("FIX_X", dict(expected.runs[0].constants))

    def test_set_comparison_is_order_insensitive(self) -> None:
        self.load('{ N = 3, FLAG = true, LABEL = "x", NAMES = ["b", "a"] }')

    def test_wrong_int_is_rejected(self) -> None:
        self.assertRejected('{ N = 4, FLAG = true, LABEL = "x", NAMES = ["a", "b"] }', "does not match")

    def test_wrong_bool_is_rejected(self) -> None:
        self.assertRejected('{ N = 3, FLAG = false, LABEL = "x", NAMES = ["a", "b"] }', "does not match")

    def test_wrong_string_is_rejected(self) -> None:
        self.assertRejected('{ N = 3, FLAG = true, LABEL = "y", NAMES = ["a", "b"] }', "does not match")

    def test_wrong_set_is_rejected(self) -> None:
        self.assertRejected('{ N = 3, FLAG = true, LABEL = "x", NAMES = ["a", "c"] }', "does not match")

    def test_int_does_not_satisfy_a_boolean(self) -> None:
        self.assertRejected('{ N = 3, FLAG = 1, LABEL = "x", NAMES = ["a", "b"] }', "does not match")

    def test_cfg_constant_missing_from_constants_is_rejected(self) -> None:
        self.assertRejected('{ FLAG = true, LABEL = "x", NAMES = ["a", "b"] }', "not declared in 'constants'")

    def test_extra_constants_key_is_rejected(self) -> None:
        self.assertRejected('{ N = 3, FLAG = true, LABEL = "x", NAMES = ["a", "b"], EXTRA = 1 }',
                            "which the config does not assign")

    def test_fix_key_in_constants_is_rejected(self) -> None:
        self.assertRejected('{ N = 3, FLAG = true, LABEL = "x", NAMES = ["a", "b"], FIX_X = false }',
                            "must not contain a fix flag")


class CfgConstantsTests(unittest.TestCase):
    """Direct tests of cfg_constants(), independent of expected.toml loading.

    cfg_constants fails CLOSED: the .cfg is the one file a person can edit to make a failing run
    pass, so anything it cannot read as one of the literal shapes below is an ExpectedError, not a
    silent skip."""

    def test_returns_typed_literals_and_omits_model_values(self) -> None:
        text = (
            "CONSTANTS\n"
            "    N = 3\n"
            "    FLAG = TRUE\n"
            '    LABEL = "x"\n'
            "    NAMES = {a, b}\n"
            "    EMPTY = {}\n"
            "    NoProc = nobody\n"
        )
        self.assertEqual(run.cfg_constants(text), {
            "N": 3,
            "FLAG": True,
            "LABEL": "x",
            "NAMES": frozenset({"a", "b"}),
            "EMPTY": frozenset(),
        })

    def test_negative_integer_is_parsed(self) -> None:
        self.assertEqual(run.cfg_constants("CONSTANT N = -1\n"), {"N": -1})

    def test_negative_integer_needs_no_space(self) -> None:
        with self.assertRaises(run.ExpectedError):
            run.cfg_constants("CONSTANT N = - 1\n")

    def test_substitution_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants("CONSTANT P <- dirOp\n")
        self.assertIn("substitutes an operator for 'P'", str(ctx.exception))

    def test_duplicate_assignment_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants("CONSTANTS\n    N = 3\n    N = 4\n")
        self.assertIn("more than once", str(ctx.exception))

    def test_duplicate_assignment_is_rejected_even_as_a_model_value(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants("CONSTANTS\n    P = dir\n    P = other\n")
        self.assertIn("more than once", str(ctx.exception))

    def test_unparsable_value_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants("CONSTANT N = 3x\n")
        self.assertIn("N", str(ctx.exception))

    def test_set_with_a_string_element_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants('CONSTANT NAMES = {a, "b"}\n')
        self.assertIn("non-identifier element", str(ctx.exception))

    def test_unterminated_set_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants("CONSTANT NAMES = {a, b\n")
        self.assertIn("never closed", str(ctx.exception))

    def test_missing_equals_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants("CONSTANT N 3\n")
        self.assertIn("not followed by '='", str(ctx.exception))

    def test_set_missing_a_comma_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants("CONSTANT NAMES = {a b}\n")
        self.assertIn("missing a comma", str(ctx.exception))

    def test_stray_token_after_assignment_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants("CONSTANT N = 3 4\n")
        self.assertIn("unexpected token where a constant name", str(ctx.exception))

    def test_bare_name_with_no_value_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants("CONSTANT N\n")
        self.assertIn("with no '=' and value", str(ctx.exception))

    def test_name_equals_nothing_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants("CONSTANT N =\n")
        self.assertIn("no value", str(ctx.exception))

    def test_trailing_comma_in_set_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            run.cfg_constants("CONSTANT S = {a, b,}\n")
        self.assertIn("trailing comma", str(ctx.exception))


SYMMETRY_MODULE = "---- MODULE M ----\nPerms == Permutations(Recoverers)\n====\n"
SYMMETRY_CFG = {"symmetry.cfg": "SPECIFICATION Spec\nCONSTANTS\n    Recoverers = {r1, r2}\nSYMMETRY Perms\nINVARIANT Safe\n"}
NO_SYMMETRY_CFG = {"symmetry.cfg": "SPECIFICATION Spec\nCONSTANTS\n    Recoverers = {r1, r2}\nINVARIANT Safe\n"}


def symmetry_toml(kind: str = "check", symmetry_literal: str | None = '{ definition = "Perms", over = "Recoverers" }',
                   constants_literal: str = '{ Recoverers = ["r1", "r2"] }') -> str:
    sym_line = f"symmetry = {symmetry_literal}\n" if symmetry_literal is not None else ""
    return f"""
scenarios = ["demo"]

[[run]]
name = "demo-posix-{kind}"
module = "M"
config = "symmetry.cfg"
scenario = "demo"
kind = "{kind}"
{sym_line}constants = {constants_literal}
timeout_minutes = 5
"""


class SymmetryTests(unittest.TestCase):
    def load(self, toml_text: str, cfgs: dict[str, str], module_text: str = SYMMETRY_MODULE) -> run.Expected:
        d = ExpectedDir(toml_text, cfgs, module_text)
        self.addCleanup(d.close)
        return d.load()

    def assertRejected(self, toml_text: str, fragment: str, cfgs: dict[str, str] = SYMMETRY_CFG,
                        module_text: str = SYMMETRY_MODULE) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            self.load(toml_text, cfgs, module_text)
        self.assertIn(fragment, str(ctx.exception))

    def test_valid_symmetry_is_accepted(self) -> None:
        expected = self.load(symmetry_toml(), SYMMETRY_CFG)
        self.assertEqual(expected.runs[0].symmetry, ("Perms", "Recoverers"))

    def test_symmetry_declared_but_absent_from_cfg_is_rejected(self) -> None:
        self.assertRejected(symmetry_toml(), "if and only if", cfgs=NO_SYMMETRY_CFG)

    def test_symmetry_in_cfg_but_undeclared_is_rejected(self) -> None:
        self.assertRejected(symmetry_toml(symmetry_literal=None), "if and only if")

    def test_wrong_symmetry_definition_is_rejected(self) -> None:
        cfgs = {"symmetry.cfg": "SPECIFICATION Spec\nCONSTANTS\n    Recoverers = {r1, r2}\nSYMMETRY OtherPerms\nINVARIANT Safe\n"}
        self.assertRejected(symmetry_toml(), "must name exactly", cfgs=cfgs)

    def test_module_without_permutations_definition_is_rejected(self) -> None:
        self.assertRejected(symmetry_toml(), "must define", module_text=DEFAULT_MODULE)

    def test_definition_only_in_a_line_comment_is_rejected(self) -> None:
        module = "---- MODULE M ----\n\\* Perms == Permutations(Recoverers)\n====\n"
        self.assertRejected(symmetry_toml(), "must define", module_text=module)

    def test_definition_only_in_a_block_comment_is_rejected(self) -> None:
        module = "---- MODULE M ----\n(* Perms == Permutations(Recoverers) *)\n====\n"
        self.assertRejected(symmetry_toml(), "must define", module_text=module)

    def test_definition_inside_a_nested_block_comment_is_rejected(self) -> None:
        # Correct nesting treats the whole span, first `(*` to last `*)`, as one comment. A
        # stripper that is not nesting-aware closes at the first `*)` instead, which would wrongly
        # leave the definition below visible as real code.
        module = "---- MODULE M ----\n(* (* inner *) Perms == Permutations(Recoverers) *)\n====\n"
        self.assertRejected(symmetry_toml(), "must define", module_text=module)

    def test_symmetry_over_must_be_a_set_of_at_least_two(self) -> None:
        self.assertRejected(symmetry_toml(constants_literal='{ Recoverers = ["r1"] }'),
                            "set of at least two elements")

    def test_symmetry_on_a_liveness_run_is_rejected(self) -> None:
        self.assertRejected(symmetry_toml(kind="liveness"), "must not declare symmetry")


class TightenedTests(unittest.TestCase):
    def test_tightened_on_a_non_liveness_run_is_rejected(self) -> None:
        expected = GOOD_EXPECTED.replace(
            'kind = "check"\nopen_findings',
            'kind = "check"\ntightened = { rung = "one crash", measurement = "did not fit" }\nopen_findings')
        d = ExpectedDir(expected)
        self.addCleanup(d.close)
        with self.assertRaises(run.ExpectedError) as ctx:
            d.load()
        self.assertIn("tightened is only allowed for liveness runs", str(ctx.exception))

    def test_tightened_on_a_liveness_run_is_accepted_and_stored(self) -> None:
        expected = GOOD_EXPECTED.replace(
            'kind = "liveness"\nopen_findings',
            'kind = "liveness"\ntightened = { rung = "one crash", measurement = "did not fit" }\nopen_findings')
        d = ExpectedDir(expected)
        self.addCleanup(d.close)
        loaded = d.load()
        self.assertEqual(loaded.runs[1].tightened, ("one crash", "did not fit"))

    def test_report_prints_tightened_line(self) -> None:
        result = run.Result("demo-posix-liveness", "ok", frozenset(), frozenset(), 10, 1.0, "", (), None)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            run.report(result, ("one crash", "did not fit"))
        self.assertIn("tightened: one crash (did not fit)", out.getvalue())


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


class CommentStrippingTests(unittest.TestCase):
    """Direct tests of strip_tla_comments(), used by the symmetry check (Part A) and by the
    PlusCal label parser (Part B)."""

    def test_removes_line_and_block_comments(self) -> None:
        text = "A\n\\* comment\nB (* block *) C\n"
        self.assertEqual(run.strip_tla_comments(text), "A\n\nB  C\n")

    def test_nested_block_comments_are_honoured(self) -> None:
        text = "before (* outer (* inner *) still-comment *) after"
        self.assertEqual(run.strip_tla_comments(text), "before  after")

    def test_real_definition_survives_stripping(self) -> None:
        text = "\\* not this one: Perms == Permutations(X)\nPerms == Permutations(Recoverers)\n"
        stripped = run.strip_tla_comments(text)
        self.assertNotIn("Permutations(X)", stripped)
        self.assertIn("Perms == Permutations(Recoverers)", stripped)


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
        for exit_code in (11, 12, 13):
            with self.subTest(exit_code=exit_code):
                # A clean run's messages carry no deadlock, invariant or temporal report, so none of the
                # violation exit codes agrees with them.
                self.assertIn("does not match", run.interpret(exit_code, output, ["P"]).tooling_error or "")

    def test_an_unknown_exit_status_is_a_tooling_failure(self) -> None:
        """The catch-all in `agrees.get(exit_code, False)` had no test, so its default could be
        flipped to accept any unrecognised code and the suite stayed green (test audit, plan 3,
        TA-3). An exit status TLC is not documented to produce must never be read as a verdict."""
        _, output = fixture("clean")
        for exit_code in (77, 1, 255):
            with self.subTest(exit_code=exit_code):
                detail = run.interpret(exit_code, output, []).tooling_error or ""
                self.assertIn("does not match", detail)
                self.assertIn(str(exit_code), detail)

    def test_crlf_output_parses(self) -> None:
        code, output = fixture("continue_two_invariants")
        self.assertEqual(run.interpret(code, output.replace("\n", "\r\n"), []).observed, {"NeverTwo", "NeverThree"})

    def test_success_exit_with_a_deadlock_message_is_a_tooling_failure(self) -> None:
        # "clean" carries Finished + Success (exit 0's messages); splicing in a message TLC would
        # never emit alongside a clean success (a deadlock, or a temporal violation) must still be
        # caught as a tooling failure - the exit code no longer agrees with the messages.
        _, output = fixture("clean")
        variants = {
            "deadlock": ("@!@!@STARTMSG 2114:1 @!@!@\nDeadlock reached.\n@!@!@ENDMSG 2114 @!@!@\n", []),
            "temporal": ("@!@!@STARTMSG 2116:1 @!@!@\nTemporal properties were violated.\n@!@!@ENDMSG 2116 @!@!@\n",
                        ["EventuallyFive"]),
        }
        for case, (extra, properties) in variants.items():
            with self.subTest(case=case):
                result = run.interpret(0, output + extra, properties)
                self.assertIn("does not match", result.tooling_error or "")

    def test_unreadable_invariant_name_is_a_tooling_failure(self) -> None:
        _, output = fixture("clean")
        extra = "@!@!@STARTMSG 2110:1 @!@!@\nSomething is wrong here.\n@!@!@ENDMSG 2110 @!@!@\n"
        result = run.interpret(12, output + extra, [])
        self.assertIn("cannot read the invariant name", result.tooling_error or "")

    def test_distinct_states_takes_the_last_report(self) -> None:
        messages = [run.Message(run.C_COVERAGE_START, 0, "5 distinct states found"),
                   run.Message(run.C_COVERAGE_START, 0, "9 distinct states found")]
        self.assertEqual(run.distinct_states(messages), 9)


class JudgeTests(unittest.TestCase):
    def setUp(self) -> None:
        d = ExpectedDir()
        self.addCleanup(d.close)
        expected = d.load()
        self.check, self.liveness, self.seeded, self.witness = expected.runs

    @staticmethod
    def seen(*names: str) -> run.Outcome:
        return run.Outcome(None, frozenset(names), 10, ())

    def test_check_passes_only_on_its_open_findings(self) -> None:
        self.assertEqual(run.judge(self.check, self.seen("Safe"), fixed=False), "ok")
        self.assertEqual(run.judge(self.check, self.seen(), fixed=False), "mismatch")
        self.assertEqual(run.judge(self.check, self.seen("Safe", "Extra"), fixed=False), "mismatch")

    def test_fixed_run_drops_open_findings(self) -> None:
        self.assertEqual(run.judge(self.check, self.seen(), fixed=True), "ok")
        self.assertEqual(run.judge(self.check, self.seen("Safe"), fixed=True), "mismatch")

    def test_liveness_passes_only_on_its_open_findings(self) -> None:
        self.assertEqual(run.judge(self.liveness, self.seen("LiveWitness"), fixed=False), "ok")
        self.assertEqual(run.judge(self.liveness, self.seen(), fixed=False), "mismatch")
        self.assertEqual(run.judge(self.liveness, self.seen("LiveWitness", "Eventually"), fixed=False), "mismatch")

    def test_liveness_fixed_run_drops_open_findings(self) -> None:
        self.assertEqual(run.judge(self.liveness, self.seen(), fixed=True), "ok")
        self.assertEqual(run.judge(self.liveness, self.seen("LiveWitness"), fixed=True), "mismatch")

    def test_seeded_needs_its_violation(self) -> None:
        self.assertEqual(run.judge(self.seeded, self.seen("Other"), fixed=False), "ok")
        self.assertEqual(run.judge(self.seeded, self.seen(), fixed=False), "mismatch")
        self.assertEqual(run.judge(self.seeded, self.seen(run.DEADLOCK), fixed=False), "mismatch")

    def test_witness_needs_its_violation(self) -> None:
        self.assertEqual(run.judge(self.witness, self.seen("Other2"), fixed=False), "ok")
        self.assertEqual(run.judge(self.witness, self.seen(), fixed=False), "mismatch")

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
        expected = d.load()
        self.check = expected.runs[0]
        target = tempfile.TemporaryDirectory()
        self.addCleanup(target.cleanup)
        self.target = Path(target.name)
        for name, value in (("TARGET", self.target), ("tlc_command", run.tlc_command)):
            self.addCleanup(setattr, run, name, getattr(run, name))
        run.TARGET = self.target
        self.commands: list[list[str]] = []

    def fake_tlc(self, script: str) -> None:
        def command(jar: Path, r: run.Run, cfg: Path, metadir: Path, fixed: bool) -> list[str]:
            self.commands.append([str(cfg)])
            return [sys.executable, "-c", script]
        run.tlc_command = command

    def test_replayed_output_is_judged_and_logged(self) -> None:
        # kind "seeded" (not "check"/"liveness"): the coverage gate does not apply, so replaying a
        # plain recorded run - no coverage block - still judges cleanly.
        self.fake_tlc(f"import sys; text = open({str(TESTDATA / 'clean.out')!r}).read(); "
                      "sys.stdout.write(text.partition(chr(10))[2]); sys.exit(0)")
        clean = run.Run("demo-posix-check", "M", "check.cfg", "demo", "seeded", (), (), (), 1, (), None, None)
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
        slow = run.Run("demo-posix-check", "M", "check.cfg", "demo", "check", ("NeverDone",), (), (), 0, (), None, None)
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
        """Run main() over GOOD_EXPECTED with execute() stubbed: each (run, fixed) reports its status,
        default ok. Scoped to --scenario demo (GOOD_EXPECTED's only scenario) so the suite-wide
        coverage union - which needs a real TLC log per run - is skipped; that union has its own
        tests (CoverageUnionTests) against real recorded logs."""
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
            code = run.main(["--expected", str(d.path / "expected.toml"), "--scenario", "demo"])
        return code, calls

    def test_all_runs_ok_is_exit_0(self) -> None:
        code, calls = self.main_with({})
        self.assertEqual(code, 0)
        # Runs with an open finding run twice: as written, then with their fix flags set.
        self.assertEqual(calls, [("demo-posix-check", False), ("demo-posix-check", True),
                                 ("demo-posix-liveness", False), ("demo-posix-liveness", True),
                                 ("demo-posix-seeded-SEED_X", False),
                                 ("demo-posix-witness-Other2", False)])

    def test_a_mismatch_is_exit_1_even_beside_a_tooling_failure(self) -> None:
        code, _ = self.main_with({("demo-posix-check", True): "tooling", ("demo-posix-liveness", False): "mismatch"})
        self.assertEqual(code, 1)

    def test_a_tooling_failure_alone_is_exit_2(self) -> None:
        code, _ = self.main_with({("demo-posix-seeded-SEED_X", False): "tooling"})
        self.assertEqual(code, 2)

    def test_an_error_inside_one_run_is_exit_2_and_later_runs_still_run(self) -> None:
        code, calls = self.main_with({("demo-posix-check", False): "raise"})
        self.assertEqual(code, 2)
        self.assertEqual(calls[-1], ("demo-posix-witness-Other2", False))

    def test_list_scenarios_prints_the_names_as_json_without_java(self) -> None:
        d = ExpectedDir(GOOD_EXPECTED.replace('scenarios = ["demo"]', 'scenarios = ["demo", "other"]') + """
[[run]]
name = "other-posix-liveness"
module = "M"
config = "live.cfg"
scenario = "other"
kind = "liveness"
constants = {}
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

    def test_list_jobs_orders_by_scenario_then_first_platform_appearance(self) -> None:
        d = ExpectedDir("""
scenarios = ["alpha", "beta"]

[[run]]
name = "alpha-windows-check"
module = "M"
config = "check.cfg"
scenario = "alpha"
kind = "check"
constants = {}
timeout_minutes = 5

[[run]]
name = "alpha-posix-liveness"
module = "M"
config = "live.cfg"
scenario = "alpha"
kind = "liveness"
constants = {}
timeout_minutes = 5

[[run]]
name = "beta-posix-check"
module = "M"
config = "check.cfg"
scenario = "beta"
kind = "check"
constants = {}
timeout_minutes = 5
""")
        self.addCleanup(d.close)
        run.shutil.which = lambda _name: None

        def refuse(*_args: object) -> Path:
            raise AssertionError("--list-jobs must not fetch or run TLC")
        run.ensure_jar = refuse
        run.execute = refuse
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = run.main(["--expected", str(d.path / "expected.toml"), "--list-jobs"])
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(out.getvalue()), [
            {"scenario": "alpha", "platform": "windows", "job": "windows"},
            {"scenario": "alpha", "platform": "posix", "job": "posix"},
            {"scenario": "beta", "platform": "posix", "job": "posix"},
        ])

    def test_list_jobs_on_the_repository_file_includes_both_recovery_platforms(self) -> None:
        run.shutil.which = lambda _name: None

        def refuse(*_args: object) -> Path:
            raise AssertionError("--list-jobs must not fetch or run TLC")
        run.ensure_jar = refuse
        run.execute = refuse
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = run.main(["--expected", str(HERE / "expected.toml"), "--list-jobs"])
        self.assertEqual(code, 0)
        jobs = json.loads(out.getvalue())
        self.assertIn({"scenario": "recovery", "platform": "posix", "job": "posix"}, jobs)
        self.assertIn({"scenario": "recovery", "platform": "windows", "job": "windows"}, jobs)

    def test_list_jobs_on_the_extended_repository_file(self) -> None:
        run.shutil.which = lambda _name: None

        def refuse(*_args: object) -> Path:
            raise AssertionError("--list-jobs must not fetch or run TLC")
        run.ensure_jar = refuse
        run.execute = refuse
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = run.main(["--expected", str(HERE / "expected-extended.toml"), "--list-jobs"])
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(out.getvalue()), [{"scenario": "breaklock", "platform": "posix", "job": "posix"},
                                                      {"scenario": "recovery", "platform": "posix", "job": "posix"},
                                                      {"scenario": "recovery", "platform": "windows", "job": "windows"}])

    def test_platform_without_scenario_exits_2(self) -> None:
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = run.main(["--expected", str(HERE / "expected.toml"), "--platform", "posix"])
        self.assertEqual(code, 2)
        self.assertIn("--platform requires --scenario", err.getvalue())

    def test_unknown_platform_exits_2(self) -> None:
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = run.main(["--expected", str(HERE / "expected.toml"), "--scenario", "recovery",
                             "--platform", "solaris"])
        self.assertEqual(code, 2)
        self.assertIn("no runs for scenario recovery on platform solaris", err.getvalue())

    def test_platform_selects_exactly_the_matching_runs(self) -> None:
        d = ExpectedDir("""
scenarios = ["demo"]

[[run]]
name = "demo-windows-check"
module = "M"
config = "check.cfg"
scenario = "demo"
kind = "check"
constants = {}
timeout_minutes = 5

[[run]]
name = "demo-posix-check"
module = "M"
config = "check.cfg"
scenario = "demo"
kind = "check"
constants = {}
timeout_minutes = 5
""")
        self.addCleanup(d.close)
        run.ensure_jar = lambda: Path("unused.jar")
        calls: list[str] = []

        def execute(r: run.Run, _jar: Path, _base: Path, fixed: bool) -> run.Result:
            calls.append(r.name)
            return ExitCodeTests.result("ok")
        run.execute = execute
        with contextlib.redirect_stdout(io.StringIO()):
            code = run.main(["--expected", str(d.path / "expected.toml"), "--scenario", "demo",
                             "--platform", "posix"])
        self.assertEqual(code, 0)
        self.assertEqual(calls, ["demo-posix-check"])

    def test_bad_expected_file_exits_2_with_the_error(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        bad = Path(tmp.name) / "expected.toml"
        bad.write_text("this is not valid toml [[[\n", encoding="utf-8")
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = run.main(["--expected", str(bad)])
        self.assertEqual(code, 2)
        self.assertIn("run.py:", err.getvalue())

    def test_missing_java_exits_2_before_any_run(self) -> None:
        d = ExpectedDir()
        self.addCleanup(d.close)
        run.shutil.which = lambda _name: None
        called: list[str] = []

        def ensure_jar() -> Path:
            called.append("ensure_jar")
            return Path("unused.jar")

        def execute(*_args: object) -> run.Result:
            called.append("execute")
            return ExitCodeTests.result("ok")
        run.ensure_jar = ensure_jar
        run.execute = execute
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = run.main(["--expected", str(d.path / "expected.toml")])
        self.assertEqual(code, 2)
        self.assertIn("java is not on PATH", err.getvalue())
        self.assertEqual(called, [])


class CommandTests(unittest.TestCase):
    def test_check_and_liveness_continue_seeded_and_witness_halt(self) -> None:
        d = ExpectedDir()
        self.addCleanup(d.close)
        expected = d.load()
        check, liveness, seeded, witness = expected.runs
        jar, cfg, meta = Path("j.jar"), Path("c.cfg"), Path("m")
        self.assertIn("-continue", run.tlc_command(jar, check, cfg, meta, fixed=False))
        self.assertIn("-continue", run.tlc_command(jar, liveness, cfg, meta, fixed=False))
        self.assertNotIn("-continue", run.tlc_command(jar, seeded, cfg, meta, fixed=False))
        self.assertNotIn("-continue", run.tlc_command(jar, witness, cfg, meta, fixed=False))
        self.assertNotIn("-deadlock", run.tlc_command(jar, check, cfg, meta, fixed=False))
        self.assertEqual(run.tlc_command(jar, check, cfg, meta, fixed=False)[-1], "M")

    def test_coverage_added_for_every_kind_unless_fixed(self) -> None:
        d = ExpectedDir()
        self.addCleanup(d.close)
        expected = d.load()
        jar, cfg, meta = Path("j.jar"), Path("c.cfg"), Path("m")
        for r in expected.runs:
            with self.subTest(kind=r.kind):
                cmd = run.tlc_command(jar, r, cfg, meta, fixed=False)
                self.assertIn("-coverage", cmd)
                self.assertEqual(cmd[cmd.index("-coverage") + 1], "1")
                self.assertNotIn("-coverage", run.tlc_command(jar, r, cfg, meta, fixed=True))


def coverage_messages(case: str) -> list[run.Message]:
    """Parse one testdata/<case>.out recording into Messages (the 'exit=' line stripped)."""
    _, output = fixture(case)
    return run.parse_messages(output)


class ParseCoverageTests(unittest.TestCase):
    """parse_coverage(), against real recorded '-coverage 1' output (testdata/*.out)."""

    def test_takes_the_final_block(self) -> None:
        # coverage_two_blocks.out splices the CoverageFixture recording's block in as an early
        # snapshot, ahead of the real final block from smoke_check_coverage.out (record_fixtures.py
        # says so). Only the CoverageFixture block carries 'only_alpha_step'. Renamed from
        # test_takes_the_last_complete_block: parse_coverage() no longer searches backward for the
        # last COMPLETE block among several - it takes ONLY the block starting at the last 2201, and
        # fails closed (None) if that one block is not terminated. This fixture's last block is
        # complete, so the observed result is unchanged; test_trailing_2201_without_a_terminator_
        # returns_none below is what actually exercises the no-fallback behaviour.
        coverage = run.parse_coverage(coverage_messages("coverage_two_blocks"))
        self.assertEqual(coverage, {"Step": (4, 8), "Overshoot": (0, 0), "Finished": (0, 1)})

    def test_returns_none_without_a_complete_block(self) -> None:
        text = "@!@!@STARTMSG 2201:0 @!@!@\nThe coverage statistics at now\n@!@!@ENDMSG 2201 @!@!@\n"
        self.assertIsNone(run.parse_coverage(run.parse_messages(text)))

    def test_returns_none_with_no_block_at_all(self) -> None:
        self.assertIsNone(run.parse_coverage(coverage_messages("clean")))

    def test_a_2777_terminated_final_block_is_returned(self) -> None:
        # coverage_long_terminator.out (measured defect): TLC switches from 2202 to 2777 ("End of
        # statistics (please note ...)") once a run has gone on long enough. The pre-fix parser
        # accepted only 2202 and so returned the file's SECOND block (2201..2202), where
        # plain_recover is 0:0; the true final block (2201..2777) has plain_recover 278:357.
        coverage = run.parse_coverage(coverage_messages("coverage_long_terminator"))
        assert coverage is not None
        self.assertEqual(coverage["plain_recover"], (278, 357))

    def test_trailing_2201_without_a_terminator_returns_none(self) -> None:
        # An earlier block (2201..2202) is complete, but the last 2201 in the file has no 2202 or
        # 2777 after it at all. The fix must fail closed here, not fall back to the earlier block.
        text = (
            "@!@!@STARTMSG 2201:0 @!@!@\nThe coverage statistics at now\n@!@!@ENDMSG 2201 @!@!@\n"
            "@!@!@STARTMSG 2772:0 @!@!@\n<Step line 1, col 1 to line 1, col 4 of module M>: 2:3\n"
            "@!@!@ENDMSG 2772 @!@!@\n"
            "@!@!@STARTMSG 2202:0 @!@!@\nEnd of statistics.\n@!@!@ENDMSG 2202 @!@!@\n"
            "@!@!@STARTMSG 2201:0 @!@!@\nThe coverage statistics at later\n@!@!@ENDMSG 2201 @!@!@\n"
            "@!@!@STARTMSG 2772:0 @!@!@\n<Step line 1, col 1 to line 1, col 4 of module M>: 9:9\n"
            "@!@!@ENDMSG 2772 @!@!@\n"
        )
        self.assertIsNone(run.parse_coverage(run.parse_messages(text)))

    def test_sums_a_repeated_name(self) -> None:
        text = (
            "@!@!@STARTMSG 2201:0 @!@!@\nThe coverage statistics at now\n@!@!@ENDMSG 2201 @!@!@\n"
            "@!@!@STARTMSG 2772:0 @!@!@\n<Step line 1, col 1 to line 1, col 4 of module M>: 2:3\n"
            "@!@!@ENDMSG 2772 @!@!@\n"
            "@!@!@STARTMSG 2772:0 @!@!@\n<Step line 5, col 1 to line 5, col 4 of module M>: 1:1\n"
            "@!@!@ENDMSG 2772 @!@!@\n"
            "@!@!@STARTMSG 2202:0 @!@!@\nEnd of statistics.\n@!@!@ENDMSG 2202 @!@!@\n"
        )
        self.assertEqual(run.parse_coverage(run.parse_messages(text)), {"Step": (3, 4)})

    def test_ignores_init_definition_and_cost_lines(self) -> None:
        coverage = run.parse_coverage(coverage_messages("smoke_check_coverage"))
        assert coverage is not None
        self.assertNotIn("Init", coverage)  # 2773
        self.assertNotIn("WithinBound", coverage)  # 2774
        self.assertNotIn("AtMostThree", coverage)  # 2774

    def test_zero_distinct_nonzero_total_is_executed(self) -> None:
        # 'Finished' reports 0:1 in the recorded check run: never a NEW state, but executed once.
        coverage = run.parse_coverage(coverage_messages("smoke_check_coverage"))
        assert coverage is not None
        self.assertEqual(coverage["Finished"], (0, 1))


MIXED_CALLERS_MODULE = """---- MODULE Mixed ----
(* --algorithm Mixed {
  procedure Shared()
  {
    shared_step:
      skip;
      return;
  }

  process (p1 \\in SetA)
  {
    p1_step:
      call Shared();
  }

  process (p2 \\in SetB)
  {
    p2_step:
      call Shared();
  }
}
*)
====
"""


class LabelUniverseTests(unittest.TestCase):
    """module_labels() and LabelUniverse.is_exempt(), against the real committed fixture module
    (CoverageFixture.tla, recorded by record_fixtures.py) and Smoke.tla."""

    def setUp(self) -> None:
        self.universe = run.module_labels((TESTDATA / "CoverageFixture.tla").read_text(encoding="utf-8"))

    def test_finds_process_and_procedure_labels(self) -> None:
        self.assertEqual(self.universe.labels, {
            "a_step", "a_done", "only_alpha_step", "b_step", "b_guarded",
            "calls_helper_step", "calls_helper_after", "helper_step", "env_step",
        })

    def test_ownership_follows_transitive_calls(self) -> None:
        # helper_step is in procedure Helper, called only by CallsHelper, called only by process b.
        self.assertEqual(self.universe.owners["helper_step"], frozenset({"b"}))

    def test_process_label_is_exempt_when_its_set_is_empty(self) -> None:
        empty = {"Alphas": frozenset(), "Betas": frozenset({"b1"})}
        self.assertTrue(self.universe.is_exempt("a_step", empty))
        self.assertTrue(self.universe.is_exempt("a_done", empty))

    def test_process_label_is_not_exempt_when_its_set_is_non_empty(self) -> None:
        non_empty = {"Alphas": frozenset({"a1"}), "Betas": frozenset({"b1"})}
        self.assertFalse(self.universe.is_exempt("a_step", non_empty))

    def test_procedure_label_exempt_only_when_its_only_caller_is_empty(self) -> None:
        empty = {"Alphas": frozenset(), "Betas": frozenset({"b1"})}
        self.assertTrue(self.universe.is_exempt("only_alpha_step", empty))  # only called via Alphas
        self.assertFalse(self.universe.is_exempt("helper_step", empty))  # only called via Betas (non-empty)

    def test_equals_value_process_is_never_exempt(self) -> None:
        self.assertFalse(self.universe.is_exempt("env_step", {"Alphas": frozenset(), "Betas": frozenset()}))

    def test_a_genuinely_unguarded_label_is_never_executed_in_the_recording(self) -> None:
        # b_guarded (await FALSE) is real recorded TOTAL 0 (testdata/coverage.out), and is NOT
        # exempt (Betas is non-empty in that recording) - the gate must catch it.
        coverage = run.parse_coverage(coverage_messages("coverage"))
        assert coverage is not None
        self.assertEqual(coverage["b_guarded"][1], 0)
        self.assertFalse(self.universe.is_exempt("b_guarded", {"Alphas": frozenset(), "Betas": frozenset({"b1"})}))

    def test_procedure_label_exempt_only_when_every_calling_set_is_empty(self) -> None:
        # A hand-written module (not real TLC output: this tests our own graph logic, not TLC's
        # behaviour) where one procedure has two different process callers.
        universe = run.module_labels(MIXED_CALLERS_MODULE)
        self.assertEqual(universe.owners["shared_step"], frozenset({"p1", "p2"}))
        self.assertTrue(universe.is_exempt("shared_step", {"SetA": frozenset(), "SetB": frozenset()}))
        self.assertFalse(universe.is_exempt("shared_step", {"SetA": frozenset(), "SetB": frozenset({"x"})}))
        self.assertFalse(universe.is_exempt("shared_step", {"SetA": frozenset({"x"}), "SetB": frozenset()}))

    def test_procedure_no_process_calls_is_gated_not_exempt(self) -> None:
        # A procedure nothing calls has no owner. "Every caller's set is empty" holds vacuously, but
        # design Section 4 says attributing such labels to nobody is the case the exemption must get
        # right: dead labels must fail the per-run gate, not pass it silently.
        universe = run.module_labels(MIXED_CALLERS_MODULE.replace(
            "  process (p1 \\in SetA)", "  procedure Orphan()\n  {\n    orphan_step:\n      return;\n  }\n\n"
            "  process (p1 \\in SetA)"))
        self.assertIn("orphan_step", universe.labels)
        self.assertEqual(universe.owners.get("orphan_step", frozenset()), frozenset())
        self.assertFalse(universe.is_exempt("orphan_step", {"SetA": frozenset(), "SetB": frozenset()}))
        self.assertFalse(universe.is_exempt("orphan_step", {"SetA": frozenset({"x"}), "SetB": frozenset()}))

    def test_non_pluscal_universe_is_the_top_level_operators(self) -> None:
        universe = run.module_labels((HERE / "Smoke.tla").read_text(encoding="utf-8"))
        self.assertFalse(universe.pluscal)
        self.assertEqual(universe.labels, {
            "Limit", "Init", "Step", "Overshoot", "Finished", "Next", "Spec",
            "WithinBound", "AtMostThree", "NeverReachedTwo", "ReachesLimit", "vars",
        })
        self.assertFalse(universe.is_exempt("Overshoot", {}))  # nothing is exempt without PlusCal


class UnreachedLoadTimeValidationTests(unittest.TestCase):
    """_load_run()'s load-time checks on 'unreached' against the real module universe (Smoke.tla),
    beyond the shape checks LoadExpectedTests already covers."""

    TOML = """
scenarios = ["selftest"]

[[run]]
name = "selftest-posix-check"
module = "M"
config = "check.cfg"
scenario = "selftest"
kind = "check"
unreached = [{{ label = "{label}", reason = "x" }}]
constants = {{ SEED_OVERSHOOT = false }}
timeout_minutes = 2
"""
    CFG = {"check.cfg": "SPECIFICATION Spec\nCONSTANTS\n    SEED_OVERSHOOT = FALSE\n    FIX_BOUND = FALSE\n"
                        "INVARIANT WithinBound\n"}

    def load(self, label: str) -> run.Expected:
        d = ExpectedDir(self.TOML.format(label=label), self.CFG, (HERE / "Smoke.tla").read_text(encoding="utf-8"))
        self.addCleanup(d.close)
        return d.load()

    def test_unknown_unreached_label_is_rejected(self) -> None:
        with self.assertRaises(run.ExpectedError) as ctx:
            self.load("NoSuchLabel")
        self.assertIn("is not in", str(ctx.exception))

    def test_known_never_exempt_label_is_accepted(self) -> None:
        # Overshoot is real and, for a non-PlusCal module, never exempt.
        expected = self.load("Overshoot")
        self.assertEqual(expected.runs[0].unreached, (("Overshoot", "x"),))

    COVERAGE_FIXTURE_TOML = """
scenarios = ["cov"]

[[run]]
name = "cov-posix-check"
module = "M"
config = "check.cfg"
scenario = "cov"
kind = "check"
unreached = [{{ label = "a_step", reason = "x" }}]
constants = {{ Alphas = {alphas}, Betas = ["b1"] }}
timeout_minutes = 2
"""

    @staticmethod
    def coverage_fixture_cfg(alphas_cfg_set: str) -> dict[str, str]:
        return {"check.cfg": f"SPECIFICATION Spec\nCONSTANTS\n    Alphas = {alphas_cfg_set}\n"
                             "    Betas = {b1}\nINVARIANT Safe\n"}

    def load_coverage_fixture(self, alphas_toml: str, alphas_cfg_set: str) -> run.Expected:
        d = ExpectedDir(self.COVERAGE_FIXTURE_TOML.format(alphas=alphas_toml),
                        self.coverage_fixture_cfg(alphas_cfg_set),
                        (TESTDATA / "CoverageFixture.tla").read_text(encoding="utf-8"))
        self.addCleanup(d.close)
        return d.load()

    def test_exempt_pluscal_unreached_label_is_rejected(self) -> None:
        # a_step is owned only by process (a \\in Alphas); with Alphas emptied by the run's
        # constants, its actor never runs here, so listing it in 'unreached' needs no entry.
        with self.assertRaises(run.ExpectedError) as ctx:
            self.load_coverage_fixture("[]", "{}")
        self.assertIn("needs no entry", str(ctx.exception))

    def test_exempt_pluscal_unreached_label_with_non_empty_set_is_accepted(self) -> None:
        # Same label, but Alphas is non-empty: process a does run here, so a_step is not exempt
        # and 'unreached' may list it.
        expected = self.load_coverage_fixture('["a1"]', "{a1}")
        self.assertEqual(expected.runs[0].unreached, (("a_step", "x"),))


class CoverageGateTests(unittest.TestCase):
    """judge_coverage(), against real recorded coverage (testdata/*.out) and Smoke.tla's real
    (non-PlusCal) universe."""

    def setUp(self) -> None:
        self.universe = run.module_labels((HERE / "Smoke.tla").read_text(encoding="utf-8"))

    @staticmethod
    def coverage(case: str) -> dict[str, tuple[int, int]]:
        result = run.parse_coverage(coverage_messages(case))
        assert result is not None
        return result

    @staticmethod
    def run_with(unreached: tuple[tuple[str, str], ...]) -> run.Run:
        return run.Run("x-check", "Smoke", "c", "x", "check", (), (), unreached, 2,
                       (("SEED_OVERSHOOT", False),), None, None)

    def test_fails_on_an_uncovered_unlisted_label(self) -> None:
        failures = run.judge_coverage(self.run_with(()), self.universe, self.coverage("smoke_check_coverage"))
        self.assertEqual(failures, ["Overshoot: gated label not covered (TOTAL 0)"])

    def test_fails_on_a_listed_but_covered_label(self) -> None:
        failures = run.judge_coverage(self.run_with((("Overshoot", "wrongly excused"),)), self.universe,
                                      self.coverage("smoke_seeded_coverage"))
        self.assertIn("Overshoot: covered - remove it from unreached", failures)

    def test_passes_when_every_gated_label_is_covered_or_listed(self) -> None:
        failures = run.judge_coverage(self.run_with((("Overshoot", "SEED_OVERSHOOT is off"),)), self.universe,
                                      self.coverage("smoke_check_coverage"))
        self.assertEqual(failures, [])

    def test_pluscal_gate_skips_an_exempt_label_and_gates_the_rest(self) -> None:
        pluscal_universe = run.module_labels((TESTDATA / "CoverageFixture.tla").read_text(encoding="utf-8"))

        def run_with_constants(constants: tuple[tuple[str, object], ...]) -> run.Run:
            return run.Run("x-check", "CoverageFixture", "c", "x", "check", (), (), (), 2, constants, None, None)

        empty_alphas = (("Alphas", frozenset()), ("Betas", frozenset({"b1"})))
        non_empty_alphas = (("Alphas", frozenset({"a1"})), ("Betas", frozenset({"b1"})))

        # a_step is exempt when Alphas is empty: no failure is reported for it.
        failures = run.judge_coverage(run_with_constants(empty_alphas), pluscal_universe, {})
        self.assertNotIn("a_step: gated label not covered (TOTAL 0)", failures)

        # Same label, Alphas non-empty: not exempt, and zero coverage, so it DOES fail.
        failures = run.judge_coverage(run_with_constants(non_empty_alphas), pluscal_universe, {})
        self.assertIn("a_step: gated label not covered (TOTAL 0)", failures)

        # env_step is never exempt (its process is declared '= "env"'); even when it is absent
        # from the coverage dict entirely (not merely TOTAL 0), it must still be gated.
        coverage_missing_env = {label: (1, 1) for label in pluscal_universe.labels if label != "env_step"}
        failures = run.judge_coverage(run_with_constants(non_empty_alphas), pluscal_universe, coverage_missing_env)
        self.assertIn("env_step: gated label not covered (TOTAL 0)", failures)


class ExecuteCoverageGateTests(unittest.TestCase):
    """execute() wired end-to-end against Smoke.tla's real content, replaying real recorded TLC
    output (no Java: TLC is replaced by a small script)."""

    def setUp(self) -> None:
        smoke_text = (HERE / "Smoke.tla").read_text(encoding="utf-8")
        toml = """
scenarios = ["demo"]

[[run]]
name = "demo-posix-check"
module = "M"
config = "check.cfg"
scenario = "demo"
kind = "check"
open_findings = [{ name = "AtMostThree", tracking = "t", fix_flag = "FIX_BOUND" }]
unreached = [{ label = "Overshoot", reason = "SEED_OVERSHOOT is off in this run" }]
constants = { SEED_OVERSHOOT = false }
timeout_minutes = 5
"""
        cfgs = {"check.cfg": "SPECIFICATION Spec\nCONSTANTS\n    SEED_OVERSHOOT = FALSE\n    FIX_BOUND = FALSE\n"
                             "INVARIANTS\n    WithinBound\n    AtMostThree\n"}
        d = ExpectedDir(toml, cfgs, smoke_text)
        self.addCleanup(d.close)
        self.base = d.path
        self.check = d.load().runs[0]
        target = tempfile.TemporaryDirectory()
        self.addCleanup(target.cleanup)
        self.target = Path(target.name)
        for name in ("TARGET", "tlc_command"):
            self.addCleanup(setattr, run, name, getattr(run, name))
        run.TARGET = self.target

    def replay(self, case: str) -> None:
        script = (f"import sys; text = open({str(TESTDATA / f'{case}.out')!r}).read(); "
                  "sys.stdout.write(text.partition(chr(10))[2]); sys.exit(0)")

        def command(jar: Path, r: run.Run, cfg: Path, metadir: Path, fixed: bool) -> list[str]:
            return [sys.executable, "-c", script]
        run.tlc_command = command

    def test_real_check_coverage_passes_with_the_gap_listed(self) -> None:
        self.replay("smoke_check_coverage")
        result = run.execute(self.check, Path("unused.jar"), self.base, fixed=False)
        self.assertEqual(result.status, "ok")

    def test_missing_coverage_report_is_a_tooling_failure(self) -> None:
        self.replay("clean")  # a recording with no coverage block at all
        result = run.execute(self.check, Path("unused.jar"), self.base, fixed=False)
        self.assertEqual(result.status, "tooling")
        self.assertEqual(result.detail, "no coverage report")

    def test_no_gate_for_a_fixed_run(self) -> None:
        # -fixed skips -coverage entirely (B1); replaying a report-less recording must not turn
        # into "no coverage report" for a -fixed run.
        self.replay("clean")
        result = run.execute(self.check, Path("unused.jar"), self.base, fixed=True)
        self.assertNotEqual(result.detail, "no coverage report")

    def test_no_gate_for_a_witness_run(self) -> None:
        witness = run.Run("demo-posix-witness", "M", "check.cfg", "demo", "witness",
                          ("NeverReachedTwo",), (), (), 5, self.check.constants, None, None)
        self.replay("clean")
        result = run.execute(witness, Path("unused.jar"), self.base, fixed=False)
        self.assertNotEqual(result.detail, "no coverage report")

    def test_no_gate_for_a_seeded_run(self) -> None:
        seeded = run.Run("demo-posix-seeded-SEED_X", "M", "check.cfg", "demo", "seeded",
                         ("WithinBound",), (), (), 5, self.check.constants, None, None)
        self.replay("clean")
        result = run.execute(seeded, Path("unused.jar"), self.base, fixed=False)
        self.assertNotEqual(result.detail, "no coverage report")


class CoverageUnionTests(unittest.TestCase):
    """judge_union(): the suite-wide coverage union (design Section 4), against real recorded logs."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.base = Path(self._tmp.name)
        (self.base / "Smoke.tla").write_text((HERE / "Smoke.tla").read_text(encoding="utf-8"), encoding="utf-8")

    def log_for(self, case: str) -> Path:
        log = self.base / f"{case}.log"
        log.write_text(fixture(case)[1], encoding="utf-8")
        return log

    def entry(self, name: str, kind: str, fixed: bool, case: str | None,
             status: str = "ok") -> tuple[run.Run, bool, run.Result]:
        r = run.Run(name, "Smoke", "c", "x", kind, (), (), (), 2, (("SEED_OVERSHOOT", kind == "seeded"),),
                   None, None)
        log = self.log_for(case) if case is not None else None
        result = run.Result(name, status, frozenset(), frozenset(), None, 1.0, "", (), log)
        return (r, fixed, result)

    def test_union_counts_a_seeded_runs_coverage(self) -> None:
        executed = [self.entry("x-check", "check", False, "smoke_check_coverage"),
                   self.entry("x-seeded", "seeded", False, "smoke_seeded_coverage")]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            failed = run.judge_union(executed, self.base, (), ())
        self.assertFalse(failed)
        self.assertIn("COVERAGE suite", out.getvalue())

    def test_union_fails_on_an_uncovered_label(self) -> None:
        # No seeded run here, so Overshoot (TOTAL 0 in the check run) is covered by nothing.
        executed = [self.entry("x-check", "check", False, "smoke_check_coverage")]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            failed = run.judge_union(executed, self.base, (), ())
        self.assertTrue(failed)
        self.assertIn("Overshoot", out.getvalue())

    def test_union_fails_on_a_covered_never_reached_label(self) -> None:
        executed = [self.entry("x-check", "check", False, "smoke_check_coverage"),
                   self.entry("x-seeded", "seeded", False, "smoke_seeded_coverage")]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            failed = run.judge_union(executed, self.base, (("Overshoot", "thought unreachable"),), ())
        self.assertTrue(failed)
        self.assertIn("never_reached but covered", out.getvalue())

    def test_union_skipped_when_a_run_had_a_tooling_failure(self) -> None:
        executed = [self.entry("x-check", "check", False, "smoke_check_coverage"),
                   self.entry("x-seeded", "seeded", False, None, status="tooling")]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            failed = run.judge_union(executed, self.base, (), ())
        self.assertFalse(failed)
        self.assertIn("not judged: a run failed or timed out", out.getvalue())

    def test_union_failure_sets_exit_1_through_main(self) -> None:
        toml = """
scenarios = ["demo"]

[[run]]
name = "demo-posix-check"
module = "Smoke"
config = "c.cfg"
scenario = "demo"
kind = "check"
unreached = [{ label = "Overshoot", reason = "not seeded in this run" }]
constants = { SEED_OVERSHOOT = false }
timeout_minutes = 5
"""
        cfg_text = "SPECIFICATION Spec\nCONSTANTS\n    SEED_OVERSHOOT = FALSE\n    FIX_BOUND = FALSE\n" \
                   "INVARIANT WithinBound\n"
        (self.base / "expected.toml").write_text(toml, encoding="utf-8")
        (self.base / "c.cfg").write_text(cfg_text, encoding="utf-8")
        log = self.log_for("smoke_check_coverage")

        def execute(r: run.Run, _jar: Path, _base: Path, fixed: bool) -> run.Result:
            return run.Result(r.name, "ok", frozenset(), frozenset(), 10, 1.0, "", (), log)

        for name in ("execute", "ensure_jar"):
            self.addCleanup(setattr, run, name, getattr(run, name))
        self.addCleanup(setattr, run.shutil, "which", run.shutil.which)
        run.shutil.which = lambda _name: "java"
        run.ensure_jar = lambda: Path("unused.jar")
        run.execute = execute
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = run.main(["--expected", str(self.base / "expected.toml")])
        self.assertEqual(code, 1)
        self.assertIn("MISMATCH suite coverage", out.getvalue())

    def test_union_gates_a_pluscal_label_no_run_reported(self) -> None:
        # A PlusCal module's label is gated even if it appears in NO run's coverage report at all
        # (unlike a non-PlusCal module, which only gates names that were reported at least once).
        (self.base / "CoverageFixture.tla").write_text(
            (TESTDATA / "CoverageFixture.tla").read_text(encoding="utf-8"), encoding="utf-8")
        log_text = (
            "@!@!@STARTMSG 2201:0 @!@!@\nThe coverage statistics at now\n@!@!@ENDMSG 2201 @!@!@\n"
            "@!@!@STARTMSG 2772:0 @!@!@\n<a_step line 1, col 1 to line 1, col 4 of module CoverageFixture>: 1:1\n"
            "@!@!@ENDMSG 2772 @!@!@\n"
            "@!@!@STARTMSG 2202:0 @!@!@\nEnd of statistics.\n@!@!@ENDMSG 2202 @!@!@\n"
        )
        log = self.base / "cf.log"
        log.write_text(log_text, encoding="utf-8")
        r = run.Run("x-check", "CoverageFixture", "c", "x", "check", (), (), (), 2,
                   (("Alphas", frozenset({"a1"})), ("Betas", frozenset({"b1"}))), None, None)
        result = run.Result("x-check", "ok", frozenset(), frozenset(), None, 1.0, "", (), log)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            failed = run.judge_union([(r, False, result)], self.base, (), ())
        self.assertTrue(failed)
        self.assertIn("env_step", out.getvalue())

    def test_union_ignores_a_fixed_runs_coverage(self) -> None:
        # The only run that covers Overshoot here is -fixed; a -fixed run's coverage must not
        # count toward the union, so Overshoot stays uncovered.
        executed = [self.entry("x-check", "check", False, "smoke_check_coverage"),
                   self.entry("x-seeded", "seeded", True, "smoke_seeded_coverage")]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            failed = run.judge_union(executed, self.base, (), ())
        self.assertTrue(failed)
        self.assertIn("Overshoot", out.getvalue())


class DeferredUnionTests(unittest.TestCase):
    """judge_union()'s 'deferred' handling (design Section 4), against real recorded logs."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.base = Path(self._tmp.name)
        (self.base / "Smoke.tla").write_text((HERE / "Smoke.tla").read_text(encoding="utf-8"), encoding="utf-8")

    def log_for(self, case: str) -> Path:
        log = self.base / f"{case}.log"
        log.write_text(fixture(case)[1], encoding="utf-8")
        return log

    def entry(self, name: str, kind: str, case: str) -> tuple[run.Run, bool, run.Result]:
        r = run.Run(name, "Smoke", "c", "x", kind, (), (), (), 2, (("SEED_OVERSHOOT", kind == "seeded"),),
                   None, None)
        result = run.Result(name, "ok", frozenset(), frozenset(), None, 1.0, "", (), self.log_for(case))
        return (r, False, result)

    def test_uncovered_deferred_label_passes_and_is_counted(self) -> None:
        executed = [self.entry("x-check", "check", "smoke_check_coverage")]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            failed = run.judge_union(executed, self.base, (), (("Overshoot", "future", "not built yet"),))
        self.assertFalse(failed)
        self.assertIn("0 never_reached, 1 deferred", out.getvalue())

    def test_covered_deferred_label_fails_the_union(self) -> None:
        executed = [self.entry("x-check", "check", "smoke_check_coverage"),
                   self.entry("x-seeded", "seeded", "smoke_seeded_coverage")]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            failed = run.judge_union(executed, self.base, (), (("Overshoot", "future", "not built yet"),))
        self.assertTrue(failed)
        self.assertIn("deferred but covered: Overshoot", out.getvalue())

    def test_covered_deferred_label_exit_code_1_through_main(self) -> None:
        toml = """
scenarios = ["demo"]

[[run]]
name = "demo-posix-check"
module = "Smoke"
config = "c.cfg"
scenario = "demo"
kind = "check"
constants = { SEED_OVERSHOOT = false }
timeout_minutes = 5

[[run]]
name = "demo-posix-seeded-SEED_X"
module = "Smoke"
config = "seed.cfg"
scenario = "demo"
kind = "seeded"
violated = ["WithinBound"]
constants = { SEED_OVERSHOOT = true }
timeout_minutes = 5

[[deferred]]
label = "Overshoot"
scenario = "future"
reason = "not built yet"
"""
        check_cfg_text = "SPECIFICATION Spec\nCONSTANTS\n    SEED_OVERSHOOT = FALSE\n    FIX_BOUND = FALSE\nINVARIANT WithinBound\n"
        seed_cfg_text = "SPECIFICATION Spec\nCONSTANT SEED_OVERSHOOT = TRUE\nINVARIANT WithinBound\n"
        (self.base / "expected.toml").write_text(toml, encoding="utf-8")
        (self.base / "c.cfg").write_text(check_cfg_text, encoding="utf-8")
        (self.base / "seed.cfg").write_text(seed_cfg_text, encoding="utf-8")
        check_log = self.log_for("smoke_check_coverage")
        seeded_log = self.log_for("smoke_seeded_coverage")

        def execute(r: run.Run, _jar: Path, _base: Path, fixed: bool) -> run.Result:
            log = check_log if r.kind == "check" else seeded_log
            return run.Result(r.name, "ok", frozenset(), frozenset(), 10, 1.0, "", (), log)

        for name in ("execute", "ensure_jar"):
            self.addCleanup(setattr, run, name, getattr(run, name))
        self.addCleanup(setattr, run.shutil, "which", run.shutil.which)
        run.shutil.which = lambda _name: "java"
        run.ensure_jar = lambda: Path("unused.jar")
        run.execute = execute
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = run.main(["--expected", str(self.base / "expected.toml")])
        self.assertEqual(code, 1)
        self.assertIn("deferred but covered", out.getvalue())


class MainUnionRoutingTests(unittest.TestCase):
    def test_scenario_run_skips_the_union(self) -> None:
        d = ExpectedDir()
        self.addCleanup(d.close)
        for name in ("execute", "ensure_jar"):
            self.addCleanup(setattr, run, name, getattr(run, name))
        self.addCleanup(setattr, run.shutil, "which", run.shutil.which)
        run.shutil.which = lambda _name: "java"
        run.ensure_jar = lambda: Path("unused.jar")
        run.execute = lambda r, _jar, _base, fixed: ExitCodeTests.result("ok")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            run.main(["--expected", str(d.path / "expected.toml"), "--scenario", "demo"])
        self.assertIn("not judged for a single scenario", out.getvalue())


class GeneratedModuleTests(unittest.TestCase):
    """LockProtocol.tla is generated (README.md): `cat LockProtocol.head algorithm.txt invariants.txt`,
    then `pcal.trans`, which INSERTS a translation block after the algorithm's closing `*)`. Nothing in
    the justfile, the workflows or these tests re-derived it, so a source edited without re-running the
    translator left CI checking a model the sources no longer described, silently (capstone, plan 3).

    This catches that case with no Java: everything OUTSIDE the inserted block must still be exactly the
    three sources concatenated. It does NOT catch a hand-edited translation block - only a real
    `pcal.trans` run can, and that needs the jar, so it belongs in CI rather than here.
    """

    BEGIN = "\\* BEGIN TRANSLATION"
    END = "\\* END TRANSLATION"

    def test_generated_module_matches_its_sources(self) -> None:
        d = Path(__file__).resolve().parent
        sources = "".join((d / name).read_text(encoding="utf-8")
                          for name in ("LockProtocol.head", "algorithm.txt", "invariants.txt"))
        generated = (d / "LockProtocol.tla").read_text(encoding="utf-8")

        lines = generated.splitlines(keepends=True)
        starts = [i for i, ln in enumerate(lines) if ln.startswith(self.BEGIN)]
        ends = [i for i, ln in enumerate(lines) if ln.startswith(self.END)]
        self.assertEqual(len(starts), 1, "expected exactly one BEGIN TRANSLATION marker")
        self.assertEqual(len(ends), 1, "expected exactly one END TRANSLATION marker")
        self.assertLess(starts[0], ends[0], "END TRANSLATION precedes BEGIN TRANSLATION")

        without_translation = "".join(lines[:starts[0]] + lines[ends[0] + 1:])
        self.assertEqual(
            without_translation, sources,
            "LockProtocol.tla no longer matches LockProtocol.head + algorithm.txt + invariants.txt. "
            "Re-run the translator: cat the three sources into LockProtocol.tla, then "
            "`java -cp target/tla/tla2tools.jar pcal.trans LockProtocol.tla` (and delete the "
            "LockProtocol.cfg it writes beside it).")


class ConfigConsistencyTests(unittest.TestCase):
    """The .cfg files are hand-written copies of each other - TLC's format has no include - so nothing
    stops one drifting from the rest. Adding a CONSTANT means editing 70-odd files by hand, and the
    `-fixed-check` configs exist only to be their `-check` sibling with the fix flag flipped. These
    two guards make that duplication safe to keep (capstone, plan 3)."""

    def configs(self) -> dict[str, str]:
        d = Path(__file__).resolve().parent / "configs"
        # The selftest configs belong to Smoke.tla, a different module with its own constants.
        return {c.name: c.read_text(encoding="utf-8") for c in sorted(d.glob("*.cfg"))
                if not c.name.startswith("selftest-")}

    @staticmethod
    def constant_names(text: str) -> frozenset[str]:
        """The names assigned in the CONSTANT/CONSTANTS section.

        Parsed per line rather than from cfg_sections' token stream: a set-literal value such as
        `Owners = {o1}` is several tokens, so the stream has no fixed stride to step over."""
        body = text[text.index("CONSTANT"):]
        for keyword in ("INVARIANT", "PROPERTY", "SYMMETRY", "SPECIFICATION"):
            if keyword in body:
                body = body[:body.index(keyword)]
        return frozenset(m.group(1) for m in re.finditer(r"^\s+([A-Za-z_][A-Za-z0-9_]*)\s*=", body, re.M))

    def test_every_config_declares_the_same_constants(self) -> None:
        names = {name: self.constant_names(text) for name, text in self.configs().items()}
        self.assertTrue(names, "expected LockProtocol configs beside the tests")
        every = frozenset().union(*names.values())
        # Assert something was PARSED, not only that the gaps between files are empty. Without
        # this a parser that extracts nothing gives every file an empty set, so `gaps` is empty
        # and the guard passes while guarding nothing (test audit, plan 3, TA-2 - the same shape
        # as the parse_cost_nodes defect: assert on the filtered result, never on the count).
        self.assertIn("LockCapability", every,
                      "the constant parser returned nothing recognisable; this guard is vacuous")
        self.assertGreater(len(every), 10, f"only {len(every)} constants parsed across {len(names)} configs")
        # Reported per CONSTANT rather than against one chosen file: whichever config is the odd one
        # out, the message must name IT, not the other seventy.
        gaps = {constant: sorted(f for f, declared in names.items() if constant not in declared)
                for constant in sorted(every)}
        gaps = {c: files for c, files in gaps.items() if files}
        self.assertEqual(
            gaps, {},
            "every LockProtocol config must assign every constant the module declares, because adding "
            "one means editing them all by hand and TLC's format has no include. Not declared by: "
            + "; ".join(f"{c} <- {', '.join(files)}" for c, files in gaps.items()))

    def test_fixed_check_configs_match_their_sibling(self) -> None:
        configs = self.configs()
        pairs = [(n, n.replace("-fixed-check.cfg", "-check.cfg")) for n in configs
                 if n.endswith("-fixed-check.cfg")]
        self.assertTrue(pairs, "expected at least one -fixed-check config")
        for fixed_name, check_name in pairs:
            self.assertIn(check_name, configs, f"{fixed_name} has no -check sibling")
            fixed, check = run.cfg_constants(configs[fixed_name]), run.cfg_constants(configs[check_name])
            self.assertEqual(
                fixed.get("FIX_REMOTE_LEASE_SPEC"), True, f"{fixed_name} must set FIX_REMOTE_LEASE_SPEC")
            self.assertEqual(
                check.get("FIX_REMOTE_LEASE_SPEC"), False, f"{check_name} must clear FIX_REMOTE_LEASE_SPEC")
            differing = {k for k in set(fixed) | set(check) if fixed.get(k) != check.get(k)}
            self.assertEqual(
                differing, {"FIX_REMOTE_LEASE_SPEC"},
                f"{fixed_name} and {check_name} must differ in FIX_REMOTE_LEASE_SPEC alone, "
                f"but also differ in {sorted(differing - {'FIX_REMOTE_LEASE_SPEC'})}")

            def invariants(text: str) -> set[str]:
                sections = run.cfg_sections(text)
                return set(sections.get("INVARIANT", []))

            self.assertEqual(
                invariants(configs[fixed_name]), invariants(configs[check_name]) | {"SingleWriter"},
                f"{fixed_name} must check exactly its sibling's invariants plus SingleWriter: the "
                f"sibling drops SingleWriter because it carries the open finding, and the whole point "
                f"of the fixed run is to check it with the flag on")


class UnionFromLogsTests(unittest.TestCase):
    """--union-from: judge the union from logs earlier CI jobs already wrote.

    The union used to be judged by re-running every run in one job; measured, that job could not fit
    its 180-minute cap, so the only place the union was judged never judged it (capstone, plan 3)."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.logs = Path(self._tmp.name) / "artifacts"
        # nested, as `gh run download` lays artifacts out: one directory per uploading job
        (self.logs / "tlc-output-x-posix").mkdir(parents=True)

    def write_log(self, name: str, case: str = "smoke_check_coverage") -> None:
        (self.logs / "tlc-output-x-posix" / f"{name}.log").write_text(fixture(case)[1], encoding="utf-8")

    def test_a_missing_log_fails_rather_than_under_reporting(self) -> None:
        d = ExpectedDir()
        self.addCleanup(d.close)
        expected = d.load()
        self.write_log(expected.runs[0].name)
        for extra in expected.runs[1:]:
            pass  # deliberately not written
        out = io.StringIO()
        with contextlib.redirect_stderr(out):
            code = run.judge_union_from(expected, d.path, self.logs)
        if len(expected.runs) > 1:
            self.assertEqual(code, 2)
            self.assertIn("cannot judge the union", out.getvalue())
            self.assertIn(expected.runs[1].name, out.getvalue())

    def test_a_log_with_no_complete_coverage_block_fails(self) -> None:
        """A log that is PRESENT but silent must fail, not count as "covered nothing".

        parse_coverage returns None rather than judging from a partial block. `or {}` used to turn
        that refusal into an empty coverage set, which inflates `uncovered` (loud) but SHRINKS
        all_covered - and all_covered is the only detector for a never_reached or deferred label
        that IS covered, so those two checks failed OPEN. Reachable because the per-run coverage
        gate covers check/liveness only, while witness and seeded runs also carry -coverage 1.
        """
        d = ExpectedDir()
        self.addCleanup(d.close)
        expected = d.load()
        for r in expected.runs:
            self.write_log(r.name)
        # one run's log is present and parses to no complete coverage block
        (self.logs / "tlc-output-x-posix" / f"{expected.runs[0].name}.log").write_text("", encoding="utf-8")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = run.judge_union_from(expected, d.path, self.logs)
        self.assertEqual(code, 1, "a silent log must fail the union, never be read as covering nothing")
        self.assertIn("no complete coverage block", out.getvalue())

    def test_no_logs_at_all_fails(self) -> None:
        d = ExpectedDir()
        self.addCleanup(d.close)
        expected = d.load()
        out = io.StringIO()
        with contextlib.redirect_stderr(out):
            code = run.judge_union_from(expected, d.path, self.logs)
        self.assertEqual(code, 2, "an empty artifact directory must fail, never pass vacuously")
        self.assertIn("cannot judge the union", out.getvalue())


class CostNodeTests(unittest.TestCase):
    """parse_cost_nodes(): the expression-granular data TLC emits and run.py used to discard.

    The suite's coverage gate is LABEL-granular, so a guarded branch inside a label that the
    non-taking path also reaches is invisible to it. That blind spot produced two capstone findings.
    TLC's `-coverage 1` has carried per-expression counts (message 2221) all along, including zeros.
    """

    def test_reads_every_cost_node_including_the_nested_ones(self) -> None:
        """The NESTED nodes are the branch-level ones, and the first version of this parser dropped
        them: TLC prefixes a node's depth with `|`, and the pattern matched leading whitespace
        only. It returned 10 of 17 while looking like it had read them all. Assert the COUNT,
        absence of this assertion is exactly what let that through."""
        text = fixture("smoke_check_coverage")[1]
        nodes = run.parse_cost_nodes(run.parse_messages(text))
        self.assertIsNotNone(nodes)
        self.assertEqual(len(nodes), text.count("@!@!@STARTMSG 2221"),
                         "every message-2221 line must be parsed, nested ones included")
        self.assertTrue(any(depth > 0 for *_rest, depth in nodes),
                        "the fixture carries nested nodes; if none parse, the depth support is dead")

    def test_an_unreadable_cost_line_fails_closed(self) -> None:
        text = fixture("smoke_check_coverage")[1]
        broken = text.replace("line 26, col 8", "LINE 26, col 8", 1)
        self.assertIsNone(run.parse_cost_nodes(run.parse_messages(broken)),
                          "a 2221 line the parser cannot read must be None, never a silent drop")

    def test_extracts_the_zero_count_nodes_of_a_constant_guarded_body(self) -> None:
        nodes = run.parse_cost_nodes(run.parse_messages(fixture("smoke_check_coverage")[1]))
        self.assertIsNotNone(nodes)
        zeros = [(module, line) for module, line, _col, count, _depth in nodes if count == 0]
        # Smoke.tla's Overshoot body is guarded by the constant SEED_OVERSHOOT and reports zero
        # in the non-seeded run. NOTE it is NOT the R4-1 shape, though this comment used to say
        # so: Overshoot is a top-level action whose own action line reads `0:0`, so the
        # LABEL-granular gate already catches it. R4-1's shape is a zero branch inside an action
        # that IS covered, and no recorded fixture contains one - so what this pins is that the
        # parser reads zero-count nodes at all, not that the blind spot is instrumented.
        self.assertEqual(zeros, [("Smoke", 26), ("Smoke", 27)])

    def test_fails_closed_like_parse_coverage(self) -> None:
        # No coverage block at all, and a block whose terminator never arrived.
        self.assertIsNone(run.parse_cost_nodes(run.parse_messages("no coverage here")))
        started = fixture("smoke_check_coverage")[1]
        cut = started[: started.rindex("@!@!@STARTMSG 2201")] + "@!@!@STARTMSG 2201:0 @!@!@" + chr(10)
        self.assertIsNone(
            run.parse_cost_nodes(run.parse_messages(cut)),
            "an unterminated final block must be None, never a partial answer")


class PartialCoverageTests(unittest.TestCase):
    """A seeded or witness run halts at its first counterexample, so its coverage block describes a
    PREFIX of the state space. Its positive coverage is sound; its SILENCE is not evidence."""

    def test_the_union_reports_which_runs_contributed_a_prefix(self) -> None:
        d = ExpectedDir()
        self.addCleanup(d.close)
        expected = d.load()
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        logs = Path(tmp.name)
        for r in expected.runs:
            (logs / f"{r.name}.log").write_text(fixture("smoke_check_coverage")[1], encoding="utf-8")
        halting = frozenset(r.name for r in expected.runs if r.kind in run.HALTING_KINDS)
        self.assertTrue(halting, "the fixture must contain a halting run or this test asserts nothing")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            run.judge_union_from(expected, d.path, logs)
        self.assertIn("PREFIX", out.getvalue(), "a union built partly from halting runs must say so")
        for name in halting:
            self.assertIn(name, out.getvalue())


if __name__ == "__main__":
    unittest.main()
