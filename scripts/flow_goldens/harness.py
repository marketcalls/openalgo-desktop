"""Run OpenAlgo web's own Flow executor against scripted stand-ins and record
what it did, for the desktop's replay goldens.

Nothing here touches real data or the network:
- every database URL points at a temporary SQLite file;
- the service modules FlowOpenAlgoClient imports lazily are replaced by
  recording fakes, so the web's real client (its `_handle_response`,
  `get_open_position`, margin and history guards) runs over scripted replies;
- sockets refuse everything except the local HTTP stub on an ephemeral port;
- the clock is frozen per scenario (IST, the process runs with TZ=Asia/Kolkata).

Run with the web checkout's interpreter in isolated mode, against an export
of the web at the recorded commit (never the web checkout itself):
    git -C <web> archive 96e5927e5 services database utils blueprints restx_api \
        subscribers sandbox broker/__init__.py docs/prompt .claude/skills/flow-builder \
        | tar -x -C scripts/flow_goldens/web
    <web>/.venv/bin/python -I scripts/flow_goldens/record.py tests/fixtures/web/flow/goldens
FLOW_GOLDENS_WEB names another export folder.
"""

from __future__ import annotations

import copy
import datetime as _dt
import http.server
import json
import math
import os
import socket
import sys
import tempfile
import threading
import types
from zoneinfo import ZoneInfo

HERE = os.path.dirname(os.path.abspath(__file__))
WEB = os.environ.get("FLOW_GOLDENS_WEB") or os.path.join(HERE, "web")
TMP = tempfile.mkdtemp(prefix="flow-goldens-")
IST = ZoneInfo("Asia/Kolkata")

os.environ["TZ"] = "Asia/Kolkata"
import time as _time  # noqa: E402

_time.tzset()
os.environ["DATABASE_URL"] = f"sqlite:///{TMP}/openalgo.db"
os.environ["LATENCY_DATABASE_URL"] = f"sqlite:///{TMP}/latency.db"
os.environ["LOGS_DATABASE_URL"] = f"sqlite:///{TMP}/logs.db"
os.environ["SANDBOX_DATABASE_URL"] = f"sqlite:///{TMP}/sandbox.db"
os.environ["HISTORIFY_DATABASE_URL"] = f"{TMP}/historify.duckdb"
os.environ["LOG_TO_FILE"] = "False"
os.environ["LOG_DIR"] = f"{TMP}/log"
os.environ["SESSION_EXPIRY_TIME"] = "03:00"
os.environ["API_KEY_PEPPER"] = "golden-" + "x" * 57
sys.path.insert(0, WEB)

# --- No network ---------------------------------------------------------------

_real_getaddrinfo = socket.getaddrinfo
_real_connect = socket.socket.connect
_ALLOWED_PORTS: set[int] = set()
STUB_HOSTS: dict[str, list[str]] = {}
_RESOLVE_LOG: list[tuple[str, int]] = []


def _is_ip(host: str) -> bool:
    try:
        import ipaddress

        ipaddress.ip_address(host)
        return True
    except ValueError:
        return False


def _guarded_getaddrinfo(host, port, family=0, type=0, proto=0, flags=0):
    """Literal IPs resolve to themselves; stub host names to the scenario's
    addresses for the executor's destination check, and to the local stub
    server when httpx connects; any other name fails as unresolvable."""
    host_s = host.decode() if isinstance(host, bytes) else str(host)
    if proto == socket.IPPROTO_TCP and type == 0:
        # The executor's _check_http_destination passes proto= only.
        if host_s in STUB_HOSTS:
            return [
                (socket.AF_INET6 if ":" in a else socket.AF_INET, socket.SOCK_STREAM, 6, "", (a, port or 0))
                for a in STUB_HOSTS[host_s]
            ]
    if host_s in STUB_HOSTS or host_s in ("127.0.0.1", "localhost"):
        stub = _TLS_STUB if port == 443 else _STUB
        stub_port = stub.port if stub else 0
        return [(socket.AF_INET, socket.SOCK_STREAM, 6, "", ("127.0.0.1", stub_port))]
    if _is_ip(host_s):
        fam = socket.AF_INET6 if ":" in host_s else socket.AF_INET
        return [(fam, socket.SOCK_STREAM, 6, "", (host_s, port or 0))]
    raise socket.gaierror(socket.EAI_NONAME, "nodename nor servname provided, or not known")


