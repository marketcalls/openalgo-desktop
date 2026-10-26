# Strategy Close Leg

Exit one leg of the current run at market. The run continues with the rest.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/strategy/close_leg
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "strategy_id": 7,
  "leg_id": 2
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/strategy/close_leg \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy_id": 7,
  "leg_id": 2
}'
```

## Sample API Response

```json
{
  "status": "success",
  "run_id": 42,
  "leg_id": 2,
  "run_stopped": false,
  "exits": [
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

## Sample API Response (Leg Not Open)

```json
{
  "status": "error",
  "message": "That leg is not open"
}
```

HTTP 409.

## Sample API Response (Entry Not Filled Yet)

```json
{
  "status": "error",
  "message": "Exit refused: The entry for this leg has been accepted but not filled, so there is no confirmed quantity to exit. Retry once it fills."
}
```

HTTP 409. Retry once the entry fills. In the desktop a sandbox entry fills on
the sandbox engine's own task, so a close sent straight after a start can get
this answer for a moment.

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| strategy_id | Strategy id, a positive integer | Mandatory | - |
| leg_id | The leg's id within the strategy, a positive integer | Mandatory | - |

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | "success" or "error" |
| run_id | integer | The run the leg belongs to |
| leg_id | integer | The leg that was closed, echoed back |
| run_stopped | boolean | `true` only when this call saw the last position fill and the run finished. An accepted exit normally returns `false`; its fill finishes the run later |
| exits | array | Exit outcome for the leg |

Each object in `exits`:

| Field | Type | Description |
|-------|------|-------------|
| leg_id | integer | The leg's id within the strategy |
| ok | boolean | Whether the exit order was accepted |
| position_ref | string or null | The exact position the exit targets |
| exit_owner | string | `live`, or `superseded` for the outgoing side of a signal flip |
| error | string or null | Rejection reason when `ok` is false |

An error response carries `status` and `message` only.

## Status Codes

| Code | Cause |
|---|---|
| 200 | The exit was accepted |
| 400 | Schema validation failed, including a missing `leg_id` |
| 403 | `Invalid openalgo apikey` |
| 404 | `Strategy not found` |
| 409 | `This strategy is not running`, `Run is not active`, `That leg is not open`, the entry is not filled yet, the broker refused the exit, the broker did not confirm the exit, or the run belongs to another broker account or no broker is signed in |
| 429 | Rate limited (100 a second) |
| 500 | `An unexpected error occurred`, or the run's own records could not be read |
| 503 | OpenAlgo is locked: sign in to the app |

## Notes

- **Leg ids are the ids the strategy page assigned**, the same values as
  `legs[].id` on [`/status`](./status.md). They are not order ids and not run
  ids.
- **The run comes from the strategy**, so you never supply a run id.
- `run_stopped` reports what this call could prove. A live broker normally
  accepts an order before it fills, so the last accepted exit returns `false`
  and its fill finishes the run later.
- This endpoint has no whole-run pending-stop field. It closes one position
  without saving a stop for the whole run.
- The accepted close writes a `leg_close_manual` event, `Operator requested
  closure of leg <leg_id>`. It does not say the leg closed; the later fill does.
- **A refused exit is a failure, not a close.** The answer is 409 `Exit refused:
  <the broker's reason>`. The leg stays exitable, so its stop loss, its target
  and the scheduler's square-off can still reach it.
- In the desktop, an exit sent without a definite answer from the broker answers
  409 `The exit was sent, but the broker did not confirm it. It may have been
  placed, so it is not sent again; check the order book. The leg stays managed
  until the broker's order book settles it.` Do not retry it blindly; check the
  order book.
- In the desktop, a Live run's exit goes only through the broker and account the
  run was opened on. Signed in to another account, the answer is 409 with
  `This was opened on Zerodha (AB1234), but OpenAlgo is signed in to Angel One
  (XY9876) now, so the order was not sent. Sign in to Zerodha (AB1234) to manage
  it.`; with no broker signed in, `Broker session is not available or has
  expired. Log in to your broker to restore it.`
- **Closing a leg by hand does not trigger trail-to-entry.** An operator closing
  a leg is an override, not a market signal.
- A `leg_id` that names no open leg is a 409, not a 404. The strategy was found;
  the leg is not in a state that can be closed.
- The exit uses the symbol the run actually holds, not a fresh resolution of the
  configuration.

## Use Cases

- **Legging out**: close the losing side of a spread and let the winner run
- **Manual repair**: remove one leg whose contract has become illiquid without
  flattening the strategy
- **Risk trimming**: reduce exposure without giving up the position entirely

---

**Back to**: [Strategy Module API](./README.md), [API Documentation](../README.md)
