#!/usr/bin/env python3
"""Record what OpenAlgo web's Flow validator answers, for the desktop's port.

The desktop validates Flow workflows in Rust (``src-tauri/src/flow/validator``
and ``src-tauri/src/flow/node_contracts.rs``). Every error must be the one the
web returns: the same ``path``, ``code``, ``message``, ``expected`` and
``received``, in the same order. This script runs the web's own
``services/flow_workflow_validator.py`` and ``services/flow_node_contracts.py``
over a corpus that reaches every rule, and writes what they return to
``validator_goldens.json``:

* ``validate``: one entry per payload, with the full error list at each
  level it is checked at (``complete``, ``draft``, ``unnamed``: see
  ``LEVELS``), or the exception the web raises there (an unhashable value it
  cannot look up answers a 500 there).
* ``migrate``: ``migrate_legacy_node_data`` over legacy node payloads.
* ``trigger_config``: ``trigger_config`` over graphs, and whether pairs of
  them compare equal.
* ``contracts``: the node-contract helpers (expiry parsing and selection,
  underlying splitting, statuses, default product, price conditions, the
  offset and expiry patterns).

Run it with the web's own interpreter against a read-only checkout. It reads
the checkout's committed ``--ref`` only (``git archive`` into a temporary
folder, so uncommitted work there never reaches the goldens), records that
commit as ``web_commit``, and writes nothing in the checkout:

    PYTHONDONTWRITEBYTECODE=1 ../openalgo/.venv/bin/python \\
        scripts/gen_flow_validator_goldens.py --web ../openalgo \\
        --ref 96e5927e5 --out src-tauri/tests/fixtures/flow

Two places iterate a Python ``set``, whose order varies between processes:
the cycle search's start node and the positive-number fields. The desktop
uses the order the nodes and fields are listed in, so the web's validator is
run with those two iterations pinned to that order; nothing else is changed.
Payloads pass through ``json.dumps(sort_keys=True)`` first, so the web sees
the keys in the order the desktop's JSON maps keep them. Cases where the
desktop deliberately answers differently (a web defect fixed) are left out
here (the script lists them) and pinned by hand in
``src/flow/validator/tests/rules.rs``. Every JSON example in the web's
``docs/prompt/flow-import-format.md`` is recorded too, wrapped as the web's
own doc-example test wraps it.
"""

from __future__ import annotations

import argparse
import io
import json
import math
import os
import re
import subprocess
import sys
import tarfile
import tempfile
from datetime import datetime
from pathlib import Path

AT = {"x": 0, "y": 0}


def node(node_id, node_type, data=None, **extra):
    entry = {"id": node_id, "type": node_type, "position": AT, "data": {} if data is None else data}
    entry.update(extra)
    return entry


def edge(edge_id, source, target, **extra):
    return {"id": edge_id, "source": source, "target": target, **extra}


def wf(nodes, edges=(), name="W"):
    return {"name": name, "nodes": list(nodes), "edges": list(edges)}


def single(node_type, data):
    """A start trigger wired to one action node."""
    return wf([node("t", "start"), node("a", node_type, data)], [edge("e", "t", "a")])


LEVELS = {
    "complete": {"require_name": True, "strict": True},
    "draft": {"require_name": False, "strict": False},
    "unnamed": {"require_name": False, "strict": True},
}

# Values that exercise Python's printing and parsing in every message.
ODD_VALUES = [
    None,
    True,
    False,
    0,
    -1,
    1.5,
    0.0,
    1e16,
    1e-05,
    "",
    "  ",
    "abc",
    "it's",
    'say "hi"',
    "1_000",
    " 2 ",
    "Infinity",
    "-1e3",
    ["x"],
    {"k": 1},
    "{{webhook.v}}",
]


def shape_cases():
    yield "payload_list", ["not", "an", "object"], ("complete",)
    for i, payload in enumerate(["text", 5, 1.5, None, True]):
        yield f"payload_scalar_{i}", payload, ("complete",)
    yield (
        "invented_schema",
        {"strategy": "Gap fill", "settings": {}, "variables": {}, "flow": []},
        ("complete", "draft"),
    )
    for i, name in enumerate([None, "", "   ", 5, ["W"], "　"]):
        payload = {"nodes": [], "edges": []}
        if name is not None or i:
            payload["name"] = name
        yield f"name_{i}", payload, ("complete", "draft", "unnamed")
    for key in ("nodes", "edges"):
        for i, value in enumerate([None, "x", {"a": 1}, 3, True]):
            payload = {"name": "W", "nodes": [], "edges": []}
            payload[key] = value
            yield f"{key}_not_list_{i}", payload, ("complete", "draft")
        missing = {"name": "W", "nodes": [], "edges": []}
        del missing[key]
        yield f"{key}_missing", missing, ("complete", "draft")
    many = [node(f"n{i}", "log") for i in range(501)]
    yield "too_many_nodes", wf(many), ("draft",)
    two = [node("a", "start"), node("b", "log")]
    yield "too_many_edges", wf(two, [edge(f"e{i}", "a", "b") for i in range(1001)]), ("draft",)


def node_shape_cases():
    for i, value in enumerate([5, None, "node", ["a"], True]):
        yield f"node_not_object_{i}", wf([node("t", "start"), value]), ("complete", "draft")
    for i, node_id in enumerate([None, "", "  ", 5, True, 1.5, ["a"]]):
        n = node("x", "log")
        if node_id is None and i == 0:
            del n["id"]
        else:
            n["id"] = node_id
        yield f"node_id_{i}", wf([node("t", "start"), n], [edge("e", "t", "x")]), ("complete", "draft")
    yield (
        "node_id_duplicate",
        wf([node("a", "start"), node("a", "log"), node("a", "log")]),
        ("complete", "draft"),
    )
    for i, node_type in enumerate([None, "", " ", 5, ["log"], "NotARealNode", "Start", "log "]):
        n = node("x", "log")
        if node_type is None:
            del n["type"]
        else:
            n["type"] = node_type
        yield f"node_type_{i}", wf([node("t", "start"), n], [edge("e", "t", "x")]), ("complete", "draft")
    for i, data in enumerate(["missing", None, [], "x", 3]):
        n = node("x", "log")
        if data == "missing":
            del n["data"]
        else:
            n["data"] = data
        yield f"node_data_{i}", wf([node("t", "start"), n], [edge("e", "t", "x")]), ("complete", "draft")
    for i, position in enumerate(
        ["missing", None, [], {"x": 0}, {"x": "1", "y": True}, {"x": 1.5, "y": None}]
    ):
        n = node("x", "log")
        if position == "missing":
            del n["position"]
        else:
            n["position"] = position
        yield f"node_position_{i}", wf([node("t", "start"), n], [edge("e", "t", "x")]), ("complete", "draft")
    yield "start_without_id", wf([{"type": "start", "position": AT, "data": {}}]), ("complete",)
    yield (
        "two_triggers_bad_id",
        wf([node("a", "start"), {"id": 7, "type": "webhookTrigger", "position": AT, "data": {}}]),
        ("complete",),
    )


def edge_cases():
    base = [node("a", "start"), node("b", "log")]
    for i, value in enumerate([5, None, "e", ["a", "b"]]):
        yield f"edge_not_object_{i}", wf(base, [edge("e0", "a", "b"), value]), ("complete", "draft")
    for i, edge_id in enumerate(["missing", None, "", " ", 3]):
        e = edge("e", "a", "b")
        if edge_id == "missing":
            del e["id"]
        else:
            e["id"] = edge_id
        yield f"edge_id_{i}", wf(base, [e]), ("complete", "draft")
    yield "edge_id_duplicate", wf(base, [edge("e", "a", "b"), edge("e", "a", "b")]), ("complete", "draft")
    for endpoint in ("source", "target"):
        for i, value in enumerate(["missing", None, "", 5, "ghost", " a"]):
            e = edge("e", "a", "b")
            if value == "missing":
                del e[endpoint]
            else:
                e[endpoint] = value
            yield f"edge_{endpoint}_{i}", wf(base, [e]), ("complete", "draft")
    yield (
        "edge_no_valid_nodes",
        wf([{"type": "log", "position": AT, "data": {}}], [edge("e", "x", "y")]),
        ("draft",),
    )


