"""The web's 8765 server, run offline (review R107-13, R107-14).

`websocket_proxy/server.py`'s own `WebSocketProxy` methods are lifted from
the web tree (`webkit.load`) and driven with in-memory clients: a client's
subscribe, unsubscribe, unsubscribe_all and order subscription go through
the web's `subscribe_client`, `unsubscribe_client` and
`subscribe_orders_client` (the adapter is asked on every subscribe request;
ownership is keyed by symbol, exchange and mode; the adapter releases a key
only when its last owner lets go); a client that leaves goes through
`cleanup_client`; and what the adapter publishes for a push goes through
the web's `zmq_listener`, whose fan-out (every client at a mode up to the
publication's, tagged with the client's mode) and order relay
(`_handle_order_update`) say which client gets which frame. Nothing here
re-implements those rules.

The adapter the server talks to is the case's glue (`webkit.Glue`), behind
`AdapterShim`. Each delivery is recorded with the publication it came from
and the held modes it serves (`serves`), which the harness consumes once
each (`harness/web.rs`).
"""

from __future__ import annotations

import asyncio
import json
from collections import defaultdict

from shadow import ShadowAdapter
from webkit import Source, load

USER = "<USER_ID>"

SOURCES = [
    Source(
        "websocket_proxy/mode_utils.py",
        None,
        ["_MODE_CANONICAL", "_MODE_BY_UPPER_LABEL", "normalize_mode", "normalize_mode_or_none"],
    ),
    Source(
        "websocket_proxy/server.py",
        "WebSocketProxy",
        [
            "subscribe_client",
            "unsubscribe_client",
            "_unsubscribe_owned_subscription",
            "_drop_subscription_ownership",
            "_matching_client_subscriptions",
            "_indexed_client_subscriptions",
            "_purge_client_subscription_ownership",
            "cleanup_client",
            "subscribe_orders_client",
            "unsubscribe_orders_client",
            "_handle_order_update",
            "zmq_listener",
            "send_message",
            "send_error",
        ],
    ),
]


class _MarketDataService:
    def process_market_data(self, _data):
        pass


class _Logger:
    """The server's logger: quiet, except that an exception the server
    would only log (its loops catch everything) stops the replay, so a
    replay that fails is never read as "the web sent nothing"."""

    def __getattr__(self, _name):
        return lambda *a, **k: None

    def exception(self, message, *args, **_kw):
        raise RuntimeError(f"the web server failed in the replay: {message % args if args else message}")


class _WebSocket:
    """A connected client: what the server sends it is recorded."""

    def __init__(self, server: "WebServer", client: str):
        self.server = server
        self.client = client

    async def send(self, text: str) -> None:
        self.server.sent.append((self.client, self.server.current, json.loads(text)))


class _Bus:
    """The ZeroMQ socket the server reads the adapter's publications from:
    the push's publications in order, then the server stops."""

    def __init__(self, server: "WebServer"):
        self.server = server
        self.queue: list[tuple[int, bytes, bytes]] = []

    async def recv_multipart(self):
        if not self.queue:
            self.server.proxy.running = False
            raise TimeoutError
        index, topic, data = self.queue.pop(0)
        self.server.current = index
        return [topic, data]


class AdapterShim:
    """The broker adapter the web server calls, played by the case's glue."""

    def __init__(self, glue):
        self.glue = glue
        # The depth the adapter last subscribed each key at: the web
        # adapter's own record, which its unsubscribe (no depth) uses.
        self.depth: dict[tuple, int] = {}
        self.shadow = ShadowAdapter(glue.web_root, glue.broker, glue.master)
        # Per adapter call of a send: whether its answer is the web adapter's.
        self.answers: list[bool] = []

    def _answer(self, reply, shadow_reply) -> dict:
        """The glue's reply if it ran the adapter's method, else the method's
        reply on the shadow adapter. Without either the replay needs an
        answer to go on, and the send is recorded as having no web reply."""
        for candidate in (reply, shadow_reply):
            if isinstance(candidate, dict):
                self.answers.append(True)
                return candidate
        self.answers.append(False)
        return {"status": "success"}

    def _row(self, symbol, exchange):
        return self.glue.master.by_symbol(symbol, exchange)

    def subscribe(self, symbol, exchange, mode, depth_level):
        row = self._row(symbol, exchange)
        if row is None:
            return self._answer(None, self.shadow.subscribe(symbol, exchange, mode, int(depth_level)))
        self.depth[(symbol, exchange, mode)] = int(depth_level)
        reply = self.glue.subscribe(row, mode, int(depth_level))
        shadow = None if isinstance(reply, dict) else self.shadow.subscribe(symbol, exchange, mode, int(depth_level))
        return self._answer(reply, shadow)

    def unsubscribe(self, symbol, exchange, mode):
        row = self._row(symbol, exchange)
        if row is None:
            return self._answer(None, self.shadow.unsubscribe(symbol, exchange, mode))
        reply = self.glue.unsubscribe(row, mode, self.depth.get((symbol, exchange, mode), 5))
        shadow = None if isinstance(reply, dict) else self.shadow.unsubscribe(symbol, exchange, mode)
        return self._answer(reply, shadow)

    def unsubscribe_all(self):
        return {"status": "success"}

    def disconnect(self):
        self.glue.adapter_disconnected = True


