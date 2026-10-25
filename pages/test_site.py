# Copyright (c) 2026 Costas Kirgoussios
#
# This software is licensed under the PolyForm Noncommercial License 1.0.0.
# You may obtain a copy of the License at:
# https://polyformproject.org/licenses/noncommercial/1.0.0/

"""Structural tests for the landing page in site/."""

from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"

RELEASE_BASE = "https://github.com/ckir/flux/releases/latest"
RELEASE_ASSETS = (
    "flux-x86_64-unknown-linux-gnu.tar.gz",
    "flux-aarch64-unknown-linux-gnu.tar.gz",
    "flux-x86_64-apple-darwin.tar.gz",
    "flux-aarch64-apple-darwin.tar.gz",
    "flux-x86_64-pc-windows-msvc.zip",
)
EXPECTED_FILES = {"index.html", "style.css", "flux-field.js"}
IMAGE_SUFFIXES = {".png", ".jpg", ".jpeg", ".svg", ".webp", ".gif", ".ico"}


def read(name: str) -> str:
    return (SITE / name).read_text(encoding="utf-8")


class SiteTests(unittest.TestCase):
    def test_site_dir_is_flat_and_has_only_expected_files(self) -> None:
        entries = list(SITE.iterdir())
        self.assertTrue(entries, "site/ is missing or empty")
        for entry in entries:
            self.assertTrue(entry.is_file(), f"{entry.name} is not a plain file")
            self.assertTrue(
                entry.name in EXPECTED_FILES or entry.suffix.lower() in IMAGE_SUFFIXES,
                f"unexpected entry {entry.name}",
            )
        self.assertTrue(EXPECTED_FILES <= {e.name for e in entries})

    def test_no_external_resources(self) -> None:
        html = read("index.html")
        external = re.compile(
            r"<(?:script|link|img)\b[^>]*?\b(?:src|href)\s*=\s*[\"']?\s*(?:https?:)?//", re.I
        )
        self.assertIsNone(external.search(html))
        css = read("style.css")
        self.assertNotIn("@import", css)
        self.assertIsNone(re.search(r"url\(\s*[\"']?\s*(?:https?:)?//", css, re.I))

    def test_internal_links_are_relative(self) -> None:
        html = read("index.html")
        self.assertIsNone(re.search(r"""(?:href|src)\s*=\s*["']/""", html))
        self.assertIn("bench/", html)
        self.assertIn("api/flux_core/index.html", html)

    def test_five_release_links_and_latest_link_present(self) -> None:
        html = read("index.html")
        for asset in RELEASE_ASSETS:
            self.assertIn(f"{RELEASE_BASE}/download/{asset}", html)
        self.assertIn(f'href="{RELEASE_BASE}"', html)

    def test_repo_file_links_are_absolute_blob_urls(self) -> None:
        html = read("index.html")
        for name in ("ROADMAP.md", "FLUX_FULL_UPDATED_SPEC_V16.md", "LICENSE"):
            self.assertIn(f"https://github.com/ckir/flux/blob/main/{name}", html)

    def test_quickstart_commands_present(self) -> None:
        html = read("index.html")
        for command in (
            "./flux copy ./photos /mnt/backup",
            "./flux copy --resume ./photos /mnt/backup",
            ".\\flux.exe copy .\\photos D:\\backup",
        ):
            self.assertIn(command, html)

    def test_chart_has_object_fallback(self) -> None:
        html = read("index.html")
        match = re.search(r"<object\b([^>]*)>(.*?)</object>", html, re.S)
        self.assertIsNotNone(match)
        attrs, fallback = match.group(1), match.group(2)
        self.assertIn('data="bench/latest.svg"', attrs)
        self.assertIn('type="image/svg+xml"', attrs)
        self.assertRegex(attrs, r"\bwidth=")
        self.assertRegex(attrs, r"\bheight=")
        self.assertTrue(re.sub(r"<[^>]*>", "", fallback).strip(), "empty fallback text")

    def test_correctness_line_precedes_chart(self) -> None:
        html = read("index.html")
        phrase = html.lower().find("correctness before performance")
        chart = html.find("<object")
        self.assertGreaterEqual(phrase, 0)
        self.assertGreaterEqual(chart, 0)
        self.assertLess(phrase, chart)

    def test_no_unconditional_or_numeric_speed_claims(self) -> None:
        html = read("index.html")
        self.assertNotIn("outperform", html.lower())
        self.assertIsNone(re.search(r"\d+(\.\d+)?\s*[x×]", html))
        self.assertIsNone(re.search(r"\d+%\s*faster", html, re.I))

    def test_licence_and_status_stated(self) -> None:
        html = read("index.html")
        self.assertIn("PolyForm Noncommercial", html)
        self.assertIn("early implementation", html.lower())

    def test_install_lines_and_unsigned_note_present(self) -> None:
        html = read("index.html")
        self.assertIn("tar -xzf", html)
        self.assertTrue("unzip" in html or "Expand-Archive" in html)
        self.assertIn("Gatekeeper", html)
        self.assertIn("SmartScreen", html)

    def test_canvas_script_honours_reduced_motion_and_hidden_tab(self) -> None:
        js = read("flux-field.js")
        self.assertRegex(js, r"matchMedia\([^)]*prefers-reduced-motion")
        self.assertIn('addEventListener("visibilitychange"', js)
        self.assertIn("document.hidden", js)


if __name__ == "__main__":
    unittest.main()
