# Market data frames on port 8765

What a `market_data` frame carries, per broker and per mode, and how one
broker tick reaches a client that holds several modes of one symbol. The
protocol itself (authenticate, subscribe, acknowledgements) is the web's:
`docs/prompt/websockets-format.md` and `docs/api/websocket-streaming/`
in the web repository, and the transcripts in `tests/fixtures/web/websocket/`.
Written for 1.0.7 (WebSocket parity, audit `WEBSOCKET_PARITY_AUDIT_2026-11-08`
findings C03 to C06 and C08), read against the web at a1b9e3250.

## Field presence (C03)

A frame leaves out a field the broker did not send. It never sends it as 0
(web openalgo#2376: a client computing the change against a close of 0
showed the whole LTP as the change). A real zero (no volume yet, an OI of 0,
a cleared book side) is sent.

- Each broker packet says what it carries (`NormalizedTick`: its mode's
  fields, less what it marks absent, more what it marks carried; see the
  module doc of `brokers::common::streaming`).
- Within one broker connection the bridge keeps each instrument's last known
  value of every field and merges each packet into it by those marks, never
  by testing a value for zero (`feed/bridge/snapshot.rs`). An LTPC-only,
  OI-only or circuit-only packet keeps the quote it did not repeat, as the
  web's merging adapters do (Upstox `_merge_feed`, HDFC, the Noren cache).
  A connection's new session (a reconnect) forgets what its earlier
  session said, so nothing from before an outage comes back as current;
  only that connection's state goes: a pool opening another connection
  forgets nothing, and the deeper socket's reconnect forgets only its books
  (each event carries its connection's session: `websocket::SessionId`,
  `bridge::snapshot::forget_session`, audit R107-04). Market-state broker
  fields (Dhan's `oi_high` and `oi_low`, Groww's `value`) stay like the
  quote; per-event ones (Upstox's auction figures, Fyers' `update_type`)
  last their packet (audit R107-12).
- The trade time belongs to its price: a new price without a trade time
  drops the old one, and the frame then sends the receive time as `ltt`, as
  the web's adapters do, unless the stream has no trade time at all (an
  index on Sahi), when `ltt` is left out.
- A packet with no traded price is not published; its other fields wait for
  the next price. No frame carries an invented LTP of 0.
- A depth frame before its instrument's first tick carries the book, its
  totals, `ltp` (0) and `timestamp` and leaves every quote field and `ltt`
  out (1.0.6 decision: the SDK's Depth callback reads `ltp` and
  `timestamp`; the web leaves `ltp` out too).
- A book without totals takes the tick's; a book without order counts
  (Upstox) sends levels without `orders`.
- A Depth holder gets only books at its own depth. While a Dhan 20 or
  Fyers 50 subscription streams from the broker's deeper socket, its
  holders get that socket's books (and every tick with the last of them),
  never the main feed's 5 levels in between; a holder at 5 gets the main
  feed's books. After a fallback the deep holder gets the main feed's book.
  Web defect: the web's listener (`server.py` around 2102) sends every
  Depth publish of a symbol to every Depth client, so a Dhan 20 client
  there sees its book alternate between 20 and 5 levels, and a 5 client
  receives 20-level books uncut (`protocol::book_for`, `reaches`; test
  `fanout_tests::a_5_and_a_20_holder_each_get_books_at_their_own_depth`).
- A frame names the levels its book was served at: Dhan's `depth_level`
  says 5 once a 20-level subscription fell back to the main feed (web
  dhan_adapter.py:930, 1042), and Fyers sends no 50-level keys then
  (`DepthBook::levels`, set by the bridge for a broker with a deeper
  socket).
- A book goes out level for level as the adapter carries it: a broker's
  fixed empty slots (price 0, quantity 0: the Zerodha golden fixtures carry
  five a side) are kept, and nothing pads a short book. The levels an
  adapter keeps follow its web adapter (AliceBlue, Groww and Fyers TBT drop
  empty levels as theirs do); the padding some web adapters add beyond
  what the broker sent (Tradejini, Sahi, HDFC Securities) is a web defect
  the desktop does not copy.
