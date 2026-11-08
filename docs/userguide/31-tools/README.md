# 31 - Tools

## Introduction

The **Tools** page (`/tools`, **Tools** in the top bar) holds OpenAlgo
Desktop's options and market analysis tools. Each card opens one tool. They
read your broker's option chains, quotes, history and live feed, so a broker
must be logged in, and a tool shows data only for the exchanges, expiries and
fields your broker supplies. Sector Rotation can also read Historify, with no
broker logged in.

OpenAlgo Desktop has 17 tools. OpenAlgo web also has the Portfolio
Backtester, SIP Backtester and Portfolio Analyzer; they need Python (pandas)
and are not in the desktop.

## The tools

| Tool | Page | What it does |
| --- | --- | --- |
| Strategy Builder | `/strategybuilder` | Build multi-leg option strategies with live Greeks, a payoff diagram and what-if simulators |
| Strategy Portfolio | `/strategybuilder/portfolio` | Saved strategies in two watchlists, **MyTrades** and **Simulation** |
| Sector Rotation | `/sector-rotation` | Which NSE sectors, stocks and indices are leading, weakening, lagging or improving against a benchmark, with daily or weekly trails, from Historify or your broker ([API](../../api/analytics-services/sectorrotation.md)) |
| Option Chain | `/optionchain` | Live option chain with Greeks, OI, volume and quick order placement |
| Option Greeks | `/ivchart` | Historical IV, Delta, Theta, Vega and Gamma of ATM options |
| OI Tracker | `/oitracker` | CE and PE open interest by strike, with PCR and the ATM strike |
| OI Range | `/oirange` | Open interest for a strike range you choose, with ATM-relative quick picks and an optional 1-minute refresh (off by default) |
| Max Pain | `/maxpain` | The max pain strike and the pain distribution across strikes |
| Straddle Chart | `/straddle` | The ATM straddle with a rolling strike, plus Spot and Synthetic Futures |
| Straddle PnL | `/straddlepnl` | A simulated intraday ATM straddle with automated N-point adjustments and a trade log |
| Vol Surface | `/volsurface` | A 3D implied volatility surface across strikes and expiries |
| GEX Dashboard | `/gex` | Gamma exposure: OI walls, net GEX per strike and the top gamma strikes |
| Strike Profile | `/strikeprofile` | GEX and DEX by strike and expiry as a heatmap, with peaks, the gamma flip and the call and put walls |
| Gamma Density | `/gammadensity` | Gamma times OI density and convexity zones, intraday and to-expiry views, ATM IV and expected-move bands |
| IV Smile | `/ivsmile` | Call and put IV curves, ATM IV and skew |
| OI Profile | `/oiprofile` | Futures candles with the OI butterfly and the daily OI change by strike |
| Arbitrage | `/arbitrage` | Futures calendar spreads on NFO and MCX, ranked by executable spread, with two-leg orders |

### Strategy Builder and Strategy Portfolio

Pick an underlying and expiry, add legs, and read the result on the
**Payoff**, **Greeks**, **P&L**, **Strategy Chart** and **Multi Strike OI**
tabs. Save a strategy to the Strategy Portfolio, or send every leg as one
basket order. Basket orders follow the mode switch (sandbox in analyzer mode)
and, because they are sent with your API key, wait in the
[Action Center](../22-action-center/README.md) when Semi-Auto is on. The same
applies to Arbitrage's two-leg orders and to orders from the Option Chain.

### How the numbers are worked out

Under their charts, GEX Dashboard, IV Smile, OI Tracker and Max Pain say how
their numbers are computed: the model (Black-76), the price it is computed
against, and the strikes covered, for example "Totals and PCR cover the 41
strikes around the ATM strike shown here, not the whole option chain. Open
interest is as your broker reports it." GEX and IV Smile compute on the spot
price, while the Option Chain's Greeks use the synthetic future, so their
values can differ slightly. These notes are a desktop addition; the web pages
do not show them.

Gamma Density says so when no option price in the chain gave an implied
volatility: its bands and densities then use an assumed IV and are estimates.

In the Option Chain, a Greek that cannot be computed for a leg (no quote, an
expired chain, a price that gives no IV) shows as a dash, never as 0.00.

## When open interest is missing

Some brokers answer an option chain without open interest for some options.
OpenAlgo Desktop does not count a missing value as zero:

- **OI Tracker, OI Range, Max Pain, GEX Dashboard, Gamma Density and OI
  Profile** show no OI figures for that chain and say why, for example: "Open
  interest did not come back from your broker for 12 of 82 options of NIFTY
  27-OCT-26, so the OI figures are not shown: a missing value would read as
  zero. Try again shortly." A gap would otherwise look like a quiet strike and
  move PCR, max pain and the walls.
- **Strike Profile** is the exception. It matches OpenAlgo web exactly
  (maintainer decision, 2026-10-20): an option whose quote or open interest
  did not come back is counted as zero open interest, and the page does not
  mark it. Keep this in mind when a strike looks empty on a broker that
  sometimes leaves OI out.

OpenAlgo web counts missing open interest as zero in every tool.

## Strike Profile in detail

Strike Profile shows gamma exposure (GEX) and delta exposure (DEX) for every
strike of several expiries at once, as a strike-by-expiry grid. It is the same
as on OpenAlgo web.

- **Units.** Values are in rupees crore. GEX is what hedgers would trade for a
  1% move in the underlying, calls positive and puts negative. DEX is the
  delta-weighted open interest (call delta times OI plus put delta times OI,
  times spot).
- **Expiries.** One column per expiry, labelled with its days to expiry. The
  nearest six are selected by default; choose more from the expiry list (up to
  20 in one request). Each expiry is one option chain request to your broker,
  so add more only if your broker answers quickly.
- **Exposure**: **GEX** or **DEX**. **View**: **Table** (the heatmap) or
  **Chart** (the net per strike, summed across the selected expiries).
  **Panes**: **1** or **2**, to see GEX and DEX together.
- **Peaks**: mark each expiry's largest positive and negative strike
  (**All**), only the larger of the two (**Max**), or none (**Off**). A near
  expiry always carries far more gamma than a far one; the peaks make a level
  on a far expiry visible.
- **Shade**: colour each cell against the whole grid (**Global**) or against
  its own expiry's largest value (**Per expiry**).
- **Flip** is the underlying price at which total GEX across the selected
  expiries changes sign, found by re-pricing every option's gamma at nearby
  prices. **C wall** and **P wall** are the strikes with the most call and put
  GEX.
- **Range**: the strikes shown around spot (**±2%**, **±5%**, **±10%** or
  **All**). The copy button copies the table.

If your broker is busy, a request may be refused for a moment; refresh again.

## Tips

- Stay logged in to your broker and keep the feed running; the tools stream
  live prices. Check **Admin**, **Diagnostics**, **Run checks** if prices look
  frozen (see [29 - Troubleshooting](../29-troubleshooting/README.md#check-the-health-page-and-run-diagnostics)).
- An empty tool usually means the underlying has no options at your broker,
  the expiry has passed, or the master contract has not loaded.
- The tools follow your theme and accent colour.

## Related

- [11 - Order Types](../11-order-types/README.md)
- [13 - Basket Orders](../13-basket-orders/README.md)
- [24 - PnL Tracker](../24-pnl-tracker/README.md)
- [32 - Charting Terminal](../32-charting-terminal/README.md)

---

**Previous**: [30 - FAQs](../30-faqs/README.md)

**Next**: [32 - Charting Terminal](../32-charting-terminal/README.md)
