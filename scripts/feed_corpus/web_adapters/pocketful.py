"""Pocketful: the adapter's own subscribe and unsubscribe (which build the
subscription the packet handlers look up and answer the acknowledgement),
its `_on_message` dispatch, the three packet handlers and the decoders in
`api/packet_decoder.py`.

The adapter is built as `test/test_pocketful_adapter_batch_watchdog.py`
builds it, but not connected (`connected = False`), so subscribe stores the
subscription and sends nothing, as it does before the socket opens; the
batching and the frames it would send are the desktop's to show. The
symbol master comes from the case in place of the database
(`SymbolMapper.get_token_from_symbol`), and the base adapter's response
helpers are `websocket_proxy/base_adapter.py`'s, written out.
"""

from collections import deque

from webkit import Glue, Source


class Pocketful(Glue):
    broker = "pocketful"
    sources = [
        Source(
            "broker/pocketful/api/packet_decoder.py",
            None,
            ["decodeSnapquoteData", "decodeDetailedMarketData", "decodeCompactMarketData"],
        ),
        Source("broker/pocketful/streaming/pocketful_mapping.py", "PocketfulExchangeMapper"),
        Source("broker/pocketful/streaming/pocketful_mapping.py", "PocketfulCapabilityRegistry"),
        Source(
            "broker/pocketful/streaming/pocketful_adapter.py",
            "PocketfulWebSocketAdapter",
            [
                "subscribe",
                "unsubscribe",
                "_on_message",
                "_handle_detailed_data",
                "_handle_compact_data",
                "_handle_snapquote_data",
                "_extract_depth_buy",
                "_extract_depth_sell",
                "_find_subscription",
                "_record_market_data",
                "_start_batch_timer_locked",
            ],
        ),
    ]

    def stubs(self):
        master = self.master

        class SymbolMapper:
            @staticmethod
            def get_token_from_symbol(symbol, exchange):
                row = master.by_symbol(symbol, exchange)
                if not row or not row["token"] or not row["brexchange"]:
                    return None
                return {"token": str(row["token"]), "brexchange": row["brexchange"]}

        return {"SymbolMapper": SymbolMapper}

    def start(self):
        self.ad = self.new(
            "PocketfulWebSocketAdapter",
            subscriptions={},
            subscription_queue={},
            connected=False,
            ws_client=None,
            batch_timer=None,
            _last_data_message_time=None,
            _data_watchdog_armed=False,
            _data_bucket_starts=deque(maxlen=3),
            publish_market_data=self.publish,
            _create_success_response=lambda message, **kw: {"status": "success", "message": message, **kw},
            _create_error_response=lambda code, message: {"status": "error", "code": code, "message": message},
        )

    def subscribe(self, row, mode, depth):
        self.ad.subscribe(row["symbol"], row["exchange"], mode, depth)

    def unsubscribe(self, row, mode, depth):
        self.ad.unsubscribe(row["symbol"], row["exchange"], mode)

    def push(self, socket, frame):
        self.ad._on_message(None, frame)
