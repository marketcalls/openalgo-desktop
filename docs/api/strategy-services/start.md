# Strategy Start

Start a run of a batch strategy in the mode you ask for. Every leg's entry order
is placed.

## Endpoint URL

```http
POST http://127.0.0.1:5000/api/v1/strategy/start
```

## Sample API Request

```json
{
  "apikey": "<APIKEY>",
  "strategy_id": 7,
  "mode": "sandbox"
}
```

## Sample cURL Request

```bash
curl -X POST http://127.0.0.1:5000/api/v1/strategy/start \
  -H 'Content-Type: application/json' \
  -d '{
  "apikey": "<APIKEY>",
  "strategy_id": 7,
  "mode": "sandbox"
}'
```

## Sample API Response

```json
{
  "status": "success",
  "run_id": 42,
  "mode": "sandbox",
  "legs": [
    {
      "leg_id": 1,
      "ok": true,
      "unconfirmed": false,
      "acknowledged": true,
      "symbol": "NIFTY04SEP2624500CE",
      "broker_order_id": "26083004118201",
      "error": null
    },
    {
      "leg_id": 2,
      "ok": true,
      "unconfirmed": false,
      "acknowledged": true,
      "symbol": "NIFTY04SEP2624500PE",
      "broker_order_id": "26083004118244",
      "error": null
    }
  ]
}
```

In the desktop, each leg also carries `unconfirmed`. A client written for the
web ignores the extra field and reads the rest unchanged.

## Sample API Response (Broker Accepted, Acknowledgement Not Recorded)

```json
{
  "status": "success",
  "run_id": 42,
  "mode": "live",
  "legs": [
    {
      "leg_id": 1,
      "ok": true,
      "unconfirmed": false,
      "acknowledged": false,
      "symbol": "NIFTY04SEP2624500CE",
      "broker_order_id": "26083004118201",
      "error": null
    }
  ]
}
```

This is a real broker order, not a rejection. The saved order intent still
exists and a critical `order_ack_unrecorded` event carries the exact broker
order id, row, run and leg. OpenAlgo binds the two at once where it can and
keeps retrying in the background; until then the run stays open and the leg
stays managed. The entry is never placed a second time.

## Sample API Request (Live)

A live start is refused unless live trading is enabled on this strategy.

```json
{
  "apikey": "<APIKEY>",
  "strategy_id": 7,
  "mode": "live"
}
```

## Sample API Response (Live Not Enabled)

```json
{
  "status": "error",
  "message": "This strategy is not enabled for live trading. Enable it on the strategy page, or start it with mode 'sandbox'."
}
```

HTTP 409.

## Sample API Response (Mode Omitted)

```json
{
  "status": "error",
  "message": {
    "mode": ["Missing data for required field."]
  }
}
```

HTTP 400. Nothing was started and no order was placed. A value outside the
vocabulary answers `{"mode": ["Must be one of: live, sandbox."]}`.

## Request Body

| Parameter | Description | Mandatory/Optional | Default Value |
|-----------|-------------|-------------------|---------------|
| apikey | Your OpenAlgo API key | Mandatory | - |
| strategy_id | Strategy id, a positive integer | Mandatory | - |
| mode | `live` or `sandbox`. Exact and case-sensitive | Mandatory | **None. There is no default** |

## Response Fields

| Field | Type | Description |
|-------|------|-------------|
| status | string | "success" or "error" |
| run_id | integer | The run that was opened |
| mode | string | The mode the run was started in, echoed back |
| legs | array | Per-leg placement outcome |

Each object in `legs`:

| Field | Type | Description |
|-------|------|-------------|
| leg_id | integer | The leg's id within the strategy |
| ok | boolean | Whether the entry order was accepted |
| unconfirmed | boolean, optional | Desktop only. `true` when the order was sent but the broker gave no definite answer (a timeout or a lost reply). It may have been placed, so it is never sent again; its order row reads `unconfirmed` and OpenAlgo settles it from the broker's order book. `ok` is `false` in this case. Absent when the order intent could not be saved and no broker call was made |
| acknowledged | boolean, optional | Whether the broker's order id and status were written back. `false` with `ok: true` is a real order awaiting automatic linking, not a rejection. Absent when the order intent itself could not be saved and no broker call was made |
| symbol | string | The contract the leg resolved to |
| broker_order_id | string or null | The broker's order id on a live run, the sandbox engine's id on a sandbox run, `null` when the order was not accepted |
| error | string or null | Rejection reason when `ok` is false |

