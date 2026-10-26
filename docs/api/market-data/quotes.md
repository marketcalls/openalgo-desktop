# Quotes

Get the latest quote for one symbol: last traded price, the day's open, high
and low, previous close, best bid and ask with their quantities, volume and
open interest.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/quotes
```

5000 is the default App port. If you changed it in Server Settings, use your
port.

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "symbol": "RELIANCE",
  "exchange": "NSE"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/quotes \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "symbol": "RELIANCE",
  "exchange": "NSE"
}'
```

## Sample API Response

Recorded with the market closed, so bid, ask and volume are 0.

```json
{
  "status": "success",
  "data": {
    "ask": 0,
    "ask_qty": 0,
    "bid": 0,
    "bid_qty": 0,
    "high": 1183.9,
    "low": 1160.8,
    "ltp": 1167.7,
    "oi": 0,
    "open": 1180.1,
    "prev_close": 1187,
    "volume": 0
  }
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key, from the API Key page | Mandatory | - |
| symbol | Symbol in OpenAlgo format, for example `RELIANCE`, `NIFTY27OCT26FUT`, `NIFTY06OCT2622400CE` | Mandatory | - |
| exchange | One of NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX, CRYPTO. Upper case only. | Mandatory | - |

The API key goes in the JSON body. A key sent only in an `X-API-KEY` header is
not read, as on OpenAlgo web.

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | "success" or "error" |
| data | object | Quote data object |

### Data Object Fields

| Field | Type | Description |
|-------|------|-------------|
| ask | number | Best ask (sell) price |
| ask_qty | number | Quantity at the best ask |
| bid | number | Best bid (buy) price |
| bid_qty | number | Quantity at the best bid |
| high | number | Day's high |
| low | number | Day's low |
| ltp | number | Last traded price |
| oi | number | Open interest (F&O); 0 for cash and index symbols |
| open | number | Day's open |
| prev_close | number | Previous day's close |
| volume | number | Volume traded today |

## Errors

| HTTP | message | Cause and fix |
|------|---------|---------------|
| 400 | Object of field errors, for example `{"symbol": ["Missing data for required field."]}` | A field is missing, unknown or not valid. Lower-case exchanges are refused. |
| 400 | `Symbol 'XYZ' not found for exchange 'NSE'. Please verify the symbol name and ensure master contracts are downloaded.` | The symbol is not in the master contract. Check the symbol format, or download the master contract again. |
| 403 | `Invalid openalgo apikey` | The key is wrong, or no broker is logged in. Log in to your broker and use the key from the API Key page. |
| 500 | The broker's own message | The broker refused the request or could not be reached. |

Answers every endpoint shares (OpenAlgo locked, rate limit, wrong method) are
in the [API overview](../README.md).

## Notes

- Quotes always come from your broker, also in analyzer mode, so a broker must
  be logged in.
- In the desktop every broker answers with all eleven fields above. Whole
  prices come back without a decimal point (`1187`), others with one
  (`1167.7`); read every price as a number.
- If OpenAlgo is pacing requests to stay within your broker's rate limit and
  this one would have waited more than about ten seconds, it is refused with
  429 and "OpenAlgo is pacing requests to stay within your broker's rate
  limit, ... Try again in a few seconds.", as on OpenAlgo web.
- For several symbols in one call use [MultiQuotes](./multiquotes.md). For
  prices as they change use the [Quote WebSocket](../websocket-streaming/quote.md).

## Example: F&O Quote

```json
{
  "apikey": "<APIKEY>",
  "symbol": "NIFTY06OCT2622400CE",
  "exchange": "NFO"
}
```

```json
{
  "status": "success",
  "data": {
    "ask": 0,
    "ask_qty": 0,
    "bid": 0,
    "bid_qty": 0,
    "high": 282.6,
    "low": 92.2,
    "ltp": 156.95,
    "oi": 3635255,
    "open": 253,
    "prev_close": 301.05,
    "volume": 0
  }
}
```

## Related Endpoints

- [MultiQuotes](./multiquotes.md) - quotes for several symbols
- [Depth](./depth.md) - five levels of the order book

---

**Back to**: [API Documentation](../README.md)
