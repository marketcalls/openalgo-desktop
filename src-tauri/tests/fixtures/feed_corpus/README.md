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
matrix as **no case**.

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
- `deviations`: see below.

### script.jsonl

One step per line. Every step may carry a `"note"` for the reader;
`{"step": "note", "text": "..."}` is a comment line.

| Step | Keys | What it does |
| --- | --- | --- |
| `send` | `client`, `message`, `reply` | Client `client` (connected and authenticated on first use) sends `message`; the reply must equal `reply` exactly, or, without `reply`, have `"status": "success"`. |
| `receive` | `client`, `frame`, `web`, `web_mode` | The client's next frame must equal `frame` (a file under the case, or the frame inline). `web`/`web_mode`: see "The web's output". |
| `silence` | `client`, `ms` | Nothing reaches the client for `ms` (default 300). |
| `close_client` | `client` | The client closes its socket. |
| `push` | `socket`, `frame`, `connection` | The fake broker sends `frame` on the open connection (of `socket`, default `main`; on connection `connection` if named). |
| `upstream_receives` | `socket`, `frames`, `any_order`, `connection` | The feed's next frames to the broker are exactly `frames`, in order (or in any order), all on `connection` if named. |
| `upstream_connected` | `socket`, `connection`, `path`, `headers` | Connection `connection` (default 1) was opened, with this path and query and these headers (only the named ones are compared). |
| `drop_upstream` | `socket` | The fake broker closes the open connection, as a broker dropping the socket does; the manager reconnects. |
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
value is deterministic. A `.hex` file takes any spacing and `#` comments,
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
floats (`1490.0`, never `1490`), quantities integers. A key the frame must
not carry is simply not in the file. The one value a case cannot know is a
time the desktop stamps on receipt; write it `"<receipt_ms>"`. It matches an
integer epoch-millisecond time from just before the push that made the frame
to its arrival, so it is never 0, never a trade time from the packet and
never in seconds.

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
web's publish of the same mode for that push (or the lowest richer one,
which is what the web proxy delivers to a client at that mode): the `data`
objects key by key, or the whole frame for an `order_update`. `web_mode`
picks another mode; `"web": false` says the web sends nothing for this
frame, which needs a deviation with key `frame`. Every key on which the
desktop differs from the web must be declared:

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

`approved` (true or false) records the lead's decision. A deviation no
difference shows any more fails the case, so remove it with the fix.

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
`.with_order_feed(..)`, a deep-book socket with `.with_depth(levels, ..)`. A
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
