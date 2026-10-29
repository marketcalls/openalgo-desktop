"""Regenerate the Playground endpoint lists from the OpenAlgo web collections.

The web's /playground/endpoints parses Bruno .bru files at request time. The
desktop ships the parsed result instead, so the .bru files (some of which
carry sample API keys) never enter this repository. The parser used here is
the web's own (blueprints/playground.py), loaded from the web checkout, so the
output is exactly what the web would serve; API keys are cleared by it.

Usage (from the repository root):
    python3 scripts/gen_playground_endpoints.py ../openalgo

Writes src-tauri/resources/playground/endpoints_<broker_type>.json.
"""

import ast
import glob
import json
import logging
import os
import re
import sys
from collections import OrderedDict

#: Endpoints for features the desktop does not ship (CLAUDE.md, Scope decisions).
EXCLUDED_PATH_PREFIXES = (
    "/api/v1/python",
    "/api/v1/backtest",
    "/api/v1/portfolio/backtest",
    "/api/v1/sip",
)


def load_web_parser(web_root):
    path = os.path.join(web_root, "blueprints", "playground.py")
    with open(path, encoding="utf-8") as f:
        tree = ast.parse(f.read())
    wanted = {"parse_bru_file", "categorize_endpoint", "load_bruno_endpoints"}
    body = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name in wanted]
    ns = {
        "re": re,
        "json": json,
        "glob": glob,
        "os": os,
        "OrderedDict": OrderedDict,
        "logger": logging.getLogger("playground"),
        "__file__": path,
    }
    exec(compile(ast.Module(body=body, type_ignores=[]), path, "exec"), ns)
    return ns["load_bruno_endpoints"]


def main():
    web_root = sys.argv[1] if len(sys.argv) > 1 else "../openalgo"
    load = load_web_parser(web_root)
    out_dir = os.path.join("src-tauri", "resources", "playground")
    os.makedirs(out_dir, exist_ok=True)
    for broker_type in ("IN_stock", "crypto"):
        endpoints = load(broker_type=broker_type)
        for category, items in endpoints.items():
            endpoints[category] = [
                e
                for e in items
                if not str(e.get("path", "")).startswith(EXCLUDED_PATH_PREFIXES)
            ]
        target = os.path.join(out_dir, f"endpoints_{broker_type}.json")
        with open(target, "w", encoding="utf-8") as f:
            json.dump(endpoints, f, indent=2, sort_keys=False)
            f.write("\n")
        print(target, {k: len(v) for k, v in endpoints.items()})


if __name__ == "__main__":
    main()
