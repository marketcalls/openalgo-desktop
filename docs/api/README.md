# OpenAlgo Desktop API Documentation

OpenAlgo Desktop serves the same REST API (`/api/v1`) and the same WebSocket feed as OpenAlgo web. A request that works against OpenAlgo web works here unchanged: same fields, same answers, same error messages, same date and symbol formats. The Python SDK, TradingView, Amibroker, Chartink, GoCharting, Excel and MCP clients need no code changes.

These pages describe what the desktop does today. Where it answers differently from OpenAlgo web, the page says so in an **In the desktop** section.

## Base URLs

```text
REST API:  http://127.0.0.1:5000/api/v1
WebSocket: ws://127.0.0.1:8765
```

These are the defaults, the same as OpenAlgo web.

- **Another port.** If you changed the ports in **Server Settings** (profile menu), use the new ones.
- **Another device on your network.** Turn on **Allow access from other devices** in Server Settings, restart the app, and use `http://<this computer's IP address>:5000`.
- **From the internet** (TradingView, Chartink, GoCharting alerts). Run a tunnel such as ngrok or Cloudflare Tunnel to `http://127.0.0.1:5000` and enter its address as **Host Server URL** under **Server Configuration** on the Broker tab in Profile.

See [Moving from OpenAlgo web](../user/moving-from-openalgo-web.md) and [Another device, a firewall, or a tunnel](../user/troubleshooting.md#another-device-a-firewall-or-a-tunnel).

## Before You Call the API

1. **Sign in to OpenAlgo.** Your API key is protected with your OpenAlgo password. After the app starts, and after you log out, every `/api/v1` endpoint answers HTTP 503 until you sign in again:

   ```json
   {
     "status": "error",
     "message": "Sign in to OpenAlgo to use the API. The API is off after OpenAlgo starts and after you log out, until you sign in again."
   }
   ```

2. **Log in to your broker.** Most endpoints need a live broker session. Without one they answer 403 `Invalid openalgo apikey`, as on the web. These need only a valid API key: `/search`, `/expiry`, `/instruments`, `/pnl/symbols`, `/chart`, `/market/holidays`, `/market/timings`, `/strategy/*`, `/telegram/*`, `/whatsapp/notify`, and `/history` with `source: "db"` (Historify).

3. **Use the desktop's API key.** Copy it from the **API Key** page. A key from OpenAlgo web does not work here.

## Authentication

POST endpoints take the API key as `apikey` in the JSON body. GET endpoints (`/instruments`, `/ticker`, `/chart`) take it as the `apikey` query parameter.

```json
{
  "apikey": "<APIKEY>"
}
```

The `X-API-KEY` header is not read by `/api/v1` endpoints, exactly as on the web: a request that sends the key only in the header gets `400 {"status": "error", "message": {"apikey": ["Missing data for required field."]}}`. The Telegram and WhatsApp endpoints are the exception: they also read `X-API-KEY`.

Requests are JSON with `Content-Type: application/json`. Request schemas are strict, as on the web: a field the endpoint does not know is refused with `Unknown field.` The exceptions are `/placegttorder` and `/modifygttorder`, which ignore extra fields, and `POST /chart`, which stores them. A trailing slash on the path is accepted (`/ping/` is `/ping`).

Never put broker credentials or broker tokens in these requests. The API key reaches your broker session inside OpenAlgo.

## Endpoint Index

The desktop serves **66 method/path pairs**, every one OpenAlgo web documents. A resource with both GET and POST counts twice. All paths are under `/api/v1`.

### Order Management

| Method | Path | Documentation |
|---|---|---|
| POST | `/placeorder` | [Place order](./order-management/placeorder.md) |
| POST | `/placesmartorder` | [Place smart order](./order-management/placesmartorder.md) |
| POST | `/optionsorder` | [Options order](./order-management/optionsorder.md) |
| POST | `/optionsmultiorder` | [Options multi-order](./order-management/optionsmultiorder.md) |
| POST | `/basketorder` | [Basket order](./order-management/basketorder.md) |
| POST | `/splitorder` | [Split order](./order-management/splitorder.md) |
| POST | `/modifyorder` | [Modify order](./order-management/modifyorder.md) |
| POST | `/cancelorder` | [Cancel order](./order-management/cancelorder.md) |
| POST | `/cancelallorder` | [Cancel all orders](./order-management/cancelallorder.md) |
| POST | `/closeposition` | [Close positions](./order-management/closeposition.md) |
| POST | `/placegttorder` | [Place GTT](./order-management/placegttorder.md) |
| POST | `/modifygttorder` | [Modify GTT](./order-management/modifygttorder.md) |
| POST | `/cancelgttorder` | [Cancel GTT](./order-management/cancelgttorder.md) |
| POST | `/gttorderbook` | [GTT order book](./order-management/gttorderbook.md) |

### Order Information

| Method | Path | Documentation |
|---|---|---|
| POST | `/orderstatus` | [Order status](./order-information/orderstatus.md) |
| POST | `/openposition` | [Open position](./order-information/openposition.md) |

### Account Services

| Method | Path | Documentation |
|---|---|---|
| POST | `/funds` | [Funds](./account-services/funds.md) |
| POST | `/margin` | [Margin](./account-services/margin.md) |
| POST | `/orderbook` | [Order book](./account-services/orderbook.md) |
| POST | `/tradebook` | [Trade book](./account-services/tradebook.md) |
| POST | `/positionbook` | [Position book](./account-services/positionbook.md) |
| POST | `/holdings` | [Holdings](./account-services/holdings.md) |

### Market Data

| Method | Path | Documentation |
|---|---|---|
| POST | `/quotes` | [Quote](./market-data/quotes.md) |
| POST | `/multiquotes` | [Multiple quotes](./market-data/multiquotes.md) |
| POST | `/depth` | [Market depth](./market-data/depth.md) |
| POST | `/history` | [Historical candles](./market-data/history.md) |
| POST | `/intervals` | [Supported intervals](./market-data/intervals.md) |
| GET | `/ticker/<exchange:symbol>` | [Ticker-compatible history](./market-data/ticker.md) |

### Symbol Services

| Method | Path | Documentation |
|---|---|---|
| POST | `/symbol` | [Symbol information](./symbol-services/symbol.md) |
| POST | `/search` | [Symbol search](./symbol-services/search.md) |
| POST | `/expiry` | [Expiry dates](./symbol-services/expiry.md) |
| GET | `/instruments` | [Instrument master](./symbol-services/instruments.md) |

### Options Services

| Method | Path | Documentation |
|---|---|---|
| POST | `/optionsymbol` | [Resolve option symbol](./options-services/optionsymbol.md) |
| POST | `/optionchain` | [Option chain](./options-services/optionchain.md) |
| POST | `/syntheticfuture` | [Synthetic future](./options-services/syntheticfuture.md) |
| POST | `/optiongreeks` | [Option Greeks](./options-services/optiongreeks.md) |
| POST | `/multioptiongreeks` | [Batch option Greeks](./options-services/multioptiongreeks.md) |

### Market Calendar

| Method | Path | Documentation |
|---|---|---|
| POST | `/market/holidays` | [Market holidays](./market-calendar/holidays.md) |
| POST | `/market/timings` | [Market timings](./market-calendar/timings.md) |

There is no `/checkholiday` endpoint. Use `/market/timings` for a date.

### Analyzer Services

| Method | Path | Documentation |
|---|---|---|
| POST | `/analyzer` | [Analyzer status](./analyzer-services/analyzerstatus.md) |
| POST | `/analyzer/toggle` | [Toggle analyzer mode](./analyzer-services/analyzertoggle.md) |
| POST | `/pnl/symbols` | [Sandbox P&L by symbol](./analyzer-services/pnlsymbols.md) |

### Chart and Utility Services

| Method | Path | Documentation |
|---|---|---|
| GET | `/chart` | [Read chart preferences](./chart-services/chart.md) |
| POST | `/chart` | [Update chart preferences](./chart-services/chart.md) |
| POST | `/ping` | [Authenticated ping](./utility-services/ping.md) |

### Strategy Module

Lifecycle and reads for the multi-leg strategies of the Strategy module. See the [strategy module API](./strategy-services/README.md) for the shared conventions.

| Method | Path | Documentation |
|---|---|---|
| POST | `/strategy/list` | [List strategies](./strategy-services/list.md) |
| POST | `/strategy/status` | [Strategy status](./strategy-services/status.md) |
| POST | `/strategy/start` | [Start a run](./strategy-services/start.md) |
| POST | `/strategy/stop` | [Stop the current run](./strategy-services/stop.md) |
| POST | `/strategy/close_all` | [Close every leg](./strategy-services/close_all.md) |
| POST | `/strategy/close_leg` | [Close one leg](./strategy-services/close_leg.md) |
| POST | `/strategy/runs` | [Run history](./strategy-services/runs.md) |
| POST | `/strategy/orders` | [Order history](./strategy-services/orders.md) |
| POST | `/strategy/events` | [Risk-event audit trail](./strategy-services/events.md) |

The module's TradingView webhook, `POST http://127.0.0.1:5000/strategy/webhook/<token>`, is not under `/api/v1` and takes no `apikey`: the token in the address identifies the strategy. See the [webhook page](./strategy-services/webhook.md).

### Messaging

| Method | Path | Documentation |
|---|---|---|
| GET, POST | `/telegram/config` | [Telegram API](./telegram-services/README.md) |
| POST | `/telegram/start` | [Telegram API](./telegram-services/README.md) |
| POST | `/telegram/stop` | [Telegram API](./telegram-services/README.md) |
| POST | `/telegram/webhook` | [Telegram API](./telegram-services/README.md) |
| GET | `/telegram/users` | [Telegram API](./telegram-services/README.md) |
| POST | `/telegram/broadcast` | [Telegram API](./telegram-services/README.md) |
| POST | `/telegram/notify` | [Telegram API](./telegram-services/README.md) |
| GET | `/telegram/stats` | [Telegram API](./telegram-services/README.md) |
| GET, POST | `/telegram/preferences` | [Telegram API](./telegram-services/README.md) |
| POST | `/whatsapp/notify` | [WhatsApp notification](./whatsapp-services/notify.md) |

The Telegram resource counts as 11 method/path pairs. See also the [WhatsApp overview](./whatsapp-services/README.md).

### Not in the Desktop

OpenAlgo web also registers six endpoints that its own docs do not list. The desktop does not serve them; they answer 404 `Not found`.

| Method | Path | Why |
|---|---|---|
| GET | `/portfolio/benchmarks` | Portfolio Backtester, which needs the web's Python backtester |
| POST | `/portfolio/backtest` | Same |
| POST | `/portfolio/tearsheet` | Same |
| POST | `/portfolio/holdings` | Same |
| GET | `/sip/frequencies` | SIP Backtester, same reason |
| POST | `/sip/backtest` | Same |

The desktop serves no `/api/v1` endpoint that OpenAlgo web lacks.

### Other Entry Points

- **Chartink webhook**: `POST http://127.0.0.1:5000/chartink/webhook/<webhook_id>`, the address shown for each Chartink strategy.
- **MCP** for AI clients: the `openalgo-desktop mcp` command (stdio) and streamable HTTP at `http://127.0.0.1:5000/mcp`, with a token from the API Key page. See [MCP](../user/mcp.md).

## WebSocket Streaming

The feed is not under `/api/v1`. Connect to `ws://127.0.0.1:8765`, authenticate with your API key, then subscribe. The shared protocol (authenticate, subscribe and its `depth` field, unsubscribe, ping, the error frame and close codes) is in [WebSocket protocol](./websocket-streaming/README.md).

| Mode | Documentation |
|---|---|
| LTP | [LTP subscription](./websocket-streaming/ltp.md) |
| Quote | [Quote subscription](./websocket-streaming/quote.md) |
| Depth | [Depth subscription](./websocket-streaming/depth.md) |
| Order updates | [Order-update stream](./websocket-streaming/order-updates.md) |

Actions: `authenticate`, `subscribe`, `unsubscribe`, `unsubscribe_all`, `subscribe_orders`, `unsubscribe_orders`, `get_broker_info`, `get_supported_brokers` and `ping`.

## Order Constants

| Kind | Values |
|---|---|
| Exchange | `NSE`, `BSE`, `NFO`, `BFO`, `CDS`, `BCD`, `MCX`, `NCDEX`, `NCO`, `NSE_INDEX`, `BSE_INDEX`, `MCX_INDEX`, `GLOBAL_INDEX`, `CRYPTO` (case-sensitive). Your broker decides which of them you can use. |
| Product | `MIS`, `CNC`, `NRML` |
| Price type | `MARKET`, `LIMIT`, `SL`, `SL-M` |
| Action | `BUY`, `SELL` (lowercase is accepted) |

Symbols are in OpenAlgo format: `RELIANCE` (equity), `NIFTY27OCT26FUT` (future), `NIFTY06OCT2622400CE` (option), `NIFTY` on `NSE_INDEX` (index). Order quantities are whole numbers except on `CRYPTO`.

## Analyzer Mode

When analyzer mode is on, every order and account endpoint works against the sandbox instead of your broker, and its answers carry `"mode": "analyze"`. Nothing reaches your broker. See [Sandbox mode](../user/sandbox.md) and the [analyzer endpoints](./analyzer-services/analyzerstatus.md).

## Semi-Auto Orders

When the order mode of your API key is **Semi-Auto** (API Key page), a new order from an API client is not sent: it waits in the **Action Center** for you to approve or reject it. The endpoint answers:

```json
{
  "status": "success",
  "message": "Order queued for approval in Action Center",
  "mode": "semi_auto",
  "pending_order_id": 12
}
```

This applies in analyzer mode too; whether analyzer mode is on when you approve decides whether the order goes to the sandbox or your broker. Modify, cancel, cancel-all, close-position and the GTT modify and cancel are refused with 403 while Semi-Auto is on, except in analyzer mode. Reads (books, funds, order status) work as usual. This is the web's behaviour.

## Responses

Most endpoints answer `{"status": "success", "data": ...}` or `{"status": "error", "message": ...}`. Some answer flat objects without `data` (`/optionsymbol`, `/optionchain`, `/optiongreeks`, `/syntheticfuture`, `/openposition`); `/multiquotes` uses `results`. `/instruments` can answer CSV and `/ticker` plain text. Each endpoint page shows its own shape.

Validation errors come in three shapes, as on the web:

- **An object** on read endpoints: `{"status": "error", "message": {"symbol": ["Missing data for required field."]}}`.
- **A string spelling out a Python dictionary** on order endpoints: `{"status": "error", "message": "{'quantity': ['Quantity must be a positive number.']}"}`.
- **An envelope** on `/optionsorder` and `/optionsmultiorder`: `{"status": "error", "message": "Validation error", "errors": {"legs": ["..."]}}`.

Business errors (an unknown symbol, an unknown order id) carry `message` as a plain string. A client should accept `message` as a string or an object.

Dates and times:

- `/expiry` and `/symbol` return expiries as `DD-MMM-YY` (`06-OCT-26`); symbols and `expiry_date` requests use `DDMMMYY` (`06OCT26`).
- `/optiongreeks` returns `expiry_date` as `DD-Mon-YYYY` (`06-Oct-2026`).
- History and ticker candles are epoch seconds; market holidays, market timings and WebSocket timestamps are epoch milliseconds.
- Order and trade timestamps are `YYYY-MM-DD HH:MM:SS` in IST.

## Errors Common to Every Endpoint

| HTTP | Body | When |
|---|---|---|
| 400 | `{"status": "error", "message": {"apikey": ["Missing data for required field."]}}` | No `apikey` in the body (on order endpoints the same text as a string) |
| 400 | `{"status": "error", "message": {"apikey": ["Length must be between 1 and 256."]}}` | An empty `apikey` |
| 400 | `{"status": "error", "message": {"foo": ["Unknown field."]}}` | A field the endpoint does not know (on order endpoints the same text as a string) |
| 400 | `{"message": "The browser (or proxy) sent a request that this server could not understand."}` | Malformed or empty JSON (no `status` field) |
| 401 | `{"status": "error", "message": "Invalid openalgo apikey"}` | `/optiongreeks` and `/multioptiongreeks`: wrong key or no broker session |
| 403 | `{"status": "error", "message": "Invalid openalgo apikey"}` | Wrong key, or no broker logged in on an endpoint that needs one |
| 404 | `{"status": "error", "message": "Not found", "path": "/api/v1/doesnotexist"}` | Unknown path, or the wrong method (GET on a POST endpoint is 404, not 405) |
| 413 | `{"status": "error", "message": "The request is too large."}` | A body over 1 MiB |
| 429 | `{"message": "100 per 1 second"}` | Rate limit (no `status`, no rate-limit headers). See [Rate limiting](./rate-limiting.md) |
| 500 | `{"status": "error", "message": "An unexpected error occurred"}` | The `Content-Type` is not JSON |
| 500 | `{"message": "Internal Server Error"}` | The JSON body is not an object (for example an array) |
| 503 | `{"status": "error", "message": "Sign in to OpenAlgo to use the API. ..."}` | OpenAlgo is locked: nobody has signed in since it started, or you logged out |

The request body is validated before the key is checked, so a request with both a bad field and a wrong key gets the 400.

## Rate Limits

100 requests a second per caller for most endpoints, 10 a second for order endpoints and for `/placesmartorder`, each endpoint in a window of its own, with a moving window and no rate-limit headers. OpenAlgo also paces the orders it sends to your broker. See [Rate limiting](./rate-limiting.md).

## Client Libraries

The OpenAlgo Python SDK (`pip install openalgo`) works unchanged. Point it at the desktop:

```python
from openalgo import api

client = api(
    api_key="<APIKEY>",
    host="http://127.0.0.1:5000",
    ws_url="ws://127.0.0.1:8765",
)
```

Any HTTP client that can send JSON works the same way.

## In the desktop

- In the desktop, `/api/v1` answers 503 until you sign in to OpenAlgo after it starts, and again after you log out. The web has no such state.
- In the desktop, a request body over 1 MiB is refused with 413. The web sets no limit.
- In the desktop, a request sent by a web page from another site in your browser is refused with 403 `Invalid openalgo apikey` before its key is checked, so a page you happen to open cannot use or lock out your key. The web lets the operator allow other sites through `CORS_ALLOWED_ORIGINS`; the desktop has no such setting.
- In the desktop, the six portfolio and SIP backtester endpoints are not served (above).
- In the desktop, an order sent to your broker with no definite answer (a timeout or a dropped connection after sending) is never sent again: OpenAlgo finds it in the broker's order book. The answer keeps the web's error shape. See [Order placement outcomes](../port/order-outcomes.md).
- In the desktop, an order is refused (500, nothing sent) when OpenAlgo cannot read whether Semi-Auto is on. The web treats an unreadable order mode as Auto and sends the order.
- In the desktop, an order quantity above 2147483647 is refused before anything is sent.
- Rate limits, order pacing and some sandbox answers differ in places; each page lists its own differences, and [Rate limiting](./rate-limiting.md) lists the limits.
