# 05 - First-Time Setup

## Introduction

This chapter takes you from a fresh install to a working setup: your account,
your first sign-in, your broker, your API key, two-factor sign-in and a first
test in analyzer mode. Everything happens inside the app.

In the desktop there is no `.env` file to edit and no security keys to
generate: the keys that protect your data are created with your account.

## The steps

| Step | Where | Time |
| --- | --- | --- |
| 1. Create your account | Sign-in card, **First time? Set up your account** | 2 minutes |
| 2. Sign in | Sign-in card | 1 minute |
| 3. Add your broker's keys | **Broker Configuration** | 5 minutes |
| 4. Log in to your broker | **Select Broker** | 2 minutes |
| 5. Find your API key | Profile menu, **API Key** | 1 minute |
| 6. Turn on two-factor sign-in (optional) | Profile menu, **Profile**, **TOTP** tab | 3 minutes |
| 7. Test in analyzer mode | Mode switch in the top bar | 5 minutes |

## Step 1: Create your account

The first time you open the app, choose **Login** on the home page. Until an
account exists, the sign-in card shows **First time? Set up your account**.
Choose it. The **Initial Setup** page asks for:

| Field | What to enter |
| --- | --- |
| **Username** | The name you will sign in with |
| **Email** | Your email address. OpenAlgo Desktop sends no email; it is stored with your account |
| **Password** | A strong password (see below) |
| **Confirm Password** | The same password again |

The password needs at least 8 characters, with an uppercase letter (A-Z), a
lowercase letter (a-z), a number (0-9) and a special character from
`!@#$%^&*`. The page ticks each rule as you type and shows the strength.

Choose **Create Account**. You see "Account created successfully" and the
sign-in card.

What happens when you create the account:

- The account exists **only on this computer**. There is one account per
  installation.
- **Your API key is created at the same time.** Find it later on the **API
  Key** page.
- The keys that encrypt your data are created and locked with your password.
- The account can be created only from the OpenAlgo Desktop window or a
  browser on the same computer, never from another device.

## Step 2: Sign in

Enter your **Username** and **Password** and choose **Sign in**. If you have
turned on two-factor sign-in (step 6), the card then asks for the 6-digit
**Authentication code** from your authenticator app; choose **Verify code**.

After you sign in, OpenAlgo tries to resume your broker session from the same
trading day. If it can, you go straight to the dashboard; otherwise you go to
**Select Broker**. On the very first sign-in there is no broker yet, so you
land on Select Broker.

### What your password protects

OpenAlgo Desktop does not keep its encryption key in your operating system's
keychain. The key is locked with your OpenAlgo password, inside
`openalgo.db`. This is why signing in matters more than in OpenAlgo web:

| | Until you sign in after OpenAlgo starts, and after you log out | While you are signed in |
| --- | --- | --- |
| REST API (`/api/v1`) | Off. Programs are told "Sign in to OpenAlgo to use the API." | On |
| Market data WebSocket | Off. Clients are told OpenAlgo is locked | On |
| TradingView, Chartink, strategy and Flow webhooks | Refused; nothing is placed | On |
| Flow schedules, price alerts and order updates | Do not run; each skip is written in the workflow's run history | Run, once your broker is logged in |
| AI clients (MCP) | Told that OpenAlgo is locked | On |
| Your broker session | Not resumed | Resumed if still from today |
| Telegram and WhatsApp bots | Not running | Running, if you switched them on |

So **after every start of the app, sign in** before you expect alerts, scripts
or AI clients to work. Quitting OpenAlgo Desktop, or logging out, locks
everything again and wipes the unlocked key from memory.

The daily 03:00 IST boundary is different: it ends your sign-in and your
broker session, but it does not lock the keys. If you want the API off
overnight, log out.

A copy of your data folder is useless to anyone without your password.

### If you forget your password

The password cannot be recovered. On the sign-in card choose **Forgot
password?**, then **Set a new password**. This works only in the OpenAlgo
Desktop window on this computer, after a confirmation from your operating
system.

Setting a new password keeps your account, settings, strategies, watchlists
and history. Because the old password locked them, you then:

- enter your broker keys again on **Broker Configuration** and log in to your
  broker;
- copy the new API key from the **API Key** page into every tool that uses it;
- set up two-factor sign-in again, if you used it;
- add your Telegram bot again and pair WhatsApp again, if you used them;
- open each Flow workflow's **Webhook** settings to give it a new secret,
  switch its webhook on again and put the secret into TradingView or
  Chartink, if you used Flow webhooks.