def handle_cases():
    condition = {"leftValue": "{{x}}", "operator": ">"}
    for source_type, data in [
        ("varCondition", condition),
        ("priceAlert", {"symbol": "X", "exchange": "NSE", "condition": "above", "price": 1}),
        ("andGate", {"inputCount": 1}),
        ("getQuote", {"symbol": "X", "exchange": "NSE"}),
        ("start", {}),
        ("group", {}),
    ]:
        for i, handle in enumerate(
            ["true", "false", "yes", "no", "maybe", "True", "output-9", "", None, 5, True]
        ):
            e = edge("e2", "c", "d")
            if handle is not None:
                e["sourceHandle"] = handle
            nodes = [node("t", "webhookTrigger"), node("c", source_type, data), node("d", "log")]
            if source_type in ("start", "priceAlert"):
                nodes = [node("c", source_type, data), node("d", "log")]
                edges = [e]
            else:
                edges = [edge("e1", "t", "c"), e]
            yield f"handle_{source_type}_{i}", wf(nodes, edges), ("complete", "draft")
    untyped = {"id": "c", "position": AT, "data": {}}
    yield (
        "handle_untyped_source",
        wf([node("t", "start"), untyped], [edge("e", "t", "c", sourceHandle="x")]),
        ("draft",),
    )
    numbered = {"id": "c", "type": 5, "position": AT, "data": {}}
    yield (
        "handle_numeric_type_source",
        wf([node("t", "start"), numbered], [edge("e", "t", "c", sourceHandle="x")]),
        ("draft",),
    )
    yield (
        "handle_dangling_source",
        wf([node("t", "start")], [edge("e", "ghost", "t", sourceHandle="x")]),
        ("draft",),
    )
    duplicated = [node("t", "start"), node("c", "varCondition", condition), node("c", "log")]
    yield (
        "handle_duplicate_id_last_type",
        wf(duplicated, [edge("e", "c", "t", sourceHandle="true")]),
        ("draft",),
    )


def gate_cases():
    for gate in ("andGate", "orGate", "notGate"):
        for i, count in enumerate(
            ["missing", None, "", 3, "3", " 4 ", 0, -1, 2.7, True, "abc", "2.5", [3], 1000, 1001]
        ):
            data = {}
            if count != "missing":
                data["inputCount"] = count
            for j, handle in enumerate(
                ["input-0", "input-1", "input-2", "input-5", "input-01", "in-1", "input-x", 5, "", None]
            ):
                e = edge("e2", "s", "g")
                if handle is not None:
                    e["targetHandle"] = handle
                if gate != "andGate" and i > 3:
                    continue
                if count in (1000, 1001) and handle not in ("input-5", "input-01"):
                    continue
                nodes = [node("t", "start"), node("s", "log"), node("g", gate, data)]
                yield f"gate_{gate}_{i}_{j}", wf(nodes, [edge("e1", "t", "s"), e]), ("draft",)
    for j, handle in enumerate(["input-1", "input-12", "output-1", 0]):
        nodes = [
            node("t", "start"),
            node("s", "log"),
            node("d", "getQuote", {"symbol": "X", "exchange": "NSE"}),
        ]
        yield f"gate_non_gate_{j}", wf(nodes, [edge("e", "s", "d", targetHandle=handle)]), ("draft",)
    yield (
        "gate_unknown_target",
        wf([node("t", "start")], [edge("e", "t", "ghost", targetHandle="input-1")]),
        ("draft",),
    )
    nameless_gate = {"type": "andGate", "position": AT, "data": {"inputCount": 2}}
    no_target = {"id": "e", "source": "t", "targetHandle": "input-5"}
    yield "gate_nameless_gate_and_edge", wf([node("t", "start"), nameless_gate], [no_target]), ("draft",)
    numbered_gate = {"id": 1, "type": "orGate", "position": AT, "data": {"inputCount": 3}}
    yield (
        "gate_numeric_id_matches_true",
        wf(
            [node("t", "start"), numbered_gate],
            [{"id": "e", "source": "t", "target": True, "targetHandle": "input-4"}],
        ),
        ("draft",),
    )
    yield (
        "gate_data_not_object",
        wf([node("t", "start"), node("g", "andGate", "x")], [edge("e", "t", "g", targetHandle="input-0")]),
        ("draft",),
    )


def trigger_and_graph_cases():
    yield "no_nodes", wf([]), ("complete", "draft", "unnamed")
    yield "no_trigger", wf([node("a", "log"), node("b", "log")]), ("complete", "draft")
    yield "only_non_objects", wf([5]), ("complete",)
    yield (
        "multiple_triggers",
        wf(
            [
                node("a", "start"),
                node("b", "priceAlert", {"symbol": "X", "exchange": "NSE", "condition": "above", "price": 1}),
                node("c", "webhookTrigger"),
            ]
        ),
        ("complete", "draft"),
    )
    for trigger in ("start", "webhookTrigger", "priceAlert", "orderUpdateTrigger"):
        yield f"lone_trigger_{trigger}", wf([node("t", trigger)]), ("complete",)
    chain = [node("t", "start"), node("a", "log"), node("b", "log"), node("c", "log")]
    yield (
        "cycle_back_edge",
        wf(chain, [edge("e1", "t", "a"), edge("e2", "a", "b"), edge("e3", "b", "c"), edge("e4", "c", "a")]),
        ("complete",),
    )
    yield "cycle_self_loop", wf(chain[:2], [edge("e1", "t", "a"), edge("e2", "a", "a")]), ("complete",)
    yield (
        "cycle_detached",
        wf(chain, [edge("e1", "t", "a"), edge("e2", "b", "c"), edge("e3", "c", "b")]),
        ("complete",),
    )
    ring = [node("t", "start")] + [node(f"r{i}", "log") for i in range(9)]
    ring_edges = [edge("e0", "t", "r0")] + [edge(f"e{i + 1}", f"r{i}", f"r{(i + 1) % 9}") for i in range(9)]
    yield "cycle_long", wf(ring, ring_edges), ("complete",)
    yield "cycle_into_trigger", wf(chain[:2], [edge("e1", "t", "a"), edge("e2", "a", "t")]), ("complete",)
    orphans = [node("t", "start")] + [node(f"o{i}", "log") for i in range(11)]
    yield "unreachable_many", wf(orphans), ("complete",)
    yield (
        "unreachable_group_exempt",
        wf([node("t", "start"), node("g", "group"), node("a", "log")], [edge("e", "t", "a")]),
        ("complete",),
    )
    yield (
        "unreachable_reverse_edge",
        wf([node("t", "start"), node("a", "log")], [edge("e", "a", "t")]),
        ("complete",),
    )
    diamond = [node("t", "start"), node("a", "log"), node("b", "log"), node("c", "log")]
    yield (
        "diamond",
        wf(diamond, [edge("e1", "t", "a"), edge("e2", "t", "b"), edge("e3", "a", "c"), edge("e4", "b", "c")]),
        ("complete",),
    )
    long_chain = [node("t", "start")] + [node(f"c{i}", "log") for i in range(499)]
    long_edges = [edge("e0", "t", "c0")] + [edge(f"e{i}", f"c{i - 1}", f"c{i}") for i in range(1, 499)]
    yield "chain_500", wf(long_chain, long_edges), ("complete",)
    yield (
        "graph_skipped_after_errors",
        wf(chain, [edge("e1", "t", "a"), edge("e2", "a", "a"), edge("e3", "a", "ghost")]),
        ("complete",),
    )


