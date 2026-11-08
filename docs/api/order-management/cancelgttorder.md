# CancelGTTOrder

Cancel an active GTT by its `trigger_id`. Cancelling an OCO cancels both legs; on Angel One they are two rules cancelled one after the other.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/cancelgttorder
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "strategy": "My GTT Strategy",
  "trigger_id": "23132604291205"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/cancelgttorder \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy": "My GTT Strategy",
  "trigger_id": "23132604291205"
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

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| strategy | Strategy name, used in the logs | Mandatory | - |
| trigger_id | The id [PlaceGTTOrder](./placegttorder.md) returned | Mandatory | - |

Any other field is refused as `Unknown field.`

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | `success` or `error` |
| trigger_id | string | The cancelled GTT's id |
| mode | string | `analyze` in analyzer mode; absent in live mode |
| message | string | The reason, on an error |

## Analyzer Mode

With analyzer mode on, the sandbox GTT is cancelled, its reserved margin goes back to your sandbox funds, and the answer carries `"mode": "analyze"`. Only an `active` GTT can be cancelled: one that fired, was cancelled or lapsed answers 404. See [Sandbox mode](../../user/sandbox.md).

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
| 400 | `{'trigger_id': ['Shorter than minimum length 1.']}` | An empty `trigger_id` |
| 400 | The broker's own reason | The broker refused, usually because the GTT is no longer active |
| 403 | `Cancel GTT order is not allowed in Semi-Auto mode. Switch to Auto mode.` | The API key's order mode is Semi-Auto (live mode only) |
| 404 | The broker's reason | The broker reports the GTT as not found |
| 501 | `GTT orders are not supported for broker 'shoonya' yet` | Your broker has no GTT in OpenAlgo |
| 500 | `Failed to cancel GTT due to internal error` | The broker call failed without a readable answer |

In analyzer mode the sandbox adds its own refusals, with `"mode": "analyze"`:

| HTTP | `message` | When |
|------|-----------|------|
| 404 | `No active GTT with trigger_id 'GTT-261003-12a093ec'` | The id is unknown, or the GTT already fired, was cancelled or lapsed |

```json
{
  "message": "No active GTT with trigger_id 'GTT-261003-12a093ec'",
  "mode": "analyze",
  "status": "error"
}
```

Errors common to every endpoint (missing or invalid API key, OpenAlgo locked, wrong method, rate limit) are listed in the [API overview](../README.md#errors-common-to-every-endpoint).

## Notes

- Only active GTTs can be cancelled.
- An OCO is cancelled as a whole; there is no cancel for one leg. On Angel One its two rules are cancelled one after the other, and if the second cannot be cancelled the error names the rule already cancelled: check the [GTT order book](./gttorderbook.md) before trying again. See [Angel One](./placegttorder.md#angel-one-an-oco-is-two-separate-rules).
- A cancelled GTT leaves the default [GTT order book](./gttorderbook.md), which lists active GTTs only. Send `"status": "all"` there to see it as `cancelled`.
- Cancelling a GTT that is already cancelled gets your broker's own answer, a success or an error such as "Trigger not found", depending on the broker. In analyzer mode it is always the 404 above.
- Semi-Auto does not queue a cancel; it refuses it (403) in live mode, as on the web.
- Rate limit: 10 requests per second per caller, shared with the other order endpoints. See [Rate limiting](../rate-limiting.md).

## In the desktop

- In the desktop, an analyzer-mode OCO that was modified to a higher target, with positions closed in between, cancels normally and releases its margin; the web can answer 500 `Could not release ... margin ... retry the cancel.` and leave the OCO active, because its sandbox had already dropped the margin of resting orders (a web defect).

---

**Back to**: [API Documentation](../README.md)
