# OptionsOrder

Place an options order by naming a strike offset (ATM, ITM or OTM) instead of a contract. OpenAlgo reads the underlying's price, picks the strike from the master contract and places the order on that contract.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/optionsorder
```

## Sample API Request (ATM Option)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "python",
  "underlying": "NIFTY",
  "exchange": "NSE_INDEX",
  "expiry_date": "06OCT26",
  "offset": "ATM",
  "option_type": "CE",
  "action": "BUY",
  "quantity": 65,
  "pricetype": "MARKET",
  "product": "NRML"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/optionsorder \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy": "python",
  "underlying": "NIFTY",
  "exchange": "NSE_INDEX",
  "expiry_date": "06OCT26",
  "offset": "ATM",
  "option_type": "CE",
  "action": "BUY",
  "quantity": 65,
  "pricetype": "MARKET",
  "product": "NRML"
}'
```

## Sample API Response (ATM Option)

```json
{
  "exchange": "NFO",
  "offset": "ATM",
  "option_type": "CE",
  "orderid": "26100318862823",
  "status": "success",
  "symbol": "NIFTY06OCT2622400CE",
  "underlying": "NIFTY",
  "underlying_ltp": 22421.95
}
```

In analyzer mode the same answer also carries `"mode": "analyze"`.

## Sample API Request (ITM Option)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "python",
  "underlying": "NIFTY",
  "exchange": "NSE_INDEX",
  "expiry_date": "06OCT26",
  "offset": "ITM2",
  "option_type": "PE",
  "action": "BUY",
  "quantity": 65,
  "pricetype": "MARKET",
  "product": "NRML"
}
```

## Sample API Response (ITM Option, Analyzer Mode)

```json
{
  "exchange": "NFO",
  "mode": "analyze",
  "offset": "ITM2",
  "option_type": "PE",
  "orderid": "26100358895357",
  "status": "success",
  "symbol": "NIFTY06OCT2622500PE",
  "underlying": "NIFTY",
  "underlying_ltp": 22421.95
}
```

## Sample API Request (Split Into Chunks)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "python",
  "underlying": "NIFTY",
  "exchange": "NSE_INDEX",
  "expiry_date": "06OCT26",
  "offset": "ATM",
  "option_type": "CE",
  "action": "BUY",
  "quantity": 130,
  "splitsize": 65,
  "pricetype": "MARKET",
  "product": "NRML"
}
```

## Sample API Response (Split Into Chunks, Analyzer Mode)

With `splitsize`, the answer has `results`, `split_size` and `total_quantity` instead of `orderid`.

```json
{
  "exchange": "NFO",
  "mode": "analyze",
  "offset": "ATM",
  "option_type": "CE",
  "results": [
    {
      "order_num": 1,
      "orderid": "26100341149649",
      "quantity": 65,
      "status": "success"
    },
    {
      "order_num": 2,
      "orderid": "26100359455808",
      "quantity": 65,
      "status": "success"
    }
  ],
  "split_size": 65,
  "status": "success",
  "symbol": "NIFTY06OCT2622400CE",
  "total_quantity": 130,
  "underlying": "NIFTY",
  "underlying_ltp": 22421.95
}
```

## Offset Values

| Offset | Description |
|--------|-------------|
| ATM | At the money: the listed strike nearest the underlying's price |
| ITM1 to ITM50 | In the money, 1 to 50 strikes away |
| OTM1 to OTM50 | Out of the money, 1 to 50 strikes away |

| Option Type | ITM Direction | OTM Direction |
|-------------|---------------|---------------|
| CE (Call) | Lower strikes | Higher strikes |
| PE (Put) | Higher strikes | Lower strikes |

This endpoint does not check the 1 to 50 range itself: an offset is accepted as long as that many listed strikes exist, as on the web.

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| strategy | Strategy name, used to tag and log the order | Mandatory | - |
| underlying | Underlying symbol (`NIFTY`, `BANKNIFTY`, `RELIANCE`), or a futures symbol that carries the expiry (`NIFTY27OCT26FUT`) | Mandatory | - |
| exchange | Exchange of the underlying: `NSE_INDEX`, `BSE_INDEX`, `NSE`, `BSE`, `NFO`, `BFO`, `MCX` and the other exchanges in the shared list | Mandatory | - |
| expiry_date | Expiry in `DDMMMYY` format (`06OCT26`) | Optional | Taken from `underlying` when it carries one |
| strike_int | Strike interval. When given, strikes are counted in steps of this size from the multiple nearest the price; when left out, the listed strikes are used (recommended) | Optional | - |
| offset | `ATM`, `ITM1` to `ITM50`, or `OTM1` to `OTM50` | Mandatory | - |
| option_type | `CE` or `PE` (lowercase is accepted) | Mandatory | - |
| action | `BUY` or `SELL` (lowercase is accepted) | Mandatory | - |
| quantity | Whole number of units, at least 1. A decimal is cut down to a whole number (65.9 becomes 65), as on the web | Mandatory | - |
| splitsize | Chunk size. 0 places one order; above 0 splits the quantity into orders of this size, the remainder last | Optional | 0 |
| pricetype | `MARKET`, `LIMIT`, `SL` or `SL-M` | Optional | MARKET |
| product | `MIS` or `NRML` (options take no CNC) | Optional | MIS |
| price | Limit price, for LIMIT and SL orders | Optional | 0 |
| trigger_price | Trigger price, for SL and SL-M orders | Optional | 0 |
| disclosed_quantity | Disclosed quantity, a non-negative whole number | Optional | 0 |

