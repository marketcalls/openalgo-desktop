# 23 - Telegram Bot (and WhatsApp)

## Introduction

OpenAlgo Desktop has a Telegram bot and a WhatsApp bot built in. Both send you
an alert when an order is placed, modified or cancelled, and both answer
commands such as `/positions` and `/pnl` from your phone.

In the desktop, both bots are native: they run inside OpenAlgo Desktop and
need no Python. The Telegram bot answers the same commands as OpenAlgo web's,
less `/stoppython` (the desktop has no Python Strategy Host).

Both bots run only while OpenAlgo Desktop is open and you are signed in. A bot
you switched on starts again by itself when you sign in, and stops when you log
out or quit.

## Telegram

### Step 1: Create your bot

1. In Telegram, open **@BotFather** and send `/newbot`.
2. Give it a name and a username ending in `bot`.
3. Copy the token BotFather gives you. Treat it like a password.

### Step 2: Give the token to OpenAlgo

1. Open **Telegram Bot** from the profile menu, then **Configuration**.
2. Paste the token in **Bot Token** and choose **Save Configuration**. Once
   saved, the page shows "Token is configured" and the field becomes **Update
   Token (optional)**; leave it blank to keep the saved token.
3. Back on the Telegram Bot page, choose **Start Bot**. The status card shows
   your bot's `@username` and "Bot is running".

### Step 3: Link your Telegram account

1. Open your bot in Telegram and send `/start`.
2. Link it with your OpenAlgo API key (from the **API Key** page):

   ```
   /link <APIKEY> http://127.0.0.1:5000
   ```

   The bot checks the key and answers "Account linked successfully!".

In the desktop, the address after the key is stored for display only:
commands always run on this OpenAlgo Desktop, and the bot says so when you
link. Send the `/link` message in a private chat with the bot, then delete it
from the chat, since it contains your API key. Wrong keys are throttled per
Telegram account.

### What the bot answers

The bot answers only in a **private chat** with it, and only to linked users
(before linking it answers `/start`, `/help` and `/link`). Messages in a group
are ignored, so nobody in a group can use your buttons.

| Command | What it does |
| --- | --- |
| `/start` | Welcome message, or "Welcome back" when linked |
| `/help` | The list of commands |
| `/link <api_key> <host_url>` | Link your OpenAlgo account |
| `/unlink` | Remove the link and your stored data |
| `/status` | Connection status of the linked account |
| `/menu` | Buttons for the commands below |
| `/orderbook` | Open orders |
| `/tradebook` | Executed trades |
| `/positions` | Current positions |
| `/holdings` | Holdings |
| `/funds` | Available, collateral and utilized funds |
| `/pnl` | Realized and unrealized P&L |
| `/quote <symbol> [exchange]` | A quote, for example `/quote RELIANCE` or `/quote NIFTY NSE_INDEX` |
| `/chart <symbol> [exchange] [type] [interval] [days]` | A chart image. `type` is `intraday`, `daily` or `both` (default both); interval `1m`, `5m`, `15m`, `30m`, `1h` or `D`; for example `/chart RELIANCE NSE intraday 15m 10` |
| `/closeall` | Close all open positions, after a **Yes, close all** confirmation |
| `/mode` | Shows Live Mode or Analyze Mode, with a button to switch |

Notes:

- Every answer follows analyzer mode: with it on, the books, funds and
  `/closeall` are the sandbox's.
- `/closeall` closes positions only (it does not cancel open orders). With
  **Semi-Auto** selected on the API Key page it is refused, like any API
  client's close (see [22 - Action Center](../22-action-center/README.md)).
- `/mode` changes the mode for the whole app, exactly like the switch in the
  top bar.

### Order alerts

While the bot is running, the linked user gets a message for each order
placed, smart order, basket, split, options and multi-leg order, modify,
cancel, cancel all, position close, and GTT placed, modified, cancelled,
triggered or expired. Sandbox orders are marked **ANALYZE MODE - No Real
Order**. Failed and invalid requests are not alerted, as on the web. Alerts
are not sent while the bot is stopped.

### The Telegram Bot pages

- **Telegram Bot**: status, **Start Bot** or **Stop Bot**, total and active
  users, **Test Message** (**Send Test**, while the bot runs), the most used
  commands of the last 7 days, and **Broadcast** (a message to every linked
  user, after a confirmation).
