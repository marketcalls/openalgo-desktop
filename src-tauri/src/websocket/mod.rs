//! Outbound broker market-data feed.
//!
//! `WebSocketManager` owns the broker socket and publishes normalised
//! `FeedEvent`s; the client-facing feed server (`crate::feed`) subscribes
//! with `subscribe_ticks()` and maps them to the web's `market_data` frames.
//! Broker specifics live in each adapter's `BrokerFeed`.

mod manager;

pub use crate::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedMode, FeedSubscription, MarketEvent, NormalizedDepth,
    NormalizedTick, OrderUpdate,
};
pub use manager::{
    Connection, DropCause, FeedConfig, FeedStats, FeedStatus, LastDrop, Reader, SinceConnect,
    WebSocketManager, TICK_CHANNEL_CAP,
};