# Every node type with no data, and with junk in every field it reads.
VALID_NODE_TYPES = None
# The web checkout, for the import-format doc's examples.
WEB = Path(".")
# Cases left out because the desktop deliberately differs.
SKIPPED: list[str] = []


def node_type_cases():
    for node_type in sorted(VALID_NODE_TYPES):
        yield f"empty_{node_type}", single(node_type, {}), ("complete", "draft")
        blanks = {"symbol": " ", "exchange": "", "action": "", "quantity": "", "message": "", "orderId": None}
        yield f"blank_{node_type}", single(node_type, blanks), ("complete",)


ENUM_FIELDS = ("exchange", "action", "product", "priceType", "pricetype", "operation")


def enum_cases():
    order = {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1}
    for field in ENUM_FIELDS:
        for i, value in enumerate(ODD_VALUES + ["nse", " Buy ", "cnc", "sl-m", "SET", "set", "LIMT", "NSEE"]):
            yield f"enum_{field}_{i}", single("placeOrder", {**order, field: value}), ("complete", "draft")
    for i, value in enumerate(ODD_VALUES):
        yield (
            f"enum_on_unknown_type_{i}",
            wf([node("t", "start"), node("x", "Mystery", {"exchange": value})], [edge("e", "t", "x")]),
            ("draft",),
        )
        yield (
            f"enum_on_empty_type_{i}",
            wf([node("t", "start"), node("x", "", {"action": value})], [edge("e", "t", "x")]),
            ("draft",),
        )


def number_cases():
    for node_type in ("placeOrder", "smartOrder", "splitOrder", "optionsOrder", "log"):
        for field in ("quantity", "splitSize", "lots"):
            for i, value in enumerate(ODD_VALUES + [1, "0", 2, "-0", "1e2"]):
                yield f"number_{node_type}_{field}_{i}", single(node_type, {field: value}), ("draft",)
    yield (
        "number_all_three_bad",
        single("splitOrder", {"lots": 0, "splitSize": -2, "quantity": "x"}),
        ("draft",),
    )


PRICED = {
    "placeOrder": {"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": 1},
    "smartOrder": {"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": 1},
    "optionsOrder": {"underlying": "NIFTY", "action": "BUY", "quantity": 1},
    "optionsMultiOrder": {"strategy": "straddle", "underlying": "NIFTY", "quantity": 1},
    "basketOrder": {"orders": "RELIANCE,NSE,BUY,1"},
    "splitOrder": {"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": 1, "splitSize": 1},
}


def priced_cases():
    for node_type, minimal in PRICED.items():
        for price_type in (
            "MARKET",
            "LIMIT",
            "SL",
            "SL-M",
            " limit ",
            "sl-m",
            "{{pt}}",
            5,
            None,
            "",
            "BOGUS",
        ):
            for k, (price, trigger) in enumerate(
                [
                    ("missing", "missing"),
                    ("", " "),
                    (0, 0),
                    (-1, "abc"),
                    (1, "2"),
                    ("{{p}}", True),
                    (None, [1]),
                ]
            ):
                data = {**minimal, "priceType": price_type}
                if price != "missing":
                    data["price"] = price
                if trigger != "missing":
                    data["triggerPrice"] = trigger
                yield f"priced_{node_type}_{price_type}_{k}", single(node_type, data), ("complete", "draft")


def node_specific_cases():
    for i, params in enumerate([{"period": 14}, [14], 14, True, None, '{"period": 14}', ""]):
        yield (
            f"indicator_params_{i}",
            single(
                "indicator", {"symbol": "SBIN", "exchange": "NSE", "indicatorName": "rsi", "params": params}
            ),
            ("complete", "draft"),
        )
    for i, series in enumerate([None, "", "  ", "{{bars}}", 5, ["x"]]):
        yield (
            f"indicator_source_{i}",
            single("indicator", {"indicatorName": "rsi", "sourceSeries": series}),
            ("complete",),
        )
    statuses = [
        "missing",
        None,
        "",
        0,
        False,
        "filled",
        " Trigger_Pending ",
        "ANY",
        123,
        ["open"],
        "trigger-pending",
        "_open_",
        "cancelled",
    ]
    for i, status in enumerate(statuses):
        for j, filters in enumerate(
            [
                {},
                {"orderId": "1"},
                {"symbol": "SBIN"},
                {"orderId": " "},
                {"orderId": "{{p.id}}"},
                {"orderId": 5, "symbol": ["x"]},
                {"orderId": "{{p.id}}", "symbol": "SBIN"},
            ]
        ):
            data = dict(filters)
            if status != "missing":
                data["status"] = status
            yield (
                f"order_update_{i}_{j}",
                wf(
                    [node("t", "orderUpdateTrigger", data), node("a", "log", {"message": "m"})],
                    [edge("e", "t", "a")],
                ),
                ("complete", "draft"),
            )
    for i, method in enumerate(["GET", " post ", "TRACE", "", "  ", "{{m}}", 5, None, "options"]):
        yield (
            f"http_method_{i}",
            single("httpRequest", {"url": "https://x.com", "method": method}),
            ("complete", "draft"),
        )
    for i, headers in enumerate(
        [
            '{"A": "b"}',
            '["a"]',
            "{not json}",
            "",
            " ",
            "{{h}}",
            {"A": "b"},
            5,
            "null",
            ' {"A": 1} ',
            '{"A": 1} x',
        ]
    ):
        yield (
            f"http_headers_{i}",
            single("httpRequest", {"url": "https://x.com", "headers": headers}),
            ("complete", "draft"),
        )
    for i, timeout in enumerate(
        [
            999,
            1000,
            60000,
            60001,
            "5000",
            " 5000 ",
            "abc",
            True,
            0,
            -5,
            1000.5,
            "{{t}}",
            None,
            "",
            [1],
            "Infinity",
            "1_000",
        ]
    ):
        yield (
            f"http_timeout_{i}",
            single("httpRequest", {"url": "https://x.com", "timeout": timeout}),
            ("complete", "draft"),
        )
    yield (
        "http_all_bad",
        single("httpRequest", {"method": "TRACE", "headers": "[]", "timeout": 1}),
        ("complete",),
    )
    for i, data in enumerate(
        [
            {},
            {"minAvailable": 0},
            {"minAvailable": ""},
            {"threshold": 5},
            {"minAvailable": None, "threshold": " "},
            {"minAvailable": "{{x}}"},
        ]
    ):
        yield f"fund_check_{i}", single("fundCheck", data), ("complete", "draft")
    for i, data in enumerate(
        [
            {"underlying": "NIFTY"},
            {"underlying": "NIFTY28AUG26"},
            {"underlying": "nifty28aug26fut"},
            {"underlying": " NIFTY28AUG26"},
            {"underlying": "{{u}}"},
            {"underlying": ""},
            {"underlying": 5},
            {"underlying": "NIFTY", "expiryDate": " "},
            {"underlying": "NIFTY", "expiryDate": "28AUG26"},
        ]
    ):
        for node_type in ("optionSymbol", "optionChain", "syntheticFuture"):
            yield (
                f"expiry_need_{node_type}_{i}",
                single(node_type, {"optionType": "CE", **data}),
                ("complete",),
            )
    operations = [
        "missing",
        None,
        "set",
        "get",
        "add",
        "subtract",
        "multiply",
        "divide",
        "increment",
        "decrement",
        "parse_json",
        "stringify",
        "append",
        "SET",
        " set ",
        "",
        "merge",
        "{{op}}",
        5,
    ]
    for i, operation in enumerate(operations):
        for j, extra in enumerate(
            [{}, {"value": 1, "sourceVariable": "s"}, {"value": "", "sourceVariable": " "}]
        ):
            data = {"variableName": "v", **extra}
            if operation != "missing":
                data["operation"] = operation
            yield f"variable_{i}_{j}", single("variable", data), ("complete", "draft")


