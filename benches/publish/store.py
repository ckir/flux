"""data.json on the data branch: every run, append-only (spec "Result shapes")."""

from __future__ import annotations

import json
from pathlib import Path

SCHEMA = 1


class SchemaError(ValueError):
    """A file or a result carries a `schema` this code does not know: stop rather than misread it."""


def load(path: Path) -> dict:
    if not path.exists():
        return {"schema": SCHEMA, "runs": []}
    data = json.loads(path.read_text("utf-8"))
    if data.get("schema") != SCHEMA:
        raise SchemaError(f"{path}: schema {data.get('schema')!r}, expected {SCHEMA}")
    return data


def save(path: Path, data: dict) -> None:
    path.write_text(json.dumps(data, indent=1, sort_keys=True) + "\n", "utf-8")


def add(data: dict, result: dict) -> None:
    """Append `result`; a re-run of a commit is a new run, never a replacement."""
    if result.get("schema") != SCHEMA:
        raise SchemaError(f"result schema {result.get('schema')!r}, expected {SCHEMA}")
    data["runs"].append(result)


def run_key(r: dict) -> tuple[int, int]:
    """`<run id>.<attempt>` as numbers: later runs compare higher; dates can tie."""
    run_id, attempt = r["run"].split(".")
    return int(run_id), int(attempt)


def plotted(data: dict) -> list[dict]:
    """Per (`commit`, `os`, image version), the run with the highest `run`, in run order."""
    best: dict[tuple[str, str, str], dict] = {}
    for r in data["runs"]:
        k = (r["commit"], r["os"], r["image"]["version"])
        if k not in best or run_key(r) > run_key(best[k]):
            best[k] = r
    return sorted(best.values(), key=run_key)


def latest_per_os(data: dict) -> dict[str, dict]:
    """Each OS's run with the highest `run`."""
    out: dict[str, dict] = {}
    for r in data["runs"]:
        if r["os"] not in out or run_key(r) > run_key(out[r["os"]]):
            out[r["os"]] = r
    return out
