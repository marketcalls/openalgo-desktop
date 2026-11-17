# Broker integration guide

How to add a broker to OpenAlgo Desktop, or change one, in Rust. Written for a
contributor who knows the broker's API and wants it to work in the desktop
exactly as it works in OpenAlgo web.

The desktop has 39 brokers, all of OpenAlgo web's, built by **sign-in family**
rather than one by one. A new broker is usually a port of the web's
`broker/<name>/` Python plugin. This guide is the desktop's *mechanics*: where
each piece goes and how it is tested. For a broker's *behaviour* (rate limits,
history windows, order-type emulation, quirks), the web's skill references in
the OpenAlgo web repository (`.claude/skills/broker-integration/references/`)
remain the companion. The agent-facing summary of this guide is the
`broker-integration` skill, [.claude/skills/broker-integration/SKILL.md](../.claude/skills/broker-integration/SKILL.md).

- [1. What an adapter does, and what it does not](#1-what-an-adapter-does-and-what-it-does-not)
- [2. Where things live](#2-where-things-live)
- [3. Pick the family](#3-pick-the-family)
- [4. The Broker trait](#4-the-broker-trait)
- [5. Register it and describe its sign-in](#5-register-it-and-describe-its-sign-in)
- [6. Sign-in, by kind](#6-sign-in-by-kind)
- [7. Symbols and the master contract](#7-symbols-and-the-master-contract)
- [8. Orders and the three outcomes](#8-orders-and-the-three-outcomes)
- [9. Market data, margin and GTT](#9-market-data-margin-and-gtt)
- [10. Streaming](#10-streaming)
- [11. Rate limits](#11-rate-limits)
- [12. Secrets and redaction](#12-secrets-and-redaction)
- [13. The pages: Broker Configuration and Select Broker](#13-the-pages-broker-configuration-and-select-broker)
- [14. Tests](#14-tests)
- [15. Certification with a real account](#15-certification-with-a-real-account)
- [16. Porting map from the web plugin](#16-porting-map-from-the-web-plugin)
- [17. Checklist](#17-checklist)
- [How this differs from the web's guide](#how-this-differs-from-the-webs-guide)

## 1. What an adapter does, and what it does not

The adapter's only job is **translation**: the broker's shapes in, OpenAlgo's
common symbol, order, book and streaming formats out.

- **Business logic lives in `src-tauri/src/services/`**, never in an adapter.
  Smart orders, close-all, basket and split, the sandbox, the strategy module
  and the order reconciler all call the adapter through the same trait.
- **Analyzer mode never reaches the adapter.** With analyzer mode on, every
  order and account service goes to the sandbox engine before any broker
  call. "It works in analyzer mode" proves nothing about an adapter.
- **Every book returns OpenAlgo symbols.** Orders, trades, positions and
  holdings carry the OpenAlgo `symbol` and `exchange`, never the broker's
  trading symbol, or smart orders and close-position silently mismatch.
- **The `/api/v1` answers are the web's, field for field.** Your adapter
  returns typed values; the services shape the web's responses from them.

## 2. Where things live

All paths are under `src-tauri/src/brokers/` unless stated.

| Path | Holds |
| --- | --- |
| `mod.rs` | The `Broker` trait and `BrokerRegistry` (one adapter per broker, sharing one symbol master) |
| `types.rs` | `LoginKind`, `Capabilities`, credentials, the resolved order types |
| `catalog.rs` | Every broker's sign-in facts: `ALL_BROKERS`, how it signs in, its authorize URL, which callback parameter carries the code, account binding, login form fields |
| `setup_guide.rs` | The keys each broker needs and their labels, served to Broker Configuration by `GET /api/broker/list` |
| `common/` | Shared plumbing: `http.rs` (the one HTTP client), `symbols.rs` (`SymToken`, `SymbolResolver`), `master_contract.rs`, `mapping.rs` (OpenAlgo constants), `outcome.rs` (placement outcomes), `streaming.rs` (`BrokerFeed`), `relay.rs`, `ratelimit.rs`, `redact.rs`, `mpp.rs` (market price protection), `order_poll.rs`, `book_read.rs` |
| `families/noren/`, `families/xts/` | The two generic families |
| `<broker>/` | One folder per broker |
| `src-tauri/src/feed/dialect.rs` | Per-broker field names on the 8765 feed, as each web adapter sends them |
| `src-tauri/tests/fixtures/brokers/<broker>/` | Recorded broker payloads for the tests |
| `src-tauri/tests/it/` | HTTP round-trip, feed and secrets tests |

A standalone broker follows the existing layout (see `angel/` or `kotak/`):
`mod.rs` (the adapter struct and `impl Broker`), `auth.rs`, `orders.rs`,
`data.rs`, `funds.rs`, `mapping.rs`, `master_contract.rs`, `streaming.rs`,
optional `gtt.rs`, and `tests.rs` behind `#[cfg(test)]`. The adapter takes a
`SymbolResolver` in `new(symbols)`, keeps a clone of `common::http::client()`,
and has a `with_base_url(..)` constructor (plus `with_feed_urls(..)` when it
streams) so tests can point it at a local fake.

## 3. Pick the family

| Family | Shape in this repository | Members | Copy |
| --- | --- | --- | --- |
| Noren / Finvasia | `families::noren::NorenBroker` with a `&'static NorenConfig` (hosts, dialect, login, master files, tick rule, index naming, market price protection scope, funds) and `NorenHooks` function pointers. A member is one `mod.rs` with `CONFIG`, `HOOKS` and `pub fn broker(symbols)` | shoonya, flattrade, tradesmart, zebu | `shoonya/mod.rs` |
| Symphony XTS | `families::xts::XtsBroker` with a `&'static XtsConfig` (login, paths, master segments, stream mode codes, binary decoder, `XtsHooks`). A member is one `mod.rs` with `CONFIG` and `pub fn broker` | fivepaisaxts, jainamxts, compositedge, rmoney, ibulls, wisdom, iifl | `fivepaisaxts/mod.rs` |
| OAuth redirect | Own module | zerodha, upstox, fyers, dhan (and dhan_sandbox, a variant of it), groww, arrow, paytm, aliceblue, definedge, pocketful, hdfcsky, hdfcsecurities, rupeezy, nuvama (its feed is plain TCP, through a loopback line relay) | `zerodha/` |
| Direct login + TOTP | Own module | angel, kotak, mstock, motilal, samco, sahi, tradejini, fivepaisa, nubra, indmoney, firstock | `angel/` |
| Bespoke | Own module | iiflcapital (REST, and MQTT through the relay), deltaexchange (HMAC signing, the `CRYPTO` exchange, leverage) | as named |

**A white label of Noren or XTS is a new `CONFIG`, not new code.** Look for
`/NorenWClientTP/` or `/interactive/user/session` in its API documentation
before deciding a broker is bespoke. If a config field cannot express a
difference, add a hook to `NorenHooks` or `XtsHooks` rather than forking the
family.

**The family is the code shape, not the sign-in.** How the trader signs in is
decided per broker in `catalog.rs` and does not follow the table: Groww (TOTP
or a pasted access token) and Definedge (OTP) sign in through the in-app
form, while the Noren members, CompositEdge, RMoney and IIFL Capital
redirect. Check the catalogue before wiring the login.

## 4. The Broker trait

`brokers/mod.rs`. Required:

| Method | Returns |
| --- | --- |
| `id`, `name`, `logo` | The id (`zerodha`), the display name, `/logos/<broker>.svg` by convention |
| `login_kind` | `LoginKind::{Redirect, DirectTotp, TwoStep, AccessToken, ApiKeySecret}` |
| `supported_exchanges` | The web `plugin.json` list: a promise the tests check |
| `capabilities` | `history`, `multiquotes_batch`, `margin`, `gtt`, `streaming`, `order_feed`, `depth_levels`. Be honest: the services and the feed read them |
| `timeframe_map` | OpenAlgo intervals to the broker's |
| `authenticate` | The sign-in itself |
| `place_order`, `modify_order`, `cancel_order` | Order operations |
| `get_order_book`, `get_trade_book`, `get_positions`, `get_holdings`, `get_funds` | The books, in OpenAlgo symbols and the web's status strings |
| `get_quote`, `get_market_depth`, `get_history` | Market data |
| `download_master_contract` | The symbol master, every `SymToken` column |

Defaults you override only when the broker differs:

| Method | Default | Override when |
| --- | --- | --- |
| `begin_login(&creds)` | The catalogue's authorize URL applies | The authorize address needs a broker call first (Dhan's consent). Called by `BrokerAuthService::start_oauth` |
| `restore_session(&creds)` | Nothing | Per-login state the stored token does not carry must be rebuilt when a session resumes after a restart (Kotak's UCC). Never sets a password, TOTP or code |
| `on_logout()` | Nothing | The adapter runs something itself (an order poller, cached session state). Called on logout, at the 03:00 IST boundary and at shutdown. Must be idempotent |
| `create_feed(&auth)` | Unsupported | The broker has a market data socket ([10](#10-streaming)) |
| `create_order_feed(&auth)` | Unsupported | Order updates come from a socket, or a poller you own (`common::order_poll`). Leave it unsupported when the market feed already carries them (Zerodha) |
| `create_depth_feed(&auth, levels)` | Unsupported | A separate socket serves books deeper than 5 (Fyers 50, Dhan 20) |
| `feed_depth_levels(exchange)` | `capabilities().depth_levels` | The depth differs by exchange |
| `reports_order_tags` | `false` | The broker reports every order's client tag in its order book, so an unconfirmed order can be found by tag ([8](#8-orders-and-the-three-outcomes)). Today Zerodha and Angel |
| `get_holdings_with_totals` | Holdings; the service computes totals | The broker reports its own portfolio totals (Angel) |
| `get_multiquotes` | One `get_quote` per symbol | The broker has a batch endpoint; set `multiquotes_batch`. The options tools ask for 180 or more symbols at once |
| `cancel_all_orders`, `close_all_positions`, `get_open_position` | Built from the books | The broker has a native call |
| `calculate_margin`, the GTT methods | Unsupported | The broker supports them; set the capability. For basket margin never sum legs when a basket endpoint exists |
| `place_order_exact`, `leverage_config`, `broker_type`, `set_leverage` | Whole units, Indian stocks | Crypto only (Delta Exchange) |
| `as_any` | `None` | A sign-in helper needs the concrete type (Definedge OTP, Samco IP check) |

`BrokerCredentials` and `AuthResponse` have redacted `Debug`; `AuthToken`
holds a `Secret`, read with `expose()` only where the request is built.

## 5. Register it and describe its sign-in

1. **`brokers/mod.rs`**: `pub mod <name>;` and one line in the registry
   (`Arc::new(<name>::XBroker::new(symbols.clone()))`, or
   `Arc::new(<name>::broker(symbols.clone()))` for a family member). Add the
   id to the sorted list in the `registry_shares_one_symbol_master` test.
2. **`brokers/catalog.rs`**:
   - `ALL_BROKERS` (the web's directory order); the
     `catalogue_has_all_web_brokers` test counts it.
   - `auth_type`: `OAuth` (a browser redirect back to `/<broker>/callback`) or
     `Form` (the in-app form posted to `/<broker>/callback`).
   - `sign_in`: how Select Broker starts it: `Redirect`, `Form` or
     `SavedKeys` (listed in `SAVED_KEY_BROKERS`: the saved keys are all the
     adapter needs).
   - `authorize_url`: the authorize URL with the server-generated `state`;
     `None` when `begin_login` builds it.
   - `extract_code`: which callback parameter carries the code. Spell it
     exactly as the broker does (Zerodha `request_token`, Fyers `auth_code`,
     never a guessed `code`).
   - `callback_carries_state`: `false` only when the broker's redirect drops
     `state`.
   - `configured_account` and `CLIENT_ID_BROKERS`: the account a sign-in must
     come back for. Dhan and the Noren family take `client_id:::key` or a
     client id; Arrow, HDFC Sky, HDFC Securities, AliceBlue and Rupeezy take
     the Client ID entered on Broker Configuration.
   - Special cases: `login_binding` and `callback_binding` (Dhan's
     `consentAppId`), `posts_callback` (the XTS `session=` form POST:
     CompositEdge, RMoney), `sends_login_otp` (Definedge, Nubra),
     `pasted_token` (TradeSmart), `login_fields` and `credential_slot` (the
     in-app form's fields and which credential each fills).
   - `CRYPTO_BROKERS` and `has_daily_boundary` for a broker that trades
     around the clock: its session has no 03:00 IST end and its master
     follows the UTC day.
3. **The redirect URL stays byte-identical to the web's**:
   `http://127.0.0.1:5000/<broker>/callback` (`ServerConfig::redirect_url_for`),
   so a broker app registered for the web works unchanged.

## 6. Sign-in, by kind

Every sign-in runs in Rust, in `services/broker_auth_service`, under one
session lock: completing a sign-in ends any other broker's session, so one
broker is logged in at a time. Saved keys are encrypted (AES-256-GCM, bound
to their row and column) and come back to the page masked.

**Redirect brokers** (`sign_in` is `Redirect`). Select Broker calls
`GET /<broker>/initiate-oauth`; the server builds the authorize URL, records a
pending sign-in with a single-use `state` (only its hash is stored, valid 10
minutes) and sends the browser to the broker. The broker returns to
`GET /<broker>/callback`, where the server takes the pending sign-in,
exchanges the code in Rust, checks that the account is the configured one,
and stores the session. The page never sees the API key, the code or the
token.

- **A broker that drops `state`** is matched to the newest pending sign-in of
  that broker from the same browser session, within 3 minutes, once. Such a
  broker must name its account in `configured_account`: a state-less
  callback with no known account is refused.
- **Account binding.** On every path, before anything is stored, the account
  the broker returns must be the configured one; a mismatch is refused and
  the live session is left as it was.

**Form brokers** (`Form`). `GET /<broker>/callback` sends the browser to the
in-app login card, `/broker/<broker>/totp` (`src/pages/BrokerTOTP.tsx`, with
the web's per-broker copy), which posts the fields `login_fields` declares to
`POST /<broker>/callback` with the session's CSRF token. Definedge and Nubra
send their login OTP when the card opens, and only for a same-origin
navigation. Samco has its own connect page, `/broker/samco/auth`.

**Saved-key brokers** (`SavedKeys`: the XTS brokers that need no redirect,
Delta Exchange, Dhan (Sandbox)). One CSRF-checked `POST /<broker>/callback`
with no fields signs in with the saved keys. A `GET` of that callback signs in
nothing (the web signs these in on a GET). XTS brokers also need the market
data key pair (`api_key_market`, `api_secret_market`) for quotes and the feed.

Every way to create a broker session, and what protects it, is the table in
[Known security residuals](security/known-residuals.md#broker-sign-in-forged-callbacks-login-csrf).
A broker that drops `state`, uses a client id, posts its callback or takes a
pasted token adds its cases there and to the forged-sign-in test
([14](#14-tests)).

## 7. Symbols and the master contract

`download_master_contract` returns `Vec<SymbolData>` (`common::symbols::SymToken`).
Fill **every** web `SymToken` column: `symbol`, `brsymbol`, `name`,
`exchange`, `brexchange`, `token`, `expiry` as `DD-MMM-YY` in upper case
(empty when it never expires), `strike`, `lotsize`, `instrumenttype`,
`tick_size`. Dropping one breaks expiry pickers and option chains. The
symbol format is the web's
([prompt/symbol-format.md](prompt/symbol-format.md),
[prompt/crypto-symbol-format.md](prompt/crypto-symbol-format.md)); the ground
truth for a broker is the web's `symtoken` table for the same broker.

- Helpers in `common/master_contract.rs`: `format_expiry`,
  `parse_broker_expiry`, `normalise_expiry`, `format_strike`,
  `future_symbol`, `option_symbol`, `split_csv_line`, and `CsvHeader` (read
  columns by header name, never by position). Downloads use
  `http::DOWNLOAD_TIMEOUT`.
- `services/master_contract_service.rs` decides when to download, stores the
  rows in the `symtoken` table in stages (`db::sqlite::master_swap`), loads
  them into the shared resolver and emits the web's Socket.IO events. You do
  not write any of that.
- `name` is for display and text search only. Brokers fill it differently,
  so lookups by underlying use `brokers::common::underlying`
  (`row_underlying`, `is_derivative_of`, `contract_kind`), never `name`.

Requests reach the adapter as `ResolvedOrder` and `ResolvedModify`, already
resolved against the master. Books map back through `SymbolResolver`
(`common/symbols.rs`): `oa_symbol`, `oa_symbol_or_raw`, `br_symbol`, `token`,
`brexchange`, `by_token`, `by_brsymbol`, `by_symbol`. Map statuses to the
web's lowercase strings (`complete`, `open`, `trigger pending`, `cancelled`,
`rejected`; `brokers::lower_status` for the rest) and constants through
`common::mapping` (`Exchange`, `Product`, `PriceType`, `Action`,
`OrderStatus`, `Validity`; values in [prompt/order-constants.md](prompt/order-constants.md)).
A book that cannot be read is an error, never an empty book
(`common/book_read.rs`).

## 8. Orders and the three outcomes

A placement has **three** outcomes, not two (`common/outcome.rs`):

| Outcome | Means | What happens |
| --- | --- | --- |
| Accepted | An order id came back | Normal |
| Refused | Provably never sent, or the broker said no in a readable answer | The claim is released; the reason goes to the caller |
| Uncertain | Sent, or maybe sent, with no definite answer: a timeout or dropped connection after sending, a 5xx, an unreadable reply, a success without an id | The order is never placed again. Its row is `unconfirmed`, and only the order reconciler settles it from the broker's order book |

Every `place_order` runs inside `outcome::placing`, where **an unmarked error
is uncertain**. Mark a readable "not placed" with `outcome::refusal`,
`refusal_with_status` or `definite` (`AppError::Refused`). A refusal after a
5xx in the same placement stays uncertain. Reads before the send (a quote for
market price protection, a client-id lookup) go through `outcome::before_send`,
so their failure is a refusal and their 5xx does not taint the order. An
adapter that knows more than the transport (a tag, an ambiguous reply)
returns `AppError::Uncertain`; never `AppError::Broker` with "check the order
book". Rate-limit answers (429) on an order are refusals.

If the broker reports every order's client tag in its order book, return
`true` from `reports_order_tags` and tag orders (`outcome::with_client_tag`):
the reconciler then finds an unconfirmed order by tag. Otherwise it matches
by symbol, exchange, side, quantity, product and shape among orders nobody
owns, and never from a row whose time cannot be read.

Per-broker status, and the lost-reply tests each adapter carries, are in
[Order placement outcomes](port/order-outcomes.md). Add your adapter's row.

## 9. Market data, margin and GTT

- **Quotes and depth**: `get_quote`, `get_market_depth`, `get_multiquotes`
  (batch when the broker has one).
- **History**: candles in epoch seconds, oldest first; respect the broker's
  window per request (fetch in chunks; a chunk that fails fails the
  history, never leaves a gap).
- **Option chains** are quoted through `Broker::get_chain_quotes`, which
  says which legs came back without open interest: a missing OI is never a
  zero.
- **Margin** and **GTT** only where the broker has them, with the capability
  set.

## 10. Streaming

Implement `common::streaming::BrokerFeed`: `broker`, `prepare` (async work
before each connect, such as an authorize call or a socket token;
`PrepareError::AuthFailed` stops retries until the next sign-in),
`ws_request`, `on_connected`, `on_authenticated` and `awaits_auth_ack`,
`subscribe_frames`, `unsubscribe_frames`, `mode_change_frames`, `parse` (a
frame to `FeedEvent::{Tick, Depth, OrderUpdate, AuthOk, AuthFailed,
Heartbeat, Reply}`), `heartbeat`, `supported_depth_levels`,
`is_auth_failure`.

You write **no reconnect loop, no thread and no queue**. The feed is driven by
`websocket::WebSocketManager` (`src-tauri/src/websocket/manager.rs`): one
owned supervisor task, connect timeout, close-before-reconnect, capped
backoff with jitter, stall watchdog, re-subscribe on reconnect,
reference-counted subscriptions, a bounded command channel and a bounded
broadcast. `services/broker_runtime.rs` starts the market, depth and order
feeds at sign-in and tears them all down at logout.

- Ticks carry OpenAlgo symbols and epoch-millisecond timestamps; use
  `NormalizedTick::derive_change` for change and percent change.
- A transport that is not a broker WebSocket (IIFL Capital's MQTT) or an
  upstream session that must be relayed (Nubra's order socket) goes through
  the loopback relay, `common/relay.rs`: implement `Upstream::open` and
  `Session`, start it with `RelayHandle::start`, and point `ws_request` at
  `RelayHandle::url()`.
- **The 8765 frames.** If the broker's web adapter names quote fields
  differently from Zerodha's (`last_trade_quantity`, `ltq`, `change`,
  `percent_change` and so on) or acknowledges a Depth subscribe with a
  different depth, add the broker to `src-tauri/src/feed/dialect.rs`, so a
  script written against the web with this broker finds the same keys.
- The 8765 contract (modes 1/2/3, depth 5/20/30/50) is checked by
  `tests/it/feed_conformance.rs`. How ticks flow from here is in
  [WebSocket architecture](websocket-architecture.md).

## 11. Rate limits

`common/ratelimit.rs`:

- **`Pacer`** paces requests per second in arrival order, and **`Budget`**
  holds rolling windows, a daily cap and a cool-down, for brokers that publish
  per-category limits (Fyers counts market data, orders and account calls
  separately per second, minute and day).
- **A request waits at most ten seconds for its turn** (`MAX_QUEUE_WAIT`);
  one whose turn is further away is refused before sending (a refusal, never
  an uncertain order). Background work that may wait (Historify) uses
  `acquire_unbounded`.
- **Orders take their own lane.** A broker that publishes no order rate
  paces places, modifies and cancels at `ORDERS_PER_SECOND` (10 a second) on
  a pacer apart from its data calls, so a queue of quotes never holds an exit
  back.
- **Only a throttle answer is retried** (HTTP 429, or the broker's throttle
  code), also for an order. A timeout or a dropped send never is.

## 12. Secrets and redaction

- One HTTP client: `common::http::client()`, with timeouts built in. Never
  `reqwest::Client::new()` in an adapter.
- Every request, socket and transport error that may be logged or returned
  goes through `common::redact`: `http(e)` (a `reqwest` error without its
  URL), `ws(e)` and `ws_error_kind(&e)`, `redact(AppError)`, and
  `url_safe_error` / `url_safe` for log lines. Brokers that put a key or token
  in a URL leak through `reqwest`'s `Display` otherwise.
- The trader reads `AppError::client_message`; the technical detail goes to
  the log once, at the boundary. Never a status code or an exception in a
  message a trader reads.
- `lib.rs::log_filter` keeps `tungstenite` at info because it traces the
  handshake URL; do not lower it.

## 13. The pages: Broker Configuration and Select Broker

The frontend keeps no broker list. Everything comes from the server:

- **`setup_guide.rs`**: the broker's entry on Broker Configuration
  (`src/pages/BrokerConfig.tsx`, served by `GET /api/broker/list`): the
  display name, the keys it needs labelled the way the broker names them, a
  hint per key, the sign-in note and the redirect URL hint. The
  `every_broker_has_help_consistent_with_its_sign_in` test checks the entry
  against the catalogue. A new broker needs an entry and nothing in
  TypeScript.
- **Select Broker** (`src/pages/BrokerSelect.tsx`) starts each sign-in the way
  `catalog::sign_in` says. A failed redirect sign-in comes back to
  `/broker?error=..&broker=<id>` and shows on that broker's card.
- **`src/pages/BrokerTOTP.tsx`** carries the web's per-broker form copy for
  form brokers; keep it close to the web's file and make its field names
  match `catalog::login_fields`.

Also update the documents that list brokers: README "Supported brokers", the
sign-in groups in [user/brokers.md](user/brokers.md), the sign-off table in
[release/broker-certification.md](release/broker-certification.md), the
family table in CLAUDE.md and the skill, and, for a state-less or client-id
broker, [security/known-residuals.md](security/known-residuals.md).

## 14. Tests

Tests ship in the same commit as the adapter.

**Fixtures** go in `src-tauri/tests/fixtures/brokers/<broker>/` (recorded
payloads: `order_book.json`, `positions.json`, `holdings.json`, `quote.json`,
`master.json` or `instruments.csv`, `errors.json`). Use the `<APIKEY>`,
`<USER_ID>` and `<EMAIL>` placeholders; fake tokens must match the allowlist
in `.github/gitleaks.toml`. Never a real account id, token or e-mail.

1. **Mapping unit tests** in `src/brokers/<broker>/tests.rs`, loading the
   fixtures (model: `zerodha/tests.rs`): master rows by header name, every
   book to OpenAlgo symbols and statuses, order payloads equal to what the
   web sends, quotes, depth, history, funds, margin, error mapping to
   trader-facing text. Compare against the web's recorded answers in
   `tests/fixtures/web/rest/` where the web recorded the same instrument.
2. **HTTP round trip** in `src-tauri/tests/it/` against a local axum fake on
   an ephemeral port (`with_base_url`): headers, bodies and mapped answers
   for sign-in, orders, books, funds, quotes, depth, history, margin and GTT.
   Model: `broker_angel_http.rs`; the families in `brokers_noren.rs` and
   `brokers_xts.rs`; batches in `brokers_oauth_batch/`,
   `brokers_direct_batch_a/` and `_b/`. Placement outcomes in
   `broker_order_outcomes.rs` (a 503, a reply cut off, a 200 without an id,
   one readable refusal). A new file is a new `mod` line in
   `tests/it/main.rs`.
3. **Secrets never leak.** Model: `tests/it/brokers_direct_batch_b/secrets.rs`.
   Drive every sign-in, REST and socket error path with sentinel credentials
   against a closed port and a fake that refuses everything, capture the
   logs at TRACE, check the capture is not empty, and check the sentinel
   appears nowhere: logs, `Display`, `Debug`, `client_message`.
4. **Feed parsing** of recorded frames (`parse` to `FeedEvent`, binary
   offsets, subscribe frames), and for a feed with its own task a hygiene
   test that tasks and descriptors return to their baseline (models:
   `broker_groww_hygiene.rs`, `broker_upstox_feed_hygiene.rs`).
5. **Sign-in refuses forgeries.** When the callback is state-less, uses a
   client id, posts its callback or takes a pasted token, add its cases to
   `server::tests::every_way_to_create_a_broker_session_refuses_a_forged_attempt`
   (`src-tauri/src/server/tests.rs`). `server::tests::sign_in` drives every
   catalogue broker through Select Broker's start.
6. **Catalogue cases**: `cargo test --lib catalog` for `extract_code`,
   `authorize_url` and `configured_account`.

Run, from `src-tauri/`:

```bash
cargo test --locked --lib brokers::<broker>
cargo test --locked --test it broker_<broker>      # or the batch module's name
cargo clippy --all-targets --locked -- -D warnings
(cd .. && python3 scripts/ci/code_health.py)
```

A new broker is new code, so it meets the code-health limits in full: an
adapter over 1,000 lines splits into `auth.rs`, `orders.rs`, `streaming.rs`
and `mapping.rs` as the existing brokers do, and a shared shape belongs in its
family's generic code, not in a copy. See [contributing.md](contributing.md)
for the gates CI runs.

## 15. Certification with a real account

Tests against fakes cannot prove that a real sign-in, the books, the feed and
an order behave. Before Live trading is offered for a broker, run the
certification kit against a running app signed in to a real account:
[Broker certification](release/broker-certification.md)
(`scripts/certify/broker_certify.py`). Its sign-off table lists all 39
brokers.

## 16. Porting map from the web plugin

| Web Python (`broker/<name>/`) | Desktop Rust (`brokers/<name>/`) |
| --- | --- |
| `plugin.json` | `id`, `name`, `logo`, `supported_exchanges`, `broker_type`, `leverage_config` |
| `api/auth_api.py` `authenticate_broker` | `auth.rs`, `Broker::authenticate`; callback parameters in `catalog.rs` |
| `blueprints/brlogin.py` special cases | `catalog.rs` (`extract_code`, `posts_callback`, `login_fields`, bindings) |
| `api/order_api.py` | `orders.rs`: place, modify, cancel, books, `cancel_all_orders`, `close_all_positions`, `get_open_position` |
| `mapping/transform_data.py` | Request builders and enum maps in `mapping.rs` or `orders.rs` |
| `mapping/order_data.py` | Book mappers in `mapping.rs` (OpenAlgo symbols, lowercase statuses) |
| `api/data.py` `BrokerData` | `data.rs`: quote, multiquotes, depth, history, `timeframe_map` |
| `api/funds.py` | `funds.rs` |
| `api/margin_api.py`, `api/gtt_api.py` | `calculate_margin`, the GTT methods |
| `database/master_contract_db.py` | `master_contract.rs` |
| `streaming/*_adapter.py`, `*_websocket.py`, `*_mapping.py` | `streaming.rs` (`BrokerFeed`); no thread, no ZeroMQ, no reconnect loop; field names in `feed/dialect.rs` |
| `utils.httpx_client`, the logging filter | `common::http::client()`, `common::redact` |
| `utils.mpp_slab` | `common::mpp` (`protected_price`, `round_to_tick`) |
| `test/test_<broker>_*.py` | Rust unit and `tests/it` cases with the same inputs and expectations |

Read the web code and its tests for behaviour, then reproduce its shapes, not
its bugs: `tests/fixtures/web/INDEX.md` lists the defects observed in the web
that the desktop does not reproduce.

## 17. Checklist

- [ ] Family chosen; a family member is a config and hooks only
- [ ] `Broker` implemented: required methods, honest capabilities, `supported_exchanges` equal to the web `plugin.json`
- [ ] Session hooks where needed: `begin_login`, `restore_session`, `on_logout` (idempotent, stops pollers)
- [ ] Registered in `BrokerRegistry` and in the registry test's id list
- [ ] `catalog.rs`: `ALL_BROKERS`, `auth_type`, `sign_in`, the authorize URL, `extract_code`, state carriage, `configured_account` / `CLIENT_ID_BROKERS`, form fields
- [ ] Every book returns OpenAlgo symbols and the web's status strings; an unreadable book is an error
- [ ] The master contract fills every `SymToken` column, expiry `DD-MMM-YY`
- [ ] Placements inside `outcome::placing`, readable refusals marked, `reports_order_tags` honest
- [ ] Feeds through `BrokerFeed` on the shared manager (the relay only for non-WebSocket transports); depth and order feeds as the broker offers; `feed/dialect.rs` entry if its field names differ
- [ ] The shared HTTP client; every error through `common::redact`; nothing secret in logs
- [ ] Fixtures with placeholders
- [ ] Unit, HTTP round trip, outcome, secrets (non-vacuous), feed parsing, forged sign-in and catalogue tests
- [ ] A `setup_guide.rs` entry; `BrokerTOTP.tsx` copy for a form broker
- [ ] Broker lists in README, `docs/user/brokers.md`, the certification table, CLAUDE.md and the skill
- [ ] `cargo fmt`, `clippy -D warnings`, the full `cargo test --locked`, the code-health gate

## How this differs from the web's guide

| OpenAlgo web | OpenAlgo Desktop |
| --- | --- |
| A plugin folder discovered at startup, listed in `.env` `VALID_BROKERS`, `start.sh` and the install scripts | A Rust module registered in `BrokerRegistry` and `catalog.rs`; no `.env` |
| `plugin.json` | Trait methods (`id`, `name`, `supported_exchanges`, ...) |
| Callback special cases in `blueprints/brlogin.py` | Declarative entries in `catalog.rs` |
| Streaming adapters publish to ZeroMQ from their own threads, with connection pooling | `BrokerFeed` translates frames; `WebSocketManager` owns the socket, reconnects and fan-out |
| A placement succeeds or fails | Accepted, refused or uncertain; uncertain orders are settled by the order reconciler |
| Broker keys in `.env` | Saved per broker on Broker Configuration, encrypted, labelled by `setup_guide.rs` |
| One fixed pace for every call (where any) | Per-category budgets where the broker publishes them, a separate order lane, a ten-second wait cap |
