# ModifyOrder

Change an order that is still working: its price, quantity or trigger price.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/modifyorder
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "orderid": "250408001002736",
  "strategy": "Python",
  "symbol": "YESBANK",
  "action": "BUY",
  "exchange": "NSE",
  "pricetype": "LIMIT",
  "product": "CNC",
  "quantity": 1,
  "price": 16.5,
  "trigger_price": 0,
  "disclosed_quantity": 0
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/modifyorder \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "orderid": "250408001002736",
  "strategy": "Python",
  "symbol": "YESBANK",
  "action": "BUY",
  "exchange": "NSE",
  "pricetype": "LIMIT",
  "product": "CNC",
  "quantity": 1,
  "price": 16.5,
  "trigger_price": 0,
  "disclosed_quantity": 0
}'
```

## Sample API Response

```json
{
  "orderid": "250408001002736",
  "status": "success"
}
```

## Sample API Response (Analyzer Mode)

Changing a resting RELIANCE LIMIT order to 2 shares at 957.5:

```json
{
  "message": "Order modified successfully",
  "mode": "analyze",
  "orderid": "26100341808353",
  "status": "success"
}
```

## Request Body

Every field is required, as on the web, even when the change does not use it.

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key (1 to 256 characters) | Mandatory | - |
| orderid | The id of the order to change | Mandatory | - |
| strategy | Strategy name | Mandatory | - |
| symbol | OpenAlgo symbol of the order | Mandatory | - |
| action | BUY or SELL (lowercase is accepted) | Mandatory | - |
| exchange | NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX or CRYPTO | Mandatory | - |
| pricetype | MARKET, LIMIT, SL or SL-M | Mandatory | - |
| product | MIS, CNC or NRML | Mandatory | - |
| quantity | New quantity, a positive number. Whole numbers only, except on CRYPTO | Mandatory | - |
| price | New price, 0 or more | Mandatory | - |
| trigger_price | New trigger price, 0 or more; send 0 when unused | Mandatory | - |
| disclosed_quantity | New disclosed quantity, a non-negative whole number; send 0 when unused | Mandatory | - |

Numbers may be sent as JSON numbers or as numeric strings. Any other field is refused as `Unknown field.`

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| orderid | string | The order that was changed |
| message | string | `Order modified successfully` in analyzer mode, or the reason for an error |
| mode | string | `analyze` when analyzer mode is on; absent in live mode |

## What Can Be Changed

| Parameter | Changeable | Notes |
|-----------|------------|-------|
| Quantity | Yes | A lot multiple for F&O |
| Price | Yes | LIMIT and SL orders |
| Trigger price | Yes | SL and SL-M orders |
| Price type | Depends on the broker | The sandbox keeps the order's price type |
| Product | No | Cancel and place a new order |
| Symbol | No | Cancel and place a new order |
| Action | No | Cancel and place a new order |

## Analyzer Mode

With analyzer mode on, the sandbox changes its own order and the answer carries `"mode": "analyze"`.

- Only an `open` or `trigger pending` order can be changed.
- The sandbox changes the quantity, price and trigger price. It keeps the order's price type, product and side.
- A price on a MARKET or SL-M order, or a trigger price on a MARKET or LIMIT order, is refused. A price or trigger price of 0 means "leave it as it is".
- If the change makes the order bigger, the extra margin is checked when the order fills; an order the funds cannot carry then is rejected with the insufficient-funds reason.

See [Sandbox mode](../../user/sandbox.md).

## Errors

Validation errors carry `message` as a string that spells out a Python dictionary. In analyzer mode the body also carries `"mode": "analyze"`:

```json
{
  "message": "{'price': ['Missing data for required field.']}",
  "mode": "analyze",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `{'price': ['Missing data for required field.']}` | A required field is missing |
| 400 | `{'price': ['Price must be a non-negative number.']}` | Negative price (likewise `trigger_price`, `disclosed_quantity`) |
| 400 | `{'quantity': ['Quantity must be a positive number.']}` | Quantity is zero or negative |
| 400 | `{'quantity': ['Quantity is larger than any exchange accepts.']}` | Quantity above 2147483647 |
| 400 | The broker's own reason | The broker refused the change |
| 429 | `OpenAlgo is pacing requests to stay within your broker's rate limit, and this one would have had to wait too long for its turn. Try again in a few seconds.` | The change would have waited more than 10 seconds for its turn at the broker; it was not sent |
| 403 | `Modify order operation is not allowed in Semi-Auto mode. Please switch to Auto mode to modify orders.` | The API key's order mode is Semi-Auto (live mode only) |
| 500 | `Failed to modify order due to internal error` | No usable answer from the broker |

In analyzer mode the sandbox adds its own refusals, with `"mode": "analyze"`:

| HTTP | `message` | When |
|------|-----------|------|
| 404 | `Order 99999999999999 not found` | No sandbox order has that id |
| 400 | `Cannot modify order in complete status` | The order has filled (likewise `cancelled`, `rejected`) |
| 400 | `Quantity must be in multiples of lot size 65` | F&O quantity not a lot multiple |
| 400 | `MARKET orders do not accept a price` | A price on an order type that has none |
| 400 | `LIMIT orders do not accept a trigger_price` | A trigger price on an order type that has none |
| 400 | `Fractional quantities are not supported in OpenAlgo Desktop yet. Use a whole-number quantity.` | A fractional CRYPTO quantity |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- Only working orders (open or trigger pending) can be changed. Filled, cancelled and rejected orders cannot.
- To change the side, product or symbol, cancel the order and place a new one.
- Modify is never queued in the Action Center. In Semi-Auto it is refused in live mode and allowed in analyzer mode, as on the web.
- Rate limit: 10 requests per second per caller, shared with the other order endpoints. See [Rate limiting](../rate-limiting.md).

## In the desktop

- In the desktop, a fractional CRYPTO quantity is accepted only for a live order; analyzer mode refuses it, because the sandbox counts whole units.
- In the desktop, a quantity above 2147483647 is refused before anything is sent; the web passed it on.
- A change the broker pacer cannot send within 10 seconds is answered 429, as on the web; the desktop always bounds the wait, the web only on a multi-threaded server.
- In the desktop, if the order mode (Auto or Semi-Auto) cannot be read, a live change is refused with 500 (`OpenAlgo could not read whether your orders wait for approval in the Action Center (Semi-Auto), so nothing was sent. ...`); the web treats an unreadable mode as Auto.

---

**Back to**: [API Documentation](../README.md)
