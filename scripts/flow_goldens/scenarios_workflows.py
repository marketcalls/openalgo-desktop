"""Every JSON example of docs/prompt/flow-import-format.md and the flow-builder
skill, run end to end (whole workflows) or as one-node goldens (node examples)."""

from __future__ import annotations

import json
import os
import re

from lib import (
    CANCEL, CLOSE, DEPTH, EXPIRY, FUNDS, HOLDINGS, HOLIDAYS, INTERVALS, MARGIN, MULTI_ORDER, MULTIQUOTES,
    NIFTY_CONTRACT, OPTION_CHAIN, OPTION_SYMBOL, OPTIONS_ORDER, ORDER, ORDER_STATUS, ORDERBOOK, POSITIONS, QUOTE,
    SPLIT, SYMBOL, SYNTHETIC, TIMINGS, TRADEBOOK, BASKET, MODIFY, CANCEL_ALL, daily_bars, node, ok, workflow,
)

from harness import WEB  # noqa: E402
DOC = "docs/prompt/flow-import-format.md"
SKILL = ".claude/skills/flow-builder/SKILL.md"

FAMILY = {}
for fam, types in {
    "orders": "placeOrder smartOrder optionsOrder optionsMultiOrder modifyOrder cancelOrder cancelAllOrders closePositions basketOrder splitOrder",
    "data": "getQuote getDepth getOrderStatus openPosition history strategyPnl priorPeriodOhlc barOffset indicator symbol optionSymbol expiry intervals multiQuotes optionChain syntheticFuture calendar holidays timings orderBook tradeBook positionBook holdings funds margin",
    "logic": "positionCheck fundCheck priceCondition varCondition timeWindow timeCondition variable mathExpression delay waitUntil log telegramAlert whatsappAlert httpRequest",
    "triggers": "priceAlert webhookTrigger orderUpdateTrigger subscribeLtp subscribeQuote subscribeDepth unsubscribe",
    "core": "start group andGate orGate notGate",
}.items():
    for t in types.split():
        FAMILY[t] = fam

DEFAULTS = {
    "quotes": QUOTE, "depth": DEPTH, "history": ok(data=daily_bars(120)), "positionbook": POSITIONS,
    "funds": FUNDS, "placeorder": ORDER, "placesmartorder": ORDER, "splitorder": SPLIT, "optionsorder": OPTIONS_ORDER,
    "optionsmultiorder": MULTI_ORDER, "expiry": EXPIRY, "cancelorder": CANCEL, "cancelallorder": CANCEL_ALL,
    "closeposition": CLOSE, "orderstatus": ORDER_STATUS, "margin": MARGIN, "basketorder": BASKET,
    "modifyorder": MODIFY, "orderbook": ORDERBOOK, "tradebook": TRADEBOOK, "holdings": HOLDINGS, "symbol": SYMBOL,
    "optionsymbol": OPTION_SYMBOL, "optionchain": OPTION_CHAIN, "syntheticfuture": SYNTHETIC, "intervals": INTERVALS,
    "multiquotes": MULTIQUOTES, "market/holidays": HOLIDAYS, "market/timings": TIMINGS,
}
HTTP = {"resolve": {"audit.example.com": ["93.184.216.34"], "hooks.example.com": ["93.184.216.34"]},
        "response": {"status": 200, "body": "{\"received\": true}", "content_type": "application/json"},
        "record_headers": ["authorization", "x-source"]}
ENV = {"master_contract": NIFTY_CONTRACT, "http": HTTP, "strategy_book": [],
       "calendar_holidays": {"2026": []}, "feed": {"username": "trader", "broker": "fakebroker", "connect_error": "down"}}

GATE_IDS = {"andGate": "N-gate-05", "orGate": "N-gate-06", "notGate": "N-gate-07"}

# Per example: a webhook body where the example reads one, and a clock that
# makes the example do what its title says.
WEBHOOK = {"symbol": "RELIANCE", "action": "BUY", "qty": 5, "quantity": 5, "exchange": "NSE"}
CLOCKS = {"Intraday Square-Off at 15:15": "2026-10-29T15:05:00"}


def _blocks(rel):
    text = open(os.path.join(WEB, rel)).read()
    for b in re.findall(r"```json\n(.*?)```", text, re.S):
        try:
            v = json.loads(b)
        except ValueError:
            continue
        yield text[: text.find(b)].count("\n") + 1, v


def _slug(name):
    return re.sub(r"[^a-z0-9]+", "_", name.lower()).strip("_")


for rel, tag in ((DOC, "doc"), (SKILL, "skill")):
    for line, v in _blocks(rel):
        if isinstance(v, dict) and "nodes" in v:
            uses_webhook = any(n.get("type") == "webhookTrigger" for n in v["nodes"])
            workflow("workflows", f"{tag}_{_slug(v.get('name') or 'unnamed')}", v,
                     webhook=WEBHOOK if uses_webhook else None, clock=CLOCKS.get(v.get("name")),
                     env=ENV, defaults=DEFAULTS, source=f"{rel}:{line}",
                     note="The example as published, run end to end with scripted replies.",
                     parity=["IE-12", "A-35"])
        elif isinstance(v, dict) and "type" in v and isinstance(v.get("data"), dict):
            t = v["type"]
            node(FAMILY.get(t, "core"), t, f"{tag}_example_line{line}", v["data"], env=ENV, defaults=DEFAULTS,
                 vars_in={"webhook": WEBHOOK}, note=f"Node example from {rel}:{line}, run with scripted replies.",
                 parity=[GATE_IDS.get(t, f"N-{t}-01")])
