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

KINDS = ("check", "liveness", "seeded", "witness")
RUN_KEYS = {"name", "module", "config", "scenario", "kind", "violated", "open_findings", "unreached", "timeout_minutes",
            "constants", "symmetry", "tightened"}
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
    unreached: tuple[tuple[str, str], ...]
    timeout_minutes: int
    constants: tuple[tuple[str, object], ...]
    symmetry: tuple[str, str] | None
    tightened: tuple[str, str] | None


@dataclass(frozen=True)
class Expected:
    scenarios: list[str]
    runs: list[Run]
    never_reached: tuple[tuple[str, str], ...]


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


def cfg_constants(text: str) -> dict[str, object]:
    """Parse the literal-valued assignments in a TLC .cfg's CONSTANT/CONSTANTS section.

    An entry is returned only for a literal value: an integer literal (int), TRUE/FALSE (bool), a
    double-quoted string (str, unquoted), or a set literal of identifiers ({a, b} or {}, frozenset[str]).
    A model-value assignment (NAME = someIdentifier) or a substitution (NAME <- op) is omitted: neither
    names a literal the runner can compare against 'constants'."""
    tokens: list[str] = []
    current: str | None = None
    for match in _CFG_TOKEN.finditer(text):
        token = match.group(0)
        if _is_comment(token):
            continue
        if token in CFG_KEYWORDS:
            current = CFG_KEYWORDS[token]
            continue
        if current == "CONSTANT":
            tokens.append(token)

    result: dict[str, object] = {}
    i, n = 0, len(tokens)
    while i < n:
        name = tokens[i]
        i += 1
        if i >= n:
            break
        op = tokens[i]
        i += 1
        if op == "<-":
            if i < n:
                i += 1  # the substituted operator name; not a literal
            continue
        if op != "=" or i >= n:
            continue
        value = tokens[i]
        if value.startswith('"'):
            result[name] = value[1:-1]
            i += 1
        elif value == "{":
            i += 1
            elements: list[str] = []
            while i < n and tokens[i] != "}":
                if tokens[i] != ",":
                    elements.append(tokens[i])
                i += 1
            if i < n:
                i += 1  # consume "}"
            result[name] = frozenset(elements)
        elif value.isdigit():
            result[name] = int(value)
            i += 1
        elif value in ("TRUE", "FALSE"):
            result[name] = value == "TRUE"
            i += 1
        else:
            i += 1  # a model value; not a literal
    return result


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


def _load_label_reasons(value: object, subject: str) -> tuple[tuple[str, str], ...]:
    """Parse a list of {label, reason} tables, as used by 'unreached' and 'never_reached'.

    `subject` names the field for error messages, e.g. "run 'x-check': unreached" or "'never_reached'".
    A label must be a non-empty TLA+ identifier: real PlusCal labels (e.g. `plain_recover`) do not all
    start with `S<digits>_`, so that stricter shape is not checked here. Whether the label exists in the
    model at all is a later task's concern."""
    _require(isinstance(value, list), f"{subject} must be an array of tables")
    assert isinstance(value, list)
    entries: list[tuple[str, str]] = []
    for item in value:
        _require(isinstance(item, dict) and set(item) == {"label", "reason"},
                 f"{subject}: each entry has exactly 'label' and 'reason'")
        label, reason = item.get("label"), item.get("reason")
        _require(isinstance(label, str) and IDENT.fullmatch(label) is not None,
                 f"{subject}: each label must be a TLA+ identifier")
        _require(isinstance(reason, str) and reason, f"{subject}: each reason must be a non-empty string")
        entries.append((label, reason))
    labels = [label for label, _ in entries]
    _require(len(set(labels)) == len(labels), f"{subject} has duplicate labels")
    return tuple(entries)


def load_expected(path: Path) -> Expected:
    """Load and validate expected.toml; module and config paths are resolved beside it."""
    base = path.parent
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as err:
        raise ExpectedError(f"cannot read {path}: {err}") from err

    _require(set(data) <= {"scenarios", "run", "never_reached"},
             f"unknown top-level keys: {sorted(set(data) - {'scenarios', 'run', 'never_reached'})}")
    never_reached = _load_label_reasons(data.get("never_reached", []), "'never_reached'")
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
    return Expected(scenarios, runs, never_reached)


