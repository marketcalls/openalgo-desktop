# WebSocket Streaming

OpenAlgo Desktop streams live market data and order updates over a WebSocket,
with the same protocol as OpenAlgo web: the Python SDK, Amibroker, Excel and
your own scripts connect unchanged.

This page covers connecting, authenticating, subscribing, the error frame and
how a connection ends. The data each mode sends is on its own page:

- [LTP](./ltp.md) - mode 1, the last traded price
- [Quote](./quote.md) - mode 2, prices, OHLC and volume
- [Depth](./depth.md) - mode 3, the order book, 5 to 50 levels
- [Order Updates](./order-updates.md) - fills, rejections and cancellations of your orders

## WebSocket URL

```
ws://127.0.0.1:8765
```

8765 is the default Market data port. If you changed it in Server Settings,
use your port.

- **From another device.** Turn on **Allow access from other devices** in
  Server Settings and connect to `ws://<this computer's IP address>:8765`.
  Use the IP address, not the computer's name: a name is refused.
- **Browser pages.** A page may open the feed only if it is the app's own page
  (`http://127.0.0.1:5000` or `http://localhost:5000`) or comes from the public
  address you set as Host Server URL or WebSocket URL on the Profile page.
  Other pages (for example a dashboard you serve from another port) are
  refused. Programs that send no `Origin` header (the Python SDK, Amibroker,
  Excel, scripts) are not affected. OpenAlgo web does not check the page.

## The Connection, Step by Step

1. Connect.
2. Send `authenticate` within 15 seconds, or the connection is closed with
   code 4401 "auth timeout".
3. Subscribe to symbols in a mode. Each subscription is acknowledged.
4. Receive `market_data` frames as prices change.
5. Unsubscribe, or simply close the connection.

Every message is a JSON object with an `action` (`type` is accepted as an
alias). An optional `request_id` on `subscribe`, `unsubscribe` and
`subscribe_orders` is echoed in the acknowledgement and in their error frames.

| action | Before authenticating | What it does |
|--------|----------------------|--------------|
| `authenticate` (alias `auth`) | Yes | Checks your API key |
| `ping` | Yes | Answers `pong` |
| `subscribe` | No | Starts streaming symbols in a mode |
| `unsubscribe` | No | Stops streaming symbols in a mode |
| `unsubscribe_all` | No | Stops every subscription of this connection |
| `subscribe_orders` / `unsubscribe_orders` | No | Starts or stops [order updates](./order-updates.md) |
| `get_broker_info` | No | The logged-in broker and whether its feed is connected |
| `get_supported_brokers` | Yes | Every broker name OpenAlgo knows |

Before authenticating, the actions marked No answer
`{"status": "error", "code": "NOT_AUTHENTICATED", "message": "You must authenticate first"}`.

## Authenticate

```json
{"action": "authenticate", "api_key": "<APIKEY>"}
```

`apikey` is accepted in place of `api_key`.

```json
{
  "type": "auth",
  "status": "success",
  "message": "Authentication successful",
  "broker": "zerodha",
  "user_id": "<USER_ID>",
  "supported_features": {"ltp": true, "quote": true, "depth": true}
}
```

| Answer | Cause and fix |
|--------|---------------|
| `{"status": "error", "code": "AUTHENTICATION_ERROR", "message": "API key is required"}` | No key in the message. |
| `{"status": "error", "code": "AUTHENTICATION_ERROR", "message": "Invalid API key"}` | Wrong key. The connection stays open; send the right key. When wrong keys keep coming from the same caller, the connection is closed (4401) after this frame. |
| `{"status": "error", "code": "AUTHENTICATION_ERROR", "message": "OpenAlgo is locked. Sign in to OpenAlgo to stream market data: ..."}` | OpenAlgo has just started, or you logged out. Sign in to OpenAlgo, then authenticate again. |
| `{"status": "error", "code": "BROKER_ERROR", "message": "No broker configuration found for user"}` | The key is right but no broker is logged in. The connection counts as authenticated, but subscribes answer "Broker adapter not found". Log in to your broker, then connect and authenticate again. |

Authenticating again on the same connection is allowed.

## Subscribe

```json
{
  "action": "subscribe",
  "symbols": [
    {"symbol": "RELIANCE", "exchange": "NSE"},
    {"symbol": "NIFTY", "exchange": "NSE_INDEX"}
  ],
  "mode": 1,
  "request_id": "req-ltp-1"
}
```