## Status Codes

| Code | Cause |
|---|---|
| 200 | A run was opened and at least one entry was accepted or is unconfirmed |
| 400 | Schema validation failed, including a missing or invalid `mode`. Also returned when the engine refused the start: a leg that could not be resolved, every entry rejected, a signal strategy, a Live start with no broker signed in, or new entries paused for a moment |
| 403 | `Invalid openalgo apikey` |
| 404 | `Strategy not found` |
| 409 | The strategy is already running, `mode` is `live` on a strategy that has not opted into live trading (also when live trading was switched off while the start was being made), or the strategy was changed while it was starting |
| 429 | Rate limited (100 a second) |
| 500 | `An unexpected error occurred` |
| 503 | OpenAlgo is locked: sign in to the app |

## Notes

- **`mode` is required and is never defaulted.** A caller that omits it is
  refused with a 400 and the engine is never reached.
- **The value is matched exactly.** `LIVE`, `Sandbox`, `paper` and `real` are
  all 400s. A near miss is never read as sandbox.
- **Live is opt-in per strategy.** Enable live trading on the strategy's page in
  the app before asking for `mode: "live"`. This endpoint cannot enable it.
- **A Live start needs your broker signed in.** Without a broker session the
  start answers 400 `Broker session is not available or has expired. Log in to
  your broker to restore it.` and nothing is placed. A sandbox start does not
  need one.
- **Starting twice is a conflict, not a silent success.** A second start against
  a running strategy answers 409 `This strategy is already running`, so two
  triggers firing at once cannot both place a full set of entries.
- In the desktop, a start re-checks the strategy just before it opens the run,
  and nothing is placed if it changed while the legs were being resolved. An
  edit or a token rotation in that window answers 409 `The strategy was changed
  while it was starting, so it did not start. Start it again.`; live trading
  switched off in that window answers 409 `This strategy is not enabled for
  live trading. Enable it first.`, the same status as the check made before
  the start (OpenAlgo web answers that one 400).
- In the desktop, new entries pause for a few seconds after a gap in order
  updates from the broker, while OpenAlgo re-reads the order book. A start in
  that window answers 400 `New entries are paused for a moment while OpenAlgo
  re-checks the broker's order book after a gap in order updates. Stops and
  exits keep running. Try again in a few seconds.`
- A start through this API records `trigger_source: "manual"` on the run.
- A run whose entry orders were **all** rejected is closed at once and the call
  answers 400. The message carries the venue's own words, for example
  `Every entry order was rejected: MIS orders cannot be placed after square-off
  time (15:15 IST). Trading resumes at 09:00 AM IST.` When legs were refused for
  different reasons, each is listed against its leg.
- Partial success is a 200. Check `legs[].ok` rather than assuming every leg is
  in the market.
- A run whose only entries are `unconfirmed` stays open and managed: those
  orders may be positions. Watch [`/events`](./events.md) for how each one is
  settled.
- Long legs are placed before short legs. On a spread, a short leg alone can be
  refused for margin the account would have had once the long leg existed.
- This endpoint is for `batch` strategies. A `signal` strategy has no start: its
  first entry signal after the session boundary opens the run, so a start
  against one answers 400 `A signal strategy has no start. Its run opens on the
  first long_entry or short_entry signal after the session boundary.` See the
  [webhook page](./webhook.md).
- A `sandbox` run's orders go to the sandbox engine and a `live` run's to your
  broker, whatever the analyzer mode switch says. Strategy orders never wait in
  the Action Center.

## Use Cases

- **Scheduled activation** from an external scheduler instead of the built-in one
- **Sandbox testing**: run the same strategy in `sandbox` before enabling live
- **Manual override** from a script, the Python SDK or an MCP client without
  opening the app

---

**Back to**: [Strategy Module API](./README.md), [API Documentation](../README.md)
