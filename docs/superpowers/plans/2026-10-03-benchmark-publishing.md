# Benchmark publishing - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** CI measures Flux against the copiers on each GitHub-hosted runner, keeps the history on a `bench-data`
branch, and publishes an inline `latest.svg` in the README and a trend page on the existing Pages site.

**Architecture:**
- **The harness:** a Python 3.14 package under `benches/publish/`, standard library only:
  - `registry` reads the comparator registry;
  - `cases` generates the source trees;
  - `measure` times the copies and writes `result-<os>.json`;
  - `store` and `stability` maintain the data files;
  - `svg` draws the summary;
  - `publish` updates a checkout of `bench-data`, and `push.sh` wraps it in the fetch-redo-push loop.
- **The workflows:** a reusable `bench-measure.yml` runs on each runner. `bench.yml` (every push to `main`) and
  `bench-calibrate.yml` (weekly) call it, then publish. `docs.yml` adds the data and the trend page to the one Pages
  deploy.

**Tech Stack:** Python 3.14 (`unittest`, `tomllib`, `hashlib`, `subprocess`), GitHub Actions, Chart.js 4.5.1 from
jsDelivr with SRI.

**Spec:** `docs/superpowers/specs/2026-10-03-benchmark-publishing-design.md` (approved at `b56c315`).

---

## Decisions this plan makes (each traced to the spec or to measured fact)

1. **Python is tested the way this repository already tests Python:** `unittest`, test files `test_*.py`, run with
   `python -W error -m unittest discover` (`.github/workflows/model.yml:41`) on Python 3.14 (`model.yml:36`). The
   harness's tests become a new job in `ci.yml`, "Bench harness". It is not a required check (`main` has 9; adding a
   job does not change them).
2. **The dry run cannot use `workflow_dispatch`.** GitHub dispatches a workflow only when the workflow file exists on the
   default branch, and these files are new. So, for the dry run only, both workflows also run on a push to
   `spec/bench-publish`. A run whose ref is not `main` always uses the tiny cases and writes to `bench-dry`, never
   `bench-data`. Task 9 removes the branch trigger once the dry run has passed.
3. **Comparator versions are optional.** macOS `cp` has no `--version` (it prints a usage error), and `robocopy` has
   none either. An entry without `version` records `unrecorded`. `rsync --version` works on Linux and macOS.
4. **Clone detection is measured, not assumed** (spec "The comparator registry"). For every case of at least 64 MiB,
   the harness reads the work directory's free space before and after each timed copy. A copy that consumed less than
   half its size in new space shares blocks with its source, and that tool is `failed` for the case.
