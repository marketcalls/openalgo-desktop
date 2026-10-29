//! The app feed's frames: what a subscribe node reads from a broker tick
//! or depth snapshot is the 8765 frame an app receives for it (parity
//! N-stream-09, N-stream-10, D-26).

use super::*;
use crate::brokers::common::streaming::{NormalizedDepth, NormalizedTick};
use crate::brokers::types::DepthLevel;

fn tick(mode: u8) -> FeedEvent {
    FeedEvent::Tick(NormalizedTick {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        mode,
        ltp: 812.5,
        open: 806.0,
        high: 818.0,
        low: 805.25,
        close: 800.0,
        volume: 1_250_000,
        timestamp_ms: 1_793_000_000_000,
        ..Default::default()
    })
}

fn key() -> InstrumentKey {
    InstrumentKey::new("SBIN", "NSE")
}

#[test]
fn a_tick_is_the_apps_frame_at_the_mode_asked() {
    let f = frame(&tick(1), &key(), Mode::Ltp, "zerodha").unwrap();
    assert_eq!(
        (
            f["symbol"].as_str(),
            f["exchange"].as_str(),
            f["mode"].as_u64()
        ),
        (Some("SBIN"), Some("NSE"), Some(1))
    );
    assert_eq!(f["data"]["ltp"], 812.5);
    assert!(
        frame(&tick(1), &key(), Mode::Quote, "zerodha").is_none(),
        "an LTP tick cannot serve Quote"
    );
    let q = frame(&tick(2), &key(), Mode::Quote, "zerodha").unwrap();
    assert_eq!(
        (
            q["mode"].as_u64(),
            q["data"]["open"].as_f64(),
            q["data"]["close"].as_f64()
        ),
        (Some(2), Some(806.0), Some(800.0))
    );
    assert!(frame(
        &tick(2),
        &InstrumentKey::new("TCS", "NSE"),
        Mode::Ltp,
        "zerodha"
    )
    .is_none());
}

#[test]
fn a_depth_snapshot_carries_its_book_as_depth_buy_and_sell() {
    let d = FeedEvent::Depth(NormalizedDepth {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        ltp: 812.5,
        buy: vec![DepthLevel {
            price: 812.4,
            quantity: 150,
            orders: 3,
        }],
        sell: vec![DepthLevel {
            price: 812.6,
            quantity: 120,
            orders: 2,
        }],
        total_buy_quantity: 210,
        total_sell_quantity: 200,
        timestamp_ms: 1_793_000_000_000,
    });
    let f = frame(&d, &key(), Mode::Depth, "zerodha").unwrap();
    assert_eq!(f["mode"].as_u64(), Some(3));
    assert_eq!(f["data"]["depth"]["buy"][0]["price"], 812.4);
    assert_eq!(f["data"]["depth"]["sell"][0]["quantity"], 120);
    assert!(
        frame(&tick(3), &key(), Mode::Depth, "zerodha").is_none(),
        "a tick has no book"
    );
}
