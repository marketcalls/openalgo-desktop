# Changelog

All notable changes to OpenAlgo Desktop are recorded here. Versions follow
[Semantic Versioning](https://semver.org/).

## 1.0.8

OpenAlgo web's work of 11 and 12 November. Order alerts can now go to Slack
and Discord as well as Telegram and WhatsApp, from one Alerts page where you
choose which events each channel gets; Flow workflows and AI clients can post
there too. Sahi signs in with TOTP API keys. Also: Flattrade and Kotak Neo
margin figures, Paytm Money stop orders and cancels, market price protection
for stocks whose symbol ends in CE or PE, an order ticket that is ready as
soon as a broker login on the sign-in card completes, and openalgo-charts
2.7.2 with Elliott waves. If you run 1.0.7, OpenAlgo offers this update in
the app; choose Update when no live strategy or active workflow is running.

### Alerts

- One **Alerts** page in the profile menu replaces the Telegram Bot and
  WhatsApp Bot entries. It covers Telegram, WhatsApp, Slack and Discord:
  switch each on or off, send a test message, and choose which events it
  gets (orders placed, modified and cancelled, positions closed, basket,
  split and options orders, GTT orders). Telegram and WhatsApp keep sending
  every alert they sent before until you change them
  (marketcalls/openalgo#2435).
- Order alerts can go to a Slack or Discord channel through a webhook you
  save on the new Slack and Discord pages. The address is kept encrypted,
  never shown again and never written to a log. Only Slack's and Discord's
  own addresses are accepted, a post never goes to your computer or home
  network, and nothing in an alert can ping a whole channel
  (marketcalls/openalgo#2435).
- Your programs can post to Slack and Discord with your API key, at
  `/api/v1/slack/notify` and `/api/v1/discord/notify`, 30 a minute each, as
  on OpenAlgo web. Chart alerts on /trading can go there too
  (marketcalls/openalgo#2435).
- WhatsApp GTT alerts now name the symbol, triggers and trigger ID instead
  of a bare "Order Update"; other WhatsApp alerts read as before
  (marketcalls/openalgo#2435).

### Flow

- New Slack Alert and Discord Alert nodes (63 node types). The message is
  posted in the background, and one longer than the channel takes is
  shortened (marketcalls/openalgo#2440).

### MCP

- AI clients can post to your Slack and Discord channels with
  `send_slack_alert` and `send_discord_alert`; a Read only token can too, as
  with Telegram (marketcalls/openalgo#2440).

### Brokers

- Market price protection: a stock whose symbol ends in CE or PE (RELIANCE,
  BAJFINANCE, REDTAPE and others) now gets the equity buffer, not the
  options one. A MARKET buy of RELIANCE at 1339.7 is sent at 1346.4 instead
  of 1353.1. Contracts the master lists as options (OPTIDX, OPTSTK and the
  others) get the options buffer on GTT orders
  (marketcalls/openalgo#2446).
- Sahi: TOTP API keys can sign in. Login with Sahi opens a short card with
  one optional field: enter your authenticator code for a TOTP key, or leave
  it blank for a Regular key you approved in Sahi's API Dashboard today.
  Nothing about the code is saved. A wrong code, an unapproved key and too
  many attempts each say what to do (marketcalls/openalgo#2441, issue
  #220).
- Sahi: an order with a product the segment does not take (CNC on NFO, NRML
  on NSE) is refused before it is sent, naming the product to use
  (marketcalls/openalgo#2441).
- After you log in on a broker's sign-in card (Angel One, Kotak, 5paisa and
  the other card brokers, and Samco), OpenAlgo loads your API key, the
  broker's features and the trading mode before the dashboard opens, so the
  order ticket is ready at once (marketcalls/openalgo#2423).
- Flattrade: the margin calculator shows the basket's own margin, without
  the margin your open positions and orders already hold, as Flattrade's
  basket screen does with "Include existing margin" unticked
  (marketcalls/openalgo#2436).
- Kotak Neo: the margin calculator shows each order's own margin instead of
  adding your account's existing usage to every leg. A basket with a leg
  Kotak cannot price now fails with Kotak's reason instead of showing the
  margin of the other legs, or 0 (marketcalls/openalgo#2436, #2439).
- Flattrade, Kotak Neo: a basket with a leg that cannot be checked names the
  leg and asks you to check the symbol, exchange and expiry date
  (marketcalls/openalgo#2436, #2439).
- Paytm Money: SL-M orders are sent as stop orders with their trigger price.
  A stop order without a trigger price is refused before it is sent, on
  place and on modify (marketcalls/openalgo#2404).
- Paytm Money: an open (resting) order can be cancelled, and Cancel All
  cancels it too. A cancel that cannot go through says why
  (marketcalls/openalgo#2374).
- Tradejini: an order Tradejini accepted without an order id says to check
  the order book before placing it again, as OpenAlgo web does
  (marketcalls/openalgo#2444).
- Broker Configuration: the broker list and the API plan list can be read in
  dark mode (OpenAlgo web ca83d3062).

### Charts

- `/trading`, Historify and every chart now use openalgo-charts 2.7.2 (from
  2.6.9). The drawing tool icons on the rail are redrawn, each with an open
  ring at every point you place.
- The Patterns group has an Elliott waves section with five counts: impulse,
  correction, triangle, double combination and triple combination. Click the
  origin first, then the end of each wave. Waves are numbered from the
  second point, in parentheses. An impulse or correction drawn in 1.0.7
  loads with its origin on the end of its first wave; drag that point away
  to draw the first wave (OpenAlgo web afc3e133f, 169bf19d2, 12bb3069b).

### Security

- Telegram `/mode` tells the trading mode only to a chat linked with your
  current API key. After you generate a new key, send `/link` again
  (OpenAlgo web 81fe8eb41).
- OpenAlgo web's November security fixes were checked against the desktop.
  Live-update connections already needed a signed-in session, `/link`
  already checked the key on this computer, the desktop has no broker
  postback addresses, and a forgotten password is replaced by starting
  fresh in the desktop window, so there is no reset link to reuse. Tests now
  hold each of these.

### Known limitations

- Sound alerts and native notifications (issue #222) are not in this
  release.
- Some broker behaviour can only be confirmed during market hours with a
  real login; OpenAlgo follows OpenAlgo web there, and the remaining checks
  are listed in docs/port/feed-live-checks.md.

## 1.0.7

WebSocket parity with OpenAlgo web across all 38 brokers. Every price,
quantity, time, order book level and order update a broker sends now reaches
your apps on port 8765 as OpenAlgo web delivers it for that broker: the same
keys and names, the same numbers and units, and nothing the broker did not
send. Each broker is tested against OpenAlgo web's own adapter code on the
same broker messages; where OpenAlgo web is wrong (a wrong unit, a made-up 0,
lost or stale data), the desktop keeps the right value and the difference is
recorded. Subscribe and unsubscribe now tell the truth, live prices are spread
over several broker connections as on OpenAlgo web, and Dhan Sandbox streams
simulated prices. If you run 1.0.6, OpenAlgo offers this update in the app;
choose Update when no live strategy or active workflow is running.

### Live market data, every broker

- A price, quantity or time your broker did not send is left out of the frame
  instead of arriving as 0, in every mode; a real 0 still arrives as 0. No
  field ever arrives as null.
- A broker message without a traded price no longer reaches LTP or Quote
  subscribers with a price of 0; its order book still reaches Depth
  subscribers.
- When a broker sends only part of a quote, the rest from earlier in the same
  connection is kept. After a reconnect, nothing from before the outage is
  sent as current.
- The change since the previous close now comes with equities and futures as
  well as indices, under the names your broker's OpenAlgo web feed uses.
  Brokers whose OpenAlgo web feed sends no change on an index send none.
- Open interest reaches Quote subscribers whenever your broker sends it, and
  Depth no longer shows an open interest of 0 the broker never sent.
- Circuit limits, the 52-week range, best bid and ask, previous OI and OI
  change, the day's close and each broker's own extra fields arrive under the
  names your broker's OpenAlgo web feed uses, in the modes it sends them.
- `data.mode`, number types (445 or 445.0) and text times (Tradejini, the
  Noren brokers, Definedge) follow your broker's OpenAlgo web feed. `ltt` and
  `timestamp` are always epoch milliseconds.
- Market depth shows exactly the levels your broker sent: no added rows of
  zeros, and an emptied side is empty. With Depth 5 and Depth 20 (or 50) held
  on one symbol, each subscription gets only books of its own depth.
- With LTP, Quote and Depth held on one symbol, each update reaches each mode.
  Once a symbol's book has arrived, Depth subscribers hear every update with
  it.

### Subscriptions and connections

- A subscribe is answered symbol by symbol, in your broker's OpenAlgo web
  words: an unknown symbol, a depth or mode your broker does not offer, an
  exchange it does not stream, or a full feed. You no longer get a success
  for a symbol that never streams, and the answer always comes before the
  symbol's first prices. Flow, the sandbox and strategies get the same
  answer.
- Unsubscribing answers success only once your broker has stopped streaming
  the symbol. If the broker refused, or the connection dropped first, the
  symbol stays subscribed, the answer says so, and unsubscribing again
  retries. A symbol listed twice in one unsubscribe keeps its subscription
  if the broker refused, and an answer that comes after the wait is still
  honoured. Asking again while the broker has not answered waits for that
  answer instead of reporting success.
- One slow or refused subscribe no longer holds up other apps' subscribes
  or the sandbox's prices, and an app waiting on its own subscribe is not
  disconnected for being silent.
- Live prices are spread over several connections to your broker, as
  OpenAlgo web does: up to 1,000 symbols per connection and 3 connections.
  Brokers with tighter limits keep to them: HDFC Sky 300 per connection,
  Upstox and HDFC Securities two connections, and one session for 5paisa,
  Flattrade and the XTS brokers. A connection opening or reconnecting
  affects only that connection's symbols, and a deep book's own connection
  reconnecting clears only those books.
- A symbol watched at 5 levels in one app and 20 in another streams 20
  levels, and goes back to 5 when the 20-level app lets go. A 20- or 50-level
  book that does not start, is refused or stays silent (Dhan: 30 seconds) is
  served from your broker's 5 levels, and says so. A deep book from before an
  unsubscribe, or from a connection that has gone down, is never shown as
  current.
- A strategy or scalping stop whose subscribe was partly refused streams what
  the broker accepted, and letting go no longer stops a symbol another app
  still holds.
- A Flow Depth node reads the traded price, not 0, on brokers that send the
  book and the price separately, and a book with no price no longer answers
  an LTP or Quote node.
- A symbol subscribed while the broker is still confirming the login is no
  longer sent to the broker twice (Kotak, Fyers, Firstock and others).
- Before a logout, broker switch or quit, OpenAlgo signs off from the
  broker's feed the way OpenAlgo web does (Dhan, Motilal, Groww, IIFL
  Capital).
- Health shows where your order updates come from, which subscriptions are
  still being set up or released, and each feed connection with its symbols,
  and warns when any one connection has stopped streaming.
  When a broker ends the feed because too many apps stream on your login,
  Health says so in the broker's words, and prices return as soon as an app
  reconnects, without logging out.

### Brokers

- Zerodha: currency (CDS) prices are in rupees; index frames carry the
  exchange time and the change.
- Angel: market depth carries the circuit limits.
- Upstox: frames carry Upstox's own time stamp, the closing-auction fields,
  open interest in Quote, and book levels without an order count of 0.
- Dhan: the previous close and the day's close are told apart; previous OI
  and the day's OI range are carried; a 20-level book no longer shows the
  sum of its visible levels as the day's totals; an exchange Dhan does not
  stream is refused with Dhan's words; the 51st 20-level book is refused;
  the day's OI range stays between updates; prices arrive as the tick Dhan
  sent (954.1, not 954.0999755859375).
- Dhan (Sandbox): streams simulated prices every second, as OpenAlgo web
  does. They are not market prices; the Select Broker card, the Broker
  Configuration page and the user guide say so.
- Fyers: best bid and ask, circuit limits, the exchange time and
  `update_type`; the token is the exchange's; 50-level depth carries the
  levels it holds and its own feed time.
- Delta Exchange: ticker updates refresh market depth and carry the best bid
  and ask; nothing from before a reconnect is shown as current.
- Nubra: frames carry Nubra's own time; Depth on an index is refused as on
  OpenAlgo web; an emptied book clears the best bid and ask, and a book that
  arrives before the first price still reaches Depth.
- Kotak: market depth shows only the levels Kotak sent; quotes carry OI and
  the exchange's percent change; order updates retry the other login format
  after 10 seconds, and market data retries an unanswered login after 15.
- Sahi: Quote and Depth carry the circuit limits; F&O carry OI in Quote and
  stocks and indices no longer show an OI of 0; a connection limit waits as
  OpenAlgo web does, then asks you to close other Sahi streaming apps; an
  expired session asks you to log in.
- Groww: LTP carries open, high, low, previous close, volume and traded
  value; depth carries the best bid and ask under OpenAlgo web's names.
- IndMoney: quotes carry the best bid and ask, OI and its change and
  IndMoney's own time; Depth is refused as on OpenAlgo web, and the /trading
  chart, depth panels and Place Order dialog fall back to the quote and say
  IndMoney does not stream market depth; order updates for orders placed
  outside OpenAlgo carry the order book's id.
- HDFC Sky and HDFC Securities: the exchange's time and trade time in
  milliseconds, circuit limits in depth, no index zeros; HDFC Securities
  stops prices and asks you to log in when HDFC ends the session, and a
  mode change no longer drops and re-adds the symbol at HDFC.
- 5paisa: best bid and ask in Quote, whole prices as 5paisa sends them, and
  market depth with only the levels 5paisa sent; a partial update no longer
  turns volume, last quantity or the buy and sell totals into 0.
- Samco: indices stream from the first subscribe; best bid and ask in Quote;
  market depth waits for Samco's first book.
- Tradejini: circuit limits in market depth, a future's OI in Quote, and the
  change as Tradejini sends it.
- mStock: the exchange time and circuit limits; an index's order flow where
  mStock sends it.
- Motilal: market depth starts with the first real book level, with circuit
  limits; LTP carries the day's volume. Where a cash share and an F&O
  contract share Motilal's code, the second is refused with the reason,
  because Motilal's packets cannot tell the two apart; OpenAlgo web shows one
  instrument's prices as the other's. A symbol you let go frees its code at
  once, even during an outage, and one you still hold keeps it for as long as
  you hold it.
- Definedge: best bid and ask and previous and total OI in Quote; circuit
  limits and the 52-week range in depth; order updates start only after
  Definedge accepts the session.
- Arrow, Rupeezy, Paytm Money, Pocketful and IIFL Capital: circuit limits,
  the 52-week range, best bid and ask, change and OI where each sends them;
  Arrow's order updates find the symbol from the instrument token.
- Pocketful: Depth shows the real last price and carries the average price
  as `average_price`, instead of showing the average as the LTP. A Depth
  subscription now streams Pocketful's quote data beside the book, and
  `/api/v1/depth` returns the last price too. Changing between Quote and
  Depth changes only the stream that differs.
- Paytm Money, IIFL Capital and Pocketful: before an instrument's first
  trade, frames leave out `ltt` instead of showing the time the update
  arrived. Firstock and AliceBlue do the same until a connection has sent a
  trade time.
- Shoonya, Flattrade, Zebu and TradeSmart: a feed that keeps answering but
  stops sending prices is reconnected after three minutes, once prices have
  been flowing; frames carry Noren's own percent change, the feed time, the
  trade time, circuit limits and the 52-week range; prices arrive even when
  a frame leaves out the exchange.
- CompositEdge, 5paisa XTS, IBulls, IIFL, Jainam, RMoney and Wisdom: a
  price-only update no longer blanks the rest of the quote; Depth 20 on NSE
  and NFO streams twenty levels; a session the server ends while the
  connection stays open is reconnected; RMoney's other message layouts
  stream; Jainam and RMoney depth arrives without a message code; RMoney's
  emptied book clears wherever its message carries it. A subscribe the
  broker refuses is answered as refused, with the broker's reason, including
  one made before the feed has connected; one refused symbol in a batch no
  longer stops the others, and after a reconnect only what the broker refuses
  is dropped, listed on Health. Changing a symbol's mode takes the new stream
  before letting go of the old one, so a refused change leaves the stream you
  had and says so, and another app's prices never stop.
- Firstock: prices start reliably after login, and invalid book levels no
  longer replace a good book.
- AliceBlue: market depth shows each level's order count; order updates show
  MIS, CNC or NRML; the order feed retries five times before saying it is not
  enabled.
- Order updates polled from the order book (5paisa, Samco, Groww) show a
  market order's price as 0.0 and a rejection reason only for a rejected
  order.

### Security

- The database engine is upgraded to SQLite 3.54.1 (RUSTSEC-2026-0459), and
  the MCP library to 3.9.0 (RUSTSEC-2026-0462).

### Known limitations

- Sound alerts and native notifications (issue #222) and the Sahi sign-in
  with TOTP API keys (issue #220) are not in this release.
- Some broker behaviour can only be confirmed during market hours with a
  real login (for example what an empty HDFC book list means, or how XTS
  shapes a nested quote); OpenAlgo follows OpenAlgo web there, and the
  remaining checks are listed in docs/port/feed-live-checks.md. Pocketful's
  documentation does not say whether its two streams can run on one
  instrument; Depth on Pocketful depends on that and is among those checks.

### Maintenance

- When many things were being saved at once, a save could fail with "database
  is locked", on Windows most of all. A save that is waiting now gets its
  turn.
- Live market data, and the answer to each subscribe and unsubscribe, are now
  tested broker by broker against OpenAlgo web's own handling of the same
  broker messages, and every difference is recorded with its reason and the
  decision that allows it
  (docs/port/feed-parity-matrix.md). A release is refused while any
  difference has no decision behind it.

## 1.0.6

OpenAlgo web's November broker fixes. Live prices from thirteen brokers now
leave out a value the broker's feed does not carry instead of showing 0, so
an app that works out the change against a previous close of 0 no longer
shows the whole price as the change. Also: Kotak, Shoonya, Dhan, CompositEdge
and RMoney fixes, a Live strategy that stays with the broker its first order
went to, Historify history at session opens, the Chrome Plugin and Excel
Add-in on the Download page, and every open code-scanning alert fixed. If you
run 1.0.5, OpenAlgo offers this update in the app; choose Update when no live
strategy or active workflow is running.

### Live market data

- The WebSocket feed leaves out a quote field your broker's feed does not
  carry, as OpenAlgo web now does, instead of sending 0. A 50-level Fyers or
  20-level Dhan book that arrives before the symbol's first quote carries its
  price, totals and levels only. Frames also carry the names each broker's
  OpenAlgo web adapter uses beside the usual ones: `last_trade_time` (Sahi,
  Kotak, Fyers, Dhan, Upstox, mStock, Samco, 5paisa) and `percent_change`
  (Fyers, Kotak, Definedge, Tradejini, 5paisa) (marketcalls/openalgo#2376).
- Sahi: Quote and Depth no longer show open, high, low, previous close,
  average price, last quantity or order book totals as 0. Sahi's live feed
  does not carry them, so they are left out, and an index no longer gets a
  made-up trade time (marketcalls/openalgo#2376).
- Fyers: market depth carries the full quote and Fyers' own order book
  totals, and a book that arrives before the first quote is sent again,
  complete. A value Fyers did not send is left out instead of 0, and indices
  no longer show a trade time, volume or open interest of 0
  (marketcalls/openalgo#2376).
- Kotak: the first prices before a full quote no longer show open, high, low
  and volume as 0, and there is no trade time before the first trade. Market
  depth shows the whole book Kotak sent, without levels the exchange had
  already removed. NIFTY and NIFTY50 both stream, and BSE currency
  derivatives (BCD) now stream at all (marketcalls/openalgo#2376).
- Dhan: a quote's close is the previous day's close all session, as Dhan
  sends it when you subscribe. After the market it no longer becomes the
  day's own close, and until Dhan has sent it the close is left out instead
  of shown as 0 (marketcalls/openalgo#2377).
- Upstox: quotes keep every value Upstox has sent instead of dropping open,
  high, low, average price and totals to 0 between ticks. The close is the
  previous day's close, volume is the day's traded volume, and market depth
  carries the full quote. Moving a symbol between LTP and Quote no longer
  drops it from the feed for a moment (marketcalls/openalgo#2377).
- Groww: index prices (NIFTY, SENSEX and others) now stream live. A quote
  shows what Groww has sent and leaves out the rest instead of zeros.
  Subscribing to MCX, NCO, CDS or another exchange Groww does not stream now
  says so (marketcalls/openalgo#2341, #2377).
- Zerodha: index quotes in Depth mode no longer show open interest as 0, and
  a tick always carries the mode its packet was sent in
  (marketcalls/openalgo#2361).
- Definedge: a quote also takes Definedge's depth feed, so open, high, low,
  previous close, last quantity, trade time and the book totals arrive even
  when the first quote came without them. A value Definedge has not sent is
  left out instead of shown as 0 (marketcalls/openalgo#2383).
- mStock: the last trade time is the full packet's own, and it is left out
  before the first trade instead of showing when the quote arrived
  (marketcalls/openalgo#2383).
- Tradejini: quotes no longer show total buy and sell quantity as 0 before a
  depth book has come. Market depth refreshes with every price change and
  waits for the price before showing a book (marketcalls/openalgo#2383,
  #2341, #2359).
- Samco: live prices carry the exchange's last trade time, not the time
  OpenAlgo received them, and leave it out when Samco sends none. Average
  price and total buy and sell quantity appear once Samco sends them, a total
  of 0 included, instead of showing 0 before. A price for one segment is
  never shown under the same code on another (marketcalls/openalgo#2396,
  #2397).
- 5paisa: live prices and market depth no longer mix up two instruments that
  share a scrip code (NSE and BSE, or cash and F&O). Market depth shows the
  latest open, high, low, close, volume, average price and trade time. An
  open-interest update no longer shows a quote with volume 0, and a missing
  trade time is left out instead of shown as now (marketcalls/openalgo#2396,
  #2397).
- HDFC Sky: circuit-limit and open-interest updates no longer blank the live
  price, OHLC and volume. After a reconnect or a new subscription, nothing
  old is shown until the first full quote (marketcalls/openalgo#2396, #2397).
- Flattrade, Shoonya, Zebu, TradeSmart: live market data keeps reconnecting
  through a long network outage, as it already did; OpenAlgo web's Flattrade
  gave up after about seven minutes, and tests now pin it here
  (marketcalls/openalgo#2324).

### Brokers

- Shoonya: the last candle of each day in 3-minute to 4-hour history is right
  again. Shoonya folded the next morning's pre-open price into it when a
  request spanned several days (marketcalls/openalgo#2343, #2360).
- Kotak Neo: holdings show the right symbol, margin errors show Kotak's
  reason, and the average price uses the decimals Kotak gives. A delivery
  position carried overnight is valued at what it cost, from Holdings, and a
  position still valued at the previous day's settlement says so in the
  position book (`average_price_basis`) (marketcalls/openalgo#2365, #2367).
- Dhan: a login is accepted only for the account Dhan confirms it belongs
  to, and an access token is accepted from the login form, never from a link
  (marketcalls/openalgo#2337).
- CompositEdge and RMoney: a login for another account than your market data
  keys is refused, and RMoney's account is confirmed with RMoney itself
  (marketcalls/openalgo#2337).
- Tradejini: the instrument download no longer fails on Tradejini's new ETF
  group. Indices are typed EQ, and BCD, which Tradejini does not list, is no
  longer offered. Charts load again for contracts whose history held a bar
  outside its own high and low, and bars missing a price are dropped instead
  of drawn at 0 (marketcalls/openalgo#2341, #2359).

### Strategies

- A Live strategy that started before you logged in to your broker now stays
  with the broker its first order went to. A login to another broker while a
  Live strategy still trades elsewhere is refused, with what to stop first
  (marketcalls/openalgo#2337).
- A pending strategy stop says "Stop requested" once, not every few seconds,
  and a stop or target hit while OpenAlgo cannot reach the strategy's broker
  is recorded once (marketcalls/openalgo#2337).

### Historify

- Scrolling back on a Historify chart at 3-minute, 5-minute, 15-minute or
  1-hour bars loads older history again; it stopped at the first bar of a
  session (marketcalls/openalgo#2358, #2310).

### Download page

- The Download page lists FastScalper, the Chrome Plugin and the Excel
  Add-in, each with what it does, how to install it, its download and what
  it needs from OpenAlgo (an API key and this app's address). The OpenAlgo
  Desktop card says new versions arrive as an update in the app, and links
  to the release page for installing on another computer.

### Security

- Select Broker no longer shows text taken from its address. After a failed
  broker login OpenAlgo keeps the reason itself and shows it once, only in
  the browser session that came back from that login, so a link someone else
  made cannot put words on the page (marketcalls/openalgo#2337).
- Every open code-scanning alert is fixed (eleven, all in Flow). None was a
  leak: ten were test code that the scan was not leaving out, and one was a
  variable named after CPython's that the scan reads as an encryption value.
  A check now refuses a scan setting that would leave out app code.

### Fixes

- Windows: when Windows has reserved the app's port (Hyper-V, WSL, Docker),
  the start page now says so and tells you to pick another port such as 5001
  and update your broker app's redirect URL. The troubleshooting guide shows
  how to check.
- Select Broker shows a failed broker sign-in at the top of the page when
  that broker has no card; before, it was not shown at all
  (marketcalls/openalgo#2337).

### Maintenance

- Tauri 2.12.5, uuid 1.29 and webpki-roots 1.0.11 (which follows Mozilla's
  root store), and the CI tooling updates.

### Documentation

- The API reference covers Sector Rotation (`/api/v1/sectorrotation`, served
  since 1.0.4), what a 502 "may have been placed" answer means and when to
  check the order book, and how a GTT OCO behaves on each broker (on Angel
  One it is two separate rules). The WebSocket pages say which fields can be
  absent. The Tools chapter lists all 17 tools.

### Known limitations

- The Windows reserved-port message covers the app's own port; the market
  data feed's port (8765) still shows the general "could not open the port"
  message in that case.
- Upstox market depth levels still carry `orders: 0`, where OpenAlgo web now
  leaves the key out.
- HDFC Sky streams up to 300 instruments per connection; past that, the
  desktop does not yet tell you.
- Sound alerts and native notifications (issue #222) are planned for 1.0.7.

## 1.0.5

Flow arrives: build trading workflows on a canvas at `/flow`, run them now,
on a schedule, on a price alert, on an order update or from a TradingView or
other webhook, and import or export the same JSON as OpenAlgo web. Every one
of the web's 61 node types is here, checked against recordings of OpenAlgo
web's own Flow engine, and the Agent can now build and save workflows for
you. Also from OpenAlgo web: Fyers market depth fixes and a newer AI model
list for the Agent. If you run 1.0.4, OpenAlgo offers this update in the
app; choose Update when no live strategy or active workflow is running.

### Flow

- The Flow editor at `/flow` (Profile menu, Flow Editor): drag nodes from
  the palette, wire them, set each node in its panel, save, and run with
  Run Now. The run log shows every step; a failed run keeps its log.
- 61 node types, as on OpenAlgo web: orders (Place, Smart, Split, Modify,
  Cancel, Cancel All, Close Positions, Basket, Options, Multi-Leg Options),
  market data and account (quotes, depth, history, indicators, option chain,
  symbols, expiries, holidays, timings, books, funds, margin, strategy P&L),
  conditions and logic (position, fund, price, variable and time checks,
  AND, OR and NOT gates, variables, maths, delays, Wait Until), alerts
  (Telegram, WhatsApp, HTTP request) and live prices (subscribe and
  unsubscribe).
- Triggers: schedules (interval, once, daily, weekly, in India time), price
  alerts, order-update watches and webhooks. Active triggers come back when
  OpenAlgo starts.
- Webhooks reach OpenAlgo through the built-in ngrok tunnel at
  `/flow/webhook/<token>`, and the editor shows the tunnel's address while it
  is on. A secret is always required, repeated wrong secrets lock the
  webhook for a while, there is a rate limit, and secrets never reach a log.
  A plain-text TradingView alert works with the secret in the address.
- In analyzer mode every order a workflow places goes to the sandbox.
- Import a workflow exported from OpenAlgo web, and export one back; exports
  never carry your API key or webhook secret.
- The Agent can check a workflow and save it to Flow (validate_flow,
  save_flow), always saved inactive, with the flow-builder skill and the full
  workflow format to read.
- Signed out, nothing runs: webhooks are refused and each schedule, price
  alert or order watch that fires is recorded as skipped. Signed in with no
  broker logged in, workflows run; a step that needs the broker stops with
  "Broker session is not available or has expired".
- Quitting or restarting for an update names the workflows that are active
  or running.

### Fixes to OpenAlgo web's Flow

These behave differently from OpenAlgo web on purpose; each is tested.

- Close Positions for one symbol closes only that position; the web closed
  every open position.
- Basket stop-loss rows keep their trigger price, and lower-case basket
  symbols work.
- A condition inserted on a wire continues from its true branch, so the
  order after it runs only when the condition holds.
- Fund Check, Position Check and Price Condition stop when they cannot read
  a real value instead of deciding on zero.
- Schedules: Once runs at its time and then stops, Daily keeps to its days,
  and every time is India time on any computer.
- Time windows, time conditions and Wait Until accept `{{variables}}` and
  stop with a clear message when one does not resolve.
- An AND/OR/NOT gate with fewer inputs wired than it is set for is refused
  at import and activation.
- Bar Offset on weekly and monthly charts skips the week or month still in
  progress; an indicator with no value on the latest bar says so.
- HTTP Request reaches only public addresses (Tailscale and other 100.64
  ranges included), never follows a redirect and reads at most 5 MB.
- Stock options declared on NFO or BFO are priced from the stock.
- The custom-leg editor keeps a `{{variable}}`'s case so it resolves.
- Validation errors always come in the same order.

### Market rules

- Index freeze limits from Monday 2 November 2026 (NSE circular
  NSE/FAOP/77189): NIFTY 1,800, BANKNIFTY 900, FINNIFTY 1,800, MIDCPNIFTY
  2,800, NIFTYNXT50 600 and NIFTYFPI 8,500, down from the October limits.
  The symbol, search and option symbol lookups show them, and scalping exits
  go out in whole lots within them (a NIFTY exit in orders of 1,755, 27 lots
  of 65). A limit you set on the Freeze Quantity page still wins
  (marketcalls/openalgo 8e4241127).
- Diwali Muhurat trading on Sunday 8 November 2026 runs 14:20 to 15:30 IST,
  with NFO and BFO to 15:40 and NSE commodities (NCO) included, as the
  exchanges published it. Earlier builds held an estimate of 18:00 to 19:15
  (MCX to 00:15); the app moves an estimate still in place to the published
  session when it starts, and keeps a session you set on the Holidays page.

### Brokers

- Fyers: market depth shows the last traded price, not a price halfway
  between the best bid and ask, and indices in depth mode no longer show
  made-up order book levels (marketcalls/openalgo#2318, #2323).

### Security

- A browser tab or login from before the daily 03:00 IST session end can no
  longer read your API key or keep receiving live updates, even in the
  seconds before the app signs it out (marketcalls/openalgo e2bdd061e).
- follow-redirects is updated to 1.16.2 for a high-severity advisory
  (GHSA-mrwp-f3q8-3g9p). The app's pages never used it: it is listed by a
  library they use, for servers only.

### Agent

- The model list includes the newest ChatGPT plan, OpenAI and Anthropic
  models (LiteLLM 1.106.6 catalogue).
- Approve and Reject on an approval card stay greyed out until the Agent's
  reply has finished. A click a moment earlier did nothing and showed
  nothing; OpenAlgo web has the same gap.

### Fixes

- A first start on a new data folder no longer logs "database is locked".

### Known limitations

- Schedules run while OpenAlgo is open; a run more than 60 seconds late,
  including one that fell due while OpenAlgo was closed, is skipped, as on
  OpenAlgo web.
- Python Strategies (`/python`) and the pandas backtesters stay on OpenAlgo
  web.
- Indicator timestamps written day-first (`11-02-2025`) or with month names
  are refused rather than guessed.
- The run log lists a result's fields in alphabetical order; OpenAlgo web
  lists them in the order its code built them.

## 1.0.4

The Agent arrives: ask about your account and the market in plain words, on
the `/agent` page or in the Assistant panel of the `/trading` terminal, on a
model you choose. It reads OpenAlgo's own data, draws charts, computes 127
indicators and asks for your approval before every order. Also from
OpenAlgo web: a new broker, Sahi; the Sector Rotation tool; every chart on
openalgo-charts 2.6.9; and fixes for Dhan, Angel, Groww and RMoney. Links
to web pages now open in your browser. If you run 1.0.3, OpenAlgo offers this update in the
app; choose Update when no live strategy is running. On a Linux `.deb`
install, download the new `.deb` from the Releases page: the notice's link
did not open before 1.0.4. The update sets up the Agent's storage in your
data by itself, after its usual backup; nothing else changes until you set
up a model.

### The Agent

- Open it from the profile menu, Agent. Set up a model first in Admin,
  Agent Config. The Agent works through your OpenAlgo API key, so generate
  one on the API Key page if you have none.
- Models from OpenAI, Anthropic, Ollama on your own computer, any
  OpenAI-compatible server (LM Studio, vLLM and similar), and other
  providers by their prefixed model name, such as `gemini/`, `groq/`,
  `deepseek/`, `mistral/`, `xai/` or `openrouter/`. Test checks a model
  before you use it, and each answer shows the tokens used and an estimated
  cost.
- A ChatGPT Plus or Pro plan: sign in once in Agent Config, and `chatgpt/`
  models run on your plan instead of API credits. The sign-in page opens in
  your browser.
- Model keys and the ChatGPT sign-in are kept encrypted in OpenAlgo's data
  on your computer, never in a file, and are never shown again once saved.
- It answers from OpenAlgo's own services: quotes, depth, history, option
  chains, Greeks, symbols and expiries, funds, the order, trade and position
  books and holdings. In analyzer mode it reads your sandbox account.
- In the chat it draws price charts with indicators, open interest, gamma
  exposure, the volatility surface, combined premium and option payoff, and
  live quote cards.
- 127 technical indicators, computed inside OpenAlgo with the same results
  as OpenAlgo web's `openalgo.ta`, and a scan of up to 15 symbols on a
  condition such as RSI below 30.
- Attach up to four images or text files to a question; images need a model
  that reads them.
- Web search, switched on per question: DuckDuckGo with no key, or Tavily or
  Perplexity with yours. Only words from your own question are sent to the
  search, and a daily limit applies.
- OpenScript: ask for a study or strategy and the Agent writes it, then
  saves it to your `/trading` scripts once you approve, never over a
  compiled script.
- Voice: with an OpenAI key used only for speech, talk to the Agent from
  the microphone button. Your chosen model still writes every answer, with
  the same tools and limits.

### Orders from the Agent

- Trading is off until you turn on "Allow the agent to place, modify and
  cancel orders" in Agent Config. Then every order, modify, cancel and close
  stops on an approval card that states it in plain words ("Buy 10 SBIN on
  NSE, intraday (MIS), at market."), and nothing is sent until you choose
  Approve. An approval left for 10 minutes expires and nothing is placed.
- Trading safety, in Agent Config: a kill switch that stops every Agent
  order, also engaged while its file exists so you can stop the Agent
  without opening OpenAlgo; limits on orders per conversation, quantity,
  order value, distance from the last price and share of your funds;
  allowed exchanges, products and symbols; a duplicate-order window;
  analyzer mode only; and whether cancel-all and close-all are allowed. The
  limits are checked again after you approve and hold across a restart.
- Orders follow OpenAlgo's mode: in analyzer mode they go to the sandbox,
  and in Semi-Auto an approved order waits in Action Center for you to
  place it.
- A smart order sends exactly the order you approved. If your position
  changed in the meantime, nothing is sent and the Agent tells you.
- Every order the Agent sends carries the strategy name OpenAlgo Agent.
  Each order call, your decision and its result are kept in an audit record
  that stays when you delete the conversation.
- With voice, an order asked for by voice can be approved by voice for 30
  seconds; an order asked for on screen is approved on screen only.

### The Assistant panel in `/trading`

- Assistant, last on the right rail, opens a chat about the chart in front
  of you: it reads the symbol, interval, visible bars, indicators and your
  drawings.
- It marks levels, trend lines and zones, finds patterns, and adds or
  removes indicators. It never changes the chart's symbol, and clearing what
  it drew never removes your own drawings.
- Orders from the panel use the same switch, limits and approval card as
  `/agent`.

### Compatibility with OpenAlgo web

- Sector Rotation, in Tools: every NSE sector index, or your own list of
  stocks and indices, on a four-quadrant map against a benchmark, daily or
  weekly, with trails; also through the API, as on OpenAlgo web. A port of
  marketcalls/openalgo b7cdc028e, 3e37288e0 and 91e35d9fb.
- When the connection to your broker drops after an order was sent,
  OpenAlgo no longer reports the order as failed: it says the order may
  have been placed and to check the order book before placing it again.
  Zerodha orders are looked up in the order book by their tag
  (marketcalls/openalgo#2305).
- The scalping terminal, Option Greeks, P&L Tracker, Straddle Chart,
  Straddle PnL, the Health Monitor and the chart test page draw with
  openalgo-charts, as `/trading` does (marketcalls/openalgo#2297,
  marketcalls/openalgo#2298, marketcalls/openalgo#2299,
  marketcalls/openalgo#2300, marketcalls/openalgo#2301,
  marketcalls/openalgo#2302, marketcalls/openalgo#2303), and the older
  chart library is gone (marketcalls/openalgo#2304).
- Every chart moves to openalgo-charts 2.6.9: Renko, range bar, line break
  and point and figure bricks carry the volume traded while they formed,
  higher-timeframe studies warm up across weekends and holidays, and OHLC
  bars widen as you zoom in (marketcalls/openalgo 583852ba4).

### Brokers

- New broker: Sahi (marketcalls/openalgo#2320), contributed by Kalaiviswa
  (#213). Save the API key and secret on Broker Configuration, then click
  Login with Sahi; approve the key in Sahi's API Dashboard once each
  trading day first. On NSE, BSE, NFO and BFO: orders, smart orders, close
  all and cancel all, the order, trade, position and holding books, funds,
  margin, quotes, depth, history, and the live feed with LTP, Quote and
  5-level Depth.
- Sahi accepts orders only from a static IP registered on your Sahi
  account, so register the internet address this computer's traffic leaves
  from. MCX and NCOM contracts are not loaded: Sahi's API does not trade
  them.
- Dhan: an index's intraday chart loads after the close on every interval.
  Dhan adds a flat bar after the close to index history, which stopped the
  30-minute chart; it is now dropped (marketcalls/openalgo#2308).
- Dhan: index rows are typed EQ with no expiry, and every NSE index and
  Dhan's 17 BSE indices take their standard names
  (marketcalls/openalgo#2306).
- Angel: modifying an SL or SL-M order moves its trigger price. Before, the
  modify answered success and the trigger at Angel stayed where it was
  (marketcalls/openalgo#2279).
- Groww: the live feed no longer drops and reconnects without end after it
  connects (marketcalls/openalgo#2312).
- RMoney and the other Symphony XTS brokers: a smart order finds an open
  position that the broker lists without an instrument id, instead of
  reading it as flat (marketcalls/openalgo#2292).
- XTS brokers and Samco: their log lines never carry your broker
  credentials, and every build now checks it (marketcalls/openalgo#2313,
  marketcalls/openalgo b06c14d29).

### Fixes

- Links to web pages open in your browser. Since 1.0.0, the release page
  in the update notice and every other link to a web page did nothing in
  the OpenAlgo window.
- Log report pages and the Telegram and WhatsApp statistics no longer hold
  up live price handling when several are open at once.

### Maintenance

- Rust 1.99.1 and Tauri 2.12.4. Several tests that failed the release
  checks at random now wait for what they check.

### Known limitations

- Flow (`/flow`) is not in 1.0.4; it is planned for 1.0.5. Use OpenAlgo web
  for it until then.
- The Agent writes OpenScript, not Python strategies as OpenAlgo web's
  Agent does.
- Models on Amazon Bedrock, SageMaker and Google Vertex AI are not offered.
- Web search with no key goes through DuckDuckGo, which turns requests
  away at busy times; the Agent then says it could not search. For search
  you can rely on, add a Tavily or Perplexity key in Agent Config.
- No external data server is offered to the Agent yet, as on OpenAlgo web,
  so Agent Config has no External data section.
- Voice needs a microphone connection in the OpenAlgo window. On a Linux
  system whose web view has none, the microphone button says so; use voice
  from Chrome or Firefox at OpenAlgo's address instead.
- The MCP server for Claude Desktop and Claude Code still computes 27
  indicators; it moves to the Agent's 127 in a later update.

## 1.0.3

A built-in ngrok tunnel for TradingView, Chartink and GoCharting alerts,
steadier Kotak and INDmoney market data, API answers brought in line with
OpenAlgo web, and screens that say what the desktop really does. If you run
1.0.2, OpenAlgo offers this update in the app; choose Update when no live
strategy is running.

### Built-in ngrok tunnel

- Paste your ngrok authtoken and your free ngrok domain in Profile, Broker
  tab, Server Configuration, turn on Enable Ngrok Tunnel, and TradingView,
  Chartink and GoCharting alerts reach OpenAlgo Desktop with nothing else to
  install.
- The tunnel always opens on your one ngrok domain, never on a random
  address, so the webhook addresses you paste into TradingView, Chartink and
  GoCharting never change. Host Server URL becomes `https://<your domain>`.
- It opens when you sign in, closes when you sign out or quit, reconnects by
  itself, and its status says what is wrong: the authtoken, the domain,
  another ngrok agent already using the domain, or no internet.
- Only the API, the strategy and Chartink webhooks and MCP are reachable
  through it. The OpenAlgo pages and their login stay on your computer, and
  a broker's sign-in redirect that arrives through the tunnel is passed back
  to OpenAlgo on your computer.
- With the tunnel on, the broker sign-in address is
  `https://<your domain>/<broker>/callback`, as on OpenAlgo web: register it
  with your broker, or keep a fixed local address on Broker Configuration.
- If Enable Ngrok Tunnel was already on, the status asks for your authtoken
  and domain; nothing changes until you add them.
- Strategy webhook URLs show your Host Server URL instead of 127.0.0.1.

### Market data

- Kotak: live prices no longer pause and reload when the market is quiet.
  The feed keeps its connection alive like OpenAlgo web and replaces a dead
  one within 40 seconds.
- Kotak: the option chain, order window and market depth move with every
  price update, not only when the order book changes.
- INDmoney: switching a symbol between quote and depth no longer interrupts
  its prices.
- Dhan, Samco, IIFL Capital and Nubra: quiet feeds stay connected instead of
  reconnecting every minute and a half.
- Health page: a new Broker Feed Connections card shows each broker feed's
  state, how often it reconnected and why.

### Compatibility with OpenAlgo web

- API rate limits count each endpoint on its own, as on OpenAlgo web: ten
  orders and ten cancels in the same second are no longer refused together.
  Option Greeks and WhatsApp alerts are limited to 30 a minute, as on the
  web.
- When your broker's rate limit would make a request wait more than 10
  seconds, OpenAlgo answers "too many requests" with a sentence telling you
  to try again in a few seconds, and nothing is sent to the broker.
- History from Historify works with just your API key, before you log in to
  your broker.
- In Semi-Auto, API tools can no longer switch analyzer mode; switch it in
  the app.
- The instruments CSV shows a crypto contract's exact lot size (0.0001), as
  the JSON answer does.
- Strategy API: the events filter accepts every event kind OpenAlgo writes,
  and a strategy whose run cannot be read reports an error instead of
  looking stopped.

### Screens

- Up and Down arrow keys move through the chart watchlist in the trading
  terminal and load each chart (#198).
- Profile, Broker, Server Status shows what is really running (the app, the
  market data server, the broker feeds, order checks and health checks) and
  what to do when a part is down.
- A Semi-Auto order placed from the chart or an order ticket says it is
  waiting for your approval in Action Center, instead of showing an error.
- The FAQ lists all 37 brokers, Rupeezy included.
- The Remote MCP, two-factor and TOTP screens no longer mention restarts,
  backup codes or password resets the desktop does not have.

### Health, monitoring and security

- Health Monitor: a strategy exit or stop that waits more than 15 seconds to
  be placed raises an alert, which clears itself once the exit is placed.
- Traffic Monitor: the Error column of the CSV export says why a request was
  refused, for example "Invalid openalgo apikey" or the missing field.
- Security: the Repeat Limit help says it applies to the bans you enter on
  the Security page; automatic bans always expire.

### Chartink, Telegram and P&L Tracker

- Chartink alerts read BUY, SELL, SHORT or COVER from the alert name, as the
  Chartink pages say, and still from the scan name.
- Chartink bulk import accepts the example header line and tells you how many
  stocks were added.
- Telegram's Rate Limit setting now works: alerts above it wait their turn
  and arrive in order.
- P&L Tracker shows today's MTM as a percentage of your sandbox capital in
  analyzer mode, or of your account margin when live, instead of a fixed
  1,00,000.

### Brokers

- Fyers loads NSE debt, warrant and InvIT series into the symbol master, and
  Angel drops non-tradable bond and index rows, types its index rows EQ and
  names the SENSEX 50 index SENSEX50, as Zerodha does. A port of
  marketcalls/openalgo#2280, contributed by Kalaiviswa (#207).

### Reliability

- OpenAlgo Desktop no longer fails to start when the computer is very busy
  at launch; it waits for its databases instead.
- Strategy pages and the strategy API no longer hold up live price handling
  while a long history loads.
- Strategy fills recovered from the broker's order book no longer pause
  price handling when the database is busy, and they reach each strategy in
  the order the broker reported them.
- On Linux, OpenAlgo returns memory it no longer uses to the system every
  minute, so its memory grows about half as fast through a trading day.

### Maintenance

- Dependency updates (React Router 8.5, Vite 8.3.5, Biome 2.6.2, Tauri CLI
  2.12.4, tokio, rmcp and others). Dependabot now waits 7 days after a
  release before proposing it, as OpenAlgo web does (marketcalls/openalgo
  24d81412b), and holds DuckDB at 1.x until Historify moves to DuckDB 2.0 as
  a planned upgrade.
- The release checks no longer fail at random: four tests that raced their
  own timers now wait for what they check, and a slow Ubuntu mirror fails
  its step with the cause instead of stalling the run.

### Known limitations

- Kotak positions do not yet carry OpenAlgo web's carry-forward valuation
  marker in the position book.
- AliceBlue and Angel market data keep their 1.0.2 keepalive until it is
  checked against live accounts.

## 1.0.2

Safety fixes from an independent production audit of 1.0.1: strategy stops
that see every price, orders that cannot split or switch between Live and
Sandbox on the way, scalping stops and order tracking that keep working
while OpenAlgo's database is busy, and a Health page that never shows a list
it could not read as empty. If you run 1.0.1, OpenAlgo offers this update in
the app; choose Update when no live strategy is running.

### Strategy stops and targets

- Every streamed price is checked against a strategy's stops, targets and
  trailing stops as it arrives. Under heavy market data 1.0.1 combined
  waiting prices, so a trailing stop could miss the price that should have
  exited it.
- An exit closes only the position it was decided for: a leg that re-entered
  meanwhile (a webhook flip) keeps its new position.
- Leg exits decided while a strategy stop is waiting are placed after it when
  the stop could not place its own orders, instead of being dropped.
- Reading prices never waits on OpenAlgo's database: a slow read for one
  strategy no longer delays the prices of the others.
- Health shows "Strategies with exits waiting" and "Longest exit wait", and
  warns when a decided exit has waited more than 15 seconds to be placed.

### Orders

- A split order is counted before any part of it is built; one that would
  need more than 100 orders is refused at once, as on OpenAlgo web.
- An options order, single or multi-leg, goes wholly to Live or wholly to
  Sandbox, decided once when it starts; switching analyzer mode while it is
  being sent no longer splits it between the two.
- A multi-leg options order whose leg got no definite answer from the broker
  says so in its reply, and an Action Center approval in that state stays
  unconfirmed, across a restart too. The remaining legs are still sent, as
  on OpenAlgo web.
- A GTT quantity that is zero, negative, blank, fractional or larger than
  any exchange accepts is refused on every exchange, from the GTT page's
  modify as well.

### Scalping, order tracking and OpenScript

- Scalping stops keep being checked while OpenAlgo's database is busy, and
  saving one stop's exit no longer holds up another stop.
- When the scalping stops cannot be read, OpenAlgo tries again and tells you
  after three failures, instead of silently not watching a stop you just
  saved.
- A scalping exit that would need more than 100 orders is refused with an
  alert and the stop stays managed, instead of sending hundreds of orders
  from a wrong position size.
- Order tracking keeps an order unconfirmed when the records it needs cannot
  be read, instead of treating it as settled.
- An OpenScript live run trades with the inputs it started with. A start
  whose saved parameters cannot be read is refused with the reason, instead
  of running the script's default quantity and risk.

### Health page

- A card that could not be refreshed keeps what it last showed, marked with
  when it was read; a list that was never read says so instead of looking
  empty. Unconfirmed orders no longer disappear after one failed refresh.
- Each figure updates as soon as it is read; a read that stalls is given up
  after 8 seconds and marked.
- A WARN or FAIL stays visible after a failed refresh, and times from an
  earlier day show their date.
- Market data skipped by a slow reader counts since OpenAlgo started, so a
  reconnect no longer hides it, and market depth is included. Database
  growth is shown over the last day.

### Security

- An API key is never read back from the window's browser storage,
  whichever version of OpenAlgo wrote it there.

### Releases

- Updates reach you sooner: a release now ships the installers CI built and
  tested for that version, signing only their update files, in about 15
  minutes instead of 50. A published release can no longer be changed by a
  later run.
- Building OpenAlgo Desktop from source needs Node.js 22.23.4, 24.22.1 or
  26.12.1 or later, following OpenAlgo web (marketcalls/openalgo f3e7de829).
  The app itself ships no Node.js.

## 1.0.1

The first update after 1.0.0. It fixes the limitations listed in 1.0.0,
makes busy pages stop slowing the app's handling of market ticks, and shows
more of the app's own health. If you run 1.0.0, OpenAlgo offers this update
in the app; choose Update when no live strategy is running.

### Windows

- OpenAlgo starts on a Windows PC that has never had Microsoft's Visual C++
  runtime installed. 1.0.0 stopped there with "MSVCP140.dll was not found";
  the app now carries that runtime inside it. A 1.0.0 that does not start
  cannot offer the update: download the 1.0.1 setup program and run it over
  1.0.0, which keeps your data.

### Order safety

- Delta Exchange: a smart order with a fractional target position is decided
  in exact quantities (holding 2 with a target of 0.5 sells exactly 1.5),
  never rounded down. Everywhere else, and in sandbox mode, a fractional
  target is refused instead of rounded.
- A GTT order with a fractional quantity is refused, when placing and when
  modifying, live and in sandbox mode.
- An options split order holds back its remaining chunks after one whose
  outcome is unknown, as a split order already did; check the order book
  before sending them again.
- Options orders, options multi-order legs and GTT orders refuse a quantity
  above 2,147,483,647, as placing an order already did.

### Security

- Your OpenAlgo API key is no longer saved in the app window's browser
  storage on disk. It is kept in memory while you are signed in, and a copy
  left there by 1.0.0 is erased the first time 1.0.1 starts.
- After you generate a new API key, the books and order forms use it at once;
  before, they kept sending the old key until the window was reloaded.
- WhatsApp alerts use ctutils 0.4.5, the release that fixes advisory
  RUSTSEC-2026-0385 (encryption code whose timing could reveal secrets).

### Speed under load

- The log, traffic, health, security, diagnostics, MCP audit and analyzer
  pages read, shape and send their answers off the threads that carry the
  market data feed. With a large log open in several browsers, strategy
  decisions were lost before; now none are, with decisions in about 5 ms.
- Strategy order books and history, OpenScript deployment books, the Action
  Center, the analyzer status and the Telegram and WhatsApp statistics do the
  same, and pages that refresh the same data share one read.
- The first API call after a start, a sign-in or a new key checks the key
  off the threads that carry the market data feed, so a burst of API calls
  no longer delays the handling of market ticks.

### Health

- The Health page shows each database's size and growth, what each reader of
  a broker feed had to skip, records that could not be saved, strategy price
  counts and order tracking, and turns amber or red with a plain sentence
  when anything was skipped or lost.
- Alerts that nothing clears on its own have a Resolve button. An open alert
  shows what is wrong now, not what was wrong when it opened.
- Health and Run diagnostics count market data dropped for apps on the 8765
  feed, and the size of every database including sandbox and Historify.
- The analyzer status and the WhatsApp statistics answer "could not be read"
  when their log cannot be read, instead of showing zero.

### Accessibility and wording

- Every signed-in page passes an accessibility scan with no exceptions, in
  light and dark: clearer contrast for margin values, statuses, password
  rules, toast messages and the Tools tiles, and labels for screen readers.
- The strategies panel says what is true in the desktop: a live run is a
  hidden app window, and quitting or signing out pauses every run and leaves
  its positions. The scalping badge reads One-Click on or off.

## 1.0.0

The first release of OpenAlgo Desktop: a single-user desktop version of
OpenAlgo web for Windows, macOS and Linux, with no server to set up, no Python
runtime and no `.env` file. Everything is set inside the app.

Installers: Windows x64 (NSIS, per user), macOS Apple Silicon (M1 to M4) and
Intel (`.dmg`), Linux x64 and ARM64 (AppImage and `.deb`). Raspberry Pi is not
a supported device for the desktop app; use OpenAlgo web on a Pi.

### Compatibility with OpenAlgo web

- `/api/v1` on `http://127.0.0.1:5000` answers like the web, field for field:
  every endpoint the web documents, pinned by contract tests against responses
  recorded from a live web instance. The Python SDK, TradingView, Amibroker,
  Chartink, GoCharting, Excel and MCP clients work unchanged.
- The market data WebSocket on `ws://127.0.0.1:8765` speaks the web protocol:
  LTP, Quote and Depth modes, depth 5, 20, 30 and 50, and the order update
  stream.
- Broker redirect URLs follow the web convention,
  `http://127.0.0.1:5000/<broker>/callback`.
- The OpenAlgo web screens carried over nearly verbatim, including the
  `/trading` terminal on openalgo-charts 2.6.7 with custom intervals and daily
  study history, and Strike Profile (GEX and DEX by strike and expiry, exactly
  as on the web).

### Brokers

- All 37 OpenAlgo web brokers: Zerodha, Fyers, Upstox, Dhan and Dhan
  (Sandbox), Kotak Securities, Groww, Angel One; the Symphony XTS family
  (5 Paisa (XTS), JainamXts, CompositEdge, RMoney, Ibulls, IIFL, Wisdom
  Capital); the Noren family (Shoonya, Flattrade, TradeSmart, Zebu) and
  Firstock; Arrow, Paytm Money, Pocketful, HDFC Sky, HDFC Securities, Rupeezy;
  5 Paisa, Tradejini, Nubra, IndMoney, IIFL Capital; Alice Blue, Definedge,
  mStock, Motilal Oswal, Samco; and Delta Exchange for crypto.
- Books, trades, positions and holdings return OpenAlgo symbols for every
  broker. A broker reply that cannot be read is an error, never an empty book
  or a flat position, so close-all never reports "all squared off" while a
  segment is unread.
- Broker keys are entered in the app; several brokers can be saved and one is
  connected at a time. Sessions resume after sign-in until the daily 03:00 IST
  cut-off.
- Orders are paced at each broker's published rate; Samco, Tradejini, HDFC Sky,
  HDFC Securities, Pocketful and 5 Paisa, which publish none, at 10 a second.

### Order safety

- A placement has three outcomes: accepted, refused, or unknown. An order whose
  outcome is unknown is never sent again; it is settled from the broker's order
  book, and new entries wait until the gap in order facts is repaired.
- Strategies, OpenScript runs, scalping stops and Action Center orders are tied
  to the broker account they were opened on. An exit is never sent through
  another account, and a run reports what it holds elsewhere.
- A strategy page that loses the app's answer keeps the order pending and asks
  again with the same identity, so a lost answer never becomes a second order.
- Semi-Auto orders approved in the Action Center are followed by their run; an
  unreadable Semi-Auto setting refuses the order instead of sending it.
- Strategy ticks never wait on the database, and a deciding price is never
  lost under load. Stop-loss saves wait for a busy database instead of failing.

### Trading

- Sandbox mode (analyzer mode) mirroring the web's sandbox: 1 crore starting
  capital, margin blocking, fills from live ticks, weighted-average netting,
  T+1 settlement, MIS auto square-off, GTT and catch-up after the app was
  closed, kept in its own database.
- Strategy module and risk management with a pure risk core that passes the
  web's risk vectors; strategy webhooks for TradingView and GoCharting.
- Chartink strategies and webhooks; Action Center for Semi-Auto orders.
- The `/trading` charting terminal with custom indicators, OpenScript files and
  live OpenScript runs on the same engine as backtests.
- Scalping terminal with its risk monitor.
- Options tools: option chain, Greeks, OI tracker and range, max pain, IV
  chart, IV smile, volatility surface, GEX, gamma density, Strike Profile,
  straddle charts, strategy builder and arbitrage.
- Historify on DuckDB with download jobs, scheduler and exports.
- Playground, API key management, P&L tracker, leverage settings.

### Alerts and AI clients

- Native Telegram bot with commands, alerts and analytics.
- Native WhatsApp bot with QR and pair-code pairing and alerts.
- Native MCP server with the web's 49 tools, over stdio for Claude Desktop and
  Claude Code and over HTTP at `/mcp`; scoped tokens created on the API Key
  page, per-token limits and an audit log.

### Monitoring and settings

- Live, sandbox, security, traffic and latency logs; Health Monitor; Run
  diagnostics, including how fresh each broker feed is.
- Admin pages for freeze quantities, holidays, market timings, diagnostics
  and Remote MCP.
- Server Settings for ports, bind address and access from other devices. A
  port already in use is reported in the app with the fix, including the
  AirPlay Receiver case on macOS.

### Security

- No OS keychain on any platform. The data key is derived from your OpenAlgo
  password and unlocked when you sign in; after a restart the API, webhooks
  and MCP stay locked until you sign in.
- Broker credentials and tokens encrypted with AES-256-GCM, each bound to its
  row and column.
- Passwords and API keys hashed with Argon2id; API keys found through an HMAC
  index with a short cache.
- Signed-in user required on every non-public route; browser origins compared
  exactly; strict Content Security Policy; devtools off in release builds.
- Broker sign-in verified by single-use `state`, with account binding for
  brokers whose redirect does not return it.
- Sign-in and API key failures throttled per address; `/api/v1` limited as on
  the web.
- Logs keep API keys, tokens, OAuth codes, passwords and TOTP secrets out.

### Updates and your data

- In-app updates: OpenAlgo checks for a newer version at start and from
  Check for Updates in the profile menu, shows the release notes, and
  downloads nothing until you choose Update. The update's signature is
  checked before it is installed, and a restart that would interrupt Live
  strategies, OpenScript runs, scalping protection or open orders is
  confirmed first. macOS, Windows and AppImage copies update themselves; a
  `.deb` install is pointed to the release page.
- Your data is backed up before a new version changes it, put back if the
  new version cannot open it, and the copy is deleted as soon as the new
  version has opened your data. An older version refuses data that a newer
  one has used, and changes nothing.
- Every release is built from a commit whose full test run passed on every
  platform, including the installed app on each system and a sandbox order
  placed through the app's own window on Linux and Windows.

### Documentation

- A documentation set organised like OpenAlgo web's, in `docs/`: the user
  guide (installation to the charting terminal), the full `/api/v1` and
  WebSocket reference, the MCP tool reference, and guides for contributors.
  Start at `docs/INDEX.md`.

### Known limitations

- The installers are not yet code-signed. Windows SmartScreen and macOS
  Gatekeeper ask you to confirm the first open; the README shows how.
- Not included: the Python Strategy Host, Flow, the Agent (including the
  Assistant panel in `/trading`), and the pandas backtesters (Portfolio
  Backtester, SIP Backtester, Portfolio Analyzer). Use OpenAlgo web for them.
- Fixes coming in the next update:
  - On Delta Exchange, a smart order with a fractional target position and a
    GTT with a fractional quantity are rounded down instead of refused. Use
    whole quantities until the update.
  - An options split order keeps sending the remaining chunks after one whose
    outcome is unknown; check the order book before resending.
  - With a very long order and log history, some log and book pages can slow
    the app's handling of market ticks while they load.
  - The Health page does not yet show market-data skips, database growth or
    records not saved; Run diagnostics does.
