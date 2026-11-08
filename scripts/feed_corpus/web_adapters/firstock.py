"""Firstock: the socket client's message handler (the login answer, V1 flat
and V2 `{"EX:TOKEN": {...}}` ticks) and the adapter's snapshot merge,
depth filter, per-mode normaliser and fan-out.

State as `FirstockWebSocketAdapter.subscribe` keeps it (firstock_adapter.py:
265-275): one subscription per OpenAlgo symbol, exchange and mode, with the
master's broker exchange and token; the snapshot is keyed by
`brexchange:token` (#1732) and dropped once nothing holds the scrip
(firstock_adapter.py:356-367). The web keeps the snapshots and the
subscriptions across a reconnect and subscribes again after the next login
answer (`_resubscribe_all`, `_process_pending_subscriptions`).
"""

import threading

from webkit import Glue, Source


class Firstock(Glue):
    broker = "firstock"
    sources = [
        Source("broker/firstock/streaming/firstock_websocket.py", "FirstockWebSocket", ["_on_message"]),
        Source(
            "broker/firstock/streaming/firstock_adapter.py",
            "FirstockWebSocketAdapter",
            [
                "_on_data",
                "_update_market_snapshot",
                "_filter_depth_data",
                "_process_market_data",
                "_normalize_market_data",
                "_extract_depth_data",
                "_process_order_update",
                "_process_position_update",
            ],
        ),
    ]

    def start(self):
        self.ad = self.new(
            "FirstockWebSocketAdapter",
            subscriptions={},
            market_snapshots={},
            publish_market_data=self.publish,
        )
        self.ws = self.new(
            "FirstockWebSocket",
            authenticated=False,
            is_running=True,
            user_id="AB1234",
            auth_token="jk",
            on_data=self.ad._on_data,
            on_message=None,
            _shutdown_event=threading.Event(),
            _process_pending_subscriptions=lambda: None,
            _safe_close_wsapp=lambda reason="": None,
        )

    @staticmethod
    def key(row, mode):
        return f"{row['symbol']}_{row['exchange']}_{mode}"

    def subscribe(self, row, mode, depth):
        self.ad.subscriptions[self.key(row, mode)] = {
            "symbol": row["symbol"],
            "exchange": row["exchange"],
            "brexchange": row["brexchange"],
            "token": str(row["token"]),
            "subscription_token": f"{row['brexchange']}:{row['token']}",
            "mode": mode,
            "depth_level": depth,
        }

    def unsubscribe(self, row, mode, depth):
        sub = self.ad.subscriptions.pop(self.key(row, mode), None)
        if sub and not any(
            s["subscription_token"] == sub["subscription_token"] for s in self.ad.subscriptions.values()
        ):
            self.ad.market_snapshots.pop(sub["subscription_token"], None)

    def reconnect(self, socket):
        self.ws.authenticated = False

    def push(self, socket, frame):
        self.ws._on_message(None, frame)
