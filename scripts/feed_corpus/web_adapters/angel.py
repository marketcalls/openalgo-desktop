"""Angel One: SmartWebSocketV2's binary parser and the adapter's
normaliser, with the state test/test_angel_quote_feed.py builds, and the
order-status adapter's normaliser.

`to_openalgo_symbol` reads the database; the glue's stand-in does the same
lookups on the case's symbol master (`Glue.to_openalgo_symbol`).
"""

from webkit import Glue, Source


class Angel(Glue):
    broker = "angel"
    sources = [
        Source("broker/angel/streaming/angel_mapping.py", "AngelExchangeMapper"),
        Source(
            "broker/angel/streaming/smartWebSocketV2.py",
            "SmartWebSocketV2",
            [
                "_parse_binary_data",
                "_unpack_data",
                "_parse_token_value",
                "_parse_best_5_buy_and_sell_data",
                "_parse_depth_20_buy_and_sell_data",
            ],
        ),
        Source(
            "broker/angel/streaming/angel_adapter.py",
            "AngelWebSocketAdapter",
            ["_on_data", "_price_divisor", "_normalize_market_data", "_extract_depth_data"],
        ),
        Source(
            "broker/angel/streaming/angel_order_adapter.py",
            None,
            ["_STATUS_CODE_MAP", "_STATUS_TEXT_MAP", "_PRODUCT_MAP", "_PRICETYPE_MAP"],
        ),
        Source("broker/angel/streaming/angel_order_adapter.py", "AngelOrderUpdateAdapter", ["normalize"]),
    ]

    def stubs(self):
        return {"to_openalgo_symbol": self.to_openalgo_symbol}

    def start(self):
        self.ws = self.new("SmartWebSocketV2")
        self.ad = self.new(
            "AngelWebSocketAdapter",
            subscriptions={},
            _token_index={},
            publish_market_data=self.publish,
        )
        self.orders = self.new("AngelOrderUpdateAdapter")

    def subscribe(self, row, mode, depth):
        sub = {
            "symbol": row["symbol"],
            "exchange": row["exchange"],
            "brexchange": row["brexchange"],
            "token": row["token"],
            "mode": mode,
        }
        exchange_type = self.web["AngelExchangeMapper"].get_exchange_type(row["brexchange"])
        self.ad.subscriptions[f"{row['symbol']}_{row['exchange']}_{mode}"] = sub
        self.ad._token_index[(str(row["token"]), exchange_type)] = sub

    def unsubscribe(self, row, mode, depth):
        self.ad.subscriptions.pop(f"{row['symbol']}_{row['exchange']}_{mode}", None)

    def push(self, socket, frame):
        if socket == "orders":
            fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
        elif isinstance(frame, bytes):
            self.ad._on_data(None, self.ws._parse_binary_data(frame))
