"""Dhan Sandbox: the web's simulated feed (`Dhan_sandboxWebSocketAdapter`).

The adapter has no broker socket: `_mock_streaming_loop` wakes once a
second and publishes a made-up frame per subscription. The corpus drives
it the way the desktop's ticker drives the desktop's feed: each frame the
script pushes is one wake of the loop (the frozen `time.sleep` that ends
the round also ends the loop), so a script's tick produces one round of
the web's frames. `random` is seeded (webkit), so the values repeat.

The reference price reads the master's calls through
`database.token_db_enhanced.fno_search_symbols`, imported inside the
method; the glue stands it up on the case's instruments, filtering and
sorting as the web's in-memory search does (exchange, the underlying the
web extracts from the symbol, the `CE` suffix, alphabetical by symbol, at
most `limit`).
"""

import sys
import types

from webkit import Glue, Source

# utils/constants.py FNO_EXCHANGES and CRYPTO_EXCHANGES.
FNO = {"NFO", "BFO", "MCX", "CDS", "BCD", "NCDEX", "NCO", "CRYPTO"}


class DhanSandbox(Glue):
    broker = "dhan_sandbox"
    sources = [
        Source(
            "database/token_db_enhanced.py",
            None,
            ["_UNDERLYING_PATTERN", "_CRYPTO_UNDERLYING_PATTERN", "extract_underlying_from_symbol"],
        ),
        Source(
            "broker/dhan_sandbox/streaming/dhan_sandbox_adapter.py",
            "Dhan_sandboxWebSocketAdapter",
            [
                "_stable_noise",
                "_parse_option_contract",
                "_estimate_spot_from_contracts",
                "_derive_reference_price",
                "subscribe",
                "unsubscribe",
                "_mock_streaming_loop",
            ],
        ),
    ]

    def stubs(self):
        return {"FNO_EXCHANGES": FNO, "CRYPTO_EXCHANGES": {"CRYPTO"}}

    def start(self):
        underlying_of = self.web["extract_underlying_from_symbol"]
        rows = self.case["instruments"]

        def fno_search_symbols(exchange=None, underlying=None, instrumenttype=None, limit=10000, **_):
            want = (underlying or "").strip().upper()
            found = [
                {"symbol": r["symbol"], "strike": r.get("strike", 0)}
                for r in rows
                if r["exchange"] == exchange
                and underlying_of(r["symbol"], r["exchange"]) == want
                and r["symbol"].upper().endswith(instrumenttype)
            ]
            found.sort(key=lambda r: r["symbol"])
            return found[:limit]

        db = types.ModuleType("database.token_db_enhanced")
        db.fno_search_symbols = fno_search_symbols
        sys.modules.setdefault("database", types.ModuleType("database"))
        sys.modules["database.token_db_enhanced"] = db
        self.ad = self.new(
            "Dhan_sandboxWebSocketAdapter",
            broker_name="dhan_sandbox",
            subscriptions={},
            _current_prices={},
            _spot_hint_cache={},
            running=False,
            publish_market_data=self.publish,
        )

    def subscribe(self, row, mode, depth):
        self.ad.subscribe(row["symbol"], row["exchange"], mode, depth)

    def unsubscribe(self, row, mode, depth):
        self.ad.unsubscribe(row["symbol"], row["exchange"], mode)

    def push(self, socket, frame):
        ad = self.ad

        def end_of_round(*_):
            ad.running = False

        self.web["time"].sleep = end_of_round
        ad.running = True
        ad._mock_streaming_loop()
