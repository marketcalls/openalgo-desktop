# CLAUDE.md

Guidance for Claude Code working in this repository. This file carries what is
**not discoverable by reading the code**: product context, invariants, runtime
constraints and conventions. Structure and commands are discoverable; read them
from the repo.

## Overview

OpenAlgo Desktop is a **single-user, cross-platform desktop port of OpenAlgo
web** (https://github.com/marketcalls/openalgo, about 2 lakh users). It is built
with Tauri 2, a Rust backend and the OpenAlgo React frontend.

One person installs it on their own machine, signs in, connects one broker and
trades. There is no server to set up and **no `.env` file**: every credential
and setting is entered and managed inside the app.

Target platforms, all first-class:

| Platform | Notes |
| --- | --- |
| Windows 10/11 x64 | NSIS installer, per-user install |
| macOS Intel and Apple Silicon | `.dmg` |
| Linux x64 | AppImage and `.deb` |
| Raspberry Pi (Linux aarch64) | AppImage and `.deb`. Every native dependency must compile on ARM64 Linux; CI builds it. |

OpenAlgo web users run on desktops, Raspberry Pi, Linux servers and Macs. A
change that only works on the machine it was written on is not done.

## Skills

Detailed procedures live in `.claude/skills/` and load on demand. Use the
skill instead of improvising the procedure:

- **`broker-integration`**: adding or changing a broker (Broker trait, auth
  families, catalog, mapping to OpenAlgo symbols, feeds, fixtures, sign-in
  tests).
- **`web-sync`**: carrying recent OpenAlgo web commits over to the desktop.
- **`fd-audit`**: after any change touching databases, sockets, feeds, spawned
  tasks, caches or registries; also for "too many open files" or rising memory.
- **`verify`**: before claiming a control holds, a bug is fixed or a test
  guards a fix.
- **`security-audit`**: the periodic security review and before a release.
- **`parallel-work`**: running several agents or branches at once, merging
  through a worktree, migration numbering, pausing work.
- **`version-bump`**: changing the app version and preparing a release.
- **`chart-indicator`** and **`openscript`**: writing a custom chart indicator
  or an OpenScript study for the `/trading` terminal.

## The compatibility contract

**Anything that works against OpenAlgo web must work against OpenAlgo Desktop
unchanged.** The Python SDK, TradingView, Amibroker, Chartink, GoCharting,
Excel and MCP clients are written against the web's wire format. The desktop is
a drop-in replacement for them.

- **`/api/v1/*` on `http://127.0.0.1:5000`** accepts the same requests and
  returns the same responses as the web, field for field: key names, status
  strings, flat versus `{status, data}` envelopes, error shapes, HTTP codes,
  number types, date and timestamp formats.
- **The WebSocket feed on `ws://127.0.0.1:8765`** speaks the web protocol:
  `authenticate`, `subscribe` / `unsubscribe` / `unsubscribe_all`, modes 1/2/3,
  depth 5/20/30/50, `market_data` frames, the order-update stream, `ping`, the
  error frame and the 4401 auth-timeout close.
- **The UI** looks and behaves like the web. The frontend is carried over from
  the web nearly verbatim; it is not a redesign.
  - **Approved deviation (maintainer, 2026-10-12): broker keys and the login
    flow follow OpenBull**, not the web. One key set per broker, for as many
    brokers as the trader likes, edited on Broker Configuration
    (`/broker/config`, `pages/BrokerConfig.tsx`); Select Broker (`/broker`)
    is a card grid with an Active / Configured / Not Configured badge; the
    sign-in page is one card that goes to `/dashboard` when the broker
    session resumes and to `/broker` otherwise; each form broker logs in on
    a small card (`/broker/<broker>/totp`); the Profile Broker tab is a
    summary. **One broker is logged in at a time**: logging in to another
    asks first, and a completed sign-in always ends any other broker's
    session (`BrokerAuthService::session_lock`). New keys for the logged-in
    broker end its session too, after the same confirm (a session belongs
    to the keys it signed in with); one account per broker is by design.
    Every way a session ends for a switch is refused while Live
    automation, unsettled placements or filling exits depend on it
    (`services::broker_switch`, fed by the order-outcomes service).
    The keys each broker needs
    and their labels come from `brokers/setup_guide.rs`, served by
    `GET /api/broker/list`; the pages keep no broker list. Unchanged: route
    names, `/<broker>/callback` and the default redirect URL, `/api/v1`, the
    feed, the SDK and MCP contracts, and the desktop's sign-in security
    (`docs/codex/18-openbull-login-design-comparison.md`: never OAuth
    `state` as an access token, never raw callback queries in a log, masked
    display apart from replacement inputs).

The contract is defined by the web, not by this repo. Sources, in order of
authority:

1. Golden fixtures recorded from a live web instance, in `tests/fixtures/`
   (REST request/response pairs and WebSocket transcripts). A contract test
   that disagrees with a fixture is a desktop bug.
2. The web docs (the web repo is read-only reference; never edit it):
   `openalgo/docs/api/**`, `docs/prompt/services_documentation.md`,
   `docs/prompt/symbol-format.md`, `docs/prompt/crypto-symbol-format.md`,
   `docs/prompt/order-constants.md`, `docs/prompt/websockets-format.md`,
   `docs/prompt/openalgo python sdk.md`.
3. The web implementation: `restx_api/*.py` and `restx_api/schemas.py` for
   request validation, `services/*.py` for response dicts,
   `websocket_proxy/server.py` for the streaming protocol.

Observed quirks that are part of the contract and must be reproduced, not
"fixed":

- The API key is read from the JSON body. The `X-API-KEY` header is not honoured
  by `/api/v1` endpoints on the web; a client sending only the header gets the
  400 "missing field" error there, and gets the same here.
- `optionsymbol`, `optionchain`, `optiongreeks`, `syntheticfuture` and
  `openposition` return flat objects; `multiquotes` uses `results`.
- Validation errors carry `message` as an object of field errors; business
  errors carry a string. Invalid API key is 403. Wrong HTTP method is 404.
- `/expiry` returns `DD-MMM-YY`; `/optiongreeks` returns `DD-Mon-YYYY`; history
  and ticker candles are epoch seconds; holidays, timings and WebSocket
  timestamps are epoch milliseconds.
- Rate limit is 100 per second per IP, moving window, no rate-limit headers.

## Ports

| Listener | Shipped default | Development on the maintainer's Mac |
| --- | --- | --- |
| HTTP (UI, `/api/v1`, broker OAuth callbacks, `/mcp`) | `127.0.0.1:5000` | `127.0.0.1:5500` |
| WebSocket feed | `127.0.0.1:8765` | `127.0.0.1:8766` |

The maintainer runs OpenAlgo web on 5000 and 8765 on the same machine, so local
testing uses the development ports. The shipped default stays 5000 and 8765 so
`http://127.0.0.1:5000/dashboard`, SDK base URLs and broker app redirect URLs
(`http://127.0.0.1:5000/<broker>/callback`, byte-identical to the web
convention) carry over from web to desktop untouched. Both ports, the bind host
and the LAN toggle are changeable in-app. Loopback is the default bind; binding
beyond loopback is an explicit user choice.

Debug builds always use the development ports. A release build uses them only
when `OPENALGO_DESKTOP_DEV_PORTS=1` is set. The only other environment variable
is `OPENALGO_MCP_TOKEN`, read by the `mcp` subcommand alone: the MCP client
(Claude Desktop, Claude Code) sets it in its own configuration, the token is
created on the API Key page, and it is never a command-line argument, never
logged and never a `.env` file. Besides these the app reads only
`APPIMAGE`, on Linux: the AppImage runtime sets it to the `.AppImage` file
being run (an OS-runtime variable, never set by the trader, carrying no
secret), and the MCP client configuration names that file instead of the
temporary mount the binary runs from. The app reads no other variables.

A port already in use (macOS AirPlay holds 5000 on many Macs) must be reported
to the user in the app with the fix, never only logged.

## Architecture

```
Tauri window ---- loads ----> http://127.0.0.1:5000  (React app, same as web)
                                     |
  External clients (SDK, TradingView, Amibroker, MCP) --+
                                     |
                         axum HTTP server  +  Socket.IO (socketioxide)
                                     |                 ws://:8765 feed server
                               service layer  <---- event bus ----> subscribers
                              /      |       \
                  sandbox engine  broker adapters  symbol master / DBs
                                     |
                           broker REST + streaming feeds
```

- **The Rust server owns everything.** Business logic lives in the service
  layer, never in a Tauri command or an HTTP handler. Commands and handlers
  validate, call a service, and shape the response.
- **The frontend talks HTTP and Socket.IO to the local server**, like the web
  frontend talks to Flask. Tauri `invoke` is reserved for things only the shell
  can do (window control, opening the browser, OS keychain prompts). This is
  what lets the web frontend be carried over with minimal edits, and what lets a
  browser open the same dashboard.
- **Event driven, like the web.** Every order operation publishes an event on
  the in-process bus (topics mirror the web's `events/` package: `order.placed`,
  `order.failed`, `order.modified`, `order.cancelled`, `order.update`,
  `position.closed`, `orders.all_cancelled`, `basket.completed`,
  `split.completed`, `sandbox.order_filled`, `sandbox.auto_squareoff`,
  `sandbox.t1_settlement`, `analyzer.error`, GTT topics). Subscribers do the side
  effects: logging, Socket.IO pushes (`order_event`, `order_update`,
  `analyzer_update`, `cache_loaded`, `master_contract_download`), alerts. The UI
  refreshes on those pushes; it does not poll. A new side effect is a new
  subscriber, never a line added to the order path.
- **Databases**, mirroring the web's isolation: main SQLite, logs SQLite,
  `sandbox.db` (fully isolated from live), latency, and DuckDB for Historify.
  Column names follow the web schemas so fixtures and ported tests line up.

### Brokers

All 36 web brokers are in scope, Delta Exchange (crypto) among them. They are built by
**auth family**, not one by one:

| Family | Implementation | Members |
| --- | --- | --- |
| Noren / Finvasia | generic `NorenBroker<Config>` with hooks | shoonya, flattrade, tradesmart, zebu (firstock is separate: own JSON transport) |
| Symphony XTS | generic `XtsBroker<Config>` | fivepaisaxts, jainamxts, compositedge, rmoney, ibulls, wisdom, and others per the audit |
| OAuth redirect | per broker | zerodha, upstox, fyers, dhan, groww, arrow, paytm, aliceblue, definedge, pocketful, hdfcsky, hdfcsecurities |
| Direct login + TOTP | per broker | angel, kotak, mstock, motilal, samco, tradejini, fivepaisa, nubra, indmoney |
| Bespoke | per broker | iiflcapital (REST + MQTT), deltaexchange (HMAC, `CRYPTO` exchange, leverage) |

Priority order: zerodha, fyers, upstox, dhan, kotak, groww, angel, then the
families. The broker's only job is translation between its shapes and the
OpenAlgo common symbol, order and streaming formats. **Every book (orders,
trades, positions, holdings) returns OpenAlgo symbols, never broker trading
symbols**, or smart orders and close-position silently mismatch.

The symbol master keeps every web `SymToken` column (`symbol`, `brsymbol`,
`exchange`, `brexchange`, `token`, `expiry` as `DD-MMM-YY`, `strike`,
`lotsize`, `instrumenttype`, `tick_size`). Dropping a column breaks expiry
pickers and option chains.

### Sandbox (analyzer mode) is mandatory

Sandbox mode is a core product, not a demo. With analyzer mode on, **every**
order and account service short-circuits to the sandbox engine before any
broker call, and responses carry the web's analyze-mode shapes.

The engine mirrors the web's `sandbox/` package: 1 crore default capital;
margin blocked on placement and netted against opposite positions; MARKET fills
at LTP (bid/ask when available); LIMIT, SL and SL-M fill from live ticks with a
polling fallback when the feed is stale; weighted-average netting with realized
P&L on reduce and average reset on reversal; T+1 settlement of CNC to holdings;
exchange-aligned MIS auto square-off at the web's configured times; the 03:00
IST session boundary; GTT; catch-up after the app was closed. Use exact decimal
arithmetic for money. Time comes from an injected clock so tests are
deterministic.

### Risk rules live in one place

When the strategy module and RMS land, stop, target, trailing and aggregate
rules live in one pure Rust module with no I/O: no database, broker, clock or
logging. Every input is an argument and every decision is a return value.
Consumers translate, they do not decide. The web's `test/risk/vectors.json` is
the contract; the Rust core must pass every vector.

### An order path decides once, under the lock

Learned by the web from defects that reversed real positions; they apply here
unchanged:

- **Claim under the same lock that checks.** The duplicate check and the claim
  marker are written in one hold, before dispatch. A refused dispatch releases
  the claim.
- **Match a fill to the order it belongs to, not to the leg.**
- **A caller that has already decided the destination says so.** Code exiting a
  position it opened passes `force_live` rather than re-reading the global
  analyzer toggle.
- A stop whose exit orders were refused leaves the run open and managed.
- **A placement has three outcomes, never two** (`brokers/common/outcome`):
  accepted (an order id), refused (provably never sent, or the broker said
  no), uncertain (sent, and no definite answer: a timeout or a dropped
  connection after sending, a 5xx, an unreadable reply, a success without
  an id). An uncertain placement keeps its claim, its row is `unconfirmed`
  (never `rejected`), and it is never placed again: only the order
  reconciler (`services/order_reconciler`) settles it from the broker's
  order book, by client tag where the adapter tags orders, else by its
  order fields. An adapter that knows more than the transport (a tag, an
  ambiguous reply) returns `AppError::Uncertain`; never `AppError::Broker`
  with "check the order book".

## Security model

Single user, local machine. Whoever controls the OS account controls the app;
the job is to keep secrets off disk in usable form, out of logs, and away from
anything that is not the signed-in user.

- **No `.env`, no plaintext secrets on disk.** The data-encryption key and the
  API-key pepper live in the OS keychain (macOS Keychain, Windows Credential
  Manager, Linux Secret Service). Where no keychain exists (headless Linux,
  some Raspberry Pi setups), fall back to a key derived from the user's password
  and say so in the UI. A hard-coded or XOR-obfuscated key is not encryption.
- Broker credentials and tokens are AES-256-GCM encrypted with associated data
  binding each ciphertext to its row and column.
- **Secrets never leave Rust**, with two exceptions shared with the web. No
  command or endpoint returns a stored broker secret or password after it is
  saved (broker keys come back masked). The OpenAlgo API key and the TOTP
  enrollment seed (with its QR code) are returned to the signed-in session,
  as on the web, because the carried-over API Key page and Profile TOTP tab
  show them; nothing else receives them, and those answers carry
  `Cache-Control: no-store`. Broker OAuth code exchange happens in Rust
  inside the callback handler, which verifies `state`.
- **Every Tauri command and every non-public HTTP route requires the signed-in
  user.** Public routes are the explicit list: `/api/v1/*` (API key), broker
  callbacks (state-verified), webhook endpoints (secret-verified), static
  assets.
- Strict CSP, `withGlobalTauri: false`, devtools off in release builds,
  `shell:allow-open` scoped to http(s).
- API keys are verified through an HMAC index plus Argon2, with a short cache;
  API-key failures are throttled per network address (never for this
  computer; per key behind a tunnel). Sign-in has the per-address request
  limit plus a failure budget per source (this computer, the tunnel, each
  network address) and account, with a growing delay capped at 5 minutes.
- Logs never contain API keys, tokens, OAuth codes, passwords, TOTP secrets or
  full request bodies of authenticated calls. Wrap secrets in a type whose
  `Debug` is redacted. Release log level is `info`.
- Broker tokens expire around 03:00 IST. The stored session resumes after
  password login until that boundary, then is revoked. Logout revokes it.

## Resource hygiene (no leaks)

The app is a long-lived process: open all trading day, reconnecting broker
feeds through outages. Anything leaked per request, per tick or per reconnect
accumulates until the process dies. This is the Rust form of the web's
`fd-audit` skill; run through it after any change that touches the items
below, and treat a wave as unfinished until it has been done.

**Descriptors**
- One shared `reqwest::Client` per process (or per broker), every request with
  an explicit timeout. Never a client per call.
- SQLite through the pool; connections returned promptly; never hold one across
  an `.await` on network I/O. DuckDB connections closed on every path.
- WebSocket adapters close the old socket before reconnecting, on the error
  path and in retry loops, with capped backoff.
- Every spawned tokio task is owned: a `JoinHandle` or `JoinSet` kept and
  aborted on shutdown, disconnect or broker logout. No fire-and-forget loops.
- Listeners are shut down gracefully on app exit and on port change.

**Memory**
- Every cache has a bound and an expiry. A `HashMap` keyed by symbol, order id,
  request id or client is unbounded unless something evicts it.
- Every subscription has a matching removal that also runs on the error path:
  bus subscribers, Socket.IO rooms, feed subscriptions per client, per-symbol
  registries.
- Channels are bounded. `broadcast` receivers handle `Lagged` instead of
  silently growing or dying.
- Do not retain large payloads (master contract, history, option chains)
  beyond the request that built them.

**Measure, do not just read.** For a suspected leak, drive the path 100+ times
and sample descriptors (`lsof -p <pid> | wc -l`) and RSS (`ps -o rss= -p <pid>`)
before and after. A flat count is proof; a plateau is a cache filling; a line is
a leak. Report a leak with file, line, the exit path that misses the release,
and what bounds it, before fixing.

## Scope decisions

In scope: everything OpenAlgo web offers, including the `/trading` charting
terminal, scalping, the strategy module and RMS, options tools, Historify,
Action Center, Playground, API key management, logs, monitoring, sandbox, all
brokers, MCP.

Out of scope, by the maintainer's decision (2026-10-03), so the desktop needs
no Python runtime:

- Python Strategy Host (`/python`)
- Flow (`/flow`)
- pandas-based backtesters: Portfolio Backtester, SIP Backtester, Portfolio
  Analyzer

**Removed from this version** (maintainer, 2026-10-12): the Python Strategy
Host and the Agent. No code for either ships, and nothing in the UI leads to
them: no route, navigation or menu entry, Admin card, `/trading` side panel
or button, Telegram command. `/python` and `/agent` are unknown pages (the
app's 404 page), `/agent/api/*` is an unknown API path (the JSON 404), and a
saved terminal layout that still names the Agent panel opens with no panel.
A later version that brings either back adds it as a new page, with its
route, SPA path and navigation entry in one change.

Decided:

- **OpenScript** runs on the existing TypeScript engine (`openalgo-script`).
  Backtests already run in a Web Worker. Live runs use the same engine in a
  hidden Tauri window fed by the event bus, so backtest and live cannot drift.
  No Rust OpenScript engine until a cross-engine conformance corpus exists.
- **MCP** is native Rust (`rmcp`): one tool registry mapping to services, served
  over stdio (the app binary with an `mcp` subcommand) and streamable HTTP at
  `/mcp`. Tool names, descriptions and input schemas match the web's MCP server
  and are pinned by a contract test. Full OAuth for remote connectors comes
  later; first version uses a scoped token from the API key page.

- **Telegram and WhatsApp are native Rust** (maintainer decision
  2026-10-07), so they too need no Python runtime:
  - Telegram: the Bot API over the shared HTTP client, same commands (less
    the Python Strategy Host's `/stoppython`), alerts and analytics as the
    web's `python-telegram-bot` service.
  - WhatsApp: depends directly on `marketcalls/whatsapp-rust` (MIT), the Rust
    crate that the web's `wars` package wraps with PyO3. Same pairing (QR and
    pair code), session export/import as an encrypted blob, alerts and bot
    commands as the web. The web's notes on `wars` threading do not apply.
- **Agent: not in this version** (maintainer, 2026-10-12, replacing the
  2026-10-07 deferral that kept its pages with a "not available" message).
  Its pages, the Assistant panel in `/trading`, the `@openuidev` packages and
  the `/agent/api` route group were removed. Working notes for a future
  version, which would be a native Rust Agent with no Python runtime: a
  provider layer of our own (OpenAI chat and Responses, Anthropic Messages,
  Gemini; Ollama and OpenAI-compatible providers through the OpenAI format),
  model list and pricing from LiteLLM's public catalogue with a bundled
  fallback, tools calling desktop services directly. Do not depend on
  `LiteLLM-Labs/litellm-rust`, a proof of concept; revisit if BerriAI ships an
  official Rust crate. The web's frontend for it (`pages/agent`,
  `components/agent`, `lib/agent`, `api/agent.ts`,
  `components/trading/AgentPanel.tsx`, `lib/trading/chartContract.ts`) is the
  starting point to carry over.

## Testing

Testing is a deliverable, not a final pass. A change is done when its tests
are in the same commit.

- **Rust unit tests** for every broker mapping, using recorded broker payloads
  as fixtures; every service against a mock `Broker`; property tests (proptest)
  for symbol parsing and sandbox netting invariants.
- **HTTP contract tests** against the axum server using the golden fixtures:
  same request in, same status and body shape out.
- **WebSocket protocol tests** with a real client against the 8765 server,
  replaying the recorded transcripts.
- **Migration tests** on populated databases, not only empty ones.
- **Web suites re-targeted**: the web's API-level tests that only speak HTTP are
  run against the desktop on the development port.
- **Frontend**: Vitest unit and component tests carried over from the web with
  the pages, axe accessibility tests, Playwright end-to-end.
- **CI** runs all of it on Linux x64, Linux ARM64, macOS and Windows, with
  coverage reported. `cargo fmt --check`, `cargo clippy -D warnings` and
  `biome check` must be clean.

The maintainer's live OpenAlgo web instance can be used to record new fixtures.
Never commit an API key, account id or email into a fixture; use the
`<APIKEY>`, `<USER_ID>`, `<EMAIL>` placeholders.

## Conventions

- **Rust**: `cargo fmt`, `cargo clippy -- -D warnings`. `tracing` for logs;
  errors logged with context once, at the boundary that handles them. No
  `unwrap()` or `expect()` on runtime data paths. Business logic in services.
- **TypeScript/React**: Biome, functional components with hooks, PascalCase
  component files, TanStack Query for server state. Keep files close to their
  web originals so future web changes can be carried over by diff.
- **Every message a user reads is written for a trader, not a developer.** Name
  the cause and the next action. Never show a status code, exception, protocol
  term or endpoint; the technical detail goes to the log.
- **Vocabulary**: "sandbox mode" and "analyzer mode", never "paper trading" or
  "virtual trading". Never "arm", "armed" or "arming" where a trader reads it:
  an alert is Active or Stopped, a destination is Live or Sandbox.
- **No icons or emojis anywhere**: source, comments, logs, commits, PRs,
  changelogs, release notes.
- **Commits**: Conventional Commits (`feat:`, `fix:`, `docs:`, `refactor:`,
  `test:`, `chore:`, `ci:`). Commit and push to GitHub at every checkpoint
  where a module builds with its tests green; nobody else uses this repo yet.
- **Schema changes ship as numbered, idempotent migrations** that check before
  altering, never clobber a user-customised value, and backfill from existing
  data rather than a default.
- **Adding a page**: the route in the frontend router, the same path served by
  the Rust server's SPA fallback, and the navigation entry, in one change.
- **/trading is a full-screen terminal**, as on the web: no app navbar there.
  The navbar and the terminal's toolbar both render
  `src/components/layout/AccountControls.tsx` (broker, Live or Analyze, mode
  switch, theme, profile menu); never fork that logic into the terminal. Every
  toolbar and rail glyph comes from the chart engine through the generated
  `src/lib/trading/chromeIcons.ts` and `drawingToolMetadata.ts` and
  `src/lib/trading/glyphs.tsx`.
- **Bumping `openalgo-charts` is one change** with the `chart-indicator`
  skill and the generated glyph modules. Pin the exact version in
  `package.json` and the lockfile, then run
  `node .claude/skills/chart-indicator/generate-api-index.mjs`,
  `node .claude/skills/chart-indicator/coverage.mjs` (must print COVERAGE
  COMPLETE), `node scripts/generate-drawing-tools.mjs` and
  `node scripts/generate-chrome-icons.mjs` (each takes `--check`), update the
  skill's prose from the upstream changelog (SKILL.md's recent changes and
  the pin in its desktop section, the table in `reference/api.md`), and run
  `npx tsc -b` so the desktop-only chart code is checked against the new
  build. Never edit the two generated files by hand and never run a
  formatter over them: `biome.json` leaves them out, because `--check`
  compares the generator's exact output and a reformatted file fails it.

