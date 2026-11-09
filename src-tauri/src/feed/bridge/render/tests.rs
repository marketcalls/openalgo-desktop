use super::*;
use crate::brokers::common::streaming::{
    AbsentFields, NormalizedDepth, NormalizedTick, QuoteField,
};

pub(in crate::feed::bridge) fn full_tick(symbol: &str, exchange: &str) -> NormalizedTick {
    NormalizedTick {
        symbol: symbol.into(),
        exchange: exchange.into(),
        mode: 3,
        ltp: 1167.7,
        open: 1180.1,
        close: 1187.0,
        volume: 10,
        oi: 0,
        last_trade_time_ms: 1_791_000_000_000,
        timestamp_ms: 1_791_000_000_100,
        ..Default::default()
    }
}

/// A full tick reaches every mode, Depth holders included, with no book
/// of its own (lead decision 2026-11-09); its book, sent on its own, reaches
/// Depth holders only.
#[test]
fn full_equity_tick_serves_every_mode_and_depth_serves_depth_holders() {
    let k = InstrumentKey::new("RELIANCE", "NSE");
    let t = full_tick("RELIANCE", "NSE");
    let u = tick_update(k.clone(), &t).expect("a price");
    assert_eq!(u.mode, Mode::Depth);
    assert!(!u.exact_mode && u.depth.is_none());
    assert_eq!(u.quote.as_ref().and_then(|q| q.oi), Some(0));
    let frame = crate::feed::protocol::market_data(&u, Mode::Depth, 5, "hdfcsky");
    let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
    assert!(v["data"].get("depth").is_none(), "{}", v);
    assert_eq!(v["data"]["ltp"], 1167.7);
    let d = NormalizedDepth {
        symbol: "RELIANCE".into(),
        exchange: "NSE".into(),
        ltp: 1167.7,
        buy: vec![Default::default(); 5],
        sell: vec![Default::default(); 5],
        total_buy_quantity: 7,
        total_sell_quantity: 8,
        timestamp_ms: 1_791_000_000_200,
        ..Default::default()
    };
    let u = depth_update(k, &d, Some(&t), AbsentFields::default());
    assert_eq!(u.mode, Mode::Depth);
    assert!(u.exact_mode);
    let q = u.quote.unwrap();
    assert_eq!((q.total_buy_quantity, q.volume), (Some(7), Some(10)));
    assert_eq!(u.ltt, Some(1_791_000_000_000));
    assert_eq!(u.depth.unwrap().buy.len(), 5);
}

/// A broker whose depth message has no LTP of its own (Fyers, web
/// openalgo#2318) leaves it at 0, and the depth frame carries the last
/// traded price of the instrument's tick, as the web's adapter borrows
/// it from the quote feed; before any tick there is none to borrow.
#[test]
fn a_depth_without_its_own_ltp_frames_the_last_traded_price() {
    let k = InstrumentKey::new("CRUDEOIL19OCT26FUT", "MCX");
    let mut t = full_tick("CRUDEOIL19OCT26FUT", "MCX");
    t.ltp = 9665.0;
    let d = NormalizedDepth {
        symbol: "CRUDEOIL19OCT26FUT".into(),
        exchange: "MCX".into(),
        ltp: 0.0,
        buy: vec![Default::default(); 5],
        sell: vec![Default::default(); 5],
        timestamp_ms: 1_791_000_000_200,
        ..Default::default()
    };
    assert_eq!(
        depth_update(k.clone(), &d, Some(&t), AbsentFields::default()).ltp,
        9665.0
    );
    assert_eq!(depth_update(k, &d, None, AbsentFields::default()).ltp, 0.0);
}

#[test]
fn index_full_tick_reaches_every_mode_with_change_fields() {
    let k = InstrumentKey::new("NIFTY", "NSE_INDEX");
    let mut t = full_tick("NIFTY", "NSE_INDEX");
    t.open = 0.0;
    t.change = 1.5;
    let u = tick_update(k, &t).expect("a price");
    assert_eq!(u.mode, Mode::Depth);
    let q = u.quote.unwrap();
    assert_eq!(q.open, None);
    assert_eq!(q.price_change, Some(1.5));
}

/// What a frame built from `t` (and, in Depth, a five-level book after
/// it) carries under `data`, for a subscriber at `mode`.
fn frame_data(t: &NormalizedTick, mode: Mode, broker: &str) -> serde_json::Value {
    let key = InstrumentKey::new(t.symbol.clone(), t.exchange.clone());
    let tick = tick_update(key.clone(), t).expect("a price");
    let update = if mode == Mode::Depth {
        let d = NormalizedDepth {
            symbol: t.symbol.clone(),
            exchange: t.exchange.clone(),
            ltp: t.ltp,
            buy: vec![Default::default(); 5],
            sell: vec![Default::default(); 5],
            timestamp_ms: t.timestamp_ms,
            ..Default::default()
        };
        depth_update(key, &d, Some(t), t.absent)
    } else {
        tick
    };
    let frame = crate::feed::protocol::market_data(&update, mode, 5, broker);
    serde_json::from_str::<serde_json::Value>(&frame).unwrap()["data"].clone()
}

