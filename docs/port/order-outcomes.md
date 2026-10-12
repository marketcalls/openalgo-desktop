# Order placement outcomes: per-broker status

Codex LOG-08. A placement has three outcomes: accepted (an order id),
refused (provably never sent, or the broker said no) and uncertain (sent,
or maybe sent, with no definite answer). The rules live in
`src-tauri/src/brokers/common/outcome.rs`; this page tracks what each
adapter marks.

## The rule: inside a placement the default is uncertain

Every adapter `place_order` runs inside `outcome::placing`. In there:

- **An error is a refusal only when the adapter marks it as one**
  (`AppError::Refused`, built by `outcome::refusal`, `refusal_with_status`
  or `definite`): a readable answer from the broker that says the order was
  not placed, or a refusal before anything was sent. Errors that cannot
  follow a send (validation, a missing session, a symbol not found, a
  refused session) are refusals too.
- **Everything else is uncertain**: an unmarked `AppError::Broker`, an
  unreadable answer, a socket or I/O failure, a transport error after the
  request may have been sent (`outcome::after_send`), a success without an
  order id. An adapter nobody reviewed therefore errs on the safe side: its
  refusals read as uncertain, never its uncertainties as refusals.
- **A server error taints the placement.** `http::read_json` (and adapters
  with their own transport, through `outcome::note_server_error`) record a
  5xx; a `refusal` after it in the same placement stays uncertain, so an
  error body on a 5xx never reads as "not placed".
- **A success status (2xx) is a refusal only with the broker's explicit
  failure marker.** `outcome::not_accepted` decides an answer without an
  order id: 5xx uncertain, 4xx refused, 2xx refused only when the adapter
  read a failure status or error field; `{}`, `null` or a body without the
  success marker are uncertain.
- **Steps before the send do not count against the order.**
  `outcome::before_send` wraps a quote read for market protection, a
  client-id lookup and similar: its failure is a refusal (nothing was sent)
  and a 5xx it met does not taint the order's own answer.
- Outside a placement (books, quotes, modify, cancel) a refusal is an
  ordinary `AppError::Broker`, as before.

