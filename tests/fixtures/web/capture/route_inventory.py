"""Write route_inventory.json: every /api/v1 method and path OpenAlgo web
registers, read from its source (restx_api/__init__.py and each namespace's
Resource classes). No server is called.

    python3 -I tests/fixtures/web/capture/route_inventory.py /path/to/openalgo > tests/fixtures/web/route_inventory.json

The desktop test tests/it/api_v1_inventory.rs requires each entry to be
served, apart from the exclusions it names (DOC-06).
"""

import ast
import json
import pathlib
import subprocess
import sys


def routes(web: pathlib.Path) -> list[str]:
    root = web / "restx_api"
    init = ast.parse((root / "__init__.py").read_text())
    modules = {}
    for node in ast.walk(init):
        if isinstance(node, ast.ImportFrom) and node.level == 1:
            for alias in node.names:
                modules[alias.asname or alias.name] = node.module
    prefixes = {}
    for node in ast.walk(init):
        if isinstance(node, ast.Call) and getattr(node.func, "attr", "") == "add_namespace":
            path = next(k.value.value for k in node.keywords if k.arg == "path")
            prefixes[modules[node.args[0].id]] = path
    out = set()
    for module, prefix in prefixes.items():
        tree = ast.parse((root / f"{module}.py").read_text())
        for cls in (n for n in tree.body if isinstance(n, ast.ClassDef)):
            paths = [
                d.args[0].value
                for d in cls.decorator_list
                if isinstance(d, ast.Call) and getattr(d.func, "attr", "") == "route"
            ]
            methods = [
                f.name.upper()
                for f in cls.body
                if isinstance(f, ast.FunctionDef) and f.name in ("get", "post", "put", "delete", "patch")
            ]
            for p in paths:
                for m in methods:
                    out.add(f"{m} /api/v1{prefix}{'' if p in ('', '/') else p}")
    return sorted(out)


def main() -> None:
    web = pathlib.Path(sys.argv[1])
    commit = subprocess.run(
        ["git", "-C", str(web), "log", "-1", "--format=%h %cs", "--", "restx_api"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    doc = {
        "source": f"OpenAlgo web restx_api at {commit}",
        "generated_by": "tests/fixtures/web/capture/route_inventory.py",
        "routes": routes(web),
    }
    print(json.dumps(doc, indent=2))


if __name__ == "__main__":
    main()
