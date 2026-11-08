"""Upstox: the adapter's per-instrument merge and payload builder
(`_on_market_data`, `_process_feed`, `_merge_feed`, `_build_payload`, the CAS
extras), fed the dict `MessageToDict` makes of each protobuf frame
(`web_adapters/upstox_proto.py`, since the protobuf package is not needed
here), with the subscriptions `UpstoxWebSocketAdapter.subscribe` keeps; and
the order adapter's normaliser.

The web keeps the merged snapshot across a reconnect (`_on_connect` replays
the subscriptions only), so `reconnect` changes nothing. The order adapter
reads the OpenAlgo symbol with `database.token_db.get_symbol`, imported
inside `normalize`; the glue puts a stand-in module there that looks the
token up on the case's master.
"""

import sys
import types

from web_adapters.upstox_proto import feed_response
from webkit import Glue, Source


class Upstox(Glue):
    broker = "upstox"
    sources = [
        Source(
            "broker/upstox/streaming/upstox_adapter.py",
            "UpstoxWebSocketAdapter",
            [
                "_create_topic",
                "_on_market_data",
                "_process_feed",
                "_full_feed_body",
                "_merge_feed",
                "_quote_fields",
                "_build_payload",
                "_extract_tick_ltpc",
                "_read_optional_double",
                "_extract_cas_fields",
            ],
        ),
        Source("broker/upstox/streaming/upstox_order_adapter.py", None, ["_STATUS_MAP", "_PRODUCT_MAP"]),
        Source("broker/upstox/streaming/upstox_order_adapter.py", "UpstoxOrderUpdateAdapter", ["normalize"]),
    ]

    def start(self):
        self.ad = self.new(
            "UpstoxWebSocketAdapter",
            subscriptions={},
            _snapshots={},
            market_status={},
            publish_market_data=self.publish,
        )
        self.orders = self.new("UpstoxOrderUpdateAdapter")
        db = types.ModuleType("database")
        token_db = types.ModuleType("database.token_db")

        def get_symbol(token, exchange):
            row = self.master.by_token(token, exchange)
            return row["symbol"] if row else None

        token_db.get_symbol = get_symbol
        db.token_db = token_db
        sys.modules.setdefault("database", db)
        sys.modules["database.token_db"] = token_db

    @staticmethod
    def key(row):
        return f"{row['brexchange']}|{str(row['token']).split('|')[-1]}"

    def subscribe(self, row, mode, depth):
        self.ad.subscriptions[f"{row['symbol']}_{row['exchange']}_{mode}"] = {
            "symbol": row["symbol"],
            "exchange": row["exchange"],
            "mode": mode,
            "depth_level": depth,
            "token": row["token"],
            "instrument_key": self.key(row),
        }

    def unsubscribe(self, row, mode, depth):
        self.ad.subscriptions.pop(f"{row['symbol']}_{row['exchange']}_{mode}", None)
        if not any(s["instrument_key"] == self.key(row) for s in self.ad.subscriptions.values()):
            self.ad._snapshots.pop(self.key(row), None)

    def push(self, socket, frame):
        if socket == "orders":
            fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
        elif isinstance(frame, bytes):
            self.ad._on_market_data(feed_response(frame))
