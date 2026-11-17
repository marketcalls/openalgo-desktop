# Broker certification

Every broker adapter is tested against recorded broker answers and fake
brokers, but only a real account proves that sign-in, the books, the feed and
an order behave as they should. Zerodha's sign-in and the UI have been checked
by the maintainer on macOS; the other brokers have not (BR-06 in the Codex
evaluation, report D).

**Offer Live trading only for a broker and segment that have a signed-off
sheet below.** Until then the broker stays usable in analyzer mode
(sandbox mode).

The kit is this checklist plus one script,
`scripts/certify/broker_certify.py`. It talks to a running OpenAlgo Desktop
app through the same public API any client uses (`/api/v1` and the WebSocket
feed) with your own API key, and writes a report: one row per step with
PASS, FAIL or N/A, a note, and the answer the app passed on from the broker.
Secrets are taken out of the report before it is written.

## Before you start

- A real account with the broker, signed up for its API, with a small funded
  balance: enough to buy one share of the test symbol at the test price
  (CNC). The test order rests and is cancelled, but the broker blocks its
  margin while it rests.
- OpenAlgo Desktop running with that broker signed in:
  - the installed release (HTTP on 5000 and the feed on 8765 unless you
    changed them in the app), or
  - the `dev_server` example (`cargo run --example dev_server`), which uses
    a throwaway data folder and the development ports 5500 and 8766. A
    broker that signs in by browser redirect sends you back to the
    redirect URL registered with it, `http://127.0.0.1:5000/<broker>/callback`,
    so use the installed app for those brokers.
  - OpenAlgo web uses 5000 and 8765 too. If it runs on the same computer,
    stop it or give the desktop app other ports, and pass them with
    `--port` and `--ws-port`. The first step checks which broker the app
    answering on that port is signed in to.
- The API key from the app's API Key page.
- Python 3.8 or newer. Nothing to install: the script uses the standard
  library only.
- Market hours for the order, GTT and feed steps. Outside market hours run
  with `--market-closed` (a feed that sends nothing is then N/A, not FAIL)
  and leave out `--place-orders`.
- Live mode in the app. In analyzer mode the order steps are N/A, because
  the orders would go to the sandbox, not the broker.

## Run it

The script asks for the API key at a prompt. It also reads the
`OPENALGO_CERT_API_KEY` environment variable. `--api-key` works, but other
programs on the computer can read a command line, so the script warns.

macOS and Linux:

```bash
# Everything except orders, safe at any time:
python3 scripts/certify/broker_certify.py --broker <id>

# The full run in market hours: one test order and one GTT, both cancelled:
python3 scripts/certify/broker_certify.py --broker <id> --place-orders --gtt

# With an F&O contract for the master and quote checks:
python3 scripts/certify/broker_certify.py --broker <id> --fo-symbol NIFTY30DEC26FUT
```

Windows (PowerShell or Command Prompt):

```powershell
py -3 scripts\certify\broker_certify.py --broker <id>
py -3 scripts\certify\broker_certify.py --broker <id> --place-orders --gtt
```

`<id>` is the broker's id in the table below. Other options:
`--symbol` and `--exchange` (default `SBIN` on `NSE`; Delta Exchange needs
a `CRYPTO` symbol), `--fo-exchange` (default `NFO`), `--product`,
`--offset` (how far below the last price the test order sits, default 5 %),
`--host`, `--port`, `--ws-port`, `--wait`, `--feed-wait`, `--stream-wait`,
`--out` (the report's path without extension), `--yes` (do not ask before
placing). `--help` lists them all.

The exit code is 0 when no step failed, 1 when one did, and 2 when the
script could not start (no API key).

### What the script will and will not do

- It places an order only with `--place-orders`, only in Live mode, and
  only after you type `YES` at its prompt (or pass `--yes`).
- The order is a BUY LIMIT for one share (one lot for a contract) priced
  `--offset` percent below the last price, rounded down to the tick. It is
  never a MARKET order. It refuses to place anything when the last price,
  the tick size or the lot size cannot be read.
- 5 % stays inside the exchanges' price bands. An order 20 % away would be
  rejected by the exchange on most stocks, and a rejected order cannot be
  modified or cancelled, which is what the steps check.
- Every order and GTT it placed is cancelled before it exits, also when an
  earlier step failed (the `cleanup` step). If the test order fills, the
  `order_place` step says so: close that position in the app.

## The steps

Run in this order. Steps marked manual are things you do in the app; the
script then records the result.

