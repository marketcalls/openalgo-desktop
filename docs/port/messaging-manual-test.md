# Telegram and WhatsApp: live test plan

The automated suite (`src-tauri/tests/messaging.rs` and the unit tests in
`src-tauri/src/messaging/`) never contacts Telegram or WhatsApp: Telegram runs
against a local fake Bot API server and WhatsApp against a closed local port.
What only a real account can prove is below. Run it on a development build
(ports 5500 / 8766) with a broker connected, or in sandbox mode.

## Telegram

1. Create a bot with BotFather and copy its token.
2. `/telegram/config`: paste the token, save. `/telegram`: Start. Expect
   "Bot started successfully" and the bot's `@username` on the page.
3. In a private chat with the bot: `/start` (welcome text), `/help`.
4. `/link <your API key> http://127.0.0.1:5500`. Expect "Account linked
   successfully!" plus the note that commands run on this desktop. The user
   appears under `/telegram/users`.
5. `/menu` and each button; `/orderbook`, `/tradebook`, `/positions`,
   `/holdings`, `/funds`, `/pnl`, `/quote SBIN`, `/quote NIFTY NSE_INDEX`.
6. `/chart SBIN` (one intraday image), `/chart SBIN NSE both` (an album of
   two), `/chart NIFTY NSE_INDEX daily D 100`. Images: title, green and red
   candles, volume panel, date labels.
7. `/mode`, then the switch button: the app's mode toggle follows (sandbox
   mode on and off). Add the bot to a group, press the same button there:
   the bot answers only that it works in a private chat; mode unchanged.
8. Place an order from the API (`/api/v1/placeorder`) live and in sandbox
   mode: alerts arrive with "LIVE MODE - Real Order" and "ANALYZE MODE - No
   Real Order".
9. `/telegram`: Test message, Broadcast. `/trading` alert with Telegram on.
10. Stop the bot: alerts stop, `/api/v1/telegram/notify` answers 409.
11. Quit and relaunch the app with the bot started: it comes back by itself.
12. Disconnect the network for two minutes while started, reconnect: the bot
    answers again without a restart.

## WhatsApp

1. `/whatsapp`: Pair (no phone number). A QR appears; it refreshes about
   every 20 seconds. Scan it from WhatsApp, Linked devices. Expect "paired",
   the number shown, and the bot started.
2. Unlink, then pair with a phone number: an 8-character pair code appears;
   enter it on the phone.
3. In "Message yourself" on the phone: `/help`, `/status`, `/funds`,
   `/positions`, `/quote SBIN`, `/mode`. Replies arrive in the same chat.
   The same commands sent in a group, or by another contact, get no answer.
4. Place an order: the alert arrives in "Message yourself".
5. `/whatsapp`: Send to a number; Test message (needs a linked user).
   `/api/v1/whatsapp/notify` with `"self": true`, with `"phone"`, with
   `"username"` set to the account name.
6. Quit and relaunch: the bot reconnects from the saved session without a
   new QR. Repeat after 10 minutes of use (the saved session is the live
   one, not the one from pairing).
7. Remove the device on the phone (Linked devices): the page says WhatsApp
   logged the device out and to pair again; notify answers 409 with that
   text; after a relaunch the page shows unpaired.
8. Turn Wi-Fi off for two minutes, then on: the status returns to running.