def _guarded_connect(self, address):
    if isinstance(address, tuple) and address[0] == "127.0.0.1" and address[1] in _ALLOWED_PORTS:
        return _real_connect(self, address)
    raise ConnectionRefusedError(f"goldens recorder: network blocked ({address!r})")


socket.getaddrinfo = _guarded_getaddrinfo
socket.socket.connect = _guarded_connect

# --- Frozen clock ---------------------------------------------------------------

_FROZEN = [_dt.datetime(2026, 10, 29, 10, 15, 0)]  # naive IST


class FrozenDatetime(_dt.datetime):
    @classmethod
    def now(cls, tz=None):
        base = _FROZEN[0]
        if tz is None:
            return cls(base.year, base.month, base.day, base.hour, base.minute, base.second, base.microsecond)
        aware = _dt.datetime(
            base.year, base.month, base.day, base.hour, base.minute, base.second, base.microsecond, tzinfo=IST
        ).astimezone(tz)
        return cls(
            aware.year, aware.month, aware.day, aware.hour, aware.minute, aware.second, aware.microsecond, tzinfo=aware.tzinfo
        )

    @classmethod
    def today(cls):
        return cls.now()

    @classmethod
    def utcnow(cls):
        return cls.now(_dt.timezone.utc).replace(tzinfo=None)


class FrozenDate(_dt.date):
    @classmethod
    def today(cls):
        b = _FROZEN[0]
        return cls(b.year, b.month, b.day)


def set_clock(iso: str) -> None:
    d = _dt.datetime.fromisoformat(iso)
    if d.tzinfo is not None:
        d = d.astimezone(IST).replace(tzinfo=None)
    _FROZEN[0] = d


# --- Recording fakes for the services the client calls ------------------------

STATE: dict = {}


def _reset_state(scenario: dict) -> None:
    STATE.clear()
    STATE["calls"] = []
    STATE["answered"] = []
    STATE["alerts"] = []
    STATE["feed"] = []
    STATE["http"] = []
    STATE["sleeps"] = []
    queues: dict[str, list] = {}
    for entry in scenario.get("script", []):
        queues.setdefault(entry["endpoint"], []).append(copy.deepcopy(entry["reply"]))
    STATE["queues"] = queues
    STATE["defaults"] = scenario.get("defaults", {})
    STATE["env"] = copy.deepcopy(scenario.get("env", {}))
    STATE["alert_reply"] = scenario.get("alert_reply")


UNSCRIPTED = {"status": "error", "error": "No reply was scripted for this call", "code": 500}

ENDPOINTS = {
    "place_order": "placeorder",
    "place_smart_order": "placesmartorder",
    "modify_order": "modifyorder",
    "cancel_order": "cancelorder",
    "cancel_all_orders": "cancelallorder",
    "close_all_positions": "closeposition",
    "close_position": "closeposition",
    "basket_order": "basketorder",
    "split_order": "splitorder",
    "get_quotes": "quotes",
    "get_depth": "depth",
    "get_history": "history",
    "orderbook": "orderbook",
    "tradebook": "tradebook",
    "positionbook": "positionbook",
    "holdings": "holdings",
    "funds": "funds",
    "optionchain": "optionchain",
    "optionsymbol": "optionsymbol",
    "options_order": "optionsorder",
    "options_multi_order": "optionsmultiorder",
    "holidays": "market/holidays",
    "timings": "market/timings",
    "margin": "margin",
    "get_multi_quotes": "multiquotes",
    "get_order_status": "orderstatus",
    "symbol": "symbol",
    "get_intervals": "intervals",
    "get_expiry": "expiry",
    "syntheticfuture": "syntheticfuture",
}


