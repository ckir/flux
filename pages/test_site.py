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
        self.rows: list[list[dict]] = []  # every <tr>: its cells as {"tag", "attrs", "text", "hrefs"}
        self._row: list[dict] | None = None
        self._cell: dict | None = None
        self.feed(source)
        self.close()

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attributes = {name: value or "" for name, value in attrs}
        self.tags.append((tag, attributes))
        self.events.append(("tag", tag))
        if tag == "a" and "href" in attributes:
            self.hrefs.append(attributes["href"])
            if self._cell is not None:
                self._cell["hrefs"].append(attributes["href"])
        if tag == "tr":
            self._row = []
        elif tag in ("th", "td") and self._row is not None:
            self._cell = {"tag": tag, "attrs": attributes, "text": "", "hrefs": []}
        if tag in HIDDEN_TEXT_TAGS:
            self._hidden += 1
        if tag == "object" and self.object_attrs is None:
            self.object_attrs = attributes
            self._object_depth = 1
        elif self._object_depth and tag == "code":
            self._code_depth += 1

    def handle_endtag(self, tag: str) -> None:
        if tag in ("th", "td") and self._cell is not None and self._row is not None:
            self._row.append(self._cell)
            self._cell = None
        elif tag == "tr" and self._row is not None:
            self.rows.append(self._row)
            self._row = None
        if tag in HIDDEN_TEXT_TAGS and self._hidden:
            self._hidden -= 1
        if self._object_depth:
            if tag == "object":
                self._object_depth = 0
            elif tag == "code" and self._code_depth:
                self._code_depth -= 1

    def handle_data(self, data: str) -> None:
        if self._cell is not None:
            self._cell["text"] += data
        if self._hidden:
            return
        self.events.append(("text", data))
        if self._object_depth and not self._code_depth:
            self.fallback += data

    @property
    def text(self) -> str:
        return "".join(value for kind, value in self.events if kind == "text")


BLOCK_TAGS = {
    "p", "li", "ul", "ol", "h1", "h2", "h3", "figure", "figcaption", "div", "section", "header",
    "main", "footer", "table", "tr", "th", "td", "pre", "object", "canvas", "body",
}
SPEED_WORDS = re.compile(r"\b(?:faster|quicker|speed|throughput|outperforms?|beats?)\b", re.I)
# every visible sentence that talks about speed must be one of these, word for word
ALLOWED_SPEED_SENTENCES = {
    "Each figure is Flux's median time divided by another copier's, measured in the same GitHub "
    "Actions job: below 1.0, Flux is faster",
    "High throughput, but correctness before performance",
}


def sentences(parsed: "Page") -> list[str]:
    """Visible text split into blocks at block-level tags, then into sentences, whitespace collapsed."""
    blocks: list[str] = [""]
    for kind, value in parsed.events:
        if kind == "tag" and value in BLOCK_TAGS:
            blocks.append("")
        elif kind == "text":
            blocks[-1] += value
    found: list[str] = []
    for block in blocks:
        collapsed = " ".join(block.split())
        for sentence in re.split(r"(?<=[.!?])\s+", collapsed):
            sentence = sentence.strip().rstrip(".!?").strip()
            if sentence:
                found.append(sentence)
    return found


def page() -> Page:
    return Page(read("index.html"))


def strip_js_comments(js: str) -> str:
    js = re.sub(r"/\*.*?\*/", "", js, flags=re.S)
    return re.sub(r"(?m)//.*$", "", js)


URL_ATTRS = ("src", "href", "xlink:href", "data", "srcset", "poster", "action")


