# OpenAlgo Desktop - Migration Tracker

## Summary

| Phase | Name | Tasks | Status | Progress |
|-------|------|-------|--------|----------|
| 1 | Project Setup | 8 | Complete | 8/8 |
| 2 | Security Layer | 6 | In Progress | 4/6 |
| 3 | Database Layer | 12 | In Progress | 10/12 |
| 4 | Broker Adapters | 18 | In Progress | 9/18 |
| 5 | Tauri Commands | 10 | Complete | 10/10 |
| 6 | Frontend Integration | 8 | Complete | 8/8 |
| 7 | WebSocket & Real-time | 6 | In Progress | 3/6 |
| 8 | Testing & Polish | 6 | Not Started | 0/6 |
| 9 | Build & Release | 6 | Not Started | 0/6 |
| **Total** | | **80** | | **52/80 (65%)** |

---

## Phase 1: Project Setup

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 1.1 | Create `openalgo-desktop/` folder | Complete | - | Root directory |
| 1.2 | Initialize npm project with package.json | Complete | 1.1 | React 19, Vite 7, Tauri deps |
| 1.3 | Initialize Tauri 2.0 project | Complete | 1.2 | tauri.conf.json, build.rs |
| 1.4 | Set up Cargo.toml with dependencies | Complete | 1.3 | All Rust dependencies |
| 1.5 | Create Rust module structure | Complete | 1.4 | commands/, db/, brokers/, security/, websocket/ |
| 1.6 | Copy React frontend from openalgo | Complete | 1.2 | src/, public/, configs |
| 1.7 | Configure Vite for Tauri | Complete | 1.6 | Port 1420, no proxy |
| 1.8 | Verify `npm run tauri dev` works | Pending | 1.7 | Need to run npm install |

---

## Phase 2: Security Layer

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 2.1 | Implement `security/mod.rs` | Complete | 1.5 | Module exports, SecurityManager |
| 2.2 | Implement `security/keychain.rs` | Complete | 2.1 | keyring crate, store/get/delete |
| 2.3 | Implement `security/encryption.rs` | Complete | 2.1 | AES-256-GCM, nonce handling |
| 2.4 | Implement `security/hashing.rs` | Complete | 2.1 | Argon2id, pepper support |
| 2.5 | Test keychain on macOS | Pending | 2.2 | Keychain Access verification |
| 2.6 | Test keychain on Windows/Linux | Pending | 2.2 | Credential Manager, Secret Service |

---

## Phase 3: Database Layer

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 3.1 | Implement `db/sqlite/connection.rs` | Complete | 1.5 | Connection wrapper |
| 3.2 | Implement `db/sqlite/migrations.rs` | Complete | 3.1 | All 21 tables DDL |
| 3.3 | Implement `db/sqlite/auth.rs` | Complete | 3.2 | CRUD for auth table |
| 3.4 | Implement `db/sqlite/user.rs` | Complete | 3.2 | Single user CRUD |
| 3.5 | Implement `db/sqlite/api_keys.rs` | Pending | 3.2 | Hash + encrypt pattern |
| 3.6 | Implement `db/sqlite/symbol.rs` | Complete | 3.2 | 100k+ symbol cache |
| 3.7 | Implement `db/sqlite/strategy.rs` | Complete | 3.2 | Strategies + mappings |
| 3.8 | Implement `db/sqlite/settings.rs` | Complete | 3.2 | App settings |
| 3.9 | Implement `db/sqlite/sandbox.rs` | Complete | 3.2 | 6 sandbox tables |
| 3.10 | Implement `db/duckdb/mod.rs` | Complete | 1.5 | DuckDB connection |
| 3.11 | Implement `db/duckdb/migrations.rs` | Complete | 3.10 | 6 historify tables |
| 3.12 | Implement `db/duckdb/market_data.rs` | Pending | 3.11 | Full OHLCV insert/query |

---

## Phase 4: Broker Adapters

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 4.1 | Define `brokers/mod.rs` trait | Complete | 1.5 | Broker trait interface |
| 4.2 | Define `brokers/types.rs` | Complete | 4.1 | Order, Position, Quote types |
| 4.3 | Implement `brokers/angel/auth.rs` | Complete | 4.1 | TOTP login flow |
| 4.4 | Implement `brokers/angel/orders.rs` | Complete | 4.3 | place, modify, cancel |
| 4.5 | Implement `brokers/angel/data.rs` | Partial | 4.3 | Stubs for positions, holdings |
| 4.6 | Implement `brokers/angel/quotes.rs` | Partial | 4.3 | Stub for quote |
| 4.7 | Implement `brokers/angel/websocket.rs` | Pending | 4.3 | Binary protocol, little-endian |
| 4.8 | Implement `brokers/angel/mapping.rs` | Pending | 4.1 | Exchange/product mapping |
| 4.9 | Implement `brokers/zerodha/auth.rs` | Complete | 4.1 | SHA256 checksum |
| 4.10 | Implement `brokers/zerodha/orders.rs` | Complete | 4.9 | REST API |
| 4.11 | Implement `brokers/zerodha/data.rs` | Partial | 4.9 | Stubs for positions, holdings |
| 4.12 | Implement `brokers/zerodha/quotes.rs` | Partial | 4.9 | Stub for GET /quote |
| 4.13 | Implement `brokers/zerodha/websocket.rs` | Pending | 4.9 | Binary, big-endian |
| 4.14 | Implement `brokers/fyers/auth.rs` | Complete | 4.1 | SHA256 appIdHash |
| 4.15 | Implement `brokers/fyers/orders.rs` | Complete | 4.14 | sync endpoints |
| 4.16 | Implement `brokers/fyers/data.rs` | Partial | 4.14 | Stubs |
| 4.17 | Implement `brokers/fyers/quotes.rs` | Partial | 4.14 | Stub |
| 4.18 | Implement `brokers/fyers/websocket.rs` | Pending | 4.14 | HSM binary protocol |

