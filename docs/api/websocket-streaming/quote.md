# Quote (WebSocket)

Stream quotes as they change: last price, the day's open, high and low,
previous close, volume and the order book totals. Mode 2.

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
  "mode": "Quote",
  "symbols": [
    {"exchange": "NSE", "symbol": "RELIANCE"},
    {"exchange": "NSE_INDEX", "symbol": "NIFTY"}
  ]
}
```

`"mode": 2` is the same, and a subscribe without `mode` is Quote.

## Sample Response

An equity (recorded with the market closed, so volume and quantities are 0):

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
    "volume": 0,
    "last_quantity": 0,
    "average_price": 0.0,
    "total_buy_quantity": 0,
    "total_sell_quantity": 0,
    "open": 1180.1,
    "high": 1183.9,
    "low": 1160.8,
    "close": 1187.0
  },
  "broker": "zerodha"
}
```

An index has no order book, so its quote is shorter and carries the change
instead:

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
    "price_change": 0,
    "price_change_percent": 0
  },
  "broker": "zerodha"
}
```

## Unsubscribe from Quotes

```json
{
  "action": "unsubscribe",
  "mode": "Quote",
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

def on_quote(data):
    print("Quote Update Received:")
    print(data)

client.connect()
client.subscribe_quote(instruments, on_data_received=on_quote)

try:
    time.sleep(60)
finally:
    client.unsubscribe_quote(instruments)
    client.disconnect()
```

## Message Fields

### Subscribe/Unsubscribe Message

| Field | Type | Description |
|-------|------|-------------|
| action | string | `subscribe` or `unsubscribe` |
| mode | integer or string | `2` or `Quote` in any case |
| symbols | array | Symbol objects. A single `symbol` and `exchange` at the top level also works. |
| request_id | any | Optional, echoed in the acknowledgement |

### Market Data Message

| Field | Type | Description |
|-------|------|-------------|
| type | string | Always `market_data` |
| symbol | string | Symbol |
| exchange | string | Exchange code |
| mode | integer | `2` |
| data | object | Quote data |
| broker | string | The broker that sent the quote |

### Data Object (equities, futures and options)

| Field | Type | Description |
|-------|------|-------------|
| symbol | string | Symbol |
| exchange | string | Exchange code |
| mode | string | `quote` |
| ltp | number | Last traded price |
| ltt | number | Last trade time, epoch milliseconds |
| timestamp | number | Update time, epoch milliseconds |
| volume | number | Volume traded today |
| last_quantity | number | Quantity of the last trade |
| average_price | number | Average traded price today |
| total_buy_quantity | number | Total buy quantity in the order book |
| total_sell_quantity | number | Total sell quantity in the order book |
| open | number | Day's open |
| high | number | Day's high |
| low | number | Day's low |
| close | number | Previous day's close |
| oi, open_interest | number | Open interest, the same value under both names. Present when the symbol is also streamed in Depth mode. |

A field your broker's feed does not carry is left out, never sent as 0, so
read it as possibly missing (`data.get("open")` in Python): Sahi's feed has no
open, high, low, previous close, last quantity, average price or order book
totals; Fyers leaves out whatever its feed did not send; Kotak's first ticks
before a full quote have only the price, the previous close and the last
trade. `ltt` is left out while there is no trade time (an index on Sahi or
Fyers, an instrument not yet traded on Kotak). On Dhan `close` is the
previous day's close and is left out until Dhan has sent it; Upstox and
Groww send the last value of each field their feed has sent and leave out
one it never sent (Groww has no last quantity, average price, totals or
open interest, and an index only its price).

### Data Object (indices)

| Field | Type | Description |
|-------|------|-------------|
| symbol, exchange, mode, ltp, ltt, timestamp, volume | | As above |
| open, high, low, close | number | Present when the broker sends them (left out when 0) |
| price_change | number | Change from the previous close |
| price_change_percent | number | Change in percent |

### Your broker's own field names

Quote data carries the names above for every broker. Where OpenAlgo web's
adapter for your broker uses another name for the same value, the desktop
sends that name too, with the same value, so a script written for either name
works:

| Field | Also sent as | Brokers |
|-------|--------------|---------|
| last_quantity | `last_trade_quantity` | Angel, Fyers, Upstox, 5paisa, mstock, Paytm, Pocketful, Samco |
| last_quantity | `ltq` | Definedge, Motilal |
| average_price | `avg_trade_price` | Fyers |
| price_change | `change` | Fyers, Arrow, HDFC Sky, HDFC Securities, Rupeezy, Tradejini, Samco |
| price_change | `change_absolute` | Paytm |
| price_change_percent | `change_percent` | Fyers, AliceBlue, Arrow, HDFC Sky, HDFC Securities, Definedge, Paytm, Rupeezy, Tradejini |
| price_change_percent | `percent_change` | Shoonya, Flattrade, Zebu, TradeSmart, Fyers, Kotak |
| price_change_percent | `change_percentage` | Samco |
| ltt | `last_trade_time` | Sahi, Kotak, Upstox; Fyers and Dhan in epoch seconds, as their web adapters send it. Beside `ltt` in every mode, LTP included |

Zerodha frames carry no extra names. A field that is left out takes its
other names with it. The change fields, and so their other
names, come with indices only.

## Notes

- `close` is the previous day's close, not today's last price.
- If the connection also holds the symbol in LTP mode, each update brings a
  mode-1 frame as well; if it holds Depth, a mode-3 frame as well.
- Use Quote for market overviews, scanners and charts.

## Related

- [LTP WebSocket](./ltp.md) - smallest frames
- [Depth WebSocket](./depth.md) - the order book
- [Quotes](../market-data/quotes.md) - one quote over REST

---

**Back to**: [API Documentation](../README.md)
