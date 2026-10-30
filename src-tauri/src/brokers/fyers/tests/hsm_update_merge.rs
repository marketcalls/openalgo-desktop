//! Web `test/test_fyers_hsm_update_merge.py` (openalgo#2318, #2323): the
//! Fyers HSM feed publishes each update whole and invents nothing. The
//! frames are built byte for byte the way HSM sends them and go through the
//! real parser, with the web test's values.
//!
//! The desktop's parser already applied an update frame whole and never
//! de-duplicated by LTP, and it always derived the change from LTP and the
//! previous close: those cases pin that. The depth LTP (a bid/ask
//! midpoint) and the index book (five levels around the index value) were
//! invented here as on the web: those cases fail before the port.

use super::*;
use crate::brokers::common::streaming::{NormalizedDepth, NormalizedTick};

const CRUDE: &str = "CRUDEOIL19OCT26FUT";
const SF: &str = "sf|mcx_fo|445001";
const DP: &str = "dp|mcx_fo|445001";
const SF_ID: u16 = 7;
const DP_ID: u16 = 8;

// `DATA_FIELDS` positions the web test sets.
const LTP: usize = 0;
const VOLUME: usize = 1;
const LTT: usize = 2;
const FEED_TIME: usize = 3;
const BID_SIZE: usize = 4;
const ASK_SIZE: usize = 5;
const BID: usize = 6;
const ASK: usize = 7;
const LTQ: usize = 8;
const TBQ: usize = 9;
const TSQ: usize = 10;
const ATP: usize = 11;
const LOW: usize = 13;
const HIGH: usize = 14;
const OPEN: usize = 19;
const PREV_CLOSE: usize = 20;

/// A full `DATA_FIELDS` record: the given values, absent elsewhere.
fn quote(set: &[(usize, i32)]) -> Vec<i32> {
    let mut v = vec![NA; DATA_FIELDS.len()];
    for (i, x) in set {
        v[*i] = *x;
    }
    v
}

/// The web test's `BASE` snapshot (CRUDEOIL at 9665 against 9798).
fn base() -> Vec<i32> {
    quote(&[
        (LTP, 966_500),
        (VOLUME, 18_450),
        (LTT, 1_793_170_000),
        (FEED_TIME, 1_793_170_000),
        (BID_SIZE, 1),
        (ASK_SIZE, 8),
        (BID, 966_500),
        (ASK, 966_600),
        (LTQ, 1),
        (TBQ, 955),
        (TSQ, 872),
        (ATP, 966_400),
        (LOW, 958_400),
        (HIGH, 973_500),
        (OPEN, 966_100),
        (PREV_CLOSE, 979_800),
    ])
}

/// The web test's `TICK`: one trade that moves LTP, volume, LTQ, bid, ask
/// and the trade time together.
fn tick() -> Vec<i32> {
    quote(&[
        (LTP, 966_600),
        (VOLUME, 18_453),
        (LTQ, 3),
        (BID, 966_600),
        (ASK, 966_700),
        (LTT, 1_793_170_001),
    ])
}

/// The web test's `DEPTH`: the best bid and ask with sizes and orders.
fn depth() -> Vec<i32> {
    let mut v = vec![NA; DEPTH_FIELD_COUNT];
    v[0] = 966_500; // bid price 1
    v[5] = 966_600; // ask price 1
    v[10] = 1; // bid size 1
    v[15] = 8; // ask size 1
    v[20] = 1; // bid orders 1
    v[25] = 2; // ask orders 1
    v
}

fn crude(mode: FeedMode) -> HsmFeed {
    let mut f = feed();
    f.subscribe_frames(&[sub(CRUDE, "MCX", mode)]);
    f
}

/// One data frame through the parser: its ticks and its depth messages.
fn send(f: &mut HsmFeed, records: &[Vec<u8>]) -> (Vec<NormalizedTick>, Vec<NormalizedDepth>) {
    let mut ticks = Vec::new();
    let mut depths = Vec::new();
    for e in f.parse(&Message::Binary(data_frame(records))) {
        match e {
            FeedEvent::Tick(t) => ticks.push(t),
            FeedEvent::Depth(d) => depths.push(d),
            other => panic!("unexpected {:?}", other),
        }
    }
    (ticks, depths)
}

/// Web `test_update_frame_is_sent_once_with_every_field`.
#[test]
fn an_update_frame_is_published_once_with_every_field() {
    let mut f = crude(FeedMode::Quote);
    send(&mut f, &[snapshot(SF_ID, SF, &base(), Some((1, 2)))]);
    let (ticks, _) = send(&mut f, &[update(SF_ID, &tick())]);
    let [t] = &ticks[..] else {
        panic!("one message per scrip per update frame: {:?}", ticks);
    };
    assert_eq!(t.ltp, 9666.0);
    // Each of these was the previous tick's value on the web before #2323.
    assert_eq!(t.volume, 18_453);
    assert_eq!(t.last_quantity, 3);
    assert_eq!(t.last_trade_time_ms, 1_793_170_001_000);
    // What the update did not carry stays as the snapshot had it.
    assert_eq!((t.open, t.close, t.average_price), (9661.0, 9798.0, 9664.0));
}

/// Web `test_update_that_moves_only_bid_and_ask_is_published`: same LTP in
/// the same second, which the web's LTP de-duplication dropped.
#[test]
fn an_update_that_moves_only_bid_and_ask_is_published() {
    let mut f = crude(FeedMode::Quote);
    send(&mut f, &[snapshot(SF_ID, SF, &base(), Some((1, 2)))]);
    send(&mut f, &[update(SF_ID, &tick())]);
    let (ticks, _) = send(
        &mut f,
        &[update(SF_ID, &quote(&[(BID, 966_650), (ASK, 966_750)]))],
    );
    let [t] = &ticks[..] else {
        panic!("the bid and ask move is published: {:?}", ticks);
    };
    assert_eq!((t.ltp, t.volume), (9666.0, 18_453));
}

