# 16 - TradingView Integration

## Introduction

TradingView alerts can place orders through OpenAlgo Desktop. When an alert
fires, TradingView sends a webhook (an HTTP request with your message) to an
address you give it; OpenAlgo Desktop receives it and places the order with
your broker, or in the sandbox in analyzer mode.

The alert messages are the same as for OpenAlgo web. What differs is how
TradingView reaches your computer, so read
[Making OpenAlgo Desktop reachable](#making-openalgo-desktop-reachable) first.

## How it works

```
TradingView alert fires
   -> TradingView's servers send the webhook to https://<your tunnel address>/api/v1/...
   -> the tunnel forwards it to OpenAlgo Desktop on http://127.0.0.1:5000
   -> OpenAlgo Desktop checks the API key and places the order
   -> your broker (Live mode) or the sandbox (Analyze mode)
```

## Before you start

1. A TradingView plan that includes webhook alerts.
2. OpenAlgo Desktop open, you signed in, your broker logged in, and the
   master contract **Ready**.
3. Your API key (the TradingView page fills it in for you).
4. A tunnel, as below.

## Making OpenAlgo Desktop reachable

TradingView sends webhooks from its own servers on the internet. By default
OpenAlgo Desktop listens only on `127.0.0.1`, which nothing outside your
computer can reach. TradingView also accepts only standard web addresses (on
ports 80 and 443) as webhook URLs, so it cannot call `http://<your IP>:5000`
even if your router forwarded it.

### What is supported, and what is not

| Way | Supported? |
| --- | --- |
| The built-in ngrok tunnel (**Profile**, **Broker** tab, **Server Configuration**) | **Yes.** The simplest way: nothing to install, it opens when you sign in |
| A tunnel you run yourself (Cloudflare Tunnel, an ngrok agent or similar), forwarding to `http://127.0.0.1:5000`, with its address entered as **Host Server URL** | **Yes** |
| **Allow access from other devices** in Server Settings | **Not for webhooks.** It lets devices on your own network (a laptop, a tablet) reach the app over plain HTTP. It does not make the app reachable from the internet |
| Forwarding port 5000 on your router to your computer | **No.** It would expose the app over plain HTTP, and TradingView does not call port 5000 |
| A plain port forwarder that adds no forwarding header (`socat`, `ssh -R`) | **No.** Its callers look like programs on your computer. Use a tunnel that adds one, such as ngrok or Cloudflare Tunnel |
| HTTPS certificates or a domain hosted by the desktop | **No.** The tunnel provides the `https` address |

And, whatever the tunnel:

- **The app must be open and you signed in.** After OpenAlgo Desktop starts,
  and after you log out, alerts are refused until you sign in again; nothing
  is placed.
- **The computer must be awake** and online, and the tunnel running.
- **The broker must be logged in.** At about 03:00 IST the broker session
  ends; log in again each trading day.

For alerts around the clock without a computer left on, run
[OpenAlgo web](https://github.com/marketcalls/openalgo) on a server instead.

### Step 1: Turn on the built-in tunnel

1. Sign up at ngrok.com (the free plan works). Copy your authtoken from
   **Your Authtoken** and your free domain from **Domains** in the ngrok
   dashboard. The domain is required: the tunnel always uses it, so your
   addresses never change.
2. In OpenAlgo, open the profile menu, **Profile**, then the **Broker** tab.
3. In the **Server Configuration** card, paste the authtoken and the domain
   and choose **Save authtoken and domain**. The authtoken is kept
   encrypted and never shown again.
4. Tick **Enable Ngrok Tunnel** and choose **Save Ngrok Settings**.

The card shows the tunnel's state: **Connecting**, then **Online** with its
`https://` address. With the switch on, your **Host Server URL** is
`https://<your domain>`, online or not: the webhook addresses on the
TradingView, GoCharting and Chartink pages and on each strategy use it, and
so does the automatic broker redirect URL. Register
`https://<your domain>/<broker>/callback` with your broker, as OpenAlgo web
users do, or keep a fixed local redirect URL on Broker Configuration. The tunnel opens each time you sign in, closes when you log
out or quit, and reconnects by itself if the connection drops. If it shows
**Error**, the message says why and what to do (a mistyped authtoken, a
domain that is not on your ngrok account, another ngrok agent using your
domain, no internet). It never falls back to another address.

Through the built-in tunnel only alerts and API calls (with your API key),
strategy, Chartink and Flow webhooks, and AI clients (with **Remote MCP** on)
reach OpenAlgo. The OpenAlgo pages and the sign-in page stay on your
computer: opening the tunnel's address in a browser shows a short notice.

### Or: a tunnel you run yourself

Point your tunnel tool at the desktop's app port (5000, or your **App port**
from Server Settings), for example
`cloudflared tunnel --url http://127.0.0.1:5000`, and copy the `https://`
address it prints. Then, in the **Server Configuration** card, enter it in
**Host Server URL** (the bare address: `https://`, the host name, nothing
after it) and choose **Save Ngrok Settings**. It applies at once; no
restart. Leave **Enable Ngrok Tunnel** off: it starts the built-in tunnel.

Why this matters:

- **Requests through a tunnel whose address is not entered here are
  refused** ("Request blocked."). This protects the app from pages that try
  to reach it under another name.
- The webhook address shown on the TradingView, GoCharting and Chartink pages
  uses it.

Other tunnel services (VS Code Dev Tunnels and others) work the same way if
they give an `https` address anyone can call without signing in, and pass on
the caller's address in a forwarding header. When you stop using the tunnel,
choose **Use automatic address** under the field (or clear it) and save.

### Security with a tunnel

A tunnel you run yourself makes the OpenAlgo sign-in page reachable from
the internet (the built-in one does not).

- Use a strong password and turn on two-factor sign-in (**Profile**, **TOTP**
  tab; [chapter 05](../05-first-time-setup/README.md#step-6-turn-on-two-factor-sign-in-optional)).
- Your API key travels inside each alert message. Keep your alerts private,
  and regenerate the key if it leaks.
- Wrong API keys arriving through the tunnel are counted per key tried, so a
  stranger's attempts never block your correct key.
- AI clients through the tunnel also need **Admin**, **Remote MCP** turned on.

More in [Troubleshooting: another device, a firewall, or a tunnel](../../user/troubleshooting.md#another-device-a-firewall-or-a-tunnel).

## Building the alert on the TradingView page

Open **Platforms** in the top bar, then **TradingView** (**TradingView
Configuration**).

- **Webhook URL**: the address to paste into TradingView, built from your
  Host Server URL, with **Copy**. While the address is still a local one
  (`127.0.0.1` or `localhost`), a red **Webhook URL not accessible!** warning
  tells you to set up a tunnel first.
- **Configuration**: two tabs.
  - **Strategy Alert**, for Pine Script strategies: the webhook goes to
    `/api/v1/placesmartorder`, and TradingView fills in the action, quantity
    and position.
  - **Line Alert**, for price or indicator alerts: the webhook goes to
    `/api/v1/placeorder` with the **Action** (BUY or SELL) and **Quantity**
    you choose.
- **Symbol** (type two letters to search), **Exchange** and **Product Type**.
- **Generated JSON**: the alert message, with your API key filled in.
  **Copy** it.

## Setting up the alert in TradingView

1. Open the chart and add your strategy or indicator.
2. Create an alert (the alarm-clock button, or Alt+A).
3. Set the **Condition** to your strategy or indicator.
4. Under **Notifications**, tick **Webhook URL** and paste the Webhook URL
   from OpenAlgo.
5. In **Message**, paste the Generated JSON.
6. Create the alert.

### Strategy Alert message (smart order)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "TradingView Strategy",
  "symbol": "SBIN",
  "exchange": "NSE",
  "action": "{{strategy.order.action}}",
  "product": "MIS",
  "pricetype": "MARKET",
  "quantity": "{{strategy.order.contracts}}",
  "position_size": "{{strategy.position_size}}"
}
```

Sent to `https://<tunnel>/api/v1/placesmartorder`. `position_size` is
TradingView's position after the order, so OpenAlgo moves your position to
match it, however many alerts were missed or repeated
([chapter 12](../12-smart-orders/README.md)).

### Line Alert message (regular order)

```json
{
  "apikey": "<APIKEY>",
  "strategy": "TradingView Line Alert",
  "symbol": "SBIN",
  "exchange": "NSE",
  "action": "BUY",
  "product": "MIS",
  "pricetype": "MARKET",
  "quantity": "1"
}
```

Sent to `https://<tunnel>/api/v1/placeorder`.

### Other messages

The page builds MARKET orders. Any `/api/v1` order request works as a message;
edit the JSON by hand for others:

**Limit order at the strategy's price** (to `/api/v1/placeorder`):

```json
{
  "apikey": "<APIKEY>",
  "strategy": "TVLimit",
  "symbol": "SBIN",
  "exchange": "NSE",
  "action": "{{strategy.order.action}}",
  "quantity": "10",
  "pricetype": "LIMIT",
  "price": "{{strategy.order.price}}",
  "product": "MIS"
}
```

**F&O** (to `/api/v1/placeorder`): use the OpenAlgo contract symbol and
exchange, with a quantity in whole lots:

```json
{
  "apikey": "<APIKEY>",
  "strategy": "TVOptions",
  "symbol": "NIFTY28OCT2625000CE",
  "exchange": "NFO",
  "action": "{{strategy.order.action}}",
  "quantity": "65",
  "pricetype": "MARKET",
  "product": "NRML"
}
```

The contract and lot size here show the format; use a current contract and its
lot size (**Search** shows both).

## TradingView variables

| Variable | Meaning | Example |
| --- | --- | --- |
| `{{strategy.order.action}}` | `buy` or `sell` | `buy` |
| `{{strategy.order.contracts}}` | Order quantity | `10` |
| `{{strategy.position_size}}` | Position after the order (negative when short) | `-10` |
| `{{strategy.order.price}}` | Order price | `625.5` |
| `{{ticker}}` | The chart's symbol | `SBIN` |
| `{{close}}` | The bar's close | `625.5` |

OpenAlgo accepts `action` in lower case, and numbers sent as text.

`{{ticker}}` is TradingView's name for the symbol. For equities it is usually
the OpenAlgo symbol too; for futures and options it is not, so write the
OpenAlgo symbol in the message.

## Strategy webhooks (the Strategies page)

The **Strategies** page has its own webhook for each strategy, at
`/strategy/webhook/<token>`, for TradingView alerts that drive a strategy
with its risk rules. The page shows that address starting with
`http://127.0.0.1:5000`; for TradingView, replace that start with your tunnel
address (for example `https://abc123.ngrok-free.app/strategy/webhook/...`).
The token in the address is the credential: keep it private, and rotate it on
the strategy's page if it leaks.

## Testing

1. Switch to **analyzer mode** (top bar). Check the badge reads **Analyze
   Mode**.
2. In TradingView, create a test alert that fires soon (for example a price
   crossing near the current price), with the webhook and message above.
3. When it fires, check in OpenAlgo Desktop:
   - **Logs**, then **Sandbox Logs**: the request and its answer;
   - **Orderbook** and **Positions**: the sandbox order and position.
4. In TradingView, the alert's log shows whether the webhook was delivered.
5. When it works, switch to **Live Mode** and start with the smallest
   quantity.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| TradingView says the webhook failed | Is the tunnel running? Is OpenAlgo Desktop open? Does the Webhook URL start with the current tunnel address? |
| "Request blocked." | The tunnel's address is not the **Host Server URL**. Enter it and save |
| "Sign in to OpenAlgo to use the API. ..." | The app was restarted or you logged out. Sign in |
| "Invalid openalgo apikey" | The key in the message is old (regenerated, or a password reset). Copy the JSON again from the TradingView page |
| "Symbol X not found on NSE" | Wrong symbol or exchange in the message, or the master contract is not loaded |
| "Quantity must be in multiples of lot size ..." | F&O quantity is not a whole number of lots |
| Order waits instead of being placed | Order Execution Mode is Semi-Auto: approve it in **Action Center** |
| Nothing arrives after the tunnel restarted | Free tunnel addresses change: update **Host Server URL** and every alert's webhook URL |
| Works in analyzer mode, not live | Broker logged in? Funds and margin? Your broker's static IP rules? Check **Live Logs** |

## Good practice

1. **Test in analyzer mode first**, with the real alert.
2. **Use smart orders** for strategies, so a repeated or missed alert cannot
   leave you with the wrong position.
3. **One alert per symbol**, or a message that names the symbol explicitly.
4. **Set an expiry** on alerts you no longer need.
5. **Watch the first live orders** in OpenAlgo and in your broker's terminal.
6. **Keep the computer awake** during market hours.

TradingView limits how many alerts you may have and how often they fire,
depending on your plan. OpenAlgo accepts up to 10 orders and 10 smart orders
per second, each kind counted on its own.

---

**Previous**: [15 - Analyzer mode](../15-analyzer-mode/README.md)

**Next**: [17 - Amibroker integration](../17-amibroker-integration/README.md)
