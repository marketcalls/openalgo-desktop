# OpenScript on the charting terminal

OpenScript is an open trading language. You write a study once, plot it on the
`/trading` chart, backtest it, and run the same file as a strategy. OpenAlgo
Desktop uses the same compiler as OpenAlgo web (`openalgo-script` 0.9.0, pinned
in `package.json`), so a script written for one works in the other.

It is not a replacement for the custom indicators described in
[Custom chart indicators](custom-indicators.md). The two sit side by side and
are meant for different uses; [Choosing between the two](#choosing-between-the-two)
says which is which.

- [Writing one](#writing-one)
- [Where your scripts live](#where-your-scripts-live)
- [Why this is safer than a custom indicator](#why-this-is-safer-than-a-custom-indicator)
- [Choosing between the two](#choosing-between-the-two)
- [What is stored](#what-is-stored)
- [Backtesting](#backtesting)
- [Running one as a strategy](#running-one-as-a-strategy)
- [The routes](#the-routes)
- [Differences from OpenAlgo web](#differences-from-openalgo-web)
- [Known gaps](#known-gaps)
- [Writing scripts with an AI assistant](#writing-scripts-with-an-ai-assistant)

## Writing one

Open the chart at `/trading` and choose **Scripts** on the right rail. The
panel is an editor, a compiler and the chart:

- **New script** (in the panel's menu) asks for a **Name** and a **Kind**,
  study or strategy, and gives you a working script to edit rather than an
  empty file.
- **It compiles as you save.** **Save**, or Ctrl+S (Cmd+S on a Mac), saves
  the script. If it does not compile, the console under the editor says what
  is wrong and on which line, in the compiler's own words.
- **It saves either way.** A script that does not compile is still saved,
  without a compiled program beside it: you can keep editing it, and nothing
  will run it until it compiles.
- **Apply to chart** puts a study on the chart; for a strategy it opens the
  **Backtest** panel. It is available once the script is saved and compiles.
- Unsaved edits are kept per script while you switch between them, and the
  menu has **Discard unsaved changes** and **Delete script**.

The compiler runs in the page; there is no compiler in the Rust server. The
page sends the compiled program with the source when it saves, and that
program is what a backtest and a live run execute.

## Where your scripts live

In the `openscript` folder of the OpenAlgo Desktop data folder:

| System | Folder |
| --- | --- |
| Windows | `%APPDATA%\com.openalgo.desktop\openscript` |
| macOS | `~/Library/Application Support/com.openalgo.desktop/openscript` |
| Linux (x64 or ARM64) | `~/.local/share/com.openalgo.desktop/openscript` |

A saved script is up to three files:

| File | Holds |
| --- | --- |
| `<name>.oscript` | The source, as you wrote it |
| `<name>.oscript.program.json` | The compiled program, when the script compiles |
| `<name>.oscript.bak` | The source as it was before the last save |

Deleting a script removes all three. A name may hold letters, digits, dots,
dashes and underscores, must start with a letter or a digit, and may be up to
64 characters before `.oscript`. A source may be up to 256 KB and a program up
to 512 KB. The folder is backed up with the rest of the data folder when you
copy it (README,
[Where your data lives, and backups](../README.md#where-your-data-lives-and-backups)).

## Why this is safer than a custom indicator

A custom indicator is JavaScript that the page imports and runs, with your
signed-in session: it can call `/api/v1`, read your positions and place
orders. An OpenScript file is not code the page runs. It is text a compiler
turns into a list of instructions, and an engine walks that list. A script can
only name what the language gives it (bars, arithmetic, things to draw and,
in a strategy, order instructions). There is no way to spell a network call, a
file or a reach into the page.

- **A script from someone else is a much smaller decision.** A study can draw
  a wrong line on your chart; it cannot place an order or send your positions
  anywhere. A strategy can ask for orders, but only through a run you start,
  on the instrument and in the mode you chose.
- **It needs no change to the security policy.** The app serves pages with
  scripts restricted to its own origin. Sources are served as plain text and
  programs as JSON, and nothing in the app imports or evaluates either.

## Choosing between the two

| You want | Use |
| --- | --- |
| A study on your own chart, written quickly and safely | OpenScript |
| A study to share with someone who should not have to trust you | OpenScript |
| A strategy you backtest and then run Live or in sandbox mode | OpenScript |
| Full access to the charting library's drawing and data API from inside an indicator | A custom indicator |
| Something the language cannot express yet | A custom indicator |

## What is stored

Two rules keep a compiled program from ever running instructions that do not
match the source you read:

- **A save that carries no program removes the program that was stored.**
  Losing a program costs one recompile; keeping the wrong one could cost a
  trade.
- **A save is refused when the program was built from other text.** The
  program records the hash of its source (`sha256:` over the text with a
  leading byte order mark removed and CRLF turned into LF), and the server
  recomputes it over the source it was handed.

The program is removed before the source is replaced and written after it,
and every write is atomic, so an interrupted save leaves either the source
with its own program or the source with none. The program's format, the
canonical encoding and how an engine reads it are the language's own
specification and are described in OpenAlgo web's
`docs/openscript.md`, "What is stored, and how a runner reads it"; the
desktop stores the same bytes the same way.

## Backtesting

A strategy is backtested in the **Backtest** panel of `/trading`:

1. The page compiles the script (a study is refused: only a strategy places
   orders).
2. It reads the bars with the API's `history` call, using your API key. A
   range over 100,000 bars is refused. A read the chart's bars cannot answer
   (fifteen or three minutes on a ten minute chart, or another instrument
   with `req.symbol`) gets its bars the same way before the run starts, at
   the interval the engine builds it from (five minutes for a fifteen minute
   read on a ten minute chart).
3. It runs the program in a Web Worker, one per backtest, so the chart stays
   responsive; the result comes back to the panel and the worker ends. A
   backtest that takes more than 60 seconds is given up, and you can stop one
   yourself. If the computer cannot start a worker, the same backtest runs on
   the page instead.

The backtest uses the same engine and the same compiled program as a live run,
so the two cannot drift.

## Running one as a strategy

A strategy that compiles can run on its own, on one instrument, placing
orders through OpenAlgo's own order path. Runs are managed in the
**Strategies** panel on the right rail of `/trading`, one row per deployment
(a script can have several deployments, each on its own instrument).

### Before it starts

**Settings** on a row saves what the deployment runs on: the symbol, the
exchange, the bar interval, optionally the product (CNC, NRML or MIS; left
empty, the script's own declaration decides) and the script's inputs. An
unknown field is refused by name. The start button reads **Start live** or
**Start in sandbox**, after the mode OpenAlgo is in, and is disabled until an
instrument is set.

A start carries nothing else, so a run started from the panel and a run
started by a schedule cannot differ. It is refused, with a sentence saying
why, when the script has no compiled program, nothing is saved against it, it
is already running, the analyzer setting cannot be read, or (for Live) no
broker is connected or OpenAlgo cannot tell which broker account it is
signed in to.

### What runs

OpenAlgo opens a hidden window for the run: no window on screen, nothing in
the taskbar. It runs the compiled program on the same engine as the chart and
the backtest. The window holds no API key and no session; it talks only to the
run's own routes, with a secret it is given when it opens.

- **Bars.** The run first replays five days of history, sending nothing, so it
  starts knowing the recent past and holding nothing. Then OpenAlgo reads the
  history every 10 seconds and hands the run each new bar. Orders are sent
  only on a confirmed bar: once a newer bar has arrived, or the clock has
  passed the bar's end. A run does not act on individual ticks.
- **Other timeframes.** A run is given its own bars only, so a
  `req.timeframe` read must be a whole multiple of the deployment's interval
  (twenty or thirty minutes on a ten minute deployment). Fifteen or three
  minutes, which the chart and the backtest build from finer bars, end the
  run as it starts, with the read and the interval in its log; the web's
  runner refuses them too. Intraday reads count from the session open, 09:15
  on NSE, as on the chart.
- **Orders.** OpenScript order types map to the platform's: market to
  MARKET, limit to LIMIT, stop to SL-M, stop-limit to SL. A quantity in lots is
  multiplied by the contract's lot size from the master contract; only whole
  numbers are sent, and a quantity in money is refused.
- **Its own books.** Each deployment has its own order book, trade book and
  positions, shown under its row.

### Where the orders go

The side a run trades on is fixed when it starts:

- **Sandbox**: every order goes to the sandbox engine.
- **Live**: every order goes to your broker through the same order path as the
  API, marked as already decided (`force_live`), so turning analyzer mode on
  while the run holds a position never sends its exits to the sandbox. In
  Semi-Auto order mode its orders wait in the **Action Center** like any
  other, and the run follows each one there: placed, rejected or not known.

A Live run belongs to the broker account it started on. If OpenAlgo is later
signed in to another broker or account, the run's orders are not sent there.
Started again on another account, a deployment begins with nothing there and
never counts, cancels or closes what it holds at the first one: its start
log, its status and its Stop answer say what it holds elsewhere ("This
strategy also holds ... through X, from an earlier run there"). Sign in to that
account and Stop it there, or close the position at your broker.

### Pause and Stop

- **Pause** ends the run and leaves its position exactly where it is, for you
  to manage.
- **Stop** asks first, then closes what the run holds and ends it. It halts
  the run, cancels its working orders and waits until the broker (or the
  sandbox) says each is finished, then sends the exit for what the run's own
  fills say it holds, never more than the account actually holds in that
  contract, and watches it for 25 seconds. If the exit has not filled by
  then, the row shows **close pending**: press Stop again, or Pause.

### When an order's answer is lost

If an order was sent and no definite answer came back, the run is never told
"rejected": the order is unconfirmed, nothing new is sent (exits included)
and OpenAlgo asks about the same batch again after 1, 2 and 5 seconds, then
every 10 seconds. A batch sent twice is answered from its record and never
placed twice. After about a minute the row shows **order unconfirmed** and the
Health page raises an alert. In Live, an order the broker never shows is held
until you mark it **Not placed** on the Health page
([Health monitoring](health-monitoring.md#unconfirmed-orders)); in sandbox
mode one missing from the book is treated as not placed.

### Schedules

A deployment can be given IST times to start and stop by itself, with days
(Monday to Friday when none are given). A scheduled start needs OpenAlgo to
be open and signed in, and a trading day on the run's exchange. A start found
more than five minutes late (the computer was asleep or OpenAlgo was closed)
is not run, only logged.

### What survives a restart

Runs live only while OpenAlgo is open and you are signed in. Logging out,
quitting and installing an update pause every run, and nothing restarts a run
by itself afterwards: start it again from the panel (or let its schedule).
Quitting asks first when runs are active
([Troubleshooting](user/troubleshooting.md#quitting-openalgo-and-when-the-computer-sleeps)).
Orders that were sent and never answered are marked unconfirmed at the next
start and settled from the broker's order book.

### Logs

Each run writes a log to the `openscript_logs` folder of the data folder,
named `<deployment>_<YYYYMMDD_HHMMSS>_IST.log`, up to 5 MB each. The status
route names the folder and the newest 20 files; there is no log viewer in the
app, so open the folder to read them.

## The routes

All need the signed-in session, except the run window's own routes, which
need the run's secret.

| Method and path | Does |
| --- | --- |
| `GET /openscript/index.json` | Your scripts, with size, time and whether a program is stored |
| `GET /openscript/{name}.oscript` | One script's source, as plain text |
| `GET /openscript/program/{name}.oscript` | The compiled program stored beside it, as JSON, byte for byte |
| `POST /openscript/{name}.oscript` | Creates or replaces one, from `{"source": "...", "program": "..."}` (`program` optional) |
| `DELETE /openscript/{name}.oscript` | Removes it, its backup and its program |
| `GET /openscript/instrument` | Facts about an instrument for the editor and the panel |
| `POST /openscript/runner/start/{name}` | Starts it; answers at once with the run's identity, never an outcome |
| `POST /openscript/runner/pause/{name}`, `.../stop/{name}` | Pause: ends the run, keeps the position |
| `POST /openscript/runner/close/{name}` | Stop: closes the position, then ends the run |
| `GET /openscript/runner/status`, `.../status/{name}` | What runs, what is scheduled, what each script runs on, the log folder |
| `GET`, `POST`, `DELETE /openscript/runner/config/{name}` (and `GET .../config`) | The run settings |
| `POST`, `DELETE /openscript/runner/schedule/{name}` | The schedule (`start_time`, optional `stop_time`, optional `days`) |
| `GET /openscript/runner/instruments`, `.../intervals` | What can be chosen |
| `GET /openscript/runner/orderbook/{name}`, `.../tradebook/{name}`, `.../positions/{name}` | A deployment's own books |
| `/openscript/runner/host/{run}/{spec,bars,inbox,intents,log,ended}` | Used only by the run's hidden window, with its secret |

## Differences from OpenAlgo web

| | OpenAlgo web | OpenAlgo Desktop |
| --- | --- | --- |
| Where scripts live | `strategies/openscript/` in the install | `openscript/` in the data folder |
| What runs a live strategy | A Python process (`openscript_host`) with the engine | A hidden window per run with the same TypeScript engine as the chart |
| Run settings and schedules | JSON files under `strategies/` | Tables in `openalgo.db` |
| Destination | Read per order from the analyzer switch; a run whose destination changes stops itself | Fixed at start; a Live run's orders carry `force_live` |
| Orders held in the Action Center | Not followed | Followed to placed, rejected or not known |
| An order with no answer | Treated as failed | Kept pending, asked about again, never placed twice |
| Account | Not recorded | Each order records its broker, account and run; other accounts' positions are reported, never touched |
| Logs | `log/strategies/` | `openscript_logs/` in the data folder |

## Known gaps

As of this writing:

- The note at the bottom of the **Strategies** panel is the web's text. It
  says a run "is a process on the server" that closing the browser does not
  stop, and that a run "stops itself if it changes" when the analyzer
  setting changes. In the desktop, quitting OpenAlgo pauses every run, and a
  run's side is fixed at start, so turning analyzer mode on or off does not
  stop it or move its orders.
- There is no log viewer in the app; read the files in `openscript_logs`.

## Writing scripts with an AI assistant

The repository's `openscript` skill (`.claude/skills/openscript/SKILL.md`)
writes a script, compiles it the way the page does and installs the source
and its program together, only when it compiles. Its references
(`reference/library.md`, `strategies.md`, `pitfalls.md`) are the language
guide for this compiler version; the language's own specification and
diagnostics are maintained with `openalgo-script`.
