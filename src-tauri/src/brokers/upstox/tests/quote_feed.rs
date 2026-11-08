//! Web `test/test_upstox_quote_feed.py` (openalgo#2377, `e2e31e91f`):
//! Upstox's incremental ticks are merged per instrument, `close` is the
//! previous close (`ltpc.cp`), OHLC comes from the day candle only, volume
//! is `vtt`, `ltt` is the trade time, and a key holds one mode. The ticks
//! are the web test's, as protobuf (`FULL_TICK` and the rest), go through
//! the real parser and come out as the 8765 frames the bridge makes of
//! them (`feed::testkit`).
//!
//! The web test's option (`NSE_FO|45450`) is not in the recorded master;
//! NIFTY06OCT2624500CE (`NSE_FO|40551`) carries the same ticks. What has
//! no counterpart: an LTP subscribe after Quote never reaches the feed
//! (the manager gives it one subscription per instrument at its highest
//! mode, `websocket::manager`), and a mode change the socket did not take
//! is retried by the manager's reconnect, not reported per call. The
//! desktop's book levels carry `orders` as 0, where the web leaves the key
//! out.

use super::*;
use crate::feed::testkit::Frames;
use crate::feed::Mode;

const KEY: &str = "NSE_FO|40551";
const SYMBOL: &str = "NIFTY06OCT2624500CE";

fn trade(ltp: f64, ltt: i64, ltq: i64) -> proto::Ltpc {
    proto::Ltpc {
        ltp,
        ltt,
        ltq,
        cp: 494.05,
        iep: None,
    }
}

fn candle(interval: &str, ohlc: [f64; 4], vol: i64, ts: i64) -> proto::Ohlc {
    let [open, high, low, close] = ohlc;
    proto::Ohlc {
        interval: interval.into(),
        open,
        high,
        low,
        close,
        vol,
        ts,
    }
}

fn level(bid_q: i64, bid_p: f64, ask_q: i64, ask_p: f64) -> proto::Quote {
    proto::Quote {
        bid_q,
        bid_p,
        ask_q,
        ask_p,
    }
}

/// The web test's `FULL_TICK`: the day candle's close is about the LTP,
/// the previous close is `cp`.
fn full_tick() -> proto::MarketFullFeed {
    proto::MarketFullFeed {
        ltpc: Some(trade(219.3, 1_740_729_552_723, 75)),
        market_level: Some(proto::MarketLevel {
            bid_ask_quote: vec![
                level(150, 219.25, 75, 219.4),
                level(300, 219.2, 225, 219.45),
            ],
        }),
        market_ohlc: Some(proto::MarketOhlc {
            ohlc: vec![
                candle(
                    "1d",
                    [480.0, 495.0, 210.0, 219.3],
                    1_234_500,
                    1_740_681_000_000,
                ),
                candle("I1", [219.0, 219.5, 218.9, 219.3], 4500, 1_740_729_480_000),
            ],
        }),
        atp: 301.5,
        vtt: 1_234_575,
        oi: 98_765.0,
        tbq: 50_000.0,
        tsq: 60_000.0,
        ..Default::default()
    }
}

fn subscribed(mode: FeedMode) -> UpstoxFeed {
    let mut f = feed();
    f.subscribe_frames(&[sub(SYMBOL, "NFO", mode)]);
    f
}

fn tick(f: &mut UpstoxFeed, m: proto::MarketFullFeed) -> Vec<FeedEvent> {
    let feed = proto::Feed {
        feed_union: Some(FeedUnion::FullFeed(proto::FullFeed {
            full_feed_union: Some(FullFeedUnion::MarketFf(m)),
        })),
        request_mode: proto::RequestMode::FullD5 as i32,
    };
    f.parse(&Message::Binary(response(vec![(KEY, feed)])))
}

/// The one frame a subscriber at `mode` receives for `events`.
fn the_frame(events: &[FeedEvent], mode: Mode) -> Value {
    let frames = Frames::new("upstox").at(events, mode);
    match frames.as_slice() {
        [d] => d.clone(),
        other => panic!("one frame expected: {:?}", other),
    }
}

