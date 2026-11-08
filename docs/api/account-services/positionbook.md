# PositionBook

Get your positions for the day, with average price, last price and P&L.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/positionbook
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/positionbook \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>"
}'
```

## Sample API Response

```json
{
  "status": "success",
  "data": [
    {
      "symbol": "NHPC",
      "exchange": "NSE",
      "product": "MIS",
      "quantity": -1,
      "average_price": "83.74",
      "ltp": 83.72,
      "pnl": 0.02
    },
    {
      "symbol": "RELIANCE",
      "exchange": "NSE",
      "product": "MIS",
      "quantity": 0,
      "average_price": "0.00",
      "ltp": 1189.9,
      "pnl": 5.9
    }
  ]
}
```

## Sample API Response (Analyzer Mode)

```json
{
  "data": [
    {
      "average_price": 1167.7,
      "exchange": "NSE",
      "lot_size": 1.0,
      "ltp": 1167.7,
      "pnl": 0.0,
      "pnlpercent": 0.0,
      "product": "MIS",
      "quantity": 5,
      "symbol": "RELIANCE",
      "today_realized_pnl": 0.0,
      "total_pnl_today": 0.0,
      "unrealized_pnl": 0.0
    }
  ],
  "mode": "analyze",
  "status": "success",
  "total_pnl": 0.0,
  "total_pnl_today": 0.0,
  "total_today_realized_pnl": 0.0,
  "total_unrealized_pnl": 0.0
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| data | array | Positions |
| mode | string | `analyze` in analyzer mode; absent in live mode |
| total_pnl, total_pnl_today, total_today_realized_pnl, total_unrealized_pnl | number | Analyzer mode only: totals over the rows |

### Position Object Fields

| Field | Type | Description |
|-------|------|-------------|
| symbol | string | OpenAlgo symbol |
| exchange | string | Exchange code |
| product | string | MIS, CNC or NRML |
| quantity | number | Net quantity: positive long, negative short, 0 closed today (fractional only on CRYPTO) |
| average_price | string (live), number (analyzer) | Average entry price; live, a string with two decimals |
| ltp | number | Last traded price, two decimals |
| pnl | number | Profit or loss, two decimals |
| average_price_basis | string | Present only when `average_price` and `pnl` are not measured from what the position cost. See [When the Entry Price Is Not Available](#when-the-entry-price-is-not-available) |
| lot_size | number | Analyzer mode, and CRYPTO rows on Delta Exchange: the contract size (0.001 for BTCUSD.P) |
| pnlpercent, unrealized_pnl, today_realized_pnl, total_pnl_today | number | Analyzer mode only |

## Analyzer Mode

The sandbox lists its open positions with the extra fields and totals above. A position closed in the sandbox drops out of the list, so a flat book is an empty list. See [Sandbox mode](../../user/sandbox.md).

## Errors

Validation errors on this endpoint carry `message` as an object, for example `{"apikey": ["Missing data for required field."]}`.

| HTTP | `message` | When |
|------|-----------|------|
| 500 | The broker's reason, or `Could not reach the broker. Check your internet connection and try again.` | The broker did not return the positions |
| 501 | `... is not available for your broker.` | Your broker offers no positions call |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- In live mode the book lists what your broker reports for the day, closed positions (quantity 0) included.
- Every row carries the OpenAlgo symbol, so `/placesmartorder`, `/closeposition` and `/openposition` match it.
- Rate limit: 100 requests per second per caller. See [Rate limiting](../rate-limiting.md).

## When the Entry Price Is Not Available

`average_price` is the price the position was entered at, and `pnl` is measured from it. A row whose numbers are measured from something else says so in `average_price_basis`; the key is absent on every row whose average is an entry price, as on the web.

| Value | Meaning |
|-------|---------|
| `carry_forward_valuation` | The position was carried over from a previous day, and the broker reports the overnight leg re-valued at the previous day's settlement price rather than at what it cost. `average_price` is that settlement price and `pnl` is the move since it, the day's profit and loss, not the position's. |

Only Kotak Neo sets it, when it cannot recover the cost of a carried position: OpenAlgo uses Kotak's uploaded price (`upldPrc`) first, then the matching Holdings `averagePrice` for a carried NSE or BSE cash position, and marks the row only when neither is there. A carried F&O position has no cost in Kotak's API, so it is always marked.

## In the desktop

- In the desktop, `quantity`, `ltp` and `pnl` are JSON numbers for every broker, and `average_price` a two-decimal string. Some of the web's broker adapters return them all as strings. A client that parses them as numbers works with both.

---

**Back to**: [API Documentation](../README.md)
