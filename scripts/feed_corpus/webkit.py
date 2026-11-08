"""Run a web broker adapter's own decoding and normalising code offline.

The web adapters are classes that open sockets, read the database and
publish on ZeroMQ when constructed. What turns a broker frame into the dict
the web forwards to 8765 clients is a handful of their methods. This module
lifts exactly those methods out of the web tree's source (by AST: nothing
of the web app is imported, so no database, network, broker or Flask
initialisation runs) into stand-in classes, gives them the state the
adapter would hold, and captures what they publish.

A broker's glue (`web_adapters/<broker>.py`) names its sources, builds its
instances the way the web's own tests do (`Class.__new__` plus attributes),
and routes the case's subscribes and pushes into them. See
`web_expected.py` for the command and the output.

Rules the extraction follows:

- A `Source` is a web file and a class in it (its class-level constants and
  the methods named, or every method when none are named), or `None` for
  module-level functions and constants by name.
- Every source of a glue is executed into one namespace, so classes refer
  to each other as in the web package.
- The file's own imports of standard-library modules are executed (with
  `time` frozen, below); every other import (database, utils, websocket,
  the proxy) is not. A name the methods need from those is a stub the glue
  passes in `stubs`.
- Annotations are not evaluated (`from __future__ import annotations`), and
  decorators other than staticmethod, classmethod and property are dropped.
- `time.time()` returns RECEIPT_S, a fixed instant no packet carries, so a
  receipt time in the output is recognised and written `"<receipt_ms>"`
  (milliseconds) or `"<receipt_s>"` (seconds), as the corpus writes it.
"""

from __future__ import annotations

import ast
import copy
import json
import threading
import types
from pathlib import Path
from typing import Any

# 2100-01-01T00:00:00Z: the frozen clock; no broker packet carries it.
RECEIPT_S = 4102444800.0
RECEIPT_MS = int(RECEIPT_S * 1000)

# Standard-library modules a web file may import and the lifted code may
# use. Anything else it imports is left out.
STDLIB = {
    "base64", "binascii", "calendar", "collections", "collections.abc", "copy",
    "dataclasses", "datetime", "decimal", "enum", "functools", "gzip", "hashlib",
    "io", "itertools", "json", "logging", "math", "operator", "os", "re",
    "struct", "sys", "threading", "time", "typing", "urllib.parse", "uuid",
    "zlib", "zoneinfo",
}

KEEP_DECORATORS = {"staticmethod", "classmethod", "property"}


class Source:
    """A web file and what to lift from it."""

    def __init__(self, path: str, cls: str | None, members: list[str] | None = None):
        self.path = path
        self.cls = cls
        self.members = members


class NullLogger:
    """Stands in for the adapter's logger: says nothing."""

    def isEnabledFor(self, _level):  # noqa: N802 (logging's name)
        return False

    def __getattr__(self, _name):
        return lambda *a, **k: None


def frozen_time() -> types.ModuleType:
    """`time` with its clock stopped at RECEIPT_S."""
    import time as real

    m = types.ModuleType("time")
    for name in dir(real):
        if not name.startswith("__"):
            setattr(m, name, getattr(real, name))
    m.time = lambda: RECEIPT_S
    m.time_ns = lambda: int(RECEIPT_S * 1_000_000_000)
    m.sleep = lambda *_: None
    return m


def _stdlib_imports(tree: ast.Module, ns: dict) -> None:
    """Execute the file's standard-library imports into `ns`."""
    import importlib

    for node in tree.body:
        if isinstance(node, ast.Import):
            for a in node.names:
                if a.name in STDLIB:
                    mod = frozen_time() if a.name == "time" else importlib.import_module(a.name)
                    ns[a.asname or a.name.split(".")[0]] = mod
        elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module in STDLIB:
            mod = frozen_time() if node.module == "time" else importlib.import_module(node.module)
            for a in node.names:
                if a.name != "*" and hasattr(mod, a.name):
                    ns[a.asname or a.name] = getattr(mod, a.name)


