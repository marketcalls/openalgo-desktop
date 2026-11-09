"""Nubra's market frames (web `broker/nubra/protos/nubrafrontend_pb2.py`,
package `protos.zanskarsecurities.nubrafrontend`) read as the web reads
them: an outer `google.protobuf.Any` whose value is an inner `Any`, the
inner `type_url` naming the batch message (`api/nubrawebsocket.py`
`_decode_protobuf`). The protobuf package is not needed: this is a proto3
wire reader for those messages, giving objects that behave like the
generated ones the adapter reads (an unset scalar is 0 or "", an unset
repeated field is empty).
"""

from __future__ import annotations

import struct

from web_adapters.upstox_proto import _fields

# message -> field number -> (attribute, type, repeated)
SCHEMA = {
    "Any": {1: ("type_url", "string", False), 2: ("value", "bytes", False)},
    "WebSocketMsgIndex": {1: ("indexname", "string", False), 2: ("timestamp", "int64", False),
                          3: ("index_value", "int64", False), 4: ("high_index_value", "int64", False),
                          5: ("low_index_value", "int64", False), 6: ("volume", "int64", False),
                          7: ("changepercent", "float", False), 8: ("tick_volume", "int64", False),
                          9: ("prev_close", "int64", False), 10: ("exchange", "string", False),
                          11: ("volume_oi", "int64", False)},
    "BatchWebSocketIndexMessage": {1: ("timestamp", "int64", False), 2: ("indexes", "WebSocketMsgIndex", True),
                                   3: ("instruments", "WebSocketMsgIndex", True)},
    "OrderBookLevel": {1: ("price", "int64", False), 2: ("quantity", "int64", False), 3: ("orders", "int64", False)},
    "WebSocketMsgOrderBook": {1: ("inst_id", "uint32", False), 2: ("timestamp", "int64", False),
                              3: ("bids", "OrderBookLevel", True), 4: ("asks", "OrderBookLevel", True),
                              5: ("ltp", "int64", False), 6: ("ltq", "int64", False), 7: ("volume", "int64", False),
                              8: ("ref_id", "int64", False)},
    "BatchWebSocketOrderbookMessage": {1: ("timestamp", "int64", False),
                                       2: ("instruments", "WebSocketMsgOrderBook", True)},
    "WebSocketMsgIndexBucket": {1: ("indexname", "string", False), 2: ("exchange", "string", False),
                                3: ("interval", "int32", False), 4: ("timestamp", "int64", False),
                                5: ("open", "int64", False), 6: ("high", "int64", False), 7: ("low", "int64", False),
                                8: ("close", "int64", False), 9: ("bucket_volume", "int64", False),
                                10: ("tick_volume", "int64", False), 11: ("cumulative_volume", "int64", False),
                                12: ("bucket_timestamp", "int64", False)},
    "BatchWebSocketIndexBucketMessage": {1: ("timestamp", "int64", False),
                                         2: ("indexes", "WebSocketMsgIndexBucket", True),
                                         3: ("instruments", "WebSocketMsgIndexBucket", True)},
}
DEFAULTS = {"string": "", "bytes": b"", "int64": 0, "int32": 0, "uint32": 0, "float": 0.0}


class Msg:
    """A decoded message: unset fields read as their proto3 defaults."""

    def __init__(self, name: str):
        for _no, (attr, kind, repeated) in SCHEMA[name].items():
            setattr(self, attr, [] if repeated else (None if kind in SCHEMA else DEFAULTS[kind]))


def _value(kind: str, raw):
    if kind in SCHEMA:
        return decode(kind, raw)
    if kind == "string":
        return raw.decode("utf-8")
    if kind == "bytes":
        return bytes(raw)
    if kind == "float":
        return struct.unpack("<f", raw)[0]
    if kind in ("int64", "int32"):
        bits = 64
        return raw - (1 << bits) if raw >= 1 << (bits - 1) else raw
    return raw  # uint32


def decode(name: str, b: bytes) -> Msg:
    m = Msg(name)
    for no, _wt, raw in _fields(b):
        field = SCHEMA[name].get(no)
        if field is None:
            continue
        attr, kind, repeated = field
        v = _value(kind, raw)
        if repeated:
            getattr(m, attr).append(v)
        else:
            setattr(m, attr, v)
    return m


def batch(raw: bytes) -> tuple[str, Msg | None]:
    """The inner type name and its batch message (None for another type)."""
    outer = decode("Any", raw)
    inner = decode("Any", outer.value)
    name = inner.type_url.rsplit(".", 1)[-1]
    return name, (decode(name, inner.value) if name in SCHEMA else None)
