//! Web `test/test_fivepaisa_quote_feed.py` (openalgo#2396, #2397): 5paisa
//! routes MarketFeedV3 frames to the quote and MarketDepthService frames to
//! the book, matches a frame to its instrument by exchange, segment and
//! token, carries the latest quote in a depth frame, publishes `ChgPcnt` as
//! `percent_change`, and leaves `ltt` out when `TickDt` is not a time. The
//! web test's frames go through the real parser and come out as the 8765
//! frames the bridge makes of them (`feed::testkit`).
//!
//! Where the desktop differs, as for the other brokers: `ltp` is in every
//! frame (0 in a book that comes before its first quote, where the web
//! leaves it out), and an equity carries no change fields, so `ChgPcnt`
//! shows as `price_change_percent` and `percent_change` on an index.

use super::*;
use crate::feed::testkit::Frames;
use crate::feed::Mode;

/// The web test's `FEED`: a MarketFeedV3 frame for ITC on NSE cash.
fn feed_frame() -> Value {
    json!({
        "Exch": "N", "ExchType": "C", "Token": 1660,
        "LastRate": 440.1, "LastQty": 5, "TotalQty": 1200, "High": 447.2,
        "Low": 439.5, "OpenRate": 445, "PClose": 442.9, "AvgRate": 441.3,
        "Time": 35950, "BidQty": 10, "BidRate": 440.0, "OffQty": 12,
        "OffRate": 440.2, "TBidQ": 100, "TOffQ": 200,
        "TickDt": "/Date(1759900055000)/", "ChgPcnt": -0.6321969,
    })
}

/// The web test's `DEPTH`: a MarketDepthService frame for the same scrip.
fn depth_frame() -> Value {
    json!({
        "Exch": "N", "ExchType": "C", "Token": 1660,
        "TBidQ": 86225, "TOffQ": 135100,
        "Details": [
            {"Quantity": 50, "Price": 440.0, "NumberOfOrders": 2, "BbBuySellFlag": 66},
            {"Quantity": 25, "Price": 440.2, "NumberOfOrders": 1, "BbBuySellFlag": 83},
        ],
        "TimeStamp": 0, "Time": "/Date(1640148238196)/",
    })
}

/// `frame` with `changes` laid over it.
fn with(frame: Value, changes: Value) -> Value {
    let mut out = frame;
    if let (Some(o), Some(c)) = (out.as_object_mut(), changes.as_object()) {
        o.extend(c.clone());
    }
    out
}

fn on(symbol: &str, exchange: &str, brexchange: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: "1660".into(),
        brsymbol: symbol.into(),
        brexchange: brexchange.into(),
        mode,
        depth: 5,
    }
}

fn itc(mode: FeedMode) -> FeedSubscription {
    on("ITC", "NSE", "NSE", mode)
}

fn feed_with(subs: &[FeedSubscription]) -> FivepaisaFeed {
    let mut f = FivepaisaFeed::new("wss://x", JWT, "5000");
    f.subscribe_frames(subs);
    f
}

fn send(f: &mut FivepaisaFeed, frame: Value) -> Vec<FeedEvent> {
    f.parse(&Message::Text(frame.to_string()))
}

/// Where each tick went.
fn ticked(events: &[FeedEvent]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            FeedEvent::Tick(t) => Some((t.exchange.clone(), t.symbol.clone())),
            _ => None,
        })
        .collect()
}