def _shaped(reply: dict) -> dict:
    """What the web client hands the node for this reply (its
    _handle_response): a failure always carries `error` (`Unknown error` when
    the service gave no message) and `code`. The golden's script holds this
    shape, which is what the desktop's FakeFlowClient answers with."""
    if "$raise" in reply or reply.get("status") == "success":
        return reply
    message = reply.get("error", reply.get("message"))
    return {"status": "error", "error": "Unknown error" if message is None else message, "code": reply.get("code", 400)}


def _next_reply(endpoint: str) -> dict:
    """The desktop FakeFlowClient's rule: replies queued per endpoint, the
    last one repeats; an endpoint with no script answers UNSCRIPTED."""
    q = STATE["queues"].get(endpoint)
    if q:
        reply = q.pop(0) if len(q) > 1 else copy.deepcopy(q[0])
    elif endpoint in STATE["defaults"]:
        reply = copy.deepcopy(STATE["defaults"][endpoint])
    else:
        reply = copy.deepcopy(UNSCRIPTED)
    reply = _shaped(reply)
    STATE["answered"].append({"endpoint": endpoint, "reply": copy.deepcopy(reply)})
    return reply


def _as_service_tuple(shaped: dict):
    """Turn the shaped client dict back into the (ok, response, code) tuple a
    service returns, so the web client's own _handle_response rebuilds it."""
    if shaped.get("status") == "success":
        return True, dict(shaped), 200
    code = shaped.get("code", 400)
    message = shaped.get("error", shaped.get("message"))
    resp = {"status": "error"}
    if message is not None:
        resp["message"] = message
    return False, resp, code


def _strip_key(d: dict) -> dict:
    return {k: copy.deepcopy(v) for k, v in d.items() if k != "apikey"}


def _service(builder):
    def fake(*args, **kwargs):
        method = sys._getframe(1).f_code.co_name
        payload = builder(*args, **kwargs)
        endpoint = ENDPOINTS[method]
        STATE["calls"].append({"method": method, "endpoint": endpoint, "payload": payload})
        reply = _next_reply(endpoint)
        if "$raise" in reply:
            # A service that raises (the web client lets it propagate; the
            # desktop client never raises, parity CL-26).
            raise RuntimeError(reply["$raise"])
        return _as_service_tuple(reply)

    return fake


def _dict_payload(data=None, api_key=None, **_):
    return _strip_key(data or {})


def _mod(name: str, **attrs) -> types.ModuleType:
    m = types.ModuleType(name)
    for k, v in attrs.items():
        setattr(m, k, v)
    sys.modules[name] = m
    return m


