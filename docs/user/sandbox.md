# Sandbox mode (analyzer mode)

Sandbox mode sends every order from the app, the API, webhooks and MCP
clients to a simulated account instead of your broker. Use it to check that a
strategy, webhook or API client sends what you expect before you trade live.
Turn it on with the mode switch in the top bar; the badge shows **Analyze
Mode** while it is on.

The sandbox is an order and account simulator that prices fills from your
broker's live data. It is not an exchange simulator and not a backtester, so
its results are a check of your order flow, not a forecast of live results.

## How orders fill

- **Market orders** fill at the ask when buying and the bid when selling, or
  at the last traded price when the quote has no bid or ask.
- **Limit orders** that are already marketable fill at the last traded price.
  Resting limit orders fill at their limit price once the price trades
  through it.
- **Stop orders** (SL and SL-M) wait until the trigger price trades, then fill
  at the last traded price (an SL only within its limit).
- Prices come from the live feed for every symbol the sandbox needs. When a
  symbol's feed has been quiet for 30 seconds, the sandbox asks your broker
  for a quote every few seconds (the **Order Check Interval** setting) and
  subscribes to that symbol again.

## Limits of the simulation

- **Orders fill in full or not at all.** There are no partial fills, no queue
  position and no market impact. A fill uses the price your broker reports at
  that moment; depth beyond the best bid and ask is not considered.
- **P&L is shown before charges.** Brokerage and statutory charges are
  taken from the available balance when an order fills (see below), and the
  P&L figures stay gross, as a broker's position book shows them, with the
  charges beside them.
- **Quantities are in contracts.** For a crypto contract the contract value
  (for example 0.001 BTC per BTCUSD contract) applies to its margin, trade
  value and P&L, so 10 BTCUSD contracts at 60,000 have a value of 600.
- **Capital is in rupees; crypto prices are in US dollars.** The sandbox does
  not convert between them: crypto margin and P&L are counted as the same
  number of rupees.
- **CNC buys settle on the next calendar day.** At 00:00 IST, CNC positions
  bought the day before move to holdings, weekends and market holidays
  included.
- **Fills depend on your broker's prices.** With no broker connected, or no
  usable price for a symbol (none at all, one outside the day's high and low,
  or one older than 30 seconds), orders wait instead of filling. Market data
  entitlements and outages at your broker affect the sandbox the same way.
- **Margin is a simple leverage rule** (the leverage settings on the Sandbox
  page), not your broker's SPAN and exposure margin.

## Brokerage and charges

Every sandbox fill pays brokerage under your own plan and the statutory
charges (STT or CTT, exchange transaction charges, SEBI fees, stamp duty and
GST), so sandbox results come close to what the same trades cost in a live
account. The charge is taken from the available balance in the same step that
fills the order; realized P&L and the margin in use do not change with it.

| Page | What it is for |
| --- | --- |
| **Sandbox Charges** (`/sandbox/commissions`) | Your broker's brokerage for each segment, the switch that turns charges off, the statutory rates, and the **Transaction Costs** tab: what trades have cost in a date range, by segment, by day and by trade, with a CSV export |
| **Brokerage Calculator** (`/sandbox/calculator`) | What a buy and a sell cost before you place them: brokerage, every statutory line, the points to breakeven and the net P&L |
| **My P&L** (`/sandbox/mypnl`) | Today's and lifetime charges beside the P&L, and a charges column per day and per trade |

Brokerage is two numbers per segment: an amount per executed order and a
percentage of the trade value. With both set, the lower of the two is charged
on each order; set either to 0 to leave it out.

| Segment | Default brokerage |
| --- | --- |
| Equity delivery | 20 per order or 2.5% of the trade value, whichever is lower |
| Equity intraday | 20 per order or 0.1%, whichever is lower |
| Futures (equity, currency, commodity) | 20 per order or 0.03%, whichever is lower |
| Options (equity, currency, commodity) | 20 per order |
| Crypto | Nothing until you enter your venue's fee |

The statutory rates are the published ones, the same at every broker, and
cannot be changed. STT and stamp duty are rounded to the nearest rupee on
each fill, the other lines to the paisa. A trader logged in to a crypto
venue (Delta Exchange) sees the crypto segments only, as a venue fee and a
Fee Calculator; a crypto trade pays that fee and nothing else.

Not charged: depository (DP) charges on a delivery sell, and STT on an
option that is held to expiry and exercised (expiry settlement closes the
position without an order). A delivery buy and sell on the same day pays
delivery rates on both. Trades made before charges were introduced (1.0.10)
are not charged after the fact. A change to the plan applies from the next
fill.

`POST /api/v1/charges` gives the charges of one order before it is placed,
under the same plan: see [Charges](../api/account-services/charges.md).

## Closing positions

A close sends a MARKET order for the open quantity. When there is no usable
price, that order waits and fills as soon as a price arrives. Close all
positions then reports it as waiting, for example "Closed 0 positions
(1 close order waiting for a price)", and asking again does not place a
second close. The automatic MIS square-off works the same way.

An exit placed against an open position (a stop or a target) blocks no
margin while the position covers it. If another order has already closed the
position when the exit fills, the exit opens a new position and the sandbox
blocks that position's margin at that moment. If your sandbox funds cannot
cover it, the exit is rejected with the reason "Insufficient funds".

## Times and calendars

- MIS positions are squared off at the exchange times on the Sandbox page
  (NSE, BSE, NFO and BFO at 15:15 IST by default). Crypto has no square-off
  and trades at any hour, every day.
- Today's realized P&L resets at 03:00 IST.
- The daily P&L snapshot in My P&L is taken at 23:59 IST on trading days, and
  every day, weekends included, when you held or traded crypto that day. If
  the app was closed at 23:59, that snapshot is filled in when the app is
  started the following day.

## Where to find it

- **Sandbox** (profile menu): capital, leverage, square-off times, and the
  buttons to **My P&L**, **Charges** (the brokerage plan and the transaction
  cost report) and **Calculator**.
- **Logs**, then **Sandbox Logs**: every request made while analyzer mode was
  on.

The sandbox keeps its data in its own database, apart from live trading. A
reset on the Sandbox page clears sandbox orders, trades, positions, holdings,
GTT orders and P&L history, restores the starting capital and puts the
settings, the brokerage plan included, back to their defaults.
