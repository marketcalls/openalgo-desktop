# Performance and durability benchmarks

Measured results of the benchmarks for the roadmap's "Performance and
durability acceptance targets". Each benchmark runs the real services
(`AppState`, the HTTP server with Socket.IO, the 8765 feed server, the
event bus, the scalping monitor, the strategy module, Historify) with
`MockBroker` registered as `zerodha` and signed in through its OAuth
callback, a synthetic broker market socket and a broker order socket, in a
temporary data folder on ephemeral ports (never 5000, 8765, 5500 or 8766),
each alone in a child process.

## Targets

Measured on the supported floor hardware: an 8 GB Apple Silicon Mac (the
agents measure here) and a mid-range Windows laptop (the maintainer
measures there). The Raspberry Pi is out of scope (maintainer, 2026-10-16).
Log retention matches the web (maintainer, 2026-10-15): the order and
analyzer logs are kept forever, so the disk bound is the monitoring tables.

| Measure | Target | Benchmark |
| --- | --- | --- |
| Feed tick to risk decision (stop, target, trailing) | p99 under 50 ms at 1,000 subscribed symbols and 2,000 ticks/s | `perf::tick_to_risk` (scalping monitor and strategy module); `perf::report_load` and `perf::trading_pages` (the scalping monitor while the report pages read a large log, or the trading pages poll a long history) |
| Order-socket reconnect to book reconciled | under 5 s | `perf::reconnect` |
| Order-path SQLite wait while Historify imports | p99 under 20 ms | `perf::sqlite_wait` |
| Critical event delivery (order, position, risk topics) | zero drops under a 10x burst; market-data drops counted and shown | `perf::event_burst` |
| Memory and descriptors over 8 h | flat within 5% after warm-up | `perf::endurance` (15 min here; 8 h by the maintainer) |
| Disk growth | monitoring tables bounded; growth reported in health | `perf::disk_growth`, `perf::endurance` |

## Running them

All are `#[ignore]`d and never run on pull requests. From `src-tauri`:

```
# every benchmark, one at a time (about 30 minutes in a debug build)
cargo test --locked --test it perf:: -- --ignored --nocapture --test-threads=1

# one of them
cargo test --locked --test it perf::tick_to_risk -- --ignored --nocapture

# the endurance run for 8 hours, samples written as they are taken
OPENALGO_PERF_MINUTES=480 OPENALGO_PERF_OUT=$PWD/endurance.jsonl \
  cargo test --locked --test it perf::endurance -- --ignored --nocapture
```

On this Mac every cargo command goes through the build lane
(`.lane/cargo-lane.sh`), as for any other test.

Each measurement prints one line:

```
PERF bench=<name> <key>=<value> ... build=debug os=macos arch=aarch64 cpus=8 mem_gib=8.0 load1=5.62 target=<what> verdict=<pass|miss>
```

`load1` is the one-minute load average when the line was printed: the
development Mac is shared with other agents' builds, so it says how much
else ran. With `OPENALGO_PERF_OUT=<file>` the same fields are appended to
the file as one JSON object per line. A benchmark asserts its target after
printing, except the two reporting gaps listed under follow-ups, which
print `verdict=miss` without failing the run until they are closed.

`.github/workflows/perf.yml` runs them nightly at 03:00 IST on
ubuntu-22.04 and on demand (`workflow_dispatch`, with the endurance length
and a test filter as inputs), and uploads `perf-results` (the JSON lines
and the log); the run summary lists the `PERF` lines.

The faults the endurance run injects, and the maintainer's drill on a real
install with the expected outcome of each fault, are in
[`fault-drill.md`](fault-drill.md). `scripts/perf/sample_app.py` samples a
running install for the 8-hour run.

## Layout

| File | What it measures |
| --- | --- |
| `tests/it/perf/mod.rs` | percentiles, the `PERF` line, the JSON record, process memory |
| `tests/it/perf/rig.rs` | the app under load: synthetic broker market socket (paced ticks, stamped ticks, drops), broker order socket, running clock |
| `tests/it/perf/tick_to_risk.rs` | tick to stop, target and trailing decisions |
| `tests/it/perf/report_load.rs` | the same decisions while four browsers keep reloading the report pages over a large log |
| `tests/it/perf/trading_pages.rs` | the same while four browsers keep reloading what the trading pages poll over a long history |
| `tests/it/perf/browsers.rs` | the browsers those two run |
| `tests/it/perf/reconnect.rs` | order-socket drop to order book reconciled |
| `tests/it/perf/sqlite_wait.rs` | order-path writes and `/api/v1/placeorder` while Historify imports |
| `tests/it/perf/event_burst.rs` | critical-lane delivery under a 10x burst; market-data drops |
| `tests/it/perf/disk_growth.rs` | monitoring tables and `logs.db` over repeated overfills; sizes in health |
| `tests/it/perf/endurance.rs`, `faults.rs` | the timed run with faults |
| `tests/it/perf/lag_log.rs` | ticks the app's consumers skipped, counted from their log lines (the app keeps no count) |