The `/api/v1` wire does not change: `Refused` answers 400 with the broker's
text like `Broker` did, and an uncertain answer keeps its old status and
body (the broker's text with 400, an internal failure with 500). The
outcome travels beside the reply for in-process callers (strategies,
scalping, OpenScript, MCP).

An uncertain placement is never placed again (and is announced as
`order.unconfirmed`, never `order.failed`); the reconciler is woken at once.
It looks the order up in the broker's order book (`find_placed`, P1-4):

- by client tag only, when the broker reports every order's tag
  (`Broker::reports_order_tags`, today Zerodha and Angel: a capability,
  never guessed from the rows) and the placement carried one;
- otherwise by symbol, exchange, side, quantity, product and shape (price
  type, where a MARKET order may show as LIMIT and an SL-M as SL after
  market price protection; limit and trigger prices) among orders nobody
  owns and that carry no other order's tag, whose own timestamp is readable
  and no more than 60 seconds before the placement. A row whose time cannot
  be read is never taken. Several candidates are never guessed between.

Multi-order replies (close-all, basket, split) carry "some order has no
definite answer" beside the web's wire shape (P1-5). Every close-all override
runs each exit inside `outcome::placing` and records it with
`CloseAllResult::record`. A split stops at the first chunk whose answer was
lost and reports the rest as not sent (the web keeps sending). An Action
Center approval whose answer was lost stays `submitting` (unconfirmed): never
marked rejected, never sent again.

A strategy exit whose order row cannot be written is not sent (P2-6): it
could not be found again if its answer were lost.

## When the book does not show it (P0-3)

- **Sandbox**: absent from 4 reads over at least 20 seconds, it was not
  placed; the claim is released.
- **Live**: never released on absence alone. The claim is held, a critical
  alert is raised once (Health page, and a strategy event), and the order is
  listed under Unconfirmed Orders on the Health page
  (`GET /health/api/held-orders`). It is released when the trader marks it
  not placed there (`POST /health/api/held-orders/not-placed`, which calls
  `OrderOwner::confirm_not_placed` under the reconciler's pass lock, after
  reading the order book and the trade book once more: an order or a trade
  that matches it, or a book that cannot be read, keeps it held), or
  automatically only with corroboration: absent for 10 minutes, no working
  order of the same instrument and side that nobody owns, no matching trade
  in the trade book, and the net position exactly what the owner had before
  sending (`Unconfirmed::net_before`; an owner that does not know it never
  releases automatically). Owners decide through `AbsenceTracker::judge`.
- **A scalping exit the broker accepted** that the order book no longer
  shows is judged the same way, so it is never left working silently. Exit
  orders placed before the current session (the 03:00 IST boundary) are
  ended when the monitor starts.

## Per adapter

Marked: the readable refusals the adapter reports as `Refused`. Uncertain:
the answers it reports as uncertain on purpose. Lost-reply tests (503,
cut-off 200, 200 without an id, plus one readable refusal) are in
`tests/it/broker_order_outcomes.rs` where named.

| Adapter | Marked refusals | Uncertain on purpose | Client tag |
| --- | --- | --- | --- |
| zerodha | Kite error envelope on a non-5xx | success with no `data`; tagged answers | per order, read back |
| angel | `status: false` on a non-5xx | ambiguous replies | per order, read back |
| aliceblue | the two refusal answers | success without an id | constant |
| arrow | error envelope on a non-5xx | success without `orderNo` | none |
| definedge | non-success body on a non-5xx | success without an id, unreadable answer | none |
| deltaexchange | `success: false` on a non-5xx, 403 IP block, spent quota | success without an id, undecodable `result` | none |
| dhan | error body on a non-5xx | 2xx without `orderId` | none |
| firstock | failure body on a non-5xx, plain-text rate limit | success without `orderNumber` (tested) | constant |
| fivepaisa | `Status` non-zero, error answers on a non-5xx | success with no or zero `BrokerOrderID` (tested) | constant |
| fyers | error envelope on a non-5xx | success without an id | constant |
| groww | readable refusal on a 2xx or 4xx (not 429) | 5xx, unreadable 2xx, success without an id | per order, not read back |
| hdfcsecurities | error envelope on a non-5xx, connection never opened | transport failure after sending, success without an id | none |
| hdfcsky | refusal on a non-5xx; LTP read is `before_send` | success without `oms_order_id` | per order, not read back |
| iiflcapital | refusal on a non-5xx | throttle on a write, unreadable answer, success without an id | none |
| indmoney | readable refusal on a non-5xx; quote read is `before_send` | "no order number", unreadable answer, success without an id | none |
| kotak | `stat` not Ok on a non-5xx | success without `nOrdNo` | per order `ig`, no tagged book |
| motilal | FAILURE envelope on a non-5xx; quote read is `before_send` | success without `uniqueorderid` | none |
| mstock | `status: false` on a non-5xx | success without an id, empty answer (tested) | empty |
| nubra | non-2xx on a non-5xx, repeated 429 | 2xx without `intentOrderId` (tested) | constant |
| paytm | error envelope on a non-5xx | 5xx, unreadable 2xx, success without a number (tested) | none |
| pocketful | error envelope on a non-5xx; client id and LTP reads are `before_send` | success without an id | none |
| samco | Failure on a non-5xx; MPP quote is `before_send` | empty or unreadable answer, Success without `orderNumber` (tested) | none |
| tradejini | `s` not ok on a non-5xx | `s: ok` without an id (tested) | none |
| upstox | error envelope on a non-5xx, 429 | non-JSON on a 2xx, 5xx | constant |
| noren family | `stat` not Ok on a non-5xx; MPP quote is `before_send` | Ok without `norenordno` | constant |
| xts family | error envelope on a non-5xx | `type: success` without `AppOrderID` (tested) | constant |

Next: per-order tags read back for Upstox, Dhan (`correlationId`), Fyers,
Kotak, Groww and HDFC Sky.
