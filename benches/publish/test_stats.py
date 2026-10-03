"""Tests for stats.py."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import stats  # noqa: E402


class StatsTests(unittest.TestCase):
    def test_round_count_is_six_or_the_next_multiple_of_the_tool_count(self) -> None:
        self.assertEqual([stats.round_count(n) for n in range(1, 8)], [6, 6, 6, 8, 10, 6, 7])

    def test_every_tool_holds_every_position_equally_often(self) -> None:
        for n in (2, 3, 4):
            tools = [f"t{i}" for i in range(n)]
            rounds = stats.round_count(n)
            for position in range(n):
                firsts = [stats.order(tools, r)[position] for r in range(rounds)]
                for t in tools:
                    self.assertEqual(firsts.count(t), rounds // n, (n, position, t))

    def test_order_keeps_every_tool_once(self) -> None:
        self.assertEqual(stats.order(["a", "b", "c"], 1), ["b", "c", "a"])
        self.assertEqual(sorted(stats.order(["a", "b", "c"], 5)), ["a", "b", "c"])

    def test_median_and_cv(self) -> None:
        self.assertEqual(stats.median([3.0, 1.0, 2.0]), 2.0)
        self.assertEqual(stats.median([1.0, 2.0, 3.0, 4.0]), 2.5)
        self.assertAlmostEqual(stats.cv([1.0, 1.0, 1.0]), 0.0)
        self.assertAlmostEqual(stats.cv([0.9, 1.1]), 0.1414213562, places=6)
        with self.assertRaises(ValueError):
            stats.cv([1.0])


if __name__ == "__main__":
    unittest.main()