/// Web test_depth_payload_carries_latest_quote_fields: a feed frame is no
/// depth update; the book that follows carries the latest quote, the
/// trade time under both names, and its own totals. OI is not on the
/// stream and is left out.
#[test]
fn a_depth_frame_carries_the_latest_quote_fields() {
    let mut f = feed_with(&[itc(FeedMode::Depth)]);
    let mut frames = Frames::new("fivepaisa");
    let quote = send(&mut f, feed_frame());
    assert!(frames.at(&quote, Mode::Depth).is_empty());
    let book = frames.at(&send(&mut f, depth_frame()), Mode::Depth);
    let [d] = book.as_slice() else {
        panic!("one depth frame: {:?}", book)
    };
    assert_eq!(
        (&d["symbol"], &d["exchange"]),
        (&json!("ITC"), &json!("NSE"))
    );
    assert_eq!(d["ltp"], 440.1, "{}", d);
    assert_eq!(d["volume"], 1200, "{}", d);
    assert_eq!(
        (&d["open"], &d["high"], &d["low"], &d["close"]),
        (&json!(445), &json!(447.2), &json!(439.5), &json!(442.9)),
        "{}",
        d
    );
    assert_eq!(d["average_price"], 441.3, "{}", d);
    assert_eq!(d["last_quantity"], 5, "{}", d);
    assert_eq!(d["ltt"], 1_759_900_055_000_i64, "{}", d);
    assert_eq!(d["last_trade_time"], 1_759_900_055_000_i64, "{}", d);
    assert_eq!(d["total_buy_quantity"], 86225, "{}", d);
    assert_eq!(d["total_sell_quantity"], 135_100, "{}", d);
    assert_eq!(d["depth"]["buy"][0]["price"], 440.0, "{}", d);
    assert_eq!(d["depth"]["sell"][0]["price"], 440.2, "{}", d);
    assert!(
        d.get("oi").is_none() && d.get("open_interest").is_none(),
        "{}",
        d
    );
}

/// Web test_depth_before_any_feed_frame_publishes_no_quote_zeros.
#[test]
fn a_depth_before_any_feed_frame_publishes_no_quote_zeros() {
    let mut f = feed_with(&[itc(FeedMode::Depth)]);
    let book = Frames::new("fivepaisa").at(&send(&mut f, depth_frame()), Mode::Depth);
    let [d] = book.as_slice() else {
        panic!("one depth frame: {:?}", book)
    };
    for k in ["volume", "open", "last_quantity", "ltt", "last_trade_time"] {
        assert!(d.get(k).is_none(), "{}: {}", k, d);
    }
    assert_eq!(d["ltp"], 0.0, "the desktop always sends ltp: {}", d);
    assert_eq!(d["total_buy_quantity"], 86225, "{}", d);
    assert!(!d["depth"]["buy"].as_array().unwrap().is_empty());
}

/// Web test_depth_frame_is_not_published_to_quote_subscription.
#[test]
fn a_depth_frame_is_not_published_to_a_quote_subscription() {
    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    let mut events = send(&mut f, feed_frame());
    events.extend(send(&mut f, depth_frame()));
    assert!(!events.iter().any(|e| matches!(e, FeedEvent::Depth(_))));
    let quotes = Frames::new("fivepaisa").at(&events, Mode::Quote);
    let [d] = quotes.as_slice() else {
        panic!("one quote frame: {:?}", quotes)
    };
    assert_eq!(d["volume"], 1200, "{}", d);
    assert_eq!(d["last_quantity"], 5, "{}", d);
    assert_eq!(d["last_trade_quantity"], 5, "{}", d);
    assert_eq!(d["ltt"], 1_759_900_055_000_i64, "{}", d);
    assert_eq!(d["last_trade_time"], 1_759_900_055_000_i64, "{}", d);
}

/// Web test_frames_routed_by_exchange_and_segment: ITC on BSE shares the
/// token; NSE's frame is not its.
#[test]
fn frames_are_routed_by_exchange_and_segment() {
    let mut f = feed_with(&[on("ITC", "BSE", "BSE", FeedMode::Quote)]);
    assert!(send(&mut f, feed_frame()).is_empty());
    let ev = send(&mut f, with(feed_frame(), json!({"Exch": "B"})));
    assert_eq!(ticked(&ev), vec![("BSE".into(), "ITC".into())]);
}

/// Web test_same_exchange_other_segment_is_not_routed: NSE cash (C) and
/// derivatives (D) can share a token; the segment decides.
#[test]
fn the_same_exchange_in_another_segment_is_not_routed() {
    let mut f = feed_with(&[on("ITCFUT", "NFO", "NFO", FeedMode::Quote)]);
    assert!(send(&mut f, feed_frame()).is_empty());
    let ev = send(&mut f, with(feed_frame(), json!({"ExchType": "D"})));
    assert_eq!(ticked(&ev), vec![("NFO".into(), "ITCFUT".into())]);
}

