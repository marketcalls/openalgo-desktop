---
name: openscript
description: Write an OpenScript study or strategy for OpenAlgo, and install it into the desktop app's openscript/ folder (app data directory) only after it compiles. Use when asked to create, port or debug an OpenScript indicator, study, strategy, backtest script or .oscript file, including porting a study or strategy written for another charting platform, or a JavaScript chart indicator (an openalgo-charts descriptor) into a study that computes the same values. This is the OpenScript path, not the JavaScript chart-indicator path and not the Python openalgo.ta path.
argument-hint: "[what the study or strategy should do]"
allowed-tools: Read, Write, Edit, Bash, Glob, Grep
---

# OpenScript studies and strategies

Write a `.oscript` file and install it into the app's `openscript/` folder, where it
becomes a study or a strategy a trader can open in the editor, backtest on
`/trading`, and deploy on an instrument.

**Which path is this.** Three unrelated things in this repository are called
indicators, and they share nothing but the word:

- **OpenScript** (this skill) is a language, compiled to a program. Files are
  `.oscript`, and they can both draw and place orders.
- **`chart-indicator`** is a plain JavaScript descriptor on `openalgo-charts`,
  drawing only, loaded by the browser at runtime.
- **`openalgo.ta`** is a Python library used from the web's Python strategy
  host, scanners and backtests. The desktop has no Python runtime, so it has
  no such path.

If the request names a `.oscript` file, the OpenScript editor, or a strategy to
deploy on `/trading`, it is this one. If it is "add an indicator to my chart"
with no more said, it is probably `chart-indicator`.


## Desktop: where this differs from the web skill

This skill is the web repository's `openscript` skill, carried over for
OpenAlgo Desktop. The desktop pins the same `openalgo-script` (0.9.0, in
`package.json` at the repository root) and compiles in the page exactly as the
web does, so the library, pitfalls, strategies and examples apply unchanged.
What differs:

- **Install folder.** The web writes `strategies/openscript/` in its checkout.
  The desktop keeps scripts in the `openscript/` folder of its app data
  directory (`ScriptStore`, `src-tauri/src/trading/scripts.rs`; run logs in
  `openscript_logs/`): macOS
  `~/Library/Application Support/com.openalgo.desktop/openscript`, Windows
  `%APPDATA%\com.openalgo.desktop\openscript`, Linux
  `${XDG_DATA_HOME:-~/.local/share}/com.openalgo.desktop/openscript`.
- **The destination is explicit.** `--install` needs `--into appdata` (the
  folder above; the app must have run once) or `--into <dir>`. When testing
  the skill itself, always use `--into <scratchpad>/openscript`, never the
  trader's real data folder.
- **The same hash rule.** `scripts::source_hash` is `sha256:` over the text
  with a leading byte order mark removed and CRLF turned into LF, the
  compiler's own normal form, so a pair the validator writes is a pair the
  desktop's save route (`POST /openscript/{file}` in
  `src-tauri/src/server/routes/openscript.rs`) would accept. File names follow
  `trading::names::is_script_name`, the same rule `SAFE_NAME` checks.
- **Live runs** use the same engine in a hidden Tauri window
  (`openscript-runner.html`, `src-tauri/src/trading/runner/`), talking to the
  server through `/openscript/runner/host/{run}/*`, so backtest and live run
  the same compiled program.
- **Library lookup.** The scripts read `node_modules/openalgo-script` at the
  repository root; in a worktree under `.claude/worktrees/` without its own
  `node_modules` they use the main checkout's.
- **`run.mjs` needs no server.** It reads a bars file and runs the engine in
  Node. The desktop app answers `/api/v1/history` as the web does, so a
  trader's bars come from the app with their own API key. When testing the
  skill itself, build a bars file instead (a JSON array of bars with
  `timestamp` in seconds): never call a running app, and on a development
  machine port 5000 may be OpenAlgo web.

## The one rule

**Never write into the app's `openscript/` folder by hand.** That folder holds two
files per script, `<name>.oscript` and `<name>.oscript.program.json`, and **the
runner reads only the program.** It never opens the source.

That makes two silent failures possible, and both matter more than a compile
error:

- **A source with no program is a strategy that cannot run.** It saves, it opens
  in the editor, it appears in the list, and starting it is refused a minute
  later in a log nobody is reading.
