# Strategy Events

The risk-event audit trail for a strategy, newest first.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/strategy/events
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
curl -X POST http://127.0.0.1:5000/api/v1/strategy/events \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy_id": 7,
  "run_id": 42,
  "severity": "warn",
  "limit": 100
}'
```

## Sample API Response

```json
{
  "status": "success",
  "data": [
    {
      "id": 2041,
      "run_id": 42,
      "strategy_id": 7,
      "ts": "2026-08-30T06:21:40.104112+00:00",
      "kind": "leg_sl_hit",
      "severity": "warn",
      "leg_id": 1,
      "message": "stop loss hit: last price 172.8 is at or above the stop 172.35 on a short position",
      "payload": null
    },
    {
      "id": 2038,
      "run_id": 42,
      "strategy_id": 7,
      "ts": "2026-08-30T03:50:11.702884+00:00",
      "kind": "leg_entry_placed",
      "severity": "info",
      "leg_id": 1,
      "message": "Entry SELL 75 NIFTY04SEP2624500CE placed",
      "payload": null
    },
    {
      "id": 2037,
      "run_id": 42,
      "strategy_id": 7,
      "ts": "2026-08-30T03:50:11.480221+00:00",
      "kind": "run_started",
      "severity": "info",
      "leg_id": null,
      "message": "Run started in sandbox mode (manual)",
      "payload": null
    }
  ]
}
```

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| strategy_id | Strategy id, a positive integer | Mandatory | - |
| run_id | Narrow the result to one run, a positive integer | Optional | null (every run) |
| kind | Filter by event kind. Must be one of the shared event kinds below | Optional | null (no filter) |
| severity | `info`, `warn` or `critical` | Optional | null (no filter) |
| limit | How many events to return, 1 to 1000 | Optional | 500 |

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | "success" or "error" |
| data | array | Event objects, newest first by timestamp |

Each object in `data`:

| Field | Type | Description |
|-------|------|-------------|
| id | integer | Event row id |
| run_id | integer or null | Run the event belongs to. `null` for configuration events such as `strategy_created` |
| strategy_id | integer | Owning strategy |
| ts | string | ISO 8601 UTC |
| kind | string | What happened |
| severity | string | `info`, `warn` or `critical` |
| leg_id | integer or null | Leg the event concerns, when it concerns one |
| message | string | What happened, in words |
| payload | object or null | Structured detail. `null` on most events |

## Event Kinds Shared With The Web

These are the kinds the `kind` filter accepts.

Lifecycle: `strategy_created`, `strategy_updated`, `webhook_token_rotated`,
`live_enabled`, `live_disabled`, `webhook_locked`, `webhook_unlocked`,
`run_started`, `run_paused`, `run_resumed`, `run_stop_requested`,
`run_stopped`, `run_stop_failed`, `flip_outgoing_exit_rejected`,
`close_all_manual`

`run_stop_requested` says the stop is saved and new signal entries are refused;
it is not proof of flatness. `run_stopped` is the finish, written after the
engine has confirmed that no owned position remains. `run_stop_failed` is
critical whenever a pending stop could not make progress: an unfilled entry, a
refused order, a rejection or cancellation that arrived later, a missing broker
session, a broker acknowledgement not yet linked to its order, or (in the
desktop) a run opened on another broker account. The run
stays open and managed for a retry.

`flip_outgoing_exit_rejected` is critical. The old side of a signal flip is
still held under its own `position_ref`, stays managed, and can be targeted by
another exit even though the new side is also live.

In the desktop, `webhook_locked` is also written, as critical, when the webhook
locks itself after 10 alerts from addresses outside its allowlist within 10
minutes. See the [webhook page](./webhook.md).

Entry and exit: `leg_entry_placed`, `leg_entry_filled`, `leg_entry_rejected`,
`leg_exit_placed`, `leg_exit_filled`, `leg_exit_rejected`, `leg_close_manual`,
`leg_expiry_fallback`, `order_ack_unrecorded`

Manual close events describe a request only. `close_all_manual` reads `Operator
requested closure of all held legs`, and `leg_close_manual` reads `Operator
requested closure of leg <leg_id>`.

`leg_expiry_fallback` is written at `warn`, before the entry goes out, when the
chain did not list the expiry the leg asked for and a nearer one was used. A
`next_week` leg trading the current week is a different trade from the one that
was configured.

A `leg_entry_placed` or `leg_exit_placed` event at `critical` severity means the
broker reported a fill without a usable average price: the quantity is managed,
but its value and realized P&L cannot be verified.

`order_ack_unrecorded` is critical. The broker accepted (or rejected) an order,
but its answer could not be written to the order row after a retry. Its
`payload` carries `version`, `order_id`, `run_id`, `leg_id`, `broker_order_id`,
`accepted`, `status` and `reject_reason`. OpenAlgo uses these to link the row at
once where it can, then again on its periodic sweep of open runs, before a stop
and after a restart. Until it is linked, the run stays open; nothing is closed
on a guess.

Per-leg risk: `leg_sl_hit`, `leg_target_hit`, `leg_trail_armed`,
`leg_trail_advanced`

Strategy risk: `overall_sl_hit`, `overall_target_hit`, `lock_profit_armed`,
`lock_profit_floor_advanced`, `lock_profit_triggered`,
`trail_to_entry_activated`, `eod_squareoff`, `expiry_squareoff`

`leg_trail_armed` and `lock_profit_armed` are wire names kept from the web.
They mean the leg's trailing stop, or the strategy's profit lock, has become
active.

Tick source: `tick_source_switched_to_polling`, `tick_source_switched_to_ws`,
`tick_source_stale`

Operational: `recovery_succeeded`, `recovery_failed`

`recovery_failed` can mean a malformed run with no proven exposure was finished,
or that proven exposure could not be rebuilt and the run was deliberately left
open and reserved for you to reconcile by hand.

## Event Kinds Only The Desktop Writes

| Kind | Severity | Meaning |
|---|---|---|
| `order_unconfirmed` | critical | An order was sent but the broker gave no definite answer. It may have been placed, so it is not sent again; OpenAlgo checks the broker's order book and keeps the position managed meanwhile. `payload` carries `version`, `order_id`, `run_id`, `leg_id`, `client_tag`, `detail` and `status_recorded` |
| `order_unconfirmed_resolved` | info | The unconfirmed order was found in the broker's order book and is managed from there |
| `order_unconfirmed_absent` | warn | The unconfirmed order was not placed; the leg is managed again |
| `order_unconfirmed_held` | critical | A Live unconfirmed order is not in the order book, but it may still be at the broker, so nothing is sent again. Check the broker's order book and trade book; if the order is not there, mark it as not placed on the Health page to resume |
| `order_unconfirmed_ambiguous` | critical | Several orders in the broker's order book match the unconfirmed order and nothing tells them apart. Check the order book; the leg is not sent again automatically |
| `schedule_missed` | warn | A scheduled start or square-off was missed because OpenAlgo was not running then (the computer was asleep). `payload` carries `trigger_source: "scheduler"` and the job id |

The `kind` filter accepts these too; OpenAlgo web does not have them.

## Notes

- Rows are ordered by timestamp, **newest first**.
- **The trail is append-only.** Nothing updates or deletes an event row.
- **`limit` is bounded rather than clamped.** A value below 1 or above 1000 is a
  400 (`Must be greater than or equal to 1 and less than or equal to 1000.`).
- **An out-of-vocabulary `kind` or `severity` is a 400**, not an empty list.
- **A `run_id` belonging to another strategy matches nothing.**
- Configuration events share the table with runtime ones and carry
  `run_id: null`. Filtering by `run_id` therefore leaves them out.
- `payload` is free-form JSON and `null` on most events, `run_started`
  included. A scheduled **live** start refused because live trading is not
  enabled writes a `live_disabled` event at `warn` carrying
  `{"trigger_source": "scheduler", "mode": "live"}`. Do not assume one shape
  across kinds.
- In the desktop, a failed read answers 500 `An unexpected error occurred`
  rather than an empty list.

## Use Cases

- **Post-mortem**: read every `critical` event on a run that ended badly
- **Alerting**: poll for `severity: "critical"` and forward to an external
  monitor
- **Rule checking**: confirm that `lock_profit_armed` and
  `lock_profit_floor_advanced` fired where they should have

---

**Back to**: [Strategy Module API](./README.md), [API Documentation](../README.md)
