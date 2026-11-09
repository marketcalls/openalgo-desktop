"""The Symphony XTS family: one glue for the seven members, each run on its
own web files (`broker/<member>/streaming/`), because they differ where it
matters.

- ibulls, iifl, wisdom and compositedge are one template: the websocket
  client hands each Socket.IO event's payload to the adapter, which takes
  the topic from its `MessageCode` (1512 LTP, 1501 Quote, 1502 Depth;
  anything else, a 1105 text packet included, is dropped), reads the quote
  from `Touchline` for 1502 only, and disconnects the whole adapter after
  any unsubscribe (`*_adapter.py` `unsubscribe`).
- fivepaisaxts is the template with its fixes: the 1105 filter compares
  normalised keys, and an unsubscribe no longer disconnects.
- jainamxts supplies a missing `MessageCode` from the event name
  (`_process_xts_data`), marks 1105 text as 1512, and publishes every
  message to each mode its subscriptions hold (`_process_json_data`).
- rmoney sets `MessageCode` from the event name, takes 1105 text as 1512,
  reads camelCase keys and segment names, takes the quote from `Touchline`
  for every code, finds a book in `Bids`/`Asks` or `BidInfo`/`AskInfo`
  (root or `Touchline`, either side), and serves LTP from 1501.

Run offline: the client's own `_setup_socketio` registers its handlers on
a stand-in `socketio.Client` that records them, and a pushed `42[...]`
frame is dispatched to the handler for its event (or the catch-all), as
python-socketio does. Subscribing records what the adapter's `subscribe`
keeps (its `subscriptions` entry, `*_adapter.py` `subscribe`) and calls the
client's own `subscribe`, whose REST call answers success with no snapshot.
`get_symbol` is the case's master. Engine.IO and Socket.IO control frames
(`0`, `40`, `2`) publish nothing.
"""

from __future__ import annotations

import json
import threading

from webkit import Glue, Source

# The adapter methods that turn a payload into what the web publishes.
ADAPTER_METHODS = [
    "_on_data",
    "_process_json_data",
    "_process_binary_data",
    "_normalize_market_data",
    "_extract_depth_data",
    "_is_index_token",
]


class _Client:
    """`socketio.Client`: keeps the handlers `on` registers."""

    def __init__(self, *args, **kwargs):
        self.handlers = {}

    def on(self, event, handler=None):
        self.handlers[event] = handler


class _SocketIO:
    Client = _Client


class _Answer:
    """The XTS subscription answer: success, no snapshot."""

    status_code = 200
    text = '{"type":"success","result":{"listQuotes":[]}}'

    def json(self):
        return json.loads(self.text)

    def close(self):
        pass


class _Http:
    """`requests` (and rmoney's `requests.Session`): every call answers success."""

    def post(self, *args, **kwargs):
        return _Answer()

    def put(self, *args, **kwargs):
        return _Answer()


def sources(member: str, prefix: str) -> list[Source]:
    """The member's mapping, websocket client and adapter, by its class prefix."""
    base = f"broker/{member}/streaming/{member}"
    return [
        Source(f"{base}_mapping.py", f"{prefix}ExchangeMapper"),
        Source(f"{base}_websocket.py", f"{prefix}WebSocketClient"),
        Source(f"{base}_adapter.py", f"{prefix}WebSocketAdapter", ADAPTER_METHODS),
    ]


class XtsGlue(Glue):
    prefix = ""
    # Whether `unsubscribe` ends the whole adapter (`self.disconnect()`).
    disconnects_on_unsubscribe = True
    # Whether the adapter sends the instrument id as a number (rmoney) or
    # as the token's string.
    numeric_instrument_id = False

    def stubs(self):
        def get_symbol(token, exchange):
            row = self.master.by_token(token, exchange)
            return row["symbol"] if row else None

        return {
            "socketio": _SocketIO,
            "requests": _Http(),
            "get_symbol": get_symbol,
            "BASE_URL": "https://xts.invalid",
            "MARKET_DATA_URL": "https://xts.invalid/apimarketdata",
        }

    def start(self):
        self.dead = False
        self.ad = self.new(
            f"{self.prefix}WebSocketAdapter",
            subscriptions={},
            publish_market_data=self.publish,
            connected=True,
            running=True,
            zmq_port=5555,
        )
        self.ws = self.new(
            f"{self.prefix}WebSocketClient",
            subscriptions={},
            connected=True,
            running=True,
            market_data_token="<token>",
            last_message_time=0.0,
            on_open=None,
            on_close=None,
            on_error=None,
            on_message=None,
            on_data=lambda _client, data: self.ad._on_data(None, data),
            _http_session=_Http(),
            _subscription_http_lock=threading.Lock(),
            subscription_url="https://xts.invalid/apimarketdata/instruments/subscription",
            _binary_packet_seen=False,
        )
        self.ws._setup_socketio()
        self.ad.ws_client = self.ws

    def instrument(self, row: dict) -> dict:
        seg = self.web[f"{self.prefix}ExchangeMapper"].get_exchange_type(row["brexchange"])
        token = str(row["token"]).strip()
        if self.numeric_instrument_id and token.isdigit():
            token = int(token)
        return {"exchangeSegment": seg, "exchangeInstrumentID": token}

    def subscribe(self, row, mode, depth):
        cid = f"{row['symbol']}_{row['exchange']}_{mode}"
        if mode == 3:
            cid = f"{cid}_{depth}"
        instruments = [self.instrument(row)]
        self.ad.subscriptions[cid] = {
            "symbol": row["symbol"],
            "exchange": row["exchange"],
            "brexchange": row["brexchange"],
            "token": row["token"],
            "mode": mode,
            "depth_level": depth,
            "instruments": instruments,
        }
        self.ws.subscribe(cid, mode, instruments)

    def unsubscribe(self, row, mode, depth):
        cid = f"{row['symbol']}_{row['exchange']}_{mode}"
        for key in [k for k in self.ad.subscriptions if k == cid or k.startswith(cid + "_")]:
            del self.ad.subscriptions[key]
            if key in self.ws.subscriptions:
                self.ws.unsubscribe(key, mode, [self.instrument(row)])
        if self.disconnects_on_unsubscribe:
            # The adapter's `disconnect()`: running and connected go false,
            # nothing reconnects, nothing more is published.
            self.dead = True

    def push(self, socket, frame):
        if self.dead or not isinstance(frame, str) or not frame.startswith("42"):
            return
        name, *args = json.loads(frame[2:])
        handlers = self.ws.sio.handlers
        if name in handlers:
            handlers[name](*args)
        elif "*" in handlers:
            handlers["*"](name, *args)
