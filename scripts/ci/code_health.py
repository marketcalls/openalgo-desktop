#!/usr/bin/env python3
"""Code-health gate (CLAUDE.md, "Code health").

Measures the Rust production code and the desktop's own frontend files and
compares them with .github/code-health-baseline.json. The baseline is a
ratchet: what was over a limit when the gate started is listed with its size
then. A listed item may shrink and may not grow, nothing new may join the
list, and an improvement must be written back so it cannot be undone.

    python3 scripts/ci/code_health.py                 check (CI runs this)
    python3 scripts/ci/code_health.py --update        lower the baseline after an improvement
    python3 scripts/ci/code_health.py --base-ref REF  also refuse a baseline that gained or
                                                      raised an entry against REF's baseline

--update only lowers or removes entries; it never adds or raises one. Adding
or raising an entry is the maintainer's call (the code-health-exception label).

Checks, on production code (test modules and test-support code excluded):
  rust_files             a .rs file over RUST_FILE_LINES lines
  rust_functions         a fn over RUST_FN_LINES lines (HANDLER_FN_LINES in an HTTP route
                         or a Tauri command, which only validate, call a service and reply)
  handler_db_access      an HTTP route or Tauri command file that opens the database itself
  lint_allows            #[allow(..)] / #[expect(..)], counted per file and lint
  dead_public_functions  a pub fn whose name appears nowhere else in the code
  frontend_files         a .ts/.tsx file over TS_FILE_LINES lines ("web": ported, exempt)

Standard library only; Python 3.10+.
"""

from __future__ import annotations

import argparse
import bisect
import json
import os
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass, field

RUST_FILE_LINES = 1000
RUST_FN_LINES = 100
HANDLER_FN_LINES = 60
TS_FILE_LINES = 1000
# The code-health pass (docs: CLAUDE.md, "Code health") is done: what stays
# listed is generated code, an allow kept on purpose with its reason, or a
# frontend file carried over from the web. A listed item may only shrink; one
# that grows fails, and a pull request that cannot avoid it needs the
# maintainer's code-health-exception label. (True turned growth into a
# warning while the pass was under way.)
EXISTING_MAY_GROW = False

BASELINE = ".github/code-health-baseline.json"
RUST_ROOT = "src-tauri/src"
HANDLER_DIRS = ("src-tauri/src/server/routes/", "src-tauri/src/commands/")
FRONTEND_ROOT = "src"
# Written by generators whose --check compares exact output; never edited by hand.
GENERATED = {
    "src/lib/trading/chromeIcons.ts",
    "src/lib/trading/drawingToolMetadata.ts",
}
# Where a pub fn may be called from: the whole Rust tree, the frontend
# (invoke by name) and the Tauri permission files.
REFERENCE_ROOTS = ("src-tauri", "src")
# "vendor": third-party crates kept as published (src-tauri/vendor), not
# ours to size, lint or count as callers of our pub fns.
SKIP_DIRS = {"target", "node_modules", "gen", "dist", ".git", "vendor"}

MAP_CHECKS = ("rust_files", "rust_functions", "handler_db_access", "lint_allows")
LIST_CHECKS = ("dead_public_functions",)

# ------------------------------------------------------------------ lexing

_TOKEN = re.compile(r"//|/\*|b?r(#*)\"|b?\"|b?'")
_BLOCK = re.compile(r"/\*|\*/")
_STRING_END = re.compile(r'(?:[^"\\]|\\.)*"', re.S)
_ESCAPED_CHAR = re.compile(r"\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.)'", re.S)
_NOT_NEWLINE = re.compile(r"[^\n]")


def _blank(s: str) -> str:
    return _NOT_NEWLINE.sub(" ", s)


