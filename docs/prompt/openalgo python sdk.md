# OpenAlgo Python SDK with OpenAlgo Desktop

The OpenAlgo Python SDK (`openalgo` on PyPI) works against OpenAlgo Desktop
without code changes. The desktop answers every `/api/v1` request the SDK
sends with the same fields, status values and error messages as OpenAlgo
web, and its market data WebSocket speaks the same protocol. The only thing
you change is the API key, and the host if you moved the ports.

This page is the web's SDK reference checked method by method against the
desktop. The examples and responses are the web's; the desktop answers in the
same shape. Order ids, prices and dates in the responses are illustrative.

## Install

```bash
# Trading API + the Rust-powered `ta` indicator library, in one package
pip install openalgo
```

A single install ships everything: the trading API client and the `ta`
technical-indicator library. `ta` is a compiled extension inside the package,
with no extra to install.

The SDK runs in your own Python. OpenAlgo Desktop itself needs no Python; the
SDK is only for your scripts.

GTT orders and the strategy module methods need **openalgo 2.0.4 or newer**.
Upgrade with `pip install -U openalgo`.

```python
import openalgo
openalgo.__version__
```

## Connect to OpenAlgo Desktop

### 1. Get the API key

1. Open OpenAlgo Desktop and sign in.
2. Open the **API Key** page (profile menu).
3. Copy the key. The desktop creates its own key when you set up your account;
   a key from OpenAlgo web does not work here.

### 2. Create the client

```python
from openalgo import api

client = api(
    api_key="<APIKEY>",
    host="http://127.0.0.1:5000",      # REST API
    ws_url="ws://127.0.0.1:8765",      # market data WebSocket
)
```

