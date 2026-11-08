//! Web `test/test_samco_quote_feed.py` (openalgo#2396, #2397): Samco
//! publishes the exchange's last trade time as `ltt` and `last_trade_time`,
//! never the local clock; the average price and the book totals when Samco
//! sent them, a 0 included, and nothing when it did not; and a tick only to
//! the subscription of its own segment. Frames go through the real parser
//! and come out as the 8765 frames the bridge makes of them
//! (`feed::testkit`), with the web test's values.
//!
//! The web test feeds `_normalize_market_data` flat dicts; the desktop's
//! parser reads Samco's wire shapes (a flat `quote` frame, a wrapped
//! `quote2` one), so the same fields arrive that way. The cross-segment
//! cases pin behaviour the desktop already had: its subscriptions are keyed
//! by `scripCode_segment`, and it never matched a scrip code alone.

use super::*;
use crate::feed::testkit::Frames;
use crate::feed::Mode;
use serde_json::Map;

/// 2019-10-01 18:47:03 IST, 13:17:03 UTC.
const DOC_LTT_MS: i64 = 1_569_935_823_000;

/// The sample quote frame of Samco's docs (13-streaming-data.md).
fn doc_quote() -> Map<String, Value> {
    json!({
        "aPr": "44561.00", "bPr": "44554.00", "aSz": "1", "bSz": "1",
        "sym": "2885_NSE", "avgPr": "44371.85", "c": "44119.00",
        "h": "44786.00", "l": "43969.00", "o": "44105.00", "oI": "361290",
        "ch": "435.00", "chPer": "0.99",
        "lTrdT": "01 Oct 2019, 06:47:03 PM", "ltp": "44554.00", "ltq": "1",
        "ltt": "01 Oct 2019, 06:47:03 PM", "lttUTC": "01 Oct 2019, 01:17:03 PM",
        "tBQ": "973", "tSQ": "610", "vol": "782763833.41",
        "streaming_type": "quote",
    })
    .as_object()
    .cloned()
    .unwrap()
}

fn without(mut frame: Map<String, Value>, fields: &[&str]) -> Map<String, Value> {
    for f in fields {
        frame.remove(*f);
    }
    frame
}

fn quote(frame: Map<String, Value>) -> Message {
    Message::Text(Value::Object(frame).to_string())
}

/// A bare `quote` frame naming `sym`.
fn tick(sym: &str) -> Message {
    Message::Text(json!({"sym": sym, "ltp": "1", "streaming_type": "quote"}).to_string())
}

/// A `quote2` frame as Samco wraps it.
fn depth_frame(data: Value) -> Message {
    Message::Text(format!(
        "{}\n",
        json!({"response": {"streaming_type": "quote2", "data": data}})
    ))
}

/// RELIANCE (2885 on NSE) held at Depth, its book (without totals)
/// already sent, so every mode's frame is made.
fn reliance() -> SamcoFeed {
    let (mut f, _) = feed();
    f.subscribe_frames(&[sub("RELIANCE", "NSE", "NSE", "2885", FeedMode::Depth)]);
    f.parse(&a_book("2885_NSE"));
    f
}

/// A one-level book without totals for `symbol`.
fn a_book(symbol: &str) -> Message {
    depth_frame(json!({
        "symbol": symbol,
        "bidValues": [{"price": "44554", "qty": "1", "no": "1"}],
        "askValues": [{"price": "44561", "qty": "1", "no": "1"}],
    }))
}

fn quote_sub(symbol: &str, exchange: &str, token: &str) -> FeedSubscription {
    sub(symbol, exchange, exchange, token, FeedMode::Quote)
}

/// Where each event of a frame went.
fn routed(events: &[FeedEvent]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            FeedEvent::Tick(t) => Some((t.exchange.clone(), t.symbol.clone())),
            _ => None,
        })
        .collect()
}

/// Web test_exchange_trade_time_is_published_as_ltt: the exchange's trade
/// time, in every mode, under both names.
#[test]
fn the_exchange_trade_time_is_published_as_ltt() {
    let mut f = reliance();
    let events = f.parse(&quote(doc_quote()));
    let mut frames = Frames::new("samco");
    for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
        let d = frames.at(&events, mode).pop().unwrap();
        assert_eq!(d["ltt"], DOC_LTT_MS, "{:?}: {}", mode, d);
        assert_eq!(d["last_trade_time"], DOC_LTT_MS, "{:?}: {}", mode, d);
    }
}