def lex(text: str) -> tuple[str, str]:
    """Return (code, nocomment), both the length of `text`.

    `code` has comments and the contents of string and char literals blanked
    (delimiters kept), so braces and keywords in it are real. `nocomment`
    keeps literals and blanks only comments, for attribute text and names.
    Newlines are kept everywhere, so offsets and line numbers line up.
    """
    code: list[str] = []
    nocom: list[str] = []
    i, n = 0, len(text)
    while i < n:
        m = _TOKEN.search(text, i)
        if not m:
            code.append(text[i:])
            nocom.append(text[i:])
            break
        s, tok = m.start(), m.group(0)
        if tok[0] in "br" and s > 0 and (text[s - 1].isalnum() or text[s - 1] == "_"):
            # The tail of an identifier, not a literal prefix.
            code.append(text[i : s + 1])
            nocom.append(text[i : s + 1])
            i = s + 1
            continue
        code.append(text[i:s])
        nocom.append(text[i:s])
        if tok == "//":
            e = text.find("\n", s)
            e = n if e < 0 else e
            code.append(_blank(text[s:e]))
            nocom.append(_blank(text[s:e]))
        elif tok == "/*":
            depth, j = 1, m.end()
            while depth and j < n:
                b = _BLOCK.search(text, j)
                if not b:
                    j = n
                    break
                depth += 1 if b.group(0) == "/*" else -1
                j = b.end()
            e = j
            code.append(_blank(text[s:e]))
            nocom.append(_blank(text[s:e]))
        elif tok.endswith('"') and "r" in tok:
            close = '"' + (m.group(1) or "")
            k = text.find(close, m.end())
            e = n if k < 0 else k + len(close)
            body_end = e if k < 0 else k
            code.append(tok + _blank(text[m.end() : body_end]) + text[body_end:e])
            nocom.append(text[s:e])
        elif tok.endswith('"'):
            q = _STRING_END.match(text, m.end())
            e = q.end() if q else n
            code.append(tok + _blank(text[m.end() : e - 1]) + text[e - 1 : e])
            nocom.append(text[s:e])
        else:  # ' : a char literal or a lifetime / label
            j = m.end()
            if j < n and text[j] == "\\":
                q = _ESCAPED_CHAR.match(text, j)
                e = q.end() if q else j
            elif j + 1 < n and text[j + 1] == "'" and text[j] != "\n":
                e = j + 2
            else:
                e = j  # a lifetime: plain code
            if e > j:
                code.append(tok + _blank(text[j : e - 1]) + "'")
            else:
                code.append(tok)
            nocom.append(text[s:e] if e > j else tok)
        i = e
    out_code, out_nocom = "".join(code), "".join(nocom)
    assert len(out_code) == len(text) == len(out_nocom), "lexer must keep offsets"
    return out_code, out_nocom


# ------------------------------------------------------- cfg predicates


def _cfg_tokens(s: str) -> list[str]:
    return re.findall(r'"[^"]*"|[A-Za-z_][A-Za-z0-9_]*|[(),=]', s)


def _parse_pred(toks: list[str], i: int):
    """One cfg predicate from toks[i]; returns (tree, next index)."""
    name = toks[i]
    i += 1
    if i < len(toks) and toks[i] == "=":
        return ("kv", name, toks[i + 1].strip('"')), i + 2
    if i < len(toks) and toks[i] == "(":
        i += 1
        args = []
        while i < len(toks) and toks[i] != ")":
            a, i = _parse_pred(toks, i)
            args.append(a)
            if i < len(toks) and toks[i] == ",":
                i += 1
        return (name, args), i + 1
    return ("word", name), i


def test_only(pred) -> bool:
    """True when the cfg predicate holds only in a test build."""
    kind = pred[0]
    if kind == "word":
        return pred[1] == "test"
    if kind == "kv":
        return pred[1] == "feature" and pred[2] == "test-support"
    if kind == "all":
        return any(test_only(a) for a in pred[1])
    if kind == "any":
        return bool(pred[1]) and all(test_only(a) for a in pred[1])
    return False  # not(..) and anything else: production