## Known pitfalls for agents

Each of these cost real time while building the desktop. They are recorded
here so they are not rediscovered. When a new one is found, add it here in the
same commit as its fix.

### The maintainer's machine is shared

- OpenAlgo web runs on 5000 and 8765 on the same Mac, so development uses 5500
  and 8766, and nothing an agent starts may bind 5000 or 8765.
- **Never run the app binary, in any mode or subcommand, against the real data
  folder** (`~/Library/Application Support/com.openalgo.desktop` on macOS). A
  stray run migrates the maintainer's database and once left a Historify log
  that stopped the app from starting. Use the `dev_server` example (temporary
  data folder, in-memory keystore) or a test harness with a temporary folder.
- A test that starts a listener pins its ports first
  (`AppState::pin_listener_ports`, port 0 for an ephemeral one): in a debug
  build every settings reload puts the development ports 5500 and 8766
  back, whatever the test wrote to the settings.
- Only one process can hold 5500 or 8766. An "OpenAlgo could not start, port
  in use" dialog means a dev server or a test child is still running: find it
  with `lsof -nP -iTCP:5500 -sTCP:LISTEN` before anything else.
- Restarting `dev_server` wipes its accounts. A browser tab left open across a
  restart still believes it is signed in and shows empty states such as "No
  API key generated"; reload it.
