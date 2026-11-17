# 02 - Why Build with OpenAlgo?

*"Why use OpenAlgo when I can call my broker's API directly?"*

Many traders start with a broker's SDK: wire a signal to an order call and you
are trading. Then the questions arrive:

- How do I see what my strategy did, order by order?
- How do I test a webhook or a script without risking money?
- How do I find the right symbol, expiry and lot size for every contract?
- What do I rewrite when I move from one broker to another?
- How does my TradingView alert reach a script on my computer?

A broker SDK gives you access. OpenAlgo gives you the rest: one API across
brokers, a sandbox, books and logs, symbols, risk rules and the screens to
watch it all. OpenAlgo Desktop puts that on your own computer, ready after
one installer.

---

## What makes OpenAlgo different

### One API for every broker

Your code calls OpenAlgo; OpenAlgo calls your broker. Order fields, symbols,
status values and error messages are the same whichever of the 39 brokers is
logged in, so a script written for one broker runs against another.

```
        Your strategy code (written once)
                     |
              OpenAlgo unified API
                     |
   Zerodha | Angel One | Dhan | Fyers | Upstox | 32 more
```

In OpenAlgo Desktop you save keys for each broker you use and switch between
them from the **Select Broker** page, without a restart. One broker is logged
in at a time.

### One symbol format

OpenAlgo writes every instrument the same way for every broker:

| Instrument | OpenAlgo symbol |
| --- | --- |
| State Bank of India on NSE | `SBIN` |
| A NIFTY future | `NIFTY28OCT26FUT` |
| A NIFTY call option | `NIFTY28OCT2625000CE` |

The master contract (each broker's list of instruments) is downloaded after
you log in to your broker and mapped to these names, with lot sizes, tick
sizes and expiries. Contract names above are examples of the format, not
live contracts.

### A sandbox that behaves like your account

Analyzer mode sends every order to a simulated account with 1 crore of
capital, priced from your broker's live data, with margin, positions,
holdings and MIS square-off. The API answers the same way it does live, so a
script or webhook can be tested unchanged. See
[chapter 15](../15-analyzer-mode/README.md).

### Signals from anywhere

| Source | How it reaches OpenAlgo |
| --- | --- |
| TradingView | Webhook alerts ([chapter 16](../16-tradingview-integration/README.md)) |
| Amibroker | HTTP calls from AFL |
| Chartink | Scanner webhooks |
| GoCharting | Webhook alerts |
| Python | The `openalgo` package on PyPI |
| Excel, Google Sheets, other languages | The REST API |
| Claude Desktop, Claude Code | The built-in MCP server |

### Watching what happened

| Tool | What it shows |
| --- | --- |
| Order book, trade book, positions, holdings | Your books, refreshed as orders change |
| Live Logs and Sandbox Logs | Every API request and its answer |
| Latency Monitor | How long orders took |
| Traffic Monitor | Requests, status and duration |
| Health Monitor | Feed health, held orders and alerts |
| P&L Tracker | Your intraday P&L curve |
| Telegram and WhatsApp | Alerts on your phone |

---

## Why a desktop app

| You get | Because |
| --- | --- |
| Nothing to set up | One installer for Windows, macOS or Linux; no Python, no `.env`, no web server |
| Keys that stay yours | Credentials are encrypted on your computer and locked with your OpenAlgo password |
| The web's screens and API | The interface and `/api/v1` are carried over from OpenAlgo web, so tutorials and tools carry over too |
| In-app updates | The app tells you when a new version is out and installs it when you choose ([guide](../../user/updating.md)) |

The trade-off: OpenAlgo Desktop trades only while it is open and your
computer is awake, and webhooks from the internet need a tunnel. For a
server that runs around the clock, or a Raspberry Pi, use
[OpenAlgo web](https://github.com/marketcalls/openalgo).

---

## Security by default

| Protection | In OpenAlgo Desktop |
| --- | --- |
| Encrypted secrets | Broker keys, tokens, the API key and the two-factor secret are encrypted; the key that opens them is locked with your password |
| Locked until you sign in | After the app starts, the API, the market data feed, webhooks and AI clients stay off until you sign in |
| This computer only | The app listens on `127.0.0.1` unless you allow other devices in **Server Settings** |
| Two-factor sign-in | Optional authenticator code at sign-in (Profile, **TOTP** tab) |
| Rate limits and lockouts | API rate limits as on the web; repeated failed sign-ins slow down |
| Signed updates | An update is installed only if its signature matches the key built into the app |

---

## Why not call broker APIs directly?

| You would build | OpenAlgo Desktop includes |
| --- | --- |
| A sandbox to test in | Analyzer mode with 1 crore of simulated capital |
| Symbol and expiry mapping | One symbol format, master contracts downloaded for you |
| A dashboard | The full OpenAlgo interface |
| Order logs | Live Logs, Sandbox Logs, traffic and latency monitors |
| Support for more than one broker | 39 brokers behind one API |
| Webhook receivers | TradingView, Chartink, GoCharting and strategy webhooks |
| Alerts | Telegram and WhatsApp |
| Risk rules | Stops, targets and trailing rules in the strategy module and scalping |

---

## Open source

OpenAlgo Desktop is released under the GNU Affero General Public License
v3.0, like OpenAlgo web: read the code, change it, build it yourself. There
are no per-order fees. Help comes from the community on
[Discord](https://discord.com/invite/UPh7QPsNhP) and
[GitHub](https://github.com/marketcalls/openalgo-desktop/issues).

---

**Broker APIs give you access. OpenAlgo gives you the platform around it.**

---

**Previous**: [01 - What is OpenAlgo Desktop](../01-what-is-openalgo/README.md)

**Next**: [03 - Key concepts](../03-key-concepts/README.md)
