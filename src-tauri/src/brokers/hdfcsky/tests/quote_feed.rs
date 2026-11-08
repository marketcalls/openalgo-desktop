//! Web `test/test_hdfcsky_quote_feed.py` (openalgo#2396, #2397): HDFC Sky's
//! `*_CIRC` and `*_OI` packets are partial refreshes, merged into the last
//! full packet of their instrument instead of being published as a quote
//! with ltp, OHLC and volume 0; a packet belongs to the subscription of its
//! own exchange (from the packet type) and token; and no baseline outlives
//! its subscription or its socket. The web test's packets go through the
//! real decoder and parser and come out as the 8765 frames the bridge makes
//! of them (`feed::testkit`).
//!
//! What has no counterpart here: the web client's own threads, send queue
//! and reconnect replay (an unsubscribe racing a subscribe frame, during the
//! handshake or at the replay, a replay frame sent twice or lost, the
//! subscription generation and feed epoch that guard them). The desktop's
//! manager calls the feed on one task, sends each frame in the order the
//! subscriptions changed, and replays from its own registry
//! (`websocket::manager`). It also keeps the broker at each instrument's
//! highest mode (`subscriptions_are_reference_counted_with_effective_mode`),
//! which the web's mode tracking fixed. The web's 300-instrument refusal
//! has no desktop counterpart yet, and circuit limits (`upper_circuit`)
//! are not in the desktop's frames for any broker.

use super::*;
use crate::feed::testkit::Frames;
use crate::feed::Mode;

/// The web test's `FULL` quote.
fn full() -> proto::MbpData {
    proto::MbpData {
        last_traded_price: 1500.5,
        open_price: 1490.0,
        high_price: 1510.0,
        low_price: 1480.0,
        closing_price: 1495.0,
        volume_traded_today: 12345,
        last_trade_quantity: 3,
        average_trade_price: 1499.0,
        last_trade_time: 1_700_000_000,
        ..Default::default()
    }
}

fn circuit() -> proto::MbpData {
    proto::MbpData {
        lower_circuit_limit: 1350.0,
        upper_circuit_limit: 1650.0,
        ..Default::default()
    }
}

fn open_interest(oi: i64) -> proto::MbpData {
    proto::MbpData {
        oi,
        ..Default::default()
    }
}

/// The web test's `_packet`: token 2885 at packet time 1700000000000.
fn packet(packet_type: i32, mbp: proto::MbpData) -> proto::GenericDto {
    proto::GenericDto {
        instrument_id: 2885,
        packet_type,
        packet_timestamp: 1_700_000_000_000,
        mbp_data: Some(mbp),
        ..Default::default()
    }
}

fn send(f: &mut HdfcSkyFeed, packets: Vec<proto::GenericDto>) -> Vec<FeedEvent> {
    f.parse(&Message::Binary(frame(packets)))
}

fn nifty_fut(mode: FeedMode) -> FeedSubscription {
    sub("NIFTYFUT", "NFO", "2885", mode)
}

fn feed_with(subs: &[FeedSubscription]) -> HdfcSkyFeed {
    let mut f = HdfcSkyFeed::new(WS_URL, "K", "T");
    f.subscribe_frames(subs);
    f
}

/// Web test_circuit_and_oi_packets_are_tagged_partial: a circuit packet
/// carries its band and an OI packet its OI, and neither is a quote.
#[test]
fn circuit_and_oi_packets_are_partial_refreshes() {
    let ticks = decode_frame(&frame(vec![
        packet(pt::NSE_FO_CIRC, circuit()),
        packet(pt::NSE_FO_OI, open_interest(777)),
    ]));
    let [circ, oi] = ticks.as_slice() else {
        panic!("{:?}", ticks)
    };
    assert_ne!(circ.kind, Kind::Mbp, "a circuit packet is no quote");
    assert_eq!((circ.lower_limit, circ.upper_limit), (1350.0, 1650.0));
    assert_ne!(oi.kind, Kind::Mbp, "an OI packet is no quote");
    assert_eq!(oi.oi, 777);
    assert_eq!(packet_exchange(oi.packet_type), Some("NFO"));
}

/// Web test_partial_packets_merge_into_snapshot_instead_of_blanking.
#[test]
fn partial_packets_merge_into_the_last_quote_instead_of_blanking_it() {
    let mut f = feed_with(&[nifty_fut(FeedMode::Depth)]);
    let mut events = send(&mut f, vec![packet(pt::NSE_FO_ALL, full())]);
    events.extend(send(
        &mut f,
        vec![
            packet(pt::NSE_FO_CIRC, circuit()),
            packet(pt::NSE_FO_OI, open_interest(777)),
        ],
    ));
    let frames = Frames::new("hdfcsky").at(&events, Mode::Quote);
    assert_eq!(
        frames.len(),
        3,
        "the quote and one merged frame per refresh"
    );
    let last = frames.last().unwrap();
    assert_eq!(last["ltp"], 1500.5, "{}", last);
    assert_eq!(last["volume"], 12345, "{}", last);
    assert_eq!(last["open"], 1490.0, "{}", last);
    assert_eq!(last["oi"], 777, "{}", last);
    assert_eq!(
        (&last["ltt"], &last["timestamp"]),
        (&json!(1_700_000_000_000_i64), &json!(1_700_000_000_000_i64)),
        "the trade time in ms and the packet's time: {}",
        last
    );
    for d in &frames {
        assert_eq!(d["ltp"], 1500.5, "no frame blanks the price: {}", d);
    }
}

