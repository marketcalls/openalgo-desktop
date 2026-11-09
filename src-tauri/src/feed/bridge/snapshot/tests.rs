//! The audit's minimum regression matrix for merges (C03): previously
//! supplied fields stay within a connection, absent never becomes 0, a
//! genuine 0 stays, and a new connection starts afresh.

use super::*;
use crate::feed::bridge::render::tests::full_tick;

fn quote_packet() -> NormalizedTick {
    NormalizedTick {
        mode: 2,
        ltp: 219.3,
        open: 480.0,
        high: 495.0,
        low: 210.0,
        close: 494.05,
        volume: 1_234_575,
        average_price: 300.5,
        last_quantity: 25,
        total_buy_quantity: 700,
        total_sell_quantity: 800,
        last_trade_time_ms: 1_740_729_550_000,
        timestamp_ms: 1_740_729_550_100,
        ..full_tick("NIFTY28NOV26FUT", "NFO")
    }
}

/// Only the price, its quantity and trade time (Upstox LTPC).
fn ltpc_packet(ltp: f64) -> NormalizedTick {
    let mut t = NormalizedTick {
        symbol: "NIFTY28NOV26FUT".into(),
        exchange: "NFO".into(),
        mode: 2,
        ltp,
        last_quantity: 150,
        last_trade_time_ms: 1_740_729_553_000,
        timestamp_ms: 1_740_729_553_100,
        ..Default::default()
    };
    for f in [
        QuoteField::Open,
        QuoteField::High,
        QuoteField::Low,
        QuoteField::Close,
        QuoteField::Volume,
        QuoteField::AveragePrice,
        QuoteField::TotalBuyQuantity,
        QuoteField::TotalSellQuantity,
    ] {
        t.mark_absent(f);
    }
    t
}

/// Only open interest, no trade (HDFC Sky's OI packet).
fn oi_packet(oi: i64) -> NormalizedTick {
    let mut t = NormalizedTick {
        symbol: "NIFTY28NOV26FUT".into(),
        exchange: "NFO".into(),
        mode: 1,
        oi,
        timestamp_ms: 1_740_729_554_000,
        ..Default::default()
    };
    t.mark_absent(QuoteField::Ltp);
    t.mark_carried(QuoteField::OpenInterest);
    t
}

/// Audit regression matrix row 1 (web upstox_adapter.py `_merge_feed`,
/// audit P-01's probe): full, then LTPC-only, then OI-only, then full. The
/// quote an LTPC packet does not repeat stays, its new price and trade time
/// replace the old, the change follows the new price, and an OI-only
/// packet changes OI alone.
#[test]
fn a_partial_packet_keeps_the_quote_it_did_not_repeat() {
    let full = quote_packet();
    let s = merge(None, &full, session(1));
    let s = merge(Some(&s), &ltpc_packet(220.0), session(1));
    let t = s.tick();
    assert_eq!((t.ltp, t.last_quantity), (220.0, 150));
    assert_eq!((t.open, t.close, t.volume), (480.0, 494.05, 1_234_575));
    assert_eq!(t.last_trade_time_ms, 1_740_729_553_000);
    assert_eq!((t.change, t.change_percent), (-274.05, -55.47));
    let k = t.known();
    for f in [QuoteField::Open, QuoteField::Volume, QuoteField::Change] {
        assert!(k.contains(f), "{:?}", f);
    }
    assert!(
        !k.contains(QuoteField::OpenInterest),
        "a Quote packet has none"
    );

    let s = merge(Some(&s), &oi_packet(98_765), session(1));
    let t = s.tick();
    assert_eq!((t.ltp, t.oi, t.volume), (220.0, 98_765, 1_234_575));
    assert!(t.knows(QuoteField::OpenInterest) && t.knows(QuoteField::Ltp));
    assert_eq!(
        t.last_trade_time_ms, 1_740_729_553_000,
        "no trade, same time"
    );

    let s = merge(Some(&s), &full, session(1));
    assert_eq!((s.tick().ltp, s.tick().oi), (219.3, 98_765));
}

/// An OI-only packet before the first price makes a snapshot with no price
/// (the bridge publishes nothing for it) and its OI waits for the price.
#[test]
fn a_packet_without_a_price_first_keeps_its_fields_for_the_price() {
    let s = merge(None, &oi_packet(0), session(1));
    assert!(!s.tick().knows(QuoteField::Ltp));
    assert!(s.tick().knows(QuoteField::OpenInterest), "a genuine 0 OI");
    let s = merge(Some(&s), &quote_packet(), session(1));
    assert_eq!(s.tick().oi, 0);
    assert!(s.tick().knows(QuoteField::OpenInterest));
}