def price_alert_cases():
    conditions = [
        "greater_than",
        "less_than",
        "crossing",
        "crossing_up",
        "crossing_down",
        "entering_channel",
        "inside_channel",
        "exiting_channel",
        "outside_channel",
        "moving_up_percent",
        "moving_down_percent",
        "moving_up",
        "moving_down",
        "above",
        "price_above",
        "below",
        "price_below",
        "crosses_above",
        "cross_above",
        "crosses_below",
        "cross_below",
        "crosses",
        "ABOVE",
        " above ",
        "Crossing-Up",
        "abov",
        "",
        None,
        5,
        "missing",
    ]
    level_sets = [
        {},
        {"price": 100},
        {"price": 0},
        {"price": -1},
        {"price": "abc"},
        {"price": "100"},
        {"price": True},
        {"price": None},
        {"priceLower": 90, "priceUpper": 110},
        {"priceLower": 110, "priceUpper": 90},
        {"priceLower": 100, "priceUpper": 100},
        {"priceLower": "90", "priceUpper": 110.5},
        {"priceLower": 0, "priceUpper": 110},
        {"priceLower": 90},
        {"percentage": 2},
        {"percentage": 0},
        {"percentage": "x"},
        {"price": "{{p}}"},
    ]
    for i, condition in enumerate(conditions):
        for j, levels in enumerate(level_sets):
            data = {"symbol": "X", "exchange": "NSE", **levels}
            if condition != "missing":
                data["condition"] = condition
            yield (
                f"price_alert_{i}_{j}",
                wf(
                    [node("t", "priceAlert", data), node("a", "log", {"message": "m"})], [edge("e", "t", "a")]
                ),
                ("complete",),
            )


def margin_leg(**overrides):
    leg = {
        "symbol": "RELIANCE",
        "exchange": "NSE",
        "action": "BUY",
        "quantity": "1",
        "product": "MIS",
        "pricetype": "MARKET",
        "price": "0",
    }
    leg.update(overrides)
    return leg


def margin_cases():
    datas = [
        {},
        {"positionsJson": ""},
        {"positionsJson": "   "},
        {"positionsJson": "not json"},
        {"positionsJson": "[]"},
        {"positionsJson": "{}"},
        {"positionsJson": "null"},
        {"positionsJson": "5"},
        {"positionsJson": '["not a leg"]'},
        {"positionsJson": '[{"symbol": "RELIANCE"}]'},
        {"positionsJson": json.dumps([margin_leg(exchange="NOPE")])},
        {"positionsJson": json.dumps([margin_leg(pricetype="LIMIT")])},
        {"positionsJson": json.dumps([margin_leg(pricetype="SL", price="5")])},
        {"positionsJson": json.dumps([margin_leg(pricetype="SL-M", trigger_price=0)])},
        {"positionsJson": "{{webhook.positions}}"},
        {"positionsJson": "[{{webhook.positions}}]"},
        {
            "positionsJson": json.dumps(
                [margin_leg(symbol="{{s}}", quantity="{{q}}", pricetype="LIMIT", price="{{p}}")]
            )
        },
        {"positionsJson": json.dumps([margin_leg()])},
        {"positions": [margin_leg()]},
        {"positions": [margin_leg(quantity=0, action="HOLD", product=5)]},
        {"positions": []},
        {"positions": {}},
        {"positions": 5},
        {"positions": "", "positionsJson": json.dumps([margin_leg()])},
        {"symbol": "RELIANCE"},
        {"symbol": " "},
        {"positionsJson": json.dumps([margin_leg(), 3, margin_leg(exchange=1)])},
        {"positionsJson": json.dumps([{"quantity": "", "pricetype": " "}])},
        {"positionsJson": json.dumps([margin_leg(quantity=True)])},
        {"positionsJson": ' [ {"symbol": "A"} ] '},
    ]
    for i, data in enumerate(datas):
        yield f"margin_{i}", single("margin", data), ("complete", "draft")


def options_multi_cases():
    base = {"underlying": "NIFTY", "quantity": 1}
    leg = {"offset": "ATM", "optionType": "CE", "action": "BUY", "quantity": 1}
    datas = [
        {**base},
        {**base, "strategy": None},
        {**base, "strategy": 5},
        {**base, "strategy": "{{s}}"},
        {**base, "strategy": " Custom "},
        {**base, "strategy": "straddle", "priceType": "SL"},
        {**base, "strategy": "iron_condor", "priceType": " limit ", "price": 0},
        {**base, "strategy": "straddle", "priceType": ""},
        {**base, "strategy": "straddle", "priceType": 5},
        {**base, "strategy": "custom"},
        {**base, "strategy": "custom", "legs": None},
        {**base, "strategy": "custom", "legs": ""},
        {**base, "strategy": "custom", "legs": []},
        {**base, "strategy": "custom", "legs": {}},
        {**base, "strategy": "custom", "legs": "x"},
        {**base, "strategy": "custom", "legs": "{{l}}"},
        {**base, "strategy": "custom", "orderLegs": [leg]},
        {**base, "strategy": "custom", "orderLegs": [{}], "legs": [leg]},
        {**base, "strategy": "custom", "legs": [5, leg]},
        {**base, "strategy": "custom", "legs": [{}]},
        {**base, "strategy": "custom", "legs": [{"offset": "ATM"}]},
        {**base, "strategy": "custom", "product": "NOPE", "legs": [leg]},
        {**base, "strategy": "custom", "product": "NRML", "legs": [{**leg, "product": "BAD"}]},
        {**base, "strategy": "custom", "priceType": "LIMIT", "legs": [leg]},
        {**base, "strategy": "custom", "priceType": "LIMIT", "price": 5, "legs": [leg]},
        {
            **base,
            "strategy": "custom",
            "priceType": "SL",
            "price": 110,
            "triggerPrice": 105,
            "legs": [{**leg, "pricetype": " sl "}],
        },
        {
            **base,
            "strategy": "custom",
            "legs": [{**leg, "pricetype": "SL", "priceType": "LIMIT", "price": 1}],
        },
        {**base, "strategy": "custom", "legs": [{**leg, "priceType": "SL-M"}]},
        {**base, "strategy": "custom", "legs": [{**leg, "optionType": "XX", "action": 1}]},
        {**base, "strategy": "custom", "legs": [{**leg, "exchange": "nfo"}]},
        {**base, "strategy": "custom", "legs": [{**leg, "quantity": "0"}]},
    ]
    strikes = [
        {"strikeMode": "STRIKE", "strike": 24500},
        {"strikeMode": " strike ", "strike": "24500.5"},
        {"strikeMode": "NEAREST", "strike": 1},
        {"strikeMode": 5, "offset": "ATM"},
        {"strikeMode": "{{m}}"},
        {"strike": 0},
        {"strike": -100},
        {"strike": "0"},
        {"strike": "abc"},
        {"strike": True},
        {"strike": [1]},
        {"strike": "{{k}}"},
        {"strike": "", "offset": "OTM2"},
        {"strike": None, "offset": " itm3 "},
        {"offset": "OTM99"},
        {"offset": "ATM1"},
        {"offset": 5},
        {"offset": "{{o}}"},
        {"offset": ""},
        {},
        {"strike": " 1_000 "},
        {"strike": "NaN"},
        {"strike": "Infinity"},
        {"strike": " +inf "},
        {"strike": "-inf"},
        {"strike": "NaN", "offset": "OTM99"},
    ]
    for strike in strikes:
        datas.append(
            {
                **base,
                "strategy": "custom",
                "legs": [{"optionType": "PE", "action": "SELL", "quantity": 2, **strike}],
            }
        )
    for expiry in ["28OCT25", "28oct25", " 28OCT25 ", "2025-10-28", "28-OCT-25", "", 5, "{{e}}", None]:
        datas.append({**base, "strategy": "custom", "legs": [{**leg, "expiry": expiry}]})
    for expiry_type in ["current_week", " Next_Month ", "next_year", "", 5, "{{t}}"]:
        datas.append({**base, "strategy": "custom", "legs": [{**leg, "expiryType": expiry_type}]})
    for i, data in enumerate(datas):
        yield f"options_multi_{i}", single("optionsMultiOrder", data), ("complete", "draft")


