"""Record the web agent's order tools and risk guard as the desktop's contract.

Reads the web's `services/agent/tools/orders.py`, `order_vocab.py` and
`services/agent/safety/risk.py`, runs them, and writes three files:

- `src-tauri/src/agent/tools/orders/specs.json`: the seven order tools exactly
  as agno hands them to the model (name, description, JSON schema) and their
  approval flag, in the order the model sees them. The desktop embeds this
  file, so the tools' words are the web's and never retyped.
- `src-tauri/tests/fixtures/agent/risk_vectors.json`: the guard's verdicts for
  every rule and refusal code, step by step through a session (claims,
  releases, commits, the duplicate window on an injected clock). The
  desktop's pure guard must reproduce each verdict field for field.
- `src-tauri/tests/fixtures/agent/order_arguments.json`: what the order
  tools' validators make of the arguments a model sends: the payload built,
  or the exact correction the model reads.

Usage, from the desktop repository root, with the web's own environment:

    /path/to/openalgo/.venv/bin/python -I scripts/agent/capture_web_orders.py /path/to/openalgo

Regenerate only from the web; never edit the outputs by hand.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
from dataclasses import replace
from datetime import UTC, datetime, timedelta
from decimal import Decimal
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPECS = ROOT / "src-tauri/src/agent/tools/orders/specs.json"
FIXTURES = ROOT / "src-tauri/tests/fixtures/agent"

#: The fixed start of every vector session; steps say how far past it they run.
T0 = datetime(2026, 10, 27, 4, 0, 0, tzinfo=UTC)
KILL_FILE = "<kill_switch_file>"
TOKEN = "<claim_token>"


def capture_specs(orders_mod, tools_pkg) -> list[dict]:
    """The order tools as the model sees them, from the real toolkit."""
    context = tools_pkg.ToolContext(api_key="k", surface="chat", trading_enabled=True)
    toolkit = orders_mod.OrdersToolkit(context)
    specs = []
    for name in orders_mod.MUTATING_TOOLS:
        function = toolkit.functions[name]
        function.process_entrypoint(strict=False)
        d = function.to_dict()
        specs.append(
            {
                "name": d["name"],
                "description": d["description"],
                "parameters": d["parameters"],
                "requires_confirmation": bool(d.get("requires_confirmation")),
            }
        )
    return specs


# ---------------------------------------------------------------- risk vectors


def _limits_json(limits) -> dict:
    d = limits.as_dict()
    d["kill_switch_file"] = KILL_FILE
    return d


def _scrub(value):
    """Replace the run-specific claim token and kill switch path."""
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            if k == "claim_token":
                out[k] = TOKEN
            else:
                out[k] = _scrub(v)
        return out
    if isinstance(value, list):
        return [_scrub(v) for v in value]
    if isinstance(value, str) and value.startswith(str(KILL_PATH)):
        return value.replace(str(KILL_PATH), KILL_FILE)
    if isinstance(value, str) and str(KILL_PATH) in value:
        return value.replace(str(KILL_PATH), KILL_FILE)
    return value


KILL_PATH: Path = Path()


def run_case(risk, base, case: dict) -> dict:
    """Run one session of guard calls and record every answer."""
    now = {"t": T0}
    overrides = dict(case.get("limits", {}))
    file_present = bool(overrides.pop("kill_switch_file_present", False))
    for key in ("allowed_exchanges", "allowed_products", "symbol_allowlist", "symbol_blocklist"):
        if key in overrides:
            overrides[key] = frozenset(overrides[key])
    for key in ("max_order_value", "max_price_deviation_pct", "max_funds_utilization_pct"):
        if key in overrides:
            overrides[key] = Decimal(overrides[key])
    limits = replace(base, kill_switch_file=str(KILL_PATH), **overrides)
    if file_present:
        KILL_PATH.write_text("")
    elif KILL_PATH.exists():
        KILL_PATH.unlink()

    guard = risk.RiskGuard(session_id="vectors", limits=limits, clock=lambda: now["t"])
    verdicts = []
    steps = []
    for step in case["steps"]:
        now["t"] = T0 + timedelta(seconds=step.get("at", 0))
        call = step["call"]
        if call == "check_order":
            v = guard.check_order(
                **step["args"],
                analyzer_mode=step.get("analyzer_mode"),
                claim=step.get("claim", True),
            )
            verdicts.append(v)
            answer = v.as_dict()
            answer["message"] = v.as_message()
        elif call == "check_basket":
            v = guard.check_basket(
                step["legs"],
                available_funds=step.get("available_funds"),
                analyzer_mode=step.get("analyzer_mode"),
                claim=step.get("claim", True),
            )
            verdicts.append(v)
            answer = v.as_dict()
            answer["message"] = v.as_message()
        elif call == "check_destructive":
            v = guard.check_destructive(
                step["operation"],
                analyzer_mode=step.get("analyzer_mode"),
                **step.get("details", {}),
            )
            verdicts.append(v)
            answer = v.as_dict()
            answer["message"] = v.as_message()
        elif call in ("release", "commit"):
            target = verdicts[step["of"]]
            answer = getattr(guard, call)(target)
            verdicts.append(None)
        else:
            raise ValueError(call)
        steps.append({**step, "expect": _scrub(answer), "orders_claimed": guard.orders_claimed})
    if KILL_PATH.exists():
        KILL_PATH.unlink()
    return {"name": case["name"], "limits": _limits_json(limits) | {"kill_switch_file_present": file_present}, "steps": steps}


def order(**kw) -> dict:
    o = {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 10, "product": "MIS"}
    o.update(kw)
    return o


def check(at=0, analyzer=None, claim=True, **kw) -> dict:
    step = {"call": "check_order", "args": order(**kw), "at": at, "claim": claim}
    if analyzer is not None:
        step["analyzer_mode"] = analyzer
    return step


def destructive(op, at=0, analyzer=None, **details) -> dict:
    step = {"call": "check_destructive", "operation": op, "at": at, "details": details}
    if analyzer is not None:
        step["analyzer_mode"] = analyzer
    return step


def basket(legs, at=0, funds=None, analyzer=None) -> dict:
    step = {"call": "check_basket", "legs": legs, "at": at}
    if funds is not None:
        step["available_funds"] = funds
    if analyzer is not None:
        step["analyzer_mode"] = analyzer
    return step


ON = {"trading_enabled": True}

CASES: list[dict] = [
    # Rules 1 to 3: the gate.
    {"name": "kill switch setting", "limits": {**ON, "kill_switch_engaged": True},
     "steps": [check(), destructive("cancel_order", orderid="A1")]},
    {"name": "kill switch file", "limits": {**ON, "kill_switch_file_present": True},
     "steps": [check(), destructive("close_all_positions")]},
    {"name": "kill switch setting wins over the file", "limits": {**ON, "kill_switch_engaged": True, "kill_switch_file_present": True},
     "steps": [check()]},
    {"name": "trading switched off", "limits": {},
     "steps": [check(), destructive("cancel_order", orderid="A1")]},
    {"name": "analyzer required and unreadable", "limits": {**ON, "require_analyzer_mode": True},
     "steps": [check(), destructive("cancel_order")]},
    {"name": "analyzer required in live mode", "limits": {**ON, "require_analyzer_mode": True},
     "steps": [check(analyzer=False), destructive("cancel_order", analyzer=False)]},
    {"name": "analyzer required in analyzer mode", "limits": {**ON, "require_analyzer_mode": True},
     "steps": [check(analyzer=True, ltp=500, available_funds=100000)]},
    {"name": "analyzer not required, mode unknown", "limits": ON,
     "steps": [check(ltp=500, available_funds=100000)]},
    # Normalisation, between rules 3 and 4.
    {"name": "normalisation refusals", "limits": ON, "steps": [
        check(symbol=""), check(symbol=None), check(symbol="  "),
        check(exchange=""), check(exchange=None),
        check(action="HOLD"), check(action=None), check(action="b"),
        check(product=""), check(product=None),
        check(price_type="STOP"), check(price_type="sl_m", trigger_price=100),
        check(quantity="abc"), check(quantity=0), check(quantity=-5), check(quantity=10.0),
        check(quantity=True), check(quantity="7"), check(quantity=None),
        check(price_type="LIMIT"), check(price_type="LIMIT", price=-5),
        check(price_type="SL", price=100), check(price_type="SL-M"),
        check(price_type="SL-M", trigger_price=0), check(price_type="SLM", trigger_price=101.5, ltp=100),
        check(price_type=None, ltp=200),
    ]},
    # Rules 4 to 7: the instrument.
    {"name": "symbol blocklist", "limits": {**ON, "symbol_blocklist": ["SBIN"]},
     "steps": [check(symbol="sbin")]},
    {"name": "symbol allowlist", "limits": {**ON, "symbol_allowlist": ["TCS", "INFY"]},
     "steps": [check(), check(symbol="tcs", ltp=3000)]},
    {"name": "blocklist before allowlist", "limits": {**ON, "symbol_allowlist": ["SBIN"], "symbol_blocklist": ["SBIN"]},
     "steps": [check()]},
    {"name": "exchange not allowed", "limits": ON,
     "steps": [check(exchange="NSE_INDEX", symbol="NIFTY"), check(exchange="nfo", symbol="NIFTY28MAR2420800CE", product="NRML", ltp=120)]},
    {"name": "exchange narrowed", "limits": {**ON, "allowed_exchanges": ["NFO"]},
     "steps": [check()]},
    {"name": "product not allowed", "limits": {**ON, "allowed_products": ["CNC"]},
     "steps": [check(), check(product="cnc", ltp=500)]},
    {"name": "quantity cap", "limits": {**ON, "max_order_quantity": 100},
     "steps": [check(quantity=101), check(quantity=100, ltp=10)]},
    # Rules 8 and 9: the claim.
    {"name": "session cap", "limits": {**ON, "max_orders_per_session": 2},
     "steps": [check(ltp=10), check(symbol="TCS", ltp=10), check(symbol="INFY", ltp=10),
               destructive("cancel_order", orderid="A1")]},
    {"name": "session cap of zero", "limits": {**ON, "max_orders_per_session": 0},
     "steps": [check(ltp=10), destructive("modify_order", orderid="A1"), destructive("close_position", symbol="SBIN")]},
    {"name": "duplicate window", "limits": ON, "steps": [
        check(ltp=10), check(at=3.5, ltp=10), check(at=9.94, ltp=10), check(at=10, ltp=10),
        check(at=10.5, ltp=10, quantity=11), check(at=12, ltp=10, price_type="LIMIT", price=10),
        check(at=13, ltp=10, price_type="LIMIT", price=10),
    ]},
    {"name": "duplicate window off", "limits": {**ON, "duplicate_order_window_seconds": 0},
     "steps": [check(ltp=10), check(ltp=10)]},
    {"name": "release gives the claim back", "limits": {**ON, "max_orders_per_session": 1},
     "steps": [check(ltp=10), {"call": "release", "of": 0}, {"call": "release", "of": 0},
               check(at=1, ltp=10), {"call": "commit", "of": 3}, {"call": "release", "of": 3},
               check(at=2, symbol="TCS", ltp=10)]},
    {"name": "a refusal has nothing to release", "limits": ON,
     "steps": [check(symbol=""), {"call": "release", "of": 0}, {"call": "commit", "of": 0}]},
    {"name": "a dry run claims nothing", "limits": {**ON, "max_orders_per_session": 1},
     "steps": [check(claim=False, ltp=10), {"call": "release", "of": 0}, check(ltp=10), check(at=1, ltp=10, claim=False)]},
    # Rule 10: notional and deviation.
    {"name": "notional cap", "limits": ON, "steps": [
        check(price_type="LIMIT", price=1000, quantity=600, ltp=1000),
        check(price_type="LIMIT", price=1000, quantity=500, ltp=1000),
        check(at=1, quantity=600, ltp=1000.5, symbol="TCS"),
        check(at=2, price_type="SL-M", trigger_price=2000, quantity=300, symbol="INFY"),
        check(at=3, quantity=5, symbol="WIPRO"),
    ]},
    {"name": "a pricing refusal gives the claim back", "limits": {**ON, "max_orders_per_session": 1},
     "steps": [check(price_type="LIMIT", price=1000, quantity=600, ltp=1000),
               check(price_type="LIMIT", price=1000, quantity=600, ltp=500, at=1)]},
    {"name": "price deviation", "limits": ON, "steps": [
        check(price_type="LIMIT", price=1100, ltp=1000),
        check(price_type="LIMIT", price=1050, ltp=1000, symbol="TCS"),
        check(price_type="SL", price=940, trigger_price=945, ltp=1000, symbol="INFY"),
        check(price_type="SL-M", trigger_price=1070.25, ltp=1000, symbol="WIPRO"),
        check(price_type="LIMIT", price=101, ltp=0, symbol="ITC", quantity=1),
        check(price_type="LIMIT", price=1450.55, ltp=1500.123, symbol="HDFC", quantity=1),
    ]},
    {"name": "deviation limit decimal", "limits": {**ON, "max_price_deviation_pct": "2.5"},
     "steps": [check(price_type="LIMIT", price=1026, ltp=1000, quantity=1)]},
    # Rule 11: affordability.
    {"name": "affordability", "limits": {**ON, "max_funds_utilization_pct": "50"}, "steps": [
        check(ltp=1000, quantity=100, available_funds=150000),
        check(ltp=1000, quantity=100, available_funds=200000, symbol="TCS"),
        check(ltp=1000, quantity=100, action="SELL", available_funds=10, symbol="INFY"),
        check(ltp=1000, quantity=100, symbol="WIPRO"),
        check(ltp=1000, quantity=1, available_funds="abc", symbol="ITC"),
        check(ltp=1000, quantity=1, available_funds=True, symbol="HDFC"),
        check(ltp=1000, quantity=1, available_funds=-100, symbol="AXIS"),
    ]},
    {"name": "affordability default", "limits": ON,
     "steps": [check(ltp=333.33, quantity=3, available_funds=999.98)]},
    # Destructive operations.
    {"name": "destructive operations", "limits": ON, "steps": [
        destructive("cancel_order", orderid="A1"),
        destructive("modify_order", orderid="A1", symbol="SBIN", exchange="NSE", quantity=5),
        destructive("close_position", symbol="SBIN", exchange="NSE", product="MIS"),
        destructive("cancel_all_orders"), destructive("close_all_positions"),
        destructive("square_off_all"), destructive(" Cancel_Order "),
        destructive("nuke"), destructive(None), destructive("cancel_gtt_order", trigger_id="T1"),
    ]},
    {"name": "bulk operations allowed", "limits": {**ON, "allow_bulk_destructive": True},
     "steps": [destructive("cancel_all_orders"), destructive("close_all_positions")]},
    # The basket check (no agent tool calls it; its codes are pinned).
    {"name": "basket", "limits": ON, "steps": [
        basket([]),
        basket([order(), "not a leg"]),
        basket([order(), order(symbol="TCS", colour="red")]),
        basket([order(), {"exchange": "NSE", "action": "BUY", "quantity": 1, "product": "MIS"}]),
        basket([order(), {"action": "BUY", "quantity": 1}]),
        basket([order(), order(symbol="TCS", quantity=0)]),
        basket([order(), order(symbol="TCS", exchange="NSE_INDEX")]),
        basket([order(ltp=10), order(ltp=10)]),
        basket([order(ltp=10), order(symbol="TCS", ltp=20, available_funds=500)], funds=None),
        basket([order(ltp=1000, symbol="A1"), order(symbol="A2", ltp=1000)], funds=15000, at=1),
        basket([order(ltp=1000, quantity=600, symbol="B1", price_type="LIMIT", price=1000)], at=2),
    ]},
    {"name": "basket cap", "limits": {**ON, "max_orders_per_session": 2},
     "steps": [basket([order(ltp=1), order(symbol="TCS", ltp=1), order(symbol="INFY", ltp=1)])]},
]


# ------------------------------------------------------------ order arguments


def capture_arguments(orders_mod, vocab, retry_cls) -> list[dict]:
    toolkit = orders_mod.OrdersToolkit.__new__(orders_mod.OrdersToolkit)
    toolkit.strategy = orders_mod.AGENT_STRATEGY

    def run(fn, *args, **kwargs):
        try:
            return {"ok": fn(*args, **kwargs)}
        except retry_cls as exc:
            return {"retry": str(exc)}

    base = {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 10,
            "product": "MIS", "price_type": "MARKET", "price": 0.0, "trigger_price": 0.0}
    builds = [
        {}, {"action": "s", "product": "intraday", "price_type": "SL M", "trigger_price": 1440.0},
        {"price": None, "trigger_price": None}, {"symbol": "NSE:SBIN", "exchange": ""},
        {"symbol": "nse:sbin", "exchange": "nse"}, {"symbol": "NSE:SBIN", "exchange": "BSE"},
        {"symbol": "NFO:NIFTY28MAR2420800CE", "exchange": "nfo", "product": "NRML"},
        {"symbol": "NIFTY", "exchange": "NSE_INDEX"}, {"symbol": "NIFTY", "exchange": "BSE_INDEX"},
        {"symbol": "CRUDE", "exchange": "MCX_INDEX"}, {"symbol": "US30", "exchange": "GLOBAL_INDEX"},
        {"exchange": "NSEX"}, {"exchange": ""}, {"exchange": None}, {"symbol": ""}, {"symbol": "   "},
        {"action": "HOLD"}, {"action": ""}, {"product": "BOGUS"}, {"product": ""},
        {"product": "delivery"}, {"product": "Carry_Forward"}, {"price_type": "BOGUS"},
        {"price_type": ""}, {"price_type": "stop loss limit", "price": 100.0, "trigger_price": 99.0},
        {"price": 1450.5}, {"price": 100.0}, {"price": 1234567.0}, {"price": 1e-05},
        {"trigger_price": 99.0, "price_type": "LIMIT", "price": 100.0},
        {"price_type": "LIMIT"}, {"price_type": "LIMIT", "price": 1450.5},
        {"price_type": "SL", "price": 100.0}, {"price_type": "SL", "trigger_price": 99.0},
        {"price_type": "SL-M"}, {"price_type": "SL-M", "trigger_price": 99.0, "price": 100.0},
        {"price": -5.0}, {"trigger_price": -1.5}, {"price": ""}, {"quantity": 0},
        {"quantity": -3}, {"quantity": 2.5}, {"quantity": "5"}, {"quantity": 5.0},
        {"quantity": None}, {"quantity": "abc"}, {"quantity": True},
    ]
    vectors = []
    for change in builds:
        args = {**base, **change}
        vectors.append({"fn": "build_order", "args": args, "expect": run(toolkit._build_order, **args)})
    for change in ({"quantity": 0}, {"quantity": 0, "action": "SELL"}):
        args = {**base, **change}
        vectors.append({"fn": "build_order_zero_ok", "args": args,
                        "expect": run(toolkit._build_order, allow_zero_quantity=True, **args)})
    for value in (240307000616990, "  A1  ", True, "", None, "   ", 0, 12.0):
        vectors.append({"fn": "order_id", "args": {"value": value}, "expect": run(toolkit._order_id, value)})
    for value in (-75, 0, 150, "20", None, 2.5):
        vectors.append({"fn": "position_size", "args": {"value": value},
                        "expect": run(toolkit._whole_number, "position_size", value, minimum=None)})
    for value in ("INTRADAY", "intraday", "DELIVERY", "CARRYFORWARD", "CARRY_FORWARD", "NORMAL",
                  " mis ", "CNC", "BOGUS", None):
        vectors.append({"fn": "canonical_product", "args": {"value": value}, "expect": {"ok": vocab.canonical_product(value)}})
    for value in ("SL_M", "SL M", "SLM", "sl-m", "STOPLOSS_MARKET", "STOP_LOSS_MARKET", "STOPLOSS",
                  "Stop Loss", "STOPLOSS_LIMIT", "stop-loss-limit", "MARKET", "LIMIT", "SL", "BOGUS", None, "  sl  -  m "):
        vectors.append({"fn": "canonical_price_type", "args": {"value": value}, "expect": {"ok": vocab.canonical_price_type(value)}})
    for value in ("B", "s", "buy", "X", None):
        vectors.append({"fn": "canonical_action", "args": {"value": value}, "expect": {"ok": vocab.canonical_action(value)}})
    for sym, exch in (("nse:sbin", ""), ("NSE:SBIN", None), ("NFO:NIFTY28MAR2420800CE", "nfo"),
                      ("NSE:SBIN", "BSE"), ("SBIN", "NSE"), (" nse : sbin ", " "), ("A:B:C", "")):
        vectors.append({"fn": "split_exchange_prefix", "args": {"symbol": sym, "exchange": exch},
                        "expect": run(lambda s, e: list(vocab.split_exchange_prefix(s, e)), sym, exch)})
    for exch in ("NSE_INDEX", "BSE_INDEX", "MCX_INDEX", "GLOBAL_INDEX", "NSE"):
        vectors.append({"fn": "index_refusal", "args": {"exchange": exch}, "expect": {"ok": orders_mod.index_refusal(exch)}})
    vectors.append({"fn": "refusal_message", "args": {"detail": "Invalid quantity."},
                    "expect": {"ok": orders_mod._refusal_message("Invalid quantity.")}})
    vectors.append({"fn": "refusal_message", "args": {"detail": ""},
                    "expect": {"ok": orders_mod._refusal_message("")}})
    return vectors


def main(web_root: str) -> None:
    global KILL_PATH
    sys.path.insert(0, web_root)
    work = tempfile.mkdtemp(prefix="capture-web-orders-")
    os.chdir(work)
    KILL_PATH = Path(work) / "agent_kill_switch"

    from agno.exceptions import RetryAgentRun  # noqa: PLC0415 - on the path only now

    import database.settings_db as settings_db  # noqa: PLC0415
    from services.agent import settings  # noqa: PLC0415
    from services.agent import tools as tools_pkg  # noqa: PLC0415
    from services.agent.safety import risk  # noqa: PLC0415
    from services.agent.tools import order_vocab, orders  # noqa: PLC0415

    def unreadable():
        raise RuntimeError("analyzer toggle unreadable in the capture")

    # A guard handed no analyzer mode reads the toggle itself; the capture
    # makes that read fail, which is what None means on the desktop.
    settings_db.get_analyze_mode = unreadable

    SPECS.parent.mkdir(parents=True, exist_ok=True)
    SPECS.write_text(json.dumps(capture_specs(orders, tools_pkg), indent=2, ensure_ascii=False) + "\n")

    base = settings.default_risk_limits()
    vectors = {
        "_comment": "Captured from the web's services/agent/safety/risk.py by "
        "scripts/agent/capture_web_orders.py. Regenerate only from the web.",
        "start": T0.isoformat(),
        "defaults": _limits_json(base),
        "cases": [run_case(risk, base, case) for case in CASES],
    }
    FIXTURES.mkdir(parents=True, exist_ok=True)
    (FIXTURES / "risk_vectors.json").write_text(json.dumps(vectors, indent=1, ensure_ascii=False) + "\n")

    arguments = {
        "_comment": "Captured from the web's services/agent/tools/orders.py and order_vocab.py "
        "by scripts/agent/capture_web_orders.py. Regenerate only from the web.",
        "strategy": orders.AGENT_STRATEGY,
        "vectors": capture_arguments(orders, order_vocab, RetryAgentRun),
    }
    (FIXTURES / "order_arguments.json").write_text(json.dumps(arguments, indent=1, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main(sys.argv[1])
