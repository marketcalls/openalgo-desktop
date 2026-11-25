"""Pocketful (rebuilt on Pocketful's new API, web a1d539764): the market
adapter's own subscribe and unsubscribe (which build the routes its packet
handlers read and answer the acknowledgement), its `_on_message` dispatch,
the three packet handlers and the decoders in `api/packet_decoder.py`; and
the order adapter's `_handle_message` and `normalize` with the frame and
record helpers of `streaming/pocketful_order_adapter.py`.

The market adapter is built as `test/test_pocketful_streaming.py` builds it,
but not connected (`connected = False`), so subscribe stores the
subscription and its routes and sends nothing, as it does before the socket
opens; the batching and the frames it would send are the desktop's to show.
A reconnect (`_on_open` of a new socket) starts with no cached Depth price.
The symbol master comes from the case in place of the database
(`SymbolMapper.get_token_from_symbol`), and the base adapter's response
helpers are `websocket_proxy/base_adapter.py`'s, written out. The order
adapter is built as `test/test_pocketful_order_updates.py` builds it, its
symbol lookup (`to_openalgo_symbol`) on the case's master.
"""

import types
from collections import deque

from webkit import Glue, Source

ADAPTER = "broker/pocketful/streaming/pocketful_adapter.py"
ORDERS = "broker/pocketful/streaming/pocketful_order_adapter.py"


class Pocketful(Glue):
    broker = "pocketful"
    sources = [
        Source(
            "broker/pocketful/api/packet_decoder.py",
            None,
            [
                "DETAILED_MODE",
                "COMPACT_MODE",
                "SNAPQUOTE_MODE",
                "CHANNEL_NAMES",
                "_DETAILED",
                "_DETAILED_FIELDS",
                "_COMPACT",
                "_COMPACT_FIELDS",
                "_SNAPQUOTE",
                "_SNAPQUOTE_LEVELS",
                "packet_mode",
                "_fits",
                "decodeDetailedMarketData",
                "decodeCompactMarketData",
                "decodeSnapquoteData",
            ],
        ),
        Source("broker/pocketful/streaming/pocketful_mapping.py", "PocketfulExchangeMapper"),
        Source(ADAPTER, None, ["MODE_CHANNELS", "MODE_LABELS", "PRICE_DIVISOR", "DEPTH_LEVELS", "_price", "_now_ms"]),
        Source(
            ADAPTER,
            "PocketfulWebSocketAdapter",
            [
                "subscribe",
                "unsubscribe",
                "_channels_of",
                "_needed_channels_locked",
                "_release_channels_locked",
                "_publish_routes_locked",
                "_on_message",
                "_note_ltp",
                "_handle_compact_data",
                "_handle_detailed_data",
                "_handle_snapquote_data",
                "_levels",
                "_record_market_data",
            ],
        ),
        Source(
            ORDERS,
            None,
            [
                "ORDER_UPDATE_MODE",
                "TRADE_UPDATE_MODE",
                "UPDATE_JSON_OFFSET",
                "_STATUS_MAP",
                "_PRICETYPE_MAP",
                "_PRODUCT_MAP",
                "_text",
                "_int",
                "_float",
                "map_order_status",
                "decode_frame",
                "order_records",
            ],
        ),
        Source(ORDERS, "PocketfulOrderUpdateAdapter", ["_handle_message", "normalize"]),
    ]

    def stubs(self):
        master = self.master

        class SymbolMapper:
            @staticmethod
            def get_token_from_symbol(symbol, exchange):
                row = master.by_symbol(symbol, exchange)
                if not row or not row["token"]:
                    return None
                return {"token": str(row["token"]), "brexchange": row.get("brexchange") or exchange}

        # `types` (MappingProxyType, the adapter's route table) is standard
        # library but not on webkit's list of imports it runs.
        return {"SymbolMapper": SymbolMapper, "to_openalgo_symbol": self.to_openalgo_symbol, "types": types}

    def start(self):
        self.ad = self.new(
            "PocketfulWebSocketAdapter",
            subscriptions={},
            subscription_queue={},
            connected=False,
            ws_client=None,
            batch_timer=None,
            _broker_channels=set(),
            _inflight={},
            _last_ltp={},
            _last_data_message_time=None,
            _data_watchdog_armed=False,
            _data_bucket_starts=deque(maxlen=3),
            publish_market_data=self.publish,
            _create_success_response=lambda message, **kw: {"status": "success", "message": message, **kw},
            _create_error_response=lambda code, message: {"status": "error", "code": code, "message": message},
        )
        self.orders = self.new(
            "PocketfulOrderUpdateAdapter",
            client_id="c",
            _publish_event_fields=self.order,
        )

    def subscribe(self, row, mode, depth):
        self.ad.subscribe(row["symbol"], row["exchange"], mode, depth)

    def unsubscribe(self, row, mode, depth):
        self.ad.unsubscribe(row["symbol"], row["exchange"], mode)

    def reconnect(self, socket):
        # `_on_open` of the new socket: nothing subscribed at the broker yet,
        # and no cached Depth price.
        if socket == "main":
            self.ad._socket_generation += 1
            self.ad._broker_channels = set()
            self.ad._inflight = {}
            self.ad._last_ltp = {}

    def push(self, socket, frame):
        if socket == "orders":
            self.orders._handle_message(frame)
        else:
            self.ad._on_message(None, frame)
