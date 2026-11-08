"""Zerodha: the Kite ticker parser and the adapter's tick fan-out.

State as `ZerodhaWebSocketAdapter.subscribe` keeps it (zerodha_adapter.py:
324-331) and as test/test_zerodha_index_packets.py builds the parser.
"""

from webkit import Glue, Source


class Zerodha(Glue):
    broker = "zerodha"
    sources = [
        Source(
            "broker/zerodha/streaming/zerodha_websocket.py",
            "ZerodhaWebSocket",
            ["_parse_binary_message", "_parse_packet"],
        ),
        Source(
            "broker/zerodha/streaming/zerodha_adapter.py",
            "ZerodhaWebSocketAdapter",
            [
                "_handle_ticks",
                "_transform_tick",
                "_transform_index_tick",
                "_transform_regular_tick",
                "_map_data_exchange",
                "_generate_topic",
            ],
        ),
    ]

    def start(self):
        self.ws = self.new("ZerodhaWebSocket", token_exchange_map={}, mode_map={})
        self.ad = self.new(
            "ZerodhaWebSocketAdapter",
            subscribed_symbols={},
            token_to_symbol={},
            publish_market_data=self.publish,
        )

    @staticmethod
    def token(row):
        return int(str(row["token"]).split("::::")[0])

    def subscribe(self, row, mode, depth):
        token, symbol, exchange = self.token(row), row["symbol"], row["exchange"]
        self.ad.subscribed_symbols[f"{exchange}:{symbol}:{mode}"] = {
            "exchange": exchange,
            "symbol": symbol,
            "token": token,
            "mode": mode,
            "mapped_exchange": "NSE" if exchange == "NSE_INDEX" else exchange,
        }
        self.ad.token_to_symbol[token] = (symbol, exchange)
        self.ws.token_exchange_map[token] = exchange

    def unsubscribe(self, row, mode, depth):
        token, symbol, exchange = self.token(row), row["symbol"], row["exchange"]
        self.ad.subscribed_symbols.pop(f"{exchange}:{symbol}:{mode}", None)
        if not any(i["token"] == token for i in self.ad.subscribed_symbols.values()):
            self.ad.token_to_symbol.pop(token, None)

    def push(self, socket, frame):
        if isinstance(frame, bytes):
            self.ad._handle_ticks(self.ws._parse_binary_message(frame))
