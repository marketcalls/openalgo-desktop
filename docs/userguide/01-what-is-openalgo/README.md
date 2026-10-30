# 01 - What is OpenAlgo Desktop?

## Introduction

**OpenAlgo Desktop** is OpenAlgo on your own computer. It is a free,
open-source trading platform that sits between your trading ideas and your
broker: a TradingView alert, an Amibroker signal, a Python script or a click
in the app becomes an order with your broker, through one API that works the
same for every supported broker.

It is the desktop version of [OpenAlgo web](https://github.com/marketcalls/openalgo),
built for one trader on one computer. You install it like any other app. There
is no server to set up, no Python to install and no `.env` file: every setting
and every credential is entered inside the app.

- **Website**: [openalgo.in](https://openalgo.in)
- **OpenAlgo Desktop on GitHub**: [github.com/marketcalls/openalgo-desktop](https://github.com/marketcalls/openalgo-desktop)
- **OpenAlgo documentation**: [docs.openalgo.in](https://docs.openalgo.in)

Already using OpenAlgo web? Read
[Moving from OpenAlgo web](../../user/moving-from-openalgo-web.md): your tools
keep working, and the page lists the few things you change.

## The problem it solves

Without automation, a signal turns into an order by hand:

1. TradingView shows a buy signal.
2. You open your broker's app and search for the stock.
3. You type the quantity and price and press Buy.
4. By then the signal is a minute or two old.

With OpenAlgo Desktop:

1. TradingView (or Amibroker, Chartink, your script) sends the signal to
   OpenAlgo Desktop.
2. OpenAlgo Desktop checks it and places the order with your broker.
3. The order book, positions and logs in the app show what happened.

## Who it is for

| Trader | What OpenAlgo Desktop gives you |
| --- | --- |
| Retail traders | Orders placed for you from alerts, on many symbols at once |
| Chart traders | TradingView, GoCharting and Chartink alerts turned into orders |
| Amibroker and Excel users | The same HTTP API OpenAlgo web offers |
| Python traders | The OpenAlgo Python SDK against your own computer |
| Options traders | Option chain, Greeks, OI tools, straddles, strategy builder and multi-leg orders |
| Anyone learning | Analyzer mode (the sandbox) with 1 crore of simulated capital and live prices |

## What it does

### Trading and automation

| Feature | What it does |
| --- | --- |
| Unified API (`/api/v1`) | Place, modify and cancel orders, read books and market data, the same for every broker |
| Smart orders | Orders that take your current position into account ([chapter 12](../12-smart-orders/README.md)) |
| Basket and split orders | Many symbols in one request, or one large order in pieces ([chapter 13](../13-basket-orders/README.md)) |
| Market data WebSocket | Live LTP, quotes and depth for your SDK and trading platforms |
| Strategies with risk management | Multi-leg strategies with stops, targets and trailing rules, started by webhook or by you |
| Flow | Build automations on a drag-and-drop canvas: a schedule, webhook or price alert, conditions and indicators, then orders and alerts ([chapter 21](../21-flow-visual-builder/README.md)) |
| Action Center | Semi-Auto mode: orders wait for your approval |
| Charting terminal and scalping | Trade from the chart (`/trading`) and from the keyboard (`/scalping`) |

### Testing and analysis

| Feature | What it does |
| --- | --- |
| Analyzer mode (sandbox) | Every order goes to a simulated account with 1 crore of capital, priced from live data ([chapter 15](../15-analyzer-mode/README.md)) |
| Options tools | Option chain, Greeks, OI tracker, max pain, IV charts, GEX, straddles, strategy builder |
| Historify | Download and keep historical market data on your computer |
| OpenScript | Write chart studies and strategies, backtest them and run them |
| P&L tracker, latency and traffic monitors | See how your day and your orders went |

### Alerts and AI clients

| Feature | What it does |
| --- | --- |
| Telegram and WhatsApp | Trade alerts and bot commands |
| MCP server | Let Claude Desktop, Claude Code and other AI clients read your account and, if you allow it, place orders ([guide](../../user/mcp.md)) |

## How it fits together

```
 TradingView   Amibroker   Chartink   Python SDK   AI clients (MCP)
      |            |           |           |              |
      +------------+-----------+-----------+--------------+
                               |
              OpenAlgo Desktop (on your computer)
       app window, REST API on 127.0.0.1:5000, market data on 127.0.0.1:8765
                               |
              +----------------+----------------+
              |                                 |
         your broker                   the sandbox (analyzer mode)
```

The app window and every outside tool talk to the same small server that runs
inside OpenAlgo Desktop while it is open. That is why the same dashboard also
opens in a browser on the same computer at `http://127.0.0.1:5000`.

## Brokers and exchanges

OpenAlgo Desktop supports all 38 brokers of OpenAlgo web, including Rupeezy
and Delta Exchange (crypto):

| | | | |
| --- | --- | --- | --- |
| 5 Paisa | 5 Paisa (XTS) | Alice Blue | Angel One |
| Arrow | CompositEdge | Definedge | Delta Exchange |
| Dhan | Dhan (Sandbox) | Firstock | Flattrade |
| Fyers | Groww | HDFC Securities | HDFC Sky |
| Ibulls | IIFL | IIFL Capital | IndMoney |
| JainamXts | Kotak Securities | Motilal Oswal | mStock by Mirae Asset |
| Nubra | Paytm Money | Pocketful | RMoney |
| Rupeezy | Sahi | Samco | Shoonya |
| Tradejini | TradeSmart | Upstox | Wisdom Capital |
| Zebu | Zerodha | | |

Exchanges use OpenAlgo's codes: `NSE` and `BSE` (equity), `NFO` and `BFO`
(F&O), `CDS` and `BCD` (currency), `MCX` and `NCDEX` (commodity), the index
codes `NSE_INDEX` and `BSE_INDEX`, and `CRYPTO` for Delta Exchange. Which
exchanges you can trade depends on your broker: each broker's card on the
**Select Broker** page lists them.

You save keys for as many brokers as you like and **one broker is logged in at
a time**. See [chapter 06](../06-broker-connection/README.md).

## Live mode and analyzer mode

- **Live mode**: orders go to your broker and the exchange.
- **Analyzer mode**: orders go to the sandbox, a simulated account with 1 crore
  of capital that fills orders from your broker's live prices. Nothing reaches
  your broker.

The switch is in the top bar, and the badge beside it reads **Live Mode** or
**Analyze Mode**. It applies to everything at once: the app, the API, webhooks
and AI clients. Test in analyzer mode first.

## Your data stays on your computer

| What | Where |
| --- | --- |
| Account, settings, strategies, broker keys | `openalgo.db` in the app's data folder |
| Order and API logs | `logs.db` |
| Sandbox | `sandbox.db`, kept apart from live trading |
| Historical data | `historify.duckdb` |

Broker credentials, broker tokens, your API key and your two-factor login
secret are encrypted, and the key that opens them is locked with your
OpenAlgo password. The data folder for each system is in
[chapter 04](../04-installation/README.md#where-your-data-lives).

The app connects to your broker, to GitHub when it checks for a new version,
and to the services you choose to connect (Telegram, WhatsApp, a tunnel for
webhooks).

## What OpenAlgo Desktop is not

| Not this | But this |
| --- | --- |
| A way to get rich | A tool. Results depend on your strategy |
| A strategy provider | You bring the trading ideas |
| A financial adviser | You make and own every trading decision |
| A cloud service | It runs on your computer and trades only while it is open |
| A server for many users | One person, one computer |

## How it differs from OpenAlgo web

| | OpenAlgo web | OpenAlgo Desktop |
| --- | --- | --- |
| Install | Server or computer with Python, `.env` file | One installer per system, nothing else |
| Settings and broker keys | `.env` file | Inside the app |
| Users and brokers | One user, one broker per instance | One user; keys for many brokers, one logged in at a time |
| Raspberry Pi | Supported | Not supported: use OpenAlgo web |
| Runs | All day on a server | While the app is open and you are signed in |
| Flow | Included | Included from 1.0.5, native (no Python); workflows import and export as on the web |
| Python Strategy Host, pandas backtesters | Included | Not included in this version |

Everything that talks to OpenAlgo web (the Python SDK, TradingView, Amibroker,
Chartink, GoCharting, Excel, MCP clients) talks to OpenAlgo Desktop the same
way: the API answers with the same fields and the same messages.

## Next steps

1. [02 - Why build with OpenAlgo](../02-why-build-with-openalgo/README.md)
2. [03 - Key concepts](../03-key-concepts/README.md)
3. [03 - System requirements](../03-system-requirements/README.md)
4. [04 - Installation](../04-installation/README.md)
5. [05 - First-time setup](../05-first-time-setup/README.md)
6. [10 - Placing your first order](../10-placing-first-order/README.md)

---

**Next**: [02 - Why build with OpenAlgo](../02-why-build-with-openalgo/README.md)
