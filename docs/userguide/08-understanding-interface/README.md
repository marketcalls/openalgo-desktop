# 08 - Understanding the Interface

## Introduction

OpenAlgo Desktop uses the OpenAlgo web interface, carried over nearly
unchanged, inside its own window. If you know OpenAlgo web, you know your way
around. This chapter is a map: the top bar, the profile menu, and what each
page is for.

The same pages also open in a browser on the same computer at
`http://127.0.0.1:5000` while the app runs.

## The top bar

```
+------------------------------------------------------------------------------+
| OpenAlgo | Dashboard Orderbook Tradebook Positions Trading Platforms         |
|          | Strategies Logs Tools          [broker] [Live Mode] (~) (sun) (O) |
+------------------------------------------------------------------------------+
```

### Left: the pages

| Item | Opens |
| --- | --- |
| **OpenAlgo** logo | The dashboard |
| **Dashboard** | Funds, master contract status, quick access ([chapter 07](../07-dashboard-overview/README.md)) |
| **Orderbook** | Today's orders, with modify, cancel and a GTT tab |
| **Tradebook** | Today's fills |
| **Positions** | Open positions, with close buttons ([chapter 14](../14-positions-holdings/README.md)) |
| **Trading** | The charting terminal (`/trading`) |
| **Platforms** | TradingView, GoCharting and Chartink setup pages |
| **Strategies** | The strategy module: multi-leg strategies with risk management, and their webhooks |
| **Logs** | Live Logs, Sandbox Logs, Latency Monitor, Traffic Monitor, Security Logs, Health Monitor |
| **Tools** | Options and analysis tools: option chain, Greeks, OI tracker, max pain, IV charts, GEX, straddles, strategy builder and more |

On a narrower window the last items move under **More** at the end of the
bar; the current page is underlined. On a very narrow window the bar becomes
a menu button, with Dashboard, Orderbook, Tradebook and Positions along the
bottom.

### Right: account controls

| Control | What it does |
| --- | --- |
| Broker badge | The broker logged in now (hidden on narrow windows) |
| **Live Mode** / **Analyze Mode** badge | Where orders go now. Violet means analyzer mode |
| Mode switch | Switch between Live and Analyze. Applies to everything: the app, the API, webhooks and AI clients |
| Light or dark | Switch the theme (only in Live mode; analyzer mode has its own violet look) |
| Your initial | The profile menu |

Switching mode shows "Switched to Analyze mode" (or Live), and on the way into
analyzer mode a reminder that it is for testing. See
[chapter 15](../15-analyzer-mode/README.md).

## The profile menu

Choose your initial at the right of the top bar.

