# 19 - GoCharting Integration

## Introduction

GoCharting is a web charting platform for Indian markets. Its alerts can call
a webhook with a message you write. Point that webhook at OpenAlgo Desktop's
order API and put an OpenAlgo order in the message, and each alert places an
order.

```
GoCharting alert fires  --->  POST <public address>/api/v1/placeorder  --->  OpenAlgo Desktop  --->  your broker
                               (the message is the order, with your API key)   (or the sandbox)
```

OpenAlgo Desktop has a **GoCharting** page that builds the webhook address and
the message for you.

## What you need

- A GoCharting plan with webhook alerts (the app notes that this needs the
  Premium plan).
- OpenAlgo Desktop open and signed in, with your broker logged in.
- An API key (created with your account; see the **API Key** page).
- A public address for OpenAlgo Desktop, because GoCharting sends alerts from
  its own servers on the internet.

## Making the desktop reachable

Turn on the built-in ngrok tunnel in **Profile**, **Broker** tab, **Server
Configuration** (your ngrok authtoken, then **Enable Ngrok Tunnel**; see
[chapter 16](../16-tradingview-integration/README.md#step-1-turn-on-the-built-in-tunnel)).
Or run a tunnel yourself on the same computer (for example
`cloudflared tunnel --url http://localhost:5000`, using your app port if you
changed it) and enter its `https://` address as **Host Server URL** there.

Until you do, the GoCharting page shows **Webhook URL not accessible!** and
its webhook address points at `127.0.0.1`, which GoCharting cannot reach.
Requests that arrive through a tunnel whose address is not entered there are
refused. More: [Troubleshooting: another device, a firewall, or a tunnel](../../user/troubleshooting.md#another-device-a-firewall-or-a-tunnel).

## Step 1: Build the alert message in OpenAlgo

1. Open **Platforms**, then **GoCharting**.
2. Copy the **Webhook URL**. It is your Host Server URL followed by
   `/api/v1/placeorder`.
3. Fill in the **Configuration** card:
   - **Symbol**: type at least two letters and pick from the list.
   - **Exchange**: the exchanges your broker supports.
   - **Product Type**: MIS, NRML or CNC.
   - **Action**: BUY or SELL.
   - **Quantity**.
4. The **Generated JSON** card updates as you type (or choose **Generate
   JSON**). Choose **Copy**.

The message looks like this, with your own API key in place of `<APIKEY>`:

```json
{
  "apikey": "<APIKEY>",
  "strategy": "GoCharting Alert",
  "symbol": "NHPC",
  "exchange": "NSE",
  "action": "BUY",
  "product": "MIS",
  "pricetype": "MARKET",
  "quantity": "1"
}
```

The message carries your API key in full. Anyone who sees it can trade on
your account: keep the alert private and regenerate the key on the API Key
page if it leaks (then update every alert and tool that uses it).

## Step 2: Create the alert in GoCharting

1. Open the chart in GoCharting and create an alert with your condition.
2. Paste the JSON as the alert's **message**.
3. Turn on **Webhook** delivery and paste the Webhook URL.
4. Save the alert.

Create one alert for BUY and another for SELL; the action is fixed in the
message.

## Other order types

The page builds MARKET orders. For other orders, edit the JSON before you
paste it. The request is the same as any `/api/v1/placeorder` call:

```json
{"apikey": "<APIKEY>", "strategy": "GoCharting Alert", "symbol": "SBIN", "exchange": "NSE",
 "action": "BUY", "product": "MIS", "pricetype": "LIMIT", "price": "625.5", "quantity": "1"}
```

For SL orders add `trigger_price`; for SL-M use `"pricetype": "SL-M"` with
`trigger_price`. To size by target position instead, use
`/api/v1/placesmartorder` as the webhook address and add `position_size` (see
[12 - Smart Orders](../12-smart-orders/README.md)). Symbols follow the
[Symbol Format Guide](../symbol-format/README.md).

## Test in analyzer mode first

Turn on analyzer mode with the mode switch in the top bar (the badge reads
**Analyze Mode**). Alerts then place sandbox orders priced from your broker's
live data, and **Logs**, **Sandbox Logs** shows every request. Switch to
**Live Mode** only when the orders look right. If **Semi-Auto** is selected on
the API Key page, each alert's order waits in the
[Action Center](../22-action-center/README.md) for your approval.

## Notes for the desktop

- **The API is off until you sign in.** After OpenAlgo starts, and after you
  log out, alerts are answered "Sign in to OpenAlgo to use the API" and
  nothing is placed.
- **The broker session ends around 03:00 IST.** Until you log in to your
  broker again, orders are refused with `Invalid openalgo apikey`, as on the
  web.
- **The page's Download Guide was written for OpenAlgo web.** Where it says to
  set `HOST_SERVER` or `ORDER_RATE_LIMIT` in a `.env` file, use the Host Server
  URL in Profile instead; the desktop has no `.env` and its order limit (10
  orders a second) is fixed.

## Troubleshooting

| What you see | Cause and fix |
| --- | --- |
| Alerts never arrive | The tunnel is down or its address changed. Restart it, update the Host Server URL and the webhook in GoCharting. |
| `Sign in to OpenAlgo to use the API...` | Sign in to OpenAlgo. |
| `Invalid openalgo apikey` | The key in the message is wrong, from OpenAlgo web, or was regenerated; or no broker is logged in. |
| A validation error naming a field | The message is not valid JSON or a field is missing. Copy it again from the GoCharting page. |
| Symbol not found | Pick the symbol from the page's search, on the right exchange. |

---

**Previous**: [18 - ChartInk Integration](../18-chartink-integration/README.md)

**Next**: [20 - Python Strategies](../20-python-strategies/README.md)