fn keys(data: &serde_json::Value) -> std::collections::BTreeSet<String> {
    data.as_object().unwrap().keys().cloned().collect()
}

/// Fields Sahi's stream never carries (web `sahi_mapping.py`).
const NOT_ON_THE_STREAM: AbsentFields = AbsentFields::of(&[
    QuoteField::Open,
    QuoteField::High,
    QuoteField::Low,
    QuoteField::Close,
    QuoteField::AveragePrice,
    QuoteField::LastQuantity,
    QuoteField::TotalBuyQuantity,
    QuoteField::TotalSellQuantity,
]);

/// Web test/test_sahi_streaming_codec.py::test_depth_tick_fans_out_to_every_mode:
/// a field the stream does not carry is left out of Quote and Depth
/// frames, never sent as 0, and so is every alias of it; what it does
/// carry (volume, OI) stays, a zero included.
#[test]
fn a_field_the_stream_does_not_carry_is_left_out_with_its_aliases() {
    let mut t = full_tick("RELIANCE", "NSE");
    t.volume = 4000;
    t.absent = NOT_ON_THE_STREAM;
    let gone = [
        "open",
        "high",
        "low",
        "close",
        "last_quantity",
        "average_price",
        "total_buy_quantity",
        "total_sell_quantity",
        "last_trade_quantity",
        "avg_trade_price",
        "ltq",
    ];
    for broker in ["sahi", "fyers", "angel", "motilal"] {
        for mode in [Mode::Quote, Mode::Depth] {
            let d = frame_data(&t, mode, broker);
            for k in gone {
                assert!(d.get(k).is_none(), "{} {:?} {}: {}", broker, mode, k, d);
            }
            assert_eq!(d["volume"], 4000, "{} {:?}", broker, mode);
            assert_eq!(d["oi"], 0, "{} {:?}", broker, mode);
            if broker == "sahi" || broker == "angel" {
                assert_eq!(d["ltt"], 1_791_000_000_000_i64, "{}", broker);
            }
        }
    }
    let d = frame_data(&t, Mode::Depth, "sahi");
    assert_eq!(d["depth"]["buy"].as_array().map(Vec::len), Some(5));
}

/// A full tick that marks nothing carries every quote field of its mode,
/// a zero included (an untraded book's 0 volume is a real 0).
#[test]
fn a_tick_that_marks_nothing_still_sends_every_field() {
    let t = full_tick("RELIANCE", "NSE");
    let expected: std::collections::BTreeSet<String> = [
        "symbol",
        "exchange",
        "mode",
        "ltp",
        "ltt",
        "timestamp",
        "volume",
        "last_quantity",
        "average_price",
        "total_buy_quantity",
        "total_sell_quantity",
        "open",
        "high",
        "low",
        "close",
        "oi",
        "open_interest",
    ]
    .map(String::from)
    .into();
    let d = frame_data(&t, Mode::Quote, "zerodha");
    assert_eq!(keys(&d), expected);
    assert_eq!(
        (d["high"].as_f64(), d["average_price"].as_f64()),
        (Some(0.0), Some(0.0))
    );
    let d = frame_data(&t, Mode::Depth, "zerodha");
    let mut with_book = expected.clone();
    with_book.insert("depth".into());
    assert_eq!(keys(&d), with_book);
}

/// Web test/test_sahi_streaming_codec.py::test_index_ltp_without_time_and_derivative_oi:
/// an index tick with no trade time publishes no `ltt` in any mode,
/// rather than the time it was received.
#[test]
fn an_index_tick_without_a_trade_time_has_no_ltt() {
    let mut t = full_tick("NIFTY", "NSE_INDEX");
    t.last_trade_time_ms = 0;
    t.absent = AbsentFields::of(&[QuoteField::LastTradeTime]);
    for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
        let d = frame_data(&t, mode, "sahi");
        assert!(d.get("ltt").is_none(), "{:?}: {}", mode, d);
        assert_eq!(d["timestamp"], 1_791_000_000_100_i64);
    }
    let mut equity = full_tick("SBIN", "NSE");
    equity.last_trade_time_ms = 0;
    let d = frame_data(&equity, Mode::Ltp, "sahi");
    assert_eq!(
        d["ltt"], 1_791_000_000_100_i64,
        "unmarked: the receive time"
    );
}