| Step | Checked | PASS means |
| --- | --- | --- |
| Sign in (manual) | Sign in to the broker in the app. | |
| `sign_in` | `/api/v1/ping` | The app answers `pong` for this broker. |
| `mode` | `/api/v1/analyzer` | The mode could be read; the note says Live or analyzer mode. |
| `master_search` | `/api/v1/search` | The master contract finds the test symbol. |
| `master_symbol` | `/api/v1/symbol` | The symbol has a token, tick size and lot size. |
| `master_fo_symbol` | `/api/v1/symbol` | The F&O contract (`--fo-symbol`) has a token, tick, lot size and expiry. |
| `master_expiry` | `/api/v1/expiry` | The contract's expiry is in its underlying's expiry list. |
| `orderbook`, `tradebook`, `positionbook`, `holdings` | the books | Each book is read and has the documented shape (an empty book is a PASS). |
| `funds` | `/api/v1/funds` | Funds are read. Compare the figures with the broker's own app. |
| `quotes` | `/api/v1/quotes` | A last price is returned. |
| `multiquotes` | `/api/v1/multiquotes` | Every requested symbol comes back with data. |
| `depth` | `/api/v1/depth` | Bids and asks are returned. |
| `intervals` | `/api/v1/intervals` | The broker's intervals are listed. |
| `history_daily`, `history_intraday` | `/api/v1/history` | Daily candles over 10 days and intraday (5m, else the first listed) over 5 days come back. |
| `feed_ltp`, `feed_quote`, `feed_depth` | the WebSocket feed | A `market_data` frame arrives for LTP, Quote and Depth. N/A when the broker has no live feed in the app, or with `--market-closed`. |
| `order_stream_subscribe` | the feed's `subscribe_orders` | The order stream acknowledges the subscription. |
| `order_place` | `/api/v1/placeorder`, `/api/v1/orderstatus` | The order is accepted and shows as open in the order book. |
| `order_modify` | `/api/v1/modifyorder` | The order book shows the new price. |
| `order_cancel` | `/api/v1/cancelorder` | The order book shows it cancelled. |
| `order_stream_updates` | the feed's `order_update` frames | Updates for the test order arrive, the last one `cancelled`. If OpenAlgo Desktop has no order update source for this broker, record N/A by hand and say so. |
| `gtt_place`, `gtt_modify`, `gtt_cancel` | the GTT endpoints | A far-from-market SINGLE BUY GTT is placed, moved and cancelled, each seen in `/api/v1/gttorderbook`. N/A when the broker has no GTT in the app. |
| `cleanup` | | Nothing the script placed is left open. |
| Daily boundary (manual, next morning) | After 03:00 IST, before signing in again, run with `--boundary-check`. | `daily_boundary`: the API refuses broker calls (the session ended at 03:00 IST) while the key itself still works. For Delta Exchange the session must still be there: crypto has no daily end. |
| Logout (manual) | Log out of the broker in the app, then run with `--after-logout`. | `logout`: broker calls are refused; the key itself still works. |

Also check by eye, in the app, while the script runs: the order appears on
the Order Book page and changes there without a refresh, the positions and
funds pages load, and the broker's own app or website shows the same order,
price and cancellation.

## The report

The script writes `certification-<broker>-<date>-<time>.json` and `.md` in
the current folder (or at `--out`). Before the report is written it removes
the API key, tokens and anything shaped like a JWT, and every value under an
account field (client id, user id, account, e-mail, mobile, PAN and the
like) wherever it appears, plus e-mail addresses, Indian mobile numbers and
PANs in any text. Read the report before sharing it all the same.

Attach the Markdown report to the release's certification issue (or the
pull request that changes the broker) and link it in the table below. Keep
the JSON with the release records. Never commit a broker answer that has
not been through the script's redaction.

## Sign-off

A broker is certified for the segments listed, on the app version listed.
A change to the broker's adapter, or a broker API change, needs a new run.

