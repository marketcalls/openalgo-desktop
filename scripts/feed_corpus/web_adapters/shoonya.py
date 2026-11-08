"""Shoonya: `shoonya_adapter.py` (its own normalisers: `last_trade_time` in
LTP too, Depth's `open_interest` from `toi`) and `shoonya_order_adapter.py`.

Shoonya's adapter routes through `_scrip_to_cids` beside `scrip_to_symbol`
and `_token_to_scrips` (`subscribe`, shoonya_adapter.py:534-547), releases
through `_remove_subscription_locked`, and clears its whole cache when it
resubscribes after a reconnect (`_resubscribe_all`, :1293). Its order
normaliser reads the product from `prd` only.
"""

from web_adapters.noren_family import InsertionOrderSet, NorenGlue, normalisers, sources


class Shoonya(NorenGlue):
    broker = "shoonya"
    adapter_class = "ShoonyaWebSocketAdapter"
    order_class = "ShoonyaOrderUpdateAdapter"
    cache_state = {"_cache": {}, "_initialized_scrips": set()}
    sources = sources(
        "shoonya",
        "ShoonyaWebSocketAdapter",
        [
            "_on_message",
            "_process_market_message",
            "_resolve_scrip_locked",
            "_should_process_message",
            "_process_subscription_message",
            "_normalize_market_data",
            "_create_subscription",
            "_remove_subscription_locked",
        ],
        ["LTPNormalizer", "QuoteNormalizer", "DepthNormalizer"],
        "ShoonyaExchangeMapper",
        "ShoonyaOrderUpdateAdapter",
    )

    def adapter_state(self):
        return {"_scrip_to_cids": {}, "normalizers": normalisers(self.web)}

    def subscribe(self, row, mode, depth):
        # shoonya_adapter.py:534-547: the subscription and its three indexes.
        cid, sub = self.subscription(row, mode, depth)
        scrip = sub["scrip"]
        self.ad.subscriptions[cid] = sub
        self.ad.scrip_to_symbol[scrip] = (sub["symbol"], sub["exchange"])
        self.ad._scrip_to_cids.setdefault(scrip, InsertionOrderSet()).add(cid)
        self.ad._token_to_scrips.setdefault(sub["token"], set()).add(scrip)

    def unsubscribe(self, row, mode, depth):
        # shoonya_adapter.py:641-646, once the broker send has succeeded.
        cid, sub = self.owner(row, mode)
        scrip = self.ad._remove_subscription_locked(cid, sub)
        if scrip:
            self.ad.market_cache.clear(scrip)

    def reconnect(self, socket):
        # shoonya_adapter.py:1293 (SA-15): the stale cache goes on resubscribe.
        self.ad.market_cache.clear()