/// Web test_utc_trade_time_used_when_ist_fields_absent.
#[test]
fn the_utc_trade_time_is_used_when_the_ist_fields_are_absent() {
    let mut f = reliance();
    let events = f.parse(&quote(without(doc_quote(), &["lTrdT", "ltt"])));
    let d = Frames::new("samco").at(&events, Mode::Quote).pop().unwrap();
    assert_eq!(d["ltt"], DOC_LTT_MS, "{}", d);
}

/// Web test_ltt_omitted_when_samco_sends_no_trade_time: no trade time, no
/// `ltt` and no `last_trade_time`, rather than the time it was received.
#[test]
fn ltt_is_left_out_when_samco_sends_no_trade_time() {
    let mut f = reliance();
    let frame = without(doc_quote(), &["lTrdT", "ltt", "lttUTC"]);
    let events = f.parse(&quote(frame));
    let mut frames = Frames::new("samco");
    for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
        let d = frames.at(&events, mode).pop().unwrap();
        for k in ["ltt", "last_trade_time"] {
            assert!(d.get(k).is_none(), "{:?} {}: {}", mode, k, d);
        }
        assert!(d["timestamp"].is_i64(), "{}", d);
    }
}

/// Web test_average_price_and_totals_published_in_quote_and_depth.
#[test]
fn the_average_price_and_totals_are_published_in_quote_and_depth() {
    let mut f = reliance();
    let events = f.parse(&quote(doc_quote()));
    let mut frames = Frames::new("samco");
    for mode in [Mode::Quote, Mode::Depth] {
        let d = frames.at(&events, mode).pop().unwrap();
        assert_eq!(d["average_price"], 44371.85, "{:?}: {}", mode, d);
        assert_eq!(d["total_buy_quantity"], 973, "{:?}: {}", mode, d);
        assert_eq!(d["total_sell_quantity"], 610, "{:?}: {}", mode, d);
    }
    let d = frames.at(&events, Mode::Quote).pop().unwrap();
    assert_eq!(
        (&d["last_quantity"], &d["last_trade_quantity"]),
        (&json!(1), &json!(1))
    );
}

/// Web test_depth_frame_totals_tbq_taq_published: a `quote2` frame's
/// totals, over the quote stream's average price and trade time.
#[test]
fn a_depth_frame_publishes_its_tbq_and_taq_totals() {
    let mut f = reliance();
    let mut events = f.parse(&quote(doc_quote()));
    events.extend(f.parse(&depth_frame(json!({
        "symbol": "2885_NSE",
        "tbq": "1778050",
        "taq": "5471990",
        "bidValues": [{"price": "44554", "qty": "1", "no": "1"}],
        "askValues": [{"price": "44561", "qty": "1", "no": "1"}],
    }))));
    let d = Frames::new("samco").at(&events, Mode::Depth).pop().unwrap();
    assert_eq!(d["total_buy_quantity"], 1_778_050, "{}", d);
    assert_eq!(d["total_sell_quantity"], 5_471_990, "{}", d);
    assert_eq!(d["average_price"], 44371.85, "{}", d);
    assert_eq!(d["ltt"], DOC_LTT_MS, "{}", d);
    assert_eq!(d["depth"]["buy"][0]["price"], 44554.0, "{}", d);
}

/// Web test_fields_not_sent_are_left_out: a stream that has not reported
/// the average price, the totals or a trade time publishes none of them,
/// in Quote or Depth, rather than 0 or the receive time. Nor the volume,
/// OHLC or last quantity it has not reported (the web sends those as 0, a
/// value it never received; the desktop did too).
#[test]
fn fields_samco_did_not_send_are_left_out() {
    let (mut f, _) = feed();
    f.subscribe_frames(&[sub("ONE", "NSE", "NSE", "1", FeedMode::Depth)]);
    f.parse(&a_book("1_NSE"));
    let events = f.parse(&Message::Text(
        json!({"sym": "1_NSE", "ltp": "10", "streaming_type": "quote"}).to_string(),
    ));
    let mut frames = Frames::new("samco");
    for mode in [Mode::Quote, Mode::Depth] {
        let d = frames.at(&events, mode).pop().unwrap();
        for k in [
            "average_price",
            "total_buy_quantity",
            "total_sell_quantity",
            "ltt",
            "last_trade_time",
            "volume",
            "open",
            "high",
            "low",
            "close",
            "last_quantity",
            "last_trade_quantity",
        ] {
            assert!(d.get(k).is_none(), "{:?} {}: {}", mode, k, d);
        }
        assert_eq!(d["ltp"], 10.0, "{}", d);
    }
}

