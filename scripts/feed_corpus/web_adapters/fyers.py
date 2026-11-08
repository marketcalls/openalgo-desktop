"""Fyers: the HSM client's frame parsers, FyersAdapter's routing (the quote
a depth message borrows, the depth sent again on the first quote) and
FyersDataMapper, as `FyersWebSocketAdapter` wires them; and the order
adapter's normaliser.

The web adapter registers one closure per subscription (`subscribe`'s
`data_callback`: it stamps the subscription's symbol, exchange and mode,
then `_send_data` publishes on `EXCHANGE_SYMBOL_MODE`) and one dispatcher
per HSM slot (`_flush_hsm_batch`, `_SLOT_MODES`: LTP and Quote share the
symbol feed). The glue builds the same callbacks; the topics and their
broker-symbol mappings are what `fyers_token_converter.py` derives from
the master's fytoken (`sf|<segment>|<fytoken[10:]>`, `dp|` for depth with
its `sf|` twin, `if|<segment>|<name>` for an index), worked here from the
case's rows as the desktop does.

`to_openalgo_symbol` reads the database; the glue's stand-in does the same
lookups on the case's symbol master (`Glue.to_openalgo_symbol`).
"""

from webkit import Glue, Source

MODES = {1: "LTP", 2: "QUOTE", 3: "DEPTH"}
SEGMENTS = {"1010": "nse_cm", "1011": "nse_fo", "1120": "mcx_fo", "1210": "bse_cm", "1211": "bse_fo",
            "1212": "bcs_fo", "1012": "cde_fo", "1020": "nse_com"}


class Fyers(Glue):
    broker = "fyers"
    sources = [
        Source(
            "broker/fyers/streaming/fyers_hsm_websocket.py",
            "FyersHSMWebSocket",
            [
                "_auth_response_ok",
                "_parse_binary_message",
                "_parse_data_feed",
                "_parse_snapshot_data",
                "_parse_scrip_snapshot",
                "_parse_index_snapshot",
                "_parse_depth_snapshot",
                "_parse_update_data",
            ],
        ),
        Source("broker/fyers/streaming/fyers_mapping.py", "FyersDataMapper"),
        Source("broker/fyers/streaming/fyers_adapter.py", None, ["_QUOTE_FIELDS_FOR_DEPTH"]),
        Source(
            "broker/fyers/streaming/fyers_adapter.py",
            "FyersAdapter",
            ["_on_message", "_quote_state", "_quote_fields_for_depth"],
        ),
        Source(
            "broker/fyers/streaming/fyers_order_adapter.py",
            None,
            [
                "_STATUS_MAP",
                "_PRICETYPE_MAP",
                "_ACTION_MAP",
                "_EXCHANGE_SEGMENT_MAP",
                "_ENVELOPE_KEYS",
                "_ID_KEYS",
                "_STATUS_KEYS",
                "_pick",
                "_unwrap_order_record",
                "_oa_exchange",
            ],
        ),
        Source("broker/fyers/streaming/fyers_order_adapter.py", "FyersOrderUpdateAdapter", ["normalize"]),
    ]

    def stubs(self):
        return {"to_openalgo_symbol": self.to_openalgo_symbol}

    def start(self):
        self.ad = self.new(
            "FyersAdapter",
            data_mapper=self.new("FyersDataMapper"),
            hsm_to_symbol={},
            symbol_to_hsm={},
            active_subscriptions={},
            subscription_callbacks={},
            last_ltp={},
            latest_quote=None,
            depth_awaiting_quote=None,
        )
        self.hsm = self.new(
            "FyersHSMWebSocket",
            subscriptions={},
            symbol_mappings={},
            scrips_data={},
            index_data={},
            depth_data={},
            on_message_callback=self.ad._on_message,
            on_error_callback=None,
            on_open_callback=None,
            authenticated=False,
            _pending_hsm_symbols=[],
        )
        self.orders = self.new("FyersOrderUpdateAdapter")
        # (mode, "EXCHANGE:SYMBOL") -> the subscription's data_callback.
        self.callbacks = {}

    @staticmethod
    def topics(row, mode):
        token, br = str(row["token"]), row["brsymbol"]
        seg = SEGMENTS[token[:4]]
        if br.endswith("-INDEX"):
            return [f"if|{seg}|{row.get('name') or br.split(':', 1)[-1].replace('-INDEX', '')}"]
        topics = [f"sf|{seg}|{token[10:]}"]
        if mode == 3:
            topics.append(f"dp|{seg}|{token[10:]}")
        return topics

    def subscribe(self, row, mode, depth):
        sym, exch = row["symbol"], row["exchange"]
        full = f"{exch}:{sym}"

        def data_callback(data, sym=sym, exch=exch, mode=mode):
            data["symbol"] = sym
            data["exchange"] = exch
            data["subscription_mode"] = mode
            self.publish(f"{exch}_{sym}_{MODES[mode]}", data)

        self.callbacks[(mode, full)] = data_callback
        slot = "DepthUpdate" if mode == 3 else "SymbolUpdate"
        modes = (3,) if mode == 3 else (1, 2)

        def dispatch(data, modes=modes):
            if not data:
                return
            key = f"{data.get('exchange')}:{data.get('symbol')}"
            targets = [cb for cb in (self.callbacks.get((m, key)) for m in modes) if cb]
            for cb in targets:
                cb(dict(data) if len(targets) > 1 else data)

        self.ad.subscription_callbacks[f"{slot}_{full}"] = dispatch
        self.ad.active_subscriptions[full] = {"exchange": exch, "symbol": sym, "data_type": slot}
        for t in self.topics(row, mode):
            self.ad.hsm_to_symbol[t] = full
            self.hsm.symbol_mappings[t] = row["brsymbol"]

    def unsubscribe(self, row, mode, depth):
        self.callbacks.pop((mode, f"{row['exchange']}:{row['symbol']}"), None)

    def push(self, socket, frame):
        if socket == "orders":
            fields = self.orders.normalize(frame)
            if fields:
                self.order(fields)
        elif isinstance(frame, bytes):
            self.hsm._parse_binary_message(bytearray(frame))
