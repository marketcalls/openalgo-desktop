# Service layer (desktop)

OpenAlgo web documents its internal Python services (`services/*.py`) for
developers who add features that import them. OpenAlgo Desktop has no Python.
Its equivalent is the Rust service layer in `src-tauri/src/services/` and a
few sibling modules. This page says where each part lives and what contract it
keeps. It is for people working on the desktop source; a trading script uses
the `/api/v1` API through the [Python SDK](openalgo%20python%20sdk.md) instead.

## The contract

The services return the same response bodies as the web's services, field for
field, because `/api/v1` is the contract: the same request gets the same status
code and body on both. This is pinned by the golden fixtures recorded from a
live web instance (`tests/fixtures/web/`) and the contract tests
(`src-tauri/tests/it/api_v1_*`). For the request and response of each
endpoint, read the API reference under [docs/api](../api/), not the service
code.

## How a call flows

1. A handler in `src-tauri/src/server/api_v1/` reads the JSON body, validates
   it with the web's Marshmallow rules (`services/schema.rs`, with each
   endpoint's schema in `services/schemas.rs`), and checks the API key.
2. It calls one service function. Business logic lives only in the service,
   never in the handler.
3. The service returns a `Reply` (`services/core.rs`): the HTTP status and the
   JSON body, sent unchanged. It is the web's
   `(success, response_data, status_code)` tuple in one value.
4. The service decides the destination once: the sandbox engine
   (`src-tauri/src/sandbox/`) in analyzer mode, otherwise the connected broker.
   In Semi-Auto mode a new order is queued in the Action Center instead
   (`services/order_router.rs`).
5. It publishes the web's event on the in-process bus
   (`src-tauri/src/events/`). Logging, Socket.IO pushes and Telegram or
   WhatsApp alerts are subscribers, not code in the order path.

In-process callers do not make HTTP calls back into the same app. The strategy
module calls the order service directly with an internal route. The MCP tools
hand their request to the `/api/v1` handler stack in process, without a
socket, so they are validated and rate-limited like an SDK client
(`src-tauri/src/mcp/dispatch.rs`).

## Where the web's services live in the desktop

| Web | Desktop |
|-----|---------|
| Place, smart, modify, cancel, cancel all, close position | `services/order_service.rs` |
| Basket and split orders | `services/batch_order_service.rs` |
| Options orders and multi-leg options orders | `services/options_order_service.rs` |
| Option symbol, chain, synthetic future, Greeks | `services/options_service/` |
| Order book, trade book, positions, holdings, funds, order status, open position | `services/account_service.rs` |
| Quotes, multi-quotes, depth, history, intervals, ticker, margin | `services/market_data_service.rs` |
| Symbol, search, expiry, instruments, freeze quantities | `services/symbol_service.rs`, `services/freeze_qty_service.rs` |
| GTT | `services/gtt_service.rs` |
| Analyzer status and toggle | `services/analyzer_service.rs` |
| Order router and Action Center (Semi-Auto) | `services/order_router.rs`, `services/action_center_service.rs` |
| Market holidays and timings | `services/market_calendar_service.rs` |
| Telegram and WhatsApp notify | `services/notify_service.rs`, with the bots in `src-tauri/src/messaging/` |
| Sandbox (`sandbox/` package) | `src-tauri/src/sandbox/` |
| Historify | `src-tauri/src/historify/` |
| Risk core (`services/risk/`) | `src-tauri/src/risk/` (pure, no I/O) |
| Strategy module (`services/strategy_module/`) | `src-tauri/src/strategy/`; see [Strategy module and RMS](strategy_rms_documentation.md) |
| MCP server | `src-tauri/src/mcp/`; see [MCP tool reference](../mcp-tool-reference.md) |

The module list in `src-tauri/src/services/mod.rs` is the current inventory.

## Differences a developer should know

- **Uncertain placements.** A `Reply` also carries what a live placement did at
  the broker (`Reply.placement`), never serialised. The wire keeps the web's
  shape, while in-process callers use it to tell a placement with no definite
  answer from a refusal.
- **A Semi-Auto setting that cannot be read refuses the order** instead of
  sending it, as the web does (`services/order_service.rs`, `semi_auto`).
- **Live obligations are bound to the broker account they were opened on**
  (`services/live_owner.rs`): an exit is refused rather than sent through a
  different broker or account.
- **Locked keys.** After the app starts and after logout, nothing that needs
  the API key runs until the user signs in (`/api/v1` answers 503).

## Not in the desktop

The web services behind the Python Strategy Host and the Portfolio and SIP
backtesters have no desktop equivalent. Those features are not in the
desktop; use OpenAlgo web for them.

Flow and the Agent are native Rust in the desktop rather than ports of the
web's services: Flow (`services/flow_*.py` on the web) is
`src-tauri/src/flow/` from 1.0.5, and the Agent is `src-tauri/src/agent/`
from 1.0.4. Both reach the services in this document the way an API client
does, through the `/api/v1` handlers run in process with the API key, so
analyzer mode, Semi-Auto and the answers documented here apply to their
orders and reads unchanged. Flow's workflow format is in
[flow-import-format.md](flow-import-format.md).
