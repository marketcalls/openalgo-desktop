"""Flattrade: `flattrade_adapter.py` (its own normalisers: no
`last_trade_time` in LTP, Depth's `total_traded_value` from `toi`) and
`flattrade_order_adapter.py`'s `om` normaliser (`pcode` before `prd`).

The web's default Flattrade order stream is REST polling
(`create_flattrade_order_adapter`, issue #1806: PiConnect allows one
session per uid); the desktop multiplexes `t=o` on the market socket, the
fix that module names. The `om` frames are compared with the normaliser
the dedicated socket uses. The cache survives a reconnect, as in
`_resubscribe_all`.
"""

from web_adapters.noren_family import NorenGlue, normalisers, sources


class Flattrade(NorenGlue):
    broker = "flattrade"
    adapter_class = "FlattradeWebSocketAdapter"
    order_class = "FlattradeOrderUpdateAdapter"
    cache_state = {"_cache": {}, "_initialized_scrips": set()}
    sources = sources(
        "flattrade",
        "FlattradeWebSocketAdapter",
        [
            "_on_message",
            "_process_market_message",
            "_resolve_scrip",
            "_get_symbol_info",
            "_find_matching_subscriptions",
            "_should_process_message",
            "_process_subscription_message",
            "_normalize_market_data",
            "_create_subscription",
            "_index_subscription",
            "_deindex_scrip",
        ],
        ["LTPNormalizer", "QuoteNormalizer", "DepthNormalizer"],
        "FlattradeExchangeMapper",
        "FlattradeOrderUpdateAdapter",
    )

    def adapter_state(self):
        return {"normalizers": normalisers(self.web)}