- Never `pkill` by pattern: it kills other agents' test runs. Stop a process by
  the PID you started or checked.
- `vite` and `vite preview` proxy the server's paths to `OPENALGO_DEV_BACKEND`,
  by default `127.0.0.1:5500`, the development app. The Playwright suite
  points that proxy at a closed port (`playwright.config.ts`, `webServer.env`)
  because it assumes no backend; anything else that starts Vite without a
  backend sets `OPENALGO_DEV_BACKEND` the same way. `scripts/ci/launch_smoke.py`
  starts a release build on 5000 and 8765 and refuses to run outside CI.
- The web repo is read-only. Read upstream changes with `gh api` or `git log`
  in the existing checkout; never pull, check out or edit there.

### One shared build folder

All worktrees share `src-tauri/target` (`CARGO_TARGET_DIR`) to save disk, so
`target/debug/openalgo-desktop` may be another checkout's build at any moment.
A test that runs the binary must give it an empty environment, a temporary
home and data folder and ephemeral ports, hold it in a guard that kills and
reaps it, and stop it if it starts the full app. The `mcp` subcommand is a thin
forwarder and must never open the data folder or a listener.

The maintainer's Mac has 8 GB of RAM. Compiling the main library takes
several GB, so four agents building at once swapped for hours and every build
queued behind the lock. Run at most two agents at a time, with
`CARGO_BUILD_JOBS=4` and one cargo command at a time; iterate with filtered
test runs and run the full suite once before pushing. Only one of them builds
at any moment: two worktrees building the crate in the shared target folder
overwrite each other's build-script output, so each forces the other's
library to recompile and both test runs stretch past an hour. Let one agent
hold the build lane while the other writes code, then swap.

