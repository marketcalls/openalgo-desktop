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
**Broker Configuration** shows for that broker under **Redirect URL** (or set
that address for the broker on the same page).

## Settings that used to be in .env

There is no `.env` file. Each setting has a place in the app:

| Web `.env` setting | In OpenAlgo Desktop |
| --- | --- |
| `BROKER_API_KEY`, `BROKER_API_SECRET` | Broker Configuration, one set per broker, labelled the way each broker names them |
| `BROKER_API_KEY_MARKET`, `BROKER_API_SECRET_MARKET` | Broker Configuration, Market Data App Key and Secret Key (XTS brokers) |
| `REDIRECT_URL` | Automatic per broker; shown, and changeable, on Broker Configuration |
| `FLASK_HOST_IP`, `FLASK_PORT` | Server Settings, App address and App port |
| `WEBSOCKET_HOST`, `WEBSOCKET_PORT` | Server Settings, Market data address and Market data port |
| `HOST_SERVER`, `NGROK_ALLOW`, `WEBSOCKET_URL` | Profile, Broker tab, Server Configuration (`NGROK_ALLOW` is **Enable Ngrok Tunnel**) |
| The ngrok authtoken (ngrok's own configuration file on the web) | Profile, Broker tab, Server Configuration, **ngrok authtoken** (kept encrypted, never shown again) |
| `APP_KEY`, `API_KEY_PEPPER` | Not needed. Created when you make your account and kept in `openalgo.db`, locked with your OpenAlgo password |

## What does not carry over

Nothing is imported from an OpenAlgo web database. Set these up again in the
desktop:

- Strategies (each gets a new webhook URL; update your TradingView alerts)
- Chartink strategies and their webhook URLs
- Flow workflows: export each from the web's editor and import it here (see
  [Flow workflows](#flow-workflows)); each gets a new webhook URL and secret
- Telegram bot token, WhatsApp pairing, SMTP settings
- Sandbox capital and settings (the sandbox starts fresh at 1 crore)
- Historify data (download it again from the Historify page)

## Webhooks from the internet

TradingView, Chartink and GoCharting alerts are sent from their own servers.
A web instance on a VPS had a public address; your desktop does not.
OpenAlgo Desktop has ngrok built in, so there is nothing to install:

1. Sign up at ngrok.com (the free plan works) and copy your authtoken from
   **Your Authtoken** and your free domain from **Domains** in the ngrok
   dashboard.
2. In OpenAlgo, open Profile, the Broker tab, **Server Configuration**. Paste
   the authtoken and the domain and choose **Save authtoken and domain**.
3. Tick **Enable Ngrok Tunnel** and choose **Save Ngrok Settings**. The
   status changes to **Online** at `https://<your domain>`.

Your Host Server URL is then `https://<your domain>`, as `HOST_SERVER` was
on the web, whether the tunnel is online or not: the webhook addresses on
the TradingView, GoCharting, Chartink, strategy and Flow pages use it, and so
does the automatic broker redirect URL
(`https://<your domain>/<broker>/callback`, which you may already have
registered with your broker for the web). The tunnel opens
each time you sign in to OpenAlgo and closes when you log out or quit; if
the connection drops, OpenAlgo reconnects by itself.

Through the built-in tunnel only alerts, API calls (with your API key) and
AI clients (with Remote MCP on) reach OpenAlgo. The OpenAlgo pages and the
login page stay on this computer: opening the tunnel's address in a browser
shows a short notice instead. If you run your own tunnel (Cloudflare Tunnel,
for example) instead, enter its address as **Host Server URL**; such a
tunnel makes the login page reachable from the internet, so use a strong
password and turn on two-factor login on the **TOTP** tab in Profile.

Your own scripts, Amibroker and Excel on the same computer need no tunnel.

## Flow workflows

Flow is in the desktop from 1.0.5, natively (no Python), and reads the same
workflow JSON as the web. To move a workflow:

1. In OpenAlgo web, open it in the Flow editor, then **...**, **Export
   Workflow**.
2. In OpenAlgo Desktop, open the profile menu, **Flow Editor**, **Import**,
   and pick the file. It arrives inactive as `<name> (imported)`.
3. Open its **Webhook** settings, switch the webhook on, and put the new URL
   and secret into TradingView or Chartink. The web's address does not carry
   over.
4. Run it once in analyzer mode, then activate it.

What behaves differently, in short: every time is IST whatever the
computer's time zone; schedules run only while the app is open and you are
signed in with your broker logged in (a missed run is skipped, not
replayed); a **Daily** schedule honours its ticked days; every webhook needs
its secret; the workflow uses the app's current API key, never a stored one;
Flow's `.env` settings (history cache, run history, webhook limits) are
fixed at the web's defaults. The full list:
[21 - Flow Visual Builder](../userguide/21-flow-visual-builder/README.md#import-and-export-moving-workflows-to-and-from-openalgo-web)
and the [import format](../prompt/flow-import-format.md).

## The market data feed

The feed on port 8765 speaks the web's protocol. Where it behaves
differently, it is on purpose:

- **Subscribe answers.** "success" for an instrument means it will be
  streamed: now, or as soon as the broker's feed connects again. When the
  broker refused the live feed, or has none, each instrument is answered
  with an error that says so, as the web answers when its broker adapter
  is not running.
- **Sign-outs close the feed.** A client that authenticated before you
  signed out, switched brokers or made a new API key is closed (code
  4401) and connects and authenticates again. The web keeps it
  connected.
- **Depth answers.** A Depth subscribe is answered as the web answers it
  for your broker (maintainer decision 2026-10-16: echo like the web).
  Where the web's adapter repeats the depth asked for (Zerodha, Fyers,
  Upstox, Kotak and others), so does the desktop, although Zerodha
  streams 5 levels when 20 or 50 are asked for; where it reports the level
  it falls back to (Angel, Arrow, Dhan, the XTS brokers and others), so
  does the desktop; Arrow and HDFC answer an LTP or Quote subscribe with
  `"depth": null` and IIFL Capital always with 5, as on the web. The answer
  can therefore name more levels than are streamed: read the depth from
  the frames, not from the answer.
- **Field names.** Quote and Depth data carry the names OpenAlgo web
  sends with Zerodha (`last_quantity`, `average_price`, `price_change`,
  `price_change_percent`) for every broker, and next to them, with the
  same values, the names your broker's web adapter uses:
  `last_trade_quantity` (Angel, Fyers, Upstox, 5paisa, mstock, Paytm,
  Pocketful, Samco), `ltq` (Definedge, Motilal), `avg_trade_price`
  (Fyers), `change` (Fyers, Arrow, HDFC, Tradejini, Samco),
  `change_absolute` (Paytm), `change_percent` (Fyers, AliceBlue, Arrow,
  HDFC, Definedge, Paytm, Tradejini), `percent_change` (Shoonya,
  Flattrade, Zebu, TradeSmart), `change_percentage` (Samco). A script
  written for either name works. The change fields come with indices
  only, as recorded from the web.
- **A message budget.** A connection that sends thousands of messages a
  second, or megabytes of them, is closed (code 1008) before they are
  read. Subscribing every instrument in one message, or one per message
  as a script loop does, stays far inside it. The web reads without a
  limit.

## What OpenAlgo Desktop does not have

- The Python Strategy Host. Python strategies still work as ordinary scripts
  using the OpenAlgo Python SDK against the desktop's API. The Telegram bot
  has no `/stoppython`, and its `/closeall` closes positions only.
- Python strategies written by the Agent: the desktop's Agent writes
  OpenScript instead, and does not offer models on Amazon Bedrock,
  SageMaker or Google Vertex AI.
- The pandas backtesters (Portfolio Backtester, SIP Backtester, Portfolio
  Analyzer). Flow is in the desktop from 1.0.5: see
  [Flow workflows](#flow-workflows).
- The Socket.IO `/market` namespace of the web's WebSocket example page.
  It needs a signed-in browser session and no SDK, trading platform or
  page of the app uses it; the market data feed on port 8765 serves the
  same data.
- More than one user, or more than one broker connected at the same time. You
  can save credentials for several brokers and switch between them.
