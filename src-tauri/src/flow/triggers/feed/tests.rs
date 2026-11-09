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
        ..Default::default()
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

// ---------------------------------------------------------------------------
// A book that carries no price (review 2, P1)
// ---------------------------------------------------------------------------

use crate::brokers::common::streaming::{AbsentFields, QuoteField};
use std::sync::Arc;

/// An app signed in to `broker`, its master holding `symbol`, and Flow's
/// feed on it.
fn signed_in(
    broker: &str,
    symbol: &str,
    exchange: &str,
    token: &str,
) -> (crate::state::testing::TestCtx, Arc<AppFeed>) {
    use crate::brokers::common::symbols::SymToken;
    let t = crate::state::testing::build(
        crate::brokers::BrokerRegistry::new(),
        "2026-11-03T04:00:00Z".parse().unwrap(),
    );
    let app = t.ctx.clone();
    app.security
        .install(&crate::security::Unlocked::random().unwrap());
    app.set_broker_session(Some(crate::state::BrokerSession {
        broker_id: broker.into(),
        auth_token: "mock-access-token".into(),
        feed_token: None,
        user_id: "AB1234".into(),
        user_name: None,
        authenticated_at: app.now(),
    }));
    app.symbols.load(vec![SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }]);
    let feed = Arc::new(AppFeed::new(Arc::downgrade(&app)));
    (t, feed)
}

/// What one wait returns while `events` arrive, 50 ms apart, once it has
/// subscribed; with how long it took. The timeout is long, so a wait that
/// fell through to it shows as a very slow answer.
async fn wait_during(
    t: &crate::state::testing::TestCtx,
    feed: &Arc<AppFeed>,
    (symbol, exchange, mode): (&str, &str, &str),
    events: Vec<FeedEvent>,
) -> (Option<Value>, Duration) {
    let started = std::time::Instant::now();
    let waiting = {
        let feed = feed.clone();
        let (symbol, exchange, mode) = (symbol.to_string(), exchange.to_string(), mode.to_string());
        tokio::spawn(async move {
            feed.first_frame(&symbol, &exchange, &mode, Duration::from_secs(30), &|_| {
                true
            })
            .await
        })
    };
    for _ in 0..500 {
        if t.ctx.websocket.instrument_count() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    for (i, event) in events.into_iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        t.ctx.websocket.emit(Arc::new(event));
    }
    let got = waiting.await.unwrap().unwrap();
    (got, started.elapsed())
}

/// A book with no price of its own, as Pocketful's snapquote, Samco's
/// quote2, Dhan's 20-level and Fyers' depth packets arrive.
fn book_without_a_price() -> FeedEvent {
    FeedEvent::Depth(NormalizedDepth {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
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
        absent: AbsentFields::of(&[QuoteField::Ltp]),
        ..Default::default()
    })
}

const SBIN_DEPTH: (&str, &str, &str) = ("SBIN", "NSE", "Depth");
const SOON: Duration = Duration::from_secs(15);

#[tokio::test]
async fn a_book_takes_the_price_that_came_before_it() {
    let (t, feed) = signed_in("pocketful", "SBIN", "NSE", "3045");
    let (got, took) =
        wait_during(&t, &feed, SBIN_DEPTH, vec![tick(2), book_without_a_price()]).await;
    let f = got.expect("the book");
    assert_eq!(f["data"]["ltp"], 812.5, "{}", f);
    assert_eq!(f["data"]["depth"]["buy"][0]["price"], 812.4, "{}", f);
    assert!(took < SOON, "{:?}", took);
}

#[tokio::test]
async fn a_book_waits_for_the_price_that_comes_after_it() {
    let (t, feed) = signed_in("pocketful", "SBIN", "NSE", "3045");
    let (got, took) =
        wait_during(&t, &feed, SBIN_DEPTH, vec![book_without_a_price(), tick(2)]).await;
    let f = got.expect("the book");
    assert_eq!(f["data"]["ltp"], 812.5, "{}", f);
    assert_eq!(f["data"]["depth"]["sell"][0]["price"], 812.6, "{}", f);
    assert!(took < SOON, "{:?}", took);
}

