# 15 - Analyzer Mode (Sandbox)

## Overview

**Analyzer mode** sends every order and account request to the **sandbox**, a
simulated account, instead of your broker. Use it to check that a strategy, a
webhook or an API client sends what you expect before you trade live.

- The sandbox starts with **1 crore** of simulated capital.
- Orders fill from your **broker's live prices**, so a broker must be logged
  in, with market data for the instruments you test.
- Its orders, trades, positions, holdings and funds are kept in `sandbox.db`,
  apart from live trading.
- The API answers in analyzer mode exactly as OpenAlgo web's does, with
  `"mode": "analyze"` in each answer, so a client can be tested unchanged.

The sandbox is an order and account simulator. It is not an exchange
simulator and not a backtester: its results check your order flow; they do
not forecast live results.

The full description of fills, limits and times is
[Sandbox mode (analyzer mode)](../../user/sandbox.md). This chapter is the
guided tour.

## Turning it on and off

**In the app**: the mode switch in the top bar, next to the badge.

| Badge | Meaning |
| --- | --- |
| **Live Mode** | Orders go to your broker |
| **Analyze Mode** (violet) | Orders go to the sandbox |

Switching shows "Switched to Analyze mode" (or Live), then a reminder that
analyzer mode is for testing. The whole interface turns violet, and the theme
switch is disabled until you return to Live.

**From the API**:

```http
POST http://127.0.0.1:5000/api/v1/analyzer
POST http://127.0.0.1:5000/api/v1/analyzer/toggle
```

```json
{"apikey": "<APIKEY>", "mode": true}
```

`/analyzer` reads the status; `/analyzer/toggle` sets it (`true` for analyzer
mode, `false` for live). The answer:

```json
{
  "status": "success",
  "data": {"analyze_mode": true, "mode": "analyze", "message": "Analyzer mode switched to analyze", "total_logs": 12}
}
```

What to know about the switch:

- **It is one switch for everything**: the app, the API, webhooks and AI
  clients. It is not per API key or per strategy.
- **It is remembered** across restarts.
- **Positions already at your broker stay there.** A Live strategy run,
  OpenScript run or scalping stop that holds a position at your broker keeps
  managing it there after you switch: its exits are not sent to the sandbox.
- Always read the status before an automated test, and again before going
  live.

## What changes in analyzer mode

| Area | In analyzer mode |
| --- | --- |
| Orders (regular, smart, basket, split, options, GTT) | Placed in the sandbox |
| Modify, cancel, cancel all, close position | Act on sandbox orders and positions |
| Order book, trade book, positions, holdings, funds | Read from the sandbox |
| Dashboard funds | The sandbox account |
| Quotes, depth, history, option chains | Still from your broker (live data) |
| Logs | Every request listed in **Logs**, **Sandbox Logs** |

## How orders fill

| Type | Fill |
| --- | --- |
| MARKET | At the ask when buying, the bid when selling, or the last price if the quote has no bid or ask |
| LIMIT already marketable | At the last traded price |
| LIMIT resting | At your limit price, once the market trades through it |
| SL, SL-M | When the trigger trades, at the last traded price (an SL only within its limit) |

