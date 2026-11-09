//! A Depth holder at the deeper socket's levels: until its own first book
//! on that connection it is shown the instrument's shallower book, named by
//! the levels it really has; after that only its own, until the socket
//! reconnects (lead ruling 2026-11-09, `depth-own-level-only`; audit R107-04
//! for the reconnect). A frame with no `depth` key broke clients reading
//! `data['depth']` and left a blank book for up to 30 s on a quiet contract.

use super::packet_tests::{book, bridge, drain, opened, packet, sid, tick};
use super::render::tests::full_tick;
use super::*;
use crate::feed::protocol;
use serde_json::Value;
use std::time::Duration;

/// The levels a frame's book has and the `depth_level` it names.
type Heard = (Option<usize>, Option<u64>);

const FIVE: Heard = (Some(5), Some(5));
const TWENTY: Heard = (Some(20), Some(20));

fn data(u: &MarketUpdate, depth: u8, broker: &str) -> Value {
    let frame = protocol::market_data(u, Mode::Depth, depth, broker);
    serde_json::from_str::<Value>(&frame).unwrap()["data"].clone()
}

fn levels(data: &Value) -> Option<usize> {
    data["depth"]["buy"].as_array().map(Vec::len)
}

/// What a Dhan Depth holder at `depth` is sent for `u`, if anything.
fn heard(u: &MarketUpdate, depth: u8) -> Option<Heard> {
    protocol::reaches(u, Mode::Depth, depth, "dhan").then(|| {
        let d = data(u, depth, "dhan");
        (levels(&d), d["depth_level"].as_u64())
    })
}

fn subscription(symbol: &str, depth: u8) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        token: "1".into(),
        brsymbol: symbol.into(),
        brexchange: "NSE".into(),
        mode: FeedMode::Depth,
        depth,
    }
}

/// `symbol` is also held at `levels`, which stream from the deeper socket.
fn streams_deep(b: &BrokerBridge, symbol: &str, levels: u8) {
    let key = InstrumentKey::new(symbol, "NSE");
    drop(b.subscribe(&key, Mode::Depth, levels));
    b.set_deep_levels(Some(levels));
    let deep = Applied::Deep {
        depth: subscription(symbol, levels),
        quote: subscription(symbol, levels),
    };
    b.state
        .lock()
        .applied
        .insert((key, Mode::Depth, levels), deep);
}

/// A 5 and a 20 holder of one symbol: the 20 holder gets the 5-level book
/// named 5 until its first 20-level book, then only 20 levels however the
/// main feed's packets interleave, and a reconnect of the deeper socket
/// alone starts it over.
#[test]
fn a_deeper_holder_gets_the_shallower_book_until_its_own_comes() {
    let (b, mut rx) = bridge(&["SBIN"]);
    streams_deep(&b, "SBIN", 20);
    let main = sid(0, 1);
    let full = || packet(main, vec![tick("SBIN"), book("SBIN", 5)]);
    let main_book = || packet(main, vec![book("SBIN", 5)]);
    let lone_tick = || packet(main, vec![tick("SBIN")]);
    let deep_book = |from| packet(from, vec![book("SBIN", 20)]);
    let (mut five, mut twenty) = (Vec::new(), Vec::new());
    let mut relay = |p: Packet, deep: bool| {
        b.relay_packet(&p, deep);
        for u in drain(&mut rx) {
            five.extend(heard(&u, 5));
            twenty.extend(heard(&u, 20));
        }
    };

    relay(full(), false);
    relay(main_book(), false);
    relay(lone_tick(), false);
    relay(deep_book(sid(0, 1)), true);
    relay(full(), false);
    relay(main_book(), false);
    relay(lone_tick(), false);
    relay(deep_book(sid(0, 1)), true);
    relay(opened(sid(0, 2)), true);
    relay(full(), false);
    relay(deep_book(sid(0, 2)), true);
    relay(full(), false);

    assert_eq!(
        twenty,
        [
            FIVE, FIVE, FIVE, // before its own first book
            TWENTY, TWENTY, TWENTY, TWENTY, // the main book alone reaches it not
            FIVE, TWENTY, TWENTY, // the deeper socket reconnected
        ]
    );
    assert_eq!(five, [FIVE; 8], "a 5 holder hears only the main feed");
}

