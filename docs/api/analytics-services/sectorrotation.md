# Sector Rotation

Place sectors, stocks and indices on a relative rotation map against a benchmark, from their daily closes: which are leading, weakening, lagging or improving. This is an analysis endpoint; it places no orders. The **Sector Rotation** tool (`/sector-rotation`, under **Tools**) draws the same map.

| Method | Path | Authentication |
|---|---|---|
| GET | `/sectorrotation/options` | None |
| POST | `/sectorrotation` | `apikey` in the JSON body |

## Endpoint URL

```http
GET  http://127.0.0.1:5000/api/v1/sectorrotation/options
POST http://127.0.0.1:5000/api/v1/sectorrotation
```

## Options

`GET /sectorrotation/options` lists what a map can be built from. It needs no API key.

```json
{
  "status": "success",
  "data": {
    "benchmarks": [
      {"symbol": "NIFTY", "exchange": "NSE_INDEX", "label": "Nifty 50"},
      {"symbol": "SENSEX", "exchange": "BSE_INDEX", "label": "Sensex"}
    ],
    "timeframes": ["daily", "weekly"],
    "exchanges": ["NSE", "BSE", "NSE_INDEX", "BSE_INDEX", "MCX_INDEX", "GLOBAL_INDEX"],
    "max_instruments": 40,
    "sectors": [
      {"symbol": "NIFTYAUTO", "exchange": "NSE_INDEX", "label": "Auto"},
      {"symbol": "NIFTYIT", "exchange": "NSE_INDEX", "label": "IT"}
    ],
    "optional_sectors": [
      {"symbol": "NIFTYHEALTHCARE", "exchange": "NSE_INDEX", "label": "Healthcare"}
    ],
    "max_tail": 30
  }
}
```

The lists are shortened here. `sectors` is the default map (14 NSE sector indices); `optional_sectors` are sector indices many brokers do not publish, left off the default map.

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "benchmark": "NIFTY",
  "timeframe": "daily",
  "source": "db",
  "instruments": [
    {"symbol": "NIFTYIT", "exchange": "NSE_INDEX", "label": "IT"},
    {"symbol": "RELIANCE", "exchange": "NSE"}
  ]
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/sectorrotation \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "benchmark": "NIFTY",
  "timeframe": "daily",
  "source": "db"
}'
```

## Sample API Response

```json
{
  "status": "success",
  "data": {
    "benchmark": "NIFTY",
    "benchmark_exchange": "NSE_INDEX",
    "benchmark_label": "Nifty 50",
    "timeframe": "daily",
    "source": "db",
    "as_of": "2026-11-06",
    "max_tail": 30,
    "instruments": [
      {
        "symbol": "NIFTYIT",
        "exchange": "NSE_INDEX",
        "label": "IT",
        "quadrant": "improving",
        "rs_ratio": 98.42,
        "rs_momentum": 101.17,
        "bars_in_quadrant": 4,
        "tail": [
          {"date": "2026-11-05", "rs_ratio": 98.1, "rs_momentum": 100.86},
          {"date": "2026-11-06", "rs_ratio": 98.42, "rs_momentum": 101.17}
        ]
      }
    ],
    "unavailable": [
      {
        "symbol": "RELIANCE",
        "exchange": "NSE",
        "label": "RELIANCE",
        "reason": "Historify has no daily history for this symbol. Download it in Historify, or switch the data source to Broker API."
      }
    ]
  }
}
```

The numbers are illustrative and the trail is shortened.

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|--------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| benchmark | `NIFTY`, `NIFTY500`, `BANKNIFTY`, `SENSEX`, `NIFTYNXT50`, `MIDCPNIFTY` or `FINNIFTY`. The benchmark decides its own exchange | Optional | NIFTY |
| timeframe | `daily`, or `weekly` (built from the daily closes) | Optional | daily |
| source | `db` (Historify on this computer) or `api` (your broker's history) | Optional | db |
| instruments | Up to 40 symbols; a symbol listed twice is placed once. Empty or left out: the default NSE sector map | Optional | [] |
| instruments[].symbol | OpenAlgo symbol, 1 to 64 characters | Mandatory per item | - |
| instruments[].exchange | `NSE`, `BSE`, `NSE_INDEX`, `BSE_INDEX`, `MCX_INDEX` or `GLOBAL_INDEX`. Futures and options are not accepted: a contract expires and rolls, so its closes are not one series | Mandatory per item | - |
| instruments[].label | A name to show for the symbol, up to 64 characters | Optional | the sector name for a known sector index, else the symbol |

The API key is checked on both sources: Historify needs no broker, but the history is still yours. `source: "api"` also needs a broker logged in.

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| benchmark, benchmark_exchange, benchmark_label | string | The benchmark the map is measured against |
| timeframe, source | string | As requested |
| as_of | string | The latest date on any trail (`YYYY-MM-DD`) |
| max_tail | number | The longest trail returned (30 points) |
| instruments | array | Each symbol that could be placed (below) |
| unavailable | array | Each symbol that could not be placed: `symbol`, `exchange`, `label` and the `reason` |

Each entry of `instruments`:

| Field | Type | Description |
|-------|------|-------------|
| symbol, exchange, label | string | The symbol |
| rs_ratio | number | Relative strength against the benchmark, centred on 100 |
| rs_momentum | number | The momentum of that relative strength, centred on 100 |
| quadrant | string | `leading` (both at or above 100), `weakening` (ratio at or above 100, momentum below), `lagging` (both below), `improving` (ratio below 100, momentum at or above) |
| bars_in_quadrant | number | How many bars, up to the latest, the symbol has been in that quadrant |
| tail | array | The trail, oldest first, up to 30 points of `date`, `rs_ratio` and `rs_momentum` |

The map is a comparison of trends against the benchmark, not a forecast and not a trading signal.

## Errors

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `These map settings were not accepted. Choose the benchmark and timeframe again, or use Reset to sectors if you added symbols.` | A field the schema refuses: an unknown benchmark, timeframe, source or exchange, more than 40 symbols, an empty symbol (the field detail goes to the log) |
| 403 | `Invalid openalgo apikey` | The API key is wrong |
| 403 | `Broker API needs an active broker login. Log in to your broker, or switch the data source to Historify.` | `source: "api"` with no broker logged in |
| 422 | `Historify has no daily history for Nifty 50. Download it in Historify, or switch the data source to Broker API.` | The benchmark's history could not be read (with `source: "api"`, the sentence names your broker instead) |
| 422 | `Nothing could be placed on the map. ...` | No symbol could be placed; the first reason follows |

A map with some symbols missing is still a 200: read `unavailable` and each reason. Errors common to every endpoint (OpenAlgo locked, wrong method, rate limit) are in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- Daily maps read 365 calendar days of closes, weekly maps 730. A symbol needs 44 sessions (daily) or 20 weeks (weekly) that it and the benchmark both traded before it is placed.
- Each symbol is read on its own, so one index your broker does not publish costs that symbol and not the map.
- The closes read for a map are kept for five minutes, so building the same map again within that time reads nothing new.
- Rate limit: 30 requests per minute per caller, for each of the two routes. See [Rate limiting](../rate-limiting.md).
- The Python SDK has no method for this endpoint; call it with any HTTP client.

---

**Back to**: [API Documentation](../README.md)
