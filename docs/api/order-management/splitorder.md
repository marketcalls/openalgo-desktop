# SplitOrder

Split one large order into several smaller orders of a fixed size, for example to stay under an exchange freeze quantity or to reduce market impact.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/splitorder
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "strategy": "Python",
  "symbol": "YESBANK",
  "exchange": "NSE",
  "action": "SELL",
  "quantity": "105",
  "splitsize": "20",
  "pricetype": "MARKET",
  "product": "MIS"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/splitorder \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy": "Python",
  "symbol": "YESBANK",
  "exchange": "NSE",
  "action": "SELL",
  "quantity": "105",
  "splitsize": "20",
  "pricetype": "MARKET",
  "product": "MIS"
}'
```

## Sample API Response

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

## Sample API Response (Analyzer Mode)

SBIN, 10 shares in orders of 3:

```json
{
  "mode": "analyze",
  "results": [
    {"order_num": 1, "orderid": "26100319045249", "quantity": 3, "status": "success"},
    {"order_num": 2, "orderid": "26100322216224", "quantity": 3, "status": "success"},
    {"order_num": 3, "orderid": "26100323761475", "quantity": 3, "status": "success"},
    {"order_num": 4, "orderid": "26100325112150", "quantity": 1, "status": "success"}
  ],
  "split_size": 3,
  "status": "success",
  "total_quantity": 10
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key (1 to 256 characters) | Mandatory | - |
| strategy | Strategy name | Mandatory | - |
| symbol | OpenAlgo symbol | Mandatory | - |
| exchange | NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX or CRYPTO | Mandatory | - |
| action | BUY or SELL (lowercase is accepted) | Mandatory | - |
| quantity | Total quantity to split, a positive whole number | Mandatory | - |
| splitsize | Size of each order, a positive whole number | Mandatory | - |
| pricetype | MARKET, LIMIT, SL or SL-M | Optional | MARKET |
| product | MIS, CNC or NRML | Optional | MIS |
| price | Limit price, for LIMIT and SL orders | Optional | 0 |
| trigger_price | Trigger price, for SL and SL-M orders | Optional | 0 |
| disclosed_quantity | Disclosed quantity, a non-negative whole number | Optional | 0 |

Numbers may be sent as JSON numbers or as numeric strings. Any other field is refused as `Unknown field.`

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` whenever the split was processed, even if every order failed |
| total_quantity | number | The total quantity asked for |
| split_size | number | The size of each order |
| results | array | One entry per order |
| mode | string | `analyze` when analyzer mode is on; absent in live mode |

### Results Array Fields

| Field | Type | Description |
|-------|------|-------------|
| order_num | number | The order's place in the sequence (1, 2, 3, ...) |
| quantity | number | This order's quantity |
| status | string | `success` or `error` for this order |
| orderid | string | The order id, on success |
| message | string | Why this order failed, on error |

## How Split Orders Work

For a total of 105 in orders of 20:

```
Order 1: 20
Order 2: 20
Order 3: 20
Order 4: 20
Order 5: 20
Order 6:  5 (the remainder)
-----------
Total:  105
```

## Analyzer Mode

With analyzer mode on, every order goes to the sandbox. OpenAlgo fetches one quote for the symbol and places the orders one after another; each fills or rests as a [PlaceOrder](./placeorder.md) order would. See [Sandbox mode](../../user/sandbox.md).

## Errors

The split answers HTTP 200 with `"status": "success"` whenever it was processed; check each order's `status`. An order can fail for any reason [PlaceOrder](./placeorder.md#errors) gives.

Validation errors carry `message` as a string that spells out a Python dictionary. In analyzer mode the body also carries `"mode": "analyze"`:

```json
{
  "message": "{'splitsize': ['Split size must be a positive integer.']}",
  "mode": "analyze",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `{'splitsize': ['Missing data for required field.']}` | A required field is missing |
| 400 | `{'splitsize': ['Split size must be a positive integer.']}` | `splitsize` is 0 or negative |
| 400 | `{'quantity': ['Total quantity must be a positive number.']}` | `quantity` is 0 or negative |
| 400 | `{'quantity': ['Quantity is larger than any exchange accepts.']}` | `quantity` above 2147483647 |
| 400 | `Total number of orders would exceed maximum limit of 100` | `quantity` / `splitsize` needs more than 100 orders (in analyzer mode with `"mode": "analyze"`) |
| 400 | `Fractional quantities are not supported in OpenAlgo Desktop yet. Use a whole-number quantity.` | A fractional CRYPTO quantity |

Order-level messages in `results` include:

| `message` | When |
|-----------|------|
| The broker's own reason | The broker refused that order |
| `OpenAlgo is pacing requests to stay within your broker's rate limit, and this one would have had to wait too long for its turn. Try again in a few seconds.` | That order would have waited more than 10 seconds for its turn at the broker; it was not sent |
| `The connection to the broker dropped after the order was sent, so it may have been placed. Check the order book before placing it again.` | That order was sent and the broker's answer was lost (a dropped connection or a timeout after sending) |
| `Failed to place order due to internal error` | That order was sent and the broker's answer could not be read |
| `Not sent: an earlier order of this split has no definite answer from the broker and may have been placed. Check the order book before sending the rest.` | An earlier order of the split has no definite answer, so this one was held back |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- At most 100 orders per split.
- Read every order's result, even when the request answers 200. Sending the whole split again because one order had no definite answer can place the others twice. See [Order outcomes](../README.md#order-outcomes-and-retries).
- The last order carries the remainder (`quantity` modulo `splitsize`). When `splitsize` is larger than `quantity`, one order is placed.
- In live mode the orders go one after another, 100 ms apart, each taking its turn in the broker pacer.
- All orders share the same price type, price and product.
- Freeze quantities change; check the current limit for the contract rather than hard-coding one.
- When the API key's order mode is Semi-Auto, the whole split waits in the Action Center for your approval and the answer is `{"status": "success", "message": "Order queued for approval in Action Center", "mode": "semi_auto", "pending_order_id": 12}`. This applies in analyzer mode too.
- Rate limit: 100 requests per second per caller, the general API limit. See [Rate limiting](../rate-limiting.md).

## In the desktop

- In the desktop, when one order of a live split is sent and no definite answer comes back, the rest are not sent; each is reported with the `Not sent: ...` message so you can check the order book first. The web keeps sending the rest.
- In the desktop, a fractional CRYPTO quantity is refused; the web truncates it to a whole number.
- In the desktop, a total quantity above 2147483647 is refused before anything is sent; the web passed it on.

---

**Back to**: [API Documentation](../README.md)
