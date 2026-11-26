"""Flattrade: `flattrade_adapter.py` (its own normalisers: no
`last_trade_time` in LTP, Depth's `total_traded_value` from `toi`) and,
since web 73599f37c (marketcalls/openalgo#2507, issue #2386), its order
updates as the web carries them over the market data socket.

PiConnect allows one session per uid (issue #1806), so the market data
adapter subscribes `t=o` on its own socket (`_subscribe_order_updates`,
from `_on_open`) and relays every `om` frame verbatim (`_on_message`,
`_relay_publish`) over `flattrade_order_relay.py` to the order adapter
(`FlattradeEventOrderAdapter._on_relay_message`, `_publish_frame`,
`normalize_order_frame`). Each relay message runs through the relay's own
`OrderRelayPublisher.publish` and `OrderRelayListener.dispatch`, the ZeroMQ
socket between them a direct call. The desktop is one process and reads the
same frames off the same socket.

After each new stream (the relay's "ready", sent as the socket opens) the
order adapter reads the order book once and publishes what changed since the
last known state, the first read only recording it (`_catch_up`,
`_publish_book_order`, `_remember`), through `services/orderbook_service.py`
(`get_orderbook`) and Flattrade's `mapping/order_data.py`. The web runs it on
its supervisor's next tick, a second at most after the socket opened, by
which time the login has been answered: the replay runs it after the next
pushed frame, the login answer in every case. The order book it reads is the
one the case serves (`http`, `http_serve`), an empty book until then.

The adapter's own socket, opened only while no market data adapter holds the
session, the relay's ZeroMQ transport, and the daily rollover check (a
database read of the login time) are not replayed: the desktop has no second
socket and no second process, and a corpus session never reaches its
boundary. The market data cache survives a reconnect, as in
`_resubscribe_all`.
"""

import json
import sys
import threading
import types

from web_adapters.noren_family import ORDER_TABLES, NorenGlue, normalisers, sources
from webkit import Source

ORDER_FILE = "broker/flattrade/streaming/flattrade_order_adapter.py"
RELAY_FILE = "broker/flattrade/streaming/flattrade_order_relay.py"
RELAY_KINDS = ["KIND_CLAIM", "KIND_READY", "KIND_ALIVE", "KIND_RELEASE", "KIND_ORDER"]
# The empty book PiConnect answers on a flat account.
EMPTY_BOOK = '{"stat": "Not_Ok", "emsg": "no data"}'
ORDER_BOOK_PATH = "/PiConnectAPI/OrderBook"


class _Again(Exception):
    """zmq.Again: the relay's send queue was full."""


