"""Update a checkout of the data branch from measure.py's results (spec "Where the data lives").

    publish.py bench --data DIR --results DIR --calibrate-flag FILE
    publish.py calibrate --data DIR --results DIR

`push.sh` runs this inside its fetch, redo and push loop.
"""

from __future__ import annotations

import argparse
import datetime
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import stability  # noqa: E402
import store  # noqa: E402
import svg  # noqa: E402


def _results(directory: Path) -> list[dict]:
    return [json.loads(p.read_text("utf-8")) for p in sorted(directory.glob("result-*.json"))]


def _draw(data_dir: Path, data: dict, stab: dict) -> None:
    (data_dir / "latest.svg").write_text(svg.render(store.latest_per_os(data), stab), "utf-8")


def bench(data_dir: Path, results: list[dict], flag: Path) -> None:
    data = store.load(data_dir / "data.json")
    stab = stability.load(data_dir / "stability.json")
    for r in results:
        store.add(data, r)
    for p in stability.rolling(stab, data):
        print(f"{p}: demoted by the rolling check")
    store.save(data_dir / "data.json", data)
    stability.save(data_dir / "stability.json", stab)
    _draw(data_dir, data, stab)
    need = stability.unseen(stab, results)
    if need:
        flag.write_text("\n".join(need) + "\n", "utf-8")
        print(f"an image not yet calibrated on: {', '.join(need)}")


def calibrate(data_dir: Path, results: list[dict], date: str) -> None:
    data = store.load(data_dir / "data.json")
    stab = stability.load(data_dir / "stability.json")
    for line in stability.calibrate(stab, results, date):
        print(line)
    stability.save(data_dir / "stability.json", stab)
    _draw(data_dir, data, stab)


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("mode", choices=["bench", "calibrate"])
    p.add_argument("--data", required=True, type=Path)
    p.add_argument("--results", required=True, type=Path)
    p.add_argument("--calibrate-flag", type=Path)
    a = p.parse_args(argv)
    results = _results(a.results)
    if not results:
        print("no results: every measure job failed; nothing published")
        return 0
    if a.mode == "bench":
        if a.calibrate_flag is None:
            p.error("bench needs --calibrate-flag")
        bench(a.data, results, a.calibrate_flag)
    else:
        calibrate(a.data, results, datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
