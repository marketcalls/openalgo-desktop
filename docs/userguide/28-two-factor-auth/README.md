# 28 - Two-Factor Authentication

## Overview

Two-factor sign-in asks for a 6-digit code from an authenticator app (Google
Authenticator, Authy, Microsoft Authenticator and others) after your
password. Someone who learns your password still cannot sign in without your
phone.

It protects signing in to OpenAlgo Desktop itself. It is separate from any
TOTP, PIN or OTP your broker asks for when you log in to the broker.

Turn it on if anyone else can reach your sign-in page: when **Allow access
from other devices** is on, or when you run a tunnel for webhooks.

## Set it up

1. Open **Profile**, then the **TOTP** tab.
2. In your authenticator app, add an account by scanning **Your TOTP QR
   Code**, or type the **Secret key for manual entry**.
3. In the **2FA Enforcement** card, turn on **Enable 2FA**, then turn on
   **Dashboard sign-in** under "Apply 2FA to".
4. Enter the current 6-digit code from the app under **Confirm with TOTP
   code** and choose **Save 2FA settings**.

The code is asked for whenever you change these settings, turning 2FA on or
off, so a stolen session cannot switch it off.

Your TOTP secret is created with your account and stored encrypted, locked
with your OpenAlgo password like your other secrets. The QR code and the
secret are shown only to you while you are signed in; nobody else receives
them.

## The purposes, in the desktop

The **2FA Enforcement** card has one purpose, **Dashboard sign-in**: a code is
asked for after your password on every sign-in.

OpenAlgo web's card has two more, which the desktop does not show because it
has nothing for them to guard:

- **Remote MCP authorization**: the desktop has no OAuth consent screen. AI
  clients use tokens from the API Key page, with read-only or order access,
  and AI clients on other computers need **Remote MCP** switched on in Admin.
- **Password reset**: the desktop sends no email, and a forgotten password is
  replaced in the desktop window (below).

The master switch, **Enable 2FA**, must be on as well.

## Signing in with a code

1. Enter your username and password.
2. Enter the 6-digit code from your app.

You have 5 minutes after the password to enter the code. After that the
sign-in is forgotten ("Login session expired. Please sign in again."); start
from the password again.

Wrong codes count against the same allowance as wrong passwords: after five
mistakes from the same place, signing in waits 30 seconds, then longer after
each further mistake, up to 5 minutes.

## What two-factor does not cover

- **The API.** Programs that use your API key do not enter a code; protect the
  key like a password and regenerate it if it leaks.
- **AI client tokens** and **webhook addresses**, which carry their own
  secrets.
- **Your broker's login**, which has its own TOTP or OTP.

## If you lose your phone

There are no backup codes, and the code does not reset a forgotten password.

- **While you can still sign in** (on this computer, with the code or with
  2FA off): move the authenticator entry to your new phone by scanning the QR
  code on the **TOTP** tab again, or keep the secret in a password manager.
- **When you cannot sign in** because the code is gone: choose **Forgot
  password?** on the sign-in page, in the OpenAlgo Desktop window on this
  computer, and set a new password. That starts fresh: your account,
  settings, strategies and history stay; two-factor is off and gets a new
  secret, and you enter your broker keys again, copy the new API key into your
  tools, and add Telegram and pair WhatsApp again. See
  [27 - Security Settings](../27-security-settings/README.md#if-you-forget-it).

## Troubleshooting

| What you see | Cause and fix |
| --- | --- |
| "Invalid TOTP code." | The code expired, or your phone's clock is off. Set the phone and the computer to set the time automatically, and enter a fresh code. |
| Back at the password after the code | The 5 minutes ran out. Sign in again. |
| "TOTP Not Available" on the TOTP tab | The secret could not be read. Sign out and in again in the desktop window; if it stays, see [Troubleshooting](../../user/troubleshooting.md#signing-in-and-a-forgotten-password). |
| Your broker asks for another code | That is the broker's own two-factor login, set up with your broker. |

---

**Previous**: [27 - Security Settings](../27-security-settings/README.md)

**Next**: [29 - Troubleshooting](../29-troubleshooting/README.md)
