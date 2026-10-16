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
macOS and Linux (x64 or ARM64), with the same screens, the same API and the
same market data feed as the web version. There is no server to set up, no Python to
install and no `.env` file: every setting and credential is entered inside the
app, and secrets are kept encrypted with a key locked by your OpenAlgo password.

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
- [Updating OpenAlgo Desktop](#updating-openalgo-desktop)
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

After every change that reaches the main branch, the installers built from
it are installed on fresh machines and started as a trader would on first
use, with the account created and signed in: the `.deb` and the AppImage on
Ubuntu 22.04 (x64 and 64-bit ARM), the setup program on Windows Server 2022
and Windows Server 2025, and the `.dmg` on Apple Silicon macOS (the "Install
smoke" checks under the repository's Actions tab). A release is built only
from a change that passed them. Windows 10 and 11, Intel Macs, ARM64 Linux
computers and the other systems in the table have not yet been installed and
used by hand.

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

**Linux** (x64 or ARM64) shows no warning.

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
