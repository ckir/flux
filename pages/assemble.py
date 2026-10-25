# Copyright (c) 2026 Costas Kirgoussios
#
# This software is licensed under the PolyForm Noncommercial License 1.0.0.
# You may obtain a copy of the License at:
# https://polyformproject.org/licenses/noncommercial/1.0.0/

"""Move rustdoc output under api/, put the landing page at the root, and leave redirect stubs."""

from __future__ import annotations

import argparse
import html
import json
import re
import shutil
import sys
from pathlib import Path

CANONICAL_BASE = "https://ckir.github.io/flux/api/"
SAFE_PATH = re.compile(r"[A-Za-z0-9._/-]+")
RESERVED_SITE_NAMES = frozenset({"api", "bench"})


class AssembleError(Exception):
    pass


def rel_target(rel_path: str) -> str:
    return "../" * rel_path.count("/") + "api/" + rel_path


def _js_string(value: str) -> str:
    return (
        json.dumps(value)
        .replace("<", "\\u003c")
        .replace(">", "\\u003e")
        .replace("&", "\\u0026")
    )


def stub_html(rel_path: str) -> str:
    target = rel_target(rel_path)
    attr = html.escape(target, quote=True)
    canonical = html.escape(CANONICAL_BASE + rel_path, quote=True)
    return (
        "<!DOCTYPE html>\n"
        '<html lang="en">\n'
        "<head>\n"
        '<meta charset="utf-8">\n'
        "<title>Redirecting</title>\n"
        f'<meta http-equiv="refresh" content="0; url={attr}">\n'
        f'<link rel="canonical" href="{canonical}">\n'
        f"<script>location.replace({_js_string(target)} + location.search + location.hash)</script>\n"
        "</head>\n"
        f'<body><p>Moved to <a href="{attr}">{attr}</a>.</p></body>\n'
        "</html>\n"
    )


def list_files(doc: Path) -> list[str]:
    return sorted(
        p.relative_to(doc).as_posix()
        for p in doc.rglob("*")
        if p.is_file() and p.relative_to(doc).as_posix() != ".lock"
    )


def _inside(path: Path, parent: Path) -> bool:
    return path == parent or parent in path.parents


def assemble(doc: Path, site: Path, template: Path, work: Path) -> None:
    doc, site, template, work = (Path(p).resolve() for p in (doc, site, template, work))
    if not doc.is_dir():
        raise AssembleError(f"rustdoc output not found: {doc}")
    if (doc / "api").exists():
        raise AssembleError(f"{doc / 'api'} already exists; refusing to assemble twice")
    if _inside(work, doc):
        raise AssembleError(f"work directory {work} is inside the doc tree {doc}")
    if not template.is_file():
        raise AssembleError(f"template not found: {template}")

    manifest = list_files(doc)
    for path in manifest:
        if path.endswith(".html") and not SAFE_PATH.fullmatch(path):
            raise AssembleError(f"unsafe html path: {path!r}")

    if not site.is_dir():
        raise AssembleError(f"site directory not found: {site}")
    entries = sorted(site.iterdir())
    for entry in entries:
        if entry.is_dir():
            raise AssembleError(f"site must be flat, found directory: {entry.name}")
    if not (site / "index.html").is_file():
        raise AssembleError("site has no index.html")
    doc_names = {p.name for p in doc.iterdir()}
    for entry in entries:
        if entry.name in RESERVED_SITE_NAMES or entry.name in doc_names:
            raise AssembleError(f"site entry collides with a rustdoc or reserved name: {entry.name}")

    work.mkdir(parents=True, exist_ok=True)
    (work / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    staging = work / "staging"
    if staging.exists():
        shutil.rmtree(staging)
    staging.mkdir()
    for entry in sorted(doc.iterdir()):
        if entry.name == ".lock":
            continue
        shutil.move(str(entry), str(staging / entry.name))
    shutil.move(str(staging), str(doc / "api"))

    shutil.copyfile(template, doc / "api" / "index.html")

    for path in manifest:
        if path.endswith(".html"):
            stub = doc / path
            stub.parent.mkdir(parents=True, exist_ok=True)
            stub.write_text(stub_html(path), encoding="utf-8")

    for entry in entries:
        shutil.copyfile(entry, doc / entry.name)


def verify(doc: Path, site: Path, work: Path) -> list[str]:
    doc, site, work = Path(doc), Path(site), Path(work)
    problems: list[str] = []
    manifest: list[str] = []
    try:
        loaded = json.loads((work / "manifest.json").read_text(encoding="utf-8"))
        if not isinstance(loaded, list) or not all(isinstance(p, str) for p in loaded):
            raise ValueError("not a list of paths")
        manifest = loaded
    except (OSError, ValueError) as e:
        problems.append(f"manifest unreadable: {work / 'manifest.json'}: {e}")

    api = doc / "api"
    for path in manifest:
        if not (api / path).is_file():
            problems.append(f"missing: api/{path}")
        if path.endswith(".html"):
            stub = doc / path
            try:
                ok = stub.is_file() and stub.read_text(encoding="utf-8") == stub_html(path)
            except (OSError, ValueError):
                ok = False
            if not ok:
                problems.append(f"bad stub: {path}")

    if not (api / "index.html").is_file():
        problems.append("missing: api/index.html")
    if not (api / "static.files").is_dir():
        problems.append("missing: api/static.files directory")
    if (api / "bench").exists():
        problems.append("unexpected: api/bench must not exist (benchmarks belong at /bench/)")

    for entry in sorted(site.iterdir()) if site.is_dir() else []:
        label = "landing page" if entry.name == "index.html" else f"site file: {entry.name}"
        try:
            if (doc / entry.name).read_bytes() != entry.read_bytes():
                problems.append(f"{label}: {entry.name} differs from site/{entry.name}")
        except OSError as e:
            problems.append(f"{label}: cannot compare {entry.name}: {e}")
    if not (site / "index.html").is_file():
        problems.append("landing page: site/index.html is missing")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    a = sub.add_parser("assemble", help="move rustdoc under api/ and write redirect stubs")
    a.add_argument("--doc", type=Path, default=Path("target/doc"))
    a.add_argument("--site", type=Path, default=Path("site"))
    a.add_argument("--template", type=Path, default=Path("pages/api-index.html"))
    a.add_argument("--work", type=Path, default=Path("target/pages-work"))
    v = sub.add_parser("verify", help="check the assembled tree against the pre-move manifest")
    v.add_argument("--doc", type=Path, default=Path("target/doc"))
    v.add_argument("--site", type=Path, default=Path("site"))
    v.add_argument("--work", type=Path, default=Path("target/pages-work"))
    args = parser.parse_args(argv)
    try:
        if args.command == "assemble":
            assemble(args.doc, args.site, args.template, args.work)
            manifest = json.loads((args.work / "manifest.json").read_text(encoding="utf-8"))
            stubs = sum(1 for p in manifest if p.endswith(".html"))
            print(f"assemble: ok ({len(manifest)} files, {stubs} stubs)")
        else:
            problems = verify(args.doc, args.site, args.work)
            if problems:
                for problem in problems:
                    print(problem, file=sys.stderr)
                return 1
            manifest = json.loads((args.work / "manifest.json").read_text(encoding="utf-8"))
            stubs = sum(1 for p in manifest if p.endswith(".html"))
            print(f"verify: ok ({len(manifest)} files, {stubs} stubs)")
    except AssembleError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
