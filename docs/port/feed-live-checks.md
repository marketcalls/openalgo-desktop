# Feed parity: checks that need a live market-hours session

The feed corpus (`src-tauri/tests/fixtures/feed_corpus/`) proves each broker's
frames against OpenAlgo web's own adapter code offline. A few behaviours can
only be confirmed with a real broker login during market hours (09:15 to
15:30 IST, MCX later): what a broker really sends in a case the documentation
leaves open, and timings no offline test can show. This is the list, by
broker, gathered from the 1.0.7 lane reports. Every check is read-only
(subscribe, watch, unsubscribe) unless it says it needs an order.

Use the built-in pages: `/websocket/test` (and `/20`, `/50` for deeper books)
for market data, the Health page for feed state. Note what you see next to
each item; a difference from the expected result is a bug report with the
broker, symbol, time and the frame text.

## All brokers

- One symbol held at LTP, Quote and Depth by one page: each mode updates; a
  field the broker does not send is absent, never 0.
- Subscribe a symbol, then read the subscribe reply: it arrives before the
  first price.
- Pull the network for 30 s and restore it: prices return, and nothing from
  before the outage is shown as current.

## Per broker

- **Zerodha:** CDS prices in rupees (USDINR near 88, not 8,812,340).
- **Angel:** depth circuit limits present.
- **Upstox:** `currentTs` as the timestamp; the closing-auction fields in
  the closing session; a second connection opens past 1,000 symbols.
- **Dhan:** the day close after 15:30; a 20-level book on an illiquid
  contract falls back to 5 levels after 30 s and says `depth_level` 5;
  Depth 5 and Depth 20 on one symbol keep a steady book size each; the
  disconnect frame at logout.
- **Dhan (Sandbox):** simulated prices every second on a real sandbox login.
- **Fyers:** circuits and best bid/ask on the HSM feed; TBT 50 beside Depth
  5: the ticker as `token`, the levels present, the book's own feed time.
- **Delta Exchange:** the ticker's best bid/ask and the order book on a live
  perpetual.
- **Nubra:** the message time as the timestamp.
- **Kotak:** a subscribe made during login is sent once; the order socket's
  login fallback after 10 s (needs a session that answers slowly).
- **Sahi:** with a sixth streaming app, the 4429 stop and the "close other
  Sahi streaming apps" message, then prices back when an app reconnects.
- **Groww:** an idle feed stays connected without WebSocket pings.
- **IndMoney:** Depth refused; /trading and the depth panels show the quote
  and the note; whether the idle price socket is ever closed by IndMoney.
- **HDFC Sky / HDFC Securities:** a second connection past 300 symbols (Sky);
  whether an empty book list means unchanged or cleared; how long a new
  client waits for its first frame on a quiet instrument.
- **5paisa:** whether whole prices come as JSON integers; zero prices after a
  reconnect; a market order's price and a cancelled order's reason in polled
  order updates (needs an order).
- **Samco:** the index listing-id lookup at a cold index subscribe; MCX
  order symbols (needs an order).
- **Tradejini:** a live compressed frame; the CDS divisor.
- **mStock:** whether full packets carry the trade time in seconds; CDS
  prices; MCX exchange type 5.
- **Motilal:** what the broadcast server does with the logout text frame at
  quit; the OI field for cash scrips.
- **Definedge:** the depth clock format; a refused `ck` sends no order
  subscription.
- **Paytm Money, Pocketful, Arrow, Rupeezy, IIFL Capital:** Pocketful's
  `last_traded_time` epoch and its signed compact change; Paytm's
  concatenated packets; IIFL Capital's OI after a reconnect.
- **Shoonya, Flattrade, Zebu, TradeSmart:** the silence watchdog on a real
  quiet session (no false reconnect after hours); whether frames always
  carry the exchange; real `toi` and `oi` values on an NFO contract.
- **XTS (CompositEdge, 5paisa XTS, IBulls, IIFL, Jainam, RMoney, Wisdom):**
  which shape a 1501 has; whether Depth 20 carries 20 levels; whether 1510
  and 1505 arrive unasked; whole prices as JSON integers; compressed binary
  packets on Jainam and RMoney; an unsubscribe the broker answers; a
  reconnect after the server ends the Socket.IO namespace.
- **Firstock:** its zero and sentinel book slots.
- **AliceBlue:** a df level sent as 0/0; whether dk/df carry the order
  counts.
