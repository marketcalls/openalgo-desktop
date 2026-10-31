# Symbol Format

OpenAlgo names every instrument the same way on every broker. You write
`NIFTY27OCT26FUT` or `NIFTY06OCT2622400CE` once, and OpenAlgo Desktop
translates it to your broker's own trading symbol when it places the order.
Order books, trade books, positions and holdings come back in the same OpenAlgo
symbols, so a strategy never has to know which broker it is running on.

OpenAlgo Desktop uses exactly the same symbology as OpenAlgo web. A symbol
that works against the web works against the desktop unchanged.

Crypto contracts (Delta Exchange) follow the same grammar with a few
crypto-only shapes; see [crypto-symbol-format.md](crypto-symbol-format.md).

## Where the symbols come from

After you sign in to your broker, the desktop downloads the broker's master
contract (its full instrument list) and converts each row to an OpenAlgo
symbol. It downloads again when it has not done so yet today, when today's
download was before 08:00 IST (00:00 UTC for Delta Exchange), or when you
switch to another broker; otherwise it reuses the stored list. If a download
fails part way, the previous list is kept.

A symbol exists only if your broker lists it. Use these endpoints to find the
exact symbol rather than typing it from memory:

| Endpoint | Use |
| --- | --- |
| `POST /api/v1/search` | Find symbols by text, optionally on one exchange |
| `POST /api/v1/symbol` | Full details of one symbol (lot size, tick size, expiry, token) |
| `POST /api/v1/expiry` | Expiry dates of an underlying's futures or options |
| `POST /api/v1/optionsymbol` | The option symbol at an offset from ATM (ATM, ITM1 to ITM50, OTM1 to OTM50) |
| `GET /api/v1/instruments` | The whole list, or one exchange, as JSON or CSV |

## Equity symbol format

An equity symbol is the base symbol of the stock.

| Instrument | Exchange | OpenAlgo symbol |
| --- | --- | --- |
| Infosys | `NSE` | `INFY` |
| Tata Motors | `BSE` | `TATAMOTORS` |
| State Bank of India | `NSE` | `SBIN` |

## Futures symbol format

**Format:** `[Base Symbol][Expiry Date]FUT`, with the expiry date as `DDMMMYY`.

| Instrument | Exchange | OpenAlgo symbol |
| --- | --- | --- |
| Bank Nifty future, 24 April 2024 | `NFO` | `BANKNIFTY24APR24FUT` |
| SENSEX future, 24 April 2024 | `BFO` | `SENSEX24APR24FUT` |
| USDINR future, 10 May 2024 | `CDS` | `USDINR10MAY24FUT` |
| Crude Oil Mini future, 20 May 2024 | `MCX` | `CRUDEOILM20MAY24FUT` |
| 7.26% GS 2033 bond future, 25 April 2024 | see below | `726GS203325APR24FUT` |

The date in an OpenAlgo symbol is always the contract's actual expiry day, even
when the broker's own symbol carries only the month. For example, Zerodha's
`CRUDEOIL26OCTFUT` is OpenAlgo `CRUDEOIL19OCT26FUT`, and Zerodha's
`NIFTY26OCTFUT` is OpenAlgo `NIFTY27OCT26FUT`.

Government bond (interest-rate) contracts follow the same pattern. Which
exchange code carries them depends on your broker's master contract; find
the exact symbol and exchange with `/api/v1/search`.

## Options symbol format

**Format:** `[Base Symbol][Expiry Date][Strike Price][CE or PE]`

| Instrument | Exchange | OpenAlgo symbol |
| --- | --- | --- |
| Nifty 20800 call, 28 March 2024 | `NFO` | `NIFTY28MAR2420800CE` |
| Vedanta 292.50 call, 25 April 2024 | `NFO` | `VEDL25APR24292.5CE` |
| USDINR 82 call, 19 April 2024 | `CDS` | `USDINR19APR2482CE` |
| Crude Oil 6750 call, 17 April 2024 | `MCX` | `CRUDEOIL17APR246750CE` |
| 7.26% GS 2032 bond 97 put, 25 April 2024 | see futures | `726GS203225APR2497PE` |

The strike is written the shortest way that keeps its value: a whole number
loses the decimal point (`20800`, not `20800.0`), and a fractional strike keeps
its fraction (`292.5`, `187.5`, `292.55`). A decimal strike is part of the
exact symbol.

The dates above show the format. For live contracts, ask
`/api/v1/expiry` or `/api/v1/search`.

## Exchange codes

