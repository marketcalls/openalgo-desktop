"""Dhan: the 5-depth market feed parser and the adapter's handler, and the
order-update adapter's normaliser, with the state
test/test_dhan_quote_feed.py builds.

`to_openalgo_symbol` (websocket_proxy/order_adapter.py:45) reads the
database; it is replaced by the same lookups on the case's symbol master:
the security id first, then the broker symbol, else the raw symbol.
"""

from webkit import Glue, Source


class Dhan(Glue):
    broker = "dhan"
    sources = [
        Source("broker/dhan/streaming/dhan_mapping.py", "DhanExchangeMapper"),
        Source(
            "broker/dhan/streaming/dhan_websocket.py",
            "DhanWebSocket",
            [
                "_parse_binary_message",
                "_parse_regular_message",
                "_parse_ticker_packet",
                "_parse_quote_packet",
                "_parse_oi_packet",
                "_parse_prev_close_packet",
                "_parse_full_packet",
                "_handle_disconnect_packet",
            ],
        ),
        Source(
            "broker/dhan/streaming/dhan_adapter.py",
            "DhanWebSocketAdapter",
            ["_on_data_5depth", "_prev_close_key", "_normalize_5depth_data"],
        ),
        Source(
            "broker/dhan/streaming/dhan_order_adapter.py",
            None,
            ["_STATUS_MAP", "_PRODUCT_MAP", "_PRICETYPE_MAP", "_ACTION_MAP", "_EXCHANGE_SEGMENT_MAP", "_field"],
        ),
        Source("broker/dhan/streaming/dhan_order_adapter.py", "DhanOrderUpdateAdapter", ["normalize"]),
    ]

    def stubs(self):
        def to_openalgo_symbol(broker_symbol, exchange, token=None):
            row = (token and self.master.by_token(token, exchange)) or self.master.by_brsymbol(
                broker_symbol, exchange
            )
            return row["symbol"] if row else broker_symbol

        return {"to_openalgo_symbol": to_openalgo_symbol}

    def start(self):
        self.ad = self.new(
            "DhanWebSocketAdapter",
            subscriptions_5depth={},
            prev_close_data={},
            publish_market_data=self.publish,
        )
        self.ws = self.new("DhanWebSocket", is_20_depth=False, on_data=self.ad._on_data_5depth)
        self.orders = self.new("DhanOrderUpdateAdapter")

    def subscribe(self, row, mode, depth):
        self.ad.subscriptions_5depth[f"{row['symbol']}_{row['exchange']}_{mode}_5"] = {
            "symbol": row["symbol"],
            "exchange": row["exchange"],
            "token": str(row["token"]),
            "mode": mode,
        }

    def unsubscribe(self, row, mode, depth):
        self.ad.subscriptions_5depth.pop(f"{row['symbol']}_{row['exchange']}_{mode}_5", None)
        if not self.ad.subscriptions_5depth:
            self.ad.prev_close_data.clear()

    def reconnect(self, socket):
        # web openalgo#2377: the stored previous close goes with the socket.
        if socket == "main":
            self.ad.prev_close_data.clear()

    def push(self, socket, frame):
        if socket == "orders":
            fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
        elif isinstance(frame, bytes):
            self.ws._parse_binary_message(frame)
