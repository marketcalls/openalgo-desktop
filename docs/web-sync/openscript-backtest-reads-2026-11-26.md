# OpenScript backtest reads, a fix to carry to OpenAlgo web (2026-11-26)

Web 264b3c2a7 ("any minute timeframe on /trading charts and backtests")
makes a backtest fetch the bars a strategy's reads need beyond the chart's
own: a fifteen or three minute read on a ten minute chart, which the engine
builds from five or one minute bars, and a `req.symbol` read of another
instrument. OpenAlgo Desktop carried the change over for 1.0.10, and review
found that on the real engine it fetches nothing, so those backtests are
still refused as on openalgo-script 0.8.1. The fix is made here and noted
for the web, as with the earlier write-ups in this folder.

Web line numbers are from `frontend/src/lib/trading/backtestRun.ts` at
264b3c2a7; the code is unchanged by the web's follow-up e44126d67. Search
for the old text if the lines have moved.

## The defect

`servedFor` (line 529) learns which reads need bars by loading the strategy
once with a provider that records what it is asked. Its host (line 538) is
`{ instrument, requestBars }`. The engine's own backtest host differs in two
ways that decide what is asked (`openalgo-script` 0.9.0,
`dist/core/backtest/drive.js`, `instrumentFor` and `hostFor`):

1. **No order route.** A strategy that calls an order function requires the
   `orders` capability, and `load` checks capabilities before it plans any
   read: "OS6006 This program requires orders and this engine does not have
   it." The result of the probe's `load` is not looked at, so no read is
   recorded, nothing is fetched and the run itself is refused: "OS6015 15 is
   not a whole multiple of 10m.", or for `req.symbol` "OS6006 This program
   requires req.symbol and this engine does not have it."
2. **No symbol or exchange.** `instrument` is the run's facts (interval,
   zone, session), which carry neither; the engine's host adds the
   contract's. With a route alone, a read of the chart's own instrument is
   asked with `instrument: null` and dropped at line 556 (OS6015 again), and
   a `req.symbol` read is recorded with `exchange: null` while the run asks
   with the chart's exchange. Its bars are fetched and never served: the
   backtest passes with no trade and says nothing.

`backtestRun.test.ts` replaces `load` with a mock that answers `ok: true` and
asks about instrument `AAA` on `XX`, so its test of the fifteen minute read
passes without the engine.

## The fix

In `servedFor`, take the contract (`priced` in `runBacktest`, line 427 passes
it) and give the probe the run's host less the bars:

```ts
const probed = engine.load(program, {
  settings: inputs,
  host: {
    instrument: { ...instrument, symbol: contract.symbol, exchange: contract.exchange },
    route: () => undefined, // nothing runs at load
    requestBars: /* unchanged */,
  },
})
if (!probed.ok) return [] // the run is refused with the same diagnostic
```

Desktop commit: `fix(openscript): a backtest's read probe states the run's
identity and an order route ...` on the 1.0.10 branch. Its test,
`src/lib/trading/backtestServedReads.test.ts`, runs `runBacktest` on the real
compiler and engine with only the network replaced, and fails on the web's
code:

| Strategy on a ten minute chart | Web code | Fixed |
| --- | --- | --- |
| `req.timeframe("15", close)`, buys | refused, OS6015 | fetches 5m of the chart's instrument, one trade |
| `req.symbol("RELIANCE", "15", close)`, buys | refused, OS6006 (route alone: ok, 0 trades) | fetches RELIANCE 5m on the chart's exchange, one trade |
| `req.timeframe("20", close)`, buys | one trade, nothing extra fetched | the same |

## Also in that file

The block inserted by 264b3c2a7 (`HISTORY_MINUTES` to `servedFor`, lines 478
to 604) sits between `fold`'s comment (line 460, "Run the engine, on a worker
where there is one ...") and `async function fold` (line 605), so the
comment documents `HISTORY_MINUTES` and `fold` has none. The desktop moved
the block above the comment.
