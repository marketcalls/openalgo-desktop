# Strategy Stop

Save a stop request, exit every position the run owns at market, and finish the
run only after fills confirm it is flat.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/strategy/stop
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
curl -X POST http://127.0.0.1:5000/api/v1/strategy/stop \
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

## Sample API Response (Exit Refused)

```json
{
  "status": "error",
  "message": "1 of 2 exit order(s) were refused. The run is still open and still managed; retry the stop.",
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
      "ok": false,
      "position_ref": "80bb5fc9333f4922a582229f06a0fe45",
      "exit_owner": "live",
      "error": "Insufficient funds"
    }
  ]
}
```

HTTP 409. The run is still open and still holding leg 2.

## Sample API Response (Run Opened On Another Account)

```json
{
  "status": "error",
  "message": "This was opened on Zerodha (AB1234), but OpenAlgo is signed in to Angel One (XY9876) now, so the order was not sent. Sign in to Zerodha (AB1234) to manage it. The run remains managed.",
  "stop_pending": true,
  "exits": []
}
```

HTTP 409. Nothing was sent. In the desktop, a Live run's orders go only through
the broker and account the run was opened on; the stop is kept and retried
once you sign back in to that account. With no broker signed in at all, the
message starts `Broker session is not available or has expired. Log in to your
broker to restore it.` instead.

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| strategy_id | Strategy id, a positive integer | Mandatory | - |

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | "success" or "error" |
| message | string | Error responses only: why the stop could not finish |
| run_id | integer | Success responses only. The run the stop applies to. It stays current while `stop_pending` is true |
| stop_pending | boolean | `true` while owned exposure still needs a fill, a retry or reconciliation; `false` only when this call confirmed flatness and finished the run |
| exits | array | Per-leg exit outcome |

Each object in `exits`:

| Field | Type | Description |
|-------|------|-------------|
| leg_id | integer | The leg's id within the strategy |
| ok | boolean | Whether the exit order was accepted |
| position_ref | string or null | The exact position the exit targets |
| exit_owner | string | `live`, or `superseded` for the outgoing side of a signal flip |
| unconfirmed | boolean, optional | Desktop only. `true` when the exit was sent but the broker gave no definite answer. It is not sent again and is not counted as refused; the stop waits for it like any working exit |
| symbol | string, optional | Held symbol, included when an unfilled entry is refused before dispatch |
| broker_order_id | null, optional | Included as `null` when an unfilled entry is refused before dispatch |
| error | string or null | Rejection reason when `ok` is false |

## Status Codes

| Code | Cause |
|---|---|
| 200 | The stop was accepted. `stop_pending: true` means exit fills are still outstanding; `false` means this call confirmed flatness and finished the run |
| 400 | Schema validation failed |
| 403 | `Invalid openalgo apikey` |
| 404 | `Strategy not found` |
| 409 | `This strategy is not running`, `Run is not active`, the stop request could not be saved, or exits could not all go out (refused, an unfilled entry, no broker session, another broker account). Read `stop_pending` |
| 429 | Rate limited (100 a second) |
| 500 | `An unexpected error occurred`, or the run's own records could not be read |
| 503 | OpenAlgo is locked: sign in to the app |

## Notes

- **You never supply a run id.** The run is the strategy's `current_run_id`, so
  a stop can never reach a run that is not the current one.
- **409, not 400, when nothing is running.** The payload is fine; the state is
  not.
- Exits go out at market. The symbol comes from what the run actually holds,
  never from re-resolving the configuration: an ATM strike resolved again hours
  later can name a different contract.
- Before exiting, a stop asks the broker to cancel any entry that is still
  working and reads that order's status, so an entry that fills meanwhile is
  exited and one that never filled leaves nothing behind.
- A leg that already has an exit on its way out is not sent a second one. The
  check and the claim happen in one hold of the run's lock.
- **A leg whose entry has been accepted but not filled is not exited, and the
  stop answers 409.** Its exit reads `The entry for this leg has been accepted
  but not filled, so there is no confirmed quantity to exit. Retry once it
  fills.` Sending the configured size the other way would be a naked position
  if that entry later cancels. The run stays open and managed; retry once the
  fill lands. In the desktop a sandbox entry fills on the sandbox engine's own
  task, so a stop sent straight after a start can get this answer for a moment.
- **A stop whose exit orders were refused does not close the run.** The
  positions are still at the broker. The response lists which legs were refused
  and the run stays open and managed; retry the stop.
- A refused exit leaves the leg exitable, so a later stop, a stop loss, a target
  or the scheduler's square-off can still reach it.
- The stop request is saved before any broker call. If that write fails, the
  answer is 409 `Could not save the stop request, so no exit order was placed.
  Try again.` with `stop_pending: false`. Once saved, new signal entries are
  refused and a restart of the app resumes the same stop.
- A 200 with `stop_pending: true` means accepted exits are working. The run
  stays current, subscribed and managed. When the fills arrive, the run finishes
  and a `run_stopped` event is written; a rejected or cancelled exit writes
  `run_stop_failed` and leaves the position open for a retry.
- `run_stopped` is an event, not a field in this response. Use `stop_pending`
  here, and the run's `stopped_at` and pending-stop fields in
  [`/runs`](./runs.md).
- Finishing the run starts the [webhook](./webhook.md) cooling-off window. An
  accepted pending stop does not.
- In the desktop, an exit whose order row cannot be written is not sent (the
  web sends it anyway): it could not be found again if its answer were lost, so
  it could be sent twice. Its outcome reads `The exit could not be recorded, so
  it was not sent`, and the leg stays managed for the next attempt.
- In the desktop, a stop whose run cannot be read answers 500 with `The strategy
  records could not be read just now, so nothing was closed and no exit order
  was placed. The run is still open and managed. Try again; if it keeps
  happening, restart the app.` The web answers 409.

## Use Cases

- **End of session**: flatten a strategy from a script rather than the app
- **Risk override**: stop a strategy from an external monitor
- **Before closing the app**: square off first; a run left open is resumed when
  the app starts again

---

**Back to**: [Strategy Module API](./README.md), [API Documentation](../README.md)