The shared folder grows past 30 GB. When free disk drops below about 8 GB
and no build is running (`pgrep -fl 'cargo|rustc'` is empty), delete
`target/debug/deps` and `build` entries older than the current session and
`incremental`; never the whole folder. Never delete anything under a running
build: removing `incremental` fails it with `failed to move dependency graph`,
and removing a `deps` entry it links against fails the link. If disk is
critical while builds run, remove only per-crate `incremental/*` folders
untouched for hours.

Cargo hashes this crate's build-script output (`target/debug/build/
openalgo-desktop-<hash>/out`, the Tauri capabilities that
`generate_context!` embeds) without the checkout path, so all worktrees share
it, and its `rerun-if-changed` names whichever worktree ran it last. A
worktree on an older base then leaves capabilities another does not match: a
build or the doc tests fail with `capability with identifier <name> not
found` although the file is there. Deleting that unit's
`.fingerprint/openalgo-desktop-<hash>/run-build-script-*` (the hash whose
`build/.../output` names another worktree) and rebuilding at once fixes it
until the older worktree builds again; the lasting fix is rebasing that
worktree onto master. CI builds fresh and is unaffected.

### Builds and CI

- The toolchain is pinned in `rust-toolchain.toml`. A newer clippy fails CI on
  lints the pinned one does not have; bumping the toolchain means fixing those
  lints in the same change.
- A crate used under `cfg(unix)` or one OS needs a matching
  `[target.'cfg(..)'.dependencies]` or `dev-dependencies` entry. `libc` was
  declared for macOS only and broke Linux clippy of the test helpers. A green
  run on the Mac proves nothing for Linux or Windows; CI is the check.
- The interface is embedded with `#[folder = "../dist"]`, relative to the crate.
  `$CARGO_MANIFEST_DIR` in that path is taken literally. Debug builds read
  `dist/` from disk at runtime. CI's Rust jobs use a stand-in page
  (`<!doctype html><title>ci</title>`), so a test may only assume the doctype.
