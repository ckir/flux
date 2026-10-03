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
