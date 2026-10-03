"""Tests for store.py."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import store  # noqa: E402


def run(commit: str, os_: str, run_id: str, image: str = "img1") -> dict:
    return {"schema": 1, "commit": commit, "os": os_, "run": run_id, "image": {"os": "x", "version": image}}


class StoreTests(unittest.TestCase):
    def test_a_missing_file_is_an_empty_store_and_an_unknown_schema_stops(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "data.json"
            self.assertEqual(store.load(path), {"schema": 1, "runs": []})
            path.write_text(json.dumps({"schema": 2, "runs": []}))
            with self.assertRaises(store.SchemaError):
                store.load(path)

    def test_runs_are_appended_never_replaced(self) -> None:
        data = store.load(Path("absent.json"))
        store.add(data, run("c1", "linux", "10.1"))
        store.add(data, run("c1", "linux", "10.2"))
        self.assertEqual([r["run"] for r in data["runs"]], ["10.1", "10.2"])
        with self.assertRaises(store.SchemaError):
            store.add(data, {**run("c2", "linux", "11.1"), "schema": 9})

    def test_the_plotted_run_is_the_highest_run_per_commit_os_and_image(self) -> None:
        data = {"schema": 1, "runs": [
            run("c1", "linux", "10.2"),
            run("c1", "linux", "9.1"),
            run("c1", "linux", "10.10"),
            run("c1", "linux", "12.1", image="img2"),
            run("c1", "macos", "10.1"),
        ]}
        got = [(r["os"], r["run"], r["image"]["version"]) for r in store.plotted(data)]
        # In run order: 10.1 (run 10, attempt 1) before 10.10 (attempt 10) before 12.1.
        self.assertEqual(got, [("macos", "10.1", "img1"), ("linux", "10.10", "img1"), ("linux", "12.1", "img2")])
        latest = store.latest_per_os(data)
        self.assertEqual({k: v["run"] for k, v in latest.items()}, {"linux": "12.1", "macos": "10.1"})

    def test_the_latest_run_is_the_highest_not_the_last_listed(self) -> None:
        # Test audit G2: a re-run of an older commit can be appended after a newer run.
        data = {"schema": 1, "runs": [run("c2", "linux", "12.1"), run("c1", "linux", "10.1")]}
        self.assertEqual(store.latest_per_os(data)["linux"]["run"], "12.1")

    def test_a_file_without_a_schema_stops(self) -> None:
        # Test audit G6.
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "data.json"
            path.write_text(json.dumps({"runs": []}))
            with self.assertRaises(store.SchemaError):
                store.load(path)

    def test_saving_and_loading_round_trips(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "data.json"
            data = {"schema": 1, "runs": [run("c1", "linux", "1.1")]}
            store.save(path, data)
            self.assertEqual(store.load(path), data)


if __name__ == "__main__":
    unittest.main()