def ported_test_cases():
    """The payloads the web's own tests build, recorded in full."""
    valid_order = {
        "symbol": "SBIN",
        "exchange": "NSE",
        "action": "BUY",
        "quantity": 10,
        "product": "MIS",
        "priceType": "MARKET",
    }
    yield "qa_valid_order", single("placeOrder", valid_order), ("complete",)
    for field, value in [
        ("exchange", "NSEE"),
        ("action", "BUYY"),
        ("product", "INTRADAY"),
        ("priceType", "LIMT"),
    ]:
        yield f"qa_bad_{field}", single("placeOrder", {**valid_order, field: value}), ("complete", "draft")
    smart = {
        "symbol": "SBIN",
        "exchange": " nse ",
        "action": " buy ",
        "quantity": 1,
        "positionSize": 0,
        "product": " mis ",
        "priceType": " limit ",
        "price": 100,
    }
    yield (
        "final_fix_canonical_smart_order",
        wf(
            [node("trigger", "webhookTrigger"), node("order", "smartOrder", smart)],
            [edge("edge", "trigger", "order")],
        ),
        ("complete",),
    )
    editor = [
        node("n1", "start", {"scheduleType": "interval", "intervalValue": 1, "intervalUnit": "minutes"}),
        node("n2", "getQuote", {"symbol": "RELIANCE", "exchange": "NSE", "outputVariable": "q"}),
        node("n3", "varCondition", {"leftValue": "{{q.data.ltp}}", "operator": ">", "rightValue": "1000"}),
        node("n4", "placeOrder", {"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": 1}),
    ]
    yield (
        "editor_built_strategy",
        wf(
            editor,
            [edge("e1", "n1", "n2"), edge("e2", "n2", "n3"), edge("e3", "n3", "n4", sourceHandle="true")],
        ),
        ("complete",),
    )


def doc_example_cases():
    """Every workflow or node snippet in the web's import-format doc, wrapped
    as the web's `_as_workflow_example` test helper wraps it."""
    import re

    prompt = (WEB / "docs" / "prompt" / "flow-import-format.md").read_text(encoding="utf-8")
    fence = re.compile(r"^```json\s*\n(.*?)^```\s*$", re.MULTILINE | re.DOTALL)
    decoder = json.JSONDecoder()
    for index, match in enumerate(fence.finditer(prompt), 1):
        raw, offset, value_index = match.group(1), 0, 0
        while True:
            while offset < len(raw) and raw[offset].isspace():
                offset += 1
            if offset == len(raw):
                break
            value, offset = decoder.raw_decode(raw, offset)
            value_index += 1
            payload = as_workflow_example(value)
            if payload is not None:
                yield f"doc_fence_{index}_{value_index}", payload, ("complete",)


def as_workflow_example(value):
    """`test_flow_workflow_validator._as_workflow_example`, verbatim in effect."""
    if isinstance(value, dict) and "nodes" in value and "edges" in value:
        return value
    snippets = [value] if isinstance(value, dict) else value if isinstance(value, list) else None
    if not snippets or not all(
        isinstance(n, dict) and isinstance(n.get("type"), str) and isinstance(n.get("data"), dict)
        for n in snippets
    ):
        return None
    nodes = []
    for index, snippet in enumerate(snippets, 1):
        n = dict(snippet)
        n.setdefault("id", f"documented_node_{index}")
        n.setdefault("position", {"x": 100, "y": index * 100})
        nodes.append(n)
    trigger_ids = [
        n["id"] for n in nodes if n["type"] in ("start", "webhookTrigger", "priceAlert", "orderUpdateTrigger")
    ]
    if not trigger_ids:
        nodes.insert(0, node("documented_trigger", "start"))
        trigger_ids = ["documented_trigger"]
    if len(nodes) == 1:
        nodes.append(node("documented_action", "log", {"message": "Prompt contract example"}))
    trigger_id = trigger_ids[0]
    edges = [
        {"id": f"documented_edge_{i}", "source": trigger_id, "target": n["id"]}
        for i, n in enumerate(nodes, 1)
        if n["id"] != trigger_id
    ]
    return wf(nodes, edges, name="Prompt contract example")


def desktop_differs(payload, level):
    """Why the desktop deliberately answers this case differently, or None.
    These are pinned by hand in src/flow/validator/tests/rules.rs."""
    if not isinstance(payload, dict) or not isinstance(payload.get("nodes"), list):
        return None
    nodes = [n for n in payload["nodes"] if isinstance(n, dict)]
    edges = (
        [e for e in payload.get("edges") or [] if isinstance(e, dict)]
        if isinstance(payload.get("edges"), list)
        else []
    )
    for n in nodes:
        data = n.get("data") if isinstance(n.get("data"), dict) else None
        if data is None:
            continue
        if n.get("type") == "optionsMultiOrder" and "strategy" not in data:
            return "V-059: the strategy defaults to custom, as the executor reads it"
        declared = executor_input_count(n.get("type"), data)
        wired = sum(1 for e in edges if e.get("target") == n.get("id"))
        if LEVELS[level]["strict"] and declared is not None and declared > wired:
            return "V-085: a gate wired to fewer inputs than it declares is refused"
    return None


def executor_input_count(node_type, data):
    """The inputs the desktop's executor counts for a gate
    (``executor::walk::declared_inputs``): AND and OR read an absent, null,
    blank or unreadable ``inputCount`` as 2 and never less than 1; NOT reads
    only a whole number or a boolean."""
    raw = data.get("inputCount", MISSING)
    if node_type == "notGate":
        return int(raw) if isinstance(raw, int) else None
    if node_type not in ("andGate", "orGate"):
        return None
    if raw is MISSING or raw is None or (isinstance(raw, str) and raw == ""):
        return 2
    if isinstance(raw, bool) or isinstance(raw, int):
        slots = int(raw)
    elif isinstance(raw, float):
        slots = math.trunc(raw) if math.isfinite(raw) else 2
    elif isinstance(raw, str):
        text = raw.strip()
        slots = int(text) if re.fullmatch(r"[+-]?[0-9]+", text) else 2
    else:
        slots = 2
    return max(slots, 1)


MISSING = object()


def migration_differs(nodes):
    """Why the desktop deliberately migrates these nodes differently, or None."""
    for n in nodes:
        if not isinstance(n, dict) or n.get("type") != "positionCheck":
            continue
        data = n.get("data") if isinstance(n.get("data"), dict) else {}
        operator = str(data.get("operator") or "").strip().lower()
        if "condition" not in data and operator in ("gte", "ge", ">=", "lte", "le", "<="):
            return "V-113: an inclusive operator keeps its meaning by moving the threshold"
    return None