/// Web test_a_zero_depth_total_is_kept_not_replaced_by_the_quote_total:
/// `quote2` reports totals of 0 (numbers) while the merged state also
/// holds the quote stream's `tBQ`/`tSQ`; 0 is a real total.
#[test]
fn a_zero_depth_total_is_kept_not_replaced_by_the_quote_total() {
    let mut f = reliance();
    let mut events = f.parse(&quote(doc_quote()));
    events.extend(f.parse(&depth_frame(json!({
        "symbol": "2885_NSE", "tbq": 0, "taq": 0, "bidValues": [], "askValues": [],
    }))));
    let d = Frames::new("samco").at(&events, Mode::Depth).pop().unwrap();
    assert_eq!(d["total_buy_quantity"], 0, "{}", d);
    assert_eq!(d["total_sell_quantity"], 0, "{}", d);
    // A Quote holder hears of them with Samco's next quote, as on the web
    // (the book frame itself reaches Depth holders only).
    events.extend(f.parse(&tick("2885_NSE")));
    let d = Frames::new("samco").at(&events, Mode::Quote).pop().unwrap();
    assert_eq!(
        d["total_buy_quantity"], 0,
        "the quote reads the same: {}",
        d
    );
}

/// Web `samco_adapter._on_data`: a book (quote2) frame reaches Depth
/// subscribers only. One that comes before Samco's first quote made a
/// tick of its own, and a Quote holder of the same instrument was shown an
/// LTP of 0 and an OHLC of zeros Samco never sent.
#[test]
fn a_book_frame_reaches_only_the_depth_holders() {
    let mut f = reliance();
    let events = f.parse(&depth_frame(json!({
        "symbol": "2885_NSE", "tbq": "5", "taq": "6",
        "bidValues": [{"price": "44554", "qty": "1", "no": "1"}], "askValues": [],
    })));
    assert!(
        events.iter().all(|e| matches!(e, FeedEvent::Depth(_))),
        "{:?}",
        events
    );
    assert!(Frames::new("samco").at(&events, Mode::Quote).is_empty());
    let d = Frames::new("samco").at(&events, Mode::Depth).pop().unwrap();
    assert_eq!(d["ltp"], 0.0, "no quote yet: {}", d);
    for k in ["open", "volume", "last_quantity", "oi", "ltt"] {
        assert!(d.get(k).is_none(), "{}: {}", k, d);
    }
}

/// Web `samcoWebSocket._normalize_market_data`: a Depth holder gets no
/// frame until Samco has sent a book, and then the levels Samco sent,
/// none invented. The desktop sent five zero levels a side for a quote
/// before the first book, and padded every book to five levels.
#[test]
fn no_book_is_sent_before_samco_sends_one_and_none_is_padded() {
    let (mut f, _) = feed();
    f.subscribe_frames(&[sub("RELIANCE", "NSE", "NSE", "2885", FeedMode::Depth)]);
    let events = f.parse(&quote(doc_quote()));
    assert!(
        Frames::new("samco").at(&events, Mode::Depth).is_empty(),
        "{:?}",
        events
    );
    let events = f.parse(&depth_frame(json!({
        "symbol": "2885_NSE",
        "bidValues": [{"price": "44554", "qty": "1", "no": "1"}],
        "askValues": [],
    })));
    let d = Frames::new("samco").at(&events, Mode::Depth).pop().unwrap();
    assert_eq!(d["depth"]["buy"].as_array().map(Vec::len), Some(1), "{}", d);
    assert_eq!(d["depth"]["sell"], json!([]), "{}", d);
}

