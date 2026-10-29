"""Scenario helpers and the reply shapes the scripted client answers with.

Replies are in the shape the web's FlowOpenAlgoClient hands a node (its
`_handle_response`): `{"status": "success", ...}` or
`{"status": "error", "error": <message>, "code": <HTTP status>}`. Shapes come
from the desktop's tests/fixtures/web/rest captures of the web.
"""

from __future__ import annotations

import copy

CONDITION_TYPES = {
    "priceCondition", "timeCondition", "timeWindow", "positionCheck", "fundCheck",
    "varCondition", "priceAlert",
}
YES_NO = {"timeCondition", "notGate"}
GATES = {"andGate", "orGate", "notGate"}

SCENARIOS: list[dict] = []


def ok(**fields) -> dict:
    return {"status": "success", **fields}


def err(message, code=400) -> dict:
    return {"status": "error", "error": message, "code": code}


def reply(endpoint: str, value: dict) -> dict:
    return {"endpoint": endpoint, "reply": copy.deepcopy(value)}


def node(family: str, ntype: str, name: str, data: dict, *, script=(), env=None, vars_in=None,
         clock=None, parity=(), note="", desktop_expected=None, inputs=None, alert_reply=None,
         workflow_name=None, analyzer=False, probes=None, defaults=None, run_context=None, edges_extra=None,
         nodes_extra=None):
    """A one-node golden: the node is `n1`, the walk starts there, and probe
    children sit on every handle it can take."""
    # Keys sorted: the desktop keeps node data in key order (no
    # preserve_order), and one web log line lists the keys as stored.
    data = {k: copy.deepcopy(data[k]) for k in sorted(data)}
    nodes = [{"id": "n1", "type": ntype, "position": {"x": 0, "y": 0}, "data": data}]
    edges = []
    if probes is None:
        if ntype in CONDITION_TYPES or ntype in GATES:
            t, f = ("yes", "no") if ntype in YES_NO else ("true", "false")
            probes = [(t, t), (f, f), ("pass", None)]
        else:
            probes = [("next", None)]
    for label, handle in probes:
        pid = f"probe-{label}"
        nodes.append({"id": pid, "type": "goldenProbe", "position": {"x": 0, "y": 100}, "data": {"probe": label}})
        e = {"id": f"e-{label}", "source": "n1", "target": pid}
        if handle:
            e["sourceHandle"] = handle
        edges.append(e)
    nodes.extend(copy.deepcopy(nodes_extra or []))
    edges.extend(copy.deepcopy(edges_extra or []))
    conditions_in = {}
    for i, value in enumerate(inputs or [], start=1):
        sid = f"in{i}"
        nodes.append({"id": sid, "type": "goldenProbe", "position": {"x": -100, "y": 0}, "data": {"probe": sid}})
        edges.append({"id": f"e-in{i}", "source": sid, "target": "n1"})
        if value is not None:
            conditions_in[sid] = value
    s = {
        "kind": "node",
        "family": family,
        "type": ntype,
        "scenario": name,
        "note": note,
        "parity": list(parity),
        "clock": clock or "2026-10-29T10:15:00",
        "analyzer": analyzer,
        "workflow_name": workflow_name or "Golden Flow",
        "vars_in": copy.deepcopy(vars_in or {}),
        "conditions_in": conditions_in,
        "env": copy.deepcopy(env or {}),
        "graph": {"nodes": nodes, "edges": edges},
        "start": "n1",
        "script": [dict(x) for x in script],
        "defaults": copy.deepcopy(defaults or {}),
        "alert_reply": alert_reply,
        "desktop_expected": desktop_expected,
        "run_context": copy.deepcopy(run_context),
    }
    SCENARIOS.append(s)
    return s


