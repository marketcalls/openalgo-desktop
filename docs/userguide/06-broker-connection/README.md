# 06 - Broker Connection

## Overview

OpenAlgo Desktop works with 37 brokers, including Rupeezy and Delta Exchange
(crypto). You enter each broker's keys inside the app, there is no `.env`
file, and:

- **one set of keys per broker**, for as many brokers as you like;
- **one broker logged in at a time**;
- **one account per broker**: to use another account at the same broker, you
  replace the saved keys.

Two pages handle it:

| Page | Where | What it does |
| --- | --- | --- |
| **Broker Configuration** (`/broker/config`) | **Broker Configuration** on Select Broker, or **Profile**, **Broker** tab | Save, change or forget each broker's keys |
| **Select Broker** (`/broker`) | Shown after sign-in when no broker is logged in | One card per broker: log in, switch, open the dashboard |

In the desktop, broker keys and the login flow follow a different design from
OpenAlgo web, where one broker's keys live in `.env` and a change needs a
restart. Here keys are saved per broker and apply as soon as you save them.

The full reference for every broker is [Broker sign-in](../../user/brokers.md).
This chapter walks you through it.

## Step 1: Create an API app at your broker

In your broker's developer portal, create an API app (sometimes called a
Connect app, a Vortex app or similar) and note the keys it gives you.

If your broker signs in on its own website (see the list below), the portal
asks for a **redirect URL**. Enter:

```
http://127.0.0.1:5000/<broker>/callback
```

for example:

```
http://127.0.0.1:5000/zerodha/callback
http://127.0.0.1:5000/fyers/callback
http://127.0.0.1:5000/upstox/callback
```

This is the same address OpenAlgo web uses, so a broker app you already set up
for OpenAlgo web on this computer works unchanged. If you changed the **App
port** in Server Settings, use that port instead of 5000. The Broker
Configuration page always shows the exact address for your broker.

The address must match exactly. Upstox, for example, refuses a sign-in whose
redirect URL differs by one character: `localhost` instead of `127.0.0.1`,
`https` instead of `http`, another port or a trailing slash all count.

## Step 2: Save the keys in Broker Configuration

1. Open **Broker Configuration**.
2. Under **API Credentials**, choose your broker in the **Broker** list. A
   badge shows **Active** (logged in now), **Configured** (keys saved) or
   **Not Configured**, with a short note on how this broker's login works.
3. Fill in the fields. The page shows only the keys this broker needs,
   labelled the way the broker names them (for example **App ID** for Fyers,
   **SmartAPI Key** for Angel One, **Application ID** for Rupeezy), each with a
   hint.
4. Check the **Redirect URL** field (brokers that sign in on their website).
5. Choose **Save**.

What to know about saved keys:

- **Saved keys are never shown again in full.** The page shows a masked value
  such as `Saved: abc***xyz`, and the field below it is empty. Leave it blank
  to keep the saved value, or type a new value to replace it.
- **Choosing another broker clears what you typed**, so nothing typed for one
  broker is saved under another.
- **New keys for the broker that is logged in end its session**, after you
  confirm, because a session belongs to the keys it signed in with. Log in
  again to use the new keys. Changing only the redirect URL ends nothing.
- **Forget this broker** removes a broker's keys from this computer after you
  confirm. For the broker that is logged in, it signs you out of it first
  (**Sign out and forget**). Positions and orders stay at the broker.
- **Fyers** also has an **API plan** choice. OpenAlgo paces Fyers requests to
  stay within that plan's limits; a change applies at once and does not sign
  you out.

### The redirect URL field

| What it shows | Meaning |
| --- | --- |
| Automatic | `http://127.0.0.1:<app port>/<broker>/callback`; while **Enable Ngrok Tunnel** is ticked in Profile, the built-in tunnel's address when it is online, else your **Host Server URL**. A sign-in that comes back through the built-in tunnel is passed on to this computer |
| Custom address | An address you typed. It must be a full address ending in `/<broker>/callback` |

**Use automatic** goes back to the automatic address; an empty field means
automatic too. The redirect URL is kept with that broker's keys.

### Brokers bound to your client ID

For some brokers OpenAlgo checks that the account you log in with is the
account you set up, so nobody can slip their own session into your app. These
need your client ID:

| Broker | Where the client ID goes |
| --- | --- |
| Dhan, Flattrade | In the first key, as `client_id:::api_key` |
| Shoonya, Zebu, TradeSmart | In the first key, as `userid:::client_id` |
| Arrow, HDFC Sky, HDFC Securities, Rupeezy | The **Client ID** field |
| Alice Blue | The **User ID** field |

A login with a different account is refused: "This sign-in is for a different
account than the one set up in OpenAlgo".

## Step 3: Log in

Open **Select Broker**. Brokers with saved keys are listed first; the search
box filters the cards. Each card shows the broker's badge and the exchanges it
trades. Choose **Login with \<broker\>** on your broker's card.

What happens next depends on how your broker signs in:

