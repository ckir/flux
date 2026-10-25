# Copyright (c) 2026 Costas Kirgoussios
#
# This software is licensed under the PolyForm Noncommercial License 1.0.0.
# You may obtain a copy of the License at:
# https://polyformproject.org/licenses/noncommercial/1.0.0/

"""Structural tests for the landing page in site/."""

from __future__ import annotations

import re
import unittest
from html.parser import HTMLParser
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


ALLOWED_ABSOLUTE = {f"{RELEASE_BASE}/download/{asset}" for asset in RELEASE_ASSETS} | {
    RELEASE_BASE,
    "https://github.com/ckir/flux/blob/main/ROADMAP.md",
    "https://github.com/ckir/flux/blob/main/FLUX_FULL_UPDATED_SPEC_V16.md",
    "https://github.com/ckir/flux/blob/main/LICENSE",
    "https://github.com/ckir/flux",
}
API_HREFS = tuple(
    f"api/{crate}/index.html"
    for crate in ("flux_core", "flux_fs", "flux_platform", "flux_hash", "flux_ui", "flux")
)
HIDDEN_TEXT_TAGS = {"script", "style", "title"}


class Page(HTMLParser):
    """Collects tags, anchors and the visible text (no comments, script, style or title)."""

    def __init__(self, source: str) -> None:
        super().__init__(convert_charrefs=True)
        self.tags: list[tuple[str, dict[str, str]]] = []
        self.hrefs: list[str] = []
        self.events: list[tuple[str, str]] = []  # ("text", str) or ("tag", name)
        self.object_attrs: dict[str, str] | None = None
        self.fallback = ""
        self._hidden = 0
        self._object_depth = 0
        self._code_depth = 0
        self.feed(source)
        self.close()

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attributes = {name: value or "" for name, value in attrs}
        self.tags.append((tag, attributes))
        self.events.append(("tag", tag))
        if tag == "a" and "href" in attributes:
            self.hrefs.append(attributes["href"])
        if tag in HIDDEN_TEXT_TAGS:
            self._hidden += 1
        if tag == "object" and self.object_attrs is None:
            self.object_attrs = attributes
            self._object_depth = 1
        elif self._object_depth and tag == "code":
            self._code_depth += 1

    def handle_endtag(self, tag: str) -> None:
        if tag in HIDDEN_TEXT_TAGS and self._hidden:
            self._hidden -= 1
        if self._object_depth:
            if tag == "object":
                self._object_depth = 0
            elif tag == "code" and self._code_depth:
                self._code_depth -= 1

    def handle_data(self, data: str) -> None:
        if self._hidden:
            return
        self.events.append(("text", data))
        if self._object_depth and not self._code_depth:
            self.fallback += data

    @property
    def text(self) -> str:
        return "".join(value for kind, value in self.events if kind == "text")


def page() -> Page:
    return Page(read("index.html"))


def strip_js_comments(js: str) -> str:
    js = re.sub(r"/\*.*?\*/", "", js, flags=re.S)
    return re.sub(r"(?m)//.*$", "", js)


def is_external(value: str) -> bool:
    value = value.strip().lower()
    return value.startswith(("http:", "https:", "//"))


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
        parsed = page()
        for tag, attrs in parsed.tags:
            self.assertNotIn(tag, {"iframe", "embed", "source"}, f"<{tag}> is forbidden")
            for name in ("src", "data", "srcset", "poster", "action"):
                self.assertFalse(is_external(attrs.get(name, "")), f"<{tag} {name}> is external")
            if tag == "link":
                self.assertFalse(is_external(attrs.get("href", "")), "<link href> is external")
        css = read("style.css")
        self.assertNotIn("@import", css)
        self.assertIsNone(re.search(r"url\(\s*[\"']?\s*(?:[a-z][a-z0-9+.-]*:|//)", css, re.I))
    def test_internal_links_are_relative(self) -> None:
        parsed = page()
        for href in parsed.hrefs:
            self.assertFalse(href.startswith("/"), f"root-relative or protocol-relative link {href}")
            if re.match(r"[a-z][a-z0-9+.-]*:", href, re.I):
                self.assertIn(href, ALLOWED_ABSOLUTE)
        self.assertIn("bench/", parsed.hrefs)
        for href in API_HREFS:
            self.assertIn(href, parsed.hrefs)
    def test_five_release_links_and_latest_link_present(self) -> None:
        hrefs = page().hrefs
        for asset in RELEASE_ASSETS:
            self.assertIn(f"{RELEASE_BASE}/download/{asset}", hrefs)
        self.assertIn(RELEASE_BASE, hrefs)
    def test_repo_file_links_are_absolute_blob_urls(self) -> None:
        hrefs = page().hrefs
        for name in ("ROADMAP.md", "FLUX_FULL_UPDATED_SPEC_V16.md", "LICENSE"):
            self.assertIn(f"https://github.com/ckir/flux/blob/main/{name}", hrefs)
    def test_quickstart_commands_present(self) -> None:
        text = page().text
        for command in (
            "./flux copy ./photos /mnt/backup",
            "./flux copy --resume ./photos /mnt/backup",
            ".\\flux.exe copy .\\photos D:\\backup",
        ):
            self.assertIn(command, text)
    def test_chart_has_object_fallback(self) -> None:
        parsed = page()
        attrs = parsed.object_attrs
        self.assertIsNotNone(attrs, "no <object>")
        self.assertEqual(attrs.get("data"), "bench/latest.svg")
        self.assertEqual(attrs.get("type"), "image/svg+xml")
        self.assertEqual(attrs.get("width"), "720")
        self.assertGreaterEqual(int(attrs.get("height", "0")), 560)
        self.assertGreaterEqual(len(parsed.fallback.strip()), 20)
    def test_correctness_line_precedes_chart(self) -> None:
        events = page().events
        seen = ""
        phrase_at = chart_at = None
        for index, (kind, value) in enumerate(events):
            if kind == "text":
                seen += value
                if phrase_at is None and "correctness before performance" in seen.lower():
                    phrase_at = index
            elif value == "object" and chart_at is None:
                chart_at = index
        self.assertIsNotNone(phrase_at)
        self.assertIsNotNone(chart_at)
        self.assertLess(phrase_at, chart_at)
    def test_no_unconditional_or_numeric_speed_claims(self) -> None:
        text = page().text
        lowered = text.lower()
        for banned in ("outperform", "fastest", "always faster", "beats cp", "beats rsync"):
            self.assertNotIn(banned, lowered)
        self.assertIsNone(re.search(r"\d+(\.\d+)?\s*[x×]", text))
        self.assertIsNone(re.search(r"\d+%\s*faster", text, re.I))
    def test_licence_and_status_stated(self) -> None:
        text = " ".join(page().text.split())
        self.assertIn("PolyForm Noncommercial", text)
        self.assertIn("early implementation", text.lower())
        self.assertRegex(text, r"(?i)commercial use is (?:not permitted|not allowed)")
    def test_install_lines_and_unsigned_note_present(self) -> None:
        text = page().text
        self.assertIn("tar -xzf", text)
        self.assertTrue("unzip" in text or "Expand-Archive" in text)
        self.assertIn("Gatekeeper", text)
        self.assertIn("SmartScreen", text)
    def test_canvas_script_honours_reduced_motion_and_hidden_tab(self) -> None:
        js = strip_js_comments(read("flux-field.js"))
        self.assertRegex(js, r"matchMedia\([^)]*prefers-reduced-motion")
        for needed in ("visibilitychange", "document.hidden", "cancelAnimationFrame"):
            self.assertIn(needed, js)

if __name__ == "__main__":
    unittest.main()