def workflow(family: str, name: str, wf: dict, *, script=(), env=None, webhook=None, clock=None,
             parity=(), note="", desktop_expected=None, defaults=None, alert_reply=None, source="",
             run_context=None, api_key=True, special=None):
    s = {
        "kind": "workflow",
        "family": family,
        "type": "workflow",
        "scenario": name,
        "note": note,
        "source": source,
        "parity": list(parity),
        "clock": clock or "2026-10-29T10:15:00",
        "analyzer": False,
        "webhook": copy.deepcopy(webhook),
        "env": copy.deepcopy(env or {}),
        "workflow": copy.deepcopy(wf),
        "script": [dict(x) for x in script],
        "defaults": copy.deepcopy(defaults or {}),
        "alert_reply": alert_reply,
        "desktop_expected": desktop_expected,
        "run_context": copy.deepcopy(run_context),
        "api_key": api_key,
        "special": special,
    }
    SCENARIOS.append(s)
    return s


# --- Reply shapes ------------------------------------------------------------------

QUOTE = ok(data={"ask": 812.6, "bid": 812.4, "high": 818.0, "low": 805.25, "ltp": 812.5,
                 "oi": 0, "open": 806.0, "prev_close": 800.0, "volume": 1250000})
ORDER = ok(orderid="250408000123456")
ORDER_ANALYZE = ok(mode="analyze", orderid="26100334197687")
BROKER_REJECT = err("Insufficient funds: required 81250.00, available 1200.00", 400)
SESSION_EXPIRED = err("Invalid openalgo apikey", 403)
DEPTH = ok(data={
    "asks": [{"price": 812.6, "quantity": 120}, {"price": 812.7, "quantity": 80}],
    "bids": [{"price": 812.4, "quantity": 150}, {"price": 812.3, "quantity": 60}],
    "high": 818.0, "low": 805.25, "ltp": 812.5, "ltq": 10, "oi": 0, "open": 806.0,
    "prev_close": 800.0, "totalbuyqty": 210, "totalsellqty": 200, "volume": 1250000})
ORDER_STATUS = ok(data={
    "action": "BUY", "average_price": 0.0, "exchange": "NSE", "order_status": "open",
    "orderid": "250408000123456", "price": 800.0, "pricetype": "LIMIT", "product": "MIS",
    "quantity": 10, "symbol": "SBIN", "timestamp": "2026-10-29 10:14:59", "trigger_price": 0.0})
POSITIONS = ok(data=[
    {"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": 10, "average_price": 800.0,
     "ltp": 812.5, "pnl": 125.0},
    {"symbol": "NIFTY25NOV2624000CE", "exchange": "NFO", "product": "NRML", "quantity": -65,
     "average_price": 110.0, "ltp": 98.5, "pnl": 747.5},
])
POSITIONS_EMPTY = ok(data=[], mode="analyze")
FUNDS = ok(data={"availablecash": 250000.5, "collateral": 0.0, "m2mrealized": 0.0,
                 "m2munrealized": 0.0, "utiliseddebits": 12000.0})
ORDERBOOK = ok(data={"orders": [{"action": "BUY", "average_price": 812.5, "exchange": "NSE",
                                 "order_status": "complete", "orderid": "250408000123456",
                                 "price": 0.0, "pricetype": "MARKET", "product": "MIS",
                                 "quantity": 10, "symbol": "SBIN", "timestamp": "2026-10-29 10:14:59",
                                 "trigger_price": 0.0}],
                     "statistics": {"total_buy_orders": 1, "total_completed_orders": 1,
                                    "total_open_orders": 0, "total_rejected_orders": 0,
                                    "total_sell_orders": 0}})
TRADEBOOK = ok(data=[{"action": "BUY", "average_price": 812.5, "exchange": "NSE", "orderid": "250408000123456",
                      "product": "MIS", "quantity": 10, "symbol": "SBIN", "timestamp": "10:14:59",
                      "trade_value": 8125.0}])
HOLDINGS = ok(data={"holdings": [{"exchange": "NSE", "pnl": 1520.0, "pnlpercent": 4.2, "product": "CNC",
                                  "quantity": 20, "symbol": "INFY"}],
                    "statistics": {"totalholdingvalue": 37720.0, "totalinvvalue": 36200.0,
                                   "totalpnlpercentage": 4.2, "totalprofitandloss": 1520.0}})