/// Web test_quote_close_is_previous_close_and_ltt_from_ltpc.
#[test]
fn the_quote_close_is_the_previous_close_and_ltt_the_trade_time() {
    let mut f = subscribed(FeedMode::Quote);
    let d = the_frame(&tick(&mut f, full_tick()), Mode::Quote);
    assert_eq!(d["close"], 494.05, "ltpc.cp, not the day candle's close");
    assert_eq!(
        (d["open"].as_f64(), d["high"].as_f64(), d["low"].as_f64()),
        (Some(480.0), Some(495.0), Some(210.0))
    );
    assert_eq!(d["volume"], 1_234_575, "vtt, the volume traded today");
    assert_eq!(d["ltt"], 1_740_729_552_723_i64);
    assert_eq!(d["last_trade_time"], 1_740_729_552_723_i64);
    assert_eq!(
        (
            d["last_quantity"].as_i64(),
            d["last_trade_quantity"].as_i64()
        ),
        (Some(75), Some(75))
    );
}

/// Web test_intraday_candle_never_stands_in_for_the_day_candle.
#[test]
fn an_intraday_candle_never_stands_in_for_the_day_candle() {
    let mut f = subscribed(FeedMode::Quote);
    let m = proto::MarketFullFeed {
        ltpc: Some(trade(219.3, 1_740_729_552_723, 0)),
        market_ohlc: Some(proto::MarketOhlc {
            ohlc: vec![candle(
                "I1",
                [219.0, 219.5, 218.9, 219.3],
                4500,
                1_740_729_480_000,
            )],
        }),
        ..Default::default()
    };
    let d = the_frame(&tick(&mut f, m), Mode::Quote);
    for k in ["open", "high", "low", "volume"] {
        assert!(d.get(k).is_none(), "{}: {}", k, d);
    }
    assert_eq!(d["close"], 494.05);
}

/// Web test_incremental_tick_keeps_merged_quote_fields: the next packet
/// carries only a new trade.
#[test]
fn an_incremental_tick_keeps_the_merged_quote_fields() {
    let mut f = subscribed(FeedMode::Quote);
    tick(&mut f, full_tick());
    let next = proto::MarketFullFeed {
        ltpc: Some(trade(219.5, 1_740_729_553_000, 25)),
        ..Default::default()
    };
    let d = the_frame(&tick(&mut f, next), Mode::Quote);
    assert_eq!(d["ltp"], 219.5);
    assert_eq!(d["open"], 480.0);
    assert_eq!(d["average_price"], 301.5);
    assert_eq!(
        (
            d["total_buy_quantity"].as_i64(),
            d["total_sell_quantity"].as_i64()
        ),
        (Some(50_000), Some(60_000))
    );
    assert_eq!(d["volume"], 1_234_575);
    assert_eq!(d["ltt"], 1_740_729_553_000_i64);
}

/// Web test_depth_payload_carries_quote_fields: a packet that only
/// updates the book.
#[test]
fn a_depth_frame_carries_the_quote_fields() {
    let mut f = subscribed(FeedMode::Depth);
    tick(&mut f, full_tick());
    let book_only = proto::MarketFullFeed {
        market_level: Some(proto::MarketLevel {
            bid_ask_quote: vec![level(10, 219.3, 20, 219.5)],
        }),
        ..Default::default()
    };
    let d = the_frame(&tick(&mut f, book_only), Mode::Depth);
    assert_eq!(d["ltp"], 219.3);
    assert_eq!(d["close"], 494.05);
    assert_eq!(d["open"], 480.0);
    assert_eq!(d["volume"], 1_234_575);
    assert_eq!(d["average_price"], 301.5);
    assert_eq!(d["total_buy_quantity"], 50_000);
    assert_eq!(
        d["depth"]["buy"][0],
        json!({"price": 219.3, "quantity": 10, "orders": 0})
    );
    assert_eq!(
        d["depth"]["sell"][0],
        json!({"price": 219.5, "quantity": 20, "orders": 0})
    );
}

