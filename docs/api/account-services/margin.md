# Margin

Ask your broker how much margin a basket of orders needs, before you place them.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/margin
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "positions": [
    {
      "symbol": "NIFTY27OCT26FUT",
      "exchange": "NFO",
      "action": "BUY",
      "quantity": "65",
      "product": "NRML",
      "pricetype": "MARKET",
      "price": "0",
      "trigger_price": "0"
    },
    {
      "symbol": "NIFTY06OCT2622400CE",
      "exchange": "NFO",
      "action": "SELL",
      "quantity": "65",
      "product": "NRML",
      "pricetype": "MARKET",
      "price": "0",
      "trigger_price": "0"
    }
  ]
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/margin \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "positions": [
    {"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": "10",
     "product": "MIS", "pricetype": "MARKET", "price": "0", "trigger_price": "0"}
  ]
}'
```

## Sample API Response

```json
{
  "data": {
    "exposure_margin": 58424.535,
    "span_margin": 153198.25,
    "total_margin_required": 201421.035
  },
  "status": "success"
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| positions | 1 to 50 position objects | Mandatory | - |

### Position Object Fields

All values are strings, as in the example. A JSON number is refused with `Not a valid string.`

| Field | Description | Mandatory/Optional | Default Value |
|-------|-------------|-------------------|---------------|
| symbol | OpenAlgo symbol, 1 to 50 characters | Mandatory | - |
| exchange | Exchange code (NSE, NFO, BFO, MCX and so on) | Mandatory | - |
| action | BUY or SELL (lowercase is accepted) | Mandatory | - |
| quantity | Whole positive number, as a string | Mandatory | - |
| product | MIS, CNC or NRML | Mandatory | - |
| pricetype | MARKET, LIMIT, SL or SL-M | Mandatory | - |
| price | Price for LIMIT and SL, as a string | Optional | "0" |
| trigger_price | Trigger price for SL and SL-M, as a string | Optional | "0" |

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| data | object | The margin figures |

### Data Object Fields

| Field | Type | Description |
|-------|------|-------------|
| total_margin_required | number | Margin the whole basket needs, after any hedge benefit your broker gives |
| span_margin | number | SPAN part, where your broker reports it (0 otherwise) |
| exposure_margin | number | Exposure part, where your broker reports it (0 otherwise) |

## Analyzer Mode

This endpoint always asks your live broker, in analyzer mode too, as on the web. Nothing is placed. It needs a broker session either way.

## Errors

Validation errors on this endpoint carry `message` as a string that spells out a Python dictionary:

```json
{
  "message": "{'positions': ['Positions must contain 1 to 50 items.']}",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `{'positions': ['Positions must contain 1 to 50 items.']}` | An empty list, or more than 50 |
| 400 | `{'positions': {0: {'quantity': ['Not a valid string.']}}}` | A number where a string is expected |
| 400 | `Position 1: Invalid quantity format` | A quantity that is not a whole number |
| 400 | `Position 1: Quantity must be a positive number` | Zero or a negative quantity |
| 400 | `Position 1: Price cannot be negative` | A negative price |
| 400 | `Position 1: Symbol NOTASYMBOL not found on NSE. Check the symbol and exchange, or download the master contract again.` | A position's symbol is not in the master contract |
| 400 | The broker's reason | The broker refused the calculation |
| 404 | `Margin calculation not supported for broker: <broker>` | Nuvama, Rupeezy and HDFC Securities, which have no margin calculation on OpenAlgo web either; answered before the positions are looked up |
| 501 | `Margin calculation not implemented for broker: <broker>` | Your broker offers no margin calculation |
| 500 | `Failed to calculate margin due to internal error` | The broker did not answer |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- Positions are numbered from 0 in validation messages (`{0: ...}`) and from 1 in the others (`Position 1: ...`), as on the web.
- Hedged baskets (spreads, iron condors) get the hedge benefit only from brokers whose margin calculator recognises it.
- Figures can move with prices between the check and the order.
- Rate limit: 100 requests per second per caller. See [Rate limiting](../rate-limiting.md).

## In the desktop

- In the desktop, a basket with a position whose symbol is not in the master contract is refused as a whole, naming the position. The web's broker adapters drop that position and total the rest, so a web "success" can leave a leg out.
- In the desktop, the answer always has the three fields above. On the web, IIFL Capital also returns `margin_benefit` (always 0.0); the web docs list `margin_benefit`, but its other brokers do not return it.

---

**Back to**: [API Documentation](../README.md)
