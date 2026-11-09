"""AliceBlue: the adapter's message handler (`ck`/`cf` login answers, `tk`
and `tf` ticks, `dk` and `df` depth), the message mapper's parsers, the
snapshot merge and the per-mode fan-out, and the order adapter's `om`
normaliser.

State as `AliceblueWebSocketAdapter.subscribe` keeps it (aliceblue_adapter.py:
588-626): one entry per `EXCHANGE|TOKEN` with every mode subscribed
(`all_modes`), the token in integer form with an index's `999` prefix
dropped, the original OpenAlgo symbol and exchange; unsubscribe drops a
mode, and the entry with its snapshot once no mode is left
(aliceblue_adapter.py:760-781). The web keeps the snapshots across a
reconnect.

`parse_tick_data` imports the app's logger inside the function; a quiet
stand-in module is registered for it (nothing of the app is imported).
`to_openalgo_symbol` reads the database; the glue's stand-in does the same
lookups on the case's symbol master.
"""

import sys
import threading
import types

from webkit import Glue, NullLogger, Source


def _quiet_logging_module():
    """`utils.logging` with only `get_logger`, for the mapper's inner import."""
    utils = sys.modules.get("utils") or types.ModuleType("utils")
    logging_mod = types.ModuleType("utils.logging")
    logging_mod.get_logger = lambda _name=None: NullLogger()
    utils.logging = logging_mod
    sys.modules.setdefault("utils", utils)
    sys.modules["utils.logging"] = logging_mod


class AliceBlue(Glue):
    broker = "aliceblue"
    sources = [
        Source("broker/aliceblue/streaming/aliceblue_mapping.py", "AliceBlueExchangeMapper"),
        Source(
            "broker/aliceblue/streaming/aliceblue_mapping.py",
            "AliceBlueMessageMapper",
            ["parse_tick_data", "parse_depth_data"],
        ),
        Source(
            "broker/aliceblue/streaming/aliceblue_adapter.py",
            "AliceblueWebSocketAdapter",
            ["_handle_message", "_on_data_received", "_update_market_snapshot", "_handle_generic_market_data"],
        ),
        Source(
            "broker/aliceblue/streaming/aliceblue_order_adapter.py",
            None,
            ["_STATUS_MAP", "_PRICETYPE_MAP", "_ACTION_MAP"],
        ),
        Source("broker/aliceblue/streaming/aliceblue_order_adapter.py", "AliceBlueOrderUpdateAdapter", ["normalize"]),
    ]

    def stubs(self):
        _quiet_logging_module()
        return {"to_openalgo_symbol": self.to_openalgo_symbol}

    def start(self):
        self.ad = self.new(
            "AliceblueWebSocketAdapter",
            subscriptions={},
            market_snapshots={},
            symbol_state={},
            exchange_mapper=self.web["AliceBlueExchangeMapper"](),
            message_mapper=self.web["AliceBlueMessageMapper"](),
            connected=False,
            _auth_event=threading.Event(),
            _resubscribe_after_auth=lambda: None,
            publish_market_data=self.publish,
        )
        self.orders = self.new("AliceBlueOrderUpdateAdapter")

    def key(self, row):
        """`subscribe`'s token and key (aliceblue_adapter.py:560-590)."""
        exchange = row["exchange"]
        token = str(int(float(str(row["token"]))))
        if exchange in ["NSE_INDEX", "BSE_INDEX", "MCX_INDEX"] and token.startswith("999"):
            token = token[3:]
        ab_exchange = self.ad.exchange_mapper.to_broker_exchange(exchange)
        return f"{ab_exchange}|{token}", ab_exchange, token

    def subscribe(self, row, mode, depth):
        sub_key, ab_exchange, token = self.key(row)
        subs = self.ad.subscriptions
        existing_mode = subs.get(sub_key, {}).get("mode", 0)
        entry = {
            "symbol": row["symbol"],
            "exchange": row["exchange"],
            "ab_exchange": ab_exchange,
            "token": token,
            "mode": mode,
            "depth_level": depth,
            "original_symbol": row["symbol"],
            "original_exchange": row["exchange"],
        }
        if mode > existing_mode:
            entry["all_modes"] = subs.get(sub_key, {}).get("all_modes", set()) | {mode}
            subs[sub_key] = entry
        elif sub_key not in subs:
            entry["all_modes"] = {mode}
            subs[sub_key] = entry
        else:
            subs[sub_key]["all_modes"] = subs[sub_key].get("all_modes", set()) | {mode}

    def unsubscribe(self, row, mode, depth):
        sub_key, _, _ = self.key(row)
        subs = self.ad.subscriptions
        if sub_key not in subs:
            return
        all_modes = subs[sub_key].get("all_modes", set())
        all_modes.discard(mode)
        if not all_modes:
            del subs[sub_key]
            self.ad.symbol_state.pop(sub_key, None)
            self.ad.market_snapshots.pop(sub_key, None)
        else:
            subs[sub_key]["all_modes"] = all_modes
            subs[sub_key]["mode"] = max(all_modes)

    def push(self, socket, frame):
        if socket == "orders":
            fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
        else:
            self.ad._handle_message(frame)
