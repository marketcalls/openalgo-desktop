"""Upstox's `FeedResponse` (web `broker/upstox/streaming/MarketDataFeedV3.proto`)
decoded to the dict `google.protobuf.json_format.MessageToDict` makes of it
with default options, which is what `upstox_client._decode_protobuf_to_dict`
hands the adapter. The protobuf package is not needed: this is a proto3 wire
reader for exactly that schema, with MessageToDict's rules:

- keys are the fields' JSON names (the .proto's names are already camelCase);
- a scalar equal to its default (0, 0.0, false, "", enum 0) is left out;
- int64 is a string, double a float, an enum its name;
- a message field on the wire is present (an empty one as {});
- `google.protobuf.DoubleValue` is its bare value, kept when present, 0.0
  included;
- repeated fields are lists and maps are dicts, in wire order.
"""

from __future__ import annotations

import struct

ENUMS = {
    "Type": {0: "initial_feed", 1: "live_feed", 2: "market_info"},
    "RequestMode": {0: "ltpc", 1: "full_d5", 2: "option_greeks", 3: "full_d30"},
    "MarketStatus": {0: "PRE_OPEN_START", 1: "PRE_OPEN_END", 2: "NORMAL_OPEN", 3: "NORMAL_CLOSE",
                     4: "CLOSING_START", 5: "CLOSING_END"},
}

# message -> field number -> (json name, type, repeated)
SCHEMA = {
    "LTPC": {1: ("ltp", "double", False), 2: ("ltt", "int64", False), 3: ("ltq", "int64", False),
             4: ("cp", "double", False), 5: ("iep", "wrap_double", False)},
    "MarketLevel": {1: ("bidAskQuote", "Quote", True)},
    "MarketOHLC": {1: ("ohlc", "OHLC", True)},
    "Quote": {1: ("bidQ", "int64", False), 2: ("bidP", "double", False), 3: ("askQ", "int64", False),
              4: ("askP", "double", False)},
    "OptionGreeks": {1: ("delta", "double", False), 2: ("theta", "double", False), 3: ("gamma", "double", False),
                     4: ("vega", "double", False), 5: ("rho", "double", False)},
    "OHLC": {1: ("interval", "string", False), 2: ("open", "double", False), 3: ("high", "double", False),
             4: ("low", "double", False), 5: ("close", "double", False), 6: ("vol", "int64", False),
             7: ("ts", "int64", False)},
    "MarketFullFeed": {1: ("ltpc", "LTPC", False), 2: ("marketLevel", "MarketLevel", False),
                       3: ("optionGreeks", "OptionGreeks", False), 4: ("marketOHLC", "MarketOHLC", False),
                       5: ("atp", "double", False), 6: ("vtt", "int64", False), 7: ("oi", "double", False),
                       8: ("iv", "double", False), 9: ("tbq", "double", False), 10: ("tsq", "double", False),
                       11: ("iep", "double", False), 12: ("rp", "double", False), 13: ("ieq", "int64", False),
                       14: ("iiqTotal", "int64", False), 15: ("iiqM", "int64", False),
                       16: ("casEligible", "bool", False)},
    "IndexFullFeed": {1: ("ltpc", "LTPC", False), 2: ("marketOHLC", "MarketOHLC", False)},
    "FullFeed": {1: ("marketFF", "MarketFullFeed", False), 2: ("indexFF", "IndexFullFeed", False)},
    "FirstLevelWithGreeks": {1: ("ltpc", "LTPC", False), 2: ("firstDepth", "Quote", False),
                             3: ("optionGreeks", "OptionGreeks", False), 4: ("vtt", "int64", False),
                             5: ("oi", "double", False), 6: ("iv", "double", False)},
    "Feed": {1: ("ltpc", "LTPC", False), 2: ("fullFeed", "FullFeed", False),
             3: ("firstLevelWithGreeks", "FirstLevelWithGreeks", False), 4: ("requestMode", "enum:RequestMode", False)},
    "StatusInfo": {1: ("status", "string", False), 2: ("updatedTime", "int64", False)},
    "MarketInfo": {1: ("segmentStatus", "map:enum:MarketStatus", False), 2: ("casMarketStatus", "map:StatusInfo", False),
                   3: ("preOpenSessionStatus", "map:StatusInfo", False)},
    "FeedResponse": {1: ("type", "enum:Type", False), 2: ("feeds", "map:Feed", False), 3: ("currentTs", "int64", False),
                     4: ("marketInfo", "MarketInfo", False)},
}


def _varint(b: bytes, i: int) -> tuple[int, int]:
    shift = result = 0
    while True:
        byte = b[i]
        i += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, i
        shift += 7


def _fields(b: bytes):
    """(field number, wire type, raw value) in wire order."""
    i = 0
    while i < len(b):
        key, i = _varint(b, i)
        no, wt = key >> 3, key & 7
        if wt == 0:
            v, i = _varint(b, i)
        elif wt == 1:
            v, i = b[i:i + 8], i + 8
        elif wt == 2:
            n, i = _varint(b, i)
            v, i = b[i:i + n], i + n
        elif wt == 5:
            v, i = b[i:i + 4], i + 4
        else:
            raise ValueError(f"wire type {wt}")
        yield no, wt, v


def _scalar(kind: str, v):
    if kind == "double":
        return struct.unpack("<d", v)[0]
    if kind == "int64":
        return v - (1 << 64) if v >= 1 << 63 else v
    if kind == "bool":
        return bool(v)
    if kind == "string":
        return v.decode("utf-8")
    if kind.startswith("enum:"):
        return v
    raise ValueError(kind)


def _render(kind: str, v):
    """MessageToDict's JSON value of a decoded scalar."""
    if kind == "int64":
        return str(v)
    if kind.startswith("enum:"):
        return ENUMS[kind[5:]].get(v, v)
    return v


def _default(kind: str, v) -> bool:
    return v in (0, 0.0, False, "") and not isinstance(v, dict)


def decode(name: str, b: bytes) -> dict:
    out: dict = {}
    for no, _wt, raw in _fields(b):
        field = SCHEMA[name].get(no)
        if field is None:
            continue
        key, kind, repeated = field
        if kind.startswith("map:"):
            vkind = kind[4:]
            k = v = None
            for eno, _ewt, eraw in _fields(raw):
                if eno == 1:
                    k = eraw.decode("utf-8")
                elif eno == 2:
                    v = eraw
            value = decode(vkind, v or b"") if vkind in SCHEMA else _render(vkind, _scalar(vkind, v or 0))
            out.setdefault(key, {})[k or ""] = value
        elif kind == "wrap_double":
            inner = dict((n, r) for n, _w, r in _fields(raw))
            out[key] = struct.unpack("<d", inner[1])[0] if 1 in inner else 0.0
        elif kind in SCHEMA:
            value = decode(kind, raw)
            if repeated:
                out.setdefault(key, []).append(value)
            else:
                out[key] = value
        else:
            v = _scalar(kind, raw)
            out[key] = v
    # proto3: a scalar left at its default is not printed.
    for no, (key, kind, _r) in SCHEMA[name].items():
        if key in out and kind not in SCHEMA and not kind.startswith(("map:", "wrap_")):
            if _default(kind, out[key]):
                del out[key]
            else:
                out[key] = _render(kind, out[key])
    return out


def feed_response(b: bytes) -> dict:
    return decode("FeedResponse", b)