class Flattrade(NorenGlue):
    broker = "flattrade"
    adapter_class = "FlattradeWebSocketAdapter"
    cache_state = {"_cache": {}, "_initialized_scrips": set()}
    sources = [
        # The market data adapter; its order adapter's `normalize` is the
        # standalone socket's and is not lifted.
        *sources(
            "flattrade",
            "FlattradeWebSocketAdapter",
            [
                "_on_message",
                "_process_market_message",
                "_resolve_scrip",
                "_get_symbol_info",
                "_find_matching_subscriptions",
                "_should_process_message",
                "_process_subscription_message",
                "_normalize_market_data",
                "_create_subscription",
                "_index_subscription",
                "_deindex_scrip",
                "_relay_publish",
                "_subscribe_order_updates",
            ],
            ["LTPNormalizer", "QuoteNormalizer", "DepthNormalizer"],
            "FlattradeExchangeMapper",
            "FlattradeOrderUpdateAdapter",
        )[:-2],
        Source(RELAY_FILE, None, RELAY_KINDS),
        Source(RELAY_FILE, "OrderRelayPublisher", ["publish"]),
        Source(RELAY_FILE, "OrderRelayListener", ["dispatch"]),
        Source(ORDER_FILE, None, [*ORDER_TABLES, "normalize_order_frame", "_comparable_status"]),
        Source(
            ORDER_FILE,
            "FlattradeEventOrderAdapter",
            ["_on_relay_message", "_publish_frame", "_remember", "_catch_up", "_publish_book_order"],
        ),
        Source(
            "services/orderbook_service.py",
            None,
            ["format_decimal", "format_order_data", "format_statistics", "get_orderbook_with_auth", "get_orderbook"],
        ),
        Source(
            "broker/flattrade/mapping/order_data.py",
            None,
            [
                "_log_unavailable_book",
                "_COMPLETE_STATUSES",
                "_REJECTED_STATUSES",
                "_CANCELLED_STATUSES",
                "_OPEN_STATUSES",
                "normalize_order_status",
                "map_order_data",
                "calculate_order_statistics",
                "_int_or_zero",
                "_float_or_zero",
                "transform_order_data",
            ],
        ),
    ]

    def stubs(self):
        self.book = EMPTY_BOOK

        def import_broker_module(broker_name):
            return {
                "get_order_book": lambda auth: json.loads(self.book),
                "map_order_data": self.web["map_order_data"],
                "calculate_order_statistics": self.web["calculate_order_statistics"],
                "transform_order_data": self.web["transform_order_data"],
            }

        class BrokerBusyError(Exception):
            pass

        return {
            **super().stubs(),
            # The case's symbol master in place of the database.
            "get_symbol": lambda token, exchange: (self.master.by_token(token, exchange) or {}).get("symbol"),
            "get_auth_token": lambda user_id, **kw: "<session>",
            "import_broker_module": import_broker_module,
            "BrokerBusyError": BrokerBusyError,
            "broker_busy_result": lambda e, what: (False, {"status": "error", "message": what}, 503),
            "zmq": types.SimpleNamespace(NOBLOCK=1, Again=_Again),
        }

    def adapter_state(self):
        return {
            "normalizers": normalisers(self.web),
            "actid": "U1",
            # The socket's send of the order subscribe succeeds.
            "ws_client": types.SimpleNamespace(_send_message=lambda message, operation: True),
        }

    def order_adapter(self):
        """The order adapter as `create_flattrade_order_adapter` builds it
        for the user, running, its relay handler added; what it publishes
        goes out as the web relays an order update."""
        return self.new(
            "FlattradeEventOrderAdapter",
            user_id="<USER_ID>",
            flattrade_uid="U1",
            _lock=threading.Lock(),
            _running=True,
            _owner_seen_at=None,
            _relay_mode_logged=False,
            _socket=None,
            _known=None,
            _catch_up_pending=False,
            _publisher=types.SimpleNamespace(_publish_event_fields=self.order),
        )

    def start(self):
        super().start()
        web = self.web
        web["relay_mod"] = types.SimpleNamespace(**{k: web[k] for k in RELAY_KINDS})
        _stub_modules({
            "database": {},
            "database.settings_db": {"get_analyze_mode": lambda: False},
            "services": {},
            "services.orderbook_service": {"get_orderbook": web["get_orderbook"]},
        })
        listener = self.new("OrderRelayListener", _lock=threading.Lock(), _handlers={1: self.orders._on_relay_message})
        socket = types.SimpleNamespace(send_string=lambda raw, flags=0: listener.dispatch(raw))
        self.ad._order_relay = self.new(
            "OrderRelayPublisher",
            user_id="<USER_ID>",
            actid="U1",
            _lock=threading.Lock(),
            _socket=socket,
            _fresh=False,
        )
        # The first connect's `_on_open`: order updates subscribed, "ready".
        self.ad._subscribe_order_updates()

    def push(self, socket, frame):
        self.ad._on_message(None, frame)
        # The order adapter's supervisor tick after a new stream.
        if self.orders._catch_up_pending:
            self.orders._catch_up()

    def reconnect(self, socket):
        # `_attempt_reconnection`: the claim, then the new socket's
        # `_on_open` subscribes order updates again and says "ready".
        self.ad._relay_publish(self.web["KIND_CLAIM"])
        self.ad._subscribe_order_updates()

    def serve(self, method, path, body):
        if path != ORDER_BOOK_PATH:
            raise SystemExit(f"the flattrade glue serves no {path}")
        self.book = body


def _stub_modules(modules):
    """What the lifted functions import inside their bodies, as stand-ins."""
    for name, attrs in modules.items():
        m = types.ModuleType(name)
        for k, v in attrs.items():
            setattr(m, k, v)
        sys.modules[name] = m
