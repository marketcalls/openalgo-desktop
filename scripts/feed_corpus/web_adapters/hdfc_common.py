"""HDFC Sky and HDFC Securities (InvestRight): sibling feeds with the same
protobuf schema and the same parser and adapter shape, in
`broker/hdfcsky/streaming/` and `broker/hdfcsecurities/streaming/`.

What runs is the web's own code: the websocket client's `_parse_frame` and
`_parse_packet` with its packet-type tables, and the adapter's `subscribe`,
`unsubscribe`, `_on_ticks`, `_merge_partial` and `_normalize`, with the
state test/test_hdfcsky_quote_feed.py builds (`_adapter()`). The protobuf
classes are read from the web's `*_pb2.py` descriptor (`protolite`), since
the corpus tools do not install the protobuf package. The adapter's feed
client is a stand-in that accepts every subscription; the token comes from
the case's symbol master in place of `get_token`. A drop of the upstream
socket is the client's own reconnect: `_on_ws_disconnect` then
`_on_ws_connect`, as test_reconnect_drops_snapshots_before_partial_packets
drives it. Text frames (acks, errors) publish nothing on the web
(`_on_ws_message`).
"""

from __future__ import annotations

import threading
from pathlib import Path

import protolite
from webkit import Glue, Source

# `mapping/transform_data.py::to_ws_prefix`.
WS_PREFIX = {
    "NSE": "NSE", "BSE": "BSE", "NFO": "NFO", "BFO": "BFO", "CDS": "NCD", "MCX": "MCX",
    "NSE_INDEX": "NSE_INDEX", "BSE_INDEX": "BSE_INDEX",
}

PACKET_TABLES = [
    "_QUOTE_PACKET_TYPES", "_CIRCUIT_PACKET_TYPES", "_OI_PACKET_TYPES", "_INDEX_PACKET_TYPES",
    "_GREEK_PACKET_TYPES", "_PACKET_TYPE_EXCHANGE",
]
ADAPTER_METHODS = [
    "_store_info", "subscribe", "unsubscribe", "_subscription_key", "_on_ticks", "_merge_partial",
    "_normalize", "_on_ws_connect", "_on_ws_disconnect",
]


class FeedClient:
    """The adapter's websocket client: every subscription is accepted."""

    def subscribe_scrips(self, scrip_ids, subscription_type="ALL"):
        return True

    def unsubscribe(self, scrip_ids):
        return True


def sources(name: str, registry: str, client: str, adapter: str, extra: list[str]) -> list[Source]:
    base = f"broker/{name}/streaming/{name}"
    return [
        Source(f"{base}_mapping.py", registry),
        Source(f"{base}_websocket.py", None, PACKET_TABLES),
        Source(f"{base}_websocket.py", client, ["_parse_frame", "_parse_packet"]),
        Source(f"{base}_adapter.py", None, ["_MODE_TO_TOPIC", "_PARTIAL_TICK_FIELDS"]),
        Source(f"{base}_adapter.py", adapter, ADAPTER_METHODS + extra),
    ]


class HdfcGlue(Glue):
    """Subclasses set the web names and the adapter's extra state."""

    pb2 = ""
    client = ""
    adapter = ""
    adapter_state: dict = {}

    def stubs(self):
        pb = protolite.module(Path(self.web_root) / self.pb2)
        return {
            "pb": pb,
            "DecodeError": pb.DecodeError,
            "ws_scrip_id": lambda exchange, token: f"{WS_PREFIX[exchange]}_{token}",
        }

    def start(self):
        def token_of(symbol, exchange):
            row = self.master.by_symbol(symbol, exchange)
            return int(row["token"]) if row else None

        self.ws = self.new(self.client)
        self.ad = self.new(
            self.adapter,
            token_info={},
            last_snapshot={},
            _state_lock=threading.Lock(),
            ws_client=FeedClient(),
            connected=True,
            publish_market_data=self.publish,
            _resolve_token=token_of,
            _create_success_response=lambda message, **kw: {"status": "success", "message": message, **kw},
            _create_error_response=lambda code, message: {"status": "error", "code": code, "message": message},
            **self.adapter_state,
        )

    def subscribe(self, row, mode, depth):
        r = self.ad.subscribe(row["symbol"], row["exchange"], mode, depth)
        if r.get("status") != "success":
            raise SystemExit(f"the web adapter refused {row['symbol']}: {r}")

    def unsubscribe(self, row, mode, depth):
        self.ad.unsubscribe(row["symbol"], row["exchange"], mode)

    def reconnect(self, socket):
        self.ad._on_ws_disconnect()
        self.ad._on_ws_connect()

    def push(self, socket, frame):
        if isinstance(frame, bytes):
            ticks = self.ws._parse_frame(frame)
            if ticks:
                self.ad._on_ticks(ticks)
