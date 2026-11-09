# Broker feed corpus

Each case here is a broker's raw upstream frames and a subscription history.
`tests/it/feed_corpus` pushes the frames from a fake broker socket (an
ephemeral loopback port) through the broker's real desktop decoder (its
`BrokerFeed`), the real `WebSocketManager`, `BrokerBridge` and the public
8765 feed server, and compares every `market_data` and `order_update` frame
a client receives with the case's expected frame **exactly**: every key,
every number with its JSON type, field presence (a field the expected frame
leaves out must be absent), timestamps and book levels. The frames the feed
sends the broker (subscribe, mode change, unsubscribe, login) are compared
exactly too. This is the oracle audit C09 asked for; the transcript tests in
`tests/it/feed_conformance.rs` still pin the control protocol.

Where a case has the web's output for the same raw frames (`web.json`,
below), every key on which the desktop's frame differs from the web's must
be a deviation the case declares, with its reason.

Run one broker or all: lane-test with filter `test(/feed_corpus::zerodha::/)`
or `test(/feed_corpus/)`; locally `cargo test --locked --test it feed_corpus`
(through the lane script).

## A case folder

```
feed_corpus/<broker>/
  broker.json      what the web offers for the broker (modes, depths, orders)
  <case>/
    case.json      what the case is, its symbol master, provenance, deviations
    script.jsonl   the steps, one JSON object per line
    frames/        raw frames the fake broker sends (.hex, .bin, .json, .txt)
    expected/      frames a client must receive (.json)
    web.json       the web's output for the same frames (generated)
```

The folder name is the test's name: `feed_corpus::<broker>::<case>`.

### broker.json

What the broker's web adapter offers, for the parity matrix
(`docs/port/feed-parity-matrix.md`, written by
`scripts/feed_corpus/parity_matrix.py`): `web.ltp`, `web.quote` (true or
false), `web.depth` (the levels, or the levels per exchange as the web's
capability registry lists them), `web.orders` (how the web streams order
updates, or false), with `sources` (the web files read) and `notes`. A
mode, depth or order stream the web offers and no case covers shows in the
matrix as **no case**; one it does not offer shows as not offered by the
web, with the cases that show the desktop refusing it too; a broker with
`"orders": false` has no order scenarios to cover.

### case.json

```json
{
  "broker": "dhan",
  "summary": "One sentence: what the frames are and what must come out.",
  "scenarios": ["first-full-packet", "partial-packet"],
  "instruments": [
    {"symbol": "RELIANCE", "exchange": "NSE", "token": "2885",
     "brsymbol": "RELIANCE", "brexchange": "NSE_EQ"}
  ],
  "depth_levels": {"NSE": [5, 20]},
  "upstream_ignore": ["ping"],
  "provenance": {
    "web_sha": "a1b9e32506bce21403a8614dddeed0ab61fd519a",
    "method": "web-test",
    "sources": ["broker/dhan/streaming/dhan_adapter.py::DhanWebSocketAdapter._normalize_5depth_data"],
    "notes": ["Anything a reviewer needs to trust the values."]
  },
  "deviations": [
    {"key": "ltt", "kind": "web-defect", "reason": "...", "ref": "dhan_adapter.py:900"}
  ]
}
```

- `instruments`: the symbol master rows the feed and the bridge share.
  `symbol`, `exchange` and `token` are required; `brsymbol` and `name`
  default to the symbol, `brexchange` to the exchange, `lotsize` to 1,
  `instrumenttype` to `EQ`, `tick_size` to 0.05, `expiry` empty, `strike` 0.
- `depth_levels` (optional): the levels the broker streams per exchange, as
  its `Broker::feed_depth_levels` says (default 5 everywhere).
