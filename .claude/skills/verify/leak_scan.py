#!/usr/bin/env python3
"""Scan real output for secrets: captured logs, test output, SQLite files.

The desktop's redaction is Rust code (`brokers::common::redact::url_safe`,
`db::sqlite::logs::redact`, `services::error_log::sanitize_url`, the `Secret`
type's redacted Debug). Reading those functions does not tell you whether a
given path leaks; running the path does. Drive the path with a sentinel
credential, capture what it wrote, and scan that output with this script.

What counts as a leak:
  marker    any --marker string (default: SENTINEL, SEKRET), anywhere
  query     a sensitive URL parameter with a value that is not a redaction
            placeholder (apikey=, access_token=, token=, jKey=, susertoken=,
            request_token=, auth_code=, code=, password=, secret=, totp=,
            Value1=, jwt=, session=, enctoken=)
  userinfo  user:password@ inside a URL
  bearer    an Authorization: Bearer / token header value
  jwt       a three-part eyJ... token with a real-looking signature

Usage:
  cargo test --test it <name> -- --nocapture 2>&1 | python3 .claude/skills/verify/leak_scan.py -
  python3 .claude/skills/verify/leak_scan.py run.log
  python3 .claude/skills/verify/leak_scan.py <test-data-dir>/logs.db <test-data-dir>/openalgo.db
  python3 .claude/skills/verify/leak_scan.py --marker MYSENTINEL123 run.log
  python3 .claude/skills/verify/leak_scan.py --shapes-only run.log    (no marker check)

Files ending .db / .sqlite / .sqlite3 are opened read-only and every text
cell of every table is scanned. Anything else is read as text.

Exit codes: 0 clean, 1 leak found, 2 nothing was scanned (an empty capture is
not evidence of anything, so it is refused rather than reported clean).

Never point this at the trader's real data folder; scan a test data
directory. Hits are printed masked (first four characters), never in full.
Stdlib only.
"""

from __future__ import annotations

import argparse
import re
import sqlite3
import sys
from pathlib import Path

DEFAULT_MARKERS = ["SENTINEL", "SEKRET"]

PLACEHOLDER = re.compile(
    r"^(?:<redacted>|%3Credacted%3E|\[redacted\]|%5Bredacted%5D|\[REDACTED\]|REDACTED|\*+|<APIKEY>|<TOKEN>|<SECRET>|x+|)$",
    re.I,
)
QUERY = re.compile(
    r"[?&;](apikey|api_key|access_token|accesstoken|token|jkey|susertoken|request_token|requesttoken|"
    r"auth_code|authcode|code|password|pwd|secret|api_secret|totp|value1|jwt|session|enctoken|"
    r"feed_token|refresh_token|id_token)=([^&\s\"'<>#]*)",
    re.I,
)
USERINFO = re.compile(r"\b[a-z][a-z0-9+.-]*://([^/\s:@]+):([^/\s@]+)@", re.I)
BEARER = re.compile(r"\b(?:authorization|x-api-key|x-privatekey)\b\s*[:=]\s*\"?(?:bearer\s+|token\s+)?([A-Za-z0-9._~+/=-]{12,})", re.I)
JWT = re.compile(r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.([A-Za-z0-9_-]{16,})")
FAKE_SIG = re.compile(r"^(?:sig|signature|c2ln)$")


def mask(v: str) -> str:
    return (v[:4] + "...") if len(v) > 4 else "..."


def scan_text(text: str, where: str, markers: list[str], shapes: bool) -> list[str]:
    hits: list[str] = []
    for n, line in enumerate(text.splitlines(), 1):
        loc = f"{where}:{n}"
        for m in markers:
            if m and m in line:
                hits.append(f"{loc}  marker   {m!r} present")
        if not shapes:
            continue
        for q in QUERY.finditer(line):
            if not PLACEHOLDER.match(q.group(2)):
                hits.append(f"{loc}  query    {q.group(1)}={mask(q.group(2))}")
        for u in USERINFO.finditer(line):
            if not PLACEHOLDER.match(u.group(2)):
                hits.append(f"{loc}  userinfo {u.group(1)[:4]}...:{mask(u.group(2))}@")
        for b in BEARER.finditer(line):
            if not PLACEHOLDER.match(b.group(1)):
                hits.append(f"{loc}  bearer   {mask(b.group(1))}")
        for j in JWT.finditer(line):
            if not FAKE_SIG.match(j.group(1)):
                hits.append(f"{loc}  jwt      eyJ...{mask(j.group(1))}")
    return hits


def scan_sqlite(path: Path, markers: list[str], shapes: bool) -> tuple[int, list[str]]:
    uri = f"file:{path}?mode=ro"
    con = sqlite3.connect(uri, uri=True)
    try:
        tables = [r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")]
        cells = 0
        hits: list[str] = []
        for t in tables:
            cols = [r[1] for r in con.execute(f'PRAGMA table_info("{t}")')]
            for row in con.execute(f'SELECT rowid, * FROM "{t}"'):
                rowid, values = row[0], row[1:]
                for col, v in zip(cols, values):
                    if isinstance(v, bytes):
                        v = v.decode("utf-8", errors="replace")
                    if not isinstance(v, str):
                        continue
                    cells += 1
                    hits.extend(scan_text(v, f"{path.name}:{t}.{col}[rowid {rowid}]", markers, shapes))
        return cells, hits
    finally:
        con.close()


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("inputs", nargs="+", help="files, SQLite databases, or - for stdin")
    ap.add_argument("--marker", action="append", help="sentinel string that must not appear (repeatable)")
    ap.add_argument("--shapes-only", action="store_true", help="skip marker checks")
    ap.add_argument("--markers-only", action="store_true", help="skip secret-shape checks")
    args = ap.parse_args(argv)

    markers = [] if args.shapes_only else (args.marker or DEFAULT_MARKERS)
    shapes = not args.markers_only
    scanned_units = 0
    hits: list[str] = []
    for item in args.inputs:
        if item == "-":
            text = sys.stdin.read()
            scanned_units += len(text.strip()) > 0
            hits.extend(scan_text(text, "<stdin>", markers, shapes))
            continue
        p = Path(item)
        if not p.exists():
            print(f"missing: {item}")
            return 2
        if p.suffix in {".db", ".sqlite", ".sqlite3"}:
            cells, h = scan_sqlite(p, markers, shapes)
            scanned_units += cells > 0
            hits.extend(h)
        else:
            text = p.read_text(encoding="utf-8", errors="replace")
            scanned_units += len(text.strip()) > 0
            hits.extend(scan_text(text, str(p), markers, shapes))

    if scanned_units == 0:
        print("NOTHING SCANNED: every input was empty. An empty capture proves nothing;"
              " check that the path ran and that logging was captured.")
        return 2
    for h in hits:
        print(h)
    if hits:
        print(f"LEAKS: {len(hits)} hit(s)")
        return 1
    print(f"clean: {scanned_units} input(s) scanned, markers {markers or 'off'}, shapes {'on' if shapes else 'off'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