| Field | Description | Default |
|-------|-------------|---------|
| symbols | Array of `{"symbol", "exchange"}`. A single `symbol` and `exchange` at the top level also works. | - |
| mode | `1`, `2` or `3`, or `"LTP"`, `"Quote"`, `"Depth"` in any case. A digit in quotes (`"2"`) or a decimal (`2.0`) is refused. | `"Quote"` |
| depth | Order book levels for Depth mode: 5, 20, 30 or 50. The old name `depth_level` is also read. | 5 |
| request_id | Any value, echoed back | - |

```json
{
  "type": "subscribe",
  "status": "success",
  "subscriptions": [
    {"symbol": "RELIANCE", "exchange": "NSE", "status": "success", "mode": "LTP", "depth": 5, "broker": "zerodha"},
    {"symbol": "NIFTY", "exchange": "NSE_INDEX", "status": "success", "mode": "LTP", "depth": 5, "broker": "zerodha"}
  ],
  "message": "Subscription processing complete",
  "broker": "zerodha",
  "request_id": "req-ltp-1"
}
```

`status` is `partial` when any symbol failed. A failed symbol has `status:
"error"` and a `message` instead of `mode` and `depth`:

| Item message | Cause and fix |
|--------------|---------------|
| `Token not found for FOOBARBAZ on NSE` | Not in the master contract (a wrong symbol, or a wrong exchange). |
| `This broker has no live market data feed in OpenAlgo Desktop, so nothing can be streamed.` | Your broker has no streaming feed in the desktop. |
| `The live market data feed did not start for this broker session. Log in to your broker again to resume streaming.` | The broker refused or failed the feed. |
| `Subscription limit of 3000 reached for this connection` | One connection holds at most 3000 subscriptions. |

A `success` means the symbol will be streamed: now, or as soon as the
broker's feed connects again.

### The `depth` in the acknowledgement

The acknowledgement names a depth for every mode, the way OpenAlgo web's
adapter for your broker names it. It is not always the number of levels you
will receive, so read the levels from the frames.

| Broker | LTP and Quote | Depth mode |
|--------|---------------|------------|
| Zerodha, Fyers, Upstox, Kotak, Groww, AliceBlue, Shoonya, Flattrade, Zebu, TradeSmart, Firstock, 5paisa, INDmoney, mstock, Paytm, Delta Exchange | The depth asked for | The depth asked for |
| Arrow, HDFC Sky, HDFC Securities, Rupeezy | `null` | The level streamed |
| IIFL Capital | 5 | 5 |
| Every other broker (Angel, Dhan, Definedge, Motilal, Nubra, Pocketful, Samco, Tradejini, the XTS brokers ...) | The depth asked for | The level streamed |

"The level streamed" is the largest level your broker streams that is not more
than you asked for, else its smallest. Which levels each broker streams is on
the [Depth](./depth.md) page.

## Unsubscribe

```json
{
  "action": "unsubscribe",
  "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}],
  "mode": "Quote",
  "request_id": "req-unsub-1"
}
```

Each symbol may carry its own `mode`; otherwise the message's `mode` is used,
and with neither, Quote (2).

```json
{
  "type": "unsubscribe",
  "status": "success",
  "message": "Unsubscription processing complete",
  "successful": [
    {"symbol": "RELIANCE", "exchange": "NSE", "mode": "Quote", "status": "success", "broker": "zerodha"}
  ],
  "failed": [],
  "broker": "zerodha",
  "request_id": "req-unsub-1"
}
```

- Unsubscribing a symbol that was not subscribed still reports success.
- A symbol with an invalid mode goes into `failed` with `"mode": null` and a
  `message`; `status` is then `partial` (or `error` when nothing succeeded).
- `{"action": "unsubscribe_all"}` stops everything this connection holds. The
  answer has `"type": "unsubscribe"` and lists every subscription in
  `successful`.
- Unsubscribing in one mode leaves the symbol's other modes streaming.

## Market Data Frames

```json
{
  "type": "market_data",
  "symbol": "RELIANCE",
  "exchange": "NSE",
  "mode": 1,
  "data": {
    "symbol": "RELIANCE",
    "exchange": "NSE",
    "mode": "ltp",
    "ltp": 1167.7,
    "ltt": 1791000285768,
    "timestamp": 1791000285768
  },
  "broker": "zerodha"
}
```

- The top-level `mode` is the number (1, 2, 3); `data.mode` is a lowercase
  label (`ltp`, `quote`, `full`), as OpenAlgo web's Zerodha frames carry it.