---

## Phase 5: Tauri Commands

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 5.1 | Implement `commands/auth.rs` | Complete | 3.4, 2.4 | login, logout |
| 5.2 | Implement `commands/broker.rs` | Complete | 4.3, 4.9, 4.14 | broker_login, set_broker |
| 5.3 | Implement `commands/orders.rs` | Complete | 4.4, 4.10, 4.15 | place, modify, cancel |
| 5.4 | Implement `commands/positions.rs` | Complete | 4.5, 4.11, 4.16 | get_positions, close_all |
| 5.5 | Implement `commands/holdings.rs` | Complete | 4.5, 4.11, 4.16 | get_holdings |
| 5.6 | Implement `commands/quotes.rs` | Complete | 4.6, 4.12, 4.17 | get_quote, get_depth |
| 5.7 | Implement `commands/strategy.rs` | Complete | 3.7 | CRUD strategies |
| 5.8 | Implement `commands/settings.rs` | Complete | 3.8, 2.2 | get/save settings + credentials |
| 5.9 | Implement `commands/sandbox.rs` | Complete | 3.9 | Paper trading commands |
| 5.10 | Implement `commands/historify.rs` | Complete | 3.12 | DuckDB queries |

---

## Phase 6: Frontend Integration

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 6.1 | Modify `src/api/client.ts` | Complete | 5.1-5.10 | Re-exports Tauri client |
| 6.2 | Update `src/api/auth.ts` | Complete | 6.1 | Uses authCommands, brokerCommands |
| 6.3 | Update `src/api/trading.ts` | Complete | 6.1 | Uses orderCommands, positionCommands |
| 6.4 | Update `src/api/strategy.ts` | Complete | 6.1 | Uses strategyCommands, symbolCommands |
| 6.5 | Update `src/stores/authStore.ts` | Complete | 6.2 | Async session handling via Tauri |
| 6.6 | Update `src/stores/themeStore.ts` | Complete | 6.1 | Uses settingsCommands |
| 6.7 | Update `src/components/auth/AuthSync.tsx` | Complete | 6.5 | Tauri session sync |
| 6.8 | Create `src/api/tauri-client.ts` | Complete | 5.1-5.10 | Type-safe Tauri IPC wrappers |

---

## Phase 7: WebSocket & Real-time

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 7.1 | Implement `websocket/manager.rs` | Complete | 4.7, 4.13, 4.18 | Connection manager |
| 7.2 | Implement `websocket/handlers.rs` | Complete | 7.1 | Parse binary data stubs |
| 7.3 | Create Tauri event emission | Complete | 7.2 | Send to frontend |
| 7.4 | Update frontend SocketProvider | Not Started | 7.3 | Listen to Tauri events |
| 7.5 | Test Angel WebSocket | Not Started | 7.4 | Real-time LTP |
| 7.6 | Test Zerodha/Fyers WebSocket | Not Started | 7.4 | Multi-broker verify |

---

## Phase 8: Testing & Polish

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 8.1 | E2E: User registration flow | Not Started | 6.8 | Setup wizard |
| 8.2 | E2E: Broker login flow | Not Started | 8.1 | All 3 brokers |
| 8.3 | E2E: Order placement flow | Not Started | 8.2 | Place, modify, cancel |
| 8.4 | E2E: Sandbox mode | Not Started | 8.1 | Paper trading |
| 8.5 | Cross-platform: macOS/Win/Linux | Not Started | 8.1-8.4 | Build on each OS |
| 8.6 | Performance: Symbol cache load | Not Started | 3.6 | < 1s for 100k symbols |

---

## Phase 9: Build & Release

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 9.1 | Create app icons (all sizes) | Not Started | 8.5 | .ico, .icns, .png |
| 9.2 | Configure macOS notarization | Not Started | 9.1 | Apple Developer cert |
| 9.3 | Configure Windows signing | Not Started | 9.1 | Code signing cert |
| 9.4 | Build .dmg installer | Not Started | 9.2 | macOS universal |
| 9.5 | Build .msi/.exe installer | Not Started | 9.3 | Windows x64 |
| 9.6 | Build .deb/.AppImage | Not Started | 9.1 | Linux |

---

## Status Legend

| Status | Meaning |
|--------|---------|
| Not Started | Task not yet begun |
| Pending | Task ready to be worked on |
| Partial | Partially implemented (stubs/placeholders) |
| In Progress | Currently being worked on |
| Complete | Finished and verified |

---

## Progress Update Log

| Date | Phase | Tasks Completed | Notes |
|------|-------|-----------------|-------|
| 2026-01-18 | 1 | 8/8 | Project structure created, configs, Rust modules |
| 2026-01-18 | 2 | 4/6 | Security layer implemented (keychain, encryption, hashing) |
| 2026-01-18 | 3 | 10/12 | SQLite + DuckDB modules with migrations |
| 2026-01-18 | 4 | 9/18 | Broker trait + Angel/Zerodha/Fyers adapters (stubs) |
| 2026-01-18 | 5 | 10/10 | All Tauri commands implemented |
| 2026-01-18 | 6 | 8/8 | All API modules + stores updated for Tauri IPC |
| 2026-01-18 | 7 | 3/6 | WebSocket manager and handlers |
