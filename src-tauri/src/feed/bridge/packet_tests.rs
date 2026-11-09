//! Audit R107-04 and R107-17, at the bridge: each event knows the session
//! of the connection that sent it, a new session forgets only what its own
//! connection said (the deeper socket's apart), and a broker message's
//! events are one packet, so pairing never depends on timing.

use super::render::tests::full_tick;
use super::*;
use crate::brokers::common::streaming::{MarketEvent, NormalizedDepth};

pub(super) fn sid(conn: usize, session: u64) -> SessionId {
    SessionId { conn, session }
}

pub(super) fn opened(from: SessionId) -> Packet {
    Packet {
        from,
        kind: PacketKind::Opened,
        events_before: 0,
    }
}

pub(super) fn closed(from: SessionId) -> Packet {
    Packet {
        from,
        kind: PacketKind::Closed,
        events_before: 0,
    }
}

pub(super) fn packet(from: SessionId, events: Vec<FeedEvent>) -> Packet {
    Packet {
        from,
        kind: PacketKind::Events(events.into_iter().map(MarketEvent::new).collect()),
        events_before: 0,
    }
}

pub(super) fn tick(symbol: &str) -> FeedEvent {
    FeedEvent::Tick(full_tick(symbol, "NSE"))
}

pub(super) fn book(symbol: &str, levels: usize) -> FeedEvent {
    FeedEvent::Depth(NormalizedDepth {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        buy: vec![Default::default(); levels],
        sell: vec![Default::default(); levels],
        ..Default::default()
    })
}

/// A bridge holding `symbols` at Depth, and its updates.
pub(super) fn bridge(
    symbols: &[&str],
) -> (Arc<BrokerBridge>, broadcast::Receiver<Arc<MarketUpdate>>) {
    let bridge = BrokerBridge::new(Arc::new(WebSocketManager::new()), SymbolResolver::new());
    for s in symbols {
        drop(bridge.subscribe(&InstrumentKey::new(*s, "NSE"), Mode::Depth, 5));
    }
    let rx = bridge.updates();
    (bridge, rx)
}

pub(super) fn drain(rx: &mut broadcast::Receiver<Arc<MarketUpdate>>) -> Vec<Arc<MarketUpdate>> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn open_price(u: &MarketUpdate) -> Option<f64> {
    u.quote.as_ref().and_then(|q| q.open)
}

/// Concrete input A: a pool opens a second connection for another
/// instrument; the first connection's instrument keeps its quote, so its
/// next book carries it (it was sent with LTP 0 and no quote).
#[test]
fn a_new_pooled_connection_keeps_the_other_connections_quotes() {
    let (b, mut rx) = bridge(&["SBIN"]);
    let first = sid(0, 1);
    b.relay_packet(&opened(first), false);
    b.relay_packet(&packet(first, vec![tick("SBIN")]), false);
    b.relay_packet(&opened(sid(1, 2)), false);
    b.relay_packet(&packet(first, vec![book("SBIN", 5)]), false);
    let book = drain(&mut rx).pop().expect("the book");
    assert_eq!((book.ltp, open_price(&book)), (1167.7, Some(1180.1)));
}

/// A secondary connection's reconnect forgets its own instruments' quotes
/// only.
#[test]
fn a_reconnect_forgets_only_its_own_connections_quotes() {
    let (b, mut rx) = bridge(&["SBIN", "TCS"]);
    b.relay_packet(&packet(sid(0, 1), vec![tick("SBIN")]), false);
    b.relay_packet(&packet(sid(1, 2), vec![tick("TCS")]), false);
    b.relay_packet(&opened(sid(1, 3)), false);
    drain(&mut rx);
    b.relay_packet(&packet(sid(1, 3), vec![book("TCS", 5)]), false);
    b.relay_packet(&packet(sid(0, 1), vec![book("SBIN", 5)]), false);
    let updates = drain(&mut rx);
    let open = |s: &str| {
        updates
            .iter()
            .find(|u| u.key.symbol == s)
            .and_then(|u| open_price(u))
    };
    assert_eq!(open("TCS"), None, "its connection was replaced");
    assert_eq!(open("SBIN"), Some(1180.1), "another connection's");
}

