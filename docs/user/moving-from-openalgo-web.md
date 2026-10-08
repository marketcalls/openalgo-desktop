# Moving from OpenAlgo web

OpenAlgo Desktop is built to replace OpenAlgo web for one trader on one
computer. Your tools talk to it exactly as they talk to the web version. This
page lists what carries over as it is and the few things you change.

## What stays the same

- **The addresses.** The dashboard and REST API are on
  `http://127.0.0.1:5000`, the market data WebSocket on
  `ws://127.0.0.1:8765`, the same defaults as OpenAlgo web.
- **The API.** Every `/api/v1` endpoint takes the same request and returns the
  same response: field names, status values, error messages, date formats and
  symbol formats. The API key is still sent in the JSON body.
- **The WebSocket feed.** Same authentication, subscribe and unsubscribe
  messages, modes (LTP, Quote, Depth) and message format.
- **The broker redirect URL.** `http://127.0.0.1:5000/<broker>/callback`, as
  on the web.
- **The screens.** The dashboard, books, strategy pages, options tools,
  Historify and settings look and work as they do on the web.

So the OpenAlgo Python SDK, TradingView, Amibroker, Chartink, GoCharting,
Excel and MCP clients work without code changes.

## What you change

### 1. The API key

The desktop creates its own API key when you set up your account. Copy it from
the **API Key** page and put it wherever you used the web key: your Python
scripts, Amibroker, Excel, TradingView alert messages, Chartink and so on.

### 2. The base URL, only if it is different

If your tools already use `http://127.0.0.1:5000` and `ws://127.0.0.1:8765`,
change nothing.

Change the base URL only when:

- **Your web instance ran on another machine** (a VPS, a home server, a
  domain). Point your tools at `http://127.0.0.1:5000` instead, on the
  computer where OpenAlgo Desktop runs.
- **You changed the desktop's ports** in **Server Settings** (profile menu),
  for example to run OpenAlgo web and OpenAlgo Desktop side by side. Use the
  new port in the base URL and WebSocket URL.

Python SDK example:

```python
from openalgo import api

client = api(
    api_key="your-desktop-api-key",
    host="http://127.0.0.1:5000",
    ws_url="ws://127.0.0.1:8765",
)
```

### 3. Your broker app's redirect URL, only if it is different

If your broker app's redirect URL is already
`http://127.0.0.1:5000/<broker>/callback`, keep it. If it pointed at a domain
or another port, change it in your broker's developer portal to the address
shown on the **Broker** tab in **Profile** (under **Current Configuration**,
**Redirect URL**).

## Settings that used to be in .env

There is no `.env` file. Each setting has a place in the app:

| Web `.env` setting | In OpenAlgo Desktop |
| --- | --- |
| `BROKER_API_KEY`, `BROKER_API_SECRET` | Profile, Broker tab, Update Credentials |
| `BROKER_API_KEY_MARKET`, `BROKER_API_SECRET_MARKET` | Profile, Broker tab, Market Data API (Optional) |
| `REDIRECT_URL` | Worked out from the broker you choose; shown on the Broker tab |
| `FLASK_HOST_IP`, `FLASK_PORT` | Server Settings, App address and App port |
| `WEBSOCKET_HOST`, `WEBSOCKET_PORT` | Server Settings, Market data address and Market data port |
| `HOST_SERVER`, `NGROK_ALLOW`, `WEBSOCKET_URL` | Profile, Broker tab, Server Configuration |
| `APP_KEY`, `API_KEY_PEPPER` | Not needed. Created on first run and kept in your system keychain |

## What does not carry over

Nothing is imported from an OpenAlgo web database. Set these up again in the
desktop:

- Strategies (each gets a new webhook URL; update your TradingView alerts)
- Chartink strategies and their webhook URLs
- Telegram bot token, WhatsApp pairing, SMTP settings
- Sandbox capital and settings (the sandbox starts fresh at 1 crore)
- Historify data (download it again from the Historify page)

## Webhooks from the internet

TradingView, Chartink and GoCharting alerts are sent from their own servers.
A web instance on a VPS had a public address; your desktop does not. To
receive these alerts, run a tunnel such as ngrok or Cloudflare Tunnel that
forwards to `http://127.0.0.1:5000`, then enter the tunnel's address as
**Host Server URL** under **Server Configuration** on the Broker tab in
Profile, so the webhook addresses the app shows use it.

A tunnel makes the OpenAlgo login page reachable from the internet. Use a
strong password and turn on two-factor login on the **TOTP** tab in Profile.

Your own scripts, Amibroker and Excel on the same computer need no tunnel.

## What OpenAlgo Desktop does not have

- The Python Strategy Host, Flow, and the pandas backtesters (Portfolio
  Backtester, SIP Backtester, Portfolio Analyzer). Python strategies still
  work as ordinary scripts using the OpenAlgo Python SDK against the desktop's
  API.
- The Agent, which is coming later.
- More than one user, or more than one broker connected at the same time. You
  can save credentials for several brokers and switch between them.