def _lift_class(node: ast.ClassDef, members: list[str] | None) -> ast.ClassDef:
    body = []
    for item in node.body:
        if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)):
            if members is None or item.name in members:
                item = copy.deepcopy(item)
                item.decorator_list = [
                    d for d in item.decorator_list
                    if isinstance(d, ast.Name) and d.id in KEEP_DECORATORS
                ]
                body.append(item)
        elif isinstance(item, (ast.Assign, ast.AnnAssign)):
            body.append(copy.deepcopy(item))
    missing = set(members or []) - {
        i.name for i in body if isinstance(i, (ast.FunctionDef, ast.AsyncFunctionDef))
    }
    if missing:
        raise SystemExit(f"{node.name} has no method {sorted(missing)}")
    return ast.ClassDef(
        name=node.name, bases=[], keywords=[], body=body or [ast.Pass()], decorator_list=[],
        type_params=[],
    )


def _lift_module(tree: ast.Module, names: list[str]) -> list[ast.stmt]:
    out = []
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)) and node.name in names:
            out.append(copy.deepcopy(node))
        elif isinstance(node, ast.Assign) and any(
            isinstance(t, ast.Name) and t.id in names for t in node.targets
        ):
            out.append(copy.deepcopy(node))
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name) and node.target.id in names:
            out.append(copy.deepcopy(node))
    found = {
        getattr(n, "name", None) or getattr(getattr(n, "target", None), "id", None)
        for n in out
    } | {t.id for n in out if isinstance(n, ast.Assign) for t in n.targets if isinstance(t, ast.Name)}
    if set(names) - found:
        raise SystemExit(f"not found at module level: {sorted(set(names) - found)}")
    return out


def load(web_root: Path, sources: list[Source], stubs: dict[str, Any] | None = None) -> dict:
    """Lift `sources` from the web tree into one namespace and return it."""
    ns: dict[str, Any] = {"__name__": "web_lifted", "logger": NullLogger()}
    trees = []
    for s in sources:
        path = web_root / s.path
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        _stdlib_imports(tree, ns)
        trees.append((s, path, tree))
    ns.update(stubs or {})
    for s, path, tree in trees:
        if s.cls is None:
            body = _lift_module(tree, s.members or [])
        else:
            node = next(
                (n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == s.cls), None
            )
            if node is None:
                raise SystemExit(f"{s.path}: no class {s.cls}")
            body = [_lift_class(node, s.members)]
        future = ast.ImportFrom(module="__future__", names=[ast.alias(name="annotations")], level=0)
        module = ast.fix_missing_locations(ast.Module(body=[future, *body], type_ignores=[]))
        exec(compile(module, str(path), "exec"), ns)  # noqa: S102 (the web's own code, read-only)
    return ns


def receipt(v: Any) -> Any:
    """`v` as JSON, with the frozen clock's values written as placeholders."""
    if isinstance(v, dict):
        out = {}
        for k, x in v.items():
            if not isinstance(k, str):
                raise SystemExit(f"a published key is not a string: {k!r}")
            out[k] = receipt(x)
        return out
    if isinstance(v, (list, tuple)):
        return [receipt(x) for x in v]
    if type(v) is int and v == RECEIPT_MS:
        return "<receipt_ms>"
    if (type(v) is int and v == int(RECEIPT_S)) or (type(v) is float and v == RECEIPT_S):
        return "<receipt_s>"
    if v is None or isinstance(v, (bool, int, float, str)):
        return v
    raise SystemExit(f"a published value is not JSON: {v!r}")


def topic_mode(topic: str) -> int:
    """The mode of a web market data topic `EXCHANGE_SYMBOL_MODE`."""
    return {"LTP": 1, "QUOTE": 2, "DEPTH": 3}[topic.rsplit("_", 1)[1].upper()]


# websocket_proxy/server.py's two-segment exchanges in a topic.
_TWO_SEGMENT_EXCHANGES = {("NSE", "INDEX"), ("BSE", "INDEX"), ("MCX", "INDEX"), ("GLOBAL", "INDEX")}


