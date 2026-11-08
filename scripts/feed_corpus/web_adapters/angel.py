"""Angel One: SmartWebSocketV2's binary parser and the adapter's
normaliser, with the state test/test_angel_quote_feed.py builds."""

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
    ]

    def start(self):
        self.ws = self.new("SmartWebSocketV2")
        self.ad = self.new(
            "AngelWebSocketAdapter",
            subscriptions={},
            _token_index={},
            publish_market_data=self.publish,
        )

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
        if isinstance(frame, bytes):
            self.ad._on_data(None, self.ws._parse_binary_data(frame))
