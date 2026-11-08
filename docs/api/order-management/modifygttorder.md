# ModifyGTTOrder

Change an active GTT: its trigger prices, limit prices, quantity or price type. The request is a full replacement, the same body as [PlaceGTTOrder](./placegttorder.md) plus `trigger_id`.

Send everything you want to keep. Modify is not a patch: a field you leave out is not carried over from the old GTT.

## SINGLE or OCO: Same Type as Before

You can change the price levels, the quantity and the price type, but not turn a SINGLE into an OCO or the other way round. To do that, cancel the GTT and place a new one.

| Type | Use it when | Triggers | Orders placed |
|------|-------------|----------|---------------|
| **SINGLE** | One entry or exit at a level | 1 | 1 |
| **OCO** (one cancels the other) | A stop-loss and a target on a position | 2 | 1 of the 2. Where the broker has a native OCO the other is cancelled; on Angel One it is not (see [Angel One](./placegttorder.md#angel-one-an-oco-is-two-separate-rules)) |

For a SINGLE, put the trigger in `triggerprice_sl` when it sits below the last price and in `triggerprice_tg` when it sits above, and send the other as `0`. For an OCO, send both triggers and both leg limits (`stoploss`, `target`). See [PlaceGTTOrder](./placegttorder.md#choosing-triggerprice_sl-or-triggerprice_tg-single).

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/modifygttorder
```

## Sample API Request (SINGLE: move the trigger and raise the quantity)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "My GTT Strategy",
  "trigger_id": "GTT-261003-12a093ec",
  "trigger_type": "SINGLE",
  "exchange": "NSE",
  "symbol": "SBIN",
  "action": "BUY",
  "product": "CNC",
  "quantity": 2,
  "pricetype": "LIMIT",
  "price": 839.6,
  "triggerprice_sl": 839.6,
  "triggerprice_tg": 0
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/modifygttorder \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy": "My GTT Strategy",
  "trigger_id": "GTT-261003-12a093ec",
  "trigger_type": "SINGLE",
  "exchange": "NSE",
  "symbol": "SBIN",
  "action": "BUY",
  "product": "CNC",
  "quantity": 2,
  "pricetype": "LIMIT",
  "price": 839.6,
  "triggerprice_sl": 839.6,
  "triggerprice_tg": 0
}'
```

## Sample API Response

```json
{
  "status": "success",
  "trigger_id": "23132604291205"
}
```

## Sample API Response (Analyzer Mode)

```json
{
  "mode": "analyze",
  "status": "success",
  "trigger_id": "GTT-261003-12a093ec"
}
```

## Sample API Request (OCO: raise the target)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "Bracket OCO",
  "trigger_id": "GTT-261003-56ad1482",
  "trigger_type": "OCO",
  "exchange": "NSE",
  "symbol": "SBIN",
  "action": "SELL",
  "product": "CNC",
  "quantity": 1,
  "pricetype": "LIMIT",
  "price": 0,
  "triggerprice_sl": 858.7,
  "stoploss": 849.1,
  "triggerprice_tg": 1097.2,
  "target": 1106.8
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| strategy | Strategy name | Mandatory | - |
| trigger_id | The id [PlaceGTTOrder](./placegttorder.md) returned | Mandatory | - |
| trigger_type | `SINGLE` or `OCO`; must match the GTT | Mandatory | - |
| exchange | Exchange of the GTT; cannot change | Mandatory | - |
| symbol | Symbol of the GTT; cannot change | Mandatory | - |
| action | `BUY` or `SELL`; cannot change | Mandatory | - |
| product | `CNC` or `NRML` (MIS is refused) | Mandatory | - |
| quantity | New quantity, a positive whole number (a lot multiple for F&O) | Mandatory | - |
| pricetype | `LIMIT` or `MARKET` | Optional | LIMIT |
| price | SINGLE: the new limit price; 0 for MARKET. OCO: not used, send 0 | Mandatory | - |
| triggerprice_sl | SINGLE: the trigger when below the last price. OCO: the stop-loss leg's trigger | Conditional | 0 |
| triggerprice_tg | SINGLE: the trigger when above the last price. OCO: the target leg's trigger | Conditional | 0 |
| stoploss | OCO only: the stop-loss leg's limit price | Conditional | null |
| target | OCO only: the target leg's limit price | Conditional | null |

`stoploss`, `target`, `triggerprice_sl` and `triggerprice_tg` may be sent as `""` (read as unset). Fields this endpoint does not know, `expires_at` included, are ignored: a modify keeps the GTT's expiry.

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| trigger_id | string | The GTT's id (the one you sent) |
| mode | string | `analyze` in analyzer mode; absent in live mode |
| message | string | The reason, on an error |

## What Can Be Changed

| Parameter | Can change | Notes |
|-----------|------------|-------|
| Trigger prices (`triggerprice_sl`, `triggerprice_tg`) | Yes | Whether an OCO's two legs change in one step depends on the broker (below) |
| Limit prices (`price`, `stoploss`, `target`) | Yes | |
| `quantity` | Yes | A lot multiple for F&O |
| `pricetype` | Yes | LIMIT or MARKET |
| `trigger_type` | No | Cancel and place a new GTT |
| `symbol`, `exchange` | No | |
| `action` | No | |

## Analyzer Mode

With analyzer mode on, the sandbox GTT is changed and the answer carries `"mode": "analyze"`.

- Only an `active` GTT can be changed. A GTT that fired, was cancelled or lapsed answers 404.
- The reservation is worked out again for the new prices and quantity. If it grows and your sandbox funds cannot cover the difference, nothing changes and the answer is 400.
- The GTT keeps its expiry and the last price recorded when it was placed.

See [Sandbox mode](../../user/sandbox.md).

## Errors

Validation errors carry `message` as a string that spells out a Python dictionary; in analyzer mode the answer also carries `"mode": "analyze"`:

```json
{
  "message": "{'trigger_id': ['Missing data for required field.']}",
  "mode": "analyze",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `{'trigger_id': ['Missing data for required field.']}` | No `trigger_id` |
| 400 | `{'trigger_type': ["Must be 'SINGLE' or 'OCO'."]}` | `trigger_type` is anything else |
| 400 | `{'triggerprice_sl': ['Stoploss trigger must be less than target trigger (triggerprice_tg).']}` | An OCO whose stop-loss trigger is not below its target trigger |
| 400 | `{'product': ['GTT supports only CNC (delivery) or NRML (overnight F&O); MIS is intraday-only.']}` | `product` is MIS |
| 400 | The other field errors of [PlaceGTTOrder](./placegttorder.md#errors) | Missing or invalid prices or quantity |
| 400 | The broker's own reason | The broker refused the change |
| 400 | `Could not fetch the last price from Zerodha to place the GTT. Try again.` | The broker needs the last price and it could not be read |
| 403 | `Modify GTT order is not allowed in Semi-Auto mode. Switch to Auto mode.` | The API key's order mode is Semi-Auto (live mode only) |
| 501 | `GTT orders are not supported for broker 'shoonya' yet` | Your broker has no GTT in OpenAlgo |
| 500 | `Failed to modify GTT due to internal error` | The broker call failed without a readable answer |

In analyzer mode the sandbox adds its own refusals, with `"mode": "analyze"`:

| HTTP | `message` | When |
|------|-----------|------|
| 404 | `No active GTT with trigger_id 'GTT-261003-12a093ec'` | The id is unknown, or the GTT is no longer active |
| 400 | `trigger_type cannot be changed (SINGLE -> OCO). Cancel and re-place.` | A different `trigger_type` |
| 400 | `symbol cannot be changed on an existing GTT (SBIN -> INFY). Cancel and re-place.` | A different `symbol` (likewise `exchange` and `action`) |
| 400 | `Insufficient funds. Required: ₹42.00. The GTT is unchanged.` | The larger reservation cannot be covered; the answer also carries `trigger_id` |
| 400 | `Symbol not found` | The GTT's symbol is no longer in the master contract |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- Only active GTTs can be changed. A GTT that fired, was cancelled or lapsed cannot.
- You do not send the last price: OpenAlgo reads it from your broker where needed.
- Whether an OCO's two legs change in one step depends on the broker. Angel One changes its two rules one after the other, and a failure after the first leaves them different; Rupeezy needs your GTT book to be readable first. After an error, check both legs in the [GTT order book](./gttorderbook.md) before sending the modify again; an error does not mean the first change was undone. See [Angel One](./placegttorder.md#angel-one-an-oco-is-two-separate-rules).
- MARKET is handled as on [PlaceGTTOrder](./placegttorder.md#notes).
- Semi-Auto does not queue a modify; it refuses it (403) in live mode, as on the web.
- Rate limit: 10 requests per second per caller, shared with the other order endpoints. See [Rate limiting](../rate-limiting.md).

## In the desktop

- In the desktop, a failed analyzer-mode modify (an unknown or no longer active `trigger_id`) answers 404 `No active GTT with trigger_id '...'` and is recorded in the sandbox log; the web answers 500 `An unexpected error occurred` and logs nothing (a web defect).
- In the desktop, when the broker's last price cannot be read the answer is 400 with a message naming the broker; the web answers 502.

---

**Back to**: [API Documentation](../README.md)