| Code | Market |
| --- | --- |
| `NSE` | NSE equity |
| `BSE` | BSE equity |
| `NFO` | NSE futures and options |
| `BFO` | BSE futures and options |
| `CDS` | NSE currency derivatives |
| `BCD` | BSE currency derivatives |
| `MCX` | MCX commodities |
| `NCDEX` | NCDEX commodities |
| `NCO` | NSE commodity futures and options |
| `NSE_INDEX` | NSE indices (quotes and data only) |
| `BSE_INDEX` | BSE indices (quotes and data only) |
| `MCX_INDEX` | MCX commodity sectoral indices (quotes and data only) |
| `GLOBAL_INDEX` | Global indices such as US30, JAPAN225, GIFTNIFTY (quotes and data only) |
| `CRYPTO` | Crypto contracts (Delta Exchange) |

An exchange code must be written in capitals exactly as above; `nse` is
refused. Not every broker carries every exchange: `NCO`, `MCX_INDEX`,
`GLOBAL_INDEX` and `NCDEX` appear only when your broker's master contract lists
them, and `CRYPTO` only with Delta Exchange. See
[order-constants.md](order-constants.md) for the full list of request values.

## Index symbols

Index symbols are the index name with spaces and hyphens removed. They are for
quotes, depth, history and WebSocket subscriptions. An index is not a tradable
instrument: trade it through its futures and options on `NFO`, `BFO` or `MCX`.
Analyzer mode does not refuse an order on an index symbol (neither does
OpenAlgo web), so a sandbox fill on one tells you nothing about the live
market.

Not every broker publishes every index. If a symbol below is missing, search
for it with `/api/v1/search`.

### NSE indices (`NSE_INDEX`)

The common ones, which the desktop names the same way for every broker:

NIFTY, NIFTYNXT50, FINNIFTY, BANKNIFTY, MIDCPNIFTY, INDIAVIX,
HANGSENGBEESNAV, NIFTY100, NIFTY200, NIFTY500, NIFTYALPHA50, NIFTYAUTO,
NIFTYCOMMODITIES, NIFTYCONSUMPTION, NIFTYCPSE, NIFTYDIVOPPS50, NIFTYENERGY,
NIFTYFMCG, NIFTYGROWSECT15, NIFTYGS10YR, NIFTYGS10YRCLN, NIFTYGS1115YR,
NIFTYGS15YRPLUS, NIFTYGS48YR, NIFTYGS813YR, NIFTYGSCOMPSITE, NIFTYINFRA,
NIFTYIT, NIFTYMEDIA, NIFTYMETAL, NIFTYMIDLIQ15, NIFTYMIDCAP100,
NIFTYMIDCAP150, NIFTYMIDCAP50, NIFTYMIDSML400, NIFTYMNC, NIFTYPHARMA,
NIFTYPSE, NIFTYPSUBANK, NIFTYPVTBANK, NIFTYREALTY, NIFTYSERVSECTOR,
NIFTYSMLCAP100, NIFTYSMLCAP250, NIFTYSMLCAP50, NIFTY100EQLWGT,
NIFTY100LIQ15, NIFTY100LOWVOL30, NIFTY100QUALTY30, NIFTY200QUALTY30,
NIFTY50DIVPOINT, NIFTY50EQLWGT, NIFTY50PR1XINV, NIFTY50PR2XLEV,
NIFTY50TR1XINV, NIFTY50TR2XLEV, NIFTY50VALUE20

Further NSE indices, as some brokers (Fyers, for example) list them:

