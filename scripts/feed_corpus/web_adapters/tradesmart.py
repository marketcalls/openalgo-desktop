"""TradeSmart: `tradesmart_adapter.py` (one inline normaliser: Quote and
Depth carry the totals and the native `oi`, never `toi`) and
`tradesmart_order_adapter.py`'s `om` normaliser. TradeSmart pushes order
updates after the login without a `t=o`. The cache survives a reconnect,
as in `_resubscribe_all`.
"""

from web_adapters.noren_family import NorenGlue, sources


class TradeSmart(NorenGlue):
    broker = "tradesmart"
    adapter_class = "TradeSmartWebSocketAdapter"
    order_class = "TradeSmartOrderUpdateAdapter"
    cache_state = {"_cache": {}}
    sources = sources(
        "tradesmart",
        "TradeSmartWebSocketAdapter",
        [
            "_on_message",
            "_process_market_message",
            "_resolve_scrip",
            "_should_process_message",
            "_publish_subscription",
            "_normalize_market_data",
            "_create_subscription",
            "_index_subscription",
            "_deindex_scrip",
        ],
        [],
        "TradeSmartExchangeMapper",
        "TradeSmartOrderUpdateAdapter",
    )
