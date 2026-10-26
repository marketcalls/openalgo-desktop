# Strategy Module API

The `/strategy` module runs multi-leg options strategies with risk management
from entry to exit, plus a signal-driven mode for TradingView alerts. Two
surfaces reach it from outside the app window:

- an API-key surface under `/api/v1/strategy/`, nine POST routes
- a public webhook at `/strategy/webhook/<token>`

Building a strategy stays in the app, on the Strategy page (`/strategy`). The
API-key surface is lifecycle plus reads only. Nothing on it can create a
strategy, edit its configuration, enable live trading, rotate a webhook token,
or delete anything.

The requests and responses are the same as OpenAlgo web's, field for field, so
a script written against the web works against the desktop unchanged. Where the
desktop answers differently, the page says so in a line starting "In the
desktop". The [differences](#desktop-differences-at-a-glance) are also
collected at the end of this page.

## Endpoint Inventory

| Method | Path | Purpose |
|---|---|---|
| POST | `/api/v1/strategy/list` | [List strategies](./list.md) |
| POST | `/api/v1/strategy/status` | [One strategy and its current run](./status.md) |
| POST | `/api/v1/strategy/start` | [Start a run](./start.md) |
| POST | `/api/v1/strategy/stop` | [Stop the current run](./stop.md) |
| POST | `/api/v1/strategy/close_all` | [Close every leg](./close_all.md) |
| POST | `/api/v1/strategy/close_leg` | [Close one leg](./close_leg.md) |
| POST | `/api/v1/strategy/runs` | [Run history](./runs.md) |
| POST | `/api/v1/strategy/orders` | [Order history](./orders.md) |
| POST | `/api/v1/strategy/events` | [Risk-event audit trail](./events.md) |
| POST | `/strategy/webhook/<token>` | [Public webhook](./webhook.md) |

Every `/api/v1/strategy/` route is a POST with the identifier in the JSON body.
TradingView, Excel and Chartink cannot always choose a method or set a header,
so there is no GET form and no path parameter.

The public webhook is **not** under `/api/v1` and takes no `apikey`. The URL
token identifies the strategy.

## Address

The desktop listens on `http://127.0.0.1:5000` by default, so every example
uses that address. If you changed the port in Server Settings, use yours. A
program on another device on your network, or a service on the internet such
as TradingView, needs the address you gave it: your computer's network address
when "Allow access from other devices" is on, or the https address of a tunnel
you run (ngrok, Cloudflare Tunnel) or a custom domain.

## Seven Rules That Cost Money If You Get Them Wrong

1. **`mode` is required on `/start` and is never defaulted.** Omitting it is a
   400, not a live order.
2. **Live is opt-in per strategy.** A strategy is created sandbox-only.
   `mode: "live"` is refused with a 409 until you enable live trading on the
   strategy's page.
3. **A strategy id that does not exist returns 404 `Strategy not found`.** The
   desktop has one user, so every strategy you can see is yours; the answer for
   an unknown id never says more than that.
4. **No endpoint returns a webhook token.** Only its SHA-256 digest is stored.
   The token is shown once, in the app, when the strategy is created and when
   the token is rotated.
5. **An accepted `/stop` is not necessarily flat.** A 200 with
   `stop_pending: true` means the stop request is saved and its exits were
   accepted, but the run stays open, subscribed and managed until fills prove
   every position it owns is flat. A 409 can also carry `stop_pending: true`
   when an unfilled entry, a refused exit or another broker session still
   needs attention. Read that flag and the per-leg outcomes; never infer
   flatness from the HTTP status.
6. **`acknowledged: false` is not an order rejection.** The broker accepted the
   entry, but its order id and status could not be written back after a retry.
   The saved order intent and a critical `order_ack_unrecorded` event remain,
   and the run stays open while OpenAlgo links the two. `unconfirmed: true` is
   a different case (desktop only): the broker gave no definite answer, the
   order may be at the broker, and it is never sent again.
7. **Final P&L is written only after flatness is confirmed.** A checkpoint is a
   live mark. After a run stops, the run row's `pnl_realized` is the total.

## Authentication

Every `/api/v1/strategy/` request carries the OpenAlgo API key as `apikey` in
the JSON body. Create it on the API Key page in the app.

```json
{
  "apikey": "<APIKEY>",
  "strategy_id": 7
}
```

- An invalid or unknown key returns 403 with `Invalid openalgo apikey`.
- A missing key is a schema failure and returns 400.
- The `X-API-KEY` header is not read by these routes, as on the web. Put the key
  in the body.
- These routes need only the API key. Unlike the order endpoints, they answer
  while your broker is signed out (a Live start still needs the broker; see
  [`/start`](./start.md)).

In the desktop, the API is off until you sign in to OpenAlgo after it starts,
and again after you log out: the keys that check an API key are made from your
OpenAlgo password and are not kept on disk. Until you sign in, every
`/api/v1/strategy/` route answers 503:

```json
{
  "status": "error",
  "message": "Sign in to OpenAlgo to use the API. The API is off after OpenAlgo starts and after you log out, until you sign in again."
}
```