Prices come from the live feed. When a symbol's feed has been quiet for 30
seconds, the sandbox asks your broker for a quote every few seconds (the
**Order Check Interval** setting). With no usable price (none, one outside the
day's high and low, or one older than 30 seconds), orders wait instead of
filling.

## Margin

Margin is a simple leverage rule, not your broker's SPAN and exposure margin:

```
margin = quantity x price / leverage
```

It is blocked when the order is placed, netted against an opposite position
(an exit blocks nothing while the position covers it), and released on cancel,
reduce and close.

Default leverage, changeable on the **Sandbox** page:

| Segment | Default |
| --- | --- |
| Equity MIS | 5x |
| Equity CNC | 1x |
| Futures (all exchanges) | 10x |
| Option buying | 1x (full premium) |
| Option selling | 1x |

If your sandbox funds cannot cover an order, it is rejected with "Insufficient
funds. Required: ..., Available: ..., Shortage: ...".

### Where the desktop matches the web, and where it does not

The sandbox follows OpenAlgo web's `sandbox` package. A few cases are worth
knowing:

| Case | OpenAlgo web | OpenAlgo Desktop |
| --- | --- | --- |
| Modifying a pending order to a larger quantity or a higher price | No margin change at the modify; the position later holds only the margin blocked at placement | **Same at the modify**: it blocks or releases nothing. In the desktop, when the order later fills and needs more margin than it holds, the difference is blocked at the fill, and the fill is rejected with "Insufficient funds" if your funds cannot cover it |
| An exit that fills after another order already closed the position | The new position holds no margin | The new position's margin is blocked at the fill (rejected if funds cannot cover it) |
| Margin of resting orders after another order fills | Dropped from used margin by the web's reconciliation, and released again on cancel | Kept until the order fills or is cancelled |
| A fill that reverses a position | Keeps only a fraction of the new position's margin | Keeps the margin for the whole new position |
| Cancelling a resting order | Releases exactly what it reserved | Same |
| **Cancel all** with SL or SL-M orders waiting for their trigger | They are not cancelled | They are cancelled |
| A sandbox GTT modify that fails | Answers with a server error | Answers with the sandbox's own reason |
| Fractional quantities | Accepted on crypto | Whole numbers only in the sandbox: "Fractional quantities are not supported in OpenAlgo Desktop yet. Use a whole-number quantity." |

The first row is deliberate parity: a modify behaves as on the web. The other
differences are places where the web's sandbox loses track of margin or
skips an order; the desktop keeps its funds equal to what its own books hold.

## Brokerage and charges

Every sandbox trade pays brokerage and statutory charges, so sandbox results
come close to what the same trades cost in a live account. The charges are
taken from the available balance when the order fills. P&L is shown before
charges, with the charges beside it, as a broker's position book shows it.

| Page | What it is for |
| --- | --- |
| **Sandbox Charges** (`/sandbox/commissions`) | Set your broker's brokerage for each segment, switch charges on or off, and see what trades have cost (**Transaction Costs**, with a CSV export) |
| **Brokerage Calculator** (`/sandbox/calculator`) | Work out what a buy and a sell cost before placing them: brokerage, every statutory line, points to breakeven and net P&L |
| **My P&L** (`/sandbox/mypnl`) | Today's and lifetime charges beside the P&L, and a charges column per day and per trade |

Open them from the **Sandbox** page (**Charges**, **Calculator**) or from
**My P&L** (**Transaction Costs**).

Brokerage is two numbers per segment: an amount per executed order and a
percentage of the trade value. With both set, the lower of the two is charged
on each order. Set either to 0 to leave it out.

| Segment | Default brokerage |
| --- | --- |
| Equity delivery | 20 per order or 2.5% of the trade value, whichever is lower |
| Equity intraday | 20 per order or 0.1%, whichever is lower |
| Futures (equity, currency, commodity) | 20 per order or 0.03%, whichever is lower |
| Options (equity, currency, commodity) | 20 per order |
| Crypto | Nothing until you enter your venue's fee |

STT or CTT, exchange transaction charges, SEBI fees, stamp duty and GST are
added at the published rates. They are the same at every broker and cannot
be changed. Logged in to a crypto venue, you see the crypto segments only,
as a venue fee and a **Fee Calculator**.

Not charged: depository (DP) charges on a delivery sell, and STT on an option
that is held to expiry and exercised. Trades made before charges were
introduced are not charged after the fact. The same figures for one order,
before it is placed, come from `POST /api/v1/charges`
([Charges](../../api/account-services/charges.md)).

## Times and settlement

| What | When |
| --- | --- |
| MIS square-off | NSE, BSE, NFO, BFO 15:15 IST; CDS, BCD 16:45; MCX 23:30; NCDEX 17:00 (changeable). Crypto has none |
| CNC buys to holdings | 00:00 IST the next calendar day, weekends and holidays included |
| Today's realized P&L reset | 03:00 IST |
| Daily P&L snapshot (My P&L) | 23:59 IST on trading days, and every day you held or traded crypto |
| Automatic fund reset | Never, unless you choose a day on the Sandbox page |

**If the app was closed** across one of these times, the sandbox catches up
when analyzer mode next runs: stale MIS positions are settled at their last
price, expired F&O contracts are settled, CNC buys move to holdings, a missing
daily snapshot is filled in, and waiting GTT triggers are checked.

## The Sandbox page

Profile menu, **Sandbox** (titled **Sandbox Configuration**):

| Setting | Default |
| --- | --- |
| Starting capital | 1,00,00,000 (1 crore) |
| Reset day / reset time | Never / 00:00 IST |
| Order check interval | 5 seconds (1 to 30) |
| MTM update interval | 5 seconds (0 to 60; 0 = manual) |
| NSE/BSE, CDS/BCD, MCX, NCDEX square-off | 15:15, 16:45, 23:30, 17:00 IST |
| Leverage | As in the table above (1x to 50x) |
| Expired F&O settlement | At the exchange close on expiry day |
| Expired option price | Last traded price (keeps in-the-money value) |

The page also shows the **Sandbox model** box (what the simulation leaves
out), **My P&L** (daily sandbox P&L history), **Charges** (the brokerage plan
and the transaction cost report), **Calculator** (the brokerage calculator)
and **Reset to Defaults**.

**Reset to Defaults** asks for confirmation, then deletes sandbox orders,
trades, positions, holdings, GTT orders and P&L history, restores the
starting capital and puts every setting back to its default, the brokerage
plan included. It cannot be undone, and it touches nothing at your broker.

## Sandbox Logs

**Logs**, then **Sandbox Logs** opens the **Sandbox Request Monitor**: every
request made while analyzer mode was on, with its answer. Use it to see
exactly what a webhook or script sent.

## Limits of the simulation

- Orders fill in full or not at all: no partial fills, queue position or
  market impact.
- P&L is shown before charges: brokerage and statutory charges are taken
  from the available balance at each fill and shown beside it. Depository
  charges and STT on an option exercised at expiry are not charged.
- Margin is a leverage rule, not your broker's margin.
- Capital is in rupees and crypto prices in US dollars, without conversion.
- Fills depend on your broker's data: outages and missing entitlements affect
  the sandbox too.

Details: [Sandbox mode](../../user/sandbox.md#limits-of-the-simulation).

## A test workflow

1. Switch to analyzer mode and check the badge (or `POST /api/v1/analyzer`
   shows `"analyze_mode": true`).
2. Place one MARKET order. Check the order book, trade book, positions and
   dashboard funds.
3. Place a LIMIT or stop order away from the market. Check it waits; modify
   it; cancel it.
4. Run the smart, basket and split orders your strategy uses.
5. Send a real alert from TradingView, Chartink or your script; check
   **Sandbox Logs**.
6. Let an MIS position reach the square-off time, or check the times on the
   Sandbox page.
7. Review **My P&L**, and what the trades cost on **Sandbox Charges**,
   **Transaction Costs**.
8. Before going live: switch to Live, reduce the quantity, check the badge
   again, and watch your broker's own terminal for the first orders.

---

**Previous**: [14 - Positions and holdings](../14-positions-holdings/README.md)

**Next**: [16 - TradingView integration](../16-tradingview-integration/README.md)