- The page's `MarketDataManager` already merges by presence (`??`) within
  one session; the zeros it kept came from the server.

## Change and open interest (C04, C05)

- The change since the previous close goes with every instrument, futures
  and equities included, when the broker sent one or a previous close above
  0 backs it. The canonical `price_change` / `price_change_percent` go on a
  non-index instrument for the brokers whose web adapter publishes a change
  for one (the `instrument_change` flag in `feed/dialect/names.rs`), with
  that broker's names beside them; Zerodha's recorded frames carry the
  change for indices only, so Zerodha equities still carry none. An index
  always carries it when known.
- Open interest goes on Quote frames whenever the broker supplied it, in
  whatever mode its packet came: a Depth packet carries OI, and a Quote
  packet does when the adapter marks it. A Depth frame carries the OI the
  instrument's ticks supplied and never an invented 0.

## Fields beyond the canonical quote (C06)

The desktop sends one canonical set (the names the Zerodha transcript
carries) and, beside it, every key the broker's web adapter publishes for
the same value, from the lowest mode the adapter sends it in upward. Keys
come from three places:

- **Aliases** of canonical values (`last_trade_quantity`, `ltq`, `prev_close`,
  `last_price`, `cp`, `percent_change`, `totalbuyqty`, ...).
- **Shared optional fields** (`QuoteExtras`): circuits, 52-week range, best
  bid and ask with quantities, previous OI, OI change, total OI, the day's
  close, the packet's exchange time. Each is sent under the broker's own
  names only. One the packet did not carry keeps its last value; one the
  packet says is gone (`QuoteExtras::clear`, `clear_best_bid_ask` for a
  book that emptied both sides, as Groww's web adapter clears its best bid
  and ask) is left out until a packet sends it again. A quote field works
  the same way (`mark_cleared`: Tradejini and Samco stop sending the book's
  totals when the Depth holder leaves, and their later Quote frames carry
  none). The packet's exchange time belongs to that packet alone and is
  never sent again with a later one.
- **Broker-only fields** (`BrokerField`, a fixed set): Upstox's auction
  fields, Dhan's `oi_high` / `oi_low`, Groww's `value`.

The canonical keys themselves follow each broker's web adapter too (lead
decisions 2026-11-09, `Dialect` in `feed/dialect/names.rs`; the envelope's
`type`, `symbol`, `exchange`, `mode` and `broker` go out for every broker,
as the web's `server.py` sends them):

- `data.mode`: `"ltp"` / `"quote"` / `"full"` (Zerodha, Sahi, IIFL Capital,
  Nubra), 1 / 2 / 3 (most), or no key (AliceBlue, Arrow, both HDFC, Kotak,
  Rupeezy, Upstox, Fyers). The app's own pages read the envelope's `mode`,
  not this one. In a lower holder's frame from a one-dict broker it is the
  dict's own mode where the web adapter writes the packet's (Paytm, Dhan,
  IndMoney, the XTS adapters but Jainam), and the holder's where it writes
  the subscription's (`mode_of_holder`: mStock and Delta Exchange publish
  once per subscription; Jainam publishes each subscription its own mode's
  dict, so its frames are per mode; Angel and Pocketful write the mode of the
  one subscription they keep per token, which on the web is the latest or
  first subscribed when a token is held in several modes).
