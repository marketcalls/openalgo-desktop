"""Samco: the adapter's subscribe, unsubscribe and tick fan-out, and the
socket client's frame handling (`_on_message`: unwrap, merge per symbol,
normalise), with the state test/test_samco_quote_feed.py builds; and the
order updates the web's poller makes of each order book (`polling.py`,
over Samco's `broker/samco/mapping/order_data.py`).

The adapter's own `subscribe` and `unsubscribe` run, down to the client's
subscription book and the frames it would send (a recording socket), so
the web's subscription quirks show as they are. Lookups the web makes
against the database and Samco's REST API are stand-ins: `SymbolMapper`
and `get_oa_symbol` read the case's symbol master, and the index streaming
ids `_get_index_listing_id` fetches from `/quote/indexQuote` are the ones
in LISTING_IDS. A dropped socket runs the client's
`_cleanup_connection_state` (the merged fields go with the connection).
"""

import threading

from web_adapters.polling import POLLING_SOURCES, Polling
from webkit import Glue, Source

# What Samco's /quote/indexQuote answers for each index (listingId).
LISTING_IDS = {"NIFTY_NSE_INDEX": "-23", "BANKNIFTY_NSE_INDEX": "-25"}


class Samco(Polling, Glue):
    broker = "samco"
    order_book_path = "/order/orderBook"
    empty_book = '{"status": "Success", "orderBookDetails": []}'
    sources = [
        Source("broker/samco/streaming/samco_mapping.py", "SamcoExchangeMapper"),
        Source("broker/samco/streaming/samco_mapping.py", "SamcoCapabilityRegistry"),
        Source(
            "broker/samco/streaming/samcoWebSocket.py",
            "SamcoWebSocket",
            [
                "subscribe",
                "unsubscribe",
                "_unsubscribe_locked",
                "_validate_connection_state",
                "_request_subscription_flush",
                "_on_message",
                "_update_last_message_time",
                "_unwrap_tick",
                "_normalize_market_data",
                "_first_present",
                "_parse_trade_time",
                "_safe_float",
                "_safe_int",
                "_cleanup_connection_state",
                "_stop_heartbeat",
                "_stop_subscription_flusher",
            ],
        ),
        Source(
            "broker/samco/streaming/samco_adapter.py",
            "SamcoWebSocketAdapter",
            ["subscribe", "unsubscribe", "_get_index_listing_id", "_on_data", "_normalize_market_data"],
        ),
        Source(
            "broker/samco/mapping/order_data.py",
            None,
            ["_PLACEHOLDERS", "_clean_text", "_to_int", "_to_float", "map_order_status", "map_order_data",
             "calculate_order_statistics", "transform_order_data"],
        ),
        *POLLING_SOURCES,
    ]

    def stubs(self):
        master = self.master

        class SymbolMapper:
            @staticmethod
            def get_token_from_symbol(symbol, exchange):
                row = master.by_symbol(symbol, exchange)
                return {"token": row["token"], "brexchange": row["brexchange"]} if row else None

        def get_oa_symbol(brsymbol, exchange):
            row = master.by_brsymbol(brsymbol, exchange)
            return row["symbol"] if row else None

        return {**self.polling_stubs(), "SymbolMapper": SymbolMapper, "get_oa_symbol": get_oa_symbol}

    def start(self):
        self.sent = []
        self.ws = self.new(
            "SamcoWebSocket",
            ws=type("Socket", (), {"send": lambda _, frame: self.sent.append(frame)})(),
            connected=True,
            running=True,
            subscribed_symbols={},
            input_request_dict={},
            RESUBSCRIBE_FLAG=False,
            _tick_state={},
            _tick_state_lock=threading.Lock(),
            _sub_dirty=threading.Event(),
            _sub_send_lock=threading.Lock(),
            _sub_flush_stop=threading.Event(),
            _heartbeat_stop_event=threading.Event(),
            _heartbeat_lock=threading.Lock(),
            _last_message_time=None,
            _on_message_callback=None,
        )
        self.ad = self.new(
            "SamcoWebSocketAdapter",
            ws_client=self.ws,
            connected=True,
            subscriptions={},
            _index_listing_cache=dict(LISTING_IDS),
            _create_success_response=lambda message, **kw: {"status": "success", "message": message, **kw},
            _create_error_response=lambda code, message: {"status": "error", "code": code, "message": message},
            publish_market_data=self.publish,
        )
        self.ws._on_data_callback = self.ad._on_data

    def subscribe(self, row, mode, depth):
        reply = self.ad.subscribe(row["symbol"], row["exchange"], mode, depth)
        if reply["status"] != "success":
            raise SystemExit(f"the web refuses {row['symbol']}: {reply}")

    def unsubscribe(self, row, mode, depth):
        self.ad.unsubscribe(row["symbol"], row["exchange"], mode)

    def reconnect(self, socket):
        self.ws._cleanup_connection_state()
        self.ws.connected = True  # SamcoWebSocket._on_open on the new socket

    def push(self, socket, frame):
        self.ws._on_message(None, frame.decode("utf-8") if isinstance(frame, bytes) else frame)
