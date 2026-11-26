# Funds

Get your account's funds: available cash, collateral, realized and unrealized mark-to-market, and margin in use.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/funds
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/funds \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>"
}'
```

## Sample API Response

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

## Sample API Response (Analyzer Mode)

```json
{
  "data": {
    "availablecash": 9666628.79,
    "collateral": 0.0,
    "grossexposure": 333371.21,
    "last_reset": "2026-10-03 09:30:17",
    "m2mrealized": 0.0,
    "m2munrealized": 0.0,
    "reset_count": 0,
    "today_charges": 0.0,
    "today_realized_pnl": 0.0,
    "total_charges": 0.0,
    "total_realized_pnl": 0.0,
    "totalpnl": 0.0,
    "utiliseddebits": 333371.21
  },
  "mode": "analyze",
  "status": "success"
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
| data | object | The funds |
| mode | string | `analyze` in analyzer mode; absent in live mode |

### Data Object Fields

| Field | Type | Description |
|-------|------|-------------|
| availablecash | string (live), number (analyzer) | Cash free for new orders |
| collateral | string (live), number (analyzer) | Margin from pledged holdings |
| m2mrealized | string (live), number (analyzer) | Realized profit or loss today |
| m2munrealized | string (live), number (analyzer) | Profit or loss on open positions |
| utiliseddebits | string (live), number (analyzer) | Margin in use |
| grossexposure | number | Analyzer mode only: margin in use |
| totalpnl | number | Analyzer mode only: realized plus unrealized P&L |
| today_realized_pnl | number | Analyzer mode only: realized P&L since the 03:00 IST reset |
| total_realized_pnl | number | Analyzer mode only: realized P&L since the last sandbox reset |
| last_reset | string | Analyzer mode only: time of the last sandbox reset, `YYYY-MM-DD HH:MM:SS` |
| reset_count | number | Analyzer mode only: how many times the sandbox was reset |
| today_charges | number | Analyzer mode only: brokerage and statutory charges paid since the 03:00 IST reset |
| total_charges | number | Analyzer mode only: brokerage and statutory charges paid since the last sandbox reset |

## Analyzer Mode

The sandbox answers with its own account: 1 crore starting capital (set on the Sandbox page), every value a JSON number, plus the analyzer-only fields above. Margin is blocked when an order is placed and released when it is cancelled or its position is closed. Each fill's brokerage and statutory charges come out of `availablecash` and are added to `today_charges` and `total_charges`; the P&L fields and the margin in use do not move with them. See [Sandbox mode](../../user/sandbox.md).

## Errors

Validation errors on this endpoint carry `message` as an object:

```json
{
  "status": "error",
  "message": {"apikey": ["Missing data for required field."]}
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 500 | The broker's reason, or `Could not reach the broker. Check your internet connection and try again.` | The broker did not return your funds |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- In live mode every value is a string with two decimals, as on the web.
- Total margin is `availablecash` plus `collateral`.
- Rate limit: 100 requests per second per caller. See [Rate limiting](../rate-limiting.md).

## In the desktop

- In the desktop, the sandbox keeps the margin that resting orders hold and the full margin of a fill that reverses a position. The web's sandbox drops both after a fill, so for the same activity the desktop can show more `utiliseddebits` and less `availablecash` than the web: it matches the sandbox's own books.
- In the desktop, the sandbox checks margin again when an order fills. An exit that fills after its position was already closed opens a new position and blocks that position's margin then, or is rejected for insufficient funds. The web opened such a position with no margin.
- In the desktop, sandbox margin on CRYPTO includes the contract value (10 BTCUSD.P contracts of 0.001 BTC at 60,000 at 10x hold 60, not 60,000).

---

**Back to**: [API Documentation](../README.md)