The same page offers **Reset account**, to start over with a new account. It
removes your login, API key and saved broker keys, ends your broker session,
stops AI client tokens, unlinks Telegram and WhatsApp, gives your strategy,
Chartink and Flow webhooks new addresses, and deactivates every Flow
workflow. Your trade and strategy history and your workflows stay.

In the desktop, a forgotten password is never reset by email or with an
authenticator code: OpenAlgo web's email reset is not part of the desktop.

### Changing your password

**Profile**, **Account** tab, **Change Password**: enter the current password
and the new one twice. Your saved broker keys, API key and two-factor setup
stay as they are.

## Step 3: Add your broker's keys

On **Select Broker**, choose **Broker Configuration** (or **Configure** on
your broker's card). Choose your broker, enter the keys from its developer
portal, and choose **Save**. The page asks only for what that broker needs,
labelled the way the broker names it.

For brokers that sign in on their own website, register this redirect URL in
your broker's developer portal:

```
http://127.0.0.1:5000/<broker>/callback
```

for example `http://127.0.0.1:5000/zerodha/callback`. Broker Configuration
shows the exact address for your broker.

Details for every broker are in [chapter 06](../06-broker-connection/README.md)
and in [Broker sign-in](../../user/brokers.md).

## Step 4: Log in to your broker

On **Select Broker**, choose **Login with \<your broker\>** on its card.
Depending on the broker, this opens its login page, a small login card in the
app (for a TOTP, PIN or OTP), or signs you in with one click from the saved
keys.

When the login completes you reach the **Trading Dashboard**. Its **Master
Contract** indicator shows the broker's symbol list downloading, then "Ready"
with the number of symbols.

## Step 5: Find your API key

Open the profile menu (your initial, top right) and choose **API Key**. The
key was created with your account: choose the eye button to show it and
**Copy** to copy it. Use it in TradingView alerts, Amibroker, Excel and the
Python SDK. See [chapter 09](../09-api-key-management/README.md).

## Step 6: Turn on two-factor sign-in (optional)

Two-factor sign-in asks for a 6-digit code from your phone after your
password. Turn it on, especially if you allow other devices on your network or
use a tunnel.

1. Open the profile menu, **Profile**, then the **TOTP** tab.
2. Scan the QR code with an authenticator app (Google Authenticator, Authy or
   similar), or type the secret shown under **Manual Entry**.
3. In the **2FA Enforcement** card, turn on **Enable 2FA**, then **Dashboard
   sign-in**.
4. Enter the current 6-digit code under **Confirm with TOTP code** and choose
   **Save 2FA settings**.

From the next sign-in, the code is asked for after the password. More in
[28 - Two-factor authentication](../28-two-factor-auth/README.md).

The code guards sign-in only: it does not reset a forgotten password, and
there are no backup codes.

## Step 7: Test in analyzer mode

Before any live order, switch to analyzer mode with the mode switch in the
top bar. The badge changes from **Live Mode** to **Analyze Mode**, the
interface turns violet, and every order goes to the sandbox with 1 crore of
simulated capital. Then place a test order as in
[chapter 10](../10-placing-first-order/README.md).

## Setup checklist

- [ ] Account created, and you can sign in
- [ ] Broker keys saved on Broker Configuration
- [ ] Redirect URL registered at your broker (brokers that sign in on their website)
- [ ] Broker logged in, dashboard shows your funds
- [ ] Master Contract shows "Ready"
- [ ] API key copied where you need it
- [ ] Two-factor sign-in on (recommended)
- [ ] A test order placed in analyzer mode

## Common setup problems

| Problem | Fix |
| --- | --- |
| No **First time? Set up your account** link | An account already exists on this computer. Sign in, or use **Forgot password?** |
| "Create your account on the computer where OpenAlgo Desktop is installed." | You opened setup from another device. Use the app window on the computer itself |
| Sign-in slows down after mistakes | After five wrong passwords or codes from one place, OpenAlgo waits 30 seconds, then twice as long after each further mistake, up to 5 minutes |
| "Invalid credentials" in a browser although the password is right | Sign in in the OpenAlgo Desktop window, which says what is wrong. See [Troubleshooting](../../user/troubleshooting.md#signing-in-and-a-forgotten-password) |
| A tool says "Sign in to OpenAlgo to use the API." | The app was restarted or you logged out. Sign in to OpenAlgo |
| Broker login fails | [Troubleshooting: broker sign-in fails](../../user/troubleshooting.md#broker-sign-in-fails) |
| Dashboard says "Broker Session Expired" | Your broker session ended (about 03:00 IST). Choose **Reconnect Broker** |

---

**Previous**: [04 - Installation](../04-installation/README.md)

**Next**: [06 - Broker connection](../06-broker-connection/README.md)