/// An index whose stream carries no previous close has no change to
/// report: `price_change` and its aliases are left out, not 0.
#[test]
fn an_index_without_a_change_sends_none_of_its_names() {
    let mut t = full_tick("NIFTY", "NSE_INDEX");
    t.close = 0.0;
    t.absent = AbsentFields::default()
        .with(QuoteField::Close)
        .with(QuoteField::Change);
    for mode in [Mode::Quote, Mode::Depth] {
        let d = frame_data(&t, mode, "fyers");
        for k in [
            "close",
            "price_change",
            "price_change_percent",
            "change",
            "change_percent",
        ] {
            assert!(d.get(k).is_none(), "{:?} {}: {}", mode, k, d);
        }
        assert_eq!(d["open"], 1180.1);
    }
}

/// A depth frame leaves out OI the stream does not carry.
#[test]
fn a_depth_frame_leaves_out_oi_the_stream_does_not_carry() {
    let mut t = full_tick("RELIANCE", "NSE");
    t.absent = AbsentFields::of(&[QuoteField::OpenInterest]);
    let d = frame_data(&t, Mode::Depth, "zerodha");
    assert!(
        d.get("oi").is_none() && d.get("open_interest").is_none(),
        "{}",
        d
    );
    assert_eq!(d["close"], 1187.0);
}

/// C03: a packet without a trade price publishes nothing, rather than an
/// LTP of 0; with its price it does.
#[test]
fn a_tick_without_a_price_makes_no_update() {
    let k = InstrumentKey::new("RELIANCE", "NSE");
    let mut t = full_tick("RELIANCE", "NSE");
    t.ltp = 0.0;
    t.mark_absent(QuoteField::Ltp);
    assert!(tick_update(k.clone(), &t).is_none());
    t.ltp = 1167.7;
    t.mark_carried(QuoteField::Ltp);
    assert!(tick_update(k, &t).is_some());
}

/// C05 (audit): a Quote packet serves OI to Quote holders when the adapter
/// says it carries OI, whatever the packet's mode; one that does not leaves
/// it out, and so does the depth frame built on it, instead of the 0 it
/// used to invent.
#[test]
fn quote_oi_goes_by_what_the_packet_carries() {
    let k = InstrumentKey::new("NIFTY28NOV26FUT", "NFO");
    let mut t = full_tick("NIFTY28NOV26FUT", "NFO");
    t.mode = 2;
    t.oi = 98_765;
    let u = tick_update(k.clone(), &t).expect("a price");
    assert_eq!(u.quote.as_ref().and_then(|q| q.oi), None, "a Quote packet");
    let book = NormalizedDepth {
        symbol: t.symbol.clone(),
        exchange: t.exchange.clone(),
        buy: vec![Default::default(); 5],
        sell: vec![Default::default(); 5],
        ..Default::default()
    };
    let d = depth_update(k.clone(), &book, Some(&t), AbsentFields::default());
    assert_eq!(d.quote.as_ref().and_then(|q| q.oi), None, "no invented OI");
    t.mark_carried(QuoteField::OpenInterest);
    let u = tick_update(k.clone(), &t).expect("a price");
    assert_eq!(u.quote.as_ref().and_then(|q| q.oi), Some(98_765));
    let d = depth_update(k, &book, Some(&t), AbsentFields::default());
    assert_eq!(d.quote.and_then(|q| q.oi), Some(98_765));
}

/// C04 (audit): the change goes with an equity or a future as with an
/// index, from the broker's own figure or a previous close above 0, and
/// not at all without either.
#[test]
fn the_change_is_known_for_every_exchange() {
    for (symbol, exchange) in [
        ("SBIN", "NSE"),
        ("NIFTY28NOV26FUT", "NFO"),
        ("NIFTY", "NSE_INDEX"),
    ] {
        let k = InstrumentKey::new(symbol, exchange);
        let mut t = full_tick(symbol, exchange);
        t.derive_change();
        let q = tick_update(k.clone(), &t).and_then(|u| u.quote).unwrap();
        assert_eq!(
            (q.price_change, q.price_change_percent),
            (Some(-19.3), Some(-1.63)),
            "{}",
            symbol
        );
        t.close = 0.0;
        t.change = 0.0;
        t.change_percent = 0.0;
        let q = tick_update(k.clone(), &t).and_then(|u| u.quote).unwrap();
        assert_eq!(q.price_change, None, "{}: no previous close", symbol);
        t.mark_carried(QuoteField::Change);
        let q = tick_update(k, &t).and_then(|u| u.quote).unwrap();
        assert_eq!(q.price_change, Some(0.0), "{}: the broker said 0", symbol);
    }
}

