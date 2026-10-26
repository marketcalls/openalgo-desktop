# WebSockets

## OpenAlgo Desktop WebSocket protocol

### Overview

OpenAlgo Desktop streams real-time market data and order updates over a
WebSocket, using the same protocol as OpenAlgo web. A client written for the
web feed (the Python SDK, a trading platform, your own script, the app's own
pages) connects to the desktop unchanged: the same `authenticate`,
`subscribe`, `unsubscribe` and `unsubscribe_all` messages, the same modes
(LTP, Quote, Depth), the same `market_data` and `order_update` frames, the same
error frame and the same 4401 close for a client that never authenticates.

The feed is broker-agnostic. You subscribe with OpenAlgo symbols (see
[Symbol format](symbol-format.md)), whichever broker is connected, and every
frame carries OpenAlgo symbols back.

Where the desktop behaves differently from the web, this page says so in a
line starting "In the desktop". The list is collected at the end under
[Differences from OpenAlgo web](#differences-from-openalgo-web).

### WebSocket URL

```
ws://127.0.0.1:8765
```

This is the default, the same as OpenAlgo web. The feed listens on this
computer only (loopback) unless you choose otherwise.

- **Changing the port or address.** Open **Server Settings** and change
  **Market data port** (and, if needed, **Market data address**). After you
  save, the feed moves to the new port within a couple of seconds. Clients
  connected to the old port are closed (code 1001) and must connect to the new
  one. Update the WebSocket URL in your scripts and platforms.
- **Port already in use.** If another program holds the port (for example
  OpenAlgo web running on the same computer), Server Settings shows "Live
  market data is not running" with the cause and the fix, and the Health
  Monitor raises an alert. The feed starts by itself as soon as the port is
  free. See [Troubleshooting](../user/troubleshooting.md#the-app-says-a-port-is-already-in-use).
- **Other devices on your network.** Turn on **Allow access from other
  devices** in Server Settings. Other devices then connect to
  `ws://<this computer's IP address>:8765`. See
  [Another device, a firewall, or a tunnel](../user/troubleshooting.md#another-device-a-firewall-or-a-tunnel).
- **Through a tunnel.** The feed accepts a connection that names the tunnel
  address you saved as **WebSocket URL** or **Host Server URL** under
  **Server Configuration** on the Broker tab in **Profile**.

In the desktop, the feed itself is plain `ws://`. There is no
`wss://yourdomain.com/ws` address: that is the nginx setup of a web server
install. Encryption is only available through a tunnel that provides it.

**Who may open the feed.** A program that sends no `Origin` header (the
Python SDK, Amibroker, Excel, your own scripts) may connect; it still has to
authenticate with the API key. A browser page may connect only if it is one
of the app's own pages (or the configured tunnel address). A page from any
other site, or a request naming a host that is not this computer, is refused
during the handshake with HTTP 403. This guards against a web page you visit
reading your feed.

### Authentication

Every session starts with the API key from the **API Key** page:

```json
{
  "action": "authenticate",
  "api_key": "<APIKEY>"
}
```

The web's alias forms are accepted too: `"type": "auth"` in place of
`"action": "authenticate"`, and `apikey` in place of `api_key`.

On success:

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

`broker` is the connected broker's id; `user_id` is your OpenAlgo username.

When authentication does not succeed, the server answers with an
[error frame](#error-frame):

| Situation | `code` | `message` |
| --- | --- | --- |
| No key sent | `AUTHENTICATION_ERROR` | `API key is required` |
| Wrong key | `AUTHENTICATION_ERROR` | `Invalid API key` |
| Right key, no broker logged in | `BROKER_ERROR` | `No broker configuration found for user` |
| OpenAlgo Desktop is locked | `AUTHENTICATION_ERROR` | `OpenAlgo is locked. Sign in to OpenAlgo to stream market data: the feed is off after OpenAlgo starts and after you log out, until you sign in again.` |

- **Wrong key.** The connection stays open, as on the web, so the client can
  try again. In the desktop, failed keys count against the same failure
  budget as `/api/v1` and MCP for the caller (this computer, the tunnel, or
  one device on your network). Once that budget is spent, a failed
  `authenticate` is answered and the connection is closed (code 4401). A
  correct key is never refused because of it.
- **No broker logged in.** As on the web, the connection counts as
  authenticated from here on, but every subscribe is answered with
  `BROKER_ERROR` `Broker adapter not found`. When you then log in to your
  broker, the desktop closes this connection (see below); connect and
  authenticate again.
- **Locked.** In the desktop, the keys are derived from your OpenAlgo
  password, so after the app starts, and after you log out of OpenAlgo, no key
  can be checked until you sign in to the app again. Nothing counts against
  you while it is locked. The connection stays unauthenticated and is closed
  by the 15-second rule below; connect again after signing in.

**Authenticate within 15 seconds.** A connection that has not authenticated
15 seconds after it opened is closed with code `4401` and reason
`auth timeout`, as on the web.

**Authentication ends when your credentials change.** In the desktop, an
authenticated connection is sent this error frame and then closed with code
`4401` (reason `authentication expired`) when you sign out of OpenAlgo, when
the broker session ends (broker logout, the daily session end around 03:00 IST,
switching brokers) or a new one starts, and when the API key is regenerated:

```json
{
  "status": "error",
  "code": "AUTHENTICATION_ERROR",
  "message": "Your OpenAlgo session ended or the API key changed. Connect again and authenticate with the current API key."
}
```

Connect again and authenticate. The Python SDK does this by itself
(`auto_reconnect=True`, the default). The web keeps such connections open.

### Data modes

| Mode | Label | What it carries |
| --- | --- | --- |
| 1 | `LTP` | Last traded price and time |
| 2 | `Quote` | LTP plus OHLC, volume and order-flow fields |
| 3 | `Depth` | Quote fields plus the buy and sell order book |

`mode` may be sent as the number (`1`, `2`, `3`) or as the label, in any
case (`"ltp"`, `"QUOTE"`, `"Depth"`), as on the web. A number in a string
(`"2"`), a decimal (`2.0`), `true` or `null` is refused with `INVALID_MODE`:

| Sent | `message` |
| --- | --- |
| `9` | `Invalid mode 9; expected 1 (LTP), 2 (Quote), or 3 (Depth)` |
| `"2"` | `Invalid mode '2'; expected 'LTP', 'Quote', or 'Depth' (case-insensitive)` |
| `2.0` | `Mode must be int or str, got float` |

Acknowledgements name the mode by its label (`"LTP"`, `"Quote"`, `"Depth"`).
`market_data` frames carry the number.

### Subscription format

#### Basic subscription

```json
{
  "action": "subscribe",
  "symbols": [
    {"symbol": "RELIANCE", "exchange": "NSE"},
    {"symbol": "NIFTY", "exchange": "NSE_INDEX"}
  ],
  "mode": "LTP",
  "request_id": "req-ltp-1"
}
```

- `mode` defaults to `Quote` when left out.
- `request_id` is optional; when sent, it is echoed in the acknowledgement or
  the error frame, so a client can match answers to requests.
- A single instrument may be sent without the list:
  `{"action": "subscribe", "symbol": "SBIN", "exchange": "NSE", "mode": "LTP"}`.
- One message may carry every instrument you want. Subscribing in one message
  per instrument also works.

#### Depth subscription (with levels)

```json
{
  "action": "subscribe",
  "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}],
  "mode": "Depth",
  "depth": 20
}
```

`depth` defaults to 5. The older key `depth_level` is accepted when `depth`
is absent, as on the web.

#### Acknowledgement

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

There is one result per instrument. `status` is `success` only when every
instrument succeeded, `partial` when at least one failed. A failed item
carries a `message` instead of `mode` and `depth`:

```json
{
  "type": "subscribe",
  "status": "partial",
  "subscriptions": [
    {"symbol": "FOOBARBAZ", "exchange": "NSE", "status": "error", "message": "Token not found for FOOBARBAZ on NSE", "broker": "zerodha"}
  ],
  "message": "Subscription processing complete",
  "broker": "zerodha"
}
```

An instrument fails when:

- it is not in the master contract: `Token not found for <symbol> on <exchange>`
  (also for an unknown exchange);
- the broker has no live market data feed in the desktop, its feed did not
  start, or the broker refused the feed session. In the desktop, each item then
  says so, for example `This broker has no live market data feed in OpenAlgo
  Desktop, so nothing can be streamed.` or `The live market data feed did not
  start for this broker session. Log in to your broker again to resume
  streaming.`;
- the connection already holds 3000 subscriptions: `Subscription limit of 3000
  reached for this connection`. In the desktop, this limit is per connection;
  open a second connection if you need more.

An item with an empty `symbol` or `exchange` is skipped, as on the web.

A `success` item means the instrument will stream: now, or as soon as the
broker's feed connects again after an outage.

Subscribing again to an instrument you already hold in the same mode is
harmless; with a different `depth` it replaces the earlier depth.

Requests refused as a whole are answered with an error frame:

| Situation | `code` | `message` |
| --- | --- | --- |
| Not authenticated yet | `NOT_AUTHENTICATED` | `You must authenticate first` |
| Invalid `mode` | `INVALID_MODE` | see [Data modes](#data-modes) |
| No instrument given | `INVALID_PARAMETERS` | `At least one symbol must be specified` |
| `depth` not a positive whole number | `INVALID_PARAMETERS` | `Depth must be one of 5, 20, 30 or 50` |
| No broker logged in | `BROKER_ERROR` | `Broker adapter not found` |

In the desktop, a `depth` that is not a positive whole number (`0`, `-5`,
`2.5`, `"abc"`) is refused up front. Any other whole number is accepted and
served as described next.

#### Depth levels

The order book a Depth subscription streams depends on the broker and the
exchange:

| Broker | Levels streamed |
| --- | --- |
| Fyers | 50 on NSE and NFO (from Fyers' 50-level feed); 5 elsewhere |
| Dhan | 20 on NSE and NFO (from Dhan's 20-level feed); 5 elsewhere. Dhan Sandbox has no live feed. |
| Every other broker | 5 |

The feed streams the largest level the broker serves that is not above the
one you asked for, or the broker's smallest level when you asked for fewer.
On Fyers, `"depth": 20` or `30` therefore streams 5 levels and `50` streams
50; on Zerodha, every depth streams 5.

The `depth` in the acknowledgement is what that broker's web adapter answers,
so it can name more levels than are streamed: Zerodha, Fyers, Upstox, Kotak
and several others repeat the depth you asked for, while Angel, Dhan, the XTS
brokers and others report the level actually used. Arrow, both HDFC brokers
and Rupeezy answer an LTP or Quote subscribe with `"depth": null`, and IIFL
Capital always with 5. **Read the number of levels from the frames, not from
the acknowledgement.** The full list is in
[Moving from OpenAlgo web](../user/moving-from-openalgo-web.md#the-market-data-feed).

In the desktop, a depth level is never refused per instrument; the web
answers some brokers' unsupported depths with an item error.

### Unsubscription

```json
{
  "action": "unsubscribe",
  "symbols": [
    {"symbol": "RELIANCE", "exchange": "NSE"}
  ],
  "mode": "Quote",
  "request_id": "req-unsub-1"
}
```

- A `mode` on an individual item wins; otherwise the top-level `mode`
  applies; a request with neither unsubscribes `Quote`.
- A single instrument may be sent without the list (`symbol`, `exchange` and
  `mode` at the top level).
- Unsubscribing something you do not hold is a success that changes nothing,
  as on the web.

Acknowledgement:

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

An item whose own `mode` is invalid is listed under `failed` with
`"mode": null` and the reason; the others are still processed. `status` is
`success` when nothing failed, `partial` when some items failed and some
succeeded, and `error` when every item failed:

```json
{
  "type": "unsubscribe",
  "status": "partial",
  "message": "Unsubscription processing complete",
  "successful": [
    {"symbol": "NIFTY", "exchange": "NSE_INDEX", "mode": "LTP", "status": "success", "broker": "zerodha"}
  ],
  "failed": [
    {"symbol": "NIFTY", "exchange": "NSE_INDEX", "mode": null, "status": "error", "message": "Invalid mode 'bogus'; expected 'LTP', 'Quote', or 'Depth' (case-insensitive)", "broker": "zerodha"}
  ],
  "broker": "zerodha",
  "request_id": "req-unsub-mixed"
}
```

A request with no instruments and no `unsubscribe_all` is answered with
`INVALID_PARAMETERS` `Either symbols or unsubscribe_all is required`.

#### Unsubscribe everything

```json
{"action": "unsubscribe_all", "request_id": "req-unsub-all"}
```

The answer has `"type": "unsubscribe"` and lists every instrument and mode
released, in the order they were subscribed.

The broker stream for an instrument stays up while any connection still
holds it, and is released when the last one lets go, whether by
unsubscribing or by disconnecting. A disconnect releases everything that
connection held; the server does not restore it on reconnect.

### Market data format

Every frame has the same outer shape:

| Key | Meaning |
| --- | --- |
| `type` | `market_data` |
| `symbol`, `exchange` | The instrument, in OpenAlgo format |
| `mode` | `1`, `2` or `3`: the mode of the subscription this frame serves |
| `data` | The values (below) |
| `broker` | The connected broker's id |

A connection that holds the same instrument in more than one mode receives
one frame per mode for each update, richest mode first. Timestamps are epoch
milliseconds.

#### LTP (mode 1)

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

`ltt` is the last trade time. In the desktop it is always present (the tick
time when the broker sends none); the web's Zerodha adapter leaves it out of
some LTP frames.

#### Quote (mode 2), a stock

```json
{
  "type": "market_data",
  "symbol": "RELIANCE",
  "exchange": "NSE",
  "mode": 2,
  "data": {
    "symbol": "RELIANCE",
    "exchange": "NSE",
    "mode": "quote",
    "ltp": 1167.7,
    "ltt": 1791000305765,
    "timestamp": 1791000305765,
    "volume": 1254300,
    "last_quantity": 12,
    "average_price": 1171.42,
    "total_buy_quantity": 402115,
    "total_sell_quantity": 388020,
    "open": 1180.1,
    "high": 1183.9,
    "low": 1160.8,
    "close": 1187.0
  },
  "broker": "zerodha"
}
```

`open`, `high`, `low` and `close` are `0.0` when the broker has not sent
them. `close` is the previous session's close. `oi` and `open_interest`
(the same value) are added when the broker sends open interest.

In the desktop, a stock's Quote and Depth frames carry no change fields. To
show the day's change, compute it from `ltp` and `close`.

#### Quote (mode 2), an index

An instrument on an `_INDEX` exchange (`NSE_INDEX`, `BSE_INDEX`, ...) has no
order flow, so its Quote frame is shorter:

```json
{
  "type": "market_data",
  "symbol": "NIFTY",
  "exchange": "NSE_INDEX",
  "mode": 2,
  "data": {
    "symbol": "NIFTY",
    "exchange": "NSE_INDEX",
    "mode": "quote",
    "ltp": 22421.95,
    "ltt": 1791000305767,
    "timestamp": 1791000305767,
    "volume": 0,
    "open": 22380.4,
    "high": 22455.1,
    "low": 22310.25,
    "close": 22398.6,
    "price_change": 23.35,
    "price_change_percent": 0.1
  },
  "broker": "zerodha"
}
```

An index's `open`, `high`, `low` and `close` are left out while the broker
has none (outside market hours, for example). `price_change` and
`price_change_percent` are `0` when the broker sends no change.

#### Depth (mode 3)

```json
{
  "type": "market_data",
  "symbol": "RELIANCE",
  "exchange": "NSE",
  "mode": 3,
  "data": {
    "symbol": "RELIANCE",
    "exchange": "NSE",
    "mode": "full",
    "ltp": 1167.7,
    "ltt": 1791000325761,
    "timestamp": 1791000325761,
    "volume": 1254300,
    "last_quantity": 12,
    "average_price": 1171.42,
    "total_buy_quantity": 402115,
    "total_sell_quantity": 388020,
    "open": 1180.1,
    "high": 1183.9,
    "low": 1160.8,
    "close": 1187.0,
    "oi": 0,
    "open_interest": 0,
    "depth": {
      "buy": [
        {"price": 1167.6, "quantity": 50, "orders": 3},
        {"price": 1167.5, "quantity": 35, "orders": 2},
        {"price": 1167.4, "quantity": 42, "orders": 4},
        {"price": 1167.3, "quantity": 28, "orders": 1},
        {"price": 1167.2, "quantity": 33, "orders": 5}
      ],
      "sell": [
        {"price": 1167.7, "quantity": 47, "orders": 2},
        {"price": 1167.8, "quantity": 39, "orders": 3},
        {"price": 1167.9, "quantity": 41, "orders": 4},
        {"price": 1168.0, "quantity": 32, "orders": 2},
        {"price": 1168.1, "quantity": 30, "orders": 1}
      ]
    }
  },
  "broker": "zerodha"
}
```

Each side is best price first and holds at most the number of levels you
asked for (see [Depth levels](#depth-levels)). `oi` and `open_interest` are
`0` when the broker sends no open interest. An index subscribed in Depth
mode receives the index Quote fields and no `depth`.

#### Field names by broker

`data.mode` is `ltp`, `quote` or `depth`. On OpenAlgo web this inner label
comes from each broker's adapter and varies (Zerodha's is `full` for depth),
so match on the top-level `mode` number instead.

The names above (`last_quantity`, `average_price`, `price_change`,
`price_change_percent`) are what OpenAlgo web sends with Zerodha. In the
desktop every broker sends them, and for brokers whose web adapter uses other
names, those names are added beside them with the same value, so a script
written for either works:

| Added name | Same value as | Brokers |
| --- | --- | --- |
| `last_trade_quantity` | `last_quantity` | Angel, Fyers, Upstox, 5paisa, mstock, Paytm, Pocketful, Samco |
| `ltq` | `last_quantity` | Definedge, Motilal |
| `avg_trade_price` | `average_price` | Fyers |
| `change` | `price_change` | Fyers, Arrow, HDFC Securities, HDFC Sky, Rupeezy, Tradejini, Samco |
| `change_absolute` | `price_change` | Paytm |
| `change_percent` | `price_change_percent` | Fyers, AliceBlue, Arrow, HDFC Securities, HDFC Sky, Definedge, Paytm, Rupeezy, Tradejini |
| `percent_change` | `price_change_percent` | Shoonya, Flattrade, Zebu, TradeSmart |
| `change_percentage` | `price_change_percent` | Samco |

The change names appear only where `price_change` does, that is, on indices.

#### What the Python SDK hands your callback

The Python SDK does not pass the raw frame to your callback. It builds its own
`data` from the frame:

- LTP: `ltp`, `timestamp` (and `ltt` when present).
- Quote: `open`, `high`, `low`, `close`, `ltp`, `volume`,
  `last_trade_quantity`, `avg_trade_price`, `change`, `change_percent`,
  `timestamp`. A key the frame does not carry becomes `0`.
- Depth: `ltp`, `timestamp`, `depth`.

So with Zerodha, for example, `last_trade_quantity` and `avg_trade_price` are
`0` in the SDK's quote data, on the web and in the desktop alike. Read the raw
frame (your own WebSocket client) if you need `last_quantity`,
`average_price`, `total_buy_quantity` or open interest.

### Order updates (account-level stream)

Order status changes (placed, filled, partly filled, rejected, cancelled) for
the whole account, pushed by the broker, or by the sandbox engine in analyzer
mode. There are no symbols or modes; one subscription covers every order.

Subscribe, after authenticating:

```json
{"action": "subscribe_orders"}
```

```json
{"type": "subscribe_orders", "status": "success", "message": "Subscribed to order updates"}
```

Stop with `{"action": "unsubscribe_orders"}`, answered with
`{"type": "unsubscribe_orders", "status": "success", "message": "Unsubscribed from order updates"}`.
Both accept a `request_id`, echoed back. Before authenticating, both are
answered with `NOT_AUTHENTICATED`.

Each update:

```json
{
  "type": "order_update",
  "user_id": "<USER_ID>",
  "mode": "live",
  "broker": "upstox",
  "orderid": "240221025997024",
  "symbol": "RELIANCE",
  "exchange": "NSE",
  "action": "BUY",
  "quantity": 10,
  "price": 1424.0,
  "trigger_price": 0.0,
  "pricetype": "LIMIT",
  "product": "MIS",
  "order_status": "complete",
  "filled_quantity": 10,
  "pending_quantity": 0,
  "average_price": 1423.85,
  "rejection_reason": ""
}
```

The 18 keys and their meaning are the web's:

- `symbol` and `exchange` are in OpenAlgo format, mapped from the broker's
  own symbols.
- `action` is `BUY` or `SELL`; `pricetype` `MARKET`, `LIMIT`, `SL` or `SL-M`;
  `product` `CNC`, `NRML` or `MIS` (see [Order constants](order-constants.md)).
- `order_status` is lowercase: `open`, `trigger pending`, `complete`,
  `rejected`, `cancelled`, plus broker extras such as `expired`.
- `rejection_reason` carries the broker's text when an order is rejected.
- `mode` is `live` for broker orders and `analyze` for sandbox orders. A
  sandbox update has `"broker": "sandbox"`.
- `user_id` is your OpenAlgo username.

Order updates are never dropped for a slow reader; a connection that stops
reading altogether is closed (see [Limits](#limits-and-disconnects)).

Where the updates come from in the desktop:

- the broker's own order-update socket, for brokers that have one (for
  example Dhan, Fyers, Upstox, Angel, AliceBlue, Kotak, Definedge, IndMoney,
  Arrow, IIFL Capital, Motilal, Nubra);
- the market data socket, for brokers that send order updates on it (for
  example Zerodha, Pocketful and the Shoonya, Flattrade, Zebu and TradeSmart
  family);
- polling the order book, for brokers without a push channel (for example
  Groww, Samco and 5paisa);
- the sandbox engine, in analyzer mode.

In the desktop there is no `/postback/<broker>` webhook receiver; that is a
server feature of the web. The order book (`/api/v1/orderbook`) remains the
record to reconcile against.

### Other actions

**Ping.** An application-level latency probe, answered before or after
authentication:

```json
{"action": "ping", "timestamp": 1791000284000, "_pingId": 7}
```

```json
{"type": "pong", "status": "success", "server_timestamp": 1791000284862, "client_timestamp": 1791000284000, "_pingId": 7}
```

`timestamp` and `_pingId` are optional and echoed back when sent. This is not
the WebSocket keepalive; see [Heartbeat](#heartbeat-and-reconnection).

**Broker information** (after authenticating):

```json
{"action": "get_broker_info"}
```

```json
{"type": "broker_info", "status": "success", "broker": "zerodha", "adapter_status": "connected", "user_id": "<USER_ID>"}
```

`adapter_status` is `connected` or `disconnected`. With no broker logged in,
the answer is `BROKER_ERROR` `Broker information not available`.

**Supported brokers:**

```json
{"action": "get_supported_brokers"}
```

```json
{"type": "supported_brokers", "status": "success", "brokers": ["aliceblue", "angel", "...", "zerodha"], "count": 37}
```

The list has the same 37 broker ids as the web. In the desktop it is in
alphabetical order.

### Error frame

Errors that concern the whole request use one shape, with no `type`:

```json
{
  "status": "error",
  "code": "INVALID_PARAMETERS",
  "message": "At least one symbol must be specified",
  "request_id": "req-nosyms"
}
```

`request_id` is present only when the request carried one (subscribe,
unsubscribe, order-update requests).

| `code` | When |
| --- | --- |
| `NOT_AUTHENTICATED` | Subscribe, unsubscribe, order updates or broker info before authenticating |
| `AUTHENTICATION_ERROR` | Missing or wrong key, OpenAlgo locked, or authentication ended |
| `BROKER_ERROR` | No broker logged in |
| `INVALID_MODE` | `mode` is not 1, 2, 3 or a label |
| `INVALID_PARAMETERS` | No instruments, or an invalid `depth` |
| `INVALID_ACTION` | Unknown action, for example `Invalid action: bogus_action`, or `Invalid action: None` when there is none |
| `INVALID_JSON` | The message is not JSON: `Invalid JSON message` |
| `SERVER_ERROR` | The message is JSON but not an object, or a list item is not an object, for example `'list' object has no attribute 'get'` |

The texts of `INVALID_MODE`, `INVALID_ACTION` and `SERVER_ERROR` reproduce the
web's word for word, so clients that match on them keep working.

### Heartbeat and reconnection

- The server sends a WebSocket ping every 20 seconds. Every WebSocket library
  answers it automatically. A connection that sends nothing at all, not even
  that automatic answer, for about 40 seconds is closed. In the desktop these
  times are fixed; the web's `WS_PING_INTERVAL` and `WS_PING_TIMEOUT` settings
  do not exist, as there is no `.env`.
- The JSON `ping` action is optional.
- After a disconnect, a client must connect, authenticate and subscribe
  again. The server keeps nothing for a closed connection. The Python SDK
  reconnects, authenticates and replays its subscriptions by itself, with a
  growing wait between attempts (up to a minute).

### Limits and disconnects

How the desktop closes a connection, and why:

| Code | Reason | Cause |
| --- | --- | --- |
| `4401` | `auth timeout` | Not authenticated within 15 seconds of connecting |
| `4401` | `authentication expired` | You signed out, the broker session ended or started, or the API key was regenerated (after an error frame) |
| `4401` | `Invalid API key` | A wrong key after this caller's failure budget was spent |
| `1001` | `server shutting down` | The app is quitting, or the market data port or address was changed |
| `1008` | `too many messages` | The connection sent more than its message budget |
| `1008` | `client too slow` | The client stopped reading what the server sends |
| `1009` | `message too big` | A single message over 1 MiB |
| `1013` | `Too many connections` | The connection limit is reached; try again later |

In the desktop:

- **Message budget.** Each connection may send up to 1000 messages a second
  (6000 in a burst) and 2 MiB a second (8 MiB in a burst). Subscribing to all
  3000 instruments a connection may hold, in one message or one message each,
  stays far inside it. The web reads without a limit.
- **Slow readers.** Market data for a client that reads slowly is not queued
  without limit: each instrument and mode keeps only its latest frame, so a
  slow client gets the latest price rather than a backlog. If the client stops
  reading entirely and over 1024 acknowledgements, errors or order updates
  pile up, it is closed with `1008`.
- **Connections.** Programs on this computer may hold up to 64 connections.
  Devices on your network and tunnel callers share a separate pool of 256, at
  most 32 from any one network address, so they can never use up the places of
  your own programs. A network address blocked on the Security page is
  disconnected before the handshake.
- **Every tick is forwarded.** There is no throttling of market data.

### Security

- Every client must authenticate with the API key from the **API Key** page.
  The same key works for `/api/v1`.
- The feed listens on this computer only unless you turn on **Allow access
  from other devices**. Traffic to other devices is not encrypted; use that
  only on a network you trust.
- Browser pages from other sites cannot open the feed (see
  [WebSocket URL](#websocket-url)).
- A regenerated API key, a sign-out and the end of the broker session
  disconnect every client that authenticated before.

### Trying the feed in the app

The app has the same test pages as OpenAlgo web: **`/websocket/test`** for
market data (`/websocket/test/20`, `/30` and `/50` request those depth levels)
and **`/websocket/order`** for the order-update stream. Use them to check a
broker's feed before you point your own client at it.

### Differences from OpenAlgo web

Everything not listed here behaves as on OpenAlgo web.

- No `wss://yourdomain/ws` address; the feed is plain `ws://`, on this
  computer by default. The port is set in Server Settings, not `.env`.
- Browser pages from other sites are refused at the handshake.
- While OpenAlgo Desktop is locked (after it starts, after you log out),
  `authenticate` answers with the "OpenAlgo is locked" error until you sign
  in.
- A client that authenticated before a sign-out, a broker session change or a
  regenerated API key is closed with `4401`; the web keeps it connected.
- Wrong keys count against the caller's failure budget, shared with
  `/api/v1` and MCP; once it is spent, a wrong key closes the connection.
- A `depth` that is not a positive whole number is refused; any other depth is
  served from the levels the broker has, never refused per instrument.
- At most 3000 subscriptions per connection.
- Quote and Depth data use one set of field names for every broker, with the
  broker's web names added beside them. Change fields are sent for indices
  only. `data.mode` is always `ltp`, `quote` or `depth`. `ltt` is always
  present.
- Per-connection message budget, 1 MiB message limit, latest-price
  coalescing for slow readers, and connection limits.
- Ping interval and timeout are fixed at 20 seconds.
- No `/postback/<broker>` webhooks for order updates.
- `get_supported_brokers` lists the brokers alphabetically.
