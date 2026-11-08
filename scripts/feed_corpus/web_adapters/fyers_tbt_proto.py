"""Fyers TBT's `SocketMessage` (web `broker/fyers/streaming/msg_pb2.py`) read
into objects that behave like the generated ones `fyers_tbt_websocket.py`
reads: `HasField`, wrapper `.value`, repeated fields as lists and the
`feeds` map as a dict. The protobuf package is not needed: a proto3 wire
reader for the fields the web reads, with the field numbers the desktop's
decoder uses (`brokers/fyers/streaming/tbt.rs`).
"""

from __future__ import annotations

from web_adapters.upstox_proto import _fields

WRAP = "wrapper"
# message -> field number -> (attribute, type, repeated)
SCHEMA = {
    "SocketMessage": {2: ("feeds", "map:MarketFeed", False), 3: ("snapshot", "bool", False),
                      4: ("msg", "string", False), 5: ("error", "bool", False)},
    "MarketFeed": {5: ("depth", "Depth", False), 6: ("feed_time", WRAP, False), 7: ("send_time", WRAP, False),
                   11: ("ticker", "string", False)},
    "Depth": {1: ("tbq", WRAP, False), 2: ("tsq", WRAP, False), 3: ("asks", "MarketLevel", True),
              4: ("bids", "MarketLevel", True)},
    "MarketLevel": {1: ("price", WRAP, False), 2: ("qty", WRAP, False), 3: ("nord", WRAP, False),
                    4: ("num", WRAP, False)},
}


class Wrapper:
    def __init__(self, value=0):
        self.value = value


class Msg:
    def __init__(self, name: str):
        self._name = name
        self._present: set[str] = set()
        for _no, (attr, kind, repeated) in SCHEMA[name].items():
            default = [] if repeated else {} if kind.startswith("map:") else (
                Wrapper() if kind == WRAP else None if kind in SCHEMA else "" if kind == "string" else False)
            setattr(self, attr, default)

    def HasField(self, name: str) -> bool:  # noqa: N802 (protobuf's name)
        return name in self._present


def decode(name: str, b: bytes) -> Msg:
    m = Msg(name)
    for no, _wt, raw in _fields(b):
        field = SCHEMA[name].get(no)
        if field is None:
            continue
        attr, kind, repeated = field
        if kind.startswith("map:"):
            k, v = "", b""
            for eno, _e, eraw in _fields(raw):
                if eno == 1:
                    k = eraw.decode("utf-8")
                elif eno == 2:
                    v = eraw
            getattr(m, attr)[k] = decode(kind[4:], v)
            continue
        if kind == WRAP:
            inner = {n: r for n, _w, r in _fields(raw)}
            v = Wrapper(inner.get(1, 0))
        elif kind in SCHEMA:
            v = decode(kind, raw)
        elif kind == "string":
            v = raw.decode("utf-8")
        else:
            v = bool(raw)
        if repeated:
            getattr(m, attr).append(v)
        else:
            setattr(m, attr, v)
        m._present.add(attr)
    return m


def socket_message(b: bytes) -> Msg:
    return decode("SocketMessage", b)
