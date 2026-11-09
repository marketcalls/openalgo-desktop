"""5paisa (the native feed, not 5paisa XTS): the websocket client's frame
split (an array frame is one `on_data` per object) and the adapter's
routing, snapshot and normalising methods, with the state
test/test_fivepaisa_quote_feed.py builds (`__new__`, `last_snapshot`,
`subscriptions` as `FivepaisaWebSocketAdapter.subscribe` stores them).

The adapter keeps `last_snapshot` across a reconnect (`_on_open` and
`_on_close` leave it), and an unsubscribe releases a scrip's snapshot only
when no subscription on its (Exch, ExchType, Token) remains, as
`unsubscribe` does.

Order updates: the web's own order-book poller (`polling.py`) over
5paisa's `broker/fivepaisa/mapping/order_data.py` (`map_order_data`,
`calculate_order_statistics`, `transform_order_data`), with
`get_symbol` reading the case's symbol master.
"""

from web_adapters.polling import POLLING_SOURCES, Polling
from webkit import Glue, Source

# brokers/fivepaisa/orders.rs ORDER_BOOK; web broker/fivepaisa/api/order_api.py get_order_book.
ORDER_BOOK = "/VendorsAPI/Service1.svc/V3/OrderBook"
# The book tests/it/feed_corpus/fivepaisa.rs serves before any case does.
EMPTY_BOOK = '{"head": {"statusDescription": "Success"}, "body": {"Message": "Success", "OrderBookDetail": [], "Status": 0}}'


class Fivepaisa(Polling, Glue):
    broker = "fivepaisa"
    order_book_path = ORDER_BOOK
    empty_book = EMPTY_BOOK
    sources = [
        Source("broker/fivepaisa/streaming/fivepaisa_mapping.py", "FivePaisaExchangeMapper"),
        Source("broker/fivepaisa/streaming/fivepaisa_mapping.py", "FivePaisaCapabilityRegistry"),
        Source("broker/fivepaisa/streaming/fivepaisa_websocket.py", "FivePaisaWebSocket", ["_on_message"]),
        Source(
            "broker/fivepaisa/streaming/fivepaisa_adapter.py",
            "FivepaisaWebSocketAdapter",
            [
                "_methods_for_mode",
                "_scrip_key",
                "_sub_scrip_key",
                "_on_data",
                "_update_snapshot",
                "_quote_fields",
                "_normalize_market_data",
                "_extract_depth_data",
                "_parse_fivepaisa_time",
            ],
        ),
        Source("broker/fivepaisa/mapping/transform_data.py", None, ["reverse_map_exchange"]),
        Source(
            "broker/fivepaisa/mapping/order_data.py",
            None,
            [
                "_ORDER_STATUS_MAP",
                "normalize_order_status",
                "convert_date_string",
                "map_order_data",
                "calculate_order_statistics",
                "transform_order_data",
            ],
        ),
        *POLLING_SOURCES,
    ]

    def stubs(self):
        def get_symbol(token, exchange):
            row = self.master.by_token(str(token), exchange)
            return row["symbol"] if row else None

        return {"get_symbol": get_symbol, "get_oa_symbol": get_symbol, **self.polling_stubs()}

    def start(self):
        self.ad = self.new(
            "FivepaisaWebSocketAdapter",
            last_snapshot={},
            subscriptions={},
            publish_market_data=self.publish,
        )
        self.ws = self.new("FivePaisaWebSocket", on_data=self.ad._on_data, on_message=lambda *a: None)

    def scrip(self, row):
        mapper = self.web["FivePaisaExchangeMapper"]
        return {
            "Exch": mapper.get_exchange_code(row["brexchange"]),
            "ExchType": mapper.get_exchange_type(row["brexchange"]),
            "ScripCode": int(row["token"]),
        }

    @staticmethod
    def correlation_id(row, mode, depth):
        cid = f"{row['symbol']}_{row['exchange']}_{mode}"
        return f"{cid}_{depth}" if mode == 3 else cid

    def subscribe(self, row, mode, depth):
        # fivepaisa_adapter.py:411-414: Depth at any level but 5 is refused
        # (INVALID_DEPTH) before anything is stored.
        if mode == 3 and depth != 5:
            return
        methods = self.ad._methods_for_mode(mode)
        self.ad.subscriptions[self.correlation_id(row, mode, depth)] = {
            "symbol": row["symbol"],
            "exchange": row["exchange"],
            "brexchange": row["brexchange"],
            "token": str(row["token"]),
            "mode": mode,
            "depth_level": depth,
            "method": methods[0],
            "methods": methods,
            "scrip_data": [self.scrip(row)],
        }

    def unsubscribe(self, row, mode, depth):
        cid = f"{row['symbol']}_{row['exchange']}_{mode}"
        for key in [k for k in self.ad.subscriptions if k == cid or k.startswith(f"{cid}_")]:
            del self.ad.subscriptions[key]
        sc = self.scrip(row)
        key = self.ad._scrip_key(sc["Exch"], sc["ExchType"], str(row["token"]))
        if not any(self.ad._sub_scrip_key(s) == key for s in self.ad.subscriptions.values()):
            self.ad.last_snapshot.pop(key, None)

    def push(self, socket, frame):
        if isinstance(frame, str):
            self.ws._on_message(None, frame)