- `upstream_ignore` (optional): text frames the fake broker does not record
  (a feed's text heartbeat). WebSocket pings are never recorded.
- `deep_book_timeout_ms` (optional): the broker's deep-book silence timeout
  (Dhan's 30 s, `broker_runtime::deep_book_timeout`, audit P-10) shortened
  to this, so a case can pass it; only with `with_broker` and for a broker
  that has one.
- `http` (optional): what the fake broker REST API answers from the start,
  a list of `{"method", "path", "status", "body", "record"}` (see "The fake
  broker API").
- `scenarios`: from this list, for the parity matrix: `first-full-packet`,
  `partial-packet`, `absent-field`, `depth-update`, `book-clear`, `index`,
  `derivative-oi`, `reconnect`, `subscription-lifecycle`, `mode-change`,
  `order-open`, `order-partial-fill`, `order-complete`, `order-rejected`,
  `order-cancelled`. The modes and depths a case covers come from its
  script's subscribes.
- `provenance`: where the expected values come from. `web_sha` is the web
  commit read; `method` is `web-test` (the inputs and values of a web test,
  named in `sources`), `web-script` (computed by
  `scripts/feed_corpus/web_expected.py`) or `by-hand` (worked from the web
  file and line named in `notes`); `sources` names the web files and
  functions (`file::Class.method`) or `file:line`.
- `deviations`: see below; `invariants` (optional): see "Expected frames".

### script.jsonl

One step per line. Every step may carry a `"note"` for the reader;
`{"step": "note", "text": "..."}` is a comment line.

| Step | Keys | What it does |
| --- | --- | --- |
| `send` | `client`, `message`, `reply` | Client `client` (connected and authenticated on first use) sends `message`; the reply must equal `reply` exactly, or, without `reply`, have `"status": "success"`. |
| `receive` | `client`, `frame`, `web`, `web_mode` | The client's next frame must equal `frame` (a file under the case, or the frame inline). `web`/`web_mode`: see "The web's output". |
| `silence` | `client`, `ms`, `web` | Nothing reaches the client for `ms` (default 300); `web` names a frame the web sends here (see "The web's output"). |
| `close_client` | `client` | The client closes its socket. |
| `push` | `socket`, `frame`, `connection` | The fake broker sends `frame` on the open connection (of `socket`, default `main`; on connection `connection` if named). |
| `upstream_receives` | `socket`, `frames`, `any_order`, `connection` | The feed's next frames to the broker are exactly `frames`, in order (or in any order), all on `connection` if named. |
| `upstream_connected` | `socket`, `connection`, `path`, `headers` | Connection `connection` (default 1) was opened, with this path and query and these headers (only the named ones are compared). |
| `drop_upstream` | `socket`, `code`, `reason` | The fake broker closes the open connection, as a broker dropping the socket does, with a close frame of that code and reason if given (Sahi's 4401, 4403, 4429); the manager reconnects or stops as the feed decides. |
| `disconnect` | `socket` | That socket's manager closes it deliberately (`WebSocketManager::disconnect`, as a logout or a broker switch does) while clients still hold subscriptions; the next `upstream_receives` names the feed's farewell frames (`BrokerFeed::farewell_frames`) and then its close (`{"close": ..}`, below). The end of the case does not expect that manager's subscriptions to be released at the broker. |
| `refuse_upgrades` | `socket`, `status`, `times` | The next `times` (default 1) connection attempts are refused with HTTP `status` (429, 401). At the very start of the script (only notes before it) it applies before the feeds connect: a socket that never starts (Dhan's 20-level socket refused, audit P-10), whose first connection the harness does not wait for. |
| `upstream_connections` | `socket`, `count`, `refused`, `ms` | The socket reaches exactly `count` opened connections (and `refused` refused upgrades, if named) and still has them after `ms` (default 300): a feed that reconnected, or one that stopped. |
| `http_serve` | `method`, `path`, `status`, `body`, `record` | The fake broker API answers `path` with this from now on (the order book a poller reads next). Counts as a push for the receipt window and for `web.json`. |
| `http_polled` | `method`, `path`, `times` | Wait until `path`'s current answer has been read `times` times (default 1): the poller has seen it. |
| `http_receives` | `requests` | The next requests the API kept (from `record` routes, or to no route) are exactly these: `{"method", "path" (with its query), "json" or "text" (the body; neither for none)}`. |

Frames, in `push` and `upstream_receives`: a file name in the case folder
(`.hex` and `.bin` are binary frames, `.json` and `.txt` text frames sent as
written; in `upstream_receives` a `.json` file is compared as JSON), or
inline `{"hex": "..."}`, `{"text": "..."}`, `{"json": {...}}` (a text
frame, sent compact, compared as JSON) or `{"binary_json": {...}}` (a binary
frame holding JSON, as Upstox sends its subscriptions). In a JSON frame or
request the feed sends, the string `"<any>"` matches any value at that key:
only for a value no case can know (Upstox's random `guid`), never where the
value is deterministic. `"<now_ms>"` and `"<now_s>"` match an integer epoch
time of this run, in ms or s: a time the feed stamps when it sends (Kotak's
`conn_req_time`). In `upstream_receives`, `{"close": null}` is the feed's
close frame with any code, `{"close": 4001}` with that code, and
`{"close": {"code": 4001, "reason": "..."}}` with that code and reason: a
feed that closes the socket itself (after an in-band authentication error)
or after a `disconnect`. On a socket whose script expects a close (or
disconnects it), every close the feed makes is recorded from the start, in
order with its frames, and a connection that ends without a close frame
shows as "ended without a close frame"; other sockets record no closes. A
`.hex` file takes any spacing and `#` comments,
so a packet is written field by field:

```
0001                    # packets in this frame
002c                    # packet length 44
00063a01                # instrument token 408065
```

At the end of the script nothing may be unread: a frame a client received
that no `receive` took, a frame the broker received that no
`upstream_receives` took, or a request the API kept that no `http_receives`
took, fails the case. Then every client leaves, and every subscription must
be released at the broker.

Sockets: `main` (the market data feed), `orders` (a dedicated order-update
socket) and `depth` (a separate deep-book socket). A broker's order or
deep-book feed is connected only in a case whose script names its socket,
so market data cases need not expect its login frames.

### The fake broker API

One fake REST API per case, on an ephemeral loopback port, for what a feed
or an order poller reads over HTTP: Upstox's authorize call before every
connect, Groww's socket token, the order book 5paisa, Samco and Groww poll
for order updates (web `_POLLING_BROKERS`), XTS's REST subscriptions. Point
the broker at `env.http_url()` with its `with_urls` constructor.

A path answers with its current answer, as many times as it is read:

- one fixed answer, registered by the broker module with
  `env.serve_json(path, json)` (any method, 200), or listed in `case.json`
  `http`;
- or a scripted sequence: each `http_serve` step replaces the answer from
  then on, and `http_polled` waits until the poller has read it.

A route's `path` without a query answers that path whatever the query; a
`path` with a query (`/v1/order/list?segment=CASH&page=0&page_size=100`)
answers exactly that path and query, so a poller that reads one path per
segment (Groww: CASH, then FNO) gets each segment's own book. `http_serve`
and `http_polled` name the route by the same string.

An answer's `body` is a file of the case (a `.json` file is served
compact), `{"json": ...}` or `{"text": ...}`; `{ws:main}`, `{ws:depth}`,
`{ws:orders}` and `{http}` in it become the fakes' addresses. `status`
defaults to 200, `method` to any. Requests to an answered path are counted,
not kept, since a poller reads every second; `"record": true` keeps them for
`http_receives` (REST subscriptions to compare exactly). A request to a path
nothing answers gets 404 and is kept, so the end of the case reports it.

Order updates from an order-book poller: in the broker module, start the
broker's poller only when the case asks for orders (`env.orders()`), with a
short interval, and hand its stream over, keeping the broker alive:

```rust
let updates = broker.start_order_updates(&auth, Duration::from_secs(1)).expect("poller");
feeds.with_order_feed(OrderFeed::Stream(updates)).keeping(broker)
```

The script then subscribes to orders, waits for the poller's silent first
read (`http_polled`), and serves each next book (`http_serve`) followed by
the `order_update` frame it makes (`receive`). `samco/order_book_polling`
is the example. The poller reads once a second at the shortest, so a book
takes up to a second to arrive.

### Expected frames

The frame exactly as the desktop must send it, in its JSON types: prices are
floats (`1490.0`, never `1490`), quantities integers. Every number the
harness compares (frames, expected files, `web.json`, answers) is read as
the exact double its text names, so a value one ulp away
(`1485.1000000000001`, the web's `148510 * (1/100)`) is a difference
(`harness/json_text.rs`; serde_json's default float parser would read it as
`1485.1`). A key the frame must
not carry is simply not in the file. What a case cannot know is written as
a placeholder:

- `"<receipt_ms>"`: a time the desktop stamps on receipt. It matches an
  integer epoch-millisecond time from just before the latest push to the
  frame's arrival, so it is never 0, never a trade time from the packet and
  never in seconds.
- `"<receipt_ms:N>"`: the receipt time of the push on script line `N`, an
  integer epoch-ms time from just before that push to just before the next
  one: a frame that carries an earlier packet's time (a book built after a
  ticker, stamped with the ticker's receipt).
- `"<ist_today_ms:HH:MM:SS>"` (or `HH:MM:SS.mmm`): that time of day on the
  IST date of receipt (the latest push's window; either date when it spans
  IST midnight), in epoch ms: a time of day the feed dates by the day it
  receives it (Definedge's depth `ltt`). The web script's `datetime` reads
  its frozen clock, an IST midnight, so `web.json` writes the web's value
  the same way (a time of day before 02:47 IST reads as a step's time
  there).
- `"<random:float>"`, `"<random:int>"`, `"<random:string>"`: a value a
  synthetic feed draws at random (Dhan Sandbox): any value of that JSON
  type. Declare the key a `random-values` deviation and bound it with
  `invariants` (below).

`invariants` in `case.json` (optional) say what must hold of the random
values, on every `market_data` frame a client receives and on the web's
publish it is compared with:

```json
"invariants": [
  {"key": "high", "at_least": "low"},
  {"key": "ltp", "at_least": 1477.6, "at_most": 1492.5},
  {"key": "open", "below": {"key": "ltp", "times": 0.96}},
  {"key": "depth.buy.*.quantity", "at_least": 10, "at_most": 500, "mode": 3}
]
```

`key` is a path in the frame's `data` (`*` for every item of a list);
`at_least`, `at_most`, `above` and `below` take a number, another key, or
`{"key", "times"}`. An invariant applies to a frame that has its key (and
is of its `mode`, if named), and one that applies to no frame of the case
fails it, as does one the web's own publish breaks (it would not be the
web's behaviour).

### The web's output, and deviations

`python3 scripts/feed_corpus/web_expected.py --web <path to the web tree>
<case folder>` runs the web adapter's own decoding and normalising methods
on the case's pushes, offline (no app imports, database or network), and
writes `web.json`: for each push (by its script line), what the web adapter
publishes, with the topic mode and the `data` it forwards (and, for orders,
the `order_update` frame the web relays). It records the web commit, the
SHA-256 of the script and the pushed frame files (text files with LF line
ends), and the instruments it used, so a stale `web.json` fails the case.
`--diff <case folder>` prints, without writing, every key on which the
case's expected frames differ from the web: the deviations the case must
declare. `--check` fails when a checked-in `web.json` is stale. How a broker
is run offline (`scripts/feed_corpus/web_adapters/<broker>.py`: the web
methods by name, and the state they read, built as the web's own tests
build it), and what to do where a web adapter cannot be run, is in that
script's docstring. CI never runs it: it has no web tree.

With `web.json` present, each `receive` after a push is compared with the
web's publish for the same instrument (the topic's exchange and symbol) and
mode for that push (or the lowest richer mode, which is what the web proxy
delivers to a client at that mode): the `data`
objects key by key, or the whole frame for an `order_update`. `web_mode`
picks another mode; `"web": false` says the web sends nothing for this
frame, which needs a deviation with key `frame`. The other way round, a
frame the web sends and the desktop does not is a `silence` step that
names the web's frame, `{"step": "silence", "client": "a", "web":
{"symbol": "BTCUSD.P", "exchange": "CRYPTO", "mode": 3}}`: the web must
publish that frame for the latest push (by instrument, then mode or the
lowest richer one), and it needs a deviation with key `missing-frame`. A
key of a `random-values` deviation is compared by presence and JSON type
at every level (an object's keys, a list's length), not by value. Every
key on which the desktop differs from the web must be declared:

```json
{"key": "upper_circuit", "kind": "open", "reason": "...", "ref": "audit C06"}
```

`kind` is one of:

- `web-defect`: the web is wrong (a wrong number, a wrong unit, lost data);
  the desktop keeps the right behaviour. Draft the write-up for the web.
- `desktop-rule`: a desktop security or resource rule in CLAUDE.md.
- `desktop-decision`: a desktop-wide wire decision already taken (the
  `data.mode` label, `open_interest` beside `oi`, the dialect aliases).
- `open`: a desktop gap still to fix. 1.0.7 ships with none.
- `random-values`: the value is drawn at random on both sides (a
  synthetic feed), so only its presence and JSON type are compared with
  the web, and the case's `invariants` bound it.

The script replays the web adapter with `random` seeded the same for every
case, and with the frozen clock at the script line being replayed, so the
web's draws are the same on every run and a receipt time it stamped at an
earlier step is written `"<receipt_ms:N>"` in `web.json`.

`approved` (true or false) records the lead's decision. A deviation no
difference shows any more fails the case, so remove it with the fix.
`feed_corpus::inventory::every_deviation_has_a_known_kind_and_a_reason`
reads every `case.json` on disk and lists together each deviation without
its key, with a key twice, of a kind not above, without a reason, or with
an `approved` that is not true or false; `parity_matrix.py` refuses to
write the matrix over such a deviation, so what it counts can be trusted.

## Adding a case to a broker

1. Make the folder `feed_corpus/<broker>/<case>/` with `case.json`,
   `script.jsonl`, the frames and the expected frames. Take the inputs from
   the web's own test where one exists (`test/test_<broker>_*feed*.py`), and
   name it in `provenance.sources`.
2. Add `<case>` to the broker's list in
   `tests/it/feed_corpus/<broker>.rs` (`corpus_cases!`). A folder not listed
   there, or a listed case without a folder, fails
   `feed_corpus::inventory`.
3. Compute `web.json` with `scripts/feed_corpus/web_expected.py`; declare
   every deviation `--diff` shows (the harness fails on an undeclared one
   and on one that no longer shows).
4. Run `python3 scripts/feed_corpus/parity_matrix.py` and commit the
   matrix with the case.
5. Run lane-test with `test(/feed_corpus::<broker>::/)`.

A frame a client receives that is not the expected one does not stop the
case: the case runs to its end and fails once, listing every mismatch
(`CORPUS-MISMATCH` lines). Every frame a client received is printed too, as
an expected frame would write it (receipt times as `"<receipt_ms>"` or
`"<receipt_ms:N>"`):

```
CORPUS-RECORD dhan/full_depth script.jsonl:7 {"type":"market_data",...}
```

The output of a failed test is in the lane-test log, so when a change moves
every frame of a case (a new wire field), one run gives every frame to pin
again. Check each against the web before pinning it: a recorded frame is
what the desktop sent, not what it should send.

## Adding a broker

`tests/it/feed_corpus/<broker>.rs`: a function that builds the broker's
feeds against the fake sockets, and its case list:

```rust
use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::dhan::streaming::{DhanFeed, DhanOrderFeed};

fn feeds(env: &Env) -> Feeds {
    Feeds::market(DhanFeed::with_url(&env.url("main"), "t", "c"))
        .with_orders(DhanOrderFeed::with_url(&env.url("orders"), "t", "c", env.symbols()))
}

corpus_cases!("dhan", feeds, [quote_previous_close, order_lifecycle]);
```

Then `mod <broker>;` and a line in `BROKERS` in `tests/it/feed_corpus/mod.rs`,
the broker's `broker.json`, and its glue in
`scripts/feed_corpus/web_adapters/` (a `webkit.Glue` naming the web
methods that decode and normalise its frames, and a line in that folder's
`__init__.py`).
Use the feed's `with_url` (or the broker's `with_urls`) constructor;
credentials are fake values (never real ones, never anything gitleaks
matches). A feed made by `Broker::create_feed` goes in with
`Feeds::boxed(..)`, an order feed from `Broker::create_order_feed` with
`.with_order_feed(..)`, a deep-book socket with `.with_depth(levels, ..)`.
Build the broker itself and pass it with the session's auth,
`.with_broker(broker.clone(), &auth)`, as the app does: the bridge then
takes its depth levels (`Broker::feed_depth_levels`, in place of
`case.json`'s `depth_levels`) and its exchange refusals
(`Broker::feed_refusal`), so a refusal case (an exchange or mode the
broker's feed does not stream) answers as it would in the app; the market
feed takes its step before a new instrument (`Broker::feed_presubscribe`,
Samco's index lookup over REST, answered by the fake broker API); and the
order relay completes each order socket's update with it
(`Broker::complete_order_update`, IndMoney's order-book lookup for an
order it did not see placed), waiting at most 5 s as the runtime does; and
a deep-book socket from `.with_depth(levels, ..)` starts as a session starts
it (`broker_runtime::connect_deep_feed`: the pool, the room refusal, the
broker's silence timeout), while `.with_failed_depth(levels)` is a deep-book
feed the broker could not make, so Depth subscribes at those levels are
served at the market feed's levels (`BrokerBridge::set_deep_unavailable`).
A deep socket refused from the start (`refuse_upgrades` first in the
script) is the manager's refused session, which the bridge reads from the
depth manager's status. The broker lives until the case ends. A
feed whose `prepare` calls the broker's REST API first (Upstox's authorize,
Groww's socket token) reads it from the fake broker API:
`env.serve_json("/v3/feed/market-data-feed/authorize", json!({"data":
{"authorized_redirect_uri": env.url("main")}}))`.

## Pitfalls

- Receive what a push makes before the next push: the feed server keeps only
  the newest frame per instrument and mode for a client that has not read
  the last one (latest-per-symbol, `feed::outbox`), so two pushes in a row
  can arrive as one frame.
- The manager replays its subscriptions in hash order after a reconnect, so
  a replay of several instruments can come in any order: use one instrument,
  or `"any_order": true` where the broker sends one frame per instrument.
- A feed that waits for a login acknowledgement subscribes only after it:
  push the acknowledgement before `upstream_receives` for the subscribe.
- A depth frame's quote fields come from the instrument's last tick, so a
  Depth-only client gets the book frame and not the tick's Quote frame.