/// C03: a book without totals takes the tick's, one with them wins; a book
/// without order counts sends none rather than 0 (Upstox, a 1.0.6 known
/// limitation); a cleared side stays empty.
#[test]
fn a_book_sends_only_what_it_carries() {
    let k = InstrumentKey::new("RELIANCE", "NSE");
    let mut t = full_tick("RELIANCE", "NSE");
    t.total_buy_quantity = 300;
    t.total_sell_quantity = 400;
    let level = crate::brokers::types::DepthLevel {
        price: 1167.5,
        quantity: 10,
        orders: 0,
    };
    let mut book = NormalizedDepth {
        symbol: "RELIANCE".into(),
        exchange: "NSE".into(),
        buy: vec![level; 5],
        sell: Vec::new(),
        absent: AbsentFields::of(&[
            QuoteField::TotalBuyQuantity,
            QuoteField::TotalSellQuantity,
            QuoteField::Orders,
        ]),
        ..Default::default()
    };
    let u = depth_update(k.clone(), &book, Some(&t), AbsentFields::default());
    let q = u.quote.as_ref().unwrap();
    assert_eq!(
        (q.total_buy_quantity, q.total_sell_quantity),
        (Some(300), Some(400))
    );
    let depth = u.depth.unwrap();
    assert_eq!(depth.buy[0].orders, None);
    assert!(depth.sell.is_empty(), "a cleared side");
    book.absent = AbsentFields::default();
    book.total_buy_quantity = 0;
    let u = depth_update(k, &book, Some(&t), AbsentFields::default());
    assert_eq!(u.quote.unwrap().total_buy_quantity, Some(0), "the book's 0");
    assert_eq!(u.depth.unwrap().buy[0].orders, Some(0));
}

/// Once a book has come in a connection, a full tick carries it to Depth
/// holders (lead decision 2026-11-09, the Samco ruling), also where the
/// broker's Depth holder waits for a book (Definedge); before the first
/// book, on a Quote packet and in a new connection the tick has none.
#[test]
fn a_full_tick_after_a_book_carries_that_book() {
    use crate::brokers::types::DepthLevel as Level;
    use crate::feed::bridge::snapshot::{keep_book, merge, LastBook};
    use std::collections::HashMap;
    let key = InstrumentKey::new("SBIN", "NSE");
    let level = |price| Level {
        price,
        quantity: 10,
        orders: 2,
    };
    let d = NormalizedDepth {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        ltp: 1167.7,
        buy: vec![level(1167.6)],
        sell: vec![level(1167.8)],
        timestamp_ms: 1_791_000_000_050,
        ..Default::default()
    };
    let mut map = HashMap::new();
    map.insert(key.clone(), merge(None, &full_tick("SBIN", "NSE"), 1));
    let first = snapshot_update(key.clone(), &map[&key]).expect("a price");
    assert!(first.depth.is_none(), "no book yet");
    assert!(!crate::feed::protocol::reaches(
        &first,
        Mode::Depth,
        5,
        "definedge"
    ));
    let book = depth_update(
        key.clone(),
        &d,
        Some(map[&key].tick()),
        map[&key].book_lacks(),
    );
    keep_book(
        &mut map,
        &key,
        LastBook::of(&book).expect("a book"),
        1,
        false,
    );

    let mut next = full_tick("SBIN", "NSE");
    next.ltp = 1168.0;
    let s = merge(map.get(&key), &next, 1);
    let u = snapshot_update(key.clone(), &s).expect("a price");
    assert_eq!(u.ltp, 1168.0, "the tick's price, not the book's");
    assert_eq!(u.depth.as_ref().map(|b| b.buy[0].price), Some(1167.6));
    assert!(crate::feed::protocol::reaches(
        &u,
        Mode::Depth,
        5,
        "definedge"
    ));
    let mut quote = next.clone();
    quote.mode = 2;
    let q = snapshot_update(key.clone(), &merge(Some(&s), &quote, 1)).expect("a price");
    assert!(q.depth.is_none(), "a Quote packet reaches no Depth holder");
    let fresh = merge(Some(&s), &next, 2);
    assert!(
        snapshot_update(key.clone(), &fresh)
            .and_then(|u| u.depth)
            .is_none(),
        "a new connection"
    );

    let mut early = HashMap::new();
    keep_book(
        &mut early,
        &key,
        LastBook::of(&book).expect("a book"),
        1,
        false,
    );
    let s = merge(early.get(&key), &next, 1);
    assert!(
        snapshot_update(key, &s).and_then(|u| u.depth).is_some(),
        "a book before the first tick"
    );
}
