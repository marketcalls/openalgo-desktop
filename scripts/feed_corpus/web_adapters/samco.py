"""Samco: the adapter's subscribe, unsubscribe and tick fan-out, and the
socket client's frame handling (`_on_message`: unwrap, merge per symbol,
normalise), with the state test/test_samco_quote_feed.py builds; and the
order updates `PollingOrderUpdateAdapter` makes of each order book.

The adapter's own `subscribe` and `unsubscribe` run, down to the client's
subscription book and the frames it would send (a recording socket), so
the web's subscription quirks show as they are. Lookups the web makes
against the database and Samco's REST API are stand-ins: `SymbolMapper`
and `get_oa_symbol` read the case's symbol master, and the index streaming
ids `_get_index_listing_id` fetches from `/quote/indexQuote` are the ones
in LISTING_IDS. A dropped socket runs the client's
`_cleanup_connection_state` (the merged fields go with the connection).

Order updates: `PollingOrderUpdateAdapter._poll_loop` itself runs, on a
thread of its own, through `services/orderbook_service.py`
(`get_orderbook`, `get_orderbook_with_auth`, `format_order_data`) and
Samco's `broker/samco/mapping/order_data.py` (`map_order_data`,
`calculate_order_statistics`, `transform_order_data`) to its
`_publish_order`. Stand-ins only where the web reaches outside: the
session token, analyzer mode (off), the broker module import (handing the
lifted Samco functions), and `get_order_book`, which answers each poll with
the next order book the case serves. The broker module serves an empty
book first and the script waits for its read (`http_polled`), so the
loop's first, silent read is that empty book.
"""

import json
import queue
import sys
import threading
import types

from webkit import Glue, Source

# What Samco's /quote/indexQuote answers for each index (listingId).
LISTING_IDS = {"NIFTY_NSE_INDEX": "-23", "BANKNIFTY_NSE_INDEX": "-25"}
ORDER_BOOK = "/order/orderBook"
EMPTY_BOOK = '{"status": "Success", "orderBookDetails": []}'


