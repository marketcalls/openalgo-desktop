"""Rupeezy: the Vortex frame splitter and packet parser, and the adapter's
tick fan-out and normaliser.

State as `RupeezyWebSocketAdapter.subscribe` and `unsubscribe` keep it
(rupeezy_adapter.py:113-171): `instruments`, keyed by the master's
brexchange and token, with every OpenAlgo mode held, so a packet is
published to each held topic up to its own richness (`_topics_for`).
"""

from webkit import Glue, Source

_PARSER = [
    "_LTP", "_OHLCV", "_FULL_HEAD", "_DEPTH_LEVEL", "_DPR", "_U16", "LTP_SIZE", "OHLCV_SIZE", "FULL_SIZE",
    "parse_packet", "split_message",
]


class Rupeezy(Glue):
    broker = "rupeezy"
    sources = [
        Source("broker/rupeezy/streaming/rupeezy_websocket.py", None, _PARSER),
        Source("broker/rupeezy/mapping/exchange.py", None, ["build_ticker"]),
        Source("broker/rupeezy/streaming/rupeezy_mapping.py", "RupeezyCapabilityRegistry"),
        Source("broker/rupeezy/streaming/rupeezy_adapter.py", None, ["_MODE_TO_TOPIC", "_NUMERIC_TO_TOPIC", "_TOPIC_RANK"]),
        Source(
            "broker/rupeezy/streaming/rupeezy_adapter.py",
            "RupeezyWebSocketAdapter",
            ["_richest_mode", "_on_ticks", "_topics_for", "_normalize"],
        ),
    ]

    def start(self):
        self.ad = self.new("RupeezyWebSocketAdapter", instruments={}, publish_market_data=self.publish)

    @staticmethod
    def key(row):
        return (row["brexchange"], int(row["token"]))

    def subscribe(self, row, mode, depth):
        entry = self.ad.instruments.setdefault(
            self.key(row),
            {
                "symbol": row["symbol"],
                "exchange": row["exchange"],
                "ticker": self.web["build_ticker"](row["brexchange"], row["brsymbol"]),
                "modes": set(),
            },
        )
        entry["modes"].add(mode)

    def unsubscribe(self, row, mode, depth):
        entry = self.ad.instruments.get(self.key(row))
        if entry:
            entry["modes"].discard(mode)
            if not entry["modes"]:
                self.ad.instruments.pop(self.key(row), None)

    def push(self, socket, frame):
        # rupeezy_websocket.py::_on_message: text frames (order postbacks) are dropped.
        if not isinstance(frame, bytes):
            return
        ticks = [t for t in map(self.web["parse_packet"], self.web["split_message"](frame)) if t]
        if ticks:
            self.ad._on_ticks(ticks)
