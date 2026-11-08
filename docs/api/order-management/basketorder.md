# BasketOrder

Place several orders in one request. Useful for portfolio rebalancing, pair trades and other multi-stock trades.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/basketorder
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "strategy": "Python",
  "orders": [
    {
      "symbol": "BHEL",
      "exchange": "NSE",
      "action": "BUY",
      "quantity": "1",
      "pricetype": "MARKET",
      "product": "MIS"
    },
    {
      "symbol": "ZOMATO",
      "exchange": "NSE",
      "action": "SELL",
      "quantity": "1",
      "pricetype": "MARKET",
      "product": "MIS"
    },
    {
      "symbol": "RELIANCE",
      "exchange": "NSE",
      "action": "BUY",
      "quantity": "1",
      "pricetype": "LIMIT",
      "product": "MIS",
      "price": "1180"
    }
  ]
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/basketorder \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy": "Python",
  "orders": [
    {"symbol": "BHEL", "exchange": "NSE", "action": "BUY", "quantity": "1", "pricetype": "MARKET", "product": "MIS"},
    {"symbol": "ZOMATO", "exchange": "NSE", "action": "SELL", "quantity": "1", "pricetype": "MARKET", "product": "MIS"},
    {"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": "1", "pricetype": "LIMIT", "product": "MIS", "price": "1180"}
  ]
}'
```

## Sample API Response

Results come back in the order the legs were placed: BUY legs first, then SELL legs.

```json
{
  "status": "success",
  "results": [
    {
      "exchange": "NSE",
      "orderid": "250408000999544",
      "product": "MIS",
      "status": "success",
      "symbol": "BHEL"
    },
    {
      "exchange": "NSE",
      "orderid": "250408000997546",
      "product": "MIS",
      "status": "success",
      "symbol": "RELIANCE"
    },
    {
      "exchange": "NSE",
      "orderid": "250408000997545",
      "product": "MIS",
      "status": "success",
      "symbol": "ZOMATO"
    }
  ]
}
```

## Sample API Response (Analyzer Mode, One Leg Refused)

```json
{
  "mode": "analyze",
  "results": [
    {
      "batch_order": true,
      "exchange": "NSE",
      "is_last_order": false,
      "orderid": "26100330880794",
      "product": "MIS",
      "status": "success",
      "symbol": "ITC"
    },
    {
      "message": "Symbol NOTASYMBOL not found on NSE",
      "status": "error",
      "symbol": "NOTASYMBOL"
    }
  ],
  "status": "success"
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key (1 to 256 characters) | Mandatory | - |
| strategy | Strategy name, applied to every leg | Mandatory | - |
| orders | List of legs, at least one | Mandatory | - |

### Leg Fields

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| symbol | OpenAlgo symbol | Mandatory | - |
| exchange | NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX or CRYPTO | Mandatory | - |
| action | BUY or SELL (lowercase is accepted) | Mandatory | - |
| quantity | Positive number. Whole numbers only, except on CRYPTO | Mandatory | - |
| pricetype | MARKET, LIMIT, SL or SL-M | Optional | MARKET |
| product | MIS, CNC or NRML | Optional | MIS |
| price | Limit price, for LIMIT and SL orders | Optional | 0 |
| trigger_price | Trigger price, for SL and SL-M orders | Optional | 0 |
| disclosed_quantity | Disclosed quantity, a non-negative whole number | Optional | 0 |

A leg takes no `apikey` or `strategy` of its own; any field not listed is refused as `Unknown field.`

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` whenever the basket was processed, even if every leg failed |
| results | array | One entry per leg, in placement order |
| mode | string | `analyze` when analyzer mode is on; absent in live mode |

### Results Array Fields

| Field | Type | Description |
|-------|------|-------------|
| symbol | string | The leg's symbol |
| status | string | `success` or `error` for this leg |
| orderid | string | The order id, on success |
| exchange | string | The leg's exchange, on success |
| product | string | The leg's product, on success |
| message | string | Why the leg failed, on error |
| batch_order | boolean | Analyzer mode only: always `true` on a placed leg |
| is_last_order | boolean | Analyzer mode only: `true` on the last leg of the basket |

## Analyzer Mode

With analyzer mode on, every leg goes to the sandbox, one after another, BUY legs first. OpenAlgo fetches quotes for all the legs in one call before placing them. Each leg fills or rests as a [PlaceOrder](./placeorder.md) order would, and a leg the sandbox refuses is reported in `results` with its reason. See [Sandbox mode](../../user/sandbox.md).

## Errors

The basket answers HTTP 200 with `"status": "success"` whenever it was processed; check each leg's `status`. A leg can fail for any reason [PlaceOrder](./placeorder.md#errors) gives.

Validation errors stop the whole basket. They carry `message` as a string that spells out a Python dictionary, with the leg's position (counted from 0) for a leg error. In analyzer mode the body also carries `"mode": "analyze"`:

```json
{
  "message": "{'orders': {0: {'product': ['Must be one of: MIS, NRML, CNC.']}}}",
  "mode": "analyze",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `{'orders': ['Orders must contain at least 1 item.']}` | `orders` is empty |
| 400 | `{'orders': {0: {'product': ['Must be one of: MIS, NRML, CNC.']}}}` | A leg failed validation |
| 400 | `{'orders': {1: {'quantity': ['Quantity is larger than any exchange accepts.']}}}` | A leg's quantity is above 2147483647 |

Leg-level messages in `results` include:

| `message` | When |
|-----------|------|
| The broker's own reason | The broker refused the leg |
| `OpenAlgo is pacing requests to stay within your broker's rate limit, and this one would have had to wait too long for its turn. Try again in a few seconds.` | The leg would have waited more than 10 seconds for its turn at the broker; it was not sent |
| `The connection to the broker dropped after the order was sent, so it may have been placed. Check the order book before placing it again.` | The leg was sent and the broker's answer was lost (a dropped connection or a timeout after sending) |
| `Failed to place order due to internal error` | The leg was sent and the broker's answer could not be read |
| `Fractional quantities are not supported in OpenAlgo Desktop yet. Use a whole-number quantity.` | A fractional CRYPTO quantity |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- BUY legs are placed before SELL legs, for the margin benefit; within each group the request order is kept.
- In live mode, legs go to the broker in batches of 10 at a time, with a one-second pause between batches. Each leg still takes its turn in the broker pacer.
- Each leg is independent: some can succeed while others fail.
- A leg sent without a definite answer is never sent again; check the order book before sending it yourself.
- Read every leg's result, even when the request answers 200. Sending the whole basket again because one leg had no definite answer can place the other legs twice. See [Order outcomes](../README.md#order-outcomes-and-retries).
- When the API key's order mode is Semi-Auto, the whole basket waits in the Action Center for your approval and the answer is `{"status": "success", "message": "Order queued for approval in Action Center", "mode": "semi_auto", "pending_order_id": 12}`. This applies in analyzer mode too.
- Rate limit: 100 requests per second per caller, the general API limit. See [Rate limiting](../rate-limiting.md).

## Example Use Cases

### Portfolio Rebalancing

```json
{
  "apikey": "<APIKEY>",
  "strategy": "Rebalance",
  "orders": [
    {"symbol": "TCS", "exchange": "NSE", "action": "BUY", "quantity": "5", "pricetype": "MARKET", "product": "CNC"},
    {"symbol": "INFY", "exchange": "NSE", "action": "BUY", "quantity": "10", "pricetype": "MARKET", "product": "CNC"},
    {"symbol": "WIPRO", "exchange": "NSE", "action": "SELL", "quantity": "8", "pricetype": "MARKET", "product": "CNC"}
  ]
}
```

### Pair Trading

```json
{
  "apikey": "<APIKEY>",
  "strategy": "PairTrade",
  "orders": [
    {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": "100", "pricetype": "MARKET", "product": "MIS"},
    {"symbol": "BANKBARODA", "exchange": "NSE", "action": "SELL", "quantity": "200", "pricetype": "MARKET", "product": "MIS"}
  ]
}
```

## In the desktop

- In the desktop, a leg with a fractional CRYPTO quantity is refused, live and in analyzer mode; basket legs carry whole units only.
- In the desktop, a leg's quantity above 2147483647 stops the basket at validation; the web passed it on.

---

**Back to**: [API Documentation](../README.md)
