# 09 - API Key Management

## Overview

Your OpenAlgo API key lets outside tools use OpenAlgo Desktop: TradingView
alerts, Amibroker, Excel, Chartink, the Python SDK and your own scripts. It is
separate from your OpenAlgo password and from your broker's keys.

- There is **one API key**, created with your account. You do not need to
  generate one before you start.
- It is 64 characters long (32 random bytes written as hexadecimal), the same
  format as OpenAlgo web's.
- A key from OpenAlgo web does not work in the desktop, and the other way
  round: each has its own.

AI clients such as Claude Desktop do not use the API key. They get their own
tokens, made on the same page (see [AI clients](#ai-clients-mcp-tokens)
below).

## The API Key page

Open the profile menu (your initial, top right) and choose **API Key**. The
page has four cards.

### API Key Management

- The key, masked: its first and last 8 characters with dots between.
- The eye button shows or hides the whole key.
- **Copy** puts it on the clipboard.
- **Regenerate** makes a new key (see below).

Under the buttons the page says when the key works: "Programs using this key
work while you are signed in to OpenAlgo."

### Order Execution Mode

| Mode | What happens to orders from the API and webhooks |
| --- | --- |
| **Auto Mode** (Execute immediately) | Placed at once, with your broker or, in analyzer mode, in the sandbox |
| **Semi-Auto** (Queue for approval) | Held in the **Action Center** until you approve or reject them |

The line under the buttons states the current mode. In Semi-Auto mode,
orders that place something (regular, smart, basket, split, options and GTT
orders) wait in the Action Center, while modify, cancel, cancel-all and
close-position requests from the API are refused with a message such as
"Modify order operation is not allowed in Semi-Auto mode. Please switch to
Auto mode to modify orders." A tool cannot switch analyzer mode either: in
Semi-Auto that switch is yours alone, made in the app. Read-only calls
(funds, books, quotes) always answer at once.

### API Playground

A link to the **Playground**, where you send API requests from the app with
your key filled in. See [chapter 10](../10-placing-first-order/README.md).

### AI clients (MCP)

Tokens for Claude Desktop, Claude Code and other AI clients. See
[AI clients](#ai-clients-mcp-tokens) below.

## Using the key

Every `/api/v1` request carries the key in the JSON body, as `apikey`:

```json
{
  "apikey": "<APIKEY>",
  "strategy": "MyStrategy",
  "exchange": "NSE",
  "symbol": "SBIN",
  "action": "BUY",
  "quantity": 1,
  "pricetype": "MARKET",
  "product": "MIS"
}
```

Send it to `http://127.0.0.1:5000/api/v1/placeorder` (or your own **App
port**). The key goes in the body, not a header: a request that sends it only
in an `X-API-KEY` header is answered with the "missing field" error, exactly
as OpenAlgo web answers it.

### Python SDK

```python
import os
from openalgo import api

client = api(
    api_key=os.environ["OPENALGO_API_KEY"],
    host="http://127.0.0.1:5000",
    ws_url="ws://127.0.0.1:8765",
)
print(client.funds())
```

Keeping the key in an environment variable keeps it out of your script files.

### When the key works

| Situation | What a tool is told |
| --- | --- |
| You are signed in to OpenAlgo and your broker is logged in | The normal answer |
| OpenAlgo Desktop has just started, or you logged out | "Sign in to OpenAlgo to use the API. The API is off after OpenAlgo starts and after you log out, until you sign in again." (HTTP 503) |
| The key is wrong or was regenerated | "Invalid openalgo apikey" (HTTP 403) |
| Too many requests | HTTP 429. Limits, each counted per endpoint: 10 a second for placing, changing and cancelling orders and for smart orders, 30 a minute for option Greeks, Telegram and WhatsApp, 100 a second for the other calls. Also HTTP 429, with a sentence saying so, when your broker's own rate limit would have kept a request waiting more than 10 seconds: nothing was sent, try again in a few seconds |
| OpenAlgo Desktop is closed | No answer: the connection is refused |

The 503 answer is a desktop addition. OpenAlgo web has no locked state; the
desktop keeps the web's `{"status": "error", "message": ...}` shape so a
tool can tell "sign in first" from "wrong key". See
[chapter 05](../05-first-time-setup/README.md#what-your-password-protects).

## Regenerating the key

1. Choose **Regenerate** on the API Key page.
2. Confirm: "The current key will be invalidated and any applications using
   it will need to be updated."
3. The new key is shown. Copy it.
4. Update every tool that used the old key: TradingView alert messages,
   Amibroker, Excel, Python scripts, Chartink and GoCharting setups.
5. Test a read-only call, then an order in analyzer mode.

The old key stops working at once; there is no grace period. Market data
clients that authenticated with the old key are disconnected and must
connect again with the new one.

**When the key changes without you choosing Regenerate:** setting a new
password with **Forgot password?** creates a new API key, because the old one
was locked with the old password. Copy the new key into your tools afterwards.

## AI clients (MCP tokens)

OpenAlgo Desktop has a built-in MCP server. An AI client connects to it with
a **token** made on the API Key page, not with the API key.

1. In the **AI clients (MCP)** card, enter a **Client name** (for example
   "Claude Desktop").
2. Choose the **Access**:
   - **Read only**: account, books and market data, no orders.
   - **Read and place orders**: also places, modifies and cancels orders.
3. Choose **Create token**.
4. **Copy the token now. It is shown only once.** The card also shows a
   ready-made configuration for Claude Desktop
   (`claude_desktop_config.json`) and a command for Claude Code, with this
   computer's path to OpenAlgo Desktop filled in. Copy those.
5. Choose **Done**.

The token goes in the AI client's own configuration, in the environment
variable `OPENALGO_MCP_TOKEN`. The client starts `openalgo-desktop mcp`, which
passes its requests to the running app at `http://127.0.0.1:5000/mcp`.

The **Active tokens** list shows each token's name, access, first characters
and when it was last used. **Revoke** disconnects that client at once. Make one
token per client, so you can revoke one without the others.

Orders from an AI client follow the Live/Analyze switch like every other
order. AI clients work only while the app is open and you are signed in.

Full setup: [Connecting Claude Desktop and Claude Code (MCP)](../../user/mcp.md).

In the desktop, AI clients connect with these tokens. OpenAlgo web's OAuth
sign-in for hosted AI clients is not part of this version; AI clients on
another computer use the same tokens and need **Admin**, **Remote MCP**
turned on.

## Keeping the key safe

- Treat it like a trading password: anyone who has it can trade your account
  while OpenAlgo Desktop is open and you are signed in.
- Keep it in a password manager or an environment variable. Never commit it,
  paste it in a chat or issue, or show it in a screenshot.
- Regenerate it at once if it may have leaked.
- OpenAlgo Desktop stores it encrypted, checks presented keys against a
  one-way hash, and never writes it to its logs.

## Troubleshooting

| Problem | Check |
| --- | --- |
| "Invalid openalgo apikey" | Copy the key again from the API Key page (no spaces). Was it regenerated, or your password reset? |
| "Sign in to OpenAlgo to use the API." | Sign in to OpenAlgo Desktop. The key is not the problem |
| An order waits instead of being placed | Order Execution Mode is Semi-Auto: open **Action Center** |
| "... is not allowed in Semi-Auto mode" | Switch to **Auto Mode** to modify, cancel or close from the API |
| A field error such as "Missing data for required field." | Compare your request with the [API reference](../../api/README.md) |
| Requests from another device are refused after a few tries | Several invalid keys in a minute block that device for a while. Fix the key and wait |
| An AI client says "OpenAlgo did not accept this AI client's token" | The token was revoked or copied incorrectly. Create a new one |

More in [Troubleshooting](../../user/troubleshooting.md#your-api-client-says-the-api-key-is-invalid).

---

**Previous**: [08 - Understanding the interface](../08-understanding-interface/README.md)

**Next**: [10 - Placing your first order](../10-placing-first-order/README.md)
