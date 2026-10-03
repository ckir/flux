"""Tests for cases.py."""

from __future__ import annotations

import sys
import tempfile
import unittest
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import cases  # noqa: E402

MIB = 1024 * 1024


class CasesTests(unittest.TestCase):
    def test_the_full_cases_match_the_spec(self) -> None:
        large, small, mixed = cases.cases()
        self.assertEqual((large.name, small.name, mixed.name), ("large", "small", "mixed"))
        self.assertEqual(large.files, (("big.bin", 1024 * MIB),))
        self.assertEqual(len(small.files), 5000)
        self.assertEqual({size for _, size in small.files}, {4096})
        self.assertEqual(len({rel.split("/")[0] for rel, _ in small.files}), 50)
        self.assertEqual(len(mixed.files), 500)
        self.assertEqual(min(s for _, s in mixed.files), 4096)
        self.assertEqual(max(s for _, s in mixed.files), 64 * MIB)
        self.assertLess(abs(mixed.total - 512 * MIB), MIB // 10)
        self.assertEqual({rel.count("/") for rel, _ in mixed.files}, {3})

    def test_tiny_keeps_the_layout_and_shrinks_it(self) -> None:
        large, small, mixed = cases.cases(tiny=True)
        self.assertEqual(large.files, (("big.bin", MIB),))
        self.assertEqual(len(small.files), 50)
        self.assertEqual(len(mixed.files), 50)
        self.assertTrue(all(size >= 1 for _, size in mixed.files))

    def test_generation_is_deterministic_and_incompressible(self) -> None:
        _, small, mixed = cases.cases(tiny=True)
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            first = cases.generate(mixed, Path(a))
            second = cases.generate(mixed, Path(b))
            for rel, size in mixed.files:
                one = (first / rel).read_bytes()
                self.assertEqual(len(one), size, rel)
                self.assertEqual(one, (second / rel).read_bytes(), rel)
            sample = (cases.generate(small, Path(a)) / small.files[0][0]).read_bytes()
            self.assertGreater(len(zlib.compress(sample, 9)), len(sample) * 0.99)
            self.assertNotIn(b"\0" * 64, sample)

    def test_generating_again_replaces_the_tree(self) -> None:
        large, _, _ = cases.cases(tiny=True)
        with tempfile.TemporaryDirectory() as d:
            src = cases.generate(large, Path(d))
            (src / "stray").write_bytes(b"x")
            src = cases.generate(large, Path(d))
            self.assertEqual(sorted(p.name for p in src.iterdir()), ["big.bin"])


if __name__ == "__main__":
    unittest.main()
