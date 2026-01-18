# OpenAlgo Desktop - Migration Tracker

## Summary

| Phase | Name | Tasks | Status | Progress |
|-------|------|-------|--------|----------|
| 1 | Project Setup | 8 | **Complete** | 8/8 |
| 2 | Security Layer | 6 | Skeleton | 0/6 |
| 3 | Database Layer | 12 | Skeleton | 0/12 |
| 4 | Broker Adapters | 18 | Skeleton | 0/18 |
| 5 | Tauri Commands | 10 | Skeleton | 0/10 |
| 6 | Frontend Integration | 8 | Skeleton | 0/8 |
| 7 | WebSocket & Real-time | 6 | Skeleton | 0/6 |
| 8 | Testing & Polish | 6 | Not Started | 0/6 |
| 9 | Build & Release | 6 | Not Started | 0/6 |
| **Total** | | **80** | | **8/80 (10%)** |

> **Note:** "Skeleton" means module files exist with basic structure and compile successfully, but need full implementation and testing.

---

## Phase 1: Project Setup ✅

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 1.1 | Create `openalgo-desktop/` folder | **Complete** | - | Root directory |
| 1.2 | Initialize npm project with package.json | **Complete** | 1.1 | React 19, Vite 7, Tauri deps |
| 1.3 | Initialize Tauri 2.0 project | **Complete** | 1.2 | tauri.conf.json, build.rs |
| 1.4 | Set up Cargo.toml with dependencies | **Complete** | 1.3 | All Rust dependencies |
| 1.5 | Create Rust module structure | **Complete** | 1.4 | commands/, db/, brokers/, security/, scheduler/, websocket/ |
| 1.6 | Copy React frontend from openalgo | **Complete** | 1.2 | src/, public/, configs |
| 1.7 | Configure Vite for Tauri | **Complete** | 1.6 | Port 5173, Tauri env prefix |
| 1.8 | Verify `npm run tauri dev` works | **Complete** | 1.7 | App runs with keychain, DB migrations pass |

---

## Phase 2: Security Layer (Skeleton exists)

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 2.1 | Implement `security/mod.rs` | Skeleton | 1.5 | Basic structure exists, needs full implementation |
| 2.2 | Implement `security/keychain.rs` | Skeleton | 2.1 | keyring crate setup, needs testing |
| 2.3 | Implement `security/encryption.rs` | Skeleton | 2.1 | AES-256-GCM structure, needs testing |
| 2.4 | Implement `security/hashing.rs` | Skeleton | 2.1 | Argon2id structure, needs testing |
| 2.5 | Test keychain on macOS | Not Started | 2.2 | Needs verification |
| 2.6 | Test keychain on Windows/Linux | Not Started | 2.2 | Needs cross-platform testing |

---

## Phase 3: Database Layer (Skeleton exists)

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 3.1 | Implement `db/sqlite/connection.rs` | Skeleton | 1.5 | Connection wrapper exists |
| 3.2 | Implement `db/sqlite/migrations.rs` | Skeleton | 3.1 | Tables created, needs CRUD testing |
| 3.3 | Implement `db/sqlite/auth.rs` | Skeleton | 3.2 | Structure exists, needs implementation |
| 3.4 | Implement `db/sqlite/user.rs` | Skeleton | 3.2 | Structure exists, needs implementation |
| 3.5 | Implement `db/sqlite/api_keys.rs` | Skeleton | 3.2 | Structure exists, needs implementation |
| 3.6 | Implement `db/sqlite/symbol.rs` | Skeleton | 3.2 | Structure exists, needs implementation |
| 3.7 | Implement `db/sqlite/strategy.rs` | Skeleton | 3.2 | Structure exists, needs implementation |
| 3.8 | Implement `db/sqlite/settings.rs` | Skeleton | 3.2 | Structure exists, needs implementation |
| 3.9 | Implement `db/sqlite/sandbox.rs` | Skeleton | 3.2 | Structure exists, needs implementation |
| 3.10 | Implement `db/duckdb/mod.rs` | Skeleton | 1.5 | Connection wrapper exists |
| 3.11 | Implement `db/duckdb/migrations.rs` | Skeleton | 3.10 | Tables created, needs CRUD |
| 3.12 | Implement `db/duckdb/market_data.rs` | Skeleton | 3.11 | Structure exists, needs implementation |

---