/// Two instruments sharing a token are two subscriptions: unsubscribing
/// one names its own exchange to 5paisa and leaves the other streaming.
/// The desktop kept one entry per bare token, so the second subscribe
/// replaced the first and an unsubscribe dropped (and named) the wrong one.
#[test]
fn two_instruments_sharing_a_token_keep_their_own_subscription() {
    let nse = itc(FeedMode::Quote);
    let bse = on("ITC", "BSE", "BSE", FeedMode::Quote);
    let mut f = feed_with(&[nse.clone(), bse]);
    let frames = f.unsubscribe_frames(&[nse]);
    let sent = text_of(&frames[0]);
    assert_eq!(
        sent["MarketFeedData"],
        json!([{"Exch": "N", "ExchType": "C", "ScripCode": 1660}])
    );
    assert!(send(&mut f, feed_frame()).is_empty(), "NSE is gone");
    let ev = send(&mut f, with(feed_frame(), json!({"Exch": "B"})));
    assert_eq!(ticked(&ev), vec![("BSE".into(), "ITC".into())]);
}

/// Web test_negative_tickdt_omits_ltt: `/Date(-62135596800000)/` is no
/// time, in LTP or Quote; a time with an offset is read.
#[test]
fn a_negative_tick_time_leaves_ltt_out() {
    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    let events = send(
        &mut f,
        with(feed_frame(), json!({"TickDt": "/Date(-62135596800000)/"})),
    );
    let mut frames = Frames::new("fivepaisa");
    for mode in [Mode::Ltp, Mode::Quote] {
        let d = frames.at(&events, mode).pop().unwrap();
        for k in ["ltt", "last_trade_time"] {
            assert!(d.get(k).is_none(), "{:?} {}: {}", mode, k, d);
        }
    }
    let events = send(
        &mut f,
        with(
            feed_frame(),
            json!({"TickDt": "/Date(1759900055000+0530)/"}),
        ),
    );
    let d = frames.at(&events, Mode::Ltp).pop().unwrap();
    assert_eq!(d["ltt"], 1_759_900_055_000_i64, "{}", d);
}

/// Web test_zero_price_holds_last_value_but_quantities_follow_the_feed.
#[test]
fn a_zero_price_holds_its_last_value_but_quantities_follow_the_feed() {
    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    let mut events = send(&mut f, feed_frame());
    events.extend(send(
        &mut f,
        with(
            feed_frame(),
            json!({"LastRate": 0, "LastQty": 0, "TotalQty": 1300}),
        ),
    ));
    let d = Frames::new("fivepaisa")
        .at(&events, Mode::Quote)
        .pop()
        .unwrap();
    assert_eq!(d["ltp"], 440.1, "{}", d);
    assert_eq!(d["last_quantity"], 0, "{}", d);
    assert_eq!(d["volume"], 1300, "{}", d);
}

/// Web test_unsubscribe_releases_scrip_snapshot.
#[test]
fn unsubscribing_releases_the_scrip_snapshot() {
    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    send(&mut f, feed_frame());
    assert_eq!(f.cached(), 1);
    f.unsubscribe_frames(&[itc(FeedMode::Quote)]);
    assert_eq!(f.cached(), 0);
}

/// Web test_frames_for_an_unsubscribed_scrip_keep_no_snapshot.
#[test]
fn frames_for_an_unsubscribed_scrip_keep_no_snapshot() {
    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    assert!(send(&mut f, with(feed_frame(), json!({"Token": 9999}))).is_empty());
    assert!(send(&mut f, with(depth_frame(), json!({"Token": 9999}))).is_empty());
    assert_eq!(f.cached(), 0);
}

/// Web test_a_depth_subscriber_still_keeps_the_feed_snapshot.
#[test]
fn a_depth_subscriber_still_keeps_the_feed_snapshot() {
    let mut f = feed_with(&[itc(FeedMode::Depth)]);
    let mut events = send(&mut f, feed_frame());
    events.extend(send(&mut f, depth_frame()));
    assert_eq!(f.cached(), 1);
    let d = Frames::new("fivepaisa")
        .at(&events, Mode::Depth)
        .pop()
        .unwrap();
    assert_eq!((&d["ltp"], &d["volume"]), (&json!(440.1), &json!(1200)));
}

