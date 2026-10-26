# PlaceOrder

Place one order with your broker, or with the sandbox when analyzer mode is on.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/placeorder
```

## Sample API Request (Market Order)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "Python",
  "symbol": "NHPC",
  "action": "BUY",
  "exchange": "NSE",
  "pricetype": "MARKET",
  "product": "MIS",
  "quantity": "1"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/placeorder \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy": "Python",
  "symbol": "NHPC",
  "action": "BUY",
  "exchange": "NSE",
  "pricetype": "MARKET",
  "product": "MIS",
  "quantity": "1"
}'
```

## Sample API Response

```json
{
  "orderid": "250408000989443",
  "status": "success"
}
```

## Sample API Request (Limit Order)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "Python",
  "symbol": "YESBANK",
  "action": "BUY",
  "exchange": "NSE",
  "pricetype": "LIMIT",
  "product": "MIS",
  "quantity": "1",
  "price": "16",
  "trigger_price": "0",
  "disclosed_quantity": "0"
}
```

## Sample API Request (Stop-Loss Order)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "Python",
  "symbol": "RELIANCE",
  "action": "SELL",
  "exchange": "NSE",
  "pricetype": "SL",
  "product": "MIS",
  "quantity": "1",
  "price": "1180",
  "trigger_price": "1185"
}
```

## Sample API Response (Analyzer Mode)

