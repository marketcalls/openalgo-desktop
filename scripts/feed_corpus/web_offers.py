#!/usr/bin/env python3
"""What the web offers each broker's clients, read from the web's own registries.

`web_offers.json` beside the corpus records, per broker of the catalog, the
depth levels the web streams and whether it delivers order updates:

- depth levels: the broker's capability registry in
  `broker/<broker>/streaming/*mapping*.py` (`get_supported_depth_levels`,
  `SUPPORTED_DEPTH_LEVELS` or `depth_support`), asked for every exchange;
- order updates: `_BROKER_FACTORIES` and `_POLLING_BROKERS` in
  `services/order_update_service.py`.

A broker whose web tree has no such registry keeps the levels its
`broker.json` cites, and is marked `"source": "by-hand"` so the count of
those stays visible. `web_expected.py` writes this file and fails `--check`
when it is stale; the parity matrix and the release gate read it (they run
where there is no web tree). Standard library only.
"""

from __future__ import annotations

import ast
import json
from pathlib import Path

import parity_matrix as pm
from shadow import Imported, exec_module

# Levels the matrix has a column for: a registry's level 1 is a best bid and
# ask, not one of the web's depth modes.
DEPTH_COLUMNS = (5, 20, 30, 50)
EXCHANGES = ("NSE", "NFO", "BSE", "BFO", "MCX", "CDS", "BCD", "NSE_INDEX", "BSE_INDEX", "MCX_INDEX", "CRYPTO")
ORDER_SERVICE = "services/order_update_service.py"


def offers_path() -> Path:
    return pm.CORPUS / "web_offers.json"


def _module_namespace(path: Path) -> dict:
    """The classes and constants of a web module (`shadow.exec_module`)."""
    ns: dict = {"__name__": "web_registry"}
    exec_module(path, ns)
    return ns


def _declared(registry: type, name: str):
    """A registry's own attribute `name` (a stubbed base class has none)."""
    for cls in registry.__mro__:
        if cls not in (Imported, object) and name in cls.__dict__:
            return getattr(registry, name)
    return None


def _levels_of(registry: type) -> set[int] | None:
    """The depth levels a capability registry declares, on any exchange;
    `None` when it declares none."""
    getter = _declared(registry, "get_supported_depth_levels")
    if getter is not None:
        found: set[int] = set()
        for exchange in EXCHANGES:
            try:
                got = getter(exchange)
            except TypeError:
                got = getter()
            found |= set(got or [])
        return found
    table = _declared(registry, "SUPPORTED_DEPTH_LEVELS") or _declared(registry, "depth_support")
    if table is None:
        return None
    if isinstance(table, dict):
        return {level for levels in table.values() for level in levels}
    return set(table)


def registry_levels(web: Path, broker: str) -> set[int] | None:
    """The levels the broker's web registry declares; `None` with no registry."""
    for path in sorted((web / "broker" / broker / "streaming").glob("*mapping*.py")):
        ns = _module_namespace(path)
        for value in ns.values():
            if isinstance(value, type) and value.__name__.endswith("CapabilityRegistry"):
                levels = _levels_of(value)
                if levels is not None:
                    return levels
    return None


def order_brokers(web: Path) -> set[str]:
    """The brokers the web delivers order updates for, pushed or polled."""
    tree = ast.parse((web / ORDER_SERVICE).read_text(encoding="utf-8"))
    found: set[str] = set()
    for node in tree.body:
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            target = node.target if isinstance(node, ast.AnnAssign) else node.targets[0]
            if getattr(target, "id", "") in ("_BROKER_FACTORIES", "_POLLING_BROKERS"):
                found |= set(ast.literal_eval(node.value))
    if not found:
        raise SystemExit(f"{ORDER_SERVICE}: the order-update registries were not found")
    return found


def by_hand_levels(broker: str) -> set[int]:
    """The levels a broker with no web registry cites in its broker.json."""
    meta = json.loads((pm.CORPUS / broker / "broker.json").read_text(encoding="utf-8"))
    depth = meta.get("web", {}).get("depth", [])
    return {d for v in depth.values() for d in v} if isinstance(depth, dict) else set(depth)


def compute(web: Path) -> dict:
    orders = order_brokers(web)
    brokers = {}
    for broker in pm.brokers():
        levels = registry_levels(web, broker)
        source = "registry" if levels is not None else "by-hand"
        if levels is None:
            levels = by_hand_levels(broker)
        brokers[broker] = {
            "depth": sorted(level for level in levels if level in DEPTH_COLUMNS),
            "orders": broker in orders,
            "source": source,
        }
    return {
        "about": "What the web offers each broker's clients, from its own registries. Written by "
                 "scripts/feed_corpus/web_expected.py at the commit pinned in web_oracle.json.",
        "brokers": brokers,
    }


def text_of(offers: dict) -> str:
    return json.dumps(offers, indent=2) + "\n"


def write(web: Path) -> None:
    offers_path().write_text(text_of(compute(web)), encoding="utf-8")


def stale(web: Path) -> bool:
    path = offers_path()
    return not path.exists() or path.read_text(encoding="utf-8") != text_of(compute(web))
