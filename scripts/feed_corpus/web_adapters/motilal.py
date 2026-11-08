"""Motilal Oswal: the broadcast client's binary parsers and stores, the
adapter's subscribe/unsubscribe and one pass of its publishing poll, and
the order adapter's normaliser.

The web adapter does not publish per packet: `MotilalWebSocket` stores
what each 30-byte packet carries (`last_quotes`, `last_depth`, `last_oi`)
and `MotilalWebSocketAdapter._poll_market_data` publishes every
subscription from those stores every 500 ms. The desktop publishes when a
packet changes what a subscription reports (an intentional improvement the
audit keeps). So a push here is the packet followed by the web's next poll:
one pass of `_poll_market_data` (its stop event set after the first pass),
publishing for every subscription what the stores then hold. A web publish
that repeats an unchanged subscription has no desktop frame to meet, and
the harness compares only the frames a client receives.

State as `MotilalWebSocketAdapter.subscribe` and
`MotilalWebSocket.register_scrip` build it, both lifted and run as written
against a stand-in socket; `SymbolMapper.get_token_from_symbol` reads the
case's symbol master in place of the database.
"""

import types

from webkit import Glue, Source

WS = "broker/motilal/api/motilal_websocket.py"
ADAPTER = "broker/motilal/streaming/motilal_adapter.py"
ORDERS = "broker/motilal/streaming/motilal_order_adapter.py"


class _Socket:
    """The broadcast socket: takes what register_scrip sends."""

    def send(self, *_a, **_k):
        pass


class _OnePass:
    """`_stop_poll` for one pass of the poll loop: not set when the loop
    starts, set when it waits after the pass."""

    def __init__(self):
        self.passed = False

    def is_set(self):
        return self.passed

    def wait(self, timeout=None):
        self.passed = True
        return True

    def clear(self):
        self.passed = False


class Motilal(Glue):
    broker = "motilal"
    sources = [
        Source("websocket_proxy/base_adapter.py", "BaseBrokerWebSocketAdapter",
               ["_create_success_response", "_create_error_response"]),
        Source("broker/motilal/streaming/motilal_mapping.py", "MotilalExchangeMapper"),
        Source("broker/motilal/streaming/motilal_mapping.py", "MotilalCapabilityRegistry"),
        Source(
            WS,
            "MotilalWebSocket",
            [
                "_parse_binary_market_data",
                "_map_exchange_back",
                "_store_key",
                "_resolve_exchange_name",
                "_parse_depth_level_packet",
                "_parse_ltp_packet",
                "_parse_dpr_packet",
                "_parse_ohlc_packet",
                "_parse_oi_packet",
                "_parse_index_packet",
                "register_scrip",
                "unregister_scrip",
                "_map_exchange_to_char",
                "get_quote",
                "get_market_depth",
                "get_open_interest",
            ],
        ),
        Source(
            ADAPTER,
            "MotilalWebSocketAdapter",
            [
                "subscribe",
                "unsubscribe",
                "_poll_market_data",
                "_normalize_ltp_data",
                "_normalize_quote_data",
                "_normalize_depth_data",
            ],
        ),
        Source("broker/motilal/mapping/transform_data.py", None,
               ["reverse_map_exchange", "reverse_map_product_type"]),
        Source(ORDERS, None, ["_STATUS_MAP", "_WS_ERROR_CODES", "_to_int", "_to_float"]),
        Source(ORDERS, "MotilalOrderUpdateAdapter", ["normalize", "_normalize_order"]),
    ]

    def stubs(self):
        glue = self

        class SymbolMapper:
            @staticmethod
            def get_token_from_symbol(symbol, exchange):
                row = glue.master.by_symbol(symbol, exchange)
                return {"token": row["token"], "brexchange": row["brexchange"]} if row else None

        websocket = types.SimpleNamespace(ABNF=types.SimpleNamespace(OPCODE_BINARY=2))
        return {
            "SymbolMapper": SymbolMapper,
            "websocket": websocket,
            # MotilalWebSocket's class-level PRIMARY_URL / UAT_URL.
            "get_ws_feed_url": lambda use_uat=False: "wss://feed.invalid",
            "to_openalgo_symbol": self.to_openalgo_symbol,
        }

    def start(self):
        self.ws = self.new(
            "MotilalWebSocket",
            is_connected=True,
            ws=_Socket(),
            subscribed_scrips={},
            subscriptions={},
            _scrip_exchange={},
            last_quotes={},
            last_depth={},
            last_oi={},
            last_index={},
        )
        base = self.web["BaseBrokerWebSocketAdapter"]
        self.ad = self.new(
            "MotilalWebSocketAdapter",
            connected=True,
            running=True,
            ws_client=self.ws,
            subscriptions={},
            _stop_poll=_OnePass(),
            publish_market_data=self.publish,
        )
        self.ad._create_success_response = types.MethodType(base._create_success_response, self.ad)
        self.ad._create_error_response = types.MethodType(base._create_error_response, self.ad)
        self.orders = self.new("MotilalOrderUpdateAdapter")

    def subscribe(self, row, mode, depth):
        r = self.ad.subscribe(row["symbol"], row["exchange"], mode, depth)
        if r.get("status") != "success":
            raise SystemExit(f"the web adapter refused {row['symbol']} on {row['exchange']}: {r}")

    def unsubscribe(self, row, mode, depth):
        self.ad.unsubscribe(row["symbol"], row["exchange"], mode, depth)

    def push(self, socket, frame):
        if socket == "orders":
            fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
        elif isinstance(frame, bytes):
            self.ws._parse_binary_market_data(frame)
            self.ad._stop_poll.clear()
            self.ad._poll_market_data()