/// Concrete input B: only the deeper socket reconnects; a full tick then
/// carries no book from before the outage, and the main quote stays.
#[test]
fn a_deep_reconnect_forgets_the_deep_books_only() {
    let (b, mut rx) = bridge(&["SBIN"]);
    b.set_deep_levels(Some(20));
    drop(b.subscribe(&InstrumentKey::new("SBIN", "NSE"), Mode::Depth, 20));
    b.relay_packet(&packet(sid(0, 1), vec![tick("SBIN")]), false);
    b.relay_packet(&packet(sid(0, 1), vec![book("SBIN", 20)]), true);
    b.relay_packet(&packet(sid(0, 1), vec![tick("SBIN")]), false);
    let before = drain(&mut rx).pop().expect("a tick");
    assert_eq!(before.deep_book.as_ref().map(|d| d.buy.len()), Some(20));
    b.relay_packet(&opened(sid(0, 2)), true);
    b.relay_packet(&packet(sid(0, 1), vec![tick("SBIN")]), false);
    let after = drain(&mut rx).pop().expect("a tick");
    assert!(
        after.deep_book.is_none(),
        "the old deep book is not current"
    );
    assert_eq!(open_price(&after), Some(1180.1), "the main quote stays");
}

/// A packet of a session already replaced is not relayed.
#[test]
fn a_replaced_sessions_packet_is_dropped() {
    let (b, mut rx) = bridge(&["SBIN"]);
    b.relay_packet(&opened(sid(0, 2)), false);
    b.relay_packet(&packet(sid(0, 1), vec![tick("SBIN")]), false);
    assert!(drain(&mut rx).is_empty());
}

/// A packet's full tick and book are one update whatever the timing; the
/// same events in two packets (the sender paused between them) are a tick
/// and an exact book, and another connection's packet in between pairs
/// nothing with them.
#[test]
fn a_packet_pairs_its_own_tick_and_book_only() {
    let (b, mut rx) = bridge(&["SBIN", "TCS"]);
    let (main, other) = (sid(0, 1), sid(1, 2));
    b.relay_packet(&packet(main, vec![tick("SBIN"), book("SBIN", 5)]), false);
    let one = drain(&mut rx);
    assert_eq!(one.len(), 1);
    assert!(!one[0].exact_mode && one[0].depth.is_some());

    b.relay_packet(&packet(main, vec![tick("SBIN")]), false);
    b.relay_packet(&packet(other, vec![tick("TCS"), book("TCS", 5)]), false);
    b.relay_packet(&packet(main, vec![book("SBIN", 5)]), false);
    let shape: Vec<(String, bool)> = drain(&mut rx)
        .iter()
        .map(|u| (u.key.symbol.clone(), u.exact_mode))
        .collect();
    assert_eq!(
        shape,
        [
            ("SBIN".to_string(), false),
            ("TCS".to_string(), false),
            ("SBIN".to_string(), true)
        ]
    );
}

/// R107-04 A as the fail-before branch reproduced it on 1.0.7: the
/// manager's pool-wide connect count moving (a pool opening a connection)
/// forgets no instrument's quote.
#[test]
fn the_pools_connect_count_forgets_no_quote() {
    let manager = Arc::new(WebSocketManager::new());
    let bridge = BrokerBridge::new(manager.clone(), SymbolResolver::new());
    drop(bridge.subscribe(&InstrumentKey::new("SBIN", "NSE"), Mode::Depth, 5));
    bridge.convert(&tick("SBIN"));
    manager
        .stats()
        .connects
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let u = bridge.convert(&book("SBIN", 5)).expect("the book");
    assert_eq!(open_price(&u), Some(1180.1), "LTP {}", u.ltp);
}
