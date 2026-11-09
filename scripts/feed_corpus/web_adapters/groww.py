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

Order updates are the web's REST poll of the order book
(`websocket_proxy/order_adapter.py::PollingOrderUpdateAdapter`): each
answer the fake broker API gives the CASH order list is the poller's next
read. The order book is the web's own Groww mapping (`map_order_data`,
`transform_order_data`, `format_order_data`, `openalgo_exchange`) after
`direct_get_order_book` resolves each row's exchange and symbol
(order_api.py:152-161, on the case's master); the poll diff and the
published fields are written from order_adapter.py:456-475 and :490-517,
since `_poll_loop` and `_publish_order` import the app.
"""

import copy
import json
import types

from webkit import Glue, Source

PROTOBUF_NAMES = [
    "VARINT", "FIXED64", "LENGTH_DELIMITED", "FIXED32",
    "FIELD_SYMBOL", "FIELD_SEGMENT", "FIELD_EXCHANGE", "FIELD_STOCK_LIVE_PRICE",
    "FIELD_MARKET_DEPTH", "FIELD_LIVE_INDICES", "FIELD_TS_IN_MILLIS", "FIELD_OPEN",
    "FIELD_HIGH", "FIELD_LOW", "FIELD_CLOSE", "FIELD_VOLUME", "FIELD_VALUE", "FIELD_LTP",
    "parse_groww_market_data",
]

ORDER_NAMES = [
    "GROWW_ORDER_STATUS_MAP", "GROWW_PRICETYPE_MAP", "map_order_status", "_orders_from",
    "map_order_data", "transform_order_data",
]
ORDER_LIST = "/v1/order/list"


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
        Source("services/orderbook_service.py", None, ["format_decimal", "format_order_data"]),
        Source(
            "broker/groww/mapping/transform_data.py",
            None,
            ["EXCHANGE_NSE", "EXCHANGE_BSE", "SEGMENT_FNO", "openalgo_exchange"],
        ),
    ]

    def stubs(self):
        # nats_websocket.py imports the decoder as a module (`from . import
        # groww_protobuf`); its one call goes to the lifted function.
        return {
            "groww_protobuf": types.SimpleNamespace(
                parse_groww_market_data=lambda data: self.web["parse_groww_market_data"](data)
            )
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
        # The order list per segment as the fake answers it, and the poll's
        # snapshot (None until its first, silent read).
        self.order_lists = {}
        self.baseline = None

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
        self.ws.nats_sids.clear()
        self.next_sid = 1
        for price_key, book_key in self.subjects.values():
            self.number(price_key, book_key)

    def push(self, socket, frame):
        # Only a MSG carries market data; INFO, +OK and PING publish nothing.
        if isinstance(frame, bytes) and b"MSG" in frame[:50]:
            self.ws._process_binary_nats_message(frame)

    def serve(self, method, path, body):
        if not path.startswith(ORDER_LIST + "?"):
            return
        segment = "FNO" if "segment=FNO" in path else "CASH"
        self.order_lists[segment] = json.loads(body)["payload"]["order_list"]
        if segment == "CASH":
            self.poll()

    def order_book(self):
        """get_orderbook_with_auth (orderbook_service.py:149-167) on
        direct_get_order_book's rows (order_api.py:130-164)."""
        rows = copy.deepcopy(self.order_lists.get("CASH", []) + self.order_lists.get("FNO", []))
        for o in rows:
            exchange = self.web["openalgo_exchange"](o.get("exchange", ""), o.get("segment", ""))
            known = self.master.by_brsymbol(o["trading_symbol"], exchange)
            o["brsymbol"], o["brexchange"] = o["trading_symbol"], o.get("exchange", "")
            o["exchange"] = exchange
            o["symbol"] = known["symbol"] if known else o["trading_symbol"]
        mapped = self.web["map_order_data"](order_data={"data": rows, "order_list": rows})
        return self.web["format_order_data"](self.web["transform_order_data"](mapped))

    def poll(self):
        """One read of _poll_loop (order_adapter.py:456-475): the first
        seeds the snapshot silently."""
        snapshot, by_id = {}, {}
        for order in self.order_book():
            orderid = str(order.get("orderid", ""))
            if not orderid:
                continue
            snapshot[orderid] = (order.get("order_status", ""), int(order.get("filled_quantity") or 0))
            by_id[orderid] = order
        if self.baseline is not None:
            for orderid, state in snapshot.items():
                if self.baseline.get(orderid) != state:
                    self.publish_order(by_id[orderid])
        self.baseline = snapshot

    def publish_order(self, order):
        """_publish_order's OrderUpdateEvent fields (order_adapter.py:494-517)."""
        raw_status = str(order.get("order_status", "")).strip().lower()
        self.order({
            "orderid": str(order.get("orderid", "")),
            "symbol": order.get("symbol", ""),
            "exchange": order.get("exchange", ""),
            "action": str(order.get("action", "")).upper(),
            "quantity": int(order.get("quantity") or 0),
            "price": float(order.get("price") or 0),
            "trigger_price": float(order.get("trigger_price") or 0),
            "pricetype": order.get("pricetype", ""),
            "product": order.get("product", ""),
            "order_status": raw_status,
            "filled_quantity": int(order.get("filled_quantity") or 0),
            "pending_quantity": int(order.get("pending_quantity") or 0),
            "average_price": float(order.get("average_price") or 0),
            "rejection_reason": order.get("rejection_reason", "") if raw_status == "rejected" else "",
        })
