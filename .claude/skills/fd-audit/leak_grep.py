#!/usr/bin/env python3
"""Print suspects for the common Rust leak patterns in src-tauri/src.

A suspect is a line to read, not a leak. Every check is a heuristic over
source text: it finds the shapes that leak in this codebase and the reader
decides whether the resource is released on every path. Test code is skipped
(files named tests.rs, anything under a tests/ directory, and everything after
the first `#[cfg(test)]` in a file).

Checks:
  spawn-unowned     tokio::spawn / tauri::async_runtime::spawn / thread::spawn
                    used as a statement, its handle dropped (fire and forget)
  http-client       reqwest::Client::new() or Client::builder() (one shared
                    client per process: brokers::common::http::client())
  unbounded-chan    unbounded_channel(), std mpsc::channel(), crossbeam
                    unbounded(): a slow consumer grows memory without limit
  map-no-evict      a HashMap/BTreeMap/HashSet/Vec/VecDeque field or static
                    behind a lock whose name is never removed from, retained,
                    cleared or drained in the same file
  broadcast-lagged  a file that receives from a broadcast channel and never
                    names Lagged (a lagged receiver errors and a loop that
                    treats every error as fatal dies quietly)
  conn-across-await a pooled SQLite/DuckDB connection bound with `let` and an
                    `.await` later in the same block, before it is dropped
  leak-forget       Box::leak or mem::forget

Usage:
  python3 .claude/skills/fd-audit/leak_grep.py                 whole src-tauri/src
  python3 .claude/skills/fd-audit/leak_grep.py --diff origin/master
                                                               only files changed since a ref
  python3 .claude/skills/fd-audit/leak_grep.py --check spawn-unowned --check map-no-evict
  python3 .claude/skills/fd-audit/leak_grep.py path/to/file.rs ...
  --strict   exit 1 when any suspect is printed (default: always exit 0)
  python3 .claude/skills/fd-audit/leak_grep.py --self-test
             every check must fire on a planted file, exactly where planted

Stdlib only.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
SRC = REPO / "src-tauri" / "src"

CHECKS = [
    "spawn-unowned",
    "http-client",
    "unbounded-chan",
    "map-no-evict",
    "broadcast-lagged",
    "conn-across-await",
    "leak-forget",
]

SPAWN = re.compile(
    r"^\s*(?:tokio::spawn|tokio::task::spawn|task::spawn|tauri::async_runtime::spawn|"
    r"std::thread::spawn|thread::spawn|tokio::task::spawn_blocking|spawn_blocking)\s*\("
)
HTTP = re.compile(r"\breqwest::Client::new\(\)|\bClient::builder\(\)|\breqwest::ClientBuilder::new\(\)")
UNBOUNDED = re.compile(
    r"\bunbounded_channel\s*(?:::<[^>]*>)?\s*\(|\bstd::sync::mpsc::channel\s*(?:::<[^>]*>)?\s*\(|"
    r"\bmpsc::channel\s*(?:::<[^>]*>)?\s*\(\s*\)|\bcrossbeam(?:_channel)?::unbounded\s*\("
)
COLLECTION = r"(?:HashMap|BTreeMap|HashSet|BTreeSet|Vec|VecDeque|IndexMap|DashMap)\s*<"
LOCKED_FIELD = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z_][a-z0-9_]*)\s*:\s*[^=;]*?"
    r"(?:Mutex|RwLock|RefCell)\s*<[^;]*?" + COLLECTION
)
DASHMAP_FIELD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z_][a-z0-9_]*)\s*:\s*[^;]*\bDashMap\s*<")
STATIC = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?static\s+([A-Z_][A-Z0-9_]*)\s*:[^;]*(?:Mutex|RwLock|DashMap)\s*<[^;]*" + COLLECTION
)
EVICT = r"\.(?:remove|retain|clear|drain|truncate|pop|pop_front|pop_back|split_off|swap_remove|take)\b|std::mem::take|mem::take|std::mem::replace|mem::replace"
BROADCAST_RECV = re.compile(
    r"broadcast::Receiver|\.subscribe_ticks\(\)|\.subscribe\(\)\s*;?\s*$|broadcast::channel", re.M
)
RECV = re.compile(r"\.recv\(\)\s*\.await")
# `let c = ctx.sqlite.conn()?;` (the connection itself is bound), not
# `let r = ctx.sqlite.conn().and_then(|c| ..)` (the connection is consumed).
CONN_LET = re.compile(
    r"^(\s*)let\s+(?:mut\s+)?([a-z_][a-z0-9_]*)\s*(?::[^=]*)?=\s*[^;]*\.conn\(\)"
    r"\s*(?:\?|\.map_err\([^;]*\)\s*\?|\.expect\([^;]*\)|\.unwrap\(\))?\s*;"
)
AWAIT = re.compile(r"\.await\b")
FORGET = re.compile(r"\bBox::leak\s*\(|\bmem::forget\s*\(")


def rel(p: Path) -> str:
    try:
        return str(p.relative_to(REPO))
    except ValueError:
        return str(p)


def production_lines(path: Path) -> list[str]:
    """Source lines up to the first #[cfg(test)] (unit tests live below it)."""
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    for i, line in enumerate(lines):
        if line.strip().startswith("#[cfg(test)]"):
            return lines[:i]
    return lines


