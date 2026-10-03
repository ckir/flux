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
