"""Which run-time sentences of the web's executor appear in no golden.

Sentences are taken two ways:
1. from the executor source: every string or f-string passed to self.log(),
   raised in an exception, or returned under "message"/"error";
2. from parity.md sections 3 and 4: every backticked item that one of those
   source sentences also matches (so code references are left out).
A sentence is covered when its pattern (f-string fields as wildcards) is found
in a golden's logs, outputs, errors, abort text or desktop_expected.

Usage: python3 sentences.py <goldens_dir> <web_dir> <parity.md>
"""

from __future__ import annotations

import ast
import glob
import json
import re
import sys

SOURCES = [
    ("services/flow_executor_service.py", None),
    ("services/indicator_service.py", {"compute_indicator", "prior_period_ohlc", "_resolve_params"}),
    ("services/strategy_pnl_service.py", {"get_strategy_pnl"}),
]


def pattern(node) -> str | None:
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        parts = [re.escape(node.value)]
    elif isinstance(node, ast.JoinedStr):
        parts = []
        for v in node.values:
            if isinstance(v, ast.Constant):
                parts.append(re.escape(v.value))
            else:
                parts.append(".*?")
    elif isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        a, b = pattern(node.left), pattern(node.right)
        return a + b if a is not None and b is not None else None
    else:
        return None
    text = "".join(parts)
    literal = re.sub(r"\.\*\?|\\", "", text)
    if len(re.findall(r"[A-Za-z]", literal)) < 6:
        return None
    return text


def source_sentences(web: str):
    out = []
    for rel, only in SOURCES:
        tree = ast.parse(open(f"{web}/{rel}").read())
        for fn in ast.walk(tree):
            if not isinstance(fn, (ast.FunctionDef, ast.Module)):
                continue
            if only is not None and (not isinstance(fn, ast.FunctionDef) or fn.name not in only):
                continue
            for n in ast.walk(fn):
                cands = []
                if isinstance(n, ast.Call):
                    f = n.func
                    name = f.attr if isinstance(f, ast.Attribute) else getattr(f, "id", "")
                    if name == "log" and n.args:
                        cands.append(n.args[0])
                    if name in ("ValueError", "Exception", "LotSizeUnavailable", "KeyError", "TypeError") and n.args:
                        cands.append(n.args[0])
                if isinstance(n, ast.Dict):
                    for k, v in zip(n.keys, n.values):
                        if isinstance(k, ast.Constant) and k.value in ("message", "error"):
                            cands.append(v)
                if isinstance(n, ast.Assign) and len(n.targets) == 1 and isinstance(n.targets[0], ast.Name) \
                        and n.targets[0].id in ("message", "error_msg"):
                    cands.append(n.value)
                for c in cands:
                    p = pattern(c)
                    if p:
                        out.append((f"{rel}:{c.lineno}", p))
    seen = set()
    uniq = []
    for loc, p in out:
        if p not in seen:
            seen.add(p)
            uniq.append((loc, p))
    return uniq


def corpus(goldens: str) -> str:
    texts = []

    def walk(v):
        if isinstance(v, str):
            texts.append(v)
        elif isinstance(v, dict):
            for x in v.values():
                walk(x)
        elif isinstance(v, list):
            for x in v:
                walk(x)

    for f in glob.glob(f"{goldens}/*/*.json"):
        g = json.load(open(f))
        walk(g.get("expected"))
        walk(g.get("desktop_expected"))
    return "\n".join(texts)


def main():
    goldens, web, parity = sys.argv[1:4]
    text = corpus(goldens)
    missing = []
    for loc, p in source_sentences(web):
        if not re.search(p, text):
            missing.append((loc, p))
    # parity sentences of sections 3-4 that are run-time text
    lines = open(parity).read().splitlines()
    s = next(i for i, l in enumerate(lines) if l.startswith("## 3. Nodes"))
    e = next(i for i, l in enumerate(lines) if l.startswith("## 5. Validator"))
    par_missing = []
    for l in lines[s:e]:
        m = re.match(r"^- \[[ x]\] (\S+)", l)
        if not m:
            continue
        for q in re.findall(r"`([^`]+)`", l):
            if len(re.findall(r"[A-Za-z]", q)) < 8 or " " not in q:
                continue
            pat = re.escape(q)
            pat = re.sub(r"\\\{[^{}]*?\\\}", ".*?", pat)
            if re.search(pat, text):
                continue
            par_missing.append((m.group(1), q))
    print(f"source sentences not in any golden: {len(missing)}")
    for loc, p in missing:
        print(f"  {loc}  {p[:150]}")
    print(f"parity.md 3-4 backticked texts not found (includes code refs; review): {len(par_missing)}")
    for pid, q in par_missing:
        print(f"  {pid}\t{q[:150]}")


if __name__ == "__main__":
    main()
