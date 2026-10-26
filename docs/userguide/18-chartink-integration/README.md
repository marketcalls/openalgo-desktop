# 18 - ChartInk Integration

## Introduction

ChartInk is a stock screener. When a scan finds stocks, ChartInk can call a
webhook with the list. OpenAlgo Desktop turns that call into orders for the
stocks you have mapped in a **Chartink strategy**: it reads the action from the
scan's name, places one order per mapped stock, and, for intraday strategies,
squares everything off at the time you set.

```
ChartInk scan finds stocks  --->  webhook to OpenAlgo Desktop  --->  orders for the mapped stocks
 "BUY - RSI Oversold"               /chartink/webhook/<id>             (to your broker, or the sandbox)
```

ChartInk does not let you write the request yourself: it always sends the
scan name and the stock list. That is why OpenAlgo uses its own webhook
address for ChartInk instead of `/api/v1/placeorder`.

## What you need

- A ChartInk plan that allows webhook alerts.
- OpenAlgo Desktop open and signed in, with your broker logged in.
- A public address for OpenAlgo Desktop (see the next section). ChartInk calls
  from its own servers on the internet, so `127.0.0.1` cannot reach it.

## Making the desktop reachable from ChartInk

Turn on the built-in ngrok tunnel in **Profile**, the **Broker** tab,
**Server Configuration** (your ngrok authtoken, then **Enable Ngrok Tunnel**;
see [chapter 16](../16-tradingview-integration/README.md#step-1-turn-on-the-built-in-tunnel)).
Or run a tunnel yourself on the same computer and tell OpenAlgo its address:

1. Start a tunnel to the app port, for example
   `cloudflared tunnel --url http://localhost:5000`. Use the port shown in
   **Server Settings** if you changed it.
2. Copy the tunnel's `https://` address.
3. Open **Profile**, the **Broker** tab, **Server Configuration**, enter it as
   **Host Server URL** and save.

The Chartink pages then show webhook addresses that start with the tunnel
address. Requests that arrive through a tunnel whose address is not entered
there are refused. When you stop using the tunnel, choose **Use automatic
address** under the field and save.

While no public address is set, the Chartink page says **Webhook URL not
accessible!**. More on tunnels and what they expose:
[Troubleshooting: another device, a firewall, or a tunnel](../../user/troubleshooting.md#another-device-a-firewall-or-a-tunnel).
A tunnel makes the OpenAlgo sign-in page reachable from the internet: use a
strong password and turn on two-factor sign-in
([28 - Two-Factor Authentication](../28-two-factor-auth/README.md)).

## Step 1: Create a Chartink strategy

1. Open **Platforms**, then **Chartink**, then **New Strategy**.
2. **Strategy Name**: 3 to 50 characters, letters, numbers, spaces, hyphens and
   underscores. OpenAlgo saves it as `chartink_<name>` (lower case, spaces
   replaced by underscores); the form shows the final name.
3. **Strategy Type**:
   - **Intraday**: set **Start Time**, **End Time** and **Square Off**
     (defaults 09:15, 15:00 and 15:15, IST). Start must be before End, and End
     before Square Off.
   - **Positional**: no trading window and no automatic square-off.
4. Choose **Create Strategy**. The strategy page opens.

## Step 2: Map the stocks to trade

Only stocks you map are traded. A stock in the alert that is not mapped is
skipped. On the strategy page choose **Configure Symbols**, then either:

- **Single Symbol**: search for the symbol, then set **Exchange** (NSE or BSE),
  **Quantity** and **Product** (MIS or CNC), and add it; or
- **Bulk Import**: paste one line per stock in the form
  `SYMBOL,EXCHANGE,QUANTITY,PRODUCT`, for example:

  ```
  RELIANCE,NSE,100,CNC
  TATAMOTORS,BSE,50,MIS
  ```

  Do not include a header line: every line is read as a stock, and a line
  whose exchange is not NSE or BSE stops the import with "Invalid exchange".
  After an import, check the **Symbol Mappings** table; the count in the
  confirmation message is not reliable.

The symbol must be written exactly as ChartInk sends it, which for NSE and BSE
stocks is the plain trading symbol (`SBIN`, `RELIANCE`).

## Step 3: Copy the webhook address

The strategy page's **Webhook Configuration** card shows the **Webhook URL**,
for example:

```
https://<your-tunnel-address>/chartink/webhook/<webhook-id>
```

The long id at the end is the only thing that proves an alert is yours. Keep
it private, like a password.

## Step 4: Name the scan and set the alert in ChartInk

1. In ChartInk, build and save your scan.
2. **Put one action keyword in the scan's name**: `BUY`, `SELL`, `SHORT` or
   `COVER`, for example "BUY - RSI Oversold Stocks". OpenAlgo reads the name
   ChartInk sends as `scan_name`. The app's own hint says "alert name"; to be
   safe, use the same keyword in the alert's name too.
3. Create the alert on the scan, turn on the webhook and paste the Webhook URL.

Use one keyword per scan. If a name holds more than one, OpenAlgo takes the
first match in this order: BUY, SELL, SHORT, COVER (so "SHORT COVER" is read as
SHORT).

## What each keyword does

| Keyword in the scan name | Order for each mapped stock in the alert |
| --- | --- |
| `BUY` | A MARKET BUY of the mapped quantity (opens or adds to a long) |
| `SELL` | A smart order to a position of zero (exits the long) |
| `SHORT` | A MARKET SELL of the mapped quantity (opens or adds to a short) |
| `COVER` | A smart order to a position of zero (exits the short) |

Exits (SELL, COVER) are smart orders: OpenAlgo compares with your open
position in that stock and sends only what closes it. Smart orders are placed
first, one a second; other orders go out up to ten a second.

## Trading hours for an intraday strategy

| Time (IST) | What happens to an alert |
| --- | --- |
| Before **Start Time** | Refused: "Cannot place orders before start time" |
| From **End Time** | BUY and SHORT refused ("Cannot place entry orders after end time"); SELL and COVER still placed |
| From **Square Off** | Everything refused: "Cannot place orders after squareoff time" |

At **Square Off**, every day the strategy is active, OpenAlgo sends a closing
smart order for every mapped stock. If the computer was asleep at that time,
the square-off runs when it wakes, provided it is still the same trading
session (before 03:00 IST); a square-off missed in an earlier session is not
sent, because it would close the new day's positions, and the log says so.
Check your positions after a long sleep.

## Analyzer mode, Semi-Auto and the lock

- Chartink orders take the same path as any API client. With analyzer mode on
  they go to the sandbox; with **Semi-Auto** selected on the API Key page they
  wait in the [Action Center](../22-action-center/README.md).
- Until you sign in after OpenAlgo starts, and after you log out, alerts are
  refused with "OpenAlgo is locked, so this alert was not acted on" and
  nothing is placed. Sign in to receive them again.
- An alert for an inactive strategy is answered but does nothing. Choose
  **Deactivate** on the strategy page to pause it, **Activate** to resume.

## Protection against wrong addresses

- Each caller may send up to 100 alerts a minute, and each webhook takes up to
  100 a minute.
- After 10 requests with a wrong webhook id aimed at one of your webhooks
  within 10 minutes, from any number of addresses, that webhook is locked for
  15 minutes, and even the right id is refused with "This Chartink webhook is
  locked after repeated alerts with a wrong webhook address". To unlock it at
  once, choose **Deactivate** and then **Activate** on the strategy page.
  Check who has the address first.

## Testing your setup

1. Turn on analyzer mode (top bar badge **Analyze Mode**).
2. Map one or two stocks with a small quantity.
3. Let the scan fire during market hours.
4. Check **Logs**, **Sandbox Logs** for the requests, and the order book and
   positions for the result.
5. Switch to **Live Mode** only when the orders look right.

## Troubleshooting

| What you see | Cause and fix |
| --- | --- |
| Nothing arrives | The tunnel is not running, its address changed, or Host Server URL is not set to it. Restart the tunnel, update the Host Server URL and the webhook address in ChartInk. |
| "No valid action keyword (BUY/SELL/SHORT/COVER) found in scan name" | Rename the scan so its name holds one keyword. |
| "No symbol mappings configured" | Map at least one stock on Configure Symbols. |
| "No orders were queued" | None of the stocks in the alert is mapped. Check the spelling in Symbol Mappings. |
| "Cannot place ... before start time / after end time / after squareoff time" | The alert came outside the intraday window. |
| "OpenAlgo is locked, so this alert was not acted on" | Sign in to OpenAlgo. |
| The webhook is locked | Choose **Deactivate**, then **Activate**, on the strategy page. |
| Orders queued but none placed | Look at **Logs**, **Live Logs**: a broker refusal, an expired broker session (log in again after 03:00 IST) or Semi-Auto waiting for approval. |

---

**Previous**: [17 - Amibroker Integration](../17-amibroker-integration/README.md)

**Next**: [19 - GoCharting Integration](../19-gocharting-integration/README.md)
