"""Sahi: the protobuf decoder, the tick builders and the adapter's tick
path, with the subscription state test/test_sahi_streaming_codec.py's
`adapter` fixture builds (`_subs` of `_Sub` by instrument id).

`sahi_mapping.py` and `sahi_adapter.py` reach the codec as
`from . import sahi_proto as pb`, a package import the lift does not run:
the codec is lifted first, on its own, and handed to them as `pb`.

Order updates: the web's order adapter on `/v1/stream/orders`
(`sahi_order_adapter.py`, openalgo#2474): its `normalize` and
`normalize_order`, through the order book's own mapping
(`mapping/order_data.py` `map_order_data`, with `transform_data.py`'s
reverse maps and `from_paise`). The base class's `_handle_message` is run
as its steps: normalise (a frame whose normalising raises is dropped),
relay what comes back. Stand-in: the master lookup (`get_oa_symbol`).
"""

import types

from webkit import Glue, Source, load

PROTO = [
    Source(
        "broker/sahi/streaming/sahi_proto.py",
        None,
        [
            "MODE_INVALID", "MODE_LTP", "MODE_QUOTE", "MODE_DEPTH",
            "SEGMENT_NSE", "SEGMENT_BSE", "SEGMENT_NFO", "SEGMENT_BFO",
            "ACK_SUBSCRIBE", "ACK_UNSUBSCRIBE", "ACK_MODE_CHANGE",
            "KIND_ACK", "KIND_LTP", "KIND_QUOTE", "KIND_DEPTH", "KIND_ERROR",
            "_WT_VARINT", "_WT_I64", "_WT_LEN", "_WT_I32", "_U64", "_I64_SIGN",
            "ProtoDecodeError", "_read_varint", "_to_int64", "_skip", "_read_len",
            "_read_repeated_int64", "_decode_depth_level", "_decode_tick", "_TICK_FIELDS",
            "_decode_ack", "_decode_error", "_PAYLOAD_DECODERS", "decode_server_message",
        ],
    )
]


class Sahi(Glue):
    broker = "sahi"
    sources = [
        Source(
            "broker/sahi/streaming/sahi_mapping.py",
            None,
            [
                "OA_MODE_LTP", "OA_MODE_QUOTE", "OA_MODE_DEPTH", "OA_TO_SAHI_MODE",
                "MODE_TOPIC", "TICK_KIND_LEVEL", "DEPTH_LEVELS", "_rupees", "_depth_side",
                "build_ltp_payload", "build_quote_payload", "build_depth_payload",
                "_BUILDERS", "build_payloads",
            ],
        ),
        Source("broker/sahi/streaming/sahi_mapping.py", "SahiExchangeMapper"),
        Source("broker/sahi/streaming/sahi_adapter.py", "_Sub"),
        Source("broker/sahi/streaming/sahi_adapter.py", "SahiWebSocketAdapter", ["_on_message", "_handle_tick"]),
        Source(
            "broker/sahi/mapping/transform_data.py",
            None,
            [
                "REVERSE_PRODUCT_MAP", "REVERSE_PRICETYPE_MAP", "REVERSE_ACTION_MAP", "from_paise",
                "reverse_map_product_type", "reverse_map_order_type", "reverse_map_action",
            ],
        ),
        Source(
            "broker/sahi/mapping/order_data.py",
            None,
            ["IST", "ORDER_STATUS_MAP", "map_order_status", "format_timestamp", "_oa_symbol", "_rows", "map_order_data"],
        ),
        Source("broker/sahi/streaming/sahi_order_adapter.py", None, ["normalize_order"]),
        Source("broker/sahi/streaming/sahi_order_adapter.py", "SahiOrderUpdateAdapter", ["normalize"]),
    ]

    def __init__(self, web_root, case):
        codec = load(web_root, PROTO)
        self.pb = types.SimpleNamespace(**{k: v for k, v in codec.items() if not k.startswith("__")})
        super().__init__(web_root, case)

    def stubs(self):
        def get_oa_symbol(brsymbol, exchange):
            row = self.master.by_brsymbol(brsymbol, exchange)
            return row["symbol"] if row else None

        return {"pb": self.pb, "get_oa_symbol": get_oa_symbol}

    def start(self):
        self.ad = self.new("SahiWebSocketAdapter", _subs={}, _keys={}, publish_market_data=self.publish)
        self.orders = self.new(
            "SahiOrderUpdateAdapter", broker_name="sahi", user_id="<USER_ID>", access_token="t"
        )

    def subscribe(self, row, mode, depth):
        # sahi_adapter.py subscribe: an exchange Sahi does not stream is
        # refused before anything is held; otherwise one _Sub per
        # instrument id, with the modes it holds.
        if not self.web["SahiExchangeMapper"].is_supported(row["exchange"]):
            return
        iid = int(row["token"])
        sub = self.ad._subs.get(iid)
        if sub is None:
            sub = self.web["_Sub"](row["symbol"], row["exchange"])
            self.ad._subs[iid] = sub
            self.ad._keys[(row["symbol"], row["exchange"])] = iid
        sub.modes = sub.modes | {mode}

    def unsubscribe(self, row, mode, depth):
        iid = self.ad._keys.get((row["symbol"], row["exchange"]))
        sub = self.ad._subs.get(iid)
        if sub is None:
            return
        sub.modes = sub.modes - {mode}
        if not sub.modes:
            del self.ad._subs[iid]
            del self.ad._keys[(row["symbol"], row["exchange"])]

    def push(self, socket, frame):
        if socket == "orders":
            # BaseOrderUpdateAdapter._handle_message.
            try:
                fields = self.orders.normalize(frame)
            except Exception:  # noqa: BLE001 (the web catches every error here)
                return
            if fields:
                self.order(fields)
            return
        if isinstance(frame, bytes):
            self.ad._on_message(frame)