def _constants_equal(a: object, b: object) -> bool:
    """Compare two constants values: an int never equals a bool, even though bool is an int subclass."""
    if isinstance(a, bool) or isinstance(b, bool):
        return isinstance(a, bool) and isinstance(b, bool) and a == b
    if isinstance(a, int) and isinstance(b, int):
        return a == b
    if isinstance(a, str) and isinstance(b, str):
        return a == b
    if isinstance(a, frozenset) and isinstance(b, frozenset):
        return a == b
    return False


def _load_constants(raw: dict, where: str) -> dict[str, object]:
    """Parse and shape-check the 'constants' table: int (not bool), bool, str, or a set of strings."""
    _require("constants" in raw, f"{where}: 'constants' is required")
    value = raw["constants"]
    _require(isinstance(value, dict), f"{where}: constants must be a table")
    assert isinstance(value, dict)
    result: dict[str, object] = {}
    for key, v in value.items():
        _require(isinstance(key, str) and IDENT.fullmatch(key) is not None,
                 f"{where}: constants key {key!r} must be a TLA+ identifier")
        _require(FIX_FLAG.fullmatch(key) is None, f"{where}: constants must not contain a fix flag ({key})")
        if isinstance(v, bool):
            result[key] = v
        elif isinstance(v, int):
            result[key] = v
        elif isinstance(v, str):
            result[key] = v
        elif isinstance(v, list) and all(isinstance(e, str) for e in v):
            _require(len(set(v)) == len(v), f"{where}: constants {key!r} has duplicate elements")
            result[key] = frozenset(v)
        else:
            _require(False, f"{where}: constants {key!r} must be an int, bool, string, or array of strings")
    return result


