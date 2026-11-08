"""5paisa (the native feed, not 5paisa XTS): the websocket client's frame
split (an array frame is one `on_data` per object) and the adapter's
routing, snapshot and normalising methods, with the state
test/test_fivepaisa_quote_feed.py builds (`__new__`, `last_snapshot`,
`subscriptions` as `FivepaisaWebSocketAdapter.subscribe` stores them).

The adapter keeps `last_snapshot` across a reconnect (`_on_open` and
`_on_close` leave it), and an unsubscribe releases a scrip's snapshot only
when no subscription on its (Exch, ExchType, Token) remains, as
`unsubscribe` does. Order updates are polled from the REST order book on
both sides (`PollingOrderUpdateAdapter`); this glue streams none.
"""

from webkit import Glue, Source


class Fivepaisa(Glue):
    broker = "fivepaisa"
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
    ]

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
