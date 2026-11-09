"""The web adapter's own `subscribe`, run for its reply alone (review 2, O4).

A broker's glue (`web_adapters/`) sets the state the web adapter's decoders
read; only some glues go through the adapter's `subscribe`. What the web tells
a client about a subscribe comes from that method, so for the others this
module runs it on a second, otherwise empty instance of the adapter: its
broker socket, its database and its timers are stand-ins (`Anything`), its
symbol lookups read the case's instruments, and its depth checks are the
broker's own capability classes.

Nothing it does reaches the instance the glue feeds frames to. A method that
cannot run this way gives no reply (`None`), and that send is recorded as
having no web reply rather than as a success. Standard library only.
"""

from __future__ import annotations

import ast
import builtins
import re
import threading
import types
from pathlib import Path
from typing import Any

from webkit import STDLIB, NullLogger, _stdlib_imports

MAX_STUBS = 60
# What a stand-in's failure looks like in a reply an adapter built from it.
ARTIFACTS = ("Anything", "_Imported", "object has no attribute", "not supported between", "object is not", "NoneType")


class Anything(dict):
    """Stands in for a socket, a timer, a queue, a table: true, callable, any
    attribute is another one, and what is stored in it stays for the run."""

    def __init__(self, *_args, **_kwargs):
        super().__init__()

    def __call__(self, *_args, **_kwargs):
        return Anything()

    def __getattr__(self, _name):
        return Anything()

    def __missing__(self, _key):
        return Anything()

    # A socket's own `clear(...)` or `pop(...)`, whatever it is given.
    def clear(self, *_args, **_kwargs):
        super().clear()

    def pop(self, key=None, *default, **_kwargs):
        return super().pop(key, default[0] if default else Anything())

    def __delitem__(self, key):
        super().pop(key, None)

    def __bool__(self):
        return True

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def __hash__(self):
        return id(self)

    def __or__(self, _other):
        return Anything()

    __ror__ = __or__
    __and__ = __or__
    __rand__ = __or__
    __add__ = __or__
    __radd__ = __or__
    __sub__ = __or__
    __rsub__ = __or__

    def __lt__(self, _other):
        return False

    __le__ = __lt__
    __gt__ = __lt__
    __ge__ = __lt__


class _NoThreads:
    """The adapter's `threading`: real locks, and no thread or timer ever
    starts, so a batch timer of a reply-only run sends nothing anywhere."""

    Lock = threading.Lock
    RLock = threading.RLock
    Event = threading.Event
    Thread = Anything
    Timer = Anything


class _AnyName(type):
    """A stand-in named by a module's import: `pb.MODE_LTP`, `Base.method`."""

    def __getattr__(cls, _name):
        return Anything()


class _Imported(metaclass=_AnyName):
    def __init__(self, *_args, **_kwargs):
        pass

    def __getattr__(self, _name):
        return Anything()

    def __call__(self, *_args, **_kwargs):
        return Anything()


def _exec_module(path: Path, ns: dict) -> None:
    """A web module's classes, functions and constants, its non-standard
    imports left out; a name they need at load time is a stand-in."""
    tree = ast.parse(path.read_text(encoding="utf-8"))
    _stdlib_imports(tree, ns)
    kept = (ast.ClassDef, ast.FunctionDef, ast.Assign, ast.AnnAssign)
    module = compile(ast.Module(body=[n for n in tree.body if isinstance(n, kept)], type_ignores=[]), str(path), "exec")
    for _ in range(MAX_STUBS):
        try:
            exec(module, ns)  # noqa: S102 (the web's own code, read-only)
            return
        except NameError as e:
            ns[e.name] = _Imported
        except Exception:  # noqa: BLE001 (a helper module this cannot load; the adapter's own is checked after)
            return


class _Module(Anything):
    """What an import inside an adapter's method gets (`from database.token_db
    import get_token`): the case's lookups by name, a stand-in for the rest."""


def _load_order(path: Path) -> int:
    """The mappings first (the adapter's class body reads them), and the
    adapter's module last, so a name two modules import differently is the
    adapter's (`time`, the module, against `datetime.time`)."""
    return 0 if "mapping" in path.name else 2 if "adapter" in path.name else 1


