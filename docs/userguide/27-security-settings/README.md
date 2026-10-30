# 27 - Security Settings

## Overview

OpenAlgo Desktop runs for one person on one computer. Its security has four
parts:

1. **Your secrets are encrypted on disk**, with a key locked by your OpenAlgo
   password.
2. **Nothing reaches the app from outside this computer** unless you allow
   it.
3. **Sign-in and API keys are throttled** against guessing.
4. **The Security dashboard** lets you ban addresses that misbehave on your
   network.

Whoever controls your computer's user account can use the app while you are
signed in, so lock your computer when you step away.

## How your secrets are kept

There is no `.env` file and no secret is stored in plain text.

| What | How it is stored |
| --- | --- |
| Your OpenAlgo password | A one-way hash (Argon2). It cannot be read back |
| The key that encrypts everything else | Wrapped with a key derived from your password (Argon2id), in the `key_vault` table of `openalgo.db` |
| Broker keys and broker session tokens | Encrypted (AES-256-GCM), each tied to its own row and column |
| Your OpenAlgo API key | Encrypted, and checked through a keyed index plus Argon2 |
| Your two-factor (TOTP) secret | Encrypted |
| The Telegram bot token, linked users' API keys, the WhatsApp session | Encrypted |

OpenAlgo Desktop does **not** use your operating system's keychain on any
platform: not the macOS Keychain, not the Windows Credential Manager, not the
Linux Secret Service. Everything lives in the data folder, and a copy of that
folder is useless without your password. Details for maintainers:
[no keychain](../../port/no-keychain.md).

### Locked until you sign in

The encryption key is in memory only from the moment you sign in until you
log out or quit; then it is wiped. While OpenAlgo is locked (after it starts,
and after you log out):

- `/api/v1` answers 503 with "Sign in to OpenAlgo to use the API. The API is
  off after OpenAlgo starts and after you log out, until you sign in again."
- The market data feed, AI clients (MCP) and the TradingView, Chartink,
  strategy and Flow webhooks are refused the same way. Nothing is placed.
- Flow's schedules, price alerts and order updates do not run their
  workflows; each skip is written in the workflow's run history, and they
  run again once you sign in and log in to your broker.
- Your broker session does not resume and the Telegram and WhatsApp bots do
  not run.

Sign in and everything comes back on: the broker session from the same
trading day resumes, and bots you had switched on start.

The 03:00 IST boundary ends your broker session and your sign-in session, but
it does not lock the keys: only logging out or quitting does.

### Connections and encryption

This is the same answer as the FAQ's "How secure is OpenAlgo?":

- OpenAlgo Desktop's connections **to your broker** use the broker's own
  secure addresses (https and wss).
- The app serves your browser and your tools over **plain http and ws on this
  computer** (`127.0.0.1`). That traffic never leaves the machine.
- If you turn on **Allow access from other devices** in Server Settings, that
  traffic is **not encrypted**. Use it only on a network you trust, or reach
  OpenAlgo through a tunnel with https.
- The app also uses CSRF protection, rate limiting and secure sessions.

## Your password

### Changing it

**Profile**, **Account** tab, **Change Password**: enter the current password
and the new one twice. The new password needs at least 8 characters, with an
uppercase letter, a lowercase letter, a number and one of `!@#$%^&*`.

The new password replaces the hash and re-wraps the encryption key in one
step, so nothing has to be entered again. A sign-in started with the old
password while you change it does not complete.

### If you forget it

The password cannot be recovered: nothing else can open what it locked. On
the sign-in page choose **Forgot password?** in the OpenAlgo Desktop window
on the computer where it is installed (not in a browser or from another
device), then **Set a new password**. You keep your account, settings,
strategies, watchlists and history. Then:

- enter your broker keys again on **Broker Configuration** and log in to your
  broker;
- copy the new API key into every tool that uses it;
- set up two-factor sign-in again, if you used it;
- add your Telegram bot again and pair WhatsApp again, if you used them.

**Reset account** on the same page removes the account, its keys and saved
broker keys, ends the broker session, revokes AI client tokens, unlinks
Telegram and WhatsApp and gives your webhooks new addresses. Trade and
strategy history stay. It too works only in the desktop window.

