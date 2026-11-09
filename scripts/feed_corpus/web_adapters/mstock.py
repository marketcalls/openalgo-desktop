"""mStock: the binary frame and packet parsers of the market data client
and the adapter's tick fan-out and normaliser, with the state
test/test_mstock_quote_feed.py builds.

`MstockWebSocketAdapter.subscribe` keeps one row per symbol, exchange and
mode (mstock_adapter.py:254-262), with the exchange type its
`MstockExchangeMapper` gives the master row's broker exchange; `_on_data`
routes a packet to every row of its exchange type and token
(`:157-210`). The web exposes no order-update stream for mStock
(services/order_update_service.py), so there is nothing to relay.
"""

from webkit import Glue, Source


class Mstock(Glue):
    broker = "mstock"
    sources = [
        Source("broker/mstock/streaming/mstock_mapping.py", "MstockExchangeMapper"),
        Source(
            "broker/mstock/api/mstockwebsocket.py",
            "MstockWebSocket",
            ["parse_binary_packet", "parse_binary_message"],
        ),
        Source(
            "broker/mstock/streaming/mstock_adapter.py",
            "MstockWebSocketAdapter",
            ["_on_data", "_normalize_market_data"],
        ),
    ]

    def start(self):
        self.ws = self.web["MstockWebSocket"]
        self.mapper = self.web["MstockExchangeMapper"]
        self.ad = self.new("MstockWebSocketAdapter", subscriptions={}, publish_market_data=self.publish)

    def subscribe(self, row, mode, depth):
        # mstock_adapter.py:239-242: INVALID_DEPTH, nothing stored.
        if mode == 3 and depth != 5:
            return
        self.ad.subscriptions[f"{row['symbol']}_{row['exchange']}_{mode}"] = {
            "symbol": row["symbol"],
            "exchange": row["exchange"],
            "brexchange": row["brexchange"],
            "token": str(row["token"]),
            "mode": mode,
            "depth_level": depth,
            "exchange_type": self.mapper.get_exchange_type(row["brexchange"]),
        }

    def unsubscribe(self, row, mode, depth):
        self.ad.subscriptions.pop(f"{row['symbol']}_{row['exchange']}_{mode}", None)

    def push(self, socket, frame):
        if isinstance(frame, bytes):
            for quote in self.ws.parse_binary_message(frame):
                self.ad._on_data(quote)
