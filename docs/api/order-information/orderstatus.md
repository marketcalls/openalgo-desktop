# OrderStatus

Get the current state of one order by its order id.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/orderstatus
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "orderid": "250828000185002",
  "strategy": "Test Strategy"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/orderstatus \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "orderid": "250828000185002",
  "strategy": "Test Strategy"
}'
```

## Sample API Response

```json
{
  "status": "success",
  "data": {
    "action": "BUY",
    "average_price": 18.95,
    "exchange": "NSE",
    "order_status": "complete",
    "orderid": "250828000185002",
    "price": 0.0,
    "pricetype": "MARKET",
    "product": "MIS",
    "quantity": 1,
    "symbol": "YESBANK",
    "timestamp": "2025-08-28 09:59:10",
    "trigger_price": 0.0
  }
}
```

## Sample API Response (Analyzer Mode)

```json
{
  "data": {
    "action": "BUY",
    "average_price": 1167.7,
    "exchange": "NSE",
    "filled_quantity": 5,
    "order_status": "complete",
    "orderid": "26100376733573",
    "pending_quantity": 0,
    "price": 1167.7,
    "price_type": "MARKET",
    "product": "MIS",
    "quantity": 5,
    "strategy": "fixtures",
    "symbol": "RELIANCE",
    "timestamp": "2026-10-03 09:42:28",
    "trigger_price": 0.0
  },
  "mode": "analyze",
  "status": "success"
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| strategy | Strategy name | Mandatory | - |
| orderid | The order id to look up | Mandatory | - |

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| data | object | The order |
| mode | string | `analyze` in analyzer mode; absent in live mode |

### Data Object Fields

| Field | Type | Description |
|-------|------|-------------|
| orderid | string | Order id |
| symbol | string | OpenAlgo symbol |
| exchange | string | Exchange code |
| action | string | BUY or SELL |
| quantity | number | Order quantity |
| price | number | Limit price; 0 for a live MARKET order |
| trigger_price | number | Trigger price |
| pricetype | string | Live mode: MARKET, LIMIT, SL or SL-M |
| price_type | string | Analyzer mode: the same, under this key |
| product | string | MIS, CNC or NRML |
| order_status | string | `open`, `trigger pending`, `complete`, `cancelled` or `rejected` |
| average_price | number | Fill price. Live: from the trade book, for a `complete` order; 0 otherwise |
| timestamp | string | Order time; `YYYY-MM-DD HH:MM:SS` IST in analyzer mode |
| filled_quantity, pending_quantity, strategy | | Analyzer mode only |

## Analyzer Mode

The sandbox answers for its own orders with `"mode": "analyze"`. The price-type key is `price_type` here, while the order book uses `pricetype`, as on the web. A cancelled order keeps its `pending_quantity`.

## Errors

Validation errors on this endpoint carry `message` as a string that spells out a Python dictionary, with `"mode": "analyze"` added in analyzer mode:

```json
{
  "message": "{'orderid': ['Missing data for required field.']}",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 404 | `Order 99999999999999 not found` | No order with that id today (in analyzer mode, with `"mode": "analyze"`) |
| 500 | The broker's reason, or `Could not reach the broker. Check your internet connection and try again.` | The broker did not return its order book |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- `strategy` is required, as in the web's code; the web docs list it as optional.
- In live mode the order is found in your broker's order book for the day, so the row has the same keys as an [order book](../account-services/orderbook.md) row plus `average_price`.
- In the desktop, an order filled in parts reports the average price of all its fills when the trade book lists each fill with its own quantity and the fills add up to what the order filled (Flattrade, for one). The web reports the first fill it finds, so for a SELL 120 filled 60 at 316.05 and 60 at 315.99 it answers 316.05 or 315.99, where the desktop answers 316.02 (a web defect). A trade book that repeats the order's quantity on every fill (Shoonya, Zebu, TradeSmart) is read as on the web.
- Rate limit: 100 requests per second per caller. See [Rate limiting](../rate-limiting.md).

---

**Back to**: [API Documentation](../README.md)
