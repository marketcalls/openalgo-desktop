# Depth (WebSocket)

Stream the order book as it changes: the quote fields plus bid and ask levels
with their quantities and order counts. Mode 3.

Connecting, authenticating, acknowledgements and errors are described in
[WebSocket Streaming](./README.md).

## WebSocket URL

```
ws://127.0.0.1:8765
```

## WebSocket Request

```json
{
  "action": "subscribe",
  "mode": "Depth",
  "depth": 5,
  "symbols": [
    {"exchange": "NSE", "symbol": "RELIANCE"},
    {"exchange": "NSE", "symbol": "INFY"}
  ]
}
```

`"mode": 3` is the same. `depth` is 5, 20, 30 or 50 and defaults to 5; the old
name `depth_level` is also read.

## Sample Response

Illustrative values:

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
    "ltp": 1187.75,
    "ltt": 1712572800000,
    "timestamp": 1712572800000,
    "volume": 14414545,
    "last_quantity": 100,
    "average_price": 1182.4,
    "total_buy_quantity": 591351,
    "total_sell_quantity": 835701,
    "open": 1172.0,
    "high": 1196.6,
    "low": 1163.3,
    "close": 1165.7,
    "oi": 0,
    "open_interest": 0,
    "depth": {
      "buy": [
        {"price": 1187.7, "quantity": 886, "orders": 4},
        {"price": 1187.65, "quantity": 212, "orders": 2},
        {"price": 1187.6, "quantity": 351, "orders": 3},
        {"price": 1187.55, "quantity": 343, "orders": 5},
        {"price": 1187.5, "quantity": 399, "orders": 2}
      ],
      "sell": [
        {"price": 1187.8, "quantity": 767, "orders": 3},
        {"price": 1187.85, "quantity": 115, "orders": 1},
        {"price": 1187.9, "quantity": 162, "orders": 2},
        {"price": 1187.95, "quantity": 1121, "orders": 6},
        {"price": 1188.0, "quantity": 430, "orders": 2}
      ]
    }
  },
  "broker": "zerodha"
}
```

## Levels Each Broker Streams

| Broker | Levels |
|--------|--------|
| Fyers | 5, and 50 on NSE and NFO |
| Dhan | 5, and 20 on NSE and NFO (not Dhan Sandbox) |
| Every other broker | 5 |

You receive the largest level your broker streams that is not more than you
asked for, else its smallest:

| Asked for | Fyers on NSE/NFO | Dhan on NSE/NFO | Others |
|-----------|------------------|-----------------|--------|
| 5 | 5 | 5 | 5 |
| 20 | 5 | 20 | 5 |
| 30 | 5 | 20 | 5 |
| 50 | 50 | 20 | 5 |

The `depth` in the subscribe acknowledgement follows OpenAlgo web's adapter
for your broker, so for some brokers it names the level asked for rather than
the level streamed (Zerodha acknowledges 20 and streams 5). Count the levels
in the frames. The full table is in
[WebSocket Streaming](./README.md#the-depth-in-the-acknowledgement).

## Unsubscribe from Depth

```json
{
  "action": "unsubscribe",
  "mode": "Depth",
  "symbols": [
    {"exchange": "NSE", "symbol": "RELIANCE"}
  ]
}
```

See [Unsubscribe](./README.md#unsubscribe).

## Python SDK Example

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

def on_depth(data):
    print("Market Depth Update Received:")
    print(data)

client.connect()
client.subscribe_depth(instruments, on_data_received=on_depth)

try:
    time.sleep(60)
finally:
    client.unsubscribe_depth(instruments)
    client.disconnect()
```

## Message Fields

### Subscribe/Unsubscribe Message

| Field | Type | Description |
|-------|------|-------------|
| action | string | `subscribe` or `unsubscribe` |
| mode | integer or string | `3` or `Depth` in any case |
| depth | integer | Levels wanted: 5, 20, 30 or 50; default 5 |
| symbols | array | Symbol objects. A single `symbol` and `exchange` at the top level also works. |
| request_id | any | Optional, echoed in the acknowledgement |

### Market Data Message

| Field | Type | Description |
|-------|------|-------------|
| type | string | Always `market_data` |
| symbol | string | Symbol |
| exchange | string | Exchange code |
| mode | integer | `3` |
| data | object | Depth data |
| broker | string | The broker that sent the book |

### Data Object

Every field of the [Quote data](./quote.md#data-object-equities-futures-and-options),
with `mode` set to `full`, and:

| Field | Type | Description |
|-------|------|-------------|
| oi, open_interest | number | Open interest under both names; 0 when the broker sends none |
| depth | object | The book: `buy` and `sell` arrays |

Your broker's own names for `last_quantity`, `average_price` and the change
fields are sent too, as listed on the [Quote](./quote.md#your-brokers-own-field-names)
page.

### Depth Object

| Field | Type | Description |
|-------|------|-------------|
| buy | array | Bid levels, best price first |
| sell | array | Ask levels, best price first |

### Buy/Sell Level Object

| Field | Type | Description |
|-------|------|-------------|
| price | number | Price level |
| quantity | number | Quantity at this level |
| orders | number | Number of orders at this level; 0 when the broker does not send it |

## Notes

- An index has no order book: in Depth mode its frames carry the index quote
  fields and no `depth`.
- With the market closed, the levels are `{"price": 0.0, "quantity": 0,
  "orders": 0}`.
- `data.mode` is `full` for every broker, as OpenAlgo web's Zerodha frames
  say. OpenAlgo web passes on its broker adapter's own label here, and some
  adapters send a number. Read the numeric top-level `mode` (3) to tell the
  modes apart.
- `ltt` is always epoch milliseconds. OpenAlgo web's Zerodha depth frames
  carried the exchange's last trade time in seconds.
- OpenAlgo web's documentation names the depth fields `ltq`, `totalbuyqty` and
  `totalsellqty`. The frames of both OpenAlgo web (with Zerodha) and the
  desktop use `last_quantity`, `total_buy_quantity` and `total_sell_quantity`.
- A Depth subscription also sends the symbol's Quote and LTP frames if the
  same connection holds those modes.

## Related

- [LTP WebSocket](./ltp.md) - smallest frames
- [Quote WebSocket](./quote.md) - OHLC and volume
- [Depth](../market-data/depth.md) - one five-level snapshot over REST

---

**Back to**: [API Documentation](../README.md)
