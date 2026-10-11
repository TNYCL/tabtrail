#!/usr/bin/env python3
"""Writes THIRD-PARTY-NOTICES.txt for the crates linked into tabtrail.

Uses `cargo metadata` (locked, offline once dependencies are fetched) to
walk the normal-dependency closure of the `tabtrail` package, and copies
each crate's own license/notice files verbatim. This documents the
licenses of *dependencies* only; it implies nothing about Tabtrail's own
license, which is an open owner decision.

  python3 scripts/third-party-notices.py OUTPUT_FILE [RUST_TARGET_TRIPLE]
"""

import json
import os
import subprocess
import sys

NOTICE_PREFIXES = ("license", "licence", "copying", "notice", "copyright", "unlicense")


def main():
    out = sys.argv[1]
    target = sys.argv[2] if len(sys.argv) > 2 else None
    if not target:
        rustc = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
        target = next(l.split(": ", 1)[1] for l in rustc.splitlines() if l.startswith("host: "))
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target],
        check=True, capture_output=True, text=True).stdout)
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = next(p["id"] for p in meta["packages"] if p["name"] == "tabtrail")
    def is_proc_macro(pid):
        return any("proc-macro" in t["kind"] for t in pkgs[pid]["targets"])

    def closure(start, through_proc_macros):
        seen, stack = set(), [start]
        while stack:
            node = nodes[stack.pop()]
            for dep in node["deps"]:
                if any(k["kind"] is None for k in dep["dep_kinds"]) and dep["pkg"] not in seen:
                    seen.add(dep["pkg"])
                    if through_proc_macros or not is_proc_macro(dep["pkg"]):
                        stack.append(dep["pkg"])
        return seen

    everything = closure(root, True)
    # Crates compiled into the binary: reached without passing through a
    # procedural macro (macros and their dependencies run only at build time).
    linked = {i for i in closure(root, False) if not is_proc_macro(i)}
    crates = sorted((pkgs[i] for i in everything), key=lambda p: (p["name"], p["version"]))
    lines = [
        "Third-party software notices for tabtrail",
        "=========================================",
        "",
        f"Target: {target}",
        "The tabtrail binary is built from the following crates (those marked",
        "'build time' run only during compilation and are not linked in).",
        "Each crate's license expression and its license/notice files are",
        "reproduced below. This list does not state a license for tabtrail itself.",
        "",
    ]
    for p in crates:
        kind = "" if p["id"] in linked else " (build time only)"
        lines.append(f"- {p['name']} {p['version']}: {p.get('license') or 'see license file'}{kind}")
    lines.append("")
    for p in crates:
        crate_dir = os.path.dirname(p["manifest_path"])
        files = sorted(f for f in os.listdir(crate_dir) if f.lower().startswith(NOTICE_PREFIXES)
                       and os.path.isfile(os.path.join(crate_dir, f)))
        if p.get("license_file"):
            lf = os.path.basename(p["license_file"])
            if lf not in files:
                files.append(lf)
        lines += ["", "=" * 72, f"{p['name']} {p['version']}",
                  f"License: {p.get('license') or 'see below'}",
                  f"Source: {p.get('repository') or 'crates.io'}", "=" * 72]
        if not files:
            lines.append("(no license file shipped in the crate package)")
        for f in files:
            with open(os.path.join(crate_dir, f), encoding="utf-8", errors="replace") as fh:
                lines += ["", f"--- {f} ---", fh.read().rstrip()]
    with open(out, "w") as fh:
        fh.write("\n".join(lines) + "\n")
    print(f"wrote {out}: {len(crates)} crates")


if __name__ == "__main__":
    main()
