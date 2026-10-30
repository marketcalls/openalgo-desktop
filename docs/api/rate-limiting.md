# Rate Limiting

OpenAlgo Desktop limits how fast a caller may use the API, the same way OpenAlgo web does: a moving window per caller, kept in memory, answered with HTTP 429 when the window is full. It also paces the orders it sends to your broker, so a burst from your scripts never breaks your broker's own limits.

The limits are fixed in the app. There is no `.env` file and no setting to change them.

## API Rate Limits

A caller is one network address: every program on this computer counts as one caller (127.0.0.1), and each device on your network is its own caller. Callers that come through a tunnel (ngrok, Cloudflare Tunnel) share one identity, so behind a tunnel a valid API key gets a window of its own.

Every endpoint has its own window per caller, as on the web: ten `/placeorder` and ten `/cancelorder` calls in the same second all go through, and so do 100 `/quotes` and 100 `/depth` calls.

| Limit | Endpoints (each in its own window) |
|-------|------------------------------------|
| 10 per second | `/placeorder`, `/modifyorder`, `/cancelorder`, `/optionsorder`, `/optionsmultiorder`, `/placegttorder`, `/modifygttorder`, `/cancelgttorder`, `/placesmartorder` |
| 30 per minute | `/optiongreeks`, `/whatsapp/notify`, and every `/telegram/*` endpoint except the two below |
| 5 per minute | `/telegram/broadcast` |
| 100 per second | Every other `/api/v1` endpoint: market data, account books, symbols, the other options analytics, the `/strategy/*` endpoints, `/cancelallorder`, `/closeposition`, `/basketorder`, `/splitorder`, `/margin`, `/telegram/webhook` and the rest |

All paths are under `http://127.0.0.1:5000/api/v1`. `/ticker/<symbol>` is one endpoint, whatever the symbol.

A basket, split or options multi-order counts as one request here, however many orders it places.

## What Happens When a Limit Is Exceeded

The request is refused with HTTP 429 and the limit in the body, as Flask-Limiter answers on the web:

```json
{
  "message": "10 per 1 second"
}
```

The body has no `status` field, and no `Retry-After` or `X-RateLimit-*` header is sent. A refused request is not counted, so a client that waits a moment and tries again gets through once the window has room. The other limits answer `"100 per 1 second"`, `"30 per 1 minute"` and `"5 per 1 minute"`.

## Order Pacing at the Broker

The API limits above decide whether OpenAlgo accepts a request. Separately, OpenAlgo sends orders to your broker no faster than the broker allows, one after another in the order they arrived.

- Brokers that publish an order rate are paced at it (for example Zerodha at 10 orders a second, Angel at 20).
- Brokers that publish none (Samco, Tradejini, HDFC Sky, HDFC Securities, Pocketful, 5paisa) are paced at 10 orders a second, the exchanges' threshold above which a retail API client's algorithm must be registered.
- Orders have their own queue, apart from quotes and history, so a burst of market data requests never holds an exit back.

What a client sees: a burst of orders is queued, so the later ones take a little longer to answer. A request whose turn would come more than 10 seconds later is not sent at all. It is answered at once with HTTP 429, as on the web (market data and account reads too):

```json
{
  "status": "error",
  "message": "OpenAlgo is pacing requests to stay within your broker's rate limit, and this one would have had to wait too long for its turn. Try again in a few seconds."
}
```

Nothing reached the broker, so it is safe to send that order again.

