# Lot Sizes

Futures and options trade in lots. A NIFTY option lot, for example, is a fixed
number of units set by the exchange, and the exchange revises these numbers
from time to time.

## Where the desktop gets lot sizes

OpenAlgo Desktop has no lot-size table of its own. Every lot size comes from
your broker's master contract, which the desktop downloads after you sign in
to the broker and refreshes once a day (after 08:00 IST; after 00:00 UTC for
Delta Exchange). The value is the `lotsize` field of each instrument.

So the lot size the desktop uses is always the one your broker publishes for
that exact contract and expiry. When the exchange changes a lot size, the new
value arrives with the next master contract download.

OpenAlgo web's `LotSize.md` is a table of NSE lot sizes for April to June 2026.
It is not reproduced here, because any fixed table goes out of date; ask the
API for the current value instead.

## How to read a lot size

| Endpoint | Where the lot size is |
| --- | --- |
| `POST /api/v1/symbol` | `data.lotsize` for one symbol |
| `POST /api/v1/optionsymbol` | `lotsize` beside the resolved option symbol |
| `POST /api/v1/optionchain` | `lotsize` on every call and put |
| `POST /api/v1/search` | `lotsize` on every match |
| `GET /api/v1/instruments` | `lotsize` on every row of the master contract |

Example request:

```json
{
  "apikey": "<APIKEY>",
  "symbol": "NIFTY27OCT26FUT",
  "exchange": "NFO"
}
```

sent to `http://127.0.0.1:5000/api/v1/symbol` answers, among the other fields:

```json
{
  "status": "success",
  "data": {
    "symbol": "NIFTY27OCT26FUT",
    "exchange": "NFO",
    "expiry": "27-OCT-26",
    "lotsize": 65,
    "tick_size": 0.1,
    "freeze_qty": 1800
  }
}
```

A cash equity has a lot size of `1`. An index row (`NSE_INDEX`, `BSE_INDEX`)
may show `0` because it is not tradable.

## Examples

For orientation only, these were the lot sizes of a few October 2026
contracts. Check the API before you trade.

| Underlying | Exchange | Lot size |
| --- | --- | --- |
| NIFTY | NFO | 65 |
| BANKNIFTY | NFO | 30 |
| CRUDEOIL | MCX | 100 |
| ABB | NFO | 125 |
| 360ONE | NFO | 500 |

## Quantity is always in units

Every `/api/v1` order endpoint takes `quantity` in units (shares or contracts),
never in lots. To buy two NIFTY lots of 65, send `"quantity": 130`.

On a derivatives exchange the quantity must be a whole multiple of the lot
size:

- In analyzer mode the sandbox checks it on `NFO`, `BFO`, `CDS`, `BCD`, `MCX`,
  `NCDEX` and `CRYPTO`, and answers HTTP 400 with "Quantity must be in
  multiples of lot size 65" (same as the web).
- In live mode your broker checks it and refuses a wrong multiple.

Neither the sandbox nor the web's sandbox checks `NCO` quantities against the
lot size.

### MCX lots with Zerodha

Zerodha publishes a lot size of `1` for every MCX contract because it counts
MCX quantities in contracts. The desktop, like the web, stores the real lot
(for example 100 for CRUDEOIL) and converts your units back to contracts when
it talks to Zerodha. You always send units: `"quantity": 100` is one CRUDEOIL
lot.

### Crypto

On Delta Exchange the lot size is the minimum order size. Futures and options
count whole contracts; spot sizes can be fractional (for example `0.0001`
BTC). See [crypto-symbol-format.md](crypto-symbol-format.md).

## Lots in the strategy module

The strategy module sizes derivative legs in lots and converts them to units
with the contract's lot size from the master contract when it builds the
order: at run start for a batch leg, when the signal arrives for a signal leg.
Cash legs are counted in shares.

A signal leg may instead be sized in units. Then the quantity must be a whole
number of lots, or the strategy is refused when you save it, for example:
"legs[0].qty is 10, which is not a whole number of lots. NIFTY27OCT26FUT on NFO
trades in lots of 65." See
[strategy_rms_documentation.md](strategy_rms_documentation.md).

## Freeze quantity

The exchange also caps how many units one order may carry (the freeze
quantity). `/api/v1/symbol` returns it as `freeze_qty`, `0` when none is known.
The desktop ships NSE's NFO freeze limits built in and lets you add your own on
the **Freeze Quantities** page (`/admin/freeze`); your entries take precedence.
Use `splitorder` (or `splitsize` on options orders) to break a large order into
pieces below the freeze quantity. NSE revises these limits too (the built-in
list carries the limits in force from 5 October 2026); a new desktop release
brings the new list.