class ShadowAdapter:
    """The broker's web adapter's `subscribe`, on an instance of its own."""

    def __init__(self, web_root: Path, broker: str, master):
        self.broker = broker
        self.master = master
        self.last_error: str | None = None
        self.ns: dict = {"__name__": "web_shadow", "__builtins__": self._builtins()}
        # The base classes are real: the replies an adapter builds come from them.
        for shared in ("mapping.py", "base_adapter.py"):
            _exec_module(web_root / "websocket_proxy" / shared, self.ns)
        self.cls = None
        streaming = web_root / "broker" / broker / "streaming"
        files = [p for p in streaming.glob("*.py") if "order" not in p.name]
        for path in sorted(files, key=lambda p: (_load_order(p), p.name)):
            _exec_module(path, self.ns)
            for node in ast.parse(path.read_text(encoding="utf-8")).body:
                value = self.ns.get(node.name) if isinstance(node, ast.ClassDef) else None
                if isinstance(value, type) and "Adapter" in value.__name__ and "subscribe" in value.__dict__:
                    self.cls = value
        self.ns.update(self._lookups())
        self.ns["threading"] = _NoThreads
        self.instance = self._instance() if self.cls is not None else None

    def _builtins(self) -> dict:
        """The builtins, with `__import__` keeping an adapter's own imports
        of the web's packages, made inside its methods, off the real ones."""

        def imported(name, globals_=None, locals_=None, fromlist=(), level=0):
            if not level and name.split(".")[0] in STDLIB:
                return builtins.__import__(name, globals_, locals_, fromlist, level)
            module = _Module()
            module.__dict__.update(self._lookups())
            return module

        return {**vars(builtins), "__import__": imported}

    def _info(self, symbol, exchange):
        row = self.master.by_symbol(symbol, exchange)
        if row is None:
            return None
        return {"token": row["token"], "brexchange": row.get("brexchange", exchange), "brsymbol": row.get("brsymbol")}

    def _lookups(self) -> dict[str, Any]:
        """The database lookups an adapter's subscribe makes, on the case's master."""
        info = self._info

        class SymbolMapper:
            @staticmethod
            def get_token_from_symbol(symbol, exchange):
                return info(symbol, exchange)

        def get_token(symbol, exchange):
            found = info(symbol, exchange)
            return found["token"] if found else None

        def get_br_symbol(symbol, exchange):
            found = info(symbol, exchange)
            return found["brsymbol"] if found else None

        def get_symbol_info(symbol, exchange):
            found = info(symbol, exchange)
            return types.SimpleNamespace(**found) if found else None

        return {
            "SymbolMapper": SymbolMapper,
            "get_token": get_token,
            "get_br_symbol": get_br_symbol,
            "get_symbol_info": get_symbol_info,
        }

    def _instance(self):
        def stand_in(obj, name):
            value = Anything()
            obj.__dict__[name] = value
            return value

        cls = type("Shadow", (self.cls,), {"__getattr__": stand_in})
        obj = cls.__new__(cls)
        obj.logger = NullLogger()
        obj.lock = threading.Lock()
        obj.broker_name = self.broker
        return obj

    def subscribe(self, symbol: str, exchange: str, mode: int, depth: int) -> dict | None:
        """The reply of the web adapter's `subscribe`, or `None` if it cannot run here."""
        return self._reply("subscribe", symbol, exchange, mode, depth)

    def unsubscribe(self, symbol: str, exchange: str, mode: int) -> dict | None:
        """The same for its `unsubscribe`, on the state its subscribes left."""
        return self._reply("unsubscribe", symbol, exchange, mode)

    def _reply(self, method: str, *args) -> dict | None:
        if self.instance is None:
            return None
        for _ in range(MAX_STUBS):
            try:
                reply = getattr(self.instance, method)(*args)
            except NameError as e:
                self.ns[e.name] = _Imported
                continue
            except Exception as e:  # noqa: BLE001 (a method this cannot run gives no reply)
                self.last_error = repr(e)
                return None
            if not isinstance(reply, dict):
                self.last_error = f"not a reply: {type(reply).__name__}"
                return None
            # An adapter that catches its own errors turns a stand-in's
            # failure into a reply: that is not the web's answer.
            text = str(reply.get("message", ""))
            missing = re.search(r"name '(\w+)' is not defined", text)
            if missing:
                self.ns[missing.group(1)] = _Imported
                continue
            if any(sign in text for sign in ARTIFACTS):
                self.last_error = text
                return None
            return reply
        return None
