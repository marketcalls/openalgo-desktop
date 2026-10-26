# WhatsApp Services

OpenAlgo Desktop can send alerts to WhatsApp and answer a few commands there.
It links to your WhatsApp account as a linked device (the same way WhatsApp
Web does), through WhatsApp's unofficial multi-device protocol.

In the desktop this is built in: it uses the Rust library `whatsapp-rust`
directly, the same library the web's `wars` package wraps. There is no Python
and nothing to install. One WhatsApp device is paired per install.

## Security model: one REST endpoint

The REST API at `/api/v1/whatsapp/` has **exactly one** endpoint:
`POST /notify`, which sends a message. Everything else is done on the
**WhatsApp** page of the app while you are signed in, and cannot be reached
with an API key.

Why: the paired session is in effect a key to your WhatsApp account. A leaked
OpenAlgo API key must never be enough to pair a new device, remove the paired
one, read your linked contacts, change the settings, or message everyone.

The paired session is stored encrypted (AES-256-GCM) with the app's data key,
which is protected by your OpenAlgo password. After OpenAlgo Desktop starts,
WhatsApp reconnects only once you sign in, and it disconnects when you log
out; it stays paired either way. See
[How the keys are protected](../../port/no-keychain.md).

## REST endpoint

| Endpoint | Method | Description |
|----------|--------|-------------|
| [Notify](./notify.md) | POST | Send a text message to yourself, one number, a linked user, or up to 5 numbers. |

That is the entire API surface.

## On the WhatsApp page only

These need you to be signed in to the app. They are not available with an API
key:

- **Pair** a device, by QR code or by pair code (enter your phone number, then
  type the 8-character code on the phone under Linked devices).
- **Unlink** the paired device (removes the stored session).
- **Start / Stop** the bot's connection.
- **Settings**: broadcast switch, rate limit, message length.
- **Users**: list and remove linked recipients.
- **Broadcast**, **Send** to a number, **Test message**.
- **Stats**: command usage.

## Quick example: alert to yourself

```bash
curl -X POST http://127.0.0.1:5000/api/v1/whatsapp/notify \
  -H 'Content-Type: application/json' \
  -d '{
    "apikey": "<APIKEY>",
    "self": true,
    "message": "NIFTY crossed 25,000"
  }'
```

`"self": true` sends to your own number. The message appears in WhatsApp's
"Message yourself" chat.

## Quick example: text to two numbers

```bash
curl -X POST http://127.0.0.1:5000/api/v1/whatsapp/notify \
  -H 'Content-Type: application/json' \
  -d '{
    "apikey": "<APIKEY>",
    "phones": ["919876543210", "919812345678"],
    "message": "Order filled: BUY RELIANCE x 10"
  }'
```

Up to 5 numbers per call; any beyond the fifth are dropped. Bulk messaging can
get the paired device logged out by WhatsApp. For real mass messaging, use the
official WhatsApp Business API.

In the desktop, `notify` sends **text only**. Images and documents
(`image_path`, `document_path`) are refused: the desktop has no attachment
folder, so no file on your computer can be sent through the API. See
[Notify](./notify.md).

## Commands on WhatsApp

Once paired, you can send commands to the bot from your own phone, in the
"Message yourself" chat:

```
/help                       # the command list (also /start and /menu)
/status                     # connection and pairing status
/orderbook                  # today's orders
/tradebook                  # today's trades
/positions                  # open positions
/holdings                   # holdings
/funds                      # available funds
/pnl                        # net P&L
/quote RELIANCE NSE         # last traded price (exchange defaults to NSE)
/closeall                   # square off all positions
/mode                       # shows live or analyze mode
```

- Only messages you send yourself are answered. Commands in a group, or from
  another contact, are not run; another contact gets one line saying the bot
  answers only the account it is paired with (at most once an hour).
- Commands use your OpenAlgo API key, so generate one on the API Key page
  first. They run inside the desktop, with the same results as the REST API.
- There is no `/link` command on WhatsApp. The web docs describe linking other
  WhatsApp users with `/link <api_key>`; neither the desktop nor the current
  web bot does this. The bot answers only its owner.
- The bot sends you at most 20 replies a minute.

## Order alerts

When an order event happens (placed, modified, cancelled, all cancelled,
position closed, basket, split, options and multi-leg orders, and the GTT
events: placed, modified, cancelled, triggered, expired), the desktop sends an
alert to your own WhatsApp number, if the device was paired from this OpenAlgo
account. Failed orders are not alerted. Alerts in analyzer mode are marked as
such. No `/notify` call is needed.

Telegram listens to the same events, so both channels alert from one order.
An alert that cannot be sent because WhatsApp is not connected is dropped, not
kept for later.

## Rate limits

| What | Limit |
|------|-------|
| `/api/v1/whatsapp/notify` | 30 per minute per caller, as on the web |
| Bot replies to you | 20 per minute |
| Bot replies to anyone else | 1 per hour |

---

**Back to**: [API Documentation](../README.md)
