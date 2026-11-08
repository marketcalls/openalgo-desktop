"""Nubra: the adapter's index, index-bucket (OHLCV) and order-book callbacks
(`NubraWebSocketAdapter._on_index_data`, `_on_ohlcv_data`,
`_on_orderbook_data`), fed the messages `NubraWebSocket._decode_protobuf`
unpacks (`web_adapters/nubra_proto.py`, since the protobuf package is not
needed here), with the routing state the adapter's subscribe keeps: LTP
and Quote ride the index channel under the subscription name
(`SUBSCRIPTION_MAP` for an index, the symbol for an instrument), Depth the
order-book channel under the numeric token. The web refuses Depth on an
index, so such a subscribe leaves no state.

Order updates: the order adapter's `normalize` and its field walker. It
unwraps the frame with protobuf's `Any`, resolves the instrument on the
master contract and maps the exchange; the glue stands in for all three
(`ProtoAny` on `nubra_proto`, and lookups on the case's master that follow
`mapping/order_data.py::resolve_instrument` for a cash instrument: the
token on the Nubra exchange first, then the broker symbol).
"""

from web_adapters.nubra_proto import batch, decode
from webkit import Glue, Source


class Nubra(Glue):
    broker = "nubra"
    sources = [
        Source("broker/nubra/api/nubrawebsocket.py", None, ["INDEX_NAME_MAP", "SUBSCRIPTION_MAP"]),
        Source(
            "broker/nubra/streaming/nubra_adapter.py",
            "NubraWebSocketAdapter",
            ["_on_index_data", "_on_ohlcv_data", "_on_orderbook_data"],
        ),
        Source(
            "broker/nubra/streaming/nubra_order_adapter.py",
            None,
            ["_STATUS_MAP", "_ACTION_MAP", "_PRICETYPE_MAP", "_PRODUCT_MAP", "_decode_fields", "_first", "_first_str"],
        ),
        Source("broker/nubra/streaming/nubra_order_adapter.py", "NubraOrderUpdateAdapter", ["normalize"]),
    ]

    def stubs(self):
        class ProtoAny:
            def ParseFromString(self, raw):  # noqa: N802 (protobuf's name)
                m = decode("Any", raw)
                self.type_url, self.value = m.type_url, m.value

        def resolve_instrument(brexchange, derivative_type=None, ref_id=None, broker_symbol=None):
            ex = str(brexchange or "").strip().upper()
            row = (ref_id and self.master.by_token(str(ref_id), ex)) or (
                broker_symbol and self.master.by_brsymbol(broker_symbol, ex))
            return (row["symbol"], ex) if row else (None, None)

        return {
            "ProtoAny": ProtoAny,
            "resolve_instrument": resolve_instrument,
            "map_exchange": lambda ex, dt=None: str(ex or "").upper(),
        }

    def start(self):
        self.ad = self.new(
            "NubraWebSocketAdapter",
            symbol_modes={},
            index_name_to_sub={},
            ref_id_to_symbol={},
            ohlcv_cache={},
            publish_market_data=self.publish,
        )
        self.orders = self.new("NubraOrderUpdateAdapter")

    def subscribe(self, row, mode, depth):
        sym, exch = row["symbol"], row["exchange"]
        index = exch.endswith("_INDEX")
        key = f"{exch}:{sym}"
        if mode == 3:
            if index:
                return  # "Depth mode not supported for index symbols"
            self.ad.ref_id_to_symbol[int(str(row["token"]).split(":")[0])] = (sym, exch)
        else:
            name = (self.web["SUBSCRIPTION_MAP"].get(sym) or row["brsymbol"]) if index else sym
            self.ad.index_name_to_sub[name.upper()] = (sym, exch)
            if index:
                for ws_name, oa_sym in self.web["INDEX_NAME_MAP"].items():
                    if oa_sym == sym:
                        self.ad.index_name_to_sub[ws_name] = (sym, exch)
        self.ad.symbol_modes.setdefault(key, set()).add(mode)

    def unsubscribe(self, row, mode, depth):
        self.ad.symbol_modes.get(f"{row['exchange']}:{row['symbol']}", set()).discard(mode)

    def push(self, socket, frame):
        if socket == "orders":
            fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
            return
        if not isinstance(frame, bytes):
            return
        name, msg = batch(frame)
        if name == "BatchWebSocketIndexMessage":
            for obj in list(msg.indexes) + list(msg.instruments):
                self.ad._on_index_data(obj)
        elif name == "BatchWebSocketIndexBucketMessage":
            for obj in list(msg.indexes) + list(msg.instruments):
                self.ad._on_ohlcv_data(obj)
        elif name == "BatchWebSocketOrderbookMessage":
            for obj in msg.instruments:
                self.ad._on_orderbook_data(obj)
