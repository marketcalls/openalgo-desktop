"""Dhan: the 5-depth and 20-depth feed parsers and the adapter's handlers,
and the order-update adapter's normaliser, with the state
test/test_dhan_quote_feed.py builds.

`to_openalgo_symbol` reads the database; the glue's stand-in does the same
lookups on the case's symbol master (`Glue.to_openalgo_symbol`).
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
                "_parse_20_depth_message",
                "_parse_ticker_packet",
                "_parse_quote_packet",
                "_parse_oi_packet",
                "_parse_prev_close_packet",
                "_parse_full_packet",
                "_parse_20_depth_packet",
                "_handle_disconnect_packet",
            ],
        ),
        Source(
            "broker/dhan/streaming/dhan_adapter.py",
            "DhanWebSocketAdapter",
            ["_on_data_5depth", "_on_data_20depth", "_prev_close_key", "_normalize_5depth_data"],
        ),
        Source(
            "broker/dhan/streaming/dhan_order_adapter.py",
            None,
            ["_STATUS_MAP", "_PRODUCT_MAP", "_PRICETYPE_MAP", "_ACTION_MAP", "_EXCHANGE_SEGMENT_MAP", "_field"],
        ),
        Source("broker/dhan/streaming/dhan_order_adapter.py", "DhanOrderUpdateAdapter", ["normalize"]),
    ]

    def stubs(self):
        return {"to_openalgo_symbol": self.to_openalgo_symbol}

    def start(self):
        self.ad = self.new(
            "DhanWebSocketAdapter",
            subscriptions_5depth={},
            subscriptions_20depth={},
            depth_20_accumulator={},
            depth_20_timeouts={},
            depth_20_data_received={},
            prev_close_data={},
            publish_market_data=self.publish,
        )
        self.ws = self.new("DhanWebSocket", is_20_depth=False, on_data=self.ad._on_data_5depth)
        self.ws20 = self.new("DhanWebSocket", is_20_depth=True, on_data=self.ad._on_data_20depth)
        self.orders = self.new("DhanOrderUpdateAdapter")

    @staticmethod
    def deep(row, mode, depth):
        """A Depth 20 subscription on NSE or NFO goes to the 20-depth socket."""
        return mode == 3 and depth == 20 and row["exchange"] in ("NSE", "NFO")

    def subscribe(self, row, mode, depth):
        sub = {"symbol": row["symbol"], "exchange": row["exchange"], "token": str(row["token"]), "mode": mode}
        if self.deep(row, mode, depth):
            self.ad.subscriptions_20depth[f"{row['symbol']}_{row['exchange']}_3_20"] = dict(sub, depth_level=20)
        else:
            self.ad.subscriptions_5depth[f"{row['symbol']}_{row['exchange']}_{mode}_5"] = sub

    def unsubscribe(self, row, mode, depth):
        if self.deep(row, mode, depth):
            self.ad.subscriptions_20depth.pop(f"{row['symbol']}_{row['exchange']}_3_20", None)
            return
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
            (self.ws20 if socket == "depth" else self.ws)._parse_binary_message(frame)
