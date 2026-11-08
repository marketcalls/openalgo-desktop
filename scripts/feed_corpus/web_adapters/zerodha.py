"""Zerodha: the Kite ticker parser and the adapter's tick fan-out, and the
order adapter's postback normaliser.

State as `ZerodhaWebSocketAdapter.subscribe` keeps it (zerodha_adapter.py:
324-331) and as test/test_zerodha_index_packets.py builds the parser.
`to_openalgo_symbol` and the master contract's MCX lot size come from the
case's symbol master in place of the database.
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
        Source("broker/zerodha/mapping/order_data.py", None, ["ZERODHA_ORDER_STATUS_MAP", "map_order_status"]),
        Source(
            "broker/zerodha/mapping/mcx_contract_size.py",
            None,
            [
                "MCX_CONTRACT_SIZES",
                "MCX_SIZE_REVISIONS",
                "_ROOTS_LONGEST_FIRST",
                "units_per_contract",
                "_resolve_size",
                "from_kite_quantity",
            ],
        ),
        Source("broker/zerodha/streaming/zerodha_order_adapter.py", None, ["_units"]),
        Source("broker/zerodha/streaming/zerodha_order_adapter.py", "ZerodhaOrderUpdateAdapter", ["normalize"]),
    ]

    def stubs(self):
        def master_lot_size(symbol, exchange):
            row = self.master.by_symbol(symbol, exchange) or self.master.by_brsymbol(symbol, exchange)
            return int(row["lotsize"]) if row and int(row["lotsize"]) > 0 else None

        return {"to_openalgo_symbol": self.to_openalgo_symbol, "_master_contract_lot_size": master_lot_size}

    def start(self):
        self.orders = self.new("ZerodhaOrderUpdateAdapter")
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
        else:
            fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
