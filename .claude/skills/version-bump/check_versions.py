#!/usr/bin/env python3
"""Check that every place the desktop version is written agrees.

Read-only. Prints each location and its version, then OK or MISMATCH.

  python3 .claude/skills/version-bump/check_versions.py
  python3 .claude/skills/version-bump/check_versions.py --expect 1.0.1
  python3 .claude/skills/version-bump/check_versions.py --tag v1.0.1

--expect   every location must hold this version
--tag      the tag about to be pushed must be v<version>, and CHANGELOG.md
           must have a `## <version>` section

Locations (the runtime reads env!("CARGO_PKG_VERSION"), so /auth/app-info,
the MCP server info, the HTTP user agent and the start-up log line follow
src-tauri/Cargo.toml and need no edit):
  src-tauri/Cargo.toml       [package] version
  src-tauri/Cargo.lock       the openalgo-desktop package entry
  src-tauri/tauri.conf.json  version (installer names and the app's About)
  package.json               version
  package-lock.json          version, and packages[""].version
  CHANGELOG.md               the newest `## x.y.z` heading

Exit 0 when all agree (and match --expect / --tag), 1 otherwise. Stdlib only.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
SEMVER = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")


def cargo_toml() -> str | None:
    text = (REPO / "src-tauri" / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r"^\[package\][^\[]*?^version\s*=\s*\"([^\"]+)\"", text, re.M | re.S)
    return m.group(1) if m else None


def cargo_lock() -> str | None:
    text = (REPO / "src-tauri" / "Cargo.lock").read_text(encoding="utf-8")
    m = re.search(r'^name = "openalgo-desktop"\nversion = "([^"]+)"', text, re.M)
    return m.group(1) if m else None


def json_field(rel: str, *path: str) -> str | None:
    data = json.loads((REPO / rel).read_text(encoding="utf-8"))
    for key in path:
        if not isinstance(data, dict) or key not in data:
            return None
        data = data[key]
    return data if isinstance(data, str) else None


def changelog_top() -> str | None:
    p = REPO / "CHANGELOG.md"
    if not p.exists():
        return None
    m = re.search(r"^##\s+\[?v?(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)\]?", p.read_text(encoding="utf-8"), re.M)
    return m.group(1) if m else None


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--expect", help="the version every location must hold")
    ap.add_argument("--tag", help="the v* tag that will be pushed")
    args = ap.parse_args(argv)

    found = {
        "src-tauri/Cargo.toml [package] version": cargo_toml(),
        "src-tauri/Cargo.lock openalgo-desktop": cargo_lock(),
        "src-tauri/tauri.conf.json version": json_field("src-tauri/tauri.conf.json", "version"),
        "package.json version": json_field("package.json", "version"),
        "package-lock.json version": json_field("package-lock.json", "version"),
        'package-lock.json packages[""].version': json_field("package-lock.json", "packages", "", "version"),
    }
    width = max(len(k) for k in found)
    for k, v in found.items():
        print(f"  {k:<{width}}  {v}")
    top = changelog_top()
    print(f"  {'CHANGELOG.md newest section':<{width}}  {top}")

    ok = True
    values = set(found.values())
    if None in values:
        print("MISSING: a location has no version (see None above).")
        ok = False
    if len(values - {None}) > 1:
        print(f"MISMATCH: {sorted(v for v in values if v)}")
        ok = False
    agreed = values - {None}
    version = next(iter(agreed)) if len(agreed) == 1 else None
    if version and not SEMVER.match(version):
        print(f"NOT SEMVER: {version}")
        ok = False
    if args.expect and version != args.expect:
        print(f"EXPECTED {args.expect}, found {version}")
        ok = False
    if args.tag and version is None:
        print(f"TAG {args.tag} cannot be checked until every location agrees")
    elif args.tag:
        if args.tag != f"v{version}":
            print(f"TAG {args.tag} does not match the version (want v{version})")
            ok = False
        if top != version:
            print(f"CHANGELOG.md has no `## {version}` section at the top (newest is {top})")
            ok = False
    elif top and version and top != version:
        print(f"note: CHANGELOG.md newest section is {top}, the version is {version}")
    print("OK" if ok else "FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
