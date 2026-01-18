# OpenAlgo Desktop - Implementation Plan

## Overview

Build a 1:1 clone of OpenAlgo as a desktop application using:
- **Frontend**: React 19 + Zustand + TanStack Query + shadcn/ui + Tailwind CSS v4
- **Backend**: Rust + Tauri 2.0
- **Database**: SQLite (rusqlite) + DuckDB
- **Security**: OS Keychain + AES-256-GCM + Argon2

## Key Requirements

1. **Exact 1:1 Clone** - Same theme (OKLch colors), same DB models, same frontend
2. **Zero Config** - Users install and configure everything via GUI
3. **Secure Credentials** - OS Keychain instead of .env files
4. **Single User, Single Broker** - One user, one broker connected at a time
5. **Initial Brokers** - Angel One, Zerodha, Fyers (others added later)

---

## Project Structure

```
openalgo-desktop/
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── icons/
│   └── src/
│       ├── main.rs                 # Entry point
│       ├── lib.rs                  # Library exports
│       ├── error.rs                # AppError enum
│       ├── state.rs                # AppState (db, session, cache)
│       ├── commands/               # Tauri IPC commands
│       │   ├── mod.rs
│       │   ├── auth.rs             # login, broker_login, logout
│       │   ├── orders.rs           # place_order, modify, cancel
│       │   ├── positions.rs        # get_positions, close_all
│       │   ├── holdings.rs         # get_holdings
│       │   ├── quotes.rs           # get_quote, get_depth
│       │   ├── strategy.rs         # CRUD for strategies
│       │   ├── settings.rs         # get/save settings
│       │   ├── sandbox.rs          # paper trading
│       │   └── historify.rs        # DuckDB queries
│       ├── db/
│       │   ├── sqlite/             # 15+ table modules
│       │   │   ├── auth.rs, user.rs, api_keys.rs
│       │   │   ├── symbol.rs, strategy.rs, settings.rs
│       │   │   ├── sandbox.rs, order_logs.rs, etc.
│       │   └── duckdb/             # Historify tables
│       │       ├── market_data.rs, watchlist.rs
│       ├── brokers/
│       │   ├── mod.rs              # Broker trait
│       │   ├── types.rs            # Order, Position, etc.
│       │   ├── angel/              # Angel One adapter
│       │   ├── zerodha/            # Zerodha adapter
│       │   └── fyers/              # Fyers adapter
│       ├── security/
│       │   ├── keychain.rs         # OS keychain (keyring crate)
│       │   ├── encryption.rs       # AES-256-GCM
│       │   └── hashing.rs          # Argon2
│       └── websocket/
│           ├── manager.rs          # Connection manager
│           └── handlers.rs         # Binary protocol handlers
├── src/                            # React frontend (copied from openalgo)
│   ├── api/client.ts               # Modified for Tauri invoke
│   ├── components/, pages/, stores/
│   └── index.css                   # Theme (unchanged)
├── package.json
├── vite.config.ts
├── MIGRATION_TRACKER.md
└── README.md
```

---

## Database Schema (Exact Replica)

### Main SQLite Tables (15 tables)

| Table | Purpose |
|-------|---------|
| `auth` | Encrypted broker tokens (AES-256-GCM) |
| `api_keys` | Argon2 hashed + encrypted API keys |
| `users` | Single user (Argon2 password) |
| `symtoken` | 100k+ symbol master contract |
| `strategies` | TradingView webhook strategies |
| `strategy_symbol_mappings` | Strategy-symbol links |
| `chartink_strategies` | Chartink webhooks |
| `chartink_symbol_mappings` | Chartink-symbol links |
| `settings` | App configuration |
| `chart_preferences` | Per-user chart settings |
| `qty_freeze` | F&O freeze limits |
| `pending_orders` | Semi-auto mode queue |
| `market_holidays` | Trading holidays |
| `market_holiday_exchanges` | Exchange-specific hours |
| `market_timings` | Market session times |

### Sandbox SQLite (6 tables)

`sandbox_orders`, `sandbox_trades`, `sandbox_positions`, `sandbox_holdings`, `sandbox_funds`, `sandbox_daily_pnl`

### DuckDB Historify (6 tables)

`market_data` (OHLCV), `watchlist`, `data_catalog`, `download_jobs`, `job_items`, `symbol_metadata`

---

## Broker Implementations