| Kind | Brokers | What you do |
| --- | --- | --- |
| On the broker's website | Zerodha, Fyers, Upstox, Dhan, Arrow, Paytm Money, Pocketful, HDFC Sky, HDFC Securities, IIFL Capital, Alice Blue, CompositEdge, RMoney, Flattrade, Shoonya, Zebu, TradeSmart, Rupeezy | Log in on the broker's page and approve access; the broker sends you back to OpenAlgo |
| A form in the app | Angel One, Kotak Securities, 5 Paisa, Firstock, Motilal Oswal, mStock, Tradejini, Nubra, IndMoney, Definedge, Groww; Samco on its own **Connect** page | Type the TOTP, PIN, OTP or password the card asks for. OpenAlgo uses them for that login only and does not save them |
| One click from saved keys | 5 Paisa (XTS), JainamXts, Ibulls, IIFL, Wisdom Capital, Delta Exchange, Dhan (Sandbox) | Nothing to type |

For the XTS brokers (and CompositEdge and RMoney), also fill in the **Market
Data App Key** and **Market Data Secret Key**: quotes and the live feed need
them.

When the login completes you reach the dashboard. If it fails, you come back
to Select Broker with the reason on that broker's card. Always start from
**Login with**: a sign-in OpenAlgo did not start, or one that took too long, is
refused with "This broker sign-in was not started from OpenAlgo or has
expired".

Per-broker fields, formats and notes:
[Broker sign-in](../../user/brokers.md).

## After the login

- **The master contract downloads.** The dashboard's **Master Contract**
  indicator shows "Downloading...", then "Ready" with the symbol count. Until
  then, symbol search, option chains and orders by symbol do not work.
- **The broker's badge** shows in the top bar.
- **Market data subscriptions.** Zerodha needs an active Kite Connect
  subscription, and Dhan an active Data API subscription, for market data.
  Their cards say so.

### Check the connection

Before your first order:

- The dashboard shows your **Available Balance** without an error.
- **Orderbook** and **Positions** load.
- **Master Contract** shows "Ready".
- Place a test order in analyzer mode ([chapter 10](../10-placing-first-order/README.md)).

## One broker at a time

Logging in to another broker ends the current broker's session. OpenAlgo asks
first: "Signing in to Y ends your X session. Open positions and orders stay
with X." The X session ends only when the sign-in to Y completes; if it fails,
X stays connected.

A switch is refused while something on X still depends on its session: Live
strategies, OpenScript runs or scalping protection still running, orders whose
outcome is not known yet, or exits that are partly filled. Positions and
working orders you placed by hand stay at X and are no longer watched by
OpenAlgo. Analyzer-mode runs carry on with the new broker's prices.

The keys of the broker you leave stay saved, so switching back needs only a
login. No restart is needed. Details:
[Switching brokers](../../user/brokers.md#switching-brokers).

## One account per broker

OpenAlgo keeps one key set per broker, by design. Several accounts at the same
broker are not supported in this version. To use another account at a broker,
enter that account's keys in place of the saved ones (or **Forget this
broker** and add them again); the next login is then checked against the new
account.

## The daily broker login

Indian brokers end API sessions every night. OpenAlgo Desktop:

- ends the broker session at about **03:00 IST**;
- ends your OpenAlgo sign-in at the same time, so you sign in to OpenAlgo
  again, then log in to your broker from Select Broker;
- keeps a login made after 03:00 IST, even if the computer was asleep at
  03:00.

The boundary is fixed at 03:00 IST; there is no setting for it.

When you sign in during the same trading day, OpenAlgo resumes the broker
session and takes you straight to the dashboard. If the broker session ended
while you were working, the dashboard shows **Broker Session Expired** with a
**Reconnect Broker** button.

**Delta Exchange** trades around the clock and its keys do not expire: its
session stays connected across 03:00 IST, and its symbol list refreshes after
00:00 UTC. You still sign in to OpenAlgo itself again after 03:00 IST.

Logging out of OpenAlgo also ends the broker session.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Broker rejects the redirect | The redirect URL in the broker portal matches the one Broker Configuration shows, character for character |
| "This broker sign-in was not started from OpenAlgo or has expired" | Choose **Login with** again and finish the broker's login within a few minutes |
| A message asks for your client ID or User ID | Fill in the field Broker Configuration names (see the table above) |
| "This sign-in is for a different account than the one set up in OpenAlgo" | Log in with the configured account, or save the new account's keys first |
| TOTP rejected | Wait for a fresh code; check that the computer's clock is set automatically |
| Logged in but no quotes or feed | Data subscription (Zerodha, Dhan); Market Data keys (XTS brokers, CompositEdge, RMoney) |
| Samco refuses the login | Your internet address must be the static IP registered with Samco |
| Symbols missing | **Master Contract** page in the profile menu: **Force Download** |
| Switch to another broker refused | Stop the Live strategies, OpenScript runs or scalping protection the card names, or let pending orders settle |

More in [Troubleshooting](../../user/troubleshooting.md#broker-sign-in-fails).
If it still fails, open **Logs**, then **Live Logs**, while you try again; it
shows where the sign-in stopped.

---

**Previous**: [05 - First-time setup](../05-first-time-setup/README.md)

**Next**: [07 - Dashboard overview](../07-dashboard-overview/README.md)