def chart_rules(css: str) -> list[str]:
    """Bodies of every CSS rule whose selector targets the object inside .chart-scroll."""
    return [
        body
        for selector, body in re.findall(r"([^{}]+)\{([^{}]*)\}", css)
        if re.search(r"\.chart-scroll\s+object\b", selector)
    ]


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
            self.assertNotIn(tag, {"iframe", "embed", "source", "base"}, f"<{tag}> is forbidden")
            for name in URL_ATTRS:
                if tag == "a" and name == "href":
                    continue  # anchors are checked against the allow-list in the links test
                self.assertFalse(is_external(attrs.get(name, "")), f"<{tag} {name}> is external")
            self.assertIsNone(
                re.search(r"url\(\s*[\"']?\s*(?:[a-z][a-z0-9+.-]*:|//)", attrs.get("style", ""), re.I),
                f"<{tag} style> loads an external url",
            )
        css = read("style.css")
        self.assertNotIn("@import", css)
        self.assertIsNone(re.search(r"url\(\s*[\"']?\s*(?:[a-z][a-z0-9+.-]*:|//)", css, re.I))
    def test_internal_links_are_relative(self) -> None:
        parsed = page()
        for tag, attrs in parsed.tags:
            for name in ("src", "href", "xlink:href"):
                value = attrs.get(name, "").strip()
                self.assertFalse(value.startswith("/"), f"<{tag} {name}={value}> is root-relative")
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
            ".\\flux.exe copy --resume .\\photos D:\\backup",
        ):
            self.assertIn(command, text)
        tags = page().tags
        for cls in ("code", "table-scroll"):
            scrollers = [a for t, a in tags if cls in a.get("class", "").split()]
            self.assertTrue(scrollers, cls)
            for attrs in scrollers:
                self.assertEqual(attrs.get("tabindex"), "0", f"{cls} is not keyboard-scrollable")
                self.assertTrue(attrs.get("aria-label"), f"{cls} has no aria-label")
    def test_chart_has_object_fallback(self) -> None:
        parsed = page()
        attrs = parsed.object_attrs
        self.assertIsNotNone(attrs, "no <object>")
        self.assertEqual(attrs.get("data"), "bench/latest.svg")
        self.assertEqual(attrs.get("type"), "image/svg+xml")
        self.assertEqual(attrs.get("width"), "720")
        self.assertGreaterEqual(int(attrs.get("height", "0")), 560)
        self.assertGreaterEqual(len(parsed.fallback.strip()), 20)
        self.assertTrue(attrs.get("aria-label"), "the <object> has no accessible name")
        scrollers = [a for _, a in parsed.tags if "chart-scroll" in a.get("class", "").split()]
        self.assertEqual(len(scrollers), 1)
        self.assertEqual(scrollers[0].get("tabindex"), "0", "chart scroller is not keyboard-focusable")
        self.assertTrue(scrollers[0].get("aria-label"))
        rules = chart_rules(read("style.css"))
        self.assertTrue(rules, "no .chart-scroll object rule in style.css")
        for rule in rules:
            for height in re.findall(r"(?<![-\w])height\s*:\s*(\d+)px", rule):
                self.assertGreaterEqual(int(height), 560)
            self.assertNotRegex(rule, r"(?<![-\w])padding")
            for width in re.findall(r"(?<![-\w])width\s*:\s*([^;}\s]+)", rule):
                self.assertEqual(width, "720px")
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
        for sentence in sentences(page()):
            if SPEED_WORDS.search(sentence):
                self.assertIn(sentence, ALLOWED_SPEED_SENTENCES, "unexpected speed sentence")
    def test_licence_and_status_stated(self) -> None:
        text = " ".join(page().text.split())
        self.assertIn("PolyForm Noncommercial", text)
        self.assertIn("early implementation", text.lower())
        self.assertRegex(
            text,
            r"(?i)commercial use is (?:not permitted|not allowed)"
            r"|does not (?:permit|allow) commercial use",
        )
        found = sentences(page())
        unimplemented = [s for s in found if "do not exist yet" in s]
        self.assertEqual(len(unimplemented), 1, unimplemented)
        self.assertIn("flux verify", unimplemented[0])
        self.assertIn("flux benchmark", unimplemented[0])
        for sentence in found:
            if re.search(r"commercial use", sentence, re.I):
                self.assertRegex(sentence, r"(?i)\bnot\b|\bno\b|\bnon", "commercial use must be negated")
        self.assertIn("robocopy", text)
        self.assertIn("only once its ratio is steady", text)
    def test_install_lines_and_unsigned_note_present(self) -> None:
        text = page().text
        self.assertIn("tar -xzf", text)
        self.assertTrue("unzip" in text or "Expand-Archive" in text)
        self.assertIn("Gatekeeper", text)
        self.assertIn("SmartScreen", text)
    def test_canvas_script_honours_reduced_motion_and_hidden_tab(self) -> None:
        js = strip_js_comments(read("flux-field.js"))
        self.assertRegex(js, r"matchMedia\([^)]*prefers-reduced-motion")
        self.assertRegex(js, r"addEventListener\(\s*[\"']visibilitychange[\"']")
        self.assertRegex(js, r"document\.hidden\b(?!\s*=[^=])")
        self.assertRegex(js, r"cancelAnimationFrame\(")

    def test_canvas_script_makes_no_network_requests(self) -> None:
        js = strip_js_comments(read("flux-field.js"))
        for banned in ("fetch(", "XMLHttpRequest", "WebSocket", "sendBeacon", "EventSource", "importScripts", "import("):
            self.assertNotIn(banned, js, f"the canvas script uses {banned}")
        self.assertNotRegex(js, r"(?i)https?:|//", "the canvas script names a URL")

    def test_download_rows_pair_each_label_with_its_target(self) -> None:
        expected = {
            ("Linux", "x86_64"): "flux-x86_64-unknown-linux-gnu.tar.gz",
            ("Linux", "aarch64"): "flux-aarch64-unknown-linux-gnu.tar.gz",
            ("macOS", "Intel (x86_64)"): "flux-x86_64-apple-darwin.tar.gz",
            ("macOS", "Apple silicon (aarch64)"): "flux-aarch64-apple-darwin.tar.gz",
            ("Windows", "x86_64"): "flux-x86_64-pc-windows-msvc.zip",
        }
        body_rows = [r for r in page().rows if r and r[0]["tag"] == "th" and r[0]["attrs"].get("scope") == "row"]
        self.assertEqual(len(body_rows), 5)
        found = {}
        for row in body_rows:
            self.assertEqual(len(row), 3)
            self.assertEqual(len(row[-1]["hrefs"]), 1)
            found[(row[0]["text"].strip(), row[1]["text"].strip())] = row[-1]["hrefs"][0]
        self.assertEqual(len(found), 5)
        self.assertEqual(found, {key: f"{RELEASE_BASE}/download/{asset}" for key, asset in expected.items()})

    def test_primary_button_fill_never_uses_violet(self) -> None:
        # dark label text on violet is 3.0:1, below WCAG AA; violet may appear only in the glow
        classes = {
            cls
            for tag, attrs in page().tags
            if tag == "a" and "button" in attrs.get("class", "").split()
            for cls in attrs["class"].split()
        }
        self.assertIn("button", classes)
        fills = []
        for selector, body in re.findall(r"([^{}]+)\{([^{}]*)\}", read("style.css")):
            if not any(re.search(rf"\.{re.escape(cls)}(?![\w-])", selector) for cls in classes):
                continue
            for value in re.findall(r"(?<![-\w])background[\w-]*\s*:\s*([^;}]+)", body):
                fills.append(value)
                self.assertNotIn("var(--violet)", value)
                self.assertNotIn("#7000ff", value.lower())
        self.assertTrue(fills, "no background rule for the primary button")
        self.assertTrue(
            any("var(--cyan)" in v and "var(--blue)" in v for v in fills), f"no cyan-to-blue fill in {fills}"
        )
        for value in fills:
            self.assertIn("var(--cyan)", value)
            self.assertIn("var(--blue)", value)

    def test_ids_are_unique(self) -> None:
        ids = [attrs["id"] for _, attrs in page().tags if "id" in attrs]
        self.assertGreaterEqual(len(ids), 10, "too few ids found; did the parse come back empty?")
        duplicated = sorted({value for value in ids if ids.count(value) > 1})
        self.assertEqual(duplicated, [], f"duplicate ids: {duplicated}")

    def test_in_page_references_resolve(self) -> None:
        parsed = page()
        ids = {attrs["id"] for _, attrs in parsed.tags if "id" in attrs}
        references: list[tuple[str, str]] = []  # (kind, target)
        for tag, attrs in parsed.tags:
            if tag == "a" and attrs.get("href", "").startswith("#") and len(attrs["href"]) > 1:
                references.append(("href", attrs["href"][1:]))
            for name in ("aria-labelledby", "aria-describedby", "aria-controls"):
                references.extend((name, token) for token in attrs.get(name, "").split())
            if tag == "label" and attrs.get("for"):
                references.append(("for", attrs["for"]))
        self.assertTrue(any(kind == "href" for kind, _ in references), "no in-page anchor found")
        labelled = [target for kind, target in references if kind == "aria-labelledby"]
        self.assertGreaterEqual(len(labelled), 5, "too few aria-labelledby references found")
        self.assertIn("main", ids, "the skip link target #main does not exist")
        dangling = sorted({f"{kind}={target}" for kind, target in references if target not in ids})
        self.assertEqual(dangling, [], f"references to ids that do not exist: {dangling}")

    def test_heading_levels_do_not_skip(self) -> None:
        levels = [int(tag[1]) for tag, _ in page().tags if re.fullmatch(r"h[1-6]", tag)]
        self.assertGreaterEqual(len(levels), 8, "too few headings found")
        self.assertEqual(levels[0], 1, f"the first heading is h{levels[0]}, not h1")
        self.assertEqual(levels.count(1), 1, f"expected exactly one h1, found {levels.count(1)}")
        for previous, current in zip(levels, levels[1:]):
            self.assertLessEqual(current, previous + 1, f"h{current} follows h{previous} in {levels}")


if __name__ == "__main__":
    unittest.main()
