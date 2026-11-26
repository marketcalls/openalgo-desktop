# 14 - Positions and Holdings

## Introduction

**Positions** are what you hold from trading today and in open F&O contracts.
**Holdings** are shares in your demat account. This chapter explains both,
the pages that show them, closing positions, and the API calls that read
them.

## Positions or holdings?

| | Positions | Holdings |
| --- | --- | --- |
| What | Today's intraday trades and open F&O contracts | Delivery shares you own |
| Products | MIS, NRML, and CNC bought today | CNC, after settlement |
| How long | Until closed, squared off or expired | Until you sell |
| Page | **Positions** in the top bar | **Holdings** in the profile menu |
| API | `/api/v1/positionbook` | `/api/v1/holdings` |

A CNC buy shows in positions on the day you buy and moves to holdings after
settlement.

## Positions

A position is your net quantity in one **symbol, exchange and product**:

| Quantity | Meaning |
| --- | --- |
| Positive (`10`) | Long: you bought more than you sold |
| Negative (`-10`) | Short: you sold more than you bought |
| `0` | Flat: closed today |

`SBIN` MIS and `SBIN` CNC are two different positions.

### The Positions page

**Positions** in the top bar.

- **Header**: a **Live** badge while prices stream (or **Paused** while the
  page is hidden), **Settings**, **Refresh**, **Export** (CSV) and **Close
  All**.
- **Summary cards**: **Open Positions**, **Long**, **Short** and **Total
  P&L**.
- **Table**: **Symbol**, **Exchange**, **Product**, **Qty**, **Avg Price**,
  **LTP**, **P&L**, **P&L %**, and a close button on each open row. Click a
  column header to sort.
- **Settings**: group rows by **Underlying** or **Underlying & Expiry** (with
  a subtotal per group), and filter by product (CNC, MIS, NRML), direction
  (Long, Short) and exchange. Active filters show as chips above the table.

Prices and P&L update from the live feed; the rows themselves refresh when an
order is placed, filled or cancelled, and when a position is closed.

### Closing a position

**One position**: choose the close button at the end of its row. OpenAlgo
sends a MARKET order for the open quantity, in that symbol, exchange and
product, at once: there is no confirmation step for a single row. A message
then says the position was closed, or why not.

**All positions**: choose **Close All**, then confirm: "This will close all N
open positions at market price. This action cannot be undone."

From the API, `closeposition` closes every open position:

```json
{"apikey": "<APIKEY>", "strategy": "EOD"}
```

```json
{"closed_positions": 15, "failed_closures": 0, "message": "Closed 15 positions", "mode": "analyze", "status": "success"}
```

With nothing open: `{"message": "No open positions to close", "status": "success"}`.

To close or resize one position from code, send a smart order with
`position_size` 0 or the size you want ([chapter 12](../12-smart-orders/README.md)).

In Semi-Auto mode, closing from the API is refused ("Close position operation
is not allowed in Semi-Auto mode..."); close from the Positions page, or
switch to Auto.

In analyzer mode, a close needs a price. If the sandbox has no usable price
for a symbol, the close order waits and fills as soon as one arrives; the
answer says so, for example "Closed 0 positions (1 close order waiting for a
price)". Asking again does not place a second close.

### Position fields from the API

```json
{
  "symbol": "SBIN",
  "exchange": "NSE",
  "product": "MIS",
  "quantity": 8,
  "average_price": 954.1,
  "ltp": 954.1,
  "pnl": 0.0,
  "pnlpercent": 0.0
}
```

| Field | Meaning |
| --- | --- |
| `symbol`, `exchange`, `product` | Which position |
| `quantity` | Net quantity: positive long, negative short |
| `average_price` | Average entry price |
| `ltp` | Last traded price |
| `pnl`, `pnlpercent` | Profit or loss in rupees and percent |

These are the fields the Positions page reads. The example is from analyzer
mode, whose answer also carries `today_realized_pnl`, `unrealized_pnl` and a
few others.