/// A frame that is neither a feed nor a depth frame (an OI frame) makes
/// nothing on its own (web `_on_data`: no target mode). The desktop held
/// the last prices over it and published a quote with every quantity 0.
#[test]
fn a_frame_that_is_neither_feed_nor_depth_makes_nothing() {
    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    send(&mut f, feed_frame());
    let oi = json!({"Exch": "N", "ExchType": "C", "Token": 1660, "OI": 5000});
    let ev = send(&mut f, oi);
    assert!(ev.is_empty(), "{:?}", ev);
}

/// `ChgPcnt` is 5paisa's own percent change: an index publishes it as the
/// web does, as `percent_change` only, without `price_change` or
/// `price_change_percent`.
#[test]
fn the_change_percent_is_chgpcnt_under_its_web_name() {
    let mut f = feed_with(&[on("NIFTY", "NSE_INDEX", "NSE_INDEX", FeedMode::Quote)]);
    let events = send(&mut f, feed_frame());
    let d = Frames::new("fivepaisa")
        .at(&events, Mode::Quote)
        .pop()
        .unwrap();
    assert_eq!(d["percent_change"], -0.6321969, "{}", d);
    assert!(
        d.get("price_change").is_none() && d.get("price_change_percent").is_none(),
        "{}",
        d
    );
}

/// 5paisa sends 0 for a price it does not have. With nothing held for the
/// instrument a frame without a traded price publishes nothing (no zero
/// LTP), and a zero OHLC or average price is left out, not sent as 0; the
/// quantities are the frame's.
#[test]
fn a_price_5paisa_does_not_have_is_left_out_not_sent_as_zero() {
    let no_trade = json!({"LastRate": 0, "LastQty": 0});
    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    let events = send(&mut f, with(feed_frame(), no_trade));
    assert!(matches!(events.as_slice(), [FeedEvent::Tick(_)]));
    assert!(Frames::new("fivepaisa").at(&events, Mode::Quote).is_empty());

    let unpriced = json!({"OpenRate": 0, "High": 0, "Low": 0, "PClose": 0, "AvgRate": 0});
    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    let events = send(&mut f, with(feed_frame(), unpriced));
    let quotes = Frames::new("fivepaisa").at(&events, Mode::Quote);
    let [d] = quotes.as_slice() else {
        panic!("one quote frame: {:?}", quotes)
    };
    assert_eq!(
        (&d["ltp"], &d["volume"]),
        (&json!(440.1), &json!(1200)),
        "{}",
        d
    );
    for k in ["open", "high", "low", "close", "average_price"] {
        assert!(d.get(k).is_none(), "{}: {}", k, d);
    }
}

/// A new connection holds nothing: the first frame after a reconnect that
/// reports 0 for every price publishes nothing, and the next priced one
/// carries only what the new connection has sent. The web keeps its
/// snapshot across the reconnect and sends the pre-outage prices as
/// current (a web defect).
#[test]
fn prices_held_before_a_reconnect_are_not_sent_after_it() {
    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    let before = Frames::new("fivepaisa").at(&send(&mut f, feed_frame()), Mode::Quote);
    assert_eq!(before.len(), 1);
    f.on_connected();
    let unpriced = json!({"OpenRate": 0, "High": 0, "Low": 0, "PClose": 0, "AvgRate": 0});
    let mut frames = Frames::new("fivepaisa");
    let zero = with(with(feed_frame(), json!({"LastRate": 0})), unpriced.clone());
    assert!(frames.at(&send(&mut f, zero), Mode::Quote).is_empty());
    let priced = with(with(feed_frame(), json!({"LastRate": 440.3})), unpriced);
    let d = frames.at(&send(&mut f, priced), Mode::Quote).pop().unwrap();
    assert_eq!(d["ltp"], 440.3, "{}", d);
    for k in ["open", "high", "low", "close", "average_price"] {
        assert!(d.get(k).is_none(), "{}: {}", k, d);
    }
}

/// A MarketFeedV3 frame is a Quote packet whatever the subscription; an
/// LTP subscriber gets the price and its trade time only.
#[test]
fn an_ltp_frame_is_cut_from_the_quote_packet() {
    let mut f = feed_with(&[itc(FeedMode::Ltp)]);
    let events = send(&mut f, feed_frame());
    assert!(matches!(events.as_slice(), [FeedEvent::Tick(t)] if t.mode == 2));
    let ltp = Frames::new("fivepaisa").at(&events, Mode::Ltp);
    assert_eq!(
        crate::feed::testkit::keys(&ltp[0]),
        [
            "exchange",
            "last_trade_time",
            "ltp",
            "ltt",
            "mode",
            "symbol",
            "timestamp"
        ]
    );
}