## Response Envelope

Success:

```json
{
  "status": "success"
}
```

Error:

```json
{
  "status": "error",
  "message": "Strategy not found"
}
```

On a schema validation failure, `message` is an object keyed by field name
rather than a string:

```json
{
  "status": "error",
  "message": {
    "mode": ["Missing data for required field."]
  }
}
```

## Status Codes

| Code | Meaning |
|---|---|
| 200 | Handled successfully |
| 400 | Schema validation failed, or the engine refused the request (for example a leg that could not be resolved) |
| 403 | `Invalid openalgo apikey` |
| 404 | `Strategy not found` |
| 409 | State conflict: not running, already running, live trading not enabled, a stop or close that could not finish |
| 429 | Rate limited |
| 500 | `An unexpected error occurred`, or the strategy's own records could not be read |
| 503 | OpenAlgo is locked: sign in to the app |

409 rather than 400 is deliberate wherever the payload is fine and the state is
not: the fix is to change the strategy's state, not the request.

In the desktop, a history read (`/runs`, `/orders`, `/events`) that fails
answers 500 `An unexpected error occurred`. The web answers an empty list with
`success`, which reads as "never ran". A stop or close that could not read its
own run answers 500 with a sentence saying nothing was closed (the web answers
409).

## Rate Limits

Each of the nine `/api/v1/strategy/` routes allows **100 requests per second**
per caller, in a moving window of its own: the web's general limit
(`API_RATE_LIMIT`) at the value of its sample settings, as for every other
general endpoint. An over-limit request gets 429 with
`{"message": "100 per 1 second"}` and no rate-limit headers.

The public webhook allows 100 requests a minute per caller and 100 a minute per
token. See the [webhook page](./webhook.md).

## Where A Strategy's Orders Go

- **The run's mode decides, not the analyzer mode switch.** A `sandbox` run
  sends every order to the sandbox (analyzer mode) engine and a `live` run to
  your broker, whatever the analyzer mode switch says while it runs. Switching
  modes in the app does not move a running strategy's exits.
- **Strategy orders never wait in the Action Center.** They are sent straight
  away even when the order mode is Semi-Auto, as on the web. The desktop's
  refusal of API orders when the Semi-Auto setting cannot be read applies to
  the order endpoints, not to strategy orders.
- **A Live run belongs to the broker account it was opened on.** The run
  records the broker and the account at start. If OpenAlgo is later signed in
  to another broker or another account, no order of that run is sent; it is
  refused with, for example, `This was opened on Zerodha (AB1234), but OpenAlgo
  is signed in to Angel One (XY9876) now, so the order was not sent. Sign in to
  Zerodha (AB1234) to manage it.` The run stays open and managed until you sign
  back in to the account it was opened on. The app also refuses a broker switch
  while a Live run is open.
- **Orders to some brokers are paced.** A strategy's Live orders go through the
  same broker connection as every other order, so a burst of legs queues
  instead of being refused by the broker. In the desktop, orders to Samco,
  Tradejini, HDFC Sky, HDFC Securities, Pocketful and 5paisa are paced at 10 a
  second, which the web does not do; brokers that publish their own order rate
  (Zerodha, for example) keep it. An order whose turn would be more than 10
  seconds away is refused with `OpenAlgo is pacing requests to stay within your
  broker's rate limit, and this one would have had to wait too long for its
  turn. Try again in a few seconds.`, which then shows as that leg's `error`.

## Vocabularies

### Strategy kinds

`batch`, `signal`

A batch strategy is a multi-leg spread entered and exited as a unit, driven by
`start` and `stop`. A signal strategy moves one leg at a time, driven by
`long_entry`, `long_exit`, `short_entry` and `short_exit`. Each kind refuses
the other's vocabulary.

### Universe tabs

`weekly_monthly`, `monthly_only`, `stocks_fno`, `mcx`

The instrument universe a strategy was built from. It decides which segments a
leg may use. A configuration saved without a tab has one derived from its own
legs rather than defaulted.

### Leg segments

`options`, `futures`, `cash`

A batch leg's segment must be one its universe tab offers. **Cash is offered on
`stocks_fno` only**: an index has no cash instrument of its own and an MCX
commodity has no spot. A signal leg takes `cash` or `futures` only; an exact
option contract is named as a symbol instead. A signal leg's segment and its
exchange must agree.

### Directions

`both`, `long_only`, `short_only`

The signal-mode direction filter. Ignored for batch strategies.

### Run modes

`live`, `sandbox`

Exact and case-sensitive on `/start`. `LIVE` is not `live`, and a near miss
such as `paper` is refused rather than read as sandbox.

### Strategy statuses

`stopped`, `running`, `paused`, `errored`

### Trigger sources

`manual`, `webhook`, `scheduler`

A run started through this API records `manual`.

### Stop reasons

`manual`, `scheduler`, `overall_sl`, `overall_target`, `lock_profit`, `eod`,
`expiry`, `daily_loss_limit`, `tick_stale`, `recovery_failed`, `error`