/// A 20 holder alone: the main feed streams only its quote, so there is no
/// shallower book to show and nothing is sent before its own.
#[test]
fn a_lone_deeper_holder_waits_for_its_own_book() {
    let (b, mut rx) = bridge(&[]);
    streams_deep(&b, "SBIN", 20);
    let mut quote = full_tick("SBIN", "NSE");
    quote.mode = 2;
    let from = sid(0, 1);
    b.relay_packet(&packet(from, vec![FeedEvent::Tick(quote)]), false);
    assert!(drain(&mut rx).iter().all(|u| heard(u, 20).is_none()));
    b.relay_packet(&packet(from, vec![book("SBIN", 20)]), true);
    let first: Vec<Heard> = drain(&mut rx).iter().filter_map(|u| heard(u, 20)).collect();
    assert_eq!(first, [TWENTY]);
}

/// A subscription that fell back to the main feed gets its book, named 5,
/// also when a 20-level book had come before the silence.
#[test]
fn after_a_fallback_the_deeper_holder_gets_the_main_book() {
    let (b, mut rx) = bridge(&["SBIN"]);
    streams_deep(&b, "SBIN", 20);
    let from = sid(0, 1);
    b.relay_packet(&packet(from, vec![book("SBIN", 20)]), true);
    drain(&mut rx);
    b.state.lock().applied.insert(
        (InstrumentKey::new("SBIN", "NSE"), Mode::Depth, 20),
        Applied::Main(subscription("SBIN", 20)),
    );
    b.relay_packet(&packet(from, vec![tick("SBIN"), book("SBIN", 5)]), false);
    b.relay_packet(&packet(from, vec![book("SBIN", 5)]), false);
    let after: Vec<Heard> = drain(&mut rx).iter().filter_map(|u| heard(u, 20)).collect();
    assert_eq!(after, [FIVE, FIVE]);
}

/// Fyers 50 beside 5: before the first TBT book the 50 holder gets the main
/// feed's 5 levels without the 50-level keys, as after a refused TBT socket;
/// once it has come, 50 levels with those keys, and never the 5 again.
#[test]
fn a_fyers_50_holder_gets_the_5_levels_until_its_first_tbt_book() {
    let (b, mut rx) = bridge(&["SBIN"]);
    streams_deep(&b, "SBIN", 50);
    let from = sid(0, 1);
    let mut latest = |p: Packet, deep: bool| {
        b.relay_packet(&p, deep);
        drain(&mut rx).pop().expect("an update")
    };

    let u = latest(packet(from, vec![tick("SBIN"), book("SBIN", 5)]), false);
    assert!(protocol::reaches(&u, Mode::Depth, 50, "fyers"));
    let before = data(&u, 50, "fyers");
    assert_eq!(levels(&before), Some(5));
    assert!(
        before.get("is_50_depth").is_none() && before.get("depth_levels").is_none(),
        "{}",
        before
    );

    let u = latest(packet(from, vec![book("SBIN", 50)]), true);
    let own = data(&u, 50, "fyers");
    assert_eq!(levels(&own), Some(50));
    assert_eq!(
        (&own["is_50_depth"], &own["depth_levels"]),
        (&true.into(), &50.into())
    );
    assert!(!protocol::reaches(&u, Mode::Depth, 5, "fyers"));

    let u = latest(packet(from, vec![tick("SBIN"), book("SBIN", 5)]), false);
    assert_eq!(levels(&data(&u, 50, "fyers")), Some(50));
    assert_eq!(levels(&data(&u, 5, "fyers")), Some(5));
}

/// A packet's updates are built and sent in one turn, so an update built
/// before an instrument's first deep book cannot follow that book out.
#[test]
fn a_packet_is_built_and_sent_in_one_turn() {
    let (b, mut rx) = bridge(&["SBIN"]);
    let turn = b.relay_turn.lock();
    let relay = {
        let b = b.clone();
        std::thread::spawn(move || b.relay_packet(&packet(sid(0, 1), vec![book("SBIN", 5)]), false))
    };
    std::thread::sleep(Duration::from_millis(50));
    assert!(drain(&mut rx).is_empty(), "sent outside the turn");
    drop(turn);
    relay.join().unwrap();
    assert_eq!(drain(&mut rx).len(), 1);
}