def topic_instrument(topic: str) -> tuple[str, str]:
    """A topic's exchange and symbol, split as websocket_proxy/server.py's
    ZeroMQ listener splits them (the frame a client gets carries these)."""
    remaining = topic.split("_")[:-1]
    if len(remaining) >= 2 and (remaining[0], remaining[1]) in _TWO_SEGMENT_EXCHANGES:
        return f"{remaining[0]}_{remaining[1]}", "_".join(remaining[2:])
    return remaining[0], "_".join(remaining[1:])


class Master:
    """The case's symbol master rows, with the defaults the harness gives."""

    def __init__(self, rows: list[dict]):
        self.rows = []
        for r in rows:
            full = {
                "brsymbol": r["symbol"], "name": r["symbol"], "brexchange": r["exchange"],
                "expiry": "", "strike": 0.0, "lotsize": 1, "instrumenttype": "EQ",
                "tick_size": 0.05,
            }
            full.update(r)
            self.rows.append(full)

    def by_symbol(self, symbol: str, exchange: str) -> dict | None:
        return next((r for r in self.rows if r["symbol"] == symbol and r["exchange"] == exchange), None)

    def by_token(self, token: str, exchange: str) -> dict | None:
        return next((r for r in self.rows if str(r["token"]) == str(token) and r["exchange"] == exchange), None)

    def by_brsymbol(self, brsymbol: str, exchange: str) -> dict | None:
        return next((r for r in self.rows if r["brsymbol"] == brsymbol and r["exchange"] == exchange), None)


def _order_event_defaults(web_root: Path) -> dict:
    """`events.OrderUpdateEvent`'s field defaults, read from the web."""
    path = web_root / "events/order_events.py"
    tree = ast.parse(path.read_text(encoding="utf-8"))
    out = {}
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name in ("OrderEvent", "OrderUpdateEvent"):
            for item in node.body:
                if isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name) and item.value is not None:
                    try:
                        out[item.target.id] = ast.literal_eval(item.value)
                    except ValueError:
                        pass
    return out


def _order_payload(web_root: Path) -> ast.Dict:
    """The `payload = {...}` dict `wsproxy_subscriber.on_order_update` relays."""
    path = web_root / "subscribers/wsproxy_subscriber.py"
    tree = ast.parse(path.read_text(encoding="utf-8"))
    fn = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "on_order_update")
    for node in ast.walk(fn):
        if (
            isinstance(node, ast.Assign) and isinstance(node.value, ast.Dict)
            and any(isinstance(t, ast.Name) and t.id == "payload" for t in node.targets)
        ):
            return node.value
    raise SystemExit("wsproxy_subscriber.on_order_update builds no payload dict")


