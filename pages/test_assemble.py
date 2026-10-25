# Copyright (c) 2026 Costas Kirgoussios
#
# This software is licensed under the PolyForm Noncommercial License 1.0.0.
# You may obtain a copy of the License at:
# https://polyformproject.org/licenses/noncommercial/1.0.0/

"""Tests for assemble.py."""

from __future__ import annotations

import contextlib
import io
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from assemble import AssembleError, assemble, list_files, main, rel_target, stub_html, verify  # noqa: E402

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

    def assert_failed_before_move(self, before, work=None):
        self.assertEqual(list_files(self.doc), before)
        self.assertFalse((self.doc / "api").exists())
        self.assertFalse(work.exists() if work is not None else self.work.exists())

    def test_moves_every_file_and_stubs_every_html_path(self):
        before = list_files(self.doc)
        contents = {p: (self.doc / p).read_bytes() for p in before}
        self.run_assemble()
        for p in before:
            self.assertTrue((self.doc / "api" / p).is_file(), p)
            self.assertEqual((self.doc / "api" / p).read_bytes(), contents[p], p)
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
        self.assert_failed_before_move(before, self.doc / "work")

    def test_root_index_html_in_doc_collides_with_site_index(self):
        put(self.doc / "index.html", "<html>rustdoc root</html>")
        before = list_files(self.doc)
        with self.assertRaises(AssembleError):
            self.run_assemble()
        self.assert_failed_before_move(before)

    def test_main_reports_assemble_error_on_stderr_and_returns_1(self):
        argv = ["assemble", "--doc", str(self.doc), "--site", str(self.site),
                "--template", str(self.template), "--work", str(self.work)]
        (self.doc / "api").mkdir()
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertEqual(main(argv), 1)
        self.assertTrue(err.getvalue().startswith("error: "), err.getvalue())
        (self.doc / "api").rmdir()
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(main(argv), 0)
        self.assertTrue((self.doc / "api" / "index.html").is_file())


class VerifyTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        root = Path(self._tmp.name)
        self.doc = make_doc(root)
        self.site = make_site(root)
        template = root / "api-index.html"
        template.write_bytes(TEMPLATE_TEXT)
        self.work = root / "work"
        assemble(self.doc, self.site, template, self.work)

    def problems(self):
        return verify(self.doc, self.site, self.work)

    def assert_exactly(self, *needles):
        found = self.problems()
        self.assertEqual(len(found), len(needles), found)
        for needle in needles:
            self.assertEqual(sum(needle in p for p in found), 1, (needle, found))

    def test_clean_tree_passes(self):
        self.assertEqual(self.problems(), [])

    def test_verify_passes_without_bench(self):
        self.assertFalse((self.doc / "bench").exists())
        self.assertFalse((self.doc / "api" / "bench").exists())
        self.assertEqual(self.problems(), [])

    def test_missing_moved_file_is_named(self):
        (self.doc / "api" / "search.index" / "s.js").unlink()
        self.assert_exactly("missing: api/search.index/s.js")

    def test_missing_stub_is_named(self):
        (self.doc / "help.html").unlink()
        self.assert_exactly("bad stub: help.html")

    def test_overwritten_stub_is_named(self):
        (self.doc / "flux_core" / "index.html").write_text("not a stub", encoding="utf-8")
        self.assert_exactly("bad stub: flux_core/index.html")

    def test_missing_api_index_is_named(self):
        (self.doc / "api" / "index.html").unlink()
        self.assert_exactly("api/index.html")

    def test_missing_static_files_is_named(self):
        shutil.rmtree(self.doc / "api" / "static.files")
        found = self.problems()
        self.assertTrue(any("api/static.files" in p for p in found), found)
        self.assertFalse(any("landing page" in p or "bad stub" in p for p in found), found)

    def test_bench_under_api_is_named(self):
        (self.doc / "api" / "bench").mkdir()
        self.assert_exactly("api/bench")

    def test_changed_landing_page_is_named(self):
        (self.doc / "index.html").write_text("<html>other</html>", encoding="utf-8")
        self.assert_exactly("landing page")

    def test_missing_manifest_is_reported(self):
        (self.work / "manifest.json").unlink()
        found = self.problems()
        self.assertTrue(any("manifest" in p for p in found), found)
        self.assertFalse(any("landing page" in p or "bad stub" in p for p in found), found)

    def test_all_problems_are_collected_not_just_the_first(self):
        (self.doc / "api" / "search.index" / "s.js").unlink()
        (self.doc / "flux_core" / "struct.X.html").write_text("not a stub", encoding="utf-8")
        (self.doc / "index.html").write_text("<html>other</html>", encoding="utf-8")
        self.assert_exactly("missing: api/search.index/s.js", "bad stub: flux_core/struct.X.html", "landing page")

    def test_cli_exit_codes(self):
        argv = ["verify", "--doc", str(self.doc), "--site", str(self.site), "--work", str(self.work)]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(main(argv), 0)
        manifest = json.loads((self.work / "manifest.json").read_text(encoding="utf-8"))
        stubs = sum(1 for p in manifest if p.endswith(".html"))
        self.assertEqual(out.getvalue(), f"verify: ok ({len(manifest)} files, {stubs} stubs)\n")
        (self.doc / "help.html").unlink()
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertEqual(main(argv), 1)
        self.assertIn("bad stub: help.html", err.getvalue())


if __name__ == "__main__":
    unittest.main()
