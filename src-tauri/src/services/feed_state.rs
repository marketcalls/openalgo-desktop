//! Where the feed's subscriptions and order updates stand now, for health
//! (`/health/api/current`: `market_data.subscriptions`,
//! `market_data.order_updates` and each broker feed's `connections`; audit
//! Phase 3.5 and its order-stream section).
//!
//! * Subscriptions taken and not yet streaming (waiting for the broker
//!   session or the symbol master), releases the broker has not confirmed
//!   yet or refused (tried again), and those refused while no app waited
//!   (re-applied after a broker session started), listed with why.
//! * Where order updates come from and whether that source is up. The web
//!   answers `subscribe_orders` with success either way, and so does the
//!   feed here: an app asking for order updates is not the broker's order
//!   feed running, so health says which it is.
//! * The market feed's connections (audit C07): the instruments each
//!   carries and its state.

use crate::services::broker_runtime::OrderSource;
use crate::services::feed_connections::state;
use crate::state::AppState;
use crate::websocket::WebSocketManager;
use serde_json::{json, Value};

/// Refused subscriptions listed by name; the count covers them all.
const LISTED: usize = 20;

/// The `subscriptions` entry.
pub fn subscriptions(ctx: &AppState) -> Value {
    let s = ctx.bridge.operations();
    json!({
        "waiting": s.waiting,
        "releasing": s.releasing,
        "release_retrying": s.release_retrying,
        "refused": s.refused.len(),
        "refused_items": s.refused.iter().take(LISTED).map(|r| json!({
            "symbol": r.symbol,
            "exchange": r.exchange,
            "mode": r.mode,
            "reason": r.reason,
        })).collect::<Vec<_>>(),
    })
}

/// The `order_updates` entry: the source, and whether it is up (`null`
/// where that is not known apart from the source: an adapter's own
/// reader, or no source).
pub fn order_updates(ctx: &AppState) -> Value {
    let (source, connected) = match ctx.runtime.order_source() {
        OrderSource::None => ("none", None),
        OrderSource::Socket => ("order_socket", Some(ctx.runtime.order_ws.is_connected())),
        OrderSource::MarketFeed => ("market_feed", Some(ctx.websocket.is_connected())),
        OrderSource::Adapter => ("broker_adapter", None),
        OrderSource::NotStarted => ("not_started", Some(false)),
    };
    json!({"source": source, "connected": connected})
}

/// Whether the `market_data` block warns for what this module adds: a
/// subscription the broker connection refused, or an order feed that is
/// down or did not start.
pub fn needs_attention(block: &Value) -> bool {
    let refused = block["subscriptions"]["refused"].as_u64().unwrap_or(0) > 0;
    let orders = &block["order_updates"];
    refused || orders["source"] == "not_started" || orders["connected"] == json!(false)
}

/// A feed's open connections: the instruments each carries and its state,
/// with the room each has.
pub fn connections(feed: &WebSocketManager) -> Value {
    let limits = feed.limits();
    json!({
        "per_connection": limits.per_connection,
        "most": limits.connections,
        "open": feed.connection_states().iter().map(|(instruments, status)| json!({
            "instruments": instruments,
            "state": state(status),
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::websocket::PoolLimits;

    #[test]
    fn a_feed_lists_its_room_and_no_connection_before_it_starts() {
        let m = WebSocketManager::new();
        let v = connections(&m);
        assert_eq!(v["per_connection"], PoolLimits::single(3000).per_connection);
        assert_eq!(v["most"], 1);
        assert_eq!(v["open"], json!([]));
    }
}
