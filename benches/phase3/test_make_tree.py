# Copyright (c) 2026 Costas Kirgoussios
#
# This software is licensed under the PolyForm Noncommercial License 1.0.0.
# You may obtain a copy of the License at:
# https://polyformproject.org/licenses/noncommercial/1.0.0/

import os
import tempfile
import unittest
from pathlib import Path

import make_tree


def snapshot(root):
    out = {}
    for p in sorted(root.rglob("*")):
        out[str(p.relative_to(root))] = p.read_bytes() if p.is_file() else None
    return out


class MakeTreeTests(unittest.TestCase):
    def test_small_builds_files_across_dirs(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "tree"
            self.assertEqual(make_tree.build("small", d, 10, 4096, 2), 10)
            dirs = sorted(p for p in d.iterdir())
            self.assertEqual([p.name for p in dirs], ["d00", "d01"])
            for sub in dirs:
                files = list(sub.iterdir())
                self.assertEqual(len(files), 5)
                for f in files:
                    self.assertEqual(f.stat().st_size, 4096)

    def test_small_content_is_deterministic(self):
        with tempfile.TemporaryDirectory() as t:
            a = Path(t) / "a"
            b = Path(t) / "b"
            make_tree.build("small", a, 7, 100, 3)
            make_tree.build("small", b, 7, 100, 3)
            self.assertEqual(snapshot(a), snapshot(b))

    def test_flat_puts_everything_in_one_dir(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "tree"
            self.assertEqual(make_tree.build("flat", d, 6, 4096, 9), 6)
            entries = list(d.iterdir())
            self.assertEqual(len(entries), 6)
            for e in entries:
                self.assertTrue(e.is_file())
                self.assertEqual(e.stat().st_size, 0)

    def test_links_pairs_share_an_inode(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "tree"
            self.assertEqual(make_tree.build("links", d, 8, 64, 2), 8)
            files = [f for f in d.rglob("*") if f.is_file()]
            self.assertEqual(len(files), 8)
            for f in files:
                self.assertEqual(f.stat().st_nlink, 2)
            inodes = {os.stat(f).st_ino for f in files}
            self.assertEqual(len(inodes), 4)

    def test_links_rejects_an_odd_count(self):
        with tempfile.TemporaryDirectory() as t:
            with self.assertRaises(ValueError):
                make_tree.build("links", Path(t) / "tree", 7, 64, 2)

    def test_zero_files_builds_an_empty_tree(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "tree"
            self.assertEqual(make_tree.build("small", d, 0, 4096, 3), 0)
            self.assertEqual(sorted(p.name for p in d.iterdir()), ["d00", "d01", "d02"])
            for sub in d.iterdir():
                self.assertEqual(list(sub.iterdir()), [])

    def test_refuses_a_non_empty_destination(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t)
            (d / "x").write_bytes(b"x")
            with self.assertRaises(FileExistsError):
                make_tree.build("small", d, 1, 1, 1)

    def test_unknown_kind(self):
        with tempfile.TemporaryDirectory() as t:
            with self.assertRaises(ValueError):
                make_tree.build("bogus", Path(t) / "tree", 1, 1, 1)


if __name__ == "__main__":
    unittest.main()
