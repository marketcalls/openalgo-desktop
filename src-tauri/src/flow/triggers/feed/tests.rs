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

/// N-stream-10, X-350: on the app's own feed manager, the wait listens
/// before it subscribes (a tick that arrives the moment the subscription
/// is taken is read), passes over another instrument's tick, ends at its
/// timeout with nothing, and drops its listener on every path; Flow holds
/// one reference for the instrument however often it waits.
#[tokio::test]
async fn the_wait_reads_the_managers_ticks_and_ends_at_its_timeout() {
    use crate::brokers::common::symbols::SymToken;
    use std::sync::Arc;
    let t = crate::state::testing::build(
        crate::brokers::BrokerRegistry::new(),
        "2026-11-03T04:00:00Z".parse().unwrap(),
    );
    let app = t.ctx.clone();
    app.security
        .install(&crate::security::Unlocked::random().unwrap());
    app.set_broker_session(Some(crate::state::BrokerSession {
        broker_id: "zerodha".into(),
        auth_token: "mock-access-token".into(),
        feed_token: None,
        user_id: "AB1234".into(),
        user_name: None,
        authenticated_at: app.now(),
    }));
    app.symbols.load(vec![SymToken {
        symbol: "SBIN".into(),
        brsymbol: "SBIN-EQ".into(),
        name: "STATE BANK OF INDIA".into(),
        exchange: "NSE".into(),
        brexchange: "NSE".into(),
        token: "3045".into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }]);
    let feed = Arc::new(AppFeed::new(Arc::downgrade(&app)));
    let before = app.websocket.tick_listeners();
    let waiting = {
        let feed = feed.clone();
        tokio::spawn(async move {
            feed.first_frame("SBIN", "NSE", "LTP", Duration::from_secs(5), &|_| true)
                .await
        })
    };
    for _ in 0..500 {
        if app.websocket.instrument_count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(app.websocket.instrument_count(), 1, "subscribed");
    assert_eq!(
        app.websocket.tick_listeners(),
        before + 1,
        "and already listening"
    );
    let FeedEvent::Tick(mut other) = tick(1) else {
        unreachable!()
    };
    other.symbol = "TCS".into();
    app.websocket.emit(Arc::new(FeedEvent::Tick(other)));
    app.websocket.emit(Arc::new(tick(1)));
    let got = waiting.await.unwrap().unwrap().expect("the SBIN tick");
    assert_eq!(
        (got["symbol"].as_str(), got["data"]["ltp"].as_f64()),
        (Some("SBIN"), Some(812.5))
    );
    assert_eq!(
        app.websocket.tick_listeners(),
        before,
        "the listener is dropped"
    );

    let none = feed
        .first_frame("SBIN", "NSE", "LTP", Duration::from_millis(50), &|_| true)
        .await
        .unwrap();
    assert!(none.is_none(), "nothing within the timeout");
    assert_eq!(
        app.websocket.tick_listeners(),
        before,
        "dropped after a timeout too"
    );
    assert_eq!(
        app.websocket.subscriptions().len(),
        1,
        "one reference for Flow"
    );
}