More: [Troubleshooting: signing in, and a forgotten password](../../user/troubleshooting.md#signing-in-and-a-forgotten-password).

## Sign-in protection

- **Two-factor sign-in.** Turn it on in Profile; see
  [28 - Two-Factor Authentication](../28-two-factor-auth/README.md).
- **Slowing down guesses.** After five wrong passwords or codes from the same
  place (this computer, the tunnel, or one device on your network), signing
  in waits 30 seconds, then twice as long after each further mistake, up to 5
  minutes. Mistakes made elsewhere never slow down signing in on this
  computer.
- **Failures say nothing.** In a browser, from another device or through a
  tunnel, every failed sign-in reads "Invalid credentials", after the same
  amount of work, so a failure never tells anyone which part was wrong. Only
  the OpenAlgo Desktop window is told why the saved keys could not be opened.

## Who can reach the app

| Setting | Where | Default |
| --- | --- | --- |
| **Allow access from other devices** | Profile menu, **Server Settings** | Off: only programs on this computer can connect |
| **Host Server URL** (a tunnel's address) | **Profile**, **Broker** tab, **Server Configuration** | Empty. Requests through a tunnel whose address is not entered are refused |
| **Remote MCP** (AI clients on other computers) | **Admin**, **Remote MCP** | Off |

Leave all three off unless you need them. A tunnel or LAN access makes your
sign-in page reachable by others: use a strong password and two-factor
sign-in. Changing addresses or ports in Server Settings takes effect after a
restart.

## API keys and AI client tokens

- Your API key is on the **API Key** page. Regenerate it there if it may have
  leaked, then update every tool that uses it.
- Invalid keys are throttled per network address (never for programs on this
  computer; through a tunnel, per key tried), so a stranger's attempts cannot
  block your correct key.
- AI clients (MCP) use their own tokens, created on the API Key page with
  **Read only** or **Read and place orders** access, and revoked there. See
  [Connecting Claude Desktop and Claude Code](../../user/mcp.md).
- An API key carries no per-operation permissions: a valid key can call every
  `/api/v1` endpoint, subject to analyzer mode, Semi-Auto and rate limits.
- Rate limits are fixed: 100 API requests a second per caller, 10 orders a
  second, 100 webhook calls a minute per caller.

## The Security dashboard

Open **Logs**, then **Security Logs** (`/logs/security`); it is also on the
**Admin** page. The page title is **Security Dashboard** and it has three
tabs.

### Threats

**Security Threshold Settings**:

| Setting | Default | Meaning |
| --- | --- | --- |
| **Auto-Ban** | Off | When on, addresses that pass a threshold are banned automatically |
| 404 threshold | 100 a day | Requests for pages that do not exist |
| 404 ban duration | Default (24 hours) | How long an automatic ban for 404s lasts |
| API threshold | 100 a day | Invalid API key attempts |
| API ban duration | Default (24 hours) | How long an automatic ban for invalid keys lasts |
| Repeat Limit | 2 | Bans of the same address before a manual ban becomes permanent |

Choose **Save Settings** after a change.

In the desktop, automatic bans are always temporary: the duration you choose,
or 24 hours for **Default**. They apply only to devices on your network: never
to this computer, never to callers through a tunnel (who share one identity),
and never to this computer's own network addresses. The Repeat Limit applies
to bans you make by hand. On OpenAlgo web a duration of 0 means permanent and
repeat offenders are banned permanently.

Below the settings:

- counters for **Total Bans**, **Permanent Bans**, **Suspicious IPs** and
  **Near Threshold**;
- **Manual IP Ban**: an address, a reason and a duration (24 hours, 48 hours,
  1 week or **Permanent**), then **Ban IP**;
- **Ban by Host/Domain**: bans every address the traffic log has seen with
  that host name;
- **Banned IPs**, each with an unban button (after you confirm);
- **Invalid API Key Attempts** and **Suspicious IPs (404 Tracker)** (with
  sample paths), each address with a quick ban, and a clear for the 404
  tracker.

A banned address is refused everywhere: the app, the API, webhooks, MCP and
the market data feed.

### Login Activity

Every sign-in attempt with its time, status (Success, Failed or Resumed),
type, user name, address, broker and device. Filter by status, or clear the
history. Records are kept for a year, up to 10,000.

### Active Sessions

The signed-in sessions, with login time, address, broker, device and last
seen; yours is marked **This device**.

## A safe baseline

1. A strong, unique OpenAlgo password, and two-factor sign-in.
2. Server Settings left on this computer only, unless you need another device.
3. A tunnel only while you need webhooks, and **Use automatic address** when
   you stop.
4. Remote MCP off unless an AI client on another computer needs it.
5. A backup of the data folder, made with OpenAlgo closed (README, "Where
   your data lives, and backups"). It opens only with the password you had
   when you made it.
6. A look at the Traffic and Security dashboards after any failed sign-in you
   did not make.

---

**Previous**: [26 - Traffic Logs](../26-traffic-logs/README.md)

**Next**: [28 - Two-Factor Authentication](../28-two-factor-auth/README.md)