```json
{
  "mode": "analyze",
  "orderid": "26100376733573",
  "status": "success"
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key (1 to 256 characters) | Mandatory | - |
| strategy | Strategy name, used to tag and log the order | Mandatory | - |
| symbol | OpenAlgo symbol (e.g. RELIANCE, NIFTY27OCT26FUT, NIFTY06OCT2622400CE) | Mandatory | - |
| action | BUY or SELL (lowercase is accepted) | Mandatory | - |
| exchange | NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX or CRYPTO (case-sensitive) | Mandatory | - |
| pricetype | MARKET, LIMIT, SL or SL-M | Optional | MARKET |
| product | MIS, CNC or NRML | Optional | MIS |
| quantity | Positive number. Whole numbers only, except on CRYPTO | Mandatory | - |
| price | Limit price, for LIMIT and SL orders | Optional | 0 |
| trigger_price | Trigger price, for SL and SL-M orders | Optional | 0 |
| disclosed_quantity | Disclosed quantity, a non-negative whole number | Optional | 0 |
| underlying_ltp | Underlying price passed along by an options order; accepted and not needed for a plain order | Optional | null |

Numbers may be sent as JSON numbers or as numeric strings (`"1"`, `"16"`). Any other field is refused as `Unknown field.`

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| orderid | string | The broker's order id (in analyzer mode, a 14-digit sandbox id) |
| message | string | The reason, on an error |
| mode | string | `analyze` when analyzer mode is on; absent in live mode |

## Analyzer Mode

With analyzer mode on, the order goes to the sandbox, never to your broker, and every answer carries `"mode": "analyze"`.

- MARKET orders fill at once at the ask (buy) or bid (sell), or at the last traded price when there is no bid or ask.
- LIMIT, SL and SL-M orders rest (`open` or `trigger pending`) and fill from live prices.
- Margin is blocked when the order is placed, netted against an opposite position.
- F&O quantities must be a multiple of the lot size.
- MIS orders that do not reduce an open MIS position are refused from the exchange's square-off time until 09:00 IST.
- A price older than 30 seconds is not filled at; the order waits for a fresh one.

See [Sandbox mode](../../user/sandbox.md) for how fills, margin and settlement work.

## Errors

Validation errors on this endpoint carry `message` as a string that spells out a Python dictionary, with no `mode` key, as on the web:

```json
{
  "message": "{'quantity': ['Quantity must be a positive number.']}",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `{'symbol': ['Missing data for required field.']}` | A required field is missing |
| 400 | `{'quantity': ['Quantity must be a positive number.']}` | Quantity is zero or negative |
| 400 | `{'quantity': ['Fractional quantity (1.5) is not allowed for non-crypto exchanges.']}` | A fractional quantity outside CRYPTO |
| 400 | `{'quantity': ['Quantity is larger than any exchange accepts.']}` | Quantity above 2147483647 |
| 400 | `{'exchange': ['Must be one of: NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX, CRYPTO.']}` | Unknown exchange |
| 400 | `{'product': ['Must be one of: MIS, NRML, CNC.']}` | Unknown product (likewise `action`, `pricetype`) |
| 400 | `{'price': ['Price must be a non-negative number.']}` | Negative price |
| 400 | The broker's own reason | The broker refused the order |
| 429 | `OpenAlgo is pacing requests to stay within your broker's rate limit, and this one would have had to wait too long for its turn. Try again in a few seconds.` | The order would have waited more than 10 seconds for its turn at the broker; it was not sent (see [Rate limiting](../rate-limiting.md)) |
| 400 | `Fractional quantities are not supported in OpenAlgo Desktop yet. Use a whole-number quantity.` | A fractional CRYPTO quantity in analyzer mode |
| 500 | `Failed to place order due to internal error` | The order was sent and no definite answer came back (a timeout or a dropped connection) |
| 500 | `OpenAlgo could not read whether analyzer mode is on, so nothing was sent. Restart OpenAlgo, then try again.` | The analyzer setting could not be read; nothing was sent |
| 500 | `OpenAlgo could not read whether your orders wait for approval in the Action Center (Semi-Auto), so nothing was sent. Try again in a moment; if it keeps happening, restart OpenAlgo.` | The API key's order mode could not be read; nothing was sent |

In analyzer mode the sandbox adds its own refusals, with `"mode": "analyze"`:

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `Symbol NOTASYMBOL not found on NSE` | The symbol is not in the master contract |
| 400 | `LIMIT orders require price` | LIMIT or SL with no price |
| 400 | `SL orders require trigger_price` | SL or SL-M with no trigger price |
| 400 | `Quantity must be in multiples of lot size 65` | F&O quantity not a lot multiple |
| 400 | `NRML product not allowed for NSE equity segment. Use CNC for delivery or MIS for intraday.` | NRML on NSE or BSE |
| 400 | `CNC product not allowed for NFO derivatives segment. Use NRML for carryforward or MIS for intraday.` | CNC on a derivatives exchange |
| 400 | `MIS orders cannot be placed after square-off time (15:15 IST). Trading resumes at 09:00 AM IST.` | A new MIS position outside trading hours |
| 400 | `Insufficient funds. Required: ..., Available: ..., Shortage: ...` | Sandbox funds cannot cover the margin |
| 400 | `Cannot sell SBIN in CNC. No positions or holdings available. ...` | A CNC sell with nothing to sell (the rejected order is recorded and its `orderid` is in the answer) |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- MARKET orders need no price. LIMIT orders need `price`. SL orders need `price` and `trigger_price`. SL-M orders need `trigger_price`.
- The symbol is in OpenAlgo format: `RELIANCE`, `NIFTY27OCT26FUT`, `NIFTY06OCT2622400CE`.
- Use MIS for intraday, CNC for equity delivery and NRML for F&O carried overnight.
- When the API key's order mode is Semi-Auto, the order is not sent: it waits in the Action Center for your approval and the answer is `{"status": "success", "message": "Order queued for approval in Action Center", "mode": "semi_auto", "pending_order_id": 12}`.
- On Delta Exchange, the leverage saved on the Leverage page is set for the instrument before each order.
- Rate limit: 10 requests per second per caller, shared with the other order endpoints. See [Rate limiting](../rate-limiting.md).

## In the desktop

- In the desktop, an order sent with no definite answer from the broker (a timeout or a dropped connection after sending) is never sent again: OpenAlgo looks for it in the broker's order book and settles it there. The answer keeps the web's shape. See [Order placement outcomes](../../port/order-outcomes.md).
- In the desktop, a quantity above 2147483647 is refused before anything is sent; the web passed it on.
- In the desktop, a fractional CRYPTO quantity in analyzer mode is refused; the web's sandbox truncates it to a whole number.
- An order the broker pacer cannot send within 10 seconds is answered 429, as on the web; the desktop always bounds the wait, the web only on a multi-threaded server.
- In the desktop, if the order mode (Auto or Semi-Auto) cannot be read, the order is refused with 500 and nothing is sent; the web treats an unreadable mode as Auto and places it.

---

**Back to**: [API Documentation](../README.md)