BHARATBONDAPR30, BHARATBONDAPR31, BHARATBONDAPR32, BHARATBONDAPR33,
NIFTY100ALPHA30, NIFTY100ENHESG, NIFTY100ESG, NIFTY100ESGSECLDR,
NIFTY200MOMENTM30, NIFTY200VALUE30, NIFTY500EW, NIFTY500LMSEQL,
NIFTY500LOWVOL50, NIFTY500MOMENTM50, NIFTY500MULTICAP, NIFTY500QLTY50,
NIFTY500SHARIAH, NIFTY500VALUE50, NIFTY50SHARIAH, NIFTYALPHALOWVOL,
NIFTYAQL30, NIFTYAQLV30, NIFTYCAPITALMKT, NIFTYCHEMICALS, NIFTYCONSRDURBL,
NIFTYCOREHOUSING, NIFTYCORPMAATR, NIFTYEV, NIFTYFINSEREXBNK, NIFTYFINSRV2550,
NIFTYFPI, NIFTYHEALTHCARE, NIFTYHIGHBETA50, NIFTYHOUSING, NIFTYINDDEFENCE,
NIFTYINDDIGITAL, NIFTYINDIAMFG, NIFTYINDTOURISM, NIFTYINTERNET, NIFTYIPO,
NIFTYLARGEMID250, NIFTYLOWVOL50, NIFTYM150QLTY50, NIFTYMICROCAP250,
NIFTYMIDSMLHLTH, NIFTYMOBILITY, NIFTYMS400MQ100, NIFTYMSFINSERV,
NIFTYMSINDCONS, NIFTYMSITTELCM, NIFTYMULTIINFRA, NIFTYMULTIMFG,
NIFTYMULTIMQ50, NIFTYNEWCONSUMP, NIFTYNONCYCCONS, NIFTYOILANDGAS,
NIFTYQLTYLV30, NIFTYQUALITY30, NIFTYRAILWAYSPSU, NIFTYRURAL, NIFTYSHARIAH25,
NIFTYSML250MQ100, NIFTYSML250Q50, NIFTYTATA25CAP, NIFTYTOP10EW, NIFTYTOP15EW,
NIFTYTOP20EW, NIFTYTOTALMKT, NIFTYTRANSLOGIS

### BSE indices (`BSE_INDEX`)

SENSEX, BANKEX, SENSEX50, BSE100, BSE150MIDCAPINDEX, BSE200,
BSE250LARGEMIDCAPINDEX, BSE400MIDSMALLCAPINDEX, BSE500, BSEAUTO,
BSECAPITALGOODS, BSECARBONEX, BSECONSUMERDURABLES, BSECPSE, BSEDOLLEX100,
BSEDOLLEX200, BSEDOLLEX30, BSEENERGY, BSEFASTMOVINGCONSUMERGOODS,
BSEFINANCIALSERVICES, BSEGREENEX, BSEHEALTHCARE, BSEINDIAINFRASTRUCTUREINDEX,
BSEINDUSTRIALS, BSEINFORMATIONTECHNOLOGY, BSEIPO, BSELARGECAP, BSEMETAL,
BSEMIDCAP, BSEMIDCAPSELECTINDEX, `BSEOIL&GAS`, BSEPOWER, BSEPSU, BSEREALTY,
BSESENSEXNEXT50, BSESMALLCAP, BSESMALLCAPSELECTINDEX, BSESMEIPO, BSETECK,
BSETELECOM, BSE100ESG, BSE100LARGECAPTMCINDEX, BSE250SMALLCAPINDEX, BSEALLCAP,
BSEBASICMATERIALS, BSEBHARAT22INDEX, `BSECONSUMERDISCRETIONARYGOODS&SERVICES`,
BSEDIVERSIFIEDFINANCIALSREVENUEGROWTHINDEX, BSEDIVIDENDSTABILITY,
BSEENHANCEDVALUE, BSEFOCUSEDIT, BSEHOSPITALS, BSEINDIAMANUFACTURING,
BSELOWVOLATILITY, BSEMOMENTUM, BSEPRIVATEBANKS, BSEQUALITY, BSEUTILITIES

Two BSE index names contain an ampersand: `BSEOIL&GAS` and
`BSECONSUMERDISCRETIONARYGOODS&SERVICES`.

### NSE commodity derivatives (`NCO`)

NCO carries commodity futures and options listed on NSE. It is available only
with brokers whose master contract includes it (for example Zerodha, Dhan and
Arrow). Underlyings:

ALUMINIUMFUTURES, ALUMINIUMMINIFUTURES, BRENTCRUDEOIL, BRENTCRUDEOILMINI,
COPPER, CRUDEDEGUMSOYBEANOIL, ELECTRICITYFUTURES, GOLD, GOLD10GM, GOLD1GM,
GOLDGUINEA8GM, GOLDMINI, LEADFUTURES, LEADMINIFUTURES, NATURALGASHENRYHUB,
NATURALGASMINI, NICKELFUTURES, PLATTSDATEDBRENTASSESS, SILVER, SILVERMICRO,
SILVERMINI, WTICRUDEOIL, WTICRUDEOILMINI, XAUGOLD, ZINCFUTURES,
ZINCMINIFUTURES

NCO futures and options use the same formats as the other derivatives
(`[Underlying][DDMMMYY]FUT` and `[Underlying][DDMMMYY][Strike][CE or PE]`).
The underlying part is the name your broker publishes for the contract, which
is not always the long name above, so search `NCO` with `/api/v1/search` for
the exact symbol.

### MCX indices (`MCX_INDEX`)