def _install_service_fakes() -> None:
    from services import flow_node_contracts

    _mod("services.place_order_service", place_order=_service(_dict_payload))
    _mod("services.place_smart_order_service", place_smart_order=_service(_dict_payload))
    _mod("services.modify_order_service", modify_order=_service(_dict_payload))
    _mod("services.cancel_order_service", cancel_order=_service(lambda order_id, api_key=None: {"orderid": order_id}))
    _mod("services.cancel_all_order_service", cancel_all_orders=_service(lambda api_key=None: {}))
    _mod("services.close_position_service", close_position=_service(_dict_payload))
    _mod("services.basket_order_service", place_basket_order=_service(_dict_payload))
    _mod("services.split_order_service", split_order=_service(_dict_payload))
    _mod(
        "services.quotes_service",
        get_quotes=_service(lambda symbol, exchange, api_key=None: {"symbol": symbol, "exchange": exchange}),
        get_multiquotes=_service(lambda symbols, api_key=None: {"symbols": copy.deepcopy(symbols)}),
    )
    _mod("services.depth_service", get_depth=_service(lambda symbol, exchange, api_key=None: {"symbol": symbol, "exchange": exchange}))
    _mod(
        "services.history_service",
        get_history=_service(
            lambda symbol, exchange, interval, start_date=None, end_date=None, api_key=None, source="api": {
                "symbol": symbol,
                "exchange": exchange,
                "interval": interval,
                "start_date": start_date,
                "end_date": end_date,
                "source": source,
            }
        ),
    )
    for svc, fn in (
        ("orderbook_service", "get_orderbook"),
        ("tradebook_service", "get_tradebook"),
        ("positionbook_service", "get_positionbook"),
        ("holdings_service", "get_holdings"),
        ("funds_service", "get_funds"),
    ):
        _mod(f"services.{svc}", **{fn: _service(lambda api_key=None: {})})
    _mod(
        "services.option_chain_service",
        get_option_chain=_service(
            lambda underlying, exchange, expiry_date="", strike_count=10, api_key=None: {
                "underlying": underlying,
                "exchange": exchange,
                "expiry_date": expiry_date,
                "strike_count": strike_count,
            }
        ),
    )
    _mod(
        "services.option_symbol_service",
        parse_underlying_symbol=flow_node_contracts.parse_underlying_symbol,
        get_option_symbol=_service(
            lambda underlying, exchange, expiry_date, strike_int=None, offset="ATM", option_type="CE", api_key=None: {
                "underlying": underlying,
                "exchange": exchange,
                "expiry_date": expiry_date,
                "strike_int": strike_int,
                "offset": offset,
                "option_type": option_type,
            }
        ),
    )
    _mod("services.place_options_order_service", place_options_order=_service(_dict_payload))
    _mod("services.options_multiorder_service", place_options_multiorder=_service(_dict_payload))

    def _holidays(year=None):
        # The calendar node's own holiday read (desktop: its trading calendar
        # port over the desktop's holiday data), not a client call.
        table = STATE["env"].get("calendar_holidays", {})
        entries = table.get(str(year))
        if entries == "raise":
            raise RuntimeError("market calendar table is missing")
        if entries == "unavailable":
            return False, {"status": "error", "message": "unavailable"}, 500
        return True, {"status": "success", "data": entries or []}, 200

    # _service reads the caller frame one level up; _holidays adds a frame,
    # so the client path calls the inner fake through a shim with its name.
    def holidays_entry(year=None):
        caller = sys._getframe(1)
        if caller.f_code.co_name == "_calendar_entries":
            return _holidays(year)
        STATE["calls"].append({"method": "holidays", "endpoint": "market/holidays", "payload": {"year": year}})
        return _as_service_tuple(_next_reply("market/holidays"))

    def timings_entry(date_str):
        STATE["calls"].append({"method": "timings", "endpoint": "market/timings", "payload": {"date": date_str}})
        return _as_service_tuple(_next_reply("market/timings"))

    _mod("services.market_calendar_service", get_holidays=holidays_entry, get_timings=timings_entry)
    _mod("services.margin_service", calculate_margin=_service(_dict_payload))
    _mod("services.orderstatus_service", get_order_status=_service(_dict_payload))
    _mod("services.symbol_service", get_symbol_info=_service(lambda symbol, exchange, api_key=None: {"symbol": symbol, "exchange": exchange}))
    _mod("services.intervals_service", get_intervals=_service(lambda api_key=None: {}))
    _mod(
        "services.expiry_service",
        get_expiry_dates=_service(
            lambda symbol, exchange, instrumenttype, api_key=None: {
                "symbol": symbol,
                "exchange": exchange,
                "instrumenttype": instrumenttype,
            }
        ),
    )
    _mod(
        "services.synthetic_future_service",
        calculate_synthetic_future=_service(
            lambda underlying, exchange, expiry_date, api_key=None: {
                "underlying": underlying,
                "exchange": exchange,
                "expiry_date": expiry_date,
            }
        ),
    )

    # Strategy book (strategyPnl): legs from the scenario's env.
    class StrategyBookUnavailable(Exception):
        pass

    def get_strategy_legs(user_id=None, strategy=None):
        book = STATE["env"].get("strategy_book", [])
        if isinstance(book, dict) and "unavailable" in book:
            raise StrategyBookUnavailable(book["unavailable"])
        STATE["feed"].append({"op": "strategy_book", "strategy": strategy})
        return [copy.deepcopy(leg) for leg in book if not strategy or leg.get("strategy") == strategy]

    _mod(
        "database.strategy_book_db",
        StrategyBookUnavailable=StrategyBookUnavailable,
        get_strategy_legs=get_strategy_legs,
    )

    # Feed (the subscribe and unsubscribe nodes): the session, the broker
    # and scripted ticks from env.feed.
    def feed():
        return STATE["env"].get("feed", {})

    def verify_api_key(key):
        if feed().get("verify_raise"):
            raise RuntimeError(feed()["verify_raise"])
        return feed().get("username", "trader")

    def get_broker_name(key):
        return feed().get("broker", "fakebroker")

    _mod(
        "database.auth_db",
        verify_api_key=verify_api_key,
        get_broker_name=get_broker_name,
        get_username_by_apikey=verify_api_key,
    )

    class FakeWs:
        def __init__(self):
            self.callbacks = []

        def register_callback(self, kind, cb):
            self.callbacks.append(cb)

        def unregister_callback(self, kind, cb):
            if cb in self.callbacks:
                self.callbacks.remove(cb)

    def get_websocket_connection(username):
        f = feed()
        if f.get("connect_error"):
            return False, None, f["connect_error"]
        ws = FakeWs()
        STATE["ws"] = ws
        return True, ws, None

    def subscribe_to_symbols(username, broker, symbols, mode):
        f = feed()
        STATE["feed"].append({"op": "subscribe", "symbols": copy.deepcopy(symbols), "mode": mode})
        if f.get("subscribe_error"):
            return False, {"status": "error", "message": f["subscribe_error"]}, 400
        for tick in f.get("ticks", []):
            for cb in list(STATE["ws"].callbacks):
                cb(copy.deepcopy(tick))
        return True, {"status": "success"}, 200

    def unsubscribe_from_symbols(username, broker, symbols, mode):
        f = feed()
        STATE["feed"].append({"op": "unsubscribe", "symbols": copy.deepcopy(symbols), "mode": mode})
        if f.get("unsubscribe_raise"):
            raise RuntimeError(f["unsubscribe_raise"])
        if "unsubscribe_error" in f:
            return False, {"status": "error", "message": f["unsubscribe_error"]}, 400
        return True, {"status": "success"}, 200

    def unsubscribe_all(username, broker):
        f = feed()
        STATE["feed"].append({"op": "unsubscribe_all"})
        if f.get("unsubscribe_raise"):
            raise RuntimeError(f["unsubscribe_raise"])
        if "unsubscribe_error" in f:
            return False, {"status": "error", "message": f["unsubscribe_error"]}, 400
        return True, {"status": "success"}, 200

    _mod(
        "services.websocket_service",
        get_websocket_connection=get_websocket_connection,
        subscribe_to_symbols=subscribe_to_symbols,
        unsubscribe_from_symbols=unsubscribe_from_symbols,
        unsubscribe_all=unsubscribe_all,
    )