- **A program built from different text is worse.** A run executes one script
  while a trader reads another.

So the two files are written together, from one compile, or neither is:

```bash
# 1. draft to a scratch file (never the openscript folder)
#    e.g. <scratchpad>/my_strategy.oscript

# 2. compile it the way the browser will
node .claude/skills/openscript/validate.mjs <scratch>/my_strategy.oscript

# 3. only on PASSED, install both files
node .claude/skills/openscript/validate.mjs <scratch>/my_strategy.oscript --install --into appdata

#    with a different installed name:
node .claude/skills/openscript/validate.mjs <scratch>/draft.oscript --install --into appdata --as mean-reversion.oscript
```

`--install` writes both files **only** when there are zero errors, and exits 1
otherwise. Before writing either, it checks that the hash the compiled program
records matches the source it is about to write beside it, which is the same
check the platform's own save route makes. A mismatched pair is refused and
nothing is written, so the failure cannot be half-applied.

## What a pass means

The compiler accepted the text: the names resolve, the types agree, the calls
exist, the limits hold. **Nothing here runs a single bar.** It does not mean the
strategy is any good, that its numbers are right, or that it will make money.

Some refusals wait for a bar, and a pass rules out none of them: a cell written
outside its grid is `OS4008` on the bar that writes it. `OS6025`, a bar handed
over with no time, is the platform's data and not the script: report the
instrument and timeframe rather than changing the script.

`reference/strategies.md` ends with the four steps between a compile and money,
and they are not optional.

## Run it on bars before installing

A pass cannot see what a script draws. Each of these compiled clean while
writing `examples/mtf-ema-crossover.oscript`, and each was found by running it:
a crossing level from weeks before that stretched the price scale until the
candles were a strip, and a read finer than the chart that drew nothing.
`run.mjs` runs the draft on real bars with the engine `/trading` ships, and
prints each plot's first value and last value and every marker with its bar.
It runs a study: it routes no orders, so for a strategy it says to backtest it
in the Backtest panel on `/trading` instead, which reports the trades:

```bash
# bars from the app's own /api/v1/history (127.0.0.1:5000 unless the port was
# changed in the app), with your API key, for the instrument and interval the chart shows
curl -s -X POST http://127.0.0.1:5000/api/v1/history -H 'Content-Type: application/json' \
  -d '{"apikey":"'"$OPENALGO_API_KEY"'","symbol":"RELIANCE","exchange":"NSE","interval":"5m","start_date":"2026-11-10","end_date":"2026-11-25"}' \
  > <scratch>/bars.json

node .claude/skills/openscript/run.mjs <scratch>/my_study.oscript <scratch>/bars.json \
  --symbol RELIANCE --interval 5m
```

Read the output against what the study is meant to do: a plot absent on every
bar, a first value much later than the warmup explains, a marker that never
fires, or a level far from the last close is a script to fix before it is
installed. `run.mjs` serves no bars beyond the file, so a read of another
instrument, or of an interval that is not a whole multiple of `--interval`, is
refused at load there and draws on a chart, which fetches them. A strategy run
live is served no more than `run.mjs` is, so a read refused here is refused
when the run starts too (`reference/pitfalls.md`).

## Read these before writing

| Page | What it is for |
|---|---|
| `reference/library.md` | every one of the 350 names, with what it answers and its warmup. Generated from the installed compiler. |
| `reference/pitfalls.md` | what goes wrong, with the diagnostic code you will actually see. Every code on it is proved against the compiler. |
| `reference/strategies.md` | the shapes a strategy takes, and which example answers which ask. |
| `reference/porting-from-javascript.md` | porting a JavaScript chart indicator into a study that computes the same values: the chart helpers whose OpenScript twin differs, absent values, `fn` scope, markers, tables, inputs, and how to check parity. |
| `examples/*.oscript` | seven worked files, all of which compile. The reasoning is in the comments. `mtf-ema-crossover.oscript` is the one for reading another timeframe. |

**Look a name up rather than recalling it.** 350 names is more than anyone holds,
and the dangerous failure is not reaching for a name that does not exist: it is
reaching for a neighbour that computes something else, which compiles and
reports a different strategy than the one that was meant.

## Porting from another chart language

