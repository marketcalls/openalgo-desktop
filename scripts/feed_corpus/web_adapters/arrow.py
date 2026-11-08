"""Arrow: the market-data client's packet parser and the adapter's tick
normaliser, and the order-update adapter's normaliser.

State as `ArrowWebSocketAdapter.subscribe` and `ArrowWebSocket` keep it
(arrow_adapter.py:126-155: `token_info`, the client's `token_exchange_map`;
arrow_websocket.py:327-342 `_send_subscribe`: `mode_map`,
`subscribed_tokens`), and as `unsubscribe` drops it (arrow_adapter.py:
157-171, arrow_websocket.py:344-365: the whole token goes, whatever mode
another client still holds). `to_openalgo_symbol` reads the database; the
glue's stand-in does the same lookups on the case's symbol master.
"""

from webkit import Glue, Source


class Arrow(Glue):
    broker = "arrow"
    sources = [
        Source("broker/arrow/streaming/arrow_mapping.py", "ArrowCapabilityRegistry"),
        Source("broker/arrow/streaming/arrow_websocket.py", "ArrowWebSocket", ["_parse_packet"]),
        Source("broker/arrow/streaming/arrow_adapter.py", None, ["_MODE_TO_TOPIC"]),
        Source("broker/arrow/streaming/arrow_adapter.py", "ArrowWebSocketAdapter", ["_on_ticks", "_normalize"]),
        Source(
            "broker/arrow/streaming/arrow_order_adapter.py",
            None,
            ["_STATUS_MAP", "_PRICETYPE_MAP", "_PRODUCT_MAP", "_ACTION_MAP", "_num"],
        ),
        Source("broker/arrow/streaming/arrow_order_adapter.py", "ArrowOrderUpdateAdapter", ["normalize"]),
    ]

    def stubs(self):
        return {"to_openalgo_symbol": self.to_openalgo_symbol}

    def start(self):
        self.ws = self.new("ArrowWebSocket", token_exchange_map={}, mode_map={}, subscribed_tokens=set())
        self.ad = self.new("ArrowWebSocketAdapter", token_info={}, publish_market_data=self.publish)
        self.orders = self.new("ArrowOrderUpdateAdapter")

    def subscribe(self, row, mode, depth):
        token = int(row["token"])
        arrow_mode = self.web["ArrowCapabilityRegistry"].get_arrow_mode_for_numeric(mode)
        self.ad.token_info[token] = {"symbol": row["symbol"], "exchange": row["exchange"], "mode": mode}
        self.ws.token_exchange_map[token] = row["exchange"]
        self.ws.mode_map[token] = arrow_mode
        self.ws.subscribed_tokens.add(token)

    def unsubscribe(self, row, mode, depth):
        token = int(row["token"])
        self.ws.subscribed_tokens.discard(token)
        self.ws.mode_map.pop(token, None)
        self.ws.token_exchange_map.pop(token, None)
        self.ad.token_info.pop(token, None)

    def push(self, socket, frame):
        if socket == "orders":
            fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
        elif isinstance(frame, bytes):
            # arrow_websocket.py:391-398 (_on_ws_message): one packet per message.
            tick = self.ws._parse_packet(frame)
            if tick:
                self.ad._on_ticks([tick])
