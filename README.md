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
- [Download and install](#download-and-install)
- [Opening the app the first time (unsigned installers)](#opening-the-app-the-first-time-unsigned-installers)
- [First run](#first-run)
- [Connecting your broker](#connecting-your-broker)
- [Sandbox mode (analyzer mode)](#sandbox-mode-analyzer-mode)
- [Ports](#ports)
- [Where your data lives, and backups](#where-your-data-lives-and-backups)
- [Supported brokers](#supported-brokers)
- [What is included, and what is not](#what-is-included-and-what-is-not)
- [Guides](#guides)
- [Building from source](#building-from-source)
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
first, so only one broker is connected at a time. No restart is needed.

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

Not included, because they need a Python runtime that the desktop does not
ship:

- the Python Strategy Host (`/python`)
- Flow (`/flow`)
- the pandas-based backtesters: Portfolio Backtester, SIP Backtester and
  Portfolio Analyzer

Coming later: the **Agent**. Its pages are present but say it is not available
in the desktop yet.

Other differences from the web: one user per installation, and one broker
connected at a time.

## Guides

- [Moving from OpenAlgo web](docs/user/moving-from-openalgo-web.md)
- [Broker sign-in](docs/user/brokers.md)
- [Connecting Claude Desktop and Claude Code (MCP)](docs/user/mcp.md)
- [Troubleshooting](docs/user/troubleshooting.md)
- [Changelog](CHANGELOG.md)
- OpenAlgo documentation: [docs.openalgo.in](https://docs.openalgo.in)

## Building from source

For developers. You need Node.js 22, Rust 1.92 (pinned in
`rust-toolchain.toml`) and the Tauri 2 prerequisites for your system:

- macOS: `xcode-select --install`
- Windows: Visual Studio 2022 Build Tools with the C++ workload, and WebView2
- Linux and Raspberry Pi:
  `sudo apt install build-essential curl wget file pkg-config patchelf libssl-dev libdbus-1-dev libxdo-dev libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev`

```bash
npm ci
npm run tauri:dev     # development build (uses ports 5500 and 8766)
npm run tauri:build   # installers for this machine, in src-tauri/target/release/bundle
```

Tests: `npm run test:run` (frontend) and `cargo test` in `src-tauri` (Rust).
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