The installed compiler carries an importer for the widely used chart language
whose scripts open with `//@version=5` or `//@version=6`. Each statement comes
across with its original meaning, or with a warning stating the difference, or
as a comment with an error, all in the range `OS9001` to `OS9012`, and what it
hands back has been compiled. Save the original as `<scratch>/original.txt`,
then from the repository root (the file is an argument, not standard input):

```bash
node --input-type=module -e "import { importScript } from './node_modules/openalgo-script/dist/core/index.js'; import { readFileSync } from 'node:fs'; const r = importScript(readFileSync(process.argv[1], 'utf8')); process.stdout.write(r.source); for (const f of r.findings) console.error(f.code, f.message)" <scratch>/original.txt > <scratch>/ported.oscript
```

The output is a draft, not a port. Read every finding: a warning marks a place
where the numbers can differ from the original, and a line kept as a comment is
behaviour that did not come across. Then take it through `validate.mjs` like
any other draft.

To port a JavaScript chart indicator (an `openalgo-charts` descriptor handed to
`registerIndicator`, with `calc(bars, settings)`) into a study, read
`reference/porting-from-javascript.md` first: there is no importer for that
direction, and several chart helpers share a name with a built-in that computes
something else.

## Three things that cost the most time

**Ninety five of the 350 names are planned and not implemented in this version.**
The library describes a name whether or not a version implements it, so a name
resolving is not a promise that it compiles. Reaching for one is `OS2020`. They
are marked in `reference/library.md`. `order.qtyForRisk` is the one most likely
to be reached for by accident, because it reads exactly like the call you want;
whole families are planned, including all of `leg.*` and all of `book.*`.

**`buy` and `sell` move a position by an amount; they do not set it to a side.**
`sell(qty = lots)` while long one lot leaves the position flat, not short.
Nothing refuses it. It is the first entry in `reference/pitfalls.md` and the
reason `examples/stop-and-reverse.oscript` exists.

**A value that is absent is not zero.** Every stateful call has a warmup, and
arithmetic on an absent value is absent rather than zero, which is what keeps a
signal from firing before its average exists. A script that substitutes a number
during warmup, with `orElse` or otherwise, is quietly wrong for its first stretch
of bars and looks like it works.

## Keeping the skill honest

The generator and two checks (the desktop CI does not run them; run them by hand after any change here or to the pin):

```bash
node .claude/skills/openscript/generate-reference.mjs   # rewrites reference/library.md
node .claude/skills/openscript/coverage.mjs             # must print COVERAGE COMPLETE
node .claude/skills/openscript/check-pitfalls.mjs       # must print PITFALLS VERIFIED
```

`coverage.mjs` holds the reference to the compiler in both directions (a name
the compiler has and the page lacks fails, and so does a name the page teaches
that the compiler does not have), checks that every planned name is marked, and
checks that each shape an author asks for is demonstrated in an example **that
compiles**, with the kind read from the compiled program rather than from the
text.

`check-pitfalls.mjs` compiles both halves of every entry in `reference/pitfalls.md`:
the wrong spelling must still produce the code named, and the fix offered must
still come out clean. It also checks that the page and the script name the same
set of codes, so neither can drift alone. Two claims on the first draft of that
page were wrong, and this is what found them.

This skill describes `openalgo-script` 0.9.0, the compiler `/trading` pins (the
web's server engine `openscript` is at the same version). What 0.9.0 changed
for a script, from its changelog: an intraday `req.timeframe` or `req.symbol`
read is counted from the trading day's open (the session's start, 09:15 on
NSE) instead of from midnight UTC, so a 10, 20, 30 or 60 minute read draws and
backtests different numbers than it did on 0.8.1, the ones that match the
broker's candles; and any whole number of minutes reads on an intraday chart
and in a backtest, finer or not a multiple of the chart's interval, built from
finer bars the page fetches. A live run is served only its own bars, so it
still refuses those reads at start (`reference/pitfalls.md`). No compiled
format change: programs compiled by 0.8.1 load unchanged. `reference/pitfalls.md`
has these under the timeframe entries, with the one 0.9.0 chart bug: a finer
read whose interval the chart's divides draws nothing when written with its
unit. When the pin moves past 0.9.0, read the changelog for what changed and
delete that bug's entry if the release carries the fix.

**Bumping `openalgo-script` means updating this skill in the same change**, the
way a chart bump does. Run the generator, run both checks, then update the prose
by hand: the generator owns the name table and nothing generates the teaching.