/// A quiet contract: no priced tick comes. The book is held 2 s, then goes
/// out with ltp 0 (the 1.0.6 rule for a book before any price), rather
/// than the node waiting out its timeout.
#[tokio::test]
async fn a_book_with_no_price_at_all_goes_out_after_two_seconds() {
    let (t, feed) = signed_in("pocketful", "SBIN", "NSE", "3045");
    let (got, took) = wait_during(&t, &feed, SBIN_DEPTH, vec![book_without_a_price()]).await;
    let f = got.expect("the book");
    assert_eq!(f["data"]["ltp"], 0.0, "{}", f);
    assert_eq!(f["data"]["depth"]["buy"][0]["price"], 812.4, "{}", f);
    assert!(took >= Duration::from_millis(1900), "held: {:?}", took);
    assert!(took < SOON, "{:?}", took);
}

/// A book that has its own price is not held.
#[tokio::test]
async fn a_book_with_its_own_price_goes_out_at_once() {
    let (t, feed) = signed_in("zerodha", "SBIN", "NSE", "3045");
    let FeedEvent::Depth(mut book) = book_without_a_price() else {
        unreachable!()
    };
    book.absent = AbsentFields::default();
    book.ltp = 812.55;
    let (got, took) = wait_during(&t, &feed, SBIN_DEPTH, vec![FeedEvent::Depth(book)]).await;
    assert_eq!(got.expect("the book")["data"]["ltp"], 812.55);
    assert!(took < Duration::from_millis(1900), "{:?}", took);
}

/// No LTP or Quote from a packet with no price (lead decision 2026-11-09):
/// a book does not answer a lower wait with ltp 0; the next tick does.
#[tokio::test]
async fn a_book_with_no_price_does_not_answer_an_ltp_wait() {
    let (t, feed) = signed_in("pocketful", "SBIN", "NSE", "3045");
    let (got, _) = wait_during(
        &t,
        &feed,
        ("SBIN", "NSE", "LTP"),
        vec![book_without_a_price(), tick(1)],
    )
    .await;
    assert_eq!(got.expect("the tick")["data"]["ltp"], 812.5);
}

/// Another broker whose book has no price: Samco's raw quote2 and quote
/// frames through its real decoder (the corpus case's frames, book first).
#[tokio::test]
async fn a_samco_book_waits_for_samcos_quote() {
    use crate::brokers::common::streaming::{BrokerFeed, FeedSubscription, Message};
    use crate::brokers::samco::streaming::SamcoFeed;
    const BOOK: &str = include_str!(
        "../../../../tests/fixtures/feed_corpus/samco/depth_book_first_future/frames/book.json"
    );
    const QUOTE: &str = include_str!(
        "../../../../tests/fixtures/feed_corpus/samco/depth_book_first_future/frames/quote.json"
    );
    let symbol = "NIFTY24NOV26FUT";
    let mut samco = SamcoFeed::new("ws://x", "t", Default::default());
    samco.subscribe_frames(&[FeedSubscription {
        symbol: symbol.into(),
        exchange: "NFO".into(),
        token: "35001".into(),
        brsymbol: symbol.into(),
        brexchange: "NFO".into(),
        mode: FeedMode::Depth,
        depth: 5,
    }]);
    let mut events = samco.parse(&Message::Text(BOOK.trim().into()));
    events.extend(samco.parse(&Message::Text(QUOTE.trim().into())));
    assert!(events.iter().any(|e| matches!(e, FeedEvent::Depth(_))));

    let (t, feed) = signed_in("samco", symbol, "NFO", "35001");
    let (got, took) = wait_during(&t, &feed, (symbol, "NFO", "Depth"), events).await;
    let f = got.expect("the book");
    assert_eq!(f["data"]["ltp"], 24610.4, "{}", f);
    assert_eq!(f["data"]["depth"]["buy"][0]["price"], 24610.0, "{}", f);
    assert!(took < SOON, "{:?}", took);
}