/// When the last Depth holder leaves, the book stream's fields go with it
/// (the web's client drops the symbol's merged fields on that
/// unsubscribe): a Quote holder was then shown quote2's totals over the
/// quote stream's fresh tBQ/tSQ, the totals of a book no longer streamed.
#[test]
fn leaving_depth_drops_the_book_streams_totals() {
    let mut f = reliance();
    f.parse(&depth_frame(json!({
        "symbol": "2885_NSE", "tbq": "5", "taq": "6", "bidValues": [], "askValues": [],
    })));
    let depth = sub("RELIANCE", "NSE", "NSE", "2885", FeedMode::Depth);
    f.mode_change_frames(&depth, &depth.with_mode(FeedMode::Quote));
    let events = f.parse(&quote(doc_quote()));
    let d = Frames::new("samco").at(&events, Mode::Quote).pop().unwrap();
    assert_eq!(
        (&d["total_buy_quantity"], &d["total_sell_quantity"]),
        (&json!(973), &json!(610)),
        "{}",
        d
    );
}

/// Samco streams OI (`oI`) on its quote stream. The web reads it and then
/// drops it from every mode (lost data, decisions 2026-11-09); the desktop
/// sends it to Quote holders as to Depth ones, and no OI Samco did not
/// send. A Quote holder got none: the tick took the subscription's mode.
#[test]
fn samcos_oi_reaches_quote_holders() {
    let (mut f, _) = feed();
    f.subscribe_frames(&[quote_sub("FUT", "NFO", "35001")]);
    let events = f.parse(&Message::Text(
        json!({"sym": "35001_NFO", "ltp": "24610.4", "oI": "10650225", "streaming_type": "quote"})
            .to_string(),
    ));
    let d = Frames::new("samco").at(&events, Mode::Quote).pop().unwrap();
    assert_eq!(d["oi"], 10_650_225, "{}", d);
    let (mut f, _) = feed();
    f.subscribe_frames(&[quote_sub("FUT", "NFO", "35001")]);
    let d = Frames::new("samco")
        .at(&f.parse(&tick("35001_NFO")), Mode::Quote)
        .pop()
        .unwrap();
    assert!(d.get("oi").is_none(), "{}", d);
}

/// Web `samcoWebSocket._normalize_market_data`: a Quote frame carries the
/// best bid and ask (best_bid_price, best_bid_quantity, best_ask_price,
/// best_ask_quantity): the book's first level once Samco has sent one,
/// else the quote's bPr/bSz and aPr/aSz, and none Samco sent neither of
/// (the web sends 0). The desktop sent none of them.
#[test]
fn the_best_bid_and_ask_are_published_at_quote() {
    let best = |d: &Value| {
        [
            "best_bid_price",
            "best_bid_quantity",
            "best_ask_price",
            "best_ask_quantity",
        ]
        .map(|k| d.get(k).cloned())
    };
    let (mut f, _) = feed();
    f.subscribe_frames(&[quote_sub("RELIANCE", "NSE", "2885")]);
    let d = Frames::new("samco")
        .at(&f.parse(&quote(doc_quote())), Mode::Quote)
        .pop()
        .unwrap();
    assert_eq!(
        best(&d),
        [json!(44554.0), json!(1), json!(44561.0), json!(1)].map(Some),
        "{}",
        d
    );
    let mut f = reliance();
    f.parse(&depth_frame(json!({
        "symbol": "2885_NSE",
        "bidValues": [{"price": "44550", "qty": "7", "no": "2"}],
        "askValues": [{"price": "44560.5", "qty": "9", "no": "3"}],
    })));
    let d = Frames::new("samco")
        .at(&f.parse(&quote(doc_quote())), Mode::Quote)
        .pop()
        .unwrap();
    assert_eq!(
        best(&d),
        [json!(44550.0), json!(7), json!(44560.5), json!(9)].map(Some),
        "the book's first level: {}",
        d
    );
    let (mut f, _) = feed();
    f.subscribe_frames(&[quote_sub("RELIANCE", "NSE", "2885")]);
    let d = Frames::new("samco")
        .at(&f.parse(&tick("2885_NSE")), Mode::Quote)
        .pop()
        .unwrap();
    assert_eq!(best(&d), [None, None, None, None], "{}", d);
}

