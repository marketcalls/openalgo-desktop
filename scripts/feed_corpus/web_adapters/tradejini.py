"""Tradejini: NxtradStream's frame decoder and L1 cache, and the adapter's
subscribe, unsubscribe, connection and data handlers, with the state
test/test_tradejini_stream_lifecycle.py builds (`_bare_adapter`).

The adapter's feed sync runs on a debounce thread (`_request_feed_sync`);
the glue sends each sync at once through the adapter's own `_sync_feed`,
which is what the thread does after its quiet period. The stream's socket
is a stand-in that takes the requests, so `subscribeL1` prunes its L1 cache
as on the web. `SymbolMapper.get_token_from_symbol` reads the case's
symbol master in place of the database.

NxtradStream formats the trade time with the server's local time zone
(`datefmt`: `str(datetime.fromtimestamp(ltt))`); the glue fixes that zone
at IST, so web.json does not depend on the machine that runs it.
"""

import datetime as _dt
import threading

from webkit import Glue, Source

_IST = _dt.timezone(_dt.timedelta(hours=5, minutes=30))


class _ServerClock(_dt.datetime):
    """`datetime` whose local zone is IST."""

    @classmethod
    def fromtimestamp(cls, t, tz=None):
        return _dt.datetime.fromtimestamp(t, tz or _IST).replace(tzinfo=None)


class _Socket:
    """The stream's websocket-client app: takes what is sent."""

    def __init__(self):
        self.sent = []

    def send(self, data):
        self.sent.append(data)


class Tradejini(Glue):
    broker = "tradejini"
    sources = [
        Source(
            "broker/tradejini/api/nxtradstream.py",
            None,
            [
                "CURRENT_VERSION", "divide", "datefmt", "L1", "L5", "OHLC", "AUTH",
                "MARKET_STATUS", "EVENTS", "PING", "GREEKS", "SEG_INFO", "PKT_TYPE",
                "DEFAULT_PKT_INFO",
            ],
        ),
        Source(
            "broker/tradejini/api/nxtradstream.py",
            "NxtradStream",
            [
                "__send_data", "__frame_from_spec", "__format_values", "__ab2str",
                "__onsinglePacket", "__decodeL1PKT", "__decodeL2PKT", "__decodeOHLC",
                "__decodeMarketStatus", "__decodeMessage", "__decodeStatus",
                "__decompressZLib", "__on_message", "_callback", "subscribeL1",
                "subscribeL2", "unsubscribeL1", "unsubscribeL2",
            ],
        ),
        Source("broker/tradejini/streaming/tradejini_mapping.py", "TradejiniCapabilityRegistry"),
        Source(
            "websocket_proxy/base_adapter.py",
            "BaseBrokerWebSocketAdapter",
            ["_create_success_response", "_create_error_response"],
        ),
        Source(
            "broker/tradejini/streaming/tradejini_adapter.py",
            "TradejiniWebSocketAdapter",
            [
                "_feed_tokens", "_sync_feed", "subscribe", "unsubscribe", "_prune_tick_cache",
                "_on_connection_event", "_on_data", "_normalize_market_data",
                "_add_reference_keys", "_extract_depth_data",
            ],
        ),
    ]

    def stubs(self):
        master = self.master

        class SymbolMapper:
            @staticmethod
            def get_token_from_symbol(symbol, exchange):
                row = master.by_symbol(symbol, exchange)
                return {"token": str(row["token"]), "brexchange": row["brexchange"]} if row else None

        return {"SymbolMapper": SymbolMapper, "datetime": _ServerClock}

    def start(self):
        self.stream = self.new(
            "NxtradStream", ws=_Socket(), isConnected=True, L1_dict={}, connect_cb=None
        )
        base = self.web["BaseBrokerWebSocketAdapter"]
        self.ad = self.new(
            "TradejiniWebSocketAdapter",
            running=False,
            connected=True,
            reconnect_attempts=0,
            subscriptions={},
            _last_l1={},
            _last_l5={},
            _flush_stop=threading.Event(),
            ws_client=self.stream,
            zmq_port=0,
            publish_market_data=self.publish,
        )
        self.ad._create_success_response = base._create_success_response.__get__(self.ad)
        self.ad._create_error_response = base._create_error_response.__get__(self.ad)
        # The debounce thread's send, made at once.
        self.ad._request_feed_sync = lambda feed, cancel_when_empty=True: self.ad._sync_feed(
            feed, cancel_when_empty
        )
        self.stream.stream_cb = self.ad._on_data

    def subscribe(self, row, mode, depth):
        response = self.ad.subscribe(row["symbol"], row["exchange"], mode, depth)
        if response.get("status") != "success":
            raise SystemExit(f"the web refuses this subscribe: {response}")

    def unsubscribe(self, row, mode, depth):
        self.ad.unsubscribe(row["symbol"], row["exchange"], mode)

    def reconnect(self, socket):
        # The socket closed (its halves go with the session) and opened
        # again (both feeds replayed); `running` is off, so no retry thread.
        self.ad._on_connection_event(None, {"s": "closed", "reason": "dropped"})
        self.ad._on_connection_event(None, {"s": "connected"})

    def push(self, socket, frame):
        if isinstance(frame, bytes):
            self.stream._NxtradStream__on_message(None, frame)