/// The web's Quote publishes 5paisa's best bid and ask (`BidRate`,
/// `BidQty`, `OffRate`, `OffQty`); a price of 0 keeps the last one held,
/// and with none held, or the key missing, is left out. A quantity of 0 is
/// sent.
#[test]
fn the_best_bid_and_ask_are_the_frames_own() {
    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    let mut frames = Frames::new("fivepaisa");
    let d = frames
        .at(&send(&mut f, feed_frame()), Mode::Quote)
        .pop()
        .unwrap();
    assert_eq!(
        (
            &d["bid_price"],
            &d["bid_quantity"],
            &d["ask_price"],
            &d["ask_quantity"]
        ),
        (&json!(440.0), &json!(10), &json!(440.2), &json!(12)),
        "{}",
        d
    );
    let thin = with(feed_frame(), json!({"BidRate": 0, "BidQty": 0}));
    let d = frames.at(&send(&mut f, thin), Mode::Quote).pop().unwrap();
    assert_eq!(
        (&d["bid_price"], &d["bid_quantity"]),
        (&json!(440.0), &json!(0)),
        "{}",
        d
    );

    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    let mut first = with(feed_frame(), json!({"BidRate": 0}));
    if let Some(o) = first.as_object_mut() {
        o.remove("OffRate");
        o.remove("OffQty");
    }
    let d = Frames::new("fivepaisa")
        .at(&send(&mut f, first), Mode::Quote)
        .pop()
        .unwrap();
    for k in ["bid_price", "ask_price", "ask_quantity"] {
        assert!(d.get(k).is_none(), "{}: {}", k, d);
    }
    assert_eq!(d["bid_quantity"], 10, "{}", d);
}

/// An empty book is two empty sides, not rows of zeros (web
/// `_extract_depth_data`), and a one-level side is one level; the depth
/// frame's price is the latest quote's.
#[test]
fn a_book_has_the_levels_5paisa_sent() {
    let mut f = feed_with(&[itc(FeedMode::Depth)]);
    let mut events = send(&mut f, feed_frame());
    events.extend(send(&mut f, depth_frame()));
    let empty = json!({"Details": [], "TBidQ": 0, "TOffQ": 0});
    events.extend(send(&mut f, with(depth_frame(), empty)));
    let books = Frames::new("fivepaisa").at(&events, Mode::Depth);
    let [one, cleared] = books.as_slice() else {
        panic!("two depth frames: {:?}", books)
    };
    assert_eq!(
        one["depth"],
        json!({
            "buy": [{"price": 440.0, "quantity": 50, "orders": 2}],
            "sell": [{"price": 440.2, "quantity": 25, "orders": 1}]
        }),
        "{}",
        one
    );
    assert_eq!(
        cleared["depth"],
        json!({"buy": [], "sell": []}),
        "{}",
        cleared
    );
    assert_eq!(cleared["ltp"], 440.1, "{}", cleared);
}

/// The web passes 5paisa's JSON numbers on as they came: the web test's
/// `"OpenRate": 445` goes out as 445, a price with a fraction as a double,
/// and a held price keeps the form 5paisa first wrote it in.
#[test]
fn a_whole_price_goes_out_as_5paisa_wrote_it() {
    let mut f = feed_with(&[itc(FeedMode::Quote)]);
    let mut frames = Frames::new("fivepaisa");
    let d = frames
        .at(&send(&mut f, feed_frame()), Mode::Quote)
        .pop()
        .unwrap();
    assert!(d["open"].is_i64() && d["open"] == 445, "{}", d);
    assert!(d["high"].is_f64() && d["bid_price"].is_f64(), "{}", d);
    let held = with(feed_frame(), json!({"OpenRate": 0, "High": 448}));
    let d = frames.at(&send(&mut f, held), Mode::Quote).pop().unwrap();
    assert!(d["open"].is_i64() && d["open"] == 445, "held: {}", d);
    assert!(d["high"].is_i64() && d["high"] == 448, "{}", d);
}