## Results

### 2026-10-20, macOS 14 (M1, 8 GB), debug build, branch `pr/benchmarks` on master `92689307`

The Mac is shared with other agents' builds, so every line carries the
one-minute load average (`load1`); 8 cores. These are debug builds: the
shipped release build is faster, so a pass here is a pass there, and a miss
is checked against the cause before it is called a product miss.

| Measure | Target | Measured | Verdict |
| --- | --- | --- | --- |
| Tick to decision, scalping monitor (300 legs: 100 stops, 100 targets, 100 trailing) | p99 under 50 ms | p50 4.2-5.3 ms, p99 10.1-18.3 ms, max 22-139 ms over 400 decisions, none lost (load 4.7-8.9) | pass |
| Tick to decision, strategy module (300 live runs, one leg each) | p99 under 50 ms | on master: p99 24.6 ms at load 4.6, 115.8 ms with four extra busy cores, 345 ms at load 3.9 in another run, 1,879 ms with 4 of 400 decisions lost at load 9.3; with the fix on `pr/strategy-tick-path` (`cee33a75`): p99 3.6 ms at load 4.6, 19.4 ms with four extra busy cores, none lost, no tick folded or missed | miss on master; pass with the fix |
| Order-socket reconnect to book reconciled | under 5 s | 0.31-0.50 s over 6 drops (each after a connection that lived 31 s): the reconnect backoff's first wait (250-500 ms) plus one order-book pass; a fill made while the socket was down was applied every time. A flapping line (drops 1 s after each reconnect): 0.61, 1.09, 3.13 s | pass |
| Order-path SQLite wait while Historify imports | p99 under 20 ms | stop save to `openalgo.db`: p99 3.29 ms (max 26.6 ms) while 3 imports of 200,000 rows ran (20,000 rows/s), against p99 1.24 ms without; `/api/v1/placeorder` p99 6.6 ms. Before the IMMEDIATE fix (PR #160, `fix(db)`) the save failed with "database is locked" within 30 s, imports or not | pass (after the fix) |
| Critical event delivery | zero drops under a 10x burst | 11,000 events (100/s, then 1,000/s for 10 s, then 100/s), every one delivered to the critical-lane probe: no critical drop, panic, closed queue or fact loss; deepest critical queue 36 of 10,000; delivery p99 0.28-0.89 ms; no best-effort entry shed | pass |
| Market-data drops | counted and shown | a feed client reading one frame every 2 ms missed 84% of 2,000 ticks/s (the feed keeps the latest per symbol, by design). Before the NEW-REL-05 follow-up the counts lived per connection and vanished with it, and health showed none; with it (`pr/health-feed-losses`), `/health/api/current` `market_data` showed the 28,367 folded frames, and each broker feed's updates and per-reader skips since it connected | miss on #160; pass with the follow-up |
| Disk growth: monitoring tables | bounded | six cycles writing 20% over every cap: every table at its cap after housekeeping; `logs.db` 99 MB after the first cycle, 141 MB after the second, then flat (141,227 to 141,255 KiB) | pass |
| Disk growth: reported in health | sizes and growth in health | every database's size, the sandbox's included since PR #160 (`fix(health)`); with the NEW-REL-05 follow-up, `database.growth` adds each store's growth and daily rate over the monitoring window (DuckDB's `.wal` counted) | miss on #160; pass with the follow-up |
| Memory and descriptors | flat within 5% after warm-up | 15-minute smoke (the harness check; 3-minute warm-up, 12 faults injected, every one handled as expected, no activity or API failure): memory 44,370 to 47,650 KiB (+7.4%), still rising about 15 MB an hour at the end; descriptors 50 to a peak of 53, pool 8, live tasks 45 to 47; data folder +34 MB an hour (the request log at 20 calls a second, until its 100,000-row cap). A 60-minute run (6-minute warm-up, 54 faults, every one as expected, no activity failure): memory 44,530 KiB at the warm-up, 49,138 at minute 30, then 49,922 from minute 50 to 60; the first half after the warm-up grew 4.6 MB, the second 0.8 MB: a plateau about 30 minutes in, so +12.1% over a 6-minute warm-up and +1.6% from minute 30. Live heap allocations (macOS `heap`) grew 0.40 MB from minute 13 to 30 and 0.13 MB from 30 to 45 while the footprint grew 2.8 and 0.7 MB: caches filling and the allocator keeping freed pages, not a line. SQLite's own memory stayed between 1.5 and 6.8 MB. Descriptors 50 to 55 with the pool 4 to 8 (two per connection), live tasks 47 throughout. 35 of about 72,000 `/api/v1` calls did not answer 200 in that run, most likely the harness's own catch-up bursts after a stall tripping the 100-a-second limit; it now paces its calls without bursts and records failures by status, and a 30-minute run with that change had none (27 faults, every one as expected) | harness proven; plateau within the hour; 5% verdict for the maintainer's 8-hour run (48-minute warm-up) |

The strategy module's miss on master: one task read the market-data
broadcast and awaited every held tick's evaluation, which scanned every
active run and, for the run holding the symbol, made two synchronous
SQLite reads and copied the strategy and the run (0.18 ms a tick with 30
runs, 0.29 ms with 180, 0.39 ms with 300, measured by direct calls); under
CPU contention it queued, and at load 9 the broadcast overran it and
skipped the deciding ticks (logged at debug, never counted). The slow
decisions were spread over the run, not clustered at the reconciler's
5-second pass. Capacity on master at load 3.5: 30 legs p99 6.7 ms, 90 legs
5.3 ms, 180 legs 15.5 ms. The trading agent's fix (`pr/strategy-tick-path`)
keeps no database on the tick path, splits a reader from the evaluator with
each symbol's prices folded so a spike is still decided, and counts queued,
folded and missed ticks in health; this benchmark is its acceptance check.

A first 45-minute run found a harness race, not an app defect: each
scalping round sent its breach tick once, and for symbols the SDK client
did not hold, the tick could reach the synthetic broker socket before the
monitor's subscription did, so the round never exited and every later
round failed. The harness now repeats the breach tick until the exit is
placed, as a market that fell through a stop keeps ticking under it; that
run's memory numbers are not used.

### 2026-10-20, report pages under load, branch `pr/report-routes-blocking` on `ac7129d3`

The roadmap's "Async-worker DB reads": the report pages read SQLite and
shaped and serialized their answers on the async workers that carry the
market data feed and the risk owners. `perf::report_load` runs the
scalping rig (1,000 symbols, 2,000 ticks/s, 90 managed legs, 120
decisions) over a log grown large (100,000 requests in the traffic log,
its cap, and 200,000 order log entries) while four browsers reload
`/traffic/api/logs?limit=1000`, `/traffic/api/stats`, `/logs?page=1` and
`/logs?page=1&search=SYM0001`, each as soon as its last answer came. Debug
build, load 3-4.

| Run | Tick to decision | Ticks the app received | Pages |
| --- | --- | --- | --- |
| Same rig, no pages (`perf::tick_to_risk` with 30 legs per kind) | p99 5.57 ms, none lost | 2,014/s of 2,000 | none |
| Before (`ac7129d3`) | all 120 decisions lost (none within 10 s) | 486/s of 2,046: the feed's socket reader starved, so the deciding ticks queued behind a growing backlog | 503 answers, p50 107 ms, p99 946 ms |
| After | p99 5.03 and 4.12 ms (two runs), none lost | 2,013/s | 330 answers, p50 87-98 ms, p99 576-592 ms |

The fix moves every Web UI report read off the async workers: the Traffic
page (requests, statistics), the Logs page, the Health page (history,
statistics, alerts, export), the Security page (data, statistics, login
activity, ban by host), the Diagnostics error log (list, statistics,
groups), the MCP audit log and the Analyzer page and export. The largest
answers are serialized where they are read (`webui::json_text`): a week of
health samples serialized on the async worker still held it for up to
0.46 s on a loaded machine. The Traffic statistics (19 scans of the log,
refreshed every 30 s by every open page) are computed once for concurrent
refreshes (`ReportFlight`; the Latency report shares one per group,
`KeyedFlight`). Regressions:
`webui_reports::report_pages_over_a_large_log_do_not_hold_the_runtime`
(two of each page at once: the runtime was held for 2.1-3.4 s at once
before, 16-26 ms after at load 16-21) and
`webui_reports::traffic_stats_refreshes_share_one_computation` (four
refreshes held all four logs connections without the single flight, one
with it).

### 2026-10-20, trading pages under load, branch `pr/sync-reads-elsewhere` on `59a1fe1d`

The reads outside the report pages (follow-up 6 below). `perf::trading_pages`
runs the same scalping rig (90 legs, 120 decisions) over a year of history,
every table kept forever as on the web: a strategy with 25,000 orders over
250 runs, an OpenScript deployment with 25,000 orders, 10,000 Action Center
orders, 500,000 analyzer log entries and 100,000 Telegram and WhatsApp
commands. Four browsers keep reloading the strategy's three books, the
deployment's positions and orders, the Action Center, `/api/v1/analyzer`,
the Telegram page and the WhatsApp statistics. Debug build, load 3.6-5.6.

| Run | Tick to decision | Ticks the app received | Pages |
| --- | --- | --- | --- |
| Before (`59a1fe1d`) | all 120 decisions lost | 207/s of 2,000 | p50 496 ms, p99 3.1 s |
| After | p99 4.89 and 6.82 ms in two runs, none lost; a first run had p99 69.5 ms from one 240 ms decision 6.1 s in, not seen again | 2,013/s | p50 288-363 ms, p99 2.0-3.9 s |

Each page alone over that history, after (debug): Action Center 710 ms,
Telegram page 340 ms, a strategy book 280-330 ms, `/api/v1/analyzer` 270
ms, a deployment book 220-240 ms, WhatsApp statistics 154 ms; all of it on
the blocking pool now.

The fix moves these reads off the async workers: the strategy module's
books, orders, runs, events, webhook calls, checkpoints, list and detail and
`/api/v1/strategy` list, runs, orders and events (`strategy::Store::blocking`);
a deployment's books and status; the Action Center; the analyzer status's
count; the Telegram page, `/api/v1/telegram/stats` and the WhatsApp
statistics. The reads a page repeats while it polls are shared
(`report_flight::KeyedFlight`): the Detail page's three books read the
strategy's history once, a deployment's books its ledger once, and the
Action Center's reloads that wait on a read share the next one (each answer
read after its reload asked, so a reload after an approval shows it). Migration 095 adds an
index on `whatsapp_command_logs.executed_at`, whose statistics scanned the
whole log. Regressions in `polled_reads` (before: the runtime held 11.4 s
by the strategy pages, 5.6 s by the Action Center, 4.5 s by the bots'
statistics, 2.7 s by a deployment's books and 1.5 s by the analyzer status
while the logs database was busy; with the single flight disabled, 3, 3 and
4 reads where 1, 1 and 2 are expected).

