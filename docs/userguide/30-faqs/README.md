# 30 - Frequently Asked Questions

## General

### What is OpenAlgo Desktop?

A single-user desktop version of [OpenAlgo web](https://github.com/marketcalls/openalgo).
You install it on your own computer, sign in, connect your broker and trade.
It has the same screens, the same REST API and the same market data feed as
the web version, so the Python SDK, TradingView, Amibroker, Chartink,
GoCharting, Excel and AI clients work with it unchanged.

### Is it free?

Yes. It is open source under the GNU Affero General Public License v3.0.
Broker API subscriptions, market data plans, ChartInk or GoCharting plans and
any tunnel service you use may cost money.

### Which brokers are supported?

37 brokers, the same as OpenAlgo web: Indian brokers from 5 Paisa to Zerodha
(Rupeezy included), and Delta Exchange for crypto. One set of keys can be saved for each broker, and one
broker is logged in at a time. How each one signs in:
[Broker sign-in](../../user/brokers.md).

### Which computers does it run on?

Windows 10 and 11 (64-bit), macOS 10.15 or later (Apple Silicon and Intel),
and 64-bit Linux on x64 or ARM64 (Ubuntu 22.04 or later, Debian 12 or later,
Linux Mint 21 or later). It needs a desktop session: it does not run on a
headless server, in Docker or as a background service. It does not support
Raspberry Pi: use OpenAlgo web on a Pi. The full table is in the
[README](../../../README.md#supported-systems).

### Do I need Python, a `.env` file or a server?

No. There is nothing to install besides the app. Every setting and credential
is entered inside it.

### What is not included in the desktop?

| Not in OpenAlgo Desktop | Use instead |
| --- | --- |
| The Python Strategy Host (`/python`) | Run your Python scripts yourself with the OpenAlgo SDK against the desktop's API, or use OpenAlgo web. See [20 - Python Strategies](../20-python-strategies/README.md) |
| Flow (`/flow`) | OpenAlgo web. See [21 - Flow Visual Builder](../21-flow-visual-builder/README.md) |
| Portfolio Backtester, SIP Backtester, Portfolio Analyzer | OpenAlgo web |
| The Agent (`/agent`, and the Assistant panel in `/trading`) | Connect Claude Desktop or Claude Code through MCP: [MCP](../../user/mcp.md) |

These were left out so the desktop needs no Python runtime (maintainer
decisions, 2026-10-03 and 2026-10-12). The app has no pages, menu entries or
buttons for them.

### Can I have several users or several brokers at once?

One user per installation, and one broker logged in at a time. You can save
keys for as many brokers as you like and switch between them without a
restart.

### Can OpenAlgo web and OpenAlgo Desktop run on the same computer?

Yes, on different ports. Both use 5000 and 8765 by default; change the
desktop's **App port** and **Market data port** in **Server Settings** (profile
menu) and restart. Only one copy of OpenAlgo Desktop can run at a time.

### Does it keep trading when I close it, or when the computer sleeps?

No. OpenAlgo trades only while it is open and the computer is awake. Closing
it stops the API, the feed, strategies, OpenScript runs and stop and target
tracking; it asks first if something is running. Orders already at your
broker stay there. When the computer wakes, OpenAlgo reconnects and catches
up. See [Quitting OpenAlgo, and when the computer sleeps](../../user/troubleshooting.md#quitting-openalgo-and-when-the-computer-sleeps).

## Updates and your data

### How do I update?

OpenAlgo Desktop checks for a newer version each time it starts; choose
**Check for Updates** in the profile menu to check at any time. When there is
one, a notice shows the version and its release notes. It only tells you:
**nothing is downloaded until you choose Update**, and it never restarts on
its own. **Update** downloads the new version, checks its signature, installs
it and restarts; then you sign in again. Before it restarts it lists anything
a restart would interrupt and asks you to confirm.

A `.deb` install on Linux is not replaced in place: the app tells you and
links to the release page. Details: [Updating OpenAlgo Desktop](../../user/updating.md).

### Is my data safe during an update?

Yes. The first time a new version opens your data, it copies every database
into `backups/` inside the data folder before changing anything, and puts the
copy back if it cannot open the data. The copy is deleted once the new version
has opened your data. An older version never opens data a newer one has used.

### Where is my data, and how do I back it up?

Everything is in one folder: `%APPDATA%\com.openalgo.desktop` on Windows,
`~/Library/Application Support/com.openalgo.desktop` on macOS,
`~/.local/share/com.openalgo.desktop` on Linux. To back up, close OpenAlgo and
copy the whole folder. A backup opens only with the password you had when you
made it. Nothing from an OpenAlgo web database is imported.

## Security

### How secure is OpenAlgo Desktop?

OpenAlgo Desktop keeps your broker credentials, API key and two-factor secret
encrypted on your computer, locked with your OpenAlgo password. Its
connections to your broker use the broker's own secure (https and wss)
addresses. The app itself serves your browser and your tools over plain http
and ws on this computer (127.0.0.1), which never leaves the machine. If you
allow access from other devices in Server Settings, that traffic is not
encrypted: use it only on a network you trust, or reach OpenAlgo through a
tunnel with https. It also uses CSRF protection, rate limiting and secure
sessions. Use a strong password and turn on two-factor sign-in.

More: [27 - Security Settings](../27-security-settings/README.md).

### Does it use my computer's keychain?

No, on no platform. The encryption key is wrapped with a key derived from
your OpenAlgo password and kept in `openalgo.db`. Earlier builds that used the
keychain move their keys out of it once, at the first sign-in in the desktop
window.

### Why does my API stop working after I restart OpenAlgo?

Because the keys that check your API key are locked until you sign in. After
OpenAlgo starts, and after you log out, programs are told "Sign in to OpenAlgo
to use the API". Sign in and they work again.

### I forgot my password. Can it be recovered?

No. Choose **Forgot password?** on the sign-in page, in the OpenAlgo Desktop
window, and set a new one. Your account, settings, strategies and history
stay; you enter your broker keys again, copy the new API key into your tools,
and set up two-factor sign-in, Telegram and WhatsApp again.

### Does the desktop support IP allowlisting?

A strategy webhook can carry an IP allowlist, which matches devices on your
network. The Security dashboard bans single addresses and can ban
automatically after too many invalid keys or missing pages. Callers through a
tunnel share one identity, so protect webhooks with their secret address.

## Broker sessions

### Why do I have to log in to my broker every day?

Indian brokers end API sessions every night. OpenAlgo ends the broker session
at about 03:00 IST, and you log in again from **Select Broker**. Your session
from the same trading day resumes when you sign in to OpenAlgo again. Delta
Exchange trades around the clock and stays connected across 03:00 IST.

### My API client says "Invalid openalgo apikey" in the morning

No broker is logged in yet. Without a broker session the API answers that way,
as on the web. Log in to your broker first.

## Trading and data

### What is analyzer mode?

A sandbox: every order from the app, the API, webhooks and AI clients goes to
a simulated account priced from your broker's live data, starting with 1
crore of simulated capital. Turn it on with the mode switch in the top bar.
See [Sandbox mode](../../user/sandbox.md) and
[15 - Analyzer Mode](../15-analyzer-mode/README.md).

### Can I backtest?

OpenScript strategies backtest in the charting terminal (`/trading`), over
history fetched from your broker. The pandas backtesters of OpenAlgo web
(Portfolio Backtester, SIP Backtester, Portfolio Analyzer) are not in the
desktop.

### What latency should I expect?

It depends on your internet connection and your broker. The
[Latency Monitor](../25-latency-monitor/README.md) shows your own numbers,
split into the broker's part and OpenAlgo's.

### What happens to my positions if OpenAlgo stops?

They stay at your broker. Use your broker's app to watch or close them.
OpenAlgo's own stops and targets are not checked while it is closed.

## API and integrations

### Is the API the same as OpenAlgo web's?

Yes, field for field: same endpoints under `/api/v1`, same requests, same
responses and error messages. The API key goes in the JSON body. The API
reference is at [docs.openalgo.in](https://docs.openalgo.in).

### What are the rate limits?

Fixed in the desktop (there is no `.env`): 100 API requests a second per
caller, 10 orders a second, and 100 webhook calls a minute per caller.

### Does an API key have permissions?

No. A valid key can call every `/api/v1` endpoint, subject to analyzer mode,
Semi-Auto and the rate limits. AI clients (MCP) get their own tokens with
**Read only** or **Read and place orders** access.

### How do I connect TradingView, ChartInk or GoCharting?

They send alerts from the internet, so turn on the built-in ngrok tunnel in
Profile (Broker tab, **Server Configuration**), or run a tunnel yourself and
enter its address as **Host Server URL** there. See [18 - ChartInk](../18-chartink-integration/README.md) and
[19 - GoCharting](../19-gocharting-integration/README.md). Your own scripts,
Amibroker and Excel on the same computer need no tunnel.

### How do I use the Python SDK?

`pip install openalgo`, then point it at `http://127.0.0.1:5000` and
`ws://127.0.0.1:8765` with the desktop's API key. See
[20 - Python Strategies](../20-python-strategies/README.md).

### Can I connect Claude or another AI assistant?

Yes, through the built-in MCP server. Create a token on the API Key page and
follow [Connecting Claude Desktop and Claude Code](../../user/mcp.md).

### I am moving from OpenAlgo web. What changes?

The API key, and the base URL only if your web instance ran elsewhere. See
[Moving from OpenAlgo web](../../user/moving-from-openalgo-web.md).

## Support

- Documentation: [docs.openalgo.in](https://docs.openalgo.in)
- Desktop issues: [github.com/marketcalls/openalgo-desktop/issues](https://github.com/marketcalls/openalgo-desktop/issues)
- Community: [openalgo.in/discord](https://openalgo.in/discord)

When you report a problem, attach the Diagnostics report (**Admin**,
**Diagnostics**, **Download .md**), and say what you did, what you expected and
what happened. Never share your password, API key, broker keys, MCP tokens or
webhook addresses.

---

**Previous**: [29 - Troubleshooting](../29-troubleshooting/README.md)

**Next**: [31 - Tools](../31-tools/README.md)