def order_cases():
    """The order the web reports errors in (V-004, V-005, V-019, V-094)."""
    # Every pass in one payload: both size limits, a node failing id, type,
    # data and position, edge pass-1 errors after later handle errors in
    # index order, and no trigger.
    nodes = [{"id": "", "type": 5, "data": [], "position": "x"}]
    nodes += [node(f"n{i}", "log") for i in range(1, 501)]
    edges = [{"id": "", "source": "", "target": 5}]
    edges.append({"id": "e1", "source": "n1", "target": "n2", "targetHandle": "input-1"})
    edges.append({"id": "e2", "source": "n1", "target": "n2", "sourceHandle": "x"})
    edges.append({"id": "e1", "source": "ghost", "target": "n2"})
    edges += [edge(f"e{i}", "n1", "n2") for i in range(4, 1001)]
    yield "error_order_every_pass", wf(nodes, edges), ("complete",)
    # The cycle search follows each node's edges in the order they are listed:
    # a reaches two cycles, b<->c and d<->e, and whichever edge out of a is
    # listed first decides which one is reported, once.
    chain = [node("t", "start")] + [node(x, "log") for x in "abcde"]
    to_bc = [edge("e1", "a", "b"), edge("e2", "b", "c"), edge("e3", "c", "b")]
    to_de = [edge("e4", "a", "d"), edge("e5", "d", "e"), edge("e6", "e", "d")]
    yield "error_order_cycle_bc_first", wf(chain, [edge("e0", "t", "a")] + to_bc + to_de), ("complete",)
    yield "error_order_cycle_de_first", wf(chain, [edge("e0", "t", "a")] + to_de + to_bc), ("complete",)
    # Two disjoint cycles, neither reachable from the trigger: one cycle error.
    loops = [node("t", "start"), node("l", "log"), node("x", "log"), node("y", "log"), node("z", "log"),
             node("w", "log")]
    loop_edges = [edge("e0", "t", "l"), edge("e1", "x", "y"), edge("e2", "y", "x"), edge("e3", "z", "w"),
                  edge("e4", "w", "z")]
    yield "error_order_two_disjoint_cycles", wf(loops, loop_edges), ("complete",)
    # Every enum field wrong on one node, with a bad quantity after them.
    bad = {"exchange": "NSEE", "action": "BUYY", "product": "X", "priceType": "Y", "pricetype": "Z",
           "operation": "w", "quantity": -1, "symbol": "SBIN"}
    yield "error_order_enum_fields", single("placeOrder", bad), ("complete", "draft")


def unicode_digit_cases():
    """Digits as Python's \\d and int() read them, Unicode included (V-084):
    ARABIC-INDIC THREE, ONE, and MATHEMATICAL DOUBLE-STRUCK DIGIT ONE."""
    for i, handle in enumerate(["input-\u0663", "input-\u0661", "input-\U0001d7d9", "input-1\u0661", "input-\u00b2"]):
        for j, (target_type, data) in enumerate([("log", {}), ("andGate", {}), ("orGate", {"inputCount": "\u0663"})]):
            nodes = [node("t", "start"), node("s", "log"), node("g", target_type, data)]
            yield f"unicode_digits_{i}_{j}", wf(nodes, [edge("e1", "t", "s"), edge("e2", "s", "g", targetHandle=handle)]), ("draft",)
    for i, count in enumerate(["\u0663", " \u0661\u0662 ", "\U0001d7d9"]):
        nodes = [node("t", "start"), node("s", "log"), node("g", "andGate", {"inputCount": count})]
        yield f"unicode_count_{i}", wf(nodes, [edge("e1", "t", "s"), edge("e2", "s", "g", targetHandle="input-5")]), ("draft",)


def python_json_cases():
    """JSON written as text that only Python's json.loads reads: NaN and
    Infinity (V-052, V-055)."""
    for i, headers in enumerate(['{"A": NaN}', '{"A": Infinity, "B": -Infinity}', '{"A": [NaN]}', "NaN"]):
        yield f"py_json_headers_{i}", single("httpRequest", {"url": "https://x.com", "headers": headers}), (
            "complete",
            "draft",
        )
    for i, positions in enumerate(
        [
            '[{"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "product": "MIS", '
            '"pricetype": "MARKET", "price": NaN}]',
            '[{"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "product": "MIS", '
            '"pricetype": "MARKET", "price": 0, "extra": Infinity}]',
        ]
    ):
        yield f"py_json_margin_{i}", single("margin", {"positionsJson": positions}), ("complete", "draft")


def option_leg_spelling_cases():
    """An option leg naming both priceType and pricetype: both are checked
    (V-057)."""
    base = {"strategy": "custom", "underlying": "NIFTY", "quantity": 1}
    leg = {"offset": "ATM", "optionType": "CE", "action": "BUY", "quantity": 1}
    for i, extra in enumerate(
        [
            {"priceType": "LIMIT", "price": 1, "pricetype": "BOGUS"},
            {"priceType": "LIMIT", "price": 1, "pricetype": 5},
            {"priceType": "LIMIT", "price": 1, "pricetype": " sl-m "},
            {"priceType": "BOGUS", "pricetype": "LIMIT"},
            {"pricetype": "BOGUS"},
        ]
    ):
        yield f"option_leg_spellings_{i}", single("optionsMultiOrder", {**base, "legs": [{**leg, **extra}]}), (
            "complete",
            "draft",
        )


def runtime_only_cases():
    """Fields the web's validator leaves to the run: the httpRequest address
    (V-102), option expiries, offsets and strategy names (V-103), and the
    presence-only fields (V-104). Each passes validation as on the web."""
    for i, url in enumerate(
        ["http://127.0.0.1:5000/api", "http://10.0.0.1/", "http://169.254.169.254/latest", "ftp://x.com/f",
         "file:///etc/passwd", "not a url", "localhost"]
    ):
        yield f"runtime_http_url_{i}", single("httpRequest", {"url": url}), ("complete",)
    option_nodes = [
        ("optionSymbol", {"underlying": "NIFTY", "optionType": "CE", "expiryDate": "2026-08-28"}),
        ("optionSymbol", {"underlying": "NIFTY", "optionType": "XX", "expiryDate": "junk", "offset": "DEEP"}),
        ("optionChain", {"underlying": "NIFTY", "expiryDate": "28-08-2026"}),
        ("syntheticFuture", {"underlying": "NIFTY", "expiryDate": "AUG"}),
        ("optionsOrder", {"underlying": "NIFTY", "action": "BUY", "quantity": 1, "expiryType": "next_year",
                          "offset": "DEEP", "expiryDate": "tomorrow"}),
        ("optionsMultiOrder", {"underlying": "NIFTY", "quantity": 1, "strategy": "butterfly",
                               "expiryType": "someday", "strangleWidth": "WIDE"}),
        ("optionsMultiOrder", {"underlying": "NIFTY", "quantity": 1, "strategy": "strangle",
                               "strangleWidth": "OTM99", "offset": "?"}),
    ]
    for i, (node_type, data) in enumerate(option_nodes):
        yield f"runtime_option_{i}", single(node_type, data), ("complete",)
    presence_only = [
        ("mathExpression", {"expression": "((1 +"}),
        ("mathExpression", {"expression": "__import__('os').system('x')"}),
        ("varCondition", {"leftValue": "1", "operator": "??", "rightValue": "x"}),
        ("priceCondition", {"symbol": "SBIN", "exchange": "NSE", "operator": "between-ish", "value": "abc"}),
        ("timeWindow", {"startTime": "25:99", "endTime": "noon"}),
        ("timeCondition", {"targetTime": "99:99", "operator": "whenever"}),
        ("delay", {"delayValue": "-5", "delayUnit": "fortnights"}),
        ("waitUntil", {"targetTime": "yesterday"}),
        ("history", {"symbol": "SBIN", "exchange": "NSE", "interval": "7x"}),
        ("indicator", {"symbol": "SBIN", "exchange": "NSE", "indicatorName": "not_an_indicator",
                       "params": "{not json"}),
        ("basketOrder", {"orders": "garbage;;;"}),
        ("basketOrder", {"orders": [1, "x", None]}),
        ("multiQuotes", {"symbols": "!!!"}),
        ("multiQuotes", {"symbols": [{"nope": 1}]}),
    ]
    for i, (node_type, data) in enumerate(presence_only):
        yield f"runtime_presence_{i}", single(node_type, data), ("complete",)


def crash_cases():
    """Payloads the web's validator raises on (a 500 there)."""
    yield (
        "crash_list_edge_source",
        wf([node("t", "start")], [{"id": "e", "source": ["t"], "target": "t", "sourceHandle": "x"}]),
        ("draft",),
    )
    yield (
        "crash_list_gate_target",
        wf([node("t", "start")], [{"id": "e", "source": "t", "target": {"a": 1}, "targetHandle": "input-1"}]),
        ("draft",),
    )
    yield (
        "crash_list_node_type_with_handle",
        wf(
            [node("t", "start"), {"id": "c", "type": ["log"], "position": AT, "data": {}}],
            [edge("e", "c", "t", sourceHandle="x")],
        ),
        ("draft",),
    )


