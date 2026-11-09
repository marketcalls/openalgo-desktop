"""Definedge: the NorenWSTRTP socket's message dispatch, the adapter's
per-scrip snapshot and its normalisers, and the order-update adapter's
normaliser, with the state test/test_definedge_quote_feed.py builds.

A frame goes through `DefinedGeWebSocket._on_message` (ck, tk/tf to the
touchline handler, dk/df to the depth handler) into the adapter's
`_on_data` / `_on_depth_data`, `_publish_to_subscribers` and
`MarketDataCache`. Subscriptions are kept as `DefinedgeWebSocketAdapter.
subscribe` stores them (definedge_adapter.py:465-481): one entry per
client reference, keyed by a correlation id that starts with
`symbol_exchange_mode`, with the scrip `EXCH|token`. An unsubscribe drops
the first matching entry and, when no entry holds the scrip any more, its
cache slot (definedge_adapter.py:575-584). A reconnect clears nothing:
`_on_close` and `_on_open` / `_resubscribe_all` keep the snapshot
(definedge_adapter.py:679-760).

`to_openalgo_symbol` reads the database; the glue's stand-in does the same
lookups on the case's symbol master (`Glue.to_openalgo_symbol`).
"""

from webkit import Glue, Source


class _Socket:
    """The order adapter's `_ws`: what it sends after the connect ack."""

    def __init__(self):
        self.sent = []

    def send(self, text):
        self.sent.append(text)


class Definedge(Glue):
    broker = "definedge"
    sources = [
        Source("broker/definedge/streaming/definedge_adapter.py", "MarketDataCache"),
        Source(
            "broker/definedge/streaming/definedge_adapter.py",
            "DefinedgeWebSocketAdapter",
            [
                "_on_data",
                "_on_depth_data",
                "_publish_to_subscribers",
                "_last_trade_time",
                "_add_reference_keys",
                "_normalize_raw_data",
                "_present_fields",
                "_safe_float",
                "_safe_int",
                "_feed_types",
                "_normalize_raw_depth_data",
            ],
        ),
        Source(
            "broker/definedge/streaming/definedge_websocket.py",
            "DefinedGeWebSocket",
            [
                "_on_message",
                "_handle_auth_response",
                "_handle_tick_data",
                "_handle_depth_data",
                "_handle_subscription_ack",
                "_handle_depth_ack",
                "_handle_unsubscribe_ack",
                "_handle_unsubscribe_depth_ack",
            ],
        ),
        Source(
            "broker/definedge/streaming/definedge_order_adapter.py",
            None,
            ["_STATUS_MAP", "_PRICETYPE_MAP", "_PRODUCT_MAP", "_ACTION_MAP"],
        ),
        Source(
            "broker/definedge/streaming/definedge_order_adapter.py",
            "DefinedgeOrderUpdateAdapter",
            ["normalize"],
        ),
    ]

    def stubs(self):
        return {"to_openalgo_symbol": self.to_openalgo_symbol}

    def start(self):
        cache = self.new("MarketDataCache", _cache={}, _initialized_scrips=set())
        cache._lock = cache.lock
        self.ad = self.new(
            "DefinedgeWebSocketAdapter",
            subscriptions={},
            scrip_to_symbol={},
            market_cache=cache,
            publish_market_data=self.publish,
        )
        self.ws = self.new(
            "DefinedGeWebSocket",
            uid="u",
            authenticated=False,
            on_tick=self.ad._on_data,
            on_depth=self.ad._on_depth_data,
        )
        self.ws._update_last_message_time = lambda: None
        self.ws._update_last_data_time = lambda: None
        self.order_ws = _Socket()
        self.orders = self.new("DefinedgeOrderUpdateAdapter", definedge_uid="u", _ws=self.order_ws)
        self.refs = 0

    @staticmethod
    def scrip(row):
        exchange = {"NSE_INDEX": "NSE", "BSE_INDEX": "BSE"}.get(row["brexchange"], row["brexchange"])
        return exchange, str(row["token"])

    def subscribe(self, row, mode, depth):
        exchange, token = self.scrip(row)
        self.refs += 1
        self.ad.subscriptions[f"{row['symbol']}_{row['exchange']}_{mode}_{self.refs}"] = {
            "symbol": row["symbol"],
            "exchange": row["exchange"],
            "brexchange": row["brexchange"],
            "definedge_exchange": exchange,
            "token": token,
            "scrip": f"{exchange}|{token}",
            "mode": mode,
            "depth_level": depth,
            "actual_depth": 5,
        }
        self.ad.scrip_to_symbol[f"{exchange}|{token}"] = (row["symbol"], row["exchange"])

    def unsubscribe(self, row, mode, depth):
        exchange, token = self.scrip(row)
        scrip = f"{exchange}|{token}"
        base = f"{row['symbol']}_{row['exchange']}_{mode}"
        cid = next((c for c in self.ad.subscriptions if c.startswith(base)), None)
        if cid is None:
            return
        del self.ad.subscriptions[cid]
        if not any(s["scrip"] == scrip for s in self.ad.subscriptions.values()):
            self.ad.scrip_to_symbol.pop(scrip, None)
            self.ad.market_cache.clear(scrip)

    def push(self, socket, frame):
        if socket == "orders":
            fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
        else:
            self.ws._on_message(None, frame)

    def reconnect(self, socket):
        # The web keeps the snapshot across a reconnect (module docstring).
        pass