- An index's order-flow fields (`last_quantity`, `average_price`, the book
  totals) go out only where the broker's keys put them on an index
  (mStock's fixed packet, `index_key(.., QUOTE_UP)`), under the canonical
  and the broker's names alike; elsewhere an index has none.
- `data.symbol`, `data.exchange` and `data.timestamp`: AliceBlue's dicts
  carry no symbol or exchange, and a timestamp in Depth only.
- `open_interest` beside `oi` only where the adapter sends both (Zerodha,
  Sahi, Rupeezy, Arrow, both HDFC, IIFL Capital; Shoonya sends only
  `open_interest`, in Depth).
- `ltt` where the adapter sends one; not in Zerodha's LTP copy of a richer
  tick (as recorded), never for Fyers, Motilal, Nubra, the Noren family and
  Delta Exchange.
- Number types: Angel's book totals are doubles (1000.0), as its packet
  carries them. A broker's list of doubles (`Dialect::floats`) covers its
  own names as well as the canonical keys, and a packet can mark the JSON
  number form of any of its values for that packet only
  (`NormalizedTick::numbers`: an integer for a whole value, as 5paisa's
  JSON sends 445 and 445.5; a double, as Jainam's and RMoney's 1105 text
  event parses with `float()`). A value kept from an earlier packet goes
  out in the broker's usual form. Groww's `ltt` and `volume` are doubles,
  as the web's protobuf reader gives them.
- A broker not in the table gets the master frame of 1.0.6 (both OI keys,
  `ltt` everywhere, a numeric `data.mode`); a broker lane changes its
  broker's entry when its corpus cases show otherwise.
- The general rule (`feed/dialect/keys.rs`): every canonical key has the
  modes it goes out in, for an index and for any other instrument
  (`Dialect::key`, `instrument_key`, `index_key` with `Modes::range(from,
  to)` or `Modes::NEVER`), apart from the values the broker's own names
  read, so turning a canonical key off never removes a web name; a web
  name is bounded with `nt(key, source, from, to)`. The switches above are
  presets of it (`open_interest`, `no_ltt`, `no_ltt_on_ltp_copies`,
  `no_index_change`, `index_book`, `bare`). No web adapter sends
  `price_change` on an equity, so a listed broker's equity carries the
  change under its own names only.

Deliberate differences, each a web defect or a desktop rule:

- `ltt` is in epoch milliseconds wherever it goes, where some adapters send
  seconds, raw XTS time or text.
- A key whose value is not known is left out, where the web sends a default
  (`.get(x, 0)`) or a placeholder (Delta Exchange's `average_price` and
  `oi_change`, always 0).
- Times: a web alias follows its adapter's unit, type and format, while
  the docs' keys `ltt` and `timestamp` are always epoch milliseconds, and
  no key is ever sent as `null` (a value not known leaves its key out).
  - Aliases: seconds for Fyers' and Dhan's `last_trade_time` (since 1.0.6);
    text for Tradejini's `last_trade_time` (`YYYY-MM-DD HH:MM:SS` as
    Python's `str(datetime.fromtimestamp(seconds))` writes it, in IST,
    `Source::TradeTimeText`, only when the broker sent a trade time) and
    for the raw strings the Noren family and Definedge pass through: the
    Noren family's `last_trade_time` is the time of day, `HH:MM:SS` in IST
    (`Source::TradeTimeOfDay`; from LTP on Shoonya, from Quote on Flattrade,
    Zebu and TradeSmart), and a packet marks the form its broker sent when
    it varies (`NormalizedTick::time_text`: Definedge's `ltt` is sometimes
    the time of day, sometimes epoch seconds as a string). The web writes
    the host's local time; the desktop keeps IST everywhere, so the text
    differs only on a web host not set to IST.
  - `ltt` and `timestamp`: the web's mixed or raw units under those names
    (Zerodha's `ltt` in seconds on full packets, XTS's raw
    `LastTradedTime`, Paytm's and IIFL Capital's seconds, Tradejini's text
    `ltt`) are not copied.
- A web key that holds a wrong value is not copied with that value: Pocketful
  publishes the average price as Depth's `ltp`, Zerodha web reads OI from the
  wrong offset; the desktop decoders keep the right values.
