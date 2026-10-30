# Copyright (c) 2026 Costas Kirgoussios
#
# This software is licensed under the PolyForm Noncommercial License 1.0.0.
# You may obtain a copy of the License at:
# https://polyformproject.org/licenses/noncommercial/1.0.0/

"""Build the source trees the phase 3 measurements run against."""

import argparse
import os
from pathlib import Path

KINDS = ("small", "flat", "links")


def _fill(index, size):
    unit = (index + 1).to_bytes(8, "little")
    return (unit * (size // 8 + 1))[:size]


def build(kind, dest, files, size, dirs):
    """Create a tree of `kind` under `dest` and return the number of files made."""
    if kind not in KINDS:
        raise ValueError(f"unknown kind: {kind!r}")
    if files < 0:
        raise ValueError("files must not be negative")
    if kind in ("small", "links"):
        if size < 0:
            raise ValueError("size must not be negative")
        if dirs < 1:
            raise ValueError("dirs must be at least 1")
    if kind == "links" and files % 2 != 0:
        raise ValueError("links needs an even number of files")
    dest = Path(dest)
    if dest.exists() and not dest.is_dir():
        raise FileExistsError(f"destination is not a directory: {dest}")
    if dest.exists() and any(dest.iterdir()):
        raise FileExistsError(f"destination is not empty: {dest}")
    dest.mkdir(parents=True, exist_ok=True)

    if kind == "flat":
        for i in range(files):
            (dest / f"f{i:06d}").write_bytes(b"")
        return files

    width = max(2, len(str(dirs - 1)))
    dir_paths = []
    for d in range(dirs):
        p = dest / f"d{d:0{width}d}"
        p.mkdir()
        dir_paths.append(p)

    def path_of(i):
        return dir_paths[i % dirs] / f"f{i:06d}"

    if kind == "small":
        for i in range(files):
            path_of(i).write_bytes(_fill(i, size))
    else:
        for k in range(files // 2):
            first = path_of(2 * k)
            first.write_bytes(_fill(2 * k, size))
            os.link(first, path_of(2 * k + 1))
    return files


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("kind", choices=KINDS)
    ap.add_argument("dest", type=Path)
    ap.add_argument("--files", type=int, default=10000)
    ap.add_argument("--bytes", type=int, default=4096, dest="size")
    ap.add_argument("--dirs", type=int, default=100)
    a = ap.parse_args(argv)
    print(build(a.kind, a.dest, a.files, a.size, a.dirs))


if __name__ == "__main__":
    main()
