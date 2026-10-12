# OpenAlgo Desktop

<div align="center">

[![GitHub Stars](https://img.shields.io/github/stars/marketcalls/openalgo?style=social)](https://github.com/marketcalls/openalgo)
[![X (formerly Twitter) Follow](https://img.shields.io/twitter/follow/openalgoHQ)](https://twitter.com/openalgoHQ)
[![YouTube Channel Subscribers](https://img.shields.io/youtube/channel/subscribers/UCw7eVneIEyiTApy4RtxrJsQ)](https://www.youtube.com/@openalgo)
[![Discord](https://img.shields.io/discord/1219847221055455263)](https://discord.com/invite/UPh7QPsNhP)

**OpenAlgo on your own computer. Install, sign in, connect your broker, trade.**

</div>

OpenAlgo Desktop is a single-user desktop version of
[OpenAlgo web](https://github.com/marketcalls/openalgo). It runs on Windows,
macOS, Linux and Raspberry Pi, with the same screens, the same API and the same
market data feed as the web version. There is no server to set up, no Python to
install and no `.env` file: every setting and credential is entered inside the
app, and secrets are kept encrypted with a key held in your operating system's
keychain.

## Contents

- [Works with everything that works with OpenAlgo web](#works-with-everything-that-works-with-openalgo-web)
- [Supported systems](#supported-systems)
- [Download and install](#download-and-install)
- [Opening the app the first time (unsigned installers)](#opening-the-app-the-first-time-unsigned-installers)
- [Running the app](#running-the-app)
- [First run](#first-run)
- [Connecting your broker](#connecting-your-broker)
- [Sandbox mode (analyzer mode)](#sandbox-mode-analyzer-mode)
- [Ports](#ports)
- [Where your data lives, and backups](#where-your-data-lives-and-backups)
- [Supported brokers](#supported-brokers)
- [What is included, and what is not](#what-is-included-and-what-is-not)
- [Guides](#guides)
- [Running from source (developers)](#running-from-source-developers)
- [License](#license)

## Works with everything that works with OpenAlgo web

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

## Supported systems

OpenAlgo Desktop is a desktop app: it needs a signed-in user with a graphical
desktop, and it trades only while it is open.

| System | Support | What it needs |
| --- | --- | --- |
| Windows 11, 64-bit (x64) | Supported | Nothing extra: WebView2, which draws the app's window, is part of Windows 11. |
| Windows 10, 64-bit (x64) | Supported | WebView2. Most Windows 10 PCs already have it; if not, the installer downloads and installs it, which needs an internet connection. |
| Windows Server 2019, 2022 or 2025 with Desktop Experience | Not tested | May work for a user signed in at the desktop or over Remote Desktop. Server does not ship WebView2, so the installer downloads it. Trading stops when that user signs out. |
| Windows Server Core, Nano Server | Not supported | No desktop to show the app in. |
| Windows on ARM, 32-bit Windows, Windows 8.1 and older | Not supported | No installer is built for them. |
| macOS 10.15 (Catalina) or later, Apple Silicon or Intel | Supported | Pick the `.dmg` for your chip. |
| Ubuntu 22.04 or later, Debian 12 or later, Linux Mint 21 or later, 64-bit (x64) | Supported | WebKitGTK 4.1 and GTK 3: `sudo apt install ./<file>.deb` installs them. The AppImage needs them and FUSE 2 installed. |
| Other 64-bit Linux distributions | Not tested | The AppImage may run where WebKitGTK 4.1, GTK 3 and glibc 2.35 or newer are installed. |
| Raspberry Pi 4 or 5 with 64-bit Raspberry Pi OS (Bookworm or later), desktop edition | Supported | As for Linux above. 4 GB of memory or more is recommended. |
| Raspberry Pi 3 or Zero 2 W with 64-bit Raspberry Pi OS | Not tested | 1 GB of memory or less is too little for comfortable use. |
| 32-bit Raspberry Pi OS (any Pi) | Not supported | No 32-bit build. |
| Headless Linux (no desktop: servers, Raspberry Pi OS Lite, SSH-only), Docker | Not supported | The app opens a window and cannot run without a desktop session. Use [OpenAlgo web](https://github.com/marketcalls/openalgo) for a server. |
| Running as a background service (Windows service, systemd unit, launchd daemon) | Not supported | The app runs as the signed-in user, with that user's keychain. |

"Supported" means an installer is built for that system and a problem on it
is a bug to report. The automated tests run on every change on the build
machines GitHub provides: Ubuntu 22.04 on x64 and on 64-bit ARM, the current
macOS on Apple Silicon (the Intel Mac installer is built there too, but not
run), and Windows Server with Desktop Experience. The installers have not
yet been installed and used by hand on every system in the table.

A computer with no internet connection and no WebView2 (some Windows 10 and
Windows Server installs) cannot install it from the setup program: install
the WebView2 "Evergreen Standalone Installer" from Microsoft first, then run
the OpenAlgo setup.

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
| Raspberry Pi 4 or 5, 64-bit Raspberry Pi OS | ends in `_arm64.deb` or `_aarch64.AppImage` | `.deb`: `sudo apt install ./<file>.deb`. AppImage: see below. |

Not sure which Mac you have? Open the Apple menu, then About This Mac. "Chip:
Apple M..." means Apple Silicon; "Processor: Intel" means Intel.

Raspberry Pi needs the 64-bit Raspberry Pi OS. The 32-bit image cannot run it.

To run an AppImage on Linux or Raspberry Pi, make it executable and start it:

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
| Linux, Raspberry Pi | `sha256sum <file>` |

## Opening the app the first time (unsigned installers)

The installers are not yet code-signed, so Windows and macOS warn you the first
time you open the app. This is expected. Download only from the GitHub Releases
page above, and check the checksum if you want to be sure the file is the one
that was published.

**Windows.** SmartScreen shows "Windows protected your PC". Choose **More
info**, then **Run anyway**.

**macOS.** The first time, macOS says the app "cannot be opened because Apple
cannot check it for malicious software", or that it is from an unidentified
developer. Either:

- In Finder, open Applications, right-click (or Control-click) OpenAlgo
  Desktop, choose **Open**, then **Open** again; or
- Try to open the app once, then go to **System Settings**, **Privacy &
  Security**, scroll down to the message about OpenAlgo Desktop, choose **Open
  Anyway**, and confirm. On macOS 15 (Sequoia) and later this is the way that
  works.

You only need to do this once. If macOS says the app "is damaged and can't be
opened", it is the same check; in Terminal run
`xattr -dr com.apple.quarantine "/Applications/OpenAlgo Desktop.app"` and open
it again.

**Linux and Raspberry Pi** show no warning.

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
| Linux or Raspberry Pi (AppImage) | `./OpenAlgo*.AppImage` from the folder you saved it to | Close the window |

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

1. **Create your account.** The first screen asks for a username, email and
   password. This account exists only on your computer. Your API key is
   created automatically at the same time; find it on the **API Key** page.
2. **Add your broker.** See the next section.
3. **Connect.** On the **Broker** page, pick your broker and choose **Connect
   Account**. The master contract (the list of symbols) downloads after you
   connect; the **Master Contract** page shows its progress.

When you open the app later, sign in with your password. Your broker session
from the same trading day is resumed until the broker's daily cut-off (around
03:00 IST); after that, connect to the broker again.

## Connecting your broker

There is no `.env` file. Broker credentials are entered in the app:

1. Open **Profile**, then the **Broker** tab (the **Configure broker** button
   on the Broker page takes you there).
2. Under **Update Credentials**, choose your broker and enter the **Broker API
   Key** and **Broker API Secret** from your broker's developer portal. Some
   brokers also ask for a **Client ID** or for market data keys; the form shows
   the fields your broker needs.
3. Choose **Save Broker Credentials**.
4. Go to the **Broker** page and choose **Connect Account**.

**Redirect URL.** When you create an app on your broker's developer portal, set
its redirect URL to

```
http://127.0.0.1:5000/<broker>/callback
```

for example `http://127.0.0.1:5000/zerodha/callback` or
`http://127.0.0.1:5000/fyers/callback`. This is the same address OpenAlgo web
uses, so a broker app you already set up for OpenAlgo web on this computer
works as it is. The **Current Configuration** card on the Broker tab shows the
exact redirect URL to use.

**Switching brokers.** Save credentials for as many brokers as you like. To
switch, choose the other broker on the Broker tab in Profile and save, or pick
it on the Broker page, then connect. Switching ends the current broker session
(OpenAlgo asks you to confirm while one is live; open positions stay at the
broker), so only one broker is connected at a time. No restart is needed.

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
using sandbox mode.

## Ports

| What | Default port | Changed in |
| --- | --- | --- |
| App, API and broker redirects | 5000 | Profile menu, **Server Settings**, **App port** |
| Market data WebSocket | 8765 | Profile menu, **Server Settings**, **Market data port** |

By default OpenAlgo Desktop only accepts connections from this computer. To use
it from another device on your network, turn on **Allow access from other
devices** in Server Settings. New ports and addresses take effect after the app
restarts.

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
| Linux and Raspberry Pi | `~/.local/share/com.openalgo.desktop` |

Inside it: `openalgo.db` (account, settings, strategies, encrypted broker
credentials), `sandbox.db` (sandbox mode), `logs.db` (order and API logs),
`historify.duckdb` (Historify market data), and your OpenScript and custom
indicator files.

Broker credentials, tokens and your API key are encrypted. The encryption key
is kept in your operating system's keychain (macOS Keychain, Windows Credential
Manager, or the Secret Service on Linux), not in this folder. On a Linux
machine with no keychain (for example Raspberry Pi OS Lite) the key is instead
protected by your OpenAlgo password and kept in `vault.json` in the same
folder; the app tells you when it is working this way.

**To back up**, close OpenAlgo Desktop and copy the whole folder. Restore it by
closing the app and copying the folder back.

A backup restores on the same computer and user account, where the keychain
still holds the key. On a new computer the keychain does not have that key, so
the copied account and credentials cannot be opened there. To move to a new
computer, install OpenAlgo Desktop there, create your account, and add your
broker again. (A backup made in password mode, with `vault.json`, opens on any
computer with your OpenAlgo password.)

Resetting your account (Reset account, on the forgotten-password page) replaces
the key in the keychain. The account and saved credentials in backups made
before a reset can then no longer be opened, because the key they need is
gone; make a new backup once you have set OpenAlgo up again. (A password-mode
backup carries its own `vault.json` and still opens with the password you had
then.)

## Supported brokers

All 36 brokers of OpenAlgo web, including Delta Exchange for crypto:

| | | | |
| --- | --- | --- | --- |
| 5 Paisa | 5 Paisa (XTS) | Alice Blue | Angel One |
| Arrow | CompositEdge | Definedge | Delta Exchange |
| Dhan | Dhan (Sandbox) | Firstock | Flattrade |
| Fyers | Groww | HDFC Securities | HDFC Sky |
| Ibulls | IIFL | IIFL Capital | IndMoney |
| JainamXts | Kotak Securities | Motilal Oswal | mStock by Mirae Asset |
| Nubra | Paytm Money | Pocketful | RMoney |
| Samco | Shoonya | Tradejini | TradeSmart |
| Upstox | Wisdom Capital | Zebu | Zerodha |

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

## Guides

- [Moving from OpenAlgo web](docs/user/moving-from-openalgo-web.md)
- [Broker sign-in](docs/user/brokers.md)
- [Connecting Claude Desktop and Claude Code (MCP)](docs/user/mcp.md)
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
| Node.js | 22 LTS recommended (20.20+, 22.22+ or 24.13+ work) | [nodejs.org](https://nodejs.org/) or a version manager such as `nvm` |
| Rust | 1.92.0, installed through `rustup` | [rustup.rs](https://rustup.rs/). The repository pins the version in `src-tauri/rust-toolchain.toml`, so `rustup` installs the right one automatically the first time you build |

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

**Linux (Ubuntu 22.04+, Debian 12+, Linux Mint 21+) and Raspberry Pi OS 64-bit**

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
(`.github/actions/linux-tauri-deps`). Building on a Raspberry Pi works but is
slow; a Pi 5 with 8 GB of memory is the practical minimum.

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
- **It uses the same data folder and keychain entries as the installed app**
  (see [Where your data lives](#where-your-data-lives-and-backups)). Close the
  installed app first, and back up the data folder if it holds an account you
  care about.

To run only the server, with no window, a temporary data folder and an
in-memory keychain, so nothing touches your real data:

```bash
npm run build                                          # build the interface into dist/
cd src-tauri && cargo run --example dev_server         # then open http://127.0.0.1:5500
```

Stop it with Ctrl+C. Every start begins with an empty account.

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
| Linux, Raspberry Pi | `deb/*.deb` and `appimage/*.AppImage` |

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
| `error: toolchain '1.92.0' is not installed` | Run `rustup toolchain install 1.92.0`, or any `cargo` command inside the repository, which installs it |
| Port 5500 already in use | A development copy or `dev_server` is still running; stop it. Find the program with `lsof -nP -iTCP:5500 -sTCP:LISTEN` (macOS, Linux) or `netstat -ano \| findstr :5500` (Windows) |
| The browser at `127.0.0.1:5500` says the interface is not built | Run `npm run build` once; the server reads the built interface from `dist/` |
| A build runs out of memory or disk | Rust builds need several GB of free memory and about 20 GB of disk. Close other heavy programs, or reduce parallel jobs with `CARGO_BUILD_JOBS=2` |

Read [CLAUDE.md](CLAUDE.md) before contributing; it describes the
compatibility contract with OpenAlgo web and the project conventions. Report
problems on [GitHub Issues](https://github.com/marketcalls/openalgo-desktop/issues).

## Community

- Discord: [Join our community](https://discord.com/invite/UPh7QPsNhP)
- X: [@openalgoHQ](https://twitter.com/openalgoHQ)
- YouTube: [@openalgo](https://www.youtube.com/@openalgo)

## License

OpenAlgo Desktop is released under the GNU Affero General Public License v3.0.
See [License.md](License.md).

## Disclaimer

**This software is for educational purposes only. Do not risk money which you
are afraid to lose. USE THE SOFTWARE AT YOUR OWN RISK. THE AUTHORS AND ALL
AFFILIATES ASSUME NO RESPONSIBILITY FOR YOUR TRADING RESULTS.**

Test your strategies in sandbox mode before trading with real money. Past
performance does not guarantee future results. Trading involves substantial
risk of loss.

---

Part of the [OpenAlgo FOSS Ecosystem](https://docs.openalgo.in/mini-foss-universe).
