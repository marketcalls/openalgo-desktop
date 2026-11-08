"""The Noren family (Shoonya, Flattrade, Zebu, TradeSmart): each member's own
market data adapter (its scrip-keyed cache merge, its normalisers, its
routing by exchange and token) and its own order adapter's `om`
normaliser, run on one multiplexed socket as the desktop runs them.

The members share the protocol but not the code: each subclass names its
own files, classes and routing methods, so a difference between members
(Shoonya's `open_interest` from `toi`, Flattrade's and Zebu's
`total_traded_value`, TradeSmart's native `oi`, which member puts
`last_trade_time` in LTP) comes out of that member's web code.

State as each adapter's `subscribe` keeps it (`subscriptions` by
correlation id, `scrip_to_symbol`, `_token_to_scrips`, and Shoonya's
`_scrip_to_cids`); a release as its `unsubscribe` does it, cache clear
included. A reconnect clears the cache only where the web's does
(Shoonya's `_resubscribe_all`). `to_openalgo_symbol` reads the case's
symbol master in place of the database.
"""

import threading

from webkit import Glue, Source

# The order adapter's module-level tables and helper, the same names in
# every member's `<member>_order_adapter.py`.
ORDER_TABLES = ["_STATUS_MAP", "_REPORTTYPE_FALLBACK", "_PRICETYPE_MAP", "_PRODUCT_MAP", "_ACTION_MAP", "_noren_text"]


class InsertionOrderSet:
    """A set that iterates in insertion order, for an index the web keeps as
    a `set` and iterates to publish (Shoonya's `_scrip_to_cids`): one scrip
    held at two modes then publishes in subscription order on every run,
    not in the order of this process's string hashes, so `web.json` is the
    same every time it is computed."""

    def __init__(self):
        self._items = {}

    def add(self, item):
        self._items[item] = None

    def discard(self, item):
        self._items.pop(item, None)

    def __iter__(self):
        return iter(list(self._items))

    def __len__(self):
        return len(self._items)

    def __contains__(self, item):
        return item in self._items


class NorenGlue(Glue):
    """A member names `adapter` (its market data adapter file), `adapter_class`,
    `routing` (the adapter methods that route, merge and normalise a frame),
    `cache_state` (what its MarketDataCache holds), `order_file` and
    `order_class`; `sources` lists them with its normalisers and mapper."""

    adapter_class = ""
    order_class = ""
    cache_state: dict = {}

    def stubs(self):
        return {"to_openalgo_symbol": self.to_openalgo_symbol}

    def start(self):
        self.next_id = 0
        state = {k: (set() if isinstance(v, set) else dict(v)) for k, v in self.cache_state.items()}
        cache = self.new("MarketDataCache", _lock=threading.Lock(), **state)
        self.ad = self.new(
            self.adapter_class,
            market_cache=cache,
            subscriptions={},
            scrip_to_symbol={},
            _token_to_scrips={},
            ws_subscription_refs={},
            zmq_port=5555,
            publish_market_data=self.publish,
            **self.adapter_state(),
        )
        self.orders = self.new(self.order_class, _ws=None)

    def adapter_state(self) -> dict:
        """What else the member's adapter reads on the frame path."""
        return {}

    def subscription(self, row, mode, depth):
        token_info = {"token": str(row["token"]), "brexchange": row["brexchange"]}
        sub = self.ad._create_subscription(row["symbol"], row["exchange"], mode, depth, token_info)
        self.next_id += 1
        return f"{row['symbol']}_{row['exchange']}_{mode}_{self.next_id}", sub

    def subscribe(self, row, mode, depth):
        cid, sub = self.subscription(row, mode, depth)
        self.ad.subscriptions[cid] = sub
        self.ad._index_subscription(sub)

    def owner(self, row, mode):
        prefix = f"{row['symbol']}_{row['exchange']}_{mode}_"
        return next((c, s) for c, s in self.ad.subscriptions.items() if c.startswith(prefix))

    def unsubscribe(self, row, mode, depth):
        cid, sub = self.owner(row, mode)
        del self.ad.subscriptions[cid]
        if not any(s["scrip"] == sub["scrip"] for s in self.ad.subscriptions.values()):
            self.ad._deindex_scrip(sub)
            self.ad.market_cache.clear(sub["scrip"])

    def push(self, socket, frame):
        # One socket carries market data and `om` order updates, as on the
        # desktop; each frame goes through the market adapter and the order
        # adapter's normaliser, and each ignores the other's frames.
        self.ad._on_message(None, frame)
        fields = self.orders.normalize(frame)
        if fields:
            self.order(fields)


def normalisers(web: dict) -> dict:
    """The adapter's `normalizers` by mode, as its `_setup_normalizers` builds them."""
    c = web["Config"]
    return {
        c.MODE_LTP: web["LTPNormalizer"](),
        c.MODE_QUOTE: web["QuoteNormalizer"](),
        c.MODE_DEPTH: web["DepthNormalizer"](),
    }


def sources(member: str, adapter_class: str, routing: list[str], normalisers: list[str], mapper: str, order_class: str):
    """The web files a member's glue lifts."""
    base = f"broker/{member}/streaming/{member}"
    return [
        Source(f"{base}_adapter.py", None, ["safe_float", "safe_int"]),
        Source(f"{base}_adapter.py", "Config"),
        Source(f"{base}_adapter.py", "MarketDataCache"),
        *[Source(f"{base}_adapter.py", n) for n in normalisers],
        Source(f"{base}_mapping.py", mapper),
        Source(f"{base}_adapter.py", adapter_class, routing),
        Source(f"{base}_order_adapter.py", None, ORDER_TABLES),
        Source(f"{base}_order_adapter.py", order_class, ["normalize"]),
    ]
