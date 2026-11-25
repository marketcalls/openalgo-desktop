"""Groww: the web client's NATS frame reader and protobuf decoder
(`GrowwNATSWebSocket._process_binary_nats_message` and
`_process_market_data_msg`, `groww_protobuf.parse_groww_market_data`) and
the adapter's merge cache and fan-out (`_GrowwMarketCache`,
`GrowwWebSocketAdapter._on_data`, `_normalize_market_data`,
`_maybe_clear_cache_entry`), with the state test/test_groww_quote_feed.py
builds.

The subscription state is what `GrowwWebSocketAdapter.subscribe`
(groww_adapter.py:392-528) and `GrowwNATSWebSocket.subscribe_batch`
(nats_websocket.py:774-858) keep: the adapter's entry per symbol, exchange
and mode (Depth on an index matched as LTP), a shadow LTP entry beside a
Depth one, and the client's entry per NATS subject.

SIDs: a NATS sid is the number the client gives a subject, and the
server's MSG names it. The glue numbers subjects as both sides do (per
connection from 1, a Depth subscription's book subject before its shadow
price subject: nats_websocket.py:59-63, groww_adapter.py:478-538), so the
one MSG frame a case pushes names the same subject on both sides.

A dropped socket is the web client's own reconnect
(nats_websocket.py:271-328, `_resubscribe_all`): new sids, and the adapter
is not told, so its merge cache keeps the old session's values.

Order updates: the web's order adapter on Groww's order subjects
(`groww_order_adapter.py`, openalgo#2474): its NATS frame reader
(`GrowwOrderUpdateAdapter._split_frames`, with no socket, so the handshake
and the PONG it would write are not modelled; the case pins what the feed
sends), the protobuf decoder (`decode_order_update`) and its `normalize`,
which reads the order's detail (`_order_detail`, through `groww_request`)
from the fake broker API and maps it (`_fields_from`). Every answer the
case serves (case.json `http`, then each `http_serve`) is what that API
answers from then on (`GrowwRest`; a served answer is 200, its body says
whether Groww gave the order). The adapter's `_handle_message` reaches the
base class's through `super()`, which a lifted class has no base for, so
the glue runs the same steps: split, normalise each payload (a payload
whose normalising raises is dropped), relay what comes back. A reconnect
resets the reader as `get_ws_url` does. Stand-ins: the httpx client
(`GrowwRest`), the master lookups (`get_oa_symbol`, `to_openalgo_symbol`),
and the backpressure ceiling (none, as under eventlet and the development
server: `check_queue_wait` and `cap_server_delay` let every wait through).
"""

import email.utils
import json
import types
import urllib.parse

from webkit import Glue, Source

PROTOBUF_NAMES = [
    "VARINT", "FIXED64", "LENGTH_DELIMITED", "FIXED32",
    "FIELD_SYMBOL", "FIELD_SEGMENT", "FIELD_EXCHANGE", "FIELD_STOCK_LIVE_PRICE",
    "FIELD_MARKET_DEPTH", "FIELD_LIVE_INDICES", "FIELD_TS_IN_MILLIS", "FIELD_OPEN",
    "FIELD_HIGH", "FIELD_LOW", "FIELD_CLOSE", "FIELD_VOLUME", "FIELD_VALUE", "FIELD_LTP",
    "parse_groww_market_data",
]

ORDER_NAMES = ["GROWW_ORDER_STATUS_MAP", "GROWW_PRICETYPE_MAP", "map_order_status"]
RATE_LIMIT_NAMES = [
    "MIN_INTERVAL", "MAX_RETRIES", "BASE_BACKOFF", "_lock", "_last_call_time", "_queue_kind",
    "apply_rate_limit", "retry_delay", "groww_request",
]
ORDER_ADAPTER_NAMES = [
    "GROWW_ORDER_DETAIL_URL", "_ORDER_STATUS", "_EXCHANGE", "_SEGMENT", "_PRODUCT", "_ORDER_TYPE",
    "_BUY_SELL", "_ORDER_FIELDS", "_read_varint", "_fields", "decode_order_update",
]


