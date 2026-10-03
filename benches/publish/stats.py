"""The arithmetic of a measurement: medians, the coefficient of variation, how many rounds, and in what order."""

from __future__ import annotations

import statistics
from typing import Sequence, TypeVar

T = TypeVar("T")


def median(xs: Sequence[float]) -> float:
    return statistics.median(xs)


def cv(xs: Sequence[float]) -> float:
    """The sample standard deviation over the mean. Needs at least two values."""
    if len(xs) < 2:
        raise ValueError("the coefficient of variation needs at least two values")
    return statistics.stdev(xs) / statistics.fmean(xs)


def round_count(n_tools: int) -> int:
    """Six, or the next multiple of `n_tools` above six, so each tool holds each position equally often."""
    return -(-6 // n_tools) * n_tools


def order(tools: Sequence[T], r: int) -> list[T]:
    """Round `r`'s order: the tool list rotated by `r`."""
    k = r % len(tools)
    return list(tools[k:]) + list(tools[:k])
