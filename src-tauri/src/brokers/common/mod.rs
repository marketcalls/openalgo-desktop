//! Code shared by every broker adapter: the HTTP client, pacing, the symbol
//! resolver, OpenAlgo constants, history and master-contract helpers, and
//! the streaming contract.

pub mod de;
pub mod history;
pub mod http;
pub mod mapping;
pub mod master_contract;
pub mod mpp;
pub mod order_poll;
pub mod position_read;
pub mod ratelimit;
pub mod redact;
pub mod relay;
pub mod streaming;
pub mod symbols;

pub use mapping::{Action, Exchange, InvalidConstant, OrderStatus, PriceType, Product, Validity};
pub use streaming::{
    BrokerFeed, FeedEvent, FeedMode, FeedSubscription, MarketEvent, NormalizedDepth,
    NormalizedTick, OrderUpdate,
};
pub use symbols::{ContractQuery, SymToken, SymbolGeneration, SymbolResolver};