To read one position's quantity, `openposition`:

```json
{"apikey": "<APIKEY>", "strategy": "Check", "symbol": "INFY", "exchange": "NSE", "product": "MIS"}
```

```json
{"mode": "analyze", "quantity": -3, "status": "success"}
```

`quantity` is 0 when there is no position.

## Holdings

Holdings are delivery shares in your demat account: bought with CNC and
settled.

### The Holdings page

Profile menu, **Holdings** (the page is titled **Investor Summary**).

- **Summary cards**: **Total Holding Value**, **Total Investment Value**,
  **Total Profit and Loss**, **Total PnL Percentage**.
- **Table**: **Trading Symbol**, **Exchange**, **Quantity**, **Avg Price**,
  **LTP**, **Product**, **Profit and Loss**, **PnL %**, and on each row
  **Add** and **Exit**, which open an order ticket to buy more or sell, filled
  in with the symbol and quantity.
- **Refresh** and **Export**.

With a crypto broker (Delta Exchange) there are no holdings: the menu entry
is hidden and the page opens the dashboard.

### The holdings answer

```json
{
  "status": "success",
  "data": {
    "holdings": [],
    "statistics": {
      "totalholdingvalue": 0.0,
      "totalinvvalue": 0.0,
      "totalpnlpercentage": 0.0,
      "totalprofitandloss": 0.0
    }
  }
}
```

Each holding carries its symbol, exchange, quantity, product, P&L and P&L
percent.

## P&L

### Positions

```
Long:   P&L = (LTP - average price) x quantity
Short:  P&L = (average price - LTP) x |quantity|

Long 100 SBIN at 620, LTP 625:   (625 - 620) x 100 = +500
Short 50 SBIN at 630, LTP 625:   (630 - 625) x 50  = +250
```

Once a position is partly or fully closed, the closed part's profit is
realized and shows in the dashboard's **Realized P&L**.

### Holdings

```
P&L   = (LTP - average price) x quantity
P&L % = (LTP - average price) / average price x 100
```

## Automatic MIS square-off

- **Live**: your broker squares off MIS positions at its own cut-off time,
  which varies by segment. Check your broker's times, and close positions
  yourself if your strategy needs a definite exit.
- **Analyzer mode**: the sandbox squares off MIS positions at the times on the
  **Sandbox** page: NSE, BSE, NFO and BFO at 15:15 IST, CDS and BCD at 16:45,
  MCX at 23:30 and NCDEX at 17:00 by default. Crypto has no square-off.

## Converting products

OpenAlgo has no API call to convert a position from MIS to NRML or CNC. Use
your broker's app or terminal, where your broker allows it, before its
square-off time.

## In analyzer mode

Positions and holdings come from the sandbox, not your broker:

- a CNC buy moves to sandbox holdings at 00:00 IST the next calendar day,
  weekends and holidays included;
- closed positions leave the position book;
- sandbox P&L is shown before charges; brokerage and statutory charges
  (on a crypto venue, only the venue fee you set) are taken from the
  sandbox balance at each fill and shown beside it.

See [Sandbox mode](../../user/sandbox.md).

## Good practice

**Positions**

- Check positions at the start and end of every day.
- Know your stop-loss before you enter.
- Close MIS positions yourself before the broker's cut-off.
- Use smart orders in automated systems, so a repeated alert cannot double a
  position.

**Holdings**

- Review them regularly.
- Remember holdings are not squared off: an Exit is a sell you place.

API reference: [positionbook](../../api/account-services/positionbook.md),
[holdings](../../api/account-services/holdings.md),
[openposition](../../api/order-information/openposition.md),
[closeposition](../../api/order-management/closeposition.md).

---

**Previous**: [13 - Basket orders](../13-basket-orders/README.md)

**Next**: [15 - Analyzer mode](../15-analyzer-mode/README.md)
