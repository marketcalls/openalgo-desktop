//! Services Layer
//!
//! Business logic shared between Tauri IPC commands and REST API handlers.
//! This architecture matches the Flask OpenAlgo pattern where services
//! contain the core logic and are called by both internal routes and external APIs.
//!
//! # Architecture
//!
//! ```text
//! Frontend UI  --> Tauri Commands ──┐
//!                                   ├──> Services --> Broker/DB
//! External SDK --> REST API ────────┘
//! ```
//!
//! # Services
//!
//! - `OrderService` - Place, modify, cancel orders
//! - `PositionService` - Get positions, close positions
//! - `HoldingsService` - Get holdings
//! - `FundsService` - Get funds/margin
//! - `QuotesService` - Get quotes, market depth
//! - `OrderbookService` - Get order book, trade book
//! - `SmartOrderService` - Smart orders, split orders, basket orders
//! - `SymbolService` - Symbol search, lookup, master contract
//! - `AnalyzerService` - Analyze mode (sandbox) management
//! - `OptionsService` - Option chain, Greeks, option orders
//! - `HistoryService` - Historical data

pub mod order_service;
pub mod position_service;
pub mod holdings_service;
pub mod funds_service;
pub mod quotes_service;
pub mod orderbook_service;
pub mod smart_order_service;
pub mod symbol_service;
pub mod analyzer_service;
pub mod options_service;
pub mod history_service;

// Re-export commonly used types and services
pub use order_service::{OrderService, PlaceOrderResult, ModifyOrderResult, CancelOrderResult};
pub use position_service::{PositionService, PositionResult, ClosePositionResult};
pub use holdings_service::{HoldingsService, HoldingsResult};
pub use funds_service::{FundsService, FundsResult};
pub use quotes_service::{QuotesService, QuoteResult, DepthResult};
pub use orderbook_service::{OrderbookService, OrderbookResult, TradebookResult, OrderStatusResult};
pub use smart_order_service::{SmartOrderService, SmartOrderResult, SplitOrderResult};
pub use symbol_service::{SymbolService, SymbolSearchResult, ExpiryResult};
pub use analyzer_service::{AnalyzerService, AnalyzerStatus};
pub use options_service::{OptionsService, OptionChainResult, OptionGreeks, OptionSymbolResult, SyntheticFutureResult};
pub use history_service::{HistoryService, HistoryResult, IntervalsResult, CandleData};