/// Web test_tick_from_another_segment_is_not_routed.
#[test]
fn a_tick_from_another_segment_is_not_routed() {
    let (mut f, _) = feed();
    f.subscribe_frames(&[quote_sub("FUT", "NFO", "11536")]);
    let ev = f.parse(&tick("11536_NSE"));
    assert!(ev.is_empty(), "{:?}", ev);
}

/// Web test_tick_from_the_subscribed_segment_is_routed.
#[test]
fn a_tick_from_the_subscribed_segment_is_routed() {
    let (mut f, _) = feed();
    f.subscribe_frames(&[
        quote_sub("FUT", "NFO", "11536"),
        quote_sub("EQ", "NSE", "11536"),
    ]);
    let ev = f.parse(&tick("11536_NFO"));
    assert_eq!(routed(&ev), vec![("NFO".into(), "FUT".into())]);
}

/// Web test_mcx_tick_routed_by_streaming_segment: MCX derivatives stream as
/// `<scrip>_MFO`, their broker exchange.
#[test]
fn an_mcx_tick_is_routed_by_its_streaming_segment() {
    let (mut f, _) = feed();
    f.subscribe_frames(&[sub("CRUDE", "MCX", "MFO", "464925", FeedMode::Ltp)]);
    let ev = f.parse(&tick("464925_MFO"));
    assert_eq!(routed(&ev), vec![("MCX".into(), "CRUDE".into())]);
}

/// Web test_a_bare_scrip_code_shared_by_two_segments_is_not_routed.
#[test]
fn a_bare_scrip_code_shared_by_two_segments_is_not_routed() {
    let (mut f, _) = feed();
    f.subscribe_frames(&[
        quote_sub("FUT", "NFO", "11536"),
        quote_sub("EQ", "NSE", "11536"),
    ]);
    let ev = f.parse(&tick("11536"));
    assert!(ev.is_empty(), "{:?}", ev);
    assert_eq!(f.state_len(), 0, "nothing is kept for a dropped tick");
}

/// Web test_a_bare_scrip_code_in_one_segment_and_an_index_id_are_routed.
/// The bare code's fields merge into its subscription's state, which goes
/// with the subscription.
#[test]
fn a_bare_scrip_code_in_one_segment_and_an_index_id_are_routed() {
    let (mut f, ids) = feed();
    ids.lock().insert("NSE_INDEX", "NIFTY", "-23");
    let eq = quote_sub("EQ", "NSE", "11536");
    f.subscribe_frames(&[
        eq.clone(),
        sub("NIFTY", "NSE_INDEX", "NSE", "NIFTY_50", FeedMode::Ltp),
    ]);
    let mut ev = Vec::new();
    for sym in ["11536", "-23"] {
        ev.extend(f.parse(&tick(sym)));
    }
    assert_eq!(
        routed(&ev),
        vec![
            ("NSE".into(), "EQ".into()),
            ("NSE_INDEX".into(), "NIFTY".into())
        ]
    );
    assert_eq!(f.state_len(), 2);
    f.unsubscribe_frames(&[eq]);
    assert_eq!(f.state_len(), 1, "the bare code's state went with EQ");
}

/// Web `samcoWebSocket._cleanup_connection_state`: fields from a dead
/// connection do not survive into the next one, so a `quote2` frame after
/// a reconnect does not carry the old connection's trade time and average
/// price (the desktop kept its merged state across reconnects).
#[test]
fn a_new_connection_does_not_carry_the_old_quote() {
    let mut f = reliance();
    f.parse(&quote(doc_quote()));
    assert_eq!(f.state_len(), 1);
    assert!(f.on_connected().is_empty());
    assert_eq!(f.state_len(), 0);
    let events = f.parse(&depth_frame(json!({
        "symbol": "2885_NSE", "tbq": "5", "taq": "6", "bidValues": [], "askValues": [],
    })));
    let d = Frames::new("samco").at(&events, Mode::Depth).pop().unwrap();
    for k in ["ltt", "last_trade_time", "average_price"] {
        assert!(d.get(k).is_none(), "{}: {}", k, d);
    }
    assert_eq!(d["total_buy_quantity"], 5, "{}", d);
}