class Samco(Glue):
    broker = "samco"
    sources = [
        Source("broker/samco/streaming/samco_mapping.py", "SamcoExchangeMapper"),
        Source("broker/samco/streaming/samco_mapping.py", "SamcoCapabilityRegistry"),
        Source(
            "broker/samco/streaming/samcoWebSocket.py",
            "SamcoWebSocket",
            [
                "subscribe",
                "unsubscribe",
                "_unsubscribe_locked",
                "_validate_connection_state",
                "_request_subscription_flush",
                "_on_message",
                "_update_last_message_time",
                "_unwrap_tick",
                "_normalize_market_data",
                "_first_present",
                "_parse_trade_time",
                "_safe_float",
                "_safe_int",
                "_cleanup_connection_state",
                "_stop_heartbeat",
                "_stop_subscription_flusher",
            ],
        ),
        Source(
            "broker/samco/streaming/samco_adapter.py",
            "SamcoWebSocketAdapter",
            ["subscribe", "unsubscribe", "_get_index_listing_id", "_on_data", "_normalize_market_data"],
        ),
        Source(
            "broker/samco/mapping/order_data.py",
            None,
            ["_PLACEHOLDERS", "_clean_text", "_to_int", "_to_float", "map_order_status", "map_order_data",
             "calculate_order_statistics", "transform_order_data"],
        ),
        Source(
            "services/orderbook_service.py",
            None,
            ["format_decimal", "format_order_data", "format_statistics", "get_orderbook_with_auth",
             "get_orderbook"],
        ),
        Source(
            "websocket_proxy/order_adapter.py",
            "PollingOrderUpdateAdapter",
            ["_is_current", "_poll_loop", "_publish_order"],
        ),
    ]

    def stubs(self):
        master = self.master
        self.events = []
        self.books = queue.Queue()
        self.polls = queue.Queue()

        class SymbolMapper:
            @staticmethod
            def get_token_from_symbol(symbol, exchange):
                row = master.by_symbol(symbol, exchange)
                return {"token": row["token"], "brexchange": row["brexchange"]} if row else None

        def get_oa_symbol(brsymbol, exchange):
            row = master.by_brsymbol(brsymbol, exchange)
            return row["symbol"] if row else None

        def get_order_book(auth):
            """broker/samco/api/order_api.py get_order_book: Samco's reply,
            the order book the case serves next. Entering it means the
            previous poll is done."""
            self.polls.put(None)
            return json.loads(self.books.get())

        def import_broker_module(broker_name):
            return {
                "get_order_book": get_order_book,
                "map_order_data": self.web["map_order_data"],
                "calculate_order_statistics": self.web["calculate_order_statistics"],
                "transform_order_data": self.web["transform_order_data"],
            }

        class BrokerBusyError(Exception):
            pass

        return {
            "SymbolMapper": SymbolMapper,
            "get_oa_symbol": get_oa_symbol,
            "import_broker_module": import_broker_module,
            "BrokerBusyError": BrokerBusyError,
            "broker_busy_result": lambda e, what: (False, {"status": "error", "message": what}, 503),
            "bus": types.SimpleNamespace(publish=self.events.append),
        }

    def start(self):
        self.sent = []
        self.ws = self.new(
            "SamcoWebSocket",
            ws=type("Socket", (), {"send": lambda _, frame: self.sent.append(frame)})(),
            connected=True,
            running=True,
            subscribed_symbols={},
            input_request_dict={},
            RESUBSCRIBE_FLAG=False,
            _tick_state={},
            _tick_state_lock=threading.Lock(),
            _sub_dirty=threading.Event(),
            _sub_send_lock=threading.Lock(),
            _sub_flush_stop=threading.Event(),
            _heartbeat_stop_event=threading.Event(),
            _heartbeat_lock=threading.Lock(),
            _last_message_time=None,
            _on_message_callback=None,
        )
        self.ad = self.new(
            "SamcoWebSocketAdapter",
            ws_client=self.ws,
            connected=True,
            subscriptions={},
            _index_listing_cache=dict(LISTING_IDS),
            _create_success_response=lambda message, **kw: {"status": "success", "message": message, **kw},
            _create_error_response=lambda code, message: {"status": "error", "code": code, "message": message},
            publish_market_data=self.publish,
        )
        self.ws._on_data_callback = self.ad._on_data
        self.poller = None

    def subscribe(self, row, mode, depth):
        reply = self.ad.subscribe(row["symbol"], row["exchange"], mode, depth)
        if reply["status"] != "success":
            raise SystemExit(f"the web refuses {row['symbol']}: {reply}")

    def unsubscribe(self, row, mode, depth):
        self.ad.unsubscribe(row["symbol"], row["exchange"], mode)

    def reconnect(self, socket):
        self.ws._cleanup_connection_state()
        self.ws.connected = True  # SamcoWebSocket._on_open on the new socket

    def push(self, socket, frame):
        self.ws._on_message(None, frame.decode("utf-8") if isinstance(frame, bytes) else frame)

    def subscribe_orders(self):
        """The web's order poller runs from the broker login on; here it
        starts with the order subscription and reads the empty book."""
        self.poller = self.new(
            "PollingOrderUpdateAdapter",
            broker_name="samco",
            user_id="<USER_ID>",
            poll_interval=1,
            _running=True,
            _generation=1,
            _lock=threading.Lock(),
        )
        _stub_modules({
            "database": {},
            "database.auth_db": {"get_auth_token": lambda user_id: "<session>"},
            "database.settings_db": {"get_analyze_mode": lambda: False},
            "services": {},
            "services.orderbook_service": {"get_orderbook": self.web["get_orderbook"]},
            "events": {"OrderUpdateEvent": lambda **fields: fields},
        })
        threading.Thread(target=self.poller._poll_loop, args=(1,), daemon=True).start()
        self.polls.get(timeout=10)  # its first read is waiting
        self.books.put(EMPTY_BOOK)
        self.polls.get(timeout=10)  # the baseline is read, the next read waits

    def serve(self, method, path, body):
        if path != ORDER_BOOK:
            raise SystemExit(f"the samco glue serves no {path}")
        if self.poller is None:
            raise SystemExit("an order book served before the order subscription")
        before = len(self.events)
        self.books.put(body)
        self.polls.get(timeout=10)  # this poll is done, the next read waits
        for fields in self.events[before:]:
            self.order(fields)


def _stub_modules(modules):
    """What the lifted functions import inside their bodies, as stand-ins."""
    for name, attrs in modules.items():
        m = types.ModuleType(name)
        for k, v in attrs.items():
            setattr(m, k, v)
        sys.modules[name] = m
