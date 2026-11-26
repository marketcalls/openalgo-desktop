# OpenScript follow-ups e44126d67 and 7a92a704c, notes for OpenAlgo web (2026-11-26)

OpenAlgo Desktop carried web e44126d67 ("fetch a finer read that is its own
base in backtests") and 7a92a704c ("skill lessons from a multi-timeframe
study, and run.mjs") over for 1.0.10. Three things found while porting them
belong to the web as well. They follow
`openscript-backtest-reads-2026-11-26.md`, which this depends on.

## 1. e44126d67 changes nothing on the web until the probe fix lands

`servedFor` now asks for a read's bars when the interval the engine asks at
is finer than the run's (`finerThanChart`), which is right. But on the web
the probe that records those reads has no order route, so for every strategy
`load` refuses with OS6006 (`orders`) before any read is planned, and nothing
is recorded to fetch (`openscript-backtest-reads-2026-11-26.md`, point 1).
Backtests run strategies only, so a fifteen minute read on an hourly run is
still refused on the web: "OS6002: The chart is 1h and the request asks for
15m." With no other read served, the engine has no provider at all, so the
run is refused rather than the read being absent, as the commit message says.

Desktop test: `src/lib/trading/backtestServedReads.test.ts`, "fetches a
fifteen minute read spelled "15m" on an hourly chart, and trades on it", on
the real compiler and engine with only the network replaced. It fails with
the web's probe host (checked by putting `{ instrument, requestBars }` back:
OS6002) and with the web's old comparison (OS6002), and passes with both
fixes. `backtestRun.test.ts` replaces `load`, so the web's two new tests
there pass either way.

## 2. A comment left on the old comparison

`servedFor`'s doc comment still says "A read of the chart's own instrument
whose `interval` is its `timeframe` folds from the chart's bars and needs
nothing", which is the comparison e44126d67 replaced and the reason it was
wrong. The desktop's says the read is asked at an interval no finer than the
chart's, and names `finerThanChart`.

## 3. run.mjs refuses every strategy

`.claude/skills/openscript/run.mjs` loads the program with
`{ host: { instrument } }`: no order route. The engine runs every study on
such a host and refuses exactly the strategies (`dist/core/engine/host.d.ts`
on `route`), so any strategy prints "Refused at load: OS6006 This program
requires orders and this engine does not have it.", which reads like a fault
in the script. SKILL.md's "Run it on bars before installing" presents it as
running any draft. The desktop's copy answers a strategy with where to check
it instead (the Backtest panel on `/trading`) and its SKILL.md says it runs a
study. A recording route would let a strategy load, but with no fills its
position never changes, so what it printed would not be the strategy either.
