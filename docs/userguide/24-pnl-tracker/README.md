# 24 - PnL Tracker

## Introduction

The PnL Tracker draws today's mark-to-market (MTM) of your whole account,
minute by minute, with its high, its low and the drawdown from the high. Use
it to see how the day went, not only where it ended.

Open **PnL Tracker** from the profile menu (`/pnl-tracker`).

It is an intraday view: it covers today's trades and the positions you hold
now. For the sandbox's day-by-day history, see **My P&L History** on the
Sandbox page ([Sandbox mode](../../user/sandbox.md#where-to-find-it)).

## What it shows

Four cards across the top:

| Card | Meaning |
| --- | --- |
| **Current MTM** | Today's realized plus unrealized P&L, now. The percentage under it is the MTM as a share of your capital: in analyzer mode the sandbox capital (the starting capital on the Sandbox page, 1 crore unless you changed it); live, your account margin as the Dashboard shows it, Available Balance plus Utilised. When your funds cannot be read, no percentage is shown. (OpenAlgo web divides by a fixed 1,00,000.) |
| **Max MTM** | The highest MTM of the day, with the time it was reached |
| **Min MTM** | The lowest MTM of the day, with its time |
| **Max Drawdown** | The largest fall from a high to a later low ("Peak to trough") |

Below them, the **Intraday PnL Curve** plots the MTM through the day, with the
drawdown underneath.

## How it is worked out

When you open the page or choose **Refresh**, OpenAlgo:

1. reads today's trade book and your current positions, including positions
   carried from earlier days;
2. fetches 1-minute candles for each symbol involved;
3. replays the trades minute by minute against those candles, adding the
   realized P&L of closed trades and marking open quantity at each minute's
   close.

This is the same calculation as OpenAlgo web's tracker. Because it fetches
history for every symbol you traded, a busy day takes a few seconds to load,
and the page does not update by itself: choose **Refresh** for the latest.

P&L is before brokerage, taxes and other charges.

## Live and analyzer mode

The tracker follows the mode switch. In **Live Mode** it reads your broker's
trade book and positions; in **Analyze Mode** it reads the sandbox's, so you
can watch a sandbox strategy's day the same way.

## Saving a picture

**Screenshot** saves the cards and the chart as a PNG file named
`PnL_Tracker_<date and time>.png`.

## What it needs

- A broker logged in. Without one the page cannot load ("Authentication
  required").
- An API key (one is created with your account).
- Market data history from your broker for the symbols you traded. If a
  symbol's history cannot be fetched, the curve for that symbol is missing
  and the log says so.

## Troubleshooting

| What you see | Cause and fix |
| --- | --- |
| "Failed to load PnL data" | No broker session (log in again after 03:00 IST), or the broker refused the trade book. Try **Refresh**; check **Logs**, **Live Logs**. |
| The curve is flat or empty | No trades today and no open positions; or you are in the other mode from the one you traded in. |
| The curve stops before now | The broker had no recent 1-minute candles yet. Refresh in a minute. |
| Numbers differ from the broker's P&L | The tracker is before charges and marks at 1-minute closes. |

---

**Previous**: [23 - Telegram Bot](../23-telegram-bot/README.md)

**Next**: [25 - Latency Monitor](../25-latency-monitor/README.md)
