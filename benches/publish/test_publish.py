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