_install_service_fakes()

import services.flow_executor_service as fx  # noqa: E402
import services.flow_node_contracts as contracts  # noqa: E402
import services.flow_openalgo_client as fc  # noqa: E402
import services.indicator_service as ind  # noqa: E402
import utils.real_threading as real_threading  # noqa: E402
import utils.session as usession  # noqa: E402
import utils.trading_calendar as tcal  # noqa: E402
from services.flow_workflow_validator import validate_workflow  # noqa: E402

for _m in (fx, contracts, usession):
    _m.datetime = FrozenDatetime
ind.datetime = FrozenDatetime
ind.date = FrozenDate


def _alert(channel):
    def send(self, message, to=None):
        STATE["alerts"].append({"channel": channel, "message": message, "to": to})
        reply = STATE["alert_reply"]
        if reply is None:
            reply = {"status": "success", "data": {"message": "Alert queued successfully"}}
        if isinstance(reply, dict) and reply.get("raise"):
            raise RuntimeError(reply["raise"])
        return copy.deepcopy(reply)

    return send


fc.FlowOpenAlgoClient.telegram = _alert("telegram")
fc.FlowOpenAlgoClient.whatsapp = _alert("whatsapp")


class _FakeTime:
    @staticmethod
    def sleep(seconds):
        STATE["sleeps"].append(seconds)
        if seconds < 0:
            # What time.sleep says on a negative length (N-delay-05).
            raise ValueError("sleep length must be non-negative")


fx.time_module = _FakeTime