- **Configuration**: the token, **Broadcast Messages** (allow or block
  broadcasts) and **Rate Limit (messages per minute)**. The rate limit caps
  what OpenAlgo sends each linked user: order alerts, notifications sent
  through the API, broadcasts and messages from these pages. A message above
  it waits for its turn, so a burst of order alerts arrives late but complete
  and in order; a message that could not go within the next minute (a burst
  of API notifications, for example) is not sent. OpenAlgo web stores this
  setting without applying it. Replies to bot commands have fixed limits of
  their own: 20 a minute to a linked user, 60 a minute in all, and one an
  hour to someone who has not linked.
- **Users**: each Telegram user with the OpenAlgo account, notification state
  and last activity; **Send Message** to one user, or **Unlink** them.
- **Analytics**: users and commands over the last 7 and 30 days.

### Chart alerts on Telegram

An alert on the `/trading` chart can also message you on Telegram: tick
**Telegram** under **When it fires** on the alert form. The bot must be
running and your account linked. See
[32 - Charting Terminal](../32-charting-terminal/README.md).

## WhatsApp

### Pairing

1. Open **WhatsApp Bot** from the profile menu.
2. Choose **Start pairing**. A QR code appears (it refreshes about every 30
   seconds), with a **pair code** below it.
3. On your phone open WhatsApp, **Linked devices**, **Link a device**, and scan
   the QR code, or use the pair code instead.
4. The status changes to **Connected** and "Device paired (owner: your
   OpenAlgo user name)". The bot starts by itself.

OpenAlgo Desktop becomes one of your WhatsApp linked devices, like WhatsApp
Web. The page never shows your phone number.

### Alerts and commands

- **Order alerts** go to your own number: they arrive in your chat with
  yourself. They cover the same order events as the Telegram alerts.
- **Commands**: send them from your own WhatsApp, outside groups. Only the
  paired owner is answered.

| Command | What it does |
| --- | --- |
| `/start`, `/help`, `/menu` | The list of commands |
| `/status` | Whether the bot is connected and the device paired |
| `/orderbook`, `/tradebook`, `/positions`, `/holdings`, `/funds`, `/pnl` | The same books as on Telegram, as text |
| `/quote <symbol> [exchange]` | A quote (exchange defaults to NSE) |
| `/closeall` | Closes all positions (no confirmation step) |
| `/mode` | Shows whether trading is live or analyze (it does not switch) |

- **Send a one-off message**: once paired, the page can send a WhatsApp
  message to any number (digits with the country code, for example
  `919876543210`).
- **Chart alerts**: tick **WhatsApp** under **When it fires** on a chart
  alert.

### The session is kept encrypted

OpenAlgo keeps the WhatsApp session as one encrypted snapshot in its
database, locked with the same key your OpenAlgo password unlocks; nothing
of it is written to disk in the clear. It is saved after pairing, every few
minutes when it changes, and when the bot stops, so a restart does not need a
new pairing. It travels with a backup of your data folder.

### Disconnecting

**Disconnect** removes the paired device after you confirm; pair again to
receive alerts. If WhatsApp itself logs the device out, the page and every
send say "WhatsApp logged this device out, so alerts cannot be sent. Pair it
again from the /whatsapp page in OpenAlgo."

## After a forgotten password or an account reset

Setting a new password from the sign-in page (Forgot password) cannot open
what the old password locked, which includes the Telegram token and the
WhatsApp session. Add your Telegram bot token again and pair WhatsApp again.
**Reset account** unlinks both.

## Troubleshooting

| What you see | Cause and fix |
| --- | --- |
| **Start Bot** is greyed out | No token saved. Save it on Configuration. |
| The bot does not answer | OpenAlgo is closed or you are not signed in; the bot is stopped; or you wrote in a group. Write to the bot in a private chat. |
| "Failed to validate API key" on `/link` | Wrong key, or a key from OpenAlgo web. Copy it from this desktop's API Key page. |
| Too many link attempts | Wait, then try again with the right key. |
| No order alerts on Telegram | The bot is stopped, or your account is not linked (the Users page lists who is linked and whether notifications are on). |
| `/closeall` refused | Semi-Auto is on. Close positions in the app, or switch to Auto. |
| WhatsApp shows Not paired after it worked | WhatsApp logged the device out, or the password was reset. Pair again. |
| "Pairing timed out" | Choose **Start pairing** again and scan within the time. |

---

**Previous**: [22 - Action Center](../22-action-center/README.md)

**Next**: [24 - PnL Tracker](../24-pnl-tracker/README.md)
