# Health monitoring

OpenAlgo Desktop watches its own process while it runs: open files, memory,
database connections, the broker feeds, orders whose outcome is not known,
and records it meant to save. This page says where to look, what each figure
means in the desktop, and what to do when something is flagged.

In the desktop, health is about one app on your own computer. OpenAlgo web's
health monitor is built for a server (load balancer, Kubernetes and Docker
health checks, thresholds set in `.env`); none of that applies here, and the
health pages are open only to you, signed in.

- [Where to find it](#where-to-find-it)
- [The Health page](#the-health-page)
- [When the page cannot read something](#when-the-page-cannot-read-something)
- [Unconfirmed orders](#unconfirmed-orders)
- [Alerts](#alerts)
- [Run checks (Diagnostics)](#run-checks-diagnostics)
- [Market data: freshness and skips](#market-data-freshness-and-skips)
- [Strategy prices and exits](#strategy-prices-and-exits)
- [Database growth](#database-growth)
- [Thresholds](#thresholds)
- [How it works (contributors)](#how-it-works-contributors)
- [Differences from OpenAlgo web](#differences-from-openalgo-web)
- [Known gaps](#known-gaps)

## Where to find it

| Page | How to open it | What it is for |
| --- | --- | --- |
| **Health Monitor** (`/health`) | **Logs**, then the **Health Monitor** card | The running figures, alerts and orders waiting for your decision |
| **Diagnostics** (`/admin/diagnostics`) | **Admin**, then the **Diagnostics** card | System information, the **Run checks** probes, recent errors and a report to download when you ask for help |

## The Health page

The page title is "System Health Monitor". It refreshes itself every 10
seconds while it is on screen (**Auto: ON**; choose it to pause), and
**Refresh** reloads at once. **Export CSV** downloads the samples as
`health_metrics.csv`.

OpenAlgo takes one sample a minute, so a figure can be up to a minute old.
The banner at the top shows **System Status** (PASS, WARN, FAIL or UNKNOWN)
and when the last sample was taken. A sample older than two minutes shows
UNKNOWN, never PASS: it means OpenAlgo has not measured recently (it was
asleep, or busy), not that all is well. The status is FAIL while an
order-update gap is open (see [Alerts](#alerts)), whatever the other
figures say. When there are no samples yet (just after the app starts) the
page says so.

The five cards, as the desktop measures them:

| Card | What it counts in the desktop |
| --- | --- |
| **File Descriptors** ("Process Handles" on Windows, where there is no fixed limit) | Files, sockets and database files the app holds open |
| **Memory Usage** | The app's resident memory, and its share of the computer's memory |
| **Database Connections** | Pooled connections to `openalgo.db` and `logs.db` |
| **WebSocket Connections** | Whether the broker's market data feed is connected (0 or 1), and how many instruments it streams. Apps connected to your feed on port 8765 are not counted here |
| **Active Threads** | The app's threads. On Windows the count is not available |

Below them, the desktop's own checks, one card each:

| Card | What it shows |
| --- | --- |
| **Data Files** | Each database's size and how fast it grows (see [Database growth](#database-growth)) |
| **Market Data** | Each broker feed's updates since it connected; the prices strategies, scalping stops and sandbox orders missed, what apps on your feed missed, and what they got as a newer price instead (see [Market data](#market-data-freshness-and-skips)) |
| **Broker Feed Connections** | Each broker feed's state, how often it reconnected (and how often because the broker went quiet), and when and why it last did (see [Broker feed connections](#broker-feed-connections)) |
| **Order Tracking** | The order book checks, unconfirmed orders, order updates lost, and each open order-update gap with its mode, start and cause |
| **Records Saved** | Order log and monitoring records that could not be saved |
| **Strategy Prices** | The prices strategy risk management read, combined while busy, and missed |

A card turns amber or red, with a sentence saying what it means, when
something was missed or lost. Then: six-hour charts of open files and
memory, minimum, average and maximum figures, a status summary, the app's
own memory under **Top Memory Processes**, **Unconfirmed Orders**,
**Active Alerts** and the last 20 samples under **Recent Metrics**.

## When the page cannot read something

The page reads five things on each refresh: the latest figures, the
history for the charts, the statistics, the alerts and the unconfirmed
orders. Each is kept on its own and shown as soon as it is read, so one
that fails or is slow does not hide or hold back the others, and nothing
that was on screen disappears because a read failed. A read that has not
answered within 8 seconds counts as failed (its request is closed), so a
stalled OpenAlgo is marked before the next refresh tries again:

- What was read before stays on screen, marked in its own card "Could not
  refresh ... at 10:15:20 am IST; shown as read at 10:14:10 am IST.": the
  checks, both charts, the statistics, **Top Memory Processes**, **Thread
  Details**, **Recent Metrics**, the alerts and the unconfirmed orders.
  Times are in IST, with the date when it is not today.
- While the latest figures could not be refreshed, the banner shows
  UNKNOWN, or the WARN or FAIL it last read, as "FAIL when last read at
  10:14:10 am IST": a failed refresh never hides bad news, and never
  carries a PASS forward.
- Something never read says so in its card ("Could not read the alerts.
  OpenAlgo tries again every 10 seconds while Auto is on, or choose
  Refresh."), and a list never read is not shown as an empty table or
  "No ... available". The checks a trader acts on (Market Data, Order
  Tracking, Records Saved, Strategy Prices) show "Not known", never a
  pass.
- A failed read changes nothing in OpenAlgo: an unconfirmed order stays
  held until it is found or you mark it not placed.

OpenAlgo web reads all four of its answers together, so one failure left
every figure as it was with only a message, and its page has no
unconfirmed orders.

## Unconfirmed orders

An order is unconfirmed when OpenAlgo sent it to your broker and got no
definite answer: the connection dropped after sending, the broker answered
with a server error, or it said "success" without an order id. OpenAlgo
never places such an order again, so it cannot be placed twice. It keeps
looking for it in the broker's order book, by tag where the broker reports
one, otherwise by symbol, side, quantity and type.

In Live mode an order that never shows up is not released on its absence
alone: the position it belongs to (a strategy, a scalping stop, an
OpenScript run) stays held, a critical alert is raised once, and the order is
listed under **Unconfirmed Orders** as "{action} {quantity} {symbol}
({exchange})", with the time since it was sent.

What to do:

1. Check your broker's order book and trade book (in the broker's own app or
   website) for that order.
2. If the broker has it, wait: OpenAlgo finds it on its next look and the row
   goes away.
3. If the broker does not have it, choose **Not placed**, then **Confirm not
   placed**. OpenAlgo reads the order book and trade book once more first,
   and keeps the order held if either shows it (or cannot be read). Then:
   "Marked as not placed. The position is managed again."

If the page cannot read this list, it never shows it as empty: orders it
listed before stay, marked with when they were read, and a list it has not
read (or that was empty when last read) shows **Unconfirmed Orders: not
known**. Check the broker's order book until a refresh succeeds. Only a list
read now and empty shows nothing.

Without your decision, OpenAlgo releases a Live order by itself only with
corroboration: absent for 10 minutes, no matching trade, no unowned working
order for the same instrument and side, and the position exactly what it was
before the order was sent. In sandbox mode an order missing from four reads
over at least 20 seconds is released. The rules and the per-broker details
are in [Order placement outcomes](port/order-outcomes.md).

## Alerts

Each alert has a severity (warn or fail), a value and a message that names
the cause. Raising the same kind again updates the open alert's time and
value instead of adding a second one, and the open alert takes the new
message. **Acknowledge** marks an alert as seen; it stays listed until its
cause clears. An alert nothing clears by itself has **Resolve**: choose it
once you have read it (OpenAlgo web defines the resolve call but its page
never offers it).

| Alert | Severity | Raised when | Clears |
| --- | --- | --- | --- |
| Open files, memory, database connections, threads (`fd_count_*`, `memory_rss_mb_*`, `db_connections_total_*`, `thread_count_*`) | warn or fail | A sample is over the level in [Thresholds](#thresholds) | When the figure drops back under the warning level |
| `order_facts_gap` | fail | Order updates from the broker may have been missed | By itself, once the order book has been read again |
| `order_unconfirmed_held` | fail | A Live order has no definite outcome (see [Unconfirmed orders](#unconfirmed-orders)) | When the order is found, or marked not placed |
| `openscript_orders` | fail | A Live OpenScript run's orders have had no answer for over a minute | By itself, once the answer comes |
| `scalping_protection` | fail | A scalping stop needs you: its exits could not be saved or read back, the position book does not show the position, or it was set on another broker account ([Scalping](scalping.md#the-protection-rules)) | Not by itself |
| `market_data_port_in_use`, `market_data_feed_failed` | fail | The market data feed for your apps (port 8765) could not start | By itself, once it starts |
| `master_contract_unavailable` | fail | The day's symbol list could not be loaded | By itself, once it loads |
| `records_not_saved` | warn | Order, analyzer, traffic, latency or security records were not saved (a full queue or a failed write) | Not by itself: the counts only grow until OpenAlgo restarts |
| `strategy_ticks_missed` | warn | Strategy risk management missed prices because the feed came faster than it was read. Stops and targets act on the next price | Not by itself |
| `historify_wal_recovered` | warn | OpenAlgo did not close cleanly and some unsaved Historify changes could not be recovered ([Troubleshooting](user/troubleshooting.md#historify-says-some-changes-could-not-be-recovered)) | Not by itself |
| `ban_list_full`, `banned_devices_on_network` | warn | The Security page's ban list is full, or devices on your network were banned | See **Logs**, **Security Logs** |

Alerts are shown only on this page. There is no pop-up or Socket.IO push for
them, so look at the page after anything unusual, and before you leave
automation running.

## Run checks (Diagnostics)

On the Diagnostics page, **Run checks** under **Latency & Connectivity
Probes** runs these and shows Status, Check, Latency and Detail for each:

| Check | What it proves |
| --- | --- |
| Database read (openalgo.db), Database read (logs.db) | The two databases answer a read |
| App and API listener | Port 5000 (or yours) accepts a connection |
| Market data feed listener | Port 8765 (or yours) accepts a connection |
| Broker session (`<broker>`) | A broker is signed in |
| Market data from the broker | With a broker session: whether its feed is connected, how many instruments it streams, and how long ago the last update came |
| Order updates from the broker | Only while the broker's order-update socket runs: the same for that socket |

The page also shows the host, runtime, build, brokers, configuration, the
databases with their sizes, and recent errors. **Download .md** and
**Download .txt** save the same information as a report (at most 1 MB) to
attach when you ask for help on Discord or GitHub; it includes the records
not saved since start.

## Market data: freshness and skips

For each broker feed (the market data feed; the order-update socket and,
for brokers that stream a deeper book (Fyers, Dhan), the market depth
socket, each while it runs or once anything it carried was skipped):

- since its current connection began: whether it is connected and for how
  long, how long ago the last update arrived (freshness), and how many
  updates arrived;
- per reader inside the app, since OpenAlgo started and since the
  connection began: how many updates it skipped because it had fallen
  behind. The readers are strategies, scalping stops and sandbox orders
  (each skip is a price a stop, a target or a fill never saw; each acts on
  the next price), apps on the market data feed, and order updates (the
  order book is read again). A skip is not a lost order.

The losses count since OpenAlgo started, so a feed that reconnects does not
hide what was missed before; only freshness is per connection.

For the apps connected to port 8765 (your scripts, Amibroker, Excel), since
OpenAlgo started: how many frames were folded (a slow app gets the newest
price for an instrument instead of a backlog, by design: shown as "Replaced
by a newer price", not as a loss, and never a warning), dropped (past a slow
app's queue) or skipped (the feed server itself fell behind). These counts
outlive each app's connection. How this works is in
[WebSocket architecture](websocket-architecture.md).

Where it shows:

- **Run checks** adds a sentence to the broker stream lines, for example
  "12345 updates since it connected, none missed." or "500 updates since it
  connected; 40 reached a reader that had fallen behind and were skipped
  (scalping stops 30, apps on the market data feed 10). Each acts on the next
  update."
- The Health page's **Market Data** card.
- `GET /health/api/current` carries a `market_data` block: `broker_feeds`
  (per feed, `name` being `market_data`, `order_updates` or `market_depth`:
  `connected`, `connected_for_s`, `last_update_age_s`,
  `received_since_connect`, `skipped_since_connect` and
  `skipped_since_start` by reader, with their totals) and `feed_clients`
  (`coalesced`, `dropped`, `skipped`). Its `status` is `warn` when anything
  was skipped or dropped since OpenAlgo started; folded frames alone do not
  warn.

What to do when skips keep appearing: subscribe fewer instruments (on the
charts and watchlists, and in your scripts), close apps on port 8765 you do
not use, and keep other heavy programs off the computer during market hours.
The counts go back to zero only when OpenAlgo restarts.

## Broker feed connections

For each broker feed listed under Market data, its `broker_feeds` entry
carries `connection`: `broker`, `state` (`connected`, `connecting`,
`reconnecting`, `disconnected`, `auth_failed`), `connects`,
`disconnects` (connections that ended or attempts that failed, each
followed by a new attempt), `stalls` (of those, the ones where the broker
went quiet) and `last_reconnect` (`reason` and `at`). The reasons are
`connect_failed`, `closed`, `silent` (nothing arrived for 90 seconds),
`data_stopped` (the connection answered but prices stopped),
`heartbeat_unanswered` (a keep-alive check went unanswered),
`not_taking_frames`, `login_not_confirmed` and `woke` (the computer slept).
The counts run from OpenAlgo's start and start again when another broker's
feed replaces this one.

The **Broker Feed Connections** card is red when the broker refused a feed
(log in to the broker again) and amber while a feed reconnects or for 15
minutes after it did, saying why in plain words. A feed that keeps
reconnecting because the broker went quiet or left a keep-alive check
unanswered points at the network between this computer and the broker.

Each feed sends the keep-alive the web sends to the same broker (most ping
every 30 seconds and expect an answer within 10; order update sockets every
20), so a broker that is quiet but connected is not taken for dead. Feeds
that reach their broker through a relay inside the app (AliceBlue, IIFL
Capital, Nubra order updates) hear the broker's answers to the relay's
keep-alive the same way.

## Strategy prices and exits

The strategy module decides every price of a held symbol as it arrives and
places what it decided (exits, stops) after that. `GET /health/api/current`
carries a `strategy_ticks` block, shown as the **Strategy Prices** card:

- `ticks_queued` (**Prices read**), `ticks_folded` (**Combined while
  busy**: decisions combined while an exit was being placed; every price is
  still decided) and `ticks_missed` (**Missed**: prices the reader never
  saw because the feed came faster than it read; each raises
  `strategy_ticks_missed`), all since OpenAlgo started;
- `exits_waiting` (**Strategies with exits waiting**): strategies with an
  exit or stop decided and not yet placed;
- `oldest_exit_wait_s` (**Longest exit wait**): how long, in seconds, the
  oldest of those has waited, or `null` when none waits; `exits_slow` is
  true once it has waited 15 seconds or more.

The block's `status` is `warn` while any price was missed or an exit has
waited 15 seconds. An order waits at most 10 seconds for its turn at the
broker and is normally answered within a second or two after that, so an
exit that late is held by a broker that does not answer or a database that
is slow. Check that strategy's positions with your broker.

## Database growth

OpenAlgo keeps the order and analyzer logs forever, as
OpenAlgo web does, so the data folder grows with your trading. Every sample
records the size of each database, side files included:

| Store | File |
| --- | --- |
| openalgo | `openalgo.db` (account, settings, strategies, the symbol master) |
| logs | `logs.db` (order, analyzer, traffic, latency, security and health records) |
| sandbox | `sandbox.db` (sandbox mode) |
| historify | `historify.duckdb` (Historify market data) |

`GET /health/api/current` reports `database.growth` over two windows: every
sample kept (up to seven days) and, under `last_day`, the last 24 hours.
Each has every store's size, how much it grew, how much a day, and the
total. The daily rate needs a day of samples: over a few hours it would
stretch one trading session's writes over a whole day.

The **Data Files** card shows each store's size with its daily average over
the samples kept, says over how many days, and how much all the data grew
in the last day (in the last 12 hours, say, when OpenAlgo ran for only part
of the day). With less than a day of samples it shows the growth itself
("+0.4 MB in 5 h"), and with less than an hour only the sizes. There are no
warning levels for growth. The sizes are also on the Diagnostics page under
**Databases** and in the downloaded report.

If the folder grows faster than your disk can take, export what you want to
keep from **Logs** and from **Historify**, and delete Historify data you no
longer use. Where the data folder is: README,
[Where your data lives, and backups](../README.md#where-your-data-lives-and-backups).

## Thresholds

Fixed in the app (`services/health_service.rs`), not settings:

| Figure | Warning | Critical |
| --- | --- | --- |
| Open files (macOS, Linux) | 700 | 900 |
| Process handles (Windows) | 5,000 | 10,000 |
| Memory | 500 MB | 1,000 MB |
| Database connections | 10 | 20 |
| WebSocket connections | 10 | 20 |
| Threads | 200 | 400 |
| A strategy exit decided and not yet placed (`strategy/tick_feed/pending.rs`) | 15 seconds | none |

The thread levels are higher than the web's (50 and 100): the Rust runtime
keeps a worker thread per processor core and a pool for blocking work.

## How it works (contributors)

- **Sampling.** `services/health_service.rs` takes a sample every 60 seconds
  on a blocking thread, stores it in `logs.db` (`health_metrics`: kept seven
  days, at most 20,000 rows) and raises or resolves the threshold alerts.
  `GET /health/api/current` takes one at once when there is none yet.
- **Alerts** live in `logs.db` (`health_alerts`). `db::sqlite::monitor`'s
  `raise_alert` reuses the open alert of the same `alert_type`; a resolved
  alert is deleted after seven days, with at most 1,000 rows. A new kind of
  alert is a new `alert_type` raised where the condition is known, with its
  own rule for clearing.
- **Routes** (`server/routes/health.rs`): `/health/status`, `/health/check`,
  `/health/api/current`, `/health/api/history` and `/health/api/stats`
  (hours clamped to 1 to 168, default 24), `/health/api/alerts` with
  `.../{id}/acknowledge` and `.../{id}/resolve`, `/health/api/held-orders`
  and `/health/api/held-orders/not-placed`, `/health/export`. All need the
  signed-in session. `/health` and `/health/status` answer 503 only on fail.
- **Diagnostics** (`services/system_info.rs`, `system_info/streams.rs`):
  `POST /admin/api/system/diagnostics` runs the checks; `GET
  /admin/api/system/report` renders the report.
- **Records not saved** (`services/record_loss.rs`) and **strategy ticks**
  (`services/strategy_ticks.rs`) count what the web loses silently: log
  entries shed by the event bus's best-effort lane or the monitor's queue,
  and prices the strategy tick reader missed.
- `services/feed_losses.rs` (the `market_data` block and the Run checks
  sentence), `websocket/manager/losses.rs` (skips per reader, since start
  and per connection), the feed server's per-app counts in `feed/outbox.rs`
  and `feed/source.rs`, and `services/store_sizes.rs` (sizes and growth
  windows).
- The page: `src/pages/HealthMonitor.tsx` (carried over from the web), with
  the desktop's parts in `src/components/desktop/health/`: each read kept on
  its own (`useHealthReads.ts`), the cards (`AppHealthCards.tsx`), the
  unconfirmed orders and the marks for what could not be read.

## Differences from OpenAlgo web

| | OpenAlgo web | OpenAlgo Desktop |
| --- | --- | --- |
| Who can read it | `/health` and `/health/check` are open, for load balancers and probes | Every health route needs the signed-in session; no health endpoint is public |
| Thresholds | Set in `.env` | Fixed in the app; threads 200/400 instead of 50/100 |
| Sampling | Every 10 seconds into `db/health.db` | Every minute into `logs.db` |
| WebSocket figure | Connections to the proxy | Whether the broker feed is connected |
| Order outcomes | Not monitored | Unconfirmed orders, the order-update gap, held positions |
| Lost records and missed prices | Not counted | Counted, with an alert |
| Feed skips and database growth | Not reported | Reported, skips since start, growth over the last day and the samples kept |
| Feed reconnects | Logged only | Counted per feed, with the last reconnect's cause and time |
| A read that fails | Every figure stays with only a message | Each read kept on its own; what could not be refreshed is marked, never emptied |
| Deployment health checks (ELB, Kubernetes, Docker) | Documented | Not applicable: there is no server to probe |

## Known gaps

As of this writing:

- **Active Threads** shows no stuck threads and **Thread Details** stays
  empty: the desktop does not measure them. **Top Memory Processes** lists
  OpenAlgo Desktop only.
- Run checks' broker stream sentence counts skips since the feed connected
  only; the Health page has the counts since OpenAlgo started.
