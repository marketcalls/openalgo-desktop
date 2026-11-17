"""Nuvama: the stream client's line handler, the market data adapter's
packet merge and normaliser, and the order adapter's normaliser.

The desktop reaches Nuvama's plain-TCP socket through its loopback line
relay (brokers/nuvama/tcp_relay.rs), which hands each line Nuvama sends on
as one text frame; the corpus pushes those frames. The web's client reads
the same lines off its own socket (`NuvamaStreamClient._handle_line`), so
`push` hands the web exactly the line the desktop parses. The relay's
ready text frame is the desktop's own and reaches nothing on the web (it
carries no `response`, which `_handle_line` skips).

State as `NuvamaWebSocketAdapter.subscribe` and `unsubscribe` keep it
(nuvama_adapter.py): `token_info` keyed by the master token with every
held mode, and the last quote and depth packet per token, dropped only
when the token's last mode goes (a reconnect keeps them). The order
adapter's packets go through `normalize_order_update`, published with the
defaults `NuvamaOrderUpdateAdapter._on_packet` sets.
"""

import threading

from webkit import Glue, Source

_ADAPTER = "broker/nuvama/streaming/nuvama_adapter.py"
_ORDERS = "broker/nuvama/streaming/nuvama_order_adapter.py"


class Nuvama(Glue):
    broker = "nuvama"
    sources = [
        Source("broker/nuvama/streaming/nuvama_mapping.py", None, ["_TOPIC_MODE", "topic_mode"]),
        Source("broker/nuvama/mapping/order_data.py", None, ["normalize_status"]),
        Source(
            "broker/nuvama/mapping/transform_data.py",
            None,
            ["_ORDER_TYPE_REVERSE", "_PRODUCT_REVERSE", "_SIDE_REVERSE", "reverse_map_order_type",
             "reverse_map_product_type", "reverse_map_side"],
        ),
        Source(_ORDERS, None, ["_FINAL_STATUSES", "normalize_order_update"]),
        # The adapter's own number readers last, so its `_f` is the one its
        # methods call (the order adapter's reads the same values alike).
        Source(_ADAPTER, None, ["_f", "_i", "_epoch_ms"]),
        Source(_ADAPTER, "NuvamaWebSocketAdapter", ["_on_packet", "_normalize", "_depth"]),
        Source("broker/nuvama/streaming/nuvama_socket.py", "NuvamaStreamClient", ["_handle_line"]),
    ]

    def stubs(self):
        return {"to_openalgo_symbol": self.to_openalgo_symbol}

    def start(self):
        self.ad = self.new("NuvamaWebSocketAdapter", token_info={}, _last_quote={}, _last_depth={},
                           _lock=threading.Lock(), publish_market_data=self.publish)
        self.market = self.new("NuvamaStreamClient", on_packet=self.ad._on_packet)
        self.orders = self.new("NuvamaStreamClient", on_packet=self._on_order_packet)

    def _on_order_packet(self, streaming_type, data):
        """nuvama_order_adapter.py `NuvamaOrderUpdateAdapter._on_packet`."""
        if streaming_type != "orderFiler":
            return
        fields = self.web["normalize_order_update"](data)
        if not fields:
            return
        fields.setdefault("mode", "live")
        fields.setdefault("broker", self.broker)
        self.order(fields)

    def subscribe(self, row, mode, depth):
        info = self.ad.token_info.setdefault(
            str(row["token"]), {"symbol": row["symbol"], "exchange": row["exchange"], "modes": set()}
        )
        info["modes"].add(int(mode))

    def unsubscribe(self, row, mode, depth):
        token = str(row["token"])
        info = self.ad.token_info.get(token)
        if info:
            info["modes"].discard(int(mode))
            if not info["modes"]:
                self.ad.token_info.pop(token, None)
                self.ad._last_quote.pop(token, None)
                self.ad._last_depth.pop(token, None)

    def push(self, socket, frame):
        line = frame.encode() if isinstance(frame, str) else frame
        client = self.orders if socket == "orders" else self.market
        client._handle_line(line)
        # One quote packet publishes per held mode from a set: put them in
        # mode order, so web.json is the same on every run.
        self.published.sort(key=lambda p: p.get("mode", 0))