# Compiled only for tests (`#[cfg(any(test, feature = "test-support"))]` on
# their `mod` line), so not part of the shipped app.
TEST_SUPPORT = {"src-tauri/src/brokers/mock.rs"}


def is_test_file(path: Path) -> bool:
    parts = path.parts
    return (
        path.name == "tests.rs"
        or "tests" in parts
        or path.name.endswith("_tests.rs")
        or rel(path) in TEST_SUPPORT
    )


def spawn_is_awaited_or_kept(code: list[str], n: int) -> bool:
    """True when the spawn call (from line n, 1-based) is followed by .await
    or its value is returned as the block's tail (no `;` after the call)."""
    # `let h =\n    tokio::spawn(..)` or a spawn passed as an argument: kept.
    prev = next((c.rstrip() for c in reversed(code[: n - 1]) if c.strip()), "")
    if prev.endswith(("=", "(", ",", "=>", "push(", "Some(")):
        return True
    text = "\n".join(code[n - 1 : n + 600])
    # Parentheses inside string and char literals do not count.
    text = re.sub(r'"(?:\\.|[^"\\])*"', '""', text)
    text = re.sub(r"'(?:\\.|[^'\\])'", "''", text)
    start = text.find("(")
    depth = 0
    for i in range(start, len(text)):
        ch = text[i]
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                rest = text[i + 1 : i + 200].lstrip()
                return rest.startswith(".await") or rest.startswith("?") or not rest.startswith(";")
    return False


def code_part(line: str) -> str:
    """The line without a trailing // comment (good enough for these checks)."""
    in_str = False
    for i, ch in enumerate(line):
        if ch == '"' and (i == 0 or line[i - 1] != "\\"):
            in_str = not in_str
        if not in_str and line.startswith("//", i):
            return line[:i]
    return line


def check_file(path: Path, enabled: set[str]) -> list[tuple[str, int, str, str]]:
    out: list[tuple[str, int, str, str]] = []
    lines = production_lines(path)
    code = [code_part(l) for l in lines]
    text = "\n".join(code)

    for n, line in enumerate(code, 1):
        stripped = line.strip()
        if stripped.startswith("//") or stripped.startswith("*"):
            continue
        if "spawn-unowned" in enabled and SPAWN.search(line) and not spawn_is_awaited_or_kept(code, n):
            # A statement whose value is dropped; `let h = tokio::spawn(..)`,
            # `set.spawn(..)`, `handles.push(tokio::spawn(..))` do not match.
            out.append(("spawn-unowned", n, stripped, "handle dropped: who aborts this task on logout/shutdown?"))
        if "http-client" in enabled and HTTP.search(line):
            out.append(("http-client", n, stripped, "a client per call leaks a pool; use the shared client, with timeouts"))
        if "unbounded-chan" in enabled and UNBOUNDED.search(line):
            out.append(("unbounded-chan", n, stripped, "unbounded queue: what bounds the backlog if the consumer stalls?"))
        if "leak-forget" in enabled and FORGET.search(line):
            out.append(("leak-forget", n, stripped, "deliberate leak: is it once per process, not per call?"))

    if "map-no-evict" in enabled:
        for n, line in enumerate(code, 1):
            m = LOCKED_FIELD.search(line) or DASHMAP_FIELD.search(line) or STATIC.search(line)
            if not m:
                continue
            name = m.group(1)
            # Evicted directly (`self.x.lock().retain(..)`), through a guard
            # (`let mut m = self.x.lock(); m.retain(..)`) or by mem::take.
            names = [name] + re.findall(
                r"let\s+(?:mut\s+)?([a-z_][a-z0-9_]*)\s*=\s*[^;\n{]*\b" + re.escape(name)
                + r"\b\s*\.(?:lock|write|borrow_mut|get_mut)\(\)",
                text,
            )
            evicts = any(
                re.search(r"\b" + re.escape(x) + r"\b[^\n;]*?(?:" + EVICT + r")", text)
                or re.search(r"(?:" + EVICT + r")\s*\(\s*&mut\s*\*?[^)]*\b" + re.escape(x) + r"\b", text)
                for x in names
            )
            if not evicts:
                out.append((
                    "map-no-evict", n, line.strip(),
                    f"no remove/retain/clear/drain of `{name}` in this file: what is its key space and who evicts?",
                ))

    if "broadcast-lagged" in enabled:
        if BROADCAST_RECV.search(text) and RECV.search(text) and "Lagged" not in text:
            for n, line in enumerate(code, 1):
                if RECV.search(line):
                    out.append((
                        "broadcast-lagged", n, line.strip(),
                        "file receives from a channel and never names Lagged: is this a broadcast receiver?",
                    ))

    if "conn-across-await" in enabled:
        for n, line in enumerate(code, 1):
            m = CONN_LET.match(line)
            if not m:
                continue
            indent, name = len(m.group(1)), m.group(2)
            for k in range(n, len(code)):
                nxt = code[k]
                if nxt.strip() == "":
                    continue
                cur_indent = len(nxt) - len(nxt.lstrip())
                if cur_indent < indent:
                    break  # the block that owns the connection ended
                if re.search(r"\bdrop\(\s*" + re.escape(name) + r"\s*\)", nxt):
                    break
                if AWAIT.search(nxt):
                    out.append((
                        "conn-across-await", n, line.strip(),
                        f"`{name}` is still held at line {k + 1} ({nxt.strip()[:60]}): drop it before awaiting",
                    ))
                    break
    return out