ALL_CASES = (
    shape_cases,
    node_shape_cases,
    edge_cases,
    handle_cases,
    gate_cases,
    trigger_and_graph_cases,
    node_type_cases,
    enum_cases,
    number_cases,
    priced_cases,
    node_specific_cases,
    price_alert_cases,
    margin_cases,
    options_multi_cases,
    ported_test_cases,
    doc_example_cases,
    order_cases,
    unicode_digit_cases,
    python_json_cases,
    option_leg_spelling_cases,
    runtime_only_cases,
    crash_cases,
)


def migrate_cases():
    yield "not_objects", [5, None, "x", ["a"]]
    yield "data_not_object", [{"id": "f", "type": "fundCheck", "data": "x"}, {"id": "g", "type": "fundCheck"}]
    for i, (operator, extra) in enumerate(
        [
            ("missing", {}),
            (None, {}),
            ("", {}),
            ("gt", {}),
            (" GTE ", {}),
            (">=", {}),
            ("ge", {}),
            ("lt", {}),
            ("<=", {}),
            (0, {}),
            (5, {}),
            ("gt", {"minAvailable": 1}),
            ("gt", {"threshold": None}),
        ]
    ):
        data = {"threshold": 10000, **extra}
        if operator != "missing":
            data["operator"] = operator
        yield f"fund_check_{i}", [{"id": f"f{i}", "type": "fundCheck", "position": AT, "data": data}]
    for i, (operator, extra) in enumerate(
        [
            ("gt", {}),
            (">", {}),
            ("gte", {}),
            ("ge", {}),
            (">=", {}),
            ("lt", {}),
            ("<", {}),
            ("lte", {}),
            ("le", {}),
            ("<=", {}),
            (" LT ", {}),
            ("weird", {}),
            ("", {}),
            (None, {}),
            (5, {}),
            ("gt", {"condition": "exists"}),
        ]
    ):
        yield (
            f"position_check_{i}",
            [
                {
                    "id": f"p{i}",
                    "type": "positionCheck",
                    "data": {"operator": operator, "threshold": 5, **extra},
                }
            ],
        )
    yield (
        "price_condition",
        [{"id": "c", "type": "priceCondition", "data": {"threshold": 101.5, "operator": ">"}}],
    )
    yield (
        "price_condition_kept",
        [{"id": "c", "type": "priceCondition", "data": {"threshold": 1, "value": 2}}],
    )
    yield (
        "id_shapes",
        [
            {"id": 7, "type": "fundCheck", "data": {"threshold": "500"}},
            {"type": "priceCondition", "data": {"threshold": [1]}},
        ],
    )
    yield (
        "mixed",
        [
            node("t", "start"),
            {"id": "f", "type": "fundCheck", "data": {"threshold": 1.5}},
            {"id": "p", "type": "positionCheck", "data": {"operator": "lt"}},
        ],
    )


def trigger_config_cases():
    full = {
        field: f"v-{field}"
        for field in (
            "scheduleType",
            "time",
            "days",
            "executeAt",
            "intervalMinutes",
            "intervalValue",
            "intervalUnit",
            "marketHoursOnly",
            "symbol",
            "exchange",
            "condition",
            "price",
            "priceLower",
            "priceUpper",
            "percentage",
            "expiration",
            "orderId",
            "status",
            "trigger",
        )
    }
    yield "not_objects", [5, None, "x"]
    yield "every_field", [{"type": "start", "data": {**full, "extra": 1}}]
    yield (
        "each_trigger",
        [
            {"type": t, "data": {"symbol": "S", "price": 1}}
            for t in ("start", "webhookTrigger", "priceAlert", "orderUpdateTrigger")
        ],
    )
    yield (
        "data_shapes",
        [
            {"type": "start"},
            {"type": "priceAlert", "data": None},
            {"type": "webhookTrigger", "data": "x"},
            {"type": "orderUpdateTrigger", "data": []},
        ],
    )
    yield (
        "later_wins",
        [{"type": "start", "data": {"time": "09:15"}}, {"type": "start", "data": {"time": "10:15"}}],
    )
    yield (
        "non_triggers",
        [{"type": "log", "data": {"symbol": "S"}}, {"type": "Start", "data": {}}, {"data": {}}],
    )


TRIGGER_PAIRS = [
    ([{"type": "priceAlert", "data": {"price": 100}}], [{"type": "priceAlert", "data": {"price": 100.0}}]),
    (
        [{"type": "start", "data": {"marketHoursOnly": True}}],
        [{"type": "start", "data": {"marketHoursOnly": 1}}],
    ),
    ([{"type": "start", "data": {"time": "09:15"}}], [{"type": "start", "data": {"time": "09:16"}}]),
    (
        [{"type": "start", "data": {"time": "09:15", "message": "a"}}],
        [{"type": "start", "data": {"time": "09:15", "message": "b"}}],
    ),
    ([{"type": "start", "data": {}}], [{"type": "webhookTrigger", "data": {}}]),
    ([{"type": "start", "data": {"days": [1, 2]}}], [{"type": "start", "data": {"days": [1.0, 2]}}]),
    ([{"type": "start", "data": {"days": None}}], [{"type": "start", "data": {}}]),
]


def contract_cases(contracts, price_monitor):
    expiries = [
        "28-AUG-26",
        "28aug26",
        "28-August-2026",
        "28AUGUST2026",
        "1-AUG-26",
        " 1-AUG-26",
        "01-JAN-68",
        "01-JAN-69",
        "31-FEB-26",
        "2026-08-28",
        "28-AUGUST-26",
        "28-AUG-2026",
        "",
        "28AUG26FUT",
        "28-Sept-26",
        "28-SEPT-2026",
        "9SEP26",
    ]
    lists = [
        ["28-AUG-26", "04-SEP-26", "11-SEP-26", "25-SEP-26", "30-OCT-26"],
        ["28-AUG-26", "25-SEP-26", "29-OCT-26"],
        ["25-SEP-26", "28-AUG-26", "04-SEP-26", "junk"],
        ["31-DEC-26", "28-JAN-27"],
        ["28-AUG-26", "28AUG26", "28-August-2026"],
        [],
    ]
    days = ["2026-08-24", "2026-09-30", "2026-12-10", "2027-01-05"]
    types = ["current_week", "next_week", "current_month", "next_month", "next_year", ""]
    select = []
    for listed in lists:
        for day in days:
            now = datetime.strptime(day, "%Y-%m-%d")
            for expiry_type in types:
                select.append(
                    {
                        "expiries": listed,
                        "today": day,
                        "type": expiry_type,
                        "result": contracts.select_expiry(listed, expiry_type, now=now),
                    }
                )
    parse = []
    for raw in expiries:
        parsed = contracts.parse_expiry_date(raw)
        parse.append(
            {
                "value": raw,
                "date": parsed.strftime("%Y-%m-%d") if parsed else None,
                "api": contracts.format_expiry_for_api(raw),
            }
        )
    underlyings = [
        "NIFTY",
        "NIFTY27AUG26",
        "crudeoil19nov25fut",
        " NIFTY27AUG26",
        "NIFTY27AUG26\n",
        "NIFTY27AUG2026",
        "BANK NIFTY",
        "M&M",
        "",
    ]
    exchanges = [
        None,
        "",
        "  ",
        "NFO",
        " nfo ",
        "MCX",
        "NSE",
        "NSE_INDEX",
        "CRYPTO",
        "NOTANEXCHANGE",
        "BCD",
        "NCO",
    ]
    statuses = ["", " TRIGGER_PENDING ", "Complete", "_open_", "cancelled", "Rejected\n"]
    conditions = [
        "above",
        " Price-Above ",
        "crosses_below",
        "crosses",
        "entering-channel",
        "abov",
        "",
        "MOVING_UP",
    ]
    offsets = [
        "ATM",
        "ITM1",
        "ITM9",
        "ITM10",
        "ITM49",
        "ITM50",
        "OTM1",
        "OTM50",
        "OTM99",
        "ATM1",
        "DEEP",
        "ITM0",
        "OTM51",
        "otm1",
        "ITM05",
        "ATM ",
    ]
    leg_expiries = ["28OCT25", "2025-10-28", "28-OCT-25", "OCT2825", "28OCTOBER25", "28oct25", "28OCT25\n"]
    return {
        "select_expiry": select,
        "parse_expiry_date": parse,
        "parse_underlying_symbol": [
            {"value": u, "result": list(contracts.parse_underlying_symbol(u))} for u in underlyings
        ],
        "default_product_for_exchange": [
            {"value": e, "result": contracts.default_product_for_exchange(e)} for e in exchanges
        ],
        "normalize_status": [{"value": s, "result": contracts.normalize_status(s)} for s in statuses],
        "normalize_price_condition": [
            {"value": c, "result": price_monitor.FlowPriceMonitor.normalize_condition(c)} for c in conditions
        ],
        "is_option_offset": [
            {"value": o, "result": bool(contracts.OPTION_OFFSET_PATTERN.fullmatch(o))} for o in offsets
        ],
        "is_expiry_date": [
            {"value": e, "result": bool(contracts.EXPIRY_DATE_PATTERN.fullmatch(e))} for e in leg_expiries
        ],
        "valid_statuses": sorted(contracts.VALID_STATUSES),
        "valid_expiry_types": sorted(contracts.VALID_EXPIRY_TYPES),
        "valid_leg_strike_modes": sorted(contracts.VALID_LEG_STRIKE_MODES),
        "derivative_exchanges": sorted(contracts.DERIVATIVE_EXCHANGES),
    }