- Windows: a child process needs `SystemRoot` in its environment; DuckDB holds
  an exclusive lock, so open database files cannot be copied in a test;
  timers are coarse.
- Third-party actions are pinned to commit SHAs and Dependabot updates them.
  CodeQL skips test code through `.github/codeql/codeql-config.yml`.

### Tests that pass locally and fail in CI

Every flaky test found so far was a race, not a slow machine:

- **Wait on the event, not on a sleep.** Await the task handle or a completion
  signal, or poll the observable condition with a bound. Retry only a status
  the API documents as retryable (a strategy leg exit answers 409 "retry once
  it fills" until the sandbox entry fills).
- **Fix the product when the race is real.** A Historify retry was claimed
  after its first `.await`, so two concurrent retries both ran; the fix
  claimed it before the await.
- Rate-limit and expiry tests use the limiter's pinnable clock, not the wall
  clock.
- Descriptor checks account for database pool growth: r2d2 refills `min_idle`
  and each SQLite connection holds two descriptors (database and WAL).
- A test that fails about one run in five is a bug. Run a suspect test 10 to
  30 times before calling it fixed.
- Every `AppState` runs the order reconciler's task (a pass every 5 s while
  an owner holds an open order, and at once on a broker session change). A
  test that counts order-book reads accounts for it, or drives
  `ctx.reconciler.pass()` itself.

