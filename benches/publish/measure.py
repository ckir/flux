"""Time Flux against the comparators on this runner and write result-<os>.json (spec "Measuring").

Run in CI by bench-measure.yml:
    python -u benches/publish/measure.py --os linux --flux target/release/flux --work "$RUNNER_TEMP/bench" \
        --out result-linux.json [--tiny]
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import time
import urllib.request
from pathlib import Path
from typing import Callable, Sequence

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import cases as cases_mod  # noqa: E402
import registry  # noqa: E402
import stats  # noqa: E402

SCHEMA = 1
# A case at least this big is checked for a copy that shares blocks with its source (plan decision 4).
SHARE_MIN = 64 * 1024 * 1024

Runner = Callable[..., subprocess.CompletedProcess]


@dataclasses.dataclass(frozen=True)
class Tool:
    name: str
    command: tuple[str, ...]
    ok_exit_codes: frozenset[int]

    def argv(self, src: str, dst: str) -> list[str]:
        return [a.replace("{src}", src).replace("{dst}", dst) for a in self.command]


def cache_drop(os_name: str) -> list[list[str]]:
    """The commands that empty the page cache before a timed copy. Windows has none: it is measured warm."""
    if os_name == "linux":
        return [["sync"], ["sudo", "sh", "-c", "echo 3 > /proc/sys/vm/drop_caches"]]
    if os_name == "macos":
        return [["sync"], ["sudo", "purge"]]
    return []


def tree_digest(root: Path) -> dict[str, str]:
    """Every file under `root`, by its relative path with `/`, to its SHA-256."""
    out = {}
    for path in sorted(root.rglob("*")):
        if path.is_file():
            h = hashlib.sha256()
            with path.open("rb") as f:
                for chunk in iter(lambda: f.read(1 << 20), b""):
                    h.update(chunk)
            out[path.relative_to(root).as_posix()] = h.hexdigest()
    return out


def check_copy(expected: dict[str, str], dst: Path) -> str | None:
    """None when `dst` holds exactly the files of `expected` (the source's `tree_digest`) with the same bytes;
    otherwise what is wrong."""
    if not dst.is_dir():
        return "no destination"
    a, b = expected, tree_digest(dst)
    missing, extra = sorted(set(a) - set(b)), sorted(set(b) - set(a))
    if missing:
        return f"missing {missing[0]}"
    if extra:
        return f"extra {extra[0]}"
    changed = sorted(k for k in a if a[k] != b[k])
    if changed:
        return f"content differs: {changed[0]}"
    return None


def shares_blocks(nbytes: int, free_before: int, free_after: int) -> bool:
    """A copy of `nbytes` that consumed less than half of it in new space shares blocks with its source."""
    return nbytes >= SHARE_MIN and free_before - free_after < nbytes // 2


def _remove(path: Path) -> None:
    def writable(fn: Callable[[str], object], p: str, _exc: object) -> None:
        os.chmod(p, stat.S_IWRITE)
        fn(p)

    if path.exists():
        shutil.rmtree(path, onexc=writable)


def _free(path: Path) -> int:
    return shutil.disk_usage(path).free


def measure_case(
    src: Path,
    nbytes: int,
    tools: Sequence[Tool],
    os_name: str,
    work: Path,
    *,
    run: Runner = subprocess.run,
    clock: Callable[[], float] = time.perf_counter,
    free: Callable[[Path], int] = _free,
    log: Callable[[str], None] = print,
) -> dict:
    """One case on this runner: the spec's "Measuring" steps 2-5."""
    dst = work / "dst"
    # The source is hashed once per case; each copy is compared against it (plan panel r1).
    expected = tree_digest(src)
    times: dict[str, list[float]] = {t.name: [] for t in tools}
    failed: dict[str, str] = {}
    if os_name == "windows":
        for t in tools:
            _remove(dst)
            run(t.argv(str(src), str(dst)), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for r in range(stats.round_count(len(tools))):
        for t in stats.order(list(tools), r):
            if t.name in failed:
                continue
            _remove(dst)
            for argv in cache_drop(os_name):
                run(argv, check=True)
            before = free(work)
            start = clock()
            proc = run(t.argv(str(src), str(dst)), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            elapsed = clock() - start
            if proc.returncode not in t.ok_exit_codes:
                err = (proc.stderr or b"")[-300:].decode("utf-8", "replace")
                failed[t.name] = f"exit code {proc.returncode}: {err}"
            elif shares_blocks(nbytes, before, free(work)):
                failed[t.name] = "the copy shares blocks with its source: a clone, not a copy"
            elif (problem := check_copy(expected, dst)) is not None:
                failed[t.name] = problem
            else:
                times[t.name].append(elapsed)
    _remove(dst)
    for name, why in sorted(failed.items()):
        log(f"{name} failed: {why}")
    medians = {n: stats.median(ts) for n, ts in times.items() if n not in failed and ts}
    ratio = {}
    if "flux" in medians:
        ratio = {n: medians["flux"] / m for n, m in medians.items() if n != "flux" and m > 0}
    return {"times_s": times, "median_s": medians, "failed": sorted(failed), "ratio": ratio}


def measure_all(
    the_cases: Sequence[cases_mod.Case], tools: Sequence[Tool], os_name: str, work: Path, *, header: dict, **kw: object
) -> dict:
    """Every case, in order, under `header`'s run fields."""
    result = {"schema": SCHEMA, **header, "cases": {}}
    for case in the_cases:
        src = cases_mod.generate(case, work)
        result["cases"][case.name] = measure_case(src, case.total, tools, os_name, work, **kw)
        _remove(src)
    return result


def obtain(c: registry.Comparator, tools_dir: Path) -> str | None:
    """The comparator's executable, or None to skip it: a preinstalled tool found on PATH, or a download whose SHA-256
    matches its pin."""
    if c.source == "preinstalled":
        return shutil.which(c.command[0])
    tools_dir.mkdir(parents=True, exist_ok=True)
    path = tools_dir / c.command[0]
    try:
        with urllib.request.urlopen(c.url or "") as response:
            data = response.read()
    except OSError as e:
        print(f"{c.name}: download failed: {e}")
        return None
    if hashlib.sha256(data).hexdigest() != c.sha256:
        print(f"{c.name}: SHA-256 does not match its pin; skipped")
        return None
    path.write_bytes(data)
    path.chmod(path.stat().st_mode | stat.S_IEXEC)
    return str(path)


def _first_line(argv: list[str]) -> str:
    try:
        p = subprocess.run(argv, capture_output=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return "unrecorded"
    for line in (p.stdout + p.stderr).decode("utf-8", "replace").splitlines():
        if line.strip():
            return line.strip()
    return "unrecorded"


def _defender() -> bool | None:
    command = "(Get-MpComputerStatus).RealTimeProtectionEnabled"
    line = _first_line(["powershell", "-NoProfile", "-NonInteractive", "-Command", command])
    return {"True": True, "False": False}.get(line)


def _git(*args: str) -> str:
    return subprocess.run(["git", *args], capture_output=True, text=True, check=True).stdout.strip()


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--os", required=True, choices=registry.OSES)
    p.add_argument("--flux", required=True, type=Path)
    p.add_argument("--registry", type=Path, default=HERE.parent / "comparators.toml")
    p.add_argument("--work", required=True, type=Path)
    p.add_argument("--out", required=True, type=Path)
    p.add_argument("--tiny", action="store_true")
    a = p.parse_args(argv)
    a.work.mkdir(parents=True, exist_ok=True)
    flux = str(a.flux.resolve())
    tools = [Tool("flux", (flux, "copy", "{src}", "{dst}"), frozenset({0}))]
    versions = {"flux": _first_line([flux, "--version"])}
    for c in registry.for_os(registry.parse(a.registry.read_text("utf-8")), a.os):
        exe = obtain(c, a.work / "tools")
        if exe is None:
            print(f"{c.name}: not available on this runner; skipped")
            continue
        tools.append(Tool(c.name, (exe, *c.command[1:]), c.ok_exit_codes))
        versions[c.name] = _first_line(list(c.version)) if c.version else "unrecorded"
    header = {
        "commit": os.environ.get("GITHUB_SHA") or _git("rev-parse", "HEAD"),
        "run": f"{os.environ.get('GITHUB_RUN_ID', '0')}.{os.environ.get('GITHUB_RUN_ATTEMPT', '0')}",
        "source": "+".join(_git("rev-parse", "HEAD:crates", "HEAD:Cargo.toml", "HEAD:Cargo.lock").split()),
        "date": datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "os": a.os,
        "image": {"os": os.environ.get("ImageOS", ""), "version": os.environ.get("ImageVersion", "")},
        "cache": "warm" if a.os == "windows" else "cold",
        "defender": _defender() if a.os == "windows" else None,
        "tools": versions,
    }
    result = measure_all(cases_mod.cases(a.tiny), tools, a.os, a.work, header=header)
    a.out.write_text(json.dumps(result, indent=2) + "\n", "utf-8")
    print(f"wrote {a.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