/// A packet with neither a price nor a trade time (HDFC before the first
/// trade, bk-b's no_trade_yet) brings only what it marks as carried: a
/// book on it carries no `ltt` and none of the zeros its mode would stand
/// for; a real trade time from earlier in the connection stays.
#[test]
fn a_packet_without_a_trade_brings_only_what_it_carries() {
    use crate::brokers::common::streaming::NormalizedDepth;
    use crate::feed::InstrumentKey;
    let mut t = NormalizedTick {
        symbol: "NIFTY28NOV26FUT".into(),
        exchange: "NFO".into(),
        mode: 3,
        close: 494.05,
        timestamp_ms: 1_740_729_540_000,
        ..Default::default()
    };
    t.mark_absent(QuoteField::Ltp);
    t.mark_absent(QuoteField::LastTradeTime);
    t.mark_carried(QuoteField::Close);
    let s = merge(None, &t, session(1));
    let k = s.tick().known();
    for f in [
        QuoteField::Open,
        QuoteField::High,
        QuoteField::Low,
        QuoteField::Volume,
    ] {
        assert!(!k.contains(f), "{:?}", f);
    }
    assert!(k.contains(QuoteField::Close));
    let mut valued = t.clone();
    valued.volume = 5;
    assert!(
        merge(None, &valued, session(1))
            .tick()
            .knows(QuoteField::Volume),
        "a value it sent"
    );
    let book = NormalizedDepth {
        symbol: t.symbol.clone(),
        exchange: t.exchange.clone(),
        timestamp_ms: t.timestamp_ms,
        ..Default::default()
    };
    let key = InstrumentKey::new(t.symbol.clone(), t.exchange.clone());
    let u = crate::feed::bridge::depth_update(key, &book, Some(s.tick()), s.book_lacks());
    assert_eq!(u.ltt, None, "not the packet's time");
    let q = u.quote.unwrap_or_default();
    assert_eq!((q.open, q.high, q.volume), (None, None, None));
    assert_eq!(q.close, Some(494.05));

    let s = merge(
        Some(&merge(None, &quote_packet(), session(1))),
        &t,
        session(1),
    );
    assert!(!s.tick().absent.contains(QuoteField::LastTradeTime));
    assert_eq!(
        s.tick().last_trade_time_ms,
        1_740_729_550_000,
        "the earlier real one"
    );
}

/// A genuine zero replaces a value: no volume yet after a session's first
/// packet, a total that went to 0.
#[test]
fn a_genuine_zero_replaces_the_old_value() {
    let s = merge(None, &quote_packet(), session(1));
    let mut next = quote_packet();
    next.total_buy_quantity = 0;
    next.volume = 0;
    let s = merge(Some(&s), &next, session(1));
    assert_eq!((s.tick().total_buy_quantity, s.tick().volume), (0, 0));
    assert!(s.tick().knows(QuoteField::TotalBuyQuantity));
}

/// Audit regression matrix row "Reconnect with first packet partial": a
/// packet of a connection's new session never merges what its earlier
/// session said; a pool's other connection opening, or the deeper socket
/// reconnecting, forgets nothing of the quote (audit R107-04).
#[test]
fn a_new_session_of_the_connection_starts_afresh() {
    let key = InstrumentKey::new("NIFTY28NOV26FUT", "NFO");
    let quoted = || HashMap::from([(key.clone(), merge(None, &quote_packet(), session(1)))]);
    let mut map = quoted();
    forget_session(&mut map, false, session(2));
    let s = merge(map.get(&key), &ltpc_packet(220.0), session(2));
    let t = s.tick();
    assert!(!t.knows(QuoteField::Open) && !t.knows(QuoteField::Close));
    assert!(!t.knows(QuoteField::Change), "no close of this connection");
    assert_eq!(t.open, 0.0);

    let mut map = quoted();
    let other = crate::websocket::SessionId {
        conn: 1,
        session: 3,
    };
    forget_session(&mut map, false, other);
    assert!(
        map[&key].tick().knows(QuoteField::Open),
        "a pool's new connection"
    );
    forget_session(&mut map, true, session(4));
    assert!(
        map[&key].tick().knows(QuoteField::Open),
        "the deeper socket's"
    );
}

/// A new price without a trade time does not keep the old trade time (it
/// belonged to the old price), and one with an explicit "no trade time"
/// mark keeps that mark.
#[test]
fn the_trade_time_goes_with_its_price() {
    let s = merge(None, &quote_packet(), session(1));
    let mut next = ltpc_packet(221.0);
    next.last_trade_time_ms = 0;
    let s = merge(Some(&s), &next, session(1));
    assert!(!s.tick().knows(QuoteField::LastTradeTime));
    assert!(!s.tick().absent.contains(QuoteField::LastTradeTime));
    next.mark_absent(QuoteField::LastTradeTime);
    let s = merge(Some(&s), &next, session(1));
    assert!(s.tick().absent.contains(QuoteField::LastTradeTime));
}

