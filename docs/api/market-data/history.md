# History

Get historical OHLCV candles (open, high, low, close, volume and open
interest) for a symbol, from your broker or from the local Historify
database.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/history
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "symbol": "RELIANCE",
  "exchange": "NSE",
  "interval": "5m",
  "start_date": "2026-09-29",
  "end_date": "2026-10-02"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/history \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "symbol": "RELIANCE",
  "exchange": "NSE",
  "interval": "5m",
  "start_date": "2026-09-29",
  "end_date": "2026-10-02"
}'
```

## Sample API Response

Trimmed to the first two candles.

```json
{
  "status": "success",
  "data": [
    {
      "close": 1194.5,
      "high": 1198.3,
      "low": 1192.4,
      "oi": 0,
      "open": 1193.8,
      "timestamp": 1790653500,
      "volume": 556592
    },
    {
      "close": 1194.2,
      "high": 1195.9,
      "low": 1193.6,
      "oi": 0,
      "open": 1195.0,
      "timestamp": 1790653800,
      "volume": 246405
    }
  ]
}
```

`1790653500` is 2026-09-29 09:15:00 IST, the start of the first five-minute
candle.

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| symbol | Symbol in OpenAlgo format | Mandatory | - |
| exchange | One of NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX, CRYPTO (upper case) | Mandatory | - |
| interval | One of the intervals below | Mandatory | - |
| start_date | First day, `YYYY-MM-DD` | Mandatory | - |
| end_date | Last day, `YYYY-MM-DD` (included) | Mandatory | - |
| source | `api` (your broker) or `db` (the local Historify database) | Optional | `api` |

## Intervals

The request accepts any of these values:

`1s`, `5s`, `10s`, `15s`, `30s`, `45s`, `1m`, `2m`, `3m`, `5m`, `10m`, `15m`,
`20m`, `30m`, `1h`, `2h`, `3h`, `4h`, `D`, `W`, `M`, `Q`, `Y`

Your broker offers only some of them. [Intervals](./intervals.md) lists the
ones it offers; with `source: "api"` an interval it does not offer is refused
by the broker. A value outside the list, such as `7m`, is refused with 400
before anything is asked of the broker.

The custom chart intervals of the `/trading` terminal and Historify (for
example `7m` or `2D`) are built inside the app from your broker's standard
intervals. They are not accepted here.

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | "success" or "error" |
| data | array | Candles, oldest first |

### Data Array Fields

| Field | Type | Description |
|-------|------|-------------|
| timestamp | number | Candle start as Unix epoch **seconds** |
| open | number | Opening price |
| high | number | Highest price |
| low | number | Lowest price |
| close | number | Closing price |
| volume | number | Volume traded |
| oi | number | Open interest; 0 when there is none |

## Timestamps

- `timestamp` is an integer number of seconds since 1970-01-01 UTC. Convert it
  to IST (or any zone) in your client; do not read the number as IST.
- Intraday candles are stamped at the start of the bar: 09:15 IST is
  03:45 UTC.
- Daily candles are stamped at 00:00 UTC of their date (05:30 IST).

## Reading from Historify (`source: "db"`)

```json
{
  "apikey": "<APIKEY>",
  "symbol": "RELIANCE",
  "exchange": "NSE",
  "interval": "D",
  "start_date": "2026-09-01",
  "end_date": "2026-10-02",
  "source": "db"
}
```

- The candles come from data you downloaded or imported in Historify. Nothing
  is asked of the broker.
- Historify stores 1-minute and daily candles. Minute and hour intervals are
  built from the 1-minute candles; `W`, `M`, `Q` and `Y` from the daily ones.
- With no stored data in the range the answer is 404:
  `No data found for RELIANCE:NSE interval D in local database. Download data first using Historify.`
- A `db` request needs a valid API key, and no broker session: the data is
  on this computer. OpenAlgo web reads the local database without checking
  the key at all; the desktop checks it.

## Errors

| HTTP | message | Cause and fix |
|------|---------|---------------|
| 400 | `{"interval": ["Must be one of: 1s, 5s, ... Q, Y."]}` | The interval is not in the list above. |
| 400 | `{"start_date": ["Not a valid date."], ...}` | A date is not `YYYY-MM-DD`. |
| 400 | `Symbol 'XYZ' not found for exchange 'NSE'. ...` | The symbol is not in the master contract. |
| 403 | `Invalid openalgo apikey` | The key is wrong, or (with `source: "api"`) no broker is logged in. |
| 404 | `No data found for ... in local database. Download data first using Historify.` | `source: "db"` and nothing stored for that range. |
| 500 | The broker's own message | The broker refused the request (for example an interval it does not offer) or could not be reached. |
| 500 | `Could not read the local Historify database. Try again.` | `source: "db"` and the database could not be read. |

## Notes

- An `end_date` before `start_date` is not an error: the answer is a success
  with an empty `data` list, as on OpenAlgo web.
- How far back data goes depends on your broker, especially for intraday
  intervals. Long ranges are fetched from the broker in pieces of the size it
  allows.
- If OpenAlgo is pacing requests to stay within your broker's rate limit and
  this one would have waited more than about ten seconds, it is refused with
  429 and "OpenAlgo is pacing requests ... Try again in a few seconds.", as
  on OpenAlgo web.
- The web's own documentation shows `timestamp` as a date string. Both OpenAlgo
  web and the desktop actually send epoch seconds, as shown here.
- History always comes from your broker (or Historify), also in analyzer mode.

## Example: Daily Data

```json
{
  "apikey": "<APIKEY>",
  "symbol": "NIFTY",
  "exchange": "NSE_INDEX",
  "interval": "D",
  "start_date": "2026-09-01",
  "end_date": "2026-10-02"
}
```

```json
{
  "status": "success",
  "data": [
    {"close": 24055.8, "high": 24143.15, "low": 23952.55, "oi": 0, "open": 24077.55, "timestamp": 1788220800, "volume": 0},
    {"close": 23914.45, "high": 23914.45, "low": 23786.8, "oi": 0, "open": 23858.0, "timestamp": 1788307200, "volume": 0}
  ]
}
```

## Related Endpoints

- [Intervals](./intervals.md) - the intervals your broker offers
- [Ticker](./ticker.md) - the same candles over GET, as JSON or plain text

---

**Back to**: [API Documentation](../README.md)
