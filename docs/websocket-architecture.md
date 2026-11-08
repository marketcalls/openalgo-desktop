# WebSocket architecture

How market data travels in OpenAlgo Desktop: from your broker's feed to the
app's own screens, to the sandbox, strategies and scalping stops, and to your
own scripts on port 8765. Written for traders first, with the developer
detail toward the end.

The protocol your scripts speak on port 8765 is the web's, unchanged:
[WebSocket streaming](api/websocket-streaming/) and
[prompt/websockets-format.md](prompt/websockets-format.md) describe the
messages. Where the desktop answers differently, on purpose, is listed in
[Moving from OpenAlgo web](user/moving-from-openalgo-web.md#the-market-data-feed).

- [The 30-second version](#the-30-second-version)
- [Why it is built this way](#why-it-is-built-this-way)
- [The big picture](#the-big-picture)
- [Who reads the feed](#who-reads-the-feed)
- [One subscription, many readers](#one-subscription-many-readers)
- [When a reader is slow: folding and skips](#when-a-reader-is-slow-folding-and-skips)
- [Order updates](#order-updates)
- [Socket.IO pushes](#socketio-pushes)
- [Limits on port 8765](#limits-on-port-8765)
- [Reconnects, sign-outs and sleep](#reconnects-sign-outs-and-sleep)
- [Differences from OpenAlgo web](#differences-from-openalgo-web)
- [Developer reference](#developer-reference)

## The 30-second version

OpenAlgo connects to your broker's live market feed **once**. Inside the app
that one feed is handed to everyone who needs prices:

1. **The screens**: charts, watchlists, the option chain and quote panels.
2. **The app's own automation**: the sandbox engine filling your orders,
   strategy stops and targets, scalping stops.
3. **Your own programs** on `ws://127.0.0.1:8765`: Python scripts, Amibroker,
   Excel, anything written for OpenAlgo web.

Adding a reader does not add load at your broker: an instrument is
subscribed at the broker once, however many readers want it. A slow reader
never holds up the others; it gets the newest price instead of a backlog.

On the web this needs three processes (the Flask app, the WebSocket proxy and
a ZeroMQ bus between them). In the desktop it is one process, and the bus is
replaced by bounded in-memory channels.

## Why it is built this way

Indian brokers limit their live feeds: usually one or two connections per
login and a few thousand instruments across them, and some drop
subscriptions silently past the limit. If every part of OpenAlgo opened its
own connection, the chart would compete with your stop-loss watcher and your
data-capture script with both. So OpenAlgo keeps one connection to the broker
per session and shares it.

## The big picture

```
                    Your broker's live feed
                              |
                              v
             Broker feed adapter (one per broker)        brokers/<broker>/streaming.rs
             translates frames to OpenAlgo ticks
                              |
                              v
             WebSocketManager (one socket, one task)      websocket/manager.rs
             reconnects, re-subscribes, counts readers
                              |
             bounded broadcast, 4,096 updates
         +--------------+-----+---------+------------------+
         |              |               |                  |
         v              v               v                  v
     Sandbox       Strategy        Scalping           Feed bridge             feed/bridge.rs
     engine        stops and       stops              (one subscription
                   targets                             per instrument and mode)
                                                           |
                                              bounded broadcast, 8,192 updates
                                                           |
                                                           v
                                              Feed server on port 8765    feed/server/
                                              one bounded outbox per client
                                                 |                  |
                                                 v                  v
                                         The app's screens    Your programs
                                         (MarketDataManager)  (SDK, Amibroker, Excel)
```

The broker sees one consumer. Everything else taps in downstream.

## Who reads the feed

| Reader | How it gets prices | Notes |
| --- | --- | --- |
| The app's screens | Port 8765, like any other client | The page's `MarketDataManager` (`src/lib/MarketDataManager.ts`) asks the server for the feed address and the API key, then authenticates. After repeated connection failures it falls back to polling quotes over REST. |
| Your programs | Port 8765 | The web protocol: authenticate, subscribe, modes 1/2/3 (LTP, Quote, Depth), depth 5/20/30/50 |
| Sandbox engine | The broker manager's broadcast, directly | Fills resting orders from ticks; quotes symbols whose feed has been quiet for 30 seconds ([Sandbox mode](user/sandbox.md)) |
| Strategy module | The broker manager's broadcast, directly | Stops, targets and trailing for strategy legs; a symbol with no tick for 10 seconds is quoted over REST every 2 seconds |
| Scalping stops | The broker manager's broadcast, directly | Each tick drives one evaluation of the stops on that symbol |
| OpenScript live runs | Not the feed | A run reads bars from history every 10 seconds ([OpenScript](openscript.md#running-one-as-a-strategy)) |

No market data goes over Socket.IO.

## One subscription, many readers

When the chart, a watchlist and your Python script all want NIFTY:

- The broker is subscribed **once** per instrument, at the highest mode
  anyone asked for (an LTP and a Depth subscriber make one Depth
  subscription at the broker).
- The feed server keeps its own count per instrument, mode and depth. It asks
  the bridge for a subscription when the first client subscribes and gives it
  up when the last one unsubscribes or disconnects, so one client leaving
  never cuts another off.
- The sandbox, strategies and scalping hold their own references on the
  broker manager, so closing every chart never stops a stop-loss watcher.

A full tick reaches Quote subscribers; LTP subscribers get the price fields;
Depth subscribers get the depth snapshot with the last tick's quote fields.
Index ticks reach every mode. A client that holds several modes of one
symbol gets one frame per mode for each tick, as the web's recorded
transcripts show. A broker that serves deeper books on a separate socket
(Fyers 50 levels, Dhan 20) gets a second manager for depth.

Within one broker connection the bridge keeps each instrument's last known
quote and merges every broker message into it by what the message says it
carries, so a message that repeats only the price keeps the rest, and a
field the broker never sent is left out of the frame rather than sent as 0.
What each broker's frames carry, and the per-mode delivery policy, are in
[Market data frames](port/market-data-frames.md).

## When a reader is slow: folding and skips

Markets burst: an expiry, a circuit, the open. A reader that cannot keep up
must not slow the broker feed or the other readers, and must not build an
ever-growing queue. Three mechanisms handle it.

**1. Folding, per client on 8765 (by design).** Each client has its own
outbox with one slot per instrument and mode. If a newer price for an
instrument arrives before the client has read the previous one, the newer one
replaces it. A slow client therefore always receives the **latest** price for
every instrument it watches, never a backlog of stale ones. Depth is folded
the same way. Folding loses nothing a trader needs: the next frame carries
the current state.

**2. Dropped frames, per client on 8765 (a fence).** The market lane of an
outbox holds at most three slots per allowed subscription (9,000). Past
that, the oldest instrument's pending frame is dropped. With the
3,000-subscription limit per client this is a last line of defence, not an
expected event.

**3. Skipped updates, inside the app.** The channels between the broker
manager and its readers, and between the bridge and the feed server, are
bounded broadcasts. A reader that falls more than a channel's length behind
is told it lagged and skips ahead to the newest update (Rust's `Lagged`). It
then acts on the next update: a stop checks the next price, a sandbox order
fills on the next tick. A skip is a price that reader did not see, not a lost
order.

The strategy module splits reading from acting (`strategy/tick_feed/`): a
reader task takes ticks off the broadcast at once and decides every price of
a held symbol there and then, through the risk core in the run's lock, so
each stop, target, trailing stop and profit lock is judged on every price in
the order it came. Only acting on a decision (placing an exit, stopping the
run, recording the event) waits for the evaluator task, which combines a
run's decisions while it is busy. The reader never waits on the database:
a run's fixed inputs are read before its prices are subscribed, and what
the reader finds missing is read in a task of its own. The reader used to
fold each symbol's waiting prices to the first, lowest, highest and latest
past 32, which lost a trailing stop's earlier peak and the dip after it
(PROD-REL-01). Prices the reader itself missed are counted and raise a
`strategy_ticks_missed` health alert.

**Seeing it.** PR #178 (`origin/pr/health-feed-losses`) counts all of it:

| Count | Where | Resets |
| --- | --- | --- |
| Updates received, per broker feed | `market_data.broker_feeds[].received_since_connect` | At each new broker connection |
| Updates skipped, per reader (strategies, scalping stops, sandbox orders, apps on the feed, order updates) | `market_data.broker_feeds[].skipped_since_connect` | At each new broker connection |
| Frames folded, dropped and skipped for apps on 8765 | `market_data.feed_clients` (`coalesced`, `dropped`, `skipped`) | When OpenAlgo restarts |

They are in `GET /health/api/current` and in the broker stream lines of
**Run checks** on the Diagnostics page ("500 updates since it connected; 40
reached a reader that had fallen behind and were skipped (scalping stops 30,
apps on the market data feed 10). Each acts on the next update."). Folds alone
never mark the feed as a warning; a skip or a drop does. See
[Health monitoring](health-monitoring.md#market-data-freshness-and-skips).
Without #178 the skips are written to the log only.

What to do when skips keep appearing: subscribe fewer instruments, close the
pages and programs you do not use, and keep other heavy work off the computer
in market hours.

## Order updates

Order updates are never folded or dropped on the way to a client: they go
through each client's control lane, in order, with acknowledgements and
errors. A client that lets that lane fill (1,024 frames) is not reading at
all and is disconnected (close 1008) rather than having updates thrown away.

Updates reach the feed through the event bus (`order.update`), relayed by
`feed::orders::OrderRelay` to clients that sent `subscribe_orders`. On the
way an update can still be lost when the app is overloaded (a full
best-effort bus queue, a relay that fell behind), and updates from an earlier
broker session are dropped on purpose. The app's own books do not depend on
them: the [order reconciler](architecture.md#the-order-reconciler) reads the
broker's order book every few seconds while anything is open and settles what
an update did not.

## Socket.IO pushes

The app's pages also hold a Socket.IO connection to the HTTP port (5000),
for events rather than prices. It accepts only signed-in browser sessions
and checks the session again on every push. The page connects to its own
address with long polling (`src/hooks/useSocket.ts`).

| Event | Sent when | From |
| --- | --- | --- |
| `order_event`, `order_notification`, `modify_order_event`, `cancel_order_event`, `close_position_event`, `gtt_event` | An order, modify, cancel, close or GTT operation completed or failed | `events/subscribers/socketio.rs` |
| `order_update` | A broker or sandbox order changed state | `events/subscribers/socketio.rs` |
| `analyzer_update` | A sandbox fill, auto square-off or T+1 settlement, and other analyze-mode changes | `events/subscribers/socketio.rs` |
| `analyzer_mode_changed` (desktop only) | Analyzer mode was turned on or off from anywhere: the app, the API, MCP or Telegram | `events/subscribers/socketio.rs` |
| `master_contract_download`, `cache_loaded` | The symbol list downloaded or loaded | `events/subscribers/socketio.rs` |
| `force_logout`, `active_sessions_update` | A session ended, or the broker session changed | `events/subscribers/socketio.rs` |
| `pending_order_created`, `pending_order_updated` | Action Center (Semi-Auto) changes | `events/subscribers/socketio.rs` |
| `historify_progress`, `historify_job_*`, `historify_schedule_*` | Historify downloads and schedules | `events/subscribers/socketio.rs` |
| `strategy_snapshot`, `strategy_delta`, `strategy_event`, `strategy_order_update`, `strategy_run_update`, `strategy_terminal` | Strategy module changes, to the room `strategy:{id}` a page joins with `strategy_subscribe` | `strategy/broadcast.rs` |
| `scalping_sl_update` | A scalping stop moved (trailing) or changed | `scalping/monitor/` |
| `whatsapp_status`, `whatsapp_qr`, `whatsapp_pair_code`, `whatsapp_paired`, `whatsapp_pair_status` | WhatsApp pairing and status | `messaging/` |
| `update_status` (desktop only) | The in-app updater's progress | `services/update/` |

The pages refresh on these pushes; they do not poll. Every route and event
the frontend uses, page by page, is in
[the frontend to backend contract](port/frontend-backend-contract.md). Health
alerts are not pushed; they are on the Health page only.

## Limits on port 8765

The web reads without limits; the desktop protects itself and your trading
from a misbehaving client:

| Limit | Value | What happens |
| --- | --- | --- |
| Authenticate within | 15 seconds | Close 4401 "auth timeout", as on the web |
| One message | 1 MiB | Close 1009 |
| Messages per connection | 6,000 burst, 1,000 a second; 8 MiB burst, 2 MiB a second | Close 1008 "too many messages" |
| Subscriptions per client | 3,000 | The subscribe is refused with an error frame |
| Connections | 64 from this computer; 256 in all; 32 per network address | Further connections are refused |
| Silence | A ping every 20 seconds; no frame from the client for 40 seconds | The connection is dropped |

Subscribing every instrument in one message, or one per message as a script
loop does, stays far inside these. Connections from other devices or through
a tunnel are classified when they connect, with the same rules as the HTTP
server: the address and the `Host` and `Origin` headers are checked, and a
wrong API key counts against that caller's failure budget.

## Reconnects, sign-outs and sleep

- **Broker reconnects** are handled once, in the broker manager: a 15-second
  connect timeout, the old socket closed before a new one opens, capped
  backoff with jitter (from half a second up to 30 seconds), a watchdog for a
  feed that goes silent for 90 seconds, and every subscription sent again on
  reconnect. A broker that refuses the session is not retried until you sign
  in to it again. Clients on 8765 stay connected through a broker reconnect.
- **Sign-outs close the feed.** When the API key cache is cleared (a new API
  key, a broker session that ended or started, a logout), every
  authenticated client gets the web's `AUTHENTICATION_ERROR` frame and close
  4401, and connects and authenticates again. The web keeps such clients
  connected; the desktop does not, so a key from before a sign-out cannot
  keep streaming.
- **Locked keys.** After OpenAlgo starts, and after a logout, `authenticate`
  answers with the error frame "Sign in to OpenAlgo..." until you sign in.
- **Sleep and wake.** When the computer wakes, OpenAlgo reconnects the broker
  sockets at once.
- **Another port.** Changing the market data port in Server Settings moves
  the feed server by itself; if the port is taken, the app raises a health
  alert and keeps trying.
- **Screens after a reconnect.** The page's `MarketDataManager` merges a tick
  only into fields from the same connection (TOOLS-09), so an old depth never
  comes back stamped as current after a reconnect.

## Differences from OpenAlgo web

| | OpenAlgo web | OpenAlgo Desktop |
| --- | --- | --- |
| Pieces | Broker adapters, a ZeroMQ bus on port 5555, the WebSocket proxy on 8765, a `MarketDataService` for the backend | One process: the broker manager, bounded channels, the feed server on 8765; backend readers subscribe to the manager directly |
| Broker connections | A pool of up to 3 connections of 1,000 symbols each | One connection per feed (market, depth, order updates), up to 3,000 instruments |
| Slow clients | Every tick forwarded; no server-side throttle in current code (the web's own architecture page still says 50 ms) | Every tick forwarded; a slow client's queue folds to the latest price per instrument |
| Strategy readers | A 10,000-tick FIFO that sheds | Every price decided on arrival; only the actions wait, with missed prices counted |
| Losses | Logged | Counted and shown in health (PR #178) |
| Client limits | None | Message size, message budget, connection pools, handshake checks |
| After a sign-out or new key | Clients stay connected | Clients are closed with 4401 |

## Developer reference

| Piece | Where |
| --- | --- |
| Broker feed contract | `brokers/common/streaming.rs`: `BrokerFeed`, `FeedEvent`, `NormalizedTick` (`derive_change`), `NormalizedDepth` |
| Broker adapters | `brokers/<broker>/streaming.rs`, `brokers/families/{noren,xts}/streaming.rs`; non-WebSocket transports (IIFL Capital's MQTT, Nubra's order socket) through `brokers/common/relay.rs` |
| Broker socket manager | `websocket/manager.rs` (`TICK_CHANNEL_CAP` 4,096), started and stopped per session by `services/broker_runtime.rs` |
| Feed server | `feed/mod.rs` (`FeedService`), `feed/server/` (listener, connection, session, message budget), `feed/protocol.rs` and `feed/protocol/market.rs` (the `market_data` frame), `feed/dialect.rs` and `feed/dialect/names.rs` (per-broker keys) |
| Subscriptions | `feed/registry.rs` (per client, reference counts at the source), `feed/bridge.rs` (one manager reference per key), `feed/bridge/snapshot.rs` (each instrument's merged quote) |
| Per-client queue | `feed/outbox.rs` |
| Order updates | `feed/orders.rs` (`OrderRelay`) |
| Socket.IO | `server/socketio.rs`, `events/subscribers/socketio.rs` |
| Frontend | `src/lib/MarketDataManager.ts`, `src/hooks/useMarketData.ts`, `src/hooks/useSocket.ts`; the feed address from `GET /api/websocket/config` |
| Tests | `src-tauri/tests/it/feed_conformance.rs` (the 8765 contract), `feed_behaviour.rs`, `feed_budget.rs`, `feed_hygiene.rs` and the other `feed_*` suites; the WebSocket transcripts recorded from the web in `tests/fixtures/web/websocket/` |

To check a broker's feed end to end without writing a client, use the app's
own pages: `/websocket/test` (market data; `/20`, `/30`, `/50` ask for those
depths) and `/websocket/order` (the order update stream).
