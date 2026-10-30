# OpenAlgo Desktop Strategy Module and Risk Engine Reference

## Purpose and scope

This document describes the strategy module of OpenAlgo Desktop: multi-leg
strategies with end-to-end risk management (`batch` kind), and the
signal-driven mode that reacts to individual TradingView alerts (`signal`
kind). It is the desktop counterpart of the web's
`docs/prompt/strategy_rms_documentation.md`, and it describes what the Rust
code on `master` does today.

The module is a port of the web's `services/strategy_module/` and
`services/risk/`. The wire formats, table and column names, vocabularies and
most behaviour are the web's. Where the desktop differs, the section says so
in a line starting "In the desktop", and the
[differences summary](#differences-from-openalgo-web) collects them.

Scope:

- the shared risk core and why it is shared
- the two strategy kinds and how they differ
- the data model
- the lifecycle of a run, from start through ticks to square-off and recovery
- the invariants that must not be broken, and why each one exists
- the order path the module shares with the rest of the app (broker account
  ownership, the order reconciler, broker pacing, Semi-Auto)
- what is deliberately not built yet

The public API (`/api/v1/strategy/*`) is documented endpoint by endpoint in
[`../api/strategy-services/`](../api/strategy-services/). For what a trader
reads, see [`../user/sandbox.md`](../user/sandbox.md),
[`../user/brokers.md`](../user/brokers.md) (broker switching) and
[`../user/troubleshooting.md`](../user/troubleshooting.md) (sleep, tunnels).

## Sources of truth

Code always wins over this document. When they disagree, the code is right
and this file is stale. All paths are under `src-tauri/src/` unless stated.

| Concern | File |
|---|---|
| Module root, startup order, file map | `strategy/mod.rs` |
| Schema, store, vocabularies | `strategy/store.rs`, `strategy/store/*.rs` |
| Risk rules (shared, pure) | `risk/` (`models.rs`, `position.rs`, `aggregate.rs`, `adapters.rs`) |
| Rule translation | `strategy/risk_adapter.rs` |
| Run lifecycle, tick decisions | `strategy/engine.rs`, `strategy/engine/{start,stop,exits,fills,ticks}.rs` |
| Signal-mode protocol | `strategy/signals.rs`, `strategy/signals/{entries,exits}.rs` |
| Live run state and every claim | `strategy/state.rs` |
| Start claim (stopped to running) | `strategy/start_claim.rs`, `strategy/store/management.rs` |
| Order placement, run destination | `strategy/dispatch.rs` |
| Broker account a Live run belongs to | `strategy/run_owner.rs`, `services/live_owner.rs` |
| Prices | `strategy/tick_feed.rs`, `strategy/tick_feed/pending.rs`, `strategy/tick_context.rs` |
| Fills | `strategy/order_events.rs` |
| Order-book reconciliation | `strategy/reconcile.rs`, `services/order_reconciler/` |
| Durability | `strategy/checkpoint.rs`, `strategy/recovery.rs` |
| Scheduler | `strategy/scheduler.rs`, `strategy/session.rs` |
| Public webhook | `strategy/webhook.rs`, `strategy/webhook/pipeline.rs` |
| Broker-backed books | `strategy/views.rs`, `strategy/store/by_mode.rs` |
| Per-strategy position book (`strategy` tag) | `strategy/book.rs` |
| Contract resolution | `strategy/resolver.rs` |
| Configuration validation | `strategy/validate.rs`, `strategy/validate/legs.rs` |
| Live push frames | `strategy/broadcast.rs` |
| Session API and Socket.IO join | `server/routes/strategy_module.rs`, `server/routes/strategy_module/history.rs` |
| API-key surface | `server/api_v1/strategy.rs` |
| Frontend | `src/pages/strategy/` (`List`, `Wizard`, `Edit`, `Detail`), `src/api/strategy_module.ts` |

Tests are the second source of truth: `src-tauri/tests/it/strategy/`
(about 240 tests against real services and a mock broker), `risk/tests.rs`,
and the golden vectors in `tests/fixtures/risk/vectors.json`. Defects that
were deliberately not carried over from the module the web ported are named
in the tests with a `PORTED DEFECT` comment, as on the web.

## Architecture

```
                  OpenAlgo window / browser          TradingView / SDK / Excel
                             |                                  |
     server/routes/strategy_module.rs              server/api_v1/strategy.rs
          (signed-in session)                          (API key)
                             |                                  |
                             +----------------+-----------------+
                                              |      /strategy/webhook/<token>
                                              |      (strategy/webhook.rs)
                                              |                 |
                              strategy/engine.rs, strategy/signals.rs
                                              |
      +---------------+---------------+-------+-------+---------------+--------------+
      |               |               |               |               |              |
 risk_adapter     dispatch        tick_feed     order_events      reconcile        state
      |               |               |               |               |              |
   crate::risk   place_order /   market-data     event bus     order reconciler  in-process
   (SHARED,      sandbox engine  feed + REST     order.update  (order-book       run state
    pure)        (per run)       multiquotes                    reads)
                                              |
                               checkpoint.rs / recovery.rs
                                              |
                          main SQLite database (six sm_ tables)
```

Everything runs inside the desktop's single Rust process. There is no worker
model to reason about: shared state sits behind `parking_lot` mutexes whose
critical sections never await, and background work is owned tokio tasks that
the module stops on shutdown (`StrategyModule::shutdown`).

## The shared risk core

`risk/` (the Rust module `crate::risk`) is the one place the desktop decides
whether a position has hit its stop, taken its target, earned a tighter
trailing stop, or whether a set of positions has run past its combined
limits.

**It performs no I/O of any kind.** No database, no broker, no market data,
no clock, no logging. Every input arrives as an argument and every decision
leaves as a return value. This is enforced, not just reviewed:
`risk/tests.rs` (`the_package_imports_nothing_that_does_io`) fails the build
if any file of the core mentions `rusqlite`, `reqwest`, `tokio`, `tracing`,
`std::fs`, `std::net`, `std::time`, `chrono`, the app's database, broker,
state or service modules, Socket.IO, or a wall-clock call.

```rust
use crate::risk::{
    PositionRisk, PositionDecision, evaluate_position,
    AggregateRisk, AggregateDecision, evaluate_aggregate,
    aggregate_pnl, trail_stops_to_entry,
    stop_from_points, target_from_points, Side, TrailMode, BreachReason,
    evaluate_trail, // the legacy scalping dict shape
};
```

Consumers translate; they do not decide. Today the core is used by the
strategy module (through `strategy/risk_adapter.rs`) and by the scalping
terminal's server-side stop monitor (`scalping/monitor/`).

**The contract is the web's `test/risk/vectors.json`**, copied to
`tests/fixtures/risk/vectors.json`: 35 cases, one tick against one position
each. Two tests run every case through the Rust core and assert the case
count, so a dropped case cannot pass silently (`risk/tests.rs` and
`tests/it/strategy/risk_vectors.rs`). A rule change starts with a new vector
in the web's file, copied here.

The browser keeps the web's TypeScript copy of the per-position rules in
`src/hooks/useTrailingSL.ts`, carried over unchanged. As on the web, its own
tests do not read `vectors.json`.

`aggregate_pnl` counts a position's realized P&L whether or not it is
currently open. A signal leg re-entered after a completed round trip carries
that round trip's result and is open again; counting realized only while
closed would make the figure vanish the moment the next entry is placed, so a
daily loss limit would reset on every flat moment.

### Points or percent

Each leg carries `risk_unit`, `points` (the default) or `percent`, and it
governs `sl_pts`, `target_pts` and both halves of `trail` together. A leg
with no `risk_unit` is a points leg.

The conversion happens in `risk_adapter` and nowhere else: the core speaks
one language, points from entry. A percentage is measured against the leg's
own entry price, so 2% on a short filled at 2500 is a stop at 2550.
Percentages are capped at 100 by the validator.

**A percent leg with no confirmed fill gets no levels at all.** A percentage
of an entry of zero is not a stop at the entry price; it is a stop that
cannot be computed yet, and inventing one would fire it on the first tick.

The field names keep saying `pts` in the wire format and the database, as on
the web, so stored strategies and existing callers keep working.

### risk_adapter surface

```rust
side_of(position: &str) -> Result<Side, String>            // strict: only "B" or "S"
leg_to_position_risk(leg: &LegState) -> Result<PositionRisk, String>
evaluate_leg(leg: &mut LegState, last_price: f64) -> Result<PositionDecision, String>
apply_leg_decision(leg: &mut LegState, d: &PositionDecision)
run_pnl(state: &RunState) -> Result<(f64, f64), String>    // (realized, unrealized)
run_to_aggregate_risk(state: &RunState, strategy: &Value) -> AggregateRisk
evaluate_run(state: &mut RunState, strategy: &Value) -> Result<AggregateDecision, String>
apply_run_decision(state: &mut RunState, d: &AggregateDecision)
trail_open_legs_to_entry(state: &mut RunState, triggering_leg_id: i64) -> Vec<String>
```

Two mappings are easy to get wrong:

- **A trailing stop's gap is `trail_step`, not `trail_trigger`.** An X-only
  configuration (fixed-distance trail) passes X for both and trails
  continuously; X with Y starts at X and advances in Y steps. Passing 0
  disables trailing, because the core needs a positive step.
- **`overall_sl_mtm` is stored positive and applied as a negative
  threshold.** It passes through unchanged; the core takes it that way.

Peak and trough are written back on every evaluation, breach or not (one of
the ported defects wrote them on one stop path only).

### Combined MTM is an exit trigger, not an execution guarantee

The combined stop, target and lock-profit rules evaluate the run's rolling
latest-known marks. A tick for one symbol updates the matching leg, and the
basket is evaluated with that new mark plus the latest marks already held for
the other legs. Independent ticks are not an exchange-atomic snapshot.

Crossing `overall_sl_mtm` or `overall_target_mtm` proves the marked basket
crossed the threshold and an exit was requested. It does not guarantee the
same realized P&L: the covering MARKET orders fill against the book, and
spread, movement and sequential placement move the fills away from the
trigger mark. The run keeps both truths: `pnl_peak` and `pnl_trough` describe
the marked path, and `pnl_realized` comes from confirmed fills.

## The daily loss limit

`daily_loss_limit_inr` is a limit on the **session**, not on a run.
`overall_sl_mtm` cannot express it: it resets every time a run opens, which
for a signal or scheduled strategy is several times a day.

The session is the one that began at the platform's session boundary (03:00
IST by default, when broker tokens expire), not midnight;
`strategy/session.rs` holds that boundary, and 01:00 on Tuesday belongs to
Monday's session. Runs finished since the boundary contribute the realized
figure on their row; the live run contributes its marked total. Crossing the
limit squares off and stops with `stop_reason = "daily_loss_limit"`, recording
a critical event that names the session loss and the limit.

