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
# Copies correctly, but only after 3 s: only the timeout can fail it.
SLOW = "import shutil, sys, time; time.sleep(3); shutil.copytree(sys.argv[1], sys.argv[2])"


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
            (dst / "a.bin").write_bytes(b"alpha!")
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

    def test_a_copy_that_does_not_finish_in_time_fails_the_tool(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            src = small_tree(Path(d))
            fake = Fake({"flux": 1.0, "slow": 1.0})
            got = measure.measure_case(
                src,
                9,
                [tool("flux", COPY), tool("slow", SLOW)],
                "windows",
                Path(d),
                run=fake.run,
                clock=fake.clock,
                free=lambda _: 0,
                log=lambda _: None,
                timeout=1.0,
            )
            self.assertEqual(got["failed"], ["slow"])
            self.assertEqual(got["ratio"], {})
            self.assertEqual(len(got["times_s"]["flux"]), 6, "the other tool is still timed")

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
