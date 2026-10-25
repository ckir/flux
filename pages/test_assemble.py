# Copyright (c) 2026 Costas Kirgoussios
#
# This software is licensed under the PolyForm Noncommercial License 1.0.0.
# You may obtain a copy of the License at:
# https://polyformproject.org/licenses/noncommercial/1.0.0/

"""Tests for assemble.py."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from assemble import AssembleError, assemble, list_files, rel_target, stub_html  # noqa: E402

TEMPLATE_TEXT = (HERE / "api-index.html").read_bytes()


def put(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def make_doc(root: Path) -> Path:
    doc = root / "doc"
    put(doc / "help.html", "<html>help</html>")
    put(doc / "crates.js", "window.ALL_CRATES = [];")
    put(doc / ".lock", "")
    put(doc / "flux_core" / "index.html", "<html>core</html>")
    put(doc / "flux_core" / "struct.X.html", "<html>X</html>")
    put(doc / "static.files" / "a.css", "body{}")
    put(doc / "search.index" / "s.js", "var s;")
    put(doc / "src" / "flux_core" / "lib.rs.html", "<html>src</html>")
    return doc


def make_site(root: Path) -> Path:
    site = root / "site"
    put(site / "index.html", "<html>landing</html>")
    put(site / "style.css", "body{}")
    put(site / "flux-field.js", "var f;")
    return site


class StubTests(unittest.TestCase):
    def test_depth_relative_targets(self):
        self.assertEqual(rel_target("help.html"), "api/help.html")
        self.assertEqual(rel_target("flux_core/struct.X.html"), "../api/flux_core/struct.X.html")
        self.assertEqual(rel_target("src/flux_core/lib.rs.html"), "../../api/src/flux_core/lib.rs.html")

    def test_stub_carries_refresh_canonical_and_hash_preserving_script(self):
        s = stub_html("flux_core/struct.X.html")
        self.assertIn('<meta http-equiv="refresh" content="0; url=../api/flux_core/struct.X.html">', s)
        self.assertIn('<link rel="canonical" href="https://ckir.github.io/flux/api/flux_core/struct.X.html">', s)
        self.assertIn('location.replace("../api/flux_core/struct.X.html" + location.search + location.hash)', s)

    def test_stub_escapes_hostile_characters(self):  # defence in depth below the whitelist
        s = stub_html('a"b</script>.html')
        self.assertNotIn('</script>.html', s)
        self.assertNotIn('"b<', s)


class AssembleTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        self.doc = make_doc(self.root)
        self.site = make_site(self.root)
        self.template = self.root / "api-index.html"
        self.template.write_bytes(TEMPLATE_TEXT)
        self.work = self.root / "work"

    def run_assemble(self):
        assemble(self.doc, self.site, self.template, self.work)

    def assert_failed_before_move(self, before):
        self.assertEqual(list_files(self.doc), before)
        self.assertFalse((self.doc / "api").exists())

    def test_moves_every_file_and_stubs_every_html_path(self):
        before = list_files(self.doc)
        self.run_assemble()
        for p in before:
            self.assertTrue((self.doc / "api" / p).is_file(), p)
            if p.endswith(".html"):
                self.assertEqual((self.doc / p).read_text(encoding="utf-8"), stub_html(p), p)

    def test_root_is_landing_page_and_api_index_is_template(self):
        self.run_assemble()
        self.assertEqual((self.doc / "index.html").read_bytes(), (self.site / "index.html").read_bytes())
        self.assertEqual((self.doc / "api" / "index.html").read_bytes(), self.template.read_bytes())
        self.assertTrue((self.doc / "style.css").is_file())
        self.assertTrue((self.doc / "flux-field.js").is_file())

    def test_lock_file_stays_put_and_is_not_in_api(self):
        self.run_assemble()
        self.assertTrue((self.doc / ".lock").exists())
        self.assertFalse((self.doc / "api" / ".lock").exists())

    def test_non_html_files_get_no_stub(self):
        self.run_assemble()
        self.assertFalse((self.doc / "crates.js").exists())
        self.assertFalse((self.doc / "static.files").exists())
        self.assertFalse((self.doc / "search.index").exists())

    def test_manifest_is_written_outside_doc(self):
        before = list_files(self.doc)
        self.run_assemble()
        manifest = self.work / "manifest.json"
        self.assertEqual(json.loads(manifest.read_text(encoding="utf-8")), before)
        self.assertNotIn("manifest.json", list_files(self.doc))

    def test_refuses_when_api_exists(self):
        (self.doc / "api").mkdir()
        with self.assertRaises(AssembleError):
            self.run_assemble()
        self.assertFalse(self.work.exists())

    def test_site_name_collisions_fail_before_any_move(self):
        for name in ("help.html", "flux_core", "api", "bench"):
            with self.subTest(name=name):
                before = list_files(self.doc)
                target = self.site / name
                put(target, "x")
                try:
                    with self.assertRaises(AssembleError):
                        self.run_assemble()
                    self.assert_failed_before_move(before)
                finally:
                    target.unlink()

    def test_site_must_be_flat_and_have_index(self):
        before = list_files(self.doc)
        (self.site / "sub").mkdir()
        with self.assertRaises(AssembleError):
            self.run_assemble()
        self.assert_failed_before_move(before)
        (self.site / "sub").rmdir()
        (self.site / "index.html").unlink()
        with self.assertRaises(AssembleError):
            self.run_assemble()
        self.assert_failed_before_move(before)

    def test_unsafe_html_path_fails_before_any_move(self):
        put(self.doc / "we<ird>.html", "x")
        before = list_files(self.doc)
        with self.assertRaises(AssembleError):
            self.run_assemble()
        self.assert_failed_before_move(before)

    def test_work_inside_doc_is_refused(self):
        before = list_files(self.doc)
        with self.assertRaises(AssembleError):
            assemble(self.doc, self.site, self.template, self.doc / "work")
        self.assert_failed_before_move(before)


if __name__ == "__main__":
    unittest.main()