/// Web test_unsubscribing_one_mode_keeps_the_key_for_the_other: the Quote
/// subscriber leaves, the LTP one stays. Upstox is sent one `sub` in
/// `ltpc` and no `unsub`, and the merged price stays; the last one leaving
/// unsubscribes the key.
#[test]
fn a_mode_change_keeps_the_key_and_its_quote() {
    let mut f = subscribed(FeedMode::Quote);
    tick(&mut f, full_tick());
    let frames = f.mode_change_frames(
        &sub(SYMBOL, "NFO", FeedMode::Quote),
        &sub(SYMBOL, "NFO", FeedMode::Ltp),
    );
    let sent: Vec<Value> = frames.iter().map(frame_json).collect();
    assert_eq!(sent.len(), 1, "{:?}", sent);
    assert_eq!(
        (&sent[0]["method"], &sent[0]["data"]["mode"]),
        (&json!("sub"), &json!("ltpc"))
    );
    assert_eq!(sent[0]["data"]["instrumentKeys"], json!([KEY]));
    let book_only = proto::MarketFullFeed {
        market_level: Some(proto::MarketLevel {
            bid_ask_quote: vec![level(10, 219.3, 20, 219.5)],
        }),
        ..Default::default()
    };
    let d = the_frame(&tick(&mut f, book_only), Mode::Ltp);
    assert_eq!(d["ltp"], 219.3);
    let un = f.unsubscribe_frames(&[sub(SYMBOL, "NFO", FeedMode::Ltp)]);
    assert_eq!(frame_json(&un[0])["method"], "unsub");
}

/// Web test_client_moves_a_key_between_modes: up to `full` and back down
/// to `ltpc`, each move is sent, never deduplicated.
#[test]
fn a_key_moves_between_modes() {
    let mut f = feed();
    let ltp = sub(SYMBOL, "NFO", FeedMode::Ltp);
    let quote = sub(SYMBOL, "NFO", FeedMode::Quote);
    let mut sent: Vec<Value> = f
        .subscribe_frames(std::slice::from_ref(&ltp))
        .iter()
        .map(frame_json)
        .collect();
    sent.extend(f.mode_change_frames(&ltp, &quote).iter().map(frame_json));
    sent.extend(f.mode_change_frames(&quote, &ltp).iter().map(frame_json));
    let moves: Vec<(Value, Value)> = sent
        .iter()
        .map(|v| (v["method"].clone(), v["data"]["mode"].clone()))
        .collect();
    assert_eq!(
        moves,
        [
            (json!("sub"), json!("ltpc")),
            (json!("sub"), json!("full")),
            (json!("sub"), json!("ltpc")),
        ]
    );
}

/// Web test_an_emptied_mode_does_not_trigger_the_combined_cap: once the
/// only `full` key has moved back to `ltpc`, the connection is one mode
/// again, so new `ltpc` keys are capped by the `ltpc` limit alone.
#[test]
fn an_emptied_mode_does_not_count_towards_the_combined_cap() {
    let mut f = feed();
    let ltp = sub(SYMBOL, "NFO", FeedMode::Ltp);
    let quote = sub(SYMBOL, "NFO", FeedMode::Quote);
    f.subscribe_frames(std::slice::from_ref(&ltp));
    f.mode_change_frames(&ltp, &quote);
    f.mode_change_frames(&quote, &ltp);
    let many: Vec<FeedSubscription> = (0..1510)
        .map(|i| FeedSubscription {
            token: format!("NSE_EQ|K{}", i),
            brexchange: "NSE_EQ".into(),
            ..sub("RELIANCE", "NSE", FeedMode::Ltp)
        })
        .collect();
    let admitted: usize = f
        .subscribe_frames(&many)
        .iter()
        .map(|m| {
            frame_json(m)["data"]["instrumentKeys"]
                .as_array()
                .map_or(0, Vec::len)
        })
        .sum();
    assert_eq!(admitted, 1510);
}
