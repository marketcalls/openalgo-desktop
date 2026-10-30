# OpenAlgo Desktop Documentation Map

The entry point for traders, contributors and AI agents. This file is a
**map, not a copy**: it points at the documents that already live under
`docs/` and at the root of the repository. Edit a source document once;
everything that reads through this map sees the change.

**How to use it:** read this map, open the one area you need, then open the
one file in it. Do not load everything at once.

OpenAlgo Desktop is the single-user desktop version of
[OpenAlgo web](https://github.com/marketcalls/openalgo): the same screens, the
same `/api/v1` on `http://127.0.0.1:5000` and the same market data feed on
`ws://127.0.0.1:8765`, installed on one computer with no server, no Python
and no `.env` file. Where a page below describes something the desktop does
differently from the web, it says so.

---

## Using OpenAlgo Desktop (product, API, SDK)

| Read this when you need... | Entry point |
| --- | --- |
| What the app is, the supported systems, installing it, first run and where your data lives | [README.md](../README.md) |
| The step-by-step user guide (first sign-in, broker, first order, integrations, tools) | [userguide/README.md](userguide/README.md) |
| To move over from OpenAlgo web: what carries over and what you change | [user/moving-from-openalgo-web.md](user/moving-from-openalgo-web.md) |
| Broker keys, the redirect URL, the three kinds of broker sign-in, switching brokers | [user/brokers.md](user/brokers.md) |
| Sandbox mode (analyzer mode): how orders fill and the limits of the simulation | [user/sandbox.md](user/sandbox.md) |
| The REST API (`/api/v1/`): orders, market data, options, account, streaming | [api/README.md](api/README.md) (every category is listed below) |
| The Python SDK: install, client, data, order and account calls | [<prompt/openalgo python sdk.md>](<prompt/openalgo python sdk.md>) |
| Symbol format across exchanges (equity, futures, options) | [prompt/symbol-format.md](prompt/symbol-format.md) |
| Crypto symbol format (Delta Exchange, the `CRYPTO` exchange) | [prompt/crypto-symbol-format.md](prompt/crypto-symbol-format.md) |
| Order constants (exchange, product, price type, action codes) | [prompt/order-constants.md](prompt/order-constants.md) |
| Contract lot sizes | [prompt/LotSize.md](prompt/LotSize.md) |
| WebSocket subscription and message format | [prompt/websockets-format.md](prompt/websockets-format.md) · [prompt/websockets-verbose-control.md](prompt/websockets-verbose-control.md) · [api/websocket-streaming/](api/websocket-streaming/) |
| Strategy module and risk engine (multi-leg options, signal mode, RMS) | [prompt/strategy_rms_documentation.md](prompt/strategy_rms_documentation.md) · [api/strategy-services/](api/strategy-services/) |
| The charting terminal at `/trading`, its order dock and its shortcuts | [userguide/32-charting-terminal](userguide/32-charting-terminal/README.md) |
| Writing your own chart indicators for `/trading` | [custom-indicators.md](custom-indicators.md) |
| Writing studies and strategies in OpenScript for `/trading` | [openscript.md](openscript.md) |
| Connecting Claude Desktop or Claude Code (MCP), tokens and Remote MCP | [user/mcp.md](user/mcp.md) |
| Every MCP tool, its inputs and what it returns | [mcp-tool-reference.md](mcp-tool-reference.md) |
| Something does not work: ports, broker sign-in, master contract, tunnels, a forgotten password | [user/troubleshooting.md](user/troubleshooting.md) |

### The REST API by category

All categories answer as the web does, field for field. Each folder has one
page per endpoint.

| Category | Folder |
| --- | --- |
| Overview, authentication, error shapes | [api/README.md](api/README.md) |
| Order management (place, smart, basket, split, options orders, modify, cancel, close, GTT) | [api/order-management/](api/order-management/) |
| Order information (order status, open position) | [api/order-information/](api/order-information/) |
| Account services (funds, order, trade and position books, holdings, margin) | [api/account-services/](api/account-services/) |
| Analyzer services (analyzer status and toggle, P&L by symbol) | [api/analyzer-services/](api/analyzer-services/) |
| Utility services (ping) | [api/utility-services/](api/utility-services/) |
| Symbol services (search, symbol, expiry, instruments) | [api/symbol-services/](api/symbol-services/) |
| Market data (quotes, multi-quotes, depth, history, intervals, ticker) | [api/market-data/](api/market-data/) |
| Options services (option symbol, chain, Greeks, multi-option Greeks, synthetic future) | [api/options-services/](api/options-services/) |
| Chart services (chart workspace preferences per API key) | [api/chart-services/](api/chart-services/) |
| Market calendar (holidays, timings) | [api/market-calendar/](api/market-calendar/) |
| WebSocket streaming (the 8765 feed: LTP, Quote, Depth, order updates) | [api/websocket-streaming/](api/websocket-streaming/) |
| Strategy services (the strategy module over the API: list, start, stop, status, runs, orders, events, webhook) | [api/strategy-services/](api/strategy-services/) |
| Telegram services (bot configuration, start and stop, users, broadcast, notify, stats, preferences) | [api/telegram-services/](api/telegram-services/) |
| WhatsApp services (notify) | [api/whatsapp-services/](api/whatsapp-services/) |
| Rate limits | [api/rate-limiting.md](api/rate-limiting.md) |

---

## Install and operate

There is no server install in the desktop. The installers for Windows, macOS
and Linux (x64 and ARM64) replace OpenAlgo web's Ubuntu server install, its
Docker image and its optional gthread web server: one person installs the app
on their own computer, signs in and connects a broker, and every setting is
made inside the app. To run OpenAlgo on a server, a VPS, a Raspberry Pi or in
Docker, use [OpenAlgo web](https://github.com/marketcalls/openalgo)
([docs.openalgo.in](https://docs.openalgo.in)).

| Topic | Entry point |
| --- | --- |
| Supported systems (and what is not supported: headless Linux, Docker, Raspberry Pi, running as a service) | README, [Supported systems](../README.md#supported-systems) |
| Download, check and install; opening the unsigned app the first time | README, [Download and install](../README.md#download-and-install) and [Opening the app the first time](../README.md#opening-the-app-the-first-time-unsigned-installers) |
| Starting, quitting, and what stops when the app is closed or the computer sleeps | README, [Running the app](../README.md#running-the-app) · [user/troubleshooting.md](user/troubleshooting.md#quitting-openalgo-and-when-the-computer-sleeps) |
| Ports 5000 and 8765, changing them, a port already in use | README, [Ports](../README.md#ports) · [user/troubleshooting.md](user/troubleshooting.md#the-app-says-a-port-is-already-in-use) |
| Another device on your network, a firewall, a tunnel for webhooks | [user/troubleshooting.md](user/troubleshooting.md#another-device-a-firewall-or-a-tunnel) · [user/moving-from-openalgo-web.md](user/moving-from-openalgo-web.md#webhooks-from-the-internet) |
| The data folder, backups, and the keys locked with your password | README, [Where your data lives, and backups](../README.md#where-your-data-lives-and-backups) |
| Signing in after a start, a forgotten password, two-factor login (the web's SMTP reset has no desktop counterpart) | [user/troubleshooting.md](user/troubleshooting.md#signing-in-and-a-forgotten-password) |
| Updates: the in-app notice, installing, going back | [user/updating.md](user/updating.md) |
| Settings that lived in the web's `.env` | [user/moving-from-openalgo-web.md](user/moving-from-openalgo-web.md#settings-that-used-to-be-in-env) |
| Broker integration (38 brokers, built by sign-in family) | [broker-integration-guide.md](broker-integration-guide.md) |
| Release notes and changelog | [releases/README.md](releases/README.md) · [CHANGELOG.md](../CHANGELOG.md) |

---

## Feature surfaces

| Feature | Entry point |
| --- | --- |
| Scalping terminal (`/scalping`), its stops and its protection rules | [scalping.md](scalping.md) |
| The Health page (`/health`), unconfirmed orders, alerts, Run checks on the Diagnostics page, feed freshness and skips, database growth | [health-monitoring.md](health-monitoring.md) |
| Custom chart indicators in `/trading` | [custom-indicators.md](custom-indicators.md) |
| OpenScript in `/trading`: backtests and live runs | [openscript.md](openscript.md) |
| Sandbox mode (analyzer mode) | [user/sandbox.md](user/sandbox.md) |
| MCP server for AI clients (stdio through `openalgo-desktop mcp`, and HTTP at `/mcp`) | [user/mcp.md](user/mcp.md) · [mcp-tool-reference.md](mcp-tool-reference.md) |
| Flow (`/flow`): the visual workflow builder, its webhooks and triggers, import and export with OpenAlgo web | [User guide chapter 21](userguide/21-flow-visual-builder/README.md) · [the workflow JSON format](prompt/flow-import-format.md) |
| The Agent: `/agent`, Agent Config under Admin, and the Assistant panel in `/trading` | [The Chart Assistant](userguide/32-charting-terminal/README.md#the-chart-assistant) · [what 1.0.4 brings](../CHANGELOG.md#104) |
| Telegram and WhatsApp (native in the desktop, no Python) | [api/telegram-services/](api/telegram-services/) · [api/whatsapp-services/](api/whatsapp-services/) · [port/messaging-manual-test.md](port/messaging-manual-test.md) |
| The market data feed: how broker ticks reach the app, your scripts and the sandbox | [websocket-architecture.md](websocket-architecture.md) |

Not in the desktop: the Python Strategy Host (`/python`) and the pandas
backtesters (Portfolio Backtester, SIP Backtester, Portfolio Analyzer). The app has no pages for them; README,
[What is included, and what is not](../README.md#what-is-included-and-what-is-not)
says what to use instead. The web's scanner architecture page describes a
separate service built on the web's stack, not a page of the app, and has no
desktop counterpart.

---

## Architecture and contributors

| Topic | Entry point |
| --- | --- |
| First-time contributor setup: dev ports, the `dev_server` example, the shared build folder, the CI gates, code health | [contributing.md](contributing.md) |
| The rules every change follows: the web compatibility contract, invariants, conventions, code health, known pitfalls | [CLAUDE.md](../CLAUDE.md) |
| System design: processes, the axum server, the 8765 feed, services, the event bus, the sandbox engine, databases, the key vault, the order reconciler, the OpenScript runner window | [architecture.md](architecture.md) |
| The market data path in detail: broker feeds, fan-out on 8765, folding and skips under load, Socket.IO pushes | [websocket-architecture.md](websocket-architecture.md) |
| Adding or changing a broker in Rust | [broker-integration-guide.md](broker-integration-guide.md) · the `broker-integration` skill ([.claude/skills/broker-integration/SKILL.md](../.claude/skills/broker-integration/SKILL.md)) |
| Every route and Socket.IO event the frontend uses, page by page | [port/frontend-backend-contract.md](port/frontend-backend-contract.md) |
| Order placement outcomes (accepted, refused, uncertain) per broker adapter | [port/order-outcomes.md](port/order-outcomes.md) |
| The key vault in `openalgo.db`, lock and unlock, caches (KEY-01) | [port/no-keychain.md](port/no-keychain.md) |
| Data-folder upgrades: backup, restore, downgrade refusal (REL-02) | [port/data-upgrades.md](port/data-upgrades.md) |
| Golden fixtures recorded from OpenAlgo web, and the web defects not reproduced | `tests/fixtures/web/INDEX.md` |
| Security residuals: what is reduced but not closed | [security/known-residuals.md](security/known-residuals.md) |
| Audits: the baseline audit of 2026-10-03, performance, soak and fault drills | [audit/2026-10-03/README.md](audit/2026-10-03/README.md) · [audit/perf.md](audit/perf.md) · [audit/soak.md](audit/soak.md) · [audit/fault-drill.md](audit/fault-drill.md) |
| Scanner triage: every open CodeQL alert's disposition, the accepted Rust and Trivy advisories with their dates, the npm full report | [audit/2026-10-22/scanner-triage.md](audit/2026-10-22/scanner-triage.md) |
| CI, the release gate and repository rules | [release/branch-protection.md](release/branch-protection.md) |
| Release checklists: platforms and brokers | [release/platform-qualification.md](release/platform-qualification.md) · [release/broker-certification.md](release/broker-certification.md) |
| How a release reaches installed copies (the updater and its signing key) | [release/updates.md](release/updates.md) |
| Procedures for agents and maintainers: web sync, fd audit, verify, security audit, parallel work, version bump, chart indicators, OpenScript | `.claude/skills/` (one folder per skill, each with a `SKILL.md`) |

---

## Governance

- User responsibilities and risk ownership: https://docs.openalgo.in/responsibilities
- Repository: https://github.com/marketcalls/openalgo-desktop
- OpenAlgo web and its documentation: https://github.com/marketcalls/openalgo · https://docs.openalgo.in