5. **The `mixed` case's sizes:** 2 x 64 MiB, 8 x 16 MiB, 90 x 1 MiB, 200 x 4 KiB and 200 x 846 KiB. That is 500 files,
   sized from 4 KiB to 64 MiB, totalling 512.02 MiB (the spec: "500 files totalling 512 MiB, sizes spread from 4 KiB to
   64 MiB").
6. **`--tiny`** (dry run only) keeps every case's layout but divides each size by 1024 (at least 1 byte). It cuts
   `small` to 5 folders of 10 files and keeps every tenth `mixed` file.
7. **The Chart.js pin:** version 4.5.1 (jsDelivr's `latest` tag, read 2026-10-03), with integrity
   `sha384-jb8JQMbMoBUzgWatfe6COACi2ljcDdZQ2OxczGA3bGNeWe+6DChMTBJemed7ZnvJ`. Both were read from the file jsDelivr
   serves (`openssl dgst -sha384 -binary | openssl base64 -A`).
8. **Two `measure` and `publish` details the spec leaves to the plan:**
   - A tool that fails in one round is skipped for that case's remaining rounds; it is already `failed`, with no
     ratio.
   - When every measure job fails, `publish` finds no results and exits 0 having written nothing.
9. **The site is redeployed by an explicit dispatch, not by `workflow_run`.** The spec says `docs.yml` runs "on
   `workflow_run` of `bench.yml`". The calibration, though, is itself started by a dispatch made with the job's
   `GITHUB_TOKEN`. GitHub documents that such a dispatch "always create[s] workflow runs", and that "events triggered
   by the GITHUB_TOKEN will not create a new workflow run" otherwise. It documents nothing about a `workflow_run`
   following a run that a token-made dispatch started (both checked in GitHub's docs, 2026-10-03; plan panel r1). So
   `push.sh` runs `gh workflow run docs.yml` after every successful push to `bench-data`, from both publish jobs, each
   holding `actions: write`. That rests only on the documented dispatch rule, and `docs.yml` needs no new trigger.

## File structure

| File | Responsibility |
|---|---|
| `benches/comparators.toml` | the comparator registry (Task 1) |
| `benches/publish/registry.py`, `test_registry.py` | parse and validate the registry (Task 1) |
| `benches/publish/stats.py`, `test_stats.py` | median, coefficient of variation, round count, rotation (Task 2) |
| `benches/publish/cases.py`, `test_cases.py` | the three cases and their seeded generation (Task 2) |
| `benches/publish/measure.py`, `test_measure.py` | timing, cache drop, copy and clone checks, `result-<os>.json` (Task 3) |
| `benches/publish/store.py`, `stability.py`, `svg.py` and their tests | `data.json`, `stability.json`, `latest.svg` (Task 4) |
| `benches/publish/publish.py`, `test_publish.py`, `push.sh` | update the data branch (Task 5) |
| `benches/publish/site/index.html` | the trend page (Task 6) |
| `.github/workflows/bench-measure.yml`, `bench.yml`, `bench-calibrate.yml` | the measurement and publishing workflows (Task 7) |
| `.github/workflows/ci.yml`, `.github/workflows/docs.yml` | the harness test job; the bench pages in the deploy (Task 7) |
| `README.md` | the "Performance" section (Task 8) |

## Commands (every task)

- Harness tests, from `E:\Rust\flux-engine`: `python -W error -m unittest discover -s benches/publish -p "test_*.py"`.
  Expected: the last line is `OK`.
- One module: `python -W error -m unittest discover -s benches/publish -p "test_stats.py"`.
- Workflows (Task 7): `actionlint`. Expected: no output, exit 0.
- Before each commit, run `typos` on the changed files. Expected: no output.
- Never `git stash`. Never script an edit through a `python -` heredoc; use the Edit or Write tool.

---

### Task 1: The comparator registry

**Files:**
- Create: `benches/comparators.toml`
- Create: `benches/publish/registry.py`
- Test: `benches/publish/test_registry.py`

**Oracle:** spec "The comparator registry": its key table, its starting set, and "Flux itself is fixed, not a
registry entry". Plan decision 3.

- [ ] **Step 0: Verify the state.** `benches/` holds only `copy.rs`, and `benches/publish/` does not exist. If either
  differs, STOP and report `STATE_MISMATCH`.

- [ ] **Step 1: The tests are already written.** Create `benches/publish/test_registry.py`:

```python
"""Tests for registry.py: the comparator registry's format and its rules."""

from __future__ import annotations

import sys
import textwrap
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import registry  # noqa: E402

GOOD = """
[[comparator]]
name = "cp"
os = ["linux", "macos"]
command = ["cp", "-Rp", "{src}/.", "{dst}"]
source = "preinstalled"
"""


def entry(**fields: str) -> str:
    """One comparator table: GOOD's fields, with `fields` replaced or added (values are TOML literals)."""
    base = {
        "name": '"t"',
        "os": '["linux"]',
        "command": '["t", "{src}", "{dst}"]',
        "source": '"preinstalled"',
    }
    base.update(fields)
    return "[[comparator]]\n" + "".join(f"{k} = {v}\n" for k, v in base.items())


class RegistryTests(unittest.TestCase):
    def test_the_committed_registry_parses_to_the_starting_set(self) -> None:
        comps = registry.parse((HERE.parent / "comparators.toml").read_text("utf-8"))
        self.assertEqual([c.name for c in comps], ["robocopy", "cp", "rsync"])
        self.assertEqual([c.name for c in registry.for_os(comps, "windows")], ["robocopy"])
        self.assertEqual([c.name for c in registry.for_os(comps, "linux")], ["cp", "rsync"])
        robocopy = comps[0]
        self.assertEqual(robocopy.ok_exit_codes, frozenset(range(8)))

    def test_a_good_entry_parses_with_its_defaults(self) -> None:
        (cp,) = registry.parse(GOOD)
        self.assertEqual(cp.os, ("linux", "macos"))
        self.assertEqual(cp.ok_exit_codes, frozenset({0}))
        self.assertIsNone(cp.version)
        self.assertIsNone(cp.note)

    def test_placeholders_are_replaced_inside_each_argument_and_never_joined(self) -> None:
        (cp,) = registry.parse(GOOD)
        self.assertEqual(
            cp.argv("/tmp/a b", "/tmp/c d"),
            ["cp", "-Rp", "/tmp/a b/.", "/tmp/c d"],
        )

    def test_a_command_without_both_placeholders_is_refused(self) -> None:
        for command in ('["t", "{src}"]', '["t", "{dst}"]', '["t"]'):
            with self.subTest(command=command), self.assertRaisesRegex(registry.RegistryError, "src"):
                registry.parse(entry(command=command))

    def test_a_command_that_is_not_an_array_is_refused(self) -> None:
        with self.assertRaisesRegex(registry.RegistryError, "array"):
            registry.parse(entry(command='"t {src} {dst}"'))

    def test_an_unknown_os_is_refused(self) -> None:
        with self.assertRaisesRegex(registry.RegistryError, "unknown os"):
            registry.parse(entry(os='["linux", "freebsd"]'))

    def test_a_download_needs_an_https_url_and_a_sha256(self) -> None:
        sha = '"' + "a" * 64 + '"'
        with self.assertRaisesRegex(registry.RegistryError, "sha256"):
            registry.parse(entry(source='"download"', url='"https://example.com/t"'))
        with self.assertRaisesRegex(registry.RegistryError, "https"):
            registry.parse(entry(source='"download"', url='"http://example.com/t"', sha256=sha))
        (t,) = registry.parse(entry(source='"download"', url='"https://example.com/t"', sha256=sha))
        self.assertEqual(t.source, "download")

    def test_a_preinstalled_entry_carries_no_url(self) -> None:
        with self.assertRaisesRegex(registry.RegistryError, "preinstalled"):
            registry.parse(entry(url='"https://example.com/t"'))

    def test_flux_and_duplicates_and_unknown_keys_are_refused(self) -> None:
        with self.assertRaisesRegex(registry.RegistryError, "flux"):
            registry.parse(entry(name='"flux"'))
        with self.assertRaisesRegex(registry.RegistryError, "twice"):
            registry.parse(entry() + entry())
        with self.assertRaisesRegex(registry.RegistryError, "unknown keys"):
            registry.parse(entry(speed='"fast"'))

    def test_bad_exit_codes_are_refused(self) -> None:
        for codes in ("[]", '["0"]', "[true]"):
            with self.subTest(codes=codes), self.assertRaisesRegex(registry.RegistryError, "ok_exit_codes"):
                registry.parse(entry(ok_exit_codes=codes))

    def test_a_file_that_is_not_toml_is_refused(self) -> None:
        with self.assertRaisesRegex(registry.RegistryError, "TOML"):
            registry.parse(textwrap.dedent("[[comparator]\n"))


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run; it fails** (`ModuleNotFoundError: No module named 'registry'`).

- [ ] **Step 3: Implement.** Create `benches/comparators.toml`:

```toml
# The copiers Flux is measured against in CI (docs/superpowers/specs/2026-10-03-benchmark-publishing-design.md,
# "The comparator registry"). One [[comparator]] table per tool. `command` and `version` are argument arrays, run
# without a shell; `{src}` and `{dst}` are replaced inside each argument. Every command copies contents, times and
# permissions, as Flux does by default. To add a tool (FastCopy, say), add a table; `source = "download"` needs a
# pinned https `url` and its `sha256`.

[[comparator]]
name = "robocopy"
os = ["windows"]
command = ["robocopy", "{src}", "{dst}", "/E", "/COPY:DAT", "/DCOPY:T", "/NJH", "/NJS", "/NFL", "/NDL", "/NP"]
# robocopy's success codes: 0 nothing copied, 1 files copied, up to 7 with extra or mismatched files reported.
ok_exit_codes = [0, 1, 2, 3, 4, 5, 6, 7]
source = "preinstalled"

[[comparator]]
name = "cp"
os = ["linux", "macos"]
command = ["cp", "-Rp", "{src}/.", "{dst}"]
source = "preinstalled"

[[comparator]]
name = "rsync"
os = ["linux", "macos"]
command = ["rsync", "-rtp", "{src}/", "{dst}/"]
version = ["rsync", "--version"]
source = "preinstalled"
```

Create `benches/publish/registry.py`:

```python
"""The comparator registry (benches/comparators.toml): the copiers Flux is measured against, and on which OS.

The spec's "The comparator registry" defines the format. Flux itself is not an entry: it is always measured.
"""

from __future__ import annotations

import dataclasses
import re
import tomllib

OSES = ("windows", "linux", "macos")
KEYS = {"name", "os", "command", "ok_exit_codes", "version", "source", "url", "sha256", "note"}


class RegistryError(ValueError):
    """The registry is not valid; the message names the entry and the rule."""


@dataclasses.dataclass(frozen=True)
class Comparator:
    name: str
    os: tuple[str, ...]
    command: tuple[str, ...]
    ok_exit_codes: frozenset[int]
    version: tuple[str, ...] | None
    source: str
    url: str | None
    sha256: str | None
    note: str | None

    def argv(self, src: str, dst: str) -> list[str]:
        """The command with `{src}` and `{dst}` replaced inside each argument. Never joined into a shell string, so a
        path with spaces stays one argument."""
        return [a.replace("{src}", src).replace("{dst}", dst) for a in self.command]


def parse(text: str) -> list[Comparator]:
    """Every comparator in `text`, in file order. Raises `RegistryError` on the first rule broken."""
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        raise RegistryError(f"not TOML: {e}") from e
    extra = set(data) - {"comparator"}
    if extra:
        raise RegistryError(f"unknown top-level keys {sorted(extra)}")
    entries = data.get("comparator", [])
    if not isinstance(entries, list):
        raise RegistryError("`comparator` must be an array of tables ([[comparator]])")
    out: list[Comparator] = []
    for i, e in enumerate(entries):
        c = _comparator(i, e)
        if any(x.name == c.name for x in out):
            raise RegistryError(f"comparator {c.name!r} is declared twice")
        out.append(c)
    return out


def for_os(comparators: list[Comparator], os_name: str) -> list[Comparator]:
    """The comparators that run on `os_name`, in registry order."""
    return [c for c in comparators if os_name in c.os]


def _strings(where: str, key: str, value: object) -> tuple[str, ...]:
    if not isinstance(value, list) or not value or not all(isinstance(v, str) and v for v in value):
        raise RegistryError(f"{where}: `{key}` must be a non-empty array of non-empty strings")
    return tuple(value)


def _comparator(i: int, e: object) -> Comparator:
    where = f"comparator #{i + 1}"
    if not isinstance(e, dict):
        raise RegistryError(f"{where}: not a table")
    unknown = set(e) - KEYS
    if unknown:
        raise RegistryError(f"{where}: unknown keys {sorted(unknown)}")
    name = e.get("name")
    if not isinstance(name, str) or not re.fullmatch(r"[a-z0-9][a-z0-9_-]*", name):
        raise RegistryError(f"{where}: `name` must be lowercase letters, digits, '-' or '_'")
    if name == "flux":
        raise RegistryError(f"{where}: `flux` is not a comparator; it is always measured")
    where = f"comparator {name!r}"
    os_ = _strings(where, "os", e.get("os"))
    bad = [o for o in os_ if o not in OSES]
    if bad:
        raise RegistryError(f"{where}: unknown os {bad}; use {list(OSES)}")
    command = _strings(where, "command", e.get("command"))
    if not any("{src}" in a for a in command) or not any("{dst}" in a for a in command):
        raise RegistryError(f"{where}: `command` must contain both {{src}} and {{dst}}")
    codes = e.get("ok_exit_codes", [0])
    if (
        not isinstance(codes, list)
        or not codes
        or not all(isinstance(c, int) and not isinstance(c, bool) for c in codes)
    ):
        raise RegistryError(f"{where}: `ok_exit_codes` must be a non-empty array of integers")
    version = e.get("version")
    if version is not None:
        version = _strings(where, "version", version)
    source, url, sha = e.get("source"), e.get("url"), e.get("sha256")
    if source == "preinstalled":
        if url is not None or sha is not None:
            raise RegistryError(f"{where}: a preinstalled comparator has no `url` or `sha256`")
    elif source == "download":
        if not isinstance(sha, str) or not re.fullmatch(r"[0-9a-f]{64}", sha):
            raise RegistryError(f"{where}: a download needs `sha256`, 64 lowercase hex digits")
        if not isinstance(url, str) or not url.startswith("https://"):
            raise RegistryError(f"{where}: a download needs an https `url`")
    else:
        raise RegistryError(f'{where}: `source` must be "preinstalled" or "download"')
    note = e.get("note")
    if note is not None and not isinstance(note, str):
        raise RegistryError(f"{where}: `note` must be a string")
    return Comparator(name, os_, command, frozenset(codes), version, source, url, sha, note)
```

- [ ] **Step 4: Run.** `python -W error -m unittest discover -s benches/publish -p "test_registry.py"`. Expected: `OK`
  (11 tests).

- [ ] **Step 5: Commit.**

```bash
git add benches/comparators.toml benches/publish/registry.py benches/publish/test_registry.py
git commit -m "bench: the comparator registry and its rules"
```

---

### Task 2: Statistics and the cases

**Files:**
- Create: `benches/publish/stats.py`, `benches/publish/cases.py`
- Test: `benches/publish/test_stats.py`, `benches/publish/test_cases.py`

**Oracle:** spec "Cases" and "Measuring" step 3 (six rounds, rotation, "next multiple"); plan decisions 5 and 6.

- [ ] **Step 0: Verify the state.** Task 1's files exist; `stats.py` and `cases.py` do not. Else STOP:
  `STATE_MISMATCH`.

- [ ] **Step 1: The tests are already written.** Create `benches/publish/test_stats.py`:

```python
"""Tests for stats.py."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import stats  # noqa: E402


class StatsTests(unittest.TestCase):
    def test_round_count_is_six_or_the_next_multiple_of_the_tool_count(self) -> None:
        self.assertEqual([stats.round_count(n) for n in range(1, 8)], [6, 6, 6, 8, 10, 6, 7])

    def test_every_tool_holds_every_position_equally_often(self) -> None:
        for n in (2, 3, 4):
            tools = [f"t{i}" for i in range(n)]
            rounds = stats.round_count(n)
            for position in range(n):
                firsts = [stats.order(tools, r)[position] for r in range(rounds)]
                for t in tools:
                    self.assertEqual(firsts.count(t), rounds // n, (n, position, t))

    def test_order_keeps_every_tool_once(self) -> None:
        self.assertEqual(stats.order(["a", "b", "c"], 1), ["b", "c", "a"])
        self.assertEqual(sorted(stats.order(["a", "b", "c"], 5)), ["a", "b", "c"])

    def test_median_and_cv(self) -> None:
        self.assertEqual(stats.median([3.0, 1.0, 2.0]), 2.0)
        self.assertEqual(stats.median([1.0, 2.0, 3.0, 4.0]), 2.5)
        self.assertAlmostEqual(stats.cv([1.0, 1.0, 1.0]), 0.0)
        self.assertAlmostEqual(stats.cv([0.9, 1.1]), 0.1414213562, places=6)
        with self.assertRaises(ValueError):
            stats.cv([1.0])


if __name__ == "__main__":
    unittest.main()
```

Create `benches/publish/test_cases.py`:

```python
"""Tests for cases.py."""

from __future__ import annotations

import sys
import tempfile
import unittest
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import cases  # noqa: E402

MIB = 1024 * 1024


class CasesTests(unittest.TestCase):
    def test_the_full_cases_match_the_spec(self) -> None:
        large, small, mixed = cases.cases()
        self.assertEqual((large.name, small.name, mixed.name), ("large", "small", "mixed"))
        self.assertEqual(large.files, (("big.bin", 1024 * MIB),))
        self.assertEqual(len(small.files), 5000)
        self.assertEqual({size for _, size in small.files}, {4096})
        self.assertEqual(len({rel.split("/")[0] for rel, _ in small.files}), 50)
        self.assertEqual(len(mixed.files), 500)
        self.assertEqual(min(s for _, s in mixed.files), 4096)
        self.assertEqual(max(s for _, s in mixed.files), 64 * MIB)
        self.assertLess(abs(mixed.total - 512 * MIB), MIB // 10)
        self.assertEqual({rel.count("/") for rel, _ in mixed.files}, {3})

    def test_tiny_keeps_the_layout_and_shrinks_it(self) -> None:
        large, small, mixed = cases.cases(tiny=True)
        self.assertEqual(large.files, (("big.bin", MIB),))
        self.assertEqual(len(small.files), 50)
        self.assertEqual(len(mixed.files), 50)
        self.assertTrue(all(size >= 1 for _, size in mixed.files))

    def test_generation_is_deterministic_and_incompressible(self) -> None:
        _, small, mixed = cases.cases(tiny=True)
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            first = cases.generate(mixed, Path(a))
            second = cases.generate(mixed, Path(b))
            for rel, size in mixed.files:
                one = (first / rel).read_bytes()
                self.assertEqual(len(one), size, rel)
                self.assertEqual(one, (second / rel).read_bytes(), rel)
            sample = (cases.generate(small, Path(a)) / small.files[0][0]).read_bytes()
            self.assertGreater(len(zlib.compress(sample, 9)), len(sample) * 0.99)
            self.assertNotIn(b"\0" * 64, sample)

    def test_generating_again_replaces_the_tree(self) -> None:
        large, _, _ = cases.cases(tiny=True)
        with tempfile.TemporaryDirectory() as d:
            src = cases.generate(large, Path(d))
            (src / "stray").write_bytes(b"x")
            src = cases.generate(large, Path(d))
            self.assertEqual(sorted(p.name for p in src.iterdir()), ["big.bin"])


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run; both fail** (no module `stats`, no module `cases`).

- [ ] **Step 3: Implement.** Create `benches/publish/stats.py`:

```python
"""The arithmetic of a measurement: medians, the coefficient of variation, how many rounds, and in what order."""

from __future__ import annotations

import statistics
from typing import Sequence, TypeVar

T = TypeVar("T")


def median(xs: Sequence[float]) -> float:
    return statistics.median(xs)


def cv(xs: Sequence[float]) -> float:
    """The sample standard deviation over the mean. Needs at least two values."""
    if len(xs) < 2:
        raise ValueError("the coefficient of variation needs at least two values")
    return statistics.stdev(xs) / statistics.fmean(xs)


def round_count(n_tools: int) -> int:
    """Six, or the next multiple of `n_tools` above six, so each tool holds each position equally often."""
    return -(-6 // n_tools) * n_tools


def order(tools: Sequence[T], r: int) -> list[T]:
    """Round `r`'s order: the tool list rotated by `r`."""
    k = r % len(tools)
    return list(tools[k:]) + list(tools[:k])
```

Create `benches/publish/cases.py`:

```python
"""The three benchmark cases (spec "Cases") and their generation from a fixed seed.

Contents are pseudo-random, so every run copies the same bytes and no file is compressible or has runs of zeros a
filesystem could store sparsely.
"""

from __future__ import annotations

import dataclasses
import random
import shutil
from pathlib import Path

KIB = 1024
MIB = 1024 * KIB
GIB = 1024 * MIB
CHUNK = MIB


@dataclasses.dataclass(frozen=True)
class Case:
    name: str
    files: tuple[tuple[str, int], ...]

    @property
    def total(self) -> int:
        return sum(size for _, size in self.files)


def _shrink(size: int, tiny: bool) -> int:
    return max(size // 1024, 1) if tiny else size


def _large(tiny: bool) -> Case:
    return Case("large", (("big.bin", _shrink(GIB, tiny)),))


def _small(tiny: bool) -> Case:
    folders, per = (5, 10) if tiny else (50, 100)
    return Case("small", tuple((f"d{d:02}/f{i:04}.bin", 4 * KIB) for d in range(folders) for i in range(per)))


def _mixed(tiny: bool) -> Case:
    sizes = [64 * MIB] * 2 + [16 * MIB] * 8 + [MIB] * 90 + [4 * KIB] * 200 + [846 * KIB] * 200
    files = []
    for i, size in enumerate(sizes):
        if tiny and i % 10:
            continue
        rel = f"a{i % 5}/b{(i // 5) % 5}/c{(i // 25) % 4}/file{i:03}.bin"
        files.append((rel, _shrink(size, tiny)))
    return Case("mixed", tuple(files))


def cases(tiny: bool = False) -> list[Case]:
    """The cases in the order they run: `large`, `small`, `mixed`. `tiny` is for the dry run only."""
    return [_large(tiny), _small(tiny), _mixed(tiny)]


def generate(case: Case, root: Path) -> Path:
    """Write `case` under `root/src-<name>`, replacing anything there, and return that directory."""
    src = root / f"src-{case.name}"
    if src.exists():
        shutil.rmtree(src)
    for rel, size in case.files:
        path = src / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        rng = random.Random(f"flux-bench/{case.name}/{rel}")
        with path.open("wb") as f:
            left = size
            while left:
                n = min(left, CHUNK)
                f.write(rng.randbytes(n))
                left -= n
    return src
```

- [ ] **Step 4: Run.** `python -W error -m unittest discover -s benches/publish -p "test_*.py"`. Expected: `OK`.

- [ ] **Step 5: Commit.**

```bash
git add benches/publish/stats.py benches/publish/cases.py benches/publish/test_stats.py benches/publish/test_cases.py
git commit -m "bench: the rounds, the rotation and the three cases"
```

---

### Task 3: Measuring

**Files:**
- Create: `benches/publish/measure.py`
- Test: `benches/publish/test_measure.py`

**Oracle:** spec "Measuring" steps 1-5 and "Result shapes" (the `result-<os>.json` example); plan decisions 3, 4 and 8.

- [ ] **Step 0: Verify the state.** Tasks 1 and 2 exist; `measure.py` does not. Else STOP: `STATE_MISMATCH`.

- [ ] **Step 1: The tests are already written.** Create `benches/publish/test_measure.py`:

```python
"""Tests for measure.py. Real copies by Python's own copier stand in for the tools; a fake clock makes the times
exact."""

from __future__ import annotations

import hashlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import cases  # noqa: E402
import measure  # noqa: E402
import registry  # noqa: E402

COPY = "import shutil, sys; shutil.copytree(sys.argv[1], sys.argv[2])"
CORRUPT = (
    "import shutil, sys, pathlib; shutil.copytree(sys.argv[1], sys.argv[2]); "
    "f = sorted(p for p in pathlib.Path(sys.argv[2]).rglob('*') if p.is_file())[0]; "
    "b = bytearray(f.read_bytes()); b[0] ^= 1; f.write_bytes(bytes(b))"
)
EXIT = "import shutil, sys; shutil.copytree(sys.argv[1], sys.argv[2]); sys.exit(int(sys.argv[3]))"


def tool(name: str, script: str, *extra: str, ok: frozenset[int] = frozenset({0})) -> measure.Tool:
    """A tool whose last argument is its name, so the fake runner can charge it its cost."""
    return measure.Tool(name, (sys.executable, "-c", script, "{src}", "{dst}", *extra, name), ok)


class Fake:
    """Runs every command for real, then advances a fake clock by the tool's cost. `sync`, `sudo` and the like are
    recorded, not run."""

    def __init__(self, costs: dict[str, float]) -> None:
        self.now = 0.0
        self.costs = costs
        self.calls: list[list[str]] = []

    def clock(self) -> float:
        return self.now

    def run(self, argv: list[str], **kw: object) -> subprocess.CompletedProcess:
        self.calls.append(list(argv))
        if argv[0] != sys.executable:
            return subprocess.CompletedProcess(argv, 0, b"", b"")
        p = subprocess.run(argv, **kw)
        self.now += self.costs.get(argv[-1], 0.0)
        return p


def small_tree(root: Path) -> Path:
    src = root / "src"
    (src / "d").mkdir(parents=True)
    (src / "a.bin").write_bytes(b"alpha")
    (src / "d" / "b.bin").write_bytes(b"beta")
    return src


class MeasureTests(unittest.TestCase):
    def test_the_cache_is_dropped_on_linux_and_macos_and_not_on_windows(self) -> None:
        self.assertEqual(
            measure.cache_drop("linux"),
            [["sync"], ["sudo", "sh", "-c", "echo 3 > /proc/sys/vm/drop_caches"]],
        )
        self.assertEqual(measure.cache_drop("macos"), [["sync"], ["sudo", "purge"]])
        self.assertEqual(measure.cache_drop("windows"), [])

    def test_the_copy_check_finds_a_missing_an_extra_and_a_changed_file(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            src = small_tree(Path(d))
            dst = Path(d) / "dst"
            subprocess.run([sys.executable, "-c", COPY, str(src), str(dst)], check=True)
            expected = measure.tree_digest(src)
            self.assertIsNone(measure.check_copy(expected, dst))
            (dst / "extra").write_bytes(b"x")
            self.assertIn("extra", measure.check_copy(expected, dst) or "")
            (dst / "extra").unlink()
            (dst / "a.bin").write_bytes(b"alphA")
            self.assertIn("a.bin", measure.check_copy(expected, dst) or "")
            (dst / "a.bin").unlink()
            self.assertIn("missing", measure.check_copy(expected, dst) or "")
            self.assertIn("no destination", measure.check_copy(expected, Path(d) / "absent") or "")

    def test_a_copy_that_consumed_little_space_shares_blocks(self) -> None:
        gib = 1024 * 1024 * 1024
        self.assertTrue(measure.shares_blocks(gib, 10 * gib, 10 * gib - 4096))
        self.assertFalse(measure.shares_blocks(gib, 10 * gib, 9 * gib))
        self.assertFalse(measure.shares_blocks(1024, 10 * gib, 10 * gib))

    def test_a_case_is_timed_in_six_rotated_rounds_with_a_cache_drop_before_each_copy(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            src = small_tree(Path(d))
            fake = Fake({"flux": 2.0, "other": 1.0, "third": 4.0})
            tools = [tool("flux", COPY), tool("other", COPY), tool("third", COPY)]
            got = measure.measure_case(
                src, 9, tools, "macos", Path(d), run=fake.run, clock=fake.clock, free=lambda _: 0, log=lambda _: None
            )
            self.assertEqual(got["times_s"], {"flux": [2.0] * 6, "other": [1.0] * 6, "third": [4.0] * 6})
            self.assertEqual(got["median_s"], {"flux": 2.0, "other": 1.0, "third": 4.0})
            self.assertEqual(got["ratio"], {"other": 2.0, "third": 0.5})
            self.assertEqual(got["failed"], [])
            copies = [c[-1] for c in fake.calls if c[0] == sys.executable]
            self.assertEqual(copies[:3], ["flux", "other", "third"])
            self.assertEqual(copies[3:6], ["other", "third", "flux"])
            self.assertEqual(sum(1 for c in fake.calls if c == ["sudo", "purge"]), 18)
            self.assertFalse((Path(d) / "dst").exists(), "the destination is removed after the case")

    def test_windows_warms_each_tool_once_and_drops_nothing(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            src = small_tree(Path(d))
            fake = Fake({})
            tools = [tool("flux", COPY), tool("other", COPY)]
            measure.measure_case(
                src, 9, tools, "windows", Path(d), run=fake.run, clock=fake.clock, free=lambda _: 0, log=lambda _: None
            )
            copies = [c[-1] for c in fake.calls if c[0] == sys.executable]
            self.assertEqual(len(copies), 2 + 6 * 2)
            self.assertEqual(copies[:2], ["flux", "other"])
            self.assertEqual([c for c in fake.calls if c[0] != sys.executable], [])

    def test_a_wrong_copy_or_a_bad_exit_code_fails_the_tool_and_gets_no_ratio(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            src = small_tree(Path(d))
            fake = Fake({"flux": 1.0, "bad": 1.0, "exit1": 1.0, "exit3": 1.0})
            tools = [
                tool("flux", COPY),
                tool("bad", CORRUPT),
                tool("exit1", EXIT, "1", ok=frozenset({0, 1})),
                tool("exit3", EXIT, "3"),
            ]
            got = measure.measure_case(
                src, 9, tools, "windows", Path(d), run=fake.run, clock=fake.clock, free=lambda _: 0, log=lambda _: None
            )
            self.assertEqual(got["failed"], ["bad", "exit3"])
            self.assertEqual(got["ratio"], {"exit1": 1.0})
            self.assertEqual(len(got["times_s"]["exit1"]), 8, "four tools: eight rounds")

    def test_a_clone_fails_the_tool_on_a_big_enough_case(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            src = small_tree(Path(d))
            fake = Fake({"flux": 1.0, "other": 1.0})
            got = measure.measure_case(
                src,
                measure.SHARE_MIN,
                [tool("flux", COPY), tool("other", COPY)],
                "windows",
                Path(d),
                run=fake.run,
                clock=fake.clock,
                free=lambda _: 10**12,
                log=lambda _: None,
            )
            self.assertEqual(got["failed"], ["flux", "other"])
            self.assertEqual(got["ratio"], {})

    def test_a_download_is_kept_only_when_its_hash_matches(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            payload = Path(d) / "tool.bin"
            payload.write_bytes(b"#!/bin/sh\n")
            url = payload.resolve().as_uri()
            good = hashlib.sha256(b"#!/bin/sh\n").hexdigest()
            comp = registry.Comparator(
                "t", ("linux",), ("tool.bin", "{src}", "{dst}"), frozenset({0}), None, "download", url, good, None
            )
            got = measure.obtain(comp, Path(d) / "tools")
            self.assertIsNotNone(got)
            self.assertEqual(Path(got or "").read_bytes(), b"#!/bin/sh\n")
            bad = registry.Comparator(
                "t", ("linux",), ("tool.bin", "{src}", "{dst}"), frozenset({0}), None, "download", url, "0" * 64, None
            )
            self.assertIsNone(measure.obtain(bad, Path(d) / "tools2"))

    def test_a_preinstalled_tool_that_is_missing_is_skipped(self) -> None:
        comp = registry.Comparator(
            "t", ("linux",), ("no-such-tool-xyz", "{src}", "{dst}"), frozenset({0}), None, "preinstalled", None, None, None
        )
        self.assertIsNone(measure.obtain(comp, Path(tempfile.gettempdir())))

    def test_the_result_has_the_spec_shape(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            fake = Fake({"flux": 2.0, "other": 1.0})
            tools = [tool("flux", COPY), tool("other", COPY)]
            got = measure.measure_all(
                cases.cases(tiny=True)[:1],
                tools,
                "linux",
                Path(d),
                header={"commit": "c" * 40},
                run=fake.run,
                clock=fake.clock,
                free=lambda _: 0,
                log=lambda _: None,
            )
            self.assertEqual(got["schema"], 1)
            self.assertEqual(got["commit"], "c" * 40)
            self.assertEqual(list(got["cases"]), ["large"])
            self.assertEqual(got["cases"]["large"]["ratio"], {"other": 2.0})


if __name__ == "__main__":
    unittest.main()
```

**Oracle for the counts:** two tools give six rounds, three give six, four give eight (`stats.round_count`). Round 1
rotates the list by one (`stats.order`). On macOS, `cache_drop` gives two commands before each timed copy, so three
tools over six rounds make 18 `sudo purge` calls. In the clone test, `free` returns the same value before and after
each copy: a copy that consumed no space, at `SHARE_MIN` bytes. If a count differs, STOP and report; do not edit the
test.

- [ ] **Step 2: Run; it fails** (no module `measure`).

- [ ] **Step 3: Implement.** Create `benches/publish/measure.py`:

```python
"""Time Flux against the comparators on this runner and write result-<os>.json (spec "Measuring").

Run in CI by bench-measure.yml:
    python -u benches/publish/measure.py --os linux --flux target/release/flux --work "$RUNNER_TEMP/bench" \
        --out result-linux.json [--tiny]
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import time
import urllib.request
from pathlib import Path
from typing import Callable, Sequence

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import cases as cases_mod  # noqa: E402
import registry  # noqa: E402
import stats  # noqa: E402

SCHEMA = 1
# A case at least this big is checked for a copy that shares blocks with its source (plan decision 4).
SHARE_MIN = 64 * 1024 * 1024

Runner = Callable[..., subprocess.CompletedProcess]


@dataclasses.dataclass(frozen=True)
class Tool:
    name: str
    command: tuple[str, ...]
    ok_exit_codes: frozenset[int]

    def argv(self, src: str, dst: str) -> list[str]:
        return [a.replace("{src}", src).replace("{dst}", dst) for a in self.command]


def cache_drop(os_name: str) -> list[list[str]]:
    """The commands that empty the page cache before a timed copy. Windows has none: it is measured warm."""
    if os_name == "linux":
        return [["sync"], ["sudo", "sh", "-c", "echo 3 > /proc/sys/vm/drop_caches"]]
    if os_name == "macos":
        return [["sync"], ["sudo", "purge"]]
    return []


def tree_digest(root: Path) -> dict[str, str]:
    """Every file under `root`, by its relative path with `/`, to its SHA-256."""
    out = {}
    for path in sorted(root.rglob("*")):
        if path.is_file():
            h = hashlib.sha256()
            with path.open("rb") as f:
                for chunk in iter(lambda: f.read(1 << 20), b""):
                    h.update(chunk)
            out[path.relative_to(root).as_posix()] = h.hexdigest()
    return out


def check_copy(expected: dict[str, str], dst: Path) -> str | None:
    """None when `dst` holds exactly the files of `expected` (the source's `tree_digest`) with the same bytes;
    otherwise what is wrong."""
    if not dst.is_dir():
        return "no destination"
    a, b = expected, tree_digest(dst)
    missing, extra = sorted(set(a) - set(b)), sorted(set(b) - set(a))
    if missing:
        return f"missing {missing[0]}"
    if extra:
        return f"extra {extra[0]}"
    changed = sorted(k for k in a if a[k] != b[k])
    if changed:
        return f"content differs: {changed[0]}"
    return None


def shares_blocks(nbytes: int, free_before: int, free_after: int) -> bool:
    """A copy of `nbytes` that consumed less than half of it in new space shares blocks with its source."""
    return nbytes >= SHARE_MIN and free_before - free_after < nbytes // 2


def _remove(path: Path) -> None:
    def writable(fn: Callable[[str], object], p: str, _exc: object) -> None:
        os.chmod(p, stat.S_IWRITE)
        fn(p)

    if path.exists():
        shutil.rmtree(path, onexc=writable)


def _free(path: Path) -> int:
    return shutil.disk_usage(path).free


def measure_case(
    src: Path,
    nbytes: int,
    tools: Sequence[Tool],
    os_name: str,
    work: Path,
    *,
    run: Runner = subprocess.run,
    clock: Callable[[], float] = time.perf_counter,
    free: Callable[[Path], int] = _free,
    log: Callable[[str], None] = print,
) -> dict:
    """One case on this runner: the spec's "Measuring" steps 2-5."""
    dst = work / "dst"
    # The source is hashed once per case; each copy is compared against it (plan panel r1).
    expected = tree_digest(src)
    times: dict[str, list[float]] = {t.name: [] for t in tools}
    failed: dict[str, str] = {}
    if os_name == "windows":
        for t in tools:
            _remove(dst)
            run(t.argv(str(src), str(dst)), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for r in range(stats.round_count(len(tools))):
        for t in stats.order(list(tools), r):
            if t.name in failed:
                continue
            _remove(dst)
            for argv in cache_drop(os_name):
                run(argv, check=True)
            before = free(work)
            start = clock()
            proc = run(t.argv(str(src), str(dst)), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            elapsed = clock() - start
            if proc.returncode not in t.ok_exit_codes:
                err = (proc.stderr or b"")[-300:].decode("utf-8", "replace")
                failed[t.name] = f"exit code {proc.returncode}: {err}"
            elif shares_blocks(nbytes, before, free(work)):
                failed[t.name] = "the copy shares blocks with its source: a clone, not a copy"
            elif (problem := check_copy(expected, dst)) is not None:
                failed[t.name] = problem
            else:
                times[t.name].append(elapsed)
    _remove(dst)
    for name, why in sorted(failed.items()):
        log(f"{name} failed: {why}")
    medians = {n: stats.median(ts) for n, ts in times.items() if n not in failed and ts}
    ratio = {}
    if "flux" in medians:
        ratio = {n: medians["flux"] / m for n, m in medians.items() if n != "flux" and m > 0}
    return {"times_s": times, "median_s": medians, "failed": sorted(failed), "ratio": ratio}


def measure_all(
    the_cases: Sequence[cases_mod.Case], tools: Sequence[Tool], os_name: str, work: Path, *, header: dict, **kw: object
) -> dict:
    """Every case, in order, under `header`'s run fields."""
    result = {"schema": SCHEMA, **header, "cases": {}}
    for case in the_cases:
        src = cases_mod.generate(case, work)
        result["cases"][case.name] = measure_case(src, case.total, tools, os_name, work, **kw)
        _remove(src)
    return result


def obtain(c: registry.Comparator, tools_dir: Path) -> str | None:
    """The comparator's executable, or None to skip it: a preinstalled tool found on PATH, or a download whose SHA-256
    matches its pin."""
    if c.source == "preinstalled":
        return shutil.which(c.command[0])
    tools_dir.mkdir(parents=True, exist_ok=True)
    path = tools_dir / c.command[0]
    try:
        with urllib.request.urlopen(c.url or "") as response:
            data = response.read()
    except OSError as e:
        print(f"{c.name}: download failed: {e}")
        return None
    if hashlib.sha256(data).hexdigest() != c.sha256:
        print(f"{c.name}: SHA-256 does not match its pin; skipped")
        return None
    path.write_bytes(data)
    path.chmod(path.stat().st_mode | stat.S_IEXEC)
    return str(path)


def _first_line(argv: list[str]) -> str:
    try:
        p = subprocess.run(argv, capture_output=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return "unrecorded"
    for line in (p.stdout + p.stderr).decode("utf-8", "replace").splitlines():
        if line.strip():
            return line.strip()
    return "unrecorded"


def _defender() -> bool | None:
    command = "(Get-MpComputerStatus).RealTimeProtectionEnabled"
    line = _first_line(["powershell", "-NoProfile", "-NonInteractive", "-Command", command])
    return {"True": True, "False": False}.get(line)


def _git(*args: str) -> str:
    return subprocess.run(["git", *args], capture_output=True, text=True, check=True).stdout.strip()


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--os", required=True, choices=registry.OSES)
    p.add_argument("--flux", required=True, type=Path)
    p.add_argument("--registry", type=Path, default=HERE.parent / "comparators.toml")
    p.add_argument("--work", required=True, type=Path)
    p.add_argument("--out", required=True, type=Path)
    p.add_argument("--tiny", action="store_true")
    a = p.parse_args(argv)
    a.work.mkdir(parents=True, exist_ok=True)
    flux = str(a.flux.resolve())
    tools = [Tool("flux", (flux, "copy", "{src}", "{dst}"), frozenset({0}))]
    versions = {"flux": _first_line([flux, "--version"])}
    for c in registry.for_os(registry.parse(a.registry.read_text("utf-8")), a.os):
        exe = obtain(c, a.work / "tools")
        if exe is None:
            print(f"{c.name}: not available on this runner; skipped")
            continue
        tools.append(Tool(c.name, (exe, *c.command[1:]), c.ok_exit_codes))
        versions[c.name] = _first_line(list(c.version)) if c.version else "unrecorded"
    header = {
        "commit": os.environ.get("GITHUB_SHA") or _git("rev-parse", "HEAD"),
        "run": f"{os.environ.get('GITHUB_RUN_ID', '0')}.{os.environ.get('GITHUB_RUN_ATTEMPT', '0')}",
        "source": "+".join(_git("rev-parse", "HEAD:crates", "HEAD:Cargo.toml", "HEAD:Cargo.lock").split()),
        "date": datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "os": a.os,
        "image": {"os": os.environ.get("ImageOS", ""), "version": os.environ.get("ImageVersion", "")},
        "cache": "warm" if a.os == "windows" else "cold",
        "defender": _defender() if a.os == "windows" else None,
        "tools": versions,
    }
    result = measure_all(cases_mod.cases(a.tiny), tools, a.os, a.work, header=header)
    a.out.write_text(json.dumps(result, indent=2) + "\n", "utf-8")
    print(f"wrote {a.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 4: Run.** `python -W error -m unittest discover -s benches/publish -p "test_*.py"`. Expected: `OK`.

- [ ] **Step 5: Commit.**

```bash
git add benches/publish/measure.py benches/publish/test_measure.py
git commit -m "bench: time Flux against the comparators, check every copy, write result-<os>.json"
```

---

### Task 4: The data files and the summary image

**Files:**
- Create: `benches/publish/store.py`, `benches/publish/stability.py`, `benches/publish/svg.py`
- Test: `benches/publish/test_store.py`, `benches/publish/test_stability.py`, `benches/publish/test_svg.py`

**Oracle:** spec "Result shapes" (append-only, the highest `run` per (`commit`, `os`, image version), `schema`), "Noise"
(the gate, image-bound `stable`, the rolling check: after `calibrated`, same image, same `source`, at least 10 points,
3%), and "Where the data lives" (what `latest.svg` must say).

- [ ] **Step 0: Verify the state.** Tasks 1-3 exist; these files do not. Else STOP: `STATE_MISMATCH`.

- [ ] **Step 1: The tests are already written.** Create `benches/publish/test_store.py`:

```python
"""Tests for store.py."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import store  # noqa: E402


def run(commit: str, os_: str, run_id: str, image: str = "img1") -> dict:
    return {"schema": 1, "commit": commit, "os": os_, "run": run_id, "image": {"os": "x", "version": image}}


class StoreTests(unittest.TestCase):
    def test_a_missing_file_is_an_empty_store_and_an_unknown_schema_stops(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "data.json"
            self.assertEqual(store.load(path), {"schema": 1, "runs": []})
            path.write_text(json.dumps({"schema": 2, "runs": []}))
            with self.assertRaises(store.SchemaError):
                store.load(path)

    def test_runs_are_appended_never_replaced(self) -> None:
        data = store.load(Path("absent.json"))
        store.add(data, run("c1", "linux", "10.1"))
        store.add(data, run("c1", "linux", "10.2"))
        self.assertEqual([r["run"] for r in data["runs"]], ["10.1", "10.2"])
        with self.assertRaises(store.SchemaError):
            store.add(data, {**run("c2", "linux", "11.1"), "schema": 9})

    def test_the_plotted_run_is_the_highest_run_per_commit_os_and_image(self) -> None:
        data = {"schema": 1, "runs": [
            run("c1", "linux", "10.2"),
            run("c1", "linux", "9.1"),
            run("c1", "linux", "10.10"),
            run("c1", "linux", "12.1", image="img2"),
            run("c1", "macos", "10.1"),
        ]}
        got = [(r["os"], r["run"], r["image"]["version"]) for r in store.plotted(data)]
        # In run order: 10.1 (run 10, attempt 1) before 10.10 (attempt 10) before 12.1.
        self.assertEqual(got, [("macos", "10.1", "img1"), ("linux", "10.10", "img1"), ("linux", "12.1", "img2")])
        latest = store.latest_per_os(data)
        self.assertEqual({k: v["run"] for k, v in latest.items()}, {"linux": "12.1", "macos": "10.1"})

    def test_saving_and_loading_round_trips(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "data.json"
            data = {"schema": 1, "runs": [run("c1", "linux", "1.1")]}
            store.save(path, data)
            self.assertEqual(store.load(path), data)


if __name__ == "__main__":
    unittest.main()
```

Create `benches/publish/test_stability.py`:

```python
"""Tests for stability.py."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import stability  # noqa: E402


def result(os_: str, ratio: float, *, image: str = "img1", date: str = "2026-10-03T00:00:00Z",
           source: str = "s1", run_id: str = "1.1") -> dict:
    return {
        "schema": 1, "os": os_, "run": run_id, "date": date, "source": source,
        "image": {"os": "x", "version": image},
        "cases": {"small": {"ratio": {"cp": ratio}, "failed": []}},
    }


class CalibrateTests(unittest.TestCase):
    def test_ten_steady_ratios_make_a_stable_pair_and_noisy_ones_an_unstable_one(self) -> None:
        stab = stability.empty()
        steady = [result("linux", 1.0 + i * 0.001) for i in range(10)]
        noisy = [result("macos", 1.0 + (0.2 if i % 2 else 0.0)) for i in range(10)]
        stability.calibrate(stab, steady + noisy, "2026-10-04T00:00:00Z")
        linux = stab["pairs"]["linux/small/cp"]
        self.assertEqual((linux["stable"], linux["source"], linux["image"]), (True, "calibration", "img1"))
        self.assertEqual(linux["calibrated"], "2026-10-04T00:00:00Z")
        self.assertFalse(stab["pairs"]["macos/small/cp"]["stable"])

    def test_fewer_than_ten_ratios_or_mixed_images_calibrate_nothing(self) -> None:
        stab = stability.empty()
        log = stability.calibrate(stab, [result("linux", 1.0)] * 9, "d")
        self.assertEqual(stab["pairs"], {})
        self.assertTrue(any("not calibrated" in line for line in log))
        mixed = [result("linux", 1.0, image=f"img{i % 2}") for i in range(10)]
        log = stability.calibrate(stab, mixed, "d")
        self.assertEqual(stab["pairs"], {})
        self.assertTrue(any("several images" in line for line in log))


class StatusTests(unittest.TestCase):
    def test_a_pair_is_stable_only_on_its_calibrated_image(self) -> None:
        stab = stability.empty()
        stability.calibrate(stab, [result("linux", 1.0)] * 10, "d")
        self.assertEqual(stability.status(stab, "linux", "small", "cp", "img1"), "stable")
        self.assertEqual(stability.status(stab, "linux", "small", "cp", "img2"), "uncalibrated")
        self.assertEqual(stability.status(stab, "linux", "large", "cp", "img1"), "uncalibrated")

    def test_an_image_no_pair_was_calibrated_on_is_unseen(self) -> None:
        stab = stability.empty()
        stability.calibrate(stab, [result("linux", 1.0)] * 10, "d")
        got = stability.unseen(stab, [result("linux", 1.0, image="img2"), result("linux", 1.0), result("macos", 1.0)])
        self.assertEqual(got, ["linux", "macos"])


class RollingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.stab = stability.empty()
        stability.calibrate(self.stab, [result("linux", 1.0)] * 10, "2026-10-04T00:00:00Z")

    def points(self, ratios: list[float], **kw: str) -> dict:
        return {"schema": 1, "runs": [
            result("linux", x, date=f"2026-10-05T00:00:{i:02}Z", run_id=f"{100 + i}.1", **kw)
            for i, x in enumerate(ratios)
        ]}

    def test_ten_noisy_points_after_the_calibration_demote_the_pair(self) -> None:
        data = self.points([1.0 + (0.3 if i % 2 else 0.0) for i in range(10)])
        self.assertEqual(stability.rolling(self.stab, data), ["linux/small/cp"])
        pair = self.stab["pairs"]["linux/small/cp"]
        self.assertEqual((pair["stable"], pair["source"]), (False, "rolling"))

    def test_nine_noisy_points_give_no_verdict(self) -> None:
        data = self.points([1.0 + (0.3 if i % 2 else 0.0) for i in range(9)])
        self.assertEqual(stability.rolling(self.stab, data), [])
        self.assertTrue(self.stab["pairs"]["linux/small/cp"]["stable"])

    def test_points_before_the_calibration_or_on_another_image_or_source_are_ignored(self) -> None:
        noisy = [1.0 + (0.3 if i % 2 else 0.0) for i in range(10)]
        before = {"schema": 1, "runs": [
            result("linux", x, date=f"2026-10-01T00:00:{i:02}Z") for i, x in enumerate(noisy)
        ]}
        self.assertEqual(stability.rolling(self.stab, before), [])
        self.assertEqual(stability.rolling(self.stab, self.points(noisy, image="img2")), [])
        mixed_source = self.points(noisy)
        mixed_source["runs"][-1]["source"] = "s2"
        self.assertEqual(stability.rolling(self.stab, mixed_source), [], "only points at the latest source count")

    def test_a_calibration_overrides_a_demotion(self) -> None:
        stability.rolling(self.stab, self.points([1.0 + (0.3 if i % 2 else 0.0) for i in range(10)]))
        stability.calibrate(self.stab, [result("linux", 1.0)] * 10, "2026-10-06T00:00:00Z")
        pair = self.stab["pairs"]["linux/small/cp"]
        self.assertEqual((pair["stable"], pair["source"]), (True, "calibration"))


if __name__ == "__main__":
    unittest.main()
```

Create `benches/publish/test_svg.py`:

```python
"""Tests for svg.py."""

from __future__ import annotations

import sys
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import stability  # noqa: E402
import svg  # noqa: E402


def run(os_: str, *, failed: list[str] | None = None, defender: bool | None = None) -> dict:
    return {
        "schema": 1, "os": os_, "run": "5.1", "commit": "abcdef1234" + "0" * 30, "date": "2026-10-05T10:00:00Z",
        "image": {"os": "x", "version": "img1"}, "cache": "warm" if os_ == "windows" else "cold",
        "defender": defender, "tools": {"flux": "flux 0.5.0", "cp": "unrecorded", "rsync": "rsync 3"},
        "cases": {"small": {"ratio": {"cp": 1.25} if not failed else {}, "failed": failed or []}},
    }


def stab_with(**pairs: bool) -> dict:
    s = stability.empty()
    for key, stable in pairs.items():
        s["pairs"][key.replace("__", "/")] = {
            "cv": 0.01, "stable": stable, "source": "calibration", "calibrated": "d", "image": "img1"
        }
    return s


class SvgTests(unittest.TestCase):
    def text(self, image: str) -> str:
        return " ".join(t.text or "" for t in ET.fromstring(image).iter("{http://www.w3.org/2000/svg}text"))

    def test_a_stable_pair_shows_its_ratio_and_the_reading_aids(self) -> None:
        out = self.text(svg.render({"linux": run("linux")}, stab_with(linux__small__cp=True)))
        self.assertIn("Below 1.0, Flux is faster", out)
        self.assertIn("Latest benchmark of main", out)
        self.assertIn("abcdef1", out)
        self.assertIn("2026-10-05", out)
        self.assertIn("Linux", out)
        self.assertIn("cold cache", out)
        self.assertIn("1.25", out)

    def test_left_out_pairs_are_listed_with_their_reason(self) -> None:
        out = self.text(svg.render({"linux": run("linux")}, stab_with(linux__small__cp=False)))
        self.assertNotIn("1.25", out)
        self.assertIn("Not shown", out)
        self.assertIn("small vs cp: too noisy", out)
        self.assertIn("small vs rsync: not yet calibrated on this image", out)
        failed = self.text(svg.render({"linux": run("linux", failed=["cp"])}, stab_with(linux__small__cp=True)))
        self.assertIn("small vs cp: the copy failed its check", failed)

    def test_windows_has_its_own_block_with_defender_and_the_image_is_valid_xml(self) -> None:
        image = svg.render({"windows": run("windows", defender=True), "linux": run("linux")}, stab_with())
        out = self.text(image)
        self.assertLess(out.index("Linux"), out.index("Windows"))
        self.assertIn("warm cache, Defender on", out)
        self.assertIn("<&>", self.text(svg.render({"linux": {**run("linux"), "commit": "<&>" + "0" * 37}}, stab_with())))


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run; they fail** (no modules `store`, `stability`, `svg`).

- [ ] **Step 3: Implement.** Create `benches/publish/store.py`:

```python
"""data.json on the data branch: every run, append-only (spec "Result shapes")."""

from __future__ import annotations

import json
from pathlib import Path

SCHEMA = 1


class SchemaError(ValueError):
    """A file or a result carries a `schema` this code does not know: stop rather than misread it."""


def load(path: Path) -> dict:
    if not path.exists():
        return {"schema": SCHEMA, "runs": []}
    data = json.loads(path.read_text("utf-8"))
    if data.get("schema") != SCHEMA:
        raise SchemaError(f"{path}: schema {data.get('schema')!r}, expected {SCHEMA}")
    return data


def save(path: Path, data: dict) -> None:
    path.write_text(json.dumps(data, indent=1, sort_keys=True) + "\n", "utf-8")


def add(data: dict, result: dict) -> None:
    """Append `result`; a re-run of a commit is a new run, never a replacement."""
    if result.get("schema") != SCHEMA:
        raise SchemaError(f"result schema {result.get('schema')!r}, expected {SCHEMA}")
    data["runs"].append(result)


def run_key(r: dict) -> tuple[int, int]:
    """`<run id>.<attempt>` as numbers: later runs compare higher; dates can tie."""
    run_id, attempt = r["run"].split(".")
    return int(run_id), int(attempt)


def plotted(data: dict) -> list[dict]:
    """Per (`commit`, `os`, image version), the run with the highest `run`, in run order."""
    best: dict[tuple[str, str, str], dict] = {}
    for r in data["runs"]:
        k = (r["commit"], r["os"], r["image"]["version"])
        if k not in best or run_key(r) > run_key(best[k]):
            best[k] = r
    return sorted(best.values(), key=run_key)


def latest_per_os(data: dict) -> dict[str, dict]:
    """Each OS's run with the highest `run`."""
    out: dict[str, dict] = {}
    for r in data["runs"]:
        if r["os"] not in out or run_key(r) > run_key(out[r["os"]]):
            out[r["os"]] = r
    return out
```

Create `benches/publish/stability.py`:

```python
"""stability.json on the data branch: which (os, case, comparator) pairs are steady enough to publish (spec "Noise")."""

from __future__ import annotations

import json
from pathlib import Path

import stats
import store

SCHEMA = 1
LIMIT = 0.03  # the coefficient of variation at or below which a pair is stable
POINTS = 10  # the ratios a calibration needs, and the points the rolling check judges


def empty() -> dict:
    return {"schema": SCHEMA, "pairs": {}}


def load(path: Path) -> dict:
    if not path.exists():
        return empty()
    stab = json.loads(path.read_text("utf-8"))
    if stab.get("schema") != SCHEMA:
        raise store.SchemaError(f"{path}: schema {stab.get('schema')!r}, expected {SCHEMA}")
    return stab


def save(path: Path, stab: dict) -> None:
    path.write_text(json.dumps(stab, indent=1, sort_keys=True) + "\n", "utf-8")


def pair(os_: str, case: str, comp: str) -> str:
    return f"{os_}/{case}/{comp}"


def calibrate(stab: dict, results: list[dict], date: str) -> list[str]:
    """Set each pair's verdict from a calibration's results. A pair needs `POINTS` ratios, all on one image."""
    log: list[str] = []
    by_os: dict[str, list[dict]] = {}
    for r in results:
        by_os.setdefault(r["os"], []).append(r)
    for os_, rs in sorted(by_os.items()):
        images = sorted({r["image"]["version"] for r in rs})
        if len(images) != 1:
            log.append(f"{os_}: runs on several images {images}; not calibrated")
            continue
        ratios: dict[str, list[float]] = {}
        for r in rs:
            for case, c in r["cases"].items():
                for comp, x in c["ratio"].items():
                    ratios.setdefault(pair(os_, case, comp), []).append(x)
        for p, xs in sorted(ratios.items()):
            if len(xs) < POINTS:
                log.append(f"{p}: {len(xs)} ratios, {POINTS} needed; not calibrated")
                continue
            c = stats.cv(xs)
            stab["pairs"][p] = {
                "cv": round(c, 4), "stable": c <= LIMIT, "source": "calibration", "calibrated": date,
                "image": images[0],
            }
            log.append(f"{p}: cv {c:.4f}, {'stable' if c <= LIMIT else 'unstable'}")
    return log


def rolling(stab: dict, data: dict) -> list[str]:
    """Demote each stable pair whose last `POINTS` points since its calibration, on its image and at the latest
    `source`, vary above `LIMIT`. Returns the demoted pairs."""
    demoted = []
    runs = sorted(data["runs"], key=store.run_key)
    for p, e in sorted(stab["pairs"].items()):
        if not e["stable"]:
            continue
        os_, case, comp = p.split("/")
        pts = [
            (r["source"], r["cases"][case]["ratio"][comp])
            for r in runs
            if r["os"] == os_
            and r["image"]["version"] == e["image"]
            and r["date"] > e["calibrated"]
            and comp in r["cases"].get(case, {}).get("ratio", {})
        ]
        if not pts:
            continue
        latest = pts[-1][0]
        xs = [x for s, x in pts if s == latest][-POINTS:]
        if len(xs) == POINTS and (c := stats.cv(xs)) > LIMIT:
            e.update(stable=False, source="rolling", cv=round(c, 4))
            demoted.append(p)
    return demoted


def status(stab: dict, os_: str, case: str, comp: str, image: str) -> str:
    """`stable`, `unstable`, or `uncalibrated` (no calibration, or one on another image)."""
    e = stab["pairs"].get(pair(os_, case, comp))
    if e is None or e["image"] != image:
        return "uncalibrated"
    return "stable" if e["stable"] else "unstable"


def unseen(stab: dict, results: list[dict]) -> list[str]:
    """The OSes whose results ran on an image no pair of theirs was calibrated on."""
    out = set()
    for r in results:
        images = {e["image"] for p, e in stab["pairs"].items() if p.startswith(r["os"] + "/")}
        if r["image"]["version"] not in images:
            out.add(r["os"])
    return sorted(out)
```

Create `benches/publish/svg.py`:

```python
"""latest.svg: the latest run of main on each runner, with the reading aids the spec requires."""

from __future__ import annotations

from xml.sax.saxutils import escape

import stability

LABELS = {"linux": "Linux", "macos": "macOS", "windows": "Windows"}
LINE = 20
WIDTH = 720


def _defender(v: bool | None) -> str:
    return {True: "on", False: "off"}.get(v, "unknown")


def _lines(latest: dict[str, dict], stab: dict) -> list[tuple[str, str]]:
    lines = [
        ("Flux benchmark - Latest benchmark of main", "title"),
        ("Flux time / comparator time. Below 1.0, Flux is faster.", "note"),
    ]
    for os_ in ("linux", "macos", "windows"):
        r = latest.get(os_)
        if r is None:
            continue
        head = f"{LABELS[os_]} - {r['cache']} cache"
        if os_ == "windows":
            head += f", Defender {_defender(r.get('defender'))}"
        head += f" - {r['commit'][:7]} {r['date'][:10]}"
        lines.append(("", "gap"))
        lines.append((head, "head"))
        comps = [t for t in r["tools"] if t != "flux"]
        hidden = []
        for case, c in r["cases"].items():
            for comp in comps:
                if comp in c["failed"] or "flux" in c["failed"]:
                    hidden.append(f"{case} vs {comp}: the copy failed its check")
                    continue
                st = stability.status(stab, os_, case, comp, r["image"]["version"])
                if st == "stable" and comp in c["ratio"]:
                    lines.append((f"{case:<6} vs {comp:<10} {c['ratio'][comp]:.2f}", "row"))
                elif st == "unstable":
                    hidden.append(f"{case} vs {comp}: too noisy")
                else:
                    hidden.append(f"{case} vs {comp}: not yet calibrated on this image")
        if hidden:
            lines.append(("Not shown:", "note"))
            lines.extend((f"  {h}", "note") for h in hidden)
    return lines


def render(latest: dict[str, dict], stab: dict) -> str:
    lines = _lines(latest, stab)
    height = LINE * (len(lines) + 1)
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" '
        f'font-family="ui-monospace, Menlo, Consolas, monospace" font-size="13">',
        f'<rect width="{WIDTH}" height="{height}" fill="#ffffff"/>',
    ]
    weight = {"title": "bold", "head": "bold"}
    for i, (text, kind) in enumerate(lines):
        if kind == "gap":
            continue
        y = LINE * (i + 1)
        fill = "#555555" if kind == "note" else "#111111"
        out.append(
            f'<text x="12" y="{y}" fill="{fill}" font-weight="{weight.get(kind, "normal")}" '
            f'xml:space="preserve">{escape(text)}</text>'
        )
    out.append("</svg>")
    return "\n".join(out) + "\n"
```

`stability.py` and `svg.py` import their siblings by name; the tests put `benches/publish/` on `sys.path`, and so do
`measure.py` and `publish.py`.

- [ ] **Step 4: Run.** `python -W error -m unittest discover -s benches/publish -p "test_*.py"`. Expected: `OK`.

- [ ] **Step 5: Commit.**

```bash
git add benches/publish/store.py benches/publish/stability.py benches/publish/svg.py benches/publish/test_store.py benches/publish/test_stability.py benches/publish/test_svg.py
git commit -m "bench: data.json, stability.json and latest.svg"
```

---

### Task 5: Publishing to the data branch

**Files:**
- Create: `benches/publish/publish.py`, `benches/publish/push.sh`
- Test: `benches/publish/test_publish.py`

**Oracle:** spec "Where the data lives" (the publish job: append, rolling check, redraw, push; re-fetch and redo on a
rejected push, up to five times, never a git merge; the calibration publishes in the same group) and "Noise" (an unseen
image triggers a calibration). Plan decisions 2 and 8.

- [ ] **Step 0: Verify the state.** Tasks 1-4 exist; these files do not. Else STOP: `STATE_MISMATCH`.

- [ ] **Step 1: The tests are already written.** Create `benches/publish/test_publish.py`:

```python
"""Tests for publish.py: one update of a data-branch checkout."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import publish  # noqa: E402


def result(os_: str, ratio: float, run_id: str, image: str = "img1") -> dict:
    return {
        "schema": 1, "commit": "c" * 40, "run": run_id, "source": "s1", "date": "2026-10-05T00:00:00Z",
        "os": os_, "image": {"os": "x", "version": image}, "cache": "cold", "defender": None,
        "tools": {"flux": "flux", "cp": "unrecorded"},
        "cases": {"small": {"times_s": {}, "median_s": {}, "failed": [], "ratio": {"cp": ratio}}},
    }


class PublishTests(unittest.TestCase):
    def dirs(self, d: str) -> tuple[Path, Path, Path]:
        data, results = Path(d) / "data", Path(d) / "results"
        data.mkdir()
        results.mkdir()
        return data, results, Path(d) / "needs-calibration"

    def test_a_benchmark_appends_draws_and_asks_for_a_calibration_on_an_unseen_image(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            data, results, flag = self.dirs(d)
            (results / "result-linux.json").write_text(json.dumps(result("linux", 1.2, "5.1")))
            (results / "result-macos.json").write_text(json.dumps(result("macos", 0.8, "5.1")))
            publish.main(["bench", "--data", str(data), "--results", str(results), "--calibrate-flag", str(flag)])
            runs = json.loads((data / "data.json").read_text())["runs"]
            self.assertEqual(sorted(r["os"] for r in runs), ["linux", "macos"])
            self.assertIn("not yet calibrated on this image", (data / "latest.svg").read_text())
            self.assertEqual(flag.read_text().split(), ["linux", "macos"])

    def test_a_calibration_makes_the_pair_publishable(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            data, results, flag = self.dirs(d)
            (results / "result-linux.json").write_text(json.dumps(result("linux", 1.2, "5.1")))
            publish.main(["bench", "--data", str(data), "--results", str(results), "--calibrate-flag", str(flag)])
            cal = Path(d) / "cal"
            cal.mkdir()
            for i in range(10):
                (cal / f"result-linux-{i}.json").write_text(json.dumps(result("linux", 1.2, f"6.{i + 1}")))
            publish.main(["calibrate", "--data", str(data), "--results", str(cal)])
            stab = json.loads((data / "stability.json").read_text())
            self.assertTrue(stab["pairs"]["linux/small/cp"]["stable"])
            self.assertIn("1.20", (data / "latest.svg").read_text())
            runs = json.loads((data / "data.json").read_text())["runs"]
            self.assertEqual(len(runs), 1, "a calibration adds nothing to the trend")
            flag.unlink()
            (results / "result-linux.json").write_text(json.dumps(result("linux", 1.2, "7.1")))
            publish.main(["bench", "--data", str(data), "--results", str(results), "--calibrate-flag", str(flag)])
            self.assertFalse(flag.exists(), "a seen image asks for nothing")

    def test_no_results_writes_nothing(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            data, results, flag = self.dirs(d)
            publish.main(["bench", "--data", str(data), "--results", str(results), "--calibrate-flag", str(flag)])
            self.assertEqual(list(data.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run; it fails** (no module `publish`).

- [ ] **Step 3: Implement.** Create `benches/publish/publish.py`:

```python
"""Update a checkout of the data branch from measure.py's results (spec "Where the data lives").

    publish.py bench --data DIR --results DIR --calibrate-flag FILE
    publish.py calibrate --data DIR --results DIR

`push.sh` runs this inside its fetch, redo and push loop.
"""

from __future__ import annotations

import argparse
import datetime
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import stability  # noqa: E402
import store  # noqa: E402
import svg  # noqa: E402


def _results(directory: Path) -> list[dict]:
    return [json.loads(p.read_text("utf-8")) for p in sorted(directory.glob("result-*.json"))]


def _draw(data_dir: Path, data: dict, stab: dict) -> None:
    (data_dir / "latest.svg").write_text(svg.render(store.latest_per_os(data), stab), "utf-8")


def bench(data_dir: Path, results: list[dict], flag: Path) -> None:
    data = store.load(data_dir / "data.json")
    stab = stability.load(data_dir / "stability.json")
    for r in results:
        store.add(data, r)
    for p in stability.rolling(stab, data):
        print(f"{p}: demoted by the rolling check")
    store.save(data_dir / "data.json", data)
    stability.save(data_dir / "stability.json", stab)
    _draw(data_dir, data, stab)
    need = stability.unseen(stab, results)
    if need:
        flag.write_text("\n".join(need) + "\n", "utf-8")
        print(f"an image not yet calibrated on: {', '.join(need)}")


def calibrate(data_dir: Path, results: list[dict], date: str) -> None:
    data = store.load(data_dir / "data.json")
    stab = stability.load(data_dir / "stability.json")
    for line in stability.calibrate(stab, results, date):
        print(line)
    stability.save(data_dir / "stability.json", stab)
    _draw(data_dir, data, stab)


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("mode", choices=["bench", "calibrate"])
    p.add_argument("--data", required=True, type=Path)
    p.add_argument("--results", required=True, type=Path)
    p.add_argument("--calibrate-flag", type=Path)
    a = p.parse_args(argv)
    results = _results(a.results)
    if not results:
        print("no results: every measure job failed; nothing published")
        return 0
    if a.mode == "bench":
        if a.calibrate_flag is None:
            p.error("bench needs --calibrate-flag")
        bench(a.data, results, a.calibrate_flag)
    else:
        calibrate(a.data, results, datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

Create `benches/publish/push.sh`:

```bash
#!/usr/bin/env bash
# Publish measure.py's results to the data branch (spec "Where the data lives").
#
# Usage: push.sh bench|calibrate. Environment: DATA_BRANCH (default bench-data), RESULTS (default results),
# RUNNER_TEMP, GITHUB_SHA. Runs inside a checkout made by actions/checkout, whose credentials the worktree shares.
#
# A rejected push is never merged: the loop re-fetches the branch and redoes the whole update on its new contents, up
# to five times, because git cannot merge two edits of a JSON file safely.
set -euo pipefail

mode="$1"
branch="${DATA_BRANCH:-bench-data}"
results="${RESULTS:-results}"
flag="${RUNNER_TEMP:-/tmp}/needs-calibration"

git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"

for attempt in 1 2 3 4 5; do
  rm -rf data "$flag"
  git worktree prune
  if git ls-remote --exit-code --heads origin "$branch" > /dev/null; then
    git fetch --depth 1 origin "$branch"
    git worktree add --detach data FETCH_HEAD
  else
    git worktree add --detach data
    git -C data checkout --orphan bench-data-new
    git -C data rm -rfq .
  fi
  if [ "$mode" = bench ]; then
    python benches/publish/publish.py bench --data data --results "$results" --calibrate-flag "$flag"
  else
    python benches/publish/publish.py calibrate --data data --results "$results"
  fi
  git -C data add -A
  if git -C data diff --cached --quiet; then
    echo "nothing to publish"
    exit 0
  fi
  git -C data commit -qm "bench: $mode at ${GITHUB_SHA:-unknown}"
  if git -C data push origin "HEAD:refs/heads/$branch"; then
    # Only the real data branch is served. A dispatch with the job's GITHUB_TOKEN always starts a run (GitHub's docs:
    # "workflow_dispatch and repository_dispatch events always create workflow runs"); see plan decision 9.
    if [ "$branch" = bench-data ]; then
      if [ "$mode" = bench ] && [ -s "$flag" ]; then
        echo "a runner image is not yet calibrated; starting a calibration"
        gh workflow run bench-calibrate.yml
      fi
      echo "redeploying the site with the new data"
      gh workflow run docs.yml
    fi
    exit 0
  fi
  echo "push rejected (attempt $attempt of 5): re-fetching and redoing the update"
done
echo "gave up after five rejected pushes" >&2
exit 1
```

Then `git update-index --chmod=+x benches/publish/push.sh` after adding it (Windows checkouts keep no mode bit).

- [ ] **Step 4: Run.** `python -W error -m unittest discover -s benches/publish -p "test_*.py"`. Expected: `OK`. Then
  `shellcheck benches/publish/push.sh`. Expected: no output.

- [ ] **Step 5: Commit.**

```bash
git add benches/publish/publish.py benches/publish/push.sh benches/publish/test_publish.py
git update-index --chmod=+x benches/publish/push.sh
git commit -m "bench: publish results to the data branch, redoing the update on a rejected push"
```

---

### Task 6: The trend page

**Files:**
- Create: `benches/publish/site/index.html`

**Oracle:** spec "What a visitor sees" (one chart per (runner, case), one line per comparator, each point's commit,
date, image and tool versions, Windows apart), "Noise" (an image change breaks the line; only `stable` pairs on their
image), and the trend page bullet in "Where the data lives". Plan decision 7 (the pin).

- [ ] **Step 0: Verify the state.** `benches/publish/site/` does not exist. Else STOP: `STATE_MISMATCH`.

- [ ] **Step 1: Write the page.** Create `benches/publish/site/index.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Flux benchmarks</title>
  <style>
    :root { color-scheme: light dark; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
      max-width: 980px; margin: 40px auto; padding: 0 20px; line-height: 1.6;
    }
    h1 { border-bottom: 1px solid #8884; padding-bottom: 10px; }
    h2 { margin-top: 2.2em; }
    .note { font-size: .92em; opacity: .75; }
    .hidden { font-size: .9em; opacity: .8; }
    canvas { max-height: 320px; }
  </style>
  <script src="https://cdn.jsdelivr.net/npm/chart.js@4.5.1/dist/chart.umd.min.js"
          integrity="sha384-jb8JQMbMoBUzgWatfe6COACi2ljcDdZQ2OxczGA3bGNeWe+6DChMTBJemed7ZnvJ"
          crossorigin="anonymous"></script>
</head>
<body>
  <h1>Flux benchmarks</h1>
  <p>
    Each point is <b>Flux's median time divided by a comparator's</b>, both measured in the same GitHub Actions job.
    <b>Below 1.0, Flux is faster.</b> Six interleaved rounds per case; every copy is checked file by file (SHA-256).
    Linux and macOS drop the page cache before each copy; Windows cannot, and is measured warm, under its own heading.
    A pair is drawn only after a calibration (10 parallel runs) shows its ratio varies by at most 3%, and only on the
    runner image it was calibrated on. A new runner image breaks the line.
  </p>
  <p class="note">
    Cases: <code>large</code>, one 1 GiB file; <code>small</code>, 5,000 files of 4 KiB; <code>mixed</code>, 500 files
    totalling 512 MiB. Source: <code>benches/publish/</code> in the repository.
  </p>
  <div id="out"></div>
  <script>
    "use strict";
    const OSES = [["linux", "Linux"], ["macos", "macOS"], ["windows", "Windows"]];
    const CASES = ["large", "small", "mixed"];
    const key = r => r.run.split(".").map(Number);
    const order = (a, b) => key(a)[0] - key(b)[0] || key(a)[1] - key(b)[1];

    function status(stab, os, cs, comp, image) {
      const e = stab.pairs[`${os}/${cs}/${comp}`];
      if (!e || e.image !== image) return "not yet calibrated on this image";
      return e.stable ? "stable" : "too noisy";
    }

    function plotted(data) {
      const best = new Map();
      for (const r of data.runs) {
        const k = `${r.commit}|${r.os}|${r.image.version}`;
        const b = best.get(k);
        if (!b || order(r, b) > 0) best.set(k, r);
      }
      return [...best.values()].sort(order);
    }

    function chart(parent, title, runs, stab, os, cs) {
      const comps = [...new Set(runs.flatMap(r => Object.keys(r.tools).filter(t => t !== "flux")))];
      const labels = [], meta = [], series = Object.fromEntries(comps.map(c => [c, []]));
      let image = null;
      for (const r of runs) {
        if (image !== null && r.image.version !== image) {
          labels.push(`new image ${r.image.version}`); meta.push(null);
          for (const c of comps) series[c].push(null);
        }
        image = r.image.version;
        labels.push(r.commit.slice(0, 7)); meta.push(r);
        for (const c of comps) {
          const v = r.cases[cs] && r.cases[cs].ratio[c];
          series[c].push(v !== undefined && status(stab, os, cs, c, r.image.version) === "stable" ? v : null);
        }
      }
      const h = document.createElement("h3"); h.textContent = title; parent.append(h);
      const canvas = document.createElement("canvas"); parent.append(canvas);
      new Chart(canvas, {
        type: "line",
        data: { labels, datasets: comps.map(c => ({ label: `vs ${c}`, data: series[c], spanGaps: false })) },
        options: {
          scales: { y: { title: { display: true, text: "Flux time / comparator time (below 1.0: Flux faster)" } } },
          plugins: { tooltip: { callbacks: { afterBody: items => {
            const r = meta[items[0].dataIndex];
            if (!r) return "";
            const tools = Object.entries(r.tools).map(([t, v]) => `${t}: ${v}`);
            return [`${r.commit.slice(0, 12)}  ${r.date}`, `image ${r.image.os} ${r.image.version}`, ...tools];
          } } } },
        },
      });
      const last = runs[runs.length - 1], hidden = [];
      for (const c of comps) {
        const lc = last.cases[cs];
        if (!lc) continue;
        if (lc.failed.includes(c) || lc.failed.includes("flux")) hidden.push(`vs ${c}: the copy failed its check`);
        else {
          const s = status(stab, os, cs, c, last.image.version);
          if (s !== "stable") hidden.push(`vs ${c}: ${s}`);
        }
      }
      if (hidden.length) {
        const p = document.createElement("p"); p.className = "hidden";
        p.textContent = `Not shown in the latest run: ${hidden.join("; ")}.`; parent.append(p);
      }
    }

    async function main() {
      const out = document.getElementById("out");
      let data, stab;
      try {
        [data, stab] = await Promise.all(["data.json", "stability.json"].map(f => fetch(f).then(r => {
          if (!r.ok) throw new Error(`${f}: ${r.status}`);
          return r.json();
        })));
      } catch (e) {
        out.textContent = `No benchmark data yet (${e.message}).`;
        return;
      }
      if (data.schema !== 1 || stab.schema !== 1) {
        out.textContent = "The data uses a schema this page does not know; nothing is drawn.";
        return;
      }
      if (typeof Chart === "undefined") {
        out.textContent = "The chart library did not load; the data is at data.json.";
        return;
      }
      const runs = plotted(data);
      for (const [os, label] of OSES) {
        const rs = runs.filter(r => r.os === os);
        if (!rs.length) continue;
        const last = rs[rs.length - 1];
        const h = document.createElement("h2");
        h.textContent = `${label}: ${last.cache} cache` +
          (os === "windows" ? `, Defender ${last.defender === true ? "on" : last.defender === false ? "off" : "unknown"}` : "");
        out.append(h);
        for (const cs of CASES) chart(out, cs, rs, stab, os, cs);
      }
    }
    main();
  </script>
</body>
</html>
```

- [ ] **Step 2: Check it by hand,** in a scratch folder OUTSIDE the repository (`$SCRATCH` below), from the repository
  root:

```bash
cargo build --release -p flux-cli
mkdir -p "$SCRATCH/results" "$SCRATCH/data"
python benches/publish/measure.py --os windows --flux target/release/flux.exe --tiny --work "$SCRATCH/work" \
  --out "$SCRATCH/results/result-windows.json"
```

  (On Linux or macOS: `--os linux` or `--os macos`, and `--flux target/release/flux`.) Then:

```bash
python benches/publish/publish.py bench --data "$SCRATCH/data" --results "$SCRATCH/results" \
  --calibrate-flag "$SCRATCH/flag"
cp benches/publish/site/index.html "$SCRATCH/data/"
python -m http.server 8000 --directory "$SCRATCH/data"
```

  Open `http://localhost:8000/`. Expected:
  - one heading for the OS;
  - three charts, with no points drawn (nothing is calibrated yet);
  - a "Not shown in the latest run" line naming each pair "not yet calibrated on this image";
  - no error in the browser console.

  Record what was seen in the report.

- [ ] **Step 3: Commit.**

```bash
git add benches/publish/site/index.html
git commit -m "bench: the trend page"
```

---

### Task 7: The workflows

**Files:**
- Create: `.github/workflows/bench-measure.yml`, `.github/workflows/bench.yml`, `.github/workflows/bench-calibrate.yml`
- Modify: `.github/workflows/ci.yml` (a job after `deny`, before `test`)
- Modify: `.github/workflows/docs.yml:3-6` (triggers) and `:92-99` (a step before `upload-pages-artifact`)

**Oracle:** spec "Where the data lives", "Noise" (the calibration triggers), "Failure behaviour"; plan decisions 1 and
2.

- [ ] **Step 0: Verify the state.** `docs.yml` lines 3-6 are its `on:` block (`push` to `main`,
  `workflow_dispatch`), and line 97 is `- uses: actions/upload-pages-artifact@v5`. `ci.yml` has a job `deny:` followed
  by `test:`. The three bench workflows do not exist. If any differs, STOP: `STATE_MISMATCH`.

- [ ] **Step 1: The reusable measure workflow.** Create `.github/workflows/bench-measure.yml`:

```yaml
name: Bench (measure)

# One runner's measurement (spec "Measuring"), called by bench.yml and bench-calibrate.yml. It uploads one
# result-<os>-<artifact>.json as the artifact named `artifact`.
on:
  workflow_call:
    inputs:
      os:
        description: The runner label.
        type: string
        required: true
      tiny:
        description: Tiny cases, for the dry run only.
        type: boolean
        default: false
      artifact:
        description: The artifact's name, unique within the calling run.
        type: string
        required: true

permissions:
  contents: read

jobs:
  measure:
    name: Measure
    runs-on: ${{ inputs.os }}
    steps:
      - uses: actions/checkout@v7
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Build Flux (release)
        run: cargo build --release -p flux-cli
      - uses: actions/setup-python@v7
        with:
          python-version: "3.14"
      - name: Measure
        shell: bash
        env:
          TINY: ${{ inputs.tiny }}
          ARTIFACT: ${{ inputs.artifact }}
        run: |
          case "$RUNNER_OS" in
            Linux) os=linux; flux=target/release/flux ;;
            macOS) os=macos; flux=target/release/flux ;;
            Windows) os=windows; flux=target/release/flux.exe ;;
            *) echo "unknown runner OS $RUNNER_OS" >&2; exit 1 ;;
          esac
          args=(--os "$os" --flux "$flux" --work "$RUNNER_TEMP/bench" --out "result-$os-$ARTIFACT.json")
          if [ "$TINY" = true ]; then args+=(--tiny); fi
          python -u benches/publish/measure.py "${args[@]}"
      - uses: actions/upload-artifact@v7
        with:
          name: ${{ inputs.artifact }}
          path: result-*.json
          if-no-files-found: error
```

- [ ] **Step 2: The benchmark workflow.** Create `.github/workflows/bench.yml`:

```yaml
name: Bench

# Every push to main is measured on each runner and published to the data branch (spec "Where the data lives").
#
# DRY RUN (plan decision 2, removed by Task 9): a push to spec/bench-publish runs the tiny cases into bench-dry,
# because workflow_dispatch reaches only workflows already on the default branch.
on:
  push:
    branches: [main, spec/bench-publish]
  workflow_dispatch:

permissions:
  contents: read

jobs:
  measure:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    uses: ./.github/workflows/bench-measure.yml
    with:
      os: ${{ matrix.os }}
      tiny: ${{ github.ref_name != 'main' }}
      artifact: result-${{ matrix.os }}

  publish:
    name: Publish
    needs: measure
    # A failed runner has no point for this commit; the others still publish (spec "Failure behaviour").
    if: always()
    runs-on: ubuntu-latest
    permissions:
      contents: write
      actions: write
    # Shared with bench-calibrate.yml, so the two never write the data branch at once. Queued, never cancelled.
    concurrency:
      group: bench
      cancel-in-progress: false
    steps:
      - uses: actions/checkout@v7
      - uses: actions/setup-python@v7
        with:
          python-version: "3.14"
      - uses: actions/download-artifact@v8
        with:
          pattern: result-*
          path: results
          merge-multiple: true
      - name: Publish to the data branch
        env:
          DATA_BRANCH: ${{ github.ref_name == 'main' && 'bench-data' || 'bench-dry' }}
          GH_TOKEN: ${{ github.token }}
        run: bash benches/publish/push.sh bench
```

- [ ] **Step 3: The calibration workflow.** Create `.github/workflows/bench-calibrate.yml`:

```yaml
name: Bench calibration

# Ten parallel measurements per runner at one commit decide which pairs are stable (spec "Noise"). Weekly, on
# dispatch, and from bench.yml when a runner reports an image no pair was calibrated on.
#
# DRY RUN (plan decision 2, removed by Task 9): a push to spec/bench-publish runs the tiny cases into bench-dry.
on:
  schedule:
    - cron: "17 3 * * 1"
  workflow_dispatch:
  push:
    branches: [spec/bench-publish]

permissions:
  contents: read

jobs:
  measure:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
        rep: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    uses: ./.github/workflows/bench-measure.yml
    with:
      os: ${{ matrix.os }}
      tiny: ${{ github.ref_name != 'main' }}
      artifact: cal-${{ matrix.os }}-${{ matrix.rep }}

  publish:
    name: Publish calibration
    needs: measure
    if: always()
    runs-on: ubuntu-latest
    permissions:
      contents: write
      actions: write
    concurrency:
      group: bench
      cancel-in-progress: false
    steps:
      - uses: actions/checkout@v7
      - uses: actions/setup-python@v7
        with:
          python-version: "3.14"
      - uses: actions/download-artifact@v8
        with:
          pattern: cal-*
          path: results
          merge-multiple: true
      - name: Publish the calibration to the data branch
        env:
          DATA_BRANCH: ${{ github.ref_name == 'main' && 'bench-data' || 'bench-dry' }}
          GH_TOKEN: ${{ github.token }}
        run: bash benches/publish/push.sh calibrate
```

- [ ] **Step 4: The harness tests in CI.** In `.github/workflows/ci.yml`, insert after the `deny:` job and before the
  `test:` job:

```yaml
  bench-harness:
    # The benchmark harness's own tests (benches/publish/), as models/lockproto's are run in model.yml.
    name: Bench harness
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: actions/setup-python@v7
        with:
          python-version: "3.14"
      - run: python -W error -m unittest discover -s benches/publish -p "test_*.py"
```

- [ ] **Step 5: The site.** `docs.yml`'s triggers stay as they are: `push.sh` dispatches it after each successful
  push to `bench-data` (plan decision 9). Insert, immediately before the comment block that precedes
  `- uses: actions/upload-pages-artifact@v5` (the `# No actions/configure-pages:` comment, line 92):

```yaml
      - name: Add the benchmark pages
        # The data branch goes into target/doc/bench/ beside the trend page, so one deploy serves the docs and the
        # benchmarks (spec "Where the data lives"). Before the first benchmark, or if the branch was deleted, the docs
        # deploy alone.
        run: |
          if git ls-remote --exit-code --heads origin bench-data > /dev/null; then
            git fetch --depth 1 origin bench-data
            mkdir -p target/doc/bench
            git archive FETCH_HEAD | tar -x -C target/doc/bench
            cp benches/publish/site/index.html target/doc/bench/index.html
          else
            echo "no bench-data branch yet: deploying the docs without bench/"
          fi

```

- [ ] **Step 6: Lint.** `actionlint`. Expected: no output, exit 0. If it reports anything, fix the workflow, not the
  linter's settings, and report what it said.

- [ ] **Step 7: Commit.**

```bash
git add .github/workflows/bench-measure.yml .github/workflows/bench.yml .github/workflows/bench-calibrate.yml .github/workflows/ci.yml .github/workflows/docs.yml
git commit -m "ci: measure and publish the benchmarks; the harness tests; the bench pages in the docs deploy"
```

---

### Task 8: The README

**Files:**
- Modify: `README.md` (a new section before `## Goals`)

**Oracle:** spec "What a visitor sees" (the inline image, three lines of method, the link).

- [ ] **Step 0: Verify the state.** `README.md` has a line `## Goals`. Else STOP: `STATE_MISMATCH`.

- [ ] **Step 1: Insert, immediately before `## Goals`:**

```markdown
## Performance

![Flux against other copiers: the latest benchmark of main](https://ckir.github.io/flux/bench/latest.svg)

Each figure is Flux's median time divided by another copier's (robocopy on Windows; `cp` and `rsync` on Linux and
macOS), measured in the same GitHub Actions job: below 1.0, Flux is faster. Linux and macOS copy with a cold cache,
Windows with a warm one, and a pair appears only once a calibration shows its ratio is steady.
[The trend over time, and how it is measured.](https://ckir.github.io/flux/bench/)

```

- [ ] **Step 2: Commit.**

```bash
git add README.md
git commit -m "README: the performance section"
```

---

### Task 9: The dry run (driver, not a subagent)

The driver runs this: it pushes, reads CI, and judges the results.

- [ ] **Step 1:** Push `spec/bench-publish`. The push runs `Bench` and `Bench calibration` with the tiny cases into
  `bench-dry` (plan decision 2), as well as `CI`.
- [ ] **Step 2: Check `Bench`.**
  - Each runner's measure job succeeded and uploaded `result-<os>-result-<runner>.json`.
  - In each result: a ratio for every comparator, `defender` recorded on Windows, `image.version` set, and `source`
    holding three object ids.
  - Any `failed` tool has a reason in the log. A macOS `cp` clone would show here as "shares blocks". The tiny cases
    are below `SHARE_MIN`, so for the clone check run `measure.py` once with full cases on macOS. Do it with a manual
    `workflow_dispatch` after the merge, or locally if a Mac is available. Record what it shows.
  - `bench-dry` holds `data.json`, `stability.json` and `latest.svg`.
- [ ] **Step 3: Check `Bench calibration`.** 30 measure jobs ran, and `bench-dry`'s `stability.json` gained pairs. With
  tiny cases they may be unstable; the point is that the path works. The publish job's log shows the queueing:
  - no rejected push;
  - or one rejected push followed by a re-fetch, then success.
- [ ] **Step 4: Remove the dry-run trigger.** In `bench.yml`, `branches: [main, spec/bench-publish]` becomes
  `branches: [main]`, and the DRY RUN comment paragraph goes. In `bench-calibrate.yml`, delete the `push:` trigger and
  its comment paragraph. Then run `actionlint`, commit (`ci: the benchmark workflows run on main only`), and push.
- [ ] **Step 5:** Delete `bench-dry`: `git push origin --delete bench-dry`.
- [ ] **Step 6:** Gates. `python -W error -m unittest discover -s benches/publish -p "test_*.py"`, `actionlint`, and
  `just check` (the Rust gate is unchanged but must stay green). Then CI.
- [ ] **Step 7: After the merge (owner's PR decision).**
  - The first push to `main` runs `Bench` into `bench-data`, and that new image triggers `Bench calibration`.
  - The `Docs` deploy then serves `https://ckir.github.io/flux/bench/`.
  - Check that the README image renders on GitHub, and record it in the memory ledger.
