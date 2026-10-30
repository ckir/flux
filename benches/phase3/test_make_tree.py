# Copyright (c) 2026 Costas Kirgoussios
#
# This software is licensed under the PolyForm Noncommercial License 1.0.0.
# You may obtain a copy of the License at:
# https://polyformproject.org/licenses/noncommercial/1.0.0/

import contextlib
import io
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

    def test_negative_files_is_refused(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "tree"
            with self.assertRaises(ValueError):
                make_tree.build("small", d, -1, 4096, 2)
            self.assertFalse(d.exists())

    def test_negative_size_is_refused_for_small_and_links_only(self):
        with tempfile.TemporaryDirectory() as t:
            for kind in ("small", "links"):
                d = Path(t) / kind
                with self.assertRaises(ValueError):
                    make_tree.build(kind, d, 4, -1, 2)
                self.assertFalse(d.exists())
            self.assertEqual(make_tree.build("flat", Path(t) / "flat", 3, -1, 1), 3)

    def test_zero_dirs_is_refused_for_small_and_links_only(self):
        with tempfile.TemporaryDirectory() as t:
            for kind in ("small", "links"):
                d = Path(t) / kind
                with self.assertRaises(ValueError):
                    make_tree.build(kind, d, 4, 4096, 0)
                self.assertFalse(d.exists())
            self.assertEqual(make_tree.build("flat", Path(t) / "flat", 3, 0, 0), 3)

    def test_a_regular_file_as_destination_is_refused(self):
        with tempfile.TemporaryDirectory() as t:
            f = Path(t) / "afile"
            f.write_bytes(b"keep")
            with self.assertRaises(FileExistsError):
                make_tree.build("small", f, 1, 1, 1)
            self.assertEqual(f.read_bytes(), b"keep")

    def test_small_places_zero_padded_files_round_robin(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "tree"
            make_tree.build("small", d, 5, 1, 2)
            self.assertEqual(
                sorted(p.name for p in (d / "d00").iterdir()),
                ["f000000", "f000002", "f000004"],
            )
            self.assertEqual(
                sorted(p.name for p in (d / "d01").iterdir()),
                ["f000001", "f000003"],
            )
            d2 = Path(t) / "flat"
            make_tree.build("flat", d2, 2, 1, 1)
            self.assertEqual(
                sorted(p.name for p in d2.iterdir()), ["f000000", "f000001"]
            )

    def test_small_fill_is_the_little_endian_index_pattern(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "tree"
            make_tree.build("small", d, 2, 10, 1)
            a = (d / "d00" / "f000000").read_bytes()
            b = (d / "d00" / "f000001").read_bytes()
            self.assertEqual(a, (1).to_bytes(8, "little") + b"\x01\x00")
            self.assertEqual(b, (2).to_bytes(8, "little") + b"\x02\x00")
            self.assertEqual(len(a), 10)
            self.assertEqual(len(b), 10)

    def test_directory_names_are_padded_to_the_widest_index(self):
        with tempfile.TemporaryDirectory() as t:
            a = Path(t) / "a"
            b = Path(t) / "b"
            make_tree.build("small", a, 0, 0, 100)
            self.assertEqual(
                sorted(p.name for p in a.iterdir()), [f"d{i:02d}" for i in range(100)]
            )
            make_tree.build("small", b, 0, 0, 101)
            self.assertEqual(
                sorted(p.name for p in b.iterdir()), [f"d{i:03d}" for i in range(101)]
            )

    def test_main_parses_the_cli_flags_and_prints_the_count(self):
        with tempfile.TemporaryDirectory() as t:
            dest = Path(t) / "tree"
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                make_tree.main(
                    ["small", str(dest), "--files", "4", "--bytes", "24", "--dirs", "2"]
                )
            self.assertEqual(out.getvalue(), "4\n")
            self.assertEqual(sorted(p.name for p in dest.iterdir()), ["d00", "d01"])
            files = [f for f in dest.rglob("*") if f.is_file()]
            self.assertEqual(len(files), 4)
            for f in files:
                self.assertEqual(f.stat().st_size, 24)

    def test_links_files_carry_the_requested_size_and_fill(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "tree"
            make_tree.build("links", d, 4, 20, 2)
            for rel, idx in (
                ("d00/f000000", 0),
                ("d01/f000001", 0),
                ("d00/f000002", 2),
                ("d01/f000003", 2),
            ):
                data = (d / rel).read_bytes()
                self.assertEqual(data, make_tree._fill(idx, 20))
                self.assertEqual(len(data), 20)

    def test_links_pair_consecutive_files_across_directories(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "tree"
            make_tree.build("links", d, 4, 64, 2)
            i0 = os.stat(d / "d00" / "f000000").st_ino
            i1 = os.stat(d / "d01" / "f000001").st_ino
            i2 = os.stat(d / "d00" / "f000002").st_ino
            i3 = os.stat(d / "d01" / "f000003").st_ino
            self.assertEqual(i0, i1)
            self.assertEqual(i2, i3)
            self.assertNotEqual(i0, i2)
            self.assertEqual(
                sorted(p.name for p in (d / "d00").iterdir()), ["f000000", "f000002"]
            )
            self.assertEqual(
                sorted(p.name for p in (d / "d01").iterdir()), ["f000001", "f000003"]
            )

    def test_zero_size_is_accepted_for_small_and_links(self):
        with tempfile.TemporaryDirectory() as t:
            for kind in ("small", "links"):
                d = Path(t) / kind
                self.assertEqual(make_tree.build(kind, d, 2, 0, 2), 2)
                files = [f for f in d.rglob("*") if f.is_file()]
                self.assertEqual(len(files), 2)
                for f in files:
                    self.assertEqual(f.stat().st_size, 0)

    def test_accepts_an_existing_empty_destination(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "tree"
            d.mkdir()
            self.assertEqual(make_tree.build("small", d, 2, 16, 1), 2)
            self.assertEqual(len(list((d / "d00").iterdir())), 2)


if __name__ == "__main__":
    unittest.main()
