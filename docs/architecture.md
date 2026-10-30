# Architecture

How OpenAlgo Desktop is built: one process on the trader's computer that
holds the window, the HTTP server, the market data feed, the services, the
sandbox engine and the databases. Written for contributors; module paths are
under `src-tauri/src/` unless they start with `src/` (the React frontend).

The rules every change follows (the web compatibility contract, the order
path invariants, code health) are in [CLAUDE.md](../CLAUDE.md). This page
describes the shape they apply to.

- [The 30-second version](#the-30-second-version)
- [One process](#one-process)
- [The HTTP server](#the-http-server)
- [The market data feed on 8765](#the-market-data-feed-on-8765)
- [The service layer](#the-service-layer)
- [The event bus and its subscribers](#the-event-bus-and-its-subscribers)
- [The sandbox engine](#the-sandbox-engine)
- [Databases](#databases)
- [The key vault](#the-key-vault)
- [The order reconciler](#the-order-reconciler)
- [The OpenScript runner window](#the-openscript-runner-window)
- [MCP](#mcp)
- [Other modules](#other-modules)
- [How this differs from OpenAlgo web](#how-this-differs-from-openalgo-web)

## The 30-second version

```
Tauri window ---- loads ----> http://127.0.0.1:5000  (the React app, as on the web)
                                     |
  SDK, TradingView, Amibroker, Excel, MCP clients --+
                                     |
                         axum HTTP server + Socket.IO (socketioxide)
                                     |               ws://127.0.0.1:8765 feed server
                               service layer  <---- event bus ----> subscribers
                              /      |       \
                  sandbox engine  broker adapters  symbol master, databases
                                     |
                           broker REST + streaming feeds
```

- **The Rust server owns everything.** Business logic lives in the service
  layer. An HTTP handler or a Tauri command parses the request, calls one
  service and shapes the reply.
- **The frontend talks HTTP and Socket.IO to the local server**, as the web
  frontend talks to Flask. That is what let the web frontend be carried over
  nearly verbatim, and what lets a browser on the same computer open the same
  dashboard. Tauri `invoke` is kept for what only the shell can do.
- **Event driven.** Every order operation publishes an event on the
  in-process bus; logging, Socket.IO pushes and alerts are subscribers.
- **One process, no Python.** The web's Flask app, its WebSocket proxy, the
  ZeroMQ bus between them and its Python bots are all replaced by Rust code in
  one process.

## One process

**Start.** `main.rs` checks for the `mcp` subcommand first: `openalgo-desktop
mcp` runs only the stdio MCP forwarder (`mcp::stdio::main`) and exits; Tauri
never starts and the data folder is never opened. Otherwise `lib.rs::run()`
sets up logging (a fixed filter, `log_filter`), the shell, dialog and updater
plugins, and runs `setup`:

1. **Open the data folder** (`open_data_folder`). `DataFolderLock` takes
   `data-folder.lock`, so a second copy of the app is refused before it reads
   anything. `AppState::open_upgrading` (`state.rs`, `db::upgrade`) refuses a
   folder a newer version wrote, backs up every store before an older folder
   is migrated, opens and migrates the stores, and puts the backup back if
   the open fails ([Data-folder upgrades](port/data-upgrades.md)). A refusal
   opens the start-up page with the reason instead of the app.
2. **Start the services** (`start_services`), in order: the session expiry
   task, the monitor (traffic, latency, bans), the health sampler, wake
   detection, the HTTP server, the strategy module, the trading module
   (OpenScript runs and schedules), Chartink, and the 8765 feed service.
3. **Open the main window** and register the OpenScript runner host.

Everything runs on the Tauri-owned tokio multi-thread runtime
(`tauri::async_runtime`). Every spawned task is owned (a `JoinHandle` or
`JoinSet` aborted on shutdown); see "Resource hygiene" in CLAUDE.md.

**Windows.** Both are built in code; `tauri.conf.json` declares none.

| Window | Loads | Notes |
| --- | --- | --- |
| `main` | `http://127.0.0.1:<port>/` in a release build; Vite on `http://localhost:5173` in a debug build | While the server is not running it shows `src-tauri/bootstrap/index.html` ("OpenAlgo is starting", or the port-in-use page with a **Port** box and **Try again**). Navigation to other local ports is blocked (`commands::main_window_may_load`). |
| `openscript-runner-<n>` | `/openscript-runner.html#run=<id>&token=<secret>` | One hidden window per live OpenScript run: not visible, not in the taskbar, background throttling off, no capability (no IPC). See [below](#the-openscript-runner-window). |

**Tauri commands** (`commands/mod.rs`) are the shell-only actions:
`startup_status` and `retry_server` (the start-up page), `restart_server`
(apply new ports or the LAN setting), `open_external` (an http(s) link in the
system browser), `reset_account` and `start_fresh` (after a native
confirmation, from the main window only), `window_proof` (the per-start proof
that a sign-in comes from the app window, `security::window`) and
`restart_to_update`. Everything else is HTTP.

**Quit.** Closing the main window goes through `commands::quit`: when Live or
sandbox work runs or waits (strategies, OpenScript runs, scalping stops, open
or unconfirmed orders, alerts and schedules), `services::lifecycle::quit`
shows a native confirmation that names it and says the keys lock at quit.
On exit the feed stops, then the HTTP server, then `AppState::shutdown`,
which ends with `security.lock()`. On macOS, Cmd+Q is routed through the same
close. Quit from the Dock, an OS shutdown and a forced quit cannot be asked
about.

**Sleep and wake.** `services::lifecycle::wake` compares the wall clock with
the monotonic clock every second (on Windows, the gap between two looks); a
gap over 30 seconds is a wake. It then reconnects the broker sockets, asks
the order reconciler for a pass and catches the sandbox up. Schedulers act on
every slot since their last look (`clock::ist_slots_between`), so a square-off
passed during sleep runs late in its own session rather than not at all.

## The HTTP server

`server/mod.rs` builds one axum router on the HTTP port (default
`127.0.0.1:5000`). The layers, outermost first:

1. path normalisation (a trailing slash is trimmed);
2. request tracing (method and path only, never a body);
3. CORS with an exact-origin predicate (`server/origin.rs`);
4. security headers: `Referrer-Policy`, `X-Frame-Options: DENY`,
   `X-Content-Type-Options`;
5. the content security policy (`server/csp.rs`; `connect-src` names the
   feed's origin);
6. **caller classification**, once: `server::source::classify` stores a
   `Source` (this computer, a device on the network, or a tunnel). Every
   later check reads that value; nothing parses forwarding headers again;
7. the monitor (`services::monitor`: traffic and latency records, bans);
8. the host check (DNS rebinding);
9. a 1 MiB body limit (raised by the few routes that take more: an OpenScript
   save, a Historify upload);
10. the browser session: cookie, same-origin check, and CSRF on mutating
    requests (`server/middleware.rs`);
11. the Socket.IO layer;
12. the routes.

**Routes.** `server/routes/mod.rs` declares every route in one table with its
access: `Public`, `User` (the signed-in session), `UserJson` (a page
navigation gets the page, a JSON request needs the session) or
`BrokerCallback`. The reviewed list of public routes is a test in
`server/tests.rs`; a route is public only if it is there. `/api/v1/*`
(`server/api_v1/`) is public and authenticated by the API key in the JSON
body, as on the web, with its own rate limit layer
(`server/ratelimit.rs`: 100 a second per address, 10 a second for orders,
moving window, no rate-limit headers). Webhooks are authenticated by their
secret address, broker callbacks by the sign-in they belong to.

**Fallbacks.** An unknown API path gets the JSON 404; a wrong method on
`/api/v1` is 404, as on the web. A GET for any other path is the React app
(`server/spa.rs`), whose route list must match the frontend router.

**The frontend.** `dist/` (built by Vite) is embedded in the binary with
`rust-embed` (`#[folder = "../dist"]`); a debug build reads it from disk. The
React app is the web's, carried over with as few edits as possible
([frontend to backend contract](port/frontend-backend-contract.md) lists
every route and event it uses).

**Socket.IO** (`server/socketio.rs`, `socketioxide`) serves the `/` namespace
on the HTTP port to signed-in browser sessions only; every emit checks the
session again. It carries order, analyzer, master contract, Action Center,
Historify, strategy, scalping, WhatsApp and update events, never market data.
The full list is in
[WebSocket architecture](websocket-architecture.md#socketio-pushes).

**Ports and binding** (`config.rs`). The shipped defaults are 5000 and 8765;
a debug build always uses 5500 and 8766, and a release build does only when
`OPENALGO_DESKTOP_DEV_PORTS=1`. The ports, the bind address and the LAN
setting are stored in the `settings` row and changed on Server Settings
(`/settings/server`): turning LAN access on with a loopback address binds
`0.0.0.0`. The HTTP listener moves after `restart_server`; the feed moves by
itself. A port in use is reported in the app with the fix (the start-up page,
and a health alert for the feed).

## The market data feed on 8765

Two modules, two directions:

- `websocket/` is the **broker side**: `WebSocketManager` owns one broker
  socket in one supervisor task (connect timeout, close-before-reconnect,
  capped backoff with jitter, stall watchdog, re-subscribe on reconnect,
  reference-counted subscriptions) and publishes normalised `FeedEvent`s on a
  bounded broadcast. Each broker's `BrokerFeed` only translates frames.
  `services/broker_runtime.rs` starts the market, depth and order-update
  managers at broker sign-in and tears them down at logout, at the 03:00 IST
  boundary and at shutdown.
- `feed/` is the **client side**, the desktop form of the web's
  `websocket_proxy/server.py`: the listener on 8765, the web protocol
  (`authenticate`, `subscribe`, modes 1/2/3, depth 5/20/30/50, `market_data`
  and `order_update` frames, `ping`, the error frame, the 4401 close), the
  per-client registry and a bounded outbox per client. `FeedService` starts
  it after the HTTP server, watches the settings and restarts it on a port
  change.

The bridge between them (`feed/bridge.rs`) holds one manager subscription per
instrument and mode, whatever the number of clients. Strategies, scalping
stops and the sandbox read the manager's broadcast directly. The full path,
with folding and skips under load, is in
[WebSocket architecture](websocket-architecture.md).

## The service layer

`services/` holds every endpoint's business logic, shared by the HTTP
handlers, MCP tools and Tauri commands. Handlers validate with
`services::schema` (the web's Marshmallow rules), call one function and send
its `core::Reply` as is.

| Purpose | Modules |
| --- | --- |
| Orders | `order_service` (place, smart, modify, cancel, cancel all, close), `batch_order_service` (basket, split), `options_order_service`, `gtt_service`, `ui_order_service`, `order_router` (Semi-Auto queueing to the Action Center), `action_center_service`, `freeze_qty_service`, `order_reconciler/` |
| Market data and symbols | `market_data_service`, `symbol_service`, `search_ui_service`, `master_contract_service`, `market_calendar_service`, `watchlist_service`, `chart_prefs_service`, `alert_log_service` |
| Options tools | `options_service/`, `chain_tools_service`, `strike_profile_service/`, `history_tools_service`, `tools_service`, `arbitrage_service`, `strategy_portfolio_service` |
| Account | `account_service`, `pnl_tracker_service`, `session_account` |
| Sign-in and brokers | `auth_service`, `apikey_service`, `broker_auth_service/`, `broker_config_service`, `broker_settings_service`, `broker_runtime`, `broker_switch`, `live_owner`, `obligations`, `request_budget_service` |
| Sandbox | `analyzer_service`, `sandbox_feed`, `sandbox_export_service`, `analyzer_log_service` |
| Health and logs | `health_service`, `monitor`, `latency_service`, `latency_probe`, `traffic_service`, `security_service`, `error_log`, `order_log_service`, `record_loss`, `strategy_ticks`, `system_info/` |
| Settings, messaging, updates, lifecycle | `settings_service`, `notify_service`, `telegram_admin_service`, `update/`, `lifecycle/` |

**Live or sandbox, decided once.** A service decides the destination once
and records it with the order: in analyzer mode `order_service` sends the
order to the sandbox engine (`ctx.sandbox`), otherwise to the broker. A
caller that has already decided (a strategy exiting a position it opened, a
Live OpenScript run, a scalping stop) passes `Route { force_live }` rather
than reading the toggle again. In Semi-Auto order mode `order_router` puts an
order in the Action Center's `pending_orders` instead of sending it.

**An obligation trades only through its own session.** A Live strategy run,
OpenScript run, scalping stop or Action Center order records the broker and
account it was opened on (`live_owner`); dispatch refuses through any other.
`broker_switch` refuses to end a session while Live automation, unsettled
placements or filling exits depend on it (counted by `obligations`).

## The event bus and its subscribers

`events/bus.rs` is the desktop form of the web's `utils/event_bus.py`, with
the web's topic names (`events/mod.rs`): `order.placed`, `order.failed`,
`order.modified`, `order.cancelled`, `order.update`, `orders.all_cancelled`,
`position.closed`, `basket.completed`, `split.completed`, `options.completed`,
`multiorder.completed`, `sandbox.order_filled`, `sandbox.auto_squareoff`,
`sandbox.t1_settlement`, `analyzer.error`, the `gtt.*` topics,
`master_contract.download`, `cache.loaded`, `session.force_logout`, the
Action Center and Historify topics, and a few desktop additions
(`order.unconfirmed`, `analyzer.mode_changed`, `analyzer.request`,
`broker.connected`, `broker.session_ended`).

- **Each subscriber has its own bounded queue and its own worker task**, so a
  slow subscriber (an alert sender) never delays another (the log), and
  events reach a subscriber in publish order. `publish` never blocks.
- **Two lanes.** Best effort (1,000 per subscriber): a full queue sheds the
  event with a sampled warning, as the web does; the loss is counted and
  raises a `records_not_saved` alert on the Health page
  ([Health monitoring](health-monitoring.md#alerts)).
  Critical (10,000): for subscribers on the money path; a full or closed
  queue, or a panic in the handler, reports a lost order fact, which wakes the
  [order reconciler](#the-order-reconciler).
- **Order updates from an earlier broker session are dropped** (EV-02), so a
  late update never lands on the next session's books.

| Subscriber | Lane | Does |
| --- | --- | --- |
| `events/subscribers/log.rs` | best effort | Order and analyzer logs in `logs.db` |
| `events/subscribers/socketio.rs` | best effort | The web's Socket.IO pushes |
| Telegram and WhatsApp alerts (`messaging/`) | best effort | Order alerts to the linked chats |
| `feed::orders::OrderRelay` | best effort | `order_update` frames to 8765 clients that subscribed to orders |
| Strategy order events and the strategy book (`strategy/`) | critical | Fills and order states for the strategy module |
| Trading logout teardown (`trading/`) | critical | Pauses OpenScript runs at logout |
| Scalping lifecycle (`scalping/`) | critical | Starts and stops the stop monitor with the broker session |
| Order reconciler lifecycle | critical | Restarts the reconciler with the broker session |

A new side effect is a new subscriber, never a line added to the order path.

## The sandbox engine

`sandbox/` mirrors the web's `sandbox/` package, with its own database
(`sandbox.db`), apart from live trading. With analyzer mode on, every order
and account service short-circuits to it before any broker call, and the
replies carry the web's analyze-mode shapes.

| File | Role |
| --- | --- |
| `engine.rs` | The task that executes resting orders from ticks |
| `placement.rs`, `orders.rs`, `execution.rs` | Placing, modifying and filling orders |
| `funds.rs` | Capital (1 crore by default) and margin, netted against opposite positions |
| `positions.rs` | Weighted-average netting, realized P&L |
| `holdings.rs` | T+1 settlement of CNC positions to holdings |
| `squareoff.rs`, `session.rs` | MIS square-off at the exchange times, the 03:00 IST session boundary |
| `gtt.rs`, `gtt/trigger.rs`, `expiry.rs` | GTT orders, contract expiry |
| `catch_up.rs` | Catching up on what was due while the app was closed |
| `quotes.rs`, `services/sandbox_feed.rs` | Prices: ticks from the broker feed, polled quotes for stale symbols |
| `events.rs` | Bus events, published only after the transaction commits |
| `config.rs`, `db.rs`, `mypnl.rs` | Settings, the store, My P&L |

Money is exact decimal arithmetic; time comes from the injected clock
(`clock.rs`) so tests are deterministic. Feed freshness is per symbol, and a
price older than 30 seconds is not filled at. The deliberate differences from
the web's engine are listed under "Sandbox deviations from the web" in
CLAUDE.md; the trader's view is [Sandbox mode](user/sandbox.md).

## Databases

All in the data folder, mirroring the web's isolation. Column names follow
the web's schemas, so fixtures and ported tests line up.

| Store | Holds | Access | Durability |
| --- | --- | --- | --- |
| `openalgo.db` | The account, settings, the key vault, encrypted broker keys and sessions, the API key, strategies, OpenScript and scalping state, the order and obligation journal, the symbol master | r2d2 pool (at most 8), WAL | `synchronous=FULL` |
| `logs.db` | Order and analyzer logs, traffic, latency (`order_latency`), security, errors, health samples and alerts | r2d2 pool (at most 4) | `NORMAL` |
| `sandbox.db` | Sandbox orders, trades, positions, holdings, funds, GTT, P&L history | one connection behind a lock | `NORMAL` |
| `historify.duckdb` | Historify market data | DuckDB, one root connection with a write lock | DuckDB's own |

There is no separate latency database: order latency is a table in
`logs.db`. Durability is chosen per store in `db::sqlite::Durability`; the
web runs everything at `NORMAL`, and the difference is deliberate (ARCH-02).

- **Migrations** are numbered and idempotent (`db/sqlite/migrations.rs` for
  `openalgo.db`, recorded in its `migrations` table; `logs.db`, `sandbox.db`
  and DuckDB have their own). A migration checks before it alters, never
  clobbers a value the trader set, and backfills from existing data.
- **Every store is in `db::upgrade::STORES`**, so it is backed up before an
  upgrade and restored if the open fails. A new store is added there in the
  same change.
- **Writing transactions open IMMEDIATE** (`db::sqlite::write_transaction`),
  and pooled connections retry a busy database (`db::sqlite::retry_busy`).
  A connection is never held across an `.await` on network I/O.
- **The symbol master is replaced in stages** (`db::sqlite::master_swap`):
  `symtoken_next` is filled in short transactions, then one transaction
  swaps the tables, so a 200,000-row download never holds the write lock
  long enough to delay an order journal write.
- **DuckDB** migrations run with `checkpoint_threshold = '0b'` and a
  `CHECKPOINT`; an unreplayable log is moved aside at open and raises a
  health alert. CLAUDE.md, "Data and migrations", says why.

## The key vault

There is no `.env`, no plaintext secret on disk and no OS keychain (KEY-01).
A random data key (AES-256-GCM) and the API-key pepper are wrapped with
Argon2id(the OpenAlgo password) in the one-row `key_vault` table of
`openalgo.db`, beside the password hash. A copied database is useless without
the password.

- **Unlocked while signed in.** `SecurityManager` (`security/mod.rs`)
  installs the keys with the session a sign-in issues, and drops and zeroes
  them at logout or quit, clearing every cache. Until the trader signs in
  after a start, `/api/v1` answers 503 "Sign in to OpenAlgo...", and `/mcp`,
  the feed and the webhooks refuse the same way; broker sessions do not
  resume and the bots do not start.
- **What it protects.** Broker keys and sessions, the TOTP secret, the API
  key (for the API Key page), the Telegram bot token and the WhatsApp
  session are encrypted with the data key, each bound to its row and column
  as associated data. API keys are verified through an HMAC index and Argon2,
  with the web's caches.
- **A sign-in tells nothing** to anyone but the app window: every failure is
  the same 401 after the same Argon2 work.

The whole design, the lock and unlock table, the migration of earlier builds
and the cache table are in [No OS keychain](port/no-keychain.md).

## The order reconciler

Order updates are events, and events can be lost: a placement whose answer
never came, a full critical queue, a relay that fell behind, an order socket
that reconnected while an order filled. `services/order_reconciler/` treats
the broker's order book as the authority.

- **A pass every 5 seconds while an owner holds an open order**, and at once
  after a gap: a lost order fact, the order feed reconnecting, a broker
  session change, a wake. Each pass reads the order book **once per
  destination** (Live through the broker session, Sandbox through the
  engine) and hands that read to every owner: strategies, scalping (the
  monitor and the page's own orders) and OpenScript runs.
- **An order with a broker id** is folded through the owner's own fill path,
  idempotent on cumulative quantities.
- **An unconfirmed placement** (sent, with no definite answer: the third of a
  placement's three outcomes, `brokers/common/outcome.rs`) is never placed
  again. It is looked up by client tag where the broker reports tags
  (`Broker::reports_order_tags`), else by its order fields among orders
  nobody owns. Absent, it is judged by `AbsenceTracker::judge`: the sandbox
  releases on a confirmed absence; Live never releases on absence alone and
  waits for the trader's "Not placed" on the Health page or for
  corroboration.
- While a gap is open, new entries pause; exits continue.

The rules and the per-broker status are in
[Order placement outcomes](port/order-outcomes.md); what the trader sees is in
[Health monitoring](health-monitoring.md#unconfirmed-orders).

## The OpenScript runner window

OpenScript runs on the TypeScript engine (`openalgo-script`) that the chart
and the backtest use, so backtest and live cannot drift. There is no Rust
OpenScript engine.

- **Backtests** run in a Web Worker in the page
  (`src/lib/trading/backtestWorker.ts`, entry `backtestWorkerEntry.ts`).
- **A live run** runs the compiled program in a hidden window the shell opens
  for it (`trading/runner/window.rs`, `RunnerHost` in `host.rs`). The page is
  `openscript-runner.html` with `src/runner/main.ts`. It holds no API key and
  no session: it gets a per-run secret in its URL fragment and talks only to
  the runner's host routes, `/openscript/runner/host/<run>/...`
  (`server/routes/openscript_runner.rs`), with the `X-Runner-Token` header.
- **What feeds it.** Not the event bus and not the 8765 feed: a pump task per
  run in Rust reads history for new bars every 10 seconds and the run's open
  orders every 2 seconds, and queues frames in the run's inbox
  (`trading/runner/inbox.rs`), which the page reads by long poll. A page
  silent for 90 seconds has stopped, and its run is ended.
- **What Rust owns.** Everything that decides: starting, pausing and
  stopping runs (one at a time per deployment, claimed under the lock that
  checks), the side a run trades on (fixed at start; a Live run's orders
  carry `force_live`), turning the page's order intents into orders on the
  platform's own order path, the books per deployment, the Stop that cancels
  working orders and closes the position from the run's own fills, the IST
  schedules, and teardown at logout and shutdown. A Live order the Action
  Center holds for approval is followed through its record
  (`trading/runner/approval.rs`).
- **Identity.** Every order row records its side, broker and account and the
  run's incarnation (`openscript_incarnations`, one per start). A batch of
  intents sent again is answered from its record and never placed twice.

The trader's view is in [OpenScript](openscript.md).

## MCP

`mcp/` is native Rust (`rmcp`). One tool registry (`mcp/catalog/`, 49 tools
with the web's names, descriptions and input schemas, pinned by a contract
test) calls the same `/api/v1` handler stack in process.

- **HTTP** (`mcp/http.rs`): `POST /mcp` and `GET /mcp` (event stream) on the
  HTTP port, authenticated by a bearer token created on the API Key page,
  with read and order scopes, per-token limits, an audit table and a kill
  switch. Callers from another device or a tunnel need Remote MCP on.
- **stdio** (`mcp/stdio.rs`): `openalgo-desktop mcp [--url]` is a thin
  forwarder that relays `tools/list` and `tools/call` to the running app's
  `/mcp`. The token comes from `OPENALGO_MCP_TOKEN` in the MCP client's own
  configuration. It never opens the data folder or a listener, and refuses
  plain `http://` to anything but this computer.

Setup for traders: [MCP](user/mcp.md); the tools: [MCP tool
reference](mcp-tool-reference.md).

## Other modules

| Module | What it is |
| --- | --- |
| `brokers/` | The `Broker` trait and the 38 adapters, by sign-in family; see [Broker integration](broker-integration-guide.md) |
| `strategy/` | The strategy module and its RMS (webhooks, legs, runs, books) |
| `risk/` | The pure stop, target, trailing and aggregate rules: no I/O, every input an argument; the web's `test/risk/vectors.json` is the contract |
| `scalping/` | The scalping terminal's service and stop monitor; see [Scalping](scalping.md) |
| `trading/` | `/trading` support: custom indicator and OpenScript files, instrument facts, the OpenScript runner |
| `chartink/` | Chartink webhooks to orders, with the intraday square-off |
| `historify/` | Historify: DuckDB history store, download jobs, schedules |
| `messaging/` | Native Telegram (the Bot API) and WhatsApp (`whatsapp-rust`) bots and alerts |
| `analytics/` | Pure options and portfolio maths for the tools |
| `session/` | Browser sessions, the 03:00 IST boundary, the expiry task |
| `security/` | The key vault, password hashing, the window proof |
| `db/` | SQLite and DuckDB stores, migrations, upgrades |
| `updater.rs`, `services/update/` | The in-app updater (notify only; see [In-app updates](release/updates.md)) |
| `clock.rs` | The injected clock and IST helpers |
| `cache.rs` | `TtlCache`, the bounded TTL cache with the web's invalidation rules |
| `error.rs` | `AppError` and `client_message`, the trader-facing text |
| `e2e.rs` | Test-only build features for CI's native end-to-end run; never shipped |

## How this differs from OpenAlgo web

| | OpenAlgo web | OpenAlgo Desktop |
| --- | --- | --- |
| Processes | Flask under Gunicorn (one eventlet or gthread worker), the WebSocket proxy, Python bots | One Rust process with a tokio runtime |
| Adapter to proxy | ZeroMQ on port 5555 | Bounded in-process channels |
| Configuration | `.env` | Settings in `openalgo.db`, changed in the app |
| Secrets | `APP_KEY` and `API_KEY_PEPPER` in `.env` | Generated at setup, wrapped with the password in `key_vault` |
| Users | One per deployment | One per installation, one broker session at a time |
| Strategies in Python, pandas backtesters | Present | Not in the desktop |
| Flow (`/flow`) | Python executor; APScheduler jobs in server-local time | Native Rust (`src-tauri/src/flow/`); schedules in IST, run while the app is open; nodes call `/api/v1` in process |
| Agent | Python, on agno | Native Rust (`src-tauri/src/agent/`), from 1.0.4 |
| OpenScript live runs | In the browser page that started them | In a hidden window per run, fed by Rust |
| Order outcomes | Accepted or failed | Accepted, refused or uncertain, settled by the order reconciler |
