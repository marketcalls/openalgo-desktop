# GTTOrderBook

List your GTTs. By default only active GTTs are returned, the ones that can still fire. Send `"status": "all"` to include the history as well (triggered, cancelled, expired, rejected), so you can see what fired and, in analyzer mode, the order it placed.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/gttorderbook
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>"
}
```

## Sample API Request (With History)

```json
{
  "apikey": "<APIKEY>",
  "status": "all"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/gttorderbook \
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

## Sample API Response (Analyzer Mode, `"status": "all"`)

```json
{
  "data": [
    {
      "created_at": "2026-10-03T09:44:03.824776",
      "exchange": "NSE",
      "expires_at": "2027-10-03T09:44:03.823976",
      "last_price": 954.1,
      "legs": [
        {
          "action": "SELL",
          "price": 849.1,
          "pricetype": "LIMIT",
          "product": "CNC",
          "quantity": 1,
          "triggered_order_id": null
        },
        {
          "action": "SELL",
          "price": 1106.8,
          "pricetype": "LIMIT",
          "product": "CNC",
          "quantity": 1,
          "triggered_order_id": null
        }
      ],
      "margin_blocked": 1106.8,
      "status": "active",
      "strategy": "fixtures",
      "symbol": "SBIN",
      "trigger_id": "GTT-261003-56ad1482",
      "trigger_prices": [
        858.7,
        1097.2
      ],
      "trigger_type": "two-leg",
      "updated_at": "2026-10-03T09:44:20.945106"
    },
    {
      "created_at": "2026-10-03T09:44:03.485996",
      "exchange": "NSE",
      "expires_at": "2027-10-03T09:44:03.484560",
      "last_price": 954.1,
      "legs": [
        {
          "action": "BUY",
          "price": 839.6,
          "pricetype": "LIMIT",
          "product": "CNC",
          "quantity": 2,
          "triggered_order_id": null
        }
      ],
      "margin_blocked": 0.0,
      "status": "cancelled",
      "strategy": "fixtures",
      "symbol": "SBIN",
      "trigger_id": "GTT-261003-12a093ec",
      "trigger_prices": [
        839.6
      ],
      "trigger_type": "single",
      "updated_at": "2026-10-03T09:44:22.041428"
    }
  ],
  "mode": "analyze",
  "status": "success"
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| status | `active`: GTTs that can still fire. `all`: also triggered, cancelled, expired and rejected ones. Anything else is a 400 | Optional | active |

Any other field is refused as `Unknown field.`

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| data | array | The GTTs (see below), active ones first; in analyzer mode each group newest first, in live mode in the broker's order |
| mode | string | `analyze` in analyzer mode; absent in live mode |
| message | string or object | The reason, on an error |

### GTT Entry

| Field | Type | Description |
|-------|------|-------------|
| trigger_id | string | The GTT's id |
| trigger_type | string | `single` (one trigger) or `two-leg` (OCO) |
| status | string | `active`; with `"status": "all"` also `triggered`, `cancelled`, `expired`, `rejected`, and broker values such as `disabled` or `deleted` passed through as they are |
| symbol | string | OpenAlgo symbol |
| exchange | string | Exchange |
| trigger_prices | array of numbers | SINGLE: `[trigger]`. OCO: `[stoploss_trigger, target_trigger]` |
| last_price | number | The last price recorded when the GTT was placed; `0` for brokers that do not give it |
| legs | array | One entry per leg, in the order of `trigger_prices` (see below) |
| created_at | string | When the GTT was created, as the broker gives it; in analyzer mode an IST date-time with microseconds (`2026-10-03T09:44:03.824776`) |
| updated_at | string | When it last changed; empty where the broker does not say |
| expires_at | string | When it lapses; empty where the broker does not say. In analyzer mode one year after placement unless `expires_at` was sent |
| strategy | string | Analyzer mode only: the strategy sent with the GTT |
| margin_blocked | number | Analyzer mode only: the margin the GTT holds while active; `0` once it fired, was cancelled or lapsed |

### Leg Object

| Field | Type | Description |
|-------|------|-------------|
| action | string | `BUY` or `SELL` |
| quantity | integer | Quantity |
| price | number | The leg's limit price (`0` for a MARKET leg) |
| pricetype | string | `LIMIT` or `MARKET` |
| product | string | `CNC` or `NRML` |
| triggered_order_id | string or null | Analyzer mode only: the sandbox order this leg placed when it fired; `null` until then. Brokers do not give this link, so live entries have no such field |

## Analyzer Mode

With analyzer mode on, the sandbox's GTTs are listed and the answer carries `"mode": "analyze"`. Each entry also has `strategy` and `margin_blocked`, and each leg `triggered_order_id`. A GTT that fired shows `"status": "triggered"`, and the order it placed is in the [order book](../account-services/orderbook.md) under the leg's `triggered_order_id`. See [Sandbox mode](../../user/sandbox.md).

## Errors

Validation errors on this endpoint carry `message` as an object of field errors, with no `mode` key:

```json
{
  "message": {
    "status": [
      "Must be one of: active, all."
    ]
  },
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `{"status": ["Must be one of: active, all."]}` | `status` is anything else, `cancelled` included |
| 501 | `GTT orders are not supported for broker 'shoonya' yet` | Your broker has no GTT in OpenAlgo (live mode) |
| 500 | `Failed to fetch GTT orderbook` | The broker call failed without a readable answer |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- Active only by default: without `status`, or with `"status": "active"`, no fired, cancelled, lapsed or rejected GTT appears.
- With `"status": "all"`, the sandbox, Zerodha and Rupeezy return the history. Upstox, Angel One, Fyers and Dhan return active GTTs only, as on the web.
- A fired GTT's order is an ordinary order: it appears in the [order book](../account-services/orderbook.md) in both modes.
- On Angel One an OCO is two independent rules: the book shows two SINGLE rows, not one OCO, and one firing does not cancel the other. See [Angel One](./placegttorder.md#angel-one-an-oco-is-two-separate-rules).
- A broker keeps its own history for a limited time, so `"status": "all"` can show less than a GTT's whole life.
- `last_price`, `created_at`, `updated_at` and `expires_at` depend on what the broker gives; they may be `0` or empty.
- Rate limit: 100 requests per second per caller, shared with the other read endpoints. See [Rate limiting](../rate-limiting.md).

---

**Back to**: [API Documentation](../README.md)
