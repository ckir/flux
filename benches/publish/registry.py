"""The comparator registry (benches/comparators.toml): the copiers Flux is measured against, and on which OS.

The spec's "The comparator registry" defines the format. Flux itself is not an entry: it is always measured.
"""

from __future__ import annotations

import dataclasses
import re
import tomllib

OSES = ("windows", "linux", "macos")
KEYS = {"name", "os", "command", "ok_exit_codes", "version", "source", "url", "sha256", "note"}


class RegistryError(ValueError):
    """The registry is not valid; the message names the entry and the rule."""


@dataclasses.dataclass(frozen=True)
class Comparator:
    name: str
    os: tuple[str, ...]
    command: tuple[str, ...]
    ok_exit_codes: frozenset[int]
    version: tuple[str, ...] | None
    source: str
    url: str | None
    sha256: str | None
    note: str | None

    def argv(self, src: str, dst: str) -> list[str]:
        """The command with `{src}` and `{dst}` replaced inside each argument. Never joined into a shell string, so a
        path with spaces stays one argument."""
        return [a.replace("{src}", src).replace("{dst}", dst) for a in self.command]


def parse(text: str) -> list[Comparator]:
    """Every comparator in `text`, in file order. Raises `RegistryError` on the first rule broken."""
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        raise RegistryError(f"not TOML: {e}") from e
    extra = set(data) - {"comparator"}
    if extra:
        raise RegistryError(f"unknown top-level keys {sorted(extra)}")
    entries = data.get("comparator", [])
    if not isinstance(entries, list):
        raise RegistryError("`comparator` must be an array of tables ([[comparator]])")
    out: list[Comparator] = []
    for i, e in enumerate(entries):
        c = _comparator(i, e)
        if any(x.name == c.name for x in out):
            raise RegistryError(f"comparator {c.name!r} is declared twice")
        out.append(c)
    return out


def for_os(comparators: list[Comparator], os_name: str) -> list[Comparator]:
    """The comparators that run on `os_name`, in registry order."""
    return [c for c in comparators if os_name in c.os]


def _strings(where: str, key: str, value: object) -> tuple[str, ...]:
    if not isinstance(value, list) or not value or not all(isinstance(v, str) and v for v in value):
        raise RegistryError(f"{where}: `{key}` must be a non-empty array of non-empty strings")
    return tuple(value)


def _comparator(i: int, e: object) -> Comparator:
    where = f"comparator #{i + 1}"
    if not isinstance(e, dict):
        raise RegistryError(f"{where}: not a table")
    unknown = set(e) - KEYS
    if unknown:
        raise RegistryError(f"{where}: unknown keys {sorted(unknown)}")
    name = e.get("name")
    if not isinstance(name, str) or not re.fullmatch(r"[a-z0-9][a-z0-9_-]*", name):
        raise RegistryError(f"{where}: `name` must be a lowercase letter or digit, then letters, digits, '-' or '_'")
    if name == "flux":
        raise RegistryError(f"{where}: `flux` is not a comparator; it is always measured")
    where = f"comparator {name!r}"
    os_ = _strings(where, "os", e.get("os"))
    bad = [o for o in os_ if o not in OSES]
    if bad:
        raise RegistryError(f"{where}: unknown os {bad}; use {list(OSES)}")
    command = _strings(where, "command", e.get("command"))
    if not any("{src}" in a for a in command) or not any("{dst}" in a for a in command):
        raise RegistryError(f"{where}: `command` must contain both {{src}} and {{dst}}")
    codes = e.get("ok_exit_codes", [0])
    if (
        not isinstance(codes, list)
        or not codes
        or not all(isinstance(c, int) and not isinstance(c, bool) for c in codes)
    ):
        raise RegistryError(f"{where}: `ok_exit_codes` must be a non-empty array of integers")
    version = e.get("version")
    if version is not None:
        version = _strings(where, "version", version)
    source, url, sha = e.get("source"), e.get("url"), e.get("sha256")
    if source == "preinstalled":
        if url is not None or sha is not None:
            raise RegistryError(f"{where}: a preinstalled comparator has no `url` or `sha256`")
    elif source == "download":
        if not isinstance(sha, str) or not re.fullmatch(r"[0-9a-f]{64}", sha):
            raise RegistryError(f"{where}: a download needs `sha256`, 64 lowercase hex digits")
        if not isinstance(url, str) or not url.startswith("https://"):
            raise RegistryError(f"{where}: a download needs an https `url`")
    else:
        raise RegistryError(f'{where}: `source` must be "preinstalled" or "download"')
    note = e.get("note")
    if note is not None and not isinstance(note, str):
        raise RegistryError(f"{where}: `note` must be a string")
    return Comparator(name, os_, command, frozenset(codes), version, source, url, sha, note)