### Order kinds

`entry`, `exit_sl`, `exit_target`, `exit_trail`, `exit_overall_sl`,
`exit_overall_target`, `exit_lock_profit`, `exit_eod`, `exit_expiry`,
`exit_daily_loss_limit`, `exit_close_all`, `exit_leg_manual`, `exit_recovery`,
`exit_signal`

### Order statuses

`pending`, `open`, `complete`, `cancelled`, `rejected`

In the desktop, an order row can also read `unconfirmed`: it was sent, the
broker gave no definite answer, and OpenAlgo is settling it from the broker's
order book. It is never sent again automatically.

### Quantity modes

`lots`, `units`

`lots` multiplies the value by the contract's lot size from the master
contract, so 5 lots of NIFTY is 5 x 65. The lot **count** is what is stored, so
a strategy survives the exchange revising the lot size. `units` is the number
of shares or contracts outright. An unknown lot size in `lots` mode is an
error, never a guess.

`qty_mode` is a **signal** leg field. A derivative venue defaults to `lots` and
a cash venue to `units`; `lots` on a cash venue is refused. A **batch** leg has
no `qty_mode`: its `lots` count is always multiplied by the master contract's
lot size (1 for a cash row).

### Products

`CNC`, `NRML`, `MIS`

A strategy carries one product for every leg, read as the **intent**: `MIS` is
intraday everywhere, and anything else means carry the position, sent as `NRML`
on a derivatives venue and `CNC` on cash. A **short** cash leg under a carrying
product is refused rather than translated, because cash cannot be held short
overnight.

### Price types

`MARKET`

Only. Neither a strategy nor a leg carries a price, so a `LIMIT`, `SL` or `SL-M`
entry would go out priced at zero. Exits are `MARKET` on every path.

### Event severities

`info`, `warn`, `critical`

### Event kinds

The full list, with the desktop's additions, is on the
[`/events`](./events.md) page. These are the ones an operator must not ignore:

- **`run_stop_requested`** (`info`): the stop is saved and new signal entries
  are gated. It is not proof that the broker is flat; wait for `run_stopped`.
- **`run_stop_failed`** (`critical`): a pending stop could not make progress.
  The run is still open and **still holding those positions**.
- **`order_ack_unrecorded`** (`critical`): the broker accepted an order but its
  acknowledgement could not be written.
- **`order_unconfirmed`** and **`order_unconfirmed_held`** (`critical`, desktop
  only): an order may be at the broker without a definite answer. See
  [`/events`](./events.md).
- **`leg_expiry_fallback`** (`warn`): the chain did not list the expiry the leg
  asked for, so a nearer one was used.
- **`flip_outgoing_exit_rejected`** (`critical`): the outgoing side of a signal
  flip is still held and still managed.

### Webhook results

`ok`, `rejected_token`, `rejected_ip`, `rate_limited`, `rejected_dedupe`,
`rejected_cooling_off`, `rejected_invalid_action`, `rejected_live_disabled`,
`rejected_locked`, `rejected_payload`, `rejected_engine_error`

Each label's HTTP status and cause is on the [webhook page](./webhook.md).

## Timestamps

Timestamps are stored as UTC and returned as ISO 8601 strings with an explicit
`+00:00` offset, for example `2026-08-30T09:15:04.118332+00:00`. `entry_time`
and `exit_time` on a strategy are IST wall-clock strings in `HH:MM` form.

Money fields are JSON numbers, never strings.

## Desktop Differences At A Glance

| Area | OpenAlgo web | OpenAlgo Desktop |
|---|---|---|
| API after a restart | Works as soon as the server is up | 503 until you sign in to the app (keys come from your password) |
| History read failure | Empty list, `success` | 500 `An unexpected error occurred` |
| Stop or close that cannot read its run | 409 | 500 with a sentence saying nothing was closed |
| An order with no definite broker answer | Not distinguished | `unconfirmed: true` on the leg, order status `unconfirmed`, never re-sent, settled from the order book |
| An exit whose order row cannot be written | Sent anyway | Not sent; the leg stays managed (`The exit could not be recorded, so it was not sent`) |
| A strategy edited while it was starting | Not checked | 409 `The strategy was changed while it was starting, so it did not start. Start it again.` |
| New entries after a gap in order updates | Not paused | Paused for a few seconds while the order book is re-read; exits keep running |
| Live run, broker account changed | Not checked per run | No order sent; the run waits for its own account |
| Extra event kinds | None | `order_unconfirmed*`, `schedule_missed` |
| Webhook IP allowlist | Matches the real client address through a proxy | Matches devices on your network only; a caller through a tunnel never matches a non-empty allowlist |
| Webhook auto-lock | None | Locked after 10 refused-address alerts in 10 minutes |
| Webhook while OpenAlgo is locked | Not applicable | 503, nothing placed |
| Webhook 429 | `Retry-After` header and `retry_after` field | `retry_after` field only |

---

**Back to**: [API Documentation](../README.md)
