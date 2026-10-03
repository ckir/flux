"""The three benchmark cases (spec "Cases") and their generation from a fixed seed.

Contents are pseudo-random, so every run copies the same bytes and no file is compressible or has runs of zeros a
filesystem could store sparsely.
"""

from __future__ import annotations

import dataclasses
import random
import shutil
from pathlib import Path

KIB = 1024
MIB = 1024 * KIB
GIB = 1024 * MIB
CHUNK = MIB


@dataclasses.dataclass(frozen=True)
class Case:
    name: str
    files: tuple[tuple[str, int], ...]

    @property
    def total(self) -> int:
        return sum(size for _, size in self.files)


def _shrink(size: int, tiny: bool) -> int:
    return max(size // 1024, 1) if tiny else size


def _large(tiny: bool) -> Case:
    return Case("large", (("big.bin", _shrink(GIB, tiny)),))


def _small(tiny: bool) -> Case:
    folders, per = (5, 10) if tiny else (50, 100)
    return Case("small", tuple((f"d{d:02}/f{i:04}.bin", 4 * KIB) for d in range(folders) for i in range(per)))


def _mixed(tiny: bool) -> Case:
    sizes = [64 * MIB] * 2 + [16 * MIB] * 8 + [MIB] * 90 + [4 * KIB] * 200 + [846 * KIB] * 200
    files = []
    for i, size in enumerate(sizes):
        if tiny and i % 10:
            continue
        rel = f"a{i % 5}/b{(i // 5) % 5}/c{(i // 25) % 4}/file{i:03}.bin"
        files.append((rel, _shrink(size, tiny)))
    return Case("mixed", tuple(files))


def cases(tiny: bool = False) -> list[Case]:
    """The cases in the order they run: `large`, `small`, `mixed`. `tiny` is for the dry run only."""
    return [_large(tiny), _small(tiny), _mixed(tiny)]


def generate(case: Case, root: Path) -> Path:
    """Write `case` under `root/src-<name>`, replacing anything there, and return that directory."""
    src = root / f"src-{case.name}"
    if src.exists():
        shutil.rmtree(src)
    for rel, size in case.files:
        path = src / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        rng = random.Random(f"flux-bench/{case.name}/{rel}")
        with path.open("wb") as f:
            left = size
            while left:
                n = min(left, CHUNK)
                f.write(rng.randbytes(n))
                left -= n
    return src