/// Web test_partial_packet_without_snapshot_is_not_published.
#[test]
fn a_partial_packet_before_any_quote_is_not_published() {
    let mut f = feed_with(&[nifty_fut(FeedMode::Quote)]);
    let ev = send(&mut f, vec![packet(pt::NSE_FO_OI, open_interest(777))]);
    assert!(ev.is_empty(), "{:?}", ev);
    let ev = send(&mut f, vec![packet(pt::NSE_FO_CIRC, circuit())]);
    assert!(ev.is_empty(), "{:?}", ev);
}

/// Web test_same_token_on_two_exchanges_routes_by_packet_type.
#[test]
fn the_same_token_on_two_exchanges_routes_by_packet_type() {
    let mut f = feed_with(&[
        sub("EQ", "NSE", "2885", FeedMode::Quote),
        sub("CUR", "CDS", "2885", FeedMode::Quote),
    ]);
    let ev = send(&mut f, vec![packet(pt::NSE_CD_ALL, full())]);
    let routed: Vec<(String, String)> = ev
        .iter()
        .filter_map(|e| match e {
            FeedEvent::Tick(t) => Some((t.exchange.clone(), t.symbol.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(routed, vec![("CDS".to_string(), "CUR".to_string())]);
}

/// Web test_modes_tracked_and_highest_kept_at_broker, at the feed. The
/// manager keeps the broker at the instrument's highest mode, so an LTP
/// subscriber beside a Depth one changes nothing
/// (`subscriptions_are_reference_counted_with_effective_mode`). When the
/// Depth subscriber leaves, HDFC Sky is moved to the LTP tier by a
/// subscribe at LTP, never an unSubscribe (the web asserts
/// `ws_client.unsubscribe.assert_not_called()`), and the instrument keeps
/// its baseline, so the next OI packet merges into it. When the last
/// subscriber leaves, it is unsubscribed once and its baseline goes.
#[test]
fn a_mode_change_moves_the_tier_and_keeps_the_baseline() {
    let frames_of = |m: Vec<Message>| m.iter().map(text).collect::<Vec<Value>>();
    let mut f = feed_with(&[nifty_fut(FeedMode::Depth)]);
    send(&mut f, vec![packet(pt::NSE_FO_ALL, full())]);
    let down = f.mode_change_frames(&nifty_fut(FeedMode::Depth), &nifty_fut(FeedMode::Ltp));
    assert_eq!(
        frames_of(down),
        [json!({"heart_beat": false, "subscribe": [{"scripId": "NFO_2885", "type": "LTP"}]})]
    );
    let events = send(&mut f, vec![packet(pt::NSE_FO_OI, open_interest(777))]);
    let d = Frames::new("hdfcsky").at(&events, Mode::Ltp).pop().unwrap();
    assert_eq!(d["ltp"], 1500.5, "{}", d);
    let up = f.mode_change_frames(&nifty_fut(FeedMode::Ltp), &nifty_fut(FeedMode::Depth));
    assert_eq!(
        frames_of(up),
        [json!({"heart_beat": false, "subscribe": [{"scripId": "NFO_2885", "type": "ALL"}]})]
    );
    assert!(
        f.mode_change_frames(&nifty_fut(FeedMode::Depth), &nifty_fut(FeedMode::Quote))
            .is_empty(),
        "Quote and Depth are both ALL"
    );
    let gone = f.unsubscribe_frames(&[nifty_fut(FeedMode::Quote)]);
    assert_eq!(
        frames_of(gone),
        [json!({"heart_beat": false, "unSubscribe": [{"scripId": "NFO_2885", "type": "ALL"}]})]
    );
    assert_eq!(f.baselines(), 0);
}

/// Web test_unsubscribe_at_the_snapshot_write_leaves_no_stale_baseline: once
/// unsubscribed and subscribed again, an OI packet before the first full
/// quote does not publish the price from the previous subscription.
#[test]
fn a_new_subscription_never_merges_into_the_previous_ones_quote() {
    let mut f = feed_with(&[nifty_fut(FeedMode::Quote)]);
    send(&mut f, vec![packet(pt::NSE_FO_ALL, full())]);
    f.unsubscribe_frames(&[nifty_fut(FeedMode::Quote)]);
    f.subscribe_frames(&[nifty_fut(FeedMode::Quote)]);
    let ev = send(&mut f, vec![packet(pt::NSE_FO_OI, open_interest(777))]);
    assert!(ev.is_empty(), "{:?}", ev);
}

/// Web test_reconnect_drops_snapshots_before_partial_packets: a new socket
/// waits for its own first full quote.
#[test]
fn a_reconnect_drops_the_baselines_before_partial_packets() {
    let mut f = feed_with(&[nifty_fut(FeedMode::Quote)]);
    send(&mut f, vec![packet(pt::NSE_FO_ALL, full())]);
    assert!(f.on_connected().is_empty());
    let ev = send(&mut f, vec![packet(pt::NSE_FO_OI, open_interest(777))]);
    assert!(ev.is_empty(), "{:?}", ev);
    let ev = send(&mut f, vec![packet(pt::NSE_FO_ALL, full())]);
    assert_eq!(ev.len(), 1, "the next full quote is published");
}
