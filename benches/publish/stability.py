"""stability.json on the data branch: which (os, case, comparator) pairs are steady enough to publish (spec "Noise")."""

from __future__ import annotations

import json
from pathlib import Path

import stats
import store

SCHEMA = 1
LIMIT = 0.03  # the coefficient of variation at or below which a pair is stable
POINTS = 10  # the ratios a calibration needs, and the points the rolling check judges


def empty() -> dict:
    return {"schema": SCHEMA, "pairs": {}}


def load(path: Path) -> dict:
    if not path.exists():
        return empty()
    stab = json.loads(path.read_text("utf-8"))
    if stab.get("schema") != SCHEMA:
        raise store.SchemaError(f"{path}: schema {stab.get('schema')!r}, expected {SCHEMA}")
    return stab


def save(path: Path, stab: dict) -> None:
    path.write_text(json.dumps(stab, indent=1, sort_keys=True) + "\n", "utf-8")


def pair(os_: str, case: str, comp: str) -> str:
    return f"{os_}/{case}/{comp}"


def calibrate(stab: dict, results: list[dict], date: str) -> list[str]:
    """Set each pair's verdict from a calibration's results. A pair needs `POINTS` ratios, all on one image."""
    log: list[str] = []
    by_os: dict[str, list[dict]] = {}
    for r in results:
        by_os.setdefault(r["os"], []).append(r)
    for os_, rs in sorted(by_os.items()):
        images = sorted({r["image"]["version"] for r in rs})
        if len(images) != 1:
            log.append(f"{os_}: runs on several images {images}; not calibrated")
            continue
        ratios: dict[str, list[float]] = {}
        for r in rs:
            for case, c in r["cases"].items():
                for comp, x in c["ratio"].items():
                    ratios.setdefault(pair(os_, case, comp), []).append(x)
        for p, xs in sorted(ratios.items()):
            if len(xs) < POINTS:
                log.append(f"{p}: {len(xs)} ratios, {POINTS} needed; not calibrated")
                continue
            c = stats.cv(xs)
            stab["pairs"][p] = {
                "cv": round(c, 4), "stable": c <= LIMIT, "source": "calibration", "calibrated": date,
                "image": images[0],
            }
            log.append(f"{p}: cv {c:.4f}, {'stable' if c <= LIMIT else 'unstable'}")
    return log


def rolling(stab: dict, data: dict) -> list[str]:
    """Demote each stable pair whose last `POINTS` points since its calibration, on its image and at the latest
    `source`, vary above `LIMIT`. Returns the demoted pairs."""
    demoted = []
    runs = sorted(data["runs"], key=store.run_key)
    for p, e in sorted(stab["pairs"].items()):
        if not e["stable"]:
            continue
        os_, case, comp = p.split("/")
        pts = [
            (r["source"], r["cases"][case]["ratio"][comp])
            for r in runs
            if r["os"] == os_
            and r["image"]["version"] == e["image"]
            and r["date"] > e["calibrated"]
            and comp in r["cases"].get(case, {}).get("ratio", {})
        ]
        if not pts:
            continue
        latest = pts[-1][0]
        xs = [x for s, x in pts if s == latest][-POINTS:]
        if len(xs) == POINTS and (c := stats.cv(xs)) > LIMIT:
            e.update(stable=False, source="rolling", cv=round(c, 4))
            demoted.append(p)
    return demoted


def status(stab: dict, os_: str, case: str, comp: str, image: str) -> str:
    """`stable`, `unstable`, or `uncalibrated` (no calibration, or one on another image)."""
    e = stab["pairs"].get(pair(os_, case, comp))
    if e is None or e["image"] != image:
        return "uncalibrated"
    return "stable" if e["stable"] else "unstable"


def unseen(stab: dict, results: list[dict]) -> list[str]:
    """The OSes whose results ran on an image no pair of theirs was calibrated on."""
    out = set()
    for r in results:
        images = {e["image"] for p, e in stab["pairs"].items() if p.startswith(r["os"] + "/")}
        if r["image"]["version"] not in images:
            out.add(r["os"])
    return sorted(out)