| Entry | What it is |
| --- | --- |
| **Profile** | Account and password, Broker summary and server configuration, Alerts (toasts and sounds), Theme, TOTP (two-factor sign-in) |
| **API Key** | Your API key, Auto or Semi-Auto order mode, the Playground, and tokens for AI clients ([chapter 09](../09-api-key-management/README.md)) |
| **Action Center** | Orders waiting for approval in Semi-Auto mode |
| **Agent** | Ask about your account and the market in a chat, on a model you set up in **Admin**, **Agent Config**; it asks for your approval before every order |
| **Master Contract** | Symbol list status per exchange, **Force Download**, **Reload Cache** |
| **Telegram Bot** | Telegram alerts and commands |
| **WhatsApp Bot** | WhatsApp alerts and commands |
| **Holdings** | Your delivery holdings (not shown for a crypto broker) |
| **Flow Editor** | Drag-and-drop workflows: triggers, conditions, orders and alerts ([chapter 21](../21-flow-visual-builder/README.md)) |
| **Scalping** | The keyboard-driven scalping terminal |
| **PnL Tracker** | Intraday MTM curve |
| **Historify** | Download and browse historical data |
| **Search** | Find any instrument's OpenAlgo symbol |
| **Sandbox** | Analyzer-mode settings: capital, leverage, square-off times, My P&L |
| **Leverage** | Leverage settings (shown only for a crypto broker) |
| **Server Settings** | Ports, addresses, and access from other devices |
| **Admin** | Freeze quantities, market holidays and timings, Diagnostics, Agent Config, Remote MCP, security, traffic and latency dashboards |
| **Check for Updates** | Ask GitHub for a newer version ([guide](../../user/updating.md)) |
| **Docs** | [docs.openalgo.in](https://docs.openalgo.in) in your browser |
| **Logout** | Sign out after a confirmation |

In the desktop the menu has no Python Strategies entry: the Python Strategy
Host is not in this version. **Server Settings** and **Check for Updates**
exist only in the desktop.

### Logging out

**Logout** asks first, because automated orders and running strategies stop
and open positions are no longer managed. After you log out, the API,
webhooks, the market data feed and AI clients are off until you sign in again,
and the broker session ends.

## Trading pages

### Orderbook

Today's orders, with counts of buy, sell, completed, open and rejected
orders. Filter by status, export to CSV, modify or cancel an open
order, or **Cancel All**. The **GTT** tab lists GTT triggers.

### Tradebook

Every fill today: symbol, side, quantity, price and time. Export to CSV.

### Positions

Open and closed positions of the day with LTP, P&L and P&L %, a **Live**
badge while prices stream, **Settings** for grouping and filters, **Refresh**,
**Export**, a close button on each row and **Close All**. See
[chapter 14](../14-positions-holdings/README.md).

### Holdings

Delivery holdings with value, investment and P&L, and **Add** and **Exit**
buttons that open an order ticket.

## API and integration pages

### API Key

Your API key (shown masked; the eye button reveals it), **Copy** and
**Regenerate**, the **Order Execution Mode** (Auto or Semi-Auto), a link to
the Playground, and the **AI clients (MCP)** card. See
[chapter 09](../09-api-key-management/README.md).

### Playground

A request tester for the API, full width, with your API key filled in:
choose an endpoint from the sidebar, edit the JSON body, choose **Send**, and
read the response. It also has WebSocket examples for the market data feed.
See [chapter 10](../10-placing-first-order/README.md).

### Search

Type part of a name to find its OpenAlgo symbol, the broker's symbol,
exchange, token, lot size and freeze quantity.

### Platforms

Setup pages for TradingView, GoCharting and Chartink. Each shows the webhook
address and builds the alert message for you. See
[chapter 16](../16-tradingview-integration/README.md).

## Monitoring pages

| Page | What it shows |
| --- | --- |
| Live Logs | Live orders and API requests with their answers |
| Sandbox Logs (Sandbox Request Monitor) | Every request made while analyzer mode was on |
| Latency Monitor | Order and API round-trip times |
| Traffic Monitor | Requests, status codes and durations |
| Security Logs | Banned addresses and the thresholds that ban them |
| Health Monitor | Feed and system health, orders whose outcome is not known yet, alerts |
| PnL Tracker | Intraday MTM curve |

## Pages outside the menus

| Page | Where you meet it |
| --- | --- |
| Select Broker (`/broker`) | After sign-in when no broker is logged in |
| Broker Configuration (`/broker/config`) | From Select Broker, or Profile, **Broker** tab |
| Port page | At start, when port 5000 is held by another program |
| Update notice | Bottom right, when a new version is out |

## Common elements

| Element | Meaning |
| --- | --- |
| Toasts (small messages at a corner) | Order placed, filled, rejected; errors. Position, count and duration are set in **Profile**, **Alerts** |
| Green / red figures | Profit / loss |
| **Live** / **Paused** badge on Positions and Holdings | Prices are streaming / streaming is paused while the page is hidden |
| Footer | App version and the number of signed-in sessions |

---

**Previous**: [07 - Dashboard overview](../07-dashboard-overview/README.md)

**Next**: [09 - API key management](../09-api-key-management/README.md)