def _load_run(raw: dict, i: int, scenarios: list[str], base: Path) -> Run:
    where = f"run #{i + 1}"
    _require(set(raw) <= RUN_KEYS, f"{where}: unknown keys {sorted(set(raw) - RUN_KEYS)}")
    for key in ("name", "module", "config", "scenario", "kind"):
        _require(isinstance(raw.get(key), str) and raw[key], f"{where}: '{key}' must be a non-empty string")
    name, module, config, scenario, kind = (raw[k] for k in ("name", "module", "config", "scenario", "kind"))
    where = f"run {name!r}"

    _require(kind in KINDS, f"{where}: kind must be one of {KINDS}")
    _require(scenario in scenarios, f"{where}: scenario {scenario!r} is not in 'scenarios'")

    # violated: forbidden for check/liveness (reachability comes from coverage, a later task), required
    # (exactly one) for seeded/witness.
    if kind in ("check", "liveness"):
        _require("violated" not in raw, f"{where}: a {kind} run must not declare 'violated'")
        violated: tuple[str, ...] = ()
    else:
        _require("violated" in raw, f"{where}: a {kind} run needs 'violated'")
        violated = _str_list(raw["violated"], f"{where}: violated")
        _require(len(violated) == 1, f"{where}: a {kind} run names exactly one invariant or property")

    if kind == "seeded":
        suffix = r"-[A-Z][A-Z0-9_]*"
        suffix_help = ", plus '-<SEED_FLAG>' exactly when it is seeded"
    elif kind == "witness":
        suffix = "-" + re.escape(violated[0])
        suffix_help = ", plus '-<Invariant>' equal to violated[0] when it is a witness"
    else:
        suffix = ""
        suffix_help = ", plus '-<SEED_FLAG>' exactly when it is seeded"
    _require(re.fullmatch(rf"{re.escape(scenario)}-[a-z0-9]+(-[a-z0-9]+)*-{kind}{suffix}", name) is not None,
             f"{where}: name must be '<scenario>-<variant>-<kind>'{suffix_help}")
    _require(IDENT.fullmatch(module) is not None and (base / f"{module}.tla").is_file(),
             f"{where}: module {module}.tla not found beside expected.toml")
    _require(re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]*(/[A-Za-z0-9_][A-Za-z0-9_.-]*)*\.cfg", config) is not None
             and ".." not in config.split("/"),
             f"{where}: config must be a relative path inside the model directory, with '/' separators")
    cfg_path = base / config
    _require(cfg_path.resolve().is_relative_to(base.resolve()) and cfg_path.is_file(),
             f"{where}: config {config} not found")

    timeout = raw.get("timeout_minutes")
    _require(isinstance(timeout, int) and not isinstance(timeout, bool) and timeout >= 1,
             f"{where}: timeout_minutes must be a whole number of minutes, at least 1")

    findings: list[OpenFinding] = []
    _require(kind in ("check", "liveness") or "open_findings" not in raw,
             f"{where}: open_findings is only allowed for check and liveness runs")
    raw_findings = raw.get("open_findings", [])
    _require(isinstance(raw_findings, list), f"{where}: open_findings must be an array of tables")
    for f in raw_findings:
        _require(isinstance(f, dict) and set(f) == FINDING_KEYS,
                 f"{where}: each open finding has exactly name, tracking, fix_flag")
        _require(all(isinstance(f[k], str) and f[k] for k in FINDING_KEYS), f"{where}: open finding fields must be non-empty strings")
        _require(IDENT.fullmatch(f["name"]) is not None, f"{where}: open finding name must be a TLA+ identifier")
        _require(FIX_FLAG.fullmatch(f["fix_flag"]) is not None, f"{where}: fix_flag must look like FIX_NAME")
        _require(f["name"] not in violated, f"{where}: {f['name']} is both expected and an open finding")
        findings.append(OpenFinding(f["name"], f["tracking"], f["fix_flag"]))
    _require(len({f.name for f in findings}) == len(findings), f"{where}: duplicate open findings")

    _require(kind in ("check", "liveness") or "unreached" not in raw,
             f"{where}: unreached is only allowed for check and liveness runs")
    unreached = _load_label_reasons(raw.get("unreached", []), f"{where}: unreached")

    raw_tightened = raw.get("tightened")
    _require(kind == "liveness" or raw_tightened is None, f"{where}: tightened is only allowed for liveness runs")
    tightened: tuple[str, str] | None = None
    if raw_tightened is not None:
        _require(isinstance(raw_tightened, dict) and set(raw_tightened) == {"rung", "measurement"},
                 f"{where}: tightened must be a table with 'rung' and 'measurement'")
        rung, measurement = raw_tightened.get("rung"), raw_tightened.get("measurement")
        _require(isinstance(rung, str) and rung, f"{where}: tightened.rung must be a non-empty string")
        _require(isinstance(measurement, str) and measurement, f"{where}: tightened.measurement must be a non-empty string")
        tightened = (rung, measurement)

    declared_constants = _load_constants(raw, where)

    raw_symmetry = raw.get("symmetry")
    _require(kind != "liveness" or raw_symmetry is None, f"{where}: a liveness run must not declare symmetry")
    symmetry: tuple[str, str] | None = None
    if raw_symmetry is not None:
        _require(isinstance(raw_symmetry, dict) and set(raw_symmetry) == {"definition", "over"},
                 f"{where}: symmetry must be a table with 'definition' and 'over'")
        definition, over = raw_symmetry.get("definition"), raw_symmetry.get("over")
        _require(isinstance(definition, str) and IDENT.fullmatch(definition) is not None,
                 f"{where}: symmetry.definition must be a non-empty TLA+ identifier")
        _require(isinstance(over, str) and IDENT.fullmatch(over) is not None,
                 f"{where}: symmetry.over must be a non-empty TLA+ identifier")
        symmetry = (definition, over)

    text = cfg_path.read_text(encoding="utf-8")
    sections = cfg_sections(text)
    _require("CHECK_DEADLOCK" not in sections, f"{where}: the config must not set CHECK_DEADLOCK (deadlock checking stays on)")
    _require(kind != "check" or "PROPERTY" not in sections,
             f"{where}: a check run's config must not declare a PROPERTY (TLC reports a temporal violation "
             "without naming it)")
    if kind == "witness":
        invariants = [t for t in sections.get("INVARIANT", []) if IDENT.fullmatch(t)]
        _require(invariants == [violated[0]],
                 f"{where}: a witness run's config lists exactly one invariant, {violated[0]!r}")
        _require("PROPERTY" not in sections, f"{where}: a witness run's config must not declare a PROPERTY")
    properties = cfg_properties(text)
    temporal_run = kind == "liveness" or (kind == "seeded" and violated[0] in properties)
    if temporal_run:
        _require(len(properties) == 1, f"{where}: a run that checks a temporal property lists exactly one PROPERTY "
                                       "(TLC does not name the property it reports violated)")
        _require("SYMMETRY" not in sections, f"{where}: symmetry is unsound with liveness checking")

    _require(("SYMMETRY" in sections) == (symmetry is not None),
             f"{where}: the config has a SYMMETRY section if and only if the run declares 'symmetry'")
    if symmetry is not None:
        definition, over = symmetry
        _require(sections["SYMMETRY"] == [definition],
                 f"{where}: the config's SYMMETRY must name exactly {definition!r}")
        module_text = (base / f"{module}.tla").read_text(encoding="utf-8")
        def_pattern = re.compile(rf"\b{re.escape(definition)}\b\s*==\s*Permutations\s*\(\s*{re.escape(over)}\s*\)")
        _require(def_pattern.search(module_text) is not None,
                 f"{where}: {module}.tla must define {definition} == Permutations({over})")
        over_value = declared_constants.get(over)
        _require(isinstance(over_value, frozenset) and len(over_value) >= 2,
                 f"{where}: symmetry.over ({over}) must be a 'constants' entry that is a set of at least two elements")

    cfg_consts = cfg_constants(text)
    for cname, cvalue in cfg_consts.items():
        if FIX_FLAG.fullmatch(cname):
            continue
        _require(cname in declared_constants,
                 f"{where}: the config assigns {cname}, which is not declared in 'constants'")
        _require(_constants_equal(cvalue, declared_constants[cname]),
                 f"{where}: constants.{cname} does not match the config's {cname} = {cvalue!r}")
    for dname in declared_constants:
        _require(dname in cfg_consts, f"{where}: constants has {dname!r}, which the config does not assign")

    if findings:
        fixed_cfg_text(text, [f.fix_flag for f in findings])  # raises if a flag is missing

    constants = tuple(sorted(declared_constants.items()))
    return Run(name, module, config, scenario, kind, violated, tuple(findings), unreached, timeout,
               constants, symmetry, tightened)


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
        got = sha256(jar) if jar.is_file() else "no file"
        if got == expected_sha:
            return jar
        failure = f"{mismatch} (expected {expected_sha}, got {got})"
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


