"""Kotak: the SFeed client (sfeed_protocol.py decodes, sfeed_websocket.py
normalises) feeding the adapter's merge and publish
(kotak_adapter.py::_on_data_received), and the realtime order socket's
normaliser, with the state test/test_kotak_quote_feed.py builds.

The adapter's subscribe paths import `get_kotak_exchange` and the
database's `get_token` inside the method; while the glue calls them, both
are served from the case (the web's own `get_kotak_exchange`, the master's
token) by stand-in modules that are removed afterwards. Its batch queue is
not run: what a subscribe sends Kotak is the desktop's to check.
"""

import contextlib
import sys
import threading
import types

from webkit import Glue, Source

PROTOCOL = "broker/kotak/streaming/sfeed_protocol.py"
CLIENT = "broker/kotak/streaming/sfeed_websocket.py"
ADAPTER = "broker/kotak/streaming/kotak_adapter.py"
ORDERS = "broker/kotak/streaming/kotak_order_adapter.py"


class Kotak(Glue):
    broker = "kotak"
    sources = [
        Source(PROTOCOL, None, [
            "HEADER_SIZE", "DEFAULT_DIVIDER", "MSG_AUTH_RESPONSE_CODES", "MSG_SUBSCRIBE_ACK",
            "MSG_MARKET_OPEN", "MSG_MARKET_CLOSE", "MSG_INDEX", "MSG_MARKET_PICTURE",
            "MSG_MARKET_STATUS", "MSG_CAS_CHANGE", "LEVEL_MINI_TOUCH_LINE", "LEVEL_TOUCH_LINE",
            "LEVEL_DEPTH", "LEVEL_FULL_DEPTH", "EXCHANGE_NAME_TO_ID", "EXCHANGE_ID_TO_NAME",
            "MARKET_STATUS_TEXT", "_HEADER", "_U16", "_INDEX_BODY", "_MINI_BODY", "_MP_BODY",
            "_MP_FIXED_END", "_DEPTH_ROW", "_DEPTH_ROW_SIZE", "_MARKET_STATUS_BODY",
            "_CAS_CHANGE_BODY", "split_batch", "_decode_string", "decode_packet", "_decode_index",
            "_decode_market_status", "_decode_cas_change", "_decode_mini", "_decode_market_picture",
        ]),
        Source(CLIENT, None, [
            "_SUBSCRIBE_EVENTS", "_UNSUBSCRIBE_EVENTS", "_UNSUBSCRIBE_CLEARS", "MAX_SUBSCRIPTIONS",
            "AUTH_TIMEOUT_SECONDS", "MAX_PENDING_FRAMES",
        ]),
        Source(CLIENT, "KotakSFeedWebSocket", [
            "set_callbacks", "_handle_text", "_handle_auth_response", "_handle_subscribe_ack",
            "_handle_binary", "_dispatch", "_symbol_for", "_trade_fields", "_to_quote",
            "_to_quote_lite", "_to_depth", "_depth_rows", "_to_index",
        ]),
        Source("broker/kotak/streaming/kotak_mapping.py", None,
               ["OPENALGO_TO_KOTAK_EXCHANGE", "get_kotak_exchange"]),
        Source(ADAPTER, None, [
            "_INDEX_NAMES", "index_name_candidates", "is_index_exchange", "_QUOTE_EXTRA_FIELDS",
            "_KNOWN_QUOTE_FIELDS", "_quote_extras", "_ltt_millis",
        ]),
        Source(ADAPTER, "KotakWebSocketAdapter", [
            "_index_targets_for", "_alias_registry", "_on_data_received", "_is_partial_update",
            "_index_feed_names", "_drop_index_alias_target", "subscribe", "unsubscribe",
            "subscribe_quote", "unsubscribe_quote", "subscribe_depth", "unsubscribe_depth",
        ]),
        Source("broker/kotak/mapping/transform_data.py", None, ["map_exchange"]),
        Source(ORDERS, None, ["_STATUS_MAP", "_PRICETYPE_MAP", "_ACTION_MAP", "_num"]),
        Source(ORDERS, "KotakOrderUpdateAdapter", ["normalize"]),
    ]

    def stubs(self):
        return {"to_openalgo_symbol": self.to_openalgo_symbol}

    @contextlib.contextmanager
    def lookups(self):
        """`get_kotak_exchange` and `get_token` for the adapter's in-method
        imports, from the case, for the length of one call."""
        mapping = types.ModuleType("broker.kotak.streaming.kotak_mapping")
        mapping.get_kotak_exchange = self.web["get_kotak_exchange"]
        token_db = types.ModuleType("database.token_db")
        token_db.get_token = lambda symbol, exchange: (
            self.master.by_symbol(symbol, exchange) or {}
        ).get("token")
        added = {"broker.kotak.streaming.kotak_mapping": mapping, "database.token_db": token_db}
        before = {k: sys.modules.get(k) for k in added}
        sys.modules.update(added)
        try:
            yield
        finally:
            for k, v in before.items():
                if v is None:
                    sys.modules.pop(k, None)
                else:
                    sys.modules[k] = v

    def client(self):
        """A fresh SFeed client, open and not yet authenticated, as each
        connection's client starts (kotak_adapter._recreate_ws_client)."""
        c = self.new(
            "KotakSFeedWebSocket",
            auth_config={"sid": "s"}, ws_url="", ucc="U",
            _lock=threading.RLock(), _ws=None, _should_run=True, _is_connected=True,
            _is_authenticated=False, _auth_event=threading.Event(), _dividers={}, _symbols={},
            _subscriptions=set(), _pending_frames=[],
        )
        c.set_callbacks(on_quote=self.ad._on_data_received, on_depth=self.ad._on_data_received)
        return c

    def start(self):
        self.ad = self.new(
            "KotakWebSocketAdapter",
            _lock=threading.RLock(), _symbol_state={}, _ltp_cache={}, _quote_cache={},
            _depth_cache={}, _depth_poll_state={}, _kotak_to_openalgo={}, _symbol_modes={},
            _index_alias_targets={}, subscriptions={}, _subscription_queue=[],
            publish_market_data=self.publish,
            _enqueue_subscription=lambda *a, **k: None,
            _create_success_response=lambda message, **k: {"status": "success", "message": message},
            _create_error_response=lambda code, message: {"status": "error", "code": code},
        )
        self.ad._ws_client = self.client()
        self.orders = self.new(
            "KotakOrderUpdateAdapter", _use_json_handshake=True, _connect_attempted=True,
            _connect_acked=False,
        )

    def subscribe(self, row, mode, depth):
        with self.lookups():
            self.ad.subscribe(row["symbol"], row["exchange"], mode, depth)

    def unsubscribe(self, row, mode, depth):
        with self.lookups():
            self.ad.unsubscribe(row["symbol"], row["exchange"], mode)

    def reconnect(self, socket):
        """kotak_adapter._attempt_reconnection: a new client, the merged
        packet state cleared (its quote, LTP and depth caches are kept),
        every subscription made again."""
        if socket != "main":
            return
        saved = list(self.ad.subscriptions.values())
        self.ad._ws_client = self.client()
        self.ad._symbol_state.clear()
        with self.lookups():
            for sub in saved:
                self.ad.subscribe(sub["symbol"], sub["exchange"], sub["mode"])

    def push(self, socket, frame):
        if socket == "orders":
            fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
        elif isinstance(frame, bytes):
            self.ad._ws_client._handle_binary(frame)
        else:
            self.ad._ws_client._handle_text(frame)
