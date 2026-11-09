"""Order updates from polling the order book, for the brokers the web has
no order stream for (`services/order_update_service.py` _POLLING_BROKERS:
5paisa, Samco, Groww): the web's own `PollingOrderUpdateAdapter._poll_loop`
runs, on a thread of its own, through `services/orderbook_service.py`
(`get_orderbook`, `get_orderbook_with_auth`, `format_order_data`,
`format_statistics`) and the broker's `broker/<name>/mapping/order_data.py`
(`map_order_data`, `calculate_order_statistics`, `transform_order_data`,
lifted by the broker's glue) to its `_publish_order`.

Stand-ins only where the web leaves the process: the session token
(`get_auth_token`), analyzer mode (off), the broker module import
(`import_broker_module`, handing the lifted mapping functions), and the
broker's `get_order_book`, which answers each poll with the next order book
the case serves. The broker module serves an empty book first and the
script waits for its read (`http_polled`), so the loop's first, silent read
is that empty book.

A broker's glue: `class Broker(Polling, Glue)`, its sources plus
POLLING_SOURCES and its mapping functions, `order_book_path` and
`empty_book` set, and `polling_stubs()` merged into its `stubs()`.
"""

import json
import queue
import sys
import threading
import types

from webkit import Source

POLLING_SOURCES = [
    Source(
        "services/orderbook_service.py",
        None,
        ["format_decimal", "format_order_data", "format_statistics", "get_orderbook_with_auth", "get_orderbook"],
    ),
    Source("websocket_proxy/order_adapter.py", "PollingOrderUpdateAdapter", ["_is_current", "_poll_loop", "_publish_order"]),
]


class Polling:
    """The poller half of a glue (see the module docs)."""

    # The REST path of the broker's order book, and the book the broker
    # module serves before the script serves any (JSON text).
    order_book_path = ""
    empty_book = ""

    def polling_stubs(self) -> dict:
        self.events = []
        self.books = queue.Queue()
        self.polls = queue.Queue()
        self.poller = None

        def get_order_book(auth):
            """The broker's reply: the order book the case serves next.
            Entering it means the previous poll is done."""
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
            "import_broker_module": import_broker_module,
            "BrokerBusyError": BrokerBusyError,
            "broker_busy_result": lambda e, what: (False, {"status": "error", "message": what}, 503),
            "bus": types.SimpleNamespace(publish=self.events.append),
        }

    def subscribe_orders(self):
        """The web's order poller runs from the broker login on; here it
        starts with the order subscription and reads the empty book."""
        self.poller = self.new(
            "PollingOrderUpdateAdapter",
            broker_name=self.broker,
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
        self.books.put(self.empty_book)
        self.polls.get(timeout=10)  # the baseline is read, the next read waits

    def serve(self, method, path, body):
        if path != self.order_book_path:
            raise SystemExit(f"the {self.broker} glue serves no {path}")
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