def _wait_for(event, timeout):
    STATE["feed"].append({"op": "wait", "timeout": timeout})
    return event.is_set()


real_threading.wait_for = _wait_for

# Master contract (lot sizes) in the temporary SQLite database.
from database.symbol import SymToken, db_session  # noqa: E402
from database.symbol import engine as _sym_engine  # noqa: E402

SymToken.__table__.create(bind=_sym_engine, checkfirst=True)


def _seed_master_contract(rows) -> None:
    db_session.query(SymToken).delete()
    for i, r in enumerate(rows or [], start=1):
        db_session.add(
            SymToken(
                id=i,
                symbol=r["symbol"],
                brsymbol=r.get("brsymbol", r["symbol"]),
                name=r.get("name", ""),
                exchange=r["exchange"],
                brexchange=r.get("brexchange", r["exchange"]),
                token=r.get("token", str(1000 + i)),
                expiry=r.get("expiry", ""),
                strike=r.get("strike", 0.0),
                lotsize=r.get("lotsize"),
                instrumenttype=r.get("instrumenttype", ""),
                tick_size=r.get("tick_size", 0.05),
            )
        )
    db_session.commit()
    db_session.remove()


# --- HTTP stub -------------------------------------------------------------------


class _StubHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):  # quiet
        pass

    def _handle(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b""
        spec = STATE.get("env", {}).get("http", {})
        configured = {k.lower() for k in spec.get("record_headers", [])} | {"content-type"}
        path, _, query = self.path.partition("?")
        STATE["http"].append(
            {
                "method": self.command,
                "path": path,
                "query": query,
                "headers": {k.lower(): v for k, v in self.headers.items() if k.lower() in configured},
                "body": body.decode("utf-8", "replace"),
            }
        )
        resp = spec.get("response", {"status": 200, "body": "{}", "content_type": "application/json"})
        if resp.get("delay"):
            _time.sleep(resp["delay"])
        out = resp.get("body", "").encode()
        self.send_response(resp.get("status", 200))
        self.send_header("Content-Type", resp.get("content_type", "application/json"))
        self.send_header("Content-Length", str(len(out)))
        for k, v in resp.get("headers", {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(out)

    do_GET = do_POST = do_PUT = do_PATCH = do_DELETE = _handle


class _QuietServer(http.server.ThreadingHTTPServer):
    def handle_error(self, request, client_address):  # a client that timed out
        pass


class _Stub:
    def __init__(self, wrap=None):
        self.server = _QuietServer(("127.0.0.1", 0), _StubHandler)
        if wrap:
            self.server.socket = wrap(self.server.socket)
        self.port = self.server.server_address[1]
        assert self.port not in (5000, 8765, 5500, 8766)
        _ALLOWED_PORTS.add(self.port)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()


_STUB = None
_TLS_STUB = None
_STUB = _Stub()


def _tls_stub():
    """The same stub behind TLS with a throwaway self-signed certificate for
    the stub host names, trusted only by the recorder's httpx clients."""
    import ssl
    import subprocess

    cert, key = f"{TMP}/stub.crt", f"{TMP}/stub.key"
    subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", "/CN=flow-golden",
         "-addext", "subjectAltName=DNS:audit.example.com,DNS:hooks.example.com,DNS:api.flow-golden.test",
         "-keyout", key, "-out", cert],
        check=True, capture_output=True,
    )
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(cert, key)
    stub = _Stub(wrap=lambda sock: ctx.wrap_socket(sock, server_side=True))
    import httpx

    client_ctx = ssl.create_default_context(cafile=cert)
    base = httpx.Client

    class TrustingClient(base):
        def __init__(self, *a, **k):
            k.setdefault("verify", client_ctx)
            super().__init__(*a, **k)

    httpx.Client = TrustingClient
    return stub


_TLS_STUB = _tls_stub()

# --- Running a scenario --------------------------------------------------------

API_KEY = "golden-api-key-placeholder"
WORKFLOW_ID = 1
PROBE = "goldenProbe"


def _reset_globals() -> None:
    fx._workflow_subscriptions.clear()
    fx._workflow_subscription_versions.clear()
    fx._workflow_subscription_owners.clear()
    with ind._history_cache_lock:
        ind._history_cache.clear()
    with ind._history_inflight_lock:
        ind._history_inflight.clear()
    for _k in list(tcal._HOLIDAY_CACHE.keys()):
        tcal._HOLIDAY_CACHE.pop(_k, None)


def _prepare(scenario: dict) -> None:
    _reset_state(scenario)
    _reset_globals()
    set_clock(scenario.get("clock", "2026-10-29T10:15:00"))
    env = scenario.get("env", {})
    STUB_HOSTS.clear()
    STUB_HOSTS.update(env.get("http", {}).get("resolve", {}))
    mc = env.get("master_contract")
    if mc == "unreadable":
        _seed_master_contract([])
    else:
        _seed_master_contract(mc or [])


_orig_chain = fx.execute_node_chain


def _probing_chain(node_id, nodes, edge_map, incoming_edge_map, executor, context, visited_count, depth=0):
    node = next((n for n in nodes if n["id"] == node_id), None)
    if node and node.get("type") == PROBE:
        STATE["next"].append(node["data"]["probe"])
        return None
    return _orig_chain(node_id, nodes, edge_map, incoming_edge_map, executor, context, visited_count, depth)


fx.execute_node_chain = _probing_chain


def _capture_results(executor, target_types):
    """Wrap the executor's handlers so the node's own returned dict is kept."""
    captured = []
    for name in dir(executor):
        if name.startswith("execute_") or name in ("unresolved_error", "option_expiry_error"):
            orig = getattr(executor, name)
            if not callable(orig):
                continue

            def wrapper(*a, _orig=orig, _name=name, **k):
                r = _orig(*a, **k)
                if r is not None:
                    captured.append((_name, copy.deepcopy(r)))
                return r

            setattr(executor, name, wrapper)
    return captured


def _broken_db():
    """The master contract cannot be read (N-common-22 (3))."""
    orig = db_session.query

    def boom(*a, **k):
        raise RuntimeError("database is locked")

    db_session.query = boom
    return lambda: setattr(db_session, "query", orig)


def run_node(scenario: dict) -> dict:
    """One node through the web's execute_node_chain, as its only node, with
    probe children on each handle the node can take."""
    _prepare(scenario)
    STATE["next"] = []
    graph = scenario["graph"]
    context = fx.WorkflowContext(workflow_id=WORKFLOW_ID)
    for k, v in copy.deepcopy(scenario.get("vars_in", {})).items():
        context.set_variable(k, v)
    for k, v in scenario.get("conditions_in", {}).items():
        context.set_condition_result(k, v)
    if (scenario.get("run_context") or {}).get("trigger") == "price_alert":
        context.trusted_price_alert = True
    logs: list = []
    client = fc.get_flow_client(API_KEY)
    executor = fx.NodeExecutor(client, context, logs, default_strategy=scenario.get("workflow_name", "Golden Flow"))
    captured = _capture_results(executor, None)
    edges = graph.get("edges", [])
    edge_map: dict = {}
    incoming: dict = {}
    for e in edges:
        edge_map.setdefault(e["source"], []).append(e)
        incoming.setdefault(e["target"], []).append(e)
    restore = _broken_db() if scenario.get("env", {}).get("master_contract") == "unreadable" else None
    abort = None
    try:
        fx.execute_node_chain(scenario["start"], graph["nodes"], edge_map, incoming, executor, context, {}, 0)
    except Exception as exc:  # the web's run-level handler
        abort = str(exc)
        logs.append({"time": "", "message": f"Error: {abort}", "level": "error"})
    finally:
        if restore:
            restore()
    outputs = None
    outputs_by = "node"
    for name, r in captured:
        outputs = r  # the last non-None handler result is the node's
        # The desktop's walk, not the node, builds these (core's walk.rs).
        walk_built = name in ("unresolved_error", "execute_and_gate", "execute_or_gate", "execute_not_gate")
        outputs_by = "walk" if walk_built else "node"
    return {
        "outputs": outputs,
        "outputs_by": outputs_by,
        "logs": [[e["level"], e["message"]] for e in logs],
        "calls": STATE["calls"],
        "script": STATE["answered"],
        "vars_out": context.variables,
        "conditions_out": dict(context.condition_results),
        "next": sorted(STATE["next"]),
        "error": {"errors": executor.errors, "abort": abort} if (executor.errors or abort) else None,
        "alerts": STATE["alerts"],
        "feed": [f for f in STATE["feed"] if f["op"] not in ("strategy_book",)],
        "subscriptions": sorted([list(e) for e in fx._workflow_subscriptions.get(WORKFLOW_ID, set())]),
        "http_requests": STATE["http"],
        "sleeps": STATE["sleeps"],
    }


class _Execution:
    id = 1


class _Workflow:
    def __init__(self, name, nodes, edges):
        self.id = WORKFLOW_ID
        self.name = name
        self.nodes = nodes
        self.edges = edges


def run_workflow(scenario: dict) -> dict:
    """A whole workflow through the web's _execute_workflow_locked."""
    _prepare(scenario)
    STATE["next"] = []
    wf = _Workflow(scenario["workflow"]["name"], copy.deepcopy(scenario["workflow"]["nodes"]), copy.deepcopy(scenario["workflow"]["edges"]))
    record = {}
    special = scenario.get("special")
    fx.get_workflow = lambda wid: None if special == "workflow_missing" else wf
    fx.create_execution = lambda wid, status="running": None if special == "no_execution_record" else _Execution()

    def update(eid, status, error=None, logs=None):
        record["status"] = status
        record["error"] = error

    fx.update_execution_status = update
    captured_ctx = {}
    orig_ctx = fx.WorkflowContext

    trusted = (scenario.get("run_context") or {}).get("trigger") == "price_alert"

    class Ctx(orig_ctx):
        def __init__(self, *a, **k):
            super().__init__(*a, **k)
            captured_ctx["ctx"] = self
            if trusted:
                self.trusted_price_alert = True

    fx.WorkflowContext = Ctx
    try:
        key = API_KEY if scenario.get("api_key", True) else None
        if special == "already_running":
            held = fx.get_workflow_lock(WORKFLOW_ID)
            held.acquire()
            try:
                result = fx.execute_workflow(WORKFLOW_ID, copy.deepcopy(scenario.get("webhook")), key)
            finally:
                held.release()
        else:
            result = fx.execute_workflow(WORKFLOW_ID, copy.deepcopy(scenario.get("webhook")), key)
    finally:
        fx.WorkflowContext = orig_ctx
    logs = result.pop("logs", [])
    result.pop("execution_id", None)
    ctx = captured_ctx.get("ctx")
    return {
        "result": result,
        "record": record or None,
        "logs": [[e["level"], e["message"]] for e in logs],
        "calls": STATE["calls"],
        "script": STATE["answered"],
        "vars_out": ctx.variables if ctx else {},
        "alerts": STATE["alerts"],
        "feed": [f for f in STATE["feed"] if f["op"] not in ("strategy_book",)],
        "subscriptions": sorted([list(e) for e in fx._workflow_subscriptions.get(WORKFLOW_ID, set())]),
        "http_requests": STATE["http"],
        "sleeps": STATE["sleeps"],
    }


# --- JSON ------------------------------------------------------------------------


def to_json(value):
    """JSON-safe copy: non-finite floats become {"$float": "nan"}, tuples and
    sets lists, numpy scalars Python numbers."""
    if isinstance(value, bool) or value is None or isinstance(value, (int, str)):
        return value
    if isinstance(value, float):
        if math.isnan(value):
            return {"$float": "nan"}
        if math.isinf(value):
            return {"$float": "inf" if value > 0 else "-inf"}
        return value
    if isinstance(value, dict):
        return {str(k): to_json(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [to_json(v) for v in value]
    if isinstance(value, (set, frozenset)):
        return sorted(to_json(v) for v in value)
    if hasattr(value, "item"):
        return to_json(value.item())
    if isinstance(value, (_dt.date, _dt.datetime)):
        return value.isoformat()
    raise TypeError(f"cannot write {type(value).__name__} to a golden")
