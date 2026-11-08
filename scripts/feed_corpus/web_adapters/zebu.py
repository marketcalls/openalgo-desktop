"""Zebu: `zebu_adapter.py` (its own normalisers: no `last_trade_time` in
LTP, Depth's `total_traded_value` from `toi`) and `zebu_order_adapter.py`'s
`om` normaliser (`prd` or `pcode`; `norenordno` or `norenoordno`). The
cache survives a reconnect, as in `_resubscribe_all`.
"""

from web_adapters.noren_family import NorenGlue, normalisers, sources


class Zebu(NorenGlue):
    broker = "zebu"
    adapter_class = "ZebuWebSocketAdapter"
    order_class = "ZebuOrderUpdateAdapter"
    cache_state = {"_cache": {}, "_initialized_scrips": set()}
    sources = sources(
        "zebu",
        "ZebuWebSocketAdapter",
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
        "ZebuExchangeMapper",
        "ZebuOrderUpdateAdapter",
    )

    def adapter_state(self):
        return {"normalizers": normalisers(self.web)}
