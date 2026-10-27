# OpenAlgo Desktop - Open Source Algorithmic Trading on Your Own Computer

<div align="center">

[![OpenAlgo Desktop downloads](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fmarketcalls%2Fopenalgo-desktop%2Fstats%2Fbadges%2Fdownloads.json)](https://github.com/marketcalls/openalgo-desktop/tree/stats)
[![OpenAlgo Desktop downloads per month](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fmarketcalls%2Fopenalgo-desktop%2Fstats%2Fbadges%2Fdownloads-month.json)](https://github.com/marketcalls/openalgo-desktop/tree/stats)
[![X (formerly Twitter) Follow](https://img.shields.io/twitter/follow/openalgoHQ)](https://twitter.com/openalgoHQ)
[![YouTube: Subscribe](https://img.shields.io/badge/YouTube-Subscribe-FF0000)](https://www.youtube.com/@openalgo)
[![Discord](https://img.shields.io/discord/1219847221055455263)](https://discord.com/invite/UPh7QPsNhP)

</div>

## What is OpenAlgo Desktop?

**OpenAlgo on your own computer. Install, sign in, connect your broker, trade.**

OpenAlgo Desktop is a free, open source, single-user desktop version of
[OpenAlgo web](https://github.com/marketcalls/openalgo). It runs on Windows,
macOS and Linux (x64 or ARM64), with the same screens, the same API and the
same market data feed as the web version, and connects to all **37 brokers**
of OpenAlgo web, Delta Exchange for crypto among them. There is no server to
set up, no Python to install and no `.env` file: every setting and credential
is entered inside the app, and secrets are kept encrypted with a key locked by
your OpenAlgo password.

Everything runs on your computer, in one app you open like any other: the
dashboard, the REST API, the market data feed, the strategy module, the
charting and scalping terminals, the options tools, sandbox mode (analyzer
mode), and the Telegram, WhatsApp and MCP connections. It is built with
Tauri 2 and a Rust backend around the OpenAlgo React interface.

### Works with everything that works with OpenAlgo web

OpenAlgo Desktop listens on the same addresses as OpenAlgo web:

| What | Address |
| --- | --- |
| Dashboard and REST API (`/api/v1`) | `http://127.0.0.1:5000` |
| Market data WebSocket | `ws://127.0.0.1:8765` |
| Broker redirect URL | `http://127.0.0.1:5000/<broker>/callback` |

The REST API accepts the same requests and sends the same responses as the web,
and the WebSocket speaks the same protocol. So the OpenAlgo Python SDK,
TradingView, Amibroker, Chartink, GoCharting, Excel and MCP clients work
unchanged: point them at the desktop and use the API key from the desktop's
API Key page.

```python
from openalgo import api

client = api(
    api_key="your-desktop-api-key",
    host="http://127.0.0.1:5000",
    ws_url="ws://127.0.0.1:8765",
)
print(client.funds())
```

Moving over from OpenAlgo web? See
[Moving from OpenAlgo web](docs/user/moving-from-openalgo-web.md).

## Ways to Trade with OpenAlgo Desktop

| Surface | Route | Who it's for |
| --- | --- | --- |
| **Unified Broker API** | `/api/v1/` | External platforms and your own code: TradingView, Amibroker, Chartink, GoCharting, Excel, the Python SDK and the other official SDKs, and AI clients over MCP. One contract across 37 brokers, answering exactly as OpenAlgo web does. |
| **Strategy Module and RMS** | `/strategy` | Traders who run multi-leg options strategies, or trade TradingView and GoCharting alerts one signal at a time, with stop, target and trailing rules decided by one risk engine. |
| **Charting Terminal and OpenScript** | `/trading` | Traders who trade from the chart: a full-screen terminal with an order dock, custom indicators, and OpenScript studies and strategies that are backtested and run live on the same engine. |
| **Scalping Terminal** | `/scalping` | Fast intraday trades in options, futures and equity from the keyboard, with stop-loss, target and trailing protection that OpenAlgo watches for you. |
| **Options Trading Suite** | `/tools` | Options traders: sixteen tools (Strategy Builder with payoff diagrams and live Greeks, Strategy Portfolio, Option Chain, Option Greeks, OI Tracker, OI Range, Max Pain, Straddle Chart, Straddle PnL, Vol Surface, GEX Dashboard, Strike Profile, Gamma Density, IV Smile, OI Profile, Arbitrage). Each one uses your connected broker's data. |

Every one of them can run in sandbox mode (analyzer mode) first: while it is
on, every order from the app, the API, webhooks and MCP clients goes to the
sandbox instead of your broker. Alerts, P&L tracking, latency and traffic
monitoring and the MCP server use the same services as the screens.

## Quick Links

- **Download**: [GitHub Releases](https://github.com/marketcalls/openalgo-desktop/releases)
- **Documentation map**: [docs/INDEX.md](docs/INDEX.md), every guide, the API reference and the developer docs
- **User guide**: [docs/userguide/README.md](docs/userguide/README.md)
- **API documentation**: [docs/api/README.md](docs/api/README.md)
- **Moving from OpenAlgo web**: [docs/user/moving-from-openalgo-web.md](docs/user/moving-from-openalgo-web.md)
- **Changelog**: [CHANGELOG.md](CHANGELOG.md)

## Download and install

Download the installer for your computer from the
[GitHub Releases page](https://github.com/marketcalls/openalgo-desktop/releases).
Each release lists a `SHA256SUMS.txt` file so you can check a download.

| Computer | File to download | How to install |
| --- | --- | --- |
| Windows 10 or 11 (64-bit) | ends in `_x64-setup.exe` | Run it. It installs for your user only and does not need administrator rights. |
| Mac with Apple Silicon (M1 and later) | ends in `_aarch64.dmg` | Open it and drag OpenAlgo Desktop to Applications. |
| Mac with an Intel processor | ends in `_x64.dmg` | Open it and drag OpenAlgo Desktop to Applications. |
| Linux, 64-bit PC (Ubuntu, Debian, Mint) | ends in `_amd64.deb` or `_amd64.AppImage` | `.deb`: `sudo apt install ./<file>.deb`. AppImage: see below. |
| Linux, 64-bit ARM (ARM64) | ends in `_arm64.deb` or `_aarch64.AppImage` | `.deb`: `sudo apt install ./<file>.deb`. AppImage: see below. |

Not sure which Mac you have? Open the Apple menu, then About This Mac. "Chip:
Apple M..." means Apple Silicon; "Processor: Intel" means Intel.

OpenAlgo Desktop does not support Raspberry Pi. On a Pi, run
[OpenAlgo web](https://github.com/marketcalls/openalgo) instead.

To run an AppImage on Linux (x64 or ARM64), make it executable and start it:

```bash
chmod +x OpenAlgo*.AppImage
./OpenAlgo*.AppImage
```

If it says FUSE is missing, install it with `sudo apt install libfuse2` (on
Ubuntu 24.04 the package is `libfuse2t64`), or use the `.deb` instead.

To check a download, compare its checksum with the line for that file in
`SHA256SUMS.txt`:

| System | Command |
| --- | --- |
| Windows (PowerShell) | `Get-FileHash .\<file> -Algorithm SHA256` |
| macOS | `shasum -a 256 <file>` |
| Linux (x64 or ARM64) | `sha256sum <file>` |

## Supported systems

OpenAlgo Desktop is a desktop app: it needs a signed-in user with a graphical
desktop, and it trades only while it is open.

| System | Support | What it needs |
| --- | --- | --- |
| Windows 11, 64-bit (x64) | Supported | Nothing extra: WebView2, which draws the app's window, is part of Windows 11. |
| Windows 10, 64-bit (x64) | Supported | WebView2. Most Windows 10 PCs already have it; if not, the installer downloads and installs it, which needs an internet connection. |
| Windows Server 2022 or 2025 with Desktop Experience | Supported | A user signed in at the desktop or over Remote Desktop. Trading stops when that user signs out. If WebView2 is missing, the installer downloads it. |
| Windows Server 2019 with Desktop Experience | Not tested | May work as on 2022. |
| Windows Server Core, Nano Server | Not supported | No desktop to show the app in. |
| Windows on ARM, 32-bit Windows, Windows 8.1 and older | Not supported | No installer is built for them. |
| macOS 10.15 (Catalina) or later, Apple Silicon or Intel | Supported | Pick the `.dmg` for your chip. |
| Ubuntu 22.04 or later, Debian 12 or later, Linux Mint 21 or later, 64-bit (x64) | Supported | WebKitGTK 4.1 and GTK 3: `sudo apt install ./<file>.deb` installs them. The AppImage needs them and FUSE 2 installed. |
| Ubuntu 22.04 or later, Debian 12 or later, 64-bit ARM (ARM64), for example an ARM laptop or virtual machine | Supported | As for x64 Linux above. |
| Other 64-bit Linux distributions (x64 or ARM64) | Not tested | The AppImage may run where WebKitGTK 4.1, GTK 3 and glibc 2.35 or newer are installed. |
| Raspberry Pi (any model) | Not supported | Use [OpenAlgo web](https://github.com/marketcalls/openalgo) on a Pi ([docs](https://docs.openalgo.in)). |
| Headless Linux (no desktop: servers, SSH-only), Docker | Not supported | The app opens a window and cannot run without a desktop session. Use [OpenAlgo web](https://github.com/marketcalls/openalgo) for a server. |
| Running as a background service (Windows service, systemd unit, launchd daemon) | Not supported | The app runs as the signed-in user, and its keys stay locked until that user signs in to OpenAlgo. |

"Supported" means an installer is built for that system and a problem on it
is a bug to report. The automated tests run on every change on the build
machines GitHub provides: Ubuntu 22.04 on x64 and on 64-bit ARM, the current
macOS on Apple Silicon (the Intel Mac installer is built there too, but not
run), and Windows Server 2025 with Desktop Experience.

Every change, before it is accepted, also has its installers installed on
fresh machines and started as a trader would on first use, with the account
created and signed in and the app's own window checked to have opened its
page: the `.deb` and the AppImage on Ubuntu 22.04 (x64 and 64-bit ARM), the
setup program on Windows Server 2022 and Windows Server 2025, and the `.dmg`
on Apple Silicon macOS. Both Mac apps are checked to ask for no newer macOS
than this table says, and the Windows app to need no DLL that Windows does
not ship, such as Microsoft's Visual C++ runtime (the build machines have it
installed; a trader's PC may not). On Ubuntu the installed app is also driven through its
window: account created, a wrong password refused, signed in and out. A
release is built only from a change that passed all of them. Windows 10 and
11, Intel Macs, ARM64 Linux computers and the other systems in the table
have not yet been installed and used by hand; the checklist for doing so is
[Platform qualification](docs/release/platform-qualification.md).

A computer with no internet connection and no WebView2 (some Windows 10 and
Windows Server installs) cannot install it from the setup program: install
the WebView2 "Evergreen Standalone Installer" from Microsoft first, then run
the OpenAlgo setup.

## Opening the app the first time (unsigned installers)

The installers are not yet code-signed, so Windows and macOS warn you the first
time you open the app. This is expected. Download only from the GitHub Releases
page above, and check the checksum if you want to be sure the file is the one
that was published.

**Windows.** SmartScreen shows "Windows protected your PC". Choose **More
info**, then **Run anyway**.

**macOS 15 (Sequoia) and later.** The first time, macOS says "Apple could not
verify "OpenAlgo Desktop" is free of malware that may harm your Mac or
compromise your privacy", with the buttons **Done** and **Move to Bin** (Move
to Trash). Choose **Done**, not Move to Bin. Then go to **System Settings**,
**Privacy & Security**, scroll down to the message about OpenAlgo Desktop,
choose **Open Anyway**, and confirm with your password or Touch ID. Open the
app again and choose **Open Anyway** if macOS asks once more.

**macOS 14 (Sonoma) and earlier.** macOS says the app "cannot be opened
because Apple cannot check it for malicious software", or that it is from an
unidentified developer. In Finder, open Applications, right-click (or
Control-click) OpenAlgo Desktop, choose **Open**, then **Open** again. The
System Settings way above works here too.

You only need to do this once. If macOS says the app "is damaged and can't be
opened", it is the same check; in Terminal run
`xattr -dr com.apple.quarantine "/Applications/OpenAlgo Desktop.app"` and open
it again.

**Linux** (x64 or ARM64) shows no warning.

## Supported Brokers

All 37 brokers of OpenAlgo web, including Rupeezy and Delta Exchange for crypto.

<details>
<summary>View all supported brokers</summary>

- 5 Paisa
- 5 Paisa (XTS)
- Alice Blue
- Angel One
- Arrow
- CompositEdge
- Definedge
- Delta Exchange
- Dhan
- Dhan (Sandbox)
- Firstock
- Flattrade
- Fyers
- Groww
- HDFC Securities
- HDFC Sky
- Ibulls
- IIFL
- IIFL Capital
- IndMoney
- JainamXts
- Kotak Securities
- Motilal Oswal
- mStock by Mirae Asset
- Nubra
- Paytm Money
- Pocketful
- RMoney
- Rupeezy
- Samco
- Shoonya
- Tradejini
- TradeSmart
- Upstox
- Wisdom Capital
- Zebu
- Zerodha

</details>

Every broker answers in OpenAlgo's common formats: orders, trades, positions
and holdings come back with OpenAlgo symbols. Exchange coverage, the way you
sign in, market data entitlement, GTT and other optional features still vary
by broker and account; [Broker sign-in](docs/user/brokers.md) covers each
kind of sign-in.

## Core Features

### Unified REST API Layer (`/api/v1/`)

The same contract as OpenAlgo web, field for field, on
`http://127.0.0.1:5000`, pinned by contract tests against answers recorded
from a live web instance:

- **Order management**: place, modify and cancel orders; smart, basket, split
  and options orders; close positions; GTT orders
- **Portfolio**: positions, holdings, order book, trade book, funds and margin
- **Market data**: quotes, multi-quotes, market depth, history, intervals and
  symbol search
- **Options**: option symbols, option chain, Greeks, synthetic futures
- **Strategies, Telegram and WhatsApp** over the API, and analyzer status and
  toggle

[API documentation](docs/api/README.md)

### Real-Time WebSocket Streaming

- The market data feed on `ws://127.0.0.1:8765` speaks the web's protocol
- Subscribe to LTP, Quote or Depth (5, 20, 30 and 50 levels) for any symbol,
  and to the order update stream
- One broker feed shared by the app's screens, your scripts and the sandbox
- Reconnects by itself after a network outage and when the computer wakes

### Options & Strategy Analytics Tools (`/tools`)

Sixteen tools for options trading and market analysis, on the **Tools** page.
They use your connected broker's data; market data entitlement and exchange
coverage still apply:

| Tool | Route | What it does |
| --- | --- | --- |
| **Strategy Builder** | `/strategybuilder` | Build multi-leg option strategies with live Greeks, payoff diagram and what-if simulators |
| **Strategy Portfolio** | `/strategybuilder/portfolio` | Saved strategies across MyTrades and Simulation watchlists |
| **Option Chain** | `/optionchain` | Real-time option chain with live Greeks, OI data, and quick order placement |
| **Option Greeks** | `/ivchart` | Historical IV, Delta, Theta, Vega, and Gamma charts for ATM options |
| **OI Tracker** | `/oitracker` | Open Interest analysis with CE/PE OI bars, PCR overlay, and ATM strike marker |
| **OI Range** | `/oirange` | Open Interest by strike for a custom range with ATM-relative quick selectors and optional 1-minute auto-refresh |
| **Max Pain** | `/maxpain` | Max Pain strike calculation with visual pain distribution across strikes |
| **Straddle Chart** | `/straddle` | Dynamic ATM Straddle chart with rolling strike, Spot, and Synthetic Futures overlay |
| **Straddle PnL** | `/straddlepnl` | Simulated intraday ATM straddle P&L with automated N-point adjustments and trade log |
| **Vol Surface** | `/volsurface` | 3D Implied Volatility surface across strikes and expiries using live option chain data |
| **GEX Dashboard** | `/gex` | Gamma Exposure analysis with OI Walls, Net GEX per strike, and top gamma strikes |
| **Strike Profile** | `/strikeprofile` | GEX and DEX heatmap by strike and expiry with per-expiry peaks, gamma flip, call and put walls |
| **Gamma Density** | `/gammadensity` | Γ×OI density and convexity zones with intraday & to-expiry views, ATM IV, and ±1σ/±2σ expected-move bands |
| **IV Smile** | `/ivsmile` | Implied Volatility smile with Call/Put IV curves, ATM IV, and skew analysis |
| **OI Profile** | `/oiprofile` | Futures candlestick with OI butterfly and daily OI change across strikes |
| **Arbitrage** | `/arbitrage` | Realtime futures calendar-spread scanner across NFO & MCX, ranked by executable bid/ask spread % with one-click two-leg orders |

### Strategy Module and Risk Management (`/strategy`)

- Multi-leg options strategies that enter and exit together, and a signal mode
  that moves one leg per TradingView or GoCharting alert
- Stop, target and trailing rules decided by one risk engine, which passes
  OpenAlgo web's risk test vectors
- Each Live strategy stays tied to the broker account it was started on

[Strategy module and risk engine](docs/prompt/strategy_rms_documentation.md)

### Charting Terminal and OpenScript (`/trading`)

- A full-screen charting terminal on `openalgo-charts`, with an order dock and
  keyboard shortcuts
- Your own chart indicators, loaded from your data folder
- OpenScript studies and strategies: backtests and live runs on the same
  engine

[Charting terminal](docs/userguide/32-charting-terminal/README.md) ·
[Custom indicators](docs/custom-indicators.md) · [OpenScript](docs/openscript.md)

### Scalping Terminal (`/scalping`)

Keyboard-driven intraday trading in options, futures and equity, with
stop-loss, target and trailing protection that OpenAlgo watches until the
position is flat. [Scalping terminal](docs/scalping.md)

### Sandbox testing

Sandbox mode (analyzer mode) gives you 1 crore of simulated capital to test
strategies, webhooks and API clients with real market prices, kept in its own
database. See [Sandbox mode (analyzer mode)](#sandbox-mode-analyzer-mode).

### Action Center

Order approval for manual control:

- **Auto mode**: orders go straight to your broker
- **Semi-Auto mode**: orders wait in the Action Center for your approval
- Approve or reject orders one at a time, or approve them all

### ChartInk Integration

Scanner alerts by webhook:

- BUY, SELL, SHORT and COVER actions
- Intraday strategies with a square-off time, and positional strategies
- Symbols added one at a time or in bulk from CSV

### TradingView and GoCharting

The **Platforms** page builds the webhook address and the alert message for
TradingView and GoCharting from the symbol and order you choose.

### Historify

Historical market data on your computer, in DuckDB: download jobs, a
scheduler, exports and charts.

### AI-Powered Trading (MCP Server)

Connect AI clients to your account and market data:

- Native MCP server with the web's 49 tools
- Over stdio for Claude Desktop and Claude Code, and over HTTP at `/mcp`;
  clients on another computer need Remote MCP turned on
- Scoped tokens created on the API Key page, with per-token limits and an
  audit log

[Connecting Claude Desktop and Claude Code](docs/user/mcp.md) ·
[MCP tool reference](docs/mcp-tool-reference.md)

### Telegram Bot

Native in the desktop, with no Python:

- Order and trade alerts delivered to Telegram
- Order book, trade book, positions, holdings and funds on demand
- Intraday and daily charts
- A button menu, and analytics of the bot's use

### WhatsApp

Native in the desktop: pair with a QR code or a pair code, then receive alerts
and send commands.

### Monitoring

- **Latency Monitor**: order round-trip times by broker
- **Traffic Monitor**: API use, errors and endpoint statistics
- **Security log**: refused requests and address bans
- **Health Monitor and diagnostics**: unconfirmed orders, alerts, and how
  fresh each broker feed is ([Health monitoring](docs/health-monitoring.md))
- **P&L Tracker**: profit and loss through the day on interactive charts

### Security

- **Your password locks your keys**: broker credentials and tokens are
  encrypted with AES-256-GCM under a key locked by your OpenAlgo password
  (Argon2id) and kept in your data folder; no OS keychain is used
- **Locked until you sign in**: after a start or a logout the API, the feed,
  webhooks and MCP stay off
- **Two-factor sign-in** with an authenticator app (TOTP)
- **Rate limits** on the API as on the web, sign-in and API key failures
  throttled, and automatic bans of devices on your network that keep failing
  (never this computer)
- **Browser protection**: a strict Content Security Policy, exact origin
  checks, CSRF protection, developer tools off in release builds
- **Clean logs**: API keys, tokens, OAuth codes, passwords and TOTP secrets
  are never written to a log
- **No data collection**: the app sends no usage data. It talks to your
  broker, to Telegram and WhatsApp when you turn them on, and to GitHub to
  check for updates

### Modern React Interface

- The OpenAlgo web screens, carried over nearly verbatim: **React 19** with
  TypeScript
- **shadcn/ui** components with **Tailwind CSS 4**
- **TanStack Query** for server state and **Zustand** for app state
- **Real-time updates** over Socket.IO: pages refresh when an order or a
  position changes, without polling
- Light and dark themes with accent colors, and a dark purple look in
  analyzer mode
- Opens in the app's own window, and in any browser on the same computer

## Supported Platforms

Connect your strategies from any of these; they reach OpenAlgo Desktop
through its API and webhooks:

- **Amibroker**: AFL strategies through the REST API
- **TradingView**: webhook alerts from Pine Script strategies
- **GoCharting**: webhook alerts
- **ChartInk**: scanner webhooks
- **Python**: the official SDK
- **Excel**: through the REST API
- **AI clients**: Claude Desktop, Claude Code and other MCP clients

TradingView, GoCharting and ChartInk send their alerts from their own servers,
so they need a tunnel to your computer: see
[Webhooks from the internet](docs/user/moving-from-openalgo-web.md#webhooks-from-the-internet).
Programs on the same computer need none.

Receive your strategy alerts on **Telegram** and **WhatsApp**.

## Technology Stack

### Desktop and Backend

- **Tauri 2**: the desktop shell, window and installers
- **Rust 1.99**: the whole backend, in one process
- **tokio**: async runtime
- **axum**: the HTTP server for the interface, `/api/v1` and webhooks
- **socketioxide**: Socket.IO for real-time updates to the interface
- **tokio-tungstenite**: the market data WebSocket and broker feeds
- **reqwest**: broker and Telegram HTTP calls
- **rmcp**: the MCP server
- **Argon2id and AES-256-GCM** (RustCrypto): password hashing and encryption
- **rust_decimal**: exact money arithmetic in the sandbox

### Frontend

- **React 19**: UI library
- **TypeScript**: type-safe JavaScript
- **Vite 8**: build tool
- **Tailwind CSS 4**: utility-first CSS
- **shadcn/ui**: components built on **Radix UI**
- **TanStack Query**: server state
- **Zustand**: client state

### Data Visualization & Editors

- **openalgo-charts**: the `/trading` charting terminal
- **openalgo-script**: OpenScript, compiled and run in the app
- **TradingView Lightweight Charts**: P&L and market data charts
- **Plotly**: options analytics charts
- **CodeMirror**: script and JSON editing

### Testing & Quality

- **cargo test** with **nextest** and **proptest**: Rust unit, contract and
  property tests
- **Vitest**: frontend unit and component tests
- **Playwright**: end-to-end tests
- **Biome**: linting and formatting
- **axe-core**: accessibility testing

### Databases

- **SQLite** (WAL, r2d2 pool): 3 databases (main, logs, sandbox)
- **DuckDB**: 1 historical market data store (Historify)

## Official SDKs

The official client libraries are written against the OpenAlgo API, so they
work with OpenAlgo Desktop unchanged: set the host to `http://127.0.0.1:5000`
and the WebSocket to `ws://127.0.0.1:8765`, and use the API key from the
desktop's API Key page.

| Language / Platform | Repository |
| --- | --- |
| Python | [openalgo-python-library](https://github.com/marketcalls/openalgo-python-library) |
| Node.js | [openalgo-node](https://github.com/marketcalls/openalgo-node) |
| Java | [openalgo-java](https://github.com/marketcalls/openalgo-java) |
| Rust | [openalgo-rust](https://github.com/marketcalls/openalgo-rust) |
| .NET / C# | [openalgo.NET](https://github.com/marketcalls/openalgo.NET) |
| Go | [openalgo-go](https://github.com/marketcalls/openalgo-go) |

## Running the app

After installing, start OpenAlgo Desktop like any other app. It opens in its
own window and keeps a small local server running while the window is open.
SDKs, TradingView, Amibroker, MCP clients and webhooks reach it through that
server.

| System | Start it | Quit it |
| --- | --- | --- |
| Windows 10 or 11 | **Start** menu, then **OpenAlgo Desktop** (or the desktop shortcut) | Close the window, or right-click its taskbar icon and choose **Close window** |
| macOS | **Applications** folder or **Launchpad**, then **OpenAlgo Desktop** | **OpenAlgo Desktop** menu, then **Quit** (Cmd+Q). Closing the window also quits it |
| Linux (`.deb`) | Your desktop's app menu, then **OpenAlgo Desktop**, or run `openalgo-desktop` in a terminal | Close the window |
| Linux (AppImage, x64 or ARM64) | `./OpenAlgo*.AppImage` from the folder you saved it to | Close the window |

What to know while it runs:

- **It trades only while it is open.** Closing the window stops the server,
  the market data feed, strategies, OpenScript runs and stop/target tracking.
  Orders already placed with your broker stay with your broker. Keep the app
  open, and the computer awake, for as long as automation should run.
- **Sleep and network loss pause it too.** When the computer wakes or the
  network returns, the app reconnects. Check your positions after a long
  sleep.
- **Only one copy can run at a time**, because both copies would need the
  same ports. If OpenAlgo web runs on the same computer, see [Ports](#ports).
- **The dashboard also opens in a browser.** While the app runs, open
  `http://127.0.0.1:5000` (or your chosen port) in any browser on the same
  computer.

## First run

1. **Create your account.** The sign-in card offers "First time? Set up your
   account" until an account exists. Setup asks for a username, email and
   password. This account exists only on your computer. Your API key is
   created automatically at the same time; find it on the **API Key** page.
2. **Sign in.** One card: username and password, then the code from your
   authenticator app if you turned on two-factor sign-in. If OpenAlgo can
   resume your broker session from the same trading day, you go straight to
   the dashboard; otherwise you go to **Select Broker**.
3. **Add your broker's keys** on the **Broker Configuration** page. See the
   next section.
4. **Log in to your broker** from its card on **Select Broker**. The master
   contract (the list of symbols) downloads after the login; the Master
   Contract indicator on the dashboard shows its progress.

Your broker session from the same trading day is resumed when you sign in
again, until the broker's daily cut-off (around 03:00 IST); after that, log
in to the broker again. Logging out returns you to the sign-in card.

## Connecting your broker

There is no `.env` file. Broker keys are saved in the app, **one set per
broker, for as many brokers as you like**, and **one broker is logged in at
a time**:

1. Open **Broker Configuration** (from Select Broker, or the **Broker** tab
   in Profile) and choose your broker. The page asks only for the keys that
   broker needs, labelled the way the broker names them, with a hint for
   each and a note on how its login works.
2. Enter the keys from your broker's developer portal and choose **Save**.
   Saved keys are shown masked (for example `Saved: abc***xyz`) and never in
   full; leave a field blank to keep the saved value.
3. On **Select Broker**, choose **Login with** on your broker's card.
   Depending on the broker this takes you to its login page, opens a small
   login card in the app (for a TOTP, PIN or OTP), or signs you in with one
   click from the saved keys. If a login fails, the reason shows on that
   broker's card.

**Redirect URL.** For brokers that sign in on their own website, set the
redirect URL of your broker app to

```
http://127.0.0.1:5000/<broker>/callback
```

for example `http://127.0.0.1:5000/zerodha/callback` or
`http://127.0.0.1:5000/fyers/callback`. This is the same address OpenAlgo web
uses, so a broker app you already set up for OpenAlgo web on this computer
works as it is. Broker Configuration shows the exact address (automatic, or
your Host Server URL while the tunnel is on), and lets you set another one
per broker or go back to the automatic one.

**Switching brokers.** Choose **Login with** on another configured broker.
While a broker is logged in, OpenAlgo asks first: "Signing in to Y ends your
X session. Open positions and orders stay with X." X's session ends only
when the sign-in to Y completes; if it fails, X stays connected. A switch is
refused while Live strategies, OpenScript runs or scalping protection still
run on X, or orders there are not yet settled. The keys of the broker you
leave stay saved, so switching back needs only a login. No restart is needed.

**Forgetting a broker.** **Forget this broker** on Broker Configuration
removes its keys from this computer, after you confirm; for the broker that
is logged in, it signs you out of it first.

Sign-in details for each kind of broker are in
[Broker sign-in](docs/user/brokers.md).

## Sandbox mode (analyzer mode)

Sandbox mode lets you test strategies, webhooks and API clients with simulated
money and real market prices. Turn it on with the mode switch in the top bar
(the badge shows **Live Mode** or **Analyze Mode**).

While analyzer mode is on, every order from the app, the API, webhooks and MCP
clients goes to the sandbox instead of your broker, and the API answers the
same way OpenAlgo web does in analyze mode. The sandbox starts with 1 crore of
simulated capital, blocks margin, fills market orders at the live price and
limit and stop orders from live ticks, settles CNC positions to holdings the
next day and squares off MIS positions at the exchange cut-off times. Its data
is kept in a separate database, apart from live trading.

- **Sandbox** (in the profile menu): Sandbox Configuration, with capital,
  leverage and square-off times, and a link to **My P&L History**.
- **Logs**, then **Sandbox Logs**: the Sandbox Request Monitor, listing every
  request made while analyzer mode was on.

Market data still comes from your connected broker, so connect a broker before
using sandbox mode. Fills are all or nothing and P&L is before costs; see
[Sandbox mode](docs/user/sandbox.md) for how orders fill and the limits of
the simulation.

## Ports

| What | Default port | Changed in |
| --- | --- | --- |
| App, API and broker redirects | 5000 | Profile menu, **Server Settings**, **App port** |
| Market data WebSocket | 8765 | Profile menu, **Server Settings**, **Market data port** |

By default OpenAlgo Desktop only accepts connections from this computer. To use
it from another device on your network, turn on **Allow access from other
devices** in Server Settings. A new market data port or address takes effect
within seconds; a new app port or address takes effect after the app restarts,
and Server Settings says which of your changes is waiting for a restart.

**Port 5000 already in use.** If another program holds port 5000, the app opens
on a page saying so instead of the dashboard.

- **On a Mac this is usually AirPlay Receiver.** Open **System Settings**,
  **General**, **AirDrop & Handoff**, turn off **AirPlay Receiver**, then
  choose **Try again**.
- Otherwise close the other program (often OpenAlgo web or a second copy of
  OpenAlgo Desktop) and choose **Try again**.
- Or type another port on that page and choose **Try again**. If you do, use
  the new port in your broker app's redirect URL and in your SDK and trading
  platform settings.

More in [Troubleshooting](docs/user/troubleshooting.md).

## Where your data lives, and backups

Everything OpenAlgo Desktop stores is in one folder:

| System | Folder |
| --- | --- |
| Windows | `%APPDATA%\com.openalgo.desktop` |
| macOS | `~/Library/Application Support/com.openalgo.desktop` |
| Linux (x64 or ARM64) | `~/.local/share/com.openalgo.desktop` |

Inside it: `openalgo.db` (account, settings, strategies, encrypted broker
credentials and the locked keys), `sandbox.db` (sandbox mode), `logs.db`
(order and API logs), `historify.duckdb` (Historify market data), and your
OpenScript and custom indicator files.

Broker credentials, tokens, your API key and your two-factor login secret are
encrypted. The encryption key is itself locked with your OpenAlgo password
(Argon2id) and kept in `openalgo.db`. OpenAlgo Desktop does not use your
operating system's keychain on any platform: not the macOS Keychain, not the
Windows Credential Manager, not the Secret Service of Linux.
A copy of the folder is useless to anyone without your password.

What this means day to day:

- **Sign in after OpenAlgo starts.** Until you do, the API (`/api/v1`), the
  market data feed, TradingView and Chartink webhooks, AI clients (MCP) and
  your broker connection stay off: programs are told "Sign in to OpenAlgo to
  use the API". Telegram and WhatsApp bots that you had switched on start
  when you sign in. Everything goes off again when you log out, and the
  unlocked key is wiped from memory; quitting OpenAlgo does the same.
- **Keep your password safe.** It cannot be recovered. If you forget it, the
  sign-in page's Forgot password sets a new one: your account, settings,
  strategies and history stay, and you enter your broker keys again, copy the
  new API key into your tools, and set up two-factor login again.

**To back up**, close OpenAlgo Desktop and copy the whole folder. Restore it by
closing the app and copying the folder back. A backup opens on any computer
with the password you had when you made it, because the locked keys travel
with it.

**Before an update changes your data**, OpenAlgo backs it up by itself: the
first time a newer version opens the folder, it copies every database into
`backups/` inside the folder before anything is migrated. If the new version
then cannot open the data, it puts that copy back, so the folder is exactly
as the old version left it, and says so. The copy exists only for that
start: it is deleted as soon as the new version has opened the data (or put
it back), so no older copy of your account, broker keys or API key is kept.
`data-version.json` records the version that last opened the folder; an
older version will not open a folder a newer one has used (it says so and
changes nothing). See
[Updating OpenAlgo Desktop](docs/user/updating.md#your-data-is-backed-up-before-an-update).

Resetting your account (Reset account, on the forgotten-password page) removes
the account and its keys; make a new backup once you have set OpenAlgo up
again. Backups made before a reset still open with the password you had then.

**Upgrading from an earlier build** that kept the keys in the system keychain
(macOS Keychain, Windows Credential Manager, or the Secret Service on Linux):
the first time you sign in in the OpenAlgo Desktop window, OpenAlgo reads them
from the keychain one last time (allow it when your computer asks), moves them
into `openalgo.db`, checks them, and deletes the keychain entries. After that
it never asks again. If the keychain cannot be read, the sign-in page offers to
try again or to start fresh. An earlier build's `vault.json` is moved into
`openalgo.db` the same way and then deleted. A sign-in from a browser, another
device or a tunnel does not move anything: it asks you to sign in once in the
desktop window first.

**What a failed sign-in tells.** Only the OpenAlgo Desktop window is told why
the saved keys could not be opened, with the ways out. A browser, a program,
another device or a tunnel gets "Invalid credentials" for every failure, after
the same amount of work, so nobody can learn from a failed sign-in whether a
name or a password was right.

## Updating OpenAlgo Desktop

OpenAlgo Desktop checks for a newer version each time it starts; to check at
any time, open the profile menu and choose **Check for Updates**. When one is
available a small notice shows the version and its release notes. It only
tells you: **nothing is downloaded until you choose Update**.

**Update** downloads the new version, checks its signature against the key
built into the app (an update that does not match is refused), installs it
and restarts; then you sign in again. Before it downloads, and again before it restarts, OpenAlgo
names anything a restart would interrupt (running Live strategies, OpenScript
runs, scalping stops, open orders at your broker) and asks you to confirm;
sandbox-only activity gets a lighter warning. It never restarts on its own.

| Installed from | How it updates |
| --- | --- |
| macOS `.dmg` | In the app: replaces OpenAlgo Desktop |
| Windows setup `.exe` | In the app: runs the new installer (passive, for your user only) |
| Linux (x64 or ARM64) AppImage | In the app: replaces the `.AppImage` file |
| Linux (x64 or ARM64) `.deb` | The app tells you and links to the release page; download the new `.deb` there and install it over the old one |

You can always install a newer version by hand over the old one from
[Releases](https://github.com/marketcalls/openalgo-desktop/releases). Your
account, settings, broker keys, strategies, sandbox data and Historify data
are kept either way.

The first time the new version starts, it backs up your data before it
changes anything, and if it then cannot open the data it puts that copy back
and says so. The copy is deleted as soon as the new version has opened your
data. An older version never opens data that a newer version has used; it
says so and changes nothing. Details:
[Updating OpenAlgo Desktop](docs/user/updating.md). Maintainers:
[how a release reaches installed copies](docs/release/updates.md).

## What is included, and what is not

Included: everything you use in OpenAlgo web for trading. The dashboard, order
book, trade book, positions and holdings; the `/trading` charting terminal and
scalping; the strategy module with risk management; TradingView, GoCharting
and Chartink webhooks; Action Center for semi-automatic orders; options tools
(option chain, Greeks, OI tracker, max pain, IV charts, GEX, straddles, strategy
builder); Historify; OpenScript; the Playground; API key management; logs,
latency and traffic monitoring; Telegram and WhatsApp alerts and bots; sandbox
mode; and an MCP server for AI clients.

Not included in this version:

- the Python Strategy Host (`/python`)
- the **Agent** (`/agent`, its settings page, and the Assistant panel in the
  `/trading` terminal)
- Flow (`/flow`)
- the pandas-based backtesters: Portfolio Backtester, SIP Backtester and
  Portfolio Analyzer

The app has no pages, menu entries or buttons for them. Python strategies
still run as ordinary scripts with the OpenAlgo Python SDK against the
desktop's API, and AI clients such as Claude Desktop connect through the MCP
server.

Other differences from the web: one user per installation, and one broker
connected at a time.

## API Documentation

Complete API reference and examples:

- **API documentation**: [docs/api/README.md](docs/api/README.md), one page per endpoint
- **Symbol format**: [docs/prompt/symbol-format.md](docs/prompt/symbol-format.md)
- **Python SDK**: [docs/prompt/openalgo python sdk.md](<docs/prompt/openalgo python sdk.md>)
- **WebSocket format**: [docs/prompt/websockets-format.md](docs/prompt/websockets-format.md)

## Documentation

- [Documentation map](docs/INDEX.md): every guide, the API reference and the developer docs
- [User guide](docs/userguide/README.md)
- [Moving from OpenAlgo web](docs/user/moving-from-openalgo-web.md)
- [Broker sign-in](docs/user/brokers.md)
- [Sandbox mode (analyzer mode)](docs/user/sandbox.md)
- [Connecting Claude Desktop and Claude Code (MCP)](docs/user/mcp.md)
- [Updating OpenAlgo Desktop](docs/user/updating.md)
- [Troubleshooting](docs/user/troubleshooting.md)
- [Changelog](CHANGELOG.md)
- OpenAlgo documentation: [docs.openalgo.in](https://docs.openalgo.in)

## Running from source (developers)

To run OpenAlgo Desktop from its source code, for development, testing or to
build your own installer.

### Prerequisites

Every system needs:

| Tool | Version | How to get it |
| --- | --- | --- |
| Git | any recent | [git-scm.com](https://git-scm.com/downloads) |
| Node.js | 22 LTS recommended (22.23.4+, 24.22.1+ or 26.12.1+; npm warns on Node 20 and on older patch levels) | [nodejs.org](https://nodejs.org/) or a version manager such as `nvm` |
| Rust | 1.99.1, installed through `rustup` | [rustup.rs](https://rustup.rs/). The repository pins the version in `rust-toolchain.toml` at its root, so `rustup` installs the right one automatically the first time you build |

Each system also needs the native libraries the app window is built on:

**Windows 10 or 11 (64-bit)**

1. Install [Visual Studio 2022 Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
   and select the **Desktop development with C++** workload (it brings the
   MSVC compiler and the Windows SDK).
2. WebView2 is part of Windows 11. On Windows 10, install the
   [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)
   if it is missing.
3. Install Rust with `rustup-init.exe` and keep the default
   `x86_64-pc-windows-msvc` toolchain.
4. Use PowerShell or the Developer Command Prompt for the commands below.

**macOS (Apple Silicon or Intel, 10.15 or later)**

```bash
xcode-select --install                                           # Apple's command line tools
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust
brew install node@22                                             # or use nvm
```

**Linux, x64 or ARM64 (Ubuntu 22.04+, Debian 12+, Linux Mint 21+)**

```bash
sudo apt update
sudo apt install build-essential curl wget file xdg-utils pkg-config patchelf \
  libssl-dev libdbus-1-dev libxdo-dev libwebkit2gtk-4.1-dev \
  libappindicator3-dev librsvg2-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then install Node.js 22 from [nodejs.org](https://nodejs.org/) or with `nvm`
(the version in the distribution's own `apt` repository is usually too old).
This package list is the one CI builds with
(`.github/actions/linux-tauri-deps`).

### Run it in development

```bash
git clone https://github.com/marketcalls/openalgo-desktop.git
cd openalgo-desktop
npm ci                 # install the frontend packages
npm run tauri:dev      # start the app with live reload
```

The first `tauri:dev` compiles the Rust backend, which takes several minutes;
later runs only rebuild what changed. What a development run does:

- It opens the app window. The interface is served by the Vite dev server
  (`http://localhost:5173`), so frontend changes reload instantly, and Rust
  changes restart the app.
- **A development build always uses ports 5500 and 8766**, not 5000 and
  8765, so it can run next to an installed copy or OpenAlgo web.
- **It uses the same data folder as the installed app**
  (see [Where your data lives](#where-your-data-lives-and-backups)). Close the
  installed app first, and back up the data folder if it holds an account you
  care about.

To run only the server, with no window and a temporary data folder, so
nothing touches your real data:

```bash
npm run build                                          # build the interface into dist/
cd src-tauri && cargo run --example dev_server         # then open http://127.0.0.1:5500
```

It starts only the HTTP server: the market data feed on 8766 and the
background services (health sampling, the traffic and latency monitor, the
strategy and OpenScript schedulers, Chartink) are not started, so the pages
that need them show their empty states. Stop it with Ctrl+C. Every start
begins with an empty account.

### Build an installer

```bash
npm ci
npm run tauri:build
```

This builds the installer for the system you run it on. Cross-building, such
as a Windows installer on a Mac, is not supported. Use that system, or the
project's GitHub Actions, which build all five installers. Output folders, in
`src-tauri/target/release/bundle/`:

| System | Output |
| --- | --- |
| Windows | `nsis/OpenAlgo Desktop_<version>_x64-setup.exe` |
| macOS | `dmg/OpenAlgo Desktop_<version>_<arch>.dmg` and `macos/OpenAlgo Desktop.app` |
| Linux (x64 or ARM64) | `deb/*.deb` and `appimage/*.AppImage` |

Installers you build yourself are unsigned, like the published ones; see
[Opening the app the first time](#opening-the-app-the-first-time-unsigned-installers).

### Tests and checks

```bash
npm run test:run                  # frontend unit and component tests (Vitest)
npx tsc -b && npx biome ci ./src  # type check and lint
npm run e2e                       # end-to-end tests (Playwright)
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked               # Rust unit and integration tests
```

### Common problems

| Problem | Fix |
| --- | --- |
| Linux: `webkit2gtk-4.1` or `javascriptcoregtk` not found | Install the Linux package list above. Ubuntu 20.04 and Debian 11 are too old; they lack WebKitGTK 4.1 |
| Windows: `link.exe` not found, or MSVC errors | Install the **Desktop development with C++** workload in the Visual Studio Build Tools, then open a new terminal |
| macOS: `xcrun: error: invalid active developer path` | Run `xcode-select --install` |
| `error: toolchain '1.99.1' is not installed` | Run `rustup toolchain install 1.99.1`, or any `cargo` command inside the repository, which installs it |
| Port 5500 already in use | A development copy or `dev_server` is still running; stop it. Find the program with `lsof -nP -iTCP:5500 -sTCP:LISTEN` (macOS, Linux) or `netstat -ano \| findstr :5500` (Windows) |
| The browser at `127.0.0.1:5500` says the interface is not built | Run `npm run build` once; the server reads the built interface from `dist/` |
| A build runs out of memory or disk | Rust builds need several GB of free memory and about 20 GB of disk. Close other heavy programs, or reduce parallel jobs with `CARGO_BUILD_JOBS=2` |

## Contributing

We welcome contributions. Read [CLAUDE.md](CLAUDE.md) first: it describes the
compatibility contract with OpenAlgo web and the project conventions. First-time
setup is in [docs/contributing.md](docs/contributing.md). Then:

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes using [Conventional Commits](https://www.conventionalcommits.org/), for example:
   ```bash
   git commit -m "feat: add amazing feature"
   ```
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## Community & Support

- **Discord**: [Join our community](https://discord.com/invite/UPh7QPsNhP)
- **Twitter/X**: [@openalgoHQ](https://twitter.com/openalgoHQ)
- **YouTube**: [@openalgo](https://www.youtube.com/@openalgo)
- **GitHub Issues**: [Report bugs or request features](https://github.com/marketcalls/openalgo-desktop/issues)

## License

OpenAlgo Desktop is released under the GNU Affero General Public License v3.0.
See [License.md](License.md).

## Credits & Acknowledgments

OpenAlgo Desktop is built on OpenAlgo web and on the open source projects
below. We are grateful to all of them.

### Core Framework

- **[OpenAlgo](https://github.com/marketcalls/openalgo)** - AGPL-3.0 - the web platform this app is the desktop version of, and its React interface
- **[Tauri](https://tauri.app)** - Apache-2.0 or MIT - desktop shell, window and installers
- **[React](https://react.dev)** - MIT License - UI library for building user interfaces
- **[Tokio](https://tokio.rs)** - MIT License - asynchronous runtime for Rust
- **[axum](https://github.com/tokio-rs/axum)** - MIT License - HTTP server

### Backend Libraries

- **[reqwest](https://github.com/seanmonstar/reqwest)** - MIT or Apache-2.0 - HTTP client
- **[tokio-tungstenite](https://github.com/snapview/tokio-tungstenite)** - MIT License - WebSocket client and server
- **[socketioxide](https://github.com/Totodore/socketioxide)** - MIT License - Socket.IO server
- **[rusqlite](https://github.com/rusqlite/rusqlite)** - MIT License - SQLite for Rust, with **[r2d2](https://github.com/sfackler/r2d2)** (MIT or Apache-2.0) connection pools
- **[DuckDB](https://duckdb.org)** - MIT License - analytical database behind Historify
- **[rmcp](https://github.com/modelcontextprotocol/rust-sdk)** - Apache-2.0 - Model Context Protocol server
- **[whatsapp-rust](https://github.com/marketcalls/whatsapp-rust)** - MIT License - WhatsApp client
- **[RustCrypto](https://github.com/RustCrypto)** - MIT or Apache-2.0 - Argon2 and AES-GCM
- **[rust_decimal](https://github.com/paupino/rust-decimal)** - MIT License - exact decimal arithmetic

### UI Components & Styling

- **[shadcn/ui](https://ui.shadcn.com)** - MIT License - components built with Radix UI and Tailwind CSS
- **[Radix UI](https://www.radix-ui.com)** - MIT License - unstyled, accessible UI components
- **[Tailwind CSS](https://tailwindcss.com)** - MIT License - utility-first CSS framework
- **[Lucide](https://lucide.dev)** - ISC License - icon library

### Data Visualization

- **[openalgo-charts](https://www.npmjs.com/package/openalgo-charts)** - Apache-2.0 - the `/trading` charting terminal
- **[TradingView Lightweight Charts](https://github.com/tradingview/lightweight-charts)** - Apache-2.0 - financial charting library
- **[Plotly](https://plotly.com/javascript/)** - MIT License - interactive charts for options analytics

### Code Editors and OpenScript

- **[CodeMirror](https://codemirror.net)** - MIT License - code editor
- **[@uiw/react-codemirror](https://uiwjs.github.io/react-codemirror)** - MIT License - CodeMirror React wrapper with themes
- **[openalgo-script](https://www.npmjs.com/package/openalgo-script)** - Apache-2.0 - the OpenScript compiler and engine

### State Management & Data Fetching

- **[TanStack Query](https://tanstack.com/query)** - MIT License - asynchronous state management
- **[Zustand](https://zustand-demo.pmnd.rs)** - MIT License - lightweight state management
- **[Axios](https://axios-http.com)** - MIT License - promise-based HTTP client

### Real-Time Communication

- **[Socket.IO](https://socket.io)** - MIT License - real-time event-based communication (the interface's client)

### Build & Development Tools

- **[Vite](https://vitejs.dev)** - MIT License - frontend build tool
- **[TypeScript](https://www.typescriptlang.org)** - Apache-2.0 - JavaScript with syntax for types
- **[Biome](https://biomejs.dev)** - MIT or Apache-2.0 - formatter and linter
- **[Vitest](https://vitest.dev)** - MIT License - unit testing
- **[Playwright](https://playwright.dev)** - Apache-2.0 - end-to-end testing
- **[axe-core](https://github.com/dequelabs/axe-core)** - MPL-2.0 - accessibility testing

### Additional Libraries

- **[React Router](https://reactrouter.com)** - MIT License - routing for React
- **[Sonner](https://sonner.emilkowal.ski)** - MIT License - toast notifications
- **[cmdk](https://cmdk.paco.me)** - MIT License - command palette component
- **[next-themes](https://github.com/pacocoursey/next-themes)** - MIT License - theme switching
- **[react-resizable-panels](https://github.com/bvaughn/react-resizable-panels)** - MIT License - resizable panel layouts
- **[html2canvas-pro](https://html2canvas.hertzen.com)** - MIT License - screenshot generation

## Disclaimer

**This software is for educational purposes only. Do not risk money which you
are afraid to lose. USE THE SOFTWARE AT YOUR OWN RISK. THE AUTHORS AND ALL
AFFILIATES ASSUME NO RESPONSIBILITY FOR YOUR TRADING RESULTS.**

Test your strategies in sandbox mode before trading with real money. Past
performance does not guarantee future results. Trading involves substantial
risk of loss.

---

Part of the [OpenAlgo FOSS Ecosystem](https://docs.openalgo.in/mini-foss-universe).
