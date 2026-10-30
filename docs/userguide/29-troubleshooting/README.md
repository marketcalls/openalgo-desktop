# 29 - Troubleshooting

## Introduction

This page covers the problems traders meet most often in OpenAlgo Desktop and
the two places in the app that tell you what is wrong: **Diagnostics** and the
**Health Monitor**. The full reference, with every message and its fix, is
[Troubleshooting](../../user/troubleshooting.md) in the desktop's user docs:

- [The app says a port is already in use](../../user/troubleshooting.md#the-app-says-a-port-is-already-in-use)
- [Broker sign-in fails](../../user/troubleshooting.md#broker-sign-in-fails)
- [Master contract errors](../../user/troubleshooting.md#master-contract-errors)
- [Historify says some changes could not be recovered](../../user/troubleshooting.md#historify-says-some-changes-could-not-be-recovered)
- [Quitting OpenAlgo, and when the computer sleeps](../../user/troubleshooting.md#quitting-openalgo-and-when-the-computer-sleeps)
- [Another device, a firewall, or a tunnel](../../user/troubleshooting.md#another-device-a-firewall-or-a-tunnel)
- [Your API client says the API key is invalid](../../user/troubleshooting.md#your-api-client-says-the-api-key-is-invalid)
- [Signing in, and a forgotten password](../../user/troubleshooting.md#signing-in-and-a-forgotten-password)
- [OpenAlgo could not open your data](../../user/troubleshooting.md#openalgo-could-not-open-your-data)

Updates have their own page: [Updating OpenAlgo Desktop](../../user/updating.md#if-an-update-fails).

There is no Python, `.env`, `uv` or server to fix in the desktop: every
setting is inside the app.

## Quick checklist

1. Is OpenAlgo Desktop open, and are you **signed in**? After a restart the
   API, webhooks, AI clients and the market data feed stay off until you sign
   in.
2. Is your **broker logged in** today? Broker sessions end around 03:00 IST.
3. **Live Mode** or **Analyze Mode**? In analyzer mode nothing reaches your
   broker.
4. **Semi-Auto** on the API Key page? Then orders from your tools wait in the
   [Action Center](../22-action-center/README.md).
5. Is the computer **awake** and online? OpenAlgo trades only while it runs.
6. What did OpenAlgo answer? Check **Logs**, **Live Logs**, and the
   [Traffic Monitor](../26-traffic-logs/README.md).

## Port 5000 is already in use

If another program holds port 5000, OpenAlgo opens on a page saying "OpenAlgo
could not start because port 5000 is already used by another program", with a
**Port** box and **Try again**.

- **On a Mac this is almost always AirPlay Receiver.** Open **System
  Settings**, **General**, **AirDrop & Handoff**, turn off **AirPlay
  Receiver**, then choose **Try again**.
- If OpenAlgo web or a second copy of OpenAlgo Desktop runs on this computer,
  close it and choose **Try again**.
- Or type a free port (for example 5001) and choose **Try again**. Then update
  your broker app's redirect URL, the base URL in your scripts and platforms,
  and the `--url` of your AI client's MCP setup.

If port **8765** (market data) is taken, the app still opens but the feed for
your SDK and platforms waits for the port; **Server Settings** shows "Live
market data is not running" and the Health Monitor raises an alert. Close the
other program or choose another **Market data port** in Server Settings, save
and restart.

## "Sign in to OpenAlgo to use the API"

OpenAlgo locks its keys whenever it starts and whenever you log out (see
[27 - Security Settings](../27-security-settings/README.md#locked-until-you-sign-in)).
Until you sign in:

| Who asks | What they get |
| --- | --- |
| A program calling `/api/v1` | 503: "Sign in to OpenAlgo to use the API. The API is off after OpenAlgo starts and after you log out, until you sign in again." |
| The market data feed (port 8765) | "OpenAlgo is locked. Sign in to OpenAlgo to stream market data..." |
| A strategy, Chartink or Flow webhook | "OpenAlgo is locked, so this alert was not acted on..." |
| A Flow schedule, price alert or order update | Nothing runs; the workflow's run history says "Skipped: OpenAlgo was signed out when this schedule fired..." |
| An AI client (MCP) | A sign-in message; nothing runs |

Your API key is not the problem. Sign in to OpenAlgo in its window (or in a
browser on this computer), and everything works again. Alerts that arrived
while it was locked are not replayed; check your positions.

If OpenAlgo must run unattended, keep it open and signed in, and keep the
computer awake. Logging out or quitting locks it again.

## The broker session ended (03:00 IST)

Indian brokers end API sessions every night, so OpenAlgo ends the broker
session at about **03:00 IST** and your sign-in session with it.

- The app sends you to sign in again, then to **Select Broker** to log in to
  your broker.
- Until you log in to the broker, `/api/v1` calls that need it answer
  `Invalid openalgo apikey` (403). This is the web's answer for a missing
  broker session, not a wrong key: log in to the broker first.
- A broker login made after 03:00 IST is kept, even if the computer was
  asleep at 03:00.
- Delta Exchange trades around the clock: its broker session stays connected
  across 03:00 IST (you still sign in to OpenAlgo again).
- A Chartink square-off or strategy action whose time passed while the
  computer slept runs late if it is still the same session, and is skipped
  (and noted in the log) if a new session has begun.

Details: [Broker sign-in](../../user/brokers.md#signing-in-to-openalgo-and-the-daily-broker-sign-in).

## Check the Health page and run diagnostics

### Diagnostics: Run checks

Open **Admin**, then **Diagnostics** (`/admin/diagnostics`). The page shows
the trading mode, this computer, the app's build, its databases (size and
last change) and recent errors. In **Latency & Connectivity Probes**, choose
**Run checks**:

| Check | Tells you |
| --- | --- |
| Database read (openalgo.db), Database read (logs.db) | The databases can be read |
| App and API listener, Market data feed listener | Something answers on the app port and the feed port |
| Broker session | Which broker is connected, or "No broker is connected" |
| **Market data from the broker** | Whether the broker's price feed is really live, and how fresh it is |
| **Order updates from the broker** | The same for the broker's order-update stream, for brokers that have one |

"Broker session: Connected" alone only proves a session exists. The two
stream lines tell you whether data is arriving:

| Line says | Means |
| --- | --- |
| "Connected, 4 instruments subscribed; last update 3 s ago." | Live and fresh |
| "Connected, no instruments subscribed; no update yet." | Connected, but nothing is subscribed yet |
| "Reconnecting to the broker (attempt 3). Prices and order updates resume once it connects." | The feed dropped and OpenAlgo is reconnecting |
| "The broker refused the session. Sign in to your broker again from the Broker page." | The broker ended the session |
| "Not connected. If this lasts, sign in to your broker again from the Broker page." | No feed |

A last update many minutes old during market hours means prices are stale:
sandbox fills, stops and charts depend on them.

The stream lines also say how many updates arrived since the feed connected
and whether any part of OpenAlgo fell behind and skipped some, for example
"12345 updates since it connected, none missed.", or "500 updates since it
connected; 40 reached a reader that had fallen behind and were skipped
(scalping stops 30, apps on the market data feed 10). Each acts on the next
update." The readers are strategies, scalping stops, sandbox orders, apps on
the market data feed, and order updates. A few skips under a burst are
harmless (each acts on the next update); steady skips mean the computer is
overloaded.

**Download .md** or **Download .txt** saves a report of all of this to attach
when you ask for help. It never contains secrets or tokens.

### Health Monitor

Open **Logs**, then **Health Monitor** (`/health`). It samples OpenAlgo itself
and shows:

- **System Status**: PASS, WARN or FAIL, with the time of the last sample. A
  missing or old sample shows UNKNOWN, never the last PASS.
- **File Descriptors** (shown as **Process Handles** when the system reports
  no limit), **Memory Usage**,
  **Database Connections**, **WebSocket Connections** and **Active Threads**,
  with 6-hour charts.
- **Unconfirmed Orders**: orders your broker neither confirmed nor shows. They
  are never sent again. Check the broker's order book and trade book; if an
  order is not there, choose **Not placed**, then **Confirm not placed**, to
  release what was waiting on it.
- **Active Alerts**: problems OpenAlgo found, such as the market data port in
  use, the master contract unavailable, records that could not be saved,
  orders whose outcome is not known, or Historify data that could not be
  recovered. **Acknowledge** marks one as seen.
- **Export CSV**: the last 24 hours of samples.

Each database's size is shown in the **Databases** section of the
Diagnostics page. The health samples also record each database's growth per
day over the last week; the Health Monitor page does not draw it yet. Watch
the data folder's free disk space if Historify or the logs grow quickly.

## Other common problems

| Problem | Where to look |
| --- | --- |
| Broker login fails | [Broker sign-in fails](../../user/troubleshooting.md#broker-sign-in-fails) |
| Symbols missing, option chain empty | [Master contract errors](../../user/troubleshooting.md#master-contract-errors); the **Master Contract** page |
| A TradingView, GoCharting or Chartink alert did nothing | The [Traffic Monitor](../26-traffic-logs/README.md): no line means it never arrived (tunnel, address); an error line tells you why |
| An order is in the Action Center | Semi-Auto is on. See [22 - Action Center](../22-action-center/README.md) |
| An option tool says open interest did not come back | Your broker answered without OI for some options; see [31 - Tools](../31-tools/README.md#when-open-interest-is-missing) |
| Sandbox orders do not fill | No broker connected, or no recent price for the symbol: see [Sandbox mode](../../user/sandbox.md) |
| "Fractional quantities are not supported in OpenAlgo Desktop yet" | Use a whole-number quantity. Only crypto orders to the live broker may be fractional |
| The app will not start: "OpenAlgo could not open your data" | [OpenAlgo could not open your data](../../user/troubleshooting.md#openalgo-could-not-open-your-data) |
| An update failed | [Updating OpenAlgo Desktop](../../user/updating.md#if-an-update-fails) |

## Asking for help

Ask on [Discord](https://openalgo.in/discord) or open a
[GitHub issue](https://github.com/marketcalls/openalgo-desktop/issues). Attach
the Diagnostics report (**Download .md**) and say what you did, what you
expected and what happened. Never share your password, API key, broker keys,
MCP tokens or webhook addresses.

---

**Previous**: [28 - Two-Factor Authentication](../28-two-factor-auth/README.md)

**Next**: [30 - FAQs](../30-faqs/README.md)