`http://127.0.0.1:5000` and `ws://127.0.0.1:8765` are the desktop's defaults,
the same as OpenAlgo web. Both are changeable in **Server Settings** ("App
port" and "Market data port"). If you change them, change `host` and `ws_url`
to match. Without `ws_url` the SDK builds the WebSocket address from the host
name and port 8765, so pass `ws_url` whenever you moved the market data port.

### 3. Keep OpenAlgo Desktop signed in

The API, webhooks and MCP are off after OpenAlgo Desktop starts and after you
log out, until you sign in again. The keys that check your API key are derived
from your OpenAlgo password and are only in memory while you are signed in.

While it is locked, every SDK call returns an error like this (HTTP 503):

```python
{'status': 'error',
 'message': 'HTTP 503: {"message":"Sign in to OpenAlgo to use the API. The API is off after OpenAlgo starts and after you log out, until you sign in again.","status":"error"}',
 'code': 503,
 'error_type': 'http_error'}
```

The fix is to sign in to OpenAlgo Desktop. A WebSocket `connect()` while
locked is refused with an authentication error that says the same thing.

In the desktop this state is new: OpenAlgo web has no locked state. A wrong
key still answers 403 "Invalid openalgo apikey", so a script can tell the two
apart by `code`.

### 4. Connect your broker

As on the web, a valid key with no broker session is reported as an invalid
key (403 "Invalid openalgo apikey") by the endpoints that need the broker.
Log in to your broker in the app first. Broker sessions end around 03:00 IST;
log in again the next day.

### 5. Running the script on another computer

By default the desktop listens only on this computer (`127.0.0.1`). To reach
it from another device on your network, turn on **Allow access from other
devices** in Server Settings and use this computer's network address as the
host. Traffic on your network is not encrypted; use this only on a network you
trust.

### Analyzer mode and Semi-Auto

- With analyzer mode on, every order and account call goes to the sandbox, not
  your broker, and order responses carry `"mode": "analyze"`, as on the web.
  See [Sandbox mode](../user/sandbox.md).
- With the API key's order mode set to Semi-Auto, a new order is not placed:
  it waits in the Action Center for your approval, and the SDK gets
  `{"status": "success", "message": "Order queued for approval in Action Center", "mode": "semi_auto", "pending_order_id": ...}`.
  Modify, cancel, close and every read run immediately.
- In the desktop, if the Semi-Auto setting cannot be read, the order is refused
  (HTTP 500, "OpenAlgo could not read whether your orders wait for approval in
  the Action Center (Semi-Auto), so nothing was sent...") rather than sent. The
  web sends it.

### Rate limits

The same as the web: 10 requests per second per address for `placeorder`,
`placesmartorder`, `modifyorder`, `cancelorder`, `optionsorder`,
`optionsmultiorder` and the GTT writes; 30 per minute for `optiongreeks`,
Telegram, WhatsApp and Sector Rotation (5 per minute for the Telegram
broadcast); 100 per second for everything else, the strategy module calls,
`basketorder`, `splitorder`, `cancelallorder` and `closeposition` included.
Over the limit the answer is HTTP 429. No rate-limit headers are sent. See
[Rate limiting](../api/rate-limiting.md).

### Order outcomes

- An `orderid` in the answer means the broker accepted the order, not that it
  filled. Check `orderstatus`, `tradebook` or the order-update stream.
- When the connection to the broker drops after an order was sent, the order
  may have been placed: `placeorder`, `placesmartorder` and `optionsorder`
  answer HTTP 502 with "The connection to the broker dropped after the order
  was sent, so it may have been placed. Check the order book before placing
  it again." A client timeout leaves the same doubt. Look in `orderbook`
  before sending it again.
- `basketorder` and `splitorder` give that sentence on the order it applies
  to: check every result, even when the call succeeds. Sending the whole call
  again can place the other orders twice.
- `strategy` on `cancelallorder()` and `closeposition()` is a label, not a
  filter: every working order is cancelled and every open position closed.
  See [order outcomes](../api/README.md#order-outcomes-and-retries).

## What works against the desktop

Every method in the `openalgo` SDK (checked against version 2.0.5) reaches an
endpoint the desktop serves:

| Area | SDK methods | Desktop |
|------|-------------|---------|
| Orders | `placeorder`, `placesmartorder`, `optionsorder`, `optionsmultiorder`, `basketorder`, `splitorder`, `modifyorder`, `cancelorder`, `cancelallorder`, `closeposition` | Same |
| Order information | `orderstatus`, `openposition`, `orderbook`, `tradebook`, `positionbook`, `holdings`, `funds`, `margin` | Same |
| GTT | `placegttorder`, `modifygttorder`, `cancelgttorder`, `gttorderbook` | Same. Live GTT on Zerodha, Upstox, Fyers, Dhan, Angel and Rupeezy, as on the web; every broker in analyzer mode |
| Market data | `quotes`, `multiquotes`, `depth`, `history`, `intervals` (and the legacy `interval`) | Same. `history(source="db")` reads Historify |
| Symbols | `symbol`, `search`, `expiry`, `instruments` | Same |
| Options | `optionsymbol`, `optionchain`, `syntheticfuture`, `optiongreeks` | Same |
| Calendar | `holidays`, `timings` | Same |
| Analyzer | `analyzerstatus`, `analyzertoggle` | Same |
| Alerts | `telegram` | Same |
| Alerts | `whatsapp` | Text messages work. Image and document attachments are refused (see below) |
| Strategy module | `strategylist`, `strategystatus`, `strategystart`, `strategystop`, `strategycloseall`, `strategycloseleg`, `strategyruns`, `strategyorders`, `strategyevents`, and the `Strategy` webhook class | Same |
| WebSocket | `connect`, `disconnect`, `subscribe_ltp` / `unsubscribe_ltp`, `subscribe_quote` / `unsubscribe_quote`, `subscribe_depth` / `unsubscribe_depth`, `subscribe_orders` / `unsubscribe_orders`, `get_ltp`, `get_quotes`, `get_depth`, `get_orders` | Same |
| Indicators | `openalgo.ta` | Runs inside the SDK on your computer and never calls the server, so it works unchanged |

The desktop also serves the web's endpoints that have no SDK method
(`ticker`, `multioptiongreeks`, `pnl/symbols`, `chart`, `sectorrotation`, the
Telegram bot management calls). Call them with plain HTTP; see the API reference under
[docs/api](../api/).

## What does not work against the desktop

- **WhatsApp attachments.** `client.whatsapp(..., image=...)` and
  `client.whatsapp(..., document=...)` answer HTTP 400 "image_path is not
  allowed" or "document_path is not allowed". The web sends files from an
  allow-listed folder on the server (`WHATSAPP_ATTACHMENT_ROOTS`); the desktop
  has no such folder and sends text only. Send the text without the
  attachment.
- **Portfolio and SIP backtesting endpoints.** `/api/v1/portfolio/*` (backtest,
  holdings, tearsheet, benchmarks) and `/api/v1/sip/*` (backtest, frequencies)
  are not in the desktop: they need the web's pandas backtesters, which are
  out of scope. No method in SDK 2.0.5 calls them; a direct HTTP call gets the
  404 for an unknown endpoint. Use OpenAlgo web for these.
- **Python strategies hosted by OpenAlgo.** The desktop has no Python Strategy
  Host (`/python`). Run your SDK script yourself, as an ordinary Python
  program, against the desktop's API.

Everything else on this page works as written.

## Examples

Refer to [order constants](order-constants.md) and
[symbol format](symbol-format.md) for the values the fields take.

### PlaceOrder example

To place a new market order:

```python
response = client.placeorder(
    strategy="Python",
    symbol="NHPC",
    action="BUY",
    exchange="NSE",
    price_type="MARKET",
    product="MIS",
    quantity=1
)
print(response)
```

Place Market Order Response

```json
{'orderid': '250408000989443', 'status': 'success'}
```

To place a new limit order:

```python
response = client.placeorder(
    strategy="Python",
    symbol="YESBANK",
    action="BUY",
    exchange="NSE",
    price_type="LIMIT",
    product="MIS",
    quantity="1",
    price="16",
    trigger_price="0",
    disclosed_quantity="0",
)
print(response)
```

Place Limit Order Response

```json
{'orderid': '250408001003813', 'status': 'success'}
```

In analyzer mode the response also carries `'mode': 'analyze'`.

### PlaceSmartOrder Example

To place a smart order considering the current position size:

```python
response = client.placesmartorder(
    strategy="Python",
    symbol="TATAMOTORS",
    action="SELL",
    exchange="NSE",
    price_type="MARKET",
    product="MIS",
    quantity=1,
    position_size=5
)
print(response)
```

Place Smart Market Order Response

```json
{'orderid': '250408000997543', 'status': 'success'}
```

### OptionsOrder Example

To place an ATM options order:

```python
response = client.optionsorder(
    strategy="python",
    underlying="NIFTY",
    exchange="NSE_INDEX",
    expiry_date="25AUG26",
    offset="ATM",
    option_type="CE",
    action="BUY",
    quantity=75,
    pricetype="MARKET",
    product="NRML",
    splitsize=0
)
print(response)
```

Place Options Order Response

```json
{
  "exchange": "NFO",
  "offset": "ATM",
  "option_type": "CE",
  "orderid": "25102800000006",
  "status": "success",
  "symbol": "NIFTY25AUG2625950CE",
  "underlying": "NIFTY25AUG26FUT",
  "underlying_ltp": 25966.05
}
```

For an in-the-money or out-of-the-money strike, change `offset`: `ITM1` to
`ITM50` or `OTM1` to `OTM50` (for example `offset="ITM4", option_type="PE"` or
`offset="OTM5", option_type="CE"`). The response has the same fields with the
resolved `symbol` and `offset`, plus `"mode": "analyze"` in analyzer mode.

### OptionsMultiOrder Example

To place an Iron Condor (same expiry):

```python
response = client.optionsmultiorder(
    strategy="Iron Condor Test",
    underlying="NIFTY",
    exchange="NSE_INDEX",
    expiry_date="25AUG26",
    legs=[
        {"offset": "OTM6", "option_type": "CE", "action": "BUY", "quantity": 75},
        {"offset": "OTM6", "option_type": "PE", "action": "BUY", "quantity": 75},
        {"offset": "OTM4", "option_type": "CE", "action": "SELL", "quantity": 75},
        {"offset": "OTM4", "option_type": "PE", "action": "SELL", "quantity": 75}
    ]
)
print(response)
```

Place OptionsMultiOrder Response

```json
{
    'status': 'success',
    'underlying': 'NIFTY',
    'underlying_ltp': 26050.45,
    'results': [
        {'action': 'BUY', 'leg': 1, 'mode': 'analyze', 'offset': 'OTM6',
         'option_type': 'CE', 'orderid': '25111996859688', 'status': 'success',
         'symbol': 'NIFTY25AUG2626350CE'},
        {'action': 'BUY', 'leg': 2, 'mode': 'analyze', 'offset': 'OTM6',
         'option_type': 'PE', 'orderid': '25111996042210', 'status': 'success',
         'symbol': 'NIFTY25AUG2625750PE'},
        {'action': 'SELL', 'leg': 3, 'mode': 'analyze', 'offset': 'OTM4',
         'option_type': 'CE', 'orderid': '25111922189638', 'status': 'success',
         'symbol': 'NIFTY25AUG2626250CE'},
        {'action': 'SELL', 'leg': 4, 'mode': 'analyze', 'offset': 'OTM4',
         'option_type': 'PE', 'orderid': '25111919252668', 'status': 'success',
         'symbol': 'NIFTY25AUG2625850PE'}
    ]
}
```

To place a Diagonal Spread (different expiries), give each leg its own
`expiry_date` and leave out the top-level one:

```python
response = client.optionsmultiorder(
    strategy="Diagonal Spread Test",
    underlying="NIFTY",
    exchange="NSE_INDEX",
    legs=[
        {"offset": "ITM2", "option_type": "CE", "action": "BUY", "quantity": 75, "expiry_date": "29SEP26"},
        {"offset": "OTM2", "option_type": "CE", "action": "SELL", "quantity": 75, "expiry_date": "25AUG26"}
    ]
)
print(response)
```

The response has the same shape, one `results` entry per leg.

### BasketOrder example

```python
basket_orders = [
    {
        "symbol": "BHEL",
        "exchange": "NSE",
        "action": "BUY",
        "quantity": 1,
        "pricetype": "MARKET",
        "product": "MIS"
    },
    {
        "symbol": "ZOMATO",
        "exchange": "NSE",
        "action": "SELL",
        "quantity": 1,
        "pricetype": "MARKET",
        "product": "MIS"
    }
]
response = client.basketorder(orders=basket_orders)
print(response)
```

**Basket Order Response**

```json
{
  "status": "success",
  "results": [
    {"symbol": "BHEL", "status": "success", "orderid": "250408000999544"},
    {"symbol": "ZOMATO", "status": "success", "orderid": "250408000997545"}
  ]
}
```

### SplitOrder example

```python
response = client.splitorder(
    symbol="YESBANK",
    exchange="NSE",
    action="SELL",
    quantity=105,
    splitsize=20,
    price_type="MARKET",
    product="MIS"
)
print(response)
```

**SplitOrder Response**

```json
{
  "status": "success",
  "split_size": 20,
  "total_quantity": 105,
  "results": [
    {"order_num": 1, "orderid": "250408001021467", "quantity": 20, "status": "success"},
    {"order_num": 2, "orderid": "250408001021459", "quantity": 20, "status": "success"},
    {"order_num": 3, "orderid": "250408001021466", "quantity": 20, "status": "success"},
    {"order_num": 4, "orderid": "250408001021470", "quantity": 20, "status": "success"},
    {"order_num": 5, "orderid": "250408001021471", "quantity": 20, "status": "success"},
    {"order_num": 6, "orderid": "250408001021472", "quantity": 5, "status": "success"}
  ]
}
```

### ModifyOrder Example

```python
response = client.modifyorder(
    order_id="250408001002736",
    strategy="Python",
    symbol="YESBANK",
    action="BUY",
    exchange="NSE",
    price_type="LIMIT",
    product="CNC",
    quantity=1,
    price=16.5
)
print(response)
```

**Modify Order Response**

```json
{'orderid': '250408001002736', 'status': 'success'}
```

### CancelOrder Example

```python
response = client.cancelorder(
    order_id="250408001002736",
    strategy="Python"
)
print(response)
```

**CancelOrder Response**

```json
{'orderid': '250408001002736', 'status': 'success'}
```

### CancelAllOrder Example

Cancels all open orders and trigger-pending orders:

```python
response = client.cancelallorder(strategy="Python")
print(response)
```

**CancelAllOrder Response**

```json
{
  "status": "success",
  "message": "Canceled 5 orders. Failed to cancel 0 orders.",
  "canceled_orders": [
    "250408001042620",
    "250408001042667",
    "250408001042642",
    "250408001043015",
    "250408001043386"
  ],
  "failed_cancellations": []
}
```

### ClosePosition Example

Closes all open positions across exchanges:

```python
response = client.closeposition(strategy="Python")
print(response)
```

**ClosePosition Response**

```json
{'message': 'All Open Positions Squared Off', 'status': 'success'}
```

### OrderStatus Example

```python
response = client.orderstatus(
    order_id="250828000185002",
    strategy="Test Strategy"
)
print(response)
```

**OrderStatus Response**

```json
{
  "data": {
    "action": "BUY",
    "average_price": 18.95,
    "exchange": "NSE",
    "order_status": "complete",
    "orderid": "250828000185002",
    "price": 0,
    "pricetype": "MARKET",
    "product": "MIS",
    "quantity": "1",
    "symbol": "YESBANK",
    "timestamp": "28-Aug-2025 09:59:10",
    "trigger_price": 0
  },
  "status": "success"
}
```

### OpenPosition Example

```python
response = client.openposition(
    strategy="Test Strategy",
    symbol="YESBANK",
    exchange="NSE",
    product="MIS"
)
print(response)
```

OpenPosition Response

```json
{'quantity': '-10', 'status': 'success'}
```

### PlaceGTTOrder Example

A GTT (Good Till Triggered) order is a price trigger that sits with the broker
until LTP crosses your level, then places the underlying order automatically.

There are two shapes:

| Type | Use when | Triggers | Orders fired |
|------|----------|----------|--------------|
| `SINGLE` | One entry or exit at a level | 1 | 1 |
| `OCO` | You hold a position and want both a stoploss and a target, whichever hits first | 2 | 1 of 2. Where the broker has a native OCO the other is cancelled; Angel One keeps two separate rules, and on Upstox an OCO opens a position first ([PlaceGTTOrder](../api/order-management/placegttorder.md#angel-one-an-oco-is-two-separate-rules)) |

For a **SINGLE**, exactly one of `triggerprice_sl` / `triggerprice_tg` carries
your level and the other stays `0`. Use `triggerprice_sl` for a level **below**
LTP (sell stop-loss, buy the dip) and `triggerprice_tg` for one **above**
(breakout buy, sell at target).

For an **OCO**, all four fields are required: `triggerprice_sl` with its
`stoploss` limit, and `triggerprice_tg` with its `target` limit, where
`triggerprice_sl < triggerprice_tg`.

GTT accepts `CNC` and `NRML` only. `MIS` is refused: a GTT can sit for days and
MIS is squared off the same session.

Live GTT works with Zerodha, Upstox, Fyers, Dhan, Angel and Rupeezy, as on the
web; other brokers answer HTTP 501 "GTT orders are not supported for broker
'...' yet". In analyzer mode GTT works with every broker.

```python
# SINGLE - "Buy IDEA if it dips to 9.55, with a LIMIT order at 9.50"
# LTP is above 9.55, so the trigger sits below it -> triggerprice_sl
response = client.placegttorder(
    strategy="My GTT Strategy",
    symbol="IDEA",
    action="BUY",
    exchange="NSE",
    product="CNC",
    quantity=1,
    price_type="LIMIT",
    price=9.50,
    triggerprice_sl=9.55
)
print(response)
```

```python
# SINGLE - "Buy RELIANCE at MARKET if it breaks above 1450"
# LTP is below 1450, so the trigger sits above it -> triggerprice_tg
response = client.placegttorder(
    strategy="My GTT Strategy",
    symbol="RELIANCE",
    action="BUY",
    exchange="NSE",
    product="CNC",
    quantity=1,
    price_type="MARKET",
    price=0,
    triggerprice_tg=1450
)
```

```python
# OCO - "I hold 5 INFY. Stop me out at 1480, take profit at 1620"
# price is 0: OCO prices each leg separately through stoploss and target
response = client.placegttorder(
    strategy="Bracket OCO",
    trigger_type="OCO",
    symbol="INFY",
    action="SELL",
    exchange="NSE",
    product="CNC",
    quantity=5,
    price_type="LIMIT",
    price=0,
    triggerprice_sl=1480,
    stoploss=1478,
    triggerprice_tg=1620,
    target=1622
)
```

PlaceGTTOrder Response:

```json
{"status": "success", "trigger_id": "23132604291205"}
```

Save the `trigger_id`: modify and cancel both need it.

### ModifyGTTOrder Example

Modify is a **full replacement**, not a patch: pass everything you want to
keep, not only the values that changed.

```python
response = client.modifygttorder(
    trigger_id="23132604291205",
    strategy="My GTT Strategy",
    symbol="IDEA",
    action="BUY",
    exchange="NSE",
    product="CNC",
    quantity=1,
    price_type="LIMIT",
    price=9.60,           # was 9.50
    triggerprice_sl=9.65  # was 9.55
)
print(response)
```

ModifyGTTOrder Response:

```json
{"status": "success", "trigger_id": "23132604291205"}
```

Trigger prices, limit prices, quantity and pricetype are modifiable.
`trigger_type`, `symbol`, `exchange` and `action` are not: cancel and re-place
instead. Only active GTTs can be modified.

### CancelGTTOrder Example

```python
response = client.cancelgttorder(
    trigger_id="23132604291205",
    strategy="My GTT Strategy"
)
print(response)
```

CancelGTTOrder Response:

```json
{"status": "success", "trigger_id": "23132604291205"}
```

Cancelling an OCO cancels both legs; there is no per-leg cancel. On Angel One
the two rules are cancelled one after the other, and a failure can leave one
active: check `gttorderbook()` before trying again.

### GTTOrderBook Example

By default this lists **active** triggers only. Pass `status="all"` to include
the history as well (triggered, cancelled, expired, rejected), active first.

```python
# Active triggers only (default)
response = client.gttorderbook()
print(response)

# Active triggers first, then the history
response = client.gttorderbook(status="all")
for gtt in response["data"]:
    print(gtt["trigger_id"], gtt["status"], gtt["symbol"], gtt["trigger_prices"])
```

GTTOrderBook Response:

```json
{
  "status": "success",
  "data": [
    {
      "trigger_id": "23132604291205",
      "trigger_type": "single",
      "status": "active",
      "symbol": "IDEA",
      "exchange": "NSE",
      "trigger_prices": [9.55],
      "last_price": 9.50,
      "legs": [
        {
          "action": "BUY",
          "quantity": 1,
          "price": 9.50,
          "pricetype": "LIMIT",
          "product": "CNC"
        }
      ],
      "created_at": "2026-04-29 12:18:42",
      "updated_at": "",
      "expires_at": ""
    }
  ]
}
```

`trigger_type` is `single` or `two-leg` (an OCO). `trigger_prices` is sorted
ascending: a SINGLE has one price and one leg, an OCO two of each with the
stoploss first.

The SDK refuses an impossible trigger spec before anything is sent:

```python
# SINGLE with no trigger price at all
client.placegttorder(symbol="IDEA", action="BUY", exchange="NSE",
                     product="CNC", quantity=1, price=9.50)
# {'status': 'error',
#  'message': 'SINGLE GTT requires a positive triggerprice_sl or triggerprice_tg.',
#  'error_type': 'validation_error'}
```

### Quotes Example

```python
response = client.quotes(symbol="RELIANCE", exchange="NSE")
print(response)
```

**Quotes Response**

```json
{
  "status": "success",
  "data": {
    "open": 1172.0,
    "high": 1196.6,
    "low": 1163.3,
    "ltp": 1187.75,
    "ask": 1188.0,
    "bid": 1187.85,
    "prev_close": 1165.7,
    "volume": 14414545
  }
}
```

### MultiQuotes Example

```python
response = client.multiquotes(symbols=[
    {"symbol": "RELIANCE", "exchange": "NSE"},
    {"symbol": "TCS", "exchange": "NSE"},
    {"symbol": "INFY", "exchange": "NSE"}
])
print(response)
```

**MultiQuotes Response** (trimmed to two symbols)

```json
{
  "status": "success",
  "results": [
    {
      "symbol": "RELIANCE",
      "exchange": "NSE",
      "data": {
        "open": 1542.3, "high": 1571.6, "low": 1540.5, "ltp": 1569.9,
        "prev_close": 1539.7, "ask": 1569.9, "bid": 0, "oi": 0,
        "volume": 14054299
      }
    },
    {
      "symbol": "TCS",
      "exchange": "NSE",
      "data": {
        "open": 3118.8, "high": 3178, "low": 3117, "ltp": 3162.9,
        "prev_close": 3119.2, "ask": 0, "bid": 3162.9, "oi": 0,
        "volume": 2508527
      }
    }
  ]
}
```

### Depth Example

```python
response = client.depth(symbol="SBIN", exchange="NSE")
print(response)
```

**Depth Response** (five levels each side; trimmed to two here)

```json
{
  "status": "success",
  "data": {
    "open": 760.0,
    "high": 774.0,
    "low": 758.15,
    "ltp": 769.6,
    "ltq": 205,
    "prev_close": 746.9,
    "volume": 9362799,
    "oi": 161265750,
    "totalbuyqty": 591351,
    "totalsellqty": 835701,
    "asks": [
      {"price": 769.6, "quantity": 767},
      {"price": 769.65, "quantity": 115}
    ],
    "bids": [
      {"price": 769.4, "quantity": 886},
      {"price": 769.35, "quantity": 212}
    ]
  }
}
```

### History Example

From the broker:

```python
response = client.history(
    symbol="SBIN",
    exchange="NSE",
    interval="5m",
    start_date="2025-04-01",
    end_date="2025-04-08",
    source="api"
)
print(response)
```

From Historify (data you downloaded on the Historify page of OpenAlgo
Desktop, stored on this computer):

```python
response = client.history(
    symbol="SBIN",
    exchange="NSE",
    interval="5m",
    start_date="2025-04-01",
    end_date="2025-04-08",
    source="db"
)
print(response)
```

**History Response** (the SDK returns a pandas DataFrame)

```
                            close    high     low    open  volume
timestamp
2025-04-01 09:15:00+05:30  772.50  774.00  763.20  766.50  318625
2025-04-01 09:20:00+05:30  773.20  774.95  772.10  772.45  197189
2025-04-01 09:25:00+05:30  775.15  775.60  772.60  773.20  227544
...                           ...     ...     ...     ...     ...
2025-04-08 14:15:00+05:30  770.05  770.50  769.05  769.05  158299
2025-04-08 14:20:00+05:30  769.95  770.50  769.40  770.05  125485

[437 rows x 5 columns]
```

### Intervals Example

```python
response = client.intervals()
print(response)
```

**Intervals Response** (depends on the connected broker)

```json
{
  "status": "success",
  "data": {
    "months": [],
    "weeks": [],
    "days": ["D"],
    "hours": ["1h"],
    "minutes": ["1m", "3m", "5m", "10m", "15m", "30m"],
    "seconds": []
  }
}
```

### OptionChain Example

Leave out `strike_count` to fetch the whole chain for an expiry.

```python
chain = client.optionchain(
    underlying="NIFTY",
    exchange="NSE_INDEX",
    expiry_date="25AUG26",
    strike_count=10
)
```

Pass `with_greeks=True` for implied volatility and Greeks on every leg. They
are derived from the quotes the call already fetches, so this adds no extra
broker requests.

```python
chain = client.optionchain(
    underlying="NIFTY",
    exchange="NSE_INDEX",
    expiry_date="25AUG26",
    strike_count=10,
    with_greeks=True,   # implied_volatility, delta, gamma, theta, vega per leg
    interest_rate=0     # annualized percentage, defaults to 0
)

for row in chain["chain"]:
    ce, pe = row.get("ce") or {}, row.get("pe") or {}
    print(row["strike"],
          ce.get("implied_volatility"), ce.get("delta"),
          pe.get("implied_volatility"), pe.get("delta"))
```

Greeks use Black-76, priced off the forward that the service derives from the
ATM call and put by put-call parity and reports as `forward_price`. Vega is
per 1% change in volatility and theta per calendar day, matching
`optiongreeks`. A leg whose Greeks cannot be computed omits those fields.

**OptionChain Response** (trimmed to one strike; the Greek fields appear only
with `with_greeks`)

```json
{
    "status": "success",
    "underlying": "NIFTY",
    "underlying_ltp": 24560.15,
    "underlying_prev_close": 24570.65,
    "expiry_date": "25AUG26",
    "expiry_ts": 1787652000,
    "server_ts": 1786356402,
    "atm_strike": 24550.0,
    "quotes_included": true,
    "greeks_included": true,
    "forward_price": 24580.0,
    "chain": [
        {
            "strike": 24550.0,
            "ce": {
                "symbol": "NIFTY25AUG2624550CE",
                "label": "ATM",
                "ltp": 311.6,
                "bid": 311.1,
                "ask": 312.1,
                "bid_qty": 1500,
                "ask_qty": 2250,
                "open": 330.3,
                "high": 345.9,
                "low": 286.65,
                "prev_close": 324.05,
                "volume": 2841075,
                "oi": 4218300,
                "lotsize": 75,
                "tick_size": 0.05,
                "implied_volatility": 14.92,
                "delta": 0.5221,
                "gamma": 0.000536,
                "theta": -9.8729,
                "vega": 19.8452
            },
            "pe": {
                "symbol": "NIFTY25AUG2624550PE",
                "label": "ATM",
                "ltp": 281.6,
                "bid": 281.1,
                "ask": 282.1,
                "bid_qty": 2100,
                "ask_qty": 1875,
                "open": 267.5,
                "high": 304.15,
                "low": 250.6,
                "prev_close": 273.15,
                "volume": 3162450,
                "oi": 5104275,
                "lotsize": 75,
                "tick_size": 0.05,
                "implied_volatility": 14.92,
                "delta": -0.4779,
                "gamma": 0.000536,
                "theta": -9.8729,
                "vega": 19.8452
            }
        }
    ]
}
```

### Symbol Example

```python
response = client.symbol(symbol="NIFTY25AUG26FUT", exchange="NFO")
print(response)
```

**Symbol Response**

```json
{
  "data": {
    "brexchange": "NSE_FO",
    "brsymbol": "NIFTY FUT 25 AUG 26",
    "exchange": "NFO",
    "expiry": "25-AUG-26",
    "freeze_qty": 1800,
    "id": 57900,
    "instrumenttype": "FUT",
    "lotsize": 75,
    "name": "NIFTY",
    "strike": 0,
    "symbol": "NIFTY25AUG26FUT",
    "tick_size": 10,
    "token": "NSE_FO|49543"
  },
  "status": "success"
}
```

`brsymbol`, `brexchange` and `token` are your broker's values and differ from
broker to broker.

### Search Example

```python
response = client.search(query="NIFTY 26000 DEC CE", exchange="NFO")
print(response)
```

**Search Response** (trimmed to two matches)

```json
{
  "data": [
    {
      "brexchange": "NSE_FO",
      "brsymbol": "NIFTY 26000 CE 29 DEC 26",
      "exchange": "NFO",
      "expiry": "29-DEC-26",
      "freeze_qty": 1800,
      "instrumenttype": "CE",
      "lotsize": 75,
      "name": "NIFTY",
      "strike": 26000,
      "symbol": "NIFTY29DEC2626000CE",
      "tick_size": 5,
      "token": "NSE_FO|71505"
    },
    {
      "brexchange": "NSE_FO",
      "brsymbol": "NIFTY 26000 CE 28 DEC 27",
      "exchange": "NFO",
      "expiry": "28-DEC-27",
      "freeze_qty": 1800,
      "instrumenttype": "CE",
      "lotsize": 75,
      "name": "NIFTY",
      "strike": 26000,
      "symbol": "NIFTY28DEC2726000CE",
      "tick_size": 5,
      "token": "NSE_FO|53628"
    }
  ],
  "message": "Found 2 matching symbols",
  "status": "success"
}
```

### OptionSymbol Example

```python
response = client.optionsymbol(
    underlying="NIFTY",
    exchange="NSE_INDEX",
    expiry_date="25AUG26",
    offset="ATM",
    option_type="CE"
)
print(response)
```

**OptionSymbol Response**

```json
{
  "status": "success",
  "symbol": "NIFTY25AUG2625950CE",
  "exchange": "NFO",
  "lotsize": 75,
  "tick_size": 5,
  "freeze_qty": 1800,
  "underlying_ltp": 25966.4
}
```

`offset="ITM3"` or `offset="OTM4"` resolve the strike that many steps in or
out of the money; the response has the same fields.

### SyntheticFuture Example

```python
response = client.syntheticfuture(
    underlying="NIFTY",
    exchange="NSE_INDEX",
    expiry_date="25AUG26"
)
print(response)
```

**SyntheticFuture Response**

```
{
 'atm_strike': 25900.0,
 'expiry': '25AUG26',
 'status': 'success',
 'synthetic_future_price': 25980.05,
 'underlying': 'NIFTY',
 'underlying_ltp': 25910.05
}
```

### OptionGreeks Example

```python
response = client.optiongreeks(
    symbol="NIFTY25AUG2626000CE",
    exchange="NFO",
    interest_rate=0.00,
    underlying_symbol="NIFTY",
    underlying_exchange="NSE_INDEX"
)
print(response)
```

**OptionGreeks Response**

```
{
 'days_to_expiry': 28.5071,
 'exchange': 'NFO',
 'expiry_date': '25-Aug-2026',
 'greeks': {'delta': 0.4967,
  'gamma': 0.000352,
  'rho': 9.733994,
  'theta': -7.919,
  'vega': 28.9489},
 'implied_volatility': 15.6,
 'interest_rate': 0.0,
 'option_price': 435,
 'option_type': 'CE',
 'spot_price': 25966.05,
 'status': 'success',
 'strike': 26000.0,
 'symbol': 'NIFTY25AUG2626000CE',
 'underlying': 'NIFTY'
}
```

`expiry_date` here is `DD-Mon-YYYY`, unlike `expiry`, which returns
`DD-MMM-YY`. Both are as on the web.

### Expiry Example

```python
response = client.expiry(
    symbol="NIFTY",
    exchange="NFO",
    instrumenttype="options"
)
print(response)
```

**Expiry Response** (trimmed)

```
{'data': ['10-JUL-25',
  '17-JUL-25',
  '24-JUL-25',
  '31-JUL-25',
  '07-AUG-25',
  ...
  '25-JUN-30'],
 'message': 'Found 18 expiry dates for NIFTY options in NFO',
 'status': 'success'}
```

### Instruments Example

```python
response = client.instruments(exchange="NSE")
print(response.tail())
```

The SDK returns a pandas DataFrame with the master contract columns:
`symbol`, `brsymbol`, `name`, `exchange`, `brexchange`, `token`, `expiry`,
`strike`, `lotsize`, `instrumenttype`, `tick_size`. Without `exchange` the SDK
downloads every exchange in turn and joins them.

### Telegram Alert Example

Before this works: set up the Telegram bot on the **Telegram** page of OpenAlgo
Desktop, start it, and link your account by sending `/link <APIKEY>
http://127.0.0.1:5000` to the bot. `username` is your OpenAlgo login name.

```python
response = client.telegram(
    username="<openalgo_loginid>",
    message="NIFTY crossed 26000!"
)
print(response)
```

**Telegram Alert Response**

```json
{
  "message": "Notification queued for delivery",
  "status": "success"
}
```

By default the message is queued and sent in the background. Pass
`wait_for_delivery=True` to send it before the call returns; the message is
then "Notification sent successfully". The web answers the same way (its own
SDK page shows only the second message).

Errors you may see:

| HTTP | Message | Cause and fix |
|------|---------|---------------|
| 409 | Telegram bot is stopped. Start the bot to send notifications. | Start the bot on the Telegram page |
| 404 | User not found or not linked to Telegram | Link your account with `/link` in the bot |
| 401 | Invalid or missing API key | Check the key on the API Key page |

### WhatsApp Alert Example

Before this works: open the **WhatsApp** page in OpenAlgo Desktop, choose
**Pair**, and scan the QR code with your phone (or use the pair code). Pairing
is only on that page, never through the API, so a leaked API key cannot pair
another device. The bot starts again by itself each time you sign in to
OpenAlgo Desktop, and stops when you log out.

#### Send to yourself (simplest case)

```python
response = client.whatsapp("NIFTY crossed 26000!")
print(response)
```

**WhatsApp Alert Response (`wait_for_delivery=True`, the default)**

```json
{
  "status": "success",
  "message": "Delivered to 1, failed 0",
  "data": {
    "sent":    ["<self>"],
    "failed":  [],
    "skipped": 0
  }
}
```

#### Send to a single phone number

```python
response = client.whatsapp(
    "Order placed: BUY RELIANCE x 10 @ MARKET",
    to="919876543210",
)
```

#### Small broadcast (up to 5 recipients)

```python
response = client.whatsapp(
    "Server maintenance starting in 10 minutes",
    to=["919876543210", "919812345678", "919900112233"],
)
```

Numbers beyond the fifth are dropped.

#### Fire-and-forget (skip the delivery report)

```python
response = client.whatsapp(
    "Stop-loss hit on BANKNIFTY!",
    wait_for_delivery=False,
)
# {'status': 'success', 'message': 'Queued for 1 recipient(s)', 'queued': 1}
```

#### Send to a linked OpenAlgo user

```python
response = client.whatsapp(
    "Position update: BANKNIFTY 48000 CE now at +21% P&L.",
    username="alice",
)
```

#### Images and documents

Not in the desktop. `image=` and `document=` answer HTTP 400 "image_path is not
allowed" or "document_path is not allowed", and nothing is sent. Send text
only.

#### Receiving messages (bot commands)

Type slash commands from your own phone in the **"Message yourself"** chat.
Messages from other people cannot drive the bot.

```
/help                   List all commands
/status                 Bot connection + paired status
/orderbook              Today's orders
/tradebook              Today's trades
/positions              Open positions
/holdings               Holdings
/funds                  Available cash / margin
/pnl                    Net P&L
/quote RELIANCE NSE     Last traded price
/closeall               Square off all positions
/mode                   Live or analyze mode
```

Errors you may see: HTTP 409 when WhatsApp is not paired or not connected
(pair it on the WhatsApp page), 404 "Username not found or not linked to
WhatsApp", 400 "Message must not exceed 4096 characters".

### Funds Example

```python
response = client.funds()
print(response)
```

**Funds Response**

```json
{
  "status": "success",
  "data": {
    "availablecash": "320.66",
    "collateral": "0.00",
    "m2mrealized": "3.27",
    "m2munrealized": "-7.88",
    "utiliseddebits": "679.34"
  }
}
```

### Margin Example

```python
response = client.margin(positions=[
    {
        "symbol": "NIFTY25AUG2625000CE",
        "exchange": "NFO",
        "action": "BUY",
        "product": "NRML",
        "pricetype": "MARKET",
        "quantity": "75"
    },
    {
        "symbol": "NIFTY25AUG2625500CE",
        "exchange": "NFO",
        "action": "SELL",
        "product": "NRML",
        "pricetype": "MARKET",
        "quantity": "75"
    }
])
```

**Margin Response**

```json
{
    "status": "success",
    "data": {
      "total_margin_required": 91555.7625,
      "span_margin": 0.0,
      "exposure_margin": 91555.7625
    }
}
```

### OrderBook Example

```python
response = client.orderbook()
print(response)
```

```json
{
  "status": "success",
  "data": {
    "orders": [
      {
        "action": "BUY",
        "symbol": "RELIANCE",
        "exchange": "NSE",
        "orderid": "250408000989443",
        "product": "MIS",
        "quantity": "1",
        "price": 1186.0,
        "pricetype": "MARKET",
        "order_status": "complete",
        "trigger_price": 0.0,
        "timestamp": "08-Apr-2025 13:58:03"
      }
    ],
    "statistics": {
      "total_buy_orders": 1.0,
      "total_sell_orders": 0.0,
      "total_completed_orders": 1.0,
      "total_open_orders": 0.0,
      "total_rejected_orders": 0.0
    }
  }
}
```

Every book (orders, trades, positions, holdings) returns OpenAlgo symbols,
never your broker's trading symbols, so the same script works with any broker.

### TradeBook Example

A trade row is a fill, so one order can have several rows. How the time is
written and the order of the rows depend on the broker.

```python
response = client.tradebook()
print(response)
```

TradeBook Response

```python
{
  "status": "success",
  "data": [
    {
      "action": "BUY",
      "symbol": "RELIANCE",
      "exchange": "NSE",
      "orderid": "250408000989443",
      "product": "MIS",
      "quantity": 1.0,
      "average_price": 1180.1,
      "timestamp": "13:58:03",
      "trade_value": 1180.1
    }
  ]
}
```

### PositionBook Example

```python
response = client.positionbook()
print(response)
```

**PositionBook Response**

```json
{
  "status": "success",
  "data": [
    {
      "symbol": "NHPC",
      "exchange": "NSE",
      "product": "MIS",
      "quantity": "-1",
      "average_price": "83.74",
      "ltp": "83.72",
      "pnl": "0.02"
    },
    {
      "symbol": "YESBANK",
      "exchange": "NSE",
      "product": "MIS",
      "quantity": "-104",
      "average_price": "17.2",
      "ltp": "17.31",
      "pnl": "-10.44"
    }
  ]
}
```

### Holdings Example

```python
response = client.holdings()
print(response)
```

Holdings Response

```json
{
  "status": "success",
  "data": {
    "holdings": [
      {
        "symbol": "RELIANCE",
        "exchange": "NSE",
        "product": "CNC",
        "quantity": 1,
        "pnl": -149.0,
        "pnlpercent": -11.1
      },
      {
        "symbol": "CANBK",
        "exchange": "NSE",
        "product": "CNC",
        "quantity": 5,
        "pnl": -69.0,
        "pnlpercent": -13.43
      }
    ],
    "statistics": {
      "totalholdingvalue": 1768.0,
      "totalinvvalue": 2001.0,
      "totalprofitandloss": -233.15,
      "totalpnlpercentage": -11.65
    }
  }
}
```

### Holidays Example

```python
response = client.holidays(year=2026)
print(response)
```

#### Holidays Response (trimmed)

```json
{'data': [
    {'closed_exchanges': ['NSE', 'BSE', 'NFO', 'BFO', 'CDS', 'BCD', 'MCX'],
     'date': '2026-01-26', 'description': 'Republic Day',
     'holiday_type': 'TRADING_HOLIDAY', 'open_exchanges': []},
    {'closed_exchanges': ['NSE', 'BSE', 'NFO', 'BFO', 'CDS', 'BCD'],
     'date': '2026-03-10', 'description': 'Holi',
     'holiday_type': 'TRADING_HOLIDAY',
     'open_exchanges': [{'end_time': 1741677900000, 'exchange': 'MCX',
                         'start_time': 1741624200000}]}
  ],
 'status': 'success'}
```

`start_time` and `end_time` are epoch milliseconds.

### Timings Example

```python
response = client.timings(date="2025-12-19")
print(response)
```

#### Timings Response

```json
{'data': [
    {'end_time': 1766138400000, 'exchange': 'NSE', 'start_time': 1766115900000},
    {'end_time': 1766138400000, 'exchange': 'BSE', 'start_time': 1766115900000},
    {'end_time': 1766138400000, 'exchange': 'NFO', 'start_time': 1766115900000},
    {'end_time': 1766138400000, 'exchange': 'BFO', 'start_time': 1766115900000},
    {'end_time': 1766168700000, 'exchange': 'MCX', 'start_time': 1766115000000},
    {'end_time': 1766143800000, 'exchange': 'BCD', 'start_time': 1766115000000},
    {'end_time': 1766143800000, 'exchange': 'CDS', 'start_time': 1766115000000}
  ], 'status': 'success'
}
```

### Analyzer Status Example

```python
response = client.analyzerstatus()
print(response)
```

Analyzer Status Response

```json
{'data': {'analyze_mode': True, 'mode': 'analyze', 'total_logs': 2},
 'status': 'success'}
```

### Analyzer Toggle Example

```python
# Switch to analyzer mode (orders go to the sandbox)
response = client.analyzertoggle(mode=True)
print(response)
```

Analyzer Toggle Response

```
{'data': {'analyze_mode': True,
  'message': 'Analyzer mode switched to analyze',
  'mode': 'analyze',
  'total_logs': 2},
 'status': 'success'}
```

## Strategy Module

The strategy module (`/strategy` in the app) runs multi-leg options
strategies with risk management, plus a signal-driven mode for TradingView
alerts. The field-by-field reference is in
[Strategy services](../api/strategy-services/README.md); how the module and
its risk rules behave in the desktop is in
[Strategy module and RMS](strategy_rms_documentation.md).

Two surfaces reach it, with different credentials:

| Surface | Credential | Use for |
|---------|-----------|---------|
| `api(api_key=...)` | Your OpenAlgo API key | Lifecycle and reads: list, status, start, stop, close_all, close_leg, runs, orders, events |
| `Strategy(...)` | The strategy's `oaws_` webhook token | The webhook at `/strategy/webhook/<token>`, which is what TradingView posts to |

Building a strategy stays in the app's strategy wizard. Nothing on the API-key
surface can create a strategy, edit it, enable live trading, rotate a webhook
token or delete anything.

Two strategy kinds, and each refuses the other's vocabulary:

- **batch**: a multi-leg spread entered and exited as a unit. `start` / `stop`.
- **signal**: one alert moves one leg. `long_entry` / `long_exit` /
  `short_entry` / `short_exit`. There is no start and no mode: the first signal
  after the session boundary opens the run.

Rules to know before you call anything:

1. **`mode` on start is required and never defaulted.** In the SDK it is a
   keyword argument with no default, so leaving it out is a `TypeError` rather
   than a live order.
2. **Live is opt-in per strategy.** A new strategy is sandbox-only, and
   `mode="live"` is refused with a 409 until you enable live trading on the
   strategy page.
3. **An accepted stop is not proof of flatness.** Read `stop_pending` and the
   per-leg outcomes.
4. **An unknown strategy id answers 404.**

In the desktop, a live run's orders go only through the broker account the run
was opened on. If you switch broker or account while a live run is open, its
exits are refused rather than sent through the other account, and the run
stays open and managed; reconnect the account it was opened on to exit it. The
web has no such check.

### StrategyList Example

```python
response = client.strategylist()
print(response)

# Optional filters. An out-of-vocabulary status is a 400, not an empty list.
client.strategylist(status="running")
client.strategylist(q="NIFTY")
```

StrategyList Response:

```json
{
  "status": "success",
  "data": [
    {
      "id": 7,
      "name": "NIFTY Short Straddle",
      "strategy_kind": "batch",
      "direction": "both",
      "underlying": "NIFTY",
      "underlying_exchange": "NSE_INDEX",
      "strategy_type": "intraday",
      "entry_time": "09:20",
      "exit_time": "15:10",
      "product": "NRML",
      "pricetype": "MARKET",
      "overall_sl_mtm": -5000.0,
      "overall_target_mtm": 8000.0,
      "live_enabled": false,
      "status": "running",
      "current_run_id": 42,
      "last_finalized_run": {"id": 41, "pnl_realized": 1250.0, "stopped_at": "2026-08-29T09:40:11.482913+00:00"}
    }
  ]
}
```

The list omits `legs`; call `strategystatus` for one strategy's legs.

### StrategyStatus Example

```python
response = client.strategystatus(strategy_id=7)
print(response)
```

StrategyStatus Response:

```json
{
  "status": "success",
  "data": {
    "id": 7,
    "name": "NIFTY Short Straddle",
    "status": "running",
    "current_run_id": 42,
    "legs": [
      {"id": 1, "segment": "options", "position": "S", "lots": 1, "option_type": "CE",
       "strike_mode": "atm", "atm_offset": "ATM", "expiry": "weekly",
       "sl_pts": 30, "target_pts": 60, "trail": {"x": 10, "y": 5}}
    ]
  },
  "run": {
    "id": 42,
    "mode": "sandbox",
    "broker": "sandbox",
    "started_at": "2026-08-30T03:50:11.402118+00:00",
    "stopped_at": null,
    "stop_reason": null,
    "stop_requested_at": null,
    "stop_requested_reason": null,
    "pnl_realized": 0.0,
    "pnl_peak": 0.0,
    "pnl_trough": 0.0,
    "trigger_source": "manual",
    "resolved_expiries": {"1": "04-SEP-26", "2": "04-SEP-26"}
  }
}
```

`run` is `null` when the strategy has no current run. A populated
`stop_requested_reason` means a stop is recorded but not yet confirmed flat.

### StrategyStart Example

Starts a **batch** strategy: every leg's entry order is placed.

```python
response = client.strategystart(strategy_id=7, mode="sandbox")
print(response)

# Partial success is a 200. Check each leg.
for leg in response.get("legs", []):
    if not leg["ok"]:
        print(f"leg {leg['leg_id']} rejected: {leg['error']}")
```

StrategyStart Response:

```json
{
  "status": "success",
  "run_id": 42,
  "mode": "sandbox",
  "legs": [
    {"leg_id": 1, "ok": true, "acknowledged": true,
     "symbol": "NIFTY04SEP2624500CE", "broker_order_id": "26083004118201", "error": null},
    {"leg_id": 2, "ok": true, "acknowledged": true,
     "symbol": "NIFTY04SEP2624500PE", "broker_order_id": "26083004118244", "error": null}
  ]
}
```

A second start against a running strategy answers 409.

### StrategyStop Example

Exits every owned position at market.

```python
response = client.strategystop(strategy_id=7)
print(response)
```

StrategyStop Response:

```json
{
  "status": "success",
  "run_id": 42,
  "stop_pending": true,
  "exits": [
    {"leg_id": 1, "ok": true, "position_ref": "969bc536b1c14d15992f730c2c136d7a",
     "exit_owner": "live", "error": null}
  ]
}
```

`stop_pending: true` means the exits were accepted but the run stays open and
managed until fills prove every position is flat. A 409 can also carry
`stop_pending: true` when an exit was refused; retry the stop.

### StrategyCloseAll Example

Same stop mechanics as `strategystop`, recorded as an operator flatten.

```python
response = client.strategycloseall(strategy_id=7)
print(response)
```

### StrategyCloseLeg Example

Exits one leg at market; the run continues with the rest. `leg_id` is the
leg's id in the strategy (`legs[].id` from `strategystatus`), not an order id.

```python
response = client.strategycloseleg(strategy_id=7, leg_id=2)
print(response)
```

StrategyCloseLeg Response:

```json
{
  "status": "success",
  "run_id": 42,
  "leg_id": 2,
  "run_stopped": false,
  "exits": [
    {"leg_id": 2, "ok": true, "position_ref": "80bb5fc9333f4922a582229f06a0fe45",
     "exit_owner": "live", "error": null}
  ]
}
```

A `leg_id` that names no open leg is a 409, not a 404.

### StrategyRuns Example

Every activation of a strategy, newest first. `limit` is 1 to 500; outside
that range is a 400.

```python
response = client.strategyruns(strategy_id=7, limit=10)
print(response)
```

StrategyRuns Response:

```json
{
  "status": "success",
  "data": [
    {
      "id": 42,
      "strategy_id": 7,
      "mode": "sandbox",
      "broker": "sandbox",
      "started_at": "2026-08-30T03:50:11.402118+00:00",
      "stopped_at": "2026-08-30T09:40:02.771905+00:00",
      "stop_reason": "eod",
      "pnl_realized": 3140.5,
      "pnl_peak": 4880.0,
      "pnl_trough": -1220.25,
      "trigger_source": "manual",
      "resolved_expiries": {"1": "04-SEP-26", "2": "04-SEP-26"}
    }
  ]
}
```

### StrategyOrders Example

Every order the engine placed, oldest first.

```python
response = client.strategyorders(strategy_id=7)

# Narrow to one run
response = client.strategyorders(strategy_id=7, run_id=42)
print(response)
```

StrategyOrders Response:

```json
{
  "status": "success",
  "data": [
    {
      "id": 318,
      "run_id": 42,
      "leg_id": 1,
      "kind": "entry",
      "position_ref": "969bc536b1c14d15992f730c2c136d7a",
      "broker_order_id": "26083004118201",
      "symbol": "NIFTY04SEP2624500CE",
      "exchange": "NFO",
      "action": "SELL",
      "qty": 75,
      "product": "NRML",
      "pricetype": "MARKET",
      "price": 0.0,
      "status": "complete",
      "placed_at": "2026-08-30T03:50:11.610224+00:00",
      "filled_at": "2026-08-30T03:50:12.004881+00:00",
      "avg_fill_price": 142.35,
      "filled_qty": 75,
      "reject_reason": null
    }
  ]
}
```

A row is written before the broker answers, so an order can appear with
`status: "pending"` and a null `broker_order_id`.

### StrategyEvents Example

The risk-event audit trail, newest first.

```python
response = client.strategyevents(strategy_id=7, limit=100)

# Filters. An out-of-vocabulary kind or severity is a 400, not an empty list.
client.strategyevents(strategy_id=7, run_id=42)
client.strategyevents(strategy_id=7, severity="critical")
client.strategyevents(strategy_id=7, kind="run_stop_failed")
```

StrategyEvents Response:

```json
{
  "status": "success",
  "data": [
    {
      "id": 2041,
      "run_id": 42,
      "strategy_id": 7,
      "ts": "2026-08-30T06:21:40.104112+00:00",
      "kind": "leg_sl_hit",
      "severity": "warn",
      "leg_id": 1,
      "message": "stop loss hit: last price 172.8 is at or above the stop 172.35 on a short position",
      "payload": null
    }
  ]
}
```

### Strategy Webhook Example

The webhook is what TradingView and other alert senders post to. It is not
under `/api/v1` and takes no API key: the `oaws_` token in the URL is the whole
credential. It is shown once, in the app, when the strategy is created or its
token is rotated. Treat it as a password.

For TradingView or another service on the internet to reach the webhook,
OpenAlgo Desktop must be reachable from the internet. A script on the same
computer can use `http://127.0.0.1:5000`. Like the API, the webhook refuses
alerts (HTTP 503) until you sign in to OpenAlgo Desktop after it starts.

```python
from openalgo import Strategy

strategy = Strategy(
    host_url="http://127.0.0.1:5000",
    webhook_token="oaws_your_webhook_token_here"
)

# Batch strategy: mode is required on start and never defaulted
print(strategy.start("sandbox"))
print(strategy.stop())
```

Webhook Start Response:

```json
{
  "status": "success",
  "result": "ok",
  "message": "Strategy start accepted",
  "strategy_id": 7,
  "run_id": 42
}
```

```python
# Signal strategy: one alert moves one leg. Name the leg by id, or by symbol
# and exchange. leg_id wins when both are given.
strategy.long_entry(leg_id=1)
strategy.long_exit(leg_id=1)
strategy.short_entry(symbol="RELIANCE", exchange="NSE")
strategy.short_exit(symbol="RELIANCE", exchange="NSE")
```

Every documented outcome is returned, not raised; read the `result` label:

```python
response = strategy.start("sandbox")
result = response.get("result")

if result == "ok":
    print(f"accepted, run {response['run_id']}")
elif result == "rejected_dedupe":
    print("duplicate delivery within 60s, already handled")   # HTTP 200
elif result == "rejected_cooling_off":
    print("stopped within the last 30s, try again shortly")   # HTTP 409
elif result == "rejected_live_disabled":
    print("enable live trading on the strategy page first")   # HTTP 403
```

A signal that does nothing is a success with a note, not a failure:
`Signal accepted (already_long)`. The notes are `already_long`,
`already_short`, `no_matching_position`, `outside_entry_window` and
`outside_trading_window`. A signal blocked by the strategy's direction, or
naming a leg that does not exist, answers `rejected_invalid_action`.

The old `Strategy.strategyorder()` method raises `NotImplementedError` in the
SDK itself; it has nothing to do with the desktop.

## Streaming (WebSocket)

The SDK's streaming methods work against the desktop's feed on
`ws://127.0.0.1:8765`. The protocol is the web's; see
[WebSocket format](websockets-format.md) and
[WebSocket verbose control](websockets-verbose-control.md) for the
messages and the SDK's `verbose` levels. The SDK reconnects, signs in again and
replays subscriptions by itself after a drop (`auto_reconnect=True`, the
default).

Differences in the desktop, both covered in
[Moving from OpenAlgo web](../user/moving-from-openalgo-web.md):

- Quote and depth messages carry extra alias field names next to the web's, so
  a script written for any broker's field names works.
- A connection that sends thousands of messages a second is closed (code 1008).
  Subscribing a list of instruments in one call, as the SDK does, stays far
  inside that budget.

### LTP Data (Streaming Websocket)

```python
from openalgo import api
import time

client = api(
    api_key="<APIKEY>",
    host="http://127.0.0.1:5000",
    ws_url="ws://127.0.0.1:8765"
)

instruments = [
    {"exchange": "NSE", "symbol": "RELIANCE"},
    {"exchange": "NSE", "symbol": "INFY"}
]

def on_ltp(data):
    print("LTP Update Received:")
    print(data)

client.connect()
client.subscribe_ltp(instruments, on_data_received=on_ltp)

try:
    time.sleep(10)
finally:
    client.unsubscribe_ltp(instruments)
    client.disconnect()
```

### Quotes (Streaming Websocket)

Same pattern with `subscribe_quote` / `unsubscribe_quote`:

```python
def on_quote(data):
    print("Quote Update Received:")
    print(data)

client.connect()
client.subscribe_quote(instruments, on_data_received=on_quote)

try:
    time.sleep(10)
finally:
    client.unsubscribe_quote(instruments)
    client.disconnect()
```

### Depth (Streaming Websocket)

Same pattern with `subscribe_depth` / `unsubscribe_depth`:

```python
def on_depth(data):
    print("Market Depth Update Received:")
    print(data)

client.connect()
client.subscribe_depth(instruments, on_data_received=on_depth)

try:
    time.sleep(10)
finally:
    client.unsubscribe_depth(instruments)
    client.disconnect()
```

### Order updates (Streaming Websocket)

An account-level stream of fills, partial fills, rejections and
cancellations, from your broker or, in analyzer mode, from the sandbox:

```python
def on_order(update):
    print(update["orderid"], update["order_status"])

client.connect()
client.subscribe_orders(on_order_update=on_order)

try:
    time.sleep(60)
finally:
    client.unsubscribe_orders()
    client.disconnect()
```

The latest values are also kept by the client: `client.get_ltp()`,
`client.get_quotes()`, `client.get_depth()` and `client.get_orders()`.

## Indicators (`openalgo.ta`)

```python
from openalgo import ta
```

`ta` computes indicator values (RSI, EMA, Supertrend and the rest) inside the
SDK on your computer. It never calls OpenAlgo, so it works the same with the
desktop as with the web. Its reference is the OpenAlgo web documentation at
https://docs.openalgo.in.
