"""Paytm Money: the socket client's packet parsers and the adapter's data
handler and normaliser.

State as `PaytmWebSocketAdapter.subscribe` and `unsubscribe` keep it
(paytm_adapter.py:366-461, 463-550): `token_map`, keyed by the security id
alone, holding the mode subscribed last; a Depth subscribe at any depth
but 5 is refused (paytm_adapter.py:388-391) and registers nothing; an
unsubscribe drops the security id whatever mode another client holds.
The client parses one packet per binary message
(paytm_websocket.py:83-110 `_on_message`, 289 `_parse_binary_data`).
"""

from webkit import Glue, Source


class Paytm(Glue):
    broker = "paytm"
    sources = [
        Source(
            "broker/paytm/streaming/paytm_websocket.py",
            "PaytmWebSocket",
            [
                "_parse_binary_data",
                "_unpack",
                "_parse_ltp_packet",
                "_parse_index_ltp_packet",
                "_parse_quote_packet",
                "_parse_index_quote_packet",
                "_parse_full_packet",
                "_parse_index_full_packet",
            ],
        ),
        Source(
            "broker/paytm/streaming/paytm_adapter.py",
            "PaytmWebSocketAdapter",
            ["_on_data", "_normalize_market_data"],
        ),
    ]

    def start(self):
        self.ws = self.new("PaytmWebSocket")
        self.ad = self.new("PaytmWebSocketAdapter", token_map={}, publish_market_data=self.publish)
        self.held = set()

    def subscribe(self, row, mode, depth):
        if mode == 3 and depth != 5:
            return
        self.held.add((row["symbol"], row["exchange"], mode))
        self.ad.token_map[str(row["token"])] = (row["symbol"], row["exchange"], mode)

    def unsubscribe(self, row, mode, depth):
        key = (row["symbol"], row["exchange"], mode)
        if key in self.held:
            self.held.discard(key)
            self.ad.token_map.pop(str(row["token"]), None)

    def push(self, socket, frame):
        if isinstance(frame, bytes):
            # `_on_message` (paytm_websocket.py:101-108): a packet the parser
            # cannot read (cut short) is logged and dropped.
            try:
                parsed = self.ws._parse_binary_data(frame)
            except Exception:
                return
            self.ad._on_data(None, parsed)
