"""Record every web agent tool's spec as the desktop's contract fixture.

Builds each toolkit in the web's registry (`services/agent/tools/__init__.py`,
`TOOLKITS`) with agno, exactly as a web run does, and writes
`src-tauri/tests/fixtures/agent/tools.json`: per toolkit its key, agno name,
surfaces, required capabilities, order and description; per tool its name, the
description and JSON-schema arguments agno sends the model (`Function.to_dict`
after `process_entrypoint(strict=False)`), the surfaces it is offered on, its
toolkit and whether it needs the trader's approval; and the tool list each
surface sees, in the order the model sees it. It also writes
`src-tauri/src/agent/tools/specs.json`, the same specs for the toolkits this
version builds, which the desktop's tools are built from
(`agent::tools::specs`). Each desktop toolkit pins its
specs against this file (`agent::tools::spec_contract`), so a wording or
argument drift fails a test instead of reaching the model.

The web is never modified. Running it needs agno and its pinned dependencies,
which a scratch virtual environment outside the web repository provides (the
versions come from the web's `uv.lock`); every import of the web's database,
broker and service layer is replaced by an inert stand-in before it can run,
so nothing connects, reads `.env`, or writes a file. Bytecode is not written.

Usage, from the desktop repository root (the scratch venv holds agno==3.0.5,
pandas, numpy, pytz and their pinned dependencies):

    /path/to/scratch/venv/bin/python -I -B scripts/agent/capture_web_tools.py /path/to/openalgo

Regenerate only from the web; never edit the fixture by hand.
"""

from __future__ import annotations

import sys

sys.dont_write_bytecode = True

import importlib  # noqa: E402 - after the bytecode switch
import importlib.abc  # noqa: E402
import importlib.machinery  # noqa: E402
import importlib.metadata  # noqa: E402
import json  # noqa: E402
import logging  # noqa: E402
import queue  # noqa: E402
import subprocess  # noqa: E402
import threading  # noqa: E402
import time  # noqa: E402
import types  # noqa: E402
from pathlib import Path  # noqa: E402

#: Top-level web packages whose import would touch a database, a broker, the
#: network or the filesystem. Each is replaced by an inert stand-in.
_STUBBED_TOP = frozenset(
    {
        "database",
        "extensions",
        "blueprints",
        "broker",
        "sandbox",
        "restx_api",
        "websocket_proxy",
        "strategies",
        "upgrade",
        "app",
        "csp",
        "cors",
        "limiter",
    }
)

#: Web modules under `utils` that are pure data and load as they are.
_REAL_UTILS = frozenset({"utils", "utils.constants"})


class _Stub:
    """Anything: callable, indexable, iterable (empty), usable as a base."""

    def __init__(self, name: str) -> None:
        self._name = name

    def __getattr__(self, item: str) -> _Stub:
        if item.startswith("__") and item.endswith("__"):
            raise AttributeError(item)
        return _Stub(f"{self._name}.{item}")

    def __call__(self, *args: object, **kwargs: object) -> _Stub:
        return _Stub(f"{self._name}()")

    def __getitem__(self, item: object) -> _Stub:
        return _Stub(f"{self._name}[]")

    def __iter__(self):
        return iter(())

    def __or__(self, other: object) -> _Stub:
        return self

    __ror__ = __or__

    def __mro_entries__(self, bases: tuple) -> tuple:
        return (object,)

    def __repr__(self) -> str:
        return f"<stub {self._name}>"


class _StubModule(types.ModuleType):
    def __getattr__(self, item: str) -> object:
        if item.startswith("__") and item.endswith("__"):
            raise AttributeError(item)
        return _Stub(f"{self.__name__}.{item}")


class _StubLoader(importlib.abc.Loader):
    def create_module(self, spec: importlib.machinery.ModuleSpec) -> types.ModuleType:
        module = _StubModule(spec.name)
        module.__path__ = []  # a package, so submodules resolve to stubs too
        return module

    def exec_module(self, module: types.ModuleType) -> None:
        return None


class _ShimLoader(importlib.abc.Loader):
    """Small real stand-ins for web utilities with import-time side effects."""

    def create_module(self, spec: importlib.machinery.ModuleSpec) -> types.ModuleType | None:
        return None

    def exec_module(self, module: types.ModuleType) -> None:
        if module.__name__ == "utils.logging":
            module.get_logger = logging.getLogger
            module.redact_text = lambda text: text
        elif module.__name__ == "utils.real_threading":
            module.run_on_hub = lambda fn, *a, timeout=None, **k: fn(*a, **k)
            module.HubQueueFull = type("HubQueueFull", (RuntimeError,), {})
            module.is_eventlet_active = lambda: False
            for name in ("Lock", "RLock", "Event", "Thread", "Condition", "get_ident"):
                setattr(module, name, getattr(threading, name))
            module.Queue, module.Empty, module.Full = queue.Queue, queue.Empty, queue.Full
            module.sleep = time.sleep


_SHIMS = frozenset({"utils.logging", "utils.real_threading"})

#: Third-party packages the web imports that the tool specs do not need.
_MISSING_THIRD_PARTY = frozenset(
    {"werkzeug", "flask", "flask_socketio", "sqlalchemy", "litellm", "cachetools", "eventlet"}
)


