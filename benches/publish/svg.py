"""latest.svg: the latest run of main on each runner, with the reading aids the spec requires."""

from __future__ import annotations

from xml.sax.saxutils import escape

import stability

LABELS = {"linux": "Linux", "macos": "macOS", "windows": "Windows"}
LINE = 20
WIDTH = 720


def _defender(v: bool | None) -> str:
    return {True: "on", False: "off"}.get(v, "unknown")


def _lines(latest: dict[str, dict], stab: dict) -> list[tuple[str, str]]:
    lines = [
        ("Flux benchmark - Latest benchmark of main", "title"),
        ("Flux time / comparator time. Below 1.0, Flux is faster.", "note"),
    ]
    for os_ in ("linux", "macos", "windows"):
        r = latest.get(os_)
        if r is None:
            continue
        head = f"{LABELS[os_]} - {r['cache']} cache"
        if os_ == "windows":
            head += f", Defender {_defender(r.get('defender'))}"
        head += f" - {r['commit'][:7]} {r['date'][:10]}"
        lines.append(("", "gap"))
        lines.append((head, "head"))
        comps = [t for t in r["tools"] if t != "flux"]
        hidden = []
        for case, c in r["cases"].items():
            for comp in comps:
                if comp in c["failed"] or "flux" in c["failed"]:
                    hidden.append(f"{case} vs {comp}: the copy failed its check")
                    continue
                st = stability.status(stab, os_, case, comp, r["image"]["version"])
                if st == "stable" and comp in c["ratio"]:
                    lines.append((f"{case:<6} vs {comp:<10} {c['ratio'][comp]:.2f}", "row"))
                elif st == "unstable":
                    hidden.append(f"{case} vs {comp}: too noisy")
                else:
                    hidden.append(f"{case} vs {comp}: not yet calibrated on this image")
        if hidden:
            lines.append(("Not shown:", "note"))
            lines.extend((f"  {h}", "note") for h in hidden)
    return lines


def render(latest: dict[str, dict], stab: dict) -> str:
    lines = _lines(latest, stab)
    height = LINE * (len(lines) + 1)
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" '
        f'font-family="ui-monospace, Menlo, Consolas, monospace" font-size="13">',
        f'<rect width="{WIDTH}" height="{height}" fill="#ffffff"/>',
    ]
    weight = {"title": "bold", "head": "bold"}
    for i, (text, kind) in enumerate(lines):
        if kind == "gap":
            continue
        y = LINE * (i + 1)
        fill = "#555555" if kind == "note" else "#111111"
        out.append(
            f'<text x="12" y="{y}" fill="{fill}" font-weight="{weight.get(kind, "normal")}" '
            f'xml:space="preserve">{escape(text)}</text>'
        )
    out.append("</svg>")
    return "\n".join(out) + "\n"