### Follow-ups

1. **Strategy tick path**: fixed on `pr/strategy-tick-path` (`cee33a75`,
   trading follow-ups agent); measured above. Once it lands, the nightly
   run holds it.
2. **Market-data drops shown in health**: done in the NEW-REL-05
   follow-up (`pr/health-feed-losses`): every reader of a broker feed
   (strategies, scalping stops, the sandbox, the relay to apps, the order
   update relay) reports what it skipped; health's `market_data` block
   shows, per feed since it connected, the updates received and each
   reader's skips, and what apps on the 8765 feed were not sent (folded,
   dropped, skipped); Run diagnostics adds the same in a sentence. The
   Health page itself still shows none of the API's blocks (a UI
   follow-up).
3. **Data folder growth in health**: done in the same follow-up:
   `database.sizes_mb` and `database.growth` (size, growth and daily rate
   per store and in total over the monitoring window). Also not yet drawn
   on the Health page.
4. **Release-build numbers**: every figure here is a debug build; a release
   run of `perf::` on the M1 and the maintainer's Windows laptop would give
   the shipped numbers.
5. **The weekly soak's one-time memory step** (soak run 39090876797): the
   CI-and-evidence agent is tracing it in `docs/audit/soak.md`; the endurance
   run measures after its warm-up.
6. **Synchronous reads outside the report pages**: done on
   `pr/sync-reads-elsewhere` (measured above): the strategy module's books
   and history, the OpenScript runner's books and status, the Action
   Center, the analyzer status and the bots' statistics.