def committed_tree(web: Path, ref: str) -> tuple[Path, str]:
    """The web's sources at ``ref`` (``git archive``) in a temporary folder,
    so uncommitted work in the checkout never reaches the goldens."""
    commit = subprocess.run(
        ["git", "-C", str(web), "rev-parse", "--short=9", ref], check=True, capture_output=True, text=True
    ).stdout.strip()
    archive = subprocess.run(
        ["git", "-C", str(web), "archive", ref, "services", "utils", "database", "docs/prompt"],
        check=True,
        capture_output=True,
    ).stdout
    out = Path(tempfile.mkdtemp(prefix="flow-goldens-web-"))
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(out, filter="data")
    return out, commit


def load_web(web: Path):
    """Import the web's validator and contracts from its committed tree."""
    sys.dont_write_bytecode = True
    os.environ["LOG_DIR"] = tempfile.mkdtemp(prefix="flow-goldens-log-")
    os.environ.setdefault("LOG_TO_FILE", "False")
    sys.path.insert(0, str(web))
    import services.flow_node_contracts as contracts
    import services.flow_price_monitor_service as price_monitor
    import services.flow_workflow_validator as validator

    # The two set iterations, pinned to list order (see the module docstring).
    validator.POSITIVE_NUMBER_FIELDS = ("quantity", "splitSize", "lots")
    original_graph = validator._graph_errors

    def graph_in_node_order(nodes, edges, node_ids, triggers):
        order = []
        for n in nodes:
            nid = n.get("id") if isinstance(n, dict) else None
            if isinstance(nid, str) and nid in node_ids and nid not in order:
                order.append(nid)
        return original_graph(nodes, edges, dict.fromkeys(order).keys(), triggers)

    validator._graph_errors = graph_in_node_order
    # The alias table must come from the monitor, not the validator's fallback.
    assert validator._canonical_condition("price_above") == "greater_than"
    return validator, contracts, price_monitor


def as_desktop_sees_it(value):
    """The JSON round trip the desktop's request parsing applies."""
    return json.loads(json.dumps(value, sort_keys=True))


def record_validate(validator):
    entries, names = [], set()
    for family in ALL_CASES:
        for name, payload, levels in family():
            assert name not in names, name
            names.add(name)
            payload = as_desktop_sees_it(payload)
            results = {}
            for level in levels:
                reason = desktop_differs(payload, level)
                if reason:
                    SKIPPED.append(f"{name} ({level}): {reason}")
                    continue
                try:
                    results[level] = {"errors": validator.validate_workflow(payload, **LEVELS[level])}
                except Exception as exc:  # noqa: BLE001 - recording the web's failure
                    results[level] = {"raises": type(exc).__name__}
            if results:
                entries.append({"name": name, "payload": payload, "levels": results})
    return entries


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--web", type=Path, required=True, help="read-only OpenAlgo web checkout")
    parser.add_argument("--out", type=Path, required=True, help="fixture folder to write into")
    parser.add_argument("--ref", default="HEAD", help="the web commit to record from (default HEAD)")
    args = parser.parse_args()

    tree, commit = committed_tree(args.web.resolve(), args.ref)
    validator, contracts, price_monitor = load_web(tree)
    global VALID_NODE_TYPES, WEB
    VALID_NODE_TYPES = validator.VALID_NODE_TYPES
    WEB = tree

    goldens = {
        "web_commit": commit,
        "trigger_config_fields": list(validator.TRIGGER_CONFIG_FIELDS),
        "validate": record_validate(validator),
        "migrate": [],
        "trigger_config": [],
        "trigger_config_equal": [],
        "contracts": contract_cases(contracts, price_monitor),
    }
    for name, nodes in migrate_cases():
        nodes = as_desktop_sees_it(nodes)
        reason = migration_differs(nodes)
        if reason:
            SKIPPED.append(f"migrate {name}: {reason}")
            continue
        migrated, notes = validator.migrate_legacy_node_data(json.loads(json.dumps(nodes)))
        goldens["migrate"].append({"name": name, "nodes": nodes, "migrated": migrated, "notes": notes})
    for name, nodes in trigger_config_cases():
        nodes = as_desktop_sees_it(nodes)
        goldens["trigger_config"].append(
            {"name": name, "nodes": nodes, "config": validator.trigger_config(nodes)}
        )
    for before, after in TRIGGER_PAIRS:
        before, after = as_desktop_sees_it(before), as_desktop_sees_it(after)
        equal = validator.trigger_config(before) == validator.trigger_config(after)
        goldens["trigger_config_equal"].append({"before": before, "after": after, "equal": equal})

    args.out.mkdir(parents=True, exist_ok=True)
    target = args.out / "validator_goldens.json"
    with target.open("w", encoding="utf-8") as handle:
        handle.write("{\n")
        sections = list(goldens.items())
        for s, (key, value) in enumerate(sections):
            handle.write(f'"{key}": ')
            if isinstance(value, list) and value:
                handle.write("[\n")
                for i, item in enumerate(value):
                    handle.write(json.dumps(item, sort_keys=True, ensure_ascii=False, allow_nan=False))
                    handle.write(",\n" if i < len(value) - 1 else "\n")
                handle.write("]")
            else:
                handle.write(json.dumps(value, sort_keys=True, ensure_ascii=False, indent=1, allow_nan=False))
            handle.write(",\n" if s < len(sections) - 1 else "\n")
        handle.write("}\n")
    total = sum(len(e["levels"]) for e in goldens["validate"])
    raised = sum(1 for e in goldens["validate"] for r in e["levels"].values() if "raises" in r)
    for skipped in SKIPPED:
        print(f"left out: {skipped}")
    print(
        f"wrote {target}: {total} validations ({raised} raise on the web), "
        f"{len(goldens['migrate'])} migrations, {len(goldens['trigger_config'])} trigger configs"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
