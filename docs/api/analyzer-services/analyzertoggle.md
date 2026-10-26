# AnalyzerToggle

Turn analyzer mode on or off. With it on, orders go to the sandbox instead of your broker.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/analyzer/toggle
```

## Sample API Request (Turn Analyzer Mode On)

```json
{
  "apikey": "<APIKEY>",
  "mode": true
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/analyzer/toggle \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "mode": true
}'
```

## Sample API Response (On)

```json
{
  "data": {
    "analyze_mode": true,
    "message": "Analyzer mode switched to analyze",
    "mode": "analyze",
    "total_logs": 83
  },
  "status": "success"
}
```

## Sample API Request (Turn Analyzer Mode Off)

```json
{
  "apikey": "<APIKEY>",
  "mode": false
}
```

## Sample API Response (Off)

```json
{
  "data": {
    "analyze_mode": false,
    "message": "Analyzer mode switched to live",
    "mode": "live",
    "total_logs": 83
  },
  "status": "success"
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| mode | `true` turns analyzer mode on, `false` turns it off. Also accepted: `"true"`/`"false"`, `1`/`0`, `"yes"`/`"no"`, `"on"`/`"off"` | Mandatory | - |

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| data | object | The result |

### Data Object Fields

| Field | Type | Description |
|-------|------|-------------|
| analyze_mode | boolean | The mode now in force: `true` for analyzer mode |
| message | string | `Analyzer mode switched to analyze` or `Analyzer mode switched to live` |
| mode | string | `analyze` or `live` |
| total_logs | number | Requests recorded in the sandbox log |

Setting the mode it already has is not an error: the answer is the same.

## Analyzer Mode

- Turning it on starts the sandbox engine. It first catches up on what fell due while it was off (MIS positions from an earlier session, expired contracts, T+1 settlement of CNC buys, GTT triggers), then fills resting orders from live prices.
- Turning it off stops the sandbox engine. Sandbox orders, positions and funds are kept for the next time.
- The app's top bar, and every open page, switch to the new mode at once.
- Strategy module runs keep the mode they were started in: a Live run keeps trading live and a Sandbox run keeps using the sandbox.

See [Sandbox mode](../../user/sandbox.md).

## Errors

Validation errors on this endpoint carry `message` as a string that spells out a Python dictionary, with no `mode` key:

```json
{
  "message": "{'mode': ['Not a valid boolean.']}",
  "status": "error"
}
```

| HTTP | `message` | When |
|------|-----------|------|
| 400 | `{'mode': ['Missing data for required field.']}` | No `mode` |
| 400 | `{'mode': ['Not a valid boolean.']}` | A `mode` that is not a true/false value |
| 403 | `Invalid openalgo apikey` | The key is wrong, or no broker is logged in |
| 403 | `Operation analyzer/toggle is not allowed in Semi-Auto mode. This operation can only be performed by the client via the UI. This restriction ensures SEBI Research Analyst compliance where mode switching is a client-only decision.` | Orders wait for approval in the Action Center (Semi-Auto): switch the mode in the app instead |
| 500 | `An unexpected error occurred` | The setting could not be saved |

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- **Turning analyzer mode off means the next orders use real money.** Check the mode before you start anything automated.
- The mode is one setting for the whole app, not one per API key.
- This endpoint needs a live broker session, as on the web.
- A switch sent while another is in progress waits for it to finish, then applies.
- Use [AnalyzerStatus](./analyzerstatus.md) to read the mode.
- Rate limit: 100 requests per second per caller. See [Rate limiting](../rate-limiting.md).

## In the desktop

- In the desktop, a switch sent while another is still being applied waits for it. On a multi-threaded server the web answers 409 `Another mode change is still in progress. Try again in a moment.` instead.
- In Semi-Auto the switch is refused with 403, as the web's code intends: switching between live and sandbox is the account holder's own decision, made in the app. The web's check looks the order mode up by the API key instead of the account, so in practice the web does not refuse it; the desktop does. If the order mode cannot be read, nothing is switched and the answer is 500.

---

**Back to**: [API Documentation](../README.md)
