//! Web `test/test_groww_quote_feed.py` (openalgo#2377, `e2e31e91f`): a
//! Groww Quote subscriber sees the merged values, never 0 placeholders. A
//! protobuf field equal to its default is not on the wire, so a tick
//! leaves out the OHLC and volume that did not change, and an index tick
//! carries only a value. The ticks are the web test's, as protobuf, go
//! through the real parser and come out as the 8765 frames the bridge
//! makes of them (`feed::testkit`).
//!
//! What has no counterpart: the web caches Groww's `value` (turnover) too;
//! the desktop's frames have one schema and no such key.

use super::*;
use crate::feed::testkit::{keys, Frames};
use crate::feed::Mode;

fn price(ltp: f64, ohlc: [f64; 4], volume: f64, ts: f64) -> Vec<u8> {
    let [open, high, low, close] = ohlc;
    proto::LiveData {
        ltp_data: Some(proto::StocksLivePrice {
            ts_in_millis: ts,
            open,
            high,
            low,
            close,
            volume,
            value: 0.0,
            ltp,
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

fn new_feed() -> GrowwFeed {
    GrowwFeed::new(
        crate::brokers::common::http::client(),
        "tok",
        FeedEndpoints::default(),
    )
}

/// The one Quote frame a price message on the first subscription's
/// subject (sid 1) makes.
fn quote(f: &mut GrowwFeed, payload: &[u8]) -> Value {
    let events = f.parse(&msg_frame(1, payload));
    let frames = Frames::new("groww").at(&events, Mode::Quote);
    match frames.as_slice() {
        [d] => d.clone(),
        other => panic!("one frame expected: {:?}", other),
    }
}

/// Web test_quote_keeps_ohlc_volume_from_earlier_tick: the next tick
/// carries only the price, volume and time; the quote keeps the rest, and
/// carries nothing Groww's feed never sends.
#[test]
fn a_quote_keeps_the_ohlc_of_an_earlier_tick() {
    let mut f = new_feed();
    f.subscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Quote)]);
    quote(
        &mut f,
        &price(
            812.5,
            [805.0, 815.0, 801.0, 803.2],
            1_200_000.0,
            1_730_790_000_000.0,
        ),
    );
    let d = quote(
        &mut f,
        &price(812.9, [0.0; 4], 1_200_500.0, 1_730_790_001_000.0),
    );
    assert_eq!(d["ltp"], 812.9);
    // A double, as the web passes Groww's protobuf double through.
    assert_eq!(d["volume"], 1_200_500.0);
    assert_eq!(
        (
            d["open"].as_f64(),
            d["high"].as_f64(),
            d["low"].as_f64(),
            d["close"].as_f64()
        ),
        (Some(805.0), Some(815.0), Some(801.0), Some(803.2))
    );
    for k in [
        "last_quantity",
        "average_price",
        "total_buy_quantity",
        "total_sell_quantity",
    ] {
        assert!(d.get(k).is_none(), "{}: {}", k, d);
    }
}

/// Web test_index_quote_omits_fields_the_tick_does_not_carry.
#[test]
fn an_index_quote_carries_only_what_the_tick_does() {
    let mut f = new_feed();
    f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "NIFTY", FeedMode::Quote)]);
    let tick = proto::LiveData {
        index_data: Some(proto::LiveIndex {
            ts_in_millis: 1_730_790_000_000.0,
            value: 24_350.75,
        }),
        ..Default::default()
    }
    .encode_to_vec();
    let d = quote(&mut f, &tick);
    assert_eq!(d["ltp"], 24_350.75);
    // `broker`, `topic` and `subscription_mode`: the meta keys the web's
    // Groww adapter adds to every frame (`groww_adapter.py`).
    assert_eq!(
        keys(&d),
        [
            "broker",
            "exchange",
            "ltp",
            "ltt",
            "mode",
            "subscription_mode",
            "symbol",
            "timestamp",
            "topic"
        ]
    );
}

/// Web test_disconnect_clears_the_merged_cache: after a reconnect, a
/// sparse first tick shows only what it carries.
#[test]
fn a_new_connection_clears_the_merged_cache() {
    let mut f = new_feed();
    let sbin = sub("SBIN", "NSE", "3045", FeedMode::Quote);
    f.subscribe_frames(std::slice::from_ref(&sbin));
    quote(
        &mut f,
        &price(812.5, [805.0, 815.0, 0.0, 0.0], 0.0, 1_730_790_000_000.0),
    );
    f.on_connected();
    f.subscribe_frames(std::slice::from_ref(&sbin));
    let d = quote(&mut f, &price(813.0, [0.0; 4], 0.0, 1_730_800_000_000.0));
    assert_eq!(d["ltp"], 813.0);
    assert!(d.get("open").is_none() && d.get("high").is_none(), "{}", d);
}
