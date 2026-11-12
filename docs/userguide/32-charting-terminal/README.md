# 32 - Charting Terminal

## Introduction

The charting terminal at `/trading` is where you read a chart and trade from
it. It uses `openalgo-charts` 2.7.2, the same charting engine as OpenAlgo web:
19 chart types, 112 built-in indicators plus your own, and 90 drawing tools,
on the same broker session and market data feed as the rest of the app. It
also runs OpenScript studies, backtests and live strategies.

Open it with **Trading** in the top bar, or `http://127.0.0.1:5000/trading`.
It is a full-screen terminal: the app's navigation bar is replaced by the
terminal's own toolbar, which still carries the broker, the **Live** or
**Analyze** mode switch, the theme and the profile menu.

The chart loads history with your API key. If there is none, the page says so
and links to the API Key page.

The **Assistant** on the right rail is the Agent's chat about the chart in
front of you; see [The Chart Assistant](#the-chart-assistant).

## The layout

| Region | What it holds |
| --- | --- |
| Top toolbar | Symbol, interval, chart type, product, quantity, indicators, comparisons, templates, layout, chart sync, workspaces, **One-Click**, replay, undo and redo, full screen, camera and the account controls |
| Left rail | Drawing tools in groups, magnet, keep tool selected, undo, redo, delete |
| Centre | The chart grid: **Single**, **2 columns**, **2 rows**, **1 + 2**, **2 × 2**, **3 × 2** or **4 × 2** (up to eight charts) |
| Right panel | **Watchlist**, **Option chain**, **Objects**, **Alerts**, **Data window**, **Scripts**, **Backtest**, **Strategies** and **Assistant**, opened from the right rail |
| Bottom dock | **Orders**, **Positions**, **Trades** and **GTT** across every symbol |

One toolbar serves the whole grid. Click a chart (or use the chart selector)
to choose which one the toolbar controls; it gets a highlighted border. Each
chart keeps its own symbol, interval, chart type, studies, alerts,
comparisons and quantity. Double-click a chart to maximize it; double-click
again to put the grid back.

**Quick entry**: with a chart focused, type a letter to open the symbol search
or a digit to open the interval box, without reaching for the mouse.

## Intervals, including custom ones

The interval menu groups seconds, minutes, hours and calendar periods. Choose
**Add custom interval**, pick the unit and type a whole number. Star an
interval to keep it as a favourite; favourites are shared by every chart and
by Historify.

Custom intervals are built from history your broker really has: 7 minutes
from 1-minute candles, 2 hours from hourly candles, multi-day, weekly,
monthly, quarterly and yearly candles from daily ones. Candles follow the
exchange sessions and holiday calendar. An interval your broker cannot supply
is disabled. Historify's charts and exports use the same custom intervals;
there a 2-week, 7-month or 3-quarter candle runs across the turn of the year
instead of restarting at 31 December.

## Studies on a higher timeframe

Many built-in studies (moving averages, bands and channels, Supertrend,
Parabolic SAR, ATR, RSI, MACD, Stochastic, CCI, ADX and others) have a
**Timeframe** setting. Left at the chart interval, the study draws as before.
Set it to Daily, for example an EMA of 200 on a 5-minute chart, and the study
fetches its own daily history, including the 200 days it needs to warm up,
independently of how much of the 5-minute chart is loaded. Historify charts do
the same with downloaded daily data.

Only confirmed values are drawn: during the session a daily study uses
completed days. If its history cannot be fetched, the study says so instead
of drawing a made-up line.

## One-Click trading

**Understand this switch before you trade from the chart.**

Each chart has a SELL and BUY panel in its top-left corner, and the
right-click menu has market, limit and stop orders. What they do depends on
**One-Click** in the toolbar:

- **Off** (the default): a click runs every check, then opens the order
  ticket filled in with exactly what would be sent. You see the order before
  it goes. Hint: "Buy and Sell on the chart open the order ticket first".
- **On** (the control turns red): a click sends the order at once, with no
  ticket. Hint: "Buy and Sell on the chart send a live order at once".

The switch applies to every chart in the grid and is remembered; if the
saved state cannot be read, it starts **off**.

Orders from the chart are tagged with the strategy name `chart-trading` and
follow Live or Analyze mode like every other order. They are sent with your
API key, so with **Semi-Auto** on they wait in the
[Action Center](../22-action-center/README.md) (the chart then reports
"placeorder returned no orderid"), and dragging or cancelling an order line on
the chart is refused in Semi-Auto. Use the dock for those.

Closing a position, cancelling and modifying work whether One-Click is on or
off: turning it off never takes away your exit.

## The bottom dock

The dock under the charts has a tab per book, each with a count: **Orders**
(today's order book), **Positions**, **Trades** and **GTT**. The header shows
the running open P&L and the number of working orders. Drag its top edge to
resize it.

From a row you can cancel or modify a working order, close one position (that
row only), or click the row to chart its symbol. **Cancel all** and **Close
all** sit in the header behind a confirmation. Rows update from your broker's
order stream and are checked against the order book shortly after.

The dock's cancel, modify and close are the app acting for you, so Semi-Auto
does not hold them.

## Drawing tools

All 90 drawing tools are on the left rail: lines and rays, channels and
pitchforks, Fibonacci and Gann tools, patterns (XABCD, head and shoulders,
harmonics) and Elliott waves, shapes, text and notes, and the volume studies
**Anchored VWAP** and **Fixed Range Volume Profile**. A group button reuses
the tool you last picked from it; the small corner opens the whole group.
Each tool's icon shows its drawing with an open ring at every point you
place.

The **Elliott waves** section of the Patterns group holds five counts:
impulse (1 2 3 4 5), correction (A B C), triangle (A B C D E), double
combination (W X Y) and triple combination (W X Y X Z). Click the origin
first, point 0, where the first wave starts, then the end of each wave. The
origin has no label; the waves are numbered from the second point, in
parentheses, each label centred over its point, and the count is drawn as
you place it. Counts are labelled in the Intermediate degree; there is no
control yet for another degree or label style. An impulse or correction
drawn in OpenAlgo Desktop 1.0.7 or earlier has its origin on the end of its
first wave; drag that point away to draw the first wave.

- **Magnet** snaps an anchor to the nearest open, high, low or close.
- **Keep tool selected** (the padlock) keeps the tool after each drawing.
- Select a drawing for a bar with colour, width, dash, lock and delete.
  Right-click it for copy, cut, paste and delete.

Drawings are saved per chart and survive a restart.

## Chart alerts

**Right-click the chart at a price** and choose **Create price alert at ...**:
the alert is made there and then. It fires once, the moment the price is
touched (**Intrabar touch**), expires in two months, and tells you with a
sound and a desktop notification. Right-click a study line or a drawing for
**Create study alert** or **Create drawing alert** instead. For a condition,
a different trigger or a message, use **Alerts** on the chart's toolbar,
which opens the full form; there **Evaluate** can be **On bar close**
instead.

Under **When it fires** you choose how you are told:

| Channel | Default | Reaches you |
| --- | --- | --- |
| Sound | on | A short tone from the app |
| Desktop notification | on | Your system's notification, shown only while the terminal is hidden |
| Telegram | off | A message through your Telegram bot; it must be running with your account linked ([23 - Telegram Bot](../23-telegram-bot/README.md)) |
| WhatsApp | off | A message through your paired WhatsApp ([23 - Telegram Bot](../23-telegram-bot/README.md#whatsapp)) |

A message may carry placeholders such as `{{ticker}}`, `{{price}}`,
`{{close}}` and `{{interval}}`; the form lists them.

The **Alerts** panel on the right rail lists your alerts, with a **Log** tab of
what fired. The log is kept in OpenAlgo's database for 90 days, so it survives
a restart. Alerts are checked by the open terminal: they fire only while
`/trading` is open in OpenAlgo Desktop (or a browser tab), and they never place
orders.

## Market replay

Click **Replay** and choose a starting bar to step through history. **Selected
chart** replays one chart; **All charts** replays the grid on one clock. While
replay is selecting, loading or playing, **no order can leave the terminal**:
the chart panels, the dock and the GTT tab all refuse, because the prices on
screen are not the market's.

## Workspaces, templates and comparisons

- **Workspaces**: save the whole grid under a name (**Save as**), open it
  again later, turn on **Autosave chart changes**, and move a workspace
  between computers with **Export JSON** and **Import workspace JSON**.
- **Templates**: **Save current studies** as a template and apply it to
  another chart with **Replace studies** or **Add studies**.
- **Compare**: **Add comparison** overlays other symbols, as prices or as
  percentages.
- The camera menu downloads or copies an image of the chart, or downloads its
  data as CSV.

## Keyboard shortcuts

Chart shortcuts act on the chart under the pointer or the focused chart. A
key typed into a field, a dialog or the order ticket belongs to that field.
`Mod` is Ctrl, or Cmd on a Mac.

### Navigation

| Key | Action |
| --- | --- |
| Left, Right | Pan |
| `Mod` + Left, Right | Pan faster |
| Up, Down | Pan vertically |
| `=`, `Shift` + `=`, Numpad `+` | Zoom in |
| `-`, Numpad `-` | Zoom out |
| `Home` or `0` | Reset the view |
| `Alt` + `F` | Fit every loaded bar |
| `Alt` + `M` | Crosshair magnet on or off |
| `Alt` + `Shift` + `S` | Save a chart image |

### Price scale

| Key | Action |
| --- | --- |
| `Alt` + `A` | Auto-fit the price scale |
| `Alt` + `I` | Invert the price scale |
| `Alt` + `R` | Reset the price scale |
| `Alt` + `L` | Logarithmic scale on or off |
| `Alt` + `P` | Percent scale on or off |
| `Alt` + `1` | Scale indexed to 100 on or off |

### Drawing tools

| Key | Selects |
| --- | --- |
| `Alt` + `T` | Trend line |
| `Alt` + `H` | Horizontal line |
| `Alt` + `J` | Horizontal ray |
| `Alt` + `V` | Vertical line |
| `Alt` + `C` | Cross line |

In the desktop (as in current OpenAlgo web), `Alt` + `V` and `Alt` + `H`
select the line tools only; the grid is toggled from the toolbar or the
right-click menu.

### Editing a drawing

| Key | Action |
| --- | --- |
| `Delete` or `Backspace` | Delete the selected drawing, or the one under the pointer; an alert line is removed only when no drawing is there |
| `Mod` + `Z`; `Mod` + `Shift` + `Z` or `Mod` + `Y` | Undo; redo |
| `Mod` + `C`, `Mod` + `X`, `Mod` + `V` | Copy, cut, paste a drawing |
| `Mod` + `D` | Duplicate the selection |
| Arrow keys, `Shift` + arrow keys | Nudge the selection 1 or 10 pixels |
| `Esc` | Release the tool, then close the open panel |
| `Enter`, `Backspace` (while placing a multi-point tool) | Finish the shape, drop the last point |

No key places, modifies or cancels an order on the chart: order entry here is
deliberately by pointer. For keyboard-driven order entry use the scalping
terminal (**Scalping** in the profile menu).

## OpenScript

OpenScript is OpenAlgo's scripting language for studies and strategies. In the
desktop it runs on the same engine as OpenAlgo web, with no Python.

- **Scripts** (right rail): write, open and save scripts. A **study** plots on
  the chart and places no orders; a **strategy** has entries and exits.
  Errors are listed with their line. Scripts are saved as `.oscript` files in
  the `openscript` folder of your data folder.
- **Backtest**: run a saved strategy over history on the selected chart, up
  to 100,000 bars. It runs in the background so the charts keep drawing; the
  result shows trades, statistics and markers on the chart.
- **Strategies**: deploy a saved strategy on an instrument, exchange and
  interval. Each deployment is its own row with its own position and log, so
  one strategy can run on several instruments at once.
  - The header says where orders go: "Orders from a running strategy go to
    your broker" in Live mode, "... go to the sandbox" in Analyze mode. The
    start button says it too: **Start live** or **Start in sandbox**. A run
    keeps the destination it started with.
  - **Pause** stops new signals and keeps the position. **Stop** closes the
    position with a market order after a confirmation that names what will be
    closed; it cannot be undone.
  - A Live run belongs to the broker account it started on. Its orders are
    never sent through another broker or account, and what it holds elsewhere
    is shown on its row.
  - With Semi-Auto on, a Live run's orders wait in the Action Center and the
    run follows each one ([22 - Action Center](../22-action-center/README.md#semi-auto-with-an-openscript-live-run)).

In the desktop, live runs are carried out by OpenAlgo itself, not by the page,
so closing the terminal does not stop them; quitting OpenAlgo does. After a
restart, OpenScript runs stay paused until you start them again (and Live
runs need you to sign in and log in to your broker first).

Custom chart indicators written in JavaScript load from the `indicators`
folder of your data folder.

## The Chart Assistant

**Assistant**, last on the right rail, opens a chat that reads the chart you
are looking at: its symbol, interval, chart type, visible bars, indicators and
your own drawings. It can mark the chart up with levels, trend lines and
zones, find patterns, and add or remove indicators. It never changes the
chart's symbol, and its markup lives in its own groups, so clearing what it
drew never removes a drawing of yours. The chips above the box fill in a
question; they do not send it.

**Orders.** It proposes orders only when **Allow the agent to place, modify
and cancel orders** is on in **Admin**, **Agent Config**, the same switch the
`/agent` page uses. Every order it proposes appears as an approval card that
states the order, and nothing is sent until you choose **Approve**; the
Trading safety limits and the kill switch on Agent Config still apply after
that. In analyzer mode the order goes to the sandbox, and in Semi-Auto it
waits in the Action Center.

**OpenScript.** Ask it to write a study or strategy and it shows you the
source, then saves it to your scripts once you approve, never over a compiled
script. It is saved as source only: open it in **Scripts**, where it compiles
and plots, and save it there once the console is clean before you run it as
a strategy.

It needs a model set up first, in **Admin**, **Agent Config**.

## What is remembered

The grid layout, chart sync, the open right panel and its width, the dock's
tab and height, One-Click, and per chart the symbol, interval, chart type,
product, studies, comparisons, drawings and settings are kept in the app
window's storage on this computer. A browser pointed at the same address keeps
its own copy. Watchlists, OpenScript files, alert logs and deployments are
kept by OpenAlgo itself.

## Related

- [11 - Order Types](../11-order-types/README.md)
- [15 - Analyzer Mode](../15-analyzer-mode/README.md)
- [22 - Action Center](../22-action-center/README.md)
- [31 - Tools](../31-tools/README.md)

---

**Previous**: [31 - Tools](../31-tools/README.md)

**Return to**: [User Guide](../README.md)