### Data and migrations

- Migration names are recorded in the `migrations` table. Branches built in
  parallel pick the same next number (074 happened twice). The later merge
  renumbers its migration and the doc comment in its `store.rs`. Never rename
  a migration that has shipped in a release.
- DuckDB 1.5.6 writes a log it cannot replay when a table holding a foreign
  key to a table with a `CURRENT_TIMESTAMP` default is dropped, so a crash
  then bricks Historify. Historify migrations therefore run with
  `checkpoint_threshold = '0b'` followed by `CHECKPOINT`, `close()`
  checkpoints, and opening moves an unreplayable log aside, keeps it and
  raises a health alert. Do not remove any of the three.
- Never hold a pooled SQLite connection across an `.await` on network I/O.

### Sandbox deviations from the web

The sandbox mirrors the web's engine; these deliberate differences fix
defects the web shares. Keep them when carrying web sandbox changes over,
and add each new one here.

- **A close counts as closed only once it fills** (SB-05). A MARKET close
  rests when there is no usable price. Close-all and the auto square-off
  report it as waiting for a price (same response keys as the web), and
  asking again, or the one-minute backup sweep, returns that close rather
  than placing a second exit (the sweep no longer cancels its own resting
  close). The web counted the placed order as closed and placed another
  each time.
- **Margin is checked again when an order fills** (SB-05). An exit placed
  against a position blocks nothing. If the position was closed or reduced
  by the time the exit fills, the fill opens or extends a position, and the
  margin that position needs is blocked then; when the account cannot carry
  it, the order is rejected with the insufficient-funds reason. The same
  check covers a modify that grew an order. The web opened such a position
  with zero margin.