/// Web `test_update_with_no_change_sends_nothing`.
#[test]
fn an_update_with_no_change_sends_nothing() {
    let mut f = crude(FeedMode::Quote);
    send(&mut f, &[snapshot(SF_ID, SF, &base(), Some((1, 2)))]);
    let (ticks, depths) = send(&mut f, &[update(SF_ID, &quote(&[(LTP, 966_500)]))]);
    assert!(ticks.is_empty() && depths.is_empty());
}

/// Web `test_update_for_unknown_topic_does_not_break_the_next_scrip`.
#[test]
fn an_update_for_an_unknown_topic_does_not_break_the_next_scrip() {
    let mut f = crude(FeedMode::Quote);
    send(&mut f, &[snapshot(SF_ID, SF, &base(), Some((1, 2)))]);
    let (ticks, _) = send(&mut f, &[update(99, &tick()), update(SF_ID, &tick())]);
    let [t] = &ticks[..] else {
        panic!("the known scrip's update: {:?}", ticks);
    };
    assert_eq!(t.volume, 18_453);
}

/// Web `test_change_is_derived_from_ltp_and_previous_close`: 9665 against
/// 9798 reads -133 (-1.36%), as the Fyers app shows.
#[test]
fn change_is_derived_from_ltp_and_previous_close() {
    let mut f = crude(FeedMode::Quote);
    let (ticks, _) = send(&mut f, &[snapshot(SF_ID, SF, &base(), Some((1, 2)))]);
    assert_eq!((ticks[0].change, ticks[0].change_percent), (-133.0, -1.36));
}

/// Web `test_change_stays_zero_without_previous_close`.
#[test]
fn change_stays_zero_without_previous_close() {
    let mut f = crude(FeedMode::Quote);
    let only_ltp = quote(&[(LTP, 966_500)]);
    let (ticks, _) = send(&mut f, &[snapshot(SF_ID, SF, &only_ltp, Some((1, 2)))]);
    assert_eq!((ticks[0].change, ticks[0].change_percent), (0.0, 0.0));
}

/// Web `test_depth_has_no_midpoint_ltp` and
/// `test_depth_alone_publishes_without_ltp`: 9665 / 9666 used to publish
/// 9665.5, a price that never traded on a contract that ticks in rupees.
#[test]
fn depth_has_no_midpoint_ltp() {
    let mut f = crude(FeedMode::Depth);
    let (_, depths) = send(&mut f, &[snapshot(DP_ID, DP, &depth(), Some((1, 2)))]);
    let [d] = &depths[..] else {
        panic!("one depth message: {:?}", depths);
    };
    assert_eq!(d.ltp, 0.0, "no LTP of its own, never the midpoint");
    assert_eq!((d.buy[0].price, d.sell[0].price), (9665.0, 9666.0));
    assert_eq!((d.buy[0].quantity, d.sell[0].orders), (1, 2));
}

/// Web `test_depth_borrows_the_real_ltp_from_the_quote_feed`. A desktop
/// depth subscription always carries the quote topic too, and the bridge
/// gives a depth frame that tick's LTP (`feed::bridge::depth_update`,
/// pinned there by `a_depth_without_its_own_ltp_frames_the_last_traded_price`):
/// the depth message itself leaves the field to it.
#[test]
fn depth_leaves_the_ltp_to_the_quote_feed() {
    let mut f = crude(FeedMode::Depth);
    let (ticks, depths) = send(
        &mut f,
        &[
            snapshot(SF_ID, SF, &base(), Some((1, 2))),
            snapshot(DP_ID, DP, &depth(), Some((1, 2))),
        ],
    );
    assert_eq!(ticks[0].ltp, 9665.0, "the traded price");
    assert_eq!(depths[0].ltp, 0.0, "the old midpoint was 9665.5");
}

/// Web `test_tbt_depth_has_no_midpoint_ltp`: the 50-level book carries no
/// LTP of its own either.
#[test]
fn tbt_depth_has_no_midpoint_ltp() {
    let mut f = TbtFeed::new(TBT_URL, &auth(), master()).unwrap();
    f.subscribe_frames(&[sub("SBIN", "NSE", FeedMode::Depth)]);
    let book = tbt_message(
        "NSE:SBIN-EQ",
        true,
        &[pb::level(966_500, 1, 1, 0)],
        &[pb::level(966_600, 8, 2, 0)],
        955,
    );
    match &f.parse(&Message::Binary(book))[..] {
        [FeedEvent::Depth(d)] => {
            assert_eq!(d.ltp, 0.0);
            assert_eq!((d.buy[0].price, d.sell[0].price), (9665.0, 9666.0));
        }
        other => panic!("unexpected {:?}", other),
    }
}

/// Web `test_index_depth_is_empty_not_invented`: an index has no order
/// book. Its depth subscribers get the real index value and no levels made
/// up around it (the desktop's index frames carry no book for any broker).
#[test]
fn index_depth_is_not_invented() {
    let mut f = feed();
    f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", FeedMode::Depth)]);
    let mut fields = vec![NA; INDEX_FIELDS.len()];
    fields[0] = 2_293_675;
    let (ticks, depths) = send(&mut f, &[snapshot(3, "if|nse_cm|Nifty 50", &fields, None)]);
    assert!(depths.is_empty(), "no made-up levels: {:?}", depths);
    let [t] = &ticks[..] else {
        panic!("the index value: {:?}", ticks);
    };
    assert_eq!((t.ltp, t.mode), (22936.75, 3));
}
