"""Tests of the model workflow's plan step (.github/workflows/model.yml). Standard library only."""

from __future__ import annotations

import re
import unittest
from pathlib import Path

WORKFLOW = Path(__file__).resolve().parent.parent.parent / ".github" / "workflows" / "model.yml"


class ChangePatternTests(unittest.TestCase):
    """The workflow runs the scenarios only when a changed path matches its plan step's pattern.

    A path that should match but does not makes CI skip every scenario while the model gate still
    passes, so both directions are pinned here.
    """

    def setUp(self) -> None:
        found = re.findall(r"^\s*pattern='([^']*)'$", WORKFLOW.read_text(encoding="utf-8"), re.M)
        self.assertEqual(len(found), 1, "model.yml must set pattern= exactly once")
        # grep -E and Python's re agree on everything this pattern uses: ^ $ ( | ) [^/] * and \.
        self.pattern = re.compile(found[0])

    def test_watched_paths_match(self) -> None:
        for path in ("models/lockproto/run.py", "models/lockproto/configs/selftest-check.cfg",
                     "FLUX_FULL_UPDATED_SPEC_V16.md", "FLUX_FULL_UPDATED_SPEC_V17.md",
                     "crates/flux-platform/tests/fs_semantics.rs", "justfile", ".github/workflows/model.yml"):
            with self.subTest(path=path):
                self.assertIsNotNone(self.pattern.search(path))

    def test_near_misses_do_not_match(self) -> None:
        for path in ("docs/models/notes.md", "FLUX_FULL_UPDATED_SPEC_V16.md.bak", "docs/FLUX_FULL_UPDATED_SPEC_V16.md",
                     "FLUX_FULL_UPDATED_SPEC_V16/notes.md", "crates/flux-platform/tests/fs_semantics.rs.orig",
                     "crates/flux-platform/src/lib.rs", "justfile.bak", "crates/justfile",
                     ".github/workflows/ci.yml", ".github/workflows/model.yml.disabled"):
            with self.subTest(path=path):
                self.assertIsNone(self.pattern.search(path))


if __name__ == "__main__":
    unittest.main()