### Broker Trait Interface

```rust
#[async_trait]
pub trait Broker: Send + Sync {
    fn id(&self) -> &'static str;
    async fn authenticate(&self, creds: BrokerCredentials) -> Result<AuthResponse>;
    async fn place_order(&self, auth: &str, order: OrderRequest) -> Result<OrderResponse>;
    async fn modify_order(&self, auth: &str, id: &str, order: ModifyRequest) -> Result<OrderResponse>;
    async fn cancel_order(&self, auth: &str, id: &str) -> Result<()>;
    async fn get_order_book(&self, auth: &str) -> Result<Vec<Order>>;
    async fn get_positions(&self, auth: &str) -> Result<Vec<Position>>;
    async fn get_holdings(&self, auth: &str) -> Result<Vec<Holding>>;
    async fn get_quote(&self, auth: &str, symbols: Vec<(String, String)>) -> Result<Vec<Quote>>;
    async fn download_master_contract(&self, auth: &str) -> Result<Vec<SymbolData>>;
    fn create_websocket(&self, feed_token: &str) -> Box<dyn BrokerWebSocket>;
}
```

### Angel One

- **Auth**: POST `/rest/auth/angelbroking/user/v1/loginByPassword` (TOTP required)
- **Orders**: `/rest/secure/angelbroking/order/v1/{placeOrder,cancelOrder,modifyOrder}`
- **WebSocket**: `wss://smartapisocket.angelone.in/smart-stream` (binary, little-endian)

### Zerodha

- **Auth**: POST `/session/token` with SHA256 checksum (api_key + request_token + api_secret)
- **Orders**: POST/PUT/DELETE `/orders/regular`
- **WebSocket**: `wss://ws.kite.trade` (binary, big-endian)

### Fyers

- **Auth**: POST `/api/v3/validate-authcode` with SHA256 appIdHash
- **Orders**: POST/PATCH/DELETE `/api/v3/orders/sync`
- **WebSocket**: `wss://socket.fyers.in/hsm/v1-5/prod` (HSM binary protocol)

---

## Security Architecture

### Credential Storage (OS Keychain)

```rust
// Using keyring crate
const SERVICE: &str = "openalgo-desktop";

// Store: keychain.store_broker_credentials("angel", api_key, api_secret)
// Retrieve: keychain.get_broker_credentials("angel") -> (api_key, Option<api_secret>)
// Delete: keychain.delete_broker_credentials("angel")
```

### Token Encryption (AES-256-GCM)

- Master key stored in OS keychain
- Auth tokens encrypted before SQLite storage
- 12-byte random nonce per encryption

### Password Hashing (Argon2id)

- User password hashed with Argon2id + pepper
- Pepper stored in OS keychain
- API keys double-protected: Argon2 hash + AES encryption

---

## Frontend Adaptation

### API Client Changes (src/api/client.ts)

```typescript
// FROM (HTTP):
const response = await axios.post('/api/v1/orders', order);

// TO (Tauri IPC):
import { invoke } from '@tauri-apps/api/core';
const response = await invoke('place_order', { order });
```

### Files to Modify

1. `src/api/client.ts` - Replace axios with Tauri invoke
2. `src/api/trading.ts` - Use invoke for all trading commands
3. `src/api/auth.ts` - Use invoke for auth commands
4. `src/stores/authStore.ts` - Minor session handling changes

### Files Unchanged (Copy As-Is)

- `src/index.css` - Full theme with OKLch colors
- `src/components/ui/*` - All 25+ shadcn components
- `src/pages/*` - All 42 page components
- `src/stores/themeStore.ts` - Theme management
- `src/config/navigation.ts` - Route config

---

## Rust Dependencies

```toml
[dependencies]
tauri = { version = "2.0", features = ["shell-open"] }
rusqlite = { version = "0.31", features = ["bundled"] }
duckdb = { version = "0.10", features = ["bundled"] }
keyring = "2.3"
argon2 = "0.5"
aes-gcm = "0.10"
reqwest = { version = "0.12", features = ["json"] }
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = "0.21"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
chrono = { version = "0.4", features = ["serde"] }
thiserror = "1.0"
tracing = "0.1"
sha2 = "0.10"
base64 = "0.21"
rand = "0.8"
parking_lot = "0.12"
```

---

## Implementation Phases

### Phase 1: Project Setup (3-4 days)

