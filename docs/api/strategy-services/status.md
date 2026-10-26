# Strategy Status

One strategy: its full configuration including legs, and its current run if it
has one.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/strategy/status
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
curl -X POST http://127.0.0.1:5000/api/v1/strategy/status \
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
  "data": {
    "id": 7,
    "name": "NIFTY Short Straddle",
    "strategy_kind": "batch",
    "direction": "both",
    "universe_tab": "weekly_monthly",
    "underlying": "NIFTY",
    "underlying_exchange": "NSE_INDEX",
    "strategy_type": "intraday",
    "entry_time": "09:20",
    "exit_time": "15:10",
    "product": "NRML",
    "pricetype": "MARKET",
    "overall_sl_mtm": -5000.0,
    "overall_target_mtm": 8000.0,
    "lock_profit": null,
    "trail_sl_to_entry": false,
    "scheduler": null,
    "live_enabled": false,
    "webhook_locked": false,
    "webhook_ip_allowlist": null,
    "daily_loss_limit_inr": 10000.0,
    "status": "running",
    "current_run_id": 42,
    "created_at": "2026-08-24T04:11:52.104883+00:00",
    "updated_at": "2026-08-30T03:50:11.482913+00:00",
    "legs": [
      {
        "id": 1,
        "segment": "options",
        "position": "S",
        "lots": 1,
        "option_type": "CE",
        "strike_mode": "atm",
        "atm_offset": "ATM",
        "expiry": "weekly",
        "risk_unit": "points",
        "sl_pts": 30,
        "target_pts": 60,
        "trail": {"x": 10, "y": 5}
      },
      {
        "id": 2,
        "segment": "options",
        "position": "S",
        "lots": 1,
        "option_type": "PE",
        "strike_mode": "atm",
        "atm_offset": "ATM",
        "expiry": "weekly",
        "risk_unit": "points",
        "sl_pts": 30,
        "target_pts": 60,
        "trail": {"x": 10, "y": 5}
      }
    ]
  },
  "run": {
    "id": 42,
    "strategy_id": 7,
    "mode": "sandbox",
    "broker": "sandbox",
    "started_at": "2026-08-30T03:50:11.402118+00:00",
    "stopped_at": null,
    "stop_reason": null,
    "stop_requested_at": null,
    "stop_requested_reason": null,
    "pnl_realized": 0.0,
    "pnl_peak": 0.0,
    "pnl_trough": 0.0,
    "trigger_source": "manual",
    "webhook_event_id": null,
    "resolved_expiries": {
      "1": "04-SEP-26",
      "2": "04-SEP-26"
    }
  }
}
```

## Sample API Response (Stopped Strategy)

```json
{
  "status": "success",
  "data": {
    "id": 7,
    "name": "NIFTY Short Straddle",
    "status": "stopped",
    "current_run_id": null,
    "legs": []
  },
  "run": null
}
```

The `data` object above is shortened for reading. A real response always
carries every field listed below.

## Sample API Response (Invalid Strategy Id)

```json
{
  "status": "error",
  "message": {
    "strategy_id": ["Not a valid integer."]
  }
}
```

HTTP 400.

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| strategy_id | Strategy id, a positive integer | Mandatory | - |

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | "success" or "error" |
| data | object | The strategy's full configuration, including `legs` |
| run | object or null | The current run, or `null` when the strategy is not running |

`data` carries the same fields as a [list](./list.md) row, with one omission and
one addition. It does **not** carry `last_finalized_run`; read the newest row
from [`/runs`](./runs.md) instead. It adds:

| Field | Type | Description |
|-------|------|-------------|
| legs | array | The leg configuration as the strategy page saved it |

Each leg carries `risk_unit`, `points` (the default) or `percent`, which governs
that leg's `sl_pts`, `target_pts` and `trail` together. A percentage is measured
against the leg's own entry price, so 2 on a short filled at 2500 is a stop at
2550.

`run`:

| Field | Type | Description |
|-------|------|-------------|
| id | integer | Run id |
| strategy_id | integer | Owning strategy |
| mode | string | `live` or `sandbox`, fixed for the life of the run |
| broker | string | Broker the run is bound to, recorded at start. `sandbox` for a sandbox run |
| started_at | string | ISO 8601 UTC |
| stopped_at | string or null | ISO 8601 UTC, `null` while the run is open |
| stop_reason | string or null | One of the stop reasons, `null` while the run is open |
| stop_requested_at | string or null | ISO 8601 UTC, set while a saved stop is pending; cleared when the run finishes |
| stop_requested_reason | string or null | Reason for the pending stop. While set, new signal entries are refused and a restart resumes the stop |
| pnl_realized | number | Realized P&L, written when the run is confirmed flat |
| pnl_peak | number | Highest P&L the run reached |
| pnl_trough | number | Lowest P&L the run reached |
| trigger_source | string | `manual`, `webhook` or `scheduler` |
| webhook_event_id | integer or null | The webhook audit row that started this run, when a webhook did |
| resolved_expiries | object or null | Leg id to the expiry resolved at start, as strings |

## Notes

- `run` is `null` whenever the strategy has no `current_run_id`, which is the
  normal state of a stopped strategy.
- If the current run's record cannot be read at that moment, the answer is
  500 `An unexpected error occurred`, as on OpenAlgo web, never `run: null`.
  Ask again; do not read it as stopped.
- While a run is open, `pnl_realized`, `pnl_peak` and `pnl_trough` are the
  values last written to the run row, not a live mark. The live figures are on
  the strategy's page in the app.
- Once the strategy is stopped, the newest finished row from
  [`/runs`](./runs.md) is the authority for realized P&L, peak, trough, stop
  time and stop reason.
- A set `stop_requested_reason` means the stop is saved but not yet confirmed
  flat. The run stays current and managed until `stopped_at` is set or `run`
  becomes `null`.
- The run records the broker at start, and in the desktop also the account at
  that broker (not shown in this response). A Live run sends orders only
  through that broker and account. See [Where A Strategy's Orders
  Go](./README.md#where-a-strategys-orders-go).
- Expiries are resolved once at run start and held for the run, so a
  positional strategy does not roll to a new contract mid-run.
  `resolved_expiries` is that snapshot.
- An unknown `strategy_id` returns 404 `Strategy not found`.
- `data` never carries the webhook token or its digest.

## Use Cases

- **Monitoring**: poll one strategy for its mode, broker and current run
- **Pre-flight**: confirm `live_enabled` before asking for a live start
- **Reconciliation**: read `resolved_expiries` to know exactly which contracts a
  run holds

---

**Back to**: [Strategy Module API](./README.md), [API Documentation](../README.md)