class WebServer:
    """One case's web server, with the case's glue as its adapter."""

    def __init__(self, glue):
        self.glue = glue
        ns = load(
            glue.web_root,
            SOURCES,
            {"get_market_data_service": _MarketDataService, "websockets": None, "zmq": None},
        )
        ns["logger"] = _Logger()
        cls = ns["WebSocketProxy"]
        p = cls.__new__(cls)
        p.clients = {}
        p.user_mapping = {}
        p.broker_adapters = {}
        p.user_broker_mapping = {}
        p.subscriptions = {}
        p.subscription_index = defaultdict(set)
        p.order_subscribers = defaultdict(set)
        p.last_message_time = {}
        p.last_tick_time = {}
        p._messages_processed = 0
        p.running = False
        p.socket = _Bus(self)
        p._cleanup_stale_throttle_entries = lambda: None
        p._log_stale_adapters = lambda: None
        p._handle_cache_invalidation = lambda *_: None
        self.proxy = p
        self.adapter = AdapterShim(glue)
        self.sent: list[tuple[str, int | None, dict]] = []
        self.current: int | None = None
        glue.adapter_disconnected = False

    def _connect(self, client: str) -> None:
        """A client authenticated, as `authenticate_client` leaves it: its
        user's adapter, made again if the last client's leaving ended it."""
        p = self.proxy
        if client in p.clients:
            return
        p.clients[client] = _WebSocket(self, client)
        p.user_mapping[client] = USER
        if USER not in p.broker_adapters:
            if self.glue.adapter_disconnected:
                self.glue.start()
                self.glue.adapter_disconnected = False
                self.adapter = AdapterShim(self.glue)
            p.broker_adapters[USER] = self.adapter
            p.user_broker_mapping[USER] = self.glue.broker

    def send(self, client: str, message: dict) -> dict:
        """A client's message, through the web server's own handler: the
        frames the server answers it with, and whether every adapter call
        behind them was answered by the web adapter's own method."""
        self._connect(client)
        p = self.proxy
        action = message.get("action") or message.get("type")
        self.current = None
        self.sent = []
        self.adapter.answers = []
        if action == "subscribe":
            asyncio.run(p.subscribe_client(client, message))
        elif action in ("unsubscribe", "unsubscribe_all"):
            asyncio.run(p.unsubscribe_client(client, message))
        elif action == "subscribe_orders":
            asyncio.run(p.subscribe_orders_client(client, message))
        elif action == "unsubscribe_orders":
            asyncio.run(p.unsubscribe_orders_client(client, message))
        return {
            "frames": [m for c, index, m in self.sent if c == client and index is None],
            "from_adapter": all(self.adapter.answers),
        }

    def close(self, client: str) -> None:
        """A client leaves: the web server's `cleanup_client`."""
        if client in self.proxy.clients:
            asyncio.run(self.proxy.cleanup_client(client))

    def deliver(self, published: list) -> list[dict]:
        """What the web server sends each client for the adapter's
        `published` frames, in order: one entry per frame a client gets,
        with the publication it came from and the held modes it serves."""
        bus = self.proxy.socket
        for i, pub in enumerate(published):
            if pub["kind"] == "market":
                topic = pub["topic"]
                data = pub["data"]
            else:
                topic = f"{self.glue.broker.upper()}_{USER}_orders"
                data = dict(pub["frame"], user_id=USER)
            bus.queue.append((i, topic.encode(), json.dumps(data).encode()))
        self.sent = []
        self.proxy.running = True
        asyncio.run(self.proxy.zmq_listener())
        out = []
        for client, index, message in self.sent:
            if index is None:
                continue
            pub = published[index]
            if pub["kind"] == "market":
                out.append({"client": client, "publication": index, "mode": message["mode"],
                            "topic_mode": pub["mode"], "broker": message.get("broker")})
            else:
                out.append({"client": client, "publication": index, "order": True})
        # The server fans out from Python sets, whose order changes from run
        # to run; a client's own frames keep their publication order.
        out.sort(key=lambda d: (d["publication"], d["client"]))
        return self._serves(out, published)

    def _serves(self, deliveries: list[dict], published: list) -> list[dict]:
        """Each delivery's `serves`: the held modes of its client on its
        instrument for which this is the stream the desktop answers with
        one frame per held mode (C08: the publications of that mode, else
        of the lowest richer mode published); the web's other fan-out to a
        lower-mode holder serves none (`one-frame-per-held-mode`)."""
        index = self.proxy.subscription_index
        by_key = defaultdict(set)
        for d in deliveries:
            if "order" not in d:
                pub = published[d["publication"]]
                by_key[(d["client"], pub["symbol"], pub["exchange"])].add(d["topic_mode"])
        for d in deliveries:
            if "order" in d:
                d["serves"] = ["order"]
                continue
            pub = published[d["publication"]]
            key = (d["client"], pub["symbol"], pub["exchange"])
            held = [m for m in (1, 2, 3) if d["client"] in index.get((pub["symbol"], pub["exchange"], m), ())]
            topics = sorted(by_key[key])
            serves = []
            for h in held:
                stream = h if h in topics else next((t for t in topics if t > h), None)
                if stream == d["topic_mode"]:
                    serves.append(h)
            d["serves"] = serves
        return deliveries