In the desktop, the banked half (the strategy's other runs this session) is
read once per run and session into the in-memory tick context
(`strategy/tick_context.rs`), off the async worker, because one strategy runs
one run at a time and that figure cannot change while it runs. The web
caches it and invalidates the cache on `finish_run`.

## The two strategy kinds

`sm_strategy.strategy_kind` is `batch` (default) or `signal`. They share
every table and all the machinery for state, orders, risk and recovery. What
differs is the protocol, the leg shape and the run lifecycle.

| | batch | signal |
|---|---|---|
| Trigger | `start` enters every leg, `stop` exits every leg | one alert moves one leg |
| Leg shape | segment, position, lots, option type, strike mode, offset, expiry | symbol, exchange, side, qty, segment, expiry |
| Options | relative option legs and spreads | an exact contract may be named; no expiry-rank or strike resolution; spreads stay in batch mode |
| Quantity | the configured count times the contract's exact lot size, on every segment (cash lot size is 1) | derivatives: `qty_mode = lots` (times the exact lot size) or `units` (a whole-lot quantity); cash: units only |
| Run | one per start-to-stop cycle | one per platform session, opened by the first signal |
| Run mode | `mode` on start (`live` needs the strategy's Live opt-in) | Live if the strategy is opted in to Live, else Sandbox |
| Webhook actions | `start`, `stop` | `long_entry`, `long_exit`, `short_entry`, `short_exit` |
| Legs at run start | all entered together | inactive until a signal opens one |
| Order kinds | `entry` plus the risk, scheduler and manual `exit_*` kinds | `entry` and `exit_signal` |

Each kind refuses the other's actions. A signal strategy has no start: a
start request answers "A signal strategy has no start. Its run opens on the
first long_entry or short_entry signal after the session boundary."

### Signal mode semantics

Three outcomes, and the difference between them is the design:

| Outcome | Answer | Example |
|---|---|---|
| Acted | 200, an order was placed | `long_entry` on a flat leg |
| No-op | 200 with a note | `long_entry` on a leg already long |
| Refused | 4xx | `short_entry` on a `long_only` strategy |

The no-op notes are `already_long`, `already_short`,
`no_matching_position`, `outside_entry_window` and `outside_trading_window`.
A no-op answers success on purpose: an alert engine repeats itself, and a
retry on an order path is how one alert becomes two positions.

Other signal rules, as on the web:

- **The side a leg is held comes from the signal that opened it**, never from
  configuration. A leg's configured `side` says which signals it accepts.
- **An opposite entry squares first, then opens.** The outgoing position is
  kept under `superseded` until its exit fills, so one leg id can describe
  both positions without either being lost.
- **A leg returns to `configured` after an exit**, not `closed`, so the same
  symbol can be signalled again in the same session. Its realized P&L
  accumulates on the leg.
- **Signal actions skip the webhook dedupe and cooling-off windows**, because
  signal mode is already idempotent by meaning and a 60 second window would
  suppress a genuine long, short, long sequence.
- **Two alerts on one bar join one run.** Finding or opening the session's
  run is serialised per strategy. The first signal of a new session closes
  the previous session's run with reason `eod` before opening a new one.

## Data model

Six tables in the main SQLite database, all `sm_` prefixed, created by
migration `072_strategy_module` with the web's table and column names:

| Table | Purpose |
|---|---|
| `sm_strategy` | configuration: legs (JSON), risk parameters, scheduler, webhook token digest, Live opt-in, webhook lock |
| `sm_strategy_run` | one activation: mode, broker and broker account, start and stop, durable pending-stop request, P&L |
| `sm_strategy_order` | every durable order intent: exact `position_ref`, acknowledgement, fills, rejection |
| `sm_strategy_checkpoint` | periodic runtime snapshot for recovery |
| `sm_webhook_event` | every webhook request admitted to the pipeline, accepted or refused |
| `sm_strategy_event` | the risk and lifecycle audit trail |

Two desktop columns were added by later migrations, both idempotent:

- `sm_strategy.revision` (`086_strategy_revision`): advanced by every
  configuration write (edit, Live opt-in, webhook lock, token rotation). A
  start and an edit compare it with the revision they validated, so a change
  that lands while a start resolves its legs refuses that start ("The
  strategy was changed while it was starting, so it did not start. Start it
  again.") instead of being overtaken by it.
- `sm_strategy_run.broker_account` (`090_strategy_run_account`): the account
  at the broker the run was opened on, next to the web's `broker` column. See
  [Broker account ownership](#broker-account-ownership-of-a-live-run).

Conventions:

- **Timestamps are naive UTC** (`YYYY-MM-DD HH:MM:SS.ffffff`), rendered with
  an explicit `+00:00` at the boundary.
- **Money is stored as SQLite REAL** in columns declared `NUMERIC(18, 2)`, and
  handed out as `f64`.
- **Children are deleted explicitly with their strategy.** SQLite enforces
  foreign keys only when asked, and a reused rowid would re-attach an orphaned
  audit trail to the next strategy created.
- **Every guard the engine depends on is one conditional statement**, not a
  read then a write: the start claim, the terminal run update, the order-fact
  fold and the acknowledgement bind.
- **Checkpoints are pruned** (see [Durability](#durability)).

Vocabularies are constant slices in `strategy/store.rs`, not SQL CHECK
constraints: `STRATEGY_KINDS`, `DIRECTIONS`, `STRATEGY_TYPES`, `RUN_MODES`
(`live`, `sandbox`), `STRATEGY_STATUSES`, `TRIGGER_SOURCES`, `STOP_REASONS`,
`ORDER_KINDS`, `ORDER_STATUSES`, `EVENT_KINDS`, `EVENT_SEVERITIES`,
`WEBHOOK_RESULTS`. In the desktop, an order row may also read `unconfirmed`
(a placement with no definite answer, see
[The order reconciler](#the-order-reconciler-and-the-entry-gate)), and the
event log also carries `order_unconfirmed` and `schedule_missed` events.

## Run state

`strategy/state.rs` holds live run state in process: one mutex per run in a
registry keyed by run id. The stored shape serialises to exactly the
checkpoint's `leg_state` JSON, so a snapshot round-trips without translation.

Two rules:

- **A leg always carries a `position`.** Building a leg state without one is
  an error, never a default: the original treated anything not `"B"` as a
  short and evaluated those legs with an inverted sign.
- **A critical section holds in-memory bookkeeping only.** No database, no
  broker, no emit, no await. Callers read what they need, release, and then
  do the slow work.

Every claim (`claim_leg_exit`, `claim_legs_for_exit`, `claim_superseded_exit`,
`claim_signal_entry`, `mark_stopping` and their release and bind
counterparts) checks and marks in one hold of the run's lock. That is the
whole defence against two decisions becoming two orders.

## Engine

```
start_run    resolve every leg, claim the strategy, place entries
stop_run     request exits and finalise after confirmed fills
close_leg    exit one leg; the run continues with the rest
process_tick evaluate risk against a price and dispatch what it decides
apply_fill   record a fill against the exact position it belongs to
```

Load-bearing orderings, kept from the web:

- **Locks are released before orders are placed.** The tick path evaluates
  under the lock, collects what it decided, releases, then dispatches.
- **Entries go BUY before SELL.** A spread whose short leg is placed first can
  be refused for margin the account would have had once the long leg existed.
- **Every leg is resolved before anything is claimed.** A leg that cannot be
  resolved leaves no run row, no claimed strategy and no orders.
- **The claim re-checks what the start decided.** One conditional UPDATE moves
  the strategy from stopped to running only if the revision, the Live opt-in
  (for a Live start) and the webhook lock (for a webhook start) are still what
  the start read (`start_claim.rs`). No lock is held across the leg
  resolution's broker price call.
- **An exit uses the symbol the run holds**, never a re-resolved one. An ATM
  leg resolved again hours later names a different strike.
- **The order row is written before the broker is called.** An entry that
  cannot be recorded is not placed; an exit that cannot be recorded is placed
  anyway, because getting flat wins and the audit row is what is lost, loudly.
- **The broker's acknowledgement is written back, and that write is checked.**
  Retried once; if it still fails, an `order_ack_unrecorded` critical event
  carries the exact row, run, leg and broker facts, and the row is repaired
  from it on the next pass, by recovery and before any stop. A stop never
  closes on a guess while an acknowledgement is unlinked.
- **A leg whose entry was accepted but not filled is never exited.** Closing
  it answers "The entry for this leg has been accepted but not filled, so
  there is no confirmed quantity to exit. Retry once it fills." A stop first
  cancels working entries through the run's own pipe, then folds one broker
  status fact for each; cancellation is intent, never proof.
- **A rejected exit is undone wherever the rejection arrives**, synchronously
  or later on the order stream, so the leg stays exitable.
- **A leg is closed by its fill arriving**, not by its exit being placed.
- **A stop request is durable before any exit.** The request time and reason
  are written to the run row, then `mark_stopping` blocks new signal entries.
  Accepted, still-working exits answer `stop_pending: true`; the run stays
  current, subscribed and managed, and finalises only once the exact owner
  quantities confirm it is flat. The scheduler retries pending stops every
  5 seconds; recovery resumes them after a restart.
- **A stop whose exits were all refused does not close the run.** It stays
  open and managed, records `run_stop_failed` at critical severity, and
  reports what was refused. `close_leg` likewise reports a refusal rather than
  answering ok.
- **Trail-to-entry fires only on a stop-driven exit**, never on a manual
  close.

A stop or close answers `{ok, stop_pending, exits, error?, run_stopped?}`.
In the desktop, a stop or close whose own records could not be read answers
an error (HTTP 500, "The strategy records could not be read just now, so
nothing was closed and no exit order was placed. The run is still open and
managed.") rather than "not running" or "not found"; any other refusal is 409.

When risk fires while there is no broker session (the 03:00 IST token expiry
window, or before the trader signs in after a restart), nothing is exited and
one critical event per episode says so ("Risk triggered but there is no
broker session, so nothing could be exited. Positions are still open."). The
positions stay managed and are acted on once the session is back.

## Order dispatch

```rust
build_order(symbol, exchange, action, quantity, product, strategy_name, pricetype) -> OrderPayload
exit_action(position: &str) -> Result<&'static str, String>   // "B" -> SELL, "S" -> BUY
product_for_exchange(product, exchange) -> String
trait OrderGateway { place, cancel, order_status, authorised, broker_name,
                     broker_account, book, ltp, order_book_rows, ... }
```

`AppGateway` is the production gateway over the app's services; tests inject
a fake. Departures from how the rest of the app places orders, all
deliberate and all as on the web:

- **The destination is per run, not global.** A Sandbox run calls the
  sandbox engine directly whatever the analyzer toggle says. A Live run calls
  the order service with `force_live`, so a trader turning analyzer mode on
  while a Live run holds real positions cannot divert its exits into the
  sandbox (which would report success and leave the real position unmanaged).
- **The Action Center is bypassed** (`internal`): a stop-loss exit that waits
  for a human to approve it is not a stop loss. Strategy orders never consult
  the Semi-Auto order mode.
- **The product is translated to the venue.** MIS is intraday everywhere;
  anything else means carry, NRML on a derivatives venue and CNC on cash. A
  short cash leg under a carrying product is refused.
- **Entries and exits are MARKET** (`PRICETYPES = ["MARKET"]`).
- **Broker authorisation is resolved per order** and never cached in run
  state; a missing session refuses the order with "Broker session is not
  available or has expired. Log in to your broker to restore it."

`exit_action` derives from the side the leg actually holds, never from the
configured side (the original doubled shorts).

**In the desktop, a placement has three outcomes, not two** (`DispatchResult`,
LOG-08): accepted (an order id), refused (provably not sent, or the broker
said no: safe to release the claim and decide again) and uncertain (sent, or
maybe sent, with no definite answer). An uncertain placement keeps its claim,
its row reads `unconfirmed`, and it is never placed again until the order
reconciler has found it in the broker's book or settled it otherwise. Which
broker errors count as a refusal is tracked per adapter in
[`../port/order-outcomes.md`](../port/order-outcomes.md).

## Broker account ownership of a Live run

In the desktop, every Live strategy run records the broker and the account at
that broker it was opened on (`sm_strategy_run.broker` and `broker_account`,
filled from `LiveOwner::of_gateway` when the run is created; a signal run
records the same when its session run opens). Every order and cancel of a run
goes through `StrategyModule::place_for_run` or `cancel_for_run`
(`strategy/run_owner.rs`), which compare that owner with the session connected
now and refuse without sending when it is another broker or another account:

> This was opened on Zerodha (AB1234), but OpenAlgo is signed in to Zerodha
> (XY9876) now, so the order was not sent. Sign in to Zerodha (AB1234) to
> manage it.

Nothing was sent, so it is a definite refusal: the claim is released and the
run stays open and managed. Risk exits on ticks, Stop, leg close and signal
orders check `authorised_for` first and tell the trader once per episode, as
for a missing session; a Stop answers `ok: false, stop_pending: true` with
that message. Back on its own session, the next Stop exits the run. If the
run's owner cannot be read, the order is refused too ("OpenAlgo could not
read which broker account this run belongs to, so the order was not sent.").

The comparison is lenient only where nothing was recorded: a run created
before owners were kept, or a broker that reports no account. Where a broker
names no account at login, the session's account is the identity of the
saved key set it was made from (`services/session_account.rs`), so a run
recorded then with the broker's placeholder is still managed by that broker's
keys, while a run recorded with a real account id is not exited through a
session told only by keys. This layer sits behind the broker switch guard
(`services/obligations.rs`), which refuses a switch to another broker while
Live strategy runs, uncertain placements or partly filled exits remain;
Sandbox runs never block a switch and keep running on the new broker's
quotes. Pinned by `tests/it/strategy/obligations.rs`
(`dispatch_refuses_an_a_owned_exit_while_b_is_active` and its neighbours).

The web records only the broker on a run and does not compare it before
sending.

## The order reconciler and the entry gate

In the desktop, order updates are treated as events that can be lost, and the
broker's order book is the authority (`services/order_reconciler/`,
`strategy/reconcile.rs`). Every 5 seconds, and at once after a gap, one
reconciler reads the order book once per destination (Live through the broker
session, Sandbox through the sandbox engine) and hands the read to every
owner of open orders: strategies, scalping and OpenScript runs.

- **A working order with a broker id** is folded through the same path as an
  `order.update` frame. The row's cumulative facts guard the fold, so a fill
  the stream already delivered is not booked twice, and one it lost is booked
  once.
- **An unconfirmed placement** is looked up by its client tag where the
  broker reports one, else by its order fields among unowned orders. Found,
  it is bound and folded. Absent: in Sandbox, four absent reads over at least
  20 seconds reject it as never placed and release the claim. In Live,
  absence alone never does: the claim is held, a critical event and a Health
  alert are raised once, and the row is released when the trader marks it
  not placed on the Health page, or automatically only after 10 minutes of
  absence with no matching trade and an unchanged net position. Several
  matching orders are never guessed between.
- **A gap in order facts** (a dropped critical bus delivery, a lagging order
  relay, an order feed reconnect, a new broker session) pauses new entries
  for that destination until a book read closes it: new strategy starts and
  signal entries are refused with a "New entries are paused for a moment
  while OpenAlgo re-checks..." message. Exits are never held by it.

The web has no reconciler; a lost update there leaves the row as it was.

## Broker order pacing

Strategy orders go through the broker adapters like every other order, and
the adapters pace outgoing requests (`brokers/common/ratelimit.rs`). Orders
have their own lane, so data calls never queue ahead of an exit. Where a
broker publishes an order rate, its adapter paces at it (Zerodha 10 a second,
for example). Six brokers that publish none (Samco, Tradejini, HDFC Sky, HDFC
Securities, Pocketful, 5paisa) are paced at `ORDERS_PER_SECOND = 10`, the
exchanges' threshold above which a retail API client's algorithm must be
registered. In the desktop this is a deliberate deviation: the web paces none
of those six.

A request whose turn would come more than 10 seconds away is refused at once,
before sending ("OpenAlgo is pacing requests to stay within your broker's
rate limit, and this one would have had to wait too long for its turn. Try
again in a few seconds."). For the strategy module that is a definite
refusal like any other: the claim is released, the leg stays exitable, and a
stop whose exits were refused leaves the run open and managed.

## Tick feed

```rust
feed.add_run(run_id, &keys).await      // refcounted; subscribes newly referenced symbols
feed.remove_run(run_id).await          // unsubscribes symbols no run holds
feed.release_all().await               // shutdown
feed.health(now)                       // prices read, combined and missed; exits waiting
```

Subscriptions are refcounted per run, so two strategies holding one contract
share one subscription. Prices come from the app's market-data feed
(`websocket::WebSocketManager`); a held symbol with no streamed tick for 10
seconds is polled over the broker's multi-quote call every 2 seconds, at most
50 symbols per poll. **Both the stream and the poll drive `process_tick`**:
a leg that has fallen back to polling is risk-evaluated on polled prices too.

Two owned tasks:

- **The reader** takes every tick of the market-data broadcast and decides
  each price of a held symbol as it arrives, through the risk core in the
  run's lock, never waiting on an action or on the database. A tick it
  misses (the broadcast lagged) is counted, and the Health page raises an
  alert.
- **The evaluator** acts on what was decided (exits, stops, events), run by
  run in the order they decided, and runs the polling fallback.

In the desktop, no price is folded or shed before risk has judged it
(PROD-REL-01): a trailing stop depends on the path, so a peak that raised
the stop and the dip that then hit it must both be seen. While the
evaluator is busy, a run's decisions are combined instead
(`strategy/tick_feed/pending.rs`): each position keeps its first exit, the
first stop is kept, and leg exits decided with or after a stop are placed
after it (a leg the stop exited refuses them). The web queues ticks in one
bounded FIFO and sheds when full, which can drop the deciding tick.

What the tick path needs of a running strategy is read once per run into
`strategy/tick_context.rs`, before its prices are subscribed, so no tick
waits on the database. This is safe because a running strategy's
configuration cannot change: edits, the Live opt-in and deletion are refused
while it runs. What the reader still finds missing is read in an owned task,
retried with a backoff capped at 30 seconds.

In the desktop, the stream-to-polling switch is not reported: the
`tick_source_switched_to_polling`, `tick_source_switched_to_ws` and
`tick_source_stale` event kinds exist in the vocabulary but are not emitted,
there is no `STALE` state that stops a run with `tick_stale`, and a leg's
`tick_source` reads `ws`. The thresholds are constants (`STALE_AFTER`,
`POLL_EVERY`, `POLL_BATCH_MAX`), not environment variables: the desktop reads
no `STRATEGY_TICK_*` settings.

## Fills

`strategy/order_events.rs` subscribes once to the event bus topic
`order.update` on its critical lane. The platform publishes it for every
asynchronous status change, Live or Sandbox. Deciding an update is not ours
costs one indexed lookup on `broker_order_id`.

- **A fill is applied exactly once.** The order row's cumulative facts are the
  guard, so a fill reported twice adds its quantity once.
- **A rejection is final.** A late working frame cannot reopen it.
- **A fill is applied only to the order it belongs to.** The row id and
  position reference travel with the fill, so a signal flip's exit fill
  settles the outgoing position, never the one just opened.
- **A price must be strictly positive and finite**, checked with the core's
  own `is_price`. A positive fill with no usable price still proves quantity;
  valuation waits for a priced witness.
- **The leg is resized to what actually filled**, and a terminal partial
  quantity is still exposure in every status.
- **An exit on a leg whose entry never filled books nothing.**

A Sandbox MARKET order fills inside the dispatch call, before its row carries
the broker id. In the desktop those frames are held in a bounded stash (512
order ids, 16 frames each, 120 seconds) and replayed the moment the row is
bound. Whatever the stream misses, the order reconciler folds from the book.

The per-strategy position book (`strategy/book.rs`, the web's
`strategy_book_db.py` and its subscriber) is a separate consumer of the same
bus: it tags `orderid -> strategy` from `order.placed` and books the unseen
part of each fill.

## Durability

**Checkpoints** (`strategy/checkpoint.rs`): every 5 seconds, one
`sm_strategy_checkpoint` row per live run. The snapshot is taken under the
run lock (in memory only) and written after release. Rows are pruned to the
newest 200 per run every 120 passes.

**Recovery** (`strategy/recovery.rs`, `recover_all` and `recover_run`) runs at
startup, before any price arrives. It merges two sources with a deliberate
precedence. Identity, disposition and ownership come from **order rows grouped
by exact `position_ref`**. Volatile risk state (last price, effective stop and
target, trail flags, favourable extremes) comes from the newest checkpoint. A
leg's side is derived from its entry action.

- A **dead** order is never upgraded by a checkpoint. A **working** one may
  be, because the checkpoint is written from the same fill the engine applies.
- Order status is normalised in one place, and an unrecognised status reads
  as working: reading an unknown exit as dead would let a second exit be
  placed, which opens the opposite position.
- When durable rows prove ambiguous exposure (more than two held owners on
  one leg, a working exit with no confirmed owner), the run is neither
  installed nor finalised: it stays open and reserves the strategy, with a
  critical `recovery_failed` event asking for manual reconciliation.
  Malformed state with no proven exposure is finalised with
  `stop_reason = "recovery_failed"` so it cannot wedge startup.
- A durable pending stop is recovered with its reason; a held rebuild is
  hydrated in stopping state and its exits are retried.
- Recovery is idempotent: a run already live in memory is never overwritten.

After a restart the desktop is locked until the trader signs in (keys are
derived from the OpenAlgo password). Recovery still rebuilds the runs from the
database at startup, but Live orders need the broker session, which resumes
after sign-in; until then a Live run's risk exits are refused as above and
the run stays managed.

## Scheduler

`strategy/scheduler.rs`. A strategy's `scheduler` configuration
(`{enabled, days, start_time, auto_stop_time, default_mode}`) becomes at most
two jobs, `strategy:{id}:start` and `strategy:{id}:stop`.

Kept from the web:

- **Every job is on Asia/Kolkata**, never the computer's local time.
- **Job defaults**: misfire grace 60 seconds, coalesce, one instance.
- **Jobs are plain values** (a job kind and a strategy id), never closures.
- **`exit_time` installs the square-off** when no `auto_stop_time` is set,
  and when the scheduler is switched off entirely, on weekdays: the default
  intraday configuration, started by an alert and squared off by the clock.
- A scheduled Live start for a strategy not opted in to Live is refused and
  recorded as a `live_disabled` warning event. The square-off is not gated on
  `enabled`: an open run is closed whatever the configuration now says.

The job table is a projection of the database, rebuilt at start and on every
create, update and delete. One owned task evaluates due jobs each second
against the injected clock and retries durable pending stops every 5 seconds.
The desktop has no APScheduler; the semantics above are reimplemented.

In the desktop, a laptop can sleep. When the computer wakes, a slot passed
over runs late only if it is a square-off in the current session (closing
late is the protection); a missed start, or a square-off from an earlier
session, is not run and is recorded as a `schedule_missed` warning event
that tells the trader what to check. See
[`../user/troubleshooting.md`](../user/troubleshooting.md).

## Public webhook

`POST /strategy/webhook/<token>`. The URL token is the whole credential:
TradingView cannot set a header. Tokens start with `oaws_`, are stored only as
a SHA-256 digest (compared in constant time), are shown once at creation or
rotation, and are never logged: the traffic log stores the path with the
token replaced, and the audit row stores a redacted payload.

Admitted requests run this pipeline, each refusal audited in
`sm_webhook_event`:

| Stage | Result label | Status |
|---|---|---|
| Token resolves | `rejected_token` | 404 |
| Kill switch off | `rejected_locked` | 403 |
| IP in allowlist | `rejected_ip` | 403 |
| Payload parses and fits (16 KB) | `rejected_payload` | 400 |
| Action valid for this kind | `rejected_invalid_action` | 400 |
| Start names a mode | `rejected_invalid_action` | 400 |
| Live opt-in | `rejected_live_disabled` | 403 |
| Not a duplicate within 60 s (batch only) | `rejected_dedupe` | 200, ok |
| Not within 30 s of a stop (batch only) | `rejected_cooling_off` | 409 |
| Engine acted | `rejected_engine_error` | 500 |

As on the web: an unknown token answers exactly like a malformed one; the
cooling-off window is noted on every stop, not only webhook-initiated ones, so
a stale alert cannot re-enter a position the strategy's own rules just
closed; audit rows for unrecognised tokens are capped at the newest 1000; the
per-token rate limit is keyed on the token's digest. The rate limits are the
web's 100 a minute, per caller address and per token.

Desktop differences:

- **While OpenAlgo is locked** (after a restart, until sign-in, or after
  logout), a known token answers 503 with "OpenAlgo is locked, so this alert
  was not acted on. Alerts are refused after OpenAlgo starts and after you
  log out: sign in to OpenAlgo to receive them again."
- **Callers are classified, not read from forwarding headers**
  (`server/source.rs`): a program on this computer, a device on the network
  (its socket address), or a caller through a tunnel or proxy on this
  computer (no address the app can see; all tunnel callers share one
  identity). The IP allowlist therefore applies to devices on the network
  only and never matches a tunnel caller, and tunnel callers are rate-limited
  per token once the token checks out. The webhook tab on the Detail page
  says so.
- **Repeated refusals lock the webhook.** Ten requests with a valid token
  from outside its allowlist within 10 minutes lock that webhook until the
  trader unlocks it (`unlock_webhook`), and each counts as a failed
  authentication against the caller.

How a trader makes the desktop reachable from TradingView (tunnel or LAN) is
in [`../user/troubleshooting.md`](../user/troubleshooting.md).

## Broker-backed books

```rust
strategy_orderbook(strategy, run_id) / strategy_tradebook(...) / strategy_positions(...)
strategy_detail(strategy) -> Value   // adds realized P&L per mode
```

These read the account's own books and filter them to the strategy: for
money the broker is the authority, and the order rows decide only which
broker rows belong here. The book comes from the **run's** mode, not the
analyzer toggle: a Sandbox run reads the sandbox books, a Live run the
broker's.

In the desktop, only orders placed in the book's mode are attributed to it
(SM-03): a residual left by a run of the other mode neither stamps a row with
its run nor enters the totals, and the Detail page shows the other mode's
residuals apart (`src/pages/strategy/OtherModePositions.tsx`). The strategy
record carries its realized P&L per mode over every finalised run (SM-05),
and every order row carries its run's mode, so an order outside the 100 runs
a page has loaded is never counted as Sandbox. The web matched owners of
every mode by contract and read runs outside the loaded page as Sandbox.

Positions carry a weaker guarantee: a position row is per contract, so a
contract also held from a manual order or another strategy is shared and
cannot be divided. A strategy's P&L comes from its own fills, never from
these rows.

## Contract resolution

`strategy/resolver.rs`. Before a batch run starts, every leg becomes an exact
contract the master contract confirms, with that contract's own lot size.
Failure is a value with a machine-readable `code`, never a plausible
substitute.

- **A lot size of zero or less is a refusal**, never a silent 1.
- **A strike stays a float.** `VEDL25APR24292.5CE` is a real contract.
- **The monthly rank is read off the data** as the last expiry within its
  calendar month, not off a weekday.
- **A commodity option has no spot.** On MCX (and CDS, BCD, NCDEX, NCO) the
  underlying is the nearest unexpired future.
- **`lot_size_for` is anchored**: the row must be the contract exactly or the
  base followed by the expiry, so `GOLD` never takes `GOLDM`'s lot size.
- **`resolve_quantity` makes lots mean lots** and stores the lot count, so a
  strategy survives an exchange revising the lot size; an unknown lot size in
  lots mode is an error.
- **`contract_exists` checks every signal leg**, on every venue including
  cash; it answers true when the master has no rows for that venue at all, so
  a strategy is not blocked merely because the master contract has not been
  downloaded yet.
- **The underlying is priced once per basket**, so two legs of one spread
  settle around the same ATM strike.

## Startup

`StrategyModule::start` (called from `lib.rs` `start_services`), in an order
that closes a window at each step:

1. **Order updates are subscribed first**, when the app state is built
   (`strategy::register`), so a fill arriving while recovery reconciles order
   rows is applied rather than lost.
2. **Recovery**, before any price arrives.
3. **Price subscriptions for the recovered runs**, then the tick reader and
   evaluator.
4. **Checkpointing**, once there is state worth snapshotting.
5. **The scheduler last**, because it can start new runs and must not do that
   until recovery has decided what is already running.

A recovery failure is logged and the module still starts. Shutdown cancels
every owned task, waits for them, and releases every price subscription.

## HTTP surfaces

**Session API** (`server/routes/strategy_module.rs`, signed-in user, same
paths as the web blueprint): `/strategy/api/strategies` list and create;
`/strategy/api/strategies/{sid}` detail, PATCH and DELETE; `webhook/rotate`,
`live`, `kill_switch`; lifecycle `start`, `stop`, `close_all`,
`legs/{leg_id}/close`, `unlock_webhook`; and the read-only views `runs`,
`orders`, `events`, `webhook_events`, `orderbook`, `tradebook`, `positions`,
`checkpoints`. The kill switch locks the webhook and stops the current run.

**API-key surface** (`server/api_v1/strategy.rs`, nine POST routes under
`/api/v1/strategy/`): `list`, `status`, `start`, `stop`, `close_all`,
`close_leg`, `runs`, `orders`, `events`. Request validation, response shapes
and status codes follow the web's `restx_api/strategy.py` and are documented
in [`../api/strategy-services/`](../api/strategy-services/). Nothing there
can create, edit, opt a strategy in to Live, rotate a token or delete.

Both enforce the web's properties:

- **Unknown fields are refused, never dropped**: a misspelt `overall_sl_mtm`
  must not save as a strategy with no stop. Enums are matched
  case-insensitively and returned canonically, so a PATCH can re-validate the
  whole merged configuration.
- **`strategy_kind` is settable at create and never updatable**; a request to
  change it is refused by name.
- **`mode` is required on start** and has no default anywhere in the chain.
- **Live is opt-in per strategy**, checked at every caller and again as the
  last point before real orders.
- **A strategy that is not yours answers 404, never 403**, identical to one
  that does not exist.
- **No response carries a webhook token.**

Limits, as on the web: 1 to 10 legs, at most 50 lots per batch leg, at most
1,000,000 units for cash, at most 20 allowlist entries.

## Live updates

A page watching one strategy emits `strategy_subscribe` on the shared
Socket.IO connection and joins the room `strategy:{id}`. Six frame kinds, each
with the envelope `type`, `strategy_id`, `run_id`, `ts` (IST ISO 8601) and
`ts_ms`:

| Event | Adds |
|---|---|
| `strategy_snapshot` | live figures and every leg |
| `strategy_delta` | live figures and the open legs (throttled to 100 ms) |
| `strategy_event` | an event row |
| `strategy_order_update` | an order row |
| `strategy_run_update` | a run row |
| `strategy_terminal` | `stop_reason`, `pnl_realized` |

- **Ownership is checked on the join**; a strategy that is not yours and one
  that does not exist are refused identically.
- **Frames are ordered by `ts_ms`** in the frontend, so a late delivery is
  dropped rather than winding numbers backwards.
- **The 5 second REST poll remains as a fallback** while the socket is not
  delivering (`src/api/strategy_module.ts`, `useStrategyLive`).
- Nothing in the push channel is load-bearing for money: every push swallows
  its own failure, and the throttle map is bounded.

## The order path shared with other automation

These rules are not part of `src-tauri/src/strategy`, but a strategy author
meets them on the same order path. They are listed here so they are not
attributed to the strategy module.

### Semi-Auto and the Action Center

With the API key's order mode on Semi-Auto, an order from an API client
(`/api/v1/placeorder`, smart, basket, split, options, options multi, GTT) is
not executed: it waits in the Action Center for the trader
(`services/order_router.rs`). Strategy orders never wait there (they are
`internal`), and neither do the app's own calls for the trader: page actions,
approved orders, strategy and OpenScript Stop exits, scalping protection.

In the desktop, an order mode that cannot be read is a refusal, not
"execute" (`order_service::semi_auto`, NEW-TR-05, PR #163): every queueable
order and every call Semi-Auto refuses an API client (modify, cancel, cancel
all, close position, GTT modify and cancel) answers HTTP 500 with
`status: error` and "OpenAlgo could not read whether your orders wait for
approval in the Action Center (Semi-Auto), so nothing was sent. Try again in a
moment; if it keeps happening, restart OpenAlgo." The web fails open and
sends the order. Because strategy orders never read the order mode, an
unreadable mode cannot hold a strategy's stop.

### Live runs from the /trading terminal (OpenScript)

OpenScript Live runs (`trading/runner/`) are a different runner from this
module, but they share the order path and the ownership model:

- **A Semi-Auto approval is followed by its run** (LOG-04, PR #147). An order
  a Live OpenScript run placed while Semi-Auto held it keeps the pending
  order's id; on every refresh the run reads the Action Center's record:
  approved and placed binds the row to the broker order (polled, counted and
  cancelled on Stop like the run's own); rejected or refused marks it
  rejected; approved with no definite answer marks it unconfirmed for the
  order book to settle. The queued order keeps its destination (`force_live`),
  so an approval sends it to the live broker whatever analyzer mode says then.
  The web records an empty order id for such an order and never follows it.
- **Bound to the broker account it was opened on** (PRs #148 and #165): every
  Live order, cancel, status read and book read of an OpenScript run goes
  only through its own session, and each order row records the run's
  incarnation, broker and account.
- **Close and stop answers `held_elsewhere`**
  (`POST /openscript/runner/close/<name>`): what the deployment still holds or
  has working at another broker or account, or from before the account was
  recorded. A non-empty list means the close was not the whole story. The
  status answer carries the same list as `elsewhere`. This field is a desktop
  addition; the strategy module's Stop does not return it (it refuses with
  the ownership message above instead).

Scalping protection stops record the same owner
(`scalping_sl_state.broker`, `broker_account`) and are not exited through
another account's session; an order a Live run queued in the Action Center
records it too (`pending_orders.broker`, `broker_account`) and its approval is
refused under another session.

## Differences from OpenAlgo web

Each is described in its section above.

- Live runs record the broker account and are never exited through another
  broker's or account's session; the broker switch guard refuses a switch
  while Live obligations remain.
- Placements have three outcomes; an uncertain one is `unconfirmed` and is
  settled by the order reconciler, which also pauses new entries while a gap
  in order facts is open.
- Brokers that publish no order rate are paced at 10 orders a second; a
  request whose turn is more than 10 seconds away is refused before sending.
- An unreadable Semi-Auto order mode refuses API-client orders instead of
  executing them (strategy orders never consult it).
- Every price is decided on arrival instead of queued in one FIFO that
  sheds; the stream-to-polling switch is not reported as events, and there
  is no `tick_stale` stop. No `STRATEGY_TICK_*` environment variables.
- A start and an edit are refused when the strategy changed in between
  (`revision`).
- A stop or close that cannot read its own records answers 500, not "not
  running".
- Books attribute orders to Live or Sandbox by the run that placed them.
- Webhooks are refused while OpenAlgo is locked; the IP allowlist applies to
  devices on the network only; ten allowlist refusals in 10 minutes lock the
  webhook.
- Missed schedule slots after sleep are run late (a square-off in the same
  session) or reported (`schedule_missed`).

## Defects deliberately not carried over

The same list as the web, each pinned by a test in
`src-tauri/tests/it/strategy/`. Most carry a `PORTED DEFECT` comment; 6 is
`checkpoints_are_pruned_to_the_newest` and 9 is
`a_rejected_exit_can_be_retried_rather_than_looking_like_a_duplicate` and
`a_refused_dispatch_releases_the_claim`.

1. Signal legs evaluated with an inverted sign (no recorded side read as a
   short).
2. A rule-driven exit on a short doubling the position (exit side from
   configuration).
3. Run P&L summed from a stale per-leg field.
4. Peak and trough persisted as zero on most stop paths.
5. An intraday strategy with `exit_time` but no `auto_stop_time` never
   squared off.
6. Checkpoints never pruned.
7. Two disagreeing order-status normalisers.
8. A repeated exit alert reversing a signal position.
9. A refused exit disabling its own leg permanently.

## Not built yet

- **Resolving an expiry rank on a signal leg.** A signal leg must name the
  contract itself; a base symbol with an expiry rank is refused rather than
  resolved.
- **A joined Socket.IO room outliving the session that joined it.** Ownership
  is checked on the join and not re-checked later; low risk on a single-user
  app.
- **`trigger_source` for API starts.** An API-key start records `manual`, as
  on the web.
- **Account-level RMS caps across strategies.** Not in the desktop either.
  The web's idea of a Flow `riskGuard` node on the shared risk core is not
  built on the web either, and the desktop's Flow (from 1.0.5) has no such
  node: a workflow's orders take the same `/api/v1` path as any API client's,
  with no strategy-level stop or target of its own.
- **Webhook audit and IP-allowlist controls on the strategy page.** The
  session API serves webhook audit rows and accepts `webhook_ip_allowlist` on
  create and PATCH, but the Detail page does not show the audit and the
  wizard has no allowlist editor, as on the web.
- **Stream-to-polling source events** (see [Tick feed](#tick-feed)).

## Related documents

- [`../api/strategy-services/`](../api/strategy-services/): the public endpoint reference
- [`order-constants.md`](order-constants.md): exchange, product, price-type and action codes
- [`symbol-format.md`](symbol-format.md): the symbol format every leg resolves to
- [`websockets-format.md`](websockets-format.md): the market-data and order-update protocols
- [`../port/order-outcomes.md`](../port/order-outcomes.md): accepted, refused and uncertain placements per broker
- [`../user/brokers.md`](../user/brokers.md): broker switching and account ownership, for traders
- [`../user/sandbox.md`](../user/sandbox.md): analyzer mode
- [`../user/troubleshooting.md`](../user/troubleshooting.md): sleep, tunnels and webhooks