- `token` (Arrow, both HDFC, Rupeezy, Upstox) is the instrument's broker
  token from the symbol master, as text. Fyers names its frames as its web
  adapter does: the exchange's token (the fytoken less its first ten
  digits, `Source::ExchangeToken`, fyers_mapping.py:82) on HSM frames and
  the ticker (`NSE:SBIN-EQ`, the master's `brsymbol`) on TBT 50-level
  frames; an index has no `token` (the web's `""` placeholder).
- Fyers' `update_type` (`BrokerField::UpdateType`, text; a depth packet's
  own on the Depth frame built on it, `NormalizedDepth::broker_fields`) and its 50-level
  frame's `total_buy_qty`, `total_sell_qty`, `feed_time` (the TBT
  packet's own feed time, seconds), `depth_levels` (the levels the book
  holds, its longer side, fyers_tbt_websocket.py:519-530), `is_50_depth`
  (`Source::FiftyLevels`, on frames whose book was served at 50 levels), and
  the `timestamp` Upstox puts inside `depth` (`Dialect::book_time`), are
  carried since 1.0.7.

## Per broker: what the web adapter publishes

Keys beyond the canonical ones, by the lowest mode the web sends them in
(L = LTP and up, Q = Quote and up, D = Depth). "Fills" is what that
broker's desktop adapter must set for the frame to carry the key: a mark on
the tick (`carried`, `absent`), a `QuoteExtras` field or a `BrokerField`.