Numbers may be sent as JSON numbers or numeric strings. Any other field is refused as `Unknown field.`

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| orderid | string | The order id (without `splitsize`) |
| symbol | string | The option contract that was traded |
| exchange | string | Where it was placed (`NFO`, `BFO`, `MCX` and so on) |
| underlying | string | The `underlying` you sent |
| underlying_ltp | number | The underlying price used to pick the strike |
| offset | string | The offset you sent |
| option_type | string | `CE` or `PE` |
| results | array | With `splitsize`: one entry per chunk, `{order_num, quantity, status, orderid}` or `{order_num, quantity, status, message}` |
| split_size | number | With `splitsize`: the chunk size |
| total_quantity | number | With `splitsize`: the quantity asked for |
| mode | string | `analyze` in analyzer mode; absent in live mode |
| message | string | The reason, on an error |

## Analyzer Mode

With analyzer mode on, the contract is still picked from the live underlying price, then the order goes to the sandbox and the answer carries `"mode": "analyze"`. The order fills the way a [PlaceOrder](./placeorder.md) order does: MARKET at once, LIMIT and stop orders from live prices, never at a price older than 30 seconds. F&O quantities must be a multiple of the lot size. A broker session is still needed, for the underlying price. See [Sandbox mode](../../user/sandbox.md).

## Errors

Validation errors on this endpoint use their own shape: a fixed `message` and the field errors as an object under `errors`, with no `mode` key:

```json
{
  "errors": {
    "option_type": [
      "Must be one of: CE, PE, ce, pe."
    ]
  },
  "message": "Validation error",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `Validation error` (field errors in `errors`) | A required field is missing or a value is not allowed, for example `product` CNC |
| 400 | `Offset ATM99X is out of range for available strikes. Please use a smaller offset.` | The offset does not start with ATM, ITM or OTM, or there are not that many listed strikes |
| 400 | `invalid literal for int() with base 10: 'X'` | ITM or OTM is followed by something that is not a number (`ITMX`), as on the web |
| 400 | `Expiry date required. Provide via expiry_date parameter or embed in underlying (e.g., NIFTY28OCT25FUT).` | No expiry was given |
| 400 | `Total number of orders would exceed maximum limit of 100` | `quantity / splitsize` needs more than 100 orders |
| 400 | `Quantity is larger than any exchange accepts.` | A live order quantity above 2147483647; nothing was sent |
| 400 | The broker's own reason | The broker refused the order |
| 429 | `OpenAlgo is pacing requests to stay within your broker's rate limit, and this one would have had to wait too long for its turn. Try again in a few seconds.` | The order would have waited more than 10 seconds for its turn at the broker; it was not sent |
| 404 | `No strikes found for NIFTY expiring 01JAN20. Please check expiry date or update master contract.` | The expiry is not a `DDMMMYY` date the master contract lists (the dashed `06-OCT-26` that `/expiry` returns must be turned into `06OCT26` first) |
| 404 | `Option symbol NIFTY06OCT2622450CE not found in NFO. Symbol may not exist or master contract needs update.` | The picked strike is not listed (usually with `strike_int`) |
| 500 | `Failed to place order due to internal error` | The order was sent and no definite answer came back |

In analyzer mode the sandbox refusals of [PlaceOrder](./placeorder.md#errors) apply, with `"mode": "analyze"`; for example:

```json
{
  "message": "Quantity must be in multiples of lot size 65",
  "mode": "analyze",
  "status": "error"
}
```

With `splitsize`, a chunk that fails does not stop the others: the answer is 200 with `status: success`, and each entry in `results` has its own `status` and `message`.

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- The underlying's price is read from the index or stock (`NSE_INDEX`, `NSE`), or from the near-month future where there is no spot (MCX, CDS, BCD, NCDEX, NCO).
- `NSE_INDEX` and `NSE` place on `NFO`; `BSE_INDEX` and `BSE` place on `BFO`.
- `/expiry` returns dates as `DD-MMM-YY` (`06-OCT-26`); this endpoint takes `DDMMMYY` (`06OCT26`).
- Chunks of a split are sent one after another, 100 ms apart.
- When the API key's order mode is Semi-Auto, the order is not placed: it waits in the Action Center and the answer is `{"status": "success", "message": "Order queued for approval in Action Center", "mode": "semi_auto", "pending_order_id": 12}`.
- If the broker's answer is lost (a timeout after sending), the order may have been placed: check the order book before sending it again.
- Rate limit: 10 requests per second per caller, shared with the other order endpoints. See [Rate limiting](../rate-limiting.md).

## In the desktop

- An order the broker pacer cannot send within 10 seconds is answered 429, as on the web; the desktop always bounds the wait, the web only on a multi-threaded server.
- In the desktop, a live order quantity above 2147483647 is refused before anything is sent; the web has no such check.
- In the desktop, an order sent with no definite answer from the broker is never sent again: OpenAlgo looks for it in the broker's order book. The answer keeps the web's shape. See [Order placement outcomes](../../port/order-outcomes.md).
- In the desktop, if OpenAlgo cannot read whether the order mode is Semi-Auto, the order is refused (500) instead of being placed; the web places it.

---

**Back to**: [API Documentation](../README.md)
