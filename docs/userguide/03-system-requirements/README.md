# 03 - System Requirements

## Introduction

OpenAlgo Desktop is a desktop app. It needs a computer with a graphical
desktop and a signed-in user, and it trades only while its window is open.
This chapter tells you which computers it runs on, what else it needs, and
when OpenAlgo web is the better choice.

## Supported systems

| System | Support | What it needs |
| --- | --- | --- |
| Windows 11, 64-bit (x64) | Supported | Nothing extra: WebView2, which draws the app's window, is part of Windows 11 |
| Windows 10, 64-bit (x64) | Supported | WebView2. Most PCs have it; if not, the installer downloads it, which needs an internet connection |
| Windows Server 2022 or 2025 with Desktop Experience | Supported | A user signed in at the desktop or over Remote Desktop. Trading stops when that user signs out |
| Windows Server 2019 with Desktop Experience | Not tested | May work as on 2022 |
| macOS 10.15 (Catalina) or later, Apple Silicon or Intel | Supported | The `.dmg` for your chip |
| Ubuntu 22.04+, Debian 12+, Linux Mint 21+, 64-bit (x64) | Supported | WebKitGTK 4.1 and GTK 3 (the `.deb` installs them); FUSE 2 for the AppImage |
| Ubuntu 22.04+, Debian 12+, 64-bit ARM (ARM64), for example an ARM laptop or virtual machine | Supported | As for x64 Linux |
| Other 64-bit Linux distributions (x64 or ARM64) | Not tested | The AppImage may run where WebKitGTK 4.1, GTK 3 and glibc 2.35 or newer are installed |
| Raspberry Pi (any model) | Not supported | Use [OpenAlgo web](https://github.com/marketcalls/openalgo), which supports the Pi |
| Windows Server Core, Nano Server, headless Linux, Docker | Not supported | The app opens a window and needs a desktop. Use OpenAlgo web for a server |
| Windows on ARM, 32-bit Windows, Windows 8.1 and older | Not supported | No installer is built for them |
| Running as a background service (Windows service, systemd unit, launchd daemon) | Not supported | The app runs as the signed-in user, and its keys stay locked until that user signs in to OpenAlgo |

"Supported" means an installer is built for that system and a problem on it is
a bug to report. The README's [Supported systems](../../../README.md#supported-systems)
section says which systems are tested on every change and which have not yet
been tried by hand.

> **Raspberry Pi.** ARM64 Linux builds exist for ARM laptops, desktops and
> virtual machines. A Raspberry Pi is not a tested or targeted device for
> OpenAlgo Desktop. On a Pi, install
> [OpenAlgo web](https://github.com/marketcalls/openalgo) instead
> ([docs](https://docs.openalgo.in)).

## What else the computer needs

### No Python, no server software

Nothing besides the installer: no Python, no database server, no web server,
no `.env` file. Everything is set inside the app.

### Disk space

The app itself is small. What grows is your data, in one folder:
`openalgo.db`, `logs.db`, `sandbox.db` and `historify.duckdb` (historical
market data, the largest if you download a lot). Two things to plan for:

- **Historify.** Years of 1-minute data for many symbols take real space.
  Download what you use.
- **Updates.** The first time a new version opens your data, it copies every
  database into a backup inside the data folder before it changes anything.
  That needs about as much free space as your data. If the disk is full, the
  new version changes nothing and asks you to free space
  ([details](../../user/updating.md#your-data-is-backed-up-before-an-update)).

### Staying awake

OpenAlgo Desktop trades only while it is open and the computer is awake:

- **Closing the window** stops the server, the market data feed, strategies,
  OpenScript runs and stop/target tracking. Orders already at your broker
  stay there.
- **Sleep and network loss** pause it. When the computer wakes, OpenAlgo
  reconnects market data, checks your orders with your broker and catches up.
  Check your positions after a long sleep.

If automation must run all day, keep the computer awake (turn off sleep in
your power settings for trading hours), or run OpenAlgo web on a server.

## Network

### Internet

A steady connection to your broker. Market data and orders go over it;
outages pause the feed until it reconnects.

### Your broker's IP rules

Many brokers now accept API orders only from an internet address (a static
IP) registered with them. OpenAlgo Desktop sends orders from your computer's
own internet connection, so the address your broker must know is the one your
computer uses. Check your broker's rules before trading live; Samco and
Tradejini, for example, check it at sign-in or on every call.

### Ports

| What | Default | Changed in |
| --- | --- | --- |
| App, API and broker redirects | `127.0.0.1:5000` | Profile menu, **Server Settings**, **App port** |
| Market data WebSocket | `127.0.0.1:8765` | Profile menu, **Server Settings**, **Market data port** |

- By default the app accepts connections **from this computer only**.
  Another device on your network can reach it only after you turn on **Allow
  access from other devices** in Server Settings.
- **On a Mac, AirPlay Receiver often holds port 5000.** The app tells you at
  start and offers the fix ([chapter 04](../04-installation/README.md#if-port-5000-is-already-in-use)).
- **OpenAlgo web on the same computer** uses the same ports. Run one at a
  time, or give one of them other ports
  ([chapter 04](../04-installation/README.md#running-alongside-openalgo-web)).

### Webhooks from the internet

TradingView, Chartink and GoCharting send alerts from their servers. Your
computer is not reachable from the internet by default, so these alerts need a
tunnel: the built-in ngrok tunnel (a free ngrok account), or one you run
yourself such as Cloudflare Tunnel. Your own
scripts, Amibroker and Excel on the same computer need nothing extra. See
[chapter 16](../16-tradingview-integration/README.md).

## Choosing your setup

| You want to | Use |
| --- | --- |
| Learn OpenAlgo and test strategies | OpenAlgo Desktop on your laptop, in analyzer mode |
| Trade from your own Python scripts, Amibroker or Excel | OpenAlgo Desktop on the computer that runs them |
| Trade TradingView or Chartink alerts while you are at your desk | OpenAlgo Desktop plus a tunnel |
| Run automation all day without a computer left on | [OpenAlgo web](https://github.com/marketcalls/openalgo) on a server |
| Run on a Raspberry Pi | OpenAlgo web |
| Several users, or several accounts at one broker at once | Several OpenAlgo web instances. OpenAlgo Desktop is one user, one broker logged in at a time |

## Before you install

- [ ] A supported system from the table above, with a desktop session.
- [ ] An account with one of the 37 supported brokers and API access set up
      in its developer portal (see [chapter 06](../06-broker-connection/README.md)).
- [ ] Your broker's API key and secret (and anything else it issues).
- [ ] An authenticator app on your phone, if you want two-factor sign-in.
- [ ] If OpenAlgo web runs on this computer: a plan for the ports.
- [ ] If you trade TradingView or Chartink alerts: a tunnel service account.

## Summary

| | OpenAlgo Desktop |
| --- | --- |
| Systems | Windows 10/11 x64, macOS 10.15+ (Apple Silicon or Intel), Linux x64 and ARM64 |
| Not for | Raspberry Pi, headless servers, Docker, background services |
| Extra software | None (WebView2 on Windows 10 if missing; WebKitGTK on Linux) |
| Ports | 5000 and 8765 on `127.0.0.1`, changeable in Server Settings |
| Runs | While the window is open, the computer awake and you signed in |

---

**Previous**: [03 - Key concepts](../03-key-concepts/README.md)

**Next**: [04 - Installation](../04-installation/README.md)
