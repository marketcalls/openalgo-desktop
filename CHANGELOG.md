# Changelog

All notable changes to OpenAlgo Desktop are recorded here. Versions follow
[Semantic Versioning](https://semver.org/).

## 1.0.0

The first release of OpenAlgo Desktop: a single-user desktop version of
OpenAlgo web for Windows, macOS, Linux and Raspberry Pi, with no server to set
up, no Python runtime and no `.env` file.

### Compatibility with OpenAlgo web

- `/api/v1` on `http://127.0.0.1:5000` at parity with the web, field for field,
  through a rewritten Rust service layer; pinned by contract tests against
  responses recorded from a live web instance.
- Market data WebSocket on `ws://127.0.0.1:8765` speaking the web protocol:
  authentication, subscribe and unsubscribe, LTP, Quote and Depth modes, depth
  5, 20, 30 and 50, and the order update stream.
- Broker redirect URLs follow the web convention,
  `http://127.0.0.1:5000/<broker>/callback`.
- The OpenAlgo web frontend carried over nearly verbatim, so the screens look
  and work as on the web.

### Brokers

- All 36 OpenAlgo web brokers, built by sign-in family: Zerodha, Fyers, Upstox,
  Dhan and Dhan (Sandbox), Kotak Securities, Groww, Angel One; the Symphony XTS
  family (5 Paisa (XTS), JainamXts, CompositEdge, RMoney, Ibulls, IIFL, Wisdom
  Capital); the Noren family (Shoonya, Flattrade, TradeSmart, Zebu) and
  Firstock; Arrow, Paytm Money, Pocketful, HDFC Sky, HDFC Securities; 5 Paisa,
  Tradejini, Nubra, IndMoney, IIFL Capital; Alice Blue, Definedge, mStock,
  Motilal Oswal, Samco; and Delta Exchange for crypto, with exact crypto
  quantities and leverage.
- Order books, trade books, positions and holdings return OpenAlgo symbols for
  every broker.
- Broker credentials entered in Profile; several brokers can be saved and
  switched in the app, one connected at a time.
- Broker sessions resume after sign-in until the daily 03:00 IST cut-off.
- Shared symbol master keeping every web column, with smart daily download and
  an in-memory cache.
- Broker streaming feeds with reconnect, a watchdog and capped backoff.

### Trading

- Sandbox mode (analyzer mode) mirroring the web's sandbox: 1 crore starting
  capital, margin blocking, fills from live ticks, weighted-average netting,
  T+1 settlement, MIS auto square-off, GTT and catch-up after the app was
  closed, kept in its own database.
- Strategy module and risk management with a pure risk core that passes the
  web's risk vectors; strategy webhooks for TradingView and GoCharting.
- Chartink strategies and webhooks.
- Action Center: Semi-Auto orders queued for approval.
- The `/trading` charting terminal with custom indicators, OpenScript files and
  live OpenScript runs on the same engine as backtests.
- Scalping terminal with its risk monitor.
- Options and portfolio tools: option chain, Greeks, OI tracker and range, max
  pain, IV chart, IV smile, volatility surface, GEX, gamma density, straddle
  charts, strategy builder and arbitrage.
- Historify on DuckDB with download jobs, scheduler and exports; a store left
  by an unclean exit reopens with the last saved data and an alert.
- Playground, API key management, P&L tracker, leverage settings.

### Alerts and AI clients

- Native Telegram bot with commands, alerts and analytics.
- Native WhatsApp bot with QR and pair-code pairing and alerts.
- Native MCP server with the web's 49 tools, over stdio for Claude Desktop and
  Claude Code and over HTTP at `/mcp`; scoped tokens created on the API Key
  page, per-token limits and an audit log.

### Monitoring and settings

- Live, sandbox, security, traffic and latency logs; Health Monitor.
- Admin pages for freeze quantities, holidays, market timings, diagnostics
  and Remote MCP.
- Server Settings for ports, bind address and access from other devices. A
  port already in use is reported in the app with the fix, including the
  AirPlay Receiver case on macOS.

### Security

- No secrets on disk in usable form: the data key and API key pepper live in
  the OS keychain, with a password-derived fallback where no keychain exists.
- Broker credentials and tokens encrypted with AES-256-GCM, each bound to its
  row and column.
- Passwords and API keys hashed with Argon2id; API keys found through an HMAC
  index with a short cache.
- Signed-in user required on every non-public route; CSRF and same-origin
  checks; strict Content Security Policy; devtools off in release builds.
- Broker sign-in verified by single-use `state`, with account binding for
  brokers whose redirect does not return it.
- Sign-in and API key failures throttled per address; `/api/v1` limited as on
  the web.
- Logs keep API keys, tokens, OAuth codes, passwords and TOTP secrets out.

### Platforms and releases

- Installers for Windows x64 (NSIS, per-user), macOS Apple Silicon and Intel
  (`.dmg`), Linux x64 and Raspberry Pi 64-bit (AppImage and `.deb`), built in
  CI on every platform.
- Release builds attach the installers and a `SHA256SUMS.txt` to a draft
  GitHub release. The installers are not yet code-signed.

### Not included

- Python Strategy Host, Flow, and the pandas backtesters (Portfolio
  Backtester, SIP Backtester, Portfolio Analyzer), which need a Python
  runtime.
- The Agent, planned for a later release.