class _WebIsolation(importlib.abc.MetaPathFinder):
    """Let the agent package load for real; stand in for everything heavy."""

    def find_spec(self, fullname, path, target=None):
        if fullname in _SHIMS:
            return importlib.machinery.ModuleSpec(fullname, _ShimLoader())
        top = fullname.split(".")[0]
        if fullname in ("services", "services.agent") or fullname.startswith("services.agent."):
            return None
        if fullname in _REAL_UTILS:
            return None
        if top in _STUBBED_TOP or top in ("services", "utils"):
            return importlib.machinery.ModuleSpec(fullname, _StubLoader(), is_package=True)
        # Third-party packages the scratch environment does not carry.
        if top in _MISSING_THIRD_PARTY:
            return importlib.machinery.ModuleSpec(fullname, _StubLoader(), is_package=True)
        return None


#: Toolkits the desktop does not build in this version (maintainer,
#: 2026-10-27): Python strategy generation, and Flow, which comes later.
_NOT_IN_THIS_VERSION = frozenset({"strategy_gen", "flow_gen"})


def _surface_names(spec, registry) -> list[str]:
    return [s for s in (registry.SURFACE_CHAT, registry.SURFACE_CHART, registry.SURFACE_VOICE) if s in spec.surfaces]


def _toolkit_tools(registry, spec) -> list[dict]:
    cls = spec.resolve()
    context = registry.ToolContext(
        api_key="capture-only",
        conversation_id=1,
        surface=_surface_names(spec, registry)[0],
        run_id="capture",
        session_id="capture",
        user_id="capture",
        trading_enabled=True,
        web_search_enabled=True,
        mcp_enabled=True,
    )
    toolkit = cls(context)
    functions = toolkit.get_functions()
    tools = []
    for name, function in functions.items():
        function.process_entrypoint(strict=False)
        exposed = function.to_dict()
        tools.append(
            {
                "name": exposed["name"],
                "toolkit": spec.key,
                "agno_toolkit": toolkit.name,
                "description": exposed.get("description", ""),
                "parameters": exposed["parameters"],
                "needs_approval": bool(exposed.get("requires_confirmation", False)),
                "surfaces": _surface_names(spec, registry),
                "requires": sorted(spec.requires),
            }
        )
        if name != exposed["name"]:
            raise SystemExit(f"{spec.key}: function {name} is exposed as {exposed['name']}")
    return tools


def main(web_root: str) -> None:
    root = Path(web_root).resolve()
    sys.meta_path.insert(0, _WebIsolation())
    sys.path.insert(0, str(root))
    logging.disable(logging.CRITICAL)
    registry = importlib.import_module("services.agent.tools")
    agno_version = importlib.metadata.version("agno")

    toolkits = []
    tools = []
    for spec in sorted(registry.TOOLKITS, key=lambda s: (s.order, s.key)):
        built = _toolkit_tools(registry, spec)
        toolkits.append(
            {
                "key": spec.key,
                "class": spec.attr,
                "module": spec.module,
                "order": spec.order,
                "surfaces": _surface_names(spec, registry),
                "requires": sorted(spec.requires),
                "description": spec.description,
                "tools": [t["name"] for t in built],
            }
        )
        tools.extend(built)

    by_surface = {
        surface: [t["name"] for t in tools if surface in t["surfaces"]]
        for surface in (registry.SURFACE_CHAT, registry.SURFACE_CHART, registry.SURFACE_VOICE)
    }
    try:
        commit = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--short=8", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        commit = "unknown"

    fixture = {
        "_comment": (
            "Captured from the web's services/agent/tools by scripts/agent/capture_web_tools.py "
            "with agno building each toolkit as a web run does. Regenerate only from the web; "
            "the desktop's tool specs must match this file."
        ),
        "web_commit": commit,
        "agno_version": agno_version,
        "capabilities": sorted(registry.CAPABILITIES),
        "toolkits": toolkits,
        "tools": tools,
        "surfaces": by_surface,
        "approval": [t["name"] for t in tools if t["needs_approval"]],
    }
    repo = Path(__file__).resolve().parents[2]
    out = repo / "src-tauri/tests/fixtures/agent/tools.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(fixture, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    # The specs the desktop builds its tools from (agent::tools::specs): every
    # tool of a toolkit in this version, as the model is sent it.
    shipped = {
        "_comment": (
            "Generated from the web by scripts/agent/capture_web_tools.py: what the model is "
            "sent for each Agent tool. Never edit by hand."
        ),
        "web_commit": commit,
        "tools": {
            t["name"]: {
                "description": t["description"],
                "parameters": t["parameters"],
                "needs_approval": t["needs_approval"],
            }
            for t in tools
            if t["toolkit"] not in _NOT_IN_THIS_VERSION
        },
    }
    specs = repo / "src-tauri/src/agent/tools/specs.json"
    specs.write_text(json.dumps(shipped, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(
        f"wrote {out}: {len(toolkits)} toolkits, {len(tools)} tools, "
        f"chat {len(by_surface['chat'])}, chart {len(by_surface['chart'])}, "
        f"voice {len(by_surface['voice'])}, approval {len(fixture['approval'])}"
    )


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: capture_web_tools.py /path/to/openalgo")
    main(sys.argv[1])