- **Feed freshness is per symbol** (SB-01). The engine records the last
  usable tick of each watched symbol, only after the watched and
  positive-price checks, and its polling fallback quotes just the symbols
  that are stale (plus resting orders on symbols not yet watched). A
  watched symbol that stays stale is subscribed again with a backoff
  doubling to 5 minutes, which also retries a subscription that failed. The
  web keeps one global timestamp, so any tick (a chart's, a watchlist's)
  kept a stalled symbol from ever being polled.
- **An old price is not filled at** (SB-06). Ticks and quotes carry the
  time this process received them, and a fill or GTT trigger waits when the
  price is older than `stale_quote_after` (30 s). The guarantee is receipt
  time only: broker quote timestamps are not part of the quote contract and
  are not relied on. The web has no receipt time.
- **Crypto margin and trade value include the contract value** (SB-03).
  Margin is `notional / leverage` with `notional = |qty| x price x
  contract value` (`funds::notional`), the multiplier P&L already used, so
  10 BTCUSD.P contracts (0.001 BTC each) at 60,000 hold 60 at 10x, not
  60,000. The web multiplies P&L only. Capital stays in rupees and crypto
  prices in dollars, with no conversion; the user docs say so.
- **Crypto books get P&L snapshots every day.** The 23:59 snapshot and
  the catch-up backfill use the trading calendar (weekdays) for the Indian
  exchanges, but a book that held or traded CRYPTO on a day gets that day's
  snapshot, weekends included (`squareoff::snapshot_day`). The web applies
  the trading calendar to every book. CRYPTO has no square-off time, so no
  MIS sweep and no leftover-MIS settlement at the 03:00 boundary touches a
  crypto position.

### Security work

- **Broker sessions change under one lock.** Completing a sign-in, resuming
  one, ending one and forgetting the logged-in broker hold
  `BrokerAuthService::session_lock`, so two sign-ins finishing together
  never leave two sessions (memory, stored rows and streaming disagreeing).
  `revoke` takes the lock; code already holding it calls `revoke_held`, or
  it waits on itself forever.
- **Findings from a background security review are fixed before merge**, on
  the branch. MCP needed three rounds: unbounded fan-out, then caps that
  concurrency could bypass, then overflowing arithmetic and a cap check that
  parsed input differently from the code doing the work. The pattern that
  closed them: parse each input once into a typed value, check limits on that
  value with checked arithmetic, and reserve one shared budget before any
  work starts.
- A route is public only if it is in the reviewed list in `server/tests.rs`,
  and a public route that changes state needs its own credential. A CSRF token
  is not one: `GET /auth/csrf-token` hands it to anyone.
- The session cookie is `SameSite=Lax`, so a cross-site link carries it on
  a GET. A GET with a side effect (the Definedge and Nubra login OTP texted
  as their page opens) runs only for a same-origin navigation
  (`middleware::same_origin_navigation`); otherwise the page offers the
  action as a CSRF-checked POST.
- Accept an advisory only when the vulnerable code provably never runs: an
  `ignore` in `.github/deny.toml` with the reason and a review date, the same
  IDs in `.trivyignore`, and the Dependabot alert dismissed as "not used" with
  the same reason.
- Dismiss a CodeQL alert as "used in tests" only after checking that it sits
  in a `#[cfg(test)]` module. A variable named `iv` that means interval trips
  the hard-coded-crypto rule; rename it.
- Check a fix by disabling it locally and watching its test fail; never
  commit or push the disabled state, not even to a wip branch.