/// The optional shared fields stay until a packet sends a new value; the
/// packet's own exchange time and the broker-only fields last one packet
/// (bk-c's mstock/quote_mode, Upstox's auction price).
#[test]
fn extras_stay_and_broker_fields_last_one_packet() {
    use crate::brokers::common::streaming::{BrokerField, BrokerValue};
    let mut first = quote_packet();
    first.extras.upper_circuit = Some(540.0);
    first.extras.exchange_time_ms = Some(1_740_729_550_050);
    first.broker_fields.set(
        BrokerField::IndicativeEquilibriumPrice,
        BrokerValue::Float(0.0),
    );
    let s = merge(None, &first, session(1));
    assert_eq!(
        s.tick()
            .broker_fields
            .get(BrokerField::IndicativeEquilibriumPrice),
        Some(BrokerValue::Float(0.0)),
        "an explicit 0 is carried"
    );
    let mut next = ltpc_packet(220.0);
    next.extras.lower_circuit = Some(440.0);
    let s = merge(Some(&s), &next, session(1));
    let x = s.tick().extras;
    assert_eq!(
        (x.upper_circuit, x.lower_circuit),
        (Some(540.0), Some(440.0))
    );
    assert_eq!(x.exchange_time_ms, None, "a packet's own time is not kept");
    assert!(s.tick().broker_fields.is_empty());
}

/// A packet can clear an extra (a book that emptied both sides has no best
/// bid or ask: web groww depth clear), which a `None` would have kept.
#[test]
fn a_cleared_extra_is_gone_where_a_missing_one_stays() {
    use crate::brokers::common::streaming::ExtraField;
    let mut first = quote_packet();
    first.extras.bid_price = Some(219.25);
    first.extras.ask_price = Some(219.35);
    first.extras.upper_circuit = Some(540.0);
    let s = merge(None, &first, session(1));
    let s = merge(Some(&s), &ltpc_packet(220.0), session(1));
    assert_eq!(s.tick().extras.bid_price, Some(219.25), "not sent: kept");
    let mut emptied = ltpc_packet(220.5);
    emptied.extras.clear_best_bid_ask();
    let s = merge(Some(&s), &emptied, session(1));
    let x = s.tick().extras;
    assert_eq!((x.bid_price, x.ask_price), (None, None));
    assert_eq!(x.upper_circuit, Some(540.0));
    let mut circuit = ltpc_packet(221.0);
    circuit.extras.clear(ExtraField::UpperCircuit);
    let s = merge(Some(&s), &circuit, session(1));
    assert_eq!(s.tick().extras.upper_circuit, None);
    assert!(
        s.tick().extras.cleared == Default::default(),
        "the mark is per packet"
    );
}

/// A field a packet clears is dropped from the snapshot until a packet
/// carries it again (web Tradejini and Samco prune the book's totals when
/// the Depth holder leaves); one it only lacks is kept.
#[test]
fn a_cleared_field_is_gone_where_an_absent_one_stays() {
    let s = merge(None, &quote_packet(), session(1));
    let mut gone = quote_packet();
    gone.mark_cleared(QuoteField::TotalBuyQuantity);
    gone.mark_absent(QuoteField::TotalSellQuantity);
    let s = merge(Some(&s), &gone, session(1));
    assert!(!s.tick().knows(QuoteField::TotalBuyQuantity));
    assert!(s.tick().knows(QuoteField::TotalSellQuantity));
    assert_eq!(s.tick().total_sell_quantity, 800);
    let s = merge(Some(&s), &ltpc_packet(221.0), session(1));
    assert!(!s.tick().knows(QuoteField::TotalBuyQuantity), "stays gone");
    let s = merge(Some(&s), &quote_packet(), session(1));
    assert!(
        s.tick().knows(QuoteField::TotalBuyQuantity),
        "back when sent"
    );
}

/// A packet that marks the change absent (its stream has none) gets none
/// worked out from the price and the close, which an adapter could not
/// otherwise leave out.
#[test]
fn a_change_marked_absent_is_not_worked_out() {
    let s = merge(None, &quote_packet(), session(1));
    assert!(s.tick().knows(QuoteField::Change));
    let mut next = ltpc_packet(221.0);
    next.mark_absent(QuoteField::Change);
    let s = merge(Some(&s), &next, session(1));
    assert!(!s.tick().knows(QuoteField::Change));
}

/// Session `n` of the first connection.
fn session(n: u64) -> crate::websocket::SessionId {
    crate::websocket::SessionId {
        conn: 0,
        session: n,
    }
}