Some brokers also count requests per minute or per day. Fyers counts orders, market data and account calls separately, by plan (Standard unless you pick Prime on Broker Configuration), and OpenAlgo keeps the day's count across restarts. A request refused for one of those limits is not sent, and the message names the limit, for example `Fyers' daily limit of 5,000 market data requests on the Standard plan is used up for today, so OpenAlgo has stopped sending them. It resets at midnight.`

## Invalid API Keys

Invalid API keys are counted per caller: 10 a minute. A caller that keeps sending wrong keys still gets the same 403 answer, and a device on your network that keeps doing it can be blocked for a while. Programs on this computer are never blocked this way, and a valid key is never refused because of someone else's mistakes. See [Your API client says the API key is invalid](../user/troubleshooting.md#your-api-client-says-the-api-key-is-invalid).

## Webhook Limits

| Endpoint | Limit |
|----------|-------|
| `/strategy/webhook/<token>` | 100 per minute per caller |
| `/chartink/webhook/<webhook_id>` | 100 per minute per caller |
| `/flow/webhook/<token>` and `/flow/webhook/<token>/<symbol>` (one window for both) | 100 per minute per caller, and 100 per minute per workflow |
| Wrong webhook addresses | 10 per minute per caller; further wrong ones are refused at once |

A Flow webhook over its limit is answered as on the web: HTTP 429 with a
`Retry-After` header and

```json
{
  "status": "error",
  "message": "Rate limit exceeded. Please slow down your requests.",
  "limit": "100 per 1 minute",
  "retry_after": 60
}
```

In the desktop, wrong secrets for a Flow webhook count against the caller's
budget for wrong webhook addresses, and ten for one workflow within 10
minutes lock that workflow's webhook for 15 minutes (OpenAlgo web has no
lockout). Behind the built-in tunnel every caller shares one identity, so the
per-workflow window is what limits a tunnel caller.

## Sign-in Limits

Signing in to OpenAlgo, and broker sign-ins, are limited to 5 a minute and 25 an hour per address. After five wrong passwords or authenticator codes from the same place, each further try waits 30 seconds, doubling up to 5 minutes. Mistakes made elsewhere never slow down signing in on this computer.

## WebSocket Limits

The market data feed on `ws://127.0.0.1:8765` has its own limits, apart from the HTTP ones:

- A client may hold up to 3000 subscriptions.
- A message larger than 1 MiB closes the connection with code 1009.
- A client that sends thousands of messages a second, or megabytes of them, is closed with code 1008 ("too many messages"). Subscribing all 3000 instruments, in one message or one per message, stays far inside it.
- A client that stops reading what it is sent is closed with code 1008 ("client too slow").

See the [WebSocket streaming](./websocket-streaming/ltp.md) pages.

## A Resource Guard

Every caller is also limited to 1000 requests a second across the whole app. Past it, only requests with an invalid credential (or, for pages, without a signed-in session) are refused, with HTTP 429 and `"Too many requests. Please slow down and try again."`. No real client comes near it.

## Recommendations

- Space requests out in your code, or use a client-side rate limiter.
- On a 429, wait a second and try again; back off further if it repeats.
- Use `/basketorder`, `/splitorder` or `/optionsmultiorder` to send many orders in one request.
- Use `/multiquotes` instead of many `/quotes` calls, and the WebSocket feed for live prices instead of polling.

## In the desktop

- In the desktop, the limits cannot be changed. They are the web's sample settings (`.sample.env`); the web reads them from `.env` (`API_RATE_LIMIT`, `ORDER_RATE_LIMIT`, `SMART_ORDER_RATE_LIMIT`, `WEBHOOK_RATE_LIMIT`, `GREEKS_RATE_LIMIT`, `TELEGRAM_RATE_LIMIT`, `WHATSAPP_RATE_LIMIT`).
- In the desktop, `/telegram/webhook` has the general limit of 100 per second; the web sets none on it.
- In the desktop, orders are paced at the broker for every broker, including those the web does not pace (Samco, Tradejini, HDFC Sky, HDFC Securities, Pocketful, 5paisa), and a request that would wait more than 10 seconds is always refused with HTTP 429. The web bounds the wait only on a multi-threaded server (gthread) and answers 429 there.
- In the desktop, the WebSocket feed limits the messages each client may send; the web reads messages without a limit.
- In the desktop, the Chartink strategy management pages (create, configure, delete) have no request limit of their own; the web limits them to 200 per minute. They need the signed-in session either way.
- In the desktop, the Flow webhook limit is counted per workflow with the token kept as a keyed digest; the web keeps each token, guesses included, as text in its limiter's memory.

---

**Back to**: [API Documentation](./README.md)