SYMBOL = ok(data={"brexchange": "NSE", "brsymbol": "SBIN-EQ", "exchange": "NSE", "expiry": "", "id": 1201,
                  "instrumenttype": "EQ", "lotsize": 1, "name": "STATE BANK OF INDIA", "strike": -0.01,
                  "symbol": "SBIN", "tick_size": 0.05, "token": "3045"})
OPTION_SYMBOL = ok(symbol="NIFTY25NOV2624000CE", exchange="NFO", lotsize=65, tick_size=0.05,
                   freeze_qty=1800, underlying_ltp=24012.35)
EXPIRY = ok(data=["04-NOV-26", "11-NOV-26", "18-NOV-26", "25-NOV-26", "30-DEC-26", "30-MAR-27"],
            message="Found 6 expiry dates for NIFTY options in NFO")
INTERVALS = ok(data={"days": ["D"], "hours": ["1h"], "minutes": ["1m", "3m", "5m", "10m", "15m", "30m"],
                     "months": [], "seconds": [], "weeks": []})
MULTIQUOTES = ok(results=[{"symbol": "SBIN", "exchange": "NSE", "data": {"ltp": 812.5, "open": 806.0}},
                          {"symbol": "INFY", "exchange": "NSE", "data": {"ltp": 1886.0, "open": 1880.0}}])
OPTION_CHAIN = ok(underlying="NIFTY", underlying_ltp=24012.35, expiry_date="25NOV26", atm_strike=24000.0,
                  chain=[{"strike": 24000.0,
                          "ce": {"symbol": "NIFTY25NOV2624000CE", "label": "ATM", "ltp": 180.5},
                          "pe": {"symbol": "NIFTY25NOV2624000PE", "label": "ATM", "ltp": 165.25}}])
SYNTHETIC = ok(underlying="NIFTY", underlying_ltp=24012.35, expiry="25NOV26", atm_strike=24000.0,
               synthetic_future_price=24015.25)
HOLIDAYS = ok(year=2026, data=[{"date": "2026-11-10", "description": "Diwali Balipratipada",
                                "holiday_type": "TRADING_HOLIDAY", "closed_exchanges": ["NSE", "BSE", "NFO"],
                                "open_exchanges": []}])
TIMINGS = ok(data=[{"exchange": "NSE", "start_time": 1793245500000, "end_time": 1793268000000},
                   {"exchange": "MCX", "start_time": 1793245200000, "end_time": 1793301000000}])
MARGIN = ok(data={"total_margin_required": 81250.0, "span_margin": 0.0, "exposure_margin": 0.0})
OPTIONS_ORDER = ok(exchange="NFO", offset="ATM", option_type="CE", orderid="250408000223344",
                   symbol="NIFTY25NOV2624000CE", underlying="NIFTY", underlying_ltp=24012.35)
MULTI_ORDER = ok(results=[{"action": "BUY", "leg": 1, "offset": "ATM", "option_type": "CE",
                           "orderid": "250408000223345", "status": "success", "symbol": "NIFTY25NOV2624000CE"},
                          {"action": "BUY", "leg": 2, "offset": "ATM", "option_type": "PE",
                           "orderid": "250408000223346", "status": "success", "symbol": "NIFTY25NOV2624000PE"}],
                 underlying="NIFTY", underlying_ltp=24012.35)
BASKET = ok(results=[{"orderid": "250408000300001", "status": "success", "symbol": "SBIN"},
                     {"orderid": "250408000300002", "status": "success", "symbol": "INFY"}])
SPLIT = ok(results=[{"order_num": 1, "orderid": "250408000400001", "quantity": 50, "status": "success"},
                    {"order_num": 2, "orderid": "250408000400002", "quantity": 50, "status": "success"}],
           split_size=50, total_quantity=100)
MODIFY = ok(message="Order modified successfully", orderid="250408000123456")
CANCEL = ok(message="Order cancelled successfully", orderid="250408000123456")
CANCEL_ALL = ok(canceled_orders=["250408000123456"], failed_cancellations=[], message="Canceled 1 orders. Failed to cancel 0 orders.")
CLOSE = ok(message="All Open Positions Squared Off")

