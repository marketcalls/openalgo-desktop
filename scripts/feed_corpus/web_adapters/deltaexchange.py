"""Delta Exchange: the adapter's ticker and ob_l2 normalisers, the per-symbol
merge cache and the fan-out to every subscription of the symbol
(`DeltaWebSocketAdapter._on_data`), with the subscriptions
`DeltaWebSocketAdapter.subscribe` keeps (`br_symbol`, the channels of the
mode from `DeltaModeMapper`)."""

import json
import threading

from webkit import Glue, Source


class DeltaExchange(Glue):
    broker = "deltaexchange"
    sources = [
        Source("broker/deltaexchange/streaming/delta_mapping.py", "DeltaModeMapper"),
        Source("broker/deltaexchange/streaming/delta_websocket.py", "DeltaWebSocket", []),
        Source("broker/deltaexchange/streaming/delta_adapter.py", None, ["_f", "_i"]),
        Source(
            "broker/deltaexchange/streaming/delta_adapter.py",
            "DeltaWebSocketAdapter",
            [
                "_on_data",
                "_normalise_ticker",
                "_normalise_orderbook",
                "_merge_into_cache",
                "_find_subscriptions_by_br_symbol",
            ],
        ),
    ]

    def start(self):
        self.ad = self.new(
            "DeltaWebSocketAdapter",
            subscriptions={},
            last_values={},
            _lock=threading.Lock(),
            publish_market_data=self.publish,
        )

    def subscribe(self, row, mode, depth):
        self.ad.subscriptions[f"{row['symbol']}_{row['exchange']}_{mode}"] = {
            "symbol": row["symbol"],
            "exchange": row["exchange"],
            "mode": mode,
            "br_symbol": row["brsymbol"],
            "channels": self.web["DeltaModeMapper"].get_channels(mode),
        }

    def unsubscribe(self, row, mode, depth):
        self.ad.subscriptions.pop(f"{row['symbol']}_{row['exchange']}_{mode}", None)

    def push(self, socket, frame):
        self.ad._on_data(None, json.loads(frame))
