# 21 - Flow Visual Builder

## Introduction

**Flow** is a drag-and-drop canvas for trading automations. You wire nodes
from a trigger (a schedule, a webhook alert, a price alert or an order update)
through data, indicators and conditions to orders and alerts, then activate the
workflow and it runs on its own while OpenAlgo Desktop is open.

```
trigger            data and conditions                  actions
Schedule  ---->  Get Quote, Indicator, Funds  ---->  Place Order, Options Order,
Webhook          Price Check, Time Window,           Close Positions, Telegram,
Price Alert      AND / OR / NOT gates                HTTP Request ...
Order Update
```

In the desktop, Flow runs natively inside the app: it needs no Python. A
workflow is the same JSON as in OpenAlgo web, so a workflow exported from
OpenAlgo web imports into the desktop unchanged, and back
([Import and export](#import-and-export-moving-workflows-to-and-from-openalgo-web)).

Flow places orders through the same path as any API client, so analyzer mode
(sandbox), Semi-Auto and the order alerts apply to it. Build and test every
workflow in analyzer mode first.

## What you need

- OpenAlgo Desktop open and signed in. A trigger that fires while you are
  signed out does not run the workflow (see
  [When a workflow does not run](#when-a-workflow-does-not-run)).
- Your broker logged in, for live orders and market data. Without one a
  workflow still runs: in analyzer mode its orders go to the sandbox (fills
  still need your broker's prices), and in live mode each node that needs the
  broker fails.
- An API key on the **API Key** page. Workflows trade with the app's current
  key, read each time a run starts, so regenerating the key needs no change to
  any workflow.
- For TradingView, Chartink and other alerts from the internet: a public
  address, best the built-in ngrok tunnel
  ([chapter 16](../16-tradingview-integration/README.md#step-1-turn-on-the-built-in-tunnel)).
- For the Telegram and WhatsApp nodes: the bots set up as in
  [23 - Telegram Bot (and WhatsApp)](../23-telegram-bot/README.md).

## The Flow page

Open the profile menu and choose **Flow Editor** (between **Holdings** and
**Scalping**). The **Flow Editor** page lists your workflows as cards:

- a green dot and a green edge when the workflow is active, grey when not;
- **Last:** and the last run's result (`completed`, `failed`, `running`), or
  **No executions**;
- the date it was last changed.

Choose a card to open it in the editor. The card's menu has **Edit**,
**Webhook** (the webhook settings), **Activate** or **Deactivate**, and
**Delete**. Delete asks first, because the workflow's schedule, webhook address
and run history go with it, and an active workflow stops.

**New Workflow** asks for a name and opens the editor. **Import** brings in a
workflow file (below).

## The editor

```
+------------------------------------------------------------------------+
| openalgo   Flow Editor                 mode, theme, profile menu       |
| <-  Workflow name   Unsaved   [Save] [Activate]   [...]                |
+-----------+---------------------------------------------+--------------+
| Nodes     |                                             | settings of  |
| Trigger   |            canvas                           | the selected |
| Action    |   (drag nodes here, connect their dots)     | node, or the |
| Data      |                                             | Execution    |
| Stream    |                                             | Log          |
| Logic     |                   Workflow inactive         |              |
| Util      |                                             |              |
+-----------+---------------------------------------------+--------------+
```

- **The node palette** on the left has six tabs: **Trigger**, **Action**,
  **Data**, **Stream**, **Logic** and **Util**. Drag a node onto the canvas,
  or choose it to add it in the middle.
- **Connect** two nodes by dragging from the dot at the bottom of one to the
  top of the next. A condition has two outputs, true and false (Yes and No on
  Time Condition and NOT Gate). The **+** on a connection inserts a node
  between the two.
- **Select a node** to edit its settings on the right. Fields that take a
  value from an earlier node have a `{ }` switch: a text box where you write a
  variable such as `{{webhook.symbol}}`.
- **The bottom of the canvas** says **Workflow active** or **Workflow
  inactive**; the minimap and the zoom controls sit in the corners.
- **Save** (or Ctrl+S, Cmd+S on a Mac) stores the workflow. **Unsaved** shows
  while there are changes. Leaving the editor with unsaved changes asks
  **Leave without saving?** first.
- **The header** is the app's own: the analyzer mode switch, the theme and the
  profile menu, so you can see at a glance whether orders go to the sandbox.

The **...** menu has **Run Now**, **Show Logs** / **Hide Logs**, **Export
Workflow**, **Replace from JSON** and **Keyboard Shortcuts**.

## The nodes

There are 61 node types. Each family is a palette tab.

### Triggers (Trigger tab)

A workflow has exactly one trigger: it decides when the workflow runs.

| Node | Runs the workflow |
| --- | --- |
| **Schedule** | On a clock: once, every day, on chosen weekdays, or every N seconds, minutes or hours. Can be held to market hours |
| **Webhook** | When TradingView, Chartink or your own program sends an alert to the workflow's webhook address |
| **Price Alert** | When a symbol's last price crosses, rises above or falls below a level (checked every 5 seconds) |
| **Order Update** | When an order you name, or any order in a symbol, is filled, rejected, cancelled or changes status |

### Actions (Action tab)

| Node | Does |
| --- | --- |
| **Place Order** | One order, quantity in shares |
| **Smart Order** | An order that takes your position in the symbol to a target size |
| **Options Order** | One option leg from an underlying, an offset (ATM, ITM1..ITM50, OTM1..OTM50), CE or PE and an expiry; quantity in lots |
| **Multi-Leg** | A straddle, strangle, iron condor, bull call or bear put spread, or up to 10 custom legs with their own strikes and expiries |
| **Basket Order** | Several orders in one go, one `SYMBOL,EXCHANGE,ACTION,QTY` line each |
| **Split Order** | One large order sent in chunks |
| **Modify Order** | Changes the price, quantity or trigger price of an open order |
| **Cancel Order** / **Cancel All** | Cancels one order, or every open order |
| **Close Positions** | Squares off every open position, or only the symbol you name |

Product is optional on every order node: leave it empty and a derivatives
exchange (NFO, BFO, MCX, CDS and the others) uses NRML, everything else MIS.
The options nodes use NRML.

### Conditions (Logic tab)

| Node | Asks |
| --- | --- |
| **Time Condition** | Is it before, at or after a time? |
| **Position Check** | Is there a position in the symbol, above or below a quantity or P&L? |
| **Fund Check** | Is at least this much cash available? |
| **Price Check** | Is the LTP, open, high, low, previous close or change % above or below a value? |
| **Time Window** | Is it inside (or outside) a window, such as 09:30 to 15:00? A window may cross midnight |
| **Var Condition** | Compare any two values: an indicator, a quote field, a variable or a number |
| **AND Gate**, **OR Gate**, **NOT Gate** | Combine conditions: all true, any true, or the opposite |

A condition that cannot get a trustworthy answer (the broker read failed, a
value did not arrive) takes **neither** branch and the run is marked failed.
It never takes the false branch as if the check had run.

### Data (Data tab)

| Node | Gives you |
| --- | --- |
| **Get Quote**, **Multi Quotes**, **Get Depth** | Live quote, quotes for many symbols, the order book |
| **History** | Candles for a symbol (at most the latest 200) |
| **Indicator** | Any of 116 indicators (RSI, EMA, Supertrend, MACD and more) on a symbol, or on another indicator's output |
| **Prior Period OHLC** | The previous hour's, day's, week's or month's open, high, low and close |
| **Bar Offset** | The candle N bars back |
| **Strategy P&L** | Realized, unrealized and total P&L of this workflow's own orders |
| **Open Position**, **Order Status** | One position, one order |
| **Order Book**, **Trade Book**, **Position Book**, **Holdings**, **Funds** | Your books and balance |
| **Symbol Info**, **Option Symbol**, **Expiry Dates**, **Intervals** | Lot size and contract details, an option's symbol, the listed expiries, the broker's candle intervals |
| **Option Chain**, **Synthetic Future** | The chain around ATM, the synthetic future price |
| **Calendar** | Trading-day facts: is this a trading day, has a new week or month started |
| **Margin Calc** | The margin an order or basket needs |

### Streams (Stream tab)

**Subscribe LTP**, **Subscribe Quote** and **Subscribe Depth** read the live
market data feed (the one the trading terminal uses) and fall back to a quote
when no update comes within a few seconds. **Unsubscribe** drops a stream.
Subscriptions are given back when the workflow is deactivated or deleted, and
Unsubscribe with **all** drops only this workflow's own, never the trading
terminal's or the sandbox's.

### Utilities (Util tab)

| Node | Does |
| --- | --- |
| **Variable** | Stores a value for later nodes (set, add, increment, parse JSON and more) |
| **Math** | Works out a number, such as `floor(0.05 * {{f.data.availablecash}} / {{q.data.ltp}})` |
| **Log** | Writes a line in the run's log |
| **Telegram**, **WhatsApp** | Sends you an alert through the desktop's own bots |
| **Delay** | Waits up to 5 minutes |
| **Wait Until** | Waits until a time (IST), at most 30 minutes ahead |
| **HTTP Request** | Calls a web address on the internet |
| **Holidays**, **Timings** | Market holidays of a year, market timings of a day |
| **Group** | A coloured frame to tidy the canvas; it does nothing when the workflow runs |

**HTTP Request** reaches public addresses only: it refuses this computer,
your home network, the Tailscale range and cloud metadata addresses, never
follows a redirect, and gives up on an answer larger than 5 MB.

The Telegram alert goes to the Telegram account you linked to the bot. If the
bot is stopped or your account is not linked, the node fails and nothing below
it runs, so put alerts **after** the orders they report, never before.

## Variables: passing values between nodes

Most nodes have an **Output Variable**, a name under which the node keeps its
result. A later node reads it with `{{name.field}}`:

| You write | You get |
| --- | --- |
| `{{quote.data.ltp}}` | The LTP from a Get Quote node whose output variable is `quote` |
| `{{rsi.latest.value}}` | The latest RSI from an Indicator node named `rsi` |
| `{{rsi.previous.value}}` | The RSI of the bar before, the last closed bar |
| `{{order.orderid}}` | The order id from a Place Order node named `order` |
| `{{webhook.symbol}}` | The `symbol` field of the alert that started the run |
| `{{time}}`, `{{date}}` | The time and date in India (IST), whatever your computer's time zone |

Every field on an order node takes a variable, including exchange, action,
product and price type. A variable that does not resolve on an order node
stops the node before anything is sent: a webhook that leaves out `quantity`
never places an order of 1. On other nodes an unresolved `{{...}}` stays in the
text as written, which shows up in the log.

The Indicator node's latest bar is usually the one still forming; use
`{{name.previous.*}}` (or **Offset Bars** 1) for the last closed bar.

## Triggers in detail

### Schedule

| Setting | What it does |
| --- | --- |
| **Once** | Runs at a date and time, then the workflow is deactivated |
| **Daily** | Runs every day at a time, or only on the days you tick |
| **Weekly** | Runs on the days you tick at a time |
| **Interval** | Runs every N seconds, minutes or hours, on the clock: every 5 minutes is 09:15:02, 09:20:02 and so on, wherever you activated it |
| **Market hours only** | Skips runs outside the exchange's session; weekends and trading holidays stay shut whatever the times say. Choose the exchange you trade so MCX and crypto get their own hours |

Every time is India time (IST), whatever time zone your computer uses.
Schedules run only while OpenAlgo Desktop is open: a run that falls due while
the app is closed or the computer sleeps runs once if the app is back within a
minute of it, and is otherwise skipped. Missed runs never run in a burst.

### Webhook (TradingView, Chartink and your own programs)

Each workflow has its own webhook address and secret, made when the workflow
is created or imported. Open the workflow card's menu, **Webhook**:

1. Turn on **Enable Webhook**.
2. Choose the **Authentication Method**:
   - **Secret in Payload** (for TradingView and your own scripts): the alert
     message is JSON with a `secret` field;
   - **Secret in URL** (for Chartink and other services whose message you
     cannot write): the secret goes in the address, `?secret=...`.
3. Copy the **Webhook URL** (or **Webhook URL (with secret)** for URL
   authentication) into TradingView or Chartink. **Webhook URL (with symbol)**
   puts the symbol in the address: replace `{symbol}` with, for example,
   `RELIANCE`.
4. Activate the workflow.

The addresses shown are the ones the alert service should call: the built-in
ngrok tunnel's while the tunnel is online, otherwise your Host Server URL. When
they are on this computer's own address (`http://127.0.0.1:5000`), the dialog
says **Webhook URL not accessible!**: TradingView and Chartink cannot reach
your computer. Turn on the tunnel in **Profile**, **Broker** tab, **Server
Configuration**, as in
[chapter 16](../16-tradingview-integration/README.md#step-1-turn-on-the-built-in-tunnel).
Through the tunnel, only the webhook addresses reach Flow: the Flow pages and
the workflows' settings stay on your computer.

A TradingView alert message for a workflow that reads `{{webhook.symbol}}`,
`{{webhook.action}}` and `{{webhook.quantity}}`:

```json
{
  "secret": "<your webhook secret>",
  "symbol": "{{ticker}}",
  "action": "BUY",
  "quantity": 10
}
```

The `secret` field is checked and removed; every other field reaches the
workflow as `{{webhook.<field>}}`. A message that is not JSON reaches it as
`{{webhook.message}}`, which only works with **Secret in URL**.

To test from this computer, send the same alert yourself (the URL and secret
from the dialog):

```bash
curl -X POST "http://127.0.0.1:5000/flow/webhook/<token>" \
  -H "Content-Type: application/json" \
  -d '{"secret":"<secret>","symbol":"SBIN","action":"BUY","quantity":1}'
```

The answer says whether the workflow ran (`Workflow '<name>' triggered`) or
why not. Things to know:

- The webhook must be switched on (otherwise "Webhook is disabled") and the
  workflow active ("Workflow is not active").
- Every webhook needs its secret. A missing or wrong secret is refused, and
  ten wrong secrets within 10 minutes lock that workflow's webhook for 15
  minutes. Regenerate the secret, or switch the webhook off and on, to unlock
  it at once.
- Each address takes up to 100 alerts a minute.
- While OpenAlgo is signed out, alerts are refused and nothing is placed, as
  for the strategy and Chartink webhooks.
- **Regenerate URL & Secret** asks first: every alert set up with the old
  address or secret stops working.
- Keep the address and the secret private. The traffic log stores the address
  with its secret part replaced.

### Price Alert

Choose the symbol, **Greater Than**, **Less Than**, **Crosses Above** or
**Crosses Below**, and the price. The price is checked every 5 seconds while
the workflow is active. **Only Once** runs the workflow on the first match and
then deactivates it; **Every Time** keeps watching. **Expiration** (1 hour to
1 week) stops the watch and deactivates the workflow. The run can read the
price that matched as `{{webhook.trigger_price}}`. **Play Sound** and **Alert
Message** are kept with the workflow so it moves between OpenAlgo web and the
desktop, but nothing reads them.

### Order Update

Name an order id or a symbol (or both), and the status to wait for: complete,
rejected, cancelled, open, trigger pending or any. The run can read
`{{webhook.orderid}}`, `{{webhook.order_status}}`,
`{{webhook.average_price}}`, `{{webhook.filled_quantity}}` and
`{{webhook.rejection_reason}}`. In analyzer mode, sandbox fills fire it too.

## Activating and running

- **Activate** checks the whole workflow first. If something is missing or
  wrong (a symbol, a quantity, a node not connected to the trigger, a gate
  with an input left unwired), the message names it and the workflow stays
  inactive. Then the trigger starts watching.
- **Run Now** saves the workflow and runs it once, now, whether it is active
  or not and outside market hours too. The **Execution Log** on the right
  shows every node's line: what it read, what it sent, what came back.
- **Deactivate** stops the trigger and gives back the workflow's market data
  subscriptions. While a run is going it answers "This workflow is still
  running. Try again when it finishes."

A workflow runs one run at a time. A second never starts while one is going:
a webhook alert is answered "Workflow is already running", and a schedule
skips that run.

A run that waits (Delay, Wait Until) more than 10 seconds in total is
answered at once and finishes in the background: the log panel says
**Running in background**, and its orders show in the order book when the wait
ends. A webhook sender gets the same quick answer, so TradingView never waits
for it.

When a node fails (the broker refused an order, a check could not be made,
the Telegram bot is stopped), nothing below it runs and the run is recorded as
failed with the broker's or the service's own words. Put independent work on
its own branch from the trigger.

### When a workflow does not run

| Situation | What happens |
| --- | --- |
| You are signed out when a trigger fires (from the app's start until you sign in, or after you log out) | The workflow does not run. Webhook alerts are refused; a schedule, price alert or order update is recorded in the run history as skipped, with the reason. The triggers run again once you sign in. Signing out does not deactivate a workflow |
| You are signed in but no broker is logged in (not yet, or after the 03:00 IST session end) | The workflow runs. In analyzer mode its orders and account reads go to the sandbox (quotes and fills still need your broker's prices). In live mode each node that needs the broker fails with a message that no broker is logged in, and its branch stops; alerts, HTTP requests, variables and time checks still run. Log in to your broker from **Select Broker** |
| You sign out or quit while a run is going | The run stops where it is. One cut off by quitting is recorded at the next start as "OpenAlgo closed while this run was in progress; it did not finish." Check your orders and positions |
| The app was closed when a schedule fell due | Skipped, unless the app was back within a minute |
| Outside market hours, with **Market hours only** on | The schedule skips that run |

The run history keeps the newest 500 runs of each workflow, up to 30 days.

## Analyzer mode, Semi-Auto and the order tag

- **Analyzer mode**: with analyzer mode on, every Flow order goes to the
  sandbox, exactly as an API order does, and the books Flow reads are the
  sandbox's. The editor's header shows the mode and switches it.
- **Semi-Auto**: with Semi-Auto chosen on the API Key page, Flow's orders wait
  in the **Action Center** for your approval, and the node's result is the
  queue's answer rather than a broker order id.
- **Order tag**: Flow orders carry the workflow's name as their strategy tag
  (a `strategyTag` on the node, set in the JSON, overrides it), which is what
  **Strategy P&L** reads. An imported workflow's name ends in `(imported)`:
  rename it if the tag matters to you.
- The order alerts (Telegram, WhatsApp, toasts) fire for Flow orders like any
  other.

## Import and export: moving workflows to and from OpenAlgo web

- **Export Workflow** (the editor's **...** menu) downloads the last saved
  version as a `.json` file. It never contains your webhook address, secret or
  API key.
- **Import** (the Flow page) takes a file or pasted JSON. The workflow is
  checked as Activate checks it, and a problem is named before anything is
  saved. It becomes a new, inactive workflow called `<name> (imported)` with
  its own new webhook address and secret, the webhook switched off.
- **Replace from JSON** (the editor's **...** menu) swaps the nodes and
  connections of this workflow for the file's and keeps its webhook address,
  secret, active state and history, so alerts pointing at it keep working. If
  the new trigger cannot start, the workflow is deactivated and the message
  says so.

A workflow exported from OpenAlgo web imports unchanged, and a desktop export
imports into OpenAlgo web. A few things behave differently in the desktop:

| | OpenAlgo web | OpenAlgo Desktop |
| --- | --- | --- |
| Times (schedules, built-in variables, Wait Until, time conditions) | The server's clock | India time (IST) on any computer |
| **Daily** schedule with days ticked | Runs every day | Runs on the ticked days |
| **Once** schedule | At midnight of the date | At the date and the time you set |
| Webhook without a secret | Runs on the address alone | Every webhook has and needs a secret |
| API key | Stored with the workflow when activated | The app's current key, never stored |
| Subscribe LTP output | `{{ltp.ltp}}` | `{{ltp}}`, as documented |
| Close Positions for one symbol, live | Closes every position | Closes that position only |
| HTTP Request | Public addresses | Public addresses, also not the Tailscale range; 5 MB at most |

The complete JSON format, every node's fields and these differences in detail
are in the [Flow import format](../../prompt/flow-import-format.md).

## Building workflows with the Agent

The Agent (profile menu, **Agent**) can write a workflow for you. Describe it:
"every 5 minutes in market hours, buy one lot of the NIFTY ATM call when RSI
on NIFTY crosses above 30, and send me a Telegram alert". The Agent:

1. reads the Flow format (its **flow-builder** skill) and writes the JSON;
2. checks it with **validate_flow**, the same check Import runs, and corrects
   it until it passes;
3. saves it with **save_flow**, which waits for your approval on a card that
   shows the workflow's name, nodes and connections.

The workflow is saved **inactive**, under its name, with a new webhook
address. The Agent never activates, runs or changes a workflow's webhook: open
it on the Flow page, read it, run it in analyzer mode, and activate it
yourself. The Agent needs a model set up in **Admin**, **Agent Config**; the
Flow tools are in the Agent chat, not in the trading terminal's Assistant.

## Keyboard shortcuts

| Keys | Does |
| --- | --- |
| Ctrl+S (Cmd+S) | Save |
| Delete or Backspace | Delete the selected node or connection |
| Escape | Clear the selection |
| ? | Open the shortcuts page |
| Scroll, Ctrl+Scroll | Zoom |
| Drag the canvas | Pan |

The full list is under **...**, **Keyboard Shortcuts**.

## Three starting points

**A TradingView alert to an order.** Webhook, then Place Order with Symbol
`{{webhook.symbol}}`, Action `{{webhook.action}}`, Quantity
`{{webhook.quantity}}`, Exchange NSE, Product MIS, then Telegram "Placed
{{webhook.action}} {{webhook.symbol}}: {{order.orderid}}" (with the Place
Order's output variable set to `order`).

**RSI entry on a schedule.** Schedule (Interval, 5 minutes, Market hours
only), Indicator (RELIANCE, interval D, rsi, `{"period": 14}`, output `rsi`),
Var Condition (`{{rsi.latest.value}}` `<` `30`), true branch to Position Check
(RELIANCE, not exists), true branch to Place Order. The position check stops a
second entry on the next run.

**Square-off before the close.** Schedule (Daily, 15:10, weekdays), Close
Positions (no symbol: everything), Telegram "Squared off at {{time}}".

## Troubleshooting

| Problem | Check |
| --- | --- |
| Activate says a field is missing | The message names the node and field; open the node and fill it |
| The workflow never runs on schedule | Active? **Market hours only** on outside the session? Signed in? The app open at that time? |
| Orders fail, saying no broker is logged in | Log in to your broker (again after 03:00 IST) from **Select Broker** |
| TradingView alerts do nothing | Webhook switched on, workflow active, the tunnel online, the address copied after the tunnel came online, `secret` in the message |
| "Invalid webhook secret" | The secret in the alert differs from the dialog's; after several, the webhook locks for 15 minutes |
| A condition takes neither branch | Its data did not arrive or a variable did not resolve: read the Execution Log |
| `{{name.field}}` shows up as text | The earlier node's **Output Variable** is not `name`, or the field is spelled differently |
| An order went to the sandbox | Analyzer mode is on: the editor header shows it |
| The Telegram node failed | Start the bot and link your account ([chapter 23](../23-telegram-bot/README.md)) |
| Import refused a file | The message names the first problem; files from OpenAlgo web import as they are |

## In the desktop

- Flow is native: no Python, and the workflow format is OpenAlgo web's.
- Every time is IST, on any computer.
- Schedules, price alerts and order updates run only while the app is open and
  you are signed in; a run skipped while signed out is recorded with its
  reason, and nothing is deactivated by a sign-out. With no broker logged in,
  workflows still run, as on OpenAlgo web: live broker calls fail per node.
- Every webhook needs its secret, wrong secrets lock it for a while, and the
  addresses follow the built-in tunnel.
- The API key is never stored with a workflow or written into an export.
- Delete and Regenerate ask first; leaving the editor with unsaved changes
  asks first.
- OpenAlgo web's operator scripts (`update_flow_workflow.py`,
  `flow_diagnose.py`) are not part of the desktop: use **Replace from JSON**,
  **Run Now** and the Execution Log.
- Flow's settings that OpenAlgo web reads from `.env` (history cache, history
  bars, run history, webhook limits) are fixed at the web's defaults.

---

**Previous**: [20 - Python Strategies](../20-python-strategies/README.md)

**Next**: [22 - Action Center](../22-action-center/README.md)
