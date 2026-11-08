//! Audit P-10, web broker/dhan/streaming/dhan_adapter.py:409-412 and
//! 972-1066 (`depth_20_timeouts`, `_fallback_monitor_loop`): a 20-level
//! book that does not come within 30 s of the subscription, or stops for
//! 30 s, falls back to the main feed's 5 levels while held; a book that
//! keeps coming stays on the depth socket.

use super::super::BrokerBridge;
use crate::brokers::common::streaming::{FeedEvent, FeedMode, NormalizedDepth};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::feed::source::{InstrumentKey, MarketDataSource, Mode};
use crate::websocket::WebSocketManager;
use std::sync::Arc;
use std::time::Duration;

fn row(symbol: &str, token: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: symbol.into(),
        exchange: "NSE".into(),
        brexchange: "NSE".into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

/// A Dhan-like bridge: 20-level books on a socket of their own, 30 s of
/// silence before one falls back.
fn dhan_like() -> (Arc<BrokerBridge>, Arc<WebSocketManager>) {
    let depth = Arc::new(WebSocketManager::new());
    let symbols = SymbolResolver::new();
    symbols.load(vec![row("SBIN", "3045"), row("INFY", "1594")]);
    let bridge =
        BrokerBridge::with_depth_manager(Arc::new(WebSocketManager::new()), depth.clone(), symbols);
    bridge.set_deep_levels(Some(20));
    bridge.set_deep_book_timeout(Some(Duration::from_secs(30)));
    bridge.start();
    (bridge, depth)
}

fn book(symbol: &str) -> Arc<FeedEvent> {
    Arc::new(FeedEvent::Depth(NormalizedDepth {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        buy: vec![Default::default(); 20],
        sell: vec![Default::default(); 20],
        ..Default::default()
    }))
}

fn on_main_at_depth(bridge: &BrokerBridge, symbol: &str) -> Option<u8> {
    bridge
        .applied()
        .iter()
        .find(|s| s.symbol == symbol && s.mode == FeedMode::Depth)
        .map(|s| s.depth)
}

fn on_depth_socket(bridge: &BrokerBridge, symbol: &str) -> bool {
    bridge.applied_deep().iter().any(|s| s.symbol == symbol)
}

/// Let the bridge's tasks run on the paused clock.
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_silent_deep_book_falls_back_and_a_live_one_stays() {
    let (bridge, depth) = dhan_like();
    let sbin = InstrumentKey::new("SBIN", "NSE");
    let infy = InstrumentKey::new("INFY", "NSE");
    assert_eq!(bridge.subscribe(&sbin, Mode::Depth, 20).await, Ok(()));
    assert_eq!(bridge.subscribe(&infy, Mode::Depth, 20).await, Ok(()));
    assert!(on_depth_socket(&bridge, "SBIN") && on_depth_socket(&bridge, "INFY"));

    // INFY's book keeps coming; SBIN's never does.
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(10)).await;
        depth.emit(book("INFY"));
        settle().await;
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert!(
        !on_depth_socket(&bridge, "SBIN"),
        "{:?}",
        bridge.applied_deep()
    );
    assert_eq!(on_main_at_depth(&bridge, "SBIN"), Some(5));
    assert!(on_depth_socket(&bridge, "INFY"));

    // INFY's book stops: it falls back 30 s after the last one.
    tokio::time::advance(Duration::from_secs(31)).await;
    settle().await;
    assert!(!on_depth_socket(&bridge, "INFY"));
    assert_eq!(on_main_at_depth(&bridge, "INFY"), Some(5));

    // The web does not move it back; a new subscription tries 20 again.
    assert_eq!(bridge.unsubscribe(&sbin, Mode::Depth, 20).await, Ok(()));
    settle().await;
    assert_eq!(bridge.subscribe(&sbin, Mode::Depth, 20).await, Ok(()));
    assert!(on_depth_socket(&bridge, "SBIN"));
    bridge.stop().await;
}

/// A broker whose web adapter has no such fallback (Fyers' TBT) keeps its
/// deep book however quiet it is.
#[tokio::test(start_paused = true)]
async fn without_a_timeout_a_quiet_deep_book_stays() {
    let (bridge, _depth) = dhan_like();
    bridge.set_deep_book_timeout(None);
    let sbin = InstrumentKey::new("SBIN", "NSE");
    assert_eq!(bridge.subscribe(&sbin, Mode::Depth, 20).await, Ok(()));
    tokio::time::advance(Duration::from_secs(120)).await;
    settle().await;
    assert!(on_depth_socket(&bridge, "SBIN"));
    bridge.stop().await;
}