def cfg_is_test_only(attr: str) -> bool:
    """`attr` is the text inside #[...]; True for cfg(test) and the like."""
    m = re.match(r"\s*cfg\s*\((.*)\)\s*$", attr, re.S)
    if not m:
        return False
    toks = _cfg_tokens(m.group(1))
    if not toks:
        return False
    try:
        pred, _ = _parse_pred(toks, 0)
    except IndexError:
        return False
    return test_only(pred)


# ------------------------------------------------------- Rust analysis


@dataclass
class RustFile:
    path: str
    text: str
    code: str
    nocom: str
    line_starts: list[int]
    test_ranges: list[tuple[int, int]] = field(default_factory=list)
    test_mods: list[str] = field(default_factory=list)  # `mod x;` under cfg(test)
    braces: dict[int, int] = field(default_factory=dict)

    def line(self, pos: int) -> int:
        return bisect.bisect_right(self.line_starts, pos)

    def in_test(self, pos: int) -> bool:
        return any(s <= pos < e for s, e in self.test_ranges)


_ATTR_START = re.compile(r"#!?\[")
_FN = re.compile(r"\bfn\s+(?:r#)?([A-Za-z_][A-Za-z0-9_]*)")
_PUB_FN = re.compile(
    r"\bpub(?:\s*\([^)]*\))?\s+(?:(?:const|async|unsafe|default)\s+|extern\s+\"[^\"]*\"\s+)*"
    r"fn\s+(?:r#)?([A-Za-z_][A-Za-z0-9_]*)"
)
_MOD_DECL = re.compile(r"\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
_ALLOW = re.compile(r"\b(?:allow|expect)\s*\(([^()]*(?:\([^()]*\)[^()]*)*)\)")
_REASON = re.compile(r'reason\s*=\s*"(?:[^"\\]|\\.)*"')
_DB_ACCESS = re.compile(
    r"\.conn\(\)|\bquery_row\(|\bquery_map\(|\.prepare(?:_cached)?\(|\brusqlite::|\bduckdb::|\.execute_batch\("
)


def _match_braces(code: str) -> dict[int, int]:
    pairs: dict[int, int] = {}
    stack: list[int] = []
    for m in re.finditer(r"[{}]", code):
        if m.group(0) == "{":
            stack.append(m.start())
        elif stack:
            pairs[stack.pop()] = m.start()
    return pairs


def _attr_end(code: str, start: int) -> int:
    """Offset just past the ']' closing the attribute at `start`."""
    depth = 0
    for j in range(start, len(code)):
        c = code[j]
        if c == "[":
            depth += 1
        elif c == "]":
            depth -= 1
            if depth == 0:
                return j + 1
    return len(code)


def _item_end(f: RustFile, pos: int) -> int:
    """End of the item that starts at or after `pos` (attributes skipped)."""
    code, n = f.code, len(f.code)
    j = pos
    while True:
        while j < n and code[j].isspace():
            j += 1
        if code.startswith("#[", j) or code.startswith("#![", j):
            j = _attr_end(code, j)
            continue
        break
    depth = 0
    while j < n:
        c = code[j]
        if c in "([":
            depth += 1
        elif c in ")]":
            if depth == 0:
                return j  # end of an enclosing list
            depth -= 1
        elif depth == 0:
            if c == ";":
                return j + 1
            if c == ",":
                return j + 1
            if c == "{":
                return f.braces.get(j, n - 1) + 1
            if c == "}":
                return j  # end of the enclosing block
        j += 1
    return n


def analyse_rust(path: str, text: str) -> RustFile:
    code, nocom = lex(text)
    starts = [0] + [m.end() for m in re.finditer(r"\n", text)]
    f = RustFile(path, text, code, nocom, starts)
    f.braces = _match_braces(code)
    for m in _ATTR_START.finditer(code):
        if f.in_test(m.start()):
            continue
        end = _attr_end(code, m.start())
        inner = nocom[m.end() : end - 1]
        if not cfg_is_test_only(inner):
            continue
        if code[m.start() + 1] == "!":  # #![cfg(test)]: the whole file
            f.test_ranges.append((0, len(code)))
            break
        item_end = _item_end(f, end)
        f.test_ranges.append((m.start(), item_end))
        decl = _MOD_DECL.match(code, end)
        if decl is None:
            # Further attributes may sit between the cfg and the `mod x;`.
            rest = code[end:item_end]
            decl = re.search(r"\bmod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;", rest)
        if decl and code[item_end - 1 : item_end] == ";":
            f.test_mods.append(decl.group(1))
    return f


def production_lines(f: RustFile) -> int:
    total = f.text.count("\n") + (0 if f.text.endswith("\n") or not f.text else 1)
    covered: set[int] = set()
    for s, e in f.test_ranges:
        covered.update(range(f.line(s), f.line(max(s, e - 1)) + 1))
    return max(0, total - len(covered))


def functions(f: RustFile) -> list[tuple[str, int, int]]:
    """(name, first line, length in lines) of each fn with a body, in production code."""
    out = []
    code, n = f.code, len(f.code)
    for m in _FN.finditer(code):
        if f.in_test(m.start()):
            continue
        depth, j, body = 0, m.end(), None
        while j < n:
            c = code[j]
            if c in "([":
                depth += 1
            elif c in ")]":
                depth -= 1
            elif depth <= 0 and c == ";":
                break  # a declaration without a body
            elif depth <= 0 and c == "{":
                body = j
                break
            j += 1
        if body is None or body not in f.braces:
            continue
        first, last = f.line(m.start()), f.line(f.braces[body])
        out.append((m.group(1), first, last - first + 1))
    return out


# ----------------------------------------------------------- collection


def _walk(root: str, exts: tuple[str, ...]):
    for d, dirs, files in os.walk(root):
        dirs[:] = sorted(x for x in dirs if x not in SKIP_DIRS)
        for name in sorted(files):
            if name.endswith(exts):
                yield os.path.join(d, name).replace(os.sep, "/")


def _read(path: str) -> str:
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def _child_dir(path: str) -> str:
    base = os.path.basename(path)
    if base in ("mod.rs", "lib.rs", "main.rs"):
        return os.path.dirname(path)
    return path[: -len(".rs")]


def measure(repo: str) -> dict:
    """Everything the baseline records, measured on the tree at `repo`."""
    cwd = os.getcwd()
    os.chdir(repo)
    try:
        return _measure()
    finally:
        os.chdir(cwd)


def _measure() -> dict:
    files = {p: analyse_rust(p, _read(p)) for p in _walk(RUST_ROOT, (".rs",))}

    # A module declared under cfg(test) is test code, file and directory alike.
    test_files: set[str] = set()
    test_dirs: list[str] = []

    def is_test(p: str) -> bool:
        return p in test_files or any(p.startswith(d) for d in test_dirs)

    for p in sorted(files):  # parents sort before their children
        f = files[p]
        if is_test(p):
            test_dirs.append(_child_dir(p) + "/")
            continue
        for name in f.test_mods:
            d = _child_dir(p)
            test_files.update({f"{d}/{name}.rs", f"{d}/{name}/mod.rs"})
            test_dirs.append(f"{d}/{name}/")

    result: dict = {k: {} for k in MAP_CHECKS}
    result["dead_public_functions"] = []
    result["frontend_files"] = {}
    where: dict[str, tuple[str, int]] = {}  # key -> (path, line) for annotations
    public: list[tuple[str, str, int]] = []

    for p in sorted(files):
        if is_test(p):
            continue
        f = files[p]
        n = production_lines(f)
        if n > RUST_FILE_LINES:
            result["rust_files"][p] = n
            where[f"rust_files:{p}"] = (p, 1)
        handler = p.startswith(HANDLER_DIRS)
        seen: Counter = Counter()
        for name, line, length in functions(f):
            seen[name] += 1
            key = f"{p}::{name}" + (f"#{seen[name]}" if seen[name] > 1 else "")
            if length > (HANDLER_FN_LINES if handler else RUST_FN_LINES):
                result["rust_functions"][key] = length
                where[f"rust_functions:{key}"] = (p, line)
        if handler:
            hits = [m for m in _DB_ACCESS.finditer(f.code) if not f.in_test(m.start())]
            if hits:
                result["handler_db_access"][p] = len(hits)
                where[f"handler_db_access:{p}"] = (p, f.line(hits[0].start()))
        for m in _ATTR_START.finditer(f.code):
            if f.in_test(m.start()):
                continue
            inner = f.nocom[m.end() : _attr_end(f.code, m.start()) - 1]
            for a in _ALLOW.finditer(inner):
                for lint in _REASON.sub("", a.group(1)).split(","):
                    lint = lint.strip()
                    if not lint or "=" in lint:  # reason = "..."
                        continue
                    key = f"{p}::{lint}"
                    result["lint_allows"][key] = result["lint_allows"].get(key, 0) + 1
                    where.setdefault(f"lint_allows:{key}", (p, f.line(m.start())))
        for m in _PUB_FN.finditer(f.code):
            if not f.in_test(m.start()):
                public.append((p, m.group(1), f.line(m.start())))

    # Every identifier in the code that could call a pub fn, comments excluded.
    words: Counter = Counter()
    ident = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
    for p, f in files.items():
        words.update(ident.findall(f.nocom))
    for root in REFERENCE_ROOTS:
        for p in _walk(root, (".rs", ".ts", ".tsx", ".toml", ".json")):
            if p in files or p.startswith(RUST_ROOT + "/"):
                continue
            text = _read(p)
            if p.endswith(".rs"):
                text = lex(text)[1]
            words.update(ident.findall(text))
    for p, name, line in public:
        if words[name] <= 1:
            key = f"{p}::{name}"
            result["dead_public_functions"].append(key)
            where[f"dead_public_functions:{key}"] = (p, line)
    result["dead_public_functions"].sort()

    for p in _walk(FRONTEND_ROOT, (".ts", ".tsx")):
        base = os.path.basename(p)
        if (
            p in GENERATED
            or ".test." in base
            or ".spec." in base
            or base.endswith(".d.ts")
            or "/__tests__/" in p
            or p.startswith("src/test/")
        ):
            continue
        n = len(_read(p).splitlines())
        if n > TS_FILE_LINES:
            result["frontend_files"][p] = n
            where[f"frontend_files:{p}"] = (p, 1)

    result["_where"] = where
    return result


# ------------------------------------------------------------- compare

FIX = {
    "rust_files": "split the file by responsibility into submodules; new code goes into a new submodule",
    "rust_functions": "split the function into named steps; a handler validates, calls one service and "
    "shapes the reply, so its logic moves into the service",
    "handler_db_access": "a handler calls a service; move the database work into a service or store function",
    "lint_allows": "fix the cause instead: a parameter struct for too_many_arguments, a boxed error for "
    "result_large_err, deletion for dead_code",
    "dead_public_functions": "delete it, or call it",
    "frontend_files": "split the component or module; a file ported from the web is marked \"web\" in the baseline",
}
UNIT = {
    "rust_files": f"lines of non-test code (limit {RUST_FILE_LINES})",
    "rust_functions": f"lines (limit {RUST_FN_LINES}; {HANDLER_FN_LINES} in an HTTP route or Tauri command)",
    "handler_db_access": "direct database calls (limit 0)",
    "lint_allows": "suppressions (limit 0)",
    "frontend_files": f"lines (limit {TS_FILE_LINES})",
}


@dataclass
class Finding:
    check: str
    key: str
    message: str
    stale: bool = False
    grew: bool = False


def compare(current: dict, baseline: dict) -> list[Finding]:
    out: list[Finding] = []
    for check in MAP_CHECKS:
        cur, base = current.get(check, {}), baseline.get(check, {})
        for key, n in sorted(cur.items()):
            b = base.get(key)
            if b is None:
                out.append(Finding(check, key, f"{key}: {n} {UNIT[check]}. Fix: {FIX[check]}."))
            elif n > b:
                out.append(
                    Finding(check, key, f"{key} grew to {n} {UNIT[check]}; the baseline is {b} and may only "
                            f"go down. Fix: {FIX[check]}.", grew=True)
                )
        for key, b in sorted(base.items()):
            n = cur.get(key, 0)
            if n < b:
                out.append(Finding(check, key, f"{key} is now {n}, below its baseline {b}.", stale=True))
    cur_dead, base_dead = set(current.get("dead_public_functions", [])), set(
        baseline.get("dead_public_functions", [])
    )
    for key in sorted(cur_dead - base_dead):
        out.append(Finding("dead_public_functions", key, f"{key} is public and nothing calls it. "
                           f"Fix: {FIX['dead_public_functions']}."))
    for key in sorted(base_dead - cur_dead):
        out.append(Finding("dead_public_functions", key, f"{key} is gone or now called.", stale=True))
    cur_fe, base_fe = current.get("frontend_files", {}), baseline.get("frontend_files", {})
    for key, n in sorted(cur_fe.items()):
        b = base_fe.get(key)
        if b == "web":
            continue
        if b is None:
            out.append(Finding("frontend_files", key, f"{key}: {n} {UNIT['frontend_files']}. "
                               f"Fix: {FIX['frontend_files']}."))
        elif n > b:
            out.append(Finding("frontend_files", key, f"{key} grew to {n} {UNIT['frontend_files']}; the "
                               f"baseline is {b} and may only go down. Fix: {FIX['frontend_files']}.", grew=True))
    for key, b in sorted(base_fe.items()):
        n = cur_fe.get(key, 0)
        if b == "web":
            if key not in cur_fe:
                out.append(Finding("frontend_files", key, f"{key} is gone or under the limit.", stale=True))
        elif n < b:
            out.append(Finding("frontend_files", key, f"{key} is now {n}, below its baseline {b}.", stale=True))
    return out


def lowered(current: dict, baseline: dict) -> dict:
    """The baseline with every entry lowered to the code; never adds or raises."""
    new: dict = {k: v for k, v in baseline.items() if k.startswith("_")}
    for check in MAP_CHECKS:
        cur = current.get(check, {})
        new[check] = {k: min(b, cur[k]) for k, b in baseline.get(check, {}).items() if cur.get(k, 0) > 0}
    cur_dead = set(current.get("dead_public_functions", []))
    new["dead_public_functions"] = sorted(k for k in baseline.get("dead_public_functions", []) if k in cur_dead)
    cur_fe = current.get("frontend_files", {})
    new["frontend_files"] = {
        k: (b if b == "web" else min(b, cur_fe[k]))
        for k, b in baseline.get("frontend_files", {}).items()
        if k in cur_fe
    }
    return new


def gained(baseline: dict, base: dict) -> list[str]:
    """Entries `baseline` added or raised against `base`, moves excepted.

    A move is a rename: an entry that left the same check with a value at
    least as large (or, for dead functions, the same function name).
    """
    out: list[str] = []
    for check in MAP_CHECKS + ("frontend_files",):
        now, was = baseline.get(check, {}), base.get(check, {})
        left = sorted((v for k, v in was.items() if k not in now), key=lambda v: (v == "web", v))
        for key, v in sorted(now.items()):
            if key in was and (v == was[key] or (v != "web" and was[key] != "web" and v <= was[key])):
                continue
            if key not in was:
                fit = next((x for x in left if x == v or (v != "web" and x != "web" and x >= v)), None)
                if fit is not None:
                    left.remove(fit)
                    continue
            out.append(f"{check}: {key} = {v}" + (f" (was {was[key]})" if key in was else " (new)"))
    now_d, was_d = set(baseline.get("dead_public_functions", [])), set(base.get("dead_public_functions", []))
    left_names = Counter(k.rsplit("::", 1)[-1] for k in was_d - now_d)
    for key in sorted(now_d - was_d):
        name = key.rsplit("::", 1)[-1]
        if left_names[name]:
            left_names[name] -= 1
            continue
        out.append(f"dead_public_functions: {key} (new)")
    return out


# ----------------------------------------------------------------- main


def _annotate(level: str, path: str, line: int, msg: str) -> None:
    if os.environ.get("GITHUB_ACTIONS") == "true":
        print(f"::{level} file={path},line={line},title=Code health::{msg}")
    else:
        print(f"{level}: {path}:{line}: {msg}")


def _load(path: str) -> dict:
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def _dump(path: str, data: dict) -> None:
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=2, sort_keys=True)
        fh.write("\n")