class Glue:
    """One broker's web adapter, run offline. Subclasses set `broker` and
    `sources`, and implement `start`, `subscribe`, `unsubscribe` and `push`
    (and `reconnect`, `subscribe_orders` where the broker needs them)."""

    broker = ""
    sources: list[Source] = []

    def __init__(self, web_root: Path, case: dict):
        self.web_root = web_root
        self.case = case
        self.master = Master(case["instruments"])
        self.logger = NullLogger()
        self.published: list[dict] = []
        self.web = load(web_root, self.sources, self.stubs())
        self._order_defaults = _order_event_defaults(web_root)
        self._order_payload = _order_payload(web_root)

    # -- what a glue provides -------------------------------------------

    def stubs(self) -> dict[str, Any]:
        """Names the lifted methods use from imports that are not run."""
        return {}

    def start(self) -> None:
        raise NotImplementedError

    def subscribe(self, row: dict, mode: int, depth: int) -> None:
        raise NotImplementedError

    def unsubscribe(self, row: dict, mode: int, depth: int) -> None:
        raise NotImplementedError

    def subscribe_orders(self) -> None:
        pass

    def push(self, socket: str, frame: bytes | str) -> None:
        raise NotImplementedError

    def reconnect(self, socket: str) -> None:
        pass

    def serve(self, method: str | None, path: str, body: str) -> None:
        """The fake broker API answers `path` with `body` from now on: for
        a polling broker, the order book the web adapter's next poll reads
        (`PollingOrderUpdateAdapter`: the first read seeds silently)."""

    # -- what a glue uses -----------------------------------------------

    def to_openalgo_symbol(self, broker_symbol: str, exchange: str, token=None) -> str:
        """`websocket_proxy/order_adapter.py::to_openalgo_symbol` on the
        case's master in place of the database: the broker token first,
        then the broker symbol, else the raw symbol."""
        row = (token is not None and self.master.by_token(token, exchange)) or self.master.by_brsymbol(
            broker_symbol, exchange
        )
        return row["symbol"] if row else broker_symbol

    def new(self, cls: str, **attrs) -> Any:
        """An instance of lifted class `cls` without its `__init__`, with
        a quiet logger, a real lock and `attrs`."""
        obj = self.web[cls].__new__(self.web[cls])
        obj.logger = NullLogger()
        obj.lock = threading.Lock()
        for k, v in attrs.items():
            setattr(obj, k, v)
        return obj

    def publish(self, topic: str, data: dict) -> None:
        """The adapter's `publish_market_data`."""
        exchange, symbol = topic_instrument(topic)
        self.published.append({
            "kind": "market", "topic": topic, "exchange": exchange, "symbol": symbol,
            "mode": topic_mode(topic), "data": receipt(copy.deepcopy(data)),
        })

    def order(self, fields: dict) -> None:
        """An order update the adapter normalised, as the web relays it:
        `OrderUpdateEvent(**fields)` (mode and broker defaulted as
        `BaseOrderUpdateAdapter._publish_event_fields` does), then the
        payload `wsproxy_subscriber.on_order_update` builds, read from its
        dict literal."""
        event = dict(self._order_defaults)
        event.update({"mode": "live", "broker": self.broker})
        event.update(fields)
        frame = {}
        for k, v in zip(self._order_payload.keys, self._order_payload.values):
            if isinstance(v, ast.Constant):
                frame[k.value] = v.value
            elif isinstance(v, ast.Name) and v.id == "user_id":
                frame[k.value] = "<USER_ID>"
            elif isinstance(v, ast.Attribute) and isinstance(v.value, ast.Name) and v.value.id == "event":
                frame[k.value] = event.get(v.attr)
            else:
                raise SystemExit(f"wsproxy_subscriber payload key {k.value!r}: not read")
        self.published.append({"kind": "order", "frame": receipt(frame)})


def read_frame(case_dir: Path, spec: Any) -> tuple[bytes | str, bytes | None]:
    """A pushed frame as the harness reads it, and the file bytes that join
    the case's inputs (None for an inline frame)."""
    if isinstance(spec, str):
        raw = (case_dir / spec).read_bytes()
        ext = Path(spec).suffix
        if ext == ".bin":
            return raw, raw
        # Text files join the inputs with LF line ends, as the harness reads them.
        raw = raw.replace(b"\r\n", b"\n")
        text = raw.decode("utf-8").rstrip("\r\n")
        if ext == ".hex":
            return hex_bytes(text), raw
        if ext in (".json", ".txt"):
            return text, raw
        raise SystemExit(f"{spec}: a frame file is .hex, .bin, .json or .txt")
    ((kind, v),) = spec.items()
    if kind == "hex":
        return hex_bytes(v), None
    if kind == "text":
        return v, None
    return json.dumps(v, separators=(",", ":")), None


def read_body(case_dir: Path, spec: Any) -> tuple[str, bytes | None]:
    """An HTTP answer's body as the harness serves it (a file of the case,
    `{"json"}`, `{"text"}`, or empty), and the file bytes that join the
    case's inputs."""
    if spec is None:
        return "", None
    if isinstance(spec, str):
        raw = (case_dir / spec).read_bytes().replace(b"\r\n", b"\n")
        return raw.decode("utf-8").rstrip("\r\n"), raw
    ((kind, v),) = spec.items()
    return (v if kind == "text" else json.dumps(v, separators=(",", ":"))), None


def hex_bytes(text: str) -> bytes:
    digits = "".join(line.split("#", 1)[0] for line in text.splitlines())
    return bytes.fromhex("".join(digits.split()))