class GrowwRest:
    """The httpx client `groww_request` calls (`request`), answering from
    the case's fake broker API as the harness does: a path with a query
    answers exactly that path and query, a path without one any query, and
    a path nothing answers is a 404 with no body."""

    def __init__(self, answers):
        self.answers = answers

    def request(self, method, url, params=None, **_):
        path = urllib.parse.urlsplit(url).path
        query = urllib.parse.urlencode(params or {})
        body = self.answers.get(f"{path}?{query}" if query else path, self.answers.get(path))
        return GrowwReply(404, "") if body is None else GrowwReply(200, body)


class GrowwReply:
    """An httpx response as `groww_request` and `_order_detail` read it."""

    def __init__(self, status_code, text):
        self.status_code = status_code
        self.text = text
        self.headers = {}

    def json(self):
        return json.loads(self.text)


class Groww(Glue):
    broker = "groww"
    sources = [
        Source("broker/groww/streaming/groww_protobuf.py", None, PROTOBUF_NAMES),
        Source("broker/groww/streaming/groww_protobuf.py", "MiniProtobufParser"),
        Source("broker/groww/streaming/groww_mapping.py", "GrowwExchangeMapper"),
        Source(
            "broker/groww/streaming/nats_websocket.py",
            "GrowwNATSWebSocket",
            ["_process_binary_nats_message", "_process_nats_message", "_process_market_data_msg"],
        ),
        Source("broker/groww/streaming/groww_adapter.py", "_GrowwMarketCache"),
        Source(
            "broker/groww/streaming/groww_adapter.py",
            "GrowwWebSocketAdapter",
            ["_on_data", "_normalize_market_data", "_maybe_clear_cache_entry"],
        ),
        Source("broker/groww/mapping/order_data.py", None, ORDER_NAMES),
        Source(
            "broker/groww/mapping/transform_data.py",
            None,
            ["EXCHANGE_NSE", "EXCHANGE_BSE", "SEGMENT_CASH", "SEGMENT_FNO", "openalgo_exchange"],
        ),
        Source("broker/groww/api/rate_limiter.py", None, RATE_LIMIT_NAMES),
        Source("broker/groww/streaming/groww_order_adapter.py", None, ORDER_ADAPTER_NAMES),
        Source(
            "broker/groww/streaming/groww_order_adapter.py",
            "GrowwOrderUpdateAdapter",
            ["_reset_connection_state", "_split_frames", "_order_detail", "_fields_from", "normalize"],
        ),
    ]

    def stubs(self):
        def get_oa_symbol(brsymbol, exchange):
            row = self.master.by_brsymbol(brsymbol, exchange)
            return row["symbol"] if row else None

        return {
            # nats_websocket.py imports the decoder as a module (`from . import
            # groww_protobuf`); its one call goes to the lifted function.
            "groww_protobuf": types.SimpleNamespace(
                parse_groww_market_data=lambda data: self.web["parse_groww_market_data"](data)
            ),
            "get_oa_symbol": get_oa_symbol,
            "to_openalgo_symbol": self.to_openalgo_symbol,
            "get_httpx_client": lambda: GrowwRest(self.answers),
            "check_queue_wait": lambda wait_seconds, kind="data": None,
            "cap_server_delay": lambda seconds, kind="data": seconds,
            "parsedate_to_datetime": email.utils.parsedate_to_datetime,
        }

    def start(self):
        self.ad = self.new(
            "GrowwWebSocketAdapter",
            subscriptions={},
            shadow_correlation_ids={},
            market_cache=self.web["_GrowwMarketCache"](),
            publish_market_data=self.publish,
        )
        self.ws = self.new(
            "GrowwNATSWebSocket",
            subscriptions={},
            nats_sids={},
            on_data=self.ad._on_data,
            # Checked to exist (nats_websocket.py:433); a MSG frame is read
            # without it.
            nats_protocol=object(),
        )
        # Per instrument, in subscribe order: its price and book subject keys.
        self.subjects = {}
        self.next_sid = 1
        # What the fake broker API answers, by path (with its query).
        self.answers = {}
        # The order adapter, with no socket: its reader writes nothing.
        self.orders = self.new(
            "GrowwOrderUpdateAdapter", broker_name="groww", user_id="<USER_ID>", auth_token="t", _ws=None
        )
        self.orders._reset_connection_state()

    def subscribe(self, row, mode, depth):
        symbol, exchange = row["symbol"], row["exchange"]
        instrumenttype = row["instrumenttype"]
        token = symbol if exchange == "NSE_INDEX" else str(row["token"])
        groww_exchange, segment = self.web["GrowwExchangeMapper"].get_exchange_segment(exchange)
        cid = f"{symbol}_{exchange}_{mode}"
        entry = {
            "symbol": symbol, "exchange": exchange, "groww_exchange": groww_exchange,
            "segment": segment, "brexchange": row["brexchange"], "token": token, "mode": mode,
            "depth_level": 5, "instrumenttype": instrumenttype,
        }
        self.ad.subscriptions[cid] = entry
        is_index = instrumenttype == "INDEX" or "INDEX" in exchange
        client = {
            "symbol": symbol, "exchange": exchange, "groww_exchange": groww_exchange,
            "segment": segment, "exchange_token": token, "instrumenttype": instrumenttype,
        }
        price_key = f"ltp_{groww_exchange}_{segment}_{token}"
        book_key = None
        if mode == 3 and is_index:
            entry["mode"] = 1  # groww_adapter.py:458-466
        elif mode == 3:
            shadow = f"_shadow_ltp_{cid}"
            self.ad.shadow_correlation_ids[cid] = shadow
            self.ad.subscriptions[shadow] = dict(
                entry, mode=1, depth_level=0, is_shadow=True, primary_correlation_id=cid
            )
            price_key, book_key = shadow, f"depth_{groww_exchange}_{segment}_{token}"
            self.ws.subscriptions[book_key] = dict(client, mode="depth", numeric_mode=3)
        self.ws.subscriptions[price_key] = dict(
            client, mode="index" if is_index else "ltp", numeric_mode=1
        )
        self.subjects[cid] = (price_key, book_key)
        self.number(price_key, book_key)

    def number(self, price_key, book_key):
        for key in (book_key, price_key):
            if key is not None:
                self.ws.nats_sids[key] = str(self.next_sid)
                self.next_sid += 1

    def unsubscribe(self, row, mode, depth):
        cid = f"{row['symbol']}_{row['exchange']}_{mode}"
        entry = self.ad.subscriptions.pop(cid, None)
        shadow = self.ad.shadow_correlation_ids.pop(cid, None)
        if shadow is not None:
            self.ad.subscriptions.pop(shadow, None)
        for key in self.subjects.pop(cid, (None, None)):
            if key is not None:
                self.ws.subscriptions.pop(key, None)
                self.ws.nats_sids.pop(key, None)
        if entry:
            self.ad._maybe_clear_cache_entry(entry)

    def reconnect(self, socket):
        if socket == "orders":
            # get_ws_url: a fresh socket token and reader for every connect.
            self.orders._reset_connection_state()
            return
        self.ws.nats_sids.clear()
        self.next_sid = 1
        for price_key, book_key in self.subjects.values():
            self.number(price_key, book_key)

    def push(self, socket, frame):
        if socket == "orders":
            self.push_orders(frame.encode("utf-8") if isinstance(frame, str) else frame)
            return
        # Only a MSG carries market data; INFO, +OK and PING publish nothing.
        if isinstance(frame, bytes) and b"MSG" in frame[:50]:
            self.ws._process_binary_nats_message(frame)

    def push_orders(self, data):
        """`GrowwOrderUpdateAdapter._handle_message` and the base class's:
        a frame the reader cannot read empties its buffer; each payload is
        normalised, and one that raises or gives nothing is dropped."""
        try:
            payloads = self.orders._split_frames(bytes(data))
        except Exception:  # noqa: BLE001 (the web catches every error here)
            self.orders._buffer = b""
            return
        for payload in payloads:
            try:
                fields = self.orders.normalize(payload)
            except Exception:  # noqa: BLE001 (as BaseOrderUpdateAdapter._handle_message)
                continue
            if fields:
                self.order(fields)

    def serve(self, method, path, body):
        """The fake broker API answers `path` (with its query) with `body`
        from now on: the order detail the next order push reads."""
        self.answers[path] = body