- Behind a tunnel or proxy on the same machine (ngrok, cloudflared) every
  caller is 127.0.0.1. Classify the caller once, in the outermost layer,
  with the one classifier (`server::source::classify`, stored as
  `Source`; the feed's handshake calls it too), and have every check read
  the stored value; a second parser of forwarding headers or `Host` is a
  way around the first. Loopback is local only without any forwarding header and with
  the app's own `Host`. No forwarding header is read as an address: tunnel
  callers are one identity, and bans and IP allowlists apply to network
  peers only.
- Refuse cross-site browser requests (`Sec-Fetch-Site`, a foreign `Origin`
  or `Referer`) before they reach any failure counter or rate limit: a web
  page can fire them blind and use up the trader's budget.
- Every check of an API key, token or webhook address counts a failure
  against the caller's budget (`middleware::failures_exhausted`,
  `count_failure`), however the credential arrives: body, URL, header or a
  WebSocket message. A key in a URL, the `/mcp` bearer token and the
  feed's `authenticate` were each left uncounted once. Check the
  credential first: a spent budget refuses only further invalid attempts,
  never a valid credential, so no stranger can lock one out. Exempt a
  request by refusing it before the check, never by skipping the count.
- Limits and bans keep five availability guarantees, held by one
  table-driven test across every surface
  (`availability_guarantees_hold_on_every_surface`): this computer (a
  loopback peer, never one granted by a list of interfaces) with a valid
  credential or session is never refused by a ban, budget, overflow or
  monitor rule, though it keeps its resource caps; a valid credential from
  any source is never refused because of other callers (check it first);
  bans apply only to devices on the network, never to this computer, the
  tunnel identity or exactly the machine's own addresses, and are capped
  and expire; invalid traffic is
  always bounded (no path admits it unlimited when a table is full);
  password and code budgets stay per source. A new limit or ban is
  checked against all five and added to that test.
- Never charge valid traffic to a low limit strangers share. Tunnel
  callers are one identity, so behind a tunnel a valid credential gets its
  own window (`middleware::limiter_key`) and the shared limit is only a
  generous resource guard. A limiter that drops live entries when its
  table fills is a reset button (invented keys once flushed every
  lockout): evict only request windows, never a failure count inside its
  window; count a foreign IPv6 device by its /64 so rotating addresses
  takes one entry (devices in the machine's own prefix, which every home
  device shares, one by one plus an aggregate for the /64 that the limiter
  charges itself, so no surface can skip it); and once
  every entry is live, put new
  callers in one bounded overflow entry per limit, never evicted, that
  refuses only invalid credentials. None of this is for passwords or
  codes, which have per-source budgets only.
- A failure budget keyed by anything the attacker picks per attempt (a user
  name, a password, a code, a credential hash) or with an evict-oldest
  table can be spread or flushed. Key it by source and account, never evict
  one still counting, and count the attempt under the same lock that checks
  the wait, before verifying (`LoginBackoff::claim`), or parallel requests
  all pass the check.
- An authenticated market data feed client (8765) is closed, with the
  web's `AUTHENTICATION_ERROR` frame and then 4401, whenever the API-key
  cache is cleared (`ApiKeyCache::clear`: a regenerated key, a broker
  session that ended or started), so a client keyed before a sign-out or
  key change cannot keep streaming. The web keeps such clients connected;
  this is a deliberate difference. Anything new that changes what the key
  reaches clears that cache.
- A request without a signed-in session acts only on its own browser
  session. An anonymous `/auth/logout` once cleared every session, so
  anything that could reach the port, a tunnel included, kept the trader
  signed out.
- End a session in memory before writing that to the database
  (`BrokerAuthService::revoke`): a failed write must never leave a broker
  session that trades behind a "Logged out". The failure sets
  `revoke_pending`, which blocks resume until the session poll or the next
  sign-in writes it.
- A GET with a global effect (`/auth/logout` ending every session) runs
  only for a same-origin navigation: the Lax cookie rides on a link from
  another site in browsers without Fetch Metadata.
- A key file opened by a password is trusted only once the password also
  matches the stored hash (`SecurityManager::unlock_for_sign_in`); around a
  password change an old or uncommitted password opens a file but must
  unlock nothing.
- Only account setup creates key material. A keychain that is locked or
  denies access at startup looks like no keychain at all
  (`KeyStoreError::Unavailable`); a sign-in that then wrote a vault with new
  keys locked the account out for good. Keychain writes put the data key
  last and read both keys back; a password change writes the new vault
  beside the old one before the hash changes.
- Fake tokens in fixtures must match the allowlist in `.github/gitleaks.toml`;
  prefer the `<APIKEY>`, `<USER_ID>`, `<EMAIL>` placeholders.

### Working with several agents

- Each agent works in its own worktree and pushes only after `cargo fmt
  --check`, `cargo clippy --all-targets --locked -- -D warnings`, the full
  `cargo test --locked`, `npx tsc -b`, vitest and gitleaks pass on the tree
  being pushed. One unverified push once broke the build on master.
- Merge a feature branch through a temporary worktree on current master and
  rerun the full gates there; the registration files (`state.rs`, `lib.rs`,
  `routes/mod.rs`, `middleware.rs`, the public-route list in
  `server/tests.rs`, `migrations.rs`) conflict often, and both sides are
  almost always kept.
- When pausing, save unfinished work to a `wip/<topic>` branch, never to
  master.
- If GitHub rejects pushes with server errors, a merge through
  `gh api repos/<owner>/<repo>/merges` works; confirm the resulting tree SHA
  matches the tested one.
