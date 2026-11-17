# 25 - Latency Monitor

## Introduction

The Latency Monitor shows how long each API call and each order took inside
OpenAlgo Desktop and at your broker. Use it to see whether a slow fill came
from your broker, from OpenAlgo, or from somewhere before the request reached
OpenAlgo.

Open **Logs**, then **Latency Monitor** (`/logs/latency`). It is also on the
**Admin** page as **Latency Dashboard**. The page title is **Latency
Monitor**.

## What is measured

Every call to `/api/v1` is recorded: orders from your programs, alerts and AI
clients, and data calls such as quotes and history. Orders you send from the
scalping terminal, and the close-position and close-all buttons in the app,
are recorded too, as on the web.

What the time covers: from the moment the request reaches OpenAlgo to the
moment the answer leaves it. It does not include the time the request took
to reach your computer (from TradingView's servers through a tunnel, for
example), or the time after the broker accepted the order (the exchange
match).

For an order, the time is split in two:

| Part | Meaning |
| --- | --- |
| **Broker API Call** (broker confirmation) | Time spent waiting for your broker's servers: network round trip, the broker's checks and its hand-off to the exchange |
| **Platform Processing** (platform overhead) | The rest: API key check, validation, symbol lookup, logging and the answer |

In analyzer mode an order never reaches the broker, so its time is all
platform processing. For a data call, the handler's time is shown as the
broker part.

## Choose a group

The buttons at the top pick which calls the page describes, and each shows
how many calls it holds. The page opens on **Orders**.

| Group | What it covers | Kept for |
| --- | --- | --- |
| Orders | Placing, changing and cancelling orders, closing positions and GTT orders | All time (newest 200,000) |
| Account and books | Order status, positions, holdings, funds, margin, and the order, trade and GTT books | 7 days |
| Market data | Quotes, depth, price history and the ticker | 7 days |
| Options | Option chain, Greeks, option symbol and synthetic future lookups | 7 days |
| Symbols and settings | Symbol search, expiry dates, the market calendar, analyzer mode, chart settings and ping, answered from OpenAlgo's own data | 7 days |
| Tools and alerts | Sector rotation, the strategy module and alert messages (Telegram, WhatsApp, Slack, Discord) | 7 days |
| Other | A call in none of the groups above; shown only when there is one | 7 days |
| All calls | Everything together | As above |

Every figure on the page is for the chosen group only. A chart or a scanner
makes hundreds of history calls for each order, and a history call returns a
large answer, so one average across both would describe neither. Read
**Orders** for order speed. **All calls** is there for the total, not for a
figure to act on.

## The page

- **Orders Tracked** (or **Calls Tracked**): every recorded call of the
  group, all time.
- **Success Rate**: calls that did not end in an error, with the number that
  failed.
- **Average Confirmation Time**: the average total time.
- **Fast Orders**: the share of orders under 150 ms (the target is 95%).
  Other groups show the median (P50) here instead.
- **Performance Levels** and **Latency Distribution**: Excellent under
  150 ms, Good 150 to 250 ms, Acceptable 250 to 400 ms, Slow over 400 ms,
  counted over every recorded order. Shown for **Orders** only: they
  describe an order confirmation, and a history download that takes a
  second is not a slow order.
- **By Endpoint**: each endpoint of the group on its own, with how many
  calls, how many failed, the average, the median (P50), the slowest 1%
  (P99), and how the average splits between the broker and OpenAlgo.
- **Broker Performance Comparison**: per broker, the average, the median
  (P50), the slowest 1% (P99), the share of fast orders (for Orders) and
  the count, for the group's calls.
- **Recent Orders** (or **Recent Calls**): the latest 100 calls of the
  group with time (IST), order id, broker, symbol, endpoint, latency and
  status. Other calls never push the orders out of this list. The button at
  the end of a row opens the breakdown with the two parts above.

Medians and the slowest 1% cover the last 30 days; counts and averages cover
everything kept. The page refreshes every 30 seconds; **Refresh** reloads it
at once.

## Export

**Export to CSV** downloads the calls of the group on screen, newest first,
as `latency_<group>.csv` (for example `latency_orders.csv`), or every call as
`latency_logs.csv` for **All calls**. The columns:

| Column | Content |
| --- | --- |
| Date & Time (IST) | When the call arrived |
| Broker | The broker logged in at the time |
| Order ID | The order id, or the request id, or `unknown` |
| Symbol | The symbol in the request, when there was one |
| Order Type | The endpoint's tag: PLACE, SMART, MODIFY, CANCEL, QUOTES, HISTORY and so on |
| Broker Confirmation (ms) | The broker part |
| Platform Overhead (ms) | The OpenAlgo part |
| Total Latency (ms) | Both |
| Status | SUCCESS or FAILED |
| Error (if any) | The error message for a failed call |

In the app window the file is saved like any download; in a browser it is an
ordinary download. The file is built a page at a time, so a large log
downloads without holding up trading.

## What is kept

The latency log lives in `logs.db` in your data folder.

- **Orders** (place, smart, modify, cancel, close, cancel all, basket, split,
  options, multi-leg and GTT) are kept up to the newest 200,000 rows.
- **Every other call** is kept for 7 days.

Old rows are removed every ten minutes. Nothing here holds a request body or
your API key: only the symbol from the request and the order id or message
from the answer.

## Reading the numbers

- **Broker part high**: your broker or your internet connection is slow.
  Compare with other times of day, and with the
  [Health page and diagnostics](../29-troubleshooting/README.md#check-the-health-page-and-run-diagnostics).
- **Platform part high**: the computer is busy (another heavy program, or the
  computer just woke from sleep). One slow call after a long idle period is
  normal.
- **Fills arrive late but latency is low**: the delay is before OpenAlgo (the
  alert service, the tunnel) or after the broker (the exchange). This page
  cannot see either.

---

**Previous**: [24 - PnL Tracker](../24-pnl-tracker/README.md)

**Next**: [26 - Traffic Logs](../26-traffic-logs/README.md)