PLANTED = r'''
use std::collections::HashMap;
pub struct Leaky {
    cache: Mutex<HashMap<String, Vec<u8>>>,
    kept: Mutex<HashMap<String, u8>>,
}
impl Leaky {
    fn evict(&self) { self.kept.lock().retain(|_, v| *v > 0); }
    async fn run(&self, ctx: &AppState) {
        tokio::spawn(async move { loop { tick().await; } });
        let owned = tokio::spawn(async {});
        let awaited = tokio::task::spawn_blocking(|| 1).await;
        let http = reqwest::Client::new();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<u8>();
        let mut ticks = ctx.bus_tx.subscribe();
        while let Ok(t) = ticks.recv().await { use_it(t); }
        let conn = ctx.sqlite.conn()?;
        let row = load(&conn);
        broker.call().await;
        let fine = ctx.sqlite.conn().and_then(|c| load(&c));
        other().await;
        Box::leak(Box::new(1));
    }
}
'''

# check -> the planted line numbers (1-based within PLANTED) it must report,
# and nothing else.
PLANTED_EXPECT = {
    "spawn-unowned": {9},
    "http-client": {12},
    "unbounded-chan": {13},
    "map-no-evict": {3},
    "broadcast-lagged": {15},
    "conn-across-await": {16},
    "leak-forget": {21},
}


def self_test() -> int:
    """Run every check on a planted file; each must fire exactly where planted."""
    import tempfile

    with tempfile.TemporaryDirectory() as d:
        f = Path(d) / "planted.rs"
        f.write_text(PLANTED.lstrip("\n"), encoding="utf-8")
        got: dict[str, set[int]] = {c: set() for c in CHECKS}
        for check, line, _, _ in check_file(f, set(CHECKS)):
            got[check].add(line)
    bad = 0
    for check in CHECKS:
        ok = got[check] == PLANTED_EXPECT[check]
        bad += not ok
        print(f"{'ok  ' if ok else 'FAIL'}  {check}: expected lines {sorted(PLANTED_EXPECT[check])}, got {sorted(got[check])}")
    print("SELF-TEST PASSED" if not bad else f"SELF-TEST FAILED: {bad} check(s)")
    return 1 if bad else 0


def changed_files(ref: str) -> list[Path]:
    res = subprocess.run(
        ["git", "-C", str(REPO), "diff", "--name-only", ref, "--", "src-tauri/src"],
        capture_output=True, text=True, check=False,
    )
    if res.returncode != 0:
        sys.exit(f"git diff against {ref} failed: {res.stderr.strip()}")
    return [REPO / p for p in res.stdout.split() if p.endswith(".rs") and (REPO / p).exists()]


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("paths", nargs="*", help="files or directories (default: src-tauri/src)")
    ap.add_argument("--diff", metavar="REF", help="only .rs files changed since REF")
    ap.add_argument("--check", action="append", choices=CHECKS, help="run only these checks")
    ap.add_argument("--strict", action="store_true", help="exit 1 when anything is printed")
    ap.add_argument("--self-test", action="store_true", help="prove every check fires on a planted file")
    args = ap.parse_args(argv)
    if args.self_test:
        return self_test()

    enabled = set(args.check or CHECKS)
    if args.diff:
        files = changed_files(args.diff)
    else:
        roots = [Path(p).resolve() for p in args.paths] or [SRC]
        files = []
        for r in roots:
            files.extend(sorted(r.rglob("*.rs")) if r.is_dir() else [r])
    files = [f for f in files if not is_test_file(f)]
    if not files:
        print("No production .rs files to check.")
        return 0

    found: dict[str, list[str]] = {c: [] for c in CHECKS}
    for f in files:
        for check, line, snippet, why in check_file(f, enabled):
            found[check].append(f"  {rel(f)}:{line}  {snippet[:110]}\n      -> {why}")

    total = 0
    for check in CHECKS:
        if check not in enabled:
            continue
        items = found[check]
        total += len(items)
        print(f"[{check}] {len(items)} suspect(s)")
        for it in items:
            print(it)
        print()
    print(f"{total} suspect(s) across {len(files)} file(s). Each is a line to read, not a verdict.")
    return 1 if (args.strict and total) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