- Times are epoch **milliseconds**.
- A connection that holds a symbol in several modes receives one frame per
  mode it holds on each update: a Quote and an LTP subscription to the same
  symbol give a mode-2 and a mode-1 frame.
- A connection that reads too slowly gets the latest frame for each symbol and
  mode, not a backlog.

## Ping

```json
{"action": "ping", "timestamp": 1791000284000}
```

```json
{"type": "pong", "status": "success", "server_timestamp": 1791000284862, "client_timestamp": 1791000284000}
```

`server_timestamp` is epoch milliseconds. A `timestamp` you send comes back as
`client_timestamp`, and a `_pingId` comes back unchanged. The server also
sends WebSocket ping frames every 20 seconds; a client that sends nothing at
all, not even the replies to these pings, for more than 40 seconds is
dropped. Most WebSocket libraries answer these pings by themselves.

## Broker Information

```json
{"action": "get_broker_info"}
```

```json
{"type": "broker_info", "status": "success", "broker": "zerodha", "adapter_status": "connected", "user_id": "<USER_ID>"}
```

`adapter_status` is `connected` while a broker session is live, else
`disconnected`.

`{"action": "get_supported_brokers"}` answers
`{"type": "supported_brokers", "status": "success", "brokers": [...], "count": 37}`.
Do not rely on the order of the names.

## Error Frame

Errors have no `type` field:

```json
{"status": "error", "code": "INVALID_MODE", "message": "Invalid mode 9; expected 1 (LTP), 2 (Quote), or 3 (Depth)", "request_id": "req-badmode"}
```

| code | When |
|------|------|
| `NOT_AUTHENTICATED` | A subscribe, unsubscribe, order-update or broker-info action before authenticating |
| `AUTHENTICATION_ERROR` | A missing or wrong key, OpenAlgo locked, or (before a close) your session or key changed |
| `BROKER_ERROR` | No broker logged in |
| `INVALID_MODE` | A mode that is not 1, 2, 3, LTP, Quote or Depth |
| `INVALID_PARAMETERS` | No symbols (`At least one symbol must be specified`), a depth that is not a positive whole number (`Depth must be one of 5, 20, 30 or 50`), or an unsubscribe with neither symbols nor `unsubscribe_all` |
| `INVALID_ACTION` | An unknown action. `{}` answers `Invalid action: None`. |
| `INVALID_JSON` | A message that is not JSON (`Invalid JSON message`) |
| `SERVER_ERROR` | A message that is JSON but not an object; a list answers `'list' object has no attribute 'get'` |

These texts are the web's, word for word, so code that matches on them keeps
working.

## How a Connection Ends

| Close code | Reason | Cause and fix |
|-----------|--------|---------------|
| 4401 | `auth timeout` | No successful `authenticate` within 15 seconds. |
| 4401 | `authentication expired` | You signed out of OpenAlgo, a broker session ended or started (a broker logout or login, a broker switch, the daily 03:00 boundary), or the API key was regenerated. An `AUTHENTICATION_ERROR` frame comes first. Connect and authenticate again. |
| 4401 | `Invalid API key` | Wrong keys keep coming from the same caller. Fix the key and connect again. |
| 1008 | `too many messages` | The connection sent thousands of messages a second or megabytes of them. Subscribing every symbol in one message, or one per message in a loop, stays far inside the limit. |
| 1008 | `client too slow` | The client stopped reading its acknowledgements and order updates. |
| 1009 | `message too big` | A message over 1 MiB. |
| 1013 | `Too many connections` | Programs on this computer may hold 64 connections; every other caller shares 256, and one device on the network at most 32. |
| 1001 | `server shutting down` | OpenAlgo is quitting, or the Market data port or address changed. |

In the desktop, a connection that authenticated before you signed out,
switched brokers or regenerated the key is closed (4401). OpenAlgo web keeps
it open. The message budget (1008) is also a desktop addition.

## Python SDK

```python
from openalgo import api

client = api(
    api_key="<APIKEY>",
    host="http://127.0.0.1:5000",
    ws_url="ws://127.0.0.1:8765"
)
client.connect()
```

See [LTP](./ltp.md), [Quote](./quote.md) and [Depth](./depth.md) for the
subscribe calls.

More on how the desktop's feed differs from OpenAlgo web is in
[Moving from OpenAlgo web](../../user/moving-from-openalgo-web.md#the-market-data-feed).

---

**Back to**: [API Documentation](../README.md)