def report(result: Result, tightened: tuple[str, str] | None = None) -> None:
    def names(s: frozenset[str]) -> str:
        return ",".join(sorted(s)) or "-"

    states = "?" if result.distinct_states is None else f"{result.distinct_states:,}"
    print(f"{result.status.upper():8} {result.name}  expected={names(result.expected)}  "
          f"observed={names(result.observed)}  states={states}  {result.seconds:.0f}s")
    if tightened is not None:
        rung, measurement = tightened
        print(f"         tightened: {rung} ({measurement})")
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
        expected = load_expected(args.expected)
    except ExpectedError as err:
        print(f"run.py: {err}", file=sys.stderr)
        return 2
    scenarios = expected.scenarios
    if args.list_scenarios:
        print(json.dumps(scenarios))
        return 0
    if args.scenario is not None and args.scenario not in scenarios:
        print(f"run.py: unknown scenario {args.scenario!r}; known: {', '.join(scenarios)}", file=sys.stderr)
        return 2
    selected = [r for r in expected.runs if args.scenario is None or r.scenario == args.scenario]

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
                report(result, run.tightened)
                results.append(result)
    except KeyboardInterrupt:
        print(f"run.py: interrupted after {len(results)} runs", file=sys.stderr)
        return 2
    code = exit_code(results)
    print(f"run.py: {len(results)} runs, exit {code}")
    return code


if __name__ == "__main__":
    sys.exit(main())