NIFTY_CONTRACT = [
    {"symbol": "NIFTY25NOV2624000CE", "name": "NIFTY", "exchange": "NFO", "lotsize": 65, "expiry": "25-NOV-26", "strike": 24000.0, "instrumenttype": "CE"},
    {"symbol": "NIFTYNXT5025NOV2668000CE", "name": "NIFTYNXT50", "exchange": "NFO", "lotsize": 25, "expiry": "25-NOV-26", "strike": 68000.0, "instrumenttype": "CE"},
    {"symbol": "BANKNIFTY25NOV2652000CE", "name": "BANKNIFTY", "exchange": "NFO", "lotsize": 30, "expiry": "25-NOV-26", "strike": 52000.0, "instrumenttype": "CE"},
    {"symbol": "SENSEX27NOV2680000CE", "name": "SENSEX", "exchange": "BFO", "lotsize": 20, "expiry": "27-NOV-26", "strike": 80000.0, "instrumenttype": "CE"},
    {"symbol": "CRUDEOIL17NOV265800CE", "name": "CRUDEOIL", "exchange": "MCX", "lotsize": 100, "expiry": "17-NOV-26", "strike": 5800.0, "instrumenttype": "CE"},
    {"symbol": "GOLD25NOV26118000CE", "name": "GOLD", "exchange": "MCX", "lotsize": 1, "expiry": "25-NOV-26", "strike": 118000.0, "instrumenttype": "CE"},
]
DESC_CONTRACT = [
    # A broker that puts the contract description in `name` (fallback path).
    {"symbol": "NIFTYNXT5025NOV2668000CE", "name": "NIFTYNXT50 25 Nov 26 68000 CE", "exchange": "NFO", "lotsize": 25},
    {"symbol": "NIFTY25NOV2624000CE", "name": "NIFTY 25 Nov 26 24000 CE", "exchange": "NFO", "lotsize": 75},
]


def bars(n=120, start_epoch=1777314600, step=86400, base=800.0):
    """Deterministic OHLCV rows (ascending epoch seconds)."""
    out = []
    price = base
    for i in range(n):
        drift = ((i * 37) % 11 - 5) * 1.25
        o = round(price, 2)
        c = round(price + drift, 2)
        h = round(max(o, c) + 2.5 + (i % 3), 2)
        lo = round(min(o, c) - 2.0 - (i % 2), 2)
        out.append({"timestamp": start_epoch + i * step, "open": o, "high": h, "low": lo, "close": c,
                    "volume": 100000 + (i * 7919) % 50000})
        price = c
    return out


def _epoch_ist(y, m, d, hh=0, mm=0):
    import datetime as dt
    from zoneinfo import ZoneInfo

    return int(dt.datetime(y, m, d, hh, mm, tzinfo=ZoneInfo("Asia/Kolkata")).timestamp())


def daily_bars(n=60, end=(2026, 10, 28), include_today=None, base=800.0):
    """n weekday bars ending on `end` (IST midnight stamps); `include_today`
    appends a forming bar for that date."""
    import datetime as dt

    days = []
    d = dt.date(*end)
    while len(days) < n:
        if d.weekday() < 5:
            days.append(d)
        d -= dt.timedelta(days=1)
    days.reverse()
    if include_today:
        days.append(dt.date(*include_today))
    rows = bars(len(days), base=base)
    for row, day in zip(rows, days):
        row["timestamp"] = _epoch_ist(day.year, day.month, day.day)
    return rows


def intraday_bars(n, minutes, last=(2026, 10, 29, 10, 10), base=812.0):
    """n bars of `minutes` each, the last one starting at `last` (IST)."""
    end = _epoch_ist(*last)
    rows = bars(n, base=base)
    for i, row in enumerate(rows):
        row["timestamp"] = end - (n - 1 - i) * minutes * 60
    return rows
