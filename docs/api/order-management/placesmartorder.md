# PlaceSmartOrder

Place an order that brings your open position in one symbol to a target size. OpenAlgo reads the current position for the symbol, exchange and product, works out what to buy or sell to reach `position_size`, and places that order, or nothing when the position already matches.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/placesmartorder
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "strategy": "Test Strategy",
  "exchange": "NSE",
  "symbol": "IDEA",
  "action": "BUY",
  "product": "MIS",
  "pricetype": "MARKET",
  "quantity": "1",
  "position_size": "5",
  "price": "0",
  "trigger_price": "0",
  "disclosed_quantity": "0"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/placesmartorder \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy": "Test Strategy",
  "exchange": "NSE",
  "symbol": "IDEA",
  "action": "BUY",
  "product": "MIS",
  "pricetype": "MARKET",
  "quantity": "1",
  "position_size": "5",
  "price": "0",
  "trigger_price": "0",
  "disclosed_quantity": "0"
}'
```

## Sample API Response

```json
{
  "orderid": "240307000616990",
  "status": "success"
}
```

## Sample API Response (No Order Needed)

```json
{
  "message": "Positions Already Matched. No Action needed.",
  "status": "success"
}
```

## Sample API Response (Analyzer Mode)

Taking an INFY MIS position from flat to 10 shares (`"action": "BUY"`, `"quantity": 10`, `"position_size": 10`):

```json
{
  "mode": "analyze",
  "orderid": "26100341320743",
  "status": "success"
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key (1 to 256 characters) | Mandatory | - |
| strategy | Strategy name, used to tag and log the order | Mandatory | - |
| exchange | NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX or CRYPTO (case-sensitive) | Mandatory | - |
| symbol | OpenAlgo symbol | Mandatory | - |
| action | BUY or SELL (lowercase is accepted). Used only when both the position and `position_size` are zero | Mandatory | - |
| quantity | Zero or a positive number. Used only when both the position and `position_size` are zero | Mandatory | - |
| position_size | The position you want after the order: positive for long, negative for short, 0 for flat | Mandatory | - |
| product | MIS, CNC or NRML | Optional | MIS |
| pricetype | MARKET, LIMIT, SL or SL-M | Optional | MARKET |
| price | Limit price, for LIMIT and SL orders | Optional | 0 |
| trigger_price | Trigger price, for SL and SL-M orders | Optional | 0 |
| disclosed_quantity | Disclosed quantity, a non-negative whole number | Optional | 0 |

Numbers may be sent as JSON numbers or as numeric strings. Any other field is refused as `Unknown field.`

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| orderid | string | The order id, when an order was placed |
| message | string | Why no order was placed, or the reason for an error |
| mode | string | `analyze` when analyzer mode is on; absent in live mode |

## How the Order Is Worked Out

OpenAlgo compares `position_size` with your current net position in the same symbol, exchange and product:

| Action | Qty (API) | position_size | Current position | What OpenAlgo does |
|--------|-----------|---------------|------------------|--------------------|
| BUY | 100 | 0 | 0 | No position: BUY 100 |
| BUY | 100 | 100 | -100 | BUY 200 to reach +100 |
| BUY | 100 | 100 | 100 | Nothing: position matched |
| BUY | 100 | 200 | 100 | BUY 100 to reach +200 |
| SELL | 100 | 0 | 0 | No position: SELL 100 |
| SELL | 100 | -100 | +100 | SELL 200 to reach -100 |
| SELL | 100 | -100 | -100 | Nothing: position matched |
| SELL | 100 | -200 | -100 | SELL 100 to reach -200 |
| any | any | 0 | +5 | SELL 5 to go flat |
| any | any | 0 | -4 | BUY 4 to go flat |

Outside the first and fifth rows, `action` and `quantity` are ignored: the side and size come from the difference between `position_size` and the current position.

When the position already equals `position_size`, no order is placed and the answer is a success with a message:

| Request | `message` |
|---------|-----------|
| `quantity` is 0 | `No OpenPosition Found. Not placing Exit order.` |
| `quantity` is not 0 | `Positions Already Matched. No Action needed.` |

`position_size` is read as a whole number: a fraction is dropped (2.5 is read as 2).

## Analyzer Mode

With analyzer mode on, the sandbox reads its own position for the symbol and places the order in the sandbox, never at your broker. Every answer carries `"mode": "analyze"`, the no-action answers included:

```json
{
  "message": "No OpenPosition Found. Not placing Exit order.",
  "mode": "analyze",
  "status": "success"
}
```

The order then fills or rests as a [PlaceOrder](./placeorder.md) order would. See [Sandbox mode](../../user/sandbox.md).

## Errors

Validation errors carry `message` as a string that spells out a Python dictionary. In analyzer mode the body also carries `"mode": "analyze"`:

```json
{
  "message": "{'position_size': ['Missing data for required field.']}",
  "mode": "analyze",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `{'position_size': ['Missing data for required field.']}` | A required field is missing |
| 400 | `{'quantity': ['Quantity must be a non-negative number.']}` | Negative quantity |
| 400 | `{'quantity': ['Fractional quantity (1.5) is not allowed for non-crypto exchanges.']}` | A fractional quantity outside CRYPTO |
| 400 | `{'quantity': ['Quantity is larger than any exchange accepts.']}` | Quantity above 2147483647 |
| 400 | `Fractional quantities are not supported in OpenAlgo Desktop yet. Use a whole-number quantity.` | A fractional CRYPTO quantity, live or in analyzer mode |
| 400 | The broker's own reason | The broker refused the order, or the position could not be read |
| 429 | `OpenAlgo is pacing requests to stay within your broker's rate limit, and this one would have had to wait too long for its turn. Try again in a few seconds.` | The order would have waited more than 10 seconds for its turn at the broker; it was not sent |
| 500 | `Failed to place order due to internal error` | The order was sent and no definite answer came back |
| 500 | `Failed to place smart order due to internal error` | The current position could not be read from the broker |

In analyzer mode the sandbox refuses an order for the same reasons as [PlaceOrder](./placeorder.md#errors), for example:

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `Symbol NOTASYMBOL not found on NSE` | The symbol is not in the master contract |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- Smart orders suit position-based strategies (TradingView alerts, Amibroker) where each signal names the position you want, not the order to send.
- Two smart orders for the same symbol, exchange and product are handled one after the other, so both never read the same starting position.
- When the API key's order mode is Semi-Auto, the order is not sent: it waits in the Action Center for your approval and the answer is `{"status": "success", "message": "Order queued for approval in Action Center", "mode": "semi_auto", "pending_order_id": 12}`. This applies in analyzer mode too.
- Rate limit: 10 requests per second per caller, in a window of its own (not shared with the other order endpoints). See [Rate limiting](../rate-limiting.md).

## In the desktop

- In the desktop, a live smart order that needs no order always answers 200 success with one of the two messages above, as analyzer mode does, whatever the broker. On the web the live answer depends on the broker's adapter: Zerodha, for example, answers `Positions Already Matched. No Action needed.` in both cases.
- In the desktop, a fractional CRYPTO quantity is refused; smart orders carry whole units only.
- In the desktop, an order sent with no definite answer from the broker is never sent again; OpenAlgo settles it from the broker's order book. See [Order placement outcomes](../../port/order-outcomes.md).
- An order the broker pacer cannot send within 10 seconds is answered 429, as on the web; the desktop always bounds the wait, the web only on a multi-threaded server.

---

**Back to**: [API Documentation](../README.md)
