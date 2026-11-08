# PlaceGTTOrder

Place a GTT (Good Till Triggered) order: a price trigger that waits, for days if needed, until the last traded price crosses your level, then places the order for you.

## SINGLE or OCO

| Type | Use it when | Triggers | Orders placed |
|------|-------------|----------|---------------|
| **SINGLE** | You want one entry or exit at a level, for example "buy SBIN if it dips to 858.70" or "sell RELIANCE if it crosses 1450" | 1 | 1 |
| **OCO** (one cancels the other) | You hold a position and want a stop-loss and a target, whichever comes first, for example "I hold SBIN: sell at 858.70 on the way down or 1049.50 on the way up" | 2 | 1 of the 2. Where the broker has a native OCO the other is cancelled; on Angel One it is not (see [Angel One](#angel-one-an-oco-is-two-separate-rules)) |

A SINGLE has no second leg: once its trigger fires and the order is placed, the GTT is finished.

On Upstox an OCO opens a position first: see [Upstox: an OCO opens a position](#upstox-an-oco-opens-a-position).

## Choosing `triggerprice_sl` or `triggerprice_tg` (SINGLE)

For a SINGLE, one of the two fields is the trigger; send the other as `0`. Pick by where the trigger sits against the last price:

| Field | The trigger sits | Typical use |
|-------|------------------|-------------|
| `triggerprice_sl` | Below the last price | Stop-loss sell, buy on a dip |
| `triggerprice_tg` | Above the last price | Breakout buy, sell at a target |

If you send both above 0, `triggerprice_sl` is used.

For an OCO, send both: `triggerprice_sl` (the lower trigger, the stop-loss leg) and `triggerprice_tg` (the higher trigger, the target leg), plus each leg's limit price in `stoploss` and `target`.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/placegttorder
```

## Sample API Request (SINGLE: buy on a dip, LIMIT)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "My GTT Strategy",
  "trigger_type": "SINGLE",
  "exchange": "NSE",
  "symbol": "SBIN",
  "action": "BUY",
  "product": "CNC",
  "quantity": 1,
  "pricetype": "LIMIT",
  "price": 858.7,
  "triggerprice_sl": 858.7,
  "triggerprice_tg": 0
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/placegttorder \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy": "My GTT Strategy",
  "trigger_type": "SINGLE",
  "exchange": "NSE",
  "symbol": "SBIN",
  "action": "BUY",
  "product": "CNC",
  "quantity": 1,
  "pricetype": "LIMIT",
  "price": 858.7,
  "triggerprice_sl": 858.7,
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

## Sample API Request (SINGLE: breakout buy, MARKET)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "My GTT Strategy",
  "trigger_type": "SINGLE",
  "exchange": "NSE",
  "symbol": "RELIANCE",
  "action": "BUY",
  "product": "CNC",
  "quantity": 1,
  "pricetype": "MARKET",
  "price": 0,
  "triggerprice_sl": 0,
  "triggerprice_tg": 1450
}
```

The trigger sits above the last price, so it goes in `triggerprice_tg`. `price` is 0 because the order is MARKET.

## Sample API Request (OCO: stop-loss and target on a holding)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "Bracket OCO",
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
  "triggerprice_tg": 1049.5,
  "target": 1059.1
}
```

`price` is not used by an OCO: each leg places at its own limit, `stoploss` for the stop-loss leg and `target` for the target leg.

## Sample API Response (OCO, Analyzer Mode)

```json
{
  "mode": "analyze",
  "status": "success",
  "trigger_id": "GTT-261003-56ad1482"
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| strategy | Strategy name, kept with the GTT and in the logs | Mandatory | - |
| trigger_type | `SINGLE` or `OCO` (any case) | Mandatory | - |
| exchange | NSE, BSE, NFO, BFO, CDS, BCD, MCX and the other exchanges in the shared list (case-sensitive) | Mandatory | - |
| symbol | OpenAlgo symbol (`SBIN`, `NIFTY27OCT26FUT`, `NIFTY06OCT2622400CE`) | Mandatory | - |
| action | `BUY` or `SELL` (lowercase is accepted). For an OCO it applies to both legs | Mandatory | - |
| product | `CNC` (equity delivery) or `NRML` (F&O carried overnight). MIS is refused: a GTT can wait for days | Mandatory | - |
| quantity | Positive whole number | Mandatory | - |
| pricetype | `LIMIT` or `MARKET` | Optional | LIMIT |
| price | SINGLE: the limit price of the order placed; send 0 for MARKET. OCO: not used, send 0 | Mandatory | - |
| triggerprice_sl | SINGLE: the trigger, when it sits below the last price. OCO: the stop-loss leg's trigger | Conditional | 0 |
| triggerprice_tg | SINGLE: the trigger, when it sits above the last price. OCO: the target leg's trigger | Conditional | 0 |
| stoploss | OCO only: the stop-loss leg's limit price | Conditional | null |
| target | OCO only: the target leg's limit price | Conditional | null |
| expires_at | Analyzer mode only: when the GTT lapses, as an ISO date or date-time (`2027-03-31`, `2027-03-31T15:30:00+05:30`). A value that cannot be read is replaced by the default | Optional | One year from placement |

`stoploss`, `target`, `triggerprice_sl` and `triggerprice_tg` may also be sent as `""` (read as unset). Fields this endpoint does not know are ignored, not refused.

### Trigger Rules

| trigger_type | What to send | Rule |
|--------------|--------------|------|
| SINGLE | `triggerprice_sl` or `triggerprice_tg` above 0; the other 0 | `price` is the order's limit; 0 for MARKET |
| OCO | `triggerprice_sl`, `stoploss`, `triggerprice_tg` and `target`, all above 0 | `triggerprice_sl` must be below `triggerprice_tg`. Both legs share `action`, `quantity` and `product` |

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| trigger_id | string | The GTT's id: keep it to modify or cancel the GTT. The broker's id in live mode; `GTT-YYMMDD-<8 hex>` in analyzer mode |
| mode | string | `analyze` in analyzer mode; absent in live mode |
| message | string | The reason, on an error |

## Analyzer Mode

With analyzer mode on, the GTT is kept in the sandbox and never sent to your broker; the answer carries `"mode": "analyze"`.

- Margin is reserved when the GTT is placed: the leg's limit price (else its trigger) times the quantity, under the sandbox's leverage rules. For an OCO the larger of the two legs is reserved. The reservation shows as used margin in [Funds](../account-services/funds.md) and as `margin_blocked` in the [GTT order book](./gttorderbook.md).
- A leg fires when the last traded price reaches its trigger: at or below `triggerprice_sl`, at or above `triggerprice_tg`. A price older than 30 seconds triggers nothing.
- When a leg fires, the GTT becomes `triggered`, its reservation is released and the leg's order is placed in the sandbox like a [PlaceOrder](./placeorder.md) order. The order id is recorded on the leg (`triggered_order_id`). The other leg of an OCO never fires.
- If that order is refused (for example, not enough funds), the GTT goes back to `active` with its reservation; if the reservation cannot be restored, the GTT is marked `rejected`.
- A GTT lapses at `expires_at` (one year by default) and its reservation is released.

See [Sandbox mode](../../user/sandbox.md).

## Errors

Validation errors on this endpoint carry `message` as a string that spells out a Python dictionary. In analyzer mode the answer also carries `"mode": "analyze"`:

```json
{
  "message": "{'product': ['GTT supports only CNC (delivery) or NRML (overnight F&O); MIS is intraday-only.']}",
  "mode": "analyze",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `{'trigger_type': ["Must be 'SINGLE' or 'OCO'."]}` | `trigger_type` is anything else |
| 400 | `{'triggerprice_sl': ['SINGLE GTT requires a positive triggerprice_sl or triggerprice_tg.']}` | A SINGLE with no trigger above 0 |
| 400 | `{'triggerprice_sl': ['Required for OCO (stoploss trigger).']}` (likewise `stoploss`, `triggerprice_tg`, `target`) | An OCO without one of its four prices |
| 400 | `{'triggerprice_sl': ['Stoploss trigger must be less than target trigger (triggerprice_tg).']}` | An OCO whose stop-loss trigger is not below its target trigger |
| 400 | `{'product': ['GTT supports only CNC (delivery) or NRML (overnight F&O); MIS is intraday-only.']}` | `product` is MIS |
| 400 | `{'quantity': ['Quantity must be a positive number.']}` | Quantity is zero or negative |
| 400 | `{'quantity': ['Fractional quantity (1.5) is not allowed for non-crypto exchanges.']}` | A fractional quantity outside CRYPTO |
| 400 | `{'pricetype': ['Must be one of: LIMIT, MARKET.']}` | `pricetype` is SL or SL-M |
| 400 | The broker's own reason | The broker refused the GTT |
| 400 | `Could not fetch the last price from Zerodha to place the GTT. Try again.` | The broker needs the last price for the GTT and it could not be read (Zerodha; Angel One for a SINGLE; Upstox for an OCO) |
| 501 | `GTT orders are not supported for broker 'shoonya' yet` | Your broker has no GTT in OpenAlgo (see Notes) |
| 500 | `Failed to place GTT due to internal error` | The broker call failed without a readable answer |

In analyzer mode the sandbox adds its own refusals, with `"mode": "analyze"`:

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `Symbol not found` | The symbol is not in the master contract |
| 400 | `Insufficient funds. Required: ₹1059.10` | Sandbox funds cannot cover the reservation |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- Live GTT works with Zerodha, Upstox, Angel One, Fyers, Dhan and Rupeezy, as on the web. With any other broker, live GTT answers 501; analyzer mode works with every broker.
- You do not send the last price: OpenAlgo reads it from your broker where the broker's GTT needs it.
- Some brokers take only LIMIT orders in a GTT. There a MARKET request is sent as a LIMIT with a price protection band around the last price (SINGLE) or each trigger (OCO), so MARKET works the same everywhere.
- A GTT's order, once placed, is an ordinary order: it appears in the [order book](../account-services/orderbook.md). A trigger that fires does not mean its order fills, above all when a MARKET request was sent as a protected LIMIT.
- In analyzer mode the trigger is checked against live prices as they arrive; it does not reproduce a broker's queue, exchange liquidity or an outage.
- When the API key's order mode is Semi-Auto, the GTT is not placed: it waits in the Action Center and the answer is `{"status": "success", "message": "Order queued for approval in Action Center", "mode": "semi_auto", "pending_order_id": 12}`.
- Rate limit: 10 requests per second per caller, shared with the other order endpoints. See [Rate limiting](../rate-limiting.md).

### Angel One: an OCO is two separate rules

Angel One has no OCO rule. OpenAlgo places an OCO as two independent rules, the stop-loss leg and then the target leg, and answers with one `trigger_id` made of both rule ids, such as `1234567-1234568`. Keep the whole id for a modify or a cancel.

- When one rule fires, Angel One does not cancel the other. Cancel it yourself once the first has fired.
- If the second rule cannot be created, OpenAlgo cancels the first, so no half OCO is left. After any error, check the [GTT order book](./gttorderbook.md): a connection that drops during that cancel can still leave the first rule active.
- A modify or a cancel changes the two rules one after the other. If it fails after the first, the error names the leg that failed (modify) or the rule already cancelled (cancel); check both rules in the GTT order book before trying again.
- The GTT order book shows the two rules as two SINGLE rows, because Angel One does not keep them as a pair.

### Rupeezy

Rupeezy triggers on its own price, so no last price is read. A modify reads your GTT book first to find the legs; when the book cannot be read, nothing is changed. With `"status": "all"` the book includes the history.

### Upstox: an OCO opens a position

On Upstox an OCO does not behave as it does on Zerodha or Dhan. Every Upstox GTT must carry an entry rule, so OpenAlgo sends an OCO as an Upstox bracket whose entry fires at once at the last price: the position opens first, then the stop-loss and target are set.

| You send | Upstox does |
|----------|-------------|
| `action: "SELL"` | Buys at market now, then sets the target and stop-loss |
| `action: "BUY"` | Sells at market now, then sets the target and stop-loss |

The `stoploss` and `target` limit prices are not used: each Upstox rule fires at its trigger price. An entry that sells delivery shares needs EDIS: Upstox refuses the GTT with `UDAPI100500 EDIS is not validated` until you authorise EDIS once in Upstox Pro. To protect shares you already hold on Upstox, use a SINGLE sell GTT.

## In the desktop

- In the desktop, when the broker's last price cannot be read the answer is 400 with a message naming the broker; the web answers 502 ("Failed to fetch last_price from Zerodha quotes", likewise Upstox).
- In the desktop, a Fyers GTT carries the tag `openalgo`; the web sends the strategy name (letters and digits, up to 20) as the Fyers order tag.
- In the desktop, use whole-number quantities for a GTT: a fractional CRYPTO quantity is cut down to a whole number in analyzer mode (no live broker with GTT trades CRYPTO).
- In the desktop, if OpenAlgo cannot read whether the order mode is Semi-Auto, the GTT is refused (500) instead of being placed; the web places it.

---

**Back to**: [API Documentation](../README.md)