- [ ] Create openalgo-desktop folder structure
- [ ] Initialize Tauri 2.0 project
- [ ] Set up Rust workspace with dependencies
- [ ] Copy React frontend from openalgo
- [ ] Verify frontend builds with Vite

### Phase 2: Security Layer (2-3 days)

- [ ] Implement keychain.rs (OS keychain integration)
- [ ] Implement encryption.rs (AES-256-GCM)
- [ ] Implement hashing.rs (Argon2id)
- [ ] Test on macOS, Windows, Linux

### Phase 3: Database Layer (4-5 days)

- [ ] Create SQLite migrations (all 15 main tables)
- [ ] Implement CRUD for each table module
- [ ] Create sandbox database tables
- [ ] Set up DuckDB for historify
- [ ] Implement symbol cache (100k+ O(1) lookup)

### Phase 4: Broker Adapters (7-10 days)

- [ ] Define Broker trait and types
- [ ] Implement Angel One adapter
  - [ ] Auth with TOTP
  - [ ] Orders, positions, holdings
  - [ ] Quotes and market data
  - [ ] WebSocket (binary protocol)
- [ ] Implement Zerodha adapter (same structure)
- [ ] Implement Fyers adapter (same structure)

### Phase 5: Tauri Commands (3-4 days)

- [ ] Auth commands (login, broker_login, logout)
- [ ] Order commands (place, modify, cancel, close_all)
- [ ] Data commands (positions, holdings, quotes)
- [ ] Strategy commands (CRUD)
- [ ] Settings commands
- [ ] Sandbox commands

### Phase 6: Frontend Integration (3-4 days)

- [ ] Modify api/client.ts for Tauri invoke
- [ ] Update all API modules
- [ ] Test all pages end-to-end
- [ ] Verify theme consistency

### Phase 7: WebSocket & Real-time (3-4 days)

- [ ] Implement WebSocket manager
- [ ] Handle binary protocols (Angel, Zerodha, Fyers)
- [ ] Emit events to frontend
- [ ] Test market data streaming

### Phase 8: Testing & Polish (3-4 days)

- [ ] End-to-end testing all flows
- [ ] Cross-platform testing
- [ ] Performance optimization
- [ ] Error handling review

### Phase 9: Build & Release (2-3 days)

- [ ] Configure app icons
- [ ] Set up code signing (macOS notarization)
- [ ] Build installers (.dmg, .msi, .deb/.AppImage)
- [ ] Create README and documentation

---

## Verification Steps

### 1. Setup Verification

```bash
cd openalgo-desktop
npm install
npm run tauri dev
# Should launch app with React frontend
```

### 2. Database Verification

- Create user via GUI
- Verify tables created in `~/.openalgo-desktop/openalgo.db`

### 3. Security Verification

- Save broker credentials via GUI
- Verify stored in OS Keychain (not filesystem)
- macOS: Keychain Access app
- Windows: Credential Manager
- Linux: Secret Service (GNOME Keyring)

### 4. Broker Login Verification

- Login with Angel One (requires TOTP)
- Verify positions/holdings load
- Place test order (paper/sandbox first)

### 5. Theme Verification

- Compare light/dark modes with original OpenAlgo
- Test analyzer mode (purple theme)
- Test sandbox mode (amber theme)

---

## Critical Files Reference

| Purpose | Source File |
|---------|-------------|
| Auth encryption patterns | `/Users/openalgo/openalgo-react/openalgo/database/auth_db.py` |
| Angel broker API | `/Users/openalgo/openalgo-react/openalgo/broker/angel/api/order_api.py` |
| Zerodha broker API | `/Users/openalgo/openalgo-react/openalgo/broker/zerodha/api/order_api.py` |
| Fyers broker API | `/Users/openalgo/openalgo-react/openalgo/broker/fyers/api/order_api.py` |
| Symbol cache design | `/Users/openalgo/openalgo-react/openalgo/database/token_db_enhanced.py` |
| Frontend API client | `/Users/openalgo/openalgo-react/openalgo/frontend/src/api/client.ts` |
| Theme CSS | `/Users/openalgo/openalgo-react/openalgo/frontend/src/index.css` |
| Sandbox schema | `/Users/openalgo/openalgo-react/openalgo/database/sandbox_db.py` |
| DuckDB historify | `/Users/openalgo/openalgo-react/openalgo/database/historify_db.py` |