| Broker | Date | App version | Segments certified | Result | Report |
| --- | --- | --- | --- | --- | --- |
| aliceblue | | | | pending | |
| angel | | | | pending | |
| arrow | | | | pending | |
| compositedge | | | | pending | |
| definedge | | | | pending | |
| deltaexchange | | | | pending | |
| dhan | | | | pending | |
| dhan_sandbox | | | | pending | |
| firstock | | | | pending | |
| fivepaisa | | | | pending | |
| fivepaisaxts | | | | pending | |
| flattrade | | | | pending | |
| fyers | | | | pending | |
| groww | | | | pending | |
| hdfcsecurities | | | | pending | |
| hdfcsky | | | | pending | |
| ibulls | | | | pending | |
| iifl | | | | pending | |
| iiflcapital | | | | pending | |
| indmoney | | | | pending | |
| jainamxts | | | | pending | |
| kotak | | | | pending | |
| motilal | | | | pending | |
| mstock | | | | pending | |
| nubra | | | | pending | |
| nuvama | | | | pending | |
| paytm | | | | pending | |
| pocketful | | | | pending | |
| rmoney | | | | pending | |
| rupeezy | | | | pending (adapter in PR #137) | |
| sahi | | | | pending | |
| samco | | | | pending | |
| shoonya | | | | pending | |
| tradejini | | | | pending | |
| tradesmart | | | | pending | |
| upstox | | | | pending | |
| wisdom | | | | pending | |
| zebu | | | | pending | |
| zerodha | | | | pending (sign-in and UI checked by the maintainer on macOS) | |

## Extra checks per broker

Questions the code and fakes cannot answer, from the Codex evaluation
(report D, section 4, "What needs a live broker account"). Record the
answer in the broker's report or its certification issue.

- **upstox**: what funds answers during Upstox's service hours (its 423),
  and that resuming a stored session after a restart in those hours does
  not take that answer as proof the token works (BR-06).
- **hdfcsky**: whether a funds call validates the token (an expired token
  must not resume) (BR-06); what HDFC Sky does with a duplicate
  `user_order_id` (rejects, drops or ignores it) and its accepted range
  (BR-05); whether every feed packet carries its segment, and any token that
  collides across segments (BF-04).
- **samco** and **aliceblue**: the exact order book answer when there are no
  orders today, so the "confirmed empty" case is not guessed (BR-02).
- **samco**: the order book's status strings, the request rate that allows a
  safe order-book poll interval, and how the static-IP check looks to the
  poller (BF-01).
- **rmoney** and **fivepaisaxts**: the error codes for a subscribe that
  fails for a moment against an "Invalid Token" (BF-03).
- **kotak**: which segment files are always published, CDE_FO and MCX_FO
  on holidays included, so the required set is not too strict (MC-02).
- **deltaexchange**: what OpenAlgo web serves for `BTC_INR`'s `lotsize`
  (needs a web session with Delta) (MC-04); a key used across several
  03:00 IST and 00:00 UTC boundaries, with sleep and wake, on an installed
  build (SES-01).

### Rupeezy live checks

From PR #137 (Rupeezy on the Vortex API), which was built without a Rupeezy
account:

1. Whether `flow.rupeezy.in` echoes `state`; without it the sign-in relies on the saved Client ID matching Vortex's `data.user_id` (confirm which id that is: `user_id` or `others.userCode`).
2. That `x-api-key` on REST calls is accepted, and the funds body shape (`exchange_combined`, `nse`, `mcx`).
3. Order book paging (`offset` from 1) past 500 orders; `order_number` / `traded_price` / `error_reason` on book rows.
4. Positions carry `lot_size` and `multiplier` on MCX rows (the P&L and close-all lean on them); holdings carry `last_price`.
5. History chunk caps (1000 days daily, 30 days intraday) and daily candle stamps.
6. The feed: one JSON subscribe per ticker, mode replaced by re-subscribing, binary packet layout, protocol pings kept alive, a 401 handshake when the token expires.
7. GTT OCO with `product`, and modify by leg id with a trailing stop.

### Nuvama live checks

From the desktop port of web openalgo#2461 (ee123273b), built without a
Nuvama account, from the web plugin's field names; nothing here has been
seen live from the desktop:

1. The sign-in: whether Nuvama's login page returns `request_id` (or `requestId` / `reqId`) and drops any `state`; that `eqAccID` (or `coAccID`) is the account id the trader saves on Broker Configuration.
2. The HTTP 222 error envelope on empty books (`ETRD0002`, `ETRD0004`) and on a refused order, and `EGN0011` for an expired session.
3. Order book rows: whether they carry a filled quantity and average price (read from `fldQty` / `flQty` / `fQty` and `avgPrc` when present) and a rejection reason (`rjRsn` / `rejRsn` / `rRsn`).
4. MARKET orders priced off the LTP and SL-M orders sent as STOP_LIMIT off the trigger, accepted by Nuvama.
5. History paging with `ltt` and the session grid on a 09:15 open (30m and 1h rebuilt from 15m bars).
6. The feed over the TCP line relay: the quote packets' field types (text or numbers), the `ltt` / `altt` formats, the depth stream's `bidValues` / `askValues`, whether Nuvama answers `{}` keepalives, and that one whole-set request per streaming type 0.5 s after the last change keeps every symbol streaming.
7. The order stream: `orderFiler` packets after a place, fill and cancel.

### Sahi live checks

From PR #213 (Sahi, the desktop port of web openalgo#2320), which was built
without a Sahi account, against docs.sahi.com and recorded shapes:

1. Sign-in with an approved key, and the answers for a key not approved today (403 `Forbidden`) and a locked account; that `/v1/profile` `user_id` is the account id the session records.
2. The static IP: orders from an unregistered address are refused with 403 `IP_NOT_ALLOWED` and the trader reads the static-IP message; the address that counts is the computer's public egress address.
3. Orders on each segment's products (NSE C and I, BSE C and I, NFO and BFO M and I), MKT and SL-MKT sent with price 0 and converted by Sahi to protected limits (the book then shows LMT); a quantity above the freeze quantity is refused (Sahi does not slice); whether `AwaitingTrigger` ever appears as an order status.
4. The trade book, read as one call per filled order, against Sahi's per-day budgets (order book, order detail and trades share 10,000 reads a day) with the order reconciler's polling.
5. Positions and holdings valued at `/v1/marketdata/ltp`; funds against Sahi's own figures (`option_premium.paid` is a net account figure).
6. The feed: the Bearer upgrade with a User-Agent, a 401 or 403 at the upgrade once the token is revoked (the docs say so), mode changes in place, and the 1000-instrument cap per connection.

## Checking the script itself

The script has a self-test that runs every step against a fake app on
ephemeral loopback ports (nothing reaches a broker or a running app):

```bash
python3 -I scripts/certify/test_broker_certify.py
```