Commodity sectoral indices, quotes and data only. The tradable index futures
and options are on `MCX` (for example `MCXBULLDEX27MAY26FUT`). Several brokers
list them, among them Zerodha, Angel One, Alice Blue, Arrow, Definedge, Motilal
Oswal and Samco.

MCXAGRI, MCXBULLDEX, MCXCOMDEX, MCXCOMPDEX, MCXCOPRDEX, MCXCRUDEX, MCXENERGY,
MCXGOLDEX, MCXMETAL, MCXMETLDEX, MCXSILVDEX

### Global indices (`GLOBAL_INDEX`)

Quotes and data only; no trading. Available with Zerodha and Upstox.

With Zerodha: AUS200, FRANCE40, GERMANY40, GIFTNIFTY, HANGSENG, JAPAN225,
SHANGHAICHINA, UK100, US100, US10YRYIELD, US30, US500, USCOMPOSITE.

With Upstox, the desktop renames the broker's global entries to HANGSENG,
DOWJONES, UK100, US500, GERMANY40, FRANCE40, JAPAN225, US100, GIFTNIFTY, US30,
BRENTOIL and WTIOIL; any other entry keeps Upstox's own name.

`GIFTNIFTY` trades on NSE IFSC, but it is listed under `GLOBAL_INDEX` so all
quote-only global feeds sit in one place.

## Master contract fields

Each instrument the desktop knows is one row with the same columns as OpenAlgo
web. `/api/v1/symbol` returns them for one symbol:

```json
{
  "status": "success",
  "data": {
    "id": 63347,
    "symbol": "NIFTY06OCT2622400CE",
    "brsymbol": "NIFTY26O0622400CE",
    "name": "NIFTY",
    "exchange": "NFO",
    "brexchange": "NFO",
    "token": "10415874::::40687",
    "expiry": "06-OCT-26",
    "strike": 22400.0,
    "lotsize": 65,
    "instrumenttype": "CE",
    "tick_size": 0.05,
    "freeze_qty": 1800
  }
}
```

| Field | Meaning |
| --- | --- |
| `symbol` | The OpenAlgo symbol |
| `brsymbol` | Your broker's trading symbol for it |
| `name` | Underlying or company name, as the broker publishes it |
| `exchange` | OpenAlgo exchange code |
| `brexchange` | The broker's exchange or segment code |
| `token` | The broker's instrument token (format varies by broker) |
| `expiry` | `DD-MMM-YY` in capitals (`06-OCT-26`); empty for instruments that do not expire |
| `strike` | Option strike; `0.0` for everything else |
| `lotsize` | Units per lot, from the broker's master (see [LotSize.md](LotSize.md)) |
| `instrumenttype` | Usually `EQ`, `FUT`, `CE` or `PE`; some brokers pass their own codes through (index rows are often `EQ`) |
| `tick_size` | Minimum price step |
| `freeze_qty` | The exchange's freeze quantity for the underlying, `0` when none is known |

In the desktop, `id` is the row's position in the loaded symbol list, not a
database row number. It changes when the master contract is downloaded again,
so never store it; use `symbol` and `exchange`.

`freeze_qty` comes from NSE's freeze-quantity list for `NFO`, built into the
app, unless
you have added your own value for that underlying on the **Freeze Quantities**
page (`/admin/freeze`); your entry wins. In the desktop the NSE list ships with
each release instead of being copied into your table, so the page lists only
your own entries.

An unknown symbol answers HTTP 404:

```json
{ "status": "error", "message": "Symbol FOOBARBAZ not found in exchange NSE" }
```

## How the strategy module uses symbols

The strategy module (`/strategy`) treats a `(symbol, exchange)` pair as one
exact contract, not a label:

- A batch strategy leg may be configured relatively (ATM offset, a fixed
  strike, an expiry such as weekly or monthly). It is resolved once, when the
  run starts. The resolved symbol is stored, and every exit uses the symbol
  actually held; an ATM leg is never re-resolved later in the session.
- A signal (webhook) leg must name an exact contract. A base symbol such as
  `NIFTY` on `NFO` is refused when the signal arrives ("NIFTY is not a contract
  on NFO") rather than guessed.
- Fills, recovery and broker-book filtering match both the exact symbol and
  the exchange.
- A broker position row is per contract for the whole account. If a manual
  order or another strategy holds the same contract, the quantity cannot be
  divided; the row is marked as shared, and the strategy's P&L comes from its
  own fills.

See [strategy_rms_documentation.md](strategy_rms_documentation.md) for the
strategy module itself.