DEFAULT_DOC = (
    "Code-health ratchet: what was over a limit when the gate started, with its size then. "
    "Entries may only go down; `python3 scripts/ci/code_health.py --update` lowers them after "
    "an improvement. Adding or raising one needs the maintainer (the code-health-exception "
    "label). See CLAUDE.md, Code health."
)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default=".", help="repository root (default: the current directory)")
    ap.add_argument("--update", action="store_true", help="lower the baseline to the code")
    ap.add_argument("--base-ref", help="refuse entries added or raised against this ref's baseline")
    ap.add_argument("--init", action="store_true", help=argparse.SUPPRESS)  # maintainer only
    args = ap.parse_args(argv)

    current = measure(args.repo)
    where = current.pop("_where")
    path = os.path.join(args.repo, BASELINE)

    if args.init:
        data = dict(current)
        data["_doc"] = DEFAULT_DOC
        if os.path.exists(path):  # keep the files marked as ported from the web
            old = _load(path).get("frontend_files", {})
            for k in data["frontend_files"]:
                if old.get(k) == "web":
                    data["frontend_files"][k] = "web"
        _dump(path, data)
        print(f"Wrote {BASELINE} from the code as it is.")
        return 0

    baseline = _load(path)
    if args.update:
        baseline = lowered(current, baseline)
        _dump(path, baseline)
        print(f"Lowered {BASELINE} to the code; commit it with the change.")

    failures = 0
    findings = compare(current, baseline)
    for f in findings:
        p, line = where.get(f"{f.check}:{f.key}", (BASELINE, 1))
        if f.grew and EXISTING_MAY_GROW:
            _annotate("warning", p, line, f.message)
        elif f.stale:
            failures += 1
            _annotate(
                "error",
                BASELINE,
                1,
                f"{f.message} Run `python3 scripts/ci/code_health.py --update` and commit the "
                "baseline, so the improvement cannot be undone.",
            )
        else:
            failures += 1
            _annotate("error", p, line, f.message)

    if args.base_ref:
        try:
            raw = subprocess.run(
                ["git", "-C", args.repo, "show", f"{args.base_ref}:{BASELINE}"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
            base = json.loads(raw)
        except (subprocess.CalledProcessError, json.JSONDecodeError):
            base = None  # the baseline is new on this branch
        if base is not None:
            for g in gained(baseline, base):
                failures += 1
                _annotate(
                    "error",
                    BASELINE,
                    1,
                    f"The baseline gained or raised {g}. Only the maintainer may allow that, with "
                    "the code-health-exception label on the pull request.",
                )

    totals = {k: len(baseline.get(k, {})) for k in MAP_CHECKS + LIST_CHECKS + ("frontend_files",)}
    print("Code health baseline (items still to fix): " + ", ".join(f"{k} {v}" for k, v in totals.items()))
    if failures:
        print(f"Code health: {failures} problem(s). See CLAUDE.md, Code health.")
        return 1
    print("Code health: nothing new over the limits, and nothing grew.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
