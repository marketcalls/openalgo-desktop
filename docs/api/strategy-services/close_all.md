# Strategy Close All

Record that you asked to close everything, save the stop, exit every position
the run owns at market, and finish the run only after flatness is confirmed.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/strategy/close_all
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "strategy_id": 7
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/strategy/close_all \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy_id": 7
}'
```

## Sample API Response

```json
{
  "status": "success",
  "run_id": 42,
  "stop_pending": true,
  "exits": [
    {
      "leg_id": 1,
      "ok": true,
      "position_ref": "969bc536b1c14d15992f730c2c136d7a",
      "exit_owner": "live",
      "error": null
    },
    {
      "leg_id": 2,
      "ok": true,
      "position_ref": "80bb5fc9333f4922a582229f06a0fe45",
      "exit_owner": "live",
      "error": null
    }
  ]
}
```

## Sample API Response (Not Running)

```json
{
  "status": "error",
  "message": "This strategy is not running"
}
```

HTTP 409.

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| strategy_id | Strategy id, a positive integer | Mandatory | - |

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | "success" or "error" |
| message | string | Error responses only: why the close could not finish |
| run_id | integer | Success responses only. The run the close applies to |
| stop_pending | boolean | `true` while owned exposure still needs a fill, a retry or reconciliation; `false` only after this call confirmed flatness |
| exits | array | Per-leg exit outcome |

Each object in `exits`:

| Field | Type | Description |
|-------|------|-------------|
| leg_id | integer | The leg's id within the strategy |
| ok | boolean | Whether the exit order was accepted |
| position_ref | string or null | The exact position the exit targets |
| exit_owner | string | `live`, or `superseded` for the outgoing side of a signal flip |
| unconfirmed | boolean, optional | Desktop only. `true` when the exit was sent but the broker gave no definite answer; it is not sent again |
| symbol | string, optional | Held symbol, included when an unfilled entry is refused before dispatch |
| broker_order_id | null, optional | Included as `null` when an unfilled entry is refused before dispatch |
| error | string or null | Rejection reason when `ok` is false |

## Status Codes

| Code | Cause |
|---|---|
| 200 | The close was accepted. Read `stop_pending`; accepted working exits are not proof of flatness |
| 400 | Schema validation failed |
| 403 | `Invalid openalgo apikey` |
| 404 | `Strategy not found` |
| 409 | `This strategy is not running`, or the stop could not finish (refused exits, an unfilled entry, no broker session, another broker account) |
| 429 | Rate limited (100 a second) |
| 500 | `An unexpected error occurred`, or the run's own records could not be read |
| 503 | OpenAlgo is locked: sign in to the app |

## Notes

- **A close whose exits the broker refused leaves the run open.** Same rule as
  [`/stop`](./stop.md): the positions are still there. Read the per-leg `ok`
  flags and retry rather than treating a 2xx answer as flat.
- **Same stop mechanics as [`/stop`](./stop.md), different audit record.** A
  `close_all_manual` event with the message `Operator requested closure of all
  held legs` is written before the stop is saved and the exits are sent. It
  proves the close was asked for, not that the broker became flat;
  `run_stopped` is that evidence.
- The run id comes from the strategy, never from the caller.
- A Live run opened on another broker account is not closed through the account
  signed in now: the answer is 409 with `stop_pending: true` and the message
  shown on [`/stop`](./stop.md#sample-api-response-run-opened-on-another-account).
- The `close_all_manual` event can be read through [`/events`](./events.md) with
  `"kind": "close_all_manual"`.

## Use Cases

- **Operator intent**: record that a person asked for a flatten, separately from
  the later confirmed-flat finish
- **Kill-switch scripting**: pair with the kill switch on the strategy's page for
  an auditable manual flatten
- **Incident response**: close a strategy and be able to show afterwards when it
  was asked for

---

**Back to**: [Strategy Module API](./README.md), [API Documentation](../README.md)
