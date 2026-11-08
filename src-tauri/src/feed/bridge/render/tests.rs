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

#[test]
fn full_equity_tick_serves_quote_and_depth_serves_depth_holders() {
    let k = InstrumentKey::new("RELIANCE", "NSE");
    let t = full_tick("RELIANCE", "NSE");
    let u = tick_update(k.clone(), &t);
    assert_eq!(u.mode, Mode::Quote);
    assert!(!u.exact_mode);
    assert_eq!(u.quote.as_ref().and_then(|q| q.oi), Some(0));
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
    let u = depth_update(k, &d, Some(&t));
    assert_eq!(u.mode, Mode::Depth);
    assert!(u.exact_mode);
    let q = u.quote.unwrap();
    assert_eq!((q.total_buy_quantity, q.volume), (7, 10));
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
    assert_eq!(depth_update(k.clone(), &d, Some(&t)).ltp, 9665.0);
    assert_eq!(depth_update(k, &d, None).ltp, 0.0);
}

#[test]
fn index_full_tick_reaches_every_mode_with_change_fields() {
    let k = InstrumentKey::new("NIFTY", "NSE_INDEX");
    let mut t = full_tick("NIFTY", "NSE_INDEX");
    t.open = 0.0;
    t.change = 1.5;
    let u = tick_update(k, &t);
    assert_eq!(u.mode, Mode::Depth);
    let q = u.quote.unwrap();
    assert_eq!(q.open, None);
    assert_eq!(q.price_change, Some(1.5));
}

/// What a frame built from `t` (and, in Depth, a five-level book after
/// it) carries under `data`, for a subscriber at `mode`.
fn frame_data(t: &NormalizedTick, mode: Mode, broker: &str) -> serde_json::Value {
    let key = InstrumentKey::new(t.symbol.clone(), t.exchange.clone());
    let tick = tick_update(key.clone(), t);
    let update = if mode == Mode::Depth && tick.mode < Mode::Depth {
        let d = NormalizedDepth {
            symbol: t.symbol.clone(),
            exchange: t.exchange.clone(),
            ltp: t.ltp,
            buy: vec![Default::default(); 5],
            sell: vec![Default::default(); 5],
            timestamp_ms: t.timestamp_ms,
            ..Default::default()
        };
        depth_update(key, &d, Some(t))
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
            assert_eq!(d["ltt"], 1_791_000_000_000_i64);
        }
    }
    let d = frame_data(&t, Mode::Depth, "sahi");
    assert_eq!(d["depth"]["buy"].as_array().map(Vec::len), Some(5));
}

/// The same tick with nothing marked renders as it always did: every
/// quote field sent, a missing one as 0.
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

/// A depth frame sends OI as 0 when a tick has none, unless the stream
/// does not carry OI at all.
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