| Broker | Web keys beyond the canonical ones | `data.mode` | Adapter fills |
| --- | --- | --- | --- |
| aliceblue | Q `change_percent` (its only change key), `total_oi` (its only OI key); the web sends depth `orders` 0, the desktop AliceBlue's own counts (web defect) | none | `carried(OpenInterest)` on a touchline's `toi`, marks for what a message lacks, real order counts |
| angel | Q `last_trade_quantity`; D `upper_circuit`, `lower_circuit`; no change keys on an index | number | `extras.upper_circuit` / `lower_circuit` from SNAP |
| arrow | L `token`, `last_price`, `close`, `prev_close`, `change`, `change_percent`; D circuits; no canonical change on an index (`no_index_change`) | none | close and change in the 17-byte LTPC packet (`carried`), circuits from 241-byte packets |
| compositedge, fivepaisaxts, ibulls, iifl, jainamxts, rmoney, wisdom (XTS) | `ltq` in LTP only (`nt`: Quote and Depth name it `last_quantity`); D circuits, and the canonical `oi` in Depth only; no canonical change on an index (`no_index_change`) | number | `absent` for every field an event lacks, `carried(LastQuantity)` on 1512, circuits from 1502 root fields; Jainam and RMoney mark a 1105 text event's counts `NumberForm::Double` (`float()` on the web); RMoney's 1501 is an LTP message while the symbol is held at LTP only (`XtsHooks::touchline_by_holding`) |
| definedge | Q `change_percent`, `percent_change`, `prev_oi`, `total_oi`, `bid`, `bid_qty`, `ask`, `ask_qty`, `last_trade_time` (the depth feed's `ltt` text); D `ltq`, `ltt_time` (the same text), `total_buy_qty`, `total_sell_qty`, circuits, `52w_high`, `52w_low` | number | `extras` previous OI, total OI, best bid and ask, circuits, 52-week range (done); OI in Quote (`carried`, done); `time_text = TimeOfDay` for a clock `ltt`, never for `ft` (done) |
| deltaexchange | L `open`, `high`, `low`, `close`, `oi` (a double), `bid_price`, `bid_qty`, `ask_price`, `ask_qty`; D `totalbuyqty`, `totalsellqty` (no canonical book totals); depth levels without `orders` | number | best bid and ask from the ticker `q` array, the last book's totals on a ticker; `absent(Orders)` on the book |
| dhan | L `last_trade_time` (s); Q `day_close`; D `prev_oi`, `oi_high`, `oi_low`, `depth_level`; no change keys on an index | number | previous close as `close` and the packet's close as `extras.day_close` (audit P-03), previous OI, `BrokerField::OiHigh` / `OiLow`; a 20-level book has no totals of its own |
| dhan_sandbox | Q `last_trade_quantity` (no `last_quantity`); every instrument alike, an index's average price, book totals and five-level book included; no change keys | number | the simulated feed (`dhan_sandbox::feed`): no broker packet, every field drawn by the web's model |
| firstock | Q `oi`, circuits (Depth's `oi` is the total OI) | number | `carried(OpenInterest)` on Quote, circuits |
| fivepaisa | L `last_trade_time`; Q `last_trade_quantity`, `percent_change`; Quote only `bid_price`, `bid_quantity`, `ask_price`, `ask_quantity`; no `price_change*` on an index | number | best bid and ask; a price 5paisa sends as 0 with none held `absent`; a whole price `NumberForm::Integer` |
| flattrade, shoonya, tradesmart, zebu (Noren) | L `<broker>_timestamp` (s); Q `percent_change` (Noren's `pc`, on every instrument; no `price_change*`); D `depth_level`, circuits, `52_week_high`, `52_week_low`, OI as `open_interest` only (Shoonya), `oi` (TradeSmart, also in Quote) or `total_traded_value` beside the desktop's `oi` (Flattrade, Zebu: a web defect); no `ltt`; Depth only from depth packets (`depth_needs_book`) | number | `extras.exchange_time_ms` from `ft`, circuits, 52-week range; mode by packet (touchline 2, depth 3); keys the snapshot never held marked absent; book slots only as sent; TradeSmart `carried(OpenInterest)` on Quote |
| fyers | L the whole symbol-feed dict: OHLC, `volume`, `oi`, `bid_price`, `ask_price`, `bid_size`, `ask_size` (up to Quote), circuits, `last_traded_time`, `last_trade_time`, `exchange_time` (s), `avg_trade_price`, `average_price`, `last_trade_quantity`, `last_quantity`, book totals, `change`, `change_percent`, `percent_change`, `data_type`, `subscription_mode` | none | best bid and ask, circuits, exchange time, `update_type`; LTP-mode packets carry the quote (`carried`) |
| groww | L OHLC, `volume`, `value`, `broker`, `topic`, `subscription_mode`; D `bid`, `bid_price`, `bid_qty`, `bid_size`, `bid_quantity`, `ask`, `ask_price`, `offer_price`, `ask_qty`, `ask_size`, `ask_quantity`, `offer_quantity` | number | `BrokerField::TradedValue`, best bid and ask from the book |
| hdfcsecurities, hdfcsky | L `token`, `last_price`, `close`, `prev_close`, `change`, `change_percent`; Q `oi` (quote packets); D circuits | none | the packet's mode (`investright::finish`): circuits from full and CIRC packets (`extras`), `carried(OpenInterest)` on derivative quote packets, `absent` volume, order flow and OI on an index, `absent(LastTradeTime)` for a packet without a trade time |
| iiflcapital | Q `bid`, `ask`, `bid_quantity`, `ask_quantity`; an index Q `change`, `change_percent` (docs keys, from the packet's close; the adapter sends none, lead 2026-11-09), no canonical change (`no_index_change`) | label | best bid and ask; OI only when the OI topic sent one; `absent(Change)` on an instrument |
| indmoney | Q `bid_price`, `bid_qty`, `ask_price`, `ask_qty`, `oi_change`, `oi`; no Depth on the web | number | best bid and ask, OI change, `carried(OpenInterest)` |
| kotak | Q `percent_change` (the packet's `nc`), `last_trade_time`; D `totalbuyqty`, `totalsellqty` | none | `carried(OpenInterest)` in Quote (market pictures), the packet's change (`carried`) |
| motilal | L `volume`; Q `ltq` (no `last_quantity`); D circuits; no `ltt`, no change on an index | number | the scrip's quote so far at Quote level (each field group absent until its packet), OI from m (or A for a derivative), circuits from W |
| mstock | L `last_trade_time`, `exchange_timestamp`; Q `prev_close`, `last_trade_quantity`; D circuits; no change on an index; an index's `last_quantity`, `average_price` and book totals from Q when the packet fills them; a lower holder gets the whole packet | number (the holder's) | `extras.exchange_time_ms`, circuits, `mark_cleared` on an index's 0-filled order flow (audit RB: true trade time) |
| nubra | Q `prev_close`; Quote only `bid`, `ask`; no change keys on an index; Depth refused on an index | label | best bid and ask, the message time |
| paytm | L `close`, `change_absolute`, `change_percent`; Q `last_trade_quantity` (Quote only; Depth sends canonical `last_quantity`); Q `52_week_high`, `52_week_low`; D `oi_change`; no canonical change on an index | number | 52-week range, OI change, `carried(Change)` |
| pocketful | L only (`nt`, the compact packet) `change`, `bid_price`, `ask_price`; L `oi`; Q `last_trade_quantity` (no canonical `last_quantity`); no canonical change on an index | number | `carried(Change, OpenInterest)` on compact packets, best bid and ask |
| rupeezy | L `token`, `last_price`, `close`, `prev_close`, `change`, `change_percent`; D circuits; no canonical change on an index (`no_index_change`) | none | circuits from full packets; `absent` last quantity, average and totals on ohlcv packets, trade time when 0 |
| sahi | L `last_trade_time`; Q `oi`, circuits | label | circuits from the price band (none when a tick has none), `carried(OpenInterest)` on Quote ticks with OI, `absent(OpenInterest)` on ticks without; book sides as streamed, no padding; no book event for an index |
| samco | L `last_trade_time`; Q `change`, `change_percentage`; Q only `last_trade_quantity`, `best_bid_price`, `best_bid_quantity`, `best_ask_price`, `best_ask_quantity` | number | best bid and ask; an index's `last_quantity` from Q (no canonical change) |
| tradejini | Q `last_trade_time` (text, IST), `change`, `change_percent`, `percent_change` (only when Tradejini sent `chng`), `oi`; D circuits; no canonical `price_change*` | number | circuits, `carried(OpenInterest)` on Quote, `absent` for every L1 field never sent, `chng` included, `cleared` book totals on the first tick after Depth goes |
| upstox | L only: `ltq`, `cp`; L `indicative_equilibrium_price`; Q `last_trade_quantity`, `last_trade_time`, `reference_price`, `indicative_equilibrium_quantity`, `indicative_imbalance_quantity_total`, `indicative_imbalance_quantity_market`, `cas_eligible`; `oi` a double; depth levels without `orders`; no change keys on an index | none | `carried(LastQuantity, Close)` on LTPC, the auction `BrokerField`s of the tick that carried them (audit P-11), `absent(Orders)` on the book |
| zerodha | Q `exchange_timestamp` (index full packets, s) | label | `extras.exchange_time_ms` on 32-byte index packets, `carried(Change)` on index packets |

## Several modes of one symbol (C08)

The web's listener (`websocket_proxy/server.py` around line 2102 at
a1b9e3250) sends, for each upstream message on a mode topic, one frame per
client, tagged with the highest mode the client holds at or below the
topic's, carrying the adapter's dict unchanged. Most adapters publish one
message per mode topic someone holds (Zerodha: Depth, then Quote and LTP
copies; Sahi: all three), so a client that holds LTP, Quote and Depth of a
symbol receives three frames per broker tick, one per mode. The recorded
transcripts show exactly that (`01_connect_auth_subscribe_flow.jsonl`,
`05_auth_alias_forms_and_mode_labels.jsonl`: mode 3, then 2, then 1, each
shaped for its mode).

The desktop's policy (lead decisions 2026-11-09; the golden transcripts
fix the Zerodha shape):

- **One packet, one update.** A full tick and its book right behind it in
  one broker message go out as one Depth update (`feed/bridge/pairing.rs`);
  the manager publishes each message's events as one packet
  (`websocket::Packet`), so this never depends on what happens to be queued
  or on another connection's events in between (audit R107-17). A book in
  a message of its own (a deep book from its own socket) reaches Depth
  holders only, so no holder gets a packet twice.
- **A full tick without its book** (lead decision 2026-11-09):
  - before the instrument's first book in the connection, a Depth holder
    hears what the broker's web adapter publishes: a frame without `depth`
    where the web publishes the packet on its Depth topic (HDFC and the
    default), nothing where the web is silent (Definedge's touchline,
    5paisa's feed frame, Tradejini's L1 tick, from their web tests:
    `Dialect::depth_needs_book`,
    decided by `protocol::reaches` for the registry and the feed tests);
    nothing stale is shown either way;
  - once a book exists, a Depth holder gets a frame on every full tick,
    with the tick's quote and the last book (`bridge::snapshot::LastBook`,
    `render::snapshot_update`; the Samco ruling). A web adapter that
    freezes Depth holders then is a web defect. A new session of the
    connection that sent the book forgets it. A full packet that says it has no book
    (`NormalizedTick::book_absent`: HDFC's empty depth list, which proto3
    cannot tell from a cleared book) goes out without `depth`, never with
    the old book, which stays for the next packet that carries one.
- **One frame per mode the client holds, for each update**
  (`feed::registry::dispatch`): a Depth update reaches a client's Depth,
  Quote and LTP holdings, a Quote update its Quote and LTP holdings, an LTP
  update its LTP holding. A client holding all three modes receives three
  frames per tick, as recorded; one holding Quote and Depth two; LTP one.
- **Each frame shaped as the broker's web adapter shapes it** (`Fold` in
  `feed/dialect/names.rs`): trimmed to the holder's mode where the adapter
  publishes a dict per mode (Zerodha, as recorded; Sahi, Rupeezy, Upstox,
  ..., AliceBlue, whose LTP dict is `ltp` and `ltt`, and Jainam, whose XTS
  adapter publishes each message to every held mode in that mode's dict);
  carrying the whole packet, its book and circuits included, with the
  dict's own `data.mode`, where the adapter publishes one dict per packet
  (Arrow, Paytm, mStock, the other XTS members, Delta Exchange, Angel,
  Dhan, IndMoney, Pocketful), as the web sends that dict to a lower holder.
- **What the Python SDK sees.** SDK 2.0.5 (`openalgo/feed.py`) calls the LTP
  callback for a frame with `mode` 1, the Quote callback for 2 and the
  Depth callback for 3 (with a book). So each callback runs once per broker
  tick for each mode subscribed, on any broker.
- **Where the web differs, and the desktop does not follow** (web defects):
  1. With a one-dict adapter, a client holding LTP, Quote and Depth gets
     one frame per packet on the web (mode 3): its LTP and Quote callbacks
     never run. RMoney and Wisdom send a 1501 touchline to Quote alone when
     Quote is held, so an LTP holding of that client never hears it either
     (Zerodha fixed its own case, web issue #1664). The desktop sends every
     held mode its frame.
  2. A client that holds LTP only, while another client holds Depth of the
     same symbol, receives on Zerodha web the Depth topic's message folded
     to mode 1 with the whole Depth dict, and a second mode-1 frame from
     the LTP topic: two LTP callbacks per tick. The desktop sends one.
  3. Where an adapter publishes nothing to Depth holders for a full packet
     without a book, its broker lane declares it with `depth_needs_book`
     (Definedge, 5paisa and Tradejini so far); every other broker's Depth holder gets
     the frame without `depth`.

Draft for the web: *The proxy's listener folds each upstream message to the
highest mode a client holds at or below the message's topic, so the
callbacks a client sees depend on which topics the broker's adapter
happens to publish: a client holding LTP alone receives a Depth message
under mode 1 with the full book dict, and on Zerodha a second LTP frame;
a client holding LTP and Quote on RMoney or Wisdom never receives an LTP
frame. Delivering one frame per held mode, each built from the richest
message at or above it and trimmed to that mode's fields, would make every
SDK callback run exactly once per tick on every broker.*
