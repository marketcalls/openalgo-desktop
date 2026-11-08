//! The price socket's packets on the 1.0.7 carrier (audit C03-C06): each
//! packet at its own mode, stamped with its own time, carrying only what
//! INDstocks sent. The frames these make are pinned end to end by the
//! corpus (`tests/fixtures/feed_corpus/indmoney`).

use super::sub;
use crate::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedMode, Message, NormalizedTick, QuoteField,
};
use crate::brokers::indmoney::streaming::IndmoneyFeed;
use serde_json::{json, Value};

fn ticks(f: &mut IndmoneyFeed, packet: Value) -> Vec<FeedEvent> {
    f.parse(&Message::Text(packet.to_string()))
}

fn tick(f: &mut IndmoneyFeed, packet: Value) -> NormalizedTick {
    match ticks(f, packet).into_iter().next() {
        Some(FeedEvent::Tick(t)) => t,
        other => panic!("no tick: {:?}", other),
    }
}

/// The web's quote dict (indmoney_adapter.py:672-688): the best bid and
/// ask and the OI change as extras, OI beyond the Quote mode, the packet's
/// time as `ltt` and `timestamp`; no last quantity and no book totals,
/// which the stream does not carry (the desktop sent the best bid's and
/// ask's quantities as the totals).
#[test]
fn a_quote_packet_carries_what_the_web_publishes() {
    let mut f = IndmoneyFeed::new("wss://x", "tok");
    f.subscribe_frames(&[sub("NIFTY24NOV26FUT", "NFO", "52345", FeedMode::Quote)]);
    let t = tick(
        &mut f,
        json!({"mode":"quote","instrument":"NFO:52345","timestamp":1794302700777_i64,
            "data":{"ltp":24510.5,"open":24400.5,"high":24550.25,"low":24380.75,"close":24390.5,
                    "volume":125000,"bid_price":24510.25,"bid_qty":75,"ask_price":24511.5,"ask_qty":150,
                    "average_price":24470.25,"oi":13456725,"oi_change":-12500}}),
    );
    assert_eq!(t.mode, 2);
    assert_eq!((t.ltp, t.volume, t.oi), (24510.5, 125000, 13456725));
    assert_eq!(
        (t.last_trade_time_ms, t.timestamp_ms),
        (1794302700777, 1794302700777)
    );
    let x = t.extras;
    assert_eq!(
        (x.bid_price, x.bid_quantity, x.ask_price, x.ask_quantity),
        (Some(24510.25), Some(75), Some(24511.5), Some(150))
    );
    assert_eq!(x.oi_change, Some(-12500));
    assert_eq!((t.change, t.change_percent), (120.0, 0.49));
    assert!(t.knows(QuoteField::OpenInterest));
    for lacked in [
        QuoteField::LastQuantity,
        QuoteField::TotalBuyQuantity,
        QuoteField::TotalSellQuantity,
    ] {
        assert!(!t.knows(lacked), "{:?}", lacked);
    }

    // A zero or a missing field is nothing new: left out of the packet (the
    // bridge keeps the last value, as the web's `get_value` does).
    let t = tick(
        &mut f,
        json!({"mode":"quote","instrument":"NFO:52345","timestamp":1794302701999_i64,
            "data":{"ltp":24512.75,"volume":125150,"oi":0,"oi_change":0}}),
    );
    assert_eq!((t.ltp, t.volume), (24512.75, 125150));
    assert!(!t.knows(QuoteField::OpenInterest) && !t.knows(QuoteField::Open));
    assert_eq!((t.extras.oi_change, t.extras.bid_price), (None, None));
}

/// A packet with no price before the instrument's first one publishes
/// nothing (no invented 0 LTP), and a new connection forgets the last
/// price: the bridge's snapshot starts afresh with it.
#[test]
fn a_new_connection_starts_afresh() {
    let mut f = IndmoneyFeed::new("wss://x", "tok");
    f.subscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Ltp)]);
    let none = json!({"mode":"ltp","instrument":"NSE:2885","timestamp":5,"data":{"ltp":0}});
    assert!(!tick(&mut f, none.clone()).knows(QuoteField::Ltp));
    let priced = json!({"mode":"ltp","instrument":"NSE:2885","timestamp":6,"data":{"ltp":1426.5}});
    assert_eq!(tick(&mut f, priced).ltp, 1426.5);
    let held = tick(&mut f, none.clone());
    assert_eq!((held.ltp, held.timestamp_ms), (1426.5, 5));
    assert!(f.on_connected().is_empty());
    assert_eq!(f.cache_len(), 0);
    assert!(!tick(&mut f, none).knows(QuoteField::Ltp));
}
