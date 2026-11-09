"""IIFL Capital: the MQTT feed client's packet decoders and dispatch, the
adapter's per-mode fan-out, and the order adapter's normalisers.

The desktop's feed reads IIFL's MQTT PUBLISH packets through its loopback
relay, which hands each one on as a binary frame `[u16 BE topic length]
[topic][payload]` (brokers/iiflcapital/mqtt_relay.rs `encode_publish`);
the corpus pushes those frames. The web's client receives the same topic
and payload from its own MQTT reader (`IiflcapitalWebSocket.
_on_mqtt_message`), so `push` unwraps the relay framing and hands the web
exactly the bytes the desktop decodes. The relay's ready text frame is
the desktop's own and reaches nothing on the web.

State as `IiflcapitalWebSocketAdapter.subscribe` / `unsubscribe` and the
client's `subscribe_instruments` / `unsubscribe_instruments` keep it
(iiflcapital_adapter.py:220-344, iiflcapital_websocket.py:388-475): modes
as a set per symbol, the broker stream at the highest mode with OI for
derivatives, one mode's release keeping the stream for the others. The
client's OI cache is dropped only by an unsubscribe, not by a reconnect
(iiflcapital_websocket.py:282, 467). Order packets resolve the exchange
by probing the instrument id on the candidate exchanges.
"""

import ctypes
import json

from webkit import Glue, Source

_WS = "broker/iiflcapital/streaming/iiflcapital_websocket.py"
_ORDERS = "broker/iiflcapital/streaming/iiflcapital_order_adapter.py"


class IiflCapital(Glue):
    broker = "iiflcapital"
    sources = [
        Source(
            _WS,
            None,
            [
                "TOPIC_MARKET_FEED", "TOPIC_INDEX_FEED", "TOPIC_OPEN_INTEREST", "MODE_LTP", "MODE_QUOTE",
                "MODE_FULL", "_Depth", "_MWBOCombined", "_MWBOCombined_SIZE", "_decode_market_feed",
                "_decode_open_interest", "_topic_key",
            ],
        ),
        Source(_WS, "IiflcapitalWebSocket", ["_on_mqtt_message", "_dispatch_feed", "_dispatch_oi"]),
        Source(
            "broker/iiflcapital/streaming/iiflcapital_mapping.py",
            None,
            ["INDEX_EXCHANGES", "OI_ELIGIBLE_EXCHANGES", "is_index_exchange", "supports_open_interest",
             "normalize_segment"],
        ),
        Source("broker/iiflcapital/streaming/iiflcapital_adapter.py", "IiflcapitalWebSocketAdapter",
               ["_handle_ticks", "_publish"]),
        Source("broker/iiflcapital/mapping/transform_data.py", None,
               ["reverse_map_order_type", "reverse_map_product_type"]),
        Source(
            _ORDERS,
            None,
            ["TOPIC_ORDER_UPDATE", "_STATUS_MAP", "_IIFL_EXCHANGE_MAP", "_CANDIDATE_EXCHANGES", "_to_int",
             "_to_float", "_ACTION_MAP", "_normalize_action", "_resolve_order_exchange"],
        ),
        Source(_ORDERS, "IiflCapitalOrderUpdateAdapter", ["normalize", "_normalize_order", "_normalize_trade"]),
    ]

    def stubs(self):
        def get_symbol(token, exchange):
            row = self.master.by_token(token, exchange)
            return row["symbol"] if row else None

        return {"ctypes": ctypes, "get_symbol": get_symbol, "to_openalgo_symbol": self.to_openalgo_symbol}

    def start(self):
        self.ad = self.new("IiflcapitalWebSocketAdapter", subscribed_symbols={}, _key_to_symbol={},
                           publish_market_data=self.publish)
        self.ws = self.new("IiflcapitalWebSocket", _subscriptions={}, _oi_cache={}, on_ticks=self.ad._handle_ticks)
        self.ws._lock = self.ws.lock
        self.orders = self.new("IiflCapitalOrderUpdateAdapter")

    def subscribe(self, row, mode, depth):
        w = self.web
        symbol, exchange, segment, token = row["symbol"], row["exchange"], row["brexchange"], str(row["token"])
        key = f"{exchange}:{symbol}"
        is_index = w["is_index_exchange"](exchange)
        modes = set(self.ad.subscribed_symbols.get(key, {}).get("modes", set())) | {mode}
        self.ad.subscribed_symbols[key] = {"exchange": exchange, "symbol": symbol, "segment": segment,
                                           "token": token, "modes": modes, "is_index": is_index}
        self.ad._key_to_symbol[f"{w['normalize_segment'](segment)}/{token}"] = (symbol, exchange)
        effective = max(modes)
        self.ws._subscriptions[w["_topic_key"](segment, token)] = {
            "mode": {1: w["MODE_LTP"], 2: w["MODE_QUOTE"], 3: w["MODE_FULL"]}[effective],
            "is_index": is_index,
            "oi": effective != 1 and w["supports_open_interest"](exchange) and not is_index,
            "segment": segment.lower(),
            "token": token,
        }

    def unsubscribe(self, row, mode, depth):
        w = self.web
        key = f"{row['exchange']}:{row['symbol']}"
        sub = self.ad.subscribed_symbols.get(key)
        if sub is None:
            return
        if sub["modes"] - {mode}:
            sub["modes"].discard(mode)
            return
        self.ad.subscribed_symbols.pop(key, None)
        self.ad._key_to_symbol.pop(f"{w['normalize_segment'](sub['segment'])}/{sub['token']}", None)
        topic_key = w["_topic_key"](sub["segment"], sub["token"])
        self.ws._subscriptions.pop(topic_key, None)
        self.ws._oi_cache.pop(topic_key, None)

    @staticmethod
    def publish_frame(frame):
        """mqtt_relay.rs `decode_publish`: [u16 BE topic length][topic][payload]."""
        n = int.from_bytes(frame[:2], "big")
        return frame[2:2 + n].decode(), frame[2 + n:]

    def push(self, socket, frame):
        if not isinstance(frame, bytes):
            return
        topic, payload = self.publish_frame(frame)
        if socket == "orders":
            kind = "order" if topic.startswith(self.web["TOPIC_ORDER_UPDATE"]) else "trade"
            fields = self.orders.normalize(kind, json.loads(payload))
            if fields:
                self.order(fields)
        else:
            self.ws._on_mqtt_message(topic, payload)
