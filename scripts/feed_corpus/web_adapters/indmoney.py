"""INDmoney (INDstocks): the price socket's message handler, the adapter's
subscribe/unsubscribe and normaliser, and the order-update adapter's
normaliser with its canonical order id lookup.

`SymbolMapper.get_token_from_symbol` reads the database; the glue's
stand-in reads the case's symbol master. The adapter's own `subscribe`
runs (so its refusal of mode 3 is the web's), with the socket left
unconnected so nothing is batched or sent.

`_canonical_order_id` fetches the REST order book on a cache miss
(`broker.indmoney.api.order_api.get_order_book`, imported inside the
function). Offline, that import is served the book in `ORDER_BOOK` below:
the orders a trader's INDmoney account would list, the one placed outside
the desktop (`96057999`, case `order_unknown_id`) among them.
"""

import contextlib
import sys
import types

from webkit import Glue, Source

# The REST order book `_canonical_order_id` reads on a miss.
ORDER_BOOK = [
    {"id": "EQ-96057848"},
    {"id": "EQ-96057849"},
    {"id": "EQ-96057850"},
    {"id": "EQ-96057999"},
]


class TTLCache(dict):
    """cachetools.TTLCache as the lifted module builds it (nothing expires
    within a case)."""

    def __init__(self, maxsize=None, ttl=None):
        super().__init__()


@contextlib.contextmanager
def rest_order_book():
    """The modules `_canonical_order_id` imports inside itself, serving
    ORDER_BOOK, for the length of one normalise."""
    fake = {
        "broker": types.ModuleType("broker"),
        "broker.indmoney": types.ModuleType("broker.indmoney"),
        "broker.indmoney.api": types.ModuleType("broker.indmoney.api"),
        "broker.indmoney.api.order_api": types.ModuleType("broker.indmoney.api.order_api"),
        "database": types.ModuleType("database"),
        "database.auth_db": types.ModuleType("database.auth_db"),
    }
    fake["broker.indmoney.api.order_api"].get_order_book = lambda _auth: [dict(o) for o in ORDER_BOOK]
    fake["database.auth_db"].get_auth_token = lambda _user_id, **_kw: "t"
    saved = {k: sys.modules.get(k) for k in fake}
    sys.modules.update(fake)
    try:
        yield
    finally:
        for k, v in saved.items():
            if v is None:
                sys.modules.pop(k, None)
            else:
                sys.modules[k] = v


class Indmoney(Glue):
    broker = "indmoney"
    sources = [
        Source("broker/indmoney/streaming/indmoney_mapping.py", "IndmoneyExchangeMapper"),
        Source("broker/indmoney/streaming/indmoney_mapping.py", "IndmoneyModeMapper"),
        Source(
            "websocket_proxy/base_adapter.py",
            "BaseBrokerWebSocketAdapter",
            ["_create_success_response", "_create_error_response"],
        ),
        Source("broker/indmoney/streaming/indWebSocket.py", "IndWebSocket", ["_on_message", "_on_pong"]),
        Source(
            "broker/indmoney/streaming/indmoney_adapter.py",
            "IndmoneyWebSocketAdapter",
            ["subscribe", "unsubscribe", "_on_data", "_normalize_market_data"],
        ),
        Source(
            "broker/indmoney/mapping/order_data.py",
            None,
            [
                "COMPLETED_STATUSES",
                "OPEN_STATUSES",
                "TRIGGER_PENDING_STATUSES",
                "REJECTED_STATUSES",
                "CANCELLED_STATUSES",
                "normalize_order_status",
            ],
        ),
        Source(
            "broker/indmoney/streaming/indmoney_order_adapter.py",
            None,
            ["_ID_CACHE", "_ID_CACHE_LOCK", "_ID_REFRESH_MIN_INTERVAL", "_id_refresh_attempts", "_canonical_order_id"],
        ),
        Source(
            "broker/indmoney/streaming/indmoney_order_adapter.py",
            "IndmoneyOrderUpdateAdapter",
            ["_field", "_as_int", "_map_status", "_decode", "normalize"],
        ),
    ]

    def stubs(self):
        master = self.master

        class SymbolMapper:
            @staticmethod
            def get_token_from_symbol(symbol, exchange):
                row = master.by_symbol(symbol, exchange)
                return {"token": str(row["token"]), "brexchange": row["brexchange"]} if row else None

        return {"SymbolMapper": SymbolMapper, "TTLCache": TTLCache}

    def start(self):
        base = self.web["BaseBrokerWebSocketAdapter"]
        self.ad = self.new(
            "IndmoneyWebSocketAdapter",
            subscriptions={},
            last_values={},
            subscription_queue=[],
            connected=False,
            ws_client=None,
            publish_market_data=self.publish,
            disconnect=lambda: None,
        )
        self.ad._create_success_response = types.MethodType(base._create_success_response, self.ad)
        self.ad._create_error_response = types.MethodType(base._create_error_response, self.ad)
        self.ws = self.new(
            "IndWebSocket",
            on_data=self.ad._on_data,
            on_message=lambda *_: None,
            last_pong_timestamp=None,
        )
        self.orders = self.new("IndmoneyOrderUpdateAdapter", user_id="<USER_ID>")

    def subscribe(self, row, mode, depth):
        # The web server ignores a refused item (its ack says why); the
        # adapter stores nothing for it.
        self.ad.subscribe(row["symbol"], row["exchange"], mode, depth)

    def unsubscribe(self, row, mode, depth):
        self.ad.unsubscribe(row["symbol"], row["exchange"], mode)

    def reconnect(self, socket):
        # `_on_open` resubscribes and keeps `last_values`.
        pass

    def push(self, socket, frame):
        if isinstance(frame, bytes):
            frame = frame.decode("utf-8")
        if socket == "orders":
            with rest_order_book():
                fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
        else:
            self.ws._on_message(None, frame)