## Phase 4: Broker Adapters (Skeleton exists)

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 4.1 | Define `brokers/mod.rs` trait | Skeleton | 1.5 | Broker trait interface exists |
| 4.2 | Define `brokers/types.rs` | Skeleton | 4.1 | Order, Position, Quote types exist |
| 4.3 | Implement `brokers/angel/auth.rs` | Skeleton | 4.1 | Structure exists, needs testing |
| 4.4 | Implement `brokers/angel/orders.rs` | Skeleton | 4.3 | Structure exists, needs testing |
| 4.5 | Implement `brokers/angel/data.rs` | Skeleton | 4.3 | Stubs for positions, holdings |
| 4.6 | Implement `brokers/angel/quotes.rs` | Skeleton | 4.3 | Stub for quote |
| 4.7 | Implement `brokers/angel/websocket.rs` | Not Started | 4.3 | Binary protocol, little-endian |
| 4.8 | Implement `brokers/angel/mapping.rs` | Not Started | 4.1 | Exchange/product mapping |
| 4.9 | Implement `brokers/zerodha/auth.rs` | Skeleton | 4.1 | Structure exists, needs testing |
| 4.10 | Implement `brokers/zerodha/orders.rs` | Skeleton | 4.9 | Structure exists, needs testing |
| 4.11 | Implement `brokers/zerodha/data.rs` | Skeleton | 4.9 | Stubs for positions, holdings |
| 4.12 | Implement `brokers/zerodha/quotes.rs` | Skeleton | 4.9 | Stub for GET /quote |
| 4.13 | Implement `brokers/zerodha/websocket.rs` | Not Started | 4.9 | Binary, big-endian |
| 4.14 | Implement `brokers/fyers/auth.rs` | Skeleton | 4.1 | Structure exists, needs testing |
| 4.15 | Implement `brokers/fyers/orders.rs` | Skeleton | 4.14 | Structure exists, needs testing |
| 4.16 | Implement `brokers/fyers/data.rs` | Skeleton | 4.14 | Stubs |
| 4.17 | Implement `brokers/fyers/quotes.rs` | Skeleton | 4.14 | Stub |
| 4.18 | Implement `brokers/fyers/websocket.rs` | Not Started | 4.14 | HSM binary protocol |

---

## Phase 5: Tauri Commands (Skeleton exists)

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 5.1 | Implement `commands/auth.rs` | Skeleton | 3.4, 2.4 | Structure exists, needs implementation |
| 5.2 | Implement `commands/broker.rs` | Skeleton | 4.3, 4.9, 4.14 | Structure exists, needs implementation |
| 5.3 | Implement `commands/orders.rs` | Skeleton | 4.4, 4.10, 4.15 | Structure exists, needs implementation |
| 5.4 | Implement `commands/positions.rs` | Skeleton | 4.5, 4.11, 4.16 | Structure exists, needs implementation |
| 5.5 | Implement `commands/holdings.rs` | Skeleton | 4.5, 4.11, 4.16 | Structure exists, needs implementation |
| 5.6 | Implement `commands/quotes.rs` | Skeleton | 4.6, 4.12, 4.17 | Structure exists, needs implementation |
| 5.7 | Implement `commands/strategy.rs` | Skeleton | 3.7 | Structure exists, needs implementation |
| 5.8 | Implement `commands/settings.rs` | Skeleton | 3.8, 2.2 | Structure exists, needs implementation |
| 5.9 | Implement `commands/sandbox.rs` | Skeleton | 3.9 | Structure exists, needs implementation |
| 5.10 | Implement `commands/historify.rs` | Skeleton | 3.12 | Structure exists, needs implementation |

---

## Phase 6: Frontend Integration (Skeleton exists)

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 6.1 | Modify `src/api/client.ts` | Skeleton | 5.1-5.10 | Re-exports Tauri client |
| 6.2 | Update `src/api/auth.ts` | Skeleton | 6.1 | Structure exists, needs implementation |
| 6.3 | Update `src/api/trading.ts` | Skeleton | 6.1 | Structure exists, needs implementation |
| 6.4 | Update `src/api/strategy.ts` | Skeleton | 6.1 | Structure exists, needs implementation |
| 6.5 | Update `src/stores/authStore.ts` | Skeleton | 6.2 | Structure exists, needs implementation |
| 6.6 | Update `src/stores/themeStore.ts` | Skeleton | 6.1 | Structure exists, needs implementation |
| 6.7 | Update `src/components/auth/AuthSync.tsx` | Skeleton | 6.5 | Structure exists, needs implementation |
| 6.8 | Create `src/api/tauri-client.ts` | Skeleton | 5.1-5.10 | Type-safe Tauri IPC wrappers |

---

## Phase 7: WebSocket & Real-time (Skeleton exists)

| ID | Task | Status | Dependencies | Notes |
|----|------|--------|--------------|-------|
| 7.1 | Implement `websocket/manager.rs` | Skeleton | 4.7, 4.13, 4.18 | Structure exists, needs implementation |
| 7.2 | Implement `websocket/handlers.rs` | Skeleton | 7.1 | Parse binary data stubs |
| 7.3 | Create Tauri event emission | Skeleton | 7.2 | Structure exists, needs implementation |
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
| Skeleton | Module/file exists with basic structure, compiles, but needs full implementation |
| In Progress | Currently being worked on |
| **Complete** | Finished and verified |

---

## Progress Update Log

| Date | Phase | Tasks Completed | Notes |
|------|-------|-----------------|-------|
| 2026-01-18 | 1 | 8/8 (skeleton) | Project structure created, configs, Rust module skeletons |
| 2026-01-19 | 1 | 8/8 | **Phase 1 Complete**: Fixed DuckDB migrations, Rust warnings, port config. App runs with `npm run tauri dev`. Skeleton code exists for phases 2-7. |
